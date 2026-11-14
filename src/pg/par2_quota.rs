//! Bounded live PAR2 retention on the caller's existing transaction. Original
//! offline preservation is separate; no other cache family is evicted here.
use anyhow::{Context,Result,ensure};
use postgres::{GenericClient,Transaction};
use serde::{Serialize,Deserialize};

pub(crate) const MAX_ROWS:i64=crate::raw_store::MAX_RAW_ARCHIVES_ROWS;
pub(crate) const MAX_BYTES:i64=crate::raw_store::MAX_RAW_ARCHIVE_NAME_BYTES;
const MAX_EVICTIONS:i32=256;

#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
pub enum Phase {#[default] Building,Trimming,Ready}
impl Phase {
    fn read(value:&str)->Result<Self> {match value {"building"=>Ok(Self::Building),"trimming"=>Ok(Self::Trimming),"ready"=>Ok(Self::Ready),_=>anyhow::bail!("unknown PAR2 accounting phase")}}
}

#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
pub struct Usage {
    pub phase:Phase,
    pub retained_rows:u64,pub retained_bytes:u64,
    pub evicted_inventories:u64,pub evicted_rows:u64,pub evicted_bytes:u64,
    #[serde(default)] pub pending_eviction_rows:u64,
    #[serde(default)] pub pending_eviction_bytes:u64,
}
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
pub struct Retention {
    /// Actual accounted rows before retention, including preserved variants.
    pub before:Usage,
    /// Actual committed retention after the bounded policy has run.
    pub retained:Usage,
}
impl Retention {
    pub fn evicted_rows(&self)->Result<u64> {
        self.retained.evicted_rows.checked_sub(self.before.evicted_rows)
          .ok_or_else(||anyhow::anyhow!("PAR2 retention counter moved backwards"))
    }
}

pub(crate) fn installed(client:&mut impl GenericClient)->Result<bool> {
    Ok(client.query_one("select to_regclass(format('%I.atlas_par2_cache_usage',current_schema())) is not null",&[])?.try_get(0)?)
}

/// Take this BEFORE any PAR2/preserved cache row lock or restore table lock.
/// The schema owner/fence is already held by the caller; this grants no owner.
pub(crate) fn lock(tx:&mut Transaction<'_>)->Result<()> {
    let row=tx.query_one("select phase from atlas_par2_cache_usage where singleton for update",&[])?;
    ensure!(Phase::read(row.try_get::<_,String>(0)?.as_str())?==Phase::Ready,"PAR2 quota initialization is incomplete");Ok(())
}

pub(crate) fn require_ready(client:&mut impl GenericClient)->Result<()> {
    let phase:String=client.query_one("select phase from atlas_par2_cache_usage where singleton",&[])?.try_get(0)?;
    ensure!(Phase::read(&phase)?==Phase::Ready,"PAR2 quota initialization is incomplete");Ok(())
}

/// A pre-claim result, not a cache miss or an authorization to evict facts.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(crate) enum Admission {Ready,Progress,MaintenanceRequired(Usage)}

/// Resume only derived accounting on the original owner, one existing bounded
/// page per turn. Startup never selects or advances an eviction victim.
pub(crate) fn admission_page(owner:&crate::pg::indexer_runtime::Owner)->Result<Admission> {
    ensure!(tokio::runtime::Handle::try_current().is_err(),"PAR2 admission requires a standard native lane");
    owner.epoch().context("PAR2 admission requires the canonical owner")?;
    owner.with(|client| {
        let mut tx=client.transaction()?;crate::pg::history_gate::native(&mut tx)?;
        let state=tx.query_one("select phase,scan_relation,evict_key is not null from atlas_par2_cache_usage where singleton for update",&[])?;
        let phase=Phase::read(state.try_get::<_,String>(0)?.as_str())?;
        let relation:i32=state.try_get(1)?;let evicting:bool=state.try_get(2)?;
        let before=usage(&mut tx)?;
        if phase==Phase::Ready {
            ensure!(relation==4&&!evicting,"PAR2 ready accounting lacks its actual EOFs");
            if before.retained_rows>MAX_ROWS as u64||before.retained_bytes>MAX_BYTES as u64 {
                tx.rollback()?;return Ok(Admission::MaintenanceRequired(before))
            }
            tx.commit()?;return Ok(Admission::Ready)
        }
        if phase==Phase::Trimming {
            ensure!(relation==4,"PAR2 accounting has not reached every source EOF");
            if evicting||before.retained_rows>MAX_ROWS as u64||before.retained_bytes>MAX_BYTES as u64 {
                tx.rollback()?;return Ok(Admission::MaintenanceRequired(before))
            }
            // Under this same row lock the trim loop immediately exits: no
            // active victim, both fixed caps satisfied. It only certifies Ready.
        } else {ensure!(!evicting,"PAR2 building accounting has an eviction victim");}
        if owner.stop.load(std::sync::atomic::Ordering::Acquire) {tx.rollback()?;return Err(crate::pg::outage::AttemptEnded.into())}
        let after=initialize_page(&mut tx)?;
        ensure!((after.evicted_inventories,after.evicted_rows,after.evicted_bytes,after.pending_eviction_rows,after.pending_eviction_bytes)
            ==(before.evicted_inventories,before.evicted_rows,before.evicted_bytes,before.pending_eviction_rows,before.pending_eviction_bytes),
            "PAR2 preadmission attempted cache eviction");
        if owner.stop.load(std::sync::atomic::Ordering::Acquire) {tx.rollback()?;return Err(crate::pg::outage::AttemptEnded.into())}
        tx.commit()?;
        Ok(if after.phase==Phase::Ready {Admission::Ready}else{Admission::Progress})
    })
}

pub(crate) fn usage(client:&mut impl GenericClient)->Result<Usage> {
    let r=client.query_one("select retained_rows,retained_bytes,evicted_inventories,evicted_rows,evicted_bytes,phase,evict_removed_rows,evict_removed_bytes from atlas_par2_cache_usage where singleton",&[])?;
    Ok(Usage {phase:Phase::read(r.try_get::<_,String>(5)?.as_str())?,retained_rows:u64::try_from(r.try_get::<_,i64>(0)?)?,retained_bytes:u64::try_from(r.try_get::<_,i64>(1)?)?,
      evicted_inventories:u64::try_from(r.try_get::<_,i64>(2)?)?,evicted_rows:u64::try_from(r.try_get::<_,i64>(3)?)?,
      evicted_bytes:u64::try_from(r.try_get::<_,i64>(4)?)?,pending_eviction_rows:u64::try_from(r.try_get::<_,i64>(6)?)?,
      pending_eviction_bytes:u64::try_from(r.try_get::<_,i64>(7)?)?})
}

/// Start only before worker admission or after the original owner drained its
/// visits. This quarantines one whole optional bundle; it grants no readiness.
pub(crate) fn begin_maintenance(tx:&mut Transaction<'_>)->Result<Usage> {
    tx.query_one("select atlas_par2_cache_begin_maintenance()",&[])?;usage(tx)
}

/// One bounded accounting/eviction page, committed by the original fenced
/// owner between calls. This never returns Ready before actual EOF and quota.
pub(crate) fn initialize_page(tx:&mut Transaction<'_>)->Result<Usage> {
    tx.query_one("select atlas_par2_cache_init_page()",&[])?;usage(tx)
}

pub(crate) fn finish(tx:&mut Transaction<'_>)->Result<Retention> {
    finish_with(tx,MAX_ROWS,MAX_BYTES,MAX_EVICTIONS)
}
fn finish_with(tx:&mut Transaction<'_>,rows:i64,bytes:i64,evictions:i32)->Result<Retention> {
    ensure!((1..=MAX_ROWS).contains(&rows) && (1..=MAX_BYTES).contains(&bytes)
      && (1..=MAX_EVICTIONS).contains(&evictions),"PAR2 quota admission outside fixed limits");
    lock(tx)?;let before=usage(tx)?;
    tx.query_one("select * from atlas_par2_cache_trim($1,$2,$3)",&[&rows,&bytes,&evictions])?;
    let retained=usage(tx)?;
    ensure!(retained.phase==Phase::Ready && retained.retained_rows<=rows as u64 && retained.retained_bytes<=bytes as u64,"PAR2 quota did not complete");
    Ok(Retention {before,retained})
}

/// Older cache-only CLI schemas remain usable without claiming quota support.
/// A partially installed/malformed quota is an error, never ignored.
/// The converter calls this only AFTER every source COPY parity check. Large
/// imports may commit in Trimming; that is explicit maintenance, not readiness.
pub(crate) fn finish_import_if_installed(tx:&mut Transaction<'_>)->Result<Option<Retention>> {
    if !installed(tx)? {return Ok(None);}
    lock(tx)?;let before=usage(tx)?;
    tx.query_one("select atlas_par2_cache_import_finish()",&[])?;
    Ok(Some(Retention {before,retained:usage(tx)?}))
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::pg::natural_cache::{self as cache,Budget,Key};
    const BUDGET:Budget=Budget {bytes:1024*1024,rows:32};
    fn inventory(rows:usize)->crate::par2::Inventory {
        crate::par2::Inventory {files:(0..rows).map(|n|crate::par2::Par2File {set_id:[1;16],file_id:[n as u8;16],md5:[3;16],md5_16k:[4;16],size:50,
          name:format!("file-{n}"),bound:true,crc32:Some(19)}).collect(),complete:true,..Default::default()}
    }
    fn count(client:&mut impl GenericClient)->i64 {
        client.query_one("select (select count(*) from ext_raw_par2)+(select count(*) from ext_raw_par2_files)+(select count(*) from ext_preserved_cache_rows where family in ('ext_raw_par2','ext_raw_par2_files'))",&[]).unwrap().get(0)
    }
    #[test]
    fn par2_quota_counts_replaced_variants_and_evicts_only_whole_content_inventories() {
        let Some(schema)=crate::pg::testing::Schema::new() else{return};let mut client=schema.pool.get().unwrap();
        let key=Key {name:"Kept",group:"g",content_id:&[1]};
        let mut tx=client.transaction().unwrap();cache::put_par2_files(&mut tx,key,&inventory(3),BUDGET).unwrap();tx.commit().unwrap();
        let mut tx=client.transaction().unwrap();cache::put_par2_files(&mut tx,Key {content_id:&[2],..key},&inventory(2),BUDGET).unwrap();
        assert_eq!(usage(&mut tx).unwrap().retained_rows,5);assert_eq!(count(&mut tx),5);
        tx.execute("insert into ext_raw_probes(name,group_name,content_id,info,probed_at) values('valuable','g',decode('01','hex'),'keep',1)",&[]).unwrap();
        tx.execute("insert into ext_preserved_cache_rows(family,source_digest,row_digest,name,group_name,content_id,schema_descriptor,row_format,row_bytes) values('ext_raw_probes',decode(repeat('01',32),'hex'),decode(repeat('02',32),'hex'),'valuable','g',decode('01','hex'),'[]',1,decode('010203','hex'))",&[]).unwrap();
        let r=finish_with(&mut tx,4,MAX_BYTES,MAX_EVICTIONS).unwrap();assert_eq!(r.evicted_rows().unwrap(),3);tx.commit().unwrap();
        assert_eq!(count(&mut *client),2);assert_eq!(usage(&mut *client).unwrap().retained_rows,2);
        let facts=cache::par2_files(&mut *client,Key {content_id:&[2],..key},BUDGET).unwrap().unwrap();assert_eq!(facts.files.len(),2);
        assert_eq!(client.query_one("select count(*) from ext_preserved_cache_rows where family='ext_raw_par2_files'",&[]).unwrap().get::<_,i64>(0),0);
        assert_eq!(client.query_one("select info from ext_raw_probes",&[]).unwrap().get::<_,String>(0),"keep");
        assert_eq!(client.query_one("select row_bytes from ext_preserved_cache_rows where family='ext_raw_probes'",&[]).unwrap().get::<_,Vec<u8>>(0),[1,2,3]);
    }
    #[test]
    fn par2_quota_existing_name_growth_and_eviction_work_refusal_are_atomic() {
        let Some(schema)=crate::pg::testing::Schema::new() else{return};let mut client=schema.pool.get().unwrap();
        for name in ["a","b","c"] {let mut tx=client.transaction().unwrap();cache::put_par2_files(&mut tx,Key {name,group:"g",content_id:&[1]},&inventory(1),BUDGET).unwrap();tx.commit().unwrap();}
        let before=usage(&mut *client).unwrap();let mut tx=client.transaction().unwrap();
        assert!(finish_with(&mut tx,1,MAX_BYTES,1).is_err());tx.rollback().unwrap();
        assert_eq!(usage(&mut *client).unwrap(),before);assert_eq!(count(&mut *client),3);
        let mut tx=client.transaction().unwrap();cache::put_par2_files(&mut tx,Key {name:"a",group:"g",content_id:&[2]},&inventory(3),BUDGET).unwrap();
        assert_eq!(usage(&mut tx).unwrap().retained_rows,6,"existing-name growth plus prior variant counts");
        let r=finish_with(&mut tx,3,MAX_BYTES,MAX_EVICTIONS).unwrap();assert!(r.evicted_rows().unwrap()>=3);tx.commit().unwrap();
        let left=client.query("select name,count(*) from ext_raw_par2_files group by name",&[]).unwrap();
        assert_eq!(left.len(),1);assert_eq!(left[0].get::<_,String>(0),"a");assert_eq!(left[0].get::<_,i64>(1),3);
        assert_eq!(usage(&mut *client).unwrap().retained_rows,3);
        let mut tx=client.transaction().unwrap();assert!(finish_with(&mut tx,MAX_ROWS+1,MAX_BYTES,1).is_err());tx.rollback().unwrap();
    }
    #[test]
    fn par2_quota_concurrent_writers_serialize_before_whole_inventory_eviction() {
        let Some(schema)=crate::pg::testing::Schema::new() else{return};let mut first=schema.pool.get().unwrap();
        for name in ["a","b"] {let mut tx=first.transaction().unwrap();cache::put_par2_files(&mut tx,Key {name,group:"g",content_id:&[1]},&inventory(2),BUDGET).unwrap();tx.commit().unwrap();}
        let first_pid:i32=first.query_one("select pg_backend_pid()",&[]).unwrap().get(0);
        let mut second=schema.pool.get().unwrap();let second_pid:i32=second.query_one("select pg_backend_pid()",&[]).unwrap().get(0);
        let mut observer=schema.pool.get().unwrap();let mut held=first.transaction().unwrap();finish_with(&mut held,3,MAX_BYTES,MAX_EVICTIONS).unwrap();
        std::thread::scope(|scope| {
        // Release the held transaction before the scope joins on panic too;
        // the borrowed test pool and schema outlive both concurrent writers.
        let held=held;
        let writer=scope.spawn(move|| {
            let mut tx=second.transaction().unwrap();cache::put_par2_files(&mut tx,Key {name:"c",group:"g",content_id:&[1]},&inventory(2),BUDGET).unwrap();
            finish_with(&mut tx,3,MAX_BYTES,MAX_EVICTIONS).unwrap();tx.commit().unwrap();
        });
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(5);let mut blocked=false;
        while std::time::Instant::now()<deadline {
            let pids:Vec<i32>=observer.query_one("select pg_blocking_pids($1)",&[&second_pid]).unwrap().get(0);
            if pids.contains(&first_pid) {blocked=true;break;}
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        held.commit().unwrap();writer.join().unwrap();assert!(blocked,"actual second writer must wait for the first quota owner");
        });
        let rows=first.query("select name,count(*) from ext_raw_par2_files group by name",&[]).unwrap();
        assert_eq!(rows.len(),1);assert_eq!(rows[0].get::<_,String>(0),"c");assert_eq!(rows[0].get::<_,i64>(1),2);
        assert_eq!(usage(&mut *first).unwrap().retained_rows,2);
    }

    #[test]
    fn par2_quota_existing_target_bootstrap_resumes_actual_eofs_and_bounded_trimming() {
        let Some(schema)=crate::pg::testing::Schema::empty() else{return};let mut client=schema.pool.get().unwrap();
        client.batch_execute("create table schema_migrations(version integer primary key,name text not null,applied_at timestamptz not null default now())").unwrap();
        for &(version,name,sql) in crate::pg::migrate::MIGRATIONS.iter().filter(|m|m.0<=10) {
            let mut tx=client.transaction().unwrap();tx.batch_execute(sql).unwrap();tx.execute("insert into schema_migrations(version,name) values($1,$2)",&[&version,&name]).unwrap();tx.commit().unwrap();
        }
        client.batch_execute("insert into ext_raw_par2_files(name,group_name,content_id,file_id,set_id,md5,md5_16k,size,file_name)
          select 'old-'||n,'g',decode('01','hex'),decode(lpad(to_hex(n),32,'0'),'hex'),decode(repeat('02',16),'hex'),decode(repeat('03',16),'hex'),decode(repeat('04',16),'hex'),50,'file' from generate_series(1,2500) n").unwrap();
        crate::pg::migrate::run(&mut client,&schema.name).unwrap();assert_eq!(usage(&mut *client).unwrap().phase,Phase::Building);
        let before=usage(&mut *client).unwrap();let mut tx=client.transaction().unwrap();
        assert!(cache::put_par2_files(&mut tx,Key {name:"refused",group:"g",content_id:&[1]},&inventory(1),BUDGET).is_err());tx.rollback().unwrap();
        let mut tx=client.transaction().unwrap();assert!(tx.query_one("select atlas_par2_cache_init_page(1,1)",&[]).is_err());tx.rollback().unwrap();
        assert_eq!(usage(&mut *client).unwrap(),before,"refusal advances neither source accounting nor counters");
        // Each page commits; dropping and reacquiring the client exercises the
        // durable cursor rather than an in-memory continuation.
        for _ in 0..32 {
            if usage(&mut *client).unwrap().phase!=Phase::Building {break;}
            let mut tx=client.transaction().unwrap();initialize_page(&mut tx).unwrap();tx.commit().unwrap();drop(client);client=schema.pool.get().unwrap();
        }
        assert_eq!(usage(&mut *client).unwrap().phase,Phase::Trimming);assert_eq!(usage(&mut *client).unwrap().retained_rows,2500);
        let mut turns=0;
        while usage(&mut *client).unwrap().phase!=Phase::Ready {
            let mut tx=client.transaction().unwrap();tx.query_one("select * from atlas_par2_cache_trim($1,$2,$3)",&[&1i64,&MAX_BYTES,&256i32]).unwrap();tx.commit().unwrap();
            turns+=1;assert!(turns<=10);
        }
        assert!(turns>1);assert_eq!(count(&mut *client),1);assert_eq!(usage(&mut *client).unwrap().retained_rows,1);
    }

    #[test]
    fn par2_quota_large_bundle_stays_quarantined_until_all_members_are_evicted() {
        let Some(schema)=crate::pg::testing::Schema::empty() else{return};let mut client=schema.pool.get().unwrap();
        client.batch_execute("create table schema_migrations(version integer primary key,name text not null,applied_at timestamptz not null default now())").unwrap();
        for &(version,name,sql) in crate::pg::migrate::MIGRATIONS.iter().filter(|m|m.0<=10) {
            let mut tx=client.transaction().unwrap();tx.batch_execute(sql).unwrap();tx.execute("insert into schema_migrations(version,name) values($1,$2)",&[&version,&name]).unwrap();tx.commit().unwrap();
        }
        client.batch_execute("insert into ext_raw_par2_files(name,group_name,content_id,file_id,set_id,md5,md5_16k,size,file_name)
          select 'one-bundle','g',decode('01','hex'),decode(lpad(to_hex(n),32,'0'),'hex'),decode(repeat('02',16),'hex'),decode(repeat('03',16),'hex'),decode(repeat('04',16),'hex'),50,'file' from generate_series(1,2500) n").unwrap();
        crate::pg::migrate::run(&mut client,&schema.name).unwrap();
        for _ in 0..32 {if usage(&mut *client).unwrap().phase!=Phase::Building {break;}let mut tx=client.transaction().unwrap();initialize_page(&mut tx).unwrap();tx.commit().unwrap();}
        assert_eq!(usage(&mut *client).unwrap().phase,Phase::Trimming);assert_eq!(count(&mut *client),2500);
        let mut tx=client.transaction().unwrap();tx.query_one("select * from atlas_par2_cache_trim($1,$2,$3)",&[&1i64,&MAX_BYTES,&256i32]).unwrap();tx.commit().unwrap();
        let partial=usage(&mut *client).unwrap();assert_eq!(partial.phase,Phase::Trimming);assert_eq!(partial.retained_rows,2500-256);
        assert_eq!(partial.pending_eviction_rows,256);assert_eq!(partial.evicted_rows,0,"partial deletion is not a whole-inventory receipt");
        assert!(cache::par2_files(&mut *client,Key {name:"one-bundle",group:"g",content_id:&[1]},BUDGET).is_err(),"partial inventory never becomes a cache miss or hit");
        let dir=tempfile::tempdir().unwrap();assert!(crate::pg::ext::dump(&mut client,dir.path(),2,false).is_err(),"a partial inventory cannot become a qualified backup");
        for _ in 0..16 {if usage(&mut *client).unwrap().phase==Phase::Ready {break;}let mut tx=client.transaction().unwrap();initialize_page(&mut tx).unwrap();tx.commit().unwrap();}
        let final_usage=usage(&mut *client).unwrap();assert_eq!(final_usage.phase,Phase::Ready);assert_eq!(final_usage.retained_rows,0);
        assert_eq!(final_usage.evicted_rows,2500);assert_eq!(final_usage.evicted_inventories,1);assert_eq!(final_usage.pending_eviction_rows,0);
        assert_eq!(count(&mut *client),0);assert_eq!(client.query_one("select count(*) from atlas_par2_cache_members",&[]).unwrap().get::<_,i64>(0),0);
    }

    #[test]
    fn par2_quota_ready_large_victim_refuses_atomically_then_drained_maintenance_completes() {
        let Some(schema)=crate::pg::testing::Schema::new() else{return};let mut client=schema.pool.get().unwrap();
        client.batch_execute("insert into ext_raw_par2_files(name,group_name,content_id,file_id,set_id,md5,md5_16k,size,file_name)
          select 'large','g',decode('01','hex'),decode(lpad(to_hex(n),32,'0'),'hex'),decode(repeat('02',16),'hex'),decode(repeat('03',16),'hex'),decode(repeat('04',16),'hex'),50,'file' from generate_series(1,300) n").unwrap();
        let before=usage(&mut *client).unwrap();let mut tx=client.transaction().unwrap();assert!(finish_with(&mut tx,1,MAX_BYTES,MAX_EVICTIONS).is_err());tx.rollback().unwrap();
        assert_eq!(usage(&mut *client).unwrap(),before);assert_eq!(count(&mut *client),300);
        let mut tx=client.transaction().unwrap();assert_eq!(begin_maintenance(&mut tx).unwrap().phase,Phase::Trimming);tx.commit().unwrap();
        let mut tx=client.transaction().unwrap();let partial=initialize_page(&mut tx).unwrap();assert_eq!(partial.phase,Phase::Trimming);tx.commit().unwrap();
        assert_eq!(usage(&mut *client).unwrap().pending_eviction_rows,256);
        let mut tx=client.transaction().unwrap();assert_eq!(initialize_page(&mut tx).unwrap().phase,Phase::Ready);tx.commit().unwrap();
        assert_eq!(count(&mut *client),0);assert_eq!(usage(&mut *client).unwrap().evicted_rows,300);
    }

    #[test]
    fn par2_quota_ready_typed_oversized_member_rolls_back_replacement_and_remains_evictable() {
        let Some(schema)=crate::pg::testing::Schema::new() else{return};let mut client=schema.pool.get().unwrap();
        let key=Key {name:"good",group:"g",content_id:&[1]};
        let mut tx=client.transaction().unwrap();cache::put_par2_files(&mut tx,key,&inventory(2),BUDGET).unwrap();tx.commit().unwrap();
        let before=usage(&mut *client).unwrap();let old=cache::par2_files(&mut *client,key,BUDGET).unwrap().unwrap().files;
        let mut oversize=inventory(1);oversize.files[0].name="x".repeat(8*1024*1024);
        let mut tx=client.transaction().unwrap();
        let error=cache::put_par2_files(&mut tx,Key {content_id:&[2],..key},&oversize,Budget {bytes:16*1024*1024,rows:32}).unwrap_err();
        assert!(error.downcast_ref::<postgres::Error>().is_some(),"the valid typed call reached the database row-admission boundary");tx.rollback().unwrap();
        assert_eq!(usage(&mut *client).unwrap(),before);assert_eq!(cache::par2_files(&mut *client,key,BUDGET).unwrap().unwrap().files,old);
        assert_eq!(client.query_one("select count(*) from ext_preserved_cache_rows where family='ext_raw_par2_files'",&[]).unwrap().get::<_,i64>(0),0,"replacement's archived originals roll back too");
        assert_eq!(client.query_one("select count(*) from atlas_par2_cache_members",&[]).unwrap().get::<_,i64>(0),2);
        let mut tx=client.transaction().unwrap();cache::put_par2_files(&mut tx,Key {content_id:&[2],..key},&inventory(1),BUDGET).unwrap();
        finish_with(&mut tx,1,MAX_BYTES,MAX_EVICTIONS).unwrap();tx.commit().unwrap();
        assert_eq!(usage(&mut *client).unwrap().phase,Phase::Ready);assert_eq!(count(&mut *client),1);
        assert_eq!(cache::par2_files(&mut *client,Key {content_id:&[2],..key},BUDGET).unwrap().unwrap().files.len(),1);
    }

    #[test]
    fn par2_quota_ready_binary_copy_oversized_member_refuses_without_changing_originals() {
        use postgres::{binary_copy::BinaryCopyInWriter,types::Type};
        let Some(schema)=crate::pg::testing::Schema::new() else{return};let mut client=schema.pool.get().unwrap();
        let key=Key {name:"good",group:"g",content_id:&[1]};
        let mut tx=client.transaction().unwrap();cache::put_par2_files(&mut tx,key,&inventory(2),BUDGET).unwrap();tx.commit().unwrap();
        let before=usage(&mut *client).unwrap();let old=cache::par2_files(&mut *client,key,BUDGET).unwrap().unwrap().files;
        let payload="x".repeat(8*1024*1024);let cid=vec![2u8];let id=vec![9u8;16];let set=vec![1u8;16];let md5=vec![3u8;16];let head=vec![4u8;16];
        let mut tx=client.transaction().unwrap();
        let sink=tx.copy_in("copy ext_raw_par2_files(name,group_name,content_id,file_id,set_id,md5,md5_16k,size,file_name) from stdin(format binary)").unwrap();
        let types=[Type::TEXT,Type::TEXT,Type::BYTEA,Type::BYTEA,Type::BYTEA,Type::BYTEA,Type::BYTEA,Type::INT8,Type::TEXT];
        let mut writer=BinaryCopyInWriter::new(sink,&types);
        let result=writer.write(&[&"incoming",&"g",&cid,&id,&set,&md5,&head,&50i64,&payload]);
        let error=match result {Ok(())=>writer.finish().unwrap_err(),Err(error)=>{drop(writer);error}};
        assert!(error.as_db_error().is_some(),"COPY must preserve the server admission error, never report a successful/missing cache row");tx.rollback().unwrap();
        assert_eq!(usage(&mut *client).unwrap(),before);assert_eq!(count(&mut *client),2);
        assert_eq!(cache::par2_files(&mut *client,key,BUDGET).unwrap().unwrap().files,old);
        assert_eq!(client.query_one("select count(*) from ext_preserved_cache_rows where family in ('ext_raw_par2','ext_raw_par2_files')",&[]).unwrap().get::<_,i64>(0),0);
        let mut tx=client.transaction().unwrap();cache::put_par2_files(&mut tx,Key {name:"small",..key},&inventory(1),BUDGET).unwrap();finish_with(&mut tx,1,MAX_BYTES,MAX_EVICTIONS).unwrap();tx.commit().unwrap();
        assert_eq!(cache::par2_files(&mut *client,Key {name:"small",..key},BUDGET).unwrap().unwrap().files.len(),1);
    }

    #[test]
    fn par2_quota_oversized_preserved_payload_refuses_bootstrap_without_changing_original() {
        let Some(schema)=crate::pg::testing::Schema::empty() else{return};let mut client=schema.pool.get().unwrap();
        client.batch_execute("create table schema_migrations(version integer primary key,name text not null,applied_at timestamptz not null default now())").unwrap();
        for &(version,name,sql) in crate::pg::migrate::MIGRATIONS.iter().filter(|m|m.0<=10) {
            let mut tx=client.transaction().unwrap();tx.batch_execute(sql).unwrap();tx.execute("insert into schema_migrations(version,name) values($1,$2)",&[&version,&name]).unwrap();tx.commit().unwrap();
        }
        // Intentionally inert preserved bytes: qualification is about bounded
        // accounting, not activating or parsing an unknown old cache format.
        client.batch_execute("insert into ext_preserved_cache_rows(family,source_digest,row_digest,name,group_name,content_id,schema_descriptor,row_format,row_bytes)
          values('ext_raw_par2_files',decode(repeat('01',32),'hex'),decode(repeat('02',32),'hex'),'oversize','g',decode('01','hex'),'[]',1,decode(repeat('ab',16*1024*1024),'hex'))").unwrap();
        let error=crate::pg::migrate::run(&mut client,&schema.name).unwrap_err();
        assert!(error.as_db_error().unwrap().message().contains("initialization row exceeds byte admission"));
        assert_eq!(crate::pg::migrate::version(&mut client,&schema.name).unwrap(),10,"failed migration is atomic");
        assert_eq!(client.query_one("select octet_length(row_bytes),name from ext_preserved_cache_rows",&[]).unwrap().get::<_,i32>(0),16*1024*1024);
        assert_eq!(client.query_one("select name from ext_preserved_cache_rows",&[]).unwrap().get::<_,String>(0),"oversize");
    }

    #[test]
    fn par2_quota_byte_ceiling_counts_preserved_payloads_and_refuses_driver_errors() {
        let Some(schema)=crate::pg::testing::Schema::new() else{return};let mut client=schema.pool.get().unwrap();
        let mut tx=client.transaction().unwrap();cache::put_par2_files(&mut tx,Key {name:"a",group:"g",content_id:&[1]},&inventory(2),BUDGET).unwrap();tx.commit().unwrap();
        let before=usage(&mut *client).unwrap();assert!(before.retained_bytes>0);
        let mut tx=client.transaction().unwrap();let r=finish_with(&mut tx,MAX_ROWS,1,MAX_EVICTIONS).unwrap();assert_eq!(r.retained.retained_rows,0);tx.commit().unwrap();assert_eq!(count(&mut *client),0);
        client.batch_execute("drop table atlas_par2_cache_usage cascade").unwrap();
        let mut tx=client.transaction().unwrap();let error=finish(&mut tx).unwrap_err();assert!(error.downcast_ref::<postgres::Error>().is_some());tx.rollback().unwrap();
    }
}

#[cfg(test)]
#[path="par2_admission_tests.rs"]
mod admission_tests;
