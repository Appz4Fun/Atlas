//! What OpenSubtitles says a probed video is, from its hash alone: a subtitle uploaded for the very same file
//! names the film or episode, with its IMDb and TMDB ids. Asked once per hash.

use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::config::{Config, OpenSubtitlesConfig};
use crate::media::http::{Client, Reply, encode, now_secs};
use crate::media::resolve::Trouble;
use crate::postprocess::schema::ShardTable;
use crate::postprocess::step::{
    Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId, StepMutation, StepWrite, mask,
};
use crate::store;

use super::step::MOVIEHASH_FACT;

pub const OPENSUBTITLES_STEP: StepId = StepId(5);
/// In the media steps' range: it runs after `media_parse` makes the release's row.
pub const OPENSUBTITLES_IMDB_STEP: StepId = StepId(35);

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

impl Feature {
    pub fn is_movie(&self) -> bool {
        self.kind.as_deref().is_some_and(|k| k.eq_ignore_ascii_case("movie"))
    }
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
        ctx.data().probe_moviehash(ctx.release().id)
    }

    fn already_asked(ctx: &StepCtx<'_>, hash: &str) -> Result<bool, StepError> {
        ctx.data().opensubtitles_asked(ctx.release().id, hash)
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
        let reply = if let Some(cached) = crate::raw_store::get_opensubtitles(&hash)
            .map_err(|_| StepError::Retry("OpenSubtitles cache read failed".into()))? {
            cached
        } else {
            let Some((key, base, client)) = self.client() else { return Ok(Outcome::default()) };
            let url = format!("{base}/subtitles?moviehash={}", encode(&hash));
            match client.get(&url, &[("Api-Key", &key), ("Accept", "application/json")]) {
                Ok(Reply::Json(json)) => {
                    let matched = best_match(&json);
                    crate::raw_store::put_opensubtitles(&hash, matched.as_ref())
                        .map_err(|_| StepError::Retry("OpenSubtitles cache write failed".into()))?;
                    matched
                }
                Ok(Reply::NotFound) => {
                    crate::raw_store::put_opensubtitles(&hash, None)
                        .map_err(|_| StepError::Retry("OpenSubtitles cache write failed".into()))?;
                    None
                }
                Err(Trouble::Paused(until) | Trouble::BadKey(until)) => {
                    return Ok(Outcome { wait_until: Some(until), ..Outcome::default() });
                }
                Err(Trouble::Bad(why)) => return Err(StepError::Retry(why)),
            }
        };
        let film_imdb = reply.as_ref().and_then(|feature| feature.imdb_id.filter(|_| feature.is_movie()));
        let matched = reply.is_some();
        let writes = vec![StepWrite::for_mutation(ctx.release().id,
            StepMutation::OpenSubtitlesMatch { moviehash: hash, feature: reply, checked_at: now_secs() })];
        // a film's id goes to media_release in its own step, after the release has a row there
        let rerun = if film_imdb.is_some() { mask(OPENSUBTITLES_IMDB_STEP) } else { 0 };
        Ok(Outcome { matched, writes, rerun, ..Outcome::default() })
    }
}

/// A film's IMDb id from its hash match, into `media_release`, once the release has a row there: a name found
/// later (an NFO's, say) makes the row after the match was asked for. An episode's own id stays out, since
/// `media_release` holds its show's and an episode's would hide it from searches by the show.
pub struct OpenSubtitlesImdb;

impl Step for OpenSubtitlesImdb {
    fn id(&self) -> StepId {
        OPENSUBTITLES_IMDB_STEP
    }
    fn name(&self) -> &'static str {
        "opensubtitles_imdb"
    }
    fn rank(&self) -> u16 {
        505
    }
    fn needs(&self) -> Needs {
        Needs::Database
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.media.parse && !cfg.media.opensubtitles.api_key.is_empty()
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: true, on_growth: true }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let id = ctx.release().id;
        let imdb = ctx.data().opensubtitles_movie_imdb(id)?;
        let Some(imdb) = imdb else { return Ok(Outcome::default()) };
        let writes = vec![StepWrite::for_mutation(id, StepMutation::PinOpenSubtitlesMovieImdb { imdb_id: imdb })];
        Ok(Outcome { matched: true, writes, ..Outcome::default() })
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
        assert!(!f.is_movie(), "its own id stays out of media_release");
    }

    #[test]
    fn hashes_are_checked_before_they_go_in_a_url() {
        assert!(is_hash("8e245d9679d31e12"));
        assert!(!is_hash("8e245d9679d31e1"));
        assert!(!is_hash("8e245d9679d31e1&"));
    }

    #[test]
    fn cache_read_failure_keeps_the_actual_step_retryable_without_fetching() {
        use std::net::TcpListener;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        let home = tempfile::tempdir().unwrap();
        let unavailable = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = unavailable.local_addr().unwrap().port();
        drop(unavailable);
        let http = TcpListener::bind("127.0.0.1:0").unwrap();
        http.set_nonblocking(true).unwrap();
        let config = format!(
            "[storage]\nbackend='postgres'\nurl='host=127.0.0.1 port={port} dbname=example user=example connect_timeout=1'\nschema='cache_fixture'\nmax_connections=1\n\
             [media.opensubtitles]\napi_key=''\nbase_url='http://127.0.0.1:{}'\n",
            http.local_addr().unwrap().port()
        );
        std::fs::write(home.path().join("config.toml"), config).unwrap();
        let sentinel = b"no native SQLite cache";
        std::fs::write(home.path().join("atlas.raw.db"), sentinel).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "probe::opensubtitles::tests::cache_error_child", "--nocapture"])
            .env("ATLAS_HOME", home.path())
            .env("ATLAS_NO_KEYRING", "1")
            .env("ATLAS_CACHE_STEP_CHILD", "1")
            .env_remove("ATLAS_PG_TEST_URL")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            if child.try_wait().unwrap().is_some() { break; }
            if Instant::now() >= until {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("owned step child did not stop");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "actual step child failed: {}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert_eq!(std::fs::read(home.path().join("atlas.raw.db")).unwrap(), sentinel);
        assert!(matches!(http.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 2);
    }

    #[test]
    #[ignore = "only the owned parent fixture runs this configured step child"]
    fn cache_error_child() {
        use std::sync::atomic::AtomicBool;
        use std::time::{Duration, Instant};
        assert_eq!(std::env::var("ATLAS_CACHE_STEP_CHILD").unwrap(), "1");
        assert!(crate::raw_store::native_pg_opensubtitles_cache_tests::open_test_native_owner().is_err(),
            "unavailable Step destination admitted a canonical owner");
        crate::raw_store::set_test_raw_path(Some(crate::paths::app_dir().join("atlas.raw.db")));
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(OPENSUBTITLES.create).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let bodies = crate::postprocess::bodies::BodyFetcher::new(
            Arc::new(crate::nntp::Pool::new(&[])),
            runtime.handle().clone(),
            Arc::new(AtomicBool::new(false)),
            crate::config::PostprocessConfig::default(),
            Arc::new(crate::postprocess::stats::Stats::default()),
        );
        let mut context = StepCtx {
            release: ReleaseInfo { id: 1, name: "Example.Release".into(), group: "example.group".into(), ..Default::default() },
            data: crate::postprocess::data::StepData::LegacySqlite(&conn),
            facts: vec![(MOVIEHASH_FACT, "abcdef0123456789".into())],
            bodies: &bodies,
            used_bodies: 0,
            deadline: Instant::now() + Duration::from_secs(2),
            lane_can_fetch: false,
            crcs: Vec::new(),
            pictures: false,
        };
        let step = OpenSubtitlesStep::default();
        assert!(step.client().is_none(), "provider must stay disabled");
        let Err(StepError::Retry(message)) = step.run(&mut context) else {
            panic!("cache failure did not produce a retryable actual Step outcome");
        };
        assert!(!message.contains("host="));
        assert!(!message.contains("port="));
        assert_eq!(conn.query_row("select count(*) from release_opensubtitles", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        assert!(context.crcs.is_empty());
    }
}
