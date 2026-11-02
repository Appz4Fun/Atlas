//! Stills from the middle of a release: an HEVC Matroska file posted as itself, and the same file in a stored
//! RAR set, each get a keyframe at a quarter, half and three quarters, read from articles there and decoded by
//! our own decoder into JPEGs.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use atlas::archive::crc32;
use atlas::archive::testkit::{R4_MARK, r4_block, r4_file_fields, r4_main};
use atlas::{db, store};
use common::{Server, post, spawn_server, yenc_encode};

const GROUP: &str = "alt.binaries.test";
const SEGMENT: usize = 20_000;
/// what each article claims to be, soo the releases are big enough to want stills
const CLAIMED: u64 = 12_000_000;

fn clip() -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/hevc/clip.mkv", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// The two volumes of a stored RAR4 set holding `inner` as `name`.
fn volumes(name: &str, inner: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let fields = r4_file_fields(name.as_bytes(), inner.len() as u32);
    let (one, two) = inner.split_at(inner.len() / 2);
    let first =
        [R4_MARK.to_vec(), r4_main(0x0001), r4_block(0x74, 0x0002, &fields, Some(one.len() as u32)), one.to_vec()]
            .concat();
    let last = [
        R4_MARK.to_vec(),
        r4_main(0x0001),
        r4_block(0x74, 0x0001, &fields, Some(two.len() as u32)),
        two.to_vec(),
        r4_block(0x7b, 0x4000, &[], None),
    ]
    .concat();
    (first, last)
}

/// A file's posts, `SEGMENT` bytes each.
fn posts_of(first_number: u64, index: usize, count: usize, name: &str, data: &[u8]) -> Vec<common::Post> {
    let total = data.len().div_ceil(SEGMENT);
    data.chunks(SEGMENT)
        .enumerate()
        .map(|(i, chunk)| {
            let n = i + 1;
            let begin = i * SEGMENT;
            let body = vec![
                format!("=ybegin part={n} total={total} line=128 size={} name={name}", data.len()).into_bytes(),
                format!("=ypart begin={} end={}", begin + 1, begin + chunk.len()).into_bytes(),
                yenc_encode(chunk),
                format!("=yend size={} part={n} pcrc32={:08x}", chunk.len(), crc32(chunk)).into_bytes(),
            ];
            let subject = format!("[{index}/{count}] \"{name}\" yEnc ({n}/{total})");
            post(first_number + i as u64, &subject, CLAIMED, body)
        })
        .collect()
}

#[test]
fn hevc_releases_get_three_stills_from_their_middles() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::set_var("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"));
    }
    let inner = clip();
    let plain = "Plain.Film.2001.1080p.WEB-DL.x265-GRP";
    let mut posts = posts_of(1, 1, 1, &format!("{plain}.mkv"), &inner);
    let set = "Stored.Film.2002.1080p.BluRay.x265-GRP";
    let (first, last) = volumes(&format!("{set}.mkv"), &inner);
    let next = posts.len() as u64 + 1;
    posts.extend(posts_of(next, 1, 2, &format!("{set}.rar"), &first));
    let next = posts.len() as u64 + 1;
    posts.extend(posts_of(next, 2, 2, &format!("{set}.r00"), &last));
    let port = spawn_server(Server::new(posts));

    let config = serde_json::json!({
        "usenet_servers": [{"host": "127.0.0.1", "username": "bob", "password": "secret", "port": port, "ssl": false, "connections": 2, "priority": 1}],
        "groups": [GROUP], "index_mode": "backfill", "request_size": 50,
        "postprocess": {"body_fetches_per_minute": 6000, "settle_secs": 1, "complete_settle_secs": 1,
            "body_min_size_bytes": 0, "bodies_per_release": 16},
        "media": {"tvmaze": {"enabled": false}}
    });
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    db::create_db().unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let cfg = atlas::config::load_config().unwrap();
    let runner = {
        let stop = stop.clone();
        std::thread::spawn(move || atlas::bg_indexer::run_until(cfg, stop))
    };
    let shard = store::shard_path(&home.path().join("atlas.db"), store::shard_of(GROUP));
    let deadline = Instant::now() + Duration::from_secs(90);
    let rows: Vec<(String, i64, i64, i64, i64)> = loop {
        let conn = db::open_shard(&shard).unwrap();
        let rows: Vec<(String, i64, i64, i64, i64)> = conn
            .prepare(
                "select r.name, s.position, s.at_ms, s.width, s.height from release_stills s
                 join releases r on r.id = s.release_id order by r.name, s.position",
            )
            .and_then(|mut q| {
                q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect()
            })
            .unwrap_or_default();
        if rows.len() == 6 {
            break rows;
        }
        assert!(Instant::now() < deadline, "stills so far: {rows:?}");
        std::thread::sleep(Duration::from_millis(100));
    };
    stop.store(true, Ordering::Relaxed);
    assert_eq!(runner.join().unwrap(), 0);

    for name in [plain, set] {
        let mine: Vec<_> = rows.iter().filter(|r| r.0.starts_with(&name[..10])).collect();
        assert_eq!(mine.iter().map(|r| r.1).collect::<Vec<_>>(), [25, 50, 75], "{name}: {rows:?}");
        assert!(mine.iter().all(|r| (r.3, r.4) == (320, 180)));
        // later in the file, later in the film: keyframes a second apart over 12 s
        let times: Vec<i64> = mine.iter().map(|r| r.2).collect();
        assert!(times.windows(2).all(|w| w[0] < w[1]) && times[0] >= 1000 && times[2] <= 11_000, "{times:?}");
    }
    // the JPEGs are kept by their hash
    let stills: Vec<_> = std::fs::read_dir(home.path().join("stills")).unwrap().filter_map(Result::ok).collect();
    assert!(!stills.is_empty());
    for f in stills {
        assert_eq!(&std::fs::read(f.path()).unwrap()[..2], &[0xff, 0xd8]);
    }
}
