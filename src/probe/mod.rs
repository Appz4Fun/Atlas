//! Looks at a release's articles without downloading it: yEnc control lines from its first and last articles,
//! integrity checks, the NNTP headers XOVER leaves out, what the first bytes of the file are, and its NFO and SFV.

pub mod archive;
pub mod media;
pub mod step;
pub mod yenc;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::postprocess::schema::ShardTable;
use crate::store;

pub use step::ProbeStep;

/// What a probe found, kept as JSON: it's only ever read whole. The columns are what can be searched on.
pub const PROBE: ShardTable = ShardTable {
    name: "release_probe",
    create: "create table if not exists release_probe (
        release_id INTEGER PRIMARY KEY,
        crc32      INTEGER,
        agent      TEXT,
        info       TEXT NOT NULL
    )",
    indexes: &[
        (
            "release_probe_crc32",
            "create index if not exists release_probe_crc32 on release_probe(crc32) where crc32 is not null",
        ),
        (
            "release_probe_agent",
            "create index if not exists release_probe_agent on release_probe(agent) where agent is not null",
        ),
    ],
    release_key: Some("release_id"),
    added: &[],
};

pub const SET_PROBE: &str =
    "insert or replace into release_probe (release_id, crc32, agent, info) values (?1, ?2, ?3, ?4)";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProbeInfo {
    /// which of the release's files this is about
    pub file: Option<String>,
    pub yenc: Option<yenc::YencLines>,
    /// the full file's checksum, from the last part
    pub file_crc32: Option<u32>,
    pub segment_size: Option<u64>,
    pub overhead_percent: Option<f64>,
    pub continuity_ok: Option<bool>,
    pub part_size_ok: Option<bool>,
    pub part_crc_ok: Option<bool>,
    /// first, middle and last article: whether a server still has them
    pub health: BTreeMap<String, bool>,
    pub headers: BTreeMap<String, String>,
    pub crossposted_to: Vec<String>,
    pub archive: Option<archive::ArchiveHead>,
    pub media: Option<media::MediaInfo>,
    pub nfo: Option<String>,
    pub imdb_ids: Vec<String>,
    pub sfv: BTreeMap<String, u32>,
}

/// The headers worth keeping, under the names they arrive with.
const KEPT_HEADERS: &[&str] = &[
    "Injection-Date",
    "NNTP-Posting-Date",
    "Date",
    "User-Agent",
    "X-Newsreader",
    "X-Newsposter",
    "Path",
    "Injection-Info",
    "Organization",
    "Xref",
    "NNTP-Posting-Host",
    "X-Complaints-To",
];

pub fn keep_headers(headers: &[(String, String)]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, value) in headers {
        if let Some(kept) = KEPT_HEADERS.iter().find(|k| k.eq_ignore_ascii_case(name)) {
            out.insert((*kept).to_string(), value.chars().take(512).collect());
        }
    }
    out
}

/// What posted this: the tool, from whichever header it's in.
pub fn agent_of(headers: &BTreeMap<String, String>) -> Option<&str> {
    ["User-Agent", "X-Newsreader", "X-Newsposter"].iter().find_map(|k| headers.get(*k)).map(String::as_str)
}

/// The groups in an `Xref: host group:123 group:456`.
pub fn xref_groups(xref: &str) -> Vec<String> {
    xref.split_whitespace().filter_map(|part| part.split_once(':').map(|(group, _)| group.to_string())).collect()
}

/// Checks that need only what the first and last parts said. `wire_bytes` is the first article's size as posted, soo
/// overhead is about this one file, however many others the release has.
pub fn derive(info: &mut ProbeInfo, wire_bytes: i64, expected_parts: Option<i64>, last: Option<&yenc::YencLines>) {
    let Some(y) = info.yenc.clone() else { return };
    if let (Some(begin), Some(end)) = (y.begin, y.end) {
        let segment = (end + 1).saturating_sub(begin);
        info.segment_size = Some(segment);
        if let Some(size) = y.yend_size {
            info.part_size_ok = Some(size == segment);
        }
    }
    if let Some(size) = y.file_size.filter(|s| *s > 0) {
        if let Some(segment) = info.segment_size.filter(|s| *s > 0 && wire_bytes > 0) {
            info.overhead_percent = Some((wire_bytes as f64 - segment as f64) / segment as f64 * 100.0);
        }
        let last_size = last.and_then(|l| Some((l.end? + 1).saturating_sub(l.begin?)));
        if let (Some(segment), Some(total), Some(last_size)) = (info.segment_size, y.total, last_size) {
            let parts_agree = expected_parts.is_none_or(|e| e == i64::from(total));
            info.continuity_ok = Some(parts_agree && u64::from(total.saturating_sub(1)) * segment + last_size == size);
        }
    }
    info.file_crc32 = last.and_then(|l| l.crc32).or(y.crc32);
}

pub fn read(conn: &rusqlite::Connection, release_id: i64) -> Option<ProbeInfo> {
    let s = store::shard_of_id(release_id);
    let text: String = conn
        .query_row(&format!("select info from s{s}.release_probe where release_id = ?"), [release_id], |r| r.get(0))
        .ok()?;
    serde_json::from_str(&text).ok()
}

/// NFOs are often CP437: what isn't UTF-8 reads as that.
pub fn nfo_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => bytes.iter().map(|b| cp437(*b)).collect(),
    }
}

fn cp437(b: u8) -> char {
    const HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";
    if b < 0x80 { char::from(b) } else { HIGH.chars().nth(usize::from(b - 0x80)).unwrap_or('?') }
}

pub fn imdb_ids(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut at = 0;
    while let Some(found) = text.get(at..).and_then(|t| t.find("tt")) {
        let start = at + found;
        let digits = bytes[start + 2..].iter().take_while(|b| b.is_ascii_digit()).count();
        let before_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        if before_ok && (7..=8).contains(&digits) {
            let id = text[start..start + 2 + digits].to_string();
            if !out.contains(&id) {
                out.push(id);
            }
        }
        at = start + 2;
    }
    out
}

/// `name crc` lines, and `;` comments.
pub fn parse_sfv(text: &str) -> BTreeMap<String, u32> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if let Some((name, crc)) = line.rsplit_once(char::is_whitespace)
            && crc.len() == 8
            && let Ok(crc) = u32::from_str_radix(crc, 16)
        {
            out.insert(name.trim().to_string(), crc);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_checks_catch_a_gap() {
        let first = yenc::YencLines {
            file_size: Some(2500),
            total: Some(3),
            begin: Some(1),
            end: Some(1000),
            yend_size: Some(1000),
            ..Default::default()
        };
        let last = yenc::YencLines { begin: Some(2001), end: Some(2500), crc32: Some(7), ..Default::default() };
        let mut info = ProbeInfo { yenc: Some(first.clone()), ..ProbeInfo::default() };
        derive(&mut info, 1030, Some(3), Some(&last));
        assert_eq!(info.segment_size, Some(1000));
        assert_eq!(info.part_size_ok, Some(true));
        assert_eq!(info.continuity_ok, Some(true));
        assert_eq!(info.overhead_percent, Some(3.0));
        assert_eq!(info.file_crc32, Some(7));

        let mut wrong = ProbeInfo { yenc: Some(first), ..ProbeInfo::default() };
        derive(&mut wrong, 1030, Some(4), Some(&last));
        assert_eq!(wrong.continuity_ok, Some(false), "fewer parts than the subject says");
    }

    #[test]
    fn headers_and_crossposts() {
        let kept = keep_headers(&[
            ("user-agent".to_string(), "Nyuu/0.4".to_string()),
            ("Xref".to_string(), "news alt.binaries.a:1 alt.binaries.b:2".to_string()),
            ("Subject".to_string(), "x".to_string()),
        ]);
        assert_eq!(agent_of(&kept), Some("Nyuu/0.4"));
        assert!(!kept.contains_key("Subject"));
        assert_eq!(xref_groups(&kept["Xref"]), ["alt.binaries.a", "alt.binaries.b"]);
    }

    #[test]
    fn nfo_text_and_what_is_in_it() {
        assert_eq!(nfo_text(b"plain"), "plain");
        assert_eq!(nfo_text(&[0xdb, b'a']), "█a");
        assert_eq!(
            imdb_ids("see https://imdb.com/title/tt0133093/ and tt1234567 xtt7654321 tt12"),
            ["tt0133093", "tt1234567"]
        );
        let sfv = parse_sfv("; comment\nmovie.r00 0a1B2c3D\nbad line\n");
        assert_eq!(sfv["movie.r00"], 0x0a1b_2c3d);
        assert_eq!(sfv.len(), 1);
    }
}
