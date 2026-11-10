//! Leases for PostgreSQL release postprocessing work.

use anyhow::{Context, Result, ensure};
use postgres::{Client, Transaction};

/// Maximum rows locked by one claim statement.
pub const MAX_CLAIM: usize = 256;

#[derive(Debug)]
pub struct Claim {
    pub release_id: i64,
    pub due_at: i64,
    pub lease_until: i64,
    generation: i64,
    token: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// This claim still owned the current generation, so it was acknowledged or rescheduled.
    Applied,
    /// A newer generation is queued. The claim's lease was released without changing the newer due time.
    Superseded,
    /// The row disappeared or the lease expired or was replaced. No queue state was changed.
    LeaseLost,
}

fn deadline(now: i64, limit: usize, lease_seconds: i64) -> Result<i64> {
    ensure!((1..=MAX_CLAIM).contains(&limit), "claim limit must be between 1 and {MAX_CLAIM}");
    ensure!(lease_seconds > 0, "lease duration must be positive");
    now.checked_add(lease_seconds).context("lease deadline is out of range")
}

/// Lease the oldest due rows with a bounded, atomic statement. Locked rows are skipped, and expired leases
/// become eligible without a separate sweeper. `now` and `due_at` are Unix seconds from the scheduler's clock.
/// Settle returned claims against the same database and schema that issued them.
pub fn claim(client: &mut Client, now: i64, limit: usize, lease_seconds: i64) -> Result<Vec<Claim>> {
    let lease_until = deadline(now, limit, lease_seconds)?;
    let limit = limit as i64;
    let rows = client.query(
        "with due as (
             select release_id from pp_queue
             where due_at <= $1 and (lease_until is null or lease_until <= $1)
             order by due_at, release_id limit $2 for update skip locked
         ), leased as (
             update pp_queue q set lease_token = nextval('pp_queue_lease_token_seq'), lease_until = $3
             from due where q.release_id = due.release_id
             returning q.release_id, q.due_at, q.lease_until, q.generation, q.lease_token
         ) select release_id, due_at, lease_until, generation, lease_token from leased order by due_at, release_id",
        &[&now, &limit, &lease_until],
    )?;
    Ok(rows
        .into_iter()
        .map(|row| Claim {
            release_id: row.get(0),
            due_at: row.get(1),
            lease_until: row.get(2),
            generation: row.get(3),
            token: row.get(4),
        })
        .collect())
}

/// Acknowledge a claim. Commit postprocessing results only with `Applied` in the same transaction.
/// Acquire any release locks before this queue lock, in the writer's order. If a caller already
/// wrote results and receives another outcome, it must roll back those results.
pub fn ack(tx: &mut Transaction<'_>, claim: &Claim, now: i64) -> Result<Outcome> {
    settle(tx, claim, now, None)
}

/// Reschedule the claimed generation at `due_at`. A newer generation keeps its own due time.
/// The same transaction and lock-order requirements as [`ack`] apply.
pub fn requeue(tx: &mut Transaction<'_>, claim: &Claim, now: i64, due_at: i64) -> Result<Outcome> {
    settle(tx, claim, now, Some(due_at))
}

fn settle(tx: &mut Transaction<'_>, claim: &Claim, now: i64, retry_at: Option<i64>) -> Result<Outcome> {
    let Some(row) = tx.query_opt(
        "select generation from pp_queue where release_id = $1 and lease_token = $2 and lease_until > $3 for update",
        &[&claim.release_id, &claim.token, &now],
    )?
    else {
        return Ok(Outcome::LeaseLost);
    };
    let current = row.get::<_, i64>(0) == claim.generation;
    if current && retry_at.is_none() {
        tx.execute("delete from pp_queue where release_id = $1", &[&claim.release_id])?;
    } else {
        let due_at = retry_at.filter(|_| current);
        tx.execute(
            "update pp_queue set due_at = coalesce($2, due_at), lease_token = null, lease_until = null
             where release_id = $1",
            &[&claim.release_id, &due_at],
        )?;
    }
    Ok(if current { Outcome::Applied } else { Outcome::Superseded })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(client: &mut postgres::Client, key: u8, due: i64) -> i64 {
        let identity: &[u8] = &[key];
        client
            .query_one(
                "with added as (
                     insert into releases (group_name, is_collection, identity_key, name, poster, posted_date,
                                           is_obfuscated)
                     values ('test.group', false, $1, 'release', 'poster', 'date', false) returning id
                 ) insert into pp_queue (release_id, due_at) select id, $2 from added returning release_id",
                &[&identity, &due],
            )
            .unwrap()
            .get(0)
    }

    #[test]
    fn invalid_limits_and_lease_arithmetic_are_refused() {
        assert_eq!(deadline(10, 1, 20).unwrap(), 30);
        assert!(deadline(10, 0, 20).is_err());
        assert!(deadline(10, MAX_CLAIM + 1, 20).is_err());
        assert!(deadline(10, 1, 0).is_err());
        assert!(deadline(i64::MAX, 1, 1).is_err());
    }

    #[test]
    fn claims_skip_locked_rows_and_respect_order_limit_and_due_time() {
        let Some(test) = crate::pg::testing::Schema::new() else { return };
        let mut a = test.pool.get().unwrap();
        let mut b = test.pool.get().unwrap();
        b.batch_execute("set lock_timeout = '1s'").unwrap();
        let oldest = push(&mut a, 1, 10);
        let second = push(&mut a, 2, 20);
        let third = push(&mut a, 3, 20);
        push(&mut a, 4, 31);

        let mut tx = a.transaction().unwrap();
        tx.query_one("select 1 from pp_queue where release_id = $1 for update", &[&oldest]).unwrap();
        let work = claim(&mut b, 30, 1, 10).unwrap();
        assert_eq!(work.iter().map(|claim| claim.release_id).collect::<Vec<_>>(), [second]);
        tx.commit().unwrap();

        let work = claim(&mut a, 30, 10, 10).unwrap();
        assert_eq!(work.iter().map(|claim| claim.release_id).collect::<Vec<_>>(), [oldest, third]);
        assert!(claim(&mut b, 30, 10, 10).unwrap().is_empty());
    }

    #[test]
    fn lease_expiry_retry_and_ack_do_not_lose_work() {
        let Some(test) = crate::pg::testing::Schema::new() else { return };
        let mut client = test.pool.get().unwrap();
        let id = push(&mut client, 1, 100);
        let first = claim(&mut client, 100, 1, 10).unwrap().pop().unwrap();
        assert_eq!(first.release_id, id);
        assert_eq!(first.lease_until, 110);
        assert!(claim(&mut client, 109, 1, 10).unwrap().is_empty());

        let mut tx = client.transaction().unwrap();
        assert_eq!(ack(&mut tx, &first, 110).unwrap(), Outcome::LeaseLost);
        tx.commit().unwrap();

        let renewed = claim(&mut client, 110, 1, 10).unwrap().pop().unwrap();
        assert_ne!(renewed.token, first.token);
        let mut tx = client.transaction().unwrap();
        assert_eq!(ack(&mut tx, &first, 110).unwrap(), Outcome::LeaseLost);
        assert_eq!(requeue(&mut tx, &renewed, 111, 200).unwrap(), Outcome::Applied);
        tx.commit().unwrap();
        assert!(claim(&mut client, 199, 1, 10).unwrap().is_empty());

        let retry = claim(&mut client, 200, 1, 10).unwrap().pop().unwrap();
        let mut tx = client.transaction().unwrap();
        assert_eq!(ack(&mut tx, &retry, 201).unwrap(), Outcome::Applied);
        tx.commit().unwrap();
        assert!(claim(&mut client, 1_000, 1, 10).unwrap().is_empty());
    }

    #[test]
    fn newer_generation_survives_ack_and_retry_at_the_same_due_time() {
        let Some(test) = crate::pg::testing::Schema::new() else { return };
        let mut client = test.pool.get().unwrap();
        for (key, retry) in [(1, false), (2, true)] {
            let id = push(&mut client, key, 100);
            let prior = claim(&mut client, 100, 1, 10).unwrap().pop().unwrap();
            client.execute("update pp_queue set generation = generation + 1 where release_id = $1", &[&id]).unwrap();
            let mut tx = client.transaction().unwrap();
            let outcome =
                if retry { requeue(&mut tx, &prior, 101, 200).unwrap() } else { ack(&mut tx, &prior, 101).unwrap() };
            assert_eq!(outcome, Outcome::Superseded);
            tx.commit().unwrap();

            let next = claim(&mut client, 101, 1, 10).unwrap().pop().unwrap();
            assert_eq!(next.release_id, id);
            assert_eq!(next.due_at, 100);
            assert_eq!(next.generation, prior.generation + 1);
            let mut tx = client.transaction().unwrap();
            assert_eq!(ack(&mut tx, &next, 102).unwrap(), Outcome::Applied);
            tx.commit().unwrap();
        }
    }

    #[test]
    fn a_recreated_queue_row_cannot_be_acked_by_an_old_worker() {
        let Some(test) = crate::pg::testing::Schema::new() else { return };
        let mut client = test.pool.get().unwrap();
        let id = push(&mut client, 1, 100);
        let first = claim(&mut client, 100, 1, 10).unwrap().pop().unwrap();
        let mut tx = client.transaction().unwrap();
        assert_eq!(ack(&mut tx, &first, 101).unwrap(), Outcome::Applied);
        tx.commit().unwrap();

        client.execute("insert into pp_queue (release_id, due_at) values ($1, 100)", &[&id]).unwrap();
        let next = claim(&mut client, 101, 1, 10).unwrap().pop().unwrap();
        assert_eq!(next.generation, first.generation);
        let mut tx = client.transaction().unwrap();
        assert_eq!(ack(&mut tx, &first, 102).unwrap(), Outcome::LeaseLost);
        assert_eq!(ack(&mut tx, &next, 102).unwrap(), Outcome::Applied);
        tx.rollback().unwrap();
        let mut tx = client.transaction().unwrap();
        assert_eq!(ack(&mut tx, &next, 103).unwrap(), Outcome::Applied);
        tx.commit().unwrap();
    }
}
