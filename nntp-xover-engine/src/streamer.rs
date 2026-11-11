//! Writing rows back out: random access by row, a range cursor that walks one
//! varint step per row, and a Tokio sink that streams `XOVER` responses.
//!
//! Nothing here allocates per row once the scratch buffers are warm: the
//! dictionary strings of the previous row are kept by id, so a run of
//! articles from one post decodes no FSST at all.

use anyhow::Result;
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::container::{
    DICT_AUTHOR, DICT_DOMAIN, DICT_LITERAL, DICT_PFX, DICT_SUBJ, DICT_XREF, DecodedRow, HotSections, NUM_DICTS,
    NxvBlockView, PADMASK_LITERAL, PoolHint, RowDecoder, RowParts, SUB_BLOCK_ROWS, msgid_field, render_row,
};
use crate::container::{STYLE_NO_BYTES, STYLE_NO_LINES, STYLE_SUBJ_IS_LOCAL};
use crate::date::{COMMENT_UTC, DATE_BUF, STYLE_FREE_BITS, format_date, style_offset_secs};
use crate::msgid_codec::unpack_msgid_tail_into;
use crate::msgid_index::msgid_hash;
use crate::subject_codec::{PadMask, SubjectNums};

pub const WIRE_BUF_CAPACITY: usize = 64 * 1024;
pub const WIRE_FLUSH_AT: usize = 60 * 1024;
const NONE: u32 = u32::MAX;

/// The previous row's dictionary strings and date, by id.
pub struct RowCache {
    block_tag: (usize, u32),
    pub cached_subj_tpl: Vec<u8>,
    pub cached_subj_tpl_id: u32,
    pub cached_xref_skel: Vec<u8>,
    pub cached_xref_skel_id: u32,
    pub cached_author: Vec<u8>,
    pub cached_author_id: u32,
    pub cached_domain: Vec<u8>,
    pub cached_domain_id: u32,
    pub cached_msgid_pfx: Vec<u8>,
    pub cached_msgid_pfx_id: u32,
    pub cached_date_bytes: [u8; DATE_BUF],
    pub cached_date_len: usize,
    pub cached_timestamp: i64,
    pub cached_style: u32,
    zone_len: usize,
    /// rows whose subject template came from the cache
    pub tpl_hits: u64,
    pub tpl_misses: u64,
    /// a buffer for message-id compares
    msgid_buf: Vec<u8>,
    /// where each dictionary's next entry starts, after the last one read (version 2)
    pool_hints: [PoolHint; NUM_DICTS],
}

impl Default for RowCache {
    fn default() -> Self {
        Self {
            block_tag: (0, 0),
            cached_subj_tpl: Vec::with_capacity(1024),
            cached_subj_tpl_id: NONE,
            cached_xref_skel: Vec::with_capacity(512),
            cached_xref_skel_id: NONE,
            cached_author: Vec::with_capacity(256),
            cached_author_id: NONE,
            cached_domain: Vec::with_capacity(256),
            cached_domain_id: NONE,
            cached_msgid_pfx: Vec::with_capacity(64),
            cached_msgid_pfx_id: NONE,
            cached_date_bytes: [0; DATE_BUF],
            cached_date_len: 0,
            cached_timestamp: 0,
            cached_style: NONE,
            zone_len: 0,
            tpl_hits: 0,
            tpl_misses: 0,
            msgid_buf: Vec::with_capacity(1024),
            pool_hints: [PoolHint::default(); NUM_DICTS],
        }
    }
}

impl RowCache {
    /// Forgets cached ids when moving to another block. The tag is the block's address and its header CRC,
    /// which covers the body hash: two blocks that differ anywhere differ in it, even when one is read from a
    /// buffer the other was freed from.
    fn bind(&mut self, view: &NxvBlockView) {
        let tag = (view.data.as_ptr() as usize, view.header.header_crc32);
        if tag != self.block_tag {
            self.block_tag = tag;
            self.invalidate();
        }
    }

    /// Forgets the cached dictionary strings, soo the next row decodes them afresh.
    pub fn invalidate(&mut self) {
        self.cached_subj_tpl_id = NONE;
        self.cached_xref_skel_id = NONE;
        self.cached_author_id = NONE;
        self.cached_domain_id = NONE;
        self.cached_msgid_pfx_id = NONE;
        self.pool_hints = [PoolHint::default(); NUM_DICTS];
    }
}

#[inline(always)]
fn refresh(h: &HotSections, dict: usize, id: u32, cached_id: &mut u32, buf: &mut Vec<u8>, hint: &mut PoolHint) -> bool {
    if *cached_id == id {
        return true;
    }
    buf.clear();
    h.dict_entry_hinted(dict, id, hint, buf);
    *cached_id = id;
    false
}

/// Connection-scoped buffers for writing rows.
pub struct XoverStreamScratch {
    pub wire_buf: Vec<u8>,
    pub cache: RowCache,
}

impl Default for XoverStreamScratch {
    fn default() -> Self {
        Self { wire_buf: Vec::with_capacity(WIRE_BUF_CAPACITY), cache: RowCache::default() }
    }
}

impl XoverStreamScratch {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Appends row `row` (decoded as `d`) to `out`, without CRLF.
#[inline]
pub(crate) fn write_row(
    h: &HotSections,
    xref_prefix: bool,
    row: usize,
    d: &DecodedRow,
    c: &mut RowCache,
    out: &mut Vec<u8>,
) {
    if d.literal {
        h.dict_entry_hinted(DICT_LITERAL, h.subj_tpl.at(row) as u32, &mut c.pool_hints[DICT_LITERAL], out);
        return;
    }
    let hints = &mut c.pool_hints;
    // a subject kept per row needs no template
    if d.subj.is_none() {
        let tpl_id = h.subj_tpl.at(row) as u32;
        if refresh(h, DICT_SUBJ, tpl_id, &mut c.cached_subj_tpl_id, &mut c.cached_subj_tpl, &mut hints[DICT_SUBJ]) {
            c.tpl_hits += 1;
        } else {
            c.tpl_misses += 1;
        }
    }
    refresh(
        h,
        DICT_XREF,
        h.skel.at(row) as u32,
        &mut c.cached_xref_skel_id,
        &mut c.cached_xref_skel,
        &mut hints[DICT_XREF],
    );
    refresh(
        h,
        DICT_AUTHOR,
        h.author.at(row) as u32,
        &mut c.cached_author_id,
        &mut c.cached_author,
        &mut hints[DICT_AUTHOR],
    );
    refresh(
        h,
        DICT_DOMAIN,
        h.domain.at(row) as u32,
        &mut c.cached_domain_id,
        &mut c.cached_domain,
        &mut hints[DICT_DOMAIN],
    );
    refresh(
        h,
        DICT_PFX,
        h.pfx.at(row) as u32,
        &mut c.cached_msgid_pfx_id,
        &mut c.cached_msgid_pfx,
        &mut hints[DICT_PFX],
    );
    let date_style = d.style & !STYLE_FREE_BITS;
    if d.ts != c.cached_timestamp || date_style != c.cached_style {
        // the hour as written is the local one: compare and patch in local time (offsets can be +0530)
        let off = style_offset_secs(date_style);
        let (local, cached_local) = (d.ts.wrapping_add(off), c.cached_timestamp.wrapping_add(off));
        let same_hour = date_style == c.cached_style && local.div_euclid(3600) == cached_local.div_euclid(3600);
        if same_hour {
            // only MM:SS changed: patch those four digits
            let s = local.rem_euclid(3600) as u32;
            let at = c.cached_date_len - c.zone_len - 6;
            let b = &mut c.cached_date_bytes;
            b[at] = b'0' + (s / 600) as u8;
            b[at + 1] = b'0' + (s / 60 % 10) as u8;
            b[at + 3] = b'0' + (s % 60 / 10) as u8;
            b[at + 4] = b'0' + (s % 10) as u8;
        } else {
            c.cached_date_len = format_date(d.ts, date_style, &mut c.cached_date_bytes);
            c.zone_len =
                if (date_style >> 2) & 3 == 0 { 5 } else { 3 } + if date_style & COMMENT_UTC != 0 { 6 } else { 0 };
        }
        c.cached_timestamp = d.ts;
        c.cached_style = date_style;
    }
    render_row(
        &RowParts {
            art: d.art,
            subj_is_local: d.style & STYLE_SUBJ_IS_LOCAL != 0,
            subj_packed: d.subj.map(|(s, e)| h.tail.get(s..e).unwrap_or(&[])),
            tpl: &c.cached_subj_tpl,
            nums: SubjectNums { vals: d.nums, pad: PadMask(h.pad.at(row)) },
            author: &c.cached_author,
            date: &c.cached_date_bytes[..c.cached_date_len],
            pfx: &c.cached_msgid_pfx,
            tail_packed: h.tail.get(d.tail.0..d.tail.1).unwrap_or(&[]),
            domain: &c.cached_domain,
            refs: h.refs.get(d.refs.0..d.refs.1).unwrap_or(&[]),
            bytes: (d.style & STYLE_NO_BYTES == 0).then_some(d.bytes),
            lines: (d.style & STYLE_NO_LINES == 0).then_some(d.lines),
            skel: &c.cached_xref_skel,
            xref_prefix,
            xref: &d.xref[..d.nx],
        },
        out,
    );
}

impl<'a> NxvBlockView<'a> {
    /// Appends row `row` (0-based, article order) to `out`, without CRLF.
    pub fn reconstruct_row(&self, row: usize, scratch: &mut RowCache, out: &mut Vec<u8>) -> Result<()> {
        anyhow::ensure!(row < self.row_count(), "row {row} out of range");
        let h = self.hot()?;
        scratch.bind(self);
        let mut dec = RowDecoder::at_sub_block(self, row / SUB_BLOCK_ROWS);
        let mut d = dec.step(h);
        while dec.row <= row {
            d = dec.step(h);
        }
        write_row(h, self.xref_prefix(), row, &d, scratch, out);
        Ok(())
    }

    /// [`Self::reconstruct_row`] into a new Vec.
    pub fn reconstruct_row_vec(&self, row: usize) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.reconstruct_row(row, &mut RowCache::default(), &mut out)?;
        Ok(out)
    }

    fn msgid_matches(&self, h: &HotSections, row: usize, raw: &[u8], buf: &mut Vec<u8>) -> bool {
        buf.clear();
        if h.pad.get(row).is_none_or(|p| p & PADMASK_LITERAL != 0) {
            h.dict_entry_into(DICT_LITERAL, h.subj_tpl.at(row) as u32, buf);
            return msgid_field(buf) == Some(raw);
        }
        buf.push(b'<');
        h.dict_entry_into(DICT_PFX, h.pfx.at(row) as u32, buf);
        let (ts, te) = h.msgid_tail_range(row);
        unpack_msgid_tail_into(h.tail.get(ts..te).unwrap_or(&[]), buf);
        buf.push(b'@');
        h.dict_entry_into(DICT_DOMAIN, h.domain.at(row) as u32, buf);
        buf.push(b'>');
        buf.as_slice() == raw
    }

    /// The row whose message-id is `raw_msgid` (with its angle brackets), by
    /// the filter, the bucket table and an exact compare. Always None for a
    /// cold block: check [`Self::may_contain_msgid`] and decompress it first.
    pub fn find_row_by_msgid_with(&self, raw_msgid: &[u8], scratch: &mut RowCache) -> Option<usize> {
        let hash = msgid_hash(raw_msgid);
        if !self.may_contain_msgid_hash(hash) {
            return None;
        }
        let h = self.hot.as_ref()?;
        let mut buf = std::mem::take(&mut scratch.msgid_buf);
        let r = h.bucket.find(hash, |row| self.msgid_matches(h, row, raw_msgid, &mut buf));
        scratch.msgid_buf = buf;
        r
    }

    pub fn find_row_by_msgid(&self, raw_msgid: &[u8]) -> Option<usize> {
        let hash = msgid_hash(raw_msgid);
        if !self.may_contain_msgid_hash(hash) {
            return None;
        }
        let h = self.hot.as_ref()?;
        let mut buf = Vec::new();
        h.bucket.find(hash, |row| self.msgid_matches(h, row, raw_msgid, &mut buf))
    }
}

/// One row's fields as a linker reads them, without writing the line out and parsing it again.
#[derive(Debug, Clone, Copy)]
pub struct RowFields<'c> {
    pub art: u64,
    /// the date as UTC seconds, and as the poster's wall clock (the zone it was written in added, then dropped)
    pub utc: i64,
    pub wall: i64,
    pub bytes: Option<i64>,
    pub lines: Option<i64>,
    pub subject: &'c [u8],
    pub author: &'c [u8],
    /// with its angle brackets
    pub msgid: &'c [u8],
    /// the whole line of a row kept literal (the fields above are empty then: parse the line)
    pub literal: Option<&'c [u8]>,
}

/// Every row of one hot block in order, as [`RowFields`]: each dictionary string decoded once per run of rows that
/// share it, no date formatted, nothing allocated per row once warm.
pub struct FieldCursor<'a> {
    view: NxvBlockView<'a>,
    h: HotSections<'a>,
    dec: RowDecoder,
    cache: RowCache,
    subj: Vec<u8>,
    msgid: Vec<u8>,
    line: Vec<u8>,
    /// rows whose wall clock falls outside this are passed over before any string is decoded
    window: (i64, i64),
    /// rows passed over for the window
    pub skipped: u64,
    #[cfg(test)]
    seek_decoded: usize,
}

impl<'a> FieldCursor<'a> {
    pub fn new(view: NxvBlockView<'a>) -> Result<Self> {
        let h = *view.hot()?;
        let dec = RowDecoder::at_sub_block(&view, 0);
        let mut cache = RowCache::default();
        cache.bind(&view);
        Ok(Self {
            view,
            h,
            dec,
            cache,
            subj: Vec::with_capacity(256),
            msgid: Vec::with_capacity(128),
            line: Vec::with_capacity(512),
            window: (i64::MIN, i64::MAX),
            skipped: 0,
            #[cfg(test)]
            seek_decoded: 0,
        })
    }

    /// Resume at a raw row ordinal using the format's 128-row checkpoints.
    /// At most `SUB_BLOCK_ROWS` numeric rows are decoded to reach the offset;
    /// skipped dictionary strings are not reconstructed. This bound applies
    /// equally to ordinary page boundaries and process restart.
    pub fn new_at_row(view: NxvBlockView<'a>, row: usize) -> Result<Self> {
        anyhow::ensure!(row <= view.row_count(), "field cursor row out of range");
        let mut cursor = Self::new(view)?;
        let checkpoint = row.min(cursor.view.row_count().saturating_sub(1)) / SUB_BLOCK_ROWS;
        cursor.dec = RowDecoder::at_sub_block(&cursor.view, checkpoint);
        while cursor.dec.row < row {
            cursor.dec.advance(&cursor.view, &cursor.h);
            #[cfg(test)]
            {
                cursor.seek_decoded += 1;
            }
        }
        Ok(cursor)
    }

    /// Only rows whose wall clock is in `from..=to` (rows kept literal always come, their date unread).
    pub fn with_wall_window(mut self, from: i64, to: i64) -> Self {
        self.window = (from, to);
        self
    }

    /// The next row's fields; None after the last.
    #[allow(clippy::should_implement_trait)]
    pub fn next_row(&mut self) -> Option<RowFields<'_>> {
        let h = &self.h;
        let (d, row, off) = loop {
            if self.dec.row >= self.view.row_count() {
                return None;
            }
            let d = self.dec.advance(&self.view, h);
            let off = style_offset_secs(d.style & !STYLE_FREE_BITS);
            let wall = d.ts.wrapping_add(off);
            if d.literal || (self.window.0..=self.window.1).contains(&wall) {
                break (d, self.dec.row - 1, off);
            }
            self.skipped += 1;
        };
        let c = &mut self.cache;
        if d.literal {
            self.line.clear();
            h.dict_entry_hinted(
                DICT_LITERAL,
                h.subj_tpl.at(row) as u32,
                &mut c.pool_hints[DICT_LITERAL],
                &mut self.line,
            );
            return Some(RowFields {
                art: d.art,
                utc: 0,
                wall: 0,
                bytes: None,
                lines: None,
                subject: &[],
                author: &[],
                msgid: &[],
                literal: Some(&self.line),
            });
        }
        let hints = &mut c.pool_hints;
        refresh(
            h,
            DICT_AUTHOR,
            h.author.at(row) as u32,
            &mut c.cached_author_id,
            &mut c.cached_author,
            &mut hints[DICT_AUTHOR],
        );
        refresh(
            h,
            DICT_DOMAIN,
            h.domain.at(row) as u32,
            &mut c.cached_domain_id,
            &mut c.cached_domain,
            &mut hints[DICT_DOMAIN],
        );
        refresh(
            h,
            DICT_PFX,
            h.pfx.at(row) as u32,
            &mut c.cached_msgid_pfx_id,
            &mut c.cached_msgid_pfx,
            &mut hints[DICT_PFX],
        );
        let tail = h.tail.get(d.tail.0..d.tail.1).unwrap_or(&[]);
        self.msgid.clear();
        self.msgid.push(b'<');
        self.msgid.extend_from_slice(&c.cached_msgid_pfx);
        unpack_msgid_tail_into(tail, &mut self.msgid);
        self.msgid.push(b'@');
        self.msgid.extend_from_slice(&c.cached_domain);
        self.msgid.push(b'>');
        self.subj.clear();
        if d.style & STYLE_SUBJ_IS_LOCAL != 0 {
            self.subj.extend_from_slice(&c.cached_msgid_pfx);
            unpack_msgid_tail_into(tail, &mut self.subj);
        } else if let Some((s, e)) = d.subj {
            unpack_msgid_tail_into(h.tail.get(s..e).unwrap_or(&[]), &mut self.subj);
        } else {
            refresh(
                h,
                DICT_SUBJ,
                h.subj_tpl.at(row) as u32,
                &mut c.cached_subj_tpl_id,
                &mut c.cached_subj_tpl,
                &mut hints[DICT_SUBJ],
            );
            crate::subject_codec::render_subject(
                &c.cached_subj_tpl,
                &SubjectNums { vals: d.nums, pad: PadMask(h.pad.at(row)) },
                &mut self.subj,
            );
        }
        Some(RowFields {
            art: d.art,
            utc: d.ts,
            wall: d.ts.wrapping_add(off),
            bytes: (d.style & STYLE_NO_BYTES == 0).then_some(d.bytes),
            lines: (d.style & STYLE_NO_LINES == 0).then_some(d.lines),
            subject: &self.subj,
            author: &c.cached_author,
            msgid: &self.msgid,
            literal: None,
        })
    }
}

/// Rows numbered `min_art..=max_art` of one hot block, in order.
pub struct NxvRangeCursor<'a> {
    view: NxvBlockView<'a>,
    h: HotSections<'a>,
    dec: RowDecoder,
    max_art: u64,
    pending: Option<DecodedRow>,
    done: bool,
}

impl<'a> NxvRangeCursor<'a> {
    pub fn new_for_art_range(view: NxvBlockView<'a>, min_art: u64, max_art: u64) -> Result<Self> {
        let h = *view.hot()?;
        let cps = view.checkpoints;
        // the sub-block before the first one starting at min_art or above: rows equal to min_art can run back over
        // a checkpoint
        let sb = cps.partition_point(|c| (c.base_art_num as u64) < min_art).saturating_sub(1);
        let mut dec = RowDecoder::at_sub_block(&view, sb);
        let rows = view.row_count();
        let mut pending = None;
        let mut done = !view.overlaps_art_range(min_art, max_art);
        while !done && dec.row < rows {
            let d = dec.advance(&view, &h);
            if d.art >= min_art {
                pending = Some(d);
                break;
            }
        }
        if pending.is_none() {
            done = true;
        }
        Ok(Self { view, h, dec, max_art, pending, done })
    }

    /// Appends the next row and CRLF to `scratch.wire_buf`; false when done.
    #[inline]
    pub fn format_next_row(&mut self, scratch: &mut XoverStreamScratch) -> bool {
        if self.done {
            return false;
        }
        let d = match self.pending.take() {
            Some(d) => d,
            None if self.dec.row < self.view.row_count() => self.dec.advance(&self.view, &self.h),
            None => {
                self.done = true;
                return false;
            }
        };
        if d.art > self.max_art {
            self.done = true;
            return false;
        }
        scratch.cache.bind(&self.view);
        let row = self.dec.row - 1;
        write_row(&self.h, self.view.xref_prefix(), row, &d, &mut scratch.cache, &mut scratch.wire_buf);
        scratch.wire_buf.extend_from_slice(b"\r\n");
        true
    }
}

/// Streams an `XOVER min-max` response from blocks in article order (blocks
/// that don't overlap the range are skipped by their headers). Returns how
/// many rows were sent. Cold blocks must be decompressed before.
pub async fn stream_xover_blocks_tokio<W: AsyncWrite + Unpin>(
    views: &[NxvBlockView<'_>],
    min_art: u64,
    max_art: u64,
    scratch: &mut XoverStreamScratch,
    writer: &mut W,
) -> Result<usize> {
    scratch.wire_buf.clear();
    scratch.wire_buf.extend_from_slice(b"224 Overview information follows\r\n");
    let mut n = 0usize;
    for v in views.iter().filter(|v| v.overlaps_art_range(min_art, max_art)) {
        let mut cur = NxvRangeCursor::new_for_art_range(*v, min_art, max_art)?;
        loop {
            if scratch.wire_buf.len() >= WIRE_FLUSH_AT {
                writer.write_all(&scratch.wire_buf).await?;
                scratch.wire_buf.clear();
            }
            if !cur.format_next_row(scratch) {
                break;
            }
            n += 1;
        }
    }
    scratch.wire_buf.extend_from_slice(b".\r\n");
    writer.write_all(&scratch.wire_buf).await?;
    scratch.wire_buf.clear();
    writer.flush().await?;
    Ok(n)
}

pub async fn stream_xover_range_tokio<W: AsyncWrite + Unpin>(
    view: NxvBlockView<'_>,
    min_art: u64,
    max_art: u64,
    scratch: &mut XoverStreamScratch,
    writer: &mut W,
) -> Result<usize> {
    stream_xover_blocks_tokio(&[view], min_art, max_art, scratch, writer).await
}

#[cfg(test)]
mod field_seek_tests {
    use super::*;
    use crate::container::{AlignedBytes, NxvBlockBuilder};
    #[test]
    fn field_cursor_seek_decodes_at_most_one_checkpoint_and_matches_sequential_rows() {
        let lines = crate::corpus::generate_realistic_xover_corpus(513);
        let refs: Vec<_> = lines.iter().map(Vec::as_slice).collect();
        let block = AlignedBytes::from_slice(&NxvBlockBuilder::build_hot_block(&refs).unwrap());
        let mut sequential = FieldCursor::new(NxvBlockView::from_mmap_slice(&block).unwrap()).unwrap();
        for offset in 0..=513 {
            let mut seek = FieldCursor::new_at_row(NxvBlockView::from_mmap_slice(&block).unwrap(), offset).unwrap();
            assert!(seek.seek_decoded <= SUB_BLOCK_ROWS, "offset {offset} decoded {} prefix rows", seek.seek_decoded);
            assert!(seek.cache.cached_author.is_empty(), "seek decoded skipped dictionary strings");
            let expected = sequential.next_row().map(|r| {
                (r.art, r.wall, r.subject.to_vec(), r.author.to_vec(), r.msgid.to_vec(), r.literal.map(<[u8]>::to_vec))
            });
            let actual = seek.next_row().map(|r| {
                (r.art, r.wall, r.subject.to_vec(), r.author.to_vec(), r.msgid.to_vec(), r.literal.map(<[u8]>::to_vec))
            });
            assert_eq!(actual, expected, "resumed ordinal {offset}");
        }
        assert!(FieldCursor::new_at_row(NxvBlockView::from_mmap_slice(&block).unwrap(), 514).is_err());
    }
}
