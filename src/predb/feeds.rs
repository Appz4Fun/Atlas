//! The JSON feeds: where each one is, and what its rows look like.
//!
//! Ported from newznab-tmux `app/Services/Predb/Feeds/Sources/{PredbClub,PredbNet,PredbOrg,Xrel}Source.php`, GPLv3.

use serde_json::Value;

use super::import::Entry;

#[derive(Debug, PartialEq)]
pub enum ParseError {
    RateLimited(String),
    Shape(String),
}

/// more than any feed sends: nothing of such a page is imported
pub const MAX_PAGE_ROWS: usize = 1000;
const MIB: f64 = 1_048_576.0;

pub struct FeedSpec {
    /// stored in titles.source and feeds.source. Never reused
    pub id: i64,
    /// the config name
    pub key: &'static str,
    pub endpoint: &'static str,
    pub page_size_param: &'static str,
    pub min_page_size: u32,
    pub max_page_size: u32,
    pub extra_query: &'static str,
    /// never ask past this
    pub max_page: Option<u32>,
    pub min_delay_ms: u64,
    pub parse: fn(&Value, u32) -> Result<Vec<Entry>, ParseError>,
}

pub const SOURCES: &[FeedSpec] = &[
    FeedSpec {
        id: 1,
        key: "predb_club",
        endpoint: "https://predb.club/api/v1/",
        page_size_param: "count",
        min_page_size: 1,
        max_page_size: 100,
        extra_query: "",
        max_page: None,
        min_delay_ms: 1_000,
        parse: parse_predb_club,
    },
    FeedSpec {
        id: 2,
        key: "predb_net",
        endpoint: "https://api.predb.net/",
        page_size_param: "limit",
        min_page_size: 1,
        max_page_size: 100,
        extra_query: "",
        // page 101 is HTTP 400
        max_page: Some(100),
        min_delay_ms: 1_000,
        parse: parse_predb_net,
    },
    FeedSpec {
        id: 3,
        key: "predb_org",
        endpoint: "https://predb.org/api/search",
        page_size_param: "limit",
        min_page_size: 1,
        max_page_size: 100,
        extra_query: "q=*",
        max_page: None,
        min_delay_ms: 1_000,
        parse: parse_predb_org,
    },
    FeedSpec {
        id: 4,
        key: "xrel",
        endpoint: "https://api.xrel.to/v2/release/latest.json",
        page_size_param: "per_page",
        min_page_size: 5,
        max_page_size: 100,
        extra_query: "",
        max_page: None,
        // 900 requests an hour
        min_delay_ms: 4_000,
        parse: parse_xrel,
    },
    FeedSpec {
        id: 5,
        key: "xrel_p2p",
        endpoint: "https://api.xrel.to/v2/p2p/releases.json",
        page_size_param: "per_page",
        min_page_size: 5,
        max_page_size: 100,
        extra_query: "",
        max_page: None,
        min_delay_ms: 4_000,
        parse: parse_xrel_p2p,
    },
];

pub fn spec(key: &str) -> Option<&'static FeedSpec> {
    SOURCES.iter().find(|s| s.key == key)
}

/// endpoint?extra&param=N&page=P, clamped
pub fn page_url(spec: &FeedSpec, page: u32, page_size: u32) -> String {
    let size = page_size.clamp(spec.min_page_size, spec.max_page_size);
    let extra = if spec.extra_query.is_empty() { String::new() } else { format!("{}&", spec.extra_query) };
    format!("{}?{extra}{}={size}&page={page}", spec.endpoint, spec.page_size_param)
}

/// numbers may arrive as JSON strings
fn number(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|n: &f64| n.is_finite())
}

fn int(v: Option<&Value>) -> Option<i64> {
    number(v).map(|n| n as i64)
}

/// an empty string is None
fn text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn megabytes(v: Option<&Value>) -> Option<i64> {
    number(v).filter(|n| *n >= 0.0).map(|n| (n * MIB) as i64)
}

fn envelope(v: &Value) -> Result<(), ParseError> {
    if v.get("status").and_then(Value::as_str) != Some("error") {
        return Ok(());
    }
    let message = v.get("message").and_then(Value::as_str).unwrap_or("the feed reported an error").to_string();
    if message.to_lowercase().contains("rate limit") {
        Err(ParseError::RateLimited(message))
    } else {
        Err(ParseError::Shape(message))
    }
}

fn rows<'a>(v: &'a Value, pointer: &str) -> Result<&'a Vec<Value>, ParseError> {
    let list = v
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| ParseError::Shape(format!("no list at {pointer}")))?;
    if list.len() > MAX_PAGE_ROWS {
        return Err(ParseError::Shape(format!("{} rows on one page", list.len())));
    }
    Ok(list)
}

/// a row with no usable title is skipped, not an error
fn map(list: &[Value], key: &str, row: impl Fn(&Value, String) -> Entry) -> Vec<Entry> {
    list.iter().filter_map(|r| text(r.get(key)).map(|title| row(r, title))).collect()
}

pub fn parse_predb_club(v: &Value, _page: u32) -> Result<Vec<Entry>, ParseError> {
    envelope(v)?;
    let list = rows(v, "/data/rows")?;
    Ok(map(list, "name", |r, title| {
        let nuke = r.get("nuke").filter(|n| n.is_object());
        let nuked = nuke.map_or(0, |n| match n.get("type").and_then(Value::as_str).unwrap_or("") {
            "unnuke" | "undelpre" => 2,
            "modnuke" => 3,
            "renuke" => 4,
            "oldnuke" => 5,
            _ => 1,
        });
        Entry {
            title,
            category: text(r.get("cat")),
            size_bytes: megabytes(r.get("size")),
            files: int(r.get("files")),
            pre_at: int(r.get("preAt")),
            nuked,
            nuke_reason: nuke.and_then(|n| text(n.get("reason"))),
        }
    }))
}

pub fn parse_predb_net(v: &Value, _page: u32) -> Result<Vec<Entry>, ParseError> {
    envelope(v)?;
    let list = rows(v, "/data")?;
    Ok(map(list, "release", |r, title| {
        let status = int(r.get("status")).unwrap_or(0);
        let nuked = match status {
            0 => 0,
            2 | 4 => 2,
            _ => 1,
        };
        Entry {
            title,
            category: text(r.get("section")),
            size_bytes: megabytes(r.get("size")),
            files: int(r.get("files")),
            pre_at: int(r.get("pretime")),
            nuked,
            nuke_reason: if nuked != 0 { text(r.get("reason")) } else { None },
        }
    }))
}

pub fn parse_predb_org(v: &Value, _page: u32) -> Result<Vec<Entry>, ParseError> {
    let list = rows(v, "/values")?;
    Ok(map(list, "rlsname", |r, title| {
        let reason = text(r.get("nukereason"));
        let nuked = i64::from(int(r.get("status")) == Some(1) || reason.is_some());
        Entry {
            title,
            category: text(r.get("section")),
            size_bytes: megabytes(r.get("size")),
            files: int(r.get("files")),
            pre_at: int(r.get("ctime")),
            nuked,
            nuke_reason: reason,
        }
    }))
}

/// a page past `pagination.total_pages` is the end of the source, whatever else the body holds
fn past_the_last_page(v: &Value, page: u32) -> bool {
    int(v.pointer("/pagination/total_pages")).is_some_and(|total| total > 0 && i64::from(page) > total)
}

fn xrel_size(size: Option<&Value>) -> Option<i64> {
    let size = size?;
    let n = number(size.get("number")).filter(|n| *n >= 0.0)?;
    let scale = match size.get("unit").and_then(Value::as_str)? {
        "KB" => 1024.0,
        "MB" => MIB,
        "GB" => MIB * 1024.0,
        "TB" => MIB * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((n * scale) as i64)
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::String(s)) => !s.is_empty() && s != "0" && s != "false",
        _ => false,
    }
}

pub fn parse_xrel(v: &Value, page: u32) -> Result<Vec<Entry>, ParseError> {
    if past_the_last_page(v, page) {
        return Ok(Vec::new());
    }
    let list = rows(v, "/list")?;
    Ok(map(list, "dirname", |r, title| Entry {
        title,
        category: text(r.pointer("/ext_info/type")).map(|c| c.to_uppercase()),
        size_bytes: xrel_size(r.get("size")),
        files: None,
        pre_at: int(r.get("time")),
        nuked: i64::from(truthy(r.pointer("/flags/nuke_rls"))),
        nuke_reason: None,
    }))
}

pub fn parse_xrel_p2p(v: &Value, page: u32) -> Result<Vec<Entry>, ParseError> {
    if past_the_last_page(v, page) {
        return Ok(Vec::new());
    }
    let list = rows(v, "/list")?;
    Ok(map(list, "dirname", |r, title| {
        let meta = text(r.pointer("/category/meta_cat")).map(|c| c.to_uppercase());
        let sub = text(r.pointer("/category/sub_cat"));
        let category = match (meta, sub) {
            (Some(m), Some(s)) => Some(format!("{m}-{s}")),
            (m, _) => m,
        };
        Entry {
            title,
            category,
            size_bytes: megabytes(r.get("size_mb")),
            files: None,
            pre_at: int(r.get("pub_time")),
            nuked: 0,
            nuke_reason: None,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn predb_club_rows() {
        let got = parse_predb_club(&fixture(include_str!("../../tests/fixtures/predb/predb_club.json")), 1).unwrap();
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(got[0].title, "Alpha.Beta.Gamma.2024.1080p.WEB-DL.H.264-TESTGRP");
        assert_eq!(got[0].category.as_deref(), Some("TV-HD"));
        assert_eq!(got[0].size_bytes, Some(13_107_200));
        assert_eq!(got[0].files, Some(35));
        assert_eq!(got[0].pre_at, Some(1_700_000_000));
        assert_eq!(got[0].nuked, 0);
        assert_eq!((got[1].nuked, got[1].nuke_reason.as_deref()), (3, Some("bad.audio")));
        assert_eq!(got[1].category, None, "an empty cat");
        assert_eq!(got[1].size_bytes, None);
        assert_eq!(got[2].size_bytes, Some(104_857_600), "as a string");
    }

    #[test]
    fn predb_net_rows() {
        let got = parse_predb_net(&fixture(include_str!("../../tests/fixtures/predb/predb_net.json")), 1).unwrap();
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(got[0].title, "Delta.Epsilon.Zeta.2024.720p.WEB-OTHER");
        assert_eq!((got[0].nuked, got[0].nuke_reason.as_deref()), (0, None));
        assert_eq!(got[0].size_bytes, Some(104_857_600));
        assert_eq!(got[0].category.as_deref(), Some("X264"));
        assert_eq!(got[0].pre_at, Some(1_700_000_001));
        assert_eq!((got[1].nuked, got[1].nuke_reason.as_deref()), (1, Some("dupe")));
        assert_eq!(got[2].nuked, 2);
    }

    #[test]
    fn predb_org_rows() {
        let got = parse_predb_org(&fixture(include_str!("../../tests/fixtures/predb/predb_org.json")), 1).unwrap();
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(got[0].title, "Eta.Theta.Iota.2019.DVDRip.x264-OLDGRP");
        assert_eq!(got[0].category.as_deref(), Some("DVDRIP"));
        assert_eq!(got[0].pre_at, Some(1_560_000_000));
        assert_eq!(got[0].nuked, 0);
        assert_eq!((got[1].nuked, got[1].nuke_reason.as_deref()), (1, None), "status 1 with no reason");
        assert_eq!((got[2].nuked, got[2].nuke_reason.as_deref()), (1, Some("mislabeled")), "a reason alone");
    }

    #[test]
    fn xrel_rows() {
        let got = parse_xrel(&fixture(include_str!("../../tests/fixtures/predb/xrel.json")), 1).unwrap();
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(got[0].title, "Kappa.Lambda.Mu.2024.German.1080p.WEB.x264-XGRP");
        assert_eq!(got[0].category.as_deref(), Some("MOVIE"));
        assert_eq!(got[0].size_bytes, Some(734_003_200));
        assert_eq!(got[0].pre_at, Some(1_700_000_002));
        assert_eq!(got[0].nuked, 0);
        assert_eq!(got[1].size_bytes, None, "a unit that isnt a size");
        assert_eq!(got[1].nuked, 1);
        assert_eq!(got[2].size_bytes, Some(2_147_483_648));
    }

    #[test]
    fn xrel_p2p_rows() {
        let got = parse_xrel_p2p(&fixture(include_str!("../../tests/fixtures/predb/xrel_p2p.json")), 1).unwrap();
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].title, "Nu.Xi.Omicron.2023.1080p.BluRay.x264-P2PGRP");
        assert_eq!(got[0].category.as_deref(), Some("MOVIES-hd"));
        assert_eq!(got[0].size_bytes, Some(2_097_152));
        assert_eq!(got[0].pre_at, Some(1_700_000_003));
        assert_eq!(got[1].category.as_deref(), Some("GAMES"));
    }

    #[test]
    fn error_envelopes() {
        let limited = fixture(include_str!("../../tests/fixtures/predb/rate_limited.json"));
        assert!(matches!(parse_predb_club(&limited, 1), Err(ParseError::RateLimited(_))));
        assert!(matches!(parse_predb_net(&limited, 1), Err(ParseError::RateLimited(_))));
        let no_list = fixture(include_str!("../../tests/fixtures/predb/no_list.json"));
        for spec in SOURCES {
            assert!(matches!((spec.parse)(&no_list, 1), Err(ParseError::Shape(_))), "{}", spec.key);
        }
    }

    #[test]
    fn too_many_rows_is_a_bad_shape() {
        let list: Vec<Value> = (0..=MAX_PAGE_ROWS).map(|_| serde_json::json!({"name": "x"})).collect();
        let v = serde_json::json!({"data": {"rows": list}});
        assert!(matches!(parse_predb_club(&v, 1), Err(ParseError::Shape(_))));
    }

    #[test]
    fn xrel_stops_at_total_pages() {
        for parse in [parse_xrel, parse_xrel_p2p] {
            let empty = serde_json::json!({"pagination": {"total_pages": 3}});
            assert_eq!(parse(&empty, 4).unwrap(), Vec::new());
            let with_rows =
                serde_json::json!({"pagination": {"total_pages": 3}, "list": [{"dirname": "A.Title.Here-GRP"}]});
            assert_eq!(parse(&with_rows, 4).unwrap(), Vec::new());
            assert_eq!(parse(&with_rows, 3).unwrap().len(), 1);
            assert!(matches!(parse(&empty, 2), Err(ParseError::Shape(_))), "only a page past the end is an end");
        }
    }

    #[test]
    fn page_urls_table() {
        assert_eq!(page_url(spec("predb_club").unwrap(), 3, 500), "https://predb.club/api/v1/?count=100&page=3");
        assert_eq!(page_url(spec("predb_org").unwrap(), 1, 100), "https://predb.org/api/search?q=*&limit=100&page=1");
        assert!(page_url(spec("xrel").unwrap(), 1, 2).contains("per_page=5"));
        assert!(spec("nope").is_none());
    }
}
