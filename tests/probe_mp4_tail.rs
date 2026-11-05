//! The probe on an MP4 posted whole with its `moov` after its `mdat`: the front holds no tracks, soo it reads the
//! parts from where the `mdat` ends and finds them there.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use atlas::archive::crc32;
use atlas::{db, store};
use common::{Server, post, spawn_server, yenc_encode};

const GROUP: &str = "alt.binaries.test";
const SEGMENT: usize = 80_000;

fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend(kind);
    out.extend(body);
    out
}

/// ftyp, 1 MB of mdat (past the six articles the front reads), then a moov with a 3840x2160 HEVC track and 90 s.
fn film() -> Vec<u8> {
    let mut entry = vec![0u8; 86];
    entry[4..8].copy_from_slice(b"hvc1");
    entry[32..34].copy_from_slice(&3840u16.to_be_bytes());
    entry[34..36].copy_from_slice(&2160u16.to_be_bytes());
    let mut stsd = vec![0u8; 8];
    stsd.extend(&entry);
    let stbl = mp4_box(b"stbl", &mp4_box(b"stsd", &stsd));
    let mut hdlr = vec![0u8; 8];
    hdlr.extend(b"vide");
    hdlr.extend([0u8; 12]);
    let mdia = [mp4_box(b"hdlr", &hdlr), mp4_box(b"minf", &stbl)].concat();
    let mut mvhd = vec![0u8; 20];
    mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
    mvhd[16..20].copy_from_slice(&90_000u32.to_be_bytes());
    let moov = mp4_box(b"moov", &[mp4_box(b"mvhd", &mvhd), mp4_box(b"trak", &mp4_box(b"mdia", &mdia))].concat());
    let frames: Vec<u8> = (0..1_000_000).map(|i| (i * 31 % 251) as u8).collect();
    [mp4_box(b"ftyp", b"isom\0\0\0\0isom"), mp4_box(b"mdat", &frames), moov].concat()
}

fn posts_of(name: &str, data: &[u8]) -> Vec<common::Post> {
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
            post(n as u64, &format!("[1/1] \"{name}\" yEnc ({n}/{total})"), 2_000_000, body)
        })
        .collect()
}

#[test]
fn an_mp4_with_its_moov_at_the_end_gives_its_tracks() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::set_var("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"));
    }
    let port = spawn_server(Server::new(posts_of("Some.Film.2001.2160p.WEB.mp4", &film())));
    let config = serde_json::json!({
        "usenet_servers": [{"host": "127.0.0.1", "username": "bob", "password": "secret", "port": port, "ssl": false, "connections": 2, "priority": 1}],
        "groups": [GROUP], "index_mode": "backfill", "request_size": 50,
        "postprocess": {"body_fetches_per_minute": 6000, "settle_secs": 1, "complete_settle_secs": 1, "body_min_size_bytes": 0},
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
    let deadline = Instant::now() + Duration::from_secs(60);
    let text: String = loop {
        let conn = db::open_shard(&shard).unwrap();
        if let Ok(text) = conn.query_row("select info from release_probe", [], |r| r.get(0)) {
            break text;
        }
        assert!(Instant::now() < deadline, "no probe");
        std::thread::sleep(Duration::from_millis(100));
    };
    stop.store(true, Ordering::Relaxed);
    assert_eq!(runner.join().unwrap(), 0);

    let info: atlas::probe::ProbeInfo = serde_json::from_str(&text).unwrap();
    let media = info.media.as_ref().expect("an mp4");
    assert_eq!(media.video.len(), 1, "the tracks from the moov at the end: {media:?}");
    assert_eq!(media.video[0].codec, "hvc1");
    assert_eq!((media.video[0].width, media.video[0].height), (Some(3840), Some(2160)));
    assert_eq!(media.duration_secs, Some(90.0));
}
