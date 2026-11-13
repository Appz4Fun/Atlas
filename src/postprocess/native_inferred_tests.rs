//! Real accepted core/own HEAD peer through the production retained lane.
use super::*;
use std::{sync::mpsc,time::{Duration,Instant}};
#[test]
#[ignore="requires isolated PostgreSQL, actual NXV Store and owned held HEAD for joined inferred lifecycle"]
fn joined_inferred_lane_cancels_actual_head_before_owner_and_store_close() {
    let (schema,runtime,_root,store,headers)=crate::inferred::native::tests::fixture();
    let now=chrono::Utc::now().timestamp();crate::inferred::native::tests::seed(&runtime,now,&[1,2,4]);
    let (entered_tx,entered_rx)=mpsc::channel();let (release_tx,release_rx)=mpsc::channel();
    let peer=crate::inferred::native::tests::peer(now,false,Some((entered_tx,release_rx)));
    let net=crate::inferred::native::tests::net();
    assert_eq!(runtime.configured_headers(),headers,"use the captured ORIGINAL base, not an already qualified catalog root");
    let mut service=crate::inferred::native::Service::new(&runtime.owner,&runtime.db,
        Arc::new(crate::nntp::Pool::new(&[peer.config.clone()])),store.clone(),&headers).unwrap();
    service.start_without_delay_for_test();
    struct Drain<'a> {owner:&'a Arc<crate::pg::indexer_runtime::Owner>,handles:Option<Vec<super::Worker>>}
    impl Drop for Drain<'_> {fn drop(&mut self) {if let Some(handles)=self.handles.take() {let _=join_workers(self.owner,handles);}}}
    let mut drain=Drain {owner:&runtime.owner,handles:Some(Vec::new())};
    workers::start_inferred(&runtime.owner,service,net.handle().clone(),drain.handles.as_mut().unwrap()).unwrap();
    assert_eq!(drain.handles.as_ref().unwrap().len(),1);entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    runtime.owner.with(|client| {assert_eq!(client.query_one("select count(*) from segments",&[])?.get::<_,i64>(0),3);Ok(())}).unwrap();
    let before=peer.hits.load(std::sync::atomic::Ordering::Acquire);assert!(before>0);
    let started=Instant::now();join_workers(&runtime.owner,drain.handles.take().unwrap()).unwrap();
    assert!(started.elapsed()<Duration::from_secs(5),"join must cancel and await actual HEAD leases, not wait for provider or the300s cadence");
    assert_eq!(peer.hits.load(std::sync::atomic::Ordering::Acquire),before);assert_eq!(runtime.owner.failure(),None);
    let mut observer=schema.pool.get().unwrap();
    assert_eq!(observer.query_one("select count(*) from segments",&[]).unwrap().get::<_,i64>(0),3);
    assert_eq!(observer.query_one("select count(*) from inferred_ids",&[]).unwrap().get::<_,i64>(0),0,"unfinished HEAD cannot gain Found acknowledgement");
    release_tx.send(()).unwrap();runtime.owner.close();store.close().unwrap();
}
