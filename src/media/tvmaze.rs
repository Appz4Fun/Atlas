//! TVMaze: no key, TV only, and one call usually gives TVDB, IMDb and TVRage ids.

use serde_json::Value;

use crate::config::TvmazeConfig;

use super::http::{Client, Memo, Reply, encode};
use super::resolve::{Answer, Ids, Provider, TitleQuery, Trouble, imdb_of, int_of, year_of};
use super::score::{Candidate, score};
use super::title::Kind;

pub const BIT: u8 = 1;
const DEFAULT_BASE: &str = "https://api.tvmaze.com";
const MAX_CANDIDATES: usize = 10;
pub(crate) const MAX_NAME: usize = 255;

pub struct Tvmaze {
    base: String,
    client: Client,
    /// the single search came to nothing for this query: go on from the full one
    single_done: Memo<()>,
}

impl Tvmaze {
    pub fn new(cfg: &TvmazeConfig) -> Tvmaze {
        let base = cfg.base_url.clone().unwrap_or_else(|| DEFAULT_BASE.to_string());
        Tvmaze { client: Client::new("tvmaze", &base, cfg.requests_per_minute), base, single_done: Memo::new() }
    }
}

fn country_of(show: &Value) -> Option<&str> {
    show.pointer("/network/country/code").or_else(|| show.pointer("/webChannel/country/code")).and_then(Value::as_str)
}

/// the show's score over its name and aliases
fn show_score(q: &TitleQuery<'_>, show: &Value) -> f64 {
    let year = show.get("premiered").and_then(year_of);
    let country = country_of(show);
    let countries: Vec<&str> = country.into_iter().collect();
    let mut names: Vec<&str> = show.get("name").and_then(Value::as_str).into_iter().collect();
    if let Some(akas) = show.pointer("/_embedded/akas").and_then(Value::as_array) {
        names.extend(akas.iter().take(1_000).filter_map(|a| a.get("name").and_then(Value::as_str)));
    }
    names
        .into_iter()
        .filter(|n| n.len() <= MAX_NAME)
        .map(|title| score(Kind::Tv, q.title, q.year, q.country, &Candidate { title, year, countries: &countries }))
        .fold(0.0, f64::max)
}

fn answer_of(show: &Value) -> Answer {
    let externals = show.get("externals");
    let field = |name: &str| externals.and_then(|e| e.get(name));
    let title = show.get("name").and_then(Value::as_str).filter(|n| n.len() <= MAX_NAME).unwrap_or_default();
    Answer::Found {
        ids: Ids {
            tvmaze: show.get("id").and_then(int_of),
            tvrage: field("tvrage").and_then(int_of),
            tvdb: field("thetvdb").and_then(int_of),
            imdb: field("imdb").and_then(imdb_of),
            ..Ids::default()
        },
        title: title.to_string(),
    }
}

impl Tvmaze {
    #[doc(hidden)]
    pub fn client(&self) -> &Client {
        &self.client
    }
}

impl Provider for Tvmaze {
    fn bit(&self) -> u8 {
        BIT
    }

    fn asks(&self, kind: Kind) -> bool {
        kind == Kind::Tv
    }

    fn find(&self, q: &TitleQuery<'_>) -> Result<Answer, Trouble> {
        let key = format!("{}|{:?}|{:?}", q.title, q.year, q.country);
        let query = encode(q.title);
        if self.single_done.get(&key).is_none() {
            let url = format!("{}/singlesearch/shows?q={query}&embed=akas", self.base);
            if let Reply::Json(show) = self.client.get(&url, &[])?
                && show_score(q, &show) > 0.0
            {
                return Ok(answer_of(&show));
            }
            self.single_done.put(&key, ());
        }
        let url = format!("{}/search/shows?q={query}", self.base);
        let reply = self.client.get(&url, &[])?;
        self.single_done.forget(&key);
        let Reply::Json(results) = reply else { return Ok(Answer::NotFound) };
        let Some(results) = results.as_array() else {
            return Err(Trouble::Bad("tvmaze: a search that isnt a list".into()));
        };
        let mut best: Option<(f64, &Value)> = None;
        for show in results.iter().take(MAX_CANDIDATES).filter_map(|r| r.get("show")) {
            let score = show_score(q, show);
            if score > 0.0 && best.is_none_or(|(b, _)| score > b) {
                best = Some((score, show));
            }
        }
        Ok(best.map_or(Answer::NotFound, |(_, show)| answer_of(show)))
    }
}
