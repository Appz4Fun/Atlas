//! Real owned store/barrier controls for canceled capture ownership. No PG,
//! provider, flag-mirror or hand-created durability acknowledgment is involved.
use super::*;
use crate::nxv_store::live::Fetch;
fn owned(root:&Path,bytes:usize)->Arc<Store> {
    let mut cfg=StoreConfig::new(root);cfg.memory_budget=bytes;cfg.sealers=1;cfg.bounded_maps=true;
    Store::open(cfg,KeepSet {live_from:i64::MIN/2,..Default::default()}).unwrap().0
}
fn rows()->Vec<Vec<u8>> {vec![b"1\towned subject\towned poster\tThu, 12 Nov 2026 00:00:00 +0000\t<owned@fetch.fixture.test>\t\t10\t1".to_vec()]}
#[test]
fn canceled_capture_finish_holds_admission_until_real_record_and_ack_finish() {
    let dir=tempfile::tempdir().unwrap();let store=owned(dir.path(),8192);
    let rt=tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    rt.block_on(async {
        let fetch=Fetch::begin_on(store.clone()).await.unwrap();
        let blocked=store.recording.write().unwrap();
        let mut capture=Box::pin(fetch.record("owned","owned.group",rows()));
        assert!(tokio::time::timeout(Duration::from_millis(30),&mut capture).await.is_err());drop(capture);
        let mut finish=Box::pin(fetch.finish());let mut next=Box::pin(Fetch::begin_on(store.clone()));
        assert!(tokio::time::timeout(Duration::from_millis(30),&mut finish).await.is_err(),"detached admitted recording remains joined");
        assert!(tokio::time::timeout(Duration::from_millis(30),&mut next).await.is_err(),"cancellation cannot refund the occupied fetch slot");
        drop(blocked);tokio::time::timeout(Duration::from_secs(2),finish).await.unwrap().unwrap();
        let next=tokio::time::timeout(Duration::from_secs(2),next).await.unwrap().unwrap();next.finish().await.unwrap();
        store.sync_point("owned.group").expect("real admitted recording required").wait(Duration::from_secs(1)).unwrap();
    });store.close().unwrap();
}
#[test]
fn finish_does_not_drain_a_foreign_requests_blocked_capture() {
    let dir=tempfile::tempdir().unwrap();let store=owned(dir.path(),2*FETCH_HEADROOM);
    let rt=tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    rt.block_on(async {
        let foreign=Fetch::begin_on(store.clone()).await.unwrap();let own=Fetch::begin_on(store.clone()).await.unwrap();
        let blocked=store.recording.write().unwrap();let mut capture=Box::pin(foreign.record("foreign","foreign.group",rows()));
        assert!(tokio::time::timeout(Duration::from_millis(30),&mut capture).await.is_err());drop(capture);
        tokio::time::timeout(Duration::from_millis(100),own.finish()).await.expect("request-local close must ignore a foreign fetch's pending work").unwrap();
        let mut foreign=Box::pin(foreign.finish());assert!(tokio::time::timeout(Duration::from_millis(30),&mut foreign).await.is_err());
        drop(blocked);tokio::time::timeout(Duration::from_secs(2),foreign).await.unwrap().unwrap();
    });store.close().unwrap();
}
#[test]
fn canceled_capture_publishes_real_storage_failure_before_finishing_admission() {
    let dir=tempfile::tempdir().unwrap();let store=owned(dir.path(),8192);
    // Actual filesystem obstruction: the incoming group's directory is a file.
    std::fs::write(dir.path().join(encode_name("broken.group")),b"not a directory").unwrap();
    let rt=tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    rt.block_on(async {
        let fetch=Fetch::begin_on(store.clone()).await.unwrap();let blocked=store.recording.write().unwrap();
        let mut capture=Box::pin(fetch.record("owned","broken.group",rows()));assert!(tokio::time::timeout(Duration::from_millis(30),&mut capture).await.is_err());drop(capture);
        let mut finish=Box::pin(fetch.finish());assert!(tokio::time::timeout(Duration::from_millis(30),&mut finish).await.is_err());
        drop(blocked);let error=tokio::time::timeout(Duration::from_secs(2),finish).await.unwrap().unwrap_err();
        assert!(store.failure().is_some(),"failure must already be observable when request admission closes");assert!(format!("{error:#}").contains("broken.group"));
        assert!(Fetch::begin_on(store.clone()).await.is_err(),"failed storage cannot admit a replacement fetch");
    });assert!(store.close().is_err());
}
