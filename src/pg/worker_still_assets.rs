//! Stills produced by the real native frame, never a caller-supplied permission.
use super::*;
use std::{collections::{HashSet,HashMap},io::{Read,Write}};

#[derive(Debug)]
pub(crate) struct StillChanged;
impl std::fmt::Display for StillChanged {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("native still source changed")}}
impl std::error::Error for StillChanged {}
#[derive(Debug)]
pub(crate) struct StillAdmission;
impl std::fmt::Display for StillAdmission {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("native still cache admission unavailable")}}
impl std::error::Error for StillAdmission {}

/// At most the producer's three JPEGs, copied to one already-admitted batch
/// spool and held until Prepared receives the original owner's settle answer.
pub(crate) struct StillFrame {
    grant:Arc<Grant>,release:i64,revision:i64,held:HashSet<String>,files:HashMap<String,super::super::fresh_assets::StillFile>,body:Option<crate::nzb_spool::Spool>,
}
impl StillFrame {
    fn current(&self)->Result<Arc<Owner>> {
        let owner=self.grant.owner()?;
        let current=owner.with(|client|Ok(client.query_opt("select native_work_revision from releases where id=$1",&[&self.release])?.map(|row|row.try_get::<_,i64>(0)).transpose()?))?;
        if current!=Some(self.revision){return Err(StillChanged.into())}Ok(owner)
    }
    fn pin(&mut self,name:&str,body:&mut crate::nzb_spool::Spool,file:super::super::fresh_assets::StillFile)->Result<()> {
        if self.held.contains(name){return Ok(())}
        if self.held.len()>=crate::probe::stills::POSITIONS.len(){return Err(StillAdmission.into())}
        ensure!(body.is_complete()&&body.len()>0&&body.len()<=super::super::fresh_assets::STILL_BYTES,"native still hold exceeds JPEG admission");
        body.rewind()?;let output=self.body.as_mut().context("native still hold was already finished")?;
        let mut scratch=[0u8;64*1024];loop {let count=body.read(&mut scratch)?;if count==0{break;}output.write_all(&scratch[..count])?;}
        body.rewind()?;self.grant.manifest.check_still_hold(name,&file)?;self.held.insert(name.into());self.files.insert(name.into(),file);Ok(())
    }
    pub(crate) fn reuse(&mut self,name:&str)->Result<bool> {
        self.current()?;
        let Some(file)=self.grant.manifest.hold_still(name)? else{return Ok(false)};
        let Some(mut body)=self.grant.read_still(name)? else{return Ok(false)};
        self.pin(name,&mut body,file)?;self.current()?;Ok(true)
    }
    pub(crate) fn keep(&mut self,bytes:&[u8],width:u32,height:u32)->Result<String> {
        if bytes.is_empty()||bytes.len() as u64>super::super::fresh_assets::STILL_BYTES
            ||width==0||width>960||height==0||height>1920 {return Err(StillAdmission.into())}
        ensure!(crate::images::sniff(bytes).is_some_and(|info|info.format=="jpeg"&&info.width==width&&info.height==height),"native still JPEG dimensions/content mismatch");
        let name=super::super::fresh_assets::hex(&sha2::Sha256::digest(bytes));let owner=self.current()?;
        if self.held.contains(&name){return Ok(name)}
        if self.held.len()>=crate::probe::stills::POSITIONS.len(){return Err(StillAdmission.into())}
        let request=Grant::still_request(&name)?;
        if self.grant.manifest.has_entry(&request)? {
            // A captured original keeps its manifest/copier provenance. It is
            // not rewritten, deleted or restamped as a newly produced file.
            let Some(file)=self.grant.manifest.hold_still(&name)? else{return Err(StillAdmission.into())};
            let Some(mut body)=self.grant.read_still(&name)? else{return Err(StillAdmission.into())};
            self.pin(&name,&mut body,file)?;
        }else {
            let Some((stored,digest))=self.grant.manifest.store_still(bytes,&self.grant.still_writes,&owner.stop)? else{return Err(StillAdmission.into())};
            ensure!(stored==name,"native still writer content changed");
            let Some(file)=self.grant.manifest.hold_still(&name)? else{return Err(StillAdmission.into())};
            let checked=AssetRequest {bytes:Some(bytes.len() as u64),..request};
            let mut body=self.grant.manifest.read_recorded(&checked,&self.grant.image_pool)?.context("native produced still disappeared")?;
            self.pin(&name,&mut body,file)?;
            let source=self.grant.manifest.native_source(AssetKind::Stills,owner.epoch().context("native still epoch absent")?.catalog())?;
            let published=owner.with(|client| {
                let mut tx=client.transaction()?;super::super::history_gate::native(&mut tx)?;
                let current=tx.query_opt("select native_work_revision from releases where id=$1 for update",&[&self.release])?.map(|row|row.try_get::<_,i64>(0)).transpose()?;
                if current!=Some(self.revision){return Ok(false)}
                let count=tx.execute("insert into ext_asset_files as old(kind,name,byte_length,digest,source_digest) values('stills',$1,$2,$3,$4) on conflict(kind,name) do update set name=excluded.name where old.byte_length=excluded.byte_length and old.digest=excluded.digest and old.source_digest=excluded.source_digest",
                    &[&name,&(bytes.len() as i64),&digest.as_slice(),&source.as_slice()])?;
                ensure!(count==1,"native still descriptor conflicts with existing namespace/content");tx.commit()?;Ok(true)
            })?;
            if !published{return Err(StillChanged.into())}
        }
        self.current()?;self.grant.check_owner(&owner)?;Ok(name)
    }
    pub(crate) fn finish(mut self)->Result<Self> {
        self.body=Some(self.body.take().context("native still hold was already finished")?.finish()?);Ok(self)
    }
    pub(crate) fn check(&self,owner:&Arc<Owner>,release:i64)->Result<()> {
        self.grant.check_owner(owner)?;self.grant.manifest.check_root()?;
        ensure!(self.release==release&&self.held.len()<=crate::probe::stills::POSITIONS.len()
            &&self.body.as_ref().is_some_and(|body|body.is_complete()&&body.len()<=3*super::super::fresh_assets::STILL_BYTES),"native still hold belongs to another frame or exceeds admission");
        ensure!(self.files.len()==self.held.len(),"native still physical holds are incomplete");
        for (name,file) in &self.files {self.grant.manifest.check_still_hold(name,file)?;}
        // Queue revalidates the source revision under participant locks.
        // An ordinary supersession must not be promoted to owner failure.
        Ok(())
    }
}
impl Grant {
    fn still_request(name:&str)->Result<AssetRequest> {
        Ok(AssetRequest {kind:AssetKind::Stills,name:name.into(),digest:Some(super::super::fresh_assets::digest(name)?),bytes:None,content_type:"image/jpeg"})
    }
    fn read_still(&self,name:&str)->Result<Option<crate::nzb_spool::Spool>> {
        let owner=self.owner()?;let request=Self::still_request(name)?;
        let receipt=owner.with(|client|super::super::fresh_assets::receipt_in(client,&request))?;
        let Some(mut body)=self.manifest.read_associated(&request,&self.image_pool,owner.epoch().context("native still epoch absent")?.catalog(),receipt.as_ref())? else{return Ok(None)};
        ensure!(body.len()>=3&&body.len()<=super::super::fresh_assets::STILL_BYTES,"native still JPEG extent admission");
        let mut prefix=[0;3];body.read_exact(&mut prefix)?;ensure!(prefix==[0xff,0xd8,0xff],"native still is not a JPEG");body.rewind()?;
        owner.with(|_|Ok(()))?;self.check_owner(&owner)?;Ok(Some(body))
    }
    pub(crate) fn still_available(&self,name:&str)->Result<bool> {Ok(self.read_still(name)?.is_some())}
    pub(crate) fn still_frame(self:&Arc<Self>,release:i64)->Result<StillFrame> {
        let owner=self.owner()?;self.manifest.check_root()?;
        let revision=owner.with(|client|Ok(client.query_opt("select native_work_revision from releases where id=$1",&[&release])?.map(|row|row.try_get::<_,i64>(0)).transpose()?))?.ok_or(StillChanged)?;
        let body=self.image_pool.open()?;
        Ok(StillFrame {grant:self.clone(),release,revision,held:HashSet::new(),files:HashMap::new(),body:Some(body)})
    }
}
