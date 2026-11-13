//! Real original-owner queue effects replace legacy in-memory SQL execution.
use super::*;
use crate::pg::{self,indexer_runtime::Runtime,stage_queue::{Queue,Visit,Settlement}};
use crate::postprocess::{apply::Applied,step::StepWrite};
fn visit(runtime:&Runtime,id:i64,now:i64,mutations:Vec<StepMutation>)->anyhow::Result<()> {
    runtime.owner.with(|client| {let mut tx=client.transaction()?;assert!(pg::stage_queue::rerun_in(&mut tx,id,4,now)?);tx.commit()?;Ok(())})?;
    let queue=Queue::for_owner(&runtime.owner)?;let claim=queue.claim(now,1,60)?.pop().expect("genuine rearmed claim");assert_eq!(claim.release_id(),id);
    let applied=Applied {release_id:id,set_done:4,writes:mutations.into_iter().map(|mutation|(4,StepWrite::for_mutation(id,mutation))).collect(),..Default::default()};
    assert_eq!(queue.settle(&claim,Visit::prepare(&claim,applied,[9;32])?,now+1)?,Settlement::Applied);Ok(())
}
fn id(runtime:&Runtime)->i64 {runtime.owner.with(|client|Ok(client.query_one("select id from releases",&[])?.get(0))).unwrap()}
#[test]
#[ignore="requires isolated PostgreSQL; genuine claims exercise native pin/probe/NFO/media/word effects"]
fn native_metadata_effects_keep_pins_probe_fields_first_nfo_and_real_queue_counts() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");let runtime=pg::stage_queue_tests::runtime(&schema);pg::stage_queue_tests::push(&runtime);let id=id(&runtime);
    runtime.owner.with(|client| {client.execute("insert into media_release(release_id,kind,title_key,quality) values($1,0,77,0)",&[&id])?;Ok(())}).unwrap();
    visit(&runtime,id,10,vec![
        StepMutation::SetCategory {category:2045},StepMutation::PinOpenSubtitlesMovieImdb {imdb_id:101},
        StepMutation::KeepProbe {crc32:Some(17),agent:Some("retained".into()),json:"old".into(),moviehash:Some("retained-hash".into())},
        StepMutation::SetProbeInfo {json:"{\"quoted\":\"'\"}".into()},
        StepMutation::KeepNfo {file:"first.nfo".into(),text:"é".repeat(262145)},StepMutation::KeepNfo {file:"later.nfo".into(),text:"replacement".into()},
        StepMutation::SetNfoPassword {password:String::new()},
    ]).unwrap();
    visit(&runtime,id,20,vec![StepMutation::PinNfoMovieImdb {imdb_id:999},StepMutation::ClearCategory,
        StepMutation::FoundMediaIds {title_key:78,ids:crate::media::resolve::Ids {tmdb:Some(888),..Default::default()}}]).unwrap();
    let initial=runtime.owner.with(|client|Ok(client.query_one("select n from word_queue where release_id=$1",&[&id])?.get::<_,i64>(0))).unwrap();
    visit(&runtime,id,30,vec![StepMutation::ReindexWords,StepMutation::ReindexWords]).unwrap();
    runtime.owner.with(|client| {
        let row=client.query_one("select imdb_id,imdb_from_nfo,tmdb_id,title_key from media_release where release_id=$1",&[&id])?;
        assert_eq!((row.get::<_,Option<i64>>(0),row.get::<_,i64>(1),row.get::<_,Option<i64>>(2),row.get::<_,i64>(3)),(Some(101),1,None,77));
        let row=client.query_one("select info,moviehash,crc32,agent from release_probe where release_id=$1",&[&id])?;
        assert_eq!((row.get::<_,String>(0),row.get::<_,String>(1),row.get::<_,i64>(2),row.get::<_,String>(3)),("{\"quoted\":\"'\"}".into(),"retained-hash".into(),17,"retained".into()));
        let row=client.query_one("select file,text from release_nfo where release_id=$1",&[&id])?;assert_eq!(row.get::<_,String>(0),"first.nfo");assert_eq!(row.get::<_,String>(1).chars().count(),262144);
        assert_eq!(client.query_one("select password from release_nfo_password where release_id=$1",&[&id])?.get::<_,String>(0),"");
        assert_eq!(client.query_one("select count(*) from release_category where release_id=$1",&[&id])?.get::<_,i64>(0),0);
        assert_eq!(client.query_one("select n from word_queue where release_id=$1",&[&id])?.get::<_,i64>(0),initial+3,"two explicit marks plus the committed work-revision dirty notification");
        assert_eq!(client.query_one("select count(*) from native_operations",&[])?.get::<_,i64>(0),3);Ok(())
    }).unwrap();runtime.owner.close();
}
#[test]
#[ignore="requires isolated PostgreSQL; real queue replaces positive/negative match without a miss fallback"]
fn native_matches_replace_negative_exactly_and_keep_quoted_text() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");let runtime=pg::stage_queue_tests::runtime(&schema);pg::stage_queue_tests::push(&runtime);let id=id(&runtime);
    visit(&runtime,id,10,vec![StepMutation::OpenSubtitlesMatch {moviehash:"hash'quoted".into(),feature:Some(Feature {imdb_id:Some(101),title:Some("A ' title".into()),..Default::default()}),checked_at:2}]).unwrap();
    runtime.owner.with(|client| {let row=client.query_one("select moviehash,imdb_id,title,checked_at from release_opensubtitles where release_id=$1",&[&id])?;
        assert_eq!((row.get::<_,String>(0),row.get::<_,Option<i64>>(1),row.get::<_,String>(2),row.get::<_,i64>(3)),("hash'quoted".into(),Some(101),"A ' title".into(),2));Ok(())}).unwrap();
    visit(&runtime,id,20,vec![StepMutation::OpenSubtitlesMatch {moviehash:"hash'quoted".into(),feature:None,checked_at:3}]).unwrap();
    runtime.owner.with(|client| {let row=client.query_one("select imdb_id,title,checked_at from release_opensubtitles where release_id=$1",&[&id])?;
        assert_eq!((row.get::<_,Option<i64>>(0),row.get::<_,Option<String>>(1),row.get::<_,i64>(2)),(None,None,3));Ok(())}).unwrap();runtime.owner.close();
}
#[test]
#[ignore="requires isolated PostgreSQL; genuine driver failure rolls back prior effects and receipt"]
fn native_mutation_driver_failure_rolls_back_effects_and_never_commits_a_receipt() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");let runtime=pg::stage_queue_tests::runtime(&schema);pg::stage_queue_tests::push(&runtime);let id=id(&runtime);
    runtime.owner.with(|client| {client.batch_execute("alter table release_opensubtitles rename to deliberately_unavailable_result_table")?;Ok(())}).unwrap();
    let error=visit(&runtime,id,10,vec![StepMutation::SetCategory {category:2045},StepMutation::OpenSubtitlesMatch {moviehash:"hash".into(),feature:None,checked_at:1}]).unwrap_err();
    assert!(error.chain().any(|cause|cause.downcast_ref::<postgres::Error>().is_some()),"original driver cause must survive");
    let mut observer=schema.pool.get().unwrap();assert_eq!(observer.query_one("select count(*) from release_category",&[]).unwrap().get::<_,i64>(0),0);
    assert_eq!(observer.query_one("select count(*) from native_operations",&[]).unwrap().get::<_,i64>(0),0);assert_eq!(runtime.owner.failure(),Some(pg::outage::Failure::Fatal));runtime.owner.close();
}
