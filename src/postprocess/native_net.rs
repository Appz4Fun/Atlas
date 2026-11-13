//! Native NET visits retain the original Step ordering, budgets and retries.
use anyhow::{Context,Result,ensure};
use std::{sync::atomic::Ordering::Relaxed,time::Instant};
use super::{Runner,Prepared,fail};
use crate::postprocess::{apply::{Applied,Lane},engine::{self,Ran},rules::{self,Rules,RowFacts,Verdict,BodyGate},step::{Needs,StepCtx,StepError,mask}};
use crate::pg::stage_queue::{Queue,Claim,Visit,NetBegin};

impl Runner {
    pub(crate) fn network_ready(&self,now:i64)->bool {now>=self.net_paused_until.load(Relaxed)}
    pub(crate) fn network(&self,queue:&Queue,claim:&Claim,now:i64)->Result<Option<Prepared>> {
        let mut holds=Vec::new();
        let reads=crate::pg::stage_reads::StepReads::for_owner(&self.owner)?;
        let result=self.network_result(queue,claim,now,&mut holds,&reads).and_then(|value|value.map(|(applied,input)|Visit::prepare_observed(claim,applied,input,reads.observed()?)).transpose());
        match result {
            Ok(visit)=>Ok(visit.map(|visit|Prepared {visit:Some(visit),owner:self.owner.clone(),holds})),
            Err(error)=>{fail(&self.owner,&error);Err(error)},
        }
    }
    fn network_result(&self,queue:&Queue,claim:&Claim,now:i64,holds:&mut Vec<Box<dyn std::any::Any+Send>>,reads:&crate::pg::stage_reads::StepReads)->Result<Option<(Applied,[u8;32])>> {
        ensure!(claim.lane()==Lane::Net,"native NET visit requires its NET lease");
        let _scope=crate::pg::cache_actor::NativeCacheScope::bind_with_assets(&self.owner,self.assets.as_ref())?;
        let Some(row)=reads.snapshot(claim.release_id())? else {
            return Ok(Some((Applied {release_id:claim.release_id(),lane:Lane::Net,no_state:true,..Default::default()},[0;32])))
        };
        let input=row.input;let id=row.info.id;
        let mut a=Applied::native_network(id,row.rerun_gen,self.steps.database,self.steps.network);
        if !self.network_ready(now) {
            a.net_mask=row.net_left.context("native NET state has no pending mask")?;
            a.net_next_at=Some(self.net_paused_until.load(Relaxed));return Ok(Some((a,input)))
        }
        let rules=Rules::new(&self.config.postprocess);
        let facts=RowFacts {group:&row.info.group,name:&row.info.name,size:row.info.size,parts:row.info.parts,complete:row.info.complete,file_total:row.info.file_total};
        let verdict=match rules::judge_row(&rules,&facts) {Some(reason)=>Verdict::Reject(reason),None=>rules::judge_files(&rules,&facts,&row.summary,now)};
        match verdict {
            Verdict::Reject(reason)=>{
                self.stats.rejected(reason);
                a.junk=if reason==rules::Reason::Executable || row.summary.executable {Some(crate::junk::EXECUTABLE)} else {reads.inner_junk(id,row.summary.has_video)?};
                return Ok(Some((a,input)))
            }
            Verdict::Wait(until)=>{a.net_mask=self.steps.network;a.net_next_at=Some(until);return Ok(Some((a,input)))},
            Verdict::Pass=>{},
        }
        let gate=rules::body_gate(&rules,&facts,&row.summary);let mut left=Vec::new();
        for step in &self.steps.ordered {
            let bit=mask(step.id());
            if step.needs()==Needs::Database || self.steps.enabled & bit==0 || (row.done|row.failed)&bit!=0 || !step.applies(&row.info) {continue}
            if step.needs()==Needs::Bodies {
                match gate {
                    BodyGate::Outside if step.id()==crate::images::step::IMAGES_STEP && reads.pending_picture_request(id)?=>{},
                    BodyGate::Outside=>{a.set_done|=bit;self.stats.unmatched.fetch_add(1,Relaxed);continue},
                    BodyGate::NotYet=>continue,
                    BodyGate::Open=>{},
                }
            }
            left.push(step);
        }
        a.net_mask=left.iter().fold(0,|bits,step|bits|mask(step.id()));
        let Some(first)=left.first() else {return Ok(Some((a,input)))};
        if row.net_attempts>=self.config.postprocess.max_attempts {
            a.set_failed|=mask(first.id());a.net_attempts=Some(0);a.net_transient=Some(0);a.net_next_at=Some(0);a.error=Some("interrupted".into());
            return Ok(Some((a,input)))
        }
        let started=Instant::now();
        let clock=||now.saturating_add(i64::try_from(started.elapsed().as_secs()).unwrap_or(i64::MAX));
        let mut waiting:Option<i64>=None;
        for step in left {
            match queue.begin_net(claim,clock(),engine::NET_CRASH_RETRY)? {
                NetBegin::Started {..}=>{},
                NetBegin::Superseded|NetBegin::LeaseLost|NetBegin::Gone=>return Ok(None),
            }
            let bit=mask(step.id());
            let mut ctx=StepCtx {release:row.info.clone(),data:crate::postprocess::data::StepData::Native(reads),facts:Vec::new(),
                bodies:&self.bodies,used_bodies:0,deadline:Instant::now()+engine::STEP_DEADLINE,lane_can_fetch:step.needs()==Needs::Bodies,crcs:Vec::new(),pictures:false};
            let ran=engine::run_step(step.as_ref(),&mut ctx);let now=clock();
            match ran {
                Ran::Outcome(mut outcome)=>{
                    self.net_troubles.store(0,Relaxed);holds.extend(outcome.hold.take());
                    if let Some(reason)=outcome.junk {return Ok(Some((Applied {junk:Some(reason),..Applied::native_network(id,a.claimed_gen,a.db_mask,a.net_steps)},input)))}
                    let other=self.steps.ordered.iter().filter(|other|other.id()!=step.id()).fold(0,|bits,step|bits|mask(step.id()));
                    a.writes.extend(std::mem::take(&mut outcome.writes).into_iter().map(|write|(bit,write)));
                    a.clear|=outcome.rerun & other;a.net_mask|=outcome.rerun & other & a.net_steps;
                    if let Some(until)=outcome.wait_until {
                        waiting=Some(waiting.map_or(until,|old|old.min(until)));a.net_attempts=Some(row.net_attempts);a.net_next_at=Some(until);continue
                    }
                    self.stats.steps_done.fetch_add(1,Relaxed);
                    if outcome.matched {self.stats.matched.fetch_add(1,Relaxed);} else {self.stats.unmatched.fetch_add(1,Relaxed);}
                    a.set_done|=bit;if a.clear & bit==0 {a.net_mask&=!bit;}
                    a.net_attempts=Some(0);a.net_transient=Some(0);a.net_next_at=Some(0);
                    if let Some(name)=outcome.name.take() {
                        let on_rename=self.steps.ordered.iter().filter(|later|later.rank()>step.rank() && later.reruns().on_rename).fold(0,|bits,step|bits|mask(step.id()));
                        a.rename_clears|=on_rename;a.net_mask|=on_rename & a.net_steps;a.names.push((bit,name));
                    }
                }
                Ran::Error(StepError::Native(error))=>return Err(error),
                Ran::Error(StepError::Stopped)=>return if self.owner.stop.load(std::sync::atomic::Ordering::Acquire) {Err(crate::pg::outage::AttemptEnded.into())} else {Err(anyhow::anyhow!("native Step stopped without an ended attempt"))},
                Ran::Error(StepError::Retry(error))=>{
                    self.stats.retries.fetch_add(1,Relaxed);let attempt=row.net_attempts+1;
                    if attempt>=self.config.postprocess.max_attempts {a.set_failed|=bit;a.net_attempts=Some(0);a.net_next_at=Some(0);} else {a.net_attempts=Some(attempt);a.net_next_at=Some(now+engine::backoff(attempt,id));}
                    a.net_transient=Some(0);a.error=Some(engine::step_error(step.name(),&error));
                }
                Ran::Error(StepError::Permanent(error))=>{a.set_failed|=bit;a.net_attempts=Some(0);a.net_transient=Some(0);a.net_next_at=Some(0);a.error=Some(engine::step_error(step.name(),&error));},
                Ran::Error(StepError::Transient(error))=>{
                    let streak=row.net_transient+1;a.net_attempts=Some(row.net_attempts);a.net_transient=Some(streak);a.net_next_at=Some(now+engine::transient_wait(streak,id));a.error=Some(engine::step_error(step.name(),&error));
                    let trouble=self.net_troubles.fetch_add(1,Relaxed)+1;let pause=engine::net_pause_secs(trouble);
                    if pause>0 {self.net_paused_until.store(now+pause,Relaxed);}
                }
                Ran::Error(StepError::OutOfBudget)=>{a.net_attempts=Some(row.net_attempts);a.net_next_at=Some(now+60);},
            }
            break;
        }
        if let Some(until)=waiting {a.net_next_at=Some(match a.net_next_at {Some(at) if at!=0=>at.min(until),_=>until});}
        self.owner.with(|_|Ok(()))?;Ok(Some((a,input)))
    }
}
