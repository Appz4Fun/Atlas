//! Statistics presentation over bounded PostgreSQL catalog reads and completed
//! native metric observations. Rate reduction and formatting are pure; this
//! module has no database opener, HTTP sampler, or stats-file fallback.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt::Write as _;
use std::sync::Arc;

use crate::api::escape;
use crate::web::{tag, thousands};

/// Rates use the same five-minute horizon as the native observation reader.
const RATE_WINDOW_SECS: f64 = 300.0;
/// A series needs a completed observation window before displaying a rate.
const MIN_WINDOW_SECS: f64 = 30.0;
const REFRESH_SECS: u32 = 30;
const STORED_REFRESH_SECS: u64 = 60;

pub(crate) type Labels = Vec<(String, String)>;

/// One read of `/metrics`: every series but histogram buckets, by metric name.
#[derive(Debug, Default)]
pub(crate) struct Scrape {
    pub(crate) at: f64,
    pub(crate) series: HashMap<String, Vec<(Labels, f64)>>,
}

fn label<'a>(labels: &'a Labels, key: &str) -> Option<&'a str> {
    labels.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// `{a="1",b="x\"y"}` -> [(a, 1), (b, x"y)]. None when it isn't well formed.
pub(crate) fn parse_labels(text: &str) -> Option<Labels> {
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

/// The newest read, the oldest kept inside the rate window to measure from, and every read in between.
#[derive(Clone, Default)]
pub(crate) struct Window {
    pub(crate) now: Option<Arc<Scrape>>,
    pub(crate) then: Option<Arc<Scrape>>,
    /// the reads from `then` to `now`, oldest first; empty means just those two
    pub(crate) chain: Vec<Arc<Scrape>>,
    /// Native rates are reduced only AFTER each original series' reset-aware
    /// deltas have been measured. They are rates, never invented then/now values.
    pub(crate) computed: Option<ComputedWindow>,
}

/// Bounded native view projection of an atomically completed observation head.
/// The loader checks the actual owner before/after its readonly snapshot and
/// reduces original series before aggregation. This data grants no startup or
/// writer authority; missing measurements are not invented counter samples.
#[derive(Clone)]
pub(crate) struct ComputedWindow {
    pub(crate) from: Option<f64>,
    pub(crate) to: f64,
    pub(crate) rates: Arc<Scrape>,
    pub(crate) changes: HashMap<String, f64>,
    pub(crate) lag: Option<LagFacts>,
}

#[derive(Clone)]
pub(crate) struct LagFacts {
    pub(crate) groups: usize,
    pub(crate) incomplete: usize,
    pub(crate) untimed: usize,
    pub(crate) behind: usize,
    pub(crate) late: usize,
    pub(crate) articles: f64,
    /// Exactly the original top15 by lag DESC, group ASC, with raw group names.
    pub(crate) rows: Vec<(String, f64, Option<f64>)>,
}

impl Window {
    fn of(samples: &VecDeque<Arc<Scrape>>) -> Window {
        let now = samples.back().cloned();
        let then = now.as_ref().and_then(|n| {
            samples.iter().find(|s| n.at - s.at <= RATE_WINDOW_SECS + 1.0 && n.at - s.at >= MIN_WINDOW_SECS).cloned()
        });
        let chain = match &then {
            Some(t) => samples.iter().filter(|s| s.at >= t.at).cloned().collect(),
            None => Vec::new(),
        };
        Window { now, then, chain, computed: None }
    }

    fn secs(&self) -> Option<f64> {
        match &self.computed {
            Some(window) => Some(window.to - window.from?),
            None => Some(self.now.as_ref()?.at - self.then.as_ref()?.at),
        }
    }

    /// Per second, over the window, of the counter series `keep` takes, added up read to read the way `rate` does:
    /// a series that went down between two reads started over with its process and counts from zero there, and a
    /// series counts only between reads that both have it.
    fn rate_where(&self, name: &str, keep: impl Fn(&Labels) -> bool) -> Option<f64> {
        if let Some(window) = &self.computed {
            return ((window.to - window.from?) >= MIN_WINDOW_SECS)
                .then(|| window.rates.sum_where(name, keep))
                .flatten();
        }
        let (now, then) = (self.now.as_ref()?, self.then.as_ref()?);
        let secs = now.at - then.at;
        if secs < MIN_WINDOW_SECS {
            return None;
        }
        let pair = [then.clone(), now.clone()];
        let chain = if self.chain.len() >= 2 { self.chain.as_slice() } else { &pair[..] };
        let mut total = None;
        for step in chain.windows(2) {
            let before: HashMap<&Labels, f64> = step[0].rows(name).iter().map(|(l, v)| (l, *v)).collect();
            for (labels, value) in step[1].rows(name) {
                if !keep(labels) {
                    continue;
                }
                let Some(old) = before.get(labels) else { continue };
                let delta = if value >= old { value - old } else { *value };
                *total.get_or_insert(0.0) += delta;
            }
        }
        total.map(|t| t / secs)
    }

    fn per_minute(&self, name: &str, keep: impl Fn(&Labels) -> bool) -> Option<f64> {
        self.rate_where(name, keep).map(|r| r * 60.0)
    }

    /// A gauge's change per second over the window (`deriv`, from two points).
    fn change(&self, name: &str) -> Option<f64> {
        if let Some(window) = &self.computed {
            return ((window.to - window.from?) >= MIN_WINDOW_SECS)
                .then(|| window.changes.get(name).copied())
                .flatten();
        }
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

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DayRow {
    pub(crate) count: i64,
    pub(crate) complete: i64,
    pub(crate) bytes: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PredbRow {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) kind: i64,
    pub(crate) category: Option<String>,
    pub(crate) matched_at: i64,
    pub(crate) pre_at: Option<i64>,
    pub(crate) size: Option<i64>,
    pub(crate) complete: bool,
    pub(crate) atlas_category: i64,
}

#[derive(Debug, Default)]
pub(crate) struct Stored {
    /// when it was read, unix seconds
    pub(crate) at: f64,
    /// how long the reads took
    pub(crate) secs: f64,
    /// by stored category, scaled up where a shard was sampled; uncategorized is `UNCATEGORIZED`
    pub(crate) day: BTreeMap<i64, DayRow>,
    /// releases posted in the day, counted exactly
    pub(crate) day_total: i64,
    /// the widest sample step a shard used: 1 when every release was looked at
    pub(crate) day_every: i64,
    /// when the day was read, and how long that took
    pub(crate) day_at: f64,
    pub(crate) day_secs: f64,
    /// shards that couldn't answer the last day's count: no posted-date index there yet
    pub(crate) day_unindexed: usize,
    pub(crate) predb: Vec<PredbRow>,
    pub(crate) errors: Vec<String>,
}

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
    crate::postprocess::steps::catalog().iter().map(|s| (s.name, s.needs != Needs::Database)).collect()
}

fn pipelines_with_pace(w: &Window, native: &crate::web_pending::NativePace) -> String {
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
    net.note = match native {
        crate::web_pending::NativePace::Measured(pace) => format!(
            "{} releases visited and {} finished a minute, measured from the native counter instance",
            short(pace.net_visits),
            short(pace.net_finished)
        ),
        crate::web_pending::NativePace::Warming { .. } => {
            "Native processing is running; its rate window is warming up.".into()
        }
        crate::web_pending::NativePace::Stopped { .. } => "Native processing is on hold.".into(),
    };
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

/// Groups furthest behind the server, from `atlas_group_article_number` and `atlas_group_posted_timestamp_seconds`.
fn lag(w: &Window, now: f64) -> String {
    if let Some(facts) = w.computed.as_ref().and_then(|window| window.lag.as_ref()) {
        return lag_from_facts(facts, now);
    }
    const SHOWN: usize = 15;
    const LATE_SECS: f64 = 30.0 * 60.0;
    let Some(n) = w.now.as_deref() else { return String::new() };
    let numbers = n.rows("atlas_group_article_number");
    let newest = n.rows("atlas_group_posted_timestamp_seconds");
    if numbers.is_empty() && newest.is_empty() {
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
    for (l, v) in newest {
        if let Some(g) = label(l, "group") {
            let entry = groups.entry(g).or_default();
            if label(l, "end") == Some("newest") {
                entry.2 = Some(*v);
            }
        }
    }
    let incomplete = groups.values().filter(|(last, live, _)| last.is_none() || live.is_none()).count();
    let untimed = groups.values().filter(|(_, _, newest)| newest.is_none()).count();
    let mut rows: Vec<(&str, f64, Option<f64>)> = groups
        .iter()
        .filter_map(|(g, (last, live, newest))| Some((*g, ((*last)? - (*live)?).max(0.0), *newest)))
        .collect();
    let behind = rows.iter().filter(|r| r.1 > 0.0).count();
    let late = groups.values().filter(|(_, _, newest)| newest.is_some_and(|at| now - at > LATE_SECS)).count();
    let total_lag: f64 = rows.iter().map(|r| r.1).sum();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(b.0)));
    let facts = LagFacts {
        groups: groups.len(),
        incomplete,
        untimed,
        behind,
        late,
        articles: total_lag,
        rows: rows.into_iter().take(SHOWN).map(|(group, lag, newest)| (group.to_owned(), lag, newest)).collect(),
    };
    lag_from_facts(&facts, now)
}

fn lag_from_facts(facts: &LagFacts, now: f64) -> String {
    let table: String = facts.rows
        .iter()
        .take(15)
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
    let known = facts.groups.saturating_sub(facts.incomplete);
    let articles = if known == 0 && facts.incomplete > 0 {
        "unavailable".into()
    } else if facts.incomplete > 0 {
        format!("{} known", short(facts.articles))
    } else {
        short(facts.articles)
    };
    let behind = if known == 0 && facts.incomplete > 0 {
        "unavailable".into()
    } else if facts.incomplete > 0 {
        format!("{} known", thousands(facts.behind as i64))
    } else {
        thousands(facts.behind as i64)
    };
    let quiet = if facts.untimed == facts.groups && facts.untimed > 0 {
        "unavailable".into()
    } else if facts.untimed > 0 {
        format!("{} known", thousands(facts.late as i64))
    } else {
        thousands(facts.late as i64)
    };
    let tiles = [
        tile("Articles behind", &articles, "server's last article minus the live cursor, over measured groups"),
        tile("Groups behind", &behind, &format!("of {} observed groups", thousands(facts.groups as i64))),
        tile("Quiet over 30 minutes", &quiet, "newest article indexed is older than that, over timed groups"),
    ];
    let mut unavailable = String::new();
    for (count, message) in [(facts.incomplete, "Progress"), (facts.untimed, "Newest-article time")] {
        if count > 0 {
            let _ = write!(
                unavailable,
                "<p class=\"note\">{message} is unavailable for {} group{}.</p>",
                thousands(count as i64),
                if count == 1 { "" } else { "s" }
            );
        }
    }
    let table = if table.is_empty() {
        if facts.incomplete == 0 {
            "<p class=\"sort\">Every group is caught up with its server.</p>".to_string()
        } else if known > 0 {
            "<p class=\"sort\">Known groups are caught up with their servers.</p>".to_string()
        } else {
            String::new()
        }
    } else {
        format!(
            "<div class=\"table-wrap\"><table><thead><tr><th>Furthest behind</th><th class=\"num\">Articles</th><th class=\"num\">Newest indexed</th></tr></thead><tbody>{table}</tbody></table></div>"
        )
    };
    format!("<h2>Realtime</h2><div class=\"stat-grid\">{}</div>{unavailable}{table}", tiles.concat())
}

fn headline_from_state(w: &Window, state: String) -> String {
    let n = w.now.as_deref();
    let get = |name: &str| n.and_then(|n| n.sum(name));
    let with = |name: &str, key: &str, value: &str| n.and_then(|n| n.with(name, key, value));
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

pub(crate) fn stored_html(s: &Stored, now: f64) -> String {
    let errors = if s.errors.is_empty() {
        String::new()
    } else {
        crate::web::diag("caution", "some shards could not be read", &s.errors.join("; "))
    };
    format!(
        "<div id=\"stats-stored\" hx-get=\"/ui/stats/stored\" hx-trigger=\"every {}s\" hx-swap=\"outerHTML\">{errors}{}{}<p class=\"sort\">PreDB matches read {}; again after a minute.</p></div>",
        STORED_REFRESH_SECS,
        day_html(s),
        predb_html(s, now),
        ago(s.at, now),
    )
}

// ---------------------------------------------------------------------------------------------------------------
// the routes

/// Presentation of actual fresh-catalog SQL and completed registry facts.
/// It never synthesizes the retired eight-shard stats.json representation.
pub(crate) struct NativeCatalog {
    pub total: i64,
    pub complete: i64,
    pub named: i64,
    pub matched: i64,
    pub obfuscated: i64,
    pub bytes: i64,
}
pub(crate) struct NativeFeed {
    pub key: String,
    pub failures: i64,
    pub last_ok: Option<i64>,
    pub error: Option<String>,
}
pub(crate) struct NativePredb {
    pub titles: i64,
    pub behind: Option<i64>,
    pub feeds: Vec<NativeFeed>,
}

pub(crate) fn native_live_from_facts(facts: &crate::pg::fresh_statistics::NativeStatistics) -> String {
    let w = &facts.window;
    let now = facts.now;
    let n = w.now.as_deref();
    let up = n.and_then(|sample| sample.sum("atlas_indexer_up"));
    let state = match up {
        _ if !facts.live_owner => tag("tag-warn", "LIVE OWNER UNAVAILABLE"),
        Some(up) if up > 0.0 => tag("tag-ok", "RUNNING"),
        Some(0.0) => tag("tag-warn", "ON HOLD"),
        _ => tag("tag-warn", "ACTIVITY NOT REPORTED"),
    };
    let catalog = &facts.catalog;
    let catalog = format!(
        "<h2>Releases</h2><div class=\"stat-grid\">{}</div><p class=\"sort\">Read from PostgreSQL. Native releases remain in this catalog.</p>",
        [
            tile("Releases stored", &thousands(catalog.total), "PostgreSQL catalog"),
            tile("Complete releases", &thousands(catalog.complete), ""),
            tile("Named", &thousands(catalog.named), ""),
            tile("Matched to the PreDB", &thousands(catalog.matched), ""),
            tile("Still obfuscated", &thousands(catalog.obfuscated), ""),
            tile("Catalog size", &bytes(catalog.bytes as f64), "")
        ]
        .concat()
    );
    let mut feeds = String::new();
    for feed in &facts.predb.feeds {
        let _ = write!(
            feeds,
            "<tr><td>{}</td><td class=\"num\">{}</td><td>{}</td><td class=\"name dim\">{}</td></tr>",
            escape(&feed.key),
            feed.failures,
            feed.last_ok.map(|at| ago(at as f64, now)).unwrap_or_else(|| "never".into()),
            escape(feed.error.as_deref().unwrap_or(""))
        );
    }
    let feed_table = if feeds.is_empty() {
        String::new()
    } else {
        format!(
            "<div class=\"table-wrap\"><table><thead><tr><th>Feed failing</th><th>In a row</th><th>Last answered</th><th>Error</th></tr></thead><tbody>{feeds}</tbody></table></div>"
        )
    };
    let predb = format!(
        "<h2>PreDB</h2><div class=\"stat-grid\">{}{}{}</div>{feed_table}",
        tile("PreDB titles", &thousands(facts.predb.titles), "retained PostgreSQL cache"),
        tile(
            "Matched an hour",
            &opt(w.per_minute("atlas_predb_matches_total", |_| true).map(|rate| rate * 60.0)),
            "releases matched to a title"
        ),
        tile(
            "Titles to look at",
            &facts.predb.behind.map(|n| thousands(n)).unwrap_or_else(|| "–".into()),
            "the recorded late matcher's backlog"
        )
    );
    let offline = if facts.live_owner { "" } else {
        "<p class=\"sort\">The live indexer owner is unavailable. Retained catalog counts remain available; live activity and rates are unavailable. Previous observations are not shown as current.</p>"
    };
    let measured = if w.computed.as_ref().is_some_and(|window| !window.rates.series.is_empty()) {
        "Original-series rates use their completed 30–300 second observation windows."
    } else {
        "Rates are unavailable until an original series has a completed observation window of at least 30 seconds."
    };
    format!(
        "<div id=\"stats-live\" hx-get=\"/ui/stats/live\" hx-trigger=\"every {REFRESH_SECS}s\" hx-swap=\"outerHTML\">{offline}{}{}{}{}{}{predb}<p class=\"sort\">Read {}; {measured}</p></div>",
        headline_from_state(w, state),
        pipelines_with_pace(w, &facts.pace),
        lag(w, now),
        backfill(w),
        catalog,
        w.now.as_ref().map(|sample| ago(sample.at, now)).unwrap_or_else(|| "unavailable".into())
    )
}

pub(crate) fn native_page_from_facts(facts: &crate::pg::fresh_statistics::NativeStatistics) -> String {
    let content = format!(
        "{}<div class=\"release stats\"><main><h1>Stats</h1>{}<div id=\"stats-stored\" hx-get=\"/ui/stats/stored\" hx-trigger=\"load\" hx-swap=\"outerHTML\"><h2>Last 24 hours</h2><p class=\"sort\">Reading PostgreSQL…</p></div></main></div>",
        crate::web::masthead(true),
        native_live_from_facts(facts)
    );
    crate::web::page("Stats · Atlas", &content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_rate_and_lag_facts_keep_reset_semantics_without_fake_samples() {
        let then =
            Arc::new(parse("atlas_articles_total{group=\"a\"} 100\natlas_articles_total{group=\"b\"} 500\n", 0.0));
        let now = Arc::new(parse(
            "atlas_articles_total{group=\"a\"} 160\natlas_articles_total{group=\"b\"} 30\natlas_articles_total{group=\"new\"} 9999\natlas_group_article_number{group=\"a\",mark=\"last\"} 100\natlas_group_article_number{group=\"a\",mark=\"live_cursor\"} 50\natlas_group_article_number{group=\"b\",mark=\"last\"} 10\natlas_group_article_number{group=\"b\",mark=\"live_cursor\"} 10\natlas_group_article_number{group=\"c\",mark=\"last\"} 50\natlas_group_article_number{group=\"c\",mark=\"live_cursor\"} 25\natlas_group_posted_timestamp_seconds{group=\"a\",end=\"newest\"} 1\natlas_group_posted_timestamp_seconds{group=\"b\",end=\"newest\"} 1900\natlas_group_posted_timestamp_seconds{group=\"c\",end=\"newest\"} 1990\n",
            60.0,
        ));
        let original = Window { then: Some(then), now: Some(now.clone()), ..Default::default() };
        let native = Window {
            now: Some(now),
            computed: Some(ComputedWindow {
                from: Some(0.0),
                to: 60.0,
                rates: Arc::new(Scrape {
                    at: 60.0,
                    series: HashMap::from([("atlas_articles_total".into(), vec![(Vec::new(), 1.5)])]),
                }),
                changes: HashMap::new(),
                lag: Some(LagFacts {
                    groups: 3,
                    incomplete: 0,
                    untimed: 0,
                    behind: 2,
                    late: 1,
                    articles: 75.0,
                    rows: vec![
                        ("a".into(), 50.0, Some(1.0)),
                        ("c".into(), 25.0, Some(1990.0)),
                        ("b".into(), 0.0, Some(1900.0)),
                    ],
                }),
            }),
            ..Default::default()
        };
        // +60 on a, +30 after b reset; a new series contributes no rate.
        assert_eq!(original.rate_where("atlas_articles_total", |_| true), Some(1.5));
        assert_eq!(native.rate_where("atlas_articles_total", |_| true), Some(1.5));
        assert!(native.then.is_none() && native.chain.is_empty());
        assert_eq!(lag(&original, 2000.0), lag(&native, 2000.0));
    }

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
        let w = Window { now: Some(Arc::new(now)), then: Some(Arc::new(then)), ..Default::default() };
        // a: +60, b restarted: 30, new: not counted
        assert_eq!(w.rate_where("c", |_| true), Some(1.5));
        assert_eq!(w.per_minute("c", |l| label(l, "g") == Some("a")), Some(60.0));
        // a restart in the middle of the window: 1000, 1200, 20, 1100 is 200 + 20 + 1080
        let reads: VecDeque<_> = [(0.0, 1000), (100.0, 1200), (200.0, 20), (300.0, 1100)]
            .map(|(at, v)| Arc::new(parse(&format!("c {v}\n"), at)))
            .into();
        let restarted = Window::of(&reads);
        assert_eq!(restarted.rate_where("c", |_| true), Some(1300.0 / 300.0));
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
    fn native_stored_facts_render_categories_and_recent_predb_matches() {
        let at = 1_800_000_000.0;
        let facts = Stored {
            at,
            day_at: at,
            day_total: 4,
            day_every: 1,
            day: BTreeMap::from([
                (2040, DayRow { count: 2, complete: 2, bytes: 2000 }),
                (crate::category::UNCATEGORIZED, DayRow { count: 2, complete: 0, bytes: 2000 }),
            ]),
            predb: vec![PredbRow {
                id: 8,
                name: "Rel.1.0".into(),
                kind: crate::predb::matcher::Kind::Exact as i64,
                category: Some("X264".into()),
                matched_at: at as i64 - 60,
                pre_at: None,
                size: Some(1000),
                complete: true,
                atlas_category: 2040,
            }],
            ..Default::default()
        };
        let html = stored_html(&facts, at);
        assert!(html.contains("Posted in the last 24 hours") && html.contains("Uncategorized"), "{html}");
        assert!(html.contains("/ui/release?id=8") && html.contains("X264 · exact"), "{html}");
        assert!(!html.contains("no index on the posted date"), "{html}");
        assert!(html.contains("hx-get=\"/ui/stats/stored\" hx-trigger=\"every 60s\""), "{html}");
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
}

#[cfg(test)]
mod native_lag_evidence_tests {
    use super::*;
    fn native(text: &str) -> Window {
        Window {
            now: Some(Arc::new(parse(text, 2000.0))),
            computed: Some(ComputedWindow {
                from: None,
                to: 0.0,
                rates: Arc::new(Scrape::default()),
                changes: HashMap::new(),
                lag: None,
            }),
            ..Default::default()
        }
    }
    #[test]
    fn missing_last_or_live_marks_never_claim_every_group_is_caught_up() {
        for text in [
            "atlas_group_article_number{group=\"unknown\",mark=\"live_cursor\"} 50\n",
            "atlas_group_article_number{group=\"unknown\",mark=\"last\"} 100\n",
        ] {
            let body = lag(&native(text), 2000.0);
            assert!(!body.contains("Every group is caught up"), "{body}");
            assert!(body.contains("Progress is unavailable for 1 group"), "{body}");
        }
    }
    #[test]
    fn mixed_unknown_and_known_progress_retains_both_evidence_states() {
        let zero = "atlas_group_article_number{group=\"known\",mark=\"last\"} 100\natlas_group_article_number{group=\"known\",mark=\"live_cursor\"} 100\n";
        let unknown = "atlas_group_article_number{group=\"unknown\",mark=\"live_cursor\"} 20\n";
        let body = lag(&native(&format!("{zero}{unknown}")), 2000.0);
        assert!(!body.contains("Every group is caught up"));
        assert!(
            body.contains("Known groups are caught up") && body.contains("Progress is unavailable for 1 group"),
            "{body}"
        );
        assert!(body.contains("of 2"), "the incomplete group must remain in the denominator: {body}");
        let behind = zero.replace("live_cursor\"} 100", "live_cursor\"} 50");
        let body = lag(&native(&format!("{behind}{unknown}")), 2000.0);
        assert!(body.contains("Progress is unavailable for 1 group") && body.contains("?group=known"), "{body}");
    }
    #[test]
    fn timestamp_only_groups_keep_unknown_progress_and_newest_time_explicit() {
        for (end, missing_newest) in [("oldest", true), ("newest", false)] {
            let text = format!("atlas_group_posted_timestamp_seconds{{group=\"timestamp-only\",end=\"{end}\"}} 1990\n");
            let body = lag(&native(&text), 2000.0);
            assert!(!body.contains("Every group is caught up"), "{body}");
            assert!(body.contains("Progress is unavailable for 1 group"), "{body}");
            assert!(body.contains("of 1 observed groups"), "the timestamp-only group must remain observed: {body}");
            assert_eq!(body.contains("Newest-article time is unavailable for 1 group"), missing_newest, "{body}");
        }
    }
    #[test]
    fn complete_zero_progress_can_claim_caught_up_but_unknown_age_is_explicit() {
        let text = "atlas_group_article_number{group=\"known\",mark=\"last\"} 100\natlas_group_article_number{group=\"known\",mark=\"live_cursor\"} 100\n";
        let body = lag(&native(text), 2000.0);
        assert!(body.contains("Every group is caught up"));
        assert!(body.contains("Newest-article time is unavailable for 1 group"), "{body}");
        let body = lag(
            &native(&format!("{text}atlas_group_posted_timestamp_seconds{{group=\"known\",end=\"newest\"}} 1990\n")),
            2000.0,
        );
        assert!(body.contains("Every group is caught up"));
        assert!(!body.contains("unavailable"), "{body}");
    }
}
