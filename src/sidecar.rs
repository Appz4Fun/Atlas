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
use crate::postprocess::step::{Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId, StepWrite};
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
    );
    -- what the texts hold in all, for `MAX_TOTAL_TEXT`
    create trigger if not exists sidecars_text_in after insert on sidecars begin
        insert into meta (key, value) values ('sidecar_text_bytes', coalesce(length(cast(new.text as blob)), 0))
            on conflict(key) do update set value = value + excluded.value;
    end;
    create trigger if not exists sidecars_text_out after delete on sidecars begin
        update meta set value = max(0, value - coalesce(length(cast(old.text as blob)), 0))
            where key = 'sidecar_text_bytes';
    end;
    create trigger if not exists sidecars_text_changed after update of text on sidecars begin
        update meta set value = max(0, value - coalesce(length(cast(old.text as blob)), 0)
            + coalesce(length(cast(new.text as blob)), 0)) where key = 'sidecar_text_bytes';
    end",
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

/// The text is kept only while the shard's texts hold less than ?10 bytes: past that, the name and UID still are, and
/// a text kept before stays. A rerun that couldn't read anything (`unread`) leaves what an earlier read found.
const SET: &str = "insert into sidecars
    (release_id, name, group_name, file, kind, release_name, segment_uid, text, at)
    values (?1, ?2, ?3, ?4, ?5, ?6, ?7,
        case when coalesce((select value from meta where key = 'sidecar_text_bytes'), 0) < ?10 then ?8 end, ?9)
    on conflict(release_id) do update set name = excluded.name, group_name = excluded.group_name,
        file = case when excluded.kind = 'unread' then sidecars.file else excluded.file end,
        kind = case when excluded.kind = 'unread' then sidecars.kind else excluded.kind end,
        release_name = case when excluded.kind = 'unread' then sidecars.release_name else excluded.release_name end,
        segment_uid = case when excluded.kind = 'unread' then sidecars.segment_uid else excluded.segment_uid end,
        text = coalesce(excluded.text, sidecars.text), at = excluded.at";

/// What a release name read from a sidecar is kept to: a real one is far shorter, and these rows outlive releases
pub const MAX_NAME: usize = 255;

/// The shard's `sidecar_text_bytes` counted again from what `sidecars` holds, as after a compaction's copy.
pub fn recount(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute(
        "insert or replace into meta (key, value)
         select 'sidecar_text_bytes', coalesce(sum(length(cast(text as blob))), 0) from sidecars",
        [],
    )?;
    Ok(())
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
    // a post that settled before all of it came is looked at again: it can turn out a sidecar, or its text whole
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: false, on_growth: true }
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
        let text = body.as_deref().map(text_of);
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
            MAX_TOTAL_TEXT.into(),
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
    fn sidecar_texts_stop_at_the_shards_total_but_their_names_dont() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("create table meta (key TEXT PRIMARY KEY, value INTEGER);").unwrap();
        conn.execute_batch(SIDECARS.create).unwrap();
        let put = |id: i64, text: &str, cap: i64| {
            let p: Vec<rusqlite::types::Value> = vec![
                id.into(),
                "n".to_string().into(),
                "g".to_string().into(),
                None::<String>.into(),
                "nfo".to_string().into(),
                Some(format!("Name.{id}")).into(),
                None::<String>.into(),
                text.to_string().into(),
                0i64.into(),
                cap.into(),
            ];
            conn.execute(SET, rusqlite::params_from_iter(p)).unwrap();
        };
        let total = || {
            conn.query_row("select value from meta where key = 'sidecar_text_bytes'", [], |r| r.get::<_, i64>(0))
                .unwrap()
        };
        put(1, "aaaa", 10);
        put(2, "bbbbbbbb", 10);
        assert_eq!(total(), 12);
        put(3, "cccc", 10);
        let (name, text): (String, Option<String>) = conn
            .query_row("select release_name, text from sidecars where release_id = 3", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((name.as_str(), text), ("Name.3", None), "past the total: the name, not the text");
        put(3, "dddd", 10);
        let kept: Option<String> =
            conn.query_row("select text from sidecars where release_id = 2", [], |r| r.get(0)).unwrap();
        assert_eq!(kept.as_deref(), Some("bbbbbbbb"));
        put(2, "zzzz", 10);
        let kept: Option<String> =
            conn.query_row("select text from sidecars where release_id = 2", [], |r| r.get(0)).unwrap();
        assert_eq!(kept.as_deref(), Some("bbbbbbbb"), "a rerun past the total keeps the text it had");
        // a rerun that read nothing keeps what was found
        let unread: Vec<rusqlite::types::Value> = vec![
            2i64.into(),
            "n".to_string().into(),
            "g".to_string().into(),
            None::<String>.into(),
            "unread".to_string().into(),
            None::<String>.into(),
            None::<String>.into(),
            None::<String>.into(),
            0i64.into(),
            100i64.into(),
        ];
        conn.execute(SET, rusqlite::params_from_iter(unread)).unwrap();
        let (kind, name): (String, Option<String>) = conn
            .query_row("select kind, release_name from sidecars where release_id = 2", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((kind.as_str(), name.as_deref()), ("nfo", Some("Name.2")));
        put(2, "b", 100);
        assert_eq!(total(), 5, "a rewrite gives back what it held");
        conn.execute("delete from sidecars where release_id = 1", []).unwrap();
        assert_eq!(total(), 1);
        conn.execute("update meta set value = 999 where key = 'sidecar_text_bytes'", []).unwrap();
        recount(&conn).unwrap();
        assert_eq!(total(), 1, "counted again from the rows, as after a compaction");
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
