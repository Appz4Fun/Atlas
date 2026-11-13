//! Retained late-match policy and version-one result SQL spellings.

use std::time::Duration;

pub const LATE_DELAY_SECS: i64 = 600;
pub const LATE_FTS_LIMIT: i64 = 32;
pub const LATE_MAX_RERUNS: usize = 16;
pub const LATE_TITLE_BUDGET: Duration = Duration::from_secs(2);
pub const NAME_PROBE: &str = "select id, name_source from releases where name = ?1 limit 17";
pub const FTS_PROBE: &str = "select rowid from releases_fts where releases_fts match ?1 order by rowid desc limit ?2";
pub const ROW_READ: &str = "select name, display_name, name_source from releases where id = ?1";
pub const INNER_PROBE: &str = "select release_id from predb_inner_key where key = ?1 limit 17";
