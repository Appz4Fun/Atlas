//! Native archive result data and typed writes. Reads belong to the checked PostgreSQL owner.

use super::{Encryption, Password};

pub const SOURCE_PAR2: i64 = 1;
pub const SOURCE_RAR: i64 = 2;
pub const SOURCE_7Z: i64 = 3;
pub const SOURCE_ZIP: i64 = 4;
/// the files of the post a posted NZB points to, not what's inside them
pub const SOURCE_NZB: i64 = 5;

/// What a list is read with now: 1, each file's CRC32, time and host, the packer and the archive's keys. A list kept
/// with less is read again (`raw_store::get_archive`).
pub const FACTS: i64 = 1;

/// What one archive's headers come to beyond its file list, as `archive_release` keeps it: the keys are digests,
/// soo nothing here names the uploader.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArchiveKeys {
    pub packer: Option<String>,
    pub twin_key: Option<i64>,
    pub owner_key: Option<i64>,
    pub zone_key: Option<i64>,
}

impl ArchiveKeys {
    /// The keys over a listing. `salt`: the install's key salt; without it no salted key is made. `posted`: the post's
    /// Date as a wall clock (`dates::naive_timestamp`), for a packer zone guessed against it.
    pub fn of(listing: &super::Listing, salt: Option<i64>, posted: Option<i64>) -> ArchiveKeys {
        use crate::keys::{as_key, digest8, salted8};
        let packer = listing.packer.clone();
        let zone = listing.zone.or_else(|| {
            listing.local_wall.zip(posted).and_then(|(wall, at)| super::facts::zone_against_post(wall, at))
        });
        let salted = |text: &super::facts::Private| salt.map(|s| as_key(salted8(s, &text.0)));
        ArchiveKeys {
            twin_key: listing.twin.as_ref().map(|t| as_key(digest8(&t.text()))),
            owner_key: listing.owner.as_ref().and_then(salted),
            zone_key: zone.map(|z| super::facts::zone_text(z, packer.as_deref())).as_ref().and_then(salted),
            packer,
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == ArchiveKeys::default()
    }
}

/// The writes that keep a set's files, every one of them: the small ones and the ones alike too, soo the set reads
/// back whole.
pub fn par2_file_writes(
    release_id: i64,
    inventory: &crate::par2::Inventory,
) -> Vec<crate::postprocess::step::StepWrite> {
    use crate::postprocess::step::{StepMutation, StepWrite};
    let mut writes = vec![StepWrite::for_mutation(release_id, StepMutation::ClearPar2Files)];
    for f in &inventory.files {
        let sealed = inventory.sealed.contains(&(f.set_id, f.file_id)) && f.bound;
        writes.push(StepWrite::for_mutation(release_id, StepMutation::KeepPar2File { file: f.clone(),sealed,complete: inventory.complete }));
    }
    for (set_id, creator) in &inventory.creators {
        writes.push(StepWrite::for_mutation(release_id, StepMutation::SetPar2Creator { set_id: *set_id,creator: creator.clone() }));
    }
    // a whole set says which files the upload has: with each of them there and whole, so is the release, whatever
    // its subject counter says (`store::SET_VOUCHED_COMPLETE`)
    if inventory.complete && !inventory.files.is_empty() {
        writes.push(StepWrite::for_mutation(release_id, StepMutation::VouchComplete));
    }
    writes
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InnerFileRow {
    pub seq: i64,
    pub name: String,
    pub size: Option<i64>,
    pub source: i64,
}

/// What the release view shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InnerView {
    pub password: Option<Password>,
    pub encryption: Option<Encryption>,
    pub names_hidden: bool,
    pub password_check: Option<super::PasswordTest>,
    pub files: Vec<InnerFileRow>,
    /// how the first file was packed (`archive_release.packer`)
    pub packer: Option<String>,
}
