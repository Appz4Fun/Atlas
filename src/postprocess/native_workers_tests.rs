//! Production worker loops with actual owner/writer/Step fixtures.
use super::*;
use std::{sync::{Mutex,mpsc},time::{Duration,Instant}};

fn fixture(runtime:&crate::pg::indexer_runtime::Runtime,handle:tokio::runtime::Handle,step:Arc<dyn Step>)->Arc<Runner> {
    let mut config=crate::config::Config::default();config.predb.enabled=false;
    let mut runner=Runner::new(&runtime.owner,&config,Arc::new(crate::nntp::Pool::new(&[])),handle).unwrap();
    let bit=mask(step.id());let id=step.id().0;let stamp=step.config_stamp() as i64;
    runner.steps=Steps {ordered:vec![step],enabled:bit,database:bit,network:0,on_growth:0,stamps:vec![(id,bit,stamp)]};
    Arc::new(runner)
}
#[test]
fn native_worker_loop_commits_real_category_then_joins_before_owner_close() {
    crate::raw_store::native_pg_opensubtitles_cache_tests::native_worker_parent(
        crate::raw_store::native_pg_opensubtitles_cache_tests::WorkerCase::Category);
}

pub(crate) fn run_category_loop(runtime:&crate::pg::indexer_runtime::Runtime) {
    crate::pg::stage_queue_tests::push(runtime);
    let handle=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let runner=fixture(runtime,handle.handle().clone(),Arc::new(crate::category::CategoryStep::new()));let bit=runner.steps.enabled;
    let stage=workers::Workers::start_runner(runner).unwrap();
    let deadline=Instant::now()+Duration::from_secs(5);
    loop {
        if runtime.owner.with(|client|Ok(client.query_one("select exists(select 1 from pp_state where done & $1<>0)",&[&bit])?.get::<_,bool>(0))).unwrap() {break}
        assert!(Instant::now()<deadline,"actual native DB worker did not commit");std::thread::sleep(Duration::from_millis(10));
    }
    stage.finish().unwrap();assert_eq!(runtime.owner.failure(),None);
    assert!(runtime.owner.stop.load(std::sync::atomic::Ordering::Acquire));
    assert!(runtime.owner.with(|_|Ok(())).unwrap_err().is::<crate::pg::outage::AttemptEnded>());
    runtime.owner.close();
}
struct HeldStep {entered:mpsc::Sender<()>,release:Mutex<mpsc::Receiver<()>>}
impl Step for HeldStep {
    fn id(&self)->super::super::step::StepId {super::super::step::StepId(2)}
    fn name(&self)->&'static str {"held-native-drain"}
    fn rank(&self)->u16 {1}
    fn needs(&self)->Needs {Needs::Database}
    fn run(&self,_:&mut super::super::step::StepCtx<'_>)->std::result::Result<super::super::step::Outcome,super::super::step::StepError> {
        self.entered.send(()).unwrap();self.release.lock().unwrap().recv().unwrap();Err(super::super::step::StepError::Stopped)
    }
}
#[test]
fn native_worker_finish_waits_for_the_actual_step_and_keeps_its_unacked_intent() {
    crate::raw_store::native_pg_opensubtitles_cache_tests::native_worker_parent(
        crate::raw_store::native_pg_opensubtitles_cache_tests::WorkerCase::Held);
}

pub(crate) fn run_held_drain(runtime:&crate::pg::indexer_runtime::Runtime) {
    crate::pg::stage_queue_tests::push(runtime);
    let handle=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let (entered_tx,entered_rx)=mpsc::channel();let (release_tx,release_rx)=mpsc::channel();
    let runner=fixture(runtime,handle.handle().clone(),Arc::new(HeldStep {entered:entered_tx,release:Mutex::new(release_rx)}));
    let stage=workers::Workers::start_runner(runner).unwrap();entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (done_tx,done_rx)=mpsc::channel();let owner=runtime.owner.clone();
    let finish=std::thread::spawn(move|| {let answer=stage.finish();done_tx.send(answer).unwrap();});
    let deadline=Instant::now()+Duration::from_secs(5);while !owner.stop.load(std::sync::atomic::Ordering::Acquire) {assert!(Instant::now()<deadline);std::thread::yield_now();}
    assert!(matches!(done_rx.recv_timeout(Duration::from_millis(50)),Err(mpsc::RecvTimeoutError::Timeout)),"finish detached a still-running Step");
    release_tx.send(()).unwrap();done_rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();finish.join().unwrap();
    assert_eq!(owner.failure(),None);runtime.owner.close();
}

#[test]
fn native_worker_drain_preserves_the_actual_failure_over_sibling_cancellation() {
    let Some(schema)=crate::pg::testing::Schema::new() else {return};let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    let canceled:Worker=std::thread::spawn(||Err(crate::pg::outage::AttemptEnded.into()));
    let unavailable:Worker=std::thread::spawn(||Err(crate::pg::outage::SessionUnavailable.into()));
    let error=match join_workers(&runtime.owner,vec![canceled,unavailable]) {Ok(_)=>panic!("drain lost a worker failure"),Err(error)=>error};
    assert!(error.is::<crate::pg::outage::SessionUnavailable>());
    assert_eq!(runtime.owner.failure(),Some(crate::pg::outage::Failure::Unavailable));runtime.owner.close();
}

#[test]
fn native_predb_pause_continues_only_after_completed_delay_and_stops_on_owner_end() {
    let Some(schema)=crate::pg::testing::Schema::new() else {return};
    let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    assert!(workers::predb_pause(&runtime.owner,Duration::ZERO));
    runtime.owner.stop.store(true,std::sync::atomic::Ordering::Release);
    assert!(!workers::predb_pause(&runtime.owner,Duration::ZERO));
    assert!(!workers::predb_pause(&runtime.owner,Duration::from_secs(1)));
    assert_eq!(runtime.owner.failure(),None);runtime.owner.close();
}

#[test]
fn native_predb_worker_runs_with_postprocess_off_without_creating_step_state() {
    crate::raw_store::native_pg_opensubtitles_cache_tests::native_worker_parent(
        crate::raw_store::native_pg_opensubtitles_cache_tests::WorkerCase::PredbOnly);
}

pub(crate) fn run_predb_only(runtime:&crate::pg::indexer_runtime::Runtime) {
    let mut config=crate::config::Config::default();config.postprocess.enabled=false;
    config.predb.enabled=true;config.predb.sources.clear();
    let ending=Arc::new(std::sync::atomic::AtomicBool::new(false));
    let end=ending.clone();let owner=runtime.owner.clone();
    let worker=std::thread::spawn(move|| {
        let handle=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        handle.block_on(workers::run(owner,config,Arc::new(crate::nntp::Pool::new(&[])),move||end.load(std::sync::atomic::Ordering::Acquire)));
    });
    let deadline=Instant::now()+Duration::from_secs(5);
    loop {
        let ready=runtime.owner.with(|client|Ok(client.query_one("select exists(select 1 from ext_predb_key_meta) and exists(select 1 from native_runtime_observations where not stage_running and stage_instance_nonce is null)",&[])?.get::<_,bool>(0))).unwrap();
        if ready {break}
        assert!(Instant::now()<deadline,"configured PreDB-only worker did not advance the real index");
        std::thread::sleep(Duration::from_millis(10));
    }
    ending.store(true,std::sync::atomic::Ordering::Release);worker.join().unwrap();
    assert!(runtime.owner.stop.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(runtime.owner.failure(),None);runtime.owner.close();
}

#[test]
fn native_cache_maintenance_advances_while_the_actual_feed_worker_is_held() {
    crate::raw_store::native_pg_opensubtitles_cache_tests::native_worker_parent(
        crate::raw_store::native_pg_opensubtitles_cache_tests::WorkerCase::FeedHeld);
}

struct HeldFetch {entered:mpsc::Sender<()>,release:Mutex<mpsc::Receiver<()>>}
impl crate::predb::poll::Fetch for HeldFetch {
    fn get(&self,_:&str)->std::result::Result<crate::predb::poll::HttpReply,String> {
        self.entered.send(()).unwrap();self.release.lock().unwrap().recv().unwrap();
        Ok(crate::predb::poll::HttpReply {status:404,retry_after:None,rate_reset:None,body:String::new()})
    }
}
pub(crate) fn run_held_feed(runtime:&crate::pg::indexer_runtime::Runtime) {
    crate::pg::stage_queue_tests::push(runtime);
    let (entered_tx,entered_rx)=mpsc::channel();let (release_tx,release_rx)=mpsc::channel();
    crate::predb::set_test_fetch(Arc::new(HeldFetch {entered:entered_tx,release:Mutex::new(release_rx)}));
    let handle=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut runner=fixture(runtime,handle.handle().clone(),Arc::new(crate::category::CategoryStep::new()));
    let config=&mut Arc::get_mut(&mut runner).unwrap().config;
    config.predb.enabled=true;config.predb.sources=vec!["predb_club".into()];
    let stage=workers::Workers::start_runner(runner).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // Create the obligation AFTER the actual feed Fetch has entered. Finishing
    // it proves maintenance did not merely run once before the blocking call.
    runtime.owner.with(|client| {
        client.execute("insert into ext_media_titles(title_key,kind,title,state,checked_mask,checked_at,misses,fanout_done) values(919,2,'Kept Title',2,0,1,0,0)",&[])?;Ok(())
    }).unwrap();
    let deadline=Instant::now()+Duration::from_secs(5);
    while !runtime.owner.with(|client|Ok(client.query_one("select fanout_done=1 from ext_media_titles where title_key=919",&[])?.get::<_,bool>(0))).unwrap() {
        assert!(Instant::now()<deadline,"feed HTTP held native cache maintenance");std::thread::sleep(Duration::from_millis(10));
    }
    let (done_tx,done_rx)=mpsc::channel();
    let finish=std::thread::spawn(move||done_tx.send(stage.finish()).unwrap());
    assert!(matches!(done_rx.recv_timeout(Duration::from_millis(50)),Err(mpsc::RecvTimeoutError::Timeout)),"drain detached the feed worker");
    release_tx.send(()).unwrap();done_rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();finish.join().unwrap();
    assert_eq!(crate::predb::poll::REAL_REQUESTS.load(std::sync::atomic::Ordering::Relaxed),0);
    assert_eq!(runtime.owner.failure(),None);runtime.owner.close();
}

#[test]
fn native_media_fanout_fences_an_existing_visit_and_does_not_bump_a_finished_batch() {
    crate::raw_store::native_pg_opensubtitles_cache_tests::native_worker_parent(
        crate::raw_store::native_pg_opensubtitles_cache_tests::WorkerCase::FanoutFence);
}
pub(crate) fn run_fanout_fence(runtime:&crate::pg::indexer_runtime::Runtime) {
    use crate::pg::stage_queue::{Queue,Visit,Settlement};
    use crate::postprocess::{apply::Applied,step::{StepWrite,StepMutation}};
    crate::pg::stage_queue_tests::push(runtime);
    let (id,before)=runtime.owner.with(|client| {
        let id:i64=client.query_one("select id from releases",&[])?.get(0);
        client.execute("insert into media_release(release_id,kind,title_key,quality) values($1,2,911,0)",&[&id])?;
        crate::pg::service_cache::put_title_in(client,&crate::media::store::TitleRow {key:911,kind:2,title:"Kept Title".into(),state:crate::media::store::STATE_FOUND,ids:crate::media::resolve::Ids {imdb:Some(919),..Default::default()},..Default::default()})?;
        Ok((id,client.query_one("select native_work_revision from releases where id=$1",&[&id])?.get::<_,i64>(0)))
    }).unwrap();
    let queue=Queue::for_owner(&runtime.owner).unwrap();let old=queue.claim(10,1,60).unwrap().pop().unwrap();
    let result=||Applied {release_id:id,writes:vec![(4,StepWrite::for_mutation(id,StepMutation::SetCategory {category:2000}))],..Default::default()};
    assert!(crate::media::native_fanout(&runtime.owner).unwrap());
    runtime.owner.with(|client| {
        assert_eq!(client.query_one("select native_work_revision from releases where id=$1",&[&id])?.get::<_,i64>(0),before+1);
        assert_eq!(client.query_one("select imdb_id from media_release where release_id=$1",&[&id])?.get::<_,i64>(0),919);Ok(())
    }).unwrap();
    assert_eq!(queue.settle(&old,Visit::prepare(&old,result(),[1;32]).unwrap(),11).unwrap(),Settlement::Superseded);
    assert!(crate::media::native_fanout(&runtime.owner).unwrap());
    runtime.owner.with(|client| {
        assert_eq!(client.query_one("select native_work_revision from releases where id=$1",&[&id])?.get::<_,i64>(0),before+1);
        assert_eq!(client.query_one("select count(*) from release_category",&[])?.get::<_,i64>(0),0);
        assert_eq!(client.query_one("select count(*) from native_operations",&[])?.get::<_,i64>(0),0);Ok(())
    }).unwrap();
    let current=queue.claim(12,1,60).unwrap().pop().unwrap();
    assert_eq!(queue.settle(&current,Visit::prepare(&current,result(),[2;32]).unwrap(),13).unwrap(),Settlement::Applied);
    assert_eq!(runtime.owner.failure(),None);runtime.owner.close();
}


#[test]
fn native_header_only_attempt_observes_real_registry_and_joins_without_step_or_predb_state() {
    crate::raw_store::native_pg_opensubtitles_cache_tests::native_worker_parent(
        crate::raw_store::native_pg_opensubtitles_cache_tests::WorkerCase::HeaderOnly);
}

pub(crate) fn run_header_only(runtime:&crate::pg::indexer_runtime::Runtime) {
    let mut config=crate::config::Config::default();config.postprocess.enabled=false;config.predb.enabled=false;
    // A genuine process counter, observed through the existing producer; no
    // fabricated Stage lane or manually inserted telemetry completion row.
    crate::metrics::names::API_REQUESTS.inc(&[("endpoint","native-header-control"),("status","200")],7);
    let ending=Arc::new(std::sync::atomic::AtomicBool::new(false));let end=ending.clone();let owner=runtime.owner.clone();
    let worker=std::thread::spawn(move|| {
        let handle=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        handle.block_on(workers::run(owner,config,Arc::new(crate::nntp::Pool::new(&[])),move||end.load(std::sync::atomic::Ordering::Acquire)));
    });
    let deadline=Instant::now()+Duration::from_secs(5);
    loop {
        let ready=runtime.owner.with(|client|Ok(client.query_one("select exists(select 1 from native_runtime_observation_current h join native_runtime_observations o using(observation_id) join native_metric_samples s using(observation_id) where not o.stage_running and o.stage_instance_nonce is null and o.enabled_mask is null and s.family='atlas_api_requests_total' and s.labels like '%native-header-control%' and s.counter_bits=7)",&[])?.get::<_,bool>(0))).unwrap();
        if ready {break}
        if Instant::now()>=deadline {ending.store(true,std::sync::atomic::Ordering::Release);worker.join().unwrap();panic!("header-only attempt never published its actual completed observation")}
        std::thread::sleep(Duration::from_millis(10));
    }
    runtime.owner.with(|client| {
        for table in ["pp_state","pp_walks","ext_predb_key_meta","native_operations"] {
            assert_eq!(client.query_one(&format!("select count(*) from {table}"),&[])?.get::<_,i64>(0),0,"disabled factory created state in {table}");
        }Ok(())
    }).unwrap();
    ending.store(true,std::sync::atomic::Ordering::Release);worker.join().unwrap();
    assert!(runtime.owner.stop.load(std::sync::atomic::Ordering::Acquire));assert_eq!(runtime.owner.failure(),None);runtime.owner.close();
}


#[test]
#[ignore="requires isolated PostgreSQL and owned HTTP for periodic details lifecycle"]
fn native_periodic_details_lane_releases_owner_and_joins_inflight_http_before_close() {
    crate::raw_store::native_pg_opensubtitles_cache_tests::native_details_parent();
}

pub(crate) fn run_details_held(runtime:&crate::pg::indexer_runtime::Runtime) {
    use std::io::{Read,Write};
    crate::pg::stage_queue_tests::push(runtime);
    runtime.owner.with(|client|{client.execute("insert into media_release(release_id,kind,title_key,quality,tmdb_id) select id,1,id,0,42 from releases",&[])?;Ok(())}).unwrap();
    let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();listener.set_nonblocking(true).unwrap();
    let base=format!("http://127.0.0.1:{}",listener.local_addr().unwrap().port());
    let (entered_tx,entered_rx)=mpsc::channel();let (release_tx,release_rx)=mpsc::channel();
    let server=std::thread::spawn(move|| {
        let until=Instant::now()+Duration::from_secs(5);
        let mut socket=loop {match listener.accept() {Ok((socket,_))=>break socket,Err(error) if error.kind()==std::io::ErrorKind::WouldBlock=>{assert!(Instant::now()<until);std::thread::sleep(Duration::from_millis(5));},Err(error)=>panic!("{error}")}};
        socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();let mut request=Vec::new();let mut byte=[0];
        while request.len()<8192&&!request.ends_with(b"\r\n\r\n") {assert_eq!(socket.read(&mut byte).unwrap(),1);request.push(byte[0]);}
        assert!(request.starts_with(b"GET /movie/42?"));entered_tx.send(()).unwrap();release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let body=r#"{"title":"Held details control","overview":"Actual owned HTTP"}"#;
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    });
    let mut config=crate::config::Config::default();config.postprocess.enabled=false;config.predb.enabled=false;
    config.media.details_per_minute=60;config.media.tmdb.api_key="fixture-key".into();config.media.tmdb.base_url=Some(base);
    let mut details=workers::details_service(&runtime.owner,&config,None).unwrap().expect("configured details-only factory");
    details.start_without_delay_for_test();let mut handles=Vec::new();workers::start_details(&runtime.owner,details,&mut handles).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // A real completed PG observation while provider I/O is held proves that
    // this independent retained lane does not monopolize the writer mutex.
    let owner=runtime.owner.clone();let (captured_tx,captured_rx)=mpsc::channel();let capture=std::thread::spawn(move||captured_tx.send(crate::pg::stage_telemetry::capture(&owner,None)).unwrap());
    let observed=captured_rx.recv_timeout(Duration::from_secs(1));
    if observed.is_err() {release_tx.send(()).unwrap();capture.join().unwrap();server.join().unwrap();let _=join_workers(&runtime.owner,handles);panic!("details provider I/O held the canonical writer mutex")}
    observed.unwrap().unwrap();capture.join().unwrap();
    let owner=runtime.owner.clone();let (done_tx,done_rx)=mpsc::channel();let finish=std::thread::spawn(move||done_tx.send(join_workers(&owner,handles)).unwrap());
    let until=Instant::now()+Duration::from_secs(3);while !runtime.owner.stop.load(std::sync::atomic::Ordering::Acquire) {assert!(Instant::now()<until);std::thread::yield_now();}
    assert!(matches!(done_rx.recv_timeout(Duration::from_millis(30)),Err(mpsc::RecvTimeoutError::Timeout)),"shutdown detached the live details HTTP worker");
    release_tx.send(()).unwrap();server.join().unwrap();let joined=done_rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();finish.join().unwrap();
    assert!(joined.pending().is_empty());assert!(joined.for_owner(&runtime.owner));assert_eq!(runtime.owner.failure(),None);runtime.owner.close();
}
