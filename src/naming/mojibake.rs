//! `atlas --undo-mojibake`: rows named from a subject that a server sent as UTF-8 read as cp1252 and encoded again
//! (`CultFilmsâ„¢` for `CultFilms™`), saved before `nntp::undo_mojibake` read overviews right. The same articles
//! from the other servers made a row under the right name: one that holds nothing that row doesnt goes, and its
//! collection points there. With no row under the right name, the row takes it. One that holds articles the other
//! doesnt is left as it is. A dry run unless `--apply`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use rusqlite::{Connection, OptionalExtension, params};

use super::walk::{self, BATCH, WalkLimits};
use crate::nntp::undo_mojibake;
use crate::{db, store};

const CURSOR: &str = "undo_mojibake_after";

/// What becomes of a row.
#[derive(Debug, PartialEq, Eq)]
enum Fix {
    /// its articles are all in this row, under the right name: it goes
    Fold { into: i64 },
    /// no row has the right name: it takes it
    Rename { to: String },
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
    let Some(other) = other else { return Ok(Some(Fix::Rename { to: right })) };
    let mine = store::release_article_keys(conn, id)?;
    let theirs = store::release_article_keys(conn, other)?;
    Ok(Some(if mine.is_subset(&theirs) { Fix::Fold { into: other } } else { Fix::Leave }))
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
        Fix::Rename { to } => {
            tx.prepare_cached("update releases set name = ? where id = ?")?.execute(params![to, id])?;
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
    use crate::parser::{Article, Release};

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
        assert_eq!(
            fix(&conn, cafe, "Caf\u{c3}\u{a9}", "alt.binaries.t").unwrap(),
            Some(Fix::Rename { to: "Caf\u{e9}".into() })
        );
        assert_eq!(fix(&conn, plain, "Caf\u{e9} plain", "alt.binaries.t").unwrap(), None);
        assert_eq!(fix(&conn, film, "Film\u{2122}", "alt.binaries.t").unwrap(), None);

        let articles = |c: &Connection| -> i64 {
            c.query_row("select value from meta where key = 'articles'", [], |r| r.get(0)).unwrap()
        };
        let before = articles(&conn);
        let tx = conn.transaction().unwrap();
        apply(&tx, film_twice, &Fix::Fold { into: film }).unwrap();
        apply(&tx, cafe, &Fix::Rename { to: "Caf\u{e9}".into() }).unwrap();
        tx.commit().unwrap();
        let gone: i64 =
            conn.query_row("select count(*) from releases where id = ?", [film_twice], |r| r.get(0)).unwrap();
        assert_eq!(gone, 0);
        assert_eq!(articles(&conn), before - 2, "its two articles came off the shard's count");
        assert_eq!(id(&conn, "Caf\u{e9}"), cafe);
    }
}
