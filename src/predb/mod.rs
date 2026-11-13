//! Native PreDB feed formats, title evidence and configured test fetch injection.

pub mod feeds;
pub mod file_import;
pub mod import;
pub mod keys;
pub mod late;
pub mod matcher;
pub mod more_feeds;
pub mod poll;
pub mod step;

use self::poll::Fetch;
use std::sync::{Arc, Mutex};

static TEST_FETCH: Mutex<Option<Arc<dyn Fetch>>> = Mutex::new(None);

/// For a test that runs the real indexer: `run_servers` builds the service, soo this is the only way in.
/// Install it before `run_until`.
#[doc(hidden)]
pub fn set_test_fetch(fetch: Arc<dyn Fetch>) {
    *TEST_FETCH.lock().unwrap() = Some(fetch);
}

pub(crate) fn test_fetch() -> Option<Arc<dyn Fetch>> {
    TEST_FETCH.lock().unwrap().clone()
}
