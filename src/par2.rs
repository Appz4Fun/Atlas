//! Pull real filenames out of a base .par2 file (deobfuscation).
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::sync::LazyLock;

use regex::Regex;

use crate::archive::names::{best_name, clean_name};

#[allow(clippy::unwrap_used)]
static BASE_PAR2_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)\.par2(?:["\s]|$)"#).unwrap());
#[allow(clippy::unwrap_used)]
static VOL_PAR2_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.vol\d+[+-]\d+\.par2").unwrap());

const MAGIC: &[u8; 8] = b"PAR2\0PKT";
const FILE_DESC: &[u8; 16] = b"PAR 2.0\0FileDesc";
const HEADER_LEN: usize = 64;

pub fn is_base_par2(subject: &str) -> bool {
    BASE_PAR2_RE.is_match(subject) && !VOL_PAR2_RE.is_match(subject)
}

/// the most files, unicode names and Main ids a read keeps
const MAX_FILES: usize = 4_096;
const MAIN: &[u8; 16] = b"PAR 2.0\0Main\0\0\0\0";
const UNICODE: &[u8; 16] = b"PAR 2.0\0UniFileN";

/// A file a PAR2 set describes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Par2File {
    pub set_id: [u8; 16],
    pub file_id: [u8; 16],
    pub md5: [u8; 16],
    pub md5_16k: [u8; 16],
    pub size: u64,
    pub name: String,
}

/// What a set's packets say.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inventory {
    pub files: Vec<Par2File>,
    /// a Main was seen, and each of its ids has a FileDesc
    pub complete: bool,
    /// a packet ran past the data
    pub truncated: bool,
}

fn id_at(b: &[u8], at: usize) -> Option<[u8; 16]> {
    b.get(at..at.checked_add(16)?)?.try_into().ok()
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// Every file in `data`'s packets, per recovery set soo a file id cant merge two
/// sets. Volumes repeat these packets, soo a repeat collapses. Any bytes will do.
pub fn inventory(data: &[u8]) -> Inventory {
    let mut inv = Inventory::default();
    let mut mains: Vec<([u8; 16], [u8; 16])> = Vec::new();
    let mut saw_main = false;
    let mut unicode: Vec<([u8; 16], [u8; 16], String)> = Vec::new();
    let mut pos = 0usize;
    while let Some(head) = data.get(pos..pos.saturating_add(HEADER_LEN)) {
        if !head.starts_with(MAGIC) {
            // resync on the next packet header
            match data.get(pos + 1..).and_then(|rest| rest.windows(8).position(|w| w == MAGIC)) {
                Some(skip) => {
                    pos += skip + 1;
                    continue;
                }
                None => break,
            }
        }
        let len = u64_at(head, 8).unwrap_or(0);
        let shaped = len >= HEADER_LEN as u64 && len.is_multiple_of(4);
        let end = usize::try_from(len).ok().and_then(|l| pos.checked_add(l));
        let Some(packet) = end.filter(|_| shaped).and_then(|e| data.get(pos..e)) else {
            // past the data; a nonsense length is not truncation
            inv.truncated |= shaped;
            pos += 8;
            continue;
        };
        pos += packet.len();

        let (Some(set_id), Some(kind), Some(body)) = (id_at(packet, 32), id_at(packet, 48), packet.get(HEADER_LEN..))
        else {
            continue;
        };
        if &kind == MAIN {
            saw_main = true;
            let ids = body.get(12..).unwrap_or(&[]);
            for id in ids.as_chunks::<16>().0.iter().take(MAX_FILES) {
                if mains.len() < MAX_FILES && !mains.contains(&(set_id, *id)) {
                    mains.push((set_id, *id));
                }
            }
        } else if &kind == FILE_DESC {
            let (Some(file_id), Some(md5), Some(md5_16k), Some(size), Some(raw)) =
                (id_at(body, 0), id_at(body, 16), id_at(body, 32), u64_at(body, 48), body.get(56..))
            else {
                continue;
            };
            let cut = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
            let name = String::from_utf8_lossy(raw.get(..cut).unwrap_or(&[])).into_owned();
            let Some(name) = clean_name(&name) else { continue };
            let there = inv.files.iter().any(|f| f.set_id == set_id && f.file_id == file_id && f.name == name);
            if !there && inv.files.len() < MAX_FILES {
                inv.files.push(Par2File { set_id, file_id, md5, md5_16k, size, name });
            }
        } else if &kind == UNICODE {
            let (Some(file_id), Some(raw)) = (id_at(body, 0), body.get(16..)) else { continue };
            let units = raw.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).take_while(|u| *u != 0);
            let name = String::from_utf16_lossy(&units.collect::<Vec<u16>>());
            if unicode.len() < MAX_FILES
                && let Some(name) = clean_name(&name)
            {
                unicode.push((set_id, file_id, name));
            }
        }
    }
    // a clean unicode name replaces its file's, in either order
    for (set_id, file_id, name) in unicode {
        for f in inv.files.iter_mut().filter(|f| f.set_id == set_id && f.file_id == file_id) {
            f.name.clone_from(&name);
        }
    }
    inv.complete =
        saw_main && mains.iter().all(|(set, id)| inv.files.iter().any(|f| f.set_id == *set && f.file_id == *id));
    inv
}

/// Every (filename, size) described by FileDesc packets, each name once.
pub fn file_descriptions(data: &[u8]) -> Vec<(String, u64)> {
    let mut files: Vec<(String, u64)> = Vec::new();
    for f in inventory(data).files {
        if !files.iter().any(|(name, _)| *name == f.name) {
            files.push((f.name, f.size));
        }
    }
    files
}

/// The set's most useful name, see `names::best_name`.
pub fn display_name(data: &[u8]) -> Option<String> {
    let inv = inventory(data);
    best_name(inv.files.iter().map(|f| (f.name.as_str(), f.size)))
}

#[cfg(test)]
pub(crate) fn file_desc_packet(name: &str, size: u64) -> Vec<u8> {
    let mut name_bytes = name.as_bytes().to_vec();
    while !name_bytes.len().is_multiple_of(4) {
        name_bytes.push(0);
    }

    let len = (HEADER_LEN + 56 + name_bytes.len()) as u64;
    let mut p = Vec::new();
    p.extend_from_slice(MAGIC);
    p.extend_from_slice(&len.to_le_bytes());
    p.extend_from_slice(&[0u8; 16]); // packet md5
    p.extend_from_slice(&[1u8; 16]); // set id
    p.extend_from_slice(FILE_DESC);
    p.extend_from_slice(&[2u8; 16]); // file id
    p.extend_from_slice(&[3u8; 16]); // md5
    p.extend_from_slice(&[4u8; 16]); // md5 16k
    p.extend_from_slice(&size.to_le_bytes());
    p.extend_from_slice(&name_bytes);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_par2_detection() {
        assert!(is_base_par2(r#"[1/9] - "abc.par2" yEnc (1/1)"#));
        assert!(is_base_par2("abc.par2"));
        assert!(!is_base_par2(r#""abc.vol00+01.par2" yEnc (1/1)"#));
        assert!(!is_base_par2(r#""abc.rar" yEnc (1/1)"#));
    }

    #[test]
    fn picks_largest_file() {
        let mut data = b"junk".to_vec();
        data.extend(file_desc_packet("small.nfo", 10));
        data.extend(file_desc_packet("Real.Movie.2024.mkv", 5_000_000));
        data.extend(file_desc_packet("small.nfo", 10));
        assert_eq!(display_name(&data).as_deref(), Some("Real.Movie.2024.mkv"));
        assert_eq!(display_name(b"nothing here"), None);
    }

    /// a packet of `kind` with `body`, in set `set`
    fn packet(set: u8, kind: &[u8; 16], body: &[u8]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(MAGIC);
        p.extend_from_slice(&((HEADER_LEN + body.len()) as u64).to_le_bytes());
        p.extend_from_slice(&[0u8; 16]);
        p.extend_from_slice(&[set; 16]);
        p.extend_from_slice(kind);
        p.extend_from_slice(body);
        p
    }

    fn desc(set: u8, id: u8, name: &str, size: u64) -> Vec<u8> {
        let mut body = vec![id; 16];
        body.extend([id + 1; 16]);
        body.extend([id + 2; 16]);
        body.extend(size.to_le_bytes());
        let mut name = name.as_bytes().to_vec();
        while !name.len().is_multiple_of(4) {
            name.push(0);
        }
        body.extend(name);
        packet(set, FILE_DESC, &body)
    }

    fn main_packet(set: u8, ids: &[u8]) -> Vec<u8> {
        let mut body = vec![0u8; 8];
        body.extend((ids.len() as u32).to_le_bytes());
        for id in ids {
            body.extend([*id; 16]);
        }
        packet(set, MAIN, &body)
    }

    fn uni(set: u8, id: u8, name: &str) -> Vec<u8> {
        let mut body = vec![id; 16];
        body.extend(name.encode_utf16().flat_map(u16::to_le_bytes));
        while !body.len().is_multiple_of(4) {
            body.push(0);
        }
        packet(set, UNICODE, &body)
    }

    #[test]
    fn a_constant_id_helper_keeps_every_name() {
        let a = [file_desc_packet("small.nfo", 10), file_desc_packet("Hidden.Show.S01E01.mkv", 9_000)].concat();
        assert_eq!(inventory(&a).files.len(), 2);
        assert_eq!(display_name(&a).as_deref(), Some("Hidden.Show.S01E01.mkv"));
        assert_eq!(file_descriptions(&a.repeat(2)).len(), 2);
    }

    #[test]
    fn a_random_named_set_gives_no_name() {
        let data =
            [desc(1, 1, "a1b2c3d4e5f6a7b8c9d0.part01.rar", 10), desc(1, 5, "a1b2c3d4e5f6a7b8c9d0.part02.rar", 10)]
                .concat();
        assert_eq!(display_name(&data), None);
    }

    #[test]
    fn volumes_give_the_set_name() {
        let data = [
            desc(1, 1, "Real.Name.2024.part01.rar", 100),
            desc(1, 5, "Real.Name.2024.part02.rar", 100),
            desc(1, 9, "Real.Name.2024.nfo", 5),
        ]
        .concat();
        assert_eq!(display_name(&data).as_deref(), Some("Real.Name.2024"));
    }

    #[test]
    fn a_small_real_name_beats_a_big_random_one() {
        let data = [
            desc(1, 1, "0123456789abcdef0123456789abcdef.mkv", 4 << 30),
            desc(1, 5, "Real.Movie.2024.1080p-GRP.nfo", 5_000),
        ]
        .concat();
        assert_eq!(display_name(&data).as_deref(), Some("Real.Movie.2024.1080p-GRP"));
    }

    #[test]
    fn the_full_list_comes_back_with_hashes() {
        let inv = inventory(&[desc(9, 1, "a.mkv", 111), desc(9, 5, "b.nfo", 222)].concat());
        let f = &inv.files[0];
        assert_eq!((f.set_id, f.file_id, f.md5, f.md5_16k, f.size), ([9; 16], [1; 16], [2; 16], [3; 16], 111));
        assert_eq!(inv.files[1].size, 222);
    }

    #[test]
    fn complete_needs_every_main_id() {
        assert!(!inventory(&[main_packet(1, &[1, 5]), desc(1, 1, "a.mkv", 1)].concat()).complete);
        assert!(
            inventory(&[main_packet(1, &[1, 5]), desc(1, 1, "a.mkv", 1), desc(1, 5, "b.mkv", 1)].concat()).complete
        );
        assert!(!inventory(&desc(1, 1, "a.mkv", 1)).complete);
    }

    #[test]
    fn sets_stay_apart() {
        assert_eq!(inventory(&[desc(1, 1, "a.mkv", 1), desc(2, 1, "a.mkv", 1)].concat()).files.len(), 2);
    }

    #[test]
    fn unicode_name_replaces_the_ascii_one() {
        for data in [
            [desc(1, 1, "____.mkv", 1), uni(1, 1, "Фильм.mkv")].concat(),
            [uni(1, 1, "Фильм.mkv"), desc(1, 1, "____.mkv", 1)].concat(),
        ] {
            assert_eq!(inventory(&data).files[0].name, "Фильм.mkv");
        }
    }

    #[test]
    fn bad_packets_do_no_harm() {
        let good = desc(1, 1, "a.mkv", 1);
        for len in [0u64, 65, u64::MAX] {
            let mut bad = MAGIC.to_vec();
            bad.extend(len.to_le_bytes());
            bad.extend([0u8; 48]);
            let inv = inventory(&[bad, good.clone()].concat());
            assert_eq!(inv.files.len(), 1, "{len}");
            assert!(!inv.truncated, "{len}");
        }
        let mut long = MAGIC.to_vec();
        long.extend((1u64 << 30).to_le_bytes());
        long.extend([0u8; 48]);
        let inv = inventory(&[long, good.clone()].concat());
        assert_eq!(inv.files.len(), 1);
        assert!(inv.truncated);

        // a count above its ids, and bodies too short
        let mut main = vec![0u8; 8];
        main.extend(1000u32.to_le_bytes());
        assert!(inventory(&packet(1, MAIN, &main)).files.is_empty());
        assert!(inventory(&packet(1, FILE_DESC, &[7; 40])).files.is_empty());
        assert!(inventory(&packet(1, UNICODE, &[7; 20])).files.is_empty());
        // an odd unicode body: whole units kept
        let mut odd = vec![1u8; 16];
        odd.extend([b'a', 0, b'b']);
        odd.push(0);
        let data = [desc(1, 1, "x.mkv", 1), packet(1, UNICODE, &odd)].concat();
        assert_eq!(inventory(&data).files[0].name, "ab");

        let many: Vec<u8> = (0..5_000).flat_map(|i| desc(1, 1, &format!("{i}.mkv"), 1)).collect();
        assert_eq!(inventory(&many).files.len(), MAX_FILES);
        assert_eq!(inventory(&desc(1, 1, "a\x1b[31m.mkv", 1)).files[0].name, "a[31m.mkv");
    }

    #[test]
    fn every_prefix_of_a_set_is_safe() {
        let data = [main_packet(1, &[1, 5]), desc(1, 1, "a.mkv", 1), desc(1, 5, "b.mkv", 1)].concat();
        let full: Vec<String> = inventory(&data).files.into_iter().map(|f| f.name).collect();
        for cut in 0..data.len() {
            let part: Vec<String> = inventory(&data[..cut]).files.into_iter().map(|f| f.name).collect();
            assert!(full.starts_with(&part), "cut {cut}");
        }
    }
}
