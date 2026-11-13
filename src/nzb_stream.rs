//! Incremental NZB XML and sorted-part validation.
//!
//! Storage readers supply already resolved physical files and segment order.
//! Text is read in bounded chunks, including a single very large subject or ID;
//! no method retains an entire file's articles or an entire XML document.

use std::io::{self, Read, Write};

const TEXT_CHUNK: usize = 8192;
const MISSING: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GapSummary {
    pub total: Option<i64>,
    pub have: i64,
    pub missing: Vec<i64>,
    pub stray: i64,
}

#[derive(Default)]
struct Numbering {
    have: i64,
    rows: i64,
    next: i64,
    missing: Vec<i64>,
}

/// A constant-size check for a file's part-ordered segments. Both numbering
/// conventions are tracked until the final row establishes whether zero occurs.
pub struct GapCheck {
    total: Option<i64>,
    limit: i64,
    previous: Option<Option<i64>>,
    rows: i64,
    zero_seen: bool,
    zero: Numbering,
    one: Numbering,
}

impl GapCheck {
    pub fn new(total: Option<i64>) -> Self {
        Self {
            total,
            limit: total.unwrap_or(0).clamp(0, crate::parser::MAX_PARTS),
            previous: None,
            rows: 0,
            zero_seen: false,
            zero: Numbering::default(),
            one: Numbering { next: 1, ..Numbering::default() },
        }
    }

    pub fn push(&mut self, part: Option<i64>) -> io::Result<()> {
        if self.previous.is_some_and(|previous| previous > part) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "segments are not in part order"));
        }
        self.rows = self.rows.checked_add(1).ok_or_else(|| io::Error::other("article count overflow"))?;
        self.zero_seen |= part == Some(0);
        let duplicate = self.previous == Some(part);
        self.previous = Some(part);
        if let Some(part) = part {
            Self::number(&mut self.zero, part, 0, self.limit, duplicate);
            Self::number(&mut self.one, part, 1, self.limit.saturating_add(1), duplicate);
        }
        Ok(())
    }

    fn number(state: &mut Numbering, part: i64, first: i64, end: i64, duplicate: bool) {
        if !(first..end).contains(&part) {
            return;
        }
        state.rows += 1;
        if duplicate {
            return;
        }
        while state.next < part && state.missing.len() < MISSING {
            state.missing.push(state.next);
            state.next += 1;
        }
        state.next = part + 1;
        state.have += 1;
    }

    pub fn finish(self) -> Option<GapSummary> {
        let mut selected = if self.zero_seen { self.zero } else { self.one };
        let end = self.limit + i64::from(!self.zero_seen);
        while selected.next < end && selected.missing.len() < MISSING {
            selected.missing.push(selected.next);
            selected.next += 1;
        }
        // Duplicate numbered articles count once for have, but do not become
        // stray: the legacy checked renderer also preserves same-part copies.
        let stray = self.rows - selected.rows;
        if self.total.is_some() && selected.have == self.limit && self.limit > 0 && stray == 0 {
            return None;
        }
        Some(GapSummary { total: self.total, have: selected.have, missing: selected.missing, stray })
    }

}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Head,
    Files,
    File,
    Segments,
    Finished,
}

/// XML is written directly into a bounded writer or private request spool.
/// A successful finish is required before a spool can become an HTTP body.
pub struct XmlWriter<W> {
    output: W,
    phase: Phase,
}

impl<W: Write> XmlWriter<W> {
    pub fn new(mut output: W, category: impl Read) -> io::Result<Self> {
        output.write_all(b"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n")?;
        output.write_all(b"<!DOCTYPE nzb PUBLIC \"-//newzBin//DTD NZB 1.1//EN\" \"http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd\">\n")?;
        output.write_all(b"<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">\n  <head>\n    <meta type=\"category\">")?;
        escaped(&mut output, category, false)?;
        output.write_all(b"</meta>\n")?;
        Ok(Self { output, phase: Phase::Head })
    }

    pub fn meta(&mut self, inferred: bool, text: impl Read) -> io::Result<()> {
        self.require(Phase::Head)?;
        self.output.write_all(if inferred { b"    <meta type=\"x-atlas-inferred\">" } else { b"    <meta type=\"x-atlas-incomplete\">" })?;
        escaped(&mut self.output, text, false)?;
        self.output.write_all(b"</meta>\n")
    }

    pub fn end_head(&mut self) -> io::Result<()> {
        self.require(Phase::Head)?;
        self.output.write_all(b"  </head>\n")?;
        self.phase = Phase::Files;
        Ok(())
    }

    pub fn file(&mut self, poster: impl Read, timestamp: i64, subject: impl Read) -> io::Result<()> {
        self.require(Phase::Files)?;
        self.output.write_all(b"  <file poster=\"")?;
        escaped(&mut self.output, poster, true)?;
        write!(self.output, "\" date=\"{timestamp}\" subject=\"")?;
        escaped(&mut self.output, subject, true)?;
        self.output.write_all(b"\">\n    <groups>\n")?;
        self.phase = Phase::File;
        Ok(())
    }

    pub fn group(&mut self, group: impl Read) -> io::Result<()> {
        self.require(Phase::File)?;
        self.output.write_all(b"      <group>")?;
        escaped(&mut self.output, group, false)?;
        self.output.write_all(b"</group>\n")
    }

    pub fn segments(&mut self) -> io::Result<()> {
        self.require(Phase::File)?;
        self.output.write_all(b"    </groups>\n    <segments>\n")?;
        self.phase = Phase::Segments;
        Ok(())
    }

    /// The source removes only leading/trailing angle brackets before exposing
    /// the ID stream, exactly as the legacy renderer's trim_matches does.
    pub fn segment(&mut self, bytes: i64, part: Option<i64>, message_id: impl Read) -> io::Result<()> {
        self.require(Phase::Segments)?;
        write!(self.output, "      <segment bytes=\"{bytes}\" number=\"")?;
        match part {
            Some(part) => write!(self.output, "{part}")?,
            None => self.output.write_all(b"None")?,
        }
        self.output.write_all(b"\">")?;
        escaped(&mut self.output, message_id, false)?;
        self.output.write_all(b"</segment>\n")
    }

    pub fn end_file(&mut self) -> io::Result<()> {
        self.require(Phase::Segments)?;
        self.output.write_all(b"    </segments>\n  </file>\n")?;
        self.phase = Phase::Files;
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<W> {
        self.require(Phase::Files)?;
        self.output.write_all(b"</nzb>")?;
        self.output.flush()?;
        self.phase = Phase::Finished;
        Ok(self.output)
    }

    fn require(&self, phase: Phase) -> io::Result<()> {
        if self.phase != phase {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid NZB XML writer phase"));
        }
        Ok(())
    }
}

/// The existing t=files schema, written without retaining filenames or a file
/// vector. Count/order are authenticated by the same snapshot's download plan.
pub(crate) struct FilesWriter<W> {
    output: W,
    expected: u64,
    written: u64,
    failed: bool,
}

impl<W: Write> FilesWriter<W> {
    pub(crate) fn new(mut output: W, release_id: i64, count: u64) -> io::Result<Self> {
        write!(output, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<files id=\"{release_id}\" count=\"{count}\">\n")?;
        Ok(Self { output, expected: count, written: 0, failed: false })
    }

    pub(crate) fn file(&mut self, name: Option<impl Read>, bytes: i64, articles: u64) -> io::Result<()> {
        if self.failed || self.written >= self.expected {
            self.failed = true;
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid file-list count or failed writer"));
        }
        self.failed = true;
        let ordinal = self.written.checked_add(1).ok_or_else(|| io::Error::other("file-list ordinal overflow"))?;
        write!(self.output, "  <file n=\"{ordinal}\"")?;
        if let Some(name) = name {
            self.output.write_all(b" name=\"")?;
            // Match api::escape plus quote replacement exactly. NZB XML
            // text has a separate legacy control-character policy.
            text_chunks(name, |text| {
                let mut start = 0;
                for (at, character) in text.char_indices() {
                    let replacement: Option<&[u8]> = match character {
                        '&' => Some(b"&amp;"), '<' => Some(b"&lt;"),
                        '>' => Some(b"&gt;"), '"' => Some(b"&quot;"),
                        _ => None,
                    };
                    if let Some(replacement) = replacement {
                        self.output.write_all(&text.as_bytes()[start..at])?;
                        self.output.write_all(replacement)?;
                        start = at + character.len_utf8();
                    }
                }
                self.output.write_all(&text.as_bytes()[start..])
            })?;
            self.output.write_all(b"\"")?;
        }
        writeln!(self.output, " size=\"{bytes}\" articles=\"{articles}\"/>")?;
        self.written = ordinal;
        self.failed = false;
        Ok(())
    }

    pub(crate) fn finish(mut self) -> io::Result<W> {
        if self.failed || self.written != self.expected {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "file-list is incomplete"));
        }
        self.output.write_all(b"</files>\n")?;
        self.output.flush()?;
        Ok(self.output)
    }
}

fn escaped(output: &mut impl Write, text: impl Read, attribute: bool) -> io::Result<()> {
    text_chunks(text, |value| {
        let mut start = 0;
        for (at, ch) in value.char_indices() {
            let replacement: Option<&[u8]> = match ch {
                '&' => Some(b"&amp;"),
                '<' => Some(b"&lt;"),
                '>' => Some(b"&gt;"),
                '"' if attribute => Some(b"&quot;"),
                '\n' if attribute => Some(b"&#10;"),
                '\r' if attribute => Some(b"&#13;"),
                '\t' if attribute => Some(b"&#09;"),
                ch if !matches!(ch, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}') => Some(b""),
                _ => None,
            };
            if let Some(replacement) = replacement {
                output.write_all(&value.as_bytes()[start..at])?;
                output.write_all(replacement)?;
                start = at + ch.len_utf8();
            }
        }
        output.write_all(&value.as_bytes()[start..])
    })
}

pub(crate) fn text_chunks(mut text: impl Read, mut visit: impl FnMut(&str) -> io::Result<()>) -> io::Result<()> {
    let mut buffer = [0u8; TEXT_CHUNK + 4];
    let mut pending = 0;
    loop {
        let got = match text.read(&mut buffer[pending..pending + TEXT_CHUNK]) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        let length = pending + got;
        let valid = match std::str::from_utf8(&buffer[..length]) {
            Ok(_) => length,
            Err(error) if error.error_len().is_none() && got != 0 => error.valid_up_to(),
            Err(_) => return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid UTF-8 in NZB text")),
        };
        let value = std::str::from_utf8(&buffer[..valid]).expect("validated UTF-8");
        visit(value)?;
        pending = length - valid;
        buffer.copy_within(valid..length, 0);
        if got == 0 {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{ArticleRow, ReleaseRow};

    // Removing incremental UTF-8 carry or XML escaping must fail this oracle.
    #[test]
    fn incremental_xml_matches_the_existing_renderer() {
        let release = ReleaseRow { group_name: "alt.binaries.test".into(), ..ReleaseRow::default() };
        let row = ArticleRow {
            message_id: "<a&b@example.test>".into(),
            filename: Some("a.bin".into()),
            part: Some(1),
            total_parts: Some(1),
            bytes: Some(17),
            subject: Some("a\"&<\n\0é.bin (1/1)".into()),
            poster: Some("p\"&@example.test".into()),
            posted_date: Some("2020-01-01 00:00:00".into()),
        };
        let expected = crate::nzb::render_checked(&release, std::slice::from_ref(&row), std::slice::from_ref(&release.group_name), true);
        let mut xml = XmlWriter::new(Vec::new(), release.group_name.as_bytes()).unwrap();
        xml.end_head().unwrap();
        xml.file(row.poster.as_deref().unwrap().as_bytes(), crate::dates::article_timestamp(row.posted_date.as_deref().unwrap()), row.subject.as_deref().unwrap().as_bytes()).unwrap();
        xml.group(release.group_name.as_bytes()).unwrap();
        xml.segments().unwrap();
        xml.segment(17, Some(1), b"a&b@example.test".as_slice()).unwrap();
        xml.end_file().unwrap();
        assert_eq!(String::from_utf8(xml.finish().unwrap()).unwrap(), expected.xml);
    }

    struct Tiny<R>(R);
    impl<R: Read> Read for Tiny<R> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.0.read(&mut out[..1])
        }
    }

    #[test]
    fn text_escapes_across_each_utf8_byte_boundary() {
        let mut out = Vec::new();
        escaped(&mut out, Tiny("é🙂&\0<".as_bytes()), false).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "é🙂&amp;&lt;");
        assert!(escaped(&mut Vec::new(), b"\xf0\x9f".as_slice(), false).is_err());
    }

    #[test]
    fn sorted_gap_check_matches_the_existing_renderer() {
        for parts in [
            vec![None, Some(-1), Some(1), Some(1), Some(3), Some(5)],
            vec![Some(0), Some(1), Some(1), Some(3)],
            vec![Some(1), Some(2), Some(3)],
            vec![],
        ] {
            for total in [None, Some(-1), Some(0), Some(3), Some(7), Some(crate::parser::MAX_PARTS + 1)] {
                let mut check = GapCheck::new(total);
                let rows: Vec<_> = parts.iter().map(|&part| {
                    check.push(part).unwrap();
                    ArticleRow { filename: Some("a.bin".into()), part, total_parts: total, message_id: String::new(), bytes: None, subject: None, poster: None, posted_date: None }
                }).collect();
                let actual = check.finish();
                let expected = crate::nzb::gaps(&rows).pop().map(|g| GapSummary { total: g.total, have: g.have, missing: g.missing, stray: g.stray });
                // An absent physical file has no gap; it is checked by whole-post file counters.
                assert_eq!(if rows.is_empty() { None } else { actual }, expected, "{parts:?} {total:?}");
            }
        }
    }

    #[test]
    fn unordered_segments_are_rejected() {
        let mut check = GapCheck::new(Some(2));
        check.push(Some(2)).unwrap();
        assert!(check.push(Some(1)).is_err());
    }

    #[test]
    fn incomplete_file_cannot_publish_a_finished_document() {
        let mut xml = XmlWriter::new(Vec::new(), b"alt.binaries.test".as_slice()).unwrap();
        xml.end_head().unwrap();
        xml.file(io::empty(), 0, io::empty()).unwrap();
        assert!(xml.finish().is_err());
    }

    #[test]
    fn failed_text_cannot_be_published_by_continuing_the_writer() {
        for phase in ["meta", "file", "segment"] {
            let mut xml = XmlWriter::new(Vec::new(), b"alt.binaries.test".as_slice()).unwrap();
            match phase {
                "meta" => {
                    assert!(xml.meta(false, b"\xff".as_slice()).is_err());
                    let _ = xml.end_head();
                }
                "file" => {
                    xml.end_head().unwrap();
                    assert!(xml.file(b"\xff".as_slice(), 0, io::empty()).is_err());
                }
                "segment" => {
                    xml.end_head().unwrap();
                    xml.file(io::empty(), 0, io::empty()).unwrap();
                    xml.segments().unwrap();
                    assert!(xml.segment(17, Some(1), b"\xff".as_slice()).is_err());
                    let _ = xml.end_file();
                }
                _ => unreachable!(),
            }
            assert!(xml.finish().is_err(), "ignored {phase} error must poison publication");
        }
    }

    #[derive(Default)]
    struct Count {
        bytes: u64,
        largest_write: usize,
    }
    impl Write for Count {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes += bytes.len() as u64;
            self.largest_write = self.largest_write.max(bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }

    #[test]
    fn one_file_with_250000_parts_does_not_retain_article_rows() {
        let mut xml = XmlWriter::new(Count::default(), b"alt.binaries.test".as_slice()).unwrap();
        xml.end_head().unwrap();
        xml.file(io::empty(), 0, b"a.bin".as_slice()).unwrap();
        xml.group(b"alt.binaries.test".as_slice()).unwrap();
        xml.segments().unwrap();
        let mut gaps = GapCheck::new(Some(250_000));
        for part in 1..=250_000 {
            gaps.push(Some(part)).unwrap();
            xml.segment(17, Some(part), b"part@example.test".as_slice()).unwrap();
        }
        assert_eq!(gaps.finish(), None);
        xml.end_file().unwrap();
        let output = xml.finish().unwrap();
        assert!(output.bytes > 15_000_000);
        assert!(output.largest_write <= TEXT_CHUNK);
    }

    #[test]
    fn ten_thousand_files_are_emitted_without_a_file_vector() {
        let mut xml = XmlWriter::new(Count::default(), b"alt.binaries.test".as_slice()).unwrap();
        xml.end_head().unwrap();
        for _ in 0..10_000 {
            xml.file(io::empty(), 0, io::empty()).unwrap();
            xml.group(b"alt.binaries.test".as_slice()).unwrap();
            xml.segments().unwrap();
            xml.segment(17, Some(1), b"part@example.test".as_slice()).unwrap();
            xml.end_file().unwrap();
        }
        let output = xml.finish().unwrap();
        assert!(output.bytes > 2_000_000);
        assert!(output.largest_write <= TEXT_CHUNK);
    }

    #[test]
    fn a_giant_text_field_is_transferred_in_bounded_chunks() {
        let mut xml = XmlWriter::new(Count::default(), b"alt.binaries.test".as_slice()).unwrap();
        xml.end_head().unwrap();
        xml.file(io::empty(), 0, io::repeat(b'x').take(8 * 1024 * 1024)).unwrap();
        xml.segments().unwrap();
        xml.segment(17, Some(1), io::repeat(b'x').take(8 * 1024 * 1024)).unwrap();
        xml.end_file().unwrap();
        let output = xml.finish().unwrap();
        assert!(output.bytes > 16 * 1024 * 1024);
        assert!(output.largest_write <= TEXT_CHUNK);
    }

    #[test]
    fn file_lists_stream_ten_thousand_names_and_a_giant_name() {
        let mut list = FilesWriter::new(Count::default(), 7, 10_001).unwrap();
        for _ in 0..10_000 {
            list.file(Some(b"same.bin #7".as_slice()), 17, 2).unwrap();
        }
        list.file(Some(io::repeat(b'x').take(8 * 1024 * 1024)), 23, 1).unwrap();
        let output = list.finish().unwrap();
        assert!(output.bytes > 8 * 1024 * 1024);
        assert!(output.largest_write <= TEXT_CHUNK);
    }

    #[test]
    fn failed_or_incomplete_file_lists_cannot_be_published() {
        let mut list = FilesWriter::new(Vec::new(), 7, 1).unwrap();
        assert!(list.file(Some(b"\xff".as_slice()), 17, 1).is_err());
        assert!(list.file(None::<io::Empty>, 17, 1).is_err());
        assert!(list.finish().is_err());
        let list = FilesWriter::new(Vec::new(), 7, 1).unwrap();
        assert!(list.finish().is_err());
        let mut list = FilesWriter::new(Vec::new(), 7, 0).unwrap();
        assert!(list.file(None::<io::Empty>, 17, 1).is_err());
        assert!(list.finish().is_err());
    }
}

