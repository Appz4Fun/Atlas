//! Synthetic consolidated captures shared by conversion integration tests.
use atlas::history_source::{CatalogCounts, FileManifest, Manifest, SourceKind};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
pub fn manifest(root: &Path) -> std::path::PathBuf {
    let mut files = Vec::new();
    for (id, kind, sql) in [
        (
            1,
            SourceKind::Current,
            "create table releases(id integer primary key,name text,group_name text,poster text,posted_date text,size integer,complete integer,parts integer,file_total integer,display_name text,is_obfuscated integer,name_source integer);create table collections(key integer primary key,release_id integer);insert into releases values(3,'Original.Title','test.group',null,'date',1,1,1,1,'',0,40);insert into collections values(0,3),(-7,899)",
        ),
        (2, SourceKind::Main, ""),
        (
            3,
            SourceKind::Frozen,
            "create table frozen_releases(id integer primary key,name text,group_name text,display_name text,poster text,posted_date text,size integer,parts integer,complete integer,category integer,reason text not null,frozen_at integer not null,payload blob not null);create table frozen_collections(key integer primary key,release_id integer not null) without rowid",
        ),
    ] {
        let path = root.join(format!("source-{id}.db"));
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch("create table meta(key text primary key,value integer)").unwrap();
        c.execute_batch(sql).unwrap();
        for key in atlas::history_source::fact_names(kind) {
            c.execute(
                "insert into meta values(?1,?2)",
                rusqlite::params![key, if *key == "next_seq" { 900 } else { 17 }],
            )
            .unwrap();
        }
        let schema_digest = atlas::history_source::schema_digest(&c).unwrap();
        drop(c);
        let bytes = std::fs::read(&path).unwrap();
        files.push(FileManifest {
            source_id: id,
            kind,
            shard: (kind != SourceKind::Main).then_some(0),
            paired_current: (kind == SourceKind::Frozen).then_some(1),
            path,
            byte_length: bytes.len() as u64,
            digest: Sha256::digest(&bytes).into(),
            schema_digest,
            catalog_counts: (kind != SourceKind::Main).then_some(CatalogCounts {
                releases: u64::from(kind == SourceKind::Current),
                collections: if kind == SourceKind::Current { 2 } else { 0 },
            }),
        });
    }
    let manifest = Manifest {
        version: 1,
        checkpoint_id: "synthetic-capture".into(),
        source_uuid: "synthetic-lineage".into(),
        shard_count: 1,
        reserved_next_id: 900,
        archive_manifest: [3; 32],
        main_receipt: [4; 32],
        ext_receipt: [5; 32],
        grouping_facts: b"synthetic grouping provenance".to_vec(),
        immutable_checkpoint: true,
        files,
    };
    let path = root.join("manifest.json");
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    path
}
pub struct Destination {
    pub client: postgres::Client,
    pub name: String,
    pub expected: atlas::pg::history_import::ExpectedDestination,
    pub url: String,
}
impl Drop for Destination {
    fn drop(&mut self) {
        let _ = self.client.batch_execute(&format!("drop schema if exists \"{}\" cascade", self.name));
    }
}
pub fn destination(root: &Path) -> Option<(Destination, std::path::PathBuf)> {
    use atlas::pg::history_import::{ExpectedDestination, ExpectedTransport};
    let url = std::env::var("ATLAS_PG_TEST_URL").ok().filter(|s| !s.is_empty())?;
    let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let row=client.query_one("select current_database(),current_user,current_setting('server_version_num')::integer,host(inet_server_addr()),inet_server_port()",&[]).unwrap();
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let name =
        format!("atlas_stage_cli_{}_{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let expected = ExpectedDestination {
        database: row.get(0),
        schema: name.clone(),
        role: row.get(1),
        owner: row.get(1),
        server_version: row.get(2),
        server_address: row.get(3),
        server_port: row.get(4),
        transport: if row.get::<_, Option<String>>(3).is_some() {
            ExpectedTransport::PlaintextTcp
        } else {
            ExpectedTransport::UnixSocket
        },
        staging_prefix: "atlas_stage_".into(),
    };
    let path = root.join("destination.json");
    std::fs::write(&path, serde_json::to_vec(&expected).unwrap()).unwrap();
    Some((Destination { client, name, expected, url }, path))
}

pub fn refresh(manifest: &mut Manifest) {
    for file in &mut manifest.files {
        let c = rusqlite::Connection::open(&file.path).unwrap();
        file.schema_digest = atlas::history_source::schema_digest(&c).unwrap();
        if file.kind != SourceKind::Main {
            let (releases, collections) = if file.kind == SourceKind::Current {
                ("releases", "collections")
            } else {
                ("frozen_releases", "frozen_collections")
            };
            file.catalog_counts = Some(CatalogCounts {
                releases: c.query_row(&format!("select count(*) from {releases}"), [], |r| r.get::<_, i64>(0)).unwrap()
                    as u64,
                collections: c
                    .query_row(&format!("select count(*) from {collections}"), [], |r| r.get::<_, i64>(0))
                    .unwrap() as u64,
            });
        }
        drop(c);
        let mut input = std::fs::File::open(&file.path).unwrap();
        file.byte_length = input.metadata().unwrap().len();
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            let n = input.read(&mut buffer).unwrap();
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        file.digest = hash.finalize().into();
    }
}
impl Destination {
    pub fn owner(&self, spool: &Path, quota: u64) -> atlas::pg::history_import::ImportOwner {
        atlas::pg::history_import::ImportOwner::connect(
            atlas::pg::StorageConfig {
                backend: atlas::pg::Backend::Postgres,
                url: self.url.clone(),
                schema: self.name.clone(),
                max_connections: 1,
                allow_plaintext: true,
            },
            self.expected.clone(),
            spool.into(),
            quota,
        )
        .unwrap()
    }
}
