//! Canonical fresh schema and actual media mutation regressions. Source only
//! until the isolated PostgreSQL qualification gate supplies its owned URL.

#[test]
fn fresh_stage_preserves_every_registered_added_column() {
    let Some(schema) = super::testing::Schema::new() else { return };
    let mut client = schema.pool.get().unwrap();
    // Independent baseline vector: the original 40 SHARD_TABLES additions
    // and RELEASE_COLUMNS' releases.name_source make all 41 contracts.
    // The retired registry is no longer needed to exercise the real PG schema.
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
        ("releases", "name_source", "INTEGER"),
    ];
    for &(table, column, declaration) in COLUMNS {
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
