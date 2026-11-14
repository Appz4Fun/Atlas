//! The ignored parent owns a migrated PostgreSQL schema and one private configured child.
//! No provider service starts; callbacks and publication use the original native owner.
use super::*;
use std::{
    sync::{atomic::AtomicUsize, mpsc},
    thread,
};

fn waiters(owner: &Owner, wanted: [usize; 3]) {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let actual = owner.admission_waiters_for_test();
        if actual == wanted {
            return;
        }
        assert!(Instant::now() < until, "admission {actual:?}, expected {wanted:?}");
        thread::yield_now();
    }
}
fn input() -> Vec<crate::parser::Release> {
    vec![crate::parser::Release {
        name: "Publication.Example".into(),
        group: "example.publication".into(),
        articles: vec![crate::parser::Article {
            filename: Some("sample.bin".into()),
            message_id: "<publication@example.test>".into(),
            part: Some(1),
            total_parts: Some(1),
            bytes: 17,
            ..Default::default()
        }],
        ..Default::default()
    }]
}
fn hold_selection(owner: &Arc<Owner>) -> (mpsc::Sender<()>, thread::JoinHandle<Result<()>>) {
    let (entered, ready) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let owner = owner.clone();
    let job = thread::spawn(move || {
        owner.with_selection(|_| {
            entered.send(())?;
            wait.recv_timeout(Duration::from_secs(10))?;
            Ok(())
        })
    });
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    (release, job)
}

fn configured_publication_case() {
    assert_eq!(std::env::var("ATLAS_NATIVE_TELEMETRY_OWNED").as_deref(), Ok("1"));
    assert!(std::env::var_os("ATLAS_HOME").is_some());
    let storage = crate::config::checked_storage_config().expect("explicit isolated owned storage");
    let mut config = crate::config::Config::default();
    config.postprocess.enabled = false;
    config.predb.enabled = false;
    config.spots.enabled = false;
    config.inferred_parts = false;
    config.media.tvmaze.enabled = false;
    config.media.allocine.enabled = false;
    config.media.details_per_minute = 0;
    let runtime = crate::pg::indexer_runtime::Runtime::open(
        &config,
        storage,
        crate::config::HeaderStoreConfig { enabled: true, ..Default::default() },
        &[],
        &mut None,
    )
    .unwrap();
    let owner = runtime.owner.clone();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    config.postprocess.enabled = true;
    let runner =
        Arc::new(Runner::new(&owner, &config, Arc::new(crate::nntp::Pool::new(&[])), rt.handle().clone()).unwrap());
    let lanes = Arc::new(LaneCounters::new(runner.steps.enabled as u64).unwrap());
    let producer = Producer::new(&runner, lanes.clone()).unwrap();
    let (release, held) = hold_selection(&owner);
    let (started, enter) = mpsc::channel();
    let done = Arc::new(AtomicUsize::new(0));
    let mut releases = Vec::new();
    let mut jobs = Vec::new();
    for n in 0..8 {
        let (release, wait) = mpsc::channel();
        releases.push(release);
        let (worker, started, done) = (owner.clone(), started.clone(), done.clone());
        jobs.push(thread::spawn(move || {
            worker.with_selection(|client| {
                assert_eq!(
                    crate::chunks::frontier_oldest(
                        &crate::pg::sql::Pg::new(client),
                        &[("example.publication".into(), 0)],
                        100
                    )?,
                    None
                );
                started.send(n)?;
                wait.recv_timeout(Duration::from_secs(10))?;
                done.fetch_add(1, Ordering::Release);
                Ok(())
            })
        }));
        waiters(&owner, [0, 0, n + 1]);
    }
    let first = owner.clone();
    let save = thread::spawn(move || {
        first.with_ingest(|client| {
            assert_eq!(crate::pg::release_writer::save(client, &input(), 1)?.parts, 1);
            Ok(())
        })
    });
    waiters(&owner, [1, 0, 8]);
    let next = owner.clone();
    let cursor = thread::spawn(move || {
        next.with_ingest(|client| {
            crate::db::update_backfill_cursor(&crate::pg::sql::Pg::new(client), "example.publication", 1999)?;
            Ok(())
        })
    });
    waiters(&owner, [2, 0, 8]);
    let (refreshed, refresh_done) = mpsc::channel();
    let (capture_now, may_capture) = mpsc::channel();
    let (captured, capture_done) = mpsc::channel();
    let (finish_cache, cache_may_finish) = mpsc::channel();
    let cache_owner = owner.clone();
    let cache = thread::spawn(move || -> Result<()> {
        assert!(matches!(producer.refresh()?, Refresh::Published));
        refreshed.send(())?;
        may_capture.recv_timeout(Duration::from_secs(10))?;
        crate::pg::stage_telemetry::capture(&cache_owner, Some(&lanes))?;
        captured.send(())?;
        // Keep the actual producer installed through the parent's commit assertions.
        cache_may_finish.recv_timeout(Duration::from_secs(10))?;
        Ok(())
    });
    waiters(&owner, [2, 1, 8]);
    release.send(()).unwrap();
    held.join().unwrap().unwrap();
    assert_eq!(enter.recv_timeout(Duration::from_secs(5)).unwrap(), 0);
    refresh_done.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(done.load(Ordering::Acquire), 0, "real refresh completed before the queued selections drained");
    save.join().unwrap().unwrap();
    cursor.join().unwrap().unwrap();
    // Keep two real ingestion callbacks pending for the SECOND service admission too.
    let replay_owner = owner.clone();
    let replay = thread::spawn(move || {
        replay_owner.with_ingest(|client| {
            assert_eq!(crate::pg::release_writer::save(client, &input(), 2)?.parts, 0);
            Ok(())
        })
    });
    waiters(&owner, [1, 0, 7]);
    let checkpoint_owner = owner.clone();
    let checkpoint = thread::spawn(move || {
        checkpoint_owner.with_ingest(|client| {
            crate::db::update_live_cursor(&crate::pg::sql::Pg::new(client), "example.publication", 2000)?;
            Ok(())
        })
    });
    waiters(&owner, [2, 0, 7]);
    let capture_not_before = chrono::Utc::now().timestamp_millis();
    capture_now.send(()).unwrap();
    waiters(&owner, [2, 1, 7]);
    releases[0].send(()).unwrap();
    assert_eq!(enter.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
    capture_done.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(done.load(Ordering::Acquire), 1, "real capture committed before all selected callbacks finished");
    replay.join().unwrap().unwrap();
    checkpoint.join().unwrap().unwrap();
    releases[1].send(()).unwrap();
    for n in 2..8 {
        assert_eq!(enter.recv_timeout(Duration::from_secs(5)).unwrap(), n);
        releases[n].send(()).unwrap();
    }
    for job in jobs {
        job.join().unwrap().unwrap();
    }
    waiters(&owner, [0; 3]);
    let head=owner.with(|client| {
        let row=client.query_one("select o.observation_id,o.epoch,o.catalog_nonce,o.observed_at_ms,(select count(*) from native_metric_samples s where s.observation_id=o.observation_id) from native_runtime_observation_current h join native_runtime_observations o using(observation_id) where h.singleton",&[])?;
        assert_eq!(row.get::<_,Vec<u8>>(1).as_slice(),owner.epoch().unwrap().id().as_slice());
        assert_eq!(row.get::<_,Vec<u8>>(2).as_slice(),owner.epoch().unwrap().catalog().nonce().as_slice());
        assert!(row.get::<_,i64>(3)>0&&row.get::<_,i64>(3)<=chrono::Utc::now().timestamp_millis());assert!(row.get::<_,i64>(4)>0);
        assert!(row.get::<_,i64>(3)>=capture_not_before,"the head must come from the requested current capture, not an older frame");
        assert_eq!(client.query_one("select count(*) from segments",&[])?.get::<_,i64>(0),1);Ok(row.get::<_,i64>(0))
    }).unwrap();
    finish_cache.send(()).unwrap();
    cache.join().unwrap().unwrap();
    let (release, held) = hold_selection(&owner);
    let stopped_owner = owner.clone();
    let stopped = thread::spawn(move || crate::pg::stage_telemetry::capture(&stopped_owner, None));
    waiters(&owner, [0, 1, 0]);
    owner.stop.store(true, Ordering::Release);
    assert!(crate::pg::outage::is_attempt_ended(&stopped.join().unwrap().unwrap_err()));
    waiters(&owner, [0; 3]);
    release.send(()).unwrap();
    held.join().unwrap().unwrap();
    assert_eq!(owner.failure(), None);
    owner.close();
    // A queued stop did not publish a new head or acknowledge a partial frame.
    let storage = crate::config::checked_storage_config().unwrap();
    let mut check = postgres::Client::connect(&storage.url, postgres::NoTls).unwrap();
    check.batch_execute(&format!("set search_path to \"{}\"", storage.schema)).unwrap();
    assert_eq!(
        check
            .query_one("select observation_id from native_runtime_observation_current where singleton", &[])
            .unwrap()
            .get::<_, i64>(0),
        head
    );
}

use std::{
    io::{self, Read, Write},
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    thread::JoinHandle,
};

const PRIVATE_CASE: &str = "ATLAS_NATIVE_NESTED_CASE";
const PRIVATE_SCHEMA: &str = "ATLAS_NATIVE_NESTED_SCHEMA";
const CASE: &str = "publication_under_selection";
const SELECTOR: &str = "postprocess::native::telemetry::selection_fair_tests::real_refresh_and_current_capture_progress_between_selection_turns_without_partial_stop_ack";
const LOG_BYTES: usize = 64 * 1024;
const CHILD_DEADLINE: Duration = Duration::from_secs(120);
const COMPLETED: &str = "native nested publication case completed";

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

#[test]
#[ignore = "requires ATLAS_PG_TEST_URL for an isolated PostgreSQL schema and private configured subprocess"]
fn real_refresh_and_current_capture_progress_between_selection_turns_without_partial_stop_ack() {
    if let Some(case) = std::env::var_os(PRIVATE_CASE) {
        assert_eq!(case.to_str(), Some(CASE), "closed private fixture case");
        let storage = crate::config::checked_storage_config().expect("private configured storage");
        assert_eq!(storage.schema, std::env::var(PRIVATE_SCHEMA).expect("parent-owned migrated schema"));
        assert_eq!(
            crate::config::header_store_config(),
            crate::config::HeaderStoreConfig { enabled: true, ..Default::default() }
        );
        assert_eq!(crate::config::header_store_config().root(), crate::paths::app_dir().join("header-store"));
        configured_publication_case();
        println!("{COMPLETED}");
        return;
    }
    assert!(cfg!(unix), "native fixture requires private Unix file permissions");
    let schema = crate::pg::testing::Schema::new().expect("ATLAS_PG_TEST_URL must select isolated PostgreSQL");
    let home = tempfile::tempdir().unwrap();
    private_directory(&home.path().join("tmp"));
    private_directory(&home.path().join("header-store"));
    write_config(home.path(), &schema);
    // Only this exact ignored selector is executed. The child cannot create a
    // second schema or inherit unrelated service/configuration environment.
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", SELECTOR, "--nocapture", "--test-threads=1"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home.path())
        .env("ATLAS_HOME", home.path())
        .env("TMPDIR", home.path().join("tmp"))
        .env("TMP", home.path().join("tmp"))
        .env("TEMP", home.path().join("tmp"))
        .env("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"))
        .env("ATLAS_NO_KEYRING", "1")
        .env("ATLAS_NATIVE_TELEMETRY_OWNED", "1")
        .env(PRIVATE_CASE, CASE)
        .env(PRIVATE_SCHEMA, &schema.name)
        .current_dir(home.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("private native publication child spawn");
    // Declared after schema/home: even panic cleanup reaps the child and joins
    // both pipe drainers before dropping the parent's schema or temporary root.
    let mut child = OwnedChild::new(child);
    let waited = child.wait();
    if waited.is_err() {
        child.terminate_if_unreaped();
    }
    let (out, err) = child.logs();
    drop(child);
    let (status, timed_out) = waited.expect("private publication child wait");
    let out = out.expect("private publication stdout drain");
    let err = err.expect("private publication stderr drain");
    assert!(!timed_out, "private publication child exceeded its 120-second deadline");
    assert!(!out.overflow && !err.overflow, "private publication output exceeded the per-pipe 64-KiB cap");
    // Keep destination-bearing diagnostics out of ordinary public test output.
    assert!(status.success(), "private publication child failed: {status}");
    assert_eq!(String::from_utf8_lossy(&out.kept).matches(COMPLETED).count(), 1, "missing completed publication case");
    drop(err);
    drop(home);
    drop(schema);
}
