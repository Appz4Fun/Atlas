//! Native media title cache records and lookup states.

use super::resolve::Ids;


pub const STATE_ASKING: i64 = 0;
pub const STATE_FOUND: i64 = 1;
pub const STATE_NOT_FOUND: i64 = 2;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TitleRow {
    pub key: i64,
    pub kind: i64,
    pub title: String,
    pub year: Option<i32>,
    pub state: i64,
    pub checked_mask: i64,
    pub ids: Ids,
    pub found_title: Option<String>,
    pub checked_at: i64,
    pub next_check: Option<i64>,
    pub misses: i64,
    pub fanout_done: bool,
}

