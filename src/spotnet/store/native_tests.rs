//! Native counterparts of the retired shard-storage behavior controls.
use super::*;
use crate::pg::fresh_reader::{Budget, ReadView};
use crate::spotnet::{native_plan::Plan, native_read_fixture_tests as fixture, native_store::Store};
use std::time::{Duration, Instant};

fn detail(schema: &crate::pg::testing::Schema, id: i64) -> Detail {
    let mut client = schema.pool.get().unwrap();
    let view =
        ReadView::open(&mut client, &schema.name, Budget::default(), Instant::now() + Duration::from_secs(5)).unwrap();
    view.spot_view(id).unwrap().unwrap().0
}

#[test]
#[ignore = "explicit isolated PG; real original-owner publication and copy settlement"]
fn native_copies_keep_newest_tag_queue_category_and_leave_missing_release_absent() {
    let (schema, runtime, _dir, id) = fixture::fixture();
    let storage = Store::new(&runtime.owner).unwrap();
    let mut observer = schema.pool.get().unwrap();
    let original = detail(&schema, id);
    let key: String = observer.query_one("select nzb_key from spots where release_id=$1", &[&id]).unwrap().get(0);
    let count: i64 = observer.query_one("select count(*) from releases", &[]).unwrap().get(0);
    let mut row = SpotRow {
        message_id: "edited@spot.net".into(),
        kind: Kind::Series,
        spot: original.spot.clone(),
        spotter_id: Some("actual".into()),
        nzb_key: key,
        xml: fixture::XML.replace("Real.Spot\0Original", "Edited.Spot\0Original"),
    };
    row.spot.title = "Edited.Spot\0Original".into();
    storage
        .with(|tx| {
            note_read(
                &crate::pg::sql::Pg::new(&mut *tx),
                "copy-account",
                3,
                &[
                    ("edited@spot.net".into(), 1, None),
                    ("older@spot.net".into(), 2, None),
                    ("missing@spot.net".into(), 3, None),
                ],
                1,
            )?;
            tx.execute("delete from pp_queue where release_id=$1", &[&id])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(storage.copy_of(id, &row, "edited-ref", 2).unwrap().release_id, Some(id));
    assert_eq!(detail(&schema, id).spot.title, "Edited.Spot\0Original");
    assert_eq!(
        observer.query_one("select count(*) from pp_queue where release_id=$1", &[&id]).unwrap().get::<_, i64>(0),
        1
    );
    row.message_id = "older@spot.net".into();
    row.spot.created -= 1;
    row.spot.title = "An older spot must not win".into();
    storage.copy_of(id, &row, "older-ref", 3).unwrap();
    assert_eq!(detail(&schema, id).message_id, "edited@spot.net");
    row.message_id = "missing@spot.net".into();
    let absent = id.checked_add(1000).unwrap();
    assert_eq!(storage.copy_of(absent, &row, "missing-ref", 4).unwrap().release_id, Some(absent));
    assert_eq!(
        observer.query_one("select count(*) from spots where release_id=$1", &[&absent]).unwrap().get::<_, i64>(0),
        0
    );
    assert_eq!(observer.query_one("select count(*) from releases", &[]).unwrap().get::<_, i64>(0), count);
    assert_eq!(known_ref(&crate::pg::sql::Pg::new(&mut *observer), "edited-ref").unwrap(), Some((SAVED, Some(id))));
    runtime.owner.close();
}

#[test]
#[ignore = "explicit isolated PG; native missing-original refusal and guarded XML backfill"]
fn native_xml_backfill_is_identity_guarded_once_and_never_invents_originals() {
    let (schema, runtime, _dir, id) = fixture::fixture();
    let storage = Store::new(&runtime.owner).unwrap();
    storage
        .with(|tx| {
            tx.execute("update spots set xml=null,native_original_xml=null where release_id=$1", &[&id])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(storage.missing_xml(10).unwrap(), [(id, "real@spot.net".into())]);
    {
        let mut client = schema.pool.get().unwrap();
        let view =
            ReadView::open(&mut client, &schema.name, Budget::default(), Instant::now() + Duration::from_secs(5))
                .unwrap();
        assert!(view.spot_view(id).is_err(), "stored columns never replace missing original XML");
    }
    assert!(!storage.set_xml(id, "other@spot.net", fixture::XML).unwrap());
    assert!(storage.set_xml(id, "real@spot.net", fixture::XML).unwrap());
    assert!(!storage.set_xml(id, "real@spot.net", "").unwrap());
    assert!(storage.missing_xml(10).unwrap().is_empty());
    let read = detail(&schema, id);
    assert_eq!(read.xml, Xml::Kept);
    assert_eq!(read.spot, crate::spotnet::full::parse(fixture::XML).unwrap());
    runtime.owner.close();
}

#[test]
#[ignore = "explicit isolated PG; actual original readers and authenticated disposal identities"]
fn native_removed_and_blacklisted_tags_keep_reasons_and_ignore_other_spotters() {
    let (schema, runtime, _dir, id) = fixture::fixture();
    let storage = Store::new(&runtime.owner).unwrap();
    let mut spot = crate::spotnet::full::parse(fixture::XML).unwrap();
    spot.created = 1;
    let key = runtime
        .owner
        .with(|client| {
            Ok(client.query_one("select nzb_key from spots where release_id=$1", &[&id])?.get::<_, String>(0))
        })
        .unwrap();
    let row = SpotRow {
        message_id: "real@spot.net".into(),
        kind: Kind::Series,
        spot,
        spotter_id: Some("actual".into()),
        nzb_key: key,
        xml: fixture::XML.replace("<Key>1</Key>", "<Key>1</Key><Created>1</Created>"),
    };
    storage.copy_of(id, &row, "dated-ref", 1).unwrap();
    let removed = |spotter: &str, at| Disposal { message_id: "real@spot.net".into(), spotter: spotter.into(), at };
    storage.with(|tx| Ok(note_disposals(&crate::pg::sql::Pg::new(tx), &[removed("other", 10)])?)).unwrap();
    assert_eq!(detail(&schema, id).removed, None);
    storage
        .with(|tx| Ok(note_disposals(&crate::pg::sql::Pg::new(tx), &[removed("actual", 1 + OWN_DISPOSAL_SECS + 1)])?))
        .unwrap();
    assert_eq!(detail(&schema, id).removed, None, "own removal after five days does not hide a saved spot");
    storage
        .with(|tx| {
            tx.execute("delete from spot_disposals where spotter='actual'", &[])?;
            Ok(())
        })
        .unwrap();
    storage.with(|tx| Ok(note_disposals(&crate::pg::sql::Pg::new(tx), &[removed("actual", 10)])?)).unwrap();
    assert_eq!(detail(&schema, id).removed, Some(Removed { at: 10, by: RemovedBy::Spotter }));
    storage
        .with(|tx| {
            tx.execute("delete from spot_disposals", &[])?;
            super::super::blacklist::replace(&crate::pg::sql::Pg::new(tx), &["actual".into()], 11)?;
            Ok(())
        })
        .unwrap();
    assert_eq!(detail(&schema, id).removed, Some(Removed { at: 0, by: RemovedBy::Blacklist }));
    {
        let mut client = schema.pool.get().unwrap();
        let view =
            ReadView::open(&mut client, &schema.name, Budget::default(), Instant::now() + Duration::from_secs(5))
                .unwrap();
        assert!(view.spot_tags(&[id]).unwrap().is_empty());
    }
    storage.with(|tx| Ok(note_disposals(&crate::pg::sql::Pg::new(tx), &[removed("", 12)])?)).unwrap();
    assert_eq!(detail(&schema, id).removed, Some(Removed { at: 12, by: RemovedBy::Moderator }));
    runtime.owner.close();
}

#[test]
#[ignore = "explicit isolated PG; actual writer file/MID placement without a shard index"]
fn native_placement_uses_the_actual_file_and_mid_pair_before_tagging() {
    use crate::archive::nzb::{NzbFile, Parsed, Segment};
    let (schema, runtime, _dir) = super::super::native_peer_tests::fixture();
    let storage = Store::new(&runtime.owner).unwrap();
    let handle = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let permit = handle.block_on(runtime.db.reserve_native_for_owner(&runtime.owner)).unwrap();
    let spot = FullSpot { title: "Exact.Native.Post".into(), created: 1700000000, ..Default::default() };
    let parsed = Parsed {
        complete: true,
        files: vec![NzbFile {
            name: "original.mkv".into(),
            subject: "\"original.mkv\" yEnc (1/1)".into(),
            groups: vec!["example.spot".into()],
            articles: 1,
            total_bytes: 1,
            segments: vec![Segment { number: 1, bytes: 1, message_id: "original@fixture.test".into() }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut seed = Plan::new(&spot, &parsed).unwrap().unwrap();
    while let Some(batch) = seed.next_batch(permit.max_job_bytes()).unwrap() {
        runtime.owner.with(|client| crate::pg::release_writer::save(client, &[batch], 1).map(|_| ())).unwrap();
    }
    let mut observer = schema.pool.get().unwrap();
    let original: i64 = observer.query_one("select id from releases", &[]).unwrap().get(0);
    let mut plan = Plan::new(&spot, &parsed).unwrap().unwrap();
    let row = SpotRow {
        message_id: "matching@spot.net".into(),
        kind: Kind::Movie,
        spot: spot.clone(),
        spotter_id: None,
        nzb_key: plan.key().into(),
        xml: String::new(),
    };
    storage
        .with(|tx| {
            Ok(note_read(
                &crate::pg::sql::Pg::new(tx),
                "placement",
                2,
                &[(row.message_id.clone(), 1, None), ("different@spot.net".into(), 2, None)],
                1,
            )?)
        })
        .unwrap();
    assert_eq!(storage.publish(&mut plan, &row, "matching-ref", &permit, 2).unwrap().release_id, Some(original));
    // Restore the actual untagged indexed-post state, so the canonical
    // NZB key cannot bypass the file/MID placement comparison under test.
    storage
        .with(|tx| {
            tx.execute("delete from spots where release_id=$1", &[&original])?;
            tx.execute("delete from spots_seen where message_id='matching@spot.net'", &[])?;
            assert_eq!(known_key(&crate::pg::sql::Pg::new(tx), &row.nzb_key)?, None);
            Ok(())
        })
        .unwrap();
    let mut wrong = parsed.clone();
    wrong.files[0].name = "other.mkv".into();
    wrong.files[0].subject = "\"other.mkv\" yEnc (1/1)".into();
    let mut plan = Plan::new(&spot, &wrong).unwrap().unwrap();
    let wrong_row = SpotRow { message_id: "different@spot.net".into(), nzb_key: plan.key().into(), ..row };
    assert_ne!(storage.publish(&mut plan, &wrong_row, "different-ref", &permit, 3).unwrap().release_id, Some(original));
    assert_eq!(
        observer.query_one("select count(*) from segments where release_id=$1", &[&original]).unwrap().get::<_, i64>(0),
        1
    );
    assert_eq!(
        observer
            .query_one("select original_filename from files where release_id=$1", &[&original])
            .unwrap()
            .get::<_, Option<Vec<u8>>>(0),
        Some(b"original.mkv".to_vec())
    );
    drop(permit);
    runtime.owner.close();
}
