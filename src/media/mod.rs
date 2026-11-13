//! TV and movie IDs: each release's title, each title resolved once, and searches by ID.

pub mod allocine;
mod cache;
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

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex, RwLock};

use crate::category;
use crate::config::{Config, MediaConfig};

use self::resolve::{Ids, Provider};
use self::store::{STATE_FOUND, TitleRow};
use self::title::Kind;

pub use self::steps::{MEDIA_RELEASE, media_target};

const MAX_CLAIMS: usize = 4_000_000;

/// All of this part's state for one database.
pub struct Media {
    cache: Arc<crate::pg::service_cache::Cache>,
    config: RwLock<MediaConfig>,
    options: RwLock<category::Options>,
    providers: RwLock<Vec<Arc<dyn Provider>>>,
    allocine: RwLock<Option<Arc<allocine::Allocine>>>,
    claims: Mutex<HashMap<i64, (i64, u32)>>,
    titles: AtomicU64,
    max_claims: AtomicUsize,
    pub titles_full: AtomicU64,
    pub claims_full: AtomicU64,
    /// writer jobs the fan-out has run
    pub fanout_jobs: AtomicU64,
}

pub(crate) struct NativeClaim<'a> {
    media: &'a Media,
    key: i64,
    holder: (i64, u32),
}
impl Drop for NativeClaim<'_> {
    fn drop(&mut self) {
        let mut claims = self.media.claims.lock().unwrap();
        if claims.get(&self.key) == Some(&self.holder) {
            claims.remove(&self.key);
        }
    }
}

/// The real configured media steps for one owned PostgreSQL attempt. No shared
/// legacy service or SQLite connection is constructed. All three share this
/// native cache service and its provider pacing/claims.
pub(crate) fn native_steps(
    owner: &Arc<crate::pg::indexer_runtime::Owner>,
    cfg: &Config,
) -> anyhow::Result<[Arc<dyn crate::postprocess::step::Step>; 3]> {
    let media = native_service(owner, cfg)?;
    Ok([
        Arc::new(steps::MediaParse(media.clone())),
        Arc::new(steps::MediaLookup(media.clone())),
        Arc::new(steps::MediaNfo(media)),
    ])
}

fn native_service(owner: &Arc<crate::pg::indexer_runtime::Owner>, cfg: &Config) -> anyhow::Result<Arc<Media>> {
    let cache = Arc::new(crate::pg::service_cache::Cache::new(owner)?);
    let count = cache.title_count()?;
    let media = Media::with_cache(cache);
    media.titles.store(count, Relaxed);
    media.configure(cfg);
    Ok(media)
}

pub(crate) fn native_fanout(owner: &Arc<crate::pg::indexer_runtime::Owner>) -> anyhow::Result<bool> {
    crate::pg::service_cache::Cache::new(owner)?.fanout_batch()
}

/// Called inside the original Stage settlement transaction. No owner lookup,
/// nested mutex, connection, provider, or deferred Rust callback is involved.
pub(crate) fn native_cached_ids_in(
    client: &mut impl postgres::GenericClient,
    kind: Kind,
    key: i64,
    parsed: &parse::Parsed,
) -> anyhow::Result<Ids> {
    anyhow::ensure!(key == steps::key_of(kind, parsed), "media title key does not bind the parsed title");
    let Some(row) = crate::pg::service_cache::title_in(client, key)? else { return Ok(Ids::default()) };
    anyhow::ensure!(row.kind == kind as i64, "media cache kind collision");
    let (expected, _) = title::split_country(&parsed.title);
    if title::normalize(&row.title) != title::normalize(expected) {
        // Old AlloCiné rows store the decoded query rather than the original
        // AC identifier. An identifier alone cannot prove that alias here;
        // Lookup re-establishes it through the real cached/provider decoder.
        if allocine::allocine_id(expected).is_some() {
            return Ok(Ids::default());
        }
        anyhow::bail!("media cache title collision");
    }
    anyhow::ensure!(row.year == parsed.year, "media cache year collision");
    Ok(if row.state == STATE_FOUND { row.ids } else { Ids::default() })
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

impl Media {
    fn with_cache(cache: Arc<crate::pg::service_cache::Cache>) -> Arc<Media> {
        Arc::new(Media {
            cache,
            config: RwLock::new(MediaConfig::default()),
            options: RwLock::new(category::Options::default()),
            providers: RwLock::new(Vec::new()),
            allocine: RwLock::new(None),
            claims: Mutex::new(HashMap::new()),
            titles: AtomicU64::new(0),
            max_claims: AtomicUsize::new(MAX_CLAIMS),
            titles_full: AtomicU64::new(0),
            claims_full: AtomicU64::new(0),
            fanout_jobs: AtomicU64::new(0),
        })
    }

    #[doc(hidden)]
    pub fn with_max_claims(&self, max: usize) {
        self.max_claims.store(max, Relaxed);
    }

    pub(crate) fn cache_step_error(&self, error: anyhow::Error) -> crate::postprocess::step::StepError {
        crate::postprocess::step::StepError::from_cache(error)
    }

    pub(crate) fn claim_native(&self, key: i64, id: i64, now: i64) -> Option<NativeClaim<'_>> {
        let holder = (id, u32::try_from(now).unwrap_or(u32::MAX));
        let mut claims = self.claims.lock().unwrap();
        if claims.contains_key(&key) || claims.len() >= self.max_claims.load(Relaxed) {
            return None;
        }
        claims.insert(key, holder);
        Some(NativeClaim { media: self, key, holder })
    }

    pub(crate) fn title_get_checked(&self, key: i64) -> anyhow::Result<Option<TitleRow>> {
        self.cache.title(key)
    }

    pub(crate) fn title_put_checked(&self, row: &TitleRow) -> anyhow::Result<()> {
        self.cache.put_title(row)
    }

    pub(crate) fn fan_out_again_checked(&self, key: i64) -> anyhow::Result<()> {
        self.cache.fanout_owed(key)
    }

    pub(crate) fn imdb_known_checked(&self, key: i64) -> anyhow::Result<bool> {
        Ok(self.title_get_checked(key)?.is_some_and(|row| row.state == STATE_FOUND && row.ids.imdb.is_some()))
    }

    pub(crate) fn configure(&self, cfg: &Config) {
        cache::native(self.cache.clone(), || self.configure_providers(cfg))
    }

    fn configure_providers(&self, cfg: &Config) {
        *self.options.write().unwrap() = cfg.category.options();
        let changed = {
            let mut current = self.config.write().unwrap();
            let changed = *current != cfg.media;
            *current = cfg.media.clone();
            changed
        };
        if changed || (cfg.media.any_provider() && self.providers.read().unwrap().is_empty()) {
            *self.providers.write().unwrap() = build_providers(&cfg.media);
            *self.allocine.write().unwrap() =
                cfg.media.allocine.enabled.then(|| Arc::new(allocine::Allocine::new(&cfg.media.allocine)));
        }
    }

    /// What reads `AC<id>` titles, while it's on.
    pub fn allocine(&self) -> Option<Arc<allocine::Allocine>> {
        self.allocine.read().unwrap().clone()
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

    pub(crate) fn remember(&self, _row: &TitleRow, new: bool) {
        if new {
            self.titles.fetch_add(1, Relaxed);
        }
    }

    pub fn category_options(&self) -> category::Options {
        *self.options.read().unwrap()
    }

    /// For stats.json.
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "titles": self.titles.load(Relaxed),
            "titles_full": self.titles_full.load(Relaxed),
            "claims_full": self.claims_full.load(Relaxed),
        })
    }
}

#[cfg(test)]
mod native_cache_tests;
