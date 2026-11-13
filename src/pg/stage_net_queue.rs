//! The NET lane leases existing pp_state on the same original owner session.
use anyhow::{Result,ensure,Context};
use super::{Queue,Claim,Lane,request,clear_claim};

#[derive(Debug,PartialEq,Eq)]
pub(crate) enum NetBegin {Started {attempts:i64},Superseded,LeaseLost,Gone}
#[derive(Debug,PartialEq,Eq)]
pub(crate) enum StopDisposition {Restored,NotStarted,Superseded}
#[derive(Debug)]
pub(crate) struct StopAuthorityError;
impl std::fmt::Display for StopAuthorityError {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("native stop finalization has no original joined-owner authority")}}
impl std::error::Error for StopAuthorityError {}

impl Queue {
    pub(crate) fn claim_net(&self,now:i64,limit:usize,lease_seconds:i64,turn:i64)->Result<Vec<Claim>> {
        self.lane()?;ensure!((1..=256).contains(&limit) && (1..=3600).contains(&lease_seconds),"native NET claim admission exceeded");
        let until=now.checked_add(lease_seconds).context("native NET lease deadline overflow")?;
        let order=crate::postprocess::engine::claim_order(turn);
        self.owner.with(|client| {
            let mut tx=client.transaction()?;crate::pg::history_gate::native(&mut tx)?;
            // All supported claim writers use this one owner's mutex. The
            // other lane's live lease is checked in the same claim transaction.
            let rows=tx.query(&format!("select release_id,native_net_serial,native_net_operation_id,native_net_revision,rerun_gen,net_next_at,
                (select native_work_revision from releases r where r.id=s.release_id),native_net_rerun
                from pp_state s where net_wanted=true and net_next_at<=$1 and (native_net_until is null or native_net_until<=$1)
                and not exists(select 1 from pp_queue q where q.release_id=s.release_id and q.lease_until>$1)
                order by {order} limit $2 for update skip locked"),&[&now,&(limit as i64)])?;
            let mut claims=Vec::new();
            for row in rows {
                if self.owner.stop.load(std::sync::atomic::Ordering::Acquire) {return Err(crate::pg::outage::AttemptEnded.into());}
                let id:i64=row.try_get(0)?;let mut serial:i64=row.try_get(1)?;let revision:i64=row.try_get(6)?;
                let old_revision:Option<i64>=row.try_get(3)?;let old_operation:Option<Vec<u8>>=row.try_get(2)?;
                let rerun:i64=row.try_get(4)?;
                let operation=if old_revision==Some(revision) && row.try_get::<_,Option<i64>>(7)?==Some(rerun) && let Some(bytes)=old_operation {
                    bytes.try_into().map_err(|_|anyhow::anyhow!("native NET operation extent"))?
                } else {
                    serial=serial.checked_add(1).context("native NET work serial overflow")?;
                    let mut operation=[0;32];getrandom::fill(&mut operation).map_err(|_|anyhow::anyhow!("native NET operation identity unavailable"))?;operation
                };
                let changed=old_revision.is_some_and(|old|old!=revision) || row.try_get::<_,Option<i64>>(7)?.is_some_and(|old|old!=rerun);
                let token:i64=tx.query_one("update pp_state set native_net_serial=$2,native_net_revision=$3,native_net_operation_id=$4,native_net_token=nextval('pp_queue_lease_token_seq'),native_net_until=$5,native_net_epoch=$6,native_net_rerun=$7,native_net_before=null,
                    net_attempts=case when $8 then 0 else net_attempts end,net_transient=case when $8 then 0 else net_transient end
                    where release_id=$1 returning native_net_token",&[&id,&serial,&revision,&&operation[..],&until,&&self.epoch[..],&rerun,&changed])?.try_get(0)?;
                claims.push(Claim {epoch:self.epoch,catalog:self.catalog,id,generation:revision,serial,token,rerun,due:row.try_get(5)?,until,revision,lane:Lane::Net,operation,walk:None,request:request(self.catalog,id,revision,serial,revision,rerun,Lane::Net,None)});
            }
            tx.commit()?;Ok(claims)
        })
    }

    /// Commit interruption accounting before a body/provider call begins.
    pub(crate) fn begin_net(&self,claim:&Claim,now:i64,crash_retry:i64)->Result<NetBegin> {
        self.lane()?;ensure!(claim.lane==Lane::Net && claim.epoch==self.epoch && claim.catalog==self.catalog,"native NET begin belongs to another lease");
        ensure!((1..=3600).contains(&crash_retry),"native NET crash retry exceeds admission");
        let next=now.checked_add(crash_retry).context("native NET crash retry overflow")?;
        self.owner.with(|client| {
            let mut tx=client.transaction()?;crate::pg::history_gate::native(&mut tx)?;
            let Some(release)=tx.query_opt("select native_work_revision from releases where id=$1 for update",&[&claim.id])? else {tx.commit()?;return Ok(NetBegin::Gone)};
            let Some(row)=tx.query_opt("select native_net_token,native_net_until,native_net_epoch,native_net_operation_id,native_net_serial,native_net_revision,rerun_gen,net_attempts,native_net_rerun from pp_state where release_id=$1 for update",&[&claim.id])? else {tx.commit()?;return Ok(NetBegin::LeaseLost)};
            if row.try_get::<_,Option<i64>>(0)?!=Some(claim.token) || row.try_get::<_,Option<i64>>(1)?!=Some(claim.until) || claim.until<=now || row.try_get::<_,Option<Vec<u8>>>(2)?.as_deref()!=Some(&self.epoch[..]) {
                tx.commit()?;return Ok(NetBegin::LeaseLost)
            }
            let current=row.try_get::<_,Option<Vec<u8>>>(3)?.as_deref()==Some(&claim.operation[..]) && row.try_get::<_,i64>(4)?==claim.serial
                && row.try_get::<_,Option<i64>>(5)?==Some(claim.revision) && row.try_get::<_,i64>(6)?==claim.rerun && row.try_get::<_,Option<i64>>(8)?==Some(claim.rerun) && release.try_get::<_,i64>(0)?==claim.revision;
            if !current {clear_claim(&mut tx,claim,now)?;tx.commit()?;return Ok(NetBegin::Superseded)}
            let attempts:i64=row.try_get(7)?;
            ensure!(attempts>=0,"native NET attempt count is invalid");
            tx.execute("update pp_state set native_net_before=coalesce(native_net_before,net_attempts),net_attempts=net_attempts+1,net_next_at=$2 where release_id=$1",&[&claim.id,&next])?;
            if self.owner.stop.load(std::sync::atomic::Ordering::Acquire) {return Err(crate::pg::outage::AttemptEnded.into())}
            tx.commit()?;Ok(NetBegin::Started {attempts})
        })
    }
}

/// Only the original Owner's closed finalizer calls this after real joins.
/// Expected input changes are explicit refusals, never successful restoration.
pub(crate) fn finalize_stop_in(tx:&mut postgres::Transaction<'_>,epoch:&crate::pg::stage::OwnerEpoch,claim:&Claim,now:i64)->Result<StopDisposition> {
    if claim.epoch!=*epoch.id() || claim.catalog!=*epoch.catalog().nonce() {return Err(StopAuthorityError.into())}
    if claim.lane!=Lane::Net {return Ok(StopDisposition::NotStarted)}
    let Some(release)=tx.query_opt("select native_work_revision from releases where id=$1 for update",&[&claim.id])? else {return Ok(StopDisposition::Superseded)};
    let Some(row)=tx.query_opt("select native_net_token,native_net_epoch,native_net_serial,native_net_operation_id,native_net_revision,native_net_rerun,rerun_gen,native_net_before,net_attempts,native_net_until from pp_state where release_id=$1 for update",&[&claim.id])? else {return Ok(StopDisposition::Superseded)};
    let current=row.try_get::<_,Option<i64>>(0)?==Some(claim.token) && row.try_get::<_,Option<Vec<u8>>>(1)?.as_deref()==Some(&claim.epoch[..])
        && row.try_get::<_,i64>(2)?==claim.serial && row.try_get::<_,Option<Vec<u8>>>(3)?.as_deref()==Some(&claim.operation[..])
        && row.try_get::<_,Option<i64>>(4)?==Some(claim.revision) && row.try_get::<_,Option<i64>>(5)?==Some(claim.rerun)
        && row.try_get::<_,i64>(6)?==claim.rerun && release.try_get::<_,i64>(0)?==claim.revision && row.try_get::<_,Option<i64>>(9)?==Some(claim.until);
    if !current {return Ok(StopDisposition::Superseded)}
    ensure!(!tx.query_one("select exists(select 1 from native_operations where operation_id=$1)",&[&&claim.operation[..]])?.try_get::<_,bool>(0)?,"a pending native NET lease already has a committed receipt");
    let before:Option<i64>=row.try_get(7)?;
    if let Some(before)=before {ensure!(before>=0 && row.try_get::<_,i64>(8)? > before,"native NET before accounting is inconsistent");}
    let restored=tx.execute("update pp_state set net_attempts=coalesce(native_net_before,net_attempts),net_next_at=case when native_net_before is not null then $7 else net_next_at end,native_net_before=null,native_net_token=null,native_net_until=null,native_net_epoch=null
        where release_id=$1 and native_net_token=$2 and native_net_serial=$3 and native_net_operation_id=$4 and native_net_epoch=$5 and native_net_revision=$6 and native_net_rerun=$8 and rerun_gen=$8
        and native_net_until=$9 and native_net_before is not distinct from $10 and not exists(select 1 from native_operations where operation_id=$4)",&[&claim.id,&claim.token,&claim.serial,&&claim.operation[..],&&claim.epoch[..],&claim.revision,&now,&claim.rerun,&claim.until,&before])?;
    ensure!(restored==1,"native NET stop CAS changed after its locks");
    Ok(if before.is_some() {StopDisposition::Restored}else{StopDisposition::NotStarted})
}
