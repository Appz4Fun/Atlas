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

/// Opens bounded live capture when enabled. Call once, before the native linker.
/// Legacy metadata is imported by that worker; startup never authorizes expiry.
pub fn spawn(cfg: &crate::config::HeaderStoreConfig) -> anyhow::Result<()> {
    if !cfg.enabled {
        return Ok(());
    }
    let _ = STARTED.set(Instant::now());
    let root = cfg.root();
    let state_dir: PathBuf = root.join("_state");
    load_claimed(&state_dir);
    use anyhow::Context;
    let (store, opened) = Store::open(StoreConfig { bounded_maps: true, ..StoreConfig::new(&root) }, keep_now())
        .with_context(|| format!("opening header store {}", root.display()))?;
    eprintln!(
        "header store: {} segments, {} blocks; {} logs replayed ({} rows), {} already committed",
        opened.segments, opened.blocks, opened.logs_replayed, opened.rows_replayed, opened.logs_already_committed
    );
    anyhow::ensure!(STORE.set(Mutex::new(Some(Arc::clone(&store)))).is_ok(), "header store already started");
    // housekeeping and refused rows ask for the keep set as of then
    store.set_keep_source(Arc::new(keep_now));
    // Keep every unprocessed day until the native linker establishes exact
    // durable coverage. Unreadable legacy state must never grant age expiry.
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
    Ok(())
}

pub(crate) fn spawn_bounded(cfg: &crate::config::HeaderStoreConfig) -> anyhow::Result<()> {
    spawn(cfg)
}

/// One admitted listing, held through NNTP reads and any detached disk work.
// Deliberately non-Clone with private admission state. finish consumes the
// only public recording capability; borrowed record futures must end first.
pub struct Fetch(Option<(Arc<Store>, Arc<super::store::FetchRequest>)>);

impl Fetch {
    pub async fn begin() -> anyhow::Result<Self> {
        let Some(store) = store() else { return Ok(Self(None)) };
        Self::begin_on(store).await
    }

    pub(crate) async fn begin_on(store: Arc<Store>) -> anyhow::Result<Self> {
        let request = store.fetch_request().await?;
        Ok(Self(Some((store, request))))
    }

    pub fn enabled(&self) -> bool {
        self.0.is_some()
    }

    /// Keep a fetched listing, waiting for byte capacity before copying it into the store.
    pub async fn record(&self, server: &str, group: &str, lines: Vec<Vec<u8>>) -> anyhow::Result<Vec<Vec<u8>>> {
        let Some((store, fetch)) = &self.0 else { return Ok(lines) };
        if lines.is_empty() {
            return Ok(lines);
        }
        let now = now();
        let (lines, ticket) =
            store.record_live_for_fetch(server, group, lines, Clamp::Unknown { now }, Arc::clone(fetch)).await?;
        refresh_claimed(&ticket.days, now);
        Ok(lines)
    }

    /// Close admission and join only this request's admitted capture jobs and
    /// exact acknowledgment. This await is never canceled by production stop.
    pub async fn finish(self)->anyhow::Result<()> {
        if let Some((store,request))=self.0 {request.finish(store).await?;}Ok(())
    }
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

    const CHILD_ROOT: &str = "ATLAS_TEST_LIVE_STARTUP_ROOT";
    const CHILD_MODE: &str = "ATLAS_TEST_LIVE_STARTUP_MODE";
    const CHILD_CASE: &str = "ATLAS_TEST_LIVE_STARTUP_CASE";
    const SPARSE_IDS: u64 = 16 * 1024 * 1024;

    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn child_fixture() -> Option<(PathBuf, String, String)> {
        std::env::var_os(CHILD_ROOT)
            .map(|root| (PathBuf::from(root), std::env::var(CHILD_MODE).unwrap(), std::env::var(CHILD_CASE).unwrap()))
    }

    fn start(root: &Path, mode: &str, enabled: bool) -> anyhow::Result<()> {
        let cfg = crate::config::HeaderStoreConfig { enabled, dir: root.to_string_lossy().into_owned() };
        match mode {
            "public" => spawn(&cfg),
            "bounded" => spawn_bounded(&cfg),
            _ => panic!("unknown isolated live startup fixture"),
        }
    }

    fn isolated(selector: &str, mode: &str, case: &str, prepare: impl FnOnce(&Path)) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("_state")).unwrap();
        std::fs::write(dir.path().join("_state/claimed-days"), b"").unwrap();
        prepare(dir.path());
        let mut child = Child(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", selector, "--nocapture"])
                .env("ATLAS_HOME", dir.path())
                .env(CHILD_ROOT, dir.path())
                .env(CHILD_MODE, mode)
                .env(CHILD_CASE, case)
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success(), "isolated {mode}/{case} startup contract failed");
                return;
            }
            assert!(Instant::now() < deadline, "isolated {mode}/{case} startup did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn legacy_fifo(root: &Path, name: &str) {
        let state = root.join("_state");
        std::fs::create_dir_all(&state).unwrap();
        for file in ["links.rec", "linked-ids.rec", "linker.rec"] {
            if file == name {
                let path = std::ffi::CString::new(state.join(file).as_os_str().as_encoded_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            } else if file == "linked-ids.rec" {
                std::fs::File::create(state.join(file)).unwrap().set_len(SPARSE_IDS).unwrap();
            } else {
                std::fs::write(state.join(file), b"unchanged legacy startup sentinel\n").unwrap();
            }
        }
    }

    fn metadata_startup(selector: &str, mode: &str) {
        if let Some((root, mode, fifo)) = child_fixture() {
            start(&root, &mode, true).unwrap();
            assert!(handle().is_some(), "actual durable capture must start");
            finish();
            assert!(handle().is_none());
            for file in ["links.rec", "linked-ids.rec", "linker.rec"] {
                if file == fifo {
                    continue;
                }
                let path = root.join("_state").join(file);
                if file == "linked-ids.rec" {
                    assert_eq!(std::fs::metadata(path).unwrap().len(), SPARSE_IDS);
                } else {
                    assert_eq!(std::fs::read(path).unwrap(), b"unchanged legacy startup sentinel\n");
                }
            }
            return;
        }
        // A separate FIFO for every legacy metadata reader catches any eager
        // open, without allocating a large map or reading the sparse ID file.
        for file in ["links.rec", "linked-ids.rec", "linker.rec"] {
            isolated(selector, mode, file, |root| legacy_fifo(root, file));
        }
    }

    #[test]
    fn bounded_spawn_never_opens_legacy_linker_metadata() {
        metadata_startup("nxv_store::live::tests::bounded_spawn_never_opens_legacy_linker_metadata", "bounded");
    }

    #[test]
    fn public_spawn_never_opens_legacy_linker_metadata() {
        metadata_startup("nxv_store::live::tests::public_spawn_never_opens_legacy_linker_metadata", "public");
    }

    fn aged_input(root: &Path) {
        let (store, _) = Store::open(
            StoreConfig { bounded_maps: true, sealers: 1, ..StoreConfig::new(root) },
            KeepSet { live_from: i64::MIN / 2, ..KeepSet::default() },
        )
        .unwrap();
        let row = b"1\tpart\tposter@example.invalid\tFri, 02 Jan 1970 03:30:00 +0000\t<part@example.invalid>\t\t100\t1"
            .to_vec();
        store.record("server.example", "group.example", &[row], Clamp::Unknown { now: 100000 }).unwrap();
        store.close().unwrap();
        assert_eq!(store.segment_snapshot("group.example", 1).unwrap().unwrap().count, 1);
    }

    #[test]
    fn failed_legacy_state_never_authorizes_live_input_expiry() {
        if let Some((root, mode, case)) = child_fixture() {
            let state = root.join("_state");
            if case == "unreadable-links" {
                let error = super::super::linker::Linker::open_in(&state, now()).err().unwrap();
                assert!(
                    error.chain().any(|e| e.downcast_ref::<std::io::Error>().is_some()),
                    "fixture must exercise a real legacy metadata I/O error"
                );
            } else {
                assert!(super::super::linker::State::load(&state).unwrap_err().to_string().contains("cut short"));
            }
            start(&root, &mode, true).unwrap();
            let store = handle().unwrap();
            // Remove grace/claims as possible explanations for retention. No
            // healthy PostgreSQL coverage has been established for this input.
            store.set_keep_source(Arc::new(|| KeepSet { live_from: i64::MAX / 2, ..KeepSet::default() }));
            assert_eq!(store.segment_snapshot("group.example", 1).unwrap().unwrap().count, 1);
            for _ in 0..3 {
                store.housekeeping();
            }
            assert_eq!(
                store.segment_snapshot("group.example", 1).unwrap().unwrap().count,
                1,
                "uncertain linker state cannot authorize deletion after the keep window"
            );
            assert!(
                store.blocks("group.example", i64::MIN, i64::MAX).is_empty(),
                "live startup must not expose a retained whole-arena mapping"
            );
            assert_eq!(std::fs::read(state.join("linker.rec")).unwrap(), b"cut short legacy state\n");
            finish();
            drop(store);
            return;
        }
        for mode in ["public", "bounded"] {
            for case in ["unreadable-links", "cut-short-state"] {
                isolated(
                    "nxv_store::live::tests::failed_legacy_state_never_authorizes_live_input_expiry",
                    mode,
                    case,
                    |root| {
                        aged_input(root);
                        let state = root.join("_state");
                        std::fs::create_dir_all(&state).unwrap();
                        std::fs::write(state.join("linker.rec"), b"cut short legacy state\n").unwrap();
                        if case == "unreadable-links" {
                            std::fs::create_dir(state.join("links.rec")).unwrap();
                        }
                    },
                );
            }
        }
    }

    #[test]
    fn live_startup_propagates_store_errors_without_publishing_a_handle() {
        if let Some((root, mode, _)) = child_fixture() {
            let blocked = root.join("not-a-directory");
            let error = start(&blocked, &mode, true).unwrap_err();
            assert!(
                error.chain().any(|e| e.downcast_ref::<std::io::Error>().is_some()),
                "startup must preserve the actual store I/O cause"
            );
            assert!(handle().is_none(), "failed startup cannot publish a ready handle");
            return;
        }
        for mode in ["public", "bounded"] {
            isolated(
                "nxv_store::live::tests::live_startup_propagates_store_errors_without_publishing_a_handle",
                mode,
                "invalid-root",
                |root| std::fs::write(root.join("not-a-directory"), b"source fixture").unwrap(),
            );
        }
    }

    #[test]
    fn disabled_live_startup_does_not_create_or_publish_a_store() {
        if let Some((root, mode, _)) = child_fixture() {
            let unopened = root.join("disabled");
            start(&unopened, &mode, false).unwrap();
            assert!(!unopened.exists());
            assert!(handle().is_none());
            assert!(STARTED.get().is_none());
            return;
        }
        for mode in ["public", "bounded"] {
            isolated(
                "nxv_store::live::tests::disabled_live_startup_does_not_create_or_publish_a_store",
                mode,
                "disabled",
                |_| {},
            );
        }
    }

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
