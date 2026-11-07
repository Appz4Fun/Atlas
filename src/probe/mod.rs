//! Looks at a release's articles without downloading it: yEnc control lines from its first and last articles,
//! integrity checks, the NNTP headers XOVER leaves out, what the first bytes of the file are, and its NFO and SFV.

pub mod archive;
pub mod asf;
pub mod audio_print;
pub mod avi;
pub mod bluray;
pub mod body_crc;
pub mod codec;
pub mod dvd;
pub mod lzma;
pub mod media;
pub mod moviehash;
pub mod mpegps;
pub mod mpegts;
pub mod nest;
pub mod nfo_store;
pub mod opensubtitles;
pub mod step;
pub mod stills;
pub mod yenc;
pub mod yenc_quirks;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::postprocess::schema::ShardTable;
use crate::store;

pub use archive::{ArchiveFormat, ArchiveHead, ArchiveStream, DecompressError};
pub use media::{
    AudioBitstream, AudioProbeResult, AudioTrack, AvcDepth, Chapter, DiscInfo, DiscProber, DolbyVisionInfo, H265Sps,
    HevcDepth, MediaInfo, MediaProber, MkvMetadata, Mp4Prober, VideoTrack, VuiInfo,
};
pub use nest::{Layer, Nest, unwrap as unwrap_nested};
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
        (
            "release_probe_moviehash",
            "create index if not exists release_probe_moviehash on release_probe(moviehash) where moviehash is not null",
        ),
    ],
    release_key: Some("release_id"),
    added: &[("moviehash", "TEXT")],
};

/// Which cover art a release has. The image itself is a file in `covers/`, named by its SHA-256, soo a cover
/// posted again and again is kept once, and the folder is trimmed to `MAX_COVER_CACHE`, oldest first: what's
/// posted can't grow it without end.
pub const COVER: ShardTable = ShardTable {
    name: "release_cover",
    create: "create table if not exists release_cover (
        release_id INTEGER PRIMARY KEY,
        mime       TEXT NOT NULL,
        sha256     TEXT NOT NULL
    )",
    indexes: &[],
    release_key: Some("release_id"),
    added: &[],
};

pub const SET_COVER: &str = "insert or replace into release_cover (release_id, mime, sha256) values (?1, ?2, ?3)";
pub const DROP_COVER: &str = "delete from release_cover where release_id = ?1";

/// Past this, the oldest covers go. Most are a few hundred KB.
const MAX_COVER_CACHE: u64 = 1 << 30;
/// How many new covers between looks at the folder's size: a look reads the whole folder.
const TRIM_COVERS_EVERY: u64 = 64;

fn cover_dir() -> std::path::PathBuf {
    crate::paths::app_dir().join("covers")
}

pub(crate) fn cover_file(sha256: &str) -> Option<std::path::PathBuf> {
    (sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit())).then(|| cover_dir().join(sha256))
}

/// Keeps a cover's bytes, and gives the name to find them by.
pub fn keep_cover(bytes: &[u8]) -> Option<String> {
    use sha2::Digest;
    use std::sync::atomic::{AtomicU64, Ordering};
    let sha256: String = sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
    let file = cover_file(&sha256)?;
    if !file.exists() {
        static WRITTEN: AtomicU64 = AtomicU64::new(0);
        std::fs::create_dir_all(cover_dir()).ok()?;
        crate::atomic::write_atomic(&file, bytes).ok()?;
        if WRITTEN.fetch_add(1, Ordering::Relaxed).is_multiple_of(TRIM_COVERS_EVERY) {
            crate::media::details::trim_posters(&cover_dir(), MAX_COVER_CACHE);
        }
    }
    Some(sha256)
}

pub(crate) fn cover_row(conn: &rusqlite::Connection, release_id: i64) -> Option<(String, String)> {
    let s = store::shard_of_id(release_id);
    conn.query_row(&format!("select mime, sha256 from s{s}.release_cover where release_id = ?"), [release_id], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })
    .ok()
}

/// A release's cover art: its type and bytes, while the folder still has them.
pub fn cover(conn: &rusqlite::Connection, release_id: i64) -> Option<(String, Vec<u8>)> {
    let (mime, sha256) = cover_row(conn, release_id)?;
    Some((mime, std::fs::read(cover_file(&sha256)?).ok()?))
}

/// Whether a release's cover art can be shown, without reading it.
pub fn has_cover(conn: &rusqlite::Connection, release_id: i64) -> bool {
    cover_row(conn, release_id).and_then(|(_, sha256)| cover_file(&sha256)).is_some_and(|f| f.exists())
}

pub const SET_PROBE: &str = "insert or replace into release_probe (release_id, crc32, agent, info, moviehash)
    values (?1, ?2, ?3, ?4, ?5)";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProbeInfo {
    /// which of the release's files this is about
    pub file: Option<String>,
    pub yenc: Option<yenc::YencLines>,
    /// how the encoder wrote its yEnc, from the first article and the last (`yenc_quirks`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quirks: Option<yenc_quirks::Quirks>,
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
    /// every header name of the probed article, in the order it came, as sent: what the poster and the servers on
    /// the way wrote, and in what order (`header_set`). Names only, no values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub header_order: Vec<String>,
    pub crossposted_to: Vec<String>,
    pub archive: Option<archive::ArchiveHead>,
    /// every archive and compressed stream the file was followed down through, outermost first
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<nest::Layer>,
    pub media: Option<media::MediaInfo>,
    pub nfo: Option<String>,
    /// the release file `nfo` was read from
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nfo_file: Option<String>,
    pub imdb_ids: Vec<String>,
    pub sfv: BTreeMap<String, u32>,
    /// the OpenSubtitles hash of the video, or of the one inside a stored RAR set
    pub moviehash: Option<String>,
}

/// The CRC32 a Highwinds-family server or Usenet.Farm stamps on an article it received: over the body as posted,
/// CRLF line ends and the closing dot line included (`keys::received_body_crc` reads it).
pub const BODY_CRC_HEADER: &str = "X-Received-Body-CRC";

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
    BODY_CRC_HEADER,
    "X-Received-Bytes",
    // the article's own id and the thread it answers: a post whose parts answer its first article (`thread_root`)
    "Message-ID",
    "References",
];

/// Header names kept in order, at most, and how long each one may be.
const MAX_HEADER_NAMES: usize = 48;
const MAX_HEADER_NAME: usize = 64;

/// The names of an article's headers in the order it gives them.
pub fn header_order(headers: &[(String, String)]) -> Vec<String> {
    headers
        .iter()
        .take(MAX_HEADER_NAMES)
        .map(|(name, _)| name.chars().filter(|c| c.is_ascii_graphic()).take(MAX_HEADER_NAME).collect())
        .collect()
}

/// What the header-set fingerprint is taken over: the names in order, case kept, the ones a server adds on the way
/// left out (they say which server, not which poster). None with no names.
pub fn header_set(order: &[String]) -> Option<String> {
    const ON_THE_WAY: &[&str] = &[
        "path",
        "xref",
        "lines",
        "bytes",
        "x-received-bytes",
        "x-received-body-crc",
        "injection-date",
        "injection-info",
        "nntp-posting-date",
        "nntp-posting-host",
        "x-complaints-to",
        "x-trace",
        "organization",
        "x-abuse-info",
    ];
    let names: Vec<&str> =
        order.iter().map(String::as_str).filter(|n| !ON_THE_WAY.contains(&n.to_ascii_lowercase().as_str())).collect();
    (!names.is_empty()).then(|| format!("headers:{}", names.join(",")))
}

/// The id a post's thread hangs on: the first id its References name, else the article's own Message-ID. Two
/// releases whose probed articles hang on one id are parts of one post, as their poster threaded it.
pub fn thread_root(headers: &BTreeMap<String, String>) -> Option<String> {
    let first_id = |v: &str| {
        let start = v.find('<')?;
        let end = start + v.get(start..)?.find('>')?;
        v.get(start..=end).filter(|id| id.len() > 2).map(str::to_string)
    };
    headers.get("References").and_then(|r| first_id(r)).or_else(|| headers.get("Message-ID").and_then(|m| first_id(m)))
}

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
        if let (Some(segment), Some(total), Some(last)) = (info.segment_size, y.total, last) {
            let parts_agree = expected_parts.is_none_or(|e| e == i64::from(total));
            // the first part starts the file, and the last starts where the parts before it end, and ends it
            let last_begins = u64::from(total.saturating_sub(1)).saturating_mul(segment).saturating_add(1);
            let ends_meet = y.begin == Some(1) && last.begin == Some(last_begins) && last.end == Some(size);
            info.continuity_ok = Some(parts_agree && ends_meet);
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

/// IMDb ids kept from one NFO. An NFO comes from Usenet, and a few are all a real one has.
const MAX_IMDB_IDS: usize = 16;

pub fn imdb_ids(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut at = 0;
    while out.len() < MAX_IMDB_IDS
        && let Some(found) = text.get(at..).and_then(|t| t.find("tt"))
    {
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

        let short = yenc::YencLines { begin: Some(1001), end: Some(1500), ..Default::default() };
        let mut shifted = ProbeInfo { yenc: Some(first.clone()), ..ProbeInfo::default() };
        derive(&mut shifted, 1030, Some(3), Some(&short));
        assert_eq!(shifted.continuity_ok, Some(false), "the last part doesn't end the file");

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
        let many: String = (0..100).map(|n| format!("tt{:07} ", 1_000_000 + n)).collect();
        assert_eq!(imdb_ids(&many).len(), MAX_IMDB_IDS);
        let sfv = parse_sfv("; comment\nmovie.r00 0a1B2c3D\nbad line\n");
        assert_eq!(sfv["movie.r00"], 0x0a1b_2c3d);
        assert_eq!(sfv.len(), 1);
    }

    #[test]
    fn header_names_keep_their_order_and_the_poster_s_make_the_set() {
        let headers: Vec<(String, String)> =
            ["From", "Newsgroups", "Subject", "Message-ID", "Path", "X-Newsposter", "Date", "Xref"]
                .iter()
                .map(|n| (n.to_string(), "v".to_string()))
                .collect();
        let order = header_order(&headers);
        assert_eq!(order, ["From", "Newsgroups", "Subject", "Message-ID", "Path", "X-Newsposter", "Date", "Xref"]);
        assert_eq!(header_set(&order).as_deref(), Some("headers:From,Newsgroups,Subject,Message-ID,X-Newsposter,Date"));
        assert_eq!(header_set(&[]), None);
        let many: Vec<(String, String)> = (0..100).map(|i| (format!("X-{i}\u{7}"), String::new())).collect();
        let cut = header_order(&many);
        assert_eq!(cut.len(), MAX_HEADER_NAMES);
        assert_eq!(cut[0], "X-0", "control bytes dropped");
    }

    #[test]
    fn a_thread_hangs_on_the_first_reference_or_the_article_itself() {
        let mut h = BTreeMap::new();
        assert_eq!(thread_root(&h), None);
        h.insert("Message-ID".to_string(), " <self@x> ".to_string());
        assert_eq!(thread_root(&h).as_deref(), Some("<self@x>"));
        h.insert("References".to_string(), "<first@x> <second@x>".to_string());
        assert_eq!(thread_root(&h).as_deref(), Some("<first@x>"));
        h.insert("References".to_string(), "garbage <>".to_string());
        assert_eq!(thread_root(&h).as_deref(), Some("<self@x>"), "an empty id is none");
        let kept = keep_headers(&[("references".into(), "<a@b>".into()), ("message-id".into(), "<c@d>".into())]);
        assert_eq!(
            (kept.get("References").map(String::as_str), kept.get("Message-ID").map(String::as_str)),
            (Some("<a@b>"), Some("<c@d>"))
        );
    }
}
