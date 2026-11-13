//! Every processing step, one line each.

use std::sync::Arc;

use crate::archive::steps as archive;
use crate::naming::steps as naming;

use super::step::{Step, StepId, Needs};

/// Code metadata only: constructing a media Step also constructs its legacy
/// cache/service. Read-only native views must not initialize those services.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Descriptor {
    pub id: StepId,
    pub name: &'static str,
    pub rank: u16,
    pub needs: Needs,
}

pub(crate) fn catalog() -> &'static [Descriptor] {
    const fn step(id: u8, name: &'static str, rank: u16, needs: Needs) -> Descriptor {
        Descriptor { id: StepId(id), name, rank, needs }
    }
    &[
        step(1, "archive_par2", 100, Needs::Bodies), step(2, "archive_list", 110, Needs::Bodies),
        step(4, "probe", 120, Needs::Bodies), step(5, "opensubtitles", 130, Needs::External),
        step(6, "stills", 140, Needs::Bodies), step(7, "upload_link", 150, Needs::Bodies),
        step(8, "naming", 200, Needs::Database), step(9, "nfo", 210, Needs::Bodies),
        step(10, "probe_body_crc", 220, Needs::Bodies), step(11, "sidecar", 230, Needs::Bodies),
        step(12, "sidecar_image", 235, Needs::Database), step(13, "images", 240, Needs::Bodies),
        step(16, "predb_match", 300, Needs::Database), step(24, "category", 400, Needs::Database),
        step(32, "media_parse", 500, Needs::Database), step(35, "opensubtitles_imdb", 505, Needs::Database),
        step(33, "media_lookup", 510, Needs::External), step(34, "media_nfo", 520, Needs::Bodies),
        step(40, "keys", 600, Needs::Database), step(41, "audio_print", 610, Needs::Bodies),
    ]
}

pub fn all() -> Vec<Arc<dyn Step>> {
    let [parse, lookup, nfo] = crate::media::steps();
    vec![
        Arc::new(archive::Par2Step::default()),
        Arc::new(archive::ListStep::default()),
        Arc::new(crate::archive::link::LinkStep::default()),
        Arc::new(crate::probe::ProbeStep::default()),
        Arc::new(crate::probe::body_crc::BodyCrcStep),
        Arc::new(crate::probe::opensubtitles::OpenSubtitlesStep::default()),
        Arc::new(crate::probe::stills::StillsStep),
        Arc::new(naming::Naming),
        Arc::new(naming::NfoName),
        Arc::new(crate::sidecar::SidecarStep),
        Arc::new(crate::sidecar::SidecarImageStep),
        Arc::new(crate::images::step::ImagesStep::default()),
        Arc::new(crate::predb::step::PredbMatch),
        Arc::new(crate::category::CategoryStep::new()),
        parse,
        lookup,
        nfo,
        Arc::new(crate::probe::opensubtitles::OpenSubtitlesImdb),
        Arc::new(crate::keys::KeysStep::default()),
        Arc::new(crate::probe::audio_print::AudioPrintStep),
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn view_catalog_matches_actual_registered_step_metadata() {
        let mut actual: Vec<_> = all().into_iter().map(|s| Descriptor {
            id: s.id(), name: s.name(), rank: s.rank(), needs: s.needs(),
        }).collect();
        actual.sort_by_key(|s| s.rank);
        assert_eq!(catalog(), actual);
    }

    #[test]
    fn steps_have_unique_ids_ranks_names_within_their_ranges() {
        // (ids, ranks) a part owns
        let ranges = [
            (1..=7, 100..=199),
            (8..=15, 200..=299),
            (16..=23, 300..=399),
            (24..=31, 400..=499),
            (32..=39, 500..=599),
            (40..=47, 600..=699),
        ];
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
