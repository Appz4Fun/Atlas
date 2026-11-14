//! Native PreDB polling. The legacy paging/backoff state machine is retained;
//! every cache operation uses the original owner, while HTTP runs outside its mutex.
use std::{io::Read, sync::{Arc,atomic::{AtomicU64,Ordering::Relaxed}}, time::{Duration,Instant}};
use anyhow::{Result,ensure};
use postgres::types::ToSql;
use crate::{config::PredbConfig, predb::{feeds::{FeedSpec,Parser,ParseError,page_url}, import::{Counts,Entry}, poll::{Fetch,HttpReply,PollEnd,wait_seconds}}};
use super::Cache;
use crate::pg::predb_cache::{BuildStatus,ImportStatus};
const MAX_BODY:usize=8<<20;
const MAX_BACKOFF:i64=21_600;
const RATE_LIMIT_DEFAULT:i64=60;

pub(crate) struct Service { pub(super) cache:Cache, poll_busy:std::sync::Mutex<()>, pub(super) late_at:std::sync::Mutex<Option<Instant>>,pub(crate) counters:Arc<Counters>,
    #[cfg(test)] pub(super) index_pause:std::sync::Mutex<Option<super::predb_late::IndexPause>>,
}
struct FeedCounters {id:i64,key:&'static str,inserted:AtomicU64,updated:AtomicU64,skipped:AtomicU64}
/// The finite canonical feed set bounds registration independently of config.
/// These are this service's events, never persisted feed/catalog gauges.
pub(crate) struct Counters {
    feeds:Vec<FeedCounters>,pub(super) titles:AtomicU64,pub(super) reruns:AtomicU64,pub(super) overflow:AtomicU64,
    native_budget_pages:AtomicU64,pub(super) late_errors:AtomicU64,poll_errors:AtomicU64,
}
impl Counters {
    fn new()->Self {Self {feeds:crate::predb::feeds::SOURCES.iter().map(|spec|FeedCounters {id:spec.id,key:spec.key,
        inserted:AtomicU64::new(0),updated:AtomicU64::new(0),skipped:AtomicU64::new(0)}).collect(),
        titles:AtomicU64::new(0),reruns:AtomicU64::new(0),overflow:AtomicU64::new(0),native_budget_pages:AtomicU64::new(0),late_errors:AtomicU64::new(0),poll_errors:AtomicU64::new(0)}}
    fn imported(&self,source:i64,counts:Counts) {
        if let Some(feed)=self.feeds.iter().find(|feed|feed.id==source) {
            feed.inserted.fetch_add(counts.inserted,Relaxed);feed.updated.fetch_add(counts.updated,Relaxed);feed.skipped.fetch_add(counts.skipped,Relaxed);
        }
    }
    pub(super) fn page(&self,page:&super::LateReport) {
        self.titles.fetch_add(page.titles as u64,Relaxed);self.reruns.fetch_add(page.reruns as u64,Relaxed);self.overflow.fetch_add(page.overflow as u64,Relaxed);
        self.native_budget_pages.fetch_add(u64::from(page.budget_exhausted),Relaxed);
    }
    pub(crate) fn snapshot(&self)->serde_json::Value {
        let sources:Vec<_>=self.feeds.iter().map(|feed|serde_json::json!({"key":feed.key,"inserted":null,"updated":null,
            "events":{"inserted":feed.inserted.load(Relaxed),"updated":feed.updated.load(Relaxed),"skipped":feed.skipped.load(Relaxed)},
            "failures":null,"last_ok_at":null,"last_rows":null})).collect();
        serde_json::json!({"titles":null,"file_bytes":null,"late_behind":null,"errors":self.poll_errors.load(Relaxed),
            "late":{"titles":self.titles.load(Relaxed),"reruns":self.reruns.load(Relaxed),"overflow":self.overflow.load(Relaxed),
                "out_of_budget":null,"native_budget_pages":self.native_budget_pages.load(Relaxed),"errors":self.late_errors.load(Relaxed)},"sources":sources})
    }
}
pub(crate) enum Tick { NotDue, Pending, Polled(PollEnd) }
#[derive(Debug)]
struct Pending;
impl std::fmt::Display for Pending {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("PreDB derived index pending")}}
impl std::error::Error for Pending {}
impl Service {
    pub(crate) fn new(owner:&Arc<crate::pg::indexer_runtime::Owner>)->Result<Self> {Ok(Self {cache:Cache::new(owner)?,poll_busy:std::sync::Mutex::new(()),late_at:std::sync::Mutex::new(None),counters:Arc::new(Counters::new()),#[cfg(test)] index_pause:std::sync::Mutex::new(None)})}
    pub(crate) fn tick(&self,cfg:&PredbConfig,fetch:&dyn Fetch,now:i64,pause:&dyn Fn(Duration)->bool)->Result<Tick> {
        let result=self.tick_inner(cfg,fetch,now,pause);
        if result.as_ref().is_err_and(|error|!error.is::<Pending>()&&!crate::pg::outage::is_attempt_ended(error))
            || matches!(&result,Ok(Tick::Polled(PollEnd::Failed(_)))) {self.counters.poll_errors.fetch_add(1,Relaxed);}
        result
    }
    fn tick_inner(&self,cfg:&PredbConfig,fetch:&dyn Fetch,now:i64,pause:&dyn Fn(Duration)->bool)->Result<Tick> {
        if !cfg.enabled {return Ok(Tick::NotDue)}
        let _poll=match self.poll_busy.try_lock() {
            Ok(guard)=>guard,
            Err(std::sync::TryLockError::WouldBlock)=>return Ok(Tick::NotDue),
            Err(std::sync::TryLockError::Poisoned(_))=>anyhow::bail!("native PreDB poll lane ended"),
        };
        if matches!(self.cache.predb_advance(512)?, BuildStatus::Pending {..}) {return Ok(Tick::Pending)}
        for key in &cfg.sources {
            let Some(spec)=crate::predb::feeds::spec(key) else {continue};
            let due=self.cache.with(|tx|Ok(tx.query_opt("select next_poll_at from ext_predb_feeds where source=$1", &[&spec.id])?.map(|row|row.try_get::<_,i64>(0)).transpose()?.unwrap_or(0)<=now))?;
            if due {
                return match self.poll(fetch,spec,cfg,now,pause) {
                    Ok(end)=>Ok(Tick::Polled(end)),
                    Err(error) if error.is::<Pending>()=>Ok(Tick::Pending),
                    Err(error)=>Err(error),
                }
            }
        }
        Ok(Tick::NotDue)
    }
    fn execute(&self,sql:&str,args:&[&(dyn ToSql+Sync)])->Result<u64> {self.cache.with(|tx|Ok(tx.execute(sql,args)?))}
    fn query(&self,sql:&str,args:&[&(dyn ToSql+Sync)])->Result<postgres::Row> {self.cache.with(|tx|Ok(tx.query_one(sql,args)?))}
    fn import(&self,source:i64,entries:&[Entry],now:i64,cursor:Option<(i64,i64)>)->Result<Counts> {
        let status=self.cache.with(|tx|crate::pg::predb_cache::import_page_in(tx,source,entries,now,cursor))?;
        let ImportStatus::Applied{counts,..}=status else {return Err(Pending.into())};
        // Cache.with has committed. A failed later page cannot erase this one.
        self.counters.imported(source,counts);
        Ok(counts)
    }

    fn cached(&self,spec:&FeedSpec,url:&str,page:u32)->Result<Option<String>> {
        if !crate::raw_store::is_immutable_predb_page(spec.key,page) {return Ok(None)}
        let key=format!("GET:{}:{}",spec.key,crate::raw_store::canonical_cache_url(url));
        self.cache.with(|tx| {
            let Some(row)=tx.query_opt("select status,octet_length(body_zst)::bigint<=$2 as admitted,case when octet_length(body_zst)::bigint<=$2 then body_zst end from ext_http_cache where cache_key=$1", &[&key,&(MAX_BODY as i64)])? else {return Ok(None)};
            ensure!(row.try_get::<_,bool>(1)?,"PreDB cached body exceeds admission");
            if row.try_get::<_,i64>(0)?!=200 {return Ok(None)}
            let compressed:Vec<u8>=row.try_get(2)?;
            let mut bytes=Vec::new();zstd::Decoder::new(&compressed[..])?.take(MAX_BODY as u64+1).read_to_end(&mut bytes)?;
            ensure!(bytes.len()<=MAX_BODY,"PreDB decoded body exceeds admission");Ok(Some(String::from_utf8(bytes)?))
        })
    }
    fn archive(&self,spec:&FeedSpec,url:&str,page:u32,body:&str)->Result<()> {
        use sha2::{Digest,Sha256};
        ensure!(body.len()<=MAX_BODY,"PreDB body exceeds admission");
        let digest=Sha256::digest(body.as_bytes()).to_vec();let compressed=zstd::bulk::compress(body.as_bytes(),3)?;
        let clean=crate::raw_store::canonical_cache_url(url);let now=chrono::Utc::now().timestamp();let page=i64::from(page);
        self.cache.with(|tx| {
            let id:i64=tx.query_one("insert into ext_predb_pages(source_id,source_key,url,page,body_sha256,body_zst,fetched_at) values($1,$2,$3,$4,$5,$6,$7)
                on conflict(source_id,body_sha256) do update set source_id=excluded.source_id returning id", &[&spec.id,&spec.key,&clean,&page,&digest,&compressed,&now])?.try_get(0)?;
            let previous=tx.query_opt("select e.log_id from ext_predb_page_events e join ext_predb_pages p on p.id=e.log_id where p.source_id=$1 and e.page=$2 order by e.seq desc limit 1", &[&spec.id,&page])?.map(|row|row.try_get::<_,i64>(0)).transpose()?;
            if previous!=Some(id) {tx.execute("insert into ext_predb_page_events(log_id,page,fetched_at) values($1,$2,$3)", &[&id,&page,&now])?;}
            if crate::raw_store::is_immutable_predb_page(spec.key,page as u32) {
                let key=format!("GET:{}:{}",spec.key,clean);
                tx.execute("insert into ext_http_cache(cache_key,source,url,status,body_zst,fetched_at) values($1,$2,$3,200,$4,$5)
                    on conflict(cache_key) do update set source=excluded.source,url=excluded.url,status=excluded.status,body_zst=excluded.body_zst,fetched_at=excluded.fetched_at", &[&key,&spec.key,&clean,&compressed,&now])?;
            }
            Ok(())
        })
    }
}

enum Trouble { Failed(String), RateLimited(i64), Stopped, Storage(anyhow::Error) }
struct Poller<'a> {
    service:&'a Service,started:Instant,fetch:&'a dyn Fetch,spec:&'a FeedSpec,page_size:u32,delay:Duration,
    pause:&'a dyn Fn(Duration)->bool,now:i64,first:bool,rows:Option<i64>,in_history:bool,
}
impl Poller<'_> {
    fn now_after(&self)->i64 {self.now.saturating_add(i64::try_from(self.started.elapsed().as_secs()).unwrap_or(i64::MAX))}
    fn page(&mut self,page:i64)->Result<Vec<Entry>,Trouble> {
        let page=u32::try_from(page).unwrap_or(u32::MAX);let url=page_url(self.spec,page,self.page_size);
        let (reply,cached)=if let Some(body)=self.service.cached(self.spec,&url,page).map_err(Trouble::Storage)? {
            self.first=false;(HttpReply {status:200,retry_after:None,rate_reset:None,body},true)
        } else {
            if !self.first && !(self.pause)(self.delay) {return Err(Trouble::Stopped)}
            self.first=false;self.service.cache.check().map_err(Trouble::Storage)?;
            (self.fetch.get(&url).map_err(Trouble::Failed)?,false)
        };
        if reply.status==429 || (reply.status==403 && self.spec.key=="predatabase") {
            return Err(Trouble::RateLimited(wait_seconds(&reply,self.now_after(),RATE_LIMIT_DEFAULT)))
        }
        if self.in_history && page>1 && matches!(reply.status,400|404) {return Ok(vec![])}
        if reply.status!=200 {return Err(Trouble::Failed(format!("HTTP {}",reply.status)))}
        if reply.body.len()>MAX_BODY {return Err(Trouble::Failed("PreDB response exceeds admission".into()))}
        let parsed=match self.spec.parse {
            Parser::Json(parse)=>{let value=serde_json::from_str(&reply.body).map_err(|_|Trouble::Failed("invalid PreDB JSON".into()))?;parse(&value,page)},
            Parser::Text(parse)=>parse(&reply.body),
        }.map_err(|error|match error {ParseError::RateLimited(_)=>Trouble::RateLimited(wait_seconds(&reply,self.now_after(),RATE_LIMIT_DEFAULT)),ParseError::Shape(why)=>Trouble::Failed(why)})?;
        if parsed.len()>5000 {return Err(Trouble::Failed("too many titles in one answer".into()))}
        if !cached && !parsed.is_empty() {self.service.archive(self.spec,&url,page,&reply.body).map_err(Trouble::Storage)?;}
        if self.rows.is_none() {self.rows=Some(parsed.len() as i64)}
        Ok(parsed)
    }
}
fn past_the_end(spec: &FeedSpec, page: i64) -> bool {
    spec.max_page.is_some_and(|m| page > i64::from(m))
}

fn set_cursor(store: &Service, source:i64, page:i64, floor:i64)->Result<()> {
    store.execute("update ext_predb_feeds set resume_page=$1,gap_floor=$2 where source=$3", &[&page,&floor,&source])?; Ok(())
}

struct Row {
    failures: i64,
    resume_page: i64,
    gap_floor: i64,
    history_page: i64,
    history_read: i64,
    last_ok_at: Option<i64>,
}

/// the steps of a poll that use the network: what ended them, or how they came out
fn walk(conn: &Service, p: &mut Poller<'_>, row: &Row, cfg: &PredbConfig) -> Result<bool, Failure> {
    let spec = p.spec;
    let (source, now) = (spec.id, p.now);
    let fresh = row.last_ok_at.is_none();
    let open = row.resume_page;
    // page 1 takes a request every poll, soo owed work needs a second one or it would never get its turn
    let owed = open > 0 || row.history_read < i64::from(cfg.history_pages);
    let mut budget = i64::from(cfg.max_pages_per_poll.max(if owed { 2 } else { 1 }));
    // the open gap's floor stays: walking down from a new cursor passes through what the old one left
    let floor = if open > 0 {
        row.gap_floor
    } else {
        conn.query("select coalesce(max(id), 0) from ext_predb_titles", &[])?.try_get(0).map_err(anyhow::Error::from)?
    };

    // forward, newest first
    let (mut page, mut caught, mut ended) = (1i64, false, false);
    while budget > 0 {
        if past_the_end(spec, page) {
            ended = true;
            break;
        }
        let entries = p.page(page)?;
        budget -= 1;
        if entries.is_empty() {
            ended = true;
            break;
        }
        let cursor = (!fresh).then_some((page + 1, floor));
        let counts = conn.import(source, &entries, now, cursor)?;
        if counts.inserted == 0 {
            caught = true;
            break;
        }
        page += 1;
    }
    if ended {
        // nothing left below: no gap, whichever was open
        set_cursor(conn, source, 0, 0)?;
    } else if !caught {
        // out of budget. The cursor says where to go on, unless older pages are history
        if fresh {
            set_cursor(conn, source, 0, 0)?;
        } else {
            return Ok(true);
        }
        return Ok(false);
    } else {
        set_cursor(conn, source, open, if open > 0 { row.gap_floor } else { 0 })?;
    }

    // the gap, down to a title from before it
    let mut in_gap = caught && open > 0;
    let mut page = open;
    while in_gap {
        if budget == 0 {
            set_cursor(conn, source, page, row.gap_floor)?;
            return Ok(true);
        }
        if past_the_end(spec, page) {
            in_gap = false;
            continue;
        }
        let entries = p.page(page)?;
        budget -= 1;
        if entries.is_empty() {
            in_gap = false;
            continue;
        }
        let counts = conn.import(source, &entries, now, Some((page + 1, row.gap_floor)))?;
        if counts.inserted == 0 && counts.min_known_id.is_some_and(|m| m <= row.gap_floor) {
            in_gap = false;
            continue;
        }
        page += 1;
        set_cursor(conn, source, page, row.gap_floor)?;
    }
    if caught && open > 0 {
        set_cursor(conn, source, 0, 0)?;
    }

    // history, on purpose only
    p.in_history = true;
    let (mut read, mut page) = (row.history_read, row.history_page.max(2));
    while budget > 0 && read < i64::from(cfg.history_pages) {
        if past_the_end(spec, page) {
            read = i64::from(cfg.history_pages);
            conn.execute("update ext_predb_feeds set history_read=$1 where source=$2", &[&read,&source])?;
            break;
        }
        let entries = p.page(page)?;
        budget -= 1;
        if entries.is_empty() {
            read = i64::from(cfg.history_pages);
            conn.execute("update ext_predb_feeds set history_read=$1 where source=$2", &[&read,&source])?;
            break;
        }
        let counts = conn.import(source, &entries, now, None)?;
        page += 1;
        read += 1;
        conn.execute(
            "update ext_predb_feeds set history_page=$1,history_read=$2 where source=$3",
            &[&page,&read,&source],
        )?;
    }
    Ok(false)
}

enum Failure { Trouble(Trouble), Storage(anyhow::Error) }
impl From<Trouble> for Failure {fn from(value:Trouble)->Self {Self::Trouble(value)}}
impl From<anyhow::Error> for Failure {fn from(value:anyhow::Error)->Self {Self::Storage(value)}}
impl Service {
    fn failed(&self,source:i64,why:String,failures:i64,poll_secs:i64,now:i64)->Result<PollEnd> {
        let failures=failures.saturating_add(1);let wait=poll_secs.saturating_mul(1i64<<failures.clamp(0,16)).min(MAX_BACKOFF);
        self.execute("update ext_predb_feeds set failures=$1,last_error=$2,next_poll_at=$3 where source=$4", &[&failures,&why,&now.saturating_add(wait),&source])?;
        Ok(PollEnd::Failed(why))
    }
    fn poll(&self,fetch:&dyn Fetch,spec:&FeedSpec,cfg:&PredbConfig,now:i64,pause:&dyn Fn(Duration)->bool)->Result<PollEnd> {
        self.execute("insert into ext_predb_feeds(source,key) values($1,$2) on conflict(source) do nothing", &[&spec.id,&spec.key])?;
        let r=self.query("select key,failures,resume_page,gap_floor,history_page,history_read,last_ok_at from ext_predb_feeds where source=$1", &[&spec.id])?;
        ensure!(r.try_get::<_,String>(0)?==spec.key,"PreDB source registry mismatch");
        let row=Row {failures:r.try_get(1)?,resume_page:r.try_get(2)?,gap_floor:r.try_get(3)?,history_page:r.try_get(4)?,history_read:r.try_get(5)?,last_ok_at:r.try_get(6)?};
        if spec.refresh_secs>0 && row.last_ok_at.is_some_and(|last|now.saturating_sub(last)<spec.refresh_secs) {
            self.execute("update ext_predb_feeds set next_poll_at=$1 where source=$2", &[&row.last_ok_at.unwrap().saturating_add(spec.refresh_secs),&spec.id])?;
            return Ok(PollEnd::CaughtUp)
        }
        let poll_secs=i64::from(cfg.poll_minutes.max(1))*60;
        let bytes:i64=self.query("select (pg_total_relation_size('ext_predb_titles')+pg_total_relation_size('ext_predb_title_keys'))::bigint", &[])?.try_get(0)?;
        if bytes>cfg.max_db_mb.saturating_mul(1<<20) {return self.failed(spec.id,"PreDB cache exceeds max_db_mb".into(),row.failures,poll_secs,now)}
        let mut poller=Poller {service:self,started:Instant::now(),fetch,spec,page_size:cfg.page_size,delay:Duration::from_millis(cfg.request_delay_ms.max(spec.min_delay_ms)),pause,now,first:true,rows:None,in_history:false};
        let walked=match walk(self,&mut poller,&row,cfg) {
            Err(Failure::Storage(error)|Failure::Trouble(Trouble::Storage(error)))=>return Err(error),
            result=>result,
        };let answered_at=poller.now_after();
        self.execute("update ext_predb_feeds set last_rows=coalesce($1,last_rows) where source=$2", &[&poller.rows,&spec.id])?;
        match walked {
            Ok(gap)=>{self.execute("update ext_predb_feeds set failures=0,last_ok_at=$1,last_error=null,next_poll_at=$2 where source=$3", &[&now,&now.saturating_add(poll_secs).saturating_add(spec.id*7),&spec.id])?;Ok(if gap {PollEnd::Gap}else{PollEnd::CaughtUp})},
            Err(Failure::Storage(error)|Failure::Trouble(Trouble::Storage(error)))=>Err(error),
            Err(Failure::Trouble(Trouble::Stopped))=>Ok(PollEnd::Stopped),
            Err(Failure::Trouble(Trouble::Failed(why)))=>self.failed(spec.id,why,row.failures,poll_secs,answered_at),
            Err(Failure::Trouble(Trouble::RateLimited(wait)))=>{self.execute("update ext_predb_feeds set next_poll_at=$1,last_error='rate limited' where source=$2", &[&answered_at.saturating_add(wait),&spec.id])?;Ok(PollEnd::RateLimited(wait))},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TITLE:&str="Alpha.Beta.Gamma.2024.1080p.WEB-DL.H.264-TESTGRP";

    #[test]
    fn native_predb_service_refuses_an_unvalidated_owner() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let owner=Arc::new(crate::pg::indexer_runtime::Owner::new(postgres::Client::connect(&schema.url,postgres::NoTls).unwrap()));
        assert!(Service::new(&owner).is_err());
    }

    /// Root supplies the real loopback HTTP oracle and independently observes
    /// failed SQLite opens/existing-handle access and all sockets for this fresh
    /// child. No legacy oracle/cache is initialized in the native process.
    #[test]
    #[ignore="Root-only owned native PG home plus actual loopback HTTP/OS observer"]
    fn configured_native_predb_child() {
        let action=std::env::var("ATLAS_NATIVE_PREDB_CASE").expect("owned PreDB action");
        let runtime=crate::raw_store::native_pg_opensubtitles_cache_tests::open_test_native_owner().expect("canonical native owner");
        let service=Service::new(&runtime.owner).unwrap();
        while matches!(service.cache.predb_advance(512).unwrap(),BuildStatus::Pending {..}) {}
        let endpoint=std::env::var("ATLAS_NATIVE_PREDB_HTTP").expect("owned loopback endpoint");
        assert!(endpoint.starts_with("http://127.0.0.1:"));
        let endpoint:&'static str=Box::leak(endpoint.into_boxed_str());
        let real=crate::predb::feeds::spec("predb_club").unwrap();
        let spec=FeedSpec { id:real.id,key:real.key,endpoint,page_size_param:real.page_size_param,min_page_size:real.min_page_size,max_page_size:real.max_page_size,
            extra_query:real.extra_query,max_page:Some(1),min_delay_ms:0,parse:real.parse,paging:real.paging,refresh_secs:0 };
        let cfg=PredbConfig {enabled:true,sources:vec![spec.key.into()],max_pages_per_poll:1,history_pages:0,request_delay_ms:0,..Default::default()};
        let fetch=crate::predb::poll::UreqFetch::new(Duration::from_secs(5));let now=chrono::Utc::now().timestamp();
        match action.as_str() {
            "feed"=>{
                assert_eq!(service.poll(&fetch,&spec,&cfg,now,&|_|true).unwrap(),PollEnd::CaughtUp);
                assert_eq!(service.poll(&fetch,&spec,&cfg,now+1,&|_|true).unwrap(),PollEnd::CaughtUp);
                runtime.owner.with(|client| {
                    let row=client.query_one("select title,category,size_bytes from ext_predb_titles where title=$1", &[&TITLE])?;
                    assert_eq!(row.try_get::<_,String>(0)?,TITLE);assert_eq!(row.try_get::<_,String>(1)?,"TV");
                    assert_eq!(row.try_get::<_,i64>(2)?,5*1024*1024);
                    let rows:i64=client.query_one("select count(*) from ext_predb_pages where source_id=$1", &[&spec.id])?.try_get(0)?;assert_eq!(rows,1);
                    let events:i64=client.query_one("select count(*) from ext_predb_page_events", &[])?.try_get(0)?;assert_eq!(events,1);
                    let saved=client.query_one("select inserted,updated,failures,last_rows from ext_predb_feeds where source=$1", &[&spec.id])?;
                    assert_eq!(saved.try_get::<_,i64>(0)?,1);assert_eq!(saved.try_get::<_,i64>(1)?,0);assert_eq!(saved.try_get::<_,i64>(2)?,0);assert_eq!(saved.try_get::<_,i64>(3)?,1);
                    let mut tx=client.build_transaction().isolation_level(postgres::IsolationLevel::RepeatableRead).read_only(true).start()?;
                    let evidence=crate::predb::matcher::Evidence {name:TITLE,display_name:None,size:5<<20,files:vec![],inner:vec![]};
                    let crate::pg::predb_cache::LookupStatus::Ready(found)=crate::pg::predb_cache::find_in(&mut tx,&evidence)? else {panic!("real feed did not complete matcher keys")};
                    assert_eq!(found.found.unwrap().title.title,TITLE);tx.commit()?;Ok(())
                }).unwrap();
            },
            "rate_limited"=>{
                let PollEnd::RateLimited(wait)=service.poll(&fetch,&spec,&cfg,now,&|_|true).unwrap() else {panic!("expected actual HTTP429")};
                assert!(wait>=5);
                let row=service.query("select next_poll_at,last_error from ext_predb_feeds where source=$1", &[&spec.id]).unwrap();
                assert!(row.get::<_,i64>(0)>=now+5);assert_eq!(row.get::<_,String>(1),"rate limited");
            },
            "negative"=>{
                assert_eq!(service.poll(&fetch,&spec,&cfg,now,&|_|true).unwrap(),PollEnd::Failed("HTTP 404".into()));
                assert_eq!(service.query("select count(*) from ext_predb_pages", &[]).unwrap().get::<_,i64>(0),0);
                assert_eq!(service.query("select failures from ext_predb_feeds where source=$1", &[&spec.id]).unwrap().get::<_,i64>(0),1);
            },
            "unavailable"=>{
                runtime.owner.with(|client|{client.batch_execute("drop table ext_predb_feeds")?;Ok(())}).unwrap();
                let error=service.poll(&fetch,&spec,&cfg,now,&|_|true).unwrap_err();
                assert!(error.chain().any(|cause|cause.downcast_ref::<postgres::Error>().is_some()));
            },
            _=>panic!("unknown owned PreDB case"),
        }
        println!("native PreDB child completed: {action}; independent Root HTTP/OS observer required");
    }
}
