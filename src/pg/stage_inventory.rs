//! Checked local result inventories. Missing rows and failed reads stay distinct.

use anyhow::{Result,ensure};
use super::StepReads;

fn admit(row:&postgres::Row)->Result<()> {
    ensure!(row.try_get::<_,i64>(0)?<=10000 && row.try_get::<_,i64>(1)?<=64*1024*1024,"native local inventory exceeds read admission");Ok(())
}
fn sixteen(row:&postgres::Row,at:usize)->Result<[u8;16]> {
    row.try_get::<_,Vec<u8>>(at)?.try_into().map_err(|_|anyhow::anyhow!("native PAR2 descriptor extent is invalid"))
}
impl StepReads {
    pub(crate) fn inner_junk(&self,id:i64,has_video:bool)->Result<Option<&'static str>> {
        self.read(|tx| {
            tx.query_one("select set_config('statement_timeout','10000',true)",&[])?;
            for relation in ["archive_inner_files","par2_files"] {
                admit(&tx.query_one(&format!("select count(*),coalesce(sum(octet_length(name)),0)::bigint from {relation} where release_id=$1"),&[&id])?)?;
                for row in tx.query(&format!("select name from {relation} where release_id=$1"),&[&id])? {
                    if self.owner.stop.load(std::sync::atomic::Ordering::Acquire) {return Err(crate::pg::outage::AttemptEnded.into());}
                    if crate::junk::is_executable(&row.try_get::<_,String>(0)?) {return Ok(Some(crate::junk::EXECUTABLE_INSIDE))}
                }
            }
            let category=tx.query_opt("select category from release_category where release_id=$1",&[&id])?.map(|row|row.try_get::<_,i64>(0)).transpose()?;
            Ok((!has_video && category.is_some_and(crate::junk::is_pc)).then_some(crate::junk::PC))
        })
    }
    pub(crate) fn stale_pc(&self,id:i64,has_video:bool)->Result<bool> {
        if !has_video {return Ok(false)}
        self.read(|tx|Ok(tx.query_opt("select category from release_category where release_id=$1",&[&id])?.map(|row|row.try_get::<_,i64>(0)).transpose()?.is_some_and(crate::junk::is_pc)))
    }
    pub(crate) fn pending_picture_request(&self,id:i64)->Result<bool> {
        self.read(|tx|Ok(tx.query_one("select exists(select 1 from picture_requests where release_id=$1 and done_at is null)",&[&id])?.try_get(0)?))
    }
    pub(crate) fn par2_inventory(&self,id:i64)->Result<Option<crate::par2::Inventory>> {
        self.read(|tx| {
            admit(&tx.query_one("select count(*),coalesce(sum(octet_length(name)::bigint+coalesce(octet_length(creator),0)::bigint+octet_length(file_id)+octet_length(set_id)+octet_length(md5)+octet_length(md5_16k)),0)::bigint from par2_files where release_id=$1 and complete<>0",&[&id])?)?;
            let mut inventory=crate::par2::Inventory {complete:true,..Default::default()};
            for row in tx.query("select file_id,set_id,md5,md5_16k,size,name,crc32,sealed,bound,creator from par2_files where release_id=$1 and complete<>0 order by name",&[&id])? {
                let file=crate::par2::Par2File {file_id:sixteen(&row,0)?,set_id:sixteen(&row,1)?,md5:sixteen(&row,2)?,md5_16k:sixteen(&row,3)?,size:u64::try_from(row.try_get::<_,i64>(4)?)?,name:row.try_get(5)?,crc32:row.try_get::<_,Option<i64>>(6)?.and_then(|crc|u32::try_from(crc).ok()),bound:row.try_get::<_,i64>(8)?!=0};
                if row.try_get::<_,i64>(7)?!=0 {inventory.sealed.push((file.set_id,file.file_id));}
                if let Some(creator)=row.try_get::<_,Option<String>>(9)? && inventory.creator_of(&file.set_id).is_none() {inventory.creators.push((file.set_id,creator));}
                inventory.files.push(file);
            }
            Ok((!inventory.files.is_empty()).then_some(inventory))
        })
    }
    pub(crate) fn kept_stills(&self,id:i64)->Result<Vec<(String,bool)>> {
        self.read(|tx| {
            admit(&tx.query_one("select count(*),coalesce(sum(octet_length(sha256)),0)::bigint from release_stills where release_id=$1",&[&id])?)?;
            tx.query("select sha256,phash is not null from release_stills where release_id=$1",&[&id])?.into_iter().map(|row|Ok((row.try_get(0)?,row.try_get(1)?))).collect()
        })
    }
    pub(crate) fn image_requests(&self,id:i64)->Result<Vec<String>> {
        self.read(|tx| {
            admit(&tx.query_one("select count(*),coalesce(sum(octet_length(file)),0)::bigint from picture_requests where release_id=$1 and (done_at is null or outcome='shown')",&[&id])?)?;
            tx.query("select file from picture_requests where release_id=$1 and (done_at is null or outcome='shown')",&[&id])?.into_iter().map(|row|Ok(row.try_get(0)?)).collect()
        })
    }
    pub(crate) fn kept_images(&self,id:i64)->Result<std::collections::HashMap<String,String>> {
        self.read(|tx| {
            admit(&tx.query_one("select count(*),coalesce(sum(octet_length(file)::bigint+octet_length(sha256)),0)::bigint from release_images where release_id=$1",&[&id])?)?;
            tx.query("select file,sha256 from release_images where release_id=$1",&[&id])?.into_iter().map(|row|Ok((row.try_get(0)?,row.try_get(1)?))).collect()
        })
    }
    pub(crate) fn beside_par2_files(&self,id:i64)->Result<std::collections::HashSet<Vec<u8>>> {
        self.read(|tx| {
            admit(&tx.query_one("select count(*),coalesce(sum(octet_length(par2_file)),0)::bigint from beside_files where release_id=$1",&[&id])?)?;
            tx.query("select par2_file from beside_files where release_id=$1",&[&id])?.into_iter().map(|row|Ok(row.try_get::<_,Option<Vec<u8>>>(0)?)).collect::<Result<Vec<_>>>().map(|rows|rows.into_iter().flatten().collect())
        })
    }
    pub(crate) fn upload_members(&self,id:i64)->Result<Vec<(i64,Option<Vec<u8>>)>> {
        self.read(|tx| {
            admit(&tx.query_one("select count(*),coalesce(sum(octet_length(par2_file)),0)::bigint from upload_link where lead_id=$1",&[&id])?)?;
            let rows=tx.query("select release_id,par2_file from upload_link where lead_id=$1",&[&id])?;
            let mut ids=rows.iter().map(|row|row.try_get(0)).collect::<std::result::Result<Vec<i64>,_>>()?;
            if !ids.is_empty() {ids.push(id);self.observe_in(tx,&ids)?;}
            rows.into_iter().map(|row|Ok((row.try_get(0)?,row.try_get(1)?))).collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::StepReads;
    #[test]
    fn native_local_inventories_preserve_complete_par2_and_requested_image_states() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let runtime=crate::pg::stage_queue_tests::runtime(&schema);crate::pg::stage_queue_tests::push(&runtime);
        let id=runtime.owner.with(|client| {
            let id:i64=client.query_one("select id from releases",&[])?.get(0);
            client.execute("insert into par2_files(release_id,file_id,set_id,md5,md5_16k,size,name,crc32,bound,sealed,complete,creator) values($1,$2,$3,$4,$5,42,'first.bin',4294967295,1,1,1,'Fixture')",&[&id,&vec![1u8;16],&vec![2u8;16],&vec![3u8;16],&vec![4u8;16]])?;
            client.execute("insert into par2_files(release_id,file_id,set_id,md5,md5_16k,size,name,complete) values($1,$2,$3,$4,$5,17,'unfinished.bin',0)",&[&id,&vec![5u8;16],&vec![2u8;16],&vec![3u8;16],&vec![4u8;16]])?;
            client.execute("insert into picture_requests(release_id,file,requested_at,done_at,outcome) values($1,'pending.jpg',1,null,null),($1,'shown.jpg',1,2,'shown'),($1,'failed.jpg',1,2,'failed')",&[&id])?;
            client.execute("insert into release_images(release_id,position,file,sha256,format,width,height,bytes) values($1,0,'shown.jpg',$2,'jpeg',8,9,17)",&[&id,&"00".repeat(32)])?;
            client.execute("insert into release_stills(release_id,position,sha256,width,height,phash) values($1,25,$2,8,9,null),($1,50,$2,8,9,-1)",&[&id,&"00".repeat(32)])?;
            Ok(id)
        }).unwrap();
        let reads=StepReads::for_owner(&runtime.owner).unwrap();let data=crate::postprocess::data::StepData::Native(&reads);
        let inventory=data.par2_inventory(id).unwrap().unwrap();assert!(inventory.complete);assert_eq!(inventory.files.len(),1);
        assert_eq!(inventory.files[0].crc32,Some(u32::MAX));assert_eq!(inventory.files[0].size,42);assert!(inventory.files[0].bound);
        assert_eq!(inventory.sealed,vec![([2;16],[1;16])]);assert_eq!(inventory.creators,vec![([2;16],"Fixture".into())]);
        let mut requests=data.image_requests(id).unwrap();requests.sort();assert_eq!(requests,vec!["pending.jpg","shown.jpg"]);
        assert_eq!(data.kept_images(id).unwrap().get("shown.jpg"),Some(&"00".repeat(32)));
        let mut stills=data.kept_stills(id).unwrap();stills.sort();assert_eq!(stills,vec![("00".repeat(32),false),("00".repeat(32),true)]);
        assert!(data.par2_inventory(id+1).unwrap().is_none());assert!(data.kept_images(id+1).unwrap().is_empty());
        runtime.owner.close();assert!(matches!(data.par2_inventory(id),Err(crate::postprocess::step::StepError::Native(_))));
    }
}
