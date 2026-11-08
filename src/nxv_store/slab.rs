//! A slab: one server's rows of one group and one day, gathered in memory until they're sealed into a block. Its rows
//! sit in one byte arena with an offset each (no allocation per row), and every row also goes to the slab's own log
//! as a framed record, written and synced by the store's batcher, soo a crash before the seal loses nothing that a
//! cursor moved past.
//!
//! A record is `[len u32][crc32 u32][ts i64][line]`, little-endian, the CRC over the timestamp and the line. Replay
//! stops at the first record that is short or fails its CRC: a torn last row is dropped, never kept cut short.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};

/// The most bytes one overview line may have in a log record.
pub const MAX_LINE: usize = 1 << 20;
const RECORD_HEAD: usize = 16;

/// `slab-<server>-<day>-<gen>.log`
pub fn log_name(server: u16, day: i64, slab_gen: u32) -> String {
    format!("slab-{server}-{}-{slab_gen}.log", super::segment::day_name(day))
}

/// (server, day, gen) of a log's file name.
pub fn parse_log_name(name: &str) -> Option<(u16, i64, u32)> {
    let rest = name.strip_prefix("slab-")?.strip_suffix(".log")?;
    let (server, rest) = rest.split_once('-')?;
    let (day, slab_gen) = rest.rsplit_once('-')?;
    Some((server.parse().ok()?, super::segment::parse_day_name(day)?, slab_gen.parse().ok()?))
}

/// One record's bytes appended to `out`.
pub fn frame(ts: i64, line: &[u8], out: &mut Vec<u8>) {
    let mut h = crc32fast::Hasher::new();
    h.update(&ts.to_le_bytes());
    h.update(line);
    out.extend_from_slice(&(line.len() as u32).to_le_bytes());
    out.extend_from_slice(&h.finalize().to_le_bytes());
    out.extend_from_slice(&ts.to_le_bytes());
    out.extend_from_slice(line);
}

/// The records of a log's bytes, up to the first short or damaged one: (timestamp, line) each.
pub fn unframe(bytes: &[u8]) -> Vec<(i64, &[u8])> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + RECORD_HEAD <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes")) as usize;
        let crc = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().expect("4 bytes"));
        if len == 0 || len > MAX_LINE || at + RECORD_HEAD + len > bytes.len() {
            break;
        }
        let ts_bytes = &bytes[at + 8..at + 16];
        let line = &bytes[at + RECORD_HEAD..at + RECORD_HEAD + len];
        let mut h = crc32fast::Hasher::new();
        h.update(ts_bytes);
        h.update(line);
        if h.finalize() != crc {
            break;
        }
        out.push((i64::from_le_bytes(ts_bytes.try_into().expect("8 bytes")), line));
        at += RECORD_HEAD + len;
    }
    out
}

/// The article number a line starts with.
pub fn art_of(line: &[u8]) -> Option<u64> {
    let n = line.iter().take_while(|b| b.is_ascii_digit()).count();
    if n == 0 || n > 19 || line.get(n) != Some(&b'\t') {
        return None;
    }
    std::str::from_utf8(&line[..n]).ok()?.parse().ok()
}

pub struct Slab {
    pub server: u16,
    pub day: i64,
    pub slab_gen: u32,
    pub log_path: PathBuf,
    pub log: Arc<File>,
    pub created: std::time::Instant,
    pub last_row: std::time::Instant,
    arena: Vec<u8>,
    /// (article, timestamp, start in arena); a row ends where the next starts
    rows: Vec<(u64, i64, u32)>,
    /// framed records not yet written to the log
    pending: Vec<u8>,
}

/// A sealed slab's rows, sorted by article number with repeats dropped, ready for the block builder.
pub struct Sealed {
    pub server: u16,
    pub day: i64,
    pub slab_gen: u32,
    pub log_path: PathBuf,
    pub lines: Vec<Vec<u8>>,
    pub dropped_repeats: usize,
}

impl Slab {
    /// A new slab and its log, made with `create_new` (a log is never truncated).
    pub fn create(dir: &Path, server: u16, day: i64, slab_gen: u32) -> Result<Slab> {
        let log_path = dir.join(log_name(server, day, slab_gen));
        let log = OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .open(&log_path)
            .with_context(|| format!("making {}", log_path.display()))?;
        let now = std::time::Instant::now();
        Ok(Slab {
            server,
            day,
            slab_gen,
            log_path,
            log: Arc::new(log),
            created: now,
            last_row: now,
            arena: Vec::new(),
            rows: Vec::new(),
            pending: Vec::new(),
        })
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// A row in memory and framed for the log (no system call).
    pub fn push(&mut self, art: u64, ts: i64, line: &[u8]) {
        self.rows.push((art, ts, self.arena.len() as u32));
        self.arena.extend_from_slice(line);
        frame(ts, line, &mut self.pending);
        self.last_row = std::time::Instant::now();
    }

    /// The framed bytes waiting for the log, taken (the batcher writes them outside the group's lock).
    pub fn take_pending(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    fn line(&self, i: usize) -> &[u8] {
        let start = self.rows[i].2 as usize;
        let end = self.rows.get(i + 1).map_or(self.arena.len(), |r| r.2 as usize);
        &self.arena[start..end]
    }

    /// The slab's rows sorted by article number, repeats dropped (a slice read twice after a crash, or a cursor that
    /// lagged), for sealing. The log stays until its block is committed.
    pub fn seal(self) -> Sealed {
        let mut order: Vec<usize> = (0..self.rows.len()).collect();
        order.sort_by_key(|&i| (self.rows[i].0, i));
        let mut lines = Vec::with_capacity(order.len());
        let mut last: Option<u64> = None;
        let mut dropped = 0;
        for i in order {
            let art = self.rows[i].0;
            if last == Some(art) {
                dropped += 1;
                continue;
            }
            last = Some(art);
            lines.push(self.line(i).to_vec());
        }
        Sealed {
            server: self.server,
            day: self.day,
            slab_gen: self.slab_gen,
            log_path: self.log_path,
            lines,
            dropped_repeats: dropped,
        }
    }
}

/// Appends `bytes` to a log (the batcher's write, outside any lock).
pub fn append(log: &File, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = log;
    f.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip_and_a_torn_or_damaged_tail_is_dropped() {
        let mut b = Vec::new();
        frame(10, b"1\tone", &mut b);
        frame(11, b"2\ttwo", &mut b);
        let whole = b.len();
        frame(12, b"3\tthree", &mut b);
        let got = unframe(&b);
        assert_eq!(got, vec![(10, &b"1\tone"[..]), (11, &b"2\ttwo"[..]), (12, &b"3\tthree"[..])]);
        // torn in the middle of the third record
        assert_eq!(unframe(&b[..b.len() - 3]).len(), 2);
        // the third record's line damaged
        let mut bad = b.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert_eq!(unframe(&bad).len(), 2);
        // zeroed space after the last record (what a power cut can leave)
        let mut zeroed = b[..whole].to_vec();
        zeroed.extend_from_slice(&[0; 40]);
        assert_eq!(unframe(&zeroed).len(), 2);
    }

    #[test]
    fn log_names_round_trip() {
        let day = super::super::segment::parse_day_name("2026-11-08").unwrap();
        let n = log_name(7, day, 42);
        assert_eq!(n, "slab-7-2026-11-08-42.log");
        assert_eq!(parse_log_name(&n), Some((7, day, 42)));
        assert_eq!(parse_log_name("slab-x-2026-11-08-1.log"), None);
    }

    #[test]
    fn a_seal_sorts_by_article_and_drops_repeats() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Slab::create(dir.path(), 1, 20_000, 1).unwrap();
        for (art, line) in [(5, "5\te"), (3, "3\tc"), (5, "5\te"), (4, "4\td")] {
            s.push(art, 0, line.as_bytes());
        }
        assert!(Slab::create(dir.path(), 1, 20_000, 1).is_err(), "a log is never made twice");
        let pending = s.take_pending();
        append(&s.log, &pending).unwrap();
        assert!(!s.has_pending());
        let sealed = s.seal();
        assert_eq!(sealed.lines, vec![b"3\tc".to_vec(), b"4\td".to_vec(), b"5\te".to_vec()]);
        assert_eq!(sealed.dropped_repeats, 1);
        let on_disk = std::fs::read(&sealed.log_path).unwrap();
        assert_eq!(unframe(&on_disk).len(), 4, "the log keeps every row until its block is committed");
    }

    #[test]
    fn article_numbers_read_from_the_line() {
        assert_eq!(art_of(b"123\tx"), Some(123));
        assert_eq!(art_of(b"x\t"), None);
        assert_eq!(art_of(b"123"), None);
    }
}
