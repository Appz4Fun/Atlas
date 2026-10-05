//! Day chunks of a group's backfill that any server carrying the group can
//! take (see docs/superpowers/specs/2026-10-05-faster-backfill-smaller-storage-design.md).
//! Lives in the main database.

use rusqlite::{Connection, OptionalExtension, Result, params};

/// a claim older than this is taken over (its worker died or was stopped)
pub const CLAIM_TIMEOUT: i64 = 30 * 60;

const PENDING: i64 = 0;
const CLAIMED: i64 = 1;
const DONE: i64 = 2;

/// Initialize the backfill_chunks table if it doesn't exist.
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

/// Atomically claim the newest chunk that is pending or whose claim went
/// stale, of `groups`: (group, oldest day the server keeps), `i64::MIN` when
/// any day will do.
pub fn claim(conn: &Connection, groups: &[(String, i64)], host: &str, now: i64) -> Result<Option<Claim>> {
    if groups.is_empty() {
        return Ok(None);
    }
    let rows = vec!["(?, ?)"; groups.len()].join(", ");
    let sql = format!(
        "with wanted (g, oldest) as (values {rows})
         update backfill_chunks set state = ?, server = ?, claimed_at = ?
         where (grp, day) = (select c.grp, c.day from backfill_chunks c join wanted w on w.g = c.grp
                             where c.day >= w.oldest
                               and (c.state = {PENDING} or (c.state = {CLAIMED} and c.claimed_at < ?))
                             order by c.day desc limit 1)
         returning grp, day"
    );
    let mut values: Vec<rusqlite::types::Value> = Vec::new();
    for (g, oldest) in groups {
        values.push(g.clone().into());
        values.push((*oldest).into());
    }
    values.extend([CLAIMED.into(), host.to_string().into(), now.into()]);
    values.push((now - CLAIM_TIMEOUT).into());
    let found: Option<(String, i64)> =
        conn.prepare(&sql)?.query_row(rusqlite::params_from_iter(values), |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    Ok(found.map(|(group, day)| Claim { group, day, server: host.to_string(), claimed_at: now }))
}

/// Mark a claimed chunk as done. False when the claim was taken over.
pub fn finish(conn: &Connection, claim: &Claim) -> Result<bool> {
    let n = conn.execute(
        &format!(
            "update backfill_chunks set state = ?
             where grp = ? and day = ? and state = {CLAIMED} and server = ? and claimed_at = ?"
        ),
        params![DONE, claim.group, claim.day, claim.server, claim.claimed_at],
    )?;
    Ok(n > 0)
}

/// Return a claimed chunk to pending state. False when the claim was taken over.
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
        assert!(finish(&c, &a).unwrap());
        assert!(release(&c, &b).unwrap());
        assert_eq!(day(claim(&c, &groups, "b", 1000)), Some(19_999), "released goes back");
        assert_eq!(day(claim(&c, &groups, "a", 1000)), Some(19_998));
        assert_eq!(claim(&c, &groups, "a", 1000).unwrap(), None);
        assert_eq!(progress(&c, "g").unwrap(), (1, 3));
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
        assert!(finish(&c, &b).unwrap());

        // a's late release and finish change nothing: done, and b's
        assert!(!release(&c, &a).unwrap());
        assert!(!finish(&c, &a).unwrap());
        let row = |c: &Connection| {
            c.query_row("select state, server from backfill_chunks where grp = 'g' and day = 5", [], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .unwrap()
        };
        assert_eq!(row(&c), (DONE, Some("b".to_string())));
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
        assert_eq!(day(claim(&c, &from_11, "short", 0)), Some(12));
        assert_eq!(day(claim(&c, &from_11, "short", 0)), Some(11));
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
