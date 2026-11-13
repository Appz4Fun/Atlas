use super::*;
use std::{sync::atomic::{AtomicBool,AtomicU64},time::{Duration,Instant,UNIX_EPOCH}};
fn fixture()->(tempfile::TempDir,AssetManifest) {
    let home=tempfile::tempdir().unwrap();let root=home.path().join("assets");std::fs::create_dir(&root).unwrap();std::fs::create_dir(root.join("images")).unwrap();
    std::fs::write(root.join("images/original"),b"captured original").unwrap();let manifest=home.path().join("assets.jsonl");
    cache_assets::capture_file(&root,&manifest,cache_assets::Limits {min_files:1,max_files:8,max_file_bytes:1<<20,max_total_bytes:8<<20,read_bytes_per_second:32<<20}).unwrap();
    let sha: [u8;32]=Sha256::digest(std::fs::read(&manifest).unwrap()).into();let admitted=AssetManifest::open(&root.canonicalize().unwrap(),&manifest.canonicalize().unwrap(),sha).unwrap();(home,admitted)
}
#[test]
fn native_image_cache_preserves_original_charge_and_oldest_eviction_in_opened_directory() {
    assert_eq!((image_charge(1),image_charge(20000),image_charge(40000)),(16384,20480,40960));
    let (home,manifest)=fixture();let root=home.path().join("assets/images");std::fs::remove_file(root.join("original")).unwrap();
    for (name,at) in [("old",1u64),("middle",2),("new",3)] {
        let path=root.join(name);std::fs::write(&path,b"tiny image").unwrap();std::fs::File::options().write(true).open(path).unwrap().set_times(std::fs::FileTimes::new().set_modified(UNIX_EPOCH+Duration::from_secs(at))).unwrap();
    }
    let directory=manifest.writable_directory(AssetKind::Images).unwrap();
    assert!(trim_images(&directory,2*16384,&AtomicBool::new(false),Instant::now()+Duration::from_secs(2)).unwrap());
    assert!(!root.join("old").exists()&&root.join("middle").exists()&&root.join("new").exists());
    // Reusing an opened directory must rewind its scan, not mistake EOF for an
    // empty cache and return a falsely successful quota result.
    assert!(trim_images(&directory,16384,&AtomicBool::new(false),Instant::now()+Duration::from_secs(2)).unwrap());assert!(!root.join("middle").exists()&&root.join("new").exists());
}
#[test]
fn native_image_writer_refuses_symlink_namespace_and_stopped_write_without_manifest_changes() {
    let (home,manifest)=fixture();let original=std::fs::read(home.path().join("assets.jsonl")).unwrap();let writes=AtomicU64::new(0);
    assert!(manifest.store_image(b"candidate",&writes,&AtomicBool::new(true)).is_err());
    let foreign=home.path().join("foreign");std::fs::create_dir(&foreign).unwrap();std::fs::write(foreign.join("sentinel"),b"valuable").unwrap();
    std::fs::rename(home.path().join("assets/images"),home.path().join("old-images")).unwrap();std::os::unix::fs::symlink(&foreign,home.path().join("assets/images")).unwrap();
    assert!(manifest.store_image(b"candidate",&writes,&AtomicBool::new(false)).is_err());assert_eq!(std::fs::read(foreign.join("sentinel")).unwrap(),b"valuable");assert_eq!(std::fs::read_dir(&foreign).unwrap().count(),1);
    assert_eq!(std::fs::read(home.path().join("assets.jsonl")).unwrap(),original);
}
