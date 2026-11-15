//! Inactive normalized accepted-batch records. These carry no release authority.
use super::{Chunk, Error, Identity, MAX_CHUNK_BYTES, MAX_RANGE_BYTES, PayloadRef, ReadSession, Result, Writer};
use sha2::{Digest, Sha256};
use std::time::Instant;

pub(crate) const MAX_BATCH_ROWS: usize = 16_384;
pub(crate) const MAX_DATA_CHUNKS: usize = 256;
pub(crate) const MAX_METADATA_PAGES: usize = 256;
pub(crate) const MAX_PAGE_BYTES: usize = 32 * 1024;
pub(crate) const MAX_METADATA_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_ROW_BYTES: usize = 16 * 1024;
pub(crate) const MAX_ROW_EXTENTS: usize = 1024;
pub(crate) const MAX_FIELD_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const MAX_LOGICAL_BYTES: u64 = 256 * 1024 * 1024;
pub(crate) const FIELD_COUNT: usize = 14;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReleaseBinding {
    pub(crate) group: String,
    pub(crate) is_collection: bool,
    pub(crate) identity_key: [u8; 32],
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Generation(pub(crate) [u8; 32]);
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BatchRef {
    pub(crate) identity: Identity,
    pub(crate) binding: ReleaseBinding,
    pub(crate) generation: Generation,
    pub(crate) root: PayloadRef,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum FieldTag {
    MessageId = 1,
    Filename,
    Subject,
    Poster,
    PostedDate,
    Name,
    DisplayName,
    OriginalName,
    OriginalFilename,
    TimingHost,
    TimingKey,
    NzbSubject,
    NzbPoster,
    NzbPostedDate,
}
impl FieldTag {
    pub(crate) fn index(self) -> usize {
        self as usize - 1
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Extent {
    pub(crate) chunk_index: u16,
    pub(crate) offset: u32,
    pub(crate) bytes: u32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FieldRef {
    pub(crate) logical_bytes: u64,
    pub(crate) extents: Vec<Extent>,
    // Input references are unbound. Only the decoder can mint a snapshot token.
    token: Option<FieldToken>,
}
impl FieldRef {
    pub(crate) fn new(logical_bytes: u64, extents: Vec<Extent>) -> Result<Self> {
        // Spare capacity is owned workspace too, including present-empty fields.
        if extents.capacity() > MAX_ROW_EXTENTS {
            return Err(Error::Limit);
        }
        validate_field(logical_bytes, &extents)?;
        Ok(Self { logical_bytes, extents, token: None })
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NzbEnvelopeV1 {
    pub(crate) timestamp: i64,
    pub(crate) file_index: Option<i64>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AcceptedRowV1 {
    pub(crate) ordinal: i64,
    pub(crate) filename_key: Option<[u8; 32]>,
    pub(crate) message_key: Option<[u8; 32]>,
    pub(crate) part: Option<i64>,
    pub(crate) expected: Option<i64>,
    pub(crate) file_total: Option<i64>,
    pub(crate) file_index: Option<i64>,
    pub(crate) bytes: i64,
    pub(crate) posted_at: i64,
    pub(crate) zero_file_index: bool,
    pub(crate) posted_clock: Option<i64>,
    pub(crate) t_date: Option<i64>,
    pub(crate) t_utc: Option<i64>,
    pub(crate) t_number: Option<i64>,
    pub(crate) t_lines: Option<i64>,
    pub(crate) t_references: Option<i64>,
    pub(crate) t_xref: Option<i64>,
    pub(crate) t_from: Option<i64>,
    pub(crate) t_encoded: Option<i64>,
    pub(crate) t_tool: Option<i64>,
    pub(crate) t_clock: Option<i64>,
    pub(crate) t_hw_n: Option<i64>,
    pub(crate) original_name_key: [u8; 32],
    pub(crate) is_obfuscated: bool,
    pub(crate) junk: bool,
    pub(crate) nzb: Option<NzbEnvelopeV1>,
    pub(crate) fields: [Option<FieldRef>; FIELD_COUNT],
}
impl Default for AcceptedRowV1 {
    fn default() -> Self {
        Self {
            ordinal: 0,
            filename_key: None,
            message_key: None,
            part: None,
            expected: None,
            file_total: None,
            file_index: None,
            bytes: 0,
            posted_at: 0,
            zero_file_index: false,
            posted_clock: None,
            t_date: None,
            t_utc: None,
            t_number: None,
            t_lines: None,
            t_references: None,
            t_xref: None,
            t_from: None,
            t_encoded: None,
            t_tool: None,
            t_clock: None,
            t_hw_n: None,
            original_name_key: [0; 32],
            is_obfuscated: false,
            junk: false,
            nzb: None,
            fields: std::array::from_fn(|_| None),
        }
    }
}

// Canonical v1 is deliberately fixed, not an extensible map. All integers are LE.
// Root: ATAMF001, reserved:u8=0, identity:[u8;48], group:u16+UTF8,
// collection:bool, identity_key:[u8;32], rows:u32, data_count:u16,
// page_count:u16, data[(digest32,bytes:u32)], pages[(digest32,bytes:u32,rows:u32)].
// Data refs sort strictly by digest; pages/rows retain supplied order.
// Page: ATAMP001, reserved:u8=0, rows:u32, repeated(row_bytes:u32,row).
// Row: ordinal:i64; optional filename/message hashes; optional part/expected/
// file_total/file_index; bytes/posted_at:i64; zero_file_index:bool;
// optional posted_clock, t_date/t_utc/t_number/t_lines/t_references/t_xref/
// t_from/t_encoded/t_tool/t_clock/t_hw_n; original_name_key:[u8;32];
// is_obfuscated/junk:bool; optional NZB(timestamp:i64,optional file_index);
// exactly 14 ordered (FieldTag:u8,optional FieldRef) entries.
// Options/bools use only tags 0/1. FieldRef: logical_bytes:u64, extents:u16,
// repeated(chunk_index:u16,offset:u32,bytes:u32). Absent and present empty differ.
// Child identity is inherited from the root. No paths or tokens are serialized.
// Any incompatible change needs a new magic/version; unknown/trailing bytes fail.
const ROOT_MAGIC: &[u8; 8] = b"ATAMF001";
const PAGE_MAGIC: &[u8; 8] = b"ATAMP001";
const PAGE_HEADER: usize = 13;
const MAX_ROOT_BYTES: usize = 9 + 48 + 2 + 4096 + 1 + 32 + 8 + 256 * 36 + 256 * 40;

fn check(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline { Err(Error::Deadline) } else { Ok(()) }
}
fn validate_field(logical: u64, extents: &[Extent]) -> Result<()> {
    if logical > MAX_FIELD_BYTES || extents.len() > MAX_ROW_EXTENTS {
        return Err(Error::Limit);
    }
    let mut total = 0u64;
    for extent in extents {
        if extent.bytes == 0 {
            return Err(Error::Corrupt);
        }
        extent.offset.checked_add(extent.bytes).ok_or(Error::Limit)?;
        total = total.checked_add(extent.bytes as u64).ok_or(Error::Limit)?;
    }
    if total != logical {
        return Err(Error::Corrupt);
    }
    Ok(())
}
fn validate_extents(field: &FieldRef, data: &[PayloadRef]) -> Result<()> {
    validate_field(field.logical_bytes, &field.extents)?;
    for extent in &field.extents {
        let chunk = data.get(extent.chunk_index as usize).ok_or(Error::Corrupt)?;
        if extent.offset.checked_add(extent.bytes).ok_or(Error::Limit)? > chunk.bytes {
            return Err(Error::Corrupt);
        }
    }
    Ok(())
}
fn binding_digest(binding: &ReleaseBinding) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update((binding.group.len() as u16).to_le_bytes());
    hash.update(binding.group.as_bytes());
    hash.update([u8::from(binding.is_collection)]);
    hash.update(binding.identity_key);
    hash.finalize().into()
}
fn descriptor_digest(field: &FieldRef) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(field.logical_bytes.to_le_bytes());
    hash.update((field.extents.len() as u16).to_le_bytes());
    for extent in &field.extents {
        hash.update(extent.chunk_index.to_le_bytes());
        hash.update(extent.offset.to_le_bytes());
        hash.update(extent.bytes.to_le_bytes());
    }
    hash.finalize().into()
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FieldToken {
    identity: Identity,
    binding: [u8; 32],
    generation: Generation,
    descriptor: [u8; 32],
}
#[derive(Clone, Copy)]
struct PageRef {
    payload: PayloadRef,
    rows: u32,
}

/// One page and one row encoder buffer, even when the accepted batch is large.
/// Including nested publish/verify buffers, owned workspace stays below 256 KiB:
/// page 32 KiB + row 16 KiB + input descriptors <=16 KiB + tables <=44 KiB
/// + binding <=4 KiB + publisher/verification 128 KiB + small fixed state.
pub(crate) struct BatchManifestBuilder<'a> {
    writer: &'a mut Writer,
    binding: ReleaseBinding,
    data: Vec<PayloadRef>,
    remap: Vec<u16>,
    pages: Vec<PageRef>,
    page: Vec<u8>,
    page_rows: u32,
    rows: u32,
    metadata_bytes: usize,
    logical_bytes: u64,
    deadline: Instant,
    failed: bool,
}
impl<'a> BatchManifestBuilder<'a> {
    pub(crate) fn new(
        writer: &'a mut Writer,
        binding: ReleaseBinding,
        data: Vec<PayloadRef>,
        deadline: Instant,
    ) -> Result<Self> {
        check(deadline)?;
        if binding.group.len() > 4096 || binding.group.capacity() > 4096 || data.len() > MAX_DATA_CHUNKS {
            return Err(Error::Limit);
        }
        let identity = writer.identity();
        for reference in &data {
            if reference.identity != identity {
                return Err(Error::IdentityMismatch);
            }
            if reference.bytes > MAX_CHUNK_BYTES {
                return Err(Error::Limit);
            }
        }
        // Do not retain a caller Vec's possibly excessive spare capacity.
        let mut indexed = Vec::with_capacity(data.len());
        for (index, reference) in data.iter().copied().enumerate() {
            indexed.push((reference, index));
        }
        drop(data);
        indexed.sort_unstable_by_key(|(reference, _)| reference.digest);
        let mut data: Vec<PayloadRef> = Vec::with_capacity(indexed.len());
        let mut remap = vec![0; indexed.len()];
        for (reference, original) in indexed {
            if let Some(last) = data.last().filter(|last| last.digest == reference.digest) {
                if last.bytes != reference.bytes {
                    return Err(Error::Corrupt);
                }
            } else {
                data.push(reference);
            }
            remap[original] = (data.len() - 1) as u16;
        }
        // A decoded label is not a durability proof. This verifies and flushes the
        // actual selected inode/parents under the held single-writer lock once.
        for reference in &data {
            writer.confirm_durable(*reference, deadline)?;
        }
        check(deadline)?;
        Ok(Self {
            writer,
            binding,
            data,
            remap,
            pages: Vec::with_capacity(MAX_METADATA_PAGES),
            page: Vec::with_capacity(MAX_PAGE_BYTES),
            page_rows: 0,
            rows: 0,
            metadata_bytes: 0,
            logical_bytes: 0,
            deadline,
            failed: false,
        })
    }
    pub(crate) fn push(&mut self, row: AcceptedRowV1) -> Result<()> {
        check(self.deadline)?;
        if self.failed {
            return Err(Error::Busy);
        }
        let result = self.push_checked(row);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn push_checked(&mut self, mut row: AcceptedRowV1) -> Result<()> {
        if self.rows as usize >= MAX_BATCH_ROWS {
            return Err(Error::Limit);
        }
        // Callers can mutate extent Vecs after construction. Bound aggregate
        // retained slots before encoding or invoking a nested page publisher.
        let mut retained_extents = 0usize;
        for field in row.fields.iter().flatten() {
            retained_extents = retained_extents.checked_add(field.extents.capacity()).ok_or(Error::Limit)?;
            if retained_extents > MAX_ROW_EXTENTS {
                return Err(Error::Limit);
            }
        }
        let mut extents = 0usize;
        let mut logical = self.logical_bytes;
        for field in row.fields.iter_mut().flatten() {
            validate_field(field.logical_bytes, &field.extents)?;
            extents = extents.checked_add(field.extents.len()).ok_or(Error::Limit)?;
            if extents > MAX_ROW_EXTENTS {
                return Err(Error::Limit);
            }
            logical = logical.checked_add(field.logical_bytes).ok_or(Error::Limit)?;
            if logical > MAX_LOGICAL_BYTES {
                return Err(Error::Limit);
            }
            for extent in &mut field.extents {
                extent.chunk_index = *self.remap.get(extent.chunk_index as usize).ok_or(Error::Corrupt)?;
            }
            validate_extents(field, &self.data)?;
        }
        let encoded = encode_row(&row)?;
        let framed = encoded.len().checked_add(4).ok_or(Error::Limit)?;
        if framed + PAGE_HEADER > MAX_PAGE_BYTES {
            return Err(Error::Limit);
        }
        if self.page_rows > 0 && self.page.len().checked_add(framed).ok_or(Error::Limit)? > MAX_PAGE_BYTES {
            self.flush_page()?;
        }
        if self.page_rows == 0 {
            if self.pages.len() >= MAX_METADATA_PAGES {
                return Err(Error::Limit);
            }
            self.page.extend_from_slice(PAGE_MAGIC);
            self.page.push(0);
            self.page.extend_from_slice(&0u32.to_le_bytes());
        }
        let new_size =
            self.metadata_bytes.checked_add(self.page.len()).and_then(|n| n.checked_add(framed)).ok_or(Error::Limit)?;
        if new_size > MAX_METADATA_BYTES {
            return Err(Error::Limit);
        }
        self.page.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
        self.page.extend_from_slice(&encoded);
        self.page_rows += 1;
        self.rows += 1;
        self.logical_bytes = logical;
        check(self.deadline)
    }
    fn flush_page(&mut self) -> Result<()> {
        check(self.deadline)?;
        if self.page_rows == 0 {
            return Ok(());
        }
        if self.pages.len() >= MAX_METADATA_PAGES {
            return Err(Error::Limit);
        }
        self.page[9..13].copy_from_slice(&self.page_rows.to_le_bytes());
        let reference = self.writer.publish(self.page.len() as u32, &mut &self.page[..], self.deadline)?;
        self.pages.push(PageRef { payload: reference, rows: self.page_rows });
        self.metadata_bytes = self.metadata_bytes.checked_add(self.page.len()).ok_or(Error::Limit)?;
        self.page.clear();
        self.page_rows = 0;
        #[cfg(test)]
        crash_boundary("manifest-page-durable");
        check(self.deadline)
    }
    pub(crate) fn finish(mut self) -> Result<BatchRef> {
        check(self.deadline)?;
        if self.failed {
            return Err(Error::Busy);
        }
        self.flush_page()?;
        let identity = self.writer.identity();
        let wire = encode_root(identity, &self.binding, self.rows, &self.data, &self.pages);
        #[cfg(test)]
        crash_boundary("manifest-root-before-publish");
        let root = self.writer.publish(wire.len() as u32, &mut &wire[..], self.deadline)?;
        #[cfg(test)]
        crash_boundary("manifest-root-durable");
        check(self.deadline)?;
        Ok(BatchRef { identity, binding: self.binding, generation: Generation(root.digest), root })
    }
}

fn put_optional_i64(out: &mut Vec<u8>, value: Option<i64>) {
    out.push(u8::from(value.is_some()));
    if let Some(value) = value {
        out.extend_from_slice(&value.to_le_bytes());
    }
}
fn put_optional_hash(out: &mut Vec<u8>, value: Option<[u8; 32]>) {
    out.push(u8::from(value.is_some()));
    if let Some(value) = value {
        out.extend_from_slice(&value);
    }
}
fn timing(row: &AcceptedRowV1) -> [Option<i64>; 11] {
    [
        row.t_date,
        row.t_utc,
        row.t_number,
        row.t_lines,
        row.t_references,
        row.t_xref,
        row.t_from,
        row.t_encoded,
        row.t_tool,
        row.t_clock,
        row.t_hw_n,
    ]
}
fn encode_row(row: &AcceptedRowV1) -> Result<Vec<u8>> {
    // Determine the full length before allocation; counts and byte lengths were
    // validated before any extent indexes were remapped.
    let scalars = [row.part, row.expected, row.file_total, row.file_index, row.posted_clock];
    let mut size = 106 + 32 * usize::from(row.filename_key.is_some()) + 32 * usize::from(row.message_key.is_some());
    size += 8 * (scalars.iter().filter(|s| s.is_some()).count() + timing(row).iter().filter(|s| s.is_some()).count());
    if let Some(nzb) = &row.nzb {
        size += 9 + 8 * usize::from(nzb.file_index.is_some());
    }
    for field in row.fields.iter().flatten() {
        size = size.checked_add(10).and_then(|n| n.checked_add(field.extents.len() * 10)).ok_or(Error::Limit)?;
    }
    if size > MAX_ROW_BYTES {
        return Err(Error::Limit);
    }
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(&row.ordinal.to_le_bytes());
    put_optional_hash(&mut out, row.filename_key);
    put_optional_hash(&mut out, row.message_key);
    for value in &scalars[..4] {
        put_optional_i64(&mut out, *value);
    }
    out.extend_from_slice(&row.bytes.to_le_bytes());
    out.extend_from_slice(&row.posted_at.to_le_bytes());
    out.push(u8::from(row.zero_file_index));
    put_optional_i64(&mut out, row.posted_clock);
    for value in timing(row) {
        put_optional_i64(&mut out, value);
    }
    out.extend_from_slice(&row.original_name_key);
    out.extend_from_slice(&[u8::from(row.is_obfuscated), u8::from(row.junk)]);
    out.push(u8::from(row.nzb.is_some()));
    if let Some(nzb) = &row.nzb {
        out.extend_from_slice(&nzb.timestamp.to_le_bytes());
        put_optional_i64(&mut out, nzb.file_index);
    }
    for (index, field) in row.fields.iter().enumerate() {
        out.extend_from_slice(&[(index + 1) as u8, u8::from(field.is_some())]);
        if let Some(field) = field {
            out.extend_from_slice(&field.logical_bytes.to_le_bytes());
            out.extend_from_slice(&(field.extents.len() as u16).to_le_bytes());
            for extent in &field.extents {
                out.extend_from_slice(&extent.chunk_index.to_le_bytes());
                out.extend_from_slice(&extent.offset.to_le_bytes());
                out.extend_from_slice(&extent.bytes.to_le_bytes());
            }
        }
    }
    debug_assert_eq!(out.len(), size);
    Ok(out)
}
fn encode_root(
    identity: Identity,
    binding: &ReleaseBinding,
    rows: u32,
    data: &[PayloadRef],
    pages: &[PageRef],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(100 + binding.group.len() + 36 * data.len() + 40 * pages.len());
    out.extend_from_slice(ROOT_MAGIC);
    out.push(0);
    out.extend_from_slice(&identity.to_bytes());
    out.extend_from_slice(&(binding.group.len() as u16).to_le_bytes());
    out.extend_from_slice(binding.group.as_bytes());
    out.push(u8::from(binding.is_collection));
    out.extend_from_slice(&binding.identity_key);
    out.extend_from_slice(&rows.to_le_bytes());
    out.extend_from_slice(&(data.len() as u16).to_le_bytes());
    out.extend_from_slice(&(pages.len() as u16).to_le_bytes());
    for reference in data {
        put_payload(&mut out, reference);
    }
    for page in pages {
        put_payload(&mut out, &page.payload);
        out.extend_from_slice(&page.rows.to_le_bytes());
    }
    out
}
fn put_payload(out: &mut Vec<u8>, reference: &PayloadRef) {
    out.extend_from_slice(&reference.digest);
    out.extend_from_slice(&reference.bytes.to_le_bytes());
}

struct Decode<'a> {
    wire: &'a [u8],
    position: usize,
}
impl<'a> Decode<'a> {
    fn new(wire: &'a [u8]) -> Self {
        Self { wire, position: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.position.checked_add(n).ok_or(Error::Limit)?;
        let value = self.wire.get(self.position..end).ok_or(Error::Corrupt)?;
        self.position = end;
        Ok(value)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().map_err(|_| Error::Corrupt)?)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }
    fn boolean(&mut self) -> Result<bool> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::Corrupt),
        }
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.array()?))
    }
    fn optional_i64(&mut self) -> Result<Option<i64>> {
        if self.boolean()? { Ok(Some(self.i64()?)) } else { Ok(None) }
    }
    fn optional_hash(&mut self) -> Result<Option<[u8; 32]>> {
        if self.boolean()? { Ok(Some(self.array()?)) } else { Ok(None) }
    }
    fn end(&self) -> Result<()> {
        if self.position == self.wire.len() { Ok(()) } else { Err(Error::Corrupt) }
    }
    fn header(&mut self, magic: &[u8; 8]) -> Result<()> {
        if self.take(8)? != magic || self.byte()? != 0 {
            return Err(Error::Corrupt);
        }
        Ok(())
    }
    fn payload(&mut self, identity: Identity) -> Result<PayloadRef> {
        PayloadRef::from_parts(identity, self.array()?, self.u32()?)
    }
}

struct DecodedRoot {
    data: Vec<PayloadRef>,
    pages: Vec<PageRef>,
    rows: u32,
}
fn decode_root(wire: &[u8], reference: &BatchRef) -> Result<DecodedRoot> {
    let mut input = Decode::new(wire);
    input.header(ROOT_MAGIC)?;
    if Identity::from_bytes(input.array()?) != reference.identity {
        return Err(Error::IdentityMismatch);
    }
    let group_len = input.u16()? as usize;
    if group_len > 4096 {
        return Err(Error::Limit);
    }
    let group = input.take(group_len)?;
    let collection = input.boolean()?;
    let key: [u8; 32] = input.array()?;
    if group != reference.binding.group.as_bytes()
        || collection != reference.binding.is_collection
        || key != reference.binding.identity_key
    {
        return Err(Error::IdentityMismatch);
    }
    let rows = input.u32()?;
    let data_count = input.u16()? as usize;
    let page_count = input.u16()? as usize;
    if rows as usize > MAX_BATCH_ROWS || data_count > MAX_DATA_CHUNKS || page_count > MAX_METADATA_PAGES {
        return Err(Error::Limit);
    }
    // Check the entire count-derived allocation against the remaining bytes first.
    let table_bytes = data_count
        .checked_mul(36)
        .and_then(|n| page_count.checked_mul(40).and_then(|p| n.checked_add(p)))
        .ok_or(Error::Limit)?;
    if input.wire.len() - input.position != table_bytes {
        return Err(Error::Corrupt);
    }
    let mut data: Vec<PayloadRef> = Vec::with_capacity(data_count);
    for _ in 0..data_count {
        let child = input.payload(reference.identity)?;
        if data.last().is_some_and(|last| last.digest >= child.digest) {
            return Err(Error::Corrupt);
        }
        data.push(child);
    }
    let mut pages = Vec::with_capacity(page_count);
    let mut metadata = 0usize;
    let mut counted = 0u32;
    for _ in 0..page_count {
        let payload = input.payload(reference.identity)?;
        let page_rows = input.u32()?;
        if payload.bytes as usize > MAX_PAGE_BYTES {
            return Err(Error::Limit);
        }
        if (payload.bytes as usize) < PAGE_HEADER + 4 || page_rows == 0 {
            return Err(Error::Corrupt);
        }
        counted = counted.checked_add(page_rows).ok_or(Error::Limit)?;
        if counted as usize > MAX_BATCH_ROWS {
            return Err(Error::Limit);
        }
        metadata = metadata.checked_add(payload.bytes as usize).ok_or(Error::Limit)?;
        if metadata > MAX_METADATA_BYTES {
            return Err(Error::Limit);
        }
        pages.push(PageRef { payload, rows: page_rows });
    }
    if counted != rows {
        return Err(Error::Corrupt);
    }
    input.end()?;
    Ok(DecodedRoot { data, pages, rows })
}

fn decode_row(wire: &[u8], data: &[PayloadRef], token: FieldToken) -> Result<AcceptedRowV1> {
    if wire.len() > MAX_ROW_BYTES {
        return Err(Error::Limit);
    }
    let mut input = Decode::new(wire);
    let mut row = AcceptedRowV1 {
        ordinal: input.i64()?,
        filename_key: input.optional_hash()?,
        message_key: input.optional_hash()?,
        part: input.optional_i64()?,
        expected: input.optional_i64()?,
        file_total: input.optional_i64()?,
        file_index: input.optional_i64()?,
        bytes: input.i64()?,
        posted_at: input.i64()?,
        zero_file_index: input.boolean()?,
        posted_clock: input.optional_i64()?,
        t_date: input.optional_i64()?,
        t_utc: input.optional_i64()?,
        t_number: input.optional_i64()?,
        t_lines: input.optional_i64()?,
        t_references: input.optional_i64()?,
        t_xref: input.optional_i64()?,
        t_from: input.optional_i64()?,
        t_encoded: input.optional_i64()?,
        t_tool: input.optional_i64()?,
        t_clock: input.optional_i64()?,
        t_hw_n: input.optional_i64()?,
        original_name_key: input.array()?,
        is_obfuscated: input.boolean()?,
        junk: input.boolean()?,
        ..Default::default()
    };
    if input.boolean()? {
        row.nzb = Some(NzbEnvelopeV1 { timestamp: input.i64()?, file_index: input.optional_i64()? });
    }
    let mut total_extents = 0usize;
    for index in 0..FIELD_COUNT {
        if input.byte()? != (index + 1) as u8 {
            return Err(Error::Corrupt);
        }
        if !input.boolean()? {
            continue;
        }
        let logical = input.u64()?;
        let count = input.u16()? as usize;
        total_extents = total_extents.checked_add(count).ok_or(Error::Limit)?;
        if logical > MAX_FIELD_BYTES || total_extents > MAX_ROW_EXTENTS {
            return Err(Error::Limit);
        }
        // Bound both count and its required remaining wire before allocating.
        if count.checked_mul(10).ok_or(Error::Limit)? > input.wire.len() - input.position {
            return Err(Error::Corrupt);
        }
        let mut extents = Vec::with_capacity(count);
        for _ in 0..count {
            extents.push(Extent { chunk_index: input.u16()?, offset: input.u32()?, bytes: input.u32()? });
        }
        let mut field = FieldRef::new(logical, extents)?;
        validate_extents(&field, data)?;
        field.token = Some(FieldToken { descriptor: descriptor_digest(&field), ..token });
        row.fields[index] = Some(field);
    }
    input.end()?;
    Ok(row)
}

/// The root remains pinned; only one page/data descriptor is retained at a time.
/// A decoded field has a compact immutable identity/release/generation seal and
/// descriptor digest. Caller mutation cannot convert it into another range token.
pub(crate) struct BatchSnapshot<'a> {
    session: ReadSession<'a>,
    _root: Chunk,
    token: FieldToken,
    data: Vec<PayloadRef>,
    pages: Vec<PageRef>,
    rows: u32,
    yielded: u32,
    logical_bytes: u64,
    page_index: usize,
    page_rows: u32,
    page_position: usize,
    page_wire: Vec<u8>,
    page: Option<Chunk>,
    current_data: Option<(u16, Chunk)>,
    deadline: Instant,
    failed: bool,
}
impl<'a> BatchSnapshot<'a> {
    pub(crate) fn open(
        session: ReadSession<'a>,
        reference: &BatchRef,
        expected: &ReleaseBinding,
        deadline: Instant,
    ) -> Result<Self> {
        check(deadline)?;
        if &reference.binding != expected || reference.root.identity != reference.identity {
            return Err(Error::IdentityMismatch);
        }
        if reference.generation.0 != reference.root.digest {
            return Err(Error::Corrupt);
        }
        if expected.group.len() > 4096 || reference.root.bytes as usize > MAX_ROOT_BYTES {
            return Err(Error::Limit);
        }
        let root = session.open_chunk(reference.root, deadline)?;
        let mut wire = vec![0; reference.root.bytes as usize];
        if root.read_range(0, &mut wire, deadline)? != wire.len() {
            return Err(Error::Corrupt);
        }
        let decoded = decode_root(&wire, reference)?;
        check(deadline)?;
        Ok(Self {
            session,
            _root: root,
            token: FieldToken {
                identity: reference.identity,
                binding: binding_digest(expected),
                generation: reference.generation,
                descriptor: [0; 32],
            },
            data: decoded.data,
            pages: decoded.pages,
            rows: decoded.rows,
            yielded: 0,
            logical_bytes: 0,
            page_index: 0,
            page_rows: 0,
            page_position: 0,
            page_wire: Vec::with_capacity(MAX_PAGE_BYTES),
            page: None,
            current_data: None,
            deadline,
            failed: false,
        })
    }
    pub(crate) fn next_row(&mut self) -> Result<Option<AcceptedRowV1>> {
        check(self.deadline)?;
        if self.failed {
            return Err(Error::Busy);
        }
        let result = self.next_checked();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn next_checked(&mut self) -> Result<Option<AcceptedRowV1>> {
        if self.yielded == self.rows {
            if self.page_rows != 0 || self.page_index != self.pages.len() {
                return Err(Error::Corrupt);
            }
            return Ok(None);
        }
        if self.page_rows == 0 {
            self.load_page()?;
        }
        let mut input = Decode::new(&self.page_wire[self.page_position..]);
        let length = input.u32()? as usize;
        if length > MAX_ROW_BYTES {
            return Err(Error::Limit);
        }
        let wire = input.take(length)?;
        let row = decode_row(wire, &self.data, self.token)?;
        let mut logical = self.logical_bytes;
        for field in row.fields.iter().flatten() {
            logical = logical.checked_add(field.logical_bytes).ok_or(Error::Limit)?;
        }
        if logical > MAX_LOGICAL_BYTES {
            return Err(Error::Limit);
        }
        let next_position = self.page_position.checked_add(input.position).ok_or(Error::Limit)?;
        // Refuse trailing bytes before yielding the final row from this page.
        if self.page_rows == 1 && next_position != self.page_wire.len() {
            return Err(Error::Corrupt);
        }
        self.page_position = next_position;
        self.page_rows -= 1;
        self.yielded += 1;
        self.logical_bytes = logical;
        check(self.deadline)?;
        Ok(Some(row))
    }
    fn load_page(&mut self) -> Result<()> {
        self.page = None; // Release old descriptor before opening a replacement.
        self.current_data = None;
        let reference = *self.pages.get(self.page_index).ok_or(Error::Corrupt)?;
        self.page_wire.clear();
        let page = self.session.open_chunk(reference.payload, self.deadline)?;
        self.page_wire.resize(reference.payload.bytes as usize, 0);
        if page.read_range(0, &mut self.page_wire, self.deadline)? != self.page_wire.len() {
            return Err(Error::Corrupt);
        }
        let mut input = Decode::new(&self.page_wire);
        input.header(PAGE_MAGIC)?;
        let rows = input.u32()?;
        if rows != reference.rows {
            return Err(Error::Corrupt);
        }
        // Validate the complete bounded page framing before exposing its first
        // row. A truncated later frame must not admit an earlier descriptor.
        let start = input.position;
        for _ in 0..rows {
            let length = input.u32()? as usize;
            if length > MAX_ROW_BYTES {
                return Err(Error::Limit);
            }
            input.take(length)?;
        }
        input.end()?;
        self.page_position = start;
        self.page_rows = rows;
        self.page_index += 1;
        self.page = Some(page);
        Ok(())
    }
    pub(crate) fn read_field_range(
        &mut self,
        field: &FieldRef,
        offset: u64,
        out: &mut [u8],
        deadline: Instant,
    ) -> Result<usize> {
        let deadline = deadline.min(self.deadline);
        check(deadline)?;
        if self.failed {
            return Err(Error::Busy);
        }
        let token = field.token.as_ref().ok_or(Error::IdentityMismatch)?;
        if token.identity != self.token.identity
            || token.binding != self.token.binding
            || token.generation != self.token.generation
        {
            return Err(Error::IdentityMismatch);
        }
        validate_extents(field, &self.data)?;
        if token.descriptor != descriptor_digest(field) {
            return Err(Error::Corrupt);
        }
        if out.len() > MAX_RANGE_BYTES || offset > field.logical_bytes {
            return Err(Error::Limit);
        }
        let end = offset.checked_add(out.len() as u64).ok_or(Error::Limit)?.min(field.logical_bytes);
        // Argument/foreign-token refusals above do not invalidate a good session.
        // Once storage access begins, any failure is terminal, including a lazy
        // Missing/Corrupt/Io: restoring the pathname cannot recover this snapshot.
        let result = self.read_field_checked(field, offset, end, out, deadline);
        if result.is_err() {
            self.failed = true;
            self.current_data = None;
        }
        result
    }
    fn read_field_checked(
        &mut self,
        field: &FieldRef,
        offset: u64,
        end: u64,
        out: &mut [u8],
        deadline: Instant,
    ) -> Result<usize> {
        let mut logical = 0u64;
        let mut read = 0usize;
        for extent in &field.extents {
            check(deadline)?;
            let extent_end = logical.checked_add(extent.bytes as u64).ok_or(Error::Limit)?;
            if extent_end > offset && logical < end {
                let start = offset.max(logical) - logical;
                let bytes = (extent_end.min(end) - offset.max(logical)) as usize;
                if self.current_data.as_ref().is_none_or(|(index, _)| *index != extent.chunk_index) {
                    self.current_data = None; // Never retain the previous data FD while opening another.
                    let chunk = self.session.open_chunk(self.data[extent.chunk_index as usize], deadline)?;
                    self.current_data = Some((extent.chunk_index, chunk));
                }
                let chunk = &self.current_data.as_ref().ok_or(Error::Corrupt)?.1;
                if chunk.read_range(extent.offset as u64 + start, &mut out[read..read + bytes], deadline)? != bytes {
                    return Err(Error::Corrupt);
                }
                read += bytes;
            }
            logical = extent_end;
            if logical >= end {
                break;
            }
        }
        check(deadline)?;
        Ok(read)
    }
}

#[cfg(test)]
fn crash_boundary(boundary: &str) {
    if std::env::var("ATLAS_MANIFEST_TEST_MODE").as_deref() != Ok(boundary) {
        return;
    }
    let marker = std::env::var_os("ATLAS_MANIFEST_TEST_MARKER").expect("child marker");
    std::fs::write(marker, boundary.as_bytes()).expect("signal actual manifest publication boundary");
    loop {
        std::thread::park();
    }
}
