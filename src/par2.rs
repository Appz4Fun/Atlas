//! Pull real filenames out of a base .par2 file (deobfuscation).
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::sync::LazyLock;

use md5::Digest;
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
const IFSC: &[u8; 16] = b"PAR 2.0\0IFSC\0\0\0\0";
const CREATOR: &[u8; 16] = b"PAR 2.0\0Creator\0";
/// The most bytes of a Creator string kept: ParPar's, URL and all, is under 60.
pub const CREATOR_MAX: usize = 128;

/// A file a PAR2 set describes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Par2File {
    pub set_id: [u8; 16],
    pub file_id: [u8; 16],
    pub md5: [u8; 16],
    pub md5_16k: [u8; 16],
    pub size: u64,
    pub name: String,
    /// its file id is the MD5 of its first-16-KiB hash, size and name, as PAR2 makes it, soo a Main that lists the id
    /// vouches for this size and hash too
    pub bound: bool,
    /// whole-file CRC32 computed by folding IFSC slice checksums with last slice unpadded
    pub crc32: Option<u32>,
}

/// What a set's packets say.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inventory {
    pub files: Vec<Par2File>,
    /// from the Main packet: how big a recovery block is
    pub block_size: Option<u64>,
    /// a Main was seen, and each of its ids has a FileDesc
    pub complete: bool,
    /// a packet ran past the data
    pub truncated: bool,
    /// (set id, file id) from each Main packet whose body hashes to its set id: a set whose list of files can't have
    /// been added to without changing its id
    pub sealed: Vec<([u8; 16], [u8; 16])>,
    /// (set id, the program that made the set) from each set's first Creator packet, as `creator_text` keeps it:
    /// "ParPar v0.4.1 x64 [https://github.com/animetosho/parpar]", "Created by par2cmdline version 0.8.1."
    pub creators: Vec<([u8; 16], String)>,
}

impl Inventory {
    /// The program that made `set_id`'s set, when its Creator packet was read.
    pub fn creator_of(&self, set_id: &[u8; 16]) -> Option<&str> {
        self.creators.iter().find(|(set, _)| set == set_id).map(|(_, c)| c.as_str())
    }
}

/// A Creator packet's body as Atlas keeps it: the text up to its NUL padding, printable ASCII only (anything else
/// a space), an e-mail address left out wherever it is (a URL's `user@` too), runs of spaces as one, at most
/// `CREATOR_MAX` bytes. The spec asks for the client's name and a contact, a URL or an e-mail: the URL names the
/// tool, the e-mail may name a person, soo it is never kept. None: no letter left, as in garbage.
pub fn creator_text(body: &[u8]) -> Option<String> {
    let cut = body.iter().position(|b| *b == 0).unwrap_or(body.len());
    // enough to hold any real one with room for an address to be cut out of
    let raw = body.get(..cut.min(CREATOR_MAX * 8)).unwrap_or(&[]);
    let text: String = raw.iter().map(|b| if b.is_ascii_graphic() { *b as char } else { ' ' }).collect();
    let mut kept: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let word = match word.find("://") {
            // a URL keeps its host and path, not who logs in to it
            Some(at) => {
                let (scheme, rest) = word.split_at(at + 3);
                let host_end = rest.find(['/', '?', '#', ']', ')', '>']).unwrap_or(rest.len());
                match rest.get(..host_end).and_then(|host| host.rfind('@')) {
                    Some(user_end) => format!("{scheme}{}", rest.get(user_end + 1..).unwrap_or("")),
                    None => word.to_string(),
                }
            }
            None if word.contains('@') => continue,
            None => word.to_string(),
        };
        // what an address was wrapped in, left empty
        if word.chars().all(|c| matches!(c, '<' | '>' | '(' | ')' | '[' | ']' | ',' | ';' | ':')) {
            continue;
        }
        kept.push(word);
    }
    let mut out = kept.join(" ");
    if out.len() > CREATOR_MAX {
        out.truncate(CREATOR_MAX);
        out.truncate(out.trim_end().len());
    }
    out.bytes().any(|b| b.is_ascii_alphabetic()).then_some(out)
}

fn id_at(b: &[u8], at: usize) -> Option<[u8; 16]> {
    b.get(at..at.checked_add(16)?)?.try_into().ok()
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

type SliceChecksum = ([u8; 16], u32);
type SetFileKey = ([u8; 16], [u8; 16]);

/// Every file in `data`'s packets, per recovery set soo a file id cant merge two
/// sets. Volumes repeat these packets, soo a repeat collapses. Any bytes will do.
pub fn inventory(data: &[u8]) -> Inventory {
    const MAX_TOTAL_IFSC_SLICES: usize = 16_384;

    let mut inv = Inventory::default();
    let mut mains: Vec<([u8; 16], [u8; 16])> = Vec::new();
    let mut block_sizes: std::collections::HashMap<[u8; 16], Option<u64>> = std::collections::HashMap::new();
    let mut saw_main = false;
    let mut unicode: Vec<([u8; 16], [u8; 16], String)> = Vec::new();
    let mut ifsc_slices: std::collections::HashMap<SetFileKey, Vec<SliceChecksum>> = std::collections::HashMap::new();
    let mut total_ifsc_slices = 0usize;
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
            if let Some(bs) = u64_at(body, 0).filter(|size| *size > 0) {
                inv.block_size = inv.block_size.or(Some(bs));
                if let Some(existing) = block_sizes.get_mut(&set_id) {
                    if *existing != Some(bs) {
                        *existing = None;
                    }
                } else if block_sizes.len() < MAX_FILES {
                    block_sizes.insert(set_id, Some(bs));
                }
            }
            let ids = body.get(12..).unwrap_or(&[]);
            let sealed = <[u8; 16]>::from(md5::Md5::digest(body)) == set_id;
            for id in ids.as_chunks::<16>().0.iter().take(MAX_FILES) {
                if sealed && inv.sealed.len() < MAX_FILES && !inv.sealed.contains(&(set_id, *id)) {
                    inv.sealed.push((set_id, *id));
                }
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
            let raw_name = raw.get(..cut).unwrap_or(&[]);
            let bound = <[u8; 16]>::from(
                md5::Md5::new()
                    .chain_update(md5_16k)
                    .chain_update(size.to_le_bytes())
                    .chain_update(raw_name)
                    .finalize(),
            ) == file_id;
            let name = String::from_utf8_lossy(raw_name).into_owned();
            let Some(name) = clean_name(&name) else { continue };
            let there = inv.files.iter().any(|f| f.set_id == set_id && f.file_id == file_id && f.name == name);
            if !there && inv.files.len() < MAX_FILES {
                inv.files.push(Par2File { set_id, file_id, md5, md5_16k, size, name, bound, crc32: None });
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
        } else if &kind == CREATOR {
            if inv.creators.len() < MAX_FILES
                && inv.creator_of(&set_id).is_none()
                && let Some(text) = creator_text(body)
            {
                inv.creators.push((set_id, text));
            }
        } else if &kind == IFSC {
            if ifsc_slices.len() >= MAX_FILES {
                continue;
            }
            let (Some(file_id), Some(slice_bytes)) = (id_at(body, 0), body.get(16..)) else { continue };
            if ifsc_slices.contains_key(&(set_id, file_id)) {
                continue;
            }
            let (chunks, rem) = slice_bytes.as_chunks::<20>();
            if chunks.is_empty()
                || !rem.is_empty()
                || chunks.len() > MAX_FILES
                || total_ifsc_slices.saturating_add(chunks.len()) > MAX_TOTAL_IFSC_SLICES
            {
                continue;
            }
            let mut slices = Vec::with_capacity(chunks.len());
            for chunk in chunks.iter().take(MAX_FILES) {
                let Some(md5) = chunk.get(0..16).and_then(|s| s.try_into().ok()) else { continue };
                let Some(crc_bytes) = chunk.get(16..20).and_then(|s| s.try_into().ok()) else { continue };
                slices.push((md5, u32::from_le_bytes(crc_bytes)));
            }
            if !slices.is_empty() {
                total_ifsc_slices += slices.len();
                ifsc_slices.insert((set_id, file_id), slices);
            }
        }
    }
    // a clean unicode name replaces its file's, in either order
    for (set_id, file_id, name) in unicode {
        for f in inv.files.iter_mut().filter(|f| f.set_id == set_id && f.file_id == file_id) {
            f.name.clone_from(&name);
        }
    }
    // calculate whole-file CRC32 by folding IFSC slice checksums with last slice unpadded
    for f in &mut inv.files {
        let Some(&Some(block_size)) = block_sizes.get(&f.set_id) else {
            continue;
        };
        let Some(slices) = ifsc_slices.get(&(f.set_id, f.file_id)) else {
            continue;
        };
        if block_size == 0 || slices.is_empty() || f.size == 0 {
            continue;
        }
        let expected_slices = f.size.div_ceil(block_size);
        let num_slices = slices.len() as u64;
        if num_slices != expected_slices {
            continue;
        }
        let last_slice_unpadded_len = f.size.saturating_sub(num_slices.saturating_sub(1) * block_size);
        let zero_padding_len = block_size.saturating_sub(last_slice_unpadded_len);

        let mut combined_crc = 0u32;
        for (i, &(_md5, slice_crc)) in slices.iter().enumerate() {
            let (slice_crc_unpadded, len) = if i + 1 == slices.len() && zero_padding_len > 0 {
                (crate::gf2::crc32_unzero(slice_crc, zero_padding_len as usize), last_slice_unpadded_len as usize)
            } else {
                (slice_crc, block_size as usize)
            };
            combined_crc = crate::gf2::crc32_combine(combined_crc, slice_crc_unpadded, len);
        }
        f.crc32 = Some(combined_crc);
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
    fn a_main_whose_body_hashes_to_its_set_id_seals_its_list() {
        let mut body = vec![0u8; 8];
        body.extend(1u32.to_le_bytes());
        body.extend([7u8; 16]);
        let id: [u8; 16] = md5::Md5::digest(&body).into();
        let mut real = Vec::new();
        real.extend_from_slice(MAGIC);
        real.extend_from_slice(&((HEADER_LEN + body.len()) as u64).to_le_bytes());
        real.extend_from_slice(&[0u8; 16]);
        real.extend_from_slice(&id);
        real.extend_from_slice(MAIN);
        real.extend_from_slice(&body);
        assert_eq!(inventory(&real).sealed, [(id, [7u8; 16])]);
        // a set id that isn't its Main's hash seals nothing
        assert!(inventory(&main_packet(1, &[7])).sealed.is_empty());
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
    fn a_desc_is_bound_only_when_its_id_hashes_its_size_and_name() {
        let (md5_16k, size, name) = ([4u8; 16], 5_000_000u64, b"a.mkv");
        let id: [u8; 16] =
            md5::Md5::new().chain_update(md5_16k).chain_update(size.to_le_bytes()).chain_update(name).finalize().into();
        let real = |size: u64| {
            let mut body = id.to_vec();
            body.extend([3u8; 16]);
            body.extend(md5_16k);
            body.extend(size.to_le_bytes());
            body.extend(b"a.mkv\0\0\0");
            packet(1, FILE_DESC, &body)
        };
        assert!(inventory(&real(size)).files[0].bound);
        // the same id claiming another size
        assert!(!inventory(&real(size + 1)).files[0].bound);
        assert!(!inventory(&desc(1, 1, "a.mkv", size)).files[0].bound);
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

    /// a Creator packet's body as a writer pads it: to a multiple of 4 with NULs
    fn creator(set: u8, text: &[u8]) -> Vec<u8> {
        let mut body = text.to_vec();
        while !body.len().is_multiple_of(4) {
            body.push(0);
        }
        packet(set, CREATOR, &body)
    }

    #[test]
    fn the_creator_is_kept_per_set() {
        let parpar = b"ParPar v0.4.1 x64 [https://github.com/animetosho/parpar]";
        let data = [
            desc(1, 1, "a.mkv", 1),
            creator(1, parpar),
            // a volume repeats it, and a second packet in the same set doesn't replace the first
            creator(1, b"Created by par2cmdline version 0.8.1."),
            creator(2, b"Created by par2cmdline version 0.8.1."),
        ]
        .concat();
        let inv = inventory(&data);
        assert_eq!(inv.creator_of(&[1; 16]), Some(std::str::from_utf8(parpar).unwrap()));
        assert_eq!(inv.creator_of(&[2; 16]), Some("Created by par2cmdline version 0.8.1."));
        assert_eq!(inv.creator_of(&[3; 16]), None);
        assert_eq!(inv.creators.len(), 2);
    }

    #[test]
    fn a_creator_keeps_its_url_and_drops_any_address() {
        assert_eq!(creator_text(b"QuickPar 0.9\0\0\0").as_deref(), Some("QuickPar 0.9"));
        assert_eq!(
            creator_text(b"SomeTool 1.2 someone@example.com https://tool.example/").as_deref(),
            Some("SomeTool 1.2 https://tool.example/"),
            "the address goes, the URL stays"
        );
        assert_eq!(creator_text(b"Tool <user@example.org>").as_deref(), Some("Tool"));
        assert_eq!(creator_text(b"Tool mailto:user@example.org").as_deref(), Some("Tool"));
        assert_eq!(
            creator_text(b"Tool [https://user:pw@host.example/x]").as_deref(),
            Some("Tool [https://host.example/x]"),
            "a URL keeps no login"
        );
        assert_eq!(creator_text(b"Tool\tv1\r\n").as_deref(), Some("Tool v1"));
    }

    #[test]
    fn a_garbage_or_long_creator_is_cut_down() {
        assert_eq!(creator_text(&[0xff, 0xfe, 0x01, 0x02, 0x80]), None, "no letter, no creator");
        assert_eq!(creator_text(b""), None);
        assert_eq!(creator_text(b"\0ParPar"), None, "nothing before the padding");
        assert_eq!(creator_text(b"Par\xffPar\x07v1").as_deref(), Some("Par Par v1"), "only printable ASCII");
        let long = creator_text(&[b'x'; 10_000]).unwrap();
        assert_eq!(long.len(), CREATOR_MAX);
        let words = creator_text(&b"ab ".repeat(100)).unwrap();
        assert!(words.len() <= CREATOR_MAX && !words.ends_with(' '), "{words:?}");
        // a packet of garbage leaves the set's files alone
        let inv = inventory(&[desc(1, 1, "a.mkv", 1), packet(1, CREATOR, &[0x90; 8])].concat());
        assert_eq!((inv.files.len(), inv.creators.len()), (1, 0));
    }

    #[test]
    fn ifsc_incomplete_or_capped_slices_leave_crc_unknown() {
        let block_size = 1024u64;
        let mut main_body = block_size.to_le_bytes().to_vec();
        main_body.extend(1u32.to_le_bytes());
        main_body.extend([1u8; 16]);
        while !main_body.len().is_multiple_of(4) {
            main_body.push(0);
        }
        let main = packet(1, MAIN, &main_body);

        // 1. File with 4,097 blocks (> MAX_FILES = 4,096): must leave crc32 as None
        let capped_blocks = (MAX_FILES + 1) as u64;
        let desc_capped = desc(1, 1, "huge.bin", capped_blocks * block_size);
        let mut ifsc_capped_body = vec![1u8; 16];
        for _ in 0..=MAX_FILES {
            ifsc_capped_body.extend([0xAAu8; 16]);
            ifsc_capped_body.extend(0x1234_5678u32.to_le_bytes());
        }
        let ifsc_capped = packet(1, IFSC, &ifsc_capped_body);
        let inv_capped = inventory(&[main.clone(), desc_capped, ifsc_capped].concat());
        assert_eq!(inv_capped.files[0].crc32, None, "exceeding MAX_FILES must leave CRC unknown");

        // 2. File requiring 3 blocks but IFSC only supplies 2 slices: must leave crc32 as None
        let desc_short = desc(1, 1, "short_ifsc.bin", 3 * block_size);
        let mut ifsc_short_body = vec![1u8; 16];
        for _ in 0..2 {
            ifsc_short_body.extend([0xBBu8; 16]);
            ifsc_short_body.extend(0x1111_2222u32.to_le_bytes());
        }
        let ifsc_short = packet(1, IFSC, &ifsc_short_body);
        let inv_short = inventory(&[main, desc_short, ifsc_short].concat());
        assert_eq!(inv_short.files[0].crc32, None, "incomplete slice list must leave CRC unknown");
    }

    #[test]
    fn ifsc_crc_folding_uses_recovery_set_own_block_size() {
        // Set 1 has block_size = 4096; Set 2 has block_size = 2048
        let mut main1_body = 4096u64.to_le_bytes().to_vec();
        main1_body.extend(1u32.to_le_bytes());
        main1_body.extend([1u8; 16]);
        let main1 = packet(1, MAIN, &main1_body);

        let mut main2_body = 2048u64.to_le_bytes().to_vec();
        main2_body.extend(1u32.to_le_bytes());
        main2_body.extend([2u8; 16]);
        let main2 = packet(2, MAIN, &main2_body);

        // Set 2 has a 2048-byte file (1 full block in Set 2 -> 0 padding bytes, so folded CRC == slice_crc2).
        // If Set 1's block_size (4096) had overwritten Set 2's, unpadding 2048 zero bytes would corrupt the CRC.
        let slice_crc2 = 0xCAFE_BABEu32;
        let desc2 = desc(2, 2, "set2.bin", 2048);
        let mut ifsc2_body = vec![2u8; 16];
        ifsc2_body.extend([0u8; 16]);
        ifsc2_body.extend(slice_crc2.to_le_bytes());
        let ifsc2 = packet(2, IFSC, &ifsc2_body);

        // Set 3 has no Main packet: its IFSC must not borrow Set 1 or Set 2's block_size
        let desc3 = desc(3, 3, "set3.bin", 2048);
        let mut ifsc3_body = vec![3u8; 16];
        ifsc3_body.extend([0u8; 16]);
        ifsc3_body.extend(slice_crc2.to_le_bytes());
        let ifsc3 = packet(3, IFSC, &ifsc3_body);

        // Put main1 after main2 so a single global block_size variable would be overwritten by 4096
        let inv = inventory(&[main2, main1, desc2, ifsc2, desc3, ifsc3].concat());
        let f2 = inv.files.iter().find(|f| f.name == "set2.bin").unwrap();
        let f3 = inv.files.iter().find(|f| f.name == "set3.bin").unwrap();
        assert_eq!(f2.crc32, Some(slice_crc2));
        assert_eq!(f3.crc32, None);
    }
}
