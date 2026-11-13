//! Executable controls require Root's isolated PG/private-home admission.
use super::*;
use crate::pg::service_cache::details as pg_details;

#[test]
#[ignore="requires explicit isolated PostgreSQL for native detail codec controls"]
fn native_detail_rows_round_trip_fixed_nulls_and_negative_tvdb_keys() {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");let mut pg=schema.pool.get().unwrap();
    for row in [Details {kind:1,tmdb_id:42,title:Some("Original".into()),overview:Some("é <raw>".into()),fetched_at:123,..Default::default()},Details {kind:2,tmdb_id:-7,title:Some("Fallback".into()),fetched_at:125,..Default::default()}] {
        pg_details::put_in(&mut *pg,&row).unwrap();
        assert_eq!(pg_details::read_in(&mut *pg,row.kind,row.tmdb_id).unwrap(),Some(row));
    }
    let key=lookup_key(2,&Ids {tvdb:Some(7),..Default::default()});
    pg_details::put_lookup_in(&mut *pg,&key,None,123).unwrap();
    assert_eq!(pg_details::lookup_in(&mut *pg,&key).unwrap(),Some((None,123)));
    assert!(pg_details::lookup_in(&mut *pg,"unknown").unwrap().is_none());
    let mut giant=Details {kind:1,tmdb_id:99,fetched_at:1,..Default::default()};giant.overview=Some("x".repeat(4*1024*1024+1));
    assert!(pg_details::put_in(&mut *pg,&giant).is_err());
    assert!(pg_details::read_in(&mut *pg,1,99).unwrap().is_none());
}

#[test]
#[ignore="requires explicit isolated PostgreSQL for bounded current-title source pages"]
fn native_detail_source_pages_use_current_release_ids_and_bounded_descending_cursor() {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    runtime.owner.with(|client| {
        for n in 0..67 {
            crate::pg::release_writer::save(client,&[crate::parser::Release {name:format!("Movie.{n}"),group:"example.group".into(),articles:vec![crate::parser::Article {filename:Some("video.mkv".into()),message_id:format!("detail-{n}@example.invalid"),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}],..Default::default()}],1)?;
        }
        client.execute("insert into media_release(release_id,kind,title_key,quality,tmdb_id) select id,1,id,0,1000000+id from releases",&[])?;
        let first=pg_details::page_in(client,None,64)?;assert_eq!(first.rows.len(),64);assert!(first.rows.windows(2).all(|p|p[0].release>p[1].release));assert!(first.rows.iter().all(|row|row.ids.tmdb==Some(1000000+row.release)),"external IDs are not native release IDs");
        let after=first.after.unwrap();let last=pg_details::page_in(client,Some(after),64)?;assert_eq!(last.rows.len(),3);assert!(last.rows.iter().all(|r|r.release<after));
        assert!(pg_details::page_in(client,last.after,64)?.exhausted);
        assert!(pg_details::page_in(client,None,65).is_err());
        client.execute("update media_release set tmdb_id=null where release_id>$1",&[&last.after.unwrap()])?;
        let unqualified=pg_details::page_in(client,None,64)?;assert!(unqualified.rows.is_empty());assert!(!unqualified.exhausted);assert!(unqualified.after.is_some(),"a bounded unresolved page advances rather than falsely ending the pass");Ok(())
    }).unwrap();
}

#[test]
#[ignore="requires actual canonical owner; disabled service must not initialize legacy storage"]
fn native_detail_disabled_service_preserves_off_policy_without_legacy_storage() {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    let mut cfg=MediaConfig::default();cfg.details_per_minute=0;cfg.tmdb.api_key="fixture-key".into();
    assert!(!native::Service::enabled(&cfg));
    let mut service=native::Service::new(&runtime.owner,&cfg,None).unwrap();
    assert!(matches!(service.tick(now_secs()).unwrap(),native::Progress::Disabled));
    cfg.details_per_minute=1;cfg.tmdb.api_key.clear();cfg.tvdb.api_key.clear();assert!(!native::Service::enabled(&cfg));
}

const NATIVE_BODY:&str=r#"{"title":"Native Movie","overview":"Original <&é>","vote_average":7.8}"#;
fn periodic_child(action:&str) {
    use std::{net::TcpListener,process::{Command,Stdio},sync::{Arc,atomic::{AtomicBool,AtomicUsize,Ordering}},time::{Duration,Instant},io::{Read,Write}};
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");let home=tempfile::tempdir().unwrap();
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();listener.set_nonblocking(true).unwrap();let base=format!("http://127.0.0.1:{}",listener.local_addr().unwrap().port());
    let stopped=Arc::new(AtomicBool::new(false));let hits=Arc::new(AtomicUsize::new(0));let stop=stopped.clone();let count=hits.clone();
    let server=std::thread::spawn(move|| {
        while !stop.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut socket,_))=>{
                    socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();let mut request=Vec::new();let mut byte=[0];
                    while request.len()<8192&&!request.ends_with(b"\r\n\r\n") {if socket.read(&mut byte).unwrap_or(0)==0{break;}request.push(byte[0]);}
                    count.fetch_add(1,Ordering::Relaxed);
                    let body=if request.starts_with(b"GET /find/"){r#"{"movie_results":[],"tv_results":[]}"#}else{NATIVE_BODY};
                    write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
                },Err(error) if error.kind()==std::io::ErrorKind::WouldBlock=>std::thread::sleep(Duration::from_millis(10)),Err(error)=>panic!("owned provider observer failed: {error}"),
            }
        }
    });
    struct Server {stop:Arc<AtomicBool>,thread:Option<std::thread::JoinHandle<()>>}
    impl Drop for Server {fn drop(&mut self){self.stop.store(true,Ordering::Release);if let Some(thread)=self.thread.take(){thread.join().unwrap();}}}
    let _server=Server {stop:stopped,thread:Some(server)};
    let text=format!("[storage]\nbackend='postgres'\nurl={}\nschema={}\nmax_connections=1\n",serde_json::to_string(&schema.url).unwrap(),serde_json::to_string(&schema.name).unwrap());
    std::fs::write(home.path().join("config.toml"),text).unwrap();let sentinel=b"native periodic services must not initialize SQLite";
    std::fs::write(home.path().join("atlas.raw.db"),sentinel).unwrap();
    let mut child=Command::new(std::env::current_exe().unwrap()).args(["--ignored","--exact","media::details::native_tests::native_periodic_child","--nocapture"])
        .env("ATLAS_HOME",home.path()).env("ATLAS_NO_KEYRING","1").env("ATLAS_PERIODIC_NATIVE_CHILD",action).env("ATLAS_PERIODIC_FIXTURE_BASE",&base).env_remove("ATLAS_PG_TEST_URL")
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let until=Instant::now()+Duration::from_secs(30);
    loop {if child.try_wait().unwrap().is_some(){break;}if Instant::now()>=until{child.kill().unwrap();let _=child.wait();panic!("owned periodic child timed out");}std::thread::sleep(Duration::from_millis(20));}
    let result=child.wait_with_output().unwrap();assert!(result.status.success(),"periodic native child failed: {}\n{}",String::from_utf8_lossy(&result.stdout),String::from_utf8_lossy(&result.stderr));
    assert_eq!(hits.load(Ordering::Relaxed),1,"actual provider visit must be paced and never used after storage failure");
    assert_eq!(std::fs::read(home.path().join("atlas.raw.db")).unwrap(),sentinel);
    for name in ["atlas.media.db","atlas.details.db","atlas.db","atlas.raw.db-wal","atlas.raw.db-shm"] {assert!(!home.path().join(name).exists(),"native child created a forbidden SQLite sidecar");}
}
#[test]
#[ignore="requires isolated PG/private home and owned loopback provider observer"]
fn native_periodic_actual_tick_matches_legacy_metadata_parser_and_pacing_without_sqlite() {periodic_child("positive");}
#[test]
#[ignore="requires isolated PG/private home and owned loopback provider observer"]
fn native_periodic_lookup_negative_is_real_and_pg_errors_never_fetch_or_replace_it() {periodic_child("negative");}

#[test]
#[ignore="only the owned periodic parent may invoke this child"]
fn native_periodic_child() {
    use std::time::{Duration,Instant};
    let action=std::env::var("ATLAS_PERIODIC_NATIVE_CHILD").unwrap();let base=std::env::var("ATLAS_PERIODIC_FIXTURE_BASE").unwrap();
    let runtime=crate::raw_store::native_pg_opensubtitles_cache_tests::open_test_native_owner().expect("actual canonical owner");
    let mut cfg=MediaConfig::default();cfg.details_per_minute=60;cfg.tmdb.api_key="fixture-key".into();cfg.tmdb.base_url=Some(base);
    cfg.tvdb.api_key.clear();cfg.omdb.api_key.clear();cfg.trakt.client_id.clear();
    let mut service=native::Service::new(&runtime.owner,&cfg,None).unwrap();let now=now_secs();let at=Instant::now()+BACKFILL_DELAY+Duration::from_secs(1);
    if action=="positive" {
        runtime.owner.with(|client| {
            crate::pg::release_writer::save(client,&[crate::parser::Release {name:"Native.Movie".into(),group:"example.group".into(),articles:vec![crate::parser::Article {filename:Some("video.mkv".into()),message_id:"periodic@example.invalid".into(),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}],..Default::default()}],1)?;
            client.execute("insert into media_release(release_id,kind,title_key,quality,tmdb_id) select id,$1,id,0,42 from releases",&[&(Kind::Movie as i64)])?;Ok(())
        }).unwrap();
        assert!(matches!(service.tick_at(now,at).unwrap(),native::Progress::Visited));
        let expected=from_tmdb(Kind::Movie as i64,42,&serde_json::from_str(NATIVE_BODY).unwrap(),now);
        runtime.owner.with(|client| {assert_eq!(pg_details::read_in(client,Kind::Movie as i64,42)?,Some(expected));Ok(())}).unwrap();
        assert!(matches!(service.tick_at(now,at).unwrap(),native::Progress::Waiting(_)));
        runtime.owner.stop.store(true,std::sync::atomic::Ordering::Release);
        let error=service.tick_at(now+2,at+Duration::from_secs(2)).unwrap_err();assert_eq!(crate::pg::outage::classify(&error),crate::pg::outage::Failure::Unavailable);
    } else {
        assert_eq!(action,"negative");let ids=Ids {imdb:Some(123),..Default::default()};
        assert!(service.for_title(Kind::Movie as i64,&ids,now).unwrap().is_none());
        assert!(service.for_title(Kind::Movie as i64,&ids,now+1).unwrap().is_none());
        let key=lookup_key(Kind::Movie as i64,&ids);
        runtime.owner.with(|client| {assert_eq!(pg_details::lookup_in(client,&key)?,Some((None,now)));client.batch_execute("alter table ext_media_details_lookups rename to fixture_details_lookups_unavailable")?;Ok(())}).unwrap();
        let error=match service.for_title(Kind::Movie as i64,&ids,now+2) {Err(crate::media::resolve::Trouble::Cache(error))=>error.into_error(),_=>panic!("actual PG cache error must not become a miss or provider visit")};
        assert!(error.chain().any(|cause|cause.downcast_ref::<postgres::Error>().is_some()));assert_eq!(crate::pg::outage::classify(&error),crate::pg::outage::Failure::Fatal);
    }
}

#[test]
#[ignore="requires actual isolated PostgreSQL for one-snapshot text admission"]
fn native_detail_admission_uses_one_relation_scan_and_rejects_concurrent_oversize_replacement() {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");
    let mut reader=schema.pool.get().unwrap();let mut writer=schema.pool.get().unwrap();
    let row=Details {kind:Kind::Movie as i64,tmdb_id:42,title:Some("kept".into()),overview:Some("small".into()),fetched_at:1,..Default::default()};
    pg_details::put_in(&mut *writer,&row).unwrap();
    let mut tx=reader.transaction().unwrap();
    let scans=|tx:&mut postgres::Transaction<'_>|->i64 {
        tx.query_one("select seq_scan+coalesce(idx_scan,0) from pg_stat_xact_user_tables where relid='ext_media_details'::regclass",&[]).unwrap().get(0)
    };
    let before=scans(&mut tx);assert_eq!(pg_details::read_in(&mut tx,row.kind,row.tmdb_id).unwrap(),Some(row.clone()));
    assert_eq!(scans(&mut tx)-before,1,"admission and payload must share one actual relation scan/snapshot, not two READ COMMITTED commands");
    // This second, real session commits while the original reader transaction
    // remains open. Its next RC statement sees the replacement, not old size.
    writer.execute("update ext_media_details set overview=repeat('x',4194305) where kind=$1 and tmdb_id=$2",&[&row.kind,&row.tmdb_id]).unwrap();
    let before=scans(&mut tx);assert!(pg_details::read_in(&mut tx,row.kind,row.tmdb_id).is_err());
    assert_eq!(scans(&mut tx)-before,1);
    assert_eq!(writer.query_one("select octet_length(overview) from ext_media_details where kind=$1 and tmdb_id=$2",&[&row.kind,&row.tmdb_id]).unwrap().get::<_,i32>(0),4194305,"refusal must preserve the actual replacement");
}

#[test]
#[ignore="requires two real isolated canonical owners and a genuine selected manifest"]
fn native_details_factory_rejects_cross_owner_asset_grant_before_provider_or_pg_effects() {
    use sha2::{Digest,Sha256};use crate::pg::{self,worker_assets::Grant};
    let hex=|bytes:&[u8]|bytes.iter().map(|byte|format!("{byte:02x}")).collect::<String>();
    let a_schema=pg::testing::Schema::new().expect("explicit isolated PG required");let b_schema=pg::testing::Schema::new().expect("explicit isolated PG required");
    let a=pg::stage_queue_tests::runtime(&a_schema);let b=pg::stage_queue_tests::runtime(&b_schema);
    let metadata=Details {kind:Kind::Movie as i64,tmdb_id:42,poster:Some("/same.jpg".into()),fetched_at:1,..Default::default()};
    a.owner.with(|client|pg_details::put_in(client,&metadata)).unwrap();b.owner.with(|client|pg_details::put_in(client,&metadata)).unwrap();
    let home=tempfile::tempdir().unwrap();let evidence=tempfile::tempdir().unwrap();std::fs::create_dir(home.path().join("covers")).unwrap();
    let bytes=[0xff,0xd8,0xff,0xe0,0xff,0xd9];let name=hex(&Sha256::digest(bytes));std::fs::write(home.path().join("covers").join(name),bytes).unwrap();
    let manifest=evidence.path().join("assets.jsonl");pg::testing::capture_asset_manifest(home.path(),&manifest,pg::testing::AssetCaptureLimits {min_files:1,max_files:8,max_file_bytes:1024,max_total_bytes:8192,read_bytes_per_second:32<<20}).unwrap();
    let selection=crate::config::NativeAssetsConfig {root:home.path().canonicalize().unwrap().to_str().unwrap().into(),manifest:manifest.canonicalize().unwrap().to_str().unwrap().into(),whole_sha256:hex(&Sha256::digest(std::fs::read(&manifest).unwrap()))};
    let grant=Grant::open(&b.owner,Some(&selection)).unwrap().unwrap();
    let observer=std::net::TcpListener::bind("127.0.0.1:0").unwrap();observer.set_nonblocking(true).unwrap();
    let mut cfg=MediaConfig::default();cfg.details_per_minute=1;cfg.tmdb.api_key="fixture-key".into();cfg.tmdb.base_url=Some(format!("http://127.0.0.1:{}",observer.local_addr().unwrap().port()));
    assert!(native::Service::new(&a.owner,&cfg,Some(grant.clone())).is_err(),"equal metadata keys/path do not authorize another owner/catalog grant");
    assert!(native::Service::new(&b.owner,&cfg,Some(grant)).is_ok(),"same original owner remains a real positive constructor");
    for runtime in [&a,&b] {runtime.owner.with(|client| {assert_eq!(pg_details::read_in(client,metadata.kind,metadata.tmdb_id)?,Some(metadata.clone()));assert_eq!(client.query_one("select count(*) from ext_asset_files",&[])?.try_get::<_,i64>(0)?,0);Ok(())}).unwrap();assert_eq!(runtime.owner.failure(),None);}
    assert!(matches!(observer.accept(),Err(error) if error.kind()==std::io::ErrorKind::WouldBlock));
}
