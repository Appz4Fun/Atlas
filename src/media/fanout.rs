//! Native fanout batching policy.

use std::time::Duration;

pub const SECOND_PASS: Duration = Duration::from_secs(60);
pub const CHUNK: i64 = 1_000;


