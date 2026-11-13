//! Shared count and duration formatting.

/// 533179307 -> "533,179,307"
pub fn count(n: i64) -> String {
    let s = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// seconds -> "3d 4h", "5h 12m", "42m", "30s"
pub fn span(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "-".into();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::human_bytes;
    use crate::db;
    use serde_json::Value;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Quick {
        progress: Vec<db::GroupProgress>,
        group_chunks: BTreeMap<String, (i64, i64)>,
    }

    fn pct(part: f64, whole: f64) -> f64 {
        if whole > 0.0 { (part / whole * 100.0).clamp(0.0, 100.0) } else { 0.0 }
    }

    struct Reading {
        load: Value,
        /// (when, bytes read, bytes written)
        disk: Option<(f64, u64, u64)>,
    }

    /// The indexer's load, per second over the last `LOAD_WINDOW`.
    #[derive(Clone, Debug, Default, PartialEq)]
    struct LoadRates {
        /// share of the time the db writer was saving (0..1)
        writer_busy: f64,
        /// slices waiting for the writer now
        queued: u64,
        /// headers fetched and not saved yet now, and the cap on them
        unsaved: u64,
        max_unsaved: u64,
        /// slices saved per transaction
        per_batch: f64,
        /// share of the time the writer spent finishing checkpoints (part of writer_busy)
        writer_checkpoint: f64,
        /// seconds a header request takes on average
        latency: f64,
        /// header requests in flight on average
        in_flight: f64,
        /// requests waiting for a free connection on average
        waiting: f64,
        /// cores busy parsing headers
        parse_cores: f64,
        /// indexer disk bytes per second
        disk_read: f64,
        disk_write: f64,
    }

    fn load_rates(old: &Reading, new: &Reading) -> Option<LoadRates> {
        let num = |r: &Reading, k: &str| r.load[k].as_f64().unwrap_or(0.0);
        let dt = num(new, "at") - num(old, "at");
        if dt <= 0.0 {
            return None;
        }
        let delta = |k: &str| (num(new, k) - num(old, k)).max(0.0);
        let ns = |k: &str| delta(k) / 1e9;
        // the writers' times add up (one per shard): busy is the average of them
        let writers = new.load["writers"].as_f64().unwrap_or(1.0).max(1.0);
        let (xovers, batches) = (delta("xovers"), delta("writer_batches"));
        let (disk_read, disk_write) = match (old.disk, new.disk) {
            (Some((t0, r0, w0)), Some((t1, r1, w1))) if t1 > t0 => {
                (r1.saturating_sub(r0) as f64 / (t1 - t0), w1.saturating_sub(w0) as f64 / (t1 - t0))
            }
            _ => (0.0, 0.0),
        };
        Some(LoadRates {
            writer_busy: (ns("writer_busy_ns") / dt / writers).min(1.0),
            queued: new.load["writer_queued"].as_u64().unwrap_or(0),
            unsaved: new.load["unsaved_headers"].as_u64().unwrap_or(0),
            max_unsaved: new.load["max_unsaved_headers"].as_u64().unwrap_or(0),
            per_batch: if batches > 0.0 { delta("writer_slices") / batches } else { 0.0 },
            writer_checkpoint: (ns("writer_checkpoint_ns") / dt / writers).min(1.0),
            latency: if xovers > 0.0 { ns("xover_ns") / xovers } else { 0.0 },
            in_flight: ns("xover_ns") / dt,
            waiting: ns("lease_wait_ns") / dt,
            parse_cores: ns("parse_ns") / dt,
            disk_read,
            disk_write,
        })
    }

    /// What limits indexing right now, and why: the busiest of the db writer,
    /// the usenet connections and the CPU.
    fn diagnose(r: &LoadRates, conns: (u64, u64), cpu_cores: f64, cores: usize) -> (String, String) {
        let (in_use, allowed) = conns;
        let conn_use = if allowed > 0 { in_use as f64 / allowed as f64 } else { 0.0 };
        let cpu_use = cpu_cores / cores.max(1) as f64;

        if r.writer_busy >= 0.85 {
            let why = if r.disk_read > 50e6 {
                format!(
                    "and it mostly waits on the disk ({}/s read): the database is bigger than what stays cached",
                    human_bytes(r.disk_read)
                )
            } else {
                "and the disk isnt busy, soo it is the work of saving itself".into()
            };
            return (
                "the database writers".into(),
                format!(
                    "saving slices takes {:.0}% of the time, {} slices are waiting, {why}",
                    r.writer_busy * 100.0,
                    r.queued
                ),
            );
        }
        if conn_use >= 0.9 || r.waiting >= 1.0 {
            return (
                "usenet connections".into(),
                format!(
                    "{in_use} of {allowed} connections are busy and {:.1} requests wait for one: more connections or servers would help",
                    r.waiting
                ),
            );
        }
        if cpu_use >= 0.85 {
            return ("CPU".into(), format!("{cpu_cores:.1} of {cores} cores are busy"));
        }
        (
            "nothing is maxed out".into(),
            format!(
                "connections are {:.0}% busy and the writer {:.0}%: more groups at once (parallel_groups) would keep them busier. \
             a request takes {:.0}ms",
                conn_use * 100.0,
                r.writer_busy * 100.0,
                r.latency * 1000.0
            ),
        )
    }

    /// a group's posting rate needs indexed posts at least this far apart
    const MIN_DATED_SPAN: i64 = 30 * 60;

    /// Backfill totals, one cursor row per group. A group has a row per server it
    /// was indexed on (article numbers differ between providers); the row that got
    /// furthest stands for the group, soo nothing is counted twice.
    #[derive(Default)]
    struct Backfill {
        total: i64,
        covered: i64,
        remaining: i64,
        measured: usize,
        /// groups whose rows have no article range yet
        unmeasured: usize,
        not_started: usize,
        done: usize,
        /// groups with indexed posts far enough apart in time to tell their posting rate
        dated: usize,
        /// articles posted per day over all measured groups, from the dated ones
        per_day: Option<f64>,
        /// how far back the backfill has reached in the middle group (unix time)
        reached: Option<i64>,
        /// (group, remaining, percent)
        behind: Vec<(String, i64, f64)>,
        nearly: Vec<(String, i64, f64)>,
    }

    /// no usenet history goes back further than this (binary retention starts in the 2000s)
    const EARLIEST_POST: i64 = 631_152_000; // 1990-01-01

    /// Articles posted per day, from the dated ends of what was indexed. None
    /// when they are too close together, or the dates cant be right: posted in
    /// the future, before 1990, or a rate that would put the group's first
    /// article (`total` back) before 1990.
    fn posting_rate(row: &db::GroupProgress, total: i64, now: i64) -> Option<f64> {
        let (low, high) = (row.low?, row.high?);
        let (articles, secs) = (high.0 - low.0, high.1 - low.1);
        let plausible = |t: i64| (EARLIEST_POST..=now + 86_400).contains(&t);
        if articles <= 0 || secs < MIN_DATED_SPAN || !plausible(low.1) || !plausible(high.1) {
            return None;
        }
        let per_day = articles as f64 / (secs as f64 / 86_400.0);
        let history_days = total as f64 / per_day;
        (history_days <= (now - EARLIEST_POST) as f64 / 86_400.0).then_some(per_day)
    }

    fn backfill_at(quick: &Quick, now: i64, configured: usize) -> Backfill {
        let mut b = Backfill::default();
        let seen: std::collections::HashSet<&str> =
            quick.progress.iter().map(|row| row.key.split('@').next().unwrap_or(&row.key)).collect();
        // group -> (total, covered, its row)
        let best = db::best_progress(&quick.progress, &quick.group_chunks);
        b.unmeasured = seen.iter().filter(|g| !best.contains_key(**g)).count();

        let mut rows: Vec<(String, i64, f64)> = Vec::new();
        let (mut rate_dated, mut total_dated, mut reached) = (0.0, 0i64, Vec::new());
        for (group, (total, covered, row)) in best {
            b.measured += 1;
            b.total += total;
            b.covered += covered;
            let left = total - covered;
            b.remaining += left;
            if left == 0 {
                b.done += 1;
            }
            if let Some(rate) = posting_rate(row, total, now) {
                b.dated += 1;
                rate_dated += rate;
                total_dated += total;
                reached.extend(row.low.map(|l| l.1));
            }
            rows.push((group, left, pct(covered as f64, total as f64)));
        }

        // the dated groups' rate, scaled up to every measured group by their article numbers
        if rate_dated > 0.0 && total_dated > 0 {
            b.per_day = Some(rate_dated * b.total as f64 / total_dated as f64);
        }
        reached.sort_unstable();
        b.reached = reached.get(reached.len() / 2).copied();

        b.not_started = configured.saturating_sub(seen.len());

        rows.sort_by_key(|r| std::cmp::Reverse(r.1));
        b.behind = rows.iter().take(8).cloned().collect();
        let mut nearly: Vec<_> = rows.into_iter().filter(|r| r.1 > 0).collect();
        nearly.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.1.cmp(&b.1)));
        b.nearly = nearly.into_iter().take(8).collect();
        b
    }

    #[test]
    fn formatting() {
        assert_eq!(count(533_179_307), "533,179,307");
        assert_eq!(count(999), "999");
        assert_eq!(count(-1234), "-1,234");
        assert_eq!(span(30.0), "30s");
        assert_eq!(span(3_700.0), "1h 1m");
        assert_eq!(span(90_000.0), "1d 1h");
        assert_eq!(span(f64::INFINITY), "-");
    }

    #[test]
    fn backfill_math() {
        let row = |key: &str, live, back, first, last| db::GroupProgress {
            key: key.into(),
            live_cursor: live,
            backfill_cursor: back,
            first: Some(first),
            last: Some(last),
            ..Default::default()
        };
        // dates in 2026
        let day = 86_400;
        let base = 1_780_000_000;
        let quick = Quick {
            progress: vec![
                // 1000 numbers, 401..1000 indexed: 600 done, 400 left. 600 articles
                // posted over 2 days = 300 a day
                db::GroupProgress {
                    low: Some((400, base + 10 * day)),
                    high: Some((1000, base + 12 * day)),
                    ..row("alt.binaries.a@news.x", 1000, 400, 1, 1000)
                },
                // the same group on another server got less far: not counted again
                row("alt.binaries.a@news.y", 5000, 4900, 1, 5000),
                // done, and indexed posts only minutes apart: no rate from it
                db::GroupProgress {
                    low: Some((1, base + 10 * day)),
                    high: Some((500, base + 10 * day + 60)),
                    ..row("alt.binaries.b", 500, 0, 1, 500)
                },
                // nearly done
                row("alt.binaries.c", 100, 1, 1, 100),
                // no range yet
                db::GroupProgress {
                    key: "alt.binaries.d".into(),
                    live_cursor: 5,
                    backfill_cursor: 5,
                    ..Default::default()
                },
                // empty group (first past last)
                row("alt.binaries.e", 9, 9, 10, 9),
            ],
            ..Quick::default()
        };

        let b = backfill_at(&quick, base + 13 * day, 0);
        assert_eq!((b.total, b.covered, b.remaining), (1600, 1199, 401));
        assert_eq!((b.measured, b.unmeasured, b.done, b.dated), (4, 1, 2, 1));
        assert_eq!(b.behind[0], ("alt.binaries.a".into(), 400, 60.0));
        assert_eq!(b.nearly.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), ["alt.binaries.c", "alt.binaries.a"]);

        // 300 a day for 1000 numbers, scaled up to all 1600
        let per_day = b.per_day.unwrap();
        assert!((per_day - 480.0).abs() < 1e-9, "{per_day}");
        assert_eq!(b.reached, Some(base + 10 * day));
    }

    #[test]
    fn split_groups_progress_follows_their_chunks_then_their_sweep() {
        let row = |key: &str| db::GroupProgress {
            key: key.into(),
            live_cursor: 1000,
            backfill_cursor: 1000, // the cursor stands still on a split group
            first: Some(1),
            last: Some(1000),
            ..Default::default()
        };
        let mut quick = Quick {
            progress: vec![row("alt.binaries.s"), row("alt.binaries.t"), row("alt.binaries.u")],
            ..Quick::default()
        };
        let b = backfill_at(&quick, 1_780_000_000, 0);
        assert_eq!((b.total, b.covered, b.remaining, b.done), (3000, 0, 3000, 0), "no chunks: the cursors");

        // s: 1 of 4 days done, t: all 6 (its sweep from the cursor to go), u is not split
        quick.group_chunks = BTreeMap::from([("alt.binaries.s".into(), (1, 4)), ("alt.binaries.t".into(), (6, 6))]);
        let b = backfill_at(&quick, 1_780_000_000, 0);
        assert_eq!((b.total, b.covered, b.remaining, b.done), (3000, 250, 2750, 0));
        assert!(b.behind.contains(&("alt.binaries.t".into(), 1000, 0.0)));

        // t's sweep halfway down
        quick.progress[1].backfill_cursor = 500;
        let b = backfill_at(&quick, 1_780_000_000, 0);
        assert_eq!((b.covered, b.remaining), (250 + 500, 2250));
        assert_eq!(b.behind[0], ("alt.binaries.u".into(), 1000, 0.0));
        assert!(b.behind.contains(&("alt.binaries.s".into(), 750, 25.0)));
    }

    #[test]
    fn implausible_dates_give_no_rate() {
        let (day, now) = (86_400, 1_780_000_000);
        let row = |low: db::Dated, high: db::Dated| db::GroupProgress {
            key: "g".into(),
            live_cursor: 1_000_000,
            backfill_cursor: 0,
            first: Some(1),
            last: Some(1_000_000),
            low: Some(low),
            high: Some(high),
        };
        let rate = |r: &db::GroupProgress| posting_rate(r, 1_000_000, now);

        // 1000 articles a day: a million is 1000 days of history, fine
        assert_eq!(rate(&row((0, now - 10 * day), (10_000, now))), Some(1000.0));
        // forged 1970 dates
        assert_eq!(rate(&row((0, 0), (10_000, now))), None);
        // posted next year
        assert_eq!(rate(&row((0, now), (10_000, now + 365 * day))), None);
        // 10 articles a day would put the first article 270 years back
        assert_eq!(rate(&row((0, now - 1000 * day), (10_000, now))), None);
        // newer articles dated earlier
        assert_eq!(rate(&row((0, now), (10_000, now - day))), None);
    }

    #[test]
    fn load_rates_and_diagnosis() {
        let reading = |at: f64,
                       busy_s: f64,
                       slices: u64,
                       batches: u64,
                       xover_s: f64,
                       xovers: u64,
                       wait_s: f64,
                       disk: u64| {
            Reading {
                load: serde_json::json!({
                    "at": at, "writer_busy_ns": (busy_s * 1e9) as u64, "writer_slices": slices, "writer_batches": batches,
                    "writer_queued": 7, "xover_ns": (xover_s * 1e9) as u64, "xovers": xovers,
                    "unsaved_headers": 120_000, "max_unsaved_headers": 500_000,
                    "lease_wait_ns": (wait_s * 1e9) as u64, "parse_ns": 0
                }),
                disk: Some((at, disk, 0)),
            }
        };
        // over 10s: writer busy 9.5s, 40 slices in 5 transactions, 200 requests
        // taking 400s in total (40 in flight), 1MB/s read
        let r = load_rates(
            &reading(100.0, 0.0, 0, 0, 0.0, 0, 0.0, 0),
            &reading(110.0, 9.5, 40, 5, 400.0, 200, 0.0, 10_000_000),
        )
        .unwrap();
        assert!((r.writer_busy - 0.95).abs() < 1e-9);
        assert_eq!((r.queued, r.per_batch), (7, 8.0));
        assert_eq!((r.unsaved, r.max_unsaved), (120_000, 500_000));
        assert!((r.latency - 2.0).abs() < 1e-9 && (r.in_flight - 40.0).abs() < 1e-9);
        assert!((r.disk_read - 1e6).abs() < 1e-3);

        let cpu_idle = 1.0;
        assert_eq!(diagnose(&r, (40, 400), cpu_idle, 10).0, "the database writers");

        let quiet_writer = LoadRates { writer_busy: 0.3, ..r.clone() };
        assert_eq!(diagnose(&quiet_writer, (395, 400), cpu_idle, 10).0, "usenet connections");
        assert_eq!(
            diagnose(&LoadRates { waiting: 3.0, ..quiet_writer.clone() }, (100, 400), cpu_idle, 10).0,
            "usenet connections"
        );
        assert_eq!(diagnose(&quiet_writer, (100, 400), 9.5, 10).0, "CPU");
        assert_eq!(diagnose(&quiet_writer, (100, 400), cpu_idle, 10).0, "nothing is maxed out");

        // no time between readings: no rates
        assert_eq!(
            load_rates(&reading(5.0, 0.0, 0, 0, 0.0, 0, 0.0, 0), &reading(5.0, 1.0, 0, 0, 0.0, 0, 0.0, 0)),
            None
        );
    }

    #[test]
    fn no_rate_without_dates() {
        let quick = Quick {
            progress: vec![db::GroupProgress {
                key: "g".into(),
                live_cursor: 100,
                backfill_cursor: 50,
                first: Some(1),
                last: Some(100),
                ..Default::default()
            }],
            ..Quick::default()
        };
        let b = backfill_at(&quick, 1_780_000_000, 0);
        assert_eq!((b.per_day, b.dated, b.covered), (None, 0, 50));
    }
}
