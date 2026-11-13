//! Fixed native result-field vectors replace the retired SQLite executor oracles.
//! Real effects/receipts are exercised separately through the native queue.
use super::*;
fn fields(mutation:StepMutation,id:i64)->Vec<Value> {mutation.v1_fields(id).1}
fn i(value:i64)->Value {Value::Integer(value)}
fn t(value:&str)->Value {Value::Text(value.into())}
#[test]
fn match_and_pin_fields_keep_negative_nulls_quotes_and_exact_release() {
    assert_eq!(fields(StepMutation::OpenSubtitlesMatch {moviehash:"hash'quoted".into(),feature:None,checked_at:2},7),vec![i(7),t("hash'quoted"),Value::Null,Value::Null,Value::Null,Value::Null,Value::Null,Value::Null,Value::Null,Value::Null,i(2)]);
    let positive=fields(StepMutation::OpenSubtitlesMatch {moviehash:"hash'quoted".into(),feature:Some(Feature {imdb_id:Some(101),title:Some("A ' title".into()),..Default::default()}),checked_at:3},7);
    assert_eq!((&positive[0],&positive[2],&positive[5],&positive[10]),(&i(7),&i(101),&t("A ' title"),&i(3)));
    assert_eq!(fields(StepMutation::PinOpenSubtitlesMovieImdb {imdb_id:101},7),vec![i(7),i(101)]);
    assert_eq!(fields(StepMutation::SetCategory {category:2045},7),vec![i(7),i(2045)]);
    assert_eq!(fields(StepMutation::ClearCategory,8),vec![i(8)]);
    assert_eq!(fields(StepMutation::SetProbeInfo {json:"{'quoted'}".into()},7),vec![i(7),t("{'quoted'}")]);
}
#[test]
fn nfo_and_media_fields_preserve_character_cap_nulls_order_and_late_native_lookup() {
    let text="é".repeat(super::super::result_v1_tags::PROBE_NFO_STORE_MAX_CHARS+1);
    let values=fields(StepMutation::KeepNfo {file:"first.nfo".into(),text},7);
    assert_eq!((&values[0],&values[1]),(&i(7),&t("first.nfo")));
    let Value::Text(text)=&values[2] else {panic!("NFO must stay UTF8 text")};assert_eq!(text.chars().count(),262144);
    assert_eq!(fields(StepMutation::SetNfoPassword {password:String::new()},7),vec![i(7),t("")]);
    let ids=crate::media::resolve::Ids {tvdb:Some(1),tvmaze:Some(2),trakt:None,tmdb:Some(3),imdb:Some(4),tvrage:None};
    assert_eq!(fields(StepMutation::FoundMediaIds {title_key:77,ids},7),vec![i(7),i(77),i(1),i(2),Value::Null,i(3),i(4),Value::Null]);
    let parsed=crate::media::parse::Parsed {year:Some(2020),season:Some(2),episode:Some(3),episode_last:Some(4),air_date:Some(20000101),quality:1080,..Default::default()};
    let values=fields(StepMutation::ParseMedia {kind:crate::media::title::Kind::Tv,title_key:77,parsed},7);
    assert_eq!(&values[..9],&[i(7),i(1),i(77),i(2),i(3),i(4),i(20000101),i(2020),i(1080)]);assert_eq!(&values[9..],&vec![Value::Null;6]);
}
#[test]
fn crc_predb_key_and_word_fields_keep_unsigned_and_signed_bits_and_nulls() {
    assert_eq!(fields(StepMutation::RecordSegmentCrc {message_id:"<n\0@example>".into(),decoded:17,crc32:u32::MAX},7),vec![i(7),t("<n\0@example>"),i(17),i(4294967295)]);
    assert_eq!(fields(StepMutation::ReindexWords,7),vec![i(7)]);
    assert_eq!(fields(StepMutation::KeepPredbMatch {title_id:9,kind:crate::predb::matcher::Kind::InnerFile,category:None,pre_at:None,nuked:1,matched_at:100},7),vec![i(7),i(9),i(4),Value::Null,Value::Null,i(1),i(100)]);
    assert_eq!(fields(StepMutation::AddPredbInnerKey {key:-7},8),vec![i(-7),i(8)]);
    assert_eq!(fields(StepMutation::ClearPredbInnerKeys,7),vec![i(7)]);
    let keys=crate::keys::Keys {account:Some(i64::MIN),host:Some(-1),file_crc:Some(i64::MAX),..Default::default()};let values=fields(StepMutation::KeepReleaseKeys {keys},7);
    assert_eq!((&values[0],&values[1],&values[2],&values[5],&values[6]),(&i(7),&i(i64::MIN),&i(-1),&i(i64::MAX),&Value::Null));
    assert_eq!(fields(StepMutation::ClearReleaseKeys,8),vec![i(8)]);
}
#[test]
fn still_audio_probe_and_cover_fields_preserve_optional_time_and_exact_blob() {
    let still=crate::raw_store::SavedStill {position:25,at_ms:None,sha256:"00".repeat(32),width:8,height:9,phash:Some(i64::MIN)};
    let values=fields(StepMutation::KeepStill(still),7);assert_eq!((&values[0],&values[1],&values[2],&values[6]),(&i(7),&i(25),&Value::Null,&i(i64::MIN)));
    let values=fields(StepMutation::KeepAudioPrint {position:50,at_ms:-1,items:vec![0,u32::MAX]},7);assert_eq!(&values[..3],&[i(7),i(50),i(-1)]);
    let Value::Blob(bytes)=&values[3] else {panic!("audio print must stay binary")};assert_eq!(bytes,&[0,0,0,0,255,255,255,255]);
    assert_eq!(fields(StepMutation::KeepAudioAnchor {anchor:i64::MIN},7),vec![i(7),i(i64::MIN)]);
    assert_eq!(fields(StepMutation::KeepProbe {crc32:None,agent:None,json:"{}".into(),moviehash:None},7),vec![i(7),Value::Null,Value::Null,t("{}"),Value::Null]);
    assert_eq!(fields(StepMutation::KeepCover {mime:"image/png".into(),sha256:"01".into()},7),vec![i(7),t("image/png"),t("01")]);
}
#[test]
fn image_archive_rev_and_par2_fields_keep_bounds_without_minting_payload_authority() {
    let image=ImageRecord {file:"a.gif".into(),sha256:"01".into(),format:"gif".into(),width:1,height:2,bytes:3,meta:None,exif_id:Some(String::new()),document_id:None};
    let values=fields(StepMutation::KeepImage {position:0,image},7);assert_eq!(values,vec![i(7),i(0),t("a.gif"),t("01"),t("gif"),i(1),i(2),i(3),Value::Null,t(""),Value::Null]);
    let values=fields(StepMutation::KeepArchiveFile {sequence:131072,source:2,file:crate::archive::InnerFile {name:"é".into(),size:Some(u64::MAX),crc32:Some(u32::MAX),mtime:Some(42),host_os:Some(3)}},7);
    assert_eq!(values,vec![i(7),i(131072),t("é"),Value::Null,i(2),i(4294967295),i(42),i(3)]);
    assert_eq!(fields(StepMutation::KeepRevVolume(crate::archive::rev::Volume {index:u16::MAX,size:u64::MAX,crc32:u32::MAX}),7),vec![i(7),i(65535),i(i64::MAX),i(4294967295)]);
    let file=crate::par2::Par2File {set_id:[1;16],file_id:[2;16],md5:[3;16],md5_16k:[4;16],size:u64::MAX,name:"quote'file".into(),bound:false,crc32:None};
    let values=fields(StepMutation::KeepPar2File {file,sealed:false,complete:false},7);assert_eq!((&values[0],&values[5],&values[6],&values[7]),(&i(7),&i(i64::MAX),&t("quote'file"),&Value::Null));assert_eq!(&values[8..],&[i(0),i(0),i(0)]);
    assert_eq!(fields(StepMutation::SetPostedFingerprint {posted:vec![0,255]},7),vec![i(7),Value::Blob(vec![0,255])]);assert_eq!(fields(StepMutation::ClearPostedFingerprint,7),vec![i(7)]);
}
#[test]
fn cross_release_and_pointed_fields_are_data_and_require_observed_resolution() {
    use crate::postprocess::step::{StepWrite,StepError};
    let mutations=[StepMutation::KeepNfoForLegacyTarget {target_id:8,file:"a.nfo".into(),text:"retained".into()},StepMutation::MarkPointed {name:"Example.Release".into()},StepMutation::LinkUploadData {member_id:8,lead_id:9,par2_file:[1;16]}];
    for mutation in mutations {let write=StepWrite::for_mutation(7,mutation);assert!(matches!(write.native_mutation_for(7),Err(StepError::Retry(_))));assert!(write.native_mutation_for(8).is_err());}
    assert_eq!(fields(StepMutation::MarkPointed {name:"Example.Release".into()},7),vec![t("Example.Release"),i(7)]);
    assert_eq!(fields(StepMutation::SetPointedPassword {name:"Example.Release".into(),password:String::new()},7),vec![t("Example.Release"),i(7),t("")]);
    assert_eq!(fields(StepMutation::RememberUploadCandidate {candidate_id:8},7),vec![i(7),i(8)]);
}
