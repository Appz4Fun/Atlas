//! Owned-schema input-shape experiment. Production COPY/index/SQL are reused.
use super::*;
use std::{io::Write,os::unix::fs::{DirBuilderExt,OpenOptionsExt},process::{Command,Stdio},time::{Duration,Instant}};

const SELECTOR:&str="pg::release_writer::multipart_plan_experiment::one_multipart_file_retains_bounded_winner_work_with_background_segments";
const CHILD:&str="ATLAS_MULTIPART_WRITER_CHILD";

#[test]
#[ignore="Root-only isolated PostgreSQL multipart writer experiment;120-second child admission"]
fn one_multipart_file_retains_bounded_winner_work_with_background_segments() {
    if std::env::var_os(CHILD).is_some() {child();return;}
    let schema=crate::pg::testing::Schema::native_writer().expect("explicit isolated PostgreSQL admission");
    let parent=std::path::PathBuf::from(std::env::var_os("ATLAS_HOME").expect("explicit private test home"));
    let home=tempfile::Builder::new().prefix("multipart-writer-").tempdir_in(parent).unwrap();
    std::fs::DirBuilder::new().mode(0o700).create(home.path().join("tmp")).unwrap();
    let mut config=std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(home.path().join("config.json")).unwrap();
    config.write_all(b"{}").unwrap();drop(config);
    let log=std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(home.path().join("child.private.log")).unwrap();
    let mut process=Command::new(std::env::current_exe().unwrap()).args(["--ignored","--exact",SELECTOR,"--nocapture"])
        .env_clear().env(CHILD,"1").env("ATLAS_PG_TEST_URL",&schema.url).env("ATLAS_MULTIPART_WRITER_SCHEMA",&schema.name)
        .env("ATLAS_HOME",home.path()).env("HOME",home.path()).env("TMPDIR",home.path().join("tmp"))
        .env("ATLAS_NO_KEYRING","1").env("ATLAS_SAB_DIR",home.path().join("no-sab"))
        .stdin(Stdio::null()).stdout(Stdio::from(log.try_clone().unwrap())).stderr(Stdio::from(log)).spawn().unwrap();
    let until=Instant::now()+Duration::from_secs(120);
    let status=loop {if let Some(status)=process.try_wait().unwrap(){break status;}
        if Instant::now()>=until {process.kill().unwrap();process.wait().unwrap();let _=home.keep();panic!("closed multipart writer child exceeded120seconds");}
        std::thread::sleep(Duration::from_millis(20));};
    let bytes=std::fs::read(home.path().join("child.private.log")).unwrap();
    eprintln!("closed multipart writer child stderr sha256={:x}",Sha256::digest(bytes));
    if !status.success() {let _=home.keep();panic!("closed multipart writer child failed; durable private evidence retained under admitted home");}
    let report:serde_json::Value=serde_json::from_slice(&std::fs::read(home.path().join("plans.private.json")).unwrap()).unwrap();
    assert_eq!(report["probes"].as_array().unwrap().len(),2);assert_eq!(report["semantics"]["parts"],2000);
    let _=home.keep();
}

fn persist(report:&serde_json::Value) {
    let home=crate::paths::app_dir();let next=home.join("plans.next.private.json");
    let mut file=std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&next).unwrap();
    file.write_all(&serde_json::to_vec_pretty(report).unwrap()).unwrap();file.sync_all().unwrap();drop(file);
    std::fs::rename(next,home.join("plans.private.json")).unwrap();std::fs::File::open(home).unwrap().sync_all().unwrap();
}
fn phase(report:&mut serde_json::Value,name:&str) {report["phase"]=name.into();persist(report);eprintln!("closed multipart writer phase:{name}");}
fn child() {
    let mut report=serde_json::json!({"input_rows":2000,"target_prior_parts":8192,"background_segments":262144,
        "background_files":2048,"linear_work_bound":16000,"probes":[],"phase":"connect"});persist(&report);
    let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||run(&mut report)));
    match result {
        Ok(Ok(()))=>{report["completed"]=true.into();persist(&report);},
        Ok(Err(error))=>{
            report["failure_sqlstate"]=error.downcast_ref::<postgres::Error>().and_then(|error|error.code()).map(|code|code.code()).into();
            persist(&report);panic!("closed multipart writer SQL/setup failure; durable phase/report retained");
        },
        Err(panic)=>{report["assertion_failed"]=true.into();persist(&report);std::panic::resume_unwind(panic);}
    }
}
fn target(first:i64,count:i64)->Release {
    Release {name:"Multipart.Diagnostic".into(),collection:Some("multipart-diagnostic".into()),group:"alt.binaries.test".into(),
        poster:"fixture".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
        articles:(first..first+count).map(|part|Article {filename:Some("movie.bin".into()),message_id:format!("<multipart-{part}@fixture.invalid>"),
            subject:format!("movie.bin yEnc ({part}/10192)"),part:Some(part),total_parts:Some(10192),bytes:17,..Default::default()}).collect(),..Default::default()}
}
fn state(client:&mut Client,id:i64)->Result<Vec<i64>> {
    let row=client.query_one("select r.parts,r.size,q.generation,(select count(*) from files where release_id=r.id),
        (select count(*) from segments where release_id=r.id),(select sum(distinct_parts)::bigint from files where release_id=r.id),
        (select count(*) from file_parts p join files f on f.id=p.file_id where f.release_id=r.id),
        (select sum(t.articles)::bigint from file_timing t where t.release_id=r.id),
        case when r.complete then 1::bigint else 0 end,
        (select case when bool_and(complete) then 1::bigint else 0 end from files where release_id=r.id)
        from releases r join pp_queue q on q.release_id=r.id where r.id=$1",&[&id])?;
    Ok((0..10).map(|column|row.get::<_,i64>(column)).collect())
}
fn probe(client:&mut Client,input:&Release,id:i64,label:&str)->Result<serde_json::Value> {
    let before=state(client,id)?;
    let batch:Vec<_>=input.articles.iter().map(|article|Incoming {release:input,release_key:identity(input).1,
        original_name_key:Sha256::digest(input.name.as_bytes()).into(),posted_at:crate::dates::article_timestamp(&input.date),
        posted_clock:crate::dates::naive_timestamp(&input.date),article:Some(article)}).collect();
    ensure!(batch.len()==2000&&batch.iter().map(Incoming::approx_bytes).sum::<usize>()<MAX_BATCH_BYTES,"bounded multipart COPY input");
    let mut tx=client.transaction()?;crate::pg::history_gate::native(&mut tx)?;
    tx.batch_execute("drop table if exists pg_temp.atlas_release_input")?;tx.batch_execute(CREATE_STAGE)?;
    let key=timing::key(&mut tx)?;let started=Instant::now();copy_input(&mut tx,&batch,Some(key))?;
    let copy_ms=started.elapsed().as_secs_f64()*1000.;
    ensure!(tx.query_one(WRITE_RELEASES,&[&input.group])?.get::<_,i64>(0)==0,"target release must already exist");
    let started=Instant::now();
    let result=tx.query(&format!("explain(analyze,buffers,format text,timing off) {WRITE_ARTICLES}"),&[&input.group]);
    let query_ms=started.elapsed().as_secs_f64()*1000.;
    let report=match result {
        Ok(rows)=>{
            let plan:Vec<String>=rows.iter().map(|row|row.get(0)).collect();
            let (join,scan)=super::plan_experiment::actual_work(&plan);
            let accepted:i64=tx.query_one("select count(*) from pg_temp.atlas_release_input where release_id=$1 and file_id is not null",&[&id])?.get(0);
            let files:i64=tx.query_one("select count(*) from files where release_id=$1",&[&id])?.get(0);
            let segments:i64=tx.query_one("select count(*) from segments where release_id=$1",&[&id])?.get(0);
            serde_json::json!({"label":label,"copy_and_index_ms":copy_ms,"statement_roundtrip_ms":query_ms,"plan":plan,
                "accepted":accepted,"files":files,"segments":segments,"accepted_join_work":join,"stage_scan_work":scan})
        },
        Err(error)=>{
            let budget=error.code().map(|code|code.code())==Some("57014")
                &&error.as_db_error().map(|db|db.message())==Some("canceling statement due to statement timeout");
            tx.rollback()?;ensure!(state(client,id)?==before,"failed probe must roll back target state");
            ensure!(budget,"only local statement timeout can be recorded as a censored probe");
            return Ok(serde_json::json!({"label":label,"copy_and_index_ms":copy_ms,"statement_roundtrip_ms":query_ms,"timeout":true}));
        }
    };
    tx.rollback()?;ensure!(state(client,id)?==before,"probe must roll back target state");Ok(report)
}
fn background(client:&mut Client)->Result<()> {
    ensure!(client.execute("insert into releases(group_name,is_collection,identity_key,name,poster,posted_date,is_obfuscated)
        select 'alt.binaries.multipart-background',false,sha256(convert_to('background-'||n,'UTF8')),'background-'||n,'fixture','',false
        from generate_series(1,512) n",&[])?==512,"bounded background release setup");
    let ids:Vec<i64>=client.query("select id from releases where group_name='alt.binaries.multipart-background' order by id",&[])?.iter().map(|row|row.get(0)).collect();
    for page in ids.chunks(128) {
        ensure!(client.execute("insert into files(release_id,filename_key,filename)
            select r.id,sha256(convert_to('file-'||n,'UTF8')),'file-'||n from releases r cross join generate_series(1,4) n where r.id=any($1)",&[&page])?==512,"bounded background files setup");
    }
    let files:Vec<i64>=client.query("select f.id from files f join releases r on r.id=f.release_id where r.group_name='alt.binaries.multipart-background' order by f.id",&[])?.iter().map(|row|row.get(0)).collect();
    for page in files.chunks(4) {
        ensure!(client.execute("insert into segments(release_id,file_id,message_key,message_id,part,bytes)
            select f.release_id,f.id,sha256(convert_to('background-'||f.id||'-'||p,'UTF8')),convert_to('<background-'||f.id||'-'||p||'@fixture.invalid>','UTF8'),p,17
            from files f cross join generate_series(1,128) p where f.id=any($1)",&[&page])?==512,"bounded background segments setup");
        ensure!(client.execute("insert into file_parts(file_id,part) select id,p from files cross join generate_series(1,128) p where id=any($1)",&[&page])?==512,"bounded background part setup");
    }
    for page in files.chunks(512) {
        ensure!(client.execute("update files set expected=128,distinct_parts=128,first_part=1,last_part=128,touched_at=100,complete=true where id=any($1)",&[&page])?==512,"bounded background file counters");
    }
    ensure!(client.execute("update releases set size=8704,parts=512,file_total=4,complete=true where id=any($1)",&[&ids])?==512,"bounded background release counters");
    ensure!(client.execute("insert into pp_queue(release_id,due_at) select id,100 from releases where id=any($1)",&[&ids])?==512,"bounded background queue setup");
    let row=client.query_one("select count(*),sum(parts)::bigint,sum(size)::bigint from releases where id=any($1)",&[&ids])?;
    ensure!((row.get::<_,i64>(0),row.get::<_,i64>(1),row.get::<_,i64>(2))==(512,262144,4456448),"background counters must match retained segments");
    client.batch_execute("analyze releases;analyze files;analyze segments")?;Ok(())
}
fn run(report:&mut serde_json::Value)->Result<()> {
    let pool=crate::pg::Pool::new(&crate::pg::StorageConfig {backend:crate::pg::Backend::Postgres,
        url:std::env::var("ATLAS_PG_TEST_URL")?,schema:std::env::var("ATLAS_MULTIPART_WRITER_SCHEMA")?,max_connections:1,allow_plaintext:true})?;
    let mut client=pool.get()?;client.batch_execute("set statement_timeout='3s';set lock_timeout='250ms'")?;
    phase(report,"seed_target");
    for page in 0..16 {let saved=save(&mut client,&[target(page*512+1,512)],100)?;
        ensure!(saved.parts==512&&saved.files==u64::from(page==0),"real target seed accepts512parts per save");}
    let id:i64=client.query_one("select id from releases where group_name='alt.binaries.test'",&[])?.get(0);
    let seeded=vec![8192,139264,16,1,8192,8192,8192,8192,0,0];ensure!(state(&mut client,id)?==seeded,"real target seed counters");
    client.batch_execute("analyze releases;analyze files;analyze segments")?;
    let input=target(8193,2000);phase(report,"probe_before_background");
    report["probes"].as_array_mut().unwrap().push(probe(&mut client,&input,id,"before_background")?);persist(report);
    phase(report,"seed_background");background(&mut client)?;ensure!(state(&mut client,id)?==seeded,"background cannot alter target counters");
    phase(report,"probe_after_background");
    report["probes"].as_array_mut().unwrap().push(probe(&mut client,&input,id,"after_background")?);persist(report);
    phase(report,"whole_save");let started=Instant::now();let result=save(&mut client,std::slice::from_ref(&input),200);
    report["whole_save_ms"]=(started.elapsed().as_secs_f64()*1000.).into();persist(report);
    let saved=result?;report["whole_save_result"]=serde_json::json!({"parts":saved.parts,"files":saved.files,"releases":saved.releases});persist(report);
    ensure!(saved==SaveStats {releases:0,files:0,parts:2000},"exact whole multipart save acceptance");
    let after=state(&mut client,id)?;ensure!(after==vec![10192,173264,17,1,10192,10192,10192,10192,1,1],"whole save maintains all target counters and completeness");
    phase(report,"replay");let started=Instant::now();let replay=save(&mut client,&[input],300)?;
    report["replay_ms"]=(started.elapsed().as_secs_f64()*1000.).into();
    ensure!(replay==SaveStats::default()&&state(&mut client,id)?==after,"real replay must preserve counts and queue generation");
    report["semantics"]=serde_json::json!({"parts":saved.parts,"new_files":saved.files,"replay_unchanged":true,"target_state":after});persist(report);
    phase(report,"linear_work_assertion");
    for probe in report["probes"].as_array().unwrap() {
        ensure!(probe["accepted"]==2000&&probe["files"]==1&&probe["segments"]==10192,"each rollback probe must accept exact target parts");
        assert!(probe["accepted_join_work"].as_f64().unwrap()<=16000.&&probe["stage_scan_work"].as_f64().unwrap()<=16000.,
            "multipart accepted winner mapping exceeded unchanged linear join/scan admission");
    }
    Ok(())
}
