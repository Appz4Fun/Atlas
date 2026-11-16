//! Migration 0016: helper writer sessions save beside the owner while it holds its exclusive lock, fenced on its epoch.
use crate::parser::{Article, Release};
use std::sync::Arc;

fn release(name: &str, message_id: &str) -> Release {
    Release {
        name: name.into(),
        group: "alt.binaries.helpers".into(),
        articles: vec![Article {
            filename: Some(format!("{name}.rar")),
            message_id: message_id.into(),
            subject: format!("{name}.rar yEnc (1/1)"),
            part: Some(1),
            total_parts: Some(1),
            bytes: 17,
            ..Article::default()
        }],
        ..Release::default()
    }
}

fn helpers(schema: &super::testing::Schema, epoch: &[u8; 32]) -> super::ingest::Helpers {
    let storage = super::StorageConfig {
        backend: super::Backend::Postgres,
        url: schema.url.clone(),
        schema: schema.name.clone(),
        max_connections: 2,
        allow_plaintext: true,
    };
    super::ingest::Helpers::new(Arc::new(super::Pool::new(&storage).unwrap()), epoch)
}

#[test]
#[ignore = "requires isolated PostgreSQL"]
fn helper_writers_save_beside_the_owner_only_on_its_epoch() {
    let schema = super::testing::Schema::new().expect("explicit isolated PostgreSQL");
    let runtime = super::stage_queue_tests::runtime(&schema);
    let epoch = *runtime.owner.epoch().expect("owner epoch").id();

    // the owner holds its exclusive lock all along: a helper naming its epoch saves anyway
    helpers(&schema, &epoch).save(&[release("Helper.Saved", "<helper@example.test>")], 1).unwrap();
    let saved: i64 = runtime
        .owner
        .with(|client| Ok(client.query_one("select count(*) from releases where name='Helper.Saved'", &[])?.get(0)))
        .unwrap();
    assert_eq!(saved, 1);

    // another epoch is refused
    let mut wrong = epoch;
    wrong[0] ^= 0xff;
    let error = helpers(&schema, &wrong).save(&[release("Helper.Refused", "<refused@example.test>")], 1).unwrap_err();
    assert!(format!("{error:#}").contains("its owner epoch ended"), "{error:#}");

    // a session naming no epoch still waits for the owner's lock, as before
    let mut plain = schema.pool.get().unwrap();
    plain.batch_execute("set lock_timeout = '500ms'").unwrap();
    let error = super::release_writer::save(&mut plain, &[release("Plain.Blocked", "<plain@example.test>")], 1).unwrap_err();
    assert!(format!("{error:#}").contains("lock timeout"), "{error:#}");

    let refused: i64 = runtime
        .owner
        .with(|client| Ok(client.query_one("select count(*) from releases where name <> 'Helper.Saved'", &[])?.get(0)))
        .unwrap();
    assert_eq!(refused, 0);
    runtime.owner.close();
}
