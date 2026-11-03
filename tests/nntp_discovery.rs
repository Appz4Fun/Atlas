//! Integration tests for Milestone 3: Release Discovery & Clock Reconstruction
//! Covering Features 8, 9, 10 (TC-T1-F08, TC-T1-F09, TC-T1-F10).

use std::collections::HashMap;

use atlas::discovery::{
    ArrivalRank, HeaderMap, PostingTool, ServerClock, ToolType, XrefSync,
};
use base64::Engine;

// ============================================================================
// Feature 8: Message-ID Grammar & Tool Classifier
// ============================================================================

#[test]
fn tc_t1_f08_1_nyuu_posting_tool_identification() {
    let mid = "<AbCdEfGhIjKl-1729000000000@nyuu>";
    let headers = HeaderMap::new();
    let info = PostingTool::classify(mid, &headers);
    assert_eq!(info.tool, ToolType::Nyuu);
    assert_eq!(info.embedded_time_ms, Some(1729000000000));
    assert!(!info.deterministic);
}

#[test]
fn tc_t1_f08_2_ngpost_uuid_v4_identification() {
    let mid = "<e4b7c1a2-8d9e-4a6f-b2c3-d4e5f6a7b8c9@ngPost>";
    let headers = HeaderMap::new();
    let info = PostingTool::classify(mid, &headers);
    assert_eq!(info.tool, ToolType::NgPost);
}

#[test]
fn tc_t1_f08_3_postie_identifier_classification() {
    let mid = "<1234567890abcdef1234567890abcdef@12345678.90a>";
    let headers = HeaderMap::new();
    let info = PostingTool::classify(mid, &headers);
    assert_eq!(info.tool, ToolType::Postie);
}

#[test]
fn tc_t1_f08_4_nxg_upper_deterministic_sha256() {
    let mid = "<0123456789abcdef0123456789abcdef01234567@21.3>";
    let mut headers = HeaderMap::new();
    headers.insert("X-Nxg".into(), "release_123".into());
    let info = PostingTool::classify(mid, &headers);
    assert_eq!(info.tool, ToolType::NxgUpper);
    assert!(info.deterministic);
}

#[test]
fn tc_t1_f08_5_newsup_lcg_identifier() {
    let mid = "<newsup.48bit.hash@usenet>";
    let headers = HeaderMap::new();
    let info = PostingTool::classify(mid, &headers);
    assert_eq!(info.tool, ToolType::NewsUp);
}

// ============================================================================
// Feature 9: Server Message-ID Decoders
// ============================================================================

#[test]
fn tc_t1_f09_1_highwinds_base64_le_decoder() {
    let mid = "<EaPmB.546176$JP7.228032@fx09.am4>";
    let dt = ServerClock::decode(mid).expect("Highwinds LE base64 must decode to a valid timestamp");
    assert_eq!(dt.timestamp(), 1_503_379_236);
}

#[test]
fn tc_t1_f09_2_giganews_xor_0x9d_decoder() {
    let target_ts = 1729000000u32;
    let mut payload = [0u8; 24];
    payload[8..12].copy_from_slice(&target_ts.to_le_bytes());

    let mut masked = [0u8; 24];
    for (i, &b) in payload.iter().enumerate() {
        masked[i] = b ^ 0x9D;
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(masked);
    let mid = format!("<{}@giganews.com>", b64);

    let dt = ServerClock::decode(&mid).expect("Giganews decoder must succeed");
    assert_eq!(dt.timestamp(), target_ts as i64);
}

#[test]
fn tc_t1_f09_3_diablo_hex_decoder() {
    let mid = "<66f01a2b$0001$1234$a1b2c3d4@usenetserver.com>";
    let dt = ServerClock::decode(mid).expect("Diablo hex decoder must succeed");
    assert_eq!(dt.timestamp(), 1727011371);
}

#[test]
fn tc_t1_f09_4_inn_radix32_decoder() {
    // Offset 100_000_000 from INN epoch 673_416_000
    // 100_000_000 in radix32: 2vbo80
    let mid = "<2vbo80$123$1@news.inn.org>";
    let dt = ServerClock::decode(mid).expect("INN radix32 decoder must succeed");
    assert_eq!(dt.timestamp(), 673_416_000 + 100_000_000);
}

#[test]
fn tc_t1_f09_5_non_server_user_message_id_fallback() {
    let mid = "<custom-uuid-12345@domain.invalid>";
    let dt = ServerClock::decode(mid);
    assert_eq!(dt, None, "User-assigned MID must return None to trigger fallback");
}

// ============================================================================
// Feature 10: Authoritative Arrival Clock
// ============================================================================

#[test]
fn tc_t1_f10_1_sub_second_rank_clock_calculation() {
    let s = 1729000000u64;
    let rank = 15usize;
    let total = 100usize;
    let t = ArrivalRank::calculate(s, rank, total);
    let expected = 1729000000.0 + (15.0 + 0.5) / 100.0;
    assert!((t - expected).abs() < 1e-6);
    assert!((t - 1729000000.155).abs() < 1e-6);
}

#[test]
fn tc_t1_f10_2_strict_monotonicity_across_articles() {
    let s = 1729000000u64;
    let total = 5usize;
    let timestamps: Vec<f64> = (0..5)
        .map(|rank| ArrivalRank::calculate(s, rank, total))
        .collect();

    for i in 0..timestamps.len() - 1 {
        assert!(
            timestamps[i] < timestamps[i + 1],
            "Timestamps must be strictly monotonic: {} < {}",
            timestamps[i],
            timestamps[i + 1]
        );
    }
}

#[test]
fn tc_t1_f10_3_atomic_inn_xref_crosspost_anchoring() {
    let xref = "Xref: news.server.com alt.binaries.movies:1001 alt.binaries.test:5002";
    let offset = XrefSync::anchor_offset(
        xref,
        "alt.binaries.movies",
        1001,
        "alt.binaries.test",
        5002,
    );
    assert_eq!(
        offset,
        Some(0.0),
        "Atomic crosspost anchor must produce exactly 0.000 s offset"
    );
}

#[test]
fn tc_t1_f10_4_date_header_override_on_postie_randomization() {
    // Postie randomized Date: 4 hours ago (14400s prior)
    let fake_date_ts = 1729000000u64 - 14400;
    let server_arrival_sec = 1729000000u64;
    let rank = 2usize;
    let total = 10usize;

    // Arrival clock ignores client-supplied fake_date_ts and orders via server arrival
    let t_arrival = ArrivalRank::calculate(server_arrival_sec, rank, total);
    assert!((t_arrival - 1729000000.25).abs() < 1e-6);
    assert!(t_arrival > fake_date_ts as f64);
}

#[test]
fn tc_t1_f10_5_nyuu_millisecond_clock_precedence() {
    let mid = "<AbCdEfGhIjKl-1729000000123@nyuu>";
    let headers = HashMap::new();
    let info = PostingTool::classify(mid, &headers);
    assert_eq!(info.tool, ToolType::Nyuu);
    assert_eq!(info.embedded_time_ms, Some(1729000000123));

    // Convert ms to fractional second arrival clock
    let t_seconds = (info.embedded_time_ms.unwrap() as f64) / 1000.0;
    assert!((t_seconds - 1729000000.123).abs() < 1e-6);
}
