use super::*;
use crate::pg::{testing::Schema,indexer_runtime::Runtime};

pub(crate) fn fixture()->(Schema,Runtime) {
    let schema=Schema::empty().expect("explicit isolated ATLAS_PG_TEST_URL required");
    let mut c=schema.pool.get().unwrap();
    c.batch_execute("create table schema_migrations(version integer primary key,name text not null,applied_at timestamptz not null default now())").unwrap();
    for &(v,n,sql) in super::super::migrate::MIGRATIONS.iter().filter(|m|m.0<=10) {
        let mut tx=c.transaction().unwrap();tx.batch_execute(sql).unwrap();tx.execute("insert into schema_migrations(version,name) values($1,$2)",&[&v,&n]).unwrap();tx.commit().unwrap();
    }
    c.batch_execute("insert into ext_raw_par2_files(name,group_name,content_id,file_id,set_id,md5,md5_16k,size,file_name)
        select 'old','g',decode('01','hex'),decode(lpad(to_hex(n),32,'0'),'hex'),decode(repeat('02',16),'hex'),decode(repeat('03',16),'hex'),decode(repeat('04',16),'hex'),50,'old-file' from generate_series(1,2500) n;
        insert into ext_raw_par2(name,group_name,content_id,covers_all) values('old','g',decode('01','hex'),1);
        insert into ext_preserved_cache_rows(family,source_digest,row_digest,name,group_name,content_id,schema_descriptor,row_format,row_bytes)
          values('ext_raw_probes',decode(repeat('01',32),'hex'),decode(repeat('02',32),'hex'),'old','g',decode('01','hex'),'[]',1,decode('010203','hex'));
        insert into ext_raw_probes(name,group_name,content_id,info,probed_at) values('kept','g',decode('01','hex'),'probe-original',1);
        insert into ext_predb_titles(title,source,added_at) values('Kept.Title',3,1);
        insert into ext_http_cache(cache_key,source,url,status,body_zst,fetched_at) values('kept','fixture','https://example.invalid',200,decode('0001ff','hex'),1)").unwrap();
    super::super::migrate::run(&mut c,&schema.name).unwrap();drop(c);
    let runtime=super::super::stage_queue_tests::runtime(&schema);(schema,runtime)
}
fn digest(c:&mut impl postgres::GenericClient,table:&str)->String {
    c.query_one(&format!("select md5(coalesce(string_agg(to_jsonb(t)::text,E'\\n' order by to_jsonb(t)::text),'')) from {table} t"),&[]).unwrap().get(0)
}
fn inventory()->crate::par2::Inventory {
    crate::par2::Inventory {files:vec![crate::par2::Par2File {file_id:[9;16],set_id:[8;16],md5:[7;16],md5_16k:[6;16],size:42,name:"new-file".into(),bound:true,crc32:None}],complete:true,..Default::default()}
}
#[test]
#[ignore="explicit isolated PG; real inherited heaps, guards and new typed live cache"]
fn archive_preserves_physical_heaps_and_other_cache_facts_and_creates_true_empty_live_scope() {
    let (_schema,runtime)=fixture();
    let before=runtime.owner.with(|c|Ok(FACTS.iter().map(|(hot,_)|digest(c,hot)).collect::<Vec<_>>())).unwrap();
    let sentinels=runtime.owner.with(|c|Ok([digest(c,"ext_raw_probes"),digest(c,"ext_predb_titles"),digest(c,"ext_http_cache")])).unwrap();
    let receipt=archive(&runtime.owner,[7;32]).unwrap();
    assert_eq!(receipt.baseline_kind,"operator_reference_to_old_scope");
    runtime.owner.with(|c| {
        for (i,(hot,cold)) in FACTS.iter().enumerate() {
            assert_eq!(digest(c,cold),before[i]);assert!(!c.query_one(&format!("select exists(select 1 from {hot})"),&[])?.get::<_,bool>(0));
        }
        assert_eq!([digest(c,"ext_raw_probes"),digest(c,"ext_predb_titles"),digest(c,"ext_http_cache")],sentinels);Ok(())
    }).unwrap();
    let key=super::super::natural_cache::Key {name:"new",group:"g",content_id:&[9]};
    runtime.owner.with(|c| {
        let mut tx=c.transaction()?;super::super::history_gate::native(&mut tx)?;
        super::super::natural_cache::put_par2_files(&mut tx,key,&inventory(),super::super::natural_cache::Budget {bytes:1<<20,rows:8})?;tx.commit()?;
        assert_eq!(super::super::natural_cache::par2_files(c,key,super::super::natural_cache::Budget {bytes:1<<20,rows:8})?.unwrap().files.len(),1);Ok(())
    }).unwrap();
    assert!(matches!(super::super::natural_cache::par2_quota::admission_page(&runtime.owner).unwrap(),super::super::natural_cache::par2_quota::Admission::Ready));
    assert_eq!(archive(&runtime.owner,[7;32]).unwrap(),receipt,"idempotent retry must not rotate new live facts");
    runtime.owner.close();
    let runtime=super::super::stage_queue_tests::runtime(&_schema);
    assert_eq!(archive(&runtime.owner,[7;32]).unwrap(),receipt,"fresh original Owner retries the same committed scope");
    assert!(runtime.owner.failure().is_none());runtime.owner.close();
}
#[test]
#[ignore="explicit isolated PG; atomic rollback after metadata renames and cold immutability"]
fn archive_failure_rolls_back_all_names_and_receipt_and_cold_writes_are_refused() {
    let (schema,runtime)=fixture();
    let before=runtime.owner.with(|c|Ok(FACTS.iter().map(|(hot,_)|digest(c,hot)).collect::<Vec<_>>())).unwrap();
    runtime.owner.with(|c| {c.batch_execute("create function atlas_par2_cold_refuse() returns trigger language plpgsql as $$ begin return null; end $$")?;Ok(())}).unwrap();
    assert!(archive(&runtime.owner,[7;32]).is_err());runtime.owner.close();
    let mut c=schema.pool.get().unwrap();
    assert!(!c.query_one("select to_regclass('atlas_par2_live_scope') is not null",&[]).unwrap().get::<_,bool>(0));
    for (i,(hot,cold)) in FACTS.iter().enumerate() {assert_eq!(digest(&mut *c,hot),before[i]);assert!(!c.query_one("select to_regclass($1) is not null",&[cold]).unwrap().get::<_,bool>(0));}
    c.batch_execute("drop function atlas_par2_cold_refuse()").unwrap();drop(c);
    let runtime=super::super::stage_queue_tests::runtime(&schema);archive(&runtime.owner,[7;32]).unwrap();
    runtime.owner.with(|c| {
        let mut tx=c.transaction()?;
        assert!(tx.execute("delete from atlas_par2_cold_files where name='old'",&[]).is_err());tx.rollback()?;
        assert_eq!(digest(c,"atlas_par2_cold_files"),before[1]);Ok(())
    }).unwrap();runtime.owner.close();
}
#[test]
fn archive_cli_requires_explicit_apply_and_closed_unmixed_arguments() {
    let args=|a:&[&str]|a.iter().map(|s|s.to_string()).collect::<Vec<_>>();
    assert!(parse(&args(&["--archive-par2-cache"])).is_err());
    assert!(parse(&args(&["--archive-par2-cache","--apply","--source-baseline-sha256",&"07".repeat(32)])).is_ok());
    assert!(parse(&args(&["--archive-par2-cache","--apply","--source-baseline-sha256",&"07".repeat(32),"--bg-indexer"])).is_err());
    assert!(parse(&args(&["--archive-par2-cache","--apply","--source-baseline-sha256","not-a-digest"])).is_err());
}

#[test]
#[ignore="explicit isolated PG; server timeout after renames must roll back the entire scope"]
fn archive_statement_timeout_restores_original_heaps_and_names() {
    let (schema,runtime)=fixture();
    let before=runtime.owner.with(|c|FACTS.iter().map(|(hot,_)|Ok((object(c,hot)?,digest(c,hot)))).collect::<Result<Vec<_>>>()).unwrap();
    // The actual production initializer is reached only after the heap-name
    // swap. Delay that server call, without replacing any archival code.
    runtime.owner.with(|c| {c.batch_execute("alter function atlas_par2_cache_init_page(integer,bigint) rename to fixture_par2_init_page;
        create function atlas_par2_cache_init_page(page_rows integer default 256,page_bytes bigint default 8388608) returns void language plpgsql as $$
        begin
          if current_setting('statement_timeout')<>'5s' or current_setting('lock_timeout')<>'5s' then
            raise exception 'fixture requires exact maintenance SQL deadlines';
          end if;
          perform pg_sleep(6);
          perform fixture_par2_init_page(page_rows,page_bytes);
        end $$")?;Ok(())}).unwrap();
    let error=archive(&runtime.owner,[7;32]).unwrap_err();
    let code=error.chain().find_map(|cause|cause.downcast_ref::<postgres::Error>().and_then(postgres::Error::code));
    assert_eq!(code,Some(&postgres::error::SqlState::QUERY_CANCELED),"must reach the actual server deadline after asserting its exact settings");
    runtime.owner.close();
    let mut c=schema.pool.get().unwrap();
    assert!(!exists(&mut *c,SCOPE).unwrap());
    for (i,(hot,cold)) in FACTS.iter().enumerate() {
        assert_eq!((object(&mut *c,hot).unwrap(),digest(&mut *c,hot)),before[i]);
        assert!(!exists(&mut *c,cold).unwrap());
    }
    for (hot,cold) in LEDGER.iter().chain(std::iter::once(&TOUCH)) {
        assert!(exists(&mut *c,hot).unwrap());assert!(!exists(&mut *c,cold).unwrap());
    }
}

#[cfg(unix)]
#[test]
#[ignore="explicit isolated PG; configured child proves actual normal PP worker settlement after archival"]
fn archived_scope_admits_the_normal_owned_pp_worker_without_provider_or_api_fallback() {
    assert!(!std::env::var("ATLAS_PG_TEST_URL").expect("explicit isolated ATLAS_PG_TEST_URL required").is_empty(),"explicit isolated ATLAS_PG_TEST_URL must not be empty");
    crate::raw_store::native_pg_opensubtitles_cache_tests::native_worker_parent(
        crate::raw_store::native_pg_opensubtitles_cache_tests::WorkerCase::ArchiveCategory);
}
