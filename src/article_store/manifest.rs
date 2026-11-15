//! Inactive normalized accepted-batch records. These carry no release authority.
use super::{Error, Identity, PayloadRef, Reader, Result, Writer};
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
    token: Option<(Identity, ReleaseBinding, Generation)>,
}
impl FieldRef {
    pub(crate) fn new(logical_bytes: u64, extents: Vec<Extent>) -> Result<Self> {
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

pub(crate) struct BatchManifestBuilder<'a> {
    _writer: &'a mut Writer,
}
impl<'a> BatchManifestBuilder<'a> {
    pub(crate) fn new(writer: &'a mut Writer, _: ReleaseBinding, _: Vec<PayloadRef>, _: Instant) -> Result<Self> {
        let _ = writer;
        Err(Error::Unsupported)
    }
    pub(crate) fn push(&mut self, _: AcceptedRowV1) -> Result<()> {
        Err(Error::Unsupported)
    }
    pub(crate) fn finish(self) -> Result<BatchRef> {
        Err(Error::Unsupported)
    }
}
pub(crate) struct BatchSnapshot<'a> {
    _reader: &'a Reader,
}
impl<'a> BatchSnapshot<'a> {
    pub(crate) fn open(_: &'a Reader, _: &BatchRef, _: &ReleaseBinding, _: Instant) -> Result<Self> {
        Err(Error::Unsupported)
    }
    pub(crate) fn next_row(&mut self) -> Result<Option<AcceptedRowV1>> {
        Err(Error::Unsupported)
    }
    pub(crate) fn read_field_range(&mut self, _: &FieldRef, _: u64, _: &mut [u8], _: Instant) -> Result<usize> {
        Err(Error::Unsupported)
    }
}
