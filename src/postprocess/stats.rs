//! What the stage has done, for `stats.json` and `status.json`.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use serde_json::{Value, json};

use crate::store::SHARDS;

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
    pub executable: AtomicU64,
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
    /// per shard, for how soon a release's steps run (its page): what each lane got through
    pub lanes: Lanes,
}

/// Per shard counters of the lanes, and readings of them for their rates.
#[derive(Default)]
pub struct Lanes {
    /// releases the net lane ran a step of (or judged), one step each
    pub net_visits: [AtomicU64; SHARDS],
    /// of those, the releases it left with no net step to run
    pub net_finished: [AtomicU64; SHARDS],
    /// queue rows the database lane took
    pub queue_taken: [AtomicU64; SHARDS],
    /// rows a walk looked at
    pub walk_rows: [AtomicU64; SHARDS],
    readings: Mutex<VecDeque<(Instant, [[u64; SHARDS]; 4])>>,
}

/// Readings this far apart at least, and the rates over this long at most.
const READING_EVERY: f64 = 10.0;
const RATE_WINDOW: f64 = 600.0;

impl Lanes {
    fn now(&self) -> [[u64; SHARDS]; 4] {
        let read = |c: &[AtomicU64; SHARDS]| std::array::from_fn(|s| c[s].load(Relaxed));
        [read(&self.net_visits), read(&self.net_finished), read(&self.queue_taken), read(&self.walk_rows)]
    }

    /// Per minute, per shard, over the last `RATE_WINDOW` at most, for `stats.json`. Keeps a reading at most every
    /// `READING_EVERY`, soo it can be called as often as stats.json is written.
    pub(crate) fn rates(&self, at: Instant) -> Value {
        let counts = self.now();
        let mut readings = self.readings.lock().unwrap();
        if readings.back().is_none_or(|(t, _)| at.duration_since(*t).as_secs_f64() >= READING_EVERY) {
            readings.push_back((at, counts));
        }
        while readings.len() > 2
            && readings.get(1).is_some_and(|(t, _)| at.duration_since(*t).as_secs_f64() >= RATE_WINDOW)
        {
            readings.pop_front();
        }
        let Some((since, old)) = readings.front().copied() else { return Value::Null };
        let secs = at.duration_since(since).as_secs_f64();
        let per_minute = |k: usize| -> Vec<f64> {
            (0..SHARDS)
                .map(|s| if secs > 0.0 { counts[k][s].saturating_sub(old[k][s]) as f64 * 60.0 / secs } else { 0.0 })
                .map(|r| (r * 10.0).round() / 10.0)
                .collect()
        };
        json!({
            "window_secs": secs.round() as i64,
            "net_visits": per_minute(0),
            "net_finished": per_minute(1),
            "queue_taken": per_minute(2),
            "walk_rows": per_minute(3),
        })
    }
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
            Reason::Executable => &self.executable,
        };
        counter.fetch_add(1, Relaxed);
    }

    /// `enabled`: the steps that are on, a bit each
    pub(crate) fn snapshot(&self, budget_per_minute: i64, walks_per_second: i64, enabled: i64) -> Value {
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
                Reason::Executable.name(): n(&self.executable),
            },
            "archive_names_over_quota": i(&self.names_over_quota),
            "bodies": n(&self.bodies),
            "body_bytes": n(&self.body_bytes),
            "budget_per_minute": budget_per_minute,
            "backlog_per_second": walks_per_second,
            "errors": n(&self.errors),
            "enabled": enabled,
            "lanes": self.lanes.rates(Instant::now()),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn lane_rates_are_per_minute_per_shard_over_the_window() {
        let lanes = Lanes::default();
        let t0 = Instant::now();
        assert_eq!(lanes.rates(t0)["window_secs"], 0);
        lanes.net_visits[2].fetch_add(240, Relaxed);
        lanes.net_finished[2].fetch_add(60, Relaxed);
        lanes.queue_taken[7].fetch_add(30, Relaxed);
        let r = lanes.rates(t0 + Duration::from_secs(120));
        assert_eq!(r["window_secs"], 120);
        assert_eq!(r["net_visits"][2], 120.0);
        assert_eq!(r["net_finished"][2], 30.0);
        assert_eq!(r["queue_taken"][7], 15.0);
        assert_eq!(r["net_visits"][0], 0.0);
        // a reading that old drops out once there's a newer one past the window
        let r = lanes.rates(t0 + Duration::from_secs(120 + RATE_WINDOW as u64));
        assert_eq!(r["window_secs"], RATE_WINDOW as i64);
        assert_eq!(r["net_visits"][2], 0.0);
    }
}
