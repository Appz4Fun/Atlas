//! What the indexer's once-a-second status round hands over: numbers that are
//! already kept elsewhere, copied into their metrics.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde_json::{Map, Value};

use super::names::*;
use super::{Family, label_key};
use crate::nntp::{FAILURE_KINDS, ServerStat};

const STATES: [&str; 4] = ["ok", "resting", "login rejected", "article only"];

/// the `load` counters of `profile::LOAD`: (json key, what it's labelled)
const LOAD_NS: [(&str, &str); 7] = [
    ("writer_busy_ns", "writer_busy"),
    ("writer_checkpoint_ns", "writer_checkpoint"),
    ("writer_seal_ns", "writer_seal"),
    ("writer_stage_ns", "writer_stage"),
    ("xover_ns", "xover_wire"),
    ("lease_wait_ns", "connection_wait"),
    ("parse_ns", "parse"),
];
const LOAD_EVENTS: [&str; 7] = [
    "writer_batches",
    "writer_slices",
    "writer_stage_jobs",
    "writer_seal_errors",
    "xovers",
    "bg_bodies",
    "bg_body_bytes",
];

fn int(v: &Value) -> i64 {
    v.as_i64().unwrap_or(0)
}

fn total(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

/// What each server's totals last read, and in which pool: a new pool starts them over, the counters go on.
static SEEN: Mutex<BTreeMap<String, Seen>> = Mutex::new(BTreeMap::new());

#[derive(Default)]
struct Seen {
    pool: u64,
    total: u64,
}

/// How much a total of `pool` grew since it was last read.
fn growth(seen: &mut Seen, pool: u64, now: u64) -> u64 {
    let before = if seen.pool == pool { seen.total } else { 0 };
    *seen = Seen { pool, total: now };
    now.saturating_sub(before)
}

/// Add what a total kept per pool has grown by to its counter.
fn grow(family: &Family, labels: &[(&str, &str)], pool: u64, now: u64) {
    let key = format!("{}{}", family.name, label_key(labels));
    let by = growth(SEEN.lock().unwrap().entry(key).or_default(), pool, now);
    family.inc(labels, by);
}

/// One `server` label's numbers: accounts on one host and port share a key.
#[derive(Default)]
struct ServerTotals<'a> {
    pool: u64,
    open: usize,
    in_use: usize,
    limit: usize,
    configured: u64,
    waiting: usize,
    states: Vec<&'a str>,
    indexing: bool,
    headers: u64,
    wire: u64,
    text: u64,
}

pub fn report_servers(servers: &[ServerStat]) {
    let mut totals: BTreeMap<&str, ServerTotals> = BTreeMap::new();
    for s in servers {
        let t = totals.entry(s.key.as_str()).or_default();
        t.pool = s.pool;
        t.open += s.open;
        t.in_use += s.in_use;
        t.limit += s.limit;
        t.configured += u64::from(s.connections);
        t.waiting += s.waiting;
        t.states.push(s.state);
        t.indexing |= s.indexing;
        t.headers += s.headers;
        t.wire += s.wire_bytes;
        t.text += s.text_bytes;
    }

    // whole readings: a server gone from the config goes from the gauges too
    let (mut connections, mut waiting, mut states, mut indexing) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (server, t) in &totals {
        let server = *server;
        let labelled = |state: &str| label_key(&[("server", server), ("state", state)]);
        connections.push((labelled("open"), t.open as f64));
        connections.push((labelled("in_use"), t.in_use as f64));
        connections.push((labelled("idle"), t.open.saturating_sub(t.in_use) as f64));
        connections.push((labelled("limit"), t.limit as f64));
        connections.push((labelled("configured"), t.configured as f64));
        waiting.push((label_key(&[("server", server)]), t.waiting as f64));
        for state in STATES {
            states.push((labelled(state), f64::from(t.states.contains(&state))));
        }
        indexing.push((label_key(&[("server", server)]), f64::from(t.indexing)));
        grow(&NNTP_HEADERS, &[("server", server)], t.pool, t.headers);
        grow(&NNTP_TRAFFIC, &[("server", server), ("kind", "wire")], t.pool, t.wire);
        grow(&NNTP_TRAFFIC, &[("server", server), ("kind", "text")], t.pool, t.text);
        for kind in FAILURE_KINDS {
            NNTP_FAILURES.inc(&[("server", server), ("kind", kind)], 0);
        }
    }
    NNTP_CONNECTIONS.replace(connections);
    NNTP_WAITING.replace(waiting);
    NNTP_SERVER_STATE.replace(states);
    NNTP_SERVER_INDEXING.replace(indexing);
}

/// The errors counted, when the status round that reports them isnt running.
pub fn report_errors(errors: u32) {
    INDEXER_ERRORS.set_total(&[], u64::from(errors));
}

/// Which of the readings in `extra` are of this pool's services: the last ones stay there until new ones
/// replace them, and their totals start over with each pool.
pub struct Live {
    pub pool: u64,
    pub stage: bool,
    pub predb: bool,
}

/// `extra` is what the indexer puts in stats.json beside its own totals.
pub fn report_indexer(extra: &Map<String, Value>, errors: u32, idle: bool, live: Live) {
    INDEXER_RUNNING.set(&[], 1.0);
    INDEXER_IDLE.set(&[], f64::from(idle));
    INDEXER_ERRORS.set_total(&[], u64::from(errors));
    let null = Value::Null;
    let get = |key: &str| extra.get(key).unwrap_or(&null);
    INDEXER_WORKERS.set(&[], int(get("workers")) as f64);
    GROUPS_CONFIGURED.set(&[("state", "indexed")], int(get("groups_configured")) as f64);
    GROUPS_CONFIGURED.set(&[("state", "blocked")], int(get("groups_blocked")) as f64);
    report_load(get("load"));
    if live.stage {
        report_stage(get("postprocess"), live.pool);
    }
    if live.predb {
        report_predb(get("predb"), live.pool);
    }
    if let Some(titles) = get("media")["titles"].as_i64() {
        MEDIA_TITLES.set(&[], titles as f64);
    }
}

fn report_load(load: &Value) {
    for (key, label) in LOAD_NS {
        LOAD.set_total(&[("kind", label)], total(&load[key]));
    }
    for key in LOAD_EVENTS {
        LOAD_COUNT.set_total(&[("kind", key)], total(&load[key]));
    }
    WRITER_QUEUED.set(&[], int(&load["writer_queued"]) as f64);
    UNSAVED_HEADERS.set(&[("state", "now")], int(&load["unsaved_headers"]) as f64);
    UNSAVED_HEADERS.set(&[("state", "cap")], int(&load["max_unsaved_headers"]) as f64);
}

fn report_stage(stage: &Value, pool: u64) {
    if !stage.is_object() {
        return;
    }
    for key in ["queue_due", "net_wanted", "failed"] {
        STAGE_QUEUE.set(&[("state", key.trim_start_matches("queue_"))], int(&stage[key]) as f64);
    }
    STAGE_WALK.set(&[("state", "done")], int(&stage["walk"]["done"]) as f64);
    STAGE_WALK.set(&[("state", "total")], int(&stage["walk"]["total"]) as f64);
    for key in ["steps_done", "matched", "unmatched", "retries", "errors"] {
        grow(&STAGE_EVENTS, &[("kind", key)], pool, total(&stage[key]));
    }
    grow(&STAGE_EVENTS, &[("kind", "rows_walked")], pool, total(&stage["walk"]["rows"]));
    if let Some(rejected) = stage["rejected"].as_object() {
        for (reason, n) in rejected {
            grow(&STAGE_REJECTED, &[("reason", reason)], pool, total(n));
        }
    }
    grow(&STAGE_BODIES, &[], pool, total(&stage["bodies"]));
    grow(&STAGE_BODY_BYTES, &[], pool, total(&stage["body_bytes"]));
}

fn report_predb(predb: &Value, pool: u64) {
    if !predb.is_object() {
        return;
    }
    PREDB_TITLES.set(&[], int(&predb["titles"]) as f64);
    PREDB_FILE_BYTES.set(&[], int(&predb["file_bytes"]) as f64);
    PREDB_LATE_BEHIND.set(&[], int(&predb["late_behind"]) as f64);
    for key in ["titles", "reruns", "overflow", "out_of_budget", "errors"] {
        grow(&PREDB_LATE, &[("kind", key)], pool, total(&predb["late"][key]));
    }
    let Some(sources) = predb["sources"].as_array() else { return };
    for source in sources {
        let Some(feed) = source["key"].as_str() else { continue };
        let labels = [("feed", feed)];
        PREDB_FEED_FAILURES.set(&labels, int(&source["failures"]) as f64);
        PREDB_FEED_LAST_OK.set(&labels, int(&source["last_ok_at"]) as f64);
        for (key, kind) in [("inserted", "inserted"), ("updated", "updated"), ("last_rows", "last")] {
            PREDB_FEED_ROWS.set(&[("feed", feed), ("kind", kind)], int(&source[key]) as f64);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Seen, growth};

    #[test]
    fn a_new_pool_adds_all_it_has_whatever_the_old_one_read() {
        let mut seen = Seen::default();
        assert_eq!(growth(&mut seen, 1, 100), 100);
        assert_eq!(growth(&mut seen, 1, 150), 50);
        assert_eq!(growth(&mut seen, 2, 200), 200, "more than the old pool's last");
        assert_eq!(growth(&mut seen, 2, 210), 10);
        assert_eq!(growth(&mut seen, 3, 20), 20, "less than the old pool's last");
    }
}
