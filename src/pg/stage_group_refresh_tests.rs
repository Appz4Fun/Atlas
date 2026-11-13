//! Owned NNTP EOF and the real canonical owner drive the refresh controls.
use super::*;
use std::{io::{BufRead,Write,BufReader},net::TcpListener,sync::{Mutex,atomic::AtomicUsize},thread};
use crate::nntp::group_capture::provider_binding;

#[derive(Clone,Copy)]
enum Reply {Complete,Denied,Truncated,Quiet}
struct Provider {config:crate::config::UsenetServer,stop:Arc<AtomicBool>,commands:Arc<Mutex<Vec<String>>>,lists:Arc<AtomicUsize>,thread:Option<thread::JoinHandle<()>>}
impl Provider {
    fn new(script:Vec<Reply>)->Self {
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();listener.set_nonblocking(true).unwrap();
        let mut config=crate::config::UsenetServer::new("127.0.0.1","refresh-account","fixture-password",listener.local_addr().unwrap().port());config.ssl=Some(false);config.compress=Some(false);config.connections=Some(1);
        let stop=Arc::new(AtomicBool::new(false));let end=stop.clone();let commands=Arc::new(Mutex::new(Vec::new()));let logged=commands.clone();let lists=Arc::new(AtomicUsize::new(0));let visits=lists.clone();
        let thread=thread::spawn(move|| {
            while !end.load(Ordering::Acquire) {
                let (socket,_)=match listener.accept() {Ok(pair)=>pair,Err(error) if error.kind()==std::io::ErrorKind::WouldBlock=>{thread::sleep(Duration::from_millis(5));continue},Err(error)=>panic!("{error}")};
                socket.set_read_timeout(Some(Duration::from_millis(50))).unwrap();socket.set_write_timeout(Some(Duration::from_secs(1))).unwrap();let mut socket=BufReader::new(socket);socket.get_mut().write_all(b"200 fixture\r\n").unwrap();
                while !end.load(Ordering::Acquire) {
                    let mut line=String::new();match socket.read_line(&mut line) {Ok(0)=>break,Ok(_)=>{},Err(error) if matches!(error.kind(),std::io::ErrorKind::WouldBlock|std::io::ErrorKind::TimedOut)=>continue,Err(_)=>break};
                    logged.lock().unwrap().push(line.trim_end().into());
                    if line.starts_with("AUTHINFO USER ") {socket.get_mut().write_all(b"281 accepted\r\n").unwrap();continue}
                    assert_eq!(line,"LIST ACTIVE\r\n");let n=visits.fetch_add(1,Ordering::AcqRel);let reply=script.get(n).copied().expect("refresh performed unexpected provider work");
                    match reply {
                        Reply::Complete=>socket.get_mut().write_all(b"215 follows\r\nalt.binaries.real 40 10 y\r\nalt.binaries.empty 0 1 y\r\n.\r\n").unwrap(),
                        Reply::Denied=>{let _=socket.get_mut().write_all(b"502 refused\r\n");},
                        Reply::Truncated=>{let _=socket.get_mut().write_all(b"215 follows\r\nalt.binaries.partial 20 10 y\r\n");break},
                        Reply::Quiet=>{socket.get_mut().write_all(b"215 follows\r\n").unwrap();while !end.load(Ordering::Acquire) {let mut byte=[0;1];match std::io::Read::read(&mut socket,&mut byte) {Ok(0)|Ok(_)=>break,Err(error) if matches!(error.kind(),std::io::ErrorKind::WouldBlock|std::io::ErrorKind::TimedOut)=>continue,Err(_)=>break}};break},
                    }
                }
            }
        });Self {config,stop,commands,lists,thread:Some(thread)}
    }
}
impl Drop for Provider {fn drop(&mut self){self.stop.store(true,Ordering::Release);self.thread.take().unwrap().join().unwrap();}}
fn current(runtime:&crate::pg::indexer_runtime::Runtime)->(i64,i64,i64) {
    runtime.owner.with(|client|Ok({let row=client.query_one("select h.capture_id,c.input_rows,(select count(*) from native_group_captures) from native_group_capture_current h join native_group_captures c on c.id=h.capture_id",&[])?;(row.get(0),row.get(1),row.get(2))})).unwrap()
}

fn completed_clock(runtime:&crate::pg::indexer_runtime::Runtime)->i64 {
    runtime.owner.with(|client|Ok(client.query_one("select c.captured_at from native_group_capture_current h join native_group_captures c on c.id=h.capture_id order by c.id desc limit 1",&[])?.get(0))).unwrap()
}

#[test]
#[ignore="requires explicit owned PG and loopback NNTP refresh admission"]
fn native_list_refresh_keeps_prior_eof_on_denial_truncation_timeout_and_reuses_the_permit() {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    let provider=Provider::new(vec![Reply::Complete,Reply::Denied,Reply::Truncated,Reply::Quiet,Reply::Complete]);
    let handle=tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let mut refresh=Refresh::new(&runtime.owner,Arc::new(crate::nntp::Pool::new(std::slice::from_ref(&provider.config))),handle.handle().clone()).unwrap();
    refresh.limits.deadline=Duration::from_millis(250);let instant=Instant::now();let wall=chrono::Utc::now().timestamp();
    assert!(matches!(refresh.tick_at(wall,instant).unwrap(),Tick::Stored(_)));let first=current(&runtime);assert_eq!((first.1,first.2),(2,1));
    let wall=completed_clock(&runtime);
    for phase in 1..=3 {
        assert!(matches!(refresh.tick_at(wall+phase*601,instant+Duration::from_secs(phase as u64*601)).unwrap(),Tick::ProviderUnavailable(_)));
        assert_eq!(current(&runtime),first,"unsuccessful wire input must not change the completed capture");
    }
    assert!(matches!(refresh.tick_at(wall+2404,instant+Duration::from_secs(2404)).unwrap(),Tick::Stored(_)));
    let last=current(&runtime);assert_ne!(last.0,first.0);assert_eq!((last.1,last.2),(2,1),"refresh retains one actual completed capture, not a growing archive");
    assert!(matches!(refresh.tick_at(wall+2405,instant+Duration::from_secs(2405)).unwrap(),Tick::Idle));assert_eq!(provider.lists.load(Ordering::Acquire),5);
    assert!(provider.commands.lock().unwrap().iter().any(|line|line=="AUTHINFO USER refresh-account"));
    runtime.owner.with(|client| {let binding=provider_binding(&provider.config)?;assert_eq!(client.query_one("select provider_binding from native_group_captures",&[])?.get::<_,Vec<u8>>(0).as_slice(),binding.as_slice());Ok(())}).unwrap();runtime.owner.close();
}

#[test]
#[ignore="requires explicit owned PG and loopback NNTP cancellation admission"]
fn native_list_refresh_stops_unfinished_input_and_replacement_account_cannot_reuse_old_scope() {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    let provider=Provider::new(vec![Reply::Complete,Reply::Complete,Reply::Quiet]);
    let handle=tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();let instant=Instant::now();let wall=chrono::Utc::now().timestamp();
    let mut first=Refresh::new(&runtime.owner,Arc::new(crate::nntp::Pool::new(std::slice::from_ref(&provider.config))),handle.handle().clone()).unwrap();
    assert!(matches!(first.tick_at(wall,instant).unwrap(),Tick::Stored(_)));
    drop(first);
    let mut changed=provider.config.clone();changed.username="another-account".into();
    let mut second=Refresh::new(&runtime.owner,Arc::new(crate::nntp::Pool::new(&[changed])),handle.handle().clone()).unwrap();
    assert!(matches!(second.tick_at(wall,instant).unwrap(),Tick::Stored(_)),"same endpoint but different actual account needs its own EOF");
    let mut observer=postgres::Client::connect(&schema.url,postgres::NoTls).unwrap();observer.batch_execute(&format!("set search_path to \"{}\"",schema.name)).unwrap();
    let before:i64=observer.query_one("select count(*) from native_group_captures",&[]).unwrap().get(0);assert_eq!(before,2);
    let wall=completed_clock(&runtime);
    let visits=provider.lists.clone();let owner=runtime.owner.clone();let cancel=thread::spawn(move|| {
        let end=Instant::now()+Duration::from_secs(3);while visits.load(Ordering::Acquire)<3 {assert!(Instant::now()<end);thread::sleep(Duration::from_millis(5));}owner.stop.store(true,Ordering::Release);
    });
    let error=second.tick_at(wall+601,instant+Duration::from_secs(601)).err().expect("stop cannot publish partial input");cancel.join().unwrap();assert!(crate::pg::outage::is_attempt_ended(&error));assert_eq!(runtime.owner.failure(),None);
    assert_eq!(observer.query_one("select count(*) from native_group_captures",&[]).unwrap().get::<_,i64>(0),before,"unfinished LIST input cannot append a header/head");runtime.owner.close();
}

#[test]
#[ignore="requires explicit owned PG and loopback NNTP commit refusal admission"]
fn native_list_refresh_pg_failure_rolls_back_new_data_and_preserves_the_real_driver_cause() {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    let provider=Provider::new(vec![Reply::Complete,Reply::Complete]);
    let handle=tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let mut refresh=Refresh::new(&runtime.owner,Arc::new(crate::nntp::Pool::new(std::slice::from_ref(&provider.config))),handle.handle().clone()).unwrap();
    let instant=Instant::now();let wall=chrono::Utc::now().timestamp();assert!(matches!(refresh.tick_at(wall,instant).unwrap(),Tick::Stored(_)));let first=current(&runtime);let wall=completed_clock(&runtime);
    // Existing completed data stays intact. Only the next real INSERT fails;
    // NOT VALID does not fabricate or edit an accepted prior capture.
    runtime.owner.with(|client| {client.batch_execute("alter table native_group_captures add constraint fixture_new_capture_refused check(input_rows<2) not valid")?;Ok(())}).unwrap();
    let mut observer=postgres::Client::connect(&schema.url,postgres::NoTls).unwrap();observer.batch_execute(&format!("set search_path to \"{}\"",schema.name)).unwrap();
    let error=refresh.tick_at(wall+601,instant+Duration::from_secs(601)).err().expect("PG write refusal is never a provider miss");
    assert!(error.chain().any(|cause|cause.downcast_ref::<postgres::Error>().is_some()));assert_eq!(runtime.owner.failure(),Some(crate::pg::outage::Failure::Fatal));
    assert_eq!(observer.query_one("select capture_id from native_group_capture_current",&[]).unwrap().get::<_,i64>(0),first.0);
    assert_eq!(observer.query_one("select count(*) from native_group_captures",&[]).unwrap().get::<_,i64>(0),1);
    assert_eq!(provider.lists.load(Ordering::Acquire),2);runtime.owner.close();
}
