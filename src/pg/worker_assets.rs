//! Attempt-bound cover reuse. Configuration selects a physical namespace; only
//! real owner-session associations plus verified bytes produce a frame hold.
use anyhow::{Result,Context,ensure};
use sha2::Digest;
use std::{io::Read,cell::RefCell,sync::{Arc,Weak},marker::PhantomData,rc::Rc};
use super::{indexer_runtime::Owner,fresh_assets::{AssetManifest,AssetRequest,AssetKind},natural_cache::{self,Key,Budget}};
const COVER_BYTES:u64=crate::probe::media::MAX_COVER as u64;

pub(crate) struct Grant {
    owner:Weak<Owner>,epoch:[u8;32],catalog:[u8;32],manifest:Arc<AssetManifest>,source:[u8;32],
    pool:Arc<crate::nzb_spool::Pool>,art_pool:Arc<crate::nzb_spool::Pool>,image_pool:Arc<crate::nzb_spool::Pool>,image_writes:std::sync::atomic::AtomicU64,_scratch:tempfile::TempDir,
}
#[derive(Debug,PartialEq,Eq)]
pub(crate) enum ArtworkStatus {Kept,Missing,Unavailable,Changed}
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
#[derive(Debug)]
pub(crate) struct ImageChanged;
impl std::fmt::Display for ImageChanged {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("native image source changed")}}
impl std::error::Error for ImageChanged {}
/// One ImagesStep outcome, retained by Prepared through actual settlement.
/// Neither a descriptor nor a digest supplied by a caller constructs this hold.
pub(crate) struct ImageFrame {grant:Arc<Grant>,release:i64,revision:i64,limit:u64,held:std::collections::HashSet<String>,body:Option<crate::nzb_spool::Spool>}
impl ImageFrame {
    pub(crate) fn check(&self,owner:&Arc<Owner>,release:i64)->Result<()> {
        self.grant.check_owner(owner)?;ensure!(release==self.release,"native image hold belongs to another frame");self.grant.manifest.check_root()?;
        ensure!(self.body.as_ref().is_some_and(|body|body.is_complete()),"native image hold is incomplete");
        // Header/route revisions are revalidated by Queue, under its release
        // locks. Normal supersession must not become an owner failure here.
        Ok(())
    }
    pub(crate) fn cached(&mut self,file:&str,name:&str)->Result<Option<Vec<u8>>> {
        let digest=super::fresh_assets::digest(name)?;let owner=self.grant.owner()?;
        let observed=owner.with(|client| {
            let mut tx=client.build_transaction().isolation_level(postgres::IsolationLevel::RepeatableRead).read_only(true).start()?;
            let current=tx.query_opt("select native_work_revision from releases where id=$1",&[&self.release])?.map(|row|row.try_get::<_,i64>(0)).transpose()?;
            if current!=Some(self.revision){return Ok(None)}
            let row=tx.query_opt("select bytes,case when octet_length(format)<=16 then format end from release_images where release_id=$1 and file=$2 and sha256=$3 order by position limit 1",&[&self.release,&file,&name])?;
            let Some(row)=row else {return Ok(Some(None))};let bytes:i64=row.try_get(0)?;let format:Option<String>=row.try_get(1)?;
            ensure!(bytes>0&&bytes as u64<=self.limit,"cached native image exceeds frame byte admission");let format=format.context("cached native image format exceeds admission")?;
            ensure!(matches!(format.as_str(),"jpeg"|"png"|"gif"|"webp"|"bmp"),"cached native image format is invalid");
            let request=AssetRequest {kind:AssetKind::Images,name:name.into(),digest:Some(digest),bytes:Some(bytes as u64),content_type:crate::images::content_type(&format)};
            let receipt=super::fresh_assets::receipt_in(&mut tx,&request)?;tx.commit()?;Ok(Some(Some((request,receipt))))
        })?;
        let Some(observed)=observed else {return Err(ImageChanged.into())};let Some((request,receipt))=observed else {return Ok(None)};
        let Some(mut body)=self.grant.manifest.read_associated(&request,&self.grant.image_pool,owner.epoch().context("native image epoch absent")?.catalog(),receipt.as_ref())? else{return Ok(None)};
        let mut bytes=vec![0;usize::try_from(body.len())?];body.read_exact(&mut bytes)?;body.rewind()?;
        ensure!(crate::images::sniff(&bytes).is_some_and(|info|crate::images::content_type(info.format)==request.content_type),"cached native image MIME/content mismatch");
        owner.with(|_|Ok(()))?;self.grant.check_owner(&owner)?;self.pin(name,&bytes)?;Ok(Some(bytes))
    }
    fn pin(&mut self,name:&str,bytes:&[u8])->Result<()> {
        use std::io::Write;
        if self.held.contains(name){return Ok(())}ensure!(self.held.len()<10000,"native image frame exceeds result-file admission");
        self.body.as_mut().context("native image hold was already finished")?.write_all(bytes)?;self.held.insert(name.into());Ok(())
    }
    pub(crate) fn finish(mut self)->Result<Self> {
        self.body=Some(self.body.take().context("native image hold was already finished")?.finish()?);Ok(self)
    }
    pub(crate) fn keep(&mut self,bytes:&[u8])->Result<bool> {
        ensure!(!bytes.is_empty()&&bytes.len() as u64<=self.limit,"native produced image exceeds frame byte admission");
        ensure!(crate::images::sniff(bytes).is_some(),"native produced bytes are not a supported image");
        let digest:[u8;32]=sha2::Sha256::digest(bytes).into();let name=super::fresh_assets::hex(&digest);let owner=self.grant.owner()?;
        let current=owner.with(|client|Ok(client.query_opt("select native_work_revision from releases where id=$1",&[&self.release])?.map(|row|row.try_get::<_,i64>(0)).transpose()?))?;
        if current!=Some(self.revision){return Err(ImageChanged.into())}
        // Cached bytes already have an actual association and a retained spool.
        // Avoid rewriting their content or reclassifying an imported receipt.
        if self.held.contains(&name){self.grant.manifest.check_root()?;return Ok(true)}
        let request=AssetRequest {kind:AssetKind::Images,name:name.clone(),digest:Some(digest),bytes:Some(bytes.len() as u64),content_type:"application/octet-stream"};
        let body=match self.grant.manifest.read_recorded(&request,&self.grant.image_pool)? {
            Some(body)=>body,
            None=> {
                let Some((stored,actual))=self.grant.manifest.store_image(bytes,&self.grant.image_writes,&owner.stop)? else{return Ok(false)};
                ensure!(stored==name&&actual==digest,"native image writer content changed");
                self.grant.manifest.read_recorded(&request,&self.grant.image_pool)?.context("native produced image disappeared")?
            }
        };
        let captured=self.grant.manifest.has_entry(&request)?;
        if captured {
            // Captured originals keep their manifest authority and any existing
            // compatible descriptor; no new native source stamp is fabricated.
            let receipt=owner.with(|client|super::fresh_assets::receipt_in(client,&request))?;
            let checked=self.grant.manifest.read_associated(&request,&self.grant.image_pool,owner.epoch().context("native image epoch absent")?.catalog(),receipt.as_ref())?;
            ensure!(checked.is_some(),"captured native image disappeared");
        }else {
            let source=self.grant.manifest.native_source(AssetKind::Images,owner.epoch().context("native image epoch absent")?.catalog())?;
            let stored=owner.with(|client| {
                let mut tx=client.transaction()?;super::history_gate::native(&mut tx)?;
                let current=tx.query_opt("select native_work_revision from releases where id=$1 for update",&[&self.release])?.map(|row|row.try_get::<_,i64>(0)).transpose()?;
                if current!=Some(self.revision){return Ok(false)}
                let count=tx.execute("insert into ext_asset_files as old(kind,name,byte_length,digest,source_digest) values('images',$1,$2,$3,$4) on conflict(kind,name) do update set name=excluded.name where old.byte_length=excluded.byte_length and old.digest=excluded.digest and old.source_digest=excluded.source_digest",&[&name,&(bytes.len() as i64),&digest.as_slice(),&source.as_slice()])?;
                ensure!(count==1,"native image descriptor conflicts with existing namespace/content");tx.commit()?;Ok(true)
            })?;if !stored{return Err(ImageChanged.into())}
        }
        owner.with(|_|Ok(()))?;self.grant.check_owner(&owner)?;drop(body);self.pin(&name,bytes)?;Ok(true)
    }
}
impl Grant {
    pub(crate) fn open(owner:&Arc<Owner>,selection:Option<&crate::config::NativeAssetsConfig>)->Result<Option<Arc<Self>>> {
        let Some(selection)=selection else{return Ok(None)};
        ensure!(tokio::runtime::Handle::try_current().is_err(),"worker assets require a standard thread");
        owner.with(|_|Ok(()))?;
        let epoch=owner.epoch().context("worker assets require the canonical owner")?;
        let manifest=Arc::new(AssetManifest::from_selection(selection)?);
        let source=manifest.native_source(AssetKind::Covers,epoch.catalog())?;
        let mut scratch=tempfile::Builder::new();
        scratch.prefix("atlas-worker-assets-");
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            scratch.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let scratch=scratch.tempdir()?;
        let pool=crate::nzb_spool::Pool::new(scratch.path().into(),crate::nzb_spool::Limits {requests:32,per_request_bytes:COVER_BYTES,total_bytes:32*COVER_BYTES,free_bytes:16*1024*1024})?;
        let art_pool=crate::nzb_spool::Pool::new(scratch.path().into(),crate::nzb_spool::Limits {requests:1,per_request_bytes:8<<20,total_bytes:8<<20,free_bytes:16<<20})?;
        // One raw batch spool per live outcome avoids a descriptor per picture.
        // These are disk limits; per-picture Rust buffers remain <=64MiB.
        let image_pool=crate::nzb_spool::Pool::new(scratch.path().into(),crate::nzb_spool::Limits {requests:128,per_request_bytes:8<<30,total_bytes:16<<30,free_bytes:16<<20})?;
        Ok(Some(Arc::new(Self {owner:Arc::downgrade(owner),epoch:*epoch.id(),catalog:*epoch.catalog().nonce(),manifest,source,pool,art_pool,image_pool,image_writes:std::sync::atomic::AtomicU64::new(0),_scratch:scratch})))
    }
    pub(crate) fn image_frame(self:&Arc<Self>,release:i64,limit:u64)->Result<ImageFrame> {
        ensure!(limit>0&&limit<=super::fresh_assets::IMAGE_BYTES,"native image frame byte admission");let owner=self.owner()?;
        let revision=owner.with(|client|Ok(client.query_opt("select native_work_revision from releases where id=$1",&[&release])?.map(|row|row.try_get::<_,i64>(0)).transpose()?))?.ok_or(ImageChanged)?;
        self.manifest.check_root()?;let body=self.image_pool.open()?;Ok(ImageFrame {grant:self.clone(),release,revision,limit,held:std::collections::HashSet::new(),body:Some(body)})
    }
    /// Service factories must bind the grant to their exact original attempt
    /// before constructing any cache/provider clients or performing work.
    pub(crate) fn require_owner(&self,owner:&Arc<Owner>)->Result<()> {self.check_owner(owner)}
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
        let captured=self.manifest.contains_cover(&name)?;
        let spool=if captured {self.manifest.read_cover(&request,&self.pool)?}else{self.manifest.read_recorded(&request,&self.pool)?}
            .context("native produced cover disappeared")?;
        let (revision,receipt)=owner.with(|client| {
            let mut tx=client.transaction()?;super::history_gate::native(&mut tx)?;
            let revision:i64=tx.query_one("select native_work_revision from releases where id=$1",&[&release])?.try_get(0)?;
            let receipt=if captured {super::fresh_assets::receipt_in(&mut tx,&request)?}else {
                let rows=tx.execute("insert into ext_asset_files as old(kind,name,byte_length,digest,source_digest) values('covers',$1,$2,$3,$4) on conflict(kind,name) do update set name=excluded.name where old.byte_length=excluded.byte_length and old.digest=excluded.digest and old.source_digest=excluded.source_digest",&[&name,&(cover.bytes.len() as i64),&digest.as_slice(),&self.source.as_slice()])?;
                ensure!(rows==1,"native asset descriptor conflicts with preserved original");None
            };tx.commit()?;Ok((revision,receipt))
        })?;
        if captured {
            // Compatible captured originals keep their copier/import provenance,
            // as in ImageFrame::keep; only unlisted covers need native evidence.
            let checked=self.manifest.read_associated(&request,&self.pool,owner.epoch().context("native cover epoch absent")?.catalog(),receipt.as_ref())?;
            ensure!(checked.is_some(),"captured native cover disappeared");
        }
        self.check_owner(&owner)?;Ok(CoverHold {grant:self.clone(),release,_revision:revision,mime:cover.mime.into(),name,spool})
    }
    fn poster_request(&self,path:&str)->Result<AssetRequest> {
        ensure!(path.len()<=16384,"native artwork path exceeds admission");
        let name=crate::media::details::kept_image_name(path).context("invalid native artwork filename")?;
        super::cache_assets::filename(&name)?;
        Ok(AssetRequest {kind:AssetKind::Posters,name,digest:None,bytes:None,content_type:"image/jpeg"})
    }
    pub(crate) fn poster_available(&self,kind:i64,id:i64,path:&str)->Result<ArtworkStatus> {
        let owner=self.owner()?;let request=self.poster_request(path)?;
        let association=owner.with(|client| {
            let mut tx=client.build_transaction().isolation_level(postgres::IsolationLevel::RepeatableRead).read_only(true).start()?;
            let same=tx.query_opt("select poster is not distinct from $3::text from ext_media_details where kind=$1 and tmdb_id=$2",&[&kind,&id,&path])?.map(|row|row.try_get::<_,bool>(0)).transpose()?.unwrap_or(false);
            if !same {return Ok(None)}
            let receipt=super::fresh_assets::receipt_in(&mut tx,&request)?;tx.commit()?;Ok(Some(receipt))
        })?;
        let Some(receipt)=association else{return Ok(ArtworkStatus::Changed)};
        let expected=self.manifest.has_entry(&request)?||receipt.is_some();
        let result=self.manifest.read_associated(&request,&self.art_pool,owner.epoch().context("artwork owner epoch missing")?.catalog(),receipt.as_ref())?;
        owner.with(|_|Ok(()))?;
        Ok(if result.is_some(){ArtworkStatus::Kept}else if expected{ArtworkStatus::Unavailable}else{ArtworkStatus::Missing})
    }
    /// Called with actual bounded bytes returned by the configured image client.
    /// The current metadata path is checked before I/O and before its receipt.
    pub(crate) fn produce_poster(&self,kind:i64,id:i64,path:&str,bytes:&[u8])->Result<ArtworkStatus> {
        let owner=self.owner()?;let request=self.poster_request(path)?;
        let same=owner.with(|client|Ok(client.query_opt("select poster is not distinct from $3::text from ext_media_details where kind=$1 and tmdb_id=$2",&[&kind,&id,&path])?.map(|row|row.try_get::<_,bool>(0)).transpose()?.unwrap_or(false)))?;
        if !same {return Ok(ArtworkStatus::Changed)}
        let digest=self.manifest.store_poster(&request.name,bytes)?;
        let checked=AssetRequest {digest:Some(digest),bytes:Some(bytes.len() as u64),..request};
        let _verified=self.manifest.read_recorded(&checked,&self.art_pool)?.context("native produced poster disappeared")?;
        let source=self.manifest.native_source(AssetKind::Posters,owner.epoch().context("artwork owner epoch missing")?.catalog())?;
        let stored=owner.with(|client| {
            let mut tx=client.transaction()?;super::history_gate::native(&mut tx)?;
            let same=tx.query_opt("select poster is not distinct from $3::text from ext_media_details where kind=$1 and tmdb_id=$2 for update",&[&kind,&id,&path])?.map(|row|row.try_get::<_,bool>(0)).transpose()?.unwrap_or(false);
            if !same {return Ok(false)}
            let rows=tx.execute("insert into ext_asset_files as old(kind,name,byte_length,digest,source_digest) values('posters',$1,$2,$3,$4) on conflict(kind,name) do update set name=excluded.name where old.byte_length=excluded.byte_length and old.digest=excluded.digest and old.source_digest=excluded.source_digest",&[&checked.name,&(bytes.len() as i64),&digest.as_slice(),&source.as_slice()])?;
            ensure!(rows==1,"native artwork descriptor conflicts with preserved original");tx.commit()?;Ok(true)
        })?;
        Ok(if stored{ArtworkStatus::Kept}else{ArtworkStatus::Changed})
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
#[cfg(all(test,unix))]
#[path="worker_asset_tests.rs"]
mod tests;

#[cfg(all(test,unix))]
#[path="worker_image_asset_tests.rs"]
mod image_tests;
