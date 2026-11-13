use super::*;
use sha2::Sha256;
use crate::pg;
use crate::postprocess::{apply::Applied,step::{StepWrite,StepMutation,ImageRecord}};
const IMAGE:&[u8]=crate::postprocess::native::image_asset_tests::IMAGE;
fn selected()->(tempfile::TempDir,crate::config::NativeAssetsConfig) {
    let home=tempfile::tempdir().unwrap();let root=home.path().join("assets");std::fs::create_dir(&root).unwrap();std::fs::create_dir(root.join("images")).unwrap();std::fs::write(root.join("images/original"),b"captured original").unwrap();
    let path=home.path().join("assets.jsonl");pg::testing::capture_asset_manifest(&root,&path,pg::testing::AssetCaptureLimits {min_files:1,max_files:8,max_file_bytes:1<<20,max_total_bytes:8<<20,read_bytes_per_second:32<<20}).unwrap();
    let config=crate::config::NativeAssetsConfig {root:root.canonicalize().unwrap().to_str().unwrap().into(),manifest:path.canonicalize().unwrap().to_str().unwrap().into(),whole_sha256:pg::fresh_assets::hex(&Sha256::digest(std::fs::read(path).unwrap()))};(home,config)
}
fn source(runtime:&pg::indexer_runtime::Runtime)->i64 {
    runtime.owner.with(|client| {
        let saved=pg::release_writer::save(client,&[crate::parser::Release {name:"Native.Image.Frame".into(),poster:"poster".into(),group:"alt.binaries.test".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
            articles:vec![crate::parser::Article {filename:Some("cover.gif".into()),subject:"cover.gif yEnc (1/1)".into(),message_id:"frame-image@fixture.invalid".into(),part:Some(1),total_parts:Some(1),bytes:IMAGE.len() as i64,..Default::default()}],..Default::default()}],-100000)?;
        assert_eq!((saved.releases,saved.files,saved.parts),(1,1,1));Ok(client.query_one("select id from releases",&[])?.get(0))
    }).unwrap()
}
#[test]
#[ignore="explicit isolated PG; actual image writer receipt, cache association, MIME and held scope"]
fn native_image_frames_verify_cache_receipts_and_refuse_corrupt_or_foreign_metadata() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PG required");let runtime=pg::stage_queue_tests::runtime(&schema);let id=source(&runtime);let (home,config)=selected();let grant=Grant::open(&runtime.owner,Some(&config)).unwrap().unwrap();
    let mut frame=grant.image_frame(id,1<<20).unwrap();assert!(frame.keep(IMAGE).unwrap());let frame=frame.finish().unwrap();frame.check(&runtime.owner,id).unwrap();assert!(frame.check(&runtime.owner,id+1).is_err());
    let sha=pg::fresh_assets::hex(&Sha256::digest(IMAGE));
    runtime.owner.with(|client|{assert_eq!(client.query_one("select count(*) from release_images",&[])?.get::<_,i64>(0),0,"receipt alone never becomes a public image association");Ok(())}).unwrap();
    let queue=pg::stage_queue::Queue::for_owner(&runtime.owner).unwrap();let claim=queue.claim(10,1,60).unwrap().pop().unwrap();let bit=crate::postprocess::step::mask(crate::images::step::IMAGES_STEP);
    let image=ImageRecord {file:"cover.gif".into(),sha256:sha.clone(),format:"gif".into(),width:1,height:1,bytes:IMAGE.len() as i64,meta:None,exif_id:None,document_id:None};
    let applied=Applied {release_id:id,set_done:bit,writes:vec![(bit,StepWrite::for_mutation(id,StepMutation::KeepImage {position:0,image}))],..Default::default()};
    let visit=pg::stage_queue::Visit::prepare(&claim,applied,Sha256::digest(IMAGE).into()).unwrap();assert_eq!(queue.settle(&claim,visit,11).unwrap(),pg::stage_queue::Settlement::Applied);drop(frame);
    let mut cached=grant.image_frame(id,1<<20).unwrap();assert_eq!(cached.cached("cover.gif",&sha).unwrap().unwrap(),IMAGE);assert!(cached.keep(IMAGE).unwrap());cached.finish().unwrap().check(&runtime.owner,id).unwrap();
    runtime.owner.with(|client|{client.execute("update ext_asset_files set source_digest=decode(repeat('00',32),'hex') where kind='images' and name=$1",&[&sha])?;Ok(())}).unwrap();
    assert!(grant.image_frame(id,1<<20).unwrap().cached("cover.gif",&sha).is_err(),"a foreign namespace is an error, not a cache miss/provider retry");
    runtime.owner.with(|client|{let source=grant.manifest.native_source(AssetKind::Images, runtime.owner.epoch().unwrap().catalog())?;client.execute("update ext_asset_files set source_digest=$1 where kind='images' and name=$2",&[&source.as_slice(),&sha])?;client.execute("update release_images set format='png' where release_id=$1",&[&id])?;Ok(())}).unwrap();
    assert!(grant.image_frame(id,1<<20).unwrap().cached("cover.gif",&sha).is_err(),"metadata MIME and actual GIF disagree");
    runtime.owner.with(|client|{client.execute("update release_images set format='gif' where release_id=$1",&[&id])?;Ok(())}).unwrap();
    std::fs::write(home.path().join("assets/images").join(&sha),vec![b'X';IMAGE.len()]).unwrap();assert!(grant.image_frame(id,1<<20).unwrap().cached("cover.gif",&sha).is_err(),"corruption cannot become a cache miss or overwrite");
    assert_eq!(std::fs::read(home.path().join("assets/images").join(&sha)).unwrap(),vec![b'X';IMAGE.len()]);runtime.owner.close();
}
#[test]
#[ignore="explicit isolated PG; normal header revision, cancellation and root identity"]
fn native_image_frame_changes_leave_only_inert_files_and_never_mint_a_late_association() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PG required");let runtime=pg::stage_queue_tests::runtime(&schema);let id=source(&runtime);let (home,config)=selected();let grant=Grant::open(&runtime.owner,Some(&config)).unwrap().unwrap();
    let mut stale=grant.image_frame(id,1<<20).unwrap();runtime.owner.with(|client| {
        pg::release_writer::save(client,&[crate::parser::Release {name:"Native.Image.Frame".into(),poster:"poster".into(),group:"alt.binaries.test".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
            articles:vec![crate::parser::Article {filename:Some("cover.gif".into()),subject:"cover.gif yEnc (2/2)".into(),message_id:"grown-frame-image@fixture.invalid".into(),part:Some(2),total_parts:Some(2),bytes:IMAGE.len() as i64,..Default::default()}],..Default::default()}],1)?;
        assert!(client.query_one("select native_work_revision from releases where id=$1",&[&id])?.get::<_,i64>(0)>stale.revision);Ok(())
    }).unwrap();
    assert!(stale.keep(IMAGE).unwrap_err().is::<ImageChanged>());assert_eq!(runtime.owner.failure(),None,"ordinary supersession is not an owner failure");
    let mut fresh=grant.image_frame(id,1<<20).unwrap();assert!(fresh.keep(IMAGE).unwrap());drop(fresh);runtime.owner.with(|client|{assert_eq!(client.query_one("select count(*) from release_images",&[])?.get::<_,i64>(0),0);Ok(())}).unwrap();
    let hold=grant.image_frame(id,1<<20).unwrap().finish().unwrap();std::fs::rename(home.path().join("assets"),home.path().join("old-assets")).unwrap();std::fs::create_dir(home.path().join("assets")).unwrap();assert!(hold.check(&runtime.owner,id).is_err());
    runtime.owner.close();assert!(hold.check(&runtime.owner,id).is_err());assert!(grant.image_frame(id,1<<20).is_err());
}
