//! Same-key candidates are display facts only. None of these keys authorizes
//! merging, lending payload, hiding a release, or declaring a verified twin.
use super::fresh_reader::ReadView;
use crate::{keys::{self,Kind,FileCrc},search::ReleaseRow};
use anyhow::{Result,Context,ensure};
use postgres::{types::ToSql,fallible_iterator::FallibleIterator};

pub(crate) struct SameKey {pub release:ReleaseRow,pub has:bool,pub rows:Vec<ReleaseRow>}
impl ReadView<'_> {
    fn key_candidates(&self,own:i64,source:&str,values:&[&(dyn ToSql+Sync)],limit:usize)->Result<Vec<i64>> {
        ensure!(limit<=200,"native key candidate horizon exceeded");
        self.before_query()?;let mut tx=self.tx.borrow_mut();
        let sql=format!("select distinct r.id,r.group_name from ({source}) candidates join releases r on r.id=candidates.release_id
            where r.id<>$1 and octet_length(r.group_name)<={} and (r.display_name is not null or(not r.is_obfuscated and(length(r.name)<16 or r.name collate \"C\" ~ '[^A-Za-z0-9]')))
            order by r.id desc",crate::filters::MAX_GROUP_NAME);
        // Existing callers supply zero, one or three already-admitted values.
        let mut params=[&own as &(dyn ToSql+Sync);4];
        ensure!(values.len()<params.len(),"native key parameter horizon exceeded");
        params[1..1+values.len()].copy_from_slice(values);
        let mut rows=tx.query_raw(&sql,params[..1+values.len()].iter().copied())?;
        let mut ids=Vec::new();
        while let Some(row)=rows.next()? {self.check_deadline()?;let group:String=row.try_get(1)?;
            ensure!(group.len()<=crate::filters::MAX_GROUP_NAME,"native related group exceeds admission");
            if !crate::filters::group_blocked(&group){ids.push(row.try_get(0)?);if ids.len()==limit{break;}}
        }Ok(ids)
    }
    fn derived_key(&self,id:i64,kind:Kind,parts:Option<i64>)->Result<Option<i64>> {
        self.before_query()?;self.admit_owned(5000*32+4096)?;
        let timing=self.tx.borrow_mut().query("select t.lines,t.articles from file_timing t join files f on f.id=t.file_id
            where f.release_id=$1 order by f.filename collate \"C\",f.id limit 5000",&[&id])?.into_iter()
            .map(|row|Ok((row.try_get(0)?,row.try_get(1)?))).collect::<Result<Vec<_>>>()?;
        Ok(keys::keys_of(self.probe(id)?.as_ref(),&timing,parts,self.own_key_salt()?).get(kind))
    }
    fn relation_file_crcs(&self,id:i64,inner:bool)->Result<Vec<FileCrc>> {
        self.admit_owned(64*1024)?;
        let mut files=if inner{Vec::new()}else{self.probe(id)?.as_ref().and_then(keys::probe_file).into_iter().collect()};
        if !inner {for file in self.folded_key_files(id)?{if !files.contains(&file){files.push(file);}}}
        self.before_query()?;
        let sql=if inner {"select distinct size,crc32 from archive_inner_files where release_id=$1 and source in(2,3,4) and crc32 is not null and size>=$2 order by size desc limit 8"}
            else {"select distinct size,crc32 from par2_files where release_id=$1 and bound=1 and crc32 is not null and size>=$2 order by size desc limit 8"};
        for row in self.tx.borrow_mut().query(sql,&[&id,&keys::CRC_MIN_SIZE])? {
            let crc=u32::try_from(row.try_get::<_,i64>(1)?).context("native file CRC is out of range")?;
            if let Some(file)=FileCrc::new(row.try_get(0)?,crc){if !files.contains(&file){files.push(file);}}
        }
        if !inner&&files.len()<8 {
            self.before_query()?;
            for row in self.tx.borrow_mut().query("select distinct size,crc32 from rev_volumes where release_id=$1 and size>=$2 order by size desc,crc32 limit 8",&[&id,&keys::CRC_MIN_SIZE])? {
                let crc=u32::try_from(row.try_get::<_,i64>(1)?).context("native recovery CRC is out of range")?;
                if let Some(file)=FileCrc::new(row.try_get(0)?,crc){if !files.contains(&file)&&files.len()<8{files.push(file);}}
            }
        }
        files.truncate(8);Ok(files)
    }
    fn folded_key_files(&self,id:i64)->Result<Vec<FileCrc>> {
        self.before_query()?;
        let files=self.tx.borrow_mut().query("select id,expected,distinct_parts from files where release_id=$1 order by filename collate \"C\",id limit 64",&[&id])?;
        let mut out=Vec::new();
        for file in files {
            let Some(n)=file.try_get::<_,Option<i64>>(1)?.filter(|n|(1..=4096).contains(n)) else{continue};
            if file.try_get::<_,i64>(2)?<n{continue;}
            self.before_query()?;let file_id:i64=file.try_get(0)?;
            let mut tx=self.tx.borrow_mut();
            let mut rows=tx.query_raw("select distinct on(s.part) s.part,c.decoded,c.crc32 from segments s join segment_crc c on c.release_id=s.release_id and c.message_id=s.message_id
                where s.release_id=$1 and s.file_id=$2 and s.part between 1 and $3 order by s.part,s.message_id",[&id,&file_id,&n])?;
            let (mut next,mut size,mut crc)=(1i64,0i64,0u32);
            while let Some(row)=rows.next()? {self.check_deadline()?;let part:i64=row.try_get(0)?;if part!=next{break;}
                let decoded:i64=row.try_get(1)?;let length=usize::try_from(decoded).context("negative native decoded length")?;
                let value=u32::try_from(row.try_get::<_,i64>(2)?).context("native segment CRC is out of range")?;
                crc=if next==1{value}else{crate::gf2::crc32_combine(crc,value,length)};
                size=size.checked_add(decoded).context("native folded file size overflow")?;next+=1;
            }
            if next==n+1 {if let Some(file)=FileCrc::new(size,crc){if !out.contains(&file){out.push(file);}}}
        }out.sort_by_key(|file|std::cmp::Reverse(file.size));Ok(out)
    }
    fn audio_key_prints(&self,id:i64)->Result<Vec<(i64,Vec<u32>)>> {
        self.before_query()?;
        let extents=self.tx.borrow_mut().query("select position,octet_length(items)::bigint from release_audio_print where release_id=$1 order by position limit 4",&[&id])?;
        ensure!(extents.len()<=3,"native audio print window horizon exceeded");
        let mut bytes=4096u64;
        for row in &extents {let n:i64=row.try_get(1)?;ensure!(n>=0&&n%4==0,"invalid native audio print extent");bytes=bytes.checked_add((n as u64).checked_mul(3).context("native audio print extent overflow")?).context("native audio print extent overflow")?;}
        self.admit_owned(bytes)?;self.before_query()?;
        let rows=self.tx.borrow_mut().query("select position,at_ms,items from release_audio_print where release_id=$1 order by position limit 4",&[&id])?;
        ensure!(rows.len()==extents.len(),"native audio print inventory changed");
        rows.into_iter().zip(extents).map(|(row,extent)|{
            let blob:Vec<u8>=row.try_get(2)?;ensure!(row.try_get::<_,i64>(0)?==extent.try_get::<_,i64>(0)?&&blob.len() as i64==extent.try_get::<_,i64>(1)?,"native audio print extent changed");
            Ok((row.try_get(1)?,crate::audio::from_blob(&blob)))
        }).collect()
    }
    pub(crate) fn same_key(&self,id:i64,kind:Kind)->Result<Option<SameKey>> {
        let Some(release)=self.release(id)? else{return Ok(None)};
        self.admit_owned(32768)?;
        let (has,ids)=if let Some(column)=kind.column() {
            self.before_query()?;
            let stored=self.tx.borrow_mut().query_opt(&format!("select {column} from release_keys where release_id=$1"),&[&id])?.map(|row|row.try_get::<_,Option<i64>>(0)).transpose()?.flatten();
            let key=match stored {Some(key)=>Some(key),None=>self.derived_key(id,kind,release.parts)?};
            let ids=match key {Some(key)=>self.key_candidates(id,&format!("select release_id from release_keys where {column}=$2"),&[&key],100)?,None=>Vec::new()};
            (key.is_some(),ids)
        }else if let Some(column)=kind.archive_column() {
            self.before_query()?;
            let key=self.tx.borrow_mut().query_opt(&format!("select {column} from archive_release where release_id=$1"),&[&id])?.map(|row|row.try_get::<_,Option<i64>>(0)).transpose()?.flatten();
            (key.is_some(),match key {Some(key)=>self.key_candidates(id,&format!("select release_id from archive_release where {column}=$2"),&[&key],100)?,None=>Vec::new()})
        }else{match kind {
            Kind::Par2=>{
                self.before_query()?;
                let extents=self.tx.borrow_mut().query("select octet_length(set_id)::bigint from(select distinct set_id from par2_files where release_id=$1 and bound=1 order by set_id limit 4) selected",&[&id])?;
                ensure!(extents.iter().all(|row|row.get::<_,i64>(0)==16),"invalid native PAR2 set key extent");
                self.before_query()?;
                let sets=self.tx.borrow_mut().query("select distinct set_id from par2_files where release_id=$1 and bound=1 order by set_id limit 4",&[&id])?.into_iter().map(|row|row.try_get::<_,Vec<u8>>(0)).collect::<std::result::Result<Vec<_>,_>>()?;
                ensure!(sets.iter().all(|set|set.len()==16),"invalid native PAR2 set key");
                (!sets.is_empty(),if sets.is_empty(){Vec::new()}else{self.key_candidates(id,"select release_id from par2_files where bound=1 and set_id=any($2)",&[&sets],100)?})
            }
            Kind::MovieHash=>{
                self.before_query()?;let extent=self.tx.borrow_mut().query_opt("select octet_length(moviehash)::bigint from release_probe where release_id=$1 and moviehash is not null",&[&id])?;
                let hash=if let Some(row)=extent{let n:i64=row.try_get(0)?;ensure!(n>=0,"negative native moviehash extent");self.admit_owned((n as u64).checked_mul(3).and_then(|n|n.checked_add(128)).context("native moviehash extent overflow")?)?;self.before_query()?;
                    Some(self.tx.borrow_mut().query_one("select moviehash from release_probe where release_id=$1",&[&id])?.try_get::<_,String>(0)?)}else{None};
                (hash.is_some(),match hash {Some(hash)=>self.key_candidates(id,"select release_id from release_probe where moviehash=$2",&[&hash],100)?,None=>Vec::new()})
            }
            Kind::FileCrc|Kind::InnerFile=>{
                let files=self.relation_file_crcs(id,kind==Kind::InnerFile)?;
                let keys=files.iter().map(|file|file.key()).collect::<Vec<_>>();let sizes=files.iter().map(|file|file.size).collect::<Vec<_>>();let crcs=files.iter().map(|file|i64::from(file.crc32)).collect::<Vec<_>>();
                let source="select release_id from release_keys where file_crc=any($2) union select p.release_id from par2_files p join unnest($3::bigint[],$4::bigint[]) f(size,crc32) using(size,crc32) where p.bound=1
                    union select v.release_id from rev_volumes v join unnest($3::bigint[],$4::bigint[]) f(size,crc32) using(size,crc32)
                    union select a.release_id from archive_inner_files a join unnest($3::bigint[],$4::bigint[]) f(size,crc32) using(size,crc32) where a.source in(2,3,4)";
                (!files.is_empty(),if files.is_empty(){Vec::new()}else{self.key_candidates(id,source,&[&keys,&sizes,&crcs],100)?})
            }
            Kind::AudioPrint=>{
                let mine=self.audio_key_prints(id)?;
                let candidates=if mine.is_empty(){Vec::new()}else{self.key_candidates(id,"select a.release_id from release_audio_anchor a where exists(select 1 from release_audio_anchor own where own.release_id=$1 and own.anchor=a.anchor)",&[],200)?};
                let mut ids=Vec::new();for candidate in candidates{if crate::probe::audio_print::prints_match(&mine,&self.audio_key_prints(candidate)?){ids.push(candidate);if ids.len()==100{break;}}}
                (!mine.is_empty(),ids)
            }
            _=>anyhow::bail!("native key kind has no source mapping"),
        }};
        let mut rows=Vec::new();for page in ids.chunks(self.budget.rows){rows.extend(self.rows(page)?);}
        Ok(Some(SameKey {release,has,rows}))
    }
}
