//! What an archive's headers say besides names: each file's CRC32, time and host OS, how it was packed, the random
//! values one archive carries and no other, and what they let slip about whoever packed it.
//!
//! The uploader's own facts (a Unix owner's names and ids, the packer's clock zone) never leave this module raw: the
//! step keeps a salted digest of them (`keys::salted8`) and nothing else, the same as the posting account. The rest
//! is a tool's fingerprint and the files' own facts, kept as they are.
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic))]

/// The host a file was packed on, as `archive_inner_files.host_os` keeps it, whatever the format calls it.
pub const HOST_WINDOWS: u8 = 0;
pub const HOST_UNIX: u8 = 1;
pub const HOST_MAC: u8 = 2;
pub const HOST_OTHER: u8 = 9;

/// The word a host is shown as.
pub fn host_word(host: u8) -> &'static str {
    match host {
        HOST_WINDOWS => "Windows",
        HOST_UNIX => "Unix",
        HOST_MAC => "classic Mac OS",
        _ => "other",
    }
}

/// RAR5's host field: 0 Windows, 1 Unix.
pub fn rar5_host(raw: u64) -> u8 {
    match raw {
        0 => HOST_WINDOWS,
        1 => HOST_UNIX,
        _ => HOST_OTHER,
    }
}

/// RAR4's HOST_OS byte: 0 MS-DOS, 1 OS/2, 2 Win32, 3 Unix, 4 Mac OS, 5 BeOS.
pub fn rar4_host(raw: u8) -> u8 {
    match raw {
        0 | 2 => HOST_WINDOWS,
        3 => HOST_UNIX,
        4 => HOST_MAC,
        _ => HOST_OTHER,
    }
}

/// ZIP's "version made by" high byte (APPNOTE 4.4.2): 0 FAT, 3 Unix, 7 Macintosh, 10 NTFS, 14 VFAT, 19 OS X.
pub fn zip_host(raw: u8) -> u8 {
    match raw {
        0 | 10 | 14 => HOST_WINDOWS,
        3 | 19 => HOST_UNIX,
        7 => HOST_MAC,
        _ => HOST_OTHER,
    }
}

/// Seconds between 1601-01-01 and 1970-01-01, the gap between a Windows FILETIME and Unix time.
const FILETIME_EPOCH: i64 = 11_644_473_600;

/// A FILETIME (100 ns since 1601, UTC) as Unix seconds. None for zero, which writers use for "not set".
pub fn filetime_secs(ft: u64) -> Option<i64> {
    (ft != 0).then(|| i64::try_from(ft / 10_000_000).ok().map(|s| s - FILETIME_EPOCH)).flatten()
}

/// A DOS date and time (2-second steps, the packer's own clock, no zone) as seconds, read as if it were UTC: a
/// wall-clock reading to set against another, never a moment by itself. None for a field that can't be a date.
pub fn dos_wall_secs(time: u16, date: u16) -> Option<i64> {
    let (year, month, day) = (1980 + i32::from(date >> 9), u32::from((date >> 5) & 0x0f), u32::from(date & 0x1f));
    let (hour, minute, second) = (u32::from(time >> 11), u32::from((time >> 5) & 0x3f), u32::from(time & 0x1f) * 2);
    let day = chrono::NaiveDate::from_ymd_opt(year, month, day)?;
    Some(day.and_hms_opt(hour, minute, second)?.and_utc().timestamp())
}

/// A 32-bit DOS stamp, time in the low half as RAR4 and ZIP keep it.
pub fn dos32_wall_secs(stamp: u32) -> Option<i64> {
    dos_wall_secs((stamp & 0xffff) as u16, (stamp >> 16) as u16)
}

/// A value that must never be printed or logged: its `Debug` says only that it's there.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Private(pub String);

impl std::fmt::Debug for Private {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Private(..)")
    }
}

/// A Unix owner record as the step keys it, in one text: user and group names, uid and gid, each part only when the
/// record has it. Kept as `Private` and turned into a salted key, never stored or shown.
pub fn owner_text(user: Option<&str>, group: Option<&str>, uid: Option<u64>, gid: Option<u64>) -> Option<Private> {
    if user.is_none() && group.is_none() && uid.is_none() && gid.is_none() {
        return None;
    }
    let clean = |s: Option<&str>| s.map(|s| s.chars().filter(|c| !c.is_control()).take(256).collect::<String>());
    Some(Private(format!(
        "owner:u={}:g={}:uid={}:gid={}",
        clean(user).unwrap_or_default(),
        clean(group).unwrap_or_default(),
        uid.map(|n| n.to_string()).unwrap_or_default(),
        gid.map(|n| n.to_string()).unwrap_or_default(),
    )))
}

/// The most a zone can be off UTC: UTC-12 to UTC+14.
const ZONE_MIN: i64 = -12 * 3600;
const ZONE_MAX: i64 = 14 * 3600;
/// Zones are whole quarter hours.
const QUARTER: i64 = 900;

/// The packer's clock zone from one moment read twice: on its own wall clock (DOS time) and in UTC (ZIP's 0x5455 or
/// NTFS extra). In seconds east of UTC, a whole quarter hour; None when the two can't be one moment in a real zone.
pub fn zone_of_pair(wall: i64, utc: i64) -> Option<i64> {
    // DOS time steps by 2 seconds and drops the rest
    let gap = wall - utc;
    let zone = (gap as f64 / QUARTER as f64).round() as i64 * QUARTER;
    ((gap - zone).abs() <= 2 && (ZONE_MIN..=ZONE_MAX).contains(&zone)).then_some(zone)
}

/// The packer's clock against the post's (`posted`, the Date header's wall clock as `releases.posted_date` keeps it),
/// for an archive that has no UTC time of its own (RAR4, a ZIP without extras): their difference to the quarter hour,
/// when the file was written within a minute of a quarter-hour step before the post and inside a real zone's range.
/// It's a guess: a file written hours before posting can land on a step by chance, soo this goes into a key only, for
/// a reader to weigh, and never decides anything.
pub fn zone_against_post(wall: i64, posted: i64) -> Option<i64> {
    let gap = wall - posted;
    let zone = (gap as f64 / QUARTER as f64).round() as i64 * QUARTER;
    ((gap - zone).abs() <= 60 && (ZONE_MIN..=ZONE_MAX).contains(&zone)).then_some(zone)
}

/// What the zone key is taken over: the zone and the packer, soo it reads "packed the same way on a clock this far
/// off". Private, like the owner: it goes into a salted key only.
pub fn zone_text(zone: i64, packer: Option<&str>) -> Private {
    Private(format!("zone:{zone}:{}", packer.unwrap_or("")))
}

/// What one archive's own random values come to, with the format first soo two formats never meet: RAR5 `-hp`'s
/// salt and check, a RAR5 or RAR4 file's salt (and RAR5's IV), 7z's AES IV, or 7z's start header (where its header
/// lies, its size and CRC, which the header's own times and sizes make one archive's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Twin {
    pub tag: &'static str,
    pub bytes: Vec<u8>,
}

impl Twin {
    pub fn new(tag: &'static str, bytes: &[u8]) -> Twin {
        Twin { tag, bytes: bytes.to_vec() }
    }

    /// What the key is taken over.
    pub fn text(&self) -> Vec<u8> {
        let mut out = format!("twin:{}:", self.tag).into_bytes();
        out.extend(&self.bytes);
        out
    }
}

/// A 7z coder's name, by its id: the ones packers use, hex for the rest.
pub fn sevenz_coder(id: &[u8], props: &[u8]) -> String {
    match id {
        [0x00] => "Copy".into(),
        [0x21] => match props.first() {
            Some(&b) if b <= 40 => format!("LZMA2:{}", dict_word(lzma2_dict(b))),
            _ => "LZMA2".into(),
        },
        [0x03, 0x01, 0x01] => match props.get(1..5).and_then(|d| d.try_into().ok()).map(u32::from_le_bytes) {
            Some(d) => format!("LZMA:{}", dict_word(u64::from(d))),
            None => "LZMA".into(),
        },
        [0x03, 0x03, 0x01, 0x03] => "BCJ".into(),
        [0x03, 0x03, 0x01, 0x1b] => "BCJ2".into(),
        [0x03] => "Delta".into(),
        [0x04, 0x01, 0x08] => "Deflate".into(),
        [0x04, 0x02, 0x02] => "BZip2".into(),
        [0x03, 0x04, 0x01] => "PPMD".into(),
        [0x0a] => "ARM64".into(),
        [0x0b] => "RISCV".into(),
        [0x06, 0xf1, 0x07, 0x01] => "AES".into(),
        _ => id.iter().map(|b| format!("{b:02x}")).collect(),
    }
}

/// LZMA2's dictionary byte as bytes.
fn lzma2_dict(b: u8) -> u64 {
    if b == 40 { u64::from(u32::MAX) } else { (2 | u64::from(b & 1)) << (b / 2 + 11) }
}

/// A dictionary size as the packer's switch would say it: 4m, 128k.
pub fn dict_word(bytes: u64) -> String {
    if bytes >= 1 << 20 && bytes.is_multiple_of(1 << 20) {
        format!("{}m", bytes >> 20)
    } else if bytes >= 1 << 10 && bytes.is_multiple_of(1 << 10) {
        format!("{}k", bytes >> 10)
    } else {
        format!("{bytes}b")
    }
}

/// The 7z AES coder's IV, from its properties: a first byte with the cycle power and flags (0x80 salt, 0x40 IV), a
/// second with the sizes less one (salt high nibble, IV low), then salt and IV.
pub fn sevenz_aes_iv(props: &[u8]) -> Option<Vec<u8>> {
    let first = *props.first()?;
    if first & 0xc0 == 0 {
        return None;
    }
    let second = *props.get(1)?;
    let salt = usize::from(first >> 7) + usize::from(second >> 4);
    let iv = usize::from((first >> 6) & 1) + usize::from(second & 0x0f);
    let at = 2 + salt;
    props.get(at..at + iv).filter(|v| !v.is_empty()).map(<[u8]>::to_vec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_as_their_formats_say() {
        // 2024-03-10 14:30:20 in DOS terms
        let date = ((2024 - 1980) << 9 | 3 << 5 | 10) as u16;
        let time = (14 << 11 | 30 << 5 | 10) as u16;
        assert_eq!(dos_wall_secs(time, date), Some(1_710_081_020));
        assert_eq!(dos32_wall_secs(u32::from(date) << 16 | u32::from(time)), Some(1_710_081_020));
        assert_eq!(dos_wall_secs(0, 0), None, "month 0 is no date");
        assert_eq!(filetime_secs(0), None);
        assert_eq!(filetime_secs(116_444_736_000_000_000), Some(0));
    }

    #[test]
    fn a_zone_comes_from_one_moment_read_twice() {
        assert_eq!(zone_of_pair(3600 + 1_000_000, 1_000_000), Some(3600));
        assert_eq!(zone_of_pair(-5 * 3600 + 1_000_001, 1_000_000), Some(-5 * 3600), "DOS's 2 seconds");
        assert_eq!(zone_of_pair(19_800 + 1_000_000, 1_000_000), Some(19_800), "India is half an hour off");
        assert_eq!(zone_of_pair(1_000_000 + 100, 1_000_000), None, "not a zone");
        assert_eq!(zone_of_pair(1_000_000 + 20 * 3600, 1_000_000), None, "no zone is 20 hours off");
        assert_eq!(zone_against_post(1_000_000 + 7200 - 40, 1_000_000), Some(7200));
        assert_eq!(zone_against_post(1_000_000 + 7200 - 400, 1_000_000), None, "too far from a step");
    }

    #[test]
    fn private_values_never_print() {
        let owner = owner_text(Some("someone"), Some("staff"), Some(1000), Some(20)).unwrap();
        assert_eq!(format!("{owner:?}"), "Private(..)");
        assert!(owner.0.contains("someone"), "the key still sees it");
        assert_eq!(owner_text(None, None, None, None), None);
        assert!(!format!("{:?}", zone_text(3600, Some("x"))).contains("3600"));
    }

    #[test]
    fn seven_zip_coders_read_as_their_switches() {
        assert_eq!(sevenz_coder(&[0x21], &[24]), "LZMA2:16m");
        assert_eq!(sevenz_coder(&[0x03, 0x01, 0x01], &[0x5d, 0, 0, 0, 1]), "LZMA:16m");
        assert_eq!(sevenz_coder(&[0x06, 0xf1, 0x07, 0x01], &[]), "AES");
        assert_eq!(sevenz_coder(&[0x7f], &[]), "7f");
        // 7-Zip's default: power 19, no salt, a 16-byte IV
        let mut props = vec![0x40 | 19, 0x0f];
        props.extend(1..=16u8);
        assert_eq!(sevenz_aes_iv(&props), Some((1..=16u8).collect::<Vec<_>>()));
        assert_eq!(sevenz_aes_iv(&[19]), None, "no IV flagged");
        assert_eq!(sevenz_aes_iv(&[0x40, 0x0f, 1, 2]), None, "cut short");
    }
}
