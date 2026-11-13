//! RAR 5 recovery volumes (`.rev`, made with `rar -rv`): the header at the front of one names the size and CRC32 of
//! every data volume of its set, soo one article gives the whole-file (size, CRC32) of each `.rar` without
//! downloading any of them. Layout from unrar's `recvol5.cpp` `RecVolumes5::ReadHeader`, as
//! `docs/research/archive-metadata/raw/w3c-rr-qo-srs.md` section 3.2 and `w8c-resolved.md` (c) give it:
//!
//! ```text
//! @0   8  "Rar!\x1aRev"
//! @8   4  CRC32 over @12 .. 16 + header size (the size field and the body)
//! @12  4  header size (6 ..= 0x100000)
//! @16     body: version 1 (1), data volumes (2), recovery volumes (2), this volume's number (2), its CRC32 (4),
//!         then for each data volume its size (8) and CRC32 (4), little endian
//! ```
//!
//! RAR 3 `.rev` files have no such table, only a 7-byte trailer at the end of the file naming the volume's own
//! place in the set: nothing to key a volume by, soo they are left alone.
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use crate::postprocess::schema::ShardTable;
use crate::postprocess::step::{StepCtx, StepError, StepMutation, StepWrite};
use crate::store::FileInfo;

pub const SIGNATURE: &[u8; 8] = b"Rar!\x1aRev";
/// The most data volumes a header is read for: as many files as a PAR2 read keeps.
pub const MAX_VOLUMES: usize = 4_096;
/// The most data volumes stored for one release, the ones big enough to look up (`keys::CRC_MIN_SIZE`): a 50 MB
/// volume set of 12 GB. One article of anyone's post can claim thousands, soo what it can make Atlas store is
/// bounded: a header past this keeps its first ones.
pub const MAX_KEPT: usize = 256;
/// The `.rev` files a release's step tries, first by name first, before it gives up.
const TRIES: usize = 2;

/// A data volume as the `.rev` header gives it: its place in the set from 0, its size in bytes and its CRC32.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Volume {
    pub index: u16,
    pub size: u64,
    pub crc32: u32,
}

/// What the front of a RAR 5 `.rev` file says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    pub data_count: u16,
    pub rec_count: u16,
    /// this file's own number in the set, data volumes first
    pub rec_num: u16,
    /// every data volume, in order; empty when the header carries no table
    pub volumes: Vec<Volume>,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// The header at the front of `data`. None: not a RAR 5 `.rev`, cut short before the header ends, or a header
/// whose CRC, version or counts don't hold, as unrar would refuse it. Any bytes will do.
pub fn parse(data: &[u8]) -> Option<Header> {
    if !data.starts_with(SIGNATURE) {
        return None;
    }
    let crc = u32_at(data, 8)?;
    let size = usize::try_from(u32_at(data, 12)?).ok()?;
    if !(6..=0x10_0000).contains(&size) {
        return None;
    }
    let checked = data.get(12..16usize.checked_add(size)?)?;
    if super::crc32(checked) != crc {
        return None;
    }
    let body = checked.get(4..)?;
    if *body.first()? != 1 {
        return None;
    }
    let (data_count, rec_count, rec_num) = (u16_at(body, 1)?, u16_at(body, 3)?, u16_at(body, 5)?);
    let total = u32::from(data_count) + u32::from(rec_count);
    if u32::from(rec_num) >= total {
        return None;
    }
    let mut volumes = Vec::new();
    // the table, when the header holds all of it: unrar reads it from the first .rev it opens
    let table = 11usize.checked_add(usize::from(data_count).checked_mul(12)?)?;
    if data_count > 0 && body.len() >= table {
        for index in 0..data_count.min(MAX_VOLUMES as u16) {
            let at = 11 + usize::from(index) * 12;
            volumes.push(Volume { index, size: u64_at(body, at)?, crc32: u32_at(body, at + 8)? });
        }
    }
    Some(Header { data_count, rec_count, rec_num, volumes })
}

/// Each data volume's whole-file size and CRC32, as a release's first `.rev` file names them. Kept soo the same RAR
/// volumes posted again, under any names, are found by `(size, crc32)` (`keys::crc_files`): candidates for a reader,
/// never a merge.
pub const REV_VOLUMES: ShardTable = ShardTable {
    name: "rev_volumes",
    create: "create table if not exists rev_volumes (
        release_id INTEGER NOT NULL,
        volume     INTEGER NOT NULL,
        size       INTEGER NOT NULL,
        crc32      INTEGER NOT NULL,
        primary key (release_id, volume)
    ) without rowid",
    indexes: &[("rev_volumes_crc", "create index if not exists rev_volumes_crc on rev_volumes(size, crc32)")],
    release_key: Some("release_id"),
    added: &[],
};

pub const DELETE_REV_VOLUMES: &str = "delete from rev_volumes where release_id = ?1";
pub const INSERT_REV_VOLUME: &str =
    "insert or replace into rev_volumes (release_id, volume, size, crc32) values (?1, ?2, ?3, ?4)";

/// The volumes worth storing: those big enough to look up by size and CRC32, at most `MAX_KEPT`.
pub fn kept(volumes: &[Volume]) -> Vec<Volume> {
    let min = u64::try_from(crate::keys::CRC_MIN_SIZE).unwrap_or(u64::MAX);
    volumes.iter().filter(|v| v.size >= min).take(MAX_KEPT).copied().collect()
}

/// The writes that keep a set's data volumes in place of what the release had.
pub fn writes(release_id: i64, volumes: &[Volume]) -> Vec<StepWrite> {
    let mut out = vec![StepWrite::for_mutation(release_id, StepMutation::ClearRevVolumes)];
    for v in volumes {
        out.push(StepWrite::for_mutation(release_id, StepMutation::KeepRevVolume(*v)));
    }
    out
}

/// A release's `.rev` files, by name.
pub fn sources(files: &[FileInfo]) -> Vec<&FileInfo> {
    let mut found: Vec<&FileInfo> =
        files.iter().filter(|f| f.filename.to_ascii_lowercase().trim_end_matches('"').ends_with(".rev")).collect();
    found.sort_by(|a, b| a.filename.cmp(&b.filename));
    found
}

/// The writes for a release's data volume table: from `atlas.raw.db` when it was read before for the same files,
/// else from the first article of its first `.rev` file (the next when that one gives nothing). Empty, and the
/// stored rows left as they are, when the release has no `.rev` file or none of them gave a table: no article is
/// fetched for a release without one.
pub fn read(ctx: &mut StepCtx<'_>, files: &[FileInfo], explicit: bool) -> Result<Vec<StepWrite>, StepError> {
    let sources = sources(files);
    if sources.is_empty() {
        return Ok(Vec::new());
    }
    let rel = ctx.release();
    let (id, name, group) = (rel.id, rel.name.clone(), rel.group.clone());
    let content_id = ctx.data().release_content_id(id)?.unwrap_or_default();
    if !explicit && let Some(saved) = crate::raw_store::get_rev_volumes(&name, &group, &content_id) {
        return Ok(writes(id, &kept(&saved)));
    }
    for file in sources.into_iter().take(TRIES) {
        let Some(seg) = super::steps::first_article(ctx, file)? else { continue };
        let Some(body) = super::steps::fetch_one(ctx, &seg, &file.filename)? else { continue };
        let Some(header) = parse(&body) else { continue };
        let volumes = kept(&header.volumes);
        if volumes.is_empty() {
            continue;
        }
        crate::raw_store::put_rev_volumes(&name, &group, &content_id, &volumes);
        return Ok(writes(id, &volumes));
    }
    Ok(Vec::new())
}

#[cfg(test)]
pub(crate) fn header_bytes(volumes: &[(u64, u32)], rec_count: u16, rec_num: u16) -> Vec<u8> {
    let mut body = vec![1u8];
    body.extend((volumes.len() as u16).to_le_bytes());
    body.extend(rec_count.to_le_bytes());
    body.extend(rec_num.to_le_bytes());
    body.extend(0u32.to_le_bytes());
    for (size, crc) in volumes {
        body.extend(size.to_le_bytes());
        body.extend(crc.to_le_bytes());
    }
    let mut checked = (body.len() as u32).to_le_bytes().to_vec();
    checked.extend(&body);
    let mut out = SIGNATURE.to_vec();
    out.extend(super::crc32(&checked).to_le_bytes());
    out.extend(checked);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SET: [(u64, u32); 3] = [(50 << 20, 0xdead_beef), (50 << 20, 0x0123_4567), (7_340_033, 0xfeed_f00d)];

    #[test]
    fn a_header_gives_every_data_volume() {
        let mut data = header_bytes(&SET, 2, 3);
        // recovery data follows the header
        data.extend([0x5a; 4096]);
        let h = parse(&data).expect("a whole header");
        assert_eq!((h.data_count, h.rec_count, h.rec_num), (3, 2, 3));
        assert_eq!(
            h.volumes,
            vec![
                Volume { index: 0, size: 50 << 20, crc32: 0xdead_beef },
                Volume { index: 1, size: 50 << 20, crc32: 0x0123_4567 },
                Volume { index: 2, size: 7_340_033, crc32: 0xfeed_f00d },
            ]
        );
    }

    #[test]
    fn a_short_or_broken_header_gives_nothing() {
        let data = header_bytes(&SET, 2, 3);
        for cut in 0..data.len() {
            assert_eq!(parse(&data[..cut]), None, "cut at {cut}");
        }
        // a flipped byte fails the CRC
        let mut bad = data.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert_eq!(parse(&bad), None);
        // RAR 5 archives and RAR 4 aren't recovery volumes
        assert_eq!(parse(b"Rar!\x1a\x07\x01\x00rest of an archive"), None);
        assert_eq!(parse(b"Rar!\x1a\x07\x00"), None);
        // a volume number past the set, or another version, as unrar refuses
        assert_eq!(parse(&header_bytes(&SET, 2, 5)), None);
        let mut v2 = data.clone();
        v2[16] = 2;
        let crc = crate::archive::crc32(&v2[12..]);
        v2[8..12].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(parse(&v2), None);
        // a size out of bounds, whatever the CRC
        let mut huge = data.clone();
        huge[12..16].copy_from_slice(&0x0010_0001u32.to_le_bytes());
        assert_eq!(parse(&huge), None);
    }

    #[test]
    fn a_header_without_its_table_still_reads() {
        // the counts say 3 data volumes, the header holds none of them
        let mut body = vec![1u8];
        body.extend(3u16.to_le_bytes());
        body.extend(1u16.to_le_bytes());
        body.extend(3u16.to_le_bytes());
        body.extend(0u32.to_le_bytes());
        let mut checked = (body.len() as u32).to_le_bytes().to_vec();
        checked.extend(&body);
        let mut data = SIGNATURE.to_vec();
        data.extend(crate::archive::crc32(&checked).to_le_bytes());
        data.extend(checked);
        let h = parse(&data).unwrap();
        assert_eq!((h.data_count, h.volumes.len()), (3, 0));
    }

    #[test]
    fn writes_replace_what_the_release_had() {
        let h = parse(&header_bytes(&SET, 1, 3)).unwrap();
        let w = writes(9, &h.volumes);
        assert_eq!(w.len(), 4);
        assert_eq!(w[0].sql, DELETE_REV_VOLUMES);
        assert!(w[1..].iter().all(|x| x.sql == INSERT_REV_VOLUME));
    }

    #[test]
    fn only_volumes_worth_looking_up_are_kept_and_not_too_many() {
        let h = parse(&header_bytes(&SET, 1, 3)).unwrap();
        assert_eq!(kept(&h.volumes), h.volumes, "every volume of 1 MiB or more");
        let small = [Volume { index: 0, size: 1_000, crc32: 1 }, Volume { index: 1, size: 2 << 20, crc32: 2 }];
        assert_eq!(kept(&small), vec![small[1]]);
        // a header claiming every volume it can: what it makes Atlas store is bounded
        let many: Vec<(u64, u32)> = (0..MAX_VOLUMES as u32).map(|i| (50 << 20, i)).collect();
        let h = parse(&header_bytes(&many, 1, 0)).unwrap();
        assert_eq!(h.volumes.len(), MAX_VOLUMES);
        let k = kept(&h.volumes);
        assert_eq!(k.len(), MAX_KEPT);
        assert_eq!(writes(1, &k).len(), MAX_KEPT + 1);
    }

    #[test]
    fn rev_files_are_found_by_name_in_order() {
        let file = |name: &str| FileInfo {
            id: 0,
            filename: name.into(),
            subject: None,
            subject_part: None,
            expected: None,
            file_total: None,
            parts_seen: 1,
            touched_at: None,
            sealed: false,
        };
        let files = [file("x.part2.rev"), file("x.part01.rar"), file("X.PART1.REV"), file("x.nfo")];
        let names: Vec<&str> = sources(&files).iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(names, ["X.PART1.REV", "x.part2.rev"]);
    }
}
