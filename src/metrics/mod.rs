//! Prometheus metrics: a small registry, the numbers every part of atlas
//! reports into it, and the `/metrics` listener.
//!
//! The indexer and the menu are separate processes and each serves its own
//! numbers, on its own port (`metrics.indexer_port`, `metrics.api_port`). A
//! counter starts over with its process, which Prometheus rate functions
//! expect. What outlives a restart is the catalog scan, see `catalog`.

pub mod catalog;
pub mod names;
mod report;
mod server;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::Instant;

pub use report::{report_errors, report_indexer, report_servers};
pub use server::{Role, serve, serve_when_free, start};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Counter,
    Gauge,
    Histogram,
}

/// One series. A histogram's `buckets` line up with its family's bounds, each
/// counting its own observations only: rendering adds them up.
struct Series {
    /// a counter's count, or a gauge's f64 bits
    value: AtomicU64,
    buckets: Vec<AtomicU64>,
    /// a histogram's sum, as f64 bits
    sum: AtomicU64,
}

/// A metric and all its series, one per set of label values.
pub struct Family {
    pub name: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    pub bounds: &'static [f64],
    series: LazyLock<RwLock<BTreeMap<String, Arc<Series>>>>,
}

fn add_bits(cell: &AtomicU64, by: f64) {
    let _ = cell.try_update(Relaxed, Relaxed, |bits| Some((f64::from_bits(bits) + by).to_bits()));
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn label_key(labels: &[(&str, &str)]) -> String {
    let mut key = String::new();
    for (i, (name, value)) in labels.iter().enumerate() {
        if i > 0 {
            key.push(',');
        }
        let _ = write!(key, "{name}=\"{}\"", escape(value));
    }
    key
}

impl Family {
    pub const fn new(name: &'static str, help: &'static str, kind: Kind, bounds: &'static [f64]) -> Family {
        Family { name, help, kind, bounds, series: LazyLock::new(Default::default) }
    }

    fn series(&self, key: String) -> Arc<Series> {
        if let Some(found) = self.series.read().unwrap().get(&key) {
            return found.clone();
        }
        let fresh = Series {
            value: AtomicU64::new(0),
            buckets: self.bounds.iter().map(|_| AtomicU64::new(0)).collect(),
            sum: AtomicU64::new(0),
        };
        self.series.write().unwrap().entry(key).or_insert_with(|| Arc::new(fresh)).clone()
    }

    /// add to a counter
    pub fn inc(&self, labels: &[(&str, &str)], by: u64) {
        self.series(label_key(labels)).value.fetch_add(by, Relaxed);
    }

    /// a counter's total, for numbers that are already kept elsewhere and only ever grow
    pub fn set_total(&self, labels: &[(&str, &str)], total: u64) {
        self.series(label_key(labels)).value.store(total, Relaxed);
    }

    pub fn set(&self, labels: &[(&str, &str)], value: f64) {
        self.series(label_key(labels)).value.store(value.to_bits(), Relaxed);
    }

    pub fn add(&self, labels: &[(&str, &str)], by: f64) {
        add_bits(&self.series(label_key(labels)).value, by);
    }

    pub fn observe(&self, labels: &[(&str, &str)], value: f64) {
        let series = self.series(label_key(labels));
        series.value.fetch_add(1, Relaxed);
        add_bits(&series.sum, value);
        // past the last bound it's only in +Inf, which is the count
        if let Some(at) = self.bounds.iter().position(|b| value <= *b) {
            series.buckets[at].fetch_add(1, Relaxed);
        }
    }

    /// Every series of a gauge at once: one a new reading leaves out is gone.
    pub fn replace(&self, rows: impl IntoIterator<Item = (String, f64)>) {
        let fresh: BTreeMap<String, Arc<Series>> = rows
            .into_iter()
            .map(|(key, value)| {
                let series =
                    Series { value: AtomicU64::new(value.to_bits()), buckets: Vec::new(), sum: AtomicU64::new(0) };
                (key, Arc::new(series))
            })
            .collect();
        *self.series.write().unwrap() = fresh;
    }

    /// A gauge's rows, for saving (labels as `replace` takes them).
    pub fn rows(&self) -> Vec<(String, f64)> {
        let series = self.series.read().unwrap();
        series.iter().map(|(key, s)| (key.clone(), f64::from_bits(s.value.load(Relaxed)))).collect()
    }

    fn render(&self, out: &mut String) {
        let series = self.series.read().unwrap();
        if series.is_empty() {
            return;
        }
        let kind = match self.kind {
            Kind::Counter => "counter",
            Kind::Gauge => "gauge",
            Kind::Histogram => "histogram",
        };
        let _ = writeln!(out, "# HELP {} {}", self.name, self.help);
        let _ = writeln!(out, "# TYPE {} {kind}", self.name);
        for (key, s) in series.iter() {
            let braces = if key.is_empty() { String::new() } else { format!("{{{key}}}") };
            match self.kind {
                Kind::Counter => {
                    let _ = writeln!(out, "{}{braces} {}", self.name, s.value.load(Relaxed));
                }
                Kind::Gauge => {
                    let _ = writeln!(out, "{}{braces} {}", self.name, f64::from_bits(s.value.load(Relaxed)));
                }
                Kind::Histogram => self.render_histogram(out, key, s),
            }
        }
    }

    fn render_histogram(&self, out: &mut String, key: &str, s: &Series) {
        let comma = if key.is_empty() { "" } else { "," };
        let mut below = 0;
        for (bound, bucket) in self.bounds.iter().zip(&s.buckets) {
            below += bucket.load(Relaxed);
            let _ = writeln!(out, "{}_bucket{{{key}{comma}le=\"{bound}\"}} {below}", self.name);
        }
        // read after the buckets, soo +Inf is never below one of them
        let count = s.value.load(Relaxed).max(below);
        let _ = writeln!(out, "{}_bucket{{{key}{comma}le=\"+Inf\"}} {count}", self.name);
        let braces = if key.is_empty() { String::new() } else { format!("{{{key}}}") };
        let _ = writeln!(out, "{}_sum{braces} {}", self.name, f64::from_bits(s.sum.load(Relaxed)));
        let _ = writeln!(out, "{}_count{braces} {count}", self.name);
    }
}

/// The text a scrape gets: every family with at least one series.
pub fn render() -> String {
    let mut out = String::with_capacity(64 * 1024);
    for family in names::ALL {
        family.render(&mut out);
    }
    out
}

/// Times one call of `function` until dropped: how long it took, how many are
/// running, and (with `failed`) how many went wrong.
pub struct Span {
    function: &'static str,
    started: Instant,
}

pub fn span(function: &'static str) -> Span {
    names::FUNCTION_IN_PROGRESS.add(&[("function", function)], 1.0);
    Span { function, started: Instant::now() }
}

impl Span {
    pub fn failed(&self) {
        names::FUNCTION_ERRORS.inc(&[("function", self.function)], 1);
    }

    /// bytes this call moved, `direction` being `received` or `sent`
    pub fn bytes(&self, direction: &'static str, n: u64) {
        function_bytes(self.function, direction, n);
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let labels = [("function", self.function)];
        names::FUNCTION_IN_PROGRESS.add(&labels, -1.0);
        names::FUNCTION_DURATION.observe(&labels, self.started.elapsed().as_secs_f64());
    }
}

/// `step.<name>` for a processing step's span: the steps are few and live as
/// long as the process, soo their labels are made once and kept.
pub fn step_function(step: &'static str) -> &'static str {
    static MADE: LazyLock<RwLock<BTreeMap<&'static str, &'static str>>> = LazyLock::new(Default::default);
    if let Some(found) = MADE.read().unwrap().get(step) {
        return found;
    }
    MADE.write().unwrap().entry(step).or_insert_with(|| Box::leak(format!("step.{step}").into_boxed_str()))
}

/// Bytes on behalf of a function that isnt timed in one piece.
pub fn function_bytes(function: &str, direction: &str, n: u64) {
    if n > 0 {
        names::FUNCTION_BYTES.inc(&[("function", function), ("direction", direction)], n);
    }
}

/// Runs `f` as a span of `function`, marking an `Err` as failed.
pub fn timed<T, E>(function: &'static str, f: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    let span = span(function);
    let result = f();
    if result.is_err() {
        span.failed();
    }
    result
}

/// when this process's numbers began, for `atlas_process_start_time_seconds`
pub(crate) static STARTED_AT: AtomicI64 = AtomicI64::new(0);

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: &[f64] = &[1.0, 10.0];

    #[test]
    fn a_counter_renders_with_help_type_and_escaped_labels() {
        static F: Family = Family::new("t_counter_total", "Things.", Kind::Counter, &[]);
        F.inc(&[("host", "a\"b\\c\nd")], 2);
        F.inc(&[("host", "a\"b\\c\nd")], 3);
        let mut out = String::new();
        F.render(&mut out);
        assert_eq!(
            out,
            "# HELP t_counter_total Things.\n# TYPE t_counter_total counter\nt_counter_total{host=\"a\\\"b\\\\c\\nd\"} 5\n"
        );
    }

    #[test]
    fn a_family_with_no_series_says_nothing() {
        static F: Family = Family::new("t_empty", "Nothing.", Kind::Gauge, &[]);
        let mut out = String::new();
        F.render(&mut out);
        assert_eq!(out, "");
    }

    #[test]
    fn a_histogram_is_cumulative_and_past_the_last_bound_only_in_inf() {
        static F: Family = Family::new("t_hist", "Sizes.", Kind::Histogram, BOUNDS);
        for value in [0.5, 5.0, 5.0, 50.0] {
            F.observe(&[("a", "b")], value);
        }
        let mut out = String::new();
        F.render(&mut out);
        for line in [
            "t_hist_bucket{a=\"b\",le=\"1\"} 1",
            "t_hist_bucket{a=\"b\",le=\"10\"} 3",
            "t_hist_bucket{a=\"b\",le=\"+Inf\"} 4",
            "t_hist_sum{a=\"b\"} 60.5",
            "t_hist_count{a=\"b\"} 4",
        ] {
            assert!(out.contains(&format!("{line}\n")), "{line} in {out}");
        }
    }

    #[test]
    fn a_histogram_with_no_labels_has_no_empty_braces() {
        static F: Family = Family::new("t_hist_plain", "Sizes.", Kind::Histogram, BOUNDS);
        F.observe(&[], 2.0);
        let mut out = String::new();
        F.render(&mut out);
        assert!(out.contains("t_hist_plain_bucket{le=\"1\"} 0\n"), "{out}");
        assert!(out.contains("t_hist_plain_sum 2\n"), "{out}");
    }

    #[test]
    fn replace_drops_a_series_the_new_reading_left_out() {
        static F: Family = Family::new("t_gauge", "Top.", Kind::Gauge, &[]);
        F.set(&[("group", "old")], 1.0);
        F.replace([(label_key(&[("group", "new")]), 2.5)]);
        let mut out = String::new();
        F.render(&mut out);
        assert!(!out.contains("old"), "{out}");
        assert!(out.contains("t_gauge{group=\"new\"} 2.5\n"), "{out}");
    }

    #[test]
    fn a_span_counts_the_call_and_settles_in_progress() {
        {
            let s = span("test.span");
            s.failed();
            s.bytes("received", 7);
            let out = render();
            assert!(out.contains("atlas_function_in_progress{function=\"test.span\"} 1\n"), "{out}");
        }
        let out = render();
        assert!(out.contains("atlas_function_in_progress{function=\"test.span\"} 0\n"), "{out}");
        assert!(out.contains("atlas_function_duration_seconds_count{function=\"test.span\"} 1\n"), "{out}");
        assert!(out.contains("atlas_function_errors_total{function=\"test.span\"} 1\n"), "{out}");
        assert!(
            out.contains("atlas_function_bytes_total{direction=\"received\",function=\"test.span\"} 7\n")
                || out.contains("atlas_function_bytes_total{function=\"test.span\",direction=\"received\"} 7\n"),
            "{out}"
        );
    }

    #[test]
    fn timed_marks_an_err_only() {
        assert_eq!(timed::<_, ()>("test.timed_ok", || Ok(1)), Ok(1));
        assert_eq!(timed::<i32, _>("test.timed_err", || Err("no")), Err("no"));
        let out = render();
        assert!(!out.contains("atlas_function_errors_total{function=\"test.timed_ok\"}"), "{out}");
        assert!(out.contains("atlas_function_errors_total{function=\"test.timed_err\"} 1\n"), "{out}");
    }

    #[test]
    fn every_family_has_a_unique_well_formed_name() {
        let mut seen = std::collections::HashSet::new();
        for f in names::ALL {
            assert!(seen.insert(f.name), "{} twice", f.name);
            assert!(f.name.starts_with("atlas_"), "{}", f.name);
            assert!(f.name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'), "{}", f.name);
            assert!(!f.help.is_empty(), "{}", f.name);
            if f.kind == Kind::Counter {
                assert!(f.name.ends_with("_total"), "{} is a counter", f.name);
            }
            if f.kind == Kind::Histogram {
                assert!(f.bounds.windows(2).all(|w| w[0] < w[1]), "{} bounds", f.name);
            }
        }
    }
}
