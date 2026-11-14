//! Explicitly admitted isolated-schema experiment; never called by the writer.
use super::*;
use std::{io::Write,os::unix::fs::{DirBuilderExt,OpenOptionsExt},process::{Command,Stdio},time::{Duration,Instant}};

const SELECTOR:&str="pg::release_writer::plan_experiment::isolated_article_writer_stage_plan_and_semantic_controls";
const CHILD:&str="ATLAS_WRITER_STAGE_PLAN_CHILD";

#[test]
#[ignore="Root-only isolated PostgreSQL writer plan experiment, bounded to 120 seconds"]
fn isolated_article_writer_stage_plan_and_semantic_controls() {
    if std::env::var_os(CHILD).is_some() {run();return;}
    let schema=crate::pg::testing::Schema::native_writer().expect("explicit isolated PostgreSQL admission");
    let parent=std::path::PathBuf::from(std::env::var_os("ATLAS_HOME").expect("explicit private test home"));
    let home=tempfile::Builder::new().prefix("writer-stage-plan-").tempdir_in(parent).unwrap();
    std::fs::DirBuilder::new().mode(0o700).create(home.path().join("tmp")).unwrap();
    let mut config=std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(home.path().join("config.json")).unwrap();
    config.write_all(b"{}").unwrap();drop(config);
    let log=std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(home.path().join("child.private.log")).unwrap();
    let mut child=Command::new(std::env::current_exe().unwrap()).args(["--ignored","--exact",SELECTOR,"--nocapture"])
        .env_clear().env(CHILD,"1").env("ATLAS_PG_TEST_URL",&schema.url).env("ATLAS_WRITER_STAGE_PLAN_SCHEMA",&schema.name)
        .env("ATLAS_HOME",home.path()).env("HOME",home.path()).env("TMPDIR",home.path().join("tmp"))
        .env("ATLAS_NO_KEYRING","1").env("ATLAS_SAB_DIR",home.path().join("no-sab"))
        .stdin(Stdio::null()).stdout(Stdio::from(log.try_clone().unwrap())).stderr(Stdio::from(log)).spawn().unwrap();
    let until=Instant::now()+Duration::from_secs(120);
    let status=loop {
        if let Some(status)=child.try_wait().unwrap(){break status;}
        if Instant::now()>=until {child.kill().unwrap();child.wait().unwrap();let _=home.keep();panic!("closed writer plan child exceeded 120-second admission");}
        std::thread::sleep(Duration::from_millis(20));
    };
    let log=std::fs::read(home.path().join("child.private.log")).unwrap();
    eprintln!("closed writer plan child stderr sha256={:x}",Sha256::digest(&log));
    let report=home.path().join("plans.private.json");
    if !status.success() {let _=home.keep();panic!("closed writer plan child failed; retained private diagnostic under admitted home");}
    let bytes=std::fs::read(report).expect("complete isolated writer plan report");
    let report:serde_json::Value=serde_json::from_slice(&bytes).unwrap();
    assert_eq!(report["variants"].as_array().unwrap().len(),8);
    let semantics:serde_json::Value=serde_json::from_slice(&std::fs::read(home.path().join("semantics.private.json")).unwrap()).unwrap();
    assert_eq!(semantics["ordinary_save_parts"],1700);
    let _=home.keep();
    eprintln!("closed writer plan report sha256={:x}; eight bounded probes and genuine save/replay parity passed",Sha256::digest(bytes));
}

fn fixture(seed:bool)->Vec<Release> {
    (0..100).map(|r|Release {
        name:format!("Plan.Fixture.{r}"),collection:Some(format!("plan-collection-{r}")),group:"alt.binaries.test".into(),
        date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),poster:"fixture".into(),
        articles:(0..if seed {16}else{20}).map(|a| {
            let mid=if seed||a<2 {format!("<seed-{r}-{a}@fixture.invalid>")}
                else {format!("<new-{r}-{}@fixture.invalid>",if a==3 {2}else{a})};
            Article {filename:if !seed&&a==19 {None}else{Some(format!("part-{a}.rar"))},message_id:mid,
                subject:format!("fixture-{r}-{a} yEnc (1/2)"),part:Some(1),total_parts:Some(2),bytes:17,..Default::default()}
        }).collect(),..Default::default()
    }).collect()
}

fn snapshot(client:&mut Client)->serde_json::Value {
    let row=client.query_one("select (select count(*) from releases),(select count(*) from files),(select count(*) from segments),
        (select coalesce(sum(size),0)::bigint from releases),(select coalesce(sum(parts),0)::bigint from releases),
        (select coalesce(sum(generation),0)::bigint from pp_queue),(select count(*) from segment_nzb_metadata)",&[]).unwrap();
    serde_json::json!((0..7).map(|n|row.get::<_,i64>(n)).collect::<Vec<_>>())
}

fn run() {
    let pool=crate::pg::Pool::new(&crate::pg::StorageConfig {backend:crate::pg::Backend::Postgres,
        url:std::env::var("ATLAS_PG_TEST_URL").unwrap(),schema:std::env::var("ATLAS_WRITER_STAGE_PLAN_SCHEMA").unwrap(),
        max_connections:1,allow_plaintext:true}).unwrap();
    let mut client=pool.get().unwrap();
    client.batch_execute("set statement_timeout='3s';set lock_timeout='250ms'").unwrap();
    // Fixed background cardinality exercises file-map planning without millions
    // of segment writes. Native migrations, constraints and triggers stay live.
    for page in 0..16i64 {
        client.execute("insert into releases(group_name,is_collection,identity_key,name,poster,posted_date,is_obfuscated)
            select 'alt.binaries.plan-background',false,sha256(convert_to('background-'||n,'UTF8')),'background-'||n,'fixture','',false
            from generate_series($1::bigint,$2::bigint) n",&[&(page*512+1),&((page+1)*512)]).unwrap();
    }
    for page in 0..16i64 {
        client.execute("insert into files(release_id,filename_key,filename)
            select r.id,sha256(convert_to('background-file-'||n,'UTF8')),'background-file-'||n
            from releases r cross join generate_series(1,20) n
            where r.group_name='alt.binaries.plan-background' and r.id>$1 and r.id<=$2",&[&(page*512),&((page+1)*512)]).unwrap();
    }
    let seed=fixture(true);assert_eq!(save(&mut client,&seed,100).unwrap(),SaveStats {releases:100,files:1600,parts:1600});
    client.batch_execute("analyze releases;analyze files;analyze segments").unwrap();
    let frozen=snapshot(&mut client);let input=fixture(false);
    let batch:Vec<_>=input.iter().flat_map(|r|r.articles.iter().map(move|a|Incoming {release:r,release_key:identity(r).1,
        original_name_key:Sha256::digest(r.name.as_bytes()).into(),posted_at:crate::dates::article_timestamp(&r.date),
        posted_clock:crate::dates::naive_timestamp(&r.date),article:Some(a)})).collect();
    assert_eq!(batch.len(),2000);assert!(batch.iter().map(Incoming::approx_bytes).sum::<usize>()<MAX_BATCH_BYTES);
    let mut reports=Vec::new();
    for (pass,order) in [[0usize,1,2,3],[3usize,2,1,0]].into_iter().enumerate() {
        for variant in order {
            let mut tx=client.transaction().unwrap();crate::pg::history_gate::native(&mut tx).unwrap();
            // Recreate, not just TRUNCATE: a previous ANALYZE must not seed the
            // next unanalysed control with retained temporary-column statistics.
            tx.batch_execute("drop table if exists pg_temp.atlas_release_input").unwrap();
            tx.batch_execute(CREATE_STAGE).unwrap();
            let key=timing::key(&mut tx).unwrap();copy_input(&mut tx,&batch,Some(key)).unwrap();
            if variant&2!=0 {tx.batch_execute("create unique index on pg_temp.atlas_release_input(ordinal)").unwrap();}
            if variant&1!=0 {tx.batch_execute("analyze pg_temp.atlas_release_input").unwrap();}
            assert_eq!(tx.query_one(WRITE_RELEASES,&[&"alt.binaries.test"]).unwrap().get::<_,i64>(0),0);
            let started=Instant::now();
            let plan=tx.query(&format!("explain (analyze,buffers,format text,timing off) {WRITE_ARTICLES}"),&[&"alt.binaries.test"]);
            match plan {
                Ok(rows)=>{
                    let plan:Vec<String>=rows.iter().map(|r|r.get(0)).collect();
                    let accepted=tx.query_one("select count(*) from pg_temp.atlas_release_input where file_id is not null",&[]).unwrap().get::<_,i64>(0);
                    assert_eq!(accepted,1700,"same MID winners for every plan variant");
                    assert_eq!(tx.query_one("select count(*) from pg_temp.atlas_release_input where ordinal%20 in(0,1,3) and file_id is not null",&[]).unwrap().get::<_,i64>(0),0,"existing MIDs and the later duplicate ordinal must lose");
                    assert_eq!(tx.query_one("select count(*) from files",&[]).unwrap().get::<_,i64>(0),165840,"each plan must add exactly four hundred files");
                    let sparse=tx.query_one("select count(*) from segment_nzb_metadata",&[]).unwrap().get::<_,i64>(0);assert_eq!(sparse,100);
                    reports.push(serde_json::json!({"pass":pass,"variant":variant,"elapsed_ms":started.elapsed().as_secs_f64()*1000.,"accepted":accepted,"plan":plan}));
                },
                Err(error)=>{
                    let code=error.code().map(|code|code.code());
                    assert_eq!(code,Some("57014"),"only the fixed local statement budget can defer an experimental variant");
                    assert_eq!(error.as_db_error().map(|db|db.message()),Some("canceling statement due to statement timeout"));
                    reports.push(serde_json::json!({"pass":pass,"variant":variant,"timeout":true,"elapsed_ms":started.elapsed().as_secs_f64()*1000.}));
                }
            }
            tx.rollback().unwrap();assert_eq!(snapshot(&mut client),frozen,"probe writes must all roll back");
        }
    }
    // Preserve all plans even if the subsequent unchanged writer exceeds its
    // statement budget. TEXT avoids enabling an extra JSON driver feature;
    // PostgreSQL still supplies every node, loop, buffer, JIT and trigger line.
    write_report("plans.private.json",&serde_json::json!({"input_rows":2000,"background_releases":8192,
        "background_files":163840,"format":"postgresql explain analyze buffers text timing off","variants":reports}));
    assert!(reports.iter().any(|report|report["accepted"]==1700),"at least one exact writer probe must finish within admission");
    // Exercise the complete real path too: timings, completeness and queue
    // generation are deliberately not substituted by the diagnostic SQL path.
    let saved=save(&mut client,&input,200).unwrap();assert_eq!(saved,SaveStats {releases:0,files:400,parts:1700});
    let after=snapshot(&mut client);assert_eq!(save(&mut client,&input,300).unwrap(),SaveStats::default());
    assert_eq!(snapshot(&mut client),after,"replayed MIDs cannot change totals or queue generations");
    let row=client.query_one("select count(*),sum(r.parts)::bigint,sum(r.size)::bigint,sum(q.generation)::bigint
        from releases r join pp_queue q on q.release_id=r.id where r.group_name='alt.binaries.test'",&[]).unwrap();
    assert_eq!((row.get::<_,i64>(0),row.get::<_,i64>(1),row.get::<_,i64>(2),row.get::<_,i64>(3)),(100,3300,56100,200));
    write_report("semantics.private.json",&serde_json::json!({"ordinary_save_parts":saved.parts,"ordinary_save_files":saved.files,"replay_unchanged":true,"final_snapshot":after}));
}

fn write_report(name:&str,report:&serde_json::Value) {
    let mut file=std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
        .open(crate::paths::app_dir().join(name)).unwrap();
    file.write_all(&serde_json::to_vec_pretty(&report).unwrap()).unwrap();file.sync_all().unwrap();
    std::fs::File::open(crate::paths::app_dir()).unwrap().sync_all().unwrap();
}
