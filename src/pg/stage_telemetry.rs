//! Completed in-process observations written by the original native owner.

use anyhow::{Context,Result,ensure};
use sha2::{Digest,Sha256};
use std::sync::{Arc,OnceLock,atomic::{AtomicBool,AtomicU64,Ordering}};
use std::time::Instant;
use super::indexer_runtime::Owner;

#[path="stage_catalog_observation.rs"]
mod catalog;

const MAX_ROWS:usize=10000;
const MAX_BYTES:usize=8*1024*1024;

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
        ensure!(bytes<=MAX_BYTES && self.rows.len()<MAX_ROWS,"native metric inventory exceeds observation admission");
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
pub(crate) fn capture(owner:&Arc<Owner>,lanes:Option<&LaneCounters>)->Result<()> {
    ensure!(tokio::runtime::Handle::try_current().is_err(),"native observations require a standard owner lane");
    let process=process()?;let mut inventory=Inventory::new();
    crate::metrics::visit(|family,labels,value|if catalog::owns(family) {Ok(())} else {inventory.push(family,labels,value)})?;
    let lane=lanes.map(LaneCounters::snapshot);
    let monotonic=i64::try_from(process.started.elapsed().as_nanos()).context("native observation clock overflow")?;
    let observed=chrono::Utc::now().timestamp_millis();
    let epoch=owner.epoch().context("native observation requires canonical owner")?;
    owner.with(|client| {
        let mut tx=client.transaction()?;super::history_gate::native(&mut tx)?;
        epoch.verify(&mut tx)?;
        tx.batch_execute("set local statement_timeout='10s';set local lock_timeout='5s'")?;
        catalog::append(&mut tx,&mut inventory,&owner.stop)?;
        let row=tx.query_one("insert into native_runtime_observations(epoch,catalog_nonce,process_nonce,stage_instance_nonce,observed_at_ms,monotonic_ns,stage_running,enabled_mask,net_visits_bits,net_finished_bits,queue_taken_bits,walk_rows_bits)
            values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) returning observation_id",
            &[&&epoch.id()[..],&&epoch.catalog().nonce()[..],&&process.nonce[..],&lane.as_ref().map(|s|&s.instance[..]),&observed,&monotonic,
                &lane.as_ref().is_some_and(|s|s.running),&lane.as_ref().map(|s|s.enabled),&lane.as_ref().map(|s|s.net_visits),
                &lane.as_ref().map(|s|s.net_finished),&lane.as_ref().map(|s|s.queue_taken),&lane.as_ref().map(|s|s.walk_rows)])?;
        let id:i64=row.try_get(0)?;
        let statement=tx.prepare("insert into native_metric_samples(observation_id,series_key,family,labels,kind,counter_bits,gauge_bits,histogram_count_bits,histogram_sum_bits) values($1,$2,$3,$4,$5,$6,$7,$8,$9)")?;
        for sample in &inventory.rows {
            if owner.stop.load(Ordering::Acquire) {return Err(super::outage::AttemptEnded.into());}
            tx.execute(&statement,&[&id,&&sample.key[..],&sample.family,&sample.labels,&sample.kind,&sample.counter,&sample.gauge,&sample.count,&sample.sum])?;
        }
        tx.execute("insert into native_runtime_observation_current(singleton,observation_id) values(true,$1) on conflict(singleton) do update set observation_id=excluded.observation_id",&[&id])?;
        tx.execute("delete from native_runtime_observations where observation_id not in(select observation_id from native_runtime_observations order by observation_id desc limit 32)",&[])?;
        epoch.verify(&mut tx)?;tx.commit()?;Ok(())
    })?;
    // Publish registry gauges only after the corresponding complete PG frame
    // committed. Failed capture leaves both prior readings intact.
    catalog::publish(&inventory.rows);Ok(())
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
