//! Completed in-process observations written by the original native owner.

use anyhow::{Context,Result,ensure};
use sha2::{Digest,Sha256};
use std::sync::{Arc,OnceLock,atomic::{AtomicBool,AtomicU64,Ordering}};
use std::time::Instant;
use super::indexer_runtime::Owner;

#[path="stage_catalog_observation.rs"]
mod catalog;

pub(crate) const MAX_ROWS:usize=65536;
pub(crate) const MAX_BYTES:usize=16*1024*1024;
const BATCH_ROWS:usize=128;
const BATCH_BYTES:usize=64*1024;

#[derive(Debug)]
pub(crate) struct LocalCapacity;
impl std::fmt::Display for LocalCapacity {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("native metric frame exceeds local capacity")}}
impl std::error::Error for LocalCapacity {}
#[derive(Debug)]
pub(crate) enum Capture {Complete,Deferred(LocalCapacity)}
#[derive(Debug)]
struct CaptureRollback {work:anyhow::Error,rollback:postgres::Error}
impl std::fmt::Display for CaptureRollback {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("native metric capture and rollback failed")}}
impl std::error::Error for CaptureRollback {
    fn source(&self)->Option<&(dyn std::error::Error+'static)> {
        if !self.work.is::<LocalCapacity>()&&super::outage::classify(&self.work)==super::outage::Failure::Fatal {Some(self.work.as_ref())}else{Some(&self.rollback)}
    }
}
fn capacity(allowed:bool)->Result<()> {if allowed {Ok(())}else{Err(LocalCapacity.into())}}

/// One actual native Stage counter instance; never an invented shard lane.
/// The runner increments these at the corresponding completed transitions.
pub(crate) struct LaneCounters {
    instance:[u8;32],running:AtomicBool,enabled:AtomicU64,
    pub(crate) net_visits:AtomicU64,pub(crate) net_finished:AtomicU64,
    pub(crate) queue_taken:AtomicU64,pub(crate) walk_rows:AtomicU64,
}
impl LaneCounters {
    pub(crate) fn new(enabled:u64)->Result<Self> {
        let mut instance=[0;32];getrandom::fill(&mut instance).map_err(|_|anyhow::anyhow!("native counter identity unavailable"))?;
        Ok(Self {instance,running:AtomicBool::new(true),enabled:AtomicU64::new(enabled),
            net_visits:AtomicU64::new(0),net_finished:AtomicU64::new(0),queue_taken:AtomicU64::new(0),walk_rows:AtomicU64::new(0)})
    }
    pub(crate) fn set_enabled(&self,enabled:u64) {self.enabled.store(enabled,Ordering::Release);}
    pub(crate) fn stop(&self) {self.running.store(false,Ordering::Release);}
    fn snapshot(&self)->LaneSample {
        LaneSample {instance:self.instance,running:self.running.load(Ordering::Acquire),enabled:self.enabled.load(Ordering::Acquire) as i64,
            net_visits:self.net_visits.load(Ordering::Relaxed) as i64,net_finished:self.net_finished.load(Ordering::Relaxed) as i64,
            queue_taken:self.queue_taken.load(Ordering::Relaxed) as i64,walk_rows:self.walk_rows.load(Ordering::Relaxed) as i64}
    }
}
struct LaneSample {instance:[u8;32],running:bool,enabled:i64,net_visits:i64,net_finished:i64,queue_taken:i64,walk_rows:i64}
struct Metric {key:[u8;32],family:&'static str,labels:String,kind:i16,counter:Option<i64>,gauge:Option<i64>,count:Option<i64>,sum:Option<i64>}
struct Inventory {rows:Vec<Metric>,bytes:usize}
impl Inventory {
    fn new()->Self {Self {rows:Vec::new(),bytes:0}}
    fn push(&mut self,family:&'static str,labels:&str,value:crate::metrics::ObservedValue)->Result<()> {
        ensure!(family.len()<=256 && labels.len()<=16384,"native metric identity exceeds observation admission");
        let bytes=self.bytes.checked_add(family.len()+labels.len()+128).context("native observation byte overflow")?;
        capacity(bytes<=MAX_BYTES && self.rows.len()<MAX_ROWS)?;
        let (kind,counter,gauge,count,sum)=match value {
            crate::metrics::ObservedValue::Counter(n)=>(1,Some(n as i64),None,None,None),
            crate::metrics::ObservedValue::GaugeBits(n)=>(2,None,Some(n as i64),None,None),
            crate::metrics::ObservedValue::Histogram {count,sum_bits}=>(3,None,None,Some(count as i64),Some(sum_bits as i64)),
        };
        let mut hash=Sha256::new();hash.update(b"atlas-native-original-series-v1");
        hash.update((family.len() as u64).to_le_bytes());hash.update(family.as_bytes());
        hash.update((labels.len() as u64).to_le_bytes());hash.update(labels.as_bytes());hash.update([kind as u8]);
        self.rows.push(Metric {key:hash.finalize().into(),family,labels:labels.into(),kind,counter,gauge,count,sum});self.bytes=bytes;
        Ok(())
    }
}
struct Process {nonce:[u8;32],started:Instant}
static PROCESS:OnceLock<Process>=OnceLock::new();
fn process()->Result<&'static Process> {
    if let Some(process)=PROCESS.get() {return Ok(process);}
    let mut nonce=[0;32];getrandom::fill(&mut nonce).map_err(|_|anyhow::anyhow!("native observation process identity unavailable"))?;
    let _=PROCESS.set(Process {nonce,started:Instant::now()});
    PROCESS.get().context("native observation process identity unavailable")
}

/// Only a full actual registry visit can reach the transaction. Overflow does
/// not move the published head. No provider, HTTP/file reader, sampler thread,
/// runtime activation, or caller-supplied metric totals are involved.
pub(crate) fn capture(owner:&Arc<Owner>,lanes:Option<&LaneCounters>)->Result<Capture> {
    ensure!(tokio::runtime::Handle::try_current().is_err(),"native observations require a standard owner lane");
    if owner.stop.load(Ordering::Acquire) {return Err(super::outage::AttemptEnded.into());}
    let process=process()?;let mut inventory=Inventory::new();
    if let Err(error)=crate::metrics::visit(|family,labels,value|if catalog::owns(family) {Ok(())} else {inventory.push(family,labels,value)}) {
        if owner.stop.load(Ordering::Acquire) {return Err(super::outage::AttemptEnded.into());}
        return if error.is::<LocalCapacity>() {Ok(Capture::Deferred(LocalCapacity))}else{Err(error)}
    }
    let lane=lanes.map(LaneCounters::snapshot);
    let monotonic=i64::try_from(process.started.elapsed().as_nanos()).context("native observation clock overflow")?;
    let observed=chrono::Utc::now().timestamp_millis();
    let epoch=owner.epoch().context("native observation requires canonical owner")?;
    let captured=owner.with(|client| {
        let mut tx=client.transaction()?;super::history_gate::native(&mut tx)?;
        epoch.verify(&mut tx)?;
        tx.batch_execute("set local statement_timeout='10s';set local lock_timeout='5s'")?;
        let result=(||->Result<()> {
        catalog::append(&mut tx,&mut inventory,&owner.stop)?;
        let row=tx.query_one("insert into native_runtime_observations(epoch,catalog_nonce,process_nonce,stage_instance_nonce,observed_at_ms,monotonic_ns,stage_running,enabled_mask,net_visits_bits,net_finished_bits,queue_taken_bits,walk_rows_bits)
            values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) returning observation_id",
            &[&&epoch.id()[..],&&epoch.catalog().nonce()[..],&&process.nonce[..],&lane.as_ref().map(|s|&s.instance[..]),&observed,&monotonic,
                &lane.as_ref().is_some_and(|s|s.running),&lane.as_ref().map(|s|s.enabled),&lane.as_ref().map(|s|s.net_visits),
                &lane.as_ref().map(|s|s.net_finished),&lane.as_ref().map(|s|s.queue_taken),&lane.as_ref().map(|s|s.walk_rows)])?;
        let id:i64=row.try_get(0)?;
        // In addition to the admitted inventory, at most 128 rows/64 KiB
        // of payload and 1152 parameter references/short placeholders live
        // here. Every batch stays in this original transaction and session.
        let mut first=0;
        while first<inventory.rows.len() {
            if owner.stop.load(Ordering::Acquire) {return Err(super::outage::AttemptEnded.into());}
            let mut end=first;let mut bytes=0;
            while end<inventory.rows.len()&&end-first<BATCH_ROWS {
                let sample=&inventory.rows[end];let size=sample.family.len()+sample.labels.len()+128;
                if bytes+size>BATCH_BYTES {break}bytes+=size;end+=1;
            }
            ensure!(end>first,"native metric identity exceeds SQL batch admission");
            let rows=(0..end-first).map(|i|format!("({})",(1..=9).map(|j|format!("${}",i*9+j)).collect::<Vec<_>>().join(","))).collect::<Vec<_>>().join(",");
            let sql=format!("insert into native_metric_samples(observation_id,series_key,family,labels,kind,counter_bits,gauge_bits,histogram_count_bits,histogram_sum_bits) values {rows}");
            let mut args:Vec<&(dyn postgres::types::ToSql+Sync)>=Vec::with_capacity((end-first)*9);
            let keys=inventory.rows[first..end].iter().map(|sample|sample.key.as_slice()).collect::<Vec<_>>();
            for (sample,key) in inventory.rows[first..end].iter().zip(&keys) {
                args.extend([&id as &(dyn postgres::types::ToSql+Sync),key,&sample.family,&sample.labels,&sample.kind,&sample.counter,&sample.gauge,&sample.count,&sample.sum]);
            }
            tx.execute(&sql,&args)?;first=end;
        }
        tx.execute("insert into native_runtime_observation_current(singleton,observation_id) values(true,$1) on conflict(singleton) do update set observation_id=excluded.observation_id",&[&id])?;
        tx.execute("delete from native_runtime_observations where observation_id not in(select observation_id from native_runtime_observations order by observation_id desc limit 32)",&[])?;
        epoch.verify(&mut tx)?;Ok(())
        })();
        match result {
            Ok(())=>{tx.commit()?;Ok(Capture::Complete)},
            Err(work)=>match tx.rollback() {
                Ok(()) if work.is::<LocalCapacity>()=>Ok(Capture::Deferred(LocalCapacity)),
                Ok(())=>Err(work),
                Err(rollback)=>Err(CaptureRollback {work,rollback}.into()),
            },
        }
    })?;
    // Publish registry gauges only after the corresponding complete PG frame
    // committed. Failed capture leaves both prior readings intact.
    if matches!(captured,Capture::Complete) {catalog::publish(&inventory.rows);}Ok(captured)
}

#[cfg(test)]
mod tests {
    #[test]
    fn lane_attempt_identity_and_unsigned_counts_are_preserved() {
        let first=super::LaneCounters::new(17).unwrap();
        let second=super::LaneCounters::new(17).unwrap();
        assert_ne!(first.instance,second.instance);
        first.net_visits.store(u64::MAX,std::sync::atomic::Ordering::Relaxed);
        let sample=first.snapshot();
        assert_eq!(sample.net_visits as u64,u64::MAX);
        assert!(sample.running);assert_eq!(sample.enabled,17);
        first.stop();assert!(!first.snapshot().running);
    }

    #[test]
    fn metric_inventory_overflow_cannot_be_a_complete_observation() {
        let mut inventory=super::Inventory::new();
        assert!(inventory.push("actual_counter", "x", crate::metrics::ObservedValue::Counter(u64::MAX)).is_ok());
        inventory.bytes=super::MAX_BYTES;
        assert!(inventory.push("next_counter", "x", crate::metrics::ObservedValue::Counter(1)).is_err());
        assert_eq!(inventory.rows.len(),1);
        assert_eq!(inventory.rows[0].counter,Some(-1));
    }
}
