//! Bounded periodic details for one canonical native attempt. No legacy service,
//! SQLite handle, detached task or transaction spanning provider I/O is used.
use super::*;
use crate::{media::{cache,resolve::{Trouble,CacheFailure}},pg::{indexer_runtime::Owner,service_cache::{Cache,details::Source},worker_assets::Grant}};
use std::{collections::{VecDeque,HashSet},time::Instant};
use anyhow::Result;

#[derive(Debug)]
pub(crate) enum Progress {Disabled,Idle,Visited,ArtworkUnavailable,Deferred,Waiting(Duration)}
pub(crate) struct Service {
    cache:Arc<Cache>,config:MediaConfig,sources:Option<Arc<Sources>>,image:Option<Client>,assets:Option<Arc<Grant>>,
    after:Option<i64>,pending:VecDeque<Source>,next:Instant,
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
    pub(crate) fn tick(&mut self,now:i64)->Result<Progress> {self.tick_at(now,Instant::now())}
    pub(super) fn tick_at(&mut self,now:i64,at:Instant)->Result<Progress> {
        if !Self::enabled(&self.config) {return Ok(Progress::Disabled)}
        if at<self.next {return Ok(Progress::Waiting(self.next-at))}
        self.cache.check()?;
        if self.pending.is_empty() {
            let page=self.cache.details_page(self.after)?;
            if page.exhausted {self.after=None;self.next=at+BACKFILL_REST;return Ok(Progress::Idle)}
            self.after=page.after;
            let mut seen=HashSet::new();
            self.pending.extend(page.rows.into_iter().filter(|row|seen.insert((row.kind,row.ids.tmdb,row.ids.imdb,row.ids.tvdb))));
            if self.pending.is_empty() {self.next=at+Duration::from_millis(100);return Ok(Progress::Visited)}
        }
        let Some(source)=self.pending.pop_front() else{return Ok(Progress::Idle)};
        self.next=at+Duration::from_secs_f64(60.0/self.config.details_per_minute.max(1) as f64);
        match self.for_title(source.kind,&source.ids,now) {
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
                            Err(error)=>return self.problem(error,source,now,at),
                        }
                    }
                }
                Ok(Progress::Visited)
            },
            Ok(None)=>Ok(Progress::Visited),
            Err(error)=>self.problem(error,source,now,at),
        }
    }
    fn problem(&mut self,error:Trouble,source:Source,now:i64,at:Instant)->Result<Progress> {
        match error {
            Trouble::Cache(error)=>Err(error.into_error()),
            Trouble::Paused(until)|Trouble::BadKey(until)=>{
                let wait=Duration::from_secs(until.saturating_sub(now).max(1) as u64);self.next=at+wait;
                self.pending.push_front(source);Ok(Progress::Waiting(wait))
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
        if !(1..=2).contains(&kind) {return Err(Trouble::Bad("unsupported native details kind".into()))}
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
                if let Some(details)=self.sources.as_ref().expect("enabled metadata sources").native_fetch(kind,tmdb,ids,now)? {
                    self.cache.put_details(&details).map_err(Self::failure)?;return Ok(Some(details))
                }
            }
            if kept.is_some() {return Ok(kept)}
        }
        let Some(tvdb)=own else{return Ok(None)};
        if own_kept.as_ref().is_some_and(fresh) {return Ok(own_kept)}
        let Some(sources)=self.sources.as_ref() else{return Ok(own_kept)};
        let Some(details)=sources.native_tvdb(tvdb,ids,now)? else{return Ok(own_kept)};
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
    fn native_fetch(&self,kind:i64,id:i64,ids:&Ids,now:i64)->Result<Option<Details>,Trouble> {
        let section=if kind==Kind::Tv as i64{"tv"}else{"movie"};let extra=if kind==Kind::Tv as i64{"credits"}else{"credits,release_dates"};
        let url=format!("{}/{section}/{id}?append_to_response={extra}&api_key={}",self.tmdb_base(),encode(&self.tmdb.api_key));
        let Reply::Json(body)=self.tmdb_client.get(&url,&[])? else{return Ok(None)};
        let mut details=from_tmdb(kind,id,&body,now);self.native_ratings(&mut details,kind,ids,now)?;Ok(Some(details))
    }
    fn native_tvdb(&self,id:i64,ids:&Ids,now:i64)->Result<Option<Details>,Trouble> {
        let Some(tvdb)=&self.tvdb else{return Ok(None)};
        let Some(body)=tvdb.series(id)? else{return Ok(None)};
        let mut details=from_tvdb(id,&body,self.tvdb_config.base_url.as_deref().unwrap_or(TVDB_IMAGES),now);
        self.native_ratings(&mut details,Kind::Tv as i64,ids,now)?;Ok(Some(details))
    }
    fn native_ratings(&self,details:&mut Details,kind:i64,ids:&Ids,now:i64)->Result<(),Trouble> {
        if let Some(imdb)=ids.imdb && !self.omdb.api_key.is_empty() {
            let base=self.omdb.base_url.as_deref().unwrap_or(OMDB_BASE);let url=format!("{base}/?i=tt{imdb:07}&apikey={}",encode(&self.omdb.api_key));
            match self.omdb_client.get(&url,&[]) {Ok(Reply::Json(body))=>add_omdb(details,&body),Ok(Reply::NotFound)=>{},Err(Trouble::Cache(error))=>return Err(Trouble::Cache(error)),Err(_)=>try_again_soon(details,now)}
        }
        if let Some(trakt)=ids.trakt && !self.trakt.client_id.is_empty() {
            let base=self.trakt.base_url.as_deref().unwrap_or(TRAKT_BASE);let section=if kind==Kind::Tv as i64{"shows"}else{"movies"};let url=format!("{base}/{section}/{trakt}?extended=full");
            match self.trakt_client.get(&url,&[("trakt-api-version","2"),("trakt-api-key",self.trakt.client_id.as_str())]) {
                Ok(Reply::Json(body))=>details.trakt_score=trakt_score(&body),Ok(Reply::NotFound)=>{},Err(Trouble::Cache(error))=>return Err(Trouble::Cache(error)),Err(_)=>try_again_soon(details,now),
            }
        }Ok(())
    }
}
