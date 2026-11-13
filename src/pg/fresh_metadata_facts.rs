//! Full own-post metadata. Relationship lookups are deliberately a separate
//! reader: a matching public header or hash does not authorize payload lending.
use super::fresh_reader::ReadView;
use anyhow::{Result,Context,ensure};
use crate::{web_meta::{MetaFacts,MetaInputs,FileTiming,PredbFacts},store::SegmentRef};

impl ReadView<'_> {
    pub(crate) fn own_metadata(&self,inputs:&MetaInputs<'_>)->Result<MetaFacts> {
        let id=inputs.release.id;let files=self.metadata_files(id)?;
        let main=crate::probe::step::main_file(&files).cloned();
        let first_id=main.as_ref().map(|file|self.metadata_first_id(id,file)).transpose()?.flatten();
        let last=if let Some(main)=&main {
            self.before_query()?;
            let extent=self.tx.borrow_mut().query_opt("select octet_length(message_id)::bigint from segments where release_id=$1 and file_id=$2 order by coalesce(part,0) desc,message_id desc limit 1",&[&id,&main.id])?;
            if let Some(extent)=extent {
                let length:i64=extent.try_get(0)?;ensure!(length>=0,"negative native last ID extent");
                self.admit_owned((length as u64).checked_mul(4).and_then(|n|n.checked_add(256)).context("native last ID extent overflow")?)?;
                self.before_query()?;
                let row=self.tx.borrow_mut().query_one("select message_id,part,bytes from segments where release_id=$1 and file_id=$2 order by coalesce(part,0) desc,message_id desc limit 1",&[&id,&main.id])?;
                let bytes:Vec<u8>=row.try_get(0)?;ensure!(bytes.len() as i64==length,"native last ID extent changed");
                Some(SegmentRef {message_id:String::from_utf8(bytes).context("native last ID is not UTF-8")?,part:row.try_get(1)?,bytes:row.try_get(2)?})
            }else{None}
        }else{None};
        self.before_query()?;
        let extent=self.tx.borrow_mut().query_one("select
            coalesce((select coalesce(octet_length(posted),0)::bigint+coalesce(octet_length(content),0)::bigint from release_fingerprint where release_id=$1),0),
            coalesce((select octet_length(category)::bigint from predb_match where release_id=$1),0),
            coalesce((select sum(coalesce(octet_length(hw_host),0)::bigint+coalesce(octet_length(hw_key),0)::bigint)::bigint
                from(select t.hw_host,t.hw_key from file_timing t join files f on f.id=t.file_id where f.release_id=$1 order by f.filename collate \"C\",f.id limit 5000) timing),0),
            coalesce((select sum(octet_length(creator)::bigint)::bigint from(select distinct creator collate \"C\" as creator from par2_files where release_id=$1 and creator is not null order by 1 limit 4) creators),0),
            (select count(*) from(select 1 from file_timing t join files f on f.id=t.file_id where f.release_id=$1 limit 5000) timing_rows)",&[&id])?;
        let timing_rows:i64=extent.try_get(4)?;ensure!((0..=5000).contains(&timing_rows),"invalid native metadata row horizon");
        let mut bytes=timing_rows as u64*512+4096;
        for at in 0usize..4 {let n:i64=extent.try_get(at)?;ensure!(n>=0,"negative native metadata extent");bytes=bytes.checked_add((n as u64).checked_mul(4).context("native metadata extent overflow")?).context("native metadata extent overflow")?;}
        self.admit_owned(bytes)?;
        self.before_query()?;
        let fingerprint=self.tx.borrow_mut().query_opt("select posted,content from release_fingerprint where release_id=$1",&[&id])?;
        let (posted_fp,content_fp)=match fingerprint {Some(row)=>(row.try_get(0)?,row.try_get(1)?),None=>(None,None)};
        self.before_query()?;
        let predb=self.tx.borrow_mut().query_opt("select kind,category,pre_at,nuked from predb_match where release_id=$1",&[&id])?.map(|row|->Result<PredbFacts>{
            Ok(PredbFacts {kind:row.try_get(0)?,category:row.try_get(1)?,pre_at:row.try_get(2)?,nuked:row.try_get::<_,i64>(3)?!=0})
        }).transpose()?;
        self.before_query()?;
        let timing=self.tx.borrow_mut().query("select t.first_date,t.last_date,t.number_first,t.number_last,t.lines,t.references_seen,t.xref_groups_max,
            t.from_variants,t.encoded_subject,t.late_articles,t.articles,t.from_hash,t.tool,t.clock_first,t.clock_last,t.hw_host,t.hw_key,t.hw_n_first,t.hw_n_last
            from file_timing t join files f on f.id=t.file_id where f.release_id=$1 order by f.filename collate \"C\",f.id limit 5000",&[&id])?.into_iter().map(|row|->Result<FileTiming>{
                Ok(FileTiming {first_date:row.try_get(0)?,last_date:row.try_get(1)?,number_first:row.try_get(2)?,number_last:row.try_get(3)?,lines:row.try_get(4)?,references_seen:row.try_get(5)?,xref_groups_max:row.try_get(6)?,
                    from_variants:row.try_get(7)?,encoded_subject:row.try_get::<_,i64>(8)?!=0,late_articles:row.try_get(9)?,articles:row.try_get(10)?,from_hash:row.try_get(11)?,tool:row.try_get(12)?,clock_first:row.try_get(13)?,clock_last:row.try_get(14)?,hw_host:row.try_get(15)?,hw_key:row.try_get(16)?,hw_n_first:row.try_get(17)?,hw_n_last:row.try_get(18)?})
            }).collect::<Result<Vec<_>>>()?;
        ensure!(timing.len()==timing_rows as usize,"native timing inventory changed within snapshot");
        self.before_query()?;
        let par2_creators=self.tx.borrow_mut().query("select distinct creator collate \"C\" as creator from par2_files where release_id=$1 and creator is not null order by 1 limit 4",&[&id])?.into_iter().map(|row|row.try_get(0)).collect::<std::result::Result<Vec<String>,_>>()?;
        self.before_query()?;
        let rev_volumes=self.tx.borrow_mut().query_one("select count(*) from rev_volumes where release_id=$1",&[&id])?.try_get(0)?;
        Ok(MetaFacts {files,main,first_id,last,posted_fp,content_fp,predb,salt:self.own_key_salt()?,
            // The old shard-specific IdKey/collection recomputation has no
            // meaning for fresh native public IDs. Never reuse its old secret.
            id_key:None,collection:None,timing,par2_creators,rev_volumes,
            // The page explicitly reports these separate readers unavailable.
            // No empty relationship result is certified or used for lending.
            same:Vec::new(),points_to:Vec::new(),sidecars:Vec::new()})
    }
}
