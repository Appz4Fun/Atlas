//! Pictures posted with a release: a cover, a thumbnail, screenshots. Each is read whole, kept by its hash, and what
//! its headers say is kept beside it. A book or comic posted as one ZIP gives the pictures inside it (`container`). JPEGs are read closely (`jpeg`); the other kinds give their size and little more.

pub mod container;
pub mod jpeg;
pub mod step;

/// What a picture's first bytes say it is, and its size in pixels when the headers say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sniffed {
    /// `jpeg`, `png`, `gif`, `webp` or `bmp`
    pub format: &'static str,
    pub width: u32,
    pub height: u32,
}

/// The kind and size of a picture by its headers alone. None for anything not a picture of a kind known here.
pub fn sniff(bytes: &[u8]) -> Option<Sniffed> {
    let be32 = |at: usize| Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    let le16 = |at: usize| Some(u32::from(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?)));
    let le24 = |at: usize| {
        let b = bytes.get(at..at + 3)?;
        Some(u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16)
    };
    let (format, width, height) = if jpeg::is_jpeg(bytes) {
        let info = jpeg::read(bytes)?;
        ("jpeg", u32::from(info.width), u32::from(info.height))
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.get(12..16) == Some(b"IHDR") {
        ("png", be32(16)?, be32(20)?)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        ("gif", le16(6)?, le16(8)?)
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        match bytes.get(12..16)? {
            b"VP8 " if bytes.get(23..26) == Some(&[0x9D, 0x01, 0x2A]) => {
                ("webp", le16(26)? & 0x3FFF, le16(28)? & 0x3FFF)
            }
            b"VP8L" if bytes.get(20) == Some(&0x2F) => {
                let bits = u32::from_le_bytes(bytes.get(21..25)?.try_into().ok()?);
                ("webp", (bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1)
            }
            b"VP8X" => ("webp", le24(24)? + 1, le24(27)? + 1),
            _ => return None,
        }
    } else if bytes.starts_with(b"BM") && bytes.len() >= 26 {
        let width = i32::from_le_bytes(bytes.get(18..22)?.try_into().ok()?);
        let height = i32::from_le_bytes(bytes.get(22..26)?.try_into().ok()?);
        ("bmp", width.unsigned_abs(), height.unsigned_abs())
    } else {
        return None;
    };
    Some(Sniffed { format, width, height })
}

/// The content type a browser is sent a picture with.
pub fn content_type(format: &str) -> &'static str {
    match format {
        "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => "application/octet-stream",
    }
}

/// Whether a file is a picture, by its name.
pub fn is_image_name(name: &str) -> bool {
    let lower = name.trim_end_matches('"').to_ascii_lowercase();
    lower
        .rsplit_once('.')
        .is_some_and(|(_, ext)| matches!(ext, "jpg" | "jpeg" | "jpe" | "png" | "gif" | "webp" | "bmp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_sizes_by_their_headers() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend(640u32.to_be_bytes());
        png.extend(480u32.to_be_bytes());
        assert_eq!(sniff(&png), Some(Sniffed { format: "png", width: 640, height: 480 }));
        let gif = b"GIF89a\x40\x01\xf0\x00".to_vec();
        assert_eq!(sniff(&gif), Some(Sniffed { format: "gif", width: 320, height: 240 }));
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0\0\0\0\0".to_vec();
        webp.extend([0x7F, 0x07, 0x00, 0x37, 0x04, 0x00]);
        assert_eq!(sniff(&webp), Some(Sniffed { format: "webp", width: 1920, height: 1080 }));
        let mut bmp = b"BM".to_vec();
        bmp.resize(18, 0);
        bmp.extend(100i32.to_le_bytes());
        bmp.extend((-50i32).to_le_bytes());
        assert_eq!(sniff(&bmp), Some(Sniffed { format: "bmp", width: 100, height: 50 }));
        let y = vec![0u8; 64];
        let c = vec![128u8; 16];
        let jpeg = crate::video::jpeg::encode(&y, &c, &c, 8, 8, 85);
        assert_eq!(sniff(&jpeg), Some(Sniffed { format: "jpeg", width: 8, height: 8 }));
        assert_eq!(sniff(b"Rar!\x1a\x07\x01\0"), None);
    }

    #[test]
    fn picture_names() {
        assert!(is_image_name("00-thumbnail.jpg"));
        assert!(is_image_name("Cover.JPEG"));
        assert!(!is_image_name("Some.Film.mkv"));
        assert!(!is_image_name("jpg"));
    }
}
