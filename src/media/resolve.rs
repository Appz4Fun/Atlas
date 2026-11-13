//! A title's IDs: asked once of each provider, and cached.
//!
//! The order of providers and the candidate rules follow newznab-tmux
//! `app/Services/TvProcessing/Providers/AbstractTvProvider.php`, GPLv3.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::Media;
use super::store::{STATE_ASKING, STATE_FOUND, STATE_NOT_FOUND, TitleRow};
use super::title::Kind;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ids {
    pub tvdb: Option<i64>,
    pub tvmaze: Option<i64>,
    pub trakt: Option<i64>,
    pub tmdb: Option<i64>,
    pub imdb: Option<i64>,
    pub tvrage: Option<i64>,
}

impl Ids {
    pub fn any(&self) -> bool {
        *self != Ids::default()
    }

    /// Where both are known, these have to agree: the first provider's identity wins.
    fn consistent(&self, o: &Ids) -> bool {
        let same = |a: Option<i64>, b: Option<i64>| a.zip(b).is_none_or(|(a, b)| a == b);
        same(self.tvdb, o.tvdb) && same(self.imdb, o.imdb) && same(self.tmdb, o.tmdb) && same(self.tvrage, o.tvrage)
    }

    fn merge(&mut self, o: &Ids) {
        self.tvdb = self.tvdb.or(o.tvdb);
        self.tvmaze = self.tvmaze.or(o.tvmaze);
        self.trakt = self.trakt.or(o.trakt);
        self.tmdb = self.tmdb.or(o.tmdb);
        self.imdb = self.imdb.or(o.imdb);
        self.tvrage = self.tvrage.or(o.tvrage);
    }
}

pub struct TitleQuery<'a> {
    pub kind: Kind,
    pub title: &'a str,
    pub year: Option<i32>,
    pub country: Option<&'a str>,
}

#[derive(Debug, PartialEq)]
pub enum Answer {
    Found { ids: Ids, title: String },
    NotFound,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Trouble {
    /// can't be asked until then
    Paused(i64),
    /// its key was refused: until then
    BadKey(i64),
    Bad(String),
    Cache(CacheFailure),
}

#[derive(Clone,Debug)]
pub struct CacheFailure(std::sync::Arc<anyhow::Error>);
impl CacheFailure {
    pub(crate) fn new(error:anyhow::Error)->Self {Self(std::sync::Arc::new(error))}
    pub(crate) fn into_error(self)->anyhow::Error {anyhow::Error::new(self)}
}
impl PartialEq for CacheFailure {fn eq(&self,other:&Self)->bool {std::sync::Arc::ptr_eq(&self.0,&other.0)}}
impl std::fmt::Display for CacheFailure {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("native service cache failed")}}
impl std::error::Error for CacheFailure {fn source(&self)->Option<&(dyn std::error::Error+'static)> {Some(self.0.as_ref().as_ref())}}

pub trait Provider: Send + Sync {
    fn bit(&self) -> u8;
    fn asks(&self, kind: Kind) -> bool;
    /// Waits at most 2 s for a token, then Paused(now + 1).
    fn find(&self, q: &TitleQuery<'_>) -> Result<Answer, Trouble>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolved {
    Found(Ids),
    NotFound,
    Later(i64),
}

static IMDB: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:tt)?([0-9]{6,10})$").unwrap());
static YEAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^((?:19|20)[0-9]{2})").unwrap());

/// A positive integer from a number or a numeric string.
pub(crate) fn int_of(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|n| *n > 0)
}

pub(crate) fn imdb_of(v: &Value) -> Option<i64> {
    let text = v.as_str()?;
    let c = IMDB.captures(text.trim())?;
    c[1].parse().ok().filter(|n: &i64| *n > 0)
}

pub(crate) fn year_of(v: &Value) -> Option<i32> {
    YEAR.captures(v.as_str()?).and_then(|c| c[1].parse().ok())
}

/// the longest a miss waits before it's asked again
const MAX_NEGATIVE_DAYS: i64 = 180;

#[derive(Debug)]
pub(crate) enum ResolveFailure {Cache(anyhow::Error),Provider(String)}
impl std::fmt::Display for ResolveFailure {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {match self {Self::Cache(error)=>write!(f,"{error}"),Self::Provider(error)=>f.write_str(error)}}
}

impl Media {
    /// What's known of this title, asking whichever providers are still owed an answer.
    pub fn resolve(&self, q: &TitleQuery<'_>, key: i64, now: i64) -> Result<Resolved, String> {
        self.resolve_until(q, key, now, &|| false)
    }

    /// `stopping` is checked before each provider. Stopped is `Later(now)`, with what was answered kept.
    pub(crate) fn resolve_until(
        &self,
        q: &TitleQuery<'_>,
        key: i64,
        now: i64,
        stopping: &dyn Fn() -> bool,
    ) -> Result<Resolved, String> {
        self.resolve_seeded(q, key, now, stopping, None)
    }

    /// As `resolve_until`, with ids already known some other way, as an AlloCiné id's from Wikidata. They come first,
    /// soo a provider's answer for another title of the same name is passed over.
    pub(crate) fn resolve_seeded(
        &self,
        q: &TitleQuery<'_>,
        key: i64,
        now: i64,
        stopping: &dyn Fn() -> bool,
        seed: Option<(&Ids, &str)>,
    ) -> Result<Resolved, String> {
        self.resolve_seeded_checked(q,key,now,stopping,seed).map_err(|error|error.to_string())
    }

    pub(crate) fn resolve_seeded_checked(
        &self,q:&TitleQuery<'_>,key:i64,now:i64,stopping:&dyn Fn()->bool,seed:Option<(&Ids,&str)>,
    )->Result<Resolved,ResolveFailure> {
        let providers = self.providers();
        let existing = self.title_get_checked(key).map_err(ResolveFailure::Cache)?;
        if self.is_native() && let Some(row)=&existing {
            if row.kind!=q.kind as i64 || row.year!=q.year || super::title::normalize(&row.title)!=super::title::normalize(q.title) {
                return Err(ResolveFailure::Cache(anyhow::anyhow!("media query/cache identity mismatch")));
            }
        }
        if self.is_native() && existing.is_none() && self.titles.load(std::sync::atomic::Ordering::Relaxed)>=u64::try_from(self.config.read().unwrap().max_titles).unwrap_or(0) {
            self.titles_full.fetch_add(1,std::sync::atomic::Ordering::Relaxed);return Ok(Resolved::Later(now+30));
        }
        let mut row = existing.clone().unwrap_or(TitleRow {
            key,
            kind: q.kind as i64,
            title: q.title.to_string(),
            year: q.year,
            state: STATE_ASKING,
            fanout_done: true,
            ..TitleRow::default()
        });
        // a miss past its time asks everyone again, starting from no ids
        if row.state == STATE_NOT_FOUND && row.next_check.is_none_or(|t| now >= t) {
            row.checked_mask = 0;
            row.ids = Ids::default();
        }
        let before = row.ids;
        if let Some((ids, title)) = seed.filter(|(ids, _)| ids.any() && row.ids.consistent(ids)) {
            row.ids.merge(ids);
            if row.found_title.is_none() {
                row.found_title = Some(title.to_string());
            }
        }
        let seeded = row.ids.any() && row.state != STATE_FOUND;
        let ask: Vec<_> =
            providers.iter().filter(|p| p.asks(q.kind) && i64::from(p.bit()) & row.checked_mask == 0).collect();
        if ask.is_empty() && !seeded {
            self.clear_claim(key);
            return Ok(if row.state == STATE_FOUND { Resolved::Found(row.ids) } else { Resolved::NotFound });
        }

        let (mut later, mut bad, mut stopped): (Option<i64>, Option<String>, bool) = (None, None, false);
        for provider in ask {
            if stopping() {
                stopped = true;
                break;
            }
            match provider.find(q) {
                Ok(Answer::Found { ids, title }) => {
                    row.checked_mask |= i64::from(provider.bit());
                    if row.ids.consistent(&ids) {
                        row.ids.merge(&ids);
                        if row.found_title.is_none() {
                            row.found_title = Some(title);
                        }
                    }
                }
                Ok(Answer::NotFound) => row.checked_mask |= i64::from(provider.bit()),
                Err(Trouble::Paused(t) | Trouble::BadKey(t)) => later = Some(later.map_or(t, |l: i64| l.min(t))),
                Err(Trouble::Bad(e)) => bad = Some(e),
                Err(Trouble::Cache(error))=>return Err(ResolveFailure::Cache(error.into_error())),
            }
        }

        let trouble = later.is_some() || bad.is_some() || stopped;
        row.checked_at = now;
        if row.ids.any() {
            row.state = STATE_FOUND;
            row.next_check = None;
            row.misses = 0;
            if row.ids != before || existing.is_none() {
                row.fanout_done = false;
            }
        } else if trouble {
            row.state = STATE_ASKING;
        } else {
            row.state = STATE_NOT_FOUND;
            let days = (self.negative_ttl_days() << row.misses.clamp(0, 16)).min(MAX_NEGATIVE_DAYS);
            row.misses += 1;
            row.next_check = Some(now + days * 86_400);
        }
        self.title_put_checked(&row).map_err(ResolveFailure::Cache)?;
        self.remember(&row, existing.is_none());
        self.clear_claim(key);
        // a provider still owed an answer keeps this release in the lane, even with IDs from another: no later
        // release of the title may come along to ask it. What was found already reaches the rows by the fan-out.
        if stopped {
            return Ok(Resolved::Later(now));
        }
        if let Some(t) = later {
            return Ok(Resolved::Later(t));
        }
        if let Some(e) = bad {
            return Err(ResolveFailure::Provider(e));
        }
        Ok(if row.state == STATE_FOUND { Resolved::Found(row.ids) } else { Resolved::NotFound })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imdb_forms() {
        for (text, want) in
            [("tt0100001", Some(100001)), ("0100001", Some(100001)), ("tt0", None), ("nm123", None), ("", None)]
        {
            assert_eq!(imdb_of(&Value::from(text)), want, "{text}");
        }
    }
}
