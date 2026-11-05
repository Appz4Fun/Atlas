//! Which releases freeze, as one statement the writer runs for a release at the end of each database-lane visit
//! (`postprocess::apply`): what the visit wrote is already there, and what an earlier visit queued is taken back
//! when the release no longer matches (it grew past the size, or a rename moved its category). Releases no visit is
//! coming back to are walked by the writer a chunk a tick (`backlog_some`).

use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, Result, params};

pub const SMALL_COMPLETE: &str = "small_complete";
pub const NOT_TV_MOVIES: &str = "not_tv_movies";

/// Releases ?1 to ?6 with the rule that matches each now as its reason, null when none does. ?2 now (for `QUEUE`), ?3
/// the small rule's size (0: off), ?4 1 when the small rule leaves movies and TV alone, ?5 1 when the not-TV/movies
/// rule is on.
/// Small: complete means every file it says it has (its subjects' `[n/N]`), each with all its parts: a release whose
/// subjects give no file count is complete as far as it has come, and can still grow past the size, soo it stays live.
/// Not TV/movies: a categorized release outside Movies (2000s), TV (5000s) and Other (8000s, where Misc and Hashed
/// hold obfuscated movies and TV), complete or not; an uncategorized release stays live, since a later step may still
/// find it is a movie or an episode.
/// Never an obfuscated row (the files of one upload sit in rows of their own until the link step joins them), a row of
/// a linked upload, one with an NZB among its files or one a stored NZB points to (the NZB chain needs them), nor one
/// thawed by hand.
macro_rules! matching {
    () => {
        "select r.id as id, case
            when ?3 > 0 and r.complete = 1 and coalesce(r.file_total, 0) > 0 and r.size > 0 and r.size < ?3
                and (?4 = 0 or c.category is null
                     or (c.category not between 2000 and 2999 and c.category not between 5000 and 5999))
                then 'small_complete'
            when ?5 = 1 and c.category is not null
                and c.category not between 2000 and 2999 and c.category not between 5000 and 5999
                and c.category not between 8000 and 8999
                then 'not_tv_movies'
            end as reason
        from releases r left join release_category c on c.release_id = r.id
        where r.id between ?1 and ?6 and coalesce(r.is_obfuscated, 0) = 0
            and not exists (select 1 from freeze_exempt e where e.release_id = r.id)
            and not exists (select 1 from upload_link u where u.release_id = r.id)
            and not exists (select 1 from upload_link u where u.lead_id = r.id)
            and not exists (select 1 from nzb_pointed p where p.release_id = r.id)
            and not exists (select 1 from files f where f.release_id = r.id and f.filename like '%.nzb')"
    };
}

const QUEUE: &str = concat!(
    "insert or ignore into freeze_pending (release_id, reason, at) select id, reason, ?2 from (",
    matching!(),
    ") where reason is not null"
);

const STILL: &str = concat!("select exists (select 1 from (", matching!(), ") where reason is not null)");

/// Whether any rule is on: a visit checks nothing otherwise.
pub fn any_on() -> bool {
    super::ENABLED.load(Relaxed) && (super::SMALL_BYTES.load(Relaxed) > 0 || super::NOT_TV_MOVIES.load(Relaxed))
}

/// Queue release `id` for freezing if a rule matches it now, and take it off the queue if none does. In the
/// caller's transaction, on the shard's own connection.
pub fn evaluate(conn: &Connection, id: i64, now: i64) -> Result<bool> {
    conn.prepare_cached("delete from freeze_pending where release_id = ?1")?.execute([id])?;
    if !any_on() {
        return Ok(false);
    }
    Ok(queue(conn, id, id, now)? > 0)
}

/// The rules' settings now: the small rule's size, whether it keeps movies and TV, whether not-TV/movies is on.
fn settings() -> (i64, i64, i64) {
    (
        super::SMALL_BYTES.load(Relaxed),
        super::SMALL_KEEPS_TV_MOVIES.load(Relaxed) as i64,
        super::NOT_TV_MOVIES.load(Relaxed) as i64,
    )
}

/// Queue what matches among releases `lo..=hi`, leaving queued ones as they are. Returns how many it queued.
fn queue(conn: &Connection, lo: i64, hi: i64, now: i64) -> Result<usize> {
    let (small, keep_video, not_tv) = settings();
    conn.prepare_cached(QUEUE)?.execute(params![lo, now, small, keep_video, not_tv, hi])
}

/// meta key: the backlog has looked at every release from here up; 0 once it got to the bottom
const BACKLOG_BELOW: &str = "freeze_backlog_below";
/// meta key: the rules the backlog walk is for (`signature`); other rules start it over from the top
const BACKLOG_RULES: &str = "freeze_backlog_rules";

/// The rules as they are now, as one number.
fn signature() -> i64 {
    let (small, keep_video, not_tv) = settings();
    small.wrapping_mul(4).wrapping_add(keep_video * 2 + not_tv)
}

fn meta(conn: &Connection, key: &str) -> Result<Option<i64>> {
    conn.prepare_cached("select value from main.meta where key = ?1")?.query_row([key], |r| r.get(0)).optional()
}

fn set_meta(conn: &Connection, key: &str, value: i64) -> Result<()> {
    conn.prepare_cached("insert or replace into main.meta (key, value) values (?1, ?2)")?
        .execute(params![key, value])?;
    Ok(())
}

/// What one backlog step did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Backlog {
    pub looked_at: usize,
    pub queued: usize,
}

/// Releases one step of the walk takes at a time, between looks at the clock and the stop flag.
const BACKLOG_CHUNK: usize = 250;

/// One step of the walk over releases no visit is coming back to: up to `limit` releases down from where the last
/// step stopped, newest first, queued where a rule matches (one queued already keeps its place). Goes a chunk at a
/// time and stops early once `budget` has passed or the indexer is stopping, soo the writer gets back to saves; where
/// it got to is kept after every chunk. Starts over from the top when the rules change, and does nothing once it got
/// to the bottom. In the caller's transaction, on the shard's writer.
pub fn backlog_some(conn: &Connection, limit: usize, budget: Duration, now: i64) -> Result<Backlog> {
    if !any_on() || limit == 0 {
        return Ok(Backlog::default());
    }
    let started = Instant::now();
    let sig = signature();
    let mut below = meta(conn, BACKLOG_BELOW)?;
    if meta(conn, BACKLOG_RULES)? != Some(sig) {
        set_meta(conn, BACKLOG_RULES, sig)?;
        below = None;
    }
    let mut below = match below {
        Some(0) => return Ok(Backlog::default()),
        Some(b) => b,
        None => i64::MAX,
    };
    let mut done = Backlog::default();
    while done.looked_at < limit {
        let take = (limit - done.looked_at).min(BACKLOG_CHUNK);
        let (lo, n): (Option<i64>, i64) = conn
            .prepare_cached(
                "select min(id), count(*) from (select id from releases where id < ?1 order by id desc limit ?2)",
            )?
            .query_row(params![below, take as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let Some(lo) = lo else {
            set_meta(conn, BACKLOG_BELOW, 0)?;
            break;
        };
        done.queued += queue(conn, lo, below - 1, now)?;
        done.looked_at += n as usize;
        set_meta(conn, BACKLOG_BELOW, lo)?;
        below = lo;
        if started.elapsed() >= budget || super::STOPPING.load(Relaxed) {
            break;
        }
    }
    Ok(done)
}

/// Whether a rule matches release `id` as it is now, for a release about to move.
pub fn still(conn: &Connection, id: i64) -> Result<bool> {
    if !any_on() {
        return Ok(false);
    }
    let (small, keep_video, not_tv) = settings();
    conn.prepare_cached(STILL)?.query_row(params![id, 0, small, keep_video, not_tv, id], |r| r.get(0))
}
