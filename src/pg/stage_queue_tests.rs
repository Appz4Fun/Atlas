use super::stage_queue::{Queue,Visit,Settlement,Reconciliation};
use crate::postprocess::apply::Applied;
use crate::postprocess::step::{StepWrite,StepMutation};

pub(crate) fn runtime(schema:&super::testing::Schema)->super::indexer_runtime::Runtime {
    let mut config=crate::config::Config::default();
    config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;
    config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;
    let storage=super::StorageConfig {backend:super::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:1,allow_plaintext:true};
    let headers=crate::config::HeaderStoreConfig {enabled:true,dir:std::env::temp_dir().join("native-queue-fixture").to_str().unwrap().into()};
    super::indexer_runtime::Runtime::open(&config,storage,headers,&[],&mut None).unwrap()
}
pub(crate) fn push(runtime:&super::indexer_runtime::Runtime) {
    // These lease tests use a fixed clock of 10. The genuine writer's settle
    // delay still applies; place the input early enough without global resets.
    let settle=crate::catalog_facts::SETTLE_SECS.load(std::sync::atomic::Ordering::Relaxed)
        .max(crate::catalog_facts::COMPLETE_SETTLE_SECS.load(std::sync::atomic::Ordering::Relaxed)).max(1);
    let posted_at=1i64.saturating_sub(settle);
    runtime.owner.with(|client|super::release_writer::save(client,&[crate::parser::Release {name:"Example.Release".into(),group:"example.group".into(),articles:vec![crate::parser::Article {filename:Some("sample.bin".into()),message_id:"<part@example.test>".into(),part:Some(1),total_parts:Some(1),bytes:1,..Default::default()}],..Default::default()}],posted_at).map(|_|())).unwrap();
}
fn applied(id:i64)->Applied {
    Applied {release_id:id,set_done:4,writes:vec![(4,StepWrite::for_mutation(id,StepMutation::SetCategory {category:2000}))],..Default::default()}
}
#[test]
fn native_junk_commits_replay_fence_receipt_and_walk_progress_without_deleting_cache() {
    let Some(schema)=super::testing::Schema::new() else {return};let runtime=runtime(&schema);push(&runtime);
    runtime.owner.with(|client|{client.execute("insert into ext_opensubtitles(moviehash,checked_at) values('0000000000000001',1)",&[])?;Ok(())}).unwrap();
    let queue=Queue::for_owner(&runtime.owner).unwrap();queue.request_walk(4,None,10).unwrap();queue.walk_tick(10).unwrap();
    let claims=queue.claim(10,1,60).unwrap();let claim=&claims[0];
    let result=||Applied {release_id:claim.release_id(),junk:Some(crate::junk::EXECUTABLE_INSIDE),..Default::default()};
    assert_eq!(queue.settle(claim,Visit::prepare(claim,result(),[1;32]).unwrap(),11).unwrap(),Settlement::Applied);
    assert_eq!(queue.settle(claim,Visit::prepare(claim,result(),[1;32]).unwrap(),12).unwrap(),Settlement::Replayed);
    push(&runtime);
    runtime.owner.with(|client| {
        assert_eq!(client.query_one("select count(*) from releases",&[])?.get::<_,i64>(0),0);
        assert_eq!(client.query_one("select count(*) from release_writer_junk",&[])?.get::<_,i64>(0),1);
        assert_eq!(client.query_one("select count(*) from native_operations",&[])?.get::<_,i64>(0),1);
        assert_eq!(client.query_one("select seen from pp_walks",&[])?.get::<_,i64>(0),1);
        assert_eq!(client.query_one("select count(*) from ext_opensubtitles",&[])?.get::<_,i64>(0),1);Ok(())
    }).unwrap();runtime.owner.close();
}

#[test]
fn native_pc_junk_refuses_a_real_video_and_preserves_the_row() {
    let Some(schema)=super::testing::Schema::new() else {return};let runtime=runtime(&schema);
    runtime.owner.with(|client|super::release_writer::save(client,&[crate::parser::Release {name:"Kept.Video".into(),group:"example.group".into(),articles:vec![crate::parser::Article {filename:Some("movie.mkv".into()),message_id:"<video@example.test>".into(),part:Some(1),total_parts:Some(1),bytes:1,..Default::default()}],..Default::default()}],-10000).map(|_|())).unwrap();
    let queue=Queue::for_owner(&runtime.owner).unwrap();let claims=queue.claim(10,1,60).unwrap();let claim=&claims[0];
    let result=Applied {release_id:claim.release_id(),junk:Some(crate::junk::PC),..Default::default()};
    assert_eq!(queue.settle(claim,Visit::prepare(claim,result,[1;32]).unwrap(),11).unwrap(),Settlement::Superseded);
    runtime.owner.with(|client| {
        assert_eq!(client.query_one("select count(*) from releases",&[])?.get::<_,i64>(0),1);
        assert_eq!(client.query_one("select count(*) from release_writer_junk",&[])?.get::<_,i64>(0),0);
        assert_eq!(client.query_one("select count(*) from native_operations",&[])?.get::<_,i64>(0),0);Ok(())
    }).unwrap();runtime.owner.close();
}
#[test]
fn native_database_claim_turns_preserve_latest_three_and_oldest_one_fairness() {
    let Some(schema)=super::testing::Schema::new() else {return};let runtime=runtime(&schema);
    let oldest_first=runtime.owner.with(|client| {
        let settle=crate::catalog_facts::SETTLE_SECS.load(std::sync::atomic::Ordering::Relaxed)
            .max(crate::catalog_facts::COMPLETE_SETTLE_SECS.load(std::sync::atomic::Ordering::Relaxed)).max(1);
        for n in 0..5 {
            super::release_writer::save(client,&[crate::parser::Release {name:format!("Fairness.Example.{n}"),group:"example.group".into(),articles:vec![crate::parser::Article {filename:Some("sample.bin".into()),message_id:format!("<fairness-{n}@example.test>"),part:Some(1),total_parts:Some(1),bytes:1,..Default::default()}],..Default::default()}],(-10i64).saturating_sub(settle)+n)?;
        }
        Ok(client.query("select release_id from pp_queue order by due_at,release_id",&[])?.into_iter().map(|row|row.get::<_,i64>(0)).collect::<Vec<_>>())
    }).unwrap();
    assert_eq!(oldest_first.len(),5);let queue=Queue::for_owner(&runtime.owner).unwrap();
    for (turn,expected) in [oldest_first[4],oldest_first[3],oldest_first[2],oldest_first[0]].into_iter().enumerate() {
        let claims=queue.claim_turn(10,1,60,turn as i64).unwrap();assert_eq!(claims.len(),1);assert_eq!(claims[0].release_id(),expected);
        assert_eq!(queue.settle(&claims[0],Visit::prepare(&claims[0],applied(expected),[9;32]).unwrap(),11).unwrap(),Settlement::Applied);
    }
    runtime.owner.close();
}
#[test]
fn native_queue_commits_closed_results_with_the_receipt_and_replays_once() {
    let Some(schema)=super::testing::Schema::new() else {return};let runtime=runtime(&schema);push(&runtime);
    let queue=Queue::for_owner(&runtime.owner).unwrap();let claims=queue.claim(10,1,60).unwrap();assert_eq!(claims.len(),1);let claim=&claims[0];
    assert_eq!(queue.settle(claim,Visit::prepare(claim,applied(claim.release_id()),[9;32]).unwrap(),11).unwrap(),Settlement::Applied);
    assert_eq!(queue.settle(claim,Visit::prepare(claim,applied(claim.release_id()),[9;32]).unwrap(),12).unwrap(),Settlement::Replayed);
    runtime.owner.with(|client| {
        assert_eq!(client.query_one("select count(*) from pp_queue",&[])?.get::<_,i64>(0),0);
        assert_eq!(client.query_one("select count(*) from native_operations",&[])?.get::<_,i64>(0),1);
        assert_eq!(client.query_one("select done from pp_state",&[])?.get::<_,i64>(0),4);
        assert_eq!(client.query_one("select category from release_category",&[])?.get::<_,i64>(0),2000);Ok(())
    }).unwrap();runtime.owner.close();
}

#[test]
fn default_freeze_policy_native_completion_retains_the_release_and_valuable_cache() {
    let Some(schema)=super::testing::Schema::new() else {return};let runtime=runtime(&schema);push(&runtime);
    assert!(crate::config::Config::default().freeze.enabled);
    runtime.owner.with(|client|{client.execute("insert into ext_opensubtitles(moviehash,checked_at) values('0000000000000001',1)",&[])?;Ok(())}).unwrap();
    let queue=Queue::for_owner(&runtime.owner).unwrap();let claims=queue.claim(10,1,60).unwrap();let claim=&claims[0];
    let mut a=Applied::native_database(claim.release_id(),0,1,4,0);a.set_done=4;
    a.writes.push((4,StepWrite::for_mutation(claim.release_id(),StepMutation::SetCategory {category:2000})));
    assert!(!a.freeze_check);assert_eq!(queue.settle(claim,Visit::prepare(claim,a,[9;32]).unwrap(),11).unwrap(),Settlement::Applied);
    runtime.owner.with(|client| {
        assert_eq!(client.query_one("select count(*) from releases",&[])?.get::<_,i64>(0),1);
        assert_eq!(client.query_one("select count(*) from ext_opensubtitles",&[])?.get::<_,i64>(0),1);
        assert_eq!(client.query_one("select count(*) from freeze_pending",&[])?.get::<_,i64>(0),0);
        assert_eq!(client.query_one("select count(*) from pp_queue",&[])?.get::<_,i64>(0),0);Ok(())
    }).unwrap();
    let mut direct=Applied::native_database(claim.release_id(),0,1,4,0);direct.freeze_check=true;
    assert!(Visit::prepare(claim,direct,[9;32]).is_err(),"a direct legacy freeze request remains refused");
    runtime.owner.close();
}

fn arm_native_net(runtime:&super::indexer_runtime::Runtime)->Queue {
    push(runtime);let queue=Queue::for_owner(&runtime.owner).unwrap();let claims=queue.claim(10,1,60).unwrap();let claim=&claims[0];
    let a=Applied {release_id:claim.release_id(),set_done:4,net_mask:8,net_steps:8,..Default::default()};
    assert_eq!(queue.settle(claim,Visit::prepare(claim,a,[1;32]).unwrap(),11).unwrap(),Settlement::Applied);queue
}

#[test]
fn native_net_claim_records_begin_before_work_and_owns_atomic_completion() {
    let Some(schema)=super::testing::Schema::new() else {return};let runtime=runtime(&schema);let queue=arm_native_net(&runtime);
    let claims=queue.claim_net(12,1,60,0).unwrap();assert_eq!(claims.len(),1);let claim=&claims[0];
    assert!(queue.claim_net(12,1,60,1).unwrap().is_empty());
    assert_eq!(queue.begin_net(claim,12,900).unwrap(),super::stage_queue::NetBegin::Started {attempts:0});
    runtime.owner.with(|client| {
        assert_eq!(client.query_one("select net_attempts,net_next_at from pp_state",&[])?.get::<_,i64>(0),1);
        assert_eq!(client.query_one("select net_next_at from pp_state",&[])?.get::<_,i64>(0),912);Ok(())
    }).unwrap();
    let a=Applied {release_id:claim.release_id(),lane:crate::postprocess::apply::Lane::Net,set_done:8,net_steps:8,net_attempts:Some(0),..Default::default()};
    assert_eq!(queue.settle(claim,Visit::prepare(claim,a,[2;32]).unwrap(),13).unwrap(),Settlement::Applied);
    assert!(queue.claim_net(14,1,60,0).unwrap().is_empty());
    runtime.owner.with(|client| {
        let row=client.query_one("select done,net_wanted,native_net_token from pp_state",&[])?;
        assert_eq!(row.get::<_,i64>(0),12);assert_eq!(row.get::<_,Option<bool>>(1),None);assert_eq!(row.get::<_,Option<i64>>(2),None);Ok(())
    }).unwrap();runtime.owner.close();
}

#[test]
fn native_net_header_change_fences_old_results_without_ack_or_partial_writes() {
    let Some(schema)=super::testing::Schema::new() else {return};let runtime=runtime(&schema);let queue=arm_native_net(&runtime);
    let claims=queue.claim_net(12,1,60,0).unwrap();let claim=&claims[0];
    runtime.owner.with(|client| {
        client.execute("update files set subject='new accepted representative' where release_id=$1",&[&claim.release_id()])?;Ok(())
    }).unwrap();
    assert_eq!(queue.begin_net(claim,13,900).unwrap(),super::stage_queue::NetBegin::Superseded);
    let a=Applied {release_id:claim.release_id(),lane:crate::postprocess::apply::Lane::Net,set_done:8,writes:vec![(8,StepWrite::for_mutation(claim.release_id(),StepMutation::SetNfoPassword {password:"must not apply".into()}))],..Default::default()};
    assert_ne!(queue.settle(claim,Visit::prepare(claim,a,[3;32]).unwrap(),14).unwrap(),Settlement::Applied);
    runtime.owner.with(|client| {
        assert_eq!(client.query_one("select count(*) from release_nfo_password",&[])?.get::<_,i64>(0),0);
        assert_eq!(client.query_one("select net_attempts from pp_state",&[])?.get::<_,i64>(0),0);Ok(())
    }).unwrap();runtime.owner.close();
}

#[test]
fn native_net_rerun_preserves_live_lease_until_the_stale_visit_drains() {
    let Some(schema)=super::testing::Schema::new() else {return};let runtime=runtime(&schema);let queue=arm_native_net(&runtime);
    let claims=queue.claim_net(12,1,60,0).unwrap();let old=&claims[0];
    runtime.owner.with(|client|{let mut tx=client.transaction()?;assert!(super::stage_queue::rerun_in(&mut tx,old.release_id(),8,13)?);tx.commit()?;Ok(())}).unwrap();
    assert!(queue.claim(13,1,60).unwrap().is_empty(),"DB cannot overlap a live NET lease");
    assert!(queue.claim_net(13,1,60,0).unwrap().is_empty(),"rerun cannot admit another BODY visit");
    assert_eq!(queue.begin_net(old,13,900).unwrap(),super::stage_queue::NetBegin::Superseded);
    let db=queue.claim(14,1,60).unwrap();assert_eq!(db.len(),1);
    assert!(queue.claim_net(14,1,60,0).unwrap().is_empty(),"NET cannot overlap a live DB lease");
    runtime.owner.close();
}

#[test]
fn native_net_replacement_keeps_uncommitted_operation_identity_and_rejects_old_capability() {
    let Some(schema)=super::testing::Schema::new() else {return};let first=runtime(&schema);let old_queue=arm_native_net(&first);
    let old_claims=old_queue.claim_net(12,1,60,0).unwrap();let old=&old_claims[0];
    let operation=*old.operation_id();let old_pid:i32=first.owner.with(|client|Ok(client.query_one("select pg_backend_pid()",&[])?.get(0))).unwrap();
    first.owner.close();
    let mut observer=schema.pool.get().unwrap();let until=std::time::Instant::now()+std::time::Duration::from_secs(3);
    loop {
        observer.query_one("select pg_stat_clear_snapshot()",&[]).unwrap();
        if !observer.query_one("select exists(select 1 from pg_stat_activity where pid=$1)",&[&old_pid]).unwrap().get::<_,bool>(0) {break}
        assert!(std::time::Instant::now()<until,"old owner did not drain");std::thread::sleep(std::time::Duration::from_millis(10));
    }
    drop(observer);
    let next=runtime(&schema);let queue=Queue::for_owner(&next.owner).unwrap();let claims=queue.claim_net(13,1,60,0).unwrap();assert_eq!(claims.len(),1);
    assert_eq!(claims[0].operation_id(),&operation);
    let result=|id|Applied {release_id:id,lane:crate::postprocess::apply::Lane::Net,set_done:8,net_steps:8,net_attempts:Some(0),..Default::default()};
    let old_visit=Visit::prepare(old,result(old.release_id()),[2;32]).unwrap();
    assert_eq!(queue.reconcile(old,&old_visit).unwrap(),Reconciliation::Pending);
    assert!(queue.settle(old,old_visit,14).is_err());assert_eq!(next.owner.failure(),None);
    assert_eq!(queue.settle(&claims[0],Visit::prepare(&claims[0],result(claims[0].release_id()),[2;32]).unwrap(),14).unwrap(),Settlement::Applied);
    assert_eq!(queue.reconcile(old,&Visit::prepare(old,result(old.release_id()),[2;32]).unwrap()).unwrap(),Reconciliation::Committed);
    next.owner.close();
}

#[test]
fn native_net_claim_turns_follow_real_rank_newest_and_oldest_orders() {
    let Some(schema)=super::testing::Schema::new() else {return};let runtime=runtime(&schema);let queue=Queue::for_owner(&runtime.owner).unwrap();
    runtime.owner.with(|client| {
        for (n,bytes) in [1,50<<20,1<<30,5i64<<30,40i64<<30].into_iter().enumerate() {
            super::release_writer::save(client,&[crate::parser::Release {name:format!("Net.Fairness.{n}"),group:"example.group".into(),articles:vec![crate::parser::Article {filename:Some("sample.bin".into()),message_id:format!("<net-fairness-{n}@example.test>"),part:Some(1),total_parts:Some(1),bytes,..Default::default()}],..Default::default()}],-10000)?;
        }Ok(())
    }).unwrap();
    for _ in 0..5 {
        let claims=queue.claim(10,1,60).unwrap();let claim=&claims[0];
        let a=Applied {release_id:claim.release_id(),set_done:4,net_mask:8,net_steps:8,writes:vec![(4,StepWrite::for_mutation(claim.release_id(),StepMutation::SetCategory {category:2000}))],..Default::default()};
        assert_eq!(queue.settle(claim,Visit::prepare(claim,a,[1;32]).unwrap(),11).unwrap(),Settlement::Applied);
    }
    let ids=runtime.owner.with(|client|Ok(client.query("select id from releases order by id",&[])?.into_iter().map(|row|row.get::<_,i64>(0)).collect::<Vec<_>>())).unwrap();
    for (turn,id) in [(0,ids[4]),(3,ids[3]),(7,ids[0]),(1,ids[2])] {
        let claims=queue.claim_net(12,1,60,turn).unwrap();assert_eq!(claims.len(),1);assert_eq!(claims[0].release_id(),id);
    }
    runtime.owner.close();
}
#[test]
fn native_queue_new_input_refuses_all_old_results_before_application() {
    let Some(schema)=super::testing::Schema::new() else {return};let runtime=runtime(&schema);push(&runtime);
    let queue=Queue::for_owner(&runtime.owner).unwrap();let claims=queue.claim(10,1,60).unwrap();let claim=&claims[0];
    runtime.owner.with(|client|super::release_writer::save(client,&[crate::parser::Release {name:"Example.Release".into(),group:"example.group".into(),articles:vec![crate::parser::Article {filename:Some("sample.bin".into()),message_id:"<next@example.test>".into(),part:Some(2),total_parts:Some(2),bytes:1,..Default::default()}],..Default::default()}],10).map(|_|())).unwrap();
    assert_eq!(queue.settle(claim,Visit::prepare(claim,applied(claim.release_id()),[9;32]).unwrap(),11).unwrap(),Settlement::Superseded);
    runtime.owner.with(|client| {
        assert_eq!(client.query_one("select count(*) from native_operations",&[])?.get::<_,i64>(0),0);
        assert_eq!(client.query_one("select count(*) from release_category",&[])?.get::<_,i64>(0),0);
        assert_eq!(client.query_one("select count(*) from pp_queue where lease_token is null",&[])?.get::<_,i64>(0),1);Ok(())
    }).unwrap();runtime.owner.close();
}
#[test]
fn native_queue_rejects_wrong_target_and_an_ended_owner_without_ack() {
    let Some(schema)=super::testing::Schema::new() else {return};let runtime=runtime(&schema);push(&runtime);
    let queue=Queue::for_owner(&runtime.owner).unwrap();let claims=queue.claim(10,1,60).unwrap();let claim=&claims[0];
    let mut wrong=applied(claim.release_id());wrong.writes[0].1=StepWrite::for_mutation(claim.release_id()+1,StepMutation::SetCategory {category:2000});
    assert!(Visit::prepare(claim,wrong,[9;32]).is_err());
    runtime.owner.close();
    assert!(queue.settle(claim,Visit::prepare(claim,applied(claim.release_id()),[9;32]).unwrap(),11).is_err());
    let mut client=schema.pool.get().unwrap();
    assert_eq!(client.query_one("select count(*) from native_operations",&[]).unwrap().get::<_,i64>(0),0);
    assert_eq!(client.query_one("select count(*) from pp_queue",&[]).unwrap().get::<_,i64>(0),1);
}

#[test]
fn native_expired_lease_never_executes_results_or_creates_a_receipt() {
    let Some(schema)=super::testing::Schema::new() else {return};let runtime=runtime(&schema);push(&runtime);
    let queue=Queue::for_owner(&runtime.owner).unwrap();let claims=queue.claim(10,1,1).unwrap();let claim=&claims[0];
    let visit=Visit::prepare(claim,applied(claim.release_id()),[9;32]).unwrap();
    assert_eq!(queue.reconcile(claim,&visit).unwrap(),Reconciliation::Pending);
    assert_eq!(queue.settle(claim,visit,11).unwrap(),Settlement::LeaseLost);
    runtime.owner.with(|client| {
        assert_eq!(client.query_one("select count(*) from native_operations",&[])?.get::<_,i64>(0),0);
        assert_eq!(client.query_one("select count(*) from release_category",&[])?.get::<_,i64>(0),0);Ok(())
    }).unwrap();runtime.owner.close();
}

#[test]
fn replacement_owner_reconciles_committed_effect_without_reusing_old_lease() {
    let Some(schema)=super::testing::Schema::new() else {return};let first=runtime(&schema);push(&first);
    let queue=Queue::for_owner(&first.owner).unwrap();let claims=queue.claim(10,1,60).unwrap();let old=&claims[0];
    assert_eq!(queue.settle(old,Visit::prepare(old,applied(old.release_id()),[9;32]).unwrap(),11).unwrap(),Settlement::Applied);
    first.owner.close();
    // Runtime::open itself refuses while the actual old backend remains live;
    // the new owner cannot adopt a session-lock or a lease by copying a row.
    let next=runtime(&schema);let current=Queue::for_owner(&next.owner).unwrap();
    let visit=Visit::prepare(old,applied(old.release_id()),[9;32]).unwrap();
    assert_eq!(current.reconcile(old,&visit).unwrap(),Reconciliation::Committed);
    assert!(current.settle(old,visit,12).is_err());
    assert_eq!(next.owner.failure(),None);
    assert!(current.claim(12,1,60).unwrap().is_empty());
    next.owner.close();
}
