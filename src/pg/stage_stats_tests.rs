//! Actual queue population/leases, never supplied metric totals.
use super::*;
use crate::pg::{self,stage_queue::Queue};
const DB:i64=1<<24;
const NET:i64=1<<20;

fn populate(runtime:&pg::indexer_runtime::Runtime)->Vec<i64> {
    runtime.owner.with(|client| {
        let releases=(0..5).map(|n|crate::parser::Release {name:format!("Telemetry.Release.{n}"),group:"alt.binaries.test".into(),
            articles:vec![crate::parser::Article {filename:Some("file.bin".into()),message_id:format!("<telemetry-{n}@test>"),
                part:Some(1),total_parts:Some(1),bytes:1,..Default::default()}],..Default::default()}).collect::<Vec<_>>();
        pg::release_writer::save(client,&releases,1)?;
        Ok(client.query("select id from releases order by id",&[])?.into_iter().map(|r|r.get(0)).collect())
    }).unwrap()
}
fn observation(runtime:&pg::indexer_runtime::Runtime,until:Instant)->Result<Observation> {
    let destination=runtime.owner.cache_destination().unwrap();let epoch=runtime.owner.epoch().unwrap();
    runtime.owner.with(|client|observe(client,destination,&destination.storage,epoch,Masks {enabled:DB|NET,network:NET,net_ready_at:i64::MIN},&runtime.owner.stop,until))
}

#[test]
#[ignore="Root-only isolated PostgreSQL and original native owner admission"]
fn actual_claim_predicates_exclude_leases_future_and_disabled_network_work() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PG");let runtime=pg::stage_queue_tests::runtime(&schema);
    let ids=populate(&runtime);let now=chrono::Utc::now().timestamp();let queue=Queue::for_owner(&runtime.owner).unwrap();
    runtime.owner.with(|client| {
        client.execute("update pp_queue set due_at=$1",&[&(now+600)])?;
        client.execute("update pp_queue set due_at=$1 where release_id=$2",&[&now,&ids[1]])?;Ok(())
    }).unwrap();assert_eq!(queue.claim(now,1,60).unwrap()[0].release_id(),ids[1]);
    runtime.owner.with(|client| {
        client.execute("insert into pp_state(release_id,net_wanted,net_left,net_next_at) values($1,true,$2,$3)",&[&ids[3],&NET,&now])?;Ok(())
    }).unwrap();assert_eq!(queue.claim_net(now,1,60,0).unwrap()[0].release_id(),ids[3]);
    runtime.owner.with(|client| {
        client.execute("update pp_queue set due_at=$1 where release_id=$2",&[&now,&ids[0]])?;
        client.execute("insert into pp_state(release_id,net_wanted,net_left,net_next_at,failed) values($1,true,$2,$3,0)",&[&ids[4],&NET,&now])?;
        client.execute("insert into pp_state(release_id,failed) values($1,$2),($3,$4)",&[&ids[0],&DB,&ids[2],&(1i64<<30)])?;Ok(())
    }).unwrap();queue.request_walk(0,None,now).unwrap();
    let Observation::Complete(found)=observation(&runtime,Instant::now()+Duration::from_secs(2)).unwrap() else {panic!("actual observation must complete")};
    assert_eq!((found.due,found.net,found.failed,found.net_claims),(1,1,1,1));
    assert_eq!(found.walk_done,0);assert_eq!(found.walk_total,ids[4]+1);
    let destination=runtime.owner.cache_destination().unwrap();let epoch=runtime.owner.epoch().unwrap();
    let disabled=runtime.owner.with(|client|observe(client,destination,&destination.storage,epoch,Masks {enabled:DB,network:NET,net_ready_at:i64::MIN},&runtime.owner.stop,Instant::now()+Duration::from_secs(2))).unwrap();
    assert!(matches!(disabled,Observation::Complete(Counts {net:0,..})));
    let paused=runtime.owner.with(|client|observe(client,destination,&destination.storage,epoch,Masks {enabled:DB|NET,network:NET,net_ready_at:now+600},&runtime.owner.stop,Instant::now()+Duration::from_secs(2))).unwrap();
    assert!(matches!(paused,Observation::Complete(Counts {net:0,..})));runtime.owner.close();
}

#[test]
#[ignore="Root-only isolated PostgreSQL and actual local read timeout admission"]
fn local_statement_timeout_defers_only_after_real_rollback_and_keeps_owner_healthy() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PG");let runtime=pg::stage_queue_tests::runtime(&schema);populate(&runtime);
    let mut lock=schema.pool.get().unwrap();let mut held=lock.transaction().unwrap();held.batch_execute("lock table pp_queue in access exclusive mode").unwrap();
    let Observation::Deferred(cause)=observation(&runtime,Instant::now()+Duration::from_millis(200)).unwrap() else {panic!("local bounded query should defer")};
    assert!(cause.is::<LocalReadDeadline>());assert!(cause.chain().any(|e|e.is::<postgres::Error>()));
    assert_eq!(runtime.owner.failure(),None);assert!(!runtime.owner.stop.load(Ordering::Acquire));held.rollback().unwrap();drop(lock);
    runtime.owner.with(|client|{assert_eq!(client.query_one("select 1",&[])?.get::<_,i32>(0),1);Ok(())}).unwrap();runtime.owner.close();
}

#[test]
#[ignore="Root-only isolated PostgreSQL and real walk population admission"]
fn local_walk_capacity_is_deferred_without_publishing_a_partial_zero() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PG");let runtime=pg::stage_queue_tests::runtime(&schema);populate(&runtime);
    runtime.owner.with(|client|{client.batch_execute("insert into pp_walks(kind,top_id,before_id,redo_mask,created_at)
        select 'request',t,t,0,1 from generate_series(1,129), (select max(id)+1 t from releases) maximum")?;Ok(())}).unwrap();
    let Observation::Deferred(cause)=observation(&runtime,Instant::now()+Duration::from_secs(2)).unwrap() else {panic!("actual walk capacity must defer")};
    assert!(cause.is::<LocalAdmission>());assert_eq!(runtime.owner.failure(),None);
    assert!(!runtime.owner.stop.load(Ordering::Acquire));runtime.owner.close();
}

#[test]
#[ignore="Root-only isolated PostgreSQL and actual external cancel admission"]
fn external_query_cancel_is_not_a_local_budget_deferral() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PG");let runtime=pg::stage_queue_tests::runtime(&schema);populate(&runtime);
    let pid=runtime.owner.with(|client|Ok(client.query_one("select pg_backend_pid()",&[])?.get::<_,i32>(0))).unwrap();
    let mut lock=schema.pool.get().unwrap();let mut held=lock.transaction().unwrap();held.batch_execute("lock table pp_queue in access exclusive mode").unwrap();
    let mut cancel=postgres::Client::connect(&schema.url,postgres::NoTls).unwrap();
    let canceller=std::thread::spawn(move|| {
        let until=Instant::now()+Duration::from_secs(1);
        loop {
            cancel.query_one("select pg_stat_clear_snapshot()",&[]).unwrap();
            if cancel.query_one("select exists(select 1 from pg_stat_activity where pid=$1 and wait_event_type='Lock')",&[&pid]).unwrap().get::<_,bool>(0) {break}
            assert!(Instant::now()<until);std::thread::sleep(Duration::from_millis(2));
        }
        assert!(cancel.query_one("select pg_cancel_backend($1)",&[&pid]).unwrap().get::<_,bool>(0));
    });
    let error=observation(&runtime,Instant::now()+Duration::from_secs(2)).unwrap_err();canceller.join().unwrap();
    assert!(!error.is::<LocalReadDeadline>());assert!(error.chain().any(|e|e.is::<postgres::Error>()));
    assert_eq!(runtime.owner.failure(),Some(pg::outage::Failure::Fatal));held.rollback().unwrap();drop(lock);runtime.owner.close();
}
