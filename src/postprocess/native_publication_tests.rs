//! Isolated PostgreSQL controls for the retained publication worker and its real service lifetime.
use super::super::telemetry::Producer;
use super::*;
use crate::pg::{self, indexer_runtime::Runtime, outage::Failure, stage_telemetry::LaneCounters};
use std::{
    io::{self, Read, Write},
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Weak, mpsc},
    thread::JoinHandle,
};

const PRIVATE_CASE: &str = "ATLAS_NATIVE_PUBLICATION_CASE";
const PRIVATE_SCHEMA: &str = "ATLAS_NATIVE_PUBLICATION_SCHEMA";
const LOG_BYTES: usize = 64 * 1024;
const CHILD_DEADLINE: Duration = Duration::from_secs(120);
const SELECTOR_PREFIX: &str = "postprocess::native::workers::publication_tests::";

fn quiet_config() -> crate::config::Config {
    let mut cfg = crate::config::Config::default();
    cfg.postprocess.enabled = false;
    cfg.predb.enabled = false;
    cfg.spots.enabled = false;
    cfg.inferred_parts = false;
    cfg.media.tvmaze.enabled = false;
    cfg.media.allocine.enabled = false;
    cfg.media.details_per_minute = 0;
    cfg
}
struct Fixture {
    runtime: Runtime,
    storage: pg::StorageConfig,
    rt: tokio::runtime::Runtime,
}
impl Fixture {
    fn new() -> Self {
        let storage = crate::config::checked_storage_config().expect("private configured PostgreSQL");
        assert_eq!(storage.schema, std::env::var(PRIVATE_SCHEMA).expect("parent-owned schema"));
        let headers = crate::config::header_store_config();
        assert_eq!(headers, crate::config::HeaderStoreConfig { enabled: true, ..Default::default() });
        let runtime = Runtime::open(&quiet_config(), storage.clone(), headers, &[], &mut None).unwrap();
        Self { runtime, storage, rt: tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap() }
    }
    fn producer(&self) -> (Producer, Arc<LaneCounters>, Weak<Runner>) {
        let mut cfg = quiet_config();
        cfg.postprocess.enabled = true;
        let runner = Arc::new(
            Runner::new(&self.runtime.owner, &cfg, Arc::new(crate::nntp::Pool::new(&[])), self.rt.handle().clone())
                .unwrap(),
        );
        let weak = Arc::downgrade(&runner);
        let lanes = Arc::new(LaneCounters::new(runner.steps.enabled as u64).unwrap());
        let producer = Producer::new(&runner, lanes.clone()).unwrap();
        drop(runner);
        (producer, lanes, weak)
    }
    fn observer(&self) -> postgres::Client {
        let mut client = postgres::Client::connect(&self.storage.url, postgres::NoTls).unwrap();
        client
            .batch_execute(&format!("set search_path to \"{}\";set statement_timeout='5s'", self.storage.schema))
            .unwrap();
        client
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.runtime.owner.close();
    }
}
struct Retained {
    owner: Arc<Owner>,
    handles: Option<Vec<Worker>>,
}
impl Retained {
    fn new(owner: &Arc<Owner>) -> Self {
        Self { owner: owner.clone(), handles: Some(Vec::new()) }
    }
    fn handles(&mut self) -> &mut Vec<Worker> {
        self.handles.as_mut().unwrap()
    }
    fn finish(&mut self) -> Result<()> {
        let handles = self.handles.take().unwrap();
        join_workers(&self.owner, handles).map(|_| ())
    }
}
impl Drop for Retained {
    fn drop(&mut self) {
        if let Some(handles) = self.handles.take() {
            let _ = join_workers(&self.owner, handles);
        }
    }
}
#[derive(Clone)]
struct Head {
    id: i64,
    at: i64,
    mono: i64,
    running: bool,
}
fn head(f: &Fixture) -> Option<Head> {
    f.runtime.owner.with(|client| {
        client.query_opt("select o.observation_id,o.observed_at_ms,o.monotonic_ns,o.stage_running,o.epoch,o.catalog_nonce,(select count(*) from native_metric_samples s where s.observation_id=o.observation_id) from native_runtime_observation_current h join native_runtime_observations o using(observation_id) where h.singleton",&[])?.map(|row| {
            assert_eq!(row.get::<_,Vec<u8>>(4).as_slice(),f.runtime.owner.epoch().unwrap().id().as_slice());
            assert_eq!(row.get::<_,Vec<u8>>(5).as_slice(),f.runtime.owner.epoch().unwrap().catalog().nonce().as_slice());assert!(row.get::<_,i64>(6)>0);
            Ok(Head{id:row.get(0),at:row.get(1),mono:row.get(2),running:row.get(3)})
        }).transpose()
    }).unwrap()
}
fn next_head(f: &Fixture, after: Option<i64>, timeout: Duration) -> Head {
    let until = Instant::now() + timeout;
    loop {
        if let Some(head) = head(f).filter(|head| after.is_none_or(|after| head.id > after)) {
            return head;
        }
        assert!(Instant::now() < until, "no new completed publication head");
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn wait_failed(owner: &Owner) -> Failure {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(failure) = owner.failure() {
            return failure;
        }
        assert!(Instant::now() < until, "publication worker did not report its actual failure");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn input(n: usize) -> crate::parser::Release {
    crate::parser::Release {
        name: format!("Publication.Maintenance.{n}"),
        group: "example.publication".into(),
        articles: vec![crate::parser::Article {
            filename: Some(format!("sample-{n}.bin")),
            message_id: format!("<publication-{n}@fixture.test>"),
            part: Some(1),
            total_parts: Some(1),
            bytes: 17,
            ..Default::default()
        }],
        ..Default::default()
    }
}
mod scenarios {
    use super::*;
    pub(super) fn recurring_publication_while_real_maintenance_is_paused(f: &Fixture) {
        let owner = f.runtime.owner.clone();
        let mut retained = Retained::new(&owner);
        owner
            .with(|client| {
                assert_eq!(pg::release_writer::save(client, &(0..32).map(input).collect::<Vec<_>>(), 1)?.releases, 32);
                Ok(())
            })
            .unwrap();
        let service = Arc::new(pg::service_cache::predb::Service::new(&owner).unwrap());
        let (paused, resume) = service.pause_after_index_for_test();
        let (completed, done) = mpsc::channel();
        let work = service.clone();
        retained.handles().push(
            spawn(&owner, "fixture-predb-maintenance".into(), move || {
                let mut cfg = crate::config::PredbConfig::default();
                cfg.enabled = true;
                cfg.late_per_second = 1;
                let report = work.maintenance(&cfg, now())?;
                completed.send(report.indexed)?;
                Ok(None)
            })
            .unwrap(),
        );
        paused.recv_timeout(Duration::from_secs(10)).unwrap();
        owner
            .with(|client| {
                assert_eq!(client.query_one("select count(*) from predb_name_pending", &[])?.get::<_, i64>(0), 31);
                assert!(client.query_one("select count(*) from predb_name_keys", &[])?.get::<_, i64>(0) > 0);
                Ok(())
            })
            .unwrap();
        let (producer, lanes, weak) = f.producer();
        let before = chrono::Utc::now().timestamp_millis();
        start_publication(&owner, Some(producer), Some(lanes), retained.handles()).unwrap();
        let first = next_head(f, None, Duration::from_secs(10));
        assert!(first.at >= before && first.running);
        // This is the unchanged production30-second cadence, not a fixture clock.
        let second = next_head(f, Some(first.id), Duration::from_secs(50));
        assert!(
            second.at > first.at && second.mono - first.mono >= 30_000_000_000,
            "a genuine recurring capture must advance real clocks"
        );
        assert!(done.try_recv().is_err(), "maintenance has not returned while both real frames published");
        assert!(weak.upgrade().is_some());
        resume.send(()).unwrap();
        assert_eq!(done.recv_timeout(Duration::from_secs(15)).unwrap(), 32);
        // Hold original work until the actual publication worker queues another read.
        let (entered, ready) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let held_owner = owner.clone();
        retained.handles().push(
            spawn(&owner, "fixture-active-owner".into(), move || {
                held_owner.with(|_| {
                    entered.send(())?;
                    wait.recv_timeout(Duration::from_secs(10))?;
                    Ok(())
                })?;
                Ok(None)
            })
            .unwrap(),
        );
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while owner.admission_waiters_for_test()[1] == 0 {
            assert!(Instant::now() < until);
            std::thread::yield_now();
        }
        owner.stop.store(true, Ordering::Release);
        release.send(()).unwrap();
        retained.finish().unwrap();
        assert!(weak.upgrade().is_none(), "the sole Producer/Runner has dropped before Owner.close");
        assert_eq!(owner.failure(), None);
        let mut check = f.observer();
        assert_eq!(
            check
                .query_one("select observation_id from native_runtime_observation_current where singleton", &[])
                .unwrap()
                .get::<_, i64>(0),
            second.id,
            "queued stop cannot publish or acknowledge a partial frame"
        );
    }
    pub(super) fn no_producer_still_publishes_header_only(f: &Fixture) {
        let owner = f.runtime.owner.clone();
        let mut retained = Retained::new(&owner);
        let before = chrono::Utc::now().timestamp_millis();
        start_publication(&owner, None, None, retained.handles()).unwrap();
        let frame = next_head(f, None, Duration::from_secs(10));
        assert!(frame.at >= before && !frame.running);
        retained.finish().unwrap();
        assert_eq!(owner.failure(), None);
    }
    pub(super) fn actual_publication_sql_error_is_fatal_and_rolls_back(f: &Fixture) {
        let owner = f.runtime.owner.clone();
        owner.with(|client| {client.batch_execute("alter table native_runtime_observations add constraint fixture_publication_refusal check(false) not valid")?;Ok(())}).unwrap();
        let (producer, lanes, weak) = f.producer();
        let mut retained = Retained::new(&owner);
        start_publication(&owner, Some(producer), Some(lanes), retained.handles()).unwrap();
        assert_eq!(wait_failed(&owner), Failure::Fatal);
        let error = retained.finish().unwrap_err();
        assert!(error.chain().any(|cause| {
            cause
                .downcast_ref::<postgres::Error>()
                .is_some_and(|pg| pg.code() == Some(&postgres::error::SqlState::CHECK_VIOLATION))
        }));
        assert_eq!(owner.failure(), Some(Failure::Fatal));
        assert!(weak.upgrade().is_none());
        let mut check = f.observer();
        assert_eq!(
            check.query_one("select count(*) from native_runtime_observations", &[]).unwrap().get::<_, i64>(0),
            0
        );
    }
    pub(super) fn original_publication_backend_loss_stays_unavailable(f: &Fixture) {
        let owner = f.runtime.owner.clone();
        let (producer, lanes, weak) = f.producer();
        let pid = owner.with(|client| Ok(client.query_one("select pg_backend_pid()", &[])?.get::<_, i32>(0))).unwrap();
        let epoch = owner.epoch().unwrap();
        let mut check = f.observer();
        let terminated:bool=check.query_one("select pg_terminate_backend(e.backend_pid) from native_runtime_current c join native_runtime_epochs e on e.epoch=c.epoch join pg_stat_activity a on a.pid=e.backend_pid and a.backend_start=e.backend_start where c.singleton and e.epoch=$1 and e.backend_pid=$2 and e.catalog_nonce=$3 and e.backend_pid<>pg_backend_pid()",&[&&epoch.id()[..],&pid,&&epoch.catalog().nonce()[..]]).unwrap().get(0);
        assert!(terminated);
        let mut retained = Retained::new(&owner);
        start_publication(&owner, Some(producer), Some(lanes), retained.handles()).unwrap();
        assert_eq!(wait_failed(&owner), Failure::Unavailable);
        let error = retained.finish().unwrap_err();
        assert_eq!(pg::outage::classify(&error), Failure::Unavailable);
        assert_eq!(owner.failure(), Some(Failure::Unavailable));
        assert!(weak.upgrade().is_none());
        assert_eq!(
            check.query_one("select count(*) from native_runtime_observations", &[]).unwrap().get::<_, i64>(0),
            0
        );
    }
    pub(super) fn constructor_error_after_publication_spawn_drains_retained_worker(f: &Fixture) {
        let owner = f.runtime.owner.clone();
        let (producer, lanes, weak) = f.producer();
        let mut retained = Retained::new(&owner);
        let mut cfg = quiet_config();
        cfg.postprocess.enabled = true;
        // Pool construction performs no IO; actual LIST identity validation rejects this endpoint.
        let pool = Arc::new(crate::nntp::Pool::new(&[crate::config::UsenetServer::new("", "", "", 119)]));
        let error = start_services(
            &owner,
            &cfg,
            pool,
            f.rt.handle().clone(),
            Some(lanes),
            Some(producer),
            None,
            f.runtime.db.clone(),
            f.runtime.configured_headers(),
            None,
            None,
            retained.handles(),
        )
        .unwrap_err();
        assert_eq!(retained.handles().len(), 1, "publication was already retained before the later constructor failed");
        fail(&owner, &error);
        let _ = retained.finish();
        assert_eq!(owner.failure(), Some(Failure::Fatal));
        assert!(weak.upgrade().is_none(), "partial startup must join and drop the actual Producer before Owner.close");
    }
}

macro_rules! cases {
    ($($case:ident),+ $(,)?)=> {
        fn case_body(name:&str)->Option<fn(&Fixture)> {match name {$(stringify!($case)=>Some(scenarios::$case),)+ _=>None}}
        $(#[test] #[ignore="requires ATLAS_PG_TEST_URL for an owned PostgreSQL schema and configured subprocess"]
        fn $case() {run_case(stringify!($case));})+
    };
}

struct Log {
    kept: Vec<u8>,
    overflow: bool,
}

// Retain a bounded prefix but keep draining: an overlong child cannot fill a pipe.
fn drain_log(mut stream: impl Read) -> io::Result<Log> {
    let mut kept = Vec::new();
    let mut overflow = false;
    let mut chunk = [0u8; 4096];
    loop {
        let count = match stream.read(&mut chunk) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            break;
        }
        let admitted = count.min(LOG_BYTES - kept.len());
        kept.extend_from_slice(&chunk[..admitted]);
        overflow |= admitted != count;
    }
    Ok(Log { kept, overflow })
}

// Declared after the parent schema/home, so cleanup runs before either drops.
struct OwnedChild {
    child: Child,
    reaped: bool,
    out: Option<JoinHandle<io::Result<Log>>>,
    err: Option<JoinHandle<io::Result<Log>>>,
}

impl OwnedChild {
    fn new(child: Child) -> Self {
        let mut owned = Self { child, reaped: false, out: None, err: None };
        let out = owned.child.stdout.take().expect("owned child stdout pipe");
        let err = owned.child.stderr.take().expect("owned child stderr pipe");
        owned.out = Some(thread::spawn(move || drain_log(out)));
        owned.err = Some(thread::spawn(move || drain_log(err)));
        owned
    }

    fn terminate_if_unreaped(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            if self.child.wait().is_ok() {
                self.reaped = true;
            }
        }
    }

    fn wait(&mut self) -> io::Result<(ExitStatus, bool)> {
        let until = Instant::now() + CHILD_DEADLINE;
        loop {
            if let Some(status) = self.child.try_wait()? {
                self.reaped = true;
                return Ok((status, false));
            }
            if Instant::now() >= until {
                // The child has not been reaped; terminate before waiting.
                let _ = self.child.kill();
                let status = self.child.wait()?;
                self.reaped = true;
                return Ok((status, true));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn logs(&mut self) -> (io::Result<Log>, io::Result<Log>) {
        fn join(reader: JoinHandle<io::Result<Log>>) -> io::Result<Log> {
            reader.join().map_err(|_| io::Error::other("owned log reader panicked"))?
        }
        // Join both even when either reader reports an I/O failure.
        let out = join(self.out.take().expect("owned stdout reader"));
        let err = join(self.err.take().expect("owned stderr reader"));
        (out, err)
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.terminate_if_unreaped();
        if let Some(reader) = self.out.take() {
            let _ = reader.join();
        }
        if let Some(reader) = self.err.take() {
            let _ = reader.join();
        }
    }
}

fn private_directory(path: &Path) {
    std::fs::create_dir(path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

fn write_config(home: &Path, schema: &crate::pg::testing::Schema) {
    let mut config = toml_edit::DocumentMut::new();
    config["storage"]["backend"] = toml_edit::value("postgres");
    config["storage"]["url"] = toml_edit::value(schema.url.as_str());
    config["storage"]["schema"] = toml_edit::value(schema.name.as_str());
    config["storage"]["max_connections"] = toml_edit::value(1);
    config["storage"]["allow_plaintext"] = toml_edit::value(true);
    config["header_store"]["enabled"] = toml_edit::value(true);
    config["header_store"]["dir"] = toml_edit::value("");
    config["postprocess"]["enabled"] = toml_edit::value(false);
    config["predb"]["enabled"] = toml_edit::value(false);
    config["spots"]["enabled"] = toml_edit::value(false);
    config["media"]["tvmaze"]["enabled"] = toml_edit::value(false);
    config["media"]["allocine"]["enabled"] = toml_edit::value(false);
    config["media"]["details_per_minute"] = toml_edit::value(0);
    config["inferred_parts"] = toml_edit::value(false);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let pending = home.join("config.toml.new");
    let mut file = options.open(&pending).unwrap();
    file.write_all(config.to_string().as_bytes()).unwrap();
    file.sync_all().unwrap();
    drop(file);
    std::fs::rename(pending, home.join("config.toml")).unwrap();
}

fn run_case(case: &str) {
    let body = case_body(case).expect("closed publication case registry");
    if let Some(selected) = std::env::var_os(PRIVATE_CASE) {
        assert_eq!(selected.to_str(), Some(case), "private publication case mismatch");
        let fixture = Fixture::new();
        body(&fixture);
        drop(fixture);
        println!("publication case completed: {case}");
        return;
    }
    assert!(cfg!(unix), "publication fixture requires private Unix file permissions");
    let schema = pg::testing::Schema::new().expect("ATLAS_PG_TEST_URL must select isolated PostgreSQL");
    let home = tempfile::tempdir().unwrap();
    private_directory(&home.path().join("tmp"));
    private_directory(&home.path().join("header-store"));
    write_config(home.path(), &schema);
    let selector = format!("{SELECTOR_PREFIX}{case}");
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", selector.as_str(), "--nocapture", "--test-threads=1"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home.path())
        .env("ATLAS_HOME", home.path())
        .env("TMPDIR", home.path().join("tmp"))
        .env("TMP", home.path().join("tmp"))
        .env("TEMP", home.path().join("tmp"))
        .env("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"))
        .env("ATLAS_NO_KEYRING", "1")
        .env(PRIVATE_CASE, case)
        .env(PRIVATE_SCHEMA, &schema.name)
        .current_dir(home.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("private publication child spawn");
    // The child guard drops before schema/home on normal return and unwinding.
    let mut child = OwnedChild::new(child);
    let waited = child.wait();
    if waited.is_err() {
        child.terminate_if_unreaped();
    }
    let (out, err) = child.logs();
    drop(child);
    let (status, timed_out) = waited.expect("private publication child wait");
    let out = out.expect("private stdout drain");
    let err = err.expect("private stderr drain");
    assert!(!timed_out, "publication case exceeded120-second child deadline");
    assert!(!out.overflow && !err.overflow, "publication case exceeded64-KiB pipe cap");
    assert!(status.success(), "private publication case failed: {status}");
    assert_eq!(String::from_utf8_lossy(&out.kept).matches(&format!("publication case completed: {case}")).count(), 1);
    drop(err);
    drop(home);
    drop(schema);
}
cases! {
    recurring_publication_while_real_maintenance_is_paused,
    no_producer_still_publishes_header_only,
    actual_publication_sql_error_is_fatal_and_rolls_back,
    original_publication_backend_loss_stays_unavailable,
    constructor_error_after_publication_spawn_drains_retained_worker,
}
