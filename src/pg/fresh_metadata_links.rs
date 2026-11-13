//! Existing metadata associations, not payload relationships. Names and
//! Segment UIDs never authorize copying, password inheritance or hiding rows.
use super::fresh_reader::ReadView;
use crate::{web_meta::MetaInputs,search::ReleaseRow,sidecar::Found};
use anyhow::{Context,Result,ensure};
use postgres::fallible_iterator::FallibleIterator;
use sha2::{Digest,Sha256};
use std::collections::HashSet;

enum SidecarMatch {Uid,Name}
impl ReadView<'_> {
    pub(super) fn metadata_nzb_targets(&self,inputs:&MetaInputs<'_>)->Result<Vec<(String,Vec<ReleaseRow>)>> {
        let bytes=inputs.inner.files.iter().filter(|file|file.source==crate::archive::store::SOURCE_NZB).try_fold(4096u64,|sum,file|sum.checked_add((file.name.len() as u64).checked_mul(8).context("native NZB target extent overflow")?).context("native NZB target extent overflow"))?;
        self.admit_owned(bytes)?;
        let names=crate::archive::nzb::release_names(inputs.inner.files.iter().filter(|file|file.source==crate::archive::store::SOURCE_NZB).map(|file|file.name.as_str()));
        let mut result=Vec::new();
        for name in names {
            self.admit_owned((name.len() as u64).checked_mul(8).and_then(|n|n.checked_add(4096)).context("native target query extent overflow")?)?;
            let key:[u8;32]=Sha256::digest(name.as_bytes()).into();self.before_query()?;
            let mut ids=Vec::new();
            {
                let mut tx=self.tx.borrow_mut();
                let mut rows=tx.query_raw("select id,group_name from releases where original_name_key=$1 and original_name=$2 and id<>$3
                    and octet_length(group_name)<=$4 and(display_name is not null or(not is_obfuscated and(length(name)<16 or name collate \"C\" ~ '[^A-Za-z0-9]'))) order by id",[&key.as_slice() as &(dyn postgres::types::ToSql+Sync),&name.as_bytes(),&inputs.release.id,&(crate::filters::MAX_GROUP_NAME as i32)])?;
                let mut rejected=0usize;
                while let Some(row)=rows.next()? {
                    self.check_deadline()?;let group:String=row.try_get(1)?;
                    ensure!(group.len()<=crate::filters::MAX_GROUP_NAME,"native target group exceeds admission");
                    if crate::filters::group_blocked(&group){rejected+=1;ensure!(rejected<=crate::search::Bounds::DEFAULT.rejects,"native target visibility scan exceeds admission");continue;}
                    ids.push(row.try_get(0)?);if ids.len()==5{break;}
                }
            }
            result.push((name,self.rows(&ids)?));
        }Ok(result)
    }
    fn metadata_sidecar_match(&self,own:i64,kind:SidecarMatch,value:&str,found:&mut Vec<Found>,seen:&mut HashSet<i64>)->Result<()> {
        if found.len()==5||value.is_empty(){return Ok(());}
        self.admit_owned((value.len() as u64).checked_mul(8).and_then(|n|n.checked_add(8192)).context("native sidecar query extent overflow")?)?;
        let (condition,value,by)=match kind {
            SidecarMatch::Uid=>("segment_uid=$1",value.to_ascii_lowercase(),"segment_uid"),
            // Exact ASCII folding matches SQLite NOCASE regardless of the
            // server locale. Its expression index belongs to Stage schema11.
            SidecarMatch::Name=>("translate(release_name,'ABCDEFGHIJKLMNOPQRSTUVWXYZ','abcdefghijklmnopqrstuvwxyz') collate \"C\"=translate($1,'ABCDEFGHIJKLMNOPQRSTUVWXYZ','abcdefghijklmnopqrstuvwxyz') collate \"C\"",value.to_owned(),"name"),
        };
        let mut selected=Vec::new();let mut bytes=4096u64;
        self.before_query()?;
        {
            let mut tx=self.tx.borrow_mut();
            let mut rows=tx.query_raw(&format!("select release_id,group_name,octet_length(name)::bigint+octet_length(group_name)::bigint+coalesce(octet_length(file),0)::bigint+octet_length(kind)::bigint+coalesce(octet_length(text),0)::bigint
                from sidecars where {condition} and release_id<>$2 and octet_length(group_name)<=$3 order by at desc,release_id desc"),[&value as &(dyn postgres::types::ToSql+Sync),&own,&(crate::filters::MAX_GROUP_NAME as i32)])?;
            let mut rejected=0usize;
            while let Some(row)=rows.next()? {
                self.check_deadline()?;let id:i64=row.try_get(0)?;if seen.contains(&id){continue;}
                let group:String=row.try_get(1)?;ensure!(group.len()<=crate::filters::MAX_GROUP_NAME,"native sidecar group exceeds admission");
                if crate::filters::group_blocked(&group){rejected+=1;ensure!(rejected<=crate::search::Bounds::DEFAULT.rejects,"native sidecar visibility scan exceeds admission");continue;}
                let extent:i64=row.try_get(2)?;ensure!(extent>=0,"negative native sidecar extent");
                bytes=bytes.checked_add((extent as u64).checked_mul(8).and_then(|n|n.checked_add(1024)).context("native sidecar extent overflow")?).context("native sidecar extent overflow")?;
                seen.insert(id);selected.push(id);if found.len()+selected.len()==5{break;}
            }
        }
        self.admit_owned(bytes)?;if selected.is_empty(){return Ok(());}
        self.before_query()?;
        let rows=self.tx.borrow_mut().query("select release_id,name,group_name,file,kind,text from sidecars where release_id=any($1)",&[&selected])?;
        ensure!(rows.len()==selected.len(),"native sidecar inventory changed");
        let mut rows=rows.into_iter().map(|row|->Result<_>{Ok((row.try_get::<_,i64>(0)?,Found {name:row.try_get(1)?,group:row.try_get(2)?,file:row.try_get(3)?,kind:row.try_get(4)?,text:row.try_get(5)?,by}))}).collect::<Result<std::collections::HashMap<_,_>>>()?;
        for id in selected {found.push(rows.remove(&id).context("native sidecar identity changed")?);}Ok(())
    }
    pub(super) fn metadata_sidecars(&self,inputs:&MetaInputs<'_>)->Result<Vec<Found>> {
        let mut found=Vec::new();let mut seen=HashSet::new();
        if let Some(uid)=inputs.probe.and_then(|probe|probe.media.as_ref()).and_then(|media|media.segment_uid.as_deref()) {
            self.metadata_sidecar_match(inputs.release.id,SidecarMatch::Uid,uid,&mut found,&mut seen)?;
        }
        self.metadata_sidecar_match(inputs.release.id,SidecarMatch::Name,&inputs.release.name,&mut found,&mut seen)?;
        if let Some(display)=inputs.names.and_then(|names|names.display.as_deref()) {
            self.metadata_sidecar_match(inputs.release.id,SidecarMatch::Name,display,&mut found,&mut seen)?;
        }Ok(found)
    }
}
