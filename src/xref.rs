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
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
use chrono::DateTime;
use rusqlite::{Connection, OptionalExtension, params};

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
/// how often the linker runs
const PASS_EVERY: Duration = Duration::from_secs(60);
/// what the name of every upload saved here starts with (`is_upload`)
pub const NAME_PREFIX: &str = "xref-upload-";
/// a volume's articles the archive step tries as its first piece: the first a group got of it, in each group. Piece
/// order isn't in the headers, and on the Wheel of Time upload the first piece was the first its group got in 8 of 40
/// volumes
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
    added   INTEGER NOT NULL
) without rowid;
create index if not exists linked_added on linked(added);
-- each upload saved: its release, the groups its articles have been read in so far, when its files were posted, and
-- how many it has, soo files read later in other groups join it; `saved` when it was first saved (`guess_round`)
create table if not exists uploads (
    release TEXT PRIMARY KEY,
    saved   INTEGER NOT NULL,
    grp     TEXT NOT NULL,
    groups  TEXT NOT NULL,
    first   INTEGER NOT NULL,
    last    INTEGER NOT NULL,
    files   INTEGER NOT NULL,
    added   INTEGER NOT NULL
) without rowid;
create index if not exists uploads_added on uploads(added);
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
    for i in order {
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
    uploads.into_iter().filter(|u| !u.files.is_empty()).map(|u| (u.seed.map(|(j, _)| j), u.files)).collect()
}

/// Stable, in decimal: a run of hex would read as a random name, and leave search (as `filters::per_article_name`).
fn upload_name(poster: &str) -> String {
    let key = format!("xref\0{poster}");
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3));
    format!("{NAME_PREFIX}{}-{}", hash >> 32, hash & 0xffff_ffff)
}

/// When the upload `release` was first saved, from the store the indexer writes. None when it isn't kept (any more), or
/// in tests.
pub fn saved_at(release: &str) -> Option<i64> {
    if cfg!(test) {
        return None;
    }
    let conn = Connection::open_with_flags(
        path_for(&crate::paths::database()),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.query_row("select saved from uploads where release = ?1", [release], |r| r.get(0)).optional().ok()?
}

/// Which of an upload's `volumes` the archive step starts its first-piece guesses at by `now`, and when to look again
/// if they find none: each hour since it was `saved` takes the next `HEAD_PROBES / LEADS` volumes, until every volume
/// has had a turn. Not known when it was saved: the first volumes, once.
pub fn guess_round(saved: Option<i64>, volumes: usize, now: i64) -> (usize, Option<i64>) {
    let per_round = (HEAD_PROBES / LEADS).max(1);
    let rounds = volumes.div_ceil(per_round) as i64;
    let Some(saved) = saved else { return (0, None) };
    let round = (now - saved).max(0) / 3600;
    let again = (round + 1 < rounds).then_some(saved + (round + 1) * 3600);
    (round as usize * per_round, again)
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
    posted: i64,
    grp: i64,
    number: i64,
    bytes: i64,
    lines: i64,
}

/// Where a file goes: its release, that release's group, its place among the upload's files, and the parts it has.
struct Place {
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

/// What a pass took as done, noted by `Store::ack` once its releases are saved.
pub struct Ack {
    finished: Vec<(String, Option<Place>, Vec<String>)>,
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
        conn.busy_timeout(Duration::from_secs(30))?;
        conn.execute_batch("pragma journal_mode = wal; pragma synchronous = normal;")?;
        conn.execute_batch(SCHEMA)?;
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

    fn group_name(&self, id: i64) -> String {
        self.groups.iter().find(|(_, v)| **v == id).map(|(k, _)| k.clone()).unwrap_or_default()
    }

    /// Keep the headers of `articles` of `group`, unless another server's listing brought them already.
    pub fn record(&mut self, group: &str, articles: &[Article], added: i64) -> Result<usize> {
        let grp = self.group_id(group)?;
        let tx = self.conn.transaction()?;
        let mut n = 0;
        {
            let mut insert = tx.prepare_cached(
                "insert or ignore into headers (message_id, poster, posted, grp, number, bytes, lines, added)
                 values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for a in articles {
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
        Ok(n)
    }

    /// Drop what arrived over `KEEP_SECS` ago.
    pub fn prune(&mut self, now: i64) -> Result<usize> {
        let before = now - KEEP_SECS;
        // what waits for a group to be read is let go at the latest a day after it would settle (see `pass_with`)
        let n = self.conn.execute(
            "delete from headers where added < ?1 and (done = 1 or added < ?2)",
            params![before, before - KEEP_SECS],
        )?;
        self.conn.execute("delete from linked where added < ?1", [before])?;
        self.conn.execute("delete from uploads where added < ?1", [before])?;
        Ok(n)
    }

    fn waiting(&self) -> Result<Vec<FileSum>> {
        let mut stmt = self.conn.prepare(
            "select poster, count(*), min(posted), max(posted), max(added), group_concat(distinct grp)
             from headers where done = 0 group by poster",
        )?;
        let rows = stmt.query_map([], |r| {
            let groups: String = r.get(5)?;
            Ok(FileSum {
                poster: r.get(0)?,
                n: r.get(1)?,
                first: r.get(2)?,
                last: r.get(3)?,
                added: r.get(4)?,
                groups: groups.split(',').filter_map(|g| g.parse().ok()).collect(),
            })
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

    fn place_of(&self, poster: &str) -> Result<Option<Place>> {
        Ok(self
            .conn
            .query_row("select release, grp, file, files, parts from linked where poster = ?1", [poster], |r| {
                Ok(Place { release: r.get(0)?, group: r.get(1)?, file: r.get(2)?, files: r.get(3)?, parts: r.get(4)? })
            })
            .optional()?)
    }

    /// A poster's articles waiting, in the order their parts are numbered: the first each group got, group by group,
    /// then each group's second, and on. A file's first piece is most often among the first (see `LEADS`).
    fn rows_of(&self, poster: &str) -> Result<Vec<Row>> {
        let mut stmt = self.conn.prepare_cached(
            "select message_id, posted, grp, number, bytes, lines from headers
             where poster = ?1 and done = 0
             order by row_number() over (partition by grp order by number, message_id), grp",
        )?;
        let rows = stmt.query_map([poster], |r| {
            Ok(Row {
                message_id: r.get(0)?,
                posted: r.get(1)?,
                grp: r.get(2)?,
                number: r.get(3)?,
                bytes: r.get(4)?,
                lines: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The uploads that have settled by `now`, each as a release, and the articles that came late for a file already
    /// in one, as that release. Their headers are marked done, and each file's place kept for what comes later.
    pub fn pass(&mut self, now: i64) -> Result<Vec<Release>> {
        let found = self.pass_with(now, &mut |_, _| false)?;
        self.ack(found.ack)?;
        Ok(found.releases)
    }

    /// `pass`, a file settled only once `unread(first, last)` says no group is still to be read for the time it was
    /// posted in, or once it has waited `KEEP_SECS`: an upload saved before every group was read for it would be
    /// split, as a group read later brings files of it that no article read before linked.
    pub fn pass_with(&mut self, now: i64, unread: &mut dyn FnMut(i64, i64) -> bool) -> Result<Found> {
        let mut waits: HashMap<String, bool> = HashMap::new();
        for f in self.waiting()? {
            let quiet = f.added < now - SETTLE_SECS;
            let settled = f.added < now - KEEP_SECS || (quiet && !unread(f.first, f.last));
            waits.insert(f.poster, !settled);
        }
        let settled = |f: &FileSum| !waits.get(&f.poster).copied().unwrap_or(true);
        let mut fresh = Vec::new();
        let mut late = Vec::new();
        for f in self.waiting()? {
            match self.place_of(&f.poster)? {
                Some(place) => late.push((f, place)),
                None => fresh.push(f),
            }
        }

        let mut out: HashMap<String, Release> = HashMap::new();
        // each poster done with: where its file went, and the articles of it this pass took
        let mut finished: Vec<(String, Option<Place>, Vec<String>)> = Vec::new();

        for (f, place) in late {
            if settled(&f) {
                let rows = self.rows_of(&f.poster)?;
                let parts = place.parts + rows.len() as i64;
                let ids = rows.iter().map(|r| r.message_id.clone()).collect();
                self.add_file(&mut out, &place, &f.poster, rows, parts);
                finished.push((f.poster, Some(Place { parts, ..place }), ids));
            }
        }

        let saved = self.saved(now)?;
        let seeds: Vec<Seed> = saved.iter().map(|u| u.seed.clone()).collect();
        let mut widened: Vec<(String, String, Seed, i64)> = Vec::new();
        for (seed, files) in chain(&fresh, &seeds) {
            if !files.iter().all(|&i| settled(&fresh[i])) {
                continue;
            }
            let articles: i64 = files.iter().map(|&i| fresh[i].n).sum();
            if seed.is_none() && articles < MIN_UPLOAD_ARTICLES {
                for &i in &files {
                    let ids = self.rows_of(&fresh[i].poster)?.into_iter().map(|r| r.message_id).collect();
                    finished.push((fresh[i].poster.clone(), None, ids));
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
                    (upload_name(&fresh[files[0]].poster), self.group_name(home), 0)
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
                };
                let ids = rows.iter().map(|r| r.message_id.clone()).collect();
                self.add_file(&mut out, &place, &fresh[i].poster, rows, parts);
                finished.push((fresh[i].poster.clone(), Some(Place { parts, ..place }), ids));
            }
            widened.push((release, group, span, count));
        }
        // the groups late articles were read in widen their upload too
        for (poster, place, _) in &finished {
            let Some(place) = place else { continue };
            if widened.iter().any(|(r, _, _, _)| *r == place.release) {
                continue;
            }
            if let Some(u) = saved.iter().find(|u| u.release == place.release) {
                let mut seed = u.seed.clone();
                let mut stmt = self.conn.prepare_cached("select distinct grp from headers where poster = ?1")?;
                for g in stmt.query_map([poster], |r| r.get::<_, i64>(0))? {
                    seed.groups.insert(g?);
                }
                match widened.iter_mut().find(|(r, _, _, _)| *r == u.release) {
                    Some((_, _, s, _)) => s.groups.extend(seed.groups),
                    None => widened.push((u.release.clone(), u.group.clone(), seed, u.files)),
                }
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
    pub fn ack(&mut self, ack: Ack) -> Result<()> {
        let Ack { finished, widened, now } = ack;
        let tx = self.conn.transaction()?;
        {
            let mut done = tx.prepare_cached("update headers set done = 1 where message_id = ?1")?;
            let mut keep = tx.prepare_cached(
                "insert into linked (poster, release, grp, file, files, parts, added) values (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 on conflict(poster) do update set parts = excluded.parts, added = excluded.added",
            )?;
            for (poster, place, ids) in &finished {
                for id in ids {
                    done.execute([id])?;
                }
                if let Some(p) = place {
                    keep.execute(params![poster, p.release, p.group, p.file, p.files, p.parts, now])?;
                }
            }
            let mut upload = tx.prepare_cached(
                "insert into uploads (release, grp, groups, first, last, files, added, saved)
                 values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
                 on conflict(release) do update set groups = excluded.groups, first = excluded.first,
                     last = excluded.last, files = excluded.files, added = excluded.added",
            )?;
            for (release, group, span, files) in &widened {
                let groups = span.groups.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
                upload.execute(params![release, group, groups, span.first, span.last, files, now])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// `rows` of one poster as a file of `place`'s release, parts numbered on from the ones it has, of `parts` in all.
    fn add_file(&self, out: &mut HashMap<String, Release>, place: &Place, poster: &str, rows: Vec<Row>, parts: i64) {
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
    }
}

/// The store the indexer writes to, opened on first use. In tests only one a test opened.
static STORE: Mutex<Option<Store>> = Mutex::new(None);

/// Headers a slice brought while a pass held the store, kept here instead of making the slice wait: (group, its
/// articles, when they came). Whoever next holds the store writes them first.
static QUEUED: Mutex<Vec<(String, Vec<Article>, i64)>> = Mutex::new(Vec::new());

fn with_store<T>(f: impl FnOnce(&mut Store) -> Result<T>) -> Result<Option<T>> {
    let store = STORE.lock().unwrap_or_else(|e| e.into_inner());
    in_store(store, f)
}

fn in_store<T>(
    mut store: std::sync::MutexGuard<'_, Option<Store>>,
    f: impl FnOnce(&mut Store) -> Result<T>,
) -> Result<Option<T>> {
    if store.is_none() {
        if cfg!(test) {
            return Ok(None);
        }
        *store = Some(Store::open(&path_for(&crate::paths::database()))?);
    }
    let s = store.as_mut().expect("opened above");
    let queued = std::mem::take(&mut *QUEUED.lock().unwrap_or_else(|e| e.into_inner()));
    for (group, articles, added) in queued {
        s.record(&group, &articles, added)?;
    }
    f(s).map(Some)
}

/// Whether `article` is one this keeps: no subject rule reads it, its subject is one random token or its own
/// Message-ID, and it names a poster.
pub fn keeps(article: &Article) -> bool {
    !article.author.is_empty()
        && matches!(unparsable_shape(article), Shape::Token | Shape::IdSubject)
        && parse_subject(&article.subject).is_none()
}

/// A slice's articles of `group` without the ones obfuscated per article, whose headers go to the store. They're
/// counted as unparsable still (`atlas_articles_unparsable_total`), as no subject rule reads them.
pub fn hold(group: &str, articles: Vec<Article>) -> Vec<Article> {
    let (held, kept): (Vec<Article>, Vec<Article>) = articles.into_iter().partition(keeps);
    if held.is_empty() {
        return kept;
    }
    let mut unparsable = Unparsable::default();
    for a in &held {
        unparsable.note(a);
    }
    crate::collections::count_unparsable(group, &unparsable);
    let store = match STORE.try_lock() {
        Ok(store) => store,
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        // a pass holds it: the slice goes on, and its headers are written after
        Err(std::sync::TryLockError::WouldBlock) => {
            QUEUED.lock().unwrap_or_else(|e| e.into_inner()).push((group.to_string(), held, now()));
            return kept;
        }
    };
    let count = held.len();
    if let Err(e) = in_store(store, |s| s.record(group, &held, now())) {
        eprintln!("xref: couldnt keep {count} headers of {group}: {e:#}");
    }
    kept
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

/// Every `PASS_EVERY`, drop the headers past the window, then save what has settled.
pub async fn run(db: crate::indexer::Db, stopping: impl Fn() -> bool) {
    while !stopping() {
        let mut slept = Duration::ZERO;
        while slept < PASS_EVERY && !stopping() {
            tokio::time::sleep(Duration::from_secs(1)).await;
            slept += Duration::from_secs(1);
        }
        if stopping() {
            break;
        }
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
                s.prune(t)?;
                s.pass_with(t, &mut unread)
            })
        })
        .await;
        let Found { releases, ack } = match found {
            Ok(Ok(Some(found))) => found,
            Ok(Ok(None)) => continue,
            Ok(Err(e)) => {
                eprintln!("xref: {e:#}");
                continue;
            }
            Err(e) => {
                eprintln!("xref: the pass failed: {e}");
                continue;
            }
        };
        let (count, articles) = (releases.len(), releases.iter().map(|r| r.articles.len()).sum::<usize>());
        // a save goes to one shard's writer, soo each shard's releases go apart
        let mut by_shard: HashMap<usize, Vec<Release>> = HashMap::new();
        for r in releases {
            by_shard.entry(crate::store::shard_of(&r.group)).or_default().push(r);
        }
        let mut saved = true;
        for (_, releases) in by_shard {
            if let Err(e) = db.save(releases).await {
                eprintln!("xref: couldnt save uploads: {e:#}");
                saved = false;
            }
        }
        // what wasnt saved is found again next pass; saving what was saved again changes nothing
        if !saved {
            continue;
        }
        match tokio::task::spawn_blocking(move || with_store(|s| s.ack(ack))).await {
            Ok(Ok(_)) if count > 0 => println!("xref: saved {count} uploads, {articles} articles"),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => eprintln!("xref: couldnt note what was saved: {e:#}"),
            Err(e) => eprintln!("xref: noting what was saved failed: {e}"),
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
        assert!(store.pass_with(later, &mut |_, _| true).unwrap().releases.is_empty(), "a group hasnt read that day");
        let found = store.pass_with(1001 + KEEP_SECS, &mut |_, _| true).unwrap();
        assert_eq!(found.releases.len(), 1, "not past a day");
        // not noted as saved: found again
        assert_eq!(store.pass_with(1002 + KEEP_SECS, &mut |_, _| true).unwrap().releases.len(), 1);
        store.ack(found.ack).unwrap();
        assert!(store.pass_with(1003 + KEEP_SECS, &mut |_, _| true).unwrap().releases.is_empty());
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
        let vol: Vec<_> = joined.articles.iter().filter(|a| a.author == "vol@x.y").collect();
        assert_eq!(vol.len(), 4);
        assert!(vol.iter().all(|a| a.file_index == Some(3) && a.file_total == Some(3)), "its third file");
        assert!(more.iter().any(|r| r.name != first[0].name && r.articles.len() == 12), "{more:?}");
    }

    #[test]
    fn each_hour_the_first_piece_guesses_take_the_next_volumes() {
        let per = HEAD_PROBES / LEADS;
        assert_eq!(guess_round(None, 40, 0), (0, None), "not known when saved: once");
        assert_eq!(guess_round(Some(0), 40, 10), (0, Some(3600)));
        assert_eq!(guess_round(Some(0), 40, 3600), (per, Some(7200)));
        let last = 40usize.div_ceil(per) as i64 - 1;
        assert_eq!(guess_round(Some(0), 40, last * 3600), (last as usize * per, None), "every volume has had a turn");
        assert_eq!(guess_round(Some(0), 3, 0), (0, None), "one turn does them all");
    }
}
