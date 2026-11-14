//! Actual legacy cache rows and migration EOFs, never fabricated accounting.
use super::*;
use crate::pg::{testing::Schema,indexer_runtime::Runtime};

fn legacy_cache(rows:i64,name_bytes:i32)->(Schema,Runtime) {
    let schema=Schema::empty().expect("explicit isolated ATLAS_PG_TEST_URL required");
    let mut client=schema.pool.get().unwrap();
    client.batch_execute("create table schema_migrations(version integer primary key,name text not null,applied_at timestamptz not null default now())").unwrap();
    for &(version,name,sql) in crate::pg::migrate::MIGRATIONS.iter().filter(|m|m.0<=10) {
        let mut tx=client.transaction().unwrap();tx.batch_execute(sql).unwrap();
        tx.execute("insert into schema_migrations(version,name) values($1,$2)",&[&version,&name]).unwrap();tx.commit().unwrap();
    }
    client.execute("insert into ext_raw_par2_files(name,group_name,content_id,file_id,set_id,md5,md5_16k,size,file_name)
        select 'legacy-cache','g',decode('01','hex'),decode(lpad(to_hex(n),32,'0'),'hex'),decode(repeat('02',16),'hex'),
        decode(repeat('03',16),'hex'),decode(repeat('04',16),'hex'),50,repeat('x',$2) from generate_series(1,$1::bigint) n",
        &[&rows,&name_bytes]).unwrap();
    client.execute("insert into ext_raw_probes(name,group_name,content_id,info,probed_at) values('valuable','g',decode('01','hex'),'preserved-original',1)",&[]).unwrap();
    crate::pg::migrate::run(&mut client,&schema.name).unwrap();
    assert_eq!(usage(&mut *client).unwrap().phase,Phase::Building,"source must genuinely exceed the migration's five pages");
    drop(client);
    let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    (schema,runtime)
}
fn facts(runtime:&Runtime)->(i64,i64,String) {
    runtime.owner.with(|c| {
        let r=c.query_one("select count(*),coalesce(sum(octet_length(file_name)),0)::bigint,(select info from ext_raw_probes where name='valuable') from ext_raw_par2_files",&[])?;
        Ok((r.try_get(0)?,r.try_get(1)?,r.try_get(2)?))
    }).unwrap()
}
fn finish_pages(runtime:&Runtime)->Admission {
    for _ in 0..64 {
        match admission_page(&runtime.owner).unwrap() {
            Admission::Progress=>{},result=>return result,
        }
    }
    panic!("bounded fixture did not reach its actual EOFs");
}
#[test]
#[ignore="explicit isolated PG; genuine legacy cache beyond migration page allowance"]
fn native_par2_preadmission_resumes_actual_accounting_and_preserves_all_cache_facts() {
    let (_schema,runtime)=legacy_cache(2500,4);
    let before=facts(&runtime);
    assert!(matches!(finish_pages(&runtime),Admission::Ready));
    assert_eq!(facts(&runtime),before);
    let after=runtime.owner.with(|c|usage(c)).unwrap();
    assert_eq!(after.phase,Phase::Ready);assert_eq!(after.retained_rows,2500);
    assert_eq!((after.evicted_rows,after.evicted_bytes,after.pending_eviction_rows),(0,0,0));
    let inventory=runtime.owner.with(|c|crate::pg::natural_cache::par2_files(c,crate::pg::natural_cache::Key {name:"legacy-cache",group:"g",content_id:&[1]},crate::pg::natural_cache::Budget {bytes:16<<20,rows:3000})).unwrap().unwrap();
    assert_eq!(inventory.files.len(),2500,"real PAR2 facts must become reusable, not a synthetic cache miss");
    assert!(matches!(admission_page(&runtime.owner).unwrap(),Admission::Ready));
    assert!(runtime.owner.failure().is_none());runtime.owner.close();
}
#[test]
#[ignore="explicit isolated PG; 33 bounded TOAST rows exceed the real fixed byte ceiling"]
fn native_par2_preadmission_over_cap_preserves_originals_and_keeps_header_owner_healthy() {
    // Each actual row remains below the existing 8MiB accounting-page bound.
    // PostgreSQL compresses these repeated synthetic filenames; only lengths
    // and counters cross into Rust, never the 276MB logical string payload.
    let (_schema,runtime)=legacy_cache(33,8_380_000);
    let before=facts(&runtime);assert!(before.1>MAX_BYTES);
    let Admission::MaintenanceRequired(blocked)=finish_pages(&runtime) else {panic!("over-cap originals must wait, never be trimmed by startup")};
    assert_eq!(blocked.phase,Phase::Trimming);assert!(blocked.retained_bytes>MAX_BYTES as u64);
    assert_eq!((blocked.evicted_rows,blocked.pending_eviction_rows),(0,0));
    for _ in 0..3 {assert!(matches!(admission_page(&runtime.owner).unwrap(),Admission::MaintenanceRequired(_)));}
    assert_eq!(facts(&runtime),before);
    crate::pg::stage_queue_tests::push(&runtime);
    runtime.owner.with(|c| {assert!(c.query_one("select exists(select 1 from releases)",&[])?.get::<_,bool>(0));Ok(())}).unwrap();
    assert!(runtime.owner.failure().is_none(),"cache maintenance must not poison actual header acceptance");
    runtime.owner.close();
}
#[test]
#[ignore="explicit isolated PG; an actual partial-retention victim is never advanced at startup"]
fn native_par2_preadmission_refuses_active_eviction_without_deleting_or_claiming_readiness() {
    let schema=Schema::new().expect("explicit isolated ATLAS_PG_TEST_URL required");
    let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    runtime.owner.with(|c| {
        let mut tx=c.transaction()?;crate::pg::history_gate::native(&mut tx)?;
        crate::pg::natural_cache::put_par2_files(&mut tx,crate::pg::natural_cache::Key {name:"one-bundle",group:"g",content_id:&[1]},
            &crate::par2::Inventory {files:vec![crate::par2::Par2File {file_id:[1;16],set_id:[2;16],md5:[3;16],md5_16k:[4;16],size:50,name:"kept".into(),bound:true,crc32:None}],complete:true,..Default::default()},
            crate::pg::natural_cache::Budget {bytes:1<<20,rows:8})?;
        tx.commit()?;Ok(())
    }).unwrap();
    runtime.owner.with(|c| {let mut tx=c.transaction()?;crate::pg::history_gate::native(&mut tx)?;begin_maintenance(&mut tx)?;tx.commit()?;Ok(())}).unwrap();
    let before=runtime.owner.with(|c|usage(c)).unwrap();
    assert!(matches!(admission_page(&runtime.owner).unwrap(),Admission::MaintenanceRequired(_)));
    assert_eq!(runtime.owner.with(|c|usage(c)).unwrap(),before);
    runtime.owner.with(|c| {assert_eq!(c.query_one("select count(*) from ext_raw_par2_files",&[])?.get::<_,i64>(0),1);Ok(())}).unwrap();
    assert!(runtime.owner.failure().is_none());runtime.owner.close();
}
#[test]
#[ignore="explicit isolated PG; cancellation and genuine schema errors retain original owner semantics"]
fn native_par2_preadmission_stop_and_pg_failure_do_not_fabricate_ready() {
    let (_schema,runtime)=legacy_cache(2500,4);
    runtime.owner.stop.store(true,std::sync::atomic::Ordering::Release);
    let error=admission_page(&runtime.owner).unwrap_err();
    assert!(crate::pg::outage::is_attempt_ended(&error));assert!(runtime.owner.failure().is_none());runtime.owner.close();
    let schema=Schema::new().expect("explicit isolated ATLAS_PG_TEST_URL required");let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    runtime.owner.with(|c|{c.batch_execute("alter table atlas_par2_cache_usage rename to fixture_usage_unavailable")?;Ok(())}).unwrap();
    let error=admission_page(&runtime.owner).unwrap_err();
    assert!(error.chain().any(|e|e.downcast_ref::<postgres::Error>().is_some()));
    assert_eq!(runtime.owner.failure(),Some(crate::pg::outage::Failure::Fatal));runtime.owner.close();
}
