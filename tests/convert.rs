//! Converting a database from before the shards: every release and article
//! moves into its group's shard, NZBs and stats come out the same, cursors
//! carry over, the old file is kept aside, and saving afterwards continues
//! with ids after the old ones.

use std::path::Path;

use atlas::{convert, db, nzb, store};
use rusqlite::Connection;

/// A database in the old layout: everything in atlas.db.
fn old_database(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "
        pragma journal_mode = wal;
        create table releases (
            id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, group_name TEXT, poster TEXT, posted_date TEXT,
            size INTEGER, complete INTEGER, parts INTEGER, file_total INTEGER, display_name TEXT,
            is_obfuscated INTEGER default 0
        );
        create table articles (
            id INTEGER PRIMARY KEY AUTOINCREMENT, release_id INTEGER, message_id TEXT, subject TEXT, filename TEXT,
            part INTEGER, total_parts INTEGER, bytes INTEGER, file_total INTEGER,
            unique(release_id, message_id)
        );
        create table groups(name TEXT PRIMARY KEY, live_cursor INTEGER, backfill_cursor INTEGER,
            first_article INTEGER, last_article INTEGER);
        insert into groups values ('alt.binaries.a@news.x', 900, 500, 1, 1000), ('alt.binaries.b', 70, 10, 5, 80);
        ",
    )
    .unwrap();

    let groups = ["alt.binaries.a", "alt.binaries.b", "alt.binaries.c", "alt.binaries.d"];
    let mut article = conn
        .prepare(
            "insert into articles (release_id, message_id, subject, filename, part, total_parts, bytes, file_total)
             values (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .unwrap();
    for r in 1..=40i64 {
        let group = groups[(r % 4) as usize];
        conn.execute(
            "insert into releases (id, name, group_name, poster, posted_date, size, complete, parts, file_total,
                display_name, is_obfuscated) values (?, ?, ?, 'poster <p@x>', '2026-10-02 10:11:12', ?, ?, ?, 2, ?, ?)",
            rusqlite::params![
                r * 3, // gaps in the ids, like a real database
                format!("Release.{r}"),
                group,
                r * 1000,
                r % 2,
                r % 7 + 1,
                (r % 5 == 0).then(|| format!("Real.Name.{r}")),
                (r % 3 == 0) as i64
            ],
        )
        .unwrap();
        for n in 1..=(r % 7 + 1) {
            let file = if n == 3 && r % 6 == 0 { None } else { Some(format!("file{}.rar", n % 2)) };
            // hex ids at a shared domain, odd ones, and two articles sharing a part number
            let message_id = match n {
                1 => format!("<{:032x}@ngPost>", r * 100 + n),
                2 => format!("<Odd-{r}-{n}@JBinUp.local>"),
                _ => format!("<x{r}y{n}@nyuu>"),
            };
            let part = if n == 4 {
                Some(1)
            } else if n == 5 {
                None
            } else {
                Some(n)
            };
            article
                .execute(rusqlite::params![
                    r * 3,
                    message_id,
                    format!("\"file{}.rar\" yEnc ({n}/7)", n % 2),
                    file,
                    part,
                    7,
                    100 + n,
                    2
                ])
                .unwrap();
        }
    }
    // an article whose release is gone
    article.execute(rusqlite::params![9999, "<orphan@x>", "s", "f", 1, 1, 1, 1]).unwrap();
}

/// every release's NZB from the old database, by release name
/// (name, nzb, (size, complete, parts))
type OldRelease = (String, String, (Option<i64>, bool, Option<i64>));

fn old_nzbs(path: &Path) -> Vec<OldRelease> {
    let conn = Connection::open(path).unwrap();
    let mut releases = conn
        .prepare("select id, name, group_name, poster, posted_date, size, complete, parts from releases order by id")
        .unwrap();
    let rows: Vec<atlas::search::ReleaseRow> = releases
        .query_map([], |r| {
            Ok(atlas::search::ReleaseRow {
                id: r.get(0)?,
                name: r.get(1)?,
                group_name: r.get(2)?,
                poster: r.get(3)?,
                posted_date: r.get(4)?,
                size: r.get(5)?,
                complete: r.get::<_, i64>(6)? != 0,
                parts: r.get(7)?,
            })
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut articles = conn
        .prepare(
            "select articles.message_id, articles.filename, articles.part, articles.total_parts, articles.bytes,
                articles.subject, releases.poster, releases.posted_date
             from articles join releases on articles.release_id = releases.id where articles.release_id = ?",
        )
        .unwrap();
    rows.into_iter()
        .map(|release| {
            let mut list: Vec<atlas::search::ArticleRow> = articles
                .query_map([release.id], |r| {
                    Ok(atlas::search::ArticleRow {
                        message_id: r.get(0)?,
                        filename: r.get(1)?,
                        part: r.get(2)?,
                        total_parts: r.get(3)?,
                        bytes: r.get(4)?,
                        subject: r.get(5)?,
                        poster: r.get(6)?,
                        posted_date: r.get(7)?,
                    })
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            list.sort_by(|a, b| (&a.filename, a.part, &a.message_id).cmp(&(&b.filename, b.part, &b.message_id)));
            (release.name.clone(), nzb::render_nzb(&release, &list), (release.size, release.complete, release.parts))
        })
        .collect()
}

#[test]
fn old_databases_convert_without_losing_anything() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    old_database(&main);
    let before = old_nzbs(&main);

    // opening it (the menu does) leaves it alone for the conversion
    db::create_db_at(&main).unwrap();
    assert!(convert::needed(&main));
    assert!(!store::exists(&main));

    let messages = std::cell::RefCell::new(Vec::new());
    let (releases, articles) = convert::run(&main, &|m| messages.borrow_mut().push(m.to_string())).unwrap();
    assert_eq!(releases, 40);
    assert_eq!(articles, (1..=40).map(|r| r % 7 + 1).sum::<i64>());
    assert!(messages.borrow().last().unwrap().contains("1 articles without a release left out"), "{messages:?}");

    assert!(!convert::needed(&main), "converted");
    assert!(store::exists(&main));
    assert!(dir.path().join("atlas.old.db").exists(), "the old database is kept aside");
    for shard in store::shard_paths(&main) {
        let mode: String = db::open_at(&shard).unwrap().query_row("pragma journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode, "wal", "shards are back on WAL after the fast fill");
    }
    db::create_db_at(&main).unwrap();

    // every release came over with the same NZB and stats, in its group's shard
    let conn = db::open_with_shards(&main).unwrap();
    for (name, nzb_before, stats_before) in &before {
        let (id, group): (i64, String) = conn
            .query_row("select id, group_name from releases where name = ?", [name], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(store::shard_of_id(id), store::shard_of(&group));
        let now = atlas::search::get_release_with(&conn, id).unwrap().unwrap();
        assert_eq!((now.size, now.complete, now.parts), *stats_before, "{name}");
        let nzb_after = nzb::render_nzb(&now, &store::articles(&conn, id).unwrap());
        assert_eq!(&nzb_after, nzb_before, "{name}");
    }
    // ids keep the old order: old id * 8 + shard
    let first: i64 = conn.query_row("select id from releases where name = 'Release.1'", [], |r| r.get(0)).unwrap();
    assert_eq!(first / store::SHARDS as i64, 3);

    // cursors carried over
    let cursor: (i64, i64, Option<i64>) = conn
        .query_row(
            "select live_cursor, backfill_cursor, last_article from groups where name = 'alt.binaries.a@news.x'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(cursor, (900, 500, Some(1000)));
    assert_eq!(store::totals(&conn).unwrap(), (40, articles));
    drop(conn);

    // new releases get ids after every old one
    let new = atlas::parser::Release {
        name: "Brand.New".into(),
        group: "alt.binaries.a".into(),
        articles: vec![atlas::parser::Article {
            message_id: "<new@x>".into(),
            filename: Some("n.bin".into()),
            part: Some(1),
            total_parts: Some(1),
            bytes: 5,
            ..Default::default()
        }],
        ..Default::default()
    };
    store::save(&main, &[new]).unwrap();
    let conn = db::open_with_shards(&main).unwrap();
    let newest: String =
        conn.query_row("select name from releases order by id desc limit 1", [], |r| r.get(0)).unwrap();
    assert_eq!(newest, "Brand.New");
}

#[test]
fn a_conversion_stopped_during_the_check_picks_up_there() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    old_database(&main);
    let before = old_nzbs(&main);
    db::create_db_at(&main).unwrap();

    // stopped as the check starts: the copy is done, nothing swapped
    let stopped = std::panic::catch_unwind(|| {
        convert::run(&main, &|m| assert!(!m.contains("checking NZBs"), "stop here")).unwrap();
    });
    assert!(stopped.is_err());
    assert!(convert::needed(&main), "the old database is still in place");

    let messages = std::cell::RefCell::new(Vec::new());
    convert::run(&main, &|m| messages.borrow_mut().push(m.to_string())).unwrap();
    assert!(messages.borrow().iter().any(|m| m.contains("copy finished earlier")), "{messages:?}");
    assert!(
        !messages.borrow().iter().any(|m| m.contains("sorting") || m.contains("% (")),
        "nothing copied again: {messages:?}"
    );
    assert!(!convert::needed(&main));

    let conn = db::open_with_shards(&main).unwrap();
    for (name, nzb_before, _) in &before {
        let id: i64 = conn.query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap();
        let now = atlas::search::get_release_with(&conn, id).unwrap().unwrap();
        assert_eq!(&nzb::render_nzb(&now, &store::articles(&conn, id).unwrap()), nzb_before, "{name}");
    }
}

/// `--convert` has the database to itself for its whole run: refused while
/// the indexer (or a save) holds it, and a second conversion is refused
/// while one runs, each without touching anything.
#[test]
fn a_conversion_runs_alone() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    old_database(&main);
    db::create_db_at(&main).unwrap();
    let new_main = dir.path().join("atlas.new.db");

    let writing = atlas::compact::try_hold_off_compaction(&main).unwrap().unwrap();
    let err = convert::run_alone(&main, &|_| {}).expect_err("converted while the indexer held the database");
    assert!(format!("{err:#}").contains("in use"), "{err:#}");
    assert!(convert::needed(&main) && !new_main.exists(), "nothing was done");
    drop(writing);

    let second = std::cell::RefCell::new(None);
    let converted = convert::run_alone(&main, &|_| {
        if second.borrow().is_none() {
            *second.borrow_mut() = Some(convert::run_alone(&main, &|_| {}).map(|_| ()));
        }
    })
    .unwrap();
    assert_eq!(converted.map(|(releases, _)| releases), Some(40));
    let err = second.into_inner().unwrap().expect_err("a second conversion ran alongside");
    assert!(format!("{err:#}").contains("in use"), "{err:#}");
    assert!(!convert::needed(&main), "the first one finished");
    assert_eq!(convert::run_alone(&main, &|_| {}).unwrap(), None, "nothing left to convert");
}
