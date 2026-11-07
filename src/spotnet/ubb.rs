//! A spot's description, or a comment, as HTML. Spotnet texts use UBB (BBCode) the way Spotweb reads it
//! (`lib/ubb`): `[b]`, `[i]`, `[u]`, `[br]`, `[url=..]..[/url]` and `[url]..[/url]`, `[quote]..[/quote]`,
//! `[youtube]..[/youtube]` and smileys `[img=name]`. Everything else is text, escaped. Only http and https links
//! become links, and bare web links in the text do too.

use std::fmt::Write;
use std::sync::LazyLock;

use regex::Regex;

use crate::api::escape;
use crate::web::attr;

/// What a link carries: no referrer, no SEO credit, no window handle.
pub(crate) const REL: &str = "noopener noreferrer nofollow";

/// A UBB tag: `/` when it closes, its name, and its value (`[url=..]`).
static TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\[(/?)(b|i|u|br|url|quote|youtube|img)(?:=([^\]\r\n]{0,2048}))?\]").unwrap());

/// The text as HTML: escaped, its line breaks kept (`[br]` as one), its UBB rendered.
pub fn to_html(text: &str) -> String {
    let text = text.trim();
    let mut out = String::with_capacity(text.len() + 64);
    // the formatting tags open, innermost last: closed in order, unmatched closers dropped, the rest closed at the end
    let mut open: Vec<&'static str> = Vec::new();
    let mut at = 0;
    while let Some(c) = TAG.captures_at(text, at) {
        let whole = c.get(0).unwrap();
        let closing = !c[1].is_empty();
        let name = c[2].to_ascii_lowercase();
        let value = c.get(3).map(|v| v.as_str().trim());
        // a value only on the tags that take one; `[b=x]` is no tag, and stays text
        if value.is_some() && (closing || !matches!(name.as_str(), "url" | "img" | "quote")) {
            plain(&mut out, &text[at..whole.end()]);
            at = whole.end();
            continue;
        }
        let before = &text[at..whole.start()];
        // a quote's tags on lines of their own (one that opens or closes a box): the line breaks around them are the
        // box's, not the text's
        let boxed = name == "quote" && (!closing || open.contains(&"blockquote"));
        let before = if boxed { before.strip_suffix('\n').unwrap_or(before) } else { before };
        plain(&mut out, before);
        at = whole.end();
        if boxed {
            at += usize::from(text[at..].starts_with('\n'));
        }
        match (name.as_str(), closing) {
            ("b" | "i" | "u" | "quote", false) => {
                let tag = element(&name);
                let _ = write!(out, "<{tag}>");
                if let Some(by) = value.filter(|v| !v.is_empty()) {
                    let _ = writeln!(out, "<cite>{}</cite>", escape(by));
                }
                open.push(tag);
            }
            ("b" | "i" | "u" | "quote", true) => close(&mut out, &mut open, element(&name)),
            ("br", false) => out.push('\n'),
            ("img", false) => match value {
                Some(name) if smiley(name) => {
                    let _ = write!(out, ":{}:", name.to_ascii_lowercase());
                }
                // `[img]address[/img]`: images aren't shown; the address, as text
                None => match find_close(text, at, &IMG_CLOSE, &IMG_OPEN) {
                    Some((end, after)) => {
                        plain(&mut out, text[at..end].trim());
                        at = after;
                    }
                    None => plain(&mut out, whole.as_str()),
                },
                Some(_) => plain(&mut out, whole.as_str()),
            },
            ("url", false) => {
                // the text up to its `[/url]`; without one before the next `[url`, `[url=..]` alone is a link named
                // by its address
                let (inner, next) = match find_close(text, at, &URL_CLOSE, &URL_OPEN) {
                    Some((end, after)) => (&text[at..end], after),
                    None => ("", at),
                };
                let (label, closes) = label_of(inner);
                let label = label.trim();
                let address = value.filter(|v| !v.is_empty()).unwrap_or(label);
                if address.is_empty() && label.is_empty() {
                    // `[url]` with nothing to link: the tag, as text
                    plain(&mut out, whole.as_str());
                    at = next;
                    continue;
                }
                match web_url(address) {
                    Some(href) => {
                        let shown = if label.is_empty() { href } else { label };
                        let _ = write!(out, "<a href=\"{}\" rel=\"{REL}\">{}</a>", attr(href), escape(shown));
                    }
                    // not a web link: its text, and the address it gave, as text (a web link in either still linked)
                    None if label.is_empty() || label == address => plain(&mut out, address),
                    None => {
                        plain(&mut out, label);
                        out.push_str(" (");
                        plain(&mut out, address);
                        out.push(')');
                    }
                }
                // formatting opened before the link and closed inside it closes after it
                for tag in closes {
                    close(&mut out, &mut open, tag);
                }
                at = next;
            }
            ("youtube", false) => match find_close(text, at, &YOUTUBE_CLOSE, &YOUTUBE_OPEN) {
                Some((end, after)) if youtube_id(text[at..end].trim()) => {
                    let id = text[at..end].trim();
                    let _ =
                        write!(out, "<a href=\"https://www.youtube.com/watch?v={id}\" rel=\"{REL}\">YouTube {id}</a>");
                    at = after;
                }
                _ => plain(&mut out, whole.as_str()),
            },
            // `[/url]`, `[/br]` and the like with nothing open: dropped
            _ => {}
        }
    }
    plain(&mut out, &text[at..]);
    for t in open.into_iter().rev() {
        let _ = write!(out, "</{t}>");
    }
    out
}

static URL_OPEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\[url[\]=]").unwrap());
static URL_CLOSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\[/url\]").unwrap());
static IMG_OPEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\[img\]").unwrap());
static IMG_CLOSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\[/img\]").unwrap());
static YOUTUBE_OPEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\[youtube\]").unwrap());
static YOUTUBE_CLOSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\[/youtube\]").unwrap());

fn element(name: &str) -> &'static str {
    match name {
        "b" => "b",
        "i" => "i",
        "u" => "u",
        "quote" => "blockquote",
        _ => unreachable!("only the formatting tags have elements"),
    }
}

/// Closes `tag` and what was opened inside it, when it's open; a closer with nothing to close is dropped.
fn close(out: &mut String, open: &mut Vec<&'static str>, tag: &'static str) {
    if let Some(depth) = open.iter().rposition(|t| *t == tag) {
        for t in open.drain(depth..).rev() {
            let _ = write!(out, "</{t}>");
        }
    }
}

/// Where the closer (`close`) after `from` starts and ends, looked for only up to the next opener (`open`).
fn find_close(text: &str, from: usize, close: &Regex, open: &Regex) -> Option<(usize, usize)> {
    let until = open.find_at(text, from).map_or(text.len(), |o| o.start());
    let m = close.find(&text[from..until])?;
    Some((from + m.start(), from + m.end()))
}

/// A link's text, and the formatting it closes that it didn't open. Only text goes in a link, so its UBB tags are
/// read by the same rules as `to_html` reads them and dropped: a smiley still shows, `[br]` is a line break, and a
/// tag with a value it can't take stays text. Other bracketed words stay.
fn label_of(s: &str) -> (String, Vec<&'static str>) {
    let mut out = String::with_capacity(s.len());
    // what the label opens itself: its closers stay inside it
    let (mut inside, mut closes): (Vec<&'static str>, Vec<&'static str>) = (Vec::new(), Vec::new());
    let mut at = 0;
    for c in TAG.captures_iter(s) {
        let whole = c.get(0).unwrap();
        out.push_str(&s[at..whole.start()]);
        at = whole.end();
        let closing = !c[1].is_empty();
        let name = c[2].to_ascii_lowercase();
        let value = c.get(3).map(|v| v.as_str().trim());
        if value.is_some() && (closing || !matches!(name.as_str(), "url" | "img" | "quote")) {
            out.push_str(whole.as_str());
            continue;
        }
        match (name.as_str(), closing) {
            ("img", false) => match value {
                Some(v) if smiley(v) => {
                    let _ = write!(out, ":{}:", v.to_ascii_lowercase());
                }
                Some(_) => out.push_str(whole.as_str()),
                None => {}
            },
            ("br", false) => out.push('\n'),
            ("b" | "i" | "u" | "quote", false) => inside.push(element(&name)),
            ("b" | "i" | "u" | "quote", true) => {
                let tag = element(&name);
                match inside.iter().rposition(|t| *t == tag) {
                    Some(i) => inside.truncate(i),
                    None => closes.push(tag),
                }
            }
            _ => {}
        }
    }
    out.push_str(&s[at..]);
    (out, closes)
}

/// The address, when it's an http or https one, with a host, that can go in a link as it is.
pub(crate) fn web_url(s: &str) -> Option<&str> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    let scheme = ["https://", "http://"].into_iter().find(|p| lower.starts_with(p))?;
    let clean = !s.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '>' | '`'));
    (clean && s.len() > scheme.len()).then_some(s)
}

fn smiley(name: &str) -> bool {
    (1..=32).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn youtube_id(id: &str) -> bool {
    (6..=20).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Text between tags into `out`: escaped, its bare web links made links.
fn plain(out: &mut String, text: &str) {
    static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)\bhttps?://[^\s<>"'\[\]]+"#).unwrap());
    let mut at = 0;
    for m in LINK.find_iter(text) {
        out.push_str(&escape(&text[at..m.start()]));
        let url = link_end(m.as_str());
        let _ = write!(out, "<a href=\"{}\" rel=\"{REL}\">{}</a>", attr(url), escape(url));
        at = m.start() + url.len();
    }
    out.push_str(&escape(&text[at..]));
}

/// A link found in text without what's more likely the sentence's: trailing punctuation, and a closing parenthesis
/// that has no opening one in the link (`(see https://x.org/a)`), while `https://x.org/Heat_(1995)` keeps its own.
fn link_end(found: &str) -> &str {
    let mut url = found;
    loop {
        let trimmed = url.trim_end_matches(['.', ',', ';', ':', '!', '?', '\'', '"']);
        let unbalanced = trimmed.ends_with(')') && trimmed.matches(')').count() > trimmed.matches('(').count();
        let next = if unbalanced { &trimmed[..trimmed.len() - 1] } else { trimmed };
        if next.len() == url.len() {
            return url;
        }
        url = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(href: &str, label: &str) -> String {
        format!("<a href=\"{href}\" rel=\"{REL}\">{label}</a>")
    }

    #[test]
    fn url_tags_become_links() {
        // the owner's description, a stray space before `]` included
        let text = "More see  [url=https://nzbusenet.com/en/spotnet-passwords ]SpotNet passwords[/url]\nAnd [url=https://nzbusenet.com/en/spotnet-tutorial]SpotNet[/url] is essential.";
        let h = to_html(text);
        assert!(h.contains(&link("https://nzbusenet.com/en/spotnet-passwords", "SpotNet passwords")), "{h}");
        assert!(h.contains(&link("https://nzbusenet.com/en/spotnet-tutorial", "SpotNet")), "{h}");
        assert!(!h.contains("[url") && !h.contains("[/url]"), "{h}");
        // `[url]address[/url]`, upper case, and `[url=..]` with no closer
        assert_eq!(to_html("[URL]https://a.org/x[/URL]"), link("https://a.org/x", "https://a.org/x"));
        assert_eq!(to_html("see [url=https://a.org/x]"), format!("see {}", link("https://a.org/x", "https://a.org/x")));
        // a label's tags are dropped, its text escaped, other bracketed words kept
        assert_eq!(to_html("[url=https://a.org/][b]A & B[/b][/url]"), link("https://a.org/", "A &amp; B"));
        assert_eq!(to_html("[url=https://a.org/][HD] Movie[/url]"), link("https://a.org/", "[HD] Movie"));
        // a `[url=..]` with no closer of its own doesn't take the next link's
        assert_eq!(
            to_html("Visit [url=https://a.org] now. Also [url]https://b.org[/url]"),
            format!(
                "Visit {} now. Also {}",
                link("https://a.org", "https://a.org"),
                link("https://b.org", "https://b.org")
            )
        );
        // formatting opened before a link and closed inside it closes after it; the label's own stays in it
        assert_eq!(
            to_html("[b][url=https://a.org]x[/b][/url] rest"),
            format!("<b>{}</b> rest", link("https://a.org", "x"))
        );
        assert_eq!(
            to_html("[b]bold [url=https://a.org][b]x[/b][/url] still[/b] not"),
            format!("<b>bold {} still</b> not", link("https://a.org", "x"))
        );
        // a smiley in a label shows; `[url]` with nothing to link stays text
        assert_eq!(to_html("[url=https://a.org]Thanks [img=klappen][/url]"), link("https://a.org", "Thanks :klappen:"));
        assert_eq!(to_html("click [url] here"), "click [url] here");
        // a label's own `[quote=..]` stays in it; `[br]` in a label is a line break
        assert_eq!(
            to_html("[quote][url=https://a.org][quote=Bob]x[/quote][/url] more[/quote]"),
            format!("<blockquote>{} more</blockquote>", link("https://a.org", "x"))
        );
        assert_eq!(to_html("[url=https://a.org]Part 1[br]Part 2[/url]"), link("https://a.org", "Part 1\nPart 2"));
    }

    #[test]
    fn only_web_links_are_links() {
        // not a web link: the label and the address, as text
        assert_eq!(to_html("[url=javascript:alert(1)]click[/url]"), "click (javascript:alert(1))");
        // its clean start still linked, the rest text
        assert_eq!(
            to_html("[url=https://a.org/\" onclick=x]click[/url]"),
            format!("click ({}\" onclick=x)", link("https://a.org/", "https://a.org/"))
        );
        assert_eq!(to_html("[url]ftp://a.org/f[/url]"), "ftp://a.org/f");
        assert_eq!(to_html("[url=www.example.com]Our site[/url]"), "Our site (www.example.com)");
        // a web link in the label or the address of one that isn't is still linked
        assert_eq!(
            to_html("[url=www.a.org]https://a.org[/url]"),
            format!("{} (www.a.org)", link("https://a.org", "https://a.org"))
        );
        // a scheme alone is no link
        assert_eq!(to_html("[url=https://]Download[/url]"), "Download (https://)");
        // an address with an apostrophe is linked, the apostrophe encoded so it can't end the attribute
        assert_eq!(to_html("[url=https://a.org/'x]y[/url]"), link("https://a.org/&#39;x", "y"));
    }

    #[test]
    fn formatting_quotes_and_smileys() {
        assert_eq!(to_html("[b]bold[/b] [I]it[/I] [u]u[/u]"), "<b>bold</b> <i>it</i> <u>u</u>");
        // closed in order, an unmatched closer dropped, an unclosed one closed at the end
        assert_eq!(to_html("[b][i]x[/b] y[/i]"), "<b><i>x</i></b> y");
        assert_eq!(to_html("[b]open"), "<b>open</b>");
        assert_eq!(to_html("[quote]Said [b]this[/b][/quote] ok"), "<blockquote>Said <b>this</b></blockquote> ok");
        assert_eq!(to_html("a[br]b"), "a\nb");
        // a value only where a tag takes one; a quote's author shown; the line breaks around a quote's tags dropped
        assert_eq!(to_html("[b=x]y"), "[b=x]y");
        assert_eq!(to_html("[quote=Olli <x>]hi[/quote]"), "<blockquote><cite>Olli &lt;x&gt;</cite>\nhi</blockquote>");
        assert_eq!(to_html("a\n[quote]\nhello\n[/quote]\nreply"), "a<blockquote>hello</blockquote>reply");
        // a stray closer keeps the lines around it
        assert_eq!(to_html("one\n[/quote]\ntwo"), "one\n\ntwo");
        assert_eq!(to_html("Thanks![img=klappen]"), "Thanks!:klappen:");
        // an image isn't shown: its address is a link like any other
        assert_eq!(to_html("[img]https://x.org/p.png[/img]"), link("https://x.org/p.png", "https://x.org/p.png"));
        assert_eq!(
            to_html("[img=https://evil/x.png]"),
            format!("[img={}]", link("https://evil/x.png", "https://evil/x.png"))
        );
        assert_eq!(
            to_html("[youtube]dQw4w9WgXcQ[/youtube]"),
            link("https://www.youtube.com/watch?v=dQw4w9WgXcQ", "YouTube dQw4w9WgXcQ")
        );
        assert_eq!(to_html("[youtube]<x>[/youtube]"), "[youtube]&lt;x&gt;");
    }

    #[test]
    fn text_is_escaped_and_bare_links_linked() {
        assert_eq!(to_html("<script>&"), "&lt;script&gt;&amp;");
        assert_eq!(to_html("[unknown]tag[/unknown]"), "[unknown]tag[/unknown]");
        let h = to_html("see https://x.org/Heat_(1995)[img=buigen] and (https://y.org/a).");
        assert_eq!(
            h,
            format!(
                "see {}:buigen: and ({}).",
                link("https://x.org/Heat_(1995)", "https://x.org/Heat_(1995)"),
                link("https://y.org/a", "https://y.org/a")
            )
        );
        assert_eq!(link_end("https://x.org/a_(b))"), "https://x.org/a_(b)");
    }
}
