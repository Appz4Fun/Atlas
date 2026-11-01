//! The control lines of a yEnc article, and what an article's raw form says.

use serde::{Deserialize, Serialize};

/// What the `=ybegin`, `=ypart` and `=yend` lines say.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct YencLines {
    pub name: Option<String>,
    pub file_size: Option<u64>,
    pub line_len: Option<u32>,
    pub part: Option<u32>,
    pub total: Option<u32>,
    pub begin: Option<u64>,
    pub end: Option<u64>,
    pub yend_size: Option<u64>,
    pub pcrc32: Option<u32>,
    pub crc32: Option<u32>,
}

/// `key=value` pairs. `name` runs to the end of the line, soo it's looked for first.
fn value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key}=");
    let mut rest = line;
    let mut offset = 0;
    while let Some(at) = rest.find(&needle) {
        let start = offset + at;
        if start == 0 || line.as_bytes().get(start - 1) == Some(&b' ') {
            let from = start + needle.len();
            return line[from..].split(' ').next();
        }
        offset = start + needle.len();
        rest = &line[offset..];
    }
    None
}

fn hex(line: &str, key: &str) -> Option<u32> {
    u32::from_str_radix(value(line, key)?.trim(), 16).ok()
}

fn number<T: std::str::FromStr>(line: &str, key: &str) -> Option<T> {
    value(line, key)?.trim().parse().ok()
}

/// Reads the lines of an article's body. None: it isn't yEnc.
pub fn parse(lines: &[Vec<u8>]) -> Option<YencLines> {
    let mut out = YencLines::default();
    let mut seen = false;
    for line in lines {
        if line.starts_with(b"=ybegin ") {
            seen = true;
            let text = String::from_utf8_lossy(line);
            if let Some(at) = text.find(" name=") {
                out.name = Some(text[at + 6..].trim().to_string());
            }
            let head = text.split(" name=").next().unwrap_or_default();
            out.file_size = number(head, "size");
            out.line_len = number(head, "line");
            out.part = number(head, "part");
            out.total = number(head, "total");
        } else if line.starts_with(b"=ypart ") {
            let text = String::from_utf8_lossy(line);
            out.begin = number(&text, "begin");
            out.end = number(&text, "end");
        } else if line.starts_with(b"=yend") {
            let text = String::from_utf8_lossy(line);
            out.yend_size = number(&text, "size");
            out.pcrc32 = hex(&text, "pcrc32");
            out.crc32 = hex(&text, "crc32");
        }
    }
    seen.then_some(out)
}

/// An article as `HEAD` and `BODY` together give it.
pub struct RawArticle {
    pub headers: Vec<(String, String)>,
    pub lines: Vec<Vec<u8>>,
}

/// Splits what `article_raw` returned: lines ending in `\n`, a blank line between the two parts.
pub fn split_article(raw: &[u8]) -> RawArticle {
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut lines = Vec::new();
    let mut in_body = false;
    for line in raw.split(|b| *b == b'\n') {
        if in_body {
            lines.push(line.to_vec());
        } else if line.is_empty() {
            in_body = true;
        } else if line.first().is_some_and(|b| *b == b' ' || *b == b'\t') {
            if let Some((_, value)) = headers.last_mut() {
                value.push(' ');
                value.push_str(String::from_utf8_lossy(line).trim());
            }
        } else if let Some(at) = line.iter().position(|b| *b == b':') {
            let name = String::from_utf8_lossy(&line[..at]).trim().to_string();
            let value = String::from_utf8_lossy(&line[at + 1..]).trim().to_string();
            headers.push((name, value));
        }
    }
    if lines.last().is_some_and(Vec::is_empty) {
        lines.pop();
    }
    RawArticle { headers, lines }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> Vec<Vec<u8>> {
        text.iter().map(|l| l.as_bytes().to_vec()).collect()
    }

    #[test]
    fn the_control_lines_come_apart() {
        let y = parse(&lines(&[
            "=ybegin part=1 total=3629 line=128 size=2684354560 name=Batman Begins (2005) size=x.mkv",
            "=ypart begin=1 end=739840",
            "data",
            "=yend size=739840 part=1 pcrc32=8bb98613 crc32=515ad3cc",
        ]))
        .unwrap();
        assert_eq!(y.name.as_deref(), Some("Batman Begins (2005) size=x.mkv"));
        assert_eq!(y.file_size, Some(2_684_354_560));
        assert_eq!((y.part, y.total, y.line_len), (Some(1), Some(3629), Some(128)));
        assert_eq!((y.begin, y.end, y.yend_size), (Some(1), Some(739_840), Some(739_840)));
        assert_eq!((y.pcrc32, y.crc32), (Some(0x8bb9_8613), Some(0x515a_d3cc)));
    }

    #[test]
    fn pcrc32_is_not_read_as_crc32() {
        let y = parse(&lines(&["=ybegin size=3 name=a", "=yend size=3 pcrc32=00000001"])).unwrap();
        assert_eq!((y.pcrc32, y.crc32), (Some(1), None));
        assert_eq!(parse(&lines(&["plain text"])), None);
    }

    #[test]
    fn an_article_splits_at_the_blank_line() {
        let a = split_article(b"From: a@b\nPath: x!\n y\n\nbody 1\n\nbody 2\n");
        assert_eq!(a.headers, [("From".to_string(), "a@b".to_string()), ("Path".to_string(), "x! y".to_string())]);
        assert_eq!(a.lines, [b"body 1".to_vec(), Vec::new(), b"body 2".to_vec()]);
    }
}
