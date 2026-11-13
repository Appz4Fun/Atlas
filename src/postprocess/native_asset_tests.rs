//! Real cached ProbeStep outcomes cross the same Runner/Prepared boundary as NET workers.
use super::*;
use std::{net::TcpListener,sync::{atomic::Ordering,mpsc},time::Duration};
use sha2::{Digest,Sha256};
use crate::pg::stage_queue::{Queue,Settlement};
use crate::raw_store::native_pg_opensubtitles_cache_tests::{native_worker_parent,WorkerCase};

#[test]
fn native_probe_asset_frame_reuses_verified_bytes_without_body_and_settles_once() {
    native_worker_parent(WorkerCase::AssetReuse);
}
#[test]
fn native_probe_asset_frame_refuses_a_real_rerun_after_capture() {
    native_worker_parent(WorkerCase::AssetStale);
}
#[test]
fn native_probe_asset_frame_drains_before_owner_close_without_a_late_ack() {
    native_worker_parent(WorkerCase::AssetDrain);
}
#[test]
fn native_probe_asset_frame_missing_selection_file_and_bad_hash_never_fetch_or_complete() {
    for case in [WorkerCase::AssetUnselected,WorkerCase::AssetMissing,WorkerCase::AssetCorrupt] {
        native_worker_parent(case);
    }
}

#[test]
fn native_probe_asset_missing_bytes_settle_only_a_pending_retry_without_done_or_provider_work() {
    for case in [WorkerCase::AssetWaitUnselected,WorkerCase::AssetWaitMissing] {native_worker_parent(case);}
}

fn element(id:&[u8],body:&[u8])->Vec<u8> {
    assert!(body.len()<0x3fff);
    [id,&[0x40|((body.len()>>8) as u8),body.len() as u8],body].concat()
}

fn saved_media()->(crate::probe::ProbeInfo,Vec<u8>) {
    let jpeg=[&[0xff,0xd8,0xff,0xe0][..],&[7;100],&[0xff,0xd9]].concat();
    let fields=[element(&[0x46,0x6e],b"cover.jpg"),element(&[0x46,0x60],b"image/jpeg"),element(&[0x46,0x5c],&jpeg)].concat();
    let attachments=element(&[0x19,0x41,0xa4,0x69],&element(&[0x61,0xa7],&fields));
    let segment=[attachments,element(&[0x1f,0x43,0xb6,0x75],&[])].concat();
    let bytes=[element(&[0x1a,0x45,0xdf,0xa3],&element(&[0x42,0x82],b"matroska")),element(&[0x18,0x53,0x80,0x67],&segment)].concat();
    let media=crate::probe::media::parse(&bytes).unwrap();
    assert_eq!(media.cover.as_ref().unwrap().bytes,jpeg);
    assert!(media.header_complete&&media.attachments_complete);
    (crate::probe::ProbeInfo {file:Some("sample.bin".into()),media:Some(media),..Default::default()},jpeg)
}

pub(crate) fn run(runtime:&crate::pg::indexer_runtime::Runtime,action:&str) {
    // The parent supplies a real native-selected private home and a raw-cache
    // sentinel. This function has no direct SQLite path or replacement owner.
    crate::pg::stage_queue_tests::push(runtime);
    let (probe,bytes)=saved_media();let json=serde_json::to_string(&probe).unwrap();
    let root=tempfile::tempdir().unwrap();let evidence=tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("covers")).unwrap();
    let sha=crate::pg::fresh_assets::hex(&Sha256::digest(&bytes));
    let file=root.path().join("covers").join(&sha);std::fs::write(&file,&bytes).unwrap();
    let manifest=evidence.path().join("assets.jsonl");
    crate::pg::cache_assets::capture_file(root.path(),&manifest,crate::pg::cache_assets::Limits {min_files:1,max_files:16,max_file_bytes:1<<20,max_total_bytes:16<<20,read_bytes_per_second:32<<20}).unwrap();
    let selection=crate::config::NativeAssetsConfig {root:root.path().canonicalize().unwrap().to_str().unwrap().into(),manifest:manifest.canonicalize().unwrap().to_str().unwrap().into(),whole_sha256:crate::pg::fresh_assets::hex(&Sha256::digest(std::fs::read(&manifest).unwrap()))};
    let step:Arc<dyn Step>=Arc::new(crate::probe::step::ProbeStep::default());let bit=mask(step.id());let stamp=step.config_stamp();
    let id=runtime.owner.with(|client| {
        let row=client.query_one("select id,original_name,group_name,parts,complete from releases",&[])?;
        let id:i64=row.try_get(0)?;let name=String::from_utf8(row.try_get(1)?)?;let group:String=row.try_get(2)?;
        let content=crate::pg::content_id::read(client,id,&runtime.owner.stop)?.unwrap();
        let mut tx=client.transaction()?;
        crate::pg::natural_cache::put_probe(&mut tx,&crate::raw_store::ProbeRecord {name:&name,group_name:&group,content_id:&content,crc32:None,agent:None,info:&json,moviehash:None,parts:row.try_get(3)?,complete:row.try_get(4)?,config_stamp:stamp,nfo_password:None,cover:Some(("image/jpeg",&sha))},crate::pg::natural_cache::Budget {bytes:1<<20,rows:8})?;
        tx.commit()?;Ok(id)
    }).unwrap();
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();listener.set_nonblocking(true).unwrap();
    let mut server=crate::config::UsenetServer::new("127.0.0.1","fixture","fixture",listener.local_addr().unwrap().port());server.ssl=Some(false);
    let handle=tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let mut config=crate::config::Config::default();config.predb.enabled=false;
    if !matches!(action,"worker-asset-unselected"|"worker-asset-wait-unselected") {config.native_assets=Some(selection);}
    let mut runner=Runner::new(&runtime.owner,&config,Arc::new(crate::nntp::Pool::new(&[server]).with_timeout(Duration::from_millis(100))),handle.handle().clone()).unwrap();
    assert!(step.enabled(&config));
    runner.steps=Steps {ordered:vec![step],enabled:bit,database:0,network:bit,on_growth:bit,stamps:vec![(crate::probe::step::PROBE_STEP.0,bit,stamp as i64)]};
    let queue=Queue::for_owner(&runtime.owner).unwrap();let db=queue.claim(10,1,60).unwrap().pop().unwrap();
    assert_eq!(runner.database(&db,0,10).unwrap().settle(&queue,&db,11).unwrap(),Settlement::Applied);
    let claim=queue.claim_net(12,1,60,0).unwrap().pop().unwrap();assert_eq!(claim.release_id(),id);
    let storage=crate::config::checked_storage_config().unwrap();
    let mut observer=postgres::Client::connect(&storage.url,postgres::NoTls).unwrap();observer.batch_execute(&format!("set search_path to \"{}\"",storage.schema)).unwrap();
    if matches!(action,"worker-asset-missing"|"worker-asset-wait-missing") {std::fs::remove_file(&file).unwrap();}
    if action=="worker-asset-corrupt" {std::fs::write(&file,vec![0;bytes.len()]).unwrap();}
    if action=="worker-asset-drain" {
        let (ready_tx,ready_rx)=mpsc::channel();let (release_tx,release_rx)=mpsc::channel();
        let owner=runtime.owner.clone();let worker:Worker=std::thread::spawn(move|| {
            let prepared=runner.network(&queue,&claim,12)?.expect("genuine cached NET visit");
            assert_eq!(prepared.holds.len(),1);assert!(prepared.holds[0].is::<crate::pg::worker_assets::CoverHold>());
            assert_eq!(runner.stats.bodies.load(Ordering::Relaxed),0);
            ready_tx.send(()).unwrap();release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            let error=prepared.settle(&queue,&claim,13).unwrap_err();assert!(crate::pg::outage::is_attempt_ended(&error));
            assert_eq!(owner.failure(),None);Ok(Some(claim))
        });
        ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let owner=runtime.owner.clone();let (done_tx,done_rx)=mpsc::channel();
        let finish=std::thread::spawn(move||{done_tx.send(join_workers(&owner,vec![worker])).unwrap();});
        let until=std::time::Instant::now()+Duration::from_secs(3);
        while !runtime.owner.stop.load(Ordering::Acquire) {assert!(std::time::Instant::now()<until);std::thread::yield_now();}
        assert!(matches!(done_rx.recv_timeout(Duration::from_millis(30)),Err(mpsc::RecvTimeoutError::Timeout)),"drain must retain the worker and its actual cover hold");
        std::fs::remove_file(&file).unwrap();release_tx.send(()).unwrap();
        let joined=done_rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();assert!(joined.for_owner(&runtime.owner));finish.join().unwrap();
        assert_eq!(runtime.owner.failure(),None);runtime.owner.close();
    } else {
        let result=runner.network(&queue,&claim,12);
        assert_eq!(runner.stats.bodies.load(Ordering::Relaxed),0);
        if action=="worker-asset-corrupt" {
            assert!(result.is_err(),"a corrupt physical asset must preserve its error");
            assert_eq!(runtime.owner.failure(),Some(crate::pg::outage::Failure::Fatal));
        } else {
            let prepared=result.unwrap().expect("actual cached-probe visit");
            if matches!(action,"worker-asset-wait-unselected"|"worker-asset-wait-missing") {
                assert!(prepared.holds.is_empty());
                assert_eq!(prepared.settle(&queue,&claim,13).unwrap(),Settlement::Applied);
                let row=observer.query_one("select done,net_wanted,net_left,net_attempts,net_transient,net_next_at from pp_state where release_id=$1",&[&id]).unwrap();
                assert_eq!(row.get::<_,i64>(0)&bit,0);assert_eq!(row.get::<_,Option<bool>>(1),Some(true));assert_eq!(row.get::<_,Option<i64>>(2),Some(bit));
                assert_eq!(row.get::<_,i64>(3),0);assert_eq!(row.get::<_,i64>(4),1);assert!(row.get::<_,i64>(5)>13);
                assert_eq!(runtime.owner.failure(),None);
            } else if matches!(action,"worker-asset-unselected"|"worker-asset-missing") {
                assert!(prepared.holds.is_empty());
                runtime.owner.stop.store(true,Ordering::Release);drop(prepared);assert_eq!(runtime.owner.failure(),None);
            } else {
                assert_eq!(prepared.holds.len(),1);assert!(prepared.holds[0].is::<crate::pg::worker_assets::CoverHold>());
                if action=="worker-asset-stale" {
                    runtime.owner.with(|client|{let mut tx=client.transaction()?;crate::pg::stage_queue::rerun_in(&mut tx,id,bit,13)?;tx.commit()?;Ok(())}).unwrap();
                    assert_eq!(prepared.settle(&queue,&claim,14).unwrap(),Settlement::Superseded);assert_eq!(runtime.owner.failure(),None);
                } else {
                    assert_eq!(action,"worker-asset-reuse");
                    std::fs::write(&file,vec![0;bytes.len()]).unwrap();
                    assert_eq!(prepared.settle(&queue,&claim,13).unwrap(),Settlement::Applied);
                    let row=observer.query_one("select done,net_attempts from pp_state where release_id=$1",&[&id]).unwrap();
                    assert_eq!(row.get::<_,i64>(0)&bit,bit);assert_eq!(row.get::<_,i64>(1),0);
                    assert_eq!(observer.query_one("select mime,sha256 from release_cover where release_id=$1",&[&id]).unwrap().get::<_,String>(1),sha);
                    assert_eq!(observer.query_one("select count(*) from native_operations",&[]).unwrap().get::<_,i64>(0),2);
                }
            }
        }
    }
    if action!="worker-asset-reuse" {
        assert_eq!(observer.query_one("select done from pp_state where release_id=$1",&[&id]).unwrap().get::<_,i64>(0)&bit,0);
        assert_eq!(observer.query_one("select count(*) from release_cover",&[]).unwrap().get::<_,i64>(0),0);
        assert_eq!(observer.query_one("select count(*) from release_probe",&[]).unwrap().get::<_,i64>(0),0);
        let receipts=if matches!(action,"worker-asset-wait-unselected"|"worker-asset-wait-missing") {2}else{1};
        assert_eq!(observer.query_one("select count(*) from native_operations",&[]).unwrap().get::<_,i64>(0),receipts,"normal wait records only the retry; canceled frames retain only DB scheduling");
    }
    assert!(matches!(listener.accept(),Err(error) if error.kind()==std::io::ErrorKind::WouldBlock),"cache reuse/refusal attempted a provider connection");
    runtime.owner.close();
}
