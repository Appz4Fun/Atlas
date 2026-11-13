//! Real canonical-owner observations and settlement, never asset/payload grants.
use super::*;
use crate::postprocess::{apply::Applied,data::StepData,step::{StepError,StepMutation,StepWrite}};
use crate::pg::stage_queue::{Queue,Visit,Settlement};
const STAMP:u64=127;
const BIT:i64=crate::postprocess::step::mask(crate::probe::step::PROBE_STEP);

fn probe()->crate::probe::ProbeInfo {
    crate::probe::ProbeInfo {
        file:Some("video.mkv".into()),
        yenc:Some(crate::probe::yenc::YencLines {file_size:Some(8192),..Default::default()}),
        file_crc32:Some(0x12345678),moviehash:Some("0011223344556677".into()),
        // Retained producer whole-rule1/stamp127 fact, not runtime authority.
        media_whole:Some(4_294_967_423),
        media:Some(crate::probe::media::MediaInfo {
            container:"matroska".into(),segment_uid:Some("00112233445566778899aabbccddeeff".into()),
            muxed_at:Some(1700000000),header_complete:true,encoder_complete:true,
            attachments_complete:true,chapters_complete:true,
            video:vec![crate::probe::media::VideoTrack {codec:"V_AV1".into(),..Default::default()}],
            ..Default::default()
        }),..Default::default()
    }
}
fn fixture(runtime:&crate::pg::indexer_runtime::Runtime)->(i64,i64,crate::probe::ProbeInfo) {
    let info=probe();let json=serde_json::to_string(&info).unwrap();
    let mux=crate::keys::as_key(crate::keys::digest8(crate::keys::mux_text(info.media.as_ref().unwrap()).unwrap().as_bytes()));
    let ids=runtime.owner.with(|client| {
        for name in ["SameMux.Own","SameMux.Donor"] {
            crate::pg::release_writer::save(client,&[crate::parser::Release {name:name.into(),group:"example.group".into(),articles:vec![crate::parser::Article {
                filename:Some("video.mkv".into()),message_id:format!("<{name}@example.test>"),part:Some(1),total_parts:Some(1),bytes:8192,..Default::default()
            }],..Default::default()}],-10000)?;
        }
        let rows=client.query("select id from releases order by id",&[])?;let own=rows[0].try_get::<_,i64>(0)?;let donor=rows[1].try_get::<_,i64>(0)?;
        client.execute("insert into release_probe(release_id,info,moviehash) values($1,$2,$3)",&[&donor,&json,&info.moviehash])?;
        client.execute("insert into release_keys(release_id,mux) values($1,$2)",&[&donor,&mux])?;Ok((own,donor))
    }).unwrap();(ids.0,ids.1,info)
}
fn result(id:i64,info:&crate::probe::ProbeInfo,donor:i64)->Applied {
    let mut kept=info.clone();kept.media_from=Some(donor);
    Applied {release_id:id,set_done:BIT,writes:vec![(BIT,StepWrite::for_mutation(id,StepMutation::KeepProbe {
        crc32:None,agent:None,json:serde_json::to_string(&kept).unwrap(),moviehash:kept.moviehash.clone(),
    }))],..Default::default()}
}
#[test]
fn native_same_mux_reads_observed_metadata_and_commits_once_without_an_asset_grant() {
    let Some(schema)=crate::pg::testing::Schema::new() else{return};let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    let (own,donor,info)=fixture(&runtime);let reads=StepReads::for_owner(&runtime.owner).unwrap();let data=StepData::Native(&reads);
    let got=data.same_mux_media(own,&info,STAMP).unwrap().expect("current checked metadata");
    assert_eq!(got.release_id,donor);assert_eq!(got.media,info.media.clone().unwrap());assert!(got.cover.is_none());
    let queue=Queue::for_owner(&runtime.owner).unwrap();let claims=queue.claim(10,2,60).unwrap();let claim=claims.iter().find(|c|c.release_id()==own).unwrap();
    assert_eq!(queue.settle(claim,Visit::prepare_observed(claim,result(own,&info,donor),[1;32],reads.observed().unwrap()).unwrap(),11).unwrap(),Settlement::Applied);
    assert_eq!(queue.settle(claim,Visit::prepare_observed(claim,result(own,&info,donor),[1;32],reads.observed().unwrap()).unwrap(),12).unwrap(),Settlement::Replayed);
    runtime.owner.with(|client| {
        let json:String=client.query_one("select info from release_probe where release_id=$1",&[&own])?.try_get(0)?;
        assert_eq!(serde_json::from_str::<crate::probe::ProbeInfo>(&json)?.media_from,Some(donor));
        assert_eq!(client.query_one("select count(*) from native_operations",&[])?.get::<_,i64>(0),1);Ok(())
    }).unwrap();runtime.owner.close();
    assert!(matches!(data.same_mux_media(own,&info,STAMP),Err(StepError::Native(_))),"ended owner is not a metadata miss");
}
#[test]
fn native_same_mux_joined_cover_and_a_real_changed_donor_refuse_stale_settlement() {
    let Some(schema)=crate::pg::testing::Schema::new() else{return};let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    let (own,donor,info)=fixture(&runtime);let reads=StepReads::for_owner(&runtime.owner).unwrap();
    assert_eq!(reads.same_mux_media(own,&info,STAMP).unwrap().unwrap().release_id,donor);
    let queue=Queue::for_owner(&runtime.owner).unwrap();let claims=queue.claim(10,2,60).unwrap();
    let source=claims.iter().find(|c|c.release_id()==own).unwrap();let other=claims.iter().find(|c|c.release_id()==donor).unwrap();
    let cover=Applied {release_id:donor,set_done:BIT,writes:vec![(BIT,StepWrite::for_mutation(donor,StepMutation::KeepCover {mime:"image/jpeg".into(),sha256:"a".repeat(64)}))],..Default::default()};
    assert_eq!(queue.settle(other,Visit::prepare(other,cover,[2;32]).unwrap(),11).unwrap(),Settlement::Applied);
    assert_eq!(queue.settle(source,Visit::prepare_observed(source,result(own,&info,donor),[1;32],reads.observed().unwrap()).unwrap(),12).unwrap(),Settlement::Superseded);
    // JSON still has no cover. The actual joined retained row alone must refuse.
    assert!(StepReads::for_owner(&runtime.owner).unwrap().same_mux_media(own,&info,STAMP).unwrap().is_none());
    runtime.owner.with(|client| {
        assert!(!client.query_one("select exists(select 1 from release_probe where release_id=$1)",&[&own])?.get::<_,bool>(0));
        assert_eq!(client.query_one("select count(*) from native_operations",&[])?.get::<_,i64>(0),1);Ok(())
    }).unwrap();assert_eq!(runtime.owner.failure(),None);runtime.owner.close();
}
#[test]
fn native_same_mux_malformed_and_expanded_probe_rows_propagate_errors_without_reuse() {
    for expanded in [false,true] {
        let Some(schema)=crate::pg::testing::Schema::new() else{return};let runtime=crate::pg::stage_queue_tests::runtime(&schema);
        let (own,donor,info)=fixture(&runtime);
        let json=if expanded {let mut value=info.clone();value.media.as_mut().unwrap().video[0].language=Some("x".repeat(600000));serde_json::to_string(&value).unwrap()} else {"{".into()};
        runtime.owner.with(|client|{client.execute("update release_probe set info=$2 where release_id=$1",&[&donor,&json])?;Ok(())}).unwrap();
        let reads=StepReads::for_owner(&runtime.owner).unwrap();
        assert!(matches!(StepData::Native(&reads).same_mux_media(own,&info,STAMP),Err(StepError::Native(_))),"decode/admission errors cannot become a candidate miss");
        runtime.owner.close();
    }
}
#[test]
fn native_same_mux_observation_cannot_cross_a_real_owner_replacement() {
    let Some(schema)=crate::pg::testing::Schema::new() else{return};let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    let (own,donor,info)=fixture(&runtime);let reads=StepReads::for_owner(&runtime.owner).unwrap();
    assert!(reads.same_mux_media(own,&info,STAMP).unwrap().is_some());let observed=reads.observed().unwrap();runtime.owner.close();
    let replacement=crate::pg::stage_queue_tests::runtime(&schema);let queue=Queue::for_owner(&replacement.owner).unwrap();let claims=queue.claim(100,2,60).unwrap();
    let claim=claims.iter().find(|c|c.release_id()==own).unwrap();
    assert!(Visit::prepare_observed(claim,result(own,&info,donor),[1;32],observed).is_err(),"old observations cannot mint replacement capabilities");
    replacement.owner.with(|client| {assert_eq!(client.query_one("select count(*) from native_operations",&[])?.get::<_,i64>(0),0);Ok(())}).unwrap();replacement.owner.close();
}
