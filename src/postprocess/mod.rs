//! The stage every release goes through after it is saved: it waits for the
//! release to settle, rejects what no step can help, and runs ordered steps
//! with retries. Runs only inside the indexer; the commands in `purge` and
//! `reprocess` work on the same tables. Rules ported from newznab-tmux, see `rules.rs`.

pub(crate) mod apply;
pub mod bodies;
pub(crate) mod engine;
pub mod rules;
pub mod schema;
pub mod stats;
pub mod step;
pub mod steps;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::{Connection, params};

use crate::config::Config;
use crate::indexer::Db;
use crate::nntp::Pool;
use crate::store;

use step::{Step, StepMask};

pub struct StageParams {
    pub main: PathBuf,
    pub db: Db,
    pub pool: Arc<Pool>,
    /// the full config: steps read theirs from it in `enabled`
    pub config: Config,
    pub steps: Vec<Arc<dyn Step>>,
    pub handle: tokio::runtime::Handle,
}

pub struct Stage {
    inner: Arc<engine::Inner>,
    db: Mutex<Option<Db>>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

static TEST_STEPS: Mutex<Option<Vec<Arc<dyn Step>>>> = Mutex::new(None);

/// For tests going through `run_until`: these steps instead of `steps::all()`.
#[doc(hidden)]
pub fn set_test_steps(steps: Vec<Arc<dyn Step>>) {
    *TEST_STEPS.lock().unwrap() = Some(steps);
}

/// The steps an indexer runs.
pub(crate) fn steps_to_run() -> Vec<Arc<dyn Step>> {
    TEST_STEPS.lock().unwrap().clone().unwrap_or_else(steps::all)
}

/// `Stage::rerun`, on the writer: clear these bits and queue the release.
pub(crate) fn rerun_on(conn: &Connection, release_id: i64, clear: StepMask, now: i64) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "insert into pp_state (release_id, rerun_gen, rerun_mask)
         select ?1, 1, ?2 where exists (select 1 from releases where id = ?1)
         on conflict(release_id) do update set
            done = done & ~?2, failed = failed & ~?2, db_attempts = 0, net_attempts = 0, net_next_at = 0,
            rerun_gen = rerun_gen + 1, rerun_mask = rerun_mask | ?2",
    )?
    .execute(params![release_id, clear])?;
    conn.prepare_cached(
        "insert into pp_queue (release_id, due_at) select ?1, ?2 where exists (select 1 from releases where id = ?1)
         on conflict(release_id) do update set due_at = excluded.due_at",
    )?
    .execute(params![release_id, now])?;
    Ok(())
}

impl Stage {
    /// Calls `enabled` on every step, seeds the first walk and `pp_steps_seen`, starts the threads.
    pub fn start(p: StageParams) -> Arc<Stage> {
        let inner = Arc::new(engine::Inner::new(&p));
        inner.seed(&p.db);
        let mut threads = Vec::new();
        for i in 0..inner.db_workers() {
            let (inner, db) = (inner.clone(), p.db.clone());
            threads.push(
                std::thread::Builder::new()
                    .name(format!("atlas-pp-db-{i}"))
                    .spawn(move || inner.db_lane(db))
                    .expect("couldnt start a processing thread"),
            );
        }
        if inner.has_net_steps() {
            for i in 0..inner.net_workers() {
                let (inner, db) = (inner.clone(), p.db.clone());
                threads.push(
                    std::thread::Builder::new()
                        .name(format!("atlas-pp-net-{i}"))
                        .spawn(move || inner.net_lane(db))
                        .expect("couldnt start a processing thread"),
                );
            }
        }
        Arc::new(Stage { inner, db: Mutex::new(Some(p.db)), threads: Mutex::new(threads) })
    }

    pub fn set_config(&self, full: &Config) {
        self.inner.set_config(full);
        if let Some(db) = &*self.db.lock().unwrap() {
            self.inner.seed(db);
        }
    }

    /// Clear these step bits and queue the release. Safe from any thread.
    pub fn rerun(&self, release_id: i64, clear: StepMask) -> anyhow::Result<()> {
        let db = self.db.lock().unwrap().clone().ok_or_else(|| anyhow::anyhow!("the stage has stopped"))?;
        let now = chrono::Utc::now().timestamp();
        let answer =
            db.on_writer(store::shard_of_id(release_id), Box::new(move |conn| rerun_on(conn, release_id, clear, now)))?;
        answer.recv()?.map_err(anyhow::Error::msg)
    }

    pub fn snapshot(&self) -> serde_json::Value {
        self.inner.snapshot()
    }

    /// Stops every stage thread, and lets go of the writers.
    pub fn stop_and_join(&self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.lock().unwrap().drain(..) {
            let _ = thread.join();
        }
        self.db.lock().unwrap().take();
    }

    /// Bounded counts for the dashboard.
    pub fn refresh_counts(&self) {
        self.inner.refresh_counts();
    }

    #[doc(hidden)]
    pub fn stopped(&self) -> Arc<AtomicBool> {
        self.inner.stop.clone()
    }
}

/// Runs until `stopping` says so, then stops every stage thread.
pub async fn run(stage: Arc<Stage>, stopping: impl Fn() -> bool + Send + 'static) {
    let mut last = std::time::Instant::now() - Duration::from_secs(60);
    while !stopping() {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if last.elapsed() >= Duration::from_secs(30) {
            last = std::time::Instant::now();
            let stage = stage.clone();
            let _ = tokio::task::spawn_blocking(move || stage.refresh_counts()).await;
        }
    }
    let stage = stage.clone();
    let _ = tokio::task::spawn_blocking(move || stage.stop_and_join()).await;
}
