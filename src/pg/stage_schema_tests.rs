//! Canonical fresh schema and actual media mutation regressions. Source only
//! until the isolated PostgreSQL qualification gate supplies its owned URL.

#[test]
fn fresh_stage_preserves_every_registered_added_column() {
    let Some(schema) = super::testing::Schema::new() else { return };
    let mut client = schema.pool.get().unwrap();
    // Literal inventory of all 41 original SHARD_TABLES added-column contracts.
    // PAR2_FILES.creator is included. The root release column is checked separately.
    const COLUMNS: &[(&str, &str, &str)] = &[
        ("pp_state", "net_transient", "INTEGER"),
        ("pp_state", "net_rank", "INTEGER"),
        ("pp_state", "net_left", "INTEGER"),
        ("archive_inner_files", "crc32", "INTEGER"),
        ("archive_inner_files", "mtime", "INTEGER"),
        ("archive_inner_files", "host_os", "INTEGER"),
        ("archive_release", "encryption", "INTEGER"),
        ("archive_release", "names_hidden", "INTEGER"),
        ("archive_release", "password_check", "BLOB"),
        ("archive_release", "packer", "TEXT"),
        ("archive_release", "twin_key", "INTEGER"),
        ("archive_release", "owner_key", "INTEGER"),
        ("archive_release", "zone_key", "INTEGER"),
        ("archive_release", "facts", "INTEGER"),
        ("par2_files", "creator", "TEXT"),
        ("release_fingerprint", "par2_block", "INTEGER"),
        ("release_fingerprint", "par2_blocks", "INTEGER"),
        ("release_fingerprint", "posted_names", "BLOB"),
        ("release_probe", "moviehash", "TEXT"),
        ("release_stills", "phash", "INTEGER"),
        ("media_release", "tmdb_from_tag", "INTEGER NOT NULL DEFAULT 0"),
        ("media_release", "tvdb_from_tag", "INTEGER NOT NULL DEFAULT 0"),
        ("file_timing", "tool", "INTEGER"),
        ("file_timing", "clock_first", "INTEGER"),
        ("file_timing", "clock_last", "INTEGER"),
        ("file_timing", "hw_host", "TEXT"),
        ("file_timing", "hw_key", "TEXT"),
        ("file_timing", "hw_n_first", "INTEGER"),
        ("file_timing", "hw_n_last", "INTEGER"),
        ("release_keys", "file_crc", "INTEGER"),
        ("release_keys", "body_crc", "INTEGER"),
        ("release_keys", "video", "INTEGER"),
        ("release_keys", "audio", "INTEGER"),
        ("release_keys", "mux", "INTEGER"),
        ("release_keys", "encode", "INTEGER"),
        ("release_keys", "thread", "INTEGER"),
        ("release_keys", "subs", "INTEGER"),
        ("release_keys", "fonts", "INTEGER"),
        ("release_keys", "chapters", "INTEGER"),
        ("release_keys", "volumes", "INTEGER"),
        ("spots", "xml", "TEXT"),
    ];
    const RELEASE_COLUMNS: &[(&str, &str, &str)] = &[("releases", "name_source", "INTEGER")];
    for &(table, column, declaration) in COLUMNS.iter().chain(RELEASE_COLUMNS) {
        let row=client.query_one("select data_type,is_nullable,column_default from information_schema.columns where table_schema=$1 and table_name=$2 and column_name=$3",
            &[&schema.name,&table,&column]).unwrap();
        let expected = declaration.split_whitespace().next().unwrap();
        let expected = match expected {
            "INTEGER" => "bigint",
            "BLOB" => "bytea",
            "TEXT" => "text",
            other => panic!("unmapped original column type {other}"),
        };
        assert_eq!(row.get::<_, String>(0), expected, "{table}.{column} type");
        assert_eq!(
            row.get::<_, String>(1),
            if declaration.contains("NOT NULL") { "NO" } else { "YES" },
            "{table}.{column} nullability"
        );
        let default: Option<String> = row.get(2);
        assert_eq!(
            default.as_deref(),
            if declaration.contains("DEFAULT 0") { Some("0") } else { None },
            "{table}.{column} default"
        );
    }
    assert_eq!(COLUMNS.len(), 41, "all original added columns remain covered");
}

#[test]
fn native_file_tag_ids_survive_lookup_fanout_and_wrong_title_updates() {
    use crate::postprocess::mutation::StepMutation;
    let Some(schema)=super::testing::Schema::new() else {return};
    let mut config=crate::config::Config::default();
    config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;
    config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;
    let storage=super::StorageConfig {backend:super::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:1,allow_plaintext:true};
    let headers=crate::config::HeaderStoreConfig {enabled:true,dir:std::env::temp_dir().join("native-tag-fixture").to_str().unwrap().into()};
    let runtime=super::indexer_runtime::Runtime::open(&config,storage,headers,&[],&mut None).unwrap();
    runtime.owner.with(|client| {
        super::release_writer::save(client,&[crate::parser::Release {name:"Example.Series".into(),group:"example.group".into(),articles:vec![crate::parser::Article {filename:Some("sample.bin".into()),message_id:"<part@example.test>".into(),part:Some(1),total_parts:Some(1),bytes:1,..Default::default()}],..Default::default()}],1)?;
        let id:i64=client.query_one("select id from releases",&[])?.get(0);
        let mut tx=client.transaction()?;
        tx.query_one("select id from releases where id=$1 for update",&[&id])?;
        tx.execute("insert into media_release(release_id,kind,title_key,quality) values($1,1,77,0)",&[&id])?;
        super::stage_mutations::apply(&mut tx,id,&StepMutation::PinTagTmdb {tmdb_id:222,kind:crate::media::title::Kind::Tv})?;
        super::stage_mutations::apply(&mut tx,id,&StepMutation::PinTagTvdb {tvdb_id:111})?;
        let ids=crate::media::resolve::Ids {tvdb:Some(900),tmdb:Some(901),tvmaze:Some(444),..Default::default()};
        assert_eq!(super::stage_mutations::apply(&mut tx,id,&StepMutation::FoundMediaIds {title_key:88,ids})?,0);
        assert_eq!(super::stage_mutations::apply(&mut tx,id,&StepMutation::FoundMediaIds {title_key:77,ids})?,1);
        let row=tx.query_one("select tvdb_id,tmdb_id,tvmaze_id,tvdb_from_tag,tmdb_from_tag from media_release where release_id=$1",&[&id])?;
        assert_eq!((row.get::<_,i64>(0),row.get::<_,i64>(1),row.get::<_,i64>(2),row.get::<_,i64>(3),row.get::<_,i64>(4)),(111,222,444,1,1));
        // Both genuine negative/unknown values stay nullable; tag flags alone
        // cannot fabricate a provider answer or an IMDb id.
        assert_eq!(tx.query_one("select imdb_id from media_release where release_id=$1",&[&id])?.get::<_,Option<i64>>(0),None);
        tx.commit()?;Ok(())
    }).unwrap();
    runtime.owner.close();
}

#[test]
fn deleting_a_release_clears_it_from_every_registered_table() {
    use super::stage_queue::{Queue, Settlement, Visit};
    use crate::parser::{Article, Release};
    use crate::postprocess::{
        apply::Applied,
        step::{StepMutation, StepWrite, mask},
    };

    let Some(schema) = super::testing::Schema::new() else {
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let mut config = crate::config::Config::default();
    config.postprocess.enabled = false;
    config.predb.enabled = false;
    config.spots.enabled = false;
    config.inferred_parts = false;
    config.media.tvmaze.enabled = false;
    config.media.allocine.enabled = false;
    config.media.details_per_minute = 0;
    let storage = super::StorageConfig {
        backend: super::Backend::Postgres,
        url: schema.url.clone(),
        schema: schema.name.clone(),
        max_connections: 1,
        allow_plaintext: true,
    };
    let headers = crate::config::HeaderStoreConfig {
        enabled: true,
        dir: home.path().join("headers").to_str().unwrap().into(),
    };
    let runtime =
        super::indexer_runtime::Runtime::open(&config, storage, headers, &[], &mut None).unwrap();
    let release = |name: &str| Release {
        name: name.into(),
        group: "example.cascade".into(),
        articles: vec![Article {
            filename: Some("data.bin".into()),
            subject: "data.bin yEnc (1/1)".into(),
            message_id: format!("<{name}@cascade.test>"),
            part: Some(1),
            total_parts: Some(1),
            bytes: 17,
            ..Default::default()
        }],
        ..Default::default()
    };
    let first = runtime
        .owner
        .with(|client| {
            let saved = super::release_writer::save(
                client,
                &[release("Cascade.Victim"), release("Cascade.Survivor")],
                1,
            )?;
            assert_eq!((saved.releases, saved.files, saved.parts), (2, 2, 2));
            Ok(client
                .query("select id from releases order by case name when 'Cascade.Victim' then 0 else 1 end", &[])?
                .into_iter()
                .map(|row| row.get::<_, i64>(0))
                .collect::<Vec<_>>())
        })
        .unwrap();
    assert_eq!(first.len(), 2);
    let (victim, survivor) = (first[0], first[1]);
    let now = chrono::Utc::now().timestamp();
    let queue = Queue::for_owner(&runtime.owner).unwrap();
    let claims = queue.claim(now, 2, 60).unwrap();
    assert_eq!(claims.len(), 2);
    let bit = mask(crate::category::step::STEP);
    for claim in claims {
        let snapshot = super::stage_reads::StepReads::for_owner(&runtime.owner)
            .unwrap()
            .snapshot(claim.release_id())
            .unwrap()
            .unwrap();
        let mut applied = Applied::native_database(
            claim.release_id(),
            snapshot.rerun_gen,
            claim.due_at(),
            bit,
            0,
        );
        applied.set_done = bit;
        // Retain a genuinely scheduled queue row as well as real state/category
        // facts and receipts; no lease, epoch or result digest is planted.
        applied.queue_at = Some(now + 10);
        applied.writes.push((
            bit,
            StepWrite::for_mutation(
                claim.release_id(),
                StepMutation::SetCategory { category: 2000 },
            ),
        ));
        assert_eq!(
            queue
                .settle(
                    &claim,
                    Visit::prepare(&claim, applied, snapshot.input).unwrap(),
                    now + 1
                )
                .unwrap(),
            Settlement::Applied
        );
    }
    // Separate real leads keep link fixtures legal and independent: neither a
    // self-link nor a survivor reference to the victim can hide broad cleanup.
    let anchors = runtime
        .owner
        .with(|client| {
            super::release_writer::save(
                client,
                &[
                    release("Cascade.Victim.Lead"),
                    release("Cascade.Survivor.Lead"),
                ],
                1,
            )?;
            Ok(client
                .query(
                    "select id from releases where id<>$1 and id<>$2 order by case name when 'Cascade.Victim.Lead' then 0 else 1 end",
                    &[&victim, &survivor],
                )?
                .into_iter()
                .map(|row| row.get::<_, i64>(0))
                .collect::<Vec<_>>())
        })
        .unwrap();
    assert_eq!(anchors.len(), 2);
    queue.request_walk(bit, None, now).unwrap();

    // Exact former SHARD_TABLES release-owned inventory. None means a genuine
    // writer/Queue path above already produced the row. Every other statement
    // is a fixed legal schema fixture, not a generic column/type filler.
    // The bool admits a real lead ID only for the four link relationships.
    const TABLES: &[(&str, &str, Option<&str>, bool)] = &[
        ("pp_queue", "release_id", None, false),
        ("pp_state", "release_id", None, false),
        (
            "archive_inner_files",
            "release_id",
            Some(
                "insert into archive_inner_files(release_id,seq,name,size,source) values($1,1,'inside.bin',17,2)",
            ),
            false,
        ),
        (
            "nzb_pointed",
            "release_id",
            Some("insert into nzb_pointed(release_id) values($1)"),
            false,
        ),
        (
            "archive_release",
            "release_id",
            Some("insert into archive_release(release_id,password) values($1,0)"),
            false,
        ),
        (
            "release_nfo_password",
            "release_id",
            Some(
                "insert into release_nfo_password(release_id,password) values($1,'fixture-password')",
            ),
            false,
        ),
        (
            "par2_files",
            "release_id",
            Some(
                "insert into par2_files(release_id,file_id,set_id,md5,md5_16k,size,name) values($1,decode(repeat('01',16),'hex'),decode(repeat('02',16),'hex'),decode(repeat('03',16),'hex'),decode(repeat('04',16),'hex'),17,'inside.bin')",
            ),
            false,
        ),
        (
            "segment_crc",
            "release_id",
            Some(
                "insert into segment_crc(release_id,message_id,decoded,crc32) values($1,'<crc@cascade.test>',17,42)",
            ),
            false,
        ),
        (
            "rev_volumes",
            "release_id",
            Some("insert into rev_volumes(release_id,volume,size,crc32) values($1,1,17,42)"),
            false,
        ),
        (
            "release_fingerprint",
            "release_id",
            Some("insert into release_fingerprint(release_id) values($1)"),
            false,
        ),
        (
            "upload_link",
            "release_id",
            Some("insert into upload_link(release_id,lead_id) values($1,$2)"),
            true,
        ),
        (
            "upload_link_tried",
            "release_id",
            Some("insert into upload_link_tried(release_id,candidate_id) values($1,$2)"),
            true,
        ),
        (
            "beside_files",
            "row_id",
            Some("insert into beside_files(row_id,release_id) values($1,$2)"),
            true,
        ),
        (
            "release_probe",
            "release_id",
            Some("insert into release_probe(release_id,info) values($1,'{}')"),
            false,
        ),
        (
            "release_nfo",
            "release_id",
            Some(
                "insert into release_nfo(release_id,file,text) values($1,'info.nfo','fixture text')",
            ),
            false,
        ),
        (
            "release_cover",
            "release_id",
            Some(
                "insert into release_cover(release_id,mime,sha256) values($1,'image/jpeg',repeat('a',64))",
            ),
            false,
        ),
        (
            "release_opensubtitles",
            "release_id",
            Some(
                "insert into release_opensubtitles(release_id,moviehash,imdb_id,checked_at) values($1,'0123456789abcdef',7,1)",
            ),
            false,
        ),
        (
            "release_stills",
            "release_id",
            Some(
                "insert into release_stills(release_id,position,sha256,width,height) values($1,0,repeat('b',64),2,2)",
            ),
            false,
        ),
        (
            "release_images",
            "release_id",
            Some(
                "insert into release_images(release_id,position,file,sha256,format,width,height,bytes) values($1,0,'picture.jpg',repeat('c',64),'jpeg',2,2,17)",
            ),
            false,
        ),
        (
            "picture_requests",
            "release_id",
            Some(
                "insert into picture_requests(release_id,file,requested_at) values($1,'picture.jpg',1)",
            ),
            false,
        ),
        (
            "release_audio_print",
            "release_id",
            Some(
                "insert into release_audio_print(release_id,position,at_ms,items) values($1,0,0,decode('01000000','hex'))",
            ),
            false,
        ),
        (
            "release_audio_anchor",
            "release_id",
            Some("insert into release_audio_anchor(release_id,anchor) values($1,42)"),
            false,
        ),
        (
            "predb_match",
            "release_id",
            Some("insert into predb_match(release_id,title_id,kind,matched_at) select $1,id,1,1 from ext_predb_titles where title='Kept.Release'"),
            false,
        ),
        (
            "predb_inner_key",
            "release_id",
            Some("insert into predb_inner_key(release_id,key) values($1,42)"),
            false,
        ),
        ("release_category", "release_id", None, false),
        (
            "media_release",
            "release_id",
            Some("insert into media_release(release_id,kind,title_key,quality) values($1,1,77,0)"),
            false,
        ),
        ("file_timing", "release_id", None, false),
        (
            "release_indexed",
            "release_id",
            Some("insert into release_indexed(release_id,at) values($1,1)"),
            false,
        ),
        (
            "upload_routes",
            "origin_id",
            Some("insert into upload_routes(origin_id,release_id,name) values($1,$2,'route.bin')"),
            true,
        ),
        (
            "release_keys",
            "release_id",
            Some("insert into release_keys(release_id,account) values($1,7)"),
            false,
        ),
        (
            "freeze_pending",
            "release_id",
            Some("insert into freeze_pending(release_id,reason,at) values($1,'fixture',1)"),
            false,
        ),
        (
            "freeze_exempt",
            "release_id",
            Some("insert into freeze_exempt(release_id,at) values($1,1)"),
            false,
        ),
        (
            "spots",
            "release_id",
            Some(
                "insert into spots(release_id,message_id,kind,title) values($1,'<spot@cascade.test>','movie','Fixture Spot')",
            ),
            false,
        ),
    ];
    assert_eq!(TABLES.len(), 33);
    assert_eq!(
        TABLES
            .iter()
            .map(|row| row.0)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        33
    );
    runtime.owner.with(|client| {
        let mut tx = client.transaction()?;
        super::history_gate::native(&mut tx)?;
        tx.batch_execute("set local statement_timeout='5s';set local lock_timeout='1s'")?;
        tx.execute("insert into ext_predb_titles(title,source,added_at) values('Kept.Release',1,1)", &[])?;
        for (id, lead) in [(victim, anchors[0]), (survivor, anchors[1])] {
            for &(table, _, sql, linked) in TABLES {
                if let Some(sql) = sql {
                    let count = if linked { tx.execute(sql, &[&id, &lead])? } else { tx.execute(sql, &[&id])? };
                    assert_eq!(count, 1, "populate {table} for {id}");
                }
            }
            tx.execute("insert into sidecars(release_id,name,group_name,kind,text,at) values($1,'Kept.Sidecar','example.cascade','nfo','kept text',1)", &[&id])?;
        }
        tx.batch_execute("insert into junk_releases(name,group_name,reason,at) values('Kept.Junk','example.cascade','fixture',1);
            insert into junk_collections(key) values(42);
            insert into ext_opensubtitles(moviehash,imdb_id,checked_at) values('0123456789abcdef',7,1);
            insert into ext_media_titles(title_key,kind,title,state,checked_mask,checked_at) values(77,1,'Kept.Show',1,1,1)")?;
        super::natural_cache::put_probe(&mut tx, &crate::raw_store::ProbeRecord {
            name: "Cascade.Victim", group_name: "example.cascade", content_id: &[1,2,3], crc32: Some(42),
            agent: None, info: "{}", moviehash: None, parts: 1, complete: true, config_stamp: 1,
            nfo_password: None, cover: None,
        }, super::natural_cache::Budget { rows: 8, bytes: 65536 })?;
        tx.commit()?;
        Ok(())
    }).unwrap();

    fn rows(
        client: &mut postgres::Client,
        table: &str,
        key: &str,
        id: i64,
    ) -> anyhow::Result<Vec<String>> {
        let sql =
            format!("select row_to_json(t)::text from {table} t where {key}=$1 order by 1 limit 3");
        let rows = client
            .query(&sql, &[&id])?
            .into_iter()
            .map(|row| row.try_get::<_, String>(0))
            .collect::<Result<Vec<_>, _>>()?;
        assert!(rows.iter().all(|row| row.len() <= 65536));
        Ok(rows)
    }
    fn keepers(client: &mut postgres::Client) -> anyhow::Result<Vec<(String, Vec<String>)>> {
        // Read-only inventory of every ext_* cache relation, including caches
        // which have no release ID. Never infer ownership from a column name.
        let mut names = vec![
            "pp_walks".to_owned(),
            "sidecars".into(),
            "junk_releases".into(),
            "junk_collections".into(),
            "native_catalog_identity".into(),
            "native_runtime_epochs".into(),
            "native_runtime_current".into(),
            "native_operations".into(),
        ];
        let caches = client.query("select tablename from pg_tables where schemaname=current_schema() and left(tablename,4)='ext_' order by tablename limit 129", &[])?;
        assert!(caches.len() <= 128);
        names.extend(caches.into_iter().map(|row| row.get::<_, String>(0)));
        let mut total = 0;
        names
            .into_iter()
            .map(|name| {
                assert!(
                    name.bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
                );
                let values = client
                    .query(
                        &format!("select row_to_json(t)::text from {name} t order by 1 limit 65"),
                        &[],
                    )?
                    .into_iter()
                    .map(|row| row.try_get::<_, String>(0))
                    .collect::<Result<Vec<_>, _>>()?;
                assert!(values.len() <= 64);
                for row in &values {
                    total += row.len();
                    assert!(row.len() <= 65536 && total <= 1024 * 1024);
                }
                Ok((name, values))
            })
            .collect()
    }
    runtime
        .owner
        .with(|client| {
            client.batch_execute("set statement_timeout='5s';set lock_timeout='1s'")?;
            let mut survivor_rows = Vec::new();
            for &(table, key, _, _) in TABLES {
                assert_eq!(
                    rows(client, table, key, victim)?.len(),
                    1,
                    "victim must populate {table} before deletion"
                );
                let saved = rows(client, table, key, survivor)?;
                assert_eq!(
                    saved.len(),
                    1,
                    "survivor must populate {table} before deletion"
                );
                survivor_rows.push(saved);
            }
            let kept = keepers(client)?;
            for name in [
                "pp_walks",
                "sidecars",
                "junk_releases",
                "junk_collections",
                "ext_opensubtitles",
                "ext_media_titles",
                "ext_predb_titles",
                "ext_raw_probes",
            ] {
                assert!(
                    !kept.iter().find(|row| row.0 == name).unwrap().1.is_empty(),
                    "populated keeper {name}"
                );
            }
            assert_eq!(
                client
                    .query_one("select count(*) from native_operations", &[])?
                    .get::<_, i64>(0),
                2,
                "real Queue receipts"
            );
            let mut tx = client.transaction()?;
            super::history_gate::native(&mut tx)?;
            assert_eq!(
                tx.execute("delete from releases where id=$1", &[&victim])?,
                1
            );
            tx.commit()?;
            for (&(table, key, _, _), saved) in TABLES.iter().zip(survivor_rows) {
                assert!(
                    rows(client, table, key, victim)?.is_empty(),
                    "deleted release left rows in {table}"
                );
                assert_eq!(
                    rows(client, table, key, survivor)?,
                    saved,
                    "unrelated release changed in {table}"
                );
            }
            assert_eq!(
                keepers(client)?,
                kept,
                "release deletion must retain global/outliving facts and actual receipts"
            );
            assert_eq!(
                client
                    .query_one(
                        "select count(*) from releases where id=$1 or id=$2 or id=$3",
                        &[&survivor, &anchors[0], &anchors[1]]
                    )?
                    .get::<_, i64>(0),
                3
            );
            for table in ["files", "segments"] {
                assert!(
                    rows(client, table, "release_id", victim)?.is_empty(),
                    "native support rows remain in {table}"
                );
                assert_eq!(rows(client, table, "release_id", survivor)?.len(), 1);
            }
            assert!(
                client
                    .query_opt(
                        "select release_id from word_queue where release_id=$1",
                        &[&victim]
                    )?
                    .is_some(),
                "real delete remains queued for word-index removal"
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(runtime.owner.failure(), None);
    runtime.owner.close();
}
