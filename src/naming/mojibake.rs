//! `atlas --undo-mojibake`: rows named from a subject that a server sent as UTF-8 read as cp1252 and encoded again
//! (`CultFilmsâ„¢` for `CultFilms™`), saved before `nntp::undo_mojibake` read overviews right. The same articles
//! from the other servers made a row under the right name: one that holds nothing that row doesnt goes, and its
//! collection points there. With no row under the right name, the row takes it. One that holds articles the other
//! doesnt is left as it is. A dry run unless `--apply`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};

use super::walk::{self, BATCH, WalkLimits};
use crate::nntp::undo_mojibake;
use crate::parser::Article;
use crate::{db, store};

const CURSOR: &str = "undo_mojibake_after";

/// What becomes of a row.
#[derive(Debug, PartialEq, Eq)]
enum Fix {
    /// its articles are all in this row, under the right name: it goes
    Fold { into: i64 },
    /// no row has the right name: it takes it, and the collections its files make read right point at it, soo the
    /// post's later articles (read right) join it
    Rename { to: String, collections: Vec<String>, files: Vec<(i64, String, Option<String>)> },
    /// another row has the right name, and this one holds articles it doesnt
    Leave,
}

/// A batch of `(id, (name, group))` past `after`, every row: the walk goes on past those with nothing to do.
fn batch(conn: &Connection, after: i64) -> rusqlite::Result<Vec<(i64, (String, String))>> {
    conn.prepare_cached("select id, name, group_name from releases where id > ? order by id limit ?")?
        .query_map(params![after, BATCH], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
        .collect()
}

fn fix(conn: &Connection, id: i64, name: &str, group: &str) -> rusqlite::Result<Option<Fix>> {
    if name.is_ascii() {
        return Ok(None);
    }
    let right = undo_mojibake(name.to_string());
    if right == name {
        return Ok(None);
    }
    let other: Option<i64> = conn
        .prepare_cached("select id from releases where name = ? and group_name = ? and id != ?")?
        .query_row(params![right, group, id], |r| r.get(0))
        .optional()?;
    // a frozen release may have the right name: it would be taken from it, and it couldnt come back
    if crate::frozen::attached(conn)? {
        let frozen: bool = conn
            .prepare_cached("select exists (select 1 from frozen.frozen_releases where name = ? and group_name = ?)")?
            .query_row(params![right, group], |r| r.get(0))?;
        if frozen && other.is_none() {
            return Ok(Some(Fix::Leave));
        }
    }
    let Some(other) = other else {
        // its files take their names read right too, or the post's later articles start other files: not when that
        // would meet a file it has under the right name already
        let files = right_files(conn, id)?;
        let mut names: HashSet<&str> = HashSet::new();
        let taken: HashSet<String> = conn
            .prepare_cached("select filename from files where release_id = ?")?
            .query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if files.iter().any(|(_, old, new, _)| (old != new && taken.contains(new)) || !names.insert(new.as_str())) {
            return Ok(Some(Fix::Leave));
        }
        let files =
            files.into_iter().filter(|(_, old, new, _)| old != new).map(|(f, _, new, subject)| (f, new, subject));
        return Ok(Some(Fix::Rename {
            to: right,
            collections: right_collections(conn, id, group)?,
            files: files.collect(),
        }));
    };
    let mine = store::release_article_keys(conn, id)?;
    let theirs = store::release_article_keys(conn, other)?;
    Ok(Some(if mine.is_subset(&theirs) { Fix::Fold { into: other } } else { Fix::Leave }))
}

/// A file of a row: its id, its name, its name read right and its subject read right.
type RightFile = (i64, String, String, Option<String>);

/// Row `id`'s files, read right.
fn right_files(conn: &Connection, id: i64) -> rusqlite::Result<Vec<RightFile>> {
    conn.prepare_cached("select id, filename, subject from files where release_id = ?")?
        .query_map([id], |r| {
            let (file, name, subject): (i64, String, Option<String>) = (r.get(0)?, r.get(1)?, r.get(2)?);
            Ok((file, name.clone(), undo_mojibake(name), subject.map(undo_mojibake)))
        })?
        .collect()
}

/// The collections row `id`'s files make of their subjects with the server's encoding undone (`Collections::of`, the
/// row's poster and day standing in for each article's).
fn right_collections(conn: &Connection, id: i64, group: &str) -> rusqlite::Result<Vec<String>> {
    let (poster, date): (Option<String>, Option<String>) = conn
        .prepare_cached("select poster, posted_date from releases where id = ?")?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let author = undo_mojibake(poster.unwrap_or_default());
    let date = date.unwrap_or_default();
    let articles: Vec<Article> = conn
        .prepare_cached("select distinct subject from files where release_id = ? and subject is not null")?
        .query_map([id], |r| r.get::<_, String>(0))?
        .map(|subject| {
            Ok(Article {
                message_id: "<recollect@atlas>".into(),
                subject: undo_mojibake(subject?),
                author: author.clone(),
                date: date.clone(),
                ..Default::default()
            })
        })
        .collect::<rusqlite::Result<_>>()?;
    let mut out: Vec<String> =
        crate::collections::group_articles(group, articles).into_values().filter_map(|r| r.collection).collect();
    out.sort();
    out.dedup();
    Ok(out)
}

fn apply(tx: &Connection, id: i64, fix: &Fix) -> rusqlite::Result<()> {
    match fix {
        Fix::Fold { into } => {
            tx.prepare_cached("update collections set release_id = ? where release_id = ?")?
                .execute(params![into, id])?;
            let deleted = store::delete_release(tx, id)?;
            let mut less = tx.prepare_cached("update meta set value = max(value - ?, 0) where key = ?")?;
            less.execute(params![deleted.articles, "articles"])?;
            less.execute(params![1, "releases"])?;
        }
        Fix::Rename { to, collections, files } => {
            tx.prepare_cached("update releases set name = ? where id = ?")?.execute(params![to, id])?;
            let mut file =
                tx.prepare_cached("update files set filename = ?, subject = coalesce(?, subject) where id = ?")?;
            for (f, name, subject) in files {
                file.execute(params![name, subject, f])?;
            }
            let key = store::IdKey::of(tx)?;
            let mut put = tx.prepare_cached("insert or ignore into collections (key, release_id) values (?, ?)")?;
            for c in collections {
                put.execute(params![key.hash(c), id])?;
            }
        }
        Fix::Leave => {}
    }
    Ok(())
}

#[derive(Default)]
struct Counts {
    folded: u64,
    renamed: u64,
    left: u64,
}

impl Counts {
    fn add(&mut self, fix: &Fix) {
        match fix {
            Fix::Fold { .. } => self.folded += 1,
            Fix::Rename { .. } => self.renamed += 1,
            Fix::Leave => self.left += 1,
        }
    }
}

fn dry_run(main: &Path, sample: usize) -> anyhow::Result<()> {
    let (mut counts, mut shown) = (Counts::default(), Vec::new());
    for path in store::shard_paths(main) {
        let conn = walk::open_for_reading(&path)?;
        let frozen = crate::frozen::path(&path);
        if frozen.exists() {
            conn.execute("attach database ?1 as frozen", [format!("file:{}?mode=ro", frozen.display())])?;
        }
        let mut after = i64::MIN;
        loop {
            let rows = batch(&conn, after)?;
            let Some((last, _)) = rows.last() else { break };
            after = *last;
            for (id, (name, group)) in rows {
                if let Some(f) = fix(&conn, id, &name, &group)? {
                    counts.add(&f);
                    if shown.len() < sample {
                        shown.push(format!("{id} {name}: {f:?}"));
                    }
                }
            }
        }
    }
    println!(
        "{} rows would go into the row with the right name, {} would be renamed, {} hold other articles and stay",
        counts.folded, counts.renamed, counts.left
    );
    for line in shown {
        println!("  {line}");
    }
    println!("a dry run: nothing was changed. --apply does it");
    Ok(())
}

fn apply_shard(
    path: &Path,
    limits: &WalkLimits,
    stop: &AtomicBool,
    commits: &AtomicU64,
    rows: &AtomicU64,
    done: &AtomicU64,
) -> anyhow::Result<()> {
    let mut conn = db::open_shard(path)?;
    store::migrate_shard(&conn)?;
    db::tune_for_writing(&conn)?;
    crate::frozen::attach(&conn, path)?;
    let mut walk = walk::Walk {
        conn: &mut conn,
        path,
        key: CURSOR,
        limits,
        stop,
        commits,
        rows,
        label: "mojibake",
        shard_commits: 0,
    };
    let write = |tx: &Connection, batch: &[(i64, (String, String))]| -> rusqlite::Result<()> {
        for (id, (name, group)) in batch {
            if let Some(f) = fix(tx, *id, name, group)?.filter(|f| *f != Fix::Leave) {
                apply(tx, *id, &f)?;
                done.fetch_add(1, Relaxed);
            }
        }
        Ok(())
    };
    while walk.step(batch, write)? {}
    walk::finish_shard(&conn, "key = 'undo_mojibake_after'")
}

/// `atlas --undo-mojibake [--sample N] [--apply]`. Exit 0 on success, 1 when refused or failed.
pub fn run(args: &[String]) -> i32 {
    let o = match walk::parse(args, "--undo-mojibake") {
        Ok(o) => o,
        Err(e) => {
            println!("{e}");
            return 1;
        }
    };
    let main = walk::main_path();
    if !o.apply {
        if let Err(e) = walk::may_read(&main) {
            println!("{e}");
            return 1;
        }
        return match dry_run(&main, o.sample) {
            Ok(()) => 0,
            Err(e) => {
                println!("{e:#}");
                1
            }
        };
    }
    let _lock = match walk::take_lock(&main) {
        Ok(lock) => lock,
        Err(e) => {
            println!("{e}");
            return 1;
        }
    };
    let limits = walk::limits("ATLAS_MOJIBAKE");
    let stop = walk::stop_flag();
    let (commits, rows, done) = (AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0));
    let result = walk::each_shard(&main, |path| apply_shard(path, &limits, &stop, &commits, &rows, &done));
    println!("{} rows fixed", done.load(Relaxed));
    match result {
        Ok(()) => 0,
        Err(e) => {
            println!("{e:#}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Release;

    fn art(id: &str, file: &str, part: i64) -> Article {
        Article {
            message_id: id.into(),
            subject: format!("\"{file}\" yEnc ({part}/2)"),
            filename: Some(file.into()),
            part: Some(part),
            total_parts: Some(2),
            bytes: 100,
            ..Default::default()
        }
    }

    fn rel(name: &str, articles: Vec<Article>) -> Release {
        Release { name: name.into(), group: "alt.binaries.t".into(), articles, ..Default::default() }
    }

    /// A row a server's double-encoded subject made goes into the row of the same articles under the right name,
    /// one with articles of its own stays, and one with no row under the right name takes it.
    #[test]
    fn each_double_encoded_row_folds_renames_or_stays() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let both = vec![art("<1@x>", "a.rar", 1), art("<2@x>", "a.rar", 2)];
        store::save(
            &main,
            &[
                rel("Film\u{2122}", both.clone()),
                rel("Film\u{e2}\u{201e}\u{a2}", both),
                rel("Show\u{2122}", vec![art("<3@x>", "s.rar", 1)]),
                rel("Show\u{e2}\u{201e}\u{a2}", vec![art("<4@x>", "s.rar", 2)]),
                rel("Caf\u{c3}\u{a9}", vec![art("<5@x>", "c.rar", 1)]),
                rel("Caf\u{e9} plain", vec![art("<6@x>", "d.rar", 1)]),
            ],
        )
        .unwrap();
        let mut conn = db::open_at(&store::shard_path(&main, store::shard_of("alt.binaries.t"))).unwrap();
        let id = |c: &Connection, name: &str| -> i64 {
            c.query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap()
        };
        let film = id(&conn, "Film\u{2122}");
        let film_twice = id(&conn, "Film\u{e2}\u{201e}\u{a2}");
        let show_twice = id(&conn, "Show\u{e2}\u{201e}\u{a2}");
        let cafe = id(&conn, "Caf\u{c3}\u{a9}");
        let plain = id(&conn, "Caf\u{e9} plain");
        assert_eq!(
            fix(&conn, film_twice, "Film\u{e2}\u{201e}\u{a2}", "alt.binaries.t").unwrap(),
            Some(Fix::Fold { into: film })
        );
        assert_eq!(fix(&conn, show_twice, "Show\u{e2}\u{201e}\u{a2}", "alt.binaries.t").unwrap(), Some(Fix::Leave));
        assert!(matches!(
            fix(&conn, cafe, "Caf\u{c3}\u{a9}", "alt.binaries.t").unwrap(),
            Some(Fix::Rename { to, .. }) if to == "Caf\u{e9}"
        ));
        assert_eq!(fix(&conn, plain, "Caf\u{e9} plain", "alt.binaries.t").unwrap(), None);
        assert_eq!(fix(&conn, film, "Film\u{2122}", "alt.binaries.t").unwrap(), None);

        // a frozen release with the right name keeps it
        let shard = store::shard_path(&main, store::shard_of("alt.binaries.t"));
        crate::frozen::attach(&conn, &shard).unwrap();
        conn.execute(
            "insert into frozen.frozen_releases (id, name, group_name, reason, frozen_at, payload)
             values (99, 'Caf\u{e9}', 'alt.binaries.t', 'small_complete', 0, x'00')",
            [],
        )
        .unwrap();
        assert_eq!(fix(&conn, cafe, "Caf\u{c3}\u{a9}", "alt.binaries.t").unwrap(), Some(Fix::Leave));
        conn.execute("delete from frozen.frozen_releases", []).unwrap();

        let articles = |c: &Connection| -> i64 {
            c.query_row("select value from meta where key = 'articles'", [], |r| r.get(0)).unwrap()
        };
        let before = articles(&conn);
        let tx = conn.transaction().unwrap();
        apply(&tx, film_twice, &Fix::Fold { into: film }).unwrap();
        apply(&tx, cafe, &Fix::Rename { to: "Caf\u{e9}".into(), collections: Vec::new(), files: Vec::new() }).unwrap();
        tx.commit().unwrap();
        let gone: i64 =
            conn.query_row("select count(*) from releases where id = ?", [film_twice], |r| r.get(0)).unwrap();
        assert_eq!(gone, 0);
        assert_eq!(articles(&conn), before - 2, "its two articles came off the shard's count");
        assert_eq!(id(&conn, "Caf\u{e9}"), cafe);
    }

    /// A renamed row of a post still coming in: the post's later articles, read right, find it by their collection.
    #[test]
    fn a_renamed_row_takes_the_rest_of_its_post() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let group = "alt.binaries.t";
        let article = |id: &str, n: i64, part: i64, name: &str| Article {
            message_id: id.into(),
            subject: format!("[{n}/2] \"{name} Long Film Title 2020 1080p.part{n}.rar\" yEnc ({part}/2)"),
            author: "poster@example.com".into(),
            date: "2026-10-26 04:18:58".into(),
            bytes: 100,
            ..Default::default()
        };
        let releases = |articles| crate::collections::group_articles(group, articles).into_values().collect::<Vec<_>>();
        // the first file as the server encoded it twice, before overviews were read right
        let twice = releases(vec![article("<1@x>", 1, 1, "Caf\u{c3}\u{a9}")]);
        assert!(twice[0].collection.is_some());
        store::save(&main, &twice).unwrap();
        let conn = db::open_at(&store::shard_path(&main, store::shard_of(group))).unwrap();
        let (id, name): (i64, String) =
            conn.query_row("select id, name from releases", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        let f = fix(&conn, id, &name, group).unwrap().unwrap();
        assert!(matches!(&f, Fix::Rename { collections, .. } if collections.len() == 1), "{f:?}");
        apply(&conn, id, &f).unwrap();
        // the rest of the first file and the second, read right
        store::save(
            &main,
            &releases(vec![
                article("<2@x>", 1, 2, "Caf\u{e9}"),
                article("<3@x>", 2, 1, "Caf\u{e9}"),
                article("<4@x>", 2, 2, "Caf\u{e9}"),
            ]),
        )
        .unwrap();
        let rows: Vec<(i64, i64)> = conn
            .prepare("select id, (select count(*) from files where release_id = releases.id) from releases")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows, vec![(id, 2)], "one release, both files");
        let complete: bool = conn.query_row("select complete from releases", [], |r| r.get(0)).unwrap();
        assert!(complete, "its first file has both parts, under its name read right");
    }
}
