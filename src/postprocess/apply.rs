//! Native visit data consumed by the original-owner queue transaction.


use super::step::{NameChange, StepMask, StepWrite};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Lane {
    #[default]
    Db,
    Net,
}

/// One release's visit. Masks are deltas: bits nobody touched stay as they are.
#[derive(Default)]
pub(crate) struct Applied {
    pub release_id: i64,
    pub lane: Lane,
    /// the queue row's time when it was read: an article since then moved it
    pub claimed_due: Option<i64>,
    /// `rerun_gen` read with the row, 0 with no row
    pub claimed_gen: i64,
    pub set_done: StepMask,
    pub set_failed: StepMask,
    pub clear: StepMask,
    /// cleared only if the name really changes
    pub rename_clears: StepMask,
    /// net steps left, for `net_wanted`
    pub net_mask: StepMask,
    /// every database step and every net step: a cleared one has to come back
    pub db_mask: StepMask,
    pub net_steps: StepMask,
    /// each with the step that made it
    pub writes: Vec<(StepMask, StepWrite)>,
    pub names: Vec<(StepMask, NameChange)>,
    pub db_attempts: Option<i64>,
    pub net_attempts: Option<i64>,
    /// transient net failures in a row (see `engine::transient_wait`)
    pub net_transient: Option<i64>,
    pub net_next_at: Option<i64>,
    pub parts_seen: Option<i64>,
    pub error: Option<String>,
    /// a wait or a retry
    pub queue_at: Option<i64>,
    /// nothing worth a `pp_state` row
    pub no_state: bool,
    /// deleted, with why, in place of all else here (see `junk`)
    pub junk: Option<&'static str>,
    /// a database-lane visit that settled it: the freezing rules look at it once all else here is written
    pub freeze_check: bool,
}

impl Applied {
    /// Fresh native releases stay in PostgreSQL. The legacy setting still
    /// governs SQLite shards, but never requests their frozen-file routing here.
    pub(crate) fn native_database(release_id:i64,claimed_gen:i64,claimed_due:i64,db_mask:StepMask,net_steps:StepMask)->Self {
        Self {release_id,claimed_gen,claimed_due:Some(claimed_due),db_mask,net_steps,..Default::default()}
    }
    pub(crate) fn native_network(release_id:i64,claimed_gen:i64,db_mask:StepMask,net_steps:StepMask)->Self {
        Self {release_id,claimed_gen,lane:Lane::Net,db_mask,net_steps,..Default::default()}
    }
}

/// Names a transaction wrote: each one's source, and whether it was the first
/// real name of an obfuscated post. Counted once they are committed, see `count_names`.
pub(crate) type Renames = Vec<(u8, bool)>;

pub(crate) fn count_names(renames: &[(u8, bool)]) {
    use crate::metrics::names::{DEOBFUSCATED, NAMES_APPLIED};
    for (source, first) in renames {
        let from = crate::metrics::catalog::name_source_name(*source);
        NAMES_APPLIED.inc(&[("source", from)], 1);
        if *first {
            DEOBFUSCATED.inc(&[("source", from)], 1);
        }
    }
}
