//! One fixed native-original PG fixture, test-only and never self-activating.
use super::{full,store,comments};
pub(crate) const XML:&str="<Spotnet><Posting><Key>1</Key><Category>0<SubCat>01z01</SubCat></Category><Title>Real.Spot\0Original</Title><Poster>Original poster</Poster><Description>Keep password alpha\0beta unchanged</Description><NZB><Segment>real@spot.net</Segment></NZB></Posting></Spotnet>";

pub(crate) fn fixture()->(crate::pg::testing::Schema,crate::pg::indexer_runtime::Runtime,tempfile::TempDir,i64) {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated ATLAS_PG_TEST_URL required");
    let dir=tempfile::tempdir().unwrap();let mut config=crate::config::Config::default();
    config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;
    config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;
    let runtime=crate::pg::indexer_runtime::Runtime::open(&config,crate::pg::StorageConfig {backend:crate::pg::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:1,allow_plaintext:true},crate::config::HeaderStoreConfig {enabled:true,dir:dir.path().to_str().unwrap().into()},&[],&mut None).unwrap();
    let spot=full::parse(XML).unwrap();let parsed=crate::archive::nzb::Parsed {complete:true,files:vec![crate::archive::nzb::NzbFile {
        name:"real.mkv".into(),subject:r#""real.mkv" yEnc (1/1)"#.into(),groups:vec!["example.spot".into()],articles:1,total_bytes:1,
        segments:vec![crate::archive::nzb::Segment {number:1,bytes:1,message_id:"real-part@spot.net".into()}],..Default::default()}],..Default::default()};
    let storage=crate::spotnet::native_store::Store::new(&runtime.owner).unwrap();
    storage.with(|tx|Ok(store::note_read(&crate::pg::sql::Pg::new(tx),"fixture",1,&[("real@spot.net".into(),1,None)],-100000)?)).unwrap();
    let handle=tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let permit=handle.block_on(runtime.db.reserve_native_for_owner(&runtime.owner)).unwrap();
    let mut plan=crate::spotnet::native_plan::Plan::new(&spot,&parsed).unwrap().unwrap();let key=plan.key().to_owned();
    let row=store::SpotRow {message_id:"real@spot.net".into(),kind:crate::spotnet::Kind::Series,spot:spot.clone(),spotter_id:Some("actual".into()),nzb_key:key,xml:XML.into()};
    let id=storage.publish(&mut plan,&row,"real-ref",&permit,-100000).unwrap().release_id.unwrap();
    storage.with(|tx|Ok(comments::note_read(&crate::pg::sql::Pg::new(tx),comments::COMMENT_GROUP,"fixture",1,&[comments::Noted {
        message_id:"real.8.one.two@spot.net".into(),spot:"real@spot.net".into(),at:1,rating:8,poster:"Actual reader".into(),spotter_id:None}],false,1)?)).unwrap();
    // Legacy Latin-1 decoding can produce20KiB UTF8 from10KiB wire. This
    // exact original remains valid; a10KiB UTF8 cap would silently regress it.
    storage.settle_comments(&[("real.8.one.two@spot.net".into(),comments::Outcome::Read(comments::body_text(&vec![0xe9;comments::MAX_BODY])))]).unwrap();
    drop(permit);(schema,runtime,dir,id)
}

// This module is cfg(test) under spotnet, where the real Store stays private.
pub(crate) fn replace_comment_and_note_disposal(runtime:&crate::pg::indexer_runtime::Runtime) {
    let storage=crate::spotnet::native_store::Store::new(&runtime.owner).unwrap();
    storage.settle_comments(&[("real.8.one.two@spot.net".into(),comments::Outcome::Read("New supplied text".into()))]).unwrap();
    storage.with(|tx|Ok(store::note_disposals(&crate::pg::sql::Pg::new(tx),&[store::Disposal {message_id:"real@spot.net".into(),spotter:String::new(),at:2}])?)).unwrap();
}
