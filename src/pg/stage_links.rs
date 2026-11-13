//! Checked native link predicates used before costly new audio/still probes.
//! These are read-only suppression hints, never article-lending authority.
use anyhow::{Result,Context,ensure};
use crate::keys::{Kind,Lookup};
use std::{sync::atomic::Ordering,time::{Duration,Instant}};
use super::StepReads;

impl StepReads {
    pub(crate) fn audio_linked_by(&self,own:i64,lookup:&Lookup)->Result<Option<Kind>> {
        let deadline=Instant::now()+Duration::from_secs(10);
        self.read(|tx| {
            tx.query_one("select set_config('statement_timeout','10000',true)",&[])?;
            for kind in Kind::ALL {
                if matches!(kind,Kind::Account|Kind::Host|Kind::Owner|Kind::Zone|Kind::AudioPrint|Kind::Subs|Kind::Fonts|Kind::Volumes) || !lookup.has(kind) {continue;}
                let selected=donors(tx,own,lookup,kind,false,deadline,&self.owner.stop)?;
                if !selected.is_empty() {self.observe_in(tx,&selected)?;return Ok(Some(kind));}
            }
            Ok(None)
        })
    }
    pub(crate) fn same_file_stills(&self,own:i64,lookup:&Lookup)->Result<Option<Kind>> {
        let deadline=Instant::now()+Duration::from_secs(10);
        let candidates=self.read(|tx| {
            tx.query_one("select set_config('statement_timeout','10000',true)",&[])?;
            let mut result=Vec::new();let mut selected=Vec::new();
            for kind in [Kind::Mux,Kind::MovieHash] {
                if !lookup.has(kind) {continue;}
                for donor in donors(tx,own,lookup,kind,true,deadline,&self.owner.stop)? {
                    selected.push(donor);
                    let mut hashes=Vec::new();
                    for row in tx.query("select case when octet_length(sha256)=64 then sha256 end from release_stills where release_id=$1 order by position limit $2",&[&donor,&(crate::probe::stills::POSITIONS.len() as i64+1)])? {
                        hashes.push(row.try_get::<_,String>(0)?);
                    }
                    ensure!(hashes.len()==crate::probe::stills::POSITIONS.len(),"native still donor changed within its snapshot");
                    result.push((kind,hashes));
                }
            }
            self.observe_in(tx,&selected)?;Ok(result)
        })?;
        for (kind,hashes) in candidates {
            if self.owner.stop.load(Ordering::Acquire) {return Err(super::super::outage::AttemptEnded.into());}
            let mut complete=true;
            for hash in hashes {
                let path=crate::probe::stills::still_file(&hash).context("native still hash is invalid")?;
                match std::fs::metadata(path) {
                    Ok(metadata)=>complete&=metadata.is_file(),
                    Err(error) if error.kind()==std::io::ErrorKind::NotFound=>complete=false,
                    Err(error)=>return Err(error.into()),
                }
            }
            if complete {return Ok(Some(kind));}
        }
        Ok(None)
    }
}

fn key(lookup:&Lookup,kind:Kind)->Option<(&'static str,&'static str,i64)> {
    let (column,value)=match kind {
        Kind::Account=>("account",lookup.keys.account),Kind::Host=>("host",lookup.keys.host),Kind::Layout=>("layout",lookup.keys.layout),Kind::Lines=>("lines",lookup.keys.lines),
        Kind::BodyCrc=>("body_crc",lookup.keys.body_crc),Kind::Video=>("video",lookup.keys.video),Kind::Audio=>("audio",lookup.keys.audio),Kind::Mux=>("mux",lookup.keys.mux),
        Kind::Encode=>("encode",lookup.keys.encode),Kind::Thread=>("thread",lookup.keys.thread),Kind::Subs=>("subs",lookup.keys.subs),Kind::Fonts=>("fonts",lookup.keys.fonts),
        Kind::Chapters=>("chapters",lookup.keys.chapters),Kind::Volumes=>("volumes",lookup.keys.volumes),
        Kind::ArchiveTwin=>return lookup.archive.twin_key.map(|n|("archive_release","twin_key",n)),
        Kind::Owner=>return lookup.archive.owner_key.map(|n|("archive_release","owner_key",n)),
        Kind::Zone=>return lookup.archive.zone_key.map(|n|("archive_release","zone_key",n)),
        _=>return None,
    };value.map(|n|("release_keys",column,n))
}
fn donors(tx:&mut postgres::Transaction<'_>,own:i64,lookup:&Lookup,kind:Kind,stills:bool,deadline:Instant,stop:&std::sync::atomic::AtomicBool)->Result<Vec<i64>> {
    let mut after:Option<i64>=None;let mut found=Vec::new();let mut buckets=[0u8;crate::catalog_facts::SHARDS];let mut scanned=0usize;
    // Preserve the original 16 candidates per group bucket for still reuse.
    // The bucket is not a PostgreSQL ID encoding or a physical SQLite shard.
    loop {
        if stop.load(Ordering::Acquire) {return Err(super::super::outage::AttemptEnded.into());}
        ensure!(Instant::now()<deadline && scanned<10000,"native link candidates exceed read admission");
        let group=format!("case when octet_length(r.group_name)<={} then r.group_name end",crate::filters::MAX_GROUP_NAME);
        let kept=if stills {format!("and (select count(*) from release_stills s where s.release_id=r.id)={} and (select count(*) from release_stills s where s.release_id=r.id and phash is not null)={}",crate::probe::stills::POSITIONS.len(),crate::probe::stills::POSITIONS.len())} else {String::new()};
        let suffix=format!("and r.id<>$1 and ($3::bigint is null or r.id<$3) {kept} order by r.id desc limit 256");
        // SQL identifiers originate only in the closed Kind mapping above.
        let rows=if let Some((table,column,value))=key(lookup,kind) {
            tx.query(&format!("select r.id,{group} from {table} k join releases r on r.id=k.release_id where k.{column}=$2 {suffix}"),&[&own,&value,&after])?
        } else if kind==Kind::MovieHash {
            tx.query(&format!("select r.id,{group} from release_probe k join releases r on r.id=k.release_id where k.moviehash=$2 {suffix}"),&[&own,&lookup.moviehash,&after])?
        } else if kind==Kind::Par2 {
            ensure!(lookup.par2_sets.len()<=4 && lookup.par2_sets.iter().all(|id|id.len()==16),"native PAR2 lookup exceeds admission");
            tx.query(&format!("select r.id,{group} from releases r where exists(select 1 from par2_files p where p.release_id=r.id and p.bound<>0 and p.set_id=any($2)) {suffix}"),&[&own,&lookup.par2_sets,&after])?
        } else if matches!(kind,Kind::FileCrc|Kind::InnerFile) {
            let files=if kind==Kind::FileCrc {&lookup.files} else {&lookup.inner_files};ensure!(files.len()<=8,"native CRC lookup exceeds admission");
            let keys=files.iter().map(|file|file.key()).collect::<Vec<_>>();let sizes=files.iter().map(|file|file.size).collect::<Vec<_>>();let crcs=files.iter().map(|file|i64::from(file.crc32)).collect::<Vec<_>>();
            tx.query(&format!("select r.id,{group} from releases r where (exists(select 1 from release_keys k where k.release_id=r.id and file_crc=any($2))
                or exists(select 1 from par2_files p join unnest($4::bigint[],$5::bigint[]) as wanted(size,crc) on p.size=wanted.size and p.crc32=wanted.crc where p.release_id=r.id and bound<>0)
                or exists(select 1 from rev_volumes v join unnest($4::bigint[],$5::bigint[]) as wanted(size,crc) on v.size=wanted.size and v.crc32=wanted.crc where v.release_id=r.id)
                or exists(select 1 from archive_inner_files f join unnest($4::bigint[],$5::bigint[]) as wanted(size,crc) on f.size=wanted.size and f.crc32=wanted.crc where f.release_id=r.id and source in (2,3,4))) {suffix}"),&[&own,&keys,&after,&sizes,&crcs])?
        } else {anyhow::bail!("native link kind is not ported")};
        if rows.is_empty() {return Ok(found);}
        let last=rows.len()<256;
        for row in rows {
            scanned+=1;let id:i64=row.try_get(0)?;after=Some(id);let group:String=row.try_get(1)?;
            if crate::filters::group_blocked(&group) {continue;}
            if !stills {return Ok(vec![id]);}
            let bucket=crate::catalog_facts::shard_of(&group);
            if buckets[bucket]<16 {buckets[bucket]+=1;found.push(id);}
        }
        if last || buckets.iter().all(|n|*n==16) {return Ok(found);}
    }
}
