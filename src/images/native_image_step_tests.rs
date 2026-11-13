//! Actual native StepCtx with an unselected asset grant must not fall through
//! to the global legacy image folder or fetch a body that cannot be admitted.
use super::*;
#[test]
#[ignore="explicit isolated PG; real native image inventory and missing-grant refusal"]
fn native_images_missing_grant_is_transient_before_body_or_legacy_keep() {
    // The native cache/asset scope is intentionally sticky per worker thread;
    // keep it off the test harness's reusable thread.
    std::thread::spawn(|| {
        let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");let runtime=crate::pg::stage_queue_tests::runtime(&schema);
        let id=runtime.owner.with(|client| {
            let saved=crate::pg::release_writer::save(client,&[crate::parser::Release {name:"No.Image.Grant.Release".into(),poster:"poster".into(),group:"alt.binaries.test".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
                articles:vec![crate::parser::Article {filename:Some("cover.gif".into()),subject:"cover.gif yEnc (1/1)".into(),message_id:"no-grant@fixture.invalid".into(),part:Some(1),total_parts:Some(1),bytes:43,..Default::default()}],..Default::default()}],-100000)?;
            assert_eq!((saved.releases,saved.files,saved.parts),(1,1,1));Ok(client.query_one("select id from releases",&[])?.get::<_,i64>(0))
        }).unwrap();
        let reads=crate::pg::stage_reads::StepReads::for_owner(&runtime.owner).unwrap();let snapshot=reads.snapshot(id).unwrap().unwrap();
        let config=crate::config::Config::default();let step=ImagesStep::default();assert!(step.enabled(&config));
        let handle=tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();let stats=std::sync::Arc::new(crate::postprocess::stats::Stats::default());
        let bodies=crate::postprocess::bodies::BodyFetcher::new(std::sync::Arc::new(crate::nntp::Pool::new(&[])),handle.handle().clone(),runtime.owner.stop.clone(),config.postprocess.clone(),stats.clone());
        let scope=crate::pg::cache_actor::NativeCacheScope::bind_with_assets(&runtime.owner,None).unwrap();
        let mut ctx=crate::postprocess::step::StepCtx {release:snapshot.info,data:crate::postprocess::data::StepData::Native(&reads),facts:Vec::new(),bodies:&bodies,used_bodies:0,
            deadline:std::time::Instant::now()+std::time::Duration::from_secs(5),lane_can_fetch:true,crcs:Vec::new(),pictures:false};
        assert!(matches!(step.run(&mut ctx),Err(StepError::Transient(message)) if message.contains("native image namespace")));
        assert_eq!(stats.bodies.load(std::sync::atomic::Ordering::Relaxed),0);runtime.owner.with(|client|{assert_eq!(client.query_one("select count(*) from release_images",&[])?.get::<_,i64>(0),0);Ok(())}).unwrap();
        drop(scope);runtime.owner.close();
    }).join().unwrap();
}
