//! The tail caller must consume already-decoded Tags before needing fetch layout.
use super::*;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::{Duration, Instant};

fn element(id: &[u8], body: &[u8]) -> Vec<u8> {
    assert!(body.len() < 0x3fff);
    [id, &[0x40 | (body.len() >> 8) as u8, body.len() as u8], body].concat()
}

fn simple_tag(name: &str, value: &str) -> Vec<u8> {
    element(&[0x67, 0xc8], &[
        element(&[0x45, 0xa3], name.as_bytes()),
        element(&[0x44, 0x87], value.as_bytes()),
    ].concat())
}

fn tag(uid: Option<u64>, values: &[(&str, &str)]) -> Vec<u8> {
    let uid = uid.map(|uid| element(&[0x63, 0xc5], &uid.to_be_bytes())).unwrap_or_default();
    let mut body = element(&[0x63, 0xc0], &uid);
    for (name, value) in values {
        body.extend(simple_tag(name, value));
    }
    element(&[0x73, 0x73], &body)
}

/// A fixed-width SeekPosition keeps the independently counted layout stable.
fn matroska(seek_override: Option<u64>) -> (Vec<u8>, usize) {
    let seek = |position: u64| element(&[0x11, 0x4d, 0x9b, 0x74], &element(&[0x4d, 0xbb], &[
        element(&[0x53, 0xab], &[0x12, 0x54, 0xc3, 0x67]),
        element(&[0x53, 0xac], &position.to_be_bytes()),
    ].concat()));
    let info = element(&[0x15, 0x49, 0xa9, 0x66], &element(&[0x57, 0x41], b"fixture"));
    let track = element(&[0xae], &[
        element(&[0xd7], &[1]), element(&[0x83], &[1]),
        element(&[0x73, 0xc5], &42u64.to_be_bytes()), element(&[0x86], b"V_VP9"),
    ].concat());
    let tracks = element(&[0x16, 0x54, 0xae, 0x6b], &track);
    let cluster = element(&[0x1f, 0x43, 0xb6, 0x75], &[
        element(&[0xe7], &[0]), element(&[0xec], &[0; 512]),
    ].concat());
    let tags = element(&[0x12, 0x54, 0xc3, 0x67], &[
        tag(Some(42), &[("BPS", "8000"), ("NUMBER_OF_FRAMES", "50"),
            ("NUMBER_OF_BYTES", "2000"), ("DURATION", "00:00:02.000000000")]),
        tag(None, &[("IMDB", "tt0133093")]),
    ].concat());
    let position = seek(0).len() + info.len() + tracks.len() + cluster.len();
    let body = [seek(seek_override.unwrap_or(position as u64)), info, tracks, cluster, tags].concat();
    let ebml = element(&[0x1a, 0x45, 0xdf, 0xa3], &element(&[0x42, 0x82], b"matroska"));
    let tags_at = ebml.len() + 6 + position;
    ([ebml, element(&[0x18, 0x53, 0x80, 0x67], &body)].concat(), tags_at)
}

fn inspected_article(body: &[u8], full_size: usize, multipart: bool) -> ProbeInfo {
    let mut encoded = Vec::new();
    for byte in body {
        let byte = byte.wrapping_add(42);
        if matches!(byte, 0 | b'\n' | b'\r' | b'=') {
            encoded.extend([b'=', byte.wrapping_add(64)]);
        } else {
            encoded.push(byte);
        }
    }
    let part = if multipart { "part=1 total=2 " } else { "" };
    let mut lines = vec![format!("=ybegin {part}line=128 size={full_size} name=fixture.mkv").into_bytes()];
    if multipart {
        lines.push(format!("=ypart begin=1 end={}", body.len()).into_bytes());
    }
    lines.extend([encoded, format!("=yend size={}", body.len()).into_bytes()]);
    let decoded = yenc_decode(&lines).expect("owned yEnc fixture decodes");
    assert_eq!(decoded, body);
    let mut info = ProbeInfo { yenc: yenc::parse(&lines), ..Default::default() };
    derive(&mut info, 0, Some(if multipart { 2 } else { 1 }), None);
    inspect(&decoded, None, &mut info);
    let media = info.media.as_ref().expect("fixture Matroska header");
    assert!(media.header_complete);
    assert_eq!(media.track_uids, [(42, 1, 0)]);
    assert!(media.video[0].stats.is_none(), "initial parser stops at the first Cluster");
    info
}

fn with_no_fetch<T>(parts: i64, run: impl FnOnce(&mut StepCtx<'_>, &FileInfo) -> T) -> T {
    let schema = crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
    let header_dir = tempfile::tempdir().unwrap();
    let mut config = crate::config::Config::default();
    config.postprocess.enabled = false;
    config.predb.enabled = false;
    config.spots.enabled = false;
    config.inferred_parts = false;
    config.media.tvmaze.enabled = false;
    config.media.allocine.enabled = false;
    config.media.details_per_minute = 0;
    let storage = crate::pg::StorageConfig {
        backend: crate::pg::Backend::Postgres,
        url: schema.url.clone(),
        schema: schema.name.clone(),
        max_connections: 1,
        allow_plaintext: true,
    };
    let headers = crate::config::HeaderStoreConfig { enabled: true, dir: header_dir.path().to_str().unwrap().into() };
    let native = crate::pg::indexer_runtime::Runtime::open(&config, storage, headers, &[], &mut None).unwrap();
    let id = native
        .owner
        .with(|client| {
            // The buffered bytes are already owned by the test. The catalog holds
            // an actual later article so a missing Tags part cannot masquerade as
            // successful no-fetch behavior through an empty segment inventory.
            crate::pg::release_writer::save(
                client,
                &[crate::parser::Release {
                    name: "Buffered.Tags.Fixture".into(),
                    group: "alt.binaries.fixture".into(),
                    articles: vec![crate::parser::Article {
                        filename: Some("fixture.mkv".into()),
                        message_id: "<needed>".into(),
                        part: Some(2),
                        total_parts: Some(parts.max(2)),
                        file_total: Some(1),
                        bytes: 100,
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                1,
            )?;
            Ok(client.query_one("select id from releases", &[])?.get::<_, i64>(0))
        })
        .unwrap();
    let reads = crate::pg::stage_reads::StepReads::for_owner(&native.owner).unwrap();
    let data = crate::postprocess::data::StepData::Native(&reads);
    let mut files = data.release_files(id).unwrap();
    assert_eq!(files.len(), 1);
    let file = files.remove(0);
    assert_eq!(file.filename, "fixture.mkv");
    assert_eq!(file.expected, Some(parts.max(2)));
    assert_eq!(file.parts_seen, 1);
    let needed = data.file_segments(file.id, &SegmentPick::Parts(vec![2])).unwrap();
    assert_eq!(needed.len(), 1);
    assert_eq!((needed[0].message_id.as_str(), needed[0].part, needed[0].bytes), ("<needed>", Some(2), 100));
    assert!(data.file_segments(file.id, &SegmentPick::Parts(vec![3])).unwrap().is_empty());
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let pool = Arc::new(crate::nntp::Pool::new(&[]));
    let stats = Arc::new(crate::postprocess::stats::Stats::default());
    let bodies = crate::postprocess::bodies::BodyFetcher::new(
        pool.clone(),
        runtime.handle().clone(),
        Arc::new(AtomicBool::new(false)),
        crate::config::PostprocessConfig::default(),
        stats.clone(),
    );
    let mut ctx = StepCtx {
        release: ReleaseInfo { id, group: "alt.binaries.fixture".into(), ..Default::default() },
        data,
        facts: Vec::new(),
        bodies: &bodies,
        used_bodies: 0,
        deadline: Instant::now() + Duration::from_secs(5),
        lane_can_fetch: false,
        crcs: Vec::new(),
        pictures: false,
    };
    let result = run(&mut ctx, &file);
    assert_eq!(ctx.used_bodies, 0);
    assert!(ctx.crcs.is_empty());
    assert_eq!(stats.bodies.load(Ordering::Relaxed), 0);
    assert!(!pool.is_connected());
    native.owner.close();
    result
}

fn assert_tail_statistics(info: &ProbeInfo) {
    let media = info.media.as_ref().unwrap();
    assert_eq!(media.video[0].stats.as_ref().map(|s| (s.bps, s.frames, s.bytes)),
        Some((Some(8000), Some(50), Some(2000))), "buffered tail statistics must reach the track");
    assert_eq!(media.video[0].stats.as_ref().unwrap().duration_secs, Some(2.0));
    assert_eq!(media.tags.get("IMDB").map(String::as_str), Some("tt0133093"));
}

#[test]
#[ignore = "requires explicit isolated PostgreSQL for native buffered Tags inventory"]
fn complete_buffered_tags_without_ypart_are_read_without_fetch() {
    let (front, tags_at) = matroska(None);
    let mut info = inspected_article(&front, front.len(), false);
    assert_eq!(info.yenc.as_ref().unwrap().begin, None);
    assert_eq!(info.segment_size, None);
    assert_eq!(media::matroska_tags_past(&front), [tags_at as u64]);
    let whole = with_no_fetch(1, |ctx, file| read_tail_tags(ctx, file, &front, None, &mut info).unwrap());
    assert_tail_statistics(&info);
    assert!(whole);
}

#[test]
#[ignore = "requires explicit isolated PostgreSQL for native buffered Tags inventory"]
fn incomplete_buffered_tags_without_layout_are_not_reported_whole() {
    let (whole, tags_at) = matroska(None);
    let front = &whole[..tags_at + 5];
    let mut info = inspected_article(front, whole.len(), false);
    let read = with_no_fetch(1, |ctx, file| read_tail_tags(ctx, file, front, None, &mut info).unwrap());
    assert!(!read);
    assert!(info.media.as_ref().unwrap().video[0].stats.is_none());
}

#[test]
#[ignore = "requires explicit isolated PostgreSQL for native buffered Tags inventory"]
fn multipart_tags_reuse_the_known_last_part_without_fetch() {
    let (whole, tags_at) = matroska(None);
    let front = &whole[..tags_at];
    let mut info = inspected_article(front, whole.len(), true);
    let tail = whole[tags_at..].to_vec();
    assert!(tail.len() <= tags_at);
    let lines = yenc::YencLines { begin: Some(tags_at as u64 + 1), ..Default::default() };
    let known = last_part(&info, Some(&lines), Some(tail)).unwrap();
    assert_eq!(known.0, 2);
    let read = with_no_fetch(2, |ctx, file| read_tail_tags(ctx, file, front, Some(&known), &mut info).unwrap());
    assert!(read);
    assert_tail_statistics(&info);
}

#[test]
#[ignore = "requires explicit isolated PostgreSQL for native buffered Tags inventory"]
fn multipart_missing_tags_still_require_a_fetch() {
    let (whole, tags_at) = matroska(None);
    let front = &whole[..tags_at];
    let mut info = inspected_article(front, whole.len(), true);
    let error = with_no_fetch(2, |ctx, file| read_tail_tags(ctx, file, front, None, &mut info).unwrap_err());
    assert!(matches!(error, StepError::Permanent(_)), "needed part must reach the closed fetch lane");
    assert!(info.media.as_ref().unwrap().video[0].stats.is_none());
}

#[test]
#[ignore = "requires explicit isolated PostgreSQL for native buffered Tags inventory"]
fn invalid_seek_and_declared_file_bounds_do_not_parse_or_fetch() {
    for position in [0, u64::MAX] {
        let (front, _) = matroska(Some(position));
        let mut info = inspected_article(&front, front.len(), true);
        assert!(with_no_fetch(2, |ctx, file| read_tail_tags(ctx, file, &front, None, &mut info).unwrap()));
        assert!(info.media.as_ref().unwrap().video[0].stats.is_none());
    }
    let (front, tags_at) = matroska(None);
    let mut info = inspected_article(&front, tags_at, true);
    assert!(with_no_fetch(2, |ctx, file| read_tail_tags(ctx, file, &front, None, &mut info).unwrap()));
    assert!(info.media.as_ref().unwrap().video[0].stats.is_none(), "declared file ends before Tags");
}
