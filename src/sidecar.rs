//! Sidecars: posts of nothing but what describes a release, an NFO or a mediainfo dump and the like, carrying none of
//! its data (`frozen::rules::SIDECAR`). They aren't releases of their own: the freeze rule takes them out of search.
//! Before it does, this step reads the text and keeps it here, with the name of the release it describes and, for a
//! mediainfo dump, the file's Matroska Segment UID, by which it can be matched to that release exactly.
//!
//! Kept by the sidecar's own release id but not with its row: a frozen release takes its rows with it, and these are
//! what's looked up when the release it describes is found.

use crate::config::Config;
use crate::frozen::rules::{SIDECAR_BYTES, SIDECAR_FILES};
use crate::postprocess::schema::ShardTable;
use crate::postprocess::step::{Needs, Outcome, ReleaseInfo, Step, StepCtx, StepError, StepId, StepWrite};
use crate::store::FileInfo;

pub const SIDECAR_STEP: StepId = StepId(11);

pub const SIDECARS: ShardTable = ShardTable {
    name: "sidecars",
    create: "create table if not exists sidecars (
        release_id   INTEGER PRIMARY KEY,
        name         TEXT NOT NULL,
        group_name   TEXT NOT NULL,
        file         TEXT,
        kind         TEXT NOT NULL,
        release_name TEXT,
        segment_uid  TEXT,
        text         TEXT,
        at           INTEGER NOT NULL
    )",
    indexes: &[
        (
            "sidecars_release_name",
            "create index if not exists sidecars_release_name on sidecars(release_name) where release_name is not null",
        ),
        (
            "sidecars_segment_uid",
            "create index if not exists sidecars_segment_uid on sidecars(segment_uid) where segment_uid is not null",
        ),
    ],
    // not the release's: a frozen release takes its rows along, and this one is looked up after it's gone
    release_key: None,
    added: &[],
};

const SET: &str = "insert or replace into sidecars
    (release_id, name, group_name, file, kind, release_name, segment_uid, text, at) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";

/// What's kept of a sidecar's text at most
pub const MAX_TEXT: usize = 64 * 1024;
/// articles a sidecar's text file is read in at most
const MAX_PARTS: usize = 8;

/// Whether a file is one a sidecar is made of, by its name.
pub fn is_sidecar_file(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(stem, ext)| !stem.is_empty() && SIDECAR_FILES.iter().any(|e| e.eq_ignore_ascii_case(ext)))
}

/// Whether these files, of a release that says it has `file_total` of them, make a sidecar: as the freeze rule has it.
pub fn is_sidecar(files: &[FileInfo], file_total: Option<i64>, size: i64) -> bool {
    !files.is_empty()
        && size > 0
        && size <= SIDECAR_BYTES
        && file_total.unwrap_or(0) <= files.len() as i64
        && files.iter().all(|f| is_sidecar_file(&f.filename))
}

/// What a sidecar's text says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Read {
    /// `mediainfo` for a MediaInfo dump, `nfo` for anything else posted as an NFO, `text` for the rest
    pub kind: &'static str,
    /// the release it describes: a mediainfo dump's `Complete name` without its extension, or the name an NFO gives
    pub release_name: Option<String>,
    /// a mediainfo dump's `Unique ID`, as 32 hex digits
    pub segment_uid: Option<String>,
}

/// Reads a sidecar file's text.
pub fn read(file: &str, body: &[u8]) -> Read {
    let text = String::from_utf8_lossy(body.get(..MAX_TEXT).unwrap_or(body));
    let field = |label: &str| {
        text.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == label).then(|| value.trim().to_string()).filter(|v| !v.is_empty())
        })
    };
    if let Some(complete) = field("Complete name") {
        let base = complete.rsplit(['/', '\\']).next().unwrap_or(&complete);
        let name = base.rsplit_once('.').map_or(base, |(stem, _)| stem).to_string();
        let segment_uid = field("Unique ID").and_then(|uid| uid_hex(&uid));
        return Read { kind: "mediainfo", release_name: (!name.is_empty()).then_some(name), segment_uid };
    }
    let nfo = file.to_ascii_lowercase().ends_with(".nfo");
    let release_name = crate::nfo::name_candidates(body).into_iter().next();
    Read { kind: if nfo { "nfo" } else { "text" }, release_name, segment_uid: None }
}

/// `114273598891547003088333253660066868446 (0x55F84B6AA194D59703CCAB394DA26CDE)` as 32 lowercase hex digits.
fn uid_hex(value: &str) -> Option<String> {
    if let Some(at) = value.find("0x") {
        let hex: String = value[at + 2..].chars().take_while(char::is_ascii_hexdigit).collect();
        return (hex.len() == 32).then(|| hex.to_ascii_lowercase());
    }
    let dec: String = value.chars().take_while(char::is_ascii_digit).collect();
    dec.parse::<u128>().ok().map(|n| format!("{n:032x}"))
}

/// The text file to read first: a mediainfo dump, then an NFO, then any other text.
fn pick(files: &[FileInfo]) -> Option<&FileInfo> {
    let lower = |f: &FileInfo| f.filename.to_ascii_lowercase();
    files
        .iter()
        .find(|f| lower(f).contains("mediainfo"))
        .or_else(|| files.iter().find(|f| lower(f).ends_with(".nfo")))
        .or_else(|| files.iter().find(|f| lower(f).ends_with(".txt") || lower(f).ends_with(".diz")))
}

#[derive(Default)]
pub struct SidecarStep;

impl Step for SidecarStep {
    fn id(&self) -> StepId {
        SIDECAR_STEP
    }
    fn name(&self) -> &'static str {
        "sidecar"
    }
    fn rank(&self) -> u16 {
        230
    }
    fn needs(&self) -> Needs {
        Needs::Bodies
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.archive.enabled
    }
    fn applies(&self, r: &ReleaseInfo) -> bool {
        r.size > 0 && r.size <= SIDECAR_BYTES
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let files = ctx.files().map_err(|e| StepError::Retry(e.to_string()))?;
        let r = ctx.release().clone();
        if !is_sidecar(&files, r.file_total, r.size) {
            return Ok(Outcome::default());
        }
        let picked = pick(&files);
        let mut left = MAX_PARTS;
        let body = match picked {
            Some(file) => crate::archive::steps::whole_file(ctx, file, &mut left)?,
            None => None,
        };
        let read = body.as_deref().map(|b| read(picked.map_or("", |f| f.filename.as_str()), b));
        let text = body.as_deref().map(|b| String::from_utf8_lossy(b.get(..MAX_TEXT).unwrap_or(b)).into_owned());
        let kind = read.as_ref().map_or("unread", |r| r.kind);
        let params = vec![
            r.id.into(),
            r.name.clone().into(),
            r.group.clone().into(),
            picked.map(|f| f.filename.clone()).into(),
            kind.to_string().into(),
            read.as_ref().and_then(|r| r.release_name.clone()).into(),
            read.as_ref().and_then(|r| r.segment_uid.clone()).into(),
            text.into(),
            chrono::Utc::now().timestamp().into(),
        ];
        Ok(Outcome { matched: body.is_some(), writes: vec![StepWrite::new(SET, params)], ..Outcome::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEDIAINFO: &str = "General
Unique ID                                : 114273598891547003088333253660066868446 (0x55F84B6AA194D59703CCAB394DA26CDE)
Complete name                            : ptbm-tourist.trap.1979.alternative.cut.german.dl.1080p.bluray.avc.mkv
Format                                   : Matroska
";

    #[test]
    fn a_mediainfo_dump_gives_its_files_name_and_segment_uid() {
        let r = read("mediainfo.txt", MEDIAINFO.as_bytes());
        assert_eq!(r.kind, "mediainfo");
        assert_eq!(
            r.release_name.as_deref(),
            Some("ptbm-tourist.trap.1979.alternative.cut.german.dl.1080p.bluray.avc")
        );
        assert_eq!(r.segment_uid.as_deref(), Some("55f84b6aa194d59703ccab394da26cde"));
        // the decimal alone says the same
        assert_eq!(
            uid_hex("114273598891547003088333253660066868446").as_deref(),
            Some("55f84b6aa194d59703ccab394da26cde")
        );
        let path = read("x.txt", b"Complete name : D:\\rips\\Some.Film.2024.mkv\n");
        assert_eq!(path.release_name.as_deref(), Some("Some.Film.2024"));
    }

    #[test]
    fn an_nfo_gives_the_name_it_carries() {
        let nfo =
            b"   T_ H_ E_  G_ R_ O_ U_ P_\n\n    Some.Movie.2024.1080p.BluRay.x264-GRP\n\n RELEASE DATE : 2024.06.14\n";
        let r = read("grp.nfo", nfo);
        assert_eq!((r.kind, r.release_name.as_deref()), ("nfo", Some("Some.Movie.2024.1080p.BluRay.x264-GRP")));
        assert_eq!(read("notes.txt", b"nothing here").kind, "text");
    }

    #[test]
    fn sidecar_files_by_name() {
        for name in ["a.nfo", "mediainfo.txt", "a.SFV", "a.srr", "a.srs", "a.md5", "a.sha1", "file_id.diz"] {
            assert!(is_sidecar_file(name), "{name}");
        }
        for name in ["a.mkv", "a.rar", "a.nzb", "nfo", ".nfo"] {
            assert!(!is_sidecar_file(name), "{name}");
        }
    }
}
