//! Test-first image producer regression using existing worker and HTTP APIs.
//! The ignored parent owns the isolated schema; the fresh child owns its actual
//! canonical runtime and joins all private NNTP/HTTP work before exiting.
use super::{exchange,KEY};
use crate::pg;
use sha2::{Digest,Sha256};
use std::{fs::OpenOptions,io::Write,os::unix::fs::OpenOptionsExt,process::{Command,Stdio},time::{Duration,Instant},thread};
const CHILD:&str="ATLAS_IMAGE_ASSET_CHILD";
const SELECTOR:&str="api::native_asset_http_tests::image_tests::native_images_step_output_reaches_authenticated_image_route";
fn hex(bytes:&[u8])->String {bytes.iter().map(|b|format!("{b:02x}")).collect()}
fn selection(value:&serde_json::Value)->crate::config::NativeAssetsConfig {
    crate::config::NativeAssetsConfig {root:value["root"].as_str().unwrap().into(),manifest:value["manifest"].as_str().unwrap().into(),whole_sha256:value["whole_sha256"].as_str().unwrap().into()}
}
fn write_private(path:&std::path::Path,bytes:&[u8]) {
    let mut file=OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).unwrap();file.write_all(bytes).unwrap();
}
fn child() {
    let home=crate::paths::app_dir();let value:serde_json::Value=serde_json::from_slice(&std::fs::read(home.join("config.json")).unwrap()).unwrap();
    let storage=crate::config::checked_storage_config().unwrap();assert!(storage.is_postgres());let selection=selection(&value["native_assets"]);
    let manifest_before=std::fs::read(&selection.manifest).unwrap();let assets=super::selected_asset_manifest(Some(&selection)).unwrap().unwrap();
    let mut config=crate::config::Config::default();config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;
    config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;config.native_assets=Some(selection.clone());
    let headers=crate::config::header_store_config();let runtime=pg::indexer_runtime::Runtime::open(&config,storage.clone(),headers,&[],&mut None).unwrap();
    // Select the ordinary auto-images path: no manually fabricated request,
    // KeepImage mutation, body, positive receipt or ready row.
    config.postprocess.enabled=true;config.postprocess.settle_secs=1;config.postprocess.complete_settle_secs=1;
    config.archive.enabled=true;config.archive.pictures_auto_max=1;config.archive.pictures_auto_max_mb=1;
    let id=crate::postprocess::native::image_asset_tests::run(&runtime,&config);
    let digest=hex(&Sha256::digest(crate::postprocess::native::image_asset_tests::IMAGE));
    assert!(!std::str::from_utf8(&manifest_before).unwrap().contains(&digest),"worker output must be absent from the selected original manifest");
    assert_eq!(std::fs::read(home.join("images").join(&digest)).unwrap(),crate::postprocess::native::image_asset_tests::IMAGE,"actual worker must durably produce these bytes");
    let url=format!("/ui/image?id={id}&at=0");
    let unsigned=exchange(&storage,&assets,"GET",&url,None,"");assert_eq!(unsigned.status,401);
    let login=exchange(&storage,&assets,"POST","/login",None,&format!("key={KEY}"));assert_eq!(login.status,303);
    let cookie=login.header("Set-Cookie").unwrap().split(';').next().unwrap();
    let get=exchange(&storage,&assets,"GET",&url,Some(cookie),"");let head=exchange(&storage,&assets,"HEAD",&url,Some(cookie),"");
    let receipts=runtime.owner.with(|client|Ok(client.query_one("select count(*) from ext_asset_files where kind='images' and name=$1",&[&digest])?.get::<_,i64>(0))).unwrap();
    let observed=serde_json::json!({"get_status":get.status,"get_bytes":get.body.len(),"get_sha256":hex(&Sha256::digest(&get.body)),"get_type":get.header("Content-Type"),"nosniff":get.header("X-Content-Type-Options"),
        "head_status":head.status,"head_bytes":head.body.len(),"head_length":head.header("Content-Length"),"image_receipts":receipts,
        "manifest_unchanged":std::fs::read(&selection.manifest).unwrap()==manifest_before,
        "no_legacy_catalog":!home.join("atlas.db").exists()&&!home.join("atlas.details.db").exists()&&!home.join("atlas.raw.db").exists()});
    // Fixed non-secret observations survive a semantic HTTP RED. No DSN,
    // headers/cookies/config/environment or image payload enters this record.
    write_private(&home.join("image-http-observed.json"),&serde_json::to_vec(&observed).unwrap());runtime.owner.close();
}
#[test]
#[ignore="explicit isolated PG, owned NNTP/HTTP child, Unix physical assets and production spool-space admission"]
fn native_images_step_output_reaches_authenticated_image_route() {
    if std::env::var(CHILD).as_deref()==Ok("1") {child();return;}
    let schema=pg::testing::Schema::new().expect("explicit isolated PG required");let home=tempfile::tempdir().unwrap();let evidence=tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join("images")).unwrap();std::fs::write(home.path().join("images/retained.gif"),b"GIF89a\x02\0\x02\0").unwrap();
    let manifest=evidence.path().join("assets.jsonl");pg::testing::capture_asset_manifest(home.path(),&manifest,pg::testing::AssetCaptureLimits {min_files:1,max_files:8,max_file_bytes:1<<20,max_total_bytes:8<<20,read_bytes_per_second:32<<20}).unwrap();
    let original=std::fs::read(&manifest).unwrap();
    let config=serde_json::json!({"storage":{"backend":"postgres","url":schema.url,"schema":schema.name,"max_connections":4,"allow_plaintext":true},
        "header_store":{"enabled":true,"dir":home.path().join("headers")},"native_assets":{"root":home.path().canonicalize().unwrap(),"manifest":manifest.canonicalize().unwrap(),"whole_sha256":hex(&Sha256::digest(&original))}});
    write_private(&home.path().join("config.json"),&serde_json::to_vec(&config).unwrap());
    let mut child=Command::new(std::env::current_exe().unwrap()).args(["--exact",SELECTOR,"--ignored","--test-threads=1"]).env("ATLAS_HOME",home.path()).env(CHILD,"1").env("ATLAS_NO_KEYRING","1")
        .stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let deadline=Instant::now()+Duration::from_secs(60);let status=loop {
        if let Some(status)=child.try_wait().unwrap(){break Some(status)}
        if Instant::now()>=deadline{child.kill().unwrap();child.wait().unwrap();break None}thread::sleep(Duration::from_millis(20));
    };
    assert!(status.is_some_and(|status|status.success()),"worker/owned transport setup failed before semantic HTTP observations");
    let file=home.path().join("image-http-observed.json");assert!(std::fs::metadata(&file).unwrap().len()<=4096);let observed:serde_json::Value=serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
    assert_eq!(observed["get_status"],200,"genuine ImagesStep/KeepImage output must be readable through authenticated literal /ui/image");
    assert_eq!(observed["get_bytes"],crate::postprocess::native::image_asset_tests::IMAGE.len());assert_eq!(observed["get_sha256"],hex(&Sha256::digest(crate::postprocess::native::image_asset_tests::IMAGE)));
    assert_eq!(observed["get_type"],"image/gif");assert_eq!(observed["nosniff"],"nosniff");assert_eq!(observed["head_status"],200);assert_eq!(observed["head_bytes"],0);
    assert_eq!(observed["head_length"],crate::postprocess::native::image_asset_tests::IMAGE.len().to_string());assert_eq!(observed["image_receipts"],1);
    assert_eq!(observed["manifest_unchanged"],true);assert_eq!(observed["no_legacy_catalog"],true);assert_eq!(std::fs::read(&manifest).unwrap(),original);
}
