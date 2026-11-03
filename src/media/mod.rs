//! TV and movie IDs: each release's title, each title resolved once, and searches by ID.

pub mod details;
pub mod fanout;
pub mod http;
pub mod parse;
pub mod query;
pub mod resolve;
pub mod score;
pub mod steps;
pub mod store;
pub mod title;
pub mod tmdb;
pub mod trakt;
pub mod tvdb;
pub mod tvmaze;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::Duration;

use rusqlite::Connection;

use crate::category;
use crate::config::{Config, MediaConfig};
use crate::indexer::Db;

use self::resolve::{Ids, Provider};
use self::store::{STATE_FOUND, TitleRow};
use self::title::Kind;

pub use self::steps::{MEDIA_RELEASE, media_target};

const MAX_CLAIMS: usize = 4_000_000;
const CLAIM_DAYS: u32 = 7;

#[derive(Clone, Copy, Debug)]
struct Known {
    next_check: u32,
    checked: u8,
    found: bool,
    imdb: bool,
}

/// All of this part's state for one database.
pub struct Media {
    main: PathBuf,
    config: RwLock<MediaConfig>,
    options: RwLock<category::Options>,
    providers: RwLock<Vec<Arc<dyn Provider>>>,
    conn: Mutex<Option<Connection>>,
    open_failed: AtomicBool,
    known: RwLock<HashMap<i64, Known>>,
    claims: Mutex<HashMap<i64, (i64, u32)>>,
    titles: AtomicU64,
    max_claims: AtomicUsize,
    second_pass: Mutex<Duration>,
    fanout: Mutex<VecDeque<i64>>,
    pub titles_full: AtomicU64,
    pub claims_full: AtomicU64,
    /// writer jobs the fan-out has run
    pub fanout_jobs: AtomicU64,
}

fn build_providers(cfg: &MediaConfig) -> Vec<Arc<dyn Provider>> {
    let mut out: Vec<Arc<dyn Provider>> = Vec::new();
    if cfg.tvmaze.enabled {
        out.push(Arc::new(tvmaze::Tvmaze::new(&cfg.tvmaze)));
    }
    if !cfg.tmdb.api_key.is_empty() {
        out.push(Arc::new(tmdb::Tmdb::new(&cfg.tmdb)));
    }
    if !cfg.trakt.client_id.is_empty() {
        out.push(Arc::new(trakt::Trakt::new(&cfg.trakt)));
    }
    if !cfg.tvdb.api_key.is_empty() {
        out.push(Arc::new(tvdb::Tvdb::new(&cfg.tvdb)));
    }
    out
}

static SHARED: LazyLock<Mutex<HashMap<PathBuf, Arc<Media>>>> = LazyLock::new(Default::default);

/// For `paths::database()`, one per process.
pub fn shared() -> Arc<Media> {
    Media::for_main(&crate::paths::database())
}

/// The three steps, for `steps::all`.
pub fn steps() -> [Arc<dyn crate::postprocess::step::Step>; 3] {
    let media = shared();
    [
        Arc::new(steps::MediaParse(media.clone())),
        Arc::new(steps::MediaLookup(media.clone())),
        Arc::new(steps::MediaNfo(media)),
    ]
}

/// Fills in resolved titles' IDs, until `stopping`. Holds a `Db` clone, soo it ends before the writers do.
pub async fn run(media: Arc<Media>, db: Db, stopping: impl Fn() -> bool + Send + 'static) {
    let _ = tokio::task::spawn_blocking(move || fanout::run(&media, &db, &stopping)).await;
}

impl Media {
    /// One per database in this process: it holds the claims the steps share.
    pub fn for_main(main: &Path) -> Arc<Media> {
        let mut all = SHARED.lock().unwrap();
        all.entry(main.to_path_buf()).or_insert_with(|| Media::new(main)).clone()
    }

    /// A second one for the same file, as a restart would make.
    #[doc(hidden)]
    pub fn new(main: &Path) -> Arc<Media> {
        Arc::new(Media {
            main: main.to_path_buf(),
            config: RwLock::new(MediaConfig::default()),
            options: RwLock::new(category::Options::default()),
            providers: RwLock::new(Vec::new()),
            conn: Mutex::new(None),
            open_failed: AtomicBool::new(false),
            known: RwLock::new(HashMap::new()),
            claims: Mutex::new(HashMap::new()),
            titles: AtomicU64::new(0),
            max_claims: AtomicUsize::new(MAX_CLAIMS),
            second_pass: Mutex::new(fanout::SECOND_PASS),
            fanout: Mutex::new(VecDeque::new()),
            titles_full: AtomicU64::new(0),
            claims_full: AtomicU64::new(0),
            fanout_jobs: AtomicU64::new(0),
        })
    }

    #[doc(hidden)]
    pub fn with_max_claims(&self, max: usize) {
        self.max_claims.store(max, Relaxed);
    }

    #[doc(hidden)]
    pub fn with_second_pass(&self, wait: Duration) {
        *self.second_pass.lock().unwrap() = wait;
    }

    /// Rebuilds the providers when `media` changed, which clears their pauses.
    pub fn set_config(&self, cfg: &Config) {
        *self.options.write().unwrap() = cfg.category.options();
        let changed = {
            let mut current = self.config.write().unwrap();
            let changed = *current != cfg.media;
            *current = cfg.media.clone();
            changed
        };
        if changed || (cfg.media.any_provider() && self.providers.read().unwrap().is_empty()) {
            *self.providers.write().unwrap() = build_providers(&cfg.media);
        }
        if cfg.media.any_provider() {
            self.open();
        }
    }

    /// The file and the map, the first time a provider is on.
    fn open(&self) {
        let mut conn = self.conn.lock().unwrap();
        if conn.is_some() || self.open_failed.load(Relaxed) {
            return;
        }
        let opened = store::open(&store::path_for(&self.main)).and_then(|c| {
            let mut known = self.known.write().unwrap();
            let mut count = 0;
            store::all(&c, |row| {
                count += 1;
                known.insert(row.key, known_of(&row));
            })?;
            self.titles.store(count, Relaxed);
            Ok(c)
        });
        match opened {
            Ok(c) => *conn = Some(c),
            Err(e) => {
                self.open_failed.store(true, Relaxed);
                eprintln!("media: the title cache cant be opened: {e}");
            }
        }
    }

    pub(crate) fn with_store<T>(&self, read: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, String> {
        self.open();
        let conn = self.conn.lock().unwrap();
        let conn = conn.as_ref().ok_or_else(|| "the title cache isnt open".to_string())?;
        read(conn).map_err(|e| e.to_string())
    }

    pub(crate) fn providers(&self) -> Vec<Arc<dyn Provider>> {
        self.providers.read().unwrap().clone()
    }

    /// which providers are on, one bit each
    pub(crate) fn provider_bits(&self) -> u8 {
        self.providers.read().unwrap().iter().fold(0, |bits, p| bits | p.bit())
    }

    pub(crate) fn negative_ttl_days(&self) -> i64 {
        i64::from(self.config.read().unwrap().negative_ttl_days.max(1))
    }

    pub(crate) fn clear_claim(&self, key: i64) {
        self.claims.lock().unwrap().remove(&key);
    }

    /// `resolve` wrote this row: the map follows, and new IDs reach every release of the title.
    pub(crate) fn remember(&self, row: &TitleRow, new: bool) {
        self.known.write().unwrap().insert(row.key, known_of(row));
        if new {
            self.titles.fetch_add(1, Relaxed);
        }
        if !row.fanout_done {
            self.fanout.lock().unwrap().push_back(row.key);
        }
    }

    /// A title that no longer has these IDs: the fan-out takes them off its rows too.
    pub(crate) fn fan_out_again(&self, key: i64) -> Result<(), String> {
        self.with_store(|c| store::fanout_owed_again(c, key))?;
        self.fanout.lock().unwrap().push_back(key);
        Ok(())
    }

    pub(crate) fn next_fanout(&self) -> Option<i64> {
        self.fanout.lock().unwrap().pop_front()
    }

    pub(crate) fn second_pass_wait(&self) -> Duration {
        *self.second_pass.lock().unwrap()
    }

    pub fn category_options(&self) -> category::Options {
        *self.options.read().unwrap()
    }

    /// None with no provider on.
    pub fn cached_ids(&self, key: i64) -> Option<Ids> {
        if self.provider_bits() == 0 || !self.known.read().unwrap().get(&key).is_some_and(|k| k.found) {
            return None;
        }
        let row = self.with_store(|c| store::get(c, key)).ok().flatten()?;
        (row.state == STATE_FOUND).then_some(row.ids)
    }

    /// In-memory, false with no provider on.
    pub fn imdb_known(&self, key: i64) -> bool {
        self.provider_bits() != 0 && self.known.read().unwrap().get(&key).is_some_and(|k| k.imdb)
    }

    /// For stats.json.
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "titles": self.titles.load(Relaxed),
            "titles_full": self.titles_full.load(Relaxed),
            "claims_full": self.claims_full.load(Relaxed),
        })
    }

    /// Should this release take its title to the net lane? One release of a title goes at a time.
    pub fn wants(&self, kind: Kind, key: i64, release_id: i64, now: i64) -> bool {
        if self.open_failed.load(Relaxed) {
            return false;
        }
        let enabled =
            self.providers.read().unwrap().iter().filter(|p| p.asks(kind)).fold(0u8, |bits, p| bits | p.bit());
        if enabled == 0 {
            return false;
        }
        let at = u32::try_from(now).unwrap_or(u32::MAX);
        let known = self.known.read().unwrap().get(&key).copied();
        if let Some(k) = known
            && enabled & !k.checked == 0
            && (k.found || at < k.next_check)
        {
            return false;
        }
        let mut claims = self.claims.lock().unwrap();
        let week = CLAIM_DAYS * 86_400;
        if let Some((holder, since)) = claims.get(&key) {
            if *holder == release_id {
                return true;
            }
            if at.saturating_sub(*since) < week {
                return false;
            }
        }
        if known.is_none()
            && self.titles.load(Relaxed) >= u64::try_from(self.config.read().unwrap().max_titles).unwrap_or(0)
        {
            self.titles_full.fetch_add(1, Relaxed);
            return false;
        }
        if claims.len() >= self.max_claims.load(Relaxed) {
            claims.retain(|_, (_, since)| at.saturating_sub(*since) < week);
            if claims.len() >= self.max_claims.load(Relaxed) {
                self.claims_full.fetch_add(1, Relaxed);
                return false;
            }
        }
        claims.insert(key, (release_id, at));
        true
    }
}

fn known_of(row: &TitleRow) -> Known {
    Known {
        next_check: row.next_check.and_then(|t| u32::try_from(t).ok()).unwrap_or(0),
        checked: u8::try_from(row.checked_mask).unwrap_or(0),
        found: row.state == STATE_FOUND,
        imdb: row.ids.imdb.is_some(),
    }
}
