//! A spot whole: the XML its `X-XML` headers carry. Spot XML is often not well formed (bare `&` in titles,
//! markup in descriptions), so this reads it the forgiving way Spotweb does: each element's text taken as it
//! stands, CDATA unwrapped and HTML entities decoded.

use std::sync::LazyLock;

use regex::Regex;

use super::header::subcats;
use super::{Kind, MAX_XML_BYTES, Subcats, valid_message_id};

/// What a spot's XML says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FullSpot {
    pub key: i32,
    /// unix seconds
    pub created: i64,
    /// 0 video, 1 music, 2 games, 3 applications
    pub category: i32,
    pub subcats: Subcats,
    pub title: String,
    pub poster: String,
    pub tag: String,
    pub description: String,
    pub website: String,
    /// bytes
    pub size: u64,
    /// the articles of the deflated NZB, in order
    pub nzb: Vec<String>,
    /// the articles of the cover image, in order
    pub image: Vec<String>,
}

impl FullSpot {
    pub fn kind(&self) -> Option<Kind> {
        self.subcats.kind(self.category)
    }

    /// The IMDb id the website links to, `tt` and its digits.
    pub fn imdb_id(&self) -> Option<String> {
        static IMDB: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"imdb\.[a-z.]+/(?:[a-z]{2}/)?title/(tt\d{7,9})").unwrap());
        IMDB.captures(&self.website.to_ascii_lowercase()).map(|c| c[1].to_string())
    }
}

/// The XML a spot's headers carry: every `X-XML` header's value, in order, joined as they are. None when there's
/// none or it's past `MAX_XML_BYTES`.
pub fn xml_from_headers(head: &[u8]) -> Option<String> {
    let mut xml = Vec::new();
    for line in head.split(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.len() > 7 && line[..7].eq_ignore_ascii_case(b"X-XML: ") {
            xml.extend_from_slice(&line[7..]);
            if xml.len() > MAX_XML_BYTES {
                return None;
            }
        }
    }
    if xml.is_empty() {
        return None;
    }
    Some(match String::from_utf8(xml) {
        Ok(s) => s,
        // older clients wrote Latin-1
        Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
    })
}

/// The spot an XML is, or None when it has no `<Posting>` or no title.
pub fn parse(xml: &str) -> Option<FullSpot> {
    if xml.len() > MAX_XML_BYTES {
        return None;
    }
    // some legacy clients cut the segment's closing tag short (Spotweb #1608)
    let xml =
        xml.replace("spot.net></Segment>", "spot.net</Segment>").replace("spot.ne</Segment>", "spot.net</Segment>");
    let posting = element(&xml, "Posting")?;
    let key = text(posting, "Key").trim().parse().unwrap_or(0);
    let mut category: i32 =
        element(posting, "Category").map(leading_text).unwrap_or_default().trim().parse().unwrap_or(-1);
    // current clients count categories from 1
    if key != 1 {
        category -= 1;
    }
    let mut subs = Subcats::default();
    if let Some(cat) = element(posting, "Category") {
        static SUB: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"(?i)<Sub(?:Cat)?>\s*(\d+[a-dz]\d+)\s*</Sub(?:Cat)?>").unwrap());
        for c in SUB.captures_iter(cat) {
            // `01a15`: the category, then the subcategory; only the letter and its number matter
            let s = &c[1];
            let at = s.find(|ch: char| ch.is_ascii_alphabetic()).unwrap_or(0);
            let one = subcats(&s[at..]);
            for (l, list) in [('a', one.a), ('b', one.b), ('c', one.c), ('d', one.d), ('z', one.z)] {
                for v in list {
                    subs.push(l, v);
                }
            }
        }
    }
    let title = text(posting, "Title");
    if title.is_empty() {
        return None;
    }
    Some(FullSpot {
        key,
        created: text(posting, "Created").trim().parse().unwrap_or(0),
        category,
        subcats: subs,
        title,
        poster: text(posting, "Poster"),
        tag: text(posting, "Tag"),
        description: text(posting, "Description").replace("[br]", "\n"),
        website: text(posting, "Website"),
        size: text(posting, "Size").trim().parse().unwrap_or(0),
        nzb: segments(element(posting, "NZB")),
        image: segments(element(posting, "Image")),
    })
}

/// The inside of the first `<name>` (or `<name attr=..>`) element, up to its first closing tag.
fn element<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let lower = xml.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    let mut from = 0;
    let start = loop {
        let at = from + lower[from..].find(&format!("<{name}"))?;
        let after = at + 1 + name.len();
        match lower.as_bytes().get(after) {
            Some(b'>') => break after + 1,
            Some(b' ' | b'\t' | b'\r' | b'\n') => break after + lower[after..].find('>')? + 1,
            // `<name/>`: there, and empty
            Some(b'/') => return Some(""),
            _ => from = after,
        }
    };
    let end = start + lower[start..].find(&format!("</{name}>"))?;
    Some(&xml[start..end])
}

/// An element's text, CDATA unwrapped and entities decoded, trimmed; empty when it isn't there.
fn text(xml: &str, name: &str) -> String {
    element(xml, name).map(unescape).unwrap_or_default().trim().to_string()
}

/// The text before an element's first child: `<Category>01<Sub>...` is `01`.
fn leading_text(inside: &str) -> &str {
    inside.split('<').next().unwrap_or_default()
}

fn unescape(raw: &str) -> String {
    let raw = raw.trim();
    let raw = raw.strip_prefix("<![CDATA[").and_then(|r| r.strip_suffix("]]>")).unwrap_or(raw);
    decode_entities(raw)
}

/// `&amp;`, `&#39;`, `&#x27;` and the few named HTML entities spot titles use; anything else stays as written.
pub fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let decoded = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => name
                .strip_prefix("#x")
                .or_else(|| name.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The `<Segment>` message-ids inside an element, in order. One invalid id voids the list, as in Spotweb: a list
/// with a hole can't be put back together.
fn segments(inside: Option<&str>) -> Vec<String> {
    static SEGMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<Segment>(.*?)</Segment>").unwrap());
    let Some(inside) = inside else { return Vec::new() };
    let mut out = Vec::new();
    for c in SEGMENT.captures_iter(inside) {
        let id = unescape(&c[1]);
        if !valid_message_id(&id) {
            return Vec::new();
        }
        out.push(id);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0"?>
<Spotnet><Posting><Key>7</Key><Created>1657355034</Created><Poster>Nordicflix</Poster><Tag>nordic</Tag><Title>Thriller - en grym film (1973) 2160p BDRemux &amp; more</Title><Description>Zweden[br]Misdaad</Description><Website>https://www.imdb.com/title/tt0072285/</Website><Image Width="477" Height="643"><Segment>6TqeS1oEOnoFjvJYgYjY6@spot.net</Segment></Image><Size>77594442517</Size><Category>01<Sub>01a15</Sub><Sub>01c06</Sub><Sub>01d05</Sub><Sub>01d07</Sub><Sub>01d20</Sub><Sub>01z00</Sub></Category><NZB><Segment>YBUZv8CCBsIFzvJYg7Xea@spot.net</Segment><Segment>LgnjXJp8ZP0GTvJYghyt3@spot.net</Segment></NZB><PREVSPOTS/></Posting><Extra><Newsreader>Spotnet 1.9.0.8</Newsreader></Extra></Spotnet>"#;

    #[test]
    fn reads_a_spot() {
        let s = parse(XML).unwrap();
        assert_eq!(s.key, 7);
        assert_eq!(s.created, 1_657_355_034);
        assert_eq!(s.category, 0);
        assert_eq!(s.subcats.spotweb(), "a15|c6|d5|d7|d20|z0|");
        assert_eq!(s.kind(), Some(Kind::Movie));
        assert_eq!(s.title, "Thriller - en grym film (1973) 2160p BDRemux & more");
        assert_eq!(s.poster, "Nordicflix");
        assert_eq!(s.tag, "nordic");
        assert_eq!(s.description, "Zweden\nMisdaad");
        assert_eq!(s.size, 77_594_442_517);
        assert_eq!(s.nzb, ["YBUZv8CCBsIFzvJYg7Xea@spot.net", "LgnjXJp8ZP0GTvJYghyt3@spot.net"]);
        assert_eq!(s.image, ["6TqeS1oEOnoFjvJYgYjY6@spot.net"]);
        assert_eq!(s.imdb_id().as_deref(), Some("tt0072285"));
    }

    #[test]
    fn forgiving() {
        // a bare & and a CDATA title, a key-1 category that isn't shifted, a cut segment tag
        let xml = "<Spotnet><Posting><Key>1</Key><Title><![CDATA[Tom & Jerry]]></Title><Category>0<Sub>0a9</Sub><Sub>0b4</Sub></Category><NZB><Segment>abc@spot.net></Segment></NZB></Posting></Spotnet>";
        let s = parse(xml).unwrap();
        assert_eq!(s.title, "Tom & Jerry");
        assert_eq!(s.category, 0);
        assert_eq!(s.kind(), Some(Kind::Series));
        assert_eq!(s.nzb, ["abc@spot.net"]);
    }

    #[test]
    fn bad_segment_voids_the_list() {
        let xml = "<Spotnet><Posting><Title>t</Title><NZB><Segment>a@b</Segment><Segment>nope</Segment></NZB></Posting></Spotnet>";
        assert!(parse(xml).unwrap().nzb.is_empty());
    }

    #[test]
    fn no_spot() {
        assert!(parse("<Spotnet></Spotnet>").is_none());
        assert!(parse("<Spotnet><Posting><Title></Title></Posting></Spotnet>").is_none());
        assert!(parse(&"x".repeat(MAX_XML_BYTES + 1)).is_none());
    }

    #[test]
    fn xml_from_split_headers() {
        let head = b"Path: x\r\nX-XML: <Spotnet><Posting>\r\nSubject: s\r\nX-XML: <Title>A</Title></Posting></Spotnet>\r\nX-XML-Signature: abc\r\n";
        assert_eq!(xml_from_headers(head).unwrap(), "<Spotnet><Posting><Title>A</Title></Posting></Spotnet>");
        assert!(xml_from_headers(b"Subject: s\n").is_none());
        // Latin-1
        assert_eq!(xml_from_headers(b"X-XML: caf\xe9\n").unwrap(), "caf\u{e9}");
    }

    #[test]
    fn entities() {
        assert_eq!(decode_entities("a &amp; b &#39;c&#x27; &bogus; & d"), "a & b 'c' &bogus; & d");
    }

    #[test]
    fn imdb_forms() {
        let s = |w: &str| FullSpot { website: w.into(), ..FullSpot::default() }.imdb_id();
        assert_eq!(s("http://www.imdb.com/title/tt1234567").as_deref(), Some("tt1234567"));
        assert_eq!(s("https://m.imdb.com/nl/title/tt12345678/?ref=x").as_deref(), Some("tt12345678"));
        assert_eq!(s("https://www.themoviedb.org/movie/1"), None);
    }
}
