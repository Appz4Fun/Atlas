//! What the service has done, as Prometheus text on a small HTTP listener of
//! its own. Every number is the service's side only: the time a request takes
//! here, against the time Atlas waits for it, is what the network costs.

use std::fmt::Write as _;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use crate::Log;

/// 50µs to 2 minutes
const SECONDS: [f64; 20] = [
    0.00005, 0.0001, 0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
    30.0, 60.0, 120.0,
];

/// A histogram over `SECONDS`, each bucket counting its own observations.
pub struct Hist {
    buckets: [AtomicU64; SECONDS.len()],
    count: AtomicU64,
    /// in nanoseconds, so it adds up without a float
    sum_ns: AtomicU64,
}

impl Hist {
    const fn new() -> Hist {
        Hist {
            buckets: [const { AtomicU64::new(0) }; SECONDS.len()],
            count: AtomicU64::new(0),
            sum_ns: AtomicU64::new(0),
        }
    }

    pub fn observe(&self, took: Duration) {
        let s = took.as_secs_f64();
        self.count.fetch_add(1, Relaxed);
        self.sum_ns.fetch_add(took.as_nanos() as u64, Relaxed);
        if let Some(at) = SECONDS.iter().position(|b| s <= *b) {
            self.buckets[at].fetch_add(1, Relaxed);
        }
    }

    pub fn since(&self, started: Instant) {
        self.observe(started.elapsed());
    }

    fn render(&self, out: &mut String, name: &str, labels: &str) {
        let comma = if labels.is_empty() { "" } else { "," };
        let mut below = 0;
        for (bound, bucket) in SECONDS.iter().zip(&self.buckets) {
            below += bucket.load(Relaxed);
            let _ = writeln!(out, "{name}_bucket{{{labels}{comma}le=\"{bound}\"}} {below}");
        }
        let count = self.count.load(Relaxed).max(below);
        let braces = if labels.is_empty() { String::new() } else { format!("{{{labels}}}") };
        let _ = writeln!(out, "{name}_bucket{{{labels}{comma}le=\"+Inf\"}} {count}");
        let _ = writeln!(out, "{name}_sum{braces} {}", self.sum_ns.load(Relaxed) as f64 / 1e9);
        let _ = writeln!(out, "{name}_count{braces} {count}");
    }
}

/// One kind of request: how many, how many failed, how long each took here.
pub struct Op {
    pub requests: AtomicU64,
    pub errors: AtomicU64,
    pub seconds: Hist,
}

impl Op {
    const fn new() -> Op {
        Op { requests: AtomicU64::new(0), errors: AtomicU64::new(0), seconds: Hist::new() }
    }
}

/// Everything counted, for the life of the process.
pub struct Stats {
    pub append: Op,
    pub read: Op,
    pub stats: Op,
    pub values_appended: AtomicU64,
    pub bytes_appended: AtomicU64,
    pub values_read: AtomicU64,
    pub bytes_read: AtomicU64,
    /// group commits, and the appends and bytes that went into them
    pub commits: AtomicU64,
    pub commit_appends: AtomicU64,
    pub commit_failures: AtomicU64,
    /// writing a group and the fdatasync after it
    pub commit_seconds: Hist,
    pub fdatasync_seconds: Hist,
    /// from an append reaching the writer's queue to the writer taking it
    pub queue_seconds: Hist,
    /// reading a value off disk and checking it
    pub disk_read_seconds: Hist,
    pub checksum_failures: AtomicU64,
    pub segments_rolled: AtomicU64,
    pub connections: AtomicI64,
    pub connections_total: AtomicU64,
    pub network_received: AtomicU64,
    pub network_sent: AtomicU64,
}

pub static STATS: Stats = Stats {
    append: Op::new(),
    read: Op::new(),
    stats: Op::new(),
    values_appended: AtomicU64::new(0),
    bytes_appended: AtomicU64::new(0),
    values_read: AtomicU64::new(0),
    bytes_read: AtomicU64::new(0),
    commits: AtomicU64::new(0),
    commit_appends: AtomicU64::new(0),
    commit_failures: AtomicU64::new(0),
    commit_seconds: Hist::new(),
    fdatasync_seconds: Hist::new(),
    queue_seconds: Hist::new(),
    disk_read_seconds: Hist::new(),
    checksum_failures: AtomicU64::new(0),
    segments_rolled: AtomicU64::new(0),
    connections: AtomicI64::new(0),
    connections_total: AtomicU64::new(0),
    network_received: AtomicU64::new(0),
    network_sent: AtomicU64::new(0),
};

static STARTED: AtomicI64 = AtomicI64::new(0);

fn head(out: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
}

fn one(out: &mut String, name: &str, kind: &str, help: &str, value: impl std::fmt::Display) {
    head(out, name, kind, help);
    let _ = writeln!(out, "{name} {value}");
}

/// The filesystem the data is on: (size, free, available to us), in bytes.
#[cfg(unix)]
pub fn filesystem(dir: &Path) -> Option<(u64, u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut vfs: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut vfs) } != 0 {
        return None;
    }
    let block = vfs.f_frsize as u64;
    Some((vfs.f_blocks as u64 * block, vfs.f_bfree as u64 * block, vfs.f_bavail as u64 * block))
}

/// What a scrape gets.
pub fn render(log: &Log) -> String {
    let s = &STATS;
    let mut out = String::with_capacity(16 * 1024);
    one(&mut out, "sealed_log_up", "gauge", "Always 1 while the service answers.", 1);
    one(&mut out, "sealed_log_start_time_seconds", "gauge", "When the service started.", STARTED.load(Relaxed));

    head(&mut out, "sealed_log_requests_total", "counter", "Requests answered, by op.");
    head(&mut out, "sealed_log_request_errors_total", "counter", "Requests answered with an error, by op.");
    let ops = [("append", &s.append), ("read", &s.read), ("stats", &s.stats)];
    for (name, op) in ops {
        let _ = writeln!(out, "sealed_log_requests_total{{op=\"{name}\"}} {}", op.requests.load(Relaxed));
    }
    for (name, op) in ops {
        let _ = writeln!(out, "sealed_log_request_errors_total{{op=\"{name}\"}} {}", op.errors.load(Relaxed));
    }
    head(&mut out, "sealed_log_request_duration_seconds", "histogram", "Time a request takes in the service, by op.");
    for (name, op) in ops {
        op.seconds.render(&mut out, "sealed_log_request_duration_seconds", &format!("op=\"{name}\""));
    }

    one(&mut out, "sealed_log_values_appended_total", "counter", "Values written.", s.values_appended.load(Relaxed));
    one(&mut out, "sealed_log_bytes_appended_total", "counter", "Value bytes written.", s.bytes_appended.load(Relaxed));
    one(&mut out, "sealed_log_values_read_total", "counter", "Values read.", s.values_read.load(Relaxed));
    one(&mut out, "sealed_log_bytes_read_total", "counter", "Value bytes read.", s.bytes_read.load(Relaxed));
    one(
        &mut out,
        "sealed_log_commits_total",
        "counter",
        "Group commits (one fdatasync each).",
        s.commits.load(Relaxed),
    );
    one(
        &mut out,
        "sealed_log_commit_appends_total",
        "counter",
        "Append requests the group commits carried.",
        s.commit_appends.load(Relaxed),
    );
    one(
        &mut out,
        "sealed_log_commit_failures_total",
        "counter",
        "Group commits that failed to write.",
        s.commit_failures.load(Relaxed),
    );
    one(
        &mut out,
        "sealed_log_checksum_failures_total",
        "counter",
        "Reads whose record failed its checksum.",
        s.checksum_failures.load(Relaxed),
    );
    one(
        &mut out,
        "sealed_log_segments_rolled_total",
        "counter",
        "Segments filled and closed.",
        s.segments_rolled.load(Relaxed),
    );

    head(&mut out, "sealed_log_commit_duration_seconds", "histogram", "Writing a group and syncing it.");
    s.commit_seconds.render(&mut out, "sealed_log_commit_duration_seconds", "");
    head(&mut out, "sealed_log_fdatasync_duration_seconds", "histogram", "The fdatasync of a group commit alone.");
    s.fdatasync_seconds.render(&mut out, "sealed_log_fdatasync_duration_seconds", "");
    head(&mut out, "sealed_log_queue_duration_seconds", "histogram", "An append waiting for the writer.");
    s.queue_seconds.render(&mut out, "sealed_log_queue_duration_seconds", "");
    head(&mut out, "sealed_log_disk_read_duration_seconds", "histogram", "Reading one value off disk and checking it.");
    s.disk_read_seconds.render(&mut out, "sealed_log_disk_read_duration_seconds", "");

    one(&mut out, "sealed_log_connections", "gauge", "Connections open now.", s.connections.load(Relaxed));
    one(
        &mut out,
        "sealed_log_connections_total",
        "counter",
        "Connections accepted.",
        s.connections_total.load(Relaxed),
    );
    head(&mut out, "sealed_log_network_bytes_total", "counter", "Bytes on the request sockets, by direction.");
    let _ =
        writeln!(out, "sealed_log_network_bytes_total{{direction=\"received\"}} {}", s.network_received.load(Relaxed));
    let _ = writeln!(out, "sealed_log_network_bytes_total{{direction=\"sent\"}} {}", s.network_sent.load(Relaxed));

    let (segment, position, bytes) = log.tail();
    one(&mut out, "sealed_log_bytes", "gauge", "Bytes the log holds, records and headers.", bytes);
    one(&mut out, "sealed_log_segments", "gauge", "Segment files.", segment as u64 + 1);
    one(&mut out, "sealed_log_tail_segment_bytes", "gauge", "How far the segment being written has come.", position);
    one(
        &mut out,
        "sealed_log_segment_size_bytes",
        "gauge",
        "How big a segment grows before the next starts.",
        log.segment_bytes(),
    );
    #[cfg(unix)]
    if let Some((size, free, avail)) = filesystem(log.dir()) {
        head(
            &mut out,
            "sealed_log_filesystem_bytes",
            "gauge",
            "The data disk: size, free, avail (free to the service).",
        );
        let _ = writeln!(out, "sealed_log_filesystem_bytes{{kind=\"size\"}} {size}");
        let _ = writeln!(out, "sealed_log_filesystem_bytes{{kind=\"free\"}} {free}");
        let _ = writeln!(out, "sealed_log_filesystem_bytes{{kind=\"avail\"}} {avail}");
    }
    out
}

pub(crate) fn answer(log: &Log, stream: &mut (impl Read + Write)) -> io::Result<()> {
    // TLS/ACL admission already happened. Bound the complete HTTP header,
    // including an unterminated line, before retaining request text.
    let mut reader = BufReader::new((&mut *stream).take(8193));
    let mut first = String::new();
    let mut total = reader.read_line(&mut first)?;
    if total == 0 || total > 8192 || !first.ends_with('\n') {
        return Err(crate::invalid("invalid metrics request line"));
    }
    loop {
        let mut line = String::new();
        let count = reader.read_line(&mut line)?;
        total += count;
        if count == 0 || total > 8192 {
            return Err(crate::invalid("metrics headers exceed admission"));
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
    }
    drop(reader);
    let path = first.split_whitespace().nth(1).unwrap_or("");
    let (status, body) = if path == "/metrics" || path.starts_with("/metrics?") {
        ("200 OK", render(log))
    } else {
        ("404 Not Found", "only /metrics here\n".to_string())
    };
    let w = stream;
    write!(
        w,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    w.flush()
}

/// Authenticated scrapes require the same explicit STATS certificate grant.
pub fn serve(log: Arc<Log>, listener: TcpListener, tls: Arc<crate::ServerTls>) -> io::Result<()> {
    STARTED.store(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64),
        Relaxed,
    );
    crate::transport::serve(log, listener, tls, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value_of(text: &str, line_start: &str) -> f64 {
        let line = text.lines().find(|l| l.starts_with(line_start)).unwrap_or_else(|| panic!("{line_start} in {text}"));
        line.rsplit(' ').next().unwrap().parse().unwrap()
    }

    #[test]
    fn a_scrape_shows_what_was_written_and_read_and_the_disk() {
        let dir = std::env::temp_dir().join(format!("sealed-log-metrics-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = crate::transport::tests::open_log(&dir);
        let mut c = crate::transport::tests::peer(log.clone(), crate::Permissions::ALL);
        let places = c.append(&[(1, &[7u8; 1000][..]), (2, b"two")]).unwrap();
        c.read(&places).unwrap();
        assert!(c.read(&[crate::Locator { segment: 99, offset: 0, len: 1 }]).is_err());

        let text = crate::transport::tests::metrics_request(log.clone(), b"GET /metrics HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");

        // other tests share these counters, so at least what this one did
        assert!(value_of(&text, "sealed_log_bytes_appended_total ") >= 1003.0);
        assert!(value_of(&text, "sealed_log_values_read_total ") >= 2.0);
        assert!(value_of(&text, "sealed_log_requests_total{op=\"append\"}") >= 1.0);
        assert!(value_of(&text, "sealed_log_request_errors_total{op=\"read\"}") >= 1.0);
        assert!(value_of(&text, "sealed_log_commits_total ") >= 1.0);
        assert!(value_of(&text, "sealed_log_fdatasync_duration_seconds_count ") >= 1.0);
        assert!(value_of(&text, "sealed_log_connections ") >= 1.0);
        assert!(value_of(&text, "sealed_log_network_bytes_total{direction=\"received\"}") > 1003.0);
        assert_eq!(value_of(&text, "sealed_log_bytes "), (2 * 20 + 1003) as f64);
        assert!(value_of(&text, "sealed_log_filesystem_bytes{kind=\"size\"}") > 0.0);
        assert!(text.contains("sealed_log_request_duration_seconds_bucket{op=\"read\",le=\"+Inf\"}"), "{text}");

        let text = crate::transport::tests::metrics_request(log.clone(), b"GET / HTTP/1.1\r\n\r\n");
        assert!(text.starts_with("HTTP/1.1 404"), "{text}");
    }

    #[test]
    fn a_histogram_is_cumulative() {
        let h = Hist::new();
        h.observe(Duration::from_micros(10));
        h.observe(Duration::from_millis(3));
        h.observe(Duration::from_secs(500));
        let mut out = String::new();
        h.render(&mut out, "t", "");
        assert!(out.contains("t_bucket{le=\"0.00005\"} 1\n"), "{out}");
        assert!(out.contains("t_bucket{le=\"0.005\"} 2\n"), "{out}");
        assert!(out.contains("t_bucket{le=\"120\"} 2\n"), "{out}");
        assert!(out.contains("t_bucket{le=\"+Inf\"} 3\n"), "{out}");
        assert!(out.contains("t_count 3\n"), "{out}");
    }
}
