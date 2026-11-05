//! Which releases freeze, as one statement the writer runs for a release at the end of each database-lane visit
//! (`postprocess::apply`): what the visit wrote is already there, and what an earlier visit queued is taken back
//! when the release no longer matches (it grew past the size, or a rename moved its category).

use std::sync::atomic::Ordering::Relaxed;

use rusqlite::{Connection, Result, params};

pub const SMALL_COMPLETE: &str = "small_complete";

/// Release ?1 with the rule that matches it now as its reason, null when none does. ?2 now (for `QUEUE`), ?3 the small
/// rule's size (0: off), ?4 1 when the small rule leaves movies and TV alone.
/// Complete means every file it says it has (its subjects' `[n/N]`), each with all its parts: a release whose subjects
/// give no file count is complete as far as it has come, and can still grow past the size, soo it stays live.
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
            end as reason
        from releases r left join release_category c on c.release_id = r.id
        where r.id = ?1 and coalesce(r.is_obfuscated, 0) = 0
            and not exists (select 1 from freeze_exempt e where e.release_id = r.id)
            and not exists (select 1 from upload_link u where u.release_id = r.id)
            and not exists (select 1 from upload_link u where u.lead_id = r.id)
            and not exists (select 1 from nzb_pointed p where p.release_id = r.id)
            and not exists (select 1 from files f where f.release_id = r.id and f.filename like '%.nzb')"
    };
}

const QUEUE: &str = concat!(
    "insert into freeze_pending (release_id, reason, at) select id, reason, ?2 from (",
    matching!(),
    ") where reason is not null"
);

const STILL: &str = concat!("select exists (select 1 from (", matching!(), ") where reason is not null)");

/// Whether any rule is on: a visit checks nothing otherwise.
pub fn any_on() -> bool {
    super::ENABLED.load(Relaxed) && super::SMALL_BYTES.load(Relaxed) > 0
}

/// Queue release `id` for freezing if a rule matches it now, and take it off the queue if none does. In the
/// caller's transaction, on the shard's own connection.
pub fn evaluate(conn: &Connection, id: i64, now: i64) -> Result<bool> {
    conn.prepare_cached("delete from freeze_pending where release_id = ?1")?.execute([id])?;
    if !any_on() {
        return Ok(false);
    }
    let small = super::SMALL_BYTES.load(Relaxed);
    let keep_video = super::SMALL_KEEPS_TV_MOVIES.load(Relaxed) as i64;
    Ok(conn.prepare_cached(QUEUE)?.execute(params![id, now, small, keep_video])? > 0)
}

/// Whether a rule matches release `id` as it is now, for a release about to move.
pub fn still(conn: &Connection, id: i64) -> Result<bool> {
    if !any_on() {
        return Ok(false);
    }
    let small = super::SMALL_BYTES.load(Relaxed);
    let keep_video = super::SMALL_KEEPS_TV_MOVIES.load(Relaxed) as i64;
    conn.prepare_cached(STILL)?.query_row(params![id, 0, small, keep_video], |r| r.get(0))
}
