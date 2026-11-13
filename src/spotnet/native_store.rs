//! Spotnet mutations on the original native owner. No pooled reconnect or
//! SQLite callback is permitted here. Publication and its bookkeeping commit
//! together; NZB-derived rows do not claim an NXV/header capture.
use anyhow::{Result, ensure};
use postgres::Transaction;
use sha2::{Digest, Sha256};
use std::sync::{Arc, atomic::Ordering};

use crate::pg::{indexer_runtime::Owner, ingest::NativeSavePermit, sql::Pg};
use super::{comments, native_plan::Plan, store::{self, Placement, Settled, SpotRow, SAVED, SKIPPED}};

pub(super) struct Store { pub(super) owner: Arc<Owner> }
impl Store {
    pub(super) fn new(owner: &Arc<Owner>) -> Result<Self> {
        ensure!(tokio::runtime::Handle::try_current().is_err(), "native Spotnet requires a standard thread");
        ensure!(owner.epoch().is_some() && owner.cache_destination().is_some(), "native Spotnet requires the original checked owner");
        let store=Self {owner:owner.clone()};store.check()?;Ok(store)
    }
    pub(super) fn check(&self)->Result<()> {
        ensure!(tokio::runtime::Handle::try_current().is_err(), "native Spotnet PG access entered Tokio");
        self.owner.with(|_|Ok(()))
    }
    pub(super) fn with<T>(&self, work:impl FnOnce(&mut Transaction<'_>)->Result<T>)->Result<T> {
        ensure!(tokio::runtime::Handle::try_current().is_err(), "native Spotnet PG access entered Tokio");
        self.owner.with(|client| {
            let mut tx=crate::pg::release_queue::begin(client)?;
            self.owner.epoch().expect("checked owner").verify(&mut *tx)?;
            let value=work(&mut tx)?;
            self.not_stopped()?;
            tx.commit()?;Ok(value)
        })
    }
    pub(super) fn not_stopped(&self)->Result<()> {
        if self.owner.stop.load(Ordering::Acquire) {return Err(crate::pg::outage::AttemptEnded.into())}
        Ok(())
    }
    pub(super) fn settle(&self,id:&str,outcome:Option<&Settled>,now:i64)->Result<()> {
        self.with(|tx|Ok(store::settle(&Pg::new(&mut *tx),id,outcome,now)?))
    }
    /// The same transaction covers the real writer acknowledgement, tag and
    /// seen state. The permit is borrowed until COMMIT/rollback has finished.
    pub(super) fn publish(&self,plan:&mut Plan<'_>,row:&SpotRow,nzb_ref:&str,permit:&NativeSavePermit,now:i64)->Result<Settled> {
        self.with(|tx| {
            self.not_stopped()?;
            // Same lock and order as the existing writer, acquired before
            // placement so an ordinary indexer save cannot race the decision.
            let group=&plan.metadata().group;
            tx.query_one("select pg_advisory_xact_lock(hashtextextended(current_schema() || ':' || $1,7936))",&[group])?;
            let known=store::known_key(&Pg::new(&mut *tx),plan.key())?;
            let tagged=tx.query_opt("select s.release_id from spots s join releases r on r.id=s.release_id where s.nzb_key=$1 order by s.release_id limit 1",&[&plan.key()])?.map(|r|r.get::<_,i64>(0));
            let id=if let Some(id)=known.or(tagged) {Some(id)} else {
                match placement(tx,plan)? {
                    Placement::SamePost(id)=>Some(id),
                    placed=> {
                        if placed==Placement::Taken {
                            plan.rename(&format!("{} [{}]",plan.metadata().name,&plan.key()[..8]));
                            // A truncated-key collision must never merge posts.
                            match placement(tx,plan)? {
                                Placement::SamePost(id)=>return finish(tx,row,id,nzb_ref,Some(plan.key()),now),
                                Placement::Taken=>anyhow::bail!("Spotnet collision suffix already belongs to another post"),
                                Placement::Free=>{},
                            }
                        }
                        // Detect the whole-input executable rule before the
                        // first batch. A late executable must not resurrect
                        // an earlier chunk after the writer retracts it.
                        if let Some(marker)=plan.junk_marker() {
                            let batch=vec![marker];permit.check_batch(&batch)?;
                            crate::pg::release_writer::save_in(tx,&batch,now)?;
                        } else {
                            loop {
                                self.not_stopped()?;
                                let Some(batch)=plan.next_batch(permit.max_job_bytes())? else {break};
                                let batch=vec![batch];permit.check_batch(&batch)?;
                                crate::pg::release_writer::save_in(tx,&batch,now)?;
                                drop(batch); // no overlap with the next expansion
                            }
                        }
                        release_id(tx,plan)?
                    }
                }
            };
            if let Some(id)=id {finish(tx,row,id,nzb_ref,Some(plan.key()),now)} else {
                let result=Settled {state:SKIPPED,nzb_ref:Some(nzb_ref.to_owned()),nzb_key:Some(plan.key().to_owned()),..Default::default()};
                store::settle(&Pg::new(&mut *tx),&row.message_id,Some(&result),now)?;Ok(result)
            }
        })
    }
    pub(super) fn copy_of(&self,id:i64,row:&SpotRow,nzb_ref:&str,now:i64)->Result<Settled> {
        self.with(|tx| {
            let key=tx.query_opt("select nzb_key from spots where release_id=$1",&[&id])?
                .and_then(|r|r.get::<_,Option<String>>(0));
            let mut row=row.clone();row.nzb_key=key.clone().unwrap_or_default();
            finish(tx,&row,id,nzb_ref,key.as_deref(),now)
        })
    }
    pub(super) fn settle_comments(&self,outcomes:&[(String,comments::Outcome)])->Result<()> {
        self.with(|tx| {
            for (id,outcome) in outcomes {
                match outcome {
                    comments::Outcome::Read(body)=>{
                        tx.execute("update spot_comments set state=$1,body=$2,native_original_body=$3 where message_id=$4",&[&comments::READ,&text(body).as_ref(),&body.as_bytes(),id])?;
                    }
                    _=>comments::settle(&Pg::new(&mut *tx),&[(id.clone(),outcome.clone())])?,
                }
            }Ok(())
        })
    }
    pub(super) fn missing_xml(&self,limit:usize)->Result<Vec<(i64,String)>> {
        self.with(|tx|Ok(tx.query("select release_id,message_id from spots where xml is null order by random() limit $1",&[&(limit as i64)])?
            .into_iter().map(|r|(r.get(0),r.get(1))).collect()))
    }
    pub(super) fn set_xml(&self,id:i64,message_id:&str,xml:&str)->Result<bool> {
        self.with(|tx|Ok(tx.execute("update spots set xml=$3,native_original_xml=$4 where release_id=$1 and message_id=$2 and xml is null",&[&id,&message_id,&text(xml).as_ref(),&xml.as_bytes()])?>0))
    }
}

fn release_id(tx:&mut Transaction<'_>,plan:&Plan<'_>)->Result<Option<i64>> {
    let (collection,key)=crate::pg::release_writer::identity(plan.metadata());
    let found=tx.query_opt("select id,original_name from releases where group_name=$1 and is_collection=$2 and identity_key=$3",&[&plan.metadata().group,&collection,&&key[..]])?;
    match found {
        None=>Ok(None),
        Some(row)=> {
            ensure!(row.get::<_,Option<Vec<u8>>>(1).as_deref()==Some(plan.metadata().name.as_bytes()),"native Spotnet release digest/original identity mismatch");
            Ok(Some(row.get(0)))
        }
    }
}
fn placement(tx:&mut Transaction<'_>,plan:&Plan<'_>)->Result<Placement> {
    let Some(id)=release_id(tx,plan)? else {return Ok(Placement::Free)};
    if tx.query_one("select exists(select 1 from spots where release_id=$1 and nzb_key=$2)",&[&id,&plan.key()])?.get::<_,bool>(0) {return Ok(Placement::SamePost(id))}
    let mut identities=plan.identities();
    loop {
        let chunk=identities.by_ref().take(512).collect::<Vec<_>>();if chunk.is_empty() {break}
        let files=chunk.iter().map(|(file,_)|file.as_bytes().to_vec()).collect::<Vec<_>>();
        let messages=chunk.iter().map(|(_,mid)|format!("<{mid}>").into_bytes()).collect::<Vec<_>>();
        let file_keys=files.iter().map(|file|Sha256::digest(file).to_vec()).collect::<Vec<_>>();
        let message_keys=messages.iter().map(|mid|Sha256::digest(mid).to_vec()).collect::<Vec<_>>();
        // Hash lookup AND exact original bytes; no digest-only identity and no
        // fetching all existing segments into memory.
        let same:bool=tx.query_one("select not exists(select 1 from unnest($2::bytea[],$3::bytea[],$4::bytea[],$5::bytea[]) w(filename,mid,file_key,mid_key) where not exists(select 1 from files f join segments s on s.release_id=f.release_id and s.file_id=f.id where f.release_id=$1 and f.filename_key=w.file_key and f.original_filename=w.filename and s.message_key=w.mid_key and s.message_id=w.mid))",&[&id,&files,&messages,&file_keys,&message_keys])?.get(0);
        if !same {return Ok(Placement::Taken)}
    }
    Ok(Placement::SamePost(id))
}
fn finish(tx:&mut Transaction<'_>,row:&SpotRow,id:i64,nzb_ref:&str,key:Option<&str>,now:i64)->Result<Settled> {
    tag(tx,id,row,now)?;
    let result=Settled {state:SAVED,release_id:Some(id),nzb_ref:Some(nzb_ref.to_owned()),nzb_key:key.map(str::to_owned)};
    store::settle(&Pg::new(&mut *tx),&row.message_id,Some(&result),now)?;Ok(result)
}
fn tag(tx:&mut Transaction<'_>,id:i64,row:&SpotRow,now:i64)->Result<()> {
    // Saved history can outlive a deleted release, as with legacy frozen IDs.
    if tx.query_opt("select id from releases where id=$1 for update",&[&id])?.is_none() {return Ok(())}
    let s=&row.spot;
    let changed=tx.execute("insert into spots(release_id,message_id,kind,title,poster,tag,spotter_id,category,subcats,size,imdb_id,created,nzb_key,xml,native_original_xml) values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15) on conflict(release_id) do update set message_id=excluded.message_id,kind=excluded.kind,title=excluded.title,poster=excluded.poster,tag=excluded.tag,spotter_id=excluded.spotter_id,category=excluded.category,subcats=excluded.subcats,size=excluded.size,imdb_id=excluded.imdb_id,created=excluded.created,nzb_key=excluded.nzb_key,xml=excluded.xml,native_original_xml=excluded.native_original_xml where coalesce(excluded.created,0)>=coalesce(spots.created,0)",&[&id,&row.message_id,&row.kind.as_str(),&text(&s.title).as_ref(),&text(&s.poster).as_ref(),&text(&s.tag).as_ref(),&row.spotter_id,&i64::from(s.category),&s.subcats.spotweb(),&(s.size as i64),&s.imdb_id(),&s.created,&row.nzb_key,&text(&row.xml).as_ref(),&row.xml.as_bytes()])?;
    if changed>0 {
        tx.execute("update releases set native_work_revision=native_work_revision+1 where id=$1",&[&id])?;
        crate::pg::stage_queue::rerun_in(tx,id,crate::postprocess::step::mask(crate::category::step::STEP),now)?;
    }Ok(())
}
fn text(value:&str)->std::borrow::Cow<'_,str> {
    if value.contains('\0') {std::borrow::Cow::Owned(value.replace('\0',"\u{fffd}"))} else {std::borrow::Cow::Borrowed(value)}
}
