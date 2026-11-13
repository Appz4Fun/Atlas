//! Bounded fresh-catalog seed and walk nomination on the original writer.
use anyhow::{Context,Result,ensure};
use super::{Claim,Queue};

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(super) struct Target {pub(super) id:i64,pub(super) before:i64,pub(super) redo:i64}
pub(super) fn target(row:&postgres::Row,start:usize)->Result<Option<Target>> {
    match (row.try_get(start)?,row.try_get(start+1)?,row.try_get(start+2)?) {
        (None,None,None)=>Ok(None),
        (Some(id),Some(before),Some(redo))=>Ok(Some(Target {id,before,redo})),
        _=>Err(anyhow::anyhow!("native walk association is incomplete")),
    }
}
#[derive(Debug,PartialEq,Eq)]
pub(crate) enum WalkProgress {Idle,Queued,Blocked,Skipped,Finished}

fn walk_in(tx:&mut postgres::Transaction<'_>,kind:&str,redo:i64,group:Option<&str>,now:i64)->Result<i64> {
    Ok(tx.query_one("insert into pp_walks(kind,top_id,before_id,redo_mask,group_name,created_at)
        select $1,t,t,$2,$3,$4 from (select coalesce(max(id),-1)+1 t from releases) maximum returning id",
        &[&kind,&redo,&group,&now])?.try_get(0)?)
}
fn set_meta(tx:&mut postgres::Transaction<'_>,key:&str,value:i64)->Result<()> {
    tx.execute("insert into meta(key,value) values($1,$2) on conflict(key) do update set value=excluded.value",&[&key,&value])?;Ok(())
}
fn meta(tx:&mut postgres::Transaction<'_>,key:&str)->Result<Option<i64>> {
    Ok(tx.query_opt("select value from meta where key=$1",&[&key])?.map(|row|row.try_get(0)).transpose()?.flatten())
}
impl Queue {
    /// The same enabled/config-stamp walk policy as the legacy engine, without
    /// its one-off repairs for discarded SQLite release facts. Kept raw facts
    /// still pass each typed cache helper's current eligibility checks.
    pub(crate) fn seed(&self,enabled:i64,stamps:&[(u8,i64,i64)],now:i64)->Result<()> {
        self.lane()?;ensure!(stamps.len()<=62 && enabled>=0,"native seed admission exceeded");
        let mut seen_ids=std::collections::HashSet::new();let mut registered=0;
        for (id,bit,_) in stamps {
            ensure!(*id<63 && *bit==1i64.checked_shl(u32::from(*id)).context("native seed bit overflow")? && seen_ids.insert(*id),"native seed step identity differs");
            registered|=*bit;
        }
        ensure!(enabled & !registered==0,"native seed enabled mask has no registered step");
        self.owner.with(|client| {
            let mut tx=client.transaction()?;crate::pg::history_gate::native(&mut tx)?;
            let seeded=meta(&mut tx,"pp_walk_seeded")?.is_some();
            if !seeded {walk_in(&mut tx,"backlog",0,None,now)?;set_meta(&mut tx,"pp_walk_seeded",now)?;}
            let seen=meta(&mut tx,"pp_steps_seen")?.unwrap_or(0);
            let new=if seeded {enabled & !seen}else{enabled};
            if seeded && new!=0 {walk_in(&mut tx,"steps",0,None,now)?;}
            set_meta(&mut tx,"pp_steps_seen",enabled)?;
            for (id,bit,stamp) in stamps {
                let key=format!("pp_stamp_{id}");let stored=meta(&mut tx,&key)?;
                if seeded && new & bit==0 && stored.is_some_and(|old|stamp & !old!=0) {walk_in(&mut tx,"steps",*bit,None,now)?;}
                if stored!=Some(*stamp) {set_meta(&mut tx,&key,*stamp)?;}
            }
            tx.commit()?;Ok(())
        })
    }
    pub(crate) fn request_walk(&self,redo:i64,group:Option<&str>,now:i64)->Result<i64> {
        self.lane()?;ensure!(redo>=0 && group.is_none_or(|group|!group.is_empty() && group.len()<=512),"native walk request exceeds admission");
        self.owner.with(|client| {let mut tx=client.transaction()?;crate::pg::history_gate::native(&mut tx)?;
            let id=walk_in(&mut tx,"request",redo,group,now)?;tx.commit()?;Ok(id)})
    }

    /// One nominated row per call; token admission belongs to the worker's
    /// backlog limiter. Newest pending walk wins after any owned visit drains.
    /// A cursor moves only for a future-due skip, a genuinely gone release, or
    /// the same transaction that commits a leased result and operation receipt.
    pub(crate) fn walk_tick(&self,now:i64)->Result<WalkProgress> {
        self.lane()?;
        self.owner.with(|client| {
            let mut tx=client.transaction()?;crate::pg::history_gate::native(&mut tx)?;
            let Some(row)=tx.query_opt("select id,before_id,redo_mask,group_name,native_inflight_release from pp_walks
                order by (native_inflight_release is not null) desc,id desc limit 1",&[])? else {tx.commit()?;return Ok(WalkProgress::Idle)};
            let walk=Target {id:row.try_get(0)?,before:row.try_get(1)?,redo:row.try_get(2)?};
            let group:Option<String>=row.try_get(3)?;let inflight:Option<i64>=row.try_get(4)?;
            let selected=if let Some(id)=inflight {Some(id)} else if let Some(group)=&group {
                tx.query_opt("select id from releases where id<$1 and group_name=$2 order by id desc limit 1",&[&walk.before,group])?.map(|row|row.try_get(0)).transpose()?
            } else {tx.query_opt("select id from releases where id<$1 order by id desc limit 1",&[&walk.before])?.map(|row|row.try_get(0)).transpose()?};
            let Some(id)=selected else {
                ensure!(tx.execute("delete from pp_walks where id=$1 and before_id=$2 and native_inflight_release is null",&[&walk.id,&walk.before])?==1,"native walk changed before EOF");
                tx.commit()?;return Ok(WalkProgress::Finished)
            };
            // Supported mutation order is release -> queue -> walk. Numeric
            // interval boundaries are deliberately not forced through an FK.
            let present=tx.query_opt("select id from releases where id=$1 for update",&[&id])?.is_some();
            let queue=tx.query_opt("select due_at,lease_until,native_walk_id,native_walk_before,native_walk_redo from pp_queue where release_id=$1 for update",&[&id])?;
            let current=tx.query_one("select before_id,redo_mask,native_inflight_release from pp_walks where id=$1 for update",&[&walk.id])?;
            ensure!(current.try_get::<_,i64>(0)?==walk.before && current.try_get::<_,i64>(1)?==walk.redo && current.try_get::<_,Option<i64>>(2)?==inflight,"native walk changed during nomination");
            if !present {
                ensure!(queue.is_none(),"native deleted release retained a queue row");
                advance(&mut tx,walk,id,0)?;tx.commit()?;return Ok(WalkProgress::Skipped)
            }
            let association=queue.as_ref().map(|row|target(row,2)).transpose()?.flatten();
            ensure!(association.is_none_or(|target|target==walk && inflight==Some(id)),"native queue belongs to another walk");
            if queue.as_ref().is_some_and(|row|row.get::<_,i64>(0)>now) && walk.redo==0 {
                if association.is_some() {
                    tx.execute("update pp_queue set native_walk_id=null,native_walk_before=null,native_walk_redo=null,work_serial=work_serial+1 where release_id=$1",&[&id])?;
                }
                advance(&mut tx,walk,id,0)?;tx.commit()?;return Ok(WalkProgress::Skipped)
            }
            let leased=queue.as_ref().and_then(|row|row.get::<_,Option<i64>>(1)).is_some_and(|until|until>now)
                || tx.query_one("select exists(select 1 from pp_state where release_id=$1 and native_net_until>$2)",&[&id,&now])?.try_get::<_,bool>(0)?;
            if leased {tx.commit()?;return Ok(WalkProgress::Blocked)}
            ensure!(inflight.is_none() || association==Some(walk),"native walk has lost its durable queued intent");
            if association.is_none() {
                tx.execute("insert into pp_queue(release_id,due_at,native_walk_id,native_walk_before,native_walk_redo) values($1,$2,$3,$4,$5)
                    on conflict(release_id) do update set due_at=least(pp_queue.due_at,excluded.due_at),work_serial=pp_queue.work_serial+1,
                    native_walk_id=excluded.native_walk_id,native_walk_before=excluded.native_walk_before,native_walk_redo=excluded.native_walk_redo",
                    &[&id,&now,&walk.id,&walk.before,&walk.redo])?;
                tx.execute("update pp_walks set native_inflight_release=$2 where id=$1",&[&walk.id,&id])?;
            } else if queue.as_ref().is_some_and(|row|row.get::<_,i64>(0)>now) {
                tx.execute("update pp_queue set due_at=$2,work_serial=work_serial+1 where release_id=$1",&[&id,&now])?;
            }
            tx.commit()?;Ok(WalkProgress::Queued)
        })
    }
}
pub(super) fn verify_in(tx:&mut postgres::Transaction<'_>,claim:&Claim)->Result<()> {
    if let Some(walk)=claim.walk {
        let row=tx.query_opt("select before_id,redo_mask,native_inflight_release from pp_walks where id=$1 for update",&[&walk.id])?.context("native claimed walk disappeared")?;
        ensure!(row.try_get::<_,i64>(0)?==walk.before && row.try_get::<_,i64>(1)?==walk.redo && row.try_get::<_,Option<i64>>(2)?==Some(claim.id),"native claimed walk no longer selects this input");
    }Ok(())
}
fn advance(tx:&mut postgres::Transaction<'_>,walk:Target,id:i64,ran:i64)->Result<()> {
    ensure!(id<walk.before,"native walk cursor did not move backwards");
    ensure!(tx.execute("update pp_walks set before_id=$2,seen=seen+1,ran=ran+$3,native_inflight_release=null where id=$1 and before_id=$4",
        &[&walk.id,&id,&ran,&walk.before])?==1,"native walk cursor changed before commit");Ok(())
}
pub(super) fn finish_in(tx:&mut postgres::Transaction<'_>,claim:&Claim,applied:&crate::postprocess::apply::Applied)->Result<()> {
    if let Some(walk)=claim.walk {
        let ran=i64::from(!applied.no_state || applied.queue_at.is_some() || applied.parts_seen.is_some() || !applied.writes.is_empty() || !applied.names.is_empty());
        advance(tx,walk,claim.id,ran)?;
        tx.execute("update pp_queue set native_walk_id=null,native_walk_before=null,native_walk_redo=null where release_id=$1",&[&claim.id])?;
    }Ok(())
}
