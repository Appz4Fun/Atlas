//! Titles that arrive after their release. This only nominates: `Stage::rerun` sends a release back
//! through `predb_match`, which decides by the same rules as ever.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use rusqlite::{Connection, params};

use crate::naming::{clean_candidate, identity_key};

use super::keys::{md5_hex, name_key, sha1_hex};
use super::matcher::{is_full_title, literally};
use super::step::lowest_in_flight;

pub const LATE_DELAY_SECS: i64 = 600;
pub const LATE_FTS_LIMIT: i64 = 32;
pub const LATE_MAX_RERUNS: usize = 16;
pub const LATE_TITLE_BUDGET: Duration = Duration::from_secs(2);

#[derive(Default, Debug, PartialEq)]
pub struct Nominees {
    pub ids: Vec<i64>,
    pub overflow: bool,
    pub out_of_budget: bool,
    pub stopped: bool,
    /// phrase queries that ran
    pub phrases: usize,
    /// routes that failed: the others still count
    pub errors: usize,
}

/// What the late path has come across, for stats.json.
#[derive(Default)]
pub struct LateCounts {
    pub titles: AtomicU64,
    pub reruns: AtomicU64,
    pub overflow: AtomicU64,
    pub out_of_budget: AtomicU64,
    pub errors: AtomicU64,
}

/// A name that can be this title: the title plus a file suffix, or the same once cleaned, letter case and separators aside.
fn accepts(row_name: &str, title: &str) -> bool {
    literally(row_name, title) || clean_candidate(row_name).is_some_and(|c| identity_key(&c) == identity_key(title))
}

fn accepts_hash(row_name: &str, md5: &str, sha1: &str) -> bool {
    clean_candidate(row_name).is_some_and(|c| {
        let c = c.to_ascii_lowercase();
        c == md5 || c == sha1
    })
}

pub const NAME_PROBE: &str = "select id, name_source from releases where name = ?1 limit 17";
pub const FTS_PROBE: &str = "select rowid from releases_fts where releases_fts match ?1 order by rowid desc limit ?2";
pub const ROW_READ: &str = "select name, display_name, name_source from releases where id = ?1";
pub const INNER_PROBE: &str = "select release_id from predb_inner_key where key = ?1 limit 17";

type Hit = (i64, String, Option<String>);

/// rows a quoted FTS token or phrase finds, with `name_source >= 100` already dropped
fn fts_rows(conn: &Connection, quoted: &str) -> rusqlite::Result<Vec<Hit>> {
    let ids: Vec<i64> = conn
        .prepare_cached(FTS_PROBE)?
        .query_map(params![quoted, LATE_FTS_LIMIT], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut read = conn.prepare_cached(ROW_READ)?;
    let mut hits = Vec::with_capacity(ids.len());
    for id in ids {
        let row: (String, Option<String>, Option<i64>) =
            read.query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        if row.2.is_none_or(|s| s < 100) {
            hits.push((id, row.0, row.1));
        }
    }
    Ok(hits)
}

/// `shards`: read-only connections, in shard order. `stop` is checked between shards and between statements.
pub fn nominate(
    shards: &[Connection],
    title: &str,
    clock: &dyn Fn() -> Instant,
    stop: &AtomicBool,
) -> rusqlite::Result<Nominees> {
    let mut n = Nominees::default();
    let start = clock();
    let (md5, sha1) = (md5_hex(title), sha1_hex(title));
    let full = is_full_title(title);
    let key = name_key(title);
    let phrase = format!("\"{title}\"");
    let mut found: Vec<i64> = Vec::new();
    let stopped = |n: &mut Nominees| {
        let s = stop.load(Relaxed);
        n.stopped |= s;
        s
    };

    for conn in shards {
        if stopped(&mut n) {
            return Ok(n);
        }
        let exact: Vec<(i64, Option<i64>)> = conn
            .prepare_cached(NAME_PROBE)?
            .query_map([title], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        found.extend(exact.into_iter().filter(|(_, s)| s.is_none_or(|s| s < 100)).map(|(id, _)| id));

        for digest in [&md5, &sha1] {
            if stopped(&mut n) {
                return Ok(n);
            }
            match fts_rows(conn, &format!("\"{digest}\"")) {
                Ok(hits) => {
                    found.extend(hits.into_iter().filter(|(_, name, _)| accepts_hash(name, &md5, &sha1)).map(|h| h.0))
                }
                Err(_) => n.errors += 1,
            }
        }

        if full {
            if clock().saturating_duration_since(start) >= LATE_TITLE_BUDGET {
                n.out_of_budget = true;
            } else {
                if stopped(&mut n) {
                    return Ok(n);
                }
                n.phrases += 1;
                match fts_rows(conn, &phrase) {
                    Ok(hits) => found.extend(
                        hits.into_iter()
                            .filter(|(_, name, display)| {
                                accepts(name, title) || display.as_deref().is_some_and(|d| accepts(d, title))
                            })
                            .map(|h| h.0),
                    ),
                    Err(_) => n.errors += 1,
                }
            }
        }

        if stopped(&mut n) {
            return Ok(n);
        }
        let inner: rusqlite::Result<Vec<i64>> =
            conn.prepare_cached(INNER_PROBE).and_then(|mut stmt| stmt.query_map([key], |r| r.get(0))?.collect());
        match inner {
            Ok(ids) => found.extend(ids),
            Err(_) => n.errors += 1,
        }
    }

    found.sort_unstable();
    found.dedup();
    if found.len() > LATE_MAX_RERUNS {
        n.overflow = true;
    } else {
        n.ids = found;
    }
    Ok(n)
}

/// Up to `budget` titles past the cursor and at least `LATE_DELAY_SECS` old. Returns how many were read.
pub fn tick(
    predb: &Connection,
    shards: &[Connection],
    now: i64,
    budget: i64,
    stop: &AtomicBool,
    rerun: &dyn Fn(i64) -> anyhow::Result<()>,
) -> anyhow::Result<usize> {
    tick_counting(predb, shards, now, budget, stop, rerun, &LateCounts::default())
}

pub(crate) fn tick_counting(
    predb: &Connection,
    shards: &[Connection],
    now: i64,
    budget: i64,
    stop: &AtomicBool,
    rerun: &dyn Fn(i64) -> anyhow::Result<()>,
    counts: &LateCounts,
) -> anyhow::Result<usize> {
    // a title newer than any uncommitted read waits for that read's keys
    let top = predb.path().and_then(|p| lowest_in_flight(Path::new(p))).unwrap_or(i64::MAX);
    let titles: Vec<(i64, String)> = predb
        .prepare_cached(
            "select id, title from titles
             where id > (select value from meta where key = 'late_after_id') and added_at <= ?1 and id <= ?3
             order by id limit ?2",
        )?
        .query_map(params![now - LATE_DELAY_SECS, budget, top], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut read = 0;
    for (id, title) in titles {
        if stop.load(Relaxed) {
            break;
        }
        let n = nominate(shards, &title, &Instant::now, stop)?;
        if n.stopped {
            break;
        }
        counts.out_of_budget.fetch_add(u64::from(n.out_of_budget), Relaxed);
        counts.overflow.fetch_add(u64::from(n.overflow), Relaxed);
        counts.errors.fetch_add(n.errors as u64, Relaxed);
        for release in &n.ids {
            rerun(*release)?;
            counts.reruns.fetch_add(1, Relaxed);
        }
        predb.prepare_cached("update meta set value = ?1 where key = 'late_after_id'")?.execute([id])?;
        counts.titles.fetch_add(1, Relaxed);
        read += 1;
    }
    Ok(read)
}
