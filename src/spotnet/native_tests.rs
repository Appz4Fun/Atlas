//! Source controls for the actual native producer. No case runs without Root admission.
use super::*;
use crate::archive::nzb::{NzbFile,Parsed,Segment};

fn spot() -> full::FullSpot {
    full::FullSpot {title:"Native.Spot.Example".into(),poster:"Spot Poster".into(),created:1_700_000_000,
        category:0,..Default::default()}
}
fn parsed() -> Parsed {
    Parsed {complete:true,files:vec![
        NzbFile {name:"part01.rar".into(),subject:r#"[01/02] "part01.rar" yEnc (1/3)"#.into(),
            poster:"Actual NZB Poster".into(),date:1_700_000_001,groups:vec!["example.spot".into()],articles:3,total_bytes:6,
            segments:vec![Segment {number:3,bytes:3,message_id:"third@example.test".into()},
                Segment {number:1,bytes:1,message_id:"first@example.test".into()},
                Segment {number:2,bytes:2,message_id:"second@example.test".into()}]},
        NzbFile {name:"part02.rar".into(),subject:r#"[02/02] "part02.rar" yEnc (1/1)"#.into(),
            groups:vec!["example.spot".into()],articles:1,total_bytes:4,
            segments:vec![Segment {number:1,bytes:4,message_id:"fourth@example.test".into()}],..Default::default()}],
        ..Default::default()}
}

#[test]
fn native_spot_batches_match_unchanged_legacy_article_metadata_and_order() {
    let spot=spot();let parsed=parsed();let expected=release::release_of(&spot,&parsed).unwrap();
    let mut plan=native_plan::Plan::new(&spot,&parsed).unwrap().unwrap();let mut articles=Vec::new();
    while let Some(batch)=plan.next_batch(4096).unwrap() {
        assert_eq!((&batch.name,&batch.group,&batch.poster,&batch.date,batch.size),
            (&expected.name,&expected.group,&expected.poster,&expected.date,expected.size));
        assert_eq!((batch.is_obfuscated,batch.display_name,batch.collection),(false,None,None));
        articles.extend(batch.articles);
    }
    assert_eq!(articles,expected.articles,"batching must preserve genuine NZB fields, including missing poster/date fallbacks");
}

#[test]
fn native_spot_valid_larger_than_one_writer_job_is_streamed_without_a_full_expanded_release() {
    let mut spot=spot();spot.title=format!("Large.{}","n".repeat(300));
    let count=20_000usize;let parsed=Parsed {complete:true,files:vec![NzbFile {
        name:"large.bin".into(),subject:r#""large.bin" yEnc (1/20000)"#.into(),poster:"Actual Poster".into(),
        groups:vec!["example.spot".into()],articles:count,total_bytes:count as u64,
        segments:(1..=count).map(|n|Segment {number:n as i64,bytes:1,message_id:format!("part-{n}@example.test")}).collect(),
        ..Default::default()}],..Default::default()};
    let mut plan=native_plan::Plan::new(&spot,&parsed).unwrap().unwrap();let(mut seen,mut batches)=(0,0);
    while let Some(batch)=plan.next_batch(8*1024*1024).unwrap() {
        assert!(!batch.articles.is_empty());assert!(batch.articles.len()*std::mem::size_of::<crate::parser::Article>()<8*1024*1024);
        seen+=batch.articles.len();batches+=1;drop(batch); // actual caller retains one shared admission through commit
    }
    assert_eq!(seen,count);assert!(batches>1,"an8MiB job must not be misused as a total valid-Spot size limit");
}

#[test]
fn native_spot_plan_refuses_incomplete_missing_or_damaged_inputs_before_publication() {
    let spot=spot();
    for edit in [0,1,2,3] {
        let mut parsed=parsed();match edit {0=>parsed.complete=false,1=>parsed.damaged=true,
            2=>parsed.left_out=1,_=>{parsed.files[0].segments.pop();}};
        assert!(native_plan::Plan::new(&spot,&parsed).unwrap().is_none());
    }
    let parsed=parsed();let mut plan=native_plan::Plan::new(&spot,&parsed).unwrap().unwrap();
    assert!(plan.next_batch(1).is_err(),"the next expansion cannot exceed its actual admitted job capacity");
}

#[test]
#[ignore="requires explicitly admitted isolated PostgreSQL; no skipped success"]
fn native_spot_bookkeeping_rollback_stops_original_owner_without_committing_cursor() {
    let schema=crate::pg::testing::Schema::new().expect("explicit ATLAS_PG_TEST_URL is required");
    let runtime=crate::pg::stage_queue_tests::runtime(&schema);let store=native_store::Store::new(&runtime.owner).unwrap();
    let error=store.with::<()>(|tx| {
        let db=crate::pg::sql::Pg::new(tx);
        store::note_read(&db,"actual-account-binding",41,&[("spot@spot.net".into(),41,Some("abc".into()))],100)?;
        anyhow::bail!("controlled publication failure")
    }).unwrap_err();
    assert!(error.to_string().contains("controlled publication failure"));
    let mut observer=schema.pool.get().unwrap();
    assert_eq!(observer.query_one("select count(*) from spots_seen",&[]).unwrap().get::<_,i64>(0),0);
    assert_eq!(observer.query_one("select count(*) from meta where key='spots_cursor:actual-account-binding'",&[]).unwrap().get::<_,i64>(0),0);
    assert_eq!(runtime.owner.failure(),Some(crate::pg::outage::Failure::Fatal));
    assert!(store.check().is_err());runtime.owner.close();
}

#[test]
#[ignore="requires explicitly admitted isolated PostgreSQL; no skipped success"]
fn native_spot_stopped_owner_cannot_note_headers_or_settle_comments() {
    let schema=crate::pg::testing::Schema::new().expect("explicit ATLAS_PG_TEST_URL is required");
    let runtime=crate::pg::stage_queue_tests::runtime(&schema);let store=native_store::Store::new(&runtime.owner).unwrap();
    runtime.owner.stop.store(true,std::sync::atomic::Ordering::Release);
    assert!(store.with(|tx| {tx.execute("insert into spot_comments(message_id,spot,at) values('c@spot.net','s@spot.net',1)",&[])?;Ok(())}).is_err());
    let mut observer=schema.pool.get().unwrap();
    assert_eq!(observer.query_one("select count(*) from spot_comments",&[]).unwrap().get::<_,i64>(0),0);
    assert_eq!(runtime.owner.failure(),None);runtime.owner.close();
}

#[test]
#[ignore="requires explicitly admitted isolated PostgreSQL and the original Native admission"]
fn native_spot_publication_preserves_newest_tag_and_never_mixes_same_title_posts() {
    let schema=crate::pg::testing::Schema::new().expect("explicit ATLAS_PG_TEST_URL is required");
    let runtime=crate::pg::stage_queue_tests::runtime(&schema);let storage=native_store::Store::new(&runtime.owner).unwrap();
    let handle=tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let permit=handle.block_on(runtime.db.reserve_native_for_owner(&runtime.owner)).unwrap();
    let parsed=parsed();let mut spot=spot();
    let row=|id:&str,spot:&full::FullSpot,key:&str|store::SpotRow {message_id:id.into(),kind:Kind::Movie,spot:spot.clone(),spotter_id:None,nzb_key:key.into(),xml:format!("<Posting><Title>{}</Title></Posting>",spot.title)};
    storage.with(|tx|Ok(store::note_read(&crate::pg::sql::Pg::new(tx),"fixture-account",3,&[
        ("first@spot.net".into(),1,None),("older@spot.net".into(),2,None),("different@spot.net".into(),3,None)],100)?)).unwrap();
    let mut first=native_plan::Plan::new(&spot,&parsed).unwrap().unwrap();
    let first_key=first.key().to_owned();
    let id=storage.publish(&mut first,&row("first@spot.net",&spot,&first_key),"first-ref",&permit,100).unwrap().release_id.unwrap();
    drop(first);
    spot.created-=1;spot.poster="Older spot must not replace newer".into();
    storage.copy_of(id,&row("older@spot.net",&spot,&first_key),"older-ref",101).unwrap();
    let mut other=parsed.clone();other.files[0].segments[0].message_id="different@fixture.test".into();
    let mut different=native_plan::Plan::new(&spot,&other).unwrap().unwrap();let key=different.key().to_owned();
    let second=storage.publish(&mut different,&row("different@spot.net",&spot,&key),"different-ref",&permit,102).unwrap().release_id.unwrap();
    assert_ne!(id,second);
    let mut observer=schema.pool.get().unwrap();
    assert_eq!(observer.query_one("select message_id from spots where release_id=$1",&[&id]).unwrap().get::<_,String>(0),"first@spot.net");
    assert_eq!(observer.query_one("select count(*) from segments where release_id=$1",&[&id]).unwrap().get::<_,i64>(0),4);
    assert_eq!(observer.query_one("select count(*) from segments where release_id=$1",&[&second]).unwrap().get::<_,i64>(0),4);
    assert!(observer.query_one("select name from releases where id=$1",&[&second]).unwrap().get::<_,String>(0).ends_with(&format!(" [{}]",&key[..8])));
    assert_eq!(observer.query_one("select count(*) from spots_seen where state=$1",&[&store::SAVED]).unwrap().get::<_,i64>(0),3);
    runtime.owner.close();
}

#[test]
#[ignore="requires explicitly admitted isolated PostgreSQL, original shared writer memory/slot controls"]
fn native_spot_foreign_db_owner_is_refused_before_any_provider_work() {
    let (_left,a,_left_dir)=super::native_peer_tests::fixture();
    let (_right,b,_right_dir)=super::native_peer_tests::fixture();
    let handle=tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let config=crate::config::SpotsConfig {blacklist_url:String::new(),..Default::default()};
    // Empty pool makes any accidental provider use invalid as well; the
    // explicit original-Db identity check must reject construction first.
    assert!(native::Service::new(&a.owner,b.db.clone(),config,std::sync::Arc::new(crate::nntp::Pool::new(&[])),handle.handle().clone()).is_err());
    assert!(handle.block_on(b.db.reserve_native_for_owner(&a.owner)).is_err());
    a.owner.close();b.owner.close();
}

#[test]
#[ignore="explicit isolated PG; actual multi-job Spot publication under original Native permit"]
fn native_spot_larger_than_one_job_commits_all_actual_parts_in_one_publication() {
    let (schema,runtime,_dir)=super::native_peer_tests::fixture();
    let storage=native_store::Store::new(&runtime.owner).unwrap();
    let handle=tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let permit=handle.block_on(runtime.db.reserve_native_for_owner(&runtime.owner)).unwrap();
    let mut spot=spot();spot.title=format!("Large.{}","n".repeat(300));let count=20_000usize;
    let parsed=Parsed {complete:true,files:vec![NzbFile {name:"large.bin".into(),subject:r#""large.bin" yEnc (1/20000)"#.into(),
        poster:"Actual Poster".into(),groups:vec!["example.spot".into()],articles:count,total_bytes:count as u64,
        segments:(1..=count).map(|n|Segment {number:n as i64,bytes:1,message_id:format!("part-{n}@example.test")}).collect(),..Default::default()}],..Default::default()};
    storage.with(|tx|Ok(store::note_read(&crate::pg::sql::Pg::new(tx),"actual-fixture-binding",1,&[("large@spot.net".into(),1,None)],100)?)).unwrap();
    let mut plan=native_plan::Plan::new(&spot,&parsed).unwrap().unwrap();let key=plan.key().to_owned();
    let row=store::SpotRow {message_id:"large@spot.net".into(),kind:Kind::Movie,spot:spot.clone(),spotter_id:None,nzb_key:key,xml:"<Posting><Title>Large</Title></Posting>".into()};
    let saved=storage.publish(&mut plan,&row,"actual-large-ref",&permit,100).unwrap();assert_eq!(saved.state,store::SAVED);
    let mut observer=schema.pool.get().unwrap();
    assert_eq!(observer.query_one("select count(*) from segments where release_id=$1",&[&saved.release_id.unwrap()]).unwrap().get::<_,i64>(0),count as i64);
    assert_eq!(observer.query_one("select count(*) from releases",&[]).unwrap().get::<_,i64>(0),1);
    assert_eq!(observer.query_one("select state from spots_seen where message_id='large@spot.net'",&[]).unwrap().get::<_,i64>(0),store::SAVED);
    runtime.owner.close();
}
