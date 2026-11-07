//! Release discovery, posting tool classification, server Message-ID decoders,
//! and authoritative arrival clock reconstruction.

use std::collections::HashMap;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use regex::Regex;

/// Posting tools identified by Message-ID syntax and headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolType {
    Nyuu,
    NgPost,
    Postie,
    NewsUp,
    NxgUpper,
    GoPostStuff,
    Unknown,
}

/// Information extracted from Message-ID grammars and headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInfo {
    pub tool: ToolType,
    pub version: Option<String>,
    pub embedded_time_ms: Option<u64>,
    pub deterministic: bool,
}

pub type HeaderMap = HashMap<String, String>;

static NYUU_TIMESTAMP_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^[a-z0-9_-]+-(\d{10,13})@.*$").unwrap());
static NYUU_RANDOM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9a-zA-Z]{24,32}@nyuu$").unwrap());
static NGPOST_UUID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}@[a-z0-9.-]+$").unwrap()
});
static POSTIE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9]{32}@[a-z0-9]{8}\.[a-z0-9]{3}$").unwrap());
static NXG_UPPER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^[0-9a-f]{40}@[0-9a-f]{21}\.3$").unwrap());
static NEWSUP_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^[a-z0-9]{20,24}@[a-z0-9.-]+$").unwrap());
static GOPOSTSTUFF_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^gps-[a-z0-9]+-(\d{10,19})@.*$").unwrap());
static GOPOSTSTUFF_ALT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(\d{10,19})\$gps@.*$").unwrap());

pub struct PostingTool;

impl PostingTool {
    /// Classify posting tool from Message-ID syntax and RFC headers.
    pub fn classify(message_id: &str, headers: &HeaderMap) -> ToolInfo {
        let mid = message_id.trim().trim_start_matches('<').trim_end_matches('>');
        if mid.len() > 4096 || !mid.contains('@') || !mid.is_ascii() {
            return ToolInfo { tool: ToolType::Unknown, version: None, embedded_time_ms: None, deterministic: false };
        }

        let (_local, domain) = match mid.split_once('@') {
            Some((l, d)) if !l.is_empty() && !d.is_empty() => (l, d),
            _ => {
                return ToolInfo {
                    tool: ToolType::Unknown,
                    version: None,
                    embedded_time_ms: None,
                    deterministic: false,
                };
            }
        };

        // 1. nxg-upper check via header or SHA256 grammar
        let has_nxg_header = headers.keys().any(|k| k.eq_ignore_ascii_case("x-nxg"));
        if has_nxg_header || NXG_UPPER_RE.is_match(mid) {
            return ToolInfo { tool: ToolType::NxgUpper, version: None, embedded_time_ms: None, deterministic: true };
        }

        // 2. GoPostStuff grammar (checked before Nyuu timestamp since gps-<id>-<ts>@... is a subset)
        if let Some(caps) = GOPOSTSTUFF_RE.captures(mid).or_else(|| GOPOSTSTUFF_ALT_RE.captures(mid)) {
            let ts: Option<u64> =
                caps.get(1).and_then(|m| m.as_str().parse::<u64>().ok()).map(Self::normalize_timestamp_to_ms);
            return ToolInfo { tool: ToolType::GoPostStuff, version: None, embedded_time_ms: ts, deterministic: false };
        }
        if domain.to_ascii_lowercase().contains("gopoststuff") {
            let ts = NYUU_TIMESTAMP_RE
                .captures(mid)
                .and_then(|caps| caps.get(1))
                .and_then(|m| m.as_str().parse::<u64>().ok())
                .map(Self::normalize_timestamp_to_ms);
            return ToolInfo { tool: ToolType::GoPostStuff, version: None, embedded_time_ms: ts, deterministic: false };
        }

        // 3. Nyuu timestamp grammar
        if let Some(caps) = NYUU_TIMESTAMP_RE.captures(mid) {
            let ms: Option<u64> = caps.get(1).and_then(|m| m.as_str().parse().ok());
            return ToolInfo { tool: ToolType::Nyuu, version: None, embedded_time_ms: ms, deterministic: false };
        }

        // Nyuu random grammar
        if NYUU_RANDOM_RE.is_match(mid) || domain.eq_ignore_ascii_case("nyuu") {
            return ToolInfo { tool: ToolType::Nyuu, version: None, embedded_time_ms: None, deterministic: false };
        }

        // 4. ngPost UUIDv4 grammar
        if NGPOST_UUID_RE.is_match(mid)
            || domain.eq_ignore_ascii_case("ngpost")
            || domain.eq_ignore_ascii_case("ngpost.com")
        {
            return ToolInfo { tool: ToolType::NgPost, version: None, embedded_time_ms: None, deterministic: false };
        }

        // 5. Postie grammar
        if POSTIE_RE.is_match(mid) {
            return ToolInfo { tool: ToolType::Postie, version: None, embedded_time_ms: None, deterministic: false };
        }

        // 6. NewsUP grammar
        if mid.to_ascii_lowercase().contains("newsup") || NEWSUP_RE.is_match(mid) {
            return ToolInfo { tool: ToolType::NewsUp, version: None, embedded_time_ms: None, deterministic: false };
        }

        ToolInfo { tool: ToolType::Unknown, version: None, embedded_time_ms: None, deterministic: false }
    }

    /// Normalize a 10–19 digit Unix timestamp (seconds, milliseconds, microseconds,
    /// or nanoseconds) into Unix milliseconds for `embedded_time_ms`.
    pub fn normalize_timestamp_to_ms(raw: u64) -> u64 {
        if raw >= 100_000_000_000_000_000 {
            // 18–19 digits: Unix nanoseconds -> milliseconds
            raw / 1_000_000
        } else if raw >= 100_000_000_000_000 {
            // 15–17 digits: Unix microseconds -> milliseconds
            raw / 1_000
        } else if raw < 100_000_000_000 {
            // 10–11 digits: Unix seconds -> milliseconds
            raw.saturating_mul(1_000)
        } else {
            // 12–14 digits: Unix milliseconds
            raw
        }
    }
}

/// What one message-id says of its article, for `file_timing`: the tool that made it, the time the tool or the server
/// wrote into it, and the Highwinds fields that tie one posting session's articles together.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdFacts {
    pub tool: Option<ToolType>,
    /// milliseconds since the epoch: the tool's clock (Nyuu, GoPostStuff) or else the server's (Highwinds, Giganews,
    /// Diablo, INN), never the poster's Date header
    pub clock_ms: Option<i64>,
    /// `T.N$K.R@host` on Highwinds: the host, the session key `K`, and the per-session counter `N`
    pub hw_host: Option<String>,
    pub hw_key: Option<String>,
    pub hw_n: Option<i64>,
}

impl ToolType {
    /// A small number for a database column, 0 for none known.
    pub fn code(self) -> i64 {
        match self {
            ToolType::Unknown => 0,
            ToolType::Nyuu => 1,
            ToolType::NgPost => 2,
            ToolType::Postie => 3,
            ToolType::NewsUp => 4,
            ToolType::NxgUpper => 5,
            ToolType::GoPostStuff => 6,
        }
    }

    pub fn from_code(code: i64) -> Option<ToolType> {
        Some(match code {
            1 => ToolType::Nyuu,
            2 => ToolType::NgPost,
            3 => ToolType::Postie,
            4 => ToolType::NewsUp,
            5 => ToolType::NxgUpper,
            6 => ToolType::GoPostStuff,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            ToolType::Unknown => "unknown",
            ToolType::Nyuu => "Nyuu",
            ToolType::NgPost => "ngPost",
            ToolType::Postie => "Postie",
            ToolType::NewsUp => "NewsUP",
            ToolType::NxgUpper => "nxg-upper",
            ToolType::GoPostStuff => "GoPostStuff",
        }
    }
}

/// Nyuu as it posts by default: letters, then the millisecond it made the id, and nothing between.
static NYUU_PLAIN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z]{12,40}(1[0-9]{12})@").unwrap());

/// The facts one message-id gives, with no headers to go by. Cheap: a few anchored regexes.
pub fn id_facts(message_id: &str) -> IdFacts {
    let mid = message_id.trim().trim_start_matches('<').trim_end_matches('>');
    let mut facts = IdFacts::default();
    if mid.len() > 1024 || !mid.is_ascii() {
        return facts;
    }
    let Some((local, domain)) = mid.split_once('@') else { return facts };
    if local.is_empty() || domain.is_empty() {
        return facts;
    }
    let info = PostingTool::classify(mid, &HeaderMap::new());
    let mut tool = info.tool;
    let mut clock = info.embedded_time_ms.and_then(|ms| i64::try_from(ms).ok());
    if clock.is_none()
        && let Some(caps) = NYUU_PLAIN_RE.captures(mid)
    {
        tool = ToolType::Nyuu;
        clock = caps.get(1).and_then(|m| m.as_str().parse().ok());
    }
    if tool != ToolType::Unknown {
        facts.tool = Some(tool);
    }
    if let Some(hw) = highwinds_fields(local, domain) {
        facts.hw_host = Some(hw.host);
        facts.hw_key = Some(hw.key);
        facts.hw_n = Some(hw.n);
    }
    if clock.is_none() {
        clock = ServerClock::decode(mid).map(|dt| dt.timestamp_millis());
    }
    facts.clock_ms = clock.filter(|ms| *ms > 0);
    facts
}

/// `T.N$K.R@fxNN.site`: the server's time `T`, its counter `N`, the session key `K` and `R`, on a Highwinds host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighwindsId {
    pub host: String,
    pub key: String,
    pub n: i64,
}

fn highwinds_fields(local: &str, domain: &str) -> Option<HighwindsId> {
    let host = domain.to_ascii_lowercase();
    if !host.starts_with("fx") || host.len() > 32 {
        return None;
    }
    let (left, right) = local.split_once('$')?;
    let (t, n) = left.split_once('.')?;
    let (key, r) = right.split_once('.')?;
    if t.is_empty() || key.is_empty() || key.len() > 16 || !r.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i64 = n.parse().ok()?;
    Some(HighwindsId { host, key: key.to_string(), n })
}

pub struct ServerClock;

impl ServerClock {
    /// Parse an NNTP `DATE` response (`111 YYYYMMDDhhmmss` or raw `YYYYMMDDhhmmss`) into UTC.
    ///
    /// Empirical probe (2026-11-03): all 8 commercial providers support `DATE` (`111`),
    /// whereas `NEWNEWS` is disabled everywhere (`400`/`500`/`502`) and `XHDR`/`HDR`
    /// returns no values for `NNTP-Posting-Date` or `Injection-Info`.
    pub fn parse_nntp_date(response: &str) -> Option<DateTime<Utc>> {
        use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
        let trimmed = response.trim();
        let token = match trimmed.strip_prefix("111") {
            Some(rest) => rest.split_whitespace().next()?,
            None => trimmed.split_whitespace().next()?,
        };
        if token.len() != 14 || !token.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let year: i32 = token[0..4].parse().ok()?;
        let month: u32 = token[4..6].parse().ok()?;
        let day: u32 = token[6..8].parse().ok()?;
        let hour: u32 = token[8..10].parse().ok()?;
        let min: u32 = token[10..12].parse().ok()?;
        let sec: u32 = token[12..14].parse().ok()?;

        let date = NaiveDate::from_ymd_opt(year, month, day)?;
        let time = NaiveTime::from_hms_opt(hour, min, sec)?;
        Some(NaiveDateTime::new(date, time).and_utc())
    }

    /// Measure per-provider clock skew in fractional seconds (`server_utc - local_utc`).
    ///
    /// Positive values indicate the server clock runs ahead of UTC (e.g., Giganews
    /// `super.newsgroupdirect.com` measured at `+5.74 s` ahead on 2026-11-03).
    pub fn measure_skew_seconds(nntp_date_response: &str, local_utc: DateTime<Utc>) -> Option<f64> {
        let server_dt = Self::parse_nntp_date(nntp_date_response)?;
        let server_sec = server_dt.timestamp() as f64 + f64::from(server_dt.timestamp_subsec_nanos()) * 1e-9;
        let local_sec = local_utc.timestamp() as f64 + f64::from(local_utc.timestamp_subsec_nanos()) * 1e-9;
        Some(server_sec - local_sec)
    }

    /// Correct a server-derived arrival timestamp (seconds) by subtracting the provider's clock skew.
    pub fn correct_skew(server_timestamp_sec: f64, skew_seconds: f64) -> f64 {
        server_timestamp_sec - skew_seconds
    }

    /// Decode server-assigned injection timestamps from Message-ID local parts.
    pub fn decode(message_id: &str) -> Option<DateTime<Utc>> {
        let mid = message_id.trim().trim_start_matches('<').trim_end_matches('>');
        if mid.len() > 1024 || !mid.contains('@') || !mid.is_ascii() {
            return None;
        }

        let now_sec = Utc::now().timestamp();
        // Allow maximum 1 day in the future for clock skew
        let max_future = now_sec + 86_400;

        let (local, domain) = mid.split_once('@')?;

        // 1. Highwinds: <T.N$S.R@fxNN.site>
        if (domain.starts_with("fx") || (local.contains('.') && local.contains('$')))
            && let Some(dt) = Self::decode_highwinds(local)
            && (876_645_500..=max_future).contains(&dt.timestamp())
        {
            return Some(dt);
        }

        // 2. Giganews: <32-char-base64@giganews.com>
        if (domain.eq_ignore_ascii_case("giganews.com") || (domain.contains("giganews") && local.len() == 32))
            && let Some(dt) = Self::decode_giganews(local)
            && (900_000_000..=max_future).contains(&dt.timestamp())
        {
            return Some(dt);
        }

        // 3. Diablo: <hex$seq$pid$hosthash@cluster>
        if local.contains('$')
            && let Some(dt) = Self::decode_diablo(local)
            && (900_000_000..=max_future).contains(&dt.timestamp())
        {
            return Some(dt);
        }

        // 4. INN: <radix32(t)$radix32(pid)$count@host>
        if local.contains('$')
            && let Some(dt) = Self::decode_inn(local)
            && (673_416_000..=max_future).contains(&dt.timestamp())
        {
            return Some(dt);
        }

        None
    }

    /// Decode Highwinds little-endian base64 timestamp offset from 876,645,500.
    pub fn decode_highwinds(local: &str) -> Option<DateTime<Utc>> {
        let t_str = local.split('.').next()?;
        if t_str.is_empty() || t_str.len() > 10 {
            return None;
        }

        // Highwinds custom alphabet: 0-9 (0..9), a-z (10..35), A-Z (36..61), _ (62), % (63)
        let decode_symbol_hw = |c: char| -> Option<u64> {
            match c {
                '0'..='9' => Some((c as u64) - ('0' as u64)),
                'a'..='z' => Some((c as u64) - ('a' as u64) + 10),
                'A'..='Z' => Some((c as u64) - ('A' as u64) + 36),
                '_' => Some(62),
                '%' => Some(63),
                _ => None,
            }
        };

        const HW_EPOCH: i64 = 876_645_500; // Oct 12, 1997

        let mut hw_val = 0u64;
        let mut factor = 1u64;
        for c in t_str.chars() {
            let sym = decode_symbol_hw(c)?;
            hw_val = hw_val.checked_add(sym.checked_mul(factor)?)?;
            factor = factor.saturating_mul(64);
        }

        let ts = HW_EPOCH.checked_add(i64::try_from(hw_val).ok()?)?;
        if (HW_EPOCH..=2_200_000_000).contains(&ts) { DateTime::from_timestamp(ts, 0) } else { None }
    }

    /// Decode Giganews 32-character base64 local part with XOR-0x9D mask.
    pub fn decode_giganews(local: &str) -> Option<DateTime<Utc>> {
        use base64::Engine;
        if local.len() < 32 {
            return None;
        }

        let b64_str = &local[..32];
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64_str)
            .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(b64_str))
            .ok()?;

        if bytes.len() != 24 {
            return None;
        }

        let mut unpacked = [0u8; 24];
        for (i, &b) in bytes.iter().enumerate() {
            unpacked[i] = b ^ 0x9D;
        }

        // w2 is at bytes 8..12 (little-endian u32)
        let w2 = u32::from_le_bytes(unpacked[8..12].try_into().ok()?);
        if (900_000_000..=2_500_000_000).contains(&w2) { DateTime::from_timestamp(w2 as i64, 0) } else { None }
    }

    /// Decode Diablo server Message-ID: <hex$seq$pid$hosthash@cluster>
    pub fn decode_diablo(local: &str) -> Option<DateTime<Utc>> {
        let parts: Vec<&str> = local.split('$').collect();
        if parts.len() < 3 {
            return None;
        }
        let hex_part = parts[0];
        if hex_part.len() != 8 && hex_part.len() != 7 {
            return None;
        }
        let ts = i64::from_str_radix(hex_part, 16).ok()?;
        if (900_000_000..=2_500_000_000).contains(&ts) { DateTime::from_timestamp(ts, 0) } else { None }
    }

    /// Decode INN radix32 Message-ID: <radix32(t)$radix32(pid)$count@host>
    pub fn decode_inn(local: &str) -> Option<DateTime<Utc>> {
        let parts: Vec<&str> = local.split('$').collect();
        if parts.len() < 3 {
            return None;
        }
        let r32_part = parts[0];
        if r32_part.is_empty() || r32_part.len() > 8 {
            return None;
        }
        let offset = u64::from_str_radix(r32_part, 32).ok()?;
        const INN_EPOCH: i64 = 673_416_000; // May 5, 1991
        let ts = INN_EPOCH.wrapping_add(offset as i64);
        if (INN_EPOCH..=2_500_000_000).contains(&ts) { DateTime::from_timestamp(ts, 0) } else { None }
    }
}

pub struct ArrivalRank;

impl ArrivalRank {
    /// Sub-second rank interpolation: t = S + (rank + 0.5) / N.
    pub fn calculate(second: u64, rank: usize, total_in_sec: usize) -> f64 {
        if total_in_sec == 0 {
            return second as f64;
        }
        second as f64 + (rank as f64 + 0.5) / (total_in_sec as f64)
    }

    /// Calculate arrival timestamp respecting Abavia synthesis detection.
    /// When Abavia backend is detected, rank interpolation is disabled.
    pub fn calculate_with_backend(second: u64, rank: usize, total_in_sec: usize, is_abavia: bool) -> f64 {
        if is_abavia { second as f64 } else { Self::calculate(second, rank, total_in_sec) }
    }
}

pub struct XrefSync;

impl XrefSync {
    /// Parse `Xref: host group1:num1 group2:num2 ...` and return group-to-article mappings.
    pub fn parse_xref_pairs(xref_str: &str) -> Vec<(String, u64)> {
        let field = xref_str.trim_start();
        let payload = match field.get(..5) {
            Some(prefix) if prefix.eq_ignore_ascii_case("xref:") => field.get(5..).unwrap_or(field),
            _ => field,
        };
        payload
            .split_whitespace()
            .skip(1)
            .filter_map(|token| {
                let (grp, num_str) = token.split_once(':')?;
                let num = num_str.parse::<u64>().ok()?;
                Some((grp.to_string(), num))
            })
            .collect()
    }

    /// Calculate relative arrival timeline offset between two groups anchored by an atomic INN Xref crosspost.
    /// Since the physical message was injected atomically into INN across all listed groups,
    /// the timestamp difference at the cross-post anchor is exactly 0.000 s.
    pub fn anchor_offset(xref: &str, group_a: &str, art_a: u64, group_b: &str, art_b: u64) -> Option<f64> {
        let pairs = Self::parse_xref_pairs(xref);
        let has_a = pairs.iter().any(|(g, n)| g == group_a && *n == art_a);
        let has_b = pairs.iter().any(|(g, n)| g == group_b && *n == art_b);
        if has_a && has_b { Some(0.0) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nyuu_classification() {
        let headers = HeaderMap::new();
        let info = PostingTool::classify("<AbCdEfGhIjKl-1729000000000@nyuu>", &headers);
        assert_eq!(info.tool, ToolType::Nyuu);
        assert_eq!(info.embedded_time_ms, Some(1729000000000));
    }

    #[test]
    fn gopoststuff_10_to_19_digit_timestamp_normalization_to_ms() {
        let headers = HeaderMap::new();
        // 13-digit milliseconds
        let info_13 = PostingTool::classify("<gps-abc-1729000000000@example.org>", &headers);
        assert_eq!(info_13.tool, ToolType::GoPostStuff);
        assert_eq!(info_13.embedded_time_ms, Some(1_729_000_000_000));

        // 10-digit seconds -> normalized to milliseconds
        let info_10 = PostingTool::classify("<gps-xyz123-1729000000@gopoststuff.local>", &headers);
        assert_eq!(info_10.tool, ToolType::GoPostStuff);
        assert_eq!(info_10.embedded_time_ms, Some(1_729_000_000_000));

        // 16-digit microseconds -> normalized to milliseconds
        let info_16 = PostingTool::classify("<1729000000123456$gps@gopoststuff>", &headers);
        assert_eq!(info_16.tool, ToolType::GoPostStuff);
        assert_eq!(info_16.embedded_time_ms, Some(1_729_000_000_123));

        // 19-digit nanoseconds -> normalized to milliseconds
        let info_19 = PostingTool::classify("<1729000000123456789$gps@gopoststuff>", &headers);
        assert_eq!(info_19.tool, ToolType::GoPostStuff);
        assert_eq!(info_19.embedded_time_ms, Some(1_729_000_000_123));
    }

    #[test]
    fn nntp_date_skew_calibration_and_giganews_offset() {
        // Empirical probe (2026-11-03): Giganews (`super.newsgroupdirect.com`) returned
        // `111 20260419205543` when local UTC was 5.74s behind (`2026-04-19T20:55:37.260Z`).
        let local = DateTime::from_timestamp(1_776_632_137, 260_000_000).unwrap();
        let skew =
            ServerClock::measure_skew_seconds("111 20260419205543", local).expect("must parse NNTP 111 DATE response");
        assert!((skew - 5.74).abs() < 1e-3, "expected +5.74s skew, got {skew}");

        let corrected = ServerClock::correct_skew(1_776_632_143.0, skew);
        assert!((corrected - 1_776_632_137.26).abs() < 1e-3);
    }

    #[test]
    fn xref_multibyte_utf8_prefix_does_not_panic() {
        let pairs = XrefSync::parse_xref_pairs("hosté alt.binaries.test:42 alt.binaries.movies:99");
        assert_eq!(pairs, vec![("alt.binaries.test".to_string(), 42), ("alt.binaries.movies".to_string(), 99),]);
    }

    #[test]
    fn ngpost_classification() {
        let headers = HeaderMap::new();
        let info = PostingTool::classify("<e4b7c1a2-8d9e-4a6f-b2c3-d4e5f6a7b8c9@ngPost>", &headers);
        assert_eq!(info.tool, ToolType::NgPost);
    }

    #[test]
    fn postie_classification() {
        let headers = HeaderMap::new();
        let info = PostingTool::classify("<1234567890abcdef1234567890abcdef@12345678.90a>", &headers);
        assert_eq!(info.tool, ToolType::Postie);
    }

    #[test]
    fn highwinds_custom_alphabet_decodes_documented_vector() {
        let mid = "<EaPmB.546176$JP7.228032@fx09.am4>";
        let dt = ServerClock::decode(mid).expect("highwinds decoded");
        assert_eq!(dt.timestamp(), 1_503_379_236);
    }

    #[test]
    fn a_message_id_gives_its_tool_clock_and_session() {
        let hw = id_facts("<EaPmB.546176$JP7.228032@fx09.am4>");
        assert_eq!(hw.tool, None);
        assert_eq!(hw.clock_ms, Some(1_503_379_236_000));
        assert_eq!(
            (hw.hw_host.as_deref(), hw.hw_key.as_deref(), hw.hw_n),
            (Some("fx09.am4"), Some("JP7"), Some(546_176))
        );
        let nyuu = id_facts("<ZjPsQyLgOjNtQvXbHeKaXyCm1730295651235@nyuu>");
        assert_eq!(nyuu.tool, Some(ToolType::Nyuu));
        assert_eq!(nyuu.clock_ms, Some(1_730_295_651_235));
        assert_eq!(nyuu.hw_key, None);
        let ng = id_facts("<1064b678-f3f5-4e28-a5af-d48a3a986076@ngPost>");
        assert_eq!(ng.tool, Some(ToolType::NgPost));
        assert_eq!(ng.clock_ms, None);
        assert_eq!(id_facts("<a@b>"), IdFacts::default());
        assert_eq!(id_facts("nonsense"), IdFacts::default());
        assert_eq!(ToolType::from_code(ToolType::NewsUp.code()), Some(ToolType::NewsUp));
    }

    #[test]
    fn diablo_decoder() {
        let mid = "<66f01a2b$0001$1234$a1b2c3d4@usenetserver.com>";
        let dt = ServerClock::decode(mid).expect("diablo decoded");
        assert_eq!(dt.timestamp(), 1727011371);
    }

    #[test]
    fn arrival_rank_formula() {
        let t = ArrivalRank::calculate(1729000000, 15, 100);
        assert!((t - 1729000000.155).abs() < 1e-6);

        let t_abavia = ArrivalRank::calculate_with_backend(1729000000, 15, 100, true);
        assert_eq!(t_abavia, 1729000000.0);
    }
}
