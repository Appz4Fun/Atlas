//! The existing isolated cache child owns this real SIGTERM/PG fixture.
use std::time::{Duration,Instant};
use crate::pg::stage_queue::{Queue,Visit,Settlement,StopDisposition};
use crate::postprocess::apply::Applied;

pub(crate) fn run(runtime:&crate::pg::indexer_runtime::Runtime,action:&str) {
    crate::pg::stage_queue_tests::push(runtime);let queue=Queue::for_owner(&runtime.owner).unwrap();
    let claims=queue.claim(10,1,60).unwrap();let db=&claims[0];
    let a=Applied {release_id:db.release_id(),set_done:4,net_mask:8,net_steps:8,net_attempts:Some(3),..Default::default()};
    assert_eq!(queue.settle(db,Visit::prepare(db,a,[1;32]).unwrap(),11).unwrap(),Settlement::Applied);
    let claim=queue.claim_net(12,1,60,0).unwrap().pop().unwrap();let id=claim.release_id();
    assert_eq!(queue.begin_net(&claim,12,900).unwrap(),crate::pg::stage_queue::NetBegin::Started {attempts:3});
    let pid:i32=runtime.owner.with(|client|Ok(client.query_one("select pg_backend_pid()",&[])?.get(0))).unwrap();
    match action {
        "net-stop-header"=>runtime.owner.with(|client|{client.execute("update files set subject='changed header' where release_id=$1",&[&id])?;Ok(())}).unwrap(),
        "net-stop-rerun"=>runtime.owner.with(|client|{let mut tx=client.transaction()?;crate::pg::stage_queue::rerun_in(&mut tx,id,8,13)?;tx.commit()?;Ok(())}).unwrap(),
        "net-stop-receipt"=>runtime.owner.with(|client| {
            client.execute("insert into native_operations(operation_id,catalog_nonce,origin_epoch,request_digest,input_digest,result_digest,outcome)
                select s.native_net_operation_id,e.catalog_nonce,e.epoch,decode(repeat('00',32),'hex'),decode(repeat('00',32),'hex'),decode(repeat('00',32),'hex'),decode('01','hex')
                from pp_state s join native_runtime_epochs e on e.epoch=s.native_net_epoch where s.release_id=$1",&[&id])?;Ok(())
        }).unwrap(),
        "net-stop"|"net-stop-owner"|"net-stop-outage"=>{},
        _=>panic!("unknown stop fixture"),
    }
    let signal=signal_hook::flag::register(signal_hook::consts::SIGTERM,runtime.owner.stop.clone()).unwrap();
    let owner=runtime.owner.clone();let (send,ready)=std::sync::mpsc::channel();
    let worker=std::thread::Builder::new().name("native-net-stop-fixture".into()).spawn(move|| {
        send.send(()).unwrap();let until=Instant::now()+Duration::from_secs(6);
        while !owner.stop.load(std::sync::atomic::Ordering::Acquire) {
            anyhow::ensure!(Instant::now()<until,"owned TERM fixture was not stopped");std::thread::sleep(Duration::from_millis(5));
        }
        Ok(Some(claim))
    }).unwrap();
    ready.recv_timeout(Duration::from_secs(1)).unwrap();
    let home=std::path::PathBuf::from(std::env::var_os("ATLAS_HOME").unwrap());let marker=home.join("native-term-ready");std::fs::write(&marker,b"ready").unwrap();
    let until=Instant::now()+Duration::from_secs(6);
    while !runtime.owner.stop.load(std::sync::atomic::Ordering::Acquire) {
        assert!(Instant::now()<until,"real SIGTERM was not received");std::thread::sleep(Duration::from_millis(5));
    }
    let storage=crate::config::checked_storage_config().unwrap();storage.check().unwrap();
    let mut observer=postgres::Client::connect(&storage.url,postgres::NoTls).unwrap();
    observer.batch_execute(&format!("set search_path to \"{}\"",storage.schema)).unwrap();
    if action=="net-stop-outage" {
        assert!(observer.query_one("select pg_terminate_backend($1)",&[&pid]).unwrap().get::<_,bool>(0));
    }
    let joined=super::join_workers(&runtime.owner,vec![worker]).unwrap();
    if action=="net-stop-owner" {
        runtime.owner.close();let until=Instant::now()+Duration::from_secs(3);
        loop {
            observer.query_one("select pg_stat_clear_snapshot()",&[]).unwrap();
            if !observer.query_one("select exists(select 1 from pg_stat_activity where pid=$1)",&[&pid]).unwrap().get::<_,bool>(0) {break}
            assert!(Instant::now()<until);std::thread::sleep(Duration::from_millis(5));
        }
        let next=crate::raw_store::native_pg_opensubtitles_cache_tests::open_test_native_owner().unwrap();
        assert!(next.owner.finalize_native_stop(&joined,13).is_err());assert_eq!(next.owner.failure(),None);next.owner.close();
    } else {
        let result=runtime.owner.finalize_native_stop(&joined,13);
        match action {
            "net-stop"=>assert_eq!(result.unwrap(),vec![StopDisposition::Restored]),
            "net-stop-header"|"net-stop-rerun"=>{assert_eq!(result.unwrap(),vec![StopDisposition::Superseded]);assert_eq!(runtime.owner.failure(),None);},
            "net-stop-receipt"=>{assert!(result.is_err());assert_eq!(runtime.owner.failure(),Some(crate::pg::outage::Failure::Fatal));},
            "net-stop-outage"=>{assert_eq!(crate::pg::outage::classify(&result.unwrap_err()),crate::pg::outage::Failure::Unavailable);assert_eq!(runtime.owner.failure(),Some(crate::pg::outage::Failure::Unavailable));},
            _=>unreachable!(),
        }
    }
    let row=observer.query_one("select net_attempts,native_net_before,net_next_at from pp_state where release_id=$1",&[&id]).unwrap();
    let expected=match action {"net-stop"=>3,"net-stop-rerun"=>0,_=>4};assert_eq!(row.get::<_,i64>(0),expected);
    if action=="net-stop" || action=="net-stop-owner" {assert_eq!(row.get::<_,Option<i64>>(1),None);} else {assert_eq!(row.get::<_,Option<i64>>(1),Some(3));}
    if action=="net-stop" {assert_eq!(row.get::<_,i64>(2),13,"intentional stop is eligible again at the stop clock");}
    std::fs::remove_file(marker).unwrap();signal_hook::low_level::unregister(signal);
}
