use super::*;

fn fixture() -> Option<super::super::testing::Schema> {
    let schema = super::super::testing::Schema::new()?;
    let mut client = schema.pool.get().unwrap();
    if !client.query_one("select to_regclass('ext_predb_title_keys') is not null", &[]).unwrap().get::<_,bool>(0) {
        client.batch_execute(include_str!("migrations/0010_predb_keys.fragment.sql")).unwrap();
    }
    drop(client);
    Some(schema)
}
fn lookup(client: &mut postgres::Client, evidence: &Evidence<'_>) -> Result<LookupStatus> {
    let mut tx = client.build_transaction().isolation_level(postgres::IsolationLevel::RepeatableRead).read_only(true).start()?;
    let result = find_in(&mut tx, evidence)?;
    tx.commit()?;
    Ok(result)
}
const TITLE: &str = "Alpha.Beta.Gamma.2024.1080p.WEB-DL.H.264-TESTGRP";
fn evidence(name: &str) -> Evidence<'_> { Evidence { name, display_name: None, size: 5 << 30, files: vec![], inner: vec![] } }

#[test]
fn native_predb_requires_complete_keys_and_matches_legacy_literal_hash_rules() {
    let Some(schema) = fixture() else { return };
    let mut client = schema.pool.get().unwrap();
    client.execute("insert into ext_predb_titles(id,title,source,added_at) values(-7,$1,1,1)", &[&TITLE]).unwrap();
    assert!(matches!(lookup(&mut client, &evidence(TITLE)).unwrap(), LookupStatus::Pending));
    let mut tx = client.transaction().unwrap();
    let BuildStatus::Ready(built) = advance_in(&mut tx, 2).unwrap() else { panic!("incomplete tiny index") };
    assert_eq!(built.titles, 1);
    tx.commit().unwrap();
    for (name, kind) in [(TITLE.to_string(), crate::predb::matcher::Kind::Exact), (crate::predb::keys::md5_hex(TITLE), crate::predb::matcher::Kind::Hash)] {
        let LookupStatus::Ready(found) = lookup(&mut client, &evidence(&name)).unwrap() else { panic!("built index pending") };
        assert_eq!(found.found.unwrap().kind, kind);
        assert_eq!(found.revision, built.revision);
    }
    let LookupStatus::Ready(miss) = lookup(&mut client, &evidence("Different.Full.Title-GROUP")).unwrap() else { panic!("built index pending") };
    assert!(miss.found.is_none());
}

#[test]
fn native_predb_mutation_invalidates_reads_and_rebuild_never_reuses_revision() {
    let Some(schema) = fixture() else { return };
    let mut client = schema.pool.get().unwrap();
    let mut tx = client.transaction().unwrap();
    let BuildStatus::Ready(first) = advance_in(&mut tx, 10).unwrap() else { panic!("incomplete empty index") };
    tx.commit().unwrap();
    client.execute("insert into ext_predb_titles(title,source,added_at) values($1,1,1)", &[&TITLE]).unwrap();
    assert!(matches!(lookup(&mut client, &evidence(TITLE)).unwrap(), LookupStatus::Pending));
    let mut tx = client.transaction().unwrap();
    assert!(verify_read_in(&mut tx, first.revision).unwrap_err().is::<StaleRead>());
    tx.rollback().unwrap();
    let mut tx = client.transaction().unwrap();
    let BuildStatus::Ready(second) = advance_in(&mut tx, 10).unwrap() else { panic!("incomplete tiny index") };
    assert!(second.revision > first.revision);
    assert!(verify_read_in(&mut tx, first.revision).unwrap_err().is::<StaleRead>());
    verify_read_in(&mut tx, second.revision).unwrap();
    tx.commit().unwrap();
    client.execute("delete from ext_predb_title_keys", &[]).unwrap();
    assert!(matches!(lookup(&mut client, &evidence(TITLE)).unwrap(), LookupStatus::Pending));
}

#[test]
fn native_predb_rebuild_quota_and_oversized_titles_refuse_without_certification() {
    let Some(schema) = fixture() else { return };
    let mut client = schema.pool.get().unwrap();
    client.execute("insert into ext_predb_titles(title,source,added_at) values($1,1,1)", &[&TITLE]).unwrap();
    let mut tx = client.transaction().unwrap();
    assert!(advance_in(&mut tx, 0).is_err());
    tx.rollback().unwrap();
    client.execute("update ext_predb_titles set title=repeat('x',4097)", &[]).unwrap();
    let mut tx = client.transaction().unwrap();
    assert!(advance_in(&mut tx, 10).is_err());
    tx.rollback().unwrap();
    assert!(matches!(lookup(&mut client, &evidence(TITLE)).unwrap(), LookupStatus::Pending));
}

#[test]
fn native_predb_build_resumes_committed_pages_and_source_change_restarts_without_serving_partial_keys() {
    let Some(schema) = fixture() else { return };
    let mut client = schema.pool.get().unwrap();
    client.execute("insert into ext_predb_titles(id,title,source,added_at) values(-5,$1,1,1),(7,'Second.Full.Title-GRP',1,1)", &[&TITLE]).unwrap();
    let mut tx = client.transaction().unwrap();
    assert!(matches!(advance_in(&mut tx, 1).unwrap(), BuildStatus::Pending { titles: 1, .. }));
    tx.commit().unwrap();
    assert!(matches!(lookup(&mut client, &evidence(TITLE)).unwrap(), LookupStatus::Pending));
    let mut tx = client.transaction().unwrap();
    assert!(matches!(advance_in(&mut tx, 1).unwrap(), BuildStatus::Pending { titles: 2, .. }));
    tx.commit().unwrap();
    client.execute("delete from ext_predb_titles where id=-5", &[]).unwrap();
    let mut tx = client.transaction().unwrap();
    assert!(matches!(advance_in(&mut tx, 1).unwrap(), BuildStatus::Pending { titles: 1, .. }));
    tx.commit().unwrap();
    let mut tx = client.transaction().unwrap();
    assert!(matches!(advance_in(&mut tx, 1).unwrap(), BuildStatus::Ready(Built { titles: 1, .. })));
    tx.commit().unwrap();
    let LookupStatus::Ready(result) = lookup(&mut client, &evidence(TITLE)).unwrap() else { panic!("completed index pending") };
    assert!(result.found.is_none());
}

#[test]
fn native_predb_incremental_import_preserves_title_identity_and_never_rebuilds_after_each_feed_row() {
    use crate::predb::import::Entry;
    let Some(schema) = fixture() else { return };
    let mut client = schema.pool.get().unwrap();
    let mut tx = client.transaction().unwrap();
    assert!(matches!(import_in(&mut tx, 1, &[], 5, None).unwrap(), ImportStatus::Pending));
    let BuildStatus::Ready(empty) = advance_in(&mut tx, 10).unwrap() else { panic!("empty build pending") };
    tx.commit().unwrap();
    let mut tx = client.transaction().unwrap();
    let first = Entry { title: TITLE.to_string(), ..Default::default() };
    let ImportStatus::Applied { counts, revision } = import_in(&mut tx, 1, &[first], 5, None).unwrap() else { panic!("completed index pending") };
    assert_eq!(counts.inserted, 1); assert!(revision > empty.revision);
    tx.commit().unwrap();
    let mut tx = client.transaction().unwrap();
    let second = Entry { title: TITLE.to_lowercase(), category: Some("TV".into()), size_bytes: Some(123), nuked: 7, nuke_reason: Some("reason".into()), ..Default::default() };
    let ImportStatus::Applied { counts, .. } = import_in(&mut tx, 9, &[second], 6, None).unwrap() else { panic!("completed index pending") };
    assert_eq!(counts.updated, 1); assert_eq!(counts.inserted, 0);
    tx.commit().unwrap();
    let row = client.query_one("select title,source,category,size_bytes,nuked,nuke_reason from ext_predb_titles", &[]).unwrap();
    assert_eq!(row.get::<_,String>(0), TITLE); assert_eq!(row.get::<_,i64>(1), 1);
    assert_eq!(row.get::<_,String>(2), "TV"); assert_eq!(row.get::<_,i64>(3), 123);
    assert_eq!(row.get::<_,i64>(4), 7); assert_eq!(row.get::<_,String>(5), "reason");
    assert!(matches!(lookup(&mut client, &evidence(TITLE)).unwrap(), LookupStatus::Ready(_)));
}

#[test]
fn native_predb_whole_page_keeps_total_progress_when_last_chunk_is_known_and_rollback_replays() {
    use crate::predb::import::Entry;
    let Some(schema)=fixture() else{return};let mut client=schema.pool.get().unwrap();
    let mut tx=client.transaction().unwrap();advance_in(&mut tx,512).unwrap();
    let known=Entry{title:TITLE.into(),..Default::default()};import_in(&mut tx,1,&[known.clone()],1,None).unwrap();
    tx.execute("insert into ext_predb_feeds(source,key,resume_page,last_ok_at) values(1,'fixture',41,1)",&[]).unwrap();tx.commit().unwrap();
    let mut page=(0..512).map(|index|Entry{title:format!("New.Genuine.Title.{index:04}-GRP"),..Default::default()}).collect::<Vec<_>>();page.push(known);
    let mut tx=client.transaction().unwrap();
    let ImportStatus::Applied{counts,..}=import_page_in(&mut tx,1,&page,2,Some((42,1))).unwrap() else {panic!("completed keys pending")};
    assert_eq!(counts.inserted,512);assert_eq!(tx.query_one("select resume_page from ext_predb_feeds where source=1",&[]).unwrap().get::<_,i64>(0),42);
    // Interruption before commit leaves neither an early chunk nor its cursor.
    tx.rollback().unwrap();
    assert_eq!(client.query_one("select count(*) from ext_predb_titles",&[]).unwrap().get::<_,i64>(0),1);
    assert_eq!(client.query_one("select resume_page from ext_predb_feeds where source=1",&[]).unwrap().get::<_,i64>(0),41);
    let mut tx=client.transaction().unwrap();
    let ImportStatus::Applied{counts,..}=import_page_in(&mut tx,1,&page,2,Some((42,1))).unwrap() else {panic!("completed keys pending")};
    assert_eq!(counts.inserted,512);tx.commit().unwrap();
    let mut tx=client.transaction().unwrap();
    let ImportStatus::Applied{counts,..}=import_page_in(&mut tx,1,&page,3,Some((42,1))).unwrap() else {panic!("completed keys pending")};
    assert_eq!(counts.inserted,0);tx.commit().unwrap();
    let row=client.query_one("select resume_page,inserted from ext_predb_feeds where source=1",&[]).unwrap();
    assert_eq!(row.get::<_,i64>(0),42);assert_eq!(row.get::<_,i64>(1),512);
}
