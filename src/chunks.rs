//! Day chunks of a group's backfill that any server carrying the group can
//! take (see docs/superpowers/specs/2026-10-05-faster-backfill-smaller-storage-design.md).
//! Lives in the main database.

use rusqlite::{Connection, OptionalExtension, Result, params};

/// a claim older than this is taken over (its worker died or was stopped)
pub const CLAIM_TIMEOUT: i64 = 30 * 60;

const PENDING: i64 = 0;
const CLAIMED: i64 = 1;
const DONE: i64 = 2;

pub fn create(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "create table if not exists backfill_chunks (
            grp TEXT NOT NULL,
            day INTEGER NOT NULL,
            state INTEGER NOT NULL,
            server TEXT,
            claimed_at INTEGER,
            primary key (grp, day)
        ) without rowid;",
    )
}

/// days since 1970-01-01 UTC
pub fn unix_day(t: i64) -> i64 {
    t.div_euclid(86_400)
}

/// Pending chunks for `newest_day` down to `oldest_day`, keeping any that exist.
pub fn add(conn: &Connection, group: &str, newest_day: i64, oldest_day: i64) -> Result<usize> {
    let mut insert = conn.prepare_cached("insert or ignore into backfill_chunks (grp, day, state) values (?, ?, ?)")?;
    let mut added = 0;
    for day in (oldest_day..=newest_day).rev() {
        added += insert.execute(params![group, day, PENDING])?;
    }
    Ok(added)
}

pub fn is_split(conn: &Connection, group: &str) -> Result<bool> {
    conn.prepare_cached("select 1 from backfill_chunks where grp = ? limit 1")?.exists([group])
}

/// The newest chunk of `groups` that is pending or whose claim went stale,
/// claimed for `host`. The indexer's main connection is the only writer, so select then update is safe.
pub fn claim(conn: &Connection, groups: &[String], host: &str, now: i64) -> Result<Option<(String, i64)>> {
    if groups.is_empty() {
        return Ok(None);
    }
    let marks = vec!["?"; groups.len()].join(",");
    let sql = format!(
        "select grp, day from backfill_chunks
         where grp in ({marks}) and (state = {PENDING} or (state = {CLAIMED} and claimed_at < ?))
         order by day desc limit 1"
    );
    let mut values: Vec<rusqlite::types::Value> = groups.iter().map(|g| g.clone().into()).collect();
    values.push((now - CLAIM_TIMEOUT).into());
    let found: Option<(String, i64)> =
        conn.prepare(&sql)?.query_row(rusqlite::params_from_iter(values), |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    if let Some((group, day)) = &found {
        conn.execute(
            "update backfill_chunks set state = ?, server = ?, claimed_at = ? where grp = ? and day = ?",
            params![CLAIMED, host, now, group, day],
        )?;
    }
    Ok(found)
}

pub fn finish(conn: &Connection, group: &str, day: i64) -> Result<()> {
    conn.execute("update backfill_chunks set state = ? where grp = ? and day = ?", params![DONE, group, day])?;
    Ok(())
}

pub fn release(conn: &Connection, group: &str, day: i64) -> Result<()> {
    conn.execute(
        "update backfill_chunks set state = ?, server = null, claimed_at = null where grp = ? and day = ?",
        params![PENDING, group, day],
    )?;
    Ok(())
}

/// (done, total) chunks of a group
pub fn progress(conn: &Connection, group: &str) -> Result<(i64, i64)> {
    conn.query_row("select coalesce(sum(state = 2), 0), count(*) from backfill_chunks where grp = ?", [group], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })
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

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        create(&c).unwrap();
        c
    }

    #[test]
    fn days_are_claimed_newest_first_and_once() {
        let c = conn();
        assert_eq!(add(&c, "g", 20_000, 19_998).unwrap(), 3);
        assert_eq!(add(&c, "g", 20_000, 19_998).unwrap(), 0, "adding again adds nothing");
        assert!(is_split(&c, "g").unwrap());
        let groups = vec!["g".to_string()];

        assert_eq!(claim(&c, &groups, "a", 1000).unwrap(), Some(("g".into(), 20_000)));
        assert_eq!(claim(&c, &groups, "b", 1000).unwrap(), Some(("g".into(), 19_999)));
        finish(&c, "g", 20_000).unwrap();
        release(&c, "g", 19_999).unwrap();
        assert_eq!(claim(&c, &groups, "b", 1000).unwrap(), Some(("g".into(), 19_999)), "released goes back");
        assert_eq!(claim(&c, &groups, "a", 1000).unwrap(), Some(("g".into(), 19_998)));
        assert_eq!(claim(&c, &groups, "a", 1000).unwrap(), None);
        assert_eq!(progress(&c, "g").unwrap(), (1, 3));
    }

    #[test]
    fn stale_claims_are_taken_over() {
        let c = conn();
        add(&c, "g", 5, 5).unwrap();
        let groups = vec!["g".to_string()];
        assert!(claim(&c, &groups, "a", 1000).unwrap().is_some());
        assert_eq!(claim(&c, &groups, "b", 1000 + CLAIM_TIMEOUT - 1).unwrap(), None);
        assert_eq!(claim(&c, &groups, "b", 1000 + CLAIM_TIMEOUT + 1).unwrap(), Some(("g".into(), 5)));
    }

    #[test]
    fn only_listed_groups_and_unsplit_groups() {
        let c = conn();
        add(&c, "g", 5, 5).unwrap();
        assert_eq!(claim(&c, &["other".to_string()], "a", 0).unwrap(), None);
        assert!(!is_split(&c, "other").unwrap());
        assert_eq!(split_groups(&c).unwrap(), vec!["g".to_string()]);
        assert_eq!(unix_day(86_400 * 3 + 5), 3);
    }
}
