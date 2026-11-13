//! One post remains one native release across slices and independently numbered
//! providers. Identical album filenames from other posters remain separate.
use crate as atlas_api;

#[path = "native.rs"]
mod native;
#[path = "nntp.rs"]
mod nntp;

use crate::indexer::{PassSettings, run_pass_on};
use crate::pg::fresh_nzb::Pick;
use native::Harness;
use nntp::{GROUP, Post, Server, mock, post, spawn_server};
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

const CASE: &str = "collection-paths";
const CHILD: &str = "indexer::native_collection_paths::fixture_child";
const POSTER_A: &str = "a <a@x>";
const POSTER_B: &str = "b <b@x>";

#[test]
#[ignore = "requires explicit isolated ATLAS_PG_TEST_URL and original native owner/live NXV"]
fn a_post_is_one_release_over_slices_and_servers() {
    native::run_child_on(CASE, CHILD);
}

#[test]
#[ignore = "internal isolated child; select the retained collection-path parent test"]
fn fixture_child() {
    assert_eq!(std::env::var("ATLAS_NATIVE_PROVIDER_CASE").expect("select the collection fixture"), CASE);
    collection_paths();
    native::mark_finished(CASE);
}

fn album(first: u64, poster: &str) -> Vec<Post> {
    (1..=50u64)
        .map(|i| Post {
            author: poster.into(),
            // The providers keep the same articles under their own numbering.
            message_id: format!("<{i}.{}@x>", &poster[..1]),
            ..post(first + i, &format!(r#"[{i:02}/50] "DSC{i:04}.jpg" yEnc (1/1)"#), 10, vec![])
        })
        .collect()
}

fn expected_album(poster: &str) -> BTreeMap<String, String> {
    (1..=50u64).map(|i| (format!("DSC{i:04}.jpg"), format!("{i}.{}@x", &poster[..1]))).collect()
}

/// A real selected-server pass owns its original native Db. Fresh per-provider
/// RunStates avoid borrowing a phase from the provider whose cursors are done.
fn index_on(harness: &Harness, indexer: &crate::indexer::Indexer, server: usize, slice: u64) {
    let ctx = harness.context(indexer.client.pool.clone());
    let db = harness.db();
    let settings = PassSettings {
        mode: "backfill".into(),
        batch_size: 10_000,
        request_size: slice,
        split_min_backlog: i64::MAX,
        ..Default::default()
    };
    indexer.client.block_on(async {
        for _ in 0..20 {
            let mut ran_on = usize::MAX;
            run_pass_on(&ctx, &settings, &db, GROUP, server, &mut |_| {}, &mut ran_on).await.unwrap();
            assert_eq!(ran_on, server, "the selected provider owns these article numbers");
            if ctx.states.is_idle(GROUP) {
                return;
            }
        }
        panic!("selected provider never reached its actual backfill cursor");
    });
    assert!(ctx.states.is_idle(GROUP));
}

fn own_nzb(harness: &Harness, id: i64) -> String {
    let root = tempfile::tempdir().unwrap();
    let spool = crate::nzb_spool::Pool::new(
        root.path().into(),
        crate::nzb_spool::Limits { requests: 5, per_request_bytes: 1 << 20, total_bytes: 5 << 20, free_bytes: 0 },
    )
    .unwrap();
    harness
        .with_reader(|view| {
            let listed = view.nzb_listed_files(id)?.expect("native release file inventory");
            let built = view.nzb_owned_build(id, Pick::Whole, &spool)?.expect("real native complete NZB");
            assert!(built.complete(), "all native file counters and parts must be whole");
            assert_eq!(listed.len(), built.xml.matches("<file ").count());
            Ok(built.xml)
        })
        .unwrap()
}

/// Read the real native XML back through the production NZB parser. Literal
/// expected names/MIDs keep the assertion independent of the ingestion result.
fn assert_nzb(xml: &str, expected: &BTreeMap<String, String>, poster: &str) {
    let parsed = crate::archive::nzb::parse(xml.as_bytes());
    assert!(parsed.complete && !parsed.damaged);
    assert_eq!(parsed.left_out, 0);
    assert_eq!(parsed.files.len(), expected.len());
    let mut actual = BTreeMap::new();
    for file in parsed.files {
        assert_eq!(file.poster, poster);
        assert_eq!(file.groups, [GROUP]);
        assert_eq!((file.articles, file.segments.len(), file.total_bytes), (1, 1, 10));
        let segment = &file.segments[0];
        assert_eq!((segment.number, segment.bytes), (1, 10));
        assert!(actual.insert(file.name, segment.message_id.clone()).is_none(), "each filename appears once");
    }
    assert_eq!(&actual, expected, "every file keeps its own article ID");
}

fn collection_paths() {
    let mut on_a = album(0, POSTER_A);
    on_a.truncate(25);
    let mut on_b = album(1000, POSTER_A);
    on_b.extend(album(2000, POSTER_B));
    on_b.push(post(3000, r#""Lone.Picture.jpg" yEnc (1/1)"#, 10, vec![]));
    let a = Server::new(on_a);
    let b = Server::new(on_b);
    let ports = [spawn_server(a.clone()), spawn_server(b.clone())];
    let configs = vec![mock(ports[0], "secret", 2, 1), mock(ports[1], "secret", 2, 1)];
    let hosts = crate::nntp::server_keys(&configs);
    assert_ne!(hosts[0], hosts[1], "configured providers have independent numbering identities");
    let keys = hosts.iter().map(|host| format!("{GROUP}@{}", host.to_lowercase())).collect::<Vec<_>>();
    let mut harness = Harness::open(configs);
    let indexer = harness.indexer("backfill");

    // Seven-article slices initially contribute half of A's fifty-file album.
    index_on(&harness, &indexer, 0, 7);
    let first = harness.release_rows();
    assert_eq!(first.len(), 1);
    assert_eq!(
        (first[0].poster.as_str(), first[0].parts, first[0].files, first[0].file_total),
        (POSTER_A, 25, 25, Some(50))
    );
    assert!(!first[0].complete);
    assert_eq!(harness.articles(), 25);
    let first_id = first[0].id;
    let frontiers = harness.cursors();
    assert_eq!(frontiers.len(), 1);
    assert_eq!(
        (frontiers[0].key.as_str(), frontiers[0].live, frontiers[0].backfill),
        (keys[0].as_str(), Some(25), Some(0))
    );

    // Eleven-article slices from B replay A's first half and contribute its
    // second half, B's same-named album, and the independent lone picture.
    // Both provider cursor rows remain real; no cursor deletion/reset occurs.
    index_on(&harness, &indexer, 1, 11);
    let rows = harness.release_rows();
    assert_eq!(rows.len(), 3, "two posters' albums and the counter-less picture");
    let albums = rows.iter().filter(|row| row.file_total == Some(50)).collect::<Vec<_>>();
    assert_eq!(albums.len(), 2);
    let album_a = albums.iter().find(|row| row.poster == POSTER_A).unwrap();
    let album_b = albums.iter().find(|row| row.poster == POSTER_B).unwrap();
    assert_eq!(album_a.id, first_id, "the original native collection ID continues across providers");
    assert_ne!(album_a.id, album_b.id, "same filenames and counters cannot merge different short-name posters");
    for album in &albums {
        assert_eq!((album.parts, album.files, album.file_total, album.complete), (50, 50, Some(50), true));
    }
    let lone = rows.iter().find(|row| row.name == "Lone.Picture.jpg").unwrap();
    assert_eq!((lone.parts, lone.files, lone.complete), (1, 1, true));
    assert_eq!(harness.articles(), 101, "the repeated twenty-five MIDs are accepted only once");
    let frontiers = harness.cursors();
    assert_eq!(frontiers.len(), 2);
    let cursor_a = frontiers.iter().find(|row| row.key == keys[0]).unwrap();
    let cursor_b = frontiers.iter().find(|row| row.key == keys[1]).unwrap();
    assert_eq!((cursor_a.live, cursor_a.backfill), (Some(25), Some(0)), "B cannot advance A's numeric cursors");
    assert_eq!((cursor_b.live, cursor_b.backfill), (Some(3000), Some(1000)));
    assert!(frontiers.iter().all(|row| row.key != GROUP), "multi-provider passes keep their actual provider keys");
    assert!(
        a.xovers.load(Ordering::SeqCst) > 1 && b.xovers.load(Ordering::SeqCst) > 1,
        "both slice widths perform real overview reads"
    );

    assert_nzb(&own_nzb(&harness, album_a.id), &expected_album(POSTER_A), POSTER_A);
    assert_nzb(&own_nzb(&harness, album_b.id), &expected_album(POSTER_B), POSTER_B);
    assert_nzb(
        &own_nzb(&harness, lone.id),
        &BTreeMap::from([("Lone.Picture.jpg".into(), "msg3000@mock".into())]),
        "poster <p@mock>",
    );
    harness.finish(indexer, 101);
}
