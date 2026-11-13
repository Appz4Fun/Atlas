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
        // The same encoded article policy also bounds decompressed framing.
        // Fixed inflater scratch is inherited; no compression-ratio reserve.
        let text_max = max
            .checked_add(rows_max.checked_mul(2).ok_or_else(limit_error)?)
            .and_then(|n| n.checked_add(3))
            .ok_or_else(limit_error)?;
        let mut inflater = Inflater::default();
        let mut text = Vec::new();
        let mut expanded = 0usize;
        let mut wire = 0usize;
        let mut terminated = false;
        loop {
            if !terminated {
                let mut scanned = 0usize;
                while let Some(nl) = text[scanned..].iter().position(|b| *b == b'\n') {
                    let raw = &text[scanned..scanned + nl];
                    let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
                    scanned += nl + 1;
                    if raw == b"." {
                        terminated = true;
                        break;
                    }
                    let raw = if raw.starts_with(b"..") { &raw[1..] } else { raw };
                    if raw.len() > max.saturating_sub(encoded) || rows == rows_max {
                        return Err(limit_error());
                    }
                    encoded += raw.len();
                    rows += 1;
                    self.text += raw.len() as u64 + 2;
                    if !line(raw) {
                        return Ok(false);
                    }
                }
                text.drain(..scanned);
                if !terminated && text.len() > max.saturating_sub(encoded).saturating_add(2) {
                    return Err(limit_error());
                }
            }
            if terminated && inflater.finished() {
                return Ok(true);
            }
            let chunk = if terminated {
                match self.read_chunk(STREAM_END_WAIT).await {
                    Err(e) if e.is_timeout() => {
                        self.poisoned = true;
                        return Ok(true);
                    }
                    r => r?,
                }
            } else {
                self.read_chunk(self.timeout).await?
            };
            wire = wire.checked_add(chunk.len()).ok_or_else(limit_error)?;
            if wire > text_max.saturating_add(64 * 1024) {
                return Err(limit_error());
            }
            let before = text.len();
            let ceiling = before.checked_add(text_max.saturating_sub(expanded)).ok_or_else(limit_error)?;
            let plain = inflater
                .feed_bounded(&chunk, &mut text, ceiling)
                .map_err(|e| if e.is_overview_budget() { limit_error() } else { e })?;
            let added =
                text.len().checked_sub(before).and_then(|n| n.checked_add(plain.len())).ok_or_else(limit_error)?;
            if added > text_max.saturating_sub(expanded) {
                return Err(limit_error());
            }
            expanded += added;
            if !terminated {
                text.reserve_exact(plain.len());
                text.extend_from_slice(&plain);
            }
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
