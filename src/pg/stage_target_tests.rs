//! Fresh native participants are observed on the real owner, never free IDs.
use super::{Queue,Claim,Visit,Settlement};
use crate::postprocess::{apply::Applied,step::{StepMutation,StepWrite}};

fn donor(runtime:&crate::pg::indexer_runtime::Runtime)->i64 {
    runtime.owner.with(|client| {
        crate::pg::release_writer::save(client,&[crate::parser::Release {name:"Donor.Release".into(),group:"example.donor".into(),articles:vec![crate::parser::Article {filename:Some("donor.mkv".into()),message_id:"<donor@example.test>".into(),part:Some(1),total_parts:Some(1),bytes:1,..Default::default()}],..Default::default()}],100000)?;
        Ok(client.query_one("select id from releases where group_name='example.donor'",&[])?.get(0))
    }).unwrap()
}
fn observed(runtime:&crate::pg::indexer_runtime::Runtime,id:i64)->super::participants::Observed {
    let mut targets=super::participants::ReadTargets::for_owner(&runtime.owner).unwrap();
    runtime.owner.with(|client| {
        let mut tx=client.build_transaction().isolation_level(postgres::IsolationLevel::RepeatableRead).read_only(true).start()?;
        // The actual fact and its source revision share this RR. A participant
        // observation taken after this transaction would be insufficient.
        assert!(tx.query_one("select exists(select 1 from files where release_id=$1)",&[&id])?.get::<_,bool>(0));
        targets.observe_in(&mut tx,&[id])?;tx.commit()?;Ok(())
    }).unwrap();targets.finish()
}
fn link(claim:&Claim,id:i64)->Applied {
    Applied {release_id:claim.release_id(),set_done:4,writes:vec![(4,StepWrite::for_mutation(claim.release_id(),StepMutation::PlaceBesideFile {row_id:claim.release_id(),owner_id:id,par2_file:None}))],..Default::default()}
}
#[test]
fn native_cross_result_requires_real_observed_targets_and_commits_once() {
    let Some(schema)=crate::pg::testing::Schema::new() else {return};let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    crate::pg::stage_queue_tests::push(&runtime);let donor=donor(&runtime);let targets=observed(&runtime,donor);
    let queue=Queue::for_owner(&runtime.owner).unwrap();let claim=queue.claim(10,1,60).unwrap().pop().unwrap();
    assert!(Visit::prepare(&claim,link(&claim,donor),[1;32]).is_err(),"raw legacy target numbers remain refused");
    let before=runtime.owner.with(|client|Ok(client.query("select id,native_work_revision from releases order by id",&[])?.into_iter().map(|row|(row.get::<_,i64>(0),row.get::<_,i64>(1))).collect::<Vec<_>>())).unwrap();
    assert_eq!(queue.settle(&claim,Visit::prepare_observed(&claim,link(&claim,donor),[1;32],targets).unwrap(),11).unwrap(),Settlement::Applied);
    runtime.owner.with(|client| {
        assert_eq!(client.query_one("select release_id from beside_files where row_id=$1",&[&claim.release_id()])?.get::<_,i64>(0),donor);
        let after=client.query("select id,native_work_revision from releases order by id",&[])?.into_iter().map(|row|(row.get::<_,i64>(0),row.get::<_,i64>(1))).collect::<Vec<_>>();
        assert_eq!(after,before.into_iter().map(|(id,n)|(id,n+1)).collect::<Vec<_>>());
        assert_eq!(client.query_one("select count(*) from native_operations",&[])?.get::<_,i64>(0),1);Ok(())
    }).unwrap();runtime.owner.close();
}

#[test]
fn native_cross_result_refuses_changed_donor_before_any_effect_or_receipt() {
    let Some(schema)=crate::pg::testing::Schema::new() else {return};let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    crate::pg::stage_queue_tests::push(&runtime);let donor=donor(&runtime);let targets=observed(&runtime,donor);
    let queue=Queue::for_owner(&runtime.owner).unwrap();let claim=queue.claim(10,1,60).unwrap().pop().unwrap();
    runtime.owner.with(|client|{client.execute("update files set subject='new genuine donor input' where release_id=$1",&[&donor])?;Ok(())}).unwrap();
    assert_eq!(queue.settle(&claim,Visit::prepare_observed(&claim,link(&claim,donor),[1;32],targets).unwrap(),11).unwrap(),Settlement::Superseded);
    runtime.owner.with(|client| {
        assert_eq!(client.query_one("select count(*) from beside_files",&[])?.get::<_,i64>(0),0);
        assert_eq!(client.query_one("select count(*) from native_operations",&[])?.get::<_,i64>(0),0);
        assert_eq!(client.query_one("select count(*) from pp_queue where release_id=$1 and lease_token is null",&[&claim.release_id()])?.get::<_,i64>(0),1);Ok(())
    }).unwrap();assert_eq!(runtime.owner.failure(),None);runtime.owner.close();
}

#[test]
fn native_cross_result_refuses_donor_facts_committed_by_another_real_visit() {
    let Some(schema)=crate::pg::testing::Schema::new() else {return};let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    crate::pg::stage_queue_tests::push(&runtime);let donor=donor(&runtime);let targets=observed(&runtime,donor);
    let queue=Queue::for_owner(&runtime.owner).unwrap();let source=queue.claim(10,1,60).unwrap().pop().unwrap();
    queue.request_walk(4,Some("example.donor"),10).unwrap();queue.walk_tick(10).unwrap();
    let other=queue.claim(10,1,60).unwrap().pop().unwrap();assert_eq!(other.release_id(),donor);
    let result=Applied {release_id:donor,set_done:4,writes:vec![(4,StepWrite::for_mutation(donor,StepMutation::SetNfoPassword {password:"new donor fact".into()}))],..Default::default()};
    assert_eq!(queue.settle(&other,Visit::prepare(&other,result,[2;32]).unwrap(),11).unwrap(),Settlement::Applied);
    assert_eq!(queue.settle(&source,Visit::prepare_observed(&source,link(&source,donor),[1;32],targets).unwrap(),12).unwrap(),Settlement::Superseded);
    runtime.owner.with(|client| {
        assert_eq!(client.query_one("select count(*) from beside_files",&[])?.get::<_,i64>(0),0);
        assert_eq!(client.query_one("select password from release_nfo_password where release_id=$1",&[&donor])?.get::<_,String>(0),"new donor fact");
        assert_eq!(client.query_one("select count(*) from native_operations",&[])?.get::<_,i64>(0),1);Ok(())
    }).unwrap();assert_eq!(runtime.owner.failure(),None);runtime.owner.close();
}

#[test]
fn native_target_observation_rejects_a_pooled_session_without_poisoning_the_owner() {
    let Some(schema)=crate::pg::testing::Schema::new() else {return};let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    crate::pg::stage_queue_tests::push(&runtime);let donor=donor(&runtime);
    let mut targets=super::participants::ReadTargets::for_owner(&runtime.owner).unwrap();let mut other=schema.pool.get().unwrap();
    let mut tx=other.build_transaction().isolation_level(postgres::IsolationLevel::RepeatableRead).read_only(true).start().unwrap();
    assert!(targets.observe_in(&mut tx,&[donor]).is_err(),"a pool transaction is not the real selected owner's snapshot");drop(tx);
    assert_eq!(runtime.owner.failure(),None);
    runtime.owner.with(|client|{assert_eq!(client.query_one("select count(*) from beside_files",&[])?.get::<_,i64>(0),0);Ok(())}).unwrap();runtime.owner.close();
}

fn add(runtime:&crate::pg::indexer_runtime::Runtime,name:&str,group:&str,mid:&str)->i64 {
    runtime.owner.with(|client| {
        crate::pg::release_writer::save(client,&[crate::parser::Release {name:name.into(),group:group.into(),articles:vec![crate::parser::Article {filename:Some("sample.bin".into()),message_id:mid.into(),part:Some(1),total_parts:Some(1),bytes:1,..Default::default()}],..Default::default()}],100000)?;
        Ok(client.query_one("select id from releases where original_name=$1 and group_name=$2",&[&name.as_bytes(),&group])?.get(0))
    }).unwrap()
}
fn observed_many(runtime:&crate::pg::indexer_runtime::Runtime,ids:&[i64])->super::participants::Observed {
    let mut targets=super::participants::ReadTargets::for_owner(&runtime.owner).unwrap();
    runtime.owner.with(|client| {let mut tx=client.build_transaction().isolation_level(postgres::IsolationLevel::RepeatableRead).read_only(true).start()?;
        targets.observe_in(&mut tx,ids)?;tx.commit()?;Ok(())}).unwrap();targets.finish()
}
fn seed_recovery(runtime:&crate::pg::indexer_runtime::Runtime,queue:&Queue,id:i64,group:&str) {
    queue.request_walk(4,Some(group),10).unwrap();queue.walk_tick(10).unwrap();let claim=queue.claim(10,1,60).unwrap().pop().unwrap();assert_eq!(claim.release_id(),id);
    let result=Applied {release_id:id,claimed_gen:claim.rerun,clear:claim.redo_mask(),writes:vec![(4,StepWrite::for_mutation(id,StepMutation::LinkUploadRecovery {lead_id:id,wanted:1,set_id:Some([2;16])}))],..Default::default()};
    assert_eq!(queue.settle(&claim,Visit::prepare_observed(&claim,result,[3;32],observed_many(runtime,&[id])).unwrap(),11).unwrap(),Settlement::Applied);
}
#[test]
fn native_upload_results_follow_the_current_lead_after_an_earlier_effect_in_same_commit() {
    let Some(schema)=crate::pg::testing::Schema::new() else {return};let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    crate::pg::stage_queue_tests::push(&runtime);let first=donor(&runtime);let next=add(&runtime,"Next.Lead","example.next","<next-lead@example.test>");
    let queue=Queue::for_owner(&runtime.owner).unwrap();let source=queue.claim(10,1,60).unwrap().pop().unwrap();
    seed_recovery(&runtime,&queue,first,"example.donor");seed_recovery(&runtime,&queue,next,"example.next");
    let targets=observed_many(&runtime,&[first,next]);
    let result=Applied {release_id:source.release_id(),writes:vec![
        (4,StepWrite::for_mutation(source.release_id(),StepMutation::LinkUploadData {member_id:source.release_id(),lead_id:first,par2_file:[1;16]})),
        (4,StepWrite::for_mutation(source.release_id(),StepMutation::MoveUpload {from_id:first,to_id:next})),
        (4,StepWrite::for_mutation(source.release_id(),StepMutation::LinkUploadRecovery {lead_id:first,wanted:5,set_id:Some([3;16])})),
    ],..Default::default()};
    assert_eq!(queue.settle(&source,Visit::prepare_observed(&source,result,[4;32],targets).unwrap(),12).unwrap(),Settlement::Applied);
    runtime.owner.with(|client| {
        let row=client.query_one("select lead_id,recovery,wanted,set_id from upload_link where release_id=$1",&[&source.release_id()])?;
        assert_eq!((row.get::<_,i64>(0),row.get::<_,i64>(1),row.get::<_,i64>(2),row.get::<_,Vec<u8>>(3)),(next,1,5,vec![3;16]));
        assert_eq!(client.query_one("select lead_id from upload_link where release_id=$1",&[&first])?.get::<_,i64>(0),next);Ok(())
    }).unwrap();runtime.owner.close();
}

#[test]
fn native_pointed_names_match_original_bytes_and_preserve_an_existing_negative_password() {
    let Some(schema)=crate::pg::testing::Schema::new() else {return};let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    crate::pg::stage_queue_tests::push(&runtime);
    let target=add(&runtime,"Pointed\0.Release","example.group","<pointed@example.test>");
    let alias=add(&runtime,"Pointed�.Release","example.group","<alias@example.test>");
    let queue=Queue::for_owner(&runtime.owner).unwrap();let source=queue.claim(10,1,60).unwrap().pop().unwrap();
    runtime.owner.with(|client| {
        // Simulate a fixed-key collision to prove the required full-byte check.
        client.execute("update releases set original_name_key=(select original_name_key from releases where id=$1) where id=$2",&[&target,&alias])?;
        client.execute("insert into release_nfo_password(release_id,password) values($1,'')",&[&target])?;Ok(())
    }).unwrap();
    let name="Pointed\0.Release".to_string();let result=Applied {release_id:source.release_id(),writes:vec![
        (4,StepWrite::for_mutation(source.release_id(),StepMutation::MarkPointed {name:name.clone()})),
        (4,StepWrite::for_mutation(source.release_id(),StepMutation::RankPointed {name:name.clone()})),
        (4,StepWrite::for_mutation(source.release_id(),StepMutation::SetPointedPassword {name,password:"new hint".into()})),
    ],..Default::default()};
    assert_eq!(queue.settle(&source,Visit::prepare_observed(&source,result,[5;32],observed_many(&runtime,&[])).unwrap(),11).unwrap(),Settlement::Applied);
    runtime.owner.with(|client| {
        assert_eq!(client.query("select release_id from nzb_pointed order by release_id",&[])?.into_iter().map(|row|row.get::<_,i64>(0)).collect::<Vec<_>>(),[target]);
        assert_eq!(client.query_one("select password from release_nfo_password where release_id=$1",&[&target])?.get::<_,String>(0),"");
        assert!(!client.query_one("select exists(select 1 from release_nfo_password where release_id=$1)",&[&alias])?.get::<_,bool>(0));Ok(())
    }).unwrap();runtime.owner.close();
}
