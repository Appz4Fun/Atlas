//! Bounded periodic details for one canonical native attempt. No legacy service,
//! SQLite handle, detached task or transaction spanning provider I/O is used.
use super::*;
use crate::{media::{cache,resolve::{Trouble,CacheFailure}},pg::{indexer_runtime::Owner,service_cache::{Cache,details::{Source,TraktIdentity}},worker_assets::Grant}};
use std::{collections::VecDeque,time::Instant};
use anyhow::Result;

#[derive(Clone,Copy,Default)]
struct RatingFailures {omdb:bool,trakt:bool}
struct Refreshed {details:Details,failed:RatingFailures}
struct Pending {source:Source,trakt_missing:bool}

/// Preserve only a failed provider's previous valid ratings, for this exact
/// natural key. Successful, absent, disabled and unrated responses remain new
/// observations; they do not inherit unrelated or obsolete provider scores.
fn preserve_failed_scores(mut fresh:Refreshed,kept:Option<&Details>)->Details {
    if let Some(old)=kept.filter(|old|old.kind==fresh.details.kind&&old.tmdb_id==fresh.details.tmdb_id) {
        let valid=|score:Option<i64>|score.filter(|score|(0..=100).contains(score));
        if fresh.failed.omdb {
            fresh.details.imdb_score=fresh.details.imdb_score.or(valid(old.imdb_score));
            fresh.details.rotten=fresh.details.rotten.or(valid(old.rotten));
            fresh.details.metacritic=fresh.details.metacritic.or(valid(old.metacritic));
        }
        if fresh.failed.trakt {fresh.details.trakt_score=fresh.details.trakt_score.or(valid(old.trakt_score));}
    }
    fresh.details
}

/// This is deliberately page-local: Cache::details_page supplies at most64
/// rows. Keep newest-release precedence, filling only a missing Trakt ID for
/// identical other provider IDs. Conflicting observations stay separate.
fn merge_sources(mut rows:Vec<Source>)->VecDeque<Pending> {
    rows.sort_by_key(|row|std::cmp::Reverse(row.release));
    let mut merged:VecDeque<Pending>=VecDeque::new();
    for row in rows {
        let same=|pending:&&mut Pending| {let kept=&pending.source;kept.kind==row.kind&&kept.ids.tmdb==row.ids.tmdb&&kept.ids.imdb==row.ids.imdb
            &&kept.ids.tvdb==row.ids.tvdb&&kept.ids.tvmaze==row.ids.tvmaze&&kept.ids.tvrage==row.ids.tvrage
            &&kept.ids.trakt.zip(row.ids.trakt).is_none_or(|(a,b)|a==b)};
        if let Some(kept)=merged.iter_mut().find(same) {kept.source.ids.trakt=kept.source.ids.trakt.or(row.ids.trakt);}
        else {merged.push_back(Pending {trakt_missing:row.ids.trakt.is_none(),source:row});}
    }
    merged
}

/// Return whether this provider failed. Cache failures remain fatal to the
/// native attempt; definitive absence does not become an outage or retry.
fn rating_reply(details:&mut Details,reply:Result<Reply,Trouble>,now:i64,apply:fn(&mut Details,&Value))->Result<bool,Trouble> {
    match reply {
        Ok(Reply::Json(body))=>{apply(details,&body);Ok(false)},
        Ok(Reply::NotFound)=>Ok(false),
        Err(Trouble::Cache(error))=>Err(Trouble::Cache(error)),
        Err(_)=>{try_again_soon(details,now);Ok(true)},
    }
}

fn bind_trakt(mut ids:Ids,identity:TraktIdentity)->Result<Ids,Trouble> {
    if ids.trakt.is_some() {return Ok(ids)}
    match identity {
        TraktIdentity::Absent=>{},
        TraktIdentity::Unique(id)=>ids.trakt=Some(id),
        TraktIdentity::Ambiguous=>return Err(Trouble::Bad("ambiguous native details rating identity".into())),
    }
    Ok(ids)
}

#[derive(Debug)]
pub(crate) enum Progress {Disabled,Idle,Visited,ArtworkUnavailable,Deferred,Waiting(Duration)}
pub(crate) struct Service {
    cache:Arc<Cache>,config:MediaConfig,sources:Option<Arc<Sources>>,image:Option<Client>,assets:Option<Arc<Grant>>,
    after:Option<i64>,pending:VecDeque<Pending>,next:Instant,
}
impl Service {
    pub(crate) fn enabled(config:&MediaConfig)->bool {backfill_on(config)}
    pub(crate) fn new(owner:&Arc<Owner>,config:&MediaConfig,assets:Option<Arc<Grant>>)->Result<Self> {
        anyhow::ensure!(tokio::runtime::Handle::try_current().is_err(),"native periodic media requires a standard worker thread");
        if let Some(assets)=&assets {assets.require_owner(owner)?;}
        let cache=Arc::new(Cache::new(owner)?);
        let (sources,image)=if Self::enabled(config) {cache::native(cache.clone(),|| {
            (Some(make_sources(config)),Some(Client::new("media-artwork",config.tmdb.image_base_url.as_deref().unwrap_or(IMAGE_BASE),config.details_per_minute)))
        })} else {(None,None)};
        Ok(Self {cache,config:config.clone(),sources,image,assets,after:None,pending:VecDeque::new(),next:Instant::now()+BACKFILL_DELAY})
    }
    #[cfg(test)]
    pub(crate) fn start_without_delay_for_test(&mut self) {self.next=Instant::now();}
    pub(crate) fn tick(&mut self,now:i64)->Result<Progress> {self.tick_at(now,Instant::now())}
    pub(super) fn tick_at(&mut self,now:i64,at:Instant)->Result<Progress> {
        if !Self::enabled(&self.config) {return Ok(Progress::Disabled)}
        if at<self.next {return Ok(Progress::Waiting(self.next-at))}
        self.cache.check()?;
        if self.pending.is_empty() {
            let page=self.cache.details_page(self.after)?;
            if page.exhausted {self.after=None;self.next=at+BACKFILL_REST;return Ok(Progress::Idle)}
            self.after=page.after;
            self.pending.extend(merge_sources(page.rows));
            if self.pending.is_empty() {self.next=at+Duration::from_millis(100);return Ok(Progress::Visited)}
        }
        let Some(Pending {source,trakt_missing})=self.pending.pop_front() else{return Ok(Progress::Idle)};
        self.next=at+Duration::from_secs_f64(60.0/self.config.details_per_minute.max(1) as f64);
        match self.for_title_with_trakt(source.kind,&source.ids,trakt_missing,now) {
            Ok(Some(details))=>{
                if let Some(path)=details.poster.as_deref() {
                    let Some(assets)=&self.assets else{return Ok(Progress::ArtworkUnavailable)};
                    match assets.poster_available(details.kind,details.tmdb_id,path)? {
                        crate::pg::worker_assets::ArtworkStatus::Kept=>return Ok(Progress::Visited),
                        crate::pg::worker_assets::ArtworkStatus::Changed=>return Ok(Progress::Deferred),
                        crate::pg::worker_assets::ArtworkStatus::Unavailable=>return Ok(Progress::ArtworkUnavailable),
                        crate::pg::worker_assets::ArtworkStatus::Missing=>{},
                    }
                    {
                        let url=if path.starts_with("http"){path.to_string()}else{format!("{}{path}",self.config.tmdb.image_base_url.as_deref().unwrap_or(IMAGE_BASE))};
                        let reply=self.image.as_ref().expect("enabled image client").get_image(&url);
                        match reply {
                            Ok(Some(bytes))=>{if !bytes.starts_with(&[0xff,0xd8,0xff]) {return Ok(Progress::Deferred)}if assets.produce_poster(details.kind,details.tmdb_id,path,&bytes)?==crate::pg::worker_assets::ArtworkStatus::Changed {return Ok(Progress::Deferred)}},
                            Ok(None)=>{},
                            Err(error)=>return self.problem(error,source,trakt_missing,now,at),
                        }
                    }
                }
                Ok(Progress::Visited)
            },
            Ok(None)=>Ok(Progress::Visited),
            Err(error)=>self.problem(error,source,trakt_missing,now,at),
        }
    }
    fn problem(&mut self,error:Trouble,source:Source,trakt_missing:bool,now:i64,at:Instant)->Result<Progress> {
        match error {
            Trouble::Cache(error)=>Err(error.into_error()),
            Trouble::Paused(until)|Trouble::BadKey(until)=>{
                let wait=Duration::from_secs(until.saturating_sub(now).max(1) as u64);self.next=at+wait;
                self.pending.push_front(Pending {source,trakt_missing});Ok(Progress::Waiting(wait))
            },
            // A provider failure says nothing about the title. Keep prior rows,
            // advance this bounded pass, and retry it on the next ordinary pass.
            Trouble::Bad(_)=>Ok(Progress::Deferred),
        }
    }
    fn failure(error:anyhow::Error)->Trouble {Trouble::Cache(CacheFailure::new(error))}
    fn lookup(&self,kind:i64,ids:&Ids,now:i64)->Result<Option<i64>,Trouble> {
        let key=lookup_key(kind,ids);
        if let Some((id,checked))=self.cache.details_lookup(&key).map_err(Self::failure)?
            && (id.is_some()||now.saturating_sub(checked)<RETRY_AFTER) {return Ok(id)}
        let Some(sources)=self.sources.as_ref().filter(|_|!self.config.tmdb.api_key.is_empty()) else{return Ok(None)};
        let found=sources.native_find(kind,ids)?;
        self.cache.put_details_lookup(&key,found,now).map_err(Self::failure)?;Ok(found)
    }
    pub(super) fn for_title(&self,kind:i64,ids:&Ids,now:i64)->Result<Option<Details>,Trouble> {
        self.for_title_with_trakt(kind,ids,ids.trakt.is_none(),now)
    }
    fn for_title_with_trakt(&self,kind:i64,ids:&Ids,trakt_missing:bool,now:i64)->Result<Option<Details>,Trouble> {
        if !(1..=2).contains(&kind) {return Err(Trouble::Bad("unsupported native details kind".into()))}
        // Fill only a missing rating-provider ID, before any lookup/provider or
        // freshness decision. Known IDs retain their existing authority.
        // A page merge must not hide ambiguity on an older page. The private
        // pending flag remembers whether the newest source originally lacked
        // Trakt; only its provisional page addition may be replaced here.
        let ids=if trakt_missing {bind_trakt(Ids {trakt:None,..*ids},self.cache.details_trakt(kind,ids).map_err(Self::failure)?)?}else{*ids};
        let ids=&ids;
        let tmdb=match ids.tmdb {Some(id)=>Some(id),None=>self.lookup(kind,ids,now)?};
        let own=if kind==Kind::Tv as i64 {ids.tvdb}else{None};
        let own_kept=match own {
            Some(id)=>self.cache.details(kind,id.checked_neg().ok_or_else(||Trouble::Bad("TVDB cache key out of range".into()))?).map_err(Self::failure)?,None=>None,
        };
        let fresh=|details:&Details|now.saturating_sub(details.fetched_at)<KEEP_FOR;
        if let Some(tmdb)=tmdb {
            let kept=self.cache.details(kind,tmdb).map_err(Self::failure)?;
            if kept.as_ref().is_some_and(fresh) {return Ok(kept)}
            if own_kept.as_ref().is_some_and(fresh) {return Ok(own_kept)}
            if !self.config.tmdb.api_key.is_empty() {
                if let Some(fresh)=self.sources.as_ref().expect("enabled metadata sources").native_fetch(kind,tmdb,ids,now)? {
                    let details=preserve_failed_scores(fresh,kept.as_ref());
                    self.cache.put_details(&details).map_err(Self::failure)?;return Ok(Some(details))
                }
            }
            if kept.is_some() {return Ok(kept)}
        }
        let Some(tvdb)=own else{return Ok(None)};
        if own_kept.as_ref().is_some_and(fresh) {return Ok(own_kept)}
        let Some(sources)=self.sources.as_ref() else{return Ok(own_kept)};
        let Some(fresh)=sources.native_tvdb(tvdb,ids,now)? else{return Ok(own_kept)};
        let details=preserve_failed_scores(fresh,own_kept.as_ref());
        self.cache.put_details(&details).map_err(Self::failure)?;Ok(Some(details))
    }
}
impl Sources {
    fn native_find(&self,kind:i64,ids:&Ids)->Result<Option<i64>,Trouble> {
        let (external,source)=match (ids.imdb,ids.tvdb) {(Some(id),_)=>(format!("tt{id:07}"),"imdb_id"),(None,Some(id))=>(id.to_string(),"tvdb_id"),_=>return Ok(None)};
        let url=format!("{}/find/{external}?external_source={source}&api_key={}",self.tmdb_base(),encode(&self.tmdb.api_key));
        match self.tmdb_client.get(&url,&[])? {
            Reply::NotFound=>Ok(None),
            Reply::Json(body)=>{
                let field=if kind==Kind::Tv as i64{"tv_results"}else{"movie_results"};
                let rows=body.get(field).and_then(Value::as_array).ok_or_else(||Trouble::Bad("media find response has no result list".into()))?;
                rows.first().map(|row|row.get("id").and_then(Value::as_i64).filter(|id|*id>0).ok_or_else(||Trouble::Bad("media find result has no valid ID".into()))).transpose()
            },
        }
    }
    fn native_fetch(&self,kind:i64,id:i64,ids:&Ids,now:i64)->Result<Option<Refreshed>,Trouble> {
        let section=if kind==Kind::Tv as i64{"tv"}else{"movie"};let extra=if kind==Kind::Tv as i64{"credits"}else{"credits,release_dates"};
        let url=format!("{}/{section}/{id}?append_to_response={extra}&api_key={}",self.tmdb_base(),encode(&self.tmdb.api_key));
        let Reply::Json(body)=self.tmdb_client.get(&url,&[])? else{return Ok(None)};
        let mut details=from_tmdb(kind,id,&body,now);let failed=self.native_ratings(&mut details,kind,ids,now)?;Ok(Some(Refreshed {details,failed}))
    }
    fn native_tvdb(&self,id:i64,ids:&Ids,now:i64)->Result<Option<Refreshed>,Trouble> {
        let Some(tvdb)=&self.tvdb else{return Ok(None)};
        let Some(body)=tvdb.series(id)? else{return Ok(None)};
        let mut details=from_tvdb(id,&body,self.tvdb_config.base_url.as_deref().unwrap_or(TVDB_IMAGES),now);
        let failed=self.native_ratings(&mut details,Kind::Tv as i64,ids,now)?;Ok(Some(Refreshed {details,failed}))
    }
    fn native_ratings(&self,details:&mut Details,kind:i64,ids:&Ids,now:i64)->Result<RatingFailures,Trouble> {
        let mut failed=RatingFailures::default();
        if let Some(imdb)=ids.imdb && !self.omdb.api_key.is_empty() {
            let base=self.omdb.base_url.as_deref().unwrap_or(OMDB_BASE);let url=format!("{base}/?i=tt{imdb:07}&apikey={}",encode(&self.omdb.api_key));
            failed.omdb=rating_reply(details,self.omdb_client.get(&url,&[]),now,add_omdb)?;
        }
        if let Some(trakt)=ids.trakt && !self.trakt.client_id.is_empty() {
            let base=self.trakt.base_url.as_deref().unwrap_or(TRAKT_BASE);let section=if kind==Kind::Tv as i64{"shows"}else{"movies"};let url=format!("{base}/{section}/{trakt}?extended=full");
            failed.trakt=rating_reply(details,self.trakt_client.get(&url,&[("trakt-api-version","2"),("trakt-api-key",self.trakt.client_id.as_str())]),now,|d,body|d.trakt_score=trakt_score(body))?;
        }Ok(failed)
    }
}

#[cfg(test)]
#[path="native_merge_tests.rs"]
mod merge_tests;
