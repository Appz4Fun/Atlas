//! Shared native search result shapes, filter budgets, and literal query helpers.

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

const COMMON_MATCHES: i64 = 2_000;

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
    /// matches of a word's exact form past which it's too common to rank
    pub common: i64,
}

impl Bounds {
    pub const DEFAULT: Bounds =
        Bounds { rejects: 2_000, no_row: 20_000, fts: 20_000, like: 100_000, common: COMMON_MATCHES };
}

pub const MAX_FILTER_OFFSET: i64 = 10_000;

pub struct CatPage {
    pub rows: Vec<ReleaseRow>,
    pub scanned: usize,
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

#[cfg(test)]
#[path = "search/native_sort_transfer_tests.rs"]
mod native_sort_transfer_tests;
