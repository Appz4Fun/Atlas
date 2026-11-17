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

    // none starts once the owner stops
    runtime.owner.close();
    let error = helpers(&schema, &epoch)
        .of(runtime.owner.clone())
        .save(&[release("Helper.AfterStop", "<after@example.test>")], 1)
        .unwrap_err();
    assert!(crate::pg::outage::is_attempt_ended(&error), "{error:#}");
}

#[test]
#[ignore = "requires isolated PostgreSQL"]
fn concurrent_saves_of_one_group_are_merged_and_all_land() {
    let schema = super::testing::Schema::new().expect("explicit isolated PostgreSQL");
    let runtime = super::stage_queue_tests::runtime(&schema);
    let epoch = *runtime.owner.epoch().expect("owner epoch").id();
    let native = super::ingest::Native::with_owner(runtime.owner.clone(), super::ingest::PgWriteLimits::default())
        .unwrap()
        .with_helpers(helpers(&schema, &epoch).of(runtime.owner.clone()));
    let jobs = 24;
    tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap().block_on(async {
        let saves: Vec<_> = (0..jobs)
            .map(|n| {
                let native = native.clone();
                tokio::spawn(async move { native.save(vec![release(&format!("Merged.{n}"), &format!("<m{n}@example.test>"))]).await })
            })
            .collect();
        for save in saves {
            save.await.unwrap().unwrap();
        }
    });
    let saved: i64 = runtime
        .owner
        .with(|client| Ok(client.query_one("select count(*) from releases where name like 'Merged.%'", &[])?.get(0)))
        .unwrap();
    assert_eq!(saved, jobs as i64, "every job of the group landed");
    runtime.owner.close();
}

#[test]
#[ignore = "requires isolated PostgreSQL"]
fn big_jobs_of_one_group_save_by_shard_side_by_side_without_lock_timeouts() {
    let schema = super::testing::Schema::new().expect("explicit isolated PostgreSQL");
    let runtime = super::stage_queue_tests::runtime(&schema);
    let epoch = *runtime.owner.epoch().expect("owner epoch").id();
    let native = super::ingest::Native::with_owner(runtime.owner.clone(), super::ingest::PgWriteLimits::default())
        .unwrap()
        .with_helpers(helpers(&schema, &epoch).of(runtime.owner.clone()));
    let big = |job: usize| -> Vec<Release> {
        (0..300)
            .map(|n| {
                let mut release = release(&format!("Big.{job}.{n}"), &format!("<big-{job}-{n}-0@example.test>"));
                release.articles = (0..8)
                    .map(|part| Article {
                        filename: Some(format!("Big.{job}.{n}.rar")),
                        message_id: format!("<big-{job}-{n}-{part}@example.test>"),
                        subject: format!("Big.{job}.{n}.rar yEnc ({}/8)", part + 1),
                        part: Some(part + 1),
                        total_parts: Some(8),
                        bytes: 1000,
                        ..Article::default()
                    })
                    .collect();
                release
            })
            .collect()
    };
    let jobs = 8;
    tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap().block_on(async {
        let saves: Vec<_> = (0..jobs)
            .map(|job| {
                let (native, releases) = (native.clone(), big(job));
                tokio::spawn(async move { native.save(releases).await })
            })
            .collect();
        for save in saves {
            save.await.unwrap().expect("every shard save of the group succeeds, no lock wait runs out");
        }
    });
    runtime
        .owner
        .with(|client| {
            let saved: i64 = client.query_one("select count(*) from releases where name like 'Big.%'", &[])?.get(0);
            assert_eq!(saved, (jobs * 300) as i64);
            let kept: i64 = client
                .query_one("select coalesce(sum(total),0)::bigint from native_metric_catalog_groups where group_name='alt.binaries.helpers'", &[])?
                .get(0);
            let shards: i64 = client
                .query_one("select count(*) from native_metric_catalog_groups where group_name='alt.binaries.helpers'", &[])?
                .get(0);
            assert_eq!(kept, saved, "the group's shards sum to its releases");
            assert!(shards > 1, "kept per shard");
            // the rows' shard is the writer's (migration 0021: mod 64), each row counting exactly its shard's releases
            let (top, off): (i16, i64) = {
                let row = client.query_one(
                    "select (select max(shard) from native_metric_catalog_groups where group_name='alt.binaries.helpers'),
                            (select count(*) from native_metric_catalog_groups m where m.group_name='alt.binaries.helpers' and m.total <>
                              (select count(*) from releases r where r.group_name=m.group_name and get_byte(r.identity_key,0)%64=m.shard))",
                    &[],
                )?;
                (row.get(0), row.get(1))
            };
            assert!(top >= 16 && top < 64, "shards past the old 16 are kept, none past 64: {top}");
            assert_eq!(off, 0, "every shard row counts its shard's releases");
            Ok(())
        })
        .unwrap();
    runtime.owner.close();
}
