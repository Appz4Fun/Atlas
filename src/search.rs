use rusqlite::{Connection, OptionalExtension, Row, params_from_iter, types::Value};

use crate::category::CatFilter;
use crate::db::{self, Result};
use crate::store;

/// One search hit. `name` is the display name when we have one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReleaseRow {
    pub id: i64,
    pub name: String,
    pub group_name: String,
    pub poster: Option<String>,
    pub posted_date: Option<String>,
    pub size: Option<i64>,
    pub complete: bool,
    pub parts: Option<i64>,
}

/// One article of a release, joined with its release's poster/date.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ArticleRow {
    pub message_id: String,
    pub filename: Option<String>,
    pub part: Option<i64>,
    pub total_parts: Option<i64>,
    pub bytes: Option<i64>,
    pub subject: Option<String>,
    pub poster: Option<String>,
    pub posted_date: Option<String>,
}

pub(crate) const COLUMNS: &str =
    "r.id, coalesce(r.display_name, r.name), r.group_name, r.poster, r.posted_date, r.size, r.complete, r.parts";

/// hide obfuscated junk unless we managed to get a real name for it
pub(crate) const VISIBLE_RELEASE: &str = "(
    r.display_name is not null
    or (r.is_obfuscated = 0 and (length(r.name) < 16 or r.name glob '*[^A-Za-z0-9]*'))
) and not atlas_blocked(r.group_name)";

pub(crate) fn release_row(r: &Row) -> rusqlite::Result<ReleaseRow> {
    Ok(ReleaseRow {
        id: r.get(0)?,
        name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
        group_name: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        poster: r.get(3)?,
        posted_date: r.get(4)?,
        size: r.get(5)?,
        complete: r.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
        parts: r.get(7)?,
    })
}

pub fn fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|raw| raw.trim_matches('"').replace('"', ""))
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\"*"))
        .collect::<Vec<_>>()
        .join(" AND ")
}

pub fn like_query(query: &str) -> String {
    let q = query.trim().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    format!("%{q}%")
}

fn query_rows(conn: &Connection, sql: &str, vals: &[Value]) -> Result<Vec<ReleaseRow>> {
    let mut stmt = conn.prepare(sql)?;

    stmt.query_map(params_from_iter(vals.iter()), release_row)?.collect()
}

/// try fts first, fall back to LIKE if fts chokes on the query
fn fts_or_like_rows(fts: (&str, Vec<Value>), like: (&str, Vec<Value>)) -> Result<Vec<ReleaseRow>> {
    let conn = db::open()?;
    query_rows(&conn, fts.0, &fts.1).or_else(|_| query_rows(&conn, like.0, &like.1))
}

fn fts_or_like_count(fts: (&str, Vec<Value>), like: (&str, Vec<Value>)) -> Result<i64> {
    let conn = db::open()?;
    let count = |sql: &str, vals: &[Value]| conn.query_row(sql, params_from_iter(vals.iter()), |r| r.get(0));
    count(fts.0, &fts.1).or_else(|_| count(like.0, &like.1))
}

fn text(s: impl Into<String>) -> Value {
    Value::Text(s.into())
}

/// The shards to look in: just the group's when there is one.
fn shards(groups: Option<&[&str]>) -> Vec<usize> {
    match groups {
        Some(list) => {
            let mut s: Vec<usize> = list.iter().map(|g| store::shard_of(g)).collect();
            s.sort_unstable();
            s.dedup();
            s
        }
        None => (0..store::SHARDS).collect(),
    }
}

/// `part` for each of `shards`, joined with union all
fn union(shards: &[usize], part: impl Fn(usize) -> String) -> String {
    let parts: Vec<String> = shards.iter().map(|&i| part(i)).collect();
    if parts.is_empty() { "select null where 0".into() } else { parts.join("\nunion all\n") }
}

/// Matches of a word's exact form, across shards, past which it's too common to rank.
const COMMON_MATCHES: i64 = 2_000;

/// The newest matches of the words as written, when they match too much to rank: scoring has to read every match, and
/// a word with a `*` has to read all of them before the first comes back, which is seconds on a cold disk. Exact words
/// are read newest first and stop at a page. None: few enough to rank, or the FTS index won't take the query.
fn common_words_newest(
    conn: &Connection,
    query: &str,
    group: Option<&str>,
    page: i64,
    page_size: i64,
    common: i64,
) -> Option<Vec<ReleaseRow>> {
    let exact = fts_query(query).replace("\"*", "\"");
    if exact.is_empty() {
        return None;
    }
    let in_shards = shards(group.map(|g| [g]).as_ref().map(|a| a.as_slice()));
    let mut found = 0;
    for i in &in_shards {
        let sql = format!(
            "select count(*) from (select 1 from s{i}.releases_fts where releases_fts match ?1 limit {common})"
        );
        found += conn.query_row(&sql, [&exact], |r| r.get::<_, i64>(0)).ok()?;
        if found >= common {
            break;
        }
    }
    if found < common {
        return None;
    }
    let group_filter = if group.is_some() { "r.group_name = ?2 and " } else { "" };
    let sql = format!(
        "select * from ({}) order by 1 desc limit ?3 offset ?4",
        union(&in_shards, |i| format!(
            "select * from (select {COLUMNS} from s{i}.releases_fts f cross join s{i}.releases r on r.id = f.rowid
             where f.releases_fts match ?1 and {group_filter}{VISIBLE_RELEASE}
             order by f.rowid desc limit ?3 + ?4)"
        ))
    );
    let vals = [
        text(exact),
        group.map(text).unwrap_or(Value::Null),
        Value::Integer(page_size),
        Value::Integer(page * page_size),
    ];
    query_rows(conn, &sql, &vals).ok()
}

/// Search within one group (`Some`) or all of them (`None`). `page` is 0 based.
///
/// Parameters: ?1 the query, ?2 the group, ?3 limit, ?4 offset.
pub fn search(query: &str, group: Option<&str>, page: i64, page_size: i64) -> Result<Vec<ReleaseRow>> {
    search_past(query, group, page, page_size, COMMON_MATCHES)
}

#[doc(hidden)]
pub fn search_past(
    query: &str,
    group: Option<&str>,
    page: i64,
    page_size: i64,
    common: i64,
) -> Result<Vec<ReleaseRow>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    if let Some(rows) = common_words_newest(&db::open()?, query, group, page, page_size, common) {
        return Ok(rows);
    }

    let group_filter = if group.is_some() { "r.group_name = ?2 and " } else { "" };
    let in_shards = shards(group.map(|g| [g]).as_ref().map(|a| a.as_slice()));

    // fts tables hold the text, the release data lives in releases
    let fts_sql = format!(
        "select * from ({})
        order by rank
        limit ?3 offset ?4",
        union(&in_shards, |i| format!(
            "select {COLUMNS}, ranked.rank as rank
            from s{i}.releases r
            join (select rowid, bm25(releases_fts) as rank from s{i}.releases_fts where releases_fts match ?1) ranked
                on ranked.rowid = r.id
            where {group_filter}{VISIBLE_RELEASE}"
        ))
    );

    let like_sql = format!(
        "select * from ({})
        order by sort_name
        limit ?3 offset ?4",
        union(&in_shards, |i| format!(
            "select {COLUMNS}, r.name as sort_name
            from s{i}.releases r
            where (r.name like ?1 escape '\\' or r.display_name like ?1 escape '\\')
            and {group_filter}{VISIBLE_RELEASE}"
        ))
    );

    let group_value = group.map(text).unwrap_or(Value::Null);
    let tail = [group_value, Value::Integer(page_size), Value::Integer(page * page_size)];
    let fts_vals: Vec<Value> = std::iter::once(text(fts_query(query))).chain(tail.clone()).collect();
    let like_vals: Vec<Value> = std::iter::once(text(like_query(query))).chain(tail).collect();

    fts_or_like_rows((&fts_sql, fts_vals), (&like_sql, like_vals))
}

/// Parameters: ?1 the query, ?2 the group.
pub fn count(query: &str, group: Option<&str>) -> Result<i64> {
    if query.trim().is_empty() {
        return Ok(0);
    }

    let group_filter = if group.is_some() { "r.group_name = ?2 and " } else { "" };
    let in_shards = shards(group.map(|g| [g]).as_ref().map(|a| a.as_slice()));

    let fts_sql = format!(
        "select coalesce(sum(c), 0) from ({})",
        union(&in_shards, |i| format!(
            "select count(*) as c from s{i}.releases r
            join (select rowid from s{i}.releases_fts where releases_fts match ?1) matches on matches.rowid = r.id
            where {group_filter}{VISIBLE_RELEASE}"
        ))
    );

    let like_sql = format!(
        "select coalesce(sum(c), 0) from ({})",
        union(&in_shards, |i| format!(
            "select count(*) as c from s{i}.releases r
            where (r.name like ?1 escape '\\' or r.display_name like ?1 escape '\\')
            and {group_filter}{VISIBLE_RELEASE}"
        ))
    );

    // ?2 is only in the sql with a group
    let mut fts_vals = vec![text(fts_query(query))];
    let mut like_vals = vec![text(like_query(query))];
    if let Some(g) = group {
        fts_vals.push(text(g));
        like_vals.push(text(g));
    }

    fts_or_like_count((&fts_sql, fts_vals), (&like_sql, like_vals))
}

pub fn search_releases(query: &str, group: &str, page: i64, page_size: i64) -> Result<Vec<ReleaseRow>> {
    search(query, Some(group), page, page_size)
}

pub fn search_all_releases(query: &str, page: i64, page_size: i64) -> Result<Vec<ReleaseRow>> {
    crate::metrics::timed("search.by_name", || search(query, None, page, page_size))
}

pub fn count_releases(query: &str, group: &str) -> Result<i64> {
    count(query, Some(group))
}

pub fn count_all_releases(query: &str) -> Result<i64> {
    count(query, None)
}

/// The newest releases matching `filter` (sql on `r`), across `in_shards`:
/// each shard's newest page worth, then merged. Ids grow in the order releases
/// were added whatever the shard, soo newest first is id order.
/// Parameters: ?1 limit, ?2 offset, then `extra` from ?3.
fn newest(in_shards: &[usize], filter: &str, extra: Vec<Value>, page: i64, page_size: i64) -> Result<Vec<ReleaseRow>> {
    let conn = db::open()?;
    let sql = format!(
        "select * from ({}) order by 1 desc limit ?1 offset ?2",
        union(in_shards, |i| format!(
            "select * from (select {COLUMNS} from s{i}.releases r where {filter} order by r.id desc limit ?1 + ?2)"
        ))
    );
    let mut vals = vec![Value::Integer(page_size), Value::Integer(page * page_size)];
    vals.extend(extra);
    query_rows(&conn, &sql, &vals)
}

/// A release by id, if it's one a client may see.
pub(crate) fn visible_release(conn: &Connection, id: i64) -> Result<Option<ReleaseRow>> {
    conn.prepare_cached(&probe_sql(store::shard_of_id(id)))?.query_row([id], release_row).optional()
}

/// What a filtered read may look at, at most.
#[derive(Clone, Copy)]
pub struct Bounds {
    /// rows from leaf cursors a query may look at and reject
    pub rejects: usize,
    /// releases per shard the no-row cursor walks, at least
    pub no_row: i64,
    /// newest matches per shard
    pub fts: i64,
    /// newest releases per shard
    pub like: i64,
}

impl Bounds {
    pub const DEFAULT: Bounds = Bounds { rejects: 2_000, no_row: 20_000, fts: 20_000, like: 100_000 };
}

pub const MAX_FILTER_OFFSET: i64 = 10_000;

pub struct CatPage {
    pub rows: Vec<ReleaseRow>,
    pub scanned: usize,
}

fn leaf_cursor_sql(s: usize) -> String {
    format!("select c.release_id from s{s}.release_category c where c.category = ?1 order by c.release_id desc")
}

fn no_row_cursor_sql(s: usize) -> String {
    format!(
        "select r.id from (select id, name, display_name, group_name, is_obfuscated from s{s}.releases order by id desc limit ?1) r
         where {VISIBLE_RELEASE}
           and not exists (select 1 from s{s}.release_category k where k.release_id = r.id)"
    )
}

fn probe_sql(s: usize) -> String {
    format!("select {COLUMNS} from s{s}.releases r where r.id = ?1 and {VISIBLE_RELEASE}")
}

/// The newest releases in a category filter, newest first: one cursor per leaf per shard, plus one per
/// shard for rows with no category when that's asked for, merged lazily. Only rows that reach the top are read.
pub fn newest_in_categories(filter: &CatFilter, offset: i64, limit: i64) -> Result<CatPage> {
    crate::metrics::timed("search.newest_in_categories", || {
        newest_in_categories_with(&db::open()?, filter, offset, limit, &Bounds::DEFAULT)
    })
}

pub fn newest_in_categories_with(
    conn: &Connection,
    f: &CatFilter,
    offset: i64,
    limit: i64,
    bounds: &Bounds,
) -> Result<CatPage> {
    if offset > MAX_FILTER_OFFSET || (f.leaves.is_empty() && !f.uncategorized) {
        return Ok(CatPage { rows: Vec::new(), scanned: 0 });
    }
    let want = offset + limit;
    let no_row = bounds.no_row.max(4 * want);
    let mut stmts: Vec<(rusqlite::Statement<'_>, i64)> = Vec::new();
    for s in 0..store::SHARDS {
        for &leaf in &f.leaves {
            stmts.push((conn.prepare(&leaf_cursor_sql(s))?, leaf));
        }
        if f.uncategorized {
            stmts.push((conn.prepare(&no_row_cursor_sql(s))?, no_row));
        }
    }
    let mut cursors = Vec::with_capacity(stmts.len());
    for (stmt, value) in stmts.iter_mut() {
        cursors.push(stmt.query([*value])?);
    }
    let mut heap = std::collections::BinaryHeap::new();
    for (i, c) in cursors.iter_mut().enumerate() {
        if let Some(row) = c.next()? {
            heap.push((row.get::<_, i64>(0)?, i));
        }
    }
    let mut probes = (0..store::SHARDS).map(|s| conn.prepare(&probe_sql(s))).collect::<Result<Vec<_>>>()?;
    let (mut rows, mut seen, mut scanned, mut rejected, mut last) = (Vec::new(), 0i64, 0usize, 0usize, None);
    while let Some((id, i)) = heap.pop() {
        if let Some(row) = cursors[i].next()? {
            heap.push((row.get::<_, i64>(0)?, i));
        }
        // a row written between two cursors' reads
        if last == Some(id) {
            continue;
        }
        last = Some(id);
        scanned += 1;
        // the no-row cursor filters on VISIBLE_RELEASE itself, soo only leaf rows get rejected here
        let Some(hit) = probes[store::shard_of_id(id)].query_row([id], release_row).optional()? else {
            rejected += 1;
            if rejected > bounds.rejects {
                break;
            }
            continue;
        };
        seen += 1;
        if seen > offset {
            rows.push(hit);
            if rows.len() as i64 == limit {
                break;
            }
        }
    }
    Ok(CatPage { rows, scanned })
}

/// ids come from the category table, never from the request, soo inlining them is safe
fn cat_clause(f: &CatFilter) -> String {
    let list = f.leaves.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    match (list.is_empty(), f.uncategorized) {
        (true, false) => "0".into(),
        (true, true) => "c.release_id is null".into(),
        (false, false) => format!("c.category in ({list})"),
        (false, true) => format!("(c.category in ({list}) or c.release_id is null)"),
    }
}

/// The newest matches of each shard that are in `f`, best first and newest first among equals, read in the order that costs least: the
/// matches off the fts index, the category table for each, a score only for those in `f` (bm25 reads a row's
/// size per call), and no `releases` row at all, which would be about a page read per match.
/// A cross join soo the matches stay the outer loop, which a plain join would leave to the planner.
/// Parameters: ?1 the query, ?2 matches per shard.
fn ranked_ids_sql(f: &CatFilter) -> String {
    let join = if f.uncategorized { "left join" } else { "cross join" };
    let clause = cat_clause(f);
    format!(
        "select id from ({}) order by rank, id desc",
        union(&shards(None), |i| format!(
            "select releases_fts.rowid as id, bm25(releases_fts) as rank
               from s{i}.releases_fts
               {join} s{i}.release_category c on c.release_id = releases_fts.rowid
              where releases_fts match ?1
                and releases_fts.rowid >= (select min(rowid) from (select rowid from s{i}.releases_fts
                                            where releases_fts match ?1 order by rowid desc limit ?2))
                and {clause}"
        ))
    )
}

/// Reads `releases` only for ids that get as far as the page, skipping the ones a client may not see.
fn page_of_ids(conn: &Connection, sql: &str, vals: &[Value], offset: i64, limit: i64) -> Result<Vec<ReleaseRow>> {
    let mut probes = (0..store::SHARDS).map(|s| conn.prepare(&probe_sql(s))).collect::<Result<Vec<_>>>()?;
    let mut stmt = conn.prepare(sql)?;
    let mut ids = stmt.query(params_from_iter(vals.iter()))?;
    let (mut rows, mut seen) = (Vec::new(), 0i64);
    while (rows.len() as i64) < limit {
        let Some(row) = ids.next()? else { break };
        let id: i64 = row.get(0)?;
        if let Some(hit) = probes[store::shard_of_id(id)].query_row([id], release_row).optional()? {
            seen += 1;
            if seen > offset {
                rows.push(hit);
            }
        }
    }
    Ok(rows)
}

/// A search within a category filter: ranked, over the newest matches of each shard.
pub fn search_in_categories(query: &str, filter: &CatFilter, offset: i64, limit: i64) -> Result<Vec<ReleaseRow>> {
    crate::metrics::timed("search.by_name_in_categories", || {
        search_in_categories_with(&db::open()?, query, filter, offset, limit, &Bounds::DEFAULT)
    })
}

pub fn search_in_categories_with(
    conn: &Connection,
    query: &str,
    f: &CatFilter,
    offset: i64,
    limit: i64,
    bounds: &Bounds,
) -> Result<Vec<ReleaseRow>> {
    if offset > MAX_FILTER_OFFSET || (f.leaves.is_empty() && !f.uncategorized) {
        return Ok(Vec::new());
    }
    let clause = cat_clause(f);
    let like = format!(
        "select * from ({}) order by sort_name limit ?3 offset ?4",
        union(&shards(None), |i| format!(
            "select {COLUMNS}, r.name as sort_name
               from (select * from s{i}.releases order by id desc limit ?2) r
               left join s{i}.release_category c on c.release_id = r.id
              where (r.name like ?1 escape '\\' or r.display_name like ?1 escape '\\')
                and {VISIBLE_RELEASE} and {clause}"
        ))
    );
    let fts_vals = [text(fts_query(query)), Value::Integer(bounds.fts)];
    let like_vals =
        [text(like_query(query)), Value::Integer(bounds.like), Value::Integer(limit), Value::Integer(offset)];
    page_of_ids(conn, &ranked_ids_sql(f), &fts_vals, offset, limit).or_else(|_| query_rows(conn, &like, &like_vals))
}

pub fn all_releases(page: i64, page_size: i64) -> Result<Vec<ReleaseRow>> {
    crate::metrics::timed("search.newest", || newest(&shards(None), VISIBLE_RELEASE, Vec::new(), page, page_size))
}

pub fn search_obfuscated(page: i64, page_size: i64) -> Result<Vec<ReleaseRow>> {
    newest(
        &shards(None),
        "r.is_obfuscated = 1 and r.display_name is null and not atlas_blocked(r.group_name)",
        Vec::new(),
        page,
        page_size,
    )
}

pub fn count_obfuscated() -> Result<i64> {
    let sql = format!(
        "select coalesce(sum(c), 0) from ({})",
        union(&shards(None), |i| format!(
            "select count(*) as c from s{i}.releases where is_obfuscated = 1 and display_name is null and not atlas_blocked(group_name)"
        ))
    );
    db::open()?.query_row(&sql, [], |r| r.get(0))
}

pub fn get_release(id: i64) -> Result<Option<ReleaseRow>> {
    get_release_with(&db::open()?, id)
}

/// `get_release` on a connection with the shards attached.
pub fn get_release_with(conn: &Connection, id: i64) -> Result<Option<ReleaseRow>> {
    let sql = format!("select {COLUMNS} from s{}.releases r where r.id = ?", store::shard_of_id(id));
    Ok(conn.query_row(&sql, [id], release_row).optional()?.filter(|r| !crate::filters::group_blocked(&r.group_name)))
}

pub fn recent_in_groups(groups: &[String], page: i64, page_size: i64) -> Result<Vec<ReleaseRow>> {
    if groups.is_empty() {
        return Ok(Vec::new());
    }

    let names: Vec<&str> = groups.iter().map(String::as_str).filter(|g| !crate::filters::group_blocked(g)).collect();
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let qs = (0..names.len()).map(|n| format!("?{}", n + 3)).collect::<Vec<_>>().join(",");
    let extra = names.iter().map(|g| text(*g)).collect();
    newest(&shards(Some(&names)), &format!("r.group_name in ({qs})"), extra, page, page_size)
}

/// Articles ordered by filename then part soo files can be reassembled.
pub fn get_articles(release_id: i64) -> Result<Vec<ArticleRow>> {
    let conn = db::open()?;
    if get_release_with(&conn, release_id)?.is_none() {
        return Ok(Vec::new());
    }
    store::articles(&conn, release_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_query_quotes_terms() {
        assert_eq!(fts_query(r#"the "matrix" 1999"#), r#""the"* AND "matrix"* AND "1999"*"#);
        assert_eq!(fts_query("   "), "");
    }

    #[test]
    fn like_query_escapes() {
        assert_eq!(like_query(" 50%_off\\ "), "%50\\%\\_off\\\\%");
    }

    // ---- category filters

    use crate::category::{CatFilter, parse_filter};

    struct Fixture {
        _dir: tempfile::TempDir,
        main: std::path::PathBuf,
    }

    fn cats_fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        Fixture { _dir: dir, main }
    }

    /// groups in different shards
    fn groups_in_shards(n: usize) -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        let mut shards = std::collections::HashSet::new();
        for i in 0.. {
            let group = format!("alt.binaries.t{i}");
            if shards.insert(store::shard_of(&group)) {
                found.push(group);
                if found.len() == n {
                    break;
                }
            }
        }
        found
    }

    fn cat_release(name: &str, group: &str) -> crate::parser::Release {
        let article = crate::parser::Article {
            message_id: format!("<{name}@x>"),
            subject: format!("\"{name}\" yEnc (1/1)"),
            author: "p <p@x>".into(),
            filename: Some(format!("{name}.bin")),
            part: Some(1),
            total_parts: Some(1),
            bytes: 10,
            ..Default::default()
        };
        crate::parser::Release {
            name: name.into(),
            group: group.into(),
            articles: vec![article],
            size: 10,
            complete: true,
            ..Default::default()
        }
    }

    impl Fixture {
        /// saved one at a time, soo ids rise in this order; `category` is its row's, if it has one
        fn add(&self, name: &str, group: &str, category: Option<i64>) -> i64 {
            store::save(&self.main, &[cat_release(name, group)]).unwrap();
            let shard = db::open_shard(&store::shard_path(&self.main, store::shard_of(group))).unwrap();
            let id: i64 = shard.query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap();
            if let Some(c) = category {
                shard
                    .execute(
                        "insert into release_category (release_id, category) values (?, ?)",
                        rusqlite::params![id, c],
                    )
                    .unwrap();
            }
            id
        }

        fn conn(&self) -> Connection {
            db::open_with_shards(&self.main).unwrap()
        }

        fn newest(&self, f: &CatFilter, offset: i64, limit: i64, bounds: &Bounds) -> CatPage {
            newest_in_categories_with(&self.conn(), f, offset, limit, bounds).unwrap()
        }
    }

    fn ids(page: &CatPage) -> Vec<i64> {
        page.rows.iter().map(|r| r.id).collect()
    }

    fn plan(conn: &Connection, sql: &str) -> String {
        let mut stmt = conn.prepare(&format!("explain query plan {sql}")).unwrap();
        let rows: Vec<String> = stmt.query_map([1], |r| r.get(3)).unwrap().map(|r| r.unwrap()).collect();
        rows.join(" / ")
    }

    #[test]
    fn the_leaf_cursor_needs_no_sort() {
        let f = cats_fixture();
        let conn = f.conn();
        let leaf = plan(&conn, &leaf_cursor_sql(0));
        assert!(leaf.contains("USING COVERING INDEX category_release"), "{leaf}");
        assert!(!leaf.contains("TEMP B-TREE"), "{leaf}");
        let no_row = plan(&conn, &no_row_cursor_sql(0));
        assert!(no_row.contains("SEARCH k USING INTEGER PRIMARY KEY"), "{no_row}");
    }

    #[test]
    fn newest_first_across_shards_with_exact_paging() {
        let f = cats_fixture();
        let groups = groups_in_shards(4);
        let mut tv = Vec::new();
        for i in 0..40 {
            let category = if i % 2 == 0 { 5040 } else { 2040 };
            let id = f.add(&format!("Rel.{i:03}"), &groups[i % 4], Some(category));
            if category == 5040 {
                tv.push(id);
            }
        }
        tv.reverse();
        let filter = parse_filter("5040").unwrap();
        assert_eq!(ids(&f.newest(&filter, 3, 7, &Bounds::DEFAULT)), tv[3..10]);
        let mut paged = Vec::new();
        for offset in (0..20).step_by(6) {
            paged.extend(ids(&f.newest(&filter, offset, 6, &Bounds::DEFAULT)));
        }
        assert_eq!(paged, tv, "no gap and no repeat");
    }

    #[test]
    fn a_parent_merges_its_leaves() {
        let f = cats_fixture();
        let groups = groups_in_shards(3);
        let a = f.add("Rel.A", &groups[0], Some(5030));
        let b = f.add("Rel.B", &groups[1], Some(5040));
        let c = f.add("Rel.C", &groups[2], Some(5999));
        assert_eq!(ids(&f.newest(&parse_filter("5000").unwrap(), 0, 10, &Bounds::DEFAULT)), [c, b, a]);
    }

    #[test]
    fn a_rare_category_is_found_past_many_others() {
        let f = cats_fixture();
        let group = "alt.binaries.t0";
        let rows: Vec<_> = (0..5_000).map(|i| cat_release(&format!("Many.{i}"), group)).collect();
        store::save(&f.main, &rows).unwrap();
        let shard = db::open_shard(&store::shard_path(&f.main, store::shard_of(group))).unwrap();
        shard.execute("insert into release_category (release_id, category) select id, 5040 from releases", []).unwrap();
        let rare = [
            f.add("Rare.A", group, Some(7020)),
            f.add("Rare.B", group, Some(7020)),
            f.add("Rare.C", group, Some(7020)),
        ];
        let page = f.newest(&parse_filter("7020").unwrap(), 0, 100, &Bounds::DEFAULT);
        assert_eq!(page.rows.len(), 3);
        assert!(page.scanned <= 3, "{}", page.scanned);
        assert_eq!(ids(&page), [rare[2], rare[1], rare[0]]);
    }

    #[test]
    fn no_row_means_eight_thousand() {
        let f = cats_fixture();
        let groups = groups_in_shards(2);
        let none = f.add("No.Row", &groups[0], None);
        let misc = f.add("Misc.Row", &groups[1], Some(8010));
        assert_eq!(ids(&f.newest(&parse_filter("8000").unwrap(), 0, 10, &Bounds::DEFAULT)), [misc, none]);
        assert!(ids(&f.newest(&parse_filter("5000").unwrap(), 0, 10, &Bounds::DEFAULT)).is_empty());
        assert!(ids(&f.newest(&parse_filter("7000").unwrap(), 0, 10, &Bounds::DEFAULT)).is_empty());
    }

    #[test]
    fn hidden_rows_are_skipped_and_the_scan_is_bounded() {
        let filter = parse_filter("8020").unwrap();
        for (hidden, found) in [(2_500, false), (100, true)] {
            let f = cats_fixture();
            let group = "alt.binaries.t0";
            let old = f.add("Visible.Old.Name", group, Some(8020));
            let rows: Vec<_> = (0..hidden)
                .map(|i| crate::parser::Release { is_obfuscated: true, ..cat_release(&format!("Hidden.{i}"), group) })
                .collect();
            store::save(&f.main, &rows).unwrap();
            let shard = db::open_shard(&store::shard_path(&f.main, store::shard_of(group))).unwrap();
            shard
                .execute("insert into release_category (release_id, category) select id, 8020 from releases where is_obfuscated = 1", [])
                .unwrap();
            let page = f.newest(&filter, 0, 10, &Bounds::DEFAULT);
            if found {
                assert_eq!(ids(&page), [old]);
            } else {
                assert!(page.rows.is_empty());
                assert_eq!(page.scanned, Bounds::DEFAULT.rejects + 1);
            }
        }
    }

    #[test]
    fn the_no_row_cursor_prepares_with_the_whole_visibility_rule() {
        let f = cats_fixture();
        let filter = CatFilter { leaves: vec![], uncategorized: true };
        assert!(ids(&f.newest(&filter, 0, 10, &Bounds::DEFAULT)).is_empty());
        let blocked = "alt.binaries.pictures.erotica.teen";
        assert!(crate::filters::group_blocked(blocked));
        let id = {
            let shard = db::open_shard(&store::shard_path(&f.main, store::shard_of(blocked))).unwrap();
            shard
                .execute(
                    "insert into releases (id, name, group_name, size, complete, parts) values (8, 'Blocked.Row', ?, 1, 1, 1)",
                    [blocked],
                )
                .unwrap();
            8
        };
        assert!(!ids(&f.newest(&filter, 0, 10, &Bounds::DEFAULT)).contains(&id));
    }

    #[test]
    fn the_no_row_cursor_is_bounded() {
        let f = cats_fixture();
        let group = "alt.binaries.t0";
        let old = f.add("Old.Row", group, None);
        let rows: Vec<_> = (0..500).map(|i| cat_release(&format!("Cat.{i}"), group)).collect();
        store::save(&f.main, &rows).unwrap();
        let shard = db::open_shard(&store::shard_path(&f.main, store::shard_of(group))).unwrap();
        shard.execute("insert into release_category (release_id, category) select id, 5040 from releases where name like 'Cat.%'", []).unwrap();
        let filter = CatFilter { leaves: vec![], uncategorized: true };
        let tight = Bounds { no_row: 100, ..Bounds::DEFAULT };
        assert!(f.newest(&filter, 0, 10, &tight).rows.is_empty());
        assert_eq!(ids(&f.newest(&filter, 0, 10, &Bounds::DEFAULT)), [old]);
    }

    #[test]
    fn an_offset_past_the_limit_is_empty() {
        let f = cats_fixture();
        f.add("Rel.A", "alt.binaries.t0", Some(5040));
        let page = f.newest(&parse_filter("5040").unwrap(), MAX_FILTER_OFFSET + 1, 10, &Bounds::DEFAULT);
        assert!(page.rows.is_empty());
        assert_eq!(page.scanned, 0);
    }

    #[test]
    fn an_empty_filter_matches_nothing() {
        let f = cats_fixture();
        f.add("Rel.A", "alt.binaries.t0", Some(5040));
        let filter = parse_filter("9999").unwrap();
        assert!(f.newest(&filter, 0, 10, &Bounds::DEFAULT).rows.is_empty());
        assert!(search_in_categories_with(&f.conn(), "Rel", &filter, 0, 10, &Bounds::DEFAULT).unwrap().is_empty());
    }

    fn names(rows: &[ReleaseRow]) -> Vec<String> {
        let mut out: Vec<String> = rows.iter().map(|r| r.name.clone()).collect();
        out.sort();
        out
    }

    #[test]
    fn search_in_categories_filters_ranks_and_keeps_no_row_rows() {
        let f = cats_fixture();
        let groups = groups_in_shards(3);
        f.add("Findme.Tv", &groups[0], Some(5040));
        f.add("Findme.None", &groups[1], None);
        let hidden = crate::parser::Release { is_obfuscated: true, ..cat_release("Findme.Hidden", &groups[2]) };
        store::save(&f.main, &[hidden]).unwrap();
        let conn = f.conn();
        let search = |cat: &str| {
            let filter = parse_filter(cat).unwrap();
            names(&search_in_categories_with(&conn, "Findme", &filter, 0, 10, &Bounds::DEFAULT).unwrap())
        };
        assert_eq!(search("5040"), ["Findme.Tv"]);
        assert_eq!(search("8000"), ["Findme.None"]);
        assert!(search("7000").is_empty());
    }

    #[test]
    fn search_in_categories_looks_at_the_newest_matches_only() {
        let f = cats_fixture();
        let group = "alt.binaries.t0";
        f.add("Findme.Old", group, Some(7020));
        let rows: Vec<_> = (0..300).map(|i| cat_release(&format!("Findme.Filler.{i}"), group)).collect();
        store::save(&f.main, &rows).unwrap();
        f.add("Findme.New", group, Some(7020));
        let conn = f.conn();
        let tight = Bounds { fts: 100, ..Bounds::DEFAULT };
        let got = search_in_categories_with(&conn, "Findme", &parse_filter("7020").unwrap(), 0, 10, &tight).unwrap();
        assert_eq!(names(&got), ["Findme.New"]);
    }

    #[test]
    fn ranking_reads_no_release_rows_and_starts_from_the_matches() {
        let f = cats_fixture();
        let conn = f.conn();
        for cat in ["5040", "8000", "5040,8000"] {
            let sql = ranked_ids_sql(&parse_filter(cat).unwrap());
            let mut stmt = conn.prepare(&format!("explain query plan {sql}")).unwrap();
            let params = rusqlite::params!["Findme", 100];
            let details: Vec<String> = stmt.query_map(params, |r| r.get(3)).unwrap().map(|r| r.unwrap()).collect();
            assert!(!details.iter().any(|d| d.contains(" r ") || d.contains("releases ")), "{details:?}");
            for d in details.iter().filter(|d| d.contains(" c ")) {
                assert!(d.contains("SEARCH c USING INTEGER PRIMARY KEY"), "{details:?}");
            }
        }
    }

    #[test]
    fn equal_ranks_come_newest_first_across_shards() {
        let f = cats_fixture();
        let groups = groups_in_shards(4);
        let added: Vec<i64> = (0..8).map(|i| f.add(&format!("Findme.{i}"), &groups[i % 4], Some(5040))).collect();
        let conn = f.conn();
        let got =
            search_in_categories_with(&conn, "Findme", &parse_filter("5040").unwrap(), 0, 3, &Bounds::DEFAULT).unwrap();
        let want: Vec<i64> = added.iter().rev().take(3).copied().collect();
        assert_eq!(got.iter().map(|r| r.id).collect::<Vec<_>>(), want);
    }

    #[test]
    fn search_in_categories_pages_past_hidden_rows() {
        let f = cats_fixture();
        let group = "alt.binaries.t0";
        // the same text soo rank doesn't depend on length, hidden and visible in turn
        let mut visible = Vec::new();
        for i in 0..6 {
            let hidden = crate::parser::Release { is_obfuscated: true, ..cat_release(&format!("Findme.H{i}"), group) };
            store::save(&f.main, &[hidden]).unwrap();
            let id = f.add(&format!("Findme.V{i}"), group, Some(5040));
            visible.push(id);
        }
        for path in store::shard_paths(&f.main) {
            db::open_shard(&path)
                .unwrap()
                .execute_batch(
                    "insert into release_category (release_id, category)
                     select id, 5040 from releases where is_obfuscated = 1",
                )
                .unwrap();
        }
        let conn = f.conn();
        let filter = parse_filter("5040").unwrap();
        let page = |offset, limit| search_in_categories_with(&conn, "Findme", &filter, offset, limit, &Bounds::DEFAULT);
        let all: Vec<i64> = page(0, 100).unwrap().iter().map(|r| r.id).collect();
        assert_eq!(all.len(), 6);
        assert!(visible.iter().all(|id| all.contains(id)));
        let first: Vec<i64> = page(0, 4).unwrap().iter().map(|r| r.id).collect();
        let rest: Vec<i64> = page(4, 4).unwrap().iter().map(|r| r.id).collect();
        assert_eq!(first, all[..4]);
        assert_eq!(rest, all[4..]);
        assert!(page(6, 4).unwrap().is_empty());
        assert!(page(0, 0).unwrap().is_empty());
    }

    #[test]
    fn the_like_fallback_filters_too() {
        let f = cats_fixture();
        let group = "alt.binaries.t0";
        f.add("Findme.Tv", group, Some(5040));
        f.add("Findme.Book", group, Some(7020));
        for path in store::shard_paths(&f.main) {
            db::open_shard(&path).unwrap().execute_batch("drop table releases_fts").unwrap();
        }
        let conn = f.conn();
        let got = search_in_categories_with(&conn, "Findme", &parse_filter("5040").unwrap(), 0, 10, &Bounds::DEFAULT)
            .unwrap();
        assert_eq!(names(&got), ["Findme.Tv"]);
    }

    #[test]
    fn categories_of_reads_each_shard() {
        let f = cats_fixture();
        let groups = groups_in_shards(8);
        let mut want = std::collections::HashMap::new();
        let mut all = Vec::new();
        for (i, group) in groups.iter().enumerate() {
            let category = (i != 3).then_some(5040 - (i as i64 % 2) * 10);
            let id = f.add(&format!("Rel.{i}"), group, category);
            all.push(id);
            if let Some(c) = category {
                want.insert(id, c);
            }
        }
        assert_eq!(crate::category::categories_of_with(&f.conn(), &all).unwrap(), want);
        assert!(crate::category::categories_of(&[]).unwrap().is_empty());
    }

    #[test]
    fn the_hidden_rule_matches_visible_release() {
        let conn = Connection::open_in_memory().unwrap();
        crate::filters::register_sql(&conn).unwrap();
        conn.execute_batch(
            "create table releases (id integer primary key, name text, display_name text, group_name text, is_obfuscated integer)",
        )
        .unwrap();
        let sql = format!("select not ({VISIBLE_RELEASE}) from releases r");
        let names = [
            "abcdefghij01234",
            "abcdefghij012345",
            "abcdefghij0123456",
            "Some.Dotted.Name.Longer",
            "short",
            "éabcdefghij012345",
        ];
        for name in names {
            for display in [None, Some("Real.Name")] {
                for flag in [false, true] {
                    conn.execute("delete from releases", []).unwrap();
                    conn.execute(
                        "insert into releases (name, display_name, group_name, is_obfuscated) values (?, ?, 'alt.binaries.t', ?)",
                        rusqlite::params![name, display, flag],
                    )
                    .unwrap();
                    let hidden: bool = conn.query_row(&sql, [], |r| r.get(0)).unwrap();
                    assert_eq!(
                        crate::category::rules::is_hidden(name, display.is_some(), flag),
                        hidden,
                        "{name} {display:?} {flag}"
                    );
                }
            }
        }
    }
}
