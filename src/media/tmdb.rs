//! TMDB: the one that covers movies well with a free key. Search, then a second call for its external ids.

use serde_json::Value;

use crate::config::TmdbConfig;

use super::http::{Client, Memo, Reply, encode};
use super::resolve::{Answer, Ids, Provider, TitleQuery, Trouble, imdb_of, int_of, year_of};
use super::score::{Candidate, score};
use super::title::Kind;
use super::tvmaze::MAX_NAME;

pub const BIT: u8 = 2;
const DEFAULT_BASE: &str = "https://api.themoviedb.org/3";
const MAX_CANDIDATES: usize = 10;

pub struct Tmdb {
    base: String,
    key: String,
    client: Client,
    /// a search's answer, waiting for its external ids
    searched: Memo<(i64, String)>,
}

impl Tmdb {
    pub fn new(cfg: &TmdbConfig) -> Tmdb {
        let base = cfg.base_url.clone().unwrap_or_else(|| DEFAULT_BASE.to_string());
        Tmdb {
            client: Client::new("tmdb", &base, cfg.requests_per_minute),
            base,
            key: cfg.api_key.clone(),
            searched: Memo::new(),
        }
    }

    fn search(&self, q: &TitleQuery<'_>) -> Result<Option<(i64, String)>, Trouble> {
        let query = encode(q.title);
        let url = match (q.kind, q.year) {
            (Kind::Tv, Some(y)) => format!(
                "{}/search/tv?query={query}&page=1&include_adult=false&first_air_date_year={y}&api_key={}",
                self.base,
                encode(&self.key)
            ),
            (Kind::Tv, None) => {
                format!(
                    "{}/search/tv?query={query}&page=1&include_adult=false&api_key={}",
                    self.base,
                    encode(&self.key)
                )
            }
            (Kind::Movie, Some(y)) => format!(
                "{}/search/movie?query={query}&page=1&include_adult=false&year={y}&api_key={}",
                self.base,
                encode(&self.key)
            ),
            (Kind::Movie, None) => {
                format!(
                    "{}/search/movie?query={query}&page=1&include_adult=false&api_key={}",
                    self.base,
                    encode(&self.key)
                )
            }
        };
        let Reply::Json(body) = self.client.get(&url, &[])? else { return Ok(None) };
        let Some(results) = body.get("results").and_then(Value::as_array) else {
            return Err(Trouble::Bad("tmdb: a search with no results".into()));
        };
        let (name_key, original_key, date_key) = match q.kind {
            Kind::Tv => ("name", "original_name", "first_air_date"),
            Kind::Movie => ("title", "original_title", "release_date"),
        };
        let mut best: Option<(f64, i64, String)> = None;
        for result in results.iter().take(MAX_CANDIDATES) {
            let Some(id) = result.get("id").and_then(int_of) else { continue };
            let year = result.get(date_key).and_then(year_of);
            let countries: Vec<&str> = result
                .get("origin_country")
                .and_then(Value::as_array)
                .map_or_else(Vec::new, |c| c.iter().filter_map(Value::as_str).collect());
            for key in [name_key, original_key] {
                let Some(title) = result.get(key).and_then(Value::as_str).filter(|t| t.len() <= MAX_NAME) else {
                    continue;
                };
                let score =
                    score(q.kind, q.title, q.year, q.country, &Candidate { title, year, countries: &countries });
                if score > 0.0 && best.as_ref().is_none_or(|(b, ..)| score > *b) {
                    best = Some((score, id, title.to_string()));
                }
            }
        }
        Ok(best.map(|(_, id, title)| (id, title)))
    }
}

impl Tmdb {
    #[doc(hidden)]
    pub fn client(&self) -> &Client {
        &self.client
    }
}

impl Provider for Tmdb {
    fn bit(&self) -> u8 {
        BIT
    }

    fn asks(&self, _kind: Kind) -> bool {
        true
    }

    fn find(&self, q: &TitleQuery<'_>) -> Result<Answer, Trouble> {
        let key = format!("{:?}|{}|{:?}|{:?}", q.kind, q.title, q.year, q.country);
        let (id, title) = match self.searched.get(&key) {
            Some(found) => found,
            None => {
                let Some(found) = self.search(q)? else { return Ok(Answer::NotFound) };
                self.searched.put(&key, found.clone());
                found
            }
        };
        let path = if q.kind == Kind::Tv { "tv" } else { "movie" };
        let url = format!("{}/{path}/{id}/external_ids?api_key={}", self.base, encode(&self.key));
        let reply = self.client.get(&url, &[])?;
        self.searched.forget(&key);
        let mut ids = Ids { tmdb: Some(id), ..Ids::default() };
        if let Reply::Json(body) = reply {
            ids.imdb = body.get("imdb_id").and_then(imdb_of);
            ids.tvdb = body.get("tvdb_id").and_then(int_of);
            ids.tvrage = body.get("tvrage_id").and_then(int_of);
        }
        Ok(Answer::Found { ids, title })
    }
}
