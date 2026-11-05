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

/// a post of `files`, each `parts` articles of `bytes`
fn post(name: &str, files: &[&str], parts: i64, bytes: i64) -> Release {
    let articles = files.iter().flat_map(|f| (1..=parts).map(move |n| article(f, n, parts, bytes))).collect();
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
    freeze_some(conn, 1_000, Duration::from_secs(60), NOW).unwrap()
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
    let hits = search(&f.main, "small", None, 10).unwrap();
    assert_eq!(hits.iter().map(|h| h.row.id).collect::<Vec<_>>(), vec![id]);
    assert!(search(&f.main, "nothing", None, 10).unwrap().is_empty());
    let (row, nzb_body) = nzb(&f.main, id).unwrap().unwrap();
    assert_eq!(row.name, "Small.Thing");
    assert_eq!(nzb_body, crate::nzb::render_nzb(&row, &articles));

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
    f.save(&[half]);
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
    let mut first = post("Late.Post", &["a.rar"], 3, 100);
    first.articles.truncate(2);
    first.complete = false;
    f.save(&[first]);
    let conn = f.writer();
    let id = f.id(&conn, "Late.Post");
    conn.execute("insert into freeze_pending (release_id, reason, at) values (?1, 'test', 1)", [id]).unwrap();
    assert_eq!(freeze_all(&conn).frozen, 1);
    assert_eq!(count(&conn, "select count(*) from releases"), 0);
    drop(conn);

    let mut last = post("Late.Post", &["a.rar"], 3, 100);
    last.articles.drain(..2);
    f.save(&[last]);
    let conn = f.writer();
    assert_eq!(count(&conn, "select count(*) from frozen.frozen_releases"), 0, "thawed");
    assert_eq!(f.id(&conn, "Late.Post"), id, "the same release");
    assert_eq!(count(&conn, "select parts from releases"), 3);
    assert_eq!(count(&conn, "select complete from releases"), 1, "complete with what came late");
    assert_eq!(live_articles(&f.main, id).len(), 3);
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
    restore(&conn, "main", &collect_frozen(&conn, id)).unwrap();
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
        search(&f.main, "some", filter, 10).unwrap().into_iter().map(|h| h.row.name).collect()
    };
    assert_eq!(names(Some(&books)), vec!["Some.Book".to_string()]);
    assert_eq!(
        names(Some(&crate::category::CatFilter { leaves: vec![], uncategorized: true })),
        vec!["Some.Song".to_string()]
    );
    assert_eq!(names(None).len(), 2);
    assert_eq!(search(&f.main, "", None, 1).unwrap().len(), 1, "newest first, the limit holds");
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
fn a_shard_without_its_frozen_file_saves_as_before() {
    let f = Fixture::new();
    store::save(&f.main, &[post("Plain", &["a.rar"], 1, 10)]).unwrap();
    let conn = db::open_shard(&f.shard_path()).unwrap();
    assert!(!attached(&conn).unwrap());
    assert!(!path(&f.shard_path()).exists(), "only the indexer's writer makes the frozen file");
    assert_eq!(count(&conn, "select count(*) from releases"), 1);
}
