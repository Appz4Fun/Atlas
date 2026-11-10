//! Stills from the middle of a release: an HEVC Matroska file posted as itself, and the same file in a stored
//! RAR set, each get a keyframe at a quarter, half and three quarters, read from articles there and decoded by
//! our own decoder into JPEGs. A keyframe cut across two volumes is read on past the second's headers.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use atlas::archive::crc32;
use atlas::archive::testkit::{R4_MARK, r4_block, r4_file_fields, r4_main};
use atlas::video::mkv::{self, Found};
use atlas::{db, store};
use common::{Server, post, spawn_server, yenc_encode};

const GROUP: &str = "alt.binaries.test";
const SEGMENT: usize = 20_000;
/// what each article claims to be, soo most releases look like full size films
const CLAIMED: u64 = 12_000_000;

fn clip(codec: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/{codec}/clip.mkv", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Keep the picture data while giving a posting its own mux identity, so still sharing cannot skip extraction.
fn distinct_mux(mut bytes: Vec<u8>, nonce: u8) -> Vec<u8> {
    let uid = bytes.windows(3).position(|b| b == [0x73, 0xa4, 0x90]).unwrap() + 3;
    bytes[uid] ^= nonce;
    // This fixture's Info has a one-byte length and a leading CRC-32 element. Repair it after changing the UID.
    let info = bytes.windows(4).rposition(|b| b == [0x15, 0x49, 0xa9, 0x66]).unwrap();
    assert_ne!(bytes[info + 4] & 0x80, 0);
    let start = info + 5;
    let end = start + usize::from(bytes[info + 4] & 0x7f);
    assert_eq!(&bytes[start..start + 2], &[0xbf, 0x84]);
    assert!(uid >= start + 6 && uid + 16 <= end);
    let checksum = crc32(&bytes[start + 6..end]);
    bytes[start + 2..start + 6].copy_from_slice(&checksum.to_le_bytes());
    bytes
}

/// The two volumes of a stored RAR4 set holding `inner` as `name`, the first with `split` bytes of it.
fn volumes(name: &str, inner: &[u8], split: usize) -> (Vec<u8>, Vec<u8>) {
    let fields = r4_file_fields(name.as_bytes(), inner.len() as u32);
    let (one, two) = inner.split_at(split);
    // a volume ends with an end-of-archive block that says another follows
    let first = [
        R4_MARK.to_vec(),
        r4_main(0x0001),
        r4_block(0x74, 0x0002, &fields, Some(one.len() as u32)),
        one.to_vec(),
        r4_block(0x7b, 0x4001, &[], None),
    ]
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

/// Where to split `inner` across two volumes so that one still's keyframe runs from the first into the second:
/// three bytes before the end of a keyframe that's the first one from where some position's article starts, so the
/// first volume's end-of-archive block is longer than what's missing.
fn split_in_a_keyframe(name: &str, inner: &[u8]) -> usize {
    let track = mkv::video_track(inner).unwrap();
    let mut keyframes = Vec::new();
    let mut from = 0;
    while let Found::Frame { start, len, .. } = mkv::find_keyframe(&inner[from..], track.number) {
        keyframes.push((from + start, len));
        from += start + len;
    }
    for &(at, len) in &keyframes {
        let split = at + len - 3;
        // the first volume is the biggest, as in a real set
        if split < inner.len() / 2 + 64 {
            continue;
        }
        let (first, last) = volumes(name, inner, split);
        let header = first.len() - split - r4_block(0x7b, 0x4001, &[], None).len();
        let (a1, a2) = (first.len().div_ceil(SEGMENT), last.len().div_ceil(SEGMENT));
        for percent in [25, 50, 75] {
            let part = (a1 + a2) * percent / 100;
            let Some(off) = (part * SEGMENT).checked_sub(header).filter(|_| part < a1) else { continue };
            if let Found::Frame { start, .. } = mkv::find_keyframe(&inner[off..], track.number)
                && off + start == at
            {
                return split;
            }
        }
    }
    panic!("no keyframe to split in")
}

/// A file's posts, `SEGMENT` bytes each, each claiming `claimed` bytes.
fn posts_of(first_number: u64, index: usize, count: usize, name: &str, data: &[u8], claimed: u64) -> Vec<common::Post> {
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
            post(first_number + i as u64, &subject, claimed, body)
        })
        .collect()
}

#[test]
fn video_releases_get_three_stills_from_their_middles() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::set_var("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"));
    }
    let inner = clip("hevc");
    let plain = "Plain.Film.2001.1080p.WEB-DL.x265-GRP";
    let mut posts = posts_of(1, 1, 1, &format!("{plain}.mkv"), &distinct_mux(inner.clone(), 1), CLAIMED);
    let avc = "Avc.Film.2003.720p.HDTV.x264-GRP";
    let next = posts.len() as u64 + 1;
    // a small release, a few hundred KB, gets stills too
    posts.extend(posts_of(next, 1, 1, &format!("{avc}.mkv"), &clip("h264"), SEGMENT as u64 + 200));
    let set = "Stored.Film.2002.1080p.BluRay.x265-GRP";
    let (first, last) = volumes(&format!("{set}.mkv"), &inner, inner.len() / 2);
    let next = posts.len() as u64 + 1;
    posts.extend(posts_of(next, 1, 2, &format!("{set}.rar"), &first, CLAIMED));
    let next = posts.len() as u64 + 1;
    posts.extend(posts_of(next, 2, 2, &format!("{set}.r00"), &last, CLAIMED));
    // a still whose keyframe runs on into the next volume
    let split_set = "Split.Film.2003.1080p.BluRay.x265-GRP";
    let split_inner = distinct_mux(inner.clone(), 2);
    let at = split_in_a_keyframe(&format!("{split_set}.mkv"), &split_inner);
    let (first, last) = volumes(&format!("{split_set}.mkv"), &split_inner, at);
    let next = posts.len() as u64 + 1;
    posts.extend(posts_of(next, 1, 2, &format!("{split_set}.rar"), &first, CLAIMED));
    let next = posts.len() as u64 + 1;
    posts.extend(posts_of(next, 2, 2, &format!("{split_set}.r00"), &last, CLAIMED));
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
        if rows.len() == 12 {
            break rows;
        }
        if Instant::now() >= deadline {
            let names: Vec<(String, Option<String>)> = conn
                .prepare("select r.name, p.info from releases r left join release_probe p on p.release_id = r.id")
                .and_then(|mut q| q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
                .unwrap_or_default();
            panic!("stills so far: {rows:?}\nreleases: {names:?}");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    stop.store(true, Ordering::Relaxed);
    assert_eq!(runner.join().unwrap(), 0);

    for name in [plain, set, split_set, avc] {
        let mine: Vec<_> = rows.iter().filter(|r| r.0.starts_with(&name[..10])).collect();
        assert_eq!(mine.iter().map(|r| r.1).collect::<Vec<_>>(), [25, 50, 75], "{name}: {rows:?}");
        assert!(mine.iter().all(|r| (r.3, r.4) == (320, 180)));
        // later in the file, later in the film: keyframes a second apart over 12 s
        let times: Vec<i64> = mine.iter().map(|r| r.2).collect();
        assert!(times.windows(2).all(|w| w[0] < w[1]) && times[0] >= 1000 && times[2] <= 11_000, "{times:?}");
    }
    // the three postings of one clip show the same pictures: the same film, each to the others
    let conn = db::open().unwrap();
    let id_of = |name: &str| -> i64 {
        db::open_shard(&shard)
            .unwrap()
            .query_row("select id from releases where name like ?1", [format!("{}%", &name[..10])], |r| r.get(0))
            .unwrap()
    };
    // (the H.264 clip is the same footage encoded again; the clip moves fast, soo a keyframe a second off is
    // sometimes another picture)
    let same = atlas::probe::stills::same_film(&conn, id_of(set));
    for other in [plain, split_set, avc] {
        assert!(same.contains(&id_of(other)), "{other}: {same:?}");
    }
    assert!(!same.contains(&id_of(set)), "not itself");
    // the JPEGs are kept by their hash
    let stills: Vec<_> = std::fs::read_dir(home.path().join("stills")).unwrap().filter_map(Result::ok).collect();
    assert!(!stills.is_empty());
    for f in stills {
        assert_eq!(&std::fs::read(f.path()).unwrap()[..2], &[0xff, 0xd8]);
    }
}
