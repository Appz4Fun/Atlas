//! Standard native worker lanes. Every handle drains before the owner closes.
use super::{Runner,Worker,fail,join_workers};
use anyhow::{Result,ensure};
use std::{sync::{Arc,Mutex,atomic::{AtomicBool,AtomicI64,Ordering}},time::{Duration,Instant}};
use crate::pg::{indexer_runtime::Owner,stage_queue::{Queue,Claim,Settlement,WalkProgress},stage_telemetry::LaneCounters};

pub(super) struct Workers {
    owner:Arc<Owner>,handles:Option<Vec<Worker>>,lanes:Arc<LaneCounters>,
}
struct Dispatch {
    runner:Arc<Runner>,lanes:Arc<LaneCounters>,walk:Mutex<super::super::bodies::TokenBucket>,
    database_turn:AtomicI64,network_turn:AtomicI64,
}
fn now()->i64 {chrono::Utc::now().timestamp()}
fn stopped(owner:&Owner,error:&anyhow::Error)->bool {
    owner.stop.load(Ordering::Acquire) && owner.failure().is_none() && crate::pg::outage::is_attempt_ended(error)
}
fn nap(owner:&Owner,duration:Duration)->bool {
    let deadline=Instant::now()+duration;
    while !owner.stop.load(Ordering::Acquire) {
        let left=deadline.saturating_duration_since(Instant::now());if left.is_zero() {return false}
        std::thread::sleep(left.min(Duration::from_millis(25)));
    }true
}
// Feed polling's legacy callback returns true to continue after a delay.
// The worker nap helper instead returns true when interrupted by shutdown.
pub(super) fn predb_pause(owner:&Owner,duration:Duration)->bool {!nap(owner,duration)}
pub(super) fn spawn(owner:&Arc<Owner>,name:String,work:impl FnOnce()->Result<Option<Claim>>+Send+'static)->Result<Worker> {
    let owner=owner.clone();Ok(std::thread::Builder::new().name(name).spawn(move|| {
        let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
            .unwrap_or_else(|_|Err(anyhow::anyhow!("native worker panicked")));
        match result {
            Err(error) if stopped(&owner,&error)=>Ok(None),
            Err(error)=>{fail(&owner,&error);Err(error)},
            result=>result,
        }
    })?)
}
impl Workers {
    fn start(owner:&Arc<Owner>,config:&crate::config::Config,ctx:crate::indexer::PassContext,handle:tokio::runtime::Handle,db:crate::indexer::Db,headers:crate::config::HeaderStoreConfig)->Result<Self> {
        check_inferred_platform(config.inferred_parts)?;
        ensure!(config.postprocess.enabled,"native Stage was not configured");
        Self::start_runner_context(Arc::new(Runner::new(owner,config,ctx.pool.clone(),handle)?),db,headers,Some(ctx))
    }
    #[cfg(test)]
    pub(super) fn start_runner(runner:Arc<Runner>,db:crate::indexer::Db,headers:crate::config::HeaderStoreConfig)->Result<Self> {
        Self::start_runner_context(runner,db,headers,None)
    }
    fn start_runner_context(runner:Arc<Runner>,db:crate::indexer::Db,headers:crate::config::HeaderStoreConfig,ctx:Option<crate::indexer::PassContext>)->Result<Self> {
        ensure!(tokio::runtime::Handle::try_current().is_err(),"native workers must start on a standard thread");
        check_inferred_platform(runner.config.inferred_parts)?;
        let owner=runner.owner.clone();let queue=Queue::for_owner(&owner)?;
        let stamps=runner.steps.stamps.iter().copied().filter(|(_,bit,_)|runner.steps.enabled & bit!=0).collect::<Vec<_>>();
        queue.seed(runner.steps.enabled,&stamps,now())?;
        let lanes=Arc::new(LaneCounters::new(runner.steps.enabled as u64)?);
        let dispatch=Arc::new(Dispatch {runner,lanes:lanes.clone(),walk:Mutex::new(super::super::bodies::TokenBucket::new()),database_turn:AtomicI64::new(0),network_turn:AtomicI64::new(0)});
        let mut handles=Vec::new();
        let started:Result<()>=(|| {
            for id in 0..dispatch.runner.config.postprocess.workers.max(1) {
                let dispatch=dispatch.clone();handles.push(spawn(&owner,format!("atlas-native-db-{id}"),move||dispatch.database())?);
            }
            if dispatch.runner.steps.enabled & dispatch.runner.steps.network!=0 {
                for id in 0..dispatch.runner.config.postprocess.body_connections.max(1) {
                    let dispatch=dispatch.clone();handles.push(spawn(&owner,format!("atlas-native-net-{id}"),move||dispatch.network())?);
                }
            }
            let (pool,handle)=dispatch.runner.bodies.service_io();
            let telemetry=super::telemetry::Producer::new(&dispatch.runner,lanes.clone())?;
            start_services(&owner,&dispatch.runner.config,pool,handle,Some(lanes.clone()),Some(telemetry),dispatch.runner.assets.clone(),db,headers,ctx,&mut handles)?;
            Ok(())
        })();
        if let Err(error)=started {
            fail(&owner,&error);
            // Even partial startup owns every successful spawn until it joins.
            let _=join_workers(&owner,handles);lanes.stop();return Err(error)
        }
        Ok(Self {owner,handles:Some(handles),lanes})
    }
    fn drain(&mut self)->Result<()> {
        let Some(handles)=self.handles.take() else {return Ok(())};
        let joined=join_workers(&self.owner,handles);self.lanes.stop();
        let joined=joined?;
        // An outage or panic never refunds a spent interruption attempt.
        if self.owner.failure().is_none() {self.owner.finalize_native_stop(&joined,now())?;}
        Ok(())
    }
    pub(super) fn finish(mut self)->Result<()> {self.drain()}
}
impl Drop for Workers {
    fn drop(&mut self) {if let Err(error)=self.drain() {fail(&self.owner,&error);}}
}
impl Dispatch {
    fn database(&self)->Result<Option<Claim>> {
        let owner=&self.runner.owner;let queue=Queue::for_owner(owner)?;
        while !owner.stop.load(Ordering::Acquire) {
            let claims=queue.claim_turn(now(),1,3600,self.database_turn.fetch_add(1,Ordering::Relaxed))?;
            let mut worked=false;
            for claim in claims {
                self.lanes.queue_taken.fetch_add(1,Ordering::Relaxed);
                let settled=self.runner.database(&claim,claim.redo_mask(),now())?.settle(&queue,&claim,now())?;
                if claim.is_walk() && settled==Settlement::Applied {self.lanes.walk_rows.fetch_add(1,Ordering::Relaxed);}
                worked=true;
            }
            let admitted=self.walk.lock().map_err(|_|anyhow::anyhow!("native walk limiter ended"))?
                .take(self.runner.config.postprocess.backlog_per_second as f64,1.0,Instant::now()).is_some_and(|wait|wait.is_zero());
            if admitted {
                match queue.walk_tick(now())? {
                    WalkProgress::Skipped=>{self.lanes.walk_rows.fetch_add(1,Ordering::Relaxed);worked=true;},
                    WalkProgress::Queued|WalkProgress::Finished=>worked=true,
                    WalkProgress::Idle|WalkProgress::Blocked=>{},
                }
            }
            if !worked {nap(owner,Duration::from_millis(100));}
        }Ok(None)
    }
    fn network(&self)->Result<Option<Claim>> {
        let owner=&self.runner.owner;let queue=Queue::for_owner(owner)?;
        while !owner.stop.load(Ordering::Acquire) {
            if !self.runner.network_ready(now()) {nap(owner,Duration::from_millis(100));continue}
            let claims=queue.claim_net(now(),1,3600,self.network_turn.fetch_add(1,Ordering::Relaxed))?;
            if claims.is_empty() {nap(owner,Duration::from_millis(100));continue}
            for claim in claims {
                self.lanes.net_visits.fetch_add(1,Ordering::Relaxed);
                let result=self.runner.network(&queue,&claim,now()).and_then(|prepared|prepared.map(|prepared|prepared.settle(&queue,&claim,now())).transpose());
                match result {
                    Ok(Some(Settlement::Applied|Settlement::Replayed))=>{self.lanes.net_finished.fetch_add(1,Ordering::Relaxed);},
                    Ok(_)=>{},
                    Err(error) if stopped(owner,&error)=>return Ok(Some(claim)),
                    Err(error)=>return Err(error),
                }
            }
        }Ok(None)
    }
}

pub(super) fn details_service(owner:&Arc<Owner>,config:&crate::config::Config,assets:Option<Arc<crate::pg::worker_assets::Grant>>)->Result<Option<crate::media::details::native::Service>> {
    if !crate::media::details::native::Service::enabled(&config.media) {return Ok(None)}
    let assets=match assets {Some(assets)=>Some(assets),None=>crate::pg::worker_assets::Grant::open(owner,config.native_assets.as_ref())?};
    Ok(Some(crate::media::details::native::Service::new(owner,&config.media,assets)?))
}

pub(super) fn start_details(owner:&Arc<Owner>,mut service:crate::media::details::native::Service,handles:&mut Vec<Worker>)->Result<()> {
    let owner_work=owner.clone();
    handles.push(spawn(owner,"atlas-native-details".into(),move|| {
        while !owner_work.stop.load(Ordering::Acquire) {
            match service.tick(now())? {
                crate::media::details::native::Progress::Disabled=>break,
                crate::media::details::native::Progress::Waiting(wait)=>{nap(&owner_work,wait);},
                // Tick retains its actual provider/rate/page deadline. The next
                // call returns Waiting rather than adding a second throttle.
                crate::media::details::native::Progress::Idle
                |crate::media::details::native::Progress::Visited
                |crate::media::details::native::Progress::ArtworkUnavailable
                |crate::media::details::native::Progress::Deferred=>{},
            }
        }Ok(None)
    })?);Ok(())
}

pub(super) fn start_spotnet(owner:&Arc<Owner>,service:Arc<crate::spotnet::native::Service>,handles:&mut Vec<Worker>)->Result<()> {
    let owner_work=owner.clone();let spots=service.clone();
    handles.push(spawn(owner,"atlas-native-spots".into(),move|| {
        while !owner_work.stop.load(Ordering::Acquire) {
            let _=crate::spotnet::native::provider_pass(spots.spots_tick(now()))?;
            nap(&owner_work,spots.poll());
        }Ok(None)
    })?);
    if service.comments_enabled() {
        let owner_work=owner.clone();
        handles.push(spawn(owner,"atlas-native-spot-comments".into(),move|| {
            while !owner_work.stop.load(Ordering::Acquire) {
                let more=crate::spotnet::native::provider_pass(service.comments_tick(now()))?.is_some_and(|pass|pass.more);
                if !more {nap(&owner_work,service.poll());}
            }Ok(None)
        })?);
    }
    Ok(())
}

pub(super) fn check_inferred_platform(enabled:bool)->Result<()> {
    #[cfg(not(unix))] ensure!(!enabled,"native inferred repair requires Unix durable HEAD confinement");
    #[cfg(unix)] let _=enabled;
    Ok(())
}
#[cfg(unix)]
pub(super) fn start_inferred(owner:&Arc<Owner>,mut service:crate::inferred::native::Service,handle:tokio::runtime::Handle,handles:&mut Vec<Worker>)->Result<()> {
    let owner_work=owner.clone();
    handles.push(spawn(owner,"atlas-native-inferred".into(),move|| {
        while !owner_work.stop.load(Ordering::Acquire) {
            let progress=service.tick(&handle,now())?;
            if let Some(wait)=progress.waiting {nap(&owner_work,wait);}
        }Ok(None)
    })?);Ok(())
}

// Every service receives the same actual runtime Db and canonical Owner.
#[allow(clippy::too_many_arguments)]
fn start_services(owner:&Arc<Owner>,config:&crate::config::Config,pool:Arc<crate::nntp::Pool>,handle:tokio::runtime::Handle,lanes:Option<Arc<LaneCounters>>,telemetry:Option<super::telemetry::Producer>,assets:Option<Arc<crate::pg::worker_assets::Grant>>,db:crate::indexer::Db,headers:crate::config::HeaderStoreConfig,ctx:Option<crate::indexer::PassContext>,handles:&mut Vec<Worker>)->Result<()> {
    check_inferred_platform(config.inferred_parts)?;
    db.check_native_owner(owner)?;
    if pool.indexing_servers().len()>1 {
        let ctx=ctx.ok_or_else(||anyhow::anyhow!("native overview repair requires the original scheduler context"))?;
        ensure!(Arc::ptr_eq(&ctx.pool,&pool),"native overview scheduler pool changed");
        let store=crate::nxv_store::live::handle().ok_or_else(||anyhow::anyhow!("native overview repair requires the existing catalog header store"))?;
        let mut service=crate::repair::native::Service::new(owner,&db,ctx,handle.clone(),store,&headers,crate::bg_indexer::settings_of(config))?;
        let owner_work=owner.clone();handles.push(spawn(owner,"atlas-native-overview-repair".into(),move|| {
            while !owner_work.stop.load(Ordering::Acquire) {
                let progress=service.tick(now())?;
                if let Some(wait)=progress.waiting {nap(&owner_work,wait);}
            }Ok(None)
        })?);
    }
    #[cfg(unix)] if config.inferred_parts {
        let store=crate::nxv_store::live::handle().ok_or_else(||anyhow::anyhow!("native inferred repair requires the existing catalog header store"))?;
        let service=crate::inferred::native::Service::new(owner,&db,pool.clone(),store,&headers)?;
        start_inferred(owner,service,handle.clone(),handles)?;
    }
    #[cfg(not(unix))] let _=&headers;
    if config.spots.enabled {
        let service=Arc::new(crate::spotnet::native::Service::new(owner,db.clone(),config.spots.clone(),pool.clone(),handle.clone())?);
        start_spotnet(owner,service,handles)?;
    }
    let predb=if config.predb.enabled {Some(Arc::new(crate::pg::service_cache::predb::Service::new(owner)?))} else {None};
    let config=Arc::new(config.clone());
    if let Some(details)=details_service(owner,&config,assets)? {start_details(owner,details,handles)?;}
    if let Some(predb)=predb.as_ref() {
        let owner_work=owner.clone();let config=config.clone();let predb=predb.clone();
        handles.push(spawn(owner,"atlas-native-predb-feed".into(),move|| {
            let fetch=crate::predb::test_fetch().unwrap_or_else(||Arc::new(crate::predb::poll::UreqFetch::new(Duration::from_secs(config.predb.timeout_secs.max(1)))));
            while !owner_work.stop.load(Ordering::Acquire) {
                predb.tick(&config.predb,fetch.as_ref(),now(),&|duration|predb_pause(&owner_work,duration))?;
                nap(&owner_work,Duration::from_secs(1));
            }Ok(None)
        })?);
    }
    let owner_work=owner.clone();
    let mut refresh=crate::pg::stage_group_refresh::Refresh::new(owner,pool,handle)?;
    handles.push(spawn(owner,"atlas-native-cache".into(),move|| {
        let mut observed=None;
        let mut next_stats=Instant::now();
        while !owner_work.stop.load(Ordering::Acquire) {
            if let Some(telemetry)=&telemetry && Instant::now()>=next_stats {
                let rest=match telemetry.refresh()? {
                    super::telemetry::Refresh::Published=>Duration::from_secs(1),
                    super::telemetry::Refresh::Deferred(cause)=>{drop(cause);Duration::from_secs(5)},
                };
                next_stats=Instant::now()+rest;
            }
            if observed.is_none_or(|at:Instant|at.elapsed()>=Duration::from_secs(30)) {
                crate::pg::stage_telemetry::capture(&owner_work,lanes.as_deref())?;observed=Some(Instant::now());
            }
            match refresh.tick()? {
                crate::pg::stage_group_refresh::Tick::Idle=>{},
                crate::pg::stage_group_refresh::Tick::Stored(_capture)=>{},
                // The actual pool already records transport failures. A
                // refused LIST is not permission to replace the prior EOF.
                crate::pg::stage_group_refresh::Tick::ProviderUnavailable(error)=>drop(error),
            }
            // This lane never waits for feed HTTP. Its bounded transactions use
            // the same canonical owner; fanout still works with credentials off.
            if config.postprocess.enabled {
                crate::media::native_fanout(&owner_work)?;
                if let Some(predb)=&predb {predb.maintenance(&config.predb,now())?;}
            }
            nap(&owner_work,Duration::from_secs(1));
        }Ok(None)
    })?);
    Ok(())
}

/// Bridge only: all PG/cache factories and worker work happen on actual
/// standard threads. Entered Tokio's spawn_blocking lane is not treated as one.
/// The existing native profile still refuses activation before this is called.
pub(crate) async fn run(owner:Arc<Owner>,db:crate::indexer::Db,headers:crate::config::HeaderStoreConfig,config:crate::config::Config,ctx:crate::indexer::PassContext,stopping:impl Fn()->bool+Send+'static) {
    let handle=tokio::runtime::Handle::current();let ending=Arc::new(AtomicBool::new(false));
    let worker_owner=owner.clone();let worker_end=ending.clone();
    let worker=std::thread::Builder::new().name("atlas-native-stage".into()).spawn(move||->Result<()> {
        if !config.postprocess.enabled {
            let mut handles=Vec::new();
            if let Err(error)=start_services(&worker_owner,&config,ctx.pool.clone(),handle,None,None,None,db,headers,Some(ctx),&mut handles) {
                fail(&worker_owner,&error);let _=join_workers(&worker_owner,handles);return Err(error)
            }
            while !worker_end.load(Ordering::Acquire) && !worker_owner.stop.load(Ordering::Acquire) {std::thread::sleep(Duration::from_millis(25));}
            join_workers(&worker_owner,handles)?;return Ok(())
        }
        let stage=Workers::start(&worker_owner,&config,ctx,handle,db,headers)?;
        while !worker_end.load(Ordering::Acquire) && !worker_owner.stop.load(Ordering::Acquire) {std::thread::sleep(Duration::from_millis(25));}
        stage.finish()
    });
    let worker=match worker {Ok(worker)=>worker,Err(error)=>{fail(&owner,&error.into());return}};
    while !stopping() && !owner.stop.load(Ordering::Acquire) && !worker.is_finished() {tokio::time::sleep(Duration::from_millis(100)).await;}
    ending.store(true,Ordering::Release);
    owner.stop.store(true,Ordering::Release);
    let result=tokio::task::spawn_blocking(move||worker.join()).await;
    let result=match result {
        Ok(Ok(result))=>result,
        Ok(Err(_))=>Err(anyhow::anyhow!("native Stage coordinator panicked")),
        Err(error)=>Err(anyhow::Error::new(error).context("native Stage join failed")),
    };
    if let Err(error)=result {fail(&owner,&error);}
}

#[cfg(test)]
#[path = "../../tests/common/native_mixed_contract.rs"]
mod mixed_contract;
