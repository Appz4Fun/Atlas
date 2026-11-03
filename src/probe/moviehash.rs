//! The OpenSubtitles hash of a file: its size plus the first and last 64 KiB read as little endian words. It
//! needs only the two ends of the file, soo a probe gets it from a few articles, and a stored RAR set gives
//! the hash of the file inside it.

use std::sync::LazyLock;

use regex::Regex;

use crate::store::FileInfo;

/// How much of each end the hash reads.
pub const CHUNK: usize = 65_536;

/// The hash as OpenSubtitles writes it, 16 hex digits. None for a file too small to have two whole ends, or
/// ends that aren't whole.
pub fn moviehash(size: u64, head: &[u8], tail: &[u8]) -> Option<String> {
    if size < 2 * CHUNK as u64 || head.len() < CHUNK || tail.len() < CHUNK {
        return None;
    }
    let words =
        |b: &[u8]| -> u64 { b.as_chunks::<8>().0.iter().fold(0u64, |sum, w| sum.wrapping_add(u64::from_le_bytes(*w))) };
    let sum = size.wrapping_add(words(&head[..CHUNK])).wrapping_add(words(&tail[tail.len() - CHUNK..]));
    Some(format!("{sum:016x}"))
}

static PART_RAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(.*)\.part0*1\.rar$").unwrap());
static PLAIN_RAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(.*)\.rar$").unwrap());

/// The last volume of the set `first` starts: `.partNN.rar`, or `.rar` then `.r00`, `.r01` and on to `.s00`.
/// The first volume itself when it has no others. None when `first` doesn't name a first volume.
pub fn last_volume<'a>(first: &str, files: &'a [FileInfo]) -> Option<&'a FileInfo> {
    volumes(first, files).pop()
}

/// The volumes of the set `first` starts, in order. Empty when `first` doesn't name a first volume.
pub fn volumes<'a>(first: &str, files: &'a [FileInfo]) -> Vec<&'a FileInfo> {
    let lower = |f: &FileInfo| f.filename.to_ascii_lowercase();
    let first_lower = first.to_ascii_lowercase();
    let Some(first_file) = files.iter().find(|f| lower(f) == first_lower) else { return Vec::new() };
    if let Some(prefix) = PART_RAR.captures(first).map(|c| c[1].to_ascii_lowercase()) {
        let rank = |f: &FileInfo| -> Option<u64> {
            let name = lower(f);
            let rest = name.strip_prefix(&prefix)?.strip_prefix(".part")?.strip_suffix(".rar")?;
            rest.bytes().all(|b| b.is_ascii_digit()).then(|| rest.parse().ok()).flatten()
        };
        let mut set: Vec<(u64, &FileInfo)> = files.iter().filter_map(|f| rank(f).map(|r| (r, f))).collect();
        set.sort_by_key(|(r, _)| *r);
        return set.into_iter().map(|(_, f)| f).collect();
    }
    let Some(prefix) = PLAIN_RAR.captures(first).map(|c| c[1].to_ascii_lowercase()) else { return Vec::new() };
    let rank = |f: &FileInfo| -> Option<(u8, u32)> {
        let name = lower(f);
        let ext = name.strip_prefix(&prefix)?.strip_prefix('.')?.as_bytes().to_vec();
        match ext.as_slice() {
            [letter @ b'r'..=b'z', d1, d2] if d1.is_ascii_digit() && d2.is_ascii_digit() => {
                Some((*letter - b'r', u32::from(d1 - b'0') * 10 + u32::from(d2 - b'0')))
            }
            _ => None,
        }
    };
    let mut later: Vec<((u8, u32), &FileInfo)> = files.iter().filter_map(|f| rank(f).map(|r| (r, f))).collect();
    later.sort_by_key(|(r, _)| *r);
    std::iter::once(first_file).chain(later.into_iter().map(|(_, f)| f)).collect()
}

/// The 1-based parts of a file posted in `segment` byte parts that hold its bytes `from..to`.
pub fn parts_for(segment: u64, from: u64, to: u64) -> Option<std::ops::RangeInclusive<i64>> {
    if segment == 0 || to <= from {
        return None;
    }
    let first = i64::try_from(from / segment).ok()? + 1;
    let last = i64::try_from((to - 1) / segment).ok()? + 1;
    Some(first..=last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> FileInfo {
        FileInfo {
            id: 1,
            filename: name.into(),
            subject: None,
            subject_part: None,
            expected: Some(10),
            file_total: None,
            parts_seen: 10,
            touched_at: None,
            sealed: false,
        }
    }

    #[test]
    fn the_hash_is_the_size_and_both_ends() {
        let size = 3 * CHUNK as u64;
        let mut head = vec![0u8; CHUNK];
        head[..8].copy_from_slice(&5u64.to_le_bytes());
        let mut tail = vec![0u8; CHUNK + 10];
        let at = tail.len() - 8;
        tail[at..].copy_from_slice(&u64::MAX.to_le_bytes());
        // only the last CHUNK of what's given counts, and the sum wraps
        assert_eq!(moviehash(size, &head, &tail), Some(format!("{:016x}", size + 5 - 1)));
        assert_eq!(moviehash(CHUNK as u64, &head, &tail), None, "too small");
        assert_eq!(moviehash(size, &head[..10], &tail), None, "a short end");
    }

    #[test]
    fn a_known_hash() {
        // a file of 2 chunks of zeros except one word: the hash of the opensubtitles algorithm by hand
        let size = 2 * CHUNK as u64;
        let mut ends = vec![0u8; CHUNK];
        ends[8..16].copy_from_slice(&0x0102_0304_0506_0708u64.to_le_bytes());
        assert_eq!(moviehash(size, &ends, &ends).as_deref(), Some("020406080a0e0e10"));
    }

    #[test]
    fn the_last_volume_of_each_naming() {
        let files = [file("a.part01.rar"), file("a.part02.rar"), file("a.part10.rar"), file("b.part99.rar")];
        assert_eq!(last_volume("a.part01.rar", &files).unwrap().filename, "a.part10.rar");
        let old = [file("A.rar"), file("a.r00"), file("a.r99"), file("a.s01"), file("a.nfo")];
        assert_eq!(last_volume("A.rar", &old).unwrap().filename, "a.s01");
        let one = [file("a.rar"), file("a.nfo")];
        assert_eq!(last_volume("a.rar", &one).unwrap().filename, "a.rar");
        assert!(last_volume("a.mkv", &[file("a.mkv")]).is_none());
        assert!(last_volume("gone.rar", &one).is_none());
    }

    #[test]
    fn parts_of_a_range() {
        assert_eq!(parts_for(100, 0, 100), Some(1..=1));
        assert_eq!(parts_for(100, 150, 250), Some(2..=3));
        assert_eq!(parts_for(100, 10, 10), None);
        assert_eq!(parts_for(0, 0, 10), None);
    }
}
