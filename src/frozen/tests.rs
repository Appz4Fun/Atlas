use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;

use super::*;
use crate::parser::{Article, Release};
use crate::{db, store};

const G: &str = "alt.binaries.t";
const NOW: i64 = 1_800_000_000;

fn article(file: &str, n: i64, of: i64, bytes: i64) -> Article {
    Article {
        message_id: format!("<{file}.{n}@news.example>"),
        subject: format!("\"{file}\" yEnc ({n}/{of})"),
        filename: Some(file.into()),
        part: Some(n),
        total_parts: Some(of),
        bytes,
        ..Default::default()
    }
}

/// a post of `files`, each `parts` articles of `bytes`, its subjects counting its files (`[n/N]`)
fn post(name: &str, files: &[&str], parts: i64, bytes: i64) -> Release {
    let total = files.len() as i64;
    let articles = files
        .iter()
        .flat_map(|f| (1..=parts).map(move |n| Article { file_total: Some(total), ..article(f, n, parts, bytes) }))
        .collect();
    Release { name: name.into(), group: G.into(), articles, complete: true, ..Default::default() }
}

struct Fixture {
    _dir: tempfile::TempDir,
    main: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        Fixture { _dir: dir, main }
    }

    fn shard_path(&self) -> PathBuf {
        store::shard_path(&self.main, store::shard_of(G))
    }

    /// the shard's writer connection: its frozen file attached
    fn writer(&self) -> Connection {
        let conn = db::open_shard(&self.shard_path()).unwrap();
        attach(&conn, &self.shard_path()).unwrap();
        conn
    }

    /// saved by a writer that has the frozen file, as the indexer's does
    fn save(&self, releases: &[Release]) {
        let mut conn = self.writer();
        let mut writer = store::ShardWriter::new(store::shard_of(G));
        writer.save(&mut conn, &store::Ids::new(&self.main), [releases]).unwrap();
    }

    fn id(&self, conn: &Connection, name: &str) -> i64 {
        conn.query_row("select id from releases where name = ?1", [name], |r| r.get(0)).unwrap()
    }
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn meta(conn: &Connection, key: &str) -> i64 {
    conn.query_row("select value from main.meta where key = ?1", [key], |r| r.get(0)).unwrap()
}

fn freeze_all(conn: &Connection) -> Moved {
    freeze_some(conn, 1_000, Duration::from_secs(60), i64::MAX, NOW).unwrap()
}

/// the articles of `id` as an NZB lists them, from the live shard
fn live_articles(main: &Path, id: i64) -> Vec<crate::search::ArticleRow> {
    let conn = db::open_shard(main).unwrap();
    store::attach(&conn, main).unwrap();
    store::articles(&conn, id).unwrap()
}

#[test]
fn a_small_complete_release_freezes_whole_and_thaws_back_as_it_was() {
    let f = Fixture::new();
    f.save(&[post("Small.Thing", &["a.rar", "b.rar"], 3, 1000), post("Other", &["c.mkv"], 1, 10)]);
    let conn = f.writer();
    let id = f.id(&conn, "Small.Thing");
    conn.execute("insert into release_category (release_id, category) values (?1, 7020)", [id]).unwrap();
    conn.execute("insert into pp_state (release_id, done) values (?1, 5)", [id]).unwrap();
    conn.execute("insert into collections (key, release_id) values (42, ?1)", [id]).unwrap();
    let before = collect(&conn, id).unwrap().unwrap();
    let articles = live_articles(&f.main, id);
    assert_eq!(articles.len(), 6);
    let (releases, total) = (meta(&conn, "releases"), meta(&conn, "articles"));

    assert!(rules::evaluate(&conn, id, NOW).unwrap(), "small and complete");
    assert_eq!(freeze_all(&conn), Moved { frozen: 1, ..Moved::default() });
    for sql in [
        "select count(*) from releases where name = 'Small.Thing'",
        "select count(*) from files where filename in ('a.rar', 'b.rar')",
        "select count(*) from release_category",
        "select count(*) from pp_state",
        "select count(*) from collections",
        "select count(*) from freeze_pending",
    ] {
        assert_eq!(count(&conn, sql), 0, "{sql}");
    }
    assert_eq!((meta(&conn, "releases"), meta(&conn, "articles")), (releases - 1, total - 6));
    assert_eq!(count(&conn, "select count(*) from frozen.frozen_releases where reason = 'small_complete'"), 1);
    assert_eq!(count(&conn, "select category from frozen.frozen_releases"), 7020);
    assert_eq!(count(&conn, "select release_id from frozen.frozen_collections where key = 42"), id);
    assert!(count(&conn, "select count(*) from word_queue") > 0, "the word index drops it");

    // deep search and its NZB, without thawing it
    let hits = search(&f.main, "small", None, &Narrow::default(), 10).unwrap();
    assert_eq!(hits.iter().map(|h| h.row.id).collect::<Vec<_>>(), vec![id]);
    assert!(search(&f.main, "nothing", None, &Narrow::default(), 10).unwrap().is_empty());
    let (row, nzb_body) = nzb(&f.main, id).unwrap().unwrap();
    assert_eq!(row.name, "Small.Thing");
    assert_eq!(nzb_body, crate::nzb::render_nzb(&row, &articles));
    assert_eq!(get(&f.main, id).unwrap().map(|(h, _)| h.row.id), Some(id), "its page");
    // a group the filters hide now: no page for it either, as a search leaves it out
    let blocked = "a.".repeat(300);
    assert!(crate::filters::group_blocked(&blocked));
    conn.execute("update frozen.frozen_releases set group_name = ?2 where id = ?1", rusqlite::params![id, blocked])
        .unwrap();
    assert!(get(&f.main, id).unwrap().is_none());
    assert!(search(&f.main, "small", None, &Narrow::default(), 10).unwrap().is_empty());
    conn.execute("update frozen.frozen_releases set group_name = ?2 where id = ?1", rusqlite::params![id, G]).unwrap();

    assert_eq!(thaw(&conn, id, true, NOW).unwrap(), Thawed::Restored);
    assert_eq!(count(&conn, "select count(*) from frozen.frozen_releases"), 0);
    assert_eq!(count(&conn, "select count(*) from frozen.frozen_collections"), 0);
    assert_eq!((meta(&conn, "releases"), meta(&conn, "articles")), (releases, total));
    assert_eq!(live_articles(&f.main, id), articles);
    let after = collect(&conn, id).unwrap().unwrap();
    assert_eq!(after.0, before.0);
    // the same rows, but the files' new ids
    let without_file_ids = |p: &Payload| -> Vec<Table> {
        p.tables
            .iter()
            .map(|t| {
                let mut t = t.clone();
                for col in ["id", "file_id"] {
                    if let Some(c) = t.column(col).filter(|_| t.name != "releases") {
                        for r in &mut t.rows {
                            r[c] = Value::Null;
                        }
                    }
                }
                t.rows.sort_by_key(|r| format!("{r:?}"));
                t
            })
            .collect()
    };
    assert_eq!(without_file_ids(&after.1), without_file_ids(&before.1));
    assert!(!rules::evaluate(&conn, id, NOW).unwrap(), "thawed by hand: it stays");
}

#[test]
fn only_what_the_small_rule_says_freezes() {
    let f = Fixture::new();
    f.save(&[
        post("Big", &["a.mkv"], 2, 600 << 20),
        post("Small", &["b.mkv"], 1, 10),
        post("Hidden.Upload", &["c.rar"], 1, 10),
        post("Pointer", &["d.nzb"], 1, 10),
        post("Pointed", &["e.rar"], 1, 10),
        post("Linked", &["f.rar"], 1, 10),
        post("Lead", &["g.rar"], 1, 10),
        post("Episode", &["h.mkv"], 1, 10),
    ]);
    let mut half = post("Half", &["i.rar"], 2, 10);
    half.articles.pop();
    // every file it has so far is whole, but its subjects don't say how many files there are: more may come
    let mut uncounted = post("Uncounted", &["j.part1.rar"], 1, 10);
    uncounted.articles.iter_mut().for_each(|a| a.file_total = None);
    f.save(&[half, uncounted]);
    let conn = f.writer();
    let id = |n: &str| f.id(&conn, n);
    conn.execute("update releases set is_obfuscated = 1 where name = 'Hidden.Upload'", []).unwrap();
    conn.execute("insert into nzb_pointed (release_id) values (?1)", [id("Pointed")]).unwrap();
    conn.execute("insert into upload_link (release_id, lead_id) values (?1, ?2)", [id("Linked"), id("Lead")]).unwrap();
    conn.execute("insert into release_category (release_id, category) values (?1, 5030)", [id("Episode")]).unwrap();
    let queued = |n: &str| rules::evaluate(&conn, id(n), NOW).unwrap();
    assert!(!queued("Big"), "1.2 GB");
    assert!(queued("Small"));
    assert!(!queued("Half"), "incomplete");
    assert_eq!(count(&conn, "select complete from releases where name = 'Uncounted'"), 1);
    assert!(!queued("Uncounted"), "no file count: it can still grow");
    for n in ["Hidden.Upload", "Pointer", "Pointed", "Linked", "Lead"] {
        assert!(!queued(n), "{n}");
    }
    assert!(queued("Episode"), "small TV too, unless kept");
    SMALL_KEEPS_TV_MOVIES.store(true, Relaxed);
    let kept = !queued("Episode");
    let small_still = queued("Small");
    SMALL_KEEPS_TV_MOVIES.store(false, Relaxed);
    assert!(kept && small_still, "kept: movies and TV only");
    // the queue follows what the release is now
    assert!(queued("Small"));
    conn.execute("update releases set size = 2000000000 where name = 'Small'", []).unwrap();
    assert!(!queued("Small"));
    assert_eq!(count(&conn, &format!("select count(*) from freeze_pending where release_id = {}", id("Small"))), 0);
}

#[test]
fn whatever_is_not_movies_tv_or_other_freezes_complete_or_not() {
    let f = Fixture::new();
    let mut music = post("Album", &["a.flac"], 3, 600 << 20);
    music.articles.pop();
    f.save(&[
        music,
        post("Film", &["b.mkv"], 2, 600 << 20),
        post("Show", &["c.mkv"], 2, 600 << 20),
        post("Misc", &["d.bin"], 2, 600 << 20),
        post("Unknown", &["e.bin"], 2, 600 << 20),
        post("Clips", &["f.rar"], 2, 600 << 20),
        post("Book.Pointer", &["g.nzb"], 2, 600 << 20),
    ]);
    let conn = f.writer();
    let id = |n: &str| f.id(&conn, n);
    for (n, category) in
        [("Album", 3040), ("Film", 2040), ("Show", 5040), ("Misc", 8010), ("Clips", 6040), ("Book.Pointer", 7020)]
    {
        conn.execute("insert into release_category (release_id, category) values (?1, ?2)", params![id(n), category])
            .unwrap();
    }
    conn.execute("update releases set is_obfuscated = 1 where name = 'Clips'", []).unwrap();
    let queued = |n: &str| rules::evaluate(&conn, id(n), NOW).unwrap();
    assert!(queued("Album"), "music, incomplete as it is");
    let reason: String =
        conn.query_row("select reason from freeze_pending where release_id = ?1", [id("Album")], |r| r.get(0)).unwrap();
    assert_eq!(reason, rules::NOT_TV_MOVIES);
    for n in ["Film", "Show", "Misc", "Unknown", "Clips", "Book.Pointer"] {
        assert!(!queued(n), "{n}");
    }
    assert_eq!(freeze_all(&conn).frozen, 1);
    assert_eq!(count(&conn, "select count(*) from frozen.frozen_releases where reason = 'not_tv_movies'"), 1);
}

#[test]
fn a_database_lane_visit_queues_a_release_for_freezing() {
    let f = Fixture::new();
    f.save(&[post("Small", &["b.mkv"], 1, 10)]);
    let conn = f.writer();
    let id = f.id(&conn, "Small");
    let applied =
        crate::postprocess::apply::Applied { release_id: id, freeze_check: true, no_state: true, ..Default::default() };
    assert!(crate::postprocess::apply::apply_one(&conn, applied, NOW, &|a| (60, a > 3), &mut Vec::new()).unwrap());
    assert_eq!(count(&conn, "select count(*) from freeze_pending"), 1);
}

#[test]
fn late_articles_thaw_their_release_and_join_it() {
    let f = Fixture::new();
    f.save(&[post("Late.Post", &["a.rar"], 3, 100)]);
    let conn = f.writer();
    let id = f.id(&conn, "Late.Post");
    assert!(rules::evaluate(&conn, id, NOW).unwrap());
    assert_eq!(freeze_all(&conn).frozen, 1);
    assert_eq!(count(&conn, "select count(*) from releases"), 0);
    drop(conn);

    // another file of the post, late
    f.save(&[post("Late.Post", &["b.rar"], 2, 100)]);
    let conn = f.writer();
    assert_eq!(count(&conn, "select count(*) from frozen.frozen_releases"), 0, "thawed");
    assert_eq!(f.id(&conn, "Late.Post"), id, "the same release");
    assert_eq!(count(&conn, "select parts from releases"), 5);
    assert_eq!(live_articles(&f.main, id).len(), 5);
}

#[test]
fn a_release_that_left_the_rules_after_it_was_queued_stays_live() {
    let f = Fixture::new();
    f.save(&[post("Grew", &["a.rar"], 1, 100)]);
    let conn = f.writer();
    let id = f.id(&conn, "Grew");
    assert!(rules::evaluate(&conn, id, NOW).unwrap());
    conn.execute("update releases set size = 2000000000 where id = ?1", [id]).unwrap();
    assert_eq!(freeze_all(&conn), Moved { dropped: 1, ..Moved::default() });
    assert_eq!(count(&conn, "select count(*) from releases"), 1);
    assert_eq!(count(&conn, "select count(*) from freeze_pending"), 0);
}

#[test]
fn a_crash_between_the_steps_loses_nothing() {
    let f = Fixture::new();
    f.save(&[post("Twice", &["a.rar"], 2, 100)]);
    let conn = f.writer();
    let id = f.id(&conn, "Twice");
    conn.execute("insert into freeze_pending (release_id, reason, at) values (?1, 'test', 1)", [id]).unwrap();
    // the copy committed, the delete never ran
    let queued = vec![(id, "test".to_string())];
    let (copied, dropped) = copy(&conn, &queued, Duration::from_secs(60), NOW).unwrap();
    assert_eq!((copied.len(), dropped.len()), (1, 0));
    assert_eq!(count(&conn, "select count(*) from releases"), 1);
    // the next tick copies again and deletes
    assert_eq!(freeze_all(&conn).frozen, 1);
    assert_eq!(count(&conn, "select count(*) from frozen.frozen_releases"), 1);
    assert_eq!(count(&conn, "select count(*) from releases"), 0);

    // a thaw cut short leaves both: the live one wins
    restore(&conn, "main", &collect_frozen(&conn, id), Domains::Map).unwrap();
    assert_eq!(thaw(&conn, id, false, NOW).unwrap(), Thawed::AlreadyLive);
    assert_eq!(count(&conn, "select count(*) from frozen.frozen_releases"), 0);
    assert_eq!(count(&conn, "select count(*) from releases"), 1);
}

fn collect_frozen(conn: &Connection, id: i64) -> Payload {
    let packed: Vec<u8> =
        conn.query_row("select payload from frozen.frozen_releases where id = ?1", [id], |r| r.get(0)).unwrap();
    payload::decode(&packed).unwrap()
}

#[test]
fn a_release_that_changes_between_the_copy_and_the_delete_stays_live() {
    let f = Fixture::new();
    f.save(&[post("Moving", &["a.rar"], 2, 100)]);
    let conn = f.writer();
    let id = f.id(&conn, "Moving");
    conn.execute("insert into freeze_pending (release_id, reason, at) values (?1, 'test', 1)", [id]).unwrap();
    let (copied, dropped) = copy(&conn, &[(id, "test".into())], Duration::from_secs(60), NOW).unwrap();
    conn.execute("update releases set parts = parts + 1, size = size + 5 where id = ?1", [id]).unwrap();
    assert_eq!(drop_copied(&conn, &copied, &dropped).unwrap(), Moved { changed: 1, ..Moved::default() });
    assert_eq!(count(&conn, "select count(*) from releases"), 1);
    assert_eq!(count(&conn, "select count(*) from freeze_pending"), 1, "still queued: the next tick copies it again");
    assert_eq!(freeze_all(&conn).frozen, 1);
    assert_eq!(count(&conn, "select parts from frozen.frozen_releases"), 3, "the copy it froze is the new one");
}

#[test]
fn a_sealed_file_thaws_with_its_message_ids_whatever_the_domains_ids_are_now() {
    let f = Fixture::new();
    f.save(&[post("Sealed", &["a.rar"], 5, 100)]);
    let mut conn = f.writer();
    let sealed = store::ShardWriter::new(store::shard_of(G)).seal_some(&mut conn, NOW).unwrap();
    assert_eq!(sealed, 1);
    let id = f.id(&conn, "Sealed");
    assert_eq!(count(&conn, "select count(*) from files where blob is not null"), 1);
    assert_eq!(count(&conn, "select count(*) from domains"), 1, "past SHARED_AFTER the domain has a row");
    let articles = live_articles(&f.main, id);
    conn.execute("insert into freeze_pending (release_id, reason, at) values (?1, 'test', 1)", [id]).unwrap();
    assert_eq!(freeze_all(&conn).frozen, 1);
    // the domain went and ids moved on, as a compaction can leave them
    conn.execute_batch("delete from domains; insert into domains (suffix) values ('@one>'), ('@two>');").unwrap();
    let (row, nzb_text) = nzb(&f.main, id).unwrap().unwrap();
    assert_eq!(nzb_text, crate::nzb::render_nzb(&row, &articles));
    assert_eq!(thaw(&conn, id, false, NOW).unwrap(), Thawed::Restored);
    assert_eq!(live_articles(&f.main, id), articles);
}

#[test]
fn a_name_another_frozen_release_has_stays_live() {
    let f = Fixture::new();
    f.save(&[post("Same", &["a.rar"], 1, 100)]);
    let conn = f.writer();
    let id = f.id(&conn, "Same");
    let (s, p) = collect(&conn, id).unwrap().unwrap();
    let other = Summary { id: id + store::SHARDS as i64, ..s.clone() };
    assert_eq!(write(&conn, &other, &p, "test", NOW).unwrap(), Written::Written);
    assert_eq!(write(&conn, &s, &p, "test", NOW).unwrap(), Written::NameTaken);
    conn.execute("insert into freeze_pending (release_id, reason, at) values (?1, 'test', 1)", [id]).unwrap();
    assert_eq!(freeze_all(&conn), Moved { dropped: 1, ..Moved::default() });
    assert_eq!(count(&conn, "select count(*) from releases"), 1);
    assert_eq!(count(&conn, "select count(*) from freeze_pending"), 0);
    // and a live one with its name keeps a frozen one frozen
    assert_eq!(thaw(&conn, other.id, false, NOW).unwrap(), Thawed::NameTaken);
}

#[test]
fn deep_search_filters_by_category() {
    let f = Fixture::new();
    f.save(&[post("Some.Book", &["a.epub"], 1, 100), post("Some.Song", &["b.flac"], 1, 100)]);
    let conn = f.writer();
    conn.execute("insert into release_category (release_id, category) values (?1, 7020)", [f.id(&conn, "Some.Book")])
        .unwrap();
    conn.execute_batch("insert into freeze_pending (release_id, reason, at) select id, 'test', 1 from releases")
        .unwrap();
    assert_eq!(freeze_all(&conn).frozen, 2);
    let books = crate::category::CatFilter { leaves: vec![7020], uncategorized: false };
    let names = |filter: Option<&crate::category::CatFilter>| -> Vec<String> {
        search(&f.main, "some", filter, &Narrow::default(), 10).unwrap().into_iter().map(|h| h.row.name).collect()
    };
    assert_eq!(names(Some(&books)), vec!["Some.Book".to_string()]);
    assert_eq!(
        names(Some(&crate::category::CatFilter { leaves: vec![], uncategorized: true })),
        vec!["Some.Song".to_string()]
    );
    assert_eq!(names(None).len(), 2);
    assert_eq!(search(&f.main, "", None, &Narrow::default(), 1).unwrap().len(), 1, "newest first, the limit holds");
    let stats = stats(&f.main).unwrap();
    assert_eq!(stats.iter().map(|s| s.2).sum::<i64>(), 2);
}

/// Freezing reads each of a release's tables by its key: a scan of one for each release would hold the writer for
/// minutes on a big shard.
#[test]
fn every_table_a_release_is_read_from_has_an_index_for_it() {
    let f = Fixture::new();
    let conn = f.writer();
    let mut reads: Vec<String> = vec![
        "select * from files where release_id = 1".into(),
        "select * from segments where file_id = 1".into(),
        "select * from held_back where file_id = 1".into(),
        "select * from collections where release_id = 1".into(),
    ];
    reads.extend(release_tables().map(|(t, k)| format!("select * from {t} where {k} = 1")));
    for sql in reads {
        let plan: Vec<String> = conn
            .prepare(&format!("explain query plan {sql}"))
            .unwrap()
            .query_map([], |r| r.get(3))
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        assert!(plan.iter().all(|p| !p.starts_with("SCAN")), "{sql}: {plan:?}");
    }
}

#[test]
fn nothing_moves_while_the_disk_is_short() {
    let f = Fixture::new();
    f.save(&[post("Small", &["b.mkv"], 1, 10)]);
    let conn = f.writer();
    conn.execute_batch("insert into freeze_pending (release_id, reason, at) select id, 'test', 1 from releases")
        .unwrap();
    let mut ticker = Ticker { disk: Some((Instant::now(), false)), ..Ticker::default() };
    assert_eq!(ticker.tick(&conn, NOW).unwrap(), Moved::default());
    let mut ticker = Ticker { disk: Some((Instant::now(), true)), ..Ticker::default() };
    assert_eq!(ticker.tick(&conn, NOW).unwrap().frozen, 1);
    assert_eq!(ticker.tick(&conn, NOW).unwrap(), Moved::default(), "a tick a second at most");
}

#[test]
fn a_release_waits_its_hours_queued_before_it_moves() {
    let f = Fixture::new();
    f.save(&[post("Fresh", &["b.mkv"], 1, 10)]);
    let conn = f.writer();
    assert!(rules::evaluate(&conn, f.id(&conn, "Fresh"), NOW).unwrap());
    let tick = |now: i64| Ticker { disk: Some((Instant::now(), true)), ..Ticker::default() }.tick(&conn, now).unwrap();
    let wait = WAIT_SECS.load(Relaxed);
    assert_eq!(tick(NOW + wait - 1), Moved::default(), "still in its wait");
    // a visit that finds it matching still starts its wait again
    assert!(rules::evaluate(&conn, f.id(&conn, "Fresh"), NOW + wait - 1).unwrap());
    assert_eq!(tick(NOW + wait), Moved::default());
    assert_eq!(tick(NOW + 2 * wait).frozen, 1);
}

#[test]
fn a_shard_without_its_frozen_file_saves_as_before() {
    let f = Fixture::new();
    store::save(&f.main, &[post("Plain", &["a.rar"], 1, 10)]).unwrap();
    let conn = db::open_shard(&f.shard_path()).unwrap();
    assert!(!attached(&conn).unwrap());
    assert!(!path(&f.shard_path()).exists(), "only the indexer's writer makes the frozen file");
    assert_eq!(count(&conn, "select count(*) from releases"), 1);
}

#[test]
fn a_save_outside_the_indexer_thaws_too() {
    let f = Fixture::new();
    f.save(&[post("Fetched", &["a.rar"], 2, 100)]);
    let conn = f.writer();
    let id = f.id(&conn, "Fetched");
    assert!(rules::evaluate(&conn, id, NOW).unwrap());
    assert_eq!(freeze_all(&conn).frozen, 1);
    drop(conn);
    // as a search that fetches headers saves them
    store::save(&f.main, &[post("Fetched", &["b.rar"], 1, 100)]).unwrap();
    let conn = f.writer();
    assert_eq!(count(&conn, "select count(*) from frozen.frozen_releases"), 0, "thawed");
    assert_eq!(count(&conn, "select count(*) from releases"), 1, "one copy");
    assert_eq!(count(&conn, "select parts from releases"), 3);
}

#[test]
fn a_reset_harvests_what_frozen_releases_hold() {
    let f = Fixture::new();
    f.save(&[post("Kept", &["a.rar"], 2, 100), post("Live", &["b.rar"], 1, 100)]);
    let conn = f.writer();
    let id = f.id(&conn, "Kept");
    conn.execute("insert into release_category (release_id, category) values (?1, 7020)", [id]).unwrap();
    assert!(rules::evaluate(&conn, id, NOW).unwrap());
    assert_eq!(freeze_all(&conn).frozen, 1);
    let shard = store::shard_of(G);
    let copy = unfrozen_copy(&f.main, shard).unwrap().expect("a frozen file");
    let back = db::open_shard(&copy).unwrap();
    assert_eq!(count(&back, "select id from releases"), id, "only the frozen ones");
    assert_eq!(count(&back, "select count(*) from files"), 1);
    assert_eq!(count(&back, "select category from release_category"), 7020);
    drop(back);
    remove_family(&copy).unwrap();
    crate::raw_store::harvest_from_shards(&f.main, &|_| {}).unwrap();
    assert!(!copy.exists(), "the harvest's copy is gone after");
    assert_eq!(unfrozen_copy(&f.main, (shard + 1) % store::SHARDS).unwrap(), None, "no frozen file, nothing to read");
}

#[test]
fn a_thaw_by_hand_between_the_steps_keeps_the_release_live() {
    let f = Fixture::new();
    f.save(&[post("Wanted", &["a.rar"], 2, 100)]);
    let conn = f.writer();
    let id = f.id(&conn, "Wanted");
    assert!(rules::evaluate(&conn, id, NOW).unwrap());
    let tx = conn.unchecked_transaction().unwrap();
    let (copied, dropped) = copy(&tx, &[(id, "small_complete".into())], Duration::from_secs(60), NOW).unwrap();
    tx.commit().unwrap();
    // `atlas --thaw` while the copy is committed and the live rows still there
    assert_eq!(thaw(&conn, id, true, NOW).unwrap(), Thawed::AlreadyLive);
    let tx = conn.unchecked_transaction().unwrap();
    assert_eq!(drop_copied(&tx, &copied, &dropped).unwrap(), Moved { dropped: 1, ..Moved::default() });
    tx.commit().unwrap();
    assert_eq!(count(&conn, "select count(*) from releases"), 1, "still live");
    assert_eq!(live_articles(&f.main, id).len(), 2);
    assert_eq!(count(&conn, "select count(*) from frozen.frozen_releases"), 0);
    assert_eq!(count(&conn, "select count(*) from freeze_pending"), 0);
}

#[test]
fn a_frozen_file_that_wont_read_stops_a_reset() {
    let f = Fixture::new();
    f.save(&[post("Any", &["a.rar"], 1, 100)]);
    let shard = store::shard_of(G);
    std::fs::write(path_of(&f.main, shard), b"not a database, not at all, not even close to one").unwrap();
    assert!(unfrozen_copy(&f.main, shard).is_err());
    assert!(crate::raw_store::harvest_from_shards(&f.main, &|_| {}).is_err(), "and the reset with it");
}

#[test]
fn a_post_naming_frozen_releases_thaws_only_what_its_save_joins_and_few_of_those() {
    let f = Fixture::new();
    let victims: Vec<Release> = (0..10)
        .map(|n| {
            let mut r = post(&format!("Victim.{n}"), &["a.rar"], 1, 100);
            r.poster = "owner <o@x>".into();
            r
        })
        .collect();
    f.save(&victims);
    let conn = f.writer();
    for n in 0..10 {
        let id = f.id(&conn, &format!("Victim.{n}"));
        conn.execute("insert into freeze_pending (release_id, reason, at) values (?1, 'test', 1)", [id]).unwrap();
    }
    assert_eq!(freeze_all(&conn).frozen, 10);
    let frozen = || count(&conn, "select count(*) from frozen.frozen_releases");
    let naming = |author: &str| -> Release {
        let mut r = post("Crafted", &["x.rar"], 10, 100);
        for (n, a) in r.articles.iter_mut().enumerate() {
            a.release_name = Some(format!("Victim.{n}"));
            a.author = author.into();
        }
        r
    };
    let tx = conn.unchecked_transaction().unwrap();
    let crafted = naming("someone <s@x>");
    let articles: Vec<(&str, &str)> =
        crafted.articles.iter().map(|a| (a.release_name.as_deref().unwrap(), a.author.as_str())).collect();
    assert_eq!(thaw_incoming(&tx, None, &crafted.name, &articles, G, NOW).unwrap(), 0, "not their poster");
    let theirs = naming("owner <o@x>");
    let articles: Vec<(&str, &str)> =
        theirs.articles.iter().map(|a| (a.release_name.as_deref().unwrap(), a.author.as_str())).collect();
    assert_eq!(thaw_incoming(&tx, None, &theirs.name, &articles, G, NOW).unwrap(), 8, "a few at most");
    tx.commit().unwrap();
    assert_eq!(frozen(), 2);
    assert_eq!(count(&conn, "select count(*) from freeze_pending"), 8, "queued again, to go back after their wait");
}

#[test]
fn the_backlog_walk_queues_what_no_visit_comes_back_to_a_chunk_at_a_time() {
    let f = Fixture::new();
    let names: Vec<String> = (0..5).map(|n| format!("Old.{n}")).collect();
    f.save(&names.iter().map(|n| post(n, &["a.rar"], 1, 100)).collect::<Vec<_>>());
    f.save(&[post("Big", &["b.mkv"], 2, 600 << 20)]);
    let conn = f.writer();
    let queued_at = |n: &str| -> Option<i64> {
        conn.query_row("select at from freeze_pending where release_id = ?1", [f.id(&conn, n)], |r| r.get(0))
            .optional()
            .unwrap()
    };
    assert!(rules::evaluate(&conn, f.id(&conn, "Old.0"), 7).unwrap(), "a visit queued it earlier");

    let step = || rules::backlog_some(&conn, 2, NOW).unwrap();
    // newest first: Big (too big), then Old.4 and Old.3
    assert_eq!(step(), rules::Backlog { looked_at: 2, queued: 1 });
    assert_eq!(queued_at("Old.4"), Some(NOW));
    assert_eq!(queued_at("Old.2"), None);
    assert_eq!(step(), rules::Backlog { looked_at: 2, queued: 2 });
    assert_eq!(step(), rules::Backlog { looked_at: 2, queued: 1 });
    assert_eq!(queued_at("Old.0"), Some(7), "one queued already keeps its place");
    assert_eq!(queued_at("Big"), None);
    assert_eq!(step(), rules::Backlog::default(), "at the bottom");
    assert_eq!(meta(&conn, "freeze_backlog_below"), 0);
    assert_eq!(step(), rules::Backlog::default(), "and done");

    // the rules changed since (as a walk made under other ones has them): it starts over from the top
    conn.execute("delete from freeze_pending", []).unwrap();
    conn.execute("update meta set value = value + 1 where key = 'freeze_backlog_rules'", []).unwrap();
    assert_eq!(step(), rules::Backlog { looked_at: 2, queued: 1 });
    assert_eq!(queued_at("Old.4"), Some(NOW));
}
