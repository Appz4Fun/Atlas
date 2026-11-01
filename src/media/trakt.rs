//! Trakt: adds `traktid`, and a fallback search for both kinds.

use serde_json::Value;

use crate::config::TraktConfig;

use super::http::{Client, Reply, encode};
use super::resolve::{Answer, Ids, Provider, TitleQuery, Trouble, imdb_of, int_of};
use super::score::{Candidate, score};
use super::title::Kind;
use super::tvmaze::MAX_NAME;

pub const BIT: u8 = 4;
const DEFAULT_BASE: &str = "https://api.trakt.tv";
const MAX_CANDIDATES: usize = 10;

pub struct Trakt {
    base: String,
    client_id: String,
    client: Client,
}

impl Trakt {
    pub fn new(cfg: &TraktConfig) -> Trakt {
        let base = cfg.base_url.clone().unwrap_or_else(|| DEFAULT_BASE.to_string());
        Trakt { client: Client::new("trakt", &base, cfg.requests_per_minute), base, client_id: cfg.client_id.clone() }
    }
}

impl Trakt {
    #[doc(hidden)]
    pub fn client(&self) -> &Client {
        &self.client
    }
}

impl Provider for Trakt {
    fn bit(&self) -> u8 {
        BIT
    }

    fn asks(&self, _kind: Kind) -> bool {
        true
    }

    fn find(&self, q: &TitleQuery<'_>) -> Result<Answer, Trouble> {
        let field = if q.kind == Kind::Tv { "show" } else { "movie" };
        let url = format!("{}/search/{field}?query={}", self.base, encode(q.title));
        let headers = [("trakt-api-version", "2"), ("trakt-api-key", self.client_id.as_str())];
        let Reply::Json(body) = self.client.get(&url, &headers)? else { return Ok(Answer::NotFound) };
        let Some(results) = body.as_array() else {
            return Err(Trouble::Bad("trakt: a search that isnt a list".into()));
        };
        let mut best: Option<(f64, &Value, &str)> = None;
        for item in results.iter().take(MAX_CANDIDATES).filter_map(|r| r.get(field)) {
            let Some(title) = item.get("title").and_then(Value::as_str).filter(|t| t.len() <= MAX_NAME) else {
                continue;
            };
            let year = item.get("year").and_then(int_of).and_then(|y| i32::try_from(y).ok());
            let score = score(q.kind, q.title, q.year, q.country, &Candidate { title, year, countries: &[] });
            if score > 0.0 && best.is_none_or(|(b, ..)| score > b) {
                best = Some((score, item, title));
            }
        }
        let Some((_, item, title)) = best else { return Ok(Answer::NotFound) };
        let id = |name: &str| item.pointer(&format!("/ids/{name}")).and_then(int_of);
        Ok(Answer::Found {
            ids: Ids {
                trakt: id("trakt"),
                tvdb: id("tvdb"),
                tmdb: id("tmdb"),
                tvrage: id("tvrage"),
                imdb: item.pointer("/ids/imdb").and_then(imdb_of),
                ..Ids::default()
            },
            title: title.to_string(),
        })
    }
}
