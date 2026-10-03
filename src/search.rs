use rusqlite::{Connection, OptionalExtension, Row, params_from_iter, types::Value};

use crate::db::{self, Result};

/// One search hit. `name` is the display name when we have one.
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
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

const COLUMNS: &str =
    "r.id, coalesce(r.display_name, r.name), r.group_name, r.poster, r.posted_date, r.size, r.complete, r.parts";

/// hide obfuscated junk unless we managed to get a real name for it
const VISIBLE_RELEASE: &str = "(
    r.display_name is not null
    or (r.is_obfuscated = 0 and (length(r.name) < 16 or r.name glob '*[^A-Za-z0-9]*'))
)";

fn release_row(r: &Row) -> rusqlite::Result<ReleaseRow> {
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

/// Search within one group (`Some`) or all of them (`None`). `page` is 0 based.
pub fn search(query: &str, group: Option<&str>, page: i64, page_size: i64) -> Result<Vec<ReleaseRow>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }

    let offset = page * page_size;
    let group_filter = if group.is_some() { "r.group_name = ? and " } else { "" };

    // fts table holds the text, real data lives in releases
    let fts_sql = format!(
        "with ranked as (
            select rowid, bm25(releases_fts) as rank from releases_fts where releases_fts match ?
        )
        select {COLUMNS}
        from releases r
        join ranked on ranked.rowid = r.id
        where {group_filter}{VISIBLE_RELEASE}
        order by ranked.rank
        limit ? offset ?"
    );

    let like_sql = format!(
        "select {COLUMNS}
        from releases r
        where (r.name like ? escape '\\' or r.display_name like ? escape '\\')
        and {group_filter}{VISIBLE_RELEASE}
        order by r.name
        limit ? offset ?"
    );

    let like = like_query(query);
    let mut fts_vals = vec![text(fts_query(query))];
    let mut like_vals = vec![text(like.clone()), text(like)];

    if let Some(g) = group {
        fts_vals.push(text(g));
        like_vals.push(text(g));
    }

    for vals in [&mut fts_vals, &mut like_vals] {
        vals.push(Value::Integer(page_size));
        vals.push(Value::Integer(offset));
    }

    fts_or_like_rows((&fts_sql, fts_vals), (&like_sql, like_vals))
}

pub fn count(query: &str, group: Option<&str>) -> Result<i64> {
    if query.trim().is_empty() {
        return Ok(0);
    }

    let group_filter = if group.is_some() { "r.group_name = ? and " } else { "" };

    let fts_sql = format!(
        "with matches as (
            select rowid from releases_fts where releases_fts match ?
        )
        select count(*) from releases r join matches on matches.rowid = r.id
        where {group_filter}{VISIBLE_RELEASE}"
    );

    let like_sql = format!(
        "select count(*) from releases r
        where (r.name like ? escape '\\' or r.display_name like ? escape '\\')
        and {group_filter}{VISIBLE_RELEASE}"
    );

    let like = like_query(query);
    let mut fts_vals = vec![text(fts_query(query))];
    let mut like_vals = vec![text(like.clone()), text(like)];

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
    search(query, None, page, page_size)
}

pub fn count_releases(query: &str, group: &str) -> Result<i64> {
    count(query, Some(group))
}

pub fn count_all_releases(query: &str) -> Result<i64> {
    count(query, None)
}

pub fn all_releases(page: i64, page_size: i64) -> Result<Vec<ReleaseRow>> {
    let conn = db::open()?;
    let sql = format!("select {COLUMNS} from releases r where {VISIBLE_RELEASE} order by r.id desc limit ? offset ?");
    query_rows(&conn, &sql, &[Value::Integer(page_size), Value::Integer(page * page_size)])
}

pub fn search_obfuscated(page: i64, page_size: i64) -> Result<Vec<ReleaseRow>> {
    let conn = db::open()?;
    let sql = format!("select {COLUMNS} from releases r where r.is_obfuscated = 1 order by r.id desc limit ? offset ?");
    query_rows(&conn, &sql, &[Value::Integer(page_size), Value::Integer(page * page_size)])
}

pub fn count_obfuscated() -> Result<i64> {
    db::open()?.query_row("select count(*) from releases where is_obfuscated = 1", [], |r| r.get(0))
}

pub fn get_release(id: i64) -> Result<Option<ReleaseRow>> {
    let conn = db::open()?;
    let sql = format!("select {COLUMNS} from releases r where r.id = ?");
    conn.query_row(&sql, [id], release_row).optional()
}

pub fn recent_in_groups(groups: &[String], page: i64, page_size: i64) -> Result<Vec<ReleaseRow>> {
    if groups.is_empty() {
        return Ok(Vec::new());
    }

    let conn = db::open()?;
    let qs = vec!["?"; groups.len()].join(",");
    let sql =
        format!("select {COLUMNS} from releases r where r.group_name in ({qs}) order by r.id desc limit ? offset ?");

    let mut vals: Vec<Value> = groups.iter().map(|g| text(g.as_str())).collect();
    vals.push(Value::Integer(page_size));
    vals.push(Value::Integer(page * page_size));

    query_rows(&conn, &sql, &vals)
}

/// Articles ordered by filename then part soo files can be reassembled.
pub fn get_articles(release_id: i64) -> Result<Vec<ArticleRow>> {
    let conn = db::open()?;
    let mut stmt = conn.prepare(
        "select articles.message_id, articles.filename,
            articles.part, articles.total_parts, articles.bytes,
            articles.subject, releases.poster, releases.posted_date
         from articles join releases on articles.release_id = releases.id
         where articles.release_id = ?
         order by articles.filename, articles.part",
    )?;

    stmt.query_map([release_id], |r| {
        Ok(ArticleRow {
            message_id: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
            filename: r.get(1)?,
            part: r.get(2)?,
            total_parts: r.get(3)?,
            bytes: r.get(4)?,
            subject: r.get(5)?,
            poster: r.get(6)?,
            posted_date: r.get(7)?,
        })
    })?
    .collect()
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
}
