//! Sidecar parsing and native Step results for NFO/mediainfo/image-only posts.
//! PostgreSQL owns storage and quota settlement. Retained names and Segment
//! UIDs associate a sidecar with the release it describes after its own post
//! leaves the visible catalog.

use crate::config::Config;
use crate::frozen::rules::{SIDECAR_BYTES, SIDECAR_FILES, SIDECAR_IMAGES};
use crate::postprocess::step::{
    Needs, Outcome, ReleaseInfo, Reruns, SidecarRecord, Step, StepCtx, StepError, StepId, StepMutation, StepWrite,
};
use crate::store::FileInfo;

pub const SIDECAR_STEP: StepId = StepId(11);
pub const SIDECAR_IMAGE_STEP: StepId = StepId(12);

/// Frozen v1 result-digest compatibility label; never executed in this module.
/// A rerun that couldn't read anything (`unread`) leaves what an earlier read found.
pub(crate) const SET: &str = "insert into sidecars
    (release_id, name, group_name, file, kind, release_name, segment_uid, text, at)
    values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
    on conflict(release_id) do update set name = excluded.name, group_name = excluded.group_name,
        file = case when excluded.kind = 'unread' then sidecars.file else excluded.file end,
        kind = case when excluded.kind = 'unread' then sidecars.kind else excluded.kind end,
        release_name = case when excluded.kind = 'unread' then sidecars.release_name else excluded.release_name end,
        segment_uid = case when excluded.kind = 'unread' then sidecars.segment_uid else excluded.segment_uid end,
        text = coalesce(excluded.text, sidecars.text), at = excluded.at";

/// Frozen v1 result-digest compatibility label, not a storage entrypoint.
/// After `SET` for release ?1: while the shard's texts hold more than ?2 bytes, the oldest texts but ?1's go, just
/// enough of them; their names and UIDs stay. Soo the newest sidecars always have their text, and the total holds.
pub(crate) const EVICT: &str = "update sidecars set text = null where release_id in (
    select release_id from (
        select release_id, length(cast(text as blob)) as len,
            sum(length(cast(text as blob))) over (order by at, release_id rows unbounded preceding) as run
        from sidecars indexed by sidecars_text_at where text is not null and release_id != ?1)
    where run - len < coalesce((select value from meta where key = 'sidecar_text_bytes'), 0) - ?2)";

/// What a release name read from a sidecar is kept to: a real one is far shorter, and these rows outlive releases
pub const MAX_NAME: usize = 255;

/// A sidecar as the page of the release it describes shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// the sidecar post's own name, group and the file read
    pub name: String,
    pub group: String,
    pub file: Option<String>,
    pub kind: String,
    /// how it was matched: `segment_uid` (the same MKV, exactly) or `name`
    pub by: &'static str,
    pub text: Option<String>,
}

/// At most `MAX_NAME` bytes of a name, cut on a character. Empty: none.
fn capped_name(name: &str) -> Option<String> {
    let name = name.trim();
    let mut end = name.len().min(MAX_NAME);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    (end > 0).then(|| name[..end].to_string())
}

/// What's kept of a sidecar's text at most
pub const MAX_TEXT: usize = 64 * 1024;
/// and of all of a shard's sidecar texts: anyone can post text, and these rows outlive their releases
pub const MAX_TOTAL_TEXT: i64 = 128 << 20;
/// articles a sidecar's text file is read in at most
const MAX_PARTS: usize = 8;

/// Whether a file is one a sidecar is made of, by its name.
pub fn is_sidecar_file(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(stem, ext)| !stem.is_empty() && SIDECAR_FILES.iter().any(|e| e.eq_ignore_ascii_case(ext)))
}

/// Whether a file is a cover or poster image a sidecar may hold, by its name.
pub fn is_sidecar_image(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(stem, ext)| !stem.is_empty() && SIDECAR_IMAGES.iter().any(|e| e.eq_ignore_ascii_case(ext)))
}

/// Whether these files, of a release that says it has `file_total` of them, make a sidecar: as the freeze rule has it.
/// Images count beside the text files, or one alone: several images and nothing else are a picture post.
pub fn is_sidecar(files: &[FileInfo], file_total: Option<i64>, size: i64) -> bool {
    !files.is_empty()
        && size > 0
        && size <= SIDECAR_BYTES
        && file_total.unwrap_or(0) <= files.len() as i64
        && files.iter().all(|f| is_sidecar_file(&f.filename) || is_sidecar_image(&f.filename))
        && (files.len() == 1 || files.iter().any(|f| is_sidecar_file(&f.filename)))
}

/// The release a lone cover image is for, from its post's name: `X-cover.jpg`, `X.poster.png` and the like give `X`.
pub fn image_release_name(name: &str) -> Option<String> {
    let stem = name.rsplit_once('.').filter(|(_, ext)| SIDECAR_IMAGES.iter().any(|e| e.eq_ignore_ascii_case(ext)));
    let stem = stem.map_or(name, |(stem, _)| stem);
    let lower = stem.to_ascii_lowercase();
    let cut = ["cover", "poster", "folder", "fanart", "front"]
        .iter()
        .flat_map(|word| ["-", ".", "_", " "].iter().map(move |sep| format!("{sep}{word}")))
        .find(|suffix| lower.ends_with(suffix.as_str()))
        .map_or(stem.len(), |suffix| stem.len() - suffix.len());
    capped_name(&stem[..cut])
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

/// A sidecar file's text, as NFOs are read: UTF-8, or else code page 437, as the whole file says, then cut to at most
/// `MAX_TEXT` bytes on a character.
pub fn text_of(body: &[u8]) -> String {
    let mut text = crate::probe::nfo_text(body);
    if text.len() > MAX_TEXT {
        let mut end = MAX_TEXT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// Reads a sidecar file's text.
pub fn read(file: &str, body: &[u8]) -> Read {
    let text = text_of(body);
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
        return Read { kind: "mediainfo", release_name: capped_name(&name), segment_uid };
    }
    let nfo = file.to_ascii_lowercase().ends_with(".nfo");
    let release_name = crate::nfo::name_candidates(text.as_bytes()).into_iter().next().and_then(|n| capped_name(&n));
    Read { kind: if nfo { "nfo" } else { "text" }, release_name, segment_uid: None }
}

/// `114273598891547003088333253660066868446 (0x55F84B6AA194D59703CCAB394DA26CDE)` as 32 lowercase hex digits, leading
/// zeros kept as the probe keeps them: from the hex when it reads, else from the decimal.
fn uid_hex(value: &str) -> Option<String> {
    let hex = value.find("0x").and_then(|at| {
        let digits: String = value[at + 2..].chars().take_while(char::is_ascii_hexdigit).collect();
        u128::from_str_radix(&digits, 16).ok()
    });
    let dec = || value.trim().chars().take_while(char::is_ascii_digit).collect::<String>().parse::<u128>().ok();
    hex.or_else(dec).map(|n| format!("{n:032x}"))
}

/// The text file to read first: a mediainfo dump, then an NFO, then any other text.
/// Never an image, whatever it's called.
fn pick(files: &[FileInfo]) -> Option<&FileInfo> {
    let lower = |f: &FileInfo| f.filename.to_ascii_lowercase();
    let text = || files.iter().filter(|f| is_sidecar_file(&f.filename));
    text()
        .find(|f| lower(f).contains("mediainfo"))
        .or_else(|| text().find(|f| lower(f).ends_with(".nfo")))
        .or_else(|| text().find(|f| lower(f).ends_with(".txt") || lower(f).ends_with(".diz")))
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
    // a post that settled before all of it came is looked at again: it can turn out a sidecar, or its text whole
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: false, on_growth: true }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let files = ctx.files().map_err(StepError::from_inventory)?;
        let r = ctx.release().clone();
        if !is_sidecar(&files, r.file_total, r.size) {
            return Ok(Outcome::default());
        }
        // a lone cover image has nothing to read: `SidecarImageStep` keeps it, as no body step runs for images
        if files.iter().all(|f| is_sidecar_image(&f.filename)) {
            return Ok(Outcome::default());
        }
        let picked = pick(&files);
        let mut left = MAX_PARTS;
        let body = match picked {
            Some(file) => crate::archive::steps::whole_file(ctx, file, &mut left)?,
            None => None,
        };
        let read = body.as_deref().map(|b| read(picked.map_or("", |f| f.filename.as_str()), b));
        let text = body.as_deref().map(text_of);
        let stored = text.as_ref().map_or(0, String::len);
        let kind = read.as_ref().map_or("unread", |r| r.kind);
        let record = SidecarRecord {
            name: r.name.clone(),
            group: r.group.clone(),
            file: picked.map(|f| f.filename.clone()),
            kind: kind.to_string(),
            release_name: read.as_ref().and_then(|r| r.release_name.clone()),
            segment_uid: read.as_ref().and_then(|r| r.segment_uid.clone()),
            text,
            at: chrono::Utc::now().timestamp(),
        };
        let mut writes = vec![StepWrite::for_mutation(r.id, StepMutation::KeepSidecar(record))];
        // the oldest texts are looked at only when this one takes the shard past its total: below it, nothing scans
        if ctx.data().sidecar_text_bytes()?.saturating_add(stored as i64) > MAX_TOTAL_TEXT {
            writes.push(StepWrite::for_mutation(r.id, StepMutation::EvictSidecarTexts { max_bytes: MAX_TOTAL_TEXT }));
        }
        Ok(Outcome { matched: body.is_some(), writes, ..Outcome::default() })
    }
}

/// A lone cover image posted on its own: kept as a sidecar of kind `image`, named after its post, with nothing read.
/// A database step: the processing stage reads no image's body, and none is needed here.
#[derive(Default)]
pub struct SidecarImageStep;

impl Step for SidecarImageStep {
    fn id(&self) -> StepId {
        SIDECAR_IMAGE_STEP
    }
    fn name(&self) -> &'static str {
        "sidecar_image"
    }
    fn rank(&self) -> u16 {
        235
    }
    fn needs(&self) -> Needs {
        Needs::Database
    }
    fn applies(&self, r: &ReleaseInfo) -> bool {
        r.size > 0 && r.size <= SIDECAR_BYTES
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: false, on_growth: true }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let files = ctx.files().map_err(StepError::from_inventory)?;
        let r = ctx.release().clone();
        if !is_sidecar(&files, r.file_total, r.size) || !files.iter().all(|f| is_sidecar_image(&f.filename)) {
            return Ok(Outcome::default());
        }
        let name = image_release_name(&r.name).or_else(|| files.first().and_then(|f| image_release_name(&f.filename)));
        let record = SidecarRecord {
            name: r.name.clone(),
            group: r.group.clone(),
            file: files.first().map(|f| f.filename.clone()),
            kind: "image".into(),
            release_name: name,
            segment_uid: None,
            text: None,
            at: chrono::Utc::now().timestamp(),
        };
        Ok(Outcome {
            matched: true,
            writes: vec![StepWrite::for_mutation(r.id, StepMutation::KeepSidecar(record))],
            ..Outcome::default()
        })
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
        // leading zero bytes, written short
        assert_eq!(uid_hex("1 (0x1)").as_deref(), Some("00000000000000000000000000000001"));
        assert_eq!(
            uid_hex("255 (0xzz)").as_deref(),
            Some("000000000000000000000000000000ff"),
            "the decimal when the hex won't read"
        );
        let long = format!("Complete name : {}.mkv\n", "x".repeat(60_000));
        assert_eq!(read("x.txt", long.as_bytes()).release_name.map(|n| n.len()), Some(MAX_NAME));
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
    fn a_long_utf8_text_is_still_utf8_where_it_is_cut() {
        let mut body = "Complete name : Amélie.2001.mkv\n".as_bytes().to_vec();
        // the cut falls inside a two-byte character
        while body.len() < MAX_TEXT - 1 {
            body.push(b' ');
        }
        body.extend("é and more".as_bytes());
        let text = text_of(&body);
        assert!(text.len() <= MAX_TEXT && text.starts_with("Complete name : Amélie"));
        assert_eq!(read("mediainfo.txt", &body).release_name.as_deref(), Some("Amélie.2001"));
    }

    #[test]
    fn a_cp437_nfo_is_read_as_one() {
        let nfo = b"\xdb\xdb Some.Movie.2024.1080p.BluRay.x264-GRP \xdb\xdb\n";
        assert!(text_of(nfo).starts_with("\u{2588}\u{2588} Some.Movie"));
        assert_eq!(read("x.nfo", nfo).release_name.as_deref(), Some("Some.Movie.2024.1080p.BluRay.x264-GRP"));
    }

    #[test]
    fn a_cover_image_is_a_sidecar_alone_or_beside_text_but_pictures_alone_are_not() {
        let file = |name: &str| FileInfo {
            id: 1,
            filename: name.into(),
            subject: None,
            subject_part: None,
            expected: Some(1),
            file_total: None,
            parts_seen: 1,
            touched_at: None,
            sealed: false,
        };
        assert!(is_sidecar(&[file("X-cover.jpg")], None, 729_538), "the boneless cover post");
        assert!(is_sidecar(&[file("x.nfo"), file("cover.jpg"), file("poster.png")], None, 900_000));
        assert!(!is_sidecar(&[file("a.jpg"), file("b.jpg")], None, 900_000), "a picture post");
        assert!(!is_sidecar(&[file("X-cover.jpg")], None, 5 << 20), "past the size");
        assert_eq!(
            image_release_name("Verity.Dunkle.Geheimnisse.2026.GERMAN.DL.720P.WEB.H264-WAYNE-cover.jpg").as_deref(),
            Some("Verity.Dunkle.Geheimnisse.2026.GERMAN.DL.720P.WEB.H264-WAYNE")
        );
        assert_eq!(image_release_name("Some.Film.2024.poster.PNG").as_deref(), Some("Some.Film.2024"));
        // the text file is read, never an image called mediainfo
        let mixed = [file("mediainfo.png"), file("release.nfo")];
        assert_eq!(pick(&mixed).map(|f| f.filename.as_str()), Some("release.nfo"));
        assert_eq!(
            image_release_name("Some.Film.2024").as_deref(),
            Some("Some.Film.2024"),
            "a post's name without its file's"
        );
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

#[cfg(all(test, unix))]
#[path = "sidecar/native_tests.rs"]
mod native_tests;
