//! Actual native Stage rates from completed owner observations. Missing/stale
//! capture, process replacement and a new counter instance are explicit states.
use super::{fresh_reader::ReadView,fresh_live::RuntimeObservation};
use crate::web_pending::{NativePace,Pace};
use anyhow::{Result,Context,ensure};

#[derive(Clone)]
struct Sample {id:i64,process:[u8;32],instance:Option<[u8;32]>,at:i64,mono:i64,running:bool,enabled:Option<i64>,counts:Option<[u64;4]>}
impl ReadView<'_> {
    pub(crate) fn processing_pace(&self,owner:Option<&RuntimeObservation>,now:i64)->Result<NativePace> {
        let Some(owner)=owner else{return Ok(NativePace::Stopped {enabled:None});};
        self.before_query()?;
        let head=self.tx.borrow_mut().query_opt("select o.observation_id from native_runtime_observation_current c
            join native_runtime_observations o on o.observation_id=c.observation_id
            where c.singleton and o.epoch=$1",&[&&owner.epoch()[..]])?.context("native Stage heartbeat is unavailable")?;
        let head:i64=head.try_get(0)?;
        self.before_query()?;
        let rows=self.tx.borrow_mut().query("select observation_id,process_nonce,stage_instance_nonce,observed_at_ms,monotonic_ns,
            stage_running,enabled_mask,net_visits_bits,net_finished_bits,queue_taken_bits,walk_rows_bits
            from native_runtime_observations where epoch=$1 and observation_id<=$2 order by observation_id desc limit 32",&[&&owner.epoch()[..],&head])?;
        let mut samples=Vec::with_capacity(rows.len());
        for row in rows {
            let fixed=|at|->Result<[u8;32]>{row.try_get::<_,Vec<u8>>(at)?.try_into().map_err(|_|anyhow::anyhow!("native observation digest extent"))};
            let process=fixed(1)?;let instance=row.try_get::<_,Option<Vec<u8>>>(2)?.map(|bytes|bytes.try_into().map_err(|_|anyhow::anyhow!("native Stage instance extent"))).transpose()?;
            let columns:[Option<i64>;4]=[row.try_get(7)?,row.try_get(8)?,row.try_get(9)?,row.try_get(10)?];
            let counts=if columns.iter().all(Option::is_some){Some(columns.map(|value|value.unwrap() as u64))}else{ensure!(columns.iter().all(Option::is_none),"partial native Stage counter observation");None};
            let sample=Sample {id:row.try_get(0)?,process,instance,at:row.try_get(3)?,mono:row.try_get(4)?,running:row.try_get(5)?,enabled:row.try_get(6)?,counts};
            ensure!(sample.mono>=0 && ((sample.instance.is_none()&&!sample.running&&sample.enabled.is_none()&&sample.counts.is_none())
                ||(sample.instance.is_some()&&sample.enabled.is_some()&&sample.counts.is_some())),"native Stage observation shape is invalid");
            samples.push(sample);
        }
        let current=samples.first().context("native Stage head disappeared")?;ensure!(current.id==head,"native Stage head changed within snapshot");
        let now_ms=now.checked_mul(1000).context("native Stage clock overflow")?;
        ensure!(current.at>0 && current.at<=now_ms+5000 && now_ms.saturating_sub(current.at)<120000,"native Stage heartbeat is stale");
        let pace=pace(&samples)?;
        self.before_query()?;owner.verify_after(&mut *self.tx.borrow_mut())?;Ok(pace)
    }
}
fn pace(newest_first:&[Sample])->Result<NativePace> {
    let current=newest_first.first().context("native Stage observation missing")?;
    if !current.running{return Ok(NativePace::Stopped {enabled:current.enabled});}
    let enabled=current.enabled.context("native Stage enabled mask missing")?;
    let mut chain=Vec::new();let mut previous_id=i64::MAX;let mut previous_time=i64::MAX;
    for sample in newest_first {
        if sample.process!=current.process||sample.instance!=current.instance||!sample.running||sample.counts.is_none(){break;}
        ensure!(sample.id<previous_id&&sample.mono<previous_time,"native Stage observation order is invalid");
        previous_id=sample.id;previous_time=sample.mono;
        let elapsed=current.mono.checked_sub(sample.mono).context("native Stage clock went backwards")?;
        if elapsed>300_000_000_000 {break;}
        chain.push(sample);
    }
    let oldest=chain.last().context("native Stage observation missing")?;
    let elapsed=current.mono-oldest.mono;
    if elapsed<60_000_000_000{return Ok(NativePace::Warming {enabled});}
    let mut deltas=[0u128;4];
    for pair in chain.windows(2) {
        let newer=pair[0].counts.unwrap();let older=pair[1].counts.unwrap();
        for n in 0..4 {deltas[n]+=if newer[n]>=older[n]{newer[n]-older[n]}else{newer[n]} as u128;}
    }
    let seconds=elapsed as f64/1_000_000_000.0;
    let rates=deltas.map(|delta|((delta as f64*60.0/seconds)*10.0).round()/10.0);
    Ok(NativePace::Measured(Pace {running:true,enabled:Some(enabled),window_secs:seconds.round() as i64,
        net_visits:rates[0],net_finished:rates[1],queue_taken:rates[2],walk_rows:rates[3]}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(id:i64,seconds:i64,count:u64)->Sample {Sample {id,process:[1;32],instance:Some([2;32]),at:1,mono:seconds*1_000_000_000,
        running:true,enabled:Some(7),counts:Some([count;4])}}
    #[test]
    fn actual_counter_identity_precedes_rate_reduction_and_large_integer_conversion() {
        let first=sample(1,0,(1u64<<54)+1);let current=sample(2,120,(1u64<<54)+43);
        let NativePace::Measured(measured)=pace(&[current.clone(),first.clone()]).unwrap() else {panic!("two-minute measurement")};
        assert_eq!(measured.net_visits,21.0);assert_eq!(measured.queue_taken,21.0);
        assert!(matches!(pace(&[current.clone()]).unwrap(),NativePace::Warming {..}));
        let mut replaced=current.clone();replaced.instance=Some([3;32]);
        assert!(matches!(pace(&[replaced,first.clone()]).unwrap(),NativePace::Warming {..}));
        let mut process=current.clone();process.process=[4;32];
        assert!(matches!(pace(&[process,first]).unwrap(),NativePace::Warming {..}));
        let mut stopped=current;stopped.running=false;
        assert!(matches!(pace(&[stopped]).unwrap(),NativePace::Stopped {..}));
    }
}
