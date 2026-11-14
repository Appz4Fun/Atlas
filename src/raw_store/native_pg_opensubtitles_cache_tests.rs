//! The configured natural-key cache API, isolated from providers and SQLite.

use crate as atlas;

use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use atlas::probe::opensubtitles::Feature;

const POSITIVE: &str = "abcdef0123456789";
const NEGATIVE: &str = "abcdef012345678a";
const DIFFERENT_CASE: &str = "ABCDEF0123456789";
const ABSENT: &str = "abcdef012345678b";
const RAW_SENTINEL: &[u8] = b"SQLite must not be opened by this native cache fixture";

// These adapters require the checked public API. An Option/void return cannot
// satisfy the native error contract or compile through this fixture.
trait CheckedRead {
    fn checked(self) -> Result<Option<Option<Feature>>, ()>;
}
impl<E> CheckedRead for Result<Option<Option<Feature>>, E> {
    fn checked(self) -> Result<Option<Option<Feature>>, ()> {
        self.map_err(|_| ())
    }
}
trait CheckedWrite {
    fn checked(self) -> Result<(), ()>;
}
impl<E> CheckedWrite for Result<(), E> {
    fn checked(self) -> Result<(), ()> {
        self.map_err(|_| ())
    }
}

trait ErrorText {
    fn error_text(&self) -> Option<String>;
}
impl<T, E: std::fmt::Display> ErrorText for Result<T, E> {
    fn error_text(&self) -> Option<String> {
        self.as_ref().err().map(ToString::to_string)
    }
}

fn feature() -> Feature {
    Feature {
        imdb_id: Some(101),
        tmdb_id: Some(202),
        kind: Some("Movie".into()),
        title: Some("Example Film".into()),
        year: Some(2001),
        season: None,
        episode: None,
        release_name: Some("Example.Film.2001".into()),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct CacheRow {
    key: String,
    feature: Feature,
    raw: Option<Vec<u8>>,
    checked_at: i64,
}

const ROW_COLUMNS: &str = "moviehash,imdb_id,tmdb_id,kind,title,year,season,episode,release_name,raw_json_zst,checked_at";

// Independent seed/upsert vectors retain the former field/null/key contract
// without compiling a second database engine into the native test graph.
fn refreshed_rows() -> Vec<CacheRow> {
    let mut positive = feature();
    positive.tmdb_id = Some(404);
    let mut rows = vec![
        CacheRow { key: POSITIVE.into(), feature: positive, raw: None, checked_at: 0 },
        CacheRow { key: NEGATIVE.into(), feature: Feature::default(), raw: None, checked_at: 1 },
        CacheRow { key: DIFFERENT_CASE.into(), feature: Feature {
            imdb_id: Some(303), title: Some("Case distinct".into()), ..Feature::default()
        }, raw: None, checked_at: 1 },
    ];
    rows.sort_by(|left, right| left.key.cmp(&right.key));
    rows
}

struct Schema {
    client: postgres::Client,
    name: String,
    url: String,
}
impl Schema {
    fn new() -> Option<Self> {
        let url = std::env::var("ATLAS_PG_TEST_URL").ok().filter(|s| !s.is_empty())?;
        let mut client = postgres::Client::connect(&url, postgres::NoTls).expect("owned test destination");
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let name = format!("t_os_cache_{}_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed), nonce);
        client.batch_execute(&format!("create schema \"{name}\"")).expect("new owned schema");
        let mut schema = Self { client, name, url };
        atlas::pg::migrate::run(&mut schema.client, &schema.name).expect("owned schema migration");
        schema.client.batch_execute(&format!("set search_path to \"{}\"", schema.name)).unwrap();
        Some(schema)
    }

    fn seed(&mut self) {
        let f = feature();
        let raw: &[u8] = b"preserved raw fixture";
        self.client
            .execute(
                &format!(
                    "insert into {}.ext_opensubtitles
                     (moviehash,imdb_id,tmdb_id,kind,title,year,release_name,raw_json_zst,checked_at)
                     values ($1,$2,$3,$4,$5,$6,$7,$8,1)",
                    self.name
                ),
                &[&POSITIVE, &f.imdb_id, &f.tmdb_id, &f.kind, &f.title, &f.year, &f.release_name, &raw],
            )
            .unwrap();
        self.client
            .execute(
                &format!("insert into {}.ext_opensubtitles (moviehash,checked_at) values ($1,1)", self.name),
                &[&NEGATIVE],
            )
            .unwrap();
        self.client
            .execute(
                &format!(
                    "insert into {}.ext_opensubtitles (moviehash,imdb_id,title,checked_at) values ($1,303,'Case distinct',1)",
                    self.name
                ),
                &[&DIFFERENT_CASE],
            )
            .unwrap();
    }

    fn rows(&mut self) -> Vec<CacheRow> {
        self.client.query(&format!("select {ROW_COLUMNS} from {}.ext_opensubtitles order by moviehash", self.name), &[])
            .unwrap().into_iter().map(|row| CacheRow {
                key: row.get(0),
                feature: Feature {
                    imdb_id: row.get(1), tmdb_id: row.get(2), kind: row.get(3), title: row.get(4), year: row.get(5),
                    season: row.get(6), episode: row.get(7), release_name: row.get(8),
                },
                raw: row.get(9), checked_at: row.get(10),
            }).collect()
    }
}
impl Drop for Schema {
    fn drop(&mut self) {
        let _ = self.client.batch_execute(&format!("drop schema \"{}\" cascade", self.name));
    }
}

fn write_config(home: &Path, url: &str, schema: &str, http_port: u16) {
    let mut config = toml_edit::DocumentMut::new();
    config["storage"]["backend"] = toml_edit::value("postgres");
    config["storage"]["url"] = toml_edit::value(url);
    config["storage"]["schema"] = toml_edit::value(schema);
    config["storage"]["max_connections"] = toml_edit::value(1);
    config["storage"]["allow_plaintext"] = toml_edit::value(true);
    config["media"]["tvmaze"]["enabled"] = toml_edit::value(false);
    config["media"]["allocine"]["enabled"] = toml_edit::value(false);
    config["media"]["details_per_minute"] = toml_edit::value(0);
    config["media"]["opensubtitles"]["api_key"] = toml_edit::value("");
    config["media"]["opensubtitles"]["base_url"] = toml_edit::value(format!("http://127.0.0.1:{http_port}"));
    config["postprocess"]["enabled"] = toml_edit::value(false);
    config["predb"]["enabled"] = toml_edit::value(false);
    config["spots"]["enabled"] = toml_edit::value(false);
    std::fs::write(home.join("config.toml"), config.to_string()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(home.join("config.toml"), std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn child(url: &str, schema: &str, action: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let http = TcpListener::bind("127.0.0.1:0").unwrap();
    http.set_nonblocking(true).unwrap();
    write_config(home.path(), url, schema, http.local_addr().unwrap().port());
    std::fs::write(home.path().join("atlas.raw.db"), RAW_SENTINEL).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "raw_store::native_pg_opensubtitles_cache_tests::configured_cache_child", "--nocapture"])
        .env("ATLAS_HOME", home.path())
        .env("ATLAS_SAB_DIR", home.path().join("no-sab"))
        .env("ATLAS_NO_KEYRING", "1")
        .env("ATLAS_CACHE_TEST_ACTION", action)
        .env_remove("ATLAS_PG_TEST_URL")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    let term=matches!(action,"net-stop"|"net-stop-header"|"net-stop-rerun"|"net-stop-owner"|"net-stop-receipt"|"net-stop-outage");let mut term_sent=false;
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if term && !term_sent && home.path().join("native-term-ready").exists() {
            // The retained Child owns this exact waiting PID; its marker is
            // written only after the original NET begin and worker handshake.
            assert_eq!(unsafe {libc::kill(child.id() as libc::pid_t,libc::SIGTERM)},0);term_sent=true;
        }
        if Instant::now() >= until {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("owned cache child exceeded its deadline");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().unwrap();
    assert!(!term || term_sent,"stop fixture never received its real TERM");
    assert!(
        output.status.success(),
        "configured cache child failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read(home.path().join("atlas.raw.db")).unwrap(), RAW_SENTINEL);
    assert!(matches!(http.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock), "cache called HTTP");
    for entry in std::fs::read_dir(home.path()).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        assert!(
            matches!(name.as_str(), "config.toml" | "atlas.raw.db"),
            "cache created unexpected files: {name}"
        );
    }
    home
}

// These are closed worker fixtures in the same configured child lane. The
// parent retains the URL and schema; workers receive only the real Runtime.
pub(crate) enum WorkerCase { ArchiveCategory, Category, Held, PredbOnly, FeedHeld, Factory, FanoutFence, AssetReuse, AssetStale, AssetDrain, AssetMissing, AssetUnselected, AssetCorrupt, AssetWaitMissing, AssetWaitUnselected, HeaderOnly }

pub(crate) fn native_worker_parent(case:WorkerCase) {
    let Some(mut schema)=Schema::new() else { return };
    schema.seed();
    let kept=schema.rows();
    let action=match case {WorkerCase::ArchiveCategory=>"worker-archive-category",WorkerCase::Category=>"worker-loop",WorkerCase::Held=>"worker-held",WorkerCase::PredbOnly=>"worker-predb",WorkerCase::FeedHeld=>"worker-feed",WorkerCase::Factory=>"worker-factory",WorkerCase::FanoutFence=>"worker-fanout-fence",WorkerCase::AssetReuse=>"worker-asset-reuse",WorkerCase::AssetStale=>"worker-asset-stale",WorkerCase::AssetDrain=>"worker-asset-drain",WorkerCase::AssetMissing=>"worker-asset-missing",WorkerCase::AssetUnselected=>"worker-asset-unselected",WorkerCase::AssetCorrupt=>"worker-asset-corrupt",WorkerCase::AssetWaitMissing=>"worker-asset-wait-missing",WorkerCase::AssetWaitUnselected=>"worker-asset-wait-unselected",WorkerCase::HeaderOnly=>"worker-header-only"};
    child(&schema.url,&schema.name,action);
    assert_eq!(schema.rows(),kept,"native completion changed retained cache facts");
    assert_eq!(schema.client.query_one("select count(*) from releases",&[]).unwrap().get::<_,i64>(0),if matches!(case,WorkerCase::PredbOnly|WorkerCase::Factory|WorkerCase::HeaderOnly) {0}else{1});
    if matches!(case,WorkerCase::PredbOnly|WorkerCase::Factory|WorkerCase::HeaderOnly) {
        assert_eq!(schema.client.query_one("select count(*) from pp_state",&[]).unwrap().get::<_,i64>(0),0);
        assert_eq!(schema.client.query_one("select count(*) from pp_walks",&[]).unwrap().get::<_,i64>(0),0);
    }
    if matches!(case,WorkerCase::Held) {
        assert_eq!(schema.client.query_one("select count(*) from native_operations",&[]).unwrap().get::<_,i64>(0),0);
        assert_eq!(schema.client.query_one("select count(*) from pp_queue",&[]).unwrap().get::<_,i64>(0),1);
    }
}

pub(crate) fn native_details_parent() {
    let mut schema=Schema::new().expect("explicit isolated PostgreSQL required for details lifecycle");
    schema.seed();let kept=schema.rows();child(&schema.url,&schema.name,"worker-details-held");
    assert_eq!(schema.rows(),kept);
    assert_eq!(schema.client.query_one("select count(*) from ext_media_details",&[]).unwrap().get::<_,i64>(0),0);
    assert_eq!(schema.client.query_one("select count(*) from pp_state",&[]).unwrap().get::<_,i64>(0),0);
}

#[test]
fn configured_postgres_cache_reads_positive_negative_missing_and_case_distinct_keys() {
    let Some(mut schema) = Schema::new() else { return };
    schema.seed();
    child(&schema.url, &schema.name, "read");
}

#[test]
fn configured_postgres_upserts_preserve_fields_and_apply_raw_json_contract() {
    let Some(mut schema) = Schema::new() else { return };
    schema.seed();
    let began = chrono::Utc::now().timestamp();
    child(&schema.url, &schema.name, "write");
    let row = schema
        .client
        .query_one(
            &format!(
                "select imdb_id,tmdb_id,kind,title,year,season,episode,release_name,raw_json_zst,checked_at
                 from {}.ext_opensubtitles where moviehash=$1",
                schema.name
            ),
            &[&POSITIVE],
        )
        .unwrap();
    assert_eq!(row.get::<_, Option<i64>>(0), Some(101));
    assert_eq!(row.get::<_, Option<i64>>(1), Some(404));
    assert_eq!(row.get::<_, Option<String>>(2).as_deref(), Some("Movie"));
    assert_eq!(row.get::<_, Option<String>>(3).as_deref(), Some("Example Film"));
    assert_eq!(row.get::<_, Option<i64>>(4), Some(2001));
    assert_eq!(row.get::<_, Option<i64>>(5), None);
    assert_eq!(row.get::<_, Option<i64>>(6), None);
    assert_eq!(row.get::<_, Option<String>>(7).as_deref(), Some("Example.Film.2001"));
    assert_eq!(row.get::<_, Option<Vec<u8>>>(8), None);
    assert!(row.get::<_, i64>(9) >= began);
}

#[test]
fn native_cache_matches_fixed_fields_nulls_keys_and_refresh_time() {
    let Some(mut schema) = Schema::new() else { return };
    schema.seed();
    let began = chrono::Utc::now().timestamp();
    child(&schema.url, &schema.name, "write");
    let expected = refreshed_rows();
    let mut actual = schema.rows();
    let finished = chrono::Utc::now().timestamp();
    let updated = actual.iter_mut().find(|row| row.key == POSITIVE).unwrap();
    assert!((began..=finished).contains(&updated.checked_at));
    updated.checked_at = 0;
    assert_eq!(actual, expected);
}

#[test]
fn mutated_cache_gets_a_fresh_ext_dump_digest_and_restores_exact_rows() {
    let Some(mut source) = Schema::new() else { return };
    source.seed();
    let Some(mut target) = Schema::new() else { return };
    let dumps = tempfile::tempdir().unwrap();
    let before = atlas::pg::ext::dump(&mut source.client, &dumps.path().join("before"), 2, false).unwrap();
    let original_manifest = std::fs::read(before.join("manifest.json")).unwrap();
    let before_manifest: atlas::pg::ext::Manifest = serde_json::from_slice(&original_manifest).unwrap();
    child(&source.url, &source.name, "write");
    let after = atlas::pg::ext::dump(&mut source.client, &dumps.path().join("after"), 2, false).unwrap();
    let after_manifest: atlas::pg::ext::Manifest = serde_json::from_slice(&std::fs::read(after.join("manifest.json")).unwrap()).unwrap();
    let old_cache = before_manifest.tables.iter().find(|row| row.table == "ext_opensubtitles").unwrap();
    let new_cache = after_manifest.tables.iter().find(|row| row.table == "ext_opensubtitles").unwrap();
    assert_eq!(new_cache.rows, 3);
    assert!(new_cache.sha256.as_ref().is_some_and(|hash| hash.len() == 64));
    assert_ne!(new_cache.sha256, old_cache.sha256, "dump still describes the cache before its mutation");
    assert_eq!(std::fs::read(before.join("manifest.json")).unwrap(), original_manifest);
    let restored = atlas::pg::ext::restore(&mut target.client, &after, false).unwrap();
    assert_eq!(restored.tables.iter().find(|row| row.table == "ext_opensubtitles").unwrap(), new_cache);
    assert_eq!(target.rows(), source.rows());
}

#[test]
fn configured_postgres_unavailable_cache_is_an_error_without_sqlite_or_http_fallback() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let url = format!("host=127.0.0.1 port={port} user=example dbname=example connect_timeout=1");
    child(&url, "cache_fixture", "unavailable");
}

#[test]
fn configured_cache_supports_the_standard_stage_thread_and_rejects_entered_tokio() {
    let Some(mut schema) = Schema::new() else { return };
    schema.seed();
    child(&schema.url, &schema.name, "context");
}

#[test]
fn configured_postgres_pin_rejects_missing_malformed_or_changed_configuration() {
    let Some(mut schema) = Schema::new() else { return };
    schema.seed();
    for action in ["config-removed", "config-malformed", "config-sqlite"] {
        child(&schema.url, &schema.name, action);
    }
}

#[test]
fn invalid_initial_cache_configuration_is_an_error_without_echoing_its_contents() {
    child("host=127.0.0.1 port=1 user=example dbname=example connect_timeout=1", "cache_fixture", "config-initial");
}

#[test]
fn configured_costly_cache_roundtrips_on_owner_and_rejects_ended_attempt() {
    let Some(schema) = Schema::new() else { return };
    child(&schema.url, &schema.name, "costly");
}

#[test]
fn native_database_runner_keeps_default_freeze_releases_and_cache_without_sqlite() {
    let Some(mut schema)=Schema::new() else {return};schema.seed();let before=schema.rows();
    child(&schema.url,&schema.name,"database-runner");
    assert_eq!(schema.rows(),before,"native completion changed the kept external cache");
    let count:i64=schema.client.query_one(&format!("select count(*) from {}.releases",schema.name),&[]).unwrap().get(0);
    assert_eq!(count,1,"native completion must retain its catalog row in PostgreSQL");
}

#[test]
fn native_network_runner_commits_a_real_body_crc_step_without_sqlite_or_http() {
    let Some(mut schema)=Schema::new() else {return};schema.seed();let before=schema.rows();
    child(&schema.url,&schema.name,"network-runner");assert_eq!(schema.rows(),before);
}

fn net_stop_parent(action:&str) {let Some(mut schema)=Schema::new() else {return};schema.seed();let before=schema.rows();child(&schema.url,&schema.name,action);assert_eq!(schema.rows(),before);}
#[test]
fn native_net_intentional_term_restores_before_budget_after_real_worker_join() {net_stop_parent("net-stop");}
#[test]
fn native_net_stop_refuses_changed_header() {net_stop_parent("net-stop-header");}
#[test]
fn native_net_stop_refuses_changed_rerun() {net_stop_parent("net-stop-rerun");}
#[test]
fn native_net_stop_refuses_a_different_owner() {net_stop_parent("net-stop-owner");}
#[test]
fn native_net_stop_refuses_a_committed_operation_receipt() {net_stop_parent("net-stop-receipt");}
#[test]
fn native_net_stop_outage_preserves_spent_attempts() {net_stop_parent("net-stop-outage");}

fn costly_roundtrip() {
    use atlas::raw_store::*;
    let (name, group, content) = ("Kept", "own.group", &[1u8, 2][..]);
    assert!(get_probe_checked(name, group, content, 4, true, 1).unwrap().is_none());
    assert!(!has_nfo_password_record_checked(name, group, content).unwrap());
    put_nfo_password_checked(name, group, content, "").unwrap();
    assert!(has_nfo_password_record_checked(name, group, content).unwrap());
    assert_eq!(get_nfo_password_checked(name, group, content).unwrap(), None);
    for key in ["atlas.nfo-name.v1:4b657074", "kept", "é\0nzb"] {
        put_nfo_password_checked(key, group, content, "distinct").unwrap();
        assert_eq!(get_nfo_password_checked(key, group, content).unwrap().as_deref(),Some("distinct"));
    }
    assert_eq!(get_nfo_password_checked(name,group,content).unwrap(),None);
    put_nested_nzb_password_checked(name, group, content, "nested-secret").unwrap();
    assert_eq!(get_nested_nzb_password_checked(name, group, content).unwrap().as_deref(), Some("nested-secret"));
    assert_eq!(get_nfo_password_checked(name, group, content).unwrap(), None);
    assert!(has_nested_nzb_password_record_checked(name, group, content).unwrap());
    put_nested_nzb_password_checked(name, group, content, "").unwrap();
    assert!(has_nested_nzb_password_record_checked(name, group, content).unwrap());
    assert_eq!(get_nested_nzb_password_checked(name, group, content).unwrap(), None);
    let probe = ProbeRecord {name, group_name:group, content_id:content, crc32:Some(19), agent:Some("tool"),
        info:r#"{"opaque":[1,2]}"#, moviehash:None, parts:4, complete:true, config_stamp:7,
        nfo_password:Some(""), cover:Some(("image/jpeg","digest"))};
    put_probe_checked(&probe).unwrap();
    let saved = get_probe_checked(name, "cross.group", content, 4, true, 7).unwrap().unwrap();
    assert_eq!(saved.info, probe.info);
    assert_eq!(saved.nfo_password.as_deref(), Some(""));
    assert_eq!(saved.cover, Some(("image/jpeg".into(),"digest".into())));
    assert!(get_probe_checked(name, group, &[9], 4, true, 7).unwrap().is_none());
    let still = SavedStill {position:25, at_ms:None, sha256:"digest".into(), width:2, height:3, phash:Some(-7)};
    put_stills_checked(name, group, content, &[still.clone()]).unwrap();
    assert_eq!(get_stills_checked(name, group, content).unwrap(), vec![still]);
    let print = SavedPrint {position:50, at_ms:17, items:vec![0,255,0,1]};
    put_audio_prints_checked(name, group, content, &[print.clone()]).unwrap();
    assert_eq!(get_audio_prints_checked(name, group, content).unwrap(), vec![print]);
    let files = [atlas::archive::InnerFile::new("inner".into(), Some(31))];
    let archive = ArchiveRecord {name, group_name:group, content_id:content, source:2, files:&files, password:Some(0),
        encryption:None, names_hidden:false, password_check:None, parts:4, complete:true, listing_complete:true,
        listing_damaged:false, content_fp:Some(&[3]), max_names_bytes:1024, keys:Default::default(), key_salt:None, facts:1};
    put_archive_checked(&archive).unwrap();
    assert_eq!(get_archive_checked(name,group,content,4,true).unwrap().unwrap().files[0].name, "inner");
    assert_eq!(get_archive_files_checked(name,group,content,2).unwrap()[0].name, "inner");
    let inventory = atlas::par2::Inventory {files:vec![atlas::par2::Par2File {set_id:[1;16],file_id:[2;16],md5:[3;16],
        md5_16k:[4;16],size:31,name:"inner".into(),bound:true,crc32:Some(19)}],complete:true,..Default::default()};
    put_par2_files_checked(name,group,content,&inventory).unwrap();
    assert_eq!(get_par2_files_checked(name,group,content).unwrap().unwrap().files, inventory.files);
    let posted = [2;21];
    put_par2_checked(&Par2Record {name, group_name:group, content_id:content,covers_all:true,posted:Some(&posted),
        sizes:Some((1024,3)),parts:4,complete:true,content_fp:None,inventory:Some(&inventory)}).unwrap();
    assert_eq!(get_par2_checked(name,group,content,4,true).unwrap().unwrap().posted, Some(posted.to_vec()));
    let volume = atlas::archive::rev::Volume {index:0,size:31,crc32:19};
    put_rev_volumes_checked(name,group,content,&[volume]).unwrap();
    assert_eq!(get_rev_volumes_checked(name,group,content).unwrap(), Some(vec![volume]));
}

fn costly_errors() {
    use atlas::raw_store::*;
    let (n,g,c) = ("Kept", "own.group", &[1u8,2][..]);
    assert!(get_probe_checked(n,g,c,4,true,7).is_err());
    assert!(get_nfo_password_checked(n,g,c).is_err());
    assert!(has_nfo_password_record_checked(n,g,c).is_err());
    assert!(get_nested_nzb_password_checked(n,g,c).is_err());
    assert!(has_nested_nzb_password_record_checked(n,g,c).is_err());
    assert!(get_stills_checked(n,g,c).is_err());
    assert!(get_audio_prints_checked(n,g,c).is_err());
    assert!(get_archive_checked(n,g,c,4,true).is_err());
    assert!(get_archive_files_checked(n,g,c,2).is_err());
    assert!(get_par2_checked(n,g,c,4,true).is_err());
    assert!(get_par2_files_checked(n,g,c).is_err());
    assert!(get_rev_volumes_checked(n,g,c).is_err());
    assert!(put_nfo_password_checked(n,g,c,"").is_err());
    assert!(put_nested_nzb_password_checked(n,g,c,"").is_err());
    assert!(put_stills_checked(n,g,c,&[]).is_err());
    assert!(put_audio_prints_checked(n,g,c,&[]).is_err());
    assert!(put_par2_files_checked(n,g,c,&Default::default()).is_err());
    assert!(put_rev_volumes_checked(n,g,c,&[]).is_err());
    assert!(put_probe_checked(&ProbeRecord {name:n,group_name:g,content_id:c,crc32:None,agent:None,info:"{}",
        moviehash:None,parts:4,complete:true,config_stamp:7,nfo_password:None,cover:None}).is_err());
    assert!(put_archive_checked(&ArchiveRecord {name:n,group_name:g,content_id:c,source:2,files:&[],password:None,
        encryption:None,names_hidden:false,password_check:None,parts:4,complete:true,listing_complete:true,
        listing_damaged:false,content_fp:None,max_names_bytes:1024,keys:Default::default(),key_salt:None,facts:1}).is_err());
    assert!(put_par2_checked(&Par2Record {name:n,group_name:g,content_id:c,covers_all:false,posted:None,
        sizes:None,parts:4,complete:true,content_fp:None,inventory:None}).is_err());
    // Even empty identities must validate the active attempt before becoming a genuine miss.
    assert!(get_probe_checked(n,g,&[],4,true,7).is_err());
}

pub(crate) fn open_test_native_owner() -> anyhow::Result<crate::pg::indexer_runtime::Runtime> {
    let storage = crate::config::checked_storage_config().map_err(|_| anyhow::anyhow!("fixture storage configuration"))?;
    let mut config = crate::config::Config::default();
    config.postprocess.enabled = false;
    config.predb.enabled = false;
    config.spots.enabled = false;
    config.inferred_parts = false;
    config.media.tvmaze.enabled = false;
    config.media.allocine.enabled = false;
    config.media.details_per_minute = 0;
    let mut headers = crate::config::HeaderStoreConfig::default();
    headers.enabled = true;
    crate::pg::indexer_runtime::Runtime::open(&config, storage, headers, &[], &mut None)
}

fn assert_cached_positive() {
    assert_eq!(atlas::raw_store::get_opensubtitles(POSITIVE).checked().unwrap(), Some(Some(feature())));
}

fn assert_cache_error() {
    assert!(atlas::raw_store::get_opensubtitles(ABSENT).checked().is_err(), "unavailable cache became a miss");
    assert!(atlas::raw_store::put_opensubtitles(ABSENT, None).checked().is_err(), "unavailable write succeeded");
}

#[test]
#[ignore = "only the fixture parent may execute this isolated configured API child"]
fn configured_cache_child() {
    let action = std::env::var("ATLAS_CACHE_TEST_ACTION").unwrap();
    let native = if !matches!(action.as_str(), "unavailable" | "config-initial")
        && crate::config::checked_storage_config().unwrap().is_postgres() {
        Some(open_test_native_owner().expect("canonical owned cache fixture"))
    } else { None };
    let scope = native.as_ref().map(|native| native.db.bind_native_cache_scope().unwrap());
    match action.as_str() {
        "read" => {
            // Production's existing processing lanes are standard threads,
            // rather than Tokio spawn_blocking tasks with an entered Handle.
            let db = native.as_ref().unwrap().db.clone();
            std::thread::Builder::new().name("cache-stage-fixture".into()).spawn(move || {
                let _scope = db.bind_native_cache_scope().unwrap();
                assert_cached_positive();
                assert_eq!(atlas::raw_store::get_opensubtitles(NEGATIVE).checked().unwrap(), Some(None));
                assert_eq!(atlas::raw_store::get_opensubtitles(ABSENT).checked().unwrap(), None);
                let distinct = atlas::raw_store::get_opensubtitles(DIFFERENT_CASE).checked().unwrap().unwrap().unwrap();
                assert_eq!(distinct.imdb_id, Some(303));
                assert_eq!(distinct.title.as_deref(), Some("Case distinct"));
                assert_eq!(distinct.tmdb_id, None);
            }).unwrap().join().unwrap();
        }
        "write" => {
            atlas::raw_store::put_opensubtitles(POSITIVE, Some(&Feature { tmdb_id: Some(404), ..Default::default() }))
                .checked()
                .unwrap();
            atlas::raw_store::put_opensubtitles(POSITIVE, None).checked().unwrap();
        }
        "costly" => {
            costly_roundtrip();
            native.as_ref().unwrap().owner.close();
            let error = atlas::raw_store::get_probe_checked("Kept","own.group",&[1,2],4,true,7).unwrap_err();
            assert!(atlas::pg::outage::is_attempt_ended(&error), "ended owner cause was erased");
            assert_eq!(atlas::pg::outage::classify(&error),atlas::pg::outage::Failure::Unavailable);
            costly_errors();
        }
        action @ ("worker-asset-reuse"|"worker-asset-stale"|"worker-asset-drain"|"worker-asset-missing"|"worker-asset-unselected"|"worker-asset-corrupt"|"worker-asset-wait-missing"|"worker-asset-wait-unselected")=>crate::postprocess::native::asset_tests::run(native.as_ref().unwrap(),action),
        "worker-details-held"=>crate::postprocess::native::worker_tests::run_details_held(native.as_ref().unwrap()),
        "worker-header-only"=>crate::postprocess::native::worker_tests::run_header_only(native.as_ref().unwrap()),
        "worker-archive-category"=>{
            crate::pg::par2_scope::archive(&native.as_ref().unwrap().owner,[7;32]).unwrap();
            crate::postprocess::native::worker_tests::run_category_loop(native.as_ref().unwrap());
        },
        "worker-loop"=>crate::postprocess::native::worker_tests::run_category_loop(native.as_ref().unwrap()),
        "worker-fanout-fence"=>crate::postprocess::native::worker_tests::run_fanout_fence(native.as_ref().unwrap()),
        "worker-factory"=>crate::postprocess::native::run_factory_fixture(native.as_ref().unwrap()),
        "worker-feed"=>crate::postprocess::native::worker_tests::run_held_feed(native.as_ref().unwrap()),
        "worker-predb"=>crate::postprocess::native::worker_tests::run_predb_only(native.as_ref().unwrap()),
        "worker-held"=>crate::postprocess::native::worker_tests::run_held_drain(native.as_ref().unwrap()),
        "database-runner"=>crate::postprocess::native::run_category_fixture(native.as_ref().unwrap()),
        "network-runner"=>crate::postprocess::native::run_network_fixture(native.as_ref().unwrap()),
        action @ ("net-stop"|"net-stop-header"|"net-stop-rerun"|"net-stop-owner"|"net-stop-receipt"|"net-stop-outage")=>crate::postprocess::native::stop_tests::run(native.as_ref().unwrap(),action),
        "unavailable" => {
            assert!(open_test_native_owner().is_err(), "unavailable destination admitted a canonical owner");
            assert_cache_error();
            costly_errors();
        }
        "context" => {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            runtime.block_on(async { assert_cache_error(); costly_errors(); });
            drop(runtime);
            let db = native.as_ref().unwrap().db.clone();
            std::thread::Builder::new().name("cache-stage-fixture".into())
                .spawn(move || { let _scope = db.bind_native_cache_scope().unwrap(); assert_cached_positive(); })
                .unwrap().join().unwrap();
        }
        action @ ("config-removed" | "config-malformed" | "config-sqlite") => {
            assert_cached_positive();
            let path = std::path::PathBuf::from(std::env::var_os("ATLAS_HOME").unwrap()).join("config.toml");
            match action {
                "config-removed" => std::fs::remove_file(path).unwrap(),
                "config-malformed" => std::fs::write(path, "invalid = [").unwrap(),
                "config-sqlite" => {
                    let mut document: toml_edit::DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
                    document["storage"]["backend"] = toml_edit::value("sqlite");
                    std::fs::write(path, document.to_string()).unwrap();
                }
                _ => unreachable!(),
            }
            assert_cache_error();
            costly_errors();
        }
        "config-initial" => {
            let marker = "fixture-private-config-marker";
            let path = std::path::PathBuf::from(std::env::var_os("ATLAS_HOME").unwrap()).join("config.toml");
            std::fs::write(path, format!("invalid = [\"{marker}\"\n")).unwrap();
            let result = atlas::raw_store::get_opensubtitles(ABSENT);
            let error = result.error_text().expect("invalid config must report an error");
            assert!(!error.contains(marker), "configuration error echoed private contents");
            assert!(result.checked().is_err());
            assert_cache_error();
        }
        _ => panic!("unknown owned fixture action"),
    }
    drop(scope);
    if let Some(native) = native { native.owner.close(); }
}
