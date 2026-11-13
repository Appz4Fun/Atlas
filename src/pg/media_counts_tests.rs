//! Real accepted overview headers, native writer, original owner, and scheduler reads.
use super::*;
use crate::{config::{Config,HeaderStoreConfig},pg::{self,indexer_runtime::Runtime},parser::Release};

fn runtime(schema:&pg::testing::Schema)->(Runtime,HeaderStoreConfig,tempfile::TempDir) {
    let root=tempfile::tempdir().unwrap();let headers=HeaderStoreConfig {enabled:true,dir:root.path().to_str().unwrap().into()};
    let mut config=Config::default();config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;
    config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;
    let runtime=Runtime::open(&config,pg::StorageConfig {backend:pg::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:1,allow_plaintext:true},headers.clone(),&[],&mut None).unwrap();
    (runtime,headers,root)
}
fn release(group:&str,name:&str,n:usize,bytes:i64)->Release {
    let overview=crate::nntp::Overview {number:n as u64+1,subject:format!("\"{name}.{n}.mkv\" yEnc (1/1)"),from:"Poster <poster@fixture.test>".into(),
        date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),message_id:format!("<media-count-{group}-{n}@fixture.test>"),references:String::new(),bytes,lines:1,xref_groups:Some(1)};
    let grouped=crate::collections::group_articles(group,crate::nntp::headers_to_articles(vec![overview]));assert_eq!(grouped.len(),1);
    let mut release=grouped.into_values().next().unwrap();release.group=group.into();release.poster=release.articles[0].author.clone();release.date=release.articles[0].date.clone();
    release.complete=crate::parser::is_complete(&release.articles);release
}
fn observed(client:&mut impl postgres::GenericClient)->Vec<(String,i64)> {
    client.query("select grp,releases from backfill_media order by grp",&[]).unwrap().into_iter().map(|row|(row.get(0),row.get(1))).collect()
}
fn finish(service:&mut Service,clock:Instant) {
    for _ in 0..100 {match service.tick_at(clock).unwrap() {Tick::Stored=>return,Tick::Progress=>{},Tick::Idle=>panic!("count was deferred before publication")}}
    panic!("native media scan failed to finish bounded fixture")
}

#[test]
#[ignore="requires explicit isolated owned PostgreSQL and genuine accepted overview headers"]
fn native_media_counts_publish_complete_pages_and_preserve_literal_scheduler_policy() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");let (runtime,headers,_root)=runtime(&schema);
    let mut input=Vec::new();
    for n in 0..22 {input.push(release("alt.binaries.remux","Movie.REMUX",n,4<<30));}
    for n in 0..20 {input.push(release("alt.binaries.uhd","Movie.2160P",n,4<<30));}
    for n in 0..19 {input.push(release("alt.binaries.few","Movie.DoVi",n,4<<30));}
    input.extend([release("alt.binaries.dv","Movie.DV.Example",0,4<<30),release("alt.binaries.small","Movie.2160p",0,(4<<30)-1),
        release("alt.binaries.plain","Movie.1080p",0,4<<30),release("alt.binaries.display","Plain.Movie",0,4<<30),release("alt.binaries.overridden","Movie.2160p",0,4<<30)]);
    for n in 0..PAGE_ROWS {input.push(release("alt.binaries.filler","Plain.File",n,1));}
    runtime.owner.with(|client| {
        assert_eq!(pg::release_writer::save(client,&input,1)?.releases,input.len() as u64);
        assert_eq!(pg::release_writer::save(client,&input,2)?.parts,0,"actual MID replay cannot inflate a release count");
        client.execute("update releases set display_name='Presented.REMUX' where group_name='alt.binaries.display'",&[])?;
        client.execute("update releases set display_name='Presented.Plain' where group_name='alt.binaries.overridden'",&[])?;
        client.execute("insert into backfill_media(grp,releases) values('previous.completed',99)",&[])?;Ok(())
    }).unwrap();
    let mut service=Service::new(&runtime.owner,runtime.db.clone(),&headers).unwrap();let clock=Instant::now();
    assert!(matches!(service.tick_at(clock).unwrap(),Tick::Progress));
    runtime.owner.with(|client| {assert_eq!(observed(client),vec![("previous.completed".into(),99)]);
        pg::release_writer::save(client,&[release("alt.binaries.later","Movie.2160p",0,4<<30)],3)?;Ok(())}).unwrap();
    finish(&mut service,clock);
    runtime.owner.with(|client| {
        assert_eq!(observed(client),vec![("alt.binaries.display".into(),1),("alt.binaries.dv".into(),1),("alt.binaries.few".into(),19),("alt.binaries.remux".into(),22),("alt.binaries.uhd".into(),20)]);
        let sql=pg::sql::Pg::new(client);assert_eq!(crate::chunks::media_groups(&sql)?,vec!["alt.binaries.remux","alt.binaries.uhd"]);Ok(())
    }).unwrap();
    assert!(matches!(service.tick_at(clock+Duration::from_secs(86399)).unwrap(),Tick::Idle));
    finish(&mut service,clock+Duration::from_secs(86401));
    runtime.owner.with(|client| {
        assert_eq!(observed(client),vec![("alt.binaries.display".into(),1),("alt.binaries.dv".into(),1),("alt.binaries.few".into(),19),("alt.binaries.later".into(),1),("alt.binaries.remux".into(),22),("alt.binaries.uhd".into(),20)]);Ok(())
    }).unwrap();
    assert_eq!(runtime.owner.failure(),None);runtime.owner.close();
}

#[test]
#[ignore="requires explicit isolated owned PostgreSQL; intentional stop must retain the prior completed tally"]
fn native_media_count_stop_between_pages_never_publishes_partial_or_success_zero() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");let (runtime,headers,_root)=runtime(&schema);
    runtime.owner.with(|client| {pg::release_writer::save(client,&(0..=PAGE_ROWS).map(|n|release("alt.binaries.uhd","Movie.2160p",n,4<<30)).collect::<Vec<_>>(),1)?;
        client.execute("insert into backfill_media(grp,releases) values('prior',42)",&[])?;Ok(())}).unwrap();
    let mut observer=schema.pool.get().unwrap();let mut service=Service::new(&runtime.owner,runtime.db.clone(),&headers).unwrap();
    assert!(matches!(service.tick().unwrap(),Tick::Progress));runtime.owner.stop.store(true,Ordering::Release);
    let error=service.tick().err().expect("stop cannot complete a count");assert!(pg::outage::is_attempt_ended(&error));assert_eq!(runtime.owner.failure(),None);
    assert_eq!(observed(&mut *observer),vec![("prior".into(),42)]);runtime.owner.close();
}

#[test]
#[ignore="requires explicit isolated owned PostgreSQL; native replacement commit refusal"]
fn native_media_count_commit_failure_rolls_back_clear_and_preserves_driver_cause() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");let (runtime,headers,_root)=runtime(&schema);
    runtime.owner.with(|client| {pg::release_writer::save(client,&[release("alt.binaries.uhd","Movie.2160p",0,4<<30)],1)?;
        client.execute("insert into backfill_media(grp,releases) values('prior',42)",&[])?;
        client.batch_execute("alter table backfill_media add constraint refuse_new_count check(grp='prior') not valid")?;Ok(())}).unwrap();
    let mut observer=schema.pool.get().unwrap();let mut service=Service::new(&runtime.owner,runtime.db.clone(),&headers).unwrap();
    let error=service.tick().err().expect("failed replacement is not an empty completed count");
    assert!(error.chain().any(|cause|cause.downcast_ref::<postgres::Error>().is_some()));assert_eq!(runtime.owner.failure(),Some(pg::outage::Failure::Fatal));
    assert_eq!(observed(&mut *observer),vec![("prior".into(),42)]);runtime.owner.close();
}

#[test]
#[ignore="requires explicit isolated owned PostgreSQL; native empty is a genuine successful zero"]
fn native_media_count_empty_catalog_replaces_prior_tally_and_old_owner_cannot_publish() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");let (runtime,headers,_root)=runtime(&schema);
    runtime.owner.with(|client| {client.execute("insert into backfill_media(grp,releases) values('prior',42)",&[])?;Ok(())}).unwrap();
    let mut service=Service::new(&runtime.owner,runtime.db.clone(),&headers).unwrap();assert!(matches!(service.tick().unwrap(),Tick::Stored));
    runtime.owner.with(|client| {assert!(observed(client).is_empty());Ok(())}).unwrap();runtime.owner.close();
    assert!(service.tick().is_err(),"closed original owner is never successful empty");
}

#[test]
#[ignore="requires explicit isolated PostgreSQL; terminates only the fixture's real canonical owner backend"]
fn native_media_count_owner_loss_during_final_replace_rolls_back_and_stays_unavailable() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");let (runtime,headers,_root)=runtime(&schema);
    let pid=runtime.owner.with(|client| {pg::release_writer::save(client,&[release("alt.binaries.uhd","Movie.2160p",0,4<<30)],1)?;
        client.execute("insert into backfill_media(grp,releases) values('prior',42)",&[])?;Ok(client.query_one("select pg_backend_pid()",&[])?.get::<_,i32>(0))}).unwrap();
    let mut observer=schema.pool.get().unwrap();let mut held=observer.transaction().unwrap();held.batch_execute("lock table backfill_media in access exclusive mode").unwrap();
    let mut service=Service::new(&runtime.owner,runtime.db.clone(),&headers).unwrap();let worker=std::thread::spawn(move||service.tick());
    let until=Instant::now()+Duration::from_secs(3);
    loop {
        held.query_one("select pg_stat_clear_snapshot()",&[]).unwrap();
        if held.query_one("select exists(select 1 from pg_stat_activity where pid=$1 and wait_event_type='Lock')",&[&pid]).unwrap().get::<_,bool>(0) {break}
        assert!(Instant::now()<until,"actual media replacement never waited on its table lock");std::thread::sleep(Duration::from_millis(5));
    }
    assert!(held.query_one("select pg_terminate_backend($1)",&[&pid]).unwrap().get::<_,bool>(0));held.rollback().unwrap();
    let error=worker.join().unwrap().err().expect("lost original backend cannot report successful completion");
    assert!(error.chain().any(|cause|cause.downcast_ref::<postgres::Error>().is_some()));assert_eq!(pg::outage::classify(&error),pg::outage::Failure::Unavailable);
    assert_eq!(runtime.owner.failure(),Some(pg::outage::Failure::Unavailable));assert_eq!(observed(&mut *observer),vec![("prior".into(),42)]);runtime.owner.close();
}

#[test]
#[ignore="requires explicit isolated owned PostgreSQL; original Db and header-root admission"]
fn native_media_count_refuses_a_different_header_root_before_counting() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");let (runtime,headers,_root)=runtime(&schema);
    let mut changed=headers.clone();changed.dir.push_str("-different");
    assert!(Service::new(&runtime.owner,runtime.db.clone(),&changed).is_err());
    runtime.owner.with(|client| {assert!(observed(client).is_empty());Ok(())}).unwrap();runtime.owner.close();
}
