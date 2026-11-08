//! Overview dates as a UTC second plus a small style word, written back with
//! Howard Hinnant's `civil_from_days` (no chrono on the read path).
//!
//! Handled shapes, which cover what posting tools put in `Date:`:
//! `[Dow, ]D[D] Mon YY[YY] HH:MM:SS (+|-)HHMM|GMT|UTC`. Anything else is
//! not parsed and the row is kept whole.
//!
//! Style word: bit 0 = no weekday, bit 1 = day not zero-padded, bits 2..4 =
//! zone (0 numeric, 1 `GMT`, 2 `UTC`), bit 4 = two-digit year (1970..2069),
//! bits 5..8 are the container's row flags (see `container`), bits 8.. =
//! zigzag of the offset in minutes. The commonest shape,
//! `Sat, 07 Nov 2026 12:00:00 +0000`, is 0.

pub const DATE_BUF: usize = 40;

const DOW: [&[u8; 3]; 7] = [b"Sun", b"Mon", b"Tue", b"Wed", b"Thu", b"Fri", b"Sat"];
const MON: [&[u8; 3]; 12] =
    [b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec"];

const NO_WEEKDAY: u32 = 1;
const DAY_UNPADDED: u32 = 2;
const ZONE_SHIFT: u32 = 2;
const YEAR2: u32 = 1 << 4;
/// Bits of the style word the date doesn't use, free for row flags.
pub const STYLE_FREE_BITS: u32 = 0b111 << 5;
const OFFSET_SHIFT: u32 = 8;

/// Days since 1970-01-01 to (year, month 1..=12, day 1..=31).
#[inline(always)]
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[inline(always)]
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

#[inline(always)]
fn two(buf: &mut [u8], at: usize, v: u32) {
    buf[at] = b'0' + (v / 10) as u8;
    buf[at + 1] = b'0' + (v % 10) as u8;
}

/// The zone offset a style word carries, in seconds east of UTC.
#[inline(always)]
pub fn style_offset_secs(style: u32) -> i64 {
    crate::varint::zigzag_decode((style >> OFFSET_SHIFT) as u64) * 60
}

/// Writes the date for UTC second `ts` in `style` into `buf`, returning its length.
pub fn format_date(ts: i64, style: u32, buf: &mut [u8; DATE_BUF]) -> usize {
    let off_min = crate::varint::zigzag_decode((style >> OFFSET_SHIFT) as u64);
    let local = ts + off_min * 60;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400) as u32;
    let (y, m, d) = civil_from_days(days);
    let mut n = 0usize;
    if style & NO_WEEKDAY == 0 {
        let dow = (days + 4).rem_euclid(7) as usize;
        buf[..3].copy_from_slice(DOW[dow]);
        buf[3] = b',';
        buf[4] = b' ';
        n = 5;
    }
    if d < 10 && style & DAY_UNPADDED != 0 {
        buf[n] = b'0' + d as u8;
        n += 1;
    } else {
        two(buf, n, d);
        n += 2;
    }
    buf[n] = b' ';
    buf[n + 1..n + 4].copy_from_slice(MON[(m - 1) as usize]);
    buf[n + 4] = b' ';
    n += 5;
    let y = y.clamp(0, 9999) as u32;
    if style & YEAR2 != 0 {
        two(buf, n, y % 100);
        n += 2;
    } else {
        two(buf, n, y / 100);
        two(buf, n + 2, y % 100);
        n += 4;
    }
    buf[n] = b' ';
    two(buf, n + 1, secs / 3600);
    buf[n + 3] = b':';
    two(buf, n + 4, secs / 60 % 60);
    buf[n + 6] = b':';
    two(buf, n + 7, secs % 60);
    buf[n + 9] = b' ';
    n += 10;
    match (style >> ZONE_SHIFT) & 3 {
        1 => {
            buf[n..n + 3].copy_from_slice(b"GMT");
            n += 3;
        }
        2 => {
            buf[n..n + 3].copy_from_slice(b"UTC");
            n += 3;
        }
        _ => {
            buf[n] = if off_min < 0 { b'-' } else { b'+' };
            let a = off_min.unsigned_abs() as u32;
            two(buf, n + 1, a / 60);
            two(buf, n + 3, a % 60);
            n += 5;
        }
    }
    n
}

#[inline(always)]
fn num2(s: &[u8], at: usize) -> Option<u32> {
    let a = s.get(at)?.wrapping_sub(b'0');
    let b = s.get(at + 1)?.wrapping_sub(b'0');
    (a < 10 && b < 10).then_some(a as u32 * 10 + b as u32)
}

/// Parses `s` into (UTC second, style), only when [`format_date`] writes it
/// back byte for byte.
pub fn parse_date(s: &[u8]) -> Option<(i64, u32)> {
    let mut style = 0u32;
    let mut p = 0usize;
    if s.len() > 5 && s[3] == b',' && s[4] == b' ' {
        DOW.iter().position(|d| &s[..3] == *d)?;
        p = 5;
    } else {
        style |= NO_WEEKDAY;
    }
    let (day, dl) = match (s.get(p)?, s.get(p + 1)?) {
        (a, b' ') if a.is_ascii_digit() => ((a - b'0') as u32, 1),
        _ => (num2(s, p)?, 2),
    };
    if dl == 1 {
        style |= DAY_UNPADDED;
    }
    p += dl;
    if s.get(p)? != &b' ' {
        return None;
    }
    let mon = MON.iter().position(|m| s.get(p + 1..p + 4) == Some(&m[..]))? as u32 + 1;
    p += 4;
    if s.get(p)? != &b' ' {
        return None;
    }
    let year = match (s.get(p + 3), s.get(p + 4)) {
        (Some(&b' '), _) => {
            style |= YEAR2;
            let yy = num2(s, p + 1)?;
            p += 3;
            if yy < 70 { 2000 + yy } else { 1900 + yy }
        }
        _ => {
            let y = num2(s, p + 1)? * 100 + num2(s, p + 3)?;
            p += 5;
            y
        }
    };
    let (h, mi, se) = (num2(s, p + 1)?, num2(s, p + 4)?, num2(s, p + 7)?);
    if s.get(p) != Some(&b' ')
        || s.get(p + 3) != Some(&b':')
        || s.get(p + 6) != Some(&b':')
        || s.get(p + 9) != Some(&b' ')
    {
        return None;
    }
    p += 10;
    let zone = s.get(p..)?;
    let off_min: i64 = match zone {
        b"GMT" => {
            style |= 1 << ZONE_SHIFT;
            0
        }
        b"UTC" => {
            style |= 2 << ZONE_SHIFT;
            0
        }
        [sign @ (b'+' | b'-'), ..] if zone.len() == 5 => {
            let v = (num2(zone, 1)? * 60 + num2(zone, 3)?) as i64;
            if *sign == b'-' { -v } else { v }
        }
        _ => return None,
    };
    if !(1..=31).contains(&day) || h > 23 || mi > 59 || se > 59 || year < 1000 {
        return None;
    }
    style |= (crate::varint::zigzag_encode(off_min) as u32) << OFFSET_SHIFT;
    let days = days_from_civil(year as i64, mon, day);
    let ts = days * 86_400 + (h * 3600 + mi * 60 + se) as i64 - off_min * 60;
    // the check: a wrong weekday, Feb 30, "-0000" and the like don't come back the same
    let mut buf = [0u8; DATE_BUF];
    let n = format_date(ts, style, &mut buf);
    (&buf[..n] == s).then_some((ts, style))
}

/// The length of `Dow, DD Mon YYYY HH:MM:SS +0000`.
pub const RFC2822_UTC_LEN: usize = 31;

/// Writes a UTC second as `Dow, DD Mon YYYY HH:MM:SS +0000`, the shape chrono's `to_rfc2822` gives for a
/// UTC time. Years outside 0..=9999 are clamped.
pub fn format_rfc2822_utc_fixed(epoch_secs: i64, out: &mut [u8; RFC2822_UTC_LEN]) {
    let mut buf = [0u8; DATE_BUF];
    let n = format_date(epoch_secs, 0, &mut buf);
    debug_assert_eq!(n, RFC2822_UTC_LEN);
    out.copy_from_slice(&buf[..RFC2822_UTC_LEN]);
}

/// The UTC second of a date in any shape [`parse_date`] reads (fixed zone offsets, GMT, UTC, 2- or 4-digit
/// years, with or without the weekday).
pub fn parse_rfc2822_to_epoch(bytes: &[u8]) -> Option<i64> {
    parse_date(bytes).map(|(ts, _)| ts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_matches_known_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(2026, 11, 7), 20_764);
        for z in [-800_000i64, -1, 0, 59, 11_016, 20_399, 2_932_896] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
    }

    #[test]
    fn dates_roundtrip() {
        for s in [
            &b"Sat, 07 Nov 2026 12:00:00 +0000"[..],
            b"Sat, 07 Nov 2026 23:59:59 -0500",
            b"7 Nov 2026 01:02:03 GMT",
            b"Sun, 8 Nov 2026 01:02:03 UTC",
            b"Thu, 01 Jan 1970 00:00:00 +0000",
            b"Fri, 12 Jul 24 21:31:07 UTC",
            b"Thu, 10 Oct 24 00:00:14 UTC",
            b"1 Jan 99 00:00:00 GMT",
            b"Tue, 29 Feb 2000 10:00:00 +0530",
        ] {
            let (ts, style) = parse_date(s).unwrap_or_else(|| panic!("{}", String::from_utf8_lossy(s)));
            let mut b = [0u8; DATE_BUF];
            let n = format_date(ts, style, &mut b);
            assert_eq!(&b[..n], s);
        }
        assert_eq!(parse_date(b"Sat, 07 Nov 2026 12:00:00 +0000").unwrap(), (20_764 * 86_400 + 43_200, 0));
        for bad in [
            &b"Fri, 07 Nov 2026 12:00:00 +0000"[..],
            b"Sat, 07 Nov 2026 12:00:00 -0000",
            b"Sat, 07 Nov 126 12:00:00 +0000",
            b"Sat,  7 Nov 2026 12:00:00 +0000",
            b"30 Feb 2026 12:00:00 GMT",
            b"",
        ] {
            assert!(parse_date(bad).is_none(), "{}", String::from_utf8_lossy(bad));
        }
    }
}
