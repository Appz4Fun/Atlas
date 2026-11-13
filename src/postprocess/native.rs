//! Fresh native worker construction. Runtime admission remains a separate gate.

use std::sync::Arc;
use anyhow::{Result,ensure};
use super::step::{Step,StepMask,Needs,mask};
#[path="native_net.rs"]
mod net;
#[path="native_workers.rs"]
mod workers;
#[path="native_telemetry.rs"]
mod telemetry;
pub(crate) use telemetry::{snapshot as stats_snapshot,backlog as backlog_snapshot,stats_json};
pub(crate) use workers::run;
#[path="native_config.rs"]
mod settings;
pub(crate) use settings::ConfigPin;
#[cfg(test)]
#[path="native_provider_tests.rs"]
mod provider_tests;
#[cfg(all(test,unix))]
#[path="native_inferred_tests.rs"]
mod inferred_tests;
#[cfg(test)]
#[path="native_workers_tests.rs"]
pub(crate) mod worker_tests;
#[cfg(test)]
#[path="native_stop_tests.rs"]
pub(crate) mod stop_tests;
#[cfg(test)]
#[path="native_asset_tests.rs"]
pub(crate) mod asset_tests;

pub(crate) struct Steps {
    pub(crate) ordered:Vec<Arc<dyn Step>>,
    pub(crate) enabled:StepMask,
    pub(crate) database:StepMask,
    pub(crate) network:StepMask,
    pub(crate) on_growth:StepMask,
    pub(crate) stamps:Vec<(u8,StepMask,i64)>,
}
impl Steps {
    pub(crate) fn for_owner(owner:&Arc<crate::pg::indexer_runtime::Owner>,cfg:&crate::config::Config)->Result<Self> {
        ensure!(tokio::runtime::Handle::try_current().is_err(),"native worker factory requires a standard worker thread");
        owner.with(|_|Ok(()))?;
        // The three media objects capture the checked PG backend. Calling
        // steps::all would initialize the legacy shared media service instead.
        let [parse,lookup,nfo]=crate::media::native_steps(owner,cfg)?;
        let mut ordered:Vec<Arc<dyn Step>>=vec![
            Arc::new(crate::archive::steps::Par2Step::default()),
            Arc::new(crate::archive::steps::ListStep::default()),
            Arc::new(crate::archive::link::LinkStep::default()),
            Arc::new(crate::probe::ProbeStep::default()),
            Arc::new(crate::probe::body_crc::BodyCrcStep),
            Arc::new(crate::probe::opensubtitles::OpenSubtitlesStep::default()),
            Arc::new(crate::probe::stills::StillsStep),
            Arc::new(crate::naming::steps::Naming),Arc::new(crate::naming::steps::NfoName),
            Arc::new(crate::sidecar::SidecarStep),Arc::new(crate::sidecar::SidecarImageStep),
            Arc::new(crate::images::step::ImagesStep::default()),Arc::new(crate::predb::step::PredbMatch),
            Arc::new(crate::category::CategoryStep::new()),parse,lookup,nfo,
            Arc::new(crate::probe::opensubtitles::OpenSubtitlesImdb),
            Arc::new(crate::keys::KeysStep::default()),Arc::new(crate::probe::audio_print::AudioPrintStep),
        ];
        ordered.sort_by_key(|step|step.rank());
        let (mut enabled,mut database,mut network,mut on_growth)=(0,0,0,0);let mut stamps=Vec::new();
        let mut ids=std::collections::HashSet::new();let mut ranks=std::collections::HashSet::new();
        for step in &ordered {
            ensure!(ids.insert(step.id().0) && ranks.insert(step.rank()),"native step registry has a duplicate identity");
            let bit=mask(step.id());
            if step.enabled(cfg) {enabled|=bit;}
            if step.needs()==Needs::Database {database|=bit;} else {network|=bit;}
            if step.reruns().on_growth {on_growth|=bit;}
            stamps.push((step.id().0,bit,step.config_stamp() as i64));
        }
        owner.with(|_|Ok(()))?;
        Ok(Self {ordered,enabled,database,network,on_growth,stamps})
    }
}

/// Holds live Step claims until the original owner's commit answer. Dropping
/// an unsubmitted result stops its attempt before those claims are released.
pub(crate) struct Prepared {
    visit:Option<crate::pg::stage_queue::Visit>,
    owner:Arc<crate::pg::indexer_runtime::Owner>,
    holds:Vec<Box<dyn std::any::Any+Send>>,
}
fn fail(owner:&crate::pg::indexer_runtime::Owner,error:&anyhow::Error) {
    let stopped=crate::pg::outage::is_attempt_ended(error)
        && owner.stop.load(std::sync::atomic::Ordering::Acquire) && owner.failure().is_none();
    if !stopped {owner.fail(crate::pg::outage::classify(error));}
}

/// Created only after every retained handle has really joined. Pending claims
/// are the ones returned by those workers, never caller-supplied numeric IDs.
pub(crate) struct JoinedWorkers {
    owner:std::sync::Weak<crate::pg::indexer_runtime::Owner>,epoch:[u8;32],
    pending:Vec<crate::pg::stage_queue::Claim>,
}
impl JoinedWorkers {
    pub(crate) fn for_owner(&self,expected:&crate::pg::indexer_runtime::Owner)->bool {
        self.owner.upgrade().is_some_and(|owner|std::ptr::eq(owner.as_ref(),expected))
            && expected.epoch().is_some_and(|epoch|epoch.id()==&self.epoch)
    }
    pub(crate) fn pending(&self)->&[crate::pg::stage_queue::Claim] {&self.pending}
}
type Worker=std::thread::JoinHandle<Result<Option<crate::pg::stage_queue::Claim>>>;
fn join_workers(owner:&Arc<crate::pg::indexer_runtime::Owner>,workers:Vec<Worker>)->Result<JoinedWorkers> {
    owner.stop.store(true,std::sync::atomic::Ordering::Release);
    let mut pending=Vec::new();let mut first=None;
    // Always drain every handle, including the siblings of a failed/panicked
    // worker. No error branch detaches one or permits a replacement owner.
    for worker in workers {
        let result=match worker.join() {Ok(result)=>result,Err(_)=>Err(anyhow::anyhow!("native worker panicked"))};
        match result {
            Ok(Some(claim))=>pending.push(claim),Ok(None)=>{},
            Err(error)=>{
                fail(owner,&error);
                // Siblings rejected during drain must not replace the actual
                // storage/invariant cause with the secondary cancellation.
                if first.as_ref().is_none_or(|prior| {
                    (crate::pg::outage::is_attempt_ended(prior) && !crate::pg::outage::is_attempt_ended(&error))
                        || (crate::pg::outage::classify(prior)==crate::pg::outage::Failure::Unavailable
                            && crate::pg::outage::classify(&error)==crate::pg::outage::Failure::Fatal)
                }) {first=Some(error);}
            },
        }
    }
    if let Some(error)=first {return Err(error)}
    let epoch=*owner.epoch().ok_or_else(||anyhow::anyhow!("joined workers have no native owner epoch"))?.id();
    Ok(JoinedWorkers {owner:Arc::downgrade(owner),epoch,pending})
}
impl Prepared {
    pub(crate) fn settle(mut self,queue:&crate::pg::stage_queue::Queue,claim:&crate::pg::stage_queue::Claim,now:i64)->Result<crate::pg::stage_queue::Settlement> {
        for hold in &self.holds {
            if let Some(cover)=hold.downcast_ref::<crate::pg::worker_assets::CoverHold>()
                && let Err(error)=cover.check(&self.owner,claim.release_id()) {
                fail(&self.owner,&error);return Err(error)
            }
        }
        for hold in &self.holds {
            if let Some(images)=hold.downcast_ref::<crate::pg::worker_assets::ImageFrame>()
                && let Err(error)=images.check(&self.owner,claim.release_id()) {
                fail(&self.owner,&error);return Err(error)
            }
        }
        let visit=self.visit.take().expect("prepared native visit");
        let result=queue.settle(claim,visit,now);
        if let Err(error)=&result {fail(&self.owner,error);}
        self.holds.clear();result
    }
}
impl Drop for Prepared {
    fn drop(&mut self) {
        if self.visit.is_some() && !self.owner.stop.load(std::sync::atomic::Ordering::Acquire) {
            self.owner.fail(crate::pg::outage::Failure::Fatal);
        }
    }
}

/// One runner per actual attempt, shared by its standard workers through Arc.
/// Its body semaphore, token bucket and trouble pause are shared, not multiplied
/// per thread. No SQLite path exists here; construction admits no thread.
pub(crate) struct Runner {
    owner:Arc<crate::pg::indexer_runtime::Owner>,steps:Steps,
    config:crate::config::Config,
    assets:Option<Arc<crate::pg::worker_assets::Grant>>,
    bodies:super::bodies::BodyFetcher,stats:Arc<super::stats::Stats>,
    net_troubles:std::sync::atomic::AtomicI64,net_paused_until:std::sync::atomic::AtomicI64,
}
impl Runner {
    pub(crate) fn new(owner:&Arc<crate::pg::indexer_runtime::Owner>,config:&crate::config::Config,pool:Arc<crate::nntp::Pool>,handle:tokio::runtime::Handle)->Result<Self> {
        let assets=crate::pg::worker_assets::Grant::open(owner,config.native_assets.as_ref())?;
        let steps=Steps::for_owner(owner,config)?;
        let stats=Arc::new(super::stats::Stats::default());
        let bodies=super::bodies::BodyFetcher::new(pool,handle,owner.stop.clone(),config.postprocess.clone(),stats.clone());
        Ok(Self {owner:owner.clone(),steps,config:config.clone(),assets,bodies,stats,
            net_troubles:std::sync::atomic::AtomicI64::new(0),net_paused_until:std::sync::atomic::AtomicI64::new(0)})
    }
    pub(crate) fn database(&self,claim:&crate::pg::stage_queue::Claim,redo:StepMask,now:i64)->Result<Prepared> {
        let mut holds=Vec::new();
        let result=(|| {
            ensure!(redo==claim.redo_mask(),"native redo must belong to the actual durable walk claim");
            let reads=crate::pg::stage_reads::StepReads::for_owner(&self.owner)?;
            let (mut applied,input)=self.database_result(claim,redo,now,&mut holds,&reads)?;
            if claim.is_walk() {applied.claimed_due=None;}
            crate::pg::stage_queue::Visit::prepare_observed(claim,applied,input,reads.observed()?)
        })();
        match result {
            Ok(visit)=>Ok(Prepared {visit:Some(visit),owner:self.owner.clone(),holds}),
            Err(error)=>{fail(&self.owner,&error);Err(error)},
        }
    }
    fn database_result(&self,claim:&crate::pg::stage_queue::Claim,redo:StepMask,now:i64,holds:&mut Vec<Box<dyn std::any::Any+Send>>,reads:&crate::pg::stage_reads::StepReads)->Result<(super::apply::Applied,[u8;32])> {
        use super::{apply::Applied,engine::{self,Ran},rules::{self,RowFacts,Rules,Verdict,BodyGate},step::{StepCtx,StepError,StepWrite,StepMutation}};
        use std::{sync::atomic::Ordering::Relaxed,time::Instant};
        let _scope=crate::pg::cache_actor::NativeCacheScope::bind_with_assets(&self.owner,self.assets.as_ref())?;
        let Some(row)=reads.snapshot(claim.release_id())? else {
            return Ok((Applied {release_id:claim.release_id(),no_state:true,..Default::default()},[0;32]));
        };
        let input=row.input;
        let base=Applied::native_database(row.info.id,row.rerun_gen,claim.due_at(),self.steps.database,self.steps.network);
        let facts=RowFacts {group:&row.info.group,name:&row.info.name,size:row.info.size,parts:row.info.parts,complete:row.info.complete,file_total:row.info.file_total};
        let rules=Rules::new(&self.config.postprocess);
        let reject=|reason|->Result<(Applied,[u8;32])> {
            self.stats.rejected(reason);
            let junk=if reason==rules::Reason::Executable || row.summary.executable {Some(crate::junk::EXECUTABLE)} else {reads.inner_junk(row.info.id,row.summary.has_video)?};
            Ok((Applied {junk,no_state:!row.has_state,..Applied::native_database(row.info.id,row.rerun_gen,claim.due_at(),self.steps.database,self.steps.network)},input))
        };
        if let Some(reason)=rules::judge_row(&rules,&facts) {return reject(reason)}
        match rules::judge_files(&rules,&facts,&row.summary,now) {
            Verdict::Reject(reason)=>return reject(reason),
            Verdict::Wait(until)=>{
                let clear=if row.has_state {redo}else{0};
                return Ok((Applied {queue_at:Some(until),clear,no_state:clear==0,..base},input))
            }
            Verdict::Pass=>{}
        }
        if let Some(reason)=reads.inner_junk(row.info.id,row.summary.has_video)? {return Ok((Applied {junk:Some(reason),..base},input))}
        let stale_pc=reads.stale_pc(row.info.id,row.summary.has_video)?;
        let mut clear=redo;if row.has_state && row.info.parts>row.parts_seen {clear|=self.steps.on_growth;}
        let (mut done,mut failed)=(row.done & !clear,row.failed & !clear);let mut a=base;
        if stale_pc {a.writes.push((0,StepWrite::for_mutation(row.info.id,StepMutation::DropPcCategory)));}
        a.parts_seen=Some(row.info.parts);
        let mut ctx=StepCtx {release:row.info.clone(),data:super::data::StepData::Native(reads),facts:Vec::new(),bodies:&self.bodies,
            used_bodies:0,deadline:Instant::now()+engine::STEP_DEADLINE,lane_can_fetch:false,crcs:Vec::new(),pictures:false};
        let mut failure=false;let mut queue_at:Option<i64>=None;
        let mut soonest=|at:i64|queue_at=Some(queue_at.map_or(at,|old|old.min(at)));
        for (at,step) in self.steps.ordered.iter().enumerate() {
            let bit=mask(step.id());
            if step.needs()!=Needs::Database || self.steps.enabled & bit==0 || (done|failed)&bit!=0 || !step.applies(&ctx.release) {continue}
            self.owner.with(|_|Ok(()))?;ctx.deadline=Instant::now()+engine::STEP_DEADLINE;
            match engine::run_step(step.as_ref(),&mut ctx) {
                Ran::Outcome(mut outcome)=>{
                    holds.extend(outcome.hold.take());
                    if let Some(reason)=outcome.junk {return Ok((Applied {junk:Some(reason),..Applied::native_database(a.release_id,a.claimed_gen,claim.due_at(),a.db_mask,a.net_steps)},input))}
                    if let Some(until)=outcome.wait_until {
                        a.writes.extend(std::mem::take(&mut outcome.writes).into_iter().map(|write|(bit,write)));soonest(until);continue
                    }
                    self.stats.steps_done.fetch_add(1,Relaxed);
                    if outcome.matched {self.stats.matched.fetch_add(1,Relaxed);} else {self.stats.unmatched.fetch_add(1,Relaxed);}
                    a.set_done|=bit;done|=bit;failed&=!bit;
                    a.writes.extend(std::mem::take(&mut outcome.writes).into_iter().map(|write|(bit,write)));ctx.facts.append(&mut outcome.facts);
                    let rerun=outcome.rerun & self.steps.ordered[at+1..].iter().fold(0,|bits,step|bits|mask(step.id()));
                    clear|=rerun;done&=!rerun;failed&=!rerun;
                    if let Some(name)=outcome.name.take() {
                        if engine::name_counts(&ctx.release,&name) {
                            let renamed=name.display_name.as_ref().is_some_and(|name|ctx.release.display_name.as_ref()!=Some(name));
                            engine::apply_name(&mut ctx.release,&name);
                            if renamed {
                                let on_rename=self.steps.ordered.iter().filter(|later|later.rank()>step.rank() && later.reruns().on_rename).fold(0,|bits,step|bits|mask(step.id()));
                                done&=!on_rename;failed&=!on_rename;clear|=on_rename & self.steps.database;a.rename_clears|=on_rename & self.steps.network;
                            }
                        }
                        a.names.push((bit,name));
                    }
                }
                Ran::Error(StepError::Native(error))=>return Err(error),
                Ran::Error(StepError::Stopped)=>return if self.owner.stop.load(std::sync::atomic::Ordering::Acquire) {
                    Err(crate::pg::outage::AttemptEnded.into())
                } else {Err(anyhow::anyhow!("native Step stopped without an ended attempt"))},
                Ran::Error(StepError::Retry(error))=>{
                    self.stats.retries.fetch_add(1,Relaxed);let attempt=row.db_attempts+1;
                    if attempt>=self.config.postprocess.max_attempts {a.set_failed|=bit;failed|=bit;a.db_attempts=Some(0);} else {a.db_attempts=Some(attempt);soonest(now+engine::backoff(attempt,row.info.id));}
                    a.error=Some(engine::step_error(step.name(),&error));failure=true;
                }
                Ran::Error(StepError::Permanent(error))=>{a.set_failed|=bit;failed|=bit;a.error=Some(engine::step_error(step.name(),&error));}
                Ran::Error(StepError::Transient(error))=>{soonest(now+300);a.error=Some(engine::step_error(step.name(),&error));}
                Ran::Error(StepError::OutOfBudget)=>soonest(now+60),
            }
        }
        if !failure {a.db_attempts=Some(0);}
        for step in &self.steps.ordered {
            let bit=mask(step.id());
            if step.needs()==Needs::Database || self.steps.enabled & bit==0 || (done|failed)&bit!=0 || !step.applies(&ctx.release) {continue}
            if step.needs()==Needs::Bodies {
                match rules::body_gate(&rules,&facts,&row.summary) {
                    BodyGate::Outside if step.id()==crate::images::step::IMAGES_STEP && reads.pending_picture_request(row.info.id)?=>{},
                    BodyGate::Outside=>{a.set_done|=bit;done|=bit;self.stats.unmatched.fetch_add(1,Relaxed);continue},
                    BodyGate::NotYet=>continue,
                    BodyGate::Open=>{},
                }
            }
            a.net_mask|=bit;
        }
        a.clear=clear & !(a.set_done|a.set_failed);a.queue_at=queue_at;
        if a.clear & self.steps.network!=0 {a.net_next_at=Some(0);a.net_transient=Some(0);}
        a.no_state=!row.has_state && a.set_done==0 && a.set_failed==0 && a.clear==0 && a.rename_clears==0 && a.net_mask==0 && a.error.is_none() && a.names.is_empty();
        self.owner.with(|_|Ok(()))?;Ok((a,input))
    }
}

#[cfg(test)]
pub(crate) fn run_category_fixture(runtime:&crate::pg::indexer_runtime::Runtime) {
    // The fixture selects one REAL Step to qualify the runner/commit boundary;
    // the separate factory test inventories all20, not their runtime admission.
    let config=crate::config::Config::default();assert!(config.freeze.enabled);
    let handle=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut runner=Runner::new(&runtime.owner,&config,Arc::new(crate::nntp::Pool::new(&[])),handle.handle().clone()).unwrap();
    let category:Arc<dyn Step>=Arc::new(crate::category::CategoryStep::new());assert!(category.enabled(&config));let bit=mask(category.id());
    runner.steps=Steps {ordered:vec![category],enabled:bit,database:bit,network:0,on_growth:0,stamps:Vec::new()};
    crate::pg::stage_queue_tests::push(runtime);
    let queue=crate::pg::stage_queue::Queue::for_owner(&runtime.owner).unwrap();let claims=queue.claim(10,1,60).unwrap();assert_eq!(claims.len(),1);
    assert_eq!(runner.database(&claims[0],0,10).unwrap().settle(&queue,&claims[0],11).unwrap(),crate::pg::stage_queue::Settlement::Applied);
    runtime.owner.with(|client| {
        assert_eq!(client.query_one("select done from pp_state",&[])?.get::<_,i64>(0),bit);
        assert_eq!(client.query_one("select count(*) from releases",&[])?.get::<_,i64>(0),1);
        assert_eq!(client.query_one("select count(*) from freeze_pending",&[])?.get::<_,i64>(0),0);
        assert_eq!(client.query_one("select count(*) from pp_queue",&[])?.get::<_,i64>(0),0);Ok(())
    }).unwrap();
}

#[cfg(test)]
pub(crate) fn run_network_fixture(runtime:&crate::pg::indexer_runtime::Runtime) {
    let config=crate::config::Config::default();assert!(config.freeze.enabled);
    let handle=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut runner=Runner::new(&runtime.owner,&config,Arc::new(crate::nntp::Pool::new(&[])),handle.handle().clone()).unwrap();
    let category:Arc<dyn Step>=Arc::new(crate::category::CategoryStep::new());assert!(category.enabled(&config));let db=mask(category.id());
    let body_crc:Arc<dyn Step>=Arc::new(crate::probe::body_crc::BodyCrcStep);assert!(body_crc.enabled(&config));let net=mask(body_crc.id());
    let mut ordered=vec![category,body_crc];ordered.sort_by_key(|step|step.rank());
    runner.steps=Steps {ordered,enabled:db|net,database:db,network:net,on_growth:0,stamps:Vec::new()};
    crate::pg::stage_queue_tests::push(runtime);
    let queue=crate::pg::stage_queue::Queue::for_owner(&runtime.owner).unwrap();let claims=queue.claim(10,1,60).unwrap();
    runner.database(&claims[0],0,10).unwrap().settle(&queue,&claims[0],11).unwrap();
    let claims=queue.claim_net(12,1,60,0).unwrap();assert_eq!(claims.len(),1);
    let prepared=runner.network(&queue,&claims[0],12).unwrap().expect("real BodyCrc visit");
    runtime.owner.with(|client| {
        assert_eq!(client.query_one("select net_attempts from pp_state",&[])?.get::<_,i64>(0),1,"NET begin must persist before the completed Step is settled");Ok(())
    }).unwrap();
    assert_eq!(prepared.settle(&queue,&claims[0],13).unwrap(),crate::pg::stage_queue::Settlement::Applied);
    runtime.owner.with(|client| {
        let row=client.query_one("select done,net_attempts,net_wanted,native_net_token,native_net_before from pp_state",&[])?;
        assert_eq!(row.get::<_,i64>(0),db|net);assert_eq!(row.get::<_,i64>(1),0);assert_eq!(row.get::<_,Option<bool>>(2),None);assert_eq!(row.get::<_,Option<i64>>(3),None);
        assert_eq!(row.get::<_,Option<i64>>(4),None);
        assert_eq!(client.query_one("select count(*) from native_operations",&[])?.get::<_,i64>(0),2);
        assert_eq!(client.query_one("select count(*) from releases",&[])?.get::<_,i64>(0),1);Ok(())
    }).unwrap();
}

#[cfg(test)]
pub(crate) fn run_factory_fixture(runtime:&crate::pg::indexer_runtime::Runtime) {
    let cfg=crate::config::Config::default();
    let steps=Steps::for_owner(&runtime.owner,&cfg).unwrap();
    assert_eq!(steps.ordered.len(),20);
    let mut ids=std::collections::HashSet::new();let mut ranks=std::collections::HashSet::new();
    for step in &steps.ordered {assert!(ids.insert(step.id().0));assert!(ranks.insert(step.rank()));}
    assert_eq!(steps.ordered.iter().filter(|step|step.needs()==Needs::Database).fold(0,|mask,step|mask|super::step::mask(step.id())),steps.database);
    assert_eq!(steps.ordered.iter().filter(|step|step.needs()!=Needs::Database).fold(0,|mask,step|mask|super::step::mask(step.id())),steps.network);
    runtime.owner.close();assert!(Steps::for_owner(&runtime.owner,&cfg).is_err());
}
#[cfg(test)]
mod tests {
    #[test]
    fn native_factory_constructs_all_twenty_steps_and_refuses_an_ended_owner() {
        crate::raw_store::native_pg_opensubtitles_cache_tests::native_worker_parent(
            crate::raw_store::native_pg_opensubtitles_cache_tests::WorkerCase::Factory);
    }
}

#[cfg(all(test,unix))]
#[path="native_image_asset_tests.rs"]
pub(crate) mod image_asset_tests;

#[cfg(test)]
#[path = "../media/native_settlement_tests.rs"]
mod media_settlement_tests;
