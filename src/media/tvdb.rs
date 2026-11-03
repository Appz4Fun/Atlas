//! TheTVDB v4: `tvdbid` straight from the source. Logs in once for a month's token, then searches in one call.

use std::sync::Mutex;

use serde_json::{Value, json};

use crate::config::TvdbConfig;

use super::http::{Client, Reply, encode, now_secs};
use super::resolve::{Answer, Ids, Provider, TitleQuery, Trouble, imdb_of, int_of};
use super::score::{Candidate, score};
use super::title::Kind;
use super::tvmaze::MAX_NAME;

pub const BIT: u8 = 8;
const DEFAULT_BASE: &str = "https://api4.thetvdb.com/v4";
const MAX_CANDIDATES: usize = 10;
const MAX_ALIASES: usize = 1_000;
/// A token lasts a month: a new one a few days early, soo none expires in the middle of a lookup.
const TOKEN_SECS: i64 = 25 * 86_400;

pub struct Tvdb {
    base: String,
    key: String,
    pin: String,
    client: Client,
    token: Mutex<Option<(String, i64)>>,
}

impl Tvdb {
    pub fn new(cfg: &TvdbConfig) -> Tvdb {
        let base = cfg.base_url.clone().unwrap_or_else(|| DEFAULT_BASE.to_string());
        Tvdb {
            client: Client::new("tvdb", &base, cfg.requests_per_minute),
            base,
            key: cfg.api_key.clone(),
            pin: cfg.pin.clone(),
            token: Mutex::new(None),
        }
    }

    /// A series' own record, for a show TMDB doesn't have. None: TheTVDB doesn't either.
    pub fn series(&self, id: i64) -> Result<Option<Value>, Trouble> {
        let token = self.bearer()?;
        let url = format!("{}/series/{id}/extended", self.base);
        let header = format!("Bearer {token}");
        match self.client.get(&url, &[("Authorization", header.as_str())]) {
            Ok(Reply::Json(body)) => Ok(body.get("data").cloned()),
            Ok(Reply::NotFound) => Ok(None),
            Err(trouble) => {
                if matches!(trouble, Trouble::BadKey(_)) {
                    *self.token.lock().unwrap() = None;
                }
                Err(trouble)
            }
        }
    }

    #[doc(hidden)]
    pub fn client(&self) -> &Client {
        &self.client
    }

    fn bearer(&self) -> Result<String, Trouble> {
        let now = now_secs();
        if let Some((token, at)) = self.token.lock().unwrap().as_ref()
            && now - at < TOKEN_SECS
        {
            return Ok(token.clone());
        }
        let url = format!("{}/login", self.base);
        let mut login = json!({"apikey": self.key});
        if !self.pin.is_empty() {
            login["pin"] = json!(self.pin);
        }
        let Reply::Json(body) = self.client.post_json(&url, &login)? else {
            return Err(Trouble::Bad("tvdb: no login route".into()));
        };
        let Some(token) = body.pointer("/data/token").and_then(Value::as_str).filter(|t| !t.is_empty()) else {
            return Err(Trouble::Bad("tvdb: a login with no token".into()));
        };
        *self.token.lock().unwrap() = Some((token.to_string(), now));
        Ok(token.to_string())
    }
}

/// TVDB writes countries in three letters, and a name only ever carries these.
fn two_letters(country: &str) -> String {
    match country.to_ascii_lowercase().as_str() {
        "usa" => "US".into(),
        "gbr" => "GB".into(),
        "aus" => "AU".into(),
        "nzl" => "NZ".into(),
        "can" => "CA".into(),
        "nld" => "NL".into(),
        other => other.to_ascii_uppercase(),
    }
}

fn candidate_score(q: &TitleQuery<'_>, item: &Value) -> f64 {
    let year = item.get("year").and_then(int_of).and_then(|y| i32::try_from(y).ok());
    let country = item.get("country").and_then(Value::as_str).filter(|c| !c.is_empty()).map(two_letters);
    let countries: Vec<&str> = country.as_deref().into_iter().collect();
    let aliases = item.get("aliases").and_then(Value::as_array);
    let translations = item.get("translations").and_then(Value::as_object);
    item.get("name")
        .and_then(Value::as_str)
        .into_iter()
        .chain(item.get("name_translated").and_then(Value::as_str))
        .chain(aliases.into_iter().flatten().take(MAX_ALIASES).filter_map(Value::as_str))
        .chain(translations.into_iter().flat_map(|t| t.values()).take(MAX_ALIASES).filter_map(Value::as_str))
        .filter(|name| name.len() <= MAX_NAME)
        .map(|title| score(q.kind, q.title, q.year, q.country, &Candidate { title, year, countries: &countries }))
        .fold(0.0, f64::max)
}

fn answer_of(item: &Value) -> Answer {
    let mut ids = Ids { tvdb: item.get("tvdb_id").and_then(int_of), ..Ids::default() };
    let remote = item.get("remote_ids").and_then(Value::as_array);
    for r in remote.into_iter().flatten().take(MAX_ALIASES) {
        let Some(id) = r.get("id") else { continue };
        match r.get("sourceName").and_then(Value::as_str).unwrap_or_default() {
            "IMDB" => ids.imdb = ids.imdb.or(imdb_of(id)),
            "TheMovieDB.com" => ids.tmdb = ids.tmdb.or(int_of(id)),
            "TV Maze" => ids.tvmaze = ids.tvmaze.or(int_of(id)),
            _ => {}
        }
    }
    let title = item.get("name").and_then(Value::as_str).filter(|n| n.len() <= MAX_NAME).unwrap_or_default();
    Answer::Found { ids, title: title.to_string() }
}

impl Provider for Tvdb {
    fn bit(&self) -> u8 {
        BIT
    }

    /// `tvdbid` is a TV search's parameter, and TMDB covers movies.
    fn asks(&self, kind: Kind) -> bool {
        kind == Kind::Tv
    }

    fn find(&self, q: &TitleQuery<'_>) -> Result<Answer, Trouble> {
        let token = self.bearer()?;
        let url = format!("{}/search?query={}&type=series&limit={MAX_CANDIDATES}", self.base, encode(q.title));
        let header = format!("Bearer {token}");
        let reply = match self.client.get(&url, &[("Authorization", header.as_str())]) {
            Ok(reply) => reply,
            Err(trouble) => {
                // a token it stopped taking: the next try logs in again
                if matches!(trouble, Trouble::BadKey(_)) {
                    *self.token.lock().unwrap() = None;
                }
                return Err(trouble);
            }
        };
        let Reply::Json(body) = reply else { return Ok(Answer::NotFound) };
        let Some(results) = body.get("data").and_then(Value::as_array) else {
            return Err(Trouble::Bad("tvdb: a search with no data".into()));
        };
        let mut best: Option<(f64, &Value)> = None;
        for item in results.iter().take(MAX_CANDIDATES) {
            let score = candidate_score(q, item);
            if score > 0.0 && best.is_none_or(|(b, _)| score > b) {
                best = Some((score, item));
            }
        }
        Ok(best.map_or(Answer::NotFound, |(_, item)| answer_of(item)))
    }
}
