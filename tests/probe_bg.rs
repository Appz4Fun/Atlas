//! The probe step in the background indexer, against a fake server: yEnc lines, headers, tracks and NFO.
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

fn el(id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend([0x40 | (body.len() >> 8) as u8, body.len() as u8]);
    out.extend(body);
    out
}

/// A Matroska header with one video track, padded to `size`.
fn first_part(size: usize) -> Vec<u8> {
    let video = [el(&[0xB0], &[0x07, 0x80]), el(&[0xBA], &[0x04, 0x38])].concat();
    let track = [el(&[0x83], &[1]), el(&[0x86], b"V_MPEG4/ISO/AVC"), el(&[0xE0], &video)].concat();
    let tracks = el(&[0x16, 0x54, 0xAE, 0x6B], &el(&[0xAE], &track));
    let segment = el(&[0x18, 0x53, 0x80, 0x67], &tracks);
    let mut data = [el(&[0x1A, 0x45, 0xDF, 0xA3], &el(&[0x42, 0x82], b"matroska")), segment].concat();
    data.resize(size, 0);
    data
}

fn part(
    name: &str,
    n: u32,
    total: u32,
    file_size: usize,
    begin: usize,
    data: &[u8],
    file_crc: Option<u32>,
) -> Vec<Vec<u8>> {
    let mut yend = format!("=yend size={} part={n} pcrc32={:08x}", data.len(), crc32(data));
    if let Some(crc) = file_crc {
        yend.push_str(&format!(" crc32={crc:08x}"));
    }
    vec![
        format!("=ybegin part={n} total={total} line=128 size={file_size} name={name}").into_bytes(),
        format!("=ypart begin={} end={}", begin + 1, begin + data.len()).into_bytes(),
        yenc_encode(data),
        yend.into_bytes(),
    ]
}

#[test]
fn a_probe_reads_what_a_release_is_without_downloading_it() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::set_var("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"));
    }
    let (first, middle, last) = (first_part(1000), vec![1u8; 1000], vec![2u8; 500]);
    let file_crc = crc32(&[first.clone(), middle.clone(), last.clone()].concat());
    let name = "Sample.Film.2024.1080p.WEB-DL.H264-TEST.mkv";
    let nfo = b"Password: letmein\r\nhttps://www.imdb.com/title/tt0133093/\r\n".to_vec();
    let posts = vec![
        post(
            1,
            r#"[1/2] "Sample.Film.2024.1080p.WEB-DL.H264-TEST.nfo" yEnc (1/1)"#,
            2_000_000,
            part("x.nfo", 1, 1, nfo.len(), 0, &nfo, None),
        ),
        post(
            2,
            r#"[2/2] "Sample.Film.2024.1080p.WEB-DL.H264-TEST.mkv" yEnc (1/3)"#,
            2_000_000,
            part(name, 1, 3, 2500, 0, &first, None),
        ),
        post(
            3,
            r#"[2/2] "Sample.Film.2024.1080p.WEB-DL.H264-TEST.mkv" yEnc (2/3)"#,
            2_000_000,
            part(name, 2, 3, 2500, 1000, &middle, None),
        ),
        post(
            4,
            r#"[2/2] "Sample.Film.2024.1080p.WEB-DL.H264-TEST.mkv" yEnc (3/3)"#,
            2_000_000,
            part(name, 3, 3, 2500, 2000, &last, Some(file_crc)),
        ),
    ];
    let port = spawn_server(Server::new(posts));
    let config = serde_json::json!({
        "usenet_servers": [{"host": "127.0.0.1", "username": "bob", "password": "secret", "port": port, "ssl": false, "connections": 2, "priority": 1}],
        "groups": [GROUP], "index_mode": "backfill", "request_size": 50,
        "postprocess": {"body_fetches_per_minute": 6000, "settle_secs": 1, "complete_settle_secs": 1, "body_min_size_bytes": 0}
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
    let (text, crc, agent, id): (String, Option<i64>, Option<String>, i64) = loop {
        let conn = db::open_shard(&shard).unwrap();
        let row = conn
            .query_row("select info, crc32, agent, release_id from release_probe", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .ok();
        if let Some(row) = row {
            break row;
        }
        assert!(Instant::now() < deadline, "no probe");
        std::thread::sleep(Duration::from_millis(100));
    };
    stop.store(true, Ordering::Relaxed);
    assert_eq!(runner.join().unwrap(), 0);

    let info: atlas::probe::ProbeInfo = serde_json::from_str(&text).unwrap();
    let yenc = info.yenc.as_ref().unwrap();
    assert_eq!(yenc.name.as_deref(), Some(name));
    assert_eq!((yenc.file_size, yenc.total, yenc.line_len), (Some(2500), Some(3), Some(128)));
    assert_eq!(info.segment_size, Some(1000));
    assert_eq!(info.continuity_ok, Some(true));
    assert_eq!(info.part_size_ok, Some(true));
    assert_eq!(info.part_crc_ok, Some(true), "the first part decodes to its checksum");
    assert_eq!(crc, Some(i64::from(file_crc)), "the last part gives the file's");
    assert_eq!(agent.as_deref(), Some("Nyuu/0.4.2"));
    assert_eq!(info.crossposted_to, ["alt.binaries.test", "alt.binaries.other"]);
    assert_eq!(info.health.get("first"), Some(&true));
    assert_eq!(info.health.get("middle"), Some(&true));
    assert_eq!(info.health.get("last"), Some(&true));
    let media = info.media.as_ref().unwrap();
    assert_eq!((media.video[0].width, media.video[0].height), (Some(1920), Some(1080)));
    assert_eq!(info.imdb_ids, ["tt0133093"]);
    assert!(info.nfo.as_deref().unwrap().contains("letmein"));
    let conn = db::open_shard(&shard).unwrap();
    let password: String =
        conn.query_row("select password from release_nfo_password where release_id = ?", [id], |r| r.get(0)).unwrap();
    assert_eq!(password, "letmein");
}
