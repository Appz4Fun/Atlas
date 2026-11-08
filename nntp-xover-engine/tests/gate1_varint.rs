//! Gate 1: varints, delta streams, ASCII writers, and the date formatter against chrono.

use nntp_xover_engine::corpus::Rng;
use nntp_xover_engine::date::{RFC2822_UTC_LEN, format_rfc2822_utc_fixed, parse_rfc2822_to_epoch};
use nntp_xover_engine::varint::*;

#[test]
fn zigzag_varint_roundtrip() {
    for v in [0i64, -1, 1, i64::MIN, i64::MAX, 84_500_000] {
        let mut buf = Vec::new();
        write_varint(zigzag_encode(v), &mut buf);
        let mut pos = 0;
        assert_eq!(zigzag_decode(read_varint_unchecked(&buf, &mut pos)), v);
        assert_eq!(pos, buf.len());
    }
}

#[test]
fn delta_streams_roundtrip() {
    let vals: Vec<i64> = vec![84_500_000, 84_500_001, 84_500_001, 84_500_009, 0, -5, i64::MAX, i64::MIN];
    let mut buf = Vec::new();
    encode_delta_stream(&vals, &mut buf);
    let mut back = Vec::new();
    assert_eq!(decode_delta_stream(&buf, vals.len(), &mut back), buf.len());
    assert_eq!(back, vals);
    // consecutive article numbers cost a byte each
    let seq: Vec<i64> = (0..1000).map(|i| 84_500_000 + i).collect();
    let mut buf = Vec::new();
    encode_delta_stream(&seq, &mut buf);
    assert!(buf.len() <= 1000 + 5, "{}", buf.len());
}

#[test]
fn padded_writer() {
    let w = |v, p| {
        let mut o = Vec::new();
        write_padded_u32(v, p, &mut o);
        o
    };
    assert_eq!(w(4, 2), b"04");
    assert_eq!(w(4, 3), b"004");
    assert_eq!(w(4, 0), b"4");
    assert_eq!(w(125, 2), b"125");
    let mut o = Vec::new();
    write_u64_ascii(18_446_744_073_709_551_615, &mut o);
    write_i64_ascii(-9_223_372_036_854_775_808, &mut o);
    assert_eq!(o, b"18446744073709551615-9223372036854775808");
}

#[test]
fn dates_match_chrono_for_fifty_thousand_timestamps() {
    let mut r = Rng::new(1);
    let mut stamps: Vec<i64> = (0..50_000).map(|_| r.below(1 << 31) as i64).collect();
    // leap days and the ends of the range
    for d in [
        "2000-02-29T23:59:59Z",
        "2024-02-29T00:00:00Z",
        "2028-02-29T12:34:56Z",
        "1970-01-01T00:00:00Z",
        "2038-01-19T03:14:07Z",
    ] {
        stamps.push(chrono::DateTime::parse_from_rfc3339(d).unwrap().timestamp());
    }
    let mut buf = [0u8; RFC2822_UTC_LEN];
    for ts in stamps {
        format_rfc2822_utc_fixed(ts, &mut buf);
        let dt = chrono::DateTime::from_timestamp(ts, 0).unwrap();
        // the fixed 31-byte shape zero-pads the day; chrono's to_rfc2822 writes `5 Jan`, which the parser
        // reads too
        let want = dt.format("%a, %d %b %Y %H:%M:%S +0000").to_string();
        assert_eq!(std::str::from_utf8(&buf).unwrap(), want, "{ts}");
        assert_eq!(parse_rfc2822_to_epoch(&buf), Some(ts), "{want}");
        assert_eq!(parse_rfc2822_to_epoch(dt.to_rfc2822().as_bytes()), Some(ts), "{}", dt.to_rfc2822());
    }
}
