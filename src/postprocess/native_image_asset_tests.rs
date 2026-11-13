//! Test-only actual ImagesStep BODY -> Prepared -> KeepImage fixture.
//! No image association or positive asset receipt is inserted by the test.
use super::*;
use crate::pg::{indexer_runtime::Runtime,stage_queue::{Queue,Settlement}};
use std::{io::{Read,Write,BufRead,BufReader},net::{TcpListener,TcpStream},sync::atomic::{AtomicBool,AtomicUsize,Ordering},thread,time::{Duration,Instant}};
pub(crate) const IMAGE:&[u8]=b"GIF89a\x01\0\x01\0\x80\0\0\0\0\0\xff\xff\xff!\xf9\x04\x01\0\0\0\0,\0\0\0\0\x01\0\x01\0\0\x02\x02D\x01\0;";
const MID:&str="<native-image@fixture.invalid>";
struct Peer {server:crate::config::UsenetServer,stop:Arc<AtomicBool>,bodies:Arc<AtomicUsize>,worker:Option<thread::JoinHandle<()>>}
impl Peer {
    fn new()->Self {
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();listener.set_nonblocking(true).unwrap();
        let mut server=crate::config::UsenetServer::new("127.0.0.1","fixture","fixture",listener.local_addr().unwrap().port());server.ssl=Some(false);server.compress=Some(false);server.connections=Some(1);
        let stop=Arc::new(AtomicBool::new(false));let bodies=Arc::new(AtomicUsize::new(0));let (ended,count)=(stop.clone(),bodies.clone());
        let worker=thread::spawn(move|| {
            let deadline=Instant::now()+Duration::from_secs(30);let mut children=Vec::new();
            while !ended.load(Ordering::Acquire)&&Instant::now()<deadline {
                match listener.accept() {
                    Ok((stream,_))=>{assert!(children.len()<4,"fixed private NNTP connection admission");let (stop,count)=(ended.clone(),count.clone());children.push(thread::spawn(move||serve(stream,&stop,&count)));},
                    Err(error) if error.kind()==std::io::ErrorKind::WouldBlock=>thread::sleep(Duration::from_millis(2)),Err(error)=>panic!("owned NNTP accept: {error}")
                }
            }
            ended.store(true,Ordering::Release);for child in children {child.join().unwrap();}
        });Self {server,stop,bodies,worker:Some(worker)}
    }
}
impl Drop for Peer {fn drop(&mut self){self.stop.store(true,Ordering::Release);if let Some(worker)=self.worker.take(){worker.join().unwrap();}}}
fn serve(mut stream:TcpStream,stop:&AtomicBool,bodies:&AtomicUsize) {
    stream.set_read_timeout(Some(Duration::from_millis(100))).unwrap();stream.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
    let mut input=BufReader::new(stream.try_clone().unwrap());stream.write_all(b"200 owned image fixture\r\n").unwrap();
    for _ in 0..128 {
        if stop.load(Ordering::Acquire){return;}let mut line=Vec::new();
        match input.by_ref().take(4097).read_until(b'\n',&mut line) {
            Ok(0)=>return,Ok(_)=>{},Err(error) if matches!(error.kind(),std::io::ErrorKind::WouldBlock|std::io::ErrorKind::TimedOut)=>continue,Err(_)=>return
        }
        assert!(line.len()<=4096,"fixed private NNTP command admission");let line=std::str::from_utf8(&line).unwrap().trim_end();let (verb,arg)=line.split_once(' ').unwrap_or((line,""));
        let reply=match verb {
            "AUTHINFO" if arg.starts_with("USER ")=>b"381 password\r\n".to_vec(),"AUTHINFO"=>b"281 accepted\r\n".to_vec(),
            "CAPABILITIES"=>b"101 capabilities\r\nVERSION 2\r\nREADER\r\n.\r\n".to_vec(),"MODE"=>b"200 reader\r\n".to_vec(),
            "GROUP"=>format!("211 1 1 1 {arg}\r\n").into_bytes(),
            "BODY"|"ARTICLE" if arg==MID=> {
                bodies.fetch_add(1,Ordering::SeqCst);let mut reply=if verb=="BODY"{b"222 1 body\r\n".to_vec()}else{b"220 1 article\r\nFrom: fixture\r\n\r\n".to_vec()};
                reply.extend(format!("=ybegin line=128 size={} name=cover.gif\r\n",IMAGE.len()).as_bytes());
                // Same yEnc byte transform as tests/common::yenc_encode.
                for &byte in IMAGE {let value=byte.wrapping_add(42);if matches!(value,0|b'\n'|b'\r'|b'='){reply.push(b'=');reply.push(value.wrapping_add(64));}else{reply.push(value);}}
                reply.extend(format!("\r\n=yend size={}\r\n.\r\n",IMAGE.len()).as_bytes());reply
            },
            "QUIT"=>{let _=stream.write_all(b"205 bye\r\n");return;},_=>b"430 no such owned article\r\n".to_vec()
        };
        if stream.write_all(&reply).is_err(){return;}
    }
}
pub(crate) fn run(runtime:&Runtime,config:&crate::config::Config)->i64 {
    let id=runtime.owner.with(|client| {
        let saved=crate::pg::release_writer::save(client,&[crate::parser::Release {name:"Actual.ImagesStep.Release".into(),poster:"poster".into(),group:"alt.binaries.test".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
            articles:vec![crate::parser::Article {filename:Some("video.mkv".into()),subject:"video.mkv yEnc (1/1)".into(),message_id:"<unfetched-video@fixture.invalid>".into(),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()},
                crate::parser::Article {filename:Some("cover.gif".into()),subject:"cover.gif yEnc (1/1)".into(),message_id:MID.into(),part:Some(1),total_parts:Some(1),bytes:IMAGE.len() as i64,..Default::default()}],..Default::default()}],-100000)?;
        assert_eq!((saved.releases,saved.files,saved.parts),(1,2,2));Ok(client.query_one("select id from releases",&[])?.get::<_,i64>(0))
    }).unwrap();
    let peer=Peer::new();let handle=tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let step:Arc<dyn Step>=Arc::new(crate::images::step::ImagesStep::default());assert!(step.enabled(config));let bit=mask(step.id());
    let mut runner=Runner::new(&runtime.owner,config,Arc::new(crate::nntp::Pool::new(&[peer.server.clone()]).with_timeout(Duration::from_secs(2))),handle.handle().clone()).unwrap();
    runner.steps=Steps {ordered:vec![step],enabled:bit,database:0,network:bit,on_growth:bit,stamps:Vec::new()};
    let queue=Queue::for_owner(&runtime.owner).unwrap();let db=queue.claim(10,1,60).unwrap().pop().expect("actual initial DB claim");
    assert_eq!(runner.database(&db,0,10).unwrap().settle(&queue,&db,11).unwrap(),Settlement::Applied);
    let net=queue.claim_net(12,1,60,0).unwrap().pop().expect("ImagesStep must enter actual NET queue");assert_eq!(net.release_id(),id);
    let prepared=runner.network(&queue,&net,12).unwrap().expect("actual ImagesStep NET visit");assert_eq!(prepared.settle(&queue,&net,13).unwrap(),Settlement::Applied);
    assert_eq!(peer.bodies.load(Ordering::SeqCst),1,"one actual owned image BODY, no video fetch or external provider");
    runtime.owner.with(|client| {
        let row=client.query_one("select i.file,i.bytes,i.width,i.height,i.format,s.done,s.failed from release_images i join pp_state s using(release_id) where i.release_id=$1 and i.position=0",&[&id])?;
        assert_eq!(row.get::<_,String>(0),"cover.gif");assert_eq!((row.get::<_,i64>(1),row.get::<_,i64>(2),row.get::<_,i64>(3)),(IMAGE.len() as i64,1,1));assert_eq!(row.get::<_,String>(4),"gif");
        assert_eq!(row.get::<_,i64>(5)&bit,bit);assert_eq!(row.get::<_,i64>(6)&bit,0);assert_eq!(client.query_one("select count(*) from native_operations",&[])?.get::<_,i64>(0),2);Ok(())
    }).unwrap();id
}
