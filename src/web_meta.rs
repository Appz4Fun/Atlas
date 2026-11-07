//! The release page's six fact panels (Names, Where and when, Posted by, Inside, Health, Keys), their three
//! lazy fragments (Headers, Parts, Hashes) and the results drawer. Render-only: it reads what Atlas already
//! stores and fetches nothing. Not rendered here, by agreement with the threads that own them: stream facts
//! (per-track CodecPrivate contents included), HDR, chapters, attachments, cover, the NZB file list and
//! muxed_at (#48); encoder, SegmentUID, editions, default flags, runtime (stream-details); the stills strip;
//! the Size cell and the recovery warning (#64); copy cards (twins).
//!
//! Privacy: the posting account, posting host and IP a server put in the headers are never printed and never
//! indexed raw. They appear as salted keys only, soo two releases from one account can be lined up without
//! anyone reading the account.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write;
use std::sync::LazyLock;

use chrono::Datelike;
use regex::Regex;
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::api::{ApiResponse, escape};
use crate::archive::store::{InnerFileRow, InnerView, SOURCE_7Z, SOURCE_NZB, SOURCE_PAR2, SOURCE_RAR, SOURCE_ZIP};
use crate::keys::Kind;
use crate::naming::NameSource;
use crate::probe::yenc_quirks;
use crate::release_info::{self, Contents, Names};
use crate::search::{ArticleRow, ReleaseRow};
use crate::store::{self, FileInfo, SegmentPick, SegmentRef};
use crate::ui::{fmt_date, fmt_size};
use crate::web::{COLUMNS, html, panel, tag, thousands, verdict};

/// The most files a page reads of a release; past it the counts say "or more".
const FILES_CAP: usize = 5_000;
/// Rows the Parts fragment lists.
const PARTS_ROWS: usize = 200;
/// Cells in a part map.
const MAP_CELLS: i64 = 64;
/// Articles a fragment reads before it stops and says the release is too big to list.
const ARTICLE_BUDGET: usize = 250_000;
/// `label: value` pairs read from an NFO, at most.
const NFO_FIELDS_MAX: usize = 8;
/// Links read from an NFO, at most.
const NFO_LINKS_MAX: usize = 8;

/// What the panels render from, all of it already loaded by the release page.
pub(crate) struct MetaInputs<'a> {
    pub release: &'a ReleaseRow,
    pub inner: &'a InnerView,
    pub names: Option<&'a Names>,
    pub contents: &'a Contents,
    pub probe: Option<&'a crate::probe::ProbeInfo>,
}

/// The six panels in reading order; a panel with nothing to say is left out. `conn` serves the facts that
/// need a query (files, segments, fingerprints, PreDB, keys); without one they are skipped.
pub(crate) fn meta_html(conn: Option<&Connection>, i: &MetaInputs<'_>) -> String {
    let mut l = look(conn, i.release.id);
    if let Some(conn) = conn {
        l.same = same_counts(conn, i, &l);
        l.points_to = nzb_targets(conn, i);
        l.sidecars = sidecars_of(conn, i);
    }
    panels_html(i, &l)
}

/// What the release can be looked up by, from what the page already read: the keys `release_keys` keeps (worked
/// out here too, soo a release the keys step hasn't reached yet counts the same), its PAR2 sets and its
/// OpenSubtitles hash.
fn lookup_of(i: &MetaInputs<'_>, l: &Looked) -> crate::keys::Lookup {
    let timing: Vec<(Option<i64>, i64)> = l.timing.iter().map(|t| (t.lines, t.articles)).collect();
    let shard = shard_conn(i.release.id);
    crate::keys::Lookup {
        keys: crate::keys::keys_of(i.probe, &timing, i.release.parts, l.salt),
        par2_sets: shard
            .as_ref()
            .map(|s| crate::keys::par2_sets(s, i.release.id).unwrap_or_default())
            .unwrap_or_default(),
        moviehash: i.probe.and_then(|p| p.moviehash.clone()),
        files: shard
            .as_ref()
            .and_then(|s| crate::keys::crc_files(s, i.release.id, i.probe).ok())
            .unwrap_or_else(|| i.probe.and_then(crate::keys::probe_file).into_iter().collect()),
        inner_files: shard.as_ref().map(|s| crate::keys::inner_crc_files(s, i.release.id)).unwrap_or_default(),
        archive: shard.as_ref().map(|s| crate::keys::archive_keys(s, i.release.id)).unwrap_or_default(),
    }
}

/// How many other releases share each key the release has, the kinds with none left out.
fn same_counts(conn: &Connection, i: &MetaInputs<'_>, l: &Looked) -> Vec<(crate::keys::Kind, i64)> {
    let lookup = lookup_of(i, l);
    crate::keys::Kind::ALL
        .into_iter()
        .filter(|kind| lookup.has(*kind))
        .map(|kind| (kind, crate::keys::count_others(conn, i.release.id, &lookup, kind)))
        .filter(|(_, n)| *n > 0)
        .collect()
}

/// " · in 12 other releases", linking to them, when other releases share the key.
fn same_link(l: &Looked, id: i64, kind: crate::keys::Kind) -> String {
    let Some((_, n)) = l.same.iter().find(|(k, _)| *k == kind) else { return String::new() };
    let count = if *n >= crate::keys::COUNT_CAP {
        format!("{} or more other releases", thousands(*n))
    } else {
        format!("{} other release{}", thousands(*n), plural(*n as usize))
    };
    format!(" · <a href=\"/ui/same?id={id}&amp;kind={}\">in {count}</a>", kind.word())
}

/// The six panels over what was looked up.
fn panels_html(i: &MetaInputs<'_>, l: &Looked) -> String {
    let id = i.release.id;
    let has_files = !l.files.is_empty();
    let has_headers = l.first_id.is_some() || i.probe.is_some_and(|p| !p.headers.is_empty());
    let mut out = String::new();
    out.push_str(&names_panel(i, l, false).finish("Names", None));
    out.push_str(&where_panel(i, l, false).finish("Where and when", None));
    out.push_str(
        &posted_by_panel(i, l, false).finish("Posted by", has_headers.then(|| details(id, "headers", "Headers"))),
    );
    out.push_str(&inside_panel(i, l, false).finish("Inside", None));
    out.push_str(&health_panel(i, l, false).finish("Health", has_files.then(|| details(id, "parts", "Parts"))));
    out.push_str(&keys_panel(i, l, false).finish("Keys", has_files.then(|| details(id, "hashes", "Hashes"))));
    out
}

/// `/ui/meta?id=&part=headers|parts|hashes`: the body of one lazy `<details>`.
pub(crate) fn fragment(args: &HashMap<String, String>) -> ApiResponse {
    let id = arg_id(args);
    let part = args.get("part").map(String::as_str).unwrap_or("");
    if !matches!(part, "headers" | "parts" | "hashes") {
        return html(400, String::new());
    }
    let Ok(conn) = crate::db::open() else { return html(500, String::new()) };
    // the reader the page, the NZB and the drawer use: a release in a blocked group, or none, is not served
    match crate::search::get_release_with(&conn, id) {
        Ok(Some(_)) => {}
        Ok(None) => return html(404, String::new()),
        Err(_) => return html(500, String::new()),
    }
    let body = match part {
        "headers" => headers_fragment(&conn, id),
        "parts" => parts_fragment(&conn, id),
        _ => hashes_fragment(&conn, id),
    };
    html(200, body)
}

/// `/ui/row?id=&as=row|card`: one `<tr class="drawer-row">` (default) or one `<div class="drawer-card">`.
pub(crate) fn row_panel(args: &HashMap<String, String>) -> ApiResponse {
    let id = arg_id(args);
    let as_card = args.get("as").is_some_and(|v| v == "card");
    let Ok(conn) = crate::db::open() else { return html(500, String::new()) };
    html(200, drawer_html(&conn, id, as_card))
}

/// The button a result row carries in its name cell: the first click loads the drawer under the row.
pub(crate) fn drawer_trigger(id: i64) -> String {
    trigger(&format!("/ui/row?id={id}"), "closest tr", "afterend")
}

/// The `+` on a copy card or a copy link: the first click appends a `<div class="drawer-card">` inside `target`, an
/// htmx target such as `closest li`, soo the facts open under the copy they belong to.
pub(crate) fn card_trigger(id: i64, target: &str) -> String {
    trigger(&format!("/ui/row?id={id}&amp;as=card"), target, "beforeend")
}

/// Every click reaches htmx until a drawer is there: web.js folds and unfolds a loaded drawer before htmx sees the
/// click, soo there is one fetch per drawer, and a failed one is tried again by the next click. While a fetch is in
/// flight, a second click is dropped rather than fetched twice.
fn trigger(url: &str, target: &str, swap: &str) -> String {
    format!(
        " <button class=\"icon-button drawer-toggle\" type=\"button\" title=\"More facts\" aria-label=\"More facts\" aria-expanded=\"false\" hx-get=\"{url}\" hx-target=\"{target}\" hx-swap=\"{swap}\" hx-trigger=\"click\" hx-sync=\"this:drop\">+</button>"
    )
}

fn arg_id(args: &HashMap<String, String>) -> i64 {
    args.get("id").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0)
}

// ---------------------------------------------------------------- what the panels read

/// What the panels read once per page: the files (5,000 cap), the main file's first and last article, the
/// fingerprints, the PreDB match, the shard's id key and the salt. Every field is optional: a missing read
/// skips its facts.
#[derive(Default)]
struct Looked {
    files: Vec<FileInfo>,
    main: Option<FileInfo>,
    /// the main file's first message-id, with its brackets
    first_id: Option<String>,
    last: Option<SegmentRef>,
    posted_fp: Option<Vec<u8>>,
    content_fp: Option<Vec<u8>>,
    predb: Option<PredbFacts>,
    salt: Option<i64>,
    id_key: Option<store::IdKey>,
    /// one row a file, in file name order; empty for a release saved before Atlas kept timing
    timing: Vec<FileTiming>,
    /// how many other releases share each key, full page only; a kind with none is left out
    same: Vec<(crate::keys::Kind, i64)>,
    /// a posted NZB's: the names its files give, each with the releases of that name, full page only
    points_to: Vec<(String, Vec<ReleaseRow>)>,
    /// sidecar posts (an NFO, a mediainfo dump) that describe this release, full page only
    sidecars: Vec<crate::sidecar::Found>,
}

/// One `file_timing` row (`store::FILE_TIMING`): what the overview said about a file's articles, folded at
/// ingest. Dates are the poster's clock with the zone dropped, soo they compare with each other and with
/// nothing else.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FileTiming {
    first_date: Option<i64>,
    last_date: Option<i64>,
    number_first: Option<i64>,
    number_last: Option<i64>,
    lines: Option<i64>,
    references_seen: i64,
    xref_groups_max: Option<i64>,
    /// runs of one From in arrival order: 1 is one From throughout
    from_variants: i64,
    /// the file's last From, hashed: with one run, the file's only From
    from_hash: Option<i64>,
    encoded_subject: bool,
    late_articles: i64,
    /// the articles the row covers
    articles: i64,
    /// what the message-ids said (`discovery::id_facts`): the tool's code, its or the server's clock in ms, and the
    /// Highwinds host, session key and counters
    tool: Option<i64>,
    clock_first: Option<i64>,
    clock_last: Option<i64>,
    hw_host: Option<String>,
    hw_key: Option<String>,
    hw_n_first: Option<i64>,
    hw_n_last: Option<i64>,
}

/// `predb_match` has `title_id, kind, category, pre_at, nuked, matched_at` and no feed, source or reason
/// column, soo these four are all the fact can say.
struct PredbFacts {
    kind: i64,
    category: Option<String>,
    pre_at: Option<i64>,
    nuked: bool,
}

/// The release's shard on its own, for the readers that want one (`release_files`, `file_segments`, `IdKey`).
fn shard_conn(release_id: i64) -> Option<Connection> {
    let main = crate::paths::database();
    crate::db::open_shard(&store::shard_path(&main, store::shard_of_id(release_id))).ok()
}

fn look(conn: Option<&Connection>, release_id: i64) -> Looked {
    let mut l = Looked::default();
    let Some(conn) = conn else { return l };
    if let Some(s) = shard_conn(release_id) {
        l.files = store::release_files(&s, release_id, FILES_CAP).unwrap_or_default();
        l.main = crate::probe::step::main_file(&l.files).cloned();
        if let Some(main) = &l.main {
            // the subject's article is the lowest in (part, message-id) order: what `First(1)` would return,
            // without decoding the file's blob
            l.first_id = s
                .query_row("select subject_mid from files where id = ?", [main.id], |r| r.get::<_, Option<String>>(0))
                .ok()
                .flatten();
            if l.first_id.is_none() {
                l.first_id = store::file_segments(&s, main.id, &SegmentPick::First(1))
                    .ok()
                    .and_then(|found| found.into_iter().next())
                    .map(|seg| seg.message_id);
            }
            l.last = store::file_segments(&s, main.id, &SegmentPick::Last(1))
                .ok()
                .and_then(|found| found.into_iter().next());
        }
        if let Ok((posted, content)) =
            s.query_row("select posted, content from release_fingerprint where release_id = ?", [release_id], |r| {
                Ok((r.get::<_, Option<Vec<u8>>>(0)?, r.get::<_, Option<Vec<u8>>>(1)?))
            })
        {
            l.posted_fp = posted;
            l.content_fp = content;
        }
        l.predb = s
            .query_row(
                "select kind, category, pre_at, nuked from predb_match where release_id = ?",
                [release_id],
                |r| {
                    Ok(PredbFacts {
                        kind: r.get(0)?,
                        category: r.get(1)?,
                        pre_at: r.get(2)?,
                        nuked: r.get::<_, Option<i64>>(3)?.unwrap_or(0) != 0,
                    })
                },
            )
            .ok();
        l.id_key = store::IdKey::of(&s).ok();
        l.timing = read_timing(&s, release_id).unwrap_or_default();
    }
    l.salt = salt(conn);
    l
}

/// The release's `file_timing` rows, by file name, through `files` soo a row of a file that is gone is not one.
fn read_timing(s: &Connection, release_id: i64) -> rusqlite::Result<Vec<FileTiming>> {
    s.prepare_cached(
        "select t.first_date, t.last_date, t.number_first, t.number_last, t.lines, t.references_seen, t.xref_groups_max,
                t.from_variants, t.encoded_subject, t.late_articles, t.articles, t.from_hash,
                t.tool, t.clock_first, t.clock_last, t.hw_host, t.hw_key, t.hw_n_first, t.hw_n_last
         from file_timing t join files f on f.id = t.file_id
         where f.release_id = ?1 order by f.filename limit ?2",
    )?
    .query_map(rusqlite::params![release_id, FILES_CAP as i64], |r| {
        Ok(FileTiming {
            first_date: r.get(0)?,
            last_date: r.get(1)?,
            number_first: r.get(2)?,
            number_last: r.get(3)?,
            lines: r.get(4)?,
            references_seen: r.get(5)?,
            xref_groups_max: r.get(6)?,
            from_variants: r.get(7)?,
            encoded_subject: r.get::<_, i64>(8)? != 0,
            late_articles: r.get(9)?,
            articles: r.get(10)?,
            from_hash: r.get(11)?,
            tool: r.get(12)?,
            clock_first: r.get(13)?,
            clock_last: r.get(14)?,
            hw_host: r.get(15)?,
            hw_key: r.get(16)?,
            hw_n_first: r.get(17)?,
            hw_n_last: r.get(18)?,
        })
    })?
    .collect()
}

/// The install's salt for the account and host keys: made once, by whoever asks first, and kept in shard 0's
/// `meta`. None when it can't be read or made, and then no salted key is shown at all.
/// The install's salt for the account and host keys, for the pages that show headers on their own.
pub(crate) fn salt_of(conn: Option<&Connection>) -> Option<i64> {
    conn.and_then(salt)
}

fn salt(conn: &Connection) -> Option<i64> {
    crate::keys::salt(conn)
}

// ---------------------------------------------------------------- a panel under construction

/// A `dl.facts` under construction: `add` escapes, `add_html` trusts, empty values are dropped.
struct Facts {
    html: String,
    rows: usize,
}

impl Facts {
    fn new() -> Facts {
        Facts { html: String::new(), rows: 0 }
    }

    fn add(&mut self, label: &str, value: Option<impl AsRef<str>>) {
        if let Some(value) = value {
            let value = value.as_ref();
            if !value.is_empty() {
                self.add_html(label, Some(escape(value)));
            }
        }
    }

    fn add_html(&mut self, label: &str, html: Option<String>) {
        if let Some(html) = html.filter(|h| !h.is_empty()) {
            let _ = write!(self.html, "<dt>{label}</dt><dd>{html}</dd>");
            self.rows += 1;
        }
    }

    /// A key or a hash: monospace, copied whole in one click.
    fn key(&mut self, label: &str, text: Option<String>) {
        self.add_html(label, text.map(|t| key_code(&t)));
    }

    /// The panel, with its lazy `<details>` inside when it has one; "" when there is nothing to say.
    fn finish(self, title: &str, details: Option<String>) -> String {
        if self.rows == 0 {
            return String::new();
        }
        let mut out = panel(title, &self.html);
        if let Some(details) = details
            && let Some(close) = out.rfind("</div>")
        {
            out.insert_str(close, &details);
        }
        out
    }
}

fn key_code(text: &str) -> String {
    format!("<code class=\"key\">{}</code>", escape(text))
}

/// The `<details>` shell of a lazy fragment: nothing loads until the reader opens it.
fn details(id: i64, part: &str, title: &str) -> String {
    format!(
        "<details class=\"meta meta-{part}\" hx-get=\"/ui/meta?id={id}&amp;part={part}\" hx-trigger=\"toggle once\" hx-target=\"find .meta-body\" hx-swap=\"innerHTML\"><summary>{title}</summary><div class=\"meta-body\"><p class=\"dim\">Loading…</p></div></details>"
    )
}

// ---------------------------------------------------------------- Names

fn names_panel(i: &MetaInputs<'_>, l: &Looked, compact: bool) -> Facts {
    let mut f = Facts::new();
    let subject = l.main.as_ref().and_then(|m| m.subject.as_deref());
    let subject_name = l.main.as_ref().map(|m| m.filename.as_str()).filter(|n| !n.is_empty());
    let yenc_name = i.probe.and_then(|p| p.yenc.as_ref()).and_then(|y| y.name.as_deref());
    let archive_name = i.probe.and_then(|p| p.archive.as_ref()).and_then(|a| a.name.as_deref()).or_else(|| {
        i.inner.files.iter().find(|r| r.source != SOURCE_PAR2 && r.source != SOURCE_NZB).map(|r| r.name.as_str())
    });
    if !compact {
        f.add("Subject", subject);
        f.add("Subject decoded", subject.and_then(decode_encoded_words));
        f.add("Subject skeleton", subject.map(subject_skeleton));
    }
    f.add("Names", name_agreement(subject_name, yenc_name, archive_name));
    f.add("Obfuscation", Some(obfuscation_fact(i.names, i.inner, &l.files)));
    if compact {
        return f;
    }
    let posted = i.names.map_or(i.release.name.as_str(), |n| n.posted.as_str());
    let tells = repack_tells(posted);
    if !tells.is_empty() {
        f.add("Tells", Some(tells.join(", ")));
    }
    if let Some(nfo) = i.probe.and_then(|p| p.nfo.as_deref()) {
        let links: Vec<String> = nfo_urls(nfo)
            .into_iter()
            .map(|(label, url)| {
                let shown = url.trim_start_matches("https://").trim_start_matches("www.");
                format!("<a href=\"{url}\" rel=\"noopener noreferrer\">{label}: {}</a>", escape(shown))
            })
            .collect();
        if !links.is_empty() {
            f.add_html("NFO links", Some(links.join("<br>")));
        }
        let fields: Vec<String> =
            nfo_fields(nfo).into_iter().map(|(label, value)| format!("{label}: {}", escape(&value))).collect();
        if !fields.is_empty() {
            f.add_html("NFO says", Some(fields.join(" · ")));
        }
        let (width, lines, art) = nfo_shape(nfo);
        if lines > 0 {
            let art = if art { ", CP437 art" } else { "" };
            f.add("NFO shape", Some(format!("{width} columns, {lines} lines{art}")));
        }
        let reports = nfo_reports(nfo);
        if !reports.is_empty() {
            let list: Vec<String> = reports.iter().map(|r| format!("a {r} report")).collect();
            f.add("NFO reports", Some(format!("includes {}", list.join(" and "))));
        }
    }
    let encoded = l.timing.iter().filter(|t| t.encoded_subject).count();
    if encoded > 0 {
        f.add(
            "Encoded subjects",
            Some(format!("{encoded} of {} file{} posted with RFC 2047 words", l.timing.len(), plural(l.timing.len()))),
        );
    }
    if let Some(p) = &l.predb {
        let kind = match p.kind {
            1 => "the exact title",
            2 => "the normalized title",
            3 => "a hash",
            4 => "an inner file name",
            _ => "a match",
        };
        let mut text = format!("matched by {kind}");
        if let Some(category) = p.category.as_deref().filter(|c| !c.is_empty()) {
            let _ = write!(text, ", {}", escape(category));
        }
        if let Some(pre_at) = p.pre_at.filter(|t| *t > 0) {
            let _ = write!(text, ", pre {}", fmt_unix(pre_at));
        }
        if p.nuked {
            text.push_str(&tag("tag-bad", "NUKED"));
        }
        f.add_html("PreDB", Some(text));
    }
    f
}

/// Which of the names were hidden, and from where the real one came back.
fn obfuscation_fact(names: Option<&Names>, inner: &InnerView, files: &[FileInfo]) -> String {
    let mut text = obfuscation_level(names, inner, files);
    let judged: Vec<&FileInfo> = files.iter().filter(|f| !f.filename.is_empty()).collect();
    let hidden = judged.iter().filter(|f| looks_obfuscated(&f.filename)).count();
    if hidden > 0 {
        let verb = if hidden == 1 { "looks" } else { "look" };
        let _ =
            write!(text, "; {hidden} of {} posted file name{} {verb} obfuscated", judged.len(), plural(judged.len()));
    }
    if inner.files.len() > 1 {
        let inner_hidden = inner.files.iter().filter(|r| looks_obfuscated(&r.name)).count();
        if inner_hidden * 2 > inner.files.len() {
            text.push_str("; the names inside are obfuscated too");
        }
    }
    text
}

/// none / subject only / file names only / subject and file names / everything (renamed from …): the subject
/// is the release name the post gave, the file names are the posted ones.
fn obfuscation_level(names: Option<&Names>, inner: &InnerView, files: &[FileInfo]) -> String {
    let subject = names.is_some_and(|n| n.obfuscated || looks_obfuscated(&n.posted));
    let judged: Vec<&str> = files.iter().map(|f| f.filename.as_str()).filter(|n| !n.is_empty()).collect();
    let file_names = !judged.is_empty() && judged.iter().filter(|n| looks_obfuscated(n)).count() * 2 > judged.len();
    let recovered = names.and_then(|n| n.source).filter(|s| {
        [NameSource::Par2, NameSource::Nfo, NameSource::ArchiveInner, NameSource::PreDb].iter().any(|k| *k as i64 == *s)
    });
    let inner_readable = !inner.files.is_empty()
        && inner.files.iter().filter(|r| looks_obfuscated(&r.name)).count() * 2 <= inner.files.len();
    match (subject, file_names) {
        (true, true) => match recovered.and_then(release_info::name_source_label) {
            Some(from) => format!("everything (renamed from {from})"),
            None if inner_readable => "everything (the names inside are readable)".into(),
            None => "subject and file names".into(),
        },
        (true, false) => "subject only".into(),
        (false, true) => "file names only".into(),
        (false, false) => "none".into(),
    }
}

/// The rule behind `releases.is_obfuscated`, on one name.
fn looks_obfuscated(name: &str) -> bool {
    crate::parser::is_obfuscated(name)
}

/// Whether the subject's file name, the yEnc name and the archive's own file name tell one story.
fn name_agreement(subject: Option<&str>, yenc: Option<&str>, archive: Option<&str>) -> Option<String> {
    let subject = subject?;
    let mut differ = Vec::new();
    if let Some(yenc) = yenc
        && !subject.trim().eq_ignore_ascii_case(yenc.trim())
    {
        differ.push("the subject and yEnc names differ");
    }
    if let Some(archive) = archive
        && stem(subject) != stem(archive)
        && !yenc.is_some_and(|y| stem(y) == stem(archive))
    {
        differ.push("the archive's file name differs from the posted name");
    }
    if yenc.is_none() && archive.is_none() {
        return None;
    }
    if !differ.is_empty() {
        return Some(differ.join("; "));
    }
    Some(
        match (yenc.is_some(), archive.is_some()) {
            (true, true) => "subject, yEnc and archive names agree",
            (true, false) => "subject and yEnc names agree",
            _ => "subject and archive names agree",
        }
        .into(),
    )
}

/// A name without its extension, volume suffix and archive extension: `Foo.part01.rar`, `Foo.7z.001` and
/// `Foo.mkv` are all `foo`.
fn stem(name: &str) -> String {
    let mut s = name.trim().to_ascii_lowercase();
    let cut = |s: &mut String, keep: &dyn Fn(&str) -> bool| {
        if let Some(dot) = s.rfind('.')
            && keep(&s[dot + 1..])
        {
            s.truncate(dot);
        }
    };
    // the plain extension, then a volume suffix, then the archive's own extension
    cut(&mut s, &|ext| !ext.is_empty() && ext.len() <= 4 && ext.chars().all(|c| c.is_ascii_alphanumeric()));
    cut(&mut s, &|ext| {
        let digits = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit());
        ext.strip_prefix("part").is_some_and(digits)
            || ext.strip_prefix("vol").is_some_and(|t| t.contains('+'))
            || (ext.len() == 3
                && (digits(ext)
                    || ext.starts_with('r') && digits(&ext[1..])
                    || ext.starts_with('z') && digits(&ext[1..])))
    });
    cut(&mut s, &|ext| matches!(ext, "rar" | "7z" | "zip" | "tar" | "gz" | "xz" | "bz2"));
    s
}

/// The subject with its digits as `#` and its file name as `"…"`: the template the posting tool used.
fn subject_skeleton(subject: &str) -> String {
    static QUOTED: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r#""[^"]*""#).unwrap()
    });
    static COUNTER: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"\d+(\s*(?:/|of)\s*)\d+").unwrap()
    });
    let quoted = QUOTED.replace_all(subject, "\"…\"");
    COUNTER
        .replace_all(&quoted, |hit: &regex::Captures<'_>| {
            hit[0].chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect::<String>()
        })
        .into_owned()
}

/// RFC 2047 encoded words decoded (B and Q, UTF-8 and Latin-1); None when the subject has none.
fn decode_encoded_words(subject: &str) -> Option<String> {
    static WORD: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"=\?([^?\s]+)\?([bBqQ])\?([^?\s]*)\?=").unwrap()
    });
    if !WORD.is_match(subject) {
        return None;
    }
    let mut out = String::new();
    let mut at = 0;
    let mut last_end: Option<usize> = None;
    for hit in WORD.captures_iter(subject) {
        let whole = hit.get(0)?;
        let between = &subject[at..whole.start()];
        // whitespace between two encoded words is not part of the text
        if !(last_end == Some(at) && between.trim().is_empty()) {
            out.push_str(between);
        }
        let charset = hit[1].split('*').next().unwrap_or("").to_ascii_lowercase();
        let bytes = match &hit[2] {
            "b" | "B" => decode_base64(&hit[3])?,
            _ => decode_q(&hit[3]),
        };
        let text = match charset.as_str() {
            "iso-8859-1" | "latin1" | "windows-1252" | "cp1252" | "iso-8859-15" => {
                bytes.iter().map(|b| char::from(*b)).collect()
            }
            _ => String::from_utf8_lossy(&bytes).into_owned(),
        };
        out.push_str(&text);
        at = whole.end();
        last_end = Some(at);
    }
    out.push_str(&subject[at..]);
    Some(out)
}

fn decode_base64(text: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(text.trim_end_matches('=')))
        .ok()
}

/// The Q encoding: `_` is a space, `=XX` a byte. Read byte by byte, never by slicing the text: a stray `=`
/// in front of a multibyte character would otherwise be cut through.
fn decode_q(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let hex = |b: &u8| (*b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'_' => out.push(b' '),
            b'=' => match (bytes.get(i + 1).and_then(hex), bytes.get(i + 2).and_then(hex)) {
                (Some(hi), Some(lo)) => {
                    out.push((hi << 4) | lo);
                    i += 2;
                }
                _ => out.push(b'='),
            },
            b => out.push(b),
        }
        i += 1;
    }
    out
}

/// Status words in a posted name: what the group says about this release against an earlier one.
const TELLS: &[&str] = &[
    "REPACK",
    "RERIP",
    "PROPER",
    "REAL",
    "READNFO",
    "DIRFIX",
    "NFOFIX",
    "SAMPLEFIX",
    "SUBFIX",
    "SYNCFIX",
    "INTERNAL",
    "OBFUSCATED",
    "DUPE",
    "RETAIL",
    "LIMITED",
    "FESTIVAL",
];

fn repack_tells(name: &str) -> Vec<&'static str> {
    let upper = name.to_ascii_uppercase().replace("READ.NFO", "READNFO");
    let mut found: Vec<(usize, &'static str)> = Vec::new();
    for token in upper.split(|c: char| !c.is_ascii_alphanumeric()) {
        if let Some(tell) = TELLS.iter().find(|t| **t == token)
            && !found.iter().any(|(_, t)| t == tell)
            && let Some(at) = upper.find(tell)
        {
            found.push((at, tell));
        }
    }
    found.sort_by_key(|(at, _)| *at);
    found.into_iter().map(|(_, t)| t).collect()
}

/// Links to the title databases an NFO names, rebuilt from the id they carry: nothing from the NFO's own text
/// becomes a link.
fn nfo_urls(text: &str) -> Vec<(&'static str, String)> {
    static TMDB: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"(?i)themoviedb\.org/(movie|tv)/(\d{1,9})").unwrap()
    });
    static TVDB: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"(?i)thetvdb\.com/(?:series/([A-Za-z0-9-]{1,80})|\?tab=series&(?:amp;)?id=(\d{1,9})|dereferrer/series/(\d{1,9}))")
            .unwrap()
    });
    static ANIDB: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"(?i)anidb\.net/(?:anime/|perl-bin/animedb\.pl\?show=anime&(?:amp;)?aid=|a)(\d{1,9})").unwrap()
    });
    static MUSICBRAINZ: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"(?i)musicbrainz\.org/(release|release-group|artist|recording)/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})")
            .unwrap()
    });
    static DISCOGS: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"(?i)discogs\.com/(?:[a-z]{2}/)?(release|master)/(\d{1,12})").unwrap()
    });
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let mut push = |label: &'static str, url: String| {
        if out.len() < NFO_LINKS_MAX && !out.iter().any(|(_, u)| *u == url) {
            out.push((label, url));
        }
    };
    for hit in TMDB.captures_iter(text) {
        push("TMDb", format!("https://www.themoviedb.org/{}/{}", hit[1].to_ascii_lowercase(), &hit[2]));
    }
    for hit in TVDB.captures_iter(text) {
        if let Some(slug) = hit.get(1) {
            push("TVDB", format!("https://thetvdb.com/series/{}", slug.as_str()));
        } else if let Some(id) = hit.get(2).or_else(|| hit.get(3)) {
            push("TVDB", format!("https://thetvdb.com/dereferrer/series/{}", id.as_str()));
        }
    }
    for hit in ANIDB.captures_iter(text) {
        push("AniDB", format!("https://anidb.net/anime/{}", &hit[1]));
    }
    for hit in MUSICBRAINZ.captures_iter(text) {
        push(
            "MusicBrainz",
            format!("https://musicbrainz.org/{}/{}", hit[1].to_ascii_lowercase(), hit[2].to_ascii_lowercase()),
        );
    }
    for hit in DISCOGS.captures_iter(text) {
        push("Discogs", format!("https://www.discogs.com/{}/{}", hit[1].to_ascii_lowercase(), &hit[2]));
    }
    out
}

/// The `Label.....: value` lines of an NFO's template, for the labels worth a row. Never Encoder: that one is
/// the stream thread's.
fn nfo_fields(text: &str) -> Vec<(String, String)> {
    static FIELD: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"(?m)^[^\p{L}\p{N}\n]{0,40}([A-Za-z][A-Za-z /]{1,24}?)[ .:_\-=]{0,60}[:=]\s*([^\r\n]{1,160})$")
            .unwrap()
    });
    const LABELS: &[(&str, &str)] = &[
        ("title", "Title"),
        ("movie", "Title"),
        ("show", "Title"),
        ("name", "Title"),
        ("year", "Year"),
        ("source", "Source"),
        ("video", "Video"),
        ("video codec", "Video"),
        ("codec", "Video"),
        ("resolution", "Resolution"),
        ("audio", "Audio"),
        ("audio codec", "Audio"),
        ("runtime", "Runtime"),
        ("duration", "Runtime"),
        ("length", "Runtime"),
        ("size", "Size"),
        ("file size", "Size"),
        ("filesize", "Size"),
        ("language", "Language"),
        ("languages", "Language"),
        ("lang", "Language"),
        ("subtitles", "Subtitles"),
        ("subtitle", "Subtitles"),
        ("subs", "Subtitles"),
        ("genre", "Genre"),
        ("genres", "Genre"),
        ("format", "Format"),
        ("container", "Format"),
    ];
    let mut out: Vec<(String, String)> = Vec::new();
    for hit in FIELD.captures_iter(text) {
        if out.len() >= NFO_FIELDS_MAX {
            break;
        }
        let label = hit[1].trim().to_ascii_lowercase();
        let Some((_, shown)) = LABELS.iter().find(|(known, _)| *known == label) else { continue };
        if out.iter().any(|(l, _)| l == shown) {
            continue;
        }
        let value = hit[2].trim().trim_end_matches(['│', '║', '|', '┃', '▌', '▐', ' ', '\t']).trim();
        let value: String = value.chars().take(80).collect();
        if value.is_empty() || value.chars().all(|c| !c.is_alphanumeric()) {
            continue;
        }
        out.push(((*shown).to_string(), value));
    }
    out
}

/// The reports pasted into an NFO: "MediaInfo", "BDInfo".
fn nfo_reports(text: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    if text.contains("Complete name") || text.contains("Unique ID") {
        out.push("MediaInfo");
    }
    if text.contains("DISC INFO:") || text.contains("PLAYLIST REPORT") {
        out.push("BDInfo");
    }
    out
}

/// Width in columns, line count, and whether it is CP437 art (box-drawing and block characters).
fn nfo_shape(text: &str) -> (usize, usize, bool) {
    let lines: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    let count = lines.iter().rposition(|l| !l.trim().is_empty()).map_or(0, |last| last + 1);
    let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let art = text.chars().filter(|c| ('\u{2500}'..='\u{25ff}').contains(c)).count() >= 20;
    (width, count, art)
}

// ---------------------------------------------------------------- Where and when

fn where_panel(i: &MetaInputs<'_>, l: &Looked, compact: bool) -> Facts {
    let mut f = Facts::new();
    let headers = i.probe.map(|p| &p.headers);
    let date = headers.and_then(|h| h.get("Date")).map(String::as_str);
    if compact {
        let posted = fmt_date(i.release.posted_date.as_deref());
        f.add("Posted", Some(if posted.is_empty() { date.unwrap_or("") } else { posted.as_str() }));
        return f;
    }
    if let Some(date) = date {
        f.add("Date style", date_style(date));
        f.add("Posted on", posted_on(date));
        let mut skews = Vec::new();
        for other in ["Injection-Date", "NNTP-Posting-Date"] {
            if let Some(value) = headers.and_then(|h| h.get(other))
                && let Some(skew) = skew_secs(date, value)
                && skew.abs() >= 2
            {
                skews.push(format!("{} against {other}", fmt_skew(skew)));
            }
        }
        if !skews.is_empty() {
            f.add("Clock skew", Some(skews.join(", ")));
        }
    }
    if let Some(ts) = l.first_id.as_deref().and_then(id_timestamp) {
        let mut text = fmt_unix(ts);
        if let Some(posted) = date.and_then(crate::dates::posted_timestamp) {
            let _ = write!(text, ", {} against Date", fmt_skew(ts - posted));
        }
        f.add("Id timestamp", Some(text));
    }
    if !l.timing.is_empty() {
        let covered = timed(&l.timing);
        let partial = i.release.parts.filter(|p| *p > covered);
        f.add("First and last article", first_and_last(&l.timing));
        if let Some(parts) = partial {
            f.add(
                "Timing covers",
                Some(format!(
                    "{} of {} articles; the rest were saved before Atlas kept timing",
                    thousands(covered),
                    thousands(parts)
                )),
            );
        }
        // the row's size is every article's: a rate in bytes only when the timing has them all
        f.add("Posting rate", posting_rate(&l.timing, if partial.is_none() { i.release.size } else { None }));
        f.add("File order", file_order(&l.timing));
        f.add("Sessions", sessions(&l.timing));
        f.add("Article numbers", article_numbers(&l.timing));
        f.add("Threaded replies", Some(threaded_replies(&l.timing)));
        f.add("Crossposts per article", crossposts(&l.timing));
        f.add("From variants", from_variants(&l.timing));
        f.add("Posting tool", posting_tool(&l.timing));
        f.add("Id clock", id_clock(&l.timing));
        f.add("Server session", server_session(&l.timing));
    }
    f
}

/// "Nyuu" or "Nyuu, ngPost": the tools the message-ids named, as `discovery` reads them.
fn posting_tool(timing: &[FileTiming]) -> Option<String> {
    let mut tools: Vec<&str> = timing
        .iter()
        .filter_map(|t| t.tool.and_then(crate::discovery::ToolType::from_code))
        .map(|t| t.label())
        .collect();
    tools.sort_unstable();
    tools.dedup();
    (!tools.is_empty()).then(|| tools.join(", "))
}

/// "2026-10-02 10:00:00.123 to 10:04:59.870 UTC by the ids' clock, 5 min": the milliseconds the tool or the server
/// wrote into the message-ids, which the poster can't have set.
fn id_clock(timing: &[FileTiming]) -> Option<String> {
    let first = timing.iter().filter_map(|t| t.clock_first).min()?;
    let last = timing.iter().filter_map(|t| t.clock_last).max()?;
    let fmt =
        |ms: i64| chrono::DateTime::from_timestamp_millis(ms).map(|d| d.format("%Y-%m-%d %H:%M:%S%.3f").to_string());
    let (a, b) = (fmt(first)?, fmt(last)?);
    let secs = (last - first) / 1000;
    let span =
        if secs < 60 { "within a minute".to_string() } else { fmt_skew(secs).trim_start_matches('+').to_string() };
    Some(format!("{a} to {b} UTC by the ids' clock, {span}"))
}

/// "fx09.am4, key JP7, counters 546,176–546,900": the Highwinds session every article came through; more than one
/// key means more than one connection or session posted the files.
fn server_session(timing: &[FileTiming]) -> Option<String> {
    let mut keys: Vec<(String, String)> =
        timing.iter().filter_map(|t| Some((t.hw_host.clone()?, t.hw_key.clone()?))).collect();
    keys.sort_unstable();
    keys.dedup();
    let first = timing.iter().filter_map(|t| t.hw_n_first).min();
    let last = timing.iter().filter_map(|t| t.hw_n_last).max();
    match keys.len() {
        0 => None,
        1 => {
            let (host, key) = &keys[0];
            let counters = match (first, last) {
                (Some(a), Some(b)) if a != b => {
                    format!(", counters {}–{}", thousands(a), thousands(b))
                }
                (Some(a), _) => format!(", counter {}", thousands(a)),
                _ => String::new(),
            };
            Some(format!("{host}, key {key}{counters}"))
        }
        n => Some(format!(
            "{n} sessions: {}",
            keys.iter().map(|(h, k)| format!("{h} key {k}")).collect::<Vec<_>>().join(", ")
        )),
    }
}

// ---------------------------------------------------------------- timing (file_timing, from the overview)

/// A gap between files longer than this opens another posting session.
const SESSION_GAP: i64 = 30 * 60;

/// The poster's clock as the row keeps it: no zone to print.
fn fmt_wall(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0).map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
}

/// The articles the rows cover.
fn timed(timing: &[FileTiming]) -> i64 {
    timing.iter().map(|t| t.articles).sum()
}

/// The first and the last date over the rows, when any has them.
fn date_span(timing: &[FileTiming]) -> Option<(i64, i64)> {
    let first = timing.iter().filter_map(|t| t.first_date).min()?;
    let last = timing.iter().filter_map(|t| t.last_date).max()?;
    Some((first, last.max(first)))
}

/// The files' own [first, last] spans, those with both dates, earliest first.
fn file_spans(timing: &[FileTiming]) -> Vec<(i64, i64)> {
    let mut spans: Vec<(i64, i64)> = timing.iter().filter_map(|t| Some((t.first_date?, t.last_date?))).collect();
    spans.sort_unstable();
    spans
}

/// "2026-10-02 10:11 to 2026-10-02 11:40 by the poster's clock, 1 h"
fn first_and_last(timing: &[FileTiming]) -> Option<String> {
    let (first, last) = date_span(timing)?;
    let span = if last - first < 60 {
        "within a minute".to_string()
    } else {
        fmt_skew(last - first).trim_start_matches('+').to_string()
    };
    Some(format!("{} to {} by the poster's clock, {span}", fmt_wall(first), fmt_wall(last)))
}

/// "41.2 articles a minute, 15.3 MB/s" over a span of a minute or more; `size` is the bytes the rows cover.
fn posting_rate(timing: &[FileTiming], size: Option<i64>) -> Option<String> {
    let (first, last) = date_span(timing)?;
    let secs = last - first;
    let articles = timed(timing);
    if secs < 60 || articles <= 0 {
        return None;
    }
    let mut text = format!("{:.1} articles a minute", articles as f64 * 60.0 / secs as f64);
    if let Some(size) = size.filter(|s| *s > 0) {
        let _ = write!(text, ", {}/s", fmt_size(Some(size / secs)));
    }
    Some(text)
}

/// "4 files one after another" or "interleaved: 2 of 4 files started before an earlier one ended".
fn file_order(timing: &[FileTiming]) -> Option<String> {
    let spans = file_spans(timing);
    if spans.len() < 2 {
        return None;
    }
    let mut end = spans[0].1;
    let mut interleaved = 0;
    for &(first, last) in &spans[1..] {
        if first < end {
            interleaved += 1;
        }
        end = end.max(last);
    }
    Some(if interleaved == 0 {
        format!("{} files one after another", spans.len())
    } else {
        format!("interleaved: {interleaved} of {} files started before an earlier one ended", spans.len())
    })
}

/// "one session" or "3 sessions, gaps over 30 min between files".
fn sessions(timing: &[FileTiming]) -> Option<String> {
    let spans = file_spans(timing);
    if spans.len() < 2 {
        return None;
    }
    let mut count = 1;
    let mut end = spans[0].1;
    for &(first, last) in &spans[1..] {
        if first - end > SESSION_GAP {
            count += 1;
        }
        end = end.max(last);
    }
    Some(if count == 1 { "one session".into() } else { format!("{count} sessions, gaps over 30 min between files") })
}

/// "1,200–1,251 where it was read, adjacent" or ", 12 other articles between them".
fn article_numbers(timing: &[FileTiming]) -> Option<String> {
    let first = timing.iter().filter_map(|t| t.number_first).min()?;
    let last = timing.iter().filter_map(|t| t.number_last).max()?;
    let mut text = format!("{}–{} where it was read", thousands(first), thousands(last));
    let others = last - first + 1 - timed(timing);
    if others <= 0 {
        text.push_str(", adjacent");
    } else {
        let _ = write!(text, ", {} other article{} between them", thousands(others), plural(others as usize));
    }
    Some(text)
}

/// "none" or "3 articles carried References".
fn threaded_replies(timing: &[FileTiming]) -> String {
    let seen: i64 = timing.iter().map(|t| t.references_seen).sum();
    if seen == 0 {
        "none".into()
    } else {
        format!("{} article{} carried References", thousands(seen), plural(seen as usize))
    }
}

/// "one group, by Xref" or "3 groups at most, by Xref"; nothing when the server sent no Xref.
fn crossposts(timing: &[FileTiming]) -> Option<String> {
    let most = timing.iter().filter_map(|t| t.xref_groups_max).max()?;
    Some(if most <= 1 { "one group, by Xref".into() } else { format!("{most} groups at most, by Xref") })
}

/// "one From throughout", "3 Froms, one a file" or "From changed 5 times within files (rotating identity)".
/// The rows count changes inside each file; across files, a file with one run has one From, its hash, soo files
/// that each keep to theirs are told apart by those.
fn from_variants(timing: &[FileTiming]) -> Option<String> {
    if timing.is_empty() {
        return None;
    }
    let changes: i64 = timing.iter().map(|t| (t.from_variants - 1).max(0)).sum();
    if changes > 0 {
        return Some(format!(
            "From changed {changes} time{} within files (rotating identity)",
            plural(changes as usize)
        ));
    }
    let froms: std::collections::HashSet<i64> = timing.iter().filter_map(|t| t.from_hash).collect();
    Some(if froms.len() > 1 { format!("{} Froms, one a file", froms.len()) } else { "one From throughout".into() })
}

/// "128 lines an article, 4,096 bytes a line"; `size` is the bytes the rows cover.
fn lines_per_article(timing: &[FileTiming], size: Option<i64>) -> Option<String> {
    let lines: i64 = timing.iter().filter_map(|t| t.lines).sum();
    let articles = timed(timing);
    if lines <= 0 || articles <= 0 {
        return None;
    }
    let mut text = format!("{} lines an article", thousands(lines / articles));
    if let Some(size) = size.filter(|s| *s > 0) {
        let _ = write!(text, ", {} bytes a line", thousands(size / lines));
    }
    Some(text)
}

/// "none" or "3 arrived after the file sealed".
fn late_articles(timing: &[FileTiming]) -> String {
    let late: i64 = timing.iter().map(|t| t.late_articles).sum();
    if late == 0 { "none".into() } else { format!("{} arrived after the file sealed", thousands(late)) }
}

/// A key over the per-file Lines sums in file name order: a post made again with the same line length keys
/// the same, whatever its subjects say. None unless the rows cover every article of the release (`parts`) and
/// every file has lines: a sum over part of a file, or a file with no row, would key a different post.
fn lines_key(timing: &[FileTiming], parts: Option<i64>) -> Option<String> {
    let files: Vec<(Option<i64>, i64)> = timing.iter().map(|t| (t.lines, t.articles)).collect();
    crate::keys::lines_text(&files, parts).map(|t| key16(t.as_bytes()))
}

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

/// The form of a Date header: RFC 5322 as written, or the quirks that tell a tool or a server apart.
fn date_style(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let (main, comment) = match raw.find('(') {
        Some(at) => (raw[..at].trim(), true),
        None => (raw, false),
    };
    let tokens: Vec<&str> = main.split_whitespace().collect();
    if tokens.len() < 4 {
        return Some("not an RFC 5322 date".into());
    }
    let is_month = |t: &str| MONTHS.iter().any(|m| t.to_ascii_lowercase().starts_with(m));
    let first = tokens[0].trim_end_matches(',');
    let day_name = !first.is_empty() && first.chars().all(|c| c.is_ascii_alphabetic()) && !is_month(first);
    let digits: Vec<&str> =
        tokens.iter().copied().filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit())).collect();
    let year_len = digits.get(1).or_else(|| digits.iter().find(|d| d.len() == 4)).map_or(0, |y| y.len());
    let last = tokens[tokens.len() - 1];
    let numeric_zone = last.len() == 5
        && (last.starts_with('+') || last.starts_with('-'))
        && last[1..].chars().all(|c| c.is_ascii_digit());
    let named_zone = !numeric_zone && last.chars().all(|c| c.is_ascii_alphabetic()) && !last.contains(':');
    let no_zone = !numeric_zone && !named_zone;
    let mut quirks = Vec::new();
    if !day_name {
        quirks.push("no day name".to_string());
    }
    if year_len != 4 {
        quirks.push(format!("{year_len}-digit year"));
    }
    if named_zone {
        quirks.push(format!("named zone ({last})"));
    }
    if no_zone {
        quirks.push("no zone".into());
    }
    if comment {
        quirks.push("a comment after the zone".into());
    }
    Some(if quirks.is_empty() {
        "RFC 5322, day name, 4-digit year, numeric zone".into()
    } else {
        format!("RFC 5322 with quirks: {}", quirks.join(", "))
    })
}

/// "a Monday at 03:14 +0100": the poster's weekday and hour, in the poster's zone.
fn posted_on(raw: &str) -> Option<String> {
    let (dt, offset) = crate::dates::parse_rfc2822(raw)?;
    let day = match dt.weekday() {
        chrono::Weekday::Mon => "Monday",
        chrono::Weekday::Tue => "Tuesday",
        chrono::Weekday::Wed => "Wednesday",
        chrono::Weekday::Thu => "Thursday",
        chrono::Weekday::Fri => "Friday",
        chrono::Weekday::Sat => "Saturday",
        chrono::Weekday::Sun => "Sunday",
    };
    let zone = match offset {
        Some(o) => format!("{}{:02}{:02}", if o < 0 { '-' } else { '+' }, o.abs() / 3600, o.abs() % 3600 / 60),
        None => "zone unknown".into(),
    };
    Some(format!("a {day} at {} {zone}", dt.format("%H:%M")))
}

/// `a` minus `b`, in seconds, when both read as dates.
fn skew_secs(a: &str, b: &str) -> Option<i64> {
    Some(crate::dates::posted_timestamp(a)? - crate::dates::posted_timestamp(b)?)
}

/// "+4 s", "-3 min", "+2 h"
fn fmt_skew(secs: i64) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let a = secs.abs();
    if a < 60 {
        format!("{sign}{a} s")
    } else if a < 3600 {
        format!("{sign}{} min", a / 60)
    } else if a < 86_400 {
        format!("{sign}{} h", a / 3600)
    } else {
        format!("{sign}{} d", a / 86_400)
    }
}

fn fmt_unix(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0).map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_default()
}

/// The time a message-id carries when its grammar embeds one: a run of 10, 13, 16 or 19 digits that falls
/// between 2000 and now.
fn id_timestamp(message_id: &str) -> Option<i64> {
    let local = message_id.trim().trim_matches(['<', '>']).split('@').next().unwrap_or("");
    let now = chrono::Utc::now().timestamp();
    let window = 946_684_800..=now + 86_400;
    local.split(|c: char| !c.is_ascii_digit()).filter(|run| !run.is_empty()).find_map(|run| {
        let n: i64 = run.parse().ok()?;
        let secs = match run.len() {
            10 => n,
            13 => n / 1_000,
            16 => n / 1_000_000,
            19 => n / 1_000_000_000,
            _ => return None,
        };
        window.contains(&secs).then_some(secs)
    })
}

// ---------------------------------------------------------------- Posted by

fn posted_by_panel(i: &MetaInputs<'_>, l: &Looked, compact: bool) -> Facts {
    let mut f = Facts::new();
    let headers = i.probe.map(|p| &p.headers);
    if let Some(poster) = i.release.poster.as_deref().filter(|p| !p.trim().is_empty()) {
        f.add("From shape", Some(from_shape(poster)));
    }
    if let Some(path) = headers.and_then(|h| h.get("Path"))
        && let Some(found) = path_facts(path)
    {
        let backbone = found.backbone.map(|b| format!(", {b}")).unwrap_or_default();
        f.add("Injected at", Some(format!("{} ({} hop{}){backbone}", found.injector, found.hops, plural(found.hops))));
    }
    let grammar = l.first_id.as_deref().and_then(id_grammar);
    if !compact {
        if let Some(info) = headers.and_then(|h| h.get("Injection-Info")) {
            let parts = injection_parts(info);
            let mut present = Vec::new();
            if parts.account.is_some() {
                present.push("posting-account");
            }
            if parts.host.is_some() {
                present.push("posting-host");
            }
            if parts.logging_data {
                present.push("logging-data");
            }
            if parts.complaints {
                present.push("mail-complaints-to");
            }
            let text = if present.is_empty() {
                "present, with none of the usual parts".to_string()
            } else {
                format!("{} present; the account and host are shown as keys only", present.join(", "))
            };
            f.add("Injection-Info", Some(text));
        }
        f.add("Ids look like", grammar);
        if let Some(q) = i.probe.and_then(|p| p.quirks.as_ref()) {
            let summary = q.summary();
            let text = match yenc_quirks::class(q) {
                Some(class) => format!("like {class}: {summary}"),
                None => format!("like no documented encoder: {summary}"),
            };
            f.add("yEnc quirks", (!summary.is_empty()).then_some(text));
        }
    }
    if let Some(p) = i.probe
        && let Some(agent) = crate::probe::agent_of(&p.headers)
    {
        let line_len = p.yenc.as_ref().and_then(|y| y.line_len);
        let mut text = escape(agent);
        let quirks = p.quirks.as_ref();
        if let Some((agrees, why)) = tool_verdict(Some(agent), p.segment_size, line_len, grammar, quirks) {
            let _ = write!(
                text,
                "{} {}",
                verdict(Some(agrees), "FINGERPRINT AGREES", "FINGERPRINT DISAGREES"),
                escape(&why)
            );
        }
        f.add_html("Tool", Some(text));
    }
    f
}

/// How the From header is written, and whether its domain is one nobody receives mail at.
fn from_shape(poster: &str) -> String {
    let raw = poster.trim();
    let parsed = release_info::parse_poster(raw);
    let shape = if raw.contains('<') && raw.ends_with('>') {
        if raw.starts_with('<') { "<address> alone" } else { "Name <address>" }
    } else if raw.ends_with(')') && raw.contains('(') {
        "address (Name)"
    } else if parsed.email.is_some() {
        "bare address"
    } else {
        "name only"
    };
    match parsed.email.as_deref().filter(|e| release_info::looks_made_up(e)) {
        Some(_) => format!("{shape}, made-up domain"),
        None => shape.into(),
    }
}

struct PathFacts {
    hops: usize,
    /// the rightmost Path host, where the article entered Usenet
    injector: String,
    backbone: Option<&'static str>,
}

/// Who runs a Path host, by a word in its name.
const BACKBONES: &[(&str, &str)] = &[
    ("giganews", "Giganews"),
    ("highwinds", "Highwinds"),
    ("hw-filter", "Highwinds"),
    ("omicron", "Omicron"),
    ("usenetserver", "Omicron (UsenetServer)"),
    ("newshosting", "Omicron (Newshosting)"),
    ("easynews", "Omicron (Easynews)"),
    ("usenetexpress", "UsenetExpress"),
    ("usenet-express", "UsenetExpress"),
    ("abavia", "Abavia"),
    ("eweka", "Abavia (Eweka)"),
    ("xsnews", "XSNews"),
    ("astraweb", "Astraweb"),
    ("uzoreto", "Uzoreto"),
    ("blocknews", "BlockNews"),
    ("usenet.farm", "Usenet.Farm"),
    ("newsxs", "NewsXS"),
    ("supernews", "Supernews"),
    ("readnews", "ReadNews"),
    ("tweaknews", "Tweaknews"),
    ("newsgroupdirect", "NewsgroupDirect"),
];

/// The hosts of a Path header: `host1!host2!injector!not-for-mail`.
fn path_facts(path: &str) -> Option<PathFacts> {
    let hosts: Vec<&str> = path
        .split('!')
        .map(str::trim)
        .filter(|h| !h.is_empty() && !h.eq_ignore_ascii_case("not-for-mail") && !h.eq_ignore_ascii_case("nntp"))
        .collect();
    let injector = (*hosts.last()?).to_string();
    let lower = path.to_ascii_lowercase();
    let backbone = BACKBONES.iter().find(|(word, _)| lower.contains(word)).map(|(_, name)| *name);
    Some(PathFacts { hops: hosts.len(), injector, backbone })
}

use crate::keys::injection_parts;

/// The grammar of a message-id: the tool that minted it, or the server that did.
fn id_grammar(message_id: &str) -> Option<&'static str> {
    let id = message_id.trim().trim_matches(['<', '>']);
    let (local, domain) = id.split_once('@')?;
    if local.is_empty() || domain.is_empty() {
        return None;
    }
    let lower = domain.to_ascii_lowercase();
    let hex = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit());
    let nyuu_strict = local.len() == 38
        && local.as_bytes()[24] == b'-'
        && local.as_bytes()[..24].chunks(2).all(|c| c[0].is_ascii_uppercase() && c[1].is_ascii_lowercase())
        && local[25..].chars().all(|c| c.is_ascii_digit());
    Some(if lower == "nyuu" {
        if nyuu_strict { "Nyuu" } else { "Nyuu-like (an imitation, or an older Nyuu)" }
    } else if lower == "ngpost" && hex(local) && local.len() == 32 {
        "ngPost"
    } else if lower.ends_with("powerpost2000aa.local")
        || lower.ends_with("camelsystem-powerpost.local")
        || (local.starts_with("part") && local.contains("of") && local.contains('.'))
    {
        "PowerPost family"
    } else if lower == "jbinup.local" {
        "JBinUp"
    } else if lower == "jbindown.local" {
        "JBinDown"
    } else if lower == "gopoststuff" {
        "GoPostStuff"
    } else if lower.ends_with(".newsup") {
        "NewsUP"
    } else if lower == "riverup" {
        "Upstream"
    } else if lower.ends_with(".geri") {
        "geri"
    } else if lower == "spot.net" {
        "Spotnet"
    } else if lower == "ufk" || lower.ends_with("usenet.farm") {
        "server-assigned (Usenet.Farm)"
    } else if local.contains('$') {
        "server-assigned (INN or Diablo)"
    } else if lower.starts_with("news.") || BACKBONES.iter().any(|(word, _)| lower.contains(word)) {
        "provider server-assigned"
    } else {
        "unknown"
    })
}

struct ToolDefaults {
    agent: &'static str,
    /// decoded bytes per article, as the tool ships
    articles: &'static [u64],
    line: u32,
    grammar: &'static str,
}

const TOOL_DEFAULTS: &[ToolDefaults] = &[
    ToolDefaults { agent: "nyuu", articles: &[716_800], line: 128, grammar: "Nyuu" },
    ToolDefaults { agent: "ngpost", articles: &[716_800], line: 128, grammar: "ngPost" },
    ToolDefaults { agent: "newsup", articles: &[768_000], line: 128, grammar: "NewsUP" },
    ToolDefaults { agent: "gopoststuff", articles: &[768_000], line: 128, grammar: "GoPostStuff" },
    ToolDefaults { agent: "newsmangler", articles: &[], line: 128, grammar: "newsmangler" },
    ToolDefaults { agent: "powerpost", articles: &[640_000, 768_000], line: 128, grammar: "PowerPost family" },
    ToolDefaults { agent: "jbinup", articles: &[], line: 128, grammar: "JBinUp" },
];

/// Whether the article's shape matches the stated tool's defaults: article size, line length, id grammar, and the
/// quirks of its yEnc (`yenc_quirks::witness`). None when the tool is not one with known defaults, or nothing can be
/// compared.
fn tool_verdict(
    agent: Option<&str>,
    segment_size: Option<u64>,
    line_len: Option<u32>,
    grammar: Option<&str>,
    quirks: Option<&yenc_quirks::Quirks>,
) -> Option<(bool, String)> {
    let agent = agent?;
    let lower = agent.to_ascii_lowercase();
    let tool = TOOL_DEFAULTS.iter().find(|t| lower.contains(t.agent));
    let mut agree = Vec::new();
    let mut differ = Vec::new();
    if let Some(tool) = tool {
        tool_shape(tool, segment_size, line_len, grammar, &mut agree, &mut differ);
    }
    // a tool whose yEnc the research documents but whose article defaults it doesn't (postie, yEnc32) still gets this
    match quirks.and_then(|q| yenc_quirks::witness(q, agent)) {
        Some((true, why)) => agree.push(why),
        Some((false, why)) => differ.push(why),
        None => {}
    }
    if agree.is_empty() && differ.is_empty() {
        return None;
    }
    Some(if differ.is_empty() {
        (true, agree.join(", "))
    } else {
        (false, format!("{} are not the tool's defaults", differ.join(", ")))
    })
}

/// The article size, line length and id grammar against a tool's defaults.
fn tool_shape(
    tool: &ToolDefaults,
    segment_size: Option<u64>,
    line_len: Option<u32>,
    grammar: Option<&str>,
    agree: &mut Vec<String>,
    differ: &mut Vec<String>,
) {
    if let Some(size) = segment_size
        && !tool.articles.is_empty()
    {
        match tool.articles.iter().find(|d| **d == size) {
            Some(d) => agree.push(format!("{} KiB articles", d / 1024)),
            None => differ.push(format!("{} bytes per article", thousands(size as i64))),
        }
    }
    if let Some(len) = line_len {
        if len == tool.line || len == tool.line + 1 {
            agree.push(format!("{len}-byte lines"));
        } else {
            differ.push(format!("{len}-byte lines"));
        }
    }
    if let Some(g) = grammar {
        if g.starts_with(tool.grammar) {
            agree.push(format!("{} ids", tool.grammar));
        } else if !g.starts_with("server-assigned") && !g.starts_with("provider") {
            differ.push(format!("ids that look like {g}"));
        }
    }
}

// ---------------------------------------------------------------- Inside

fn inside_panel(i: &MetaInputs<'_>, l: &Looked, compact: bool) -> Facts {
    let mut f = Facts::new();
    let size = i.release.size;
    let head = i.probe.and_then(|p| p.archive.as_ref());
    f.add("Contents", contents_profile(&l.files, size, i.contents));
    if compact {
        let mut packing = Vec::new();
        if let Some(a) = head {
            packing.push(a.format.clone());
            if let Some(method) = &a.method {
                packing.push(method.clone());
            }
        }
        if let Some(kind) = i.inner.encryption {
            packing.push(kind.label().to_string());
        } else if matches!(i.inner.password, Some(crate::archive::Password::Yes)) {
            packing.push("encrypted".into());
        }
        if !packing.is_empty() {
            f.add("Packing", Some(packing.join(", ")));
        }
        f.add("PAR2", repair_capacity(i.contents));
        return f;
    }
    f.add("Volumes", volume_scheme(&l.files, size, i.contents));
    f.add(
        "Unpacks to",
        unpacked_estimate(&l.files, size, i.contents, i.inner, head).map(|u| match u {
            Unpacked::About(bytes) => format!("about {}", fmt_size(Some(bytes))),
            Unpacked::AtLeast(bytes) => format!("at least {} (partial listing)", fmt_size(Some(bytes))),
        }),
    );
    // the head's packed size is the first file's bytes in the volume read, its unpacked size the whole file's: a
    // ratio between them means something only when one volume holds the whole archive
    if let Some(a) = head
        && let (Some(packed), Some(unpacked)) = (a.packed, a.unpacked)
        && unpacked > 0
        && archive_volumes(&l.files) <= 1
    {
        let stored = if a.method.as_deref() == Some("m0") || packed == unpacked { ", stored" } else { "" };
        f.add(
            "Packed",
            Some(format!(
                "{} of {} ({}%){stored}",
                fmt_size(Some(packed as i64)),
                fmt_size(Some(unpacked as i64)),
                packed * 100 / unpacked
            )),
        );
    }
    let tells = packer_tells(head, i.probe.map_or(&[][..], |p| &p.layers), &l.files, size, i.contents);
    if !tells.is_empty() {
        f.add("Packed by", Some(tells.join("; ")));
    }
    // what the archive's first file header says: format, method, dictionary, host, how times are kept
    f.add("Packer settings", i.inner.packer.clone());
    f.add("Listed from", listing_source(i.inner));
    f.add_html("Points to", points_to_html(&l.points_to));
    f.add_html("Sidecars", sidecars_html(&l.sidecars));
    f.add("PAR2", repair_capacity(i.contents));
    f
}

/// The names of the files a posted NZB lists, as a save would name their release, PAR2 files last. At most 3.
fn nzb_release_names(inner: &InnerView) -> Vec<String> {
    crate::archive::nzb::release_names(inner.files.iter().filter(|r| r.source == SOURCE_NZB).map(|r| r.name.as_str()))
}

/// The releases a posted NZB points to, by name in every shard, not this one: up to 5 a name.
fn nzb_targets(conn: &Connection, i: &MetaInputs<'_>) -> Vec<(String, Vec<ReleaseRow>)> {
    let own = i.release.id;
    nzb_release_names(i.inner)
        .into_iter()
        .map(|name| {
            let mut found = Vec::new();
            for shard in 0..store::SHARDS {
                let sql = format!("select id from s{shard}.releases where name = ?1 and id != ?2 order by id limit 5");
                let ids: Vec<i64> = conn
                    .prepare(&sql)
                    .and_then(|mut stmt| stmt.query_map(rusqlite::params![name, own], |r| r.get(0))?.collect())
                    .unwrap_or_default();
                found.extend(ids.into_iter().filter_map(|id| crate::search::get_release_with(conn, id).ok().flatten()));
            }
            found.truncate(5);
            (name, found)
        })
        .collect()
}

/// Sidecars posted on their own that describe this release: by its MKV's Segment UID, then by its names.
fn sidecars_of(conn: &Connection, i: &MetaInputs<'_>) -> Vec<crate::sidecar::Found> {
    let mut names: Vec<&str> = vec![i.release.name.as_str()];
    if let Some(display) = i.names.and_then(|n| n.display.as_deref()) {
        names.push(display);
    }
    let uid = i.probe.and_then(|p| p.media.as_ref()).and_then(|m| m.segment_uid.as_deref());
    crate::sidecar::describing(conn, i.release.id, &names, uid, 5)
}

/// each sidecar: what it is, the post it came in and how it was matched, its text folded under it
fn sidecars_html(found: &[crate::sidecar::Found]) -> Option<String> {
    let items: Vec<String> = found
        .iter()
        .map(|s| {
            let what = match s.kind.as_str() {
                "mediainfo" => "MediaInfo",
                "nfo" => "NFO",
                "image" => "Cover image",
                _ => "text",
            };
            let by = if s.by == "segment_uid" { "same file, by its Segment UID" } else { "by name" };
            let file = s.file.as_deref().map(|f| format!(" ({})", escape(f))).unwrap_or_default();
            let text = s
                .text
                .as_deref()
                .map(|t| format!("<details><summary>Text</summary><pre class=\"nfo\">{}</pre></details>", escape(t)))
                .unwrap_or_default();
            format!(
                "{what}{file} <span class=\"dim\">posted as {} in {}, {by}</span>{text}",
                escape(&s.name),
                escape(&s.group)
            )
        })
        .collect();
    (!items.is_empty()).then(|| items.join("<br>"))
}

/// each name a posted NZB gives, linked to the releases of that name, or said not to be here
fn points_to_html(targets: &[(String, Vec<ReleaseRow>)]) -> Option<String> {
    let lines: Vec<String> = targets
        .iter()
        .map(|(name, found)| {
            if found.is_empty() {
                return format!("{} <span class=\"dim\">(not indexed here)</span>", escape(name));
            }
            let links: Vec<String> = found
                .iter()
                .map(|r| {
                    let state = if r.complete { "complete" } else { "incomplete" };
                    format!(
                        "<a href=\"/ui/release?id={}\">{}</a> <span class=\"dim\">in {}, {state}</span>",
                        r.id,
                        escape(&r.name),
                        escape(&r.group_name)
                    )
                })
                .collect();
            links.join("<br>")
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join("<br>"))
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FileKind {
    RarVolume,
    Rev,
    SevenZVolume,
    ZipVolume,
    RawSplit,
    Par2Index,
    Par2Recovery,
    Nfo,
    Sfv,
    Nzb,
    Srr,
    Sample,
    Video,
    Audio,
    Image,
    Text,
    Other,
}

fn kind_of(name: &str) -> FileKind {
    let lower = name.trim().to_ascii_lowercase();
    let (base, ext) = lower.rsplit_once('.').unwrap_or((lower.as_str(), ""));
    let digits = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit());
    if ext == "par2" {
        return if base.contains(".vol") { FileKind::Par2Recovery } else { FileKind::Par2Index };
    }
    match ext {
        "rar" => return FileKind::RarVolume,
        "rev" => return FileKind::Rev,
        "7z" => return FileKind::SevenZVolume,
        "zip" => return FileKind::ZipVolume,
        _ => {}
    }
    if ext.len() == 3 {
        if ext.starts_with('r') && digits(&ext[1..]) {
            return FileKind::RarVolume;
        }
        if ext.starts_with('z') && digits(&ext[1..]) {
            return FileKind::ZipVolume;
        }
        if digits(ext) {
            return if base.ends_with(".7z") { FileKind::SevenZVolume } else { FileKind::RawSplit };
        }
    }
    if lower.contains("sample") {
        return FileKind::Sample;
    }
    match ext {
        "nfo" => FileKind::Nfo,
        "sfv" => FileKind::Sfv,
        "nzb" => FileKind::Nzb,
        "srr" | "srs" => FileKind::Srr,
        "mkv" | "mp4" | "avi" | "m4v" | "mpg" | "mpeg" | "ts" | "m2ts" | "wmv" | "mov" | "vob" | "webm" => {
            FileKind::Video
        }
        "mp3" | "flac" | "m4a" | "aac" | "ogg" | "opus" | "wav" | "ape" | "wv" | "dts" | "ac3" => FileKind::Audio,
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" => FileKind::Image,
        "txt" | "md" | "cue" | "log" | "m3u" | "srt" | "idx" | "sub" => FileKind::Text,
        _ => FileKind::Other,
    }
}

fn counts(files: &[FileInfo]) -> BTreeMap<FileKind, usize> {
    let mut by_kind = BTreeMap::new();
    for f in files.iter().filter(|f| !f.filename.is_empty()) {
        *by_kind.entry(kind_of(&f.filename)).or_insert(0) += 1;
    }
    by_kind
}

/// How many archive volumes the posted files hold, whatever their scheme.
fn archive_volumes(files: &[FileInfo]) -> usize {
    counts(files)
        .iter()
        .filter(|(k, _)| {
            matches!(k, FileKind::RarVolume | FileKind::SevenZVolume | FileKind::ZipVolume | FileKind::RawSplit)
        })
        .map(|(_, n)| *n)
        .sum()
}

/// The bytes of the files that are not PAR2, from the counts when they are there, else the release's size.
fn data_bytes(size: Option<i64>, c: &Contents) -> Option<i64> {
    if c.data_bytes > 0 {
        return Some(c.data_bytes);
    }
    size.map(|s| (s - c.recovery_bytes).max(0)).filter(|s| *s > 0)
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// "47 RAR volumes (about 50 MB each), 9 PAR2 (8 recovery), NFO, SFV, 2 samples": the type mix.
fn contents_profile(files: &[FileInfo], size: Option<i64>, c: &Contents) -> Option<String> {
    let by_kind = counts(files);
    if by_kind.is_empty() {
        return None;
    }
    let n = |k: FileKind| by_kind.get(&k).copied().unwrap_or(0);
    let volumes = n(FileKind::RarVolume) + n(FileKind::SevenZVolume) + n(FileKind::ZipVolume) + n(FileKind::RawSplit);
    let each = data_bytes(size, c)
        .filter(|_| volumes > 0)
        .map(|bytes| match volumes {
            1 => format!(" ({})", fmt_size(Some(bytes))),
            n => format!(" (about {} each)", fmt_size(Some(bytes / n as i64))),
        })
        .unwrap_or_default();
    let mut parts = Vec::new();
    for (kind, word) in [
        (FileKind::RarVolume, "RAR volume"),
        (FileKind::SevenZVolume, "7z volume"),
        (FileKind::ZipVolume, "ZIP volume"),
        (FileKind::RawSplit, "raw split part"),
    ] {
        let count = n(kind);
        if count > 0 {
            parts.push(format!("{count} {word}{}{each}", plural(count)));
        }
    }
    if n(FileKind::Rev) > 0 {
        parts.push(format!("{} .rev", n(FileKind::Rev)));
    }
    let par2 = n(FileKind::Par2Index) + n(FileKind::Par2Recovery);
    if par2 > 0 {
        parts.push(format!("{par2} PAR2 ({} recovery)", n(FileKind::Par2Recovery)));
    }
    for (kind, word) in [(FileKind::Nfo, "NFO"), (FileKind::Sfv, "SFV"), (FileKind::Nzb, "NZB"), (FileKind::Srr, "SRR")]
    {
        match n(kind) {
            0 => {}
            1 => parts.push(word.to_string()),
            count => parts.push(format!("{count} {word}s")),
        }
    }
    for (kind, word) in [
        (FileKind::Sample, "sample"),
        (FileKind::Video, "video file"),
        (FileKind::Audio, "audio file"),
        (FileKind::Image, "image"),
        (FileKind::Text, "text file"),
        (FileKind::Other, "other file"),
    ] {
        let count = n(kind);
        if count > 0 {
            parts.push(format!("{count} {word}{}", plural(count)));
        }
    }
    Some(parts.join(", "))
}

/// "47 volumes × 50.0 MB (.partNN.rar)": the count, the size each and the naming scheme.
fn volume_scheme(files: &[FileInfo], size: Option<i64>, c: &Contents) -> Option<String> {
    let names: Vec<String> = files.iter().map(|f| f.filename.to_ascii_lowercase()).collect();
    let has = |test: &dyn Fn(&str) -> bool| names.iter().any(|n| test(n));
    let digits = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit());
    let part_rar = |n: &str| {
        n.ends_with(".rar")
            && n.trim_end_matches(".rar")
                .rsplit_once('.')
                .is_some_and(|(_, p)| p.strip_prefix("part").is_some_and(digits))
    };
    let r_nn = |n: &str| n.rsplit_once('.').is_some_and(|(_, e)| e.len() == 3 && e.starts_with('r') && digits(&e[1..]));
    let sevenz = |n: &str| n.rsplit_once('.').is_some_and(|(b, e)| e.len() == 3 && digits(e) && b.ends_with(".7z"));
    let z_nn = |n: &str| n.rsplit_once('.').is_some_and(|(_, e)| e.len() == 3 && e.starts_with('z') && digits(&e[1..]));
    let raw = |n: &str| n.rsplit_once('.').is_some_and(|(b, e)| e.len() == 3 && digits(e) && !b.ends_with(".7z"));
    let scheme = if has(&part_rar) {
        ".partNN.rar"
    } else if has(&r_nn) {
        "old-style .rNN"
    } else if has(&sevenz) {
        ".7z.NNN"
    } else if has(&z_nn) {
        ".zNN"
    } else if has(&raw) {
        "raw split .NNN"
    } else {
        return None;
    };
    let by_kind = counts(files);
    let count = match scheme {
        ".partNN.rar" | "old-style .rNN" => by_kind.get(&FileKind::RarVolume).copied().unwrap_or(0),
        ".7z.NNN" => by_kind.get(&FileKind::SevenZVolume).copied().unwrap_or(0),
        ".zNN" => by_kind.get(&FileKind::ZipVolume).copied().unwrap_or(0),
        _ => by_kind.get(&FileKind::RawSplit).copied().unwrap_or(0),
    };
    if count == 0 {
        return None;
    }
    let each =
        data_bytes(size, c).map(|bytes| format!(" × {}", fmt_size(Some(bytes / count as i64)))).unwrap_or_default();
    let rev = by_kind.get(&FileKind::Rev).filter(|n| **n > 0).map(|n| format!(", {n} .rev")).unwrap_or_default();
    Some(format!("{count} volume{}{each} ({scheme}){rev}", plural(count)))
}

/// What the release unpacks to, and how far the figure can be trusted.
#[derive(Debug, PartialEq, Eq)]
enum Unpacked {
    /// the whole of it, within about a tenth
    About(i64),
    /// from a listing that may stop before the archive's end: no less than this
    AtLeast(i64),
}

/// What the release unpacks to: the listed files' sizes when they are known, the archive's own figure for a
/// single file, else the posted data less yEnc's overhead. A stored listing can stop short (RAR headers past the
/// bytes read, a ZIP read from its front) and whether it did is not stored, soo a listing's sum counts as the
/// whole only when it comes within a tenth of the posted data; below that it is a lower bound.
fn unpacked_estimate(
    files: &[FileInfo],
    size: Option<i64>,
    c: &Contents,
    inner: &InnerView,
    head: Option<&crate::probe::archive::ArchiveHead>,
) -> Option<Unpacked> {
    // yEnc adds about 2% in escapes and a little in control lines
    let posted = data_bytes(size, c).map(|bytes| bytes * 100 / 103);
    let against_posted = |bytes: i64| match posted {
        Some(p) if bytes * 10 >= p * 9 => Unpacked::About(bytes),
        _ => Unpacked::AtLeast(bytes),
    };
    // the archive's own listing when there is one. A PAR2 inventory lists the posted files, which are the
    // contents only when none of them is an archive; added to the archive's listing it would count the volumes
    // and what is inside them both. A posted NZB lists another post's files, which say nothing of this one
    let listed: Vec<&InnerFileRow> =
        inner.files.iter().filter(|r| r.source != SOURCE_PAR2 && r.source != SOURCE_NZB).collect();
    if !listed.is_empty() {
        if listed.iter().all(|r| r.size.is_some()) {
            return Some(against_posted(listed.iter().filter_map(|r| r.size).sum()));
        }
    } else {
        // a PAR2 set names every file it protects, soo its sum is the whole
        let par2: Vec<&InnerFileRow> = inner.files.iter().filter(|r| r.source == SOURCE_PAR2).collect();
        if !par2.is_empty()
            && !crate::duplicates::lists_archives(par2.iter().map(|r| r.name.as_str()))
            && par2.iter().all(|r| r.size.is_some())
        {
            return Some(Unpacked::About(par2.iter().filter_map(|r| r.size).sum()));
        }
    }
    // the first file's size, which is the archive's when it holds one file
    if let Some(unpacked) = head.and_then(|a| a.unpacked)
        && listed.len() <= 1
    {
        return Some(against_posted(unpacked as i64));
    }
    let by_kind = counts(files);
    let volumes = by_kind
        .iter()
        .filter(|(k, _)| {
            matches!(
                k,
                FileKind::RarVolume
                    | FileKind::SevenZVolume
                    | FileKind::ZipVolume
                    | FileKind::RawSplit
                    | FileKind::Video
                    | FileKind::Audio
            )
        })
        .count();
    if volumes == 0 && !files.is_empty() {
        return None;
    }
    posted.map(Unpacked::About)
}

/// Tells about the packer: ngPost's defaults, an archive inside an archive.
fn packer_tells(
    head: Option<&crate::probe::archive::ArchiveHead>,
    layers: &[crate::probe::nest::Layer],
    files: &[FileInfo],
    size: Option<i64>,
    c: &Contents,
) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(a) = head {
        let stored =
            a.method.as_deref() == Some("m0") || a.method.as_deref().is_some_and(|m| m.eq_ignore_ascii_case("stored"));
        let by_kind = counts(files);
        let volumes = by_kind.get(&FileKind::RarVolume).copied().unwrap_or(0);
        let fifty = volumes > 1
            && data_bytes(size, c).is_some_and(|bytes| {
                let each = bytes / volumes as i64;
                (45 << 20..=55 << 20).contains(&each)
            });
        let random = files.iter().filter(|f| !f.filename.is_empty()).count() > 0
            && files.iter().filter(|f| looks_obfuscated(&f.filename)).count() * 2 > files.len();
        if stored && fifty && random {
            out.push("ngPost's defaults: stored, 50 MB volumes, random names".to_string());
        } else if stored && a.format.starts_with("RAR") {
            out.push("stored, not compressed (m0)".to_string());
        }
    }
    let archives: Vec<&str> =
        layers.iter().map(|l| l.format.as_str()).filter(|f| matches!(*f, "RAR4" | "RAR5" | "7z" | "ZIP")).collect();
    if archives.len() > 1 {
        out.push(format!("an archive inside an archive ({} in {})", archives[1], archives[0]));
    }
    out
}

/// Where the inner file names came from: the PAR2 index, the archive headers, or both.
fn listing_source(inner: &InnerView) -> Option<String> {
    if inner.files.is_empty() {
        return None;
    }
    let sources: BTreeSet<i64> = inner.files.iter().map(|r| r.source).collect();
    let words: Vec<&str> = sources
        .iter()
        .filter_map(|s| match *s {
            SOURCE_PAR2 => Some("the PAR2 index"),
            SOURCE_RAR => Some("the RAR headers"),
            SOURCE_7Z => Some("the 7z headers"),
            SOURCE_ZIP => Some("the ZIP headers"),
            SOURCE_NZB => Some("an NZB's file list"),
            _ => None,
        })
        .collect();
    (!words.is_empty()).then(|| format!("names from {}", words.join(" and ")))
}

/// "slice 1.0 MB, up to 96 of 1,000 blocks (9.6%)" once the PAR2 set has been read.
fn repair_capacity(c: &Contents) -> Option<String> {
    let block = c.par2_block.filter(|b| *b > 0)?;
    let blocks = c.par2_blocks.filter(|b| *b > 0)?;
    let per_mille = c.recovery_blocks * 1000 / blocks;
    Some(format!(
        "slice {}, up to {} of {} blocks ({})",
        fmt_size(Some(block)),
        thousands(c.recovery_blocks),
        thousands(blocks),
        per_mille_text(per_mille)
    ))
}

/// 30 as "3%", 96 as "9.6%"
fn per_mille_text(n: i64) -> String {
    if n % 10 == 0 { format!("{}%", n / 10) } else { format!("{}.{}%", n / 10, n % 10) }
}

// ---------------------------------------------------------------- Health

fn health_panel(i: &MetaInputs<'_>, l: &Looked, compact: bool) -> Facts {
    let mut f = Facts::new();
    if compact {
        if let Some(p) = i.probe
            && !p.health.is_empty()
        {
            let tags: String = ["first", "middle", "last"]
                .into_iter()
                .filter_map(|part| p.health.get(part).map(|there| (part, *there)))
                .map(|(part, there)| {
                    let word = part.to_uppercase();
                    if there {
                        tag("tag-ok", &format!("{word} THERE"))
                    } else {
                        tag("tag-bad", &format!("{word} GONE"))
                    }
                })
                .collect();
            f.add_html("On the server", Some(tags));
        }
        return f;
    }
    if let Some(y) = i.probe.and_then(|p| p.yenc.as_ref()) {
        let mut parts = Vec::new();
        if let Some(part) = y.part {
            parts.push(match y.total {
                Some(total) => format!("part {part} of {total}"),
                None => format!("part {part}"),
            });
        }
        if let (Some(begin), Some(end)) = (y.begin, y.end) {
            parts.push(format!("bytes {}–{}", thousands(begin as i64), thousands(end as i64)));
        }
        if let Some(size) = y.yend_size {
            parts.push(format!("=yend size {}", thousands(size as i64)));
        }
        if !parts.is_empty() {
            f.add("yEnc control lines", Some(parts.join(", ")));
        }
        if let Some(main) = &l.main {
            let subject_part = main.subject_part;
            let subject_total = main.expected;
            let yenc_part = y.part.map(i64::from);
            let yenc_total = y.total.map(i64::from);
            let disagree = (subject_part.is_some() && yenc_part.is_some() && subject_part != yenc_part)
                || (subject_total.is_some() && yenc_total.is_some() && subject_total != yenc_total);
            if disagree {
                let say = |a: Option<i64>, b: Option<i64>, sep: &str| {
                    format!(
                        "{}{sep}{}",
                        a.map_or("?".into(), |n| n.to_string()),
                        b.map_or("?".into(), |n| n.to_string())
                    )
                };
                f.add(
                    "Numbering",
                    Some(format!(
                        "subject says {}, yEnc says {}",
                        say(subject_part, subject_total, "/"),
                        say(yenc_part, yenc_total, " of ")
                    )),
                );
            }
        }
    }
    let touched: Vec<i64> = l.files.iter().filter_map(|f| f.touched_at).filter(|t| *t > 0).collect();
    if let (Some(newest), Some(oldest)) = (touched.iter().max(), touched.iter().min()) {
        let grew = newest - oldest;
        let span =
            if grew >= 60 { format!(", grew for {}", fmt_skew(grew).trim_start_matches('+')) } else { String::new() };
        f.add("Last arrival", Some(format!("{}{span}", fmt_unix(*newest))));
    }
    if !l.timing.is_empty() {
        let partial = i.release.parts.is_some_and(|p| p > timed(&l.timing));
        f.add("Lines per article", lines_per_article(&l.timing, if partial { None } else { i.release.size }));
        f.add("Late articles", Some(late_articles(&l.timing)));
    }
    f
}

// ---------------------------------------------------------------- Keys

fn keys_panel(i: &MetaInputs<'_>, l: &Looked, compact: bool) -> Facts {
    let mut f = Facts::new();
    let headers = i.probe.map(|p| &p.headers);
    f.key("First-id key", l.first_id.as_deref().map(|id| key16(id.as_bytes())));
    let parts = headers.and_then(|h| h.get("Injection-Info")).map(|info| injection_parts(info)).unwrap_or_default();
    let host = parts.host.as_deref().or_else(|| headers.and_then(|h| h.get("NNTP-Posting-Host")).map(String::as_str));
    let id = i.release.id;
    let with_link = |text: String, kind| format!("{}{}", key_code(&text), same_link(l, id, kind));
    if let Some(salt) = l.salt {
        f.add_html("Account key", parts.account.as_deref().map(|a| with_link(key12(salt, a), Kind::Account)));
        if !compact {
            f.add_html("Host key", host.map(|h| with_link(key12(salt, h), Kind::Host)));
        }
    }
    if compact {
        return f;
    }
    f.add("Tail bytes", l.last.as_ref().map(|s| format!("{} bytes", thousands(s.bytes))));
    if let (Some(key), Some(main)) = (&l.id_key, &l.main)
        && let Some(subject) = main.subject.as_deref()
        && let Some((hash, weak)) = collection_key(
            key,
            &i.release.group_name,
            subject,
            i.release.poster.as_deref().unwrap_or(""),
            i.release.posted_date.as_deref().unwrap_or(""),
        )
    {
        let note =
            if weak { ", from the poster and day on the row, which a later merge may have replaced" } else { "" };
        f.add_html("Collection key", Some(format!("{} (recomputed{note})", key_code(&hash.to_string()))));
    }
    f.key("Posted fingerprint", l.posted_fp.as_deref().map(hex));
    f.key("Content fingerprint", l.content_fp.as_deref().map(hex));
    f.add_html(
        "Structural key",
        i.probe.and_then(|p| p.media.as_ref()).and_then(structural_key).map(|k| with_link(k, Kind::Layout)),
    );
    f.key("Password check", i.inner.password_check.as_ref().map(password_check));
    if let Some(main) = &l.main
        && let Some(subject) = main.subject.as_deref()
    {
        let link = nzblnk(i, subject);
        f.add_html("NZBLNK", Some(format!("<a href=\"{}\">{}</a>", escape(&link), key_code(&link))));
    }
    f.add_html("Lines key", lines_key(&l.timing, i.release.parts).map(|k| with_link(k, Kind::Lines)));
    f.add_html(
        "Video stream key",
        i.probe
            .and_then(|p| p.media.as_ref())
            .and_then(|m| m.video.first())
            .and_then(|v| v.keyframe_key.clone())
            .map(|k| with_link(k, Kind::Video)),
    );
    f.add_html(
        "Audio stream key",
        i.probe
            .and_then(|p| p.media.as_ref())
            .and_then(crate::keys::audio_stream_key)
            .map(|k| with_link(k.to_string(), Kind::Audio)),
    );
    // counted from the tables the twin and probe work keep, shown here only when another release shares them
    f.add_html(
        "File CRC",
        i.probe
            .and_then(crate::keys::probe_file)
            .map(|file| {
                format!(
                    "{} over {} bytes{}",
                    key_code(&format!("{:08x}", file.crc32)),
                    thousands(file.size),
                    same_link(l, id, Kind::FileCrc)
                )
            })
            .or_else(|| {
                // no probed file, but a PAR2-described one another release has too
                let link = same_link(l, id, Kind::FileCrc);
                (!link.is_empty()).then(|| format!("A file its PAR2 set describes{link}"))
            }),
    );
    f.add_html(
        "First-article body CRC",
        i.probe
            .and_then(|p| p.headers.get(crate::probe::BODY_CRC_HEADER))
            .and_then(|v| crate::keys::received_body_crc(v))
            .map(|crc| with_link(format!("{crc:08x}"), Kind::BodyCrc)),
    );
    if !same_link(l, id, Kind::Par2).is_empty() {
        f.add_html("PAR2 set", Some(format!("PAR2 recovery set{}", same_link(l, id, Kind::Par2))));
    }
    f.add_html(
        "OpenSubtitles hash",
        i.probe
            .and_then(|p| p.moviehash.as_deref())
            .filter(|_| !same_link(l, id, Kind::MovieHash).is_empty())
            .map(|h| with_link(h.to_string(), Kind::MovieHash)),
    );
    let media = i.probe.and_then(|p| p.media.as_ref());
    f.add_html("Mux key", media.and_then(crate::keys::mux_text).map(|t| with_link(key16(t.as_bytes()), Kind::Mux)));
    f.add_html(
        "Encode key",
        media.and_then(crate::keys::encode_text).map(|t| with_link(key16(t.as_bytes()), Kind::Encode)),
    );
    f.add_html(
        "Thread key",
        i.probe
            .and_then(|p| crate::probe::thread_root(&p.headers))
            .map(|root| with_link(key16(format!("thread:{root}").as_bytes()), Kind::Thread)),
    );
    // the poster's header names in order, the ones servers add left out: a tool's fingerprint, shared by everyone
    // posting with it, soo no list
    f.key("Header set", i.probe.and_then(|p| crate::probe::header_set(&p.header_order)).map(|t| key16(t.as_bytes())));
    // an archive's own keys: how many releases share one, never its value
    for (label, what, kind) in [
        ("Archive file CRC", "A file inside its archive, by size and CRC32", Kind::InnerFile),
        ("Archive twin", "The archive's own random values", Kind::ArchiveTwin),
        ("Archive owner", "The Unix owner its archive recorded (kept as a salted key only)", Kind::Owner),
        ("Packer zone", "Its packer's settings and clock zone (kept as a salted key only)", Kind::Zone),
    ] {
        let link = same_link(l, id, kind);
        if !link.is_empty() {
            f.add_html(label, Some(format!("{what}{link}")));
        }
    }
    f
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256, first 8 bytes, lowercase hex.
fn key16(bytes: &[u8]) -> String {
    crate::keys::hex16(crate::keys::as_key(crate::keys::digest8(bytes)))
}

/// SHA-256 over the salt and the value, first 6 bytes: lines two postings up without naming the account.
fn key12(salt: i64, value: &str) -> String {
    crate::keys::hex12(crate::keys::as_key(crate::keys::salted8(salt, value)))
}

/// The ingest's own collection key, rebuilt from the row: `Collections::of` over the subject, then the shard's
/// `IdKey`. The bool says the key is a weak-name one, whose author and day are the first article's.
fn collection_key(
    key: &store::IdKey,
    group: &str,
    subject: &str,
    poster: &str,
    posted_date: &str,
) -> Option<(i64, bool)> {
    let parsed = crate::parser::parse_subject(subject)?;
    let article = crate::parser::Article {
        subject: subject.to_string(),
        author: poster.to_string(),
        date: posted_date.to_string(),
        ..Default::default()
    };
    let collection = crate::collections::Collections::new(group).of(&article, &parsed)?;
    Some((key.hash(&collection), is_weak(&collection)))
}

/// A weak-name key has five fields: group, cleaned name, files, author, day (collections.rs). The author and
/// day are the first article's, which a later merge may have replaced on the row.
fn is_weak(collection: &str) -> bool {
    collection.matches('\0').count() == 4
}

/// A key over the track layout, the chapter count and the duration: the same encode across remuxes and
/// renames. The fields themselves are the media thread's to show.
fn structural_key(media: &crate::probe::media::MediaInfo) -> Option<String> {
    crate::keys::layout_text(media).map(|t| key16(t.as_bytes()))
}

/// The archive's stored password test, as hex: what a password has to fold down to.
fn password_check(check: &crate::archive::PasswordTest) -> String {
    use crate::archive::PasswordTest;
    match check {
        PasswordTest::Rar5(c) => format!("RAR5 kdf 2^{}, salt {}, check {}", c.kdf, hex(&c.salt), hex(&c.check)),
        PasswordTest::Zip(crate::archive::zip::PasswordCheck::Crypto { check, .. }) => {
            format!("ZipCrypto check byte {check:02x}")
        }
        PasswordTest::Zip(crate::archive::zip::PasswordCheck::Aes { strength, salt, verifier }) => {
            let bits = match strength {
                1 => 128,
                2 => 192,
                _ => 256,
            };
            let salt_len = (usize::from(*strength) * 4 + 4).min(salt.len());
            format!("ZIP AES-{bits}, salt {}, verifier {}", hex(&salt[..salt_len]), hex(verifier))
        }
    }
}

/// The release's NZBLNK as <https://nzblnk.info/> specifies it: the name a reader sees as `t`, the main file's
/// subject (its release name or file name stem) as `h`, the group as `g` and, when a name or that subject carries
/// one (`{{secret}}`, `password: secret`), the poster's password as `p`. Nothing is tried or looked up for it.
fn nzblnk(i: &MetaInputs<'_>, subject: &str) -> String {
    let display = i.names.and_then(|n| n.display.as_deref()).filter(|d| !d.trim().is_empty());
    let texts = [Some(i.release.name.as_str()), i.names.map(|n| n.posted.as_str()), display, Some(subject)];
    let password = texts.into_iter().flatten().find_map(crate::release_info::find_password);
    crate::nzblnk::NzbLnk {
        title: crate::nzblnk::strip_password(display.unwrap_or(&i.release.name)),
        header: crate::nzblnk::header_of(subject),
        groups: vec![i.release.group_name.clone()],
        password,
    }
    .to_uri()
}

// ---------------------------------------------------------------- the lazy fragments

/// The probed article's headers, with the account and host as keys, plus its Message-ID and Subject.
fn headers_fragment(conn: &Connection, id: i64) -> String {
    let probe = crate::probe::read(conn, id);
    let l = look(Some(conn), id);
    let mut rows = String::new();
    let mut row = |name: &str, value: String, bytes: usize| {
        let _ = write!(rows, "<tr><td>{}</td><td>{value}</td><td class=\"num\">{bytes}</td></tr>", escape(name));
    };
    if let Some(id) = &l.first_id {
        row("Message-ID", escape(id), id.len());
    }
    if let Some(subject) = l.main.as_ref().and_then(|m| m.subject.as_deref()) {
        row("Subject", escape(subject), subject.len());
    }
    if let Some(p) = &probe {
        const ORDER: &[&str] = &[
            "Path",
            "Date",
            "Injection-Date",
            "NNTP-Posting-Date",
            "Injection-Info",
            "NNTP-Posting-Host",
            "User-Agent",
            "X-Newsreader",
            "X-Newsposter",
            "Organization",
            "X-Complaints-To",
            "Xref",
        ];
        let rank = |name: &str| ORDER.iter().position(|o| *o == name).unwrap_or(ORDER.len());
        let mut headers: Vec<(&String, &String)> = p.headers.iter().collect();
        headers.sort_by_key(|(name, _)| rank(name));
        for (name, value) in headers {
            row(name, redact_header(name, value, l.salt), value.len());
        }
    }
    if rows.is_empty() {
        return "<p class=\"dim\">No article of this release has been read yet.</p>".into();
    }
    format!(
        "<div class=\"table-wrap\"><table><thead><tr><th>Header</th><th>Value</th><th class=\"num\">Bytes</th></tr></thead><tbody>{rows}</tbody></table></div><p class=\"dim\">The headers kept from the main file's first article, 512 characters each at most. The posting account and host are shown as salted keys.</p>"
    )
}

/// A header's value as HTML, with the posting account, host and IP replaced by their keys.
pub(crate) fn redact_header(name: &str, value: &str, salt: Option<i64>) -> String {
    let keyed = |v: &str| match salt {
        Some(salt) => key_code(&key12(salt, v)),
        None => "<span class=\"dim\">hidden</span>".to_string(),
    };
    match name {
        "NNTP-Posting-Host" => keyed(value),
        "Injection-Info" => value
            .split(';')
            .map(|part| match part.split_once('=') {
                Some((key, v)) => {
                    let key = key.trim();
                    let v = v.trim().trim_matches('"');
                    match key.to_ascii_lowercase().as_str() {
                        "posting-account" | "posting-host" => format!("{}={}", escape(key), keyed(v)),
                        "logging-data" => format!("{}=<span class=\"dim\">present</span>", escape(key)),
                        _ => format!("{}={}", escape(key), escape(v)),
                    }
                }
                None => escape(part.trim()),
            })
            .collect::<Vec<_>>()
            .join("; "),
        _ => escape(value),
    }
}

/// What one pass over the articles says about a file.
#[derive(Default)]
struct FileParts {
    seen: BTreeSet<i64>,
    reposted: usize,
    counts: BTreeMap<i64, usize>,
    first: Option<(i64, String)>,
    last: Option<(i64, String)>,
    smallest: Option<i64>,
    largest: Option<i64>,
}

impl FileParts {
    fn offer(&mut self, part: Option<i64>, message_id: String, bytes: Option<i64>) {
        if let Some(bytes) = bytes {
            self.smallest = Some(self.smallest.map_or(bytes, |s| s.min(bytes)));
            self.largest = Some(self.largest.map_or(bytes, |l| l.max(bytes)));
        }
        let part = part.unwrap_or(0);
        let count = self.counts.entry(part).or_insert(0);
        *count += 1;
        if *count == 2 {
            self.reposted += 1;
        }
        self.seen.insert(part);
        if self.first.as_ref().is_none_or(|(p, id)| (part, &message_id) < (*p, id)) {
            self.first = Some((part, message_id.clone()));
        }
        if self.last.as_ref().is_none_or(|(p, id)| (part, &message_id) > (*p, id)) {
            self.last = Some((part, message_id));
        }
    }
}

/// Every article of the release through `each`, stopping after `budget` of them: false when it stopped or the
/// read failed, soo the caller says the list is partial instead of showing it as whole.
fn walk_articles(conn: &Connection, id: i64, budget: usize, each: &mut dyn FnMut(ArticleRow)) -> bool {
    let mut seen = 0usize;
    store::each_article(conn, id, &mut |a| {
        seen += 1;
        if seen > budget {
            return Err(rusqlite::Error::ToSqlConversionFailure("over the article budget".into()));
        }
        each(a);
        Ok(())
    })
    .is_ok()
}

/// One row per file: subject, part map, first and last id, id grammar, article sizes, reposted parts.
fn parts_fragment(conn: &Connection, id: i64) -> String {
    let Some(shard) = shard_conn(id) else { return String::new() };
    let files = store::release_files(&shard, id, FILES_CAP).unwrap_or_default();
    parts_fragment_of(conn, id, &files, ARTICLE_BUDGET)
}

/// The Parts table from one pass over the articles, which gathers only the files the table shows and stops at
/// `budget` articles.
fn parts_fragment_of(conn: &Connection, id: i64, files: &[FileInfo], budget: usize) -> String {
    if files.is_empty() {
        return "<p class=\"dim\">This release has no files on record.</p>".into();
    }
    let shown: HashSet<&str> = files.iter().take(PARTS_ROWS).map(|f| f.filename.as_str()).collect();
    let mut by_file: HashMap<String, FileParts> = HashMap::new();
    let walked = walk_articles(conn, id, budget, &mut |a| {
        let name = a.filename.unwrap_or_default();
        if shown.contains(name.as_str()) {
            by_file.entry(name).or_default().offer(a.part, a.message_id, a.bytes);
        }
    });
    // a pass cut short would show the files it never reached as holed: the maps stay blank and the note says why
    if !walked {
        by_file.clear();
    }
    parts_table(files, &by_file, walked)
}

/// The Parts table's heading: no Name, Posted size or Articles column, which the NZB file list shows.
const PARTS_HEAD: &str = "<tr><th>Subject</th><th>Parts</th><th>First id</th><th>Last id</th><th>Ids look like</th><th class=\"num\">Smallest</th><th class=\"num\">Largest</th><th class=\"num\">Reposted</th></tr>";

/// The first `PARTS_ROWS` files as rows. `walked`: the article pass finished; when it did not, the maps show
/// the parts counted at ingest only.
fn parts_table(files: &[FileInfo], by_file: &HashMap<String, FileParts>, walked: bool) -> String {
    let mut rows = String::new();
    for f in files.iter().take(PARTS_ROWS) {
        let parts = by_file.get(&f.filename);
        let expected = f.expected.unwrap_or(f.parts_seen).max(f.parts_seen);
        let seen = parts.map(|p| p.seen.clone()).unwrap_or_default();
        let id_of = |pick: Option<&(i64, String)>| pick.map(|(_, id)| escape(id)).unwrap_or_default();
        let grammar = parts.and_then(|p| p.first.as_ref()).and_then(|(_, id)| id_grammar(id)).unwrap_or("");
        let bytes = |n: Option<i64>| n.map(thousands).unwrap_or_default();
        let _ = write!(
            rows,
            "<tr><td class=\"name\">{subject}</td><td class=\"partmap\" title=\"{seen_count} of {expected} parts\">{map}</td><td>{first}</td><td>{last}</td><td>{grammar}</td><td class=\"num\">{smallest}</td><td class=\"num\">{largest}</td><td class=\"num\">{reposted}</td></tr>",
            subject = escape(f.subject.as_deref().unwrap_or(&f.filename)),
            seen_count = f.parts_seen,
            map = part_map(&seen, expected),
            first = id_of(parts.and_then(|p| p.first.as_ref())),
            last = id_of(parts.and_then(|p| p.last.as_ref())),
            smallest = bytes(parts.and_then(|p| p.smallest)),
            largest = bytes(parts.and_then(|p| p.largest)),
            reposted = parts.map_or(0, |p| p.reposted),
        );
    }
    let mut notes = String::new();
    if !walked {
        notes.push_str(
            "<p class=\"dim\">Too many articles to list: the maps are blank, and each map's title has the parts counted at ingest.</p>",
        );
    }
    if files.len() > PARTS_ROWS {
        let total = if files.len() >= FILES_CAP {
            format!("{} or more", thousands(FILES_CAP as i64))
        } else {
            thousands(files.len() as i64)
        };
        let _ = write!(notes, "<p class=\"dim\">The first {PARTS_ROWS} of {total} files.</p>");
    }
    format!("<div class=\"table-wrap\"><table><thead>{PARTS_HEAD}</thead><tbody>{rows}</tbody></table></div>{notes}")
}

/// 64 cells over the parts 1..=expected: `▮` when every part a cell covers was seen, `▯` otherwise.
fn part_map(seen: &BTreeSet<i64>, expected: i64) -> String {
    (0..MAP_CELLS)
        .map(|cell| {
            if expected <= 0 {
                return '▯';
            }
            let lo = cell * expected / MAP_CELLS + 1;
            let hi = ((cell + 1) * expected / MAP_CELLS).max(lo);
            if (lo..=hi).all(|p| seen.contains(&p)) { '▮' } else { '▯' }
        })
        .collect()
}

/// The digests over the articles: the segment set, the first part's CRC.
fn hashes_fragment(conn: &Connection, id: i64) -> String {
    hashes_fragment_of(conn, id, ARTICLE_BUDGET)
}

/// `hashes_fragment` with the segment set read from at most `budget` articles.
fn hashes_fragment_of(conn: &Connection, id: i64, budget: usize) -> String {
    let probe = crate::probe::read(conn, id);
    let mut ids: Vec<String> = Vec::new();
    let walked = walk_articles(conn, id, budget, &mut |a| ids.push(a.message_id));
    let mut f = Facts::new();
    if walked && !ids.is_empty() {
        ids.sort();
        ids.dedup();
        let mut h = Sha256::new();
        for id in &ids {
            h.update(id.as_bytes());
            h.update(b"\n");
        }
        f.add_html(
            "Segment set",
            Some(format!(
                "{} over {} article{}",
                key_code(&hex(&h.finalize())),
                thousands(ids.len() as i64),
                plural(ids.len())
            )),
        );
    }
    if let Some(y) = probe.as_ref().and_then(|p| p.yenc.as_ref()) {
        f.key("First part CRC", y.pcrc32.map(|c| format!("{c:08x}")));
        f.key("First part yEnc CRC32", y.crc32.map(|c| format!("{c:08x}")));
    }
    if !walked {
        f.add("Segment set", Some("too many articles to list"));
    }
    if f.rows == 0 {
        return "<p class=\"dim\">Nothing to hash yet: no article of this release is on record.</p>".into();
    }
    format!("<dl class=\"facts\">{}</dl>", f.html)
}

// ---------------------------------------------------------------- the drawer

/// The compact facts of one release under its result row, or as a card.
fn drawer_html(conn: &Connection, id: i64, as_card: bool) -> String {
    let release = crate::search::get_release_with(conn, id).ok().flatten();
    let body = match &release {
        None => "<p class=\"dim\">There is no such release.</p>".to_string(),
        Some(release) => {
            let inner = crate::archive::store::view_with(conn, id);
            let names = release_info::names_with(conn, id).ok().flatten();
            let contents = release_info::contents_with(conn, id).unwrap_or_default();
            let probe = crate::probe::read(conn, id);
            let i = MetaInputs {
                release,
                inner: &inner,
                names: names.as_ref(),
                contents: &contents,
                probe: probe.as_ref(),
            };
            compact_html(&i, &look(Some(conn), id))
        }
    };
    drawer_shell(id, body, as_card)
}

/// The drawer's facts: each panel's compact rows, in one `dl`.
fn compact_html(i: &MetaInputs<'_>, l: &Looked) -> String {
    let facts: String = [
        where_panel(i, l, true),
        posted_by_panel(i, l, true),
        names_panel(i, l, true),
        inside_panel(i, l, true),
        health_panel(i, l, true),
        keys_panel(i, l, true),
    ]
    .into_iter()
    .map(|f| f.html)
    .collect();
    if facts.is_empty() {
        "<p class=\"dim\">Nothing more is known about this release yet.</p>".to_string()
    } else {
        format!("<dl class=\"facts facts-wide\">{facts}</dl>")
    }
}

/// The row under a result, spanning the table, or the card under a copy.
fn drawer_shell(id: i64, body: String, as_card: bool) -> String {
    let link = format!("<p class=\"dim\"><a href=\"/ui/release?id={id}\">Full page</a></p>");
    if as_card {
        format!("<div class=\"drawer-card\">{body}{link}</div>")
    } else {
        format!("<tr class=\"drawer-row\"><td colspan=\"{COLUMNS}\">{body}{link}</td></tr>")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::{PasswordTest, rar, zip};
    use crate::probe::ProbeInfo;
    use crate::probe::archive::ArchiveHead;
    use crate::probe::media::{AudioTrack, MediaInfo, SubtitleTrack, VideoTrack};
    use crate::probe::nest::Layer;
    use crate::probe::yenc::YencLines;
    use crate::probe::yenc_quirks::{Escapes, Quirks, Votes};

    const NAME: &str = "Cool.Movie.2024.1080p.BluRay.x264-GRP";
    const GROUP: &str = "alt.binaries.test";
    const VOLUME1: &str = "Cool.Movie.2024.1080p.BluRay.x264-GRP.part01.rar";
    /// minted 3 seconds after the Date header below
    const NYUU_ID: &str = "<AbCdEfGhIjKlMnOpQrStUvWx-1791049312000@nyuu>";
    const DATE: &str = "Sat, 03 Oct 2026 17:41:49 +0000";
    const ACCOUNT: &str = "acct-secret-9";
    const HOST: &str = "203.0.113.77";
    const MIB: i64 = 1 << 20;
    const NFO: &str = "┌──────────────────────────────────────┐
│ Title......: Cool Movie              │
│ Year.......: 2024                    │
│ Encoder....: someone                 │
│ Video......: H.264 1920x1080         │
│ https://www.themoviedb.org/movie/603 │
│ Complete name : Cool.Movie.mkv       │
└──────────────────────────────────────┘
";

    fn file(name: &str, parts: i64) -> FileInfo {
        FileInfo {
            id: 0,
            filename: name.into(),
            subject: Some(format!("[01/52] - \"{name}\" yEnc (1/{parts})")),
            subject_part: Some(1),
            expected: Some(parts),
            file_total: Some(52),
            parts_seen: parts,
            touched_at: Some(1_700_000_000),
            sealed: true,
        }
    }

    /// `n` RAR volumes, the NFO, the PAR2 index and three recovery volumes
    fn volumes(n: usize) -> Vec<FileInfo> {
        let mut files: Vec<FileInfo> = (1..=n).map(|v| file(&format!("{NAME}.part{v:02}.rar"), 50)).collect();
        files.push(file(&format!("{NAME}.nfo"), 1));
        files.push(file(&format!("{NAME}.par2"), 1));
        for (index, blocks) in [(0, 1), (1, 2), (3, 4)] {
            files.push(file(&format!("{NAME}.vol{index:03}+{blocks:02}.par2"), 2));
        }
        files
    }

    fn release() -> ReleaseRow {
        ReleaseRow {
            id: 15,
            name: NAME.into(),
            group_name: GROUP.into(),
            poster: Some("poster <p@example.com>".into()),
            posted_date: Some("2026-10-03 17:41:49".into()),
            size: Some(47 * 50 * MIB),
            complete: true,
            parts: Some(47 * 50),
        }
    }

    fn contents() -> Contents {
        Contents {
            files: 52,
            par2_files: 4,
            recovery_volumes: 3,
            data_bytes: 47 * 50 * MIB,
            recovery_bytes: 7 * MIB,
            recovery_blocks: 96,
            par2_block: Some(MIB),
            par2_blocks: Some(1000),
            ..Default::default()
        }
    }

    fn headers() -> BTreeMap<String, String> {
        let info = format!(
            "posting-account=\"{ACCOUNT}\"; posting-host=\"{HOST}\"; logging-data=\"4242\"; mail-complaints-to=\"abuse@example.com\""
        );
        [
            ("Date", DATE),
            ("Injection-Date", "Sat, 03 Oct 2026 17:41:45 +0000"),
            ("Path", "news.giganews.com!local!not-for-mail"),
            ("Injection-Info", info.as_str()),
            ("User-Agent", "Nyuu/0.4.2"),
            ("Organization", "Mock Usenet"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    /// What Nyuu's first article shows: its key order, lowercase padded CRCs, tab and space escaped at both edges
    /// and a dot at the start, nothing inside.
    fn nyuu_quirks() -> Quirks {
        let edge = Votes(40, 0);
        Quirks {
            begin: "part total line size name".into(),
            ypart: "begin end".into(),
            yend: "size part pcrc32".into(),
            upper: Votes(0, 1),
            padded: Votes(1, 0),
            blank_before_yend: Votes(0, 1),
            over_long: Votes(0, 1),
            escapes: Escapes {
                lead_tab: edge,
                lead_space: edge,
                lead_dot: edge,
                end_tab: edge,
                end_space: edge,
                mid_dot: Votes(0, 900),
                ..Escapes::default()
            },
            ..Quirks::default()
        }
    }

    fn probe() -> ProbeInfo {
        ProbeInfo {
            file: Some(VOLUME1.into()),
            yenc: Some(YencLines {
                name: Some(VOLUME1.into()),
                file_size: Some(50 * MIB as u64),
                line_len: Some(128),
                part: Some(1),
                total: Some(50),
                begin: Some(1),
                end: Some(716_800),
                yend_size: Some(716_800),
                pcrc32: Some(0xdead_beef),
                crc32: None,
            }),
            segment_size: Some(716_800),
            quirks: Some(nyuu_quirks()),
            headers: headers(),
            archive: Some(ArchiveHead {
                format: "RAR5".into(),
                method: Some("m0".into()),
                name: Some(format!("{NAME}.mkv")),
                unpacked: Some(2_400_000_000),
                packed: Some(2_400_000_000),
                ..Default::default()
            }),
            layers: vec![Layer { format: "RAR5".into(), ..Default::default() }],
            media: Some(MediaInfo {
                container: "Matroska".into(),
                duration_secs: Some(5400.4),
                video: vec![VideoTrack { codec: "H.264".into(), ..Default::default() }],
                audio: vec![AudioTrack { codec: "E-AC-3".into(), ..Default::default() }],
                subtitles: vec![SubtitleTrack { codec: "SRT".into(), ..Default::default() }],
                ..Default::default()
            }),
            nfo: Some(NFO.into()),
            ..Default::default()
        }
    }

    fn inner() -> InnerView {
        InnerView {
            files: vec![
                InnerFileRow { seq: 0, name: format!("{NAME}.mkv"), size: Some(2_400_000_000), source: SOURCE_RAR },
                InnerFileRow { seq: 1, name: format!("{NAME}.nfo"), size: Some(4_000), source: SOURCE_PAR2 },
            ],
            ..Default::default()
        }
    }

    fn looked(files: Vec<FileInfo>) -> Looked {
        Looked {
            main: files.first().cloned(),
            files,
            first_id: Some(NYUU_ID.into()),
            last: Some(SegmentRef {
                message_id: "<AbCdEfGhIjKlMnOpQrStUvWx-1791049360000@nyuu>".into(),
                part: Some(50),
                bytes: 123_456,
            }),
            posted_fp: Some(vec![1, 2, 3, 4]),
            content_fp: Some(vec![0xab, 0xcd]),
            predb: Some(PredbFacts {
                kind: 1,
                category: Some("Movies".into()),
                pre_at: Some(1_759_000_000),
                nuked: true,
            }),
            salt: Some(7),
            id_key: None,
            timing: Vec::new(),
            same: Vec::new(),
            points_to: Vec::new(),
            sidecars: Vec::new(),
        }
    }

    #[test]
    fn sidecars_show_what_they_are_where_they_came_from_and_their_text_folded() {
        let found = crate::sidecar::Found {
            name: "mediainfo".into(),
            group: "alt.binaries.x".into(),
            file: Some("mediainfo.txt".into()),
            kind: "mediainfo".into(),
            by: "segment_uid",
            text: Some("Complete name : <a>.mkv".into()),
        };
        let html = sidecars_html(&[found]).unwrap();
        assert!(html.starts_with("MediaInfo (mediainfo.txt)"), "{html}");
        assert!(html.contains("posted as mediainfo in alt.binaries.x, same file, by its Segment UID"), "{html}");
        assert!(
            html.contains(
                "<details><summary>Text</summary><pre class=\"nfo\">Complete name : &lt;a&gt;.mkv</pre></details>"
            ),
            "{html}"
        );
        assert_eq!(sidecars_html(&[]), None);
    }

    #[test]
    fn a_posted_nzb_points_to_releases_by_the_names_its_files_give() {
        let row = |seq, name: &str, source| InnerFileRow { seq, name: name.into(), size: Some(1), source };
        let inner = InnerView {
            files: vec![
                row(0, "Some.Movie.2016-GRP.par2", SOURCE_NZB),
                row(1, "Some.Movie.2016-GRP.part01.rar", SOURCE_NZB),
                row(2, "Other.Thing.mkv", SOURCE_RAR),
            ],
            ..Default::default()
        };
        assert_eq!(nzb_release_names(&inner), ["Some.Movie.2016-GRP"]);
        assert!(nzb_release_names(&self::inner()).is_empty());
        let found = ReleaseRow {
            id: 9,
            name: "Some.Movie.2016-GRP".into(),
            group_name: "alt.binaries.x".into(),
            complete: true,
            ..Default::default()
        };
        let html = points_to_html(&[("Some.Movie.2016-GRP".into(), vec![found]), ("<b>".into(), vec![])]).unwrap();
        assert!(html.contains("<a href=\"/ui/release?id=9\">Some.Movie.2016-GRP</a>"), "{html}");
        assert!(
            html.contains("in alt.binaries.x, complete")
                && html.contains("&lt;b&gt; <span class=\"dim\">(not indexed here)"),
            "{html}"
        );
        assert_eq!(points_to_html(&[]), None);
    }

    /// What the release page hands over: a well-posted scene movie, probed down to its media.
    struct Fixture {
        release: ReleaseRow,
        inner: InnerView,
        names: Option<Names>,
        contents: Contents,
        probe: Option<ProbeInfo>,
    }

    impl Fixture {
        fn new() -> Fixture {
            let names = Names { posted: NAME.into(), display: None, obfuscated: false, source: None };
            Fixture {
                release: release(),
                inner: inner(),
                names: Some(names),
                contents: contents(),
                probe: Some(probe()),
            }
        }

        /// a row and nothing else known about it
        fn empty() -> Fixture {
            Fixture {
                release: ReleaseRow { id: 1, ..Default::default() },
                inner: InnerView::default(),
                names: None,
                contents: Contents::default(),
                probe: None,
            }
        }

        fn inputs(&self) -> MetaInputs<'_> {
            MetaInputs {
                release: &self.release,
                inner: &self.inner,
                names: self.names.as_ref(),
                contents: &self.contents,
                probe: self.probe.as_ref(),
            }
        }
    }

    /// the six panels over the whole fixture
    fn full() -> String {
        let f = Fixture::new();
        panels_html(&f.inputs(), &looked(volumes(47)))
    }

    #[test]
    fn panels_skip_what_is_empty() {
        let bare = Fixture::empty();
        let html = panels_html(&bare.inputs(), &Looked::default());
        assert_eq!(html.matches("<h2>").count(), 1, "{html}");
        assert!(html.contains("<h2>Names</h2>") && html.contains("<dt>Obfuscation</dt><dd>none</dd>"), "{html}");
        assert!(!html.contains("<details"), "{html}");
        assert_eq!(Facts::new().finish("Health", Some(details(1, "parts", "Parts"))), "");

        let full = full();
        for title in ["Names", "Where and when", "Posted by", "Inside", "Health", "Keys"] {
            assert!(full.contains(&format!("<h2>{title}</h2>")), "{title} missing in {full}");
        }
        for part in ["headers", "parts", "hashes"] {
            let shell = details(15, part, "x");
            assert!(
                shell.contains(&format!("hx-get=\"/ui/meta?id=15&amp;part={part}\""))
                    && shell.contains("hx-trigger=\"toggle once\"")
                    && shell.contains("<div class=\"meta-body\">"),
                "{shell}"
            );
            assert!(full.contains(&format!("class=\"meta meta-{part}\"")), "{part} missing in {full}");
        }
        // the fold-out sits inside its panel, after the facts
        let keys = full.find("<h2>Keys</h2>").unwrap();
        let hashes = full.find("meta-hashes").unwrap();
        assert!(keys < hashes && full[hashes..].contains("</details></div>"), "{full}");
    }

    #[test]
    fn names_panel_grades_obfuscation() {
        let readable = volumes(2);
        let hidden: Vec<FileInfo> =
            (1..=3).map(|v| file(&format!("x7Qp2mZk9vL4nB8wT3rY.part{v:02}.rar"), 50)).collect();
        assert!(looks_obfuscated(&hidden[0].filename) && !looks_obfuscated(&readable[0].filename));
        let plain = InnerView::default();
        let subject = |hidden: bool, source: Option<NameSource>| Names {
            posted: NAME.into(),
            display: None,
            obfuscated: hidden,
            source: source.map(|s| s as i64),
        };
        assert_eq!(obfuscation_level(None, &plain, &readable), "none");
        // a random posted name is a hidden subject even once the row has been renamed and its flag reset
        let renamed = Names { posted: "aB3dE6fG9hJ2kL5mN8pQ".into(), ..subject(false, Some(NameSource::Par2)) };
        assert_eq!(obfuscation_level(Some(&renamed), &plain, &readable), "subject only");
        assert_eq!(obfuscation_level(Some(&renamed), &plain, &hidden), "everything (renamed from the PAR2 file)");
        assert_eq!(obfuscation_level(Some(&subject(true, None)), &plain, &readable), "subject only");
        assert_eq!(obfuscation_level(Some(&subject(false, None)), &plain, &hidden), "file names only");
        assert_eq!(obfuscation_level(Some(&subject(true, None)), &plain, &hidden), "subject and file names");
        assert_eq!(
            obfuscation_level(Some(&subject(true, Some(NameSource::Par2))), &plain, &hidden),
            "everything (renamed from the PAR2 file)"
        );
        assert_eq!(
            obfuscation_level(Some(&subject(true, None)), &inner(), &hidden),
            "everything (the names inside are readable)"
        );
        // how many posted names look random, and whether the names inside do too
        assert_eq!(
            obfuscation_fact(Some(&subject(false, None)), &plain, &hidden),
            "file names only; 3 of 3 posted file names look obfuscated"
        );
        let mut mixed = readable.clone();
        mixed.push(hidden[0].clone());
        assert_eq!(obfuscation_fact(None, &plain, &mixed), "none; 1 of 8 posted file names looks obfuscated");
        let hidden_inside = InnerView {
            files: vec![
                InnerFileRow { seq: 0, name: "x7Qp2mZk9vL4nB8wT3rY.mkv".into(), size: None, source: SOURCE_RAR },
                InnerFileRow { seq: 1, name: "q9Wv4nB7mK2pL6zX1cR8.mkv".into(), size: None, source: SOURCE_RAR },
            ],
            ..Default::default()
        };
        assert_eq!(obfuscation_fact(None, &hidden_inside, &readable), "none; the names inside are obfuscated too");

        // the three names of a file, and the stems they are compared by
        assert_eq!(stem("Foo.part01.rar"), "foo");
        assert_eq!(stem("Foo.7z.001"), "foo");
        assert_eq!(stem("foo.r00"), "foo");
        assert_eq!(stem("Foo.vol000+01.par2"), "foo");
        assert_eq!(stem("Foo.Bar.mkv"), "foo.bar");
        assert_eq!(name_agreement(Some(VOLUME1), None, None), None);
        assert_eq!(
            name_agreement(Some(VOLUME1), Some(VOLUME1), Some(&format!("{NAME}.mkv"))).as_deref(),
            Some("subject, yEnc and archive names agree")
        );
        assert_eq!(
            name_agreement(Some(VOLUME1), Some("Other.part01.rar"), None).as_deref(),
            Some("the subject and yEnc names differ")
        );
        assert_eq!(
            name_agreement(Some(VOLUME1), None, Some("Other.mkv")).as_deref(),
            Some("the archive's file name differs from the posted name")
        );
        let html = full();
        assert!(html.contains("<dt>Names</dt><dd>subject, yEnc and archive names agree</dd>"), "{html}");
        assert!(html.contains("<dt>Obfuscation</dt><dd>none</dd>"), "{html}");
    }

    #[test]
    fn subject_skeleton_and_encoded_words() {
        // a stray `=` in front of a multibyte character is kept as it is, never sliced through
        assert_eq!(decode_q("=€"), "=€".as_bytes());
        assert_eq!(decode_q("a=4"), b"a=4");
        assert_eq!(decode_q("=4=4a_b="), b"=4J b=");
        assert_eq!(decode_encoded_words("[1/2] =?UTF-8?Q?=€_x?= yEnc").as_deref(), Some("[1/2] =€ x yEnc"));
        assert_eq!(
            subject_skeleton("Foo.S01E01 [01/23] - \"foo.r00\" yEnc (1/50)"),
            "Foo.S01E01 [##/##] - \"…\" yEnc (#/##)"
        );
        assert_eq!(
            subject_skeleton("x7Qp2mZk9vL4nB8wT3rY [3 of 12] (01 of 50)"),
            "x7Qp2mZk9vL4nB8wT3rY [# of ##] (## of ##)"
        );
        assert_eq!(decode_encoded_words("=?utf-8?B?w6k=?= x").as_deref(), Some("é x"));
        assert_eq!(decode_encoded_words("=?iso-8859-1?Q?caf=E9_noir?=").as_deref(), Some("café noir"));
        // the space between two encoded words is not part of the text
        assert_eq!(decode_encoded_words("=?utf-8?Q?a?= =?utf-8?Q?b?=").as_deref(), Some("ab"));
        assert_eq!(decode_encoded_words("plain subject"), None);
        assert_eq!(decode_encoded_words("=?utf-8?B?%%%?="), None);
        let html = full();
        assert!(
            html.contains("<dt>Subject skeleton</dt><dd>[##/##] - &quot;…&quot; yEnc (#/##)</dd>")
                || html.contains("<dt>Subject skeleton</dt><dd>[##/##] - \"…\" yEnc (#/##)</dd>"),
            "{html}"
        );
        assert!(!html.contains("Subject decoded"), "nothing encoded, nothing decoded: {html}");
    }

    #[test]
    fn posted_by_parses_path_and_injection_info() {
        let path = path_facts("news.giganews.com!local!not-for-mail").unwrap();
        assert_eq!((path.hops, path.injector.as_str(), path.backbone), (2, "local", Some("Giganews")));
        let plain = path_facts("a!b!c").unwrap();
        assert_eq!((plain.hops, plain.injector.as_str(), plain.backbone), (3, "c", None));
        assert!(path_facts("not-for-mail").is_none());

        let info = headers()["Injection-Info"].clone();
        let parts = injection_parts(&info);
        assert_eq!(
            (parts.account.as_deref(), parts.host.as_deref(), parts.logging_data, parts.complaints),
            (Some(ACCOUNT), Some(HOST), true, true)
        );
        let none = injection_parts("posting-account=\"\"; x=1");
        assert!(none.account.is_none() && !none.logging_data && !none.complaints);

        assert_eq!(from_shape("poster <p@example.com>"), "Name <address>, made-up domain");
        assert_eq!(from_shape("<p@gmail.com>"), "<address> alone");
        assert_eq!(from_shape("p@gmail.com (Poster)"), "address (Name)");
        assert_eq!(from_shape("p@gmail.com"), "bare address");
        assert_eq!(from_shape("Poster"), "name only");

        // the header rows: the account, host and IP never appear, only their keys
        let keyed = redact_header("Injection-Info", &info, Some(7));
        assert!(!keyed.contains(ACCOUNT) && !keyed.contains(HOST), "{keyed}");
        assert!(keyed.contains(&format!("posting-account={}", key_code(&key12(7, ACCOUNT)))), "{keyed}");
        assert!(keyed.contains(&format!("posting-host={}", key_code(&key12(7, HOST)))), "{keyed}");
        assert!(
            keyed.contains("logging-data=<span class=\"dim\">present</span>")
                && keyed.contains("mail-complaints-to=abuse@example.com"),
            "{keyed}"
        );
        let unsalted = redact_header("Injection-Info", &info, None);
        assert!(unsalted.contains("posting-host=<span class=\"dim\">hidden</span>") && !unsalted.contains(HOST));
        assert_eq!(redact_header("NNTP-Posting-Host", HOST, Some(7)), key_code(&key12(7, HOST)));
        assert_eq!(redact_header("User-Agent", "<b>x</b>", None), "&lt;b&gt;x&lt;/b&gt;");

        // the tool against its defaults
        assert_eq!(
            tool_verdict(Some("Nyuu/0.4.2"), Some(716_800), Some(128), Some("Nyuu"), None),
            Some((true, "700 KiB articles, 128-byte lines, Nyuu ids".into()))
        );
        assert_eq!(
            tool_verdict(Some("ngPost v4.16"), Some(500_000), Some(128), Some("server-assigned (INN or Diablo)"), None),
            Some((false, "500,000 bytes per article are not the tool's defaults".into()))
        );
        assert_eq!(
            tool_verdict(Some("Nyuu/0.4.2"), None, None, Some("ngPost"), None),
            Some((false, "ids that look like ngPost are not the tool's defaults".into()))
        );
        assert!(tool_verdict(Some("Mozilla Thunderbird"), Some(716_800), Some(128), None, None).is_none());
        assert!(tool_verdict(None, Some(716_800), Some(128), Some("Nyuu"), None).is_none());

        // the yEnc quirks as one more witness
        let q = nyuu_quirks();
        assert_eq!(
            tool_verdict(Some("Nyuu/0.4.2"), Some(716_800), Some(128), Some("Nyuu"), Some(&q)),
            Some((true, "700 KiB articles, 128-byte lines, Nyuu ids, Nyuu yEnc quirks".into()))
        );
        let ngpost = Quirks {
            yend: "size pcrc32".into(),
            padded: Votes(0, 3),
            blank_before_yend: Votes(1, 0),
            over_long: Votes::default(),
            escapes: Escapes {
                end_tab: Votes(0, 9),
                end_space: Votes(0, 9),
                lead_tab: Votes::default(),
                lead_space: Votes::default(),
                ..q.escapes.clone()
            },
            ..q.clone()
        };
        assert_eq!(
            tool_verdict(Some("Nyuu/0.4.2"), Some(716_800), Some(128), Some("Nyuu"), Some(&ngpost)),
            Some((false, "yEnc quirks of ngPost or NewsUP are not the tool's defaults".into()))
        );
        assert_eq!(
            tool_verdict(Some("ngPost v4.16"), None, None, None, Some(&ngpost)),
            Some((true, "ngPost yEnc quirks".into()))
        );
        // postie has no article defaults here: its yEnc alone answers
        assert_eq!(
            tool_verdict(Some("postie/0.0.35"), None, None, None, Some(&q)),
            Some((true, "postie yEnc quirks".into()))
        );
        assert!(tool_verdict(Some("JBinUp"), None, None, None, Some(&q)).is_none(), "its yEnc isn't documented");
        assert!(tool_verdict(Some("Nyuu/0.4.2"), None, None, None, Some(&Quirks::default())).is_none());

        let html = full();
        assert!(html.contains("<dt>From shape</dt><dd>Name &lt;address&gt;, made-up domain</dd>"), "{html}");
        assert!(html.contains("<dt>Injected at</dt><dd>local (2 hops), Giganews</dd>"), "{html}");
        assert!(
            html.contains(
                "<dt>Injection-Info</dt><dd>posting-account, posting-host, logging-data, mail-complaints-to present; the account and host are shown as keys only</dd>"
            ),
            "{html}"
        );
        assert!(html.contains("<dt>Ids look like</dt><dd>Nyuu</dd>"), "{html}");
        assert!(
            html.contains("<dt>Tool</dt><dd>Nyuu/0.4.2 <span class=\"tag tag-ok\">FINGERPRINT AGREES</span>"),
            "{html}"
        );
        assert!(
            html.contains(
                "FINGERPRINT AGREES</span> 700 KiB articles, 128-byte lines, Nyuu ids, Nyuu yEnc quirks</dd>"
            ),
            "{html}"
        );
        assert!(
            html.contains(
                "<dt>yEnc quirks</dt><dd>like Nyuu or postie: =ybegin part total line size name; =ypart begin end; =yend size part pcrc32; lowercase zero-padded CRCs; escapes leading tabs, leading spaces, leading dots, trailing tabs, trailing spaces</dd>"
            ),
            "{html}"
        );
        assert!(!html.contains(ACCOUNT) && !html.contains(HOST), "{html}");
    }

    #[test]
    fn keys_are_salted_and_stable() {
        assert_eq!(key12(7, ACCOUNT), key12(7, ACCOUNT));
        assert_ne!(key12(7, ACCOUNT), key12(8, ACCOUNT));
        assert_ne!(key12(7, ACCOUNT), key12(7, HOST));
        assert!(key12(7, ACCOUNT).len() == 12 && key12(7, ACCOUNT).chars().all(|c| c.is_ascii_hexdigit()));
        assert!(key16(NYUU_ID.as_bytes()).len() == 16 && key16(b"a") != key16(b"b"));
        assert_eq!(hex(&[0, 0xab, 0xff]), "00abff");

        let f = Fixture::new();
        let l = looked(volumes(3));
        let full = keys_panel(&f.inputs(), &l, false).finish("Keys", None);
        assert!(!full.contains(ACCOUNT) && !full.contains(HOST), "{full}");
        for label in [
            "First-id key",
            "Account key",
            "Host key",
            "Tail bytes",
            "Posted fingerprint",
            "Content fingerprint",
            "Structural key",
            "NZBLNK",
        ] {
            assert!(full.contains(&format!("<dt>{label}</dt>")), "{label} missing in {full}");
        }
        assert!(full.contains(&key_code(&key12(7, ACCOUNT))) && full.contains(&key_code(&key12(7, HOST))), "{full}");
        assert!(full.contains(&key_code(&key16(NYUU_ID.as_bytes()))), "{full}");
        assert!(
            full.contains("<dd>123,456 bytes</dd>")
                && full.contains(&key_code("01020304"))
                && full.contains(&key_code("abcd")),
            "{full}"
        );
        assert!(!full.contains("Collection key"), "no id key, no recomputed collection key: {full}");
        let compact = keys_panel(&f.inputs(), &l, true).html;
        assert!(
            compact.contains("<dt>Account key</dt>") && !compact.contains("Host key") && !compact.contains("NZBLNK"),
            "{compact}"
        );
        // no salt yet: the keys that need one are left out rather than made up
        let unsalted = Looked { salt: None, ..looked(volumes(3)) };
        let html = keys_panel(&f.inputs(), &unsalted, false).html;
        assert!(html.contains("First-id key") && !html.contains("Account key") && !html.contains("Host key"), "{html}");
    }

    #[test]
    fn message_id_grammar_is_classified() {
        let cases = [
            (NYUU_ID, "Nyuu"),
            ("<abcdefghijklmnopqrstuvwx-1791049312000@nyuu>", "Nyuu-like (an imitation, or an older Nyuu)"),
            ("<0123456789abcdef0123456789abcdef@ngPost>", "ngPost"),
            ("<part3of50.ABCDEFGHIJKLMNOPQRST@powerpost2000AA.local>", "PowerPost family"),
            ("<x_1o5@JBinUp.local>", "JBinUp"),
            ("<x@JBinDown.local>", "JBinDown"),
            ("<x@GoPostStuff>", "GoPostStuff"),
            ("<x@host.NewsUP>", "NewsUP"),
            ("<x@riverup>", "Upstream"),
            ("<x@a.geri>", "geri"),
            ("<x@spot.net>", "Spotnet"),
            ("<x@UFK>", "server-assigned (Usenet.Farm)"),
            ("<abc$def@news.example.com>", "server-assigned (INN or Diablo)"),
            ("<abc@news.giganews.com>", "provider server-assigned"),
            ("<abc@example.org>", "unknown"),
        ];
        for (id, grammar) in cases {
            assert_eq!(id_grammar(id), Some(grammar), "{id}");
        }
        assert_eq!(id_grammar("nonsense"), None);
        assert_eq!(id_grammar("<@x>"), None);
        assert_eq!(id_grammar("<x@>"), None);
    }

    #[test]
    fn date_styles_and_skews() {
        assert_eq!(date_style(DATE).as_deref(), Some("RFC 5322, day name, 4-digit year, numeric zone"));
        assert_eq!(
            date_style("03 Oct 26 17:41:49 GMT").as_deref(),
            Some("RFC 5322 with quirks: no day name, 2-digit year, named zone (GMT)")
        );
        assert_eq!(
            date_style("Sat, 03 Oct 2026 17:41:49 +0000 (UTC)").as_deref(),
            Some("RFC 5322 with quirks: a comment after the zone")
        );
        assert_eq!(date_style("Sat, 03 Oct 2026 17:41:49").as_deref(), Some("RFC 5322 with quirks: no zone"));
        assert_eq!(date_style("yesterday").as_deref(), Some("not an RFC 5322 date"));
        assert_eq!(date_style("  "), None);
        assert_eq!(posted_on("Mon, 05 Oct 2026 03:14:00 +0100").as_deref(), Some("a Monday at 03:14 +0100"));
        assert_eq!(posted_on("Sat, 03 Oct 2026 17:41:49 -0330").as_deref(), Some("a Saturday at 17:41 -0330"));
        assert_eq!(posted_on("nonsense"), None);
        assert_eq!(skew_secs(DATE, "Sat, 03 Oct 2026 17:41:45 +0000"), Some(4));
        assert_eq!(skew_secs(DATE, "x"), None);
        assert_eq!(fmt_skew(4), "+4 s");
        assert_eq!(fmt_skew(-120), "-2 min");
        assert_eq!(fmt_skew(7200), "+2 h");
        assert_eq!(fmt_skew(-172_800), "-2 d");
        assert_eq!(fmt_unix(1_700_000_000), "2023-11-14 22:13 UTC");
        assert_eq!(id_timestamp("<1700000000.42@x>"), Some(1_700_000_000));
        assert_eq!(id_timestamp(NYUU_ID), Some(1_791_049_312));
        assert_eq!(id_timestamp("<part3of50.abc@powerpost2000AA.local>"), None);
        assert_eq!(id_timestamp("<99999999999@x>"), None);

        let html = full();
        assert!(html.contains("<dt>Date style</dt><dd>RFC 5322, day name, 4-digit year, numeric zone</dd>"), "{html}");
        assert!(html.contains("<dt>Posted on</dt><dd>a Saturday at 17:41 +0000</dd>"), "{html}");
        assert!(html.contains("<dt>Clock skew</dt><dd>+4 s against Injection-Date</dd>"), "{html}");
        assert!(html.contains("<dt>Id timestamp</dt><dd>2026-10-03 17:41 UTC, +3 s against Date</dd>"), "{html}");
    }

    #[test]
    fn nfo_fields_links_and_reports() {
        let links = nfo_urls(
            "see https://www.themoviedb.org/movie/603 and thetvdb.com/series/the-show, anidb.net/anime/1 and javascript:alert(1) https://www.themoviedb.org/movie/603",
        );
        assert_eq!(
            links,
            vec![
                ("TMDb", "https://www.themoviedb.org/movie/603".to_string()),
                ("TVDB", "https://thetvdb.com/series/the-show".into()),
                ("AniDB", "https://anidb.net/anime/1".into()),
            ]
        );
        assert!(nfo_urls("nothing here").is_empty());
        let music = nfo_urls("musicbrainz.org/release/0f4c2a3e-1b2c-4d5e-8f90-123456789abc discogs.com/release/123");
        assert_eq!(music.len(), 2, "{music:?}");
        assert!(
            music.iter().any(|(l, u)| *l == "MusicBrainz"
                && u == "https://musicbrainz.org/release/0f4c2a3e-1b2c-4d5e-8f90-123456789abc")
        );
        assert!(music.iter().any(|(l, u)| *l == "Discogs" && u == "https://www.discogs.com/release/123"));

        let fields = nfo_fields(NFO);
        assert_eq!(
            fields,
            vec![
                ("Title".to_string(), "Cool Movie".to_string()),
                ("Year".into(), "2024".into()),
                ("Video".into(), "H.264 1920x1080".into()),
            ]
        );
        let many = "Title: a\nYear: 2000\nSource: s\nVideo: v\nResolution: r\nAudio: a\nRuntime: 1h\nSize: 1\nLanguage: en\nSubtitles: en\nGenre: g\nFormat: f\nTitle: again\n";
        let fields = nfo_fields(many);
        assert_eq!(fields.len(), NFO_FIELDS_MAX, "{fields:?}");
        assert_eq!(fields.iter().filter(|(l, _)| l == "Title").count(), 1, "{fields:?}");
        assert_eq!(nfo_reports(NFO), vec!["MediaInfo"]);
        assert_eq!(nfo_reports("DISC INFO:\nUnique ID : 1"), vec!["MediaInfo", "BDInfo"]);
        assert!(nfo_reports("plain").is_empty());
        assert_eq!(nfo_shape("abc\r\nde\r\n\r\n"), (3, 2, false));
        let (width, lines, art) = nfo_shape(NFO);
        assert!(width >= 38 && lines == 8 && art, "{width} {lines} {art}");

        let html = full();
        assert!(
            html.contains(
                "<a href=\"https://www.themoviedb.org/movie/603\" rel=\"noopener noreferrer\">TMDb: themoviedb.org/movie/603</a>"
            ),
            "{html}"
        );
        assert!(
            html.contains("<dt>NFO says</dt><dd>Title: Cool Movie · Year: 2024 · Video: H.264 1920x1080</dd>"),
            "{html}"
        );
        assert!(!html.contains("Encoder") && !html.contains("someone"), "{html}");
        assert!(html.contains("<dt>NFO reports</dt><dd>includes a MediaInfo report</dd>"), "{html}");
        assert!(html.contains("lines, CP437 art</dd>"), "{html}");
        let pre = format!("<dt>PreDB</dt><dd>matched by the exact title, Movies, pre {}", fmt_unix(1_759_000_000));
        assert!(html.contains(&pre) && html.contains("NUKED"), "{html}");
    }

    #[test]
    fn tells_come_in_name_order() {
        assert_eq!(repack_tells("X.REPACK.PROPER-GRP"), vec!["REPACK", "PROPER"]);
        assert_eq!(repack_tells("Some.Show.S01E01.READ.NFO-GRP"), vec!["READNFO"]);
        assert_eq!(repack_tells("internal.repack.INTERNAL"), vec!["INTERNAL", "REPACK"]);
        assert_eq!(repack_tells("Plain.Movie.2024-GRP"), Vec::<&str>::new());
        assert!(!full().contains("<dt>Tells</dt>"));
        let mut f = Fixture::new();
        if let Some(n) = f.names.as_mut() {
            n.posted = "Cool.Movie.2024.REPACK.1080p.BluRay.x264-GRP".into();
        }
        let html = names_panel(&f.inputs(), &looked(volumes(2)), false).html;
        assert!(html.contains("<dt>Tells</dt><dd>REPACK</dd>"), "{html}");
    }

    #[test]
    fn volume_scheme_from_names() {
        let c = contents();
        let size = Some(47 * 50 * MIB);
        assert_eq!(volume_scheme(&volumes(47), size, &c).as_deref(), Some("47 volumes × 50.0 MB (.partNN.rar)"));
        let old: Vec<FileInfo> = ["x.rar", "x.r00", "x.r01", "x.rev"].iter().map(|n| file(n, 10)).collect();
        assert_eq!(
            volume_scheme(&old, Some(300 * MIB), &Contents::default()).as_deref(),
            Some("3 volumes × 100.0 MB (old-style .rNN), 1 .rev")
        );
        let sevenz: Vec<FileInfo> = ["x.7z.001", "x.7z.002"].iter().map(|n| file(n, 10)).collect();
        assert_eq!(volume_scheme(&sevenz, None, &Contents::default()).as_deref(), Some("2 volumes (.7z.NNN)"));
        let zipped = [file("x.z01", 1), file("x.zip", 1)];
        assert_eq!(volume_scheme(&zipped, None, &Contents::default()).as_deref(), Some("2 volumes (.zNN)"));
        assert_eq!(
            volume_scheme(&[file("x.001", 1)], None, &Contents::default()).as_deref(),
            Some("1 volume (raw split .NNN)")
        );
        assert_eq!(volume_scheme(&[file("x.mkv", 1)], None, &Contents::default()), None);

        assert_eq!(
            contents_profile(&volumes(47), size, &c).as_deref(),
            Some("47 RAR volumes (about 50.0 MB each), 4 PAR2 (3 recovery), NFO")
        );
        let mixed: Vec<FileInfo> = ["a.mkv", "a-sample.mkv", "a.sfv", "a.srt", "a.jpg", "a.bin", "a.flac"]
            .iter()
            .map(|n| file(n, 1))
            .collect();
        assert_eq!(
            contents_profile(&mixed, None, &Contents::default()).as_deref(),
            Some("SFV, 1 sample, 1 video file, 1 audio file, 1 image, 1 text file, 1 other file")
        );
        assert_eq!(contents_profile(&[], None, &Contents::default()), None);
        let single = [file("x.rar", 1), file("x.nfo", 1)];
        assert_eq!(
            contents_profile(&single, Some(MIB), &Contents::default()).as_deref(),
            Some("1 RAR volume (1.0 MB), NFO")
        );
        assert!(kind_of("x.part01.rar") == FileKind::RarVolume && kind_of("x.r00") == FileKind::RarVolume);
        assert!(kind_of("x.7z.001") == FileKind::SevenZVolume && kind_of("x.z01") == FileKind::ZipVolume);
        assert!(kind_of("x.001") == FileKind::RawSplit && kind_of("x.rev") == FileKind::Rev);
        assert!(kind_of("x.par2") == FileKind::Par2Index && kind_of("x.vol000+01.par2") == FileKind::Par2Recovery);
        assert!(
            kind_of("X.NFO") == FileKind::Nfo && kind_of("x.srr") == FileKind::Srr && kind_of("x.nzb") == FileKind::Nzb
        );

        assert_eq!(listing_source(&inner()).as_deref(), Some("names from the PAR2 index and the RAR headers"));
        assert_eq!(listing_source(&InnerView::default()), None);
        let html = full();
        assert!(html.contains("<dt>Volumes</dt><dd>47 volumes × 50.0 MB (.partNN.rar)</dd>"), "{html}");
        assert!(
            html.contains("<dt>Contents</dt><dd>47 RAR volumes (about 50.0 MB each), 4 PAR2 (3 recovery), NFO</dd>"),
            "{html}"
        );
        assert!(html.contains("<dt>Listed from</dt><dd>names from the PAR2 index and the RAR headers</dd>"), "{html}");
    }

    #[test]
    fn repair_capacity_from_contents() {
        assert_eq!(repair_capacity(&contents()).as_deref(), Some("slice 1.0 MB, up to 96 of 1,000 blocks (9.6%)"));
        assert_eq!(
            repair_capacity(&Contents { recovery_blocks: 30, ..contents() }).as_deref(),
            Some("slice 1.0 MB, up to 30 of 1,000 blocks (3%)")
        );
        assert_eq!(repair_capacity(&Contents::default()), None);
        assert_eq!(per_mille_text(30), "3%");
        assert_eq!(per_mille_text(96), "9.6%");
        assert_eq!(per_mille_text(1000), "100%");

        let head = probe().archive.unwrap();
        let files = volumes(47);
        let size = Some(47 * 50 * MIB);
        // the archive's listing when all of it is sized, else the archive's figure, else the data less yEnc's
        // overhead; the PAR2 inventory's rows are not added to the archive's listing
        let about = |n: i64| Some(Unpacked::About(n));
        assert_eq!(unpacked_estimate(&files, size, &contents(), &inner(), Some(&head)), about(2_400_000_000));
        assert_eq!(
            unpacked_estimate(&files, size, &contents(), &InnerView::default(), Some(&head)),
            about(2_400_000_000)
        );
        assert_eq!(
            unpacked_estimate(&files, size, &contents(), &InnerView::default(), None),
            about(47 * 50 * MIB * 100 / 103)
        );
        // a listing that stops short of the archive's end (one episode of a season pack) is a lower bound,
        // and so is any listing when nothing says what was posted
        let episode = InnerView {
            files: vec![InnerFileRow {
                seq: 0,
                name: "Show.S01E01.mkv".into(),
                size: Some(350_000_000),
                source: SOURCE_RAR,
            }],
            ..Default::default()
        };
        assert_eq!(
            unpacked_estimate(&files, size, &contents(), &episode, Some(&head)),
            Some(Unpacked::AtLeast(350_000_000))
        );
        assert_eq!(
            unpacked_estimate(&files, None, &Contents::default(), &inner(), Some(&head)),
            Some(Unpacked::AtLeast(2_400_000_000))
        );
        let first_of_many = ArchiveHead { unpacked: Some(350_000_000), ..head.clone() };
        assert_eq!(
            unpacked_estimate(&files, size, &contents(), &InnerView::default(), Some(&first_of_many)),
            Some(Unpacked::AtLeast(350_000_000))
        );
        assert_eq!(
            unpacked_estimate(&[file("x.nfo", 1)], Some(4000), &Contents::default(), &InnerView::default(), None),
            None
        );
        // a PAR2 inventory of archive volumes says nothing about what is inside them
        let par2_of_volumes = InnerView {
            files: (1..=47i64)
                .map(|v| InnerFileRow {
                    seq: v,
                    name: format!("{NAME}.part{v:02}.rar"),
                    size: Some(50 * MIB),
                    source: SOURCE_PAR2,
                })
                .collect(),
            ..Default::default()
        };
        assert_eq!(
            unpacked_estimate(&files, size, &contents(), &par2_of_volumes, None),
            about(47 * 50 * MIB * 100 / 103)
        );
        // one of plain files is the contents
        let par2_of_contents = InnerView {
            files: vec![
                InnerFileRow { seq: 0, name: format!("{NAME}.mkv"), size: Some(2_400_000_000), source: SOURCE_PAR2 },
                InnerFileRow { seq: 1, name: format!("{NAME}.nfo"), size: Some(4_000), source: SOURCE_PAR2 },
            ],
            ..Default::default()
        };
        let plain = [file(&format!("{NAME}.mkv"), 3000), file(&format!("{NAME}.nfo"), 1)];
        assert_eq!(unpacked_estimate(&plain, size, &contents(), &par2_of_contents, None), about(2_400_004_000));

        let random: Vec<FileInfo> =
            (1..=47).map(|v| file(&format!("x7Qp2mZk9vL4nB8wT3rY.part{v:02}.rar"), 50)).collect();
        assert_eq!(
            packer_tells(Some(&head), &[], &random, size, &contents()),
            vec!["ngPost's defaults: stored, 50 MB volumes, random names"]
        );
        assert_eq!(packer_tells(Some(&head), &[], &files, size, &contents()), vec!["stored, not compressed (m0)"]);
        let nested = [
            Layer { format: "RAR5".into(), ..Default::default() },
            Layer { format: "7z".into(), ..Default::default() },
        ];
        let compressed = ArchiveHead { method: Some("m3".into()), ..head.clone() };
        assert_eq!(
            packer_tells(Some(&compressed), &nested, &files, size, &contents()),
            vec!["an archive inside an archive (7z in RAR5)"]
        );
        assert!(packer_tells(None, &[], &files, size, &contents()).is_empty());

        let html = full();
        assert!(html.contains("<dt>PAR2</dt><dd>slice 1.0 MB, up to 96 of 1,000 blocks (9.6%)</dd>"), "{html}");
        assert!(html.contains("<dt>Unpacks to</dt><dd>about 2.2 GB</dd>"), "{html}");
        let mut partial = Fixture::new();
        partial.inner = episode;
        let html = inside_panel(&partial.inputs(), &looked(volumes(47)), false).html;
        assert!(html.contains("<dt>Unpacks to</dt><dd>at least 333.8 MB (partial listing)</dd>"), "{html}");
        // 47 volumes: the head's packed size is one volume's slice of the first file, soo no ratio
        assert!(!html.contains("<dt>Packed</dt>"), "{html}");
        let f = Fixture::new();
        let one = inside_panel(&f.inputs(), &looked(vec![file("x.rar", 50)]), false).html;
        assert!(one.contains("<dt>Packed</dt><dd>2.2 GB of 2.2 GB (100%), stored</dd>"), "{one}");
        assert_eq!(archive_volumes(&volumes(47)), 47);
        assert_eq!(archive_volumes(&[file("x.7z.001", 1), file("x.7z.002", 1), file("x.par2", 1)]), 2);
        assert!(html.contains("<dt>Packed by</dt><dd>stored, not compressed (m0)</dd>"), "{html}");
    }

    #[test]
    fn health_reads_the_control_lines() {
        let html = full();
        assert!(
            html.contains("<dt>yEnc control lines</dt><dd>part 1 of 50, bytes 1–716,800, =yend size 716,800</dd>"),
            "{html}"
        );
        assert!(html.contains(&format!("<dt>Last arrival</dt><dd>{}</dd>", fmt_unix(1_700_000_000))), "{html}");
        assert!(!html.contains("<dt>Numbering</dt>"), "{html}");
        // the subject and the yEnc lines disagree on the part count
        let mut f = Fixture::new();
        if let Some(y) = f.probe.as_mut().and_then(|p| p.yenc.as_mut()) {
            y.total = Some(49);
        }
        let mut files = volumes(3);
        files[0].touched_at = Some(1_700_007_200);
        let html = health_panel(&f.inputs(), &looked(files), false).html;
        assert!(html.contains("<dt>Numbering</dt><dd>subject says 1/50, yEnc says 1 of 49</dd>"), "{html}");
        assert!(
            html.contains(&format!("<dt>Last arrival</dt><dd>{}, grew for 2 h</dd>", fmt_unix(1_700_007_200))),
            "{html}"
        );
        // the compact row: what the server still has
        let mut probe = probe();
        probe.health = BTreeMap::from([("first".to_string(), true), ("last".to_string(), false)]);
        f.probe = Some(probe);
        let compact = health_panel(&f.inputs(), &Looked::default(), true).html;
        assert!(
            compact.contains("FIRST THERE") && compact.contains("LAST GONE") && !compact.contains("MIDDLE"),
            "{compact}"
        );
        assert!(compact.starts_with("<dt>On the server</dt>"), "{compact}");
    }

    #[test]
    fn structural_key_password_check_and_nzblnk() {
        let media = probe().media.unwrap();
        assert_eq!(structural_key(&media), Some(key16(b"v:H.264|a:E-AC-3|s:SRT|c:0|d:5400")));
        let longer = MediaInfo { duration_secs: Some(5401.0), ..media.clone() };
        assert_ne!(structural_key(&longer), structural_key(&media));
        assert_eq!(structural_key(&MediaInfo::default()), None);

        let rar5 = PasswordTest::Rar5(rar::PasswordCheck { kdf: 15, salt: [0xab; 16], check: [1; 8] });
        assert_eq!(password_check(&rar5), format!("RAR5 kdf 2^15, salt {}, check 0101010101010101", "ab".repeat(16)));
        let crypto = PasswordTest::Zip(zip::PasswordCheck::Crypto { header: [0; 12], check: 0x7f });
        assert_eq!(password_check(&crypto), "ZipCrypto check byte 7f");
        let aes = PasswordTest::Zip(zip::PasswordCheck::Aes { strength: 1, salt: [0x11; 16], verifier: [0x22, 0x33] });
        assert_eq!(password_check(&aes), format!("ZIP AES-128, salt {}, verifier 2233", "11".repeat(8)));
        let aes256 =
            PasswordTest::Zip(zip::PasswordCheck::Aes { strength: 3, salt: [0x11; 16], verifier: [0x22, 0x33] });
        assert!(password_check(&aes256).starts_with(&format!("ZIP AES-256, salt {}", "11".repeat(16))));
        let mut f = Fixture::new();
        f.inner.password_check = Some(crypto);
        let html = keys_panel(&f.inputs(), &looked(volumes(2)), false).html;
        assert!(
            html.contains(&format!("<dt>Password check</dt><dd>{}</dd>", key_code("ZipCrypto check byte 7f"))),
            "{html}"
        );

        let subject = format!("[01/52] - \"{VOLUME1}\" yEnc (1/50)");
        let mut f = Fixture::new();
        assert_eq!(nzblnk(&f.inputs(), &subject), format!("nzblnk:?t={NAME}&h={NAME}&g={GROUP}"));
        // a reader's name as the title, the poster's {{password}} out of it and into p
        f.names.as_mut().unwrap().display = Some("Cool Movie 2024 {{s3cret}}".into());
        let link = nzblnk(&f.inputs(), &subject);
        assert_eq!(link, format!("nzblnk:?t=Cool+Movie+2024&h={NAME}&g={GROUP}&p=s3cret"));
        let parsed = crate::nzblnk::NzbLnk::parse(&link).unwrap();
        assert_eq!((parsed.title.as_str(), parsed.password.as_deref()), ("Cool Movie 2024", Some("s3cret")));
        // an obfuscated subject: the quoted file name's stem
        f.names.as_mut().unwrap().display = None;
        assert_eq!(nzblnk(&f.inputs(), "\"a1b2c3d4.par2\" yEnc"), format!("nzblnk:?t={NAME}&h=a1b2c3d4&g={GROUP}"));
        let html = keys_panel(&f.inputs(), &looked(volumes(2)), false).html;
        assert!(html.contains(&format!("<dt>NZBLNK</dt><dd><a href=\"nzblnk:?t={NAME}&amp;h=")), "{html}");
    }

    #[test]
    fn the_collection_key_is_the_ingests_own() {
        assert!(is_weak("g\0n\x005\0a\0d") && !is_weak("g\0n\x005"));
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        crate::db::create_db_at(&main).unwrap();
        let subject = format!("[01/52] - \"{VOLUME1}\" yEnc (1/50)");
        let poster = "poster <p@example.com>";
        let date = "2026-10-03 17:41:49";
        let parsed = crate::parser::parse_subject(&subject).unwrap();
        let article = crate::parser::Article {
            subject: subject.clone(),
            author: poster.into(),
            date: date.into(),
            message_id: NYUU_ID.into(),
            bytes: 716_800,
            filename: Some(parsed.filename.clone()),
            release_name: Some(parsed.release_name.clone()),
            part: Some(1),
            total_parts: Some(50),
            file_index: Some(1),
            file_total: Some(52),
            ..Default::default()
        };
        let collection = crate::collections::Collections::new(GROUP)
            .of(&article, &parsed)
            .expect("a file of a 52-file post belongs to a collection");
        let release = crate::parser::Release {
            name: parsed.release_name.clone(),
            articles: vec![article],
            size: 716_800,
            group: GROUP.into(),
            poster: poster.into(),
            date: date.into(),
            collection: Some(collection.clone()),
            ..Default::default()
        };
        store::save(&main, &[release]).unwrap();
        let shard = crate::db::open_shard(&store::shard_path(&main, store::shard_of(GROUP))).unwrap();
        let saved: i64 = shard.query_row("select key from collections", [], |r| r.get(0)).unwrap();
        let key = store::IdKey::of(&shard).unwrap();
        assert_eq!(collection_key(&key, GROUP, &subject, poster, date), Some((saved, is_weak(&collection))));
        assert_eq!(collection_key(&key, GROUP, "no file name here", poster, date), None);
    }

    #[test]
    fn everything_from_usenet_is_escaped() {
        let hostile = "<script>alert(1)</script>";
        let mut f = Fixture::new();
        f.release.name = format!("{hostile}.mkv");
        f.release.poster = Some(format!("<b>p</b> <p@x.org> {hostile}"));
        f.release.group_name = "<i>g</i>".into();
        if let Some(n) = f.names.as_mut() {
            n.posted = hostile.into();
        }
        if let Some(p) = f.probe.as_mut() {
            p.headers.insert("User-Agent".into(), format!("Nyuu {hostile}"));
            p.headers.insert("Path".into(), format!("{hostile}!not-for-mail"));
            p.headers.insert(
                "Injection-Info".into(),
                format!("posting-account=\"{hostile}\"; mail-complaints-to=\"{hostile}\""),
            );
            p.nfo = Some(format!("Title: {hostile}\nhttps://www.themoviedb.org/movie/603\" onclick=\"{hostile}"));
            if let Some(y) = p.yenc.as_mut() {
                y.name = Some(hostile.into());
            }
            if let Some(a) = p.archive.as_mut() {
                a.name = Some(hostile.into());
            }
        }
        f.inner.files[0].name = hostile.into();
        let mut files = volumes(2);
        files[0].filename = format!("{hostile}.part01.rar");
        files[0].subject = Some(format!("[01/02] - \"{hostile}\" yEnc (1/50)"));
        let mut l = looked(files.clone());
        l.first_id = Some(format!("<{hostile}@nyuu>"));
        l.predb = Some(PredbFacts { kind: 1, category: Some(hostile.into()), pre_at: None, nuked: false });
        let rendered = [
            panels_html(&f.inputs(), &l),
            compact_html(&f.inputs(), &l),
            parts_table(&files, &HashMap::new(), true),
            redact_header("X-Newsreader", hostile, None),
        ];
        for html in rendered {
            assert!(
                !html.contains("<script")
                    && !html.contains("<b>")
                    && !html.contains("<i>")
                    && !html.contains("onclick"),
                "{html}"
            );
            assert!(html.contains("&lt;script&gt;"), "{html}");
        }
    }

    /// a file posted from `first` to `last` by the poster's clock, `articles` articles of 100 lines
    fn timed(first: i64, last: i64, articles: i64, numbers: (i64, i64)) -> FileTiming {
        FileTiming {
            first_date: Some(first),
            last_date: Some(last),
            number_first: Some(numbers.0),
            number_last: Some(numbers.1),
            lines: Some(articles * 100),
            from_variants: 1,
            articles,
            ..Default::default()
        }
    }

    /// 2026-10-02 10:00 by the poster's clock
    const TEN: i64 = 1_790_935_200;

    #[test]
    fn timing_facts_read_the_rows() {
        // three files: two back to back, the third after a 31 minute gap, 30 articles over an hour
        let rows = vec![
            timed(TEN, TEN + 600, 10, (100, 109)),
            timed(TEN + 700, TEN + 1300, 10, (110, 119)),
            timed(TEN + 3200, TEN + 3800, 10, (120, 129)),
        ];
        assert_eq!(
            first_and_last(&rows).as_deref(),
            Some("2026-10-02 10:00 to 2026-10-02 11:03 by the poster's clock, 1 h")
        );
        let rate = posting_rate(&rows, Some(3_800_000)).unwrap();
        assert!(rate.starts_with("0.5 articles a minute, ") && rate.ends_with("/s"), "{rate}");
        assert_eq!(posting_rate(&rows, None).as_deref(), Some("0.5 articles a minute"));
        assert_eq!(file_order(&rows).as_deref(), Some("3 files one after another"));
        assert_eq!(sessions(&rows).as_deref(), Some("2 sessions, gaps over 30 min between files"));
        assert_eq!(article_numbers(&rows).as_deref(), Some("100–129 where it was read, adjacent"));
        assert_eq!(threaded_replies(&rows), "none");
        assert_eq!(crossposts(&rows), None, "no Xref from this server");
        assert_eq!(from_variants(&rows).as_deref(), Some("one From throughout"));
        assert_eq!(
            lines_per_article(&rows, Some(3_800_000)).as_deref(),
            Some("100 lines an article, 1,266 bytes a line")
        );
        assert_eq!(lines_per_article(&rows, None).as_deref(), Some("100 lines an article"));
        assert_eq!(late_articles(&rows), "none");
        assert_eq!(lines_key(&rows, Some(30)).as_deref(), Some(key16(b"1000,1000,1000").as_str()));
        assert_eq!(lines_key(&rows, Some(31)), None, "an article saved before timing was kept: no key");
        assert_eq!(lines_key(&rows, None), None);

        // files that each keep to one From, but not the same one
        let mut two = rows.clone();
        two[0].from_hash = Some(1);
        two[1].from_hash = Some(2);
        two[2].from_hash = Some(1);
        assert_eq!(from_variants(&two).as_deref(), Some("2 Froms, one a file"));
        two[1].from_hash = Some(1);
        assert_eq!(from_variants(&two).as_deref(), Some("one From throughout"));

        // a minute or less: no rate, and the span says so
        let quick = vec![timed(TEN, TEN + 30, 3, (1, 3))];
        assert_eq!(
            first_and_last(&quick).as_deref(),
            Some("2026-10-02 10:00 to 2026-10-02 10:00 by the poster's clock, within a minute")
        );
        assert_eq!(posting_rate(&quick, Some(1)), None);
        assert_eq!(file_order(&quick), None, "one file has no order");
        assert_eq!(sessions(&quick), None);

        // interleaved files, other articles between the numbers, threads, Xref, rotating Froms, late rows
        let mut busy = vec![timed(TEN, TEN + 600, 10, (100, 109)), timed(TEN + 300, TEN + 900, 10, (111, 120))];
        busy[0].references_seen = 3;
        busy[0].xref_groups_max = Some(1);
        busy[1].xref_groups_max = Some(3);
        busy[0].from_variants = 3;
        busy[1].from_variants = 2;
        busy[1].late_articles = 2;
        busy[1].lines = None;
        assert_eq!(file_order(&busy).as_deref(), Some("interleaved: 1 of 2 files started before an earlier one ended"));
        assert_eq!(sessions(&busy).as_deref(), Some("one session"));
        assert_eq!(article_numbers(&busy).as_deref(), Some("100–120 where it was read, 1 other article between them"));
        assert_eq!(threaded_replies(&busy), "3 articles carried References");
        assert_eq!(crossposts(&busy).as_deref(), Some("3 groups at most, by Xref"));
        assert_eq!(crossposts(&busy[..1]).as_deref(), Some("one group, by Xref"));
        assert_eq!(from_variants(&busy).as_deref(), Some("From changed 3 times within files (rotating identity)"));
        assert_eq!(late_articles(&busy), "2 arrived after the file sealed");
        assert_eq!(lines_key(&busy, Some(20)), None, "a file without lines keys nothing");
        assert_eq!(lines_per_article(&busy, Some(1)).as_deref(), Some("50 lines an article, 0 bytes a line"));

        // rows without dates or numbers say nothing about them
        let bare = vec![FileTiming { articles: 4, from_variants: 1, ..Default::default() }];
        assert_eq!(first_and_last(&bare), None);
        assert_eq!(article_numbers(&bare), None);
        assert_eq!(from_variants(&bare).as_deref(), Some("one From throughout"));
        assert_eq!(from_variants(&[]), None);
    }

    #[test]
    fn timing_rows_fill_the_panels_and_not_the_drawer() {
        let f = Fixture::new();
        let mut l = looked(volumes(3));
        assert!(!where_panel(&f.inputs(), &l, false).html.contains("First and last article"), "no rows, no facts");
        // the row has 2,350 articles; the timing covers 2,000 of them
        l.timing = vec![timed(TEN, TEN + 3600, 1000, (500, 1499)), timed(TEN + 3700, TEN + 7200, 1000, (1500, 2499))];
        let page = panels_html(&f.inputs(), &l);
        for fact in [
            "<dt>First and last article</dt><dd>2026-10-02 10:00 to 2026-10-02 12:00 by the poster's clock, 2 h</dd>",
            "<dt>Timing covers</dt><dd>2,000 of 2,350 articles; the rest were saved before Atlas kept timing</dd>",
            "<dt>Posting rate</dt><dd>16.7 articles a minute</dd>",
            "<dt>File order</dt><dd>2 files one after another</dd>",
            "<dt>Sessions</dt><dd>one session</dd>",
            "<dt>Article numbers</dt><dd>500–2,499 where it was read, adjacent</dd>",
            "<dt>Threaded replies</dt><dd>none</dd>",
            "<dt>From variants</dt><dd>one From throughout</dd>",
            "<dt>Lines per article</dt><dd>100 lines an article</dd>",
            "<dt>Late articles</dt><dd>none</dd>",
        ] {
            assert!(page.contains(fact), "{fact}\n{page}");
        }
        assert!(!page.contains("Crossposts per article") && !page.contains("Encoded subjects"), "{page}");
        assert!(!page.contains("Lines key"), "part of the release timed: no Lines key\n{page}");
        // the whole release timed: the rates carry bytes too
        l.timing[1].articles = 1350;
        l.timing[1].encoded_subject = true;
        l.timing[0].xref_groups_max = Some(2);
        let page = panels_html(&f.inputs(), &l);
        for fact in [
            "<dt>Posting rate</dt><dd>19.6 articles a minute, 334.2 KB/s</dd>",
            "<dt>Lines per article</dt><dd>85 lines an article, 12,320 bytes a line</dd>",
            "<dt>Crossposts per article</dt><dd>2 groups at most, by Xref</dd>",
            "<dt>Encoded subjects</dt><dd>1 of 2 files posted with RFC 2047 words</dd>",
            "<dt>Lines key</dt>",
        ] {
            assert!(page.contains(fact), "{fact}\n{page}");
        }
        assert!(!page.contains("Timing covers"), "{page}");
        let drawer = compact_html(&f.inputs(), &l);
        assert!(!drawer.contains("First and last article") && !drawer.contains("Lines key"), "{drawer}");
    }

    #[test]
    fn drawer_markup_spans_the_table() {
        assert_eq!(
            drawer_shell(15, "<p>x</p>".into(), false),
            format!(
                "<tr class=\"drawer-row\"><td colspan=\"{COLUMNS}\"><p>x</p><p class=\"dim\"><a href=\"/ui/release?id=15\">Full page</a></p></td></tr>"
            )
        );
        assert!(drawer_shell(15, "<p>x</p>".into(), true).starts_with("<div class=\"drawer-card\"><p>x</p>"));
        let trigger = drawer_trigger(7);
        assert!(
            trigger.contains("class=\"icon-button drawer-toggle\"")
                && trigger.contains("hx-get=\"/ui/row?id=7\"")
                && trigger.contains("hx-target=\"closest tr\"")
                && trigger.contains("hx-swap=\"afterend\""),
            "{trigger}"
        );
        assert!(
            trigger.contains("aria-label=\"More facts\"") && trigger.contains("aria-expanded=\"false\""),
            "{trigger}"
        );
        let card = card_trigger(7, "closest li");
        assert!(
            card.contains("class=\"icon-button drawer-toggle\"")
                && card.contains("hx-get=\"/ui/row?id=7&amp;as=card\"")
                && card.contains("hx-target=\"closest li\"")
                && card.contains("hx-swap=\"beforeend\""),
            "{card}"
        );
        let f = Fixture::new();
        let compact = compact_html(&f.inputs(), &looked(volumes(3)));
        assert!(compact.starts_with("<dl class=\"facts facts-wide\">") && compact.ends_with("</dl>"), "{compact}");
        for label in [
            "Posted",
            "From shape",
            "Injected at",
            "Tool",
            "Names",
            "Obfuscation",
            "Contents",
            "Packing",
            "PAR2",
            "First-id key",
            "Account key",
        ] {
            assert!(compact.contains(&format!("<dt>{label}</dt>")), "{label} missing in {compact}");
        }
        for label in ["Subject", "Date style", "Host key", "NZBLNK", "Volumes", "Injection-Info"] {
            assert!(!compact.contains(&format!("<dt>{label}</dt>")), "{label} is the full page's: {compact}");
        }
        assert!(compact.contains("<dt>Posted</dt><dd>2026-10-03</dd>"), "{compact}");
        assert!(compact.contains("<dt>Packing</dt><dd>RAR5, m0</dd>"), "{compact}");
        assert!(!compact.contains(ACCOUNT) && !compact.contains(HOST), "{compact}");
    }

    #[test]
    fn not_ours_labels_never_appear() {
        // what the release page shows already: the probe's facts and the summary's
        const THEIRS: &[&str] = &[
            "Articles",
            "Audio",
            "Category",
            "Edition",
            "Email",
            "Encryption",
            "Files",
            "Group",
            "Groups",
            "Languages",
            "Links",
            "Name from",
            "PAR2 files",
            "Parts",
            "Password",
            "Posted as",
            "Posted",
            "Poster",
            "Release group",
            "Repair",
            "Ripped from",
            "Site",
            "Size",
            "Title",
            "Year",
            "Article size",
            "CRC32",
            "Checks",
            "Container",
            "Crossposted to",
            "File CRC32",
            "File name",
            "First file",
            "Format",
            "Layers",
            "Line length",
            "Method",
            "Muxed by",
            "On the server",
            "OpenSubtitles hash",
            "OpenSubtitles match",
            "Overhead",
            "Posted with",
            "Subtitles",
            "Video",
            "Written by",
        ];
        let html = full();
        let labels: Vec<&str> = html.split("<dt>").skip(1).filter_map(|rest| rest.split("</dt>").next()).collect();
        assert!(labels.len() >= 30, "{labels:?}");
        let mut seen = BTreeSet::new();
        for label in &labels {
            assert!(!THEIRS.contains(label), "{label} is already on the release page");
            assert!(seen.insert(*label), "{label} appears twice in {html}");
        }
    }

    #[test]
    fn fragments_stop_at_the_article_budget() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        crate::db::create_db_at(&main).unwrap();
        let poster = "poster <p@example.com>";
        let date = "2026-10-03 17:41:49";
        let articles: Vec<crate::parser::Article> = (1..=3)
            .map(|part| {
                let subject = format!("[01/52] - \"{VOLUME1}\" yEnc ({part}/3)");
                let parsed = crate::parser::parse_subject(&subject).unwrap();
                crate::parser::Article {
                    subject,
                    author: poster.into(),
                    date: date.into(),
                    message_id: format!("<part{part}@nyuu>"),
                    bytes: 700_000 + part,
                    filename: Some(parsed.filename.clone()),
                    release_name: Some(parsed.release_name.clone()),
                    part: Some(part),
                    total_parts: Some(3),
                    file_index: Some(1),
                    file_total: Some(52),
                    ..Default::default()
                }
            })
            .collect();
        let parsed = crate::parser::parse_subject(&articles[0].subject).unwrap();
        let collection = crate::collections::Collections::new(GROUP).of(&articles[0], &parsed);
        let release = crate::parser::Release {
            name: parsed.release_name.clone(),
            articles,
            size: 2_100_006,
            group: GROUP.into(),
            poster: poster.into(),
            date: date.into(),
            collection,
            ..Default::default()
        };
        store::save(&main, &[release]).unwrap();
        let conn = crate::db::open_with_shards(&main).unwrap();
        let shard = crate::db::open_shard(&store::shard_path(&main, store::shard_of(GROUP))).unwrap();
        let id: i64 = shard.query_row("select id from releases", [], |r| r.get(0)).unwrap();
        let files = store::release_files(&shard, id, FILES_CAP).unwrap();
        assert_eq!(files.len(), 1);
        // within the budget: the three parts are mapped and the ids read
        let table = parts_fragment_of(&conn, id, &files, ARTICLE_BUDGET);
        assert!(
            table.contains("title=\"3 of 3 parts\"")
                && table.contains("<td>&lt;part1@nyuu&gt;</td><td>&lt;part3@nyuu&gt;</td>")
                && !table.contains("Too many articles"),
            "{table}"
        );
        let hashes = hashes_fragment_of(&conn, id, ARTICLE_BUDGET);
        assert!(hashes.contains("over 3 articles"), "{hashes}");
        // past it: the pass stops, the maps stay blank and both fragments say so
        let table = parts_fragment_of(&conn, id, &files, 2);
        assert!(table.contains("Too many articles to list") && !table.contains("part1@nyuu"), "{table}");
        let hashes = hashes_fragment_of(&conn, id, 2);
        assert!(hashes.contains("too many articles to list") && !hashes.contains("over 3 articles"), "{hashes}");
    }

    #[test]
    fn fragments_are_bounded() {
        let many: Vec<FileInfo> = (0..300).map(|n| file(&format!("f{n:03}.bin"), 10)).collect();
        let table = parts_table(&many, &HashMap::new(), true);
        assert_eq!(table.matches("<tr><td class=\"name\">").count(), PARTS_ROWS, "{table}");
        assert!(table.contains("The first 200 of 300 files.") && !table.contains("Too many articles"), "{table}");
        assert!(table.starts_with("<div class=\"table-wrap\"><table><thead><tr><th>Subject</th>"), "{table}");
        let capped: Vec<FileInfo> = (0..FILES_CAP).map(|n| file(&format!("f{n}.bin"), 1)).collect();
        let table = parts_table(&capped, &HashMap::new(), false);
        assert!(table.contains("The first 200 of 5,000 or more files.") && table.contains("Too many articles to list"));
        // the parts a pass over the articles found, mapped into 64 cells
        let mut parts = FileParts::default();
        for p in 1..=5 {
            parts.offer(Some(p), format!("<{p}@x>"), Some(700_000 + p));
        }
        parts.offer(Some(3), "<3b@x>".into(), Some(700_003));
        let by_file = HashMap::from([("f000.bin".to_string(), parts)]);
        let table = parts_table(&many[..1], &by_file, true);
        assert!(
            table.contains("title=\"10 of 10 parts\"") && table.contains("<td>&lt;1@x&gt;</td><td>&lt;5@x&gt;</td>"),
            "{table}"
        );
        assert!(
            table.contains("<td class=\"num\">700,001</td><td class=\"num\">700,005</td><td class=\"num\">1</td>"),
            "{table}"
        );
        assert_eq!(part_map(&BTreeSet::new(), 0).chars().count(), 64);
        assert!(part_map(&BTreeSet::new(), 10).chars().all(|c| c == '▯'));
        assert!(part_map(&(1..=10).collect(), 10).chars().all(|c| c == '▮'));
        let half = part_map(&(1..=5).collect(), 10);
        assert!(half.chars().count() == 64 && half.contains('▮') && half.contains('▯'), "{half}");
    }
}
