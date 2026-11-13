//! Fresh native counterparts of the mixed blocked-group regression.
//! Synthetic blocked rows exercise visibility only, never writer acceptance.
use crate as atlas_api;
#[path = "native.rs"]
mod native;
#[path = "nntp.rs"]
mod nntp;
use crate::{
    field_search::Fields,
    indexer::MainConnection,
    parser::{Article, Release},
    pg,
};
use nntp::{GROUP, Server, mock, par2_file_desc, post, spawn_server, yenc_body};
use std::{
    io::Read,
    sync::{Arc, atomic::Ordering::SeqCst},
    time::{Duration, Instant},
};
const BLOCKED: &str = "alt.binaries.pictures.erotica.teen";
const OPEN: &str = "alt.binaries.open";
const CHILD: &str = "indexer::native_blocked_contract::fixture_child";
#[test]
#[ignore = "requires explicit isolated ATLAS_PG_TEST_URL and actual durable NXV/owned provider"]
fn a_blocked_group_is_never_indexed_or_written() {
    native::run_child_on("blocked-pass", CHILD);
}
#[test]
#[ignore = "requires explicit isolated PostgreSQL, native word keeper, and actual public NZB spool admission"]
fn blocked_native_rows_are_not_served_by_fields_search_nzb_or_api() {
    native::run_child_on("blocked-readers", CHILD);
}
#[test]
#[ignore = "internal isolated child; select the retained native blocked parent"]
fn fixture_child() {
    let case = std::env::var("ATLAS_NATIVE_PROVIDER_CASE").expect("select native blocked parent");
    match case.as_str() {
        "blocked-pass" => blocked_pass(),
        "blocked-readers" => blocked_readers(),
        _ => panic!("unknown native blocked case"),
    }
    native::mark_finished(&case);
}
fn release(name: &str, group: &str, obfuscated: bool) -> Release {
    Release {
        name: name.into(),
        group: group.into(),
        poster: "p".into(),
        date: "Fri, 02 Oct 2026 10:11:12 +0000".into(),
        size: 10,
        complete: true,
        is_obfuscated: obfuscated,
        articles: vec![Article {
            filename: Some(format!("{name}.bin")),
            subject: format!("\"{name}.bin\" yEnc (1/1)"),
            message_id: format!("<{name}@blocked.test>"),
            part: Some(1),
            total_parts: Some(1),
            bytes: 10,
            ..Default::default()
        }],
        ..Default::default()
    }
}
fn blocked_pass() {
    let par2 = par2_file_desc("Real.Name.mkv", 100);
    let mut server = Server::new(vec![
        post(1, r#""Unsafe.Thing.par2" yEnc (1/1)"#, 10, yenc_body("Unsafe.Thing.par2", &par2)),
        post(2, r#""Other.Thing.bin" yEnc (1/1)"#, 10, vec![]),
    ]);
    Arc::get_mut(&mut server).unwrap().any_group = true;
    let port = spawn_server(server.clone());
    let mut harness = native::Harness::open(vec![mock(port, "secret", 2, 1)]);
    // A cold pool proves absence of attempts, including unsuccessful HEAD/BODY.
    let pool = crate::nntp::BlockingPool::new(&[mock(port, "secret", 2, 1)]);
    let mut indexer = harness.indexer_on(pool, "backfill");
    indexer.request_size = 50;
    indexer.index_group_with(BLOCKED, &mut |_| panic!("no slice of a blocked group")).expect("blocked pass refusal");
    assert_eq!(server.accepted.load(SeqCst), 0, "no provider connection/command attempt");
    assert_eq!(server.xovers.load(SeqCst), 0, "no headers");
    assert_eq!(server.bodies_sent.load(SeqCst), 0, "no bodies");
    assert_eq!(server.heads_sent.load(SeqCst), 0, "no HEADs");
    assert_eq!(server.articles_sent.load(SeqCst), 0, "no ARTICLEs");
    assert!(server.groups_seen.lock().unwrap().is_empty());
    assert!(harness.cursors().is_empty());
    assert!(harness.release_rows().is_empty());
    assert_eq!(harness.articles(), 0);
    let db = harness.db();
    let MainConnection::OwnedPostgres(owner) = &*db.conn else { panic!("fixture requires original owner") };
    let stats = owner
        .with(|client| pg::release_writer::save(client, &[release("Late", BLOCKED, false)], 1))
        .unwrap_or_else(|_| panic!("native writer blocked admission"));
    assert_eq!(
        (stats.releases, stats.files, stats.parts),
        (0, 0, 0),
        "valid blocked article is refused, not an empty input artifact"
    );
    assert!(harness.release_rows().is_empty());
    assert_eq!(harness.articles(), 0);
    assert!(harness.cursors().is_empty());
    // Positive control arms the same listener, owner and live NXV after the
    // negative assertions; the blocked test never relies on a dead provider.
    for _ in 0..10 {
        indexer.index_group(GROUP).expect("actual open-group control");
        if indexer.is_idle(GROUP) {
            break;
        }
    }
    assert!(server.accepted.load(SeqCst) > 0 && server.xovers.load(SeqCst) > 0);
    assert_eq!(harness.articles(), 2);
    assert!(harness.cursors().iter().all(|cursor| !cursor.key.starts_with(BLOCKED)));
    harness.finish(indexer, 2);
}
struct Ids {
    hidden: i64,
    hidden_obf: i64,
    shown: i64,
    shown_obf: i64,
}
fn seed_visibility_boundary(harness: &native::Harness) -> Ids {
    let db = harness.db();
    let MainConnection::OwnedPostgres(owner) = &*db.conn else { panic!("fixture requires original owner") };
    owner
        .with(|client| {
            let rows = [
                release("Findable.Thing.Hidden", OPEN, false),
                release("a1B2c3D4e5F6g7H8i9J0", OPEN, true),
                release("Findable.Thing.Shown", OPEN, false),
                release("z9Y8x7W6v5U4t3S2r1Q0", OPEN, true),
            ];
            let saved = pg::release_writer::save(client, &rows, 1)?;
            assert_eq!((saved.releases, saved.files, saved.parts), (4, 4, 4));
            let actual = client.query("select id,name from releases order by id limit 5", &[])?;
            assert_eq!(actual.len(), 4);
            let id = |name: &str| {
                actual.iter().find(|row| row.get::<_, String>(1) == name).expect("genuine accepted ID").get::<_, i64>(0)
            };
            let ids = Ids {
                hidden: id(&rows[0].name),
                hidden_obf: id(&rows[1].name),
                shown: id(&rows[2].name),
                shown_obf: id(&rows[3].name),
            };
            // CLOSED test-only legacy/unsupported-row simulation, after genuine
            // bootstrap and accepted saves. Never mint identity/readiness/receipts.
            let mut tx = client.transaction()?;
            pg::guard_native_transaction(&mut tx)?;
            let changed = tx.execute(
                "update releases set group_name=$1 where id=any($2)",
                &[&BLOCKED, &vec![ids.hidden, ids.hidden_obf]],
            )?;
            assert_eq!(changed, 2);
            tx.commit()?;
            Ok(ids)
        })
        .unwrap_or_else(|_| panic!("closed native visibility fixture setup"))
}
fn api(query: &str) -> crate::api::ApiResponse {
    crate::api::handle("/api", &format!("{query}&apikey=k"), "http://atlas:9090", "k")
}
fn body(mut response: crate::api::ApiResponse) -> String {
    if let Some(mut spool) = response.spool.take() {
        let mut xml = String::new();
        spool.read_to_string(&mut xml).expect("actual native API body");
        xml
    } else {
        response.body
    }
}
fn blocked_readers() {
    let mut harness = native::Harness::open(vec![]);
    let indexer = harness.indexer_on(crate::nntp::BlockingPool::new(&[]), "backfill");
    let ids = seed_visibility_boundary(&harness);
    harness.start_words();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(page) = harness.with_reader(|view| view.api_query("findable", None, None, 0, 10)) {
            if page.rows.iter().map(|row| row.id).collect::<Vec<_>>() == [ids.shown] {
                assert_eq!(page.total, Some(1));
                break;
            }
        }
        assert!(Instant::now() < deadline, "real native keeper did not publish expected visible row");
        std::thread::sleep(Duration::from_millis(20));
    }
    harness
        .with_reader(|view| {
            assert_eq!(view.count()?, 4, "reader filtering never deletes physical rows");
            assert_eq!(view.visible_count()?, 1);
            assert_eq!(view.recent(0, 10)?.iter().map(|row| row.id).collect::<Vec<_>>(), [ids.shown]);
            assert_eq!(
                view.rows(&[ids.hidden, ids.hidden_obf, ids.shown, ids.shown_obf])?
                    .iter()
                    .map(|row| row.id)
                    .collect::<Vec<_>>(),
                [ids.shown]
            );
            for id in [ids.hidden, ids.hidden_obf, ids.shown_obf] {
                assert!(!view.visible(id)?);
                assert!(view.release(id)?.is_none());
                assert!(view.nzb_listed_files(id)?.is_none());
            }
            for words in ["findable", ""] {
                let (rows, count) =
                    view.field_search(&Fields { group: BLOCKED.into(), ..Default::default() }, words, None, 0, 10)?;
                assert!(rows.is_empty());
                assert_eq!(count, 0, "blocked scoped search/recent counts remain zero");
            }
            let (rows, count) =
                view.field_search(&Fields { group: OPEN.into(), ..Default::default() }, "findable", None, 0, 10)?;
            assert_eq!(rows.iter().map(|row| row.id).collect::<Vec<_>>(), [ids.shown]);
            assert_eq!(count, 1);
            let (rows, count) = view.field_search(&Fields::default(), "Hidden\"(", None, 0, 10)?;
            assert!(rows.is_empty());
            assert_eq!(count, 0, "literal malformed-FTS input remains a native field negative");
            Ok(())
        })
        .expect("actual native blocked reader oracles");
    for hidden in [ids.hidden, ids.hidden_obf, ids.shown_obf] {
        assert!(crate::nzb::build_nzb(hidden).expect("public native blocked NZB").is_none());
        assert!(crate::nzb::generate_nzb(hidden, Some(&crate::paths::app_dir())).is_err());
        assert_eq!(api(&format!("t=get&id={hidden}")).status, 404);
        let files = api(&format!("t=files&id={hidden}"));
        assert_eq!(files.status, 200);
        let error = body(files);
        assert!(
            error.contains("code=\"300\"") && !error.contains("<segment"),
            "blocked file listing is an explicit missing-item response"
        );
    }
    let xml = crate::nzb::build_nzb(ids.shown).expect("public native shown NZB").expect("shown own payload");
    assert!(xml.contains("Findable.Thing.Shown") && !xml.contains("Findable.Thing.Hidden"));
    for query in ["t=search&q=findable", "t=search"] {
        let response = api(query);
        assert_eq!(response.status, 200);
        let xml = body(response);
        assert_eq!(xml.matches("<item>").count(), 1);
        assert!(xml.contains("Findable.Thing.Shown") && !xml.contains("Findable.Thing.Hidden"));
        assert!(!xml.contains("a1B2c3D4") && !xml.contains("z9Y8x7W"));
    }
    let shown = api(&format!("t=get&id={}", ids.shown));
    assert_eq!(shown.status, 200);
    assert!(body(shown).contains("Findable.Thing.Shown"));
    assert!(
        !std::fs::read_dir(crate::paths::app_dir()).unwrap().any(|entry| entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|ext| ext == "nzb")),
        "blocked export never publishes a file"
    );
    let db = harness.db();
    let MainConnection::OwnedPostgres(owner) = &*db.conn else { panic!("original owner required") };
    let refused = owner
        .with(|client| pg::release_writer::save(client, &[release("Late", BLOCKED, false)], 2))
        .unwrap_or_else(|_| panic!("actual writer blocked replay refusal"));
    assert_eq!((refused.releases, refused.files, refused.parts), (0, 0, 0));
    assert_eq!(
        harness.with_reader(|view| view.count()).unwrap(),
        4,
        "no implicit purge and no accepted blocked append"
    );
    harness.finish(indexer, 0);
}
