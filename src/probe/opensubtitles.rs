//! What OpenSubtitles says a probed video is, from its hash alone: a subtitle uploaded for the very same file
//! names the film or episode, with its IMDb and TMDB ids. Asked once per hash.

use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::config::{Config, OpenSubtitlesConfig};
use crate::media::http::{Client, Reply, encode, now_secs};
use crate::media::resolve::Trouble;
use crate::postprocess::step::{
    Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId, StepMutation, StepWrite, mask,
};

use super::step::MOVIEHASH_FACT;

pub const OPENSUBTITLES_STEP: StepId = StepId(5);
/// In the media steps' range: it runs after `media_parse` makes the release's row.
pub const OPENSUBTITLES_IMDB_STEP: StepId = StepId(35);

const BASE_URL: &str = "https://api.opensubtitles.com/api/v1";

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
        let reply = if let Some(cached) = crate::raw_store::get_opensubtitles(&hash).map_err(StepError::from_cache)? {
            cached
        } else {
            let Some((key, base, client)) = self.client() else { return Ok(Outcome::default()) };
            let url = format!("{base}/subtitles?moviehash={}", encode(&hash));
            match client.get(&url, &[("Api-Key", &key), ("Accept", "application/json")]) {
                Ok(Reply::Json(json)) => {
                    let matched = best_match(&json);
                    crate::raw_store::put_opensubtitles(&hash, matched.as_ref()).map_err(StepError::from_cache)?;
                    matched
                }
                Ok(Reply::NotFound) => {
                    crate::raw_store::put_opensubtitles(&hash, None).map_err(StepError::from_cache)?;
                    None
                }
                Err(Trouble::Paused(until) | Trouble::BadKey(until)) => {
                    return Ok(Outcome { wait_until: Some(until), ..Outcome::default() });
                }
                Err(Trouble::Bad(why)) => return Err(StepError::Retry(why)),
                Err(Trouble::Cache(error)) => return Err(StepError::from_cache(error.into_error())),
            }
        };
        let film_imdb = reply.as_ref().and_then(|feature| feature.imdb_id.filter(|_| feature.is_movie()));
        let matched = reply.is_some();
        let writes = vec![StepWrite::for_mutation(
            ctx.release().id,
            StepMutation::OpenSubtitlesMatch { moviehash: hash, feature: reply, checked_at: now_secs() },
        )];
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

    fn schema() -> crate::pg::testing::Schema {
        crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required")
    }

    fn run_step(runtime: &crate::pg::indexer_runtime::Runtime, id: i64, hash: &str) -> Result<Outcome, StepError> {
        use std::{
            sync::atomic::AtomicBool,
            time::{Duration, Instant},
        };
        let reads = crate::pg::stage_reads::StepReads::for_owner(&runtime.owner).unwrap();
        let executor = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let bodies = crate::postprocess::bodies::BodyFetcher::new(
            Arc::new(crate::nntp::Pool::new(&[])),
            executor.handle().clone(),
            Arc::new(AtomicBool::new(false)),
            crate::config::PostprocessConfig::default(),
            Arc::new(crate::postprocess::stats::Stats::default()),
        );
        let mut context = StepCtx {
            release: ReleaseInfo {
                id,
                name: "Example.Release".into(),
                group: "example.group".into(),
                ..Default::default()
            },
            data: crate::postprocess::data::StepData::Native(&reads),
            facts: vec![(MOVIEHASH_FACT, hash.into())],
            bodies: &bodies,
            used_bodies: 0,
            deadline: Instant::now() + Duration::from_secs(5),
            lane_can_fetch: false,
            crcs: Vec::new(),
            pictures: false,
        };
        let step = OpenSubtitlesStep::default();
        assert!(step.client().is_none(), "provider stays disabled: only the guarded native cache may answer");
        let result = step.run(&mut context);
        assert!(context.crcs.is_empty());
        assert_eq!(context.used_bodies, 0);
        result
    }

    fn release_id(runtime: &crate::pg::indexer_runtime::Runtime) -> i64 {
        crate::pg::stage_queue_tests::push(runtime);
        runtime.owner.with(|client| Ok(client.query_one("select id from releases", &[])?.get(0))).unwrap()
    }

    #[test]
    #[ignore = "requires explicit isolated PostgreSQL for owned OpenSubtitles assertions"]
    fn missing_native_owner_is_fatal_without_fetching_or_cache_fallback() {
        let schema = schema();
        let runtime = crate::pg::stage_queue_tests::runtime(&schema);
        let id = release_id(&runtime);
        // The actual inventory is available. Only the cache attempt scope is
        // absent, so this exercises the step's guarded natural-cache failure.
        let Err(StepError::Native(error)) = run_step(&runtime, id, "abcdef0123456789") else {
            panic!("an unbound cache must preserve the actual Step storage error")
        };
        assert_eq!(crate::pg::outage::classify(&error), crate::pg::outage::Failure::Fatal);
        assert!(!error.to_string().contains("host="));
        assert!(!error.to_string().contains("port="));
        runtime
            .owner
            .with(|client| {
                assert_eq!(client.query_one("select count(*) from release_opensubtitles", &[])?.get::<_, i64>(0), 0);
                assert_eq!(client.query_one("select count(*) from ext_opensubtitles", &[])?.get::<_, i64>(0), 0);
                Ok(())
            })
            .unwrap();
        runtime.owner.close();
    }

    #[test]
    #[ignore = "requires explicit isolated PostgreSQL for owned OpenSubtitles assertions"]
    fn owned_cache_positive_and_negative_matches_produce_closed_results_without_provider() {
        let schema = schema();
        let runtime = crate::pg::stage_queue_tests::runtime(&schema);
        let id = release_id(&runtime);
        let scope = runtime.db.bind_native_cache_scope().unwrap().expect("genuine owned cache scope");
        let hash = "abcdef0123456789";
        let feature = Feature {
            imdb_id: Some(133093),
            tmdb_id: Some(603),
            kind: Some("Movie".into()),
            title: Some("The Film".into()),
            ..Default::default()
        };
        crate::raw_store::put_opensubtitles(hash, Some(&feature)).unwrap();
        let result = run_step(&runtime, id, hash).unwrap();
        assert!(result.matched);
        assert_eq!(result.rerun, mask(OPENSUBTITLES_IMDB_STEP));
        assert_eq!(result.writes.len(), 1);
        assert!(
            matches!(result.writes[0].native_mutation_for(id).unwrap(),StepMutation::OpenSubtitlesMatch {moviehash,feature:Some(found),..} if moviehash==hash && found==&feature)
        );
        assert!(result.writes[0].native_mutation_for(id + 1).is_err());
        let negative = "0000000000000001";
        crate::raw_store::put_opensubtitles(negative, None).unwrap();
        let result = run_step(&runtime, id, negative).unwrap();
        assert!(!result.matched);
        assert_eq!(result.rerun, 0);
        assert_eq!(result.writes.len(), 1);
        assert!(
            matches!(result.writes[0].native_mutation_for(id).unwrap(),StepMutation::OpenSubtitlesMatch {moviehash,feature:None,..} if moviehash==negative)
        );
        runtime
            .owner
            .with(|client| {
                client.execute(
                    "insert into release_opensubtitles(release_id,moviehash,checked_at) values($1,$2,1)",
                    &[&id, &negative],
                )?;
                Ok(())
            })
            .unwrap();
        let result = run_step(&runtime, id, negative).unwrap();
        assert!(result.writes.is_empty(), "already asked hashes do not fetch or rewrite");
        drop(scope);
        runtime.owner.close();
    }

    #[test]
    #[ignore = "requires explicit isolated PostgreSQL for genuine owned backend loss"]
    fn owned_pg_loss_preserves_actual_step_unavailability_without_fetching() {
        use crate::pg::outage::{AttemptOutcome, Failure, SessionUnavailable};
        use std::time::{Duration, Instant};
        let schema = schema();
        let runtime = crate::pg::stage_queue_tests::runtime(&schema);
        let id = release_id(&runtime);
        let scope = runtime.db.bind_native_cache_scope().unwrap().expect("genuine owned cache scope");
        let (pid, started) = runtime
            .owner
            .with(|client| {
                let row = client.query_one(
                    "select pid,backend_start::text from pg_stat_activity where pid=pg_backend_pid()",
                    &[],
                )?;
                Ok((row.try_get::<_, i32>(0)?, row.try_get::<_, String>(1)?))
            })
            .unwrap();
        // Build the read capability before killing its original session. No
        // replacement connection can satisfy this attempt's inventory/cache.
        let reads = crate::pg::stage_reads::StepReads::for_owner(&runtime.owner).unwrap();
        let mut observer = schema.pool.get().unwrap();
        let killed=observer.query("select pg_terminate_backend(pid) from pg_stat_activity where pid=$1 and backend_start=$2::text::timestamptz and usename=current_user and datname=current_database()",&[&pid,&started]).unwrap();
        assert_eq!(killed.len(), 1, "only the actual owned backend identity may be stopped");
        assert!(killed[0].get::<_, bool>(0));
        let until = Instant::now() + Duration::from_secs(5);
        while observer
            .query_one(
                "select exists(select 1 from pg_stat_activity where pid=$1 and backend_start=$2::text::timestamptz)",
                &[&pid, &started],
            )
            .unwrap()
            .get::<_, bool>(0)
        {
            assert!(Instant::now() < until, "owned backend did not end");
            std::thread::sleep(Duration::from_millis(10));
        }
        // The real Step first reads its already-asked inventory. That failed
        // read must retain driver/session provenance before any cache/provider.
        let executor = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let bodies = crate::postprocess::bodies::BodyFetcher::new(
            Arc::new(crate::nntp::Pool::new(&[])),
            executor.handle().clone(),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            crate::config::PostprocessConfig::default(),
            Arc::new(crate::postprocess::stats::Stats::default()),
        );
        let mut context = StepCtx {
            release: ReleaseInfo { id, ..Default::default() },
            data: crate::postprocess::data::StepData::Native(&reads),
            facts: vec![(MOVIEHASH_FACT, "abcdef0123456789".into())],
            bodies: &bodies,
            used_bodies: 0,
            deadline: Instant::now() + Duration::from_secs(5),
            lane_can_fetch: false,
            crcs: Vec::new(),
            pictures: false,
        };
        let step = OpenSubtitlesStep::default();
        assert!(step.client().is_none());
        let Err(StepError::Native(error)) = step.run(&mut context) else {
            panic!("real owned session loss must preserve the actual Step cause")
        };
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<SessionUnavailable>() || cause.downcast_ref::<postgres::Error>().is_some()),
            "native error must retain real driver/session provenance"
        );
        assert_eq!(crate::pg::outage::classify(&error), Failure::Unavailable);
        assert_eq!(runtime.owner.failure(), Some(Failure::Unavailable));
        let cache_error = crate::raw_store::get_opensubtitles("abcdef0123456789").unwrap_err();
        assert_eq!(
            crate::pg::outage::classify(&cache_error),
            Failure::Unavailable,
            "the original bound cache also refuses the lost owner instead of returning a miss"
        );
        assert!(matches!(AttemptOutcome::from_exit(1, runtime.owner.failure()), AttemptOutcome::Unavailable));
        assert!(context.crcs.is_empty());
        assert_eq!(context.used_bodies, 0);
        assert_eq!(observer.query_one("select count(*) from release_opensubtitles", &[]).unwrap().get::<_, i64>(0), 0);
        assert_eq!(observer.query_one("select count(*) from ext_opensubtitles", &[]).unwrap().get::<_, i64>(0), 0);
        drop(scope);
        runtime.owner.close();
    }
}
