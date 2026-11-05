//! A spot as its overview line gives it: the `From` header carries the poster, category, subcategories and
//! size, the subject the title and tag. Enough to pick the spots worth fetching whole.
//!
//! `From: Nickname <PUBKEY[.USERSIG]@<cat><keyid><subcats>.<size>.<random>.<date>.<custom-id>.<custom-value>.<sig>>`

use base64::Engine;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

use super::{Kind, Subcats};

/// What an overview line says about a spot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpotHeader {
    pub poster: String,
    pub title: String,
    pub tag: String,
    /// 0 video, 1 music, 2 games, 3 applications
    pub category: i32,
    pub key_id: i32,
    pub subcats: Subcats,
    /// bytes, as the poster gave them
    pub size: u64,
    /// the poster's public key modulus, base64, from before the `@`
    pub public_key: String,
    /// whether the header carries a signature, which every spot from a current key or after 2010 must
    pub signed: bool,
}

impl SpotHeader {
    /// The kind of spot this is, when it is one Atlas takes.
    pub fn kind(&self) -> Option<Kind> {
        self.subcats.kind(self.category)
    }

    /// The id Spotweb shows for the poster: from a CRC of their public key, so it names them across nicknames.
    pub fn spotter_id(&self) -> Option<String> {
        spotter_id(&self.public_key)
    }
}

/// The spot an overview line is, or None when its `From` isn't a spot's.
pub fn parse(subject: &str, from: &str) -> Option<SpotHeader> {
    let open = from.find('<')?;
    let address = from[open + 1..].strip_suffix('>').unwrap_or(&from[open + 1..]);
    // the nickname may hold an @, the address part before it can't
    let (local, domain) = address.split_once('@')?;
    let fields: Vec<&str> = domain.split('.').collect();
    if fields.len() < 6 {
        return None;
    }
    let head = fields[0].as_bytes();
    if head.len() < 2 || !head[0].is_ascii_digit() || !head[1].is_ascii_digit() {
        return None;
    }
    let category = i32::from(head[0] - b'0') - 1;
    let key_id = i32::from(head[1] - b'0');
    let poster = from[..open].trim().trim_matches('"').trim().to_string();
    let (title, tag) = title_and_tag(subject, key_id);
    if title.is_empty() || poster.is_empty() {
        return None;
    }
    let public_key = local.split('.').next().unwrap_or_default().to_string();
    Some(SpotHeader {
        poster,
        title,
        tag,
        category,
        key_id,
        subcats: subcats(&fields[0][2..]),
        size: fields[1].parse().unwrap_or(0),
        public_key,
        signed: fields.last().is_some_and(|s| !s.is_empty()),
    })
}

/// A recent key writes `Title | Tag`; key 1 wrote `Title | ... | Tag`, the last two pieces being the tag and a
/// leftover.
fn title_and_tag(subject: &str, key_id: i32) -> (String, String) {
    let pieces: Vec<&str> = subject.split('|').collect();
    if key_id != 1 {
        return (pieces[0].trim().to_string(), pieces.get(1).map(|t| t.trim().to_string()).unwrap_or_default());
    }
    if pieces.len() <= 1 {
        return (subject.trim().to_string(), String::new());
    }
    let tag = pieces[pieces.len() - 1].trim().to_string();
    let title = pieces[..pieces.len().saturating_sub(2)].join("|").trim().to_string();
    (title, tag)
}

/// `a09b04c00d11z01`, or the older `a9b4c0d11`: a letter and its number, over and over.
pub fn subcats(list: &str) -> Subcats {
    let mut out = Subcats::default();
    let mut letter: Option<char> = None;
    let mut number = String::new();
    for c in list.chars().map(|c| c.to_ascii_lowercase()).chain(std::iter::once('!')) {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        if let (Some(l), Ok(n)) = (letter, number.parse::<u16>()) {
            out.push(l, n);
        }
        letter = Some(c);
        number.clear();
    }
    out
}

/// Spot base64: `/` written as `-s` and `+` as `-p`, padding dropped.
pub fn unprepare_base64(s: &str) -> String {
    s.replace("-s", "/").replace("-p", "+")
}

/// Decodes base64 the way PHP's `base64_decode` does when it isn't strict: padding or not, stray bits ignored.
pub fn decode_base64(s: &str) -> Option<Vec<u8>> {
    const LENIENT: GeneralPurpose = GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    let clean: String = s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '/').collect();
    // a length of 4n+1 can't be base64; PHP drops the last character
    let clean = if clean.len() % 4 == 1 { &clean[..clean.len() - 1] } else { &clean[..] };
    LENIENT.decode(clean).ok()
}

/// Spotweb's spotter id: the CRC32 of the decoded key, little-endian, in base64 without `/`, `+` or `=`.
pub fn spotter_id(public_key: &str) -> Option<String> {
    let key = decode_base64(&unprepare_base64(public_key))?;
    if key.is_empty() {
        return None;
    }
    let mut crc = flate2::Crc::new();
    crc.update(&key);
    let id = base64::engine::general_purpose::STANDARD.encode(crc.sum().to_le_bytes());
    Some(id.chars().filter(|c| !matches!(c, '/' | '+' | '=')).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FROM: &str =
        "NUKE <2Kv6KxB3Rk-sJ0-pqWeYx9.sig@17a15b11c06c07c10d06z00.19600000000.11.1762300000.0.0.Zm9vYmFy>";

    #[test]
    fn reads_a_from_header() {
        let h = parse("The Last First: Winter K2 (2026) 2160p | NUKE", FROM).unwrap();
        assert_eq!(h.poster, "NUKE");
        assert_eq!(h.title, "The Last First: Winter K2 (2026) 2160p");
        assert_eq!(h.tag, "NUKE");
        assert_eq!(h.category, 0);
        assert_eq!(h.key_id, 7);
        assert_eq!(h.size, 19_600_000_000);
        assert_eq!(h.subcats.spotweb(), "a15|b11|c6|c7|c10|d6|z0|");
        assert_eq!(h.kind(), Some(Kind::Movie));
        assert_eq!(h.public_key, "2Kv6KxB3Rk-sJ0-pqWeYx9");
        assert!(h.signed);
    }

    #[test]
    fn recent_key_title_and_tag() {
        let from = FROM;
        let h = parse("Black Doves S02 2160p | fratertje", from).unwrap();
        assert_eq!(h.key_id, 7);
        assert_eq!((h.title.as_str(), h.tag.as_str()), ("Black Doves S02 2160p", "fratertje"));
        let h = parse("No tag here", from).unwrap();
        assert_eq!((h.title.as_str(), h.tag.as_str()), ("No tag here", ""));
    }

    #[test]
    fn key_one_title_and_tag() {
        assert_eq!(title_and_tag("A | B | x | tag", 1), ("A | B".into(), "tag".into()));
        assert_eq!(title_and_tag("plain", 1), ("plain".into(), String::new()));
    }

    #[test]
    fn not_a_spot() {
        assert!(parse("title", "someone <abc@example.com>").is_none());
        assert!(parse("title", "someone without address").is_none());
        assert!(parse("", FROM).is_none());
        assert!(parse("title", "<k@ab.1.2.3.4.5>").is_none());
    }

    #[test]
    fn unsigned() {
        let from = "x <k@11a09z00.100.1.2.3.4.>";
        assert!(!parse("t", from).unwrap().signed);
    }

    #[test]
    fn old_subcat_form() {
        assert_eq!(subcats("a9b4c0d5d15d11").spotweb(), "a9|b4|c0|d5|d15|d11|");
        assert_eq!(subcats("a09b04c00d05").spotweb(), "a9|b4|c0|d5|");
        assert_eq!(subcats("").spotweb(), "");
    }

    #[test]
    fn spotter_id_matches_spotweb() {
        // public keys from free.pt `From` headers, and the spotter ids Spotweb stored for them
        for (key, id) in [
            ("4JFBdnpR3wQvyGv0r37hFnRIgOSkgNe7oe1epFJBnSDwq3KxhrbMWzIUitFl-pdW1", "onerow"),
            ("tjdZLN1vFCBMh6uCDBq8RW3Dp1pT2BULWDkV13JjSVB2BMZHg16FdB437-pYnB27p", "xrNLkw"),
            ("1FNddBgs6c-pMxF9DDUjo9SQmtH-pgI5xOShhXz6EyZ7GHuXXWG-sSKP8sXt2EfOcz9", "baTFcw"),
        ] {
            assert_eq!(spotter_id(key).as_deref(), Some(id), "{key}");
        }
        assert_eq!(spotter_id(""), None);
    }

    #[test]
    fn lenient_base64() {
        assert_eq!(decode_base64("aGk").unwrap(), b"hi");
        assert_eq!(decode_base64("aGk=").unwrap(), b"hi");
        assert_eq!(decode_base64(&unprepare_base64("-s-s-s-s")).unwrap(), vec![0xff, 0xff, 0xff]);
    }
}
