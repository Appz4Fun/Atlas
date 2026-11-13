//! Local Step families execute through the genuine queue/owner transaction.
use super::stage_queue::{Queue,Visit,Settlement};
use crate::postprocess::{apply::Applied,step::{StepMutation,StepWrite}};

#[test]
fn native_local_result_families_preserve_exact_values_and_share_the_receipt_commit() {
    let Some(schema)=super::testing::Schema::new() else {return};
    let runtime=super::stage_queue_tests::runtime(&schema);super::stage_queue_tests::push(&runtime);
    let queue=Queue::for_owner(&runtime.owner).unwrap();let claims=queue.claim(10,1,60).unwrap();let claim=&claims[0];let id=claim.release_id();
    let mutations=vec![
        StepMutation::RecordSegmentCrc {message_id:"<exact\0ID@example.test>".into(),decoded:17,crc32:u32::MAX},
        StepMutation::KeepReleaseKeys {keys:crate::keys::Keys {mux:Some(-1),video:Some(9),..Default::default()}},
        StepMutation::SetNfoPassword {password:String::new()},
        StepMutation::KeepStill(crate::raw_store::SavedStill {position:25,at_ms:Some(1000),sha256:"00".repeat(32),width:8,height:9,phash:Some(-1)}),
        StepMutation::KeepAudioPrint {position:50,at_ms:1000,items:vec![0,u32::MAX]},
        StepMutation::KeepAudioAnchor {anchor:-1},
        StepMutation::SetArchivePassword {password:crate::archive::Password::Yes},
        StepMutation::KeepArchiveFile {sequence:0,source:1,file:crate::archive::InnerFile {name:"inner.bin".into(),size:Some(42),crc32:Some(u32::MAX),mtime:Some(-1),host_os:None}},
        StepMutation::AdjustArchiveNameBytes {delta:41},
        StepMutation::KeepRevVolume(crate::archive::rev::Volume {index:0,size:42,crc32:u32::MAX}),
        StepMutation::SetPar2Sizes {block:1024,blocks:4},
        StepMutation::KeepPar2File {file:crate::par2::Par2File {file_id:[1;16],set_id:[2;16],md5:[3;16],md5_16k:[4;16],size:42,name:"sample.bin".into(),bound:true,crc32:Some(u32::MAX)},sealed:true,complete:true},
        StepMutation::SetPar2Creator {set_id:[2;16],creator:"Fixture".into()},
    ];
    let a=Applied {release_id:id,set_done:4,writes:mutations.into_iter().map(|mutation|(4,StepWrite::for_mutation(id,mutation))).collect(),..Default::default()};
    assert_eq!(queue.settle(claim,Visit::prepare(claim,a,[9;32]).unwrap(),11).unwrap(),Settlement::Applied);
    runtime.owner.with(|client| {
        let row=client.query_one("select message_id,crc32 from segment_crc",&[])?;
        assert_eq!(row.get::<_,Vec<u8>>(0),b"<exact\0ID@example.test>");assert_eq!(row.get::<_,i64>(1),i64::from(u32::MAX));
        assert_eq!(client.query_one("select password from release_nfo_password",&[])?.get::<_,String>(0),"");
        assert_eq!(client.query_one("select mux from release_keys",&[])?.get::<_,i64>(0),-1);
        assert_eq!(client.query_one("select phash from release_stills",&[])?.get::<_,i64>(0),-1);
        assert_eq!(crate::audio::from_blob(&client.query_one("select items from release_audio_print",&[])?.get::<_,Vec<u8>>(0)),vec![0,u32::MAX]);
        assert_eq!(client.query_one("select crc32 from rev_volumes",&[])?.get::<_,i64>(0),i64::from(u32::MAX));
        assert_eq!(client.query_one("select creator from par2_files",&[])?.get::<_,String>(0),"Fixture");
        assert_eq!(client.query_one("select count(*) from native_operations",&[])?.get::<_,i64>(0),1);Ok(())
    }).unwrap();runtime.owner.close();
}
