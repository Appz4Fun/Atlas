//! What the indexer's once-a-second status round hands over: numbers that are
//! already kept elsewhere, copied into their metrics.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde_json::{Map, Value};

use super::label_key;
use super::names::*;
use crate::nntp::ServerStat;

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

/// What each server's totals last read: a new pool starts them over, the counters go on.
static SEEN: Mutex<BTreeMap<(String, &'static str), u64>> = Mutex::new(BTreeMap::new());

/// How much a total grew since it was `last` read.
fn growth(last: &mut u64, now: u64) -> u64 {
    let grew = if now >= *last { now - *last } else { now };
    *last = now;
    grew
}

/// One `server` label's numbers: accounts on one host and port share a key.
#[derive(Default)]
struct ServerTotals<'a> {
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
        let mut seen = SEEN.lock().unwrap();
        let mut grown = |kind: &'static str, now: u64| growth(seen.entry((server.to_string(), kind)).or_default(), now);
        NNTP_HEADERS.inc(&[("server", server)], grown("headers", t.headers));
        NNTP_TRAFFIC.inc(&[("server", server), ("kind", "wire")], grown("wire", t.wire));
        NNTP_TRAFFIC.inc(&[("server", server), ("kind", "text")], grown("text", t.text));
    }
    NNTP_CONNECTIONS.replace(connections);
    NNTP_WAITING.replace(waiting);
    NNTP_SERVER_STATE.replace(states);
    NNTP_SERVER_INDEXING.replace(indexing);
}

/// `extra` is what the indexer puts in stats.json beside its own totals.
pub fn report_indexer(extra: &Map<String, Value>, errors: u32, idle: bool) {
    INDEXER_RUNNING.set(&[], 1.0);
    INDEXER_IDLE.set(&[], f64::from(idle));
    INDEXER_ERRORS.set_total(&[], u64::from(errors));
    let null = Value::Null;
    let get = |key: &str| extra.get(key).unwrap_or(&null);
    INDEXER_WORKERS.set(&[], int(get("workers")) as f64);
    GROUPS_CONFIGURED.set(&[("state", "indexed")], int(get("groups_configured")) as f64);
    GROUPS_CONFIGURED.set(&[("state", "blocked")], int(get("groups_blocked")) as f64);
    report_load(get("load"));
    report_stage(get("postprocess"));
    report_predb(get("predb"));
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

fn report_stage(stage: &Value) {
    if !stage.is_object() {
        return;
    }
    for key in ["queue_due", "net_wanted", "failed"] {
        STAGE_QUEUE.set(&[("state", key.trim_start_matches("queue_"))], int(&stage[key]) as f64);
    }
    STAGE_WALK.set(&[("state", "done")], int(&stage["walk"]["done"]) as f64);
    STAGE_WALK.set(&[("state", "total")], int(&stage["walk"]["total"]) as f64);
    for key in ["steps_done", "matched", "unmatched", "retries", "errors"] {
        STAGE_EVENTS.set_total(&[("kind", key)], total(&stage[key]));
    }
    STAGE_EVENTS.set_total(&[("kind", "rows_walked")], total(&stage["walk"]["rows"]));
    if let Some(rejected) = stage["rejected"].as_object() {
        for (reason, n) in rejected {
            STAGE_REJECTED.set_total(&[("reason", reason)], total(n));
        }
    }
    STAGE_BODIES.set_total(&[], total(&stage["bodies"]));
    STAGE_BODY_BYTES.set_total(&[], total(&stage["body_bytes"]));
}

fn report_predb(predb: &Value) {
    if !predb.is_object() {
        return;
    }
    PREDB_TITLES.set(&[], int(&predb["titles"]) as f64);
    PREDB_FILE_BYTES.set(&[], int(&predb["file_bytes"]) as f64);
    PREDB_LATE_BEHIND.set(&[], int(&predb["late_behind"]) as f64);
    for key in ["titles", "reruns", "overflow", "out_of_budget", "errors"] {
        PREDB_LATE.set_total(&[("kind", key)], total(&predb["late"][key]));
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
    use super::growth;

    #[test]
    fn a_pool_starting_over_doesnt_take_from_the_counters() {
        let mut last = 0;
        assert_eq!(growth(&mut last, 100), 100);
        assert_eq!(growth(&mut last, 150), 50);
        assert_eq!(growth(&mut last, 20), 20, "a new pool");
        assert_eq!(growth(&mut last, 30), 10);
    }
}
