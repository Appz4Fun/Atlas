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

static NYUU_TIMESTAMP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[a-z0-9_-]+-(\d{10,13})@.*$").unwrap());
static NYUU_RANDOM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9a-zA-Z]{24,32}@nyuu$").unwrap());
static NGPOST_UUID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}@[a-z0-9.-]+$").unwrap()
});
static POSTIE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9]{32}@[a-z0-9]{8}\.[a-z0-9]{3}$").unwrap());
static NXG_UPPER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[0-9a-f]{40}@[0-9a-f]{21}\.3$").unwrap());
static NEWSUP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[a-z0-9]{20,24}@[a-z0-9.-]+$").unwrap());
static GOPOSTSTUFF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^gps-[a-z0-9]+-(\d{10,19})@.*$").unwrap());
static GOPOSTSTUFF_ALT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(\d{10,19})\$gps@.*$").unwrap());

pub struct PostingTool;

impl PostingTool {
    /// Classify posting tool from Message-ID syntax and RFC headers.
    pub fn classify(message_id: &str, headers: &HeaderMap) -> ToolInfo {
        let mid = message_id.trim().trim_start_matches('<').trim_end_matches('>');
        if mid.len() > 4096 || !mid.contains('@') || !mid.is_ascii() {
            return ToolInfo {
                tool: ToolType::Unknown,
                version: None,
                embedded_time_ms: None,
                deterministic: false,
            };
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
            return ToolInfo {
                tool: ToolType::NxgUpper,
                version: None,
                embedded_time_ms: None,
                deterministic: true,
            };
        }

        // 2. Nyuu timestamp grammar
        if let Some(caps) = NYUU_TIMESTAMP_RE.captures(mid) {
            let ms: Option<u64> = caps.get(1).and_then(|m| m.as_str().parse().ok());
            return ToolInfo {
                tool: ToolType::Nyuu,
                version: None,
                embedded_time_ms: ms,
                deterministic: false,
            };
        }

        // Nyuu random grammar
        if NYUU_RANDOM_RE.is_match(mid) || domain.eq_ignore_ascii_case("nyuu") {
            return ToolInfo {
                tool: ToolType::Nyuu,
                version: None,
                embedded_time_ms: None,
                deterministic: false,
            };
        }

        // 3. ngPost UUIDv4 grammar
        if NGPOST_UUID_RE.is_match(mid) || domain.eq_ignore_ascii_case("ngpost") || domain.eq_ignore_ascii_case("ngpost.com") {
            return ToolInfo {
                tool: ToolType::NgPost,
                version: None,
                embedded_time_ms: None,
                deterministic: false,
            };
        }

        // 4. Postie grammar
        if POSTIE_RE.is_match(mid) {
            return ToolInfo {
                tool: ToolType::Postie,
                version: None,
                embedded_time_ms: None,
                deterministic: false,
            };
        }

        // 5. GoPostStuff grammar
        if let Some(caps) = GOPOSTSTUFF_RE.captures(mid).or_else(|| GOPOSTSTUFF_ALT_RE.captures(mid)) {
            let ts: Option<u64> = caps.get(1).and_then(|m| m.as_str().parse().ok());
            return ToolInfo {
                tool: ToolType::GoPostStuff,
                version: None,
                embedded_time_ms: ts,
                deterministic: false,
            };
        }
        if domain.to_ascii_lowercase().contains("gopoststuff") {
            return ToolInfo {
                tool: ToolType::GoPostStuff,
                version: None,
                embedded_time_ms: None,
                deterministic: false,
            };
        }

        // 6. NewsUP grammar
        if mid.to_ascii_lowercase().contains("newsup") || NEWSUP_RE.is_match(mid) {
            return ToolInfo {
                tool: ToolType::NewsUp,
                version: None,
                embedded_time_ms: None,
                deterministic: false,
            };
        }

        ToolInfo {
            tool: ToolType::Unknown,
            version: None,
            embedded_time_ms: None,
            deterministic: false,
        }
    }
}

pub struct ServerClock;

impl ServerClock {
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

        // Try standard base64 symbol values in little-endian order: val = sum(sym * 64^i)
        let decode_symbol_std = |c: char| -> Option<u64> {
            match c {
                'A'..='Z' => Some((c as u64) - ('A' as u64)),
                'a'..='z' => Some((c as u64) - ('a' as u64) + 26),
                '0'..='9' => Some((c as u64) - ('0' as u64) + 52),
                '+' | '-' => Some(62),
                '/' | '_' => Some(63),
                _ => None,
            }
        };

        // Try Highwinds empirical symbol values: 0-9 (0..9), a-z (10..35), A-Z (36..61), _ (62), % (63)
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

        // Evaluate standard base64 LE
        let mut std_val = 0u64;
        let mut factor = 1u64;
        let mut std_ok = true;
        for c in t_str.chars() {
            if let Some(sym) = decode_symbol_std(c) {
                std_val = std_val.wrapping_add(sym.wrapping_mul(factor));
                factor = factor.wrapping_mul(64);
            } else {
                std_ok = false;
                break;
            }
        }

        if std_ok {
            let ts = HW_EPOCH.wrapping_add(std_val as i64);
            // Check if plausible (1997..2040)
            if (HW_EPOCH..=2_200_000_000).contains(&ts) {
                return DateTime::from_timestamp(ts, 0);
            }
        }

        // Evaluate HW custom alphabet LE
        let mut hw_val = 0u64;
        factor = 1u64;
        for c in t_str.chars() {
            let sym = decode_symbol_hw(c)?;
            hw_val = hw_val.wrapping_add(sym.wrapping_mul(factor));
            factor = factor.wrapping_mul(64);
        }

        let ts = HW_EPOCH.wrapping_add(hw_val as i64);
        if (HW_EPOCH..=2_200_000_000).contains(&ts) {
            DateTime::from_timestamp(ts, 0)
        } else {
            None
        }
    }

    /// Decode Giganews 32-character base64 local part with XOR-0x9D mask.
    pub fn decode_giganews(local: &str) -> Option<DateTime<Utc>> {
        use base64::Engine;
        if local.len() < 32 {
            return None;
        }

        let b64_str = &local[..32];
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64_str)
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
        if (900_000_000..=2_500_000_000).contains(&w2) {
            DateTime::from_timestamp(w2 as i64, 0)
        } else {
            None
        }
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
        if (900_000_000..=2_500_000_000).contains(&ts) {
            DateTime::from_timestamp(ts, 0)
        } else {
            None
        }
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
        if (INN_EPOCH..=2_500_000_000).contains(&ts) {
            DateTime::from_timestamp(ts, 0)
        } else {
            None
        }
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
    pub fn calculate_with_backend(
        second: u64,
        rank: usize,
        total_in_sec: usize,
        is_abavia: bool,
    ) -> f64 {
        if is_abavia {
            second as f64
        } else {
            Self::calculate(second, rank, total_in_sec)
        }
    }
}

pub struct XrefSync;

impl XrefSync {
    /// Parse `Xref: host group1:num1 group2:num2 ...` and return group-to-article mappings.
    pub fn parse_xref_pairs(xref_str: &str) -> Vec<(String, u64)> {
        let field = xref_str.trim_start();
        let payload = if field.len() >= 5 && field[..5].eq_ignore_ascii_case("xref:") {
            &field[5..]
        } else {
            field
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
    pub fn anchor_offset(
        xref: &str,
        group_a: &str,
        art_a: u64,
        group_b: &str,
        art_b: u64,
    ) -> Option<f64> {
        let pairs = Self::parse_xref_pairs(xref);
        let has_a = pairs.iter().any(|(g, n)| g == group_a && *n == art_a);
        let has_b = pairs.iter().any(|(g, n)| g == group_b && *n == art_b);
        if has_a && has_b {
            Some(0.0)
        } else {
            None
        }
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
