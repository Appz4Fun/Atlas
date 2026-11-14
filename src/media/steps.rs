//! The steps: a title from a name, its IDs from a provider, and a movie's IMDb id from its NFO.

use std::sync::{Arc, LazyLock};

use regex::bytes::RegexBuilder;

use crate::archive::select::nfo_sources;
use crate::catalog_facts::SegmentPick;
use crate::category::{self, Decision};
use crate::config::Config;
use crate::postprocess::step::{
    BodyRequest, Got, Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId, StepMutation, StepWrite,
};
use crate::probe::nfo_store;

use super::Media;
use super::allocine::allocine_id;
use super::http::now_secs;
use super::parse::{Parsed, parse_movie, parse_tv};
use super::query::MAX_TITLE;
use super::resolve::{Ids, Resolved, TitleQuery, Trouble};
use super::title::{Kind, normalize, split_country, title_key};


pub const NFO_READ: usize = 65_536;

static IMDB_IN_NFO: LazyLock<regex::bytes::Regex> =
    LazyLock::new(|| RegexBuilder::new(r"(?i)(?:imdb\.com/title/)?\btt([0-9]{7,8})\b").unicode(false).build().unwrap());

/// The first IMDb id stated in an NFO.
pub fn imdb_in_nfo(body: &[u8]) -> Option<i64> {
    let c = IMDB_IN_NFO.captures(body)?;
    std::str::from_utf8(c.get(1)?.as_bytes()).ok()?.parse().ok()
}

/// What this part acts on. A group hint alone isn't evidence of a title.
pub fn media_target(category: i64, rule: &str, name: &str) -> Option<(Kind, Parsed)> {
    if rule.starts_with("group_name_") {
        return None;
    }
    let parsed = if category::is_tv(category) {
        parse_tv(name).map(|p| (Kind::Tv, p))
    } else if category::is_movie(category) {
        parse_movie(name).map(|p| (Kind::Movie, p))
    } else {
        None
    };
    // a poster chooses the name: nothing longer than this is a title, and none reaches a provider or the cache
    parsed.filter(|(_, p)| p.title.len() <= MAX_TITLE)
}

pub(crate) fn key_of(kind: Kind, p: &Parsed) -> i64 {
    title_key(kind, &normalize(&p.title), p.year)
}

fn name_of(r: &ReleaseInfo) -> &str {
    r.display_name.as_deref().unwrap_or(&r.name)
}

fn target_of(media: &Media, r: &ReleaseInfo) -> Option<(Kind, Parsed)> {
    // a spot's title is what its spotter typed, which the name rules often don't read as a movie or show: the
    // spotter's kind says which it is (`spotnet::category`)
    if let Some(kind) = r.spot_kind {
        let category = match kind {
            crate::spotnet::Kind::Movie => 2000,
            crate::spotnet::Kind::Series => 5000,
        };
        return media_target(category, "spot_kind", name_of(r));
    }
    let d: Decision = category::classify_release(r, &media.category_options());
    media_target(d.category, d.rule, name_of(r))
}

pub struct MediaParse(pub Arc<Media>);

impl Step for MediaParse {
    fn id(&self) -> StepId {
        StepId(32)
    }
    fn name(&self) -> &'static str {
        "media_parse"
    }
    fn rank(&self) -> u16 {
        500
    }
    fn needs(&self) -> Needs {
        Needs::Database
    }
    fn enabled(&self, cfg: &Config) -> bool {
        self.0.configure(cfg);
        cfg.media.parse
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: true, on_growth: true }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let r = ctx.release();
        let id = r.id;
        let from_facts = ctx.fact("category").and_then(|c| c.parse::<i64>().ok()).zip(ctx.fact("category_rule"));
        let target = match from_facts {
            Some((category, rule)) => media_target(category, rule, name_of(r)),
            None => target_of(&self.0, r),
        };
        let Some((kind, p)) = target else {
            let stale = ctx.data().media_exists(id)?;
            let writes = if stale { vec![StepWrite::for_mutation(id, StepMutation::ClearMedia)] } else { Vec::new() };
            return Ok(Outcome { writes, ..Outcome::default() });
        };
        let key = key_of(kind, &p);
        let write = StepWrite::for_mutation(id, StepMutation::ParseMedia { kind, title_key: key, parsed: p });
        let mut writes = vec![write];
        // the title a probed file's own tags name, put on the row as it's made: a release named only after its probe
        // ran had no row for the probe to pin them on, and the probe isn't run again for a rename
        writes.extend(crate::probe::step::title_writes(id, ctx.data().saved_probe_media(id)?.as_ref()));
        Ok(Outcome { matched: true, writes, ..Outcome::default() })
    }
}

fn found_write(id: i64, key: i64, ids: &Ids) -> StepWrite {
    StepWrite::for_mutation(id, StepMutation::FoundMediaIds { title_key: key, ids: *ids })
}

pub struct MediaLookup(pub Arc<Media>);

impl Step for MediaLookup {
    fn id(&self) -> StepId {
        StepId(33)
    }
    fn name(&self) -> &'static str {
        "media_lookup"
    }
    fn rank(&self) -> u16 {
        510
    }
    fn needs(&self) -> Needs {
        Needs::External
    }
    fn enabled(&self, cfg: &Config) -> bool {
        self.0.configure(cfg);
        cfg.media.any_provider()
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: true, on_growth: true }
    }
    fn config_stamp(&self) -> u64 {
        u64::from(self.0.provider_bits())
    }
    fn applies(&self, r: &ReleaseInfo) -> bool {
        target_of(&self.0, r).is_some()
    }
    /// Without `wants`, which claims a title for the release that asks: the page mustn't.
    fn skip_reason(&self, r: &ReleaseInfo) -> Option<&'static str> {
        match target_of(&self.0, r) {
            None => Some("no movie or TV title in its name to look up"),
            Some(_) => None,
        }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        if ctx.stopping() {
            return Err(StepError::Stopped);
        }
        let id = ctx.release().id;
        let Some((kind, p)) = target_of(&self.0, ctx.release()) else { return Ok(Outcome::default()) };
        let key = key_of(kind, &p);
        let (title, country) = split_country(&p.title);
        let _claim = match self.0.claim_native(key, id, now_secs()) {
            Some(claim) => claim,
            None => return Ok(Outcome { wait_until: Some(now_secs() + 1), ..Outcome::default() }),
        };
        // `AC31594` is AlloCiné's: what it is, by name and by whatever ids Wikidata has, before any provider
        let decoded = match (allocine_id(title), self.0.allocine()) {
            (Some(ac), Some(allocine)) => match allocine.decode(kind, ac) {
                Ok(found) => found,
                // as `resolve` does on its way out: another release of the title may ask it next
                Err(Trouble::Paused(t) | Trouble::BadKey(t)) => {
                    self.0.clear_claim(key);
                    return Ok(Outcome { wait_until: Some(t + id.rem_euclid(30)), ..Outcome::default() });
                }
                Err(Trouble::Bad(e)) => {
                    self.0.clear_claim(key);
                    return Err(StepError::Retry(e));
                }
                Err(Trouble::Cache(error)) => return Err(StepError::from_cache(error.into_error())),
            },
            _ => None,
        };
        let q = match &decoded {
            Some(d) => TitleQuery { kind, title: &d.title, year: p.year.or(d.year), country: None },
            None => TitleQuery { kind, title, year: p.year, country },
        };
        let seed = decoded.as_ref().map(|d| (&d.ids, d.title.as_str()));
        let resolved = {
            let ctx_ref = &*ctx;
            self.0.resolve_seeded_checked(&q, key, now_secs(), &|| ctx_ref.stopping(), seed)
        };
        let reindex = || decoded.as_ref().map(|_| StepWrite::for_mutation(id, StepMutation::ReindexWords));
        match resolved.map_err(|error| match error {
            super::resolve::ResolveFailure::Cache(error) => self.0.cache_step_error(error),
            super::resolve::ResolveFailure::Provider(error) => StepError::Retry(error),
        })? {
            Resolved::Found(ids) => {
                let writes = std::iter::once(found_write(id, key, &ids)).chain(reindex()).collect();
                Ok(Outcome { matched: true, writes, ..Outcome::default() })
            }
            Resolved::NotFound => {
                // nothing asked a title with IDs in the cache, soo these are from a row deleted by hand: a wrong match
                // and a title decoded from an AlloCiné id: its other releases are found by it too, once fan-out
                // queues them for the word index
                if decoded.is_some() || ctx.data().stale_media_ids(key)? {
                    self.0.fan_out_again_checked(key).map_err(|error| self.0.cache_step_error(error))?;
                }
                Ok(Outcome { writes: reindex().into_iter().collect(), ..Outcome::default() })
            }
            Resolved::Later(_) if ctx.stopping() => Err(StepError::Stopped),
            Resolved::Later(t) => Ok(Outcome { wait_until: Some(t + id.rem_euclid(30)), ..Outcome::default() }),
        }
    }
}

pub struct MediaNfo(pub Arc<Media>);

impl Step for MediaNfo {
    fn id(&self) -> StepId {
        StepId(34)
    }
    fn name(&self) -> &'static str {
        "media_nfo"
    }
    fn rank(&self) -> u16 {
        520
    }
    fn needs(&self) -> Needs {
        Needs::Bodies
    }
    fn enabled(&self, cfg: &Config) -> bool {
        self.0.configure(cfg);
        cfg.media.nfo && cfg.media.parse && cfg.postprocess.body_fetches_per_minute > 0
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: true, on_growth: true }
    }
    fn applies(&self, r: &ReleaseInfo) -> bool {
        target_of(&self.0, r).is_some_and(|(kind, _)| kind == Kind::Movie)
    }
    fn skip_reason(&self, r: &ReleaseInfo) -> Option<&'static str> {
        match target_of(&self.0, r) {
            None => Some("no movie title in its name to find an IMDb id for"),
            Some((Kind::Tv, _)) => Some("an episode: only a movie's NFO is read for its IMDb id"),
            Some(_) => None,
        }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let id = ctx.release().id;
        if let Some((kind, parsed)) = target_of(&self.0, ctx.release()) {
            if self.0.imdb_known_checked(key_of(kind, &parsed)).map_err(StepError::from_cache)? {
                return Ok(Outcome::default());
            }
        }
        let current = ctx.data().media_imdb_marker(id)?;
        if !matches!(current, Some(None)) {
            return Ok(Outcome::default());
        }
        let files = ctx.files().map_err(StepError::from_inventory)?;
        let Some(file) = nfo_sources(&files).into_iter().next() else { return Ok(Outcome::default()) };
        let par2_names = ctx.data().par2_names(id, &files)?;
        let par2_name = par2_names.get(&file.filename).map(String::as_str);
        // an NFO the probe or another step read is used as it was kept, not fetched again
        if let Some(kept) = ctx.data().stored_nfo(id)?.filter(|k| k.is_of(&file.filename, par2_name)) {
            let Some(imdb) = imdb_in_nfo(kept.text.as_bytes()) else { return Ok(Outcome::default()) };
            let write = StepWrite::for_mutation(id, StepMutation::PinNfoMovieImdb { imdb_id: imdb });
            return Ok(Outcome { matched: true, writes: vec![write], ..Outcome::default() });
        }
        let segment =
            ctx.segments(file.id, &SegmentPick::First(1)).map_err(StepError::from_inventory)?.into_iter().next();
        let Some(segment) = segment.filter(|s| s.part.is_none_or(|p| p <= 1)) else { return Ok(Outcome::default()) };
        let wanted =
            [BodyRequest { message_id: segment.message_id, file: file.filename.clone(), max_bytes: Some(NFO_READ) }];
        let got = ctx.fetch_bodies(&wanted)?.into_iter().next();
        let Some(Got::Body(body)) = got.map(|b| b.got) else { return Ok(Outcome::default()) };
        // kept whole only: a read cut at `NFO_READ` isn't the NFO for the next reader
        let mut writes: Vec<StepWrite> = (body.len() < NFO_READ)
            .then(|| nfo_store::keep(id, &file.filename, &crate::probe::nfo_text(&body)))
            .into_iter()
            .collect();
        let Some(imdb) = imdb_in_nfo(&body) else { return Ok(Outcome { writes, ..Outcome::default() }) };
        writes.push(StepWrite::for_mutation(id, StepMutation::PinNfoMovieImdb { imdb_id: imdb }));
        Ok(Outcome { matched: true, writes, ..Outcome::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spots_kind_gives_its_title_to_look_up() {
        crate::media::with_native_test_runtime(
            "media::steps::tests::a_spots_kind_gives_its_title_to_look_up",
            |runtime| {
                // a name the category rules don't read as a movie: the spot says it is one
                let media = super::super::native_service(&runtime.owner, &Config::default()).unwrap();
                let r = ReleaseInfo {
                    name: "01. The Matrix (1999) 4k UHD Rife 60fps AI-Enhanced Dolby True-HD Atmos".into(),
                    group: "alt.binaries.boneless".into(),
                    poster: Some("yEncBin@Poster.com (FrameMaster)".into()),
                    ..Default::default()
                };
                assert!(target_of(&media, &r).is_none(), "no movie by its name alone");
                let spot = ReleaseInfo { spot_kind: Some(crate::spotnet::Kind::Movie), ..r };
                let (kind, p) = target_of(&media, &spot).unwrap();
                assert_eq!((kind, p.title.as_str(), p.year), (Kind::Movie, "The Matrix", Some(1999)));
                assert_eq!(MediaLookup(media.clone()).skip_reason(&spot), None);

            },
        );
    }

    #[test]
    fn each_step_has_its_lane() {
        crate::media::with_native_test_runtime(
            "media::steps::tests::each_step_has_its_lane",
            |runtime| {
                let media = super::super::native_service(&runtime.owner, &Config::default()).unwrap();
                assert_eq!(MediaParse(media.clone()).needs(), Needs::Database);
                assert_eq!(MediaLookup(media.clone()).needs(), Needs::External);
                assert_eq!(MediaNfo(media).needs(), Needs::Bodies);

            },
        );
    }

    #[test]
    fn the_saved_probes_tags_reach_a_row_made_later() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let runtime = crate::pg::stage_queue_tests::runtime(&schema);
        crate::pg::stage_queue_tests::push(&runtime);
        let reads = crate::pg::stage_reads::StepReads::for_owner(&runtime.owner).unwrap();
        let id = runtime.owner.with(|c| Ok(c.query_one("select id from releases", &[])?.get::<_, i64>(0))).unwrap();
        assert!(reads.saved_probe_media(id).unwrap().is_none());
        let probe = crate::probe::ProbeInfo {
            media: Some(crate::probe::media::MediaInfo {
                tags: [("IMDB".to_string(), "tt0133093".to_string())].into_iter().collect(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let json = serde_json::to_string(&probe).unwrap();
        runtime
            .owner
            .with(|c| Ok(c.execute("insert into release_probe(release_id,info) values($1,$2)", &[&id, &json])?))
            .unwrap();
        let media = reads.saved_probe_media(id).unwrap().expect("saved native media");
        assert_eq!(media.title_ids().imdb, Some(133_093));
        assert_eq!(crate::probe::step::title_writes(id, Some(&media)).len(), 1);
        runtime
            .owner
            .with(|c| Ok(c.execute("update release_probe set info='not json' where release_id=$1", &[&id])?))
            .unwrap();
        assert!(reads.saved_probe_media(id).is_err(), "malformed native storage remains a typed failure");
    }
}
