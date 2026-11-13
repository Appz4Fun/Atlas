//! Native late-match policy limits.

use std::time::Duration;

pub const LATE_DELAY_SECS: i64 = 600;
pub const LATE_FTS_LIMIT: i64 = 32;
pub const LATE_MAX_RERUNS: usize = 16;
pub const LATE_TITLE_BUDGET: Duration = Duration::from_secs(2);
