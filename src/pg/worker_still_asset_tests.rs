//! Actual native JPEG bytes, original owner, Prepared settlement and signed-in asset routing.
//! Root runs these only with an isolated PG database/private ATLAS_HOME and HTTP spool admission.
use super::*;
use crate::{pg,pg::worker_assets::{Grant,StillFrame},raw_store::SavedStill};
use std::io::Read;
use sha2::{Sha256,Digest};

fn hex(bytes:&[u8])->String {bytes.iter().map(|byte|format!("{byte:02x}")).collect()}
fn jpeg(value:u8)->Vec<u8> {crate::video::jpeg::encode(&vec![value;16*16],&vec![128;8*8],&vec![128;8*8],16,16,85)}
fn setup(schema:&pg::testing::Schema)->(pg::indexer_runtime::Runtime,tempfile::TempDir,crate::config::NativeAssetsConfig,i64) {
    let home=tempfile::tempdir().unwrap();let root=home.path().join("assets");std::fs::create_dir(&root).unwrap();std::fs::create_dir(root.join("stills")).unwrap();
    let original=jpeg(10);let name=hex(&Sha256::digest(&original));std::fs::write(root.join("stills").join(&name),original).unwrap();
    let manifest=home.path().join("assets.jsonl");pg::testing::capture_asset_manifest(&root,&manifest,pg::testing::AssetCaptureLimits {min_files:1,max_files:8,max_file_bytes:1<<20,max_total_bytes:8<<20,read_bytes_per_second:32<<20}).unwrap();
    let selection=crate::config::NativeAssetsConfig {root:root.canonicalize().unwrap().to_str().unwrap().into(),manifest:manifest.canonicalize().unwrap().to_str().unwrap().into(),whole_sha256:hex(&Sha256::digest(std::fs::read(&manifest).unwrap()))};
    let mut config=crate::config::Config::default();config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;
    config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;
    let headers=crate::config::HeaderStoreConfig {enabled:true,dir:home.path().join("headers").to_str().unwrap().into()};
    let runtime=pg::indexer_runtime::Runtime::open(&config,pg::StorageConfig {backend:pg::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:1,allow_plaintext:true},headers,&[],&mut None).unwrap();
    let id=runtime.owner.with(|client| {
        pg::release_writer::save(client,&[release("<native-still-1@fixture.invalid>",1)],-100000)?;
        Ok(client.query_one("select id from releases",&[])?.get::<_,i64>(0))
    }).unwrap();(runtime,home,selection,id)
}
fn release(mid:&str,part:i64)->crate::parser::Release {crate::parser::Release {name:"Native.Still.Publication".into(),group:"alt.binaries.test".into(),poster:"poster".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
    articles:vec![crate::parser::Article {filename:Some("video.mkv".into()),subject:format!("video.mkv yEnc ({part}/2)"),message_id:mid.into(),part:Some(part),total_parts:Some(2),bytes:123,..Default::default()}],..Default::default()}}
fn generated(grant:&Arc<Grant>,id:i64)->(StillFrame,Vec<SavedStill>) {
    let mut frame=grant.still_frame(id).unwrap();let mut stills=Vec::new();
    for (n,position) in crate::probe::stills::POSITIONS.into_iter().enumerate() {
        let value=50+n as u8;let name=frame.keep(&jpeg(value),16,16).unwrap();
        let luma=crate::video::picture::Plane8 {data:vec![value;16*16],width:16,height:16};
        let phash=crate::video::phash::phash(&luma).map(|value|value as i64);
        stills.push(SavedStill {position,at_ms:Some(n as i64*1000),sha256:name,width:16,height:16,phash});
    }
    (frame.finish().unwrap(),stills)
}
fn prepare(runtime:&pg::indexer_runtime::Runtime,id:i64,frame:StillFrame,stills:&[SavedStill])->(pg::stage_queue::Queue,pg::stage_queue::Claim,Prepared) {
    prepare_at(runtime,id,frame,stills,10)
}
fn prepare_at(runtime:&pg::indexer_runtime::Runtime,id:i64,frame:StillFrame,stills:&[SavedStill],now:i64)->(pg::stage_queue::Queue,pg::stage_queue::Claim,Prepared) {
    use crate::postprocess::{apply::Applied,step::{StepWrite,StepMutation}};
    let queue=pg::stage_queue::Queue::for_owner(&runtime.owner).unwrap();let claim=queue.claim(now,1,60).unwrap().pop().expect("real writer-generated queue claim");assert_eq!(claim.release_id(),id);
    let bit=mask(crate::probe::stills::STILLS_STEP);let mut writes=vec![(bit,StepWrite::for_mutation(id,StepMutation::ClearStills))];
    writes.extend(stills.iter().cloned().map(|still|(bit,StepWrite::for_mutation(id,StepMutation::KeepStill(still)))));
    let visit=pg::stage_queue::Visit::prepare(&claim,Applied {release_id:id,set_done:bit,writes,..Default::default()},Sha256::digest(b"native-generated-stills").into()).unwrap();
    let prepared=Prepared {visit:Some(visit),owner:runtime.owner.clone(),holds:vec![Box::new(frame)]};(queue,claim,prepared)
}
fn http(runtime:&pg::indexer_runtime::Runtime,selection:&crate::config::NativeAssetsConfig,id:i64)->crate::api::ApiResponse {
    let assets=pg::fresh_assets::AssetManifest::from_selection(selection).unwrap();let storage=&runtime.owner.cache_destination().unwrap().storage;
    let cookie=format!("atlas_session={}",crate::web::session_token("owned-still-fixture"));
    crate::web::get_with_storage_and_assets("/ui/still",&format!("id={id}&at=25"),Some(&cookie),"","owned-still-fixture",storage,Some(&assets)).unwrap()
}
#[test]
#[ignore="Root-only isolated PG, private ATLAS_HOME and real HTTP spool disk admission"]
fn native_generated_stills_prepared_settlement_serves_exact_jpeg_without_manifest_rewrite() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PG");let (runtime,home,selection,id)=setup(&schema);
    let original=std::fs::read(&selection.manifest).unwrap();assert_ne!(std::path::Path::new(&selection.root),crate::paths::app_dir());
    let grant=Grant::open(&runtime.owner,Some(&selection)).unwrap().unwrap();let (frame,stills)=generated(&grant,id);
    assert_eq!(http(&runtime,&selection,id).status,404,"a file/receipt alone is not a public still association");
    let (queue,claim,prepared)=prepare(&runtime,id,frame,&stills);assert_eq!(prepared.settle(&queue,&claim,11).unwrap(),pg::stage_queue::Settlement::Applied);
    let mut response=http(&runtime,&selection,id);assert_eq!(response.status,200);assert_eq!(response.content_type,Some("image/jpeg"));
    let mut bytes=Vec::new();response.spool.take().unwrap().read_to_end(&mut bytes).unwrap();assert_eq!(bytes,jpeg(50));
    assert_eq!(std::fs::read(&selection.manifest).unwrap(),original);assert_eq!(std::fs::read(home.path().join("assets/stills").join(&stills[0].sha256)).unwrap(),bytes);
    let mut cached=grant.still_frame(id).unwrap();for still in &stills {assert!(cached.reuse(&still.sha256).unwrap());}cached.finish().unwrap().check(&runtime.owner,id).unwrap();
    runtime.owner.close();
}
#[test]
#[ignore="Root-only isolated PG; captured authority, cache cadence and source mismatch"]
fn native_stills_preserve_captured_originals_and_refuse_foreign_receipts() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PG");let (runtime,home,selection,id)=setup(&schema);let grant=Grant::open(&runtime.owner,Some(&selection)).unwrap().unwrap();
    let captured=jpeg(10);let sha=hex(&Sha256::digest(&captured));let manifest=std::fs::read(&selection.manifest).unwrap();
    let mut frame=grant.still_frame(id).unwrap();assert_eq!(frame.keep(&captured,16,16).unwrap(),sha);drop(frame.finish().unwrap());
    runtime.owner.with(|client|{assert_eq!(client.query_one("select count(*) from ext_asset_files where kind='stills'",&[])?.get::<_,i64>(0),0,"captured manifest authority is not restamped as native");Ok(())}).unwrap();
    // Sparse, unlisted content exercises the real 2GiB trim boundary without a large payload read.
    let old=home.path().join("assets/stills").join("00".repeat(32));std::fs::File::create(&old).unwrap().set_len(2<<30).unwrap();
    let mut frame=grant.still_frame(id).unwrap();let made=frame.keep(&jpeg(90),16,16).unwrap();let pending=frame.finish().unwrap();assert!(!old.exists());
    assert_eq!(std::fs::read(home.path().join("assets/stills").join(&sha)).unwrap(),captured);assert_eq!(std::fs::read(&selection.manifest).unwrap(),manifest);
    let later=home.path().join("assets/stills").join("11".repeat(32));std::fs::File::create(&later).unwrap().set_len(2<<30).unwrap();
    for n in 0..63 {let mut frame=grant.still_frame(id).unwrap();frame.keep(&jpeg(100+n),16,16).unwrap();drop(frame.finish().unwrap());}
    assert!(later.exists(),"the existing every64-write cadence is retained");
    let mut frame=grant.still_frame(id).unwrap();frame.keep(&jpeg(200),16,16).unwrap();drop(frame.finish().unwrap());
    assert!(!later.exists());assert_eq!(std::fs::read(home.path().join("assets/stills").join(&sha)).unwrap(),captured);
    pending.check(&runtime.owner,id).unwrap();assert_eq!(std::fs::read(home.path().join("assets/stills").join(&made)).unwrap(),jpeg(90),"pending Prepared bytes cannot be evicted by a sibling trim");drop(pending);
    runtime.owner.with(|client|{client.execute("update ext_asset_files set source_digest=decode(repeat('00',32),'hex') where kind='stills' and name=$1",&[&made])?;Ok(())}).unwrap();
    assert!(grant.still_available(&made).is_err(),"a receipt from another namespace is not permission to serve or reuse");runtime.owner.close();
}
#[test]
#[ignore="Root-only isolated PG; actual supersession and stopped owner hold checks"]
fn native_still_prepared_supersession_and_owner_loss_never_clear_the_prior_association() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PG");let (runtime,_home,selection,id)=setup(&schema);let grant=Grant::open(&runtime.owner,Some(&selection)).unwrap().unwrap();
    let (prior,stills)=generated(&grant,id);let (queue,claim,prepared)=prepare(&runtime,id,prior,&stills);
    assert_eq!(prepared.settle(&queue,&claim,11).unwrap(),pg::stage_queue::Settlement::Applied);
    let want=stills.iter().map(|still|still.sha256.clone()).collect::<Vec<_>>();
    queue.request_walk(mask(crate::probe::stills::STILLS_STEP),None,20).unwrap();queue.walk_tick(20).unwrap();
    let (frame,next)=generated(&grant,id);let (queue,claim,prepared)=prepare_at(&runtime,id,frame,&next,20);
    runtime.owner.with(|client|{pg::release_writer::save(client,&[release("<native-still-2@fixture.invalid>",2)],21)?;Ok(())}).unwrap();
    assert_eq!(prepared.settle(&queue,&claim,22).unwrap(),pg::stage_queue::Settlement::Superseded);assert_eq!(runtime.owner.failure(),None);
    let (stopped,next)=generated(&grant,id);let (queue,claim,prepared)=prepare_at(&runtime,id,stopped,&next,100000);
    let held=grant.still_frame(id).unwrap().finish().unwrap();runtime.owner.close();
    assert!(prepared.settle(&queue,&claim,100001).is_err());assert!(held.check(&runtime.owner,id).is_err());assert!(grant.still_frame(id).is_err());
    let other_schema=pg::testing::Schema::new().expect("explicit second isolated PG schema");let (other,_other_home,_selection,other_id)=setup(&other_schema);
    assert!(held.check(&other.owner,other_id).is_err(),"a retained frame cannot move to another actual epoch/owner");other.owner.close();
    let mut observer=schema.pool.get().unwrap();let actual=observer.query("select sha256 from release_stills where release_id=$1 order by position",&[&id]).unwrap().into_iter().map(|row|row.get::<_,String>(0)).collect::<Vec<_>>();
    assert_eq!(actual,want,"supersession and closed-owner settlement preserve the prior genuine association");
}
#[test]
#[ignore="Root-only isolated PG; width, output horizon, physical namespace and refusal preservation"]
fn native_still_admission_and_symlink_refusals_leave_prior_stills_untouched() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PG");let (runtime,home,selection,id)=setup(&schema);let grant=Grant::open(&runtime.owner,Some(&selection)).unwrap().unwrap();
    let (prior,stills)=generated(&grant,id);let (queue,claim,prepared)=prepare(&runtime,id,prior,&stills);
    assert_eq!(prepared.settle(&queue,&claim,11).unwrap(),pg::stage_queue::Settlement::Applied);
    let busy=(0..128).map(|_|grant.still_frame(id).unwrap()).collect::<Vec<_>>();
    let refusal=grant.still_frame(id).err().expect("actual existing 128-request spool admission");
    assert!(refusal.downcast_ref::<std::io::Error>().and_then(std::io::Error::get_ref).is_some_and(|cause|cause.is::<crate::nzb_spool::AdmissionError>()));
    assert_eq!(runtime.owner.failure(),None,"local spool admission cannot poison the owner");drop(busy);
    let mut frame=grant.still_frame(id).unwrap();assert!(frame.keep(&jpeg(20),961,16).is_err());assert!(frame.keep(&jpeg(20),16,1921).is_err());for n in 0..3 {frame.keep(&jpeg(40+n),16,16).unwrap();}
    assert!(frame.keep(&jpeg(99),16,16).is_err(),"a fourth unique image exceeds the producer horizon");drop(frame);
    let held=grant.still_frame(id).unwrap().finish().unwrap();std::fs::rename(home.path().join("assets"),home.path().join("original-assets")).unwrap();
    std::os::unix::fs::symlink(home.path().join("original-assets"),home.path().join("assets")).unwrap();assert!(held.check(&runtime.owner,id).is_err());
    assert!(grant.still_frame(id).is_err());
    runtime.owner.with(|client| {
        let kept=client.query("select sha256 from release_stills where release_id=$1 order by position",&[&id])?.into_iter().map(|row|row.get::<_,String>(0)).collect::<Vec<_>>();
        assert_eq!(kept,stills.iter().map(|still|still.sha256.clone()).collect::<Vec<_>>(),"admission/root refusal cannot clear prior genuine stills");Ok(())
    }).unwrap();runtime.owner.close();
}

#[test]
#[ignore="Root-only isolated PG; actual sibling trim after a prior still snapshot"]
fn native_still_final_choice_recounts_after_trim_instead_of_discarding_valid_new_output() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PG");let (runtime,home,selection,id)=setup(&schema);let grant=Grant::open(&runtime.owner,Some(&selection)).unwrap().unwrap();
    let (prior,stills)=generated(&grant,id);let (queue,claim,prepared)=prepare(&runtime,id,prior,&stills);
    assert_eq!(prepared.settle(&queue,&claim,11).unwrap(),pg::stage_queue::Settlement::Applied);
    let kept=stills.iter().map(|still|(still.sha256.clone(),still.phash.is_some())).collect::<Vec<_>>();
    let mut candidate=grant.still_frame(id).unwrap();let sha=candidate.keep(&jpeg(220),16,16).unwrap();
    let sparse=home.path().join("assets/stills").join("22".repeat(32));std::fs::File::create(&sparse).unwrap().set_len(2<<30).unwrap();
    for value in 100..160 {let mut other=grant.still_frame(id).unwrap();other.keep(&jpeg(value),16,16).unwrap();drop(other.finish().unwrap());}
    let mut other=grant.still_frame(id).unwrap();other.keep(&jpeg(200),16,16).unwrap();drop(other.finish().unwrap());
    for (hash,_) in &kept {assert!(!grant.still_available(hash).unwrap(),"the real sibling trim removed the unheld old set");}
    assert!(candidate.prefer_previous(&kept,1).unwrap().is_none(),"a stale pre-BODY count must not discard the new held JPEG");
    queue.request_walk(mask(crate::probe::stills::STILLS_STEP),None,20).unwrap();queue.walk_tick(20).unwrap();
    let luma=crate::video::picture::Plane8 {data:vec![220;16*16],width:16,height:16};
    let next=[SavedStill {position:25,at_ms:Some(0),sha256:sha.clone(),width:16,height:16,phash:crate::video::phash::phash(&luma).map(|value|value as i64)}];
    let (queue,claim,prepared)=prepare_at(&runtime,id,candidate.finish().unwrap(),&next,20);
    assert_eq!(prepared.settle(&queue,&claim,21).unwrap(),pg::stage_queue::Settlement::Applied);
    runtime.owner.with(|client| {let rows=client.query("select sha256 from release_stills where release_id=$1",&[&id])?;assert_eq!(rows.len(),1);assert_eq!(rows[0].get::<_,String>(0),sha);Ok(())}).unwrap();runtime.owner.close();
}
