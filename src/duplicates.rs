//! Other postings of the same content. A **twin** has the same posted files, proven by PAR2 hashes, soo
//! its articles can stand in for this one's. A **pair** holds the same files packed differently, soo
//! it's another whole download, not a source of articles.
//!
//! Each release keeps two fingerprints in its shard, and copies are found by looking them up in every
//! shard, soo there are no clusters to keep in step: purging a release takes its row with it.

use std::collections::{HashMap, HashSet};

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
pub const SET_POSTED_NAMES: &str = "insert into release_fingerprint (release_id, posted_names) values (?1, ?2)
    on conflict(release_id) do update set posted_names = excluded.posted_names";
pub const SET_PAR2_SIZES: &str =
    "insert into release_fingerprint (release_id, par2_block, par2_blocks) values (?1, ?2, ?3)
    on conflict(release_id) do update set par2_block = excluded.par2_block, par2_blocks = excluded.par2_blocks";
pub const SET_CONTENT: &str = "insert into release_fingerprint (release_id, content) values (?1, ?2)
    on conflict(release_id) do update set content = excluded.content";

/// Files smaller than this don't count: NFOs, samples' subtitles, and the like differ between postings.
const MIN_CONTENT_FILE: u64 = 1 << 20;
const MAX_COPIES: i64 = 50;

/// What the posted files are, from a PAR2 set: their sizes and MD5s. Two equal ones are byte-identical.
pub fn posted_fingerprint(files: &[Par2File]) -> Option<Vec<u8>> {
    if files.is_empty() {
        return None;
    }
    let mut hashes: Vec<(u64, [u8; 16])> = files.iter().map(|f| (f.size, f.md5)).collect();
    hashes.sort_unstable();
    hashes.dedup();
    let mut sha = Sha1::new();
    for (size, md5) in hashes {
        sha.update(size.to_le_bytes());
        sha.update(md5);
    }
    Some(sha.finalize().to_vec())
}

/// The same, with each file's name. Two equal ones have the same bytes under each name, which is what filling one's
/// gaps from the other by file name takes: the same files under swapped names are still twins, but not this.
pub fn posted_names_fingerprint(files: &[Par2File]) -> Option<Vec<u8>> {
    if files.is_empty() {
        return None;
    }
    let mut named: Vec<(&str, u64, [u8; 16])> = files.iter().map(|f| (f.name.as_str(), f.size, f.md5)).collect();
    named.sort_unstable();
    named.dedup();
    let mut sha = Sha1::new();
    for (name, size, md5) in named {
        sha.update((name.len() as u64).to_le_bytes());
        sha.update(name.as_bytes());
        sha.update(size.to_le_bytes());
        sha.update(md5);
    }
    Some(sha.finalize().to_vec())
}

fn posted_names(conn: &Connection, id: i64) -> Option<Vec<u8>> {
    let s = store::shard_of_id(id);
    let sql = format!("select posted_names from s{s}.release_fingerprint where release_id = ?");
    conn.query_row(&sql, [id], |r| r.get(0)).ok().flatten()
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
/// soo these aren't the real files, or there's nothing big enough to go by.
pub fn content_fingerprint<'a>(files: impl Iterator<Item = (&'a str, Option<u64>)>) -> Option<Vec<u8>> {
    let mut sizes = Vec::new();
    for (name, size) in files {
        if is_archive(name) {
            return None;
        }
        if !is_sample(name)
            && let Some(size) = size.filter(|s| *s >= MIN_CONTENT_FILE)
        {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Copy {
    Twin,
    Pair,
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

/// The other postings of a release's content, twins first.
pub fn copies_with(conn: &Connection, id: i64) -> db::Result<Vec<(Copy, ReleaseRow)>> {
    let s = store::shard_of_id(id);
    let sql = format!("select posted, content from s{s}.release_fingerprint where release_id = ?");
    let Ok(mut stmt) = conn.prepare(&sql) else { return Ok(Vec::new()) };
    let row =
        stmt.query_row([id], |r| Ok((r.get::<_, Option<Vec<u8>>>(0)?, r.get::<_, Option<Vec<u8>>>(1)?))).optional()?;
    let Some((posted, content)) = row else { return Ok(Vec::new()) };
    let twins = match &posted {
        Some(fingerprint) => matching(conn, "posted", fingerprint, id)?,
        None => Vec::new(),
    };
    let pairs = match &content {
        Some(fingerprint) => matching(conn, "content", fingerprint, id)?,
        None => Vec::new(),
    };
    let mut out = Vec::new();
    for twin in &twins {
        if let Some(release) = get_release_with(conn, *twin)? {
            out.push((Copy::Twin, release));
        }
    }
    for pair in pairs.iter().filter(|pair| !twins.contains(pair)) {
        if let Some(release) = get_release_with(conn, *pair)? {
            out.push((Copy::Pair, release));
        }
    }
    Ok(out)
}

pub fn copies(id: i64) -> db::Result<Vec<(Copy, ReleaseRow)>> {
    copies_with(&db::open()?, id)
}

/// How many other copies a release has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CopyCount {
    pub twins: i64,
    pub pairs: i64,
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
        // a twin with the same content is a twin, not a pair as well
        let same_posting = match (p, c) {
            (Some(p), Some(c)) => both.get(&(p.clone(), c.clone())).map_or(0, |n| (n - 1).max(0)),
            _ => 0,
        };
        let pairs = c.as_ref().and_then(|c| content.get(c)).map_or(0, |n| (n - 1 - same_posting).max(0));
        if twins > 0 || pairs > 0 {
            out.insert(*id, CopyCount { twins, pairs });
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

/// Adds what `own` is missing from a twin's articles, file by file. A twin's file counts only under the
/// same name, with the same number of parts, and the same size for every part both have. Returns how many.
pub fn fill_gaps(own: &mut Vec<ArticleRow>, twin: &[ArticleRow]) -> usize {
    let mut have: HashMap<Option<String>, HashMap<i64, Option<i64>>> = HashMap::new();
    let mut templates: HashMap<Option<String>, ArticleRow> = HashMap::new();
    for article in own.iter() {
        if let Some(part) = article.part {
            have.entry(article.filename.clone()).or_default().insert(part, article.bytes);
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
        let same_sizes =
            articles.iter().all(|a| a.part.and_then(|part| parts.get(&part)).is_none_or(|bytes| *bytes == a.bytes));
        // with no part in common, nothing says the two were cut up the same way
        let shared = articles.iter().any(|a| a.part.is_some_and(|part| parts.contains_key(&part)));
        if !same_total || !same_sizes || !shared {
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
pub fn can_fill(id: i64, copies: &[(Copy, ReleaseRow)]) -> bool {
    let Ok(conn) = db::open() else { return false };
    let names = posted_names(&conn, id);
    names.is_some() && copies.iter().any(|(kind, r)| *kind == Copy::Twin && posted_names(&conn, r.id) == names)
}

/// A release's NZB, its gaps filled from its twins.
pub fn filled_nzb(id: i64) -> db::Result<Option<String>> {
    let conn = db::open()?;
    let Some(release) = get_release_with(&conn, id)? else { return Ok(None) };
    let mut articles = store::articles(&conn, id)?;
    if articles.is_empty() {
        return Ok(None);
    }
    let names = posted_names(&conn, id);
    for (kind, twin) in copies_with(&conn, id)? {
        if kind == Copy::Twin && names.is_some() && posted_names(&conn, twin.id) == names {
            fill_gaps(&mut articles, &store::articles(&conn, twin.id)?);
        }
    }
    Ok(Some(crate::nzb::render_nzb(&release, &articles)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1 << 30;

    #[test]
    fn swapped_names_are_twins_but_cant_lend_articles() {
        let file = |name: &str, md5: u8| Par2File { name: name.into(), ..par2(GB, md5) };
        let (one, swapped) = ([file("a.rar", 1), file("b.rar", 2)], [file("a.rar", 2), file("b.rar", 1)]);
        assert_eq!(posted_fingerprint(&one), posted_fingerprint(&swapped));
        assert_ne!(posted_names_fingerprint(&one), posted_names_fingerprint(&swapped));
        assert_eq!(posted_names_fingerprint(&one), posted_names_fingerprint(&[file("b.rar", 2), file("a.rar", 1)]));
    }

    fn par2(size: u64, md5: u8) -> Par2File {
        Par2File { set_id: [0; 16], file_id: [0; 16], md5: [md5; 16], md5_16k: [0; 16], size, name: "x".into() }
    }

    #[test]
    fn posted_files_match_by_hash_in_any_order() {
        let a = posted_fingerprint(&[par2(GB, 1), par2(GB, 2)]);
        assert!(a.is_some());
        assert_eq!(a, posted_fingerprint(&[par2(GB, 2), par2(GB, 1)]));
        assert_ne!(a, posted_fingerprint(&[par2(GB, 1), par2(GB, 3)]), "same sizes, other bytes");
        assert_eq!(posted_fingerprint(&[]), None);
    }

    fn content(files: &[(&str, u64)]) -> Option<Vec<u8>> {
        content_fingerprint(files.iter().map(|(name, size)| (*name, Some(*size))))
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
