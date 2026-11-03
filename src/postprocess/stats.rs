//! What the stage has done, for `stats.json` and `status.json`.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};

use serde_json::{Value, json};

use super::rules::Reason;

/// releases due or waiting for the network, for `status.json`
static BACKLOG: AtomicI64 = AtomicI64::new(0);

pub fn backlog() -> i64 {
    BACKLOG.load(Relaxed)
}

pub(crate) fn set_backlog(n: i64) {
    BACKLOG.store(n, Relaxed);
}

#[derive(Default)]
pub struct Stats {
    pub steps_done: AtomicU64,
    pub matched: AtomicU64,
    pub unmatched: AtomicU64,
    pub retries: AtomicU64,
    pub rows_walked: AtomicU64,
    pub blocked: AtomicU64,
    pub empty: AtomicU64,
    pub too_small: AtomicU64,
    pub fragment: AtomicU64,
    pub too_few_files: AtomicU64,
    pub par2_only: AtomicU64,
    pub bodies: AtomicU64,
    pub body_bytes: AtomicU64,
    /// bounded counts, refreshed every 30 s
    pub queue_due: AtomicI64,
    pub net_wanted: AtomicI64,
    pub failed: AtomicI64,
    /// how far the walks have come, and how far they go, in release ids
    pub walk_done: AtomicI64,
    pub walk_total: AtomicI64,
    /// releases whose inner files went unstored, past `archive.max_names_mb`
    pub names_over_quota: AtomicI64,
    pub errors: AtomicU64,
}

impl Stats {
    pub(crate) fn rejected(&self, reason: Reason) {
        let counter = match reason {
            Reason::Blocked => &self.blocked,
            Reason::Empty => &self.empty,
            Reason::TooSmall => &self.too_small,
            Reason::Fragment => &self.fragment,
            Reason::TooFewFiles => &self.too_few_files,
            Reason::Par2Only => &self.par2_only,
        };
        counter.fetch_add(1, Relaxed);
    }

    pub(crate) fn snapshot(&self, budget_per_minute: i64, walks_per_second: i64) -> Value {
        let n = |c: &AtomicU64| c.load(Relaxed);
        let i = |c: &AtomicI64| c.load(Relaxed);
        json!({
            "queue_due": i(&self.queue_due),
            "net_wanted": i(&self.net_wanted),
            "failed": i(&self.failed),
            "walk": {"done": i(&self.walk_done), "total": i(&self.walk_total), "rows": n(&self.rows_walked)},
            "steps_done": n(&self.steps_done),
            "matched": n(&self.matched),
            "unmatched": n(&self.unmatched),
            "retries": n(&self.retries),
            "rejected": {
                Reason::Blocked.name(): n(&self.blocked),
                Reason::Empty.name(): n(&self.empty),
                Reason::TooSmall.name(): n(&self.too_small),
                Reason::Fragment.name(): n(&self.fragment),
                Reason::TooFewFiles.name(): n(&self.too_few_files),
                Reason::Par2Only.name(): n(&self.par2_only),
            },
            "archive_names_over_quota": i(&self.names_over_quota),
            "bodies": n(&self.bodies),
            "body_bytes": n(&self.body_bytes),
            "budget_per_minute": budget_per_minute,
            "backlog_per_second": walks_per_second,
            "errors": n(&self.errors),
        })
    }
}
