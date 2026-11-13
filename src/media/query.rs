//! Searches by ID, and what results say of a release's IDs.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::config::{self, MediaConfig};
use crate::search::ReleaseRow;

use super::resolve::Ids;
use super::title::Kind;

pub const ID_ROWS: i64 = 20_000;
pub const MAX_TITLE: usize = 255;
pub const ID_SCAN: i64 = 200_000;
const PARAMS_FOR: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdSet {
    pub tvdb: bool,
    pub tvmaze: bool,
    pub trakt: bool,
    pub tmdb: bool,
    pub imdb: bool,
    pub rid: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdParams {
    pub tv: IdSet,
    pub movie: IdSet,
}

impl IdSet {
    /// What `caps` adds to a search's parameters.
    pub fn caps(&self) -> String {
        let mut out = String::new();
        for (on, name) in [
            (self.tvdb, "tvdbid"),
            (self.imdb, "imdbid"),
            (self.rid, "rid"),
            (self.tvmaze, "tvmazeid"),
            (self.tmdb, "tmdbid"),
            (self.trakt, "traktid"),
        ] {
            if on {
                out.push(',');
                out.push_str(name);
            }
        }
        out
    }
}

/// The IDs each provider can fill.
pub fn id_params(cfg: &MediaConfig, body_budget_set: bool) -> IdParams {
    let (maze_on, tmdb, trakt, tvdb) = (
        cfg.tvmaze.enabled,
        !cfg.tmdb.api_key.is_empty(),
        !cfg.trakt.client_id.is_empty(),
        !cfg.tvdb.api_key.is_empty(),
    );
    let any = cfg.any_provider();
    let nfo = cfg.nfo && cfg.parse && body_budget_set;
    IdParams {
        tv: IdSet {
            tvdb: any,
            rid: maze_on || tmdb || trakt,
            imdb: any,
            tvmaze: maze_on || tvdb,
            tmdb: tmdb || trakt || tvdb,
            trakt,
        },
        movie: IdSet { imdb: tmdb || trakt || nfo, tmdb: tmdb || trakt, trakt, ..IdSet::default() },
    }
}

static CACHED: LazyLock<Mutex<Option<(Instant, IdParams)>>> = LazyLock::new(Default::default);

/// Re-read at most every 30 s. `t=caps` refreshes it.
pub fn cached_id_params() -> IdParams {
    let mut cached = CACHED.lock().unwrap();
    if let Some((at, on)) = *cached
        && at.elapsed() < PARAMS_FOR
    {
        return on;
    }
    let on = id_params(&config::media_config(), config::body_budget_set());
    *cached = Some((Instant::now(), on));
    on
}

/// What `t=caps` advertises, from the file.
pub fn refreshed_id_params() -> IdParams {
    let on = id_params(&config::media_config(), config::body_budget_set());
    *CACHED.lock().unwrap() = Some((Instant::now(), on));
    on
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdColumn {
    Tvdb,
    Tvmaze,
    Trakt,
    Tmdb,
    Imdb,
    Tvrage,
}

impl IdColumn {
    fn column(self) -> &'static str {
        match self {
            IdColumn::Tvdb => "tvdb_id",
            IdColumn::Tvmaze => "tvmaze_id",
            IdColumn::Trakt => "trakt_id",
            IdColumn::Tmdb => "tmdb_id",
            IdColumn::Imdb => "imdb_id",
            IdColumn::Tvrage => "tvrage_id",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdQuery {
    pub kind: Kind,
    pub ids: Vec<(IdColumn, i64)>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
}

fn positive(args: &HashMap<String, String>, key: &str) -> Option<i64> {
    args.get(key)?.trim().parse::<i64>().ok().filter(|n| *n > 0)
}

fn imdb_arg(args: &HashMap<String, String>) -> Option<i64> {
    let text = args.get("imdbid")?.trim();
    let digits = text.strip_prefix("tt").or_else(|| text.strip_prefix("TT")).unwrap_or(text);
    digits.parse::<i64>().ok().filter(|n| *n > 0)
}

fn episode_arg(args: &HashMap<String, String>, season: i64) -> Option<i64> {
    let text = args.get("ep")?.trim();
    if let Some((m, d)) = text.split_once('/') {
        if !(1000..=9999).contains(&season) {
            return None;
        }
        return Some(m.parse::<i64>().ok()? * 100 + d.parse::<i64>().ok()?);
    }
    text.parse().ok()
}

/// Only `t=tvsearch` and `t=movie`. Takes parameters that are on. None when none remain.
pub fn from_args(t: &str, args: &HashMap<String, String>, on: &IdParams) -> Option<IdQuery> {
    let (kind, set) = match t {
        "tvsearch" => (Kind::Tv, on.tv),
        "movie" => (Kind::Movie, on.movie),
        _ => return None,
    };
    let mut ids = Vec::new();
    let mut take = |enabled: bool, column: IdColumn, value: Option<i64>| {
        if enabled && let Some(v) = value {
            ids.push((column, v));
        }
    };
    take(set.tvdb, IdColumn::Tvdb, positive(args, "tvdbid"));
    take(set.tvmaze, IdColumn::Tvmaze, positive(args, "tvmazeid"));
    take(set.trakt, IdColumn::Trakt, positive(args, "traktid"));
    take(set.tmdb, IdColumn::Tmdb, positive(args, "tmdbid"));
    take(set.imdb, IdColumn::Imdb, imdb_arg(args));
    take(set.rid, IdColumn::Tvrage, positive(args, "rid").or_else(|| positive(args, "tvrageid")));
    if ids.is_empty() {
        return None;
    }
    let season = args.get("season").and_then(|s| s.trim().parse::<i64>().ok());
    let episode = season.and_then(|s| episode_arg(args, s));
    Some(IdQuery { kind, ids, season, episode })
}

pub struct IdPage {
    pub rows: Vec<ReleaseRow>,
    pub scanned: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaAttrs {
    pub kind: i64,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub air_date: Option<i64>,
    pub year: Option<i64>,
    pub ids: Ids,
}

fn attr(out: &mut String, name: &str, value: impl std::fmt::Display) {
    out.push_str(&format!("<newznab:attr name=\"{name}\" value=\"{value}\"/>"));
}

impl MediaAttrs {
    /// `on.tv` or `on.movie`, by `kind`. Season, episode and year always; an ID only when its parameter is on.
    pub fn xml(&self, on: &IdParams) -> String {
        let set = if self.kind == Kind::Tv as i64 { on.tv } else { on.movie };
        let mut out = String::new();
        if let Some(season) = self.season {
            attr(&mut out, "season", season);
        }
        if let Some(date) = self.air_date {
            let (y, m, d) = (date / 10_000, date / 100 % 100, date % 100);
            attr(&mut out, "episode", format!("{m:02}/{d:02}"));
            attr(&mut out, "tvairdate", format!("{y:04}-{m:02}-{d:02}"));
        } else if let Some(episode) = self.episode {
            attr(&mut out, "episode", episode);
        }
        if let Some(year) = self.year {
            attr(&mut out, "year", year);
        }
        if set.tvdb
            && let Some(id) = self.ids.tvdb
        {
            attr(&mut out, "tvdbid", id);
        }
        if set.tvmaze
            && let Some(id) = self.ids.tvmaze
        {
            attr(&mut out, "tvmazeid", id);
        }
        if set.trakt
            && let Some(id) = self.ids.trakt
        {
            attr(&mut out, "traktid", id);
        }
        if set.tmdb
            && let Some(id) = self.ids.tmdb
        {
            attr(&mut out, "tmdbid", id);
        }
        if set.rid
            && let Some(id) = self.ids.tvrage
        {
            attr(&mut out, "rageid", id);
            attr(&mut out, "tvrageid", id);
        }
        if set.imdb
            && let Some(id) = self.ids.imdb
        {
            let digits = format!("{id:07}");
            attr(&mut out, "imdb", &digits);
            attr(&mut out, "imdbid", &digits);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(q: &str) -> HashMap<String, String> {
        crate::api::parse_query(q)
    }

    fn all_on() -> IdParams {
        let set = IdSet { tvdb: true, tvmaze: true, trakt: true, tmdb: true, imdb: true, rid: true };
        IdParams { tv: set, movie: IdSet { tvdb: false, tvmaze: false, rid: false, ..set } }
    }

    #[test]
    fn from_args_table() {
        let on = all_on();
        let q = from_args("tvsearch", &args("tvdbid=3001&season=1&ep=2"), &on).unwrap();
        assert_eq!((q.ids, q.season, q.episode), (vec![(IdColumn::Tvdb, 3001)], Some(1), Some(2)));
        let q = from_args(
            "tvsearch",
            &args("tvdbid=3001&imdbid=tt0100001&rid=2001&tvmazeid=101&tmdbid=401&season=1&ep=2"),
            &on,
        )
        .unwrap();
        assert_eq!(q.ids.len(), 5);
        let q = from_args("movie", &args("imdbid=0200002"), &on).unwrap();
        assert_eq!(q.ids, [(IdColumn::Imdb, 200002)]);
        assert_eq!(from_args("movie", &args("imdbid=tt0200002&tmdbid=501"), &on).unwrap().ids.len(), 2);
        assert_eq!(from_args("tvsearch", &args("tvrageid=2001"), &on).unwrap().ids, [(IdColumn::Tvrage, 2001)]);
        let q = from_args("tvsearch", &args("tvdbid=3001&season=2025&ep=11/10"), &on).unwrap();
        assert_eq!((q.season, q.episode), (Some(2025), Some(1110)));
        assert_eq!(from_args("tvsearch", &args("tvdbid=3001&ep=2"), &on).unwrap().episode, None);
        for (t, q) in [
            ("tvsearch", "tvdbid=0"),
            ("tvsearch", "q=Some+Show&season=1"),
            ("movie", "tvdbid=3001"),
            ("search", "tvdbid=3001"),
            ("music", "imdbid=1"),
        ] {
            assert_eq!(from_args(t, &args(q), &on), None, "{t} {q}");
        }
        let off = IdParams::default();
        assert_eq!(from_args("tvsearch", &args("tvdbid=3001"), &off), None);
        let mut only_tvmaze = MediaConfig::default();
        only_tvmaze.tvmaze.enabled = true;
        let on = id_params(&only_tvmaze, false);
        assert_eq!(from_args("tvsearch", &args("tmdbid=401"), &on), None);
    }

    #[test]
    fn tvdb_offers_only_what_it_fills() {
        let mut cfg = MediaConfig::default();
        cfg.tvmaze.enabled = false;
        cfg.tvdb.api_key = "K".into();
        let on = id_params(&cfg, false);
        let tv = IdSet { tvdb: true, imdb: true, tvmaze: true, tmdb: true, ..IdSet::default() };
        assert_eq!(on, IdParams { tv, movie: IdSet::default() });
    }

    #[test]
    fn attrs_xml() {
        let on = all_on();
        let ids =
            Ids { tvdb: Some(1), tvmaze: Some(2), trakt: Some(3), tmdb: Some(4), imdb: Some(100001), tvrage: Some(6) };
        let tv = MediaAttrs { kind: 1, season: Some(1), episode: Some(2), year: Some(2019), ids, ..Default::default() };
        let xml = tv.xml(&on);
        for name in ["season", "episode", "year", "tvdbid", "tvmazeid", "traktid", "tmdbid", "rageid", "tvrageid"] {
            assert!(xml.contains(&format!("name=\"{name}\"")), "{name}: {xml}");
        }
        assert!(xml.contains(r#"name="imdb" value="0100001""#), "{xml}");
        assert!(xml.contains(r#"name="imdbid" value="0100001""#), "{xml}");

        let daily = MediaAttrs {
            kind: 1,
            season: Some(2024),
            episode: Some(315),
            air_date: Some(20240315),
            ..Default::default()
        };
        let xml = daily.xml(&on);
        assert!(xml.contains(r#"name="tvairdate" value="2024-03-15""#), "{xml}");
        assert!(xml.contains(r#"name="season" value="2024""#), "{xml}");
        assert!(xml.contains(r#"name="episode" value="03/15""#), "{xml}");

        let pack = MediaAttrs { kind: 1, season: Some(3), ..Default::default() };
        let xml = pack.xml(&on);
        assert!(xml.contains("season") && !xml.contains("episode"), "{xml}");

        let off = tv.xml(&IdParams::default());
        assert!(off.contains("season") && !off.contains("tvdbid") && !off.contains("imdb"), "{off}");
    }

    #[test]
    fn id_params_table() {
        let mut default = MediaConfig::default();
        assert!(id_params(&default, false).tv.tvmaze, "tvmaze is on unless turned off");
        default.tvmaze.enabled = false;
        let on = id_params(&default, false);
        assert_eq!(on, IdParams::default());
        let budget = id_params(&default, true);
        assert!(budget.movie.imdb);
        assert!(!budget.tv.imdb);
    }
}
