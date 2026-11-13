//! Native queue/receipt coverage for retained Sidecar storage semantics.
use crate::pg::{
    self,
    stage_queue::{Claim, Queue, Settlement, Visit},
};
use crate::postprocess::{
    apply::Applied,
    step::{SidecarRecord, StepMutation, StepWrite},
};

fn settle(queue: &Queue, claim: &Claim, record: SidecarRecord, now: i64) {
    let id = claim.release_id();
    let bit = crate::postprocess::step::mask(super::SIDECAR_STEP);
    let writes = vec![
        (bit, StepWrite::for_mutation(id, StepMutation::KeepSidecar(record))),
        (bit, StepWrite::for_mutation(id, StepMutation::EvictSidecarTexts { max_bytes: 10 })),
    ];
    let applied = Applied { release_id: id, set_done: bit, writes, ..Default::default() };
    assert_eq!(
        queue.settle(claim, Visit::prepare(claim, applied, [7; 32]).unwrap(), now).unwrap(),
        Settlement::Applied
    );
}

#[test]
#[ignore = "requires explicit isolated PostgreSQL original-owner queue and Sidecar quota qualification"]
fn native_sidecar_quota_settlement_preserves_names_and_unread_reruns() {
    let schema = pg::testing::Schema::new().expect("explicit isolated PG required");
    let runtime = pg::stage_queue_tests::runtime(&schema);
    pg::stage_queue_tests::push(&runtime);
    let queue = Queue::for_owner(&runtime.owner).unwrap();
    let claims = queue.claim(10, 1, 60).unwrap();
    assert_eq!(claims.len(), 1);
    let own = claims[0].release_id();
    // Sidecar records deliberately outlive their source releases. Their text
    // counters are produced by actual PG triggers, never hand-written meta.
    runtime
        .owner
        .with(|client| {
            client.batch_execute(
                "insert into sidecars(release_id,name,group_name,kind,release_name,text,at) values
            (-101,'older-a','example.group','nfo','Name.A','éé',1),
            (-102,'older-b','example.group','nfo','Name.B','bbbb',2)",
            )?;
            assert_eq!(
                client.query_one("select value from meta where key='sidecar_text_bytes'", &[])?.get::<_, i64>(0),
                8
            );
            Ok(())
        })
        .unwrap();
    settle(
        &queue,
        &claims[0],
        SidecarRecord {
            name: "native-sidecar".into(),
            group: "example.group".into(),
            file: Some("new.nfo".into()),
            kind: "nfo".into(),
            release_name: Some("Name.Own".into()),
            segment_uid: Some("55f84b6aa194d59703ccab394da26cde".into()),
            text: Some("cccccc".into()),
            at: 3,
        },
        11,
    );
    runtime
        .owner
        .with(|client| {
            let row = client.query_one("select release_name,text from sidecars where release_id=-101", &[])?;
            assert_eq!(row.get::<_, String>(0), "Name.A");
            assert_eq!(row.get::<_, Option<String>>(1), None);
            assert_eq!(
                client.query_one("select text from sidecars where release_id=-102", &[])?.get::<_, String>(0),
                "bbbb"
            );
            assert_eq!(
                client.query_one("select value from meta where key='sidecar_text_bytes'", &[])?.get::<_, i64>(0),
                10
            );
            let mut tx = client.transaction()?;
            assert!(pg::stage_queue::rerun_in(&mut tx, own, crate::postprocess::step::mask(super::SIDECAR_STEP), 20)?);
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    let claims = queue.claim(20, 1, 60).unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].release_id(), own);
    settle(
        &queue,
        &claims[0],
        SidecarRecord {
            name: "native-sidecar".into(),
            group: "example.group".into(),
            file: None,
            kind: "unread".into(),
            release_name: None,
            segment_uid: None,
            text: None,
            at: 4,
        },
        21,
    );
    runtime
        .owner
        .with(|client| {
            let row = client.query_one(
                "select file,kind,release_name,segment_uid,text from sidecars where release_id=$1",
                &[&own],
            )?;
            assert_eq!(row.get::<_, String>(0), "new.nfo");
            assert_eq!(row.get::<_, String>(1), "nfo");
            assert_eq!(row.get::<_, String>(2), "Name.Own");
            assert_eq!(row.get::<_, String>(3), "55f84b6aa194d59703ccab394da26cde");
            assert_eq!(row.get::<_, String>(4), "cccccc");
            assert_eq!(
                client.query_one("select value from meta where key='sidecar_text_bytes'", &[])?.get::<_, i64>(0),
                10
            );
            Ok(())
        })
        .unwrap();
    runtime.owner.close();
}
