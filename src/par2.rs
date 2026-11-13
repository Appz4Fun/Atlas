//! Pull real filenames out of a base .par2 file (deobfuscation).
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic))]

#[cfg(test)]
use md5::Digest;

use crate::archive::names::{best_name, clean_name};

#[cfg(test)]
const MAGIC: &[u8; 8] = b"PAR2\0PKT";
#[cfg(test)]
const FILE_DESC: &[u8; 16] = b"PAR 2.0\0FileDesc";
#[cfg(test)]
const HEADER_LEN: usize = 64;

/// Does a subject name a base `.par2` index (not a `.volNNN+MM.par2` volume)? `par_rs`'s rule,
/// as asked for in `docs/PARITY_REQUESTS.md` §5: the name ends right after `.par2` (end of
/// subject, whitespace, a quote or a closing bracket follows), so `a.par2.rar` is not one and
/// `"a.par2"`, `(a.par2)` are. One allocation-free pass per subject.
pub fn is_base_par2(subject: &str) -> bool {
    crate::parity::is_base_par2(subject)
}

/// The most files, Unicode names, creators and Main ids a read keeps.
const MAX_FILES: usize = 4_096;
#[cfg(test)]
const MAIN: &[u8; 16] = b"PAR 2.0\0Main\0\0\0\0";
#[cfg(test)]
const UNICODE: &[u8; 16] = b"PAR 2.0\0UniFileN";
#[cfg(test)]
const IFSC: &[u8; 16] = b"PAR 2.0\0IFSC\0\0\0\0";
#[cfg(test)]
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

/// Every file in `data`'s packets, decoded and metadata-MD5-checked by par-rs.
///
/// Atlas keeps its historical meanings of `bound` (the FileDesc id authenticates its
/// size and raw name) and `sealed` (a Main body authenticates its complete file list).
/// The crate's similarly named inventory fields have different meanings.
pub fn inventory(data: &[u8]) -> Inventory {
    use std::collections::{HashMap, HashSet};

    // The archive step reads at most two bounded articles. Apply the same bound to all
    // callers before the crate allocates decoded metadata; a capped prefix is never complete.
    const MAX_BYTES: usize = 2 * crate::archive::ARTICLE_CAP;
    let bytes = data.get(..data.len().min(MAX_BYTES)).unwrap_or(&[]);
    let mut limited = bytes.len() != data.len();
    let mut packets = par_rs::packet::PacketIter::new(bytes, par_rs::packet::ScanOptions::UNVERIFIED);
    let mut metadata = Vec::new();
    let mut working_bytes = 0usize;
    for packet in packets.by_ref() {
        let Ok(packet) = packet else {
            limited = true;
            break;
        };
        let kind = crate::parity::PacketKind::from_type(&packet.header.packet_type);
        if !matches!(
            kind,
            crate::parity::PacketKind::Main
                | crate::parity::PacketKind::PkdMain
                | crate::parity::PacketKind::FileDesc
                | crate::parity::PacketKind::Ifsc
                | crate::parity::PacketKind::UniFileN
                | crate::parity::PacketKind::Creator
        ) {
            if !matches!(
                kind,
                crate::parity::PacketKind::RecvSlic
                    | crate::parity::PacketKind::PkdRecvS
                    | crate::parity::PacketKind::FileSlic
            ) && !par_rs::packet::verify_packet_md5(bytes, packet.offset, &packet.header).unwrap_or(false)
            {
                limited = true;
            }
            continue;
        }
        let Ok(length) = usize::try_from(packet.header.length) else {
            limited = true;
            break;
        };
        // Reserve the working set BEFORE decoding: 8 bytes per serialized byte covers
        // the compact input, decoded names/checksums (including Vec growth), and CRC
        // folding scratch; names get another 8 bytes per raw byte for lossy decoding
        // and Atlas's path-preserving copy. 512 per packet covers map entries. The aggregate
        // stays inside the existing two-article byte budget, independent of IFSC count.
        let extra_name = match kind {
            crate::parity::PacketKind::FileDesc => length.saturating_sub(120),
            crate::parity::PacketKind::UniFileN => length.saturating_sub(80),
            crate::parity::PacketKind::Creator => length.saturating_sub(64),
            _ => 0,
        };
        let charge = length.checked_add(extra_name).and_then(|n| n.checked_mul(8)).and_then(|n| n.checked_add(512));
        let Some(total) = charge.and_then(|n| working_bytes.checked_add(n)).filter(|n| *n <= MAX_BYTES) else {
            limited = true;
            break;
        };
        if !par_rs::packet::verify_packet_md5(bytes, packet.offset, &packet.header).unwrap_or(false) {
            limited = true;
            continue;
        }
        let Some(start) = usize::try_from(packet.offset).ok() else {
            limited = true;
            break;
        };
        let Some(end) = start.checked_add(length) else {
            limited = true;
            break;
        };
        let Some(raw) = bytes.get(start..end) else {
            limited = true;
            break;
        };
        working_bytes = total;
        metadata.extend_from_slice(raw);
    }
    limited |= packets.position() != bytes.len() as u64 || packets.stats().skipped_bytes != 0;
    // The crate owns framing, validation, decoding and CRC folding. Recovery payloads
    // never enter this compact metadata buffer and their packet MD5 is never hashed.
    let bytes = metadata.as_slice();
    let mut inspection = crate::parity::inspect(bytes);
    limited |= inspection.packets.iter().any(|packet| match packet.kind {
        crate::parity::PacketKind::FileDesc => packet.length < 120,
        crate::parity::PacketKind::Main => packet.length < 76,
        crate::parity::PacketKind::PkdMain => packet.length < 84,
        crate::parity::PacketKind::Ifsc | crate::parity::PacketKind::UniFileN => packet.length < 80,
        _ => false,
    });
    for length in [
        inspection.files.len(),
        inspection.main.len(),
        inspection.ifsc.len(),
        inspection.unicode_names.len(),
        inspection.creators.len(),
    ] {
        limited |= length > MAX_FILES;
    }
    inspection.files.truncate(MAX_FILES);
    inspection.main.truncate(MAX_FILES);
    inspection.ifsc.truncate(MAX_FILES);
    inspection.unicode_names.truncate(MAX_FILES);
    inspection.creators.truncate(MAX_FILES);
    let mut ids_left = MAX_FILES;
    for main in &mut inspection.main {
        limited |= main.truncated;
        for ids in [&mut main.recovery_set, &mut main.non_recovery_set] {
            limited |= ids.len() > ids_left;
            ids.truncate(ids_left);
            ids_left -= ids.len();
        }
    }

    // Keep the first complete IFSC for each file, as Atlas has always done. The
    // inspection accepts whole checksum pairs from a malformed tail; such a tail
    // must not certify a whole-file CRC in the archive inventory.
    let mut ifsc_seen = HashSet::new();
    let ifsc_lengths = inspection
        .packets
        .iter()
        .filter(|packet| {
            packet.kind == crate::parity::PacketKind::Ifsc && packet.md5_ok == Some(true) && packet.length >= 80
        })
        .map(|packet| packet.length);
    inspection.ifsc = ifsc_lengths
        .zip(std::mem::take(&mut inspection.ifsc))
        .filter_map(|(length, ifsc)| {
            if !(length - 80).is_multiple_of(20) || ifsc.slices.is_empty() {
                limited = true;
                return None;
            }
            ifsc_seen.insert((ifsc.set_id, ifsc.file_id)).then_some(ifsc)
        })
        .collect();

    let descriptions: HashMap<_, _> =
        inspection.files.iter().rev().map(|file| ((file.set_id, file.file_id), file)).collect();
    // Atlas stores posted paths, while the library's display names are basenames.
    // Decode only ranges the library has already authenticated and identified.
    let unicode_packets = inspection.packets.iter().filter(|packet| {
        packet.kind == crate::parity::PacketKind::UniFileN && packet.md5_ok == Some(true) && packet.length >= 80
    });
    let unicode_names: HashMap<_, _> = unicode_packets
        .zip(&inspection.unicode_names)
        .filter_map(|(packet, name)| {
            let start = packet.offset.checked_add(80)?;
            let end = packet.offset.checked_add(usize::try_from(packet.length).ok()?)?;
            let units = bytes
                .get(start..end)?
                .as_chunks::<2>()
                .0
                .iter()
                .map(|unit| u16::from_le_bytes(*unit))
                .take_while(|unit| *unit != 0)
                .collect::<Vec<_>>();
            clean_name(&String::from_utf16_lossy(&units)).map(|value| ((name.set_id, name.file_id), value))
        })
        .collect();
    let mut blocks = HashMap::new();
    let mut conflicting_blocks = HashSet::new();
    for main in &inspection.main {
        if let Some(previous) = blocks.insert(main.set_id, main.slice_size)
            && previous != main.slice_size
        {
            conflicting_blocks.insert(main.set_id);
        }
    }
    limited |= !conflicting_blocks.is_empty();
    let native = crate::parity::inventory_of(&inspection);
    limited |= native.files.len() > MAX_FILES;
    let mut inv =
        Inventory { block_size: native.block_size, truncated: inspection.truncated || limited, ..Inventory::default() };
    for file in native.files.into_iter().take(MAX_FILES) {
        let Some(description) = descriptions.get(&(file.set_id, file.file_id)) else { continue };
        let name = unicode_names
            .get(&(file.set_id, file.file_id))
            .cloned()
            .or_else(|| clean_name(&String::from_utf8_lossy(&description.name_raw)));
        let Some(name) = name else { continue };
        let bound = par_rs::packet::compute_file_id(&file.md5_16k, file.size, &description.name_raw).0 == file.file_id;
        inv.files.push(Par2File {
            set_id: file.set_id,
            file_id: file.file_id,
            md5: file.md5,
            md5_16k: file.md5_16k,
            size: file.size,
            name,
            bound,
            crc32: if file.size == 0 || conflicting_blocks.contains(&file.set_id) { None } else { file.crc32 },
        });
    }
    let described: HashSet<_> = inv.files.iter().map(|file| (file.set_id, file.file_id)).collect();
    inv.complete = !inv.truncated
        && !inspection.main.is_empty()
        && inspection.main.iter().all(|main| {
            !main.truncated
                && !conflicting_blocks.contains(&main.set_id)
                && main
                    .recovery_set
                    .iter()
                    .chain(&main.non_recovery_set)
                    .all(|id| described.contains(&(main.set_id, *id)))
        });
    let mut sealed = HashSet::new();
    for main in &inspection.main {
        if !limited && !main.truncated && main.body_hashes_to_set_id {
            for id in main.recovery_set.iter().chain(&main.non_recovery_set) {
                if sealed.insert((main.set_id, *id)) {
                    inv.sealed.push((main.set_id, *id));
                }
            }
        }
    }
    for creator in &inspection.creators {
        if let Some(set) = creator.set_id
            && inv.creator_of(&set).is_none()
            && let Some(text) = creator_text(creator.raw.as_bytes())
        {
            inv.creators.push((set, text));
        }
    }
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
    p.extend_from_slice(&par_rs::packet::compute_file_id(&[4; 16], size, name.as_bytes()).0); // file id
    p.extend_from_slice(&[3u8; 16]); // md5
    p.extend_from_slice(&[4u8; 16]); // md5 16k
    p.extend_from_slice(&size.to_le_bytes());
    p.extend_from_slice(&name_bytes);
    let hash = md5::Md5::digest(&p[32..]);
    p[16..32].copy_from_slice(&hash);
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
        let hash = md5::Md5::digest(&p[32..]);
        p[16..32].copy_from_slice(&hash);
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
        let hash = md5::Md5::digest(&real[32..]);
        real[16..32].copy_from_slice(&hash);
        assert_eq!(inventory(&real).sealed, [(id, [7u8; 16])]);
        // a set id that isn't its Main's hash seals nothing
        assert!(inventory(&main_packet(1, &[7])).sealed.is_empty());
    }

    #[test]
    fn repeated_metadata_keeps_every_distinct_file() {
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
    fn posted_paths_remain_available_for_archive_associations() {
        let data = [
            desc(1, 1, "Season.01/Episode.01.mkv", 1),
            desc(1, 5, "Season.01/Episode.02.mkv", 2),
            uni(1, 5, "Сезон.01/Серия.02.mkv"),
        ]
        .concat();
        let inventory = inventory(&data);
        assert_eq!(inventory.files[0].name, "Season.01/Episode.01.mkv");
        assert_eq!(inventory.files[1].name, "Сезон.01/Серия.02.mkv");
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
            assert!(inv.truncated, "a malformed source cannot certify completeness: {len}");
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

        let many: Vec<u8> = (0..5_000).flat_map(|i| file_desc_packet(&format!("{i}.mkv"), 1)).collect();
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
    fn ifsc_large_inventory_folds_but_incomplete_slices_leave_crc_unknown() {
        let block_size = 1024u64;
        let mut main_body = block_size.to_le_bytes().to_vec();
        main_body.extend(1u32.to_le_bytes());
        main_body.extend([1u8; 16]);
        while !main_body.len().is_multiple_of(4) {
            main_body.push(0);
        }
        let main = packet(1, MAIN, &main_body);

        // A file with more slices than the row limit still gets a whole-file CRC.
        let capped_blocks = (MAX_FILES + 1) as u64;
        let desc_capped = desc(1, 1, "huge.bin", capped_blocks * block_size);
        let mut ifsc_capped_body = vec![1u8; 16];
        for _ in 0..=MAX_FILES {
            ifsc_capped_body.extend([0xAAu8; 16]);
            ifsc_capped_body.extend(0x1234_5678u32.to_le_bytes());
        }
        let ifsc_capped = packet(1, IFSC, &ifsc_capped_body);
        let inv_capped = inventory(&[main.clone(), desc_capped, ifsc_capped].concat());
        assert_eq!(
            inv_capped.files[0].crc32,
            crate::parity::file_crc_from_ifsc(
                &vec![0x1234_5678; MAX_FILES + 1],
                block_size,
                capped_blocks * block_size
            )
        );

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

    #[test]
    fn first_intact_ifsc_wins_over_conflicting_repeated_metadata() {
        let mut main = 1024u64.to_le_bytes().to_vec();
        main.extend(1u32.to_le_bytes());
        main.extend([1; 16]);
        let ifsc = |crc: u32| {
            let mut body = vec![1; 16];
            body.extend([0; 16]);
            body.extend(crc.to_le_bytes());
            packet(1, IFSC, &body)
        };
        let data =
            [packet(1, MAIN, &main), desc(1, 1, "file.bin", 1024), ifsc(0xCAFE_BABE), ifsc(0xDEAD_BEEF)].concat();
        let inventory = inventory(&data);
        assert_eq!(inventory.files[0].crc32, Some(0xCAFE_BABE));
    }
}
