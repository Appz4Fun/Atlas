//! Titles that arrive after their release, through indexes only.

mod common;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use atlas::archive::store::{INSERT_FILE, SOURCE_RAR};
use atlas::config::{CategoryConfig, Config, PostprocessConfig, PredbConfig};
use atlas::indexer::shared_db;
use atlas::nntp::Pool;
use atlas::parser::Release;
use atlas::postprocess::step::mask;
use atlas::postprocess::{Stage, StageParams, steps};
use atlas::predb::db::{create, path_for};
use atlas::predb::import::{Entry, import};
use atlas::predb::keys::{md5_hex, name_key, sha1_hex};
use atlas::predb::late::*;
use atlas::predb::step::PREDB_MATCH;
use atlas::{db, store};
use common::stage::*;
use rusqlite::{Connection, params};

const T: &str = "Alpha.Beta.Gamma.2024.1080p.WEB-DL.H.264-TESTGRP";

fn shards(h: &Harness) -> Vec<Connection> {
    store::shard_paths(&h.main).iter().map(|p| db::open_shard(p).unwrap()).collect()
}

fn nominated(h: &Harness, title: &str) -> Nominees {
    nominate(&shards(h), title, &Instant::now, &AtomicBool::new(false)).unwrap()
}

fn found_by_name(name: &str) {
    let h = fresh();
    h.save(&[release_of(name, "x.bin", 2, 2)]);
    let id = h.id_of(name);
    let n = nominated(&h, T);
    assert_eq!(n.ids, [id], "{name}");
}

#[test]
fn each_route_nominates() {
    let (md5, sha1) = (md5_hex(T), sha1_hex(T));
    let mut names = vec![
        T.to_string(),
        format!("{T}.mkv"),
        format!("{}.mkv", T.to_lowercase()),
        format!("{T} [12345678]"),
        format!("{T}.mkv [12345678]"),
    ];
    for hash in [md5.clone(), md5.to_uppercase(), sha1.clone(), sha1.to_uppercase()] {
        names.push(hash.clone());
        names.push(format!("{hash}.mkv"));
        names.push(format!("{hash} [12345678]"));
    }
    for name in &names {
        found_by_name(name);
    }

    let h = fresh();
    h.save(&[Release { display_name: Some(T.into()), ..release_of("Xk3Mz9PqL", "x.bin", 2, 2) }]);
    assert_eq!(nominated(&h, T).ids, [h.id_of("Xk3Mz9PqL")]);

    let h = fresh();
    h.save(&[release_of("Xk3Mz9PqL", "x.bin", 2, 2)]);
    let id = h.id_of("Xk3Mz9PqL");
    h.shard().execute("insert into predb_inner_key (key, release_id) values (?, ?)", params![name_key(T), id]).unwrap();
    assert_eq!(nominated(&h, T).ids, [id]);
}

fn with_title(h: &Harness, title: &str, added_at: i64) -> Connection {
    let mut conn = create(&path_for(&h.main)).unwrap();
    let e = Entry { title: title.into(), ..Default::default() };
    import(&mut conn, 1, &[e], added_at, None).unwrap();
    conn
}

fn stage_on(h: &mut Harness) {
    let conn = db::open_at(&h.main).unwrap();
    let config = Config {
        postprocess: PostprocessConfig { min_size_bytes: 0, ..quick_config() },
        predb: PredbConfig { enabled: true, ..Default::default() },
        category: CategoryConfig { enabled: false, ..Default::default() },
        ..Config::default()
    };
    h.stage = Some(Stage::start(StageParams {
        main: h.main.clone(),
        db: shared_db(conn),
        pool: Arc::new(Pool::new(&[])),
        config,
        steps: steps::all(),
        handle: h.runtime.handle().clone(),
    }));
}

#[test]
fn a_title_only_in_a_later_file_is_a_known_miss() {
    let mut h = fresh();
    let mut r = release_of("grp-t.mkv", "grp-t.mkv", 2, 2);
    r.articles.push(article(&format!("{T}.nfo"), 1, 1, "<later@x>"));
    h.save(&[r]);
    let id = h.id_of("grp-t.mkv");
    let _titles = with_title(&h, T, 5);
    assert!(nominated(&h, T).ids.is_empty(), "the miss");
    stage_on(&mut h);
    h.stage.clone().unwrap().rerun(id, mask(PREDB_MATCH)).unwrap();
    wait_for("the remedy", || {
        h.shard()
            .query_row("select name_source from releases where id = ?", [id], |r| r.get::<_, Option<i64>>(0))
            .unwrap()
            == Some(100)
    });
}

#[test]
fn an_fts_hit_needs_a_literal_agreement() {
    let h = fresh();
    h.save(&[release_of(&format!("{T}.Extra"), "x.bin", 2, 2)]);
    assert!(nominated(&h, T).ids.is_empty());
}

#[test]
fn a_short_title_skips_the_phrase() {
    let h = fresh();
    h.save(&[release_of("Foo.Bar-GRP.mkv", "x.bin", 2, 2)]);
    let n = nominated(&h, "Foo.Bar-GRP");
    assert!(n.ids.is_empty());
    assert_eq!(n.phrases, 0);

    let h = fresh();
    h.save(&[release_of("Foo.Bar-GRP", "x.bin", 2, 2)]);
    assert_eq!(nominated(&h, "Foo.Bar-GRP").ids, [h.id_of("Foo.Bar-GRP")]);
}

#[test]
fn a_named_release_is_left_out() {
    let h = fresh();
    h.save(&[release_of(T, "x.bin", 2, 2)]);
    h.shard().execute("update releases set name_source = 100, display_name = ?", [T]).unwrap();
    assert!(nominated(&h, T).ids.is_empty());
}

#[test]
fn too_many_is_refused() {
    let h = fresh();
    let groups: Vec<String> = (0..17).map(|i| format!("alt.binaries.g{i}")).collect();
    let rows: Vec<Release> =
        groups.iter().map(|g| Release { group: g.clone(), ..release_of(T, "x.bin", 2, 2) }).collect();
    h.save(&rows);
    let n = nominated(&h, T);
    assert!(n.overflow, "{n:?}");
    assert!(n.ids.is_empty());
    let conn = with_title(&h, T, 5);
    let reruns = std::cell::Cell::new(0);
    let read = tick(&conn, &shards(&h), 10_000, 10, &AtomicBool::new(false), &|_| {
        reruns.set(reruns.get() + 1);
        Ok(())
    })
    .unwrap();
    assert_eq!((read, reruns.get()), (1, 0));
}

#[test]
fn the_budget_stops_between_shards() {
    let h = fresh();
    h.save(&[release_of(T, "x.bin", 2, 2)]);
    let start = Instant::now();
    let calls = std::cell::Cell::new(0u32);
    // after the first phrase query, 3 s have gone
    let clock = || {
        let n = calls.get();
        calls.set(n + 1);
        if n >= 2 { start + Duration::from_secs(3) } else { start }
    };
    let n = nominate(&shards(&h), T, &clock, &AtomicBool::new(false)).unwrap();
    assert!(n.out_of_budget, "{n:?}");
    assert_eq!(n.phrases, 1);
    assert_eq!(n.ids, [h.id_of(T)], "the other routes still ran");
}

#[test]
fn stop_ends_a_title_between_statements() {
    let h = fresh();
    let conn = with_title(&h, T, 5);
    let stop = AtomicBool::new(true);
    let n = nominate(&shards(&h), T, &Instant::now, &stop).unwrap();
    assert!(n.stopped);
    let read = tick(&conn, &shards(&h), 10_000, 10, &stop, &|_| Ok(())).unwrap();
    assert_eq!(read, 0);
    let cursor: i64 = conn.query_row("select value from meta where key = 'late_after_id'", [], |r| r.get(0)).unwrap();
    assert_eq!(cursor, 0, "the cursor stays");
}

#[test]
fn the_cursor_waits_and_resumes() {
    let h = fresh();
    let conn = with_title(&h, T, 10_000);
    let stop = AtomicBool::new(false);
    let shards = shards(&h);
    assert_eq!(tick(&conn, &shards, 10_010, 10, &stop, &|_| Ok(())).unwrap(), 0, "10 s old");
    assert_eq!(tick(&conn, &shards, 10_601, 10, &stop, &|_| Ok(())).unwrap(), 1, "601 s old");
    drop(conn);
    let conn = create(&path_for(&h.main)).unwrap();
    assert_eq!(
        tick(&conn, &shards, 10_700, 10, &stop, &|_| Ok(())).unwrap(),
        0,
        "a new connection carries on after it"
    );
}

#[test]
fn a_failed_rerun_keeps_the_title() {
    let h = fresh();
    h.save(&[release_of(T, "x.bin", 2, 2)]);
    let conn = with_title(&h, T, 5);
    let failing = tick(&conn, &shards(&h), 10_000, 10, &AtomicBool::new(false), &|_| anyhow::bail!("no"));
    assert!(failing.is_err());
    let cursor: i64 = conn.query_row("select value from meta where key = 'late_after_id'", [], |r| r.get(0)).unwrap();
    assert_eq!(cursor, 0);
}

fn plan(conn: &Connection, sql: &str, args: impl rusqlite::Params) -> String {
    conn.query_row(&format!("explain query plan {sql}"), args, |r| r.get(3)).unwrap()
}

#[test]
fn no_statement_scans_releases() {
    let h = fresh();
    let conn = h.shard();
    assert_eq!(plan(&conn, NAME_PROBE, ["x"]), "SEARCH releases USING INDEX idx_release_unique (name=?)");
    let fts = plan(&conn, FTS_PROBE, params!["\"x\"", 32]);
    assert!(fts.starts_with("SCAN releases_fts VIRTUAL TABLE INDEX"), "{fts}");
    assert_eq!(plan(&conn, ROW_READ, [1]), "SEARCH releases USING INTEGER PRIMARY KEY (rowid=?)");
    assert_eq!(plan(&conn, INNER_PROBE, [1]), "SEARCH predb_inner_key USING PRIMARY KEY (key=?)");
}

#[test]
fn a_title_arriving_later_renames_the_release() {
    let mut h = fresh();
    let file = format!("{T}.mkv");
    h.save(&[release_of(&file, &file, 2, 2)]);
    let id = h.id_of(&file);
    let _ = with_title(&h, "Some.Other.Title.2024.WEB.x264-GRP", 5);
    stage_on(&mut h);
    wait_for("no match yet", || h.bits(&file).is_some_and(|b| b.0 & mask(PREDB_MATCH) != 0));

    let now = 1_000_000;
    let mut conn = create(&path_for(&h.main)).unwrap();
    conn.execute("update meta set value = (select max(id) from titles) where key = 'late_after_id'", []).unwrap();
    let e = Entry { title: T.into(), ..Default::default() };
    import(&mut conn, 1, &[e], now - 601, None).unwrap();
    let stage = h.stage.clone().unwrap();
    let rerun = |release: i64| stage.rerun(release, mask(PREDB_MATCH));
    assert_eq!(tick(&conn, &shards(&h), now, 10, &AtomicBool::new(false), &rerun).unwrap(), 1);
    wait_for("the title", || {
        h.shard()
            .query_row("select name_source from releases where id = ?", [id], |r| r.get::<_, Option<i64>>(0))
            .unwrap()
            == Some(100)
    });
}

#[test]
fn inner_rows_reach_the_late_path() {
    let mut h = fresh();
    h.save(&[complete("Xk3Mz9PqL")]);
    let id = h.id_of("Xk3Mz9PqL");
    h.shard().execute(INSERT_FILE, params![id, SOURCE_RAR * 65_536, format!("{T}.mkv"), 100, SOURCE_RAR]).unwrap();
    let _ = with_title(&h, "Some.Other.Title.2024.WEB.x264-GRP", 5);
    stage_on(&mut h);
    wait_for("its key", || {
        h.shard()
            .query_row("select count(*) from predb_inner_key where key = ?", [name_key(T)], |r| r.get::<_, i64>(0))
            .unwrap()
            == 1
    });
    assert_eq!(nominated(&h, T).ids, [id]);
}

#[test]
#[ignore]
fn late_cost_on_a_big_shard() {
    let h = fresh();
    let rows: Vec<Release> =
        (0..200_000).map(|i| release_of(&format!("Rel.{i}.1080p.web.dl-GRP"), "x.bin", 1, 1)).collect();
    for chunk in rows.chunks(10_000) {
        h.save(chunk);
    }
    let start = Instant::now();
    let n = nominated(&h, T);
    println!("{:?}, {:?}", start.elapsed(), n);
}
