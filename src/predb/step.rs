//! The step that gives a release its catalogue title.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use rusqlite::{Connection, OptionalExtension};

use crate::archive;
use crate::config::Config;
use crate::naming::{
    Grade, NameSource, best_from_paths, clean_candidate, current_name, is_image_name, judge, renames_pack_to_episode,
};
use crate::postprocess::schema::ShardTable;
use crate::postprocess::step::{
    NameChange, Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId, StepMask, StepWrite,
};

use super::db;
use super::keys::{hash_key, name_key};
use super::matcher::{Evidence, find};

pub const MATCH: ShardTable = ShardTable {
    name: "predb_match",
    create: "create table if not exists predb_match (
        release_id INTEGER PRIMARY KEY,
        title_id   INTEGER NOT NULL,
        kind       INTEGER NOT NULL,
        category   TEXT,
        pre_at     INTEGER,
        nuked      INTEGER NOT NULL DEFAULT 0,
        matched_at INTEGER NOT NULL
    )",
    indexes: &[],
    release_key: Some("release_id"),
    added: &[],
};

/// Names inner files suggest, for releases with no match yet: how a late title finds them.
pub const INNER_KEY: ShardTable = ShardTable {
    name: "predb_inner_key",
    create: "create table if not exists predb_inner_key (
        key INTEGER NOT NULL, release_id INTEGER NOT NULL, primary key (key, release_id)
    ) without rowid",
    indexes: &[(
        "predb_inner_key_release",
        "create index if not exists predb_inner_key_release on predb_inner_key(release_id)",
    )],
    release_key: Some("release_id"),
    added: &[],
};

const MATCH_UPSERT: &str =
    "insert or replace into predb_match (release_id, title_id, kind, category, pre_at, nuked, matched_at)
    values (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
const KEYS_CLEAR: &str = "delete from predb_inner_key where release_id = ?1";
const KEY_ADD: &str = "insert or ignore into predb_inner_key (key, release_id) values (?1, ?2)";

const KEYS_KEPT: usize = 4;

pub const PREDB_MATCH: StepId = StepId(16);
/// Parts 7 and 8: ids 24..=39. A match that keeps the same name is not a rename, and they still want to look again.
const LATER_PARTS: StepMask = 0xFFFF_i64 << 24;

/// What no title could equal, without reading anything. A title names a release directory, soo a name with an image
/// extension is out. With one article there is no other file to look at, soo a random name is out too, unless it is a hash.
pub fn worth_looking_up(r: &ReleaseInfo) -> bool {
    if r.name_source.is_some_and(|s| s >= NameSource::PreDb as i64) {
        return false;
    }
    let name = current_name(r);
    if is_image_name(name) {
        return false;
    }
    if r.parts > 1 {
        return true;
    }
    if clean_candidate(name).as_deref().and_then(hash_key).is_some() {
        return true;
    }
    judge(name) != Grade::Junk
}

/// `…/atlas.s3.db` -> `…/atlas.predb.db`. `StepCtx` has no main path, and `ctx.conn().path()` is the shard's.
pub fn db_path_from_shard(shard: &Path) -> Option<PathBuf> {
    let file = shard.file_name()?.to_str()?;
    let stem = file.strip_suffix(".db")?;
    let (base, shard_no) = stem.rsplit_once(".s")?;
    if shard_no.is_empty() || !shard_no.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(shard.with_file_name(format!("{base}.predb.db")))
}

thread_local! {
    static READERS: RefCell<HashMap<PathBuf, Connection>> = RefCell::new(HashMap::new());
}

/// One `query_only` connection per thread and file. A missing file is an error, even with a connection open on it.
fn with_reader<T>(path: &Path, read: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    READERS.with_borrow_mut(|readers| {
        if !path.exists() {
            readers.remove(path);
            return Err(rusqlite::Error::InvalidPath(path.to_path_buf()));
        }
        if !readers.contains_key(path) {
            readers.insert(path.to_path_buf(), db::open_reader(path)?);
        }
        match readers.get(path) {
            Some(conn) => read(conn),
            None => Err(rusqlite::Error::InvalidPath(path.to_path_buf())),
        }
    })
}

type Reads = BTreeMap<u64, i64>;
static IN_FLIGHT: LazyLock<Mutex<HashMap<PathBuf, Reads>>> = LazyLock::new(Default::default);
static NEXT_READ: AtomicU64 = AtomicU64::new(0);

/// A step read `titles` up to this id and its keys aren't committed: a newer title has to wait.
struct InFlight {
    path: PathBuf,
    token: u64,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let mut all = IN_FLIGHT.lock().unwrap();
        if let Some(reads) = all.get_mut(&self.path) {
            reads.remove(&self.token);
            if reads.is_empty() {
                all.remove(&self.path);
            }
        }
    }
}

fn record_read(path: &Path, top: i64) -> InFlight {
    let token = NEXT_READ.fetch_add(1, Ordering::Relaxed);
    IN_FLIGHT.lock().unwrap().entry(path.to_path_buf()).or_default().insert(token, top);
    InFlight { path: path.to_path_buf(), token }
}

/// what a run holds while its batch is uncommitted, for a test
#[doc(hidden)]
pub fn hold_for_test(path: &Path, predb: &Connection, top: i64) -> impl Sized + use<> {
    record_read(predb.path().map_or(path, Path::new), top)
}

/// the lowest id any uncommitted step has read up to
pub(crate) fn lowest_in_flight(path: &Path) -> Option<i64> {
    IN_FLIGHT.lock().unwrap().get(path).and_then(|reads| reads.values().min().copied())
}

pub struct PredbMatch;

impl Step for PredbMatch {
    fn id(&self) -> StepId {
        PREDB_MATCH
    }
    fn name(&self) -> &'static str {
        "predb_match"
    }
    fn rank(&self) -> u16 {
        300
    }
    fn needs(&self) -> Needs {
        Needs::Database
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.predb.enabled
    }
    fn applies(&self, r: &ReleaseInfo) -> bool {
        worth_looking_up(r)
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: true, on_growth: true }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let retry = |e: rusqlite::Error| StepError::Retry(e.to_string());
        let id = ctx.release().id;
        let files = ctx.files().map_err(retry)?;
        let inner = archive::store::inner_files(ctx.conn(), id).map_err(retry)?;
        let path = ctx
            .conn()
            .path()
            .and_then(|p| db_path_from_shard(Path::new(p)))
            .ok_or_else(|| StepError::Retry("this shard's PreDB file cant be worked out".into()))?;

        let r = ctx.release();
        let evidence = Evidence {
            name: &r.name,
            display_name: r.display_name.as_deref(),
            size: r.size,
            files: files.iter().map(|f| f.filename.as_str()).collect(),
            inner: inner.iter().map(|f| f.name.as_str()).collect(),
        };
        // before reading titles: a title landing after this waits for this run's keys
        let (hold, found) = with_reader(&path, |c| {
            let top: i64 = c.query_row("select coalesce(max(id), 0) from titles", [], |r| r.get(0))?;
            let hold = record_read(&path, top);
            Ok((hold, find(c, &evidence)?))
        })
        .map_err(retry)?;

        let hit = found.filter(|m| !renames_pack_to_episode(current_name(r), &m.title.title));
        let lookup = if hit.is_some() { "matched" } else { "no_match" };
        crate::metrics::names::PREDB_LOOKUPS.inc(&[("result", lookup)], 1);
        let Some(m) = hit else {
            let mut writes = Vec::new();
            if !inner.is_empty() {
                writes.push(StepWrite::new(KEYS_CLEAR, vec![id.into()]));
                let paths = inner.iter().map(|f| (f.name.as_str(), f.size.unwrap_or(1).max(1) as u64));
                for name in best_from_paths(paths, Grade::Weak).into_iter().take(KEYS_KEPT) {
                    writes.push(StepWrite::new(KEY_ADD, vec![name_key(&name).into(), id.into()]));
                }
            }
            return Ok(Outcome { writes, hold: Some(Box::new(hold)), ..Outcome::default() });
        };

        let t = m.title;
        let how = match m.kind {
            super::matcher::Kind::Exact => "exact",
            super::matcher::Kind::Normalized => "normalized",
            super::matcher::Kind::Hash => "hash",
            super::matcher::Kind::InnerFile => "inner_file",
        };
        let predb_category = t.category.as_deref().unwrap_or("none");
        crate::metrics::names::PREDB_MATCHES.inc(&[("kind", how), ("category", predb_category)], 1);
        let known: Option<i64> = ctx
            .conn()
            .prepare_cached("select title_id from predb_match where release_id = ?1")
            .and_then(|mut stmt| stmt.query_row([id], |r| r.get(0)).optional())
            .map_err(retry)?;
        let now = chrono::Utc::now().timestamp();
        let mut facts = vec![("predb.title", t.title.clone())];
        if let Some(category) = &t.category {
            facts.push(("predb.category", category.clone()));
        }
        let writes = vec![
            StepWrite::new(
                MATCH_UPSERT,
                vec![
                    id.into(),
                    t.id.into(),
                    (m.kind as i64).into(),
                    t.category.clone().into(),
                    t.pre_at.into(),
                    t.nuked.into(),
                    now.into(),
                ],
            ),
            StepWrite::new(KEYS_CLEAR, vec![id.into()]),
        ];
        Ok(Outcome {
            matched: true,
            writes,
            // a title outranks every other name, soo this goes around naming::improves
            name: Some(NameChange {
                display_name: Some(t.title.clone()),
                obfuscated: Some(false),
                source: NameSource::PreDb.value(),
            }),
            rerun: if known == Some(t.id) { 0 } else { LATER_PARTS },
            facts,
            wait_until: None,
            hold: Some(Box::new(hold)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_path_from_shard_table() {
        assert_eq!(db_path_from_shard(Path::new("/x/atlas.s3.db")), Some(PathBuf::from("/x/atlas.predb.db")));
        assert_eq!(db_path_from_shard(Path::new("/x/other.s0.db")), Some(PathBuf::from("/x/other.predb.db")));
        assert_eq!(db_path_from_shard(Path::new("/x/atlas.db")), None);
    }
}
