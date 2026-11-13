//! Newznab compatible api for prowlarr / sonarr / radarr.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read;
use std::sync::{Arc, Mutex, Once};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tiny_http::{Header, Request, Response, Server};

use crate::config::{Config, get_api_key};
use crate::dates::pub_date;
use crate::db;
use crate::nzb::{build_nzb_checked, build_nzb_file_checked, nzb_filename};
use crate::search::{all_releases, get_release, newest_in_categories, search_all_releases, search_in_categories};
use crate::ui;
pub(crate) mod fresh;

const WORKERS: usize = 4;
/// Reads several GB when little is cached, which on a machine short of memory is every pass, soo not often.
const WARM_EVERY: Duration = Duration::from_secs(3600);

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
    fn xml(status: u16, body: String) -> Self {
        ApiResponse { status, content_type: Some(XML), body, headers: Vec::new(), bytes: None, spool: None, disposition: None, failed: false }
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

    let args = parse_query(query);
    let t = args.get("t").map(String::as_str);

    let storage = match crate::config::checked_storage_config() {
        Ok(storage) => storage,
        Err(_) => return fresh::unavailable(),
    };
    if storage.is_postgres() {
        if t != Some("caps") && args.get("apikey").map(String::as_str) != Some(api_key) {
            return ApiResponse::xml(401, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<error code=\"100\" description=\"Invalid API Key\"/>".into());
        }
        return fresh::handle(&storage, &args, base, api_key);
    }

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
        Some("get") => get_nzb(&args),
        Some("files") => files_list(&args),
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
    let conn = crate::db::open().ok();
    let found = page(conn.as_ref(), offset, limit, wants_incomplete(args), |at, n| {
        crate::media::query::find(ids, filter.as_ref(), at, n).map(|rows| (rows, HashMap::new()))
    });
    match found {
        Ok((listed, _)) if listed.rows.is_empty() => {
            // by name only when the ID matched nothing, not when all it matched is incomplete
            if listed.read == 0
                && args.get("q").is_some_and(|q| !q.trim().is_empty())
                && let Some(term) = search_term(kind, args)
            {
                return search(&term, args, base, api_key);
            }
            rss(base, api_key, offset, &[], &HashMap::new(), &HashMap::new(), &HashMap::new(), on)
        }
        Ok((listed, _)) => {
            let found: Vec<i64> = listed.rows.iter().map(|r| r.id).collect();
            let categories = crate::category::categories_of(&found).unwrap_or_default();
            let media = crate::media::query::attrs_of(&found).unwrap_or_default();
            rss_listed(
                base,
                api_key,
                offset,
                listed,
                &crate::archive::store::passwords(&found),
                &categories,
                &media,
                on,
            )
        }
        Err(e) => {
            eprintln!("api id search failed: {e}");
            ApiResponse::error(900, "Search failed")
        }
    }
}

/// By name, by category, by both, or the newest with neither.
pub(crate) fn find_releases(
    q: &str,
    filter: Option<&crate::category::CatFilter>,
    offset: i64,
    limit: i64,
) -> rusqlite::Result<Vec<crate::search::ReleaseRow>> {
    if crate::word_index::searchable(q)
        && let Some(index) = crate::word_index::ready()
    {
        // every match ranked, not the newest few thousand of each shard
        let (from, count) = (usize::try_from(offset).unwrap_or(0), usize::try_from(limit).unwrap_or(0));
        return db::with_reader(|conn| {
            Ok(index.search(conn, q, filter, &[], crate::word_index::By::Relevance, true, from, count))
        })?
        .map(|hits| hits.rows)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()));
    }
    match (filter, q.trim().is_empty()) {
        (None, true) => all_releases(offset / limit, limit),
        (None, false) => search_all_releases(q, offset / limit, limit),
        (Some(f), true) => newest_in_categories(f, offset, limit).map(|p| p.rows),
        (Some(f), false) => search_in_categories(q, f, offset, limit),
    }
}

/// How far a deep search pages, live and frozen together.
const DEEP_MAX: i64 = 1_000;

/// `deep=1`: the live releases and the frozen ones (`frozen`) together, newest first, with the frozen ones'
/// categories. A page past `DEEP_MAX` is empty.
pub(crate) fn deep_releases(
    q: &str,
    filter: Option<&crate::category::CatFilter>,
    offset: i64,
    limit: i64,
) -> anyhow::Result<(Vec<crate::search::ReleaseRow>, HashMap<i64, i64>)> {
    let want = (offset + limit).min(DEEP_MAX);
    if want <= offset {
        return Ok((Vec::new(), HashMap::new()));
    }
    let live = if crate::word_index::searchable(q)
        && let Some(index) = crate::word_index::ready()
    {
        db::with_reader(|conn| {
            Ok(index.search(conn, q, filter, &[], crate::word_index::By::Newest, true, 0, want as usize))
        })??
        .rows
    } else {
        match (filter, q.trim().is_empty()) {
            (None, true) => all_releases(0, want)?,
            (None, false) => search_all_releases(q, 0, want)?,
            (Some(f), true) => newest_in_categories(f, 0, want)?.rows,
            (Some(f), false) => search_in_categories(q, f, 0, want)?,
        }
    };
    let frozen =
        crate::frozen::search(&crate::paths::database(), q, filter, &crate::frozen::Narrow::default(), want as usize)?;
    let mut categories = HashMap::new();
    let mut seen = std::collections::HashSet::new();
    let mut all: Vec<crate::search::ReleaseRow> = Vec::with_capacity(live.len() + frozen.len());
    for r in live {
        if seen.insert(r.id) {
            all.push(r);
        }
    }
    for h in frozen {
        // a copy left frozen by a crash mid thaw: the live one stands
        if seen.insert(h.row.id) {
            if let Some(c) = h.category {
                categories.insert(h.row.id, c);
            }
            all.push(h.row);
        }
    }
    all.sort_by_key(|r| std::cmp::Reverse(r.id));
    let page = all.into_iter().skip(offset as usize).take(limit as usize).collect();
    Ok((page, categories))
}

fn search(q: &str, args: &HashMap<String, String>, base: &str, api_key: &str) -> ApiResponse {
    let limit = args.get("limit").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(100).clamp(1, 100);
    let offset = args.get("offset").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0).max(0);

    let filter = args.get("cat").and_then(|c| crate::category::parse_filter(c));
    // frozen releases only when asked for: `deep` isnt in the Newznab spec
    let deep = args.get("deep").is_some_and(|v| matches!(v.trim(), "1" | "true" | "yes"));
    let conn = crate::db::open().ok();
    let all = wants_incomplete(args);
    // a deep search reads live and frozen from the top each time: once, as far as the page can read, then sliced
    let mut deep_window: Option<(Vec<crate::search::ReleaseRow>, HashMap<i64, i64>)> = None;
    let found = page(conn.as_ref(), offset, limit, all, |at, n| {
        if deep {
            if deep_window.is_none() {
                let reach = if all { limit } else { limit * COMPLETE_SCAN_PAGES };
                deep_window = Some(deep_releases(q, filter.as_ref(), offset, reach)?);
            }
            let (rows, categories) = deep_window.as_ref().map_or((&[][..], None), |(r, c)| (&r[..], Some(c)));
            let from = usize::try_from(at - offset).unwrap_or(0).min(rows.len());
            let to = from.saturating_add(usize::try_from(n).unwrap_or(0)).min(rows.len());
            Ok((rows[from..to].to_vec(), categories.cloned().unwrap_or_default()))
        } else {
            find_releases(q, filter.as_ref(), at, n).map(|r| (r, HashMap::new())).map_err(anyhow::Error::from)
        }
    });

    match found {
        Ok((mut listed, frozen_categories)) => {
            if !deep {
                listed.total = match_count(q, filter.as_ref());
            }
            let ids: Vec<i64> = listed.rows.iter().map(|release| release.id).collect();
            let mut categories = crate::category::categories_of(&ids).unwrap_or_else(|e| {
                eprintln!("api categories failed: {e}");
                HashMap::new()
            });
            categories.extend(frozen_categories);
            let on = crate::media::query::cached_id_params();
            let media = crate::media::query::attrs_of(&ids).unwrap_or_default();
            rss_listed(base, api_key, offset, listed, &crate::archive::store::passwords(&ids), &categories, &media, &on)
        }
        Err(e) => {
            eprintln!("api search failed: {e}");
            ApiResponse::error(900, "Search failed")
        }
    }
}

/// How many releases match a search, for Newznab's `total`: the full count, the same on every page, which clients
/// page by. Counted the way the search finds them, incomplete ones too, soo it can be more than paging reaches. None
/// where there's no count to be had cheaply (a category filter without the word index): `total` is then what was
/// listed up to this page, as before.
fn match_count(q: &str, filter: Option<&crate::category::CatFilter>) -> Option<i64> {
    if crate::word_index::searchable(q)
        && let Some(index) = crate::word_index::ready()
    {
        return index.count(q, filter, &[]).ok().and_then(|n| i64::try_from(n).ok());
    }
    match filter {
        None if !q.trim().is_empty() => crate::search::count_all_releases(q).ok(),
        _ => None,
    }
}

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

/// Rows to list, each with all of the post it's part of (`release_info::whole_post_with`): what its NZB holds.
/// And whether each is whole (`listed_complete`), worked out once for the filter and the feed.
#[derive(Default)]
struct Listed {
    rows: Vec<crate::search::ReleaseRow>,
    posts: Vec<Option<crate::release_info::SplitPost>>,
    complete: Vec<bool>,
    /// of the complete ones, those whole only once a client repairs them from their PAR2 volumes
    repair: Vec<bool>,
    /// rows the search gave, listed or not: none means nothing matched at all
    read: usize,
    /// how many match in all, every page alike (Newznab's `total`), where the search can count them
    total: Option<i64>,
}

impl Listed {
    fn of(conn: Option<&rusqlite::Connection>, rows: Vec<crate::search::ReleaseRow>) -> Listed {
        let posts: Vec<Option<crate::release_info::SplitPost>> = rows
            .iter()
            .map(|r| conn.and_then(|conn| crate::release_info::whole_post_with(conn, r).ok().flatten()))
            .collect();
        let (complete, repair) =
            rows.iter().zip(&posts).map(|(r, post)| listed_complete(conn, r, post.as_ref())).unzip();
        Listed { read: rows.len(), rows, posts, complete, repair, total: None }
    }
}

/// Whether a row is whole the way the web page shows it (`web::Search::fold_split_post`): all of its post when it's
/// part of one, else its own flag, or its files by the counter rule of now when the flag was saved by an older one.
/// Or whole once repaired: a row missing data its PAR2 volumes can rebuild (`release_info::repair_needed`) is listed,
/// with that said (the second answer), as a client repairs it on its own.
fn listed_complete(
    conn: Option<&rusqlite::Connection>,
    r: &crate::search::ReleaseRow,
    post: Option<&crate::release_info::SplitPost>,
) -> (bool, bool) {
    match post {
        Some(post) => (post.complete, false),
        None if r.complete
            || conn.is_some_and(|conn| crate::release_info::single_post_complete(conn, r.id).unwrap_or(false)) =>
        {
            (true, false)
        }
        None => {
            let repair = conn.is_some_and(|conn| crate::release_info::repair_needed(conn, r.id).unwrap_or(false));
            (repair, repair)
        }
    }
}

/// The most pages of rows one page of complete ones reads through: past it the page comes out short, even though
/// more may follow (more than this many pages of incomplete rows in a row).
const COMPLETE_SCAN_PAGES: i64 = 10;

/// A page of `limit` rows from `offset`, read through `fetch(offset, limit)` (rows, and categories of frozen ones).
/// With `all` that's every row; else only complete ones (`listed_complete`), and the rows past the page are read on,
/// a page at a time, until `limit` are found, `fetch` runs dry or `COMPLETE_SCAN_PAGES` pages were read. Clients
/// take a page short of `limit` for the last one, and during a backfill most new rows aren't complete yet, soo a
/// page fills from further on where it can. The next page, at `offset + limit`, can list again a row this one read
/// on to; a client drops it by its guid.
fn page<E>(
    conn: Option<&rusqlite::Connection>,
    offset: i64,
    limit: i64,
    all: bool,
    mut fetch: impl FnMut(i64, i64) -> Result<(Vec<crate::search::ReleaseRow>, HashMap<i64, i64>), E>,
) -> Result<(Listed, HashMap<i64, i64>), E> {
    if all {
        let (rows, categories) = fetch(offset, limit)?;
        return Ok((Listed::of(conn, rows), categories));
    }
    let mut listed = Listed::default();
    let mut categories = HashMap::new();
    let mut at = offset;
    for _ in 0..COMPLETE_SCAN_PAGES {
        let (rows, found) = fetch(at, limit)?;
        let read = rows.len() as i64;
        listed.read += rows.len();
        let batch = Listed::of(conn, rows);
        for (((r, post), complete), repair) in
            batch.rows.into_iter().zip(batch.posts).zip(batch.complete).zip(batch.repair)
        {
            if listed.rows.len() as i64 >= limit {
                break;
            }
            if complete {
                if let Some(c) = found.get(&r.id) {
                    categories.insert(r.id, *c);
                }
                listed.rows.push(r);
                listed.posts.push(post);
                listed.complete.push(true);
                listed.repair.push(repair);
            }
        }
        at += read;
        if read < limit || listed.rows.len() as i64 >= limit {
            break;
        }
    }
    Ok((listed, categories))
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
    let conn = crate::db::open().ok();
    let listed = Listed::of(conn.as_ref(), releases.to_vec());
    rss_listed(base, api_key, offset, listed, passwords, categories, media, on)
}

#[allow(clippy::too_many_arguments)]
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
    let ids: Vec<i64> = releases.iter().map(|r| r.id).collect();
    let copies = crate::duplicates::counts(&ids).unwrap_or_default();
    let spots = crate::spotnet::store::spots_of(&ids).unwrap_or_default();
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
            let nzb_url = format!("{base}/api?t=get&id={}&apikey={api_key}", r.id);
            // listed only with `incomplete=1` (`wants_incomplete`): said soo, not passed off as whole
            // and one whole once repaired from its PAR2 volumes says that: a client that won't repair can skip it
            let incomplete = if !complete {
                "<newznab:attr name=\"complete\" value=\"0\"/>"
            } else if repair {
                "<newznab:attr name=\"repair\" value=\"1\"/>"
            } else {
                ""
            };
            let size = whole.map_or(r.size.unwrap_or(0), |post| post.size);
            let password = passwords
                .get(&r.id)
                .map_or(String::new(), |p| format!("<newznab:attr name=\"password\" value=\"{}\"/>", p.code()));
            let category = categories.get(&r.id).copied().unwrap_or(crate::category::UNCATEGORIZED);
            let attrs = media.get(&r.id).map_or(String::new(), |a| a.xml(on));
            let attrs = format!("{attrs}{}{incomplete}", release_attrs(r, copies.get(&r.id), spots.get(&r.id)));
            format!(
                "    <item>      <title>{}</title>      <guid isPermaLink=\"false\">{}</guid>      <link>{}</link>      <size>{size}</size>      <pubDate>{}</pubDate>      <enclosure url=\"{}\" type=\"application/x-nzb\" length=\"{size}\"/>      <newznab:attr name=\"category\" value=\"{category}\"/><newznab:attr name=\"size\" value=\"{size}\"/>{attrs}{password}    </item>",
                escape(&r.name),
                r.id,
                escape(&nzb_url),
                pub_date(r.posted_date.as_deref().unwrap_or("")),
                escape(&nzb_url),
            )
        })
        .collect();

    rss_document(base,offset,total,&items)
}

/// Rendering only: all storage and relationship loading precedes this call.
fn rss_document(base:&str,offset:i64,total:i64,items:&str)->ApiResponse {
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

pub(crate) fn get_nzb(args: &HashMap<String, String>) -> ApiResponse {
    let release_id = args.get("id").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0);

    let Ok(Some(release)) = get_release(release_id) else {
        // a frozen release's NZB is built from its frozen row, left frozen (only the whole of it)
        if args.contains_key("file") || args.contains_key("name") {
            return ApiResponse::not_found();
        }
        return match crate::frozen::nzb(&crate::paths::database(), release_id) {
            Ok(Some((release, built))) => nzb_response(&release, release_id, built.xml, &built.gaps),
            Ok(None) => ApiResponse::not_found(),
            Err(e) => {
                eprintln!("api frozen nzb failed: {e}");
                ApiResponse::not_found()
            }
        };
    };

    // `file` isnt in the Newznab spec: one file of the release, and one that isnt there is a miss
    // `name`, neither: one file by its name, as a release page links it
    let built = match (args.get("file"), args.get("name")) {
        (Some(file), _) => file.trim().parse().ok().map_or(Ok(None), |file| build_nzb_file_checked(release_id, file)),
        (None, Some(name)) => crate::nzb::build_nzb_named_checked(release_id, name),
        (None, None) => build_nzb_checked(release_id),
    };
    let Ok(Some(built)) = built else {
        return ApiResponse::not_found();
    };

    nzb_response(&release, release_id, built.xml, &built.gaps)
}

/// `t=files`, not in the Newznab spec: the files a release's NZB lists, each with the `file=N` that `t=get` takes for
/// it alone. Frozen releases have no list: their NZB comes whole only.
fn files_list(args: &HashMap<String, String>) -> ApiResponse {
    let release_id = args.get("id").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0);
    match crate::nzb::file_list(release_id) {
        Ok(Some(files)) => ApiResponse::xml(200, files_xml(release_id, &files)),
        Ok(None) => ApiResponse::error(300, "No such item"),
        Err(e) => {
            eprintln!("api files failed: {e}");
            ApiResponse::error(900, "Couldn't read the release's files")
        }
    }
}

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

fn nzb_response(
    release: &crate::search::ReleaseRow,
    release_id: i64,
    content: String,
    gaps: &[crate::nzb::FileGap],
) -> ApiResponse {
    let filename = nzb_filename(&release.name, Some(release_id)).replace(['"', '\\'], "_");
    let mut headers = vec![("Content-Disposition".into(), format!("attachment; filename=\"{filename}\""))];
    // not in the Newznab spec: a client can tell an NZB short of segments without parsing it
    if !gaps.is_empty() {
        headers.push(("X-Atlas-Incomplete".into(), gaps.len().to_string()));
    }

    ApiResponse {
        status: 200,
        content_type: Some("application/x-nzb"),
        body: content,
        headers,
        bytes: None,
        spool: None,
        disposition: None,
        failed: false,
    }
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

fn respond(mut request: Request, api_key: &str, fallback_host: &str) {
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
    let resp = if is_get {
        crate::web::get(path, query, cookies.as_deref(), &base, api_key)
            .unwrap_or_else(|| handle(path, query, &base, api_key))
    } else if *request.method() == tiny_http::Method::Post && crate::web::takes_post(path) {
        let mut body = String::new();
        let _ = request.as_reader().take(MAX_FORM_BODY + 1).read_to_string(&mut body);
        let too_long = body.len() as u64 > MAX_FORM_BODY;
        let htmx = request.headers().iter().any(|h| h.field.equiv("HX-Request"));
        let csrf = request
            .headers()
            .iter()
            .find(|h| h.field.equiv(crate::web_settings::CSRF_HEADER))
            .map(|h| h.value.as_str().to_string());
        if too_long {
            ApiResponse {
                status: 413,
                content_type: None,
                body: String::new(),
                headers: Vec::new(),
                bytes: None,
                spool: None,
                disposition: None,
                failed: false,
            }
        } else {
            crate::web::post(path, &body, cookies.as_deref(), htmx, csrf.as_deref(), &base, api_key)
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
    let sent = resp.spool.as_ref().map_or_else(||resp.bytes.as_ref().map_or(resp.body.len(), Vec::len) as u64, crate::nzb_spool::Spool::len);
    let status = resp.status.to_string();
    let labels = [("endpoint", endpoint.label), ("status", status.as_str())];
    crate::metrics::names::API_REQUESTS.inc(&labels, 1);
    crate::metrics::names::API_BYTES.inc(&[("endpoint", endpoint.label)], sent);
    crate::metrics::function_bytes(endpoint.function, "sent", sent);

    if let Some(spool) = resp.spool {
        let _ = crate::nzb_http::send(request, spool, resp.disposition, resp.status,
            resp.content_type.unwrap_or("text/html; charset=utf-8"), resp.headers);
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

/// Start the api on a few worker threads. Returns false when it couldnt bind.
pub fn start(config: &mut Config) -> bool {
    // Activation remains closed while the full fresh native UI/cache/runtime
    // qualification is incomplete. Do this before binding or legacy warmers.
    if match crate::config::checked_storage_config() { Ok(storage) => storage.is_postgres(), Err(_) => true } {
        ui::print("[yellow]native Web UI is awaiting full reader and runtime qualification[/yellow]");
        return false;
    }
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
    crate::word_index::start(crate::paths::database());
    start_warming();
    true
}

/// One thread however often the api restarts, idle while it's stopped.
fn start_warming() {
    static WARMING: Once = Once::new();
    WARMING.call_once(|| {
        thread::spawn(|| {
            loop {
                if RUNNING.lock().unwrap_or_else(|e| e.into_inner()).is_some()
                    && let Err(e) = db::open().and_then(|conn| db::warm_search_tables(&conn))
                {
                    eprintln!("warming the search tables failed: {e}");
                }
                thread::sleep(WARM_EVERY);
            }
        });
        thread::spawn(|| {
            crate::web::keep_sort_windows_warm(|| RUNNING.lock().unwrap_or_else(|e| e.into_inner()).is_some());
        });
    });
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
        let r = handle("/api", "t=caps", "http://x", "key");
        assert_eq!(r.status, 200);
        assert!(r.body.contains("<caps>"));

        let r = handle("/api", "t=search&apikey=wrong", "http://x", "key");
        assert_eq!(r.status, 401);
        assert!(r.body.contains("Invalid API Key"));

        assert_eq!(handle("/nope", "t=caps", "http://x", "key").status, 404);
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

    fn slice(rows: &[crate::search::ReleaseRow], at: i64, n: i64) -> Vec<crate::search::ReleaseRow> {
        rows.iter().skip(at as usize).take(n as usize).cloned().collect()
    }

    /// issue #232: an incomplete release isn't listed unless asked for, and a page reads on for complete ones
    #[test]
    fn a_page_lists_complete_releases_and_reads_on_for_them() {
        let rows = rows_every(25, 3);
        let mut asked = Vec::new();
        let (listed, _) = page::<()>(None, 0, 5, false, |at, n| {
            asked.push(at);
            Ok((slice(&rows, at, n), HashMap::new()))
        })
        .unwrap();
        assert_eq!(listed.rows.iter().map(|r| r.id).collect::<Vec<_>>(), [3, 6, 9, 12, 15]);
        assert_eq!(listed.posts.len(), 5);
        assert_eq!(asked, [0, 5, 10], "read on till five were found, no further");
        assert_eq!(listed.read, 15);

        // `incomplete=1`: the page as it comes, complete or not
        let (listed, _) = page::<()>(None, 5, 5, true, |at, n| Ok((slice(&rows, at, n), HashMap::new()))).unwrap();
        assert_eq!(listed.rows.iter().map(|r| r.id).collect::<Vec<_>>(), [6, 7, 8, 9, 10]);

        // the rows run out: a short page, which tells the client there's no more
        let (listed, _) = page::<()>(None, 20, 5, false, |at, n| Ok((slice(&rows, at, n), HashMap::new()))).unwrap();
        assert_eq!(listed.rows.iter().map(|r| r.id).collect::<Vec<_>>(), [21, 24]);
    }

    #[test]
    fn a_page_of_complete_releases_reads_so_far_at_most() {
        let rows = rows_every(10_000, 10_000);
        let mut asked = 0;
        let (listed, _) = page::<()>(None, 0, 100, false, |at, n| {
            asked += 1;
            Ok((slice(&rows, at, n), HashMap::new()))
        })
        .unwrap();
        assert!(listed.rows.is_empty());
        assert_eq!(listed.read, 1000, "rows matched, none whole: an ID search doesn't fall back to a name");
        assert_eq!(asked, COMPLETE_SCAN_PAGES);
    }

    #[test]
    fn a_frozen_rows_category_stays_with_it_when_listed() {
        let rows = rows_every(4, 2);
        let categories = HashMap::from([(1, 7000), (2, 5040)]);
        let (listed, kept) =
            page::<()>(None, 0, 10, false, |at, n| Ok((slice(&rows, at, n), categories.clone()))).unwrap();
        assert_eq!(listed.rows.iter().map(|r| r.id).collect::<Vec<_>>(), [2, 4]);
        assert_eq!(kept, HashMap::from([(2, 5040)]));
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
            read: 2,
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
            read: 2,
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
