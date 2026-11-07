//! Where spots are kept. The main database has the ingest's bookkeeping: each spot seen in `free.pt` that may be
//! worth fetching, and how far the overview has been read. Each shard has `spots`, the spot a release came from:
//! its tag, what the spot says that a release row doesn't (IMDb id, spotter, Spotnet categories), and the spot's
//! XML whole, for the release page (its description, website, cover image and newsreader).

use std::collections::{BTreeMap, HashMap};

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
/// Spots of one spotter fetched in a round of `todo` before the next spotter's: one key posting thousands can't
/// hold the others back.
pub const PER_SPOTTER: i64 = 10;
/// Spots waiting at most; past it the oldest are given up on. About two months of movie and TV spots.
pub const MAX_TODO: i64 = 20_000;
/// A spot still waiting after this long is given up on, and one that came to nothing is forgotten (its `free.pt`
/// article is long behind the cursor). A saved one stays: its comments hang off it (`comments`).
pub const TODO_DAYS: i64 = 30;
pub const KEEP_DAYS: i64 = 180;

/// The main database's tables, if missing.
pub fn create_main(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "create table if not exists spots_seen (
            message_id TEXT PRIMARY KEY,
            number INTEGER NOT NULL,
            state INTEGER NOT NULL DEFAULT 0,
            tries INTEGER NOT NULL DEFAULT 0,
            release_id INTEGER,
            at INTEGER NOT NULL,
            spotter TEXT,
            nzb_ref TEXT,
            nzb_key TEXT
        ) without rowid;
        create index if not exists spots_seen_todo on spots_seen(state, number);
        create index if not exists spots_seen_ref on spots_seen(nzb_ref) where nzb_ref is not null;
        create index if not exists spots_seen_key on spots_seen(nzb_key) where nzb_key is not null;
        create index if not exists spots_seen_release on spots_seen(release_id) where release_id is not null;",
    )?;
    super::comments::create(conn)
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
        nzb_key TEXT,
        xml TEXT
    )",
    indexes: &[
        ("spots_message_id", "create index if not exists spots_message_id on spots(message_id)"),
        ("spots_nzb_key", "create index if not exists spots_nzb_key on spots(nzb_key)"),
        // what `missing_xml` looks for, and once the backfill is done, nothing
        ("spots_xml_missing", "create index if not exists spots_xml_missing on spots(release_id) where xml is null"),
    ],
    release_key: Some("release_id"),
    // NULL: saved before the XML was kept, to be read again (`missing_xml`); empty: read again and gone
    added: &[("xml", "TEXT")],
};

fn cursor_key(server: &str) -> String {
    format!("spots_cursor:{server}")
}

/// The last `free.pt` article read on `server`, by its numbers there.
pub fn cursor(conn: &Connection, server: &str) -> Result<Option<u64>> {
    read_cursor(conn, &cursor_key(server))
}

/// A group's cursor in `meta`: the last article read, by its numbers on the server the key names.
pub(super) fn read_cursor(conn: &Connection, key: &str) -> Result<Option<u64>> {
    let v: Option<i64> = conn.query_row("select value from meta where key = ?", [key], |r| r.get(0)).optional()?;
    Ok(v.and_then(|v| u64::try_from(v).ok()))
}

pub(super) fn write_cursor(conn: &Connection, key: &str, last: u64) -> Result<()> {
    conn.execute(
        "insert into meta (key, value) values (?, ?) on conflict(key) do update set value = excluded.value",
        params![key, last as i64],
    )?;
    Ok(())
}

/// A spot the overview lists: its message-id, article number and spotter id.
pub type Noted = (String, u64, Option<String>);

/// Notes the spots of one stretch of the overview, and that it was read up to `last`, together.
pub fn note_read(conn: &mut Connection, server: &str, last: u64, spots: &[Noted], now: i64) -> Result<usize> {
    let tx = conn.transaction()?;
    let mut added = 0;
    {
        let mut insert = tx.prepare_cached(
            "insert or ignore into spots_seen (message_id, number, state, tries, at, spotter) values (?, ?, 0, 0, ?, ?)",
        )?;
        for (id, number, spotter) in spots {
            added += insert.execute(params![id, *number as i64, now, spotter])?;
        }
        write_cursor(&tx, &cursor_key(server), last)?;
    }
    tx.commit()?;
    Ok(added)
}

/// Spots to fetch. Half of `limit` goes to the oldest waiting, whoever posted them, so every spot gets its turn
/// however many new ones come in. The rest goes to each spotter's newest, a spotter at a time in turn
/// (`PER_SPOTTER` deep at most).
pub fn todo(conn: &Connection, limit: usize) -> Result<Vec<String>> {
    let oldest: Vec<String> = conn
        .prepare_cached("select message_id from spots_seen where state = ?1 order by number limit ?2")?
        .query_map(params![TODO, (limit / 2) as i64], |r| r.get(0))?
        .collect::<Result<_>>()?;
    let newest: Vec<String> = conn
        .prepare_cached(
            "select message_id from (
                select message_id, number,
                    row_number() over (partition by coalesce(spotter, '') order by number desc) as turn
                from spots_seen where state = ?1
             ) where turn <= ?2 order by turn, number desc limit ?3",
        )?
        .query_map(params![TODO, PER_SPOTTER, limit as i64], |r| r.get(0))?
        .collect::<Result<_>>()?;
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
pub fn waiting(conn: &Connection) -> Result<i64> {
    conn.query_row("select count(*) from spots_seen where state = ?", [TODO], |r| r.get(0))
}

/// Gives up on spots waiting past `TODO_DAYS` and on the oldest past `MAX_TODO`, and forgets the ones that came to
/// nothing past `KEEP_DAYS`. Saved ones stay. Returns the rows it changed.
pub fn prune(conn: &Connection, now: i64) -> Result<usize> {
    let day = 86_400;
    let mut n = conn.execute(
        "update spots_seen set state = ?1 where state = ?2 and at < ?3",
        params![GAVE_UP, TODO, now - TODO_DAYS * day],
    )?;
    n += conn.execute(
        "update spots_seen set state = ?1 where state = ?2 and message_id not in
            (select message_id from spots_seen where state = ?2 order by number desc limit ?3)",
        params![GAVE_UP, TODO, MAX_TODO],
    )?;
    n += conn.execute(
        "delete from spots_seen where state not in (?1, ?2) and at < ?3",
        params![TODO, SAVED, now - KEEP_DAYS * day],
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
pub fn settle(conn: &Connection, message_id: &str, outcome: Option<&Settled>, now: i64) -> Result<()> {
    match outcome {
        Some(s) => conn.execute(
            "update spots_seen set state = ?, release_id = ?, nzb_ref = ?, nzb_key = ?, at = ? where message_id = ?",
            params![s.state, s.release_id, s.nzb_ref, s.nzb_key, now, message_id],
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

/// What an earlier spot pointing at the same NZB articles came to, `SAVED` (with its release) or `SKIPPED`: then
/// they needn't be read again.
pub fn known_ref(conn: &Connection, nzb_ref: &str) -> Result<Option<(i64, Option<i64>)>> {
    conn.query_row(
        "select state, release_id from spots_seen where nzb_ref = ? and state in (?, ?) order by state limit 1",
        params![nzb_ref, SAVED, SKIPPED],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

/// The release an earlier spot with the same NZB made. Kept here, in the main database, so a release that has
/// been frozen since (its shard row gone) is still found.
pub fn known_key(conn: &Connection, nzb_key: &str) -> Result<Option<i64>> {
    conn.query_row(
        "select release_id from spots_seen where nzb_key = ? and state = ? and release_id is not null limit 1",
        params![nzb_key, SAVED],
        |r| r.get(0),
    )
    .optional()
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

/// Tags release `id` with this spot when it is live (newer spot wins, `put`). A frozen or deleted one is left as
/// it is. Returns `id` either way: the spot is that release's, and nothing is saved again.
pub fn tag_release(conn: &Connection, id: i64, row: &SpotRow) -> Result<i64> {
    let live: Option<i64> = conn.query_row("select id from releases where id = ?", [id], |r| r.get(0)).optional()?;
    if live.is_some() {
        put(conn, id, row)?;
    }
    Ok(id)
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

/// Whether a release, live or frozen, has the name `name` in `group`: only then does `placement` have work to do.
pub fn name_in_use(conn: &Connection, name: &str, group: &str) -> Result<bool> {
    let live = conn
        .prepare_cached("select 1 from releases where name = ? and group_name = ?")?
        .exists(params![name, group])?;
    if live {
        return Ok(true);
    }
    Ok(crate::frozen::attached(conn)?
        && conn
            .prepare_cached("select 1 from frozen.frozen_releases where name = ? and group_name = ?")?
            .exists(params![name, group])?)
}

/// Whether the release named `name` in `group`, if there is one, is the post a spot's NZB lists: tagged with the
/// same NZB, or already holding every article of it, each in the file of the same name. Then the spot adds nothing
/// and just tags it. Any other release with the name, spot or not, is another post: the spot can't add articles or
/// files to it, and gets a name of its own.
pub fn placement(
    conn: &Connection,
    name: &str,
    group: &str,
    nzb_key: &str,
    files: &[(String, Vec<String>)],
) -> Result<Placement> {
    let id: Option<i64> = conn
        .query_row("select id from releases where name = ? and group_name = ?", params![name, group], |r| r.get(0))
        .optional()?;
    let Some(id) = id else {
        // a frozen release with the name would be thawed by the save and the spot's articles folded into it
        // (`frozen::thaw_incoming`); its articles aren't to hand to compare, so it counts as another post
        let frozen = crate::frozen::attached(conn)?
            && conn
                .prepare_cached("select 1 from frozen.frozen_releases where name = ? and group_name = ?")?
                .exists(params![name, group])?;
        return Ok(if frozen { Placement::Taken } else { Placement::Free });
    };
    let same_nzb = conn
        .prepare_cached("select 1 from spots where release_id = ? and nzb_key = ?")?
        .exists(params![id, nzb_key])?;
    if same_nzb {
        return Ok(Placement::SamePost(id));
    }
    if files.is_empty() {
        return Ok(Placement::Taken);
    }
    // every message-id of the release, sealed files' too, by their keyed hash, with the file it's in
    let key = crate::store::IdKey::of(conn)?;
    crate::store::index_release(conn, &key, id)?;
    let mut file_of = conn.prepare_cached(
        "select f.filename from release_ids r join files f on f.id = r.file_id where r.release_id = ? and r.id_hash = ?",
    )?;
    for (filename, ids) in files {
        for message_id in ids {
            let found: Option<String> =
                file_of.query_row(params![id, key.hash(&format!("<{message_id}>"))], |r| r.get(0)).optional()?;
            if found.as_deref() != Some(filename.as_str()) {
                return Ok(Placement::Taken);
            }
        }
    }
    Ok(Placement::SamePost(id))
}

/// The release's spot row, unless it already has a newer spot (by when the spot was made): spots are fetched
/// newest first, so an older copy of a spot comes after the edit that replaced it.
fn put(conn: &Connection, release_id: i64, row: &SpotRow) -> Result<()> {
    let spot = &row.spot;
    let changed = conn.execute(
        "insert into spots (release_id, message_id, kind, title, poster, tag, spotter_id, category,
            subcats, size, imdb_id, created, nzb_key, xml) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         on conflict(release_id) do update set message_id = excluded.message_id, kind = excluded.kind,
            title = excluded.title, poster = excluded.poster, tag = excluded.tag, spotter_id = excluded.spotter_id,
            category = excluded.category, subcats = excluded.subcats, size = excluded.size,
            imdb_id = excluded.imdb_id, created = excluded.created, nzb_key = excluded.nzb_key, xml = excluded.xml
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
            row.xml,
        ],
    )?;
    // the spot's kind is a hint for the release's category (`spotnet::category`): the step looks again
    if changed > 0 {
        recategorize(conn, release_id)?;
    }
    Ok(())
}

/// Queues release `id` for the category step again, on its shard's writer.
fn recategorize(conn: &Connection, id: i64) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    crate::postprocess::rerun_on(conn, id, crate::postprocess::step::mask(crate::category::step::STEP), now)
}

/// Once per shard: queues every release saved from a spot before the spot's kind was a category hint for the
/// category step again. Returns how many.
pub fn recategorize_all(conn: &Connection) -> Result<usize> {
    const DONE: &str = "spot_category_hint";
    if crate::store::get_meta(conn, DONE)?.is_some() {
        return Ok(0);
    }
    let ids: Vec<i64> =
        conn.prepare("select release_id from spots")?.query_map([], |r| r.get(0))?.collect::<Result<_>>()?;
    for id in &ids {
        recategorize(conn, *id)?;
    }
    crate::store::set_meta(conn, DONE, 1)?;
    Ok(ids.len())
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

/// The spots of these releases, by release id, from each one's shard.
pub fn spots_of(ids: &[i64]) -> Result<HashMap<i64, Tagged>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    crate::db::with_reader(|conn| spots_of_with(conn, ids))
}

pub fn spots_of_with(conn: &Connection, ids: &[i64]) -> Result<HashMap<i64, Tagged>> {
    let mut by_shard: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for id in ids {
        by_shard.entry(crate::store::shard_of_id(*id)).or_default().push(id.to_string());
    }
    let mut out = HashMap::new();
    for (shard, list) in by_shard {
        let sql = format!(
            "select release_id, message_id, kind, title, coalesce(poster, ''), coalesce(tag, ''), spotter_id, imdb_id,
                coalesce(subcats, '') from s{shard}.spots where release_id in ({})",
            list.join(",")
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                Tagged {
                    message_id: r.get(1)?,
                    kind: r.get(2)?,
                    title: r.get(3)?,
                    poster: r.get(4)?,
                    tag: r.get(5)?,
                    spotter_id: r.get(6)?,
                    imdb_id: r.get(7)?,
                    subcats: r.get(8)?,
                },
            ))
        })?;
        for row in rows {
            let (id, tagged) = row?;
            out.insert(id, tagged);
        }
    }
    Ok(out)
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
}

/// Release `id`'s spot, from its shard, when it has one.
pub fn detail_of(id: i64) -> Result<Option<Detail>> {
    crate::db::with_reader(|conn| detail_of_with(conn, id))
}

pub fn detail_of_with(conn: &Connection, id: i64) -> Result<Option<Detail>> {
    let sql = format!(
        "select message_id, spotter_id, imdb_id, title, coalesce(poster, ''), coalesce(tag, ''), category,
            coalesce(subcats, ''), size, created, xml
         from s{}.spots where release_id = ?",
        crate::store::shard_of_id(id)
    );
    let row = conn
        .query_row(&sql, [id], |r| {
            let columns = FullSpot {
                title: r.get(3)?,
                poster: r.get(4)?,
                tag: r.get(5)?,
                category: r.get::<_, Option<i32>>(6)?.unwrap_or(0),
                subcats: super::header::subcats(&r.get::<_, String>(7)?),
                size: r.get::<_, Option<i64>>(8)?.and_then(|n| u64::try_from(n).ok()).unwrap_or(0),
                created: r.get::<_, Option<i64>>(9)?.unwrap_or(0),
                ..FullSpot::default()
            };
            let parts: (String, Option<String>, Option<String>) = (r.get(0)?, r.get(1)?, r.get(2)?);
            Ok((parts, columns, r.get::<_, Option<String>>(10)?))
        })
        .optional()?;
    Ok(row.map(|((message_id, spotter_id, imdb_id), columns, xml)| {
        let parsed = xml.as_deref().filter(|x| !x.is_empty()).and_then(super::full::parse);
        let state = match (&parsed, &xml) {
            (Some(_), _) => Xml::Kept,
            (None, None) => Xml::Pending,
            (None, Some(_)) => Xml::Gone,
        };
        let imdb_id = parsed.as_ref().map_or(imdb_id, FullSpot::imdb_id);
        Detail { message_id, spotter_id, imdb_id, spot: parsed.unwrap_or(columns), xml: state }
    }))
}

/// Spots of this shard saved before their XML was kept: (release id, spot message-id), at most `limit`, in no
/// order, so one whose article can't be read for now doesn't hold the rest back pass after pass.
pub fn missing_xml(conn: &Connection, limit: usize) -> Result<Vec<(i64, String)>> {
    conn.prepare_cached("select release_id, message_id from spots where xml is null order by random() limit ?")?
        .query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect()
}

/// The XML read again for a spot `missing_xml` listed, empty when the article is gone. Kept only while the row is
/// still that spot's: a newer spot that came meanwhile brought its own.
pub fn set_xml(conn: &Connection, release_id: i64, message_id: &str, xml: &str) -> Result<bool> {
    let n = conn.execute(
        "update spots set xml = ? where release_id = ? and message_id = ? and xml is null",
        params![xml, release_id, message_id],
    )?;
    Ok(n > 0)
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
        let spots = vec![("a@spot.net".to_string(), 10, None), ("b@spot.net".to_string(), 12, None)];
        assert_eq!(note_read(&mut conn, "news.example", 20, &spots, 1).unwrap(), 2);
        // read again: nothing new, the cursor still moves
        assert_eq!(note_read(&mut conn, "news.example", 25, &spots, 2).unwrap(), 0);
        assert_eq!(cursor(&conn, "news.example").unwrap(), Some(25));
        assert_eq!(cursor(&conn, "other").unwrap(), None);
        assert_eq!(todo(&conn, 10).unwrap(), ["a@spot.net", "b@spot.net"]);
    }

    #[test]
    fn settled_spots_leave_the_list_and_tries_run_out() {
        let mut conn = main_db();
        let spots = vec![("a@spot.net".to_string(), 1, None), ("b@spot.net".to_string(), 2, None)];
        note_read(&mut conn, "s", 2, &spots, 0).unwrap();
        settle(&conn, "b@spot.net", Some(&Settled { state: SAVED, release_id: Some(7), ..Settled::default() }), 1)
            .unwrap();
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

    /// A shard's tables: tagging a release touches its row and the stage's queue (`recategorize`).
    fn tagging_shard(conn: &Connection) {
        conn.execute_batch("create table releases (id INTEGER PRIMARY KEY, name TEXT, group_name TEXT)").unwrap();
        for table in [&crate::postprocess::schema::PP_STATE, &crate::postprocess::schema::PP_QUEUE] {
            conn.execute_batch(table.create).unwrap();
            for (column, kind) in table.added {
                conn.execute_batch(&format!("alter table {} add column {column} {kind}", table.name)).unwrap();
            }
        }
    }

    fn queued(conn: &Connection) -> Vec<i64> {
        conn.prepare("select release_id from pp_queue order by release_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_>>()
            .unwrap()
    }

    #[test]
    fn tags_the_release_by_name_and_group_and_copies_by_nzb() {
        let conn = Connection::open_in_memory().unwrap();
        tagging_shard(&conn);
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
            xml: String::new(),
        };
        assert_eq!(tag_copy(&conn, &row).unwrap(), None);
        assert_eq!(tag(&conn, "Movie (2026)", "alt.binaries.y", &row).unwrap(), None);
        assert_eq!(tag(&conn, "Movie (2026)", "alt.binaries.x", &row).unwrap(), Some(5));
        assert_eq!(queued(&conn), [5], "the category step looks again, with the spot's kind");
        conn.execute("delete from pp_queue", []).unwrap();
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
        assert_eq!(queued(&conn), [5]);
        conn.execute("delete from releases where id = 5", []).unwrap();
        assert_eq!(tag_copy(&conn, &row).unwrap(), None);
    }

    #[test]
    fn spot_releases_are_queued_for_their_category_once() {
        let conn = Connection::open_in_memory().unwrap();
        tagging_shard(&conn);
        conn.execute_batch(SPOTS.create).unwrap();
        conn.execute_batch("create table meta (key TEXT PRIMARY KEY, value INTEGER)").unwrap();
        conn.execute("insert into releases values (8, 'A', 'g'), (16, 'B', 'g'), (24, 'C', 'g')", []).unwrap();
        conn.execute("insert into spots (release_id, message_id, kind, title) values (8, 'a', 'movie', 'A'), (24, 'c', 'series', 'C')", []).unwrap();
        assert_eq!(recategorize_all(&conn).unwrap(), 2);
        assert_eq!(queued(&conn), [8, 24]);
        conn.execute("delete from pp_queue", []).unwrap();
        assert_eq!(recategorize_all(&conn).unwrap(), 0, "once");
        assert!(queued(&conn).is_empty());
    }

    #[test]
    fn an_older_copy_keeps_the_newer_spot() {
        let conn = Connection::open_in_memory().unwrap();
        tagging_shard(&conn);
        conn.execute("insert into releases values (5, 'Movie (2026)', 'g')", []).unwrap();
        conn.execute_batch(SPOTS.create).unwrap();
        let spot = |title: &str, created| FullSpot { title: title.into(), created, ..FullSpot::default() };
        let row = |id: &str, title, created| SpotRow {
            message_id: id.into(),
            kind: Kind::Movie,
            spot: spot(title, created),
            spotter_id: None,
            nzb_key: "k1".into(),
            xml: String::new(),
        };
        assert_eq!(tag(&conn, "Movie (2026)", "g", &row("new@spot.net", "Edited", 200)).unwrap(), Some(5));
        assert_eq!(tag_copy(&conn, &row("old@spot.net", "Original", 100)).unwrap(), Some(5));
        let title: String = conn.query_row("select title from spots", [], |r| r.get(0)).unwrap();
        assert_eq!(title, "Edited");
    }

    #[test]
    fn todo_takes_spotters_in_turn() {
        let mut conn = main_db();
        // one spotter with 30 spots, newer than another's 2
        let mut spots: Vec<Noted> =
            (0..30).map(|n| (format!("flood{n}@spot.net"), 100 + n, Some("A".into()))).collect();
        spots.push(("b1@spot.net".into(), 1, Some("B".into())));
        spots.push(("b2@spot.net".into(), 2, Some("B".into())));
        note_read(&mut conn, "s", 200, &spots, 0).unwrap();
        // half to the oldest, whoever posted them; the rest to spotters in turn, newest first
        let first = todo(&conn, 4).unwrap();
        assert_eq!(first, ["b1@spot.net", "b2@spot.net", "flood29@spot.net", "flood28@spot.net"]);
        // flooded with new keys, the oldest still get half of every round
        let fresh: Vec<Noted> =
            (0..50).map(|n| (format!("key{n}@spot.net"), 1000 + n, Some(format!("K{n}")))).collect();
        note_read(&mut conn, "s", 2000, &fresh, 0).unwrap();
        let round = todo(&conn, 10).unwrap();
        assert_eq!(&round[..2], ["b1@spot.net", "b2@spot.net"]);
        assert!(round[2..5].iter().all(|id| id.starts_with("flood")), "{round:?}");
    }

    #[test]
    fn prune_gives_up_on_stale_and_excess_spots_and_forgets_old_ones() {
        let mut conn = main_db();
        let day = 86_400;
        let spots: Vec<Noted> = vec![("old@spot.net".into(), 1, None), ("new@spot.net".into(), 2, None)];
        note_read(&mut conn, "s", 2, &spots, 0).unwrap();
        conn.execute("update spots_seen set at = ? where message_id = 'new@spot.net'", [TODO_DAYS * day]).unwrap();
        assert_eq!(prune(&conn, TODO_DAYS * day + 1).unwrap(), 1);
        assert_eq!(todo(&conn, 10).unwrap(), ["new@spot.net"]);
        // past KEEP_DAYS, a spot that came to nothing goes and a saved one stays
        settle(&conn, "new@spot.net", Some(&Settled { state: SAVED, ..Settled::default() }), 0).unwrap();
        prune(&conn, KEEP_DAYS * day + 1).unwrap();
        let left: Vec<String> = conn
            .prepare("select message_id from spots_seen")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(left, ["new@spot.net"]);
    }

    #[test]
    fn nzbs_are_known_by_their_articles_and_their_contents() {
        let mut conn = main_db();
        let spots: Vec<Noted> = vec![("a@spot.net".into(), 1, None), ("b@spot.net".into(), 2, None)];
        note_read(&mut conn, "s", 2, &spots, 0).unwrap();
        assert_eq!(known_ref(&conn, "r1").unwrap(), None);
        let saved =
            Settled { state: SAVED, release_id: Some(9), nzb_ref: Some("r1".into()), nzb_key: Some("k1".into()) };
        settle(&conn, "a@spot.net", Some(&saved), 0).unwrap();
        let skipped = Settled { state: SKIPPED, nzb_ref: Some("r2".into()), ..Settled::default() };
        settle(&conn, "b@spot.net", Some(&skipped), 0).unwrap();
        assert_eq!(known_ref(&conn, "r1").unwrap(), Some((SAVED, Some(9))));
        assert_eq!(known_ref(&conn, "r2").unwrap(), Some((SKIPPED, None)));
        assert_eq!(known_key(&conn, "k1").unwrap(), Some(9));
        assert_eq!(known_key(&conn, "k2").unwrap(), None);
    }

    #[test]
    fn a_frozen_release_keeps_its_row_when_a_copy_comes() {
        let conn = Connection::open_in_memory().unwrap();
        tagging_shard(&conn);
        conn.execute_batch(SPOTS.create).unwrap();
        let row = SpotRow {
            message_id: "m@spot.net".into(),
            kind: Kind::Movie,
            spot: FullSpot { title: "T".into(), ..FullSpot::default() },
            spotter_id: None,
            nzb_key: "k".into(),
            xml: String::new(),
        };
        // not live (frozen): nothing written, the id still answered
        assert_eq!(tag_release(&conn, 7, &row).unwrap(), 7);
        let n: i64 = conn.query_row("select count(*) from spots", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        conn.execute("insert into releases values (7, 'T', 'g')", []).unwrap();
        assert_eq!(tag_release(&conn, 7, &row).unwrap(), 7);
        let n: i64 = conn.query_row("select count(*) from spots", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn a_spot_joins_a_release_only_when_it_is_the_same_post() {
        let path = tempfile::tempdir().unwrap();
        let shard = path.path().join("s.db");
        crate::store::create_shard(&shard).unwrap();
        let mut conn = crate::db::open_shard(&shard).unwrap();
        let mut writer = crate::store::ShardWriter::new(0);
        let ids = crate::store::Ids::new(&path.path().join("main.db"));
        let main = Connection::open(path.path().join("main.db")).unwrap();
        crate::store::create_main(&main).unwrap();
        let article = |id: &str, file: &str| crate::parser::Article {
            message_id: format!("<{id}>"),
            filename: Some(file.into()),
            release_name: Some("Title".into()),
            part: Some(1),
            total_parts: Some(1),
            ..Default::default()
        };
        let release = crate::parser::Release {
            name: "Title".into(),
            group: "alt.binaries.g".into(),
            articles: vec![article("a@x", "a.mkv"), article("b@x", "b.par2")],
            ..Default::default()
        };
        writer.save(&mut conn, &ids, [std::slice::from_ref(&release)]).unwrap();
        let files = |l: &[(&str, &str)]| -> Vec<(String, Vec<String>)> {
            l.iter().map(|(f, id)| (f.to_string(), vec![id.to_string()])).collect()
        };
        let place = |l: &[(&str, &str)]| placement(&conn, "Title", "alt.binaries.g", "k", &files(l)).unwrap();
        let other = placement(&conn, "Other", "alt.binaries.g", "k", &files(&[("a.mkv", "a@x")])).unwrap();
        assert_eq!(other, Placement::Free);
        let Placement::SamePost(id) = place(&[("a.mkv", "a@x"), ("b.par2", "b@x")]) else { panic!("the same post") };
        // an article it doesn't have, a real article under another file's name, or nothing to tell by
        assert_eq!(place(&[("a.mkv", "a@x"), ("evil.mkv", "evil@x")]), Placement::Taken);
        assert_eq!(place(&[("setup.exe", "a@x")]), Placement::Taken);
        assert_eq!(place(&[]), Placement::Taken);
        // tagged with the same NZB: the same post
        let tag_it =
            "insert into spots (release_id, message_id, kind, title, nzb_key) values (?, 'm', 'movie', 'T', 'k')";
        conn.execute(tag_it, [id]).unwrap();
        assert_eq!(place(&[]), Placement::SamePost(id));
    }

    #[test]
    fn spots_of_reads_each_shard() {
        let conn = Connection::open_in_memory().unwrap();
        let (a, b) = (crate::store::global_id(1, 0), crate::store::global_id(1, 3));
        for shard in [0, 3] {
            conn.execute(&format!("attach ':memory:' as s{shard}"), []).unwrap();
            conn.execute_batch(&SPOTS.create.replace("exists spots", &format!("exists s{shard}.spots"))).unwrap();
        }
        conn.execute("insert into s0.spots (release_id, message_id, kind, title, poster) values (?, 'a@spot.net', 'movie', 'A', 'p')", [a]).unwrap();
        conn.execute("insert into s3.spots (release_id, message_id, kind, title, imdb_id) values (?, 'b@spot.net', 'series', 'B', 'tt1')", [b]).unwrap();
        let found = spots_of_with(&conn, &[a, b, crate::store::global_id(2, 0)]).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[&a].poster, "p");
        assert_eq!(found[&b].imdb_id.as_deref(), Some("tt1"));
        assert_eq!(found[&b].kind, "series");
    }

    #[test]
    fn a_frozen_release_with_the_title_counts_as_another_post() {
        let conn = Connection::open_in_memory().unwrap();
        tagging_shard(&conn);
        conn.execute_batch(SPOTS.create).unwrap();
        let files = vec![("a.mkv".to_string(), vec!["a@x".to_string()])];
        assert_eq!(placement(&conn, "Title", "g", "k", &files).unwrap(), Placement::Free);
        conn.execute("attach ':memory:' as frozen", []).unwrap();
        conn.execute_batch("create table frozen.frozen_releases (id INTEGER PRIMARY KEY, name TEXT, group_name TEXT)")
            .unwrap();
        assert_eq!(placement(&conn, "Title", "g", "k", &files).unwrap(), Placement::Free);
        conn.execute("insert into frozen.frozen_releases values (3, 'Title', 'g')", []).unwrap();
        assert_eq!(placement(&conn, "Title", "g", "k", &files).unwrap(), Placement::Taken);
        assert_eq!(placement(&conn, "Title", "other", "k", &files).unwrap(), Placement::Free);
    }

    #[test]
    fn a_name_is_in_use_live_or_frozen() {
        let conn = Connection::open_in_memory().unwrap();
        tagging_shard(&conn);
        conn.execute("insert into releases values (1, 'Live', 'g')", []).unwrap();
        assert!(name_in_use(&conn, "Live", "g").unwrap());
        assert!(!name_in_use(&conn, "Frozen", "g").unwrap());
        conn.execute("attach ':memory:' as frozen", []).unwrap();
        conn.execute_batch("create table frozen.frozen_releases (id INTEGER PRIMARY KEY, name TEXT, group_name TEXT)")
            .unwrap();
        conn.execute("insert into frozen.frozen_releases values (2, 'Frozen', 'g')", []).unwrap();
        assert!(name_in_use(&conn, "Frozen", "g").unwrap());
        assert!(!name_in_use(&conn, "Frozen", "other").unwrap());
    }
    #[test]
    fn the_page_reads_the_spot_whole_or_from_its_columns() {
        let conn = Connection::open_in_memory().unwrap();
        let id = crate::store::global_id(4, 2);
        conn.execute("attach ':memory:' as s2", []).unwrap();
        conn.execute_batch(&SPOTS.create.replace("exists spots", "exists s2.spots")).unwrap();
        assert_eq!(detail_of_with(&conn, id).unwrap(), None);
        // saved before the XML was kept: what the columns say
        conn.execute(
            "insert into s2.spots (release_id, message_id, kind, title, poster, category, subcats, size, imdb_id, created)
             values (?, 'm@spot.net', 'movie', 'Old', 'p', 0, 'a9|d5|z0|', 42, 'tt1234567', 100)",
            [id],
        )
        .unwrap();
        let d = detail_of_with(&conn, id).unwrap().unwrap();
        assert_eq!(d.xml, Xml::Pending);
        assert_eq!(d.imdb_id.as_deref(), Some("tt1234567"));
        assert_eq!(d.spot.website, "", "the row keeps the IMDb id, not the website");
        assert_eq!((d.spot.title.as_str(), d.spot.size, d.spot.created), ("Old", 42, 100));
        assert_eq!(d.spot.subcats.spotweb(), "a9|d5|z0|");
        assert_eq!(d.spot.description, "");
        // read again: the XML wins, description and all
        let shard = Connection::open_in_memory().unwrap();
        shard.execute_batch(SPOTS.create).unwrap();
        shard
            .execute(
                "insert into spots (release_id, message_id, kind, title) values (?, 'm@spot.net', 'movie', 'Old')",
                [id],
            )
            .unwrap();
        assert_eq!(missing_xml(&shard, 10).unwrap(), [(id, "m@spot.net".to_string())]);
        let xml = "<Spotnet><Posting><Title>New</Title><Description>Line one[br]pw: hunter2</Description></Posting><Extra><Newsreader>SpotPost 2</Newsreader></Extra></Spotnet>";
        // another spot's XML doesn't land on this row
        assert!(!set_xml(&shard, id, "other@spot.net", xml).unwrap());
        assert!(set_xml(&shard, id, "m@spot.net", xml).unwrap());
        assert!(!set_xml(&shard, id, "m@spot.net", "").unwrap(), "only once");
        assert!(missing_xml(&shard, 10).unwrap().is_empty());
        shard.execute("attach ':memory:' as s2", []).unwrap();
        shard.execute_batch(&SPOTS.create.replace("exists spots", "exists s2.spots")).unwrap();
        shard.execute("insert into s2.spots select * from main.spots", []).unwrap();
        let d = detail_of_with(&shard, id).unwrap().unwrap();
        assert_eq!(d.xml, Xml::Kept);
        assert_eq!(d.imdb_id, None, "the XML has no IMDb link");
        assert_eq!(d.spot.title, "New");
        assert_eq!(d.spot.description, "Line one\npw: hunter2");
        assert_eq!(d.spot.newsreader, "SpotPost 2");
        // read again and gone: the columns, not tried again
        shard.execute("update s2.spots set xml = ''", []).unwrap();
        assert_eq!(detail_of_with(&shard, id).unwrap().unwrap().xml, Xml::Gone);
    }
}
