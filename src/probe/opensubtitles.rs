//! What OpenSubtitles says a probed video is, from its hash alone: a subtitle uploaded for the very same file
//! names the film or episode, with its IMDb and TMDB ids. Asked once per hash.

use std::sync::{Arc, Mutex};

use rusqlite::OptionalExtension;
use serde_json::Value;

use crate::config::{Config, OpenSubtitlesConfig};
use crate::media::http::{Client, Reply, encode, now_secs};
use crate::media::resolve::Trouble;
use crate::postprocess::schema::ShardTable;
use crate::postprocess::step::{Needs, Outcome, ReleaseInfo, Step, StepCtx, StepError, StepId, StepWrite};
use crate::store;

use super::step::MOVIEHASH_FACT;

pub const OPENSUBTITLES_STEP: StepId = StepId(5);

const BASE_URL: &str = "https://api.opensubtitles.com/api/v1";

/// One row per release that had a hash to ask about. Nulls: asked, and nothing matched.
pub const OPENSUBTITLES: ShardTable = ShardTable {
    name: "release_opensubtitles",
    create: "create table if not exists release_opensubtitles (
        release_id   INTEGER PRIMARY KEY,
        moviehash    TEXT NOT NULL,
        imdb_id      INTEGER,
        tmdb_id      INTEGER,
        kind         TEXT,
        title        TEXT,
        year         INTEGER,
        season       INTEGER,
        episode      INTEGER,
        release_name TEXT,
        checked_at   INTEGER NOT NULL
    )",
    indexes: &[(
        "release_opensubtitles_imdb",
        "create index if not exists release_opensubtitles_imdb on release_opensubtitles(imdb_id) where imdb_id is not null",
    )],
    release_key: Some("release_id"),
    added: &[],
};

const SET_MATCH: &str = "insert or replace into release_opensubtitles
    (release_id, moviehash, imdb_id, tmdb_id, kind, title, year, season, episode, release_name, checked_at)
    values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)";

/// The same pin an NFO's id gets: it came from the release's own bytes, soo a title lookup can't replace it.
const SET_IMDB: &str =
    "update media_release set imdb_id = ?2, imdb_from_nfo = 1 where release_id = ?1 and imdb_id is null";

/// What one hash matched.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Feature {
    pub imdb_id: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub kind: Option<String>,
    pub title: Option<String>,
    pub year: Option<i64>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub release_name: Option<String>,
}

/// The film or episode most of the hash's exact matches agree on. Subtitles found by name rather than by
/// hash say nothing about this file, soo only `moviehash_match` ones count.
pub fn best_match(reply: &Value) -> Option<Feature> {
    let mut votes: Vec<(i64, usize, Feature)> = Vec::new();
    for entry in reply.get("data")?.as_array()? {
        let attrs = entry.get("attributes")?;
        if attrs.get("moviehash_match").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let Some(details) = attrs.get("feature_details") else { continue };
        let int = |key: &str| details.get(key).and_then(Value::as_i64).filter(|n| *n > 0);
        let text = |v: Option<&Value>| {
            v.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(|s| s.chars().take(300).collect())
        };
        let Some(imdb) = int("imdb_id") else { continue };
        let kind: Option<String> = text(details.get("feature_type"));
        let is_episode = kind.as_deref().is_some_and(|k| k.eq_ignore_ascii_case("episode"));
        let feature = Feature {
            imdb_id: Some(imdb),
            tmdb_id: int("tmdb_id"),
            title: if is_episode {
                text(details.get("parent_title")).or_else(|| text(details.get("title")))
            } else {
                text(details.get("title"))
            },
            kind,
            year: int("year"),
            season: int("season_number"),
            episode: int("episode_number"),
            release_name: text(attrs.get("release")),
        };
        match votes.iter_mut().find(|(id, _, _)| *id == imdb) {
            Some(vote) => vote.1 += 1,
            None => votes.push((imdb, 1, feature)),
        }
    }
    // most votes, and of those the first seen
    let best = votes.iter().map(|(_, n, _)| *n).max()?;
    votes.into_iter().find(|(_, n, _)| *n == best).map(|(_, _, f)| f)
}

/// A hash as the probe writes it: 16 hex digits.
fn is_hash(text: &str) -> bool {
    text.len() == 16 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

#[derive(Default)]
pub struct OpenSubtitlesStep {
    client: Mutex<Option<(OpenSubtitlesConfig, Arc<Client>)>>,
}

impl OpenSubtitlesStep {
    fn client(&self) -> Option<(String, String, Arc<Client>)> {
        let held = self.client.lock().unwrap_or_else(|e| e.into_inner());
        held.as_ref().map(|(cfg, client)| {
            let base = cfg.base_url.clone().unwrap_or_else(|| BASE_URL.to_string());
            (cfg.api_key.clone(), base, client.clone())
        })
    }

    fn hash_of(ctx: &StepCtx<'_>) -> Result<Option<String>, StepError> {
        if let Some(hash) = ctx.fact(MOVIEHASH_FACT) {
            return Ok(Some(hash.to_string()));
        }
        ctx.conn()
            .query_row("select moviehash from release_probe where release_id = ?1", [ctx.release().id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()
            .map(Option::flatten)
            .map_err(|e| StepError::Retry(e.to_string()))
    }

    fn already_asked(ctx: &StepCtx<'_>, hash: &str) -> Result<bool, StepError> {
        ctx.conn()
            .query_row(
                "select 1 from release_opensubtitles where release_id = ?1 and moviehash = ?2",
                rusqlite::params![ctx.release().id, hash],
                |_| Ok(()),
            )
            .optional()
            .map(|r| r.is_some())
            .map_err(|e| StepError::Retry(e.to_string()))
    }
}

impl Step for OpenSubtitlesStep {
    fn id(&self) -> StepId {
        OPENSUBTITLES_STEP
    }
    fn name(&self) -> &'static str {
        "opensubtitles"
    }
    fn rank(&self) -> u16 {
        130
    }
    fn needs(&self) -> Needs {
        Needs::External
    }
    fn enabled(&self, cfg: &Config) -> bool {
        let wanted = &cfg.media.opensubtitles;
        let on = cfg.archive.enabled && !wanted.api_key.is_empty();
        let mut held = self.client.lock().unwrap_or_else(|e| e.into_inner());
        if !on {
            *held = None;
        } else if held.as_ref().is_none_or(|(had, _)| had != wanted) {
            let base = wanted.base_url.clone().unwrap_or_else(|| BASE_URL.to_string());
            let client = Client::new("opensubtitles", &base, wanted.requests_per_minute);
            *held = Some((wanted.clone(), Arc::new(client)));
        }
        on
    }
    fn applies(&self, release: &ReleaseInfo) -> bool {
        let _ = release;
        true
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        if ctx.stopping() {
            return Err(StepError::Stopped);
        }
        let Some(hash) = Self::hash_of(ctx)?.filter(|h| is_hash(h)) else { return Ok(Outcome::default()) };
        if Self::already_asked(ctx, &hash)? {
            return Ok(Outcome::default());
        }
        let Some((key, base, client)) = self.client() else { return Ok(Outcome::default()) };
        let url = format!("{base}/subtitles?moviehash={}", encode(&hash));
        let reply = match client.get(&url, &[("Api-Key", &key), ("Accept", "application/json")]) {
            Ok(Reply::Json(json)) => best_match(&json),
            Ok(Reply::NotFound) => None,
            Err(Trouble::Paused(until) | Trouble::BadKey(until)) => {
                return Ok(Outcome { wait_until: Some(until), ..Outcome::default() });
            }
            Err(Trouble::Bad(why)) => return Err(StepError::Retry(why)),
        };
        let id = ctx.release().id;
        let f = reply.clone().unwrap_or_default();
        let mut writes = vec![StepWrite::new(
            SET_MATCH,
            vec![
                id.into(),
                hash.into(),
                f.imdb_id.into(),
                f.tmdb_id.into(),
                f.kind.into(),
                f.title.into(),
                f.year.into(),
                f.season.into(),
                f.episode.into(),
                f.release_name.into(),
                now_secs().into(),
            ],
        )];
        if let Some(imdb) = f.imdb_id {
            writes.push(StepWrite::new(SET_IMDB, vec![id.into(), imdb.into()]));
        }
        Ok(Outcome { matched: reply.is_some(), writes, ..Outcome::default() })
    }
}

/// What the release page shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stored {
    pub moviehash: String,
    pub feature: Option<Feature>,
}

pub fn read(conn: &rusqlite::Connection, release_id: i64) -> Option<Stored> {
    let s = store::shard_of_id(release_id);
    conn.query_row(
        &format!(
            "select moviehash, imdb_id, tmdb_id, kind, title, year, season, episode, release_name
             from s{s}.release_opensubtitles where release_id = ?"
        ),
        [release_id],
        |r| {
            let imdb_id: Option<i64> = r.get(1)?;
            let feature = Feature {
                imdb_id,
                tmdb_id: r.get(2)?,
                kind: r.get(3)?,
                title: r.get(4)?,
                year: r.get(5)?,
                season: r.get(6)?,
                episode: r.get(7)?,
                release_name: r.get(8)?,
            };
            Ok(Stored { moviehash: r.get(0)?, feature: imdb_id.map(|_| feature) })
        },
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_hash_matches_vote() {
        let reply = json!({"data": [
            {"attributes": {"moviehash_match": false, "feature_details": {"imdb_id": 1, "title": "By name"}}},
            {"attributes": {"moviehash_match": true, "release": "The.Film.2001.1080p.BluRay.x264-GRP",
                "feature_details": {"imdb_id": 133093, "tmdb_id": 603, "title": "The Film", "year": 2001,
                    "feature_type": "Movie"}}},
            {"attributes": {"moviehash_match": true, "feature_details": {"imdb_id": 7, "title": "Other"}}},
            {"attributes": {"moviehash_match": true, "feature_details": {"imdb_id": 133093, "title": "The Film"}}},
        ]});
        let f = best_match(&reply).unwrap();
        assert_eq!(f.imdb_id, Some(133093));
        assert_eq!(f.tmdb_id, Some(603));
        assert_eq!(f.title.as_deref(), Some("The Film"));
        assert_eq!(f.release_name.as_deref(), Some("The.Film.2001.1080p.BluRay.x264-GRP"));
        assert_eq!(best_match(&json!({"data": [reply["data"][0].clone()]})), None);
        assert_eq!(best_match(&json!({"total_count": 0})), None);
    }

    #[test]
    fn an_episode_is_named_by_its_show() {
        let reply = json!({"data": [{"attributes": {"moviehash_match": true, "feature_details": {
            "imdb_id": 11, "title": "Pilot", "parent_title": "The Show", "feature_type": "Episode",
            "season_number": 1, "episode_number": 2}}}]});
        let f = best_match(&reply).unwrap();
        assert_eq!((f.title.as_deref(), f.season, f.episode), (Some("The Show"), Some(1), Some(2)));
    }

    #[test]
    fn hashes_are_checked_before_they_go_in_a_url() {
        assert!(is_hash("8e245d9679d31e12"));
        assert!(!is_hash("8e245d9679d31e1"));
        assert!(!is_hash("8e245d9679d31e1&"));
    }
}
