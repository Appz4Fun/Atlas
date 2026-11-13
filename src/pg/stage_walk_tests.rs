//! Real owner/writer regressions for seed and durable walk progress.
use super::{Queue,Visit,Settlement};
use crate::postprocess::{apply::Applied,step::{StepMutation,StepWrite}};

fn result(claim:&super::Claim)->Applied {
    Applied {release_id:claim.release_id(),claimed_gen:claim.rerun,clear:claim.redo_mask(),set_done:4,
        writes:vec![(4,StepWrite::for_mutation(claim.release_id(),StepMutation::SetCategory {category:2000}))],..Default::default()}
}
fn progress(runtime:&crate::pg::indexer_runtime::Runtime)->(i64,i64,i64,Option<i64>) {
    runtime.owner.with(|client| {
        let row=client.query_one("select before_id,seen,ran,native_inflight_release from pp_walks order by id desc limit 1",&[])?;
        Ok((row.get(0),row.get(1),row.get(2),row.get(3)))
    }).unwrap()
}

#[test]
fn native_walk_seed_is_idempotent_and_config_changes_create_bounded_walks() {
    let Some(schema)=crate::pg::testing::Schema::new() else {return};
    let runtime=crate::pg::stage_queue_tests::runtime(&schema);crate::pg::stage_queue_tests::push(&runtime);
    let queue=Queue::for_owner(&runtime.owner).unwrap();
    queue.seed(4,&[(2,4,1)],10).unwrap();queue.seed(4,&[(2,4,1)],11).unwrap();
    runtime.owner.with(|client| {
        assert_eq!(client.query_one("select count(*) from pp_walks",&[])?.get::<_,i64>(0),1);
        assert_eq!(client.query_one("select top_id=(select max(id)+1 from releases) from pp_walks",&[])?.get::<_,bool>(0),true);Ok(())
    }).unwrap();
    queue.seed(4,&[(2,4,3)],12).unwrap();
    assert_eq!(runtime.owner.with(|client|Ok(client.query_one("select redo_mask from pp_walks order by id desc limit 1",&[])?.get::<_,i64>(0))).unwrap(),4);
    assert!(queue.seed(4,&[(2,8,3)],13).is_err(),"a caller cannot invent a step bit");
    runtime.owner.close();
}

#[test]
fn native_walk_cursor_waits_for_atomic_result_and_receipt_then_replay_is_once() {
    let Some(schema)=crate::pg::testing::Schema::new() else {return};
    let runtime=crate::pg::stage_queue_tests::runtime(&schema);crate::pg::stage_queue_tests::push(&runtime);
    let queue=Queue::for_owner(&runtime.owner).unwrap();queue.request_walk(4,None,10).unwrap();
    let initial=progress(&runtime);assert_eq!(initial.1,0);
    assert_eq!(queue.walk_tick(10).unwrap(),super::WalkProgress::Queued);
    let claims=queue.claim(10,1,60).unwrap();let claim=&claims[0];assert_eq!(claim.redo_mask(),4);
    assert_eq!(progress(&runtime),(initial.0,0,0,Some(claim.release_id())));
    assert_eq!(queue.settle(claim,Visit::prepare(claim,result(claim),[7;32]).unwrap(),11).unwrap(),Settlement::Applied);
    assert_eq!(progress(&runtime),(claim.release_id(),1,1,None));
    assert_eq!(queue.settle(claim,Visit::prepare(claim,result(claim),[7;32]).unwrap(),12).unwrap(),Settlement::Replayed);
    assert_eq!(progress(&runtime),(claim.release_id(),1,1,None));
    assert_eq!(queue.walk_tick(12).unwrap(),super::WalkProgress::Finished);
    runtime.owner.close();
}

#[test]
fn native_walk_future_queue_is_skipped_but_explicit_redo_keeps_its_owned_visit() {
    let Some(schema)=crate::pg::testing::Schema::new() else {return};
    let runtime=crate::pg::stage_queue_tests::runtime(&schema);crate::pg::stage_queue_tests::push(&runtime);
    let queue=Queue::for_owner(&runtime.owner).unwrap();
    runtime.owner.with(|client|{client.execute("update pp_queue set due_at=100",&[])?;Ok(())}).unwrap();
    queue.request_walk(0,None,10).unwrap();assert_eq!(queue.walk_tick(10).unwrap(),super::WalkProgress::Skipped);
    assert_eq!(progress(&runtime).1,1);assert!(queue.claim(10,1,60).unwrap().is_empty());
    queue.request_walk(4,None,11).unwrap();assert_eq!(queue.walk_tick(11).unwrap(),super::WalkProgress::Queued);
    let claims=queue.claim(11,1,60).unwrap();assert_eq!(claims.len(),1);assert_eq!(claims[0].redo_mask(),4);
    let before=progress(&runtime);assert_eq!(queue.walk_tick(12).unwrap(),super::WalkProgress::Blocked);
    assert_eq!(progress(&runtime),before);runtime.owner.close();
}

#[test]
fn native_walk_stale_header_refuses_results_and_keeps_cursor_for_current_input() {
    let Some(schema)=crate::pg::testing::Schema::new() else {return};
    let runtime=crate::pg::stage_queue_tests::runtime(&schema);crate::pg::stage_queue_tests::push(&runtime);
    let queue=Queue::for_owner(&runtime.owner).unwrap();queue.request_walk(4,None,10).unwrap();queue.walk_tick(10).unwrap();
    let claims=queue.claim(10,1,60).unwrap();let claim=&claims[0];let before=progress(&runtime);
    runtime.owner.with(|client|{client.execute("update files set subject='a genuinely changed accepted header' where release_id=$1",&[&claim.release_id()])?;Ok(())}).unwrap();
    assert_eq!(queue.settle(claim,Visit::prepare(claim,result(claim),[7;32]).unwrap(),11).unwrap(),Settlement::Superseded);
    assert_eq!(progress(&runtime),before);
    runtime.owner.with(|client|{assert_eq!(client.query_one("select count(*) from release_category",&[])?.get::<_,i64>(0),0);Ok(())}).unwrap();
    let next=queue.claim(12,1,60).unwrap();assert_eq!(next.len(),1);assert_eq!(next[0].redo_mask(),4);
    assert_eq!(queue.settle(&next[0],Visit::prepare(&next[0],result(&next[0]),[8;32]).unwrap(),13).unwrap(),Settlement::Applied);
    assert_eq!(progress(&runtime).1,1);runtime.owner.close();
}

#[test]
fn native_walk_replacement_reclaims_the_same_pending_operation_after_actual_drain() {
    let Some(schema)=crate::pg::testing::Schema::new() else {return};
    let first=crate::pg::stage_queue_tests::runtime(&schema);crate::pg::stage_queue_tests::push(&first);
    let queue=Queue::for_owner(&first.owner).unwrap();queue.request_walk(4,None,10).unwrap();queue.walk_tick(10).unwrap();
    let old=queue.claim(10,1,60).unwrap().pop().unwrap();let before=progress(&first);
    let pid:i32=first.owner.with(|client|Ok(client.query_one("select pg_backend_pid()",&[])?.get(0))).unwrap();first.owner.close();
    let mut observer=schema.pool.get().unwrap();let deadline=std::time::Instant::now()+std::time::Duration::from_secs(3);
    loop {
        observer.query_one("select pg_stat_clear_snapshot()",&[]).unwrap();
        if !observer.query_one("select exists(select 1 from pg_stat_activity where pid=$1)",&[&pid]).unwrap().get::<_,bool>(0) {break}
        assert!(std::time::Instant::now()<deadline,"old walk owner did not drain");std::thread::sleep(std::time::Duration::from_millis(10));
    }drop(observer);
    let next=crate::pg::stage_queue_tests::runtime(&schema);let queue=Queue::for_owner(&next.owner).unwrap();
    assert_eq!(progress(&next),before);let current=queue.claim(12,1,60).unwrap().pop().unwrap();
    assert_eq!(current.operation_id(),old.operation_id());assert_eq!(current.redo_mask(),4);
    assert!(queue.settle(&old,Visit::prepare(&old,result(&old),[7;32]).unwrap(),13).is_err());assert_eq!(next.owner.failure(),None);
    assert_eq!(queue.settle(&current,Visit::prepare(&current,result(&current),[7;32]).unwrap(),13).unwrap(),Settlement::Applied);
    assert_eq!(progress(&next),(current.release_id(),1,1,None));next.owner.close();
}

#[test]
fn native_walk_receipt_failure_rolls_back_mutations_and_cursor_together() {
    let Some(schema)=crate::pg::testing::Schema::new() else {return};
    let runtime=crate::pg::stage_queue_tests::runtime(&schema);crate::pg::stage_queue_tests::push(&runtime);
    let queue=Queue::for_owner(&runtime.owner).unwrap();queue.request_walk(4,None,10).unwrap();queue.walk_tick(10).unwrap();
    let claim=queue.claim(10,1,60).unwrap().pop().unwrap();let before=progress(&runtime);
    runtime.owner.with(|client| {client.batch_execute("create function reject_walk_receipt() returns trigger language plpgsql as $$
        begin raise exception 'synthetic receipt failure' using errcode='23514'; end $$;
        create trigger reject_walk_receipt before insert on native_operations for each row execute function reject_walk_receipt()")?;Ok(())}).unwrap();
    let error=queue.settle(&claim,Visit::prepare(&claim,result(&claim),[7;32]).unwrap(),11).unwrap_err();
    assert!(error.downcast_ref::<postgres::Error>().is_some());assert_eq!(runtime.owner.failure(),Some(crate::pg::outage::Failure::Fatal));runtime.owner.close();
    let mut inspect=schema.pool.get().unwrap();let row=inspect.query_one("select before_id,seen,ran,native_inflight_release from pp_walks",&[]).unwrap();
    assert_eq!((row.get(0),row.get(1),row.get(2),row.get(3)),before);
    assert_eq!(inspect.query_one("select count(*) from release_category",&[]).unwrap().get::<_,i64>(0),0);
    assert_eq!(inspect.query_one("select count(*) from native_operations",&[]).unwrap().get::<_,i64>(0),0);
    assert_eq!(inspect.query_one("select count(*) from pp_queue",&[]).unwrap().get::<_,i64>(0),1);
}
