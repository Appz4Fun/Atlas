//! The steps: a title from a name, its IDs from a provider, and a movie's IMDb id from its NFO.

use std::sync::{Arc, LazyLock};

use regex::bytes::RegexBuilder;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension};

use crate::archive::select::nfo_sources;
use crate::category::{self, Decision};
use crate::config::Config;
use crate::postprocess::schema::ShardTable;
use crate::postprocess::step::{
    BodyRequest, Got, Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId, StepWrite,
};
use crate::store::SegmentPick;

use super::Media;
use super::http::now_secs;
use super::parse::{Parsed, parse_movie, parse_tv};
use super::query::MAX_TITLE;
use super::resolve::{Ids, Resolved, TitleQuery};
use super::title::{Kind, normalize, split_country, title_key};

pub const MEDIA_RELEASE: ShardTable = ShardTable {
    name: "media_release",
    create: "create table if not exists media_release (
        release_id    INTEGER PRIMARY KEY,
        kind          INTEGER NOT NULL,
        title_key     INTEGER NOT NULL,
        season        INTEGER,
        episode       INTEGER,
        episode_last  INTEGER,
        air_date      INTEGER,
        year          INTEGER,
        quality       INTEGER NOT NULL,
        tvdb_id       INTEGER,
        tvmaze_id     INTEGER,
        trakt_id      INTEGER,
        tmdb_id       INTEGER,
        imdb_id       INTEGER,
        tvrage_id     INTEGER,
        imdb_from_nfo INTEGER NOT NULL DEFAULT 0
    )",
    indexes: &[
        (
            "media_release_title",
            "create index if not exists media_release_title on media_release(title_key, release_id)",
        ),
        (
            "media_release_tvdb",
            "create index if not exists media_release_tvdb on media_release(tvdb_id, release_id, season, episode, episode_last, kind) where tvdb_id is not null",
        ),
        (
            "media_release_tvmaze",
            "create index if not exists media_release_tvmaze on media_release(tvmaze_id, release_id, season, episode, episode_last, kind) where tvmaze_id is not null",
        ),
        (
            "media_release_trakt",
            "create index if not exists media_release_trakt on media_release(trakt_id, release_id, season, episode, episode_last, kind) where trakt_id is not null",
        ),
        (
            "media_release_tmdb",
            "create index if not exists media_release_tmdb on media_release(tmdb_id, release_id, season, episode, episode_last, kind) where tmdb_id is not null",
        ),
        (
            "media_release_imdb",
            "create index if not exists media_release_imdb on media_release(imdb_id, release_id, season, episode, episode_last, kind) where imdb_id is not null",
        ),
        (
            "media_release_rage",
            "create index if not exists media_release_rage on media_release(tvrage_id, release_id, season, episode, episode_last, kind) where tvrage_id is not null",
        ),
    ],
    release_key: Some("release_id"),
    added: &[],
};

/// The same title keeps IDs already on the row, soo a late article cant lose them. A new title starts clean.
const UPSERT: &str = "insert into media_release (release_id, kind, title_key, season, episode, episode_last, air_date, year, quality,
                           tvdb_id, tvmaze_id, trakt_id, tmdb_id, imdb_id, tvrage_id)
values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
on conflict(release_id) do update set
    tvdb_id   = iif(media_release.title_key = excluded.title_key, coalesce(excluded.tvdb_id,   media_release.tvdb_id),   excluded.tvdb_id),
    tvmaze_id = iif(media_release.title_key = excluded.title_key, coalesce(excluded.tvmaze_id, media_release.tvmaze_id), excluded.tvmaze_id),
    trakt_id  = iif(media_release.title_key = excluded.title_key, coalesce(excluded.trakt_id,  media_release.trakt_id),  excluded.trakt_id),
    tmdb_id   = iif(media_release.title_key = excluded.title_key, coalesce(excluded.tmdb_id,   media_release.tmdb_id),   excluded.tmdb_id),
    tvrage_id = iif(media_release.title_key = excluded.title_key, coalesce(excluded.tvrage_id, media_release.tvrage_id), excluded.tvrage_id),
    imdb_id   = iif(media_release.title_key = excluded.title_key, iif(media_release.imdb_from_nfo = 1, media_release.imdb_id, coalesce(excluded.imdb_id, media_release.imdb_id)), excluded.imdb_id),
    imdb_from_nfo = iif(media_release.title_key = excluded.title_key, media_release.imdb_from_nfo, 0),
    kind = excluded.kind, title_key = excluded.title_key, season = excluded.season, episode = excluded.episode,
    episode_last = excluded.episode_last, air_date = excluded.air_date, year = excluded.year, quality = excluded.quality";
const DELETE: &str = "delete from media_release where release_id = ?1";
/// only while it's still this title: a rename while the lookup ran mustn't take the old title's IDs
const FOUND: &str = "update media_release
    set tvdb_id = ?3, tvmaze_id = ?4, trakt_id = ?5, tmdb_id = ?6, imdb_id = iif(imdb_from_nfo = 1, imdb_id, ?7), tvrage_id = ?8
    where release_id = ?1 and title_key = ?2";
const NFO_IMDB: &str =
    "update media_release set imdb_id = ?2, imdb_from_nfo = 1 where release_id = ?1 and imdb_id is null";

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
    let d: Decision = category::classify_release(r, &media.category_options());
    media_target(d.category, d.rule, name_of(r))
}

fn id_value(id: Option<i64>) -> Value {
    id.map_or(Value::Null, Value::Integer)
}

fn int_value(n: Option<i32>) -> Value {
    n.map_or(Value::Null, |v| Value::Integer(i64::from(v)))
}

fn has_row(conn: &Connection, id: i64) -> rusqlite::Result<bool> {
    let found: Option<i64> = conn
        .prepare_cached("select 1 from media_release where release_id = ?1")?
        .query_row([id], |r| r.get(0))
        .optional()?;
    Ok(found.is_some())
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
        self.0.set_config(cfg);
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
            let stale = has_row(ctx.conn(), id).map_err(|e| StepError::Retry(e.to_string()))?;
            let writes = if stale { vec![StepWrite::new(DELETE, vec![id.into()])] } else { Vec::new() };
            return Ok(Outcome { writes, ..Outcome::default() });
        };
        let key = key_of(kind, &p);
        let media = self.0.clone();
        let late = Box::new(move || {
            // on the shard's writer, as the row is written: a fan-out pass is a job on the same writer
            let ids = media.cached_ids(key).unwrap_or_default();
            vec![
                Value::Integer(id),
                Value::Integer(kind as i64),
                Value::Integer(key),
                int_value(p.season),
                int_value(p.episode),
                int_value(p.episode_last),
                int_value(p.air_date),
                int_value(p.year),
                Value::Integer(i64::from(p.quality)),
                id_value(ids.tvdb),
                id_value(ids.tvmaze),
                id_value(ids.trakt),
                id_value(ids.tmdb),
                id_value(ids.imdb),
                id_value(ids.tvrage),
            ]
        });
        let write = StepWrite { sql: UPSERT, params: Vec::new(), late_params: Some(late) };
        Ok(Outcome { matched: true, writes: vec![write], ..Outcome::default() })
    }
}

/// Whether rows of this title in this shard hold IDs. The newest few are enough: a fan-out reaches them all alike.
fn stale_ids(conn: &Connection, key: i64) -> rusqlite::Result<bool> {
    conn.prepare_cached(
        "select 1 from (select tvdb_id, tvmaze_id, trakt_id, tmdb_id, imdb_id, tvrage_id, imdb_from_nfo
           from media_release indexed by media_release_title where title_key = ?1 order by release_id desc limit 100)
         where tvdb_id is not null or tvmaze_id is not null or trakt_id is not null or tmdb_id is not null
            or tvrage_id is not null or (imdb_id is not null and imdb_from_nfo = 0)
         limit 1",
    )?
    .exists([key])
}

fn found_write(id: i64, key: i64, ids: &Ids) -> StepWrite {
    StepWrite::new(
        FOUND,
        vec![
            id.into(),
            key.into(),
            id_value(ids.tvdb),
            id_value(ids.tvmaze),
            id_value(ids.trakt),
            id_value(ids.tmdb),
            id_value(ids.imdb),
            id_value(ids.tvrage),
        ],
    )
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
        self.0.set_config(cfg);
        cfg.media.any_provider()
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: true, on_growth: true }
    }
    fn config_stamp(&self) -> u64 {
        u64::from(self.0.provider_bits())
    }
    fn applies(&self, r: &ReleaseInfo) -> bool {
        target_of(&self.0, r).is_some_and(|(kind, p)| self.0.wants(kind, key_of(kind, &p), r.id, now_secs()))
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        if ctx.stopping() {
            return Err(StepError::Stopped);
        }
        let id = ctx.release().id;
        let Some((kind, p)) = target_of(&self.0, ctx.release()) else { return Ok(Outcome::default()) };
        let key = key_of(kind, &p);
        let (title, country) = split_country(&p.title);
        let q = TitleQuery { kind, title, year: p.year, country };
        let resolved = {
            let ctx_ref = &*ctx;
            self.0.resolve_until(&q, key, now_secs(), &|| ctx_ref.stopping())
        };
        match resolved.map_err(StepError::Retry)? {
            Resolved::Found(ids) => {
                Ok(Outcome { matched: true, writes: vec![found_write(id, key, &ids)], ..Outcome::default() })
            }
            Resolved::NotFound => {
                // nothing asked a title with IDs in the cache, soo these are from a row deleted by hand: a wrong match
                let conn = ctx.conn();
                if stale_ids(conn, key).map_err(|e| StepError::Retry(e.to_string()))? {
                    self.0.fan_out_again(key).map_err(StepError::Retry)?;
                }
                Ok(Outcome::default())
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
        self.0.set_config(cfg);
        cfg.media.nfo && cfg.media.parse && cfg.postprocess.body_fetches_per_minute > 0
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: true, on_growth: true }
    }
    fn applies(&self, r: &ReleaseInfo) -> bool {
        target_of(&self.0, r).is_some_and(|(kind, p)| kind == Kind::Movie && !self.0.imdb_known(key_of(kind, &p)))
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let retry = |e: rusqlite::Error| StepError::Retry(e.to_string());
        let id = ctx.release().id;
        let current: Option<Option<i64>> = ctx
            .conn()
            .prepare_cached("select imdb_id from media_release where release_id = ?1")
            .and_then(|mut stmt| stmt.query_row([id], |r| r.get(0)).optional())
            .map_err(retry)?;
        if !matches!(current, Some(None)) {
            return Ok(Outcome::default());
        }
        let files = ctx.files().map_err(retry)?;
        let Some(file) = nfo_sources(&files).into_iter().next() else { return Ok(Outcome::default()) };
        let segment = ctx.segments(file.id, &SegmentPick::First(1)).map_err(retry)?.into_iter().next();
        let Some(segment) = segment.filter(|s| s.part.is_none_or(|p| p <= 1)) else { return Ok(Outcome::default()) };
        let wanted =
            [BodyRequest { message_id: segment.message_id, file: file.filename.clone(), max_bytes: Some(NFO_READ) }];
        let got = ctx.fetch_bodies(&wanted)?.into_iter().next();
        let Some(Got::Body(body)) = got.map(|b| b.got) else { return Ok(Outcome::default()) };
        let Some(imdb) = imdb_in_nfo(&body) else { return Ok(Outcome::default()) };
        let write = StepWrite::new(NFO_IMDB, vec![id.into(), imdb.into()]);
        Ok(Outcome { matched: true, writes: vec![write], ..Outcome::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn each_step_has_its_lane() {
        let media = Media::new(Path::new("/nowhere/atlas.db"));
        assert_eq!(MediaParse(media.clone()).needs(), Needs::Database);
        assert_eq!(MediaLookup(media.clone()).needs(), Needs::External);
        assert_eq!(MediaNfo(media).needs(), Needs::Bodies);
    }
}
