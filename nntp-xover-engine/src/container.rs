//! The `.nxv` block: up to 65,536 overview rows in one self-describing,
//! 64-byte aligned value that a reader uses in place from an mmap.
//!
//! ```text
//! 0     NxvBlockHeader (64)
//! 64    SectionEntry x 16 (256)
//! 320   SkipCheckpoints (1 per 128 rows, 64 each)   \  stay uncompressed
//!       MsgIdXor8 (header + fingerprints)            /  in a cold block
//!       -- cold prefix ends here, 64-aligned --
//!       FsstTables, MsgIdBucketIndex, u16 id columns, u32 offsets,
//!       string pool, packed message-id tails, numeric varints, references
//! ```
//!
//! Every row is either *structured* (each field taken apart by its codec and
//! checked to write back byte for byte) or *literal* (the whole line kept in
//! the literal dictionary, marked by bit 15 of its PadMask). The ingest check
//! is what makes the block lossless for any input.
//!
//! Numbers go into one varint stream, row after row, each a delta against the
//! row before; a checkpoint every 128 rows holds the state to start from, so
//! a row is at most 127 steps from a known state and a range walks one step
//! per row.
//!
//! Version 2 (what the builder writes; readers take both) changes how some
//! sections are kept, nothing else:
//! - A row's message-id tail data is a *segment*: the packed tail, then, when
//!   the subject is a random token kept per row, the packed subject. Packed
//!   strings delimit themselves, soo `MsgIdTailOffsets` holds one u32 per 32
//!   rows (where the first one's segment starts) instead of one per row, and a
//!   row's segment is at most 31 segments from one.
//! - A per-row token subject has the subject template id one past the subject
//!   dictionary's last entry, and no dictionary entry.
//! - String pool entries delimit themselves too (a packed entry by its header,
//!   a raw or FSST one by a varint length before it), soo `StringPoolOffsets`
//!   holds one u32 per 32 entries of each dictionary.
//! - Each u16 id column is one value when the block has one, a palette and a u8
//!   per row when it has at most 256, nothing when each row's id is its row
//!   number, a u16 per row otherwise (see [`IdCol`]).
//! - A subject template with three or more counters writes a mask of the ones
//!   that changed, then only those deltas.
//! - A sub-block whose rows have no references stores nothing for them.
//! - A dictionary may be *shaped* ([`DICT_SHAPED`]): each entry a shape id and
//!   its alphabet runs packed, for posters made of random tokens.
//! - A sub-block whose article numbers run one by one stores none, and one
//!   whose Xref slot 0 is always the article number stores no slot 0 (flags in
//!   its checkpoint, see [`CP_CONSECUTIVE`]).

use anyhow::{Context, Result, bail, ensure};
use std::collections::HashMap;

use crate::date::{DATE_BUF, STYLE_FREE_BITS, format_date, parse_date};

/// Style word bits for an empty `Lines` / `Bytes` field (the value isn't stored).
pub(crate) const STYLE_NO_LINES: u32 = 1 << 5;
pub(crate) const STYLE_NO_BYTES: u32 = 1 << 6;
/// The subject is the message-id's local part (obfuscating posters do this): no template is stored.
pub(crate) const STYLE_SUBJ_IS_LOCAL: u32 = 1 << 7;
const _: () = assert!(STYLE_FREE_BITS == STYLE_NO_LINES | STYLE_NO_BYTES | STYLE_SUBJ_IS_LOCAL);
use crate::msgid_codec::{
    in_pack_alphabet, pack_msgid_tail, packed_tail_len, packs_by_alphabet, split_msgid, split_prefix,
    unpack_msgid_tail_into,
};
use crate::msgid_index::{BucketIndexView, Xor8Builder, Xor8View, build_bucket_index, msgid_hash};
use crate::subject_codec::{SubjectNums, extract_subject_template, render_subject};
use crate::varint::{
    parse_canonical_i64, parse_canonical_u64, read_varint_unchecked, write_i64_ascii, write_u64_ascii, write_varint,
    zigzag_decode, zigzag_encode,
};
use crate::xref_codec::{MAX_XREF_GROUPS, XREF_PREFIX, extract_xref_skeleton, render_xref, slot_count};

#[cfg(target_endian = "big")]
compile_error!("the .nxv format is read in place and assumes a little-endian host");

pub const MAGIC: [u8; 4] = *b"NXVB";
/// The first format: a u32 offset per row and per dictionary entry, every id column a u16 per row.
pub const VERSION_1: u16 = 1;
/// Per-row token subjects, self-delimiting tails and pool entries with a checkpoint every 32, narrow id columns,
/// masked subject counters, sub-blocks without references stored empty. See the module notes.
pub const VERSION_2: u16 = 2;
/// The version the builder writes.
pub const VERSION: u16 = VERSION_2;
/// A subject at least this long, made only of the packing alphabet, that no other row of the block has, is kept per
/// row instead of in the subject dictionary.
pub const TOKEN_SUBJECT_MIN: usize = 12;
/// A subject template with at least this many counters writes a mask of the ones that changed (version 2).
const MASKED_COUNTERS: u32 = 3;
pub const FLAG_COLD: u16 = 1;
pub const FLAG_XREF_PREFIX: u16 = 2;
pub const SUB_BLOCK_ROWS: usize = 128;
/// Version 2: rows per message-id tail checkpoint (a sub-block holds a whole number of them), and dictionary entries
/// per string pool checkpoint. A row's segment or an entry is then at most this many less one steps from one.
pub const TAIL_CP_ROWS: usize = 32;
pub const POOL_CP_ENTRIES: usize = 32;
const _: () = assert!(SUB_BLOCK_ROWS.is_multiple_of(TAIL_CP_ROWS));
pub const MAX_BLOCK_ROWS: usize = 65_536;
/// PadMask bit 15: the row is kept whole in the literal dictionary.
pub const PADMASK_LITERAL: u16 = 0x8000;
pub const HEADER_LEN: usize = 64;
pub const SECTION_TABLE_LEN: usize = 16 * 16;
pub const DATA_START: usize = HEADER_LEN + SECTION_TABLE_LEN;
/// The most a hot block can be: 65,536 rows of the longest lines servers send, with room to spare. A
/// header claiming more is damaged.
pub const MAX_HOT_BLOCK_BYTES: usize = 1 << 30;
/// The xxh3 hash of the hot block after its section table (checkpoints, filter, body) is kept in the `encoding_flags` of
/// these two section entries, low word then high word. The header CRC covers the section table, soo it
/// covers this hash too.
const BODY_HASH_LO: SectionId = SectionId::FsstTables;
const BODY_HASH_HI: SectionId = SectionId::ReferencesData;

/// The section entries whose `encoding_flags` hold the store's fields (`BlockMeta`): the server a block's rows came
/// from and the slab it was sealed from. The header CRC covers the section table, soo it covers these too, and a
/// block built without them reads as server 0, slab 0.
const META_SLAB_GEN: SectionId = SectionId::SkipCheckpoints;
const META_SERVER: SectionId = SectionId::MsgIdXor8;

/// What a store keeps about a block besides its rows: which server sent them, and which slab they were sealed from
/// (recovery tells a slab's leftover log was committed by it).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockMeta {
    pub server_id: u16,
    pub slab_gen: u32,
}

/// The body hash a block's section table holds.
fn stored_body_hash(sections: &[SectionEntry]) -> u64 {
    sections[BODY_HASH_LO as usize].encoding_flags as u64
        | ((sections[BODY_HASH_HI as usize].encoding_flags as u64) << 32)
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(C, align(64))]
pub struct NxvBlockHeader {
    pub magic: [u8; 4],
    pub version: u16,
    pub flags: u16,
    pub min_art_num: i64,
    pub max_art_num: i64,
    pub min_timestamp: i64,
    pub max_timestamp: i64,
    pub row_count: u32,
    pub checkpoint_count: u32,
    pub total_block_bytes: u32,
    pub header_crc32: u32,
    /// where the cold prefix (header, sections table, checkpoints, filter) ends
    pub cold_prefix_bytes: u32,
    /// the hot block's length (equal to total_block_bytes when hot)
    pub hot_block_bytes: u32,
}
const _: () = assert!(std::mem::size_of::<NxvBlockHeader>() == HEADER_LEN);

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(C, align(16))]
pub struct SectionEntry {
    pub offset: u32,
    pub byte_len: u32,
    pub item_count: u32,
    pub encoding_flags: u32,
}
const _: () = assert!(std::mem::size_of::<SectionEntry>() == 16);

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SectionId {
    FsstTables = 0,
    SkipCheckpoints = 1,
    MsgIdXor8 = 2,
    MsgIdBucketIndex = 3,
    SubjTplIds = 4,
    SubjPadMasks = 5,
    XrefSkelIds = 6,
    AuthorIds = 7,
    DomainIds = 8,
    MsgIdPfxIds = 9,
    MsgIdTailOffsets = 10,
    StringPoolOffsets = 11,
    StringPoolData = 12,
    MsgIdTailData = 13,
    NumericVarints = 14,
    ReferencesData = 15,
}

/// The delta state at the first row of a 128-row sub-block. Fields that the
/// first row's own value doesn't fit (or that a literal row lacks) hold 0;
/// a row's deltas are always against exactly these values.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(C, align(64))]
pub struct SkipCheckpoint {
    pub base_art_num: i64,
    pub base_timestamp: i64,
    pub base_xref_slot1: i64,
    pub base_bytes: i32,
    pub base_lines: i32,
    pub base_subj_nums: [i32; 5],
    pub base_xref_slot0_delta: i32,
    /// where the sub-block's numbers start; in version 2 the top two bits are [`CP_CONSECUTIVE`] and
    /// [`CP_SLOT0_IS_ART`] (a block is at most 1 GiB, soo an offset never reaches them)
    pub varint_byte_offset: u32,
    pub refs_byte_offset: u32,
}
const _: () = assert!(std::mem::size_of::<SkipCheckpoint>() == 64);

/// Version 2 checkpoint flag: each row's article number is one more than the row before's, and none is stored.
pub const CP_CONSECUTIVE: u32 = 1 << 31;
/// Version 2 checkpoint flag: every structured row's Xref slot 0 is its own article number, and none is stored.
pub const CP_SLOT0_IS_ART: u32 = 1 << 30;
const CP_OFFSET_MASK: u32 = CP_SLOT0_IS_ART - 1;
const _: () = assert!(MAX_HOT_BLOCK_BYTES as u64 <= CP_OFFSET_MASK as u64 + 1);

pub const DICT_SUBJ: usize = 0;
pub const DICT_XREF: usize = 1;
pub const DICT_AUTHOR: usize = 2;
pub const DICT_DOMAIN: usize = 3;
pub const DICT_PFX: usize = 4;
pub const DICT_LITERAL: usize = 5;
pub const NUM_DICTS: usize = 6;

const DICT_RAW: u16 = 0;
const DICT_FSST: u16 = 1;
/// each entry packed by alphabet like a message-id tail (random strings that FSST can't shrink)
const DICT_PACKED: u16 = 2;
/// Version 2: each entry a shape id (varint), then each run of the packing alphabet in it packed like a message-id
/// tail. A shape is the entry with each run as one [`SHAPE_SLOT`] byte (`\0 <\0@\0>` for most obfuscated posters);
/// the shapes are a table in the FsstTables section: `n_symbols + 1` u32 offsets, then the shapes' bytes.
const DICT_SHAPED: u16 = 3;
const SHAPE_SLOT: u8 = 0;

/// A dictionary's entries in shaped mode: (the shape table, how many shapes it has, each entry coded), or None when an
/// entry holds a [`SHAPE_SLOT`] byte or there are more shapes than a `u16` counts.
fn shape_dict(entries: &[&[u8]]) -> Option<(Vec<u8>, usize, Vec<Vec<u8>>)> {
    let mut ids: HashMap<Vec<u8>, u32> = HashMap::new();
    let mut shapes: Vec<Vec<u8>> = Vec::new();
    let mut coded = Vec::with_capacity(entries.len());
    let mut shape = Vec::new();
    for e in entries {
        if e.contains(&SHAPE_SLOT) {
            return None;
        }
        shape.clear();
        let mut runs = Vec::with_capacity(e.len());
        let mut i = 0;
        while i < e.len() {
            if in_pack_alphabet(e[i]) {
                let start = i;
                while i < e.len() && in_pack_alphabet(e[i]) {
                    i += 1;
                }
                pack_msgid_tail(&e[start..i], &mut runs);
                shape.push(SHAPE_SLOT);
            } else {
                shape.push(e[i]);
                i += 1;
            }
        }
        let id = match ids.get(shape.as_slice()) {
            Some(&id) => id,
            None => {
                let id = shapes.len() as u32;
                ids.insert(shape.clone(), id);
                shapes.push(shape.clone());
                id
            }
        };
        let mut c = Vec::with_capacity(runs.len() + 2);
        write_varint(id as u64, &mut c);
        c.extend_from_slice(&runs);
        coded.push(c);
    }
    if shapes.len() > u16::MAX as usize {
        return None;
    }
    let mut table = Vec::new();
    let mut at = 0u32;
    for s in &shapes {
        table.extend_from_slice(&at.to_le_bytes());
        at += s.len() as u32;
    }
    table.extend_from_slice(&at.to_le_bytes());
    for s in &shapes {
        table.extend_from_slice(s);
    }
    Some((table, shapes.len(), coded))
}

/// Shape `id` of a shape table (see [`DICT_SHAPED`]) with `n` shapes; empty when there is none.
#[inline]
fn shape_of(table: &[u8], n: usize, id: usize) -> &[u8] {
    let Some(offs) = table.get(..4 * (n + 1)).and_then(cast_slice::<u32>) else { return &[] };
    let body = &table[4 * (n + 1)..];
    match (offs.get(id), offs.get(id + 1)) {
        (Some(&a), Some(&b)) => body.get(a as usize..b as usize).unwrap_or(&[]),
        _ => &[],
    }
}

/// Appends the entry a shaped entry `coded` stands for. Returns how many bytes of `coded` it took.
#[inline]
fn unshape_into(coded: &[u8], table: &[u8], n: usize, out: Option<&mut Vec<u8>>) -> usize {
    let mut pos = 0usize;
    let id = read_varint_unchecked(coded, &mut pos) as usize;
    let shape = shape_of(table, n, id);
    match out {
        Some(out) => {
            for &b in shape {
                if b == SHAPE_SLOT {
                    let run = coded.get(pos..).unwrap_or(&[]);
                    let len = packed_tail_len(run).min(run.len());
                    unpack_msgid_tail_into(&run[..len], out);
                    pos += len;
                } else {
                    out.push(b);
                }
            }
        }
        None => {
            for _ in shape.iter().filter(|&&b| b == SHAPE_SLOT) {
                pos = pos.saturating_add(packed_tail_len(coded.get(pos..).unwrap_or(&[])));
            }
        }
    }
    pos.min(coded.len())
}

#[derive(Copy, Clone, Debug, Default)]
#[repr(C)]
pub struct DictDesc {
    pub first_entry: u32,
    pub entry_count: u32,
    /// FSST table (u64 symbols then u8 lengths) offset in the FsstTables section
    pub table_offset: u32,
    pub n_symbols: u16,
    pub mode: u16,
}

#[derive(Copy, Clone, Debug, Default)]
#[repr(C)]
pub struct FsstDirectory {
    pub dicts: [DictDesc; 8],
    /// one byte per xref skeleton: how many number slots it has
    pub slot_counts_offset: u32,
    pub slot_counts_len: u32,
    /// one byte per subject template: which counters it has (`subj_mask`)
    pub subj_masks_offset: u32,
    pub subj_masks_len: u32,
}
const FSST_DIR_LEN: usize = std::mem::size_of::<FsstDirectory>();
const _: () = assert!(FSST_DIR_LEN == 144);

/// Types that are plain bytes in any bit pattern, so a slice of them can be
/// read straight out of a mapped file.
///
/// # Safety
/// Implementors must be `repr(C)`/primitive with no padding-dependent
/// invariants and valid for every bit pattern.
pub unsafe trait Pod: Copy + 'static {}
unsafe impl Pod for u8 {}
unsafe impl Pod for u16 {}
unsafe impl Pod for u32 {}
unsafe impl Pod for u64 {}
unsafe impl Pod for i64 {}
unsafe impl Pod for NxvBlockHeader {}
unsafe impl Pod for SectionEntry {}
unsafe impl Pod for SkipCheckpoint {}
unsafe impl Pod for DictDesc {}
unsafe impl Pod for FsstDirectory {}
unsafe impl Pod for crate::msgid_index::Xor8Header {}

/// `bytes` as a slice of `T`, without copying. None if misaligned or not a
/// whole number of `T`.
#[inline]
pub fn cast_slice<T: Pod>(bytes: &[u8]) -> Option<&[T]> {
    let size = std::mem::size_of::<T>();
    if !(bytes.as_ptr() as usize).is_multiple_of(std::mem::align_of::<T>()) || !bytes.len().is_multiple_of(size) {
        return None;
    }
    // SAFETY: alignment and length checked above; T is Pod (any bit pattern is valid)
    // and the result borrows `bytes`.
    Some(unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<T>(), bytes.len() / size) })
}

#[inline(always)]
pub const fn align_up(n: usize, a: usize) -> usize {
    (n + a - 1) & !(a - 1)
}

#[derive(Clone, Copy)]
#[repr(C, align(64))]
struct Chunk64([u8; 64]);

/// A 64-byte aligned byte buffer, for holding a block in memory the way an
/// mmap would (pages are aligned, blocks start at 64-byte boundaries).
#[derive(Clone)]
pub struct AlignedBytes {
    chunks: Vec<Chunk64>,
    len: usize,
}

impl AlignedBytes {
    pub fn from_slice(b: &[u8]) -> Self {
        let mut chunks = vec![Chunk64([0; 64]); b.len().div_ceil(64)];
        // SAFETY: chunks is a contiguous run of chunks.len()*64 initialised bytes.
        let dst = unsafe { std::slice::from_raw_parts_mut(chunks.as_mut_ptr().cast::<u8>(), chunks.len() * 64) };
        dst[..b.len()].copy_from_slice(b);
        Self { chunks, len: b.len() }
    }
}

impl std::ops::Deref for AlignedBytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        // SAFETY: as in from_slice; len <= chunks.len()*64.
        unsafe { std::slice::from_raw_parts(self.chunks.as_ptr().cast::<u8>(), self.len) }
    }
}

const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

pub fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c = CRC_TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

/// CRC of the header (with its crc field as zero) and the sections table.
fn header_crc(block: &[u8]) -> u32 {
    let mut tmp = [0u8; DATA_START];
    tmp.copy_from_slice(&block[..DATA_START]);
    tmp[52..56].fill(0);
    crc32(&tmp)
}

fn write_header(out: &mut [u8], h: &NxvBlockHeader) {
    out[0..4].copy_from_slice(&h.magic);
    out[4..6].copy_from_slice(&h.version.to_le_bytes());
    out[6..8].copy_from_slice(&h.flags.to_le_bytes());
    out[8..16].copy_from_slice(&h.min_art_num.to_le_bytes());
    out[16..24].copy_from_slice(&h.max_art_num.to_le_bytes());
    out[24..32].copy_from_slice(&h.min_timestamp.to_le_bytes());
    out[32..40].copy_from_slice(&h.max_timestamp.to_le_bytes());
    out[40..44].copy_from_slice(&h.row_count.to_le_bytes());
    out[44..48].copy_from_slice(&h.checkpoint_count.to_le_bytes());
    out[48..52].copy_from_slice(&h.total_block_bytes.to_le_bytes());
    out[52..56].copy_from_slice(&h.header_crc32.to_le_bytes());
    out[56..60].copy_from_slice(&h.cold_prefix_bytes.to_le_bytes());
    out[60..64].copy_from_slice(&h.hot_block_bytes.to_le_bytes());
}

fn seal_header(block: &mut [u8]) {
    let crc = header_crc(block);
    block[52..56].copy_from_slice(&crc.to_le_bytes());
}

// ---------------------------------------------------------------- rows

/// The parts of a structured row, as the writer puts them back together.
pub(crate) struct RowParts<'x> {
    pub art: u64,
    /// the subject is `pfx` and the unpacked tail
    pub subj_is_local: bool,
    /// the subject is this packed token (kept per row)
    pub subj_packed: Option<&'x [u8]>,
    pub tpl: &'x [u8],
    pub nums: SubjectNums,
    pub author: &'x [u8],
    pub date: &'x [u8],
    pub pfx: &'x [u8],
    pub tail_packed: &'x [u8],
    pub domain: &'x [u8],
    pub refs: &'x [u8],
    pub bytes: Option<i64>,
    pub lines: Option<i64>,
    /// the skeleton dictionary entry: 0 = no field 8, 1 + skeleton otherwise
    pub skel: &'x [u8],
    pub xref_prefix: bool,
    pub xref: &'x [u64],
}

#[inline]
pub(crate) fn render_row(p: &RowParts, out: &mut Vec<u8>) {
    write_u64_ascii(p.art, out);
    out.push(b'\t');
    if p.subj_is_local {
        out.extend_from_slice(p.pfx);
        crate::msgid_codec::unpack_msgid_tail_into(p.tail_packed, out);
    } else if let Some(t) = p.subj_packed {
        crate::msgid_codec::unpack_msgid_tail_into(t, out);
    } else {
        render_subject(p.tpl, &p.nums, out);
    }
    out.push(b'\t');
    out.extend_from_slice(p.author);
    out.push(b'\t');
    out.extend_from_slice(p.date);
    out.extend_from_slice(b"\t<");
    out.extend_from_slice(p.pfx);
    crate::msgid_codec::unpack_msgid_tail_into(p.tail_packed, out);
    out.push(b'@');
    out.extend_from_slice(p.domain);
    out.extend_from_slice(b">\t");
    out.extend_from_slice(p.refs);
    out.push(b'\t');
    if let Some(b) = p.bytes {
        write_i64_ascii(b, out);
    }
    out.push(b'\t');
    if let Some(l) = p.lines {
        write_i64_ascii(l, out);
    }
    if let Some((&1, skel)) = p.skel.split_first() {
        out.push(b'\t');
        if p.xref_prefix {
            out.extend_from_slice(XREF_PREFIX);
        }
        render_xref(skel, p.xref, out);
    }
}

/// The first eight tab-separated fields and whatever follows the eighth tab.
/// Fields 1..=8 of a line and what follows the 8th tab.
type Fields<'a> = ([&'a [u8]; 8], Option<&'a [u8]>);

fn split_fields(line: &[u8]) -> Option<Fields<'_>> {
    let mut f: [&[u8]; 8] = [&[]; 8];
    let mut rest = line;
    for slot in f.iter_mut().take(7) {
        let t = rest.iter().position(|&b| b == b'\t')?;
        *slot = &rest[..t];
        rest = &rest[t + 1..];
    }
    match rest.iter().position(|&b| b == b'\t') {
        Some(t) => {
            f[7] = &rest[..t];
            Some((f, Some(&rest[t + 1..])))
        }
        None => {
            f[7] = rest;
            Some((f, None))
        }
    }
}

/// The 5th field of a raw line: its message-id.
pub(crate) fn msgid_field(line: &[u8]) -> Option<&[u8]> {
    line.split(|&b| b == b'\t').nth(4)
}

/// The article number at the head of a line (leading zeros allowed: such a
/// row is kept literal, but still sorts and routes by its number).
fn art_number(line: &[u8]) -> Option<u64> {
    let f = line.split(|&b| b == b'\t').next()?;
    if f.is_empty() || f.len() > 19 {
        return None;
    }
    let mut v = 0u64;
    for &b in f {
        let d = b.wrapping_sub(b'0');
        if d > 9 {
            return None;
        }
        v = v.checked_mul(10)?.checked_add(d as u64)?;
    }
    (v <= i64::MAX as u64).then_some(v)
}

#[derive(Default)]
struct Dict {
    map: HashMap<Vec<u8>, u32>,
    entries: Vec<Vec<u8>>,
}

impl Dict {
    fn intern(&mut self, s: &[u8]) -> u32 {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = self.entries.len() as u32;
        self.entries.push(s.to_vec());
        self.map.insert(s.to_vec(), id);
        id
    }
}

#[derive(Clone, Copy, Default)]
struct Structured {
    tpl: u32,
    skel: u32,
    author: u32,
    domain: u32,
    pfx: u32,
    pad: u16,
    ts: i64,
    style: u32,
    bytes: i64,
    lines: i64,
    nums: [u32; 5],
    xref: [u64; MAX_XREF_GROUPS],
    nx: u8,
    smask: u8,
    refs: (u32, u32),
    /// the subject is a token kept per row, after the message-id tail (version 2)
    tok: bool,
}

enum Kind {
    Structured(Structured),
    Literal(u32),
}

struct Row {
    art: u64,
    kind: Kind,
}

/// The decode state one row's deltas are taken against.
#[derive(Copy, Clone, Default, Debug)]
pub(crate) struct DeltaState {
    pub art: i64,
    pub ts: i64,
    pub style: u32,
    pub bytes: i64,
    pub lines: i64,
    pub subj: [i64; 5],
    pub slot0d: i64,
    pub xref: [i64; MAX_XREF_GROUPS],
    /// the last xref numbers of the last few skeletons seen in this sub-block: a post cross-posted to the same
    /// groups moves each group's number a little, while rows of other posts in between would make big jumps
    pub memo: SkelMemo,
}

pub(crate) const SKEL_MEMO: usize = 16;

#[derive(Copy, Clone, Debug)]
pub(crate) struct SkelMemo {
    ids: [u32; SKEL_MEMO],
    /// slot 0 holds the slot-0 delta, the others the numbers
    vals: [[i64; MAX_XREF_GROUPS]; SKEL_MEMO],
    next: usize,
}

impl Default for SkelMemo {
    fn default() -> Self {
        Self { ids: [u32::MAX; SKEL_MEMO], vals: [[0; MAX_XREF_GROUPS]; SKEL_MEMO], next: 0 }
    }
}

impl SkelMemo {
    #[inline(always)]
    fn find(&self, skel: u32) -> Option<usize> {
        self.ids.iter().position(|&i| i == skel)
    }
    #[inline(always)]
    fn slot(&mut self, skel: u32) -> usize {
        match self.find(skel) {
            Some(i) => i,
            None => {
                let i = self.next;
                self.next = (self.next + 1) % SKEL_MEMO;
                self.ids[i] = skel;
                i
            }
        }
    }
}

impl DeltaState {
    /// The values a row with skeleton `skel` takes its xref deltas against: that skeleton's last row in this
    /// sub-block, else the previous row's.
    #[inline(always)]
    pub(crate) fn xref_bases(&self, skel: u32) -> [i64; MAX_XREF_GROUPS] {
        match self.memo.find(skel) {
            Some(i) => self.memo.vals[i],
            None => {
                let mut b = self.xref;
                b[0] = self.slot0d;
                b
            }
        }
    }
    #[inline(always)]
    pub(crate) fn xref_done(&mut self, skel: u32, vals: &[i64; MAX_XREF_GROUPS], nx: usize) {
        if nx == 0 {
            return;
        }
        self.slot0d = vals[0];
        self.xref[1..nx].copy_from_slice(&vals[1..nx]);
        let i = self.memo.slot(skel);
        self.memo.vals[i] = *vals;
    }

    pub fn from_checkpoint(cp: &SkipCheckpoint) -> Self {
        let mut s = DeltaState {
            art: cp.base_art_num,
            ts: cp.base_timestamp,
            style: 0,
            bytes: cp.base_bytes as i64,
            lines: cp.base_lines as i64,
            subj: [0; 5],
            slot0d: cp.base_xref_slot0_delta as i64,
            xref: [0; MAX_XREF_GROUPS],
            memo: SkelMemo::default(),
        };
        for (d, &b) in s.subj.iter_mut().zip(&cp.base_subj_nums) {
            *d = b as i64;
        }
        s.xref[1] = cp.base_xref_slot1;
        s
    }
}

/// Which counters a subject template has: bit 0 the bracket fraction (slots 0, 1), bit 1 the paren fraction
/// (2, 3), bit 2 the file part (4). Only those are in the numeric stream.
pub(crate) fn subj_mask(tpl: &[u8]) -> u8 {
    use crate::subject_codec::{SLOT_BRACKET_FRAC, SLOT_FILE_PART, SLOT_PAREN_FRAC};
    let mut m = 0;
    for &b in tpl {
        m |= match b {
            SLOT_BRACKET_FRAC => 1,
            SLOT_PAREN_FRAC => 2,
            SLOT_FILE_PART => 4,
            _ => 0,
        };
    }
    m
}

/// Whether subject slot `k` (0..5) is in a template with this mask.
#[inline(always)]
pub(crate) fn has_slot(mask: u8, k: usize) -> bool {
    mask & (1 << (k / 2).min(2)) != 0
}

fn fit_i32(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(0)
}

#[inline(always)]
fn put_delta(v: i64, prev: &mut i64, out: &mut Vec<u8>) {
    write_varint(zigzag_encode(v.wrapping_sub(*prev)), out);
    *prev = v;
}

/// Encoded scratch for one row while checking it.
#[derive(Default)]
struct RowScratch {
    tpl: Vec<u8>,
    skel: Vec<u8>,
    tail: Vec<u8>,
    tok: Vec<u8>,
    check: Vec<u8>,
}

/// How many counters a template with this mask has.
#[inline(always)]
fn counter_count(smask: u8) -> u32 {
    2 * (smask & 1) as u32 + 2 * ((smask >> 1) & 1) as u32 + ((smask >> 2) & 1) as u32
}

#[inline(always)]
fn varint_len(v: u64) -> usize {
    (64 - (v | 1).leading_zeros() as usize).div_ceil(7)
}

/// A u16 id column as version 2 keeps it: (bytes, the section's `encoding_flags`). One value: the palette alone
/// (width 0). At most 256 values: the palette, padded to 8 bytes, then a u8 index per row (width 1). Each row's id
/// its row number (a block where every row brings a new poster or domain): nothing (width 3). Otherwise a u16 per row
/// (width 2). The flags are the width, with the palette's length from bit 8.
fn encode_id_col(vals: &[u16]) -> (Vec<u8>, u32) {
    let wide = || -> Vec<u8> { vals.iter().flat_map(|v| v.to_le_bytes()).collect() };
    if vals.len() > 1 && vals.iter().enumerate().all(|(i, &v)| v as usize == i) {
        return (Vec::new(), ID_COL_ROW);
    }
    let mut pal = vals.to_vec();
    pal.sort_unstable();
    pal.dedup();
    if pal.len() > 256 {
        return (wide(), 2);
    }
    let mut out: Vec<u8> = pal.iter().flat_map(|v| v.to_le_bytes()).collect();
    out.resize(align_up(out.len(), 8), 0);
    let width = if pal.len() == 1 { 0 } else { 1 };
    if width == 1 {
        // narrower than u16s unless the block is tiny
        if out.len() + vals.len() >= 2 * vals.len() {
            return (wide(), 2);
        }
        out.extend(vals.iter().map(|v| pal.binary_search(v).unwrap_or(0) as u8));
    }
    (out, width | ((pal.len() as u32) << 8))
}

pub struct NxvBlockBuilder;

impl NxvBlockBuilder {
    /// Builds a hot block from raw overview lines (no CR/LF). Rows are kept in
    /// article-number order (a stable sort, so equal numbers keep their order).
    pub fn build_hot_block(lines: &[&[u8]]) -> Result<Vec<u8>> {
        Self::build_hot_block_meta(lines, BlockMeta::default())
    }

    /// `build_hot_block`, with the store's fields kept in the section table.
    pub fn build_hot_block_meta(lines: &[&[u8]], meta: BlockMeta) -> Result<Vec<u8>> {
        Self::build_hot_block_version(lines, meta, VERSION)
    }

    /// `build_hot_block_meta` in format `version` ([`VERSION_1`] or [`VERSION_2`]). Version 1 is kept for tests and
    /// for comparing sizes; a store writes [`VERSION`].
    pub fn build_hot_block_version(lines: &[&[u8]], meta: BlockMeta, version: u16) -> Result<Vec<u8>> {
        ensure!(version == VERSION_1 || version == VERSION_2, "can't write .nxv version {version}");
        ensure!(!lines.is_empty(), "a block needs at least one row");
        ensure!(lines.len() <= MAX_BLOCK_ROWS, "{} rows is more than a block holds", lines.len());
        let mut order: Vec<(u64, usize)> = Vec::with_capacity(lines.len());
        for (i, l) in lines.iter().enumerate() {
            ensure!(!l.contains(&b'\n') && !l.contains(&b'\r'), "row {i} has a line break in it");
            let art = art_number(l).with_context(|| format!("row {i} doesn't start with an article number"))?;
            order.push((art, i));
        }
        order.sort_by_key(|&(art, _)| art);

        // one prefix flag for the block: every field 8 there is starts "Xref: "
        let mut any_tail = false;
        let mut all_prefixed = true;
        for l in lines {
            if let Some((_, Some(t))) = split_fields(l) {
                any_tail = true;
                all_prefixed &= t.starts_with(XREF_PREFIX);
            }
        }
        let xref_prefix = any_tail && all_prefixed;

        // version 2: a subject that is one random token, on no other row, is kept per row
        let mut subjects: HashMap<&[u8], u32> = HashMap::new();
        if version >= VERSION_2 {
            for l in lines {
                if let Some(s) = l.split(|&b| b == b'\t').nth(1)
                    && s.len() >= TOKEN_SUBJECT_MIN
                {
                    *subjects.entry(s).or_default() += 1;
                }
            }
        }
        let token_ok = |line: &[u8]| {
            line.split(|&b| b == b'\t').nth(1).is_some_and(|s| subjects.get(s) == Some(&1) && packs_by_alphabet(s))
        };

        let mut dicts: [Dict; NUM_DICTS] = Default::default();
        let mut tail_data = Vec::new();
        let mut tail_offsets: Vec<u32> = Vec::with_capacity(lines.len() + 1);
        let mut refs_pool: Vec<u8> = Vec::new();
        let mut rows: Vec<Row> = Vec::with_capacity(lines.len());
        let mut sc = RowScratch::default();
        let mut index_entries: Vec<(u64, u16)> = Vec::with_capacity(lines.len());
        for (r, &(art, i)) in order.iter().enumerate() {
            let line = lines[i];
            tail_offsets.push(tail_data.len() as u32);
            if let Some(m) = msgid_field(line) {
                index_entries.push((msgid_hash(m), r as u16));
            }
            let tok = token_ok(line);
            let kind =
                match Self::structure(line, art, xref_prefix, tok, &mut sc, &mut tail_data, &mut refs_pool, &mut dicts)
                {
                    Some(s) => Kind::Structured(s),
                    None => {
                        tail_data.truncate(*tail_offsets.last().unwrap() as usize);
                        Kind::Literal(dicts[DICT_LITERAL].intern(line))
                    }
                };
            rows.push(Row { art, kind });
        }
        tail_offsets.push(tail_data.len() as u32);

        Self::assemble(rows, dicts, tail_offsets, tail_data, refs_pool, index_entries, xref_prefix, meta, version)
    }

    /// Takes a line apart; None when some field won't write back as it was. `token`: keep the subject per row when it
    /// has no counters (the caller checked it is a token no other row has).
    #[allow(clippy::too_many_arguments)]
    fn structure(
        line: &[u8],
        art: u64,
        xref_prefix: bool,
        token: bool,
        sc: &mut RowScratch,
        tail_data: &mut Vec<u8>,
        refs_pool: &mut Vec<u8>,
        dicts: &mut [Dict; NUM_DICTS],
    ) -> Option<Structured> {
        let (f, tail) = split_fields(line)?;
        parse_canonical_u64(f[0], 19)?;
        let nums = extract_subject_template(f[1], &mut sc.tpl)?;
        let (ts, style) = parse_date(f[3])?;
        let (local, domain) = split_msgid(f[4])?;
        let subj_is_local = local == f[1];
        // a template that is the whole subject has no counters
        let tok = token && !subj_is_local && sc.tpl.as_slice() == f[1];
        sc.tok.clear();
        let (nums, style) = if subj_is_local {
            sc.tpl.clear();
            (SubjectNums::default(), style | STYLE_SUBJ_IS_LOCAL)
        } else if tok {
            sc.tpl.clear();
            pack_msgid_tail(f[1], &mut sc.tok);
            (SubjectNums::default(), style)
        } else {
            (nums, style)
        };
        let (pfx, rest) = split_prefix(local);
        let bytes = if f[6].is_empty() { None } else { Some(parse_canonical_i64(f[6])?) };
        let lines = if f[7].is_empty() { None } else { Some(parse_canonical_i64(f[7])?) };
        let style =
            style | if bytes.is_none() { STYLE_NO_BYTES } else { 0 } | if lines.is_none() { STYLE_NO_LINES } else { 0 };
        let mut xref = [0u64; MAX_XREF_GROUPS];
        sc.skel.clear();
        let nx = match tail {
            None => {
                sc.skel.push(0);
                0
            }
            Some(t) => {
                let body = if xref_prefix { &t[XREF_PREFIX.len()..] } else { t };
                let mut tmp = std::mem::take(&mut sc.check);
                let n = extract_xref_skeleton(body, &mut tmp, &mut xref);
                sc.skel.push(1);
                sc.skel.extend_from_slice(&tmp);
                sc.check = tmp;
                n?
            }
        };
        // xref slot 0 is taken against the article number: both must be i64
        if xref.iter().take(nx).any(|&x| x > i64::MAX as u64) {
            return None;
        }
        sc.tail.clear();
        pack_msgid_tail(rest, &mut sc.tail);
        let mut date = [0u8; DATE_BUF];
        let dn = format_date(ts, style & !STYLE_FREE_BITS, &mut date);
        sc.check.clear();
        render_row(
            &RowParts {
                art,
                subj_is_local,
                subj_packed: tok.then_some(sc.tok.as_slice()),
                tpl: &sc.tpl,
                nums,
                author: f[2],
                date: &date[..dn],
                pfx,
                tail_packed: &sc.tail,
                domain,
                refs: f[5],
                bytes,
                lines,
                skel: &sc.skel,
                xref_prefix,
                xref: &xref[..nx],
            },
            &mut sc.check,
        );
        if sc.check != line {
            return None;
        }
        tail_data.extend_from_slice(&sc.tail);
        tail_data.extend_from_slice(&sc.tok);
        let refs_start = refs_pool.len() as u32;
        refs_pool.extend_from_slice(f[5]);
        Some(Structured {
            // a token's id is one past the subject dictionary's last entry, known once every row is in
            tpl: if tok { u32::MAX } else { dicts[DICT_SUBJ].intern(&sc.tpl) },
            skel: dicts[DICT_XREF].intern(&sc.skel),
            author: dicts[DICT_AUTHOR].intern(f[2]),
            domain: dicts[DICT_DOMAIN].intern(domain),
            pfx: dicts[DICT_PFX].intern(pfx),
            pad: nums.pad.0,
            ts,
            style,
            bytes: bytes.unwrap_or(0),
            lines: lines.unwrap_or(0),
            nums: nums.vals,
            xref,
            nx: nx as u8,
            smask: subj_mask(&sc.tpl),
            refs: (refs_start, refs_pool.len() as u32),
            tok,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble(
        rows: Vec<Row>,
        dicts: [Dict; NUM_DICTS],
        tail_offsets: Vec<u32>,
        tail_data: Vec<u8>,
        refs_pool: Vec<u8>,
        index_entries: Vec<(u64, u16)>,
        xref_prefix: bool,
        meta: BlockMeta,
        version: u16,
    ) -> Result<Vec<u8>> {
        let v2 = version >= VERSION_2;
        let n = rows.len();
        // ---- numeric stream, references and checkpoints
        let mut num = Vec::with_capacity(n * 12);
        let mut refs = Vec::with_capacity(refs_pool.len() + n);
        let mut cps: Vec<SkipCheckpoint> = Vec::with_capacity(n.div_ceil(SUB_BLOCK_ROWS));
        let (mut min_ts, mut max_ts) = (i64::MAX, i64::MIN);
        for sb in rows.chunks(SUB_BLOCK_ROWS) {
            let mut cp = SkipCheckpoint {
                base_art_num: sb[0].art as i64,
                varint_byte_offset: u32::try_from(num.len()).context("block too large")?,
                refs_byte_offset: u32::try_from(refs.len()).context("block too large")?,
                ..Default::default()
            };
            if let Some(s) = sb.iter().find_map(|r| match &r.kind {
                Kind::Structured(s) => Some(s),
                Kind::Literal(_) => None,
            }) {
                cp.base_timestamp = s.ts;
                cp.base_bytes = fit_i32(s.bytes);
                cp.base_lines = fit_i32(s.lines);
                // (an empty field's 0 is a fine base too: the state only moves on rows that have one)
                for (b, &v) in cp.base_subj_nums.iter_mut().zip(&s.nums) {
                    *b = fit_i32(v as i64);
                }
                if s.nx > 0 {
                    let art = sb[0].art as i64;
                    cp.base_xref_slot0_delta = fit_i32((s.xref[0] as i64).wrapping_sub(art));
                }
                if s.nx > 1 {
                    cp.base_xref_slot1 = s.xref[1] as i64;
                }
            }
            ensure!(num.len() < CP_OFFSET_MASK as usize, "block too large");
            // version 2: flags for sub-blocks whose article numbers run one by one, or whose Xref slot 0 is always
            // the article number (neither is then stored)
            let consecutive = v2 && sb.iter().enumerate().all(|(i, r)| r.art == sb[0].art + i as u64);
            let slot0_is_art = v2
                && sb.iter().all(|r| match &r.kind {
                    Kind::Structured(s) => s.nx == 0 || s.xref[0] == r.art,
                    Kind::Literal(_) => true,
                });
            if consecutive {
                cp.varint_byte_offset |= CP_CONSECUTIVE;
            }
            if slot0_is_art {
                cp.varint_byte_offset |= CP_SLOT0_IS_ART;
            }
            let mut st = DeltaState::from_checkpoint(&cp);
            // version 2 stores nothing for a sub-block without references: its refs offset equals the next one's
            let refs_here = !v2 || sb.iter().any(|r| matches!(&r.kind, Kind::Structured(s) if s.refs.1 > s.refs.0));
            for r in sb {
                let art = r.art as i64;
                if consecutive {
                    st.art = art;
                } else {
                    put_delta(art, &mut st.art, &mut num);
                }
                let Kind::Structured(s) = &r.kind else { continue };
                min_ts = min_ts.min(s.ts);
                max_ts = max_ts.max(s.ts);
                let changed = s.style != st.style;
                let d = zigzag_encode(s.ts.wrapping_sub(st.ts));
                write_varint((d << 1) | changed as u64, &mut num);
                st.ts = s.ts;
                if changed {
                    write_varint(s.style as u64, &mut num);
                    st.style = s.style;
                }
                if s.style & STYLE_NO_BYTES == 0 {
                    put_delta(s.bytes, &mut st.bytes, &mut num);
                }
                if s.style & STYLE_NO_LINES == 0 {
                    put_delta(s.lines, &mut st.lines, &mut num);
                }
                if v2 && counter_count(s.smask) >= MASKED_COUNTERS {
                    // which counters moved, then only those
                    let mut moved = 0u64;
                    for k in 0..5 {
                        if has_slot(s.smask, k) && s.nums[k] as i64 != st.subj[k] {
                            moved |= 1 << k;
                        }
                    }
                    write_varint(moved, &mut num);
                    for k in 0..5 {
                        if moved & (1 << k) != 0 {
                            put_delta(s.nums[k] as i64, &mut st.subj[k], &mut num);
                        }
                    }
                } else {
                    for k in 0..5 {
                        if has_slot(s.smask, k) {
                            put_delta(s.nums[k] as i64, &mut st.subj[k], &mut num);
                        }
                    }
                }
                let nx = s.nx as usize;
                if nx > 0 {
                    let bases = st.xref_bases(s.skel);
                    let mut vals = [0i64; MAX_XREF_GROUPS];
                    vals[0] = (s.xref[0] as i64).wrapping_sub(art);
                    for (v, &x) in vals.iter_mut().zip(&s.xref).take(nx).skip(1) {
                        *v = x as i64;
                    }
                    // slot 0 less the article number is 0 throughout such a sub-block
                    for k in slot0_is_art as usize..nx {
                        write_varint(zigzag_encode(vals[k].wrapping_sub(bases[k])), &mut num);
                    }
                    st.xref_done(s.skel, &vals, nx);
                }
                if refs_here {
                    let body = &refs_pool[s.refs.0 as usize..s.refs.1 as usize];
                    write_varint(body.len() as u64, &mut refs);
                    refs.extend_from_slice(body);
                }
            }
            cps.push(cp);
        }
        if min_ts > max_ts {
            (min_ts, max_ts) = (0, 0);
        }

        // ---- id columns: (bytes, encoding flags)
        let col = |f: &dyn Fn(&Row) -> u16| -> (Vec<u8>, u32) {
            let vals: Vec<u16> = rows.iter().map(f).collect();
            if v2 { encode_id_col(&vals) } else { (vals.iter().flat_map(|v| v.to_le_bytes()).collect(), 0) }
        };
        let sid = |pick: fn(&Structured) -> u32| {
            move |r: &Row| match &r.kind {
                Kind::Structured(s) => pick(s) as u16,
                Kind::Literal(_) => 0,
            }
        };
        // a per-row token subject: one past the last template (there are at most 65,535 when any row has a token)
        let token_tpl = dicts[DICT_SUBJ].entries.len() as u16;
        let tpl_col = col(&|r| match &r.kind {
            Kind::Structured(s) if s.tok => token_tpl,
            Kind::Structured(s) => s.tpl as u16,
            Kind::Literal(id) => *id as u16,
        });
        let pad_col = col(&|r| match &r.kind {
            Kind::Structured(s) => s.pad,
            Kind::Literal(_) => PADMASK_LITERAL,
        });
        let skel_col = col(&sid(|s| s.skel));
        let author_col = col(&sid(|s| s.author));
        let domain_col = col(&sid(|s| s.domain));
        let pfx_col = col(&sid(|s| s.pfx));

        // ---- dictionaries: FSST where it pays
        let mut fsst_sec = vec![0u8; FSST_DIR_LEN];
        let mut dir = FsstDirectory::default();
        let mut pool = Vec::new();
        let mut pool_offsets: Vec<u32> = Vec::new();
        let mut entry_index = 0u32;
        for (k, d) in dicts.iter().enumerate() {
            let refs: Vec<&[u8]> = d.entries.iter().map(|e| e.as_slice()).collect();
            let raw_len: usize = refs.iter().map(|e| e.len()).sum();
            let comp = fsst::Compressor::train(&refs);
            let coded: Vec<Vec<u8>> = refs.iter().map(|e| comp.compress(e)).collect();
            let ns = comp.n_symbols();
            let mut fsst_len: usize = coded.iter().map(|c| c.len()).sum::<usize>() + ns * 9 + 8;
            // version 2: a raw or FSST entry carries its length (a packed one delimits itself)
            let mut raw_len = raw_len;
            if v2 {
                raw_len += refs.iter().map(|e| varint_len(e.len() as u64)).sum::<usize>();
                fsst_len += coded.iter().map(|c| varint_len(c.len() as u64)).sum::<usize>();
            }
            let packed: Vec<Vec<u8>> = refs
                .iter()
                .map(|e| {
                    let mut p = Vec::with_capacity(e.len() + 2);
                    pack_msgid_tail(e, &mut p);
                    p
                })
                .collect();
            let packed_len: usize = packed.iter().map(|p| p.len()).sum();
            let desc = &mut dir.dicts[k];
            desc.first_entry = entry_index;
            desc.entry_count = d.entries.len() as u32;
            let use_fsst = ns > 0 && fsst_len < raw_len && fsst_len <= packed_len;
            let best_len = if use_fsst { fsst_len } else { packed_len.min(raw_len) };
            // version 2: shaped, when smaller than the best of the others and every entry comes back from it
            let shaped = if v2 { shape_dict(&refs) } else { None }.filter(|(table, n, coded)| {
                align_up(table.len(), 8) + 8 + coded.iter().map(|c| c.len()).sum::<usize>() < best_len
                    && coded.iter().zip(&refs).all(|(c, e)| {
                        let mut back = Vec::with_capacity(e.len());
                        unshape_into(c, table, *n, Some(&mut back)) == c.len() && back == *e
                    })
            });
            let chosen: Vec<&[u8]> = if let Some((table, n, coded)) = &shaped {
                desc.mode = DICT_SHAPED;
                desc.n_symbols = *n as u16;
                fsst_sec.resize(align_up(fsst_sec.len(), 8), 0);
                desc.table_offset = fsst_sec.len() as u32;
                fsst_sec.extend_from_slice(table);
                coded.iter().map(|c| c.as_slice()).collect()
            } else if !use_fsst && packed_len < raw_len {
                desc.mode = DICT_PACKED;
                packed.iter().map(|c| c.as_slice()).collect()
            } else if use_fsst {
                desc.mode = DICT_FSST;
                desc.n_symbols = ns as u16;
                fsst_sec.resize(align_up(fsst_sec.len(), 8), 0);
                desc.table_offset = fsst_sec.len() as u32;
                for s in comp.symbol_table() {
                    fsst_sec.extend_from_slice(&s.to_u64().to_le_bytes());
                }
                fsst_sec.extend_from_slice(comp.symbol_lengths());
                coded.iter().map(|c| c.as_slice()).collect()
            } else {
                desc.mode = DICT_RAW;
                refs.clone()
            };
            let self_delimiting = desc.mode == DICT_PACKED || desc.mode == DICT_SHAPED;
            for (i, e) in chosen.into_iter().enumerate() {
                // version 1: every entry's offset; version 2: every 32nd entry's, each dictionary from its first
                if !v2 || i % POOL_CP_ENTRIES == 0 {
                    pool_offsets.push(u32::try_from(pool.len()).context("string pool too large")?);
                }
                if v2 && !self_delimiting {
                    write_varint(e.len() as u64, &mut pool);
                }
                pool.extend_from_slice(e);
            }
            entry_index += d.entries.len() as u32;
        }
        if !v2 {
            pool_offsets.push(u32::try_from(pool.len()).context("string pool too large")?);
        }
        ensure!(pool.len() <= u32::MAX as usize, "string pool too large");
        fsst_sec.resize(align_up(fsst_sec.len(), 8), 0);
        dir.slot_counts_offset = fsst_sec.len() as u32;
        dir.slot_counts_len = dicts[DICT_XREF].entries.len() as u32;
        for e in &dicts[DICT_XREF].entries {
            fsst_sec.push(match e.split_first() {
                Some((&1, skel)) => slot_count(skel) as u8,
                _ => 0,
            });
        }
        dir.subj_masks_offset = fsst_sec.len() as u32;
        dir.subj_masks_len = dicts[DICT_SUBJ].entries.len() as u32;
        for e in &dicts[DICT_SUBJ].entries {
            fsst_sec.push(subj_mask(e));
        }
        fsst_sec.resize(align_up(fsst_sec.len(), 8), 0);
        write_fsst_dir(&mut fsst_sec[..FSST_DIR_LEN], &dir);

        // ---- message-id filter and buckets
        let hashes: Vec<u64> = index_entries.iter().map(|&(h, _)| h).collect();
        let xor8 = Xor8Builder::build(&hashes);
        let buckets = build_bucket_index(&index_entries);

        // ---- layout
        let mut out = vec![0u8; DATA_START];
        let mut table = [SectionEntry::default(); 16];
        let mut place = |out: &mut Vec<u8>, id: SectionId, bytes: &[u8], items: usize, align: usize| {
            out.resize(align_up(out.len(), align), 0);
            table[id as usize] = SectionEntry {
                offset: out.len() as u32,
                byte_len: bytes.len() as u32,
                item_count: items as u32,
                encoding_flags: 0,
            };
            out.extend_from_slice(bytes);
        };
        let cp_bytes: Vec<u8> = cps.iter().flat_map(checkpoint_bytes).collect();
        place(&mut out, SectionId::SkipCheckpoints, &cp_bytes, cps.len(), 64);
        place(&mut out, SectionId::MsgIdXor8, &xor8, hashes.len(), 8);
        out.resize(align_up(out.len(), 64), 0);
        let cold_prefix = out.len();
        place(&mut out, SectionId::FsstTables, &fsst_sec, NUM_DICTS, 64);
        place(&mut out, SectionId::MsgIdBucketIndex, &buckets, index_entries.len(), 8);
        let cols = [
            (SectionId::SubjTplIds, tpl_col),
            (SectionId::SubjPadMasks, pad_col),
            (SectionId::XrefSkelIds, skel_col),
            (SectionId::AuthorIds, author_col),
            (SectionId::DomainIds, domain_col),
            (SectionId::MsgIdPfxIds, pfx_col),
        ];
        for (id, (bytes, _)) in &cols {
            place(&mut out, *id, bytes, n, 8);
        }
        // version 1: an offset per row (and one past the last); version 2: one per sub-block
        let to: Vec<u8> = if v2 {
            tail_offsets[..n].iter().step_by(TAIL_CP_ROWS).flat_map(|o| o.to_le_bytes()).collect()
        } else {
            tail_offsets.iter().flat_map(|o| o.to_le_bytes()).collect()
        };
        place(&mut out, SectionId::MsgIdTailOffsets, &to, to.len() / 4, 8);
        let po: Vec<u8> = pool_offsets.iter().flat_map(|o| o.to_le_bytes()).collect();
        place(&mut out, SectionId::StringPoolOffsets, &po, pool_offsets.len(), 8);
        place(&mut out, SectionId::StringPoolData, &pool, entry_index as usize, 8);
        place(&mut out, SectionId::MsgIdTailData, &tail_data, n, 8);
        place(&mut out, SectionId::NumericVarints, &num, n, 8);
        place(&mut out, SectionId::ReferencesData, &refs, n, 8);
        out.resize(align_up(out.len(), 64), 0);
        // the hash of everything after the section table (checkpoints, filter and body) goes in the section
        // table, which the header CRC covers: two blocks with the same layout but different contents then have
        // different CRCs, and a reader can check every byte of a block against it
        let body_hash = xxhash_rust::xxh3::xxh3_64(&out[DATA_START..]);
        table[BODY_HASH_LO as usize].encoding_flags = body_hash as u32;
        table[BODY_HASH_HI as usize].encoding_flags = (body_hash >> 32) as u32;
        table[META_SLAB_GEN as usize].encoding_flags = meta.slab_gen;
        table[META_SERVER as usize].encoding_flags = meta.server_id as u32;
        for (id, (_, flags)) in &cols {
            table[*id as usize].encoding_flags = *flags;
        }
        let total = u32::try_from(out.len()).context("block over 4 GiB")?;
        for (i, e) in table.iter().enumerate() {
            let at = HEADER_LEN + i * 16;
            out[at..at + 4].copy_from_slice(&e.offset.to_le_bytes());
            out[at + 4..at + 8].copy_from_slice(&e.byte_len.to_le_bytes());
            out[at + 8..at + 12].copy_from_slice(&e.item_count.to_le_bytes());
            out[at + 12..at + 16].copy_from_slice(&e.encoding_flags.to_le_bytes());
        }
        let header = NxvBlockHeader {
            magic: MAGIC,
            version,
            flags: if xref_prefix { FLAG_XREF_PREFIX } else { 0 },
            min_art_num: rows[0].art as i64,
            max_art_num: rows[n - 1].art as i64,
            min_timestamp: min_ts,
            max_timestamp: max_ts,
            row_count: n as u32,
            checkpoint_count: cps.len() as u32,
            total_block_bytes: total,
            header_crc32: 0,
            cold_prefix_bytes: cold_prefix as u32,
            hot_block_bytes: total,
        };
        write_header(&mut out, &header);
        seal_header(&mut out);
        Ok(out)
    }
}

fn checkpoint_bytes(c: &SkipCheckpoint) -> Vec<u8> {
    let mut b = Vec::with_capacity(64);
    b.extend_from_slice(&c.base_art_num.to_le_bytes());
    b.extend_from_slice(&c.base_timestamp.to_le_bytes());
    b.extend_from_slice(&c.base_xref_slot1.to_le_bytes());
    b.extend_from_slice(&c.base_bytes.to_le_bytes());
    b.extend_from_slice(&c.base_lines.to_le_bytes());
    for v in c.base_subj_nums {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(&c.base_xref_slot0_delta.to_le_bytes());
    b.extend_from_slice(&c.varint_byte_offset.to_le_bytes());
    b.extend_from_slice(&c.refs_byte_offset.to_le_bytes());
    debug_assert_eq!(b.len(), 64);
    b
}

fn write_fsst_dir(out: &mut [u8], d: &FsstDirectory) {
    for (i, e) in d.dicts.iter().enumerate() {
        let at = i * 16;
        out[at..at + 4].copy_from_slice(&e.first_entry.to_le_bytes());
        out[at + 4..at + 8].copy_from_slice(&e.entry_count.to_le_bytes());
        out[at + 8..at + 12].copy_from_slice(&e.table_offset.to_le_bytes());
        out[at + 12..at + 14].copy_from_slice(&e.n_symbols.to_le_bytes());
        out[at + 14..at + 16].copy_from_slice(&e.mode.to_le_bytes());
    }
    out[128..132].copy_from_slice(&d.slot_counts_offset.to_le_bytes());
    out[132..136].copy_from_slice(&d.slot_counts_len.to_le_bytes());
    out[136..140].copy_from_slice(&d.subj_masks_offset.to_le_bytes());
    out[140..144].copy_from_slice(&d.subj_masks_len.to_le_bytes());
}

// ---------------------------------------------------------------- cold tier

/// Zstd-compresses everything after the cold prefix. The header, sections
/// table, checkpoints and message-id filter stay as they are at the front, so
/// range and `OVER <message-id>` routing still read them in place.
pub fn compress_block_to_cold(hot_block: &[u8], zstd_level: i32) -> Result<Vec<u8>> {
    let aligned = AlignedBytes::from_slice(hot_block);
    let view = NxvBlockView::from_mmap_slice(&aligned)?;
    ensure!(!view.is_cold(), "block is already cold");
    let h = *view.header;
    let prefix = h.cold_prefix_bytes as usize;
    let hot_len = h.hot_block_bytes as usize;
    let body = hot_block.get(prefix..hot_len).context("hot block cut short")?;
    let frame = zstd::bulk::compress(body, zstd_level)?;
    let mut out = Vec::with_capacity(prefix + 8 + frame.len() + 64);
    out.extend_from_slice(&hot_block[..prefix]);
    out.extend_from_slice(&(frame.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&frame);
    out.resize(align_up(out.len(), 64), 0);
    let mut nh = h;
    nh.flags |= FLAG_COLD;
    nh.total_block_bytes = u32::try_from(out.len())?;
    write_header(&mut out, &nh);
    seal_header(&mut out);
    Ok(out)
}

/// The hot block a cold one was made from.
pub fn decompress_cold_block(cold: &[u8]) -> Result<AlignedBytes> {
    let aligned = AlignedBytes::from_slice(cold);
    let view = NxvBlockView::from_mmap_slice(&aligned)?;
    ensure!(view.is_cold(), "block is not cold");
    let h = *view.header;
    let prefix = h.cold_prefix_bytes as usize;
    let flen = u32::from_le_bytes(cold.get(prefix..prefix + 4).context("cold block cut short")?.try_into()?) as usize;
    let frame = cold.get(prefix + 8..prefix + 8 + flen).context("cold block cut short")?;
    let body = zstd::bulk::decompress(frame, h.hot_block_bytes as usize - prefix)?;
    ensure!(body.len() == h.hot_block_bytes as usize - prefix, "cold block decompressed to the wrong length");
    ensure!(
        {
            let mut h = xxhash_rust::xxh3::Xxh3::new();
            h.update(&cold[DATA_START..prefix]);
            h.update(&body);
            h.digest()
        } == stored_body_hash(view.sections),
        "cold block body does not match the hash in its header"
    );
    let mut hot = Vec::with_capacity(h.hot_block_bytes as usize);
    hot.extend_from_slice(&cold[..prefix]);
    hot.extend_from_slice(&body);
    let mut nh = h;
    nh.flags &= !FLAG_COLD;
    nh.total_block_bytes = nh.hot_block_bytes;
    write_header(&mut hot, &nh);
    seal_header(&mut hot);
    Ok(AlignedBytes::from_slice(&hot))
}

// ---------------------------------------------------------------- reading

/// Version 2 id column width: each row's id is its row number, and nothing is stored.
const ID_COL_ROW: u32 = 3;

/// A u16 id column read in place: a u16 per row (width 2, and every version-1 column), a palette and a u8 index per
/// row (width 1), one value for every row (width 0), or the row number (width 3). See [`encode_id_col`].
#[derive(Copy, Clone)]
pub(crate) struct IdCol<'a> {
    width: u8,
    rows: usize,
    palette: &'a [u16],
    narrow: &'a [u8],
    wide: &'a [u16],
}

impl<'a> IdCol<'a> {
    fn from_section(sec: &'a [u8], flags: u32, rows: usize, version: u16) -> Option<Self> {
        let width = if version >= VERSION_2 { (flags & 0xFF) as u8 } else { 2 };
        let pal = (flags >> 8) as usize;
        let mut c = Self { width, rows, palette: &[], narrow: &[], wide: &[] };
        match width {
            2 => {
                c.wide = cast_slice::<u16>(sec)?;
                (c.wide.len() == rows).then_some(c)
            }
            0 | 1 => {
                if !(1..=256).contains(&pal) || (width == 0 && pal != 1) {
                    return None;
                }
                c.palette = cast_slice::<u16>(sec.get(..2 * pal)?)?;
                if width == 1 {
                    let at = align_up(2 * pal, 8);
                    c.narrow = sec.get(at..at + rows)?;
                }
                Some(c)
            }
            3 => (pal == 0 && rows <= MAX_BLOCK_ROWS).then_some(c),
            _ => None,
        }
    }

    /// Row `row`'s value; None past the last row (or for an index past the palette, in a damaged block).
    #[inline(always)]
    pub fn get(&self, row: usize) -> Option<u16> {
        match self.width {
            2 => self.wide.get(row).copied(),
            1 => self.palette.get(*self.narrow.get(row)? as usize).copied(),
            3 => (row < self.rows).then_some(row as u16),
            _ => (row < self.rows).then(|| self.palette[0]),
        }
    }

    /// Row `row`'s value, 0 if there is none.
    #[inline(always)]
    pub fn at(&self, row: usize) -> u16 {
        self.get(row).unwrap_or(0)
    }
}

/// Where the next entry of a dictionary starts, after the last one a reader decoded (version 2): a run of rows with
/// new entries in a row then reads each from where the one before ended, without walking from a checkpoint.
#[derive(Copy, Clone, Debug)]
pub(crate) struct PoolHint {
    next_id: u32,
    next_at: u32,
}

impl Default for PoolHint {
    fn default() -> Self {
        Self { next_id: u32::MAX, next_at: 0 }
    }
}

/// The parts of a hot block a row read needs.
#[derive(Copy, Clone)]
pub(crate) struct HotSections<'a> {
    pub version: u16,
    pub fsst_sec: &'a [u8],
    pub dicts: &'a [DictDesc],
    pub slot_counts: &'a [u8],
    pub subj_masks: &'a [u8],
    pub bucket: BucketIndexView<'a>,
    pub subj_tpl: IdCol<'a>,
    pub pad: IdCol<'a>,
    pub skel: IdCol<'a>,
    pub author: IdCol<'a>,
    pub domain: IdCol<'a>,
    pub pfx: IdCol<'a>,
    /// version 1: each row's tail offset, and one past the last; version 2: every 32nd row's segment
    pub tail_off: &'a [u32],
    /// version 1: each entry's offset, and one past the last; version 2: every 32nd entry's, dictionary by
    /// dictionary, dictionary `k`'s from `pool_cp_base[k]`
    pub pool_off: &'a [u32],
    pub pool_cp_base: [u32; NUM_DICTS],
    /// the subject template id of a per-row token subject (u32::MAX in version 1, which has none)
    pub token_tpl: u32,
    pub pool: &'a [u8],
    pub tail: &'a [u8],
    pub num: &'a [u8],
    pub refs: &'a [u8],
}

/// A block read in place: from an mmap'd file, or an [`AlignedBytes`].
#[derive(Copy, Clone)]
pub struct NxvBlockView<'a> {
    pub data: &'a [u8],
    pub header: &'a NxvBlockHeader,
    pub sections: &'a [SectionEntry],
    pub checkpoints: &'a [SkipCheckpoint],
    pub(crate) xor8: Xor8View<'a>,
    pub(crate) hot: Option<HotSections<'a>>,
}

impl<'a> NxvBlockView<'a> {
    /// `slice` must start 64-byte aligned (an mmap at a block boundary, or an
    /// [`AlignedBytes`]); it may run past the block (the rest of a file).
    pub fn from_mmap_slice(slice: &'a [u8]) -> Result<Self> {
        ensure!((slice.as_ptr() as usize).is_multiple_of(64), "block slice is not 64-byte aligned");
        ensure!(slice.len() >= DATA_START, "block slice shorter than a header");
        let header: &NxvBlockHeader = &cast_slice::<NxvBlockHeader>(&slice[..HEADER_LEN]).context("header cast")?[0];
        ensure!(header.magic == MAGIC, "not an .nxv block");
        let version = header.version;
        ensure!(version == VERSION_1 || version == VERSION_2, "unsupported .nxv version {version}");
        ensure!(header_crc(slice) == header.header_crc32, "block header checksum mismatch");
        let total = header.total_block_bytes as usize;
        ensure!(total <= slice.len() && total.is_multiple_of(64), "block cut short");
        ensure!(total >= DATA_START, "block shorter than its header");
        ensure!(header.hot_block_bytes as usize <= MAX_HOT_BLOCK_BYTES, "block claims an implausible size");
        let data = &slice[..total];
        let sections = cast_slice::<SectionEntry>(&data[HEADER_LEN..DATA_START]).context("section table cast")?;
        let rows = header.row_count as usize;
        ensure!((1..=MAX_BLOCK_ROWS).contains(&rows), "bad row count {rows}");
        let cold = header.flags & FLAG_COLD != 0;
        let prefix = header.cold_prefix_bytes as usize;
        ensure!(prefix >= DATA_START && prefix <= total && prefix.is_multiple_of(64), "bad cold prefix");
        if cold {
            ensure!(prefix <= header.hot_block_bytes as usize, "cold prefix past the hot block");
        } else {
            ensure!(header.hot_block_bytes as usize == total, "hot block size mismatch");
        }
        let sec = |id: SectionId| -> Result<&'a [u8]> {
            let e = sections[id as usize];
            let (o, l) = (e.offset as usize, e.byte_len as usize);
            let limit = if cold { prefix } else { total };
            ensure!(o % 8 == 0 && o >= DATA_START && o + l <= limit, "section {id:?} out of bounds");
            Ok(&data[o..o + l])
        };
        let checkpoints = cast_slice::<SkipCheckpoint>(sec(SectionId::SkipCheckpoints)?).context("checkpoint cast")?;
        ensure!(checkpoints.len() == rows.div_ceil(SUB_BLOCK_ROWS), "checkpoint count mismatch");
        let xor8 = Xor8View::from_section(sec(SectionId::MsgIdXor8)?).context("bad message-id filter")?;
        let hot = if cold {
            None
        } else {
            let u16col = |id: SectionId| -> Result<IdCol<'a>> {
                IdCol::from_section(sec(id)?, sections[id as usize].encoding_flags, rows, version)
                    .with_context(|| format!("{id:?} is not a column of {rows} rows"))
            };
            let fsst_sec = sec(SectionId::FsstTables)?;
            let dir: &FsstDirectory =
                &cast_slice::<FsstDirectory>(fsst_sec.get(..FSST_DIR_LEN).context("short dictionary directory")?)
                    .context("directory cast")?[0];
            let slot_counts = fsst_sec
                .get(dir.slot_counts_offset as usize..dir.slot_counts_offset as usize + dir.slot_counts_len as usize)
                .context("slot counts out of bounds")?;
            let subj_masks = fsst_sec
                .get(dir.subj_masks_offset as usize..dir.subj_masks_offset as usize + dir.subj_masks_len as usize)
                .context("subject masks out of bounds")?;
            let dicts = &dir.dicts[..NUM_DICTS];
            let tail_off = cast_slice::<u32>(sec(SectionId::MsgIdTailOffsets)?).context("tail offsets cast")?;
            let want = if version >= VERSION_2 { rows.div_ceil(TAIL_CP_ROWS) } else { rows + 1 };
            ensure!(tail_off.len() == want, "tail offsets count mismatch");
            let pool_off = cast_slice::<u32>(sec(SectionId::StringPoolOffsets)?).context("pool offsets cast")?;
            let mut pool_cp_base = [0u32; NUM_DICTS];
            if version >= VERSION_2 {
                let mut cps = 0u64;
                for (b, d) in pool_cp_base.iter_mut().zip(dicts) {
                    *b = cps as u32;
                    cps += d.entry_count.div_ceil(POOL_CP_ENTRIES as u32) as u64;
                }
                ensure!(pool_off.len() as u64 == cps, "pool checkpoint count mismatch");
            } else {
                ensure!(!pool_off.is_empty(), "empty pool offsets");
            }
            let bucket_e = sections[SectionId::MsgIdBucketIndex as usize];
            let bucket = BucketIndexView::from_section(sec(SectionId::MsgIdBucketIndex)?, bucket_e.item_count as usize)
                .context("bad message-id buckets")?;
            Some(HotSections {
                version,
                pool_cp_base,
                token_tpl: if version >= VERSION_2 { dicts[DICT_SUBJ].entry_count } else { u32::MAX },
                fsst_sec,
                dicts,
                slot_counts,
                subj_masks,
                bucket,
                subj_tpl: u16col(SectionId::SubjTplIds)?,
                pad: u16col(SectionId::SubjPadMasks)?,
                skel: u16col(SectionId::XrefSkelIds)?,
                author: u16col(SectionId::AuthorIds)?,
                domain: u16col(SectionId::DomainIds)?,
                pfx: u16col(SectionId::MsgIdPfxIds)?,
                tail_off,
                pool_off,
                pool: sec(SectionId::StringPoolData)?,
                tail: sec(SectionId::MsgIdTailData)?,
                num: sec(SectionId::NumericVarints)?,
                refs: sec(SectionId::ReferencesData)?,
            })
        };
        Ok(Self { data, header, sections, checkpoints, xor8, hot })
    }

    /// The store's fields (server, slab), from the section table.
    #[inline]
    pub fn store_meta(&self) -> BlockMeta {
        BlockMeta {
            server_id: self.sections[META_SERVER as usize].encoding_flags as u16,
            slab_gen: self.sections[META_SLAB_GEN as usize].encoding_flags,
        }
    }

    /// How many posters the block has (its author dictionary's size); 0 for a cold block.
    #[inline]
    pub fn poster_count(&self) -> u32 {
        self.hot.map_or(0, |h| h.dicts[DICT_AUTHOR].entry_count)
    }

    /// The xxh3 hash of the hot body, from the section table.
    #[inline]
    pub fn body_hash(&self) -> u64 {
        stored_body_hash(self.sections)
    }

    /// Whether a hot block's body still matches its hash. A cold block's body is checked when it is
    /// decompressed, soo this is true for one.
    pub fn verify_body(&self) -> bool {
        if self.is_cold() {
            return true;
        }
        xxhash_rust::xxh3::xxh3_64(&self.data[DATA_START..]) == self.body_hash()
    }

    #[inline]
    pub fn is_cold(&self) -> bool {
        self.header.flags & FLAG_COLD != 0
    }
    #[inline]
    pub fn row_count(&self) -> usize {
        self.header.row_count as usize
    }
    #[inline]
    pub fn xref_prefix(&self) -> bool {
        self.header.flags & FLAG_XREF_PREFIX != 0
    }
    /// Whether the block may hold rows numbered in `min..=max`.
    #[inline]
    pub fn overlaps_art_range(&self, min: u64, max: u64) -> bool {
        (self.header.min_art_num as u64) <= max && min <= self.header.max_art_num as u64
    }
    #[inline]
    pub fn may_contain_msgid(&self, raw_msgid: &[u8]) -> bool {
        self.xor8.contains_hash(msgid_hash(raw_msgid))
    }
    #[inline]
    pub fn may_contain_msgid_hash(&self, hash: u64) -> bool {
        self.xor8.contains_hash(hash)
    }

    pub(crate) fn hot(&self) -> Result<&HotSections<'a>> {
        self.hot.as_ref().context("cold block: decompress it first (decompress_cold_block)")
    }

    /// The first row numbered at least `art` (row_count if none), by the
    /// checkpoints and then at most 127 steps.
    pub fn lower_bound_row(&self, art: u64) -> Result<usize> {
        let h = self.hot()?;
        let cps = self.checkpoints;
        // the sub-block before the first one starting at art or above (equal numbers can straddle a checkpoint)
        let sb = cps.partition_point(|c| (c.base_art_num as u64) < art).saturating_sub(1);
        let mut cur = RowDecoder::at_sub_block(self, sb);
        let end = self.row_count();
        while cur.row < end {
            let d = cur.advance(self, h);
            if d.art >= art {
                return Ok(cur.row - 1);
            }
        }
        Ok(end)
    }
}

impl HotSections<'_> {
    /// Version 2: the entry starting at `at` of a dictionary in `mode`: (its stored bytes' start, their end, where the
    /// next entry starts). A packed entry is stored with its header; a raw or FSST one after its varint length.
    #[inline(always)]
    fn pool_entry_at(&self, at: usize, d: &DictDesc) -> (usize, usize) {
        let pool = self.pool;
        let rest = pool.get(at..).unwrap_or(&[]);
        match d.mode {
            DICT_PACKED => (at, at.saturating_add(packed_tail_len(rest)).min(pool.len())),
            DICT_SHAPED => (at, at + unshape_into(rest, self.shape_table(d), d.n_symbols as usize, None)),
            _ => {
                let mut pos = at;
                let len = read_varint_unchecked(pool, &mut pos) as usize;
                (pos, pos.saturating_add(len).min(pool.len()))
            }
        }
    }

    /// A shaped dictionary's shape table (see [`DICT_SHAPED`]), to the end of the FsstTables section.
    #[inline(always)]
    fn shape_table(&self, d: &DictDesc) -> &[u8] {
        self.fsst_sec.get(d.table_offset as usize..).unwrap_or(&[])
    }

    /// Where entry `id` of dictionary `dict` is kept in the pool, as stored. Version 2 walks from the entry's
    /// checkpoint (at most 31 entries), or from `hint` when that is nearer, and moves `hint` past the entry.
    #[inline]
    fn dict_entry_range(&self, dict: usize, id: u32, hint: &mut PoolHint) -> Option<(usize, usize)> {
        let d = &self.dicts[dict];
        if id >= d.entry_count {
            return None;
        }
        if self.version < VERSION_2 {
            let e = d.first_entry as usize + id as usize;
            return Some((*self.pool_off.get(e)? as usize, *self.pool_off.get(e + 1)? as usize));
        }
        let step = POOL_CP_ENTRIES as u32;
        let (mut at, mut from) = if (id - id % step..=id).contains(&hint.next_id) {
            (hint.next_at as usize, hint.next_id)
        } else {
            (*self.pool_off.get((self.pool_cp_base[dict] + id / step) as usize)? as usize, id - id % step)
        };
        while from < id {
            at = self.pool_entry_at(at, d).1;
            from += 1;
        }
        let (s, e) = self.pool_entry_at(at, d);
        *hint = PoolHint { next_id: id + 1, next_at: e as u32 };
        Some((s, e))
    }

    /// Appends dictionary entry `id` of dictionary `dict` to `out`.
    #[inline]
    pub fn dict_entry_into(&self, dict: usize, id: u32, out: &mut Vec<u8>) {
        self.dict_entry_hinted(dict, id, &mut PoolHint::default(), out)
    }

    /// [`Self::dict_entry_into`], starting from `hint` when it is nearer than the checkpoint (version 2).
    #[inline]
    pub fn dict_entry_hinted(&self, dict: usize, id: u32, hint: &mut PoolHint, out: &mut Vec<u8>) {
        let d = &self.dicts[dict];
        let Some((s, t)) = self.dict_entry_range(dict, id, hint) else { return };
        let Some(coded) = self.pool.get(s..t) else { return };
        match d.mode {
            DICT_FSST => {}
            DICT_PACKED => return crate::msgid_codec::unpack_msgid_tail_into(coded, out),
            DICT_SHAPED => {
                unshape_into(coded, self.shape_table(d), d.n_symbols as usize, Some(out));
                return;
            }
            _ => {
                out.extend_from_slice(coded);
                return;
            }
        }
        let ns = d.n_symbols as usize;
        let to = d.table_offset as usize;
        let (Some(syms), Some(lens)) = (
            self.fsst_sec.get(to..to + 8 * ns).and_then(cast_slice::<u64>),
            self.fsst_sec.get(to + 8 * ns..to + 9 * ns),
        ) else {
            return;
        };
        fsst_decode(coded, syms, lens, out);
    }

    /// Whether structured row `row` keeps its subject per row (a token after its message-id tail).
    #[inline(always)]
    pub fn token_subject(&self, row: usize) -> bool {
        self.subj_tpl.get(row).is_some_and(|t| t as u32 == self.token_tpl)
    }

    /// Version 2: where the segment after row `row`'s starts, given that row's starts at `at`.
    #[inline(always)]
    fn next_segment(&self, row: usize, at: usize) -> usize {
        if self.pad.get(row).is_none_or(|p| p & PADMASK_LITERAL != 0) {
            return at;
        }
        let item = |at: usize| at.saturating_add(packed_tail_len(self.tail.get(at..).unwrap_or(&[])));
        let at = item(at);
        if self.token_subject(row) { item(at) } else { at }
    }

    /// Row `row`'s packed message-id tail: (start, end) in the tail data. Version 2 walks from the nearest tail
    /// checkpoint before it, at most 31 rows.
    pub fn msgid_tail_range(&self, row: usize) -> (usize, usize) {
        if self.version < VERSION_2 {
            let at = |r: usize| self.tail_off.get(r).copied().unwrap_or(0) as usize;
            return (at(row), at(row + 1));
        }
        let cp = row / TAIL_CP_ROWS;
        let mut at = self.tail_off.get(cp).copied().unwrap_or(0) as usize;
        for r in cp * TAIL_CP_ROWS..row {
            at = self.next_segment(r, at);
        }
        (at, at.saturating_add(packed_tail_len(self.tail.get(at..).unwrap_or(&[]))))
    }

    /// (start, end) of dictionary `dict` in the string pool, for size breakdowns.
    fn dict_pool_span(&self, dict: usize) -> (usize, usize) {
        let d = &self.dicts[dict];
        if self.version < VERSION_2 {
            let at = |e: u32| self.pool_off.get(e as usize).copied().unwrap_or(0) as usize;
            return (at(d.first_entry), at(d.first_entry + d.entry_count));
        }
        let first = |k: usize| {
            (self.dicts[k].entry_count > 0)
                .then(|| self.pool_off.get(self.pool_cp_base[k] as usize).copied().unwrap_or(0) as usize)
        };
        let end = (dict + 1..NUM_DICTS).find_map(first).unwrap_or(self.pool.len());
        (first(dict).unwrap_or(end), end)
    }
}

/// FSST decode: code 255 escapes the next byte, any other code is a symbol.
#[inline]
fn fsst_decode(coded: &[u8], syms: &[u64], lens: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < coded.len() {
        let c = coded[i] as usize;
        if c == 255 {
            if let Some(&b) = coded.get(i + 1) {
                out.push(b);
            }
            i += 2;
        } else {
            if let (Some(&s), Some(&l)) = (syms.get(c), lens.get(c)) {
                out.extend_from_slice(&s.to_le_bytes()[..(l as usize).min(8)]);
            }
            i += 1;
        }
    }
}

/// One decoded row's numbers.
#[derive(Copy, Clone, Default, Debug)]
pub(crate) struct DecodedRow {
    pub literal: bool,
    pub art: u64,
    pub ts: i64,
    pub style: u32,
    pub bytes: i64,
    pub lines: i64,
    pub nums: [u32; 5],
    pub xref: [u64; MAX_XREF_GROUPS],
    pub nx: usize,
    pub refs: (usize, usize),
    /// the packed message-id tail, in the tail data
    pub tail: (usize, usize),
    /// the packed subject, in the tail data, when the subject is kept per row
    pub subj: Option<(usize, usize)>,
}

/// Walks the numeric, references and (version 2) tail streams one row at a time.
pub(crate) struct RowDecoder {
    pub row: usize,
    st: DeltaState,
    vpos: usize,
    rpos: usize,
    /// version 2: where the next row's segment starts in the tail data
    tpos: usize,
    /// whether this sub-block's structured rows have references stored (a length each)
    has_refs: bool,
    /// version 2 checkpoint flags: no article number deltas, no Xref slot 0 deltas
    consecutive: bool,
    slot0_is_art: bool,
}

impl RowDecoder {
    pub fn at_sub_block(view: &NxvBlockView, sb: usize) -> Self {
        let cp = &view.checkpoints[sb];
        let hot = view.hot.as_ref();
        let flags = if hot.is_some_and(|h| h.version >= VERSION_2) { cp.varint_byte_offset } else { 0 };
        // a sub-block holds references when its stream offset moves before the next one's (or the stream's end)
        let next =
            view.checkpoints.get(sb + 1).map_or(hot.map_or(0, |h| h.refs.len()), |c| c.refs_byte_offset as usize);
        Self {
            row: sb * SUB_BLOCK_ROWS,
            st: DeltaState::from_checkpoint(cp),
            vpos: (cp.varint_byte_offset & CP_OFFSET_MASK) as usize,
            rpos: cp.refs_byte_offset as usize,
            tpos: hot
                .filter(|h| h.version >= VERSION_2)
                .map_or(0, |h| h.tail_off.get(sb * (SUB_BLOCK_ROWS / TAIL_CP_ROWS)).copied().unwrap_or(0) as usize),
            has_refs: next != cp.refs_byte_offset as usize,
            consecutive: flags & CP_CONSECUTIVE != 0,
            slot0_is_art: flags & CP_SLOT0_IS_ART != 0,
        }
    }

    /// Decodes row `self.row` and moves to the next. At a sub-block boundary
    /// the state is reset from the checkpoint by the caller (see `advance`).
    #[inline(always)]
    pub fn step(&mut self, h: &HotSections) -> DecodedRow {
        let num = h.num;
        let r = self.row;
        let st = &mut self.st;
        let mut d = DecodedRow::default();
        if !self.consecutive {
            st.art = st.art.wrapping_add(zigzag_decode(read_varint_unchecked(num, &mut self.vpos)));
        } else if !r.is_multiple_of(SUB_BLOCK_ROWS) {
            st.art = st.art.wrapping_add(1);
        }
        d.art = st.art as u64;
        self.row += 1;
        let pad = h.pad.get(r).unwrap_or(PADMASK_LITERAL);
        if pad & PADMASK_LITERAL != 0 {
            d.literal = true;
            return d;
        }
        let t = read_varint_unchecked(num, &mut self.vpos);
        st.ts = st.ts.wrapping_add(zigzag_decode(t >> 1));
        if t & 1 != 0 {
            st.style = read_varint_unchecked(num, &mut self.vpos) as u32;
        }
        d.ts = st.ts;
        d.style = st.style;
        if st.style & STYLE_NO_BYTES == 0 {
            st.bytes = st.bytes.wrapping_add(zigzag_decode(read_varint_unchecked(num, &mut self.vpos)));
        }
        if st.style & STYLE_NO_LINES == 0 {
            st.lines = st.lines.wrapping_add(zigzag_decode(read_varint_unchecked(num, &mut self.vpos)));
        }
        d.bytes = st.bytes;
        d.lines = st.lines;
        let tpl = h.subj_tpl.at(r) as u32;
        let token = tpl == h.token_tpl;
        let smask = if token { 0 } else { h.subj_masks.get(tpl as usize).copied().unwrap_or(0) };
        let moved = if h.version >= VERSION_2 && counter_count(smask) >= MASKED_COUNTERS {
            read_varint_unchecked(num, &mut self.vpos)
        } else {
            u64::MAX
        };
        for k in 0..5 {
            if has_slot(smask, k) && moved & (1 << k) != 0 {
                st.subj[k] = st.subj[k].wrapping_add(zigzag_decode(read_varint_unchecked(num, &mut self.vpos)));
            }
            d.nums[k] = st.subj[k] as u32;
        }
        let skel = h.skel.at(r) as usize;
        let nx = (h.slot_counts.get(skel).copied().unwrap_or(0) as usize).min(MAX_XREF_GROUPS);
        if nx > 0 {
            let bases = st.xref_bases(skel as u32);
            let mut vals = [0i64; MAX_XREF_GROUPS];
            for k in self.slot0_is_art as usize..nx {
                vals[k] = bases[k].wrapping_add(zigzag_decode(read_varint_unchecked(num, &mut self.vpos)));
            }
            d.xref[0] = st.art.wrapping_add(vals[0]) as u64;
            for (x, &v) in d.xref.iter_mut().zip(&vals).take(nx).skip(1) {
                *x = v as u64;
            }
            st.xref_done(skel as u32, &vals, nx);
        }
        d.nx = nx;
        if self.has_refs {
            let len = read_varint_unchecked(h.refs, &mut self.rpos) as usize;
            let start = self.rpos.min(h.refs.len());
            self.rpos = self.rpos.saturating_add(len).min(h.refs.len());
            d.refs = (start, self.rpos);
        } else {
            d.refs = (self.rpos, self.rpos);
        }
        if h.version >= VERSION_2 {
            let item = |at: usize| (at, at.saturating_add(packed_tail_len(h.tail.get(at..).unwrap_or(&[]))));
            d.tail = item(self.tpos);
            self.tpos = d.tail.1;
            if token {
                let s = item(self.tpos);
                self.tpos = s.1;
                d.subj = Some(s);
            }
        } else {
            let at = |r: usize| h.tail_off.get(r).copied().unwrap_or(0) as usize;
            d.tail = (at(r), at(r + 1));
        }
        d
    }

    /// Decodes the next row, moving onto the next sub-block's checkpoint first
    /// when this one is done.
    #[inline(always)]
    pub fn advance(&mut self, view: &NxvBlockView, h: &HotSections) -> DecodedRow {
        if self.row.is_multiple_of(SUB_BLOCK_ROWS) && self.row > 0 {
            *self = Self::at_sub_block(view, self.row / SUB_BLOCK_ROWS);
        }
        self.step(h)
    }
}

/// Header fields of an untrusted slice, without the alignment requirement.
pub fn peek_header(b: &[u8]) -> Result<NxvBlockHeader> {
    ensure!(b.len() >= DATA_START, "short block");
    ensure!(b[..4] == MAGIC, "not an .nxv block");
    let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let i64_at = |o: usize| i64::from_le_bytes(b[o..o + 8].try_into().unwrap());
    let h = NxvBlockHeader {
        magic: MAGIC,
        version: u16::from_le_bytes([b[4], b[5]]),
        flags: u16::from_le_bytes([b[6], b[7]]),
        min_art_num: i64_at(8),
        max_art_num: i64_at(16),
        min_timestamp: i64_at(24),
        max_timestamp: i64_at(32),
        row_count: u32_at(40),
        checkpoint_count: u32_at(44),
        total_block_bytes: u32_at(48),
        header_crc32: u32_at(52),
        cold_prefix_bytes: u32_at(56),
        hot_block_bytes: u32_at(60),
    };
    if header_crc(b) != h.header_crc32 {
        bail!("block header checksum mismatch");
    }
    Ok(h)
}

/// The message-id field of a raw overview line (its 5th field).
pub fn msgid_field_pub(line: &[u8]) -> Option<&[u8]> {
    msgid_field(line)
}

impl NxvBlockView<'_> {
    /// How many rows are kept whole in the literal dictionary.
    pub fn literal_rows(&self) -> usize {
        self.hot.map_or(0, |h| {
            (0..self.row_count()).filter(|&r| h.pad.get(r).is_some_and(|p| p & PADMASK_LITERAL != 0)).count()
        })
    }
    /// How many rows keep their subject per row (version 2 token subjects).
    pub fn token_subject_rows(&self) -> usize {
        self.hot.map_or(0, |h| {
            (0..self.row_count())
                .filter(|&r| h.token_subject(r) && h.pad.get(r).is_some_and(|p| p & PADMASK_LITERAL == 0))
                .count()
        })
    }
    /// (section, bytes) for each section, for size breakdowns.
    pub fn section_sizes(&self) -> [(SectionId, u32); 16] {
        use SectionId::*;
        let ids = [
            FsstTables,
            SkipCheckpoints,
            MsgIdXor8,
            MsgIdBucketIndex,
            SubjTplIds,
            SubjPadMasks,
            XrefSkelIds,
            AuthorIds,
            DomainIds,
            MsgIdPfxIds,
            MsgIdTailOffsets,
            StringPoolOffsets,
            StringPoolData,
            MsgIdTailData,
            NumericVarints,
            ReferencesData,
        ];
        ids.map(|id| (id, self.sections[id as usize].byte_len))
    }
    /// (entries, string pool bytes, mode) of each dictionary, for size breakdowns.
    pub fn dict_stats(&self) -> [(u32, u32, u16); NUM_DICTS] {
        let mut out = [(0, 0, 0); NUM_DICTS];
        if let Some(h) = self.hot {
            for (k, (o, d)) in out.iter_mut().zip(h.dicts).enumerate() {
                let (a, b) = h.dict_pool_span(k);
                *o = (d.entry_count, b.saturating_sub(a) as u32, d.mode);
            }
        }
        out
    }
    /// Entries in each dictionary (subject, xref, author, domain, prefix, literal).
    pub fn dict_sizes(&self) -> [u32; NUM_DICTS] {
        let mut out = [0; NUM_DICTS];
        if let Some(h) = self.hot {
            for (o, d) in out.iter_mut().zip(h.dicts) {
                *o = d.entry_count;
            }
        }
        out
    }
}

/// Why a line would be kept whole, for benchmarks and diagnostics (None: it is structured).
pub fn literal_reason(line: &[u8]) -> Option<&'static str> {
    let Some((f, tail)) = split_fields(line) else { return Some("fewer than 8 fields") };
    if parse_canonical_u64(f[0], 19).is_none() {
        return Some("article number not canonical");
    }
    let mut tpl = Vec::new();
    if extract_subject_template(f[1], &mut tpl).is_none() {
        return Some("subject holds a placeholder byte");
    }
    if parse_date(f[3]).is_none() {
        return Some("date shape");
    }
    if split_msgid(f[4]).is_none() {
        return Some("message-id shape");
    }
    if !f[6].is_empty() && parse_canonical_i64(f[6]).is_none() {
        return Some("bytes not canonical");
    }
    if !f[7].is_empty() && parse_canonical_i64(f[7]).is_none() {
        return Some("lines not canonical");
    }
    if let Some(t) = tail {
        let mut skel = Vec::new();
        let mut nums = [0u64; MAX_XREF_GROUPS];
        if extract_xref_skeleton(t, &mut skel, &mut nums).is_none() {
            return Some("xref holds a placeholder byte");
        }
    }
    let mut d = [Dict::default(), Dict::default(), Dict::default(), Dict::default(), Dict::default(), Dict::default()];
    let xp = tail.is_some_and(|t| t.starts_with(XREF_PREFIX));
    let art = parse_canonical_u64(f[0], 19).unwrap_or(0);
    let s = NxvBlockBuilder::structure(
        line,
        art,
        xp,
        false,
        &mut RowScratch::default(),
        &mut Vec::new(),
        &mut Vec::new(),
        &mut d,
    );
    if s.is_none() { Some("does not write back the same") } else { None }
}

impl NxvBlockView<'_> {
    /// The first row numbered exactly `art`, if the block has one. None for a cold block.
    pub fn find_row_by_art_num(&self, art: u64) -> Option<usize> {
        if !self.overlaps_art_range(art, art) {
            return None;
        }
        let h = self.hot.as_ref()?;
        let sb = self.checkpoints.partition_point(|c| (c.base_art_num as u64) < art).saturating_sub(1);
        let mut cur = RowDecoder::at_sub_block(self, sb);
        let end = self.row_count();
        while cur.row < end {
            let d = cur.advance(self, h);
            if d.art >= art {
                return (d.art == art).then_some(cur.row - 1);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col_round_trip(vals: &[u16]) -> u32 {
        let (bytes, flags) = encode_id_col(vals);
        let al = AlignedBytes::from_slice(&bytes);
        let c = IdCol::from_section(&al, flags, vals.len(), VERSION_2).expect("column reads back");
        for (i, &v) in vals.iter().enumerate() {
            assert_eq!(c.get(i), Some(v), "row {i}");
        }
        assert_eq!(c.get(vals.len()), None);
        assert_eq!(c.at(vals.len()), 0);
        flags & 0xFF
    }

    #[test]
    fn id_columns_come_back_at_every_width() {
        assert_eq!(col_round_trip(&[7]), 0);
        assert_eq!(col_round_trip(&[PADMASK_LITERAL; 300]), 0);
        let few: Vec<u16> = (0..1000).map(|i| [3u16, 65_535, 0, 900][i % 4]).collect();
        assert_eq!(col_round_trip(&few), 1);
        let palette_full: Vec<u16> = (0..1000u32).map(|i| (i % 256 * 255) as u16).collect();
        assert_eq!(col_round_trip(&palette_full), 1);
        let many: Vec<u16> = (0..1000u32).map(|i| (i % 257) as u16).collect();
        assert_eq!(col_round_trip(&many), 2);
        let rows: Vec<u16> = (0..65_536u32).map(|i| i as u16).collect();
        assert_eq!(col_round_trip(&rows), 3);
        assert_eq!(col_round_trip(&rows[..2]), 3);
        // a palette that doesn't pay for itself in a tiny block
        assert_eq!(col_round_trip(&[1, 2, 3, 4, 5, 6, 7, 9]), 2);
        // version 1 columns are always u16s
        let v1: Vec<u8> = few.iter().flat_map(|v| v.to_le_bytes()).collect();
        let al = AlignedBytes::from_slice(&v1);
        let c = IdCol::from_section(&al, 0, few.len(), VERSION_1).unwrap();
        assert_eq!(c.get(5), Some(few[5]));
        // damaged flags are refused, not trusted
        let (bytes, _) = encode_id_col(&few);
        let al = AlignedBytes::from_slice(&bytes);
        for flags in [4, 1 | (300 << 8), 1, 2 << 8, 3 | (1 << 8)] {
            assert!(IdCol::from_section(&al, flags, few.len(), VERSION_2).is_none(), "{flags:#x}");
        }
        assert!(IdCol::from_section(&al, 1 | (4 << 8), few.len() + 1000, VERSION_2).is_none());
    }

    #[test]
    fn shaped_entries_come_back() {
        let long = "x".repeat(300);
        let entries: Vec<&[u8]> = vec![
            b"Abc123 <def.GH-9@ij.kl>",
            b"",
            b"plain",
            b" <>@ ",
            b"a_b_c",
            long.as_bytes(),
            b"\xff\xfe9\x01",
            b"Abc123 <def.GH-9@ij.kl>",
        ];
        let (table, n, coded) = shape_dict(&entries).unwrap();
        // the first and last entries share a shape, and so do the two that are one run; the empty one's is empty
        assert_eq!(n, 6);
        for (c, e) in coded.iter().zip(&entries) {
            let mut back = Vec::new();
            let mut pool = c.clone();
            pool.extend_from_slice(b"next entry");
            assert_eq!(unshape_into(&pool, &table, n, Some(&mut back)), c.len());
            assert_eq!(unshape_into(&pool, &table, n, None), c.len());
            assert_eq!(back, *e);
        }
        assert!(shape_dict(&[b"a\0b"]).is_none());
        // a shape id past the table, or a cut entry, gives what there is and never reads past the end
        let mut out = Vec::new();
        assert_eq!(unshape_into(&[200, 1], &table, n, Some(&mut out)), 2);
        assert_eq!(unshape_into(&coded[0][..3], &table, n, Some(&mut out)), 3);
        assert_eq!(unshape_into(&[], &table, n, None), 0);
        assert_eq!(shape_of(&table[..3], n, 0), b"");
    }

    #[test]
    fn helpers() {
        for v in [0u64, 1, 127, 128, 16_383, 16_384, u32::MAX as u64, u64::MAX] {
            let mut b = Vec::new();
            write_varint(v, &mut b);
            assert_eq!(varint_len(v), b.len(), "{v}");
        }
        assert_eq!([0u8, 1, 2, 3, 4, 5, 6, 7].map(counter_count), [0, 2, 2, 4, 1, 3, 3, 5]);
    }
}
