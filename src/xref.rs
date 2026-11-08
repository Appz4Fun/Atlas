//! `atlas.xref.db`: the headers of uploads obfuscated per article, kept for a day, and linked back into uploads
//! across groups.
//!
//! Some posters give every article its own random subject, with no part counter and no file name, soo no subject rule
//! reads them (`parser::Unparsable`) and they used to be dropped. What stays the same is the poster: every article of
//! one file carries the same random From, in every group the file was spread over. And the files of one upload go up
//! back to back to the same groups, a one article PAR2 index first. This keeps those articles' headers for
//! `KEEP_SECS` after they arrive, groups them into files by poster and the files into uploads by time and groups, and
//! saves each upload as one release.
//!
//! Nothing here reads a body. Which article starts a file isn't in its headers, soo a file's parts are numbered in the
//! order they were posted, and a downloader puts each where its yEnc header says.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use chrono::DateTime;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::parser::{Article, Release, Shape, Unparsable, is_complete, parse_subject, unparsable_shape};

/// how long a header is kept after it arrives: the window an upload's files are linked in
pub const KEEP_SECS: i64 = 24 * 3600;
/// a file no article has come for in this long is taken as whole once every group has been read for the days it was
/// posted on (see `Store::pass_with`), and its upload as done once all of its are
pub const SETTLE_SECS: i64 = 15 * 60;
/// the most seconds between one file of an upload and the next
pub const GAP_SECS: i64 = 5;
/// an upload of fewer articles is noise: a random poster for each article makes uploads of one
pub const MIN_UPLOAD_ARTICLES: i64 = 10;
/// how long a connection waits for another's write: the recorder, the linker and the archive step share the file
const BUSY: Duration = Duration::from_secs(120);
/// SQLite's wait for another connection's write, up to `BUSY`, given up at once when the indexer stops: a slice
/// waiting to record its headers then fails and is read again later (its cursor didn't move), rather than holding
/// the stop up.
fn wait_unless_stopping(tries: i32) -> bool {
    const STEP: Duration = Duration::from_millis(10);
    if crate::frozen::STOPPING.load(std::sync::atomic::Ordering::Relaxed) || STEP * tries.max(0) as u32 > BUSY {
        return false;
    }
    std::thread::sleep(STEP);
    true
}

/// rows one write transaction takes at most, soo no writer holds the file for long
const WRITE_CHUNK: usize = 2000;
/// how often the linker runs
const PASS_EVERY: Duration = Duration::from_secs(60);
/// what the name of every upload saved here starts with (`is_upload`)
pub const NAME_PREFIX: &str = "xref-upload-";
/// a volume's articles the archive step tries as its first piece: the first a group got of it, in each group. Piece
/// order isn't in the headers, but the first piece goes up first, soo it is most likely the lowest numbered in its group
pub const LEADS: usize = 6;
/// the most first-piece guesses the archive step makes for one upload, over its volumes in turn
pub const HEAD_PROBES: usize = 40;

const SCHEMA: &str = "
create table if not exists groups (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
-- one row per article, whichever server's listing brought it first. `posted` is the poster's wall clock in seconds
-- (`dates::naive_timestamp`), `added` when it arrived here, `done` once its file is in a release
create table if not exists headers (
    message_id TEXT PRIMARY KEY,
    poster     TEXT NOT NULL,
    posted     INTEGER NOT NULL,
    grp        INTEGER NOT NULL,
    number     INTEGER NOT NULL,
    bytes      INTEGER NOT NULL,
    lines      INTEGER NOT NULL,
    added      INTEGER NOT NULL,
    done       INTEGER NOT NULL DEFAULT 0
) without rowid;
create index if not exists headers_poster on headers(poster);
create index if not exists headers_waiting on headers(poster) where done = 0;
create index if not exists headers_added on headers(added);
-- the file a poster's articles went to, soo ones that come later join it
create table if not exists linked (
    poster  TEXT PRIMARY KEY,
    release TEXT NOT NULL,
    grp     TEXT NOT NULL,
    file    INTEGER NOT NULL,
    files   INTEGER NOT NULL,
    parts   INTEGER NOT NULL,
    -- the parts that were the first a group got of the file in their batch, comma separated (`leads`)
    leads   TEXT NOT NULL DEFAULT '',
    -- when the oldest of its saved articles came: while that's within the window, what comes again is told apart
    oldest  INTEGER NOT NULL DEFAULT 0,
    added   INTEGER NOT NULL
) without rowid;
create index if not exists linked_added on linked(added);
create index if not exists linked_release on linked(release, file);
-- each upload saved: its release, the groups its articles have been read in so far, when its files were posted, and
-- how many it has, soo files read later in other groups join it; `probed` how many rounds of first-piece guesses the
-- archive step has tried (`tried`)
create table if not exists uploads (
    release TEXT PRIMARY KEY,
    probed  INTEGER NOT NULL DEFAULT 0,
    -- bumped each time the upload grows, which starts its guesses over: a run that began before can't note progress
    gen     INTEGER NOT NULL DEFAULT 0,
    grp     TEXT NOT NULL,
    groups  TEXT NOT NULL,
    first   INTEGER NOT NULL,
    last    INTEGER NOT NULL,
    files   INTEGER NOT NULL,
    added   INTEGER NOT NULL
) without rowid;
create index if not exists uploads_added on uploads(added);
-- `started`: when the store began keeping headers (`Store::open`), which the prune leaves no trace of in `headers`
create table if not exists meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL) without rowid;
";

/// `atlas.xref.db` beside the main database
pub fn path_for(main: &Path) -> PathBuf {
    main.with_file_name("atlas.xref.db")
}

/// What a poster's articles waiting in the store add up to: one file of an upload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSum {
    pub poster: String,
    /// its articles
    pub n: i64,
    /// when its first and last article were posted
    pub first: i64,
    pub last: i64,
    /// when its newest article arrived
    pub added: i64,
    pub groups: BTreeSet<i64>,
}

/// An upload saved before, that files read since in other groups can join.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seed {
    /// the groups its articles were read in
    pub groups: BTreeSet<i64>,
    pub first: i64,
    pub last: i64,
}

struct Upload {
    /// the groups its files go to; None while it has only its PAR2 index
    groups: Option<BTreeSet<i64>>,
    /// the one article file it started with, whose groups are some of the upload's
    index_groups: BTreeSet<i64>,
    /// the saved upload it goes on, and the groups that had been read for it then
    seed: Option<(usize, BTreeSet<i64>)>,
    files: Vec<usize>,
    first: i64,
    last: i64,
}

/// The files of `files` that make one upload each, in the order they went up, and the upload of `seeds` each goes on.
///
/// A one article file is a PAR2 index and starts an upload; it sorts before the files posted in the same second. Any
/// other file goes to an upload it started within, from `GAP_SECS` before its first file to as long after its last:
/// first one posted to the same groups (or one only an index has started, whose groups it covers), the most recently
/// active; else, as a small file can't reach every group its upload went to, one whose groups take in its own, the
/// fewest of them first; else a saved upload none of its groups had been read for, the closest: groups are read apart,
/// and a file read in one only after its upload was saved from another shares none with it.
pub fn chain(files: &[FileSum], seeds: &[Seed]) -> Vec<(Option<usize>, Vec<usize>)> {
    chain_until(files, seeds, &|| false).expect("never stopped")
}

/// `chain`, None once `stop` says so (asked every few thousand files).
fn chain_until(files: &[FileSum], seeds: &[Seed], stop: &dyn Fn() -> bool) -> Option<Vec<(Option<usize>, Vec<usize>)>> {
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by(|&a, &b| {
        let (x, y) = (&files[a], &files[b]);
        (x.first, x.n != 1, x.last, &x.poster).cmp(&(y.first, y.n != 1, y.last, &y.poster))
    });

    let mut uploads: Vec<Upload> = seeds
        .iter()
        .enumerate()
        .map(|(j, s)| Upload {
            groups: Some(s.groups.clone()),
            index_groups: BTreeSet::new(),
            seed: Some((j, s.groups.clone())),
            files: Vec::new(),
            first: s.first,
            last: s.last,
        })
        .collect();
    // the uploads a file can still go to, soo a pass over a day of headers isn't quadratic: seeds come in as their
    // time comes, and an upload leaves once files start past its last
    let mut by_start: Vec<usize> = (0..uploads.len()).collect();
    by_start.sort_by_key(|&j| std::cmp::Reverse(uploads[j].first));
    let mut active: Vec<usize> = Vec::new();
    for (k, i) in order.into_iter().enumerate() {
        if k % 4096 == 0 && stop() {
            return None;
        }
        let f = &files[i];
        while by_start.last().is_some_and(|&j| uploads[j].first - GAP_SECS <= f.first) {
            active.extend(by_start.pop());
        }
        active.retain(|&j| uploads[j].last + GAP_SECS >= f.first);
        let near = |u: &Upload| f.first >= u.first - GAP_SECS && f.first <= u.last + GAP_SECS;
        let pick = if f.n == 1 {
            None
        } else {
            let candidates = || active.iter().map(|&j| (j, &uploads[j])).filter(|(_, u)| near(u));
            let same = candidates()
                .filter(|(_, u)| match &u.groups {
                    Some(g) => *g == f.groups,
                    None => u.index_groups.is_subset(&f.groups),
                })
                // the most recently active, and an upload only an index has started over one it would follow on
                .max_by_key(|(_, u)| (u.last, u.groups.is_none()))
                .map(|(j, _)| j);
            let within = || {
                candidates()
                    .filter_map(|(j, u)| {
                        u.groups.as_ref().filter(|g| f.groups.is_subset(g)).map(|g| (j, g.len(), u.last))
                    })
                    .min_by_key(|&(_, len, last)| (len, std::cmp::Reverse(last)))
                    .map(|(j, _, _)| j)
            };
            let read_since = || {
                candidates()
                    .filter(|(_, u)| u.seed.as_ref().is_some_and(|(_, read)| read.is_disjoint(&f.groups)))
                    .min_by_key(|(_, u)| (f.first - u.last).abs())
                    .map(|(j, _)| j)
            };
            same.or_else(within).or_else(read_since)
        };
        match pick {
            Some(j) => {
                let u = &mut uploads[j];
                match &mut u.groups {
                    None => u.groups = Some(f.groups.clone()),
                    Some(g) if u.seed.is_some() => g.extend(f.groups.iter().copied()),
                    Some(_) => {}
                }
                u.files.push(i);
                u.first = u.first.min(f.first);
                u.last = u.last.max(f.last);
            }
            None => {
                active.push(uploads.len());
                uploads.push(Upload {
                    groups: (f.n > 1).then(|| f.groups.clone()),
                    index_groups: f.groups.clone(),
                    seed: None,
                    files: vec![i],
                    first: f.first,
                    last: f.last,
                });
            }
        }
    }
    Some(uploads.into_iter().filter(|u| !u.files.is_empty()).map(|u| (u.seed.map(|(j, _)| j), u.files)).collect())
}

/// Stable, in decimal: a run of hex would read as a random name, and leave search (as `filters::per_article_name`).
fn upload_name(poster: &str) -> String {
    let key = format!("xref\0{poster}");
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3));
    format!("{NAME_PREFIX}{}-{}", hash >> 32, hash & 0xffff_ffff)
}

/// The archive step's connection: its notes are best effort, soo it waits briefly rather than hold up a run
fn open_rw() -> Option<Connection> {
    let conn = Connection::open(path_for(&crate::paths::database())).ok()?;
    conn.busy_timeout(Duration::from_secs(5)).ok()?;
    Some(conn)
}

/// How many of the upload `release`'s first-piece guesses the archive step has tried (`uploads.probed`), in the order
/// `archive::steps::xref_order` gives them, and the generation of its guesses that count is of. None when not known
/// (pruned, or in tests): the first ones, once.
pub fn tried(release: &str) -> Option<(i64, i64)> {
    if cfg!(test) {
        return None;
    }
    open_rw()?
        .query_row("select probed, gen from uploads where release = ?1", [release], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .ok()?
}

/// The first `count` guesses of generation `generation` of the upload `release` were tried: a later run takes the ones after.
/// Nothing when the upload has grown since, and its guesses started over.
pub fn tried_through(release: &str, generation: i64, count: i64) {
    if cfg!(test) {
        return;
    }
    if let Some(conn) = open_rw() {
        let _ = conn.execute(
            "update uploads set probed = max(probed, ?3) where release = ?1 and gen = ?2",
            params![release, generation, count],
        );
    }
}

/// The parts of file `file` of the upload `release` that were the first a group got of it, in the order they were
/// saved: the likely first pieces. Empty when not known (pruned, or in tests).
pub fn leads(release: &str, file: i64) -> Vec<i64> {
    if cfg!(test) {
        return Vec::new();
    }
    let Ok(conn) = Connection::open_with_flags(
        path_for(&crate::paths::database()),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return Vec::new();
    };
    let leads: Option<String> = conn
        .query_row("select leads from linked where release = ?1 and file = ?2 limit 1", params![release, file], |r| {
            r.get(0)
        })
        .optional()
        .ok()
        .flatten();
    leads.unwrap_or_default().split(',').filter_map(|p| p.parse().ok()).collect()
}

/// The place of a file named for it (`add_file`): `<release>-<file>`.
pub fn file_of(filename: &str) -> Option<i64> {
    filename.rsplit_once('-')?.1.parse().ok()
}

/// Whether `name` is that of an upload this linked, whose files are named for their place and whose parts are in a
/// guessed order.
pub fn is_upload(name: &str) -> bool {
    name.starts_with(NAME_PREFIX)
}

/// `YYYY-MM-DD HH:MM:SS` of a wall clock in seconds, what `dates::to_iso_date` gives
fn iso(posted: i64) -> String {
    DateTime::from_timestamp(posted, 0).map_or_else(String::new, |d| d.format("%Y-%m-%d %H:%M:%S").to_string())
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// One article as the store keeps it.
struct Row {
    message_id: String,
    added: i64,
    /// the first its group got of the file, of the rows taken together
    lead: bool,
    posted: i64,
    grp: i64,
    number: i64,
    bytes: i64,
    lines: i64,
}

/// Where a file goes: its release, that release's group, its place among the upload's files, and the parts it has.
struct Place {
    /// the parts that were a group's first, comma separated (`leads`)
    leads: String,
    /// when the oldest of its saved articles came
    oldest: i64,
    release: String,
    group: String,
    file: i64,
    files: i64,
    parts: i64,
}

/// What a pass found: its releases to save, then `Store::ack`.
pub struct Found {
    pub releases: Vec<Release>,
    pub ack: Ack,
}

/// Which of a poster's waiting articles a pass took: these, or every one that had come by then (`added` at most this),
/// for a file let go whole, whose articles aren't read one by one.
#[derive(Clone, Debug)]
pub enum Taken {
    Ids(Vec<String>),
    /// every waiting article that came by this time, as long as they are still the count the pass saw: an article
    /// that took long to commit is left to the next pass rather than let go unseen
    UpTo(i64, i64),
}

/// What a pass took as done, noted by `Store::ack` once its releases are saved.
pub struct Ack {
    finished: Vec<(String, Option<Place>, Taken)>,
    widened: Vec<(String, String, Seed, i64)>,
    now: i64,
}

/// An upload saved before.
struct Saved {
    release: String,
    group: String,
    seed: Seed,
    files: i64,
}

pub struct Store {
    conn: Connection,
    groups: HashMap<String, i64>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        let conn = Connection::open(path)?;
        conn.busy_handler(Some(wait_unless_stopping))?;
        // a WAL reset leaves its file at most this big
        conn.execute_batch(
            "pragma journal_mode = wal; pragma synchronous = normal; pragma journal_size_limit = 268435456;",
        )?;
        conn.execute_batch(SCHEMA)?;
        // once: a store made by a build from before this kept no start, soo its file's birth or oldest header says
        if conn.query_row("select 1 from meta where key = 'started'", [], |_| Ok(())).optional()?.is_none() {
            let started = started_by_its_file(&conn, path)?.unwrap_or_else(now);
            conn.execute("insert or ignore into meta (key, value) values ('started', ?1)", [started])?;
        }
        let groups = {
            let mut stmt = conn.prepare("select name, id from groups")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
        };
        Ok(Store { conn, groups })
    }

    fn group_id(&mut self, name: &str) -> Result<i64> {
        if let Some(id) = self.groups.get(name) {
            return Ok(*id);
        }
        self.conn.execute("insert or ignore into groups (name) values (?1)", [name])?;
        let id = self.conn.query_row("select id from groups where name = ?1", [name], |r| r.get(0))?;
        self.groups.insert(name.to_string(), id);
        Ok(id)
    }

    /// The name of group `id`, from the database when another connection (the recorder) added it since.
    fn group_name(&mut self, id: i64) -> Result<String> {
        if let Some((name, _)) = self.groups.iter().find(|(_, v)| **v == id) {
            return Ok(name.clone());
        }
        let name: String = self.conn.query_row("select name from groups where id = ?1", [id], |r| r.get(0))?;
        self.groups.insert(name.clone(), id);
        Ok(name)
    }

    /// Keep the headers of `articles` of `group`, unless another server's listing brought them already. In
    /// transactions of `WRITE_CHUNK` rows, soo the linker's writes go between them. An error part way leaves the
    /// chunks before it kept, and the slice is read again; should the linker settle those first (only after
    /// `SETTLE_SECS`), the rest come as late articles or files of their upload.
    pub fn record(&mut self, group: &str, articles: &[Article], added: i64) -> Result<usize> {
        let grp = self.group_id(group)?;
        let mut n = 0;
        for chunk in articles.chunks(WRITE_CHUNK) {
            let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            {
                let mut insert = tx.prepare_cached(
                    "insert or ignore into headers (message_id, poster, posted, grp, number, bytes, lines, added)
                     values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )?;
                for a in chunk {
                    let Some(posted) = crate::dates::naive_timestamp(&a.date) else { continue };
                    n += insert.execute(params![
                        a.message_id,
                        a.author,
                        posted,
                        grp,
                        a.number as i64,
                        a.bytes,
                        a.lines,
                        added
                    ])?;
                }
            }
            tx.commit()?;
        }
        Ok(n)
    }

    /// The WAL written back into the file and started over, between passes. Writers commit all the time, soo the
    /// passive checkpoints at each commit never get to the end of it while a pass reads: on a busy run the WAL
    /// grew past the file many times over (hundreds of GB in hours), and every write and read through it slowed
    /// with it. Here no read is open on this connection; a write that comes meanwhile waits on the busy handler.
    /// Returns the WAL's frames and those written back.
    pub fn checkpoint(&mut self) -> Result<(i64, i64)> {
        Ok(self.conn.query_row("pragma wal_checkpoint(truncate)", [], |r| Ok((r.get(1)?, r.get(2)?)))?)
    }

    /// Drop what arrived over `KEEP_SECS` ago.
    pub fn prune(&mut self, now: i64) -> Result<usize> {
        let before = now - KEEP_SECS;
        // what waits for a group to be read is let go at the latest a day after it would settle (see `pass_with`)
        // a cursor over (added, message_id), soo the rows that stay (waiting for a group) are passed once, not again
        // for every batch
        let mut n = 0;
        let mut after: (i64, String) = (i64::MIN, String::new());
        loop {
            let batch: Vec<(i64, String)> = self
                .conn
                .prepare_cached(&format!(
                    "select added, message_id from headers
                     where added < ?1 and (done = 1 or added < ?2) and (added, message_id) > (?3, ?4)
                     order by added, message_id limit {WRITE_CHUNK}"
                ))?
                .query_map(params![before, before - KEEP_SECS, after.0, after.1], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let Some(last) = batch.last().cloned() else { break };
            let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            {
                let mut delete = tx.prepare_cached("delete from headers where message_id = ?1")?;
                for (_, id) in &batch {
                    n += delete.execute([id])?;
                }
            }
            tx.commit()?;
            // the rest waits for the next pass once the indexer stops
            if batch.len() < WRITE_CHUNK || crate::frozen::STOPPING.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            after = last;
        }
        // a file's place outlasts the window too: its leads are what the archive step guesses from
        self.conn.execute("delete from linked where added < ?1", [before - 7 * KEEP_SECS])?;
        // an upload outlasts the linking window by a week: the archive step may still be taking its rounds
        self.conn.execute("delete from uploads where added < ?1", [before - 7 * KEEP_SECS])?;
        Ok(n)
    }

    #[cfg(test)]
    fn place_of(&self, poster: &str) -> Result<Option<Place>> {
        Ok(self
            .conn
            .query_row(
                "select release, grp, file, files, parts, leads, oldest from linked where poster = ?1",
                [poster],
                |r| {
                    Ok(Place {
                        release: r.get(0)?,
                        group: r.get(1)?,
                        file: r.get(2)?,
                        files: r.get(3)?,
                        parts: r.get(4)?,
                        leads: r.get(5)?,
                        oldest: r.get(6)?,
                    })
                },
            )
            .optional()?)
    }

    #[cfg(test)]
    fn waiting(&self) -> Result<Vec<FileSum>> {
        Ok(self.waiting_placed()?.into_iter().map(|(f, _)| f).collect())
    }

    /// Each poster's articles waiting, and its file's place when it has one: one read for the pass, rather than one
    /// a poster (most are one-article noise).
    fn waiting_placed(&self) -> Result<Vec<(FileSum, Option<Place>)>> {
        let mut stmt = self.conn.prepare(
            "select w.poster, w.n, w.first, w.last, w.added, w.groups,
                    l.release, l.grp, l.file, l.files, l.parts, l.leads, l.oldest
             from (select poster, count(*) n, min(posted) first, max(posted) last, max(added) added,
                          group_concat(distinct grp) groups
                   from headers where done = 0 group by poster) w
             left join linked l on l.poster = w.poster",
        )?;
        let rows = stmt.query_map([], |r| {
            let groups: String = r.get(5)?;
            let release: Option<String> = r.get(6)?;
            let place = match release {
                Some(release) => Some(Place {
                    release,
                    group: r.get(7)?,
                    file: r.get(8)?,
                    files: r.get(9)?,
                    parts: r.get(10)?,
                    leads: r.get(11)?,
                    oldest: r.get(12)?,
                }),
                None => None,
            };
            Ok((
                FileSum {
                    poster: r.get(0)?,
                    n: r.get(1)?,
                    first: r.get(2)?,
                    last: r.get(3)?,
                    added: r.get(4)?,
                    groups: groups.split(',').filter_map(|g| g.parse().ok()).collect(),
                },
                place,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The uploads saved within `KEEP_SECS` of `now`.
    fn saved(&self, now: i64) -> Result<Vec<Saved>> {
        let mut stmt = self.conn.prepare_cached(
            "select release, grp, groups, first, last, files from uploads where added >= ?1 order by first, release",
        )?;
        let rows = stmt.query_map([now - KEEP_SECS], |r| {
            let groups: String = r.get(2)?;
            Ok(Saved {
                release: r.get(0)?,
                group: r.get(1)?,
                seed: Seed {
                    groups: groups.split(',').filter_map(|g| g.parse().ok()).collect(),
                    first: r.get(3)?,
                    last: r.get(4)?,
                },
                files: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn rows_of(&self, poster: &str) -> Result<Vec<Row>> {
        let mut stmt = self.conn.prepare_cached(
            "select message_id, posted, grp, number, bytes, lines,
                    row_number() over (partition by grp order by number, message_id) as rank, added
             from headers where poster = ?1 and done = 0
             order by added, grp, number, message_id",
        )?;
        let rows = stmt.query_map([poster], |r| {
            Ok(Row {
                message_id: r.get(0)?,
                posted: r.get(1)?,
                grp: r.get(2)?,
                number: r.get(3)?,
                bytes: r.get(4)?,
                lines: r.get(5)?,
                lead: r.get::<_, i64>(6)? == 1,
                added: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The uploads that have settled by `now`, each as a release, and the articles that came late for a file already
    /// in one, as that release. Their headers are marked done, and each file's place kept for what comes later.
    pub fn pass(&mut self, now: i64) -> Result<Vec<Release>> {
        let found = self.pass_with(now, &mut |_, _| false, &|| false)?;
        self.ack(&found.ack)?;
        Ok(found.releases)
    }

    /// `pass`, a file settled only once `unread(first, last)` says no group is still to be read for the time it was
    /// posted in, or once it has waited `KEEP_SECS`: an upload saved before every group was read for it would be
    /// split, as a group read later brings files of it that no article read before linked.
    ///
    /// `stop` is asked as it goes: once it says so, the pass gives up what it found (nothing is written until `ack`)
    /// and comes back with nothing, soo the indexer's stop doesn't wait on it.
    pub fn pass_with(
        &mut self,
        now: i64,
        unread: &mut dyn FnMut(i64, i64) -> bool,
        stop: &dyn Fn() -> bool,
    ) -> Result<Found> {
        let nothing = || Found { releases: Vec::new(), ack: Ack { finished: Vec::new(), widened: Vec::new(), now } };
        let mut waits: HashMap<String, bool> = HashMap::new();
        let mut fresh = Vec::new();
        let mut late = Vec::new();
        let waiting = self.waiting_placed()?;
        if stop() {
            return Ok(nothing());
        }
        for (k, (f, place)) in waiting.into_iter().enumerate() {
            if k % 4096 == 0 && stop() {
                return Ok(nothing());
            }
            let quiet = f.added < now - SETTLE_SECS;
            let settled = f.added < now - KEEP_SECS || (quiet && !unread(f.first, f.last));
            waits.insert(f.poster.clone(), !settled);
            match place {
                Some(place) => late.push((f, place)),
                None => fresh.push(f),
            }
        }
        let settled = |f: &FileSum| !waits.get(&f.poster).copied().unwrap_or(true);

        let mut out: HashMap<String, Release> = HashMap::new();
        // each poster done with: where its file went, and the articles of it this pass took
        let mut finished: Vec<(String, Option<Place>, Taken)> = Vec::new();

        for (k, (f, place)) in late.into_iter().enumerate() {
            if k % 256 == 0 && stop() {
                return Ok(nothing());
            }
            if settled(&f) && place.oldest < now - KEEP_SECS {
                // the file's first articles have left the window: one read again couldnt be told from a new one, and
                // would be numbered as another part. Let go
                let taken = Taken::UpTo(f.added, f.n);
                finished.push((f.poster, None, taken));
            } else if settled(&f) {
                let rows = self.rows_of(&f.poster)?;
                let parts = place.parts + rows.len() as i64;
                let ids = Taken::Ids(rows.iter().map(|r| r.message_id.clone()).collect());
                // a group that led the file before offers a new first piece only in an article it numbers lower than
                // all it gave before: one another server's listing brought, say
                let earliest: HashMap<i64, i64> = {
                    let mut stmt = self.conn.prepare_cached(
                        "select grp, min(number) from headers where poster = ?1 and done = 1 group by grp",
                    )?;
                    stmt.query_map([&f.poster], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
                };
                let rows: Vec<Row> = rows
                    .into_iter()
                    .map(|r| Row { lead: r.lead && earliest.get(&r.grp).is_none_or(|&n| r.number < n), ..r })
                    .collect();
                let leads = with_leads(&place.leads, self.add_file(&mut out, &place, &f.poster, rows, parts));
                finished.push((f.poster, Some(Place { parts, leads, ..place }), ids));
            }
        }

        // the groups and posting times late articles came with widen their upload, before this pass's new files are
        // matched to it
        let mut saved = self.saved(now)?;
        let mut grown: BTreeSet<usize> = BTreeSet::new();
        for (poster, place, _) in &finished {
            let Some(place) = place else { continue };
            let Some(j) = saved.iter().position(|u| u.release == place.release) else { continue };
            let mut stmt = self
                .conn
                .prepare_cached("select grp, min(posted), max(posted) from headers where poster = ?1 group by grp")?;
            for row in
                stmt.query_map([poster], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))?
            {
                let (g, first, last) = row?;
                let seed = &mut saved[j].seed;
                seed.groups.insert(g);
                seed.first = seed.first.min(first);
                seed.last = seed.last.max(last);
            }
            grown.insert(j);
        }
        let seeds: Vec<Seed> = saved.iter().map(|u| u.seed.clone()).collect();
        let mut widened: Vec<(String, String, Seed, i64)> = Vec::new();
        let Some(chains) = chain_until(&fresh, &seeds, stop) else { return Ok(nothing()) };
        for (k, (seed, files)) in chains.into_iter().enumerate() {
            if k % 4096 == 0 && stop() {
                return Ok(nothing());
            }
            if !files.iter().all(|&i| settled(&fresh[i])) {
                continue;
            }
            let articles: i64 = files.iter().map(|&i| fresh[i].n).sum();
            if seed.is_none() && articles < MIN_UPLOAD_ARTICLES {
                // noise, let go whole: no need to read its articles
                for &i in &files {
                    finished.push((fresh[i].poster.clone(), None, Taken::UpTo(fresh[i].added, fresh[i].n)));
                }
                continue;
            }
            let mut by_group: HashMap<i64, usize> = HashMap::new();
            let mut rows_by_file = Vec::with_capacity(files.len());
            for &i in &files {
                let rows = self.rows_of(&fresh[i].poster)?;
                for r in &rows {
                    *by_group.entry(r.grp).or_default() += 1;
                }
                rows_by_file.push(rows);
            }
            let mut span = Seed {
                groups: by_group.keys().copied().collect(),
                first: files.iter().map(|&i| fresh[i].first).min().unwrap_or(0),
                last: files.iter().map(|&i| fresh[i].last).max().unwrap_or(0),
            };
            let (release, group, before) = match seed {
                Some(j) => {
                    let u = &saved[j];
                    span.groups.extend(u.seed.groups.iter().copied());
                    span.first = span.first.min(u.seed.first);
                    span.last = span.last.max(u.seed.last);
                    (u.release.clone(), u.group.clone(), u.files)
                }
                None => {
                    // the release lives in the group most of its articles were posted to
                    let home = by_group.iter().max_by_key(|&(g, n)| (*n, std::cmp::Reverse(*g))).map_or(0, |(g, _)| *g);
                    (upload_name(&fresh[files[0]].poster), self.group_name(home)?, 0)
                }
            };
            let count = before + files.len() as i64;
            for (k, (&i, rows)) in files.iter().zip(rows_by_file).enumerate() {
                let parts = rows.len() as i64;
                let place = Place {
                    release: release.clone(),
                    group: group.clone(),
                    file: before + k as i64 + 1,
                    files: count,
                    parts: 0,
                    leads: String::new(),
                    oldest: rows.iter().map(|r| r.added).min().unwrap_or(now),
                };
                let ids = Taken::Ids(rows.iter().map(|r| r.message_id.clone()).collect());
                let leads = with_leads("", self.add_file(&mut out, &place, &fresh[i].poster, rows, parts));
                finished.push((fresh[i].poster.clone(), Some(Place { parts, leads, ..place }), ids));
            }
            widened.push((release, group, span, count));
        }
        // uploads late articles grew and no file of this pass joined
        for j in grown {
            let u = &saved[j];
            if !widened.iter().any(|(r, _, _, _)| *r == u.release) {
                widened.push((u.release.clone(), u.group.clone(), u.seed.clone(), u.files));
            }
        }

        let mut releases: Vec<Release> = out.into_values().collect();
        for r in &mut releases {
            r.complete = is_complete(&r.articles);
            if let Some(first) = r.articles.first() {
                r.poster = first.author.clone();
                r.date = first.date.clone();
            }
        }
        Ok(Found { releases, ack: Ack { finished, widened, now } })
    }

    /// Note what `ack`'s pass took as done, once its releases are saved: until then the next pass finds them again.
    /// Articles that came for a file since aren't in it, soo they wait for the next.
    pub fn ack(&mut self, ack: &Ack) -> Result<()> {
        let Ack { finished, widened, now } = ack;
        if finished.is_empty() && widened.is_empty() {
            return Ok(());
        }
        // the one write worth finishing on a stop: its releases are saved already. It waits as long as it takes
        self.conn.busy_timeout(BUSY)?;
        let noted = self.note(finished, widened, *now);
        self.conn.busy_handler(Some(wait_unless_stopping))?;
        noted
    }

    fn note(
        &mut self,
        finished: &[(String, Option<Place>, Taken)],
        widened: &[(String, String, Seed, i64)],
        now: i64,
    ) -> Result<()> {
        // one transaction, whatever its size: a note half made would leave an upload's last files to be found again
        // as an upload of their own. It is key updates only, quick next to the recorder's chunks
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        {
            let mut done = tx.prepare_cached("update headers set done = 1 where message_id = ?1")?;
            let mut keep = tx.prepare_cached(
                "insert into linked (poster, release, grp, file, files, parts, leads, oldest, added)
                 values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 on conflict(poster) do update set parts = excluded.parts, leads = excluded.leads, added = excluded.added",
            )?;
            let mut done_upto = tx.prepare_cached(
                "update headers set done = 1 where poster = ?1 and done = 0 and added <= ?2
                 and (select count(*) from headers where poster = ?1 and done = 0 and added <= ?2) = ?3",
            )?;
            for (poster, place, taken) in finished {
                match taken {
                    Taken::Ids(ids) => {
                        for id in ids {
                            done.execute([id])?;
                        }
                    }
                    Taken::UpTo(upto, n) => {
                        done_upto.execute(params![poster, upto, n])?;
                    }
                }
                if let Some(p) = place {
                    keep.execute(params![
                        poster, p.release, p.group, p.file, p.files, p.parts, p.leads, p.oldest, now
                    ])?;
                }
            }
            let mut upload = tx.prepare_cached(
                "insert into uploads (release, grp, groups, first, last, files, added)
                 values (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 on conflict(release) do update set groups = excluded.groups, first = excluded.first,
                     last = excluded.last, files = excluded.files, added = excluded.added, probed = 0, gen = gen + 1",
            )?;
            for (release, group, span, files) in widened {
                let groups = span.groups.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
                upload.execute(params![release, group, groups, span.first, span.last, files, now])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// `rows` of one poster as a file of `place`'s release, parts numbered on from the ones it has, of `parts` in all.
    /// The parts that were a group's first of them.
    fn add_file(
        &self,
        out: &mut HashMap<String, Release>,
        place: &Place,
        poster: &str,
        rows: Vec<Row>,
        parts: i64,
    ) -> Vec<i64> {
        let mut leads = Vec::new();
        let key = format!("{}\0{}", place.release, place.group);
        let release = out.entry(key).or_insert_with(|| Release {
            name: place.release.clone(),
            is_obfuscated: true,
            group: place.group.clone(),
            collection: Some(format!("xref\0{}", place.release)),
            ..Release::default()
        });
        // no count of files in it: files read later in other groups join the upload
        let filename = format!("{}-{:05}", place.release, place.file);
        for (k, row) in rows.into_iter().enumerate() {
            let part = place.parts + k as i64 + 1;
            if row.lead {
                leads.push(part);
            }
            release.size += row.bytes.max(0);
            release.articles.push(Article {
                number: row.number.max(0) as u64,
                subject: format!("[{}/{}] - \"{filename}\" yEnc ({part}/{parts})", place.file, place.files),
                author: poster.to_string(),
                date: iso(row.posted),
                message_id: row.message_id,
                bytes: row.bytes,
                lines: row.lines,
                filename: Some(filename.clone()),
                release_name: Some(place.release.clone()),
                part: Some(part),
                total_parts: Some(parts),
                file_index: Some(place.file),
                file_total: Some(place.files),
                ..Article::default()
            });
        }
        leads
    }
}

/// `before`, a comma separated list of parts, with `more` after them.
fn with_leads(before: &str, more: Vec<i64>) -> String {
    before
        .split(',')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .chain(more.iter().map(i64::to_string))
        .collect::<Vec<_>>()
        .join(",")
}

/// The store the indexer writes to, opened on first use. In tests only one a test opened.
static STORE: Mutex<Option<Store>> = Mutex::new(None);

/// The store `hold` writes headers to: its own connection, soo a slice never waits on a linker pass (which reads, then
/// writes in short transactions; SQLite's busy timeout covers the overlap).
static RECORDER: Mutex<Option<Store>> = Mutex::new(None);

fn with_store<T>(f: impl FnOnce(&mut Store) -> Result<T>) -> Result<Option<T>> {
    in_store(&STORE, f)
}

fn in_store<T>(store: &Mutex<Option<Store>>, f: impl FnOnce(&mut Store) -> Result<T>) -> Result<Option<T>> {
    let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
    if store.is_none() {
        if cfg!(test) {
            return Ok(None);
        }
        *store = Some(Store::open(&path_for(&crate::paths::database()))?);
    }
    f(store.as_mut().expect("opened above")).map(Some)
}

/// Whether `article` is one this keeps: no subject rule reads it, its subject is one random token or its own
/// Message-ID, and it names a poster.
pub fn keeps(article: &Article) -> bool {
    !article.author.is_empty()
        && matches!(unparsable_shape(article), Shape::Token | Shape::IdSubject)
        && parse_subject(&article.subject).is_none()
}

/// A slice's articles of `group` without the ones obfuscated per article, whose headers go to the store. They're
/// counted as unparsable still (`atlas_articles_unparsable_total`), as no subject rule reads them. An error when the
/// headers couldnt be kept: the slice must not count as read.
pub fn hold(group: &str, articles: Vec<Article>) -> Result<Vec<Article>> {
    let (held, kept): (Vec<Article>, Vec<Article>) = articles.into_iter().partition(keeps);
    if held.is_empty() {
        return Ok(kept);
    }
    in_store(&RECORDER, |s| s.record(group, &held, now()))
        .map_err(|e| e.context(format!("xref: keeping {} headers of {group}", held.len())))?;
    let mut unparsable = Unparsable::default();
    for a in &held {
        unparsable.note(a);
    }
    crate::collections::count_unparsable(group, &unparsable);
    Ok(kept)
}

/// Whether a group still has to read (back) any day from a day before `first` to a day after `last`, the poster's wall
/// clock, its backfill chunk pending or claimed (`chunks`). `days` keeps the answers of one pass. Unknown counts as
/// read: a database that can't be asked shouldn't hold every upload for a day.
fn unread_days(main: &Connection, days: &mut HashMap<i64, bool>, first: i64, last: i64) -> bool {
    (first.div_euclid(86_400) - 1..=last.div_euclid(86_400) + 1).any(|day| {
        *days.entry(day).or_insert_with(|| {
            main.prepare_cached("select 1 from backfill_chunks where day = ?1 and state != 2 limit 1")
                .and_then(|mut stmt| stmt.exists([day]))
                .unwrap_or(false)
        })
    })
}

/// `atlas --xref-reread [--since YYYY-MM-DD] [--done-before UNIX_TIME] [--apply]`: the backfill days read before
/// this linker kept the headers of uploads obfuscated per article, put back to be read again whole
/// (`chunks::reopen_done`). Lockstep then reads them newest first, beside the days still to do. Days finished since
/// the linker started keeping headers (the start its store kept, `linker_since`, unless `--done-before` says) were
/// read with it and are left alone, as are days of groups the config doesn't index. Runs beside the indexer.
pub fn run_reread(args: &[String]) -> i32 {
    let apply = args.iter().any(|a| a == "--apply");
    let value_of = |flag: &str| -> std::result::Result<Option<&String>, String> {
        match args.iter().position(|a| a == flag) {
            None => Ok(None),
            Some(i) => match args.get(i + 1) {
                Some(v) if !v.starts_with("--") => Ok(Some(v)),
                _ => Err(format!("{flag} needs a value")),
            },
        }
    };
    let since = match value_of("--since") {
        Ok(None) => i64::MIN,
        Ok(Some(day)) => match chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d") {
            Ok(d) => d.and_hms_opt(0, 0, 0).expect("midnight").and_utc().timestamp() / 86_400,
            Err(_) => {
                println!("--since takes a day as YYYY-MM-DD, not {day}");
                return 1;
            }
        },
        Err(e) => {
            println!("{e}");
            return 1;
        }
    };
    let main = crate::paths::database();
    let done_before = match value_of("--done-before") {
        Ok(Some(t)) => match t.parse::<i64>() {
            Ok(t) => Some(t),
            Err(_) => {
                println!("--done-before takes a unix time in seconds, not {t}");
                return 1;
            }
        },
        Ok(None) => match linker_since(&main) {
            Ok(t) => t,
            Err(e) => {
                println!("couldnt read when the linker started from {}: {e:#}", path_for(&main).display());
                println!("pass --done-before to say");
                return 1;
            }
        },
        Err(e) => {
            println!("{e}");
            return 1;
        }
    };
    let Some(groups) = crate::config::load_config().map(|cfg| cfg.tracked_groups()) else {
        println!("couldnt read the config for the groups indexed");
        return 1;
    };
    let conn = match crate::db::open_at(&main) {
        Ok(c) => c,
        Err(e) => {
            println!("couldnt open {}: {e:#}", main.display());
            return 1;
        }
    };
    let (chunks, over, untracked) = match crate::chunks::done_since(&conn, since, done_before, &groups) {
        Ok(n) => n,
        Err(e) => {
            println!("couldnt count the days read: {e:#}");
            return 1;
        }
    };
    let span = match value_of("--since") {
        Ok(Some(day)) => format!("the days read from {day} on"),
        _ => "every day read".to_string(),
    };
    match done_before {
        Some(t) => println!(
            "{span}, finished before {} (when the linker started keeping headers; --done-before to change): {chunks} day chunks over {over} groups",
            chrono::DateTime::from_timestamp(t, 0)
                .map_or_else(|| t.to_string(), |d| d.format("%Y-%m-%d %H:%M:%SZ").to_string())
        ),
        None => println!(
            "{span}, whenever finished (no header store to tell when the linker started; --done-before to say): {chunks} day chunks over {over} groups"
        ),
    }
    if untracked > 0 {
        println!("{untracked} day chunks of groups the config doesn't index are left alone");
    }
    if !apply {
        println!("dry run: pass --apply to put them back to be read (the indexer may keep running)");
        return 0;
    }
    // a write to the main database, beside the indexer: held off a compaction, refused on a merge half done
    let _held = match crate::compact::hold_off_compaction(&main) {
        Ok(h) => h,
        Err(e) => {
            println!("{e:#}");
            return 1;
        }
    };
    match crate::chunks::reopen_done(&conn, since, done_before, &groups) {
        Ok(n) => {
            println!("{n} day chunks put back to be read again");
            0
        }
        Err(e) => {
            println!("couldnt put them back: {e:#}");
            1
        }
    }
}

/// When the linker started keeping headers: the start the store beside `main` kept, or for one a build from before
/// that made and none since opened, its file's birth or oldest header. None without a store; an error when one is
/// there but can't be read (not taken as none, which would put back the days read with the linker too).
fn linker_since(main: &Path) -> Result<Option<i64>> {
    let path = path_for(main);
    if !path.try_exists()? {
        return Ok(None);
    }
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(&path, flags)?;
    conn.busy_timeout(Duration::from_secs(30))?;
    let has_meta: bool =
        conn.prepare("select 1 from sqlite_master where type = 'table' and name = 'meta'")?.exists([])?;
    if has_meta
        && let Some(t) = conn.query_row("select value from meta where key = 'started'", [], |r| r.get(0)).optional()?
    {
        return Ok(Some(t));
    }
    started_by_its_file(&conn, &path)
}

/// When the store at `path` began, by what it holds: its file's birth (where the filesystem keeps one) or its oldest
/// header, whichever is earlier. The headers alone don't say, as the prune drops them past a day or two.
fn started_by_its_file(conn: &Connection, path: &Path) -> Result<Option<i64>> {
    let born = std::fs::metadata(path)
        .and_then(|m| m.created())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    let oldest: Option<i64> = conn.query_row("select min(added) from headers", [], |r| r.get(0))?;
    Ok(match (born, oldest) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    })
}

/// Every `PASS_EVERY`, drop the headers past the window, then save what has settled.
pub async fn run(db: crate::indexer::Db, stopping: impl Fn() -> bool) {
    // a pass whose releases or note didnt all go through: tried again as it was, before any new pass, soo a retry
    // saves the same parts under the same numbers
    let mut pending: Option<Arc<Found>> = None;
    while !stopping() {
        let mut slept = Duration::ZERO;
        while slept < PASS_EVERY && !stopping() {
            tokio::time::sleep(Duration::from_secs(1)).await;
            slept += Duration::from_secs(1);
        }
        if stopping() {
            break;
        }
        // the WAL written back before each pass, while no pass reads (`Store::checkpoint`)
        match tokio::task::spawn_blocking(|| with_store(|s| s.checkpoint())).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => eprintln!("xref: couldnt write the WAL back: {e:#}"),
            Err(e) => eprintln!("xref: writing the WAL back failed: {e}"),
        }
        let found = match pending.take() {
            Some(found) => found,
            None => {
                let found = tokio::task::spawn_blocking(|| {
                    let main = Connection::open_with_flags(
                        crate::paths::database(),
                        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
                    )?;
                    main.busy_timeout(Duration::from_secs(30))?;
                    let mut days: HashMap<i64, bool> = HashMap::new();
                    let mut unread = |first: i64, last: i64| unread_days(&main, &mut days, first, last);
                    with_store(|s| {
                        let t = now();
                        if crate::frozen::STOPPING.load(std::sync::atomic::Ordering::Relaxed) {
                            return Ok(Found {
                                releases: Vec::new(),
                                ack: Ack { finished: Vec::new(), widened: Vec::new(), now: t },
                            });
                        }
                        s.prune(t)?;
                        s.pass_with(t, &mut unread, &|| {
                            crate::frozen::STOPPING.load(std::sync::atomic::Ordering::Relaxed)
                        })
                    })
                })
                .await;
                match found {
                    Ok(Ok(Some(found))) => Arc::new(found),
                    Ok(Ok(None)) => continue,
                    Ok(Err(e)) => {
                        eprintln!("xref: {e:#}");
                        continue;
                    }
                    Err(e) => {
                        eprintln!("xref: the pass failed: {e}");
                        continue;
                    }
                }
            }
        };
        let (count, articles) = (found.releases.len(), found.releases.iter().map(|r| r.articles.len()).sum::<usize>());
        // a save goes to one shard's writer, soo each shard's releases go apart; saving one again changes nothing
        let mut by_shard: HashMap<usize, Vec<Release>> = HashMap::new();
        for r in &found.releases {
            by_shard.entry(crate::store::shard_of(&r.group)).or_default().push(r.clone());
        }
        let mut saved = true;
        for (_, releases) in by_shard {
            if let Err(e) = db.save(releases).await {
                eprintln!("xref: couldnt save uploads: {e:#}");
                saved = false;
            }
        }
        if !saved {
            pending = Some(found);
            continue;
        }
        let noting = found.clone();
        match tokio::task::spawn_blocking(move || with_store(|s| s.ack(&noting.ack))).await {
            Ok(Ok(_)) if count > 0 => println!("xref: saved {count} uploads, {articles} articles"),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                eprintln!("xref: couldnt note what was saved: {e:#}");
                pending = Some(found);
            }
            Err(e) => {
                eprintln!("xref: noting what was saved failed: {e}");
                pending = Some(found);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(poster: &str, n: i64, first: i64, last: i64, groups: &[i64]) -> FileSum {
        FileSum { poster: poster.into(), n, first, last, added: 0, groups: groups.iter().copied().collect() }
    }

    const SIX: [i64; 6] = [1, 2, 3, 4, 5, 6];
    const FOUR: [i64; 4] = [2, 3, 4, 5];

    fn posters(files: &[FileSum], chains: &[(Option<usize>, Vec<usize>)]) -> Vec<Vec<String>> {
        chains.iter().map(|(_, c)| c.iter().map(|&i| files[i].poster.clone()).collect()).collect()
    }

    #[test]
    fn an_upload_is_its_index_then_files_to_the_same_groups_back_to_back() {
        // another uploader's files to four groups end as this one starts, and its PAR2 tail comes in among them
        let files = vec![
            file("other-a", 482, -17, -15, &FOUR),
            file("other-b", 499, -1, 1, &FOUR),
            file("index", 1, 0, 0, &[1]),
            file("rar1", 293, 0, 1, &SIX),
            file("other-par2", 151, 2, 3, &FOUR),
            file("other-tail", 5, 2, 2, &[2, 4, 5]),
            file("rar2", 293, 1, 2, &SIX),
            file("rar3", 293, 2, 3, &SIX),
            file("vol-small", 8, 38, 38, &[1, 2, 3, 6]),
            file("vol-big", 469, 40, 42, &SIX),
            // well after: a third uploader's stray par2
            file("stray", 5, 73, 73, &[2, 4, 5]),
        ];
        let chains = chain(&files, &[]);
        let got = posters(&files, &chains);
        let ours = got.iter().find(|c| c.contains(&"rar1".to_string())).unwrap();
        // vol-small is 35s after rar3: past the gap, it starts an upload of its own here, soo the test spaces them
        assert_eq!(ours, &vec!["index", "rar1", "rar2", "rar3"]);
        let other = got.iter().find(|c| c.contains(&"other-b".to_string())).unwrap();
        assert!(other.contains(&"other-tail".to_string()) && other.contains(&"other-par2".to_string()), "{got:?}");
        assert!(got.iter().any(|c| c == &vec!["stray".to_string()]), "{got:?}");
    }

    #[test]
    fn the_par2_tail_follows_the_last_volume_within_the_gap() {
        let files = vec![
            file("index", 1, 0, 0, &[3]),
            file("rar1", 293, 0, 1, &SIX),
            file("rar2", 293, 1, 2, &SIX),
            file("vol8", 8, 2, 2, &[1, 2, 3, 6]),
            file("vol15", 15, 2, 3, &SIX),
            file("vol469", 469, 3, 5, &SIX),
        ];
        let got = posters(&files, &chain(&files, &[]));
        assert_eq!(got, vec![vec!["index", "rar1", "rar2", "vol8", "vol15", "vol469"]]);
    }

    #[test]
    fn the_next_index_to_the_same_groups_starts_the_next_upload() {
        let files = vec![
            file("a-index", 1, 0, 0, &[1]),
            file("a-rar1", 74, 0, 1, &SIX),
            file("a-vol", 59, 1, 2, &SIX),
            file("b-index", 1, 3, 3, &[2]),
            file("b-rar1", 74, 3, 4, &SIX),
            file("b-rar2", 74, 4, 5, &SIX),
        ];
        let got = posters(&files, &chain(&files, &[]));
        assert_eq!(got, vec![vec!["a-index", "a-rar1", "a-vol"], vec!["b-index", "b-rar1", "b-rar2"]]);
    }

    fn article(mid: &str, poster: &str, date: &str, number: u64) -> Article {
        Article {
            number,
            subject: format!("{mid}x{}", "Q".repeat(40)),
            author: poster.into(),
            date: date.into(),
            message_id: format!("<{mid}@h.x>"),
            bytes: 740_000,
            lines: 5692,
            ..Article::default()
        }
    }

    #[test]
    fn only_subjects_no_rule_reads_are_kept() {
        let token = article("aaaaaaaaaaaaaaaaaaaaaaaa", "p@q.r", "2023-12-03 16:23:23", 1);
        assert!(keeps(&token));
        let mut counted = token.clone();
        counted.subject = format!("{} (1/50)", counted.subject);
        assert!(!keeps(&counted), "a part counter is read by `gather_per_article`");
        let mut no_poster = token.clone();
        no_poster.author.clear();
        assert!(!keeps(&no_poster));
        let mut id = token.clone();
        id.subject = "aaaaaaaaaaaaaaaaaaaaaaaa".into();
        assert!(keeps(&id), "ngPost -x");
    }

    #[test]
    fn a_settled_upload_is_saved_as_one_release_spanning_its_groups() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("atlas.xref.db")).unwrap();
        let groups = ["a.b.one", "a.b.two", "a.b.three"];
        let mut n = 0;
        // an index, then two files of 6 articles spread over three groups, a second apart
        let mut put = |store: &mut Store, poster: &str, count: usize, second: u32, only: Option<&str>| {
            for k in 0..count {
                let g = only.unwrap_or(groups[k % 3]);
                n += 1;
                let a = article(&format!("{poster}{k:020}"), poster, &format!("2023-12-03 16:23:{second:02}"), n);
                store.record(g, &[a], 1000).unwrap();
            }
        };
        put(&mut store, "index@x.y", 1, 23, Some("a.b.one"));
        put(&mut store, "file1@x.y", 6, 23, None);
        put(&mut store, "file2@x.y", 6, 24, None);
        // a second server's listing brings an article again
        let again = article(&format!("file1@x.y{:020}", 0), "file1@x.y", "2023-12-03 16:23:23", 1);
        assert_eq!(store.record("a.b.one", &[again], 1000).unwrap(), 0);

        assert!(store.pass(1000 + SETTLE_SECS - 1).unwrap().is_empty(), "not settled yet");
        let releases = store.pass(1001 + SETTLE_SECS).unwrap();
        assert_eq!(releases.len(), 1);
        let r = &releases[0];
        assert_eq!(r.articles.len(), 13);
        assert!(r.name.starts_with("xref-upload-"));
        assert!(r.is_obfuscated);
        let files: BTreeSet<_> = r.articles.iter().map(|a| a.filename.clone().unwrap()).collect();
        assert_eq!(files.len(), 3);
        assert!(r.articles.iter().all(|a| a.file_total == Some(3)));
        assert!(r.complete, "every file has each of its parts");
        assert!(store.pass(2001 + SETTLE_SECS).unwrap().is_empty(), "saved once");

        // an article of file2 that comes late joins its file
        let late = article(&format!("file2@x.y{:020}", 99), "file2@x.y", "2023-12-03 16:23:24", 99);
        store.record("a.b.two", &[late], 5000).unwrap();
        let more = store.pass(5001 + SETTLE_SECS).unwrap();
        assert_eq!(more.len(), 1);
        assert_eq!(more[0].name, r.name);
        assert_eq!(more[0].articles.len(), 1);
        assert_eq!(more[0].articles[0].part, Some(7));
        assert_eq!(more[0].articles[0].total_parts, Some(7));

        assert_eq!(store.prune(5000 + KEEP_SECS + 1).unwrap(), 14);
    }

    #[test]
    fn uploads_too_small_to_be_one_are_let_go() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("atlas.xref.db")).unwrap();
        // a random poster for every article: each its own file of one
        for k in 0..20 {
            let a = article(&format!("m{k:023}"), &format!("p{k}@x.y"), "2023-12-03 16:23:23", k);
            store.record("a.b.one", &[a], 1000).unwrap();
        }
        assert!(store.pass(1001 + SETTLE_SECS).unwrap().is_empty());
        assert!(store.waiting().unwrap().is_empty(), "marked done, left for the prune");
    }

    #[test]
    fn an_upload_waits_while_a_group_is_still_to_be_read_for_its_day() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("atlas.xref.db")).unwrap();
        for k in 0..12u64 {
            let a = article(&format!("w{k:023}"), "file@x.y", "2023-12-03 16:23:23", k);
            store.record(["a.b.one", "a.b.two"][k as usize % 2], &[a], 1000).unwrap();
        }
        let later = 1001 + SETTLE_SECS;
        assert!(
            store.pass_with(later, &mut |_, _| true, &|| false).unwrap().releases.is_empty(),
            "a group hasnt read that day"
        );
        let found = store.pass_with(1001 + KEEP_SECS, &mut |_, _| true, &|| false).unwrap();
        assert_eq!(found.releases.len(), 1, "not past a day");
        // not noted as saved: found again
        assert_eq!(store.pass_with(1002 + KEEP_SECS, &mut |_, _| true, &|| false).unwrap().releases.len(), 1);
        store.ack(&found.ack).unwrap();
        assert!(store.pass_with(1003 + KEEP_SECS, &mut |_, _| true, &|| false).unwrap().releases.is_empty());
    }

    #[test]
    fn a_file_read_later_in_another_group_joins_its_saved_upload() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("atlas.xref.db")).unwrap();
        let mut n = 0;
        let mut put = |store: &mut Store, poster: &str, count: usize, second: u32, group: &str, added: i64| {
            for k in 0..count {
                n += 1;
                let a =
                    article(&format!("{poster}{group}{k:010}"), poster, &format!("2023-12-03 16:23:{second:02}"), n);
                store.record(group, &[a], added).unwrap();
            }
        };
        put(&mut store, "rar1@x.y", 8, 23, "a.b.one", 1000);
        put(&mut store, "rar2@x.y", 8, 24, "a.b.one", 1000);
        let first = store.pass(1001 + SETTLE_SECS).unwrap();
        assert_eq!(first.len(), 1);
        // a.b.two is read an hour on: more of rar2, and a PAR2 volume none of whose articles went to a.b.one
        put(&mut store, "rar2@x.y", 3, 24, "a.b.two", 4600);
        put(&mut store, "vol@x.y", 4, 25, "a.b.two", 4600);
        // another uploader's file to a.b.one a minute on is no part of it
        put(&mut store, "other@x.y", 12, 59, "a.b.one", 4600);
        let more = store.pass(4601 + SETTLE_SECS).unwrap();
        let joined = more.iter().find(|r| r.name == first[0].name).expect("joins the saved upload");
        assert_eq!(joined.articles.len(), 7);
        // a.b.two's first article of rar2 is a likely first piece too, though numbered after a.b.one's
        assert_eq!(store.place_of("rar2@x.y").unwrap().unwrap().leads, "1,9");
        // a.b.one again, from a listing that has an article numbered below all it gave before: a lead too
        let lower = article(&format!("rar2@x.ya.b.one{:010}", 99), "rar2@x.y", "2023-12-03 16:23:24", 0);
        let higher = article(&format!("rar2@x.ya.b.one{:010}", 98), "rar2@x.y", "2023-12-03 16:23:24", 9_999);
        store.record("a.b.one", &[higher], 8200).unwrap();
        store.record("a.b.one", &[lower], 8200).unwrap();
        store.pass(8201 + SETTLE_SECS).unwrap();
        assert_eq!(store.place_of("rar2@x.y").unwrap().unwrap().leads, "1,9,12");
        let vol: Vec<_> = joined.articles.iter().filter(|a| a.author == "vol@x.y").collect();
        assert_eq!(vol.len(), 4);
        assert!(vol.iter().all(|a| a.file_index == Some(3) && a.file_total == Some(3)), "its third file");
        assert!(more.iter().any(|r| r.name != first[0].name && r.articles.len() == 12), "{more:?}");
    }

    #[test]
    fn leads_lists_join_on() {
        assert_eq!(file_of("xref-upload-1-2-00007"), Some(7));
        assert_eq!(with_leads("1,2", vec![9]), "1,2,9");
        assert_eq!(with_leads("", vec![1, 3]), "1,3");
    }

    #[test]
    fn a_pass_found_again_numbers_its_parts_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("atlas.xref.db")).unwrap();
        for k in 0..12u64 {
            let a = article(&format!("r{k:023}"), "file@x.y", "2023-12-03 16:23:23", 100 + k);
            store.record("a.b.one", &[a], 1000).unwrap();
        }
        let parts = |found: &Found| -> HashMap<String, i64> {
            found.releases.iter().flat_map(|r| &r.articles).map(|a| (a.message_id.clone(), a.part.unwrap())).collect()
        };
        let first = parts(&store.pass_with(1001 + SETTLE_SECS, &mut |_, _| false, &|| false).unwrap());
        // not noted: another group's article, numbered lower, comes before the next pass
        let late = article(&format!("s{:023}", 0), "file@x.y", "2023-12-03 16:23:23", 1);
        store.record("a.b.two", &[late.clone()], 1002 + SETTLE_SECS).unwrap();
        let again = parts(&store.pass_with(1003 + 2 * SETTLE_SECS, &mut |_, _| false, &|| false).unwrap());
        assert!(first.iter().all(|(id, p)| again[id] == *p), "the same numbers");
        assert_eq!(again[&late.message_id], 13, "what came since goes after");
    }

    #[test]
    fn articles_of_a_file_saved_past_the_window_are_let_go() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("atlas.xref.db")).unwrap();
        for k in 0..12u64 {
            let a = article(&format!("p{k:023}"), "file@x.y", "2023-12-03 16:23:23", k);
            store.record("a.b.one", &[a], 1000).unwrap();
        }
        assert_eq!(store.pass(1001 + SETTLE_SECS).unwrap().len(), 1);
        // read again two days on, its headers gone: it can't be told from a new part
        let t = 1000 + 2 * KEEP_SECS;
        store.prune(t).unwrap();
        let again = article(&format!("p{:023}", 0), "file@x.y", "2023-12-03 16:23:23", 0);
        assert_eq!(store.record("a.b.one", &[again], t).unwrap(), 1);
        assert!(store.pass(t + 1 + SETTLE_SECS).unwrap().is_empty());
        assert!(store.waiting().unwrap().is_empty(), "noted, not saved");
    }

    #[test]
    fn late_parts_widen_their_upload_before_new_files_are_matched() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("atlas.xref.db")).unwrap();
        let put = |store: &mut Store, poster: &str, tag: &str, count: u64, second: u32, group: &str, added: i64| {
            for k in 0..count {
                let a = article(&format!("{poster}{tag}{k:08}"), poster, &format!("2023-12-03 16:23:{second:02}"), k);
                store.record(group, &[a], added).unwrap();
            }
        };
        put(&mut store, "rar1@x.y", "a", 12, 1, "a.b.one", 1000);
        let first = store.pass(1001 + SETTLE_SECS).unwrap();
        assert_eq!(first.len(), 1);
        // a.b.two read later: rar1's parts posted up to second 10, and a small file at second 14
        put(&mut store, "rar1@x.y", "b", 3, 10, "a.b.two", 5000);
        put(&mut store, "vol@x.y", "b", 4, 14, "a.b.two", 5000);
        let more = store.pass(5001 + SETTLE_SECS).unwrap();
        assert_eq!(more.len(), 1);
        assert_eq!(more[0].name, first[0].name);
        assert_eq!(more[0].articles.len(), 7, "the late parts and the small file both join");
    }

    #[test]
    fn a_stop_gives_the_pass_up_and_noise_let_go_spares_what_came_after() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("atlas.xref.db")).unwrap();
        for k in 0..20u64 {
            let a = article(&format!("n{k:023}"), &format!("p{k}@x.y"), "2023-12-03 16:23:23", k);
            store.record("a.b.one", &[a], 1000).unwrap();
        }
        let given_up = store.pass_with(1001 + SETTLE_SECS, &mut |_, _| false, &|| true).unwrap();
        assert!(given_up.releases.is_empty() && given_up.ack.finished.is_empty(), "nothing found, nothing to note");
        let found = store.pass_with(1001 + SETTLE_SECS, &mut |_, _| false, &|| false).unwrap();
        assert_eq!(found.ack.finished.len(), 20, "each a file of one, let go");
        // an article of one of them comes before the note is made: it isn't noted with them
        let later = article(&format!("n{:023}", 99), "p3@x.y", "2023-12-03 16:23:23", 99);
        store.record("a.b.one", &[later], 2000).unwrap();
        store.ack(&found.ack).unwrap();
        assert_eq!(store.waiting().unwrap().len(), 1, "the later article waits");
    }

    #[test]
    fn a_checkpoint_writes_the_wal_back_and_starts_it_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.xref.db");
        let mut store = Store::open(&path).unwrap();
        // a reader on another connection while the writes go in: passive checkpoints can't finish meanwhile
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("begin; select count(*) from headers;").unwrap();
        let batch: Vec<Article> = (0..5000u64)
            .map(|k| article(&format!("w{k:023}"), &format!("p{}@x.y", k % 7), "2023-12-03 16:23:23", k))
            .collect();
        store.record("a.b.one", &batch, 1000).unwrap();
        let wal = std::path::PathBuf::from(format!("{}-wal", path.display()));
        assert!(std::fs::metadata(&wal).unwrap().len() > 0);
        reader.execute_batch("commit;").unwrap();
        drop(reader);
        let (frames, written) = store.checkpoint().unwrap();
        assert_eq!(frames, written, "all of it written back");
        assert_eq!(std::fs::metadata(&wal).unwrap().len(), 0, "and the WAL started over");
        assert_eq!(store.waiting().unwrap().len(), 7, "nothing lost");
    }

    #[test]
    fn a_big_slice_is_recorded_in_short_transactions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.xref.db");
        let mut store = Store::open(&path).unwrap();
        let commits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = commits.clone();
        store
            .conn
            .commit_hook(Some(move || {
                seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                false
            }))
            .unwrap();
        let batch: Vec<Article> = (0..(WRITE_CHUNK * 5 + 1) as u64)
            .map(|k| article(&format!("y{k:023}"), &format!("p{}@x.y", k % 7), "2023-12-03 16:23:23", k))
            .collect();
        store.group_id("a.b.one").unwrap();
        commits.store(0, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(store.record("a.b.one", &batch, 1000).unwrap(), batch.len());
        assert_eq!(commits.load(std::sync::atomic::Ordering::Relaxed), 6, "a commit every WRITE_CHUNK rows");
    }

    #[test]
    fn the_linker_start_is_what_the_store_kept_not_its_oldest_header() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        assert_eq!(linker_since(&main).unwrap(), None, "no store, no start");
        let path = path_for(&main);
        // a store from a build that kept no start, its headers pruned to recent ones: the earlier of its file's birth
        // and its oldest header
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&SCHEMA.replace("create table if not exists meta", "create table if not exists unused"))
                .unwrap();
            conn.execute_batch("drop table unused; insert into groups (name) values ('a.b.one');").unwrap();
            conn.execute(
                "insert into headers (message_id, poster, posted, grp, number, bytes, lines, added)
                 values ('<m@x>', 'p@x', 1, 1, 1, 1, 1, ?1)",
                [now() + 3600],
            )
            .unwrap();
        }
        let born = linker_since(&main).unwrap().unwrap();
        assert!(born <= now() && born > now() - 600, "its file's birth, before its oldest header: {born}");
        // opened by this build: kept, and what's read from then on whatever the prune drops
        drop(Store::open(&path).unwrap());
        Connection::open(&path).unwrap().execute("update meta set value = 1000 where key = 'started'", []).unwrap();
        let mut store = Store::open(&path).unwrap();
        store.prune(now() + 30 * KEEP_SECS).unwrap();
        drop(store);
        assert_eq!(linker_since(&main).unwrap(), Some(1000), "kept once, not set again");
        // a store that can't be read is an error, not taken as none
        std::fs::write(&path, b"not a database, not at all, not even close to one, no").unwrap();
        for side in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{side}", path.display()));
        }
        assert!(linker_since(&main).is_err());
    }
}
