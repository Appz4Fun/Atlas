//! The preservation CLI reads a selected snapshot without changing it.
use std::path::Path;
use std::process::{Command, Output};

struct Schema {
    name: String,
    url: String,
    client: postgres::Client,
}

impl Schema {
    fn empty() -> Option<Self> {
        let url = std::env::var("ATLAS_PG_TEST_URL").ok().filter(|s| !s.is_empty())?;
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let name =
            format!("t_move_ext_{}_{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let mut client = postgres::Client::connect(&url, postgres::NoTls).expect("test database connection");
        client.batch_execute(&format!("create schema {name}; set search_path to {name}")).unwrap();
        Some(Self { name, url, client })
    }
}

impl Drop for Schema {
    fn drop(&mut self) {
        let _ = self.client.batch_execute(&format!("drop schema {} cascade", self.name));
    }
}

fn sqlite_files(dir: &Path) {
    let main = dir.join("atlas.db");
    atlas::db::create_at(&main).unwrap();
    let p = atlas::predb::db::create(&atlas::predb::db::path_for(&main)).unwrap();
    p.execute("insert into titles (id, title, source, added_at) values (1, 'Current.Title', 3, 10)", []).unwrap();
    let raw = atlas::raw_store::create(&atlas::raw_store::path_for(&main)).unwrap();
    raw.execute_batch(
        "insert into raw_predb_log values (1, 3, 'feed', 'https://example.invalid/current', 1, x'01', x'a1', 10);
        insert into raw_predb_events values (1, 1, 1, 10)",
    )
    .unwrap();
    atlas::media::store::open(&atlas::media::store::path_for(&main)).unwrap();
    let d = atlas::db::create_at(&atlas::media::details::path_for(&main)).unwrap();
    d.execute_batch(
        "create table details (kind integer, tmdb_id integer, fetched_at integer);
        create table details_lookups (lookup text, tmdb_id integer, checked_at integer)",
    )
    .unwrap();
}

fn run(s: &Schema, args: &[&str]) -> Output {
    let home = tempfile::tempdir().unwrap();
    sqlite_files(home.path());
    let config = serde_json::json!({"storage": {"backend": "postgres", "url": s.url, "schema": s.name, "allow_plaintext": true}});
    std::fs::write(home.path().join("config.toml"), toml_edit::ser::to_string(&config).unwrap()).unwrap();
    Command::new(env!("CARGO_BIN_EXE_atlas"))
        .args(["--move-ext", "--apply"])
        .args(args)
        .env("ATLAS_HOME", home.path())
        .env("ATLAS_NO_KEYRING", "1")
        .env("ATLAS_SAB_DIR", home.path().join("sab"))
        .output()
        .unwrap()
}

#[test]
fn cli_selected_partial_keep_adds_missing_facts_and_leaves_source_unchanged() {
    let Some(mut source) = Schema::empty() else { return };
    let Some(mut destination) = Schema::empty() else { return };
    let c = &mut source.client;
    c.batch_execute(
        "create table predb_titles (id bigint, title text, source bigint, added_at bigint);
        insert into predb_titles values (1, 'CURRENT.TITLE', 5, 20), (2, 'Preserved.Title', 3, 5)",
    )
    .unwrap();
    c.batch_execute("create table predb_pages (id bigint, source_id bigint, source_key text, url text, page bigint, body_sha256 bytea, body_zst bytea, fetched_at bigint);
        insert into predb_pages values (1, 3, 'feed', 'https://example.invalid/old', 4, decode('02','hex'), decode('a2','hex'), 20),
            (8, 3, 'feed', 'https://example.invalid/current', 1, decode('01','hex'), decode('a1','hex'), 10);
        create table predb_page_events (seq bigint, log_id bigint, page bigint, fetched_at bigint);
        insert into predb_page_events values (1, 1, 4, 30), (2, 8, 1, 10);
        create table freeze_exempt (name text, group_name text, at bigint, release_id bigint);
        insert into freeze_exempt values ('Preserved.Title', 'example.group', 50, 7);
        create table predb_meta (key text, value bigint);
        insert into predb_meta values ('kept_meta', 42)").unwrap();
    let snapshot = |c: &mut postgres::Client| -> Vec<String> {
        ["predb_titles", "predb_pages", "predb_page_events", "freeze_exempt", "predb_meta"]
            .iter()
            .map(|table| {
                c.query_one(
                    &format!(
                        "select coalesce(jsonb_agg(t order by to_jsonb(t)::text), '[]'::jsonb)::text from {table} t"
                    ),
                    &[],
                )
                .unwrap()
                .get(0)
            })
            .collect()
    };
    let before = snapshot(c);
    let output = run(&destination, &["--keep-schema", &source.name]);
    assert!(output.status.success(), "move-ext failed");
    let d = &mut destination.client;
    let rows: Vec<(i64, String, i64)> = d
        .query("select id, title, added_at from ext_predb_titles order by id", &[])
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect();
    assert_eq!(rows, [(1, "Current.Title".into(), 10), (2, "Preserved.Title".into(), 5)]);
    let pages: Vec<(i64, Vec<u8>)> = d
        .query("select id, body_zst from ext_predb_pages order by id", &[])
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    assert_eq!(pages, [(1, vec![0xa1]), (2, vec![0xa2])]);
    let events: Vec<(i64, i64, i64, i64)> = d
        .query("select seq, log_id, page, fetched_at from ext_predb_page_events order by seq", &[])
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
        .collect();
    assert_eq!(events, [(1, 1, 1, 10), (2, 2, 4, 30)]);
    let thaw: (String, String, i64, i64) = {
        let r = d.query_one("select name, group_name, at, old_release_id from ext_freeze_exempt", &[]).unwrap();
        (r.get(0), r.get(1), r.get(2), r.get(3))
    };
    assert_eq!(thaw, ("Preserved.Title".into(), "example.group".into(), 50, 7));
    assert_eq!(
        d.query_one("select value from ext_predb_meta where key='kept_meta'", &[]).unwrap().get::<_, i64>(0),
        42
    );
    let after = snapshot(c);
    assert_eq!(after, before);
}

#[test]
fn cli_rejects_invalid_sources_without_committing_destination_changes() {
    let Some(mut source) = Schema::empty() else { return };
    let Some(mut destination) = Schema::empty() else { return };
    let missing = format!("{}_missing", source.name);
    let destination_name = destination.name.clone();
    let cases = [
        (vec!["--keep-schema", "bad;name"], "plain lowercase"),
        (vec!["--keep-schema"], "requires a value"),
        (vec!["--keep-schema", "--apply"], "requires a value"),
        (vec!["--keep-schema", missing.as_str()], "does not exist"),
        (vec!["--keep-schema", source.name.as_str()], "no compatible"),
        (vec!["--keep-schema", destination_name.as_str()], "destination"),
    ];
    for (args, message) in cases {
        let output = run(&destination, &args);
        assert!(!output.status.success(), "invalid source was accepted");
        assert!(
            (String::from_utf8_lossy(&output.stdout).contains(message)
                || String::from_utf8_lossy(&output.stderr).contains(message)),
            "missing source diagnostic: {message}"
        );
        let count: i64 = destination
            .client
            .query_one("select count(*) from information_schema.tables where table_schema=$1", &[&destination.name])
            .unwrap()
            .get(0);
        assert_eq!(count, 0, "refused source committed destination tables");
    }
    for ddl in [
        "create table predb_titles (id bigint, title text)",
        "drop table predb_titles; create table predb_titles (id text, title text, source bigint, added_at bigint)",
        "drop table predb_titles; create view predb_titles as select 1::bigint id, 'a'::text title, 3::bigint source, 1::bigint added_at",
        "drop view predb_titles; create table predb_titles (id bigint, title text, source bigint, added_at bigint, unsupported text)",
        "drop table predb_titles; create table predb_page_events (seq bigint, log_id bigint, fetched_at bigint)",
    ] {
        source.client.batch_execute(ddl).unwrap();
        let output = run(&destination, &["--keep-schema", &source.name]);
        assert!(!output.status.success(), "incompatible source was accepted");
        assert!(
            (String::from_utf8_lossy(&output.stdout).contains("incompatible")
                || String::from_utf8_lossy(&output.stderr).contains("incompatible")),
            "missing shape diagnostic"
        );
        let count: i64 = destination
            .client
            .query_one("select count(*) from information_schema.tables where table_schema=$1", &[&destination.name])
            .unwrap()
            .get(0);
        assert_eq!(count, 0, "incompatible source committed destination tables");
    }
}
