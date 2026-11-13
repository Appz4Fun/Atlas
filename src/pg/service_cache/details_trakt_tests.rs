//! Real native releases/media inputs and the original canonical owner.
use super::*;

fn seed(runtime:&crate::pg::indexer_runtime::Runtime,count:usize) {
    runtime.owner.with(|client| {
        let rows=(0..count).map(|n|crate::parser::Release {name:format!("Native.Identity.Movie.{n}"),group:"alt.binaries.native.identity".into(),articles:vec![crate::parser::Article {
            filename:Some("video.mkv".into()),subject:"video.mkv yEnc (1/1)".into(),message_id:format!("<native-details-identity-{n}@fixture.test>"),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}],..Default::default()}).collect::<Vec<_>>();
        assert_eq!(crate::pg::release_writer::save(client,&rows,1)?.releases,count as u64);
        client.execute("insert into media_release(release_id,kind,title_key,quality,tmdb_id) select id,1,id,0,1000000+id from releases",&[])?;Ok(())
    }).unwrap();
}

#[test]
#[ignore="requires explicit isolated PostgreSQL and actual native cross-page media identities"]
fn native_trakt_identity_seeks_past_the_source_page_without_treating_missing_as_conflicting() {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");let runtime=crate::pg::stage_queue_tests::runtime(&schema);seed(&runtime,67);
    runtime.owner.with(|client| {
        client.execute("update media_release set tmdb_id=42,imdb_id=1234567 where release_id in(select min(id) from releases union all select max(id) from releases)",&[])?;
        client.execute("update media_release set trakt_id=900 where release_id=(select min(id) from releases)",&[])?;
        let page=page_in(client,None,64)?;assert_eq!(page.rows.len(),64);
        let source=page.rows.iter().find(|row|row.ids.tmdb==Some(42)).unwrap();assert_eq!(source.ids.trakt,None);
        assert_eq!(trakt_identity_in(client,source.kind,&source.ids)?,TraktIdentity::Unique(900));
        assert_eq!(trakt_identity_in(client,1,&Ids {tmdb:Some(999),..Default::default()})?,TraktIdentity::Absent);
        let old=page_in(client,page.after,64)?;assert_eq!(old.rows.len(),3);assert_eq!(old.rows.last().unwrap().ids.trakt,Some(900));Ok(())
    }).unwrap();assert_eq!(runtime.owner.failure(),None);runtime.owner.close();
}

#[test]
#[ignore="requires explicit isolated PostgreSQL and conflicting native media identities"]
fn native_trakt_identity_uses_the_exact_nullable_tuple_and_refuses_ambiguous_endpoint_ids() {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");let runtime=crate::pg::stage_queue_tests::runtime(&schema);seed(&runtime,8);
    runtime.owner.with(|client| {
        client.execute("update media_release set tmdb_id=42,imdb_id=1234567,trakt_id=900",&[])?;
        let ids=Ids {tmdb:Some(42),imdb:Some(1234567),..Default::default()};
        assert_eq!(trakt_identity_in(client,1,&ids)?,TraktIdentity::Unique(900),"repeated same provider ID remains unique");
        // Populated optional identity fields cannot be borrowed into a NULL-key source.
        for field in ["tvdb_id","tvmaze_id","tvrage_id"] {
            client.batch_execute(&format!("update media_release set {field}=7,trakt_id=901 where release_id=(select min(id) from releases)"))?;
            assert_eq!(trakt_identity_in(client,1,&ids)?,TraktIdentity::Unique(900));
            client.batch_execute(&format!("update media_release set {field}=null,trakt_id=900"))?;
        }
        client.execute("update media_release set kind=2,trakt_id=901 where release_id=(select min(id) from releases)",&[])?;
        assert_eq!(trakt_identity_in(client,1,&ids)?,TraktIdentity::Unique(900));
        client.execute("update media_release set kind=1,trakt_id=901 where release_id=(select min(id) from releases)",&[])?;
        assert_eq!(trakt_identity_in(client,1,&ids)?,TraktIdentity::Ambiguous);
        assert_eq!(trakt_identity_in(client,1,&Ids {imdb:Some(7654321),..ids})?,TraktIdentity::Absent);Ok(())
    }).unwrap();runtime.owner.close();
}

#[test]
#[ignore="requires explicit isolated PostgreSQL; missing table must retain its actual PG error cause"]
fn native_trakt_identity_database_failure_is_never_absent_or_unique() {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    runtime.owner.with(|client| {client.batch_execute("alter table media_release rename to fixture_media_release_unavailable")?;Ok(())}).unwrap();
    let error=runtime.owner.with(|client|trakt_identity_in(client,1,&Ids {tmdb:Some(42),..Default::default()})).unwrap_err();
    assert!(error.chain().any(|cause|cause.downcast_ref::<postgres::Error>().is_some()));assert_eq!(runtime.owner.failure(),Some(crate::pg::outage::Failure::Fatal));runtime.owner.close();
}
