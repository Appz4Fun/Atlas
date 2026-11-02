//! Every processing step, one line each.

use std::sync::Arc;

use crate::archive::steps as archive;
use crate::naming::steps as naming;

use super::step::Step;

pub fn all() -> Vec<Arc<dyn Step>> {
    let [parse, lookup, nfo] = crate::media::steps();
    vec![
        Arc::new(archive::Par2Step::default()),
        Arc::new(archive::ListStep::default()),
        Arc::new(crate::probe::ProbeStep::default()),
        Arc::new(naming::Naming),
        Arc::new(naming::NfoName),
        Arc::new(crate::predb::step::PredbMatch),
        Arc::new(crate::category::CategoryStep::new()),
        parse,
        lookup,
        nfo,
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn steps_have_unique_ids_ranks_names_within_their_ranges() {
        // (ids, ranks) a part owns
        let ranges =
            [(1..=7, 100..=199), (8..=15, 200..=299), (16..=23, 300..=399), (24..=31, 400..=499), (32..=39, 500..=599)];
        let (mut ids, mut ranks, mut names) = (HashSet::new(), HashSet::new(), HashSet::new());
        for step in all() {
            assert!(ids.insert(step.id().0), "id {}", step.id().0);
            assert!(ranks.insert(step.rank()), "rank {}", step.rank());
            assert!(names.insert(step.name()), "{}", step.name());
            assert!(
                ranges.iter().any(|(i, r)| i.contains(&step.id().0) && r.contains(&step.rank())),
                "{} has id {} rank {}",
                step.name(),
                step.id().0,
                step.rank()
            );
        }
    }
}
