//! Attempt-bound cover reuse. Configuration selects a physical namespace; only
//! real owner-session associations plus verified bytes produce a frame hold.
use anyhow::{Result,Context,ensure};
use sha2::{Digest,Sha256};
use std::{io::Read,cell::RefCell,sync::{Arc,Weak},marker::PhantomData,rc::Rc};
use super::{indexer_runtime::Owner,fresh_assets::{AssetManifest,AssetRequest,AssetKind},natural_cache::{self,Key,Budget}};
const COVER_BYTES:u64=crate::probe::media::MAX_COVER as u64;

pub(crate) struct Grant {
    owner:Weak<Owner>,epoch:[u8;32],catalog:[u8;32],manifest:Arc<AssetManifest>,source:[u8;32],
    pool:Arc<crate::nzb_spool::Pool>,_scratch:tempfile::TempDir,
}
pub(crate) enum CoverStatus {Verified(CoverHold),Changed,Missing}
pub(crate) struct CoverHold {
    grant:Arc<Grant>,release:i64,_revision:i64,mime:String,name:String,
    spool:crate::nzb_spool::Spool,
}
impl CoverHold {
    pub(crate) fn descriptor(&self)->(String,String) {(self.mime.clone(),self.name.clone())}
    pub(crate) fn check(&self,owner:&Arc<Owner>,release:i64)->Result<()> {
        self.grant.check_owner(owner)?;
        ensure!(release==self.release&&self.spool.is_complete()&&self.spool.len()<=COVER_BYTES,"worker cover belongs to another frame");
        self.grant.manifest.check_root()?;
        // The source revision is pinned in the actual visit's Observed set;
        // Queue checks it under sorted participant locks before any effect.
        // Do not convert normal supersession into a fatal owner failure here.
        Ok(())
    }
}
impl Grant {
    pub(crate) fn open(owner:&Arc<Owner>,selection:Option<&crate::config::NativeAssetsConfig>)->Result<Option<Arc<Self>>> {
        let Some(selection)=selection else{return Ok(None)};
        ensure!(tokio::runtime::Handle::try_current().is_err(),"worker assets require a standard thread");
        owner.with(|_|Ok(()))?;
        let epoch=owner.epoch().context("worker assets require the canonical owner")?;
        let manifest=Arc::new(AssetManifest::from_selection(selection)?);
        let mut source=Sha256::new();source.update(b"atlas.native-cover-writer.v1\0");source.update(manifest.namespace()?);source.update(epoch.catalog().nonce());
        let scratch=tempfile::Builder::new().prefix("atlas-worker-assets-").tempdir()?;
        let pool=crate::nzb_spool::Pool::new(scratch.path().into(),crate::nzb_spool::Limits {requests:32,per_request_bytes:COVER_BYTES,total_bytes:32*COVER_BYTES,free_bytes:16*1024*1024})?;
        Ok(Some(Arc::new(Self {owner:Arc::downgrade(owner),epoch:*epoch.id(),catalog:*epoch.catalog().nonce(),manifest,source:source.finalize().into(),pool,_scratch:scratch})))
    }
    fn check_owner(&self,owner:&Arc<Owner>)->Result<()> {
        ensure!(self.owner.upgrade().is_some_and(|bound|Arc::ptr_eq(&bound,owner)),"worker asset owner changed");
        ensure!(owner.epoch().is_some_and(|epoch|epoch.id()==&self.epoch&&epoch.catalog().nonce()==&self.catalog),"worker asset epoch/catalog changed");
        if owner.stop.load(std::sync::atomic::Ordering::Acquire) {return Err(super::outage::AttemptEnded.into())}Ok(())
    }
    fn owner(&self)->Result<Arc<Owner>> {let owner=self.owner.upgrade().context("worker asset attempt ended")?;self.check_owner(&owner)?;Ok(owner)}
    fn request(mime:&str,name:&str)->Result<AssetRequest> {
        ensure!(matches!(mime,"image/jpeg"|"image/png"),"invalid worker cover MIME");
        let digest=super::fresh_assets::digest(name)?;
        Ok(AssetRequest {kind:AssetKind::Covers,name:name.into(),digest:Some(digest),bytes:None,content_type:if mime=="image/png"{"image/png"}else{"image/jpeg"}})
    }
    pub(crate) fn verify_probe(self:&Arc<Self>,release:i64,key:Key<'_>,parts:i64,complete:bool,stamp:u64,saved:&crate::raw_store::SavedProbe,checked:&crate::probe::step::CheckedSavedProbe)->Result<CoverStatus> {
        let owner=self.owner()?;
        // The full natural-key association and canonical source are checked on
        // the original owner RR, not inferred from caller-supplied SHA strings.
        let association=owner.with(|client| {
            let mut tx=client.build_transaction().isolation_level(postgres::IsolationLevel::RepeatableRead).read_only(true).start()?;
            let row=tx.query_opt("select native_work_revision,original_name=$2 and group_name=$3 and coalesce(parts,0)=$4 and complete=$5 from releases where id=$1",&[&release,&key.name.as_bytes(),&key.group,&parts,&complete])?;
            let Some(row)=row else{return Ok(None)};
            if !row.try_get::<_,bool>(1)? || super::content_id::read(&mut tx,release,&owner.stop)?.as_deref()!=Some(key.content_id) {return Ok(None)}
            let Some(actual)=natural_cache::probe(&mut tx,key,parts,complete,stamp,Budget {bytes:32<<20,rows:8})? else{return Ok(None)};
            if actual!=*saved {return Ok(None)}
            if !checked.matches(&actual.info) {return Ok(None)}
            let Some((mime,name))=actual.cover else{return Ok(None)};
            let revision:i64=row.try_get(0)?;
            let receipt=tx.query_opt("select byte_length,digest,source_digest from ext_asset_files where kind='covers' and name=$1",&[&name])?
                .map(|row|Ok::<_,postgres::Error>((row.try_get::<_,i64>(0)?,row.try_get::<_,Vec<u8>>(1)?,row.try_get::<_,Vec<u8>>(2)?))).transpose()?;
            tx.commit()?;Ok(Some((revision,mime,name,receipt)))
        })?;
        let Some((revision,mime,name,receipt))=association else{return Ok(CoverStatus::Changed)};
        let mut request=Self::request(&mime,&name)?;
        if let Some((length,digest,_))=&receipt {
            ensure!((1..=COVER_BYTES as i64).contains(length)&&Some(digest.as_slice())==request.digest.as_ref().map(|d|d.as_slice()),"PG cover descriptor and actual association disagree");
            request.bytes=Some(*length as u64);
        }
        let spool=if self.manifest.contains_cover(&name)? {
            self.manifest.read_cover(&request,&self.pool)?
        } else if let Some((length,digest,source))=receipt {
            ensure!(source.as_slice()==self.source.as_slice() && (1..=COVER_BYTES as i64).contains(&length)&&Some(digest.as_slice())==request.digest.as_ref().map(|d|d.as_slice()),"native asset receipt namespace/content mismatch");
            request.bytes=Some(length as u64);self.manifest.read_recorded(&request,&self.pool)?
        } else {None};
        let Some(mut spool)=spool else{return Ok(CoverStatus::Missing)};
        ensure!(spool.len()<=COVER_BYTES,"worker cover exceeds owned admission");
        let mut bytes=vec![0;spool.len() as usize];spool.read_exact(&mut bytes)?;
        ensure!(crate::probe::media::cover_mime(&bytes)==Some(mime.as_str()),"retained cover bytes/MIME mismatch");
        spool.rewind()?;
        owner.with(|_|Ok(()))?;self.check_owner(&owner)?;
        Ok(CoverStatus::Verified(CoverHold {grant:self.clone(),release,_revision:revision,mime,name,spool}))
    }
    /// Called only with bytes the current native probe actually produced. A
    /// durable content-addressed file precedes its same-owner descriptor commit;
    /// interruption can leave an inert file, never a fabricated receipt or hit.
    pub(crate) fn produce_cover(self:&Arc<Self>,release:i64,cover:&crate::probe::media::Cover)->Result<CoverHold> {
        let owner=self.owner()?;
        let exists=owner.with(|client|Ok(client.query_opt("select 1 from releases where id=$1",&[&release])?.is_some()))?;
        ensure!(exists,"native cover source no longer exists");
        ensure!(crate::probe::media::cover_mime(&cover.bytes)==Some(cover.mime),"native cover producer bytes/MIME mismatch");
        let (name,digest)=self.manifest.store_cover(&cover.bytes)?;
        let request=AssetRequest {bytes:Some(cover.bytes.len() as u64),..Self::request(cover.mime,&name)?};
        let spool=self.manifest.read_recorded(&request,&self.pool)?.context("native produced cover disappeared")?;
        let revision=owner.with(|client| {
            let mut tx=client.transaction()?;super::history_gate::native(&mut tx)?;
            let revision:i64=tx.query_one("select native_work_revision from releases where id=$1",&[&release])?.try_get(0)?;
            let rows=tx.execute("insert into ext_asset_files as old(kind,name,byte_length,digest,source_digest) values('covers',$1,$2,$3,$4) on conflict(kind,name) do update set name=excluded.name where old.byte_length=excluded.byte_length and old.digest=excluded.digest and old.source_digest=excluded.source_digest",&[&name,&(cover.bytes.len() as i64),&digest.as_slice(),&self.source.as_slice()])?;
            ensure!(rows==1,"native asset descriptor conflicts with preserved original");tx.commit()?;Ok(revision)
        })?;
        self.check_owner(&owner)?;Ok(CoverHold {grant:self.clone(),release,_revision:revision,mime:cover.mime.into(),name,spool})
    }
}
thread_local! {static CURRENT:RefCell<Option<(Weak<Owner>,Option<Arc<Grant>>)>>=const{RefCell::new(None)};}
pub(crate) struct Scope {previous:Option<(Weak<Owner>,Option<Arc<Grant>>)>,_thread:PhantomData<Rc<()>>}
impl Scope {
    pub(crate) fn bind(owner:&Arc<Owner>,grant:Option<&Arc<Grant>>)->Result<Self> {
        if let Some(grant)=grant {grant.check_owner(owner)?;}
        let previous=CURRENT.with(|slot|slot.replace(Some((Arc::downgrade(owner),grant.cloned()))));
        Ok(Self {previous,_thread:PhantomData})
    }
}
impl Drop for Scope {fn drop(&mut self){CURRENT.with(|slot|{slot.replace(self.previous.take());});}}
pub(crate) fn current()->Result<Option<Arc<Grant>>> {
    CURRENT.with(|slot| {
        let bound=slot.borrow();let (owner,grant)=bound.as_ref().context("worker asset frame is unavailable")?;
        let owner=owner.upgrade().context("worker asset frame ended")?;
        if let Some(grant)=grant {grant.check_owner(&owner)?;}Ok(grant.clone())
    })
}
#[cfg(test)]
#[path="worker_asset_tests.rs"]
mod tests;
