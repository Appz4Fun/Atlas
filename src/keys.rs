//! Keys worth indexing: the few keys that tie a release to others, kept in one small table soo the release page
//! can say "in 12 other releases" and list them.
//!
//! Each key is the first 8 bytes of a SHA-256, stored as a big-endian i64, soo a row is four integers and an index
//! entry is one. The page prints the same digest as hex: 16 digits for the layout and Lines keys, the first 12 for
//! the salted account and host keys. Two keys the twin and probe work already index are counted from their own
//! tables, not copied here: the PAR2 set id (`par2_files.set_id`) and the OpenSubtitles hash
//! (`release_probe.moviehash`).
//!
//! These are candidates for a reader, never a merge: no key here groups, links or completes anything on its own.
//! The raw posting account and host are never stored, only their salted digests.

use std::sync::OnceLock;

use rusqlite::{Connection, OptionalExtension, params_from_iter};
use sha2::{Digest, Sha256};

use crate::postprocess::schema::ShardTable;
use crate::postprocess::step::{Needs, Outcome, Reruns, Step, StepCtx, StepError, StepId, StepWrite};
use crate::probe::ProbeInfo;
use crate::store;

pub const RELEASE_KEYS: ShardTable = ShardTable {
    name: "release_keys",
    create: "create table if not exists release_keys (
        release_id INTEGER PRIMARY KEY,
        account    INTEGER,
        host       INTEGER,
        layout     INTEGER,
        lines      INTEGER
    )",
    indexes: &[
        (
            "release_keys_account",
            "create index if not exists release_keys_account on release_keys(account) where account is not null",
        ),
        (
            "release_keys_host",
            "create index if not exists release_keys_host on release_keys(host) where host is not null",
        ),
        (
            "release_keys_layout",
            "create index if not exists release_keys_layout on release_keys(layout) where layout is not null",
        ),
        (
            "release_keys_lines",
            "create index if not exists release_keys_lines on release_keys(lines) where lines is not null",
        ),
        (
            "release_keys_video",
            "create index if not exists release_keys_video on release_keys(video) where video is not null",
        ),
    ],
    release_key: Some("release_id"),
    // the first keyframe's key, from the probe's `keyframe_key`
    added: &[("video", "INTEGER")],
};

const UPSERT: &str = "insert or replace into release_keys (release_id, account, host, layout, lines, video)
     values (?1, ?2, ?3, ?4, ?5, ?6)";
const DELETE: &str = "delete from release_keys where release_id = ?1";

pub const KEYS_STEP: StepId = StepId(40);

/// The most other releases a count goes to, and the most the list page shows.
pub const COUNT_CAP: i64 = 1_000;
pub const LIST_CAP: i64 = 100;
/// PAR2 sets a release is looked up by, at most: nearly every release has one.
const SETS_CAP: usize = 4;
/// Files a Lines key is taken over, at most: the page reads as many (`web_meta::FILES_CAP`).
const FILES_CAP: i64 = 5_000;

/// What another release can share with this one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Account,
    Host,
    Layout,
    Lines,
    Par2,
    MovieHash,
    Video,
}

impl Kind {
    pub const ALL: [Kind; 7] =
        [Kind::Account, Kind::Host, Kind::Layout, Kind::Lines, Kind::Par2, Kind::MovieHash, Kind::Video];

    /// The word in `/ui/same?kind=`.
    pub fn word(self) -> &'static str {
        match self {
            Kind::Account => "account",
            Kind::Host => "host",
            Kind::Layout => "layout",
            Kind::Lines => "lines",
            Kind::Par2 => "par2",
            Kind::MovieHash => "moviehash",
            Kind::Video => "video",
        }
    }

    pub fn parse(word: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.word() == word)
    }

    /// What the releases on the list have in common, for its heading.
    pub fn heading(self) -> &'static str {
        match self {
            Kind::Account => "Same posting account",
            Kind::Host => "Same posting host",
            Kind::Layout => "Same track layout and length",
            Kind::Lines => "Same Lines per file",
            Kind::Par2 => "Same PAR2 set",
            Kind::MovieHash => "Same OpenSubtitles hash",
            Kind::Video => "Same video stream",
        }
    }

    /// One sentence under the heading: what sharing the key means, and what it doesn't.
    pub fn about(self) -> &'static str {
        match self {
            Kind::Account => {
                "Posted from the same account at the injecting server, going by a salted hash of it: the same uploader, as a rule, whatever the names say."
            }
            Kind::Host => {
                "Posted from the same host, going by a salted hash of it. A shared posting service puts many uploaders behind one host."
            }
            Kind::Layout => {
                "The same video, audio and subtitle codecs in the same order, the same chapter count and the same length to the second: often the same encode, renamed or posted again. Not proof."
            }
            Kind::Lines => {
                "Every file posted with the same number of lines: the same post made again, as a rule. Kept for releases indexed since Atlas began timing articles."
            }
            Kind::Par2 => "The same PAR2 recovery set: the same files, whoever posted them.",
            Kind::MovieHash => {
                "The same OpenSubtitles hash over the main file's first and last 64 KB and its size: the same file, as a rule."
            }
            Kind::Video => {
                "The same first keyframe, to the bit: the same encode or the same disc's video, remuxed, renamed or in another container, whoever posted it and in whatever group. Another encode of the same film has another."
            }
        }
    }

    /// The `release_keys` column, for the kinds kept there.
    fn column(self) -> Option<&'static str> {
        match self {
            Kind::Account => Some("account"),
            Kind::Host => Some("host"),
            Kind::Layout => Some("layout"),
            Kind::Lines => Some("lines"),
            Kind::Video => Some("video"),
            Kind::Par2 | Kind::MovieHash => None,
        }
    }
}

/// The keys `release_keys` keeps for a release. All None: no row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keys {
    pub account: Option<i64>,
    pub host: Option<i64>,
    pub layout: Option<i64>,
    pub lines: Option<i64>,
    pub video: Option<i64>,
}

impl Keys {
    pub fn is_empty(&self) -> bool {
        *self == Keys::default()
    }

    fn get(&self, kind: Kind) -> Option<i64> {
        match kind {
            Kind::Account => self.account,
            Kind::Host => self.host,
            Kind::Layout => self.layout,
            Kind::Lines => self.lines,
            Kind::Video => self.video,
            Kind::Par2 | Kind::MovieHash => None,
        }
    }
}

/// What a release can be looked up by: its `release_keys`, its PAR2 set ids and its OpenSubtitles hash.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lookup {
    pub keys: Keys,
    pub par2_sets: Vec<Vec<u8>>,
    pub moviehash: Option<String>,
}

impl Lookup {
    pub fn has(&self, kind: Kind) -> bool {
        match kind {
            Kind::Par2 => !self.par2_sets.is_empty(),
            Kind::MovieHash => self.moviehash.is_some(),
            _ => self.keys.get(kind).is_some(),
        }
    }
}

// ---------------------------------------------------------------- the digests

/// SHA-256, first 8 bytes.
pub fn digest8(bytes: &[u8]) -> [u8; 8] {
    let d = Sha256::digest(bytes);
    let mut out = [0u8; 8];
    out.copy_from_slice(&d[..8]);
    out
}

/// SHA-256 over the salt and the value, first 8 bytes: lines two postings up without naming the account.
pub fn salted8(salt: i64, value: &str) -> [u8; 8] {
    let mut h = Sha256::new();
    h.update(salt.to_le_bytes());
    h.update(value.as_bytes());
    let mut out = [0u8; 8];
    out.copy_from_slice(&h.finalize()[..8]);
    out
}

/// A digest as the integer `release_keys` stores.
pub fn as_key(digest: [u8; 8]) -> i64 {
    i64::from_be_bytes(digest)
}

/// A stored key as the page prints it: the digest's 16 hex digits.
pub fn hex16(key: i64) -> String {
    format!("{:016x}", key as u64)
}

/// A salted key as the page prints it: the digest's first 12 hex digits.
pub fn hex12(key: i64) -> String {
    hex16(key)[..12].to_string()
}

/// The `key=value;` parts of an Injection-Info header that name who posted. The values stay in this module and
/// `web_meta`: the pages print their salted keys only.
#[derive(Default)]
pub(crate) struct InjectionParts {
    pub account: Option<String>,
    pub host: Option<String>,
    pub logging_data: bool,
    pub complaints: bool,
}

pub(crate) fn injection_parts(info: &str) -> InjectionParts {
    let mut out = InjectionParts::default();
    for part in info.split(';') {
        let Some((key, value)) = part.split_once('=') else { continue };
        let value = value.trim().trim_matches('"').trim();
        if value.is_empty() {
            continue;
        }
        match key.trim().to_ascii_lowercase().as_str() {
            "posting-account" => out.account = Some(value.to_string()),
            "posting-host" => out.host = Some(value.to_string()),
            "logging-data" => out.logging_data = true,
            "mail-complaints-to" => out.complaints = true,
            _ => {}
        }
    }
    out
}

/// The posting account and host from a probe's headers: Injection-Info's `posting-account` and `posting-host`,
/// the host from `NNTP-Posting-Host` when Injection-Info names none.
pub(crate) fn account_and_host(probe: &ProbeInfo) -> (Option<String>, Option<String>) {
    let parts = probe.headers.get("Injection-Info").map(|info| injection_parts(info)).unwrap_or_default();
    let host = parts.host.or_else(|| probe.headers.get("NNTP-Posting-Host").map(|h| h.trim().to_string()));
    (parts.account, host.filter(|h| !h.is_empty()))
}

/// What the layout key is taken over: the codecs in track order, the chapter count and the duration to the
/// second. None for a probe that found no tracks and no duration.
pub(crate) fn layout_text(media: &crate::probe::media::MediaInfo) -> Option<String> {
    if media.video.is_empty() && media.audio.is_empty() && media.subtitles.is_empty() && media.duration_secs.is_none() {
        return None;
    }
    let codecs = |list: Vec<&str>| list.join(",");
    Some(format!(
        "v:{}|a:{}|s:{}|c:{}|d:{}",
        codecs(media.video.iter().map(|t| t.codec.as_str()).collect()),
        codecs(media.audio.iter().map(|t| t.codec.as_str()).collect()),
        codecs(media.subtitles.iter().map(|t| t.codec.as_str()).collect()),
        media.chapters.len(),
        media.duration_secs.map_or(0, |d| d.round() as i64)
    ))
}

/// The first video track's keyframe key, as the integer `release_keys` stores: its 16 hex digits read back.
pub(crate) fn video_key(media: &crate::probe::media::MediaInfo) -> Option<i64> {
    let hex = media.video.first()?.keyframe_key.as_deref()?;
    (hex.len() == 16).then(|| u64::from_str_radix(hex, 16).ok()).flatten().map(|k| k as i64)
}

/// What the Lines key is taken over: each file's Lines sum in file name order, joined by commas. None unless the
/// rows cover every article of the release (`parts`) and every file has lines: a sum over part of a file, or a file
/// with no row, would key a different post. `files` is `(lines, articles)` a file.
pub(crate) fn lines_text(files: &[(Option<i64>, i64)], parts: Option<i64>) -> Option<String> {
    let timed: i64 = files.iter().map(|(_, articles)| articles).sum();
    if files.is_empty() || parts != Some(timed) {
        return None;
    }
    let lines: Vec<String> =
        files.iter().map(|(lines, _)| lines.filter(|l| *l > 0).map(|l| l.to_string())).collect::<Option<_>>()?;
    Some(lines.join(","))
}

/// The keys over what a release has: its probe and its `file_timing` rows.
pub fn keys_of(
    probe: Option<&ProbeInfo>,
    timing: &[(Option<i64>, i64)],
    parts: Option<i64>,
    salt: Option<i64>,
) -> Keys {
    let mut keys = Keys::default();
    if let Some(p) = probe {
        if let Some(salt) = salt {
            let (account, host) = account_and_host(p);
            keys.account = account.map(|a| as_key(salted8(salt, &a)));
            keys.host = host.map(|h| as_key(salted8(salt, &h)));
        }
        keys.layout = p.media.as_ref().and_then(layout_text).map(|t| as_key(digest8(t.as_bytes())));
        keys.video = p.media.as_ref().and_then(video_key);
    }
    keys.lines = lines_text(timing, parts).map(|t| as_key(digest8(t.as_bytes())));
    keys
}

// ---------------------------------------------------------------- reading

/// The install's salt for the account and host keys: made once, by whoever asks first, and kept in shard 0's
/// `meta`. `conn` has the shards attached. None when it can't be read or made, and then no salted key is made.
pub fn salt(conn: &Connection) -> Option<i64> {
    salt_in(conn, "s0.meta")
}

/// `salt` over `meta`, the shard 0 table as `conn` names it: `s0.meta` with the shards attached, `meta` on shard 0
/// opened alone.
fn salt_in(conn: &Connection, meta: &str) -> Option<i64> {
    let read = || {
        conn.query_row(&format!("select value from {meta} where key = 'key_salt'"), [], |r| r.get::<_, i64>(0))
            .optional()
            .ok()
            .flatten()
    };
    if let Some(found) = read() {
        return Some(found);
    }
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).ok()?;
    let fresh = i64::from_le_bytes(bytes);
    // two at once: `or ignore` keeps the first, and both read it back
    let _ = conn.execute(&format!("insert or ignore into {meta} (key, value) values ('key_salt', ?1)"), [fresh]);
    read()
}

/// The probe a release has, decoded. None when it has none or it doesn't decode.
fn probe_of(shard: &Connection, release_id: i64) -> rusqlite::Result<Option<ProbeInfo>> {
    let info: Option<String> = shard
        .prepare_cached("select info from release_probe where release_id = ?1")?
        .query_row([release_id], |r| r.get(0))
        .optional()?;
    Ok(info.and_then(|i| serde_json::from_str(&i).ok()))
}

/// A release's `file_timing` rows as `(lines, articles)`, in file name order.
fn timing_of(shard: &Connection, release_id: i64) -> rusqlite::Result<Vec<(Option<i64>, i64)>> {
    shard
        .prepare_cached(
            "select t.lines, t.articles from file_timing t join files f on f.id = t.file_id
             where f.release_id = ?1 order by f.filename limit ?2",
        )?
        .query_map(rusqlite::params![release_id, FILES_CAP], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect()
}

/// The keys a release has now, read from its own shard (`shard`, on its own or as `main`).
pub fn read_keys(shard: &Connection, release_id: i64, parts: Option<i64>, salt: Option<i64>) -> rusqlite::Result<Keys> {
    let probe = probe_of(shard, release_id)?;
    let timing = timing_of(shard, release_id)?;
    Ok(keys_of(probe.as_ref(), &timing, parts, salt))
}

/// What a release can be looked up by, read from its own shard: the keys it has now, and the PAR2 sets and the
/// OpenSubtitles hash.
pub fn read_lookup(
    shard: &Connection,
    release_id: i64,
    parts: Option<i64>,
    salt: Option<i64>,
) -> rusqlite::Result<Lookup> {
    let keys = read_keys(shard, release_id, parts, salt)?;
    let par2_sets = par2_sets(shard, release_id)?;
    let moviehash = shard
        .prepare_cached("select moviehash from release_probe where release_id = ?1")
        .ok()
        .and_then(|mut q| q.query_row([release_id], |r| r.get::<_, Option<String>>(0)).optional().ok().flatten())
        .flatten();
    Ok(Lookup { keys, par2_sets, moviehash })
}

/// The PAR2 set ids a release's files are described by, at most `SETS_CAP`; none on a shard without the table yet.
pub fn par2_sets(shard: &Connection, release_id: i64) -> rusqlite::Result<Vec<Vec<u8>>> {
    let Ok(mut q) = shard.prepare_cached(
        "select distinct set_id from par2_files where release_id = ?1 and bound = 1 order by set_id limit ?2",
    ) else {
        return Ok(Vec::new());
    };
    q.query_map(rusqlite::params![release_id, SETS_CAP as i64], |r| r.get(0))?.collect()
}

/// The `release_keys` row a release has. None: no row.
fn stored(shard: &Connection, release_id: i64) -> rusqlite::Result<Option<Keys>> {
    shard
        .prepare_cached("select account, host, layout, lines, video from release_keys where release_id = ?1")?
        .query_row([release_id], |r| {
            Ok(Keys { account: r.get(0)?, host: r.get(1)?, layout: r.get(2)?, lines: r.get(3)?, video: r.get(4)? })
        })
        .optional()
}

/// The query over one shard (`s{shard}.`) for the releases other than `?1` sharing `kind`, newest first, with the
/// values bound from `?3` on, at most `?2`; and how many values it takes. Only releases the pages would show count:
/// one deleted or in a blocked group (`atlas_blocked`) is left out before the limit, soo the count and the list agree.
fn others_sql(kind: Kind, shard: usize, sets: usize) -> String {
    let (from, matches) = match kind.column() {
        Some(col) => (format!("s{shard}.release_keys k"), format!("k.{col} = ?3")),
        None if kind == Kind::Par2 => {
            let marks: Vec<String> = (0..sets).map(|i| format!("?{}", i + 3)).collect();
            (format!("s{shard}.par2_files k"), format!("k.set_id in ({}) and k.bound = 1", marks.join(", ")))
        }
        None => (format!("s{shard}.release_probe k"), "k.moviehash = ?3".to_string()),
    };
    format!(
        "select distinct k.release_id from {from} join s{shard}.releases r on r.id = k.release_id
         where {matches} and k.release_id != ?1 and not atlas_blocked(r.group_name)
         order by k.release_id desc limit ?2"
    )
}

fn values(lookup: &Lookup, kind: Kind) -> Vec<rusqlite::types::Value> {
    use rusqlite::types::Value;
    match kind {
        Kind::Par2 => lookup.par2_sets.iter().map(|s| Value::Blob(s.clone())).collect(),
        Kind::MovieHash => lookup.moviehash.iter().map(|h| Value::Text(h.clone())).collect(),
        _ => lookup.keys.get(kind).map(Value::Integer).into_iter().collect(),
    }
}

/// The releases other than `own` that share `kind` with it, over every shard of `conn` (the main database with
/// the shards attached), newest first, at most `limit`. A shard without the table yet has none.
pub fn others(conn: &Connection, own: i64, lookup: &Lookup, kind: Kind, limit: i64) -> Vec<i64> {
    let bound = values(lookup, kind);
    if bound.is_empty() || limit <= 0 {
        return Vec::new();
    }
    let mut found = Vec::new();
    for shard in 0..store::SHARDS {
        let sql = others_sql(kind, shard, bound.len());
        let Ok(mut q) = conn.prepare(&sql) else { continue };
        let args = [rusqlite::types::Value::Integer(own), rusqlite::types::Value::Integer(limit)]
            .into_iter()
            .chain(bound.iter().cloned());
        let Ok(rows) = q.query_map(params_from_iter(args), |r| r.get::<_, i64>(0)) else { continue };
        found.extend(rows.filter_map(Result::ok));
    }
    // ids grow with time, within a shard and across them closely enough
    found.sort_unstable_by(|a, b| b.cmp(a));
    found.dedup();
    found.truncate(limit as usize);
    found
}

/// How many releases other than `own` share `kind` with it, up to `COUNT_CAP`.
pub fn count_others(conn: &Connection, own: i64, lookup: &Lookup, kind: Kind) -> i64 {
    let bound = values(lookup, kind);
    if bound.is_empty() {
        return 0;
    }
    let mut total = 0;
    for shard in 0..store::SHARDS {
        let sql = format!("select count(*) from ({})", others_sql(kind, shard, bound.len()));
        let Ok(mut q) = conn.prepare(&sql) else { continue };
        let args = [rusqlite::types::Value::Integer(own), rusqlite::types::Value::Integer(COUNT_CAP - total)]
            .into_iter()
            .chain(bound.iter().cloned());
        total += q.query_row(params_from_iter(args), |r| r.get::<_, i64>(0)).unwrap_or(0);
        if total >= COUNT_CAP {
            return COUNT_CAP;
        }
    }
    total
}

// ---------------------------------------------------------------- the step

/// Keeps `release_keys` up to date: after a probe (which asks for it again), and as a release grows, for the Lines
/// key. A release with no key has no row.
#[derive(Default)]
pub struct KeysStep {
    salt: OnceLock<i64>,
}

impl KeysStep {
    /// The salt, read once from shard 0, beside the shard `conn` is on: the one the pages read.
    fn salt(&self, conn: &Connection) -> Result<i64, StepError> {
        if let Some(s) = self.salt.get() {
            return Ok(*s);
        }
        let none = || StepError::Retry("the key salt could not be read or made".into());
        let zero = conn.path().and_then(shard_zero).ok_or_else(none)?;
        let shard = crate::db::open_shard(&zero).map_err(|e| StepError::Retry(e.to_string()))?;
        let found = salt_in(&shard, "meta").ok_or_else(none)?;
        Ok(*self.salt.get_or_init(|| found))
    }
}

/// Shard 0's file, from another shard's (`atlas.s3.db` gives `atlas.s0.db`): `store::shard_path`'s names.
fn shard_zero(path: &str) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(path);
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".db")?;
    let (base, shard) = stem.rsplit_once(".s")?;
    shard.parse::<usize>().ok()?;
    Some(path.with_file_name(format!("{base}.s0.db")))
}

impl Step for KeysStep {
    fn id(&self) -> StepId {
        KEYS_STEP
    }
    fn name(&self) -> &'static str {
        "keys"
    }
    fn rank(&self) -> u16 {
        600
    }
    fn needs(&self) -> Needs {
        Needs::Database
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: false, on_growth: true }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let retry = |e: rusqlite::Error| StepError::Retry(e.to_string());
        let id = ctx.release().id;
        let parts = Some(ctx.release().parts);
        let salt = self.salt(ctx.conn())?;
        let keys = read_keys(ctx.conn(), id, parts, Some(salt)).map_err(retry)?;
        let had = stored(ctx.conn(), id).map_err(retry)?;
        let writes = match (keys.is_empty(), had) {
            (true, None) => Vec::new(),
            (true, Some(_)) => vec![StepWrite::new(DELETE, vec![id.into()])],
            (false, Some(old)) if old == keys => Vec::new(),
            (false, _) => vec![StepWrite::new(
                UPSERT,
                vec![
                    id.into(),
                    keys.account.into(),
                    keys.host.into(),
                    keys.layout.into(),
                    keys.lines.into(),
                    keys.video.into(),
                ],
            )],
        };
        Ok(Outcome { matched: !keys.is_empty(), writes, ..Outcome::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_print_as_the_pages_always_printed_them() {
        let d = digest8(b"abc");
        // SHA-256("abc") starts ba7816bf8f01cfea
        assert_eq!(hex16(as_key(d)), "ba7816bf8f01cfea");
        let salted = as_key(salted8(7, "user@example"));
        assert_eq!(hex12(salted).len(), 12);
        assert_eq!(hex12(salted), hex16(salted)[..12]);
        assert_ne!(salted8(7, "user@example"), salted8(8, "user@example"), "another install, another key");
    }

    #[test]
    fn keys_come_from_the_probe_and_the_timing() {
        let mut probe = ProbeInfo::default();
        probe.headers.insert(
            "Injection-Info".into(),
            "news.example; posting-account=\"acc-1\"; posting-host=\"10.0.0.1\"; logging-data=\"7\"".into(),
        );
        let keys = keys_of(Some(&probe), &[], None, Some(5));
        assert_eq!(keys.account, Some(as_key(salted8(5, "acc-1"))));
        assert_eq!(keys.host, Some(as_key(salted8(5, "10.0.0.1"))));
        assert_eq!((keys.layout, keys.lines), (None, None));
        assert_eq!(keys_of(Some(&probe), &[], None, None).account, None, "no salt, no salted key");

        // NNTP-Posting-Host stands in when Injection-Info names no host
        let mut old = ProbeInfo::default();
        old.headers.insert("NNTP-Posting-Host".into(), " 192.0.2.4 ".into());
        assert_eq!(keys_of(Some(&old), &[], None, Some(5)).host, Some(as_key(salted8(5, "192.0.2.4"))));

        // the Lines key needs every article timed and every file with lines
        let files = [(Some(1000), 10), (Some(500), 5)];
        let lines = keys_of(None, &files, Some(15), None).lines;
        assert_eq!(lines, Some(as_key(digest8(b"1000,500"))));
        assert_eq!(keys_of(None, &files, Some(16), None).lines, None);
        assert_eq!(keys_of(None, &files, None, None).lines, None);
        assert_eq!(keys_of(None, &[(None, 10), (Some(500), 5)], Some(15), None).lines, None);
        assert!(keys_of(None, &[], Some(0), None).is_empty());
    }

    #[test]
    fn the_video_key_is_the_first_keyframes() {
        use crate::probe::media::{MediaInfo, VideoTrack};
        let probe = |key: Option<&str>| ProbeInfo {
            media: Some(MediaInfo {
                video: vec![VideoTrack { keyframe_key: key.map(str::to_string), ..Default::default() }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let keys = keys_of(Some(&probe(Some("ba7816bf8f01cfea"))), &[], None, None);
        assert_eq!(keys.video, Some(as_key(digest8(b"abc"))));
        assert_eq!(hex16(keys.video.unwrap()), "ba7816bf8f01cfea", "the page prints what the probe kept");
        assert_eq!(keys_of(Some(&probe(None)), &[], None, None).video, None);
        assert_eq!(keys_of(Some(&probe(Some("nothex"))), &[], None, None).video, None);
        assert!(
            others_sql(Kind::Video, 2, 1).contains("s2.release_keys k")
                && others_sql(Kind::Video, 2, 1).contains("k.video = ?3")
        );
    }

    #[test]
    fn shard_zero_sits_beside_any_shard() {
        let main = std::path::Path::new("/data/atlas.db");
        for n in 0..store::SHARDS {
            let shard = store::shard_path(main, n);
            assert_eq!(shard_zero(shard.to_str().unwrap()), Some(store::shard_path(main, 0)));
        }
        assert_eq!(shard_zero("/data/atlas.db"), None);
        assert_eq!(shard_zero(""), None);
    }

    #[test]
    fn kinds_round_trip_their_words() {
        for kind in Kind::ALL {
            assert_eq!(Kind::parse(kind.word()), Some(kind));
            assert!(!kind.about().is_empty() && !kind.heading().is_empty());
        }
        assert_eq!(Kind::parse("raw"), None);
    }

    #[test]
    fn par2_lookups_bind_each_set() {
        let sql = others_sql(Kind::Par2, 3, 2);
        assert!(sql.contains("s3.par2_files k") && sql.contains("set_id in (?3, ?4)"), "{sql}");
        assert!(sql.contains("join s3.releases r") && sql.contains("not atlas_blocked(r.group_name)"), "{sql}");
        assert!(others_sql(Kind::Lines, 0, 1).contains("s0.release_keys k join s0.releases r"));
        assert!(others_sql(Kind::Lines, 0, 1).contains("k.lines = ?3"));
        assert!(others_sql(Kind::MovieHash, 7, 1).contains("k.moviehash = ?3"));
    }

    #[test]
    fn only_bound_par2_files_of_shown_releases_match() {
        let conn = Connection::open_in_memory().unwrap();
        crate::filters::register_sql(&conn).unwrap();
        conn.execute("attach ':memory:' as s0", []).unwrap();
        conn.execute_batch(&crate::archive::store::PAR2_FILES.create.replace("par2_files (", "s0.par2_files ("))
            .unwrap();
        conn.execute_batch("create table s0.releases (id integer primary key, group_name text not null)").unwrap();
        let blocked = "alt.binaries.pictures.erotica.teen";
        assert!(crate::filters::group_blocked(blocked));
        let set = vec![7u8; 16];
        // 1 asks; 2 shares it; 3 only holds an unbound copy; 4 is in a blocked group; 5 has no release row
        for (release, bound, group) in
            [(1, 1, Some("a.b.c")), (2, 1, Some("a.b.c")), (3, 0, Some("a.b.c")), (4, 1, Some(blocked)), (5, 1, None)]
        {
            if let Some(group) = group {
                conn.execute(
                    "insert into s0.releases (id, group_name) values (?1, ?2)",
                    rusqlite::params![release, group],
                )
                .unwrap();
            }
            conn.execute(
                "insert into s0.par2_files (release_id, file_id, set_id, md5, md5_16k, size, name, bound)
                 values (?1, ?2, ?3, x'', x'', 1, 'a.mkv', ?4)",
                rusqlite::params![release, vec![release as u8], set, bound],
            )
            .unwrap();
        }
        let lookup = Lookup { keys: Keys::default(), par2_sets: vec![set], moviehash: None };
        assert_eq!(others(&conn, 1, &lookup, Kind::Par2, 1), vec![2], "the limit counts only the shown ones");
        assert_eq!(count_others(&conn, 1, &lookup, Kind::Par2), 1);
    }
}
