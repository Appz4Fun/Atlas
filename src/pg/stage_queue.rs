//! Attempt-bound native queue claims and atomic closed result settlement.
//! Neither a numeric release ID nor a callback can manufacture a lease.
use anyhow::{Context,Result,ensure};
use sha2::{Digest,Sha256};
use std::sync::Arc;
use crate::postprocess::apply::Applied;
use super::indexer_runtime::Owner;
use crate::postprocess::apply::Lane;
#[path="stage_net_queue.rs"]
mod net;
pub(crate) use net::{NetBegin,StopDisposition,StopAuthorityError,finalize_stop_in};
#[path="stage_walk.rs"]
mod walk;
#[path="stage_participants.rs"]
pub(crate) mod participants;
pub(crate) use walk::WalkProgress;
#[cfg(test)]
#[path="stage_walk_tests.rs"]
mod walk_tests;
#[cfg(test)]
#[path="stage_target_tests.rs"]
mod target_tests;
#[cfg(test)]
#[path="stage_result_codec_tests.rs"]
mod result_codec_tests;

pub(crate) struct Queue {owner:Arc<Owner>,epoch:[u8;32],catalog:[u8;32]}
pub(crate) struct Claim {
    epoch:[u8;32],catalog:[u8;32],id:i64,generation:i64,serial:i64,token:i64,
    rerun:i64,due:i64,until:i64,revision:i64,lane:Lane,operation:[u8;32],request:[u8;32],walk:Option<walk::Target>,
}
impl Claim {
    pub(crate) fn release_id(&self)->i64 {self.id}
    pub(crate) fn due_at(&self)->i64 {self.due}
    pub(crate) fn lane(&self)->Lane {self.lane}
    pub(crate) fn redo_mask(&self)->i64 {self.walk.map_or(0,|walk|walk.redo)}
    pub(crate) fn is_walk(&self)->bool {self.walk.is_some()}
    /// Durable identity is inspectable result data, never a replacement lease.
    pub(crate) fn operation_id(&self)->&[u8;32] {&self.operation}
}
#[derive(Debug,PartialEq,Eq)]
pub(crate) enum Settlement {Applied,Replayed,Superseded,LeaseLost,Gone}
#[derive(Debug,PartialEq,Eq)]
pub(crate) enum Reconciliation {Committed,Pending,Superseded,Gone}

/// A digest is data, not write authority. The real runner must derive input
/// from the same observed release/files/state; caller-supplied SQL is rejected.
pub(crate) struct Visit {applied:Applied,input:[u8;32],result:[u8;32],targets:Option<participants::Observed>}
impl Visit {
    pub(crate) fn prepare(claim:&Claim,applied:Applied,input:[u8;32])->Result<Self> {
        Self::prepare_inner(claim,applied,input,None)
    }
    pub(crate) fn prepare_observed(claim:&Claim,applied:Applied,input:[u8;32],targets:participants::Observed)->Result<Self> {
        ensure!(targets.belongs(claim),"native result participants belong to another owner attempt");
        Self::prepare_inner(claim,applied,input,Some(targets))
    }
    fn prepare_inner(claim:&Claim,applied:Applied,input:[u8;32],targets:Option<participants::Observed>)->Result<Self> {
        ensure!(applied.release_id==claim.id,"native visit targets another release");
        ensure!(applied.lane==claim.lane,"native visit belongs to another worker lane");
        ensure!(!applied.freeze_check,"native releases do not use SQLite freeze routing");
        ensure!(applied.junk.is_none_or(|reason|matches!(reason,crate::junk::EXECUTABLE|crate::junk::EXECUTABLE_INSIDE|crate::junk::PC)),"native junk reason is not supported");
        ensure!(applied.writes.len()<=10000 && applied.names.len()<=1024,"native result exceeds mutation admission");
        let mut digest=Sha256::new();digest.update(b"atlas-native-visit-v1");
        let mut bytes=0usize;
        for (step,write) in &applied.writes {
            mutation(write,claim.id,targets.as_ref())?;
            write.encode_native_v1(*step,&mut bytes,&mut |part|digest.update(part))?;
        }
        for (step,name) in &applied.names {
            digest.update(step.to_le_bytes());digest.update([name.source]);
            match &name.display_name {Some(text)=>{digest.update([1]);feed(&mut digest,text.as_bytes(),&mut bytes)?;},None=>digest.update([0])}
            digest.update([match name.obfuscated {None=>0,Some(false)=>1,Some(true)=>2}]);
        }
        for value in [Some(applied.release_id),Some(applied.claimed_gen),applied.claimed_due,Some(applied.set_done),Some(applied.set_failed),
            Some(applied.clear),Some(applied.rename_clears),Some(applied.net_mask),Some(applied.db_mask),Some(applied.net_steps),
            applied.db_attempts,applied.net_attempts,applied.net_transient,applied.net_next_at,applied.parts_seen,applied.queue_at] {
            match value {Some(n)=>{digest.update([1]);digest.update(n.to_le_bytes());},None=>digest.update([0])}
        }
        digest.update([u8::from(applied.no_state),if applied.lane==crate::postprocess::apply::Lane::Db {0} else {1}]);
        match &applied.error {Some(error)=>{digest.update([1]);feed(&mut digest,error.as_bytes(),&mut bytes)?;},None=>digest.update([0])}
        match applied.junk {Some(reason)=>{digest.update([1]);feed(&mut digest,reason.as_bytes(),&mut bytes)?;},None=>digest.update([0])}
        if let Some(targets)=&targets {targets.digest_into(&mut digest);}
        Ok(Self {applied,input,result:digest.finalize().into(),targets})
    }
}
fn feed(hash:&mut Sha256,value:&[u8],bytes:&mut usize)->Result<()> {
    *bytes=bytes.checked_add(value.len()).context("native result size overflow")?;
    ensure!(*bytes<=64*1024*1024,"native result exceeds byte admission");
    hash.update((value.len() as u64).to_le_bytes());hash.update(value);Ok(())
}
impl Queue {
    pub(crate) fn for_owner(owner:&Arc<Owner>)->Result<Self> {
        let epoch=owner.epoch().context("native queue requires canonical owner")?;
        Ok(Self {epoch:*epoch.id(),catalog:*epoch.catalog().nonce(),owner:owner.clone()})
    }
    fn lane(&self)->Result<()> {
        ensure!(tokio::runtime::Handle::try_current().is_err(),"native queue requires a standard worker lane");
        ensure!(self.owner.epoch().is_some_and(|e|e.id()==&self.epoch && e.catalog().nonce()==&self.catalog),"native queue owner changed");Ok(())
    }
    pub(crate) fn claim(&self,now:i64,limit:usize,lease_seconds:i64)->Result<Vec<Claim>> {
        self.claim_turn(now,limit,lease_seconds,3)
    }
    /// The same finite fairness cycle as the original database lane: three
    /// newest-due turns, then one oldest-due turn to keep backlog moving.
    pub(crate) fn claim_turn(&self,now:i64,limit:usize,lease_seconds:i64,batches:i64)->Result<Vec<Claim>> {
        self.lane()?;ensure!((1..=256).contains(&limit) && (1..=3600).contains(&lease_seconds),"native claim admission exceeded");
        let until=now.checked_add(lease_seconds).context("native lease deadline overflow")?;
        self.owner.with(|client| {
            let mut tx=client.transaction()?;super::history_gate::native(&mut tx)?;
            // Claiming has no callback or result writes. Settlement separately
            // obtains the release lock before validating the queue lock.
            let rows=tx.query(&format!("select release_id,generation,work_serial,logical_operation_id,
                coalesce((select rerun_gen from pp_state s where s.release_id=q.release_id),0),due_at,
                (select native_work_revision from releases r where r.id=q.release_id),q.native_work_revision,
                native_walk_id,native_walk_before,native_walk_redo
                from pp_queue q where due_at<=$1 and (lease_until is null or lease_until<=$1)
                and not exists(select 1 from pp_state s where s.release_id=q.release_id and s.native_net_until>$1)
                order by {},release_id limit $2 for update skip locked",crate::postprocess::engine::queue_order(batches)),&[&now,&(limit as i64)])?;
            let mut claims=Vec::with_capacity(rows.len());
            for row in rows {
                if self.owner.stop.load(std::sync::atomic::Ordering::Acquire) {return Err(super::outage::AttemptEnded.into());}
                let id:i64=row.try_get(0)?;let generation:i64=row.try_get(1)?;let serial:i64=row.try_get(2)?;
                let revision:i64=row.try_get(6)?;
                let operation=match row.try_get::<_,Option<Vec<u8>>>(3)? {
                    Some(bytes) if row.try_get::<_,Option<i64>>(7)?==Some(revision)=>bytes.try_into().map_err(|_|anyhow::anyhow!("native queued operation extent"))?,
                    _=>{let mut id=[0;32];getrandom::fill(&mut id).map_err(|_|anyhow::anyhow!("native operation identity unavailable"))?;id},
                };
                let token:i64=tx.query_one("update pp_queue set lease_token=nextval('pp_queue_lease_token_seq'),lease_until=$2,lease_epoch=$3,logical_operation_id=$4,native_work_revision=$5 where release_id=$1 returning lease_token",
                    &[&id,&until,&&self.epoch[..],&&operation[..],&revision])?.try_get(0)?;
                let rerun:i64=row.try_get(4)?;
                let walk=walk::target(&row,8)?;
                claims.push(Claim {epoch:self.epoch,catalog:self.catalog,id,generation,serial,token,rerun,due:row.try_get(5)?,until,revision,lane:Lane::Db,operation,walk,request:request(self.catalog,id,generation,serial,revision,rerun,Lane::Db,walk)});
            }
            tx.commit()?;Ok(claims)
        })
    }

    /// The new canonical owner can inspect an old operation only after actual
    /// session drain/fence acquisition by Runtime::open. This grants no old
    /// epoch capability and never executes a result callback or ACK.
    pub(crate) fn reconcile(&self,old:&Claim,visit:&Visit)->Result<Reconciliation> {
        self.lane()?;ensure!(old.catalog==self.catalog,"native operation catalog changed");
        self.owner.with(|client| {
            let mut tx=client.build_transaction().isolation_level(postgres::IsolationLevel::RepeatableRead).read_only(true).start()?;
            if let Some(row)=tx.query_opt("select catalog_nonce,request_digest,input_digest,result_digest,outcome from native_operations where operation_id=$1",&[&&old.operation[..]])? {
                check_receipt(&row,old,visit)?;tx.commit()?;return Ok(Reconciliation::Committed);
            }
            let sql=match old.lane {
                Lane::Db=>"select exists(select 1 from pp_queue q join releases r on r.id=q.release_id where q.release_id=$1 and q.logical_operation_id=$2 and q.generation=$3 and q.work_serial=$4 and r.native_work_revision=$5 and q.native_work_revision=$5 and coalesce((select rerun_gen from pp_state where release_id=r.id),0)=$6)",
                Lane::Net=>"select exists(select 1 from pp_state s join releases r on r.id=s.release_id where s.release_id=$1 and s.native_net_operation_id=$2 and s.native_net_revision=$3 and s.native_net_serial=$4 and r.native_work_revision=$5 and s.rerun_gen=$6 and s.native_net_rerun=$6)",
            };
            let present:bool=tx.query_one(sql,&[&old.id,&&old.operation[..],&old.generation,&old.serial,&old.revision,&old.rerun])?.try_get(0)?;
            if present {tx.commit()?;return Ok(Reconciliation::Pending);}
            let changed_sql=match old.lane {
                Lane::Db=>"select exists(select 1 from releases r where r.id=$1 and (r.native_work_revision<>$2 or exists(select 1 from pp_queue q where q.release_id=r.id and (q.work_serial<>$3 or q.generation<>$4))))",
                Lane::Net=>"select exists(select 1 from releases r where r.id=$1 and (r.native_work_revision<>$2 or exists(select 1 from pp_state s where s.release_id=r.id and (s.native_net_serial<>$3 or s.native_net_revision is distinct from $4 or s.rerun_gen<>$5))))",
            };
            let changed=if old.lane==Lane::Db {tx.query_one(changed_sql,&[&old.id,&old.revision,&old.serial,&old.generation])?} else {tx.query_one(changed_sql,&[&old.id,&old.revision,&old.serial,&old.generation,&old.rerun])?};
            let changed:bool=changed.try_get(0)?;
            if changed {tx.commit()?;return Ok(Reconciliation::Superseded);}
            let live:bool=tx.query_one("select exists(select 1 from releases where id=$1)",&[&old.id])?.try_get(0)?;
            ensure!(!live,"native operation has neither its pending intent nor receipt");
            tx.commit()?;Ok(Reconciliation::Gone)
        })
    }

    pub(crate) fn settle(&self,claim:&Claim,mut visit:Visit,now:i64)->Result<Settlement> {
        self.lane()?;ensure!(claim.epoch==self.epoch && claim.catalog==self.catalog,"native claim belongs to an ended attempt");
        self.owner.with(|client| {
            let mut tx=client.transaction()?;super::history_gate::native(&mut tx)?;
            // Global operation identity is independent of the epoch/lease. A
            // replay verifies every digest before reaching any result writer.
            if let Some(row)=tx.query_opt("select catalog_nonce,request_digest,input_digest,result_digest,outcome from native_operations where operation_id=$1",&[&&claim.operation[..]])? {
                check_receipt(&row,claim,&visit)?;
                tx.commit()?;return Ok(Settlement::Replayed);
            }
            let participants=visit.targets.as_ref().map(|targets|targets.lock_in(&mut tx,claim,&visit.applied.writes)).transpose()?;
            let Some(release)=tx.query_opt("select display_name,coalesce(name_source,0),is_obfuscated,native_work_revision from releases where id=$1 for update",&[&claim.id])? else {
                tx.commit()?;return Ok(Settlement::Gone);
            };
            let queue_sql=match claim.lane {
                Lane::Db=>"select generation,work_serial,lease_token,lease_until,lease_epoch,logical_operation_id,native_work_revision,null::bigint,native_walk_id,native_walk_before,native_walk_redo from pp_queue where release_id=$1 for update",
                Lane::Net=>"select native_net_revision,native_net_serial,native_net_token,native_net_until,native_net_epoch,native_net_operation_id,native_net_revision,native_net_rerun,null::bigint,null::bigint,null::bigint from pp_state where release_id=$1 for update",
            };
            let Some(queue)=tx.query_opt(queue_sql,&[&claim.id])? else {
                tx.commit()?;return Ok(Settlement::LeaseLost);
            };
            let owned=queue.try_get::<_,Option<i64>>(2)?==Some(claim.token) && queue.try_get::<_,Option<i64>>(3)?==Some(claim.until) && claim.until>now
                && queue.try_get::<_,Option<Vec<u8>>>(4)?.as_deref()==Some(&self.epoch[..]);
            if !owned {tx.commit()?;return Ok(Settlement::LeaseLost);}
            let rerun:i64=tx.query_one("select coalesce((select rerun_gen from pp_state where release_id=$1),0)",&[&claim.id])?.try_get(0)?;
            let current=queue.try_get::<_,i64>(0)?==claim.generation && queue.try_get::<_,i64>(1)?==claim.serial && rerun==claim.rerun && release.try_get::<_,i64>(3)?==claim.revision
                && queue.try_get::<_,Option<i64>>(6)?==Some(claim.revision) && (claim.lane==Lane::Db || queue.try_get::<_,Option<i64>>(7)?==Some(claim.rerun))
                && queue.try_get::<_,Option<Vec<u8>>>(5)?.as_deref()==Some(&claim.operation[..]) && walk::target(&queue,8)?==claim.walk;
            if !current {
                if claim.walk.is_some() && walk::target(&queue,8)?!=claim.walk {
                    // A walk skipped this future-due row while its old worker
                    // drained. Preserve its actual backoff/settling deadline.
                    tx.execute("update pp_queue set lease_token=null,lease_until=null,lease_epoch=null,work_serial=work_serial+1 where release_id=$1",&[&claim.id])?;
                } else {clear_claim(&mut tx,claim,now)?;}
                tx.commit()?;return Ok(Settlement::Superseded);
            }
            if participants.as_ref().is_some_and(|locked|!locked.current()) {
                clear_claim(&mut tx,claim,now)?;tx.commit()?;return Ok(Settlement::Superseded)
            }
            walk::verify_in(&mut tx,claim)?;
            let targets=visit.targets.as_ref();
            let applied=&mut visit.applied;
            ensure!(applied.claimed_gen==claim.rerun,"native visit observed a different rerun generation");
            if let Some(reason)=applied.junk {
                // PC classification depends on absence of video. Recheck the
                // actual current files before deleting, as the legacy writer
                // does; an executable verdict is independent of that absence.
                if reason==crate::junk::PC && video_in(&mut tx,claim.id,&self.owner.stop)? {
                    clear_claim(&mut tx,claim,now)?;tx.commit()?;return Ok(Settlement::Superseded)
                }
                walk::finish_in(&mut tx,claim,applied)?;
                tx.execute("insert into release_writer_junk(group_name,is_collection,identity_key)
                    select group_name,is_collection,identity_key from releases where id=$1 on conflict do nothing",&[&claim.id])?;
                // The canonical writer uses its fixed-size identity marker,
                // including collection identity. Its replay fence must not
                // depend on fitting a possibly giant name in a text B-tree.
                // Existing FK/accounting/word-queue triggers retract only this
                // release. Valuable ext_* facts/assets have no release FK.
                ensure!(tx.execute("delete from releases where id=$1",&[&claim.id])?==1,"native junk release disappeared under its lock");
                receipt_in(&mut tx,claim,&visit,&self.epoch)?;
                if self.owner.stop.load(std::sync::atomic::Ordering::Acquire) {return Err(super::outage::AttemptEnded.into());}
                tx.commit()?;return Ok(Settlement::Applied)
            }
            // Cache observations are all checked before the first result. A
            // newer real PreDB index is expected input change, never a storage
            // failure and never permission to ACK the old no-match result.
            for (_,write) in &applied.writes {
                if let crate::postprocess::step::StepMutation::XrefRead {generation}|crate::postprocess::step::StepMutation::XrefTried {generation,..}=mutation(write,claim.id,targets)? {
                    if let Err(error)=super::stage_reads::xref::verify_read_in(&mut tx,claim.id,*generation) {
                        if error.downcast_ref::<super::stage_reads::xref::StaleWindow>().is_none() {return Err(error);}
                        clear_claim(&mut tx,claim,now)?;
                        tx.commit()?;return Ok(Settlement::Superseded);
                    }
                }
                if let crate::postprocess::step::StepMutation::PredbRead {revision}=mutation(write,claim.id,targets)? {
                    if let Err(error)=super::predb_cache::verify_read_in(&mut tx,*revision) {
                        if error.downcast_ref::<super::predb_cache::StaleRead>().is_none() {return Err(error);}
                        clear_claim(&mut tx,claim,now)?;
                        tx.commit()?;return Ok(Settlement::Superseded);
                    }
                }
            }
            for (_,write) in &applied.writes {
                if self.owner.stop.load(std::sync::atomic::Ordering::Acquire) {return Err(super::outage::AttemptEnded.into());}
                let mutation=mutation(write,claim.id,targets)?;
                if let Some(locked)=&participants {super::stage_mutations::apply_observed(&mut tx,claim.id,mutation,locked)?;}
                else {super::stage_mutations::apply(&mut tx,claim.id,mutation)?;}
            }
            let mut current:Option<String>=release.try_get(0)?;let mut source:i64=release.try_get(1)?;
            for (_,name) in &applied.names {
                if i64::from(name.source)<source || (name.display_name.is_none() && name.obfuscated.is_none()) {continue;}
                if name.display_name.as_ref().is_some_and(|name|current.as_ref()!=Some(name)) {applied.clear|=applied.rename_clears;current=name.display_name.clone();}
                source=i64::from(name.source);
                tx.execute("update releases set display_name=coalesce($2,display_name),is_obfuscated=coalesce($3,is_obfuscated),name_source=$4 where id=$1",&[&claim.id,&name.display_name,&name.obfuscated,&source])?;
            }
            if !applied.no_state {write_state(&mut tx,applied)?;}
            walk::finish_in(&mut tx,claim,applied)?;
            if claim.lane==Lane::Db && let Some(at)=applied.queue_at {
                tx.execute("update pp_queue set due_at=$2,work_serial=work_serial+1,lease_token=null,lease_until=null,lease_epoch=null where release_id=$1",&[&claim.id,&at])?;
            } else if claim.lane==Lane::Db {tx.execute("delete from pp_queue where release_id=$1",&[&claim.id])?;}
            if applied.clear & applied.db_mask!=0 || (claim.lane==Lane::Net && applied.queue_at.is_some()) {
                let at=applied.queue_at.unwrap_or(now);
                tx.execute("insert into pp_queue(release_id,due_at) values($1,$2) on conflict(release_id) do update set due_at=least(pp_queue.due_at,excluded.due_at),work_serial=pp_queue.work_serial+1",&[&claim.id,&at])?;
            }
            if !applied.no_state || !applied.writes.is_empty() || !applied.names.is_empty() {
                if let Some(locked)=&participants {locked.bump_in(&mut tx,claim.id,&applied.writes)?;}
                else {tx.execute("update releases set native_work_revision=native_work_revision+1 where id=$1 and native_work_revision=$2",&[&claim.id,&claim.revision])?;}
            }
            tx.execute("update pp_state set native_net_serial=native_net_serial+1,native_net_revision=(select native_work_revision from releases where id=$1),native_net_rerun=rerun_gen,native_net_operation_id=null,native_net_token=null,native_net_until=null,native_net_epoch=null,native_net_before=null where release_id=$1",&[&claim.id])?;
            receipt_in(&mut tx,claim,&visit,&self.epoch)?;
            if self.owner.stop.load(std::sync::atomic::Ordering::Acquire) {return Err(super::outage::AttemptEnded.into());}
            tx.commit()?;Ok(Settlement::Applied)
        })
    }
}
fn mutation<'a>(write:&'a crate::postprocess::step::StepWrite,id:i64,targets:Option<&participants::Observed>)->Result<&'a crate::postprocess::step::StepMutation> {
    match targets {Some(targets)=>write.native_observed_for(id,targets),None=>write.native_mutation_for(id)}
        .map_err(|_|anyhow::anyhow!("native result is not a closed mutation with observed participants"))
}

fn receipt_in(tx:&mut postgres::Transaction<'_>,claim:&Claim,visit:&Visit,epoch:&[u8;32])->Result<()> {
    tx.execute("insert into native_operations(operation_id,catalog_nonce,origin_epoch,request_digest,input_digest,result_digest,outcome) values($1,$2,$3,$4,$5,$6,$7)",
        &[&&claim.operation[..],&&claim.catalog[..],&&epoch[..],&&claim.request[..],&&visit.input[..],&&visit.result[..],&&[1u8][..]])?;Ok(())
}
fn video_in(tx:&mut postgres::Transaction<'_>,id:i64,stop:&std::sync::atomic::AtomicBool)->Result<bool> {
    let mut after=0i64;let mut bytes=0i64;
    loop {
        if stop.load(std::sync::atomic::Ordering::Acquire) {return Err(super::outage::AttemptEnded.into())}
        let remaining=64*1024*1024-bytes;
        let rows=tx.query("with page as materialized(select id,original_filename,octet_length(original_filename)::bigint n
            from files where release_id=$1 and id>$2 order by id limit 64), admitted as (select *,(sum(n) over())::bigint total from page)
            select id,n,coalesce(total<=$3,false),case when total<=$3 then original_filename end from admitted order by id",&[&id,&after,&remaining])?;
        if rows.is_empty() {return Ok(false)}
        for row in rows {
            after=row.try_get(0)?;let length:i64=row.try_get::<_,Option<i64>>(1)?.context("native junk file has no original name")?;
            ensure!(row.try_get::<_,bool>(2)?,"native junk file names exceed read admission");
            bytes=bytes.checked_add(length).context("native junk file name budget overflow")?;
            ensure!(bytes<=64*1024*1024,"native junk file names exceed read admission");
            let name=String::from_utf8(row.try_get::<_,Vec<u8>>(3)?)?;
            if crate::junk::is_video(&name) {return Ok(true)}
        }
    }
}

fn request(catalog:[u8;32],id:i64,generation:i64,serial:i64,revision:i64,rerun:i64,lane:Lane,walk:Option<walk::Target>)->[u8;32] {
    let mut hash=Sha256::new();hash.update(b"atlas-native-queue-request-v3");hash.update(catalog);
    hash.update([if lane==Lane::Db {0}else{1}]);for n in [id,generation,serial,revision,rerun] {hash.update(n.to_le_bytes());}
    if let Some(walk)=walk {hash.update([1]);for n in [walk.id,walk.before,walk.redo] {hash.update(n.to_le_bytes());}} else {hash.update([0]);}
    hash.finalize().into()
}
fn clear_claim(tx:&mut postgres::Transaction<'_>,claim:&Claim,now:i64)->Result<()> {
    match claim.lane {
        Lane::Db=>{tx.execute("update pp_queue set due_at=least(due_at,$2),lease_token=null,lease_until=null,lease_epoch=null,work_serial=work_serial+1 where release_id=$1",&[&claim.id,&now])?;}
        Lane::Net=>{tx.execute("update pp_state set native_net_serial=native_net_serial+1,native_net_operation_id=null,native_net_rerun=null,native_net_token=null,native_net_until=null,native_net_epoch=null,native_net_before=null,net_attempts=0,net_transient=0,net_next_at=least(net_next_at,$2) where release_id=$1",&[&claim.id,&now])?;}
    }Ok(())
}

fn check_receipt(row:&postgres::Row,claim:&Claim,visit:&Visit)->Result<()> {
    ensure!(row.try_get::<_,Vec<u8>>(0)?==claim.catalog && row.try_get::<_,Vec<u8>>(1)?==claim.request
        && row.try_get::<_,Vec<u8>>(2)?==visit.input && row.try_get::<_,Vec<u8>>(3)?==visit.result
        && row.try_get::<_,Vec<u8>>(4)?==[1],"native operation replay conflicts");Ok(())
}

fn write_state(tx:&mut postgres::Transaction<'_>,a:&Applied)->Result<()> {
    tx.execute("insert into pp_state as old(release_id,done,failed,net_wanted,db_attempts,net_attempts,net_next_at,parts_seen,last_error,rerun_gen,rerun_mask,net_transient,net_left)
        values($1::bigint,$2::bigint & ~$4::bigint,$3::bigint & ~$2::bigint & ~$4::bigint,case when ($5::bigint & ~($2::bigint & ~$4::bigint) & ~($3::bigint & ~$4::bigint))<>0 then true end,coalesce($6::bigint,0),coalesce($7::bigint,0),coalesce($8::bigint,0),coalesce($9::bigint,0),$10::text,case when $4::bigint<>0 then 1 else 0 end,$4::bigint,$11::bigint,nullif($5::bigint & ~($2::bigint & ~$4::bigint) & ~($3::bigint & ~$4::bigint),0))
        on conflict(release_id) do update set done=(old.done|$2::bigint) & ~$4::bigint,failed=(old.failed|$3::bigint) & ~$2::bigint & ~$4::bigint,
        net_wanted=case when ($5::bigint & ~((old.done|$2::bigint) & ~$4::bigint) & ~((old.failed|$3::bigint) & ~$4::bigint))<>0 then true end,
        net_left=nullif($5::bigint & ~((old.done|$2::bigint) & ~$4::bigint) & ~((old.failed|$3::bigint) & ~$4::bigint),0),db_attempts=coalesce($6::bigint,old.db_attempts),net_attempts=coalesce($7::bigint,old.net_attempts),
        net_next_at=coalesce($8::bigint,old.net_next_at),parts_seen=coalesce($9::bigint,old.parts_seen),last_error=$10::text,rerun_gen=old.rerun_gen+case when $4::bigint<>0 then 1 else 0 end,rerun_mask=old.rerun_mask|$4::bigint,net_transient=coalesce($11::bigint,old.net_transient)",
        &[&a.release_id,&a.set_done,&a.set_failed,&a.clear,&a.net_mask,&a.db_attempts,&a.net_attempts,&a.net_next_at,&a.parts_seen,&a.error,&a.net_transient])?;
    tx.execute(NET_RANK,&[&a.release_id])?;
    Ok(())
}

// The actual legacy rank expression: PostgreSQL scalar greatest replaces
// SQLite scalar max; no dynamic SQL or caller expression is evaluated.
pub(super) const NET_RANK:&str="update pp_state set net_rank=coalesce((select greatest(case
        when c.category is not null and c.category not between 2000 and 2999 and c.category not between 5000 and 5999
            and c.category not between 8000 and 8999 then 0
        when r.size >= 42949672960 then 4
        when r.size >= 16106127360 then 3
        when r.size >= 4294967296 then 2
        when r.size >= 1073741824 then 1
        when r.size >= 52428800 and (c.category between 2000 and 2999 or c.category between 5000 and 5999) then 1
        else 0 end,
        case
            when r.size <= 67108864 and exists (select 1 from files f where f.release_id = r.id and f.filename like '%.nzb')
                then 5
            when r.size <= 4194304 and exists (select 1 from files f where f.release_id = r.id)
                and not exists (select 1 from files f where f.release_id = r.id and not (
                    f.filename like '%.nfo' or f.filename like '%.txt' or f.filename like '%.sfv'
                    or f.filename like '%.srr' or f.filename like '%.srs' or f.filename like '%.md5'
                    or f.filename like '%.sha1' or f.filename like '%.diz'
                    or f.filename like '%.jpg' or f.filename like '%.jpeg' or f.filename like '%.png'
                    or f.filename like '%.gif' or f.filename like '%.webp'))
                and ((select count(*) from files f where f.release_id = r.id) = 1
                    or exists (select 1 from files f where f.release_id = r.id and not (
                        f.filename like '%.jpg' or f.filename like '%.jpeg' or f.filename like '%.png'
                        or f.filename like '%.gif' or f.filename like '%.webp')))
                then 5
            when exists (select 1 from files f where f.release_id = r.id and f.filename like '%.nzb')
                or exists (select 1 from nzb_pointed p where p.release_id = r.id) then 3
            else 0 end)
    from releases r left join release_category c on c.release_id = r.id where r.id = $1), 0) where release_id=$1";

/// Native late matching and reprocess share the same release-first order. The
/// caller is already inside the original owner's validated RC transaction.
/// In-flight leases remain fenced until their stale visit drains or expires;
/// this never admits a second BODY run just because a rerun was requested.
pub(crate) fn rerun_in(tx:&mut postgres::Transaction<'_>,release_id:i64,clear:i64,now:i64)->Result<bool> {
    super::history_gate::native(tx)?;
    if tx.query_opt("select id from releases where id=$1 for update",&[&release_id])?.is_none() {return Ok(false);}
    tx.query_opt("select release_id from pp_queue where release_id=$1 for update",&[&release_id])?;
    tx.execute("insert into pp_state as old(release_id,rerun_gen,rerun_mask) values($1,1,$2)
        on conflict(release_id) do update set done=old.done & ~$2::bigint,failed=old.failed & ~$2::bigint,
        db_attempts=0,net_attempts=0,net_transient=0,net_next_at=0,rerun_gen=old.rerun_gen+1,rerun_mask=old.rerun_mask|$2::bigint",&[&release_id,&clear])?;
    tx.execute("insert into pp_queue(release_id,due_at) values($1,$2) on conflict(release_id) do update set due_at=excluded.due_at,generation=pp_queue.generation+1,work_serial=pp_queue.work_serial+1",&[&release_id,&now])?;
    Ok(true)
}
