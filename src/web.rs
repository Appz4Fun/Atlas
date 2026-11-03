//! A small browser ui on the api port: search, browse and download NZBs.
//!
//! The api key never reaches a page. Signing in with it sets a cookie derived from it,
//! and the server does the searching and the NZB downloads itself.

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::LazyLock;

use sha1::{Digest, Sha1};

use crate::api::{ApiResponse, escape, find_releases, get_nzb, parse_query};
use crate::archive::Password;
use crate::category::{TABLE, UNCATEGORIZED, label, parse_filter};
use crate::field_search::{self, Fields};
use crate::gemini;
use crate::media::query::MediaAttrs;
use crate::media::title::Kind;
use crate::search::{ReleaseRow, get_release};
use crate::ui::{fmt_date, fmt_size};
use crate::{release_info, release_tags};

const HTMX: &str = include_str!("../assets/htmx-2.0.11.min.js");
const CSS: &str = include_str!("../assets/web.css");
const JS: &str = include_str!("../assets/web.js");
const MARK: &str = include_str!("../assets/atlas.svg");

/// On the asset links, soo a browser holding an older build's files fetches the new ones.
static ASSET_VERSION: LazyLock<String> = LazyLock::new(|| {
    let hash = [CSS, JS, HTMX, MARK]
        .iter()
        .flat_map(|asset| asset.bytes())
        .fold(0xcbf29ce484222325u64, |hash, b| (hash ^ u64::from(b)).wrapping_mul(0x100000001b3));
    format!("{hash:016x}")
});

const PAGE_SIZE: i64 = 50;
const COOKIE: &str = "atlas_session";
const COOKIE_DAYS: i64 = 30;

/// Everything comes from this origin, soo a release name that got past escaping still can't run.
const CSP: &str = "default-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

/// no eval, no requests elsewhere, and no copy of the results in localStorage
const HTMX_CONFIG: &str =
    r#"{"allowEval":false,"selfRequestsOnly":true,"includeIndicatorStyles":false,"historyCacheSize":0}"#;

/// A GET the ui answers. None: not ours, the api decides.
pub fn get(path: &str, query: &str, cookies: Option<&str>, base: &str, api_key: &str) -> Option<ApiResponse> {
    let signed_in = signed_in(cookies, api_key);
    let args = parse_query(query);
    Some(match path {
        "/static/htmx.min.js" => asset("text/javascript; charset=utf-8", HTMX),
        "/static/atlas.css" => asset("text/css; charset=utf-8", CSS),
        "/static/atlas.js" => asset("text/javascript; charset=utf-8", JS),
        "/static/atlas.svg" => asset("image/svg+xml", MARK),
        "/" | "" if signed_in => html(200, search_page(&args, base)),
        "/" | "" => html(200, login_page(base, false)),
        "/ui/results" if signed_in => results(&args),
        "/ui/results" => ApiResponse { headers: vec![("HX-Redirect".into(), "/".into())], ..html(401, String::new()) },
        "/ui/release" if signed_in => release_page(&args),
        "/ui/duplicates" if signed_in => duplicates_page(&args),
        "/ui/details" if signed_in => details_panel(&args),
        "/ui/poster" if signed_in => poster(&args),
        "/ui/meta" if signed_in => crate::web_meta::fragment(&args),
        "/ui/row" if signed_in => crate::web_meta::row_panel(&args),
        "/ui/settings" if signed_in => html(200, settings_page(None)),
        "/ui/nzb" if signed_in && args.get("fill").is_some_and(|v| v == "1") => filled_nzb(&args),
        "/ui/nzb" if signed_in => get_nzb(&args),
        "/ui/details" | "/ui/poster" | "/ui/meta" | "/ui/row" => {
            ApiResponse { headers: vec![("HX-Redirect".into(), "/".into())], ..html(401, String::new()) }
        }
        "/ui/nzb" | "/ui/release" | "/ui/duplicates" | "/ui/settings" => redirect(Vec::new()),
        _ => return None,
    })
}

/// A POST the ui answers. None: not ours. `htmx`: htmx sent it, and swaps in what comes back.
pub fn post(
    path: &str,
    body: &str,
    cookies: Option<&str>,
    htmx: bool,
    base: &str,
    api_key: &str,
) -> Option<ApiResponse> {
    let signed_in = signed_in(cookies, api_key);
    match path {
        "/login" => {
            let args = parse_query(body);
            let key = args.get("key").map_or("", |k| k.trim());
            Some(if same(key, api_key) {
                let max_age = COOKIE_DAYS * 86_400;
                redirect(vec![cookie_header(&session_token(api_key), max_age)])
            } else {
                html(401, login_page(base, true))
            })
        }
        "/logout" => Some(redirect(vec![cookie_header("", 0)])),
        "/ui/ask" if signed_in => Some(ask(body, htmx)),
        "/ui/settings" if signed_in => Some(save_settings(body)),
        "/ui/ask" if htmx => {
            Some(ApiResponse { headers: vec![("HX-Redirect".into(), "/".into())], ..html(401, String::new()) })
        }
        "/ui/ask" | "/ui/settings" => Some(redirect(Vec::new())),
        _ => None,
    }
}

/// The ui reads a request body only on these paths.
pub fn takes_post(path: &str) -> bool {
    matches!(path, "/login" | "/logout" | "/ui/ask" | "/ui/settings")
}

fn cookie_header(value: &str, max_age: i64) -> (String, String) {
    ("Set-Cookie".into(), format!("{COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}"))
}

fn redirect(mut headers: Vec<(String, String)>) -> ApiResponse {
    headers.push(("Location".into(), "/".into()));
    ApiResponse { status: 303, content_type: None, body: String::new(), headers, bytes: None, failed: false }
}

fn asset(content_type: &'static str, body: &str) -> ApiResponse {
    ApiResponse {
        status: 200,
        content_type: Some(content_type),
        body: body.into(),
        headers: vec![
            // small files on a LAN: a new build's styles show on the next load
            ("Cache-Control".into(), "no-cache".into()),
            ("X-Content-Type-Options".into(), "nosniff".into()),
        ],
        bytes: None,
        failed: false,
    }
}

pub(crate) fn html(status: u16, body: String) -> ApiResponse {
    ApiResponse {
        status,
        content_type: Some("text/html; charset=utf-8"),
        body,
        headers: vec![
            ("Content-Security-Policy".into(), CSP.into()),
            ("Cache-Control".into(), "no-store".into()),
            ("X-Content-Type-Options".into(), "nosniff".into()),
        ],
        bytes: None,
        failed: false,
    }
}

/// What the cookie holds: stands in for the key, and changes with it.
fn session_token(api_key: &str) -> String {
    let digest = Sha1::digest(format!("atlas web session\0{api_key}").as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn signed_in(cookies: Option<&str>, api_key: &str) -> bool {
    let want = session_token(api_key);
    cookies
        .into_iter()
        .flat_map(|c| c.split(';'))
        .filter_map(|c| c.trim().split_once('='))
        .any(|(name, value)| name == COOKIE && same(value, &want))
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// `escape`, and safe inside a quoted attribute too
pub(crate) fn attr(s: &str) -> String {
    escape(s).replace('"', "&quot;").replace('\'', "&#39;")
}

pub(crate) fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// The search as a query string, with the empty parts left out.
fn search_query(q: &str, cat: &str, offset: i64) -> String {
    let mut parts = Vec::new();
    if !q.is_empty() {
        parts.push(format!("q={}", url_encode(q)));
    }
    if !cat.is_empty() {
        parts.push(format!("cat={}", url_encode(cat)));
    }
    if offset > 0 {
        parts.push(format!("offset={offset}"));
    }
    parts.join("&")
}

struct Search {
    q: String,
    cat: String,
    offset: i64,
    /// from `FILTERS`, nothing else
    filters: Vec<&'static str>,
    fields: Fields,
    sort: Option<SortBy>,
    descending: bool,
}

/// Up to this many results get sorted: the newest or best matching ones.
const SORT_WINDOW: i64 = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SortBy {
    Name,
    Category,
    Size,
    Posted,
    Group,
    Rating,
}

impl SortBy {
    const ALL: [(SortBy, &'static str); 6] = [
        (SortBy::Name, "name"),
        (SortBy::Category, "category"),
        (SortBy::Size, "size"),
        (SortBy::Posted, "posted"),
        (SortBy::Group, "group"),
        (SortBy::Rating, "rating"),
    ];

    fn word(self) -> &'static str {
        SortBy::ALL.iter().find(|(s, _)| *s == self).map_or("", |(_, w)| w)
    }

    fn from_word(word: &str) -> Option<SortBy> {
        SortBy::ALL.iter().find(|(_, w)| *w == word).map(|(s, _)| *s)
    }
}

/// Puts results in order, with those that have no value for the column last either way.
fn sort_rows(
    rows: &mut [ReleaseRow],
    by: SortBy,
    descending: bool,
    categories: &HashMap<i64, i64>,
    scores: &HashMap<i64, i64>,
) {
    let flip = |o: std::cmp::Ordering| if descending { o.reverse() } else { o };
    rows.sort_by(|a, b| match by {
        SortBy::Name => flip(a.name.to_lowercase().cmp(&b.name.to_lowercase())),
        SortBy::Category => {
            let of =
                |r: &ReleaseRow| label(categories.get(&r.id).copied().unwrap_or(UNCATEGORIZED)).unwrap_or_default();
            flip(of(a).cmp(&of(b)))
        }
        SortBy::Size => match (a.size, b.size) {
            (Some(x), Some(y)) => flip(x.cmp(&y)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        },
        SortBy::Posted => {
            let of = |r: &ReleaseRow| fmt_date(r.posted_date.as_deref());
            match (of(a), of(b)) {
                (x, y) if x.is_empty() || y.is_empty() => y.len().cmp(&x.len()),
                (x, y) => flip(x.cmp(&y)),
            }
        }
        SortBy::Group => flip(a.group_name.cmp(&b.group_name)),
        SortBy::Rating => match (scores.get(&a.id), scores.get(&b.id)) {
            (Some(x), Some(y)) => flip(x.cmp(y)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        },
    });
}

/// How far back "Hide copies" looks for what a page's copies are copies of.
const MAX_EARLIER_FOR_COPIES: i64 = 2_000;

/// The quick filters over results. Each but the last keeps the results whose name's tags say so, among the first
/// `SORT_WINDOW`: searching the index for each way a tag is written was three or four full searches a click.
const FILTERS: &[&str] = &["2160p", "1080p", "DV", "HDR", "Remux", "WEB-DL", "BluRay", HIDE_COPIES];
const HIDE_COPIES: &str = "Hide copies";

/// How many names have their tags read at once, before checking whether the page is full.
const TAG_SLICE: usize = 128;

/// Whether a name's tags say what a filter asks. Tags, not words, soo "DV" leaves DVDRip out.
fn filter_matches(filter: &str, tags: &release_tags::Tags) -> bool {
    let quality = tags.quality.as_deref().unwrap_or_default();
    match filter {
        "2160p" | "1080p" => tags.resolution.as_deref() == Some(filter),
        "DV" => tags.hdr.iter().any(|hdr| hdr == "DV"),
        "HDR" => tags.hdr.iter().any(|hdr| hdr == "HDR" || hdr == "HDR10+"),
        "Remux" => quality.contains("REMUX"),
        "WEB-DL" => quality == "WEB-DL",
        "BluRay" => quality.starts_with("BluRay"),
        _ => true,
    }
}

impl Search {
    fn from_args(args: &HashMap<String, String>) -> Search {
        let text = |k: &str| args.get(k).map_or(String::new(), |v| v.trim().to_string());
        // the name search pages by offset / limit, soo a page starts on a multiple
        let offset = args.get("offset").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0).max(0);
        let asked = text("filters");
        let filters = FILTERS.iter().copied().filter(|f| asked.split(',').any(|a| a.trim() == *f)).collect();
        let fields = Fields {
            group: text("group"),
            poster: text("poster"),
            email: text("email"),
            size: text("size").replace([',', ' '], ""),
            team: text("team"),
            crc: text("crc"),
            agent: text("agent"),
        };
        let sort = SortBy::from_word(&text("sort"));
        let descending = text("dir") != "asc";
        Search {
            q: text("q"),
            cat: text("cat"),
            offset: offset - offset % PAGE_SIZE,
            filters,
            fields,
            sort,
            descending,
        }
    }

    /// `search_query`, with this search's filters, or `filters` in their place.
    fn query_with(&self, cat: &str, offset: i64, filters: &[&str]) -> String {
        let mut query = search_query(&self.q, cat, offset);
        if !filters.is_empty() {
            if !query.is_empty() {
                query.push('&');
            }
            query.push_str(&format!("filters={}", url_encode(&filters.join(","))));
        }
        let mut more = self.fields.query();
        if let Some(sort) = self.sort {
            more.push(format!("sort={}&dir={}", sort.word(), if self.descending { "desc" } else { "asc" }));
        }
        for part in more {
            if !query.is_empty() {
                query.push('&');
            }
            query.push_str(&part);
        }
        query
    }

    /// A column heading: sorts by it, and flips when it's already the sort.
    fn sort_link(&self, by: SortBy, heading: &str) -> String {
        let (current, descending) = (self.sort == Some(by), self.descending);
        let flipped = Search {
            q: self.q.clone(),
            cat: self.cat.clone(),
            offset: 0,
            filters: self.filters.clone(),
            fields: self.fields.clone(),
            sort: Some(by),
            descending: if current {
                !descending
            } else {
                matches!(by, SortBy::Size | SortBy::Posted | SortBy::Rating)
            },
        };
        let query = attr(&flipped.query(&self.cat, 0));
        let arrow = match (current, descending) {
            (false, _) => "",
            (true, true) => " ▼",
            (true, false) => " ▲",
        };
        let state = match (current, descending) {
            (false, _) => "none",
            (true, true) => "descending",
            (true, false) => "ascending",
        };
        format!(
            "<a class=\"sort-link\" href=\"/?{query}\" hx-get=\"/ui/results?{query}\" hx-target=\"#results\" hx-sync=\"#search:replace\" hx-indicator=\"#busy\" aria-sort=\"{state}\">{heading}{arrow}</a>"
        )
    }

    fn query(&self, cat: &str, offset: i64) -> String {
        self.query_with(cat, offset, &self.filters)
    }

    /// Whether a filter other than "Hide copies" is on: those go by the tags of the results.
    fn filters_by_tags(&self) -> bool {
        self.filters.iter().any(|f| *f != HIDE_COPIES)
    }

    /// Whether the page is cut from the first `SORT_WINDOW` results, sorted or filtered, instead of read by offset.
    fn windowed(&self) -> bool {
        self.sort.is_some() || self.filters_by_tags()
    }

    /// Whether a name's tags say what every filter asks.
    fn has_tags(&self, row: &ReleaseRow) -> bool {
        if !self.filters_by_tags() {
            return true;
        }
        let tags = release_tags::parse(&row.name);
        self.filters.iter().all(|filter| filter_matches(filter, &tags))
    }

    /// The rows `has_tags` keeps, in order, until there are `enough`. Reading a name's tags takes about a millisecond,
    /// soo they are read a slice at a time, on every core.
    fn keep_tagged(&self, rows: Vec<ReleaseRow>, enough: usize) -> Vec<ReleaseRow> {
        if !self.filters_by_tags() {
            return rows;
        }
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get()).min(16);
        let mut kept = Vec::new();
        for slice in rows.chunks(TAG_SLICE) {
            let per = slice.len().div_ceil(cores).max(1);
            let keep: Vec<bool> = std::thread::scope(|scope| {
                let parts: Vec<_> = slice
                    .chunks(per)
                    .map(|part| scope.spawn(move || part.iter().map(|r| self.has_tags(r)).collect::<Vec<_>>()))
                    .collect();
                parts.into_iter().flat_map(|part| part.join().unwrap_or_default()).collect()
            });
            kept.extend(slice.iter().zip(keep).filter(|(_, keep)| *keep).map(|(r, _)| r.clone()));
            if kept.len() >= enough {
                break;
            }
        }
        kept
    }

    /// The search itself: one, whatever the filters.
    fn search(
        &self,
        filter: Option<&crate::category::CatFilter>,
        from: i64,
        count: i64,
    ) -> Result<Vec<ReleaseRow>, String> {
        if self.fields.any() {
            field_search::find(&self.fields, &self.q, filter, from, count).map_err(|e| e.to_string())
        } else {
            find_releases(&self.q, filter, from, count).map_err(|e| e.to_string())
        }
    }

    fn filter_links(&self) -> String {
        let mut out = String::from("<div class=\"chips\" role=\"group\" aria-label=\"Filters\">");
        for filter in FILTERS {
            let on = self.filters.contains(filter);
            let toggled: Vec<&str> = if on {
                self.filters.iter().copied().filter(|f| f != filter).collect()
            } else {
                let mut more = self.filters.clone();
                more.push(filter);
                more
            };
            let query = attr(&self.query_with(&self.cat, 0, &toggled));
            let pressed = if on { " aria-current=\"true\"" } else { "" };
            let _ = write!(
                out,
                "<a class=\"chip\" href=\"/?{query}\" hx-get=\"/ui/results?{query}\" hx-target=\"#results\" hx-sync=\"#search:replace\" hx-indicator=\"#busy\"{pressed}>{}</a>",
                escape(filter)
            );
        }
        out.push_str(&self.sort_link(SortBy::Rating, "Best rated"));
        out.push_str("</div>");
        out
    }

    /// The table rows, or a full-width diagnostic when the first page has none.
    fn body(&self) -> String {
        let filter = parse_filter(&self.cat);
        let windowed = self.windowed();
        let (from, count) = if windowed { (0, SORT_WINDOW) } else { (self.offset, PAGE_SIZE) };
        let searched = self.search(filter.as_ref(), from, count);
        let found = match searched {
            Ok(found) => found,
            Err(e) => {
                eprintln!("web search failed: {e}");
                let message = "Atlas couldn't read its database. The error is in its log. Search again in a moment.";
                return if self.offset == 0 {
                    diag("caution", "the search failed", message)
                } else {
                    format!("<tr><td colspan=\"{COLUMNS}\" class=\"note\">{}</td></tr>", escape(message))
                };
            }
        };
        let read = found.len() as i64;
        let tagged = |r: &ReleaseRow| self.has_tags(r);
        // a sort needs every match in the window, a page only up to one past its end
        let enough = if self.sort.is_some() { usize::MAX } else { (self.offset + PAGE_SIZE) as usize + 1 };
        let mut found = self.keep_tagged(found, enough);
        if found.is_empty() {
            return if self.offset == 0 {
                self.nothing_found()
            } else {
                format!("<tr><td colspan=\"{COLUMNS}\" class=\"note dim\">That's all of them.</td></tr>")
            };
        }
        let hide_copies = self.filters.contains(&HIDE_COPIES);
        let mut before = Vec::new();
        let page_was_full = if windowed {
            if let Some(sort) = self.sort {
                let ids: Vec<i64> = found.iter().map(|r| r.id).collect();
                let categories = crate::category::categories_of(&ids).unwrap_or_default();
                // only the scores already kept: fetching them for a whole search would use up the providers' limits
                let scores: HashMap<i64, i64> = if sort == SortBy::Rating {
                    let media = crate::media::query::attrs_of(&ids).unwrap_or_default();
                    crate::media::details::kept_for(media.iter().map(|(id, m)| (*id, m.kind, &m.ids)))
                        .into_iter()
                        .filter_map(|(id, d)| Some((id, score_value(&d)?)))
                        .collect()
                } else {
                    HashMap::new()
                };
                // by the size it shows: all of a post spread over several groups
                if sort == SortBy::Size
                    && let Ok(conn) = crate::db::open()
                {
                    for r in found.iter_mut().filter(|r| !r.complete) {
                        if let Ok(Some(post)) = release_info::split_post_with(&conn, r) {
                            r.size = Some(post.size);
                        }
                    }
                }
                sort_rows(&mut found, sort, self.descending, &categories, &scores);
            }
            let start = usize::try_from(self.offset).unwrap_or(0).min(found.len());
            let end = (start + PAGE_SIZE as usize).min(found.len());
            let more = end < found.len();
            if hide_copies {
                let earlier = start.saturating_sub(MAX_EARLIER_FOR_COPIES as usize);
                before = found[earlier..start].to_vec();
            }
            found = found.drain(start..end).collect();
            more
        } else {
            read == PAGE_SIZE
        };
        // a post spread over several groups is one row, unless the search is about one of the groups. Before copies are
        // hidden: its rows can look like copies of each other
        let mut split = HashMap::new();
        let conn = crate::db::open().ok();
        let one_group = !self.fields.group.is_empty();
        let post_of = |r: &ReleaseRow| {
            if r.complete {
                return None;
            }
            conn.as_ref().and_then(|conn| release_info::split_post_with(conn, r).ok().flatten())
        };
        let folded = |r: &ReleaseRow, post: &release_info::SplitPost| !one_group && post.listed_id() != r.id;
        found.retain_mut(|r| {
            let Some(post) = post_of(r) else { return true };
            if folded(r, &post) {
                return false;
            }
            r.size = Some(post.size);
            r.complete = post.complete;
            split.insert(r.id, post.groups.len());
            true
        });
        if hide_copies {
            // a copy of something on an earlier page is a copy too
            if !windowed && self.offset > 0 {
                let earlier = self.offset.min(MAX_EARLIER_FOR_COPIES);
                let start = self.offset - earlier;
                before = self.search(filter.as_ref(), start, earlier).unwrap_or_default();
            }
            before.retain(|r| !post_of(r).is_some_and(|post| folded(r, &post)));
            let ids: Vec<i64> = before.iter().filter(|r| tagged(r)).chain(found.iter()).map(|r| r.id).collect();
            let hidden = crate::duplicates::later_copies(&ids).unwrap_or_default();
            found.retain(|r| !hidden.contains(&r.id));
        }
        let ids: Vec<i64> = found.iter().map(|r| r.id).collect();
        let categories = crate::category::categories_of(&ids).unwrap_or_default();
        let passwords = crate::archive::store::passwords(&ids);
        let media = crate::media::query::attrs_of(&ids).unwrap_or_default();
        let extras = RowExtras {
            split,
            copies: crate::duplicates::counts(&ids).unwrap_or_default(),
            details: crate::media::details::kept_for(media.iter().map(|(id, m)| (*id, m.kind, &m.ids))),
        };

        let mut out: String = found.iter().map(|r| row(r, &categories, &passwords, &media, &extras)).collect();
        if page_was_full {
            let next = self.query(&self.cat, self.offset + PAGE_SIZE);
            let next = attr(&next);
            let _ = write!(
                out,
                "<tr><td colspan=\"{COLUMNS}\" class=\"note\"><a class=\"button\" href=\"/?{next}\" hx-get=\"/ui/results?{next}\" hx-target=\"closest tr\" hx-swap=\"outerHTML\">Load more</a></td></tr>"
            );
        }
        out
    }

    fn nothing_found(&self) -> String {
        let scope = category_name(&self.cat);
        let title = match (self.q.is_empty(), &scope) {
            _ if !self.filters.is_empty() && self.q.is_empty() => "nothing matches these filters".to_string(),
            _ if self.fields.any() && self.q.is_empty() => "nothing matches these fields".to_string(),
            (true, Some(scope)) => format!("nothing is in {scope} yet"),
            (true, None) => "no releases yet".to_string(),
            (false, Some(scope)) => format!("nothing matches \u{201c}{}\u{201d} in {scope}", self.q),
            (false, None) => format!("nothing matches \u{201c}{}\u{201d}", self.q),
        };
        let mut hint = String::new();
        if scope.is_some() {
            let everywhere = attr(&self.query("", 0));
            let _ = write!(
                hint,
                "<a href=\"/?{everywhere}\" hx-get=\"/ui/results?{everywhere}\" hx-target=\"#results\">Look in all categories</a>. "
            );
        }
        if self.filters_by_tags() {
            let _ = write!(hint, "The filters look through the first {SORT_WINDOW} results. ");
        }
        if !self.q.is_empty() {
            hint.push_str("A search matches every word you type, from the start of a word in the release name.");
        } else {
            hint.push_str("Releases get a category once Atlas has processed them.");
        }
        format!(
            "<div class=\"diag\"><p class=\"diag-head\"><span class=\"diag-level\">note:</span> {}</p><div class=\"diag-body\"><p>{hint}</p></div></div>",
            escape(&title)
        )
    }

    /// Rust-style path of where this is, as on the RustyGet site.
    fn summary(&self) -> String {
        let mut path = String::from("atlas::releases");
        if let Some(name) = category_name(&self.cat) {
            for part in name.split(" / ") {
                let _ = write!(path, "::<b>{}</b>", escape(&part.to_lowercase().replace(' ', "_")));
            }
        }
        let sort = match self.sort {
            Some(by) => format!(
                "By {} {}, among the first {SORT_WINDOW}",
                by.word(),
                if self.descending { "descending" } else { "ascending" }
            ),
            None if self.filters_by_tags() => format!(
                "{} first, filtered among the first {SORT_WINDOW}",
                if self.q.is_empty() { "Newest" } else { "Best or newest matches" }
            ),
            None if self.q.is_empty() => "Newest first".to_string(),
            None => "Best or newest matches first".to_string(),
        };
        let mut scope = String::new();
        for (name, value) in self.fields.chosen() {
            let without = Search {
                q: self.q.clone(),
                cat: self.cat.clone(),
                offset: 0,
                filters: self.filters.clone(),
                fields: self.fields.without(name),
                sort: self.sort,
                descending: self.descending,
            };
            let query = attr(&without.query(&self.cat, 0));
            let _ = write!(
                scope,
                "<a class=\"chip\" aria-current=\"true\" href=\"/?{query}\" hx-get=\"/ui/results?{query}\" hx-target=\"#results\" title=\"Remove\">{}: {} ✕</a>",
                escape(name),
                escape(&value)
            );
        }
        // the count goes by words and fields only, soo with tags filtering the page it would count rows it hides
        let total = if self.fields.any() && !self.filters_by_tags() {
            let filter = parse_filter(&self.cat);
            match field_search::count_any(&self.fields, std::slice::from_ref(&self.q), filter.as_ref()) {
                Ok(total) => format!("<strong class=\"total\">{}</strong>", escape(&total.describe())),
                Err(e) => {
                    eprintln!("web count failed: {e}");
                    String::new()
                }
            }
        } else {
            String::new()
        };
        format!(
            "<div class=\"summary\"><span class=\"path\">{path}</span>{total}<span class=\"sort\">{}</span></div>{}",
            escape(&sort),
            if scope.is_empty() { String::new() } else { format!("<div class=\"chips\">{scope}</div>") },
        )
    }
}

/// What a result shows beyond its own row.
#[derive(Default)]
struct RowExtras {
    /// groups, for a post spread over several
    split: HashMap<i64, usize>,
    copies: HashMap<i64, crate::duplicates::CopyCount>,
    details: HashMap<i64, crate::media::details::Details>,
}

/// Columns of the results table.
pub(crate) const COLUMNS: usize = 6;

pub(crate) fn tag(class: &str, text: &str) -> String {
    format!(" <span class=\"tag {class}\">{}</span>", escape(text))
}

fn password_tag(password: Option<&Password>) -> String {
    match password {
        Some(Password::Yes) => tag("tag-bad", "PASSWORD"),
        Some(Password::Maybe) => tag("tag-warn", "PASSWORD?"),
        _ => String::new(),
    }
}

/// The score to sort by, out of 100, the same one a result shows.
fn score_value(d: &crate::media::details::Details) -> Option<i64> {
    d.imdb_score.or(d.tmdb_score).or(d.trakt_score)
}

/// The one score a result has room for.
fn best_score(d: &crate::media::details::Details) -> Option<String> {
    d.imdb_score
        .map(|s| format!("IMDB {s}"))
        .or_else(|| d.tmdb_score.map(|s| format!("TMDB {s}")))
        .or_else(|| d.trakt_score.map(|s| format!("TRAKT {s}")))
}

/// The tags worth a place in a list of results.
fn name_tags(parsed: &release_tags::Tags) -> String {
    let mut out = String::new();
    if let Some(resolution) = &parsed.resolution {
        out.push_str(&tag("tag-ref", resolution));
    }
    for hdr in &parsed.hdr {
        out.push_str(&tag("tag-ok", hdr));
    }
    if let Some(quality) = &parsed.quality {
        out.push_str(&tag("", quality));
    }
    out
}

/// S01E02 for an episode, S01 for a season, else the year.
fn media_label(media: &MediaAttrs) -> Option<String> {
    match (media.season, media.episode, media.year) {
        (Some(s), Some(e), _) => Some(format!("S{s:02}E{e:02}")),
        (Some(s), None, _) => Some(format!("S{s:02}")),
        (None, _, Some(year)) => Some(year.to_string()),
        _ => None,
    }
}

fn row(
    r: &ReleaseRow,
    categories: &HashMap<i64, i64>,
    passwords: &HashMap<i64, Password>,
    media: &HashMap<i64, MediaAttrs>,
    extras: &RowExtras,
) -> String {
    let category = escape(&label(categories.get(&r.id).copied().unwrap_or(UNCATEGORIZED)).unwrap_or_default());
    let size = fmt_size(r.size);
    let posted = escape(&fmt_date(r.posted_date.as_deref()));
    let mut tags = String::new();
    if let Some(text) = media.get(&r.id).and_then(media_label) {
        tags.push_str(&tag("tag-ref", &text));
    }
    tags.push_str(&name_tags(&release_tags::parse(&r.name)));
    let details = extras.details.get(&r.id);
    if let Some(score) = details.and_then(best_score) {
        tags.push_str(&tag("tag-ref", &score));
    }
    let thumb = details.filter(|d| crate::media::details::poster_kept(d)).map_or(String::new(), |d| {
        format!(
            "<img class=\"thumb\" src=\"/ui/poster?kind={}&amp;tmdb={}\" alt=\"\" loading=\"lazy\" width=\"32\" height=\"48\">",
            d.kind, d.tmdb_id
        )
    });
    if let Some(count) = extras.copies.get(&r.id) {
        if count.twins > 0 {
            tags.push_str(&tag("tag-ok", &format!("{} TWIN{}", count.twins, if count.twins == 1 { "" } else { "S" })));
        }
        if count.alts > 0 {
            tags.push_str(&tag("tag-ref", &format!("{} ALT{}", count.alts, if count.alts == 1 { "" } else { "S" })));
        }
    }
    if let Some(groups) = extras.split.get(&r.id) {
        tags.push_str(&tag("tag-ref", &format!("{groups} GROUPS")));
    }
    tags.push_str(&password_tag(passwords.get(&r.id)));
    if !r.complete {
        tags.push_str(&tag("tag-warn", "INCOMPLETE"));
    }
    tags.push_str(&crate::web_meta::drawer_trigger(r.id));
    let meta = [category.as_str(), size.as_str(), posted.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    format!(
        "<tr><td class=\"name\">{thumb}<a href=\"/ui/release?id={id}\">{name}</a>{tags}<div class=\"row-meta\">{meta}</div></td><td class=\"col-cat dim\">{category}</td><td class=\"num col-size\" title=\"{bytes} bytes\">{size}</td><td class=\"dim col-posted\">{posted}</td><td class=\"dim col-group\"><a class=\"quiet\" href=\"/?group={group_link}\">{group}</a></td><td><a class=\"button small primary\" href=\"/ui/nzb?id={id}\">NZB</a></td></tr>",
        id = r.id,
        name = escape(&r.name),
        group = escape(&r.group_name),
        group_link = attr(&url_encode(&r.group_name)),
        bytes = thousands(r.size.unwrap_or(0)),
    )
}

/// A link to search on a value, showing other words than the value.
pub(crate) fn search_link_to(param: &str, value: &str, shown: &str) -> String {
    format!("<a href=\"/?{param}={}\">{}</a>", attr(&url_encode(value)), escape(shown))
}

/// 2684354560 as 2,684,354,560
pub(crate) fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

fn table(search: &Search, rows: &str) -> String {
    format!(
        "<div class=\"table-wrap\"><table><thead><tr><th>{}</th><th class=\"col-cat\">{}</th><th class=\"num col-size\">{}</th><th class=\"col-posted\">{}</th><th class=\"col-group\">{}</th><th><span class=\"sr-only\">Download</span></th></tr></thead><tbody>{rows}</tbody></table></div>",
        search.sort_link(SortBy::Name, "Name"),
        search.sort_link(SortBy::Category, "Category"),
        search.sort_link(SortBy::Size, "Size"),
        search.sort_link(SortBy::Posted, "Posted"),
        search.sort_link(SortBy::Group, "Group"),
    )
}

/// A compiler-style callout, the way Oxide writes them.
fn diag(level: &str, title: &str, body: &str) -> String {
    format!(
        "<div class=\"diag diag-{level}\" role=\"alert\"><p class=\"diag-head\"><span class=\"diag-level\">{level}:</span> {}</p><div class=\"diag-body\"><p>{}</p></div></div>",
        escape(title),
        escape(body),
    )
}

/// What goes in #results for a first page.
fn results_html(search: &Search) -> String {
    let body = search.body();
    let content = if body.starts_with("<tr") { table(search, &body) } else { body };
    format!("{}{}{content}", search.filter_links(), search.summary())
}

/// What htmx swaps in: the whole result area for a new search, more rows for "Load more".
fn results(args: &HashMap<String, String>) -> ApiResponse {
    let search = Search::from_args(args);
    if search.offset > 0 {
        return html(200, search.body());
    }
    first_page(&search, &ask_note("", " hx-swap-oob=\"true\""))
}

/// A new search's results, with the sidebar and the form following it. `more` goes in too, for out of band swaps.
fn first_page(search: &Search, more: &str) -> ApiResponse {
    let oob = " hx-swap-oob=\"true\"";
    let body = format!(
        "{}{}{}{}{more}",
        results_html(search),
        category_nav(search, oob),
        category_input(&search.cat, oob),
        scope_inputs(search, oob),
    );
    let mut resp = html(200, body);
    // the address bar follows the search, and reloads the full page
    let query = search.query(&search.cat, 0);
    let url = if query.is_empty() { "/".to_string() } else { format!("/?{query}") };
    resp.headers.push(("HX-Push-Url".into(), url));
    resp
}

/// "Movies / HD" for a category the table knows, whatever the url asked for.
fn category_name(cat: &str) -> Option<String> {
    let id: i64 = cat.trim().parse().ok()?;
    TABLE.iter().find_map(|root| {
        if root.id == id {
            return Some(root.name.to_string());
        }
        root.subs.iter().find(|s| s.id == id).map(|s| format!("{} / {}", root.name, s.name))
    })
}

fn category_input(cat: &str, oob: &str) -> String {
    format!("<input id=\"cat\" type=\"hidden\" name=\"cat\" value=\"{}\"{oob}>", attr(cat))
}

/// More things to search on, with the sort: part of the form, soo typing keeps them.
fn scope_inputs(search: &Search, oob: &str) -> String {
    let open = if search.fields.any() { " open" } else { "" };
    let field = |name: &str, label: &str, value: &str| {
        format!(
            "<label>{label}<input class=\"scope-field\" name=\"{name}\" value=\"{}\" autocomplete=\"off\" spellcheck=\"false\"></label>",
            attr(value)
        )
    };
    let hidden = |name: &str, value: &str| format!("<input type=\"hidden\" name=\"{name}\" value=\"{}\">", attr(value));
    format!(
        "<details id=\"scope\" class=\"more-fields\"{open}{oob}><summary>More fields</summary><div class=\"field-grid\">{}{}{}{}{}{}{}</div>{}{}{}</details>",
        field("group", "Usenet group", &search.fields.group),
        field("poster", "Poster", &search.fields.poster),
        field("email", "Email", &search.fields.email),
        field("size", "Size, in bytes or as 1.5 GB", &search.fields.size),
        field("team", "Release group", &search.fields.team),
        field("crc", "CRC32", &search.fields.crc),
        field("agent", "Posted with", &search.fields.agent),
        hidden("filters", &search.filters.join(",")),
        hidden("sort", search.sort.map_or("", SortBy::word)),
        hidden(
            "dir",
            if search.sort.is_none() {
                ""
            } else if search.descending {
                "desc"
            } else {
                "asc"
            }
        ),
    )
}

/// The sidebar. Its links keep whatever is in the search box, through `hx-include`.
fn category_nav(search: &Search, oob: &str) -> String {
    let link = |id: &str, name: &str, class: &str| {
        let current = if id == search.cat { " aria-current=\"page\"" } else { "" };
        let page = attr(&search.query(id, 0));
        let swap = if id.is_empty() { String::new() } else { format!("?cat={id}") };
        format!("<a{class} href=\"/?{page}\" hx-get=\"/ui/results{swap}\"{current}>{}</a>", escape(name))
    };
    let mut out = format!(
        "<nav id=\"categories\" aria-label=\"Categories\" hx-target=\"#results\" hx-include=\"#q, #scope input\" hx-sync=\"#search:replace\" hx-indicator=\"#busy\"{oob}><div class=\"nav-group\">{}</div>",
        link("", "All categories", ""),
    );
    for root in TABLE {
        out.push_str("<div class=\"nav-group\">");
        out.push_str(&link(&root.id.to_string(), root.name, " class=\"nav-root\""));
        for sub in root.subs {
            out.push_str(&link(&sub.id.to_string(), sub.name, ""));
        }
        out.push_str("</div>");
    }
    out.push_str("</nav>");
    out
}

fn page(title: &str, content: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><meta name=\"color-scheme\" content=\"light dark\"><meta name=\"htmx-config\" content=\"{}\"><title>{}</title><link rel=\"icon\" href=\"/static/atlas.svg?v={v}\" type=\"image/svg+xml\"><link rel=\"stylesheet\" href=\"/static/atlas.css?v={v}\"><script src=\"/static/atlas.js?v={v}\"></script><script src=\"/static/htmx.min.js?v={v}\"></script></head><body>{content}</body></html>",
        attr(HTMX_CONFIG),
        escape(title),
        v = *ASSET_VERSION,
    )
}

fn masthead(signed_in: bool) -> String {
    let tools = if signed_in {
        "<a class=\"button\" href=\"/ui/duplicates\">Duplicates</a><a class=\"button\" href=\"/ui/settings\">Settings</a><form method=\"post\" action=\"/logout\"><button class=\"button\">Sign out</button></form>"
    } else {
        ""
    };
    format!(
        r#"<header class="masthead"><a class="brand" href="/" aria-label="Atlas home">{}<span class="brand-name">At<span>las</span></span></a><span class="brand-tag">newznab indexer</span><div class="masthead-tools"><button class="button icon-button" type="button" data-theme-toggle aria-label="Switch between light and dark theme"><svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="6.5" fill="none" stroke="currentColor" stroke-width="1.5"/><path d="M8 1.5a6.5 6.5 0 0 1 0 13Z" fill="currentColor"/></svg></button>{tools}</div></header>"#,
        MARK,
    )
}

fn search_page(args: &HashMap<String, String>, base: &str) -> String {
    let search = Search::from_args(args);
    page(
        "Atlas",
        &format!(
            r##"{masthead}
<div class="layout"><aside class="sidebar"><details class="navigation" open><summary>Categories</summary>{nav}</details></aside>
<main>
<form id="search" class="searchbar" role="search" action="/" method="get" hx-get="/ui/results" hx-target="#results" hx-sync="this:replace" hx-indicator="#busy" hx-trigger="submit, input changed delay:300ms from:#q, change target:.scope-field">
<div class="search-field">{q}<kbd>/</kbd></div>
{cat}<button class="button primary">Search</button><span id="busy" class="busy">searching…</span>
{scope}</form>
{ask}
<div id="failure"></div>
<div id="results">{results}</div>
<footer class="footer">Newznab API: <code>{base}/api</code></footer>
</main></div>"##,
            masthead = masthead(true),
            nav = category_nav(&search, ""),
            q = q_input(&search.q),
            ask = ask_box(&gemini::load()),
            cat = category_input(&search.cat, ""),
            scope = scope_inputs(&search, ""),
            results = results_html(&search),
            base = escape(base),
        ),
    )
}

/// One release: what's known about it, and its files where Atlas has looked inside.
fn release_page(args: &HashMap<String, String>) -> ApiResponse {
    let id = args.get("id").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0);
    let release = match get_release(id) {
        Ok(Some(release)) => release,
        Ok(None) => return release_problem(404, "note", "there is no such release", "It may have been purged."),
        Err(e) => {
            eprintln!("web release {id} failed: {e}");
            return release_problem(500, "caution", "the release could not be read", "The error is in Atlas's log.");
        }
    };
    let ids = [id];
    let category = crate::category::categories_of(&ids).unwrap_or_default().get(&id).copied();
    let inner = crate::archive::store::view(id);
    let media = crate::media::query::attrs_of(&ids).unwrap_or_default().remove(&id);
    let names = release_info::names(id).unwrap_or_default();
    let mut contents = release_info::contents(id).unwrap_or_default();
    let mut release = release;
    // a post spread over several groups is shown whole
    if !release.complete
        && let Ok(conn) = crate::db::open()
        && let Ok(Some(post)) = release_info::split_post_with(&conn, &release)
    {
        contents = release_info::contents_of_split(&conn, &post).unwrap_or(contents);
        release.size = Some(post.size);
        release.complete = post.complete;
        release.parts = Some(contents.articles_seen);
    }
    html(200, release_html(&release, category, &inner, media.as_ref(), names.as_ref(), &contents))
}

fn release_problem(status: u16, level: &str, title: &str, body: &str) -> ApiResponse {
    let content = format!(
        "{}<div class=\"signin\">{}<p><a href=\"/\">Back to the releases</a></p></div>",
        masthead(true),
        diag(level, title, body)
    );
    html(status, page("Atlas", &content))
}

fn release_html(
    r: &ReleaseRow,
    category: Option<i64>,
    inner: &crate::archive::store::InnerView,
    media: Option<&MediaAttrs>,
    names: Option<&release_info::Names>,
    contents: &release_info::Contents,
) -> String {
    let category = category.unwrap_or(UNCATEGORIZED);
    let mut tags = String::new();
    if let Some(text) = media.and_then(media_label) {
        tags.push_str(&tag("tag-ref", &text));
    }
    tags.push_str(&password_tag(inner.password.as_ref()));
    tags.push_str(&if r.complete { tag("tag-ok", "COMPLETE") } else { tag("tag-warn", "INCOMPLETE") });
    if names.is_some_and(|n| n.obfuscated) {
        tags.push_str(&tag("tag-warn", "OBFUSCATED"));
    }

    let mut facts = String::new();
    let mut fact = |name: &str, value: String| {
        if !value.is_empty() {
            let _ = write!(facts, "<dt>{name}</dt><dd>{value}</dd>");
        }
    };
    let parsed = release_tags::parse(&r.name);
    tags.push_str(&name_tags(&parsed));
    fact("Category", format!("<a href=\"/?cat={category}\">{}</a>", escape(&label(category).unwrap_or_default())));
    let search_link =
        |param: &str, value: &str| format!("<a href=\"/?{param}={}\">{}</a>", attr(&url_encode(value)), escape(value));
    if !parsed.title.is_empty() {
        fact("Title", search_link("q", &parsed.title));
    }
    if let Some(size) = r.size {
        fact(
            "Size",
            format!("{} · <a href=\"/?size={size}\">{} bytes</a>", escape(&fmt_size(r.size)), thousands(size)),
        );
    }
    fact("Posted", escape(&fmt_date(r.posted_date.as_deref())));
    let posted_name = names.map_or(r.name.as_str(), |n| n.posted.as_str());
    let groups = release_info::groups_of(posted_name).unwrap_or_default();
    if groups.len() > 1 {
        let list: Vec<String> = groups
            .iter()
            .map(|g| format!("{} ({} articles)", search_link("group", &g.group), thousands(g.articles)))
            .collect();
        fact("Groups", list.join("<br>"));
    } else {
        fact("Group", search_link("group", &r.group_name));
    }
    if let Some(raw) = &r.poster {
        let poster = release_info::parse_poster(raw);
        fact("Poster", poster.name.as_deref().map_or(String::new(), |name| search_link_to("poster", raw, name)));
        if let Some(email) = poster.email {
            let made_up = if release_info::looks_made_up(&email) { tag("tag-warn", "MADE UP") } else { String::new() };
            fact("Email", format!("{}{made_up}", search_link("email", &email)));
        }
    }
    if let Some(n) = names {
        if n.posted != r.name {
            fact("Posted as", escape(&n.posted));
        }
        if let Some(from) = n.source.and_then(release_info::name_source_label) {
            fact("Name from", escape(from));
        }
    }
    if let Some(year) = parsed.year {
        fact("Year", year.to_string());
    }
    if let Some(network) = &parsed.network {
        fact("Ripped from", escape(network));
    }
    if let Some(site) = &parsed.site {
        fact("Site", escape(site));
    }
    fact(
        "Video",
        escape(
            &[parsed.codec.as_deref(), parsed.bit_depth.as_deref(), parsed.container.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", "),
        ),
    );
    fact("Audio", escape(&parsed.audio.iter().chain(&parsed.channels).cloned().collect::<Vec<_>>().join(", ")));
    fact("Languages", escape(&parsed.languages.join(", ")));
    if let Some(edition) = &parsed.edition {
        fact("Edition", escape(edition));
    }
    if let Some(group) = &parsed.group {
        fact("Release group", search_link("team", group));
    }
    if let Some(parts) = r.parts {
        fact("Parts", parts.to_string());
    }
    fact("Encryption", encryption_fact(inner));
    if let Some(hint) = release_info::password_hint(r.id, names) {
        fact("Password", password_fact(inner, &hint));
    }
    if contents.files > 0 {
        fact("Files", contents.files.to_string());
    }
    if contents.par2_files > 0 {
        fact("PAR2 files", format!("{} ({} recovery)", contents.par2_files, contents.recovery_volumes));
    }
    if let Some(missing) = contents.missing_per_mille() {
        fact("Articles", health(contents, missing));
    }
    fact("Repair", repair_fact(contents));
    let links = media.map(|m| id_links(m.kind, &m.ids)).unwrap_or_default();
    if !links.is_empty() {
        fact("Links", links);
    }

    let files = if inner.files.is_empty() {
        String::new()
    } else {
        let rows: String = inner
            .files
            .iter()
            .map(|f| {
                format!(
                    "<tr><td class=\"name\">{}</td><td class=\"num\">{}</td></tr>",
                    escape(&f.name),
                    fmt_size(f.size)
                )
            })
            .collect();
        format!(
            "<h2>Files inside</h2><div class=\"table-wrap\"><table><thead><tr><th>Name</th><th class=\"num\">Size</th></tr></thead><tbody>{rows}</tbody></table></div>"
        )
    };

    // the probe row is decoded once: the panels below read the same one
    let conn = crate::db::open().ok();
    let found = conn.as_ref().and_then(|c| crate::probe::read(c, r.id));
    let probe = match (conn.as_ref(), found.as_ref()) {
        (Some(c), Some(found)) => {
            probe_html(found, crate::probe::opensubtitles::read(c, r.id).as_ref(), crate::web_meta::salt_of(Some(c)))
        }
        _ => String::new(),
    };
    let meta = crate::web_meta::meta_html(
        conn.as_ref(),
        &crate::web_meta::MetaInputs { release: r, inner, names, contents, probe: found.as_ref() },
    );
    let copies = crate::duplicates::copies(r.id).unwrap_or_default();
    let has_twin = crate::duplicates::can_fill(&copies);
    let fill = if has_twin && contents.articles_seen < contents.articles_expected {
        format!(
            "<a class=\"button\" href=\"/ui/nzb?id={}&amp;fill=1\">NZB, gaps filled from a twin (check it with PAR2)</a>",
            r.id
        )
    } else {
        String::new()
    };
    let copies = copies_html(&copies);

    page(
        &format!("{} · Atlas", r.name),
        &format!(
            r#"{masthead}<div class="detail"><main>
<p class="path"><a href="/">atlas::releases</a>::<b>{id}</b></p>
<h1>{name}</h1><p>{tags}</p>
<div class="actions"><a class="button primary" href="/ui/nzb?id={id}">Download NZB</a>{fill}<a class="button" href="/?cat={category}">More in this category</a></div>
<div id="title-details" hx-get="/ui/details?id={id}" hx-trigger="load" hx-swap="outerHTML"></div>
<div class="panel"><dl class="facts">{facts}</dl></div>
{copies}
{meta}
{probe}
{files}
</main></div>"#,
            masthead = masthead(true),
            id = r.id,
            name = escape(&r.name),
        ),
    )
}

/// A release's NZB with what it's missing taken from its twins, or as it is when they don't help.
fn filled_nzb(args: &HashMap<String, String>) -> ApiResponse {
    let id = args.get("id").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0);
    match crate::duplicates::filled_nzb(id) {
        Ok(Some(content)) => {
            let name = get_release(id).ok().flatten().map(|r| r.name).unwrap_or_default();
            let filename = crate::nzb::nzb_filename(&name, Some(id)).replace(['"', '\\'], "_");
            ApiResponse {
                status: 200,
                content_type: Some("application/x-nzb"),
                body: content,
                headers: vec![("Content-Disposition".into(), format!("attachment; filename=\"{filename}\""))],
                bytes: None,
                failed: false,
            }
        }
        Ok(None) => ApiResponse::not_found(),
        Err(e) => {
            eprintln!("web filled nzb {id} failed: {e}");
            html(500, String::new())
        }
    }
}

pub(crate) fn verdict(ok: Option<bool>, good: &str, bad: &str) -> String {
    match ok {
        Some(true) => tag("tag-ok", good),
        Some(false) => tag("tag-bad", bad),
        None => String::new(),
    }
}

pub(crate) fn panel(title: &str, facts: &str) -> String {
    if facts.is_empty() {
        return String::new();
    }
    format!("<h2>{title}</h2><div class=\"panel\"><dl class=\"facts\">{facts}</dl></div>")
}

/// What reading a release's articles found, without downloading it.
fn probe_html(
    p: &crate::probe::ProbeInfo,
    matched: Option<&crate::probe::opensubtitles::Stored>,
    salt: Option<i64>,
) -> String {
    let dd = |out: &mut String, name: &str, value: String| {
        if !value.is_empty() {
            let _ = write!(out, "<dt>{name}</dt><dd>{value}</dd>");
        }
    };
    let number = |n: Option<u64>| n.map_or(String::new(), |n| thousands(n as i64));
    let mut yenc = String::new();
    if let Some(y) = &p.yenc {
        dd(&mut yenc, "File name", y.name.as_deref().map_or(String::new(), escape));
        if let Some(size) = y.file_size {
            dd(
                &mut yenc,
                "File size",
                format!("{} · {} bytes", escape(&fmt_size(Some(size as i64))), thousands(size as i64)),
            );
        }
        dd(&mut yenc, "Parts", y.total.map_or(String::new(), |n| thousands(i64::from(n))));
        dd(&mut yenc, "Line length", y.line_len.map_or(String::new(), |n| n.to_string()));
        dd(&mut yenc, "Article size", number(p.segment_size));
        if let Some(crc) = p.file_crc32 {
            dd(&mut yenc, "CRC32", search_link_to("crc", &format!("{crc:08x}"), &format!("{crc:08x}")));
        }
        if let Some(overhead) = p.overhead_percent {
            dd(&mut yenc, "Overhead", format!("{overhead:.2}%"));
        }
    }
    let checks = format!(
        "{}{}{}",
        verdict(p.continuity_ok, "ALL BYTES COVERED", "BYTES MISSING"),
        verdict(p.part_size_ok, "PART SIZE OK", "PART SIZE WRONG"),
        verdict(p.part_crc_ok, "PART CRC OK", "PART CRC WRONG"),
    );
    dd(&mut yenc, "Checks", checks);
    let health: String = ["first", "middle", "last"]
        .into_iter()
        .filter_map(|part| p.health.get(part).map(|there| (part, there)))
        .map(|(part, there)| {
            if *there {
                tag("tag-ok", &format!("{} THERE", part.to_uppercase()))
            } else {
                tag("tag-bad", &format!("{} GONE", part.to_uppercase()))
            }
        })
        .collect();
    dd(&mut yenc, "On the server", health);

    let mut posted = String::new();
    if let Some(agent) = crate::probe::agent_of(&p.headers) {
        dd(&mut posted, "Posted with", search_link_to("agent", agent, agent));
    }
    for (name, value) in &p.headers {
        if !matches!(name.as_str(), "User-Agent" | "X-Newsreader" | "X-Newsposter" | "Xref") {
            // the posting account, host and IP are shown as salted keys, as the Headers fold-out shows them
            dd(&mut posted, &escape(name), crate::web_meta::redact_header(name, value, salt));
        }
    }
    if !p.crossposted_to.is_empty() {
        let list: Vec<String> = p.crossposted_to.iter().map(|g| search_link_to("group", g, g)).collect();
        dd(&mut posted, "Crossposted to", list.join("<br>"));
    }

    let mut packed = String::new();
    if let Some(a) = &p.archive {
        dd(&mut packed, "Format", escape(&a.format));
        dd(&mut packed, "Method", a.method.as_deref().map_or(String::new(), escape));
        let lock = match (a.headers_encrypted, a.encrypted) {
            (true, _) => tag("tag-bad", "NAMES AND FILES ENCRYPTED"),
            (false, true) => tag("tag-bad", "FILES ENCRYPTED"),
            // a 7z's first bytes, or a RAR cut off before its first file, say nothing either way
            _ if a.name.is_some() => tag("tag-ok", "NOT ENCRYPTED"),
            _ => String::new(),
        };
        dd(&mut packed, "Encryption", lock);
        dd(&mut packed, "First file", a.name.as_deref().map_or(String::new(), escape));
        if let Some(size) = a.unpacked {
            dd(
                &mut packed,
                "Unpacked",
                format!("{} · {} bytes", escape(&fmt_size(Some(size as i64))), thousands(size as i64)),
            );
        }
        dd(&mut packed, "File CRC32", a.crc32.map_or(String::new(), |c| format!("{c:08x}")));
    }
    if p.layers.len() > 1 || (p.archive.is_none() && !p.layers.is_empty()) {
        dd(&mut packed, "Layers", nested_chain(&p.layers));
    }

    let mut tracks = String::new();
    if let Some(m) = &p.media {
        dd(&mut tracks, "Container", escape(&m.container));
        dd(&mut tracks, "Written by", m.writing_app.as_deref().map_or(String::new(), escape));
        dd(&mut tracks, "Muxed by", m.muxing_app.as_deref().map_or(String::new(), escape));
        dd(&mut tracks, "Title", m.title.as_deref().map_or(String::new(), escape));
        dd(
            &mut tracks,
            "Duration",
            m.duration_secs.map_or(String::new(), |s| {
                let s = s as u64;
                format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
            }),
        );
        for v in &m.video {
            let mut parts = vec![v.codec.clone()];
            if let (Some(w), Some(h)) = (v.width, v.height) {
                parts.push(format!("{w}×{h}"));
            }
            if let Some(rate) = v.frame_rate {
                parts.push(format!("{rate:.3} fps"));
            }
            if let Some(depth) = v.bit_depth {
                parts.push(format!("{depth}-bit"));
            }
            parts.extend(v.hdr.clone());
            if let Some(profile) = v.dv_profile {
                let layer = match v.dv_enhancement_layer {
                    Some(true) => ", enhancement layer",
                    _ => "",
                };
                parts.push(format!("Dolby Vision profile {profile}{layer}"));
            }
            dd(&mut tracks, "Video", escape(&parts.join(", ")));
        }
        for a in &m.audio {
            let mut parts = vec![a.codec.clone()];
            parts.extend(a.channels.map(|c| format!("{c} channels")));
            parts.extend(a.sample_rate.map(|r| format!("{r:.0} Hz")));
            parts.extend(a.language.clone());
            if a.atmos {
                parts.push("Atmos".into());
            }
            // a name that says so already
            if a.commentary && !a.name.as_deref().is_some_and(|n| n.to_ascii_lowercase().contains("commentary")) {
                parts.push("commentary".into());
            }
            parts.extend(a.name.clone());
            dd(&mut tracks, "Audio", escape(&parts.join(", ")));
        }
        for s in &m.subtitles {
            let mut parts = vec![s.codec.clone()];
            parts.extend(s.language.clone());
            if s.forced {
                parts.push("forced".into());
            }
            if s.hearing_impaired {
                parts.push("SDH".into());
            }
            // a name that says so already
            if s.commentary && !s.name.as_deref().is_some_and(|n| n.to_ascii_lowercase().contains("commentary")) {
                parts.push("commentary".into());
            }
            parts.extend(s.name.clone());
            dd(&mut tracks, "Subtitles", escape(&parts.join(", ")));
        }
    }

    if let Some(hash) = &p.moviehash {
        dd(&mut tracks, "OpenSubtitles hash", escape(hash));
    }
    if let Some(f) = matched.and_then(|m| m.feature.as_ref()) {
        let mut name = f.title.clone().unwrap_or_default();
        if let (Some(season), Some(episode)) = (f.season, f.episode) {
            let _ = write!(name, " S{season:02}E{episode:02}");
        }
        if let Some(year) = f.year {
            let _ = write!(name, " ({year})");
        }
        let link = f.imdb_id.map_or(escape(&name), |id| {
            format!("<a href=\"https://www.imdb.com/title/tt{id:07}/\" rel=\"noreferrer\">{}</a>", escape(&name))
        });
        let release = f.release_name.as_deref().map_or(String::new(), |r| format!(" · {}", escape(r)));
        dd(&mut tracks, "OpenSubtitles match", format!("{link}{release}"));
    }
    let mut chapters = String::new();
    if let Some(m) = &p.media {
        for (n, c) in m.chapters.iter().enumerate() {
            let s = c.start_secs.max(0.0) as u64;
            let at = format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60);
            let name = c.name.as_deref().map_or_else(|| format!("Chapter {}", n + 1), str::to_string);
            dd(&mut chapters, &at, escape(&name));
        }
    }

    let imdb: Vec<String> = p
        .imdb_ids
        .iter()
        .map(|id| format!("<a href=\"https://www.imdb.com/title/{id}/\" rel=\"noreferrer\">{id}</a>", id = attr(id)))
        .collect();
    let nfo = p.nfo.as_deref().map_or(String::new(), |text| {
        format!("<h2>NFO</h2><p>{}</p><pre class=\"nfo\">{}</pre>", imdb.join(" · "), escape(text))
    });
    let sfv = if p.sfv.is_empty() {
        String::new()
    } else {
        let rows: String = p
            .sfv
            .iter()
            .map(|(name, crc)| {
                format!("<tr><td class=\"name\">{}</td><td class=\"num\">{crc:08x}</td></tr>", escape(name))
            })
            .collect();
        format!("<h2>SFV</h2><div class=\"table-wrap\"><table><tbody>{rows}</tbody></table></div>")
    };
    format!(
        "{}{}{}{}{}{nfo}{sfv}",
        panel("Tracks", &tracks),
        panel("Chapters", &chapters),
        panel("yEnc", &yenc),
        panel("Packing", &packed),
        panel("How it was posted", &posted),
    )
}

/// Copies read in full on one page: each reads its articles.
const FULL_COPIES: usize = 12;
const CLUSTERS_PER_PAGE: i64 = 10;
/// Of one cluster, the most read in full. The rest are links.
const FULL_MEMBERS: usize = 6;

/// One copy, with everything that helps to choose between copies. Scores and posters are what's already kept, soo a
/// list never calls out: they come once someone has opened the title.
fn copy_card(conn: &rusqlite::Connection, badge: &str, r: &ReleaseRow) -> String {
    let parsed = release_tags::parse(&r.name);
    let media = crate::media::query::attrs_with(conn, &[r.id]).unwrap_or_default().remove(&r.id);
    let details = media
        .as_ref()
        .and_then(|m| crate::media::details::kept_for(std::iter::once((r.id, m.kind, &m.ids))).remove(&r.id));
    let names = release_info::names_with(conn, r.id).unwrap_or_default();
    let contents = release_info::contents_with(conn, r.id).unwrap_or_default();
    let inner = crate::archive::store::view_with(conn, r.id);
    let hint = release_info::password_hint_with(conn, r.id, names.as_ref());

    let mut tags = badge.to_string();
    tags.push_str(&name_tags(&parsed));
    if names.as_ref().is_some_and(|n| n.obfuscated) {
        tags.push_str(&tag("tag-warn", "OBFUSCATED"));
    }
    tags.push_str(&password_tag(inner.password.as_ref()));
    if !r.complete {
        tags.push_str(&tag("tag-warn", "INCOMPLETE"));
    }
    if let Some(d) = &details {
        tags.push_str(&scores_html(d));
    }

    let mut facts = String::new();
    let mut fact = |name: &str, value: String| {
        if !value.is_empty() {
            let _ = write!(facts, "<dt>{name}</dt><dd>{value}</dd>");
        }
    };
    if let Some(size) = r.size {
        fact("Size", format!("{} · {} bytes", escape(&fmt_size(r.size)), thousands(size)));
    }
    fact("Group", search_link_to("group", &r.group_name, &r.group_name));
    if let Some(raw) = &r.poster {
        let poster = release_info::parse_poster(raw);
        fact("Poster", poster.name.as_deref().map_or(String::new(), |name| search_link_to("poster", raw, name)));
        if let Some(email) = poster.email {
            let made_up = if release_info::looks_made_up(&email) { tag("tag-warn", "MADE UP") } else { String::new() };
            fact("Email", format!("{}{made_up}", search_link_to("email", &email, &email)));
        }
    }
    if contents.par2_files > 0 {
        fact("PAR2 files", format!("{} ({} recovery)", contents.par2_files, contents.recovery_volumes));
    }
    fact("Repair", repair_fact(&contents));
    if let Some(missing) = contents.missing_per_mille() {
        fact("Articles", health(&contents, missing));
    }
    fact("Encryption", encryption_fact(&inner));
    if let Some(hint) = &hint {
        fact("Password", password_fact(&inner, hint));
    }
    fact(
        "Video",
        escape(
            &[parsed.codec.as_deref(), parsed.bit_depth.as_deref(), parsed.container.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", "),
        ),
    );
    fact("Audio", escape(&parsed.audio.iter().chain(&parsed.channels).cloned().collect::<Vec<_>>().join(", ")));
    fact("Languages", escape(&parsed.languages.join(", ")));
    if let Some(network) = &parsed.network {
        fact("Ripped from", escape(network));
    }
    if let Some(group) = &parsed.group {
        fact("Release group", search_link_to("team", group, group));
    }
    if let Some(m) = &media {
        fact("Links", id_links(m.kind, &m.ids));
    }
    let thumb = details.filter(crate::media::details::poster_kept).map_or(String::new(), |d| {
        format!(
            "<img class=\"copy-poster\" src=\"/ui/poster?kind={}&amp;tmdb={}\" alt=\"\" loading=\"lazy\" width=\"72\" height=\"108\">",
            d.kind, d.tmdb_id
        )
    });
    format!(
        "<article class=\"panel copy\">{thumb}<div class=\"copy-body\"><div class=\"copy-head\"><div class=\"name\"><a href=\"/ui/release?id={id}\">{name}</a>{tags}{more}</div><a class=\"button small primary\" href=\"/ui/nzb?id={id}\">NZB</a></div><dl class=\"facts facts-wide\">{facts}</dl></div></article>",
        id = r.id,
        name = escape(&r.name),
        more = crate::web_meta::card_trigger(r.id, "closest .copy-body"),
    )
}

/// A copy past the first few: its name, badge and size, with the `+` that opens its drawer in place.
fn copy_link(badge: &str, r: &ReleaseRow) -> String {
    format!(
        "<li><a href=\"/ui/release?id={}\">{}</a>{badge} {}{}</li>",
        r.id,
        escape(&r.name),
        escape(&fmt_size(r.size)),
        crate::web_meta::card_trigger(r.id, "closest li")
    )
}

/// Other postings of this release's content.
fn copies_html(copies: &[(crate::duplicates::Copy, ReleaseRow)]) -> String {
    use crate::duplicates::Copy;
    if copies.is_empty() {
        return String::new();
    }
    let badge = |kind: &Copy| match kind {
        Copy::Twin => tag("tag-ok", "TWIN"),
        Copy::Alt => tag("tag-ref", "ALT"),
    };
    let Ok(conn) = crate::db::open() else { return String::new() };
    let cards: String =
        copies.iter().take(FULL_COPIES).map(|(kind, copy)| copy_card(&conn, &badge(kind), copy)).collect();
    let rest: String = copies.iter().skip(FULL_COPIES).map(|(kind, copy)| copy_link(&badge(kind), copy)).collect();
    let rest = if rest.is_empty() { rest } else { format!("<ul class=\"copy-rest\">{rest}</ul>") };
    format!(
        "<h2>Other copies</h2><div class=\"copies\">{cards}</div>{rest}<p class=\"sort\">A twin is the same posting: the same files, PAR2 set, recovery volumes and article counts. An alt holds the same content packed differently.</p>"
    )
}

/// Every cluster of copies, whichever release they're found from.
fn duplicates_page(args: &HashMap<String, String>) -> ApiResponse {
    use crate::duplicates::{ClusterKind, cluster_count_with, clusters_with};
    let kind = if args.get("kind").is_some_and(|k| k == "alts") { ClusterKind::Alts } else { ClusterKind::Twins };
    let number = args.get("page").and_then(|p| p.parse::<i64>().ok()).unwrap_or(1).max(1);
    let read = crate::db::open().and_then(|conn| {
        let twins = cluster_count_with(&conn, ClusterKind::Twins)?;
        let alts = cluster_count_with(&conn, ClusterKind::Alts)?;
        let page = clusters_with(&conn, kind, (number - 1) * CLUSTERS_PER_PAGE, CLUSTERS_PER_PAGE)?;
        Ok((conn, twins, alts, page))
    });
    let (conn, twins, alts, page) = match read {
        Ok(found) => found,
        Err(e) => {
            eprintln!("web duplicates failed: {e}");
            return release_problem(500, "caution", "the copies could not be read", "The error is in Atlas's log.");
        }
    };
    let chip = |word: &str, label: &str, count: i64, on: bool| {
        let current = if on { " aria-current=\"true\"" } else { "" };
        format!("<a class=\"chip\" href=\"/ui/duplicates?kind={word}\"{current}>{label} {}</a>", thousands(count))
    };
    let (total, word, badge, about) = match kind {
        ClusterKind::Twins => (
            twins,
            "twins",
            tag("tag-ok", "TWIN"),
            "Twins are the same posting: the same files, PAR2 set, recovery volumes and article counts, so one can fill the other's gaps.",
        ),
        ClusterKind::Alts => (
            alts,
            "alts",
            tag("tag-ref", "ALT"),
            "Alts hold files of the same exact sizes, packed differently, so each is a separate download.",
        ),
    };
    let clusters: String = page
        .iter()
        .map(|members| {
            let title = release_tags::parse(&members[0].name).title;
            let title = if title.is_empty() { members[0].name.clone() } else { title };
            let cards: String = members.iter().take(FULL_MEMBERS).map(|r| copy_card(&conn, &badge, r)).collect();
            let rest: String = members.iter().skip(FULL_MEMBERS).map(|r| copy_link(&badge, r)).collect();
            let rest = if rest.is_empty() { rest } else { format!("<ul class=\"copy-rest\">{rest}</ul>") };
            format!(
                "<section class=\"cluster\"><h2>{} <span class=\"count\">{} copies</span></h2><div class=\"copies\">{cards}</div>{rest}</section>",
                escape(&title),
                members.len()
            )
        })
        .collect();
    let body = if clusters.is_empty() && number == 1 {
        diag(
            "note",
            &format!("no {word} found yet"),
            "Copies are found once archives and PAR2 sets have been read, and that needs a body budget.",
        )
    } else {
        clusters
    };
    let pages = (total + CLUSTERS_PER_PAGE - 1) / CLUSTERS_PER_PAGE;
    let pager = if pages > 1 {
        let link = |to: i64, text: &str| {
            format!("<a class=\"button\" href=\"/ui/duplicates?kind={word}&amp;page={to}\">{text}</a>")
        };
        format!(
            "<nav class=\"actions\" aria-label=\"Pages\">{}<span class=\"sort\">Page {number} of {pages}</span>{}</nav>",
            if number > 1 { link(number - 1, "Previous") } else { String::new() },
            if number < pages { link(number + 1, "Next") } else { String::new() },
        )
    } else {
        String::new()
    };
    let content = format!(
        "{masthead}<div class=\"release\"><main><h1>Duplicates</h1><div class=\"chips\">{twins}{alts}</div><p class=\"sort\">{about}</p>{body}{pager}</main></div>",
        masthead = masthead(true),
        twins = chip("twins", "Twins", twins, kind == ClusterKind::Twins),
        alts = chip("alts", "Alts", alts, kind == ClusterKind::Alts),
    );
    html(200, self::page("Duplicates · Atlas", &content))
}

/// Loaded after the page, soo a slow provider doesn't hold it up. Empty when the title isn't known.
fn details_panel(args: &HashMap<String, String>) -> ApiResponse {
    let id = args.get("id").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0);
    let media = crate::media::query::attrs_of(&[id]).unwrap_or_default().remove(&id);
    let details =
        media.as_ref().and_then(|m| crate::media::details::for_title(&crate::config::media_config(), m.kind, &m.ids));
    html(200, details.map(|d| details_html(&d)).unwrap_or_default())
}

fn scores_html(d: &crate::media::details::Details) -> String {
    let score = |name: &str, score: Option<i64>| score.map(|s| tag("tag-ref", &format!("{name} {s}")));
    [
        score("IMDB", d.imdb_score),
        score("RT", d.rotten),
        score("MC", d.metacritic),
        score("TMDB", d.tmdb_score),
        score("TRAKT", d.trakt_score),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn details_html(d: &crate::media::details::Details) -> String {
    let mut lines = String::new();
    let mut line = |name: &str, value: Option<String>| {
        if let Some(value) = value {
            let _ = write!(lines, "<dt>{name}</dt><dd>{}</dd>", escape(&value));
        }
    };
    line("Released", d.released.clone());
    line("Blu-ray", d.bluray.clone());
    line("Genres", d.genres.clone());
    line("Runtime", d.runtime.map(|minutes| format!("{minutes} min")));
    line("Network", d.network.clone());
    line("Status", d.status.clone());
    line("Cast", d.cast.clone());
    let scores = scores_html(d);
    let poster = if d.poster.is_some() {
        format!(
            "<img class=\"poster\" src=\"/ui/poster?kind={}&amp;tmdb={}\" alt=\"\" loading=\"lazy\" width=\"171\" height=\"256\">",
            d.kind, d.tmdb_id
        )
    } else {
        String::new()
    };
    let backdrop = if d.backdrop.is_some() {
        format!(
            "<img class=\"backdrop\" src=\"/ui/poster?kind={}&amp;tmdb={}&amp;art=backdrop\" alt=\"\" loading=\"lazy\">",
            d.kind, d.tmdb_id
        )
    } else {
        String::new()
    };
    format!(
        "{backdrop}<section class=\"panel title-details\">{poster}<div><h2>{}</h2><p>{scores}</p><p>{}</p><dl class=\"facts\">{lines}</dl></div></section>",
        escape(d.title.as_deref().unwrap_or_default()),
        escape(d.overview.as_deref().unwrap_or_default()),
    )
}

fn poster(args: &HashMap<String, String>) -> ApiResponse {
    let number = |key: &str| args.get(key).and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0);
    let cfg = crate::config::media_config();
    let found = if args.get("art").is_some_and(|art| art == "backdrop") {
        crate::media::details::backdrop(number("kind"), number("tmdb"), &cfg)
    } else {
        crate::media::details::poster(number("kind"), number("tmdb"), &cfg)
    };
    let Some(bytes) = found else {
        return ApiResponse::not_found();
    };
    ApiResponse {
        status: 200,
        content_type: Some("image/jpeg"),
        body: String::new(),
        headers: vec![
            ("Cache-Control".into(), "private, max-age=2592000".into()),
            ("X-Content-Type-Options".into(), "nosniff".into()),
        ],
        bytes: Some(bytes),
        failed: false,
    }
}

fn password_fact(inner: &crate::archive::store::InnerView, hint: &release_info::PasswordHint) -> String {
    let verdict = match inner.password_check.and_then(|check| check.accepts(&hint.password)) {
        Some(true) if inner.password_check.is_some_and(|check| !check.certain()) => tag("tag-ok", "PROBABLY WORKS"),
        Some(true) => tag("tag-ok", "WORKS"),
        Some(false) => tag("tag-bad", "WRONG"),
        None => ", not tried".to_string(),
    };
    format!("<code>{}</code> found in {}{verdict}", escape(&hint.password), escape(hint.source))
}

/// How much of a release its recovery volumes can repair: exact from the PAR2 set, or else an estimate.
fn repair_fact(contents: &release_info::Contents) -> String {
    if let Some(repair) = contents.exact_repair_per_mille() {
        per_mille(repair)
    } else if let Some(repair) = contents.repair_per_mille().filter(|_| contents.recovery_bytes > 0) {
        format!("about {} ({})", per_mille(repair), escape(&fmt_size(Some(contents.recovery_bytes))))
    } else {
        String::new()
    }
}

/// What's known about a password protected release's encryption.
fn encryption_fact(inner: &crate::archive::store::InnerView) -> String {
    let hidden = if inner.names_hidden { ", file names hidden" } else { "" };
    match (inner.encryption, inner.password) {
        (Some(kind), _) => format!("{}{hidden}", escape(kind.label())),
        (None, Some(Password::Yes)) => "Encrypted, type not recorded".into(),
        (None, Some(Password::Maybe)) => "Maybe encrypted".into(),
        _ => String::new(),
    }
}

/// 30 as "3%", 25 as "2.5%"
fn per_mille(n: i64) -> String {
    if n % 10 == 0 { format!("{}%", n / 10) } else { format!("{}.{}%", n / 10, n % 10) }
}

/// How much is there, and whether the recovery volumes would cover what isn't.
fn health(contents: &release_info::Contents, missing: i64) -> String {
    let counts = format!("{} of {}", contents.articles_seen, contents.articles_expected);
    if missing == 0 && contents.articles_seen == contents.articles_expected {
        return format!("{counts}{}", tag("tag-ok", "ALL THERE"));
    }
    let repairable = contents.repairable();
    let verdict = if repairable { tag("tag-warn", "REPAIRABLE") } else { tag("tag-bad", "NOT ENOUGH RECOVERY") };
    format!("{counts}, {} missing{verdict}", per_mille(missing.max(1)))
}

/// Where the ids a release matched lead, on the sites that give them out.
fn id_links(kind: i64, ids: &crate::media::resolve::Ids) -> String {
    let link = |name: &str, url: String| format!("<a href=\"{}\" rel=\"noreferrer\">{name}</a>", attr(&url));
    let mut out = Vec::new();
    if let Some(id) = ids.imdb {
        out.push(link("IMDb", format!("https://www.imdb.com/title/tt{id:07}/")));
    }
    if let Some(id) = ids.tmdb {
        let section = if kind == Kind::Tv as i64 { "tv" } else { "movie" };
        out.push(link("TMDB", format!("https://www.themoviedb.org/{section}/{id}")));
    }
    if let Some(id) = ids.tvdb {
        out.push(link("TheTVDB", format!("https://thetvdb.com/dereferrer/series/{id}")));
    }
    if let Some(id) = ids.tvmaze {
        out.push(link("TVmaze", format!("https://www.tvmaze.com/shows/{id}")));
    }
    if let Some(id) = ids.trakt {
        let id_type = if kind == Kind::Tv as i64 { "show" } else { "movie" };
        out.push(link("Trakt", format!("https://trakt.tv/search/trakt/{id}?id_type={id_type}")));
    }
    out.join(" · ")
}

/// Also what someone pointing a browser at the bare address gets, soo it says how to set up a client.
fn q_input(q: &str) -> String {
    format!(
        "<input id=\"q\" name=\"q\" type=\"search\" placeholder=\"Search releases\" value=\"{}\" autocomplete=\"off\" spellcheck=\"false\" autofocus aria-label=\"Search releases\">",
        attr(q)
    )
}

/// Where what Gemini made of a question goes: always on the page, soo a plain search can empty it.
fn ask_note(content: &str, oob: &str) -> String {
    format!("<div id=\"ask-note\" aria-live=\"polite\"{oob}>{content}</div>")
}

/// The plain words box, or where to turn it on.
fn ask_box(settings: &gemini::Settings) -> String {
    let note = ask_note("", "");
    if !settings.has_key() {
        return format!(
            "<p class=\"ask-off\">Search in plain words with Gemini: <a href=\"/ui/settings\">add a key in Settings</a>.</p>{note}"
        );
    }
    format!(
        r##"<form id="ask" class="searchbar askbar" action="/ui/ask" method="post" hx-post="/ui/ask" hx-target="#results" hx-sync="#search:replace" hx-indicator="#asking" hx-disabled-elt="find button">
<div class="search-field"><input id="ask-q" name="ask" type="search" maxlength="{max}" placeholder="Ask in plain words: 4K Dolby Vision remuxes of Dune, biggest first" autocomplete="off" aria-label="Ask in plain words"></div>
<button class="button">Ask AI</button><span id="asking" class="busy">asking Gemini…</span></form>
{note}"##,
        max = gemini::MAX_QUESTION,
    )
}

/// The search page's arguments for what Gemini read a question as, keeping only what the page knows.
fn plan_args(plan: &gemini::Plan) -> HashMap<String, String> {
    let mut args = HashMap::new();
    let mut put = |k: &str, v: &str| {
        if !v.is_empty() {
            args.insert(k.to_string(), v.to_string());
        }
    };
    put("q", &plan.q);
    if category_name(&plan.cat).is_some() {
        put("cat", &plan.cat);
    }
    put("filters", &plan.filters.join(","));
    put("group", &plan.group);
    put("poster", &plan.poster);
    put("team", &plan.team);
    if SortBy::from_word(&plan.sort).is_some() {
        put("sort", &plan.sort);
        put("dir", if plan.dir == "asc" { "asc" } else { "desc" });
    }
    args
}

/// "“dune” in Movies / UHD, 2160p and DV, biggest first": the search a question became, in words.
fn described(search: &Search) -> String {
    let mut what =
        if search.q.is_empty() { "the newest releases".to_string() } else { format!("\u{201c}{}\u{201d}", search.q) };
    if let Some(name) = category_name(&search.cat) {
        what.push_str(&format!(" in {name}"));
    }
    let mut parts = vec![what];
    if !search.filters.is_empty() {
        parts.push(search.filters.join(", "));
    }
    for (name, value) in search.fields.chosen() {
        parts.push(format!("{} {value}", name.to_lowercase()));
    }
    if let Some(by) = search.sort {
        let way = match (by, search.descending) {
            (SortBy::Posted, true) => "newest first".to_string(),
            (SortBy::Posted, false) => "oldest first".to_string(),
            (SortBy::Size, true) => "biggest first".to_string(),
            (SortBy::Size, false) => "smallest first".to_string(),
            (SortBy::Rating, true) => "best rated first".to_string(),
            (by, true) => format!("by {}, Z to A", by.word()),
            (by, false) => format!("by {}, A to Z", by.word()),
        };
        parts.push(way);
    }
    parts.join(", ")
}

/// Plain words to a search: Gemini reads them, the page searches with what it read.
fn ask(body: &str, htmx: bool) -> ApiResponse {
    let args = parse_query(body);
    let question: String = args.get("ask").map_or("", |q| q.trim()).chars().take(gemini::MAX_QUESTION).collect();
    let failed = |title: &str, why: &str| {
        let mut resp = html(200, diag("caution", title, why));
        resp.headers.push(("HX-Retarget".into(), "#ask-note".into()));
        resp.headers.push(("HX-Reswap".into(), "innerHTML".into()));
        resp
    };
    if question.is_empty() {
        return failed("ask something first", "Type what you want in the Ask AI box, like 4K remuxes of Dune.");
    }
    let sorts: Vec<&str> = std::iter::once("relevance").chain(SortBy::ALL.iter().map(|(_, word)| *word)).collect();
    let choices = gemini::Choices { filters: FILTERS, sorts: &sorts };
    let settings = gemini::load();
    let plan = match gemini::plan(&settings, &question, &choices) {
        Ok(plan) => plan,
        Err(why) => return failed("Gemini did not answer", &why),
    };
    let search = Search::from_args(&plan_args(&plan));
    let query = search.query(&search.cat, 0);
    if !htmx {
        let to = if query.is_empty() { "/".to_string() } else { format!("/?{query}") };
        return ApiResponse {
            status: 303,
            content_type: None,
            body: String::new(),
            headers: vec![("Location".into(), to)],
            bytes: None,
            failed: false,
        };
    }
    let said = if plan.note.is_empty() { String::new() } else { format!(" {}", escape(&plan.note)) };
    let note = format!(
        "<div class=\"ask-read\" data-q=\"{}\"><p><span class=\"tag tag-ref\">GEMINI</span>{said}</p><p class=\"path\">Searched {}{}</p></div>",
        attr(&search.q),
        escape(&described(&search)),
        if plan.model.is_empty() || plan.model == settings.model() {
            String::new()
        } else {
            format!(". Answered by {}, as {} was busy", escape(&plan.model), escape(settings.model()))
        },
    );
    // the search box takes the words from `data-q` (web.js): swapping the box would drop the form's listener on it
    first_page(&search, &ask_note(&note, " hx-swap-oob=\"true\""))
}

fn settings_page(message: Option<(&str, &str, String)>) -> String {
    let settings = gemini::load();
    let masked = escape(&gemini::masked(&settings.api_key));
    let status = if settings.from_env {
        format!(
            "Atlas uses the key in <code>GEMINI_API_KEY</code>, <code>{masked}</code>. A key saved here takes its place."
        )
    } else if settings.has_key() {
        format!("A key is saved: <code>{masked}</code>. Paste another to replace it.")
    } else {
        "No key yet. Make one in <a href=\"https://aistudio.google.com/apikey\" target=\"_blank\" rel=\"noreferrer noopener\">Google AI Studio</a>, then paste it here.".to_string()
    };
    let forget = if settings.has_key() && !settings.from_env {
        "<button class=\"button\" name=\"do\" value=\"forget\">Remove key</button>"
    } else {
        ""
    };
    let message = message.map_or(String::new(), |(level, title, body)| diag(level, title, &body));
    page(
        "Atlas settings",
        &format!(
            r#"{masthead}<main class="detail settings">
<p class="path"><a href="/">Atlas</a> › Settings</p><h1>Settings</h1>{message}
<section class="panel"><h2>Ask AI with Gemini</h2>
<p>Ask AI, on the search page, sends what you type to Google's Gemini with the list of categories and filters, then searches with the fields Gemini sends back. Nothing from your index goes to Google.</p>
<p>{status}</p>
<form class="settings-form" method="post" action="/ui/settings">
<label for="gemini-key">Gemini API key</label>
<input id="gemini-key" name="key" type="password" autocomplete="off" spellcheck="false" placeholder="{placeholder}">
<label for="gemini-model">Model</label>
<input id="gemini-model" name="model" type="text" value="{model}" placeholder="{default}" autocomplete="off" spellcheck="false">
<p class="hint">Leave it empty for <code>{default}</code>, which follows Google's newest Flash model.</p>
<div class="actions"><button class="button primary" name="do" value="save">Save</button>{forget}</div>
</form></section></main>"#,
            masthead = masthead(true),
            placeholder = if settings.has_key() { "Paste a new key to replace it" } else { "Paste your key" },
            model = attr(&settings.model),
            default = gemini::DEFAULT_MODEL,
        ),
    )
}

fn save_settings(body: &str) -> ApiResponse {
    let args = parse_query(body);
    let get = |k: &str| args.get(k).map_or("", |v| v.trim());
    let message = if get("do") == "forget" {
        match gemini::forget() {
            Ok(()) => ("help", "the key is removed", "Ask AI is off until you add another.".to_string()),
            Err(e) => ("caution", "could not remove the key", e.to_string()),
        }
    } else {
        let (key, model) = (get("key"), get("model"));
        if !key.is_empty() && !gemini::looks_like_key(key) {
            ("caution", "that is not a Gemini key", "A key is one line of letters, digits, - and _.".to_string())
        } else if !model.is_empty()
            && let Err(why) = gemini::model_path(model)
        {
            ("caution", "that is not a model name", why)
        } else {
            match gemini::save(key, model) {
                Err(e) => ("caution", "could not save", e.to_string()),
                Ok(saved) if !saved.has_key() => ("note", "saved", "Paste a key to turn Ask AI on.".to_string()),
                Ok(saved) => match gemini::check(&saved) {
                    Ok(()) => ("help", "saved", format!("Gemini took the key, and knows {}.", saved.model())),
                    Err(why) => ("caution", "saved, but Gemini did not take it", why),
                },
            }
        }
    };
    html(200, settings_page(Some(message)))
}

fn login_page(base: &str, wrong_key: bool) -> String {
    let error = if wrong_key {
        diag("caution", "that is not the API key", "Check api_key in config.toml.")
    } else {
        String::new()
    };
    let base = escape(base);
    page(
        "Atlas sign in",
        &format!(
            r#"{masthead}<div class="signin">
<div class="panel">{error}<form method="post" action="/login">
<label for="key">API key</label>
<input id="key" name="key" type="password" autocomplete="current-password" autofocus required>
<button class="button primary">Sign in</button>
</form></div>
<section class="panel setup"><h2>Add Atlas to Prowlarr, Sonarr or Radarr</h2>
<p>Add it as a Newznab indexer, with the <code>api_key</code> in <code>config.toml</code>.</p>
<ul><li>URL: <code>{base}</code></li><li>API path: <code>/api</code></li><li>Check it: <code>{base}/api?t=caps</code></li></ul></section>
</div>"#,
            masthead = masthead(false),
        ),
    )
}

/// Each layer an archive was followed down through, outermost first: `RAR5 m0 › 7z copy › movie.mkv`.
fn nested_chain(layers: &[crate::probe::nest::Layer]) -> String {
    let mut parts: Vec<String> = layers
        .iter()
        .map(|l| {
            let method = l.method.as_deref().map(|m| format!(" {m}")).unwrap_or_default();
            let stop =
                l.stop.as_deref().map(|why| format!(" {}", tag("tag-bad", &why.to_uppercase()))).unwrap_or_default();
            format!("{}{}{stop}", escape(&l.format), escape(&method))
        })
        .collect();
    if let Some(name) = layers.last().filter(|l| l.stop.is_none()).and_then(|l| l.name.as_deref()) {
        parts.push(escape(name));
    }
    parts.join(" › ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "secret-key";

    fn cookie() -> String {
        format!("other=1; {COOKIE}={}", session_token(KEY))
    }

    fn header<'a>(r: &'a ApiResponse, name: &str) -> Option<&'a str> {
        r.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    #[test]
    fn signing_in_takes_the_key_and_the_cookie_is_not_the_key() {
        let wrong = post("/login", "key=nope", None, false, "http://x", KEY).unwrap();
        assert_eq!(wrong.status, 401);
        assert!(header(&wrong, "Set-Cookie").is_none());
        assert!(wrong.body.contains("not the API key"));

        let right = post("/login", &format!("key={KEY}"), None, false, "http://x", KEY).unwrap();
        assert_eq!(right.status, 303);
        assert_eq!(header(&right, "Location"), Some("/"));
        let set = header(&right, "Set-Cookie").unwrap();
        assert!(set.contains("HttpOnly") && set.contains("SameSite=Strict"), "{set}");
        assert!(!set.contains(KEY), "{set}");

        let out = post("/logout", "", None, false, "http://x", KEY).unwrap();
        assert!(header(&out, "Set-Cookie").unwrap().contains("Max-Age=0"));
        assert!(post("/api", "", None, false, "http://x", KEY).is_none());
    }

    #[test]
    fn only_the_right_cookie_gets_in() {
        for bad in [None, Some("atlas_session=nope"), Some("x=1"), Some("atlas_session"), Some(KEY)] {
            assert!(!signed_in(bad, KEY), "{bad:?}");
            let results = get("/ui/results", "", bad, "http://x", KEY).unwrap();
            assert_eq!(results.status, 401, "{bad:?}");
            assert_eq!(header(&results, "HX-Redirect"), Some("/"));
            assert_eq!(get("/ui/details", "id=1", bad, "http://x", KEY).unwrap().status, 401, "{bad:?}");
            assert_eq!(get("/ui/poster", "kind=2&tmdb=1", bad, "http://x", KEY).unwrap().status, 401, "{bad:?}");
            let nzb = get("/ui/nzb", "id=1", bad, "http://x", KEY).unwrap();
            assert_eq!(nzb.status, 303, "{bad:?}");
            assert_eq!(get("/ui/release", "id=1", bad, "http://x", KEY).unwrap().status, 303, "{bad:?}");
            let root = get("/", "", bad, "http://x", KEY).unwrap();
            assert!(root.body.contains("action=\"/login\""), "{bad:?}");
        }
        assert!(signed_in(Some(&cookie()), KEY));
        assert!(!signed_in(Some(&cookie()), "another key"), "a new key ends old sessions");
    }

    #[test]
    fn the_sign_in_page_says_how_to_set_up_a_client_without_the_key() {
        let root = get("/", "", None, "http://192.168.1.172:9091", KEY).unwrap();
        assert_eq!(root.status, 200);
        assert!(root.body.contains("http://192.168.1.172:9091/api?t=caps"));
        assert!(!root.body.contains(KEY));
    }

    #[test]
    fn a_hostile_host_header_is_escaped() {
        let root = get("/", "", None, "http://<script>alert(1)</script>", KEY).unwrap();
        assert!(!root.body.contains("<script>alert"), "{}", root.body);
    }

    #[test]
    fn htmx_and_css_are_served_without_signing_in() {
        let js = get("/static/htmx.min.js", "", None, "http://x", KEY).unwrap();
        assert_eq!(js.status, 200);
        assert!(js.body.contains("htmx"));
        assert!(js.content_type.unwrap().starts_with("text/javascript"));
        assert_eq!(get("/static/atlas.css", "", None, "http://x", KEY).unwrap().status, 200);
        assert_eq!(get("/static/atlas.js", "", None, "http://x", KEY).unwrap().status, 200);
        assert_eq!(get("/static/atlas.svg", "", None, "http://x", KEY).unwrap().status, 200);
        assert!(get("/static/nope", "", None, "http://x", KEY).is_none());
        assert!(get("/api", "t=caps", None, "http://x", KEY).is_none());
    }

    #[test]
    fn a_row_escapes_what_came_from_usenet() {
        let release = ReleaseRow {
            id: 7,
            name: "<img src=x onerror=alert(1)>".into(),
            group_name: "<b>g</b>".into(),
            poster: None,
            posted_date: Some("<i>2026</i>".into()),
            size: Some(2048),
            complete: false,
            parts: None,
        };
        let passwords = HashMap::from([(7, Password::Maybe)]);
        let html = row(&release, &HashMap::from([(7, 5040)]), &passwords, &HashMap::new(), &RowExtras::default());
        assert!(!html.contains("<img") && !html.contains("<b>") && !html.contains("<i>"), "{html}");
        assert!(html.contains("TV/HD") && html.contains("2.0 KB"), "{html}");
        assert!(html.contains("PASSWORD?") && html.contains("INCOMPLETE"), "{html}");
        assert!(html.contains("/ui/nzb?id=7") && html.contains("/ui/release?id=7"), "{html}");
    }

    #[test]
    fn a_row_carries_the_drawer_trigger() {
        let release = ReleaseRow {
            id: 7,
            name: "Some.Show.S01E01-GRP".into(),
            group_name: "alt.binaries.g".into(),
            poster: None,
            posted_date: Some("2026-10-03 17:41:49".into()),
            size: Some(2048),
            complete: false,
            parts: None,
        };
        let html = row(&release, &HashMap::new(), &HashMap::new(), &HashMap::new(), &RowExtras::default());
        assert_eq!(html.matches("class=\"icon-button drawer-toggle\"").count(), 1, "{html}");
        // after the tags, before the row's second line
        let toggle = html.find("drawer-toggle").unwrap();
        assert!(
            html.find("INCOMPLETE").unwrap() < toggle && toggle < html.find("<div class=\"row-meta\">").unwrap(),
            "{html}"
        );
        assert!(html.contains("hx-get=\"/ui/row?id=7\"") && html.contains("hx-target=\"closest tr\""), "{html}");
        assert!(html.contains("<td class=\"num col-size\" title=\"2,048 bytes\">2.0 KB</td>"), "{html}");
    }

    #[test]
    fn a_copy_carries_the_drawer_trigger_as_a_card() {
        let release = ReleaseRow {
            id: 7,
            name: "Some.Show.S01E01-GRP".into(),
            group_name: "alt.binaries.g".into(),
            poster: Some("poster <p@example.com>".into()),
            posted_date: Some("2026-10-03 17:41:49".into()),
            size: Some(2048),
            complete: true,
            parts: None,
        };
        // a link opens its drawer inside its own list item
        let link = copy_link(&tag("tag-ok", "TWIN"), &release);
        assert!(
            link.starts_with("<li><a href=\"/ui/release?id=7\">Some.Show.S01E01-GRP</a>") && link.ends_with("</li>")
        );
        assert_eq!(link.matches("drawer-toggle").count(), 1, "{link}");
        assert!(
            link.contains("hx-get=\"/ui/row?id=7&amp;as=card\"")
                && link.contains("hx-target=\"closest li\"")
                && link.contains("hx-swap=\"beforeend\""),
            "{link}"
        );
        // a card opens it under its facts, inside the card's body
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        crate::db::create_db_at(&main).unwrap();
        let conn = crate::db::open_with_shards(&main).unwrap();
        let card = copy_card(&conn, &tag("tag-ok", "TWIN"), &release);
        assert_eq!(card.matches("drawer-toggle").count(), 1, "{card}");
        let toggle = card.find("drawer-toggle").unwrap();
        assert!(card.find("TWIN").unwrap() < toggle && toggle < card.find("href=\"/ui/nzb?id=7\"").unwrap(), "{card}");
        assert!(
            card.contains("hx-get=\"/ui/row?id=7&amp;as=card\"") && card.contains("hx-target=\"closest .copy-body\""),
            "{card}"
        );
        assert!(card.contains("<dt>Poster</dt>") && card.contains("<dt>Email</dt>"), "{card}");
    }

    #[test]
    fn search_links_keep_the_search_and_stay_on_a_page_start() {
        assert_eq!(search_query("", "", 0), "");
        assert_eq!(search_query("a b&c", "5000", 50), "q=a%20b%26c&cat=5000&offset=50");
        let args = parse_query("q=+x+&cat=2000&offset=120");
        let s = Search::from_args(&args);
        assert_eq!((s.q.as_str(), s.cat.as_str(), s.offset), ("x", "2000", 100));
        assert_eq!(Search::from_args(&parse_query("offset=-5")).offset, 0);
        assert!(attr(r#"a"b'c<"#).find(['"', '\'', '<']).is_none());
    }

    #[test]
    fn the_sidebar_marks_the_category_and_keeps_the_search() {
        let search = Search {
            q: "a b".into(),
            cat: "5040".into(),
            offset: 0,
            filters: Vec::new(),
            fields: Fields::default(),
            sort: None,
            descending: true,
        };
        let nav = category_nav(&search, "");
        assert_eq!(nav.matches("aria-current").count(), 1, "{nav}");
        assert!(
            nav.contains(r#"href="/?q=a%20b&amp;cat=5040" hx-get="/ui/results?cat=5040" aria-current="page""#),
            "{nav}"
        );
        let unknown = Search {
            q: String::new(),
            cat: "<x>".into(),
            offset: 0,
            filters: Vec::new(),
            fields: Fields::default(),
            sort: None,
            descending: true,
        };
        assert_eq!(category_nav(&unknown, "").matches("aria-current").count(), 0);
        assert!(!category_input("\"><x>", "").contains("<x>"));
    }

    #[test]
    fn an_empty_result_says_why_and_how_to_widen_it() {
        let hd = Search {
            q: String::new(),
            cat: "2040".into(),
            offset: 0,
            filters: Vec::new(),
            fields: Fields::default(),
            sort: None,
            descending: true,
        }
        .nothing_found();
        assert!(hd.contains("nothing is in Movies / HD yet"), "{hd}");
        assert!(hd.contains("Look in all categories"), "{hd}");
        let word = Search {
            q: "<b>".into(),
            cat: String::new(),
            offset: 0,
            filters: Vec::new(),
            fields: Fields::default(),
            sort: None,
            descending: true,
        }
        .nothing_found();
        assert!(word.contains("&lt;b&gt;") && !word.contains("<b>"), "{word}");
        assert!(!word.contains("Look in all categories"), "{word}");
    }

    #[test]
    fn the_path_names_the_category() {
        let path = Search {
            q: String::new(),
            cat: "2040".into(),
            offset: 0,
            filters: Vec::new(),
            fields: Fields::default(),
            sort: None,
            descending: true,
        }
        .summary();
        assert!(path.contains("atlas::releases::<b>movies</b>::<b>hd</b>"), "{path}");
        assert!(path.contains("Newest first"), "{path}");
        assert!(
            Search {
                q: "x".into(),
                cat: "9999".into(),
                offset: 0,
                filters: Vec::new(),
                fields: Fields::default(),
                sort: None,
                descending: true
            }
            .summary()
            .contains("Best or newest matches first")
        );
    }

    #[test]
    fn health_compares_what_is_missing_with_what_can_be_repaired() {
        let whole = release_info::Contents { articles_seen: 100, articles_expected: 100, ..Default::default() };
        assert!(health(&whole, 0).contains("ALL THERE"));
        let holes = release_info::Contents {
            articles_seen: 107,
            articles_expected: 110,
            data_articles_seen: 97,
            data_articles_expected: 100,
            recovery_articles_seen: 10,
            recovery_articles_expected: 10,
            recovery_volumes: 3,
            data_bytes: 1000,
            recovery_bytes: 100,
            ..Default::default()
        };
        let text = health(&holes, holes.missing_per_mille().unwrap());
        assert!(text.contains("2.7% missing") && text.contains("REPAIRABLE"), "{text}");
        let thin = release_info::Contents { recovery_bytes: 10, ..holes };
        assert!(health(&thin, 27).contains("NOT ENOUGH RECOVERY"));
        assert_eq!(per_mille(25), "2.5%");
    }

    #[test]
    fn title_details_are_escaped_and_leave_out_what_is_missing() {
        let d = crate::media::details::Details {
            kind: 2,
            tmdb_id: 603,
            title: Some("<b>Film</b>".into()),
            overview: Some("A <i>film</i>.".into()),
            imdb_score: Some(87),
            poster: Some("/a.jpg".into()),
            bluray: Some("1999-09-21".into()),
            ..Default::default()
        };
        let html = details_html(&d);
        assert!(!html.contains("<b>") && !html.contains("<i>"), "{html}");
        assert!(html.contains("IMDB 87") && !html.contains("Metacritic"), "{html}");
        assert!(html.contains("<dt>Blu-ray</dt><dd>1999-09-21</dd>"), "{html}");
        assert!(html.contains("/ui/poster?kind=2&amp;tmdb=603"), "{html}");
        assert!(!html.contains("art=backdrop"), "{html}");
        let wide = details_html(&crate::media::details::Details { backdrop: Some("/b.jpg".into()), ..d.clone() });
        assert!(wide.contains("art=backdrop"), "{wide}");
    }

    #[test]
    fn encryption_says_what_is_known() {
        use crate::archive::Encryption;
        use crate::archive::store::InnerView;
        let aes = InnerView {
            password: Some(Password::Yes),
            encryption: Some(Encryption::Rar5Aes256),
            names_hidden: true,
            ..InnerView::default()
        };
        assert_eq!(encryption_fact(&aes), "RAR5, AES-256, file names hidden");
        let old = InnerView { password: Some(Password::Yes), ..InnerView::default() };
        assert_eq!(encryption_fact(&old), "Encrypted, type not recorded");
        assert_eq!(encryption_fact(&InnerView::default()), "");
    }

    fn release(id: i64, name: &str, size: Option<i64>) -> ReleaseRow {
        ReleaseRow {
            id,
            name: name.into(),
            group_name: format!("g{id}"),
            poster: None,
            posted_date: None,
            size,
            complete: true,
            parts: None,
        }
    }

    #[test]
    fn every_column_sorts_both_ways() {
        let order = |by, descending| {
            let mut rows = vec![release(1, "b", Some(5)), release(2, "A", None), release(3, "c", Some(9))];
            sort_rows(&mut rows, by, descending, &HashMap::new(), &HashMap::new());
            rows.iter().map(|r| r.id).collect::<Vec<_>>()
        };
        assert_eq!(order(SortBy::Name, false), [2, 1, 3]);
        assert_eq!(order(SortBy::Name, true), [3, 1, 2]);
        assert_eq!(order(SortBy::Size, false), [1, 3, 2], "no size goes last");
        assert_eq!(order(SortBy::Size, true), [3, 1, 2], "either way");
        assert_eq!(order(SortBy::Group, true), [3, 2, 1]);
    }

    #[test]
    fn tags_are_read_in_order_until_the_page_is_full() {
        let names = ["A.2024.2160p.WEB-DL.DV.H265-X", "B.2024.1080p.WEB-DL.H264-X", "C.2024.2160p.BluRay.REMUX-X"];
        let rows: Vec<ReleaseRow> = (0..300)
            .map(|i| ReleaseRow { id: i, name: names[i as usize % 3].to_string(), ..Default::default() })
            .collect();
        let uhd = Search::from_args(&parse_query("filters=2160p"));
        let kept = uhd.keep_tagged(rows.clone(), usize::MAX);
        assert_eq!(kept.len(), 200);
        assert!(kept.windows(2).all(|w| w[0].id < w[1].id), "in order");
        assert_eq!(uhd.keep_tagged(rows.clone(), 3).len(), (0..TAG_SLICE).filter(|i| i % 3 != 1).count(), "one slice");
        let all = Search::from_args(&parse_query("filters=Hide%20copies"));
        assert_eq!(all.keep_tagged(rows, 1).len(), 300, "no tags to read");
    }

    #[test]
    fn rated_rows_come_first_either_way() {
        let row = |id| ReleaseRow { id, ..Default::default() };
        let scores = HashMap::from([(1, 50), (2, 90)]);
        let mut rows = [row(3), row(1), row(2)];
        sort_rows(&mut rows, SortBy::Rating, true, &HashMap::new(), &scores);
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [2, 1, 3]);
        sort_rows(&mut rows, SortBy::Rating, false, &HashMap::new(), &scores);
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [1, 2, 3]);
    }

    #[test]
    fn sort_headings_flip_and_keep_the_search() {
        let s = Search::from_args(&parse_query("q=x&sort=size&dir=desc&email=a%40b.c"));
        assert_eq!(s.sort, Some(SortBy::Size));
        let link = s.sort_link(SortBy::Size, "Size");
        assert!(link.contains("sort=size&amp;dir=asc") && link.contains("▼"), "{link}");
        assert!(link.contains("email=a%40b.c") && link.contains("q=x"), "{link}");
        let other = s.sort_link(SortBy::Name, "Name");
        assert!(other.contains("sort=name&amp;dir=asc") && !other.contains("▼"), "{other}");
        assert_eq!(thousands(2_684_354_560), "2,684,354,560");
    }

    #[test]
    fn filters_come_from_the_list_and_stay_in_links() {
        let s = Search::from_args(&parse_query("q=x&filters=DV,<b>,Remux"));
        assert_eq!(s.filters, ["DV", "Remux"]);
        assert!(s.windowed(), "tags are checked over the first results");
        assert_eq!(s.query("2040", 0), "q=x&cat=2040&filters=DV%2CRemux");
        let hide = Search::from_args(&parse_query("filters=Hide%20copies"));
        assert!(!hide.filters_by_tags() && !hide.windowed(), "pages by offset as before");
        let links = s.filter_links();
        assert_eq!(links.matches("aria-current").count(), 2, "{links}");
        assert!(!filter_matches("DV", &release_tags::parse("A.1999.DVDRip.XviD-X")), "not a DVD");
        assert!(filter_matches("DV", &release_tags::parse("A.2024.2160p.DV.WEB-DL.H265-X")));
        assert!(filter_matches("HDR", &release_tags::parse("A.2024.HDR10Plus.WEB-DL")));
        assert!(filter_matches("Remux", &release_tags::parse("A.2024.BluRay.REMUX-X")));
    }

    #[test]
    fn result_tags_show_resolution_hdr_and_source() {
        let tags = name_tags(&release_tags::parse("A.2024.2160p.WEB-DL.DV.HDR10Plus.H265-X"));
        for word in ["2160p", "DV", "HDR10+", "WEB-DL"] {
            assert!(tags.contains(word), "{tags}");
        }
    }

    #[test]
    fn media_labels_and_id_links() {
        let episode = MediaAttrs { season: Some(1), episode: Some(2), ..MediaAttrs::default() };
        assert_eq!(media_label(&episode).as_deref(), Some("S01E02"));
        assert_eq!(media_label(&MediaAttrs { year: Some(2024), ..MediaAttrs::default() }).as_deref(), Some("2024"));
        assert_eq!(media_label(&MediaAttrs::default()), None);
        let ids = crate::media::resolve::Ids { imdb: Some(133093), tmdb: Some(603), ..Default::default() };
        let movie = id_links(Kind::Movie as i64, &ids);
        assert!(movie.contains("https://www.imdb.com/title/tt0133093/"), "{movie}");
        assert!(movie.contains("https://www.themoviedb.org/movie/603"), "{movie}");
        assert!(id_links(Kind::Tv as i64, &ids).contains("https://www.themoviedb.org/tv/603"));
        let trakt = crate::media::resolve::Ids { trakt: Some(9), ..Default::default() };
        assert!(id_links(Kind::Tv as i64, &trakt).contains("trakt.tv/search/trakt/9?id_type=show"));
    }
}
