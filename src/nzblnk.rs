//! NZBLNK links, as <https://nzblnk.info/> specifies them: `nzblnk:?t=<title>&h=<header>&g=<group>&p=<password>`.
//!
//! `t` (a title to label the download) and `h` (enough of a post's header to find it) are mandatory; `g` names a
//! group to search and may repeat; `p` is a password the poster gave. Parameters go in any order, encoded the way
//! the spec's own generator does it: percent-encoding with `+` for a space.

/// One NZBLNK's parameters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NzbLnk {
    pub title: String,
    pub header: String,
    pub groups: Vec<String>,
    pub password: Option<String>,
}

impl NzbLnk {
    /// The link, `t` and `h` first, then every `g`, then `p`; empty optional parameters are left out.
    pub fn to_uri(&self) -> String {
        let mut out = format!("nzblnk:?t={}&h={}", encode(&self.title), encode(&self.header));
        for group in self.groups.iter().map(|g| g.trim()).filter(|g| !g.is_empty()) {
            out.push_str("&g=");
            out.push_str(&encode(group));
        }
        if let Some(password) = self.password.as_deref().filter(|p| !p.is_empty()) {
            out.push_str("&p=");
            out.push_str(&encode(password));
        }
        out
    }

    /// A link the spec allows: `nzblnk:` (any case), an optional `?`, then `key=value` pairs. None when it isn't
    /// one or lacks a mandatory `t` or `h`. Unknown parameters are ignored; a repeated `t`, `h` or `p` keeps the
    /// first.
    pub fn parse(text: &str) -> Option<NzbLnk> {
        let text = text.trim();
        let rest = text.get(..7).filter(|s| s.eq_ignore_ascii_case("nzblnk:")).map(|_| &text[7..])?;
        let rest = rest.strip_prefix("//").unwrap_or(rest);
        let rest = rest.strip_prefix('?').unwrap_or(rest);
        let mut link = NzbLnk::default();
        let (mut title, mut header) = (None, None);
        for pair in rest.split('&') {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let value = decode(value);
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            match key.trim() {
                "t" if title.is_none() => title = Some(value.to_string()),
                "h" if header.is_none() => header = Some(value.to_string()),
                "g" => link.groups.push(value.to_string()),
                "p" if link.password.is_none() => link.password = Some(value.to_string()),
                _ => {}
            }
        }
        link.title = title?;
        link.header = header?;
        Some(link)
    }
}

/// What a search box or `q=` should look for when it was handed an NZBLNK: the link's header, which the spec
/// says identifies the post. None when `text` isn't one.
pub fn search_text(text: &str) -> Option<String> {
    NzbLnk::parse(text).map(|l| l.header)
}

/// The `h` for a post from its subject: the release name the subject parser reads, or else the quoted file name
/// without its extension (what the spec's generator takes from an NZB), or else the subject itself.
pub fn header_of(subject: &str) -> String {
    if let Some(parsed) = crate::parser::parse_subject(subject)
        && !parsed.release_name.trim().is_empty()
    {
        return parsed.release_name.trim().to_string();
    }
    let quoted = subject.split('"').nth(1).map(str::trim).filter(|s| !s.is_empty());
    if let Some(name) = quoted {
        let stem = name.split_once('.').map_or(name, |(stem, _)| stem).trim();
        if !stem.is_empty() {
            return stem.to_string();
        }
    }
    subject.trim().to_string()
}

/// `name` with any `{{password}}` taken out, the way a poster's NZB file name carries one; spaces tidied.
pub fn strip_password(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(open) = rest.find("{{") {
        let Some(close) = rest[open + 2..].find("}}") else { break };
        out.push_str(&rest[..open]);
        rest = &rest[open + 2 + close + 2..];
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ").trim_matches(['.', '-', '_', ' ']).to_string()
}

/// Percent-encoding as JavaScript's `encodeURIComponent`, then `%20` as `+`, matching the spec's generator.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b' ' => out.push('+'),
            b if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) => out.push(b as char),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `+` as a space and `%XX` as its byte; a stray `%` is kept as it is.
fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                match std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(v) => {
                        out.push(v);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_example_round_trips() {
        let spec = "nzblnk:?t=Our+Sommervacation&h=fbzzreinpngvba&g=a.b.documentaries&p=v4c4t10n4tw1n";
        let link = NzbLnk::parse(spec).unwrap();
        assert_eq!(link.title, "Our Sommervacation");
        assert_eq!(link.header, "fbzzreinpngvba");
        assert_eq!(link.groups, ["a.b.documentaries"]);
        assert_eq!(link.password.as_deref(), Some("v4c4t10n4tw1n"));
        assert_eq!(link.to_uri(), spec);
    }

    #[test]
    fn encodes_like_the_spec_generator() {
        let link = NzbLnk {
            title: "A Film (2024) & more".into(),
            header: "a/b".into(),
            groups: vec!["alt.binaries.x".into(), " ".into(), "alt.binaries.y".into()],
            password: Some("p@ss word".into()),
        };
        // encodeURIComponent keeps ( ) and turns the space into %20, which the generator then writes as +
        assert_eq!(
            link.to_uri(),
            "nzblnk:?t=A+Film+(2024)+%26+more&h=a%2Fb&g=alt.binaries.x&g=alt.binaries.y&p=p%40ss+word"
        );
        assert_eq!(NzbLnk::parse(&link.to_uri()).unwrap().groups, ["alt.binaries.x", "alt.binaries.y"]);
        let bare = NzbLnk { title: "t".into(), header: "h".into(), groups: vec![], password: Some(String::new()) };
        assert_eq!(bare.to_uri(), "nzblnk:?t=t&h=h", "no empty g or p");
    }

    #[test]
    fn parse_takes_any_order_and_case_and_needs_t_and_h() {
        let link = NzbLnk::parse("  NZBLNK:?g=a.b.x&h=Some%20Name&t=T&x=1&g=a.b.y  ").unwrap();
        assert_eq!((link.title.as_str(), link.header.as_str()), ("T", "Some Name"));
        assert_eq!(link.groups, ["a.b.x", "a.b.y"]);
        assert_eq!(link.password, None);
        assert!(NzbLnk::parse("nzblnk:t=T&h=H").is_some(), "the ? is tolerated missing");
        assert!(NzbLnk::parse("nzblnk:?t=T").is_none(), "h is mandatory");
        assert!(NzbLnk::parse("nzblnk:?h=H&t=").is_none(), "t is mandatory");
        assert!(NzbLnk::parse("magnet:?xt=urn:btih:x").is_none());
        assert!(NzbLnk::parse("nzblnk").is_none());
        assert_eq!(NzbLnk::parse("nzblnk:?t=100%&h=%zz").unwrap().title, "100%", "a stray % stays");
        assert_eq!(search_text("nzblnk:?t=T&h=The.Film.2024.2160p"), Some("The.Film.2024.2160p".into()));
        assert_eq!(search_text("the film"), None);
    }

    #[test]
    fn header_and_password_from_names() {
        assert_eq!(header_of(r#"[01/52] - "Some.Film.2024.part01.rar" yEnc (1/50)"#), "Some.Film.2024");
        assert_eq!(header_of(r#"abc123 "x7f3a9.par2" yEnc"#), "x7f3a9");
        assert_eq!(header_of("  0f3c9a1b2d  "), "0f3c9a1b2d");
        assert_eq!(strip_password("Some.Film.2024{{s3cret}}"), "Some.Film.2024");
        assert_eq!(strip_password("Some Film {{a}} 2024"), "Some Film 2024");
        assert_eq!(strip_password("no {{close"), "no {{close");
    }
}
