//! Day chunks of a group's backfill that any server carrying the group can
//! take (see docs/superpowers/specs/2026-10-05-faster-backfill-smaller-storage-design.md).
//! Lives in the main database.

use rusqlite::{Connection, OptionalExtension, Result, params};

/// a claim older than this is taken over (its worker died or was stopped)
pub const CLAIM_TIMEOUT: i64 = 30 * 60;

/// A chunk whose run failed rests this long before any server tries it again, doubling with each failure in a row up
/// to `FAIL_REST_MOST`. While it rests it neither is claimed nor holds the others back (see `frontier`): on prod eight
/// chunks of November that timed out or came back with broken compressed headers every time held all 4.8 million
/// older chunks back, for a day.
pub const FAIL_REST: i64 = 5 * 60;
pub const FAIL_REST_MOST: i64 = 6 * 3600;

/// SQL: chunk `c` isnt resting after a failure at unix time `now`
fn not_resting(now: i64) -> String {
    format!(
        "(c.failed_at is null or c.failed_at + min({FAIL_REST} << min(c.failures - 1, 7), {FAIL_REST_MOST}) <= {now})"
    )
}

const PENDING: i64 = 0;
const CLAIMED: i64 = 1;
const DONE: i64 = 2;

/// Initialize the backfill_chunks table if it doesn't exist, and give one
/// made before chunks kept their finish time its done_at column. Also the
/// server that goes back furthest, by split group (see `deepest`), and the
/// servers whose sweep of a split group is done (see `swept`).
pub fn create(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "create table if not exists backfill_chunks (
            grp TEXT NOT NULL,
            day INTEGER NOT NULL,
            state INTEGER NOT NULL,
            server TEXT,
            claimed_at INTEGER,
            done_at INTEGER,
            primary key (grp, day)
        ) without rowid;
        create table if not exists backfill_deepest (
            grp TEXT PRIMARY KEY,
            server TEXT NOT NULL,
            oldest_at INTEGER,
            runner_up_at INTEGER
        ) without rowid;
        create table if not exists backfill_sweeps (
            grp TEXT NOT NULL,
            server TEXT NOT NULL,
            primary key (grp, server)
        ) without rowid;
        create table if not exists backfill_sweep_gen (
            grp TEXT PRIMARY KEY,
            gen INTEGER NOT NULL
        ) without rowid;
        create table if not exists backfill_speculative (
            grp TEXT PRIMARY KEY,
            runner_up_day INTEGER NOT NULL,
            reach INTEGER NOT NULL,
            days INTEGER NOT NULL,
            articles INTEGER NOT NULL
        ) without rowid;
        create table if not exists backfill_media (
            grp TEXT PRIMARY KEY,
            releases INTEGER NOT NULL
        ) without rowid;
        create table if not exists backfill_wanted (
            grp TEXT NOT NULL,
            day INTEGER NOT NULL,
            at INTEGER NOT NULL,
            primary key (grp, day)
        ) without rowid;
        create index if not exists idx_backfill_chunks_grp_state_day
            on backfill_chunks (grp, state, day desc, claimed_at);
        -- whether any group still has a day to read (`xref::unread_days`)
        create index if not exists idx_backfill_chunks_day_state on backfill_chunks (day, state);",
    )?;
    add_column(conn, "backfill_chunks", "done_at")?;
    add_column(conn, "backfill_chunks", "failures")?;
    add_column(conn, "backfill_chunks", "failed_at")?;
    add_column(conn, "backfill_deepest", "oldest_at")?;
    add_column(conn, "backfill_deepest", "runner_up_at")
}

/// Give `table` the INTEGER column `column` if a table made before it lacks it.
fn add_column(conn: &Connection, table: &str, column: &str) -> Result<()> {
    add_column_as(conn, table, column, "INTEGER")
}

/// Adds `column` of `kind` to `table` when it's missing.
pub(crate) fn add_column_as(conn: &Connection, table: &str, column: &str, kind: &str) -> Result<()> {
    let has =
        || conn.prepare(&format!("select 1 from pragma_table_info('{table}') where name = '{column}'"))?.exists([]);
    if has()? {
        return Ok(());
    }
    match conn.execute_batch(&format!("alter table {table} add column {column} {kind}")) {
        // the indexer and the menu can both add it at once
        Err(_) if has()? => Ok(()),
        r => r,
    }
}

/// days since 1970-01-01 UTC
pub fn unix_day(t: i64) -> i64 {
    t.div_euclid(86_400)
}

/// Pending chunks for `newest_day` down to `oldest_day`, keeping any that exist.
pub fn add(conn: &Connection, group: &str, newest_day: i64, oldest_day: i64) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let mut added = 0;
    {
        let mut insert =
            tx.prepare_cached("insert or ignore into backfill_chunks (grp, day, state) values (?, ?, ?)")?;
        for day in (oldest_day..=newest_day).rev() {
            added += insert.execute(params![group, day, PENDING])?;
        }
    }
    tx.commit()?;
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
pub fn reach_back(conn: &Connection, group: &str, oldest_day: i64, oldest_at: i64) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let Some(old) = self::oldest_day(&tx, group)? else { return Ok(0) };
    // never past the window of days only the deepest server has
    let spec = speculative(&tx, group)?;
    let floor = spec.as_ref().map_or(i64::MIN, |s| s.runner_up_day - s.reach);
    if oldest_day < floor {
        if floor >= old {
            return Ok(0);
        }
        return add_speculative(&tx, group, old, floor).and_then(|n| tx.commit().map(|_| n));
    }
    if oldest_day > old {
        return Ok(0);
    }
    if spec.as_ref().is_some_and(|s| old <= s.runner_up_day) && oldest_day < old {
        // only days the deepest server alone has: added, nothing redone
        return add_speculative(&tx, group, old, oldest_day).and_then(|n| tx.commit().map(|_| n));
    }
    if oldest_day == old {
        let since: Option<i64> = tx
            .query_row("select oldest_at from backfill_deepest where grp = ?", [group], |r| r.get(0))
            .optional()?
            .flatten();
        if since.is_none_or(|since| oldest_at >= since) {
            return Ok(0);
        }
    }
    let mut added = 0;
    {
        let mut insert =
            tx.prepare_cached("insert or ignore into backfill_chunks (grp, day, state) values (?, ?, ?)")?;
        for day in (oldest_day..old).rev() {
            added += insert.execute(params![group, day, PENDING])?;
        }
    }
    let redo = tx.execute(
        &format!(
            "update backfill_chunks set state = {PENDING}, server = null, claimed_at = null, done_at = null
             where grp = ? and day = ? and state in ({DONE}, {CLAIMED})"
        ),
        params![group, old],
    )?;
    tx.execute("delete from backfill_deepest where grp = ?", [group])?;
    // a carrier's sweep stopped at its first article then: it may keep older ones now
    tx.execute("delete from backfill_sweeps where grp = ?", [group])?;
    // and a sweep that started before this doesnt get to note itself done (see `set_swept`)
    tx.execute(
        "insert into backfill_sweep_gen (grp, gen) values (?, 1) on conflict (grp) do update set gen = gen + 1",
        [group],
    )?;
    tx.commit()?;
    Ok(added + redo)
}

/// Chunks for the days before `old` back to `from`, all of them days only
/// the deepest server has: they dont change what any sweep covers.
fn add_speculative(tx: &Connection, group: &str, old: i64, from: i64) -> Result<usize> {
    let mut insert = tx.prepare_cached("insert or ignore into backfill_chunks (grp, day, state) values (?, ?, ?)")?;
    let mut added = 0;
    for day in (from..old).rev() {
        added += insert.execute(params![group, day, PENDING])?;
    }
    Ok(added)
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

pub fn speculative(conn: &Connection, group: &str) -> Result<Option<Speculative>> {
    conn.query_row(
        "select runner_up_day, reach, days, articles from backfill_speculative where grp = ?",
        [group],
        |r| Ok(Speculative { runner_up_day: r.get(0)?, reach: r.get(1)?, days: r.get(2)?, articles: r.get(3)? }),
    )
    .optional()
}

pub fn set_speculative(conn: &Connection, group: &str, s: &Speculative) -> Result<()> {
    conn.execute(
        "insert or replace into backfill_speculative (grp, runner_up_day, reach, days, articles) values (?, ?, ?, ?, ?)",
        params![group, s.runner_up_day, s.reach, s.days, s.articles],
    )?;
    Ok(())
}

/// Drop split `group`'s chunks older than `day` that arent done: days no
/// carrier keeps (a split reaching back that far was made from forged
/// dates), which would wait for a server forever and keep the split from
/// completing. Returns the chunks dropped.
pub fn drop_before(conn: &Connection, group: &str, day: i64) -> Result<usize> {
    conn.execute(
        &format!("delete from backfill_chunks where grp = ? and day < ? and state != {DONE}"),
        params![group, day],
    )
}

/// Check if a group has any chunks in backfill.
pub fn is_split(conn: &Connection, group: &str) -> Result<bool> {
    conn.prepare_cached("select 1 from backfill_chunks where grp = ? limit 1")?.exists([group])
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
}

/// Ask for a day of a group's backfill before the others: one a post found
/// in part was posted on. A day that's done or has no chunk is left as it is.
pub fn want(conn: &Connection, group: &str, day: i64, now: i64) -> Result<bool> {
    let n = conn.execute(
        &format!(
            "insert or ignore into backfill_wanted (grp, day, at)
             select grp, day, ?3 from backfill_chunks where grp = ?1 and day = ?2 and state != {DONE}"
        ),
        params![group, day, now],
    )?;
    Ok(n > 0)
}

/// Days of backfill a group may run ahead of the slowest: every group of a server reads one day before any goes more
/// than this many days past it, soo all groups stay at about the same posting time (`frontier`). The articles of an
/// upload spread over several groups are then read together, and linked across them (`xref`).
pub const LOCKSTEP_DAYS: i64 = 1;

/// Atomically claim the newest chunk that is pending or whose claim went
/// stale, of `groups`: (group, oldest day the server keeps), `i64::MIN` when
/// any day will do. A day asked for (`want`) goes first. The rest go in
/// lockstep, newest day first: no group goes back more than `LOCKSTEP_DAYS`
/// past the newest day any of them still has to do or is doing (`frontier`),
/// soo a small group waits for a big one instead of racing back alone, and
/// spare workers wait too. Within that the newest day goes first, a group big
/// releases come from (`media_groups`) first among equals.
pub fn claim(conn: &Connection, groups: &[(String, i64)], host: &str, now: i64) -> Result<Option<Claim>> {
    if groups.is_empty() {
        return Ok(None);
    }
    if let Some(claim) = claim_wanted(conn, groups, host, now)? {
        return Ok(Some(claim));
    }
    let Some(frontier) = frontier(conn, groups, now)? else { return Ok(None) };
    let floor = frontier - LOCKSTEP_DAYS;
    let groups: Vec<(String, i64)> = groups.iter().map(|(g, oldest)| (g.clone(), (*oldest).max(floor))).collect();
    let media = media_groups(conn)?;
    claim_newest(conn, &groups, &media, host, now)
}

/// The claim of a pass over split `group` on its own backfill turn: (group, oldest day the server keeps). A day asked
/// for (`want`) of it first, else its newest day no more than `LOCKSTEP_DAYS` past the newest day any of `all`
/// (likewise, `group` among them) still has to do or is doing, else none: a group whose days left are older waits
/// for the others to come back to them (see `claim`).
pub fn claim_turn(
    conn: &Connection,
    group: (String, i64),
    all: &[(String, i64)],
    host: &str,
    now: i64,
) -> Result<Option<Claim>> {
    if let Some(claim) = claim_wanted(conn, std::slice::from_ref(&group), host, now)? {
        return Ok(Some(claim));
    }
    let Some(frontier) = frontier(conn, all, now)? else { return Ok(None) };
    let (g, oldest) = group;
    claim_newest(conn, &[(g, oldest.max(frontier - LOCKSTEP_DAYS))], &[], host, now)
}

/// The newest day any of `groups` still has to do or is doing: (group, oldest day the server keeps). A day pending
/// (not resting after a failure, see `FAIL_REST`) or claimed, however fresh the claim: a day being read holds the
/// groups behind it (`claim`). None when none is left.
pub fn frontier(conn: &Connection, groups: &[(String, i64)], now: i64) -> Result<Option<i64>> {
    if groups.is_empty() {
        return Ok(None);
    }
    let rows = vec!["(?, ?)"; groups.len()].join(", ");
    let sql = format!(
        "with wanted (g, oldest) as (values {rows}),
              newest (day) as (
                  select (select c.day from backfill_chunks c
                          where c.grp = w.g and c.state = {PENDING} and c.day >= w.oldest and {resting}
                          order by c.day desc limit 1)
                  from wanted w
                  union all
                  select (select c.day from backfill_chunks c
                          where c.grp = w.g and c.state = {CLAIMED} and c.day >= w.oldest
                          order by c.day desc limit 1)
                  from wanted w
              )
         select max(day) from newest",
        resting = not_resting(now)
    );
    let mut values: Vec<rusqlite::types::Value> = Vec::with_capacity(groups.len() * 2);
    for (g, oldest) in groups {
        values.push(g.clone().into());
        values.push((*oldest).into());
    }
    conn.prepare(&sql)?.query_row(rusqlite::params_from_iter(values), |r| r.get(0))
}

/// Groups the big releases the indexer is after mostly come from, at least `MEDIA_MIN` each: their backfill goes
/// before the rest.
pub const MEDIA_GROUPS: usize = 16;
pub const MEDIA_MIN: i64 = 20;

/// Notes how many big releases each group holds (see `MEDIA_GROUPS`), in place of what was noted before.
pub fn set_media(conn: &Connection, counts: &[(String, i64)]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("delete from backfill_media", [])?;
    {
        let mut add = tx.prepare_cached("insert or replace into backfill_media (grp, releases) values (?1, ?2)")?;
        for (group, n) in counts {
            add.execute(params![group, n])?;
        }
    }
    tx.commit()
}

/// The groups of `MEDIA_GROUPS`, most big releases first.
pub fn media_groups(conn: &Connection) -> Result<Vec<String>> {
    conn.prepare_cached("select grp from backfill_media where releases >= ?1 order by releases desc limit ?2")?
        .query_map(params![MEDIA_MIN, MEDIA_GROUPS as i64], |r| r.get(0))?
        .collect()
}

/// The newest day asked for of `groups`, pending or with a stale claim: read from the few asked for, not the backlog.
fn claim_wanted(conn: &Connection, groups: &[(String, i64)], host: &str, now: i64) -> Result<Option<Claim>> {
    if !conn.prepare_cached("select 1 from backfill_wanted limit 1")?.exists([])? {
        return Ok(None);
    }
    let rows = vec!["(?, ?)"; groups.len()].join(", ");
    let sql = format!(
        "with wanted (g, oldest) as (values {rows})
         update backfill_chunks set state = ?, server = ?, claimed_at = ?
         where (grp, day) = (
             select c.grp, c.day from backfill_wanted r
             join wanted w on w.g = r.grp
             join backfill_chunks c on c.grp = r.grp and c.day = r.day
             where c.day >= w.oldest and (c.state = {PENDING} or (c.state = {CLAIMED} and c.claimed_at < ?))
             order by c.day desc limit 1)
         returning grp, day"
    );
    let mut values: Vec<rusqlite::types::Value> = Vec::with_capacity(groups.len() * 2 + 4);
    for (g, oldest) in groups {
        values.push(g.clone().into());
        values.push((*oldest).into());
    }
    values.extend([CLAIMED.into(), host.to_string().into(), now.into(), (now - CLAIM_TIMEOUT).into()]);
    let found: Option<(String, i64)> =
        conn.prepare(&sql)?.query_row(rusqlite::params_from_iter(values), |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    Ok(found.map(|(group, day)| Claim { group, day, server: host.to_string(), claimed_at: now }))
}

/// The chunk `claim` takes after the days asked for: of each group its newest day pending or with a stale claim, then
/// of those the newest, a group of `media` first among equals (see `claim`).
fn claim_newest(
    conn: &Connection,
    groups: &[(String, i64)],
    media: &[String],
    host: &str,
    now: i64,
) -> Result<Option<Claim>> {
    let rows = vec!["(?, ?, ?)"; groups.len()].join(", ");
    let sql = format!(
        "with wanted (g, oldest, media) as (values {rows}),
              candidates (grp, media, p_day, s_day) as materialized (
                  select w.g, w.media,
                         (select c.day from backfill_chunks c
                          where c.grp = w.g and c.state = {PENDING} and c.day >= w.oldest and {resting}
                          order by c.day desc limit 1),
                         (select c.day from backfill_chunks c
                          where c.grp = w.g and c.state = {CLAIMED} and c.day >= w.oldest and c.claimed_at < ?
                          order by c.day desc limit 1)
                  from wanted w
              ),
              best (grp, media, day) as (
                  select grp, media, case
                      when p_day is null then s_day
                      when s_day is null then p_day
                      when p_day >= s_day then p_day
                      else s_day
                  end
                  from candidates
              )
         update backfill_chunks set state = ?, server = ?, claimed_at = ?
         where (grp, day) = (
             select grp, day from best where day is not null
             order by day desc, media desc limit 1)
         returning grp, day",
        resting = not_resting(now)
    );
    let mut values: Vec<rusqlite::types::Value> = Vec::with_capacity(groups.len() * 3 + 4);
    for (g, oldest) in groups {
        values.push(g.clone().into());
        values.push((*oldest).into());
        values.push(i64::from(media.contains(g)).into());
    }
    values.push((now - CLAIM_TIMEOUT).into());
    values.extend([CLAIMED.into(), host.to_string().into(), now.into()]);
    let found: Option<(String, i64)> =
        conn.prepare(&sql)?.query_row(rusqlite::params_from_iter(values), |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    Ok(found.map(|(group, day)| Claim { group, day, server: host.to_string(), claimed_at: now }))
}

/// Mark a claimed chunk as done at `now`. False when the claim was taken over.
pub fn finish(conn: &Connection, claim: &Claim, now: i64) -> Result<bool> {
    let n = conn.execute(
        &format!(
            "update backfill_chunks set state = ?, done_at = ?
             where grp = ? and day = ? and state = {CLAIMED} and server = ? and claimed_at = ?"
        ),
        params![DONE, now, claim.group, claim.day, claim.server, claim.claimed_at],
    )?;
    if n > 0 {
        conn.execute("delete from backfill_wanted where grp = ?1 and day = ?2", params![claim.group, claim.day])?;
    }
    Ok(n > 0)
}

/// Return a claimed chunk to pending state. False when the claim was taken over.
/// Give `claim` back after its run failed: it rests before it is tried again (see `FAIL_REST`). False when the
/// claim no longer owns the chunk.
pub fn fail(conn: &Connection, claim: &Claim, now: i64) -> Result<bool> {
    let n = conn.execute(
        &format!(
            "update backfill_chunks set state = ?, server = null, claimed_at = null,
                    failures = coalesce(failures, 0) + 1, failed_at = ?
             where grp = ? and day = ? and state = {CLAIMED} and server = ? and claimed_at = ?"
        ),
        params![PENDING, now, claim.group, claim.day, claim.server, claim.claimed_at],
    )?;
    Ok(n > 0)
}

pub fn release(conn: &Connection, claim: &Claim) -> Result<bool> {
    let n = conn.execute(
        &format!(
            "update backfill_chunks set state = ?, server = null, claimed_at = null
             where grp = ? and day = ? and state = {CLAIMED} and server = ? and claimed_at = ?"
        ),
        params![PENDING, claim.group, claim.day, claim.server, claim.claimed_at],
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
pub fn per_group(conn: &Connection) -> Result<std::collections::BTreeMap<String, GroupChunks>> {
    let has: bool = conn.query_row(
        "select exists(select 1 from sqlite_master where type = 'table' and name = 'backfill_chunks')",
        [],
        |r| r.get(0),
    )?;
    if !has {
        return Ok(Default::default());
    }
    let mut stmt = conn.prepare(
        "select grp, coalesce(sum(state = 2), 0), count(*), min(day), max(day),
                min(case when state = 2 then day end)
         from backfill_chunks group by grp",
    )?;
    stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            GroupChunks {
                done: r.get(1)?,
                total: r.get(2)?,
                oldest_day: r.get(3)?,
                newest_day: r.get(4)?,
                oldest_done_day: r.get(5)?,
            },
        ))
    })?
    .collect()
}

/// (done, total) chunks of a group
pub fn progress(conn: &Connection, group: &str) -> Result<(i64, i64)> {
    conn.query_row("select coalesce(sum(state = 2), 0), count(*) from backfill_chunks where grp = ?", [group], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })
}

/// the oldest day of a group's chunks
pub fn oldest_day(conn: &Connection, group: &str) -> Result<Option<i64>> {
    conn.query_row("select min(day) from backfill_chunks where grp = ?", [group], |r| r.get(0))
}

/// the newest day of a group's chunks
pub fn newest_day(conn: &Connection, group: &str) -> Result<Option<i64>> {
    conn.query_row("select max(day) from backfill_chunks where grp = ?", [group], |r| r.get(0))
}

/// The server whose first article of a split group is the oldest: the one
/// to index the split's oldest day, which no server keeps whole.
pub fn deepest(conn: &Connection, group: &str) -> Result<Option<String>> {
    conn.query_row("select server from backfill_deepest where grp = ?", [group], |r| r.get(0)).optional()
}

/// Note `server` as the one going back furthest on `group`, from `oldest_at`
/// (when its first article was posted, unix seconds).
pub fn set_deepest(conn: &Connection, group: &str, server: &str, oldest_at: i64) -> Result<()> {
    conn.execute(
        "insert or replace into backfill_deepest (grp, server, oldest_at) values (?, ?, ?)",
        params![group, server, oldest_at],
    )?;
    Ok(())
}

/// Note how far back the server going back second furthest on `group`
/// goes (unix seconds): days before it are the furthest one's alone.
pub fn set_runner_up(conn: &Connection, group: &str, runner_up_at: i64) -> Result<()> {
    conn.execute("update backfill_deepest set runner_up_at = ? where grp = ?", params![runner_up_at, group])?;
    Ok(())
}

/// How far back the server going back second furthest on `group` goes
/// (unix seconds), when noted with the furthest one.
pub fn runner_up(conn: &Connection, group: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row("select runner_up_at from backfill_deepest where grp = ?", [group], |r| r.get(0))
        .optional()?
        .flatten())
}

/// Forget `server` as the one going back furthest on `group`, when it no
/// longer carries it or no longer keeps the oldest day: the next to reach
/// the oldest day asks again.
pub fn forget_deepest(conn: &Connection, group: &str, server: &str) -> Result<()> {
    conn.execute("delete from backfill_deepest where grp = ? and server = ?", [group, server])?;
    Ok(())
}

/// Whether any chunk of `groups`: (group, oldest day the server keeps) is
/// still to do (pending or claimed) on a day the server keeps.
pub fn waiting(conn: &Connection, groups: &[(String, i64)]) -> Result<bool> {
    let mut stmt = conn.prepare_cached("select 1 from backfill_chunks where grp = ? and day >= ? and state != 2")?;
    for (g, oldest) in groups {
        if stmt.exists(params![g, oldest])? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether every day chunk of split `group` is done.
pub fn chunks_done(conn: &Connection, group: &str) -> Result<bool> {
    Ok(is_split(conn, group)?
        && !conn.prepare_cached("select 1 from backfill_chunks where grp = ? and state != 2")?.exists([group])?)
}

/// The split generation of `group`: counts the times its split reached back
/// (see `reach_back`), which undoes what a sweep covered.
pub fn sweep_generation(conn: &Connection, group: &str) -> Result<i64> {
    Ok(conn
        .query_row("select gen from backfill_sweep_gen where grp = ?", [group], |r| r.get(0))
        .optional()?
        .unwrap_or(0))
}

/// Note `server`'s sweep of split `group` as done: its cursor backfill got
/// down to its first article after the day chunks (see `indexer::run_sweep`).
/// `generation` is the group's (see `sweep_generation`) when the sweep was
/// chosen: one from before the split reached back is stale and notes nothing.
pub fn set_swept(conn: &Connection, group: &str, server: &str, generation: i64) -> Result<()> {
    conn.execute(
        "insert or ignore into backfill_sweeps (grp, server)
         select ?1, ?2 where coalesce((select gen from backfill_sweep_gen where grp = ?1), 0) = ?3",
        params![group, server, generation],
    )?;
    Ok(())
}

/// Whether `server`'s sweep of split `group` is done.
pub fn swept(conn: &Connection, group: &str, server: &str) -> Result<bool> {
    conn.prepare_cached("select 1 from backfill_sweeps where grp = ? and server = ?")?.exists([group, server])
}

/// Whether split `group` is complete: every day chunk done and every one of
/// `servers` (the ones carrying it) swept down to its first article.
pub fn complete(conn: &Connection, group: &str, servers: &[String]) -> Result<bool> {
    if !chunks_done(conn, group)? {
        return Ok(false);
    }
    for s in servers {
        if !swept(conn, group, s)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// groups with chunks still to do
pub fn split_groups(conn: &Connection) -> Result<Vec<String>> {
    conn.prepare("select distinct grp from backfill_chunks where state != 2 order by grp")?
        .query_map([], |r| r.get(0))?
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_chunk_rests_and_doesnt_hold_the_others() {
        let c = Connection::open_in_memory().unwrap();
        create(&c).unwrap();
        // `new` has the newest day (2024-11-03), `old` only October
        add(&c, "new", 20_030, 20_030).unwrap();
        add(&c, "old", 20_020, 20_019).unwrap();
        let all: Vec<(String, i64)> = ["new", "old"].iter().map(|g| (g.to_string(), i64::MIN)).collect();
        let turn = |g: &str, host: &str, now| claim_turn(&c, (g.to_string(), i64::MIN), &all, host, now).unwrap();
        assert!(turn("old", "a", 1000).is_none(), "new's day first: old waits");
        let new = turn("new", "a", 1000).unwrap();
        assert!(fail(&c, &new, 1000).unwrap());
        assert!(!fail(&c, &new, 1000).unwrap(), "a claim gives it back once");
        // resting, it neither is taken nor holds the others
        assert!(turn("new", "b", 1001).is_none());
        let old = turn("old", "b", 1001).expect("old goes on while new's day rests");
        assert_eq!(old.day, 20_020);
        assert_eq!(claim(&c, &all, "c", 1001).unwrap().map(|k| k.day), Some(20_019), "nor does a free worker take it");
        // after its rest it is the newest day again, and a second failure rests twice as long
        let again = turn("new", "b", 1000 + FAIL_REST).unwrap();
        assert!(fail(&c, &again, 1000 + FAIL_REST).unwrap());
        assert!(turn("new", "b", 1000 + 2 * FAIL_REST).is_none());
        assert!(turn("new", "b", 1000 + 3 * FAIL_REST).is_some());
        // released (stopped, or a day this server lacks), it is taken again at once
        let c2 = Connection::open_in_memory().unwrap();
        create(&c2).unwrap();
        add(&c2, "g", 20_030, 20_030).unwrap();
        let g = vec![("g".to_string(), i64::MIN)];
        let k = claim(&c2, &g, "a", 0).unwrap().unwrap();
        assert!(release(&c2, &k).unwrap());
        assert!(claim(&c2, &g, "b", 1).unwrap().is_some());
    }

    #[test]
    fn per_group_counts_and_spans_each_groups_chunks() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(per_group(&conn).unwrap().is_empty(), "no table: nothing");
        create(&conn).unwrap();
        add(&conn, "g", 100, 97).unwrap();
        let claim = claim(&conn, &[("g".to_string(), i64::MIN)], "s", 1000).unwrap().unwrap();
        assert!(finish(&conn, &claim, 1100).unwrap());
        let g = per_group(&conn).unwrap()["g"];
        assert_eq!((g.done, g.total, g.oldest_day, g.newest_day), (1, 4, 97, 100));
        assert_eq!(g.oldest_done_day, Some(claim.day));
    }

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        create(&c).unwrap();
        c
    }

    /// the second furthest server's retention is kept with the furthest,
    /// and goes with it
    #[test]
    fn the_runner_up_is_kept_with_the_deepest() {
        let c = conn();
        set_deepest(&c, "g", "a", 100).unwrap();
        assert_eq!(runner_up(&c, "g").unwrap(), None);
        set_runner_up(&c, "g", 200).unwrap();
        assert_eq!(runner_up(&c, "g").unwrap(), Some(200));
        forget_deepest(&c, "g", "a").unwrap();
        assert_eq!(runner_up(&c, "g").unwrap(), None);
    }

    #[test]
    fn reaching_back_keeps_to_the_speculative_window() {
        let c = conn();
        // days 100.. the next deepest keeps, 40..99 the window, older pruned
        add(&c, "g", 120, 40).unwrap();
        set_speculative(&c, "g", &Speculative { runner_up_day: 100, reach: 60, days: 3, articles: 7 }).unwrap();
        set_swept(&c, "g", "a", 0).unwrap();
        // the hourly look finds the deep server back in 2000 again: nothing
        assert_eq!(reach_back(&c, "g", 10, 10 * 86_400).unwrap(), 0);
        assert_eq!(oldest_day(&c, "g").unwrap(), Some(40));
        assert!(swept(&c, "g", "a").unwrap(), "the sweep isnt to do again");
        // the window kept across a restart
        assert_eq!(speculative(&c, "g").unwrap().map(|s| (s.reach, s.days, s.articles)), Some((60, 3, 7)));
        // a wider window: the days in it, sweeps left alone
        set_speculative(&c, "g", &Speculative { runner_up_day: 100, reach: 70, days: 3, articles: 7 }).unwrap();
        assert_eq!(reach_back(&c, "g", 5, 5 * 86_400).unwrap(), 10);
        assert_eq!(oldest_day(&c, "g").unwrap(), Some(30));
        assert!(swept(&c, "g", "a").unwrap());
    }

    /// the day of a claim
    fn day(claimed: Result<Option<Claim>>) -> Option<i64> {
        claimed.unwrap().map(|c| c.day)
    }

    #[test]
    fn days_are_claimed_newest_first_and_once() {
        let c = conn();
        assert_eq!(add(&c, "g", 20_000, 19_998).unwrap(), 3);
        assert_eq!(add(&c, "g", 20_000, 19_998).unwrap(), 0, "adding again adds nothing");
        assert!(is_split(&c, "g").unwrap());
        let groups = vec![("g".to_string(), i64::MIN)];

        let a = claim(&c, &groups, "a", 1000).unwrap().unwrap();
        assert_eq!(a, Claim { group: "g".into(), day: 20_000, server: "a".into(), claimed_at: 1000 });
        let b = claim(&c, &groups, "b", 1000).unwrap().unwrap();
        assert_eq!(b.day, 19_999);
        assert!(finish(&c, &a, 1100).unwrap());
        assert!(release(&c, &b).unwrap());
        assert_eq!(day(claim(&c, &groups, "b", 1000)), Some(19_999), "released goes back");
        assert_eq!(day(claim(&c, &groups, "a", 1000)), Some(19_998));
        assert_eq!(claim(&c, &groups, "a", 1000).unwrap(), None);
        assert_eq!(progress(&c, "g").unwrap(), (1, 3));
    }

    #[test]
    fn the_groups_big_releases_come_from_are_backfilled_first() {
        let c = conn();
        // days 20_000..=20_010 are 2024-10-04..=14, one month
        add(&c, "plain", 20_010, 20_000).unwrap();
        add(&c, "media", 20_010, 20_000).unwrap();
        add(&c, "few", 20_010, 20_000).unwrap();
        let groups: Vec<(String, i64)> = ["plain", "media", "few"].iter().map(|g| (g.to_string(), 20_005)).collect();
        // ties go by group name, `media` before `plain`: start where that wouldn't show
        let first = claim(&c, &groups, "a", 0).unwrap().unwrap();
        assert_eq!(first.day, 20_010);
        assert!(finish(&c, &first, 0).unwrap());
        set_media(&c, &[("media".into(), 500), ("few".into(), MEDIA_MIN - 1)]).unwrap();
        let mut got = Vec::new();
        while let Some(claim) = claim(&c, &groups, "a", 0).unwrap() {
            assert!(finish(&c, &claim, 0).unwrap());
            got.push((claim.group, claim.day));
        }
        // day by day, newest first, and of each day `media` first
        assert!(got.windows(2).all(|w| w[0].1 >= w[1].1), "{got:?}");
        for d in 20_005..20_010 {
            assert_eq!(got.iter().find(|(_, day)| *day == d).map(|(g, _)| g.as_str()), Some("media"), "{got:?}");
        }
        // noted again from scratch
        set_media(&c, &[("plain".into(), 30)]).unwrap();
        assert_eq!(media_groups(&c).unwrap(), ["plain"]);
    }

    /// Every group's backfill goes back day by day together: no group claims a day more than `LOCKSTEP_DAYS` past
    /// the newest day any of them still has to do or is doing, however far back its own days go.
    #[test]
    fn groups_are_backfilled_in_lockstep() {
        let c = conn();
        // `deep` (a media group) 2024-08-25..=10-24, `recent` 2024-10-04..=11-03
        add(&c, "deep", 20_020, 19_960).unwrap();
        add(&c, "recent", 20_030, 20_000).unwrap();
        set_media(&c, &[("deep".into(), 500)]).unwrap();
        let groups: Vec<(String, i64)> = ["deep", "recent"].iter().map(|g| (g.to_string(), i64::MIN)).collect();
        let mut got = Vec::new();
        while let Some(claim) = claim(&c, &groups, "a", 0).unwrap() {
            assert!(finish(&c, &claim, 0).unwrap());
            got.push((claim.group, claim.day));
        }
        assert_eq!(got.len(), 61 + 31);
        assert!(got.windows(2).all(|w| w[0].1 >= w[1].1), "newest day first over both: {got:?}");
        assert_eq!(got[10], ("deep".to_string(), 20_020), "a media group first among equals");

        // a day being read holds the rest: the next day goes, not one further back
        let c = conn();
        add(&c, "big", 20_030, 20_020).unwrap();
        add(&c, "small", 20_030, 20_020).unwrap();
        let groups: Vec<(String, i64)> = ["big", "small"].iter().map(|g| (g.to_string(), i64::MIN)).collect();
        let big = claim(&c, &groups, "a", 1000).unwrap().unwrap();
        assert_eq!((big.group.as_str(), big.day), ("big", 20_030));
        let mut small = Vec::new();
        while let Some(k) = claim(&c, &groups, "b", 1000).unwrap() {
            assert!(finish(&c, &k, 1000).unwrap());
            small.push((k.group, k.day));
        }
        assert!(small.iter().all(|(_, d)| *d >= 20_030 - LOCKSTEP_DAYS), "{small:?}");
        assert_eq!(small.len(), 3, "small's 20030 and 20029, big's 20029: {small:?}");
        // once big's day is read, the rest move on a day
        assert!(finish(&c, &big, 1000).unwrap());
        assert_eq!(day(claim(&c, &groups, "b", 1000)), Some(20_028));
        // a stale claim is taken over first, and holds the rest meanwhile
        let c = conn();
        add(&c, "g", 20_030, 20_026).unwrap();
        let groups = vec![("g".to_string(), i64::MIN)];
        let first = claim(&c, &groups, "a", 1000).unwrap().unwrap();
        assert_eq!(day(claim(&c, &groups, "a", 1000)), Some(20_029));
        assert_eq!(claim(&c, &groups, "a", 1000).unwrap(), None, "20030 is still being read");
        let late = claim(&c, &groups, "b", 1000 + CLAIM_TIMEOUT + 1).unwrap().unwrap();
        assert_eq!(late.day, first.day, "the stale claim first");
    }

    /// A split group's own backfill turn takes a day no more than `LOCKSTEP_DAYS` past the newest any tracked split
    /// group has left, or a day asked for, and otherwise nothing: one whose days left are years old waits for the
    /// others to come back to them.
    #[test]
    fn a_groups_own_turn_waits_for_the_others() {
        let c = conn();
        // `old` 2022-04-04..=13, `recent` 2024-10-30..=11-03
        add(&c, "old", 19_095, 19_086).unwrap();
        add(&c, "recent", 20_030, 20_026).unwrap();
        let all: Vec<(String, i64)> = ["old", "recent"].iter().map(|g| (g.to_string(), i64::MIN)).collect();
        let turn = |g: &str, now: i64| claim_turn(&c, (g.to_string(), i64::MIN), &all, "a", now).unwrap();
        assert_eq!(frontier(&c, &all, 0).unwrap(), Some(20_030));
        assert_eq!(turn("old", 0), None, "nothing while recent's days are left");
        // a day asked for goes anyway
        assert!(want(&c, "old", 19_090, 0).unwrap());
        let asked = turn("old", 0).unwrap();
        assert_eq!(asked.day, 19_090);
        assert!(finish(&c, &asked, 0).unwrap());
        assert_eq!(turn("old", 0), None);
        // `recent` takes its days one by one as each is read
        let mut days = Vec::new();
        while let Some(k) = turn("recent", 0) {
            assert!(finish(&c, &k, 0).unwrap());
            days.push(k.day);
        }
        assert_eq!(days, [20_030, 20_029, 20_028, 20_027, 20_026]);
        // recent all read: `old`'s days are the newest left
        assert_eq!(turn("old", 0).map(|c| c.day), Some(19_095));
        assert_eq!(frontier(&c, &[], 0).unwrap(), None);
    }

    #[test]
    fn a_day_asked_for_is_claimed_first() {
        let c = conn();
        add(&c, "g", 20_000, 19_990).unwrap();
        add(&c, "h", 20_000, 19_990).unwrap();
        let groups = vec![("g".to_string(), i64::MIN), ("h".to_string(), i64::MIN)];
        assert!(want(&c, "h", 19_993, 500).unwrap());
        assert!(!want(&c, "h", 18_000, 500).unwrap(), "no chunk for that day");
        let a = claim(&c, &groups, "a", 1000).unwrap().unwrap();
        assert_eq!((a.group.as_str(), a.day), ("h", 19_993));
        // claimed by another: the rest go newest first again
        assert_eq!(day(claim(&c, &groups, "b", 1000)), Some(20_000));
        // a claim gone stale is taken over before the rest, and a day once done isn't asked for again
        let late = claim(&c, &groups, "c", 1000 + CLAIM_TIMEOUT + 1).unwrap().unwrap();
        assert_eq!((late.group.as_str(), late.day), ("h", 19_993));
        assert!(finish(&c, &late, 5000).unwrap());
        assert!(!want(&c, "h", 19_993, 6000).unwrap());
        let left: i64 = c.query_row("select count(*) from backfill_wanted", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn the_oldest_day_is_redone_when_retention_reaches_further_into_it() {
        let c = conn();
        let done = |c: &Connection| {
            c.execute("update backfill_chunks set state = 2, server = 'a', done_at = 1 where day = 10", []).unwrap()
        };
        let state = |c: &Connection| -> i64 {
            c.query_row("select state from backfill_chunks where day = 10", [], |r| r.get(0)).unwrap()
        };
        add(&c, "g", 12, 10).unwrap();
        done(&c);
        // "a" went back furthest, to 18:00 on day 10
        set_deepest(&c, "g", "a", 10 * 86_400 + 18 * 3600).unwrap();

        assert_eq!(reach_back(&c, "g", 10, 10 * 86_400 + 18 * 3600).unwrap(), 0, "no further");
        assert_eq!(reach_back(&c, "g", 10, 10 * 86_400 + 20 * 3600).unwrap(), 0, "less far");
        assert_eq!((state(&c), deepest(&c, "g").unwrap().as_deref()), (2, Some("a")));

        assert_eq!(reach_back(&c, "g", 10, 10 * 86_400 + 3600).unwrap(), 1, "01:00 the same day");
        assert_eq!((state(&c), deepest(&c, "g").unwrap()), (0, None), "to do again, deepest asked again");

        // a deepest noted before its time was: nothing to compare, left alone
        done(&c);
        c.execute("insert into backfill_deepest (grp, server) values ('g', 'a')", []).unwrap();
        assert_eq!(reach_back(&c, "g", 10, 10 * 86_400).unwrap(), 0);
        assert_eq!(state(&c), 2);
        // a day before it still reaches back
        assert_eq!(reach_back(&c, "g", 9, 9 * 86_400).unwrap(), 2, "day 9 added, day 10 to do again");
    }

    #[test]
    fn a_claimed_oldest_day_is_reset_when_retention_reaches_further_into_it() {
        let c = conn();
        add(&c, "g", 12, 10).unwrap();
        let groups = vec![("g".to_string(), i64::MIN)];
        // a reads day 12, then day 11, then takes day 10 (the split's oldest), as the deepest
        for _ in 0..2 {
            let k = claim(&c, &groups, "a", 1000).unwrap().unwrap();
            assert!(finish(&c, &k, 1000).unwrap());
        }
        let a = claim(&c, &groups, "a", 1000).unwrap().unwrap();
        assert_eq!(a.day, 10);
        set_deepest(&c, "g", "a", 10 * 86_400 + 18 * 3600).unwrap();

        // b goes back further into day 10 while a is still on it
        assert_eq!(reach_back(&c, "g", 10, 10 * 86_400 + 3600).unwrap(), 1);
        assert_eq!(deepest(&c, "g").unwrap(), None);
        let row: (i64, Option<String>, Option<i64>) = c
            .query_row("select state, server, claimed_at from backfill_chunks where day = 10", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!(row, (PENDING, None, None), "pending again, a's claim gone");

        // a's late finish and release do nothing
        assert!(!finish(&c, &a, 2000).unwrap());
        assert!(!release(&c, &a).unwrap());
        assert_eq!(progress(&c, "g").unwrap(), (2, 3), "days 12 and 11 read");
        // and the next to claim it is not a stale claim's
        assert_eq!(day(claim(&c, &groups, "b", 1001)), Some(10));
    }

    /// a carrier swept down to its first article before the split reached
    /// back sweeps again: its first article may be older now
    #[test]
    fn reaching_back_undoes_the_sweeps() {
        let c = conn();
        add(&c, "g", 12, 10).unwrap();
        c.execute("update backfill_chunks set state = 2", []).unwrap();
        set_deepest(&c, "g", "a", 10 * 86_400 + 18 * 3600).unwrap();
        set_swept(&c, "g", "b", 0).unwrap();
        set_swept(&c, "other", "b", 0).unwrap();

        assert_eq!(reach_back(&c, "g", 10, 10 * 86_400 + 20 * 3600).unwrap(), 0, "not further");
        assert!(swept(&c, "g", "b").unwrap(), "kept when nothing changed");

        assert_eq!(reach_back(&c, "g", 9, 9 * 86_400).unwrap(), 2);
        assert!(!swept(&c, "g", "b").unwrap(), "to sweep again");
        assert!(swept(&c, "other", "b").unwrap(), "other groups' sweeps stay");
    }

    /// a sweep chosen before the split reached back, finishing after, doesnt
    /// note itself done: it covered what is older now only down to the old first article
    #[test]
    fn a_sweep_from_before_reaching_back_is_not_noted() {
        let c = conn();
        add(&c, "g", 12, 10).unwrap();
        c.execute("update backfill_chunks set state = 2", []).unwrap();
        set_deepest(&c, "g", "a", 10 * 86_400 + 18 * 3600).unwrap();
        let started = sweep_generation(&c, "g").unwrap();

        assert_eq!(reach_back(&c, "g", 9, 9 * 86_400).unwrap(), 2);
        set_swept(&c, "g", "b", started).unwrap();
        assert!(!swept(&c, "g", "b").unwrap(), "stale, left to do");

        let now = sweep_generation(&c, "g").unwrap();
        assert_ne!(now, started);
        set_swept(&c, "g", "b", now).unwrap();
        assert!(swept(&c, "g", "b").unwrap(), "one started since is noted");
    }

    #[test]
    fn days_before_are_dropped_unless_done() {
        let c = conn();
        add(&c, "g", 12, 5).unwrap();
        add(&c, "h", 12, 5).unwrap();
        c.execute("update backfill_chunks set state = 2 where grp = 'g' and day = 6", []).unwrap();
        claim(&c, &[("g".to_string(), i64::MIN)], "a", 1000).unwrap();
        assert_eq!(drop_before(&c, "g", 8).unwrap(), 2, "days 5 and 7");
        assert_eq!(oldest_day(&c, "g").unwrap(), Some(6), "a done day stays");
        assert_eq!(progress(&c, "h").unwrap(), (0, 8), "other groups untouched");
    }

    #[test]
    fn stale_claims_are_taken_over() {
        let c = conn();
        add(&c, "g", 5, 5).unwrap();
        let groups = vec![("g".to_string(), i64::MIN)];
        assert!(claim(&c, &groups, "a", 1000).unwrap().is_some());
        assert_eq!(claim(&c, &groups, "b", 1000 + CLAIM_TIMEOUT - 1).unwrap(), None);
        assert_eq!(day(claim(&c, &groups, "b", 1000 + CLAIM_TIMEOUT + 1)), Some(5));
    }

    #[test]
    fn a_claim_taken_over_cant_be_finished_or_given_back() {
        let c = conn();
        add(&c, "g", 5, 5).unwrap();
        let groups = vec![("g".to_string(), i64::MIN)];
        // a's chunk runs past CLAIM_TIMEOUT, b takes it over and finishes it
        let a = claim(&c, &groups, "a", 1000).unwrap().unwrap();
        let b = claim(&c, &groups, "b", 1000 + CLAIM_TIMEOUT + 1).unwrap().unwrap();
        let done_at = 1000 + CLAIM_TIMEOUT + 60;
        assert!(finish(&c, &b, done_at).unwrap());

        // a's late release and finish change nothing: done, and b's
        assert!(!release(&c, &a).unwrap());
        assert!(!finish(&c, &a, done_at + 60).unwrap());
        let row = |c: &Connection| {
            c.query_row("select state, server, done_at from backfill_chunks where grp = 'g' and day = 5", [], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<i64>>(2)?))
            })
            .unwrap()
        };
        assert_eq!(row(&c), (DONE, Some("b".to_string()), Some(done_at)));
    }

    #[test]
    fn a_table_from_before_done_at_gets_the_column() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "create table backfill_chunks (grp TEXT NOT NULL, day INTEGER NOT NULL, state INTEGER NOT NULL,
                 server TEXT, claimed_at INTEGER, primary key (grp, day)) without rowid;
             insert into backfill_chunks values ('g', 5, 1, 'a', 1000);",
        )
        .unwrap();
        create(&c).unwrap();
        create(&c).unwrap();
        let a = Claim { group: "g".into(), day: 5, server: "a".into(), claimed_at: 1000 };
        assert!(finish(&c, &a, 1100).unwrap(), "the chunk claimed before is still there");
        let done_at: Option<i64> = c.query_row("select done_at from backfill_chunks", [], |r| r.get(0)).unwrap();
        assert_eq!(done_at, Some(1100));
    }

    #[test]
    fn only_listed_groups_and_unsplit_groups() {
        let c = conn();
        add(&c, "g", 5, 5).unwrap();
        assert_eq!(claim(&c, &[("other".to_string(), i64::MIN)], "a", 0).unwrap(), None);
        assert!(!is_split(&c, "other").unwrap());
        assert_eq!(split_groups(&c).unwrap(), vec!["g".to_string()]);
        assert_eq!(unix_day(86_400 * 3 + 5), 3);
    }

    #[test]
    fn days_older_than_a_servers_oldest_are_left_to_others() {
        let c = conn();
        add(&c, "g", 12, 10).unwrap();
        let from_11 = [("g".to_string(), 11)];
        for d in [12, 11] {
            let k = claim(&c, &from_11, "short", 0).unwrap().unwrap();
            assert_eq!(k.day, d);
            assert!(finish(&c, &k, 0).unwrap());
        }
        assert_eq!(day(claim(&c, &from_11, "short", 0)), None, "day 10 is older than it keeps");
        assert_eq!(day(claim(&c, &[("g".to_string(), i64::MIN)], "long", 0)), Some(10));
    }

    #[test]
    fn claim_is_atomic_with_concurrent_connections() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        // create db and add chunks
        let c1 = Connection::open(&db_path).unwrap();
        create(&c1).unwrap();
        add(&c1, "g", 20, 19).unwrap();
        c1.close().ok();

        // open two connections and claim
        let c_a = Connection::open(&db_path).unwrap();
        let c_b = Connection::open(&db_path).unwrap();

        let groups = vec![("g".to_string(), i64::MIN)];
        let a_claim = claim(&c_a, &groups, "a", 1000).unwrap();
        let b_claim = claim(&c_b, &groups, "b", 1000).unwrap();

        // they should get different days (newest first, then next)
        assert!(a_claim.is_some() && b_claim.is_some());
        assert_ne!(a_claim.as_ref().map(|c| c.day), b_claim.as_ref().map(|c| c.day));
        assert_eq!(a_claim.as_ref().map(|c| c.day), Some(20));
        assert_eq!(b_claim.as_ref().map(|c| c.day), Some(19));

        // third claim should get None
        let c_c = Connection::open(&db_path).unwrap();
        assert_eq!(claim(&c_c, &groups, "c", 1000).unwrap(), None);
    }
}
