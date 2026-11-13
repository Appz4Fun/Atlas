//! A small browser ui on the api port: search, browse and download NZBs.
//!
//! The api key never reaches a page. Signing in with it sets a cookie derived from it,
//! and the server does the searching and the NZB downloads itself.

use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::sync::{Arc, LazyLock};

use sha1::{Digest, Sha1};

use crate::api::{ApiResponse, escape, parse_query};
use crate::archive::Password;
use crate::category::{TABLE, UNCATEGORIZED, label, parse_filter};
use crate::field_search::{self, Fields};
use crate::gemini;
use crate::media::query::MediaAttrs;
use crate::media::title::Kind;
use crate::search::ReleaseRow;
use crate::ui::{fmt_date, fmt_size};
use crate::{release_info, release_tags, word_index};

mod fresh;

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

/// Whether the results leave out incomplete releases: `incomplete=hide` or `incomplete=show` in a url, and the last one
/// asked for kept in this cookie, soo it holds across visits until it's turned off again.
const INCOMPLETE_ARG: &str = "incomplete";
/// The least score a search keeps, out of 100: `rated=70` is 7 out of 10 and up.
const RATED_ARG: &str = "rated";
/// The scores the rating chips offer.
const RATED: [i64; 3] = [60, 70, 80];
/// asks a search for frozen releases too
const DEEP_ARG: &str = "deep";
const INCOMPLETE_COOKIE: &str = "atlas_incomplete";
const HIDE: &str = "hide";
const SHOW: &str = "show";
/// how long the choice is kept
const INCOMPLETE_DAYS: i64 = 365;

/// Everything comes from this origin, soo a release name that got past escaping still can't run.
const CSP: &str = "default-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

/// no eval, no requests elsewhere, and no copy of the results in localStorage
const HTMX_CONFIG: &str =
    r#"{"allowEval":false,"selfRequestsOnly":true,"includeIndicatorStyles":false,"historyCacheSize":0}"#;

/// A GET the ui answers. None: not ours, the api decides.
pub fn get(path: &str, query: &str, cookies: Option<&str>, base: &str, api_key: &str) -> Option<ApiResponse> {
    let storage = match crate::config::checked_storage_config() {
        Ok(storage) => storage,
        Err(_) => {
            return Some(html(503, diag("caution", "catalog unavailable", "Storage configuration could not be read.")));
        }
    };
    get_with_storage(path, query, cookies, base, api_key, &storage)
}

pub(crate) fn get_with_storage(
    path: &str,
    query: &str,
    cookies: Option<&str>,
    base: &str,
    api_key: &str,
    storage: &crate::pg::StorageConfig,
) -> Option<ApiResponse> {
    get_with_storage_and_assets(path, query, cookies, base, api_key, storage, None)
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn get_with_storage_and_assets(
    path: &str,
    query: &str,
    cookies: Option<&str>,
    base: &str,
    api_key: &str,
    storage: &crate::pg::StorageConfig,
    assets: Option<&crate::pg::fresh_assets::AssetManifest>,
) -> Option<ApiResponse> {
    let signed_in = signed_in(cookies, api_key);
    let mut args = parse_query(query);
    let remember = incomplete_choice(&mut args, cookies);
    let remembered = |mut resp: ApiResponse| {
        resp.headers.extend(remember.clone());
        resp
    };
    if signed_in && (matches!(path, "/" | "") || path.starts_with("/ui/")) {
        return fresh::get_with_assets(path, &args, base, api_key, storage, assets).map(remembered);
    }
    Some(match path {
        "/static/htmx.min.js" => asset("text/javascript; charset=utf-8", HTMX),
        "/static/atlas.css" => asset("text/css; charset=utf-8", CSS),
        "/static/atlas.js" => asset("text/javascript; charset=utf-8", JS),
        "/static/atlas.svg" => asset("image/svg+xml", MARK),
        "/" | "" => html(200, login_page(base, false)),
        "/ui/results" => ApiResponse { headers: vec![("HX-Redirect".into(), "/".into())], ..html(401, String::new()) },
        "/ui/total" | "/ui/details" | "/ui/pending" | "/ui/poster" | "/ui/cover" | "/ui/still" | "/ui/samefilm"
        | "/ui/image" | "/ui/samepicture" | "/ui/picture" | "/ui/meta" | "/ui/row" | "/ui/stats/live"
        | "/ui/stats/stored" => {
            ApiResponse { headers: vec![("HX-Redirect".into(), "/".into())], ..html(401, String::new()) }
        }
        "/ui/nzb"
        | "/ui/release"
        | "/ui/duplicates"
        | "/ui/stats"
        | "/ui/same"
        | "/ui/settings"
        | "/ui/settings/groups/search" => redirect(Vec::new()),
        _ => return None,
    })
}

/// A POST the ui answers. None: not ours. `htmx`: htmx sent it, and swaps in what comes back. `csrf`: the
/// `X-CSRF-Token` header, which the settings forms take in place of their field.
pub fn post(
    path: &str,
    body: &str,
    cookies: Option<&str>,
    htmx: bool,
    csrf: Option<&str>,
    base: &str,
    api_key: &str,
) -> Option<ApiResponse> {
    let storage = match crate::config::checked_storage_config() {
        Ok(storage) => storage,
        Err(_) => {
            return Some(html(503, diag("caution", "catalog unavailable", "Storage configuration could not be read.")));
        }
    };
    post_with_storage(path, body, cookies, htmx, csrf, base, api_key, &storage)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn post_with_storage(
    path: &str,
    body: &str,
    cookies: Option<&str>,
    htmx: bool,
    csrf: Option<&str>,
    base: &str,
    api_key: &str,
    storage: &crate::pg::StorageConfig,
) -> Option<ApiResponse> {
    let signed_in = signed_in(cookies, api_key);
    if signed_in && path.starts_with("/ui/") {
        return Some(html(
            503,
            diag("caution", "native operation unavailable", "Native mutation admission is awaiting qualification."),
        ));
    }
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
        "/ui/picture" => {
            Some(ApiResponse { headers: vec![("HX-Redirect".into(), "/".into())], ..html(401, String::new()) })
        }
        "/ui/ask" if htmx => {
            Some(ApiResponse { headers: vec![("HX-Redirect".into(), "/".into())], ..html(401, String::new()) })
        }
        "/ui/ask" | "/ui/thaw" => Some(redirect(Vec::new())),
        p if crate::web_settings::takes_post(p) => Some(redirect(Vec::new())),
        _ => None,
    }
}

/// The ui reads a request body only on these paths.
pub fn takes_post(path: &str) -> bool {
    matches!(path, "/login" | "/logout" | "/ui/ask" | "/ui/thaw" | "/ui/picture")
        || crate::web_settings::takes_post(path)
}

/// Whether to hide incomplete releases, put in `args` as `incomplete=hide` when the url doesn't say but the cookie does,
/// and the cookie to set when the url asks for something else than it holds.
fn incomplete_choice(args: &mut HashMap<String, String>, cookies: Option<&str>) -> Option<(String, String)> {
    let saved = cookie_value(cookies, INCOMPLETE_COOKIE) == Some(HIDE);
    let asked = args.get(INCOMPLETE_ARG).map(|v| v.trim().to_string());
    let cookie = |value: &str, days: i64| {
        let max_age = days * 86_400;
        (
            "Set-Cookie".to_string(),
            format!("{INCOMPLETE_COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}"),
        )
    };
    match asked.as_deref() {
        Some(HIDE) => (!saved).then(|| cookie(HIDE, INCOMPLETE_DAYS)),
        Some(SHOW) => saved.then(|| cookie("", 0)),
        _ => {
            if saved {
                args.insert(INCOMPLETE_ARG.into(), HIDE.into());
            }
            None
        }
    }
}

/// A cookie's value, as the browser sent it.
fn cookie_value<'a>(cookies: Option<&'a str>, name: &str) -> Option<&'a str> {
    cookies
        .into_iter()
        .flat_map(|c| c.split(';'))
        .filter_map(|c| c.trim().split_once('='))
        .find(|(n, _)| *n == name)
        .map(|(_, v)| v)
}

fn cookie_header(value: &str, max_age: i64) -> (String, String) {
    ("Set-Cookie".into(), format!("{COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}"))
}

fn redirect(mut headers: Vec<(String, String)>) -> ApiResponse {
    headers.push(("Location".into(), "/".into()));
    ApiResponse {
        status: 303,
        content_type: None,
        body: String::new(),
        headers,
        bytes: None,
        spool: None,
        disposition: None,
        failed: false,
    }
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
        spool: None,
        disposition: None,
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
        spool: None,
        disposition: None,
        failed: false,
    }
}

/// What the cookie holds: stands in for the key, and changes with it.
pub(crate) fn session_token(api_key: &str) -> String {
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

pub(crate) fn same(a: &str, b: &str) -> bool {
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
    /// leaves out what shows as INCOMPLETE: the whole post a row stands for, as the row shows it
    hide_incomplete: bool,
    /// frozen releases too, listed after the live ones (`frozen`)
    deep: bool,
    /// leaves out releases whose best kept score (`score_value`, out of 100) is below this, and those with none kept
    min_score: Option<i64>,
}

/// Up to this many results get sorted, the newest or best matching ones, unless the database orders by the column: then
/// a window starts with this many of the top ones, and pages past it read a bigger one.
const SORT_WINDOW: i64 = 2_000;
/// How far a window grows to fill a page with a rating chip on: in a category where few titles have a score kept, most
/// rows are left out, and each is looked up first.
/// How far a window grows to fill a page when it also filters on tags, which are read from each name.
/// The deepest a ranked window reads: 2,000 pages of Load more. Each shard reads up to this many before they're merged,
/// soo a url asking for more can't make the server hold eight times a whole database at once.
/// Rows kept across every window: past it, the grown windows of searches not used lately go back to their first pages.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SortBy {
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
        // as SQLite's lower(), which only knows ASCII: a sort over a whole category picks its rows that way
        SortBy::Name => flip(a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase())),
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

/// The quick filters over results. Each but the last two keeps the results whose name's tags say so, among the first
/// `SORT_WINDOW`: searching the index for each way a tag is written was three or four full searches a click.
const FILTERS: &[&str] = &["2160p", "1080p", "DV", "HDR", "Remux", "WEB-DL", "BluRay", HIDE_SPOTS, HIDE_COPIES];
const HIDE_COPIES: &str = "Hide copies";
/// Leaves out the rows tagged SPOT: releases saved from a Spotnet spot.
const HIDE_SPOTS: &str = "Hide spots";

/// Whether a filter goes by the tags read from a name, not by what else is known of a row.
fn is_tag_filter(filter: &str) -> bool {
    filter != HIDE_COPIES && filter != HIDE_SPOTS
}

/// How many names have their tags read at once, before checking whether the page is full.

/// Whether a name's tags say what a filter asks. Tags, not words, soo "DV" leaves DVDRip out.
pub(crate) fn filter_matches(filter: &str, tags: &release_tags::Tags) -> bool {
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
            // a pasted NZBLNK searches for its header, the part that names the post
            q: crate::nzblnk::search_text(&text("q")).unwrap_or_else(|| text("q")),
            cat: text("cat"),
            offset: offset - offset % PAGE_SIZE,
            filters,
            fields,
            sort,
            descending,
            hide_incomplete: text(INCOMPLETE_ARG) == HIDE,
            deep: matches!(text(DEEP_ARG).as_str(), "1" | "true" | "yes" | "on"),
            min_score: text(RATED_ARG).parse().ok().filter(|n| RATED.contains(n)),
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
        if self.hide_incomplete {
            more.push(format!("{INCOMPLETE_ARG}={HIDE}"));
        }
        if self.deep {
            more.push(format!("{DEEP_ARG}=1"));
        }
        if let Some(min) = self.min_score {
            more.push(format!("{RATED_ARG}={min}"));
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
            hide_incomplete: self.hide_incomplete,
            deep: self.deep,
            min_score: self.min_score,
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

    /// Whether a filter other than "Hide copies" or "Hide spots" is on: those go by the tags of the results.
    fn filters_by_tags(&self) -> bool {
        self.filters.iter().any(|f| is_tag_filter(f))
    }

    /// Whether rows tagged SPOT are left out. Like incomplete ones, by the row each shows once its post is folded in.
    fn hides_spots(&self) -> bool {
        self.filters.contains(&HIDE_SPOTS)
    }

    /// The tag filters on, as the word index has them.
    fn tag_filters(&self) -> Vec<&'static str> {
        self.filters.iter().copied().filter(|f| is_tag_filter(f)).collect()
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
        out.push_str(&self.incomplete_link());
        out.push_str(&self.rated_links());
        out.push_str(&self.sort_link(SortBy::Rating, "Best rated"));
        out.push_str("</div>");
        out
    }

    /// This search's first page with incomplete releases hidden, or shown again. Showing them says so in the url, soo
    /// the cookie that keeps them hidden goes too.
    fn incomplete_toggled(&self) -> String {
        let toggled = Search { offset: 0, hide_incomplete: !self.hide_incomplete, ..self.clone() };
        let mut query = toggled.query(&self.cat, 0);
        if self.hide_incomplete {
            if !query.is_empty() {
                query.push('&');
            }
            query.push_str(&format!("{INCOMPLETE_ARG}={SHOW}"));
        }
        query
    }

    /// The chip that hides incomplete releases, or shows them again.
    fn incomplete_link(&self) -> String {
        let query = attr(&self.incomplete_toggled());
        let pressed = if self.hide_incomplete { " aria-current=\"true\"" } else { "" };
        format!(
            "<a class=\"chip\" href=\"/?{query}\" hx-get=\"/ui/results?{query}\" hx-target=\"#results\" hx-sync=\"#search:replace\" hx-indicator=\"#busy\" title=\"Leave out releases missing parts. Kept on until you turn it off.\"{pressed}>Hide incomplete</a>"
        )
    }

    /// The chips that keep only releases rated that high, by the best score kept for their title. The one that's on
    /// turns it off again.
    fn rated_links(&self) -> String {
        RATED
            .iter()
            .map(|&min| {
                let on = self.min_score == Some(min);
                let toggled = Search { offset: 0, min_score: (!on).then_some(min), ..self.clone() };
                let query = attr(&toggled.query(&self.cat, 0));
                let pressed = if on { " aria-current=\"true\"" } else { "" };
                format!(
                    "<a class=\"chip\" href=\"/?{query}\" hx-get=\"/ui/results?{query}\" hx-target=\"#results\" hx-sync=\"#search:replace\" hx-indicator=\"#busy\" title=\"Only releases whose title scores {} out of 10 or more on IMDb, else TMDB or Trakt. Unrated ones are left out.\"{pressed}>Rated {}+</a>",
                    min / 10,
                    min / 10
                )
            })
            .collect()
    }

    fn load_more(&self) -> String {
        let next = attr(&self.query(&self.cat, self.offset + PAGE_SIZE));
        format!(
            "<tr><td colspan=\"{COLUMNS}\" class=\"note\"><a class=\"button\" href=\"/?{next}\" hx-get=\"/ui/results?{next}\" hx-target=\"closest tr\" hx-swap=\"outerHTML\">Load more</a></td></tr>"
        )
    }

    /// Whether a row is left out for being incomplete: once `fold_split_post` has made it say what all of its post comes
    /// to, the way it would show.
    fn hides(&self, r: &ReleaseRow) -> bool {
        self.hide_incomplete && !r.complete
    }

    /// Whether a release is left out for its score: below the least asked for, or none kept.
    fn rated_out(&self, id: i64, scores: &HashMap<i64, i64>) -> bool {
        self.scored_out(scores.get(&id).copied())
    }

    /// `rated_out`, for a row whose score is already known.
    fn scored_out(&self, score: Option<i64>) -> bool {
        self.min_score.is_some_and(|min| score.is_none_or(|s| s < min))
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
        if self.filters_by_tags() && self.indexed().is_none() {
            let _ = write!(hint, "The filters look through the first {SORT_WINDOW} results. ");
        }
        if self.hide_incomplete {
            let shown = attr(&self.incomplete_toggled());
            let _ = write!(
                hint,
                "Incomplete releases are hidden. <a href=\"/?{shown}\" hx-get=\"/ui/results?{shown}\" hx-target=\"#results\">Show them</a>. "
            );
        }
        if self.hides_spots() {
            let filters: Vec<&'static str> = self.filters.iter().copied().filter(|f| *f != HIDE_SPOTS).collect();
            let shown = attr(&Search { offset: 0, filters, ..self.clone() }.query(&self.cat, 0));
            let _ = write!(
                hint,
                "Spots are hidden. <a href=\"/?{shown}\" hx-get=\"/ui/results?{shown}\" hx-target=\"#results\">Show them</a>. "
            );
        }
        if self.min_score.is_some() {
            let all = attr(&Search { offset: 0, min_score: None, ..self.clone() }.query(&self.cat, 0));
            let _ = write!(
                hint,
                "Only releases rated that high are shown, by the scores kept so far. <a href=\"/?{all}\" hx-get=\"/ui/results?{all}\" hx-target=\"#results\">Show every rating</a>. "
            );
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
}

/// What a total counts that the rows don't show one by one.
const ROWS_FOLDED: &str =
    "Every release counts once. A post spread over several groups, or an upload linked by its PAR2 set, is one row.";

/// `a`, `a and b`, `a, b and c`.
fn list_of(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// How old a window can get before a click on it starts a new one. The click still gets the old one straight away.
/// What a result shows beyond its own row.
#[cfg(test)]
#[derive(Default)]
struct RowExtras {
    /// all of the post a row lists: spread over several groups, or linked by its PAR2 set
    split: HashMap<i64, release_info::SplitPost>,
    copies: HashMap<i64, crate::duplicates::CopyCount>,
    details: HashMap<i64, crate::media::details::Details>,
    /// rows with nothing but PAR2 data in them
    recovery: HashMap<i64, release_info::RecoveryOnly>,
    /// rows saved from a Spotnet spot
    spots: HashMap<i64, crate::spotnet::store::Tagged>,
    /// rows whole only once repaired from their PAR2 volumes
    repair: HashSet<i64>,
}

/// Columns of the results table.
pub(crate) const COLUMNS: usize = 6;

/// A release whole only once repaired: some of its data is missing, and its PAR2 volumes can rebuild it.
fn repair_tag() -> String {
    " <span class=\"tag tag-warn\" title=\"Some data is missing. Its PAR2 recovery volumes can rebuild it: the download needs a repair, which NZBGet and SABnzbd do on their own.\">NEEDS REPAIR</span>".to_string()
}

pub(crate) fn tag(class: &str, text: &str) -> String {
    format!(" <span class=\"tag {class}\">{}</span>", escape(text))
}

/// What a row's size is, for its tooltip.
fn size_note(size: Option<i64>, recovery: Option<&release_info::RecoveryOnly>) -> String {
    size_note_bytes(size, recovery.map(release_info::RecoveryOnly::repairs_bytes))
}

fn size_note_bytes(size: Option<i64>, recovery: Option<Option<i64>>) -> String {
    let bytes = format!("{} bytes", thousands(size.unwrap_or(0)));
    match recovery {
        None => bytes,
        Some(recovery) => match recovery {
            Some(whole) => {
                format!("{bytes} of PAR2 recovery data only. The files it repairs are {}", fmt_size(Some(whole)))
            }
            None => format!("{bytes} of PAR2 recovery data only"),
        },
    }
}

/// Says a release came from a Spotnet spot, and whose.
fn spot_tag(poster: &str, spot_title: &str) -> String {
    let by = if poster.is_empty() { String::new() } else { format!(" by {poster}") };
    let title = format!("Spotnet spot{by}: {spot_title}");
    format!(" <span class=\"tag tag-ok\" title=\"{}\">SPOT</span>", attr(&title))
}

/// What's been said about a spot: its comments, oldest first, and how many spam reports it has.
#[derive(Clone, Debug, Default)]
struct SpotSaid {
    comments: Vec<crate::spotnet::comments::Comment>,
    reports: i64,
}

/// A release page's section on its spot: everything the spot says, as Spotweb's spot page shows it, then its
/// description and its comments. A spot saved before its XML was kept shows what its row has until the XML is read
/// again.
fn spot_html(d: &crate::spotnet::store::Detail, said: &SpotSaid) -> String {
    use crate::spotnet::names;
    use crate::spotnet::store::Xml;
    let spot = &d.spot;
    let mut facts = String::new();
    let mut fact = |name: &str, value: String| {
        if !value.is_empty() {
            let _ = write!(facts, "<dt>{name}</dt><dd>{value}</dd>");
        }
    };
    fact("Title", escape(&spot.title));
    let subcats = &spot.subcats;
    for (letter, label, list) in [
        ('z', "Type", &subcats.z),
        ('a', "Format", &subcats.a),
        ('b', "Source", &subcats.b),
        ('c', "Language", &subcats.c),
        ('d', "Genre", &subcats.d),
    ] {
        let named: Vec<String> = list
            .iter()
            .map(|&n| names::name(letter, n).map_or_else(|| format!("{letter}{n}"), str::to_string))
            .collect();
        fact(label, escape(&named.join(", ")));
    }
    if spot.created > 0 {
        fact(
            "Posted",
            chrono::DateTime::from_timestamp(spot.created, 0)
                .map_or(String::new(), |t| t.format("%Y-%m-%d %H:%M UTC").to_string()),
        );
    }
    if let Some(size) = i64::try_from(spot.size).ok().filter(|n| *n > 0) {
        fact("Size", format!("{} · {} bytes", escape(&fmt_size(Some(size))), thousands(size)));
    }
    let mut sender = escape(&spot.poster);
    if let Some(id) = d.spotter_id.as_deref().filter(|id| !id.is_empty()) {
        let _ = write!(sender, "{}spotter {}", if sender.is_empty() { "" } else { " · " }, escape(id));
    }
    fact("Sender", sender);
    fact("Tag", escape(&spot.tag));
    let website = spot.website.trim();
    match crate::spotnet::ubb::web_url(website) {
        Some(href) => fact(
            "Website",
            format!("<a href=\"{}\" rel=\"{}\">{}</a>", attr(href), crate::spotnet::ubb::REL, escape(href)),
        ),
        None => fact("Website", escape(website)),
    }
    if let Some(imdb) = d
        .imdb_id
        .as_deref()
        .filter(|id| id.starts_with("tt") && id.len() > 2 && id[2..].bytes().all(|b| b.is_ascii_digit()))
    {
        fact("IMDb", format!("<a href=\"https://www.imdb.com/title/{imdb}/\" rel=\"noreferrer\">{imdb}</a>"));
    }
    if !spot.image.is_empty() {
        let size = spot.image_size.map_or(String::new(), |(w, h)| format!("{w} × {h}, "));
        let n = spot.image.len();
        fact("Cover image", format!("{size}{n} article{}", if n == 1 { "" } else { "s" }));
    }
    fact("Spotted with", escape(&spot.newsreader));
    fact("Message-ID", format!("<code>{}</code>", escape(&d.message_id)));
    if d.xml == Xml::Kept && spot.key > 0 {
        fact("Key", spot.key.to_string());
    }
    let ratings: Vec<u32> = said.comments.iter().map(|c| u32::from(c.rating)).filter(|r| *r > 0).collect();
    if !ratings.is_empty() {
        let mean = f64::from(ratings.iter().sum::<u32>()) / ratings.len() as f64;
        let n = ratings.len();
        fact("Rating", format!("{mean:.1} / 10 from {n} comment{}", if n == 1 { "" } else { "s" }));
    }
    fact("Spam reports", said.reports.to_string());
    let description = if d.xml == Xml::Gone {
        "<p class=\"note\">No description: this spot was saved before Atlas kept it, and its article could no longer be read.</p>".to_string()
    } else if d.xml == Xml::Pending {
        "<p class=\"note\">The description isn't here yet: this spot was saved before Atlas kept it, and the indexer reads it again on its next spot pass.</p>".to_string()
    } else if spot.description.trim().is_empty() {
        "<p class=\"note\">The spot has no description.</p>".to_string()
    } else {
        format!("<div class=\"panel spot-desc\">{}</div>", crate::spotnet::ubb::to_html(&spot.description))
    };
    format!(
        "<h2>Spot</h2>{}<div class=\"panel\"><dl class=\"facts\">{facts}</dl></div><h3>Description</h3>{description}{}",
        d.removed.map_or(String::new(), spot_removed),
        spot_comments_html(&said.comments)
    )
}

/// Says a spot was taken back: the release stays, but the spot no longer vouches for it.
fn spot_removed(removed: crate::spotnet::store::Removed) -> String {
    let when =
        chrono::DateTime::from_timestamp(removed.at, 0).map_or(String::new(), |t| t.format(" on %Y-%m-%d").to_string());
    use crate::spotnet::store::RemovedBy;
    let body = match removed.by {
        RemovedBy::Moderator => format!(
            "A Spotnet moderator removed this spot{when}, which they do for fakes, spam and spots in the wrong category. The release is still here, but the spot no longer vouches for what it is."
        ),
        RemovedBy::Spotter => format!(
            "Its spotter removed this spot{when}, which they do when they repost it or it was wrong. The release is still here, but the spot no longer vouches for what it is."
        ),
        RemovedBy::Blacklist => "Its spotter is on the Spotnet blacklist, which lists spotters known for spam and fakes. The release is still here, but the spot no longer vouches for what it is.".to_string(),
    };
    diag("caution", "this spot was removed", &body)
}

/// A spot's comments, oldest first: who wrote each (and their spotter id), when, the rating it gives, and its text.
fn spot_comments_html(comments: &[crate::spotnet::comments::Comment]) -> String {
    use crate::spotnet::comments::{GONE, READ};
    if comments.is_empty() {
        return "<h3>Comments</h3><p class=\"note\">No comments on this spot yet.</p>".to_string();
    }
    let mut out = format!("<h3>Comments ({})</h3>", comments.len());
    for c in comments {
        let mut by = vec![if c.poster.is_empty() { "?".to_string() } else { escape(&c.poster) }];
        if let Some(id) = c.spotter_id.as_deref().filter(|id| !id.is_empty()) {
            by.push(format!("spotter {}", escape(id)));
        }
        if let Some(t) = chrono::DateTime::from_timestamp(c.at, 0) {
            by.push(t.format("%Y-%m-%d %H:%M UTC").to_string());
        }
        if c.rating > 0 {
            by.push(format!("rated {} / 10", c.rating));
        }
        let text = match c.state {
            READ if c.body.is_empty() => "<p class=\"note\">An empty comment.</p>".to_string(),
            READ => format!("<div class=\"spot-desc\">{}</div>", crate::spotnet::ubb::to_html(&c.body)),
            GONE => "<p class=\"note\">Its text couldn't be read: the article is gone, too big to be a comment, or the servers kept failing.</p>".to_string(),
            _ => "<p class=\"note\">Its text is read on the indexer's next spot pass.</p>".to_string(),
        };
        let _ = write!(out, "<div class=\"panel spot-comment\"><p class=\"dim\">{}</p>{text}</div>", by.join(" · "));
    }
    out
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
    recovery_tag_bytes(recovery.repairs_bytes())
}

fn recovery_tag_bytes(bytes: Option<i64>) -> String {
    let title = match bytes {
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

/// The name a release is shown under, TRaSH's for a movie or an episode, and the name it has when that differs.
fn shown_names(
    r: &ReleaseRow,
    category: i64,
    media: Option<&MediaAttrs>,
    details: Option<&crate::media::details::Details>,
) -> (String, Option<String>) {
    shown_names_known(r, category, &crate::trash_name::Known::of(media, details))
}

fn shown_names_known(r: &ReleaseRow, category: i64, known: &crate::trash_name::Known<'_>) -> (String, Option<String>) {
    match crate::trash_name::name(&r.name, category, known) {
        Some(shown) if shown != r.name => (shown, Some(r.name.clone())),
        _ => (r.name.clone(), None),
    }
}

/// The name a release has, under the one it's shown under.
fn raw_name_html(raw: Option<&str>) -> String {
    raw.map_or(String::new(), |raw| format!("<div class=\"raw-name\">{}</div>", escape(raw)))
}

#[cfg(test)]
fn row(
    r: &ReleaseRow,
    categories: &HashMap<i64, i64>,
    passwords: &HashMap<i64, Password>,
    media: &HashMap<i64, MediaAttrs>,
    extras: &RowExtras,
) -> String {
    let details = extras.details.get(&r.id);
    let post = extras.split.get(&r.id);
    row_from_facts(
        r,
        ResultFacts {
            category: categories.get(&r.id).copied().unwrap_or(UNCATEGORIZED),
            password: passwords.get(&r.id),
            media: media.get(&r.id),
            details: details.map(ResultDetails::from),
            poster: details
                .filter(|details| crate::media::details::poster_kept(details))
                .map(|details| (details.kind, details.tmdb_id)),
            recovery: extras.recovery.get(&r.id).map(release_info::RecoveryOnly::repairs_bytes),
            copies: extras.copies.get(&r.id),
            spot: extras.spots.get(&r.id).map(|spot| SpotBadge { poster: &spot.poster, title: &spot.title }),
            whole: post.map(|post| WholePostBadge {
                groups: post.groups.len(),
                linked_rows: post.lead.map(|_| post.ids.len()),
            }),
            repair: extras.repair.contains(&r.id),
        },
    )
}

/// Presentation-only whole-post facts. Native callers consume the safe
/// purpose-qualified derivation; constructing an unsafe legacy SplitPost is
/// neither necessary nor sufficient to render a badge.
#[derive(Clone, Copy)]
pub(crate) struct WholePostBadge {
    pub groups: usize,
    pub linked_rows: Option<usize>,
}

#[derive(Clone, Copy)]
pub(crate) struct SpotBadge<'a> {
    pub poster: &'a str,
    pub title: &'a str,
}

#[derive(Clone, Copy)]
pub(crate) struct ResultDetails<'a> {
    pub title: Option<&'a str>,
    pub released: Option<&'a str>,
    pub imdb_score: Option<i64>,
    pub tmdb_score: Option<i64>,
    pub trakt_score: Option<i64>,
}
impl<'a> From<&'a crate::media::details::Details> for ResultDetails<'a> {
    fn from(details: &'a crate::media::details::Details) -> Self {
        Self {
            title: details.title.as_deref(),
            released: details.released.as_deref(),
            imdb_score: details.imdb_score,
            tmdb_score: details.tmdb_score,
            trakt_score: details.trakt_score,
        }
    }
}

/// All enrichment and kept-artwork decisions precede rendering. None of the
/// optional values means "try a legacy/provider lookup"; a native loader must
/// obtain covered present/absent facts in its authenticated snapshot.
pub(crate) struct ResultFacts<'a> {
    pub category: i64,
    pub password: Option<&'a Password>,
    pub media: Option<&'a MediaAttrs>,
    pub details: Option<ResultDetails<'a>>,
    pub poster: Option<(i64, i64)>,
    pub recovery: Option<Option<i64>>,
    pub copies: Option<&'a crate::duplicates::CopyCount>,
    pub spot: Option<SpotBadge<'a>>,
    pub whole: Option<WholePostBadge>,
    pub repair: bool,
}

pub(crate) fn row_from_facts(r: &ReleaseRow, facts: ResultFacts<'_>) -> String {
    let category_id = facts.category;
    let category = escape(&label(category_id).unwrap_or_default());
    let mut known = crate::trash_name::Known::of(facts.media, None);
    known.title = facts.details.and_then(|details| details.title);
    known.released = facts.details.and_then(|details| details.released);
    let (shown, raw) = shown_names_known(r, category_id, &known);
    let size = fmt_size(r.size);
    let posted = escape(&fmt_date(r.posted_date.as_deref()));
    let mut tags = String::new();
    if let Some(text) = facts.media.and_then(media_label) {
        tags.push_str(&tag("tag-ref", &text));
    }
    tags.push_str(&name_tags(&release_tags::parse(&r.name)));
    let details = facts.details;
    let score = details.and_then(|details| {
        details
            .imdb_score
            .map(|score| format!("IMDB {score}"))
            .or_else(|| details.tmdb_score.map(|score| format!("TMDB {score}")))
            .or_else(|| details.trakt_score.map(|score| format!("TRAKT {score}")))
    });
    if let Some(score) = score {
        tags.push_str(&tag("tag-ref", &score));
    }
    let thumb = facts.poster.map_or(String::new(), |(kind, tmdb_id)| {
        format!(
            "<img class=\"thumb\" src=\"/ui/poster?kind={}&amp;tmdb={}\" alt=\"\" loading=\"lazy\" width=\"32\" height=\"48\">",
            kind, tmdb_id
        )
    });
    let recovery = facts.recovery;
    if let Some(recovery) = recovery {
        tags.push_str(&recovery_tag_bytes(recovery));
    }
    // a recovery volume is no copy of the others of its set, whatever the set's list makes them look like
    if let Some(count) = facts.copies.filter(|_| recovery.is_none()) {
        if count.xposts > 0 {
            let plural = if count.xposts == 1 { "" } else { "S" };
            tags.push_str(&tag("tag-ref", &format!("{} XPOST{plural}", count.xposts)));
        }
        if count.twins > 0 {
            tags.push_str(&tag("tag-ok", &format!("{} TWIN{}", count.twins, if count.twins == 1 { "" } else { "S" })));
        }
        if count.alts > 0 {
            tags.push_str(&tag("tag-ref", &format!("{} ALT{}", count.alts, if count.alts == 1 { "" } else { "S" })));
        }
    }
    if let Some(spot) = facts.spot {
        tags.push_str(&spot_tag(&spot.poster, &spot.title));
    }
    if let Some(post) = facts.whole {
        if post.groups > 1 {
            tags.push_str(&tag("tag-ref", &format!("{} GROUPS", post.groups)));
        }
        if let Some(rows) = post.linked_rows {
            tags.push_str(&linked_tag(rows));
        }
    }
    tags.push_str(&password_tag(facts.password));
    if !r.complete {
        tags.push_str(&tag("tag-warn", "INCOMPLETE"));
    } else if facts.repair {
        tags.push_str(&repair_tag());
    }
    tags.push_str(&crate::web_meta::drawer_trigger(r.id));
    let meta = [category.as_str(), size.as_str(), posted.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    format!(
        "<tr><td class=\"name\">{thumb}<a href=\"/ui/release?id={id}\">{name}</a>{tags}{raw}<div class=\"row-meta\">{meta}</div></td><td class=\"col-cat dim\">{category}</td><td class=\"num col-size\" title=\"{size_note}\">{size}</td><td class=\"dim col-posted\">{posted}</td><td class=\"dim col-group\"><a class=\"quiet\" href=\"/?group={group_link}\">{group}</a></td><td><a class=\"button small primary\" href=\"/ui/nzb?id={id}\">NZB</a></td></tr>",
        id = r.id,
        name = escape(&shown),
        raw = raw_name_html(raw.as_deref()),
        group = escape(&r.group_name),
        group_link = attr(&url_encode(&r.group_name)),
        size_note = attr(&size_note_bytes(r.size, recovery)),
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
pub(crate) fn diag(level: &str, title: &str, body: &str) -> String {
    format!(
        "<div class=\"diag diag-{level}\" role=\"alert\"><p class=\"diag-head\"><span class=\"diag-level\">{level}:</span> {}</p><div class=\"diag-body\"><p>{}</p></div></div>",
        escape(title),
        escape(body),
    )
}

/// How many frozen releases a deep search lists, after the live ones.

/// A count from the rows themselves.
fn exact_html(n: usize) -> String {
    format!(
        "<strong class=\"total\" title=\"{}\">{}</strong>",
        attr(&format!("Counted from the rows themselves. {ROWS_FOLDED}")),
        escape(&field_search::Total { count: n as i64, exact: true }.describe())
    )
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
    scope_inputs_for(search, oob, true)
}

fn scope_inputs_for(search: &Search, oob: &str, include_frozen: bool) -> String {
    let open = if search.fields.any() || search.deep { " open" } else { "" };
    let field = |name: &str, label: &str, value: &str| {
        format!(
            "<label>{label}<input class=\"scope-field\" name=\"{name}\" value=\"{}\" autocomplete=\"off\" spellcheck=\"false\"></label>",
            attr(value)
        )
    };
    let hidden = |name: &str, value: &str| format!("<input type=\"hidden\" name=\"{name}\" value=\"{}\">", attr(value));
    let deep = if include_frozen {
        format!(
            "<label class=\"deep-field\"><input class=\"scope-field\" type=\"checkbox\" name=\"{DEEP_ARG}\" value=\"1\"{}> Deep search: frozen releases too</label>",
            if search.deep { " checked" } else { "" }
        )
    } else {
        String::new()
    };
    format!(
        "<details id=\"scope\" class=\"more-fields\"{open}{oob}><summary>More fields</summary><div class=\"field-grid\">{}{}{}{}{}{}{}</div>{}{}{}{}{}{}</details>",
        field("group", "Usenet group", &search.fields.group),
        field("poster", "Poster", &search.fields.poster),
        field("email", "Email", &search.fields.email),
        field("size", "Size, in bytes or as 1.5 GB", &search.fields.size),
        field("team", "Release group", &search.fields.team),
        field("crc", "CRC32", &search.fields.crc),
        field("agent", "Posted with", &search.fields.agent),
        deep,
        hidden("filters", &search.filters.join(",")),
        hidden(INCOMPLETE_ARG, if search.hide_incomplete { HIDE } else { "" }),
        hidden(RATED_ARG, &search.min_score.map_or(String::new(), |m| m.to_string())),
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

pub(crate) fn page(title: &str, content: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><meta name=\"color-scheme\" content=\"light dark\"><meta name=\"htmx-config\" content=\"{}\"><title>{}</title><link rel=\"icon\" href=\"/static/atlas.svg?v={v}\" type=\"image/svg+xml\"><link rel=\"stylesheet\" href=\"/static/atlas.css?v={v}\"><script src=\"/static/atlas.js?v={v}\"></script><script src=\"/static/htmx.min.js?v={v}\"></script></head><body>{content}{}</body></html>",
        attr(HTMX_CONFIG),
        escape(title),
        site_footer(),
        v = *ASSET_VERSION,
    )
}

pub(crate) fn masthead(signed_in: bool) -> String {
    let tools = if signed_in {
        "<a class=\"button\" href=\"/ui/stats\">Stats</a><a class=\"button\" href=\"/ui/duplicates\">Duplicates</a><a class=\"button\" href=\"/ui/settings\">Settings</a><form method=\"post\" action=\"/logout\"><button class=\"button\">Sign out</button></form>"
    } else {
        ""
    };
    format!(
        r#"<header class="masthead"><a class="brand" href="/" aria-label="Atlas home">{}<span class="brand-name">At<span>las</span></span></a><span class="brand-tag">newznab indexer</span><div class="masthead-tools"><button class="button icon-button" type="button" data-theme-toggle aria-label="Switch between light and dark theme"><svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="6.5" fill="none" stroke="currentColor" stroke-width="1.5"/><path d="M8 1.5a6.5 6.5 0 0 1 0 13Z" fill="currentColor"/></svg></button>{tools}</div></header>"#,
        MARK,
    )
}

fn search_page_from_results(search: &Search, base: &str, results: &str, native: bool) -> String {
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
            scope = scope_inputs_for(search, "", !native),
            results = results,
            base = escape(base),
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

/// Own/qualified display facts only. Loading and trust decisions precede this
/// renderer; it cannot obtain missing metadata, copies, or provider data.
struct ReleaseHeaderFacts<'a> {
    category: i64,
    details: Option<&'a crate::media::details::Details>,
    spot: Option<SpotBadge<'a>>,
    spot_removed: bool,
    repair: bool,
    groups: &'a [release_info::GroupPosting],
    password: &'a str,
}
#[allow(clippy::too_many_arguments)]
fn release_header_from_facts(
    r: &ReleaseRow,
    inner: &crate::archive::store::InnerView,
    media: Option<&MediaAttrs>,
    names: Option<&release_info::Names>,
    contents: &release_info::Contents,
    recovery: Option<&release_info::RecoveryOnly>,
    loaded: ReleaseHeaderFacts<'_>,
) -> (String, Option<String>, String, String) {
    let category = loaded.category;
    let (shown, raw) = shown_names(r, category, media, loaded.details);
    let mut tags = String::new();
    if let Some(text) = media.and_then(media_label) {
        tags.push_str(&tag("tag-ref", &text));
    }
    if let Some(recovery) = recovery {
        tags.push_str(&recovery_tag(recovery));
    }
    tags.push_str(&password_tag(inner.password.as_ref()));
    if loaded.spot_removed {
        tags.push_str(&tag("tag-warn", "SPOT REMOVED"));
    } else if let Some(spot) = loaded.spot {
        tags.push_str(&spot_tag(spot.poster, spot.title));
    }
    let repair = loaded.repair;
    tags.push_str(&if r.complete || repair { tag("tag-ok", "COMPLETE") } else { tag("tag-warn", "INCOMPLETE") });
    if repair {
        tags.push_str(&repair_tag());
    }
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
    let groups = loaded.groups;
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
    fact("Password", loaded.password.to_owned());
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

    (shown, raw, tags, facts)
}

struct ReleaseSections {
    fill: String,
    warning: String,
    spot: String,
    nfo: String,
    images: String,
    stills: String,
    copies: String,
    meta: String,
    probe: String,
    nzb_files: String,
    files: String,
}
fn release_page_from_facts(
    r: &ReleaseRow,
    shown: &str,
    raw: Option<&str>,
    tags: &str,
    facts: &str,
    category: i64,
    sections: ReleaseSections,
) -> String {
    let ReleaseSections { fill, warning, spot, nfo, images, stills, copies, meta, probe, nzb_files, files } = sections;
    page(
        &format!("{shown} · Atlas"),
        &format!(
            r#"{masthead}<div class="detail"><main>
<p class="path"><a href="/">atlas::releases</a>::<b>{id}</b></p>
<h1>{name}</h1>{raw}<p>{tags}</p>
<div class="actions"><a class="button primary" href="/ui/nzb?id={id}">Download NZB</a>{fill}<a class="button" href="/?cat={category}">More in this category</a></div>
{warning}<div id="title-details" hx-get="/ui/details?id={id}" hx-trigger="load" hx-swap="outerHTML"></div>
<div class="panel"><dl class="facts">{facts}</dl></div>
<div id="pending" hx-get="/ui/pending?id={id}" hx-trigger="load" hx-swap="outerHTML"></div>
{spot}
{nfo}
{images}
{stills}
{copies}
{meta}
{probe}
{nzb_files}
{files}
</main></div>"#,
            masthead = masthead(true),
            id = r.id,
            name = escape(&shown),
            raw = raw_name_html(raw),
        ),
    )
}

fn inner_files_html(inner: &crate::archive::store::InnerView, repairs: bool) -> String {
    let (heading, listed) = inner_files_listed(inner, repairs);
    if listed.is_empty() {
        String::new()
    } else {
        let rows: String = listed
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
            "<h2>{heading}</h2><div class=\"table-wrap\"><table><thead><tr><th>Name</th><th class=\"num\">Size</th></tr></thead><tbody>{rows}</tbody></table></div>"
        )
    }
}

/// The release page's list of the files a release holds, and its heading. An archive's own headers say what is
/// inside it: when they were read, they are the list. A PAR2 set lists the posted files, which are the archive
/// volumes when the release is an archive set: shown as what the set protects, not as what is inside, or a set's
/// first volume reads as an archive inside the archive. A posted NZB lists another post's files.
fn inner_files_listed(
    inner: &crate::archive::store::InnerView,
    repairs: bool,
) -> (&'static str, Vec<&crate::archive::store::InnerFileRow>) {
    use crate::archive::store::{SOURCE_NZB, SOURCE_PAR2};
    if repairs {
        return ("Files its PAR2 set repairs", inner.files.iter().collect());
    }
    let read: Vec<_> = inner.files.iter().filter(|f| f.source != SOURCE_PAR2 && f.source != SOURCE_NZB).collect();
    if !read.is_empty() {
        // what the PAR2 set names besides archives (an NFO, a sample) is posted beside them: still shown
        let beside = inner.files.iter().filter(|f| {
            f.source == SOURCE_PAR2 && !crate::duplicates::lists_archives(std::iter::once(f.name.as_str()))
        });
        return ("Files inside the archives", read.into_iter().chain(beside).collect());
    }
    let par2: Vec<_> = inner.files.iter().filter(|f| f.source == SOURCE_PAR2).collect();
    if !par2.is_empty() {
        let heading = if crate::duplicates::lists_archives(par2.iter().map(|f| f.name.as_str())) {
            "Archive volumes its PAR2 set protects"
        } else {
            "Files its PAR2 set protects"
        };
        return (heading, par2);
    }
    ("Files its posted NZB points to", inner.files.iter().collect())
}

/// The files the NZB lists, with how many of each one's articles were seen. With more than one, each name links an
/// NZB of that file alone, picked by its name (`t=get&name=`), since the list is sorted by name and not in the NZB's
/// order that `file=N` counts in.
fn nzb_files_html(
    id: i64,
    contents: &release_info::Contents,
    pictures: &HashMap<String, PictureState>,
    csrf: &str,
) -> String {
    nzb_files_html_with(id, contents, pictures, csrf, picture_state_html)
}
fn nzb_files_html_with(
    id: i64,
    contents: &release_info::Contents,
    pictures: &HashMap<String, PictureState>,
    csrf: &str,
    render_picture: fn(i64, &str, &PictureState, &str) -> String,
) -> String {
    if contents.list.is_empty() {
        return String::new();
    }
    let one_by_one = contents.files > 1;
    let picture_column = !pictures.is_empty();
    let rows: String = contents
        .list
        .iter()
        .map(|f| {
            let state = if f.seen >= f.expected {
                tag("tag-ok", "COMPLETE")
            } else {
                tag("tag-warn", &format!("{} MISSING", thousands(f.expected - f.seen)))
            };
            // a file with no name is many <file>s in the NZB, none of which a name picks
            let name = if one_by_one && !f.name.is_empty() {
                format!(
                    "<a href=\"/ui/nzb?id={id}&amp;name={}\" title=\"An NZB of this file alone\">{}</a>",
                    attr(&url_encode(&f.name)),
                    escape(&f.name)
                )
            } else {
                escape(&f.name)
            };
            let picture = if picture_column {
                let cell = pictures.get(&f.name).map(|p| render_picture(id, &f.name, p, csrf)).unwrap_or_default();
                format!("<td class=\"col-picture\">{cell}</td>")
            } else {
                String::new()
            };
            format!(
                "<tr><td class=\"name\">{name}</td><td class=\"num col-size\" title=\"{} bytes\">{}</td><td class=\"num\">{} / {}</td><td class=\"col-state\">{state}</td>{picture}</tr>",
                f.bytes,
                escape(&fmt_size(Some(f.bytes))),
                thousands(f.seen),
                thousands(f.expected),
            )
        })
        .collect();
    let picture_head = if picture_column { "<th class=\"col-picture\">Picture</th>" } else { "" };
    let hint = if one_by_one { "<p class=\"note dim\">Click a name for an NZB of that file alone.</p>" } else { "" };
    let more = if (contents.files as usize) > contents.list.len() {
        format!("<p class=\"note\">The first {} of {} files.</p>", contents.list.len(), thousands(contents.files))
    } else {
        String::new()
    };
    format!(
        "<h2>Files in the NZB</h2>{hint}<div class=\"table-wrap\"><table><thead><tr><th>Name</th><th class=\"num col-size\">Posted size</th><th class=\"num\">Articles</th><th class=\"col-state\">State</th>{picture_head}</tr></thead><tbody>{rows}</tbody></table></div>{more}"
    )
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
            if m.container == "bluray" { "Blu-ray disc ID" } else { "DVD disc ID" },
            m.disc_id.as_deref().map_or(String::new(), |id| format!("<code>{}</code>", escape(id))),
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
                parts.extend(dv_rpu_facts(v));
            }
            parts.extend(v.max_cll.filter(|n| *n > 0).map(|n| format!("MaxCLL {n} nits")));
            parts.extend(v.max_fall.filter(|n| *n > 0).map(|n| format!("MaxFALL {n} nits")));
            parts.extend(v.language.clone().filter(|l| l != "und"));
            parts.extend(stats_facts(v.stats.as_ref(), "frames"));
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
            if a.dtsx && !a.name.as_deref().is_some_and(names_dtsx) {
                parts.push("DTS:X".into());
            }
            if a.default {
                parts.push("default".into());
            }
            // a name that says so already
            if a.commentary && !a.name.as_deref().is_some_and(|n| n.to_ascii_lowercase().contains("commentary")) {
                parts.push("commentary".into());
            }
            parts.extend(a.name.clone());
            parts.extend(stats_facts(a.stats.as_ref(), "frames"));
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
            parts.extend(stats_facts(s.stats.as_ref(), "events"));
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

    let mut tags = String::new();
    if let Some(m) = &p.media {
        let mut shown: Vec<(&String, &String)> = m.tags.iter().filter(|(n, v)| tag_shown(n, v)).collect();
        shown.sort_by_key(|(n, _)| n.to_ascii_uppercase());
        for (name, value) in shown {
            dd(&mut tags, &escape(name), tag_value(name, value));
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
        "{cover}{}{}{}{attachments}{}{}{}{sfv}",
        panel("Tracks", &tracks),
        panel("Chapters", &chapters),
        panel("Tags", &tags),
        panel("yEnc", &yenc),
        panel("Packing", &packed),
        panel("How it was posted", &posted),
    )
}

/// A track's counts from mkvmerge's statistics tags: its exact bitrate, frames (a subtitle track's events) and bytes.
fn stats_facts(stats: Option<&crate::probe::media::TrackStats>, frames: &str) -> Vec<String> {
    let Some(s) = stats else { return Vec::new() };
    let mut out = Vec::new();
    out.extend(s.bps.map(|bps| match bps {
        0..1000 => format!("{bps} b/s"),
        _ => format!("{} kb/s", thousands((bps / 1000) as i64)),
    }));
    out.extend(s.frames.map(|n| format!("{} {frames}", thousands(n as i64))));
    out.extend(s.bytes.map(|n| format!("{} bytes", thousands(n as i64))));
    out
}

/// Matroska tags that can name the person who made or bought the file: kept in the probe, never shown.
const PRIVATE_TAGS: [&str; 4] = ["ENCODED_BY", "PURCHASE_OWNER", "PURCHASE_INFO", "PURCHASE_ITEM"];

/// Whether a Matroska tag goes on the page: not one that names a person, and no value with an email address in it.
fn tag_shown(name: &str, value: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    !PRIVATE_TAGS.contains(&upper.as_str()) && !upper.contains("EMAIL") && !value.contains('@')
}

/// The most of a tag's value shown: a long one is cut.
const TAG_VALUE_CHARS: usize = 300;

/// A tag's value as the page shows it: an IMDb or TMDB ID links to its title there, the rest is text.
fn tag_value(name: &str, value: &str) -> String {
    let value = value.trim();
    let text = |v: &str| {
        let cut: String = v.chars().take(TAG_VALUE_CHARS).collect();
        if cut.len() < v.len() { format!("{}…", escape(&cut)) } else { escape(&cut) }
    };
    match name.to_ascii_uppercase().as_str() {
        "IMDB" if value.len() > 2 && value.starts_with("tt") && value[2..].bytes().all(|b| b.is_ascii_digit()) => {
            format!("<a href=\"https://www.imdb.com/title/{v}/\" rel=\"noreferrer\">{v}</a>", v = attr(value))
        }
        "TMDB"
            if value.split_once('/').is_some_and(|(kind, id)| {
                matches!(kind, "movie" | "tv") && !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())
            }) =>
        {
            format!("<a href=\"https://www.themoviedb.org/{v}\" rel=\"noreferrer\">{v}</a>", v = attr(value))
        }
        _ => text(value),
    }
}

/// Copies read in full on one page: each reads its articles.
/// Of one cluster, the most read in full. The rest are links.

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

/// What the kinds of copy on a release's page are.

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

/// Stills from the video, in order: each a link to the full size JPEG, with where in the film it's from. `id` is the
/// release they were made for; `from` says so when that's another release with the same video file, and by which key.
fn stills_html(id: i64, stills: &[(u8, Option<i64>, u32, u32)], from: Option<(i64, crate::keys::Kind)>) -> String {
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
    // the releases showing the same pictures, looked up after the page is up: it reads every shard. For stills shown
    // from another release, that's the other release's list: the pictures are its
    let note = from
        .map(|(other, kind)| {
            let why = match kind {
                crate::keys::Kind::Mux => "the same Matroska mux (SegmentUID and date)",
                _ => "the same OpenSubtitles hash",
            };
            format!(
                "<p class=\"sort\">The same video file as <a href=\"/ui/release?id={other}\">release {other}</a> ({why}): its stills are shown here.</p>"
            )
        })
        .unwrap_or_default();
    format!(
        "<h2>Stills</h2>{note}<div class=\"stills\">{figures}</div><div id=\"same-film\" hx-get=\"/ui/samefilm?id={id}\" hx-trigger=\"load\" hx-swap=\"outerHTML\"></div>"
    )
}

/// The NFO panel: its text as posted, and the IMDb titles it names.
fn nfo_html(text: &str, imdb_ids: &[String]) -> String {
    let imdb: Vec<String> = imdb_ids
        .iter()
        .map(|id| format!("<a href=\"https://www.imdb.com/title/{id}/\" rel=\"noreferrer\">{id}</a>", id = attr(id)))
        .collect();
    let links = if imdb.is_empty() { String::new() } else { format!("<p>{}</p>", imdb.join(" · ")) };
    format!("<section id=\"nfo\"><h2>NFO</h2>{links}<pre class=\"nfo\">{}</pre></section>", escape(text))
}

/// The pictures posted with a release, each with what its headers say: `pictures` are (the row each is kept for, the
/// picture), and `id` is the release shown.
fn images_html(id: i64, pictures: &[(i64, crate::images::step::Image)]) -> String {
    if pictures.is_empty() {
        return String::new();
    }
    let figures: String = pictures
        .iter()
        .map(|(row, i)| {
            // the picture's hash in the address: a rerun that puts another picture at this place isn't served from cache
            let src = format!("/ui/image?id={row}&amp;at={}&amp;v={}", i.position, &i.sha256[..i.sha256.len().min(12)]);
            let facts = image_facts(i);
            format!(
                "<figure class=\"picture\"><a href=\"{src}\"><img src=\"{src}\" width=\"{}\" height=\"{}\" loading=\"lazy\" alt=\"{}\"></a><figcaption><b>{}</b><dl class=\"facts\">{facts}</dl></figcaption></figure>",
                i.width,
                i.height,
                attr(&i.file),
                escape(&i.file)
            )
        })
        .collect();
    format!(
        "<section id=\"pictures\"><h2>Pictures</h2><div class=\"pictures\">{figures}</div><div id=\"same-picture\" hx-get=\"/ui/samepicture?id={id}\" hx-trigger=\"load\" hx-swap=\"outerHTML\"></div></section>"
    )
}

/// Where one of a release's pictures stands, for its line in the files list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PictureState {
    /// in the Pictures panel
    Shown,
    /// asked for and not read yet: when it's read, in words
    Queued(String),
    /// asked for and read, but it isn't a picture that could be shown (`images::step::Answer`)
    Failed(String),
    /// not read: a Fetch button
    Offer,
}

/// Rows of a post looked through for its pictures: the one holding a picture asked for, the pictures shown, and
/// the requests their cells show. One limit for all three, soo a picture that can be asked for shows how it went.

/// One picture's cell in the files list: shown, queued with when it's read, failed, or a Fetch button. A queued one
/// asks again every 30 seconds.
fn picture_state_html(id: i64, name: &str, state: &PictureState, csrf: &str) -> String {
    let query = format!("id={id}&amp;name={}", attr(&url_encode(name)));
    let button = |label: &str| {
        let vals = serde_json::json!({"id": id.to_string(), "name": name}).to_string();
        let headers = serde_json::json!({"X-CSRF-Token": csrf}).to_string();
        format!(
            "<button class=\"button small\" hx-post=\"/ui/picture\" hx-vals=\"{}\" hx-headers=\"{}\" hx-target=\"closest .picture-state\" hx-swap=\"outerHTML\" title=\"Read this picture at the release's next turn and show it with what its headers say\">{label}</button>",
            attr(&vals),
            attr(&headers)
        )
    };
    match state {
        PictureState::Shown => {
            "<div class=\"picture-state\"><a href=\"#pictures\">".to_string() + &tag("tag-ok", "SHOWN") + "</a></div>"
        }
        PictureState::Queued(when) => format!(
            "<div class=\"picture-state\" hx-get=\"/ui/picture?{query}\" hx-trigger=\"every 30s\" hx-swap=\"outerHTML\">{} <span class=\"dim\">ready {}</span></div>",
            tag("tag-warn", "QUEUED"),
            escape(when)
        ),
        PictureState::Failed(why) => format!(
            "<div class=\"picture-state\">{} <span class=\"dim\">{}</span> {}</div>",
            tag("tag-bad", "NOT SHOWN"),
            escape(why),
            button("Try again")
        ),
        PictureState::Offer => format!("<div class=\"picture-state\">{}</div>", button("Fetch")),
    }
}

/// A picture's facts, as `<dt>`/`<dd>` pairs: its size and kind, then what a JPEG's headers add.
fn image_facts(i: &crate::images::step::Image) -> String {
    let mut facts: Vec<(&str, String)> = vec![
        ("Size", format!("{} × {} · {}", i.width, i.height, fmt_size(Some(i.bytes)))),
        ("Format", i.format.to_ascii_uppercase()),
    ];
    if let Some(j) = &i.jpeg {
        let mut coding = j.process.clone();
        if let Some(sampling) = &j.sampling {
            coding.push_str(&format!(" · {sampling}"));
        }
        if j.precision > 0 {
            coding.push_str(&format!(" · {} bit", j.precision));
        }
        facts.push(("Coding", coding));
        if j.components > 0 {
            facts.push(("Components", j.components.to_string()));
        }
        if let Some(transform) = j.adobe_transform {
            let value = match transform {
                0 => "0 (none)".to_string(),
                1 => "1 (YCbCr)".to_string(),
                2 => "2 (YCCK)".to_string(),
                _ => transform.to_string(),
            };
            facts.push(("Adobe color transform", value));
        }
        if let Some(q) = j.quality {
            facts.push(("Quality", if j.quality_exact { format!("{q}") } else { format!("about {q}") }));
        }
        if let Some(hash) = j.tables_hash {
            facts.push(("Quantization table hash", format!("{hash:016x}")));
        }
        if let Some(v) = &j.jfif {
            facts.push(("JFIF", v.clone()));
        }
        if let Some((x, y)) = j.dpi {
            facts.push(("Density", if x == y { format!("{x} dpi") } else { format!("{x} × {y} dpi") }));
        }
        if let Some(icc) = &j.icc {
            facts.push(("Colour profile", icc.clone()));
        }
        let x = &j.exif;
        let camera = match (&x.make, &x.model) {
            (Some(make), Some(model)) if model.starts_with(make.as_str()) => Some(model.clone()),
            (Some(make), Some(model)) => Some(format!("{make} {model}")),
            (make, model) => make.clone().or_else(|| model.clone()),
        };
        let text = [
            ("Camera", camera),
            ("Camera serial", x.serial.clone()),
            ("Lens", x.lens.clone()),
            ("Taken", x.taken.clone()),
            ("Software", x.software.clone()),
            ("XMP creator tool", j.xmp.creator_tool.clone()),
            ("Artist", x.artist.clone()),
            ("Copyright", x.copyright.clone()),
            ("Description", x.description.clone()),
            ("Comment", j.comment.clone()),
            ("Image id", x.unique_id.clone()),
            ("XMP document ID", j.xmp.document_id.clone()),
            ("XMP original document ID", j.xmp.original_document_id.clone()),
            ("XMP instance ID", j.xmp.instance_id.clone()),
        ];
        facts.extend(text.into_iter().filter_map(|(k, v)| v.map(|v| (k, v))));
        if let Some(orientation) = x.orientation {
            facts.push(("Orientation", orientation.to_string()));
        }
        if let Some((lat, lon)) = x.gps {
            facts.push(("Location", format!("{lat:.5}, {lon:.5}")));
        }
        if let Some(n) = x.thumbnail_bytes {
            facts.push(("Preview inside", fmt_size(Some(i64::from(n)))));
        }
        if j.after_end > 0 {
            facts.push(("After its end", format!("{} bytes", thousands(j.after_end as i64))));
        }
    }
    facts.iter().map(|(k, v)| format!("<dt>{k}</dt><dd>{}</dd>", escape(v))).collect()
}

fn same_picture_html(rows: &[(ReleaseRow, crate::images::step::Tie)]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let items: String = rows
        .iter()
        .map(|(r, tie)| {
            format!(
                "<tr><td><a href=\"/ui/release?id={}\">{}</a></td><td>{}</td><td class=\"num\">{}</td><td>{}</td></tr>",
                r.id,
                escape(&r.name),
                escape(&r.group_name),
                escape(&fmt_size(r.size)),
                tie.label()
            )
        })
        .collect();
    format!(
        "<h2>The same picture elsewhere</h2><div class=\"table-wrap\"><table><thead><tr><th>Name</th><th>Group</th><th class=\"num\">Size</th><th>Tied by</th></tr></thead><tbody>{items}</tbody></table></div><p class=\"sort\">Releases posting one of these pictures: the same upload in another group, or a post that reused its thumbnail.</p>"
    )
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

fn same_key_page_from_facts(
    release: &ReleaseRow,
    kind: crate::keys::Kind,
    has: bool,
    rows: &[ReleaseRow],
) -> ApiResponse {
    let id = release.id;
    let body = if !has {
        diag("note", "this release has no such key", "Its probe, PAR2 set or article timing doesn't give one.")
    } else if rows.is_empty() {
        diag("note", "no other release shares it", "Releases indexed later that share it will be listed here.")
    } else {
        let items: String = rows
            .iter()
            .map(|r| {
                let incomplete = if r.complete { String::new() } else { format!(" {}", tag("tag-warn", "INCOMPLETE")) };
                format!(
                    "<tr><td class=\"name\"><a href=\"/ui/release?id={}\">{}</a>{incomplete}</td><td class=\"num\">{}</td><td class=\"dim col-posted\">{}</td><td class=\"dim col-group\">{}</td></tr>",
                    r.id,
                    escape(&r.name),
                    escape(&fmt_size(r.size)),
                    escape(&fmt_date(r.posted_date.as_deref())),
                    escape(&r.group_name)
                )
            })
            .collect();
        let capped = if rows.len() as i64 >= crate::keys::LIST_CAP {
            format!("<p class=\"sort\">The newest {} are listed.</p>", crate::keys::LIST_CAP)
        } else {
            String::new()
        };
        format!(
            "<div class=\"table-wrap\"><table><thead><tr><th>Name</th><th class=\"num\">Size</th><th class=\"col-posted\">Posted</th><th class=\"col-group\">Group</th></tr></thead><tbody>{items}</tbody></table></div>{capped}"
        )
    };
    let content = format!(
        "{masthead}<div class=\"release\"><main><h1>{heading}</h1><p class=\"sort\">Other releases that share this key with <a href=\"/ui/release?id={id}\">{name}</a>. {about}</p>{body}</main></div>",
        masthead = masthead(true),
        heading = escape(kind.heading()),
        name = escape(&release.name),
        about = escape(kind.about()),
    );
    html(200, self::page(&format!("{} · Atlas", kind.heading()), &content))
}

/// A time into the film as h:mm:ss, or m:ss under an hour.
fn clock(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// SDR, HDR10, HLG, PQ, DV, or DV with the layer it falls back to: `DV HDR10`. Empty when nothing says which.
fn hdr_summary(v: &crate::probe::media::VideoTrack) -> String {
    // PQ with no mastering data is HDR10 whose static metadata wasn't carried into the container
    let base = v.hdr.clone().map(|h| if h == "PQ" { "HDR10 (no mastering data)".to_string() } else { h });
    match (v.dv_profile, base) {
        // profile 5 has no fallback, whatever the container says: its colour only DV players get right
        (Some(5), _) => "DV P5".into(),
        (Some(p), Some(base)) => format!("DV P{p}{} + {base}", dv_layer(v)),
        (Some(p), None) => format!("DV P{p}{}", dv_layer(v)),
        (None, Some(base)) => base,
        // SDR only when the container says what the transfer is: MP4 and many MKVs don't, and that's not SDR
        (None, None) if v.transfer.is_some_and(|t| t != 2) => "SDR".into(),
        (None, None) => String::new(),
    }
}

/// ` FEL` or ` MEL` for profile 7 when its RPU was read, which is what decides whether a remux keeps the full
/// enhancement layer. Empty otherwise.
fn dv_layer(v: &crate::probe::media::VideoTrack) -> &'static str {
    match &v.dolby_vision {
        Some(d) if d.from_rpu && d.profile == 7 && d.el_present => {
            if d.fel {
                " FEL"
            } else {
                " MEL"
            }
        }
        _ => "",
    }
}

/// What only a read RPU says: FEL or MEL, and the content mapping version. Nothing when it wasn't read.
fn dv_rpu_facts(v: &crate::probe::media::VideoTrack) -> Vec<String> {
    let Some(d) = v.dolby_vision.as_ref().filter(|d| d.from_rpu) else { return Vec::new() };
    let mut out = Vec::new();
    if let Some(layer) = dv_layer(v).strip_prefix(' ') {
        out.push(layer.to_string());
    }
    if !d.cm_version.is_empty() {
        out.push(format!("CM {}", d.cm_version));
    }
    out
}

/// Whether a track's name already says DTS:X.
fn names_dtsx(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("dts:x") || lower.contains("dts-x") || lower.contains("dtsx")
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

fn password_fact(inner: &crate::archive::store::InnerView, hint: &release_info::PasswordHint) -> String {
    let verdict = match inner.password_check.and_then(|check| check.accepts(&hint.password)) {
        Some(true) if inner.password_check.is_some_and(|check| !check.certain()) => tag("tag-ok", "PROBABLY WORKS"),
        Some(true) => tag("tag-ok", "WORKS"),
        Some(false) => tag("tag-bad", "WRONG"),
        None => ", not tried".to_string(),
    };
    format!("<code>{}</code> found in {}{verdict}", escape(&hint.password), escape(hint.source))
}

/// A password found on a posting of the same bytes: where it was found, and no verdict, as it was never tried here.
fn inherited_password_fact(from: &crate::duplicates::Inherited) -> String {
    format!(
        "<code>{}</code> inherited from <a href=\"/ui/release?id={}\">{}</a>, the same files, found there in {}",
        escape(&from.hint.password),
        from.from,
        escape(&from.from_name),
        escape(from.hint.source)
    )
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
            "<p class=\"ask-off\">Search in plain words with Gemini: <a href=\"/ui/settings#gemini\">add a key in Settings</a>.</p>{note}"
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

/// Each layer an archive was followed down through, outermost first, with the file each one holds where it names
/// it: `RAR5 m0 › movie.7z › 7z copy › movie.mkv`. A layer that stopped still names the file it couldn't open.
fn nested_chain(layers: &[crate::probe::nest::Layer]) -> String {
    let mut parts = Vec::new();
    for l in layers {
        let method = l.method.as_deref().map(|m| format!(" {m}")).unwrap_or_default();
        let stop = l.stop.as_deref().map(|why| format!(" {}", tag("tag-bad", &why.to_uppercase()))).unwrap_or_default();
        parts.push(format!("{}{}{stop}", escape(&l.format), escape(&method)));
        if let Some(name) = l.name.as_deref() {
            parts.push(escape(name));
        }
    }
    parts.join(" › ")
}

/// A 7z keeps its names in its end header, soo a probe of its front can't name the file it found at byte 32. The
/// listing read from that end header can: its first file with any bytes is the one the packed streams open with.
/// Only when the release has one 7z set and the probe read it, as the listing and the probe each pick their own
/// archive; and not when the listing found encryption, which the front can't see: "not encrypted" would be wrong.
fn seven_z_first_file(
    mut p: crate::probe::ProbeInfo,
    inner: &crate::archive::store::InnerView,
    contents: &release_info::Contents,
) -> crate::probe::ProbeInfo {
    use crate::archive::names::strip_volume_suffix;
    use crate::archive::select::{Kind, classify};
    if inner.encryption.is_some() || inner.names_hidden {
        return p;
    }
    let set_of =
        |name: &str| matches!(classify(name), Kind::SevenZip(_)).then(|| strip_volume_suffix(name).to_lowercase());
    let Some(probed) = p.file.as_deref().and_then(set_of) else { return p };
    let all_listed = usize::try_from(contents.files).is_ok_and(|n| n <= contents.list.len());
    if !all_listed || contents.list.iter().filter_map(|f| set_of(&f.name)).any(|set| set != probed) {
        return p;
    }
    let first =
        inner.files.iter().find(|f| f.source == crate::archive::store::SOURCE_7Z && f.size.is_some_and(|s| s > 0));
    let Some(first) = first else { return p };
    // a volume set inside is kept as one row, its volumes' sizes summed: not the size of the file at byte 32
    let one_file = strip_volume_suffix(&first.name) == first.name;
    let size = first.size.and_then(|s| u64::try_from(s).ok()).filter(|_| one_file);
    let method = p.layers.first().filter(|l| l.format == "7z").and_then(|l| l.method.clone());
    if let Some(a) = p.archive.as_mut().filter(|a| a.format == "7z" && a.name.is_none()) {
        a.name = Some(first.name.clone());
        a.unpacked = a.unpacked.or(size);
        a.method = a.method.take().or(method);
    }
    if let Some(l) = p.layers.first_mut().filter(|l| l.format == "7z" && l.name.is_none()) {
        l.name = Some(first.name.clone());
        l.unpacked = l.unpacked.or(size);
    }
    p
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_spot_shows_as_a_tag() {
        let t = super::spot_tag("NUKE", "Movie <2026> & co");
        assert!(t.contains(">SPOT</span>") && t.contains("Spotnet spot by NUKE: Movie &lt;2026&gt; &amp; co"), "{t}");
    }

    #[test]
    fn each_file_of_a_release_links_an_nzb_of_it_alone() {
        let file = |n: &str| crate::release_info::NzbFile { name: n.into(), bytes: 1, seen: 1, expected: 1 };
        let mut contents = crate::release_info::Contents {
            files: 3,
            list: vec![file("a b&c.mkv"), file("a.par2"), file("")],
            ..Default::default()
        };
        let html = super::nzb_files_html(9, &contents, &HashMap::new(), "");
        assert!(html.contains("<a href=\"/ui/nzb?id=9&amp;name=a%20b%26c.mkv\""), "{html}");
        assert!(html.contains(">a b&amp;c.mkv</a>"), "{html}");
        assert_eq!(html.matches("<a href").count(), 2, "no link for a file with no name: {html}");
        contents.files = 1;
        contents.list.truncate(1);
        assert!(
            !super::nzb_files_html(9, &contents, &HashMap::new(), "").contains("<a href"),
            "one file: the whole NZB is that file"
        );
    }

    #[test]
    fn a_spot_section_shows_everything_the_spot_says() {
        let xml = r#"<Spotnet><Posting><Key>7</Key><Created>1657355034</Created><Poster>NUKE &lt;x&gt;</Poster><Tag>nuke-tag</Tag><Title>Movie &lt;2026&gt; &amp; co</Title><Description>[b]Great[/b] film[br]Password: s3cr&lt;et[br]More at https://example.org/page?a=1&amp;b=2. Bye</Description><Website>https://www.imdb.com/title/tt39162904/</Website><Image Width="477" Height="643"><Segment>img@spot.net</Segment></Image><Size>77594442517</Size><Category>01<Sub>01a15</Sub><Sub>01b11</Sub><Sub>01c04</Sub><Sub>01d05</Sub><Sub>01d20</Sub><Sub>01z00</Sub></Category><NZB><Segment>n@spot.net</Segment></NZB></Posting><Extra><Newsreader>Spotnet 1.9.0.8</Newsreader></Extra></Spotnet>"#;
        let spot = crate::spotnet::full::parse(xml).unwrap();
        let d = crate::spotnet::store::Detail {
            message_id: "m<1>@spot.net".into(),
            spotter_id: Some("lQhLVQ".into()),
            imdb_id: spot.imdb_id(),
            spot,
            xml: crate::spotnet::store::Xml::Kept,
            removed: None,
        };
        use crate::spotnet::comments::{Comment, GONE, READ, WAITING};
        let said = super::SpotSaid {
            comments: vec![
                Comment {
                    at: 1_787_086_436,
                    rating: 8,
                    poster: "Olli <W>".into(),
                    spotter_id: Some("ab1".into()),
                    state: READ,
                    body: "Thanks![img=klappen]\nWachtwoord: s3cret <b> https://example.org/x_(1)[img=buigen]".into(),
                },
                Comment { at: 1_787_089_272, rating: 6, poster: "Penny".into(), state: GONE, ..Default::default() },
                Comment { at: 1_787_091_828, poster: "Vector".into(), state: WAITING, ..Default::default() },
            ],
            reports: 2,
        };
        let h = super::spot_html(&d, &said);
        for want in [
            "<dt>Title</dt><dd>Movie &lt;2026&gt; &amp; co</dd>",
            "<dt>Type</dt><dd>Movie</dd>",
            "<dt>Format</dt><dd>UHD</dd>",
            "<dt>Source</dt><dd>WEB-DL</dd>",
            "<dt>Language</dt><dd>English subtitles (builtin)</dd>",
            "<dt>Genre</dt><dd>Crime, Thriller</dd>",
            "<dt>Posted</dt><dd>2022-07-09 08:23 UTC</dd>",
            "<dt>Size</dt><dd>72.3 GB · 77,594,442,517 bytes</dd>",
            "<dt>Sender</dt><dd>NUKE &lt;x&gt; · spotter lQhLVQ</dd>",
            "<dt>Tag</dt><dd>nuke-tag</dd>",
            "<dt>Website</dt><dd><a href=\"https://www.imdb.com/title/tt39162904/\"",
            "<dt>IMDb</dt><dd><a href=\"https://www.imdb.com/title/tt39162904/\"",
            "<dt>Cover image</dt><dd>477 × 643, 1 article</dd>",
            "<dt>Spotted with</dt><dd>Spotnet 1.9.0.8</dd>",
            "<dt>Message-ID</dt><dd><code>m&lt;1&gt;@spot.net</code></dd>",
            "<dt>Key</dt><dd>7</dd>",
            "<dt>Rating</dt><dd>7.0 / 10 from 2 comments</dd>",
            "<dt>Spam reports</dt><dd>2</dd>",
            "<h3>Comments (3)</h3>",
            "<p class=\"dim\">Olli &lt;W&gt; · spotter ab1 · 2026-08-18 20:53 UTC · rated 8 / 10</p>",
            // a password a commenter wrote, shown as written
            "Thanks!:klappen:\nWachtwoord: s3cret &lt;b&gt; <a href=\"https://example.org/x_(1)\" rel=\"noopener noreferrer nofollow\">https://example.org/x_(1)</a>:buigen:",
            "<p class=\"dim\">Penny · 2026-08-18 21:41 UTC · rated 6 / 10</p><p class=\"note\">Its text couldn't be read",
            "<p class=\"dim\">Vector · 2026-08-18 22:23 UTC</p><p class=\"note\">Its text is read on the indexer's next",
            // the description whole, escaped, a password the poster wrote included, its link made a link
            "<b>Great</b> film\nPassword: s3cr&lt;et\nMore at <a href=\"https://example.org/page?a=1&amp;b=2\" rel=\"noopener noreferrer nofollow\">https://example.org/page?a=1&amp;b=2</a>. Bye",
        ] {
            assert!(h.contains(want), "missing {want:?} in {h}");
        }
        assert!(!h.contains("[b]"), "{h}");
        // a website that isn't a web link is shown, not linked
        let mut odd = d.clone();
        odd.spot.website = "javascript:alert(1)".into();
        let h = super::spot_html(&odd, &said);
        assert!(h.contains("<dt>Website</dt><dd>javascript:alert(1)</dd>"), "{h}");
        // saved before the XML was kept: what the row has, and a note on the description
        let bare = crate::spotnet::store::Detail {
            message_id: "m@spot.net".into(),
            spot: crate::spotnet::full::FullSpot { title: "T".into(), ..Default::default() },
            ..Default::default()
        };
        let h = super::spot_html(&bare, &super::SpotSaid::default());
        assert!(h.contains("<dt>Title</dt><dd>T</dd>") && h.contains("saved before Atlas kept it"), "{h}");
        assert!(h.contains("<dt>Spam reports</dt><dd>0</dd>") && h.contains("No comments on this spot yet"), "{h}");
        assert!(!h.contains("<dt>Rating</dt>"), "{h}");
        assert!(!h.contains("<dt>Key</dt>") && !h.contains("<dt>Sender</dt>"), "{h}");
        let removed = crate::spotnet::store::Detail {
            removed: Some(crate::spotnet::store::Removed {
                at: 1_793_000_000,
                by: crate::spotnet::store::RemovedBy::Moderator,
            }),
            ..bare.clone()
        };
        let h = super::spot_html(&removed, &super::SpotSaid::default());
        assert!(h.contains("A Spotnet moderator removed this spot on 2026-10-26"), "{h}");
        let own = crate::spotnet::store::Detail {
            removed: Some(crate::spotnet::store::Removed {
                at: 1_793_000_000,
                by: crate::spotnet::store::RemovedBy::Spotter,
            }),
            ..bare.clone()
        };
        assert!(super::spot_html(&own, &super::SpotSaid::default()).contains("Its spotter removed this spot"));
        let listed = crate::spotnet::store::Detail {
            removed: Some(crate::spotnet::store::Removed { at: 0, by: crate::spotnet::store::RemovedBy::Blacklist }),
            ..bare.clone()
        };
        let h = super::spot_html(&listed, &super::SpotSaid::default());
        assert!(h.contains("Its spotter is on the Spotnet blacklist") && !h.contains("1970"), "{h}");
        assert!(!super::spot_html(&bare, &super::SpotSaid::default()).contains("removed"));
        let gone = crate::spotnet::store::Detail { xml: crate::spotnet::store::Xml::Gone, ..bare };
        assert!(super::spot_html(&gone, &super::SpotSaid::default()).contains("could no longer be read"));
    }

    #[test]
    fn a_nested_chain_names_each_file_it_holds() {
        use crate::probe::nest::Layer;
        let layers = vec![
            Layer { format: "RAR5".into(), method: Some("m0".into()), name: Some("x.7z".into()), ..Default::default() },
            Layer {
                format: "7z".into(),
                method: Some("copy".into()),
                name: Some("a.zip".into()),
                ..Default::default()
            },
            Layer {
                format: "ZIP".into(),
                name: Some("movie.mkv".into()),
                stop: Some("encrypted".into()),
                ..Default::default()
            },
        ];
        let chain = super::nested_chain(&layers);
        assert!(chain.starts_with("RAR5 m0 › x.7z › 7z copy › a.zip › ZIP "), "{chain}");
        assert!(chain.ends_with(" › movie.mkv"), "a stopped layer still names its file: {chain}");
    }

    /// A PAR2 set's list of an archive set is its volumes (stored as the first one with the whole set's size): not
    /// what is inside it, which only the archive's headers say, and next to those it isnt shown at all.
    #[test]
    fn a_par2_list_of_volumes_isnt_called_the_inside() {
        use crate::archive::store::{InnerFileRow, InnerView, SOURCE_NZB, SOURCE_PAR2, SOURCE_RAR};
        let row = |seq, name: &str, source| InnerFileRow { seq, name: name.into(), size: Some(9), source };
        let view = |files| InnerView { files, ..Default::default() };
        fn names((h, rows): (&'static str, Vec<&InnerFileRow>)) -> (&'static str, Vec<String>) {
            (h, rows.iter().map(|r| r.name.clone()).collect())
        }

        let volumes = view(vec![row(0, "Film.part001.rar", SOURCE_PAR2), row(1, "Film.nfo", SOURCE_PAR2)]);
        assert_eq!(
            names(super::inner_files_listed(&volumes, false)),
            ("Archive volumes its PAR2 set protects", vec!["Film.part001.rar".into(), "Film.nfo".into()])
        );
        let both = view(vec![
            row(0, "Film.part001.rar", SOURCE_PAR2),
            row(1, "Film.nfo", SOURCE_PAR2),
            row(2, "Film/BDMV/index.bdmv", SOURCE_RAR),
        ]);
        assert_eq!(
            names(super::inner_files_listed(&both, false)),
            ("Files inside the archives", vec!["Film/BDMV/index.bdmv".into(), "Film.nfo".into()])
        );
        let plain = view(vec![row(0, "Film.mkv", SOURCE_PAR2)]);
        assert_eq!(super::inner_files_listed(&plain, false).0, "Files its PAR2 set protects");
        assert_eq!(super::inner_files_listed(&plain, true).0, "Files its PAR2 set repairs");
        let nzb = view(vec![row(0, "Other.mkv", SOURCE_NZB)]);
        assert_eq!(super::inner_files_listed(&nzb, false).0, "Files its posted NZB points to");
        assert!(super::inner_files_listed(&view(vec![]), false).1.is_empty());
    }

    #[test]
    fn a_7z_probe_takes_its_first_file_from_the_listing() {
        use crate::archive::store::{InnerFileRow, InnerView, SOURCE_7Z, SOURCE_PAR2};
        use crate::probe::archive::ArchiveHead;
        use crate::probe::nest::Layer;
        let probe = crate::probe::ProbeInfo {
            file: Some("Show.S01E04.7z.001".into()),
            archive: Some(ArchiveHead { format: "7z".into(), ..Default::default() }),
            layers: vec![Layer { format: "7z".into(), method: Some("copy".into()), ..Default::default() }],
            ..Default::default()
        };
        let row = |seq, name: &str, size, source| InnerFileRow { seq, name: name.into(), size, source };
        let inner = InnerView {
            files: vec![
                row(0, "Show.S01E04.7z.026", Some(9), SOURCE_PAR2),
                row(1, "Show", Some(0), SOURCE_7Z),
                row(2, "Show/Show.S01E04.mkv", Some(8_000_000_000), SOURCE_7Z),
            ],
            ..Default::default()
        };
        let listed = |names: &[&str]| crate::release_info::Contents {
            files: names.len() as i64,
            list: names
                .iter()
                .map(|n| crate::release_info::NzbFile { name: (*n).into(), bytes: 1, seen: 1, expected: 1 })
                .collect(),
            ..Default::default()
        };
        let one_set = listed(&["Show.S01E04.7z.001", "Show.S01E04.7z.002", "Show.S01E04.par2"]);
        let named = super::seven_z_first_file(probe.clone(), &inner, &one_set);
        assert_eq!(named.archive.as_ref().unwrap().name.as_deref(), Some("Show/Show.S01E04.mkv"));
        assert_eq!(named.archive.as_ref().unwrap().unpacked, Some(8_000_000_000));
        assert_eq!(named.archive.as_ref().unwrap().method.as_deref(), Some("copy"), "as the front found it");
        assert_eq!(named.layers[0].name.as_deref(), Some("Show/Show.S01E04.mkv"));

        let encrypted = InnerView { encryption: Some(crate::archive::Encryption::SevenZipAes256), ..inner.clone() };
        let left = super::seven_z_first_file(probe.clone(), &encrypted, &one_set);
        assert_eq!(
            left.archive.unwrap().name,
            None,
            "the front can't tell it is encrypted: no name, no 'not encrypted'"
        );

        let two_sets = listed(&["Show.S01E04.7z.001", "Extras.7z"]);
        let left = super::seven_z_first_file(probe.clone(), &inner, &two_sets);
        assert_eq!(left.archive.unwrap().name, None, "the listing may be of the other set");

        let rar_set = InnerView {
            files: vec![row(1, "Show/movie.part01.rar", Some(9_000_000_000), SOURCE_7Z)],
            ..Default::default()
        };
        let named = super::seven_z_first_file(probe, &rar_set, &one_set);
        let a = named.archive.unwrap();
        assert_eq!(a.name.as_deref(), Some("Show/movie.part01.rar"));
        assert_eq!(a.unpacked, None, "a volume row's size is the whole set's");
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
        let wrong = post("/login", "key=nope", None, false, None, "http://x", KEY).unwrap();
        assert_eq!(wrong.status, 401);
        assert!(header(&wrong, "Set-Cookie").is_none());
        assert!(wrong.body.contains("not the API key"));

        let right = post("/login", &format!("key={KEY}"), None, false, None, "http://x", KEY).unwrap();
        assert_eq!(right.status, 303);
        assert_eq!(header(&right, "Location"), Some("/"));
        let set = header(&right, "Set-Cookie").unwrap();
        assert!(set.contains("HttpOnly") && set.contains("SameSite=Strict"), "{set}");
        assert!(!set.contains(KEY), "{set}");

        let out = post("/logout", "", None, false, None, "http://x", KEY).unwrap();
        assert!(header(&out, "Set-Cookie").unwrap().contains("Max-Age=0"));
        assert!(post("/api", "", None, false, None, "http://x", KEY).is_none());
    }

    #[test]
    fn the_settings_pages_need_signing_in_and_a_csrf_token() {
        let base = "http://127.0.0.1:9090";
        let page = get("/ui/settings", "", Some(&cookie()), base, KEY).unwrap();
        assert_eq!(page.status, 200);
        for want in ["id=\"servers\"", "id=\"indexing\"", "id=\"api\"", "id=\"gemini\"", "name=\"_csrf\""] {
            assert!(page.body.contains(want), "missing {want:?} in {}", page.body);
        }
        for path in ["/ui/settings", "/ui/settings/groups/search"] {
            assert_ne!(get(path, "", None, base, KEY).unwrap().status, 200, "{path} needs signing in");
        }
        assert!(crate::web_settings::takes_post("/ui/settings/servers"));
        let forged = post("/ui/settings/servers", "host=x", Some(&cookie()), false, None, base, KEY).unwrap();
        assert_ne!(forged.status, 200, "{}", forged.body);
    }

    #[test]
    fn pictures_show_jpeg_metadata_without_hiding_distinct_values() {
        use crate::images::jpeg::{Exif, JpegInfo, Xmp};
        use crate::images::step::Image;

        let image = Image {
            position: 0,
            file: "cover.jpg".into(),
            sha256: "a".repeat(64),
            format: "jpeg".into(),
            width: 640,
            height: 480,
            bytes: 900,
            jpeg: Some(JpegInfo {
                width: 640,
                height: 480,
                precision: 8,
                components: 3,
                process: "progressive".into(),
                sampling: Some("4:2:0".into()),
                adobe_transform: Some(1),
                tables_hash: Some(0xabcd),
                exif: Exif {
                    software: Some("Camera firmware".into()),
                    serial: Some("A<&>1".into()),
                    orientation: Some(6),
                    ..Default::default()
                },
                xmp: Xmp {
                    creator_tool: Some("Photo editor".into()),
                    document_id: Some("current-id".into()),
                    original_document_id: Some("original-id".into()),
                    instance_id: Some("instance-id".into()),
                },
                ..Default::default()
            }),
        };

        let html = super::images_html(42, &[(42, image)]);
        for fact in [
            "<dt>Coding</dt><dd>progressive · 4:2:0 · 8 bit</dd>",
            "<dt>Components</dt><dd>3</dd>",
            "<dt>Adobe color transform</dt><dd>1 (YCbCr)</dd>",
            "<dt>Quantization table hash</dt><dd>000000000000abcd</dd>",
            "<dt>Orientation</dt><dd>6</dd>",
            "<dt>Camera serial</dt><dd>A&lt;&amp;&gt;1</dd>",
            "<dt>Software</dt><dd>Camera firmware</dd>",
            "<dt>XMP creator tool</dt><dd>Photo editor</dd>",
            "<dt>XMP document ID</dt><dd>current-id</dd>",
            "<dt>XMP original document ID</dt><dd>original-id</dd>",
            "<dt>XMP instance ID</dt><dd>instance-id</dd>",
        ] {
            assert!(html.contains(fact), "missing {fact} in {html}");
        }
    }

    #[test]
    fn stills_link_their_jpeg_and_say_where_they_are() {
        assert_eq!(same_film_html(&[]), "");
        let other = ReleaseRow { id: 9, name: "<A>.Film".into(), size: Some(1 << 30), ..Default::default() };
        let html = same_film_html(&[other]);
        assert!(html.contains("/ui/release?id=9") && html.contains("&lt;A&gt;.Film"), "{html}");
        assert_eq!(stills_html(7, &[], None), "");
        let html = stills_html(7, &[(25, Some(1_834_500), 960, 540), (50, None, 960, 540)], None);
        assert!(html.contains("<img src=\"/ui/still?id=7&amp;at=25\" width=\"960\" height=\"540\""), "{html}");
        assert!(html.contains("<figcaption>25% · 30:34</figcaption>") && html.contains("<figcaption>50%</figcaption>"));
        assert!(!html.contains("same video file"), "{html}");
        let shared = stills_html(9, &[(25, None, 960, 540)], Some((9, crate::keys::Kind::Mux)));
        assert!(shared.contains("/ui/still?id=9&amp;at=25") && shared.contains("/ui/release?id=9"), "{shared}");
        assert!(shared.contains("SegmentUID"), "{shared}");
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
    fn a_movie_row_is_shown_under_its_trash_name_with_its_own_name_under_it() {
        let name = "AC317772.2026.MULTi.VFF.AD.2160p.HDR.DV.WEB.EAC3.5.1.Atmos.H265-FRQC";
        let release = ReleaseRow {
            id: 7,
            name: name.into(),
            group_name: "alt.binaries.r2d2".into(),
            poster: None,
            posted_date: None,
            size: Some(2048),
            complete: true,
            parts: None,
        };
        let media =
            HashMap::from([(7, MediaAttrs { kind: Kind::Movie as i64, year: Some(2026), ..MediaAttrs::default() })]);
        let details = crate::media::details::Details {
            kind: Kind::Movie as i64,
            tmdb_id: 1,
            title: Some("Matchbox the Movie".into()),
            ..Default::default()
        };
        let extras = RowExtras { details: HashMap::from([(7, details)]), ..RowExtras::default() };
        let html = row(&release, &HashMap::from([(7, 2045)]), &HashMap::new(), &media, &extras);
        assert!(
            html.contains(">Matchbox the Movie (2026) [WEB-DL-2160p][DV HDR10][EAC3 Atmos 5.1][MULTi][H265]-FRQC</a>"),
            "{html}"
        );
        assert!(html.contains(&format!("<div class=\"raw-name\">{name}</div>")), "{html}");
        // anything but a movie or an episode keeps its own name, once
        let html = row(&release, &HashMap::from([(7, 3040)]), &HashMap::new(), &media, &extras);
        assert!(html.contains(&format!(">{name}</a>")) && !html.contains("raw-name"), "{html}");
    }

    #[test]
    fn search_links_keep_the_search_and_stay_on_a_page_start() {
        assert_eq!(search_query("", "", 0), "");
        assert_eq!(search_query("a b&c", "5000", 50), "q=a%20b%26c&cat=5000&offset=50");
        let args = parse_query("q=+x+&cat=2000&offset=120");
        let s = Search::from_args(&args);
        assert_eq!((s.q.as_str(), s.cat.as_str(), s.offset), ("x", "2000", 100));
        assert_eq!(Search::from_args(&parse_query("offset=-5")).offset, 0);
        // a pasted NZBLNK searches for its header
        let pasted = format!("q={}", url_encode(" nzblnk:?t=Some+Film&h=Some.Film.2024&g=a.b.x "));
        assert_eq!(Search::from_args(&parse_query(&pasted)).q, "Some.Film.2024");
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
            hide_incomplete: false,
            min_score: None,
            deep: false,
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
            hide_incomplete: false,
            min_score: None,
            deep: false,
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
            hide_incomplete: false,
            min_score: None,
            deep: false,
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
            hide_incomplete: false,
            min_score: None,
            deep: false,
        }
        .nothing_found();
        assert!(word.contains("&lt;b&gt;") && !word.contains("<b>"), "{word}");
        assert!(!word.contains("Look in all categories"), "{word}");
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
    fn matroska_tags_and_the_dvd_disc_id_show_but_never_a_person() {
        use crate::probe::media::MediaInfo;
        let tags = [
            ("IMDB", "tt0133093"),
            ("TMDB", "movie/603"),
            ("BPS", "8123456"),
            ("ENCODED_BY", "Some Person"),
            ("COMMENT", "mail me at someone@example.com"),
            ("DESCRIPTION", "<b>bold</b>"),
            ("TVDB", "tt<script>"),
        ];
        let media = MediaInfo {
            tags: tags.iter().map(|(n, v)| (n.to_string(), v.to_string())).collect(),
            disc_id: Some("918d26bab2dcf09adf08a37da9267d21".into()),
            ..Default::default()
        };
        let html = probe_html(&crate::probe::ProbeInfo { media: Some(media), ..Default::default() }, None, None, None);
        assert!(html.contains("<h2>Tags</h2>"), "{html}");
        assert!(
            html.contains("<a href=\"https://www.imdb.com/title/tt0133093/\" rel=\"noreferrer\">tt0133093</a>"),
            "{html}"
        );
        assert!(html.contains("https://www.themoviedb.org/movie/603"), "{html}");
        assert!(html.contains("<dt>BPS</dt><dd>8123456</dd>"), "{html}");
        assert!(!html.contains("Some Person") && !html.contains("ENCODED_BY"), "{html}");
        assert!(!html.contains("example.com"), "{html}");
        assert!(html.contains("&lt;b&gt;bold&lt;/b&gt;") && !html.contains("<script>"), "{html}");
        assert!(html.contains("<dt>DVD disc ID</dt><dd><code>918d26bab2dcf09adf08a37da9267d21</code></dd>"), "{html}");
        // sorted by name
        assert!(html.find("<dt>BPS</dt>") < html.find("<dt>IMDB</dt>"), "{html}");
        let long = tag_value("COMMENT", &"x".repeat(1000));
        assert!(long.ends_with('…') && long.chars().count() == TAG_VALUE_CHARS + 1);
    }

    #[test]
    fn mkvmerge_statistics_show_on_their_track_and_a_blu_ray_disc_id_says_so() {
        use crate::probe::media::{AudioTrack, MediaInfo, SubtitleTrack, TrackStats, VideoTrack};
        let stats = |bps, frames, bytes| {
            Some(TrackStats { bps: Some(bps), frames: Some(frames), bytes: Some(bytes), ..TrackStats::default() })
        };
        let media = MediaInfo {
            container: "matroska".into(),
            video: vec![VideoTrack {
                codec: "V_MPEGH/ISO/HEVC".into(),
                stats: stats(8_123_456, 172_345, 7_302_958_152),
                ..VideoTrack::default()
            }],
            audio: vec![AudioTrack {
                codec: "A_EAC3".into(),
                stats: stats(640_000, 224_740, 575_334_400),
                ..AudioTrack::default()
            }],
            subtitles: vec![SubtitleTrack {
                codec: "S_TEXT/UTF8".into(),
                stats: stats(72, 1534, 64_893),
                ..SubtitleTrack::default()
            }],
            ..Default::default()
        };
        let html = probe_html(&crate::probe::ProbeInfo { media: Some(media), ..Default::default() }, None, None, None);
        assert!(html.contains("V_MPEGH/ISO/HEVC, 8,123 kb/s, 172,345 frames, 7,302,958,152 bytes"), "{html}");
        assert!(html.contains("A_EAC3, 640 kb/s, 224,740 frames, 575,334,400 bytes"), "{html}");
        assert!(html.contains("S_TEXT/UTF8, 72 b/s, 1,534 events, 64,893 bytes"), "{html}");
        let disc = MediaInfo { container: "bluray".into(), disc_id: Some("00010203a0a1".into()), ..Default::default() };
        let html = probe_html(&crate::probe::ProbeInfo { media: Some(disc), ..Default::default() }, None, None, None);
        assert!(html.contains("<dt>Blu-ray disc ID</dt><dd><code>00010203a0a1</code></dd>"), "{html}");
        assert!(!html.contains("DVD disc ID"), "{html}");
    }

    #[test]
    fn dolby_vision_says_fel_or_mel_and_cm_only_from_an_rpu() {
        use crate::probe::media::{AudioTrack, DolbyVisionInfo, MediaInfo, VideoTrack};
        let dv = |fel: bool, cm: &str, from_rpu: bool| VideoTrack {
            hdr: Some("HDR10".into()),
            dv_profile: Some(7),
            dolby_vision: Some(DolbyVisionInfo {
                profile: 7,
                level: Some(6),
                rpu_present: true,
                el_present: true,
                fel,
                cm_version: cm.into(),
                from_rpu,
            }),
            ..VideoTrack::default()
        };
        assert_eq!(hdr_summary(&dv(true, "v4.0", true)), "DV P7 FEL + HDR10");
        assert_eq!(hdr_summary(&dv(false, "v2.9", true)), "DV P7 MEL + HDR10");
        // a dvcC box alone doesn't say which layer it is
        assert_eq!(hdr_summary(&dv(false, "v2.9", false)), "DV P7 + HDR10");
        assert_eq!(dv_rpu_facts(&dv(true, "v4.0", true)), ["FEL", "CM v4.0"]);
        assert!(dv_rpu_facts(&dv(false, "v2.9", false)).is_empty());
        let p8 = VideoTrack {
            dv_profile: Some(8),
            dolby_vision: Some(DolbyVisionInfo {
                profile: 8,
                cm_version: "v4.0".into(),
                from_rpu: true,
                ..Default::default()
            }),
            ..VideoTrack::default()
        };
        assert_eq!(dv_rpu_facts(&p8), ["CM v4.0"]);

        let audio = |name: Option<&str>| AudioTrack {
            codec: "A_DTS".into(),
            dtsx: true,
            name: name.map(str::to_string),
            ..Default::default()
        };
        let media = MediaInfo { audio: vec![audio(None), audio(Some("DTS:X 7.1"))], ..Default::default() };
        let html = probe_html(&crate::probe::ProbeInfo { media: Some(media), ..Default::default() }, None, None, None);
        assert_eq!(html.matches("DTS:X").count(), 2, "{html}");
    }

    #[test]
    fn a_pictures_cell_says_where_it_stands_and_offers_a_fetch() {
        let offer = picture_state_html(9, "00 thumb.jpg", &PictureState::Offer, "tok");
        assert!(offer.contains("hx-post=\"/ui/picture\""), "{offer}");
        assert!(offer.contains("&quot;name&quot;:&quot;00 thumb.jpg&quot;"), "{offer}");
        assert!(offer.contains("&quot;X-CSRF-Token&quot;:&quot;tok&quot;"), "{offer}");
        let queued = picture_state_html(9, "a.jpg", &PictureState::Queued("in about 4 min".into()), "tok");
        assert!(queued.contains("QUEUED") && queued.contains("ready in about 4 min") && queued.contains("every 30s"));
        assert!(picture_state_html(9, "a.jpg", &PictureState::Shown, "tok").contains("SHOWN"));
        let failed = picture_state_html(9, "a.jpg", &PictureState::Failed("not a picture".into()), "tok");
        assert!(failed.contains("not a picture") && failed.contains("Try again"));
        let req = |done: Option<i64>, outcome: Option<&str>| crate::images::step::Request {
            release_id: 9,
            file: "a.jpg".into(),
            requested_at: 1,
            done_at: done,
            outcome: outcome.map(str::to_string),
            passed_over: false,
        };
        assert_eq!(picture_state("a.jpg", &[], &[]), PictureState::Offer);
        assert_eq!(
            picture_state("a.jpg", &[], &[req(Some(2), Some("unreadable"))]),
            PictureState::Failed("unreadable".into())
        );
        assert_eq!(
            picture_state("a.jpg", &[], &[req(Some(2), Some("shown"))]),
            PictureState::Offer,
            "its file went from the folder"
        );
        assert!(matches!(picture_state("a.jpg", &[], &[req(None, None)]), PictureState::Queued(_)));
        let passed = crate::images::step::Request { passed_over: true, ..req(None, None) };
        assert!(matches!(picture_state("a.jpg", &[], &[passed]), PictureState::Failed(_)), "no run will answer it");
    }

    #[test]
    fn the_nfo_has_a_panel_of_its_own() {
        let html = nfo_html("<Some NFO>", &["tt1409024".to_string()]);
        assert!(html.contains("<section id=\"nfo\"><h2>NFO</h2>"));
        assert!(html.contains("&lt;Some NFO&gt;"));
        assert!(html.contains("imdb.com/title/tt1409024/"));
        assert!(!nfo_html("x", &[]).contains("<p>"));
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
    fn hiding_incomplete_is_kept_in_links_and_a_cookie() {
        let s = Search::from_args(&parse_query("q=x&cat=2040&incomplete=hide"));
        assert!(s.hide_incomplete);
        assert_eq!(s.query("2040", 0), "q=x&cat=2040&incomplete=hide");
        assert_eq!(s.incomplete_toggled(), "q=x&cat=2040&incomplete=show", "turning it off says so");
        let off = Search::from_args(&parse_query("q=x"));
        assert!(!off.hide_incomplete);
        assert_eq!(off.incomplete_toggled(), "q=x&incomplete=hide");
        assert!(s.filter_links().contains("aria-current=\"true\">Hide incomplete<"));
        assert!(!off.filter_links().contains("aria-current=\"true\">Hide incomplete<"));
        assert!(s.hides(&ReleaseRow { complete: false, ..Default::default() }));
        assert!(!s.hides(&ReleaseRow { complete: true, ..Default::default() }));
        assert!(!off.hides(&ReleaseRow { complete: false, ..Default::default() }));
        // copies hidden still go a page at a time

        let saved = Some("atlas_session=abc; atlas_incomplete=hide");
        let mut args = parse_query("q=x");
        assert_eq!(incomplete_choice(&mut args, saved), None);
        assert_eq!(args.get("incomplete").map(String::as_str), Some("hide"), "the cookie stands in for the url");
        let mut args = parse_query("q=x");
        assert_eq!(incomplete_choice(&mut args, Some("atlas_session=abc")), None);
        assert!(!args.contains_key("incomplete"), "shown, as before, with no cookie");
        let set = incomplete_choice(&mut parse_query("incomplete=hide"), None).unwrap();
        assert!(set.1.starts_with("atlas_incomplete=hide;") && set.1.contains("Max-Age=31536000"), "{set:?}");
        assert_eq!(incomplete_choice(&mut parse_query("incomplete=hide"), saved), None, "already kept");
        let cleared = incomplete_choice(&mut parse_query("incomplete=show"), saved).unwrap();
        assert!(cleared.1.starts_with("atlas_incomplete=;") && cleared.1.contains("Max-Age=0"), "{cleared:?}");
        let mut args = parse_query("incomplete=show");
        assert_eq!(incomplete_choice(&mut args, None), None);
        assert!(!Search::from_args(&args).hide_incomplete);
    }

    #[test]
    fn a_rating_chip_keeps_only_releases_rated_that_high() {
        let s = Search::from_args(&parse_query("q=film&rated=70"));
        assert_eq!(s.min_score, Some(70));
        assert!(s.query(&s.cat, 0).contains("rated=70"), "it stays in the url");
        assert_eq!(Search::from_args(&parse_query("rated=75")).min_score, None, "only the chips' scores");
        assert_eq!(Search::from_args(&parse_query("rated=x")).min_score, None);
        let scores: HashMap<i64, i64> = [(1, 82), (2, 70), (3, 69)].into_iter().collect();
        let out: Vec<i64> = (1..=4).filter(|id| s.rated_out(*id, &scores)).collect();
        assert_eq!(out, [3, 4], "below 7, and no score kept");
        let chips = s.rated_links();
        assert!(chips.contains("aria-current=\"true\">Rated 7+</a>"), "{chips}");
        assert!(!chips.contains("rated=70\" hx-get"), "the chip that's on turns it off: {chips}");
        assert!(chips.contains("rated=80"), "{chips}");
        let off = Search::from_args(&parse_query("q=film"));
        assert!(!off.rated_out(4, &scores), "no chip, nothing left out");
    }

    #[test]
    fn filters_come_from_the_list_and_stay_in_links() {
        let s = Search::from_args(&parse_query("q=x&filters=DV,<b>,Remux"));
        assert_eq!(s.filters, ["DV", "Remux"]);
        assert_eq!(s.query("2040", 0), "q=x&cat=2040&filters=DV%2CRemux");
        let hide = Search::from_args(&parse_query("filters=Hide%20copies"));
        assert!(!hide.filters_by_tags());
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
