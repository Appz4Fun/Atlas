//! Other postings of the same content. An **xpost** is the same upload seen in another group: the very same
//! articles, by message-id. A **twin** is another upload of the same posted files, proven by PAR2 hashes, soo its
//! articles can stand in for this one's. An **alt** holds the same files packed differently, soo it's another whole
//! download, not a source of articles.
//!
//! Each release keeps two fingerprints in its shard, and copies are found by looking them up in every
//! shard, soo there are no clusters to keep in step: purging a release takes its row with it.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use rusqlite::{Connection, OptionalExtension};
use sha1::{Digest, Sha1};

use crate::db;
use crate::par2::Par2File;
use crate::postprocess::schema::ShardTable;
use crate::search::{ArticleRow, ReleaseRow, get_release_with};
use crate::store;

pub const FINGERPRINT: ShardTable = ShardTable {
    name: "release_fingerprint",
    create: "create table if not exists release_fingerprint (
        release_id INTEGER PRIMARY KEY,
        posted     BLOB,
        content    BLOB
    )",
    indexes: &[
        (
            "release_fingerprint_posted",
            "create index if not exists release_fingerprint_posted on release_fingerprint(posted) where posted is not null",
        ),
        (
            "release_fingerprint_content",
            "create index if not exists release_fingerprint_content on release_fingerprint(content) where content is not null",
        ),
    ],
    release_key: Some("release_id"),
    added: &[("par2_block", "INTEGER"), ("par2_blocks", "INTEGER"), ("posted_names", "BLOB")],
};

pub const SET_POSTED: &str = "insert into release_fingerprint (release_id, posted) values (?1, ?2)
    on conflict(release_id) do update set posted = excluded.posted";
pub const SET_PAR2_SIZES: &str =
    "insert into release_fingerprint (release_id, par2_block, par2_blocks) values (?1, ?2, ?3)
    on conflict(release_id) do update set par2_block = excluded.par2_block, par2_blocks = excluded.par2_blocks";
pub const SET_CONTENT: &str = "insert into release_fingerprint (release_id, content) values (?1, ?2)
    on conflict(release_id) do update set content = excluded.content";

/// A release that no longer proves what it did, as when more of it has arrived. Its old answer goes.
pub const CLEAR_POSTED: &str = "update release_fingerprint set posted = null, par2_block = null,
    par2_blocks = null where release_id = ?1";
pub const CLEAR_CONTENT: &str = "update release_fingerprint set content = null where release_id = ?1";

/// Files smaller than this don't count: NFOs, samples' subtitles, and the like differ between postings.
const MIN_CONTENT_FILE: u64 = 1 << 20;
const MAX_COPIES: i64 = 50;

/// Everything a posting is, from a complete PAR2 set and the files posted with it: the set itself and its block size,
/// each file's name, size and MD5, and every posted file's name with its article count, recovery volumes included.
/// Two equal ones are the same posting in all but message-ids, which is what makes a **twin**: the same bytes under the
/// same names, cut up the same way, with the same repair. Part counts and sizes equal means articles are too, and sizes
/// on the wire can't say more, as they also count headers that differ between posters.
pub fn twin_fingerprint(files: &[Par2File], block_size: Option<u64>, posted: &[(&str, i64)]) -> Option<Vec<u8>> {
    let first = files.first()?;
    let mut named: Vec<(&str, u64, [u8; 16])> = files.iter().map(|f| (f.name.as_str(), f.size, f.md5)).collect();
    named.sort_unstable();
    named.dedup();
    let mut posted = posted.to_vec();
    posted.sort_unstable();
    let mut sha = Sha1::new();
    sha.update(first.set_id);
    sha.update(block_size.unwrap_or(0).to_le_bytes());
    sha.update((posted.len() as u64).to_le_bytes());
    for (name, parts) in posted {
        sha.update((name.len() as u64).to_le_bytes());
        sha.update(name.as_bytes());
        sha.update(parts.to_le_bytes());
    }
    for (name, size, md5) in named {
        sha.update((name.len() as u64).to_le_bytes());
        sha.update(name.as_bytes());
        sha.update(size.to_le_bytes());
        sha.update(md5);
    }
    let mut out = vec![TWIN_VERSION];
    out.extend(sha.finalize());
    Some(out)
}

/// Marks a fingerprint as this kind. Ones from before a twin meant all of this are as long as the hash alone.
const TWIN_VERSION: u8 = 2;

/// Those older ones proved less, soo they go when indexing starts: nothing may lend articles on their word. Their
/// releases are read again for the new kind, including any whose old fingerprint went before this queued them: they
/// have a PAR2 set's sizes and no fingerprint, which nothing else leaves behind.
pub fn forget_old_twins(conn: &Connection) {
    let bit = crate::postprocess::step::mask(crate::archive::steps::PAR2_STEP);
    let now = crate::media::http::now_secs();
    for s in 0..store::SHARDS {
        let old = "(posted is not null and length(posted) != 21) or (posted is null and par2_block is not null)";
        let sql = format!(
            "begin immediate;
             insert into s{s}.pp_queue (release_id, due_at)
                 select release_id, {now} from s{s}.release_fingerprint where {old}
                 on conflict(release_id) do update set due_at = excluded.due_at;
             update s{s}.pp_state set done = done & ~{bit}, failed = failed & ~{bit}
                 where release_id in (select release_id from s{s}.release_fingerprint where {old});
             update s{s}.release_fingerprint set posted = null, par2_block = null, par2_blocks = null where {old};
             commit;"
        );
        // a shard that hasn't got the tables yet has none
        if conn.execute_batch(&sql).is_err() {
            let _ = conn.execute_batch("rollback");
        }
    }
}

/// Whether one archive's listing says what a whole release holds: everything real in it is that archive's volumes.
/// Names that differ count as different archives, soo an obfuscated set makes no fingerprint, which is the safe way
/// to be wrong: a release with another archive beside the one read must not match one without it.
pub fn one_archive_set<'a>(mut names: impl Iterator<Item = &'a str>) -> bool {
    use crate::archive::select::{Kind, classify};
    let mut set = None;
    names.all(|name| {
        let family = match classify(name) {
            Kind::Par2Base | Kind::Par2Vol(_) | Kind::Nfo | Kind::Nzb | Kind::Image => return true,
            Kind::Rar(_) => "rar",
            Kind::SevenZip(_) => "7z",
            Kind::Zip(_) => "zip",
            Kind::Numbered(_) => "numbered",
            Kind::NotArchive | Kind::Unidentified => {
                let lower = name.to_ascii_lowercase();
                return is_sample(&lower) || lower.ends_with(".sfv");
            }
        };
        let this = (family, volume_set(name));
        *set.get_or_insert(this) == this
    }) && set.is_some()
}

/// A volume's name without what makes it that volume: `a.part01.rar`, `a.r00`, `a.z01`, `a.7z.001` and `a.001` are `a`.
fn volume_set(name: &str) -> &str {
    static VOLUME_END: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"(?i)(\.part\d+\.rar|\.7z\.\d+|\.[^./\\]+)$").unwrap()
    });
    let name = crate::archive::names::base_name(name);
    VOLUME_END.find(name).filter(|m| m.start() > 0).map_or(name, |m| &name[..m.start()])
}

/// Whether any of these is an archive, soo a list of them isn't a list of what's inside.
pub fn lists_archives<'a>(mut names: impl Iterator<Item = &'a str>) -> bool {
    names.any(is_archive)
}

fn is_archive(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or_default();
    matches!(ext, "rar" | "zip" | "7z" | "par2" | "ace" | "arj" | "cab" | "lzh" | "tar" | "gz" | "bz2" | "xz")
        || (ext.len() == 3 && ext.starts_with('r') && ext[1..].bytes().all(|b| b.is_ascii_digit()))
        || (ext.len() == 3 && ext.bytes().all(|b| b.is_ascii_digit()))
}

fn is_sample(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("sample") || lower.contains("proof")
}

/// What's in a release, from what's inside its archives, or its PAR2 set when it isn't packed: the exact
/// sizes of its real files. Names don't count, because obfuscation changes them. None: a file is packed,
/// soo these aren't the real files, a real file's size isn't known, or there's nothing big enough to go by.
pub fn content_fingerprint<'a>(files: impl Iterator<Item = (&'a str, Option<u64>)>) -> Option<Vec<u8>> {
    let mut sizes = Vec::new();
    for (name, size) in files {
        if is_archive(name) {
            return None;
        }
        if is_sample(name) {
            continue;
        }
        // leaving it out would make this the same as a release without that file
        let size = size?;
        if size >= MIN_CONTENT_FILE {
            sizes.push(size);
        }
    }
    if sizes.is_empty() {
        return None;
    }
    sizes.sort_unstable();
    let mut sha = Sha1::new();
    for size in sizes {
        sha.update(size.to_le_bytes());
    }
    Some(sha.finalize().to_vec())
}

/// What another posting is to a release, in the order a page lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Copy {
    /// the same upload in another group: the same articles under the same message-ids
    Xpost,
    /// another upload of the same files, cut up the same way: other message-ids, the same bytes
    Twin,
    Alt,
}

/// Whether two releases hold the same articles: a file of one starts with the very message-id a file of the other
/// does. Message-ids are unique, soo that's one upload crossposted to both groups, not two uploads of the same bytes.
/// A file's first message-id is that of its lowest part seen, soo a copy missing a file's first part can still tie
/// by another file. False when either is gone or a shard hasn't got the table.
pub fn shares_articles(conn: &Connection, a: i64, b: i64) -> bool {
    let (sa, sb) = (store::shard_of_id(a), store::shard_of_id(b));
    let sql = format!(
        "select 1 from s{sb}.files where release_id = ?2 and subject_mid in
             (select subject_mid from s{sa}.files where release_id = ?1 and subject_mid is not null)
         limit 1"
    );
    conn.prepare_cached(&sql).and_then(|mut stmt| stmt.exists(rusqlite::params![a, b])).unwrap_or(false)
}

/// How many other releases with this posted fingerprint hold `id`'s very articles, over every shard and uncapped,
/// soo it can be taken from a tally of the fingerprint.
fn count_xposts(conn: &Connection, id: i64, posted: &[u8]) -> i64 {
    let own = store::shard_of_id(id);
    (0..store::SHARDS)
        .filter_map(|s| {
            let sql = format!(
                "select count(*) from s{s}.release_fingerprint f where f.posted = ?1 and f.release_id != ?2
                 and exists (select 1 from s{s}.files x where x.release_id = f.release_id and x.subject_mid in
                     (select subject_mid from s{own}.files where release_id = ?2 and subject_mid is not null))"
            );
            // a shard that hasn't got the tables yet has none
            conn.query_row(&sql, rusqlite::params![posted, id], |r| r.get::<_, i64>(0)).ok()
        })
        .sum()
}

/// Of these copies of `id` proven to hold the same files, an xpost when it shares articles with `id`, else a twin.
fn same_files_kind(conn: &Connection, id: i64, other: i64) -> Copy {
    if shares_articles(conn, id, other) { Copy::Xpost } else { Copy::Twin }
}

fn matching(conn: &Connection, column: &str, fingerprint: &[u8], id: i64) -> db::Result<Vec<i64>> {
    let mut found = Vec::new();
    for s in 0..store::SHARDS {
        let sql = format!(
            "select release_id from s{s}.release_fingerprint where {column} = ?1 and release_id != ?2 limit {MAX_COPIES}"
        );
        match conn.prepare_cached(&sql) {
            Ok(mut stmt) => {
                let ids = stmt.query_map(rusqlite::params![fingerprint, id], |r| r.get::<_, i64>(0))?;
                found.extend(ids.collect::<db::Result<Vec<_>>>()?);
            }
            // a shard that hasn't got the table yet has no copies
            Err(_) => continue,
        }
    }
    Ok(found)
}

/// A release's (posted, content) fingerprints, either one when a read has kept it.
type FingerprintPair = (Option<Vec<u8>>, Option<Vec<u8>>);

/// A release's posted and content fingerprints, when a read has kept them. A release with no fingerprint row can
/// still have file twins by its set.
fn fingerprints_with(conn: &Connection, id: i64) -> db::Result<FingerprintPair> {
    let s = store::shard_of_id(id);
    let sql = format!("select posted, content from s{s}.release_fingerprint where release_id = ?");
    Ok(match conn.prepare(&sql) {
        Ok(mut stmt) => stmt
            .query_row([id], |r| Ok((r.get::<_, Option<Vec<u8>>>(0)?, r.get::<_, Option<Vec<u8>>>(1)?)))
            .optional()?
            .unwrap_or((None, None)),
        Err(_) => (None, None),
    })
}

/// The other postings with this release's posted fingerprint: the same files, cut up the same way, in every shard.
/// Those lend articles file by file under the posted names; a file twin lends only the files its set vouches for.
pub fn twins_with(conn: &Connection, id: i64) -> db::Result<Vec<i64>> {
    match fingerprints_with(conn, id)?.0 {
        Some(fingerprint) => matching(conn, "posted", &fingerprint, id),
        None => Ok(Vec::new()),
    }
}

/// The other postings of a release's content: xposts, then twins, then alts.
pub fn copies_with(conn: &Connection, id: i64) -> db::Result<Vec<(Copy, ReleaseRow)>> {
    let (_, content) = fingerprints_with(conn, id)?;
    let twins = twins_with(conn, id)?;
    let alts = match &content {
        Some(fingerprint) => matching(conn, "content", fingerprint, id)?,
        None => Vec::new(),
    };
    let mut out = Vec::new();
    for twin in &twins {
        if let Some(release) = get_release_with(conn, *twin)? {
            out.push((same_files_kind(conn, id, *twin), release));
        }
    }
    for file_twin in file_twins_with(conn, id) {
        if !twins.contains(&file_twin.id)
            && let Some(release) = get_release_with(conn, file_twin.id)?
        {
            out.push((same_files_kind(conn, id, file_twin.id), release));
        }
    }
    // the same upload first: it's this release, seen in another group
    out.sort_by_key(|(kind, _)| *kind);
    let listed: HashSet<i64> = out.iter().map(|(_, r)| r.id).collect();
    for alt in alts.iter().filter(|alt| !listed.contains(alt)) {
        if let Some(release) = get_release_with(conn, *alt)? {
            out.push((Copy::Alt, release));
        }
    }
    Ok(out)
}

pub fn copies(id: i64) -> db::Result<Vec<(Copy, ReleaseRow)>> {
    copies_with(&db::open()?, id)
}

/// Which copies a list of clusters is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClusterKind {
    Twins,
    Alts,
}

impl ClusterKind {
    pub fn column(self) -> &'static str {
        match self {
            ClusterKind::Twins => "posted",
            ClusterKind::Alts => "content",
        }
    }
}

/// Every fingerprint in every shard, as `release_id, posted, content`. Empty: no shard has the table yet.
fn all_fingerprints(conn: &Connection) -> String {
    (0..store::SHARDS)
        .filter(|s| conn.prepare(&format!("select 1 from s{s}.release_fingerprint limit 0")).is_ok())
        // a row can outlive its release, and a blocked group's are never shown
        .map(|s| {
            format!(
                "select f.release_id, f.posted, f.content from s{s}.release_fingerprint f
                 join s{s}.releases r on r.id = f.release_id where not atlas_blocked(r.group_name)"
            )
        })
        .collect::<Vec<_>>()
        .join(" union all ")
}

/// Which fingerprints make a cluster: twins are equal posted files, and an alt has to differ in them.
fn cluster_sql(conn: &Connection, kind: ClusterKind) -> Option<String> {
    let all = all_fingerprints(conn);
    if all.is_empty() {
        return None;
    }
    let column = kind.column();
    let copies = match kind {
        ClusterKind::Twins => "count(*)",
        ClusterKind::Alts => "count(distinct coalesce(posted, release_id))",
    };
    Some(format!(
        "select {column} as fingerprint, count(*) as members from ({all}) where {column} is not null
         group by {column} having {copies} > 1"
    ))
}

/// How many clusters there are of this kind.
pub fn cluster_count_with(conn: &Connection, kind: ClusterKind) -> db::Result<i64> {
    let Some(sql) = cluster_sql(conn, kind) else { return Ok(0) };
    conn.query_row(&format!("select count(*) from ({sql})"), [], |r| r.get(0))
}

/// A page of clusters, biggest first, each as the releases in it. Every release of a cluster is listed, up to
/// `MAX_COPIES` a shard.
pub fn clusters_with(
    conn: &Connection,
    kind: ClusterKind,
    offset: i64,
    limit: i64,
) -> db::Result<Vec<Vec<ReleaseRow>>> {
    let Some(sql) = cluster_sql(conn, kind) else { return Ok(Vec::new()) };
    let mut stmt =
        conn.prepare(&format!("select fingerprint from ({sql}) order by members desc, fingerprint limit ? offset ?"))?;
    let prints = stmt.query_map([limit, offset], |r| r.get::<_, Vec<u8>>(0))?.collect::<db::Result<Vec<_>>>()?;
    let mut out = Vec::with_capacity(prints.len());
    for print in prints {
        let mut members = Vec::new();
        // no release has id 0, soo nothing is left out
        for id in matching(conn, kind.column(), &print, 0)? {
            members.extend(get_release_with(conn, id)?);
        }
        if members.len() > 1 {
            out.push(members);
        }
    }
    Ok(out)
}

/// How many other copies a release has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CopyCount {
    /// the same upload in other groups
    pub xposts: i64,
    /// other uploads of the same files
    pub twins: i64,
    pub alts: i64,
}

type Fingerprints = HashMap<i64, (Option<Vec<u8>>, Option<Vec<u8>>)>;

fn fingerprints_of(conn: &Connection, ids: &[i64]) -> db::Result<Fingerprints> {
    let mut out = HashMap::with_capacity(ids.len());
    for s in 0..store::SHARDS {
        let list: Vec<String> = ids.iter().filter(|id| store::shard_of_id(**id) == s).map(i64::to_string).collect();
        if list.is_empty() {
            continue;
        }
        let sql = format!(
            "select release_id, posted, content from s{s}.release_fingerprint where release_id in ({})",
            list.join(",")
        );
        // a shard without the table has no fingerprints
        let Ok(mut stmt) = conn.prepare(&sql) else { continue };
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, (r.get(1)?, r.get(2)?))))?;
        for row in rows {
            let (id, prints) = row?;
            out.insert(id, prints);
        }
    }
    Ok(out)
}

/// How often each fingerprint appears, over every shard: one query per shard and column.
fn tally(conn: &Connection, column: &str, prints: &[&Vec<u8>]) -> db::Result<HashMap<Vec<u8>, i64>> {
    let mut out: HashMap<Vec<u8>, i64> = HashMap::new();
    if prints.is_empty() {
        return Ok(out);
    }
    let marks = vec!["?"; prints.len()].join(",");
    for s in 0..store::SHARDS {
        let sql = format!(
            "select {column}, count(*) from s{s}.release_fingerprint where {column} in ({marks}) group by {column}"
        );
        let Ok(mut stmt) = conn.prepare(&sql) else { continue };
        let rows = stmt.query_map(rusqlite::params_from_iter(prints.iter()), |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (print, count) = row?;
            *out.entry(print).or_default() += count;
        }
    }
    Ok(out)
}

type PrintCounts = HashMap<(Vec<u8>, Vec<u8>), i64>;

/// How many releases have each posted and content fingerprint together, for these content fingerprints.
fn tally_both(conn: &Connection, prints: &[&Vec<u8>]) -> db::Result<PrintCounts> {
    let mut out = PrintCounts::new();
    if prints.is_empty() {
        return Ok(out);
    }
    let marks = vec!["?"; prints.len()].join(",");
    for s in 0..store::SHARDS {
        let sql = format!(
            "select posted, content, count(*) from s{s}.release_fingerprint
             where content in ({marks}) and posted is not null group by posted, content"
        );
        let Ok(mut stmt) = conn.prepare(&sql) else { continue };
        let rows = stmt.query_map(rusqlite::params_from_iter(prints.iter()), |r| {
            Ok(((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?), r.get::<_, i64>(2)?))
        })?;
        for row in rows {
            let (key, count) = row?;
            *out.entry(key).or_default() += count;
        }
    }
    Ok(out)
}

/// Copies per release, for a page of results.
pub fn counts_with(conn: &Connection, ids: &[i64]) -> db::Result<HashMap<i64, CopyCount>> {
    let prints = fingerprints_of(conn, ids)?;
    let posted: Vec<&Vec<u8>> = prints.values().filter_map(|(p, _)| p.as_ref()).collect();
    let content: Vec<&Vec<u8>> = prints.values().filter_map(|(_, c)| c.as_ref()).collect();
    let both = tally_both(conn, &content)?;
    let (posted, content) = (tally(conn, "posted", &posted)?, tally(conn, "content", &content)?);
    let mut out = HashMap::new();
    for (id, (p, c)) in &prints {
        let twins = p.as_ref().and_then(|p| posted.get(p)).map_or(0, |n| (n - 1).max(0));
        // a twin with the same content is a twin, not an alt as well
        let same_posting = match (p, c) {
            (Some(p), Some(c)) => both.get(&(p.clone(), c.clone())).map_or(0, |n| (n - 1).max(0)),
            _ => 0,
        };
        let alts = c.as_ref().and_then(|c| content.get(c)).map_or(0, |n| (n - 1 - same_posting).max(0));
        // of those with the same posted files, the ones holding this release's very articles are it, crossposted
        let xposts = match p {
            Some(p) if twins > 0 => count_xposts(conn, *id, p),
            _ => 0,
        };
        let twins = (twins - xposts).max(0);
        if xposts > 0 || twins > 0 || alts > 0 {
            out.insert(*id, CopyCount { xposts, twins, alts });
        }
    }
    Ok(out)
}

pub fn counts(ids: &[i64]) -> db::Result<HashMap<i64, CopyCount>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    counts_with(&db::open()?, ids)
}

/// Of these releases, in order, those that repeat the content of one before them.
pub fn later_copies(ids: &[i64]) -> db::Result<Vec<i64>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let prints = fingerprints_of(&db::open()?, ids)?;
    let mut seen: HashSet<&Vec<u8>> = HashSet::new();
    let mut later = Vec::new();
    for id in ids {
        let Some((posted, content)) = prints.get(id) else { continue };
        let mut repeat = false;
        for print in [posted, content].into_iter().flatten() {
            repeat |= !seen.insert(print);
        }
        if repeat {
            later.push(*id);
        }
    }
    Ok(later)
}

/// A PAR2 set's block size and how many data blocks it has, when a read has kept them.
pub fn par2_sizes(conn: &Connection, id: i64) -> Option<(i64, i64)> {
    let s = store::shard_of_id(id);
    let sql = format!("select par2_block, par2_blocks from s{s}.release_fingerprint where release_id = ?");
    conn.query_row(&sql, [id], |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?)))
        .ok()
        .and_then(|(block, blocks)| Some((block?, blocks?)))
}

/// A file another posting has too, as the two PAR2 sets describe it: the same size, first 16 KiB and whole MD5, soo
/// its articles hold the same bytes, cut up the same way when their counts and sizes agree, whatever the two posts
/// called it and whoever posted them. A **file twin** lends articles file by file where a twin lends them all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileTwin {
    pub id: i64,
    /// (what the other posting calls the file, what this one does), by the names their sets gave
    pub names: Vec<(String, String)>,
    /// both sets describe one data file: a posting of it under a random name is that file
    pub one_file: bool,
}

/// A name a set gives a file beside the data: its own volumes and the sidecars posted with it.
fn is_sidecar(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".par2", ".nfo", ".sfv", ".srr", ".srs", ".txt"].iter().any(|ext| lower.ends_with(ext))
}

/// A password found on one posting, shown on another that holds the same bytes: an xpost or twin of it. The same
/// archive bytes take the same password, soo it carries as it was found; whether it works was only ever tried on
/// the posting it came from.
pub struct Inherited {
    pub hint: crate::release_info::PasswordHint,
    /// the posting it was found on
    pub from: i64,
    pub from_name: String,
}

/// The most copies looked at for a password to carry over: the first ones with one of their own answer.
const MAX_PASSWORD_SOURCES: usize = 12;

/// The password a release's own names, NFO and files hint at, or else the name its PAR2 set gives its files when
/// they were posted under another (`par2_name_password`).
pub fn own_password_hint(
    conn: &Connection,
    id: i64,
    names: Option<&crate::release_info::Names>,
) -> Option<crate::release_info::PasswordHint> {
    crate::release_info::password_hint_with(conn, id, names).or_else(|| par2_name_password(conn, id))
}

/// Whether two postings are proven to hold the same whole payload, not just a file of it: the same posted fingerprint
/// (set, names, part counts and hashes), or sets that describe the very same data files, sidecars aside, by size and
/// MD5. A file twin by an NFO alone, or by one volume of two archives, isn't, soo nothing is carried between those.
pub fn same_payload(conn: &Connection, a: i64, b: i64) -> bool {
    if let (Ok((Some(pa), _)), Ok((Some(pb), _))) = (fingerprints_with(conn, a), fingerprints_with(conn, b))
        && pa == pb
    {
        return true;
    }
    let data = |id: i64| {
        let mut files: Vec<(u64, [u8; 16])> = crate::archive::store::par2_files_with(conn, id)
            .into_iter()
            .filter(|f| f.bound && !is_sidecar(&f.name))
            .map(|f| (f.size, f.md5))
            .collect();
        files.sort_unstable();
        files.dedup();
        files
    };
    let mine = data(a);
    !mine.is_empty() && mine == data(b)
}

/// The password `id`'s xposts and twins carry to it: of those holding its whole payload (`same_payload`), the first
/// with a password hint of its own.
pub fn carried_password(conn: &Connection, id: i64, copies: &[(Copy, ReleaseRow)]) -> Option<Inherited> {
    first_password(conn, carriers(conn, id, copies).iter().copied())
}

/// The copies of `id` a password carries to and from: xposts and twins holding its whole payload.
pub fn carriers<'a>(conn: &Connection, id: i64, copies: &'a [(Copy, ReleaseRow)]) -> Vec<&'a ReleaseRow> {
    copies
        .iter()
        .filter(|(kind, r)| matches!(kind, Copy::Xpost | Copy::Twin) && same_payload(conn, id, r.id))
        .map(|(_, r)| r)
        .collect()
}

/// Of these postings, every one holding the same bytes, the first with a password hint of its own.
pub fn first_password<'a>(conn: &Connection, rows: impl IntoIterator<Item = &'a ReleaseRow>) -> Option<Inherited> {
    rows.into_iter().take(MAX_PASSWORD_SOURCES).find_map(|r| {
        let names = crate::release_info::names_with(conn, r.id).ok().flatten();
        own_password_hint(conn, r.id, names.as_ref()).map(|hint| Inherited {
            hint,
            from: r.id,
            from_name: r.name.clone(),
        })
    })
}

/// The name a release's PAR2 set gives its archive, when the volumes were posted under another: a repost that
/// renamed the files kept the set made for the first upload, and an uploader that names an archive and its password
/// from one random string leaves the password there. Only a generated-looking name counts (letters and digits,
/// mixed case, 10 to 64 of them), and only one archive's volumes, every one renamed: a set of unpacked files has none.
pub fn par2_name_password(conn: &Connection, id: i64) -> Option<crate::release_info::PasswordHint> {
    let set = crate::archive::store::par2_files_with(conn, id);
    let data: Vec<&crate::par2::Par2File> = set.iter().filter(|f| f.bound && !is_sidecar(&f.name)).collect();
    // an unpacked file posted under another name has no password to give: only archive volumes are named for one
    if !data.iter().all(|f| is_archive(&f.name)) {
        return None;
    }
    let mut stems = data.iter().map(|f| volume_set(&f.name));
    let stem = stems.next()?;
    if !stems.all(|other| other == stem) || !looks_generated(stem) {
        return None;
    }
    let s = store::shard_of_id(id);
    let mut stmt = conn.prepare(&format!("select filename from s{s}.files where release_id = ?")).ok()?;
    let posted: Vec<String> = stmt.query_map([id], |r| r.get(0)).ok()?.filter_map(Result::ok).collect();
    // posted under the set's own name: nothing was renamed, soo the name says nothing more than it did
    if posted.is_empty() || posted.iter().any(|name| volume_set(name).eq_ignore_ascii_case(stem)) {
        return None;
    }
    Some(crate::release_info::PasswordHint { password: stem.to_string(), source: "the PAR2 set's file names" })
}

/// A name an uploader made up, not a title: letters and digits only, both cases.
fn looks_generated(stem: &str) -> bool {
    (10..=64).contains(&stem.len())
        && stem.bytes().all(|b| b.is_ascii_alphanumeric())
        && stem.bytes().any(|b| b.is_ascii_uppercase())
        && stem.bytes().any(|b| b.is_ascii_lowercase())
}

/// How many data files, sidecars aside, a release's set describes.
fn data_files_in_set(conn: &Connection, id: i64) -> usize {
    crate::archive::store::par2_files_with(conn, id).iter().filter(|f| f.bound && !is_sidecar(&f.name)).count()
}

/// The other postings that hold one or more of this release's files, by the sets' hashes, in every shard.
pub fn file_twins_with(conn: &Connection, id: i64) -> Vec<FileTwin> {
    let own = crate::archive::store::par2_files_with(conn, id);
    let mine_one = own.iter().filter(|f| f.bound && !is_sidecar(&f.name)).count() == 1;
    let mut by_release: HashMap<i64, Vec<(String, String)>> = HashMap::new();
    for file in own.iter().filter(|f| f.bound) {
        for (other, name) in crate::archive::store::same_par2_file_with(conn, file.size, &file.md5_16k, &file.md5, id) {
            by_release.entry(other).or_default().push((name, file.name.clone()));
        }
    }
    let mut out: Vec<FileTwin> = by_release
        .into_iter()
        .map(|(id, names)| FileTwin { id, names, one_file: mine_one && data_files_in_set(conn, id) == 1 })
        .collect();
    out.sort_by_key(|t| t.id);
    out.truncate(MAX_COPIES as usize);
    out
}

/// The posted name a set's name stands for among a release's articles: the same name, or the same but for case, or,
/// when `one_archive` (the set's names are one archive's volumes), the volume with the same ending under the one
/// archive name this posting used, or, when `one_file` (the set describes one data file), the one data file of a row
/// that posted it under a random name.
fn posted_name<'a>(articles: &'a [ArticleRow], name: &str, one_file: bool, one_archive: bool) -> Option<&'a str> {
    let names: Vec<&str> = {
        let mut seen: Vec<&str> = articles.iter().filter_map(|a| a.filename.as_deref()).collect();
        seen.sort_unstable();
        seen.dedup();
        seen
    };
    if let Some(found) = names.iter().copied().find(|n| *n == name) {
        return Some(found);
    }
    if let Some(found) = names.iter().copied().find(|n| n.eq_ignore_ascii_case(name)) {
        return Some(found);
    }
    if let Some(found) = renamed_volume(&names, name).filter(|_| one_archive) {
        return Some(found);
    }
    if !one_file || is_sidecar(name) {
        return None;
    }
    let data: Vec<&str> = names.iter().copied().filter(|n| !is_sidecar(n)).collect();
    match data[..] {
        [one] => Some(one),
        _ => None,
    }
}

/// The posted name of a volume a set names under another archive name: a repost that renamed every volume of one
/// archive (`YMFF.part001.rar` posted as `oCBv.part001.rar`) and kept the set made for the first upload. Only when
/// the posting's data files are all volumes of one archive, soo the set's name and the posted one stand for the same
/// thing, and only the volume with the very same ending.
fn renamed_volume<'a>(names: &[&'a str], name: &str) -> Option<&'a str> {
    if is_sidecar(name) {
        return None;
    }
    let base = crate::archive::names::base_name(name);
    let suffix = &base[volume_set(name).len()..];
    if !is_volume_ending(suffix) {
        return None;
    }
    let mut stems = names.iter().filter(|n| !is_sidecar(n)).map(|n| volume_set(n));
    let posted = stems.next()?;
    if !stems.all(|stem| stem == posted) {
        return None;
    }
    names
        .iter()
        .copied()
        .find(|n| !is_sidecar(n) && crate::archive::names::base_name(n)[posted.len()..].eq_ignore_ascii_case(suffix))
}

/// Whether what follows an archive's name is a volume's numbered ending: `.part001.rar`, `.r00`, `.z01`, `.7z.001` or
/// `.001`. A plain `.mkv` says nothing about which file it is.
fn is_volume_ending(suffix: &str) -> bool {
    static VOLUME: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"(?i)^\.(part\d+\.rar|r\d{2,}|z\d{2,}|7z\.\d+|\d{3,})$").unwrap()
    });
    VOLUME.is_match(suffix)
}

/// Whether a set's names, sidecars aside, are all volumes of one archive.
fn one_archive<'a>(names: impl Iterator<Item = &'a str>) -> bool {
    let mut stems = names.filter(|n| !is_sidecar(n)).map(volume_set);
    let Some(first) = stems.next() else { return false };
    stems.all(|stem| stem == first)
}

/// What articles were found to hold, by message-id: `(decoded length, CRC32)` from `segment_crc`, for the two
/// postings a fill is between. A part both have and both know must agree, or the two files are not the same bytes
/// however alike their sets and names.
pub type KnownCrcs = HashMap<String, (i64, i64)>;

/// Adds what `own` is missing from a file twin's articles: each of the twin's files the sets say is one of this
/// release's is read as that file, and fills its gaps by `fill_gaps`'s rules. Nothing else of the twin's is touched:
/// two postings that share an NFO can hold different archives under the same names. Returns how many.
pub fn fill_gaps_from(
    own: &mut Vec<ArticleRow>,
    twin: &[ArticleRow],
    names: &[(String, String)],
    one_file: bool,
    known: &KnownCrcs,
) -> usize {
    let mut renamed: Vec<ArticleRow> = Vec::new();
    // a renamed repost's volumes map by their endings only when each side's names are one archive's volumes
    let theirs_one = one_archive(names.iter().map(|(theirs, _)| theirs.as_str()));
    let mine_one = one_archive(names.iter().map(|(_, mine)| mine.as_str()));
    for (theirs, mine) in names {
        let (Some(from), Some(to)) =
            (posted_name(twin, theirs, one_file, theirs_one), posted_name(own, mine, one_file, mine_one))
        else {
            continue;
        };
        let to = to.to_string();
        renamed.extend(
            twin.iter()
                .filter(|a| a.filename.as_deref() == Some(from))
                .map(|a| ArticleRow { filename: Some(to.clone()), ..a.clone() }),
        );
    }
    if renamed.is_empty() {
        return 0;
    }
    fill_gaps_checked(own, &renamed, known)
}

/// Adds what `own` is missing from every twin and file twin of `release`, and returns how many articles that was.
/// A file twin lends only the files its set vouches for, under the names the sets map; a twin by the posted
/// fingerprint lends every file by its posted name. `conn` has the shards attached.
pub fn fill_from_copies_with(conn: &Connection, id: i64, own: &mut Vec<ArticleRow>) -> db::Result<usize> {
    fill_from_copies_of(conn, id, &[id], own)
}

/// `fill_from_copies_with` for articles gathered from several rows (`rows`: a post cut across groups, or a linked
/// upload), soo what each row's articles were read to hold is checked against the donors too.
pub fn fill_from_copies_of(conn: &Connection, id: i64, rows: &[i64], own: &mut Vec<ArticleRow>) -> db::Result<usize> {
    let mut added = 0;
    // what every posting seen soo far was read to hold: an article borrowed from one donor keeps its CRC, soo the
    // next donor's copy of that part is checked against it too
    let mut known = crate::archive::store::segment_crcs_with(conn, id);
    for row in rows.iter().filter(|row| **row != id) {
        known.extend(crate::archive::store::segment_crcs_with(conn, *row));
    }
    for twin in file_twins_with(conn, id) {
        known.extend(crate::archive::store::segment_crcs_with(conn, twin.id));
        added += fill_gaps_from(own, &store::articles(conn, twin.id)?, &twin.names, twin.one_file, &known);
    }
    let mut seen: HashSet<i64> = rows.iter().copied().collect();
    for twin in twins_with(conn, id)? {
        if seen.insert(twin) {
            known.extend(crate::archive::store::segment_crcs_with(conn, twin));
            added += fill_gaps_checked(own, &store::articles(conn, twin)?, &known);
        }
    }
    // a file of the same size and whole-file CRC32 in a posting no fingerprint or set hash ties to this one: a repost
    // under other names, or a set that hashes the same bytes differently. It lends by `fill_gaps`'s rules, soo only
    // when it was cut up the same way, and only to a file of this release that is missing parts.
    for mine in crate::keys::named_crc_files(conn, id) {
        let Some(posted) = posted_name(own, &mine.name, false, false).map(str::to_string) else { continue };
        if !has_gaps(own, &posted) {
            continue;
        }
        for (donor, theirs) in crate::keys::crc_donors(conn, id, mine.file, mine.md5.as_deref()) {
            if rows.contains(&donor) {
                continue;
            }
            known.extend(crate::archive::store::segment_crcs_with(conn, donor));
            let lent = fill_gaps_from(own, &store::articles(conn, donor)?, &[(theirs, posted.clone())], false, &known);
            if lent > 0 {
                crate::metrics::names::NZB_FILLED_BY_CRC.inc(&[], lent as u64);
            }
            added += lent;
            if !has_gaps(own, &posted) {
                break;
            }
        }
    }
    Ok(added)
}

/// Whether the file posted as `name` is missing parts among `articles`: fewer distinct parts than its subject counts.
fn has_gaps(articles: &[ArticleRow], name: &str) -> bool {
    let mut parts: HashSet<i64> = HashSet::new();
    let mut total = None;
    for a in articles.iter().filter(|a| a.filename.as_deref() == Some(name)) {
        parts.extend(a.part);
        total = total.or(a.total_parts);
    }
    total.is_some_and(|t| (parts.len() as i64) < t)
}

/// `fill_gaps_checked` with nothing known about the articles' bytes.
pub fn fill_gaps(own: &mut Vec<ArticleRow>, twin: &[ArticleRow]) -> usize {
    fill_gaps_checked(own, twin, &KnownCrcs::new())
}

/// Adds what `own` is missing from a twin's articles, file by file. A twin's file counts only under the
/// same name, with the same number of parts, the same size for every part both have, and the same decoded length and
/// CRC32 for every part both have that `known` has read: one that differs says the two files are not the same bytes,
/// and the twin lends nothing of that file. Returns how many.
pub fn fill_gaps_checked(own: &mut Vec<ArticleRow>, twin: &[ArticleRow], known: &KnownCrcs) -> usize {
    /// a file's parts this posting has: each part's wire size and message-id
    type Parts = HashMap<i64, (Option<i64>, String)>;
    let mut have: HashMap<Option<String>, Parts> = HashMap::new();
    let mut templates: HashMap<Option<String>, ArticleRow> = HashMap::new();
    for article in own.iter() {
        if let Some(part) = article.part {
            have.entry(article.filename.clone()).or_default().insert(part, (article.bytes, article.message_id.clone()));
        }
        templates.entry(article.filename.clone()).or_insert_with(|| article.clone());
    }
    let mut by_file: HashMap<&Option<String>, Vec<&ArticleRow>> = HashMap::new();
    for article in twin {
        by_file.entry(&article.filename).or_default().push(article);
    }
    let mut added = 0;
    for (filename, articles) in by_file {
        let (Some(parts), Some(template)) = (have.get(filename), templates.get(filename)) else { continue };
        let same_total = articles.iter().all(|a| a.total_parts.is_some() && a.total_parts == template.total_parts);
        let same_sizes = articles
            .iter()
            .all(|a| a.part.and_then(|part| parts.get(&part)).is_none_or(|(bytes, _)| *bytes == a.bytes));
        // a part both have, both read: the same bytes or not the same file
        let same_bytes = articles.iter().all(|a| {
            let (Some(theirs), Some((_, mine))) = (known.get(&a.message_id), a.part.and_then(|part| parts.get(&part)))
            else {
                return true;
            };
            known.get(mine).is_none_or(|mine| mine == theirs)
        });
        // with no part in common, nothing says the two were cut up the same way
        let shared = articles.iter().any(|a| a.part.is_some_and(|part| parts.contains_key(&part)));
        if !same_total || !same_sizes || !same_bytes || !shared {
            continue;
        }
        for article in articles {
            if article.part.is_some_and(|part| !parts.contains_key(&part)) {
                own.push(ArticleRow {
                    message_id: article.message_id.clone(),
                    part: article.part,
                    bytes: article.bytes,
                    ..template.clone()
                });
                added += 1;
            }
        }
    }
    if added > 0 {
        own.sort_by(|a, b| (&a.filename, a.part, &a.message_id).cmp(&(&b.filename, b.part, &b.message_id)));
    }
    added
}

/// Whether a twin of this release can lend it articles.
pub fn can_fill(copies: &[(Copy, ReleaseRow)]) -> bool {
    // an xpost's row can have parts this group's listing missed: the same articles, read in another group
    copies.iter().any(|(kind, _)| matches!(kind, Copy::Twin | Copy::Xpost))
}

/// For a cluster of postings with the same files, in order, which are an xpost of one listed before them: the same
/// upload seen in another group. The rest are each a separate upload, twins of one another.
pub fn xposts_of_earlier(conn: &Connection, members: &[ReleaseRow]) -> Vec<bool> {
    // every member before it, not one of each upload: two rows can tie only through a third, as each file's first
    // message-id is the lowest part that row saw, and crossposts can miss different parts in different groups
    (0..members.len())
        .map(|i| {
            members[i.saturating_sub(MAX_XPOST_WITNESSES)..i]
                .iter()
                .any(|earlier| shares_articles(conn, earlier.id, members[i].id))
        })
        .collect()
}

/// How many members before it a cluster member is checked against: a cluster is rarely more than a few uploads.
const MAX_XPOST_WITNESSES: usize = 64;

/// A release's NZB, its gaps filled from its twins: what every NZB is now, kept for the page's own link to it.
pub fn filled_nzb(id: i64) -> db::Result<Option<String>> {
    crate::nzb::build_nzb(id).map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1 << 30;

    fn content(files: &[(&str, u64)]) -> Option<Vec<u8>> {
        content_fingerprint(files.iter().map(|(name, size)| (*name, Some(*size))))
    }

    fn par2(size: u64, md5: u8) -> Par2File {
        Par2File {
            set_id: [0; 16],
            file_id: [0; 16],
            md5: [md5; 16],
            md5_16k: [0; 16],
            size,
            name: "x".into(),
            bound: true,
            crc32: None,
        }
    }

    #[test]
    fn only_a_made_up_name_is_a_password_hint() {
        assert!(looks_generated("YMFFZeoeVI6fG4iz"));
        assert!(looks_generated("oCBvoIlhFvS0L3k1"));
        assert!(!looks_generated("Gone.with.the.Wind.1939"), "a title");
        assert!(!looks_generated("abcdefghijklmnop"), "one case");
        assert!(!looks_generated("Short1"), "too short");
    }

    #[test]
    fn a_list_of_volumes_is_a_list_of_archives() {
        assert!(lists_archives(["a.part01.rar", "a.nfo"].into_iter()));
        assert!(!lists_archives(["a.mkv", "a.nfo"].into_iter()));
    }

    #[test]
    fn only_one_archive_speaks_for_a_release() {
        let one = |names: &[&str]| one_archive_set(names.iter().copied());
        assert!(one(&["a.part01.rar", "a.part02.rar", "a.par2", "a.vol0+1.par2", "a.nfo", "a-sample.mkv"]));
        assert!(one(&["a.rar", "a.r00", "a.r01"]));
        assert!(one(&["a.7z.001", "a.7z.002"]));
        assert!(one(&["a.z01", "a.z02", "a.zip"]), "a split ZIP");
        assert!(one(&["a.rar", "a.s00"]));
        assert!(one(&["a.cbz"]));
        assert!(!one(&["a.rar", "a.zip"]), "same name, different archives");
        assert!(!one(&["a.rar", "b.rar"]), "two archives");
        assert!(!one(&["a.rar", "a.mkv"]), "something outside it");
        assert!(!one(&["a.nfo"]), "no archive at all");
    }

    #[test]
    fn a_file_of_unknown_size_makes_no_content_fingerprint() {
        assert!(content_fingerprint([("a.mkv", Some(GB)), ("b.mkv", None)].into_iter()).is_none());
        assert!(content_fingerprint([("a.mkv", Some(GB)), ("sample.mkv", None)].into_iter()).is_some());
    }

    #[test]
    fn a_twin_is_the_same_in_everything() {
        let file = |name: &str, md5: u8| Par2File { name: name.into(), ..par2(GB, md5) };
        let files = [file("a.rar", 1), file("b.rar", 2)];
        let posted = [("a.rar", 1400), ("b.rar", 1400), ("a.par2", 1), ("a.vol000+16.par2", 9)];
        let print = |files: &[Par2File], block, posted: &[(&str, i64)]| twin_fingerprint(files, Some(block), posted);
        let base = print(&files, 1024, &posted);
        assert!(base.is_some());
        let shuffled = [posted[3], posted[0], posted[2], posted[1]];
        assert_eq!(base, print(&[file("b.rar", 2), file("a.rar", 1)], 1024, &shuffled), "in any order");
        assert_ne!(base, print(&[file("a.rar", 2), file("b.rar", 1)], 1024, &posted), "swapped names");
        assert_ne!(base, print(&[file("a.rar", 1), file("b.rar", 3)], 1024, &posted), "other bytes");
        assert_ne!(base, print(&files, 2048, &posted), "another block size");
        let other_volume = [posted[0], posted[1], posted[2], ("a.vol016+16.par2", 9)];
        assert_ne!(base, print(&files, 1024, &other_volume), "other recovery slices");
        let more = [posted[0], posted[1], posted[2], posted[3], ("a.vol016+16.par2", 9)];
        assert_ne!(base, print(&files, 1024, &more), "more recovery");
        let swapped_counts = [("a.rar", 1300), ("b.rar", 1500), posted[2], posted[3]];
        let other_counts = [("a.rar", 1500), ("b.rar", 1300), posted[2], posted[3]];
        assert_ne!(print(&files, 1024, &swapped_counts), print(&files, 1024, &other_counts), "cut up differently");
        let other_set = Par2File { set_id: [7; 16], ..file("a.rar", 1) };
        assert_ne!(base, print(&[other_set, file("b.rar", 2)], 1024, &posted), "another PAR2 set");
        assert_eq!(twin_fingerprint(&[], Some(1024), &posted), None);
    }

    #[test]
    fn content_matches_on_sizes_whatever_the_names() {
        let a = content(&[("Movie.2024.mkv", 5 * GB), ("Movie.nfo", 2_000)]);
        assert!(a.is_some());
        assert_eq!(a, content(&[("c2f94a1b.mkv", 5 * GB)]), "obfuscated, and no NFO");
        assert_eq!(a, content(&[("Movie.2024.mkv", 5 * GB), ("Sample/movie-sample.mkv", 50 << 20)]), "plus a sample");
        assert_ne!(a, content(&[("Movie.2024.mkv", 5 * GB + 1)]), "another encode");
    }

    fn article(file: &str, part: i64, total: i64, bytes: i64, id: &str) -> ArticleRow {
        ArticleRow {
            message_id: id.into(),
            filename: Some(file.into()),
            part: Some(part),
            total_parts: Some(total),
            bytes: Some(bytes),
            subject: Some(format!("own {file}")),
            poster: None,
            posted_date: None,
        }
    }

    #[test]
    fn gaps_fill_only_from_a_file_that_matches() {
        let mut own = vec![article("a.rar", 1, 3, 100, "own1"), article("a.rar", 3, 3, 50, "own3")];
        let twin = vec![
            article("a.rar", 1, 3, 100, "t1"),
            article("a.rar", 2, 3, 100, "t2"),
            article("a.rar", 3, 3, 50, "t3"),
            article("other.rar", 1, 1, 10, "x"),
        ];
        assert_eq!(fill_gaps(&mut own, &twin), 1);
        assert_eq!(own.iter().map(|a| a.message_id.as_str()).collect::<Vec<_>>(), ["own1", "t2", "own3"]);
        assert_eq!(own[1].subject.as_deref(), Some("own a.rar"), "keeps this release's subject");

        // split differently: its articles wouldn't fit
        let mut own = vec![article("a.rar", 1, 3, 100, "own1")];
        assert_eq!(fill_gaps(&mut own, &[article("a.rar", 1, 3, 90, "t1"), article("a.rar", 2, 3, 90, "t2")]), 0);
        assert_eq!(fill_gaps(&mut own, &[article("a.rar", 2, 4, 100, "t2")]), 0, "another part count");
        assert_eq!(fill_gaps(&mut own, &[article("a.rar", 2, 3, 100, "t2")]), 0, "no part in common");
        // a part both have and both read must hold the same bytes, or the twin's file is another file
        let mut own = vec![article("a.rar", 1, 2, 100, "own1")];
        let twin = vec![article("a.rar", 1, 2, 100, "t1"), article("a.rar", 2, 2, 100, "t2")];
        let known: KnownCrcs = [("own1".to_string(), (90, 1)), ("t1".to_string(), (90, 2))].into_iter().collect();
        assert_eq!(fill_gaps_checked(&mut own, &twin, &known), 0, "other bytes under the same name");
        let known: KnownCrcs = [("own1".to_string(), (90, 1)), ("t1".to_string(), (90, 1))].into_iter().collect();
        assert_eq!(fill_gaps_checked(&mut own, &twin, &known), 1, "the same bytes");
        let known: KnownCrcs = [("t1".to_string(), (90, 2))].into_iter().collect();
        let mut own = vec![article("a.rar", 1, 2, 100, "own1")];
        assert_eq!(fill_gaps_checked(&mut own, &twin, &known), 1, "one side unread says nothing");
    }

    #[test]
    fn a_file_twin_fills_a_file_under_another_name() {
        let mut own = vec![article("abc123.bin", 1, 3, 100, "own1"), article("abc123.bin", 3, 3, 50, "own3")];
        let twin = vec![
            article("Movie.2024.mkv", 1, 3, 100, "t1"),
            article("Movie.2024.mkv", 2, 3, 100, "t2"),
            article("Movie.2024.mkv", 3, 3, 50, "t3"),
            article("Movie.2024.nfo", 1, 1, 10, "n1"),
        ];
        // the sets call it Movie.2024.mkv on both sides; this posting's one data file carries a random name
        let names = vec![("Movie.2024.mkv".to_string(), "Movie.2024.mkv".to_string())];
        assert_eq!(fill_gaps_from(&mut own, &twin, &names, true, &KnownCrcs::new()), 1);
        assert_eq!(own.iter().map(|a| a.message_id.as_str()).collect::<Vec<_>>(), ["own1", "t2", "own3"]);
        assert_eq!(own[1].filename.as_deref(), Some("abc123.bin"), "under this posting's name");
        // a set of several data files says nothing about which one a random name stands for
        let mut own = vec![article("abc123.bin", 1, 3, 100, "own1"), article("abc123.bin", 3, 3, 50, "own3")];
        assert_eq!(fill_gaps_from(&mut own, &twin, &names, false, &KnownCrcs::new()), 0);
        // a name the twin's articles don't carry fills nothing
        let mut own = vec![article("a.rar", 1, 2, 100, "own1")];
        assert_eq!(fill_gaps_from(&mut own, &twin, &[("x.rar".into(), "a.rar".into())], true, &KnownCrcs::new()), 0);
        // two data files here: which is which can't be told by name, soo nothing fills
        let mut own = vec![article("one.bin", 1, 3, 100, "o1"), article("two.bin", 1, 3, 100, "o2")];
        assert_eq!(fill_gaps_from(&mut own, &twin, &names, true, &KnownCrcs::new()), 0);
        // only the file the sets share is read as this release's; the twin's other files aren't, whatever they're called
        let mut own = vec![article("abc123.bin", 1, 3, 100, "own1")];
        let twin = vec![article("abc123.bin", 2, 3, 100, "t2"), article("Movie.2024.nfo", 1, 1, 10, "n1")];
        let nfo = vec![("Movie.2024.nfo".to_string(), "Movie.2024.nfo".to_string())];
        assert_eq!(fill_gaps_from(&mut own, &twin, &nfo, true, &KnownCrcs::new()), 0);
        assert_eq!(own.len(), 1);
    }

    #[test]
    fn a_renamed_repost_fills_from_the_volumes_its_set_names() {
        // the set names the first upload's volumes; this posting renamed every one of them
        let mut own = vec![
            article("oCBvoIlhFvS0L3k1.part001.rar", 1, 2, 100, "o1"),
            article("oCBvoIlhFvS0L3k1.part002.rar", 1, 2, 100, "o3"),
            article("oCBvoIlhFvS0L3k1.part002.rar", 2, 2, 100, "o4"),
            article("oCBvoIlhFvS0L3k1.par2", 1, 1, 10, "op"),
        ];
        let twin = vec![
            article("YMFFZeoeVI6fG4iz.part001.rar", 1, 2, 100, "y1"),
            article("YMFFZeoeVI6fG4iz.part001.rar", 2, 2, 100, "y2"),
            article("YMFFZeoeVI6fG4iz.part002.rar", 1, 2, 100, "y3"),
        ];
        let names: Vec<(String, String)> = ["YMFFZeoeVI6fG4iz.part001.rar", "YMFFZeoeVI6fG4iz.part002.rar"]
            .iter()
            .map(|n| (n.to_string(), n.to_string()))
            .collect();
        assert_eq!(fill_gaps_from(&mut own, &twin, &names, false, &KnownCrcs::new()), 1);
        let borrowed = own.iter().find(|a| a.message_id == "y2").unwrap();
        assert_eq!(borrowed.filename.as_deref(), Some("oCBvoIlhFvS0L3k1.part001.rar"), "under this posting's name");
        // a set of two archives' volumes can't say which posted archive is which
        let two: Vec<(String, String)> =
            ["A.part001.rar", "B.part001.rar"].iter().map(|n| (n.to_string(), n.to_string())).collect();
        let mut own = vec![article("P.part001.rar", 1, 2, 100, "p1")];
        let twin = vec![article("A.part001.rar", 2, 2, 100, "a2"), article("B.part001.rar", 2, 2, 100, "b2")];
        assert_eq!(fill_gaps_from(&mut own, &twin, &two, false, &KnownCrcs::new()), 0);
        // a plain file's ending says nothing about which file it is
        assert_eq!(renamed_volume(&["Other.mkv"], "Movie.mkv"), None);
        // a posting of two archives can't say which one the set's name stands for
        let posted = ["a.part001.rar", "b.part001.rar"];
        assert_eq!(renamed_volume(&posted, "YMFFZeoeVI6fG4iz.part001.rar"), None);
        // a volume with no posted counterpart, and a file that is no volume, map to nothing
        let posted = ["oCBv.part001.rar", "oCBv.par2"];
        assert_eq!(renamed_volume(&posted, "YMFF.part002.rar"), None);
        assert_eq!(renamed_volume(&posted, "YMFF.part001.rar"), Some("oCBv.part001.rar"));
        assert_eq!(renamed_volume(&posted, "YMFF.nfo"), None);
    }

    #[test]
    fn packed_files_and_small_files_say_nothing_about_content() {
        assert_eq!(content(&[("a.part01.rar", GB), ("a.mkv", GB)]), None);
        assert_eq!(content(&[("a.r00", GB)]), None);
        assert_eq!(content(&[("a.001", GB)]), None);
        assert_eq!(content(&[("a.nfo", 2_000)]), None);
        assert_eq!(content(&[]), None);
    }
}
