//! What a spot posts as binaries: its NZB, deflated, and its image. Neither is yEnc: the bytes are written as
//! text with only `\n`, `\r`, `\0` and `=` escaped (`=C`, `=B`, `=A`, `=D`), split into lines, over one or more
//! articles.

use std::io::Read;

/// The largest NZB read out of a spot, inflated. A spot NZB of a 100 GB release is a few MB.
pub const MAX_NZB_BYTES: usize = 64 * 1024 * 1024;

/// The bytes the articles' bodies hold together, in order. Line ends are the posting's own, not the data's, so
/// they go; the escapes are undone in one pass, so `=D` followed by `C` stays `=C`.
pub fn unescape(bodies: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bodies.iter().map(|b| b.len()).sum());
    let mut escaped = false;
    for body in bodies {
        for &b in *body {
            if b == b'\r' || b == b'\n' {
                continue;
            }
            if escaped {
                escaped = false;
                match b {
                    b'C' => out.push(b'\n'),
                    b'B' => out.push(b'\r'),
                    b'A' => out.push(0),
                    b'D' => out.push(b'='),
                    other => {
                        out.push(b'=');
                        out.push(other);
                    }
                }
            } else if b == b'=' {
                escaped = true;
            } else {
                out.push(b);
            }
        }
    }
    if escaped {
        out.push(b'=');
    }
    out
}

/// A spot's NZB from its articles' bodies: unescaped, then inflated (raw deflate, PHP's `gzinflate`). None
/// when it won't inflate or comes to more than `MAX_NZB_BYTES`.
pub fn nzb(bodies: &[&[u8]]) -> Option<Vec<u8>> {
    let deflated = unescape(bodies);
    let mut out = Vec::new();
    let mut reader = flate2::read::DeflateDecoder::new(&deflated[..]).take(MAX_NZB_BYTES as u64 + 1);
    reader.read_to_end(&mut out).ok()?;
    if out.len() > MAX_NZB_BYTES || out.is_empty() {
        return None;
    }
    Some(out)
}

/// Spotweb's `specialZipStr`, for tests: how a poster writes the bytes.
#[cfg(test)]
pub fn escape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for &b in data {
        match b {
            b'=' => out.extend_from_slice(b"=D"),
            b'\n' => out.extend_from_slice(b"=C"),
            b'\r' => out.extend_from_slice(b"=B"),
            0 => out.extend_from_slice(b"=A"),
            b => out.push(b),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn deflate(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn round_trip() {
        let data: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        let escaped = escape(&data);
        // split over two articles of short lines, as a poster would
        let lines: Vec<u8> = escaped.chunks(128).flat_map(|c| c.iter().copied().chain(*b"\r\n")).collect();
        let (one, two) = lines.split_at(lines.len() / 2);
        assert_eq!(unescape(&[one, two]), data);
    }

    #[test]
    fn escapes_in_one_pass() {
        assert_eq!(unescape(&[b"=DC"]), b"=C");
        assert_eq!(unescape(&[b"a=", b"\r\nCb"]), b"a\nb");
        assert_eq!(unescape(&[b"x="]), b"x=");
    }

    #[test]
    fn inflates_an_nzb() {
        let xml = b"<?xml version=\"1.0\"?>\n<nzb><file subject=\"a\"></file></nzb>";
        let posted = escape(&deflate(xml));
        assert_eq!(nzb(&[&posted]).unwrap(), xml);
        assert!(nzb(&[b"not deflate at all"]).is_none());
        assert!(nzb(&[]).is_none());
    }
}
