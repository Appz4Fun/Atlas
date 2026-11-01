//! A catalogue of release titles, kept in its own file, and what uses it. Off until `predb.enabled`.

pub mod db;
pub mod feeds;
pub mod file_import;
pub mod import;
pub mod keys;
pub mod late;
pub mod matcher;
pub mod more_feeds;
pub mod poll;
pub mod step;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{Value, json};

use crate::config::{Config, PredbConfig};
use crate::postprocess::Stage;
use crate::postprocess::step::mask;
use crate::store;

use self::late::LateCounts;
use self::poll::{Fetch, UreqFetch, poll_source};
use self::step::PREDB_MATCH;

const ERROR_NAP: Duration = Duration::from_secs(60);

pub struct ServiceParams {
    pub main: PathBuf,
    pub config: PredbConfig,
    pub stage: Option<Arc<Stage>>,
    pub fetch: Option<Arc<dyn Fetch>>,
}

static TEST_FETCH: Mutex<Option<Arc<dyn Fetch>>> = Mutex::new(None);

/// For a test that runs the real indexer: `run_servers` builds the service, soo this is the only way in.
/// Install it before `run_until`.
#[doc(hidden)]
pub fn set_test_fetch(fetch: Arc<dyn Fetch>) {
    *TEST_FETCH.lock().unwrap() = Some(fetch);
}

pub(crate) fn test_fetch() -> Option<Arc<dyn Fetch>> {
    TEST_FETCH.lock().unwrap().clone()
}

struct Shared {
    path: PathBuf,
    main: PathBuf,
    config: RwLock<PredbConfig>,
    stop: AtomicBool,
    late: LateCounts,
}

impl Shared {
    fn stopping(&self) -> bool {
        self.stop.load(Relaxed)
    }

    /// False: it's stopping.
    fn nap(&self, how_long: Duration) -> bool {
        let mut left = how_long;
        while !left.is_zero() {
            if self.stopping() {
                return false;
            }
            let slice = left.min(Duration::from_millis(100));
            std::thread::sleep(slice);
            left -= slice;
        }
        !self.stopping()
    }
}

pub struct Service {
    shared: Arc<Shared>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

fn feed_thread(shared: Arc<Shared>, fetch: Option<Arc<dyn Fetch>>) {
    let mut conn = match db::create(&shared.path) {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("predb: {e}");
            return;
        }
    };
    let mut unknown: HashSet<String> = HashSet::new();
    while shared.nap(Duration::from_secs(1)) {
        let config = shared.config.read().unwrap().clone();
        let real;
        let fetch: &dyn Fetch = match &fetch {
            Some(fetch) => fetch.as_ref(),
            None => {
                real = UreqFetch::new(Duration::from_secs(config.timeout_secs));
                &real
            }
        };
        for key in &config.sources {
            if shared.stopping() {
                return;
            }
            let Some(spec) = feeds::spec(key) else {
                if unknown.insert(key.clone()) {
                    println!("predb: no source named {key}, ignoring it");
                }
                continue;
            };
            let now = chrono::Utc::now().timestamp();
            let next: rusqlite::Result<Option<i64>> = conn
                .query_row("select next_poll_at from feeds where source = ?1", [spec.id], |r| r.get(0))
                .map(Some)
                .or_else(|e| if e == rusqlite::Error::QueryReturnedNoRows { Ok(None) } else { Err(e) });
            let due = match next {
                Ok(next) => next.unwrap_or(0) <= now,
                Err(e) => {
                    eprintln!("predb: {e}");
                    shared.nap(ERROR_NAP);
                    continue;
                }
            };
            if !due {
                continue;
            }
            let pause = |d: Duration| shared.nap(d);
            if let Err(e) = poll_source(&mut conn, fetch, spec, &config, now, &pause) {
                eprintln!("predb: {e}");
                shared.nap(ERROR_NAP);
            }
        }
    }
}

fn late_thread(shared: Arc<Shared>, stage: Arc<Stage>) {
    let open = || -> rusqlite::Result<_> {
        let predb = db::create(&shared.path)?;
        let shards = store::shard_paths(&shared.main)
            .iter()
            .map(|p| {
                let conn = crate::db::open_shard(p)?;
                conn.execute_batch("pragma query_only = on")?;
                Ok(conn)
            })
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((predb, shards))
    };
    let (predb, shards) = match open() {
        Ok(opened) => opened,
        Err(e) => {
            eprintln!("predb: {e}");
            return;
        }
    };
    let rerun = |id: i64| stage.rerun(id, mask(PREDB_MATCH));
    while shared.nap(Duration::from_secs(1)) {
        let budget = shared.config.read().unwrap().late_per_second;
        if budget <= 0 {
            continue;
        }
        let now = chrono::Utc::now().timestamp();
        if let Err(e) = late::tick_counting(&predb, &shards, now, budget, &shared.stop, &rerun, &shared.late) {
            if !shared.stopping() {
                eprintln!("predb: {e:#}");
            }
            shared.nap(ERROR_NAP);
        }
    }
    // the connections go with this thread: a compaction's checkpoint never meets one
}

impl Service {
    /// Makes the file. Threads `atlas-predb-feed`, and `atlas-predb-late` when there is a stage.
    pub fn start(p: ServiceParams) -> anyhow::Result<Arc<Service>> {
        let path = db::path_for(&p.main);
        drop(db::create(&path)?);
        let shared = Arc::new(Shared {
            path,
            main: p.main,
            config: RwLock::new(p.config),
            stop: AtomicBool::new(false),
            late: LateCounts::default(),
        });
        let mut threads = Vec::new();
        let (feed, fetch) = (shared.clone(), p.fetch);
        threads
            .push(std::thread::Builder::new().name("atlas-predb-feed".into()).spawn(move || feed_thread(feed, fetch))?);
        // whatever `late_per_second` says now: a reload can turn it on, and it waits while that's 0
        if let Some(stage) = p.stage {
            let late = shared.clone();
            threads.push(
                std::thread::Builder::new().name("atlas-predb-late".into()).spawn(move || late_thread(late, stage))?,
            );
        }
        Ok(Arc::new(Service { shared, threads: Mutex::new(threads) }))
    }

    pub fn set_config(&self, full: &Config) {
        *self.shared.config.write().unwrap() = full.predb.clone();
    }

    /// What `stats.json` says of it. Reads the file: nothing is kept open between reports.
    pub fn snapshot(&self) -> Value {
        let file_bytes = std::fs::metadata(&self.shared.path).map(|m| m.len()).unwrap_or(0);
        let late = &self.shared.late;
        let mut out = json!({
            "file_bytes": file_bytes,
            "late": {
                "titles": late.titles.load(Relaxed),
                "reruns": late.reruns.load(Relaxed),
                "overflow": late.overflow.load(Relaxed),
                "out_of_budget": late.out_of_budget.load(Relaxed),
                "errors": late.errors.load(Relaxed),
            },
        });
        let read = || -> rusqlite::Result<(i64, i64, Vec<Value>)> {
            let conn = db::open_reader(&self.shared.path)?;
            let meta = |key: &str| -> rusqlite::Result<i64> {
                conn.query_row("select value from meta where key = ?1", [key], |r| r.get(0))
            };
            let top: i64 = conn.query_row("select coalesce(max(id), 0) from titles", [], |r| r.get(0))?;
            let behind = (top - meta("late_after_id")?).max(0);
            let mut stmt = conn.prepare(
                "select key, last_ok_at, last_error, next_poll_at, failures, inserted, updated from feeds order by source",
            )?;
            let sources = stmt
                .query_map([], |r| {
                    Ok(json!({
                        "key": r.get::<_, String>(0)?,
                        "last_ok_at": r.get::<_, Option<i64>>(1)?,
                        "last_error": r.get::<_, Option<String>>(2)?,
                        "next_poll_at": r.get::<_, i64>(3)?,
                        "failures": r.get::<_, i64>(4)?,
                        "inserted": r.get::<_, i64>(5)?,
                        "updated": r.get::<_, i64>(6)?,
                    }))
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok((meta("titles")?, behind, sources))
        };
        if let Ok((titles, behind, sources)) = read() {
            out["titles"] = json!(titles);
            out["late_behind"] = json!(behind);
            out["sources"] = json!(sources);
        }
        out
    }

    /// Sets the stop flag and joins both threads. The feed thread may finish one HTTP request first.
    pub fn stop_and_join(&self) {
        self.shared.stop.store(true, Relaxed);
        let threads = std::mem::take(&mut *self.threads.lock().unwrap());
        for thread in threads {
            let _ = thread.join();
        }
    }
}

/// Naps 250 ms until `stopping`, then `stop_and_join` through `spawn_blocking`.
pub async fn run(service: Arc<Service>, stopping: impl Fn() -> bool + Send + 'static) {
    while !stopping() {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let _ = tokio::task::spawn_blocking(move || service.stop_and_join()).await;
}
