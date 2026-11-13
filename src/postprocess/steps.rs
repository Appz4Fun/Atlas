//! Every processing step, one line each.

use super::step::{StepId,Needs};

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
    static CATALOG:[Descriptor;20] = [
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
    ];
    &CATALOG
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn catalog_exposes_twenty_ordered_native_steps_without_constructing_services() {
        assert_eq!(catalog().len(),20);
        assert!(catalog().windows(2).all(|pair|pair[0].rank<pair[1].rank));
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
        for step in catalog() {
            assert!(ids.insert(step.id.0), "id {}", step.id.0);
            assert!(ranks.insert(step.rank), "rank {}", step.rank);
            assert!(names.insert(step.name), "{}", step.name);
            assert!(
                ranges.iter().any(|(i, r)| i.contains(&step.id.0) && r.contains(&step.rank)),
                "{} has id {} rank {}",
                step.name,
                step.id.0,
                step.rank
            );
        }
    }
}
