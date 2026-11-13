//! Spotnet bookkeeping in the native catalog, and pure DTOs used by its producer and readers.
//! The original-owner native store owns publication; ReadView owns bounded presentation.

use super::Kind;
use super::full::FullSpot;
use crate::pg::sql::{Sql, vals};

/// A spot waiting to be fetched whole.
pub const TODO: i64 = 0;
/// Saved as a release.
pub const SAVED: i64 = 1;
/// Fetched and not taken: not movies or TV after all, no NZB, an NZB that won't read, a group never saved.
pub const SKIPPED: i64 = 2;
/// Tried `MAX_TRIES` times without an answer.
pub const GAVE_UP: i64 = 3;

/// Tries a spot gets before it's given up on.
pub const MAX_TRIES: i64 = 5;
/// Spots of one spotter fetched in a round of `todo` before the next spotter's: one key posting thousands can't
/// hold the others back.
pub const PER_SPOTTER: i64 = 10;
/// Spots waiting at most; past it the oldest are given up on. About two months of movie and TV spots.
pub const MAX_TODO: i64 = 20_000;
/// A spot still waiting after this long is given up on, and one that came to nothing is forgotten (its `free.pt`
/// article is long behind the cursor). A saved one stays: its comments hang off it (`comments`).
pub const TODO_DAYS: i64 = 30;
pub const KEEP_DAYS: i64 = 180;

fn cursor_key(server: &str) -> String {
    format!("spots_cursor:{server}")
}

/// The last `free.pt` article read on `server`, by its numbers there.
pub fn cursor(db: &dyn Sql, server: &str) -> crate::pg::sql::Result<Option<u64>> {
    read_cursor(db, &cursor_key(server))
}

/// A group's cursor in `meta`: the last article read, by its numbers on the server the key names.
pub(super) fn read_cursor(db: &dyn Sql, key: &str) -> crate::pg::sql::Result<Option<u64>> {
    Ok(crate::store::meta(db, key)?.and_then(|v| u64::try_from(v).ok()))
}

pub(super) fn write_cursor(db: &dyn Sql, key: &str, last: u64) -> crate::pg::sql::Result<()> {
    crate::store::put_meta(db, key, last as i64)
}

/// A spot the overview lists: its message-id, article number and spotter id.
pub type Noted = (String, u64, Option<String>);

/// Notes the spots of one stretch of the overview, and that it was read up to `last`, together.
pub fn note_read(db: &dyn Sql, server: &str, last: u64, spots: &[Noted], now: i64) -> crate::pg::sql::Result<usize> {
    db.tx(|tx| {
        let mut added = 0;
        for (id, number, spotter) in spots {
            added += tx.execute(
                "insert into spots_seen (message_id, number, state, tries, at, spotter) values ($1, $2, 0, 0, $3, $4)
                 on conflict (message_id) do nothing",
                vals![id, *number as i64, now, spotter.as_deref()],
            )?;
        }
        write_cursor(tx, &cursor_key(server), last)?;
        Ok(added)
    })
}

/// A spot (or comment) removed with a `DISPOSE`, `DELETE` or `REMOVE` in `free.pt`: by a Spotnet moderator
/// (`spotter` empty), or by its own spotter (`spotter` their id), which holds only for that spotter's posts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Disposal {
    pub message_id: String,
    pub spotter: String,
    /// when the command was posted
    pub at: i64,
}

/// How long after a spot its spotter's own removal of it holds, as in Spotweb: five days.
pub const OWN_DISPOSAL_SECS: i64 = 5 * 86_400;

/// SQL for whether the row `s` (with its spot's `message_id`, and its spotter id in column `spotter`) was removed:
/// by a disposal, aliased `d`, or because its spotter is on the blacklist (`blacklist`). `created` is the column
/// with when the spot was made, when the row has one: a spotter's own removal holds only `OWN_DISPOSAL_SECS` after
/// it.
pub(crate) fn disposed_sql(d: &str, s: &str, spotter: &str, created: Option<&str>) -> String {
    disposed_sql_in("main.", d, s, spotter, created)
}

/// `disposed_sql` with the main database's tables named with `main` (`main.`, or empty on a query of the main
/// database alone, which runs on Postgres too).
fn disposed_sql_in(main: &str, d: &str, s: &str, spotter: &str, created: Option<&str>) -> String {
    // 0: the spot's XML had no date, so not known, as NULL
    let made = created.map_or(format!("{d}.at"), |c| format!("coalesce(nullif({s}.{c}, 0), {d}.at)"));
    format!(
        "(exists (select 1 from {main}spot_disposals {d} where {d}.message_id = {s}.message_id
            and ({d}.spotter = '' or ({d}.spotter = {s}.{spotter} and {d}.at - {made} <= {OWN_DISPOSAL_SECS})))
          or exists (select 1 from {main}spot_blacklist {d}_b where {d}_b.spotter = {s}.{spotter}))"
    )
}

/// Notes removals read in the overview. Kept whatever they name, now or later: a spot noted, saved or commented on
/// after its removal is still removed (`todo` and the native Spotnet reader). Returns how many were new.
pub fn note_disposals(db: &dyn Sql, disposals: &[Disposal]) -> crate::pg::sql::Result<usize> {
    db.tx(|tx| {
        let mut added = 0;
        for d in disposals {
            added += tx.execute(
                "insert into spot_disposals (message_id, spotter, at) values ($1, $2, $3)
                 on conflict (message_id, spotter) do nothing",
                vals![&d.message_id, &d.spotter, d.at],
            )?;
        }
        Ok(added)
    })
}

/// Spots to fetch. Half of `limit` goes to the oldest waiting, whoever posted them, so every spot gets its turn
/// however many new ones come in. The rest goes to each spotter's newest, a spotter at a time in turn
/// (`PER_SPOTTER` deep at most).
pub fn todo(db: &dyn Sql, limit: usize) -> crate::pg::sql::Result<Vec<String>> {
    let oldest: Vec<String> = db.column(
        &format!(
            "select message_id from spots_seen where state = $1 and not {} order by number, message_id limit $2",
            seen_disposed()
        ),
        vals![TODO, (limit / 2) as i64],
    )?;
    let newest: Vec<String> = db.column(
        &format!(
            "select message_id from (
                select message_id, number,
                    row_number() over (partition by coalesce(spotter, '') order by number desc) as turn
                from spots_seen where state = $1 and not {}
             ) t where turn <= $2 order by turn, number desc, message_id limit $3",
            seen_disposed()
        ),
        vals![TODO, PER_SPOTTER, limit as i64],
    )?;
    let mut out = oldest;
    for id in newest {
        if out.len() >= limit {
            break;
        }
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(out)
}

/// Spots waiting to be fetched.
pub fn waiting(db: &dyn Sql) -> crate::pg::sql::Result<i64> {
    db.one(&format!("select count(*) from spots_seen where state = $1 and not {}", seen_disposed()), vals![TODO])
}

/// Whether the `spots_seen` row was removed: a spot waiting has no `created`, so a spotter's own removal holds
/// whenever it came.
fn seen_disposed() -> String {
    disposed_sql_in("", "d", "spots_seen", "spotter", None)
}

/// Gives up on spots waiting past `TODO_DAYS` and on the oldest past `MAX_TODO`, and forgets the ones that came to
/// nothing past `KEEP_DAYS`, and removals of nothing kept. Saved ones stay. Returns the rows it changed.
pub fn prune(db: &dyn Sql, now: i64) -> crate::pg::sql::Result<usize> {
    let day = 86_400;
    let mut n = db.execute(
        "update spots_seen set state = $1 where state = $2 and at < $3",
        vals![GAVE_UP, TODO, now - TODO_DAYS * day],
    )?;
    n += db.execute(
        "update spots_seen set state = $1 where state = $2 and message_id not in
            (select message_id from spots_seen where state = $2 order by number desc, message_id limit $3)",
        vals![GAVE_UP, TODO, MAX_TODO],
    )?;
    n += db.execute(
        "delete from spots_seen where state not in ($1, $2) and at < $3",
        vals![TODO, SAVED, now - KEEP_DAYS * day],
    )?;
    // a removal is kept as long as what it removes may be: a spot waiting, or a saved spot or a comment (kept for
    // good)
    n += db.execute(
        "delete from spot_disposals where at < $1
            and message_id not in (select message_id from spots_seen where state in ($2, $3))
            and message_id not in (select message_id from spot_comments)",
        vals![now - KEEP_DAYS * day, SAVED, TODO],
    )?;
    Ok(n)
}

/// What came of fetching a spot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settled {
    /// `SAVED` or `SKIPPED`
    pub state: i64,
    pub release_id: Option<i64>,
    /// the spot's NZB articles (`release::ids_key`), and what its NZB holds (`release::nzb_key`), when read
    pub nzb_ref: Option<String>,
    pub nzb_key: Option<String>,
}

/// What came of fetching a spot, or one more try (given up after `MAX_TRIES`).
pub fn settle(db: &dyn Sql, message_id: &str, outcome: Option<&Settled>, now: i64) -> crate::pg::sql::Result<()> {
    match outcome {
        Some(s) => db.execute(
            "update spots_seen set state = $1, release_id = $2, nzb_ref = $3, nzb_key = $4, at = $5
             where message_id = $6",
            vals![s.state, s.release_id, s.nzb_ref.as_deref(), s.nzb_key.as_deref(), now, message_id],
        )?,
        None => db.execute(
            "update spots_seen set tries = tries + 1, at = $1,
                state = case when tries + 1 >= $2 then $3 else state end
             where message_id = $4",
            vals![now, MAX_TRIES, GAVE_UP, message_id],
        )?,
    };
    Ok(())
}

/// What an earlier spot pointing at the same NZB articles came to, `SAVED` (with its release) or `SKIPPED`: then
/// they needn't be read again.
pub fn known_ref(db: &dyn Sql, nzb_ref: &str) -> crate::pg::sql::Result<Option<(i64, Option<i64>)>> {
    db.row(
        "select state, release_id from spots_seen where nzb_ref = $1 and state in ($2, $3)
         order by state, message_id limit 1",
        vals![nzb_ref, SAVED, SKIPPED],
    )?
    .map(|r| Ok((r.get(0)?, r.get(1)?)))
    .transpose()
}

/// The release an earlier spot with the same NZB made. Kept here, in the main database, so a release that has
/// been frozen since (its shard row gone) is still found.
pub fn known_key(db: &dyn Sql, nzb_key: &str) -> crate::pg::sql::Result<Option<i64>> {
    db.opt(
        "select release_id from spots_seen where nzb_key = $1 and state = $2 and release_id is not null
         order by message_id limit 1",
        vals![nzb_key, SAVED],
    )
}

/// A spot as its release's row in `spots` keeps it.
#[derive(Clone, Debug)]
pub struct SpotRow {
    pub message_id: String,
    pub kind: Kind,
    pub spot: FullSpot,
    pub spotter_id: Option<String>,
    /// what the spot's NZB holds (`release::nzb_key`): an edited or reposted spot has the same one
    pub nzb_key: String,
    /// the spot's XML as its headers carry it (`full::xml_from_headers`)
    pub xml: String,
}

/// Where a spot's release can go, by its title and group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// no release has the name
    Free,
    /// the release with the name is this post: the spot's articles can join it
    SamePost(i64),
    /// another post has the name; folding into it would mix posts
    Taken,
}

/// What a release's spot says, for its page, its row and the API.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tagged {
    pub message_id: String,
    pub kind: String,
    pub title: String,
    pub poster: String,
    pub tag: String,
    pub spotter_id: Option<String>,
    pub imdb_id: Option<String>,
    pub subcats: String,
}

/// Whether a spot row has the spot's XML.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Xml {
    /// kept, and it reads
    Kept,
    /// saved before the XML was kept, and not read again yet (`missing_xml`)
    #[default]
    Pending,
    /// read again, and the article was gone or its XML wouldn't read
    Gone,
}

/// Everything a release's spot row keeps, for the release page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Detail {
    pub message_id: String,
    pub spotter_id: Option<String>,
    /// the spot's IMDb id, `tt` and its digits
    pub imdb_id: Option<String>,
    /// what the spot says: from its XML when that's `Kept`, else what the row's columns have (title, poster, tag,
    /// categories, size, when it was made)
    pub spot: FullSpot,
    pub xml: Xml,
    /// when the spot was removed, and by whom
    pub removed: Option<Removed>,
}

/// A spot's removal, for its release page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Removed {
    /// when the removal was posted; 0 for the blacklist, which says no when
    pub at: i64,
    pub by: RemovedBy,
}

/// Who took a spot back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemovedBy {
    Moderator,
    Spotter,
    /// its spotter is on the Spotnet blacklist
    Blacklist,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn both(body: impl Fn(&dyn Sql)) {
        crate::pg::testing::each(body);
    }

    #[test]
    fn reading_the_overview_moves_the_cursor_with_the_spots() {
        both(|conn| {
            assert_eq!(cursor(conn, "news.example").unwrap(), None);
            let spots = vec![("a@spot.net".to_string(), 10, None), ("b@spot.net".to_string(), 12, None)];
            assert_eq!(note_read(conn, "news.example", 20, &spots, 1).unwrap(), 2);
            // read again: nothing new, the cursor still moves
            assert_eq!(note_read(conn, "news.example", 25, &spots, 2).unwrap(), 0);
            assert_eq!(cursor(conn, "news.example").unwrap(), Some(25));
            assert_eq!(cursor(conn, "other").unwrap(), None);
            assert_eq!(todo(conn, 10).unwrap(), ["a@spot.net", "b@spot.net"]);
        });
    }

    #[test]
    fn settled_spots_leave_the_list_and_tries_run_out() {
        both(|conn| {
            let spots = vec![("a@spot.net".to_string(), 1, None), ("b@spot.net".to_string(), 2, None)];
            note_read(conn, "s", 2, &spots, 0).unwrap();
            settle(conn, "b@spot.net", Some(&Settled { state: SAVED, release_id: Some(7), ..Settled::default() }), 1)
                .unwrap();
            assert_eq!(todo(conn, 10).unwrap(), ["a@spot.net"]);
            for _ in 0..MAX_TRIES {
                assert_eq!(todo(conn, 10).unwrap(), ["a@spot.net"]);
                settle(conn, "a@spot.net", None, 1).unwrap();
            }
            assert!(todo(conn, 10).unwrap().is_empty());
            let state: i64 = conn.one("select state from spots_seen where message_id = 'a@spot.net'", vals![]).unwrap();
            assert_eq!(state, GAVE_UP);
        });
    }

    #[test]
    fn todo_takes_spotters_in_turn() {
        both(|conn| {
            // one spotter with 30 spots, newer than another's 2
            let mut spots: Vec<Noted> =
                (0..30).map(|n| (format!("flood{n}@spot.net"), 100 + n, Some("A".into()))).collect();
            spots.push(("b1@spot.net".into(), 1, Some("B".into())));
            spots.push(("b2@spot.net".into(), 2, Some("B".into())));
            note_read(conn, "s", 200, &spots, 0).unwrap();
            // half to the oldest, whoever posted them; the rest to spotters in turn, newest first
            let first = todo(conn, 4).unwrap();
            assert_eq!(first, ["b1@spot.net", "b2@spot.net", "flood29@spot.net", "flood28@spot.net"]);
            // flooded with new keys, the oldest still get half of every round
            let fresh: Vec<Noted> =
                (0..50).map(|n| (format!("key{n}@spot.net"), 1000 + n, Some(format!("K{n}")))).collect();
            note_read(conn, "s", 2000, &fresh, 0).unwrap();
            let round = todo(conn, 10).unwrap();
            assert_eq!(&round[..2], ["b1@spot.net", "b2@spot.net"]);
            assert!(round[2..5].iter().all(|id| id.starts_with("flood")), "{round:?}");
        });
    }

    #[test]
    fn prune_gives_up_on_stale_and_excess_spots_and_forgets_old_ones() {
        both(|conn| {
            let day = 86_400;
            let spots: Vec<Noted> = vec![("old@spot.net".into(), 1, None), ("new@spot.net".into(), 2, None)];
            note_read(conn, "s", 2, &spots, 0).unwrap();
            conn.execute("update spots_seen set at = $1 where message_id = 'new@spot.net'", vals![TODO_DAYS * day])
                .unwrap();
            assert_eq!(prune(conn, TODO_DAYS * day + 1).unwrap(), 1);
            assert_eq!(todo(conn, 10).unwrap(), ["new@spot.net"]);
            // past KEEP_DAYS, a spot that came to nothing goes and a saved one stays
            settle(conn, "new@spot.net", Some(&Settled { state: SAVED, ..Settled::default() }), 0).unwrap();
            prune(conn, KEEP_DAYS * day + 1).unwrap();
            let left: Vec<String> = conn.column("select message_id from spots_seen", vals![]).unwrap();
            assert_eq!(left, ["new@spot.net"]);
        });
    }

    #[test]
    fn nzbs_are_known_by_their_articles_and_their_contents() {
        both(|conn| {
            let spots: Vec<Noted> = vec![("a@spot.net".into(), 1, None), ("b@spot.net".into(), 2, None)];
            note_read(conn, "s", 2, &spots, 0).unwrap();
            assert_eq!(known_ref(conn, "r1").unwrap(), None);
            let saved =
                Settled { state: SAVED, release_id: Some(9), nzb_ref: Some("r1".into()), nzb_key: Some("k1".into()) };
            settle(conn, "a@spot.net", Some(&saved), 0).unwrap();
            let skipped = Settled { state: SKIPPED, nzb_ref: Some("r2".into()), ..Settled::default() };
            settle(conn, "b@spot.net", Some(&skipped), 0).unwrap();
            assert_eq!(known_ref(conn, "r1").unwrap(), Some((SAVED, Some(9))));
            assert_eq!(known_ref(conn, "r2").unwrap(), Some((SKIPPED, None)));
            assert_eq!(known_key(conn, "k1").unwrap(), Some(9));
            assert_eq!(known_key(conn, "k2").unwrap(), None);
        });
    }

    #[test]
    fn removed_and_blacklisted_spots_are_not_fetched_on_both_backends() {
        both(|conn| {
            let spots = vec![
                ("mod@spot.net".to_string(), 1, Some("aaa".to_string())),
                ("own@spot.net".to_string(), 2, Some("bbb".to_string())),
                ("forged@spot.net".to_string(), 3, Some("ccc".to_string())),
                ("kept@spot.net".to_string(), 4, Some("ddd".to_string())),
                ("listed@spot.net".to_string(), 5, Some("fff".to_string())),
            ];
            note_read(conn, "s", 5, &spots, 0).unwrap();
            let d = |id: &str, spotter: &str, at: i64| Disposal { message_id: id.into(), spotter: spotter.into(), at };
            let removals = [d("mod@spot.net", "", 10), d("own@spot.net", "bbb", 10), d("forged@spot.net", "bbb", 10)];
            assert_eq!(note_disposals(conn, &removals).unwrap(), 3);
            assert_eq!(note_disposals(conn, &removals).unwrap(), 0, "noted once");
            super::super::blacklist::replace(conn, &["fff".into()], 0).unwrap();
            assert_eq!(todo(conn, 10).unwrap(), ["forged@spot.net", "kept@spot.net"]);
            assert_eq!(waiting(conn).unwrap(), 2);
        });
    }
    #[test]
    fn native_cursor_distinguishes_absent_null_negative_zero_and_accounts() {
        both(|db| {
            assert_eq!(cursor(db, "a").unwrap(), None);
            db.execute("insert into meta(key,value) values('spots_cursor:a',null)", vals![]).unwrap();
            assert_eq!(cursor(db, "a").unwrap(), None);
            db.execute("update meta set value=-1 where key='spots_cursor:a'", vals![]).unwrap();
            assert_eq!(cursor(db, "a").unwrap(), None);
            write_cursor(db, &cursor_key("a"), 0).unwrap();
            assert_eq!(cursor(db, "a").unwrap(), Some(0));
            assert_eq!(cursor(db, "b").unwrap(), None);
        });
    }

    #[test]
    fn disposals_keep_saved_and_pending_spots_but_expire_discarded_ones() {
        both(|db| {
            let now = KEEP_DAYS * 86400 + 2;
            let notes = vec![
                ("saved@spot.net".into(), 1, None),
                ("discarded@spot.net".into(), 2, None),
                ("waiting@spot.net".into(), 3, None),
            ];
            note_read(db, "account", 3, &notes, now).unwrap();
            let removals = ["saved@spot.net", "discarded@spot.net", "waiting@spot.net", "unknown@spot.net"]
                .map(|id| Disposal { message_id: id.into(), spotter: String::new(), at: 1 });
            note_disposals(db, &removals).unwrap();
            settle(
                db,
                "saved@spot.net",
                Some(&Settled { state: SAVED, release_id: Some(7), ..Default::default() }),
                now,
            )
            .unwrap();
            settle(db, "discarded@spot.net", Some(&Settled { state: SKIPPED, ..Default::default() }), now).unwrap();
            prune(db, now).unwrap();
            assert_eq!(
                db.column::<String>("select message_id from spot_disposals order by message_id", vals![]).unwrap(),
                ["saved@spot.net", "waiting@spot.net"]
            );
            assert!(todo(db, 10).unwrap().is_empty());
        });
    }
}

#[cfg(all(test, unix))]
#[path = "store/native_tests.rs"]
mod native_tests;
