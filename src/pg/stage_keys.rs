//! Native key inputs retain their original catalog salt and checked PG facts.

use anyhow::{Result,ensure};
use postgres::Transaction;
use crate::keys::{Keys,Lookup,FileCrc};
use super::StepReads;
#[path="stage_key_budget.rs"]
mod budget;
pub(crate) use budget::{KeysRead,KeysSnapshot};

const PROBE_QUERY:&str="select case when octet_length(info)<=67108864 then info end,octet_length(info),case when octet_length(moviehash)<=65536 then moviehash end,octet_length(moviehash) from release_probe where release_id=$1";
const TIMING_QUERY:&str="select t.lines,t.articles from file_timing t join files f on f.id=t.file_id where f.release_id=$1 order by f.original_filename limit 5000";
const STORED_QUERY:&str="select account,host,layout,lines,file_crc,body_crc,video,audio,mux,encode,thread,subs,fonts,chapters,volumes from release_keys where release_id=$1";
fn stored_key_row(row:postgres::Row)->Result<Keys> {
    Ok(Keys {account:row.try_get(0)?,host:row.try_get(1)?,layout:row.try_get(2)?,lines:row.try_get(3)?,file_crc:row.try_get(4)?,body_crc:row.try_get(5)?,video:row.try_get(6)?,audio:row.try_get(7)?,mux:row.try_get(8)?,encode:row.try_get(9)?,thread:row.try_get(10)?,subs:row.try_get(11)?,fonts:row.try_get(12)?,chapters:row.try_get(13)?,volumes:row.try_get(14)?})
}

impl StepReads {
    pub(crate) fn key_salt(&self,cached:Option<i64>)->Result<i64> {
        self.read(|tx| {
            let salt:i64=tx.query_one("select value from meta where key='key_salt'",&[])?.try_get(0)?;
            ensure!(cached.is_none_or(|old|old==salt),"native key salt changed within its Step instance");
            Ok(salt)
        })
    }
    pub(crate) fn stored_release_keys(&self,id:i64)->Result<Option<Keys>> {
        self.read(|tx|tx.query_opt(STORED_QUERY,&[&id])?.map(stored_key_row).transpose())
    }
    pub(crate) fn release_keys(&self,id:i64,parts:Option<i64>,salt:i64)->Result<Keys> {
        self.read(|tx| {
            let actual:i64=tx.query_one("select value from meta where key='key_salt'",&[])?.try_get(0)?;
            ensure!(actual==salt,"native key input belongs to a different salt");
            let (probe,_)=probe_in(tx,id)?;let mut keys=keys_in(tx,id,parts,Some(salt),probe.as_ref())?;
            if keys.file_crc.is_none() {let local=self.local_in(tx)?;keys.file_crc=folded_in(tx,id,&self.owner.stop,local.as_deref())?.first().copied().map(FileCrc::key);}
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
            let local=self.local_in(tx)?;
            for file in folded_in(tx,id,&self.owner.stop,local.as_deref())? {if !files.contains(&file) {files.push(file);}}
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
    decode_probe(tx.query_opt(PROBE_QUERY,&[&id])?)
}
fn decode_probe(row:Option<postgres::Row>)->Result<(Option<crate::probe::ProbeInfo>,Option<String>)> {
    let Some(row)=row else {return Ok((None,None))};
    ensure!(row.try_get::<_,Option<i32>>(1)?.is_none_or(|n|n<=64*1024*1024) && row.try_get::<_,Option<i32>>(3)?.is_none_or(|n|n<=65536),"native key probe exceeds read admission");
    let info:Option<String>=row.try_get(0)?;
    // Native malformed/admission-refused JSON is an error, not missing facts.
    // Charge expanded ownership before serde constructs defaulted track arrays.
    let probe=info.map(|text|crate::pg::probe_decode::decode(text.as_bytes(),&mut (32u64<<20))).transpose()?;
    Ok((probe,row.try_get(2)?))
}
fn keys_in(tx:&mut Transaction<'_>,id:i64,parts:Option<i64>,salt:Option<i64>,probe:Option<&crate::probe::ProbeInfo>)->Result<Keys> {
    let timing=tx.query(TIMING_QUERY,&[&id])?.into_iter().map(|row|Ok((row.try_get(0)?,row.try_get(1)?))).collect::<Result<Vec<_>>>()?;
    Ok(crate::keys::keys_of(probe,&timing,parts,salt))
}
fn file_crc(row:&postgres::Row)->Result<Option<FileCrc>> {
    let size:i64=row.try_get(0)?;let crc:i64=row.try_get(1)?;
    Ok(u32::try_from(crc).ok().and_then(|crc|FileCrc::new(size,crc)))
}
fn crc_rows(tx:&mut Transaction<'_>,sql:&str,id:i64)->Result<Vec<FileCrc>> {
    tx.query(sql,&[&id])?.into_iter().map(|row|file_crc(&row)).collect::<Result<Vec<_>>>().map(|rows|rows.into_iter().flatten().collect())
}
fn folded_in(tx:&mut Transaction<'_>,id:i64,stop:&std::sync::atomic::AtomicBool,local:Option<&crate::article_local::Store>)->Result<Vec<FileCrc>> {
    folded_in_with_budget(tx,id,stop,None,local)
}
/// The local-store form of the per-file CRC statement below: for each part
/// 1..=expected, the first of its sixteen lowest message IDs that has a
/// recorded CRC, as (part, decoded, crc32) in part order. `None` as soon as a
/// part has none, where the statement would have returned too few rows.
fn local_part_crcs(tx:&mut Transaction<'_>,store:&crate::article_local::Store,release:i64,file:i64,expected:i64,budget:Option<&budget::Budget<'_>>)->Result<Option<Vec<(i64,i64,i64)>>> {
    let crc_sql="select message_id,decoded,crc32 from segment_crc where release_id=$1 and message_id=any($2)";
    let mut out=Vec::new();let mut from=1i64;
    while from<=expected {
        if let Some(budget)=budget {budget.check()?;}
        // 256 parts a page keeps at most 4096 candidate IDs in memory and on the wire.
        let to=expected.min(from+255);
        let candidates=store.file_part_copies_capped(file,from,to,16)?;
        let ids:Vec<&[u8]>=candidates.iter().map(|(_,mid)|mid.as_slice()).collect();
        let rows=match budget {Some(b)=>b.rows(tx,crc_sql,&[&release,&ids])?,None=>tx.query(crc_sql,&[&release,&ids])?};
        let mut known=std::collections::HashMap::with_capacity(rows.len());
        for row in rows {known.insert(row.try_get::<_,Vec<u8>>(0)?,(row.try_get::<_,i64>(1)?,row.try_get::<_,i64>(2)?));}
        let before=out.len();
        // Candidates arrive by part then message ID, so the first hit of a part is the one DISTINCT ON kept.
        for (part,mid) in &candidates {
            if out.last().is_some_and(|(last,_,_)|last==part) {continue}
            if let Some((decoded,crc)) = known.get(mid) {out.push((*part,*decoded,*crc));}
        }
        if (out.len()-before) as i64!=to-from+1 {return Ok(None)}
        from=to+1;
    }
    Ok(Some(out))
}
fn folded_in_with_budget(tx:&mut Transaction<'_>,id:i64,stop:&std::sync::atomic::AtomicBool,budget:Option<&budget::Budget<'_>>,local:Option<&crate::article_local::Store>)->Result<Vec<FileCrc>> {
    let count_sql="select count(*) from segment_crc where release_id=$1";
    let count:i64=match budget {Some(b)=>b.one(tx,count_sql,&[&id])?,None=>tx.query_one(count_sql,&[&id])?}.try_get(0)?;
    if count==0 {return Ok(Vec::new())}
    let mut out=Vec::new();
    let files_sql="select id,expected,distinct_parts from files where release_id=$1 order by id limit 64";
    let files=match budget {Some(b)=>b.rows(tx,files_sql,&[&id])?,None=>tx.query(files_sql,&[&id])?};
    for file in files {
        if stop.load(std::sync::atomic::Ordering::Acquire) {return Err(crate::pg::outage::AttemptEnded.into());}
        if let Some(budget)=budget {budget.check()?;}
        let file_id:i64=file.try_get(0)?;let expected:Option<i64>=file.try_get(1)?;let seen:i64=file.try_get(2)?;
        let Some(expected)=expected.filter(|n|(1..=4096).contains(n) && seen>=*n && count>=*n) else {continue};
        if let Some(store)=local {
            let Some(rows)=local_part_crcs(tx,store,id,file_id,expected,budget)? else {continue};
            if rows.len() as i64!=expected {continue}
            let (mut crc,mut size,mut valid)=(0u32,0i64,true);
            for (index,&(part,len,got)) in rows.iter().enumerate() {
                if let Some(budget)=budget {budget.check()?;}
                let (Ok(bytes),Ok(got))=(usize::try_from(len),u32::try_from(got)) else {valid=false;break};
                if part!=index as i64+1 {valid=false;break}
                crc=if index==0 {got}else{crate::gf2::crc32_combine(crc,got,bytes)};
                size=size.checked_add(len).ok_or_else(||anyhow::anyhow!("native folded file size overflow"))?;
            }
            if valid && let Some(file)=FileCrc::new(size,crc) && !out.contains(&file) {out.push(file);}
            continue;
        }
        // SegmentPick::Parts keeps the first sixteen MIDs for each part. The
        // first of those with a recorded CRC wins, as in the legacy map fold.
        let rows_sql="select distinct on(s.part) s.part,c.decoded,c.crc32 from (select part,message_id,row_number() over(partition by part order by message_id) as n from segments where file_id=$1 and part between 1 and $2) s join segment_crc c on c.release_id=$3 and c.message_id=s.message_id where s.n<=16 order by s.part,s.message_id";
        let rows=match budget {Some(b)=>b.rows(tx,rows_sql,&[&file_id,&expected,&id])?,None=>tx.query(rows_sql,&[&file_id,&expected,&id])?};
        if rows.len() as i64!=expected {continue}
        let (mut crc,mut size,mut valid)=(0u32,0i64,true);
        for (index,row) in rows.iter().enumerate() {
            if let Some(budget)=budget {budget.check()?;}
            let part:i64=row.try_get(0)?;let len:i64=row.try_get(1)?;let got:i64=row.try_get(2)?;
            let (Ok(bytes),Ok(got))=(usize::try_from(len),u32::try_from(got)) else {valid=false;break};
            if part!=index as i64+1 {valid=false;break}
            crc=if index==0 {got}else{crate::gf2::crc32_combine(crc,got,bytes)};
            size=size.checked_add(len).ok_or_else(||anyhow::anyhow!("native folded file size overflow"))?;
        }
        if valid && let Some(file)=FileCrc::new(size,crc) && !out.contains(&file) {out.push(file);}
    }
    out.sort_by_key(|file|std::cmp::Reverse(file.size));
    if let Some(budget)=budget {budget.check()?;}Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::StepReads;

    thread_local! {
        static QUERY_TIMEOUT_MS: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    }
    pub(super) fn query_budget()->Option<std::time::Duration> {
        QUERY_TIMEOUT_MS.with(|timeout|timeout.get()).map(|millis| {
            assert!(millis>0);std::time::Duration::from_millis(millis)
        })
    }
    struct QueryTimeout(Option<u64>);
    impl Drop for QueryTimeout {
        fn drop(&mut self) {QUERY_TIMEOUT_MS.with(|timeout|timeout.set(self.0));}
    }

    #[test]
    #[ignore="requires explicit isolated PostgreSQL and an actual blocked Keys SELECT"]
    fn keys_step_local_query_timeout_defers_without_poisoning_owner() {
        use crate::postprocess::{data::StepData,step::{ReleaseInfo,Step,StepCtx,StepError}};
        use std::{sync::{Arc,atomic::Ordering},time::{Duration,Instant}};

        let schema=crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
        let runtime=crate::pg::stage_queue_tests::runtime(&schema);
        crate::pg::stage_queue_tests::push(&runtime);
        let id=runtime.owner.with(|client| {
            let id:i64=client.query_one("select id from releases",&[])?.get(0);
            client.execute("insert into release_keys(release_id,video) values($1,99)",&[&id])?;
            Ok(id)
        }).unwrap();
        let reads=StepReads::for_owner(&runtime.owner).unwrap();
        let executor=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let bodies=crate::postprocess::bodies::BodyFetcher::new(
            Arc::new(crate::nntp::Pool::new(&[])),executor.handle().clone(),runtime.owner.stop.clone(),
            crate::config::PostprocessConfig::default(),Arc::new(crate::postprocess::stats::Stats::default()));
        let mut blocker=schema.pool.get().unwrap();
        let mut held=blocker.transaction().unwrap();
        held.batch_execute("lock table release_probe in access exclusive mode").unwrap();
        let _timeout=QueryTimeout(QUERY_TIMEOUT_MS.with(|timeout|timeout.replace(Some(100))));
        let mut context=StepCtx {
            release:ReleaseInfo {id,parts:1,..Default::default()},data:StepData::Native(&reads),facts:Vec::new(),
            bodies:&bodies,used_bodies:0,deadline:Instant::now()+Duration::from_millis(200),
            lane_can_fetch:false,crcs:Vec::new(),pictures:false,
        };
        let result=crate::keys::KeysStep::default().run(&mut context);
        // Drain the real blocker before any regression assertion can unwind.
        held.rollback().unwrap();
        drop(blocker);
        assert_eq!(context.used_bodies,0);
        assert!(context.crcs.is_empty());
        if let Err(StepError::Native(error))=&result {
            let pg=error.chain().find_map(|cause|cause.downcast_ref::<postgres::Error>())
                .expect("the unfixed path must retain its actual SELECT timeout cause");
            assert_eq!(pg.code(),Some(&postgres::error::SqlState::QUERY_CANCELED));
            assert_eq!(pg.as_db_error().unwrap().message(),"canceling statement due to statement timeout");
        }
        let failure=runtime.owner.failure();
        let healthy=failure.is_none()&&!runtime.owner.stop.load(Ordering::Acquire);
        if healthy {
            runtime.owner.with(|client| {
                assert_eq!(client.query_one("select 1",&[])?.get::<_,i32>(0),1);
                Ok(())
            }).expect("a deferred read must leave the original owner usable after rollback");
        }
        let mut observer=schema.pool.get().unwrap();
        assert_eq!(observer.query_one("select video from release_keys where release_id=$1",&[&id]).unwrap().get::<_,i64>(0),99);
        assert_eq!(observer.query_one("select count(*) from native_operations",&[]).unwrap().get::<_,i64>(0),0);
        drop(observer);
        runtime.owner.close();
        let error=result.err().expect("the blocked Keys read must defer rather than complete");
        assert!(matches!(&error,StepError::OutOfBudget)&&healthy,
            "a local Keys SELECT deadline must defer without ending the owner: error={error:?}, owner_failure={failure:?}");
    }

    #[test]
    #[ignore="requires explicit isolated PostgreSQL and an actual endpoint-owned SELECT timeout"]
    fn keys_owned_select_timeout_preserves_driver_cause_after_rollback() {
        use std::{sync::atomic::Ordering,time::{Duration,Instant}};
        let schema=crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
        let runtime=crate::pg::stage_queue_tests::runtime(&schema);
        crate::pg::stage_queue_tests::push(&runtime);
        let id=runtime.owner.with(|client|Ok(client.query_one("select id from releases",&[])?.get::<_,i64>(0))).unwrap();
        let reads=StepReads::for_owner(&runtime.owner).unwrap();
        let mut blocker=schema.pool.get().unwrap();let mut held=blocker.transaction().unwrap();
        held.batch_execute("lock table release_probe in access exclusive mode").unwrap();
        let _timeout=QueryTimeout(QUERY_TIMEOUT_MS.with(|timeout|timeout.replace(Some(500))));
        let result=reads.keys_snapshot(id,Some(1),None,Instant::now()+Duration::from_secs(5));
        held.rollback().unwrap();drop(blocker);
        let super::KeysRead::Deferred(cause)=result.unwrap() else {panic!("the blocked Keys SELECT must defer")};
        assert!(cause.is::<super::budget::LocalKeysDeadline>());
        let pg=cause.chain().find_map(|error|error.downcast_ref::<postgres::Error>()).expect("the actual SELECT timeout driver cause must survive deferral");
        assert_eq!(pg.code(),Some(&postgres::error::SqlState::QUERY_CANCELED));
        assert_eq!(pg.as_db_error().unwrap().message(),"canceling statement due to statement timeout");
        assert_eq!(runtime.owner.failure(),None);assert!(!runtime.owner.stop.load(Ordering::Acquire));
        runtime.owner.with(|client| {
            assert_eq!(client.query_one("select 1",&[])?.get::<_,i32>(0),1);Ok(())
        }).unwrap();runtime.owner.close();
    }

    #[test]
    #[ignore="requires explicit isolated PostgreSQL and actual administrative query cancellation"]
    fn keys_administrative_cancel_is_not_a_local_budget_deferral() {
        interrupt_keys_read(false);
    }

    #[test]
    #[ignore="requires explicit isolated PostgreSQL and termination of the original fixture backend"]
    fn keys_connection_loss_is_not_a_local_budget_deferral() {
        interrupt_keys_read(true);
    }

    fn interrupt_keys_read(terminate:bool) {
        use std::time::{Duration,Instant};
        let schema=crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
        let runtime=crate::pg::stage_queue_tests::runtime(&schema);
        crate::pg::stage_queue_tests::push(&runtime);
        let (id,pid)=runtime.owner.with(|client| {
            Ok((client.query_one("select id from releases",&[])?.get::<_,i64>(0),
                client.query_one("select pg_backend_pid()",&[])?.get::<_,i32>(0)))
        }).unwrap();
        let reads=StepReads::for_owner(&runtime.owner).unwrap();
        let mut blocker=schema.pool.get().unwrap();let mut held=blocker.transaction().unwrap();
        held.batch_execute("lock table release_probe in access exclusive mode").unwrap();
        let url=schema.url.clone();
        let cancel=std::thread::spawn(move|| {
            let mut observer=postgres::Client::connect(&url,postgres::NoTls).unwrap();
            observer.batch_execute("set statement_timeout='500ms'").unwrap();
            let until=Instant::now()+Duration::from_secs(2);
            loop {
                observer.query_one("select pg_stat_clear_snapshot()",&[]).unwrap();
                if observer.query_one("select exists(select 1 from pg_stat_activity where pid=$1 and wait_event_type='Lock')",&[&pid]).unwrap().get::<_,bool>(0) {break}
                assert!(Instant::now()<until,"Keys never reached its actual blocked SELECT");
                std::thread::sleep(Duration::from_millis(2));
            }
            let sql=if terminate {"select pg_terminate_backend($1)"} else {"select pg_cancel_backend($1)"};
            assert!(observer.query_one(sql,&[&pid]).unwrap().get::<_,bool>(0));
        });
        let result=reads.keys_snapshot(id,Some(1),None,Instant::now()+Duration::from_secs(5));
        held.rollback().unwrap();drop(blocker);cancel.join().unwrap();
        let error=result.expect_err("external interruption must remain a native error");
        assert!(!error.is::<super::budget::LocalKeysDeadline>());
        let pg=error.chain().find_map(|cause|cause.downcast_ref::<postgres::Error>()).unwrap();
        if terminate {
            assert!(error.is::<super::budget::ReadRollback>(),"lost original connection must not certify a successful rollback");
            assert_eq!(runtime.owner.failure(),Some(crate::pg::outage::Failure::Unavailable));
        } else {
            assert_eq!(pg.code(),Some(&postgres::error::SqlState::QUERY_CANCELED));
            assert_ne!(pg.as_db_error().unwrap().message(),"canceling statement due to statement timeout");
            assert_eq!(runtime.owner.failure(),Some(crate::pg::outage::Failure::Fatal));
        }
        runtime.owner.close();
    }

    #[test]
    #[ignore="requires explicit isolated PostgreSQL and a real mismatched cached catalog salt"]
    fn bounded_keys_snapshot_keeps_cached_salt_mismatch_fatal() {
        let schema=crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
        let runtime=crate::pg::stage_queue_tests::runtime(&schema);
        crate::pg::stage_queue_tests::push(&runtime);
        let id=runtime.owner.with(|client|Ok(client.query_one("select id from releases",&[])?.get::<_,i64>(0))).unwrap();
        let reads=StepReads::for_owner(&runtime.owner).unwrap();let salt=reads.key_salt(None).unwrap();
        let error=reads.keys_snapshot(id,Some(1),Some(salt.wrapping_add(1)),std::time::Instant::now()+std::time::Duration::from_secs(5))
            .expect_err("a cached salt mismatch must not become an empty or deferred Keys result");
        assert!(!error.is::<super::budget::LocalKeysDeadline>());
        assert_eq!(runtime.owner.failure(),Some(crate::pg::outage::Failure::Fatal));
        runtime.owner.close();
    }

    fn held_owner(owner:&std::sync::Arc<crate::pg::indexer_runtime::Owner>)->(std::sync::mpsc::Sender<()>,std::thread::JoinHandle<anyhow::Result<()>>) {
        use std::{sync::mpsc,time::Duration};
        let (entered,ready)=mpsc::channel();let (release,proceed)=mpsc::channel();
        let owner=owner.clone();
        let held=std::thread::spawn(move||owner.with(|_| {
            entered.send(())?;proceed.recv_timeout(Duration::from_secs(5))?;Ok(())
        }));
        ready.recv_timeout(Duration::from_secs(2)).unwrap();(release,held)
    }

    #[test]
    #[ignore="requires explicit isolated PostgreSQL and actual original-owner queue contention"]
    fn keys_deadline_expiring_in_owner_queue_defers_before_payload_reads() {
        use std::{sync::{mpsc,atomic::Ordering},time::{Duration,Instant}};
        let schema=crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
        let runtime=crate::pg::stage_queue_tests::runtime(&schema);
        crate::pg::stage_queue_tests::push(&runtime);
        let id=runtime.owner.with(|client| {
            let id=client.query_one("select id from releases",&[])?.get::<_,i64>(0);
            // Any attempted salt payload read after the deadline would now fail.
            client.execute("delete from meta where key='key_salt'",&[])?;Ok(id)
        }).unwrap();
        let (release,held)=held_owner(&runtime.owner);
        let (deadline,known)=mpsc::channel();let read_owner=runtime.owner.clone();
        let read=std::thread::spawn(move|| {
            let reads=StepReads::for_owner(&read_owner).unwrap();
            let until=Instant::now()+Duration::from_millis(100);
            deadline.send(until).unwrap();reads.keys_snapshot(id,Some(1),None,until)
        });
        let until=known.recv_timeout(Duration::from_secs(2)).unwrap();
        let admission_until=Instant::now()+Duration::from_secs(2);
        while runtime.owner.admission_waiters_for_test()[1]!=1 {
            assert!(Instant::now()<admission_until,"Keys did not queue on its original owner");
            std::thread::yield_now();
        }
        while Instant::now()<=until {std::thread::sleep(Duration::from_millis(2));}
        release.send(()).unwrap();held.join().unwrap().unwrap();
        let result=read.join().unwrap().unwrap();
        let super::KeysRead::Deferred(cause)=result else {panic!("expired queued Keys must defer")};
        assert!(cause.is::<super::budget::LocalKeysDeadline>());
        assert!(!cause.chain().any(|error|error.is::<postgres::Error>()),"no expired Keys payload query should execute");
        assert_eq!(runtime.owner.failure(),None);
        assert!(!runtime.owner.stop.load(Ordering::Acquire));
        runtime.owner.with(|client| {
            assert_eq!(client.query_one("select 1",&[])?.get::<_,i32>(0),1);Ok(())
        }).unwrap();
        runtime.owner.close();
    }

    #[test]
    #[ignore="requires explicit isolated PostgreSQL and a stop during original-owner queue contention"]
    fn keys_stop_while_queued_remains_attempt_end_without_owner_failure() {
        use std::{sync::atomic::Ordering,time::{Duration,Instant}};
        let schema=crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
        let runtime=crate::pg::stage_queue_tests::runtime(&schema);
        let (release,held)=held_owner(&runtime.owner);let owner=runtime.owner.clone();
        let read=std::thread::spawn(move|| {
            let reads=StepReads::for_owner(&owner).unwrap();
            reads.keys_snapshot(1,None,None,Instant::now()+Duration::from_secs(2))
        });
        let until=Instant::now()+Duration::from_secs(2);
        while runtime.owner.admission_waiters_for_test()[1]!=1 {
            assert!(Instant::now()<until,"Keys did not queue before stop");std::thread::yield_now();
        }
        runtime.owner.stop.store(true,Ordering::Release);
        let result=read.join().unwrap();
        let failure=runtime.owner.failure();
        release.send(()).unwrap();held.join().unwrap().unwrap();
        let error=result.expect_err("a stopped queued Keys read cannot complete or defer");
        assert!(crate::pg::outage::is_attempt_ended(&error));
        assert!(!error.is::<super::budget::LocalKeysDeadline>());
        assert_eq!(failure,None);assert_eq!(runtime.owner.failure(),None);
        runtime.owner.close();
    }

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
        let super::KeysRead::Complete(snapshot)=reads.keys_snapshot(id,Some(1),Some(salt),std::time::Instant::now()+std::time::Duration::from_secs(5)).unwrap()
            else {panic!("small admitted Keys inputs must complete")};
        assert_eq!(snapshot.salt,salt);
        assert_eq!(snapshot.computed,expected);
        assert_eq!(snapshot.stored,Some(stored));
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
