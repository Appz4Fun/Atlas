//! Fixed synthetic input; no reader9 DDL or test-only readiness publisher.
//! Call only under an explicit isolated PG + syscall-audit execution lease.
use super::subject as atlas;
use atlas::history_source::{CatalogCounts, FileManifest, Manifest, Source, SourceKind};
use atlas::pg::history_import::{ExpectedDestination, ExpectedTransport, ImportBudget, ImportOwner};
use rusqlite::types::Value;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const KEY: &str = "synthetic-web-key";
pub const CURRENT: [i64; 8] = [3, 5, 7, 9, 11, 13, 15, 17];
pub const FROZEN: [i64; 9] = [3, 19, 21, 23, 25, 27, 29, 31, 33];

const RELEASES: &str = "create table releases(id integer primary key,name text,group_name text,poster text,posted_date text,size integer,complete integer,parts integer,file_total integer,display_name text,is_obfuscated integer,name_source integer)";
const FILES: &str = "create table files(id integer primary key,release_id integer not null,filename text not null,subject text,subject_part integer,subject_mid text,expected integer,file_total integer,seen blob not null,touched_at integer,blob blob);create table domains(id integer primary key,suffix text not null);create table segments(file_id integer not null,local blob not null,domain integer not null,part integer,bytes integer,primary key(file_id,local,domain)) without rowid";

fn columns(names: &str) -> Vec<String> { names.split(',').map(str::to_owned).collect() }
fn release(id: i64, embedded: bool) -> Vec<Value> {
    vec![Value::Integer(id), Value::Text(if embedded { format!("Embedded.&<Frozen>.{id}") } else { format!("Raw.&<Current>.{id}") }),
        Value::Text(if embedded { "alt.binaries.payload" } else { "alt.binaries.fixture" }.into()),
        Value::Text("poster@example.invalid".into()), Value::Text("2020-01-01 00:00:00".into()), Value::Integer(34),
        Value::Integer(1), Value::Integer(2), Value::Integer(1),
        if id == 5 { Value::Text(String::new()) } else if id == 7 { Value::Text("Display.Title.7".into()) } else { Value::Null },
        Value::Integer(0), Value::Integer(40)]
}
fn payload(id: i64) -> atlas::frozen::payload::Payload {
    use atlas::frozen::payload::{Payload, Table};
    let file = vec![Value::Integer(id * 10), Value::Integer(id), Value::Text(format!("payload-{id}.bin")),
        Value::Text(format!("\"payload-{id}.bin\" (1/2)")), Value::Null, Value::Null, Value::Integer(2),
        Value::Integer(1), Value::Blob(vec![3]), Value::Integer(1), Value::Null];
    let segments = (1..=2).map(|part| vec![Value::Integer(id * 10),
        Value::Blob([&[0][..], format!("part{part}-id{id}").as_bytes()].concat()), Value::Integer(1),
        Value::Integer(part), Value::Integer(17)]).collect();
    Payload { domains: vec![(1, "@example.invalid>".into())], tables: vec![
        Table { name: "releases".into(), columns: columns("id,name,group_name,poster,posted_date,size,complete,parts,file_total,display_name,is_obfuscated,name_source"), rows: vec![release(id, true)] },
        Table { name: "files".into(), columns: columns("id,release_id,filename,subject,subject_part,subject_mid,expected,file_total,seen,touched_at,blob"), rows: vec![file] },
        Table { name: "segments".into(), columns: columns("file_id,local,domain,part,bytes"), rows: segments },
    ] }
}

fn capture(root: &Path) -> Manifest {
    let mut files = Vec::new();
    for (source_id, kind) in [(1, SourceKind::Current), (2, SourceKind::Main), (3, SourceKind::Frozen)] {
        let path = root.join(format!("source-{source_id}.db"));
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch("pragma journal_mode=delete; create table meta(key text primary key,value integer)").unwrap();
        for name in atlas::history_source::fact_names(kind) {
            connection.execute("insert into meta values(?1,?2)", rusqlite::params![name, if *name == "next_seq" { 900 } else { 17 }]).unwrap();
        }
        match kind {
            SourceKind::Current => {
                connection.execute_batch(RELEASES).unwrap(); connection.execute_batch(FILES).unwrap();
                connection.execute_batch("create table collections(key integer primary key,release_id integer);insert into collections values(0,3),(-7,899);insert into domains values(1,'@example.invalid>')").unwrap();
                for id in CURRENT {
                    connection.execute("insert into releases values(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)", rusqlite::params_from_iter(release(id, false))).unwrap();
                    let p = payload(id);
                    connection.execute("insert into files values(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)", rusqlite::params_from_iter(&p.tables[1].rows[0])).unwrap();
                    for row in &p.tables[2].rows {
                        connection.execute("insert into segments values(?1,?2,?3,?4,?5)", rusqlite::params_from_iter(row)).unwrap();
                    }
                }
            }
            SourceKind::Frozen => {
                connection.execute_batch("create table frozen_releases(id integer primary key,name text,group_name text,display_name text,poster text,posted_date text,size integer,parts integer,complete integer,category integer,reason text not null,frozen_at integer not null,payload blob not null,articles integer);create table frozen_collections(key integer primary key,release_id integer not null) without rowid;insert into frozen_collections values(-9,19),(-11,3)").unwrap();
                for id in FROZEN {
                    let packed = atlas::frozen::payload::encode(&payload(id)).unwrap();
                    connection.execute("insert into frozen_releases values(?1,?2,'alt.binaries.header',null,'header@example.invalid','2020-01-01 00:00:00',34,2,1,5040,'synthetic',1,?3,2)", rusqlite::params![id, format!("Header.Frozen.{id}"), packed]).unwrap();
                }
            }
            SourceKind::Main => {}
        }
        let schema_digest = atlas::history_source::schema_digest(&connection).unwrap(); drop(connection);
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() <= 128 * 1024, "fixed source fixture must stay small");
        files.push(FileManifest { source_id, kind, shard: (kind != SourceKind::Main).then_some(0),
            paired_current: (kind == SourceKind::Frozen).then_some(1), path, byte_length: bytes.len() as u64,
            digest: Sha256::digest(&bytes).into(), schema_digest,
            catalog_counts: match kind { SourceKind::Current => Some(CatalogCounts { releases: 8, collections: 2 }),
                SourceKind::Frozen => Some(CatalogCounts { releases: 9, collections: 2 }), SourceKind::Main => None } });
    }
    Manifest { version: 1, checkpoint_id: "synthetic-web-capture".into(), source_uuid: "synthetic-web-lineage".into(),
        shard_count: 1, reserved_next_id: 900, archive_manifest: [3; 32], main_receipt: [4; 32], ext_receipt: [5; 32],
        grouping_facts: b"captured synthetic grouping facts".to_vec(), immutable_checkpoint: true, files }
}

#[derive(Clone, Serialize, Deserialize)]
pub enum Endpoint { Tcp { host: String, port: u16 }, Unix { path: String } }

fn quoted(value: &str) -> String { format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'")) }
fn reader_url(url: &str, expected: &ExpectedDestination) -> (String, Endpoint) {
    let config: postgres::Config = url.parse().expect("explicit isolated test PostgreSQL URL");
    assert_eq!(config.get_hosts().len(), 1, "syscall audit needs one literal PG endpoint");
    assert!(config.get_ports().len() <= 1);
    let port = config.get_ports().first().copied().unwrap_or(5432);
    let (host, endpoint) = match &config.get_hosts()[0] {
        postgres::config::Host::Tcp(host) => {
            let host = host.parse::<std::net::IpAddr>().expect("literal PG IP avoids unqualified DNS traffic").to_string();
            assert!(host != "127.0.0.1" || port != 9, "PG and provider trap endpoints must differ");
            (host.clone(), Endpoint::Tcp { host, port })
        }
        #[cfg(unix)]
        postgres::config::Host::Unix(path) => (path.to_str().unwrap().into(), Endpoint::Unix { path: path.join(format!(".s.PGSQL.{port}")).to_str().unwrap().into() }),
        _ => panic!("unsupported audited PG transport"),
    };
    let options = format!("{} -c default_transaction_read_only=on", config.get_options().unwrap_or(""));
    let mut values = vec![("host", host), ("port", port.to_string()), ("dbname", expected.database.clone()),
        ("user", expected.role.clone()), ("connect_timeout", "2".into()), ("sslmode", "disable".into()),
        ("application_name", "atlas-web-reader-fixture".into()), ("options", options)];
    if let Some(password) = config.get_password() { values.push(("password", std::str::from_utf8(password).expect("UTF8 test credential").into())); }
    (values.into_iter().map(|(key, value)| format!("{key}={}", quoted(&value))).collect::<Vec<_>>().join(" "), endpoint)
}

struct OwnedSchema { client: postgres::Client, name: String, role: String, oid: Option<u32>, token: String }
impl OwnedSchema {
    fn bind(&mut self) {
        let row = self.client.query_one("select oid,pg_get_userbyid(nspowner) from pg_namespace where nspname=$1", &[&self.name]).unwrap();
        assert_eq!(row.get::<_, String>(1), self.role); self.oid = Some(row.get(0));
        self.client.batch_execute(&format!("comment on schema \"{}\" is '{}'", self.name, self.token)).unwrap();
    }
    fn remove(&mut self) -> Result<(), String> {
        let Some(oid) = self.oid else { return Ok(()); };
        let row = self.client.query_opt("select oid,pg_get_userbyid(nspowner),obj_description(oid,'pg_namespace') from pg_namespace where nspname=$1", &[&self.name]).map_err(|_| "schema identity query failed")?.ok_or("owned schema disappeared")?;
        if row.get::<_, u32>(0) != oid || row.get::<_, String>(1) != self.role || row.get::<_, Option<String>>(2).as_deref() != Some(self.token.as_str()) {
            return Err("schema identity changed; refusing unrelated cleanup".into());
        }
        self.client.batch_execute(&format!("drop schema \"{}\" cascade", self.name)).map_err(|_| "owned schema cleanup failed")?;
        self.oid = None; Ok(())
    }
}
impl Drop for OwnedSchema { fn drop(&mut self) { if let Err(error) = self.remove() { eprintln!("test schema {}: {error}", self.name); } } }

pub struct Fixture {
    pub root: tempfile::TempDir, pub home: PathBuf, pub schema: String, pub import: [u8; 32], pub endpoint: Endpoint,
    pub expected_current_nzb: String, pub expected_frozen_nzb: String,
    evidence: PathBuf,
    db: OwnedSchema, before: Vec<(String, i64, String)>, before_home: Vec<(PathBuf, u32, u64, [u8; 32])>,
    companions: Option<CompanionBindings>,
}

struct CompanionBindings { source: [u8; 32], main: [u8; 32], ext: [u8; 32] }

fn assert_capture_shape(manifest: &Manifest) {
    let mut releases = 0i64; let mut articles = 0usize;
    for file in &manifest.files {
        if file.kind == SourceKind::Main { continue; }
        let connection = rusqlite::Connection::open_with_flags(&file.path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        if file.kind == SourceKind::Current {
            releases += connection.query_row("select count(*) from releases", [], |row| row.get::<_, i64>(0)).unwrap();
            articles += usize::try_from(connection.query_row("select count(*) from segments", [], |row| row.get::<_, i64>(0)).unwrap()).unwrap();
        } else {
            let mut statement = connection.prepare("select payload from frozen_releases order by id").unwrap();
            let mut rows = statement.query([]).unwrap();
            while let Some(row) = rows.next().unwrap() {
                releases += 1;
                let packed: Vec<u8> = row.get(0).unwrap(); assert!(packed.len() <= 65536);
                let decoded = atlas::frozen::payload::decode(&packed).unwrap();
                articles += decoded.tables.iter().filter(|table| table.name == "segments").map(|table| table.rows.len()).sum::<usize>();
            }
        }
    }
    assert_eq!((releases, articles), (17, 34), "preserve the fixed source-format fixture");
}

/// The existing main-import fixture constructs the complete legacy main
/// tables through the real store/chunk/Spotnet setup, including nonempty
/// cursor and reference facts. All sidecars stay private synthetic inputs.
fn companion_inputs(root: &Path, manifest: &mut Manifest) {
    let inputs = root.join("companions"); std::fs::create_dir(&inputs).unwrap(); private(&inputs);
    let main = atlas::pg::main_import::tests::sqlite_snapshot(&inputs);
    let predb = atlas::predb::db::path_for(&main);
    let raw = atlas::raw_store::path_for(&main);
    let media = atlas::media::store::path_for(&main);
    let details = atlas::media::details::path_for(&main);
    drop(atlas::predb::db::create(&predb).unwrap());
    drop(atlas::raw_store::create(&raw).unwrap());
    drop(atlas::media::store::open(&media).unwrap());
    let connection = atlas::db::create_at(&details).unwrap();
    connection.execute_batch(atlas::media::details::SCHEMA).unwrap();
    drop(connection);
    let mut total = 0u64;
    for path in [&main, &predb, &raw, &media, &details] {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection.execute_batch("pragma wal_checkpoint(truncate);pragma journal_mode=delete").unwrap();
        drop(connection); private_file(path);
        let length = std::fs::metadata(path).unwrap().len();
        assert!(length <= 2 * 1024 * 1024, "complete synthetic companion schema exceeded admission");
        total += length;
        for suffix in ["-wal", "-shm", "-journal"] { assert!(!PathBuf::from(format!("{}{suffix}", path.display())).exists()); }
    }
    assert!(total <= 4 * 1024 * 1024, "synthetic companion family exceeded admission");
    let connection = rusqlite::Connection::open(&main).unwrap();
    let schema_digest = atlas::history_source::schema_digest(&connection).unwrap(); drop(connection);
    let bytes = std::fs::read(&main).unwrap();
    let file = manifest.files.iter_mut().find(|file| file.kind == SourceKind::Main).unwrap();
    file.path = main; file.byte_length = bytes.len() as u64; file.digest = Sha256::digest(&bytes).into(); file.schema_digest = schema_digest;
}

fn stored_companions(client: &mut postgres::Client, schema: &str, source: [u8; 32]) -> CompanionBindings {
    let rows = client.query(&format!("select owner_code,source_digest,receipt_digest,manifest,dirty from \"{schema}\".history_companion_receipts order by owner_code"), &[]).unwrap();
    assert_eq!(rows.len(), 2, "both real converter receipts are required");
    let mut found = std::collections::BTreeMap::new();
    for row in rows {
        let owner: String = row.get(0); let bytes: Vec<u8> = row.get(3);
        assert!(bytes.len() <= 65536 && !row.get::<_, bool>(4));
        assert_eq!(row.get::<_, Vec<u8>>(1), source);
        let digest: [u8; 32] = row.get::<_, Vec<u8>>(2).try_into().unwrap();
        assert_eq!(<[u8; 32]>::from(Sha256::digest(&bytes)), digest);
        let receipt: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(receipt["version"], 1); assert_eq!(receipt["owner"], owner);
        assert_eq!(serde_json::from_value::<[u8; 32]>(receipt["main_source_digest"].clone()).unwrap(), source);
        let relations = receipt["relations"].as_array().unwrap();
        assert!(!relations.is_empty() && relations.len() <= 64);
        assert_eq!(receipt["scope"], if owner == "main_import" { "main_compat_v1" } else { "ext_normalized_and_keep_v1" });
        assert!(found.insert(owner, digest).is_none());
    }
    let bindings = CompanionBindings { source, main: *found.get("main_import").unwrap(), ext: *found.get("ext").unwrap() };
    assert_ne!(bindings.main, [4; 32]); assert_ne!(bindings.ext, [5; 32]); bindings
}

impl Fixture {
    pub fn new() -> Self {
        Self::build(false)
    }
    pub fn with_real_companions() -> Self {
        Self::build(true)
    }
    fn build(real_companions: bool) -> Self {
        let url = std::env::var("ATLAS_PG_TEST_URL").expect("requires explicitly admitted isolated test PostgreSQL");
        let evidence_root = PathBuf::from(std::env::var_os("ATLAS_WEB_TEST_EVIDENCE_DIR").expect("requires explicit private evidence placement"));
        assert!(evidence_root.is_absolute() && evidence_root.is_dir());
        for parent in [evidence_root.as_path()].into_iter().chain(evidence_root.ancestors().skip(1)) { assert!(!parent.is_symlink()); }
        #[cfg(unix)] { use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let metadata = evidence_root.metadata().unwrap(); assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700); }
        let root = tempfile::tempdir().unwrap();
        let capture_dir = root.path().join("capture"); let spool = root.path().join("spool"); let home = root.path().join("home");
        for path in [&capture_dir, &spool, &home] { std::fs::create_dir(path).unwrap(); private(path); }
        let mut manifest = capture(&capture_dir);
        if real_companions { companion_inputs(&capture_dir, &mut manifest); assert_capture_shape(&manifest); }
        let mut client = postgres::Client::connect(&url, postgres::NoTls).expect("isolated PG fixture connection");
        let row = client.query_one("select current_database(),current_user,current_setting('server_version_num')::integer,host(inet_server_addr()),inet_server_port()", &[]).unwrap();
        let suffix: String = root.path().file_name().unwrap().to_str().unwrap().chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
        let schema = format!("atlas_stage_web_{}_{}", std::process::id(), suffix);
        assert!(schema.len() < 63 && schema.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'));
        assert!(!client.query_one("select exists(select 1 from pg_namespace where nspname=$1)", &[&schema]).unwrap().get::<_, bool>(0), "never reuse an existing schema");
        let expected = ExpectedDestination { database: row.get(0), schema: schema.clone(), role: row.get(1), owner: row.get(1),
            server_version: row.get(2), server_address: row.get(3), server_port: row.get(4),
            transport: if row.get::<_, Option<String>>(3).is_some() { ExpectedTransport::PlaintextTcp } else { ExpectedTransport::UnixSocket }, staging_prefix: "atlas_stage_web_".into() };
        let mut db = OwnedSchema { client, name: schema.clone(), role: expected.role.clone(), oid: None, token: format!("synthetic-web-fixture-{suffix}") };
        let evidence = evidence_root.join(&schema); std::fs::create_dir(&evidence).unwrap(); private(&evidence);
        let companions = if real_companions {
            let intent = evidence.join("companion-owner-intent.json");
            std::fs::write(&intent, serde_json::to_vec(&serde_json::json!({"top_pid":std::process::id(),"schema":schema,"owner":db.role,"nonce":db.token,"operation":"main_import::import_snapshot"})).unwrap()).unwrap(); private_file(&intent);
            let main = manifest.files.iter().find(|file| file.kind == SourceKind::Main).unwrap();
            let restored = atlas::pg::main_import::import_snapshot(&main.path, &mut db.client, &expected.database, &schema).unwrap();
            // Only a successful production schema-creation transaction can
            // establish our OID/owner/nonce binding. An ambiguous failure is
            // retained for operator review, never prefix-based cleanup.
            db.bind();
            assert!(restored.contains(&("groups", 1)) && restored.contains(&("meta", 2)) && restored.contains(&("spots_seen", 1)));
            let moved = atlas::pg::ext::copy_from_sqlite_with_keep(&mut db.client, &main.path, &schema, true, atlas::pg::ext::KeepSource::Disabled).unwrap();
            assert_eq!(moved.len(), 11); assert!(moved.iter().all(|table| table.from_keep == 0));
            let bindings = stored_companions(&mut db.client, &schema, main.digest);
            manifest.main_receipt = bindings.main; manifest.ext_receipt = bindings.ext;
            Some(bindings)
        } else { None };
        let source = Source::open(manifest.clone(), 4 * 1024 * 1024).unwrap();
        let mut owner = ImportOwner::connect(atlas::pg::StorageConfig { backend: atlas::pg::Backend::Postgres,
            url: url.clone(), schema: schema.clone(), max_connections: 1, allow_plaintext: true }, expected.clone(), spool, 8 * 1024 * 1024).unwrap();
        let import = owner.begin(&source).unwrap();
        if !real_companions { db.bind(); }
        let mut sequence = 0; let mut complete = false;
        for _ in 0..512 {
            let receipt = owner.step(&source, import, sequence, ImportBudget::default()).unwrap(); sequence = receipt.sequence;
            if receipt.complete { complete = true; break; }
        }
        assert!(complete, "fixed fixture must finish within its checkpoint budget");
        owner.finish_identity(&source, import).unwrap(); assert_eq!(owner.status(import).unwrap().0, "identity_ready");
        drop(owner); drop(source);
        assert!(atlas::pg::migrate::version(&mut db.client, &schema).unwrap() >= 8, "use released migration registry");
        db.client.batch_execute(&format!("set search_path to \"{schema}\"")).unwrap();
        assert_eq!(db.client.query_one("select name from schema_migrations where version=8", &[]).unwrap().get::<_, String>(0), "history_catalog");
        {
            use atlas::pg::history_catalog::{Lookup, LookupPurpose, ReadBudget, Snapshot};
            let mut catalog = Snapshot::open(&mut db.client, import).unwrap();
            for id in CURRENT { match catalog.lookup(id, LookupPurpose::Raw, ReadBudget::default()).unwrap() {
                Lookup::Found(found) => {
                    assert_eq!(found.source_id, 1, "current wins shadowed public identity");
                    let expected = if id == 5 { String::new() } else if id == 7 { "Display.Title.7".into() } else { format!("Raw.&<Current>.{id}") };
                    assert_eq!(value_text(&mut catalog, found.metadata.effective_name().unwrap()), Some(expected));
                }, _ => panic!("current identity missing") } }
            for id in FROZEN.into_iter().filter(|id| *id != 3) { match catalog.lookup(id, LookupPurpose::Raw, ReadBudget::default()).unwrap() {
                Lookup::Found(found) => {
                    assert_eq!(found.source_id, 3); assert!(found.header_object.is_some() && found.payload_object.is_some());
                    assert_eq!(value_text(&mut catalog, found.metadata.effective_name().unwrap()), Some(format!("Embedded.&<Frozen>.{id}")));
                }, _ => panic!("frozen identity missing") } }
            assert!(matches!(catalog.lookup(899, LookupPurpose::Raw, ReadBudget::default()).unwrap(), Lookup::Missing));
            let dangling = catalog.resolve_legacy(1, -7).unwrap().unwrap(); assert!(dangling.dangling_in_source && dangling.target_public_id == 899);
            let shadow = catalog.resolve_legacy(3, -11).unwrap().unwrap(); assert!(!shadow.dangling_in_source && shadow.selected_source == Some(1));
        }
        let (reader, endpoint) = reader_url(&url, &expected);
        let mut check = postgres::Client::connect(&reader, postgres::NoTls).unwrap();
        assert_eq!(check.query_one("show default_transaction_read_only", &[]).unwrap().get::<_, String>(0), "on"); drop(check);
        let trap = "http://127.0.0.1:9";
        let config = serde_json::json!({"storage":{"backend":"postgres","url":reader,"schema":schema,"max_connections":1,"allow_plaintext":true},
            "media":{"tvmaze":{"enabled":true,"base_url":trap},"tmdb":{"api_key":"synthetic-key","base_url":trap,"image_base_url":trap},
                "tvdb":{"api_key":"synthetic-key","base_url":trap},"omdb":{"api_key":"synthetic-key","base_url":trap},"trakt":{"client_id":"synthetic-key","base_url":trap},
                "opensubtitles":{"api_key":"synthetic-key","base_url":trap},"allocine":{"enabled":true,"base_url":trap,"wikidata_url":trap}},
            "usenet_servers":[{"host":"127.0.0.1","port":9,"username":"synthetic-user","password":"synthetic-key","ssl":false}]});
        let config_path = home.join("config.json"); std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap(); private_file(&config_path);
        // Trap even attempted fallback opens; syscall auditing includes errors.
        for name in ["atlas.db", "atlas.s0.db", "atlas.raw.db", "atlas.media.db", "atlas.details.db", "atlas.predb.db"] { std::fs::create_dir(home.join(name)).unwrap(); }
        let current = atlas::search::ReleaseRow { id: 3, name: "Raw.&<Current>.3".into(), group_name: "alt.binaries.fixture".into(), complete: true, ..Default::default() };
        let articles = (1..=2).map(|part| atlas::search::ArticleRow { filename: Some("payload-3.bin".into()), message_id: format!("<part{part}-id3@example.invalid>"), part: Some(part), total_parts: Some(2), bytes: Some(17), subject: Some("\"payload-3.bin\" (1/2)".into()), poster: Some("poster@example.invalid".into()), posted_date: Some("2020-01-01 00:00:00".into()) }).collect::<Vec<_>>();
        let expected_current_nzb = atlas::nzb::render_nzb(&current, &articles);
        let oracle = root.path().join("oracle"); std::fs::create_dir(&oracle).unwrap(); private(&oracle);
        std::fs::copy(&manifest.files.iter().find(|file| file.kind == SourceKind::Frozen).unwrap().path, oracle.join("atlas.s0.frozen.db")).unwrap();
        let expected_frozen_nzb = atlas::frozen::nzb(&oracle.join("atlas.db"), 19).unwrap().unwrap().1.xml;
        let before = snapshot(&mut db.client, &schema);
        let before_home = home_inventory(&home);
        Self { root, home, schema, import, endpoint, expected_current_nzb, expected_frozen_nzb, evidence, db, before, before_home, companions }
    }
    pub fn assert_real_companion_control(&mut self) {
        let expected = self.companions.as_ref().expect("real companion constructor required");
        let actual = stored_companions(&mut self.db.client, &self.schema, expected.source);
        assert_eq!((actual.main, actual.ext), (expected.main, expected.ext));
        let row = self.db.client.query_one("select oid,pg_get_userbyid(nspowner),obj_description(oid,'pg_namespace') from pg_namespace where nspname=$1", &[&self.schema]).unwrap();
        assert_eq!(row.get::<_, u32>(0), self.db.oid.unwrap()); assert_eq!(row.get::<_, String>(1), self.db.role);
        assert_eq!(row.get::<_, Option<String>>(2).as_deref(), Some(self.db.token.as_str()));
        let row = self.db.client.query_one(&format!("select state,checkpoint_sequence,source_fingerprint,identity_digest,(select count(*) from history_catalog),(select count(*) from history_release_variants),manifest_object_id from \"{}\".history_imports where import_id=$1", self.schema), &[&&self.import[..]]).unwrap();
        assert_eq!(row.get::<_, String>(0), "identity_ready"); assert!(row.get::<_, i64>(1) > 0);
        assert_eq!(row.get::<_, Vec<u8>>(2), self.import); assert_eq!(row.get::<_, Vec<u8>>(3).len(), 32);
        assert_eq!(row.get::<_, i64>(4), 16); assert_eq!(row.get::<_, i64>(5), 17);
        assert_eq!((CURRENT.len() + FROZEN.len()) * 2, 34);
        let manifest_object: i64 = row.get(6);
        let mut catalog = atlas::pg::history_catalog::Snapshot::open(&mut self.db.client, self.import).unwrap();
        let mut cursor = catalog.object_cursor(manifest_object).unwrap(); let mut bytes = Vec::new(); let mut complete = false;
        for _ in 0..16 {
            let page = catalog.read_value(&mut cursor, Default::default(), |part| {
                assert!(bytes.len() + part.len() <= 65536); bytes.extend_from_slice(part); Ok(())
            }).unwrap();
            if page.complete { complete = true; break; }
        }
        assert!(complete, "fixed captured manifest did not finish");
        let captured: Manifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!((captured.main_receipt, captured.ext_receipt), (actual.main, actual.ext));
        assert_eq!(captured.files.iter().find(|file| file.kind == SourceKind::Main).unwrap().digest, expected.source);
        drop(catalog);
        let binding = self.evidence.join("real-companion-control.json");
        std::fs::write(&binding, serde_json::to_vec(&serde_json::json!({"top_pid":std::process::id(),"schema":self.schema,"schema_oid":self.db.oid,"owner":self.db.role,"nonce":self.db.token,"import":self.import,"main_source":expected.source,"main_receipt":actual.main,"ext_receipt":actual.ext,"source_rows":17,"source_articles":34,"winners":16,"state":"identity_ready"})).unwrap()).unwrap(); private_file(&binding);
    }
    pub fn finish(mut self) {
        assert_eq!(snapshot(&mut self.db.client, &self.schema), self.before, "reader phase mutated the isolated schema");
        assert_eq!(home_inventory(&self.home), self.before_home, "reader phase mutated private settings/cache/file state");
        self.db.remove().expect("only the owned schema may be removed");
    }
}
fn home_inventory(home: &Path) -> Vec<(PathBuf, u32, u64, [u8; 32])> {
    let mut pending = vec![home.to_owned()]; let mut found = Vec::new();
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(&path).unwrap() {
            let path = entry.unwrap().path(); let metadata = std::fs::symlink_metadata(&path).unwrap();
            assert!(!metadata.file_type().is_symlink()); assert!(found.len() + pending.len() < 128);
            #[cfg(unix)] let mode = { use std::os::unix::fs::PermissionsExt; metadata.permissions().mode() };
            #[cfg(not(unix))] let mode = u32::from(metadata.permissions().readonly());
            let (length, digest) = if metadata.is_dir() { pending.push(path.clone()); (0, [0; 32]) }
                else { assert!(metadata.is_file() && metadata.len() <= 65536); let bytes = std::fs::read(&path).unwrap();
                    (bytes.len() as u64, Sha256::digest(&bytes).into()) };
            found.push((path.strip_prefix(home).unwrap().to_owned(), mode, length, digest));
        }
    }
    found.sort(); found
}
fn snapshot(client: &mut postgres::Client, schema: &str) -> Vec<(String, i64, String)> {
    let rows = client.query("select tablename::text from pg_tables where schemaname=$1 order by tablename", &[&schema]).unwrap();
    assert!(rows.len() <= 128);
    rows.into_iter().map(|row| {
        let name: String = row.get(0); assert!(name.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'));
        let row = client.query_one(&format!("select count(*),md5(coalesce(string_agg(row_to_json(t)::text,E'\\n' order by row_to_json(t)::text collate \"C\"),'')) from \"{schema}\".\"{name}\" t"), &[]).unwrap();
        let count: i64 = row.get(0); assert!(count <= 8192, "fixed fixture unexpectedly grew"); (name, count, row.get(1))
    }).collect()
}
fn private(path: &Path) { #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap(); } }
fn private_file(path: &Path) { #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap(); } }

fn value_text(snapshot: &mut atlas::pg::history_catalog::Snapshot<'_>, value: &atlas::history_identity::Value) -> Option<String> {
    if matches!(value, atlas::history_identity::Value::Null) { return None; }
    let mut cursor = snapshot.value_cursor(value).unwrap(); let mut output = Vec::new();
    for _ in 0..16 {
        let progress = snapshot.read_value(&mut cursor, atlas::pg::history_catalog::ReadBudget::default(), |bytes| {
            assert!(output.len() + bytes.len() <= 4096, "fixed metadata unexpectedly grew"); output.extend_from_slice(bytes); Ok(())
        }).unwrap();
        if progress.complete { return Some(String::from_utf8(output).unwrap()); }
    }
    panic!("fixed metadata did not finish");
}

#[derive(Serialize, Deserialize)]
pub struct ChildContext { pub home: PathBuf, pub group: String, pub http_port: u16, pub schema: String, pub import: [u8; 32], pub schema_oid: u32, pub sqlite_events: PathBuf,
    pub expected_current_nzb: String, pub expected_frozen_nzb: String }

#[cfg(unix)]
static SQLITE_EVENT_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
const SQLITE_MARKER: &[u8] = b"sqlite-auto-extension-v1\n";

/// Remains installed for the whole child lifetime, including late background
/// threads. One owned CLOEXEC FD closes at process exit; no payload is logged.
#[cfg(unix)]
pub fn install_sqlite_observer(path: &Path) {
    use std::io::Write;
    use std::os::fd::IntoRawFd;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path).unwrap(); private_file(path);
    file.write_all(SQLITE_MARKER).unwrap();
    let fd = file.into_raw_fd();
    assert_eq!(SQLITE_EVENT_FD.swap(fd, std::sync::atomic::Ordering::SeqCst), -1);
    // SAFETY: the pinned FFI exposes this exact SQLite callback signature. Its
    // code and event FD remain valid until process exit; no Rust panic crosses C.
    assert_eq!(unsafe { rusqlite::ffi::sqlite3_auto_extension(Some(deny_sqlite)) }, rusqlite::ffi::SQLITE_OK);
}
#[cfg(unix)]
unsafe extern "C" fn deny_sqlite(
    _: *mut rusqlite::ffi::sqlite3, error: *mut *mut std::ffi::c_char, _: *const rusqlite::ffi::sqlite3_api_routines,
) -> std::ffi::c_int {
    if !error.is_null() { unsafe { *error = std::ptr::null_mut(); } }
    let fd = SQLITE_EVENT_FD.load(std::sync::atomic::Ordering::SeqCst);
    loop {
        let written = unsafe { libc::write(fd, b"+".as_ptr().cast(), 1) };
        if written == 1 { return rusqlite::ffi::SQLITE_ERROR; }
        if written == -1 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted { continue; }
        // A failed observer write must never be reported as zero SQLite use.
        unsafe { libc::_exit(120); }
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
pub enum AuditExpectation { Clean, SqliteControl, ConnectControl }

/// Trace the fresh reader process, not source/oracle/bootstrap connections.
/// Failed syscalls are included. A missing tracer/receipt is a test failure,
/// never a skipped no-I/O claim. No shell or environment credentials are logged.
#[cfg(target_os = "linux")]
pub fn audited_child(fixture: &Fixture, group: &str, allow_pg: bool, expectation: AuditExpectation) {
    use std::io::{BufRead, Read};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let http_port = reservation.local_addr().unwrap().port(); drop(reservation);
    assert!(http_port >= 1024);
    let home_path = fixture.home.to_str().unwrap();
    assert!(home_path.is_ascii() && home_path.len() <= 1024 && !home_path.contains(['"', '\\'])
        && !home_path.bytes().any(|byte| byte.is_ascii_control()), "audit paths require unescaped bounded ASCII");
    let context = ChildContext { home: fixture.home.clone(), group: group.into(), http_port,
        schema: fixture.schema.clone(), import: fixture.import, schema_oid: fixture.db.oid.unwrap(),
        sqlite_events: fixture.root.path().join(format!("{group}-capi.events")),
        expected_current_nzb: fixture.expected_current_nzb.clone(), expected_frozen_nzb: fixture.expected_frozen_nzb.clone() };
    let context_path = fixture.root.path().join(format!("{group}-context.json"));
    std::fs::write(&context_path, serde_json::to_vec(&context).unwrap()).unwrap(); private_file(&context_path);
    let trace = fixture.root.path().join(format!("{group}-syscalls.log"));
    let stdout = fixture.root.path().join(format!("{group}-stdout.log"));
    let stderr = fixture.root.path().join(format!("{group}-stderr.log"));
    let output = std::fs::File::create(&stdout).unwrap(); let errors = std::fs::File::create(&stderr).unwrap();
    private_file(&stdout); private_file(&stderr);
    let mut command = Command::new("strace");
    command.args(["-f", "-qq", "-s", "4096", "-e",
        "trace=open,openat,openat2,creat,stat,lstat,fstat,newfstatat,statx,access,faccessat,faccessat2,readlink,readlinkat,rename,renameat,renameat2,unlink,unlinkat,mkdir,mkdirat,rmdir,truncate,ftruncate,socket,connect,bind,listen,accept,accept4,getsockname,getpeername,shutdown",
        "-o"]).arg(&trace).arg("--")
        .arg(std::env::current_exe().unwrap()).args(["--exact", "api::native_pg_readers::reader_http_child", "--nocapture", "--test-threads=1"])
        .env("ATLAS_WEB_READER_FIXTURE", &context_path).env("ATLAS_HOME", &fixture.home)
        .env("ATLAS_SAB_DIR", fixture.root.path().join("sab")).env("ATLAS_NO_KEYRING", "1")
        .stdout(Stdio::from(output)).stderr(Stdio::from(errors));
    for key in ["ATLAS_NNTP_HOST", "ATLAS_NNTP_USER", "ATLAS_NNTP_PASS", "ATLAS_NNTP_PORT", "ATLAS_API_PORT", "ATLAS_API_HOST",
        "ATLAS_PG_TEST_URL", "PGPASSWORD", "PGHOST", "PGPORT", "PGUSER", "PGDATABASE"] { command.env_remove(key); }
    // The owned tracer and all tracees share a fresh process group. Teardown
    // never addresses the test runner, another schema owner or other services.
    unsafe { command.pre_exec(|| {
        if libc::setpgid(0, 0) == -1 { return Err(std::io::Error::last_os_error()); }
        let output = libc::rlimit { rlim_cur: 4 * 1024 * 1024, rlim_max: 4 * 1024 * 1024 };
        if libc::setrlimit(libc::RLIMIT_FSIZE, &output) == -1 { return Err(std::io::Error::last_os_error()); }
        Ok(())
    }); }
    let mut child = command.spawn().expect("explicit Linux syscall audit tool is required");
    let until = Instant::now() + Duration::from_secs(20);
    let (status, resource_rejected) = loop {
        // WNOWAIT deliberately keeps the leader PID reserved until the entire
        // owned group is stopped, even if strace fails before its tracee. This
        // avoids addressing a reused numeric PGID after Child::try_wait reaps.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let observed = unsafe { libc::waitid(libc::P_PID, child.id(), &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT) };
        if observed == -1 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted { continue; }
            unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL); } let _ = child.wait(); panic!("owned tracer status unavailable");
        }
        let exited = unsafe { info.si_pid() } != 0;
        let limit = Instant::now() >= until || std::fs::metadata(&trace).is_ok_and(|meta| meta.len() >= 3 * 1024 * 1024);
        if exited || limit {
            // SAFETY: setpgid established this owned group; WNOWAIT has not
            // released the leader identity. No unrelated process is targeted.
            unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL); }
            let status = child.wait().unwrap(); break (status, limit);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // Preserve independent evidence before any semantic assertion can unwind
    // the temporary fixture. Private settings/source DBs are never copied.
    let evidence = fixture.evidence.join(group); std::fs::create_dir(&evidence).unwrap(); private(&evidence);
    for (source, name) in [(&trace, "syscalls.log"), (&stdout, "stdout.log"), (&stderr, "stderr.log"), (&context.sqlite_events, "capi.events")] {
        if source.exists() {
            let mut input = std::fs::File::open(source).unwrap();
            let target = evidence.join(name); let mut output = std::fs::File::create(&target).unwrap(); private_file(&target);
            let copied = std::io::copy(&mut (&mut input).take(4 * 1024 * 1024 + 1), &mut output).unwrap();
            assert!(copied <= 4 * 1024 * 1024, "fixed evidence exceeded admission");
        }
    }
    let binding = evidence.join("binding.json");
    std::fs::write(&binding, serde_json::to_vec(&serde_json::json!({"child":context,"allow_pg":allow_pg,"pg_endpoint":fixture.endpoint})).unwrap()).unwrap(); private_file(&binding);
    assert!(!resource_rejected, "bounded reader child time/output admission exceeded; private evidence preserved");
    let mut sqlite = 0u64; let mut foreign = 0u64; let mut datagrams = 0u64; let mut calls = 0usize;
    let input = std::fs::File::open(&trace).expect("missing independent syscall trace");
    assert!(input.metadata().unwrap().len() <= 4 * 1024 * 1024, "fixed trace exceeded admission");
    for line in std::io::BufReader::new(input).lines() {
        let line = line.unwrap(); assert!(line.len() <= 16 * 1024);
        if line.contains("socket(") && (line.contains("SOCK_DGRAM") || line.contains("SOCK_RAW") || line.contains("AF_PACKET")) {
            datagrams += 1;
        }
        if line.contains("connect(") {
            calls += 1;
            let http = tcp_connect(&line, "127.0.0.1", http_port);
            let pg = allow_pg && match &fixture.endpoint {
                Endpoint::Tcp { host, port } => tcp_connect(&line, host, *port),
                Endpoint::Unix { path } => line.contains("sa_family=AF_UNIX") && line.contains(&format!("sun_path=\"{path}\"")),
            };
            if !http && !pg { foreign += 1; }
        }
        // Match attempted SQLite family access, including ENOENT/EISDIR. All
        // fixture paths are ASCII; non-ASCII paths require a qualified decoder.
        for quoted in line.split('"').skip(1).step_by(2) {
            if quoted.contains(".db") || quoted.contains(".sqlite") {
                sqlite += 1;
            }
        }
    }
    match expectation {
        AuditExpectation::SqliteControl => assert!(sqlite > 0 && calls == 0, "observer must catch a real failed SQLite attempt"),
        AuditExpectation::ConnectControl => assert!(foreign > 0 && sqlite == 0, "observer must catch a real owned non-allowlisted connection"),
        AuditExpectation::Clean => assert!(calls > 0 && sqlite == 0 && foreign == 0 && datagrams == 0,
            "ineffective or violated audit: calls={calls}, sqlite={sqlite}, foreign={foreign}, datagrams={datagrams}"),
    }
    let mut diagnostic = String::new(); std::fs::File::open(stderr).unwrap().take(64 * 1024).read_to_string(&mut diagnostic).unwrap();
    let mut events = Vec::new(); std::fs::File::open(&context.sqlite_events).expect("missing SQLite observer").take(4 * 1024 * 1024 + 1).read_to_end(&mut events).unwrap();
    assert!(events.len() <= 4 * 1024 * 1024 && events.starts_with(SQLITE_MARKER));
    let attempts = &events[SQLITE_MARKER.len()..]; assert!(attempts.iter().all(|byte| *byte == b'+'));
    if matches!(expectation, AuditExpectation::SqliteControl) { assert!(!attempts.is_empty(), "must detect real in-memory SQLite initialization"); }
    else { assert!(attempts.is_empty(), "native reader attempted in-memory/initialized SQLite"); }
    assert!(status.success(), "reader={group}, status={status}, sqlite={sqlite}, foreign={foreign}, datagrams={datagrams}\n{diagnostic}");
}

#[cfg(target_os = "linux")]
fn tcp_connect(line: &str, host: &str, port: u16) -> bool {
    (line.contains("sa_family=AF_INET,") && line.contains(&format!("sin_port=htons({port})")) && line.contains(&format!("inet_addr(\"{host}\")")))
        || (line.contains("sa_family=AF_INET6,") && line.contains(&format!("sin6_port=htons({port})")) && line.contains(&format!("\"{host}\"")))
}
