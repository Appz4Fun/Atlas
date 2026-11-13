//! Native Step execution, retry/backoff and queue fairness helpers.
//! Retained execution code carries no database connection or shard engine.
use std::panic::{AssertUnwindSafe,catch_unwind};
use std::time::Duration;
use super::step::{Step,StepCtx,StepError,Outcome,ReleaseInfo,NameChange};

pub(crate) const NET_BATCH: i64 = 4;

const NET_PAUSE_MAX: i64 = 60;

const NET_PAUSE_AFTER: i64 = 20;

pub(crate) const NET_CRASH_RETRY: i64 = 900;

pub(crate) const STEP_DEADLINE: Duration = Duration::from_secs(120);

const TRANSIENT_FREE: i64 = 3;

pub(crate) fn net_pause_secs(in_a_row: i64) -> i64 {
    if in_a_row < NET_PAUSE_AFTER {
        return 0;
    }
    1i64.checked_shl((in_a_row - NET_PAUSE_AFTER).min(6) as u32).unwrap_or(NET_PAUSE_MAX).min(NET_PAUSE_MAX)
}

pub(crate) fn transient_wait(in_a_row: i64, release_id: i64) -> i64 {
    if in_a_row <= TRANSIENT_FREE { 300 } else { backoff(in_a_row - TRANSIENT_FREE, release_id) }
}

pub(crate) fn step_error(step: &str, error: &str) -> String {
    format!("{step}: {error}")
}

pub(crate) fn backoff(attempt: i64, release_id: i64) -> i64 {
    let shift = (attempt.max(1) - 1).min(16) as u32;
    (900i64.saturating_mul(4i64.saturating_pow(shift))).min(604_800) + release_id.rem_euclid(60)
}

fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panicked".into())
}

pub(crate) fn run_step(step: &dyn Step, ctx: &mut StepCtx<'_>) -> Ran {
    let span = crate::metrics::span(crate::metrics::step_function(step.name()));
    let ran = match catch_unwind(AssertUnwindSafe(|| step.run(ctx))) {
        Ok(Ok(mut outcome)) => {
            // the CRCs of the articles it read go with whatever it found, waiting or done
            outcome.writes.append(&mut ctx.crcs);
            Ran::Outcome(outcome)
        }
        Ok(Err(e)) => {
            ctx.crcs.clear();
            Ran::Error(e)
        }
        Err(panic) => {
            ctx.crcs.clear();
            crate::metrics::names::STEP_PANICS.inc(&[("step", step.name())], 1);
            Ran::Error(StepError::Retry(panic_text(panic)))
        }
    };
    let result = match &ran {
        Ran::Outcome(o) if o.wait_until.is_some() => "waiting",
        Ran::Outcome(o) if o.matched => "matched",
        Ran::Outcome(_) => "unmatched",
        Ran::Error(StepError::Native(_)) => "storage_error",
        Ran::Error(StepError::Stopped) => "stopped",
        Ran::Error(StepError::OutOfBudget) => "out_of_budget",
        Ran::Error(StepError::Retry(_)) => "retry",
        Ran::Error(StepError::Permanent(_)) => "permanent",
        Ran::Error(StepError::Transient(_)) => "transient",
    };
    if matches!(result, "retry" | "permanent" | "transient" | "storage_error") {
        span.failed();
    }
    crate::metrics::names::STEP_RUNS.inc(&[("step", step.name()), ("result", result)], 1);
    ran
}

pub(crate) fn name_counts(release: &ReleaseInfo, name: &NameChange) -> bool {
    i64::from(name.source) >= release.name_source.unwrap_or(0)
        && (name.display_name.is_some() || name.obfuscated.is_some())
}

pub(crate) fn apply_name(release: &mut ReleaseInfo, name: &NameChange) {
    if let Some(d) = &name.display_name {
        release.display_name = Some(d.clone());
    }
    if let Some(o) = name.obfuscated {
        release.is_obfuscated = o;
    }
    release.name_source = Some(i64::from(name.source));
}

pub(crate) fn claim_order(claims: i64) -> &'static str {
    match claims % 8 {
        7 => "release_id asc",
        3 => "release_id desc",
        _ => "net_rank desc, release_id desc",
    }
}

pub(crate) fn queue_order(batches: i64) -> &'static str {
    if batches % 4 == 3 { "due_at" } else { "due_at desc" }
}

pub(crate) fn claim_shares() -> (f64, f64, f64) {
    let (mut rank, mut newest, mut oldest) = (0.0, 0.0, 0.0);
    for claims in 0..8 {
        match claim_order(claims) {
            "release_id asc" => oldest += 1.0,
            "release_id desc" => newest += 1.0,
            _ => rank += 1.0,
        }
    }
    (rank / 8.0, newest / 8.0, oldest / 8.0)
}

pub(crate) fn queue_shares() -> (f64, f64) {
    let latest = (0..4).filter(|b| queue_order(*b) == "due_at desc").count() as f64;
    (latest / 4.0, 1.0 - latest / 4.0)
}

pub(crate) enum Ran {
    /// a result, or an error the lane acts on
    Outcome(Outcome),
    Error(StepError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_shares_follow_the_turns() {
        assert_eq!(claim_shares(), (0.75, 0.125, 0.125));
        assert_eq!(queue_shares(), (0.75, 0.25));
    }

    #[test]
    fn transient_failures_back_off_after_a_few() {
        let waits: Vec<i64> = (1..=7).map(|n| transient_wait(n, 0)).collect();
        assert_eq!(waits, [300, 300, 300, 900, 3600, 14400, 57600]);
        assert_eq!(transient_wait(40, 0), 604_800, "a week at most");
    }

    #[test]
    fn a_few_bad_articles_dont_rest_the_net_lane_but_a_run_of_them_does() {
        assert_eq!(net_pause_secs(1), 0);
        assert_eq!(net_pause_secs(NET_PAUSE_AFTER - 1), 0);
        assert_eq!(net_pause_secs(NET_PAUSE_AFTER), 1);
        assert_eq!(net_pause_secs(NET_PAUSE_AFTER + 3), 8);
        assert_eq!(net_pause_secs(10_000), NET_PAUSE_MAX);
    }
}
