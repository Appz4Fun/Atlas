//! The store in the indexer, behind `[header_store] enabled` (off by default): listings recorded as they're fetched,
//! the slice's cursor held until its rows are durable, the keep set refreshed every minute, the store's linker run
//! (`linker::run`, started by the indexer), and everything sealed and committed at shutdown.
//!
//! The keep set's backfill part is the days claimed lately: a backfill day claimed (`chunks::claim`,
//! `chunks::claim_turn`, by any host, wanted days included) is kept, with a day either side, while it's being read, and
//! for `CLAIMED_KEEP` after its last claim or its last batch. The claims are kept in the store's `_state/claimed-days`
//! across restarts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::store::{Clamp, DAY, KeepSet, Store, StoreConfig, Ticket};

static STORE: OnceLock<Mutex<Option<Arc<Store>>>> = OnceLock::new();

/// How long a backfill day stays kept after a claim of it was last made (wall time).
pub const CLAIMED_KEEP: Duration = Duration::from_secs(6 * 3600);
/// After a start, how long every day is kept: the claims of the days being read come again first.
pub const START_GRACE: Duration = Duration::from_secs(600);
const CLAIMED_FILE: &str = "claimed-days";

/// The backfill days claimed lately: unix day, when last claimed (unix seconds).
static CLAIMED: Mutex<BTreeMap<i64, i64>> = Mutex::new(BTreeMap::new());
static STARTED: OnceLock<Instant> = OnceLock::new();

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

/// A backfill day claimed (`chunks::claim`, `chunks::claim_turn`), to be kept around.
pub fn note_claimed_day(day: i64) {
    if let Ok(mut m) = CLAIMED.lock() {
        m.insert(day, now());
    }
    // the store keeps it at once, not at the next refresh: its rows are about to come
    if let Some(s) = store() {
        s.add_claimed(day);
    }
}

/// A slice was just recorded, `days` its posting days and how many rows of each: a claimed day that most of the slice
/// falls in or beside is still being read, however long its chunk takes, soo it stays kept. A stray date or two in
/// another slice doesn't keep a day read and gone. A timestamp moves only when minutes old.
fn refresh_claimed(days: &[(i64, usize)], now: i64) {
    let total: usize = days.iter().map(|(_, n)| n).sum();
    if total == 0 {
        return;
    }
    let Ok(mut m) = CLAIMED.lock() else { return };
    for (claimed, at) in m.iter_mut() {
        let near: usize = days.iter().filter(|(d, _)| (d - claimed).abs() <= 1).map(|(_, n)| n).sum();
        if near * 2 > total && now - *at >= 300 {
            *at = now;
        }
    }
}

/// The days claimed within `CLAIMED_KEEP` of `now`; older ones are forgotten.
pub fn claimed_days(now: i64) -> Vec<i64> {
    let keep = CLAIMED_KEEP.as_secs() as i64;
    let Ok(mut m) = CLAIMED.lock() else { return Vec::new() };
    m.retain(|_, at| now - *at <= keep);
    m.keys().copied().collect()
}

/// `day at` lines into `CLAIMED` (the later claim of a day wins).
fn load_claimed_text(text: &str) {
    let Ok(mut m) = CLAIMED.lock() else { return };
    for line in text.lines() {
        if let Some((d, at)) = line.split_once(' ')
            && let (Ok(d), Ok(at)) = (d.parse::<i64>(), at.parse::<i64>())
        {
            let e = m.entry(d).or_insert(at);
            *e = (*e).max(at);
        }
    }
}

/// The claims kept at `dir` back in `CLAIMED`: the store's own file, else (once, after the header window went) the
/// one the window kept beside the databases.
fn load_claimed(state_dir: &Path) {
    let own = state_dir.join(CLAIMED_FILE);
    let text = std::fs::read_to_string(&own)
        .or_else(|_| std::fs::read_to_string(crate::paths::app_dir().join("header-window").join(CLAIMED_FILE)));
    if let Ok(text) = text {
        load_claimed_text(&text);
    }
}

/// The claims into the store's file, when they changed since `last` (what was written then). Returns what's there now.
fn save_claimed(path: &Path, last: &str) -> std::io::Result<String> {
    let text: String =
        CLAIMED.lock().map(|m| m.iter().map(|(d, at)| format!("{d} {at}\n")).collect()).unwrap_or_default();
    if text != last {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text.as_bytes())?;
        std::fs::rename(&tmp, path)?;
    }
    Ok(text)
}

/// The days kept now: the live window (two days back, until cursors say where live reading stands), the days around
/// each backfill day claimed lately, and the days with an hour the store's linker hasn't linked. Just after a start,
/// every day (`START_GRACE`).
pub fn keep_now() -> KeepSet {
    let now = now();
    let in_grace = STARTED.get().is_none_or(|t| t.elapsed() < START_GRACE);
    KeepSet {
        live_from: if in_grace { i64::MIN / 2 } else { (now - 2 * DAY).div_euclid(DAY) },
        claimed: claimed_days(now).into_iter().collect(),
        unlinked: super::linker::unlinked_days(),
    }
}

/// Opens the store when it's on. Call once, from the indexer; it starts the store's linker after.
pub fn spawn(cfg: &crate::config::HeaderStoreConfig) {
    if !cfg.enabled {
        return;
    }
    let _ = STARTED.set(Instant::now());
    let root = cfg.root();
    let state_dir: PathBuf = root.join("_state");
    load_claimed(&state_dir);
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
    // housekeeping and refused rows ask for the keep set as of then
    store.set_keep_source(Arc::new(keep_now));
    // the store's linker arms expiry after its first pass; with a linker that can't read its state, nothing would, soo
    // expiry follows the keep set from the start
    if let Err(e) = super::linker::Linker::open(&store, now()) {
        eprintln!("header store: its linker couldnt read its state ({e:#}); expiry follows the keep set alone");
        store.arm_expiry();
    }
    let weak = Arc::downgrade(&store);
    let claimed_path = state_dir.join(CLAIMED_FILE);
    let _ = std::thread::Builder::new().name("nxv-keep".into()).spawn(move || {
        let mut saved = String::new();
        loop {
            std::thread::sleep(Duration::from_secs(60));
            let Some(s) = weak.upgrade() else { return };
            if let Some(e) = s.failure() {
                // fail-stop: nothing more is written, and a restart runs recovery (launchd starts the indexer again
                // after a non-zero exit)
                eprintln!(
                    "header store: stopped after a write error ({e}); exiting soo recovery runs at the next start"
                );
                std::process::exit(70);
            }
            match save_claimed(&claimed_path, &saved) {
                Ok(t) => saved = t,
                Err(e) => eprintln!("header store: couldnt keep the claimed days: {e}"),
            }
        }
    });
}

/// Whether the store is recording, soo the connection layer knows to hand it listings.
pub fn enabled() -> bool {
    store().is_some()
}

pub async fn wait_for_fetch() -> anyhow::Result<()> {
    if let Some(s) = store() {
        s.wait_for_fetch().await?;
    }
    Ok(())
}

/// Keep a fetched listing, waiting for byte capacity before copying it into the store.
pub async fn record(server: &str, group: &str, lines: &[Vec<u8>]) -> anyhow::Result<()> {
    let Some(s) = store() else { return Ok(()) };
    if lines.is_empty() {
        return Ok(());
    }
    let now = now();
    let t = s.record_live(server, group, lines, Clamp::Unknown { now }).await?;
    refresh_claimed(&t.days, now);
    Ok(())
}

/// What a slice waits on before it counts as read: every row recorded for `group` so far durable. None when the
/// store is off.
pub fn sync_point(group: &str) -> anyhow::Result<Option<Ticket>> {
    let Some(s) = store() else { return Ok(None) };
    if let Some(e) = s.failure() {
        anyhow::bail!("the header store stopped: {e}");
    }
    Ok(s.sync_point(group))
}

/// Seals and commits what's open, then stops. For shutdown.
pub fn finish() {
    let Some(m) = STORE.get() else { return };
    let Some(s) = m.lock().ok().and_then(|mut g| g.take()) else { return };
    if let Err(e) = s.close() {
        eprintln!("header store: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claimed_days_stay_while_read_and_come_back_from_their_file() {
        let now: i64 = 1_794_150_000;
        let day = now.div_euclid(DAY) - 20;
        CLAIMED.lock().unwrap().insert(day, now - CLAIMED_KEEP.as_secs() as i64 + 60);
        // a slice mostly on and beside the claimed day refreshes it
        refresh_claimed(&[(day, 90), (day + 1, 5), (day + 9, 5)], now);
        assert!(claimed_days(now + 3600).contains(&day), "refreshed: kept on");
        // stray rows don't
        let other = day - 5;
        CLAIMED.lock().unwrap().insert(other, now - 400);
        refresh_claimed(&[(day, 100), (other, 1)], now);
        assert_eq!(CLAIMED.lock().unwrap().get(&other), Some(&(now - 400)));
        // written and read back
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CLAIMED_FILE);
        let text = save_claimed(&path, "").unwrap();
        assert!(text.contains(&format!("{day} {now}")));
        CLAIMED.lock().unwrap().clear();
        load_claimed(dir.path());
        assert_eq!(CLAIMED.lock().unwrap().get(&day), Some(&now));
        // past their keep, forgotten
        assert!(!claimed_days(now + CLAIMED_KEEP.as_secs() as i64 + 1).contains(&day));
    }
}
