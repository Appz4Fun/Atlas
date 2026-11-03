//! A resolved title's IDs, onto every release of it in every shard.

use std::collections::VecDeque;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::{Connection, params};

use crate::indexer::Db;
use crate::store;

use super::Media;
use super::resolve::Ids;

pub const SECOND_PASS: Duration = Duration::from_secs(60);
pub const CHUNK: i64 = 1_000;

/// `indexed by` is required: with a rowid range the planner could otherwise walk every release between two rows of the title.
pub const UPDATE: &str = "update media_release indexed by media_release_title
   set tvdb_id = ?4, tvmaze_id = ?5, trakt_id = ?6, tmdb_id = ?7, imdb_id = iif(imdb_from_nfo = 1, imdb_id, ?8), tvrage_id = ?9
 where title_key = ?1 and release_id > ?2 and release_id <= ?3
   and (tvdb_id is not ?4 or tvmaze_id is not ?5 or trakt_id is not ?6 or tmdb_id is not ?7
        or (imdb_from_nfo = 0 and imdb_id is not ?8) or tvrage_id is not ?9)";

/// One chunk: where it reached, and how many rows it looked at.
fn chunk(conn: &Connection, key: i64, after: i64, ids: &Ids) -> rusqlite::Result<(i64, i64)> {
    let (top, count): (Option<i64>, i64) = conn.query_row(
        "select max(release_id), count(*) from (select release_id from media_release
          where title_key = ?1 and release_id > ?2 order by release_id limit ?3)",
        params![key, after, CHUNK],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let Some(top) = top else { return Ok((after, 0)) };
    conn.prepare_cached(UPDATE)?
        .execute(params![key, after, top, ids.tvdb, ids.tvmaze, ids.trakt, ids.tmdb, ids.imdb, ids.tvrage])?;
    Ok((top, count))
}

/// One pass over one title. False: it's stopping, or a writer is gone.
fn pass(media: &Media, db: &Db, key: i64, ids: Ids, stopping: &dyn Fn() -> bool) -> bool {
    for shard in 0..store::SHARDS {
        let mut after = 0i64;
        loop {
            if stopping() {
                return false;
            }
            let reached: Arc<Mutex<(i64, i64)>> = Arc::default();
            let out = reached.clone();
            let job = db.on_writer(
                shard,
                Box::new(move |conn: &Connection| {
                    *out.lock().unwrap() = chunk(conn, key, after, &ids)?;
                    Ok(())
                }),
            );
            let Ok(answer) = job else { return false };
            if !matches!(answer.recv(), Ok(Ok(()))) {
                return false;
            }
            media.fanout_jobs.fetch_add(1, Relaxed);
            let (top, count) = *reached.lock().unwrap();
            after = top;
            if count < CHUNK {
                break;
            }
        }
    }
    true
}

/// Until `stopping`. Unfinished titles stay `fanout_done = 0`, and the next start finishes them.
pub fn run(media: &Arc<Media>, db: &Db, stopping: &dyn Fn() -> bool) {
    if let Ok(owed) = media.with_store(super::store::fanout_owed) {
        for key in owed {
            media.fanout.lock().unwrap().push_back(key);
        }
    }
    let mut second: VecDeque<(Instant, i64)> = VecDeque::new();
    while !stopping() {
        let due = second.front().is_some_and(|(at, _)| *at <= Instant::now());
        if due && let Some((_, key)) = second.pop_front() {
            let ids = media.with_store(|c| super::store::get(c, key)).ok().flatten().map(|r| r.ids);
            if let Some(ids) = ids {
                if !pass(media, db, key, ids, stopping) {
                    return;
                }
                let _ = media.with_store(|c| super::store::fanout_finished(c, key));
            }
            continue;
        }
        if let Some(key) = media.next_fanout() {
            let ids = media.with_store(|c| super::store::get(c, key)).ok().flatten().map(|r| r.ids);
            if let Some(ids) = ids {
                if !pass(media, db, key, ids, stopping) {
                    return;
                }
                second.push_back((Instant::now() + media.second_pass_wait(), key));
            }
            continue;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
