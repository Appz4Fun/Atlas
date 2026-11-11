//! Bounded traversal of the legacy append log. Commit markers name the last N
//! complete lines, not necessarily every line since the preceding marker: a
//! failed append may have left an uncommitted prefix. Scan offsets first, then
//! revisit the accepted range. Neither a huge append nor a huge ID line needs
//! to be held in memory. The caller must keep the source immutable while reading.

use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::ops::Range;

const PREFIX: usize = 64;

struct Line {
    start: u64,
    end: u64,
    prefix: [u8; PREFIX],
    len: usize,
}

/// A line's bounds plus only enough bytes to recognize a version/commit marker.
/// Incomplete terminal lines are deliberately not returned.
fn line<R: BufRead>(reader: &mut R, offset: &mut u64) -> io::Result<Option<Line>> {
    let start = *offset;
    let mut prefix = [0; PREFIX];
    let mut len = 0usize;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok(None);
        }
        let end = buf.iter().position(|&b| b == b'\n');
        let n = end.map_or(buf.len(), |i| i + 1);
        let take = n.min(PREFIX.saturating_sub(len));
        prefix[len.min(PREFIX)..len.min(PREFIX) + take].copy_from_slice(&buf[..take]);
        len = len.saturating_add(n);
        *offset = offset.checked_add(n as u64).ok_or_else(|| io::Error::other("legacy offset overflow"))?;
        reader.consume(n);
        if end.is_some() {
            return Ok(Some(Line { start, end: *offset, prefix, len }));
        }
    }
}

impl Line {
    fn text(&self) -> Option<&str> {
        (self.len <= PREFIX).then(|| std::str::from_utf8(&self.prefix[..self.len]).ok()).flatten()
    }

    fn marker(&self) -> Option<u64> {
        self.text()?.strip_prefix("C\t")?.strip_suffix("\tok\n")?.parse().ok()
    }

    fn is_marker(&self) -> bool {
        self.len >= 2 && &self.prefix[..2] == b"C\t"
    }
}

/// Returns one committed byte range at a time. Empty commits are legal. For
/// old unmarked logs, each complete line is its own committed range. The reader
/// never retains offsets for individual pending lines.
pub(crate) struct Committed<R> {
    reader: BufReader<R>,
    offset: u64,
    pending_start: u64,
    pending_lines: u64,
    marked: bool,
    first: Option<Line>,
}

impl<R: Read + Seek> Committed<R> {
    pub(crate) fn new(source: R) -> io::Result<Self> {
        let mut reader = BufReader::new(source);
        let mut offset = 0;
        let first = line(&mut reader, &mut offset)?;
        let marked = first.as_ref().is_some_and(|l| l.text() == Some("V\t2\n"));
        Ok(Self {
            reader,
            offset,
            pending_start: if marked { offset } else { 0 },
            pending_lines: 0,
            marked,
            first: if marked { None } else { first },
        })
    }

    pub(crate) fn next_range(&mut self) -> io::Result<Option<Range<u64>>> {
        loop {
            let Some(l) =
                self.first.take().map_or_else(|| line(&mut self.reader, &mut self.offset), |l| Ok(Some(l)))?
            else {
                return Ok(None);
            };
            if !self.marked {
                return Ok(Some(l.start..l.end));
            }
            if l.is_marker() {
                let count = l.marker().filter(|&n| n <= self.pending_lines);
                let start = self.pending_start;
                let pending = self.pending_lines;
                self.pending_start = l.end;
                self.pending_lines = 0;
                let Some(count) = count else { continue };
                // A second sequential scan discards the torn prefix. This is
                // linear in source size, independent of the commit's length.
                self.reader.seek(SeekFrom::Start(start))?;
                let mut from = start;
                let mut skipped = 0;
                while skipped < pending - count {
                    let previous = line(&mut self.reader, &mut from)?
                        .ok_or_else(|| io::Error::other("legacy log changed during scan"))?;
                    if previous.text() != Some("V\t2\n") {
                        skipped += 1;
                    }
                }
                self.reader.seek(SeekFrom::Start(l.end))?;
                return Ok(Some(from..l.start));
            }
            if l.text() != Some("V\t2\n") {
                self.pending_lines =
                    self.pending_lines.checked_add(1).ok_or_else(|| io::Error::other("legacy line count overflow"))?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn accepted(bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut scan = Committed::new(Cursor::new(bytes)).unwrap();
        let mut out = Vec::new();
        while let Some(r) = scan.next_range().unwrap() {
            out.push(bytes[r.start as usize..r.end as usize].to_vec());
        }
        out
    }

    #[test]
    fn legacy_ranges_preserve_commit_suffix_and_ignore_torn_tail() {
        let log = b"V\t2\nF\tabandoned\nF\tkept\nI\t7\tff\nC\t2\tok\nF\tuncommitted\nC\t1\tok";
        assert_eq!(accepted(log), vec![b"F\tkept\nI\t7\tff\n".to_vec()]);
    }

    #[test]
    fn legacy_ranges_reject_bad_counts_and_partial_markers() {
        let log = b"V\t2\nF\tbad\nC\t2\tok\nF\tbad\nC\t1\nF\tgood\nC\t1\tok\n";
        assert_eq!(accepted(log), vec![b"F\tgood\n".to_vec()]);
    }

    #[test]
    fn legacy_ranges_accept_old_complete_lines_only() {
        assert_eq!(accepted(b"F\tone\nI\t2\tff\ncut"), vec![b"F\tone\n".to_vec(), b"I\t2\tff\n".to_vec()]);
    }

    #[test]
    fn legacy_ranges_scan_large_record_without_retaining_it() {
        let mut log = b"V\t2\nI\t2\t".to_vec();
        log.extend(std::iter::repeat_n(b'f', 100_000));
        log.extend_from_slice(b"\nC\t1\tok\n");
        let mut scan = Committed::new(Cursor::new(&log)).unwrap();
        let r = scan.next_range().unwrap().unwrap();
        assert_eq!(r, 4..100_009);
        assert!(scan.next_range().unwrap().is_none());
    }
}
