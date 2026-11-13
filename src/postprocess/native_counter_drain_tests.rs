//! Mandatory real PG controls. Every selector runs in a fresh owned PG-home child.
use super::*;
use crate::{pg,postprocess::{native::{Steps,workers},step::{Step,StepId,Needs,StepCtx,Outcome,StepError,mask}}};
use std::sync::{atomic::AtomicBool,mpsc};

fn runtime()->pg::indexer_runtime::Runtime {
    assert_eq!(std::env::var("ATLAS_NATIVE_COUNTER_DRAIN_OWNED").as_deref(),Ok("1"));
    assert!(std::env::var_os("ATLAS_HOME").is_some());
    let storage=crate::config::checked_storage_config().expect("explicit isolated storage");
    let mut cfg=crate::config::Config::default();cfg.postprocess.enabled=false;cfg.predb.enabled=false;cfg.spots.enabled=false;cfg.inferred_parts=false;
    cfg.media.tvmaze.enabled=false;cfg.media.allocine.enabled=false;cfg.media.details_per_minute=0;
    pg::indexer_runtime::Runtime::open(&cfg,storage,crate::config::HeaderStoreConfig {enabled:true,..Default::default()},&[],&mut None).unwrap()
}
struct Held {entered:mpsc::Sender<()>,release:Mutex<mpsc::Receiver<()>>}
impl Step for Held {
    fn id(&self)->StepId {StepId(2)} fn name(&self)->&'static str {"real-final-counter-event"}
    fn rank(&self)->u16 {1} fn needs(&self)->Needs {Needs::Database}
    fn run(&self,_:&mut StepCtx<'_>)->std::result::Result<Outcome,StepError> {
        self.entered.send(()).unwrap();self.release.lock().unwrap().recv_timeout(Duration::from_secs(10)).unwrap();
        Ok(Outcome {matched:true,..Default::default()})
    }
}

#[test]
#[ignore="Root-only owned configured PostgreSQL home; actual worker/held Step/join"]
fn actual_worker_last_event_is_unavailable_before_join_and_drained_once_after_stop() {
    worker_case(false);
}

#[test]
#[ignore="Root-only owned configured PostgreSQL home; genuine original-owner failure during held worker"]
fn actual_worker_failure_retains_final_events_without_reviving_read_gauges() {worker_case(true);}

fn worker_case(failed:bool) {
    let runtime=runtime();pg::stage_queue_tests::push(&runtime);
    let handle=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut cfg=crate::config::Config::default();cfg.predb.enabled=false;cfg.spots.enabled=false;cfg.inferred_parts=false;
    cfg.media.tvmaze.enabled=false;cfg.media.allocine.enabled=false;cfg.media.details_per_minute=0;cfg.postprocess.workers=1;
    let mut runner=Runner::new(&runtime.owner,&cfg,Arc::new(crate::nntp::Pool::new(&[])),handle.handle().clone()).unwrap();
    let (entered_tx,entered_rx)=mpsc::channel();let (release_tx,release_rx)=mpsc::channel();
    let step:Arc<dyn Step>=Arc::new(Held {entered:entered_tx,release:Mutex::new(release_rx)});let bit=mask(step.id());
    runner.steps=Steps {ordered:vec![step],enabled:bit,database:bit,network:0,on_growth:0,stamps:vec![(2,bit,0)]};
    let runner=Arc::new(runner);let drain=Arc::new(Drain::new(&runtime.owner).unwrap());
    let stage=workers::Workers::start_runner_with_drain(runner.clone(),runtime.db.clone(),runtime.configured_headers(),drain.clone()).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(drain.take_after_join(&runtime.owner).unwrap().is_none());
    assert_eq!(runner.stats.steps_done.load(Ordering::Relaxed),0);
    assert!(drain.retain_stage(&runner,Arc::new(LaneCounters::new(bit as u64).unwrap())).is_err(),"duplicate registration must not replace original counters");
    if failed {runtime.owner.fail(pg::outage::Failure::Unavailable);}
    let (joined_tx,joined_rx)=mpsc::channel();
    let thread=std::thread::spawn(move||{let result=stage.finish();joined_tx.send(result).unwrap();});
    let end=Instant::now()+Duration::from_secs(5);
    while !runtime.owner.stop.load(Ordering::Acquire) {assert!(Instant::now()<end);std::thread::yield_now();}
    assert!(joined_rx.recv_timeout(Duration::from_millis(20)).is_err());
    assert!(drain.take_after_join(&runtime.owner).unwrap().is_none());
    release_tx.send(()).unwrap();let result=joined_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    if !failed {result.unwrap();}thread.join().unwrap();
    // Same explicit acknowledgment as the async bridge, after actual returned join.
    drain.joined();
    let finished=drain.take_after_join(&runtime.owner).unwrap().expect("joined actual events");
    assert_eq!(finished.stage.as_ref().unwrap()["steps_done"],1);
    assert_eq!(finished.stage.as_ref().unwrap()["matched"],1);
    assert!(finished.stage.as_ref().unwrap()["queue_due"].is_null());
    assert!(snapshot().is_none());assert!(drain.take_after_join(&runtime.owner).unwrap().is_none());
    let mut stats=crate::bg_indexer::Stats::new();stats.retain_final_services(finished,101);stats.write("","backfill",false,true);
    let file:Value=serde_json::from_slice(&std::fs::read(crate::paths::stats_file()).unwrap()).unwrap();
    assert_eq!(file["postprocess"]["steps_done"],1);assert_eq!(runtime.owner.failure(),if failed {Some(pg::outage::Failure::Unavailable)}else{None});
    runtime.owner.close();
}

#[test]
#[ignore="Root-only owned configured PostgreSQL home; actual PreDB commit followed by parse failure"]
fn actual_predb_import_survives_a_later_failed_page_and_stopped_owner() {
    struct Pages(AtomicBool);
    impl crate::predb::poll::Fetch for Pages {
        fn get(&self,_:&str)->std::result::Result<crate::predb::poll::HttpReply,String> {
            let body=if !self.0.swap(true,Ordering::AcqRel) {r#"{"status":"success","data":{"rows":[{"name":"Real.Retained.Drain.2026.1080p-GRP"}]}}"#} else {"malformed"};
            Ok(crate::predb::poll::HttpReply {status:200,retry_after:None,rate_reset:None,body:body.into()})
        }
    }
    let runtime=runtime();let drain=Drain::new(&runtime.owner).unwrap();
    let service=pg::service_cache::predb::Service::new(&runtime.owner).unwrap();
    drain.retain_predb(&runtime.owner,service.counters.clone()).unwrap();
    let cfg=crate::config::PredbConfig {enabled:true,sources:vec!["predb_club".into()],max_pages_per_poll:2,history_pages:0,request_delay_ms:0,..Default::default()};
    let fetch=Pages(AtomicBool::new(false));let end=Instant::now()+Duration::from_secs(10);
    loop {
        match service.tick(&cfg,&fetch,chrono::Utc::now().timestamp(),&|_|true).unwrap() {
            pg::service_cache::predb::Tick::Pending=>assert!(Instant::now()<end),
            pg::service_cache::predb::Tick::Polled(crate::predb::poll::PollEnd::Failed(_))=>break,
            _=>panic!("actual second page must fail after first page committed"),
        }
    }
    runtime.owner.with(|client|{assert_eq!(client.query_one("select count(*) from ext_predb_titles where title='Real.Retained.Drain.2026.1080p-GRP'",&[])?.get::<_,i64>(0),1);Ok(())}).unwrap();
    runtime.owner.with(|client|pg::release_writer::save(client,&[crate::parser::Release {
        name:"Real.Retained.Drain.2026.1080p-GRP".into(),group:"example.group".into(),articles:vec![crate::parser::Article {
            filename:Some("retained.mkv".into()),message_id:"<retained-drain@test>".into(),part:Some(1),total_parts:Some(1),bytes:1,..Default::default()}],..Default::default()}],1).map(|_|())).unwrap();
    let now=chrono::Utc::now().timestamp()+crate::predb::late::LATE_DELAY_SECS+1;
    let page=service.late_tick(now,32).unwrap();assert_eq!((page.titles,page.reruns),(1,1));
    // This genuine later public admission failure must not erase the already
    // committed title/rerun page, or count one failed call twice.
    assert!(service.late_tick(now,0).is_err());
    runtime.owner.stop.store(true,Ordering::Release);drain.joined();
    let final_read=drain.take_after_join(&runtime.owner).unwrap().unwrap();
    let predb=final_read.predb.unwrap();assert_eq!(predb["sources"][0]["events"]["inserted"],1);assert!(predb["sources"][0]["inserted"].is_null());assert_eq!(predb["errors"],1);
    assert_eq!(predb["late"]["titles"],1);assert_eq!(predb["late"]["reruns"],1);assert_eq!(predb["late"]["errors"],1);
    assert!(predb["titles"].is_null());assert!(predb["file_bytes"].is_null());assert_eq!(runtime.owner.failure(),None);
    runtime.owner.close();
}

#[test]
#[ignore="Root-only two isolated real PostgreSQL owners; no counterfeit identity"]
fn wrong_owner_and_unacknowledged_coordinator_panic_cannot_consume_final_counters() {
    let runtime=runtime();let drain=Drain::new(&runtime.owner).unwrap();
    let panic=std::thread::spawn(||panic!("coordinator panic does not prove descendant drain"));assert!(panic.join().is_err());
    assert!(drain.take_after_join(&runtime.owner).unwrap().is_none());
    let schema=pg::testing::Schema::new().expect("second explicitly configured isolated schema required");
    let other=pg::stage_queue_tests::runtime(&schema);
    assert!(drain.take_after_join(&other.owner).is_err());
    other.owner.close();runtime.owner.close();
}

#[test]
#[ignore="Root-only owned configured PostgreSQL home; actual65-candidate bounded late page"]
fn actual_late_page_budget_requires_a_known_unexamined_candidate() {
    const TITLE:&str="Real.Bounded.Predb.Drain.2026.1080p-GRP";
    let runtime=runtime();let service=pg::service_cache::predb::Service::new(&runtime.owner).unwrap();
    struct NoFetch;
    impl crate::predb::poll::Fetch for NoFetch {fn get(&self,_:&str)->std::result::Result<crate::predb::poll::HttpReply,String> {panic!("empty selected sources must never fetch")}}
    let cfg=crate::config::PredbConfig {enabled:true,sources:vec![],..Default::default()};
    let end=Instant::now()+Duration::from_secs(10);
    while matches!(service.tick(&cfg,&NoFetch,1,&|_|true).unwrap(),pg::service_cache::predb::Tick::Pending) {assert!(Instant::now()<end);}
    runtime.owner.with(|client| {
        let releases:Vec<_>=(0..65).map(|id|crate::parser::Release {name:format!("Unrelated.Collision.{id}.2026-GRP"),group:"example.group".into(),
            articles:vec![crate::parser::Article {filename:Some(format!("file{id}.bin")),message_id:format!("<candidate{id}@drain.test>"),
                part:Some(1),total_parts:Some(1),bytes:1,..Default::default()}],..Default::default()}).collect();
        pg::release_writer::save(client,&releases,1)?;
        let mut tx=client.transaction()?;tx.execute("insert into ext_predb_feeds(source,key) values(1,'predb_club')",&[])?;
        assert!(matches!(pg::predb_cache::import_page_in(&mut tx,1,&[crate::predb::import::Entry {title:TITLE.into(),..Default::default()}],1,None)?,pg::predb_cache::ImportStatus::Applied{..}));
        tx.commit()?;Ok(())
    }).unwrap();
    // Build actual native name keys before the title is old enough to scan.
    for _ in 0..3 {service.late_tick(0,32).unwrap();}
    runtime.owner.with(|client| {
        assert_eq!(client.query_one("select count(*) from releases",&[])?.get::<_,i64>(0),65);
        assert!(!client.query_one("select exists(select 1 from predb_name_pending)",&[])?.get::<_,bool>(0));
        // A genuine collision fixture nominates65 nonmatching releases. Actual
        // evidence must reject them; no readiness/settlement row is fabricated.
        client.execute("insert into predb_name_keys(key,release_id) select $1,id from releases on conflict do nothing",&[&crate::predb::keys::name_key(TITLE)])?;Ok(())
    }).unwrap();
    let now=chrono::Utc::now().timestamp();let page=service.late_tick(now,32).unwrap();
    assert!(page.pending&&page.budget_exhausted);assert_eq!(page.titles,0);
    assert_eq!(service.counters.snapshot()["late"]["native_budget_pages"],1);
    let page=service.late_tick(now,32).unwrap();assert_eq!(page.titles,1);assert!(!page.budget_exhausted);
    let cfg=crate::config::PredbConfig {enabled:true,late_per_second:1,..Default::default()};
    service.maintenance(&cfg,now).unwrap();
    assert_eq!(service.counters.snapshot()["late"]["native_budget_pages"],1,"completed/empty work is not a new exhausted page");
    assert!(service.counters.snapshot()["late"]["out_of_budget"].is_null(),"legacy time-budget semantics are unavailable");
    let actual=service.counters.snapshot();crate::metrics::report_service_counters(None,Some(&actual),202);
    let metrics=crate::metrics::render();
    assert!(metrics.contains("atlas_predb_late_total{kind=\"native_budget_pages\"} 1\n"));
    assert!(!metrics.contains("atlas_predb_late_total{kind=\"out_of_budget\"}"),"unobservable legacy timing is not a fresh zero counter");
    runtime.owner.close();
}
