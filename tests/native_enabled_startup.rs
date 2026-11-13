//! Normal binary admission, original canonical owner and actual enabled lanes.
//! Explicitly selected Linux/PG test: no ambient account or external endpoint.
#![cfg(target_os = "linux")]

use atlas as atlas_api;
#[path = "common/nntp.rs"]
mod nntp;

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

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
            .expect("explicit ATLAS_PG_TEST_URL is mandatory for the enabled-startup test");
        let mut options: postgres::Config = url.parse().expect("fixture connection format");
        options.connect_timeout(Duration::from_secs(5));
        options.tcp_user_timeout(Duration::from_secs(5));
        options.options("-c statement_timeout=10000 -c lock_timeout=1000");
        let client = options.connect(postgres::NoTls).unwrap_or_else(|_| panic!("connect private fixture server"));
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).expect("fixture namespace entropy");
        let name = format!("t_enabled_{}_{:016x}", std::process::id(), u64::from_le_bytes(nonce));
        let mut schema = Self { client, name, url };
        atlas::pg::migrate::run(&mut schema.client, &schema.name).unwrap_or_else(|_| panic!("migrate private schema"));
        schema.client.batch_execute(&format!("set search_path to \"{}\"", schema.name)).unwrap();
        schema
    }
}
impl Drop for Schema {
    fn drop(&mut self) {
        if self.client.batch_execute(&format!("drop schema if exists \"{}\" cascade", self.name)).is_err() {
            eprintln!("enabled-startup fixture schema cleanup failed");
        }
    }
}

struct Runner(Child);
impl Runner {
    fn stop(&mut self) {
        if self.0.try_wait().unwrap().is_some() {
            return;
        }
        unsafe {
            libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(status.success(), "normal enabled native shutdown failed");
                return;
            }
            if Instant::now() >= deadline {
                self.0.kill().unwrap();
                self.0.wait().unwrap();
                panic!("normal enabled native shutdown exceeded deadline");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Runner {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            self.0.kill().ok();
            self.0.wait().ok();
        }
    }
}

struct Http {
    url: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Http {
    fn open() -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                if let Some(request) = server.recv_timeout(Duration::from_millis(100)).unwrap() {
                    // Every configured HTTP origin is owned by this fixture.
                    // Fresh non-media rows need no metadata response to start a lane.
                    let _ = request.respond(tiny_http::Response::from_string("{}").with_status_code(404));
                }
            }
        });
        Self { url, stop, thread: Some(thread) }
    }
}
impl Drop for Http {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn private_file(path: &std::path::Path) -> File {
    OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).unwrap()
}
fn thread_names(pid: u32) -> Vec<String> {
    std::fs::read_dir(format!("/proc/{pid}/task"))
        .unwrap()
        .map(|task| std::fs::read_to_string(task.unwrap().path().join("comm")).unwrap_or_default().trim().to_owned())
        .collect()
}
fn lane_count(names: &[String], expected: &str) -> usize {
    // Linux comm contains at most 15 bytes, including Rust's actual thread names.
    let expected = &expected[..expected.len().min(15)];
    names.iter().filter(|name| name.as_str() == expected).count()
}
fn no_sqlite(home: &std::path::Path) {
    for entry in std::fs::read_dir(home).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            no_sqlite(&path);
        } else {
            let mut prefix = [0; 16];
            let size = File::open(path).unwrap().read(&mut prefix).unwrap();
            assert_ne!(&prefix[..size], b"SQLite format 3\0");
        }
    }
}

#[test]
#[ignore = "explicit Linux PG qualification; launches the real Atlas binary and owned fixture transports"]
fn normal_binary_starts_configured_native_services_without_disabling_them() {
    let mut schema = Schema::open();
    let home = tempfile::tempdir().unwrap();
    let http = Http::open();
    let server = nntp::Server::new(
        (1..=2).map(|n| nntp::post(n, &format!("\"Enabled.Startup.fixture.bin\" yEnc ({n}/2)"), 100, vec![])).collect(),
    );
    let port = nntp::spawn_server(server.clone());
    let provider = |key: &str| serde_json::json!({(key):"fixture", "base_url":http.url, "image_base_url":http.url, "requests_per_minute":1});
    let config = serde_json::json!({
        "usenet_servers":[{"host":"127.0.0.1","port":port,"username":"bob","password":"secret",
            "ssl":false,"connections":2,"priority":1}],
        "group":nntp::GROUP,"groups":[nntp::GROUP],"index_mode":"backfill",
        "request_size":10,"batch_size":10,"parallel_groups":1,"max_unsaved_headers":100,
        "header_store":{"enabled":true,"dir":home.path().join("headers")},
        "storage":{"backend":"postgres","url":schema.url,"schema":schema.name,"max_connections":4,"allow_plaintext":true},
        "postprocess":{"enabled":true,"workers":1,"body_connections":1,"body_max_bytes":134217728,
            "settle_secs":3600,"complete_settle_secs":3600},
        "predb":{"enabled":true,"sources":[],"poll_minutes":5},
        "spots":{"enabled":true,"comments":true,"concurrency":1,"blacklist_url":format!("{}/blacklist",http.url)},
        "inferred_parts":true,"archive":{"enabled":true},
        "media":{"parse":true,"nfo":true,"details_per_minute":1,
            "tvmaze":{"enabled":true,"base_url":http.url,"requests_per_minute":1},
            "allocine":{"enabled":true,"base_url":http.url,"wikidata_url":http.url,"requests_per_minute":1},
            "tmdb":provider("api_key"),"tvdb":provider("api_key"),"omdb":provider("api_key"),
            "trakt":provider("client_id"),"opensubtitles":provider("api_key")},
        "metrics":{"enabled":false},"auto_run_compact":false
    });
    let text = toml_edit::ser::to_string_pretty(&config).unwrap();
    let selected = home.path().join("config.toml");
    private_file(&selected).write_all(text.as_bytes()).unwrap();
    let log = private_file(&home.path().join("startup.log"));
    let child = Command::new(env!("CARGO_BIN_EXE_atlas"))
        .arg("--bg-indexer")
        .env_clear()
        .env("ATLAS_HOME", home.path())
        .env("ATLAS_NO_KEYRING", "1")
        .env("ATLAS_SAB_DIR", home.path().join("absent-sab"))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .unwrap();
    let mut runner = Runner(child);
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        assert!(runner.0.try_wait().unwrap().is_none(), "normal binary refused the enabled native profile");
        let parts: i64 = schema.client.query_one("select count(*) from segments", &[]).unwrap().get(0);
        let sample = schema.client.query_opt("select o.stage_running,o.enabled_mask from native_runtime_observation_current c join native_runtime_observations o using(observation_id)", &[]).unwrap();
        let running = sample
            .as_ref()
            .is_some_and(|row| row.get::<_, bool>(0) && row.get::<_, Option<i64>>(1).is_some_and(|mask| mask != 0));
        let names = thread_names(runner.0.id());
        let lanes = [
            "atlas-native-db-0",
            "atlas-native-net-0",
            "atlas-native-cache",
            "atlas-native-details",
            "atlas-native-inferred",
            "atlas-native-predb-feed",
        ]
        .iter()
        .all(|name| lane_count(&names, name) >= 1)
            && lane_count(&names, "atlas-native-spots") >= 2; // Spotnet + comments share Linux's truncated name.
        if parts == 2 && running && lanes {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "actual enabled service lanes/header persistence/Stage observation did not appear"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(server.accepted.load(Ordering::SeqCst) > 0);
    assert!(server.xovers.load(Ordering::SeqCst) > 0);
    assert_eq!(
        schema.client.query_one("select count(*) from native_catalog_identity", &[]).unwrap().get::<_, i64>(0),
        1
    );
    assert_eq!(
        schema.client.query_one("select count(*) from meta where key='key_salt'", &[]).unwrap().get::<_, i64>(0),
        1
    );
    assert_eq!(std::fs::read_to_string(&selected).unwrap(), text, "startup must not rewrite flags to gain admission");
    no_sqlite(home.path());
    runner.stop();
    no_sqlite(home.path());
}
