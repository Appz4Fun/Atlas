//! The one-off HEAD for probes from before the probe kept the server's body CRC, in the background indexer.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use atlas::{db, store};
use common::{Server, post, spawn_server, yenc_encode};

const GROUP: &str = "alt.binaries.test";

fn part(name: &str, n: u32, total: u32, file_size: usize, begin: usize, data: &[u8]) -> Vec<Vec<u8>> {
    vec![
        format!("=ybegin part={n} total={total} line=128 size={file_size} name={name}").into_bytes(),
        format!("=ypart begin={} end={}", begin + 1, begin + data.len()).into_bytes(),
        yenc_encode(data),
        format!("=yend size={} part={n} pcrc32={:08x}", data.len(), atlas::archive::crc32(data)).into_bytes(),
    ]
}

fn run(home: &std::path::Path, until: impl Fn(&rusqlite::Connection) -> bool) {
    let stop = Arc::new(AtomicBool::new(false));
    let cfg = atlas::config::load_config().unwrap();
    let runner = {
        let stop = stop.clone();
        std::thread::spawn(move || atlas::bg_indexer::run_until(cfg, stop))
    };
    let shard = store::shard_path(&home.join("atlas.db"), store::shard_of(GROUP));
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(conn) = db::open_shard(&shard)
            && until(&conn)
        {
            break;
        }
        assert!(Instant::now() < deadline, "never got there");
        std::thread::sleep(Duration::from_millis(100));
    }
    stop.store(true, Ordering::Relaxed);
    assert_eq!(runner.join().unwrap(), 0);
}

fn info(conn: &rusqlite::Connection) -> Option<atlas::probe::ProbeInfo> {
    let text: String = conn.query_row("select info from release_probe", [], |r| r.get(0)).ok()?;
    serde_json::from_str(&text).ok()
}

#[test]
fn an_old_movie_probe_gets_the_body_crc_from_one_head() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::set_var("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"));
    }
    let name = "Sample.Film.2024.1080p.WEB-DL.H264-TEST.mkv";
    let (a, b) = (vec![1u8; 1000], vec![2u8; 500]);
    let posts = vec![
        post(1, &format!(r#""{name}" yEnc (1/2)"#), 2_000_000, part(name, 1, 2, 1500, 0, &a)),
        post(2, &format!(r#""{name}" yEnc (2/2)"#), 2_000_000, part(name, 2, 2, 1500, 1000, &b)),
    ];
    let port = spawn_server(Server::new(posts));
    let config = serde_json::json!({
        "usenet_servers": [{"host": "127.0.0.1", "username": "bob", "password": "secret", "port": port, "ssl": false, "connections": 2, "priority": 1}],
        "groups": [GROUP], "index_mode": "backfill", "request_size": 50,
        "postprocess": {"body_fetches_per_minute": 6000, "settle_secs": 1, "complete_settle_secs": 1, "body_min_size_bytes": 0}
    });
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    db::create_db().unwrap();

    // a first run probes it; the mock's ARTICLE carries no body CRC, as an old probe kept none
    run(home.path(), |conn| info(conn).is_some());
    let shard = store::shard_path(&home.path().join("atlas.db"), store::shard_of(GROUP));
    let conn = db::open_shard(&shard).unwrap();
    let probe = info(&conn).unwrap();
    assert!(!probe.headers.contains_key(atlas::probe::BODY_CRC_HEADER));
    let id: i64 = conn.query_row("select release_id from release_probe", [], |r| r.get(0)).unwrap();
    conn.execute("insert or replace into release_category (release_id, category) values (?1, 2040)", [id]).unwrap();
    // as a database from before this change: the seed hasn't run
    conn.execute("delete from meta where key = 'pp_probe_body_crc'", []).unwrap();
    drop(conn);

    // the next start asks for one HEAD, and the probe and the first-article key take what it says
    run(home.path(), |conn| {
        conn.query_row("select body_crc from release_keys", [], |r| r.get::<_, Option<i64>>(0)).ok().flatten().is_some()
    });
    let conn = db::open_shard(&shard).unwrap();
    let probe = info(&conn).unwrap();
    assert_eq!(probe.headers.get(atlas::probe::BODY_CRC_HEADER).map(String::as_str), Some("3735928559"));
    assert_eq!(probe.yenc.as_ref().and_then(|y| y.name.as_deref()), Some(name), "the rest as it was");
    let walks: i64 = conn.query_row("select count(*) from pp_walks where kind = 'steps'", [], |r| r.get(0)).unwrap();
    assert_eq!(walks, 0, "no walk over every release for the new step");
}
