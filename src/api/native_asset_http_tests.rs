//! Explicit real-PG literal HTTP route regression. The parent retains schema
//! and original owner lifetime; a fresh child selects only its private config.
use super::*;
use crate::pg;
use sha2::{Digest,Sha256};
use std::{io::{Read,Write},net::TcpStream,process::{Command,Stdio},thread,time::Instant};
const KEY:&str="asset-route-fixture-key";
const CHILD:&str="ATLAS_ASSET_ROUTE_CHILD";
const SELECTOR:&str="api::native_asset_http_tests::authenticated_dynamic_posters_and_covers_use_literal_http_routes";
fn jpeg(label:&[u8])->Vec<u8> {[&[0xff,0xd8,0xff,0xe0][..],label,&[0xff,0xd9]].concat()}
fn hex(bytes:&[u8])->String {bytes.iter().map(|byte|format!("{byte:02x}")).collect()}
struct Wire {status:u16,headers:Vec<(String,String)>,body:Vec<u8>}
impl Wire {fn header(&self,key:&str)->Option<&str> {self.headers.iter().find(|(name,_)|name.eq_ignore_ascii_case(key)).map(|(_,value)|value.as_str())}}
fn exchange(storage:&pg::StorageConfig,assets:&Arc<pg::fresh_assets::AssetManifest>,method:&str,path:&str,cookie:Option<&str>,body:&str)->Wire {
    let server=Server::http_bounded("127.0.0.1:0",tiny_http::RequestLimits {max_connections:2,read_idle:Duration::from_secs(2),write_idle:Duration::from_secs(2),
        request_timeout:Duration::from_secs(10),max_header_bytes:4096,max_header_count:32,max_request_line_bytes:4096,max_body_wire_bytes:16384}).unwrap();
    let address=server.server_addr().to_ip().unwrap();let storage=storage.clone();let assets=assets.clone();
    let worker=thread::spawn(move|| {
        let request=server.recv_timeout(Duration::from_secs(3)).unwrap().expect("one owned HTTP request");
        respond_with_assets(request,KEY,"fixture.invalid",&storage,Some(&assets));
    });
    let mut client=TcpStream::connect(address).unwrap();client.set_read_timeout(Some(Duration::from_secs(12))).unwrap();client.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
    write!(client,"{method} {path} HTTP/1.1\r\nHost: fixture.invalid\r\nConnection: close\r\n").unwrap();
    if let Some(cookie)=cookie {write!(client,"Cookie: {cookie}\r\n").unwrap();}
    if method=="POST" {write!(client,"Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n",body.len()).unwrap();}
    write!(client,"\r\n{body}").unwrap();client.flush().unwrap();
    let mut bytes=Vec::new();client.take(65537).read_to_end(&mut bytes).unwrap();worker.join().unwrap();assert!(bytes.len()<=65536,"fixed HTTP response exceeded fixture admission");
    let split=bytes.windows(4).position(|part|part==b"\r\n\r\n").expect("actual HTTP header terminator");let mut lines=std::str::from_utf8(&bytes[..split]).unwrap().split("\r\n");
    let status=lines.next().unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
    let headers=lines.map(|line|{let (name,value)=line.split_once(':').unwrap();(name.into(),value.trim().into())}).collect();
    let wire=Wire {status,headers,body:bytes[split+4..].to_vec()};assert!(wire.header("Date").is_some()&&wire.header("Server").is_some());
    if method!="HEAD" {assert_eq!(wire.header("Content-Length").unwrap().parse::<usize>().unwrap(),wire.body.len());}wire
}
fn child() {
    let home=crate::paths::app_dir();let config:serde_json::Value=serde_json::from_slice(&std::fs::read(home.join("config.json")).unwrap()).unwrap();
    let storage=crate::config::checked_storage_config().expect("private selected PG config");assert!(storage.is_postgres());
    let assets=selected_asset_manifest(Some(&crate::config::NativeAssetsConfig {root:config["native_assets"]["root"].as_str().unwrap().into(),
        manifest:config["native_assets"]["manifest"].as_str().unwrap().into(),whole_sha256:config["native_assets"]["whole_sha256"].as_str().unwrap().into()})).unwrap().unwrap();
    let id=config["fixture_release"].as_i64().unwrap();let cover=format!("/ui/cover?id={id}");let poster="/ui/poster?kind=2&tmdb=42";
    for path in [poster,cover.as_str()] {let response=exchange(&storage,&assets,"GET",path,None,"");assert_eq!(response.status,401);assert_eq!(response.header("HX-Redirect"),Some("/"));assert!(response.body.is_empty());}
    let login=exchange(&storage,&assets,"POST","/login",None,&format!("key={KEY}"));assert_eq!(login.status,303);
    let cookie=login.header("Set-Cookie").unwrap().split(';').next().unwrap();assert!(cookie.starts_with("atlas_session=")&&!cookie.contains(KEY));
    for (path,wanted) in [(poster,jpeg(b"new poster")),(cover.as_str(),jpeg(b"new cover"))] {
        let response=exchange(&storage,&assets,"GET",path,Some(cookie),"");assert_eq!(response.status,200);assert_eq!(response.body,wanted);
        assert_eq!(response.header("Content-Type"),Some("image/jpeg"));assert_eq!(response.header("X-Content-Type-Options"),Some("nosniff"));
        let head=exchange(&storage,&assets,"HEAD",path,Some(cookie),"");assert_eq!(head.status,200);assert!(head.body.is_empty());assert_eq!(head.header("Content-Length").unwrap().parse::<usize>().unwrap(),wanted.len());
    }
    for (id,status) in [(43,404),(44,503),(45,503)] {
        let response=exchange(&storage,&assets,"GET",&format!("/ui/poster?kind=2&tmdb={id}"),Some(cookie),"");assert_eq!(response.status,status,"missing/corrupt/unbound assets must not be substituted");
        assert_ne!(response.body,jpeg(b"new poster"));
    }
    assert!(!home.join("atlas.db").exists()&&!home.join("atlas.details.db").exists(),"literal native routes must not initialize legacy catalog/cache files");
}
#[test]
#[ignore="requires explicit isolated PG, Unix asset confinement, admitted production spool disk and owned HTTP child"]
fn authenticated_dynamic_posters_and_covers_use_literal_http_routes() {
    if std::env::var(CHILD).as_deref()==Ok("1") {child();return;}
    let schema=pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
    let home=tempfile::tempdir().unwrap();let root=tempfile::tempdir().unwrap();let evidence=tempfile::tempdir().unwrap();
    let headers=crate::config::HeaderStoreConfig {enabled:true,dir:home.path().join("headers").to_str().unwrap().into()};
    let storage=pg::StorageConfig {backend:pg::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:4,allow_plaintext:true};
    let mut cfg=crate::config::Config::default();cfg.postprocess.enabled=false;cfg.predb.enabled=false;cfg.spots.enabled=false;cfg.inferred_parts=false;
    cfg.media.tvmaze.enabled=false;cfg.media.allocine.enabled=false;cfg.media.details_per_minute=0;
    let runtime=pg::indexer_runtime::Runtime::open(&cfg,storage.clone(),headers.clone(),&[],&mut None).unwrap();
    let id=runtime.owner.with(|client| {
        let saved=pg::release_writer::save(client,&[crate::parser::Release {name:"Actual.Asset.Route.Release".into(),poster:"poster".into(),group:"alt.binaries.test".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
            articles:vec![crate::parser::Article {filename:Some("video.mkv".into()),subject:"video.mkv yEnc (1/1)".into(),message_id:"asset-route@example.invalid".into(),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}],..Default::default()}],-100000)?;
        assert_eq!((saved.releases,saved.files,saved.parts),(1,1,1));Ok(client.query_one("select id from releases",&[])?.get::<_,i64>(0))
    }).unwrap();
    std::fs::create_dir(root.path().join("posters")).unwrap();std::fs::write(root.path().join("posters/original.jpg"),jpeg(b"retained original")).unwrap();
    let manifest=evidence.path().join("assets.jsonl");pg::testing::capture_asset_manifest(root.path(),&manifest,pg::testing::AssetCaptureLimits {min_files:1,max_files:8,max_file_bytes:1<<20,max_total_bytes:8<<20,read_bytes_per_second:32<<20}).unwrap();
    let original_manifest=std::fs::read(&manifest).unwrap();let selection=crate::config::NativeAssetsConfig {root:root.path().canonicalize().unwrap().to_str().unwrap().into(),
        manifest:manifest.canonicalize().unwrap().to_str().unwrap().into(),whole_sha256:hex(&Sha256::digest(&original_manifest))};
    let grant=pg::worker_assets::Grant::open(&runtime.owner,Some(&selection)).unwrap().unwrap();
    for (tmdb,name) in [(42,"generated.jpg"),(43,"missing.jpg"),(44,"corrupt.jpg"),(45,"wrong.jpg")] {
        let path=format!("/{name}");let details=crate::media::details::Details {kind:2,tmdb_id:tmdb,poster:Some(path.clone()),fetched_at:1,..Default::default()};
        runtime.owner.with(|client|pg::service_cache::details::put_in(client,&details)).unwrap();
        assert_eq!(grant.produce_poster(2,tmdb,&path,&jpeg(b"new poster")).unwrap(),pg::worker_assets::ArtworkStatus::Kept);
    }
    let cover=grant.produce_cover(id,&crate::probe::media::Cover {mime:"image/jpeg",bytes:jpeg(b"new cover")}).unwrap();cover.check(&runtime.owner,id).unwrap();let (mime,sha256)=cover.descriptor();let cover_name=sha256.clone();
    let queue=pg::stage_queue::Queue::for_owner(&runtime.owner).unwrap();let claims=queue.claim(10,1,60).unwrap();assert_eq!(claims.len(),1);let claim=&claims[0];assert_eq!(claim.release_id(),id);
    let redo=runtime.owner.with(|client|Ok(client.query_one("select coalesce((select rerun_gen from pp_state where release_id=$1),0)",&[&id])?.get::<_,i64>(0))).unwrap();
    let bit=crate::postprocess::step::mask(crate::probe::step::PROBE_STEP);let mut applied=crate::postprocess::apply::Applied::native_database(id,redo,claim.due_at(),bit,0);
    applied.set_done=bit;applied.writes.push((bit,crate::postprocess::step::StepWrite::for_mutation(id,crate::postprocess::step::StepMutation::KeepCover {mime,sha256})));
    let visit=pg::stage_queue::Visit::prepare(claim,applied,Sha256::digest(jpeg(b"new cover")).into()).unwrap();assert_eq!(queue.settle(claim,visit,11).unwrap(),pg::stage_queue::Settlement::Applied);drop(cover);
    std::fs::remove_file(root.path().join("posters/missing.jpg")).unwrap();std::fs::write(root.path().join("posters/corrupt.jpg"),jpeg(b"bad poster")).unwrap();
    runtime.owner.with(|client|{client.execute("update ext_asset_files set source_digest=decode(repeat('00',32),'hex') where kind='posters' and name='wrong.jpg'",&[])?;Ok(())}).unwrap();
    assert_eq!(std::fs::read(&manifest).unwrap(),original_manifest);assert!(!std::str::from_utf8(&original_manifest).unwrap().contains("generated.jpg"));assert!(!std::str::from_utf8(&original_manifest).unwrap().contains(&cover_name),"new cover is absent from the selected original manifest");
    let config=serde_json::json!({"storage":{"backend":"postgres","url":schema.url,"schema":schema.name,"max_connections":4,"allow_plaintext":true},
        "header_store":{"enabled":true,"dir":headers.dir},"native_assets":{"root":selection.root,"manifest":selection.manifest,"whole_sha256":selection.whole_sha256},"fixture_release":id});
    use std::os::unix::fs::OpenOptionsExt;
    let mut file=std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(home.path().join("config.json")).unwrap();file.write_all(&serde_json::to_vec(&config).unwrap()).unwrap();drop(file);
    let mut child=Command::new(std::env::current_exe().unwrap()).args(["--exact",SELECTOR,"--ignored","--test-threads=1"]).env("ATLAS_HOME",home.path()).env(CHILD,"1")
        .stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let deadline=Instant::now()+Duration::from_secs(60);let status=loop {
        if let Some(status)=child.try_wait().unwrap(){break Some(status)}
        if Instant::now()>=deadline {child.kill().unwrap();child.wait().unwrap();break None}thread::sleep(Duration::from_millis(20));
    };
    runtime.owner.close();assert!(status.is_some_and(|status|status.success()),"owned asset HTTP child failed or exceeded its deadline; no environment or credentials captured");
    assert_eq!(std::fs::read(&manifest).unwrap(),original_manifest,"new writer receipts must not rewrite the selected original manifest");
}

#[path="native_image_asset_http_tests.rs"]
mod image_tests;
