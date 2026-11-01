//! What the indexer's once-a-second status round hands over: numbers that are
//! already kept elsewhere, copied into their metrics.

use serde_json::{Map, Value};

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

pub fn report_servers(servers: &[ServerStat]) {
    for s in servers {
        let server = s.key.as_str();
        let label = |state: &'static str| [("server", server), ("state", state)];
        NNTP_CONNECTIONS.set(&label("open"), s.open as f64);
        NNTP_CONNECTIONS.set(&label("in_use"), s.in_use as f64);
        NNTP_CONNECTIONS.set(&label("idle"), s.open.saturating_sub(s.in_use) as f64);
        NNTP_CONNECTIONS.set(&label("limit"), s.limit as f64);
        NNTP_CONNECTIONS.set(&label("configured"), f64::from(s.connections));
        NNTP_WAITING.set(&[("server", server)], s.waiting as f64);
        for state in STATES {
            NNTP_SERVER_STATE.set(&label(state), f64::from(s.state == state));
        }
        NNTP_SERVER_INDEXING.set(&[("server", server)], f64::from(s.indexing));
        NNTP_HEADERS.set_total(&[("server", server)], s.headers);
        NNTP_TRAFFIC.set_total(&[("server", server), ("kind", "wire")], s.wire_bytes);
        NNTP_TRAFFIC.set_total(&[("server", server), ("kind", "text")], s.text_bytes);
    }
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
