//! The probe on a stored RAR set holding a Matroska file whose header runs past the first article: it reads on
//! until the header is whole, finds the chapters, takes the OpenSubtitles hash from the first and last volumes,
//! and the next step asks a fake OpenSubtitles what that hash is.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use atlas::archive::crc32;
use atlas::archive::testkit::{R4_MARK, r4_block, r4_file_fields, r4_main};
use atlas::probe::moviehash::{CHUNK, moviehash};
use atlas::{db, store};
use common::http::{FixtureServer, route};
use common::{Server, post, spawn_server, yenc_encode};

const GROUP: &str = "alt.binaries.test";
const SEGMENT: usize = 80_000;
const INNER: &str = "Some.Film.2001.mkv";
const INNER_SIZE: usize = 300_000;
const IN_FIRST: usize = 200_000;

fn el(id: &[u8], body: &[u8]) -> Vec<u8> {
    assert!(body.len() < 0x3fff);
    let mut out = id.to_vec();
    out.extend([0x40 | (body.len() >> 8) as u8, body.len() as u8]);
    out.extend(body);
    out
}

/// An element with an 8 byte size, for big ones.
fn el_big(id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.push(0x01);
    out.extend(&(body.len() as u64).to_be_bytes()[1..]);
    out.extend(body);
    out
}

/// A film: tracks, then 90 KB of padding soo the chapters and the first cluster are past the first article.
fn film() -> Vec<u8> {
    let info = [el(&[0x7B, 0xA9], b"Some Film"), el(&[0x44, 0x89], &(5_400_000f64).to_be_bytes())].concat();
    let video = [el(&[0xB0], &[0x07, 0x80]), el(&[0xBA], &[0x04, 0x38])].concat();
    let track = [el(&[0x83], &[1]), el(&[0x86], b"V_MPEG4/ISO/AVC"), el(&[0xE0], &video)].concat();
    let commentary = [el(&[0x83], &[2]), el(&[0x86], b"A_AC3"), el(&[0x53, 0x6E], b"Commentary")].concat();
    let tracks = el(&[0x16, 0x54, 0xAE, 0x6B], &[el(&[0xAE], &track), el(&[0xAE], &commentary)].concat());
    let atom = |start: u64, name: &[u8]| {
        el(&[0xB6], &[el(&[0x91], &start.to_be_bytes()), el(&[0x80], &el(&[0x85], name))].concat())
    };
    let edition = [atom(0, b"Opening"), atom(600_000_000_000, b"The Heist")].concat();
    let chapters = el(&[0x10, 0x43, 0xA7, 0x70], &el(&[0x45, 0xB9], &edition));
    let head = [el(&[0x15, 0x49, 0xA9, 0x66], &info), tracks, el_big(&[0xEC], &vec![0u8; 90_000]), chapters].concat();
    let ebml = el(&[0x1A, 0x45, 0xDF, 0xA3], &el(&[0x42, 0x82], b"matroska"));
    // a segment of unknown size, as a muxer streaming it writes
    let mut data = [ebml, vec![0x18, 0x53, 0x80, 0x67, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff], head].concat();
    let rest = INNER_SIZE - data.len() - 12;
    let video_bytes: Vec<u8> = (0..rest).map(|i| (i * 31 % 251) as u8).collect();
    data.extend(el_big(&[0x1F, 0x43, 0xB6, 0x75], &video_bytes));
    assert_eq!(data.len(), INNER_SIZE);
    data
}

/// The two volumes of a stored RAR4 set holding `inner`.
fn volumes(inner: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let fields = r4_file_fields(INNER.as_bytes(), inner.len() as u32);
    let (one, two) = inner.split_at(IN_FIRST);
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

/// A volume's posts, `SEGMENT` bytes each.
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
            post(first_number + i as u64, &subject, 2_000_000, body)
        })
        .collect()
}

#[test]
fn a_stored_set_gives_its_films_hash_and_what_it_is() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::set_var("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"));
    }
    let inner = film();
    let expected = moviehash(INNER_SIZE as u64, &inner[..CHUNK], &inner[INNER_SIZE - CHUNK..]).unwrap();
    let (first, last) = volumes(&inner);
    let base = "Some.Film.2001.1080p.BluRay.x264-GRP";
    let mut posts = posts_of(1, 1, 2, &format!("{base}.rar"), &first);
    let next = posts.len() as u64 + 1;
    posts.extend(posts_of(next, 2, 2, &format!("{base}.r00"), &last));
    let port = spawn_server(Server::new(posts));

    let answer = serde_json::json!({"total_count": 1, "data": [{"attributes": {
        "moviehash_match": true, "release": "Some.Film.2001.1080p.BluRay.x264-GRP",
        "feature_details": {"imdb_id": 133093, "tmdb_id": 603, "title": "Some Film", "year": 2001,
            "feature_type": "Movie"}}}]});
    let opensubtitles = FixtureServer::start(vec![route("/subtitles", answer.to_string())]);
    let config = serde_json::json!({
        "usenet_servers": [{"host": "127.0.0.1", "username": "bob", "password": "secret", "port": port, "ssl": false, "connections": 2, "priority": 1}],
        "groups": [GROUP], "index_mode": "backfill", "request_size": 50,
        "postprocess": {"body_fetches_per_minute": 6000, "settle_secs": 1, "complete_settle_secs": 1, "body_min_size_bytes": 0},
        "media": {"tvmaze": {"enabled": false},
            "opensubtitles": {"api_key": "test-key", "base_url": opensubtitles.base}}
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
    let (text, hash, imdb): (String, Option<String>, Option<i64>) = loop {
        let conn = db::open_shard(&shard).unwrap();
        let row = conn
            .query_row(
                "select p.info, p.moviehash, o.imdb_id from release_probe p
                 join release_opensubtitles o using (release_id)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok();
        if let Some(row) = row {
            break row;
        }
        assert!(Instant::now() < deadline, "no probe and match");
        std::thread::sleep(Duration::from_millis(100));
    };
    stop.store(true, Ordering::Relaxed);
    assert_eq!(runner.join().unwrap(), 0);

    let info: atlas::probe::ProbeInfo = serde_json::from_str(&text).unwrap();
    let media = info.media.as_ref().expect("the film inside the set");
    assert_eq!(media.title.as_deref(), Some("Some Film"));
    let names: Vec<_> = media.chapters.iter().map(|c| (c.start_secs, c.name.clone().unwrap())).collect();
    assert_eq!(names, [(0.0, "Opening".to_string()), (600.0, "The Heist".to_string())], "read past the first article");
    assert!(media.audio[0].commentary);
    assert_eq!(hash.as_deref(), Some(expected.as_str()));
    assert_eq!(info.moviehash.as_deref(), Some(expected.as_str()));
    assert_eq!(imdb, Some(133093));

    let asked = opensubtitles.requests();
    assert_eq!(asked, [format!("/subtitles?moviehash={expected}")], "asked once");
    let key = opensubtitles.headers_of(0).into_iter().find(|(k, _)| k.eq_ignore_ascii_case("api-key"));
    assert_eq!(key.map(|(_, v)| v).as_deref(), Some("test-key"));
}
