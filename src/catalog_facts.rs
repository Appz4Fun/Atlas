//! Storage-independent catalog facts, identities, progress and part sets.

use std::sync::atomic::AtomicI64;

use crate::parser::Article;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupState {
    pub live_cursor: i64,
    pub backfill_cursor: i64,
}

/// An article number and when the articles around it were posted (unix time).
pub type Dated = (i64, i64);

/// One row of indexing progress: cursors, the server's article range and the
/// dated ends of what has been indexed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupProgress {
    pub key: String,
    pub live_cursor: i64,
    pub backfill_cursor: i64,
    pub first: Option<i64>,
    pub last: Option<i64>,
    pub low: Option<Dated>,
    pub high: Option<Dated>,
}

/// A row's article numbers on the server, and how many of them are indexed: everything between its cursors.
pub fn row_numbers(row: &GroupProgress) -> Option<(i64, i64)> {
    let (first, last) = (row.first?, row.last?);
    let total = (last - first + 1).max(0);
    let covered = (row.live_cursor.min(last) - row.backfill_cursor.max(first - 1)).clamp(0, total);
    Some((total, covered))
}

/// Each group's (article numbers, of them indexed, the row they come from): of its rows (one a server), the one
/// furthest along. A split group's cursors stand still while its day chunks (`chunks`: done, all) are indexed, soo
/// those say how far it is, as that share of its article numbers; once they are all done its home cursor sweeps on
/// for what they missed, and the cursor says it again. Rows with no range on the server yet are left out.
pub fn best_progress<'a>(
    progress: &'a [GroupProgress],
    chunks: &std::collections::BTreeMap<String, (i64, i64)>,
) -> std::collections::BTreeMap<String, (i64, i64, &'a GroupProgress)> {
    let mut best: std::collections::BTreeMap<String, (i64, i64, &GroupProgress)> = Default::default();
    for row in progress {
        let group = row.key.split('@').next().unwrap_or(&row.key);
        let Some((total, covered)) = row_numbers(row) else { continue };
        if best.get(group).is_none_or(|(t, c, _)| (covered, total) > (*c, *t)) {
            best.insert(group.to_string(), (total, covered, row));
        }
    }
    for (group, (total, covered, _)) in best.iter_mut() {
        if let Some(&(done, all)) = chunks.get(group).filter(|c| c.1 > 0 && c.0 < c.1) {
            *covered = (*total as i128 * done.clamp(0, all) as i128 / all as i128) as i64;
        }
    }
    best
}

pub const SHARDS: usize = 1;

/// the shard a group's releases live in
pub fn shard_of(group: &str) -> usize {
    let hash = group.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    (hash % SHARDS as u64) as usize
}

/// the shard a release id lives in
pub fn shard_of_id(id: i64) -> usize {
    id.rem_euclid(SHARDS as i64) as usize
}

pub fn global_id(seq: i64, shard: usize) -> i64 {
    seq * SHARDS as i64 + shard as i64
}

/// How long a release has to be quiet before the stage looks at it, seconds.
/// Saves have no config: the stage sets these from it. At least 1, soo a touch
/// always moves `due_at`.
pub static SETTLE_SECS: AtomicI64 = AtomicI64::new(7200);
/// the same for a complete release
pub static COMPLETE_SETTLE_SECS: AtomicI64 = AtomicI64::new(900);

// Legacy bitmaps end in a nonzero byte. A trailing zero marks a bitmap made of
// sorted blocks, each with a one-byte index and 256 bytes of part bits.
const PART_BLOCK_BYTES: usize = 256;
const PART_BLOCK_RECORD: usize = PART_BLOCK_BYTES + 1;
const LEGACY_PART_LIMIT: i64 = 100_000;

fn sparse_parts(bits: &[u8]) -> bool {
    bits.len() > PART_BLOCK_RECORD && bits.last() == Some(&0) && bits.len() % PART_BLOCK_RECORD == 1
}

fn part_blocks(bits: &[u8]) -> impl Iterator<Item = (usize, &[u8])> {
    let sparse = sparse_parts(bits);
    bits.chunks(if sparse { PART_BLOCK_RECORD } else { PART_BLOCK_BYTES }).enumerate().filter_map(
        move |(index, block)| {
            if sparse {
                (block.len() == PART_BLOCK_RECORD).then(|| (usize::from(block[0]), &block[1..]))
            } else {
                Some((index, block))
            }
        },
    )
}

fn part_byte(bits: &[u8], byte: usize) -> u8 {
    if !sparse_parts(bits) {
        return bits.get(byte).copied().unwrap_or(0);
    }
    let index = byte / PART_BLOCK_BYTES;
    part_blocks(bits).find(|(block, _)| *block == index).map_or(0, |(_, block)| block[byte % PART_BLOCK_BYTES])
}

fn add_part_byte(bits: &mut Vec<u8>, byte: usize, value: u8) {
    if value == 0 {
        return;
    }
    if !sparse_parts(bits) {
        if byte <= LEGACY_PART_LIMIT as usize / 8 {
            if bits.len() <= byte {
                bits.resize(byte + 1, 0);
            }
            bits[byte] |= value;
            return;
        }
        let mut sparse = Vec::new();
        for (index, block) in part_blocks(bits).filter(|(_, block)| block.iter().any(|&b| b != 0)) {
            sparse.push(index as u8);
            sparse.extend_from_slice(block);
            sparse.resize(sparse.len() + PART_BLOCK_BYTES - block.len(), 0);
        }
        sparse.push(0);
        *bits = sparse;
    }
    let index = (byte / PART_BLOCK_BYTES) as u8;
    let at = bits[..bits.len() - 1].chunks_exact(PART_BLOCK_RECORD).take_while(|block| block[0] < index).count()
        * PART_BLOCK_RECORD;
    if at == bits.len() - 1 || bits[at] != index {
        let mut block = [0; PART_BLOCK_RECORD];
        block[0] = index;
        bits.splice(at..at, block);
    }
    bits[at + 1 + byte % PART_BLOCK_BYTES] |= value;
}

pub(crate) fn insert_part(bits: &mut Vec<u8>, part: i64) {
    if (0..=crate::parser::MAX_PARTS).contains(&part) {
        add_part_byte(bits, (part / 8) as usize, 1 << (part % 8));
    }
}

pub(crate) fn has_part(bits: &[u8], part: i64) -> bool {
    (0..=crate::parser::MAX_PARTS).contains(&part) && part_byte(bits, (part / 8) as usize) & (1 << (part % 8)) != 0
}

fn part_bytes(bits: &[u8]) -> impl Iterator<Item = (usize, u8)> {
    part_blocks(bits).flat_map(|(index, block)| {
        block
            .iter()
            .enumerate()
            .filter_map(move |(offset, &byte)| (byte != 0).then_some((index * PART_BLOCK_BYTES + offset, byte)))
    })
}

pub(crate) fn same_parts(left: &[u8], right: &[u8]) -> bool {
    part_bytes(left).eq(part_bytes(right))
}

pub(crate) fn adds_parts(ours: &[u8], theirs: &[u8], expected: Option<i64>) -> bool {
    part_bytes(theirs).any(|(index, byte)| {
        let new = byte & !part_byte(ours, index);
        new != 0 && expected.is_none_or(|end| (index * 8 + new.trailing_zeros() as usize) as i64 <= end)
    })
}

pub(crate) fn parts_overlap(left: &[u8], right: &[u8]) -> bool {
    part_bytes(right).any(|(index, byte)| part_byte(left, index) & byte != 0)
}

pub(crate) fn merge_parts(left: &mut Vec<u8>, right: &[u8]) -> bool {
    let mut overlap = false;
    for (index, byte) in part_bytes(right) {
        overlap |= part_byte(left, index) & byte != 0;
        add_part_byte(left, index, byte);
    }
    overlap
}

/// Exactly parts 1..=expected or exactly 0..expected for zero-based files.
pub(crate) fn is_exactly(bits: &[u8], expected: i64) -> bool {
    if expected <= 0 {
        return false;
    }
    let mut count = 0;
    let mut highest = None;
    for (index, byte) in part_bytes(bits) {
        count += i64::from(byte.count_ones());
        highest = Some(index as i64 * 8 + 7 - i64::from(byte.leading_zeros()));
    }
    count == expected && highest == Some(expected - i64::from(has_part(bits, 0)))
}

pub(crate) const ROUTE_SLACK_SECS: i64 = 90;

/// What a save reads off one article without the database (its Date, what its message-id says), worked out by
/// `prepare` before the slice reaches the shard's writer: the writer is what limits how fast slices are saved, and
/// this is a tenth of its time otherwise.
#[derive(Debug, Default)]
pub(crate) struct ArticleFacts {
    pub(crate) date: Option<i64>,
    pub(crate) id: crate::discovery::IdFacts,
}

impl ArticleFacts {
    pub(crate) fn of(a: &Article) -> ArticleFacts {
        ArticleFacts { date: crate::dates::naive_timestamp(&a.date), id: crate::discovery::id_facts(&a.message_id) }
    }
}

/// the article whose subject a file's NZB entry uses comes first in (part, a
/// missing part first, then message-id) order: how the NZB lists them
pub(crate) fn subject_key(part: Option<i64>, message_id: &str) -> (bool, i64, &str) {
    (part.is_some(), part.unwrap_or(0), message_id)
}

/// SipHash of `text` under the pair, as `IdKey::hash`; never 0 (a release's own row in `release_ids`)
pub(crate) fn keyed_hash(k0: u64, k1: u64, text: &str) -> i64 {
    #[allow(deprecated)]
    let mut h = std::hash::SipHasher::new_with_keys(k0, k1);
    std::hash::Hasher::write(&mut h, text.as_bytes());
    match std::hash::Hasher::finish(&h) as i64 {
        0 => 1,
        n => n,
    }
}

/// The small files posted beside an archive set that aren't part of it.
pub(crate) const SIDE_EXTENSIONS: &[&str] = &[
    ".par2", ".nfo", ".sfv", ".srr", ".srs", ".txt", ".jpg", ".jpeg", ".png", ".md5", ".srt", ".sub", ".idx", ".nzb",
    ".url",
];

/// The extensions of a video that plays on its own.
/// `.m2ts` before `.ts`, so `film.m2ts` comes out as `film`.
pub(crate) const VIDEO_EXTENSIONS: &[&str] =
    &[".mkv", ".mp4", ".m4v", ".avi", ".m2ts", ".ts", ".wmv", ".mov", ".mpg", ".mpeg", ".webm"];

/// Whether `files` are one video named as the release (any of `names`: the file's name, with or without its
/// extension, or with the post's id after it, `name [54205819]`) beside side files only (`SIDE_EXTENSIONS`: its PAR2
/// set, an NFO, a thumbnail). The video plays on its own, soo the release is whole whatever the subject counter
/// says. A season posted under one counter is a row per episode, each "1 of 24 files", and each a whole episode. An
/// uploader can count a file that turns up as a row of its own: `[9/9] - "thumbnail.jpg"` under another name and
/// poster, posted after the film and its PAR2 set. Not a part of a video cut in pieces (`CD1`, `Part 2`, `Disc 1`):
/// that needs the others; nor a sample, which is whole but not what was posted; nor two videos.
pub(crate) fn one_video_named<'a, 'b>(
    names: impl IntoIterator<Item = &'a str>,
    files: impl IntoIterator<Item = &'b str>,
) -> bool {
    static PIECE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        regex::Regex::new(r"(?i)(?:^|[^a-z0-9])(cd|dis[ck]|part|pt)[ ._-]?\d|sample").unwrap()
    });
    let mut others = files
        .into_iter()
        .map(|file| file.trim().to_lowercase())
        .filter(|file| !SIDE_EXTENSIONS.iter().any(|ext| file.ends_with(ext)));
    let (Some(file), None) = (others.next(), others.next()) else { return false };
    let Some(stem) = VIDEO_EXTENSIONS.iter().find_map(|ext| file.strip_suffix(ext)) else { return false };
    if stem.is_empty() || PIECE.is_match(&file) {
        return false;
    }
    names.into_iter().any(|name| {
        let name = name.trim().to_lowercase();
        let name = without_post_id(&name);
        name == file || name == stem
    })
}

/// A release name without the post's id a collection put after it: `name [54205819]` is `name`.
fn without_post_id(name: &str) -> &str {
    name.strip_suffix(']')
        .and_then(|rest| rest.rsplit_once(" ["))
        .filter(|(_, id)| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
        .map_or(name, |(name, _)| name)
}

/// files untouched this long get sealed even when incomplete
pub const SEAL_AGE: i64 = 3 * 86_400;

/// A file with more segments than this never seals: a blob is built in
/// memory, and a poster can put any number of rows under one file. Real files
/// have a few thousand parts at most.
pub const SEAL_MAX_SEGMENTS: i64 = 50_000;

/// The most a stored blob may hold when read. Older files can hold more than `SEAL_MAX_SEGMENTS`, and refusing them
/// stops every save and NZB that touches them, so a late article could never join its release. This only guards
/// against a corrupt count: nothing seals past `SEAL_MAX_SEGMENTS`.
pub const READ_MAX_SEGMENTS: i64 = 1_000_000;

/// A file with rows is due to seal when it's complete and not sealed yet, or
/// untouched for `SEAL_AGE` (never touched since the upgrade counts as old).
pub(crate) fn due(expected: Option<i64>, seen: &[u8], touched: Option<i64>, sealed: bool, now: i64) -> bool {
    let complete = !sealed && expected.is_some_and(|e| is_exactly(seen, e));
    complete || touched.is_none_or(|t| t < now - SEAL_AGE)
}

/// A release's file, for a processing step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileInfo {
    pub id: i64,
    pub filename: String,
    pub subject: Option<String>,
    pub subject_part: Option<i64>,
    pub expected: Option<i64>,
    pub file_total: Option<i64>,
    /// how many parts were seen
    pub parts_seen: i64,
    pub touched_at: Option<i64>,
    pub sealed: bool,
}

/// One article of a file, for a processing step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentRef {
    pub message_id: String,
    pub part: Option<i64>,
    pub bytes: i64,
}

/// Which of a file's articles a step wants.
#[derive(Clone, Debug)]
pub enum SegmentPick {
    First(usize),
    Last(usize),
    Parts(Vec<i64>),
}

/// How many parts a `seen` bitmap has.
pub fn seen_count(bits: &[u8]) -> i64 {
    part_bytes(bits).map(|(_, byte)| i64::from(byte.count_ones())).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_hashes_keep_published_siphash24_vectors() {
        let key0 = 0x0706_0504_0302_0100;
        let key1 = 0x0f0e_0d0c_0b0a_0908;
        for (text, hash) in [
            ("", 0x726f_db47_dd0e_0e31u64),
            ("\0", 0x74f8_39c5_93dc_67fd),
            ("\0\x01", 0x0d6c_8009_d9a9_4f5a),
            ("\0\x01\x02", 0x8567_6696_d7fb_7e2d),
        ] {
            assert_eq!(keyed_hash(key0, key1, text), hash as i64);
        }
    }

    #[test]
    fn part_bitmap_encodings_keep_fixed_legacy_and_sparse_bytes() {
        let mut legacy = Vec::new();
        for part in [1, 2, 3, -1, 250_001] {
            insert_part(&mut legacy, part);
        }
        assert_eq!(legacy, [0x0e]);
        let mut sparse = Vec::new();
        insert_part(&mut sparse, 234_450);
        let mut expected = vec![0; 258];
        expected[0] = 114;
        expected[123] = 4;
        assert_eq!(sparse, expected);
        assert_eq!(seen_count(&sparse), 1);
    }

    #[test]
    fn progress_keeps_server_selection_and_partial_day_fraction() {
        let progress = [
            GroupProgress {
                key: "group@first".into(),
                first: Some(1),
                last: Some(10),
                live_cursor: 9,
                backfill_cursor: 2,
                ..Default::default()
            },
            GroupProgress {
                key: "group@second".into(),
                first: Some(1),
                last: Some(20),
                live_cursor: 10,
                backfill_cursor: 3,
                ..Default::default()
            },
            GroupProgress { key: "unknown@server".into(), ..Default::default() },
        ];
        assert_eq!(row_numbers(&progress[0]), Some((10, 7)));
        assert_eq!(row_numbers(&progress[2]), None);
        let best = best_progress(&progress, &Default::default());
        assert_eq!(best.len(), 1);
        assert_eq!((best["group"].0, best["group"].1), (20, 7));
        assert_eq!(best["group"].2.key, "group@second");
        let chunks = std::collections::BTreeMap::from([("group".into(), (1, 3))]);
        let partial = best_progress(&progress, &chunks);
        assert_eq!((partial["group"].0, partial["group"].1), (20, 6));
        let complete = best_progress(&progress, &std::collections::BTreeMap::from([("group".into(), (3, 3))]));
        assert_eq!(complete["group"].1, 7);
    }

    #[test]
    fn sealing_due_keeps_complete_sealed_and_age_boundary_rules() {
        assert!(due(Some(2), &[0x06], Some(1_000_000), false, 1_000_000));
        assert!(!due(Some(2), &[0x06], Some(1_000_000), true, 1_000_000));
        assert!(!due(None, &[], Some(740_800), false, 1_000_000));
        assert!(due(None, &[], Some(740_799), false, 1_000_000));
        assert!(due(None, &[], None, true, 1_000_000));
    }

    #[test]
    fn whole_video_matching_keeps_side_files_and_rejects_split_or_sample() {
        assert!(one_video_named([" Movie [123] "], ["movie.m2ts", "movie.nfo", "cover.jpg"]));
        assert!(!one_video_named(["Movie"], ["movie.mkv", "movie.mp4"]));
        assert!(!one_video_named(["Movie.Part1"], ["movie.part1.mkv"]));
        assert!(!one_video_named(["Movie.Sample"], ["movie.sample.mkv"]));
    }

    #[test]
    fn subject_choice_and_article_facts_keep_original_input_meaning() {
        assert!(subject_key(None, "z") < subject_key(Some(0), "a"));
        assert!(subject_key(Some(1), "a") < subject_key(Some(1), "b"));
        let facts = ArticleFacts::of(&Article {
            date: "Thu, 1 Jan 1970 00:00:00 +0200".into(),
            message_id: "<@host>".into(),
            ..Default::default()
        });
        assert_eq!(facts.date, Some(0));
        assert_eq!(facts.id, crate::discovery::IdFacts::default());
    }

    #[test]
    fn sparse_part_sets_preserve_legacy_bits_and_bound_high_parts() {
        let mut legacy = vec![0b110];
        let mut high = Vec::new();
        insert_part(&mut high, 234_450);
        insert_part(&mut high, 1_099_511_627_776);
        assert_eq!(high.len(), PART_BLOCK_RECORD + 1);
        assert_eq!(seen_count(&high), 1);
        assert!(has_part(&high, 234_450));
        assert!(!has_part(&high, 234_449));
        assert!(!is_exactly(&high, 234_450));
        assert!(!parts_overlap(&high, &legacy));
        assert!(adds_parts(&legacy, &high, Some(234_450)));
        assert!(!adds_parts(&legacy, &high, Some(234_449)));
        assert!(!merge_parts(&mut legacy, &high));
        assert!(parts_overlap(&high, &legacy));
        assert!(merge_parts(&mut high, &legacy));
        assert!(same_parts(&legacy, &high));
        let mut complete = Vec::new();
        for part in 1..=250_000 {
            insert_part(&mut complete, part);
        }
        assert!(is_exactly(&complete, 250_000));
    }

    #[test]
    fn part_sets() {
        let mut bits = Vec::new();
        for i in [1, 2, 3] {
            insert_part(&mut bits, i);
        }
        assert!(is_exactly(&bits, 3));
        assert!(!is_exactly(&bits, 4));
        insert_part(&mut bits, 5);
        assert!(!is_exactly(&bits, 4), "part 4 missing");
        let mut zero = Vec::new();
        insert_part(&mut zero, 0);
        insert_part(&mut zero, 1);
        assert!(!is_exactly(&zero, 1), "a part 0 doesnt belong");
        assert!(is_exactly(&zero, 2), "0 and 1 are a whole file numbered from 0");
        insert_part(&mut zero, 3);
        assert!(!is_exactly(&zero, 3), "part 2 missing");
        assert!(!is_exactly(&zero, 4), "part 2 missing, counted from 0");
        insert_part(&mut zero, 2);
        assert!(is_exactly(&zero, 4));
        let mut huge = Vec::new();
        insert_part(&mut huge, 5_000_000_000);
        assert!(huge.is_empty());
    }
}
