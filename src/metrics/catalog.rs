//! Numbers about what is stored, not about what happened since the start: how
//! many releases, in which categories, groups and sizes, and what the PreDB
//! matched. Only a walk over every release knows them, soo one thread walks
//! the shards every `metrics.catalog_every_secs`, in short reads that pin no
//! WAL. The last result is saved and read back, soo a restart doesnt blank the
//! dashboards until the next walk is through.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::{Connection, params};

use super::names::*;
use super::{Family, label_key};
use crate::category::{self, UNCATEGORIZED};
use crate::db;
use crate::naming::NameSource;
use crate::paths;
use crate::store;

/// rows per read: a read this short leaves the WAL free to start over
const BATCH: i64 = 20_000;
/// breathing room between reads, for the writers
const NAP: Duration = Duration::from_millis(2);
/// how often the cheap numbers are read
const QUICK_EVERY: Duration = Duration::from_secs(30);
/// a walk that failed (a compaction swapped a file away) goes again after this long
const RETRY: Duration = Duration::from_secs(5 * 60);
/// series for groups and release groups: the rest are summed into `(other)`
const TOP_GROUPS: usize = 100;
const TOP_RELEASE_GROUPS: usize = 150;
const OTHER: &str = "(other)";
/// titles held during a walk before asking the PreDB file for their feeds
const MAX_TITLES: usize = 100_000;
/// release group names kept during a walk; past this the ones seen once go
const MAX_RELEASE_GROUPS: usize = 200_000;

/// what the walk publishes, and what is saved of it
const SAVED: &[&Family] = &[
    &CATALOG_RELEASES,
    &CATALOG_BYTES,
    &CATALOG_NAME_SOURCE,
    &CATALOG_CATEGORY,
    &CATALOG_CATEGORY_BYTES,
    &CATALOG_GROUP,
    &CATALOG_GROUP_BYTES,
    &CATALOG_RELEASE_GROUP,
    &CATALOG_VIDEO,
    &CATALOG_SIZE,
    &CATALOG_SIZE_SUM,
    &CATALOG_SIZE_COUNT,
    &CATALOG_PREDB_KIND,
    &CATALOG_PREDB_CATEGORY,
    &CATALOG_PREDB_FEED,
    &CATALOG_PREDB_ROOT,
    &CATALOG_SCAN_AT,
    &CATALOG_SCAN_SECONDS,
];

fn name_source_label(source: Option<i64>) -> &'static str {
    match source {
        None => "posted",
        Some(s) if s == NameSource::Cleared as i64 => "cleared",
        Some(s) if s == NameSource::PostedFile as i64 => "posted_file",
        Some(s) if s == NameSource::NamingRegex as i64 => "naming_regex",
        Some(s) if s == NameSource::InlineBody as i64 => "inline_body",
        Some(s) if s == NameSource::Nfo as i64 => "nfo",
        Some(s) if s == NameSource::ArchiveInner as i64 => "archive_inner",
        Some(s) if s == NameSource::Par2 as i64 => "par2",
        Some(s) if s == NameSource::PreDb as i64 => "predb",
        Some(_) => "other",
    }
}

/// for a name source of `source`, the label of `atlas_deobfuscated_total` and of the walk
pub fn name_source_name(source: u8) -> &'static str {
    name_source_label(Some(i64::from(source)))
}

/// how `predb_match.kind` matched
fn predb_kind_label(kind: i64) -> &'static str {
    use crate::predb::matcher::Kind;
    match kind {
        k if k == Kind::Hash as i64 => "hash",
        k if k == Kind::Exact as i64 => "exact",
        k if k == Kind::Normalized as i64 => "normalized",
        k if k == Kind::InnerFile as i64 => "inner_file",
        _ => "other",
    }
}

/// `TV/HD`, or the id for one outside the table.
fn category_label(id: i64) -> String {
    if id == UNCATEGORIZED {
        return "Uncategorized".into();
    }
    category::label(id).unwrap_or_else(|| id.to_string())
}

fn is_video_root(root: i64) -> bool {
    root == 2000 || root == 5000
}

/// Words that follow a dash in a scene name without being who released it.
const NOT_A_GROUP: &[&str] = &[
    "ac3", "avc", "bluray", "dd5", "dl", "dts", "dv", "h264", "h265", "hd", "hdr", "hdtv", "hevc", "sd", "uhd", "web",
    "webdl", "webrip", "x264", "x265", "xvid", "480p", "576p", "720p", "1080i", "1080p", "2160p", "4k",
];

/// The `GROUP` of `Some.Movie.2024.1080p.WEB-DL.x264-GROUP`, once a file
/// extension or a `[tag]` after it is taken off. None for a name not built like one.
pub fn release_group(name: &str) -> Option<&str> {
    let mut name = name.trim();
    while let Some(open) = name.strip_suffix(']').and_then(|n| n.rfind('[')) {
        name = name[..open].trim_end();
    }
    if let Some((stem, ext)) = name.rsplit_once('.')
        && (2..=4).contains(&ext.len())
        && ["mkv", "mp4", "avi", "m4v", "ts", "m2ts", "iso", "nzb", "nfo", "rar", "par2", "sfv"]
            .iter()
            .any(|e| ext.eq_ignore_ascii_case(e))
    {
        name = stem;
    }
    let (head, group) = name.rsplit_once('-')?;
    let separators = head.bytes().filter(|b| matches!(b, b'.' | b' ' | b'_')).count();
    let looks_right = separators >= 2
        && (2..=20).contains(&group.len())
        && group.bytes().all(|b| b.is_ascii_alphanumeric())
        && group.bytes().any(|b| b.is_ascii_alphabetic())
        // a hash or a random tail
        && !(group.len() >= 12 && group.bytes().all(|b| b.is_ascii_hexdigit()))
        && !NOT_A_GROUP.iter().any(|w| group.eq_ignore_ascii_case(w));
    looks_right.then_some(group)
}

#[derive(Default)]
struct Sizes {
    /// own count per bound of `SIZES`, with one more past the last
    buckets: Vec<u64>,
    sum: f64,
    count: u64,
}

#[derive(Default)]
struct GroupTally {
    blocked: bool,
    releases: u64,
    bytes: u64,
}

/// One walk's running totals.
#[derive(Default)]
pub struct Tally {
    pub rows: u64,
    total: u64,
    bytes: u64,
    complete: u64,
    nzb_ready: u64,
    named: u64,
    still_obfuscated: u64,
    predb_matched: u64,
    name_sources: BTreeMap<&'static str, u64>,
    /// category id -> (releases, bytes)
    categories: BTreeMap<i64, (u64, u64)>,
    groups: HashMap<String, GroupTally>,
    release_groups: HashMap<String, u64>,
    /// releases of the names dropped to stay under the limit
    release_groups_evicted: u64,
    video_with_group: u64,
    video_without: u64,
    sizes: BTreeMap<i64, Sizes>,
    predb_kinds: BTreeMap<i64, u64>,
    predb_categories: BTreeMap<String, u64>,
    predb_titles: HashMap<i64, u64>,
    /// root -> (matched, not matched)
    predb_roots: BTreeMap<i64, (u64, u64)>,
    feeds: BTreeMap<String, u64>,
}

/// One row of the walk.
pub struct Row<'a> {
    pub group: &'a str,
    pub size: i64,
    pub complete: bool,
    pub parts: i64,
    pub obfuscated: bool,
    pub named: bool,
    pub name_source: Option<i64>,
    pub name: &'a str,
    pub category: Option<i64>,
    pub predb: Option<(i64, Option<&'a str>, i64)>,
}

impl Tally {
    pub fn add(&mut self, row: &Row<'_>) {
        self.rows += 1;
        let group = match self.groups.get_mut(row.group) {
            Some(group) => group,
            None => self.groups.entry(row.group.to_string()).or_insert_with(|| GroupTally {
                blocked: crate::filters::group_blocked(row.group),
                ..GroupTally::default()
            }),
        };
        if group.blocked {
            return;
        }
        let size = row.size.max(0) as u64;
        group.releases += 1;
        group.bytes += size;

        self.total += 1;
        self.bytes += size;
        self.complete += u64::from(row.complete);
        self.nzb_ready += u64::from(row.parts > 0);
        self.named += u64::from(row.named);
        self.still_obfuscated += u64::from(row.obfuscated && !row.named);
        *self.name_sources.entry(name_source_label(row.name_source)).or_default() += 1;

        let category = row.category.unwrap_or(UNCATEGORIZED);
        let root = category::root_of(category);
        let in_category = self.categories.entry(category).or_default();
        in_category.0 += 1;
        in_category.1 += size;

        let sizes = self.sizes.entry(root).or_default();
        if sizes.buckets.is_empty() {
            sizes.buckets = vec![0; SIZES.len() + 1];
        }
        let at = SIZES.iter().position(|b| size as f64 <= *b).unwrap_or(SIZES.len());
        sizes.buckets[at] += 1;
        sizes.sum += size as f64;
        sizes.count += 1;

        let in_root = self.predb_roots.entry(root).or_default();
        if let Some((kind, predb_category, title)) = row.predb {
            in_root.0 += 1;
            self.predb_matched += 1;
            *self.predb_kinds.entry(kind).or_default() += 1;
            *self.predb_categories.entry(predb_category.unwrap_or("none").to_string()).or_default() += 1;
            *self.predb_titles.entry(title).or_default() += 1;
        } else {
            in_root.1 += 1;
        }

        if is_video_root(root) && !(row.obfuscated && !row.named) {
            match release_group(row.name) {
                Some(found) => {
                    self.video_with_group += 1;
                    self.count_release_group(found, MAX_RELEASE_GROUPS);
                }
                None => self.video_without += 1,
            }
        }
    }

    /// One more release of `found`. At `limit` names the smaller half go into `(other)`: each pass frees
    /// at least half the room, soo the passes stay rare, however even the counts are.
    fn count_release_group(&mut self, found: &str, limit: usize) {
        if let Some(n) = self.release_groups.get_mut(found) {
            *n += 1;
            return;
        }
        if self.release_groups.len() >= limit {
            let mut counts: Vec<u64> = self.release_groups.values().copied().collect();
            let middle = counts.len() / 2;
            let keep_above = *counts.select_nth_unstable(middle).1;
            let mut evicted = 0;
            self.release_groups.retain(|_, n| {
                let keep = *n > keep_above;
                if !keep {
                    evicted += *n;
                }
                keep
            });
            self.release_groups_evicted += evicted;
        }
        self.release_groups.insert(found.to_string(), 1);
    }

    /// Put the totals into their metrics.
    pub fn publish(&self, took: Duration) {
        let one = |name: &str, value: &str| label_key(&[(name, value)]);
        let states = [
            ("total", self.total),
            ("complete", self.complete),
            ("nzb_ready", self.nzb_ready),
            ("named", self.named),
            ("still_obfuscated", self.still_obfuscated),
            ("predb_matched", self.predb_matched),
        ];
        CATALOG_RELEASES.replace(states.map(|(state, n)| (one("state", state), n as f64)));
        CATALOG_BYTES.replace([(String::new(), self.bytes as f64)]);
        CATALOG_NAME_SOURCE.replace(self.name_sources.iter().map(|(s, n)| (one("source", s), *n as f64)));

        let category_key = |id: i64| {
            let root = category_label(category::root_of(id));
            label_key(&[("category", &category_label(id)), ("root", &root)])
        };
        CATALOG_CATEGORY.replace(self.categories.iter().map(|(id, (n, _))| (category_key(*id), *n as f64)));
        CATALOG_CATEGORY_BYTES.replace(self.categories.iter().map(|(id, (_, b))| (category_key(*id), *b as f64)));

        let mut groups: Vec<(&String, &GroupTally)> =
            self.groups.iter().filter(|(_, g)| !g.blocked && g.releases > 0).collect();
        groups.sort_by_key(|(_, g)| std::cmp::Reverse(g.bytes));
        let (top, rest) = groups.split_at(groups.len().min(TOP_GROUPS));
        let rest_releases: u64 = rest.iter().map(|(_, g)| g.releases).sum();
        let rest_bytes: u64 = rest.iter().map(|(_, g)| g.bytes).sum();
        let tail = |n: u64| (!rest.is_empty()).then(|| (one("group", OTHER), n as f64));
        CATALOG_GROUP.replace(top.iter().map(|(g, t)| (one("group", g), t.releases as f64)).chain(tail(rest_releases)));
        CATALOG_GROUP_BYTES.replace(top.iter().map(|(g, t)| (one("group", g), t.bytes as f64)).chain(tail(rest_bytes)));

        let mut release_groups: Vec<(&String, &u64)> = self.release_groups.iter().collect();
        release_groups.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        let (top, rest) = release_groups.split_at(release_groups.len().min(TOP_RELEASE_GROUPS));
        let rest_total: u64 = rest.iter().map(|(_, n)| **n).sum::<u64>() + self.release_groups_evicted;
        CATALOG_RELEASE_GROUP.replace(
            top.iter()
                .map(|(g, n)| (one("release_group", g), **n as f64))
                .chain((rest_total > 0).then(|| (one("release_group", OTHER), rest_total as f64))),
        );
        CATALOG_VIDEO.replace([
            (one("release_group", "found"), self.video_with_group as f64),
            (one("release_group", "none"), self.video_without as f64),
        ]);

        let mut size_rows = Vec::new();
        for (root, sizes) in &self.sizes {
            let root = category_label(*root);
            let mut below = 0;
            for (bound, own) in SIZES.iter().zip(&sizes.buckets) {
                below += own;
                size_rows.push((label_key(&[("root", &root), ("le", &bound.to_string())]), below as f64));
            }
            size_rows.push((label_key(&[("root", &root), ("le", "+Inf")]), sizes.count as f64));
        }
        CATALOG_SIZE.replace(size_rows);
        CATALOG_SIZE_SUM.replace(self.sizes.iter().map(|(r, s)| (one("root", &category_label(*r)), s.sum)));
        CATALOG_SIZE_COUNT.replace(self.sizes.iter().map(|(r, s)| (one("root", &category_label(*r)), s.count as f64)));

        CATALOG_PREDB_KIND
            .replace(self.predb_kinds.iter().map(|(k, n)| (one("kind", predb_kind_label(*k)), *n as f64)));
        CATALOG_PREDB_CATEGORY.replace(self.predb_categories.iter().map(|(c, n)| (one("category", c), *n as f64)));
        CATALOG_PREDB_FEED.replace(self.feeds.iter().map(|(f, n)| (one("feed", f), *n as f64)));
        CATALOG_PREDB_ROOT.replace(self.predb_roots.iter().flat_map(|(root, (yes, no))| {
            let root = category_label(*root);
            [
                (label_key(&[("root", &root), ("matched", "yes")]), *yes as f64),
                (label_key(&[("root", &root), ("matched", "no")]), *no as f64),
            ]
        }));

        CATALOG_SCAN_AT.replace([(String::new(), chrono::Utc::now().timestamp() as f64)]);
        CATALOG_SCAN_SECONDS.replace([(String::new(), took.as_secs_f64())]);
    }

    /// Which feed brought the titles matched, from the PreDB file next to the database.
    fn read_feeds(&mut self, main: &Path) {
        let path = main.with_file_name(format!(
            "{}.predb.db",
            main.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "atlas".into())
        ));
        let titles = std::mem::take(&mut self.predb_titles);
        if !path.exists() || titles.is_empty() {
            return;
        }
        let read = || -> rusqlite::Result<BTreeMap<String, u64>> {
            let conn = crate::predb::db::open_reader(&path)?;
            let mut stmt = conn.prepare_cached(
                "select coalesce(f.key, t.source) from titles t left join feeds f on f.source = t.source where t.id = ?1",
            )?;
            let mut feeds = BTreeMap::new();
            for (title, n) in &titles {
                let found: rusqlite::Result<String> = stmt.query_row([title], |r| {
                    r.get_ref(0).map(|v| match v.as_str() {
                        Ok(key) => key.to_string(),
                        Err(_) => "unknown".to_string(),
                    })
                });
                let feed = found.unwrap_or_else(|_| "unknown".into());
                *feeds.entry(feed).or_default() += n;
            }
            Ok(feeds)
        };
        match read() {
            Ok(feeds) => {
                for (feed, n) in feeds {
                    *self.feeds.entry(feed).or_default() += n;
                }
            }
            Err(e) => println!("metrics: couldnt read the predb feeds: {e}"),
        }
    }
}

fn has_table(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row("select exists (select 1 from sqlite_master where type = 'table' and name = ?1)", [table], |r| {
        r.get(0)
    })
}

/// A shard's rows after `after`, all counted: where the walk has got to, None past the last.
fn read_batch(conn: &Connection, after: i64, limit: i64, tally: &mut Tally) -> rusqlite::Result<Option<i64>> {
    // a shard from before the stage ran has neither
    let category = if has_table(conn, "release_category")? {
        ("c.category", "left join release_category c on c.release_id = r.id")
    } else {
        ("null", "")
    };
    let predb = if has_table(conn, "predb_match")? {
        ("p.kind, p.category, p.title_id", "left join predb_match p on p.release_id = r.id")
    } else {
        ("null, null, null", "")
    };
    let sql = format!(
        "select r.id, r.group_name, coalesce(r.size, 0), coalesce(r.complete, 0), coalesce(r.parts, 0),
                coalesce(r.is_obfuscated, 0), r.display_name is not null, r.name_source,
                coalesce(r.display_name, r.name), {}, {}
         from releases r {} {} where r.id > ?1 order by r.id limit ?2",
        category.0, predb.0, category.1, predb.1
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params![after, limit])?;
    let (mut last, mut seen) = (after, 0);
    while let Some(r) = rows.next()? {
        last = r.get(0)?;
        seen += 1;
        let kind: Option<i64> = r.get(10)?;
        let predb = match kind {
            Some(kind) => Some((kind, r.get_ref(11)?.as_str_or_null()?, r.get::<_, Option<i64>>(12)?.unwrap_or(0))),
            None => None,
        };
        tally.add(&Row {
            group: r.get_ref(1)?.as_str_or_null()?.unwrap_or(""),
            size: r.get(2)?,
            complete: r.get::<_, i64>(3)? != 0,
            parts: r.get(4)?,
            obfuscated: r.get::<_, i64>(5)? != 0,
            named: r.get::<_, i64>(6)? != 0,
            name_source: r.get(7)?,
            name: r.get_ref(8)?.as_str_or_null()?.unwrap_or(""),
            category: r.get(9)?,
            predb,
        });
    }
    Ok((seen == limit).then_some(last))
}

/// One walk over every shard. None: stopped part way.
pub fn scan(main: &Path, stop: &AtomicBool) -> rusqlite::Result<Option<Tally>> {
    let mut tally = Tally::default();
    for path in &store::shard_paths(main) {
        let mut after = i64::MIN;
        loop {
            if stop.load(Relaxed) {
                return Ok(None);
            }
            // a compaction has it out of the way for now: totals without it are wrong, soo no totals
            if !path.exists() {
                return Err(rusqlite::Error::InvalidPath(path.clone()));
            }
            // a connection per read holds nothing open for a compaction to wait out
            let conn = db::open_shard(path)?;
            conn.execute_batch("pragma query_only = on")?;
            let next = read_batch(&conn, after, BATCH, &mut tally)?;
            drop(conn);
            if tally.predb_titles.len() >= MAX_TITLES {
                tally.read_feeds(main);
            }
            CATALOG_SCAN_ROWS.set(&[("state", "running")], tally.rows as f64);
            match next {
                Some(last) => after = last,
                None => break,
            }
            thread::sleep(NAP);
        }
    }
    tally.read_feeds(main);
    Ok(Some(tally))
}

/// Save what the walk published.
fn save(path: &Path) -> anyhow::Result<()> {
    let rows: BTreeMap<&str, Vec<(String, f64)>> = SAVED.iter().map(|f| (f.name, f.rows())).collect();
    crate::atomic::write_atomic(path, serde_json::to_string(&rows)?)?;
    Ok(())
}

/// Bring back a saved walk. When it finished, if there was one.
fn restore(path: &Path) -> Option<i64> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut rows: BTreeMap<String, Vec<(String, f64)>> = serde_json::from_str(&text).ok()?;
    for family in SAVED {
        if let Some(found) = rows.remove(family.name) {
            family.replace(found);
        }
    }
    CATALOG_SCAN_AT.rows().first().map(|(_, at)| *at as i64)
}

/// The numbers that take no walk.
fn read_quick(main: &Path) {
    let stem = main.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "atlas".into());
    let named = |name: String| main.with_file_name(format!("{stem}.{name}.db"));
    let mut files: Vec<(String, PathBuf)> = vec![("main".into(), main.to_path_buf())];
    files.extend(store::shard_paths(main).into_iter().enumerate().map(|(i, p)| (format!("s{i}"), p)));
    files.push(("predb".into(), named("predb".into())));
    files.push(("media".into(), named("media".into())));
    for (file, path) in &files {
        let wal = PathBuf::from(format!("{}-wal", path.display()));
        for (kind, path) in [("db", path), ("wal", &wal)] {
            let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            DB_BYTES.set(&[("file", file), ("kind", kind)], bytes as f64);
        }
    }

    let read = || -> rusqlite::Result<()> {
        let conn = db::open()?;
        conn.execute_batch("pragma query_only = on")?;
        let (releases, articles) = store::totals(&conn)?;
        DB_TOTALS.set(&[("kind", "releases")], releases as f64);
        DB_TOTALS.set(&[("kind", "articles")], articles as f64);
        if has_table(&conn, "backfill_chunks")? {
            let hour_ago = chrono::Utc::now().timestamp() - 3600;
            let (done, total, recent): (i64, i64, i64) = conn.query_row(
                "select coalesce(sum(state = 2), 0), count(*), coalesce(sum(state = 2 and done_at > ?1), 0) from backfill_chunks",
                [hour_ago],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            BACKFILL_CHUNKS.set(&[("state", "done")], done as f64);
            BACKFILL_CHUNKS.set(&[("state", "total")], total as f64);
            BACKFILL_CHUNKS.set(&[("state", "done_last_hour")], recent as f64);
            let (groups, splitting): (i64, i64) = conn.query_row(
                "select count(*), coalesce(sum(pending > 0), 0) from
                 (select grp, sum(state != 2) as pending from backfill_chunks group by grp)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            BACKFILL_GROUPS.set(&[("state", "all")], groups as f64);
            BACKFILL_GROUPS.set(&[("state", "splitting")], splitting as f64);
        }
        Ok(())
    };
    if let Err(e) = read() {
        println!("metrics: couldnt read the database totals: {e}");
    }
}

fn nap(how_long: Duration, stop: &AtomicBool) {
    let until = Instant::now() + how_long;
    while !stop.load(Relaxed) && Instant::now() < until {
        thread::sleep(Duration::from_millis(250).min(until - Instant::now()));
    }
}

fn cache_path() -> PathBuf {
    paths::app_dir().join("metrics_catalog.json")
}

/// The two threads, for as long as `stop` isnt set.
pub fn spawn(every: Duration, stop: Arc<AtomicBool>) {
    let main = paths::database();
    let (quick_main, quick_stop) = (main.clone(), stop.clone());
    let _ = thread::Builder::new().name("atlas-metrics-quick".into()).spawn(move || {
        while !quick_stop.load(Relaxed) {
            read_quick(&quick_main);
            nap(QUICK_EVERY, &quick_stop);
        }
    });
    let _ =
        thread::Builder::new().name("atlas-metrics-catalog".into()).spawn(move || walk_forever(&main, every, &stop));
}

fn walk_forever(main: &Path, every: Duration, stop: &AtomicBool) {
    let saved_at = restore(&cache_path());
    let age = saved_at.map(|at| (chrono::Utc::now().timestamp() - at).max(0) as u64);
    // no sooner than its interval after the last one, however often atlas restarts
    nap(age.map_or(Duration::ZERO, |age| every.saturating_sub(Duration::from_secs(age))), stop);
    while !stop.load(Relaxed) {
        let started = Instant::now();
        let span = super::span("metrics.catalog_scan");
        let wait = match scan(main, stop) {
            Ok(Some(tally)) => {
                tally.publish(started.elapsed());
                CATALOG_SCAN_ROWS.set(&[("state", "last")], tally.rows as f64);
                if let Err(e) = save(&cache_path()) {
                    println!("metrics: couldnt save the catalog numbers: {e:#}");
                }
                every
            }
            Ok(None) => return,
            Err(e) => {
                span.failed();
                println!("metrics: catalog walk stopped, trying again in {}m: {e}", RETRY.as_secs() / 60);
                RETRY
            }
        };
        drop(span);
        nap(wait, stop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_group_reads_scene_names() {
        for (name, want) in [
            ("Some.Movie.2024.1080p.WEB-DL.x264-GROUP", Some("GROUP")),
            ("Some.Movie.2024.1080p.WEB-DL.x264-GROUP.mkv", Some("GROUP")),
            ("Some Show S01E02 1080p WEB h264-NTb [rartv]", Some("NTb")),
            ("Some.Movie.2024.1080p.WEB-DL", None),
            ("Some.Movie.2024.2160p-x265", None),
            ("short-name", None),
            ("Some.Movie.2024.1080p-0123456789abcdef", None),
            ("Some.Movie.2024.1080p-1234", None),
            ("", None),
        ] {
            assert_eq!(release_group(name), want, "{name}");
        }
    }

    #[test]
    fn release_groups_past_the_limit_add_up_under_other() {
        let mut tally = Tally::default();
        // every name seen twice, soo no count is smaller than another
        for name in ["a", "b", "c", "d"] {
            tally.count_release_group(name, 4);
            tally.count_release_group(name, 4);
        }
        tally.count_release_group("e", 4);
        assert!(tally.release_groups.len() <= 4, "{:?}", tally.release_groups);
        assert_eq!(tally.release_groups["e"], 1);
        let kept: u64 = tally.release_groups.values().sum();
        assert_eq!(kept + tally.release_groups_evicted, 9, "nothing is lost");
        assert!(tally.release_groups_evicted > 0);
    }

    #[test]
    fn a_saved_walk_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saved.json");
        CATALOG_GROUP.replace([(label_key(&[("group", "a.b.c")]), 7.0)]);
        CATALOG_SCAN_AT.replace([(String::new(), 1234.0)]);
        save(&path).unwrap();
        CATALOG_GROUP.replace([]);
        CATALOG_SCAN_AT.replace([]);
        assert_eq!(restore(&path), Some(1234));
        assert_eq!(CATALOG_GROUP.rows(), vec![(label_key(&[("group", "a.b.c")]), 7.0)]);
        assert_eq!(restore(&dir.path().join("missing.json")), None);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(restore(&path), None, "a damaged file is no file");
    }

    #[test]
    fn every_name_source_has_a_label() {
        for (source, label) in [
            (NameSource::Nfo, "nfo"),
            (NameSource::Par2, "par2"),
            (NameSource::PreDb, "predb"),
            (NameSource::ArchiveInner, "archive_inner"),
        ] {
            assert_eq!(name_source_name(source.value()), label);
        }
        assert_eq!(name_source_label(Some(77)), "other");
    }
}
