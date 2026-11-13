//! Typed provider completion races through the real original Owner/service
//! network bridge and the same standard worker terminal handler. Opt-in only.
use super::*;
use crate::{pg::{indexer_runtime::{Owner,Runtime},outage::Failure},spotnet::native::{Service,provider_pass},nntp::NntpError};
use std::{sync::{Arc,mpsc,atomic::Ordering},time::Duration};

fn fixture()->(crate::pg::testing::Schema,Runtime,tokio::runtime::Runtime,Arc<Service>) {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
    let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    let handle=tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let service=Arc::new(Service::new(&runtime.owner,runtime.db.clone(),crate::config::SpotsConfig {blacklist_url:String::new(),..Default::default()},
        Arc::new(crate::nntp::Pool::new(&[])),handle.handle().clone()).unwrap());
    (schema,runtime,handle,service)
}
#[derive(Clone,Copy,Debug)]
enum End {Live,Stop,Unavailable,Fatal}
fn end(owner:&Owner,kind:End) {
    match kind {End::Live=>{},End::Stop=>owner.stop.store(true,Ordering::Release),End::Unavailable=>owner.fail(Failure::Unavailable),End::Fatal=>owner.fail(Failure::Fatal)}
}
fn expected(kind:End)->Option<Failure> {
    match kind {End::Live|End::Stop=>None,End::Unavailable=>Some(Failure::Unavailable),End::Fatal=>Some(Failure::Fatal)}
}
fn no_effects(schema:&crate::pg::testing::Schema) {
    let mut observer=schema.pool.get().unwrap();
    assert_eq!(observer.query_one("select (select count(*) from releases)+(select count(*) from spots)+(select count(*) from native_operations)",&[]).unwrap().get::<_,i64>(0),0,"provider retry/cancel cannot manufacture acceptance or a receipt");
}
#[test]
#[ignore="requires isolated PG for deterministic actual-owner ready NNTP terminal races"]
fn ready_reply_and_io_never_upgrade_stop_or_unavailable_at_either_interleaving() {
    for kind in [End::Live,End::Stop,End::Unavailable,End::Fatal] {
        for after_await in [false,true] {
            for io in [false,true] {
                let (schema,runtime,_handle,service)=fixture();let owner=runtime.owner.clone();
                let (done_tx,done_rx)=mpsc::channel();
                let worker=workers::spawn(&runtime.owner,"native-ready-provider-control".into(),move|| {
                    let result=service.network_for_test::<()>(async {
                        if !after_await {end(&owner,kind);}
                        if io {Err(NntpError::Io(std::io::Error::new(std::io::ErrorKind::ConnectionReset,"owned deterministic ready transport")))}
                        else {Err(NntpError::Reply {code:431,message:"owned deterministic ready retry".into()})}
                    });
                    assert!(result.as_ref().unwrap_err().downcast_ref::<NntpError>().is_some(),"original typed provider cause survives tagging");
                    // Explicitly exercise the later caller-guard window too;
                    // a network-only post-await sample cannot close it.
                    if after_await {end(&owner,kind);}
                    let terminal=provider_pass(result);
                    assert!(matches!(terminal,Ok(None)),"{kind:?}, after_await={after_await}, io={io}");
                    assert_eq!(owner.failure(),expected(kind));done_tx.send(()).unwrap();
                    terminal?;Ok(None)
                }).unwrap();
                done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                join_workers(&runtime.owner,vec![worker]).unwrap();assert_eq!(runtime.owner.failure(),expected(kind));
                if matches!(kind,End::Fatal) {assert!(matches!(crate::pg::outage::AttemptOutcome::from_exit(0,runtime.owner.failure()),crate::pg::outage::AttemptOutcome::Fatal));}
                no_effects(&schema);runtime.owner.close();
            }
        }
    }
}
#[test]
#[ignore="requires isolated PG for actual original-owner quiet pending NNTP cancellation"]
fn quiet_pending_provider_stop_joins_without_acceptance_or_recorded_failure() {
    let (schema,runtime,_handle,service)=fixture();let (entered_tx,entered_rx)=mpsc::channel();
    let worker=workers::spawn(&runtime.owner,"native-quiet-provider-control".into(),move|| {
        let result=service.network_for_test::<()>(async {entered_tx.send(()).unwrap();std::future::pending::<crate::nntp::Result<()>>().await});
        assert!(crate::pg::outage::is_attempt_ended(result.as_ref().unwrap_err()));
        provider_pass(result)?;Ok(None)
    }).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    join_workers(&runtime.owner,vec![worker]).unwrap();assert_eq!(runtime.owner.failure(),None);
    no_effects(&schema);runtime.owner.close();
}
#[test]
#[ignore="requires isolated PG for fatal header/storage/SQL/invariant stop precedence"]
fn stopped_header_storage_sql_and_invariant_errors_remain_fatal_with_their_causes() {
    for kind in 0..4 {
        let (schema,runtime,_handle,service)=fixture();let owner=runtime.owner.clone();let (done_tx,done_rx)=mpsc::channel();
        let worker=workers::spawn(&runtime.owner,"native-fatal-provider-control".into(),move|| {
            let result:Result<()>=match kind {
                0=>service.network_for_test(async {owner.stop.store(true,Ordering::Release);Err(NntpError::HeaderStore("owned storage failure".into()))}),
                1=>{owner.stop.store(true,Ordering::Release);Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,"owned local storage failure").into())},
                2=>owner.with(|client|{client.batch_execute("select 1/0")?;Ok(())}),
                _=>{owner.stop.store(true,Ordering::Release);Err(anyhow::anyhow!("native Spotnet provider failure"))},
            };
            let terminal=provider_pass(result);assert!(terminal.is_err());done_tx.send(()).unwrap();terminal?;Ok(None)
        }).unwrap();
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();let error=match join_workers(&runtime.owner,vec![worker]) {Ok(_)=>panic!("fatal cause must not become provider retry"),Err(error)=>error};
        assert_eq!(runtime.owner.failure(),Some(Failure::Fatal));
        match kind {
            0=>assert!(matches!(error.downcast_ref::<NntpError>(),Some(NntpError::HeaderStore(_)))),
            1=>assert_eq!(error.downcast_ref::<std::io::Error>().unwrap().kind(),std::io::ErrorKind::PermissionDenied),
            2=>assert!(error.chain().any(|cause|cause.downcast_ref::<postgres::Error>().is_some_and(|pg|pg.code()==Some(&postgres::error::SqlState::DIVISION_BY_ZERO)))),
            _=>assert!(error.to_string().contains("native Spotnet provider failure")),
        }
        no_effects(&schema);runtime.owner.close();
    }
}
