//! Blocked groups stay on disk until a purge, but nothing about them is indexed
//! or served.
//!
//! One test, and the environment is set before any thread starts.

mod common;

use atlas::config::Config;
use atlas::indexer::Indexer;
use atlas::nntp::BlockingPool;
use atlas::{api, db, nzb, search, store};
use common::{GROUP, Server, mock, par2_file_desc, post, spawn_server, yenc_body};

const BLOCKED: &str = "alt.binaries.pictures.erotica.teen";
const OPEN: &str = "alt.binaries.open";

fn old_row(main: &std::path::Path, group: &str, seq: i64, name: &str, obfuscated: bool) -> i64 {
    let shard = store::shard_of(group);
    let id = store::global_id(seq, shard);
    let conn = db::open_shard(&store::shard_path(main, shard)).unwrap();
    conn.execute(
        "insert into releases (id, name, group_name, poster, posted_date, size, complete, parts, is_obfuscated)
         values (?1, ?2, ?3, 'p', '2026-10-02 10:11:12', 10, 1, 1, ?4)",
        rusqlite::params![id, name, group, obfuscated],
    )
    .unwrap();
    conn.execute(
        "insert into files (release_id, filename, subject, subject_part, subject_mid, expected, seen)
         values (?1, ?2, 'x', 1, '<m@x>', 1, x'02')",
        rusqlite::params![id, format!("{name}.bin")],
    )
    .unwrap();
    let file = conn.last_insert_rowid();
    let local = [&[0u8][..], format!("<{name}@x>").as_bytes()].concat();
    conn.execute(
        "insert into segments (file_id, local, domain, part, bytes) values (?, ?, 0, 1, 10)",
        rusqlite::params![file, local],
    )
    .unwrap();
    id
}

#[test]
fn a_blocked_group_is_never_indexed_or_served() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
    }
    db::create_db().unwrap();

    let main = atlas::paths::database();
    let hidden = old_row(&main, BLOCKED, 1, "Findable.Thing.Hidden", false);
    let hidden_obf = old_row(&main, BLOCKED, 2, "a1B2c3D4e5F6g7H8i9J0", true);
    let shown = old_row(&main, OPEN, 3, "Findable.Thing.Shown", false);
    let shown_obf = old_row(&main, OPEN, 4, "z9Y8x7W6v5U4t3S2r1Q0", true);

    let hits = search::search_all_releases("findable", 0, 10).unwrap();
    assert_eq!(hits.iter().map(|r| r.id).collect::<Vec<_>>(), vec![shown]);
    assert_eq!(search::count_all_releases("findable").unwrap(), 1);
    assert_eq!(search::search_releases("findable", BLOCKED, 0, 10).unwrap(), vec![]);
    assert_eq!(search::count_releases("findable", BLOCKED).unwrap(), 0);
    // the LIKE fallback too: an fts syntax error takes it
    assert!(search::search_all_releases("Hidden\"(", 0, 10).unwrap().is_empty());
    assert_eq!(search::all_releases(0, 10).unwrap().iter().map(|r| r.id).collect::<Vec<_>>(), vec![shown]);
    let obfuscated = search::search_obfuscated(0, 10).unwrap();
    assert_eq!(obfuscated.iter().map(|r| r.id).collect::<Vec<_>>(), vec![shown_obf]);
    assert_eq!(search::count_obfuscated().unwrap(), 1);
    assert_eq!(search::recent_in_groups(&[BLOCKED.to_string()], 0, 10).unwrap(), vec![]);

    assert_eq!(search::get_release(hidden).unwrap(), None);
    assert_eq!(search::get_release(hidden_obf).unwrap(), None);
    assert_eq!(search::get_articles(hidden).unwrap(), vec![]);
    assert_eq!(nzb::build_nzb(hidden).unwrap(), None);
    assert!(nzb::generate_nzb(hidden, Some(home.path())).is_err());
    assert!(nzb::build_nzb(shown).unwrap().is_some());

    let r = api::handle("/api", "t=search&q=findable&apikey=k", "http://x", "k");
    assert!(r.body.contains("Findable.Thing.Shown") && !r.body.contains("Findable.Thing.Hidden"), "{}", r.body);
    let r = api::handle("/api", "t=search&apikey=k", "http://x", "k");
    assert!(!r.body.contains("Hidden"), "{}", r.body);
    assert_eq!(api::handle("/api", &format!("t=get&id={hidden}&apikey=k"), "http://x", "k").status, 404);
    assert_eq!(api::handle("/api", &format!("t=get&id={shown}&apikey=k"), "http://x", "k").status, 200);

    // nothing is deleted before a purge
    let conn = db::open().unwrap();
    assert_eq!(conn.query_row("select count(*) from releases", [], |r| r.get::<_, i64>(0)).unwrap(), 4);

    let config = Config {
        group: BLOCKED.into(),
        groups: vec![BLOCKED.into(), GROUP.into(), "alt.binaries.yong.eroitca".into()],
        ..Config::default()
    };
    assert_eq!(config.tracked_groups(), vec![GROUP.to_string()]);
    assert_eq!(config.blocked_groups(), vec![BLOCKED.to_string(), "alt.binaries.yong.eroitca".to_string()]);
    let only = Config { group: BLOCKED.into(), groups: vec![BLOCKED.into()], ..Config::default() };
    assert_eq!(only.tracked_groups(), Vec::<String>::new());

    // a pass over a blocked group: no header, no body, no cursor, no row
    let par2 = par2_file_desc("Real.Name.mkv", 100);
    let mut server = Server::new(vec![
        post(1, r#""Unsafe.Thing.par2" yEnc (1/1)"#, 10, yenc_body("Unsafe.Thing.par2", &par2)),
        post(2, r#""Other.Thing.bin" yEnc (1/1)"#, 10, vec![]),
    ]);
    std::sync::Arc::get_mut(&mut server).unwrap().any_group = true;
    let port = spawn_server(server.clone());
    let client = BlockingPool::new(&[mock(port, "secret", 2, 1)]);
    client.connect().unwrap();
    let mut indexer = Indexer::new(client, "backfill", db::open().unwrap());
    indexer.index_group_with(BLOCKED, &mut |_| panic!("no slice of a blocked group")).unwrap();
    assert_eq!(server.xovers.load(std::sync::atomic::Ordering::SeqCst), 0, "no headers");
    assert_eq!(server.bodies_sent.load(std::sync::atomic::Ordering::SeqCst), 0, "no bodies");
    let cursors: i64 =
        conn.query_row("select count(*) from groups where name like ?", [format!("{BLOCKED}%")], |r| r.get(0)).unwrap();
    assert_eq!(cursors, 0);

    // the store takes nothing of it, whatever path it came by
    let release = atlas::parser::Release { name: "Late".into(), group: BLOCKED.into(), ..Default::default() };
    store::save(&main, &[release]).unwrap();
    assert_eq!(conn.query_row("select count(*) from releases", [], |r| r.get::<_, i64>(0)).unwrap(), 4);
}
