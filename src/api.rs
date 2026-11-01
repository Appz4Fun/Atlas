//! Newznab compatible api for prowlarr / sonarr / radarr.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use tiny_http::{Header, Request, Response, Server};

use crate::config::{Config, get_api_key};
use crate::dates::pub_date;
use crate::nzb::{build_nzb, build_nzb_file, nzb_filename};
use crate::search::{all_releases, get_release, newest_in_categories, search_all_releases, search_in_categories};
use crate::ui;

const WORKERS: usize = 4;

const CAPS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<caps>
  <server version="1.0" title="Atlas" strapline="Atlas Usenet Indexer" url="http://127.0.0.1:9090" email="" image=""/>
  <limits max="100" default="100"/>
  <retention days="0"/>
  <registration available="no" open="no"/>
  <searching>
    <search available="yes" supportedParams="q,cat,limit,offset"/>
    <tv-search available="yes" supportedParams="q,season,ep,cat,limit,offset<!--tv-ids-->"/>
    <movie-search available="yes" supportedParams="q,cat,limit,offset<!--movie-ids-->"/>
    <audio-search available="yes" supportedParams="q,cat,limit,offset"/>
    <book-search available="yes" supportedParams="q,cat,limit,offset"/>
  </searching>
  <categories><!--categories--></categories>
</caps>"#;

const XML: &str = "application/xml; charset=utf-8";

#[derive(Debug, PartialEq, Eq)]
pub struct ApiResponse {
    pub status: u16,
    pub content_type: Option<&'static str>,
    pub body: String,
    pub headers: Vec<(String, String)>,
}

impl ApiResponse {
    fn xml(status: u16, body: String) -> Self {
        ApiResponse { status, content_type: Some(XML), body, headers: Vec::new() }
    }

    fn not_found() -> Self {
        ApiResponse { status: 404, content_type: None, body: String::new(), headers: Vec::new() }
    }

    /// newznab error, sent with http 200 like real indexers do
    fn error(code: u16, description: &str) -> Self {
        ApiResponse::xml(
            200,
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<error code=\"{code}\" description=\"{description}\"/>"
            ),
        )
    }
}

struct Running {
    server: Arc<Server>,
    workers: Vec<JoinHandle<()>>,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn percent_decode(s: &str) -> String {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => match (bytes.get(i + 1).copied().and_then(hex), bytes.get(i + 2).copied().and_then(hex)) {
                (Some(h), Some(l)) => {
                    out.push(h << 4 | l);
                    i += 3;
                    continue;
                }
                _ => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }

    String::from_utf8_lossy(&out).into_owned()
}

/// first value wins, like flask's request.args.get
pub fn parse_query(query: &str) -> HashMap<String, String> {
    let mut args = HashMap::new();

    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        args.entry(percent_decode(k)).or_insert_with(|| percent_decode(v));
    }

    args
}

/// Handle `/api?{query}`. `base` is the url root without trailing slash.
pub fn handle(path: &str, query: &str, base: &str, api_key: &str) -> ApiResponse {
    // a browser pointed at the bare address gets told where the api is,
    // instead of a blank 404 that looks like nothing is listening
    if path == "/" || path.is_empty() {
        return ApiResponse {
            status: 200,
            content_type: Some("text/plain; charset=utf-8"),
            body: format!(
                "Atlas newznab api is running.\n\nAdd it to Prowlarr / Sonarr / Radarr as a Newznab indexer:\n  URL:      {base}\n  API path: /api\n  API key:  the api_key in config.toml\n\nCheck it: {base}/api?t=caps\n"
            ),
            headers: Vec::new(),
        };
    }

    if path != "/api" {
        return ApiResponse::not_found();
    }

    let args = parse_query(query);
    let t = args.get("t").map(String::as_str);

    if t == Some("caps") {
        // advertise the address the client actually used
        return ApiResponse::xml(200, caps_body(base, &crate::media::query::refreshed_id_params()));
    }

    if args.get("apikey").map(String::as_str) != Some(api_key) {
        return ApiResponse::xml(
            401,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<error code=\"100\" description=\"Invalid API Key\"/>".into(),
        );
    }

    match t {
        Some("get") => get(&args),
        Some(kind @ ("search" | "tvsearch" | "movie" | "music" | "audio" | "book")) => {
            let on = crate::media::query::cached_id_params();
            if let Some(ids) = crate::media::query::from_args(kind, &args, &on) {
                return id_search(kind, &ids, &args, base, api_key, &on);
            }
            match search_term(kind, &args) {
                Some(term) => search(&term, &args, base, api_key),
                // only ids, and none of them on: an empty answer, soo a client falls back to a name
                None => rss(base, api_key, 0, &[], &HashMap::new(), &HashMap::new(), &HashMap::new(), &on),
            }
        }
        None => ApiResponse::error(200, "Missing parameter (t)"),
        Some(_) => ApiResponse::error(203, "Function not available"),
    }
}

/// id parameters that alone mean nothing to search by name
const ID_PARAMS: [&str; 8] = ["tvdbid", "rid", "tvrageid", "tvmazeid", "imdbid", "tmdbid", "traktid", "doubanid"];

/// What to search for. `tvsearch` adds `S01E02` style season/episode terms.
/// None when the request only has ids and nothing to search by name.
pub fn search_term(kind: &str, args: &HashMap<String, String>) -> Option<String> {
    let mut term = args.get("q").map(|q| q.trim().to_string()).unwrap_or_default();

    if kind == "tvsearch" {
        let num = |k: &str| args.get(k).and_then(|v| v.trim().parse::<u32>().ok());
        let daily = args.get("ep").and_then(|e| e.trim().split_once('/')).and_then(|(m, d)| {
            let (m, d) = (m.parse::<u32>().ok()?, d.parse::<u32>().ok()?);
            num("season").filter(|y| (1000..=9999).contains(y)).map(|y| (y, m, d))
        });
        if let Some((y, m, d)) = daily {
            term = format!("{term} {y} {m:02} {d:02}");
        }
        match (num("season").filter(|_| daily.is_none()), num("ep")) {
            (Some(s), Some(e)) => term = format!("{term} S{s:02}E{e:02}"),
            (Some(s), None) => term = format!("{term} S{s:02}"),
            _ => {}
        }
        term = term.trim().to_string();
    }

    let has_id = ID_PARAMS.iter().any(|k| args.get(*k).is_some_and(|v| !v.trim().is_empty()));
    if has_id && args.get("q").is_none_or(|q| q.trim().is_empty()) {
        return None;
    }

    Some(term)
}

/// `caps`, with the ID parameters that are on.
pub fn caps_body(base: &str, on: &crate::media::query::IdParams) -> String {
    CAPS.replace("http://127.0.0.1:9090", &escape(base))
        .replace("<!--categories-->", &crate::category::caps_xml())
        .replace("<!--tv-ids-->", &on.tv.caps())
        .replace("<!--movie-ids-->", &on.movie.caps())
}

/// A search by ID. Nothing found and a `q`: by name, which keeps "an ID next to `q`" useful.
fn id_search(
    kind: &str,
    ids: &crate::media::query::IdQuery,
    args: &HashMap<String, String>,
    base: &str,
    api_key: &str,
    on: &crate::media::query::IdParams,
) -> ApiResponse {
    let limit = args.get("limit").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(100).clamp(1, 100);
    let offset = args.get("offset").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0).max(0);
    let filter = args.get("cat").and_then(|c| crate::category::parse_filter(c));
    match crate::media::query::find(ids, filter.as_ref(), offset, limit) {
        Ok(rows) if rows.is_empty() => {
            if args.get("q").is_some_and(|q| !q.trim().is_empty())
                && let Some(term) = search_term(kind, args)
            {
                return search(&term, args, base, api_key);
            }
            rss(base, api_key, offset, &[], &HashMap::new(), &HashMap::new(), &HashMap::new(), on)
        }
        Ok(rows) => {
            let found: Vec<i64> = rows.iter().map(|r| r.id).collect();
            let categories = crate::category::categories_of(&found).unwrap_or_default();
            let media = crate::media::query::attrs_of(&found).unwrap_or_default();
            rss(base, api_key, offset, &rows, &crate::archive::store::passwords(&found), &categories, &media, on)
        }
        Err(e) => {
            eprintln!("api id search failed: {e}");
            ApiResponse::error(900, "Search failed")
        }
    }
}

fn search(q: &str, args: &HashMap<String, String>, base: &str, api_key: &str) -> ApiResponse {
    let limit = args.get("limit").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(100).clamp(1, 100);
    let offset = args.get("offset").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0).max(0);

    let filter = args.get("cat").and_then(|c| crate::category::parse_filter(c));
    let releases = match (&filter, q.trim().is_empty()) {
        (None, true) => all_releases(offset / limit, limit),
        (None, false) => search_all_releases(q, offset / limit, limit),
        (Some(f), true) => newest_in_categories(f, offset, limit).map(|p| p.rows),
        (Some(f), false) => search_in_categories(q, f, offset, limit),
    };

    match releases {
        Ok(r) => {
            let ids: Vec<i64> = r.iter().map(|release| release.id).collect();
            let categories = crate::category::categories_of(&ids).unwrap_or_else(|e| {
                eprintln!("api categories failed: {e}");
                HashMap::new()
            });
            let on = crate::media::query::cached_id_params();
            let media = crate::media::query::attrs_of(&ids).unwrap_or_default();
            rss(base, api_key, offset, &r, &crate::archive::store::passwords(&ids), &categories, &media, &on)
        }
        Err(e) => {
            eprintln!("api search failed: {e}");
            ApiResponse::error(900, "Search failed")
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn rss(
    base: &str,
    api_key: &str,
    offset: i64,
    releases: &[crate::search::ReleaseRow],
    passwords: &HashMap<i64, crate::archive::Password>,
    categories: &HashMap<i64, i64>,
    media: &HashMap<i64, crate::media::query::MediaAttrs>,
    on: &crate::media::query::IdParams,
) -> ApiResponse {
    let items: String = releases
        .iter()
        .map(|r| {
            let nzb_url = format!("{base}/api?t=get&id={}&apikey={api_key}", r.id);
            let size = r.size.unwrap_or(0);
            let password = passwords
                .get(&r.id)
                .map_or(String::new(), |p| format!("<newznab:attr name=\"password\" value=\"{}\"/>", p.code()));
            let category = categories.get(&r.id).copied().unwrap_or(crate::category::UNCATEGORIZED);
            let attrs = media.get(&r.id).map_or(String::new(), |a| a.xml(on));
            format!(
                "    <item>      <title>{}</title>      <guid isPermaLink=\"false\">{}</guid>      <link>{}</link>      <size>{size}</size>      <pubDate>{}</pubDate>      <enclosure url=\"{}\" type=\"application/x-nzb\" length=\"{size}\"/>      <newznab:attr name=\"category\" value=\"{category}\"/>{attrs}{password}    </item>",
                escape(&r.name),
                r.id,
                escape(&nzb_url),
                pub_date(r.posted_date.as_deref().unwrap_or("")),
                escape(&nzb_url),
            )
        })
        .collect();

    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"
     xmlns:atom="http://www.w3.org/2005/Atom"
     xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
    <channel>
        <title>Atlas</title>
        <description>Atlas search results</description>
        <link>{}/api</link>
        <language>en-gb</language>
        <newznab:response offset="{offset}" total="{}"/>
        {items}
    </channel>
</rss>"#,
        escape(base),
        offset + releases.len() as i64,
    );

    ApiResponse::xml(200, xml)
}

fn get(args: &HashMap<String, String>) -> ApiResponse {
    let release_id = args.get("id").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0);

    let Ok(Some(release)) = get_release(release_id) else {
        return ApiResponse::not_found();
    };

    // `file` isnt in the Newznab spec: one file of the release, and one that isnt there is a miss
    let built = match args.get("file") {
        Some(file) => file.trim().parse().ok().map_or(Ok(None), |file| build_nzb_file(release_id, file)),
        None => build_nzb(release_id),
    };
    let Ok(Some(content)) = built else {
        return ApiResponse::not_found();
    };

    let filename = nzb_filename(&release.name, Some(release_id)).replace(['"', '\\'], "_");

    ApiResponse {
        status: 200,
        content_type: Some("application/x-nzb"),
        body: content,
        headers: vec![("Content-Disposition".into(), format!("attachment; filename=\"{filename}\""))],
    }
}

fn respond(request: Request, api_key: &str, fallback_host: &str) {
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));

    let host = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Host"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_else(|| fallback_host.to_string());
    let base = format!("http://{host}");

    let is_get = matches!(request.method(), tiny_http::Method::Get | tiny_http::Method::Head);
    let resp = if is_get {
        handle(path, query, &base, api_key)
    } else {
        ApiResponse { status: 405, content_type: None, body: String::new(), headers: Vec::new() }
    };

    let mut response = Response::from_string(resp.body).with_status_code(resp.status);

    let mut headers = resp.headers;
    headers.push(("Content-Type".into(), resp.content_type.unwrap_or("text/html; charset=utf-8").into()));

    for (k, v) in headers {
        if let Ok(h) = Header::from_bytes(k.as_bytes(), v.as_bytes()) {
            response.add_header(h);
        }
    }

    let _ = request.respond(response);
}

/// Start the api on a few worker threads. Returns false when it couldnt bind.
pub fn start(config: &mut Config) -> bool {
    let (api_key, created) = get_api_key(config);

    if created {
        ui::print(&format!("[yellow]api key: {api_key}[/yellow]"));
    }

    let port = config.api_port();
    let host = config.api_host.clone();
    let bind_host = if host.contains(':') && !host.starts_with('[') { format!("[{host}]") } else { host.clone() };
    let addr = format!("{bind_host}:{port}");

    let server = match Server::http(&addr) {
        Ok(s) => Arc::new(s),
        Err(_) => {
            ui::print(&format!("[red]port {port} is already in use, api not started[/red]"));
            ui::print("[dim]change it under Settings -> Change api port[/dim]");
            return false;
        }
    };

    ui::print(&format!("[dim]newznab api on http://{addr} — apikey required for search/get[/dim]"));

    let workers = (0..WORKERS)
        .map(|_| {
            let server = server.clone();
            let key = api_key.clone();
            let fallback = addr.clone();
            thread::spawn(move || {
                for request in server.incoming_requests() {
                    respond(request, &key, &fallback);
                }
            })
        })
        .collect();

    *RUNNING.lock().unwrap() = Some(Running { server, workers });
    true
}

pub fn stop() {
    let Some(running) = RUNNING.lock().unwrap().take() else { return };

    for _ in &running.workers {
        running.server.unblock();
    }

    for w in running.workers {
        let _ = w.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parsing() {
        let a = parse_query("t=search&q=the+matrix%20reloaded&q=ignored&apikey=a%2Bb&bad=%zz");
        assert_eq!(a["q"], "the matrix reloaded");
        assert_eq!(a["apikey"], "a+b");
        assert_eq!(a["bad"], "%zz");
    }

    #[test]
    fn caps_needs_no_key_and_others_do() {
        let r = handle("/api", "t=caps", "http://x", "key");
        assert_eq!(r.status, 200);
        assert!(r.body.contains("<caps>"));

        let r = handle("/api", "t=search&apikey=wrong", "http://x", "key");
        assert_eq!(r.status, 401);
        assert!(r.body.contains("Invalid API Key"));

        assert_eq!(handle("/nope", "t=caps", "http://x", "key").status, 404);

        let root = handle("/", "", "http://192.168.1.172:9091", "key");
        assert_eq!(root.status, 200);
        assert!(root.body.contains("http://192.168.1.172:9091/api?t=caps"));
        assert!(!root.body.contains("key\n"), "never print the api key");
        assert!(
            handle("/api", "t=caps", "http://192.168.1.172:9091", "key")
                .body
                .contains(r#"url="http://192.168.1.172:9091""#)
        );
        let unknown = handle("/api", "t=wat&apikey=key", "http://x", "key");
        assert_eq!(unknown.status, 200);
        assert!(unknown.body.contains(r#"<error code="203""#));
        assert!(handle("/api", "apikey=key", "http://x", "key").body.contains(r#"<error code="200""#));
    }

    #[test]
    fn search_terms_for_each_type() {
        let term = |kind: &str, q: &str| search_term(kind, &parse_query(q));

        assert_eq!(term("search", "q=the+matrix").as_deref(), Some("the matrix"));
        assert_eq!(term("tvsearch", "q=Some+Show&season=1&ep=2").as_deref(), Some("Some Show S01E02"));
        assert_eq!(term("tvsearch", "q=Some+Show&season=3").as_deref(), Some("Some Show S03"));
        assert_eq!(term("tvsearch", "season=1&ep=2").as_deref(), Some("S01E02"));
        assert_eq!(term("movie", "q=Heat").as_deref(), Some("Heat"));
        // ids alone cant be matched: an empty answer, not recent releases
        assert_eq!(term("tvsearch", "tvdbid=121361&season=1&ep=1"), None);
        assert_eq!(term("movie", "imdbid=0133093"), None);
        // an id next to a name searches the name
        assert_eq!(term("movie", "imdbid=0133093&q=The+Matrix").as_deref(), Some("The Matrix"));
        // no q at all = recent releases, like t=search
        assert_eq!(term("music", "").as_deref(), Some(""));
    }

    #[test]
    fn caps_lists_every_search_type() {
        let caps = handle("/api", "t=caps", "http://x", "key").body;
        for kind in ["<search ", "<tv-search ", "<movie-search ", "<audio-search ", "<book-search "] {
            assert!(caps.contains(&format!(r#"{kind}available="yes""#)), "{kind} in {caps}");
        }
    }

    #[test]
    fn rss_reports_a_known_password_only() {
        use crate::archive::Password;
        let row = |id: i64| crate::search::ReleaseRow {
            id,
            name: format!("r{id}"),
            group_name: "g".into(),
            poster: None,
            posted_date: None,
            size: None,
            complete: true,
            parts: None,
        };
        let passwords = HashMap::from([(1, Password::Yes), (2, Password::Maybe)]);
        let body = rss(
            "http://x",
            "k",
            0,
            &[row(1), row(2), row(3)],
            &passwords,
            &HashMap::new(),
            &HashMap::new(),
            &Default::default(),
        )
        .body;
        assert!(body.contains(r#"<newznab:attr name="password" value="1"/>"#), "{body}");
        assert!(body.contains(r#"<newznab:attr name="password" value="2"/>"#), "{body}");
        assert_eq!(body.matches("name=\"password\"").count(), 2);
        assert_eq!(body.matches("name=\"category\"").count(), 3);
    }

    #[test]
    fn rss_reports_the_category_and_defaults_to_8000() {
        let row = |id: i64| crate::search::ReleaseRow {
            id,
            name: format!("r{id}"),
            group_name: "g".into(),
            poster: None,
            posted_date: None,
            size: None,
            complete: true,
            parts: None,
        };
        let categories = HashMap::from([(1, 5040)]);
        let body = rss(
            "http://x",
            "k",
            0,
            &[row(1), row(2)],
            &HashMap::new(),
            &categories,
            &HashMap::new(),
            &Default::default(),
        )
        .body;
        assert!(body.contains(r#"name="category" value="5040""#), "{body}");
        assert!(body.contains(r#"name="category" value="8000""#), "{body}");
    }

    #[test]
    fn caps_lists_real_categories() {
        let body = handle("/api", "t=caps", "http://x", "k").body;
        for want in [
            r#"<category id="5000" name="TV">"#,
            r#"<subcat id="5040" name="HD"/>"#,
            r#"<category id="7000" name="Books">"#,
            r#"<category id="8000" name="Other">"#,
        ] {
            assert!(body.contains(want), "{want}: {body}");
        }
        for (start, _) in body.match_indices("supportedParams=\"") {
            let params = &body[start + 17..];
            let params = &params[..params.find('"').unwrap()];
            assert!(params.split(',').any(|p| p == "cat"), "{params}");
        }
    }

    #[test]
    fn caps_reflects_what_is_on() {
        use crate::media::query::{IdParams, IdSet};
        let none = caps_body("http://x", &IdParams::default());
        assert!(
            none.contains(r#"<tv-search available="yes" supportedParams="q,season,ep,cat,limit,offset"/>"#),
            "{none}"
        );
        let set = IdSet { tvdb: true, imdb: true, rid: true, tvmaze: true, ..IdSet::default() };
        let tvmaze = caps_body("http://x", &IdParams { tv: set, movie: IdSet::default() });
        assert!(tvmaze.contains("offset,tvdbid,imdbid,rid,tvmazeid"), "{tvmaze}");
        let all = IdSet { tvdb: true, tvmaze: true, trakt: true, tmdb: true, imdb: true, rid: true };
        let movie = IdSet { imdb: true, tmdb: true, trakt: true, ..IdSet::default() };
        let every = caps_body("http://x", &IdParams { tv: all, movie });
        assert!(every.contains("offset,imdbid,tmdbid,traktid"), "{every}");
        for name in ["tvdbid", "imdbid", "rid", "tvmazeid", "tmdbid", "traktid"] {
            assert!(every.contains(name), "{name}");
        }
    }

    #[test]
    fn search_term_finds_a_daily_show() {
        let term = search_term("tvsearch", &parse_query("q=Some+Show&season=2025&ep=11/10"));
        assert_eq!(term.as_deref(), Some("Some Show 2025 11 10"));
    }
}
