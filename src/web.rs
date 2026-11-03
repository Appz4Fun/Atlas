//! A small browser ui on the api port: search, browse and download NZBs.
//!
//! The api key never reaches a page. Signing in with it sets a cookie derived from it,
//! and the server does the searching and the NZB downloads itself.

use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

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
        "/ui/cover" if signed_in => cover(&args),
        "/ui/still" if signed_in => still(&args),
        "/ui/samefilm" if signed_in => same_film_panel(&args),
        "/ui/meta" if signed_in => crate::web_meta::fragment(&args),
        "/ui/row" if signed_in => crate::web_meta::row_panel(&args),
        "/ui/settings" if signed_in => html(200, settings_page(None)),
        "/ui/nzb" if signed_in && args.get("fill").is_some_and(|v| v == "1") => filled_nzb(&args),
        "/ui/nzb" if signed_in => get_nzb(&args),
        "/ui/details" | "/ui/poster" | "/ui/cover" | "/ui/still" | "/ui/samefilm" | "/ui/meta" | "/ui/row" => {
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

#[derive(Clone)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    rows.sort_by(|a, b| row_order(a, b, by, descending, categories, scores));
}

fn row_order(
    a: &ReleaseRow,
    b: &ReleaseRow,
    by: SortBy,
    descending: bool,
    categories: &HashMap<i64, i64>,
    scores: &HashMap<i64, i64>,
) -> std::cmp::Ordering {
    let flip = |o: std::cmp::Ordering| if descending { o.reverse() } else { o };
    match by {
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
    }
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
        if self.sorts_from_a_window() {
            return self.body_from_window();
        }
        let filter = parse_filter(&self.cat);
        let windowed = self.windowed();
        let (from, count) = if windowed { (0, SORT_WINDOW) } else { (self.offset, PAGE_SIZE) };
        let searched = self.search(filter.as_ref(), from, count);
        let found = match searched {
            Ok(found) => found,
            Err(e) => return self.search_failed(&e),
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
                    for r in found.iter_mut() {
                        if let Ok(Some(post)) = release_info::whole_post_with(&conn, r) {
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
        let post_of =
            |r: &ReleaseRow| conn.as_ref().and_then(|conn| release_info::whole_post_with(conn, r).ok().flatten());
        let folded = |r: &ReleaseRow, post: &release_info::SplitPost| self.folds(r, post);
        // a row folded into one that isn't on this page brings that one here in its place: a page of nothing but an
        // upload's recovery rows would show none of it
        let on_page: HashSet<i64> = found.iter().map(|r| r.id).collect();
        let mut brought = HashSet::new();
        found = std::mem::take(&mut found)
            .into_iter()
            .filter_map(|mut r| match self.fold_split_post(conn.as_ref(), &mut r, &mut split) {
                None => Some(r),
                Some(listed) if !on_page.contains(&listed) && brought.insert(listed) => {
                    self.bring(conn.as_ref(), listed, &mut split)
                }
                Some(_) => None,
            })
            .collect();
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
        let mut out = rows_html(&found, split).concat();
        if page_was_full {
            out.push_str(&self.load_more());
        }
        out
    }

    fn search_failed(&self, error: &str) -> String {
        eprintln!("web search failed: {error}");
        let message = "Atlas couldn't read its database. The error is in its log. Search again in a moment.";
        if self.offset == 0 {
            diag("caution", "the search failed", message)
        } else {
            format!("<tr><td colspan=\"{COLUMNS}\" class=\"note\">{}</td></tr>", escape(message))
        }
    }

    fn load_more(&self) -> String {
        let next = attr(&self.query(&self.cat, self.offset + PAGE_SIZE));
        format!(
            "<tr><td colspan=\"{COLUMNS}\" class=\"note\"><a class=\"button\" href=\"/?{next}\" hx-get=\"/ui/results?{next}\" hx-target=\"closest tr\" hx-swap=\"outerHTML\">Load more</a></td></tr>"
        )
    }

    /// Whether a row of a post is left out for the row that stands for the post: a post spread over several groups,
    /// unless the search is about one of its groups, or an upload linked by its PAR2 set, which is in one group soo it
    /// stays one row whatever the search.
    fn folds(&self, r: &ReleaseRow, post: &release_info::SplitPost) -> bool {
        (self.fields.group.is_empty() || post.lead.is_some() || post.groups.len() == 1) && post.listed_id() != r.id
    }

    /// Makes a row of a post say what all of the post comes to. When another row stands for it, that row's id: the
    /// row is left out for it.
    fn fold_split_post(
        &self,
        conn: Option<&rusqlite::Connection>,
        r: &mut ReleaseRow,
        split: &mut HashMap<i64, release_info::SplitPost>,
    ) -> Option<i64> {
        let Some(post) = conn.and_then(|conn| release_info::whole_post_with(conn, r).ok().flatten()) else {
            if !r.complete
                && let Some(conn) = conn
                && release_info::single_post_complete(conn, r.id).unwrap_or(false)
            {
                r.complete = true;
            }
            return None;
        };
        if self.folds(r, &post) {
            return Some(post.listed_id());
        }
        if post.lead.is_none() {
            r.name = release_info::post_stem(&r.name);
        }
        r.size = Some(post.size);
        r.complete = post.complete;
        split.insert(r.id, post);
        None
    }

    /// The row that stands for a post, read to show in place of one folded into it.
    fn bring(
        &self,
        conn: Option<&rusqlite::Connection>,
        listed: i64,
        split: &mut HashMap<i64, release_info::SplitPost>,
    ) -> Option<ReleaseRow> {
        let mut r = conn.and_then(|conn| crate::search::get_release_with(conn, listed).ok().flatten())?;
        // it stands for its post, soo it isn't folded itself
        self.fold_split_post(conn, &mut r, split);
        Some(r)
    }

    /// A sort takes its page from the search's window, with no database in the way once the window has been filled.
    /// "Hide copies" depends on what's before the page, soo it still works the page out each time.
    fn sorts_from_a_window(&self) -> bool {
        self.sort.is_some() && !self.filters.contains(&HIDE_COPIES)
    }

    fn body_from_window(&self) -> String {
        let window = match window_of(self) {
            Ok(window) => window,
            Err(e) => return self.search_failed(&e),
        };
        let Some(by) = self.sort else { return String::new() };
        let (rows, more) = window.page(self, by);
        if rows.is_empty() {
            return if self.offset == 0 {
                self.nothing_found()
            } else {
                format!("<tr><td colspan=\"{COLUMNS}\" class=\"note dim\">That's all of them.</td></tr>")
            };
        }
        let mut out = rows.concat();
        if more {
            out.push_str(&self.load_more());
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
        let total = if !self.fields.any() || self.filters_by_tags() {
            String::new()
        } else if self.sorts_from_a_window()
            && let Ok(window) = window_of(self)
        {
            window.total.get_or_init(|| self.total()).clone()
        } else {
            self.total()
        };
        format!(
            "<div class=\"summary\"><span class=\"path\">{path}</span>{total}<span class=\"sort\">{}</span></div>{}",
            escape(&sort),
            if scope.is_empty() { String::new() } else { format!("<div class=\"chips\">{scope}</div>") },
        )
    }

    fn total(&self) -> String {
        let filter = parse_filter(&self.cat);
        match field_search::count_any(&self.fields, std::slice::from_ref(&self.q), filter.as_ref()) {
            Ok(total) => format!("<strong class=\"total\">{}</strong>", escape(&total.describe())),
            Err(e) => {
                eprintln!("web count failed: {e}");
                String::new()
            }
        }
    }
}

/// How old a window can get before a click on it starts a new one. The click still gets the old one straight away.
const WINDOW_FRESH: Duration = Duration::from_secs(60);
/// How often the windows nobody has clicked lately are made again, soo the first click of the day finds one.
const WINDOW_IDLE_FRESH: Duration = Duration::from_secs(15 * 60);
/// What counts as lately.
const WINDOW_IN_USE: Duration = Duration::from_secs(10 * 60);
/// Searches with a window: a filled one is a few MB.
const WINDOWS_KEPT: usize = 24;
/// How long a click sorting by size waits for a new window's split posts, the most that a size can depend on. Past it
/// the page goes by the sizes known so far, and the next click has them all.
const SIZES_WAIT: Duration = Duration::from_secs(1);
/// Threads rendering a window's rows.
const FILL_THREADS: usize = 4;

/// Places in a window's rows, in the order a column and direction put them.
type Order = Arc<Vec<usize>>;

/// One row as a results page shows it.
struct ShownRow {
    html: String,
    /// what it sorts by: all of a post spread over several groups
    size: Option<i64>,
    /// another row stands for its post
    folded: bool,
}

/// The first `SORT_WINDOW` results of one search, whatever the sort, kept in memory: choosing a column only puts these
/// in another order. Rows are rendered in the background, each the first time something needs it.
struct Window {
    built: Instant,
    rows: Vec<ReleaseRow>,
    /// by release, for the columns that need them, read once
    categories: OnceLock<HashMap<i64, i64>>,
    scores: OnceLock<HashMap<i64, i64>>,
    total: OnceLock<String>,
    shown: Mutex<HashMap<i64, ShownRow>>,
    /// rows brought in for a post whose own row isn't in the window: once each
    brought: Mutex<HashSet<i64>>,
    orders: Mutex<HashMap<(SortBy, bool), Order>>,
    /// every row rendered, soo every size is final: a size order is kept only from then on
    sized: AtomicBool,
    /// bumped, under `shown`, each time rows are kept: a size order read from older sizes isn't kept
    shown_gen: std::sync::atomic::AtomicU64,
    filled: AtomicBool,
}

impl Window {
    fn read(search: &Search) -> Result<Window, String> {
        let filter = parse_filter(&search.cat);
        let rows = search.search(filter.as_ref(), 0, SORT_WINDOW)?;
        Ok(Window {
            built: Instant::now(),
            rows: search.keep_tagged(rows, usize::MAX),
            categories: OnceLock::new(),
            scores: OnceLock::new(),
            total: OnceLock::new(),
            shown: Mutex::new(HashMap::new()),
            brought: Mutex::new(HashSet::new()),
            orders: Mutex::new(HashMap::new()),
            sized: AtomicBool::new(false),
            shown_gen: std::sync::atomic::AtomicU64::new(0),
            filled: AtomicBool::new(false),
        })
    }

    fn ids(&self) -> Vec<i64> {
        self.rows.iter().map(|r| r.id).collect()
    }

    fn categories(&self) -> &HashMap<i64, i64> {
        self.categories.get_or_init(|| crate::category::categories_of(&self.ids()).unwrap_or_default())
    }

    /// Only the scores already kept: fetching them for a whole search would use up the providers' limits.
    fn scores(&self) -> &HashMap<i64, i64> {
        self.scores.get_or_init(|| {
            let media = crate::media::query::attrs_of(&self.ids()).unwrap_or_default();
            crate::media::details::kept_for(media.iter().map(|(id, m)| (*id, m.kind, &m.ids)))
                .into_iter()
                .filter_map(|(id, d)| Some((id, score_value(&d)?)))
                .collect()
        })
    }

    fn order(&self, by: SortBy, descending: bool) -> Order {
        if let Some(order) = lock(&self.orders).get(&(by, descending)) {
            return order.clone();
        }
        let none = HashMap::new();
        let categories = if by == SortBy::Category { self.categories() } else { &none };
        let scores = if by == SortBy::Rating { self.scores() } else { &none };
        let mut sized = None;
        // read before the sizes: an order from sizes that aren't final yet is answered with but never kept, soo it
        // can't outlive the fill
        let keep = by != SortBy::Size || self.sized.load(Ordering::Acquire);
        let mut gen_read = None;
        if by == SortBy::Size {
            let shown = lock(&self.shown);
            gen_read = Some(self.shown_gen.load(Ordering::Acquire));
            let mut rows = self.rows.clone();
            for r in &mut rows {
                if let Some(row) = shown.get(&r.id) {
                    r.size = row.size;
                }
            }
            sized = Some(rows);
        }
        let rows = sized.as_ref().unwrap_or(&self.rows);
        let mut order: Vec<usize> = (0..rows.len()).collect();
        order.sort_by(|&a, &b| row_order(&rows[a], &rows[b], by, descending, categories, scores));
        let order = Arc::new(order);
        if keep {
            let mut orders = lock(&self.orders);
            // a render that published since may have changed a size: then this order is answered with, not kept
            if gen_read.is_none_or(|read| read == self.shown_gen.load(Ordering::Acquire)) {
                orders.insert((by, descending), order.clone());
            }
        }
        order
    }

    /// Renders rows that aren't yet.
    fn render(&self, search: &Search, rows: &[ReleaseRow]) {
        let conn = crate::db::open().ok();
        // each kept row under the id of the window row it shows in
        let mut places = Vec::with_capacity(rows.len());
        let mut kept = Vec::with_capacity(rows.len());
        let mut split = HashMap::new();
        let mut done = Vec::with_capacity(rows.len());
        for r in rows {
            let place = r.id;
            let mut r = r.clone();
            match search.fold_split_post(conn.as_ref(), &mut r, &mut split) {
                None => {
                    places.push(place);
                    kept.push(r);
                }
                // a row folded into one that isn't in the window brings that one in its place, once
                Some(listed)
                    if !self.rows.iter().any(|w| w.id == listed)
                        && lock(&self.brought).insert(listed)
                        && let Some(shown) = search.bring(conn.as_ref(), listed, &mut split) =>
                {
                    places.push(place);
                    kept.push(shown);
                }
                Some(_) => done.push((place, ShownRow { html: String::new(), size: r.size, folded: true })),
            }
        }
        for ((place, r), html) in places.into_iter().zip(&kept).zip(rows_html(&kept, split)) {
            done.push((place, ShownRow { html, size: r.size, folded: false }));
        }
        self.publish(done);
    }

    /// Keeps rendered rows, each under its place.
    fn publish(&self, done: Vec<(i64, ShownRow)>) {
        let mut shown = lock(&self.shown);
        for (place, row) in done {
            // two renders of one place can race for the row it brings in: the one that lost says folded, and mustn't
            // hide the row the other one shows there
            if row.folded && shown.get(&place).is_some_and(|r| !r.folded) {
                continue;
            }
            shown.insert(place, row);
        }
        // a render can publish after the fill finished (a page's, that won the race for a brought row): a size order
        // kept from before it may be wrong now
        self.shown_gen.fetch_add(1, Ordering::AcqRel);
        drop(shown);
        lock(&self.orders).retain(|(by, _), _| *by != SortBy::Size);
    }

    /// A page's rows, and whether more follow. Renders only what the page reaches and nothing has rendered yet.
    fn page(&self, search: &Search, by: SortBy) -> (Vec<String>, bool) {
        if by == SortBy::Size {
            let started = Instant::now();
            while !self.filled.load(Ordering::Acquire) && started.elapsed() < SIZES_WAIT {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let order = self.order(by, search.descending);
        let skip = usize::try_from(search.offset).unwrap_or(0);
        let wanted = PAGE_SIZE as usize + 1;
        loop {
            let mut page = Vec::with_capacity(wanted);
            let mut missing = Vec::new();
            {
                let shown = lock(&self.shown);
                let mut passed = 0;
                for &place in order.iter() {
                    let r = &self.rows[place];
                    match shown.get(&r.id) {
                        Some(row) if row.folded => continue,
                        Some(row) => {
                            if passed >= skip {
                                page.push(row.html.clone());
                            }
                            passed += 1;
                        }
                        None => missing.push(r.clone()),
                    }
                    if page.len() == wanted || missing.len() == PAGE_SIZE as usize {
                        break;
                    }
                }
            }
            if missing.is_empty() {
                let more = page.len() == wanted;
                page.truncate(PAGE_SIZE as usize);
                return (page, more);
            }
            self.render(search, &missing);
        }
    }

    /// Renders every row and works out every order, soo no click has any of it left to do.
    fn fill(&self, search: &Search) {
        let shown = lock(&self.shown);
        let missing: Vec<ReleaseRow> = self.rows.iter().filter(|r| !shown.contains_key(&r.id)).cloned().collect();
        drop(shown);
        let slices: Vec<&[ReleaseRow]> = missing.chunks(PAGE_SIZE as usize).collect();
        let next = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..FILL_THREADS {
                scope.spawn(|| {
                    while let Some(slice) = slices.get(next.fetch_add(1, Ordering::Relaxed)) {
                        self.render(search, slice);
                    }
                });
            }
        });
        // sizes may have changed with the split posts
        self.sized.store(true, Ordering::Release);
        lock(&self.orders).clear();
        for (by, _) in SortBy::ALL {
            self.order(by, false);
            self.order(by, true);
        }
        self.filled.store(true, Ordering::Release);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// A search's window, and what keeps two clicks from each reading one.
struct WindowSlot {
    /// the search, with no sort or page
    search: Search,
    window: Mutex<Option<Arc<Window>>>,
    reading: Mutex<()>,
    replacing: AtomicBool,
    used: Mutex<Instant>,
    /// a category's own page, kept warm whether or not anyone has clicked it
    always: bool,
}

static WINDOWS: LazyLock<Mutex<HashMap<String, Arc<WindowSlot>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// One background read at a time, on a machine that's indexing.
static BACKGROUND: Mutex<()> = Mutex::new(());

/// Background fills and replacements waiting for their turn: no more than there are windows kept.
static WAITING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A turn at `BACKGROUND` once one comes, or None when as many are waiting as there are windows kept.
fn background_turn() -> Option<Waiting> {
    if WAITING.fetch_add(1, Ordering::AcqRel) >= WINDOWS_KEPT {
        WAITING.fetch_sub(1, Ordering::AcqRel);
        return None;
    }
    Some(Waiting)
}

/// One of `WAITING`, given back however the work ends.
struct Waiting;

impl Drop for Waiting {
    fn drop(&mut self) {
        WAITING.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Whether a slot is still kept: one dropped from `WINDOWS` while its work waited has no one to show it to.
fn still_kept(slot: &Arc<WindowSlot>) -> bool {
    lock(&WINDOWS).values().any(|kept| Arc::ptr_eq(kept, slot))
}

fn slot_of(search: &Search, always: bool) -> Arc<WindowSlot> {
    let plain = Search { offset: 0, sort: None, descending: true, ..search.clone() };
    let key = plain.query(&plain.cat, 0);
    let mut windows = lock(&WINDOWS);
    if !windows.contains_key(&key) && windows.len() >= WINDOWS_KEPT {
        let oldest = windows
            .iter()
            .filter(|(_, slot)| !slot.always)
            .min_by_key(|(_, slot)| *lock(&slot.used))
            .map(|(k, _)| k.clone());
        if let Some(oldest) = oldest {
            windows.remove(&oldest);
        }
    }
    windows
        .entry(key)
        .or_insert_with(|| {
            Arc::new(WindowSlot {
                search: plain,
                window: Mutex::new(None),
                reading: Mutex::new(()),
                replacing: AtomicBool::new(false),
                // nobody has clicked it yet
                used: Mutex::new(Instant::now().checked_sub(WINDOW_IN_USE).unwrap_or_else(Instant::now)),
                always,
            })
        })
        .clone()
}

/// The window to answer with now: the one there is, however old, or a new one when there's none.
fn window_of(search: &Search) -> Result<Arc<Window>, String> {
    let slot = slot_of(search, false);
    *lock(&slot.used) = Instant::now();
    if let Some(window) = lock(&slot.window).clone() {
        if window.built.elapsed() > WINDOW_FRESH {
            replace_in_background(&slot);
        }
        return Ok(window);
    }
    let _reading = lock(&slot.reading);
    if let Some(window) = lock(&slot.window).clone() {
        return Ok(window);
    }
    let window = Arc::new(Window::read(&slot.search)?);
    *lock(&slot.window) = Some(window.clone());
    // the window answers clicks without it, rendering what a page needs; filled, a click has nothing left to do.
    // Waiting, it holds neither the window nor its slot, soo a window dropped meanwhile is gone, and isn't filled
    if let Some(waiting) = background_turn() {
        let (filling, for_fill) = (Arc::downgrade(&window), Arc::downgrade(&slot));
        std::thread::spawn(move || {
            let _waiting = waiting;
            let _turn = lock(&BACKGROUND);
            let (Some(window), Some(slot)) = (filling.upgrade(), for_fill.upgrade()) else { return };
            let current = lock(&slot.window).as_ref().is_some_and(|w| Arc::ptr_eq(w, &window));
            if current && still_kept(&slot) {
                window.fill(&slot.search);
            }
        });
    }
    Ok(window)
}

/// Clears `replacing` however the work ends.
struct Replacing(Arc<WindowSlot>);

impl Drop for Replacing {
    fn drop(&mut self) {
        self.0.replacing.store(false, Ordering::Release);
    }
}

fn replace_in_background(slot: &Arc<WindowSlot>) {
    if slot.replacing.swap(true, Ordering::AcqRel) {
        return;
    }
    let replacing = Replacing(slot.clone());
    let Some(waiting) = background_turn() else { return };
    std::thread::spawn(move || {
        let _waiting = waiting;
        replace(replacing);
    });
}

/// A new window takes over only once it's filled, soo a click never finds a worse one than the last.
fn replace(replacing: Replacing) {
    let slot = &replacing.0;
    let _turn = lock(&BACKGROUND);
    if !still_kept(slot) {
        return;
    }
    let Ok(window) = Window::read(&slot.search) else { return };
    window.fill(&slot.search);
    *lock(&slot.window) = Some(Arc::new(window));
}

/// Keeps a window ready for each category's page and for what was clicked lately. Doesn't return.
pub(crate) fn keep_sort_windows_warm(running: impl Fn() -> bool) {
    let plain = Search::from_args(&HashMap::new());
    slot_of(&plain, true);
    for root in TABLE {
        slot_of(&Search { cat: root.id.to_string(), ..plain.clone() }, true);
    }
    loop {
        if running() {
            let slots: Vec<Arc<WindowSlot>> = lock(&WINDOWS).values().cloned().collect();
            for slot in slots {
                let in_use = lock(&slot.used).elapsed() < WINDOW_IN_USE;
                let fresh_for = if in_use { WINDOW_FRESH } else { WINDOW_IDLE_FRESH };
                let current = lock(&slot.window).clone();
                let stale = current.is_none_or(|window| window.built.elapsed() > fresh_for);
                if stale && !slot.replacing.swap(true, Ordering::AcqRel) {
                    replace(Replacing(slot.clone()));
                }
            }
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

/// Each row's HTML, with what it shows beyond itself read for all of them at once.
fn rows_html(found: &[ReleaseRow], split: HashMap<i64, release_info::SplitPost>) -> Vec<String> {
    let ids: Vec<i64> = found.iter().map(|r| r.id).collect();
    let mut all_ids = ids.clone();
    for post in split.values() {
        all_ids.extend_from_slice(&post.ids);
    }
    all_ids.sort_unstable();
    all_ids.dedup();
    let mut categories = crate::category::categories_of(&all_ids).unwrap_or_default();
    let passwords = crate::archive::store::passwords(&ids);
    let mut media = crate::media::query::attrs_of(&all_ids).unwrap_or_default();
    for (&id, post) in &split {
        if categories.get(&id).copied().unwrap_or(UNCATEGORIZED) == UNCATEGORIZED
            && let Some(cat) = post.ids.iter().filter_map(|i| categories.get(i).copied()).find(|c| *c != UNCATEGORIZED)
        {
            categories.insert(id, cat);
        }
        if !media.contains_key(&id)
            && let Some(m) = post.ids.iter().find_map(|i| media.get(i).cloned())
        {
            media.insert(id, m);
        }
    }
    // a post spread over several groups is all its rows together, whatever one of them holds
    let single: Vec<i64> = ids.iter().copied().filter(|id| !split.contains_key(id)).collect();
    let extras = RowExtras {
        recovery: release_info::recovery_only_of(&single),
        split,
        copies: crate::duplicates::counts(&ids).unwrap_or_default(),
        details: crate::media::details::kept_for(media.iter().map(|(id, m)| (*id, m.kind, &m.ids))),
    };
    found.iter().map(|r| row(r, &categories, &passwords, &media, &extras)).collect()
}

/// What a result shows beyond its own row.
#[derive(Default)]
struct RowExtras {
    /// all of the post a row lists: spread over several groups, or linked by its PAR2 set
    split: HashMap<i64, release_info::SplitPost>,
    copies: HashMap<i64, crate::duplicates::CopyCount>,
    details: HashMap<i64, crate::media::details::Details>,
    /// rows with nothing but PAR2 data in them
    recovery: HashMap<i64, release_info::RecoveryOnly>,
}

/// Columns of the results table.
pub(crate) const COLUMNS: usize = 6;

pub(crate) fn tag(class: &str, text: &str) -> String {
    format!(" <span class=\"tag {class}\">{}</span>", escape(text))
}

/// What a row's size is, for its tooltip.
fn size_note(size: Option<i64>, recovery: Option<&release_info::RecoveryOnly>) -> String {
    let bytes = format!("{} bytes", thousands(size.unwrap_or(0)));
    match recovery {
        None => bytes,
        Some(recovery) => match recovery.repairs_bytes() {
            Some(whole) => {
                format!("{bytes} of PAR2 recovery data only. The files it repairs are {}", fmt_size(Some(whole)))
            }
            None => format!("{bytes} of PAR2 recovery data only"),
        },
    }
}

/// Says a release is an upload's rows put back together.
fn linked_tag(rows: usize) -> String {
    let title = format!("{rows} files posted under random names, joined by their PAR2 set");
    format!(" <span class=\"tag tag-ref\" title=\"{}\">LINKED</span>", attr(&title))
}

/// Why a PAR2-only row's download isn't the release it's named after.
fn recovery_warning(recovery: &release_info::RecoveryOnly) -> String {
    let files: Vec<&str> = recovery.repairs.iter().map(|(name, _)| name.as_str()).collect();
    let body = match (recovery.repairs_bytes(), files.as_slice()) {
        (Some(whole), [one]) => format!(
            "Its NZB holds PAR2 recovery data only. The file it repairs, {one} ({}), was posted under another name, and Atlas hasn't linked the two yet.",
            fmt_size(Some(whole))
        ),
        (Some(whole), _) => format!(
            "Its NZB holds PAR2 recovery data only. The {} files it repairs ({}) were posted under other names, and Atlas hasn't linked them yet.",
            files.len(),
            fmt_size(Some(whole))
        ),
        (None, _) => "Its NZB holds PAR2 recovery data only. The files it repairs aren't in this release.".to_string(),
    };
    diag("caution", "this is not the whole release", &body)
}

/// Says a row is only PAR2 data, and what it repairs.
fn recovery_tag(recovery: &release_info::RecoveryOnly) -> String {
    let title = match recovery.repairs_bytes() {
        Some(whole) => format!(
            "Only PAR2 recovery data. The files it repairs ({}) were posted under other names",
            fmt_size(Some(whole))
        ),
        None => "Only PAR2 recovery data".to_string(),
    };
    format!(" <span class=\"tag tag-warn\" title=\"{}\">PAR2 ONLY</span>", attr(&title))
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
    let recovery = extras.recovery.get(&r.id);
    if let Some(recovery) = recovery {
        tags.push_str(&recovery_tag(recovery));
    }
    // a recovery volume is no copy of the others of its set, whatever the set's list makes them look like
    if let Some(count) = extras.copies.get(&r.id).filter(|_| recovery.is_none()) {
        if count.twins > 0 {
            tags.push_str(&tag("tag-ok", &format!("{} TWIN{}", count.twins, if count.twins == 1 { "" } else { "S" })));
        }
        if count.alts > 0 {
            tags.push_str(&tag("tag-ref", &format!("{} ALT{}", count.alts, if count.alts == 1 { "" } else { "S" })));
        }
    }
    if let Some(post) = extras.split.get(&r.id) {
        if post.groups.len() > 1 {
            tags.push_str(&tag("tag-ref", &format!("{} GROUPS", post.groups.len())));
        }
        if post.lead.is_some() {
            tags.push_str(&linked_tag(post.ids.len()));
        }
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
        "<tr><td class=\"name\">{thumb}<a href=\"/ui/release?id={id}\">{name}</a>{tags}<div class=\"row-meta\">{meta}</div></td><td class=\"col-cat dim\">{category}</td><td class=\"num col-size\" title=\"{size_note}\">{size}</td><td class=\"dim col-posted\">{posted}</td><td class=\"dim col-group\"><a class=\"quiet\" href=\"/?group={group_link}\">{group}</a></td><td><a class=\"button small primary\" href=\"/ui/nzb?id={id}\">NZB</a></td></tr>",
        id = r.id,
        name = escape(&r.name),
        group = escape(&r.group_name),
        group_link = attr(&url_encode(&r.group_name)),
        size_note = attr(&size_note(r.size, recovery)),
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

pub(crate) const BUILD_VERSION: &str = env!("ATLAS_BUILD_VERSION");
pub(crate) const GIT_SHA: &str = env!("ATLAS_GIT_SHA");
pub(crate) const GIT_SHORT_SHA: &str = env!("ATLAS_GIT_SHORT_SHA");
pub(crate) const BUILD_YEAR: &str = env!("ATLAS_BUILD_YEAR");
pub(crate) const RUSTC_VERSION: &str = env!("ATLAS_RUSTC_VERSION");
pub(crate) const BUILD_DATE: &str = env!("ATLAS_BUILD_DATE");

fn site_footer() -> String {
    let commit = if GIT_SHA.is_empty() || GIT_SHA == "unknown" {
        format!("<code>{}</code>", escape(GIT_SHORT_SHA))
    } else {
        format!(
            "<a href=\"https://github.com/Appz4Fun/Atlas/commit/{}\" target=\"_blank\" rel=\"noreferrer noopener\"><code>{}</code></a>",
            attr(GIT_SHA),
            escape(GIT_SHORT_SHA),
        )
    };
    format!(
        r#"<footer class="site-footer" role="contentinfo"><div class="site-footer-inner"><span><strong>Atlas - Rust Edition.</strong> Authored by: <a href="https://github.com/Eraxty" target="_blank" rel="noreferrer noopener">Eraxty</a> &amp; Forked and ported to Rust by: <a href="https://github.com/xbmc4lyfe" target="_blank" rel="noreferrer noopener">xbmc4lyfe</a></span><span>Version: <code>{version}</code> · last git commit: {commit}</span><span>&copy; {year} · License: <a href="https://www.gnu.org/licenses/gpl-3.0.txt" target="_blank" rel="noreferrer noopener">GPL v3 (GNU General Public License v3.0)</a></span><span>Rust Version: <code>{rustc}</code> · Compilation Date: <code>{date}</code></span><span>Donate to my Patreon to help with AI token cost and get prioritized features and personal support: <a class="patreon-link" href="https://www.patreon.com/cw/xbmc4lyfe" target="_blank" rel="noreferrer noopener"><svg class="patreon-icon" viewBox="0 0 24 24" aria-hidden="true"><circle cx="14.5" cy="9.5" r="6.5" fill="currentColor"/><rect x="3" y="3" width="3.5" height="18" fill="currentColor"/></svg><span>Patreon · xbmc4lyfe</span></a></span></div></footer>"#,
        version = escape(BUILD_VERSION),
        year = escape(BUILD_YEAR),
        rustc = escape(RUSTC_VERSION),
        date = escape(BUILD_DATE),
    )
}

fn page(title: &str, content: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><meta name=\"color-scheme\" content=\"light dark\"><meta name=\"htmx-config\" content=\"{}\"><title>{}</title><link rel=\"icon\" href=\"/static/atlas.svg?v={v}\" type=\"image/svg+xml\"><link rel=\"stylesheet\" href=\"/static/atlas.css?v={v}\"><script src=\"/static/atlas.js?v={v}\"></script><script src=\"/static/htmx.min.js?v={v}\"></script></head><body>{content}{}</body></html>",
        attr(HTMX_CONFIG),
        escape(title),
        site_footer(),
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
<div class="footer">Newznab API: <code>{base}/api</code></div>
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
    let mut category = crate::category::categories_of(&ids).unwrap_or_default().get(&id).copied();
    let mut inner = crate::archive::store::view(id);
    let mut media = crate::media::query::attrs_of(&ids).unwrap_or_default().remove(&id);
    let mut names = release_info::names(id).unwrap_or_default();
    let mut contents = release_info::contents(id).unwrap_or_default();
    let mut release = release;
    let mut whole_post = false;
    let mut split_groups = None;
    // a post spread over several groups, or linked by its PAR2 set, is shown whole
    if let Ok(conn) = crate::db::open()
        && let Ok(Some(post)) = release_info::whole_post_with(&conn, &release)
    {
        contents = release_info::contents_of_split(&conn, &post).unwrap_or(contents);
        split_groups = release_info::groups_of_ids(&conn, &post.ids).ok();
        if post.lead.is_none() {
            release.name = release_info::post_stem(&release.name);
            if let Some(names) = &mut names {
                names.posted = release_info::post_stem(&names.posted);
            }
        }
        release.size = Some(post.size);
        release.complete = post.complete;
        release.parts = Some(contents.articles_seen);
        whole_post = true;
        if category.unwrap_or(UNCATEGORIZED) == UNCATEGORIZED {
            let cats = crate::category::categories_of(&post.ids).unwrap_or_default();
            if let Some(found) = post.ids.iter().filter_map(|i| cats.get(i).copied()).find(|c| *c != UNCATEGORIZED) {
                category = Some(found);
            }
        }
        if media.is_none() {
            let mut all_media = crate::media::query::attrs_of(&post.ids).unwrap_or_default();
            media = post.ids.iter().find_map(|i| all_media.remove(i));
        }
        // a linked or split upload's file list may live on its recovery or base row
        if inner.files.is_empty()
            && let Some(listed) = post
                .recovery
                .iter()
                .chain(&post.ids)
                .map(|id| crate::archive::store::view(*id))
                .find(|v| !v.files.is_empty())
        {
            inner.files = listed.files;
        }
    } else if !release.complete
        && let Ok(conn) = crate::db::open()
        && release_info::single_post_complete(&conn, release.id).unwrap_or(false)
    {
        release.complete = true;
    }
    let recovery = (!whole_post)
        .then(|| crate::db::open().ok().and_then(|conn| release_info::recovery_only_with(&conn, id).ok().flatten()))
        .flatten();
    html(
        200,
        release_html(
            &release,
            category,
            &inner,
            media.as_ref(),
            names.as_ref(),
            &contents,
            recovery.as_ref(),
            split_groups.as_deref(),
        ),
    )
}

fn release_problem(status: u16, level: &str, title: &str, body: &str) -> ApiResponse {
    let content = format!(
        "{}<div class=\"signin\">{}<p><a href=\"/\">Back to the releases</a></p></div>",
        masthead(true),
        diag(level, title, body)
    );
    html(status, page("Atlas", &content))
}

#[allow(clippy::too_many_arguments)]
fn release_html(
    r: &ReleaseRow,
    category: Option<i64>,
    inner: &crate::archive::store::InnerView,
    media: Option<&MediaAttrs>,
    names: Option<&release_info::Names>,
    contents: &release_info::Contents,
    recovery: Option<&release_info::RecoveryOnly>,
    split_groups: Option<&[release_info::GroupPosting]>,
) -> String {
    let category = category.unwrap_or(UNCATEGORIZED);
    let mut tags = String::new();
    if let Some(text) = media.and_then(media_label) {
        tags.push_str(&tag("tag-ref", &text));
    }
    if let Some(recovery) = recovery {
        tags.push_str(&recovery_tag(recovery));
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
        let repairs = recovery.map_or(String::new(), |recovery| match recovery.repairs_bytes() {
            Some(whole) => format!(
                "<br>PAR2 recovery data only. The files it repairs are {} · {} bytes",
                escape(&fmt_size(Some(whole))),
                thousands(whole)
            ),
            None => "<br>PAR2 recovery data only".to_string(),
        });
        fact(
            "Size",
            format!("{} · <a href=\"/?size={size}\">{} bytes</a>{repairs}", escape(&fmt_size(r.size)), thousands(size)),
        );
    }
    fact("Posted", escape(&fmt_date(r.posted_date.as_deref())));
    let posted_name = names.map_or(r.name.as_str(), |n| n.posted.as_str());
    let groups = split_groups
        .map(<[release_info::GroupPosting]>::to_vec)
        .unwrap_or_else(|| release_info::groups_of(posted_name).unwrap_or_default());
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

    let nzb_files = nzb_files_html(contents);
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
        let heading = if recovery.is_some() { "Files its PAR2 set repairs" } else { "Files inside the archives" };
        format!(
            "<h2>{heading}</h2><div class=\"table-wrap\"><table><thead><tr><th>Name</th><th class=\"num\">Size</th></tr></thead><tbody>{rows}</tbody></table></div>"
        )
    };

    // the probe row is decoded once: the panels below read the same one
    let conn = crate::db::open().ok();
    let stills =
        conn.as_ref().map(|conn| stills_html(r.id, &crate::probe::stills::stills(conn, r.id))).unwrap_or_default();
    let found = conn.as_ref().and_then(|c| crate::probe::read(c, r.id));
    let probe = match (conn.as_ref(), found.as_ref()) {
        (Some(c), Some(found)) => probe_html(
            found,
            crate::probe::opensubtitles::read(c, r.id).as_ref(),
            crate::probe::has_cover(c, r.id).then_some(r.id),
            crate::web_meta::salt_of(Some(c)),
        ),
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
    // the other volumes of its set aren't copies of the release, whatever the set's list makes them look like
    let copies = if recovery.is_some() { String::new() } else { copies_html(&copies) };
    let warning = recovery.map_or(String::new(), recovery_warning);

    page(
        &format!("{} · Atlas", r.name),
        &format!(
            r#"{masthead}<div class="detail"><main>
<p class="path"><a href="/">atlas::releases</a>::<b>{id}</b></p>
<h1>{name}</h1><p>{tags}</p>
<div class="actions"><a class="button primary" href="/ui/nzb?id={id}">Download NZB</a>{fill}<a class="button" href="/?cat={category}">More in this category</a></div>
{warning}<div id="title-details" hx-get="/ui/details?id={id}" hx-trigger="load" hx-swap="outerHTML"></div>
<div class="panel"><dl class="facts">{facts}</dl></div>
{stills}
{copies}
{meta}
{probe}
{nzb_files}
{files}
</main></div>"#,
            masthead = masthead(true),
            id = r.id,
            name = escape(&r.name),
        ),
    )
}

/// The files the NZB lists, with how many of each one's articles were seen.
fn nzb_files_html(contents: &release_info::Contents) -> String {
    if contents.list.is_empty() {
        return String::new();
    }
    let rows: String = contents
        .list
        .iter()
        .map(|f| {
            let state = if f.seen >= f.expected {
                tag("tag-ok", "COMPLETE")
            } else {
                tag("tag-warn", &format!("{} MISSING", thousands(f.expected - f.seen)))
            };
            format!(
                "<tr><td class=\"name\">{}</td><td class=\"num col-size\" title=\"{} bytes\">{}</td><td class=\"num\">{} / {}</td><td class=\"col-state\">{state}</td></tr>",
                escape(&f.name),
                f.bytes,
                escape(&fmt_size(Some(f.bytes))),
                thousands(f.seen),
                thousands(f.expected),
            )
        })
        .collect();
    let more = if (contents.files as usize) > contents.list.len() {
        format!("<p class=\"note\">The first {} of {} files.</p>", contents.list.len(), thousands(contents.files))
    } else {
        String::new()
    };
    format!(
        "<h2>Files in the NZB</h2><div class=\"table-wrap\"><table><thead><tr><th>Name</th><th class=\"num col-size\">Posted size</th><th class=\"num\">Articles</th><th class=\"col-state\">State</th></tr></thead><tbody>{rows}</tbody></table></div>{more}"
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
    cover: Option<i64>,
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
        dd(&mut tracks, "HDR", m.video.first().map_or(String::new(), |v| escape(&hdr_summary(v))));
        if let Some(encoder) = &m.encoder {
            let settings = m.encoder_options.as_deref().map_or(String::new(), |o| {
                let key = key_options(o);
                let key = if key.is_empty() { String::new() } else { format!(" · {}", escape(&key)) };
                format!(
                    "{key}<details class=\"settings\"><summary>All settings</summary><code>{}</code></details>",
                    escape(o)
                )
            });
            dd(&mut tracks, "Encoder", format!("{}{settings}", escape(encoder)));
        }
        if m.editions > 1 {
            dd(&mut tracks, "Editions", format!("{} (often a seamless branching cut)", m.editions));
        }
        dd(
            &mut tracks,
            "SegmentUID",
            m.segment_uid.as_deref().map_or(String::new(), |u| format!("<code>{}</code>", escape(u))),
        );
        dd(
            &mut tracks,
            "Muxed",
            m.muxed_at
                .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                .map_or(String::new(), |t| t.format("%Y-%m-%d %H:%M UTC").to_string()),
        );
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
                if let Some(aspect) = display_aspect(w, h, v.display_width, v.display_height) {
                    parts.push(aspect);
                }
            }
            if let Some(rate) = v.frame_rate {
                parts.push(format!("{rate:.3} fps"));
            }
            if let Some(depth) = v.bit_depth {
                parts.push(format!("{depth}-bit"));
            }
            parts.extend(v.hdr.clone());
            parts.extend(v.primaries.and_then(primaries_name).map(str::to_string));
            if v.hdr.is_none() {
                parts.extend(v.transfer.and_then(transfer_name).map(str::to_string));
            }
            if let Some(profile) = v.dv_profile {
                let level = v.dv_level.filter(|l| *l > 0).map_or(String::new(), |l| format!(" level {l}"));
                let layer = match v.dv_enhancement_layer {
                    Some(true) => ", enhancement layer",
                    _ => "",
                };
                parts.push(format!("Dolby Vision profile {profile}{level}{layer}"));
            }
            parts.extend(v.max_cll.filter(|n| *n > 0).map(|n| format!("MaxCLL {n} nits")));
            parts.extend(v.max_fall.filter(|n| *n > 0).map(|n| format!("MaxFALL {n} nits")));
            parts.extend(v.language.clone().filter(|l| l != "und"));
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
            if a.default {
                parts.push("default".into());
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
            // a name that says so already
            if s.hearing_impaired
                && !s.name.as_deref().is_some_and(|n| {
                    let n = n.to_ascii_lowercase();
                    n.contains("sdh") || n.contains("hearing")
                })
            {
                parts.push("SDH".into());
            }
            if s.default {
                parts.push("default".into());
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

    let attachments = p.media.as_ref().map_or(String::new(), |m| {
        if m.attachments.is_empty() {
            return String::new();
        }
        let rows: String = m
            .attachments
            .iter()
            .map(|a| {
                format!(
                    "<tr><td class=\"name\">{}</td><td class=\"dim\">{}</td><td class=\"num\">{}</td></tr>",
                    escape(&a.name),
                    escape(a.description.as_deref().or(a.mime.as_deref()).unwrap_or("")),
                    fmt_size(Some(a.size as i64))
                )
            })
            .collect();
        format!(
            "<h2>Attachments</h2><div class=\"table-wrap\"><table><thead><tr><th>Name</th><th>Type</th><th class=\"num\">Size</th></tr></thead><tbody>{rows}</tbody></table></div>"
        )
    });
    let cover = cover.map_or(String::new(), |id| {
        format!(
            "<h2>Cover art</h2><div class=\"panel cover-panel\"><a href=\"/ui/cover?id={id}\"><img class=\"cover\" src=\"/ui/cover?id={id}\" alt=\"Cover art attached to the video\" loading=\"lazy\"></a></div>"
        )
    });
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
        "{cover}{}{}{attachments}{}{}{}{nfo}{sfv}",
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
    let file_secs = crate::db::open()
        .ok()
        .and_then(|conn| crate::probe::read(&conn, id))
        .and_then(|p| p.media.and_then(|m| m.duration_secs));
    html(200, details.map(|d| details_html(&d, file_secs)).unwrap_or_default())
}

/// How this file's length compares with the title's, when they differ by more than credits and logos do.
fn runtime_note(minutes: i64, file_secs: Option<f64>) -> String {
    let Some(secs) = file_secs.filter(|s| *s > 60.0) else { return String::new() };
    let file_minutes = (secs / 60.0).round() as i64;
    let diff = file_minutes - minutes;
    if diff.abs() < 4 || minutes <= 0 {
        return String::new();
    }
    let what = if diff > 0 { "longer: maybe an extended cut" } else { "shorter: maybe a cut version, or not the film" };
    format!(" · this file is {file_minutes} min, {} min {what}", diff.abs())
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

fn details_html(d: &crate::media::details::Details, file_secs: Option<f64>) -> String {
    let mut lines = String::new();
    let mut line = |name: &str, value: Option<String>| {
        if let Some(value) = value {
            let _ = write!(lines, "<dt>{name}</dt><dd>{}</dd>", escape(&value));
        }
    };
    line("Released", d.released.clone());
    line("Blu-ray", d.bluray.clone());
    line("Genres", d.genres.clone());
    // a show's runtime is the series' usual one, so a long premiere or a double episode isn't a cut
    let file_secs = file_secs.filter(|_| d.kind == crate::media::title::Kind::Movie as i64);
    line("Runtime", d.runtime.map(|minutes| format!("{minutes} min{}", runtime_note(minutes, file_secs))));
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

/// Stills from the video, in order: each a link to the full size JPEG, with where in the film it's from.
fn stills_html(id: i64, stills: &[(u8, Option<i64>, u32, u32)]) -> String {
    if stills.is_empty() {
        return String::new();
    }
    let figures: String = stills
        .iter()
        .map(|&(position, at_ms, width, height)| {
            let src = format!("/ui/still?id={id}&amp;at={position}");
            let when = at_ms.map_or(format!("{position}%"), |ms| format!("{position}% · {}", clock(ms)));
            format!(
                "<figure><a href=\"{src}\"><img src=\"{src}\" width=\"{width}\" height=\"{height}\" loading=\"lazy\" alt=\"Still at {when}\"></a><figcaption>{when}</figcaption></figure>"
            )
        })
        .collect();
    // the releases showing the same pictures, looked up after the page is up: it reads every shard
    format!(
        "<h2>Stills</h2><div class=\"stills\">{figures}</div><div id=\"same-film\" hx-get=\"/ui/samefilm?id={id}\" hx-trigger=\"load\" hx-swap=\"outerHTML\"></div>"
    )
}

/// Other releases whose stills match this one's: the same film, from another source or encode.
fn same_film_panel(args: &HashMap<String, String>) -> ApiResponse {
    let id = args.get("id").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0);
    let Ok(conn) = crate::db::open() else { return html(200, String::new()) };
    let rows: Vec<ReleaseRow> = crate::probe::stills::same_film(&conn, id)
        .into_iter()
        .filter_map(|other| crate::search::get_release_with(&conn, other).ok().flatten())
        .collect();
    html(200, same_film_html(&rows))
}

fn same_film_html(rows: &[ReleaseRow]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let items: String = rows
        .iter()
        .map(|r| {
            format!(
                "<tr><td><a href=\"/ui/release?id={}\">{}</a></td><td class=\"num\">{}</td></tr>",
                r.id,
                escape(&r.name),
                escape(&fmt_size(r.size))
            )
        })
        .collect();
    format!(
        "<h2>Same film</h2><div class=\"table-wrap\"><table><thead><tr><th>Name</th><th class=\"num\">Size</th></tr></thead><tbody>{items}</tbody></table></div><p class=\"sort\">Releases whose stills show the same pictures: another encode, a remux or a different cut. Not twins.</p>"
    )
}

/// A time into the film as h:mm:ss, or m:ss under an hour.
fn clock(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// One still's JPEG.
fn still(args: &HashMap<String, String>) -> ApiResponse {
    let number = |key: &str| args.get(key).and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0);
    let (id, at) = (number("id"), number("at"));
    let found = u8::try_from(at)
        .ok()
        .and_then(|at| crate::db::open().ok().and_then(|conn| crate::probe::stills::still(&conn, id, at)));
    let Some(bytes) = found else {
        return ApiResponse::not_found();
    };
    ApiResponse {
        status: 200,
        content_type: Some("image/jpeg"),
        body: String::new(),
        headers: vec![
            ("Cache-Control".into(), "private, max-age=86400".into()),
            ("X-Content-Type-Options".into(), "nosniff".into()),
        ],
        bytes: Some(bytes),
        failed: false,
    }
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

/// SDR, HDR10, HLG, PQ, DV, or DV with the layer it falls back to: `DV HDR10`. Empty when nothing says which.
fn hdr_summary(v: &crate::probe::media::VideoTrack) -> String {
    // PQ with no mastering data is HDR10 whose static metadata wasn't carried into the container
    let base = v.hdr.clone().map(|h| if h == "PQ" { "HDR10 (no mastering data)".to_string() } else { h });
    match (v.dv_profile, base) {
        // profile 5 has no fallback, whatever the container says: its colour only DV players get right
        (Some(5), _) => "DV P5".into(),
        (Some(p), Some(base)) => format!("DV P{p} + {base}"),
        (Some(p), None) => format!("DV P{p}"),
        (None, Some(base)) => base,
        // SDR only when the container says what the transfer is: MP4 and many MKVs don't, and that's not SDR
        (None, None) if v.transfer.is_some_and(|t| t != 2) => "SDR".into(),
        (None, None) => String::new(),
    }
}

/// What most decides how an encode looks: rate control and its value, and x265's preset when it's written.
fn key_options(options: &str) -> String {
    let wanted = ["preset", "crf", "bitrate", "qp", "rc", "tune"];
    options
        .split_whitespace()
        .filter(|kv| kv.split_once('=').is_some_and(|(k, _)| wanted.contains(&k)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The shape it's shown in, when that's not the shape it's stored in: anamorphic video.
fn display_aspect(w: u64, h: u64, dw: Option<u64>, dh: Option<u64>) -> Option<String> {
    let (dw, dh) = (dw?, dh.unwrap_or(h));
    if dw == 0 || dh == 0 || dw * h == dh * w {
        return None;
    }
    Some(format!("shown at {:.2}:1", dw as f64 / dh as f64))
}

/// ITU-T H.273 colour primaries, as Matroska stores them.
fn primaries_name(code: u64) -> Option<&'static str> {
    match code {
        1 => Some("BT.709"),
        5 | 6 => Some("BT.601"),
        9 => Some("BT.2020"),
        11 => Some("DCI-P3"),
        12 => Some("Display P3"),
        _ => None,
    }
}

/// The transfer, when it isn't already named as HDR.
fn transfer_name(code: u64) -> Option<&'static str> {
    match code {
        16 => Some("PQ"),
        18 => Some("HLG"),
        _ => None,
    }
}

/// The cover art attached to a release's video.
fn cover(args: &HashMap<String, String>) -> ApiResponse {
    let id = args.get("id").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0);
    let Some((mime, bytes)) = crate::db::open().ok().and_then(|conn| crate::probe::cover(&conn, id)) else {
        return ApiResponse::not_found();
    };
    let content_type = if mime == "image/png" { "image/png" } else { "image/jpeg" };
    ApiResponse {
        status: 200,
        content_type: Some(content_type),
        body: String::new(),
        headers: vec![
            ("Cache-Control".into(), "private, max-age=86400".into()),
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

    fn window_of_rows(rows: Vec<ReleaseRow>) -> Window {
        Window {
            built: Instant::now(),
            rows,
            categories: OnceLock::new(),
            scores: OnceLock::new(),
            total: OnceLock::new(),
            shown: Mutex::new(HashMap::new()),
            brought: Mutex::new(HashSet::new()),
            orders: Mutex::new(HashMap::new()),
            sized: AtomicBool::new(false),
            shown_gen: std::sync::atomic::AtomicU64::new(0),
            filled: AtomicBool::new(false),
        }
    }

    fn shown(html: &str, size: i64, folded: bool) -> ShownRow {
        ShownRow { html: html.into(), size: Some(size), folded }
    }

    #[test]
    fn a_render_that_lost_the_race_for_a_brought_row_doesnt_hide_it() {
        let window = window_of_rows(Vec::new());
        window.publish(vec![(7, shown("<tr>lead</tr>", 10, false))]);
        window.publish(vec![(7, shown("", 1, true))]);
        assert_eq!(lock(&window.shown)[&7].html, "<tr>lead</tr>");
        // the other way round the row still wins
        window.publish(vec![(8, shown("", 1, true))]);
        window.publish(vec![(8, shown("<tr>8</tr>", 2, false))]);
        assert!(!lock(&window.shown)[&8].folded);
    }

    #[test]
    fn a_size_order_from_sizes_not_final_yet_isnt_kept() {
        let row = |id: i64, size: i64| ReleaseRow { id, size: Some(size), ..Default::default() };
        let window = window_of_rows(vec![row(1, 1), row(2, 2)]);
        window.order(SortBy::Size, true);
        assert!(lock(&window.orders).is_empty(), "the fill hasn't rendered every row yet");
        window.order(SortBy::Name, true);
        assert_eq!(lock(&window.orders).len(), 1, "a name order doesn't wait on sizes");
        window.sized.store(true, Ordering::Release);
        window.order(SortBy::Size, true);
        assert!(lock(&window.orders).contains_key(&(SortBy::Size, true)));
        // a row published after the fill, with another size: the size order kept goes, the name order stays
        window.publish(vec![(1, shown("<tr>1</tr>", 50, false))]);
        assert!(!lock(&window.orders).contains_key(&(SortBy::Size, true)));
        assert!(lock(&window.orders).contains_key(&(SortBy::Name, true)));
        assert_eq!(*window.order(SortBy::Size, true), vec![0, 1], "the bigger size comes first now");
    }

    #[test]
    fn background_work_waits_no_more_than_the_windows_kept() {
        let turns: Vec<_> = std::iter::from_fn(background_turn).take(WINDOWS_KEPT + 5).collect();
        assert!(turns.len() <= WINDOWS_KEPT);
        assert!(background_turn().is_none());
        drop(turns);
        assert!(background_turn().is_some(), "a turn given back makes room");
    }

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
    fn stills_link_their_jpeg_and_say_where_they_are() {
        assert_eq!(same_film_html(&[]), "");
        let other = ReleaseRow { id: 9, name: "<A>.Film".into(), size: Some(1 << 30), ..Default::default() };
        let html = same_film_html(&[other]);
        assert!(html.contains("/ui/release?id=9") && html.contains("&lt;A&gt;.Film"), "{html}");
        assert_eq!(stills_html(7, &[]), "");
        let html = stills_html(7, &[(25, Some(1_834_500), 960, 540), (50, None, 960, 540)]);
        assert!(html.contains("<img src=\"/ui/still?id=7&amp;at=25\" width=\"960\" height=\"540\""), "{html}");
        assert!(html.contains("<figcaption>25% · 30:34</figcaption>") && html.contains("<figcaption>50%</figcaption>"));
        assert_eq!(clock(3_725_000), "1:02:05");
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
            assert_eq!(get("/ui/still", "id=1&at=50", bad, "http://x", KEY).unwrap().status, 401, "{bad:?}");
            assert_eq!(get("/ui/samefilm", "id=1", bad, "http://x", KEY).unwrap().status, 401, "{bad:?}");
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
        let html = details_html(&d, None);
        assert!(!html.contains("<b>") && !html.contains("<i>"), "{html}");
        assert!(html.contains("IMDB 87") && !html.contains("Metacritic"), "{html}");
        assert!(html.contains("<dt>Blu-ray</dt><dd>1999-09-21</dd>"), "{html}");
        assert!(html.contains("/ui/poster?kind=2&amp;tmdb=603"), "{html}");
        assert!(!html.contains("art=backdrop"), "{html}");
        let wide = details_html(&crate::media::details::Details { backdrop: Some("/b.jpg".into()), ..d.clone() }, None);
        assert!(wide.contains("art=backdrop"), "{wide}");
        let film = crate::media::details::Details { runtime: Some(136), ..d.clone() };
        assert!(details_html(&film, Some(152.0 * 60.0)).contains("16 min longer"));
        // a show's runtime is the series' usual one: a double episode isn't a cut
        let show = crate::media::details::Details { kind: 1, ..film };
        assert!(!details_html(&show, Some(152.0 * 60.0)).contains("longer"));
    }

    #[test]
    fn a_file_much_longer_or_shorter_than_the_title_says_so() {
        assert_eq!(runtime_note(136, Some(136.0 * 60.0 + 100.0)), "");
        assert_eq!(runtime_note(136, None), "");
        assert!(runtime_note(136, Some(152.0 * 60.0)).contains("16 min longer"));
        assert!(runtime_note(136, Some(44.0 * 60.0)).contains("92 min shorter"));
    }

    #[test]
    fn a_subtitle_named_sdh_doesnt_say_it_twice() {
        use crate::probe::media::{MediaInfo, SubtitleTrack};
        let sub = |name: Option<&str>| SubtitleTrack {
            codec: "SRT".into(),
            name: name.map(str::to_string),
            hearing_impaired: true,
            ..Default::default()
        };
        let media = MediaInfo { subtitles: vec![sub(Some("SDH")), sub(None)], ..Default::default() };
        let html = probe_html(&crate::probe::ProbeInfo { media: Some(media), ..Default::default() }, None, None, None);
        assert_eq!(html.matches("SDH").count(), 2, "{html}");
    }

    #[test]
    fn hdr_is_summed_up_and_the_key_settings_picked() {
        use crate::probe::media::VideoTrack;
        let v = |hdr: Option<&str>, dv: Option<u8>| VideoTrack {
            hdr: hdr.map(str::to_string),
            dv_profile: dv,
            ..VideoTrack::default()
        };
        assert_eq!(hdr_summary(&v(None, None)), "");
        assert_eq!(hdr_summary(&VideoTrack { transfer: Some(1), ..v(None, None) }), "SDR");
        assert_eq!(hdr_summary(&VideoTrack { transfer: Some(2), ..v(None, None) }), "");
        assert_eq!(hdr_summary(&v(Some("HDR10"), None)), "HDR10");
        assert_eq!(hdr_summary(&v(Some("HDR10"), Some(7))), "DV P7 + HDR10");
        assert_eq!(hdr_summary(&v(None, Some(5))), "DV P5");
        assert_eq!(hdr_summary(&v(Some("PQ"), Some(5))), "DV P5");
        assert_eq!(hdr_summary(&v(Some("HLG"), None)), "HLG");
        assert_eq!(hdr_summary(&v(Some("PQ"), Some(8))), "DV P8 + HDR10 (no mastering data)");
        assert_eq!(key_options("cpuid=1 preset=slow crf=18.0 aq-mode=3 rc=crf"), "preset=slow crf=18.0 rc=crf");
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

    #[test]
    fn every_page_has_the_persistent_build_footer() {
        let sign_in = get("/", "", None, "http://127.0.0.1:9090", KEY).unwrap().body;
        let settings = get("/ui/settings", "", Some(&cookie()), "http://127.0.0.1:9090", KEY).unwrap().body;
        for html in [&sign_in, &settings] {
            assert!(html.contains("class=\"site-footer\""), "{html}");
            assert!(html.contains("Atlas - Rust Edition."), "{html}");
            assert!(html.contains("https://github.com/Eraxty") && html.contains(">Eraxty<"), "{html}");
            assert!(html.contains("https://github.com/xbmc4lyfe") && html.contains(">xbmc4lyfe<"), "{html}");
            assert!(html.contains(&format!("Version: <code>{BUILD_VERSION}</code>")), "{html}");
            if !GIT_SHA.is_empty() && GIT_SHA != "unknown" {
                assert!(html.contains(&format!("https://github.com/Appz4Fun/Atlas/commit/{GIT_SHA}")), "{html}");
            }
            assert!(html.contains(&format!("<code>{GIT_SHORT_SHA}</code>")), "{html}");
            assert!(html.contains(&format!("&copy; {BUILD_YEAR}")), "{html}");
            assert!(html.contains("https://www.gnu.org/licenses/gpl-3.0.txt"), "{html}");
            assert!(html.contains(&format!("Rust Version: <code>{RUSTC_VERSION}</code>")), "{html}");
            assert!(html.contains(&format!("Compilation Date: <code>{BUILD_DATE}</code>")), "{html}");
            assert!(html.contains("https://www.patreon.com/cw/xbmc4lyfe"), "{html}");
        }
    }
}
