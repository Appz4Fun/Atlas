//! All PG cases are explicit opt-in; none manufacture a successful skip.
use super::*;
use crate::spotnet::{full,store,comments};
use std::time::{Duration,Instant};
use crate::spotnet::native_read_fixture_tests::XML;

#[test]
fn original_spot_text_refuses_missing_malformed_and_mixed_display_values() {
    assert_eq!(original(Some(b"alpha\0beta".to_vec()),Some("alpha\u{fffd}beta"),32).unwrap(),Some("alpha\0beta".into()));
    assert_eq!(original(Some(Vec::new()),Some(""),32).unwrap(),Some(String::new()));
    let nul=vec![0;comments::MAX_BODY];let displayed="\u{fffd}".repeat(comments::MAX_BODY);
    assert_eq!(original(Some(nul.clone()),Some(&displayed),2*comments::MAX_BODY).unwrap().unwrap().as_bytes(),nul.as_slice());
    assert!(original(None,Some("sanitized is not original"),32).is_err());
    assert!(original(Some(vec![255]),Some("lossy"),32).is_err());
    assert!(original(Some(b"real".to_vec()),Some("other"),32).is_err());
    assert!(original(Some(vec![b'x';33]),Some(""),32).is_err());
}

fn fixture()->(crate::pg::testing::Schema,crate::pg::indexer_runtime::Runtime,tempfile::TempDir,i64) {
    crate::spotnet::native_read_fixture_tests::fixture()
}

#[test]
#[ignore="explicit isolated PG; actual producer originals and native read snapshot"]
fn native_original_spot_and_body_roundtrip_through_real_web_and_step_adapters() {
    let (schema,runtime,_dir,id)=fixture();let mut client=schema.pool.get().unwrap();
    let view=crate::pg::fresh_reader::ReadView::open(&mut client,&schema.name,crate::pg::fresh_reader::Budget::default(),Instant::now()+Duration::from_secs(5)).unwrap();
    let (detail,said,reports)=view.spot_view(id).unwrap().unwrap();
    assert_eq!(detail.xml,store::Xml::Kept);assert_eq!(detail.spot,full::parse(XML).unwrap());
    assert_eq!(detail.spot.nzb,vec!["real@spot.net".to_owned()]);assert!(detail.spot.description.contains("alpha\0beta"));
    assert_eq!(said.len(),1);assert_eq!(said[0].body,"é".repeat(comments::MAX_BODY));assert_eq!(reports,0);
    let tags=view.spot_tags(&[id]).unwrap();assert_eq!(tags[&id].title,detail.spot.title);assert_eq!(tags[&id].kind,"series");
    let reads=crate::pg::stage_reads::StepReads::for_owner(&runtime.owner).unwrap();
    assert_eq!(reads.spot_detail(id).unwrap().unwrap().spot,detail.spot);
    assert_eq!(reads.category_spot(id).unwrap().unwrap().0,crate::spotnet::Kind::Series);
    assert_eq!(reads.snapshot(id).unwrap().unwrap().info.spot_kind,Some(crate::spotnet::Kind::Series));
    drop(view);runtime.owner.close();assert!(reads.spot_detail(id).is_err());
}

#[test]
#[ignore="explicit isolated PG; repeatable original snapshot and disposal semantics"]
fn native_original_spot_snapshot_does_not_mix_newer_body_and_removed_tags() {
    let (schema,runtime,_dir,id)=fixture();let mut client=schema.pool.get().unwrap();
    let view=crate::pg::fresh_reader::ReadView::open(&mut client,&schema.name,crate::pg::fresh_reader::Budget::default(),Instant::now()+Duration::from_secs(5)).unwrap();
    let before=view.spot_view(id).unwrap().unwrap();
    crate::spotnet::native_read_fixture_tests::replace_comment_and_note_disposal(&runtime);
    assert_eq!(view.spot_view(id).unwrap().unwrap().1,before.1,"same RR cannot mix later body state");
    assert!(view.spot_tags(&[id]).unwrap().contains_key(&id));drop(view);
    let view=crate::pg::fresh_reader::ReadView::open(&mut client,&schema.name,crate::pg::fresh_reader::Budget::default(),Instant::now()+Duration::from_secs(5)).unwrap();
    assert!(!view.spot_tags(&[id]).unwrap().contains_key(&id));
    let after=view.spot_view(id).unwrap().unwrap();assert_eq!(after.1[0].body,"New supplied text");
    assert_eq!(after.0.removed.unwrap().by,store::RemovedBy::Moderator);drop(view);runtime.owner.close();
}

#[test]
#[ignore="explicit isolated PG; missing integrity/original, malformed and admission refusal"]
fn native_original_spot_malformed_missing_and_overbudget_never_become_display_fallbacks() {
    for mutation in ["update spots set native_original_xml=null","update spots set native_original_xml=decode('ff','hex')","update spots set native_original_xml=convert_to('<broken/>','UTF8'),xml='<broken/>'","update spot_comments set native_original_body=null"] {
        let (schema,runtime,_dir,id)=fixture();runtime.owner.with(|client|{client.batch_execute(mutation)?;Ok(())}).unwrap();
        let mut client=schema.pool.get().unwrap();let view=crate::pg::fresh_reader::ReadView::open(&mut client,&schema.name,crate::pg::fresh_reader::Budget::default(),Instant::now()+Duration::from_secs(5)).unwrap();
        assert!(view.spot_view(id).is_err(),"original integrity errors cannot become absence or sanitized fallback");drop(view);runtime.owner.close();
    }
    let (bounded_schema,bounded_runtime,_bounded_dir,bounded_id)=fixture();
    let mut bounded_client=bounded_schema.pool.get().unwrap();
    let bounded_view=crate::pg::fresh_reader::ReadView::open(&mut bounded_client,&bounded_schema.name,crate::pg::fresh_reader::Budget {rows:1,bytes:131072},Instant::now()+Duration::from_secs(5)).unwrap();
    assert!(bounded_view.spot_view(bounded_id).is_err(),"actual original plus display/decoder ownership must fit the existing request admission");
    drop(bounded_view);bounded_runtime.owner.close();
    let (schema,runtime,_dir,id)=fixture();
    runtime.owner.with(|client|{client.execute("update spots set native_original_xml=$1,xml=$2",&[&vec![b'x';crate::spotnet::MAX_XML_BYTES+1],&"x".repeat(crate::spotnet::MAX_XML_BYTES+1)])?;Ok(())}).unwrap();
    let mut client=schema.pool.get().unwrap();let view=crate::pg::fresh_reader::ReadView::open(&mut client,&schema.name,crate::pg::fresh_reader::Budget::default(),Instant::now()+Duration::from_secs(5)).unwrap();
    assert!(view.spot_view(id).is_err());drop(view);runtime.owner.close();
}

#[test]
#[ignore="explicit isolated PG; actual work revision and replacement-owner observations"]
fn native_original_spot_observation_refuses_stale_and_replacement_owner_settlement() {
    use crate::pg::stage_queue::{Queue,Visit,Settlement};
    use crate::postprocess::{apply::Applied,step::{StepWrite,StepMutation}};
    let (schema,runtime,dir,id)=fixture();let reads=crate::pg::stage_reads::StepReads::for_owner(&runtime.owner).unwrap();
    assert!(reads.spot_detail(id).unwrap().is_some());let observed=reads.observed().unwrap();
    let queue=Queue::for_owner(&runtime.owner).unwrap();let claim=queue.claim(10,1,60).unwrap().pop().unwrap();
    let applied=||Applied {release_id:id,set_done:4,writes:vec![(4,StepWrite::for_mutation(id,StepMutation::SetCategory {category:5000}))],..Default::default()};
    runtime.owner.with(|client|{client.execute("update releases set native_work_revision=native_work_revision+1 where id=$1",&[&id])?;Ok(())}).unwrap();
    assert_eq!(queue.settle(&claim,Visit::prepare_observed(&claim,applied(),[9;32],observed.clone()).unwrap(),11).unwrap(),Settlement::Superseded);
    runtime.owner.close();assert!(reads.spot_detail(id).is_err());
    let mut config=crate::config::Config::default();config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;
    let replacement=crate::pg::indexer_runtime::Runtime::open(&config,crate::pg::StorageConfig {backend:crate::pg::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:1,allow_plaintext:true},crate::config::HeaderStoreConfig {enabled:true,dir:dir.path().to_str().unwrap().into()},&[],&mut None).unwrap();
    let queue=Queue::for_owner(&replacement.owner).unwrap();let claim=queue.claim(100,1,60).unwrap().pop().unwrap();
    assert!(Visit::prepare_observed(&claim,applied(),[9;32],observed).is_err());replacement.owner.close();
}

#[test]
#[ignore="explicit isolated PG; original Owner snapshot kind pretransfer admission"]
fn native_original_owner_snapshot_refuses_oversized_and_malformed_kind_before_fallback() {
    for (mutation,expected) in [
        ("update spots set kind=repeat('x',8388608)","native Spotnet field exceeds original XML admission"),
        ("update spots set kind='not-a-kind'","native Spotnet kind does not match its original"),
    ] {
        let (schema,runtime,_dir,id)=fixture();
        runtime.owner.with(|client|{client.execute("update pp_state set net_left=37 where release_id=$1",&[&id])?;Ok(())}).unwrap();
        let reads=crate::pg::stage_reads::StepReads::for_owner(&runtime.owner).unwrap();
        let positive=reads.snapshot(id).unwrap().unwrap();
        assert_eq!(positive.info.spot_kind,Some(crate::spotnet::Kind::Series));
        assert_eq!(positive.net_left,Some(37),"removing kind must preserve the following net_left column");
        let before=runtime.owner.with(|client|Ok(client.query_one("select native_work_revision from releases where id=$1",&[&id])?.get::<_,i64>(0))).unwrap();
        runtime.owner.with(|client|{client.batch_execute(mutation)?;Ok(())}).unwrap();
        // This asserts actual owned refusal/error semantics. Removing the
        // unused SQL projection supplies the pretransfer bound; this assertion
        // alone is not a peak-memory measurement or a semantic RED claim.
        let error=reads.snapshot(id).err().expect("oversized or malformed stored kind must refuse the owned snapshot");
        assert!(error.chain().any(|cause|cause.to_string().contains(expected)),"preserve original admission/integrity error chain: {error:#}");
        assert_eq!(crate::pg::outage::classify(&error),crate::pg::outage::Failure::Fatal);
        assert_eq!(runtime.owner.failure(),Some(crate::pg::outage::Failure::Fatal));
        assert!(runtime.owner.stop.load(std::sync::atomic::Ordering::Acquire));
        let mut observer=schema.pool.get().unwrap();
        assert_eq!(observer.query_one("select native_work_revision from releases where id=$1",&[&id]).unwrap().get::<_,i64>(0),before,"refused reads cannot mutate source revision");
        assert_eq!(observer.query_one("select count(*) from native_operations",&[]).unwrap().get::<_,i64>(0),0,"a failed read cannot mint a settlement receipt");
        runtime.owner.close();
    }
}
