//! Day chunks of a group's backfill that any server carrying the group can
//! take (see docs/superpowers/specs/2026-10-05-faster-backfill-smaller-storage-design.md).
//! Lives in the native PostgreSQL catalog; migrations own the schema.

use crate::pg::sql::{Result, Sql, vals};

/// a claim older than this is taken over (its worker died or was stopped)
pub const CLAIM_TIMEOUT: i64 = 30 * 60;

/// A chunk whose run failed rests this long before any server tries it again, doubling with each failure in a row up
/// to `FAIL_REST_MOST`. While it rests it neither is claimed nor holds the others back (see `frontier`): on prod eight
/// chunks of November that timed out or came back with broken compressed headers every time held all 4.8 million
/// older chunks back, for a day.
pub const FAIL_REST: i64 = 5 * 60;
pub const FAIL_REST_MOST: i64 = 6 * 3600;

/// How long a chunk rests after `failures` failures in a row (see `FAIL_REST`).
fn rest(failures: i64) -> i64 {
    (FAIL_REST << (failures - 1).clamp(0, 7)).min(FAIL_REST_MOST)
}

/// SQL: chunk `c` isnt resting after a failure at unix time `now`
fn not_resting(now: i64) -> String {
    not_resting_of("c.", now)
}

/// SQL: the chunk whose columns are named with `prefix` isnt resting after a failure at unix time `now`
fn not_resting_of(prefix: &str, now: i64) -> String {
    let steps: String = (1..=8).map(|n| format!(" when {n} then {}", rest(n))).collect();
    format!(
        "({prefix}failed_at is null or {prefix}failed_at + case coalesce({prefix}failures, 1){steps} \
         else {FAIL_REST_MOST} end <= {now})"
    )
}

/// SQL: claimed chunk `c`'s claim isnt stale at unix time `now` (see `CLAIM_TIMEOUT`)
fn claim_live(now: i64) -> String {
    format!("(c.claimed_at >= {})", now - CLAIM_TIMEOUT)
}

const PENDING: i64 = 0;
const CLAIMED: i64 = 1;
const DONE: i64 = 2;

/// days since 1970-01-01 UTC
pub fn unix_day(t: i64) -> i64 {
    t.div_euclid(86_400)
}

/// Pending chunks for `newest_day` down to `oldest_day`, keeping any that exist.
pub fn add(db: &dyn Sql, group: &str, newest_day: i64, oldest_day: i64) -> Result<usize> {
    db.tx(|tx| add_days(tx, group, oldest_day, newest_day + 1))
}

/// SQL: a pending chunk `$1`, `$2`, kept as it is when there is one
const ADD: &str = "insert into backfill_chunks (grp, day, state) values ($1, $2, $3) on conflict (grp, day) do nothing";

/// Pending chunks for the days before `before` back to `from`, newest first, keeping any that exist. The count added.
fn add_days(tx: &dyn Sql, group: &str, from: i64, before: i64) -> Result<usize> {
    let mut added = 0;
    for day in (from..before).rev() {
        added += tx.execute(ADD, vals![group, day, PENDING])?;
    }
    Ok(added)
}

/// When a server goes back further than any did on split `group`, to
/// `oldest_at` (unix seconds) in `oldest_day`: chunks for the days from
/// before the split's oldest one back to `oldest_day`. The old oldest day
/// was done (if it was) only from where the server going back furthest then
/// started: it is to do again, and that server is forgotten (see `deepest`).
/// A claim on it is cleared too, so its worker's finish does nothing, and
/// the carriers' sweeps are to do again.
/// Also when `oldest_day` is the oldest day but `oldest_at` is before where
/// that server started in it. Returns the chunks added or to do again.
pub fn reach_back(db: &dyn Sql, group: &str, oldest_day: i64, oldest_at: i64) -> Result<usize> {
    db.tx(|tx| {
        // what's added depends on the window's floor: read as it is, not as it was before a change to it commits
        tx.lock(&speculative_lock(group))?;
        let Some(old) = self::oldest_day(tx, group)? else { return Ok(0) };
        // never past the window of days only the deepest server has
        let spec = speculative(tx, group)?;
        let floor = spec.as_ref().map_or(i64::MIN, |s| s.runner_up_day - s.reach);
        if oldest_day < floor {
            if floor >= old {
                return Ok(0);
            }
            // days only the deepest server has: they dont change what any sweep covers
            return add_days(tx, group, floor, old);
        }
        if oldest_day > old {
            return Ok(0);
        }
        if spec.as_ref().is_some_and(|s| old <= s.runner_up_day) && oldest_day < old {
            // only days the deepest server alone has: added, nothing redone
            return add_days(tx, group, oldest_day, old);
        }
        if oldest_day == old {
            let since: Option<i64> =
                tx.opt::<Option<i64>>("select oldest_at from backfill_deepest where grp = $1", vals![group])?.flatten();
            if since.is_none_or(|since| oldest_at >= since) {
                return Ok(0);
            }
        }
        let added = add_days(tx, group, oldest_day, old)?;
        let redo = tx.execute(
            &format!(
                "update backfill_chunks set state = {PENDING}, server = null, claimed_at = null, done_at = null
                 where grp = $1 and day = $2 and state in ({DONE}, {CLAIMED})"
            ),
            vals![group, old],
        )?;
        tx.execute("delete from backfill_deepest where grp = $1", vals![group])?;
        // a carrier's sweep stopped at its first article then: it may keep older ones now
        tx.execute("delete from backfill_sweeps where grp = $1", vals![group])?;
        // and a sweep that started before this doesnt get to note itself done (see `set_swept`)
        tx.execute(
            "insert into backfill_sweep_gen (grp, gen) values ($1, 1)
             on conflict (grp) do update set gen = backfill_sweep_gen.gen + 1",
            vals![group],
        )?;
        Ok(added + redo)
    })
}

/// A split's days older than the next deepest server goes back stand on the
/// deepest one's own (forgeable) Date headers. At most `reach` of them,
/// counted back from `runner_up_day` (the first day the next deepest
/// keeps), are chunks at once; the older ones are left to the deep server's
/// sweep. `days` of them were done holding `articles` dated in them. Kept
/// per group in the database, soo a restart or `reach_back` doesnt undo it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Speculative {
    pub runner_up_day: i64,
    pub reach: i64,
    pub days: i64,
    pub articles: i64,
}

/// The lock a group's speculative window is read and changed under (`Sql::lock`), on Postgres.
pub fn speculative_lock(group: &str) -> String {
    format!("atlas.speculative.{group}")
}

pub fn speculative(db: &dyn Sql, group: &str) -> Result<Option<Speculative>> {
    db.row("select runner_up_day, reach, days, articles from backfill_speculative where grp = $1", vals![group])?
        .map(|r| Ok(Speculative { runner_up_day: r.get(0)?, reach: r.get(1)?, days: r.get(2)?, articles: r.get(3)? }))
        .transpose()
}

pub fn set_speculative(db: &dyn Sql, group: &str, s: &Speculative) -> Result<()> {
    db.execute(
        "insert into backfill_speculative (grp, runner_up_day, reach, days, articles) values ($1, $2, $3, $4, $5)
         on conflict (grp) do update set runner_up_day = excluded.runner_up_day, reach = excluded.reach,
             days = excluded.days, articles = excluded.articles",
        vals![group, s.runner_up_day, s.reach, s.days, s.articles],
    )?;
    Ok(())
}

/// Drop split `group`'s chunks older than `day` that arent done: days no
/// carrier keeps (a split reaching back that far was made from forged
/// dates), which would wait for a server forever and keep the split from
/// completing. Returns the chunks dropped.
pub fn drop_before(db: &dyn Sql, group: &str, day: i64) -> Result<usize> {
    db.execute(
        &format!("delete from backfill_chunks where grp = $1 and day < $2 and state != {DONE}"),
        vals![group, day],
    )
}

/// Check if a group has any chunks in backfill.
pub fn is_split(db: &dyn Sql, group: &str) -> Result<bool> {
    db.exists("select 1 from backfill_chunks where grp = $1 limit 1", vals![group])
}

/// A claimed chunk. Its server and claim time tell this claim from a later
/// one of the same chunk: a chunk that ran past CLAIM_TIMEOUT and was taken
/// over is the new owner's, finishing or giving back the old claim does nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub group: String,
    pub day: i64,
    pub server: String,
    pub claimed_at: i64,
    /// a day read before, put back by `reopen_done`: read whole again, not up to where this server's backfill
    /// cursor has got since (`indexer::chunk_range`)
    pub reread: bool,
}

/// Ask for a day of a group's backfill before the others: one a post found
/// in part was posted on. A day that's done or has no chunk is left as it is.
pub fn want(db: &dyn Sql, group: &str, day: i64, now: i64) -> Result<bool> {
    let n = db.execute(
        &format!(
            "insert into backfill_wanted (grp, day, at)
             select grp, day, cast($3 as bigint) from backfill_chunks where grp = $1 and day = $2 and state != {DONE}
             on conflict (grp, day) do nothing"
        ),
        vals![group, day, now],
    )?;
    Ok(n > 0)
}

/// Days of backfill a group may run ahead of the slowest: every group of a server reads one day before any goes more
/// than this many days past it, soo all groups stay at about the same posting time (`frontier`). The articles of an
/// upload spread over several groups are then read together, and linked across them (`xref`).
pub const LOCKSTEP_DAYS: i64 = 1;

/// The first day forward backfill reads (`backfill_start_days_back` in the config, worked out once and kept: see
/// `set_forward_start`), 0 for the usual newest day first. Forward, only days from then on are claimed, oldest
/// first, in lockstep: no group goes more than `LOCKSTEP_DAYS` past the oldest day any of them still has to do from
/// then, soo the backfill cursor walks toward live reading with every group together. Older days stay pending for a
/// later backward run. Set at the indexer's start: a change needs a restart.
pub static FORWARD_START_DAY: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

const FORWARD_BACK_KEY: &str = "backfill_forward_back";
const FORWARD_START_KEY: &str = "backfill_forward_start";

/// Whether forward backfill is on.
pub fn forward_on() -> bool {
    FORWARD_START_DAY.load(std::sync::atomic::Ordering::Relaxed) > 0
}

fn forward_from(_now: i64) -> Option<i64> {
    let day = FORWARD_START_DAY.load(std::sync::atomic::Ordering::Relaxed);
    (day > 0).then_some(day)
}

/// The forward backfill's first day for `back` days back (None or 0: off), fixed the first time it's worked out
/// and kept in the main database, soo it stays "30 days before it began" rather than moving on every midnight; worked
/// out again only when `back` changes. Sets `FORWARD_START_DAY`. Returns the day, 0 when off.
pub fn set_forward_start(db: &dyn Sql, back: Option<i64>, now: i64) -> Result<i64> {
    let day = forward_start_day(db, back, now)?;
    FORWARD_START_DAY.store(day, std::sync::atomic::Ordering::Relaxed);
    Ok(day)
}

/// The day `set_forward_start` sets, worked out or read back (and kept), without setting it.
fn forward_start_day(db: &dyn Sql, back: Option<i64>, now: i64) -> Result<i64> {
    let back = back.filter(|b| *b > 0).unwrap_or(0);
    let day = if back == 0 {
        // off: what was kept goes, soo turning it on again later starts from then, not from the old start
        db.execute("delete from meta where key in ($1, $2)", vals![FORWARD_BACK_KEY, FORWARD_START_KEY])?;
        0
    } else {
        let kept_back = crate::store::meta(db, FORWARD_BACK_KEY)?;
        let kept_start = crate::store::meta(db, FORWARD_START_KEY)?;
        match (kept_back, kept_start) {
            (Some(b), Some(start)) if b == back => start,
            _ => {
                let start = now.div_euclid(86_400) - back;
                crate::store::put_meta(db, FORWARD_BACK_KEY, back)?;
                crate::store::put_meta(db, FORWARD_START_KEY, start)?;
                start
            }
        }
    };
    Ok(day)
}

/// Atomically claim the newest chunk that is pending or whose claim went
/// stale, of `groups`: (group, oldest day the server keeps), `i64::MIN` when
/// any day will do. A day asked for (`want`) goes first. The rest go in
/// lockstep, newest day first: no group goes back more than `LOCKSTEP_DAYS`
/// past the newest day any of them still has to do or is doing (`frontier`),
/// soo a small group waits for a big one instead of racing back alone, and
/// spare workers wait too. Within that the newest day goes first, a group big
/// releases come from (`media_groups`) first among equals.
pub fn claim(db: &dyn Sql, groups: &[(String, i64)], host: &str, now: i64) -> Result<Option<Claim>> {
    let claimed = claim_any(db, groups, host, now)?;
    if let Some(c) = &claimed {
        crate::nxv_store::live::note_claimed_day(c.day);
    }
    Ok(claimed)
}

fn claim_any(db: &dyn Sql, groups: &[(String, i64)], host: &str, now: i64) -> Result<Option<Claim>> {
    claim_with(db, groups, host, now, forward_from(now))
}

/// `claim`, forward from day `start` when given (`FORWARD_START_DAY`).
fn claim_with(
    db: &dyn Sql,
    groups: &[(String, i64)],
    host: &str,
    now: i64,
    start: Option<i64>,
) -> Result<Option<Claim>> {
    if groups.is_empty() {
        return Ok(None);
    }
    if let Some(claim) = claim_wanted(db, groups, host, now)? {
        return Ok(Some(claim));
    }
    let media = media_groups(db)?;
    if let Some(start) = start {
        let from: Vec<(String, i64)> = groups.iter().map(|(g, oldest)| (g.clone(), (*oldest).max(start))).collect();
        let Some(frontier) = frontier_oldest(db, &from, now)? else { return Ok(None) };
        let ranges: Vec<(String, i64, i64)> =
            from.into_iter().map(|(g, oldest)| (g, oldest, frontier + LOCKSTEP_DAYS)).collect();
        return claim_ordered(db, &ranges, &media, host, now, true);
    }
    let Some(frontier) = frontier(db, groups, now)? else { return Ok(None) };
    let floor = frontier - LOCKSTEP_DAYS;
    let groups: Vec<(String, i64)> = groups.iter().map(|(g, oldest)| (g.clone(), (*oldest).max(floor))).collect();
    claim_newest(db, &groups, &media, host, now)
}

/// The claim of a pass over split `group` on its own backfill turn: (group, oldest day the server keeps). A day asked
/// for (`want`) of it first, else its newest day no more than `LOCKSTEP_DAYS` past the newest day any of `all`
/// (likewise, `group` among them) still has to do or is doing, else none: a group whose days left are older waits
/// for the others to come back to them (see `claim`).
pub fn claim_turn(
    db: &dyn Sql,
    group: (String, i64),
    all: &[(String, i64)],
    host: &str,
    now: i64,
) -> Result<Option<Claim>> {
    let claimed = claim_turn_any(db, group, all, host, now)?;
    if let Some(c) = &claimed {
        crate::nxv_store::live::note_claimed_day(c.day);
    }
    Ok(claimed)
}

fn claim_turn_any(
    db: &dyn Sql,
    group: (String, i64),
    all: &[(String, i64)],
    host: &str,
    now: i64,
) -> Result<Option<Claim>> {
    claim_turn_with(db, group, all, host, now, forward_from(now))
}

/// `claim_turn`, forward from day `start` when given.
fn claim_turn_with(
    db: &dyn Sql,
    group: (String, i64),
    all: &[(String, i64)],
    host: &str,
    now: i64,
    start: Option<i64>,
) -> Result<Option<Claim>> {
    if let Some(claim) = claim_wanted(db, std::slice::from_ref(&group), host, now)? {
        return Ok(Some(claim));
    }
    if let Some(start) = start {
        let from: Vec<(String, i64)> = all.iter().map(|(g, oldest)| (g.clone(), (*oldest).max(start))).collect();
        let Some(frontier) = frontier_oldest(db, &from, now)? else { return Ok(None) };
        let (g, oldest) = group;
        return claim_ordered(db, &[(g, oldest.max(start), frontier + LOCKSTEP_DAYS)], &[], host, now, true);
    }
    let Some(frontier) = frontier(db, all, now)? else { return Ok(None) };
    let (g, oldest) = group;
    claim_newest(db, &[(g, oldest.max(frontier - LOCKSTEP_DAYS))], &[], host, now)
}

/// The newest day any of `groups` still has to do or is doing: (group, oldest day the server keeps). A day pending
/// (not resting after a failure, see `FAIL_REST`) or claimed within `CLAIM_TIMEOUT`: a day being read holds the
/// groups behind it (`claim`), one whose claim went stale (its worker was killed) doesnt, it is anyone's to take over
/// (`claim` takes it first). Only when stale claims are all that's left are they the frontier. None when none is left.
pub fn frontier(db: &dyn Sql, groups: &[(String, i64)], now: i64) -> Result<Option<i64>> {
    frontier_by(db, groups, now, true)
}

/// The oldest day any of `groups` still has to do or is doing, from each one's `oldest` on: (group, first day). The
/// forward backfill's frontier (`FORWARD_START_DAY`), as `frontier` is the usual one's, and like it not held by a
/// stale claim: on prod claims left by killed restarts held every group's forward backfill for half an hour.
pub fn frontier_oldest(db: &dyn Sql, groups: &[(String, i64)], now: i64) -> Result<Option<i64>> {
    frontier_by(db, groups, now, false)
}

/// `frontier` when `newest`, else `frontier_oldest`.
fn frontier_by(db: &dyn Sql, groups: &[(String, i64)], now: i64, newest: bool) -> Result<Option<i64>> {
    if groups.is_empty() {
        return Ok(None);
    }
    let (order, most) = if newest { ("desc", "max") } else { ("asc", "min") };
    let rows = values_rows(groups.len(), &["text", "bigint"], 0);
    let sql = format!(
        "with wanted (g, oldest) as (values {rows}),
              days (day, stale) as (
                  select (select c.day from backfill_chunks c
                          where c.grp = w.g and c.state = {PENDING} and c.day >= w.oldest and {resting}
                          order by c.day {order} limit 1), 0
                  from wanted w
                  union all
                  select (select c.day from backfill_chunks c
                          where c.grp = w.g and c.state = {CLAIMED} and c.day >= w.oldest and {live}
                          order by c.day {order} limit 1), 0
                  from wanted w
                  union all
                  select (select c.day from backfill_chunks c
                          where c.grp = w.g and c.state = {CLAIMED} and c.day >= w.oldest and not {live}
                          order by c.day {order} limit 1), 1
                  from wanted w
              )
         select coalesce({most}(case when stale = 0 then day end), {most}(day)) from days",
        resting = not_resting(now),
        live = claim_live(now)
    );
    first(db.query_uncached(&sql, &group_values(groups))?)?.map_or(Ok(None), |r| r.get(0))
}

/// Groups the big releases the indexer is after mostly come from, at least `MEDIA_MIN` each: their backfill goes
/// before the rest.
pub const MEDIA_GROUPS: usize = 16;
pub const MEDIA_MIN: i64 = 20;

/// Notes how many big releases each group holds (see `MEDIA_GROUPS`), in place of what was noted before.
pub fn set_media(db: &dyn Sql, counts: &[(String, i64)]) -> Result<()> {
    db.tx(|tx| {
        tx.execute("delete from backfill_media", vals![])?;
        for (group, n) in counts {
            tx.execute(
                "insert into backfill_media (grp, releases) values ($1, $2)
                 on conflict (grp) do update set releases = excluded.releases",
                vals![group, *n],
            )?;
        }
        Ok(())
    })
}

/// The groups of `MEDIA_GROUPS`, most big releases first.
pub fn media_groups(db: &dyn Sql) -> Result<Vec<String>> {
    db.column(
        "select grp from backfill_media where releases >= $1 order by releases desc, grp limit $2",
        vals![MEDIA_MIN, MEDIA_GROUPS as i64],
    )
}

/// The newest day asked for of `groups`, pending or with a stale claim: read from the few asked for, not the backlog.
fn claim_wanted(db: &dyn Sql, groups: &[(String, i64)], host: &str, now: i64) -> Result<Option<Claim>> {
    if !db.exists("select 1 from backfill_wanted limit 1", vals![])? {
        return Ok(None);
    }
    // the few days asked for are read and checked against `groups` here: joined in SQL with a values list of every
    // group, SQLite's plan for it walked the whole chunk table on prod, under the main connection's lock
    // a group listed twice is open from the older of its days, as the join with every entry was
    let mut oldest: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    for (g, o) in groups {
        let e = oldest.entry(g.as_str()).or_insert(*o);
        *e = (*e).min(*o);
    }
    let stale = now - CLAIM_TIMEOUT;
    let row = db.tx(|tx| {
        tx.lock("atlas.backfill_claim")?;
        // the days asked for alone, each claimed by its key below: joined to the chunks, SQLite's plan (no stats for
        // the small table) scanned all of backfill_chunks by its day index first
        let asked = tx.query("select grp, day from backfill_wanted order by day desc, grp", vals![])?;
        for r in asked {
            let (grp, day): (String, i64) = (r.get(0)?, r.get(1)?);
            if oldest.get(grp.as_str()).is_none_or(|o| day < *o) {
                continue;
            }
            let claimed = tx
                .query(
                    &format!(
                        "update backfill_chunks set state = $1, server = $2, claimed_at = $3
                         where grp = $4 and day = $5 and (state = {PENDING} or (state = {CLAIMED} and claimed_at < $6))
                         returning grp, day, coalesce(reread, 0)"
                    ),
                    vals![CLAIMED, host, now, grp.as_str(), day, stale],
                )?
                .into_iter()
                .next();
            // a day done or claimed fresh since it was asked for: the next one
            if claimed.is_some() {
                return Ok(claimed);
            }
        }
        Ok(None)
    })?;
    let Some(r) = row else { return Ok(None) };
    Ok(Some(Claim { group: r.get(0)?, day: r.get(1)?, server: host.to_string(), claimed_at: now, reread: r.get(2)? }))
}

/// The chunk `claim` takes after the days asked for: of each group its newest day pending or with a stale claim, then
/// of those the newest, a group of `media` first among equals (see `claim`).
fn claim_newest(
    db: &dyn Sql,
    groups: &[(String, i64)],
    media: &[String],
    host: &str,
    now: i64,
) -> Result<Option<Claim>> {
    let ranges: Vec<(String, i64, i64)> = groups.iter().map(|(g, oldest)| (g.clone(), *oldest, i64::MAX)).collect();
    claim_ordered(db, &ranges, media, host, now, false)
}

/// Claim one day of `ranges` (group, first day, last day): the newest pending or stale-claimed day first, or the
/// oldest when `oldest_first` (forward backfill), a group big releases come from first among equals.
fn claim_ordered(
    db: &dyn Sql,
    ranges: &[(String, i64, i64)],
    media: &[String],
    host: &str,
    now: i64,
    oldest_first: bool,
) -> Result<Option<Claim>> {
    let (order, pick, best) = if oldest_first { ("asc", "<=", "asc") } else { ("desc", ">=", "desc") };
    let n = ranges.len() * 4;
    let rows = values_rows(ranges.len(), &["text", "bigint", "bigint", "bigint"], 0);
    let sql = format!(
        "with wanted (g, oldest, newest, media) as (values {rows}),
              candidates (grp, media, p_day, s_day) as materialized (
                  select w.g, w.media,
                         (select c.day from backfill_chunks c
                          where c.grp = w.g and c.state = {PENDING} and c.day >= w.oldest and c.day <= w.newest
                            and {resting}
                          order by c.day {order} limit 1),
                         (select c.day from backfill_chunks c
                          where c.grp = w.g and c.state = {CLAIMED} and c.day >= w.oldest and c.day <= w.newest
                            and c.claimed_at < ${stale}
                          order by c.day {order} limit 1)
                  from wanted w
              ),
              best (grp, media, day) as (
                  select grp, media, case
                      when p_day is null then s_day
                      when s_day is null then p_day
                      when p_day {pick} s_day then p_day
                      else s_day
                  end
                  from candidates
              )
         update backfill_chunks set state = ${a}, server = ${b}, claimed_at = ${c}
         where (grp, day) = (
             select grp, day from best where day is not null
             order by day {best}, media desc, grp limit 1)
           -- as in `claim_wanted`: still claimable when the update gets to it, and not failed and resting since
           and ((state = {PENDING} and {rested}) or (state = {CLAIMED} and claimed_at < ${stale}))
         returning grp, day, coalesce(reread, 0)",
        resting = not_resting(now),
        rested = not_resting_of("", now),
        stale = n + 1,
        a = n + 2,
        b = n + 3,
        c = n + 4,
    );
    let mut values = Vec::with_capacity(n + 4);
    for (g, oldest, newest) in ranges {
        values.extend([g.into(), (*oldest).into(), (*newest).into(), media.contains(g).into()]);
    }
    values.extend([(now - CLAIM_TIMEOUT).into(), CLAIMED.into(), host.into(), now.into()]);
    claim_one(db, &sql, &values, host, now)
}

/// Runs a claiming update, which picks a chunk and returns (group, day, reread) of it, as a claim by `host` at
/// `now`. One at a time across connections (`lock`): two picking at once would pick the same chunk.
fn claim_one(db: &dyn Sql, sql: &str, values: &[crate::pg::sql::Value], host: &str, now: i64) -> Result<Option<Claim>> {
    let row = db.tx(|tx| {
        tx.lock("atlas.backfill_claim")?;
        Ok(tx.query_uncached(sql, values)?.into_iter().next())
    })?;
    let Some(r) = row else { return Ok(None) };
    Ok(Some(Claim { group: r.get(0)?, day: r.get(1)?, server: host.to_string(), claimed_at: now, reread: r.get(2)? }))
}

/// The first of `rows`; an error past one, which a query here never returns.
fn first(rows: Vec<crate::pg::sql::Row>) -> Result<Option<crate::pg::sql::Row>> {
    match rows.len() {
        0 | 1 => Ok(rows.into_iter().next()),
        n => Err(crate::pg::sql::Error::Value(format!("{n} rows where one was expected"))),
    }
}

/// SQL: `n` rows of parameters cast to `types`, numbered from `$first + 1`.
/// PostgreSQL needs explicit types in a `values` list.
fn values_rows(n: usize, types: &[&str], first: usize) -> String {
    let mut p = first;
    (0..n)
        .map(|_| {
            let cols: Vec<String> = types
                .iter()
                .map(|t| {
                    p += 1;
                    format!("cast(${p} as {t})")
                })
                .collect();
            format!("({})", cols.join(", "))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The parameters of `values_rows(groups.len(), &["text", "bigint"], 0)`.
fn group_values(groups: &[(String, i64)]) -> Vec<crate::pg::sql::Value> {
    groups.iter().flat_map(|(g, oldest)| [g.into(), (*oldest).into()]).collect()
}

/// SQL: the groups a reopen takes in, as the CTE `staged (name)`: its parameters from `$3` on (see `staged_values`).
/// A group no chunk has stands for nothing, soo none makes it an empty list.
fn staged(groups: &[String]) -> String {
    let rows =
        if groups.is_empty() { "(cast(null as text))".to_string() } else { values_rows(groups.len(), &["text"], 2) };
    format!("with staged (name) as (values {rows})")
}

/// The parameters of a query on `REOPENS` and `staged(groups)`.
fn staged_values(since: i64, done_before: Option<i64>, groups: &[String]) -> Vec<crate::pg::sql::Value> {
    let mut v: Vec<crate::pg::sql::Value> = vec![since.into(), done_before.into()];
    v.extend(groups.iter().map(Into::into));
    v
}

/// SQL: the day chunks done, not put back before, from day `$1` on, finished before `$2` (null for any time)
const DONE_SINCE: &str = "state = 2 and coalesce(reread, 0) = 0 and day >= $1
    and (cast($2 as bigint) is null or done_at is null or done_at < $2)";

/// SQL: the day chunks `reopen_done` takes: `DONE_SINCE`, of a staged group
fn reopens() -> String {
    format!("{DONE_SINCE} and grp in (select name from staged)")
}

/// Every day chunk of `groups` done from `since` on (a unix day; `i64::MIN` for all of them) and finished before
/// `done_before` (unix seconds; None for any time) put back to pending, to be read again whole: for what the first
/// read dropped and a later build keeps, as the headers of uploads obfuscated per article (`xref`). Articles saved
/// already are passed over on the second read. A day put back once isn't put back again (`reread`). The count.
pub fn reopen_done(db: &dyn Sql, since: i64, done_before: Option<i64>, groups: &[String]) -> Result<usize> {
    db.execute_uncached(
        &format!(
            "{staged}
             update backfill_chunks set state = {PENDING}, server = null, claimed_at = null, done_at = null,
                    failures = null, failed_at = null, reread = 1
             where {reopens}",
            staged = staged(groups),
            reopens = reopens(),
        ),
        &staged_values(since, done_before, groups),
    )
}

/// What `reopen_done` would put back: how many day chunks, over how many groups, and how many done chunks of the
/// same days are left alone for being of a group not in `groups`.
pub fn done_since(db: &dyn Sql, since: i64, done_before: Option<i64>, groups: &[String]) -> Result<(i64, i64, i64)> {
    let values = staged_values(since, done_before, groups);
    let staged = staged(groups);
    let r = first(db.query_uncached(
        &format!(
            "{staged} select count(*), count(distinct grp) from backfill_chunks where {reopens}",
            reopens = reopens()
        ),
        &values,
    )?)?
    .unwrap_or_default();
    let (chunks, over) = (r.get(0)?, r.get(1)?);
    // `not in` a list holding the null of no groups is never true: none staged leaves every done chunk out
    let untracked = first(db.query_uncached(
        &format!(
            "{staged} select count(*) from backfill_chunks
             where {DONE_SINCE} and grp not in (select name from staged where name is not null)"
        ),
        &values,
    )?)?
    .map_or(Ok(0), |r| r.get(0))?;
    Ok((chunks, over, untracked))
}

/// Mark a claimed chunk as done at `now`. False when the claim was taken over.
pub fn finish(db: &dyn Sql, claim: &Claim, now: i64) -> Result<bool> {
    db.tx(|tx| {
        let n = tx.execute(
            &format!(
                "update backfill_chunks set state = $1, done_at = $2
                 where grp = $3 and day = $4 and state = {CLAIMED} and server = $5 and claimed_at = $6"
            ),
            vals![DONE, now, &claim.group, claim.day, &claim.server, claim.claimed_at],
        )?;
        if n > 0 {
            tx.execute("delete from backfill_wanted where grp = $1 and day = $2", vals![&claim.group, claim.day])?;
        }
        Ok(n > 0)
    })
}

/// Return a claimed chunk to pending state. False when the claim was taken over.
/// Give `claim` back after its run failed: it rests before it is tried again (see `FAIL_REST`). False when the
/// claim no longer owns the chunk.
pub fn fail(db: &dyn Sql, claim: &Claim, now: i64) -> Result<bool> {
    let n = db.execute(
        &format!(
            "update backfill_chunks set state = $1, server = null, claimed_at = null,
                    failures = coalesce(failures, 0) + 1, failed_at = $2
             where grp = $3 and day = $4 and state = {CLAIMED} and server = $5 and claimed_at = $6"
        ),
        vals![PENDING, now, &claim.group, claim.day, &claim.server, claim.claimed_at],
    )?;
    Ok(n > 0)
}

/// At an indexer's start, every chunk still claimed by one of `hosts` (this indexer's servers, `nntp::server_keys`)
/// put back to pending: only one indexer runs on a database, soo they are claims of a run that was killed before it
/// could give them back, which otherwise hold their days for `CLAIM_TIMEOUT`. The count.
pub fn release_left_over(db: &dyn Sql, hosts: &[String]) -> Result<usize> {
    if hosts.is_empty() {
        return Ok(0);
    }
    // one pass over the table: the hosts in one list
    let list: Vec<String> = (1..=hosts.len()).map(|i| format!("cast(${i} as text)")).collect();
    let values: Vec<crate::pg::sql::Value> = hosts.iter().map(Into::into).collect();
    db.execute_uncached(
        &format!(
            "update backfill_chunks set state = {PENDING}, server = null, claimed_at = null
             where state = {CLAIMED} and server in ({list})",
            list = list.join(", ")
        ),
        &values,
    )
}

pub fn release(db: &dyn Sql, claim: &Claim) -> Result<bool> {
    let n = db.execute(
        &format!(
            "update backfill_chunks set state = $1, server = null, claimed_at = null
             where grp = $2 and day = $3 and state = {CLAIMED} and server = $4 and claimed_at = $5"
        ),
        vals![PENDING, &claim.group, claim.day, &claim.server, claim.claimed_at],
    )?;
    Ok(n > 0)
}

/// One group's day chunks: how many, how many done, and the days they span.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GroupChunks {
    pub done: i64,
    pub total: i64,
    /// the oldest and newest day of its chunks
    pub oldest_day: i64,
    pub newest_day: i64,
    /// the oldest day done: backfill goes newest day first, soo this is how far back it has come
    pub oldest_done_day: Option<i64>,
}

/// Every group's day chunks; none without the table.
pub fn per_group(db: &dyn Sql) -> Result<std::collections::BTreeMap<String, GroupChunks>> {
    if !db.has_table("backfill_chunks")? {
        return Ok(Default::default());
    }
    db.query(
        &format!(
            "select grp, {done}, count(*), min(day), max(day), min(case when state = {DONE} then day end)
             from backfill_chunks group by grp",
            done = count_done()
        ),
        vals![],
    )?
    .into_iter()
    .map(|r| {
        Ok((
            r.get::<String>(0)?,
            GroupChunks {
                done: r.get(1)?,
                total: r.get(2)?,
                oldest_day: r.get(3)?,
                newest_day: r.get(4)?,
                oldest_done_day: r.get(5)?,
            },
        ))
    })
    .collect()
}

/// (done, total) chunks of a group
pub fn progress(db: &dyn Sql, group: &str) -> Result<(i64, i64)> {
    let r = db
        .row(&format!("select {}, count(*) from backfill_chunks where grp = $1", count_done()), vals![group])?
        .unwrap_or_default();
    Ok((r.get(0)?, r.get(1)?))
}

/// SQL: how many of the rows are done chunks
fn count_done() -> String {
    format!("cast(coalesce(sum(case when state = {DONE} then 1 else 0 end), 0) as bigint)")
}

/// the oldest day of a group's chunks
pub fn oldest_day(db: &dyn Sql, group: &str) -> Result<Option<i64>> {
    db.one("select min(day) from backfill_chunks where grp = $1", vals![group])
}

/// the newest day of a group's chunks
pub fn newest_day(db: &dyn Sql, group: &str) -> Result<Option<i64>> {
    db.one("select max(day) from backfill_chunks where grp = $1", vals![group])
}

/// The server whose first article of a split group is the oldest: the one
/// to index the split's oldest day, which no server keeps whole.
pub fn deepest(db: &dyn Sql, group: &str) -> Result<Option<String>> {
    db.opt("select server from backfill_deepest where grp = $1", vals![group])
}

/// Note `server` as the one going back furthest on `group`, from `oldest_at`
/// (when its first article was posted, unix seconds).
pub fn set_deepest(db: &dyn Sql, group: &str, server: &str, oldest_at: i64) -> Result<()> {
    // in place of what was noted, the runner-up's retention too
    db.execute(
        "insert into backfill_deepest (grp, server, oldest_at) values ($1, $2, $3)
         on conflict (grp) do update set server = excluded.server, oldest_at = excluded.oldest_at, runner_up_at = null",
        vals![group, server, oldest_at],
    )?;
    Ok(())
}

/// Note how far back the server going back second furthest on `group`
/// goes (unix seconds): days before it are the furthest one's alone.
pub fn set_runner_up(db: &dyn Sql, group: &str, runner_up_at: i64) -> Result<()> {
    db.execute("update backfill_deepest set runner_up_at = $1 where grp = $2", vals![runner_up_at, group])?;
    Ok(())
}

/// How far back the server going back second furthest on `group` goes
/// (unix seconds), when noted with the furthest one.
pub fn runner_up(db: &dyn Sql, group: &str) -> Result<Option<i64>> {
    Ok(db.opt::<Option<i64>>("select runner_up_at from backfill_deepest where grp = $1", vals![group])?.flatten())
}

/// Forget `server` as the one going back furthest on `group`, when it no
/// longer carries it or no longer keeps the oldest day: the next to reach
/// the oldest day asks again.
pub fn forget_deepest(db: &dyn Sql, group: &str, server: &str) -> Result<()> {
    db.execute("delete from backfill_deepest where grp = $1 and server = $2", vals![group, server])?;
    Ok(())
}

/// Whether any chunk of `groups`: (group, oldest day the server keeps) is
/// still to do (pending or claimed) on a day the server keeps.
pub fn waiting(db: &dyn Sql, groups: &[(String, i64)]) -> Result<bool> {
    for (g, oldest) in groups {
        if db.exists(
            "select 1 from backfill_chunks where grp = $1 and day >= $2 and state != 2 limit 1",
            vals![g, *oldest],
        )? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether every day chunk of split `group` is done.
pub fn chunks_done(db: &dyn Sql, group: &str) -> Result<bool> {
    Ok(is_split(db, group)?
        && !db.exists("select 1 from backfill_chunks where grp = $1 and state != 2 limit 1", vals![group])?)
}

/// The split generation of `group`: counts the times its split reached back
/// (see `reach_back`), which undoes what a sweep covered.
pub fn sweep_generation(db: &dyn Sql, group: &str) -> Result<i64> {
    Ok(db.opt("select gen from backfill_sweep_gen where grp = $1", vals![group])?.unwrap_or(0))
}

/// Note `server`'s sweep of split `group` as done: its cursor backfill got
/// down to its first article after the day chunks (see `indexer::run_sweep`).
/// `generation` is the group's (see `sweep_generation`) when the sweep was
/// chosen: one from before the split reached back is stale and notes nothing.
pub fn set_swept(db: &dyn Sql, group: &str, server: &str, generation: i64) -> Result<()> {
    db.execute(
        "insert into backfill_sweeps (grp, server)
         select cast($1 as text), cast($2 as text)
         where coalesce((select gen from backfill_sweep_gen where grp = $1), 0) = $3
         on conflict (grp, server) do nothing",
        vals![group, server, generation],
    )?;
    Ok(())
}

/// Whether `server`'s sweep of split `group` is done.
pub fn swept(db: &dyn Sql, group: &str, server: &str) -> Result<bool> {
    db.exists("select 1 from backfill_sweeps where grp = $1 and server = $2", vals![group, server])
}

/// Whether split `group` is complete: every day chunk done and every one of
/// `servers` (the ones carrying it) swept down to its first article.
pub fn complete(db: &dyn Sql, group: &str, servers: &[String]) -> Result<bool> {
    if !chunks_done(db, group)? {
        return Ok(false);
    }
    for s in servers {
        if !swept(db, group, s)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// groups with chunks still to do
pub fn split_groups(db: &dyn Sql) -> Result<Vec<String>> {
    db.column("select distinct grp from backfill_chunks where state != 2 order by grp", vals![])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_chunk_rests_and_doesnt_hold_the_others() {
        both(|c| {
            // `new` has the newest day (2024-11-03), `old` only October
            add(c, "new", 20_030, 20_030).unwrap();
            add(c, "old", 20_020, 20_019).unwrap();
            let all: Vec<(String, i64)> = ["new", "old"].iter().map(|g| (g.to_string(), i64::MIN)).collect();
            let turn = |g: &str, host: &str, now| claim_turn(c, (g.to_string(), i64::MIN), &all, host, now).unwrap();
            assert!(turn("old", "a", 1000).is_none(), "new's day first: old waits");
            let new = turn("new", "a", 1000).unwrap();
            assert!(fail(c, &new, 1000).unwrap());
            assert!(!fail(c, &new, 1000).unwrap(), "a claim gives it back once");
            // resting, it neither is taken nor holds the others
            assert!(turn("new", "b", 1001).is_none());
            let old = turn("old", "b", 1001).expect("old goes on while new's day rests");
            assert_eq!(old.day, 20_020);
            assert_eq!(
                claim(c, &all, "c", 1001).unwrap().map(|k| k.day),
                Some(20_019),
                "nor does a free worker take it"
            );
            // after its rest it is the newest day again, and a second failure rests twice as long
            let again = turn("new", "b", 1000 + FAIL_REST).unwrap();
            assert!(fail(c, &again, 1000 + FAIL_REST).unwrap());
            assert!(turn("new", "b", 1000 + 2 * FAIL_REST).is_none());
            assert!(turn("new", "b", 1000 + 3 * FAIL_REST).is_some());
            // released (stopped, or a day this server lacks), it is taken again at once
            wipe(c);
            add(c, "g", 20_030, 20_030).unwrap();
            let g = vec![("g".to_string(), i64::MIN)];
            let k = claim(c, &g, "a", 0).unwrap().unwrap();
            assert!(release(c, &k).unwrap());
            assert!(claim(c, &g, "b", 1).unwrap().is_some());
        });
    }

    #[test]
    fn per_group_counts_and_spans_each_groups_chunks() {
        both(|c| {
            assert!(per_group(c).unwrap().is_empty(), "fresh catalog has no chunks");
            add(c, "g", 100, 97).unwrap();
            let claim = claim(c, &[("g".to_string(), i64::MIN)], "s", 1000).unwrap().unwrap();
            assert!(finish(c, &claim, 1100).unwrap());
            let g = per_group(c).unwrap()["g"];
            assert_eq!((g.done, g.total, g.oldest_day, g.newest_day), (1, 4, 97, 100));
            assert_eq!(g.oldest_done_day, Some(claim.day));
        });
    }

    /// `body` on a freshly migrated PostgreSQL schema.
    fn both(body: impl Fn(&dyn Sql)) {
        crate::pg::testing::each(body);
    }

    /// every chunk and note gone, as in a new database
    fn wipe(c: &dyn Sql) {
        for t in [
            "backfill_chunks",
            "backfill_deepest",
            "backfill_sweeps",
            "backfill_sweep_gen",
            "backfill_speculative",
            "backfill_media",
            "backfill_wanted",
        ] {
            c.execute(&format!("delete from {t}"), vals![]).unwrap();
        }
    }

    /// the second furthest server's retention is kept with the furthest,
    /// and goes with it
    #[test]
    fn the_runner_up_is_kept_with_the_deepest() {
        both(|c| {
            set_deepest(c, "g", "a", 100).unwrap();
            assert_eq!(runner_up(c, "g").unwrap(), None);
            set_runner_up(c, "g", 200).unwrap();
            assert_eq!(runner_up(c, "g").unwrap(), Some(200));
            forget_deepest(c, "g", "a").unwrap();
            assert_eq!(runner_up(c, "g").unwrap(), None);
        });
    }

    #[test]
    fn reaching_back_keeps_to_the_speculative_window() {
        both(|c| {
            // days 100.. the next deepest keeps, 40..99 the window, older pruned
            add(c, "g", 120, 40).unwrap();
            set_speculative(c, "g", &Speculative { runner_up_day: 100, reach: 60, days: 3, articles: 7 }).unwrap();
            set_swept(c, "g", "a", 0).unwrap();
            // the hourly look finds the deep server back in 2000 again: nothing
            assert_eq!(reach_back(c, "g", 10, 10 * 86_400).unwrap(), 0);
            assert_eq!(oldest_day(c, "g").unwrap(), Some(40));
            assert!(swept(c, "g", "a").unwrap(), "the sweep isnt to do again");
            // the window kept across a restart
            assert_eq!(speculative(c, "g").unwrap().map(|s| (s.reach, s.days, s.articles)), Some((60, 3, 7)));
            // a wider window: the days in it, sweeps left alone
            set_speculative(c, "g", &Speculative { runner_up_day: 100, reach: 70, days: 3, articles: 7 }).unwrap();
            assert_eq!(reach_back(c, "g", 5, 5 * 86_400).unwrap(), 10);
            assert_eq!(oldest_day(c, "g").unwrap(), Some(30));
            assert!(swept(c, "g", "a").unwrap());
        });
    }

    /// the day of a claim
    fn day(claimed: Result<Option<Claim>>) -> Option<i64> {
        claimed.unwrap().map(|c| c.day)
    }

    #[test]
    fn days_are_claimed_newest_first_and_once() {
        both(|c| {
            assert_eq!(add(c, "g", 20_000, 19_998).unwrap(), 3);
            assert_eq!(add(c, "g", 20_000, 19_998).unwrap(), 0, "adding again adds nothing");
            assert!(is_split(c, "g").unwrap());
            let groups = vec![("g".to_string(), i64::MIN)];

            let a = claim(c, &groups, "a", 1000).unwrap().unwrap();
            assert_eq!(
                a,
                Claim { group: "g".into(), day: 20_000, server: "a".into(), claimed_at: 1000, reread: false }
            );
            let b = claim(c, &groups, "b", 1000).unwrap().unwrap();
            assert_eq!(b.day, 19_999);
            assert!(finish(c, &a, 1100).unwrap());
            assert!(release(c, &b).unwrap());
            assert_eq!(day(claim(c, &groups, "b", 1000)), Some(19_999), "released goes back");
            assert_eq!(day(claim(c, &groups, "a", 1000)), Some(19_998));
            assert_eq!(claim(c, &groups, "a", 1000).unwrap(), None);
            assert_eq!(progress(c, "g").unwrap(), (1, 3));
        });
    }

    #[test]
    fn the_groups_big_releases_come_from_are_backfilled_first() {
        both(|c| {
            // days 20_000..=20_010 are 2024-10-04..=14, one month
            add(c, "plain", 20_010, 20_000).unwrap();
            add(c, "media", 20_010, 20_000).unwrap();
            add(c, "few", 20_010, 20_000).unwrap();
            let groups: Vec<(String, i64)> =
                ["plain", "media", "few"].iter().map(|g| (g.to_string(), 20_005)).collect();
            // ties go by group name, `media` before `plain`: start where that wouldn't show
            let first = claim(c, &groups, "a", 0).unwrap().unwrap();
            assert_eq!(first.day, 20_010);
            assert!(finish(c, &first, 0).unwrap());
            set_media(c, &[("media".into(), 500), ("few".into(), MEDIA_MIN - 1)]).unwrap();
            let mut got = Vec::new();
            while let Some(claim) = claim(c, &groups, "a", 0).unwrap() {
                assert!(finish(c, &claim, 0).unwrap());
                got.push((claim.group, claim.day));
            }
            // day by day, newest first, and of each day `media` first
            assert!(got.windows(2).all(|w| w[0].1 >= w[1].1), "{got:?}");
            for d in 20_005..20_010 {
                assert_eq!(got.iter().find(|(_, day)| *day == d).map(|(g, _)| g.as_str()), Some("media"), "{got:?}");
            }
            // noted again from scratch
            set_media(c, &[("plain".into(), 30)]).unwrap();
            assert_eq!(media_groups(c).unwrap(), ["plain"]);
        });
    }

    /// Every group's backfill goes back day by day together: no group claims a day more than `LOCKSTEP_DAYS` past
    /// the newest day any of them still has to do or is doing, however far back its own days go.
    #[test]
    fn groups_are_backfilled_in_lockstep() {
        both(|c| {
            // `deep` (a media group) 2024-08-25..=10-24, `recent` 2024-10-04..=11-03
            add(c, "deep", 20_020, 19_960).unwrap();
            add(c, "recent", 20_030, 20_000).unwrap();
            set_media(c, &[("deep".into(), 500)]).unwrap();
            let groups: Vec<(String, i64)> = ["deep", "recent"].iter().map(|g| (g.to_string(), i64::MIN)).collect();
            let mut got = Vec::new();
            while let Some(claim) = claim(c, &groups, "a", 0).unwrap() {
                assert!(finish(c, &claim, 0).unwrap());
                got.push((claim.group, claim.day));
            }
            assert_eq!(got.len(), 61 + 31);
            assert!(got.windows(2).all(|w| w[0].1 >= w[1].1), "newest day first over both: {got:?}");
            assert_eq!(got[10], ("deep".to_string(), 20_020), "a media group first among equals");

            // a day being read holds the rest: the next day goes, not one further back
            wipe(c);
            add(c, "big", 20_030, 20_020).unwrap();
            add(c, "small", 20_030, 20_020).unwrap();
            let groups: Vec<(String, i64)> = ["big", "small"].iter().map(|g| (g.to_string(), i64::MIN)).collect();
            let big = claim(c, &groups, "a", 1000).unwrap().unwrap();
            assert_eq!((big.group.as_str(), big.day), ("big", 20_030));
            let mut small = Vec::new();
            while let Some(k) = claim(c, &groups, "b", 1000).unwrap() {
                assert!(finish(c, &k, 1000).unwrap());
                small.push((k.group, k.day));
            }
            assert!(small.iter().all(|(_, d)| *d >= 20_030 - LOCKSTEP_DAYS), "{small:?}");
            assert_eq!(small.len(), 3, "small's 20030 and 20029, big's 20029: {small:?}");
            // once big's day is read, the rest move on a day
            assert!(finish(c, &big, 1000).unwrap());
            assert_eq!(day(claim(c, &groups, "b", 1000)), Some(20_028));
            // a stale claim is taken over first, and holds the rest meanwhile
            wipe(c);
            add(c, "g", 20_030, 20_026).unwrap();
            let groups = vec![("g".to_string(), i64::MIN)];
            let first = claim(c, &groups, "a", 1000).unwrap().unwrap();
            assert_eq!(day(claim(c, &groups, "a", 1000)), Some(20_029));
            assert_eq!(claim(c, &groups, "a", 1000).unwrap(), None, "20030 is still being read");
            let late = claim(c, &groups, "b", 1000 + CLAIM_TIMEOUT + 1).unwrap().unwrap();
            assert_eq!(late.day, first.day, "the stale claim first");
        });
    }

    /// A split group's own backfill turn takes a day no more than `LOCKSTEP_DAYS` past the newest any tracked split
    /// group has left, or a day asked for, and otherwise nothing: one whose days left are years old waits for the
    /// others to come back to them.
    #[test]
    fn a_groups_own_turn_waits_for_the_others() {
        both(|c| {
            // `old` 2022-04-04..=13, `recent` 2024-10-30..=11-03
            add(c, "old", 19_095, 19_086).unwrap();
            add(c, "recent", 20_030, 20_026).unwrap();
            let all: Vec<(String, i64)> = ["old", "recent"].iter().map(|g| (g.to_string(), i64::MIN)).collect();
            let turn = |g: &str, now: i64| claim_turn(c, (g.to_string(), i64::MIN), &all, "a", now).unwrap();
            assert_eq!(frontier(c, &all, 0).unwrap(), Some(20_030));
            assert_eq!(turn("old", 0), None, "nothing while recent's days are left");
            // a day asked for goes anyway
            assert!(want(c, "old", 19_090, 0).unwrap());
            let asked = turn("old", 0).unwrap();
            assert_eq!(asked.day, 19_090);
            assert!(finish(c, &asked, 0).unwrap());
            assert_eq!(turn("old", 0), None);
            // `recent` takes its days one by one as each is read
            let mut days = Vec::new();
            while let Some(k) = turn("recent", 0) {
                assert!(finish(c, &k, 0).unwrap());
                days.push(k.day);
            }
            assert_eq!(days, [20_030, 20_029, 20_028, 20_027, 20_026]);
            // recent all read: `old`'s days are the newest left
            assert_eq!(turn("old", 0).map(|c| c.day), Some(19_095));
            assert_eq!(frontier(c, &[], 0).unwrap(), None);
        });
    }

    #[test]
    fn days_done_are_put_back_to_be_read_again_from_a_day_on() {
        both(|c| {
            add(c, "g", 20_010, 20_000).unwrap();
            add(c, "other", 20_010, 20_009).unwrap();
            let groups = [("g".to_string(), i64::MIN), ("other".to_string(), i64::MIN)];
            // g's five newest done at 1000, then other's two at 5000
            for _ in 0..5 {
                let k = claim(c, &[groups[0].clone()], "a", 1000).unwrap().unwrap();
                assert!(finish(c, &k, 1000).unwrap());
            }
            for _ in 0..2 {
                let k = claim(c, &[groups[1].clone()], "a", 5000).unwrap().unwrap();
                assert!(finish(c, &k, 5000).unwrap());
            }
            let g = vec!["g".to_string()];
            assert_eq!(done_since(c, i64::MIN, None, &g).unwrap(), (5, 1, 2), "other's two aren't a group taken in");
            assert_eq!(done_since(c, 20_008, None, &g).unwrap(), (3, 1, 2));
            let both = vec!["g".to_string(), "other".to_string()];
            assert_eq!(done_since(c, i64::MIN, Some(2000), &both).unwrap(), (5, 1, 0), "other's were finished after");
            assert_eq!(reopen_done(c, 20_008, None, &g).unwrap(), 3);
            assert_eq!(progress(c, "g").unwrap(), (2, 11), "20006 and 20007 stay done");
            // read again newest first, before the days never read, and marked as read again
            let k = claim(c, &[groups[0].clone()], "a", 1000).unwrap().unwrap();
            assert_eq!((k.day, k.reread), (20_010, true));
            assert!(finish(c, &k, 1000).unwrap());
            // put back again: the rest of g, not what was put back once (done again or not)
            assert_eq!(reopen_done(c, i64::MIN, None, &g).unwrap(), 2);
            assert_eq!(reopen_done(c, i64::MIN, None, &g).unwrap(), 0);
            assert_eq!(progress(c, "g").unwrap(), (1, 11), "only the day read again is done");
        });
    }

    #[test]
    fn a_day_asked_for_is_claimed_first() {
        both(|c| {
            add(c, "g", 20_000, 19_990).unwrap();
            add(c, "h", 20_000, 19_990).unwrap();
            let groups = vec![("g".to_string(), i64::MIN), ("h".to_string(), i64::MIN)];
            assert!(want(c, "h", 19_993, 500).unwrap());
            assert!(!want(c, "h", 18_000, 500).unwrap(), "no chunk for that day");
            let a = claim(c, &groups, "a", 1000).unwrap().unwrap();
            assert_eq!((a.group.as_str(), a.day), ("h", 19_993));
            // claimed by another: the rest go newest first again
            assert_eq!(day(claim(c, &groups, "b", 1000)), Some(20_000));
            // a claim gone stale is taken over before the rest, and a day once done isn't asked for again
            let late = claim(c, &groups, "c", 1000 + CLAIM_TIMEOUT + 1).unwrap().unwrap();
            assert_eq!((late.group.as_str(), late.day), ("h", 19_993));
            assert!(finish(c, &late, 5000).unwrap());
            assert!(!want(c, "h", 19_993, 6000).unwrap());
            let left: i64 = c.one("select count(*) from backfill_wanted", vals![]).unwrap();
            assert_eq!(left, 0);
        });
    }

    #[test]
    fn the_oldest_day_is_redone_when_retention_reaches_further_into_it() {
        both(|c| {
            let done = |c: &dyn Sql| {
                c.execute("update backfill_chunks set state = 2, server = 'a', done_at = 1 where day = 10", vals![])
                    .unwrap()
            };
            let state =
                |c: &dyn Sql| -> i64 { c.one("select state from backfill_chunks where day = 10", vals![]).unwrap() };
            add(c, "g", 12, 10).unwrap();
            done(c);
            // "a" went back furthest, to 18:00 on day 10
            set_deepest(c, "g", "a", 10 * 86_400 + 18 * 3600).unwrap();

            assert_eq!(reach_back(c, "g", 10, 10 * 86_400 + 18 * 3600).unwrap(), 0, "no further");
            assert_eq!(reach_back(c, "g", 10, 10 * 86_400 + 20 * 3600).unwrap(), 0, "less far");
            assert_eq!((state(c), deepest(c, "g").unwrap().as_deref()), (2, Some("a")));

            assert_eq!(reach_back(c, "g", 10, 10 * 86_400 + 3600).unwrap(), 1, "01:00 the same day");
            assert_eq!((state(c), deepest(c, "g").unwrap()), (0, None), "to do again, deepest asked again");

            // a deepest noted before its time was: nothing to compare, left alone
            done(c);
            c.execute("insert into backfill_deepest (grp, server) values ('g', 'a')", vals![]).unwrap();
            assert_eq!(reach_back(c, "g", 10, 10 * 86_400).unwrap(), 0);
            assert_eq!(state(c), 2);
            // a day before it still reaches back
            assert_eq!(reach_back(c, "g", 9, 9 * 86_400).unwrap(), 2, "day 9 added, day 10 to do again");
        });
    }

    #[test]
    fn a_claimed_oldest_day_is_reset_when_retention_reaches_further_into_it() {
        both(|c| {
            add(c, "g", 12, 10).unwrap();
            let groups = vec![("g".to_string(), i64::MIN)];
            // a reads day 12, then day 11, then takes day 10 (the split's oldest), as the deepest
            for _ in 0..2 {
                let k = claim(c, &groups, "a", 1000).unwrap().unwrap();
                assert!(finish(c, &k, 1000).unwrap());
            }
            let a = claim(c, &groups, "a", 1000).unwrap().unwrap();
            assert_eq!(a.day, 10);
            set_deepest(c, "g", "a", 10 * 86_400 + 18 * 3600).unwrap();

            // b goes back further into day 10 while a is still on it
            assert_eq!(reach_back(c, "g", 10, 10 * 86_400 + 3600).unwrap(), 1);
            assert_eq!(deepest(c, "g").unwrap(), None);
            let r = c
                .row("select state, server, claimed_at from backfill_chunks where day = 10", vals![])
                .unwrap()
                .unwrap();
            let row: (i64, Option<String>, Option<i64>) = (r.get(0).unwrap(), r.get(1).unwrap(), r.get(2).unwrap());
            assert_eq!(row, (PENDING, None, None), "pending again, a's claim gone");

            // a's late finish and release do nothing
            assert!(!finish(c, &a, 2000).unwrap());
            assert!(!release(c, &a).unwrap());
            assert_eq!(progress(c, "g").unwrap(), (2, 3), "days 12 and 11 read");
            // and the next to claim it is not a stale claim's
            assert_eq!(day(claim(c, &groups, "b", 1001)), Some(10));
        });
    }

    /// a carrier swept down to its first article before the split reached
    /// back sweeps again: its first article may be older now
    #[test]
    fn reaching_back_undoes_the_sweeps() {
        both(|c| {
            add(c, "g", 12, 10).unwrap();
            c.execute("update backfill_chunks set state = 2", vals![]).unwrap();
            set_deepest(c, "g", "a", 10 * 86_400 + 18 * 3600).unwrap();
            set_swept(c, "g", "b", 0).unwrap();
            set_swept(c, "other", "b", 0).unwrap();

            assert_eq!(reach_back(c, "g", 10, 10 * 86_400 + 20 * 3600).unwrap(), 0, "not further");
            assert!(swept(c, "g", "b").unwrap(), "kept when nothing changed");

            assert_eq!(reach_back(c, "g", 9, 9 * 86_400).unwrap(), 2);
            assert!(!swept(c, "g", "b").unwrap(), "to sweep again");
            assert!(swept(c, "other", "b").unwrap(), "other groups' sweeps stay");
        });
    }

    /// a sweep chosen before the split reached back, finishing after, doesnt
    /// note itself done: it covered what is older now only down to the old first article
    #[test]
    fn a_sweep_from_before_reaching_back_is_not_noted() {
        both(|c| {
            add(c, "g", 12, 10).unwrap();
            c.execute("update backfill_chunks set state = 2", vals![]).unwrap();
            set_deepest(c, "g", "a", 10 * 86_400 + 18 * 3600).unwrap();
            let started = sweep_generation(c, "g").unwrap();

            assert_eq!(reach_back(c, "g", 9, 9 * 86_400).unwrap(), 2);
            set_swept(c, "g", "b", started).unwrap();
            assert!(!swept(c, "g", "b").unwrap(), "stale, left to do");

            let now = sweep_generation(c, "g").unwrap();
            assert_ne!(now, started);
            set_swept(c, "g", "b", now).unwrap();
            assert!(swept(c, "g", "b").unwrap(), "one started since is noted");
        });
    }

    #[test]
    fn days_before_are_dropped_unless_done() {
        both(|c| {
            add(c, "g", 12, 5).unwrap();
            add(c, "h", 12, 5).unwrap();
            c.execute("update backfill_chunks set state = 2 where grp = 'g' and day = 6", vals![]).unwrap();
            claim(c, &[("g".to_string(), i64::MIN)], "a", 1000).unwrap();
            assert_eq!(drop_before(c, "g", 8).unwrap(), 2, "days 5 and 7");
            assert_eq!(oldest_day(c, "g").unwrap(), Some(6), "a done day stays");
            assert_eq!(progress(c, "h").unwrap(), (0, 8), "other groups untouched");
        });
    }

    #[test]
    fn stale_claims_are_taken_over() {
        both(|c| {
            add(c, "g", 5, 5).unwrap();
            let groups = vec![("g".to_string(), i64::MIN)];
            assert!(claim(c, &groups, "a", 1000).unwrap().is_some());
            assert_eq!(claim(c, &groups, "b", 1000 + CLAIM_TIMEOUT - 1).unwrap(), None);
            assert_eq!(day(claim(c, &groups, "b", 1000 + CLAIM_TIMEOUT + 1)), Some(5));
        });
    }

    #[test]
    fn a_claim_taken_over_cant_be_finished_or_given_back() {
        both(|c| {
            add(c, "g", 5, 5).unwrap();
            let groups = vec![("g".to_string(), i64::MIN)];
            // a's chunk runs past CLAIM_TIMEOUT, b takes it over and finishes it
            let a = claim(c, &groups, "a", 1000).unwrap().unwrap();
            let b = claim(c, &groups, "b", 1000 + CLAIM_TIMEOUT + 1).unwrap().unwrap();
            let done_at = 1000 + CLAIM_TIMEOUT + 60;
            assert!(finish(c, &b, done_at).unwrap());

            // a's late release and finish change nothing: done, and b's
            assert!(!release(c, &a).unwrap());
            assert!(!finish(c, &a, done_at + 60).unwrap());
            let row = |c: &dyn Sql| {
                let r =
                    c.row("select state, server, done_at from backfill_chunks where grp = 'g' and day = 5", vals![]);
                let r = r.unwrap().unwrap();
                (r.get::<i64>(0).unwrap(), r.get::<Option<String>>(1).unwrap(), r.get::<Option<i64>>(2).unwrap())
            };
            assert_eq!(row(c), (DONE, Some("b".to_string()), Some(done_at)));
        });
    }

    #[test]
    fn only_listed_groups_and_unsplit_groups() {
        both(|c| {
            add(c, "g", 5, 5).unwrap();
            assert_eq!(claim(c, &[("other".to_string(), i64::MIN)], "a", 0).unwrap(), None);
            assert!(!is_split(c, "other").unwrap());
            assert_eq!(split_groups(c).unwrap(), vec!["g".to_string()]);
            assert_eq!(unix_day(86_400 * 3 + 5), 3);
        });
    }

    #[test]
    fn forward_backfill_reads_from_its_start_oldest_first_with_every_group_together() {
        both(|c| {
            add(c, "a", 20_010, 19_990).unwrap();
            add(c, "b", 20_010, 19_990).unwrap();
            let all: Vec<(String, i64)> = ["a", "b"].iter().map(|g| (g.to_string(), i64::MIN)).collect();
            // from day 20_000 on: nothing older, oldest first, neither group more than a day ahead of the other
            let mut read = Vec::new();
            while let Some(k) = claim_with(c, &all, "h", 0, Some(20_000)).unwrap() {
                if k.group == "a" && k.day == 20_003 {
                    // `a`'s day 20_003 is slow: the rest may go a day past it, no further
                    let mut ahead = Vec::new();
                    while let Some(k2) = claim_with(c, &all, "h", 0, Some(20_000)).unwrap() {
                        ahead.push(k2.day);
                        assert!(finish(c, &k2, 0).unwrap());
                    }
                    assert!(!ahead.is_empty() && ahead.iter().all(|d| *d <= 20_004), "{ahead:?}");
                    read.extend(ahead);
                }
                assert!(k.day >= 20_000);
                read.push(k.day);
                assert!(finish(c, &k, 0).unwrap());
            }
            assert_eq!(read.len(), 22, "both groups' days from the start: {read:?}");
            assert_eq!(read.iter().min(), Some(&20_000));
            // the days before the start are still to do, for a backward run
            assert_eq!(frontier(c, &all, 0).unwrap(), Some(19_999));
        });
    }

    #[test]
    fn forward_backfill_on_a_groups_own_turn_waits_for_the_others_and_passes_a_resting_day() {
        both(|c| {
            add(c, "a", 20_005, 19_995).unwrap();
            add(c, "b", 20_005, 19_995).unwrap();
            let all: Vec<(String, i64)> = ["a", "b"].iter().map(|g| (g.to_string(), i64::MIN)).collect();
            let turn = |g: &str, now: i64| {
                claim_turn_with(c, (g.to_string(), i64::MIN), &all, "h", now, Some(20_000)).unwrap()
            };
            // `b` reads its first two days: then waits for `a`, a day past the oldest left
            for d in [20_000, 20_001] {
                let k = turn("b", 0).unwrap();
                assert_eq!(k.day, d);
                assert!(finish(c, &k, 0).unwrap());
            }
            assert_eq!(turn("b", 0), None, "no further than a day past a's 20_000");
            // `a`'s 20_000 fails and rests: the others go on past it, and it comes back first once rested
            let k = turn("a", 0).unwrap();
            assert_eq!(k.day, 20_000);
            assert!(fail(c, &k, 10).unwrap());
            assert_eq!(turn("a", 10).map(|k| k.day), Some(20_001));
            let later = 10 + FAIL_REST + 1;
            assert_eq!(turn("a", later).map(|k| k.day), Some(20_000), "rested, the oldest again");
        });
    }

    #[test]
    fn the_forward_start_is_worked_out_once_and_kept() {
        both(|c| {
            let day = |d: i64| d * 86_400 + 5;
            // the global isn't touched here: other tests claim in parallel
            assert_eq!(forward_start_day(c, Some(30), day(20_100)).unwrap(), 20_070);
            assert_eq!(forward_start_day(c, Some(30), day(20_105)).unwrap(), 20_070, "a later start keeps it");
            assert_eq!(forward_start_day(c, Some(10), day(20_105)).unwrap(), 20_095, "a new count, a new start");
            assert_eq!(forward_start_day(c, None, day(20_105)).unwrap(), 0);
            assert_eq!(forward_start_day(c, Some(10), day(20_200)).unwrap(), 20_190, "on again: from then");
        });
    }

    #[test]
    fn days_older_than_a_servers_oldest_are_left_to_others() {
        both(|c| {
            add(c, "g", 12, 10).unwrap();
            let from_11 = [("g".to_string(), 11)];
            for d in [12, 11] {
                let k = claim(c, &from_11, "short", 0).unwrap().unwrap();
                assert_eq!(k.day, d);
                assert!(finish(c, &k, 0).unwrap());
            }
            assert_eq!(day(claim(c, &from_11, "short", 0)), None, "day 10 is older than it keeps");
            assert_eq!(day(claim(c, &[("g".to_string(), i64::MIN)], "long", 0)), Some(10));
        });
    }

    /// A claim left by a killed run doesnt hold the frontier past `CLAIM_TIMEOUT`: the days ahead of it go on, and
    /// the stale day itself is still anyone's to take over (forward, it's the oldest soo it goes first).
    #[test]
    fn a_stale_claim_doesnt_hold_the_frontier() {
        both(|c| {
            add(c, "big", 20_005, 20_000).unwrap();
            add(c, "small", 20_005, 20_000).unwrap();
            let all: Vec<(String, i64)> = ["big", "small"].iter().map(|g| (g.to_string(), i64::MIN)).collect();
            // forward from 20_000: big's 20_000 claimed by a run that then died, small reads its first two days
            let big = claim_turn_with(c, ("big".into(), i64::MIN), &all, "h", 1000, Some(20_000)).unwrap().unwrap();
            assert_eq!(big.day, 20_000);
            for d in [20_000, 20_001] {
                let k = claim_turn_with(c, ("small".into(), i64::MIN), &all, "h", 1000, Some(20_000)).unwrap().unwrap();
                assert_eq!(k.day, d);
                assert!(finish(c, &k, 1000).unwrap());
            }
            let small = || claim_turn_with(c, ("small".into(), i64::MIN), &all, "h", 2000, Some(20_000)).unwrap();
            assert_eq!(frontier_oldest(c, &all, 2000).unwrap(), Some(20_000), "a fresh claim holds it");
            assert_eq!(small(), None);
            // past the timeout big's claim no longer counts: the frontier is big's next pending day
            let late = 1000 + CLAIM_TIMEOUT + 1;
            assert_eq!(frontier_oldest(c, &all, late).unwrap(), Some(20_001));
            assert_eq!(frontier(c, &all, late).unwrap(), Some(20_005));
            // and the stale day is taken over first
            let k = claim_with(c, &all, "h2", late, Some(20_000)).unwrap().unwrap();
            assert_eq!((k.group.as_str(), k.day), ("big", 20_000));
            assert!(!finish(c, &big, late).unwrap(), "the dead run's claim is no longer its own");
        });
    }

    /// At start, every chunk still claimed by one of the indexer's servers goes back to pending; another server's
    /// claims, pending and done days are left alone.
    #[test]
    fn a_start_releases_its_servers_left_over_claims() {
        both(|c| {
            let one = |g: &str| vec![(g.to_string(), i64::MIN)];
            for g in ["a1", "a2", "other", "done"] {
                add(c, g, 20_005, 20_005).unwrap();
            }
            let a1 = claim(c, &one("a1"), "a", 1000).unwrap().unwrap();
            let a2 = claim(c, &one("a2"), "a:119", 1000).unwrap().unwrap();
            let other = claim(c, &one("other"), "other", 1000).unwrap().unwrap();
            let done = claim(c, &one("done"), "a", 1000).unwrap().unwrap();
            assert!(finish(c, &done, 1000).unwrap());
            assert_eq!(release_left_over(c, &[]).unwrap(), 0);
            assert_eq!(release_left_over(c, &["a".into(), "a:119".into()]).unwrap(), 2);
            let state = |k: &Claim| -> (i64, Option<String>) {
                let r = c.row(
                    "select state, server from backfill_chunks where grp = $1 and day = $2",
                    vals![&k.group, k.day],
                );
                let r = r.unwrap().unwrap();
                (r.get(0).unwrap(), r.get(1).unwrap())
            };
            assert_eq!(state(&a1), (PENDING, None));
            assert_eq!(state(&a2), (PENDING, None));
            assert_eq!(state(&other), (CLAIMED, Some("other".into())));
            assert_eq!(state(&done).0, DONE);
            // the released days are claimed again at once, not after CLAIM_TIMEOUT
            assert_eq!(day(claim(c, &one("a1"), "a", 1001)), Some(20_005));
        });
    }

    #[test]
    #[ignore = "requires explicitly admitted isolated PostgreSQL"]
    fn claim_is_atomic_with_concurrent_connections() {
        use crate::pg::sql::Pg;
        let schema = crate::pg::testing::Schema::new().expect("explicit ATLAS_PG_TEST_URL is required");
        let seed = Pg::new(schema.pool.get().unwrap());
        add(&seed, "g", 20, 19).unwrap();
        drop(seed);
        let a = Pg::new(schema.pool.get().unwrap());
        let b = Pg::new(schema.pool.get().unwrap());
        let groups = vec![("g".to_string(), i64::MIN)];
        let a_claim = claim(&a, &groups, "a", 1000).unwrap().unwrap();
        let b_claim = claim(&b, &groups, "b", 1000).unwrap().unwrap();
        assert_ne!(a_claim.day, b_claim.day);
        assert_eq!((a_claim.day, b_claim.day), (20, 19));
        let c = Pg::new(schema.pool.get().unwrap());
        assert_eq!(claim(&c, &groups, "c", 1000).unwrap(), None);
    }

    /// Claims on Postgres take turns (`Sql::lock`): workers claiming at once on their own connections each get a day of
    /// their own, none twice
    #[test]
    fn claims_at_once_on_postgres_get_a_day_each() {
        use crate::pg::sql::Pg;
        let Some(s) = crate::pg::testing::Schema::new() else { return };
        add(&Pg::new(s.pool.get().unwrap()), "g", 40, 1).unwrap();
        let groups = vec![("g".to_string(), i64::MIN)];
        let days: Vec<i64> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..4)
                .map(|w| {
                    let (pool, groups) = (&s.pool, &groups);
                    scope.spawn(move || {
                        let db = Pg::new(pool.get().unwrap());
                        let mut got = Vec::new();
                        // a day being read holds the ones a day past it (`LOCKSTEP_DAYS`): one goes at a time
                        let mut idle = 0;
                        while idle < 50 {
                            match claim(&db, groups, &format!("w{w}"), 1000).unwrap() {
                                Some(k) => {
                                    got.push(k.day);
                                    assert!(finish(&db, &k, 1000).unwrap());
                                    idle = 0;
                                }
                                None => idle += 1,
                            }
                        }
                        got
                    })
                })
                .collect();
            workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
        });
        let mut sorted = days.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (1..=40).collect::<Vec<_>>(), "every day once: {days:?}");
    }
}
