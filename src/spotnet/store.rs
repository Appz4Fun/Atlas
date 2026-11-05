//! Where spots are kept. The main database has the ingest's bookkeeping: each spot seen in `free.pt` that may be
//! worth fetching, and how far the overview has been read. Each shard has `spots`, the spot a release came from:
//! its tag, and what the spot says that a release row doesn't (IMDb id, spotter, Spotnet categories).

use rusqlite::{Connection, OptionalExtension, Result, params};

use crate::postprocess::schema::ShardTable;

use super::Kind;
use super::full::FullSpot;

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

/// The main database's tables, if missing.
pub fn create_main(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "create table if not exists spots_seen (
            message_id TEXT PRIMARY KEY,
            number INTEGER NOT NULL,
            state INTEGER NOT NULL DEFAULT 0,
            tries INTEGER NOT NULL DEFAULT 0,
            release_id INTEGER,
            at INTEGER NOT NULL
        ) without rowid;
        create index if not exists spots_seen_todo on spots_seen(state, number);",
    )
}

/// The spot a release came from, in its shard: made on open, gone with its release, carried by compaction and
/// freezing (`postprocess::schema::SHARD_TABLES`).
pub const SPOTS: ShardTable = ShardTable {
    name: "spots",
    create: "create table if not exists spots (
        release_id INTEGER PRIMARY KEY,
        message_id TEXT NOT NULL,
        kind TEXT NOT NULL,
        title TEXT NOT NULL,
        poster TEXT,
        tag TEXT,
        spotter_id TEXT,
        category INTEGER,
        subcats TEXT,
        size INTEGER,
        imdb_id TEXT,
        created INTEGER,
        nzb_key TEXT
    )",
    indexes: &[
        ("spots_message_id", "create index if not exists spots_message_id on spots(message_id)"),
        ("spots_nzb_key", "create index if not exists spots_nzb_key on spots(nzb_key)"),
    ],
    release_key: Some("release_id"),
    added: &[],
};

fn cursor_key(server: &str) -> String {
    format!("spots_cursor:{server}")
}

/// The last `free.pt` article read on `server`, by its numbers there.
pub fn cursor(conn: &Connection, server: &str) -> Result<Option<u64>> {
    let v: Option<i64> =
        conn.query_row("select value from meta where key = ?", [cursor_key(server)], |r| r.get(0)).optional()?;
    Ok(v.and_then(|v| u64::try_from(v).ok()))
}

/// Notes the spots of one stretch of the overview, and that it was read up to `last`, together.
pub fn note_read(conn: &mut Connection, server: &str, last: u64, spots: &[(String, u64)], now: i64) -> Result<usize> {
    let tx = conn.transaction()?;
    let mut added = 0;
    {
        let mut insert = tx.prepare_cached(
            "insert or ignore into spots_seen (message_id, number, state, tries, at) values (?, ?, 0, 0, ?)",
        )?;
        for (id, number) in spots {
            added += insert.execute(params![id, *number as i64, now])?;
        }
        tx.execute(
            "insert into meta (key, value) values (?, ?) on conflict(key) do update set value = excluded.value",
            params![cursor_key(server), last as i64],
        )?;
    }
    tx.commit()?;
    Ok(added)
}

/// Spots to fetch, newest first.
pub fn todo(conn: &Connection, limit: usize) -> Result<Vec<String>> {
    conn.prepare_cached("select message_id from spots_seen where state = ? order by number desc limit ?")?
        .query_map(params![TODO, limit as i64], |r| r.get(0))?
        .collect()
}

/// What came of fetching a spot: `state` with the release it made, or one more try (given up after `MAX_TRIES`).
pub fn settle(conn: &Connection, message_id: &str, outcome: Option<(i64, Option<i64>)>, now: i64) -> Result<()> {
    match outcome {
        Some((state, release_id)) => conn.execute(
            "update spots_seen set state = ?, release_id = ?, at = ? where message_id = ?",
            params![state, release_id, now, message_id],
        )?,
        None => conn.execute(
            "update spots_seen set tries = tries + 1, at = ?,
                state = case when tries + 1 >= ? then ? else state end
             where message_id = ?",
            params![now, MAX_TRIES, GAVE_UP, message_id],
        )?,
    };
    Ok(())
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
}

/// Tags the release named `name` in `group` with its spot, in its shard. Returns its id, or None when the save left
/// no such release (a blocked group, junk).
pub fn tag(conn: &Connection, name: &str, group: &str, row: &SpotRow) -> Result<Option<i64>> {
    let id: Option<i64> = conn
        .query_row("select id from releases where name = ? and group_name = ?", params![name, group], |r| r.get(0))
        .optional()?;
    let Some(id) = id else { return Ok(None) };
    put(conn, id, row)?;
    Ok(Some(id))
}

/// Tags the release an earlier spot with the same NZB made, when there is one, and returns its id: an edited spot
/// or a repost renames nothing and adds no second copy of the post.
pub fn tag_copy(conn: &Connection, row: &SpotRow) -> Result<Option<i64>> {
    let id: Option<i64> = conn
        .query_row(
            "select s.release_id from spots s join releases r on r.id = s.release_id where s.nzb_key = ? limit 1",
            [&row.nzb_key],
            |r| r.get(0),
        )
        .optional()?;
    let Some(id) = id else { return Ok(None) };
    put(conn, id, row)?;
    Ok(Some(id))
}

/// Whether the release named `name` in `group` came from a spot with another NZB: then a spot of the same title
/// and group is another upload, and must not be folded into it.
pub fn name_taken(conn: &Connection, name: &str, group: &str, nzb_key: &str) -> Result<bool> {
    let other: Option<String> = conn
        .query_row(
            "select s.nzb_key from releases r join spots s on s.release_id = r.id
             where r.name = ? and r.group_name = ? and s.nzb_key is not ?",
            params![name, group, nzb_key],
            |r| r.get(0),
        )
        .optional()?;
    Ok(other.is_some())
}

/// The release's spot row, unless it already has a newer spot (by when the spot was made): spots are fetched
/// newest first, so an older copy of a spot comes after the edit that replaced it.
fn put(conn: &Connection, release_id: i64, row: &SpotRow) -> Result<()> {
    let spot = &row.spot;
    conn.execute(
        "insert into spots (release_id, message_id, kind, title, poster, tag, spotter_id, category,
            subcats, size, imdb_id, created, nzb_key) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         on conflict(release_id) do update set message_id = excluded.message_id, kind = excluded.kind,
            title = excluded.title, poster = excluded.poster, tag = excluded.tag, spotter_id = excluded.spotter_id,
            category = excluded.category, subcats = excluded.subcats, size = excluded.size,
            imdb_id = excluded.imdb_id, created = excluded.created, nzb_key = excluded.nzb_key
         where coalesce(excluded.created, 0) >= coalesce(spots.created, 0)",
        params![
            release_id,
            row.message_id,
            row.kind.as_str(),
            spot.title,
            spot.poster,
            spot.tag,
            row.spotter_id,
            spot.category,
            spot.subcats.spotweb(),
            spot.size as i64,
            spot.imdb_id(),
            spot.created,
            row.nzb_key,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn main_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("create table meta (key TEXT PRIMARY KEY, value INTEGER)").unwrap();
        create_main(&conn).unwrap();
        create_main(&conn).unwrap();
        conn
    }

    #[test]
    fn reading_the_overview_moves_the_cursor_with_the_spots() {
        let mut conn = main_db();
        assert_eq!(cursor(&conn, "news.example").unwrap(), None);
        let spots = vec![("a@spot.net".to_string(), 10), ("b@spot.net".to_string(), 12)];
        assert_eq!(note_read(&mut conn, "news.example", 20, &spots, 1).unwrap(), 2);
        // read again: nothing new, the cursor still moves
        assert_eq!(note_read(&mut conn, "news.example", 25, &spots, 2).unwrap(), 0);
        assert_eq!(cursor(&conn, "news.example").unwrap(), Some(25));
        assert_eq!(cursor(&conn, "other").unwrap(), None);
        assert_eq!(todo(&conn, 10).unwrap(), ["b@spot.net", "a@spot.net"]);
    }

    #[test]
    fn settled_spots_leave_the_list_and_tries_run_out() {
        let mut conn = main_db();
        let spots = vec![("a@spot.net".to_string(), 1), ("b@spot.net".to_string(), 2)];
        note_read(&mut conn, "s", 2, &spots, 0).unwrap();
        settle(&conn, "b@spot.net", Some((SAVED, Some(7))), 1).unwrap();
        assert_eq!(todo(&conn, 10).unwrap(), ["a@spot.net"]);
        for _ in 0..MAX_TRIES {
            assert_eq!(todo(&conn, 10).unwrap(), ["a@spot.net"]);
            settle(&conn, "a@spot.net", None, 1).unwrap();
        }
        assert!(todo(&conn, 10).unwrap().is_empty());
        let state: i64 =
            conn.query_row("select state from spots_seen where message_id = 'a@spot.net'", [], |r| r.get(0)).unwrap();
        assert_eq!(state, GAVE_UP);
    }

    #[test]
    fn tags_the_release_by_name_and_group_and_copies_by_nzb() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("create table releases (id INTEGER PRIMARY KEY, name TEXT, group_name TEXT)").unwrap();
        conn.execute("insert into releases values (5, 'Movie (2026)', 'alt.binaries.x')", []).unwrap();
        conn.execute_batch(SPOTS.create).unwrap();
        let spot = FullSpot {
            title: "Movie (2026)".into(),
            website: "https://www.imdb.com/title/tt1234567/".into(),
            size: 9,
            ..FullSpot::default()
        };
        let row = SpotRow {
            message_id: "m@spot.net".into(),
            kind: Kind::Movie,
            spot: spot.clone(),
            spotter_id: Some("abc".into()),
            nzb_key: "k1".into(),
        };
        assert_eq!(tag_copy(&conn, &row).unwrap(), None);
        assert_eq!(tag(&conn, "Movie (2026)", "alt.binaries.y", &row).unwrap(), None);
        assert_eq!(tag(&conn, "Movie (2026)", "alt.binaries.x", &row).unwrap(), Some(5));
        // an edit of the spot, retitled, with the same NZB: the same release, the newer spot
        let edited = FullSpot { title: "Movie (2026) edited".into(), ..spot.clone() };
        let again = SpotRow { message_id: "m2@spot.net".into(), spot: edited, ..row.clone() };
        assert_eq!(tag_copy(&conn, &again).unwrap(), Some(5));
        let other = SpotRow { nzb_key: "k2".into(), ..again.clone() };
        assert_eq!(tag_copy(&conn, &other).unwrap(), None);
        let (id, title, imdb, n): (String, String, String, i64) = conn
            .query_row("select message_id, title, imdb_id, (select count(*) from spots) from spots", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap();
        assert_eq!(
            (id.as_str(), title.as_str(), imdb.as_str(), n),
            ("m2@spot.net", "Movie (2026) edited", "tt1234567", 1)
        );
        // a release that went (junk) leaves its spot row behind for the trigger, not for copies
        conn.execute("delete from releases where id = 5", []).unwrap();
        assert_eq!(tag_copy(&conn, &row).unwrap(), None);
    }

    #[test]
    fn an_older_copy_keeps_the_newer_spot_and_another_nzb_takes_the_name() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("create table releases (id INTEGER PRIMARY KEY, name TEXT, group_name TEXT)").unwrap();
        conn.execute("insert into releases values (5, 'Movie (2026)', 'g')", []).unwrap();
        conn.execute_batch(SPOTS.create).unwrap();
        let spot = |title: &str, created| FullSpot { title: title.into(), created, ..FullSpot::default() };
        let row = |id: &str, title, created| SpotRow {
            message_id: id.into(),
            kind: Kind::Movie,
            spot: spot(title, created),
            spotter_id: None,
            nzb_key: "k1".into(),
        };
        assert!(!name_taken(&conn, "Movie (2026)", "g", "k1").unwrap());
        assert_eq!(tag(&conn, "Movie (2026)", "g", &row("new@spot.net", "Edited", 200)).unwrap(), Some(5));
        assert_eq!(tag_copy(&conn, &row("old@spot.net", "Original", 100)).unwrap(), Some(5));
        let title: String = conn.query_row("select title from spots", [], |r| r.get(0)).unwrap();
        assert_eq!(title, "Edited");
        // the same title with another NZB is another upload; the same NZB isn't
        assert!(name_taken(&conn, "Movie (2026)", "g", "k2").unwrap());
        assert!(!name_taken(&conn, "Movie (2026)", "g", "k1").unwrap());
        assert!(!name_taken(&conn, "Movie (2026)", "other", "k2").unwrap());
    }
}
