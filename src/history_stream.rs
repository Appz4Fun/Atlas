//! Bounded, SQLite-free readers for v1 sealed segments and frozen payloads.
//!
//! The caller supplies already-resolved compressed bytes through [`Read`]. No
//! locator is fetched here. Events (including completed rows) are **provisional
//! until the function returns `Ok`**: consumers must roll back transactions or
//! discard spools on any error. This includes late corruption and failure of the
//! final EOF check. A callback error is returned unchanged, without another read
//! or callback. Borrowed chunks live only for the callback and may split UTF-8.
//!
//! Retained decoder state is two configured read buffers, one chunk buffer,
//! constant format/UTF-8 state, and Zstd's window-bounded context/working storage
//! (`O(2^window_log_max)`). There are no count- or cell-sized allocations. Caller
//! retention and caller-supplied readers are outside this bound. Counts are not
//! truncated; admission limits return errors. Empty-column rows use one explicit
//! run event, bounding callback work even when a malformed tiny input claims
//! many rows with no encoded values. No runtime consumer is switched.

use std::io::{self, BufReader, Read};

const MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
const FROZEN_MAX: u64 = 1 << 30;
const MAX_BUFFER: usize = 1 << 20;

/// Explicit resource admission. Defaults accommodate the existing v1 encoders.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Capacity of each of the compressed and expanded read buffers (1..=1MiB).
    pub read_buffer_bytes: usize,
    /// Maximum bytes in one callback chunk (1..=1MiB).
    pub chunk_bytes: usize,
    /// Maximum Zstd window log, 10..=31. Default 27 bounds the window to 128MiB.
    pub window_log_max: u32,
    /// Maximum expanded bytes. Frozen payloads additionally retain the 1GiB cap.
    pub max_expanded_bytes: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self { read_buffer_bytes: 8192, chunk_bytes: 8192, window_log_max: 27, max_expanded_bytes: FROZEN_MAX }
    }
}

/// Successful full-input counters, not an allocator/RSS measurement. On failure
/// the caller can retain its own provisional callback/source counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Bytes returned by the compressed input reader, including read-ahead.
    pub compressed_bytes: u64,
    /// Calls to the compressed reader, including the final EOF probe.
    pub read_calls: u64,
    /// Largest buffer passed to the compressed reader.
    pub max_read_request: usize,
    /// Bytes returned by Zstd (all validated on successful return).
    pub expanded_bytes: u64,
    /// Total configured Rust buffer capacity: two read buffers plus one chunk.
    /// Excludes the separately window-bounded Zstd C allocations and stack state.
    pub buffer_bytes: usize,
    /// Largest chunk actually delivered to the visitor.
    pub max_chunk_bytes: usize,
    /// Sealed segments processed.
    pub segments: u64,
    /// Frozen domain entries processed.
    pub domains: u64,
    /// Frozen tables processed.
    pub tables: u64,
    /// Frozen rows processed across all tables.
    pub rows: u64,
    /// Frozen typed values processed across all rows.
    pub values: u64,
}

/// Provenance of the current byte sequence. All kinds except Local/Blob must
/// contain valid UTF-8, checked incrementally before final success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BytesKind {
    /// Exact sealed local/message-ID bytes, which need not be UTF-8.
    Local,
    /// A frozen domain suffix.
    DomainSuffix,
    /// A frozen table name.
    TableName,
    /// A frozen column name.
    ColumnName,
    /// A typed text cell.
    Text,
    /// An arbitrary binary cell.
    Blob,
}

/// Ordered visitor events. Indexes are zero-based and counts are exact. A byte
/// sequence always has Start, zero or more Chunks, then End on valid input.
/// Frozen order: Domains/Domain+suffix, Tables/Table+name/Columns/Column+name/
/// Rows/Row+values/EndRow/EndTable (or Rows/EmptyRows/EndTable for zero columns).
/// Sealed order: Segments/Segment+local/EndSegment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event<'a> {
    /// Sealed segment count.
    Segments(u64),
    /// Segment metadata; signed identities and wrapping delta arithmetic match v1.
    Segment {
        /// Position in the encoded file.
        index: u64,
        /// Original optional part, with legacy two's-complement representation.
        part: Option<i64>,
        /// Exact signed byte count after applying its wrapped delta.
        bytes: i64,
        /// Signed domain identity after applying the explicit base (zero stays zero).
        domain: i64,
    },
    /// End of a segment, after its local bytes.
    EndSegment,
    /// Frozen domain count.
    Domains(u64),
    /// Signed frozen domain ID, followed by its suffix.
    Domain(i64),
    /// Frozen table count.
    Tables(u64),
    /// Table index, followed by its name.
    Table(u64),
    /// Column count for this table.
    Columns(u64),
    /// Column index, followed by its name.
    Column(u64),
    /// Row count for this table.
    Rows(u64),
    /// Row index within this table, followed by one value per column.
    Row(u64),
    /// All zero-column rows in this table, with logical indexes 0..count. Replaces
    /// individual Row/EndRow pairs, preserving the exact count without work
    /// amplification from an untrusted count that consumes no encoded bytes.
    /// Do not expand this provisional run before final validation; spool its
    /// metadata and apply consumer work/output admission when replaying it.
    EmptyRows(u64),
    /// SQL null.
    Null,
    /// Exact signed integer.
    Integer(i64),
    /// Exact IEEE-754 f64 bits, preserving NaN payloads and negative zero.
    RealBits(u64),
    /// Begin a named byte sequence or cell.
    BytesStart {
        /// Sequence provenance.
        kind: BytesKind,
        /// Exact encoded byte length.
        len: u64,
    },
    /// Borrowed bytes, at most Config::chunk_bytes, possibly splitting UTF-8.
    Chunk(&'a [u8]),
    /// End of the current byte sequence.
    BytesEnd,
    /// End of the current row.
    EndRow,
    /// End of the current table.
    EndTable,
}

/// Source inspection does not fetch, decompress, or validate the full frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source<'a> {
    /// Borrowed bytes starting with ordinary Zstd magic.
    Inline(&'a [u8]),
    /// Fixed-size sealed-log reference; the caller owns resolution and retries.
    Locator(sealed_log::Locator),
}
/// A stored sealed reference, with an explicit domain-base adjustment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reference<'a> {
    /// Apply to each nonzero domain with legacy wrapping addition.
    pub domain_base: i64,
    /// Inline bytes or an external locator; never an implicit service fallback.
    pub source: Source<'a>,
}

/// Inspect inline, Locator, or one SHIFTED_TAG wrapper without allocating.
/// Malformed wrappers/locators and unrecognized magic are explicit errors.
/// Nested wrappers are not a v1 encoding and are rejected.
pub fn inspect_reference(bytes: &[u8]) -> io::Result<Reference<'_>> {
    let (domain_base, inner) = match bytes.strip_prefix(&crate::blob::SHIFTED_TAG) {
        Some(rest) => {
            if rest.len() <= 8 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "incomplete shifted reference"));
            }
            (i64::from_le_bytes(rest[..8].try_into().expect("checked length")), &rest[8..])
        }
        None => (0, bytes),
    };
    let source = if inner.starts_with(&sealed_log::REF_TAG) {
        Source::Locator(sealed_log::Locator::from_ref(inner).ok_or_else(|| bad("invalid locator"))?)
    } else if inner.starts_with(&MAGIC) {
        Source::Inline(inner)
    } else {
        return Err(bad("unknown sealed reference"));
    };
    Ok(Reference { domain_base, source })
}

/// Stream one resolved v1 sealed Zstd frame. Domain base is explicit and domain
/// zero is never shifted. Byte deltas, parts and signed domain bits retain the
/// legacy wrapping semantics. Overflowed varints and trailing bytes/frames,
/// which legacy parsing sometimes ignored, are deliberately rejected.
pub fn sealed<R: Read, V: FnMut(Event<'_>) -> io::Result<()>>(
    reader: R,
    domain_base: i64,
    config: Config,
    mut visitor: V,
) -> io::Result<Stats> {
    let mut p = Parser::new(reader, config, false)?;
    p.version()?;
    let count = p.count(4)?;
    visitor(Event::Segments(count))?;
    let mut previous = 0i64;
    for index in 0..count {
        let part = match p.varint()? {
            0 => None,
            value => Some((value - 1) as i64),
        };
        let bytes = previous.wrapping_add(unzigzag(p.varint()?));
        previous = bytes;
        let domain = p.varint()? as i64;
        let domain = if domain == 0 { 0 } else { domain.wrapping_add(domain_base) };
        visitor(Event::Segment { index, part, bytes, domain })?;
        p.bytes(BytesKind::Local, &mut visitor)?;
        visitor(Event::EndSegment)?;
    }
    p.stats.segments = count;
    p.finish()
}

/// I/O class for an explicitly admitted streaming consumer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadClass {
    Compressed,
    Expanded,
}
/// Optional backpressure at both actual read boundaries. An allowance may
/// block, but must return 1..=requested or an error. Completion charges exactly
/// the bytes actually returned, including decoder read-ahead and EOF probes.
pub trait Admission: Send + Sync {
    fn allowance(&self, class: ReadClass, requested: usize) -> io::Result<usize>;
    fn completed(&self, class: ReadClass, bytes: usize) -> io::Result<()>;
}
/// The ordinary codec contract with caller-owned I/O backpressure.
pub fn frozen_admitted<R: Read, V: FnMut(Event<'_>) -> io::Result<()>>(
    reader: R,
    config: Config,
    admission: std::sync::Arc<dyn Admission>,
    visitor: V,
) -> io::Result<Stats> {
    frozen_with_admission(reader, config, Some(admission), visitor)
}

/// Stream a complete v1 frozen frame, without SQLite or table/row materialization.
/// Counts use the legacy minimum-encoded-size checks, deferred to final validation
/// when the frame's expanded length is known. Empty-column rows therefore remain
/// compatible with the old parser, including its minimum-one-byte admission rule.
pub fn frozen<R: Read, V: FnMut(Event<'_>) -> io::Result<()>>(
    reader: R,
    config: Config,
    visitor: V,
) -> io::Result<Stats> {
    frozen_with_admission(reader, config, None, visitor)
}

fn frozen_with_admission<R: Read, V: FnMut(Event<'_>) -> io::Result<()>>(
    reader: R,
    config: Config,
    admission: Option<std::sync::Arc<dyn Admission>>,
    mut visitor: V,
) -> io::Result<Stats> {
    let mut p = Parser::new_admitted(reader, config, true, admission)?;
    p.version()?;
    let domains = p.count(2)?;
    visitor(Event::Domains(domains))?;
    for _ in 0..domains {
        visitor(Event::Domain(unzigzag(p.varint()?)))?;
        p.bytes(BytesKind::DomainSuffix, &mut visitor)?;
    }
    let tables = p.count(3)?;
    visitor(Event::Tables(tables))?;
    for table in 0..tables {
        visitor(Event::Table(table))?;
        p.bytes(BytesKind::TableName, &mut visitor)?;
        let columns = p.count(1)?;
        visitor(Event::Columns(columns))?;
        for column in 0..columns {
            visitor(Event::Column(column))?;
            p.bytes(BytesKind::ColumnName, &mut visitor)?;
        }
        let rows = p.count(columns.max(1))?;
        p.stats.rows = checked_add(p.stats.rows, rows)?;
        p.stats.values =
            checked_add(p.stats.values, rows.checked_mul(columns).ok_or_else(|| bad("cell count overflow"))?)?;
        visitor(Event::Rows(rows))?;
        if columns == 0 {
            if rows != 0 {
                visitor(Event::EmptyRows(rows))?;
            }
            visitor(Event::EndTable)?;
            continue;
        }
        for row in 0..rows {
            visitor(Event::Row(row))?;
            for _ in 0..columns {
                match p.byte()? {
                    0 => visitor(Event::Null)?,
                    1 => visitor(Event::Integer(unzigzag(p.varint()?)))?,
                    2 => visitor(Event::RealBits(u64::from_le_bytes(p.fixed()?)))?,
                    3 => p.bytes(BytesKind::Text, &mut visitor)?,
                    4 => p.bytes(BytesKind::Blob, &mut visitor)?,
                    _ => return Err(bad("unknown frozen value tag")),
                }
            }
            visitor(Event::EndRow)?;
        }
        visitor(Event::EndTable)?;
    }
    p.stats.domains = domains;
    p.stats.tables = tables;
    p.finish()
}

fn bad(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn checked_add(a: u64, b: u64) -> io::Result<u64> {
    a.checked_add(b).ok_or_else(|| bad("history count/length overflow"))
}
fn unzigzag(value: u64) -> i64 {
    ((value >> 1) as i64) ^ -((value & 1) as i64)
}

fn admit(admission: &Option<std::sync::Arc<dyn Admission>>, class: ReadClass, requested: usize) -> io::Result<usize> {
    if requested == 0 {
        return Ok(0);
    }
    let n = if let Some(a) = admission { a.allowance(class, requested)? } else { requested };
    if n == 0 || n > requested {
        return Err(bad("invalid history read allowance"));
    }
    Ok(n)
}

struct Input<R> {
    reader: R,
    stats: Stats,
    magic_seen: usize,
    admission: Option<std::sync::Arc<dyn Admission>>,
}
impl<R: Read> Read for Input<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.stats.read_calls = checked_add(self.stats.read_calls, 1)?;
        self.stats.max_read_request = self.stats.max_read_request.max(out.len());
        let allowed = admit(&self.admission, ReadClass::Compressed, out.len())?;
        let n = self.reader.read(&mut out[..allowed])?;
        if let Some(a) = &self.admission {
            a.completed(ReadClass::Compressed, n)?;
        }
        self.stats.compressed_bytes = checked_add(self.stats.compressed_bytes, n as u64)?;
        for &byte in &out[..n.min(4 - self.magic_seen)] {
            if byte != MAGIC[self.magic_seen] {
                return Err(bad("expected one ordinary Zstd frame"));
            }
            self.magic_seen += 1;
        }
        Ok(n)
    }
}

type Frame<R> = zstd::stream::read::Decoder<'static, BufReader<Input<R>>>;
struct Expanded<R> {
    reader: R,
    total: u64,
    limit: u64,
    admission: Option<std::sync::Arc<dyn Admission>>,
}
impl<R: Read> Read for Expanded<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let remaining = self.limit - self.total;
        // One extra byte proves a limit violation; even at the limit we must
        // still ask Zstd to validate its trailer and detect expanded garbage.
        let size = out.len().min(usize::try_from(remaining.saturating_add(1)).unwrap_or(usize::MAX));
        let size = admit(&self.admission, ReadClass::Expanded, size)?;
        let n = self.reader.read(&mut out[..size])?;
        if let Some(a) = &self.admission {
            a.completed(ReadClass::Expanded, n)?;
        }
        if n as u64 > remaining {
            return Err(bad("expanded history exceeds configured limit"));
        }
        self.total += n as u64;
        Ok(n)
    }
}

struct Parser<R: Read> {
    reader: BufReader<Expanded<Frame<R>>>,
    chunk: Vec<u8>,
    limit: u64,
    position: u64,
    minimum_end: u64,
    stats: Stats,
}
impl<R: Read> Parser<R> {
    fn new(reader: R, config: Config, is_frozen: bool) -> io::Result<Self> {
        Self::new_admitted(reader, config, is_frozen, None)
    }
    fn new_admitted(
        reader: R,
        config: Config,
        is_frozen: bool,
        admission: Option<std::sync::Arc<dyn Admission>>,
    ) -> io::Result<Self> {
        if !(1..=MAX_BUFFER).contains(&config.read_buffer_bytes)
            || !(1..=MAX_BUFFER).contains(&config.chunk_bytes)
            || !(10..=31).contains(&config.window_log_max)
            || (is_frozen && config.max_expanded_bytes > FROZEN_MAX)
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid history reader limits"));
        }
        let input = Input { reader, stats: Stats::default(), magic_seen: 0, admission: admission.clone() };
        let input = BufReader::with_capacity(config.read_buffer_bytes, input);
        let mut decoder = zstd::stream::read::Decoder::with_buffer(input)?.single_frame();
        decoder.window_log_max(config.window_log_max)?;
        let expanded = Expanded { reader: decoder, total: 0, limit: config.max_expanded_bytes, admission };
        Ok(Self {
            reader: BufReader::with_capacity(config.read_buffer_bytes, expanded),
            chunk: vec![0; config.chunk_bytes],
            limit: config.max_expanded_bytes,
            position: 0,
            minimum_end: 0,
            stats: Stats { buffer_bytes: 2 * config.read_buffer_bytes + config.chunk_bytes, ..Stats::default() },
        })
    }
    fn fixed<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let mut out = [0; N];
        self.reader.read_exact(&mut out)?;
        self.position = checked_add(self.position, N as u64)?;
        Ok(out)
    }
    fn byte(&mut self) -> io::Result<u8> {
        Ok(self.fixed::<1>()?[0])
    }
    fn version(&mut self) -> io::Result<()> {
        if self.byte()? != 1 {
            return Err(bad("unknown history version"));
        }
        Ok(())
    }
    fn varint(&mut self) -> io::Result<u64> {
        let mut value = 0;
        for shift in (0..64).step_by(7) {
            let byte = self.byte()?;
            // The tenth byte has only one representable bit. The old readers
            // dropped excess bits; accepting that would silently change IDs.
            if shift == 63 && byte > 1 {
                return Err(bad("overflowed history varint"));
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(bad("history varint too long"))
    }
    fn count(&mut self, minimum_bytes: u64) -> io::Result<u64> {
        let count = self.varint()?;
        let size = count.checked_mul(minimum_bytes).ok_or_else(|| bad("history count overflow"))?;
        let end = checked_add(self.position, size)?;
        if end > self.limit {
            return Err(bad("history count/length exceeds expanded limit"));
        }
        self.minimum_end = self.minimum_end.max(end);
        Ok(count)
    }
    fn bytes<V: FnMut(Event<'_>) -> io::Result<()>>(&mut self, kind: BytesKind, visitor: &mut V) -> io::Result<()> {
        let len = self.count(1)?;
        visitor(Event::BytesStart { kind, len })?;
        let text = !matches!(kind, BytesKind::Local | BytesKind::Blob);
        let mut utf8 = Utf8::default();
        let mut left = len;
        while left > 0 {
            let n = left.min(self.chunk.len() as u64) as usize;
            self.reader.read_exact(&mut self.chunk[..n])?;
            self.position = checked_add(self.position, n as u64)?;
            if text {
                utf8.feed(&self.chunk[..n])?;
            }
            self.stats.max_chunk_bytes = self.stats.max_chunk_bytes.max(n);
            visitor(Event::Chunk(&self.chunk[..n]))?;
            left -= n as u64;
        }
        if utf8.remaining != 0 {
            return Err(bad("incomplete UTF-8 in history text"));
        }
        visitor(Event::BytesEnd)
    }
    fn finish(mut self) -> io::Result<Stats> {
        let mut byte = [0];
        if self.reader.read(&mut byte)? != 0 {
            return Err(bad("trailing expanded history bytes"));
        }
        if self.position < self.minimum_end {
            return Err(bad("history count past expanded end"));
        }
        let expanded = self.reader.into_inner();
        self.stats.expanded_bytes = expanded.total;
        let mut input = expanded.reader.finish();
        // single_frame leaves any look-ahead in this BufReader. Probe it before
        // unwrapping, so an empty second frame or buffered garbage cannot vanish.
        if input.read(&mut byte)? != 0 {
            return Err(bad("extra compressed frame or trailing garbage"));
        }
        let input_stats = input.into_inner().stats;
        self.stats.compressed_bytes = input_stats.compressed_bytes;
        self.stats.read_calls = input_stats.read_calls;
        self.stats.max_read_request = input_stats.max_read_request;
        Ok(self.stats)
    }
}

#[derive(Default)]
struct Utf8 {
    remaining: u8,
    low: u8,
    high: u8,
}
impl Utf8 {
    fn feed(&mut self, bytes: &[u8]) -> io::Result<()> {
        for &byte in bytes {
            if self.remaining > 0 {
                if !(self.low..=self.high).contains(&byte) {
                    return Err(bad("invalid UTF-8 in history text"));
                }
                self.remaining -= 1;
                self.low = 0x80;
                self.high = 0xbf;
            } else {
                let (remaining, low, high) = match byte {
                    0..=0x7f => (0, 0x80, 0xbf),
                    0xc2..=0xdf => (1, 0x80, 0xbf),
                    0xe0 => (2, 0xa0, 0xbf),
                    0xe1..=0xec | 0xee..=0xef => (2, 0x80, 0xbf),
                    0xed => (2, 0x80, 0x9f),
                    0xf0 => (3, 0x90, 0xbf),
                    0xf1..=0xf3 => (3, 0x80, 0xbf),
                    0xf4 => (3, 0x80, 0x8f),
                    _ => return Err(bad("invalid UTF-8 in history text")),
                };
                self.remaining = remaining;
                self.low = low;
                self.high = high;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    struct DenyExpanded;
    impl Admission for DenyExpanded {
        fn allowance(&self, class: ReadClass, requested: usize) -> io::Result<usize> {
            if class == ReadClass::Expanded {
                Err(io::Error::other("expanded admission denied"))
            } else {
                Ok(requested)
            }
        }
        fn completed(&self, _: ReadClass, _: usize) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn admitted_frozen_charges_expansion_before_callback() {
        let packed = zstd::bulk::compress(&[1, 0, 0], 1).unwrap();
        let mut callbacks = 0;
        let result = frozen_admitted(packed.as_slice(), Config::default(), std::sync::Arc::new(DenyExpanded), |_| {
            callbacks += 1;
            Ok(())
        });
        assert!(result.is_err(), "expanded input must obey admission");
        assert_eq!(callbacks, 0, "no provisional callback may bypass expansion admission");
    }
}
