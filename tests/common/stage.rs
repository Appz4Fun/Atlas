//! A processing stage on its own, with steps a test writes: no indexer.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use atlas::config::{Config, PostprocessConfig, UsenetServer};
use atlas::indexer::shared_db;
use atlas::nntp::Pool;
use atlas::parser::{Article, Release};
use atlas::postprocess::step::{Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId};
use atlas::postprocess::{Stage, StageParams};
use atlas::{db, store};
use rusqlite::Connection;

pub const STAGE_GROUP: &str = "alt.binaries.t";

type RunFn = dyn Fn(&mut StepCtx<'_>) -> Result<Outcome, StepError> + Send + Sync;
type AppliesFn = dyn Fn(&ReleaseInfo) -> bool + Send + Sync;

/// A step made of closures, recording what it saw: (its name, the release's name, its display name).
pub struct TestStep {
    pub id: u8,
    pub name: &'static str,
    pub rank: u16,
    pub needs: Needs,
    pub reruns: Reruns,
    pub run: Box<RunFn>,
    pub applies: Box<AppliesFn>,
    pub stamp: Arc<Mutex<u64>>,
    pub on: Arc<Mutex<bool>>,
    pub log: Log,
}

pub type Log = Arc<Mutex<Vec<(&'static str, String, Option<String>)>>>;

impl TestStep {
    pub fn new(id: u8, name: &'static str, rank: u16, needs: Needs, log: &Log) -> TestStep {
        TestStep {
            id,
            name,
            rank,
            needs,
            reruns: Reruns::default(),
            run: Box::new(|_| Ok(Outcome::default())),
            applies: Box::new(|_| true),
            stamp: Arc::new(Mutex::new(0)),
            on: Arc::new(Mutex::new(true)),
            log: log.clone(),
        }
    }

    pub fn runs(
        mut self,
        run: impl Fn(&mut StepCtx<'_>) -> Result<Outcome, StepError> + Send + Sync + 'static,
    ) -> Self {
        self.run = Box::new(run);
        self
    }

    pub fn applies(mut self, applies: impl Fn(&ReleaseInfo) -> bool + Send + Sync + 'static) -> Self {
        self.applies = Box::new(applies);
        self
    }

    pub fn reruns(mut self, on_rename: bool, on_growth: bool) -> Self {
        self.reruns = Reruns { on_rename, on_growth };
        self
    }

    pub fn arc(self) -> Arc<dyn Step> {
        Arc::new(self)
    }
}

impl Step for TestStep {
    fn id(&self) -> StepId {
        StepId(self.id)
    }
    fn name(&self) -> &'static str {
        self.name
    }
    fn rank(&self) -> u16 {
        self.rank
    }
    fn needs(&self) -> Needs {
        self.needs
    }
    fn enabled(&self, _cfg: &Config) -> bool {
        *self.on.lock().unwrap()
    }
    fn applies(&self, release: &ReleaseInfo) -> bool {
        (self.applies)(release)
    }
    fn reruns(&self) -> Reruns {
        self.reruns
    }
    fn config_stamp(&self) -> u64 {
        *self.stamp.lock().unwrap()
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let r = ctx.release();
        self.log.lock().unwrap().push((self.name, r.name.clone(), r.display_name.clone()));
        (self.run)(ctx)
    }
}

pub fn quick_config() -> PostprocessConfig {
    PostprocessConfig { settle_secs: 1, complete_settle_secs: 1, body_fetches_per_minute: 0, ..Default::default() }
}

/// A shard, a stage's worth of runtime, and whatever was saved first.
pub struct Harness {
    pub dir: tempfile::TempDir,
    pub main: PathBuf,
    pub runtime: tokio::runtime::Runtime,
    pub stage: Option<Arc<Stage>>,
}

pub fn fresh() -> Harness {
    // what a save queues releases by: a stage sets these, and tests save before starting one
    store::SETTLE_SECS.store(1, std::sync::atomic::Ordering::Relaxed);
    store::COMPLETE_SETTLE_SECS.store(1, std::sync::atomic::Ordering::Relaxed);
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    db::create_db_at(&main).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    Harness { dir, main, runtime, stage: None }
}

impl Harness {
    pub fn start(&mut self, steps: Vec<Arc<dyn Step>>, postprocess: PostprocessConfig) {
        self.start_on(steps, postprocess, Vec::new());
    }

    pub fn start_on(&mut self, steps: Vec<Arc<dyn Step>>, postprocess: PostprocessConfig, servers: Vec<UsenetServer>) {
        let pool = Arc::new(Pool::new(&servers));
        self.start_with_pool(steps, postprocess, pool);
    }

    pub fn start_with_pool(&mut self, steps: Vec<Arc<dyn Step>>, postprocess: PostprocessConfig, pool: Arc<Pool>) {
        let conn = db::open_at(&self.main).unwrap();
        self.stage = Some(Stage::start(StageParams {
            main: self.main.clone(),
            db: shared_db(conn),
            pool,
            config: Config { postprocess, ..Config::default() },
            steps,
            handle: self.runtime.handle().clone(),
        }));
    }

    pub fn stop(&mut self) {
        if let Some(stage) = self.stage.take() {
            stage.stop_and_join();
        }
    }

    pub fn save(&self, releases: &[Release]) {
        store::save(&self.main, releases).unwrap();
    }

    pub fn shard(&self) -> Connection {
        db::open_shard(&store::shard_path(&self.main, store::shard_of(STAGE_GROUP))).unwrap()
    }

    pub fn id_of(&self, name: &str) -> i64 {
        self.shard().query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap()
    }

    pub fn bits(&self, name: &str) -> Option<(i64, i64, Option<i64>)> {
        let id = self.id_of(name);
        self.shard()
            .query_row("select done, failed, net_wanted from pp_state where release_id = ?", [id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .ok()
    }

    pub fn field(&self, name: &str, column: &str) -> i64 {
        let id = self.id_of(name);
        self.shard()
            .query_row(&format!("select {column} from pp_state where release_id = ?"), [id], |r| r.get(0))
            .unwrap()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Until `done`, failing at 20 s.
pub fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// How often `step` ran, of what was logged.
pub fn runs_of(log: &Log, step: &str) -> usize {
    log.lock().unwrap().iter().filter(|(s, ..)| *s == step).count()
}

pub fn article(file: &str, part: i64, parts: i64, id: &str) -> Article {
    Article {
        message_id: id.into(),
        subject: format!("\"{file}\" yEnc ({part}/{parts})"),
        author: "poster <p@x>".into(),
        filename: Some(file.into()),
        part: Some(part),
        total_parts: Some(parts),
        bytes: 700_000,
        ..Default::default()
    }
}

/// `file` with `have` of its `parts`, in release `name`.
pub fn release_of(name: &str, file: &str, have: i64, parts: i64) -> Release {
    let articles: Vec<Article> =
        (1..=have).map(|p| article(file, p, parts, &format!("<{name}.{file}.{p}@x>"))).collect();
    let size = articles.iter().map(|a| a.bytes).sum();
    Release {
        name: name.into(),
        group: STAGE_GROUP.into(),
        articles,
        size,
        complete: have == parts,
        ..Default::default()
    }
}

/// a whole release
pub fn complete(name: &str) -> Release {
    release_of(name, &format!("{name}.bin"), 2, 2)
}
