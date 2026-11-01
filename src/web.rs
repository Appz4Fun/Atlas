//! A small browser ui on the api port: search, browse and download NZBs.
//!
//! The api key never reaches a page. Signing in with it sets a cookie derived from it,
//! and the server does the searching and the NZB downloads itself.

use std::collections::HashMap;
use std::fmt::Write;

use sha1::{Digest, Sha1};

use crate::api::{ApiResponse, escape, find_releases, get_nzb, parse_query};
use crate::archive::Password;
use crate::category::{TABLE, UNCATEGORIZED, label, parse_filter};
use crate::search::ReleaseRow;
use crate::ui::{fmt_date, fmt_size};

const HTMX: &str = include_str!("../assets/htmx-2.0.11.min.js");
const CSS: &str = include_str!("../assets/web.css");

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
        "/" | "" if signed_in => html(200, search_page(&args, base)),
        "/" | "" => html(200, login_page(base, false)),
        "/ui/results" if signed_in => results(&args),
        "/ui/results" => ApiResponse { headers: vec![("HX-Redirect".into(), "/".into())], ..html(401, String::new()) },
        "/ui/nzb" if signed_in => get_nzb(&args),
        "/ui/nzb" => redirect(Vec::new()),
        _ => return None,
    })
}

/// A POST the ui answers. None: not ours.
pub fn post(path: &str, body: &str, base: &str, api_key: &str) -> Option<ApiResponse> {
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
        _ => None,
    }
}

/// The ui reads a request body only on these paths.
pub fn takes_post(path: &str) -> bool {
    matches!(path, "/login" | "/logout")
}

fn cookie_header(value: &str, max_age: i64) -> (String, String) {
    ("Set-Cookie".into(), format!("{COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}"))
}

fn redirect(mut headers: Vec<(String, String)>) -> ApiResponse {
    headers.push(("Location".into(), "/".into()));
    ApiResponse { status: 303, content_type: None, body: String::new(), headers }
}

fn asset(content_type: &'static str, body: &str) -> ApiResponse {
    ApiResponse {
        status: 200,
        content_type: Some(content_type),
        body: body.into(),
        headers: vec![
            ("Cache-Control".into(), "public, max-age=86400".into()),
            ("X-Content-Type-Options".into(), "nosniff".into()),
        ],
    }
}

fn html(status: u16, body: String) -> ApiResponse {
    ApiResponse {
        status,
        content_type: Some("text/html; charset=utf-8"),
        body,
        headers: vec![
            ("Content-Security-Policy".into(), CSP.into()),
            ("Cache-Control".into(), "no-store".into()),
            ("X-Content-Type-Options".into(), "nosniff".into()),
        ],
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
fn attr(s: &str) -> String {
    escape(s).replace('"', "&quot;").replace('\'', "&#39;")
}

fn url_encode(s: &str) -> String {
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
}

impl Search {
    fn from_args(args: &HashMap<String, String>) -> Search {
        let text = |k: &str| args.get(k).map_or(String::new(), |v| v.trim().to_string());
        // the name search pages by offset / limit, soo a page starts on a multiple
        let offset = args.get("offset").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0).max(0);
        Search { q: text("q"), cat: text("cat"), offset: offset - offset % PAGE_SIZE }
    }

    fn rows(&self) -> String {
        let filter = parse_filter(&self.cat);
        let found = match find_releases(&self.q, filter.as_ref(), self.offset, PAGE_SIZE) {
            Ok(found) => found,
            Err(e) => {
                eprintln!("web search failed: {e}");
                return "<tr><td colspan=\"6\" class=\"note\">Search failed.</td></tr>".into();
            }
        };
        let ids: Vec<i64> = found.iter().map(|r| r.id).collect();
        let categories = crate::category::categories_of(&ids).unwrap_or_default();
        let passwords = crate::archive::store::passwords(&ids);

        let mut out: String = found.iter().map(|r| row(r, &categories, &passwords)).collect();
        if found.is_empty() && self.offset == 0 {
            out.push_str("<tr><td colspan=\"6\" class=\"note\">No releases found.</td></tr>");
        }
        if found.len() as i64 == PAGE_SIZE {
            let next = search_query(&self.q, &self.cat, self.offset + PAGE_SIZE);
            let next = attr(&next);
            let _ = write!(
                out,
                "<tr><td colspan=\"6\" class=\"note\"><a href=\"/?{next}\" hx-get=\"/ui/results?{next}\" hx-target=\"closest tr\" hx-swap=\"outerHTML\">Load more</a></td></tr>"
            );
        }
        out
    }
}

fn row(r: &ReleaseRow, categories: &HashMap<i64, i64>, passwords: &HashMap<i64, Password>) -> String {
    let category = categories.get(&r.id).copied().unwrap_or(UNCATEGORIZED);
    let mut flags = String::new();
    match passwords.get(&r.id) {
        Some(Password::Yes) => flags.push_str(" <span class=\"flag\">password</span>"),
        Some(Password::Maybe) => flags.push_str(" <span class=\"flag\">password?</span>"),
        _ => {}
    }
    if !r.complete {
        flags.push_str(" <span class=\"flag\">incomplete</span>");
    }
    format!(
        "<tr><td class=\"name\">{}{flags}</td><td>{}</td><td class=\"num\">{}</td><td class=\"posted\">{}</td><td class=\"group\">{}</td><td><a href=\"/ui/nzb?id={}\">NZB</a></td></tr>",
        escape(&r.name),
        escape(&label(category).unwrap_or_default()),
        fmt_size(r.size),
        escape(&fmt_date(r.posted_date.as_deref())),
        escape(&r.group_name),
        r.id,
    )
}

fn table(rows: &str) -> String {
    format!(
        "<table><thead><tr><th>Name</th><th>Category</th><th class=\"num\">Size</th><th class=\"posted\">Posted</th><th class=\"group\">Group</th><th></th></tr></thead><tbody>{rows}</tbody></table>"
    )
}

/// What htmx swaps in: the whole table for a new search, more rows for "Load more".
fn results(args: &HashMap<String, String>) -> ApiResponse {
    let search = Search::from_args(args);
    if search.offset > 0 {
        return html(200, search.rows());
    }
    let mut resp = html(200, table(&search.rows()));
    // the address bar follows the search, and reloads the full page
    let query = search_query(&search.q, &search.cat, 0);
    let url = if query.is_empty() { "/".to_string() } else { format!("/?{query}") };
    resp.headers.push(("HX-Push-Url".into(), url));
    resp
}

fn category_options(selected: &str) -> String {
    let mut out = String::from("<option value=\"\">All categories</option>");
    let mut option = |id: i64, name: &str| {
        let chosen = if id.to_string() == selected { " selected" } else { "" };
        let _ = write!(out, "<option value=\"{id}\"{chosen}>{}</option>", escape(name));
    };
    for root in TABLE {
        option(root.id, root.name);
        for sub in root.subs {
            option(sub.id, &format!("{} / {}", root.name, sub.name));
        }
    }
    out
}

fn page(title: &str, content: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><meta name=\"htmx-config\" content=\"{}\"><title>{}</title><link rel=\"stylesheet\" href=\"/static/atlas.css\"><script src=\"/static/htmx.min.js\"></script></head><body>{content}</body></html>",
        attr(HTMX_CONFIG),
        escape(title),
    )
}

fn search_page(args: &HashMap<String, String>, base: &str) -> String {
    let search = Search::from_args(args);
    page(
        "Atlas",
        &format!(
            r##"<header><h1>Atlas</h1><form method="post" action="/logout"><button>Sign out</button></form></header>
<form class="search" action="/" method="get" hx-get="/ui/results" hx-target="#results" hx-sync="this:replace" hx-indicator="#busy" hx-trigger="submit, input changed delay:400ms from:#q, change from:#cat">
<input id="q" name="q" type="search" placeholder="Search releases" value="{}" autofocus aria-label="Search releases">
<select id="cat" name="cat" aria-label="Category">{}</select>
<button>Search</button><span id="busy" class="htmx-indicator">Searching…</span>
</form>
<main id="results">{}</main>
<footer>Newznab API: {}/api</footer>"##,
            attr(&search.q),
            category_options(&search.cat),
            table(&search.rows()),
            escape(base),
        ),
    )
}

/// Also what someone pointing a browser at the bare address gets, soo it says how to set up a client.
fn login_page(base: &str, wrong_key: bool) -> String {
    let error = if wrong_key { "<p class=\"error\">That is not the API key.</p>" } else { "" };
    let base = escape(base);
    page(
        "Atlas sign in",
        &format!(
            r#"<header><h1>Atlas</h1></header>
<form class="login" method="post" action="/login">
<label for="key">API key</label>
<input id="key" name="key" type="password" autocomplete="current-password" autofocus required>
<button>Sign in</button>{error}
</form>
<section class="setup"><h2>Add Atlas to Prowlarr, Sonarr or Radarr</h2>
<p>Add it as a Newznab indexer, with the <code>api_key</code> in <code>config.toml</code>.</p>
<ul><li>URL: <code>{base}</code></li><li>API path: <code>/api</code></li><li>Check it: <code>{base}/api?t=caps</code></li></ul></section>"#
        ),
    )
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
        let wrong = post("/login", "key=nope", "http://x", KEY).unwrap();
        assert_eq!(wrong.status, 401);
        assert!(header(&wrong, "Set-Cookie").is_none());
        assert!(wrong.body.contains("not the API key"));

        let right = post("/login", &format!("key={KEY}"), "http://x", KEY).unwrap();
        assert_eq!(right.status, 303);
        assert_eq!(header(&right, "Location"), Some("/"));
        let set = header(&right, "Set-Cookie").unwrap();
        assert!(set.contains("HttpOnly") && set.contains("SameSite=Strict"), "{set}");
        assert!(!set.contains(KEY), "{set}");

        let out = post("/logout", "", "http://x", KEY).unwrap();
        assert!(header(&out, "Set-Cookie").unwrap().contains("Max-Age=0"));
        assert!(post("/api", "", "http://x", KEY).is_none());
    }

    #[test]
    fn only_the_right_cookie_gets_in() {
        for bad in [None, Some("atlas_session=nope"), Some("x=1"), Some("atlas_session"), Some(KEY)] {
            assert!(!signed_in(bad, KEY), "{bad:?}");
            let results = get("/ui/results", "", bad, "http://x", KEY).unwrap();
            assert_eq!(results.status, 401, "{bad:?}");
            assert_eq!(header(&results, "HX-Redirect"), Some("/"));
            let nzb = get("/ui/nzb", "id=1", bad, "http://x", KEY).unwrap();
            assert_eq!(nzb.status, 303, "{bad:?}");
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
        let html = row(&release, &HashMap::from([(7, 5040)]), &passwords);
        assert!(!html.contains("<img") && !html.contains("<b>") && !html.contains("<i>"), "{html}");
        assert!(html.contains("TV/HD") && html.contains("2.0 KB"), "{html}");
        assert!(html.contains("password?") && html.contains("incomplete"), "{html}");
        assert!(html.contains("/ui/nzb?id=7"), "{html}");
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
    fn the_category_list_keeps_the_choice() {
        let options = category_options("5040");
        assert!(options.contains(r#"<option value="5040" selected>TV / HD</option>"#), "{options}");
        assert_eq!(options.matches(" selected").count(), 1);
        assert_eq!(category_options("<x>").matches(" selected").count(), 0);
    }
}
