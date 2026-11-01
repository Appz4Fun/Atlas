//! What a run of a release's steps comes to, written in one go on the shard's writer.

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params};

use super::step::{NameChange, StepMask, StepWrite};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Lane {
    #[default]
    Db,
    Net,
}

/// One release's visit. Masks are deltas: bits nobody touched stay as they are.
#[derive(Default)]
pub(crate) struct Applied {
    pub release_id: i64,
    pub lane: Lane,
    /// the queue row's time when it was read: an article since then moved it
    pub claimed_due: Option<i64>,
    /// `rerun_gen` read with the row, 0 with no row
    pub claimed_gen: i64,
    pub set_done: StepMask,
    pub set_failed: StepMask,
    pub clear: StepMask,
    /// cleared only if the name really changes
    pub rename_clears: StepMask,
    /// net steps left, for `net_wanted`
    pub net_mask: StepMask,
    /// every database step and every net step: a cleared one has to come back
    pub db_mask: StepMask,
    pub net_steps: StepMask,
    /// each with the step that made it
    pub writes: Vec<(StepMask, StepWrite)>,
    pub names: Vec<(StepMask, NameChange)>,
    pub db_attempts: Option<i64>,
    pub net_attempts: Option<i64>,
    pub net_next_at: Option<i64>,
    pub parts_seen: Option<i64>,
    pub error: Option<String>,
    /// a wait or a retry
    pub queue_at: Option<i64>,
    /// nothing worth a `pp_state` row
    pub no_state: bool,
}

/// One release's visit in a savepoint: a write that fails undoes this release
/// alone, which then counts as a failure of the lane that ran it.
pub(crate) fn apply_one(
    conn: &Connection,
    applied: Applied,
    now: i64,
    retry: &dyn Fn(i64) -> (i64, bool),
) -> rusqlite::Result<bool> {
    conn.execute_batch("savepoint one")?;
    let (id, lane, due, set_done) = (applied.release_id, applied.lane, applied.claimed_due, applied.set_done);
    let err = match write(conn, applied, now) {
        Ok(()) => {
            conn.execute_batch("release one")?;
            return Ok(true);
        }
        Err(e) => e.to_string(),
    };
    conn.execute_batch("rollback to one; release one")?;
    let column = if lane == Lane::Db { "db_attempts" } else { "net_attempts" };
    let counted: i64 = conn
        .prepare_cached(&format!("select {column} from pp_state where release_id = ?"))?
        .query_row([id], |r| r.get(0))
        .optional()?
        .unwrap_or(0);
    // a net run's attempt was counted before it ran
    let attempts = if lane == Lane::Db { counted + 1 } else { counted.max(1) };
    let (after, give_up) = retry(attempts);
    let failed = if give_up { set_done } else { 0 };
    let attempts = if give_up { 0 } else { attempts };
    conn.prepare_cached(&format!(
        "insert into pp_state (release_id, failed, {column}, last_error) values (?1, ?2, ?3, ?4)
         on conflict(release_id) do update set failed = failed | ?2, {column} = ?3, last_error = ?4"
    ))?
    .execute(params![id, failed, attempts, err])?;
    if lane == Lane::Db {
        conn.prepare_cached(
            "insert into pp_queue (release_id, due_at) values (?1, ?2)
             on conflict(release_id) do update set due_at = excluded.due_at where ?3 is null or pp_queue.due_at = ?3",
        )?
        .execute(params![id, now + after, due])?;
    } else {
        conn.prepare_cached("update pp_state set net_next_at = ? where release_id = ?")?
            .execute(params![now + after, id])?;
    }
    Ok(false)
}

fn write(conn: &Connection, mut a: Applied, now: i64) -> rusqlite::Result<()> {
    let row: Option<(Option<String>, i64, bool)> = conn
        .prepare_cached(
            "select display_name, coalesce(name_source, 0), coalesce(is_obfuscated, 0) != 0 from releases where id = ?",
        )?
        .query_row([a.release_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    let Some((mut current, mut source, was_obfuscated)) = row else {
        // gone while it ran. the trigger took its rows
        return Ok(());
    };

    // bits cleared since the claim may include a step that just ran, on input older
    // than the request: all of its result is dropped, and it runs again
    let stale: Option<(bool, i64)> = conn
        .prepare_cached("select rerun_gen != ?, rerun_mask from pp_state where release_id = ?")?
        .query_row(params![a.claimed_gen, a.release_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let dropped = stale.filter(|(differs, _)| *differs).map(|(_, mask)| mask);
    let is_stale = dropped.is_some();
    let dropped = dropped.unwrap_or(0);

    for (step, w) in a.writes.drain(..) {
        if step & dropped != 0 {
            continue;
        }
        let params: Vec<Value> = match w.late_params {
            Some(late) => late(),
            None => w.params,
        };
        conn.prepare_cached(w.sql)?.execute(rusqlite::params_from_iter(params))?;
    }
    for (step, name) in a.names.drain(..) {
        if step & dropped != 0
            || i64::from(name.source) < source
            || (name.display_name.is_none() && name.obfuscated.is_none())
        {
            continue;
        }
        if name.display_name.as_ref().is_some_and(|d| current.as_ref() != Some(d)) {
            a.clear |= a.rename_clears;
            let from = crate::metrics::catalog::name_source_name(name.source);
            crate::metrics::names::NAMES_APPLIED.inc(&[("source", from)], 1);
            // the first real name an obfuscated post gets
            if was_obfuscated && current.is_none() {
                crate::metrics::names::DEOBFUSCATED.inc(&[("source", from)], 1);
            }
            current = name.display_name.clone();
        }
        source = i64::from(name.source);
        conn.prepare_cached(
            "update releases set display_name = coalesce(?2, display_name), is_obfuscated = coalesce(?3, is_obfuscated),
             name_source = ?4 where id = ?1",
        )?
        .execute(params![a.release_id, name.display_name, name.obfuscated, source])?;
    }

    let dropped_db = (a.set_done | a.set_failed) & dropped & a.db_mask;
    let (set_done, set_failed) = (a.set_done & !dropped, a.set_failed & !dropped);
    if is_stale {
        // the run's own idea of what is left predates the clear
        a.net_mask |= a.net_steps & dropped;
        if a.lane == Lane::Net {
            a.net_next_at = Some(0);
        }
    }
    if !a.no_state {
        conn.prepare_cached(
            "insert into pp_state (release_id, done, failed, net_wanted, db_attempts, net_attempts, net_next_at, parts_seen,
                                   last_error, rerun_gen, rerun_mask)
             values (?1, ?2 & ~?4, ?3 & ~?2 & ~?4, case when (?5 & ~(?2 & ~?4) & ~(?3 & ~?4)) != 0 then 1 end,
                     coalesce(?6, 0), coalesce(?7, 0), coalesce(?8, 0), coalesce(?9, 0), ?10, ?4 != 0, ?4)
             on conflict(release_id) do update set
                done = (done | ?2) & ~?4,
                failed = (failed | ?3) & ~?2 & ~?4,
                net_wanted = case when (?5 & ~((done | ?2) & ~?4) & ~((failed | ?3) & ~?4)) != 0 then 1 end,
                db_attempts = coalesce(?6, db_attempts),
                net_attempts = coalesce(?7, net_attempts),
                net_next_at = coalesce(?8, net_next_at),
                parts_seen = coalesce(?9, parts_seen),
                last_error = ?10,
                rerun_gen = rerun_gen + (?4 != 0),
                rerun_mask = rerun_mask | ?4",
        )?
        .execute(params![
            a.release_id,
            set_done,
            set_failed,
            a.clear,
            a.net_mask,
            a.db_attempts,
            a.net_attempts,
            a.net_next_at,
            a.parts_seen,
            a.error,
        ])?;
    }

    if let (Some(claimed), None, false) = (a.claimed_due, a.queue_at, is_stale) {
        conn.prepare_cached("delete from pp_queue where release_id = ? and due_at = ?")?
            .execute(params![a.release_id, claimed])?;
    }
    if let Some(at) = a.queue_at {
        conn.prepare_cached(
            "insert into pp_queue (release_id, due_at) values (?1, ?2)
             on conflict(release_id) do update set due_at = excluded.due_at where ?3 is null or pp_queue.due_at = ?3",
        )?
        .execute(params![a.release_id, at, a.claimed_due])?;
    }
    // a cleared database step, as a net rename or a rerun leaves it, or one dropped as stale
    if a.clear & a.db_mask != 0 || dropped_db != 0 {
        conn.prepare_cached(
            "insert into pp_queue (release_id, due_at) values (?, ?) on conflict(release_id) do nothing",
        )?
        .execute(params![a.release_id, now])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::parser::{Article, Release};
    use crate::postprocess::step::{StepId, mask};
    use crate::store;

    const G: &str = "alt.binaries.t";
    const NOW: i64 = 1_000_000;
    const A: StepMask = mask(StepId(1));
    const B: StepMask = mask(StepId(2));
    const C: StepMask = mask(StepId(3));

    fn shard() -> (tempfile::TempDir, Connection, i64) {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let article = Article {
            message_id: "<a@x>".into(),
            subject: "\"Old.Name\" yEnc (1/1)".into(),
            filename: Some("Old.Name".into()),
            part: Some(1),
            total_parts: Some(1),
            bytes: 10,
            ..Default::default()
        };
        store::save(
            &main,
            &[Release { name: "Old.Name".into(), group: G.into(), articles: vec![article], ..Default::default() }],
        )
        .unwrap();
        let conn = db::open_shard(&store::shard_path(&main, store::shard_of(G))).unwrap();
        let id = conn.query_row("select id from releases", [], |r| r.get(0)).unwrap();
        conn.execute("delete from pp_queue", []).unwrap();
        (dir, conn, id)
    }

    fn run(conn: &Connection, a: Applied) -> bool {
        apply_one(conn, a, NOW, &|attempts| (900 * attempts, attempts >= 5)).unwrap()
    }

    fn state(conn: &Connection, id: i64) -> (i64, i64, Option<i64>, i64, i64, i64, Option<String>) {
        conn.query_row(
            "select done, failed, net_wanted, db_attempts, net_attempts, net_next_at, last_error from pp_state where release_id = ?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )
        .unwrap()
    }

    fn rename(name: &str, source: u8) -> NameChange {
        NameChange { display_name: Some(name.into()), obfuscated: None, source }
    }

    fn fts_hits(conn: &Connection, word: &str) -> i64 {
        conn.query_row("select count(*) from releases_fts where releases_fts match ?", [word], |r| r.get(0)).unwrap()
    }

    #[test]
    fn a_rename_updates_search_and_clears_later_steps_only() {
        let (_dir, conn, id) = shard();
        conn.execute("insert into pp_state (release_id, done) values (?, ?)", params![id, A | B | C]).unwrap();
        run(
            &conn,
            Applied {
                release_id: id,
                names: vec![(A, rename("First.Name", 40))],
                rename_clears: B,
                ..Default::default()
            },
        );
        assert_eq!(fts_hits(&conn, "First"), 1);
        assert_eq!(state(&conn, id).0, A | C, "only the rename_clears bits go");
        run(&conn, Applied { release_id: id, names: vec![(A, rename("Second.Name", 40))], ..Default::default() });
        assert_eq!(fts_hits(&conn, "Second"), 1);
        assert_eq!(fts_hits(&conn, "First"), 0, "the old display_name token is gone");
        let source: i64 = conn.query_row("select name_source from releases", [], |r| r.get(0)).unwrap();
        assert_eq!(source, 40);
    }

    #[test]
    fn the_same_name_clears_nothing() {
        let (_dir, conn, id) = shard();
        conn.execute("update releases set display_name = 'Same', name_source = 40", []).unwrap();
        conn.execute("insert into pp_state (release_id, done) values (?, ?)", params![id, B]).unwrap();
        run(
            &conn,
            Applied { release_id: id, names: vec![(A, rename("Same", 40))], rename_clears: B, ..Default::default() },
        );
        assert_eq!(state(&conn, id).0, B);
    }

    #[test]
    fn a_lower_source_does_not_replace_a_higher_one() {
        let (_dir, conn, id) = shard();
        conn.execute("update releases set display_name = 'High', name_source = 40", []).unwrap();
        run(&conn, Applied { release_id: id, names: vec![(A, rename("Low", 10))], ..Default::default() });
        let name: String = conn.query_row("select display_name from releases", [], |r| r.get(0)).unwrap();
        assert_eq!(name, "High");
    }

    #[test]
    fn obfuscated_none_leaves_the_flag_and_display_name_none_leaves_the_name() {
        let (_dir, conn, id) = shard();
        conn.execute("update releases set is_obfuscated = 1", []).unwrap();
        let read = |conn: &Connection| -> (Option<String>, i64, Option<i64>) {
            conn.query_row("select display_name, is_obfuscated, name_source from releases", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap()
        };
        let both_none = NameChange { display_name: None, obfuscated: None, source: 40 };
        run(&conn, Applied { release_id: id, names: vec![(A, both_none)], ..Default::default() });
        assert_eq!(read(&conn), (None, 1, None), "nothing happens");
        run(&conn, Applied { release_id: id, names: vec![(A, rename("Named", 40))], ..Default::default() });
        assert_eq!(read(&conn), (Some("Named".into()), 1, Some(40)));
        conn.execute("insert or replace into pp_state (release_id, done) values (?, ?)", params![id, B]).unwrap();
        let flag = NameChange { display_name: None, obfuscated: Some(false), source: 50 };
        run(&conn, Applied { release_id: id, names: vec![(A, flag)], rename_clears: B, ..Default::default() });
        assert_eq!(read(&conn), (Some("Named".into()), 0, Some(50)));
        assert_eq!(state(&conn, id).0, B, "a flag change clears nothing");
    }

    #[test]
    fn a_release_deleted_meanwhile_records_nothing() {
        let (_dir, conn, id) = shard();
        conn.execute_batch("delete from segments; delete from files; delete from releases").unwrap();
        let w = StepWrite::new("insert into pp_queue (release_id, due_at) values (1, 1)", vec![]);
        run(&conn, Applied { release_id: id, set_done: A, writes: vec![(A, w)], ..Default::default() });
        let rows: i64 = conn
            .query_row("select (select count(*) from pp_state) + (select count(*) from pp_queue)", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn a_bad_write_fails_that_release_alone() {
        let (_dir, conn, id) = shard();
        conn.execute_batch("create table side (id INTEGER PRIMARY KEY)").unwrap();
        let good = StepWrite::new("insert into side (id) values (1)", vec![]);
        let bad = StepWrite::new("insert into no_such_table values (2)", vec![]);
        assert!(!run(
            &conn,
            Applied { release_id: id, set_done: A, writes: vec![(A, good), (A, bad)], ..Default::default() }
        ));
        let s = state(&conn, id);
        assert_eq!((s.0, s.3), (0, 1), "no bit, one attempt");
        assert!(s.6.is_some(), "last_error");
        let side: i64 = conn.query_row("select count(*) from side", [], |r| r.get(0)).unwrap();
        assert_eq!(side, 0, "its own earlier write is undone");
    }

    /// what `Stage::rerun` does on the writer
    fn rerun(conn: &Connection, id: i64, m: StepMask) {
        crate::postprocess::rerun_on(conn, id, m, NOW).unwrap();
    }

    #[test]
    fn masks_are_deltas() {
        let (_dir, conn, id) = shard();
        conn.execute("insert into pp_state (release_id, done) values (?, ?)", params![id, A | B]).unwrap();
        rerun(&conn, id, B);
        run(&conn, Applied { release_id: id, claimed_gen: 1, set_done: C, ..Default::default() });
        assert_eq!(state(&conn, id).0, A | C, "the rerun's cleared bit stays clear");
    }

    #[test]
    fn a_rerun_of_the_running_step_survives_its_result() {
        let (_dir, conn, id) = shard();
        conn.execute("insert into pp_state (release_id) values (?)", [id]).unwrap();
        conn.execute("insert into pp_queue (release_id, due_at) values (?, ?)", params![id, NOW]).unwrap();
        rerun(&conn, id, A);
        run(
            &conn,
            Applied { release_id: id, claimed_gen: 0, claimed_due: Some(NOW), set_done: A | B, ..Default::default() },
        );
        assert_eq!(state(&conn, id).0, B, "A stays clear, B wasnt in the mask");
        let queued: i64 = conn.query_row("select count(*) from pp_queue", [], |r| r.get(0)).unwrap();
        assert_eq!(queued, 1, "the row stays, though its due_at equals the claim");
    }

    #[test]
    fn a_stale_results_writes_and_name_are_dropped_too() {
        let (_dir, conn, id) = shard();
        conn.execute_batch("create table side (id INTEGER PRIMARY KEY)").unwrap();
        conn.execute("insert into pp_state (release_id) values (?)", [id]).unwrap();
        rerun(&conn, id, A);
        run(
            &conn,
            Applied {
                release_id: id,
                claimed_gen: 0,
                set_done: A | B,
                writes: vec![
                    (A, StepWrite::new("insert into side (id) values (1)", vec![])),
                    (B, StepWrite::new("insert into side (id) values (2)", vec![])),
                ],
                names: vec![(A, rename("Stale", 40))],
                ..Default::default()
            },
        );
        let ids: Vec<i64> = conn
            .prepare("select id from side")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(ids, vec![2], "only the other step's write");
        let name: Option<String> = conn.query_row("select display_name from releases", [], |r| r.get(0)).unwrap();
        assert_eq!(name, None);
    }

    #[test]
    fn a_stale_net_result_leaves_the_net_step_wanted() {
        let (_dir, conn, id) = shard();
        conn.execute(
            "insert into pp_state (release_id, done, net_attempts, net_next_at) values (?, ?, 1, ?)",
            params![id, A, NOW + 900],
        )
        .unwrap();
        // a DB visit clears it and asks for it
        run(
            &conn,
            Applied { release_id: id, claimed_gen: 0, clear: A, net_mask: A, net_steps: A, ..Default::default() },
        );
        assert_eq!(state(&conn, id).2, Some(1));
        // the old run comes back thinking nothing is left
        run(
            &conn,
            Applied {
                release_id: id,
                lane: Lane::Net,
                claimed_gen: 0,
                set_done: A,
                net_mask: 0,
                net_steps: A,
                net_attempts: Some(0),
                ..Default::default()
            },
        );
        let s = state(&conn, id);
        assert_eq!((s.0, s.2, s.5), (0, Some(1), 0), "not done, still wanted, due now");
        let queued: i64 = conn.query_row("select count(*) from pp_queue", [], |r| r.get(0)).unwrap();
        assert_eq!(queued, 0, "the net lane runs it with no new queue row");
    }

    #[test]
    fn a_net_completion_doesnt_use_up_growth() {
        let (_dir, conn, id) = shard();
        conn.execute("insert into pp_state (release_id, parts_seen) values (?, 1)", [id]).unwrap();
        run(&conn, Applied { release_id: id, lane: Lane::Net, set_done: A, parts_seen: None, ..Default::default() });
        let seen: i64 = conn.query_row("select parts_seen from pp_state", [], |r| r.get(0)).unwrap();
        assert_eq!(seen, 1);
    }

    #[test]
    fn a_finished_step_has_no_failure_left() {
        let (_dir, conn, id) = shard();
        conn.execute("insert into pp_state (release_id, failed) values (?, ?)", params![id, A | B]).unwrap();
        run(&conn, Applied { release_id: id, set_done: A, ..Default::default() });
        let s = state(&conn, id);
        assert_eq!((s.0, s.1), (A, B));
    }

    #[test]
    fn a_rerun_of_a_release_with_no_state_is_remembered() {
        let (_dir, conn, id) = shard();
        rerun(&conn, id, A);
        run(&conn, Applied { release_id: id, claimed_gen: 0, set_done: A, ..Default::default() });
        assert_eq!(state(&conn, id).0, 0);
        rerun(&conn, id + 1_000_000, A);
        let rows: i64 = conn.query_row("select count(*) from pp_state", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1, "a rerun of an id with no release writes nothing");
    }

    #[test]
    fn a_db_visit_leaves_the_net_columns_alone() {
        let (_dir, conn, id) = shard();
        conn.execute(
            "insert into pp_state (release_id, net_attempts, net_next_at, db_attempts) values (?, 3, 999, 2)",
            [id],
        )
        .unwrap();
        run(&conn, Applied { release_id: id, set_done: A, db_attempts: Some(0), ..Default::default() });
        let s = state(&conn, id);
        assert_eq!((s.3, s.4, s.5), (0, 3, 999));
        run(
            &conn,
            Applied {
                release_id: id,
                lane: Lane::Net,
                net_attempts: Some(0),
                net_next_at: Some(0),
                ..Default::default()
            },
        );
        conn.execute("update pp_state set db_attempts = 2", []).unwrap();
        run(&conn, Applied { release_id: id, lane: Lane::Net, net_attempts: Some(1), ..Default::default() });
        assert_eq!(state(&conn, id).3, 2, "the net lane's keeps db_attempts");
    }

    #[test]
    fn net_wanted_follows_the_mask() {
        let (_dir, conn, id) = shard();
        run(&conn, Applied { release_id: id, net_mask: A, ..Default::default() });
        assert_eq!(state(&conn, id).2, Some(1));
        run(&conn, Applied { release_id: id, net_mask: A, set_done: A, ..Default::default() });
        assert_eq!(state(&conn, id).2, None, "done");
        run(&conn, Applied { release_id: id, net_mask: A, clear: A, ..Default::default() });
        assert_eq!(state(&conn, id).2, Some(1), "clear takes it back");
        run(&conn, Applied { release_id: id, claimed_gen: 1, net_mask: A, set_failed: A, ..Default::default() });
        assert_eq!(state(&conn, id).2, None, "failed");
    }

    #[test]
    fn an_article_during_a_run_keeps_the_queue_row() {
        let (_dir, conn, id) = shard();
        conn.execute("insert into pp_queue (release_id, due_at) values (?, ?)", params![id, NOW + 500]).unwrap();
        run(&conn, Applied { release_id: id, claimed_due: Some(NOW), set_done: A, ..Default::default() });
        let due =
            |conn: &Connection| -> i64 { conn.query_row("select due_at from pp_queue", [], |r| r.get(0)).unwrap() };
        assert_eq!(due(&conn), NOW + 500);
        run(&conn, Applied { release_id: id, claimed_due: Some(NOW), queue_at: Some(NOW + 60), ..Default::default() });
        assert_eq!(due(&conn), NOW + 500, "a retry doesnt overwrite its newer time");
    }

    #[test]
    fn a_net_write_failure_uses_only_the_attempt_begin_counted() {
        let (_dir, conn, id) = shard();
        conn.execute("insert into pp_state (release_id, net_attempts) values (?, 2)", [id]).unwrap();
        let bad = StepWrite::new("insert into no_such_table values (2)", vec![]);
        assert!(!run(
            &conn,
            Applied { release_id: id, lane: Lane::Net, set_done: A, writes: vec![(A, bad)], ..Default::default() }
        ));
        assert_eq!(state(&conn, id).4, 2);
    }
}
