//! Native key inputs retain their original catalog salt and checked PG facts.

use anyhow::{Result,ensure};
use postgres::Transaction;
use crate::keys::{Keys,Lookup,FileCrc};
use super::StepReads;

impl StepReads {
    pub(crate) fn key_salt(&self,cached:Option<i64>)->Result<i64> {
        self.read(|tx| {
            let salt:i64=tx.query_one("select value from meta where key='key_salt'",&[])?.try_get(0)?;
            ensure!(cached.is_none_or(|old|old==salt),"native key salt changed within its Step instance");
            Ok(salt)
        })
    }
    pub(crate) fn stored_release_keys(&self,id:i64)->Result<Option<Keys>> {
        self.read(|tx|tx.query_opt("select account,host,layout,lines,file_crc,body_crc,video,audio,mux,encode,thread,subs,fonts,chapters,volumes from release_keys where release_id=$1",&[&id])?.map(|row|Ok(Keys {
            account:row.try_get(0)?,host:row.try_get(1)?,layout:row.try_get(2)?,lines:row.try_get(3)?,file_crc:row.try_get(4)?,body_crc:row.try_get(5)?,video:row.try_get(6)?,audio:row.try_get(7)?,mux:row.try_get(8)?,encode:row.try_get(9)?,thread:row.try_get(10)?,subs:row.try_get(11)?,fonts:row.try_get(12)?,chapters:row.try_get(13)?,volumes:row.try_get(14)?,
        })).transpose())
    }
    pub(crate) fn release_keys(&self,id:i64,parts:Option<i64>,salt:i64)->Result<Keys> {
        self.read(|tx| {
            let actual:i64=tx.query_one("select value from meta where key='key_salt'",&[])?.try_get(0)?;
            ensure!(actual==salt,"native key input belongs to a different salt");
            let (probe,_)=probe_in(tx,id)?;let mut keys=keys_in(tx,id,parts,Some(salt),probe.as_ref())?;
            if keys.file_crc.is_none() {keys.file_crc=folded_in(tx,id,&self.owner.stop)?.first().copied().map(FileCrc::key);}
            if probe.as_ref().and_then(|p|p.archive.as_ref()).is_some_and(|a|a.encrypted||a.headers_encrypted) {
                let metadata=tx.query_one("select count(*),coalesce(sum(octet_length(name)),0)::bigint from par2_files where release_id=$1 and bound<>0 and complete<>0",&[&id])?;
                ensure!(metadata.try_get::<_,i64>(0)?<=10000 && metadata.try_get::<_,i64>(1)?<=64*1024*1024,"native volume keys exceed read admission");
                let files=tx.query("select name,size from par2_files where release_id=$1 and bound<>0 and complete<>0",&[&id])?.into_iter().map(|row|Ok((row.try_get::<_,String>(0)?,row.try_get::<_,i64>(1)?))).collect::<Result<Vec<_>>>()?;
                keys.volumes=crate::keys::volumes_text(&files).map(|text|crate::keys::as_key(crate::keys::digest8(text.as_bytes())));
            }
            Ok(keys)
        })
    }
    pub(crate) fn release_lookup(&self,id:i64,parts:Option<i64>)->Result<Lookup> {
        self.read(|tx| {
            let (probe,moviehash)=probe_in(tx,id)?;
            let keys=keys_in(tx,id,parts,None,probe.as_ref())?;
            let par2_sets=tx.query("select distinct set_id from par2_files where release_id=$1 and bound<>0 order by set_id limit 4",&[&id])?.into_iter().map(|row| {
                let set:Vec<u8>=row.try_get(0)?;ensure!(set.len()==16,"native PAR2 set extent is invalid");Ok(set)
            }).collect::<Result<Vec<_>>>()?;
            let mut files:Vec<FileCrc>=probe.as_ref().and_then(crate::keys::probe_file).into_iter().collect();
            for file in folded_in(tx,id,&self.owner.stop)? {if !files.contains(&file) {files.push(file);}}
            for file in crc_rows(tx,"select distinct size,crc32 from par2_files where release_id=$1 and bound<>0 and crc32 is not null and size>=1048576 order by size desc limit 8",id)? {
                if !files.contains(&file) {files.push(file);}
            }
            let room=8usize.saturating_sub(files.len());
            if room>0 {
                for row in tx.query("select distinct size,crc32 from rev_volumes where release_id=$1 and size>=1048576 order by size desc,crc32 limit $2",&[&id,&((room+files.len()) as i64)])? {
                    if let Some(file)=file_crc(&row)? && !files.contains(&file) && files.len()<8 {files.push(file);}
                }
            }
            files.truncate(8);
            let inner_files=crc_rows(tx,"select distinct size,crc32 from archive_inner_files where release_id=$1 and source in (2,3,4) and crc32 is not null and size>=1048576 order by size desc limit 8",id)?;
            let archive=if let Some(row)=tx.query_opt("select case when octet_length(packer)<=65536 then packer end,octet_length(packer),twin_key,owner_key,zone_key from archive_release where release_id=$1",&[&id])? {
                ensure!(row.try_get::<_,Option<i32>>(1)?.is_none_or(|n|n<=65536),"native archive packer exceeds read admission");
                crate::archive::store::ArchiveKeys {packer:row.try_get(0)?,twin_key:row.try_get(2)?,owner_key:row.try_get(3)?,zone_key:row.try_get(4)?}
            } else {Default::default()};
            let audio_print:bool=tx.query_one("select exists(select 1 from release_audio_print where release_id=$1)",&[&id])?.try_get(0)?;
            Ok(Lookup {keys,par2_sets,moviehash,files,inner_files,archive,audio_print})
        })
    }
    pub(crate) fn stills_lookup(&self,id:i64)->Result<Lookup> {
        self.read(|tx| {
            let (probe,moviehash)=probe_in(tx,id)?;
            let mux=probe.as_ref().and_then(|p|p.media.as_ref()).and_then(crate::keys::mux_text).map(|text|crate::keys::as_key(crate::keys::digest8(text.as_bytes())));
            Ok(Lookup {keys:Keys {mux,..Default::default()},moviehash:moviehash.filter(|hash|!hash.is_empty()),..Default::default()})
        })
    }
}

fn probe_in(tx:&mut Transaction<'_>,id:i64)->Result<(Option<crate::probe::ProbeInfo>,Option<String>)> {
    tx.query_one("select set_config('statement_timeout','10000',true)",&[])?;
    let Some(row)=tx.query_opt("select case when octet_length(info)<=67108864 then info end,octet_length(info),case when octet_length(moviehash)<=65536 then moviehash end,octet_length(moviehash) from release_probe where release_id=$1",&[&id])? else {return Ok((None,None))};
    ensure!(row.try_get::<_,Option<i32>>(1)?.is_none_or(|n|n<=64*1024*1024) && row.try_get::<_,Option<i32>>(3)?.is_none_or(|n|n<=65536),"native key probe exceeds read admission");
    let info:Option<String>=row.try_get(0)?;
    // Native malformed/admission-refused JSON is an error, not missing facts.
    // Charge expanded ownership before serde constructs defaulted track arrays.
    let probe=info.map(|text|crate::pg::probe_decode::decode(text.as_bytes(),&mut (32u64<<20))).transpose()?;
    Ok((probe,row.try_get(2)?))
}
fn keys_in(tx:&mut Transaction<'_>,id:i64,parts:Option<i64>,salt:Option<i64>,probe:Option<&crate::probe::ProbeInfo>)->Result<Keys> {
    let timing=tx.query("select t.lines,t.articles from file_timing t join files f on f.id=t.file_id where f.release_id=$1 order by f.original_filename limit 5000",&[&id])?.into_iter().map(|row|Ok((row.try_get(0)?,row.try_get(1)?))).collect::<Result<Vec<_>>>()?;
    Ok(crate::keys::keys_of(probe,&timing,parts,salt))
}
fn file_crc(row:&postgres::Row)->Result<Option<FileCrc>> {
    let size:i64=row.try_get(0)?;let crc:i64=row.try_get(1)?;
    Ok(u32::try_from(crc).ok().and_then(|crc|FileCrc::new(size,crc)))
}
fn crc_rows(tx:&mut Transaction<'_>,sql:&str,id:i64)->Result<Vec<FileCrc>> {
    tx.query(sql,&[&id])?.into_iter().map(|row|file_crc(&row)).collect::<Result<Vec<_>>>().map(|rows|rows.into_iter().flatten().collect())
}
fn folded_in(tx:&mut Transaction<'_>,id:i64,stop:&std::sync::atomic::AtomicBool)->Result<Vec<FileCrc>> {
    let count:i64=tx.query_one("select count(*) from segment_crc where release_id=$1",&[&id])?.try_get(0)?;
    if count==0 {return Ok(Vec::new())}
    let mut out=Vec::new();
    for file in tx.query("select id,expected,distinct_parts from files where release_id=$1 order by id limit 64",&[&id])? {
        if stop.load(std::sync::atomic::Ordering::Acquire) {return Err(crate::pg::outage::AttemptEnded.into());}
        let file_id:i64=file.try_get(0)?;let expected:Option<i64>=file.try_get(1)?;let seen:i64=file.try_get(2)?;
        let Some(expected)=expected.filter(|n|(1..=4096).contains(n) && seen>=*n && count>=*n) else {continue};
        // SegmentPick::Parts keeps the first sixteen MIDs for each part. The
        // first of those with a recorded CRC wins, as in the legacy map fold.
        let rows=tx.query("select distinct on(s.part) s.part,c.decoded,c.crc32 from (select part,message_id,row_number() over(partition by part order by message_id) as n from segments where file_id=$1 and part between 1 and $2) s join segment_crc c on c.release_id=$3 and c.message_id=s.message_id where s.n<=16 order by s.part,s.message_id",&[&file_id,&expected,&id])?;
        if rows.len() as i64!=expected {continue}
        let (mut crc,mut size,mut valid)=(0u32,0i64,true);
        for (index,row) in rows.iter().enumerate() {
            let part:i64=row.try_get(0)?;let len:i64=row.try_get(1)?;let got:i64=row.try_get(2)?;
            let (Ok(bytes),Ok(got))=(usize::try_from(len),u32::try_from(got)) else {valid=false;break};
            if part!=index as i64+1 {valid=false;break}
            crc=if index==0 {got}else{crate::gf2::crc32_combine(crc,got,bytes)};
            size=size.checked_add(len).ok_or_else(||anyhow::anyhow!("native folded file size overflow"))?;
        }
        if valid && let Some(file)=FileCrc::new(size,crc) && !out.contains(&file) {out.push(file);}
    }
    out.sort_by_key(|file|std::cmp::Reverse(file.size));Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::StepReads;

    #[test]
    fn native_key_inputs_preserve_original_salt_nulls_crc_and_still_hash_policy() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let runtime=crate::pg::stage_queue_tests::runtime(&schema);
        crate::pg::stage_queue_tests::push(&runtime);
        let probe=crate::probe::ProbeInfo {headers:std::collections::BTreeMap::from([("Message-ID".into(),"<original@example.test>".into())]),..Default::default()};
        let info=serde_json::to_string(&probe).unwrap();
        let (id,file)=runtime.owner.with(|client| {
            let row=client.query_one("select release_id,id from files",&[])?;let id:i64=row.get(0);let file:i64=row.get(1);
            client.execute("insert into release_probe(release_id,info,moviehash) values($1,$2,'')",&[&id,&info])?;
            assert_eq!(client.execute("update file_timing set lines=7,articles=1 where file_id=$1 and release_id=$2",&[&file,&id])?,1,"the real accepted header must already own its timing row");
            client.execute("insert into release_keys(release_id,account,video) values($1,-1,99)",&[&id])?;
            client.execute("insert into archive_inner_files(release_id,seq,name,size,crc32,source) values($1,0,'excluded',2097152,1,1),($1,1,'kept',2097152,4294967295,2)",&[&id])?;
            client.execute("insert into segment_crc(release_id,message_id,decoded,crc32) values($1,$2,2097152,4294967295)",&[&id,&b"<part@example.test>".as_slice()])?;
            Ok((id,file))
        }).unwrap();
        assert!(file>0);
        let reads=StepReads::for_owner(&runtime.owner).unwrap();let data=crate::postprocess::data::StepData::Native(&reads);
        let salt=data.key_salt(None).unwrap();assert_eq!(data.key_salt(Some(salt)).unwrap(),salt);
        let stored=data.stored_release_keys(id).unwrap().unwrap();assert_eq!(stored.account,Some(-1));assert_eq!(stored.video,Some(99));assert_eq!(stored.mux,None);
        let mut expected=crate::keys::keys_of(Some(&probe),&[(Some(7),1)],Some(1),Some(salt));
        let file=crate::keys::FileCrc {size:2097152,crc32:u32::MAX};expected.file_crc=Some(file.key());
        assert_eq!(data.release_keys(id,Some(1),salt).unwrap(),expected);
        let lookup=data.release_lookup(id,Some(1)).unwrap();
        assert_eq!(lookup.keys,crate::keys::keys_of(Some(&probe),&[(Some(7),1)],Some(1),None));
        assert_eq!(lookup.moviehash,Some(String::new()));assert_eq!(lookup.files,vec![file]);assert_eq!(lookup.inner_files,vec![file]);
        assert_eq!(data.stills_lookup(id).unwrap().moviehash,None,"the existing still lookup excludes empty hashes");
        assert!(data.stored_release_keys(id+1).unwrap().is_none());
        assert!(data.key_salt(Some(salt.wrapping_add(1))).is_err());
        runtime.owner.close();
        assert!(matches!(data.release_lookup(id,Some(1)),Err(crate::postprocess::step::StepError::Native(_))));
    }

    #[test]
    fn native_key_salt_survives_a_canonical_owner_replacement() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let runtime=crate::pg::stage_queue_tests::runtime(&schema);
        let reads=StepReads::for_owner(&runtime.owner).unwrap();let salt=reads.key_salt(None).unwrap();
        let old=*runtime.owner.epoch().unwrap().id();runtime.owner.close();
        let next=crate::pg::stage_queue_tests::runtime(&schema);
        assert_ne!(next.owner.epoch().unwrap().id(),&old);
        assert_eq!(StepReads::for_owner(&next.owner).unwrap().key_salt(None).unwrap(),salt);
        next.owner.close();
    }
}
