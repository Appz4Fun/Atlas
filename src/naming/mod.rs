//! Better names for releases from what Atlas already knows: naming regexes over subjects,
//! posted filenames, and what the archive steps read.

pub mod clean;
pub mod decide;
pub mod judge;
pub mod obfuscation;
pub mod reflag;
pub mod regexes;
pub mod steps;
pub mod unname;
pub(crate) mod walk;

pub use clean::{
    basename, clean_candidate, has_collision_suffix, identity_key, parent_dir, strip_file_suffixes,
    without_collision_suffix,
};
pub use decide::{
    best_from_paths, current_name, decide, effective_source, grade_now, improves, is_image_name, worth_running,
};
pub use judge::{Grade, distinct_episodes, is_episode_name, is_season_pack, judge, renames_pack_to_episode};

use crate::postprocess::step::StepId;

/// Where a name came from: a higher value is better evidence. `name_source` stores it.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum NameSource {
    /// the posted name. Never written: `name_source` stays NULL
    Ingest = 0,
    /// not a name: `atlas --unname` took one away, and the naming step leaves it until asked again
    Cleared = 1,
    PostedFile = 3,
    NamingRegex = 5,
    /// never written: what a good name with a NULL `name_source` counts as
    InlineBody = 9,
    Nfo = 10,
    ArchiveInner = 15,
    Par2 = 20,
    PreDb = 100,
}

impl NameSource {
    pub const fn value(self) -> u8 {
        self as u8
    }
}

pub const NAMING: StepId = StepId(8);
pub const NFO: StepId = StepId(9);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive;

    #[test]
    fn values_match_part_4() {
        assert_eq!(NameSource::Nfo.value(), archive::NAME_SOURCE_NFO);
        assert_eq!(NameSource::ArchiveInner.value(), archive::NAME_SOURCE_INNER);
        assert_eq!(NameSource::Par2.value(), archive::NAME_SOURCE_PAR2);
    }

    #[test]
    fn all_values_fit_part_3() {
        for source in [
            NameSource::PostedFile,
            NameSource::NamingRegex,
            NameSource::Nfo,
            NameSource::ArchiveInner,
            NameSource::Par2,
            NameSource::PreDb,
        ] {
            assert!((1..=127).contains(&source.value()), "{source:?}");
        }
    }
}
