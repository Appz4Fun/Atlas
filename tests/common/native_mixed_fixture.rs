//! Test-only adapter to the original native runtime; no alternate database API.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CHILD_TEST: &str = "postprocess::native::workers::mixed_contract::fixture_child";
const COMPLETED: &str = "mixed-contract-completed";

/// Only this parent session creates/drops the random test namespace. Bound
/// setup and cleanup as well as the child; never format a PostgreSQL error.
struct ParentSchema {
    client: postgres::Client,
    name: String,
}

impl ParentSchema {
    fn open(url: &str) -> Self {
        let mut connection: postgres::Config =
            url.parse().unwrap_or_else(|_| panic!("parse explicit native fixture URL"));
        connection.connect_timeout(Duration::from_secs(5));
        connection.tcp_user_timeout(Duration::from_secs(5));
        connection.options("-c statement_timeout=10000 -c lock_timeout=1000");
        let client =
            connection.connect(postgres::NoTls).unwrap_or_else(|_| panic!("connect explicit native fixture server"));
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap_or_else(|_| panic!("native fixture namespace entropy"));
        let name = format!("t_mixed_{}_{:016x}", std::process::id(), u64::from_le_bytes(nonce));
        let mut schema = Self { client, name };
        crate::pg::migrate::run(&mut schema.client, &schema.name)
            .unwrap_or_else(|_| panic!("migrate isolated native fixture namespace"));
        schema
    }
}

impl Drop for ParentSchema {
    fn drop(&mut self) {
        if self.client.batch_execute(&format!("drop schema if exists \"{}\" cascade", self.name)).is_err() {
            // This generated identifier is nonsecret; no connection error is
            // interpolated into output, even during another assertion unwind.
            eprintln!("native mixed fixture cleanup failed for {}", self.name);
        }
    }
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        // Keep the parent-owned schema alive until the child has actually exited,
        // including assertion and observer-error unwinds.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn mark_finished(case: &str) {
    let home = PathBuf::from(std::env::var_os("ATLAS_HOME").expect("isolated fixture home"));
    std::fs::write(home.join(COMPLETED), case).expect("record completed native mixed oracles");
}

fn required_url() -> String {
    std::env::var("ATLAS_PG_TEST_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .expect("selected native mixed test requires explicit ATLAS_PG_TEST_URL")
}

/// Run one fixture in a clean process before any live NXV global is initialized.
/// Child output stays private even on failure: PostgreSQL errors may contain
/// connection information, so no captured child output enters assertion debug.
pub fn run_child(case: &str) {
    let url = required_url();
    // The parent owns schema cleanup even when a timed-out child must be killed.
    let schema = ParentSchema::open(&url);
    let home = tempfile::tempdir().expect("private native fixture home");
    let log_path = home.path().join("mixed-child.log");
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let log = options.open(log_path).expect("private native fixture log");
    let mut child = ChildGuard(
        Command::new(std::env::current_exe().expect("native test executable"))
            .args(["--exact", CHILD_TEST, "--ignored", "--test-threads=1", "--nocapture"])
            .env_clear()
            .env("ATLAS_HOME", home.path())
            .env("ATLAS_NO_KEYRING", "1")
            .env("ATLAS_PG_TEST_URL", url)
            .env("ATLAS_NATIVE_MIXED_CASE", case)
            .env("ATLAS_NATIVE_MIXED_SCHEMA", &schema.name)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().expect("private log clone")))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("start isolated native mixed child"),
    );
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if let Some(status) = child.0.try_wait().expect("observe native mixed child") {
            if !status.success() || std::fs::read(home.path().join(COMPLETED)).ok().as_deref() != Some(case.as_bytes())
            {
                let retained = home.keep();
                panic!("native mixed child failed; private fixture retained at {}", retained.display());
            }
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.0.kill();
            let _ = child.0.wait();
            let retained = home.keep();
            panic!("native mixed child timed out; private fixture retained at {}", retained.display());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

use crate::config::{Config, HeaderStoreConfig};
use crate::pg::{
    Backend, StorageConfig,
    indexer_runtime::Runtime,
    stage_queue::{Queue, Settlement},
};
use crate::postprocess::{
    native::{Runner, Steps},
    step::{Needs, Step, mask},
};
use std::sync::{Arc, atomic::Ordering};

pub struct Fixture {
    pub observer: crate::pg::Pool,
    pub runtime: Option<Runtime>,
    pub config: Config,
    storage: StorageConfig,
    headers: HeaderStoreConfig,
    identity: Option<crate::pg::indexer_runtime::DestinationIdentity>,
    handle: tokio::runtime::Runtime,
    owner_pid: i32,
}
impl Fixture {
    pub fn open(settle: i64) -> Self {
        let url = required_url();
        let schema = std::env::var("ATLAS_NATIVE_MIXED_SCHEMA").expect("parent-owned native fixture schema");
        let home = PathBuf::from(std::env::var_os("ATLAS_HOME").expect("private native fixture home"));
        let storage =
            StorageConfig { backend: Backend::Postgres, url, schema, max_connections: 2, allow_plaintext: true };
        // The real cache actor rechecks selected storage on every command.
        // Give it this fixture's exact destination, never an ambient config.
        let selected = serde_json::json!({"storage": {"backend":"postgres", "url":&storage.url,
            "schema":&storage.schema, "max_connections":storage.max_connections, "allow_plaintext":true}});
        let text = toml_edit::ser::to_string_pretty(&selected)
            .unwrap_or_else(|_| panic!("serialize private selected storage"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(home.join("config.toml"))
            .expect("private selected fixture configuration")
            .write_all(text.as_bytes())
            .expect("write private selected fixture configuration");
        let observer = crate::pg::Pool::new(&storage).unwrap_or_else(|_| panic!("native fixture observer"));
        let mut config = Config::default();
        config.postprocess.enabled = false;
        config.predb.enabled = false;
        config.spots.enabled = false;
        config.inferred_parts = false;
        config.media.tvmaze.enabled = false;
        config.media.allocine.enabled = false;
        config.media.details_per_minute = 0;
        config.archive.enabled = false;
        config.metrics.enabled = false;
        config.postprocess.settle_secs = settle;
        config.postprocess.complete_settle_secs = settle;
        config.postprocess.workers = 1;
        config.postprocess.backlog_per_second = 100;
        config.postprocess.body_fetches_per_minute = 0;
        let headers = HeaderStoreConfig { enabled: true, dir: home.join("headers").to_string_lossy().into_owned() };
        let mut identity = None;
        let runtime = Runtime::open(&config, storage.clone(), headers.clone(), &[], &mut identity)
            .unwrap_or_else(|_| panic!("open original native fixture Runtime"));
        crate::nxv_store::live::spawn_bounded(&runtime.header_config().expect("original native header configuration"))
            .unwrap_or_else(|_| panic!("open original live NXV"));
        let owner_pid = runtime
            .owner
            .with(|client| Ok(client.query_one("select pg_backend_pid()", &[])?.get(0)))
            .expect("actual native owner PID");
        let handle =
            tokio::runtime::Builder::new_current_thread().enable_all().build().expect("native fixture IO handle");
        Self { observer, runtime: Some(runtime), config, storage, headers, identity, handle, owner_pid }
    }
    pub fn runtime(&self) -> &Runtime {
        self.runtime.as_ref().expect("original native fixture runtime")
    }
    pub fn queue(&self) -> Queue {
        Queue::for_owner(&self.runtime().owner).expect("original native fixture queue")
    }
    pub fn runner(&self, steps: Vec<Arc<dyn Step>>) -> Arc<Runner> {
        let mut config = self.config.clone();
        config.postprocess.enabled = true;
        config.naming.enabled = true;
        let mut runner = Runner::new(
            &self.runtime().owner,
            &config,
            Arc::new(crate::nntp::Pool::new(&[])),
            self.handle.handle().clone(),
        )
        .unwrap_or_else(|_| panic!("construct original native StepRunner"));
        let mut steps = steps;
        steps.sort_by_key(|step| step.rank());
        let mut enabled = 0;
        let mut database = 0;
        let mut network = 0;
        let mut on_growth = 0;
        let mut stamps = Vec::new();
        for step in &steps {
            let bit = mask(step.id());
            enabled |= bit;
            if step.needs() == Needs::Database {
                database |= bit;
            } else {
                network |= bit;
            }
            if step.reruns().on_growth {
                on_growth |= bit;
            }
            stamps.push((step.id().0, bit, step.config_stamp() as i64));
        }
        runner.steps = Steps { ordered: steps, enabled, database, network, on_growth, stamps };
        Arc::new(runner)
    }
    pub fn save(&self, name: &str, group: &str, display: Option<&str>) -> i64 {
        let release = crate::parser::Release {
            name: name.into(),
            group: group.into(),
            display_name: display.map(String::from),
            is_obfuscated: crate::parser::is_obfuscated(name),
            complete: true,
            articles: (1..=2)
                .map(|part| crate::parser::Article {
                    message_id: format!("<{name}.{part}@fixture.invalid>"),
                    filename: Some("x.bin".into()),
                    subject: format!("\"x.bin\" yEnc ({part}/2)"),
                    part: Some(part),
                    total_parts: Some(2),
                    bytes: 17,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        self.runtime()
            .owner
            .with(|client| {
                crate::pg::release_writer::save(client, &[release], now())?;
                Ok(client
                    .query_one(
                        "select id from releases where original_name=$1 and group_name=$2",
                        &[&name.as_bytes(), &group],
                    )?
                    .get(0))
            })
            .unwrap_or_else(|_| panic!("save through original native writer"))
    }
    pub fn bits(&self, id: i64) -> (i64, i64, i64) {
        let row = self
            .observer
            .get()
            .expect("native fixture observer")
            .query_one("select done,failed,db_attempts from pp_state where release_id=$1", &[&id])
            .expect("actual native step state");
        (row.get(0), row.get(1), row.get(2))
    }
    pub fn rerun(&self, id: i64, clear: i64) {
        self.runtime()
            .owner
            .with(|client| {
                let mut tx = client.transaction()?;
                anyhow::ensure!(
                    crate::pg::stage_queue::rerun_in(&mut tx, id, clear, now())?,
                    "native fixture release vanished"
                );
                tx.commit()?;
                Ok(())
            })
            .unwrap_or_else(|_| panic!("actual owned native rerun"));
    }
    pub fn walk(&self, group: &str, redo: i64) {
        let queue = self.queue();
        queue.request_walk(redo, Some(group), now()).expect("actual native walk request");
        assert_eq!(
            queue.walk_tick(now()).expect("actual native walk nomination"),
            crate::pg::stage_queue::WalkProgress::Queued
        );
    }
    pub fn visit(&self, runner: &Runner) -> bool {
        let queue = self.queue();
        let Some(claim) = queue.claim(now(), 1, 60).expect("actual native claim").pop() else { return false };
        let prepared = runner.database(&claim, claim.redo_mask(), now()).expect("actual native StepRunner visit");
        assert_eq!(prepared.settle(&queue, &claim, now()).expect("actual native atomic result"), Settlement::Applied);
        true
    }
    pub fn run_until(&self, runner: &Runner, mut done: impl FnMut() -> bool) {
        wait("actual native StepRunner completion", || {
            if done() {
                return true;
            }
            self.visit(runner);
            done()
        });
    }
    pub fn close_owner(&mut self) {
        let Some(runtime) = self.runtime.take() else { return };
        let pid = self.owner_pid;
        runtime.owner.close();
        drop(runtime);
        wait("original native owner session drain", || {
            let mut c = self.observer.get().expect("native fixture drain observer");
            c.query_one("select pg_stat_clear_snapshot()", &[]).ok();
            !c.query_one("select exists(select 1 from pg_stat_activity where pid=$1)", &[&pid])
                .expect("actual original owner drain")
                .get::<_, bool>(0)
        });
    }
    pub fn reopen(&mut self) {
        self.close_owner();
        let runtime = Runtime::open(&self.config, self.storage.clone(), self.headers.clone(), &[], &mut self.identity)
            .unwrap_or_else(|_| panic!("reopen original native Runtime after drain"));
        self.owner_pid = runtime
            .owner
            .with(|client| Ok(client.query_one("select pg_backend_pid()", &[])?.get(0)))
            .expect("replacement original owner PID");
        self.runtime = Some(runtime);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.owner.stop.store(true, Ordering::Release);
            runtime.owner.close();
        }
        crate::nxv_store::live::finish();
    }
}
pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
pub fn wait(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(12);
    while !ready() {
        assert!(Instant::now() < deadline, "never reached {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
