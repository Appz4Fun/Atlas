//! Actual Atlas binary fixtures. Only a selected test with an explicit PG URL
//! can create an isolated namespace; no private production authority is exposed.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::nntp::GROUP;

struct Schema {
    client: postgres::Client,
    name: String,
    url: String,
}

impl Schema {
    fn open() -> Self {
        let url = std::env::var("ATLAS_PG_TEST_URL")
            .ok()
            .filter(|url| !url.trim().is_empty())
            .expect("selected native background test requires explicit ATLAS_PG_TEST_URL");
        let mut connection: postgres::Config = url.parse().unwrap_or_else(|_| panic!("parse explicit fixture URL"));
        connection.connect_timeout(Duration::from_secs(5));
        connection.tcp_user_timeout(Duration::from_secs(5));
        connection.options("-c statement_timeout=10000 -c lock_timeout=1000");
        let client = connection.connect(postgres::NoTls).unwrap_or_else(|_| panic!("connect explicit fixture server"));
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap_or_else(|_| panic!("native fixture namespace entropy"));
        let name = format!("t_background_{}_{:016x}", std::process::id(), u64::from_le_bytes(nonce));
        let mut schema = Self { client, name, url };
        atlas::pg::migrate::run(&mut schema.client, &schema.name)
            .unwrap_or_else(|_| panic!("migrate isolated fixture namespace"));
        schema
            .client
            .batch_execute(&format!("set search_path to \"{}\"", schema.name))
            .unwrap_or_else(|_| panic!("select isolated fixture namespace"));
        schema
    }
}

impl Drop for Schema {
    fn drop(&mut self) {
        if self.client.batch_execute(&format!("drop schema if exists \"{}\" cascade", self.name)).is_err() {
            eprintln!("native background fixture cleanup failed for {}", self.name);
        }
    }
}

pub struct Harness {
    schema: Schema,
    home: Option<tempfile::TempDir>,
    starts: usize,
}

impl Harness {
    pub fn new() -> Self {
        let schema = Schema::open();
        let home = tempfile::tempdir().expect("private background fixture home");
        Self { schema, home: Some(home), starts: 0 }
    }

    fn home(&self) -> &Path {
        self.home.as_ref().expect("owned private fixture home").path()
    }

    pub fn configure(&self, mode: &str, port: u16, connections: u32) {
        let config = serde_json::json!({
            "usenet_servers": [{"host":"127.0.0.1", "username":"bob", "password":"secret", "port":port,
                "ssl":false, "connections":connections, "priority":1}],
            "group":GROUP, "groups":[GROUP], "index_mode":mode,
            "batch_size":200, "request_size":20, "parallel_groups":1,
            "auto_run_compact":false, "inferred_parts":false,
            "postprocess":{"enabled":false}, "predb":{"enabled":false}, "spots":{"enabled":false},
            "archive":{"enabled":false}, "metrics":{"enabled":false},
            "media":{"tvmaze":{"enabled":false}, "allocine":{"enabled":false}, "details_per_minute":0},
            "header_store":{"enabled":true, "dir":self.home().join("headers")},
            "storage":{"backend":"postgres", "url":self.schema.url, "schema":self.schema.name,
                "max_connections":2, "allow_plaintext":true}
        });
        let text = toml_edit::ser::to_string_pretty(&config)
            .unwrap_or_else(|_| panic!("serialize private fixture configuration"));
        let path = self.home().join("config.toml");
        let mut file = private_file(&path, false);
        file.write_all(text.as_bytes()).unwrap_or_else(|_| panic!("write private fixture configuration"));
    }

    /// The original cursor regression starts at (0,200). No identity, owner,
    /// queue, timing or receipt state is fabricated: normal Atlas bootstraps it.
    pub fn seed_cursors(&mut self, live: i64, backfill: i64) {
        assert_eq!(self.starts, 0, "seed only before the first real owner");
        self.schema
            .client
            .execute(
                "insert into groups(name,live_cursor,backfill_cursor) values($1,$2,$3)",
                &[&GROUP, &live, &backfill],
            )
            .unwrap_or_else(|_| panic!("seed original cursor fixture"));
    }

    pub fn cursors(&mut self) -> (i64, i64) {
        let row = self
            .schema
            .client
            .query_one("select live_cursor,backfill_cursor from groups where name=$1", &[&GROUP])
            .unwrap_or_else(|_| panic!("observe actual native cursors"));
        (row.get(0), row.get(1))
    }

    fn saved(&mut self) -> i64 {
        self.schema
            .client
            .query_one("select count(*) from releases", &[])
            .unwrap_or_else(|_| panic!("observe actual native releases"))
            .get(0)
    }

    pub fn start(&mut self) -> Runner<'_> {
        self.starts += 1;
        let log = private_file(&self.home().join(format!("background-{}.log", self.starts)), true);
        let child = Command::new(env!("CARGO_BIN_EXE_atlas"))
            .arg("--bg-indexer")
            .env_clear()
            .env("ATLAS_HOME", self.home())
            .env("ATLAS_NO_KEYRING", "1")
            .env("ATLAS_SAB_DIR", self.home().join("no-sab"))
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().expect("private background log clone")))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("start actual Atlas background binary");
        Runner { child, fixture: self, joined: false }
    }

    pub fn verify_nxv(&mut self, minimum_rows: u64) {
        let root: String = self
            .schema
            .client
            .query_one("select header_root from native_catalog_identity where singleton", &[])
            .unwrap_or_else(|_| panic!("read actual bootstrapped native header identity"))
            .get(0);
        let root = PathBuf::from(root);
        assert!(root.starts_with(self.home()), "native fixture header identity escaped its private home");
        let keep = atlas::nxv_store::KeepSet {
            live_from: i64::MIN,
            claimed: Default::default(),
            unlinked: Default::default(),
        };
        let (store, _) = atlas::nxv_store::Store::open(atlas::nxv_store::StoreConfig::new(&root), keep)
            .expect("reopen actual background NXV after owner drain");
        let mut rows = 0;
        for (group, day, _) in store.segments() {
            let (map, entries) = store.day_blocks(&group, day);
            let map = map.expect("actual background NXV mapping");
            for entry in entries {
                let view = nntp_xover_engine::container::NxvBlockView::from_mmap_slice(&map[entry.offset as usize..])
                    .expect("actual background NXV block");
                assert!(view.verify_body(), "actual background NXV checksum");
                if view.is_cold() {
                    view.verify_cold_body().expect("actual background NXV cold checksum");
                }
                rows += u64::from(view.header.row_count);
            }
        }
        assert!(rows >= minimum_rows, "native cursor advanced without durable overview input");
        store.close().expect("close verified background NXV observer");
        assert_no_sqlite(self.home());
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if std::thread::panicking() {
            if let Some(home) = self.home.take() {
                let retained = home.keep();
                eprintln!("private native background fixture retained at {}", retained.display());
            }
        }
    }
}

pub struct Runner<'a> {
    child: Child,
    fixture: &'a mut Harness,
    joined: bool,
}

impl Runner<'_> {
    pub fn cursors(&mut self) -> (i64, i64) {
        self.fixture.cursors()
    }
    pub fn saved(&mut self) -> i64 {
        self.fixture.saved()
    }

    /// Await actual protocol and commit observations, never a sleep as proof.
    pub fn wait_for(&mut self, what: &str, mut ready: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            assert!(
                self.child.try_wait().expect("observe owned Atlas child").is_none(),
                "actual Atlas child exited before {what}; inspect private fixture log"
            );
            if self.owned_pid_ready() && ready(self) {
                return;
            }
            assert!(Instant::now() < deadline, "actual Atlas child never reached {what}; inspect private fixture log");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn owned_pid_ready(&self) -> bool {
        let Ok(text) = std::fs::read_to_string(self.fixture.home().join("bg_indexer.pid")) else {
            return false;
        };
        let pid = text.trim().parse::<u32>().expect("owned Atlas PID file");
        assert_eq!(pid, self.child.id(), "PID file must belong to this exact Atlas child");
        true
    }

    /// Send TERM only to this owned Child PID. A forced kill is cleanup and
    /// always fails the test; it can never count as prompt graceful drain.
    #[cfg(unix)]
    pub fn stop_within(&mut self, limit: Duration) -> Duration {
        assert!(!self.joined, "one graceful stop per owned child");
        assert!(self.owned_pid_ready(), "actual owned PID before TERM");
        let began = Instant::now();
        assert_eq!(
            unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM) },
            0,
            "TERM exact owned Atlas child"
        );
        loop {
            if let Some(status) = self.child.try_wait().expect("wait for actual Atlas drain") {
                self.joined = true;
                let elapsed = began.elapsed();
                assert!(status.success(), "actual Atlas failed shutdown; inspect private fixture log");
                assert!(elapsed < limit, "actual Atlas shutdown exceeded its limit");
                let status =
                    std::fs::read_to_string(self.fixture.home().join("status.json")).expect("actual background status");
                assert!(status.contains(r#""status":"stopped""#), "actual background status did not record stopped");
                let locked: bool = self
                    .fixture
                    .schema
                    .client
                    .query_one(
                        "select exists(select 1 from pg_locks where locktype='advisory' and granted and objsubid=2
                    and database=(select oid from pg_database where datname=current_database())
                    and classid::bigint=(hashtext('atlas.indexer')::bigint & 4294967295)
                    and objid::bigint=(hashtext($1)::bigint & 4294967295))",
                        &[&self.fixture.schema.name],
                    )
                    .unwrap_or_else(|_| panic!("observe actual owner session drain"))
                    .get(0);
                assert!(!locked, "exited Atlas must release its original owner session");
                return elapsed;
            }
            if began.elapsed() >= limit {
                let _ = self.child.kill();
                let _ = self.child.wait();
                self.joined = true;
                panic!("actual Atlas required forced cleanup after TERM; prompt graceful drain failed");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Runner<'_> {
    fn drop(&mut self) {
        if !self.joined {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn private_file(path: &Path, new: bool) -> File {
    let mut options = OpenOptions::new();
    options.write(true);
    if new {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).unwrap_or_else(|_| panic!("create private native fixture file"))
}

fn assert_no_sqlite(root: &Path) {
    for entry in std::fs::read_dir(root).expect("inspect isolated background home") {
        let path = entry.expect("isolated background entry").path();
        if path.is_dir() {
            assert_no_sqlite(&path);
        } else {
            let mut magic = [0; 16];
            let n = File::open(path)
                .expect("inspect native background file")
                .read(&mut magic)
                .expect("native background file magic");
            assert!(n != magic.len() || &magic != b"SQLite format 3\0", "actual Atlas background created SQLite");
        }
    }
}
