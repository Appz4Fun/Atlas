//! The store in the indexer, behind `[header_store] enabled` (off by default): listings recorded as they're fetched,
//! the slice's cursor held until its rows are durable, the keep set refreshed every minute, and everything sealed and
//! committed at shutdown.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use super::store::{Clamp, DAY, KeepSet, Store, StoreConfig, Ticket};

static STORE: OnceLock<Mutex<Option<Arc<Store>>>> = OnceLock::new();
/// The store is open and its linker takes the place of the SQLite one (`[header_store] linker`).
static LINKS_ON: AtomicBool = AtomicBool::new(false);

/// Whether obfuscated headers go to the store's linker only (`xref::hold` keeps none in `atlas.xref.db`).
pub fn links_on() -> bool {
    LINKS_ON.load(Ordering::Relaxed)
}

/// The store the indexer records to, when it's on.
pub fn handle() -> Option<Arc<Store>> {
    store()
}

fn store() -> Option<Arc<Store>> {
    STORE.get().and_then(|m| m.lock().ok().and_then(|g| g.clone()))
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The days kept now: the live window (two days back, until cursors say where live reading stands), the days around
/// each backfill day claimed lately (the header window's claims), and the days with an hour the store's linker hasn't
/// linked (when it runs).
pub fn keep_now() -> KeepSet {
    let now = now();
    KeepSet {
        live_from: (now - 2 * DAY).div_euclid(DAY),
        claimed: crate::header_window::claimed_days(now).into_iter().collect(),
        unlinked: super::linker::unlinked_days(),
    }
}

/// Opens the store when it's on. Call once, from the indexer.
pub fn spawn(cfg: &crate::config::HeaderStoreConfig) {
    if !cfg.enabled {
        return;
    }
    let root = cfg.root();
    let (store, opened) = match Store::open(StoreConfig::new(&root), keep_now()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("header store: couldnt open {}: {e:#}; not recording", root.display());
            return;
        }
    };
    eprintln!(
        "header store: {} segments, {} blocks; {} logs replayed ({} rows), {} already committed",
        opened.segments, opened.blocks, opened.logs_replayed, opened.rows_replayed, opened.logs_already_committed
    );
    if STORE.set(Mutex::new(Some(Arc::clone(&store)))).is_err() {
        return;
    }
    LINKS_ON.store(cfg.linker, Ordering::Relaxed);
    let weak = Arc::downgrade(&store);
    let _ = std::thread::Builder::new().name("nxv-keep".into()).spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(60));
            let Some(s) = weak.upgrade() else { return };
            if s.failure().is_some() {
                return;
            }
            s.set_keep(keep_now());
        }
    });
}

/// Whether the store is recording, soo the connection layer knows to hand it listings.
pub fn enabled() -> bool {
    store().is_some()
}

/// A fetched listing of `group` from `server`. A failure is said once the store stops; the slice then fails at its
/// `sync_point`, and is read again after a restart.
pub fn record(server: &str, group: &str, lines: &[Vec<u8>]) {
    let Some(s) = store() else { return };
    if lines.is_empty() {
        return;
    }
    if let Err(e) = s.record(server, group, lines, Clamp::Unknown { now: now() }) {
        eprintln!("header store: couldnt record {group}: {e:#}");
    }
}

/// What a slice waits on before it counts as read: every row recorded for `group` so far durable. None when the
/// store is off.
pub fn sync_point(group: &str) -> Option<Ticket> {
    store()?.sync_point(group)
}

/// Seals and commits what's open, then stops. For shutdown.
pub fn finish() {
    let Some(m) = STORE.get() else { return };
    let Some(s) = m.lock().ok().and_then(|mut g| g.take()) else { return };
    if let Err(e) = s.close() {
        eprintln!("header store: {e:#}");
    }
}
