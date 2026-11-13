//! Completed native Stage stats; BG is the sole stats.json/status.json writer.
use anyhow::{Result,ensure};
use std::{sync::{Arc,Weak,Mutex,atomic::Ordering},time::{Duration,Instant},path::PathBuf};
use serde_json::{Value,json};
use crate::pg::{indexer_runtime::{Owner,CacheDestination},stage::{OwnerEpoch},stage_stats::{self,Masks,Observation},stage_telemetry::LaneCounters};
use super::Runner;
const FRESH:Duration=Duration::from_secs(3);
const MAX_PAYLOAD:usize=64*1024;
const _:()=assert!(crate::catalog_facts::SHARDS==1);

struct Reading {at:Instant,value:Value,backlog:i64}
struct Slot {owner:Weak<Owner>,epoch:[u8;32],root:PathBuf,reading:Option<Reading>}
static CURRENT:Mutex<Option<Slot>>=Mutex::new(None);

pub(crate) struct Producer {
    runner:Arc<Runner>,owner:Arc<Owner>,destination:CacheDestination,epoch:OwnerEpoch,stats:Arc<crate::postprocess::stats::Stats>,
    lanes:Arc<LaneCounters>,config:crate::config::PostprocessConfig,masks:Masks,
}
#[derive(Debug)]
pub(crate) enum Refresh {Published,Deferred(anyhow::Error)}
impl Producer {
    pub(crate) fn new(runner:&Arc<Runner>,lanes:Arc<LaneCounters>)->Result<Self> {
        let owner=runner.owner.clone();let destination=owner.cache_destination().ok_or_else(||anyhow::anyhow!("native Stage stats destination unavailable"))?.clone();
        let epoch=owner.epoch().ok_or_else(||anyhow::anyhow!("native Stage stats epoch unavailable"))?.clone();
        let mut slot=CURRENT.lock().map_err(|_|anyhow::anyhow!("native Stage stats publication poisoned"))?;
        ensure!(slot.as_ref().and_then(|slot|slot.owner.upgrade()).is_none_or(|old|old.stop.load(Ordering::Acquire)),"native Stage stats producer is already active");
        *slot=Some(Slot {owner:Arc::downgrade(&owner),epoch:*epoch.id(),root:crate::paths::app_dir(),reading:None});
        Ok(Self {runner:runner.clone(),owner,destination,epoch,stats:runner.stats.clone(),lanes,config:runner.config.postprocess.clone(),
            masks:Masks {enabled:runner.steps.enabled,network:runner.steps.network,net_ready_at:i64::MIN}})
    }
    fn withdraw(&self) {
        let mut slot=CURRENT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(slot)=slot.as_mut().filter(|slot|slot.epoch==*self.epoch.id()&&Weak::ptr_eq(&slot.owner,&Arc::downgrade(&self.owner))) {slot.reading=None;}
    }
    pub(crate) fn refresh(&self)->Result<Refresh> {
        let observed=self.owner.with(|client| {
            let storage=crate::config::checked_storage_config().map_err(|_|anyhow::anyhow!("native Stage stats configuration unavailable"))?;
            let masks=Masks {net_ready_at:self.runner.net_paused_until.load(Ordering::Acquire),..self.masks};
            stage_stats::observe(client,&self.destination,&storage,&self.epoch,masks,&self.owner.stop,Instant::now()+Duration::from_secs(2))
        });
        let counts=match observed {
            Ok(Observation::Complete(counts))=>counts,
            Ok(Observation::Deferred(cause))=>{self.withdraw();return Ok(Refresh::Deferred(cause))},
            Err(error)=>{self.withdraw();return Err(error)},
        };
        if self.owner.stop.load(Ordering::Acquire) {self.withdraw();return Err(crate::pg::outage::AttemptEnded.into())}
        self.stats.queue_due.store(counts.due,Ordering::Relaxed);self.stats.net_wanted.store(counts.net,Ordering::Relaxed);
        self.stats.failed.store(counts.failed,Ordering::Relaxed);self.stats.walk_done.store(counts.walk_done,Ordering::Relaxed);
        self.stats.walk_total.store(counts.walk_total,Ordering::Relaxed);self.stats.names_over_quota.store(counts.names_over_quota,Ordering::Relaxed);
        let rows=self.lanes.walk_rows.load(Ordering::Relaxed);self.stats.rows_walked.store(rows,Ordering::Relaxed);
        self.stats.lanes.net_visits[0].store(self.lanes.net_visits.load(Ordering::Relaxed),Ordering::Relaxed);
        self.stats.lanes.net_finished[0].store(self.lanes.net_finished.load(Ordering::Relaxed),Ordering::Relaxed);
        self.stats.lanes.queue_taken[0].store(self.lanes.queue_taken.load(Ordering::Relaxed),Ordering::Relaxed);
        self.stats.lanes.walk_rows[0].store(rows,Ordering::Relaxed);
        let mut value=self.stats.snapshot(self.config.body_fetches_per_minute,self.config.backlog_per_second,self.masks.enabled);
        let most=if self.masks.enabled&self.masks.network==0 {0}else{self.config.body_connections.max(1)};
        value["net_claims"]=json!({"running":[counts.net_claims],"most":most});
        if serde_json::to_vec(&value)?.len()>MAX_PAYLOAD {self.withdraw();return Ok(Refresh::Deferred(stage_stats::LocalAdmission.into()))}
        let backlog=counts.due.checked_add(counts.net).ok_or_else(||anyhow::anyhow!("native Stage backlog overflow"))?;
        let mut slot=CURRENT.lock().map_err(|_|anyhow::anyhow!("native Stage stats publication poisoned"))?;
        let slot=slot.as_mut().filter(|slot|slot.epoch==*self.epoch.id()&&Weak::ptr_eq(&slot.owner,&Arc::downgrade(&self.owner)))
            .ok_or_else(||anyhow::anyhow!("native Stage stats publication attempt changed"))?;
        if self.owner.stop.load(Ordering::Acquire)||self.owner.failure().is_some() {slot.reading=None;return Err(crate::pg::outage::AttemptEnded.into())}
        crate::postprocess::stats::set_backlog(backlog);
        slot.reading=Some(Reading {at:Instant::now(),value,backlog});Ok(Refresh::Published)
    }
}
impl Drop for Producer {
    fn drop(&mut self) {
        let mut slot=CURRENT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.as_ref().is_some_and(|slot|slot.epoch==*self.epoch.id()&&Weak::ptr_eq(&slot.owner,&Arc::downgrade(&self.owner))) {*slot=None;}
    }
}
fn current<T>(read:impl FnOnce(&Reading)->T)->Option<T> {
    let slot=CURRENT.lock().ok()?;let slot=slot.as_ref()?;let owner=slot.owner.upgrade()?;
    if owner.stop.load(Ordering::Acquire)||owner.failure().is_some()||slot.root!=crate::paths::app_dir() {return None}
    let reading=slot.reading.as_ref().filter(|reading|reading.at.elapsed()<FRESH)?;Some(read(reading))
}
pub(crate) fn snapshot()->Option<Value> {current(|reading|reading.value.clone())}
pub(crate) fn backlog()->Option<i64> {current(|reading|reading.backlog)}

/// The existing BG writer retains all root/permission/atomic-replace policy.
/// Bound encoding before a growing map/string can allocate an unbounded copy.
pub(crate) fn stats_json(value:&Value)->std::io::Result<Vec<u8>> {
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        fn write(&mut self,bytes:&[u8])->std::io::Result<usize> {
            if self.0.len().checked_add(bytes.len()).is_none_or(|total|total>16*1024*1024) {
                return Err(std::io::Error::other("native stats JSON exceeds encoding admission"))
            }
            self.0.extend_from_slice(bytes);Ok(bytes.len())
        }
        fn flush(&mut self)->std::io::Result<()> {Ok(())}
    }
    let mut out=Bounded(Vec::new());serde_json::to_writer(&mut out,value).map_err(|_|std::io::Error::other("native stats JSON encoding failed"))?;Ok(out.0)
}

#[cfg(test)]
#[path="native_telemetry_tests.rs"]
mod tests;
