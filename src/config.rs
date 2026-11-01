use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;

use serde_json::{Map, Value, json};

use crate::atomic::write_atomic;
use crate::paths::config_file;
use std::path::{Path, PathBuf};

pub const SERVICE: &str = "atlas";
pub const DEFAULT_API_PORT: u16 = 9090;
pub const DEFAULT_API_HOST: &str = "127.0.0.1";
/// requests in flight per server (indexer and sabnzbd) when config.json doesnt say
pub const DEFAULT_CONNECTIONS: u32 = 10;
/// servers without a priority go after the ones that have one
pub const DEFAULT_PRIORITY: i64 = 99;
/// article numbers the indexer takes on per pass over a group (50 requests)
pub const DEFAULT_BATCH_SIZE: u64 = 500_000;
/// with `parallel_groups` unset, one group runs per this many connections
pub const CONNECTIONS_PER_GROUP: usize = 5;
/// sanity cap on groups indexed at once
pub const MAX_PARALLEL_GROUPS: usize = 256;
/// article numbers per XOVER request (one connection's slice of a batch).
/// providers spend most of a request's time on their side, not the network:
/// measured on old articles, 10k per request got 3-6x the headers/s of 1k on
/// one connection, and 50k was no better on most servers and erratic
pub const DEFAULT_REQUEST_SIZE: u64 = 10_000;
/// headers fetched and not saved yet, over every group at once, when
/// config.json doesnt say (about 0.3GB of headers)
pub const DEFAULT_MAX_UNSAVED_HEADERS: u64 = 500_000;
/// article numbers of backfill left on a group's home server before its
/// backfill is split into day chunks over every server that carries it
pub const SPLIT_MIN_BACKLOG: i64 = 10_000_000;

/// One usenet provider. Lower `priority` is tried first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsenetServer {
    pub host: String,
    pub username: String,
    pub password: String,
    pub port: u16,
    /// None = guess from the port (563 ssl, 119 plain)
    pub ssl: Option<bool>,
    pub connections: Option<u32>,
    pub priority: i64,
    /// ask for gzip compressed header listings, default on
    pub compress: Option<bool>,
    /// take part in indexing (header downloads), default on. off keeps e.g. a
    /// block account for downloads and article lookups only
    pub index: Option<bool>,
    /// set apart in stored state (cursors, chunk claims, sweeps) from other
    /// accounts on the same host and port, see `nntp::server_keys`. Only
    /// for a second account on a provider that numbers articles differently
    pub key: Option<String>,
}

impl UsenetServer {
    pub fn new(host: &str, username: &str, password: &str, port: u16) -> Self {
        UsenetServer {
            host: host.trim().to_string(),
            username: username.to_string(),
            password: password.to_string(),
            port,
            ssl: None,
            connections: None,
            priority: 1,
            compress: None,
            index: None,
            key: None,
        }
    }

    pub fn use_ssl(&self) -> bool {
        self.ssl.unwrap_or_else(|| crate::nntp::detect_use_ssl(&self.host, self.port))
    }

    pub fn connections(&self) -> u32 {
        self.connections.unwrap_or(DEFAULT_CONNECTIONS)
    }

    pub fn indexes(&self) -> bool {
        self.index.unwrap_or(true)
    }

    fn from_value(v: &Value) -> Option<UsenetServer> {
        let s = |key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        let host = s("host").trim().to_string();

        if host.is_empty() {
            return None;
        }

        Some(UsenetServer {
            host,
            username: s("username"),
            password: s("password"),
            port: v.get("port").and_then(as_port).unwrap_or(563),
            ssl: v.get("ssl").and_then(as_bool),
            connections: v.get("connections").and_then(as_int).and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0),
            priority: v.get("priority").and_then(as_int).unwrap_or(DEFAULT_PRIORITY),
            compress: v.get("compress").and_then(as_bool),
            index: v.get("index").and_then(as_bool),
            key: v.get("key").and_then(Value::as_str).map(str::trim).filter(|k| !k.is_empty()).map(String::from),
        })
    }

    fn to_value(&self, password: bool) -> Value {
        let mut m = Map::new();
        m.insert("host".into(), json!(self.host));
        m.insert("username".into(), json!(self.username));
        m.insert("password".into(), json!(if password { self.password.as_str() } else { "" }));
        m.insert("port".into(), json!(self.port));
        if let Some(ssl) = self.ssl {
            m.insert("ssl".into(), json!(ssl));
        }
        if let Some(c) = self.connections {
            m.insert("connections".into(), json!(c));
        }
        m.insert("priority".into(), json!(self.priority));
        if let Some(c) = self.compress {
            m.insert("compress".into(), json!(c));
        }
        if let Some(i) = self.index {
            m.insert("index".into(), json!(i));
        }
        if let Some(k) = &self.key {
            m.insert("key".into(), json!(k));
        }
        Value::Object(m)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// sorted by priority, first one is tried first
    pub servers: Vec<UsenetServer>,
    pub group: String,
    pub groups: Vec<String>,
    pub index_mode: String,
    pub api_port: Option<u16>,
    pub api_host: String,
    pub api_key: Option<String>,
    pub batch_size: Option<u64>,
    pub request_size: Option<u64>,
    /// None = SPLIT_MIN_BACKLOG
    pub split_min_backlog: Option<i64>,
    /// groups indexed at the same time, None = worked out from the connections
    pub parallel_groups: Option<usize>,
    /// compact the database every 24 hours from the indexer
    pub auto_run_compact: bool,
    /// None = DEFAULT_MAX_UNSAVED_HEADERS
    pub max_unsaved_headers: Option<u64>,
    pub postprocess: PostprocessConfig,
    pub archive: ArchiveConfig,
    pub naming: NamingConfig,
    pub predb: PredbConfig,
    pub category: CategoryConfig,
    pub media: MediaConfig,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TvmazeConfig {
    pub enabled: bool,
    pub requests_per_minute: i64,
    pub base_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TvdbConfig {
    pub api_key: String,
    pub requests_per_minute: i64,
    pub base_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TmdbConfig {
    pub api_key: String,
    pub requests_per_minute: i64,
    pub base_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraktConfig {
    pub client_id: String,
    pub requests_per_minute: i64,
    pub base_url: Option<String>,
}

/// The `media` object of config.toml, see the README. Every provider is off until set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaConfig {
    pub parse: bool,
    pub nfo: bool,
    pub negative_ttl_days: u32,
    pub max_titles: i64,
    pub tvmaze: TvmazeConfig,
    pub tmdb: TmdbConfig,
    pub trakt: TraktConfig,
    pub tvdb: TvdbConfig,
}

impl Default for MediaConfig {
    fn default() -> Self {
        MediaConfig {
            parse: true,
            nfo: true,
            negative_ttl_days: 30,
            max_titles: 10_000_000,
            tvmaze: TvmazeConfig { enabled: false, requests_per_minute: 90, base_url: None },
            tmdb: TmdbConfig { api_key: String::new(), requests_per_minute: 240, base_url: None },
            trakt: TraktConfig { client_id: String::new(), requests_per_minute: 120, base_url: None },
            tvdb: TvdbConfig { api_key: String::new(), requests_per_minute: 120, base_url: None },
        }
    }
}

impl MediaConfig {
    pub fn any_provider(&self) -> bool {
        self.tvmaze.enabled
            || !self.tmdb.api_key.is_empty()
            || !self.trakt.client_id.is_empty()
            || !self.tvdb.api_key.is_empty()
    }

    pub fn from_value(v: Option<&Value>) -> MediaConfig {
        let mut c = MediaConfig::default();
        let Some(v) = v else { return c };
        let flag = |key: &str, slot: &mut bool| {
            if let Some(b) = v.get(key).and_then(as_bool) {
                *slot = b;
            }
        };
        flag("parse", &mut c.parse);
        flag("nfo", &mut c.nfo);
        if let Some(n) = v.get("negative_ttl_days").and_then(as_int) {
            c.negative_ttl_days = n.clamp(1, 180) as u32;
        }
        if let Some(n) = v.get("max_titles").and_then(as_int) {
            c.max_titles = n.max(1_000);
        }
        let rate = |o: Option<&Value>, max: i64, slot: &mut i64| {
            if let Some(n) = o.and_then(|o| o.get("requests_per_minute")).and_then(as_int) {
                *slot = n.clamp(1, max);
            }
        };
        let text = |o: Option<&Value>, key: &str| {
            o.and_then(|o| o.get(key)).and_then(Value::as_str).map(str::trim).map(String::from)
        };
        let tvmaze = v.get("tvmaze");
        if let Some(b) = tvmaze.and_then(|o| o.get("enabled")).and_then(as_bool) {
            c.tvmaze.enabled = b;
        }
        rate(tvmaze, 120, &mut c.tvmaze.requests_per_minute);
        c.tvmaze.base_url = text(tvmaze, "base_url").filter(|u| !u.is_empty());
        let tmdb = v.get("tmdb");
        c.tmdb.api_key = text(tmdb, "api_key").unwrap_or_default();
        rate(tmdb, 3_000, &mut c.tmdb.requests_per_minute);
        c.tmdb.base_url = text(tmdb, "base_url").filter(|u| !u.is_empty());
        let trakt = v.get("trakt");
        c.trakt.client_id = text(trakt, "client_id").unwrap_or_default();
        rate(trakt, 200, &mut c.trakt.requests_per_minute);
        c.trakt.base_url = text(trakt, "base_url").filter(|u| !u.is_empty());
        let tvdb = v.get("tvdb");
        c.tvdb.api_key = text(tvdb, "api_key").unwrap_or_default();
        rate(tvdb, 600, &mut c.tvdb.requests_per_minute);
        c.tvdb.base_url = text(tvdb, "base_url").filter(|u| !u.is_empty());
        c
    }
}

/// The file's `media` object, for the API: no keyring.
pub fn media_config() -> MediaConfig {
    read_file().map_or_else(MediaConfig::default, |v| MediaConfig::from_value(v.get("media")))
}

/// Whether the file sets a body budget, for the API: no keyring.
pub fn body_budget_set() -> bool {
    read_file().is_some_and(|v| PostprocessConfig::from_value(v.get("postprocess")).body_fetches_per_minute > 0)
}

/// The `category` object of config.json, see the README.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CategoryConfig {
    pub enabled: bool,
    pub foreign: bool,
    pub web_dl: bool,
}

impl Default for CategoryConfig {
    fn default() -> Self {
        CategoryConfig { enabled: true, foreign: true, web_dl: false }
    }
}

impl CategoryConfig {
    fn from_value(v: Option<&Value>) -> CategoryConfig {
        let mut c = CategoryConfig::default();
        let Some(v) = v else { return c };
        let flag = |key: &str, slot: &mut bool| {
            if let Some(b) = v.get(key).and_then(as_bool) {
                *slot = b;
            }
        };
        flag("enabled", &mut c.enabled);
        flag("foreign", &mut c.foreign);
        flag("web_dl", &mut c.web_dl);
        c
    }

    pub fn options(&self) -> crate::category::Options {
        crate::category::Options { foreign: self.foreign, web_dl: self.web_dl }
    }
}

/// The `predb` object of config.json, see the README.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PredbConfig {
    pub enabled: bool,
    pub sources: Vec<String>,
    pub poll_minutes: u32,
    pub page_size: u32,
    pub max_pages_per_poll: u32,
    pub max_db_mb: i64,
    pub request_delay_ms: u64,
    pub timeout_secs: u64,
    pub history_pages: u32,
    pub late_per_second: i64,
}

impl Default for PredbConfig {
    fn default() -> Self {
        PredbConfig {
            enabled: false,
            sources: Vec::new(),
            poll_minutes: 5,
            page_size: 100,
            max_pages_per_poll: 3,
            max_db_mb: 8192,
            request_delay_ms: 1000,
            timeout_secs: 15,
            history_pages: 0,
            late_per_second: 1,
        }
    }
}

impl PredbConfig {
    fn from_value(v: Option<&Value>) -> PredbConfig {
        let mut c = PredbConfig::default();
        let Some(v) = v else { return c };
        if let Some(b) = v.get("enabled").and_then(as_bool) {
            c.enabled = b;
        }
        if let Some(list) = v.get("sources").and_then(Value::as_array) {
            c.sources = list.iter().filter_map(Value::as_str).map(String::from).collect();
        }
        let int = |key: &str, min: i64, max: i64| v.get(key).and_then(as_int).map(|n| n.clamp(min, max));
        if let Some(n) = int("poll_minutes", 1, 1_440) {
            c.poll_minutes = n as u32;
        }
        if let Some(n) = int("page_size", 1, 100) {
            c.page_size = n as u32;
        }
        if let Some(n) = int("max_pages_per_poll", 1, 20) {
            c.max_pages_per_poll = n as u32;
        }
        if let Some(n) = int("max_db_mb", 64, i64::MAX) {
            c.max_db_mb = n;
        }
        if let Some(n) = int("request_delay_ms", 250, 60_000) {
            c.request_delay_ms = n as u64;
        }
        if let Some(n) = int("timeout_secs", 1, 120) {
            c.timeout_secs = n as u64;
        }
        if let Some(n) = int("history_pages", 0, 1_000_000) {
            c.history_pages = n as u32;
        }
        if let Some(n) = int("late_per_second", 0, 1_000) {
            c.late_per_second = n;
        }
        c
    }
}

/// The `naming` object of config.json, see the README.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamingConfig {
    pub enabled: bool,
    pub nfo_step: bool,
}

impl Default for NamingConfig {
    fn default() -> Self {
        NamingConfig { enabled: true, nfo_step: true }
    }
}

impl NamingConfig {
    fn from_value(v: Option<&Value>) -> NamingConfig {
        let mut c = NamingConfig::default();
        let Some(v) = v else { return c };
        if let Some(b) = v.get("enabled").and_then(as_bool) {
            c.enabled = b;
        }
        if let Some(b) = v.get("nfo_step").and_then(as_bool) {
            c.nfo_step = b;
        }
        c
    }
}

/// The `archive` object of config.json, see the README.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveConfig {
    pub enabled: bool,
    pub max_bodies_per_release: i64,
    pub probe_files: i64,
    pub probes_per_minute: i64,
    pub max_names_mb: i64,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        ArchiveConfig {
            enabled: true,
            max_bodies_per_release: 4,
            probe_files: 2,
            probes_per_minute: 60,
            max_names_mb: 16_384,
        }
    }
}

impl ArchiveConfig {
    fn from_value(v: Option<&Value>) -> ArchiveConfig {
        let mut c = ArchiveConfig::default();
        let Some(v) = v else { return c };
        if let Some(b) = v.get("enabled").and_then(as_bool) {
            c.enabled = b;
        }
        let int = |key: &str, min: i64, max: i64, slot: &mut i64| {
            if let Some(n) = v.get(key).and_then(as_int) {
                *slot = n.clamp(min, max);
            }
        };
        int("max_bodies_per_release", 1, 16, &mut c.max_bodies_per_release);
        int("probe_files", 0, 8, &mut c.probe_files);
        int("probes_per_minute", 12, 600, &mut c.probes_per_minute);
        int("max_names_mb", 64, i64::MAX, &mut c.max_names_mb);
        c
    }
}

/// A group's own floors, taken over the global ones when higher.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupRule {
    pub min_size_bytes: i64,
    pub min_files: i64,
}

/// The `postprocess` object of config.json, see the README. Each key has a
/// default, and a bad value reads as absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostprocessConfig {
    pub enabled: bool,
    pub workers: usize,
    pub settle_secs: i64,
    pub complete_settle_secs: i64,
    pub min_size_bytes: i64,
    pub min_files: i64,
    pub reject_par2_only: bool,
    pub group_rules: HashMap<String, GroupRule>,
    pub backlog_per_second: i64,
    pub body_fetches_per_minute: i64,
    pub body_connections: usize,
    pub body_min_size_bytes: i64,
    pub body_max_size_bytes: i64,
    pub body_max_bytes: i64,
    pub bodies_per_release: i64,
    pub min_completion_percent: i64,
    pub dead_after_secs: i64,
    pub max_attempts: i64,
}

impl Default for PostprocessConfig {
    fn default() -> Self {
        PostprocessConfig {
            enabled: true,
            workers: 2,
            settle_secs: 7200,
            complete_settle_secs: 900,
            min_size_bytes: 0,
            min_files: 1,
            reject_par2_only: true,
            group_rules: HashMap::new(),
            backlog_per_second: 2000,
            body_fetches_per_minute: 0,
            body_connections: 2,
            body_min_size_bytes: 1 << 20,
            body_max_size_bytes: 100 << 30,
            body_max_bytes: 16 << 20,
            bodies_per_release: 8,
            min_completion_percent: 95,
            dead_after_secs: 172_800,
            max_attempts: 5,
        }
    }
}

impl PostprocessConfig {
    fn from_value(v: Option<&Value>) -> PostprocessConfig {
        let mut c = PostprocessConfig::default();
        let Some(v) = v else { return c };
        let int = |key: &str, slot: &mut i64, min: i64| {
            if let Some(n) = v.get(key).and_then(as_int).filter(|n| *n >= min) {
                *slot = n;
            }
        };
        int("settle_secs", &mut c.settle_secs, 0);
        int("complete_settle_secs", &mut c.complete_settle_secs, 0);
        int("min_size_bytes", &mut c.min_size_bytes, 0);
        int("min_files", &mut c.min_files, 0);
        int("backlog_per_second", &mut c.backlog_per_second, 0);
        int("body_fetches_per_minute", &mut c.body_fetches_per_minute, 0);
        int("body_min_size_bytes", &mut c.body_min_size_bytes, 0);
        int("body_max_size_bytes", &mut c.body_max_size_bytes, 0);
        int("body_max_bytes", &mut c.body_max_bytes, 1);
        int("bodies_per_release", &mut c.bodies_per_release, 0);
        int("min_completion_percent", &mut c.min_completion_percent, 0);
        int("dead_after_secs", &mut c.dead_after_secs, 0);
        int("max_attempts", &mut c.max_attempts, 1);
        c.settle_secs = c.settle_secs.max(1);
        c.complete_settle_secs = c.complete_settle_secs.max(1);
        c.min_completion_percent = c.min_completion_percent.min(100);
        let count = |key: &str, slot: &mut usize| {
            if let Some(n) = v.get(key).and_then(as_int).filter(|n| *n > 0) {
                *slot = n as usize;
            }
        };
        count("workers", &mut c.workers);
        count("body_connections", &mut c.body_connections);
        if let Some(b) = v.get("enabled").and_then(as_bool) {
            c.enabled = b;
        }
        if let Some(b) = v.get("reject_par2_only").and_then(as_bool) {
            c.reject_par2_only = b;
        }
        if let Some(rules) = v.get("group_rules").and_then(Value::as_object) {
            for (group, rule) in rules {
                let field = |key: &str| rule.get(key).and_then(as_int).filter(|n| *n >= 0).unwrap_or(0);
                c.group_rules.insert(
                    group.clone(),
                    GroupRule { min_size_bytes: field("min_size_bytes"), min_files: field("min_files") },
                );
            }
        }
        c
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            servers: Vec::new(),
            group: String::new(),
            groups: Vec::new(),
            index_mode: "dynamic".into(),
            api_port: None,
            api_host: DEFAULT_API_HOST.into(),
            api_key: None,
            batch_size: None,
            request_size: None,
            split_min_backlog: None,
            parallel_groups: None,
            auto_run_compact: false,
            max_unsaved_headers: None,
            postprocess: PostprocessConfig::default(),
            archive: ArchiveConfig::default(),
            naming: NamingConfig::default(),
            predb: PredbConfig::default(),
            category: CategoryConfig::default(),
            media: MediaConfig::default(),
        }
    }
}

impl Config {
    pub fn api_port(&self) -> u16 {
        self.api_port.unwrap_or(DEFAULT_API_PORT)
    }

    pub fn batch_size(&self) -> u64 {
        self.batch_size.unwrap_or(DEFAULT_BATCH_SIZE).max(1)
    }

    /// Groups indexed at once on each server, given each indexing server's
    /// connections: one group per `CONNECTIONS_PER_GROUP` connections, or
    /// `parallel_groups` shared out by connections. Every server gets at least one.
    pub fn workers_per_server(&self, connections: &[usize]) -> Vec<usize> {
        if connections.is_empty() {
            return Vec::new();
        }

        let Some(total) = self.parallel_groups else {
            return connections.iter().map(|c| (c / CONNECTIONS_PER_GROUP).clamp(1, MAX_PARALLEL_GROUPS)).collect();
        };

        let total = total.clamp(connections.len(), MAX_PARALLEL_GROUPS);
        let sum: usize = connections.iter().sum::<usize>().max(1);

        // largest remainder, at least one each
        let mut shares: Vec<(usize, usize, usize)> =
            connections.iter().enumerate().map(|(i, c)| (i, (total * c / sum).max(1), (total * c) % sum)).collect();
        let mut given: usize = shares.iter().map(|s| s.1).sum();

        shares.sort_by_key(|s| std::cmp::Reverse(s.2));
        let n = shares.len();
        let mut k = 0;
        while given < total {
            shares[k % n].1 += 1;
            given += 1;
            k += 1;
        }

        shares.sort_by_key(|s| s.0);
        shares.into_iter().map(|s| s.1).collect()
    }

    pub fn request_size(&self) -> u64 {
        self.request_size.unwrap_or(DEFAULT_REQUEST_SIZE).max(1)
    }

    /// headers the indexer fetches ahead of saving them, over every group at
    /// once: what the pool takes of the setting (`nntp::unsaved_cap`)
    pub fn max_unsaved_headers(&self) -> usize {
        let n = self.max_unsaved_headers.unwrap_or(DEFAULT_MAX_UNSAVED_HEADERS);
        crate::nntp::unsaved_cap(usize::try_from(n).unwrap_or(usize::MAX))
    }

    /// article numbers of backfill left before a group's backfill is split
    /// over every server that carries it
    pub fn split_min_backlog(&self) -> i64 {
        self.split_min_backlog.unwrap_or(SPLIT_MIN_BACKLOG).max(1)
    }

    /// the highest priority server
    pub fn primary(&self) -> Option<&UsenetServer> {
        self.servers.first()
    }

    /// `news.a.com` or `news.a.com (+2 fallback)`
    pub fn servers_label(&self) -> String {
        match self.servers.len() {
            0 => String::new(),
            1 => self.servers[0].host.clone(),
            n => format!("{} (+{} fallback)", self.servers[0].host, n - 1),
        }
    }

    /// stable sort: equal priorities keep their order from the file
    pub fn sort_servers(&mut self) {
        self.servers.sort_by_key(|s| s.priority);
    }

    /// groups the indexer should walk, falls back to the single `group` field
    pub fn tracked_groups(&self) -> Vec<String> {
        let groups: Vec<String> =
            self.groups.iter().filter(|g| !g.is_empty() && !crate::filters::group_blocked(g)).cloned().collect();

        if groups.is_empty() && !self.group.is_empty() && !crate::filters::group_blocked(&self.group) {
            return vec![self.group.clone()];
        }

        groups
    }

    /// configured groups that are never indexed, see `filters::group_blocked`
    pub fn blocked_groups(&self) -> Vec<String> {
        let mut blocked: Vec<String> =
            self.groups.iter().chain([&self.group]).filter(|g| crate::filters::group_blocked(g)).cloned().collect();
        blocked.sort_unstable();
        blocked.dedup();
        blocked
    }

    fn from_value(v: &Value) -> Config {
        let s = |key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_string();

        let mut servers: Vec<UsenetServer> = v
            .get("usenet_servers")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(UsenetServer::from_value).collect())
            .unwrap_or_default();

        // the old single server layout
        if servers.is_empty()
            && let Some(mut legacy) = UsenetServer::from_value(v)
        {
            legacy.priority = 1;
            legacy.connections = None;
            servers.push(legacy);
        }

        let mut cfg = Config {
            servers,
            group: s("group"),
            groups: v
                .get("groups")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
                .unwrap_or_default(),
            index_mode: Some(s("index_mode")).filter(|m| !m.is_empty()).unwrap_or_else(|| "dynamic".into()),
            api_port: v.get("api_port").and_then(as_port),
            api_host: Some(s("api_host")).filter(|h| !h.is_empty()).unwrap_or_else(|| DEFAULT_API_HOST.into()),
            api_key: v.get("api_key").and_then(Value::as_str).filter(|k| !k.is_empty()).map(String::from),
            batch_size: v.get("batch_size").and_then(as_int).and_then(|n| u64::try_from(n).ok()).filter(|n| *n > 0),
            request_size: v.get("request_size").and_then(as_int).and_then(|n| u64::try_from(n).ok()).filter(|n| *n > 0),
            split_min_backlog: v.get("split_min_backlog").and_then(as_int).filter(|n| *n > 0),
            parallel_groups: v
                .get("parallel_groups")
                .and_then(as_int)
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n > 0),
            auto_run_compact: v.get("auto_run_compact").and_then(Value::as_bool).unwrap_or(false),
            postprocess: PostprocessConfig::from_value(v.get("postprocess")),
            archive: ArchiveConfig::from_value(v.get("archive")),
            naming: NamingConfig::from_value(v.get("naming")),
            predb: PredbConfig::from_value(v.get("predb")),
            category: CategoryConfig::from_value(v.get("category")),
            media: MediaConfig::from_value(v.get("media")),
            max_unsaved_headers: v
                .get("max_unsaved_headers")
                .and_then(as_int)
                .and_then(|n| u64::try_from(n).ok())
                .filter(|n| *n > 0),
        };

        cfg.sort_servers();

        if cfg.groups.is_empty() && !cfg.group.is_empty() {
            cfg.groups = vec![cfg.group.clone()];
        }

        cfg
    }
}

fn as_port(v: &Value) -> Option<u16> {
    as_int(v).and_then(|n| u16::try_from(n).ok())
}

fn as_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn as_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_i64().map(|n| n != 0),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn env_nonempty(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}

/// server creds come from ATLAS_NNTP_* (docker) instead of config.toml
fn env_creds_active() -> bool {
    ["ATLAS_NNTP_HOST", "ATLAS_NNTP_USER", "ATLAS_NNTP_PASS"].iter().all(|k| env_nonempty(k).is_some())
}

/// Why the config file cant be used though it's there, see `file_problem_at`.
pub fn file_problem() -> Option<String> {
    file_problem_at(&config_file())
}

/// Why the config file at `path` cant be used though it's there: it cant be
/// read, or doesnt parse (where, never the text: it holds passwords), or is
/// JSON that isnt an object (`null`, a list...: it would load as an empty
/// config, and setup would save over it). None when it's fine, or isnt there at all.
pub fn file_problem_at(path: &Path) -> Option<String> {
    let text = match fs::read_to_string(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => return Some(format!("couldnt read {}: {e}", path.display())),
        Ok(text) => text,
    };
    match parse_config(path, &text) {
        Err(e) => Some(format!("{} isnt valid {e}", path.display())),
        Ok(Value::Object(_)) => None,
        Ok(other) => {
            let kind = match other {
                Value::Null => "null",
                Value::Bool(_) => "a boolean",
                Value::Number(_) => "a number",
                Value::String(_) => "a string",
                _ => "an array",
            };
            Some(format!("{} holds {kind}, not an object", path.display()))
        }
    }
}

fn is_json(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "json")
}

/// By its extension. An error names the format and where, never the text: toml's own display quotes the line.
fn parse_config(path: &Path, text: &str) -> Result<Value, String> {
    if is_json(path) {
        return serde_json::from_str(text).map_err(|e| format!("JSON: {e}"));
    }
    toml_edit::de::from_str(text).map_err(|e| {
        let at = e.span().map_or(0, |s| s.start).min(text.len());
        let line = text.as_bytes()[..at].iter().filter(|b| **b == b'\n').count() + 1;
        let first_line = e.message().lines().next().unwrap_or_default();
        // a message can quote a value: say only what's safe
        let what = if first_line.contains('`') || first_line.contains('"') { "syntax error" } else { first_line };
        format!("TOML: {what} at line {line}")
    })
}

fn read_file() -> Option<Value> {
    let path = config_file();
    let text = fs::read_to_string(&path).ok()?;
    parse_config(&path, &text).ok().filter(Value::is_object)
}

/// A config.json becomes config.toml, keeping the old one as `config.json.bak-toml` to go back with.
/// Run once at start. One that doesnt parse stays, for `file_problem` to report.
pub fn migrate_to_toml() -> io::Result<Option<PathBuf>> {
    let dir = crate::paths::app_dir();
    let (json, toml) = (dir.join("config.json"), dir.join("config.toml"));
    if toml.exists() || !json.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&json)?;
    let Ok(Value::Object(_)) = parse_config(&json, &text) else { return Ok(None) };
    let value = parse_config(&json, &text).unwrap_or(Value::Null);
    write_toml_at(&toml, &value)?;
    let back = fs::read_to_string(&toml).ok().and_then(|t| parse_config(&toml, &t).ok());
    if back.as_ref() != Some(&without_nulls(&value)) {
        let _ = fs::remove_file(&toml);
        return Err(io::Error::other("the converted config didnt read back the same"));
    }
    fs::rename(&json, dir.join("config.json.bak-toml"))?;
    Ok(Some(toml))
}

/// TOML has no null, and an absent key means the same to every reader here.
fn without_nulls(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k.clone(), without_nulls(v))).collect(),
        ),
        Value::Array(list) => Value::Array(list.iter().filter(|v| !v.is_null()).map(without_nulls).collect()),
        other => other.clone(),
    }
}

/// Only the keys that changed are replaced, soo comments elsewhere in the file survive a save. Always private: it can hold passwords.
fn write_toml_at(path: &Path, value: &Value) -> io::Result<()> {
    let value = without_nulls(value);
    // pretty: `[[usenet_servers]]` and `[media]`, not one long inline line
    let pretty = toml_edit::ser::to_string_pretty(&value).map_err(io::Error::other)?;
    let new = pretty.parse::<toml_edit::DocumentMut>().map_err(io::Error::other)?;
    let old_text = fs::read_to_string(path).unwrap_or_default();
    let text = match (old_text.parse::<toml_edit::DocumentMut>(), toml_edit::de::from_str::<Value>(&old_text)) {
        (Ok(mut doc), Ok(old)) => {
            let gone: Vec<String> = doc.iter().map(|(k, _)| k.to_string()).filter(|k| value.get(k).is_none()).collect();
            for key in gone {
                doc.remove(&key);
            }
            for (key, item) in new.iter() {
                if old.get(key) != value.get(key) {
                    doc[key] = item.clone();
                }
            }
            doc.to_string()
        }
        _ => pretty,
    };
    write_atomic(path, text.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Load config. Servers come from `usenet_servers` (or the old top level
/// host/username/password/port). `ATLAS_NNTP_*` env vars (docker) add a
/// server in front of those, while groups and the api key still come from
/// the file soo they survive restarts.
pub fn load_config() -> Option<Config> {
    let file = read_file();

    let host = env_nonempty("ATLAS_NNTP_HOST");
    let user = env_nonempty("ATLAS_NNTP_USER");
    let pass = env_nonempty("ATLAS_NNTP_PASS");

    if let (Some(host), Some(user), Some(pass)) = (host, user, pass) {
        let mut cfg = file.as_ref().map(Config::from_value).unwrap_or_default();

        let port = env::var("ATLAS_NNTP_PORT").ok().and_then(|p| p.trim().parse().ok()).unwrap_or(563);
        let mut server = UsenetServer::new(&host, &user, &pass, port);
        server.priority = i64::MIN;
        server.connections =
            env::var("ATLAS_NNTP_CONNECTIONS").ok().and_then(|c| c.trim().parse().ok()).filter(|c| *c > 0);

        cfg.servers.retain(|s| !s.host.eq_ignore_ascii_case(&server.host));
        cfg.servers.insert(0, server);

        if let Some(mode) = env_nonempty("ATLAS_INDEX_MODE") {
            cfg.index_mode = mode;
        }

        if let Some(port) = env_nonempty("ATLAS_API_PORT") {
            cfg.api_port = Some(port.trim().parse().unwrap_or(DEFAULT_API_PORT));
        }

        cfg.api_host = env_nonempty("ATLAS_API_HOST").unwrap_or_else(|| DEFAULT_API_HOST.into());

        return Some(cfg);
    }

    let mut cfg = Config::from_value(&file?);

    // passwords that arent in the file live in the os keyring
    for server in cfg.servers.iter_mut().filter(|s| s.password.is_empty()) {
        if let Some(password) = keyring::get(&server.username) {
            server.password = password;
        }
    }

    Some(cfg)
}

/// The settings in config.json, for a command that needs no password:
/// `load_config` asks the keyring.
pub fn load_settings() -> Config {
    read_file().map_or_else(Config::default, |v| Config::from_value(&v))
}

/// Persist the config, keeping any keys in config.json atlas doesnt know about.
///
/// One server is saved in the old top level layout with its password in the
/// os keyring when possible. Several servers are saved as `usenet_servers`;
/// a password that was already written in the file stays there, new ones go
/// to the keyring when it works. Server creds from env vars never get saved.
pub fn save_config(cfg: &Config) -> io::Result<()> {
    let existing = read_file();
    let mut out = match &existing {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    };

    let mut private = out.get("password").and_then(Value::as_str).is_some_and(|p| !p.is_empty());

    if !env_creds_active() {
        let list_form = out.contains_key("usenet_servers") || cfg.servers.len() > 1;

        if list_form {
            let in_file = |host: &str| {
                existing.as_ref().map(Config::from_value).is_some_and(|old| {
                    old.servers.iter().any(|s| s.host.eq_ignore_ascii_case(host) && !s.password.is_empty())
                })
            };

            let servers: Vec<Value> = cfg
                .servers
                .iter()
                .map(|s| {
                    let plain = in_file(&s.host) || !keyring::set(&s.username, &s.password);
                    private |= plain && !s.password.is_empty();
                    s.to_value(plain)
                })
                .collect();

            for key in ["host", "username", "password", "port", "ssl"] {
                out.remove(key);
            }
            out.insert("usenet_servers".into(), Value::Array(servers));
            private |= out.get("password").is_some();
        } else {
            let server = cfg.servers.first().cloned().unwrap_or_else(|| UsenetServer::new("", "", "", 563));
            out.insert("host".into(), json!(server.host));
            out.insert("username".into(), json!(server.username));
            out.insert("port".into(), json!(server.port));

            if keyring::set(&server.username, &server.password) {
                out.remove("password");
                private = false;
            } else {
                out.insert("password".into(), json!(server.password));
                private = true;
            }
        }
    } else if out.get("usenet_servers").is_some() {
        private = true;
    }

    let api_port = cfg.api_port.or_else(|| out.get("api_port").and_then(as_port));
    let api_key = out
        .get("api_key")
        .and_then(Value::as_str)
        .filter(|k| !k.is_empty())
        .map(String::from)
        .or_else(|| cfg.api_key.clone());

    out.insert("group".into(), json!(cfg.group));
    out.insert("groups".into(), json!(cfg.groups.iter().filter(|g| !g.is_empty()).collect::<Vec<_>>()));
    out.insert("index_mode".into(), json!(cfg.index_mode));

    if let Some(port) = api_port {
        out.insert("api_port".into(), json!(port));
    }

    if let Some(key) = api_key {
        out.insert("api_key".into(), json!(key));
    }

    if let Some(n) = cfg.split_min_backlog {
        out.insert("split_min_backlog".into(), json!(n));
    }

    if cfg.auto_run_compact {
        out.insert("auto_run_compact".into(), json!(true));
    }

    write_config(&Value::Object(out), private)
}

/// Returns the api key, generating and persisting one the first time.
/// The bool is true when a new key was made.
pub fn get_api_key(cfg: &mut Config) -> (String, bool) {
    if let Some(key) = &cfg.api_key {
        return (key.clone(), false);
    }

    let key = token_urlsafe(32);
    cfg.api_key = Some(key.clone());

    let mut saved = match read_file() {
        Some(Value::Object(map)) => map,
        Some(_) => return (key, true),
        None if config_file().exists() => return (key, true),
        None => Map::new(),
    };

    let has_password = saved.contains_key("password") || saved.contains_key("usenet_servers");
    saved.insert("api_key".into(), json!(key));
    let _ = write_config(&Value::Object(saved), has_password);

    (key, true)
}

fn write_config(value: &Value, private: bool) -> io::Result<()> {
    let path = config_file();
    if !is_json(&path) {
        return write_toml_at(&path, value);
    }
    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
    serde::Serialize::serialize(value, &mut ser).map_err(io::Error::other)?;

    write_atomic(&path, &buf)?;

    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }

    #[cfg(not(unix))]
    let _ = private;

    Ok(())
}

/// Same shape as python's secrets.token_urlsafe
pub fn token_urlsafe(nbytes: usize) -> String {
    use base64::Engine;

    let mut bytes = vec![0u8; nbytes];
    getrandom::fill(&mut bytes).expect("os rng unavailable");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub mod keyring {
    use super::SERVICE;

    fn disabled() -> bool {
        std::env::var_os("ATLAS_NO_KEYRING").is_some()
    }

    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    pub fn get(username: &str) -> Option<String> {
        if disabled() || username.is_empty() {
            return None;
        }

        ::keyring::Entry::new(SERVICE, username).and_then(|e| e.get_password()).ok().filter(|p| !p.is_empty())
    }

    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    pub fn set(username: &str, password: &str) -> bool {
        if disabled() || username.is_empty() {
            return false;
        }

        ::keyring::Entry::new(SERVICE, username).and_then(|e| e.set_password(password)).is_ok()
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    pub fn get(_username: &str) -> Option<String> {
        let _ = (SERVICE, disabled());
        None
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    pub fn set(_username: &str, _password: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_is_all_off_when_absent() {
        let c = Config::from_value(&json!({})).media;
        assert!(!c.any_provider());
        assert!(c.parse);
    }

    #[test]
    fn tvmaze_needs_its_switch() {
        assert!(!MediaConfig::from_value(Some(&json!({"tvmaze": {}}))).any_provider());
        assert!(MediaConfig::from_value(Some(&json!({"tvmaze": {"enabled": true}}))).any_provider());
    }

    #[test]
    fn an_empty_key_is_off() {
        assert!(!MediaConfig::from_value(Some(&json!({"tmdb": {"api_key": ""}}))).any_provider());
    }

    #[test]
    fn a_key_turns_tmdb_on() {
        assert!(MediaConfig::from_value(Some(&json!({"tmdb": {"api_key": "K"}}))).any_provider());
    }

    #[test]
    fn a_client_id_turns_trakt_on() {
        assert!(MediaConfig::from_value(Some(&json!({"trakt": {"client_id": "ID"}}))).any_provider());
    }

    #[test]
    fn a_key_turns_tvdb_on() {
        assert!(MediaConfig::from_value(Some(&json!({"tvdb": {"api_key": "K"}}))).any_provider());
        assert!(!MediaConfig::from_value(Some(&json!({"tvdb": {"api_key": " "}}))).any_provider());
    }

    #[test]
    fn requests_per_minute_is_clamped() {
        let high = MediaConfig::from_value(Some(&json!({"tvmaze": {"requests_per_minute": 5000}})));
        assert_eq!(high.tvmaze.requests_per_minute, 120);
        let low = MediaConfig::from_value(Some(&json!({"tvmaze": {"requests_per_minute": 0}})));
        assert_eq!(low.tvmaze.requests_per_minute, 1);
    }

    #[test]
    fn the_example_keeps_every_provider_off() {
        let example = parse_config(Path::new("config.toml"), include_str!("../config.example.toml")).unwrap();
        assert!(!Config::from_value(&example).media.any_provider());
    }

    #[test]
    fn the_docs_describe_ids() {
        let readme = include_str!("../README.md");
        assert!(readme.contains("### TV and movie IDs"));
        assert!(!readme.contains("searches by ID, such as TVDB or IMDb, return nothing"));
    }

    #[test]
    fn category_defaults_when_absent() {
        assert_eq!(
            Config::from_value(&json!({})).category,
            CategoryConfig { enabled: true, foreign: true, web_dl: false }
        );
    }

    #[test]
    fn category_keys_parse() {
        let c = Config::from_value(&json!({"category": {"enabled": false, "foreign": false, "web_dl": true}})).category;
        assert_eq!(c, CategoryConfig { enabled: false, foreign: false, web_dl: true });
    }

    #[test]
    fn a_bad_category_value_falls_back() {
        let c = Config::from_value(&json!({"category": {"enabled": "perhaps", "web_dl": []}})).category;
        assert_eq!(c, CategoryConfig::default());
        assert_eq!(Config::from_value(&json!({"category": 1})).category, CategoryConfig::default());
    }

    #[test]
    fn the_docs_describe_categories() {
        let readme = include_str!("../README.md");
        assert!(readme.contains("### Categories") && readme.contains("cat=8000"));
        assert!(
            !readme.contains("Every release is in category")
                && !readme.contains("Atlas reports every release as category")
        );
        let example = parse_config(Path::new("config.toml"), include_str!("../config.example.toml")).unwrap();
        assert_eq!(Config::from_value(&example).category, CategoryConfig::default());
    }

    #[test]
    fn predb_defaults_when_absent() {
        let c = Config::from_value(&json!({})).predb;
        assert_eq!(c, PredbConfig::default());
        assert!(!c.enabled);
        assert!(c.sources.is_empty());
    }

    #[test]
    fn predb_keys_parse() {
        let c = Config::from_value(&json!({"predb": {
            "enabled": true, "sources": ["predb_club", "xrel"], "poll_minutes": 10, "page_size": 50,
            "max_pages_per_poll": 2, "max_db_mb": 100, "request_delay_ms": 2000, "timeout_secs": 30,
            "history_pages": 7, "late_per_second": 3
        }}))
        .predb;
        assert!(c.enabled);
        assert_eq!(c.sources, ["predb_club", "xrel"]);
        assert_eq!((c.poll_minutes, c.page_size, c.max_pages_per_poll, c.history_pages), (10, 50, 2, 7));
        assert_eq!((c.max_db_mb, c.request_delay_ms, c.timeout_secs, c.late_per_second), (100, 2000, 30, 3));
    }

    #[test]
    fn a_bad_predb_value_falls_back() {
        let c = Config::from_value(&json!({"predb": {"enabled": "yes please", "sources": 7, "poll_minutes": "soon"}}))
            .predb;
        assert_eq!(c, PredbConfig::default());
        assert_eq!(Config::from_value(&json!({"predb": 3})).predb, PredbConfig::default());
    }

    #[test]
    fn values_are_clamped() {
        let c = Config::from_value(&json!({"predb": {
            "poll_minutes": 0, "page_size": 9000, "max_pages_per_poll": 99, "max_db_mb": 1,
            "request_delay_ms": 1, "timeout_secs": 999, "late_per_second": -4, "history_pages": -1
        }}))
        .predb;
        assert_eq!((c.poll_minutes, c.page_size, c.max_pages_per_poll), (1, 100, 20));
        assert_eq!((c.max_db_mb, c.request_delay_ms, c.timeout_secs), (64, 250, 120));
        assert_eq!((c.late_per_second, c.history_pages), (0, 0));
    }

    #[test]
    fn naming_defaults_when_absent() {
        let c = Config::from_value(&json!({})).naming;
        assert_eq!(c, NamingConfig { enabled: true, nfo_step: true });
    }

    #[test]
    fn naming_keys_parse() {
        let c = Config::from_value(&json!({"naming": {"enabled": false, "nfo_step": false}})).naming;
        assert_eq!(c, NamingConfig { enabled: false, nfo_step: false });
    }

    #[test]
    fn a_bad_naming_value_falls_back() {
        let c = Config::from_value(&json!({"naming": {"enabled": "perhaps", "nfo_step": 7}})).naming;
        assert_eq!(c, NamingConfig::default());
        assert_eq!(Config::from_value(&json!({"naming": 7})).naming, NamingConfig::default());
    }

    #[test]
    fn archive_defaults_when_absent() {
        let c = ArchiveConfig::from_value(None);
        assert!(c.enabled);
        assert_eq!((c.max_bodies_per_release, c.probe_files, c.probes_per_minute, c.max_names_mb), (4, 2, 60, 16_384));
    }

    #[test]
    fn archive_keys_parse() {
        let v = serde_json::json!({
            "enabled": false, "max_bodies_per_release": 2, "probe_files": 1, "probes_per_minute": 30, "max_names_mb": 128
        });
        let c = ArchiveConfig::from_value(Some(&v));
        assert!(!c.enabled);
        assert_eq!((c.max_bodies_per_release, c.probe_files, c.probes_per_minute, c.max_names_mb), (2, 1, 30, 128));
    }

    #[test]
    fn archive_values_clamp() {
        let v = serde_json::json!({
            "max_bodies_per_release": 500, "probe_files": 99, "probes_per_minute": 1, "max_names_mb": 1
        });
        let c = ArchiveConfig::from_value(Some(&v));
        assert_eq!((c.max_bodies_per_release, c.probe_files, c.probes_per_minute, c.max_names_mb), (16, 8, 12, 64));
        let c = ArchiveConfig::from_value(Some(
            &serde_json::json!({"max_bodies_per_release": 0, "probes_per_minute": 9000}),
        ));
        assert_eq!((c.max_bodies_per_release, c.probes_per_minute), (1, 600));
    }

    #[test]
    fn a_bad_archive_value_falls_back() {
        let c = ArchiveConfig::from_value(Some(&serde_json::json!({"enabled": "maybe", "probe_files": "many"})));
        assert!(c.enabled);
        assert_eq!(c.probe_files, 2);
        assert_eq!(ArchiveConfig::from_value(Some(&serde_json::json!(7))).probe_files, 2);
    }

    #[test]
    fn readme_lists_every_archive_key() {
        let readme = include_str!("../README.md");
        for key in ["max_bodies_per_release", "probe_files", "probes_per_minute", "max_names_mb"] {
            assert!(readme.contains(key), "{key}");
        }
    }

    #[test]
    fn token_is_43_chars_urlsafe() {
        let t = token_urlsafe(32);
        assert_eq!(t.len(), 43);
        assert!(t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn from_value_is_lenient() {
        let v = json!({"host": "h", "username": "u", "port": "119", "group": "a.b", "api_port": 8000});
        let cfg = Config::from_value(&v);
        assert_eq!(cfg.servers.len(), 1);
        assert_eq!(cfg.servers[0].port, 119);
        assert!(!cfg.servers[0].use_ssl());
        assert_eq!(cfg.groups, vec!["a.b".to_string()]);
        assert_eq!(cfg.api_port(), 8000);
        assert_eq!(cfg.api_host, "127.0.0.1");
        assert_eq!(cfg.index_mode, "dynamic");
        assert_eq!(cfg.max_unsaved_headers(), DEFAULT_MAX_UNSAVED_HEADERS as usize);
    }

    #[test]
    fn an_invalid_config_says_where_without_its_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        assert_eq!(file_problem_at(&path), None, "missing isnt a problem: setup offers to make one");

        // a trailing comma after the last server
        std::fs::write(&path, "{\n  \"usenet_servers\": [{\"host\": \"news.x\", \"password\": \"hunter2\"},]\n}\n")
            .unwrap();
        let problem = file_problem_at(&path).expect("a trailing comma is a problem");
        assert!(problem.contains(&path.display().to_string()), "{problem}");
        assert!(problem.contains("trailing comma at line 2 column"), "{problem}");
        assert!(!problem.contains("hunter2") && !problem.contains("news.x"), "no file contents: {problem}");

        std::fs::write(&path, "{\"groups\": []}").unwrap();
        assert_eq!(file_problem_at(&path), None);
    }

    #[test]
    fn valid_json_that_isnt_an_object_is_a_problem_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        // setup would save over any of these, as if there were no config
        for (text, kind) in [("null", "null"), ("[]", "an array"), ("\"hunter2\"", "a string"), ("42", "a number")] {
            std::fs::write(&path, text).unwrap();
            let problem = file_problem_at(&path).unwrap_or_else(|| panic!("{text} isnt a config"));
            assert!(problem.contains(&path.display().to_string()), "{problem}");
            assert!(problem.contains(&format!("{kind}, not an object")), "{problem}");
            assert!(!problem.contains("hunter2"), "no file contents: {problem}");
        }
        std::fs::write(&path, "{}").unwrap();
        assert_eq!(file_problem_at(&path), None, "an empty object is still a config");
    }

    #[test]
    fn invalid_toml_says_where_without_its_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "groups = []\npassword = \"hunter2\n").unwrap();
        let problem = file_problem_at(&path).expect("an unclosed string");
        assert!(problem.contains("TOML") && problem.contains("line 2"), "{problem}");
        assert!(!problem.contains("hunter2"), "no file contents: {problem}");
        std::fs::write(&path, "groups = []\n").unwrap();
        assert_eq!(file_problem_at(&path), None);
    }

    #[test]
    fn a_save_keeps_comments_on_what_didnt_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let text = "# my servers\napi_port = 9091 # the port\ngroups = [\"a\"]\n\n# mine\n[media]\nparse = true\n";
        std::fs::write(&path, text).unwrap();
        let value = json!({"api_port": 9091, "groups": ["a", "b"], "media": {"parse": true}});
        write_toml_at(&path, &value).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("# the port") && saved.contains("# mine"), "{saved}");
        assert_eq!(parse_config(&path, &saved).unwrap(), value);
    }

    #[test]
    fn every_shape_of_config_survives_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let value = json!({
            "archive": {"enabled": true, "ratio": 0.5},
            "group": "alt.binaries.x",
            "groups": [],
            "note": null,
            "usenet_servers": [{"host": "a", "port": 563, "ssl": true}, {"host": "b"}],
            "z_after_tables": 1,
        });
        write_toml_at(&path, &value).unwrap();
        let back = parse_config(&path, &std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back, without_nulls(&value));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn max_unsaved_headers_from_the_file() {
        let cfg = |v: Value| Config::from_value(&v).max_unsaved_headers();
        assert_eq!(cfg(json!({"max_unsaved_headers": 2_000_000})), 2_000_000);
        assert_eq!(cfg(json!({"max_unsaved_headers": "40000"})), 40_000);
        // nonsense falls back to the default
        assert_eq!(cfg(json!({"max_unsaved_headers": 0})), DEFAULT_MAX_UNSAVED_HEADERS as usize);
        assert_eq!(cfg(json!({"max_unsaved_headers": -5})), DEFAULT_MAX_UNSAVED_HEADERS as usize);
    }

    /// A cap past what the pool can hold reads as what the pool holds, soo
    /// the config reload (which compares the two) doesnt see a change every
    /// time it looks.
    #[test]
    fn a_max_unsaved_headers_past_the_pools_limit_reads_as_the_pools_cap() {
        let cfg = Config::from_value(&json!({"max_unsaved_headers": 1u64 << 62}));
        let pool = crate::nntp::Pool::new(&[]).with_max_unsaved(cfg.max_unsaved_headers());
        assert_eq!(cfg.max_unsaved_headers(), pool.max_unsaved());
    }

    #[test]
    fn multiple_servers_sorted_by_priority() {
        let v = json!({
            "usenet_servers": [
                {"host": "d.example", "username": "u", "password": "p", "port": 563, "ssl": true, "connections": 10, "priority": 4},
                {"host": "b.example", "username": "u", "password": "p", "port": 563, "ssl": true, "connections": 10, "priority": 2},
                {"host": "a.example", "username": "u", "password": "p", "port": 563, "ssl": true, "priority": 1},
                {"host": "e.example", "username": "u", "password": "p", "port": "119", "ssl": "false", "priority": 4},
                {"host": "", "username": "skipped"},
                {"host": "z.example", "username": "u", "password": "p"}
            ],
            "host": "ignored.legacy",
            "group": "alt.binaries.x"
        });
        let cfg = Config::from_value(&v);
        let hosts: Vec<_> = cfg.servers.iter().map(|s| s.host.as_str()).collect();
        // ties keep file order, missing priority goes last
        assert_eq!(hosts, vec!["a.example", "b.example", "d.example", "e.example", "z.example"]);
        assert_eq!(cfg.servers[1].connections(), 10);
        assert_eq!(cfg.servers[0].connections(), DEFAULT_CONNECTIONS);
        assert!(!cfg.servers[3].use_ssl());
        assert_eq!(cfg.servers[3].port, 119);
        assert_eq!(cfg.servers_label(), "a.example (+4 fallback)");
    }

    #[test]
    fn workers_follow_connections() {
        let auto = Config::default();
        assert_eq!(auto.workers_per_server(&[25, 25, 25, 25]), vec![5, 5, 5, 5]);
        assert_eq!(auto.workers_per_server(&[10, 3]), vec![2, 1], "at least one each");

        let fixed = Config { parallel_groups: Some(20), ..Config::default() };
        assert_eq!(fixed.workers_per_server(&[25, 25, 25, 25]), vec![5, 5, 5, 5]);
        assert_eq!(fixed.workers_per_server(&[10, 30]), vec![5, 15]);
        assert_eq!(fixed.workers_per_server(&[1, 1, 1]).iter().sum::<usize>(), 20);

        let tiny = Config { parallel_groups: Some(1), ..Config::default() };
        assert_eq!(tiny.workers_per_server(&[10, 10]), vec![1, 1], "every server gets one");
    }

    #[test]
    fn index_flag_round_trips() {
        let v = json!({"usenet_servers": [
            {"host": "a", "password": "p", "connections": 30},
            {"host": "b", "password": "p", "index": false}
        ]});
        let cfg = Config::from_value(&v);
        assert_eq!(cfg.servers[0].connections(), 30);
        assert!(cfg.servers[0].indexes());
        assert!(!cfg.servers[1].indexes());
        assert!(cfg.servers[0].to_value(true).get("index").is_none());
        assert!(cfg.servers[1].to_value(true)["index"] == false);
    }

    #[test]
    fn postprocess_defaults_when_absent() {
        let c = Config::from_value(&json!({}));
        assert_eq!(c.postprocess, PostprocessConfig::default());
        assert!(c.postprocess.enabled);
        assert_eq!(c.postprocess.body_fetches_per_minute, 0, "no bodies unless asked");
    }

    #[test]
    fn postprocess_keys_parse() {
        let c = Config::from_value(&json!({"postprocess": {
            "enabled": false, "workers": 3, "settle_secs": 60, "complete_settle_secs": 30, "min_size_bytes": 5,
            "min_files": 2, "reject_par2_only": false, "backlog_per_second": 10, "body_fetches_per_minute": 300,
            "body_connections": 4, "body_min_size_bytes": 7, "body_max_size_bytes": 8, "body_max_bytes": 9,
            "bodies_per_release": 2, "min_completion_percent": 90, "dead_after_secs": 11, "max_attempts": 3,
            "group_rules": {"alt.binaries.x": {"min_size_bytes": 100, "min_files": 2}}
        }}))
        .postprocess;
        assert!(!c.enabled && !c.reject_par2_only);
        assert_eq!((c.workers, c.body_connections), (3, 4));
        assert_eq!((c.settle_secs, c.complete_settle_secs, c.min_size_bytes, c.min_files), (60, 30, 5, 2));
        assert_eq!((c.backlog_per_second, c.body_fetches_per_minute), (10, 300));
        assert_eq!((c.body_min_size_bytes, c.body_max_size_bytes, c.body_max_bytes), (7, 8, 9));
        assert_eq!((c.bodies_per_release, c.min_completion_percent, c.dead_after_secs, c.max_attempts), (2, 90, 11, 3));
        assert_eq!(c.group_rules["alt.binaries.x"], GroupRule { min_size_bytes: 100, min_files: 2 });
    }

    #[test]
    fn a_bad_value_falls_back() {
        let c = Config::from_value(&json!({"postprocess": {
            "enabled": "maybe", "workers": 0, "settle_secs": 0, "complete_settle_secs": -5, "body_fetches_per_minute": "lots",
            "min_completion_percent": 500, "max_attempts": 0, "group_rules": 7
        }}))
        .postprocess;
        assert!(c.enabled);
        assert_eq!(c.workers, 2);
        assert_eq!(c.settle_secs, 1, "floored");
        assert_eq!(c.complete_settle_secs, 900);
        assert_eq!(c.body_fetches_per_minute, 0);
        assert_eq!(c.min_completion_percent, 100);
        assert_eq!(c.max_attempts, 5);
        assert!(c.group_rules.is_empty());
        assert_eq!(Config::from_value(&json!({"postprocess": 3})).postprocess, PostprocessConfig::default());
    }
}
