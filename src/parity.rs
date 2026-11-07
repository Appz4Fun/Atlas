//! Atlas's PAR2 library: a thin door to the `par_rs` crate.
//!
//! Every PAR2 and CRC-32 computation goes through `par_rs`; this module only picks the
//! entry points Atlas uses, fixes the options Atlas wants (packet MD5s checked on metadata,
//! never on recovery data) and names the results. Nothing here re-implements PAR2 or
//! GF(2) math. An API Atlas needs that `par_rs` lacks is written down in
//! `docs/PARITY_REQUESTS.md` instead of being worked around here.
//!
//! The `par_rs` parsing modules never panic on untrusted bytes: a truncated, corrupt or
//! hostile buffer gives a partial result. Treat every return as data, not as an error path.
//!
//! Names follow `par_rs`, not the older atlas-parity spec (see the crate's README, "API notes
//! and deviations"): `Inventory::block_size` (not `slice_size`), `Par2File::size` (not
//! `length`), `Par2File::md5_16k` (not `hash_16k`).
//!
//! What it covers:
//! - sniffing: [`format_of`], [`par2_name`], [`is_base_par2`];
//! - packets: [`inspect`] ([`Par2Inspection`]: Main, FileDesc, IFSC, recovery slices,
//!   Creator, comments, unknown packets, resyncs over garbage), [`creator`];
//! - the files a set describes: [`inventory`], [`inventory_of`], kept in `libpar2` order
//!   (file id as a little-endian u128) for presenting and comparing sets;
//! - obfuscated names: [`resolve`], [`real_names`], [`rar5_volume`];
//! - CRC algebra on yEnc/NZB metadata without bodies: [`file_crc`], [`file_crc_indexed`],
//!   [`file_crc_from_ifsc`], [`articles_agree_with_ifsc`] and the re-exported `crc32_*`,
//!   [`lcm_grid`], [`compute_interval_crcs`];
//! - slice verification of bytes Atlas does have: [`verifier`], [`usable_recovery`] and
//!   [`SetVerifier`]'s `verify_file` / `verify_files` / `sliding_search` / `assess`.
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::collections::BTreeSet;

pub use par_rs::bridge::{BridgeReport, Candidate, Par2Bridge, RAR5_SIGNATURE, Resolution, ResolveMethod};
pub use par_rs::gf2::{
    FoldError, compute_interval_crcs, crc32, crc32_combine, crc32_uncombine_prefix, crc32_uncombine_suffix,
    crc32_unzero, crc32_zeros, lcm_grid,
};
pub use par_rs::inspect::{
    CommentInfo, CreatorInfo, CreatorTool, FileDescInfo, IfscInfo, MainInfo, PacketInfo, PacketKind, Par2FileName,
    Par2Inspection, ParFormat, RecoverySliceInfo, UnicodeNameInfo, VerifyMode,
};
pub use par_rs::par2::{Inventory, Par2File};
pub use par_rs::verify::{
    FileVerification, FoundSlice, RepairAssessment, SetVerifier, SliceStatus, SourceSpec, VerificationReport,
};

// ---------------------------------------------------------------------------------------------
// Sniffing
// ---------------------------------------------------------------------------------------------

/// Which parity format `bytes` start with: PAR1, PAR2 or PAR3. `None` for anything else.
pub fn format_of(bytes: &[u8]) -> Option<ParFormat> {
    par_rs::detect_format(bytes)
}

/// `name.par2`, `name.volNNN+MM.par2` or `name.volNNN-MMM.par2` taken apart: the base name,
/// the first recovery block and block count of a volume, and whether it is the index file.
/// `None` for a name that isn't a `.par2`.
pub fn par2_name(name: &str) -> Option<Par2FileName> {
    par_rs::parse_par2_filename(name)
}

/// Does a Usenet subject name the base `.par2` index (not a `.volNNN+MM.par2` volume)?
pub fn is_base_par2(subject: &str) -> bool {
    par_rs::is_base_par2(subject)
}

// ---------------------------------------------------------------------------------------------
// Packets
// ---------------------------------------------------------------------------------------------

/// Every packet in `bytes`, metadata packets MD5-checked (`VerifyMode::Metadata`): a
/// FileDesc, Main or IFSC whose hash fails counts in `bad_md5` and is not believed, while the
/// bulk recovery data is located but not hashed.
pub fn inspect(bytes: &[u8]) -> Par2Inspection {
    par_rs::inspect_par2_with(bytes, VerifyMode::Metadata)
}

/// The tool, version and platform a Creator packet body names.
pub fn creator(body: &[u8]) -> CreatorInfo {
    CreatorInfo::parse(body)
}

// ---------------------------------------------------------------------------------------------
// The files a set describes
// ---------------------------------------------------------------------------------------------

/// The files the set in `index` describes, primary set first and in `libpar2` order within a
/// set. Keep that order when presenting or comparing sets.
pub fn inventory(index: &[u8]) -> Inventory {
    par_rs::inventory(index)
}

/// [`inventory`] from an inspection already made, so a buffer is scanned once.
pub fn inventory_of(inspection: &Par2Inspection) -> Inventory {
    par_rs::par2::inventory_from(inspection)
}

// ---------------------------------------------------------------------------------------------
// Obfuscated names
// ---------------------------------------------------------------------------------------------

/// Which of `candidates` (downloaded or listed files under obfuscated names) is which file of
/// `inventory`. A [`Resolution`]'s `file` indexes `inventory.files`; its `method` says whether
/// the match was by unique size, 16 KiB MD5, folded CRC-32 or RAR5 volume number.
pub fn resolve(inventory: &Inventory, candidates: &[Candidate]) -> BridgeReport {
    Par2Bridge::new(inventory).resolve(candidates)
}

/// `(candidate index, real name, method)` for each candidate `report` resolved, in candidate
/// order. A resolution pointing outside `inventory.files` (a report from another inventory)
/// is left out.
pub fn real_names<'a>(inventory: &'a Inventory, report: &BridgeReport) -> Vec<(usize, &'a str, ResolveMethod)> {
    report
        .resolved
        .iter()
        .filter_map(|r| inventory.files.get(r.file).map(|f| (r.candidate, f.name.as_str(), r.method)))
        .collect()
}

/// The volume number in a raw RAR5 main archive header (`0` for the first volume). `None`
/// when `head` isn't RAR5 or the header is cut short.
pub fn rar5_volume(head: &[u8]) -> Option<u64> {
    par_rs::rar5_volume_number(head)
}

// ---------------------------------------------------------------------------------------------
// CRC algebra on yEnc / NZB metadata
// ---------------------------------------------------------------------------------------------

/// A file's whole CRC-32 from its articles' `(crc32, decoded length)`, in part order.
pub fn file_crc(articles: &[(u32, usize)]) -> Result<u32, FoldError> {
    par_rs::fold_yenc_parts(articles)
}

/// [`file_crc`] for articles in any order, as `(part index, crc32, length)`: every index in
/// `0..total` must be there once (a repeat must agree).
pub fn file_crc_indexed(total: usize, articles: &[(usize, u32, usize)]) -> Result<u32, FoldError> {
    par_rs::fold_yenc_indexed_parts(total, articles)
}

/// A file's whole CRC-32 from its IFSC slice CRCs (the last slice zero padded to
/// `block_size`). `None` when the slice count doesn't fit `file_size`.
pub fn file_crc_from_ifsc(slice_crcs: &[u32], block_size: u64, file_size: u64) -> Option<u32> {
    par_rs::fold_ifsc_crcs(slice_crcs, block_size, file_size)
}

/// Do a file's articles and its IFSC slice CRCs describe the same bytes? Both are folded to
/// the whole-file CRC-32 and compared. `None` when either side can't be folded (no articles,
/// a slice count that doesn't fit `file_size`) or the articles' lengths don't add up to
/// `file_size`: nothing can be said then.
pub fn articles_agree_with_ifsc(
    articles: &[(u32, usize)],
    slice_crcs: &[u32],
    block_size: u64,
    file_size: u64,
) -> Option<bool> {
    let posted = articles.iter().try_fold(0u64, |sum, &(_, len)| sum.checked_add(u64::try_from(len).ok()?))?;
    if posted != file_size {
        return None;
    }
    let from_articles = file_crc(articles).ok()?;
    let from_ifsc = file_crc_from_ifsc(slice_crcs, block_size, file_size)?;
    Some(from_articles == from_ifsc)
}

// ---------------------------------------------------------------------------------------------
// Slice verification
// ---------------------------------------------------------------------------------------------

/// A slice verifier for the primary set of `index` (the set of its first Main packet).
/// `None` without a Main packet or with a block size of 0. Files without an IFSC packet are
/// left out: they can't be checked slice by slice.
pub fn verifier(index: &[u8]) -> Option<SetVerifier> {
    SetVerifier::from_inspection(&inspect(index))
}

/// How many distinct recovery slices of `set_id` the inspected volumes hold whole (every
/// declared data byte present), for [`SetVerifier::assess`]. An exponent found in two volumes
/// counts once.
pub fn usable_recovery(volumes: &[Par2Inspection], set_id: &[u8; 16]) -> usize {
    volumes
        .iter()
        .flat_map(|v| v.recovery.iter())
        .filter(|r| r.set_id == *set_id && u64::try_from(r.available).is_ok_and(|have| have == r.data_len))
        .map(|r| r.exponent)
        .collect::<BTreeSet<u32>>()
        .len()
}
