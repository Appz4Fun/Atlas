//! `atlas --recount-articles --since UNIX_TIME`: size and parts of rows saved while a build counted a part saved again
//! under another message-id once (26f0497, #242). Different files can share a file row, soo such a part isnt known to
//! be a repost, and those saves left size and parts short. Every row with a file saved since `--since` gets them back
//! as the sum and count of every article it holds, what saves count. A dry run unless `--apply`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use rusqlite::{Connection, params};

use super::walk::{self, BATCH, WalkLimits};
use crate::{db, store};

const CURSOR: &str = "recount_articles_after";

/// A batch of `(id, (size, parts))` past `after`: every row comes back, soo the walk goes on past rows with nothing
/// to do.
fn batch(conn: &Connection, after: i64) -> rusqlite::Result<Vec<(i64, (i64, i64))>> {
    conn.prepare_cached("select id, size, parts from releases where id > ? order by id limit ?")?
        .query_map(params![after, BATCH], |r| {
            Ok((r.get(0)?, (r.get::<_, Option<i64>>(1)?.unwrap_or(0), r.get::<_, Option<i64>>(2)?.unwrap_or(0))))
        })?
        .collect()
}

/// What a row saved since `since` should have: `(size, parts)`, when that's not what it has.
fn short(conn: &Connection, id: i64, size: i64, parts: i64, since: i64) -> rusqlite::Result<Option<(i64, i64)>> {
    let saved: bool = conn
        .prepare_cached("select exists (select 1 from files where release_id = ? and touched_at >= ?)")?
        .query_row(params![id, since], |r| r.get(0))?;
    if !saved {
        return Ok(None);
    }
    Ok(store::article_totals(conn, id)?.filter(|t| *t != (size, parts)))
}

fn dry_run(main: &Path, since: i64, sample: usize) -> anyhow::Result<()> {
    let (mut rows, mut bytes, mut articles, mut shown) = (0u64, 0i64, 0i64, Vec::new());
    for path in store::shard_paths(main) {
        let conn = walk::open_for_reading(&path)?;
        let mut after = i64::MIN;
        loop {
            let ids = batch(&conn, after)?;
            let Some((last, _)) = ids.last() else { break };
            after = *last;
            for (id, (size, parts)) in ids {
                if let Some((s, p)) = short(&conn, id, size, parts, since)? {
                    rows += 1;
                    bytes += s - size;
                    articles += p - parts;
                    if shown.len() < sample {
                        shown.push(format!("{id}: size {size} -> {s}, parts {parts} -> {p}"));
                    }
                }
            }
        }
    }
    println!("{rows} rows would change by {articles} articles and {bytes} bytes in all");
    for line in shown {
        println!("  {line}");
    }
    println!("a dry run: nothing was changed. --apply sets them");
    Ok(())
}

fn apply_shard(
    path: &Path,
    since: i64,
    limits: &WalkLimits,
    stop: &AtomicBool,
    commits: &AtomicU64,
    rows: &AtomicU64,
    fixed: &AtomicU64,
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
        label: "recount",
        shard_commits: 0,
    };
    let write = |tx: &Connection, ids: &[(i64, (i64, i64))]| -> rusqlite::Result<()> {
        let mut update = tx.prepare_cached("update releases set size = ?, parts = ? where id = ?")?;
        for (id, (size, parts)) in ids {
            if let Some((s, p)) = short(tx, *id, *size, *parts, since)? {
                update.execute(params![s, p, id])?;
                fixed.fetch_add(1, Relaxed);
            }
        }
        Ok(())
    };
    while walk.step(batch, write)? {}
    walk::finish_shard(&conn, "key = 'recount_articles_after'")
}

/// `--since UNIX_TIME` out of the arguments, and the rest for `walk::parse`.
fn since(args: &[String]) -> Result<(i64, Vec<String>), String> {
    let mut rest = Vec::new();
    let mut since = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--since" {
            let v = it.next().ok_or("--since needs a unix time")?;
            since = Some(v.parse::<i64>().map_err(|_| format!("--since needs a unix time, not {v}"))?);
        } else {
            rest.push(a.clone());
        }
    }
    Ok((since.ok_or("--since UNIX_TIME is needed: rows with a file saved since then are recounted")?, rest))
}

/// `atlas --recount-articles --since UNIX_TIME [--sample N] [--apply]`. Exit 0 on success, 1 when refused or failed.
pub fn run(args: &[String]) -> i32 {
    let (since, rest) = match since(args) {
        Ok(x) => x,
        Err(e) => {
            println!("{e}");
            return 1;
        }
    };
    let o = match walk::parse(&rest, "--recount-articles") {
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
        return match dry_run(&main, since, o.sample) {
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
    let limits = walk::limits("ATLAS_RECOUNT");
    let stop = walk::stop_flag();
    let (commits, rows, fixed) = (AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0));
    let result = walk::each_shard(&main, |path| apply_shard(path, since, &limits, &stop, &commits, &rows, &fixed));
    println!("{} rows recounted", fixed.load(Relaxed));
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

    /// A row a short-counting save left gets the sum and count of what it holds; one saved before `--since`, or
    /// already right, is left.
    #[test]
    fn a_row_saved_since_gets_every_article_counted() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let art = |copy: u8| Article {
            message_id: format!("<p1c{copy}@x>"),
            subject: "\"a.rar\" yEnc (1/1)".into(),
            filename: Some("a.rar".into()),
            part: Some(1),
            total_parts: Some(1),
            bytes: 100,
            ..Default::default()
        };
        let rel = Release {
            name: "Rel".into(),
            group: "alt.binaries.t".into(),
            articles: vec![art(0), art(1), art(2)],
            ..Default::default()
        };
        store::save(&main, &[rel]).unwrap();
        let conn = db::open_at(&store::shard_path(&main, store::shard_of("alt.binaries.t"))).unwrap();
        let id: i64 = conn.query_row("select id from releases", [], |r| r.get(0)).unwrap();
        assert_eq!(short(&conn, id, 300, 3, 0).unwrap(), None, "already right");
        // as the short-counting build left it
        assert_eq!(short(&conn, id, 100, 1, 0).unwrap(), Some((300, 3)));
        assert_eq!(short(&conn, id, 100, 1, i64::MAX).unwrap(), None, "not saved since");
        assert!(since(&["--recount-articles".into()]).is_err());
        assert_eq!(since(&["--since".into(), "5".into(), "--apply".into()]).unwrap(), (5, vec!["--apply".into()]));
    }
}
