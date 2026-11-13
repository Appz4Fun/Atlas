//! Fresh-catalog participants observed with their facts on the original RR.
//! No caller number, historical reference, pool or ready flag mints this value.
//! These are operational associations, never article payload/lending authority.
use anyhow::{Context,Result,ensure};
use std::{collections::{BTreeMap,BTreeSet},sync::Arc};
use sha2::Digest;
use super::{Claim,Owner};
use crate::postprocess::step::{StepMutation,StepWrite};
const LIMIT:usize=10000;

pub(crate) struct ReadTargets {owner:Arc<Owner>,observed:Observed}
#[derive(Clone)]
pub(crate) struct Observed {epoch:[u8;32],catalog:[u8;32],rows:BTreeMap<i64,i64>}
pub(in crate::pg) struct Locked {
    current:bool,source:i64,rows:BTreeMap<i64,i64>,observed:Observed,
    pointed:BTreeMap<String,Vec<i64>>,affected:BTreeSet<i64>,
}
fn target_ids(source:i64,mutation:&StepMutation)->Option<Vec<i64>> {
    use StepMutation::*;
    Some(match mutation {
        RememberUploadCandidate {candidate_id}=>vec![*candidate_id],
        PlaceBesideFile {row_id,owner_id,..}=>vec![*row_id,*owner_id],
        NameUploadLead {lead_id,..}|RerunUploadLead {lead_id,..}|QueueUploadLead {lead_id,..}=>vec![*lead_id],
        MoveUpload {from_id,to_id}=>vec![*from_id,*to_id],
        LinkUploadData {member_id,lead_id,..}=>vec![*member_id,*lead_id],
        LinkUploadRecovery {lead_id,..}=>vec![source,*lead_id],
        KeepNfoForLegacyTarget {target_id,..}=>vec![*target_id],
        _=>return None,
    })
}
fn pointed_name(mutation:&StepMutation)->Option<&str> {
    use StepMutation::*;
    match mutation {MarkPointed {name}|RankPointed {name}|RerunPointedList {name}|RerunPointedProbe {name}|SetPointedPassword {name,..}=>Some(name),_=>None}
}
fn include(tx:&mut postgres::Transaction<'_>,expected:&mut BTreeMap<i64,i64>,ids:&[i64])->Result<bool> {
    ensure!(ids.len()<=LIMIT,"native resolved participant page exceeds admission");
    let rows=tx.query("select id,native_work_revision from releases where id=any($1) order by id",&[&ids])?;
    let complete=rows.len()==ids.iter().collect::<BTreeSet<_>>().len();
    for row in rows {
        expected.entry(row.try_get(0)?).or_insert(row.try_get(1)?);
    }
    ensure!(expected.len()<=LIMIT,"native resolved participant inventory exceeds admission");Ok(complete)
}
impl ReadTargets {
    pub(crate) fn for_owner(owner:&Arc<Owner>)->Result<Self> {
        let epoch=owner.epoch().context("native read participants need the canonical owner")?;
        Ok(Self {owner:owner.clone(),observed:Observed {epoch:*epoch.id(),catalog:*epoch.catalog().nonce(),rows:BTreeMap::new()}})
    }
    pub(crate) fn observe_in(&mut self,tx:&mut postgres::Transaction<'_>,ids:&[i64])->Result<()> {
        if self.owner.stop.load(std::sync::atomic::Ordering::Acquire) {return Err(crate::pg::outage::AttemptEnded.into())}
        ensure!(ids.len()<=50000,"native participant page exceeds admission");
        if ids.is_empty() {return Ok(())}
        let epoch=self.owner.epoch().context("native participant owner ended")?;
        ensure!(epoch.id()==&self.observed.epoch && epoch.catalog().nonce()==&self.observed.catalog,"native participant owner changed");
        epoch.verify(tx)?;
        let mode=tx.query_one("select current_setting('transaction_isolation'),current_setting('transaction_read_only')",&[])?;
        let isolation:String=mode.try_get(0)?;
        ensure!(matches!(isolation.as_str(),"repeatable read"|"serializable") && mode.try_get::<_,String>(1)?=="on",
            "native participants require the same readonly repeatable snapshot as their evidence");
        let ids=ids.iter().copied().collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>();
        ensure!(ids.len()<=LIMIT,"native participant inventory exceeds admission");
        let rows=tx.query("select id,native_work_revision from releases where id=any($1) order by id",&[&ids])?;
        ensure!(rows.len()==ids.len(),"native selected participant is absent from its evidence snapshot");
        for row in rows {
            // Keep the first observation. If later reads see a changed donor,
            // monotonic revisions make settlement refuse the mixed result.
            self.observed.rows.entry(row.try_get(0)?).or_insert(row.try_get(1)?);
            ensure!(self.observed.rows.len()<=LIMIT,"native participant inventory exceeds admission");
        }Ok(())
    }
    pub(crate) fn snapshot(&self)->Observed {self.observed.clone()}
    #[cfg(test)]
    pub(crate) fn finish(self)->Observed {self.observed}
}
impl Observed {
    pub(super) fn belongs(&self,claim:&Claim)->bool {self.epoch==claim.epoch && self.catalog==claim.catalog}
    pub(crate) fn permits(&self,source:i64,mutation:&StepMutation)->bool {
        if let Some(ids)=target_ids(source,mutation) {return ids.iter().all(|id|*id==source || self.rows.contains_key(id))}
        // A closed exact-name selector resolves fresh native rows on the
        // canonical writer; it never translates a retained legacy numeric ID.
        pointed_name(mutation).is_some() || !mutation.requires_target_resolution()
    }
    pub(super) fn digest_into(&self,hash:&mut sha2::Sha256) {
        hash.update(b"atlas-native-observed-targets-v1");hash.update((self.rows.len() as u64).to_le_bytes());
        for (id,revision) in &self.rows {hash.update(id.to_le_bytes());hash.update(revision.to_le_bytes());}
    }
    pub(super) fn lock_in(&self,tx:&mut postgres::Transaction<'_>,claim:&Claim,writes:&[(i64,StepWrite)])->Result<Locked> {
        ensure!(self.belongs(claim),"native participant capability belongs to another attempt");
        let mut expected=self.rows.clone();expected.entry(claim.id).or_insert(claim.revision);
        let mut pointed=BTreeMap::new();let mut leads=BTreeMap::new();let mut affected=BTreeSet::from([claim.id]);let mut complete=true;
        let source_group=tx.query_opt("select case when octet_length(group_name)<=512 then group_name end from releases where id=$1",&[&claim.id])?
            .map(|row|row.try_get::<_,String>(0)).transpose()?;
        for (_,write) in writes {
            let mutation=write.native_observed_for(claim.id,self).map_err(|_|anyhow::anyhow!("native result participant representation changed"))?;
            if let Some(name)=pointed_name(mutation) {
                if !pointed.contains_key(name) {
                    let mut selected=Vec::new();
                    if let Some(group)=&source_group {
                        let key:[u8;32]=sha2::Sha256::digest(name.as_bytes()).into();
                        let rows=tx.query("select id,case when octet_length(group_name)<=512 then group_name end from releases
                            where original_name_key=$1 and original_name=$2 and id<>$3 order by id limit 10001",&[&&key[..],&name.as_bytes(),&claim.id])?;
                        ensure!(rows.len()<=LIMIT,"native pointed target fanout exceeds admission");
                        for row in rows {
                            let other:String=row.try_get(1)?;
                            if crate::catalog_facts::shard_of(&other)==crate::catalog_facts::shard_of(group) {selected.push(row.try_get::<_,i64>(0)?);}
                        }
                    }
                    complete &= include(tx,&mut expected,&selected)?;pointed.insert(name.to_owned(),selected);
                }
                affected.extend(pointed.get(name).into_iter().flatten());
            }
            let current_leads=match mutation {
                StepMutation::MoveUpload {from_id,to_id}=>vec![*from_id,*to_id],
                StepMutation::LinkUploadData {member_id,lead_id,..}=>vec![*member_id,*lead_id],
                StepMutation::LinkUploadRecovery {lead_id,..}=>vec![claim.id,*lead_id],
                _=>Vec::new(),
            };
            if !current_leads.is_empty() {
                let ids=current_leads;
                for id in ids {
                    let lead:i64=tx.query_one("select coalesce((select lead_id from upload_link where release_id=$1),$1)",&[&id])?.try_get(0)?;
                    leads.insert(id,lead);complete &= include(tx,&mut expected,&[lead])?;
                }
            }
            use StepMutation::*;
            match mutation {
                RememberUploadCandidate {..}=>{},
                PlaceBesideFile {row_id,owner_id,..}=>{
                    affected.extend([*row_id,*owner_id]);
                    if let Some(row)=tx.query_opt("select release_id from beside_files where row_id=$1",&[row_id])? {
                        let old:i64=row.try_get(0)?;complete &= include(tx,&mut expected,&[old])?;affected.insert(old);
                    }
                },
                MoveUpload {from_id,to_id}=>{
                    let rows=tx.query("select release_id from upload_link where lead_id=$1 order by release_id limit 10001",&[from_id])?;
                    ensure!(rows.len()<=LIMIT,"native upload move exceeds participant admission");
                    let ids=rows.into_iter().map(|row|row.try_get(0)).collect::<std::result::Result<Vec<i64>,_>>()?;
                    complete &= include(tx,&mut expected,&ids)?;affected.extend(ids);affected.extend([*from_id,*to_id]);
                },
                NameUploadLead {lead_id,..}|RerunUploadLead {lead_id,..}|QueueUploadLead {lead_id,..}=>{affected.insert(*lead_id);},
                LinkUploadData {member_id,lead_id,..}=>{affected.extend([*member_id,*lead_id]);},
                LinkUploadRecovery {lead_id,..}=>{affected.extend([claim.id,*lead_id]);},
                KeepNfoForLegacyTarget {target_id,..}=>{affected.insert(*target_id);},
                _=>{},
            }
        }
        affected.extend(leads.values().copied());
        ensure!(expected.len()<=LIMIT,"native locked participant inventory exceeds admission");
        let ids=expected.keys().copied().collect::<Vec<_>>();
        let rows=tx.query("select id,native_work_revision from releases where id=any($1) order by id for update",&[&ids])?;
        let actual=rows.into_iter().map(|row|Ok((row.try_get(0)?,row.try_get(1)?))).collect::<Result<BTreeMap<i64,i64>>>()?;
        // After all release locks, retain the same ordered queue/state lock
        // order for every participating target before any closed effect.
        tx.query("select release_id from pp_queue where release_id=any($1) order by release_id for update",&[&ids])?;
        tx.query("select release_id from pp_state where release_id=any($1) order by release_id for update",&[&ids])?;
        Ok(Locked {current:complete && actual==expected,source:claim.id,rows:actual,observed:self.clone(),pointed,affected})
    }
}
impl Locked {
    pub(super) fn current(&self)->bool {self.current}
    pub(in crate::pg) fn permits(&self,source:i64,mutation:&StepMutation)->bool {self.current && self.source==source && self.observed.permits(source,mutation)}
    pub(in crate::pg) fn pointed(&self,name:&str)->Result<&[i64]> {self.pointed.get(name).map(Vec::as_slice).context("native pointed selector was not resolved under the owner")}
    pub(in crate::pg) fn lead_in(&self,tx:&mut postgres::Transaction<'_>,id:i64)->Result<i64> {
        let lead:i64=tx.query_one("select coalesce((select lead_id from upload_link where release_id=$1),$1)",&[&id])?.try_get(0)?;
        ensure!(self.current && self.rows.contains_key(&id) && self.rows.contains_key(&lead),"native upload lead moved outside the locked participants");Ok(lead)
    }
    pub(super) fn bump_in(&self,tx:&mut postgres::Transaction<'_>,source:i64,writes:&[(i64,StepWrite)])->Result<()> {
        ensure!(self.current,"changed native participants cannot be mutated");
        let affected=self.affected.clone();
        for (_,write) in writes {
            // Representation and authority were already checked before any
            // effect; this pass enumerates only the releases actually changed.
            if let StepMutation::PlaceBesideFile {row_id,owner_id,..}=write.native_observed_for(source,&self.observed)
                .map_err(|_|anyhow::anyhow!("native participant mutation representation changed"))? {
                ensure!(affected.contains(row_id) && affected.contains(owner_id),"native beside effect was not locked");
            }
        }
        let ids=affected.into_iter().collect::<Vec<_>>();
        let versions=ids.iter().map(|id|self.rows.get(id).copied().context("native changed participant was not locked")).collect::<Result<Vec<_>>>()?;
        // Actual header/name effects may have advanced the revision already.
        // Otherwise one successful settlement supplies exactly one increment.
        tx.execute("update releases r set native_work_revision=r.native_work_revision+1
            from unnest($1::bigint[],$2::bigint[]) as prior(id,revision) where r.id=prior.id and r.native_work_revision=prior.revision",&[&ids,&versions])?;Ok(())
    }
}
