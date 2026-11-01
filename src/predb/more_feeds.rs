//! The feeds that aren't plain paged JSON: more JSON shapes, RSS, catalogue XML, a CSV dump, and a few HTML listings.
//!
//! Ported from newznab-tmux `app/Services/Predb/Feeds/Sources/`, GPLv3. Upstream validates each page's whole DOM. Here
//! each listing is read by a pattern for its rows, and a title that isn't one literal name is skipped: a page that
//! changed shape reads as no rows, which is an error, and can't give a wrong title.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::feeds::{ParseError, int, megabytes, number, rows, text};
use super::import::Entry;

const MAX_TITLE_CHARS: usize = 255;
const MAX_RSS_ITEMS: usize = 5_000;
const MAX_CATALOGUE: usize = 5_000;
const NZEDB_FIELDS: usize = 12;

macro_rules! re {
    ($name:ident, $pattern:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pattern).unwrap());
    };
}

re!(ENTITY, r"&(?:#([0-9]{1,7})|#[xX]([0-9a-fA-F]{1,6})|(amp|lt|gt|quot|apos));");
re!(DOCTYPE, r"(?i)<!(?:DOCTYPE|ENTITY)\b");
re!(ITEM, r"(?s)<item(?:\s[^>]*)?>(.*?)</item>");
re!(RSS_TITLE, r"(?s)<title(?:\s[^>]*)?>(.*?)</title>");
re!(RSS_CATEGORY, r"(?s)<category(?:\s[^>]*)?>(.*?)</category>");
re!(RSS_DATE, r"(?s)<pubDate(?:\s[^>]*)?>(.*?)</pubDate>");
re!(SIZE_SUFFIX, r" \x{2013} [0-9]+(?:\.[0-9]+)? (?:MB|GB)$");
re!(EZTV_SUFFIX, r"(?i)\s+EZTV$");
re!(SPACES, r"\s+");
re!(SCENE_NAME, r"^[^.]+\.[^.]+(?:\.[^.]+)*-[\p{L}\p{N}_]+$");
re!(DOTTED_NAME, r"^[^.]+\.[^.]+(?:\.[^.]+)*$");
re!(CROWDNFO_LINK, r#"(?s)<a\s[^>]*href="/release/[1-9][0-9]*"[^>]*>(.*?)</a>"#);
re!(H2, r"(?s)<h2(?:\s[^>]*)?>(.*?)</h2>");
re!(NIMA4K_SUBTITLE, r#"(?s)<span\s[^>]*class="(?:[^"]*\s)?subtitle(?:\s[^"]*)?"[^>]*>([^<]*)</span>"#);
re!(G4U_TODAY, r"(?s)<h1(?:\s[^>]*)?>\s*Updates from today,");
re!(HEADING, r"(?i)<h[12][\s>]");
re!(G4U_TITLE, r#"(?s)<div\s[^>]*class="(?:[^"]*\s)?w3-opacity-min(?:\s[^"]*)?"[^>]*>([^<]*)</div>"#);
re!(GABBER_LIST, r#"(?s)<ul\s[^>]*class="(?:[^"]*\s)?0day-[a-z-]+(?:\s[^"]*)?"[^>]*>(.*?)</ul>"#);
re!(GABBER_NAME, r"(?s)<li(?:\s[^>]*)?>\s*<a\s[^>]*>([^<]*)</a>\s*</li>");
re!(H3, r"(?i)<h3[\s>]");
re!(SAS_ROW, r#"<li\s[^>]*data-rowid="([1-9][0-9]*)""#);
re!(
    SAS_TITLE,
    r#"(?s)<a\s[^>]*href="https://seriousaboutsound\.net/topic/([1-9][0-9]*)-[a-z0-9_-]+/"[^>]*>\s*<span(?:\s[^>]*)?>([^<]*)</span>\s*</a>"#
);
re!(SAS_NAME, r"^[\p{L}\p{N}_][\p{L}\p{M}\p{N}_.()&'+,-]*-[\p{L}\p{N}_]+$");
re!(RELEASE, r"(?s)<release(?:\s[^>]*)?>(.*?)</release>");
re!(RELEASE_ID, r"(?s)<id(?:\s[^>]*)?>\s*([1-9][0-9]*)\s*</id>");
re!(RELEASE_NAME, r"(?s)<releasename(?:\s[^>]*)?>(.*?)</releasename>");
re!(GAME, r#"(?s)<game\s[^>]*name="([^"]*)"[^>]*>(.*?)</game>"#);
re!(ROM, r"<rom[\s/>]");

fn shape(why: &str) -> ParseError {
    ParseError::Shape(why.to_string())
}

/// `&amp;` and friends, and numeric references, in text from XML or HTML. CDATA is taken as it stands.
fn unescape(raw: &str) -> String {
    let raw = raw.trim();
    if let Some(inner) = raw.strip_prefix("<![CDATA[").and_then(|r| r.strip_suffix("]]>")) {
        return inner.trim().to_string();
    }
    ENTITY
        .replace_all(raw, |c: &regex::Captures<'_>| {
            let code = c.get(1).and_then(|d| d.as_str().parse::<u32>().ok());
            let code = code.or_else(|| c.get(2).and_then(|h| u32::from_str_radix(h.as_str(), 16).ok()));
            if let Some(code) = code {
                return char::from_u32(code).map_or(String::new(), String::from);
            }
            match &c[3] {
                "amp" => "&",
                "lt" => "<",
                "gt" => ">",
                "quot" => "\"",
                _ => "'",
            }
            .to_string()
        })
        .into_owned()
}

/// One name as a release has it: bounded, and no whitespace or control characters.
fn literal(title: &str) -> bool {
    !title.is_empty()
        && title.chars().count() <= MAX_TITLE_CHARS
        && !title.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn elided(title: &str) -> bool {
    title.contains("...") || title.contains('…')
}

fn named(title: String) -> Entry {
    Entry { title, ..Entry::default() }
}

fn no_doctype(body: &str) -> Result<(), ParseError> {
    if DOCTYPE.is_match(body) { Err(shape("a document declaring entities")) } else { Ok(()) }
}

#[derive(Clone, Copy, Default)]
pub struct Rss {
    /// titles written with spaces: put the dots back
    pub spaced: bool,
    pub map_category: bool,
    /// its date is when a blog posted it, not when the release was made
    pub listed_only: bool,
    pub strip_size: bool,
}

pub fn parse_rss(body: &str, o: Rss) -> Result<Vec<Entry>, ParseError> {
    no_doctype(body)?;
    if !body.contains("<rss") || !body.contains("<channel") {
        return Err(shape("not an RSS document"));
    }
    let items: Vec<_> = ITEM.captures_iter(body).take(MAX_RSS_ITEMS + 1).collect();
    if items.len() > MAX_RSS_ITEMS {
        return Err(shape("too many RSS items"));
    }
    let mut out = Vec::new();
    for item in &items {
        let block = &item[1];
        let Some(raw) = RSS_TITLE.captures(block) else { continue };
        let mut title = unescape(&raw[1]);
        if o.strip_size {
            title = SIZE_SUFFIX.replace(&title, "").into_owned();
        }
        if o.spaced {
            title = SPACES.replace_all(&EZTV_SUFFIX.replace(title.trim(), ""), ".").into_owned();
        }
        if title.is_empty() || title.contains(' ') {
            continue;
        }
        let category = if o.map_category {
            RSS_CATEGORY.captures(block).map(|c| unescape(&c[1])).filter(|c| !c.is_empty())
        } else {
            None
        };
        let pre_at = if o.listed_only {
            None
        } else {
            RSS_DATE
                .captures(block)
                .and_then(|c| chrono::DateTime::parse_from_rfc2822(unescape(&c[1]).as_str()).ok())
                .map(|at| at.timestamp())
        };
        out.push(Entry { title, category, pre_at, ..Entry::default() });
    }
    Ok(out)
}

pub fn parse_predb_ovh(v: &Value, _page: u32) -> Result<Vec<Entry>, ParseError> {
    super::feeds::envelope(v)?;
    let mut out = Vec::new();
    for r in rows(v, "/data/rows")? {
        let Some(title) = text(r.get("name")).map(|t| t.trim().to_string()) else { continue };
        let nuke = r.get("nuke").filter(|n| n.is_object()).filter(|n| {
            // a nuke of some other pre isn't this one's
            match (int(r.get("id")), int(n.get("preId"))) {
                (Some(id), Some(pre)) => id == pre,
                _ => true,
            }
        });
        let type_id = nuke.and_then(|n| int(n.get("typeId"))).unwrap_or(0);
        let nuked = match type_id {
            0 => 0,
            2 | 5 => 2,
            3 => 3,
            _ => 1,
        };
        out.push(Entry {
            title,
            category: text(r.get("cat")),
            size_bytes: number(r.get("size")).filter(|n| *n >= 0.0).map(|kb| (kb * 1024.0) as i64),
            files: int(r.get("files")),
            pre_at: int(r.get("preAt")),
            nuked,
            nuke_reason: if type_id == 0 { None } else { nuke.and_then(|n| text(n.get("reason"))) },
        });
    }
    Ok(out)
}

/// Its `date` is when srrDB listed it, not a pre time.
pub fn parse_srrdb(v: &Value, _page: u32) -> Result<Vec<Entry>, ParseError> {
    Ok(rows(v, "/results")?
        .iter()
        .filter_map(|r| text(r.get("release")))
        .map(|t| named(t.trim().to_string()))
        .collect())
}

pub fn parse_predatabase(v: &Value, _page: u32) -> Result<Vec<Entry>, ParseError> {
    let mut out = Vec::new();
    for r in rows(v, "/results")? {
        let Some(title) = text(r.get("rlsname")).map(|t| t.trim().to_string()) else { continue };
        out.push(Entry {
            title,
            category: text(r.get("section")),
            size_bytes: megabytes(r.get("size")),
            files: int(r.get("files")),
            pre_at: int(r.get("ctime")).filter(|t| *t > 0),
            ..Entry::default()
        });
    }
    Ok(out)
}

pub fn parse_apibay(v: &Value, _page: u32) -> Result<Vec<Entry>, ParseError> {
    let mut out = Vec::new();
    for r in rows(v, "")? {
        let Some(title) = text(r.get("name")).map(|t| t.trim().to_string()) else { continue };
        let category = match int(r.get("category")) {
            Some(201) => "MOVIE",
            Some(207) => "MOVIE-HD",
            Some(205) => "TV",
            Some(208) => "TV-HD",
            _ => continue,
        };
        out.push(Entry {
            title,
            category: Some(category.to_string()),
            size_bytes: int(r.get("size")).filter(|n| *n >= 0),
            files: int(r.get("num_files")),
            ..Entry::default()
        });
    }
    Ok(out)
}

/// The movie browse: a hit in another category, or a title that isn't one dotted name, is skipped.
pub fn parse_knaben(v: &Value, _page: u32) -> Result<Vec<Entry>, ParseError> {
    let mut out = Vec::new();
    for hit in rows(v, "/hits")? {
        let Some(title) = hit.get("title").and_then(Value::as_str) else { continue };
        let movie = hit
            .get("categoryId")
            .and_then(Value::as_array)
            .is_some_and(|c| c.iter().any(|i| i.as_i64() == Some(3_000_000)));
        if !movie || elided(title) || !literal(title) || !DOTTED_NAME.is_match(title) {
            continue;
        }
        out.push(Entry { title: title.to_string(), category: text(hit.get("category")), ..Entry::default() });
    }
    Ok(out)
}

/// The cards under "Recent Releases", up to the next section.
pub fn parse_crowdnfo(body: &str) -> Result<Vec<Entry>, ParseError> {
    let heading = H2
        .captures_iter(body)
        .find(|c| c[1].trim() == "Recent Releases")
        .and_then(|c| c.get(0))
        .ok_or_else(|| shape("no Recent Releases heading"))?;
    let rest = &body[heading.end()..];
    let scope = H2.find(rest).map_or(rest, |next| &rest[..next.start()]);
    let out: Vec<_> = CROWDNFO_LINK
        .captures_iter(scope)
        .map(|c| unescape(&c[1]))
        .filter(|t| literal(t) && !elided(t) && SCENE_NAME.is_match(t))
        .map(named)
        .collect();
    if out.is_empty() { Err(shape("no release cards")) } else { Ok(out) }
}

/// A card's subtitle, when its cover image says the same. Anything else named `subtitle` doesn't.
pub fn parse_nima4k(body: &str) -> Result<Vec<Entry>, ParseError> {
    let out: Vec<_> = NIMA4K_SUBTITLE
        .captures_iter(body)
        .filter(|c| body.contains(&format!("alt=\"{}\"", c[1].trim())))
        .map(|c| unescape(&c[1]))
        .filter(|t| literal(t))
        .map(named)
        .collect();
    if out.is_empty() { Err(shape("no release cards")) } else { Ok(out) }
}

/// The cards under "Updates from today", up to the next heading.
pub fn parse_g4u(body: &str) -> Result<Vec<Entry>, ParseError> {
    let today = G4U_TODAY.find(body).ok_or_else(|| shape("no today heading"))?;
    let rest = &body[today.end()..];
    let scope = HEADING.find(rest).map_or(rest, |next| &rest[..next.start()]);
    Ok(G4U_TITLE.captures_iter(scope).map(|c| unescape(&c[1])).filter(|t| literal(t)).map(named).collect())
}

/// The latest day's genre lists, each name once.
pub fn parse_gabber(body: &str) -> Result<Vec<Entry>, ParseError> {
    let view = body.find("view-id-0day_releases").ok_or_else(|| shape("no release view"))?;
    let rest = &body[view..];
    let first = H3.find(rest).ok_or_else(|| shape("no latest day"))?;
    let rest = &rest[first.end()..];
    let scope = H3.find(rest).map_or(rest, |next| &rest[..next.start()]);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for list in GABBER_LIST.captures_iter(scope) {
        for name in GABBER_NAME.captures_iter(&list[1]) {
            let name = unescape(&name[1]);
            if literal(&name) && seen.insert(name.to_lowercase()) {
                out.push(named(name));
            }
        }
    }
    if out.is_empty() { Err(shape("no directory names")) } else { Ok(out) }
}

/// Topics posted by its release bot. The pinned one and anything by a person are skipped.
pub fn parse_seriousaboutsound(body: &str) -> Result<Vec<Entry>, ParseError> {
    let starts: Vec<_> =
        SAS_ROW.captures_iter(body).filter_map(|c| Some((c.get(0)?.start(), c[1].to_string()))).collect();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (i, (start, id)) in starts.iter().enumerate() {
        let end = starts.get(i + 1).map_or(body.len(), |(next, _)| *next);
        let row = &body[*start..end];
        if row.contains("title=\"Pinned\"") || !row.contains("/profile/1294-rls-bot/") {
            continue;
        }
        let Some(c) = SAS_TITLE.captures(row) else { continue };
        let title = unescape(&c[2]);
        let canonical = literal(&title) && !elided(&title) && SAS_NAME.is_match(&title) && title.contains(['.', '_']);
        if &c[1] == id && canonical && seen.insert(title.to_lowercase()) {
            out.push(named(title));
        }
    }
    if out.is_empty() { Err(shape("no release topics")) } else { Ok(out) }
}

/// nswdb and 3dsdb: a whole catalogue, newest id first.
pub fn parse_catalogue(body: &str) -> Result<Vec<Entry>, ParseError> {
    no_doctype(body)?;
    if !body.contains("<releases") {
        return Err(shape("not a release catalogue"));
    }
    let mut rows: Vec<(i64, String)> = Vec::new();
    for (count, r) in RELEASE.captures_iter(body).enumerate() {
        if count >= MAX_CATALOGUE {
            return Err(shape("over the record limit"));
        }
        let id =
            RELEASE_ID.captures(&r[1]).and_then(|c| c[1].parse().ok()).ok_or_else(|| shape("a record with no id"))?;
        let Some(title) = RELEASE_NAME.captures(&r[1]).map(|c| unescape(&c[1])) else { continue };
        if literal(&title) {
            rows.push((id, title));
        }
    }
    if rows.is_empty() {
        return Err(shape("no records"));
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.0));
    let mut seen = HashSet::new();
    Ok(rows.into_iter().filter(|(_, t)| seen.insert(t.to_lowercase())).map(|(_, t)| named(t)).collect())
}

/// A DAT export: a game of one rom is one release, by its name. A game of many isn't a release.
pub fn parse_dat(body: &str) -> Result<Vec<Entry>, ParseError> {
    no_doctype(body)?;
    if !body.contains("<datafile") {
        return Err(shape("not a DAT export"));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (count, g) in GAME.captures_iter(body).enumerate() {
        if count >= MAX_CATALOGUE {
            return Err(shape("over the record limit"));
        }
        let title = unescape(&g[1]);
        if ROM.find_iter(&g[2]).count() == 1 && literal(&title) && seen.insert(title.to_lowercase()) {
            out.push(named(title));
        }
    }
    if out.is_empty() { Err(shape("no games")) } else { Ok(out) }
}

/// A MySQL OUTFILE field: `\N`, or quoted with backslash escapes. The next offset with it.
fn outfile_field(csv: &[u8], mut at: usize) -> Result<(Option<String>, usize), ParseError> {
    if csv[at..].starts_with(b"\\N") {
        return Ok((None, at + 2));
    }
    if csv.get(at) != Some(&b'\'') {
        return Err(shape("an invalid dump field"));
    }
    at += 1;
    let mut value = Vec::new();
    while let Some(&b) = csv.get(at) {
        at += 1;
        match b {
            b'\'' => return Ok((Some(String::from_utf8_lossy(&value).into_owned()), at)),
            b'\\' => {
                let escaped = *csv.get(at).ok_or_else(|| shape("a truncated dump escape"))?;
                at += 1;
                value.push(match escaped {
                    b'0' => 0,
                    b'b' => 8,
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'Z' => 0x1a,
                    b'\'' | b'"' | b'\\' => escaped,
                    _ => return Err(shape("an invalid dump escape")),
                });
            }
            other => value.push(other),
        }
    }
    Err(shape("an unterminated dump field"))
}

/// nZEDb's dump: twelve fields a row, the first the title and the eighth the category.
pub fn parse_nzedb(body: &str) -> Result<Vec<Entry>, ParseError> {
    let csv = body.as_bytes();
    let (mut at, mut records) = (0, 0);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    while at < csv.len() {
        records += 1;
        if records > MAX_CATALOGUE {
            return Err(shape("over the record limit"));
        }
        let mut fields = Vec::with_capacity(NZEDB_FIELDS);
        for column in 0..NZEDB_FIELDS {
            let (field, next) = outfile_field(csv, at)?;
            fields.push(field);
            let separator: &[u8] = if column == NZEDB_FIELDS - 1 { b"\r\n" } else { b"\t\t" };
            if !csv[next..].starts_with(separator) {
                return Err(shape("a dump row without twelve fields"));
            }
            at = next + 2;
        }
        let Some(title) = fields[0].take() else { continue };
        if title.is_empty()
            || title != title.trim()
            || title.chars().count() > MAX_TITLE_CHARS
            || title.chars().any(char::is_control)
            || !seen.insert(title.to_lowercase())
        {
            continue;
        }
        let category = fields[7].take().filter(|c| !c.is_empty() && !c.chars().any(char::is_control));
        out.push(Entry { title, category, ..Entry::default() });
    }
    if out.is_empty() { Err(shape("no records")) } else { Ok(out) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn titles(got: &[Entry]) -> Vec<&str> {
        got.iter().map(|e| e.title.as_str()).collect()
    }

    const RSS_DOC: &str = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Feed</title>
      <item><title>Some.Show.S01E02.720p-GRP</title><category>TV</category><pubDate>Mon, 02 Mar 2026 10:00:00 +0000</pubDate></item>
      <item><title><![CDATA[Some.Movie.2019.1080p-GRP]]></title><category>Movies</category></item>
      <item><title>Words With Spaces</title></item>
      <item><title></title></item>
    </channel></rss>"#;

    #[test]
    fn rss_rows_and_dates() {
        let got = parse_rss(RSS_DOC, Rss::default()).unwrap();
        assert_eq!(titles(&got), ["Some.Show.S01E02.720p-GRP", "Some.Movie.2019.1080p-GRP"]);
        assert_eq!(got[0].pre_at, Some(1_772_445_600));
        assert_eq!(got[0].category, None, "not asked for");
    }

    #[test]
    fn rss_options() {
        let got = parse_rss(RSS_DOC, Rss { map_category: true, listed_only: true, ..Rss::default() }).unwrap();
        assert_eq!((got[0].category.as_deref(), got[0].pre_at), (Some("TV"), None));

        let doc = r#"<rss><channel><item><title>Some Show S01E02 720p EZTV</title></item></channel></rss>"#;
        assert_eq!(titles(&parse_rss(doc, Rss { spaced: true, ..Rss::default() }).unwrap()), ["Some.Show.S01E02.720p"]);

        let doc = "<rss><channel><item><title>Some.Movie-GRP \u{2013} 1.5 GB</title></item></channel></rss>";
        assert_eq!(titles(&parse_rss(doc, Rss { strip_size: true, ..Rss::default() }).unwrap()), ["Some.Movie-GRP"]);
    }

    #[test]
    fn rss_refuses_what_isnt_one() {
        assert!(parse_rss("<html></html>", Rss::default()).is_err());
        let entities = r#"<!DOCTYPE x [<!ENTITY a "b">]><rss><channel></channel></rss>"#;
        assert!(parse_rss(entities, Rss::default()).is_err());
        assert_eq!(parse_rss("<rss><channel></channel></rss>", Rss::default()).unwrap(), []);
    }

    #[test]
    fn entities_decode() {
        assert_eq!(unescape(" A&amp;B&#46;C&#x2e;D "), "A&B.C.D");
    }

    #[test]
    fn predb_ovh_rows() {
        let page = json!({"status": "success", "data": {"rows": [
            {"id": 1, "name": "A.Release-GRP", "cat": "TV", "size": 2048, "files": 5, "preAt": 100, "nuke": null},
            {"id": 2, "name": "B.Release-GRP", "nuke": {"preId": 2, "typeId": 1, "reason": "dupe"}},
            {"id": 3, "name": "C.Release-GRP", "nuke": {"preId": 3, "typeId": 2, "reason": "fine"}},
            {"id": 4, "name": "D.Release-GRP", "nuke": {"preId": 9, "typeId": 1, "reason": "other"}},
            {"id": 5, "name": ""},
        ]}});
        let got = parse_predb_ovh(&page, 1).unwrap();
        assert_eq!(got.len(), 4);
        assert_eq!((got[0].size_bytes, got[0].files, got[0].pre_at), (Some(2_097_152), Some(5), Some(100)));
        assert_eq!((got[1].nuked, got[1].nuke_reason.as_deref()), (1, Some("dupe")));
        assert_eq!(got[2].nuked, 2);
        assert_eq!((got[3].nuked, got[3].nuke_reason.as_deref()), (0, None), "another pre's nuke");
        let limited = json!({"status": "error", "message": "rate limit exceeded"});
        assert!(matches!(parse_predb_ovh(&limited, 1), Err(ParseError::RateLimited(_))));
    }

    #[test]
    fn srrdb_rows_have_no_pre_time() {
        let page = json!({"results": [{"release": "A.Release-GRP", "date": "2026-01-02 03:04:05"}, {"release": ""}]});
        let got = parse_srrdb(&page, 1).unwrap();
        assert_eq!((titles(&got), got[0].pre_at), (vec!["A.Release-GRP"], None));
        assert!(parse_srrdb(&json!({}), 1).is_err());
    }

    #[test]
    fn predatabase_rows() {
        let page = json!({"results": [{"rlsname": "A.Release-GRP", "section": "TV", "size": 2, "files": 3, "ctime": 7}, {"rlsname": "B.Release-GRP", "ctime": 0}]});
        let got = parse_predatabase(&page, 1).unwrap();
        assert_eq!((got[0].size_bytes, got[0].pre_at), (Some(2_097_152), Some(7)));
        assert_eq!(got[1].pre_at, None);
    }

    #[test]
    fn apibay_keeps_its_video_categories() {
        let page = json!([
            {"name": "A.Movie-GRP", "category": "207", "size": "1000", "num_files": "2", "added": "5"},
            {"name": "An.Album-GRP", "category": "101"},
        ]);
        let got = parse_apibay(&page, 1).unwrap();
        assert_eq!(titles(&got), ["A.Movie-GRP"]);
        assert_eq!(
            (got[0].category.as_deref(), got[0].size_bytes, got[0].files),
            (Some("MOVIE-HD"), Some(1000), Some(2))
        );
    }

    #[test]
    fn knaben_takes_literal_movie_names() {
        let page = json!({"total": {"relation": "eq", "value": 5}, "hits": [
            {"title": "A.Movie.2019-GRP", "categoryId": [3000000], "category": "Movies"},
            {"title": "A Movie 2019", "categoryId": [3000000]},
            {"title": "A.Movie...", "categoryId": [3000000]},
            {"title": "An.Album-GRP", "categoryId": [1000000]},
            {"title": "NoDots", "categoryId": [3000000]},
        ]});
        assert_eq!(titles(&parse_knaben(&page, 1).unwrap()), ["A.Movie.2019-GRP"]);
    }

    #[test]
    fn crowdnfo_reads_only_recent_releases() {
        let doc = r#"<h2>Top</h2><a href="/release/1">Old.Release-GRP</a>
          <h2> Recent Releases </h2><div class="row"><div><a class="x" href="/release/2">A.Release.2019-GRP</a></div>
          <div><a href="/release/3">A.Longer.Na...</a></div><div><a href="/about">Not.One-GRP</a></div></div>
          <h2>Other</h2><a href="/release/4">Later.Release-GRP</a>"#;
        assert_eq!(titles(&parse_crowdnfo(doc).unwrap()), ["A.Release.2019-GRP"]);
        assert!(parse_crowdnfo("<h2>Nothing</h2>").is_err());
    }

    #[test]
    fn nima4k_needs_its_cover_to_agree() {
        let doc = r#"<div class="article"><div class="cover-box"><img alt="A.Release.2019-GRP"></div>
          <div class="article-right"><h2><span class="subtitle">A.Release.2019-GRP</span></h2></div></div>
          <div class="article"><div class="cover-box"><img alt="Other"></div>
          <div class="article-right"><h2><span class="subtitle">Not.The.Same-GRP</span></h2></div></div>"#;
        assert_eq!(titles(&parse_nima4k(doc).unwrap()), ["A.Release.2019-GRP"]);
        assert!(parse_nima4k("<div></div>").is_err());
    }

    #[test]
    fn g4u_reads_only_today() {
        let doc = r#"<h1>Updates from today, 1 Nov <span class="w3-right">1</span></h1>
          <a href="/en/1-x"><ul class="mgame"><li class="mtitel"><div class="w3-opacity-min">A_Game-GRP</div></li></ul></a>
          <h1>Yesterday</h1><a><div class="w3-opacity-min">Old_Game-GRP</div></a>"#;
        assert_eq!(titles(&parse_g4u(doc).unwrap()), ["A_Game-GRP"]);
        assert!(parse_g4u("<h1>Nope</h1>").is_err());
    }

    #[test]
    fn gabber_reads_the_latest_day() {
        let doc = r#"<div class="view-id-0day_releases"><div class="view-content"><h3><a>2026-11-01</a></h3>
          <div class="views-row"><ul class="item 0day-hardcore"><li><a href="/x">Some_Artist-Track-WEB-2026-GRP</a></li>
          <li><a href="/y">SOME_ARTIST-TRACK-WEB-2026-GRP</a></li></ul></div>
          <h3><a>2026-10-31</a></h3><ul class="0day-hardstyle"><li><a>Old_Track-GRP</a></li></ul></div></div>"#;
        assert_eq!(titles(&parse_gabber(doc).unwrap()), ["Some_Artist-Track-WEB-2026-GRP"]);
        assert!(parse_gabber("<div></div>").is_err());
    }

    #[test]
    fn seriousaboutsound_takes_its_bots_topics() {
        let row = |id: i64, name: &str, author: &str, pinned: &str| {
            format!(
                r#"<li class="r" data-rowid="{id}">{pinned}<h4><span class="ipsType_break"><a href="https://seriousaboutsound.net/topic/{id}-slug/"><span>{name}</span></a></span></h4><a href="https://seriousaboutsound.net/profile/{author}/">x</a></li>"#
            )
        };
        let doc = [
            row(1, "An_Artist-Album-WEB-2026-GRP", "1294-rls-bot", ""),
            row(2, "A_Person-Post-2026-GRP", "5-someone", ""),
            row(3, "A_Pinned-Post-2026-GRP", "1294-rls-bot", r#"<i title="Pinned"></i>"#),
            row(4, "Not a release", "1294-rls-bot", ""),
        ]
        .concat();
        assert_eq!(titles(&parse_seriousaboutsound(&doc).unwrap()), ["An_Artist-Album-WEB-2026-GRP"]);
        assert!(parse_seriousaboutsound("<ol></ol>").is_err());
    }

    #[test]
    fn a_catalogue_goes_newest_first_once() {
        let doc = r#"<releases>
          <release><id>1</id><releasename>Old.Game-GRP</releasename></release>
          <release><id>3</id><releasename>New.Game-GRP</releasename></release>
          <release><id>2</id><releasename>NEW.GAME-GRP</releasename></release>
          <release><id>4</id><releasename>Has Spaces</releasename></release>
        </releases>"#;
        assert_eq!(titles(&parse_catalogue(doc).unwrap()), ["New.Game-GRP", "Old.Game-GRP"]);
        assert!(parse_catalogue("<releases></releases>").is_err());
        assert!(parse_catalogue("<releases><release><releasename>X-Y</releasename></release></releases>").is_err());
    }

    #[test]
    fn a_dat_game_of_one_rom_is_a_release() {
        let doc = r#"<datafile><header></header>
          <game name="A_Game-GRP"><description>A_Game-GRP</description><rom name="a.zip" mia="yes"/></game>
          <game name="Two_Roms-GRP"><rom name="a"/><rom name="b"/></game>
          <game name="Has Spaces"><rom name="c"/></game></datafile>"#;
        assert_eq!(titles(&parse_dat(doc).unwrap()), ["A_Game-GRP"]);
        assert!(parse_dat("<other/>").is_err());
    }

    #[test]
    fn an_nzedb_dump_row() {
        let row = |title: &str, category: &str| {
            let mut fields = vec![format!("'{title}'")];
            fields.extend((1..12).map(|i| if i == 7 { format!("'{category}'") } else { "\\N".to_string() }));
            fields.join("\t\t") + "\r\n"
        };
        let doc = [row("A.Release-GRP", "TV"), row("It\\'s.A.Release-GRP", ""), row("A.RELEASE-GRP", "TV")].concat();
        let got = parse_nzedb(&doc).unwrap();
        assert_eq!(titles(&got), ["A.Release-GRP", "It's.A.Release-GRP"]);
        assert_eq!((got[0].category.as_deref(), got[1].category.as_deref()), (Some("TV"), None));
        assert!(parse_nzedb("'only one field'\r\n").is_err());
        assert!(parse_nzedb("'unterminated").is_err());
    }
}
