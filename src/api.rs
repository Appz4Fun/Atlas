//! Newznab compatible api for prowlarr / sonarr / radarr.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tiny_http::{Header, Request, Response, Server};

use crate::config::{Config, get_api_key};
use crate::dates::pub_date;
use crate::ui;
pub(crate) mod fresh;

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
    /// sent in place of `body`, for what isn't text
    pub bytes: Option<Vec<u8>>,
    pub spool: Option<crate::nzb_spool::Spool>,
    pub disposition: Option<crate::nzb_http::Disposition>,
    /// atlas itself failed, whatever the status says: Newznab errors go out as 200
    pub failed: bool,
}

impl ApiResponse {
    pub(crate) fn xml(status: u16, body: String) -> Self {
        ApiResponse {
            status,
            content_type: Some(XML),
            body,
            headers: Vec::new(),
            bytes: None,
            spool: None,
            disposition: None,
            failed: false,
        }
    }

    pub(crate) fn not_found() -> Self {
        ApiResponse {
            status: 404,
            content_type: None,
            body: String::new(),
            headers: Vec::new(),
            bytes: None,
            spool: None,
            disposition: None,
            failed: false,
        }
    }

    /// newznab error, sent with http 200 like real indexers do
    fn error(code: u16, description: &str) -> Self {
        let mut response = ApiResponse::xml(
            200,
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<error code=\"{code}\" description=\"{description}\"/>"
            ),
        );
        // 1xx and 2xx are about the request; 900 is atlas's own
        response.failed = code >= 900;
        response
    }
}

struct Running {
    server: Arc<Server>,
    workers: Vec<JoinHandle<()>>,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
// An API restart cannot move its selected PostgreSQL destination. A changed
// destination requires a fresh process.
static PROCESS_STORAGE: OnceLock<crate::pg::StorageConfig> = OnceLock::new();

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
    if path != "/api" {
        return ApiResponse::not_found();
    }

    let storage = match crate::config::checked_storage_config() {
        Ok(storage) => storage,
        Err(_) => return fresh::unavailable(),
    };
    handle_with_storage(path, query, base, api_key, &storage)
}

fn handle_with_storage(
    path: &str,
    query: &str,
    base: &str,
    api_key: &str,
    storage: &crate::pg::StorageConfig,
) -> ApiResponse {
    if path != "/api" {
        return ApiResponse::not_found();
    }
    let args = parse_query(query);
    let t = args.get("t").map(String::as_str);

    if t != Some("caps") && args.get("apikey").map(String::as_str) != Some(api_key) {
        return ApiResponse::xml(
            401,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<error code=\"100\" description=\"Invalid API Key\"/>".into(),
        );
    }
    fresh::handle(storage, &args, base, api_key)
}

/// id parameters that alone mean nothing to search by name
const ID_PARAMS: [&str; 8] = ["tvdbid", "rid", "tvrageid", "tvmazeid", "imdbid", "tmdbid", "traktid", "doubanid"];

/// What to search for. `tvsearch` adds `S01E02` style season/episode terms.
/// None when the request only has ids and nothing to search by name.
pub fn search_term(kind: &str, args: &HashMap<String, String>) -> Option<String> {
    let mut term = args.get("q").map(|q| q.trim().to_string()).unwrap_or_default();
    // an NZBLNK in q searches for its header, the part that names the post
    if let Some(header) = crate::nzblnk::search_text(&term) {
        term = header;
    }

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

/// How far a deep search pages, live and frozen together.

/// What a name says and how many copies a release has, as Newznab attributes. Words other indexers use where
/// they exist: `team` for the group, `video` and `audio` for codecs.
fn release_attrs(
    r: &crate::search::ReleaseRow,
    copies: Option<&crate::duplicates::CopyCount>,
    spot: Option<&crate::spotnet::store::Tagged>,
) -> String {
    let tags = crate::release_tags::parse(&r.name);
    let mut out = String::new();
    let mut add = |name: &str, value: &str| {
        if !value.is_empty() {
            out.push_str(&format!(
                "<newznab:attr name=\"{name}\" value=\"{}\"/>",
                escape(value).replace('"', "&quot;")
            ));
        }
    };
    add("poster", r.poster.as_deref().unwrap_or_default());
    add("group", &r.group_name);
    add("resolution", tags.resolution.as_deref().unwrap_or_default());
    add("video", tags.codec.as_deref().unwrap_or_default());
    add("audio", &tags.audio.join(", "));
    add("hdr", &tags.hdr.join(", "));
    add("source", tags.quality.as_deref().unwrap_or_default());
    add("language", &tags.languages.join(", "));
    add("team", tags.group.as_deref().unwrap_or_default());
    if let Some(copies) = copies {
        add("xposts", &copies.xposts.to_string());
        add("twins", &copies.twins.to_string());
        add("alts", &copies.alts.to_string());
    }
    // a release saved from a Spotnet spot: `tag` is the attribute indexers flag releases with
    if let Some(spot) = spot {
        add("tag", "spot");
        add("spotter", spot.spotter_id.as_deref().unwrap_or_default());
    }
    out
}

/// Which rows get an item: a post whose articles went to several groups, a row for each, or an upload linked by its
/// PAR2 set, is one item, the way the web page lists it, and each of its rows' NZBs is all of it anyway. The row that
/// stands for the post when it's on the page, else the first of its rows that is. `posts[i]` is `rows[i]`'s post.
#[cfg(test)]
fn one_item_per_post(
    rows: &[crate::search::ReleaseRow],
    posts: &[Option<crate::release_info::SplitPost>],
) -> Vec<bool> {
    let on_page: std::collections::HashSet<i64> = rows.iter().map(|r| r.id).collect();
    let mut given = std::collections::HashSet::new();
    rows.iter()
        .zip(posts)
        .map(|(r, post)| match post {
            None => true,
            Some(post) => {
                let listed = post.listed_id();
                (r.id == listed || !on_page.contains(&listed)) && given.insert(listed)
            }
        })
        .collect()
}

/// `incomplete=1` (not in the Newznab spec): releases known to miss files or parts are listed too, each with
/// `<newznab:attr name="complete" value="0"/>`. Without it they are left out, as the web page's "Hide incomplete"
/// leaves them out: a client takes every item it's given for an NZB it can download whole (issue #232).
fn wants_incomplete(args: &HashMap<String, String>) -> bool {
    args.get("incomplete").is_some_and(|v| matches!(v.trim(), "1" | "true" | "yes"))
}

/// The most pages of rows one page of complete ones reads through: past it the page comes out short, even though
/// more may follow (more than this many pages of incomplete rows in a row).
const COMPLETE_SCAN_PAGES: i64 = 10;

#[allow(clippy::too_many_arguments)]

struct RssItemFacts<'a> {
    size: i64,
    complete: bool,
    repair: bool,
    category: i64,
    password: Option<&'a crate::archive::Password>,
    media: Option<&'a crate::media::query::MediaAttrs>,
    copies: Option<&'a crate::duplicates::CopyCount>,
    spot: Option<&'a crate::spotnet::store::Tagged>,
}
/// Presentation only: no storage/provider or relationship resolution here.
fn rss_item(
    base: &str,
    api_key: &str,
    r: &crate::search::ReleaseRow,
    on: &crate::media::query::IdParams,
    facts: RssItemFacts<'_>,
) -> String {
    let size = facts.size;
    let nzb_url = format!("{base}/api?t=get&id={}&apikey={api_key}", r.id);
    // listed only with `incomplete=1` (`wants_incomplete`): said soo, not passed off as whole
    // and one whole once repaired from its PAR2 volumes says that: a client that won't repair can skip it
    let incomplete = if !facts.complete {
        "<newznab:attr name=\"complete\" value=\"0\"/>"
    } else if facts.repair {
        "<newznab:attr name=\"repair\" value=\"1\"/>"
    } else {
        ""
    };
    let password =
        facts.password.map_or(String::new(), |p| format!("<newznab:attr name=\"password\" value=\"{}\"/>", p.code()));
    let category = facts.category;
    let attrs = facts.media.map_or(String::new(), |a| a.xml(on));
    let attrs = format!("{attrs}{}{incomplete}", release_attrs(r, facts.copies, facts.spot));
    format!(
        "    <item>      <title>{}</title>      <guid isPermaLink=\"false\">{}</guid>      <link>{}</link>      <size>{size}</size>      <pubDate>{}</pubDate>      <enclosure url=\"{}\" type=\"application/x-nzb\" length=\"{size}\"/>      <newznab:attr name=\"category\" value=\"{category}\"/><newznab:attr name=\"size\" value=\"{size}\"/>{attrs}{password}    </item>",
        escape(&r.name),
        r.id,
        escape(&nzb_url),
        pub_date(r.posted_date.as_deref().unwrap_or("")),
        escape(&nzb_url),
    )
}

#[allow(clippy::too_many_arguments)]

/// Rendering only: all storage and relationship loading precedes this call.
fn rss_document(base: &str, offset: i64, total: i64, items: &str) -> ApiResponse {
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
        total,
    );

    ApiResponse::xml(200, xml)
}

#[cfg(test)]
fn files_xml(release_id: i64, files: &[crate::nzb::ListedFile]) -> String {
    let mut out =
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<files id=\"{release_id}\" count=\"{}\">\n", files.len());
    for f in files {
        let name =
            f.name.as_deref().map(|n| format!(" name=\"{}\"", escape(n).replace('"', "&quot;"))).unwrap_or_default();
        let _ = writeln!(out, "  <file n=\"{}\"{name} size=\"{}\" articles=\"{}\"/>", f.n, f.bytes, f.articles);
    }
    out.push_str("</files>\n");
    out
}

/// What a request is called in metrics: only names from this list, soo no client can make new series.
struct Endpoint {
    label: &'static str,
    function: &'static str,
}

fn endpoint_of(path: &str, query: &str) -> Endpoint {
    let named = |label, function| Endpoint { label, function };
    if path != "/api" {
        return named("other", "api.other");
    }
    match parse_query(query).get("t").map(String::as_str) {
        Some("caps") => named("caps", "api.caps"),
        Some("get") => named("get", "api.get"),
        Some("files") => named("files", "api.files"),
        Some("search") => named("search", "api.search"),
        Some("tvsearch") => named("tvsearch", "api.tvsearch"),
        Some("movie") => named("movie", "api.movie"),
        Some("music" | "audio") => named("music", "api.music"),
        Some("book") => named("book", "api.book"),
        _ => named("other", "api.other"),
    }
}

/// A small form, not an upload: a key, or an Ask AI question of 500 characters percent-encoded. Anything longer is
/// turned away whole, never read in part.
const MAX_FORM_BODY: u64 = 8192;

fn read_form(request: &mut Request) -> Result<String, u16> {
    let declared = request.body_length();
    if declared.is_some_and(|length| length as u64 > MAX_FORM_BODY) {
        return Err(413);
    }
    let mut body = Vec::with_capacity(MAX_FORM_BODY as usize + 1);
    let read = request.as_reader().take(MAX_FORM_BODY + 1).read_to_end(&mut body);
    if body.len() as u64 > MAX_FORM_BODY {
        return Err(413);
    }
    if read.is_err() {
        return Err(if request.control().is_some_and(|control| control.body_limit_exceeded()) { 413 } else { 400 });
    }
    if declared.is_some_and(|length| length != body.len()) {
        return Err(400);
    }
    String::from_utf8(body).map_err(|_| 400)
}

fn respond(request: Request, api_key: &str, fallback_host: &str, storage: &crate::pg::StorageConfig) {
    respond_with_assets(request, api_key, fallback_host, storage, None)
}
fn respond_with_assets(
    mut request: Request,
    api_key: &str,
    fallback_host: &str,
    storage: &crate::pg::StorageConfig,
    assets: Option<&crate::pg::fresh_assets::AssetManifest>,
) {
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));

    let host = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Host"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_else(|| fallback_host.to_string());
    let base = format!("http://{host}");

    let cookies = request.headers().iter().find(|h| h.field.equiv("Cookie")).map(|h| h.value.as_str().to_string());

    let is_get = matches!(request.method(), tiny_http::Method::Get | tiny_http::Method::Head);
    let endpoint = endpoint_of(path, query);
    let span = crate::metrics::span(endpoint.function);
    let resp = if crate::config::checked_storage_config().as_ref().ok() != Some(storage) {
        fresh::unavailable()
    } else if is_get {
        crate::web::get_with_storage_and_assets(path, query, cookies.as_deref(), &base, api_key, storage, assets)
            .unwrap_or_else(|| handle_with_storage(path, query, &base, api_key, storage))
    } else if *request.method() == tiny_http::Method::Post && crate::web::takes_post(path) {
        let form = read_form(&mut request);
        let htmx = request.headers().iter().any(|h| h.field.equiv("HX-Request"));
        let csrf = request
            .headers()
            .iter()
            .find(|h| h.field.equiv(crate::web_settings::CSRF_HEADER))
            .map(|h| h.value.as_str().to_string());
        if let Err(status) = form {
            ApiResponse {
                status,
                content_type: None,
                body: String::new(),
                headers: Vec::new(),
                bytes: None,
                spool: None,
                disposition: None,
                failed: false,
            }
        } else {
            crate::web::post_with_storage(
                path,
                &form.unwrap(),
                cookies.as_deref(),
                htmx,
                csrf.as_deref(),
                &base,
                api_key,
                storage,
            )
            .unwrap_or_else(ApiResponse::not_found)
        }
    } else {
        ApiResponse {
            status: 405,
            content_type: None,
            body: String::new(),
            headers: Vec::new(),
            bytes: None,
            spool: None,
            disposition: None,
            failed: false,
        }
    };
    if resp.status >= 500 || resp.failed {
        span.failed();
    }
    drop(span);
    let sent = resp
        .spool
        .as_ref()
        .map_or_else(|| resp.bytes.as_ref().map_or(resp.body.len(), Vec::len) as u64, crate::nzb_spool::Spool::len);
    let status = resp.status.to_string();
    let labels = [("endpoint", endpoint.label), ("status", status.as_str())];
    crate::metrics::names::API_REQUESTS.inc(&labels, 1);
    crate::metrics::names::API_BYTES.inc(&[("endpoint", endpoint.label)], sent);
    crate::metrics::function_bytes(endpoint.function, "sent", sent);

    if let Some(spool) = resp.spool {
        let _ = crate::nzb_http::send(
            request,
            spool,
            resp.disposition,
            resp.status,
            resp.content_type.unwrap_or("text/html; charset=utf-8"),
            resp.headers,
        );
        return;
    }
    let mut response = match resp.bytes {
        Some(bytes) => Response::from_data(bytes),
        None => Response::from_string(resp.body),
    }
    .with_status_code(resp.status);

    let mut headers = resp.headers;
    headers.push(("Content-Type".into(), resp.content_type.unwrap_or("text/html; charset=utf-8").into()));

    for (k, v) in headers {
        if let Ok(h) = Header::from_bytes(k.as_bytes(), v.as_bytes()) {
            response.add_header(h);
        }
    }

    let _ = request.respond(response);
}

/// Called only after listener/runtime admission. A configured selection is
/// verified against its actual root and whole manifest file before binding;
/// missing configuration grants no file access and never fetches assets.
fn selected_asset_manifest(
    selection: Option<&crate::config::NativeAssetsConfig>,
) -> anyhow::Result<Option<Arc<crate::pg::fresh_assets::AssetManifest>>> {
    selection
        .map(crate::pg::fresh_assets::AssetManifest::from_selection)
        .transpose()
        .map(|manifest| manifest.map(Arc::new))
}

/// Start the api on a few worker threads. Returns false when it couldnt bind.
pub fn start(config: &mut Config) -> bool {
    let storage = match crate::config::checked_storage_config() {
        Ok(storage) => storage,
        Err(_) => {
            ui::print("[yellow]storage configuration unavailable; API not started[/yellow]");
            return false;
        }
    };
    start_for_storage(config, &storage)
}

/// The native bootstrap supplies its already selected backend. A changed
/// configuration refuses startup rather than choosing another storage engine.
pub(crate) fn start_for_storage(config: &mut Config, storage: &crate::pg::StorageConfig) -> bool {
    start_for_storage_with_assets(config, storage, None)
}
/// The bootstrap may supply a real root/manifest-checked retained-asset reader.
/// Its presence never bypasses native listener/runtime qualification.
pub(crate) fn start_for_storage_with_assets(
    config: &mut Config,
    storage: &crate::pg::StorageConfig,
    assets: Option<Arc<crate::pg::fresh_assets::AssetManifest>>,
) -> bool {
    if crate::config::checked_storage_config().as_ref().ok() != Some(storage) {
        ui::print("[yellow]storage configuration changed; API not started[/yellow]");
        return false;
    }
    if PROCESS_STORAGE.get().is_some_and(|bound| bound != storage) {
        ui::print("[yellow]storage backend or destination changed; restart Atlas before starting the API[/yellow]");
        return false;
    }
    // Activation remains closed before binding or any legacy initializer.
    if storage.is_postgres() {
        ui::print("[yellow]native Web UI is awaiting full reader and runtime qualification[/yellow]");
        return false;
    }
    let assets = if assets.is_none() {
        match selected_asset_manifest(config.native_assets.as_ref()) {
            Ok(assets) => assets,
            Err(_) => {
                ui::print("[yellow]selected native asset root or manifest unavailable; API not started[/yellow]");
                return false;
            }
        }
    } else {
        assets
    };
    let (api_key, created) = get_api_key(config);

    if created {
        ui::print(&format!("[yellow]api key: {api_key}[/yellow]"));
    }

    let port = config.api_port();
    let host = config.api_host.clone();
    let bind_host = if host.contains(':') && !host.starts_with('[') { format!("[{host}]") } else { host.clone() };
    let addr = format!("{bind_host}:{port}");

    let listener = Server::http_bounded(
        &addr,
        tiny_http::RequestLimits {
            max_connections: WORKERS * 2,
            read_idle: Duration::from_secs(5),
            write_idle: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
            max_header_bytes: 65536,
            max_header_count: 128,
            max_request_line_bytes: 65536,
            max_body_wire_bytes: 65536,
        },
    );
    let server = match listener {
        Ok(s) => Arc::new(s),
        Err(_) => {
            ui::print(&format!("[red]port {port} is already in use, api not started[/red]"));
            ui::print("[dim]change it under Settings -> Change api port[/dim]");
            return false;
        }
    };

    if PROCESS_STORAGE.get_or_init(|| storage.clone()) != storage {
        ui::print("[yellow]storage backend or destination changed; API not started[/yellow]");
        return false;
    }

    ui::print(&format!("[dim]newznab api on http://{addr} — apikey required for search/get[/dim]"));

    let workers = (0..WORKERS)
        .map(|_| {
            let server = server.clone();
            let key = api_key.clone();
            let fallback = addr.clone();
            let storage = storage.clone();
            let assets = assets.clone();
            thread::spawn(move || {
                for request in server.incoming_requests() {
                    respond_with_assets(request, &key, &fallback, &storage, assets.as_deref());
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
    #[derive(Default)]
    struct Listed {
        rows: Vec<crate::search::ReleaseRow>,
        posts: Vec<Option<crate::release_info::SplitPost>>,
        complete: Vec<bool>,
        /// of the complete ones, those whole only once a client repairs them from their PAR2 volumes
        repair: Vec<bool>,
        /// how many match in all, every page alike (Newznab's `total`), where the search can count them
        total: Option<i64>,
    }

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
        let listed = Listed {
            rows: releases.to_vec(),
            posts: vec![None; releases.len()],
            complete: releases.iter().map(|row| row.complete).collect(),
            repair: vec![false; releases.len()],
            total: None,
        };
        rss_listed(base, api_key, offset, listed, passwords, categories, media, on)
    }

    fn rss_listed(
        base: &str,
        api_key: &str,
        offset: i64,
        listed: Listed,
        passwords: &HashMap<i64, crate::archive::Password>,
        categories: &HashMap<i64, i64>,
        media: &HashMap<i64, crate::media::query::MediaAttrs>,
        on: &crate::media::query::IdParams,
    ) -> ApiResponse {
        let Listed { rows: releases, posts: wholes, complete, repair, total, .. } = listed;
        // never less than this page reaches
        let total = total.unwrap_or(0).max(offset + releases.len() as i64);
        let releases = &releases[..];
        // what the NZB holds: all of a post cut across groups, or of an upload linked by its PAR2 set
        let keep = one_item_per_post(releases, &wholes);
        let items: String = releases
            .iter()
            .zip(wholes)
            .zip(complete)
            .zip(repair)
            .zip(keep)
            .filter(|(_, keep)| *keep)
            .map(|((((r, whole), complete), repair), _)| {
                rss_item(
                    base,
                    api_key,
                    r,
                    on,
                    RssItemFacts {
                        size: whole.map_or(r.size.unwrap_or(0), |post| post.size),
                        complete,
                        repair,
                        category: categories.get(&r.id).copied().unwrap_or(crate::category::UNCATEGORIZED),
                        password: passwords.get(&r.id),
                        media: media.get(&r.id),
                        copies: None,
                        spot: None,
                    },
                )
            })
            .collect();

        rss_document(base, offset, total, &items)
    }

    #[test]
    fn only_atlass_own_errors_count_as_failed() {
        assert!(ApiResponse::error(900, "Search failed").failed);
        assert_eq!(ApiResponse::error(900, "Search failed").status, 200);
        assert!(!ApiResponse::error(200, "Missing parameter (t)").failed);
        assert!(!ApiResponse::error(100, "Incorrect user credentials").failed);
    }

    #[test]
    fn a_client_cant_invent_an_endpoint_label() {
        assert_eq!(endpoint_of("/api", "t=movie&q=x").label, "movie");
        assert_eq!(endpoint_of("/api", "t=audio").label, "music");
        assert_eq!(endpoint_of("/api", "t=whatever").label, "other");
        assert_eq!(endpoint_of("/some/path", "t=movie").label, "other");
    }

    #[test]
    fn query_parsing() {
        let a = parse_query("t=search&q=the+matrix%20reloaded&q=ignored&apikey=a%2Bb&bad=%zz");
        assert_eq!(a["q"], "the matrix reloaded");
        assert_eq!(a["apikey"], "a+b");
        assert_eq!(a["bad"], "%zz");
    }

    #[test]
    fn files_lists_each_files_number_and_escapes_names() {
        use crate::nzb::ListedFile;
        let xml = files_xml(
            7,
            &[
                ListedFile { n: 1, name: Some("a&b <1> \" n=\"9.mkv".into()), bytes: 15, articles: 2 },
                ListedFile { n: 2, name: None, bytes: 3, articles: 1 },
            ],
        );
        assert!(xml.contains("<files id=\"7\" count=\"2\">"), "{xml}");
        assert!(
            xml.contains("<file n=\"1\" name=\"a&amp;b &lt;1&gt; &quot; n=&quot;9.mkv\" size=\"15\" articles=\"2\"/>"),
            "{xml}"
        );
        assert!(xml.contains("<file n=\"2\" size=\"3\" articles=\"1\"/>"), "{xml}");
        assert_eq!(handle("/api", "t=files&id=1", "http://x", "key").status, 401, "it needs the key");
    }

    #[test]
    fn caps_needs_no_key_and_others_do() {
        let r = ApiResponse::xml(200, caps_body("http://x", &Default::default()));
        assert_eq!(r.status, 200);
        assert!(r.body.contains("<caps>"));

        let r = handle("/api", "t=search&apikey=wrong", "http://x", "key");
        assert_eq!(r.status, 401);
        assert!(r.body.contains("Invalid API Key"));

        assert_eq!(handle("/nope", "t=caps", "http://x", "key").status, 404);
        assert!(
            caps_body("http://192.168.1.172:9091", &Default::default()).contains(r#"url="http://192.168.1.172:9091""#)
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
        let caps = caps_body("http://x", &Default::default());
        for kind in ["<search ", "<tv-search ", "<movie-search ", "<audio-search ", "<book-search "] {
            assert!(caps.contains(&format!(r#"{kind}available="yes""#)), "{kind} in {caps}");
        }
    }

    #[test]
    fn a_spot_release_is_tagged_spot() {
        let row = crate::search::ReleaseRow { id: 1, name: "Movie (2026)".into(), ..Default::default() };
        let spot = crate::spotnet::store::Tagged { spotter_id: Some("baTFcw".into()), ..Default::default() };
        let attrs = release_attrs(&row, None, Some(&spot));
        assert!(attrs.contains(r#"<newznab:attr name="tag" value="spot"/>"#), "{attrs}");
        assert!(attrs.contains(r#"<newznab:attr name="spotter" value="baTFcw"/>"#), "{attrs}");
        assert!(!release_attrs(&row, None, None).contains("spot"));
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
    fn a_post_cut_across_groups_is_one_item() {
        use crate::release_info::SplitPost;
        let row = |id: i64| crate::search::ReleaseRow { id, name: "Some.Film-GRP".into(), ..Default::default() };
        let post = |ids: &[i64]| {
            Some(SplitPost {
                ids: ids.to_vec(),
                groups: vec!["a".into(), "b".into()],
                size: 1,
                complete: false,
                lead: None,
                recovery: Vec::new(),
                beside: Vec::new(),
            })
        };
        // the post of rows 1, 2 and 4 stands as row 4, its highest id; row 3 is a post of its own
        let rows = [row(1), row(2), row(3), row(4)];
        let posts = [post(&[1, 2, 4]), post(&[1, 2, 4]), None, post(&[1, 2, 4])];
        assert_eq!(one_item_per_post(&rows, &posts), [false, false, true, true]);
        // its row isn't on this page: the first of the post's rows that is stands for it, once
        let rows = [row(1), row(2), row(3)];
        let posts = [post(&[1, 2, 4]), post(&[1, 2, 4]), None];
        assert_eq!(one_item_per_post(&rows, &posts), [true, false, true]);
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
        let body = caps_body("http://x", &Default::default());
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
    fn an_nzblnk_in_q_searches_its_header() {
        let q = format!("t=search&q={}", crate::web::url_encode("nzblnk:?t=Some+Film&h=Some.Film.2024.2160p&g=a.b.x"));
        assert_eq!(search_term("search", &parse_query(&q)).as_deref(), Some("Some.Film.2024.2160p"));
        let q = format!("t=movie&q={}", crate::web::url_encode("nzblnk:?t=T"));
        assert_eq!(search_term("movie", &parse_query(&q)).as_deref(), Some("nzblnk:?t=T"), "no h, not a link");
    }

    #[test]
    fn search_term_finds_a_daily_show() {
        let term = search_term("tvsearch", &parse_query("q=Some+Show&season=2025&ep=11/10"));
        assert_eq!(term.as_deref(), Some("Some Show 2025 11 10"));
    }

    /// Rows 1..=n, every `every`th of them complete, read as a search would hand them out.
    fn rows_every(n: i64, every: i64) -> Vec<crate::search::ReleaseRow> {
        (1..=n)
            .map(|id| crate::search::ReleaseRow {
                id,
                name: format!("r{id}"),
                complete: id % every == 0,
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn incomplete_is_asked_for_by_name() {
        assert!(!wants_incomplete(&parse_query("t=search&q=x")));
        assert!(!wants_incomplete(&parse_query("t=search&incomplete=0")));
        assert!(wants_incomplete(&parse_query("t=search&incomplete=1")));
        assert!(wants_incomplete(&parse_query("t=search&incomplete=true")));
    }

    /// listed with `incomplete=1`, an incomplete release says so
    #[test]
    fn rss_flags_an_incomplete_release() {
        let rows = rows_every(2, 2);
        let listed = Listed {
            rows,
            posts: vec![None, None],
            complete: vec![false, true],
            repair: vec![false, false],
            total: None,
        };
        let body = rss_listed(
            "http://x",
            "k",
            0,
            listed,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &Default::default(),
        )
        .body;
        assert_eq!(body.matches(r#"<newznab:attr name="complete" value="0"/>"#).count(), 1, "{body}");
        let first = body.split("<item>").nth(1).unwrap();
        assert!(first.contains("<title>r1</title>") && first.contains(r#"name="complete" value="0""#), "{first}");
    }

    /// one whole once repaired from its PAR2 volumes is listed as complete, and says it needs the repair
    #[test]
    fn rss_flags_a_release_that_needs_a_repair() {
        let listed = Listed {
            rows: rows_every(2, 2),
            posts: vec![None, None],
            complete: vec![true, true],
            repair: vec![true, false],
            total: None,
        };
        let body = rss_listed(
            "http://x",
            "k",
            0,
            listed,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &Default::default(),
        )
        .body;
        assert!(!body.contains(r#"name="complete""#), "{body}");
        let items: Vec<&str> = body.split("<item>").skip(1).collect();
        assert!(items[0].contains(r#"<newznab:attr name="repair" value="1"/>"#), "{}", items[0]);
        assert!(!items[1].contains(r#"name="repair""#), "{}", items[1]);
    }
}

#[cfg(test)]
mod native_form_body_tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::{Shutdown, TcpStream},
        thread,
    };
    fn exchange(length: usize, body: &[u8], half_close: bool) -> Option<u16> {
        let server = Server::http_bounded(
            "127.0.0.1:0",
            tiny_http::RequestLimits {
                max_connections: 2,
                read_idle: Duration::from_millis(150),
                write_idle: Duration::from_secs(1),
                request_timeout: Duration::from_secs(2),
                max_header_bytes: 4096,
                max_header_count: 32,
                max_request_line_bytes: 4096,
                max_body_wire_bytes: 16384,
            },
        )
        .unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let worker = thread::spawn(move || {
            let mut request = server.recv_timeout(Duration::from_secs(2)).unwrap().expect("owned POST request");
            let status = match read_form(&mut request) {
                Ok(_) => 200,
                Err(status) => status,
            };
            let response = Response::empty(status).with_header(Header::from_bytes("Connection", "close").unwrap());
            let _ = request.respond(response);
        });
        let mut client = TcpStream::connect(address).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        write!(
            client,
            "POST /login HTTP/1.1\r\nHost: fixture.invalid\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        client.write_all(body).unwrap();
        client.flush().unwrap();
        if half_close {
            client.shutdown(Shutdown::Write).unwrap();
        }
        let mut response = String::new();
        client.take(16384).read_to_string(&mut response).unwrap();
        worker.join().unwrap();
        response.split_whitespace().nth(1).map(|status| status.parse().unwrap())
    }
    #[test]
    fn truncated_declared_form_cannot_publish_a_valid_login_prefix() {
        assert_eq!(exchange(2048, b"key=valid", true), Some(400));
    }
    #[test]
    fn quiet_incomplete_form_expires_without_accepting_its_login_prefix() {
        assert!(matches!(exchange(2048, b"key=valid", false), None | Some(400)));
    }
    #[test]
    fn invalid_utf8_form_is_rejected_as_a_whole() {
        assert_eq!(exchange(2, &[b'x', 0xff], true), Some(400));
    }
    #[test]
    fn complete_small_form_remains_accepted() {
        assert_eq!(exchange(9, b"key=valid", true), Some(200));
    }
    #[test]
    fn complete_streamed_form_keeps_the_declared_body() {
        let mut body = b"key=valid&padding=".to_vec();
        body.resize(2048, b'x');
        assert_eq!(exchange(body.len(), &body, true), Some(200));
    }
    #[test]
    fn complete_form_at_the_byte_limit_remains_accepted() {
        let mut body = b"key=valid&padding=".to_vec();
        body.resize(8192, b'x');
        assert_eq!(exchange(body.len(), &body, true), Some(200));
    }
    #[test]
    fn declared_oversize_form_is_rejected_without_accepting_a_partial_body() {
        assert_eq!(exchange(8193, b"", true), Some(413));
    }
    #[test]
    fn invalid_utf8_after_a_valid_key_never_becomes_a_login_prefix() {
        let body = b"key=valid&note=\xff";
        assert_eq!(exchange(body.len(), body, true), Some(400));
    }
    #[test]
    fn rejected_complete_form_keeps_the_next_pipelined_body_distinct() {
        let server = Server::http_bounded(
            "127.0.0.1:0",
            tiny_http::RequestLimits {
                max_connections: 2,
                read_idle: Duration::from_millis(150),
                write_idle: Duration::from_secs(1),
                request_timeout: Duration::from_secs(2),
                max_header_bytes: 4096,
                max_header_count: 32,
                max_request_line_bytes: 4096,
                max_body_wire_bytes: 16384,
            },
        )
        .unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let worker = thread::spawn(move || {
            let mut accepted = Vec::new();
            for _ in 0..2 {
                let mut request = server.recv_timeout(Duration::from_secs(2)).unwrap().expect("owned pipelined POST");
                let (status, body) = match read_form(&mut request) {
                    Ok(body) => (200, Some(body)),
                    Err(status) => (status, None),
                };
                accepted.push(body);
                request.respond(Response::empty(status)).unwrap();
            }
            accepted
        });
        let mut client = TcpStream::connect(address).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        client.write_all(b"POST /login HTTP/1.1\r\nHost: fixture.invalid\r\nContent-Length: 2\r\n\r\nx\xffPOST /login HTTP/1.1\r\nHost: fixture.invalid\r\nContent-Length: 9\r\nConnection: close\r\n\r\nkey=valid").unwrap();
        client.flush().unwrap();
        let mut response = String::new();
        client.take(16384).read_to_string(&mut response).unwrap();
        let statuses = response
            .lines()
            .filter_map(|line| line.strip_prefix("HTTP/1.1 ").and_then(|rest| rest.split_whitespace().next()))
            .collect::<Vec<_>>();
        assert_eq!(statuses, ["400", "200"], "{response}");
        assert_eq!(
            worker.join().unwrap(),
            [None, Some("key=valid".into())],
            "invalid bytes never become accepted form data or consume a following request"
        );
    }
    #[test]
    fn truncated_stream_flushes_rejection_then_closes_instead_of_reusing_its_remainder() {
        let server = Server::http_bounded(
            "127.0.0.1:0",
            tiny_http::RequestLimits {
                max_connections: 2,
                read_idle: Duration::from_millis(150),
                write_idle: Duration::from_secs(1),
                request_timeout: Duration::from_secs(2),
                max_header_bytes: 4096,
                max_header_count: 32,
                max_request_line_bytes: 4096,
                max_body_wire_bytes: 16384,
            },
        )
        .unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let worker = thread::spawn(move || {
            let mut request = server.recv_timeout(Duration::from_secs(2)).unwrap().expect("owned truncated POST");
            let status = match read_form(&mut request) {
                Ok(_) => 200,
                Err(status) => status,
            };
            // No explicit response Connection header: the bounded transport
            // must force close for the incomplete request on its own.
            request.respond(Response::empty(status)).unwrap();
            assert!(
                server.recv_timeout(Duration::from_millis(200)).unwrap().is_none(),
                "a remainder cannot become a new request"
            );
        });
        let mut client = TcpStream::connect(address).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        client.write_all(b"POST /login HTTP/1.1\r\nHost: fixture.invalid\r\nContent-Length: 2048\r\n\r\nkey=validGET /unexpected HTTP/1.1\r\nHost: fixture.invalid\r\n\r\n").unwrap();
        client.flush().unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        client.take(16384).read_to_string(&mut response).unwrap();
        worker.join().unwrap();
        assert_eq!(response.split_whitespace().nth(1), Some("400"), "{response}");
        assert!(response.to_ascii_lowercase().contains("connection: close"), "{response}");
        assert_eq!(response.lines().filter(|line| line.starts_with("HTTP/1.1 ")).count(), 1, "{response}");
    }

    fn chunked_exchange(wire: &[u8]) -> (Option<u16>, Option<String>) {
        let server = Server::http_bounded(
            "127.0.0.1:0",
            tiny_http::RequestLimits {
                max_connections: 2,
                read_idle: Duration::from_millis(150),
                write_idle: Duration::from_secs(1),
                request_timeout: Duration::from_secs(2),
                max_header_bytes: 4096,
                max_header_count: 32,
                max_request_line_bytes: 4096,
                max_body_wire_bytes: 16384,
            },
        )
        .unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let worker = thread::spawn(move || {
            let mut request = server.recv_timeout(Duration::from_secs(2)).unwrap().expect("owned chunked POST");
            let (status, body) = match read_form(&mut request) {
                Ok(body) => (200, Some(body)),
                Err(status) => (status, None),
            };
            request
                .respond(Response::empty(status).with_header(Header::from_bytes("Connection", "close").unwrap()))
                .unwrap();
            body
        });
        let mut client = TcpStream::connect(address).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        client.write_all(b"POST /login HTTP/1.1\r\nHost: fixture.invalid\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
        client.write_all(wire).unwrap();
        client.flush().unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        client.take(16384).read_to_string(&mut response).unwrap();
        (response.split_whitespace().nth(1).map(|status| status.parse().unwrap()), worker.join().unwrap())
    }
    #[test]
    fn chunked_form_requires_the_complete_chunk_and_terminal_chunk() {
        for wire in [b"1000\r\nkey=valid".as_slice(), b"9\r\nkey=valid\r\n".as_slice(), b"9\r\nkey=vali".as_slice()] {
            assert_eq!(chunked_exchange(wire), (Some(400), None), "unfinished framing cannot publish a valid prefix");
        }
        assert_eq!(chunked_exchange(b"9\r\nkey=valid\r\n0\r\n\r\n"), (Some(200), Some("key=valid".into())));
    }
    #[test]
    fn complete_chunked_invalid_utf8_cannot_publish_a_login_prefix() {
        let body = b"key=valid&note=\xff";
        let wire = [format!("{:x}\r\n", body.len()).into_bytes(), body.to_vec(), b"\r\n0\r\n\r\n".to_vec()].concat();
        assert_eq!(chunked_exchange(&wire), (Some(400), None));
    }

    #[test]
    fn chunked_delimiter_eof_flushes_a_rejection_without_accepting_the_form_prefix() {
        for wire in [
            b"9\r\nkey=valid".as_slice(),
            b"9\r\nkey=valid\r".as_slice(),
            b"9\r\nkey=valid\r\n0\r".as_slice(),
            b"9\r\nkey=valid\r\n0\r\n".as_slice(),
            b"9\r\nkey=valid\r\n0\r\n\r".as_slice(),
        ] {
            assert_eq!(
                chunked_exchange(wire),
                (Some(400), None),
                "delimiter EOF must reject the whole form and flush before closing"
            );
        }
    }
}

#[cfg(all(test, unix))]
mod native_asset_selection_tests {
    use super::*;
    use sha2::{Digest, Sha256};
    #[test]
    fn selected_manifest_is_verified_and_absence_grants_nothing() {
        assert!(selected_asset_manifest(None).unwrap().is_none());
        let root = tempfile::tempdir().unwrap();
        let evidence = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("posters")).unwrap();
        std::fs::write(root.path().join("posters/original.jpg"), b"retained original bytes").unwrap();
        let manifest = evidence.path().join("assets.jsonl");
        crate::pg::testing::capture_asset_manifest(
            root.path(),
            &manifest,
            crate::pg::testing::AssetCaptureLimits {
                min_files: 1,
                max_files: 8,
                max_file_bytes: 1 << 20,
                max_total_bytes: 8 << 20,
                read_bytes_per_second: 32 << 20,
            },
        )
        .unwrap();
        let digest = Sha256::digest(std::fs::read(&manifest).unwrap())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let selection = crate::config::NativeAssetsConfig {
            root: root.path().canonicalize().unwrap().to_str().unwrap().into(),
            manifest: manifest.canonicalize().unwrap().to_str().unwrap().into(),
            whole_sha256: digest,
        };
        assert!(selected_asset_manifest(Some(&selection)).unwrap().is_some());
        let mismatched = crate::config::NativeAssetsConfig { whole_sha256: "00".repeat(32), ..selection.clone() };
        assert!(selected_asset_manifest(Some(&mismatched)).is_err());
        let missing = crate::config::NativeAssetsConfig {
            manifest: evidence.path().join("missing.jsonl").to_str().unwrap().into(),
            ..selection
        };
        assert!(selected_asset_manifest(Some(&missing)).is_err());
        assert!(!root.path().join("posters/missing.jsonl").exists());
    }
}

#[cfg(all(test, unix))]
#[path = "api/native_asset_http_tests.rs"]
mod native_asset_http_tests;
