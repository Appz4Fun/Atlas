//! The stats page, `/ui/stats`: how each part of the indexer is keeping up, what the shards hold, and what came in
//! over the last day. The live numbers are the ones Grafana graphs, read off the indexer's own `/metrics` listener
//! (`metrics.indexer_port`), sampled every `SAMPLE_EVERY` while someone looks, soo rates are over the same kind of
//! window as a `rate(...[5m])`. The net lane's per-shard rates come from the indexer's `stats.json`, which the
//! release page's Processing panel reads too. What only the shards know (the last day by category, the latest PreDB
//! matches) is read with indexed queries, each shard on its own thread: the matches are kept for `STORED_TTL`, the
//! day for `DAY_TTL`, and a busy shard's day is broken down from a sample (`DAY_SAMPLE`).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use crate::api::{ApiResponse, escape};
use crate::web::{html, tag, thousands};

/// How often the indexer's metrics are read while the page is being looked at.
const SAMPLE_EVERY: Duration = Duration::from_secs(15);
/// Rates are over about this long, like Grafana's `[5m]`.
const RATE_WINDOW_SECS: f64 = 300.0;
/// Under this, a rate isn't shown yet.
const MIN_WINDOW_SECS: f64 = 30.0;
/// Samples older than this are dropped.
const KEEP_SECS: f64 = RATE_WINDOW_SECS + 120.0;
/// The sampler stops when nobody has looked for this long, and starts again on the next look.
const IDLE_STOP: Duration = Duration::from_secs(30 * 60);
/// Reading `/metrics` gives up after this long.
const SCRAPE_TIMEOUT: Duration = Duration::from_secs(5);
/// The latest PreDB matches are read again after this long.
const STORED_TTL: Duration = Duration::from_secs(60);
/// The last day's breakdown, after this long: on a busy shard millions of releases were posted in a day.
const DAY_TTL: Duration = Duration::from_secs(5 * 60);
/// Releases per shard the breakdown looks at; past it, it looks at every nth (by id) and scales up. The total is
/// always exact: it comes from the posted-date index alone.
const DAY_SAMPLE: i64 = 20_000;
/// stats.json older than this: the indexer isn't running.
const STALE_SECS: f64 = 120.0;
/// Latest PreDB matches listed.
const PREDB_RECENT: usize = 25;
/// The browser asks for the live part again this often.
const REFRESH_SECS: u32 = 30;

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

// ---------------------------------------------------------------------------------------------------------------
// reading /metrics

type Labels = Vec<(String, String)>;

/// One read of `/metrics`: every series but histogram buckets, by metric name.
#[derive(Debug, Default)]
struct Scrape {
    at: f64,
    series: HashMap<String, Vec<(Labels, f64)>>,
}

fn label<'a>(labels: &'a Labels, key: &str) -> Option<&'a str> {
    labels.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// `{a="1",b="x\"y"}` -> [(a, 1), (b, x"y)]. None when it isn't well formed.
fn parse_labels(text: &str) -> Option<Labels> {
    let mut out = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        let eq = rest.find('=')?;
        let key = rest[..eq].trim().trim_start_matches(',').trim().to_string();
        rest = rest[eq + 1..].trim_start().strip_prefix('"')?;
        let mut value = String::new();
        let mut chars = rest.char_indices();
        let end = loop {
            let (i, c) = chars.next()?;
            match c {
                '\\' => match chars.next()?.1 {
                    'n' => value.push('\n'),
                    other => value.push(other),
                },
                '"' => break i,
                c => value.push(c),
            }
        };
        out.push((key, value));
        rest = rest[end + 1..].trim_start().trim_start_matches(',').trim_start();
    }
    Some(out)
}

/// The Prometheus text format, as atlas writes it. Histogram buckets are left out: nothing here reads them.
fn parse(text: &str, at: f64) -> Scrape {
    let mut series: HashMap<String, Vec<(Labels, f64)>> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (head, value) = match line.rfind('}') {
            Some(close) => (&line[..=close], line[close + 1..].trim()),
            None => match line.split_once(char::is_whitespace) {
                Some((name, value)) => (name, value.trim()),
                None => continue,
            },
        };
        let Some(value) = value.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()) else { continue };
        let (name, labels) = match head.split_once('{') {
            Some((name, labels)) => match parse_labels(labels.trim_end_matches('}')) {
                Some(labels) => (name, labels),
                None => continue,
            },
            None => (head, Vec::new()),
        };
        if name.ends_with("_bucket") {
            continue;
        }
        series.entry(name.to_string()).or_default().push((labels, value));
    }
    Scrape { at, series }
}

impl Scrape {
    fn rows(&self, name: &str) -> &[(Labels, f64)] {
        self.series.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The sum of a metric's series that `keep` takes. None when there are none.
    fn sum_where(&self, name: &str, keep: impl Fn(&Labels) -> bool) -> Option<f64> {
        let mut found = None;
        for (labels, value) in self.rows(name) {
            if keep(labels) {
                *found.get_or_insert(0.0) += value;
            }
        }
        found
    }

    fn sum(&self, name: &str) -> Option<f64> {
        self.sum_where(name, |_| true)
    }

    /// The series with `key` = `value`.
    fn with(&self, name: &str, key: &str, value: &str) -> Option<f64> {
        self.sum_where(name, |l| label(l, key) == Some(value))
    }
}

/// The newest read, and the oldest kept inside the rate window to measure from.
#[derive(Clone, Default)]
struct Window {
    now: Option<Arc<Scrape>>,
    then: Option<Arc<Scrape>>,
}

impl Window {
    fn of(samples: &VecDeque<Arc<Scrape>>) -> Window {
        let now = samples.back().cloned();
        let then = now.as_ref().and_then(|n| {
            samples.iter().find(|s| n.at - s.at <= RATE_WINDOW_SECS + 1.0 && n.at - s.at >= MIN_WINDOW_SECS).cloned()
        });
        Window { now, then }
    }

    fn secs(&self) -> Option<f64> {
        Some(self.now.as_ref()?.at - self.then.as_ref()?.at)
    }

    /// Per second, over the window, of the counter series `keep` takes. A series that went down started over with
    /// its process, and counts from zero; one that wasn't there at the start of the window isn't counted.
    fn rate_where(&self, name: &str, keep: impl Fn(&Labels) -> bool) -> Option<f64> {
        let (now, then) = (self.now.as_ref()?, self.then.as_ref()?);
        let secs = now.at - then.at;
        if secs < MIN_WINDOW_SECS {
            return None;
        }
        let before: HashMap<&Labels, f64> = then.rows(name).iter().map(|(l, v)| (l, *v)).collect();
        let mut total = None;
        for (labels, value) in now.rows(name) {
            if !keep(labels) {
                continue;
            }
            let Some(old) = before.get(labels) else { continue };
            let delta = if value >= old { value - old } else { *value };
            *total.get_or_insert(0.0) += delta;
        }
        total.map(|t| t / secs)
    }

    fn per_minute(&self, name: &str, keep: impl Fn(&Labels) -> bool) -> Option<f64> {
        self.rate_where(name, keep).map(|r| r * 60.0)
    }

    /// A gauge's change per second over the window (`deriv`, from two points).
    fn change(&self, name: &str) -> Option<f64> {
        let (now, then) = (self.now.as_ref()?, self.then.as_ref()?);
        let secs = now.at - then.at;
        if secs < MIN_WINDOW_SECS {
            return None;
        }
        Some((now.sum(name)? - then.sum(name)?) / secs)
    }

    fn function_per_minute(&self, function: &str) -> Option<f64> {
        self.per_minute("atlas_function_duration_seconds_count", |l| label(l, "function") == Some(function))
    }
}

struct Sampler {
    samples: VecDeque<Arc<Scrape>>,
    last_error: Option<String>,
    running: bool,
    last_view: Option<Instant>,
}

static SAMPLER: Mutex<Sampler> =
    Mutex::new(Sampler { samples: VecDeque::new(), last_error: None, running: false, last_view: None });

/// Where the indexer serves its metrics, or why it doesn't.
fn metrics_url() -> Result<String, String> {
    let m = crate::config::metrics_config();
    if !m.enabled {
        return Err("metrics are off: set `enabled = true` under `[metrics]` in config.toml".into());
    }
    let host = match m.host.as_str() {
        "" | "0.0.0.0" | "::" | "[::]" => "127.0.0.1".to_string(),
        h if h.contains(':') && !h.starts_with('[') => format!("[{h}]"),
        h => h.to_string(),
    };
    Ok(format!("http://{host}:{}/metrics", m.indexer_port))
}

fn scrape(url: &str) -> Result<Scrape, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(SCRAPE_TIMEOUT)).build().into();
    let mut response = agent.get(url).call().map_err(|e| e.to_string())?;
    let text = response.body_mut().with_config().limit(64 * 1024 * 1024).read_to_string().map_err(|e| e.to_string())?;
    Ok(parse(&text, now()))
}

fn sample_once() {
    let read = metrics_url().and_then(|url| scrape(&url));
    let mut s = SAMPLER.lock().unwrap();
    match read {
        Ok(scrape) => {
            let at = scrape.at;
            s.samples.push_back(Arc::new(scrape));
            while s.samples.front().is_some_and(|f| at - f.at > KEEP_SECS) {
                s.samples.pop_front();
            }
            s.last_error = None;
        }
        Err(e) => s.last_error = Some(e),
    }
}

/// The samples so far, starting the sampler if it isn't running, and reading once right away when it has nothing.
fn window() -> (Window, Option<String>) {
    let start = {
        let mut s = SAMPLER.lock().unwrap();
        s.last_view = Some(Instant::now());
        let start = !s.running;
        s.running = true;
        start
    };
    if start {
        let fresh = SAMPLER.lock().unwrap().samples.back().is_none_or(|b| now() - b.at > SAMPLE_EVERY.as_secs_f64());
        if fresh {
            sample_once();
        }
        let spawned = thread::Builder::new().name("atlas-web-stats".into()).spawn(|| {
            loop {
                thread::sleep(SAMPLE_EVERY);
                {
                    let mut s = SAMPLER.lock().unwrap();
                    if s.last_view.is_none_or(|v| v.elapsed() > IDLE_STOP) {
                        s.running = false;
                        return;
                    }
                }
                sample_once();
            }
        });
        if spawned.is_err() {
            SAMPLER.lock().unwrap().running = false;
        }
    }
    let s = SAMPLER.lock().unwrap();
    (Window::of(&s.samples), s.last_error.clone())
}

// ---------------------------------------------------------------------------------------------------------------
// what the shards hold

/// Releases of one category posted in the last day.
#[derive(Clone, Debug, Default, PartialEq)]
struct DayRow {
    count: i64,
    complete: i64,
    bytes: i64,
}

#[derive(Clone, Debug, PartialEq)]
struct PredbRow {
    id: i64,
    name: String,
    kind: i64,
    category: Option<String>,
    matched_at: i64,
    pre_at: Option<i64>,
    size: Option<i64>,
    complete: bool,
    atlas_category: i64,
}

#[derive(Debug, Default)]
struct Stored {
    /// when it was read, unix seconds
    at: f64,
    /// how long the reads took
    secs: f64,
    /// by stored category, scaled up where a shard was sampled; uncategorized is `UNCATEGORIZED`
    day: BTreeMap<i64, DayRow>,
    /// releases posted in the day, counted exactly
    day_total: i64,
    /// the widest sample step a shard used: 1 when every release was looked at
    day_every: i64,
    /// when the day was read, and how long that took
    day_at: f64,
    day_secs: f64,
    /// shards that couldn't answer the last day's count: no posted-date index there yet
    day_unindexed: usize,
    predb: Vec<PredbRow>,
    errors: Vec<String>,
}

static STORED: Mutex<Option<(Instant, Arc<Stored>)>> = Mutex::new(None);
/// held while the shards are being read, soo two looks at once read them once
static READING: Mutex<()> = Mutex::new(());

fn has_table(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row("select 1 from sqlite_master where type = 'table' and name = ?1", [table], |_| Ok(()))
        .optional()
        .map(|f| f.is_some())
}

fn has_index(conn: &Connection, index: &str) -> rusqlite::Result<bool> {
    conn.query_row("select 1 from sqlite_master where type = 'index' and name = ?1", [index], |_| Ok(()))
        .optional()
        .map(|f| f.is_some())
}

/// One shard's releases posted since a time: the exact total, and by category from every `every`th of them.
#[derive(Debug, Default, PartialEq)]
struct Day {
    total: i64,
    every: i64,
    /// scaled up to the total
    rows: BTreeMap<i64, DayRow>,
}

/// One shard's last day. None when the shard has no index on `posted_date`: without it the count would read every
/// release. The total is counted off the index alone; the breakdown needs each release's row and category, soo on a
/// busy shard it looks at one release in `every` (by id, which says nothing of category or size) and scales up.
fn day_of(conn: &Connection, since: &str, sample: i64) -> rusqlite::Result<Option<Day>> {
    if !has_index(conn, crate::field_search::POSTED_INDEX)? {
        return Ok(None);
    }
    let total: i64 = conn.query_row(
        "select count(*) from releases indexed by idx_release_posted where posted_date >= ?1",
        [since],
        |r| r.get(0),
    )?;
    let sample = sample.max(1);
    let every = ((total + sample - 1) / sample).max(1);
    let shards = crate::store::SHARDS as i64;
    let categorized = has_table(conn, "release_category")?;
    let sql = if categorized {
        format!(
            "select c.category, count(*), coalesce(sum(r.complete = 1), 0), coalesce(sum(r.size), 0)
             from releases r indexed by idx_release_posted left join release_category c on c.release_id = r.id
             where r.posted_date >= ?1 and (r.id / {shards}) % ?2 = 0 group by 1"
        )
    } else {
        format!(
            "select null, count(*), coalesce(sum(complete = 1), 0), coalesce(sum(size), 0)
             from releases indexed by idx_release_posted where posted_date >= ?1 and (id / {shards}) % ?2 = 0"
        )
    };
    let mut sampled = BTreeMap::new();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![since, every], |r| {
        Ok((r.get::<_, Option<i64>>(0)?, DayRow { count: r.get(1)?, complete: r.get(2)?, bytes: r.get(3)? }))
    })?;
    for row in rows {
        let (category, day) = row?;
        if day.count == 0 {
            continue;
        }
        let entry: &mut DayRow = sampled.entry(category.unwrap_or(crate::category::UNCATEGORIZED)).or_default();
        entry.count += day.count;
        entry.complete += day.complete;
        entry.bytes += day.bytes;
    }
    let seen: i64 = sampled.values().map(|r: &DayRow| r.count).sum();
    let scale = if seen > 0 { total as f64 / seen as f64 } else { 0.0 };
    let up = |n: i64| (n as f64 * scale).round() as i64;
    let rows = sampled
        .into_iter()
        .map(|(c, r)| (c, DayRow { count: up(r.count), complete: up(r.complete), bytes: up(r.bytes) }))
        .collect();
    Ok(Some(Day { total, every, rows }))
}

/// One shard's latest PreDB matches. `predb_match` is small (one row per matched release), soo it is read whole;
/// the newest are picked before the join, which the planner would otherwise drive from a scan of `releases`.
fn predb_of(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<PredbRow>> {
    if !has_table(conn, "predb_match")? {
        return Ok(Vec::new());
    }
    let category = if has_table(conn, "release_category")? {
        "(select category from release_category where release_id = m.release_id)"
    } else {
        "null"
    };
    let sql = format!(
        "select m.release_id, coalesce(r.display_name, r.name), m.kind, m.category, m.matched_at, m.pre_at, r.size,
                r.complete, {category}
         from (select * from predb_match order by matched_at desc, release_id desc limit ?1) m
         join releases r on r.id = m.release_id
         order by m.matched_at desc, m.release_id desc"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([limit as i64], |r| {
        Ok(PredbRow {
            id: r.get(0)?,
            name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            kind: r.get(2)?,
            category: r.get(3)?,
            matched_at: r.get(4)?,
            pre_at: r.get(5)?,
            size: r.get(6)?,
            complete: r.get::<_, Option<i64>>(7)? == Some(1),
            atlas_category: r.get::<_, Option<i64>>(8)?.unwrap_or(crate::category::UNCATEGORIZED),
        })
    })?;
    rows.collect()
}

/// One shard's last day (outer None: not asked for; inner None: no posted-date index) and its latest PreDB matches.
type ShardRead = (Option<Option<Day>>, Vec<PredbRow>);

fn read_shard(path: &Path, since: Option<&str>) -> Result<ShardRead, String> {
    let conn = crate::db::open_at(path).map_err(|e| e.to_string())?;
    let _ = conn.pragma_update(None, "query_only", true);
    let day = since.map(|since| day_of(&conn, since, DAY_SAMPLE)).transpose().map_err(|e| e.to_string())?;
    let predb = predb_of(&conn, PREDB_RECENT).map_err(|e| e.to_string())?;
    Ok((day, predb))
}

/// Reads the shards. The last day is taken from `previous` while it is younger than `DAY_TTL`.
fn read_stored(main: &Path, at: f64, previous: Option<&Stored>) -> Stored {
    let started = Instant::now();
    let mut stored = Stored { at, ..Default::default() };
    let keep_day = previous.filter(|p| p.day_at > 0.0 && at - p.day_at < DAY_TTL.as_secs_f64());
    if let Some(p) = keep_day {
        stored.day = p.day.clone();
        stored.day_total = p.day_total;
        stored.day_every = p.day_every;
        stored.day_at = p.day_at;
        stored.day_secs = p.day_secs;
        stored.day_unindexed = p.day_unindexed;
    }
    let since = chrono::DateTime::from_timestamp(at as i64 - 86_400, 0)
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .filter(|_| keep_day.is_none());
    let paths: Vec<_> = crate::store::shard_paths(main).into_iter().filter(|p| p.exists()).collect();
    let results: Vec<_> = thread::scope(|scope| {
        let handles: Vec<_> = paths.iter().map(|p| scope.spawn(|| read_shard(p, since.as_deref()))).collect();
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("the read panicked".into()))).collect()
    });
    if since.is_some() {
        stored.day_at = at;
        stored.day_every = 1;
    }
    for (path, result) in paths.iter().zip(results) {
        match result {
            Ok((day, predb)) => {
                match day {
                    Some(Some(day)) => {
                        stored.day_total += day.total;
                        stored.day_every = stored.day_every.max(day.every);
                        for (category, row) in day.rows {
                            let entry = stored.day.entry(category).or_default();
                            entry.count += row.count;
                            entry.complete += row.complete;
                            entry.bytes += row.bytes;
                        }
                    }
                    Some(None) => stored.day_unindexed += 1,
                    None => {}
                }
                stored.predb.extend(predb);
            }
            Err(e) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                eprintln!("web stats: reading {name} failed: {e}");
                stored.errors.push(format!("{name}: {e}"));
            }
        }
    }
    stored.predb.sort_by(|a, b| b.matched_at.cmp(&a.matched_at).then(b.id.cmp(&a.id)));
    stored.predb.truncate(PREDB_RECENT);
    stored.secs = started.elapsed().as_secs_f64();
    if since.is_some() {
        stored.day_secs = stored.secs;
    }
    stored
}

fn stored() -> Arc<Stored> {
    let fresh = |slot: &Option<(Instant, Arc<Stored>)>| {
        slot.as_ref().filter(|(at, _)| at.elapsed() < STORED_TTL).map(|(_, s)| s.clone())
    };
    if let Some(s) = fresh(&STORED.lock().unwrap()) {
        return s;
    }
    let _reading = READING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = fresh(&STORED.lock().unwrap()) {
        return s;
    }
    let previous = STORED.lock().unwrap().as_ref().map(|(_, s)| s.clone());
    let s = Arc::new(read_stored(&crate::paths::database(), now(), previous.as_deref()));
    *STORED.lock().unwrap() = Some((Instant::now(), s.clone()));
    s
}

// ---------------------------------------------------------------------------------------------------------------
// writing numbers

/// 1234 -> "1,234"; 12_345_678 -> "12.3M"
fn short(n: f64) -> String {
    if !n.is_finite() {
        return "–".into();
    }
    let a = n.abs();
    if a >= 1e9 {
        format!("{:.1}B", n / 1e9)
    } else if a >= 1e6 {
        format!("{:.1}M", n / 1e6)
    } else if a >= 10_000.0 {
        format!("{:.1}k", n / 1e3)
    } else if a >= 100.0 || n.fract() == 0.0 {
        thousands(n.round() as i64)
    } else if a >= 10.0 {
        format!("{n:.1}")
    } else {
        format!("{n:.2}")
    }
}

fn opt(n: Option<f64>) -> String {
    n.map_or_else(|| "–".to_string(), short)
}

fn count(n: Option<f64>) -> String {
    n.map_or_else(|| "–".to_string(), |n| thousands(n.round() as i64))
}

/// 1536 -> "1.5 KB"
fn bytes(n: f64) -> String {
    let mut n = n.max(0.0);
    for unit in ["B", "KB", "MB", "GB", "TB"] {
        if n < 1024.0 {
            return if unit == "B" { format!("{n:.0} B") } else { format!("{n:.1} {unit}") };
        }
        n /= 1024.0;
    }
    format!("{n:.1} PB")
}

/// seconds -> "3d 4h", "5h 12m", "42m", "30s"
fn span(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "–".into();
    }
    let s = secs as u64;
    let (d, h, m) = (s / 86_400, s % 86_400 / 3600, s % 3600 / 60);
    if d >= 365 {
        format!("{:.1} years", d as f64 / 365.0)
    } else if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

fn ago(at: f64, now: f64) -> String {
    format!("{} ago", span(now - at))
}

fn percent(part: f64, whole: f64) -> Option<f64> {
    (whole > 0.0).then(|| (part / whole * 100.0).clamp(0.0, 100.0))
}

/// A thin bar for a share, in SVG (the page's CSP allows no inline styles). `level`: ok, warn, bad or none.
fn bar(share: f64, level: &str, title: &str) -> String {
    let w = (share.clamp(0.0, 1.0) * 100.0 * 10.0).round() / 10.0;
    format!(
        "<svg class=\"bar bar-{level}\" viewBox=\"0 0 100 6\" preserveAspectRatio=\"none\" role=\"img\" aria-label=\"{t}\"><title>{t}</title><rect class=\"bar-track\" width=\"100\" height=\"6\"/><rect class=\"bar-fill\" width=\"{w}\" height=\"6\"/></svg>",
        t = escape(title),
    )
}

/// A tile: what it is, the number, and a line under it.
fn tile(label: &str, value: &str, note: &str) -> String {
    let note = if note.is_empty() { String::new() } else { format!("<p class=\"stat-note\">{note}</p>") };
    format!(
        "<div class=\"stat\"><p class=\"stat-label\">{}</p><p class=\"stat-value\">{}</p>{note}</div>",
        escape(label),
        escape(value)
    )
}

// ---------------------------------------------------------------------------------------------------------------
// the live part

fn stats_json() -> Value {
    std::fs::read_to_string(crate::paths::stats_file())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
}

/// One line of the pipeline table.
struct Lane {
    name: String,
    what: &'static str,
    running: Option<f64>,
    waiting: Option<f64>,
    per_minute: Option<f64>,
    /// a meter when the waiting line has a cap
    meter: Option<(f64, f64)>,
    note: String,
}

impl Lane {
    fn new(name: &str, what: &'static str) -> Lane {
        Lane {
            name: name.into(),
            what,
            running: None,
            waiting: None,
            per_minute: None,
            meter: None,
            note: String::new(),
        }
    }

    fn row(&self) -> String {
        let meter = match self.meter {
            Some((now, cap)) if cap > 0.0 => {
                let share = now / cap;
                let level = if share >= 0.95 {
                    "bad"
                } else if share >= 0.75 {
                    "warn"
                } else {
                    "ok"
                };
                bar(share, level, &format!("{} of {}", thousands(now as i64), thousands(cap as i64)))
            }
            _ => String::new(),
        };
        let note =
            if self.note.is_empty() { String::new() } else { format!("<div class=\"raw-name\">{}</div>", self.note) };
        format!(
            "<tr><td class=\"name\"><b>{}</b><div class=\"raw-name\">{}</div>{note}</td><td class=\"num\">{}</td><td class=\"num\">{}{meter}</td><td class=\"num\">{}</td></tr>",
            escape(&self.name),
            escape(self.what),
            opt(self.running),
            count(self.waiting),
            opt(self.per_minute),
        )
    }
}

fn in_progress(w: &Window, function: &str) -> Option<f64> {
    w.now.as_ref()?.with("atlas_function_in_progress", "function", function)
}

/// The steps of the processing stage, as the metrics name them, in the order they run.
fn step_names() -> Vec<(&'static str, bool)> {
    use crate::postprocess::step::Needs;
    let mut steps: Vec<_> = crate::postprocess::steps::all()
        .into_iter()
        .map(|s| (s.rank(), s.name(), s.needs() != Needs::Database))
        .collect();
    steps.sort_by_key(|s| s.0);
    steps.into_iter().map(|(_, name, net)| (name, net)).collect()
}

fn pipelines(w: &Window, stats: &Value) -> String {
    let n = w.now.as_deref();
    let get = |name: &str| n.and_then(|n| n.sum(name));
    let with = |name: &str, key: &str, value: &str| n.and_then(|n| n.with(name, key, value));
    let mut lanes = Vec::new();

    let mut xover = Lane::new("Headers", "XOVER reads of new articles, live and backfill");
    xover.running = in_progress(w, "nntp.xover");
    xover.waiting = get("atlas_nntp_waiting_requests");
    xover.per_minute = w.function_per_minute("nntp.xover");
    if let Some(a) = w.rate_where("atlas_articles_total", |_| true) {
        xover.note = format!("{} articles a second", short(a));
    }
    lanes.push(xover);

    let mut parse = Lane::new("Parse", "slices of headers made into releases");
    parse.running = in_progress(w, "indexer.parse_slice");
    parse.per_minute = w.function_per_minute("indexer.parse_slice");
    let unsaved = with("atlas_unsaved_headers", "state", "now");
    let cap = with("atlas_unsaved_headers", "state", "cap");
    parse.waiting = unsaved;
    if let (Some(u), Some(c)) = (unsaved, cap) {
        parse.meter = Some((u, c));
        parse.note = format!("headers fetched and not saved yet, of a cap of {}", short(c));
    }
    lanes.push(parse);

    let mut save = Lane::new("Save", "slices written into the shards");
    save.running = in_progress(w, "indexer.save_slice");
    save.waiting = get("atlas_writer_queued_slices");
    save.per_minute = w.function_per_minute("indexer.save_slice");
    if let Some(r) = w.change("atlas_db_rows").and_then(|_| {
        let (now, then) = (w.now.as_ref()?, w.then.as_ref()?);
        let releases = |s: &Scrape| s.with("atlas_db_rows", "kind", "releases");
        Some((releases(now)? - releases(then)?) / (now.at - then.at) * 60.0)
    }) {
        save.note = format!("{} new releases a minute", short(r));
    }
    lanes.push(save);

    let mut seal = Lane::new("Seal", "finished files packed into blobs");
    seal.running = in_progress(w, "db.seal");
    seal.per_minute = w.function_per_minute("db.seal");
    lanes.push(seal);

    let mut stage = Lane::new("Processing queue", "releases due for the database steps");
    stage.waiting = with("atlas_stage_queue", "state", "due");
    stage.running = in_progress(w, "db.stage_transaction");
    stage.per_minute = w.per_minute("atlas_step_runs_total", |l| {
        step_names().iter().any(|(name, net)| !net && label(l, "step") == Some(name))
    });
    if let Some(f) = with("atlas_stage_queue", "state", "failed").filter(|f| *f > 0.0) {
        stage.note = format!("{} failed", thousands(f as i64));
    }
    lanes.push(stage);

    let mut net = Lane::new("Net lane", "releases waiting for article bodies or an outside lookup");
    net.waiting = with("atlas_stage_queue", "state", "net_wanted");
    net.running = in_progress(w, "stage.fetch_body");
    net.per_minute = w.per_minute("atlas_stage_bodies_total", |_| true);
    let lane = &stats["postprocess"]["lanes"];
    let sum = |key: &str| lane[key].as_array().map(|v| v.iter().filter_map(Value::as_f64).sum::<f64>());
    if let (Some(visits), Some(finished)) = (sum("net_visits"), sum("net_finished")) {
        net.note = format!(
            "per minute: bodies fetched; {} releases visited and {} finished a minute",
            short(visits),
            short(finished)
        );
    }
    lanes.push(net);

    let rows: String = lanes.iter().map(Lane::row).collect();
    let mut steps = String::new();
    for (name, is_net) in step_names() {
        let of = |l: &Labels| label(l, "step") == Some(name);
        let runs = w.per_minute("atlas_step_runs_total", of);
        let matched = w.per_minute("atlas_step_runs_total", |l| of(l) && label(l, "result") == Some("matched"));
        let total = n.and_then(|n| n.sum_where("atlas_step_runs_total", of));
        let running = in_progress(w, &format!("step.{name}"));
        let panics = n.and_then(|n| n.with("atlas_step_panics_total", "step", name)).filter(|p| *p > 0.0);
        let panics = panics.map(|p| tag("tag-bad", &format!("{} PANICS", thousands(p as i64)))).unwrap_or_default();
        let lane = if is_net { tag("tag-ref", "NET") } else { tag("", "DB") };
        let _ = write!(
            steps,
            "<tr><td class=\"name\">{}{lane}{panics}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
            escape(crate::web_pending::label(name)),
            opt(running),
            opt(runs),
            opt(matched),
            count(total),
        );
    }
    format!(
        "<h2>Pipelines</h2><div class=\"table-wrap\"><table><thead><tr><th>Stage</th><th class=\"num\">Running</th><th class=\"num\">Waiting</th><th class=\"num\">Per minute</th></tr></thead><tbody>{rows}</tbody></table></div>\
         <h2>Processing steps</h2><div class=\"table-wrap\"><table><thead><tr><th>Step</th><th class=\"num\">Running</th><th class=\"num\">Runs a minute</th><th class=\"num\">Matched a minute</th><th class=\"num\">Runs since start</th></tr></thead><tbody>{steps}</tbody></table></div>"
    )
}

/// Each shard's lanes: from `atlas_stage_lane_total` and `atlas_stage_net_claims` when the indexer exports them,
/// else from stats.json's ten-minute rates.
fn net_shards(w: &Window, stats: &Value) -> String {
    let lane = &stats["postprocess"]["lanes"];
    let n = w.now.as_deref();
    let exported = n.is_some_and(|n| !n.rows("atlas_stage_lane_total").is_empty());
    let shards = if exported {
        n.map_or(0, |n| {
            n.rows("atlas_stage_lane_total")
                .iter()
                .filter_map(|(l, _)| label(l, "shard")?.parse::<usize>().ok())
                .max()
                .map_or(0, |m| m + 1)
        })
    } else {
        lane["net_visits"].as_array().map_or(0, Vec::len)
    };
    if shards == 0 {
        return String::new();
    }
    let rate = |key: &str, i: usize| {
        if exported {
            let shard = i.to_string();
            w.per_minute("atlas_stage_lane_total", |l| {
                label(l, "shard") == Some(shard.as_str()) && label(l, "lane") == Some(key)
            })
        } else {
            lane[key].get(i).and_then(Value::as_f64)
        }
    };
    let limit = n.and_then(|n| n.sum("atlas_stage_net_claims_limit"));
    let claims = |i: usize| n.and_then(|n| n.with("atlas_stage_net_claims", "shard", &i.to_string()));
    let rows: String = (0..shards)
        .map(|i| {
            let held = match (claims(i), limit) {
                (Some(c), Some(l)) if l > 0.0 => format!(
                    "{} of {}{}",
                    thousands(c as i64),
                    thousands(l as i64),
                    bar(c / l, if c >= l { "warn" } else { "ok" }, &format!("{c} of {l} claims held"))
                ),
                (Some(c), _) => thousands(c as i64),
                _ => "–".into(),
            };
            format!(
                "<tr><td>s{i}</td><td class=\"num\">{held}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                opt(rate("net_visits", i)),
                opt(rate("net_finished", i)),
                opt(rate("queue_taken", i)),
            )
        })
        .collect();
    let source = if exported {
        "From the indexer's metrics, over the same window as the rates above.".to_string()
    } else {
        let window = lane["window_secs"].as_f64().map(|s| format!(" over the last {}", span(s))).unwrap_or_default();
        format!("From the indexer's stats.json{window}.")
    };
    format!(
        "<h2>Lanes per shard</h2><div class=\"table-wrap\"><table><thead><tr><th>Shard</th><th class=\"num\">Net claims</th><th class=\"num\">Net visits a minute</th><th class=\"num\">Net finished a minute</th><th class=\"num\">Queue taken a minute</th></tr></thead><tbody>{rows}</tbody></table></div><p class=\"sort\">{source}</p>"
    )
}

/// Groups furthest behind the server, from `atlas_group_article_number` and `atlas_group_posted_timestamp_seconds`.
fn lag(w: &Window, now: f64) -> String {
    const SHOWN: usize = 15;
    const LATE_SECS: f64 = 30.0 * 60.0;
    let Some(n) = w.now.as_deref() else { return String::new() };
    let numbers = n.rows("atlas_group_article_number");
    if numbers.is_empty() {
        return String::new();
    }
    // per group: the server's last article, the live cursor, the newest article indexed
    type Marks = (Option<f64>, Option<f64>, Option<f64>);
    let mut groups: HashMap<&str, Marks> = HashMap::new();
    for (l, v) in numbers {
        let (Some(g), Some(mark)) = (label(l, "group"), label(l, "mark")) else { continue };
        let e = groups.entry(g).or_default();
        match mark {
            "last" => e.0 = Some(*v),
            "live_cursor" => e.1 = Some(*v),
            _ => {}
        }
    }
    for (l, v) in n.rows("atlas_group_posted_timestamp_seconds") {
        if let (Some(g), Some("newest")) = (label(l, "group"), label(l, "end"))
            && let Some(e) = groups.get_mut(g)
        {
            e.2 = Some(*v);
        }
    }
    let mut rows: Vec<(&str, f64, Option<f64>)> = groups
        .iter()
        .filter_map(|(g, (last, live, newest))| Some((*g, ((*last)? - (*live)?).max(0.0), *newest)))
        .collect();
    let behind = rows.iter().filter(|r| r.1 > 0.0).count();
    let late = rows.iter().filter(|r| r.2.is_some_and(|t| now - t > LATE_SECS)).count();
    let total_lag: f64 = rows.iter().map(|r| r.1).sum();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(b.0)));
    let table: String = rows
        .iter()
        .take(SHOWN)
        .filter(|r| r.1 > 0.0)
        .map(|(g, articles, newest)| {
            format!(
                "<tr><td class=\"name\"><a href=\"/?group={}\">{}</a></td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                crate::web::url_encode(g),
                escape(g),
                thousands(*articles as i64),
                newest.map(|t| span(now - t)).unwrap_or_else(|| "–".into()),
            )
        })
        .collect();
    let tiles = [
        tile("Articles behind", &short(total_lag), "server's last article minus the live cursor, summed"),
        tile("Groups behind", &thousands(behind as i64), &format!("of {}", thousands(rows.len() as i64))),
        tile("Quiet over 30 minutes", &thousands(late as i64), "newest article indexed is older than that"),
    ];
    let table = if table.is_empty() {
        "<p class=\"sort\">Every group is caught up with its server.</p>".to_string()
    } else {
        format!(
            "<div class=\"table-wrap\"><table><thead><tr><th>Furthest behind</th><th class=\"num\">Articles</th><th class=\"num\">Newest indexed</th></tr></thead><tbody>{table}</tbody></table></div>"
        )
    };
    format!("<h2>Realtime</h2><div class=\"stat-grid\">{}</div>{table}", tiles.concat())
}

fn headline(w: &Window, stats: &Value, now: f64) -> String {
    let n = w.now.as_deref();
    let get = |name: &str| n.and_then(|n| n.sum(name));
    let with = |name: &str, key: &str, value: &str| n.and_then(|n| n.with(name, key, value));
    // the indexer writes stats.json every second: a listener that still answers while that has gone quiet is
    // a process that stopped moving
    let wrote = stats["load"]["at"].as_f64().filter(|at| *at > 0.0);
    let fresh = wrote.is_some_and(|at| now - at < STALE_SECS);
    let up = get("atlas_indexer_up").is_some_and(|u| u > 0.0);
    let start = get("atlas_process_start_time_seconds");
    let state = match (fresh, up, wrote) {
        (true, _, _) => format!("{} up {}", tag("tag-ok", "RUNNING"), start.map(|s| span(now - s)).unwrap_or_default()),
        (false, true, Some(at)) => format!(
            "{} its metrics still answer, but it last wrote stats.json {}; the counters below aren't moving",
            tag("tag-warn", "NOT MOVING"),
            ago(at, now)
        ),
        (false, true, None) => {
            format!("{} up {}", tag("tag-ok", "RUNNING"), start.map(|s| span(now - s)).unwrap_or_default())
        }
        (false, false, _) => tag("tag-bad", "STOPPED"),
    };
    let wire = w.rate_where("atlas_nntp_traffic_bytes_total", |l| label(l, "kind") == Some("wire"));
    let articles = w.rate_where("atlas_articles_total", |_| true);
    let failures = w.per_minute("atlas_nntp_failures_total", |l| {
        label(l, "kind").is_some_and(|k| !k.starts_with("no_such_") && k != "skipped_while_resting")
    });
    let open = with("atlas_nntp_connections", "state", "open");
    let in_use = with("atlas_nntp_connections", "state", "in_use");
    let errors = get("atlas_indexer_errors_total");
    let tiles = [
        tile("Articles a second", &opt(articles), "across every group"),
        tile("Bandwidth", &wire.map_or_else(|| "–".into(), |b| format!("{}/s", bytes(b))), "off the wire"),
        tile(
            "Connections",
            &count(open),
            &in_use.map(|u| format!("{} in use", thousands(u as i64))).unwrap_or_default(),
        ),
        tile("NNTP failures a minute", &opt(failures), "missing articles left out"),
        tile("Indexer errors", &count(errors), "since it started"),
        tile(
            "Workers",
            &count(get("atlas_indexer_workers")),
            &with("atlas_groups", "state", "indexed")
                .map(|g| format!("{} groups", thousands(g as i64)))
                .unwrap_or_default(),
        ),
    ];
    format!("<p class=\"sort\">{state}</p><div class=\"stat-grid\">{}</div>", tiles.concat())
}

fn catalog(w: &Window, now: f64) -> String {
    let n = w.now.as_deref();
    let state = |s: &str| n.and_then(|n| n.with("atlas_catalog_releases", "state", s));
    let total = state("total");
    let live = n.and_then(|n| n.with("atlas_db_rows", "kind", "releases"));
    let complete = state("complete");
    let share = |part: Option<f64>| match (part, total) {
        (Some(p), Some(t)) => percent(p, t).map(|p| format!("{p:.1}% of the catalog")).unwrap_or_default(),
        _ => String::new(),
    };
    let walked = n.and_then(|n| n.sum("atlas_catalog_scan_timestamp_seconds")).filter(|t| *t > 0.0);
    let size = n.and_then(|n| n.sum("atlas_catalog_bytes"));
    let tiles = [
        tile("Releases stored", &count(live), "every shard, now"),
        tile("Complete releases", &count(complete), &share(complete)),
        tile("Named", &count(state("named")), &share(state("named"))),
        tile("Matched to the PreDB", &count(state("predb_matched")), &share(state("predb_matched"))),
        tile("Still obfuscated", &count(state("still_obfuscated")), &share(state("still_obfuscated"))),
        tile("Catalog size", &size.map_or_else(|| "–".into(), bytes), ""),
    ];
    let when = match walked {
        Some(t) => format!(
            "Counted by the catalog walk {} (it took {}). Releases stored is the shards' running total.",
            ago(t, now),
            n.and_then(|n| n.sum("atlas_catalog_scan_duration_seconds")).map(span).unwrap_or_else(|| "–".into())
        ),
        None => "The catalog walk hasn't finished one walk yet.".into(),
    };
    format!("<h2>Releases</h2><div class=\"stat-grid\">{}</div><p class=\"sort\">{when}</p>", tiles.concat())
}

fn backfill(w: &Window) -> String {
    let n = w.now.as_deref();
    let chunk = |s: &str| n.and_then(|n| n.with("atlas_backfill_chunks", "state", s));
    let (Some(done), Some(total)) = (chunk("done"), chunk("total")) else { return String::new() };
    let articles = |state: &str| n.and_then(|n| n.with("atlas_group_backfill_articles", "state", state));
    let covered = match (articles("covered"), articles("total")) {
        (Some(c), Some(t)) if t > 0.0 => format!(
            "<p class=\"stat-line\"><b>{:.2}%</b> of the servers' articles indexed: {} of {}</p>{}",
            c / t * 100.0,
            short(c),
            short(t),
            bar(c / t, "ok", &format!("{:.2}% of articles", c / t * 100.0)),
        ),
        _ => String::new(),
    };
    let hour = chunk("done_last_hour");
    let share = if total > 0.0 { done / total } else { 0.0 };
    let eta = match hour {
        Some(h) if h > 0.0 => format!("about {} to go at this pace", span((total - done) / h * 3600.0)),
        _ => "no day chunks finished in the last hour".into(),
    };
    let groups = n.and_then(|n| n.with("atlas_backfill_groups", "state", "all"));
    format!(
        "<h2>Backfill</h2><div class=\"panel\">{covered}<p class=\"stat-line\"><b>{:.2}%</b> of the day chunks done: {} of {}</p>{}<p class=\"sort\">{} finished in the last hour, {eta}. {} groups have day chunks.</p></div>",
        share * 100.0,
        thousands(done as i64),
        thousands(total as i64),
        bar(share, "ok", &format!("{:.2}% done", share * 100.0)),
        count(hour),
        count(groups),
    )
}

fn predb_feeds(w: &Window, stats: &Value, now: f64) -> String {
    let n = w.now.as_deref();
    let titles = n.and_then(|n| n.sum("atlas_predb_titles"));
    let behind = n.and_then(|n| n.sum("atlas_predb_late_behind"));
    let matched = w.per_minute("atlas_predb_matches_total", |_| true).map(|m| m * 60.0);
    let mut failing = String::new();
    for feed in stats["predb"]["sources"].as_array().into_iter().flatten() {
        let failures = feed["failures"].as_i64().unwrap_or(0);
        if failures == 0 {
            continue;
        }
        let last = feed["last_ok_at"].as_f64().map(|t| ago(t, now)).unwrap_or_else(|| "never".into());
        let _ = write!(
            failing,
            "<tr><td>{}</td><td class=\"num\">{failures}</td><td class=\"dim\">{last}</td><td class=\"name dim\">{}</td></tr>",
            escape(feed["key"].as_str().unwrap_or("")),
            escape(feed["last_error"].as_str().unwrap_or("")),
        );
    }
    if titles.is_none() && failing.is_empty() {
        return String::new();
    }
    let tiles = [
        tile("PreDB titles", &count(titles), ""),
        tile("Matched an hour", &opt(matched), "releases matched to a title"),
        tile("Titles to look at", &count(behind), "the late matcher's backlog"),
    ];
    let failing = if failing.is_empty() {
        String::new()
    } else {
        format!(
            "<div class=\"table-wrap\"><table><thead><tr><th>Feed failing</th><th class=\"num\">In a row</th><th>Last answered</th><th>Error</th></tr></thead><tbody>{failing}</tbody></table></div>"
        )
    };
    format!("<h2>PreDB</h2><div class=\"stat-grid\">{}</div>{failing}", tiles.concat())
}

/// The part that refreshes itself: everything but the shard reads.
fn live_html(w: &Window, error: Option<&str>, stats: &Value, now: f64) -> String {
    let problem = match (w.now.as_ref(), error) {
        (None, Some(e)) => crate::web::diag("note", "the indexer's metrics could not be read", e),
        (Some(s), Some(e)) => {
            crate::web::diag("note", &format!("these numbers are from {}: the last read failed", ago(s.at, now)), e)
        }
        _ => String::new(),
    };
    let measured = match (w.secs(), w.now.as_ref()) {
        (Some(secs), Some(s)) => format!("Read {}; rates over the last {}.", ago(s.at, now), span(secs)),
        (None, Some(s)) => format!("Read {}; rates show once a second read is in, within a minute.", ago(s.at, now)),
        _ => String::new(),
    };
    format!(
        "<div id=\"stats-live\" hx-get=\"/ui/stats/live\" hx-trigger=\"every {REFRESH_SECS}s\" hx-swap=\"outerHTML\">{problem}{}{}{}{}{}{}{}<p class=\"sort\">{measured} Refreshes every {REFRESH_SECS} seconds.</p></div>",
        headline(w, stats, now),
        pipelines(w, stats),
        net_shards(w, stats),
        lag(w, now),
        backfill(w),
        catalog(w, now),
        predb_feeds(w, stats, now),
    )
}

// ---------------------------------------------------------------------------------------------------------------
// the shard part

fn kind_name(kind: i64) -> &'static str {
    use crate::predb::matcher::Kind;
    match kind {
        k if k == Kind::Exact as i64 => "exact",
        k if k == Kind::Normalized as i64 => "normalized",
        k if k == Kind::Hash as i64 => "hash",
        k if k == Kind::InnerFile as i64 => "inner file",
        _ => "other",
    }
}

fn category_label(id: i64) -> String {
    if id == crate::category::UNCATEGORIZED {
        "Uncategorized".into()
    } else {
        crate::category::label(id).unwrap_or_else(|| id.to_string())
    }
}

fn day_html(s: &Stored) -> String {
    let total: DayRow = s.day.values().fold(DayRow::default(), |mut t, r| {
        t.count += r.count;
        t.complete += r.complete;
        t.bytes += r.bytes;
        t
    });
    // per root: its sum, and its sub-categories
    type Root<'a> = (DayRow, Vec<(i64, &'a DayRow)>);
    let mut roots: BTreeMap<i64, Root> = BTreeMap::new();
    for (category, row) in &s.day {
        let root =
            if *category == crate::category::UNCATEGORIZED { i64::MAX } else { crate::category::root_of(*category) };
        let entry = roots.entry(root).or_default();
        entry.0.count += row.count;
        entry.0.complete += row.complete;
        entry.0.bytes += row.bytes;
        if *category != root && *category != crate::category::UNCATEGORIZED {
            entry.1.push((*category, row));
        }
    }
    let mut order: Vec<_> = roots.into_iter().collect();
    order.sort_by_key(|r| std::cmp::Reverse(r.1.0.count));
    let sampled = s.day_every > 1;
    let about = if sampled { "≈ " } else { "" };
    let line = |label: &str, r: &DayRow, sub: bool| {
        let share = if total.count > 0 { r.count as f64 / total.count as f64 } else { 0.0 };
        let class = if sub { " class=\"sub\"" } else { "" };
        format!(
            "<tr{class}><td class=\"name\">{}</td><td class=\"num\">{about}{}</td><td class=\"share\">{}</td><td class=\"num\">{about}{}</td><td class=\"num\">{about}{}</td></tr>",
            escape(label),
            thousands(r.count),
            bar(share, "none", &format!("{:.1}% of the day's releases", share * 100.0)),
            thousands(r.complete),
            bytes(r.bytes as f64),
        )
    };
    let mut rows = String::new();
    for (root, (sum, mut subs)) in order {
        let name = if root == i64::MAX {
            "Uncategorized".to_string()
        } else {
            crate::category::label(root).unwrap_or_else(|| root.to_string())
        };
        rows.push_str(&line(&name, &sum, false));
        subs.sort_by_key(|r| std::cmp::Reverse(r.1.count));
        for (category, row) in subs {
            let label = category_label(category);
            let label = label.split_once('/').map_or(label.as_str(), |(_, sub)| sub);
            rows.push_str(&line(label, row, true));
        }
    }
    let unindexed = if s.day_unindexed > 0 {
        crate::web::diag(
            "note",
            &format!("{} of the shards have no index on the posted date", s.day_unindexed),
            "They are left out of this count, which would otherwise read every release. Run atlas --search-indexes --apply to build it.",
        )
    } else {
        String::new()
    };
    let tiles = [
        tile("Posted in the last 24 hours", &thousands(s.day_total), "by the usenet date of their newest articles"),
        tile(
            "Complete",
            &format!("{about}{}", short(total.complete as f64)),
            &percent(total.complete as f64, total.count as f64).map(|p| format!("{p:.1}%")).unwrap_or_default(),
        ),
        tile("Size", &format!("{about}{}", bytes(total.bytes as f64)), ""),
    ];
    let table = if rows.is_empty() {
        String::new()
    } else {
        format!(
            "<div class=\"table-wrap\"><table class=\"day\"><thead><tr><th>Category</th><th class=\"num\">Releases</th><th><span class=\"sr-only\">Share</span></th><th class=\"num\">Complete</th><th class=\"num\">Size</th></tr></thead><tbody>{rows}</tbody></table></div>"
        )
    };
    let how = if sampled {
        format!(
            " The count is exact; the breakdown, complete and size are scaled up from one release in {} on the busiest shard.",
            thousands(s.day_every)
        )
    } else {
        String::new()
    };
    format!(
        "<h2>Last 24 hours</h2>{unindexed}<div class=\"stat-grid\">{}</div>{table}<p class=\"sort\">Counted {} in {:.1} s, again after five minutes.{how}</p>",
        tiles.concat(),
        ago(s.day_at, s.at),
        s.day_secs,
    )
}

fn predb_html(s: &Stored, now: f64) -> String {
    if s.predb.is_empty() {
        return "<h2>Recently identified through the PreDB</h2><p class=\"sort\">No release has been matched to a PreDB title yet.</p>".into();
    }
    let rows: String = s
        .predb
        .iter()
        .map(|r| {
            let incomplete = if r.complete { String::new() } else { tag("tag-warn", "INCOMPLETE") };
            let pre = r.pre_at.map(|p| format!("pre {}", ago(p as f64, now))).unwrap_or_default();
            format!(
                "<tr><td class=\"name\"><a href=\"/ui/release?id={}\">{}</a>{incomplete}<div class=\"raw-name\">{} · {}{}</div></td><td class=\"col-cat dim\">{}</td><td class=\"num col-size\">{}</td><td class=\"dim\">{}</td></tr>",
                r.id,
                escape(&r.name),
                escape(r.category.as_deref().unwrap_or("no PreDB category")),
                kind_name(r.kind),
                if pre.is_empty() { String::new() } else { format!(" · {pre}") },
                escape(&category_label(r.atlas_category)),
                r.size.map(|b| bytes(b as f64)).unwrap_or_else(|| "–".into()),
                ago(r.matched_at as f64, now),
            )
        })
        .collect();
    format!(
        "<h2>Recently identified through the PreDB</h2><div class=\"table-wrap\"><table><thead><tr><th>Release</th><th class=\"col-cat\">Category</th><th class=\"num col-size\">Size</th><th>Matched</th></tr></thead><tbody>{rows}</tbody></table></div>"
    )
}

fn stored_html(s: &Stored, now: f64) -> String {
    let errors = if s.errors.is_empty() {
        String::new()
    } else {
        crate::web::diag("caution", "some shards could not be read", &s.errors.join("; "))
    };
    format!(
        "<div id=\"stats-stored\">{errors}{}{}<p class=\"sort\">PreDB matches read {}; again after a minute.</p></div>",
        day_html(s),
        predb_html(s, now),
        ago(s.at, now),
    )
}

// ---------------------------------------------------------------------------------------------------------------
// the routes

pub fn page() -> ApiResponse {
    let (w, error) = window();
    let now = now();
    let content = format!(
        "{masthead}<div class=\"release stats\"><main><h1>Stats</h1>{live}<div id=\"stats-stored\" hx-get=\"/ui/stats/stored\" hx-trigger=\"load\" hx-swap=\"outerHTML\"><h2>Last 24 hours</h2><p class=\"sort\">Reading the shards…</p></div></main></div>",
        masthead = crate::web::masthead(true),
        live = live_html(&w, error.as_deref(), &stats_json(), now),
    );
    html(200, crate::web::page("Stats · Atlas", &content))
}

pub fn live() -> ApiResponse {
    let (w, error) = window();
    html(200, live_html(&w, error.as_deref(), &stats_json(), now()))
}

pub fn stored_fragment() -> ApiResponse {
    html(200, stored_html(&stored(), now()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    const TEXT: &str = r#"# HELP atlas_articles_total Articles indexed, per group.
# TYPE atlas_articles_total counter
atlas_articles_total{group="a.b.c"} 100
atlas_articles_total{group="x \"y\""} 50
atlas_unsaved_headers{state="now"} 7500000
atlas_unsaved_headers{state="cap"} 10000000
atlas_function_duration_seconds_bucket{function="nntp.xover",le="0.1"} 3
atlas_function_duration_seconds_count{function="nntp.xover"} 10
atlas_indexer_up 1
"#;

    #[test]
    fn reads_the_text_format() {
        let s = parse(TEXT, 1.0);
        assert_eq!(s.sum("atlas_articles_total"), Some(150.0));
        assert_eq!(s.with("atlas_articles_total", "group", "x \"y\""), Some(50.0));
        assert_eq!(s.with("atlas_unsaved_headers", "state", "cap"), Some(1e7));
        assert_eq!(s.sum("atlas_indexer_up"), Some(1.0));
        assert!(!s.series.contains_key("atlas_function_duration_seconds_bucket"), "buckets are left out");
        assert_eq!(s.sum("atlas_nothing"), None);
        assert_eq!(
            parse_labels(r#"a="1",b="2\n""#).unwrap(),
            vec![("a".into(), "1".into()), ("b".into(), "2\n".into())]
        );
        assert_eq!(parse_labels(r#"a="open"#), None);
    }

    #[test]
    fn rates_count_restarts_from_zero_and_skip_new_series() {
        let then = parse("c{g=\"a\"} 100\nc{g=\"b\"} 500\n", 0.0);
        let now = parse("c{g=\"a\"} 160\nc{g=\"b\"} 30\nc{g=\"new\"} 9999\n", 60.0);
        let w = Window { now: Some(Arc::new(now)), then: Some(Arc::new(then)) };
        // a: +60, b restarted: 30, new: not counted
        assert_eq!(w.rate_where("c", |_| true), Some(1.5));
        assert_eq!(w.per_minute("c", |l| label(l, "g") == Some("a")), Some(60.0));
        let short_window = Window { then: Some(Arc::new(parse("c 1\n", 50.0))), ..w.clone() };
        assert_eq!(short_window.rate_where("c", |_| true), None, "under MIN_WINDOW_SECS");
    }

    #[test]
    fn the_window_measures_from_the_oldest_read_inside_it() {
        let mut samples = VecDeque::new();
        for at in [0.0, 100.0, 200.0, 400.0, 450.0] {
            samples.push_back(Arc::new(parse("c 1\n", at)));
        }
        let w = Window::of(&samples);
        assert_eq!(w.secs(), Some(250.0), "from 200: 100 is past the five minutes");
        let one: VecDeque<_> = [Arc::new(parse("c 1\n", 5.0))].into();
        assert_eq!(Window::of(&one).secs(), None);
    }

    #[test]
    fn numbers_read_short() {
        assert_eq!(short(12_345_678.0), "12.3M");
        assert_eq!(short(1234.0), "1,234");
        assert_eq!(short(42_100.0), "42.1k");
        assert_eq!(short(4.5678), "4.57");
        assert_eq!(bytes(1536.0), "1.5 KB");
        assert_eq!(span(3.0 * 3600.0 + 120.0), "3h 2m");
    }

    #[test]
    fn the_live_part_shows_pipelines_and_says_when_rates_are_coming() {
        let w = Window { now: Some(Arc::new(parse(TEXT, 1000.0))), then: None };
        let stats = serde_json::json!({"load": {"at": 1000.0}, "postprocess": {"lanes": {"window_secs": 600,
            "net_visits": [1.0, 2.0], "net_finished": [0.5, 0.5], "queue_taken": [3.0, 4.0]}}});
        let out = live_html(&w, None, &stats, 1001.0);
        assert!(out.contains("RUNNING"));
        assert!(out.contains("7,500,000"), "unsaved headers as the parse lane's waiting line");
        assert!(out.contains("rates show once"));
        assert!(out.contains("Lanes per shard") && out.contains("<td>s1</td>"));
        assert!(out.contains("hx-trigger=\"every 30s\""));
        let quiet = live_html(&w, None, &stats, 1000.0 + STALE_SECS + 5.0);
        assert!(quiet.contains("NOT MOVING") && !quiet.contains("RUNNING"), "metrics answer, stats.json went quiet");
        let failed = live_html(&Window::default(), Some("connection refused"), &Value::Null, 1001.0);
        assert!(failed.contains("could not be read") && failed.contains("connection refused"));
    }

    #[test]
    fn a_busy_shards_day_is_counted_exactly_and_broken_down_from_a_sample() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.db");
        let conn = crate::db::create_at(&path).unwrap();
        crate::store::build_shard(&conn).unwrap();
        conn.execute_batch(&format!(
            "create index {} on releases(posted_date);
             create table if not exists release_category (release_id INTEGER PRIMARY KEY, category INTEGER NOT NULL);",
            crate::field_search::POSTED_INDEX
        ))
        .unwrap();
        // 1,000 fresh releases, a seventh of them TV and complete, and one old
        for n in 0..1_000i64 {
            let id = n * crate::store::SHARDS as i64;
            conn.execute(
                "insert into releases (id, name, group_name, posted_date, size, complete) values (?, ?, 'g', '2026-11-05 10:00:00', 10, ?)",
                params![id, format!("r{n}"), (n % 7 == 3) as i64],
            )
            .unwrap();
            if n % 7 == 3 {
                conn.execute("insert into release_category values (?, 5040)", [id]).unwrap();
            }
        }
        conn.execute("insert into releases (id, name, group_name, posted_date) values (99999992, 'old', 'g', '2026-11-01 00:00:00')", []).unwrap();
        let day = day_of(&conn, "2026-11-04 12:00:00", 100).unwrap().unwrap();
        assert_eq!((day.total, day.every), (1_000, 10));
        let tv = &day.rows[&5040];
        assert_eq!(tv.count, tv.complete);
        assert!((100..=190).contains(&tv.count), "about a seventh: {}", tv.count);
        let all = day_of(&conn, "2026-11-04 12:00:00", 20_000).unwrap().unwrap();
        assert_eq!(
            (all.every, all.rows[&5040].count, all.rows[&crate::category::UNCATEGORIZED].bytes),
            (1, 143, 8_570)
        );
    }

    #[test]
    fn the_shards_give_the_last_day_and_the_latest_predb_matches() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        crate::db::create_db_at(&main).unwrap();
        let shards = crate::store::shard_paths(&main);
        let at = 1_800_000_000.0;
        let fmt =
            |t: f64| chrono::DateTime::from_timestamp(t as i64, 0).unwrap().format("%Y-%m-%d %H:%M:%S").to_string();
        for (i, path) in shards.iter().enumerate().take(2) {
            let conn = crate::db::open_at(path).unwrap();
            conn.execute_batch(&format!(
                "create index if not exists {} on releases(posted_date);
                 create table if not exists release_category (release_id INTEGER PRIMARY KEY, category INTEGER NOT NULL);
                 create table if not exists predb_match (release_id INTEGER PRIMARY KEY, title_id INTEGER NOT NULL,
                    kind INTEGER NOT NULL, category TEXT, pre_at INTEGER, nuked INTEGER NOT NULL DEFAULT 0,
                    matched_at INTEGER NOT NULL);",
                crate::field_search::POSTED_INDEX
            ))
            .unwrap();
            let base = i as i64;
            // fresh movie (complete), fresh uncategorized, and one from two days ago
            for (n, posted, complete, cat) in
                [(1, at - 3600.0, 1, Some(2040)), (2, at - 7200.0, 0, None), (3, at - 2.0 * 86_400.0, 1, Some(2040))]
            {
                let id = n * 8 + base;
                conn.execute(
                    "insert into releases (id, name, group_name, posted_date, size, complete) values (?, ?, 'g', ?, 1000, ?)",
                    params![id, format!("Rel.{n}.{i}"), fmt(posted), complete],
                )
                .unwrap();
                if let Some(cat) = cat {
                    conn.execute("insert into release_category values (?, ?)", params![id, cat]).unwrap();
                }
            }
            conn.execute(
                "insert into predb_match (release_id, title_id, kind, category, matched_at) values (?, 1, 1, 'X264', ?)",
                params![8 + base, at as i64 - 60 * (base + 1)],
            )
            .unwrap();
        }
        let s = read_stored(&main, at, None);
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        assert_eq!(s.day_unindexed, shards.len() - 2, "shards without the index are left out, not scanned");
        assert_eq!(s.day[&2040], DayRow { count: 2, complete: 2, bytes: 2000 });
        assert_eq!(s.day[&crate::category::UNCATEGORIZED].count, 2);
        assert_eq!((s.day_total, s.day_every, s.day_at), (4, 1, at));
        // a minute later the matches are read again, the day is kept
        let later = read_stored(&main, at + 60.0, Some(&s));
        assert_eq!((later.day_total, later.day_at), (4, at));
        assert_eq!(later.predb.len(), 2);
        assert_eq!(s.predb.iter().map(|r| r.id).collect::<Vec<_>>(), vec![8, 9], "newest match first");
        assert_eq!(s.predb[0].atlas_category, 2040);
        let out = stored_html(&s, at);
        assert!(out.contains("Posted in the last 24 hours") && out.contains("Uncategorized"));
        assert!(out.contains("/ui/release?id=8") && out.contains("X264 · exact"));
        assert!(out.contains("no index on the posted date"));
    }
}
