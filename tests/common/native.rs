//! Test-only adapter to the original native runtime; no alternate database API.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::config::{Config, HeaderStoreConfig, UsenetServer};
use crate::indexer::{Db, Indexer, PassContext, RunStates};
use crate::nntp::BlockingPool;
use crate::nntp::Pool;
use crate::pg::indexer_runtime::Runtime;
use std::sync::Arc;

const CHILD_TEST: &str = "indexer::native_provider_contract::fixture_child";
const COMPLETED: &str = "provider-contract-completed";

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
        let name = format!("t_provider_{}_{:016x}", std::process::id(), u64::from_le_bytes(nonce));
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
            eprintln!("native provider fixture cleanup failed for {}", self.name);
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
    std::fs::write(home.join(COMPLETED), case).expect("record completed native provider oracles");
}

fn required_url() -> String {
    std::env::var("ATLAS_PG_TEST_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .expect("selected native provider test requires explicit ATLAS_PG_TEST_URL")
}

/// Run one fixture in a clean process before any live NXV global is initialized.
/// Child output stays private even on failure: PostgreSQL errors may contain
/// connection information, so no captured child output enters assertion debug.
pub fn run_child(case: &str) {
    run_child_on(case, CHILD_TEST);
}

/// Dedicated sibling contracts retain their own exact child registry selector.
pub fn run_child_on(case: &str, child_test: &str) {
    let url = required_url();
    // The parent owns schema cleanup even when a timed-out child must be killed.
    let schema = ParentSchema::open(&url);
    let home = tempfile::tempdir().expect("private native fixture home");
    let log_path = home.path().join("provider-child.log");
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
            .args(["--exact", child_test, "--ignored", "--test-threads=1", "--nocapture"])
            .env_clear()
            .env("ATLAS_HOME", home.path())
            .env("ATLAS_NO_KEYRING", "1")
            .env("ATLAS_PG_TEST_URL", url)
            .env("ATLAS_NATIVE_PROVIDER_CASE", case)
            .env("ATLAS_NATIVE_PROVIDER_SCHEMA", &schema.name)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().expect("private log clone")))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("start isolated native provider child"),
    );
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if let Some(status) = child.0.try_wait().expect("observe native provider child") {
            if !status.success() || std::fs::read(home.path().join(COMPLETED)).ok().as_deref() != Some(case.as_bytes())
            {
                let retained = home.keep();
                panic!("native provider child failed; private fixture retained at {}", retained.display());
            }
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.0.kill();
            let _ = child.0.wait();
            let retained = home.keep();
            panic!("native provider child timed out; private fixture retained at {}", retained.display());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub struct NativeRelease {
    pub id: i64,
    pub name: String,
    pub poster: String,
    pub parts: i64,
    pub file_total: Option<i64>,
    pub complete: bool,
    pub files: i64,
}

pub struct NativeCursor {
    pub key: String,
    pub live: Option<i64>,
    pub backfill: Option<i64>,
}

pub struct Harness {
    observer: crate::pg::Pool,
    runtime: Runtime,
    config: Config,
    home: PathBuf,
    headers: PathBuf,
    finished: bool,
    schema: String,
    words: Option<crate::pg::stage_words::Keeper>,
}

impl Harness {
    pub fn open(servers: Vec<UsenetServer>) -> Self {
        let url = required_url();
        let schema = std::env::var("ATLAS_NATIVE_PROVIDER_SCHEMA").expect("parent-owned isolated schema");
        let home = PathBuf::from(std::env::var_os("ATLAS_HOME").expect("isolated fixture home"));
        let mut config = Config { servers, inferred_parts: false, ..Config::default() };
        config.postprocess.enabled = false;
        config.predb.enabled = false;
        config.spots.enabled = false;
        config.media.tvmaze.enabled = false;
        config.media.allocine.enabled = false;
        config.media.details_per_minute = 0;
        let storage = crate::pg::StorageConfig {
            backend: crate::pg::Backend::Postgres,
            url,
            schema,
            max_connections: 2,
            allow_plaintext: true,
        };
        let observer = crate::pg::Pool::new(&storage).unwrap_or_else(|_| panic!("native fixture observer pool"));
        let headers = HeaderStoreConfig { enabled: true, dir: home.join("headers").to_string_lossy().into_owned() };
        // Normal API/cache readers recheck the selected destination and header
        // configuration. The fixture never consults an ambient real config.
        let selected = serde_json::json!({
            "storage":{"backend":"postgres","url":&storage.url,"schema":&storage.schema,
                "max_connections":storage.max_connections,"allow_plaintext":storage.allow_plaintext},
            "header_store":{"enabled":true,"dir":&headers.dir},
            "postprocess":{"enabled":false},"predb":{"enabled":false},"spots":{"enabled":false},
            "inferred_parts":false,"metrics":{"enabled":false},
            "media":{"tvmaze":{"enabled":false},"allocine":{"enabled":false},"details_per_minute":0}
        });
        let text = toml_edit::ser::to_string_pretty(&selected)
            .unwrap_or_else(|_| panic!("serialize private selected fixture configuration"));
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
        let schema = storage.schema.clone();
        let hosts = crate::nntp::server_keys(&config.servers);
        let runtime = Runtime::open(&config, storage, headers, &hosts, &mut None)
            .unwrap_or_else(|_| panic!("open original native fixture owner"));
        let effective = runtime.header_config().unwrap_or_else(|_| panic!("check original native header root"));
        crate::nxv_store::live::spawn_bounded(&effective).unwrap_or_else(|_| panic!("start original bounded live NXV"));
        crate::archive::sync_inline_lookups(&config);
        Self { observer, runtime, config, home, headers: effective.root(), finished: false, schema, words: None }
    }

    pub fn indexer(&self, mode: &str) -> Indexer {
        let pool = BlockingPool::new(&self.config.servers);
        pool.connect().expect("connect scripted loopback NNTP");
        self.indexer_on(pool, mode)
    }

    pub fn indexer_on(&self, pool: BlockingPool, mode: &str) -> Indexer {
        let mut indexer = Indexer::new(pool, mode, self.runtime.db.clone());
        indexer.stop = self.runtime.owner.stop.clone();
        indexer
    }

    pub fn context(&self, pool: Arc<Pool>) -> PassContext {
        PassContext { pool, states: RunStates::default(), stop: self.runtime.owner.stop.clone(), verbose: false }
    }

    pub fn db(&self) -> Db {
        self.runtime.db.clone()
    }

    /// The real original-owner delivery lane builds and ACKs Tantivy input.
    /// Tests observe actual search results instead of writing a ready flag.
    pub fn start_words(&mut self) {
        assert!(self.words.is_none(), "one actual word keeper per fixture");
        self.words = Some(
            crate::pg::stage_words::Keeper::start(&self.runtime.owner, self.runtime.configured_headers())
                .expect("start original native word keeper"),
        );
    }

    pub fn with_reader<T>(
        &self,
        read: impl FnOnce(&crate::pg::fresh_reader::ReadView<'_>) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let mut connection = self.observer.get()?;
        let view = crate::pg::fresh_reader::ReadView::open(
            &mut connection,
            &self.schema,
            crate::pg::fresh_reader::Budget { rows: 100, bytes: 8 * 1024 * 1024 },
            Instant::now() + Duration::from_secs(10),
        )?;
        read(&view)
    }

    /// Closed, bounded fixture observation of genuine ingestion results.
    /// Collection tests require poster and file-total identity as well as IDs.
    pub fn release_rows(&self) -> Vec<NativeRelease> {
        self.runtime
            .owner
            .with(|client| {
                let mut tx = client
                    .build_transaction()
                    .isolation_level(postgres::IsolationLevel::RepeatableRead)
                    .read_only(true)
                    .start()?;
                tx.batch_execute("set local statement_timeout='10s';set local lock_timeout='1s'")?;
                let rows = tx.query(
                    "select r.id,r.name,r.poster,r.parts,r.file_total,r.complete,
                (select count(*) from files f where f.release_id=r.id) from releases r order by r.id limit 257",
                    &[],
                )?;
                anyhow::ensure!(rows.len() <= 256, "native fixture release snapshot exceeds admission");
                let rows = rows
                    .into_iter()
                    .map(|row| NativeRelease {
                        id: row.get(0),
                        name: row.get(1),
                        poster: row.get(2),
                        parts: row.get(3),
                        file_total: row.get(4),
                        complete: row.get(5),
                        files: row.get(6),
                    })
                    .collect();
                tx.commit()?;
                Ok(rows)
            })
            .expect("observe original-owner native release snapshot")
    }

    /// A finite unique-row observation for BODY-derived ingestion names.
    pub fn display_name(&self, raw_name: &str) -> Option<String> {
        self.runtime
            .owner
            .with(|client| {
                anyhow::ensure!(raw_name.len() <= 4096, "native fixture raw-name lookup exceeds admission");
                let mut tx = client
                    .build_transaction()
                    .isolation_level(postgres::IsolationLevel::RepeatableRead)
                    .read_only(true)
                    .start()?;
                tx.batch_execute("set local statement_timeout='10s';set local lock_timeout='1s'")?;
                let rows = tx.query(
                    "select case when octet_length(display_name)<=1048576 then display_name end,
                octet_length(display_name) from releases where name=$1 limit 2",
                    &[&raw_name],
                )?;
                anyhow::ensure!(rows.len() == 1, "native fixture raw name is missing or ambiguous");
                let bytes: Option<i32> = rows[0].try_get(1)?;
                anyhow::ensure!(
                    bytes.is_none_or(|n| (0..=1048576).contains(&n)),
                    "native fixture display-name extent exceeds admission"
                );
                let name = rows[0].try_get(0)?;
                tx.commit()?;
                Ok(name)
            })
            .expect("observe actual BODY-derived native display name")
    }

    pub fn cursors(&self) -> Vec<NativeCursor> {
        self.runtime
            .owner
            .with(|client| {
                let mut tx = client
                    .build_transaction()
                    .isolation_level(postgres::IsolationLevel::RepeatableRead)
                    .read_only(true)
                    .start()?;
                tx.batch_execute("set local statement_timeout='10s';set local lock_timeout='1s'")?;
                let rows =
                    tx.query("select name,live_cursor,backfill_cursor from groups order by name limit 257", &[])?;
                anyhow::ensure!(rows.len() <= 256, "native fixture cursor snapshot exceeds admission");
                let rows = rows
                    .into_iter()
                    .map(|row| NativeCursor { key: row.get(0), live: row.get(1), backfill: row.get(2) })
                    .collect();
                tx.commit()?;
                Ok(rows)
            })
            .expect("observe actual native cursor snapshot")
    }

    pub fn articles(&mut self) -> i64 {
        self.observer
            .get()
            .expect("native fixture observer")
            .query_one("select count(*) from segments", &[])
            .expect("native committed article count")
            .get(0)
    }

    pub fn backfill_cursor(&mut self, group: &str) -> i64 {
        self.observer
            .get()
            .expect("native fixture observer")
            .query_one("select backfill_cursor from groups where name=$1", &[&group])
            .expect("native committed group cursor")
            .get(0)
    }

    /// The real BlockingPool runtime drains before sealing NXV or closing the
    /// original owner. Reopen and verify the actual committed NXV blocks.
    pub fn finish(&mut self, indexer: Indexer, minimum_rows: u64) {
        if let Some(words) = self.words.take() {
            words.finish().expect("drain original native word keeper");
        }
        self.runtime.owner.stop.store(true, Ordering::Release);
        indexer.client.disconnect();
        drop(indexer);
        let live = crate::nxv_store::live::handle().expect("original live NXV handle");
        assert!(live.failure().is_none(), "live NXV failed before drain");
        crate::nxv_store::live::finish();
        assert!(live.failure().is_none(), "live NXV failed while sealing");
        drop(live);
        assert_eq!(self.runtime.owner.failure(), None, "provider quirks cannot fail storage ownership");
        self.runtime.owner.close();
        self.finished = true;
        let keep = crate::nxv_store::KeepSet {
            live_from: i64::MIN,
            claimed: Default::default(),
            unlinked: Default::default(),
        };
        let (store, _) = crate::nxv_store::Store::open(crate::nxv_store::StoreConfig::new(&self.headers), keep)
            .expect("reopen actually sealed NXV");
        let mut rows = 0;
        for (group, day, _) in store.segments() {
            let (map, entries) = store.day_blocks(&group, day);
            let map = map.expect("committed NXV mapping");
            for entry in entries {
                let view = nntp_xover_engine::container::NxvBlockView::from_mmap_slice(&map[entry.offset as usize..])
                    .expect("committed NXV block");
                assert!(view.verify_body(), "committed NXV checksum");
                if view.is_cold() {
                    view.verify_cold_body().expect("committed NXV cold checksum");
                }
                rows += u64::from(view.header.row_count);
            }
        }
        assert!(rows >= minimum_rows, "committed cursor requires durable NXV input");
        store.close().expect("close verified NXV observer");
        assert_no_sqlite(&self.home);
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if !self.finished {
            if let Some(words) = self.words.take() {
                let _ = words.finish();
            }
            self.runtime.owner.stop.store(true, Ordering::Release);
            crate::nxv_store::live::finish();
            self.runtime.owner.close();
        }
    }
}

fn assert_no_sqlite(root: &Path) {
    for entry in std::fs::read_dir(root).expect("inspect isolated native home") {
        let path = entry.expect("isolated native entry").path();
        if path.is_dir() {
            assert_no_sqlite(&path);
        } else {
            let mut magic = [0; 16];
            let n = File::open(path).expect("inspect native fixture file").read(&mut magic).expect("native file magic");
            assert!(n != magic.len() || &magic != b"SQLite format 3\0", "native provider fixture created SQLite");
        }
    }
}
