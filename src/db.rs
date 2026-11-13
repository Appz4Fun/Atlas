//! PostgreSQL group cursors, server bounds and dated indexing progress.

pub use crate::catalog_facts::{Dated, GroupProgress, GroupState, best_progress, row_numbers};
use crate::pg::sql::{Sql, vals};

pub fn get_group_state(db: &dyn Sql, group: &str) -> crate::pg::sql::Result<Option<GroupState>> {
    let row: Option<(Option<i64>, Option<i64>)> = db
        .row("select live_cursor, backfill_cursor from groups where name = $1", vals![group])?
        .map(|r| Ok::<_, crate::pg::sql::Error>((r.get(0)?, r.get(1)?)))
        .transpose()?;

    Ok(match row {
        Some((Some(live), Some(back))) => Some(GroupState { live_cursor: live, backfill_cursor: back }),
        _ => None,
    })
}

pub fn init_group_state(db: &dyn Sql, group: &str, cursor: i64) -> crate::pg::sql::Result<()> {
    db.execute(
        "insert into groups(name, live_cursor, backfill_cursor) values ($1, $2, $3)
         on conflict (name) do update set
         live_cursor = excluded.live_cursor,
         backfill_cursor = excluded.backfill_cursor",
        vals![group, cursor, cursor],
    )?;
    Ok(())
}

/// Remember the article numbers the server reported for a group (cursor key).
pub fn save_group_bounds(db: &dyn Sql, group: &str, first: i64, last: i64) -> crate::pg::sql::Result<()> {
    db.execute("update groups set first_article = $1, last_article = $2 where name = $3", vals![first, last, group])?;
    Ok(())
}

/// Widen the dated ends of what has been indexed on a group (cursor key): `low`
/// replaces the stored low end when it is further back, `high` the high end
/// when it is further ahead.
pub fn save_group_dates(db: &dyn Sql, group: &str, low: Dated, high: Dated) -> crate::pg::sql::Result<()> {
    // every right hand side sees the row as it was, soo each pair moves together
    db.execute(
        "update groups set
         low_article = case when low_article is null or $1 < low_article then $1 else low_article end,
         low_posted = case when low_article is null or $1 < low_article then $2 else low_posted end,
         high_article = case when high_article is null or $3 > high_article then $3 else high_article end,
         high_posted = case when high_article is null or $3 > high_article then $4 else high_posted end
         where name = $5",
        vals![low.0, low.1, high.0, high.1, group],
    )?;
    Ok(())
}

pub fn group_progress(db: &dyn Sql) -> crate::pg::sql::Result<Vec<GroupProgress>> {
    let pair = |a: Option<i64>, b: Option<i64>| a.zip(b);
    db.query(
        "select name, live_cursor, backfill_cursor, first_article, last_article,
                low_article, low_posted, high_article, high_posted from groups
         where live_cursor is not null and backfill_cursor is not null",
        vals![],
    )?
    .into_iter()
    .map(|r| {
        Ok(GroupProgress {
            key: r.get(0)?,
            live_cursor: r.get(1)?,
            backfill_cursor: r.get(2)?,
            first: r.get(3)?,
            last: r.get(4)?,
            low: pair(r.get(5)?, r.get(6)?),
            high: pair(r.get(7)?, r.get(8)?),
        })
    })
    .collect()
}

pub fn save_group_state(db: &dyn Sql, group: &str, state: GroupState) -> crate::pg::sql::Result<()> {
    db.execute(
        "insert into groups(name, live_cursor, backfill_cursor) values ($1, $2, $3)
         on conflict (name) do update set
         live_cursor = excluded.live_cursor,
         backfill_cursor = excluded.backfill_cursor",
        vals![group, state.live_cursor, state.backfill_cursor],
    )?;
    Ok(())
}

pub fn update_live_cursor(db: &dyn Sql, group: &str, article: i64) -> crate::pg::sql::Result<()> {
    db.execute(
        "insert into groups(name, live_cursor, backfill_cursor) values ($1, $2, $3)
         on conflict (name) do update set live_cursor = excluded.live_cursor",
        vals![group, article, article],
    )?;
    Ok(())
}

/// Remember the posting time a row's live cursor began reading from (cursor key), when it is started or jumped:
/// the time it was found by, or now when it starts at the newest article. Nothing posted before that is read through
/// on this row; NULL (a row from before this was kept) counts as long ago.
pub fn set_start_posted(db: &dyn Sql, group: &str, posted: i64) -> crate::pg::sql::Result<()> {
    db.execute("update groups set start_posted = $1 where name = $2", vals![posted, group])?;
    Ok(())
}

pub fn update_backfill_cursor(db: &dyn Sql, group: &str, article: i64) -> crate::pg::sql::Result<()> {
    db.execute(
        "insert into groups(name, live_cursor, backfill_cursor) values ($1, $2, $3)
         on conflict (name) do update set backfill_cursor = excluded.backfill_cursor",
        vals![group, article, article],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_group_cursors_keep_absence_and_independent_updates() {
        crate::pg::testing::each(|db| {
            assert_eq!(get_group_state(db, "missing").unwrap(), None);
            update_live_cursor(db, "group", 10).unwrap();
            assert_eq!(
                get_group_state(db, "group").unwrap(),
                Some(GroupState { live_cursor: 10, backfill_cursor: 10 })
            );
            update_backfill_cursor(db, "group", 5).unwrap();
            update_live_cursor(db, "group", 20).unwrap();
            assert_eq!(get_group_state(db, "group").unwrap(), Some(GroupState { live_cursor: 20, backfill_cursor: 5 }));
            save_group_state(db, "group", GroupState { live_cursor: 30, backfill_cursor: 4 }).unwrap();
            set_start_posted(db, "group", 123).unwrap();
            assert_eq!(db.one::<i64>("select start_posted from groups where name='group'", vals![]).unwrap(), 123);
            assert_eq!(get_group_state(db, "group").unwrap(), Some(GroupState { live_cursor: 30, backfill_cursor: 4 }));
            db.execute("update groups set backfill_cursor=null where name='group'", vals![]).unwrap();
            assert_eq!(get_group_state(db, "group").unwrap(), None);
            assert!(group_progress(db).unwrap().is_empty());
        });
    }

    #[test]
    fn group_dates_only_ever_widen() {
        crate::pg::testing::each(|conn| {
            init_group_state(conn, "g@news.x", 1000).unwrap();
            save_group_bounds(conn, "g@news.x", 1, 1000).unwrap();
            let ends = || {
                let p = group_progress(conn).unwrap().remove(0);
                (p.low, p.high)
            };
            assert_eq!(ends(), (None, None));

            // first pass: the newest slice
            save_group_dates(conn, "g@news.x", (900, 9_000), (990, 9_900)).unwrap();
            assert_eq!(ends(), (Some((900, 9_000)), Some((990, 9_900))));

            // backfill reaches further back: only the low end moves, with its date
            save_group_dates(conn, "g@news.x", (500, 5_000), (800, 8_000)).unwrap();
            assert_eq!(ends(), (Some((500, 5_000)), Some((990, 9_900))));

            // a live pass: only the high end moves
            save_group_dates(conn, "g@news.x", (995, 9_950), (1000, 10_000)).unwrap();
            assert_eq!(ends(), (Some((500, 5_000)), Some((1000, 10_000))));
        });
    }
}
