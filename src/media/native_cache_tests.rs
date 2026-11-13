//! Source qualification uses actual PG rows. The ignored child additionally
//! uses the canonical Runtime/owner fixture; Root must supply its owned home,
//! namespace and independent failed-open/existing-FD/OS network observer.

use super::*;
use crate::pg::service_cache::{title_in,put_title_in,http_in,put_http_in};

#[test]
fn native_media_rows_preserve_positive_negative_masks_times_and_nullable_ids() {
    let Some(schema)=crate::pg::testing::Schema::new() else {return};
    let mut client=schema.pool.get().unwrap();
    let row=TitleRow {key:7,kind:2,title:"Some Movie".into(),year:Some(2001),state:2,checked_mask:15,checked_at:17,next_check:Some(31),misses:4,fanout_done:false,..Default::default()};
    put_title_in(&mut *client,&row).unwrap();assert_eq!(title_in(&mut *client,7).unwrap(),Some(row.clone()));
    let mut positive=row;positive.state=1;positive.ids.imdb=Some(133093);positive.next_check=None;positive.found_title=Some("Canonical Name".into());
    put_title_in(&mut *client,&positive).unwrap();assert_eq!(title_in(&mut *client,7).unwrap(),Some(positive));
    assert!(title_in(&mut *client,9).unwrap().is_none());
}

#[test]
fn native_media_late_ids_validate_full_kind_title_year_and_read_current_transaction() {
    let Some(schema)=crate::pg::testing::Schema::new() else {return};let mut client=schema.pool.get().unwrap();
    let parsed=parse::Parsed {title:"Some Movie".into(),year:Some(2001),..Default::default()};let key=steps::key_of(Kind::Movie,&parsed);
    let row=TitleRow {key,kind:2,title:parsed.title.clone(),year:parsed.year,state:1,ids:Ids {imdb:Some(11),..Default::default()},..Default::default()};
    put_title_in(&mut *client,&row).unwrap();let mut tx=client.transaction().unwrap();
    tx.execute("update ext_media_titles set imdb_id=22 where title_key=$1",&[&key]).unwrap();
    assert_eq!(native_cached_ids_in(&mut tx,Kind::Movie,key,&parsed).unwrap().imdb,Some(22));
    tx.execute("update ext_media_titles set title='Other Movie' where title_key=$1",&[&key]).unwrap();
    assert!(native_cached_ids_in(&mut tx,Kind::Movie,key,&parsed).is_err());tx.rollback().unwrap();
    assert!(native_cached_ids_in(&mut *client,Kind::Tv,key,&parsed).is_err());
    let mut changed=parsed;changed.year=Some(2002);assert!(native_cached_ids_in(&mut *client,Kind::Movie,key,&changed).is_err());
}

#[test]
fn native_http_cache_keeps_status_payload_canonical_key_and_typed_database_errors() {
    let Some(schema)=crate::pg::testing::Schema::new() else {return};let mut client=schema.pool.get().unwrap();
    let url="https://example.invalid/search?api_key=fixture-only&q=Some";
    put_http_in(&mut *client,"fixture",url,404,"not found").unwrap();
    assert_eq!(http_in(&mut *client,"fixture","https://example.invalid/search?q=Some").unwrap(),Some((404,"not found".into())));
    let saved:String=client.query_one("select url from ext_http_cache",&[]).unwrap().get(0);assert!(!saved.contains("api_key"));
    put_http_in(&mut *client,"fixture",url,200,"{\"ok\":true}").unwrap();
    assert_eq!(http_in(&mut *client,"fixture",url).unwrap(),Some((200,"{\"ok\":true}".into())));
    client.batch_execute("drop table ext_http_cache").unwrap();
    assert!(http_in(&mut *client,"fixture",url).unwrap_err().downcast_ref::<postgres::Error>().is_some());
}

#[test]
fn native_service_factory_refuses_an_unvalidated_owner() {
    let Some(schema)=crate::pg::testing::Schema::new() else {return};
    let owner=Arc::new(crate::pg::indexer_runtime::Owner::new(postgres::Client::connect(&schema.url,postgres::NoTls).unwrap()));
    assert!(native_steps(&owner,&Config::default()).is_err());
}

#[test]
#[ignore="Root-only configured child; requires owned native PG home and independent SQLite/network observer"]
fn configured_native_media_child() {
    let action=std::env::var("ATLAS_NATIVE_MEDIA_CASE").expect("owned media child action");
    let runtime=crate::raw_store::native_pg_opensubtitles_cache_tests::open_test_native_owner().expect("canonical native owner");
    let mut config=Config::default();config.media.parse=true;config.media.nfo=true;config.media.tvmaze.enabled=true;
    config.media.tvmaze.base_url=Some(std::env::var("ATLAS_NATIVE_MEDIA_HTTP").expect("owned loopback observer"));
    assert!(config.media.tvmaze.base_url.as_ref().unwrap().starts_with("http://127.0.0.1:"));
    config.media.allocine.enabled=false;config.media.details_per_minute=0;config.postprocess.body_fetches_per_minute=1;
    let configured=native_steps(&runtime.owner,&config).unwrap();assert_eq!(configured.map(|step|step.name()),["media_parse","media_lookup","media_nfo"]);
    let media=native_service(&runtime.owner,&config).unwrap();assert!(media.is_native());
    let key=title::title_key(Kind::Tv,"some show",None);let now=http::now_secs();
    match action.as_str() {
        "negative"=>{
            media.title_put_checked(&TitleRow {key,kind:1,title:"Some Show".into(),state:2,checked_mask:255,checked_at:now,next_check:Some(now+3600),..Default::default()}).unwrap();
            let query=resolve::TitleQuery {kind:Kind::Tv,title:"Some Show",year:None,country:None};
            assert_eq!(media.resolve_seeded_checked(&query,key,now,&||false,None).unwrap(),resolve::Resolved::NotFound);
        },
        "unavailable"=>{
            runtime.owner.with(|client|{client.batch_execute("drop table ext_media_titles")?;Ok(())}).unwrap();
            let query=resolve::TitleQuery {kind:Kind::Tv,title:"Some Show",year:None,country:None};
            let error=match media.resolve_seeded_checked(&query,key,now,&||false,None) {Err(resolve::ResolveFailure::Cache(error))=>error,other=>panic!("expected original cache failure: {other:?}")};
            assert!(error.chain().any(|cause|cause.downcast_ref::<postgres::Error>().is_some()));
        },
        _=>panic!("unknown owned media child action"),
    }
    println!("native media child completed: {action}; Root OS observer remains required");
}
