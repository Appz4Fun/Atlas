//! BODY ownership is bounded independently of overview/header admission.
use super::*;

const LIMIT_MESSAGE: &str = "NNTP body exceeds article admission";
pub(super) fn limit_error() -> NntpError {
    NntpError::Protocol(LIMIT_MESSAGE.into())
}
pub(super) fn is_limit(error: &NntpError) -> bool {
    matches!(error,NntpError::Protocol(message) if message==LIMIT_MESSAGE)
}
pub(super) fn default_limit() -> usize {
    crate::config::PostprocessConfig::default().body_max_bytes as usize
}

/// Incremental equivalent of yenc_decode's line grammar. Advertised size,
/// begin/end offsets and filenames never participate in a reserve operation.
pub(super) struct Decoder {
    out: Vec<u8>,
    limit: usize,
    yenc: bool,
    ended: bool,
    lines: usize,
}
impl Decoder {
    pub(super) fn new(limit: usize) -> Self {
        Self { out: Vec::new(), limit, yenc: false, ended: false, lines: 0 }
    }
    pub(super) fn line(&mut self, line: &[u8]) -> bool {
        if !self.yenc && line.starts_with(b"=ybegin ") {
            self.out.clear();
            self.yenc = true;
            return true;
        }
        if self.yenc {
            if self.ended || line.starts_with(b"=ypart ") {
                return true;
            }
            if line.starts_with(b"=yend") {
                self.ended = true;
                return true;
            }
            let mut escaped = false;
            let mut bytes = 0usize;
            for &b in line {
                if escaped {
                    bytes += 1;
                    escaped = false;
                } else if b == b'=' {
                    escaped = true;
                } else if b != b'\r' && b != b'\n' {
                    bytes += 1;
                }
            }
            if bytes > self.limit.saturating_sub(self.out.len()) {
                return false;
            }
            self.out.reserve_exact(bytes);
            let mut escaped = false;
            for &b in line {
                if escaped {
                    self.out.push(b.wrapping_sub(64).wrapping_sub(42));
                    escaped = false;
                } else if b == b'=' {
                    escaped = true;
                } else if b != b'\r' && b != b'\n' {
                    self.out.push(b.wrapping_sub(42));
                }
            }
        } else {
            let separator = usize::from(self.lines != 0);
            if line.len().checked_add(separator).is_none_or(|n| n > self.limit.saturating_sub(self.out.len())) {
                return false;
            }
            self.out.reserve_exact(line.len() + separator);
            if separator != 0 {
                self.out.push(b'\n');
            }
            self.out.extend_from_slice(line);
            self.lines += 1;
        }
        true
    }
    pub(super) fn finish(self) -> Vec<u8> {
        self.out
    }
}
impl Conn {
    /// Calls the consumer only after per-line, aggregate encoded bytes and
    /// row metadata admission. No Vec<Vec> or joined-body copy is retained.
    pub(super) async fn body_lines(
        &mut self,
        status: &str,
        max: usize,
        rows_max: usize,
        mut line: impl FnMut(&[u8]) -> bool,
    ) -> Result<bool> {
        let result = self.body_lines_in(status, max, rows_max, &mut line).await;
        if result.is_err() || matches!(result, Ok(false)) {
            self.poisoned = true;
        }
        result
    }
    async fn body_lines_in(
        &mut self,
        status: &str,
        max: usize,
        rows_max: usize,
        line: &mut impl FnMut(&[u8]) -> bool,
    ) -> Result<bool> {
        let mut rows = 0usize;
        let mut encoded = 0usize;
        if !status.to_ascii_uppercase().contains("COMPRESS=GZIP") {
            loop {
                // One byte for dot-stuffing or a terminator after an exactly
                // full payload; read_line_capped separately admits only CRLF.
                let cap = max.saturating_sub(encoded).saturating_add(1).max(1);
                let mut bytes = self.read_line_capped(cap).await?.ok_or_else(limit_error)?;
                if bytes == b"." {
                    return Ok(true);
                }
                if bytes.starts_with(b"..") {
                    bytes.remove(0);
                }
                if bytes.len() > max.saturating_sub(encoded) || rows == rows_max {
                    return Err(limit_error());
                }
                encoded += bytes.len();
                rows += 1;
                if !line(&bytes) {
                    return Ok(false);
                }
            }
        }
        let mut frame = Frame::new(max, rows_max)?;
        let mut inflater = Inflater::default();
        let mut wire = 0usize;
        loop {
            if frame.terminated && inflater.finished() {
                return Ok(true);
            }
            let chunk = if frame.terminated {
                match self.read_chunk(STREAM_END_WAIT).await {
                    Err(error) if error.is_timeout() => {
                        self.poisoned = true;
                        return Ok(true);
                    }
                    result => result?,
                }
            } else {
                self.read_chunk(self.timeout).await?
            };
            wire = wire.checked_add(chunk.len()).ok_or_else(limit_error)?;
            if wire > frame.max_expanded.saturating_add(64 * 1024) {
                return Err(limit_error());
            }
            let before = frame.expanded;
            if !inflater.feed_body(&chunk, &mut frame, line)? {
                return Ok(false);
            }
            self.text += (frame.expanded - before) as u64;
        }
    }
    pub(super) async fn decoded_body(&mut self, status: &str, max: usize, prefix: bool) -> Result<(Vec<u8>, bool)> {
        let mut decoder = Decoder::new(max);
        let mut prefix_wire = 0usize;
        let result = self
            .body_lines(status, max, if prefix { max / 2 + 1 } else { max / 16 + 1 }, |line| {
                if prefix {
                    let Some(next) = prefix_wire.checked_add(line.len()).and_then(|n| n.checked_add(2)) else {
                        return false;
                    };
                    if next > max.saturating_add(2) {
                        return false;
                    }
                    prefix_wire = next;
                }
                let accepted = decoder.line(line);
                accepted && (!prefix || prefix_wire < max)
            })
            .await;
        match result {
            Ok(whole) => Ok((decoder.finish(), whole)),
            Err(error) if prefix && is_limit(&error) => Ok((decoder.finish(), false)),
            Err(error) => Err(error),
        }
    }
    pub(super) async fn raw_body(&mut self, status: &str, max: usize, prefix: bool) -> Result<(Vec<u8>, bool)> {
        let mut out = Vec::new();
        let result = self
            .body_lines(status, max, max.saturating_add(1), |line| {
                if line.len().checked_add(1).is_none_or(|n| n > max.saturating_sub(out.len())) {
                    return false;
                }
                out.reserve_exact(line.len() + 1);
                out.extend_from_slice(line);
                out.push(b'\n');
                true
            })
            .await;
        match result {
            Ok(whole) => Ok((out, whole)),
            Err(error) if prefix && is_limit(&error) => Ok((out, false)),
            Err(error) => Err(error),
        }
    }
}

/// Bound the compatibility/AI aggregate before Vec growth or leaf retention.
/// Active indexing uses stream_headers and does not call this collector.
pub(super) fn append_headers(out: &mut Vec<Overview>, rows: Vec<Overview>, owned: &mut usize) -> Result<()> {
    let mut next = *owned;
    for row in &rows {
        let text = [&row.subject, &row.from, &row.date, &row.message_id, &row.references]
            .into_iter()
            .try_fold(0usize, |sum, value| sum.checked_add(value.capacity()))
            .ok_or_else(overview_budget)?;
        let charge = text.checked_add(4 * std::mem::size_of::<Overview>()).ok_or_else(overview_budget)?;
        next = next.checked_add(charge).ok_or_else(overview_budget)?;
        if next > crate::nxv_store::store::FETCH_HEADROOM {
            return Err(overview_budget());
        }
    }
    out.reserve_exact(rows.len());
    out.extend(rows);
    *owned = next;
    Ok(())
}

/// Parse each fixed inflater scratch fragment before retaining another one.
/// Even a malformed unterminated line cannot use the article framing allowance
/// as extra line capacity; only dot-stuffing and CRLF add three bytes.
struct Frame {
    max: usize,
    rows_max: usize,
    encoded: usize,
    rows: usize,
    expanded: usize,
    max_expanded: usize,
    line: Vec<u8>,
    terminated: bool,
}
impl Frame {
    fn new(max: usize, rows_max: usize) -> Result<Self> {
        let max_expanded = max
            .checked_add(rows_max.checked_mul(3).ok_or_else(limit_error)?)
            .and_then(|n| n.checked_add(3))
            .ok_or_else(limit_error)?;
        Ok(Self { max, rows_max, encoded: 0, rows: 0, expanded: 0, max_expanded, line: Vec::new(), terminated: false })
    }
    fn push(&mut self, bytes: &[u8], consumer: &mut impl FnMut(&[u8]) -> bool) -> Result<bool> {
        self.expanded = self.expanded.checked_add(bytes.len()).ok_or_else(limit_error)?;
        if self.expanded > self.max_expanded {
            return Err(limit_error());
        }
        if self.terminated {
            return Ok(true);
        }
        let mut rest = bytes;
        while !rest.is_empty() {
            let ended = rest.iter().position(|b| *b == b'\n');
            let used = ended.map_or(rest.len(), |at| at + 1);
            let cap = self.max.saturating_sub(self.encoded).saturating_add(3);
            if used > cap.saturating_sub(self.line.len()) {
                return Err(limit_error());
            }
            self.line.reserve_exact(used);
            self.line.extend_from_slice(&rest[..used]);
            rest = &rest[used..];
            if ended.is_none() {
                continue;
            }
            self.line.pop();
            if self.line.last() == Some(&b'\r') {
                self.line.pop();
            }
            if self.line == b"." {
                self.line.clear();
                self.terminated = true;
                return Ok(true);
            }
            let raw = if self.line.starts_with(b"..") { &self.line[1..] } else { &self.line[..] };
            if raw.len() > self.max.saturating_sub(self.encoded) || self.rows == self.rows_max {
                return Err(limit_error());
            }
            self.encoded += raw.len();
            self.rows += 1;
            if !consumer(raw) {
                return Ok(false);
            }
            self.line.clear();
        }
        Ok(true)
    }
}
impl Inflater {
    /// BODY-only event adapter; the shared overview inflater and its limits
    /// remain unchanged. No advertised yEnc size or compression ratio reserves.
    fn feed_body(&mut self, input: &[u8], frame: &mut Frame, consumer: &mut impl FnMut(&[u8]) -> bool) -> Result<bool> {
        if let InflateState::Start = self.state {
            if input.len() > frame.max_expanded.saturating_sub(self.head.len()) {
                return Err(limit_error());
            }
            self.head.reserve_exact(input.len());
            self.head.extend_from_slice(input);
            let head = std::mem::take(&mut self.head);
            if head.len() < 2
                || (head[0] == b'.' && head.len() < 3 && !head.ends_with(b"\n"))
                || (head.iter().all(|b| b.is_ascii_digit()) && head.len() < 20)
            {
                self.head = head;
                return Ok(true);
            }
            let digits = head.iter().take_while(|b| b.is_ascii_digit()).count();
            if head.starts_with(b".\r\n")
                || head.starts_with(b".\n")
                || (digits > 0 && head.get(digits) == Some(&b'\t'))
            {
                self.state = InflateState::Done;
                return frame.push(&head, consumer);
            }
            let (decompress, gzip, skip) = if head.starts_with(&[0x1f, 0x8b]) {
                match gzip_header_len(&head) {
                    None => {
                        self.head = head;
                        return Ok(true);
                    }
                    Some(Err(error)) => return Err(error),
                    Some(Ok(skip)) => (flate2::Decompress::new(false), true, skip),
                }
            } else if head[0] & 0x0f == 8
                && (head[0] >> 4) <= 7
                && (head[1] & 0x20) == 0
                && ((u16::from(head[0]) << 8) | u16::from(head[1])) % 31 == 0
            {
                (flate2::Decompress::new(true), false, 0)
            } else {
                (flate2::Decompress::new(false), false, 0)
            };
            self.state = InflateState::Running(Box::new(decompress), gzip);
            return self.feed_body(&head[skip..], frame, consumer);
        }
        let mut rest = input;
        if let InflateState::Running(decompress, gzip) = &mut self.state {
            let gzip = *gzip;
            loop {
                let mut scratch = [0u8; 64 * 1024];
                let (before_in, before_out) = (decompress.total_in(), decompress.total_out());
                let status = decompress
                    .decompress(rest, &mut scratch, flate2::FlushDecompress::None)
                    .map_err(|error| NntpError::Decompress(error.to_string()))?;
                rest = &rest[(decompress.total_in() - before_in) as usize..];
                let produced = (decompress.total_out() - before_out) as usize;
                if !frame.push(&scratch[..produced], consumer)? {
                    return Ok(false);
                }
                if status == flate2::Status::StreamEnd {
                    self.state = InflateState::Done;
                    self.trailer_left = if gzip { 8 } else { 0 };
                    break;
                }
                if produced == 0 {
                    if rest.is_empty() {
                        return Ok(true);
                    }
                    if decompress.total_in() == before_in {
                        return Err(NntpError::Decompress("stuck".into()));
                    }
                }
            }
        }
        let skip = self.trailer_left.min(rest.len());
        self.trailer_left -= skip;
        frame.push(&rest[skip..], consumer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn compressed_unterminated_line_never_retains_past_payload_plus_wire_framing() {
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::none());
        encoder.write_all(&[b'x'; 80]).unwrap();
        let wire = encoder.finish().unwrap();
        let mut inflater = Inflater::default();
        let mut frame = Frame::new(32, 33).unwrap();
        let mut seen = 0;
        let mut refused = false;
        let mut callback = |_: &[u8]| -> bool { panic!("an unterminated oversized line cannot reach a consumer") };
        for byte in wire.chunks(1) {
            let result = inflater.feed_body(byte, &mut frame, &mut callback);
            seen = seen.max(frame.line.capacity());
            assert!(
                frame.line.len() <= 35 && frame.line.capacity() <= 35,
                "only dot-stuffing and CRLF may exceed the admitted payload"
            );
            if let Err(error) = result {
                assert!(is_limit(&error));
                refused = true;
                break;
            }
        }
        assert!(seen > 0, "the test must observe actual partial-line retention before refusal");
        assert!(refused);
    }
    #[test]
    fn compressed_multiline_and_plain_terminator_after_stream_preserve_exact_rows() {
        for inside in [true, false] {
            let text = if inside { b"a\r\n..b\r\n.\r\n".as_slice() } else { b"a\r\n..b\r\n".as_slice() };
            let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(text).unwrap();
            let mut wire = encoder.finish().unwrap();
            if !inside {
                wire.extend(b".\r\n");
            }
            let mut inflater = Inflater::default();
            let mut frame = Frame::new(8, 9).unwrap();
            let mut rows = Vec::new();
            let mut callback = |line: &[u8]| {
                rows.push(line.to_vec());
                true
            };
            for bytes in wire.chunks(1) {
                assert!(inflater.feed_body(bytes, &mut frame, &mut callback).unwrap());
            }
            assert!(frame.terminated && inflater.finished());
            assert_eq!(rows, [b"a".to_vec(), b".b".to_vec()]);
        }
    }
}
