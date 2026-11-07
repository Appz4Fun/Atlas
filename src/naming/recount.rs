//! `atlas --recount-reposts`: size and parts of rows saved before a repost was counted once. An uploader that posts
//! the same file again under other message-ids (five times over, every article the same) made one release of them
//! all, and each copy of a part was added to its size and parts: a 40 GB post read 200 GB. Saves now count a part
//! once (`store::repost_excess` tells what they didnt). A dry run unless `--apply`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use rusqlite::{Connection, params};

use super::walk::{self, BATCH, WalkLimits};
use crate::{db, store};

const CURSOR: &str = "recount_reposts_after";

/// A batch of `(id, parts)` past `after`: every row comes back, soo the walk goes on past rows with nothing to do.
fn batch(conn: &Connection, after: i64) -> rusqlite::Result<Vec<(i64, i64)>> {
    conn.prepare_cached("select id, parts from releases where id > ? order by id limit ?")?
        .query_map(params![after, BATCH], |r| Ok((r.get(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0))))?
        .collect()
}

/// What a row's reposts added, when it has any: `(bytes, articles)`. Never more than `parts` is past the parts its
/// files have seen and its articles without a part number: a repost saved after this build came in wasnt counted,
/// and isnt taken out again (the bytes in proportion, the reposts of one row being copies of the same articles).
fn excess(conn: &Connection, id: i64, parts: i64) -> rusqlite::Result<Option<(i64, i64)>> {
    let seen = store::seen_parts(conn, id)?;
    if parts <= seen {
        return Ok(None);
    }
    let found = store::repost_excess(conn, id)?;
    let over = parts - seen - found.unseen;
    if found.count <= 0 || over <= 0 {
        return Ok(None);
    }
    let take = found.count.min(over);
    let bytes =
        i64::try_from(i128::from(found.bytes) * i128::from(take) / i128::from(found.count)).unwrap_or(found.bytes);
    Ok(Some((bytes, take)))
}

fn dry_run(main: &Path, sample: usize) -> anyhow::Result<()> {
    let (mut rows, mut bytes, mut articles, mut shown) = (0u64, 0i64, 0i64, Vec::new());
    for path in store::shard_paths(main) {
        let conn = walk::open_for_reading(&path)?;
        let mut after = i64::MIN;
        loop {
            let ids = batch(&conn, after)?;
            let Some((last, _)) = ids.last() else { break };
            after = *last;
            for (id, parts) in ids {
                if let Some((b, n)) = excess(&conn, id, parts)? {
                    rows += 1;
                    bytes += b;
                    articles += n;
                    if shown.len() < sample {
                        shown.push((id, b, n));
                    }
                }
            }
        }
    }
    println!("{rows} rows would lose {articles} reposted articles, {bytes} bytes in all");
    for (id, b, n) in shown {
        println!("  {id}: {n} articles, {b} bytes");
    }
    println!("a dry run: nothing was changed. --apply takes them out of size and parts");
    Ok(())
}

fn apply_shard(
    path: &Path,
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
    let write = |tx: &Connection, ids: &[(i64, i64)]| -> rusqlite::Result<()> {
        // the reposted articles stay, and so does the shard's count of articles: only the release's size and parts
        let mut update = tx.prepare_cached("update releases set size = size - ?, parts = parts - ? where id = ?")?;
        // postprocessing's growth mark comes down with it, or the next real part would read as no growth
        let mut mark =
            tx.prepare_cached("update pp_state set parts_seen = max(parts_seen - ?, 0) where release_id = ?")?;
        for (id, parts) in ids {
            if let Some((bytes, count)) = excess(tx, *id, *parts)? {
                update.execute(params![bytes, count, id])?;
                mark.execute(params![count, id])?;
                fixed.fetch_add(1, Relaxed);
            }
        }
        Ok(())
    };
    while walk.step(batch, write)? {}
    walk::finish_shard(&conn, "key = 'recount_reposts_after'")
}

/// `atlas --recount-reposts [--sample N] [--apply]`. Exit 0 on success, 1 when refused or failed.
pub fn run(args: &[String]) -> i32 {
    let o = match walk::parse(args, "--recount-reposts") {
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
    let limits = walk::limits("ATLAS_RECOUNT");
    let stop = walk::stop_flag();
    let (commits, rows, fixed) = (AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0));
    let result = walk::each_shard(&main, |path| apply_shard(path, &limits, &stop, &commits, &rows, &fixed));
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

    /// Of a row's reposts, only as many as its `parts` is past what its files have seen come out: one an older save
    /// counted, not the one a save of this build left uncounted.
    #[test]
    fn only_the_reposts_a_save_counted_come_out() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let art = |copy: u8| Article {
            message_id: format!("<p1c{copy}@x>"),
            subject: "\"a.rar\" yEnc (1/1)".into(),
            filename: Some("a.rar".into()),
            part: Some(1),
            total_parts: Some(1),
            bytes: 1_000,
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
        let (id, parts): (i64, i64) = conn
            .query_row("select id, parts from releases where name = 'Rel'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(parts, 1);
        assert_eq!(excess(&conn, id, parts).unwrap(), None, "a row saved by this build has nothing to take out");
        // as an older save left it: one of the two reposts counted
        assert_eq!(excess(&conn, id, parts + 1).unwrap(), Some((1_000, 1)));
    }
}
