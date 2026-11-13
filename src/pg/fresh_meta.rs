//! Native metadata horizons keep the original 5000 files/200 shown rows and
//! 250000-article partial-walk semantics; actual facts come from the same RR.
use super::fresh_reader::ReadView;
use crate::{store::FileInfo,web_meta::{PartFacts,PartsFacts}};
use anyhow::{Result,Context,ensure};
use std::collections::HashMap;

impl ReadView<'_> {
    pub(crate) fn metadata_files(&self,id:i64)->Result<Vec<FileInfo>> {
        self.before_query()?;
        let extents=self.tx.borrow_mut().query("select id,octet_length(filename)::bigint,coalesce(octet_length(subject),0)::bigint
            from files where release_id=$1 order by id limit 5000",&[&id])?;
        let bytes=extents.iter().try_fold(4096u64,|sum,row|->Result<u64>{
            let name:i64=row.try_get(1)?;let subject:i64=row.try_get(2)?;ensure!(name>=0&&subject>=0,"negative native metadata extent");
            Ok(sum.checked_add((name as u64).checked_add(subject as u64).and_then(|n|n.checked_mul(4)).and_then(|n|n.checked_add(400)).context("native file extent overflow")?).context("native files extent overflow")?)
        })?;
        self.admit_owned(bytes)?;self.before_query()?;
        let rows=self.tx.borrow_mut().query("select id,filename,subject,subject_part,expected,file_total,distinct_parts,touched_at
            from files where release_id=$1 order by id limit 5000",&[&id])?;
        ensure!(rows.len()==extents.len(),"native metadata files changed within snapshot");
        rows.into_iter().zip(extents).map(|(row,extent)|->Result<FileInfo>{
            let file=FileInfo {id:row.try_get(0)?,filename:row.try_get(1)?,subject:row.try_get(2)?,subject_part:row.try_get(3)?,expected:row.try_get(4)?,file_total:row.try_get(5)?,parts_seen:row.try_get(6)?,touched_at:row.try_get(7)?,sealed:false};
            ensure!(file.id==extent.try_get::<_,i64>(0)?&&file.filename.len() as i64==extent.try_get::<_,i64>(1)?&&file.subject.as_ref().map_or(0,String::len) as i64==extent.try_get::<_,i64>(2)?,"native metadata file extent changed");Ok(file)
        }).collect()
    }
    fn metadata_id(&self,id:i64,file:i64,last:bool)->Result<Option<String>> {
        self.before_query()?;
        let order=if last{"coalesce(part,0) desc,message_id desc"}else{"coalesce(part,0),message_id"};
        // Identifiers/order are selected from closed literals, not request SQL.
        let extent=self.tx.borrow_mut().query_opt(&format!("select octet_length(message_id)::bigint from segments
            where release_id=$1 and file_id=$2 order by {order} limit 1"),&[&id,&file])?;
        let Some(extent)=extent else{return Ok(None)};let length:i64=extent.try_get(0)?;
        ensure!(length>=0,"negative native metadata ID extent");self.admit_owned((length as u64).checked_mul(4).and_then(|n|n.checked_add(256)).context("native metadata ID overflow")?)?;
        self.before_query()?;
        let bytes:Vec<u8>=self.tx.borrow_mut().query_one(&format!("select message_id from segments where release_id=$1 and file_id=$2 order by {order} limit 1"),&[&id,&file])?.try_get(0)?;
        ensure!(bytes.len() as i64==length,"native metadata ID extent changed");Ok(Some(String::from_utf8(bytes).context("native metadata ID is not UTF-8")?))
    }
    pub(super) fn metadata_first_id(&self,id:i64,main:&FileInfo)->Result<Option<String>> {
        self.before_query()?;
        let extent:Option<i64>=self.tx.borrow_mut().query_one("select octet_length(subject_mid)::bigint from files where id=$1 and release_id=$2",&[&main.id,&id])?.try_get(0)?;
        if let Some(length)=extent {
            ensure!(length>=0,"negative subject ID extent");self.admit_owned((length as u64).checked_mul(4).and_then(|n|n.checked_add(256)).context("native subject ID overflow")?)?;
            self.before_query()?;
            let bytes:Vec<u8>=self.tx.borrow_mut().query_one("select subject_mid from files where id=$1 and release_id=$2",&[&main.id,&id])?.try_get(0)?;
            ensure!(bytes.len() as i64==length,"native subject ID extent changed");Ok(Some(String::from_utf8(bytes).context("native subject ID is not UTF-8")?))
        } else {self.metadata_id(id,main.id,false)}
    }
    pub(crate) fn metadata_headers(&self,id:i64)->Result<Option<String>> {
        if !self.visible(id)?{return Ok(None);}
        let files=self.metadata_files(id)?;let main=crate::probe::step::main_file(&files);
        let first=main.map(|main|self.metadata_first_id(id,main)).transpose()?.flatten();
        self.before_query()?;
        let salt=self.tx.borrow_mut().query_opt("select value from meta where key='key_salt'",&[])?.map(|row|row.try_get::<_,i64>(0)).transpose()?;
        let probe=self.probe(id)?;
        Ok(Some(crate::web_meta::headers_from_fields(first.as_deref(),main.and_then(|main|main.subject.as_deref()),salt,probe.as_ref().map(|probe|&probe.headers))))
    }
    pub(crate) fn metadata_parts(&self,id:i64)->Result<Option<PartsFacts>> {
        if !self.visible(id)?{return Ok(None);}
        let files=self.metadata_files(id)?;
        self.before_query()?;
        let count:i64=self.tx.borrow_mut().query_one("select count(*) from(select 1 from segments where release_id=$1 limit 250001) horizon",&[&id])?.try_get(0)?;
        ensure!((0..=250001).contains(&count),"invalid native parts horizon");let complete=count<=250000;
        let shown:Vec<i64>=files.iter().take(200).map(|file|file.id).collect();
        let mut maps=HashMap::new();let mut numbers=HashMap::new();
        if complete && !shown.is_empty() {
            self.before_query()?;
            let rows=self.tx.borrow_mut().query("with cells as (
                select f.id,c.cell, greatest(coalesce(f.expected,f.distinct_parts),f.distinct_parts) expected
                from files f cross join generate_series(0,63) c(cell) where f.id=any($1)
            ), ranges as (select id,cell,
                floor(cell::numeric*expected/64)::bigint+1 lo,
                greatest(floor((cell+1)::numeric*expected/64)::bigint,floor(cell::numeric*expected/64)::bigint+1) hi,expected from cells),
            covered as(select r.id,r.cell,r.expected,count(distinct s.part)=r.hi-r.lo+1 whole from ranges r
                left join segments s on s.file_id=r.id and s.part>=r.lo and s.part<=r.hi group by r.id,r.cell,r.expected,r.lo,r.hi)
            select id,bit_or(case when whole and expected>0 then 1::bigint<<cell else 0::bigint end) from covered group by id",&[&shown])?;
            for row in rows{maps.insert(row.try_get::<_,i64>(0)?,row.try_get::<_,i64>(1)? as u64);}
            self.before_query()?;
            let rows=self.tx.borrow_mut().query("select file_id,min(bytes),max(bytes),
                (select count(*) from(select coalesce(p.part,0) from segments p where p.file_id=s.file_id group by coalesce(p.part,0) having count(*)>1) duplicates)
                from segments s where file_id=any($1) group by file_id",&[&shown])?;
            for row in rows{numbers.insert(row.try_get::<_,i64>(0)?,(row.try_get::<_,Option<i64>>(1)?,row.try_get::<_,Option<i64>>(2)?,usize::try_from(row.try_get::<_,i64>(3)?)?));}
        }
        let mut rows=Vec::with_capacity(shown.len());
        for file in files.iter().take(200) {
            let (first,last)=if complete{(self.metadata_id(id,file.id,false)?,self.metadata_id(id,file.id,true)?)}else{(None,None)};
            let (smallest,largest,reposted)=numbers.remove(&file.id).unwrap_or((None,None,0));
            rows.push(PartFacts {subject:file.subject.as_deref().unwrap_or(&file.filename).to_owned(),expected:file.expected.unwrap_or(file.parts_seen).max(file.parts_seen),seen:file.parts_seen,
                map:maps.remove(&file.id).unwrap_or(0),first,last,smallest,largest,reposted});
        }
        Ok(Some(PartsFacts {rows,files:files.len(),complete}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{pg::{self,fresh_reader::Budget},parser::{Article,Release},nzb_spool::{Pool,Limits}};
    use std::{time::{Duration,Instant}};
    #[test]
    #[ignore="requires explicit isolated schema9 and bounded native metadata admission"]
    fn real_native_metadata_keeps_header_privacy_part_copies_and_exact_hash_bytes() {
        let schema=pg::testing::Schema::new().expect("explicit isolated PG schema9");let mut client=schema.pool.get().unwrap();
        let articles=["<b@example.test>","<a@example.test>"].into_iter().map(|mid|Article {filename:Some("sample.bin".into()),subject:"sample.bin yEnc (1/1)".into(),
            message_id:mid.into(),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}).collect();
        pg::release_writer::save(&mut client,&[Release {name:"Metadata.Example.Release".into(),group:"alt.binaries.test".into(),poster:"poster".into(),date:"2020-01-01".into(),articles,..Default::default()}],1).unwrap();
        let id:i64=client.query_one("select id from releases",&[]).unwrap().get(0);
        let mut probe=crate::probe::ProbeInfo::default();probe.headers.insert("NNTP-Posting-Host".into(),"192.0.2.1".into());
        probe.headers.insert("Injection-Info".into(),"posting-account=\"private-account\"; logging-data=\"secret\"".into());
        probe.headers.insert("X-Ordinary".into(),"<kept & escaped>".into());
        client.execute("insert into release_probe(release_id,info) values($1,$2)",&[&id,&serde_json::to_string(&probe).unwrap()]).unwrap();
        client.execute("insert into meta(key,value) values('key_salt',7)",&[]).unwrap();
        let root=tempfile::tempdir().unwrap();
        #[cfg(unix)]{use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(root.path(),std::fs::Permissions::from_mode(0o700)).unwrap();}
        let pool=Pool::new(root.path().into(),Limits {requests:3,per_request_bytes:1<<20,total_bytes:3<<20,free_bytes:0}).unwrap();
        let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
        let headers=view.metadata_headers(id).unwrap().unwrap();
        assert!(headers.contains("&lt;a@example.test&gt;"));assert!(headers.contains("sample.bin yEnc (1/1)"));
        assert!(headers.contains("&lt;kept &amp; escaped&gt;"));
        assert!(!headers.contains("192.0.2.1"));assert!(!headers.contains("private-account"));assert!(!headers.contains("secret"));
        let parts=view.metadata_parts(id).unwrap().unwrap();assert!(parts.complete);assert_eq!(parts.files,1);assert_eq!(parts.rows.len(),1);
        let row=&parts.rows[0];assert_eq!(row.map,u64::MAX);assert_eq!(row.expected,1);assert_eq!(row.seen,1);assert_eq!(row.reposted,1);
        assert_eq!(row.first.as_deref(),Some("<a@example.test>"));assert_eq!(row.last.as_deref(),Some("<b@example.test>"));
        assert_eq!((row.smallest,row.largest),(Some(17),Some(17)));
        let (hashes,kept)=view.metadata_hashes(id,&pool).unwrap().unwrap();assert!(hashes.complete);assert_eq!(hashes.articles,2);assert_eq!(kept.unwrap(),probe);
        let hex=hashes.digest.unwrap().iter().map(|n|format!("{n:02x}")).collect::<String>();
        assert_eq!(hex,"cfae56924aeff5c0977631862021f71142f6487df4d99a46c374e7f0ae078a35");
        assert!(view.metadata_headers(id+1).unwrap().is_none());assert!(view.metadata_parts(id+1).unwrap().is_none());
    }
}
