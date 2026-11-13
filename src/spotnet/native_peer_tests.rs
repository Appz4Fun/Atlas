//! Owned loopback NNTP controls. Constructed only by explicit ignored PG cases.
use super::*;
use std::{io::{BufRead,BufReader,Write},net::{TcpListener,TcpStream},sync::{Arc,Mutex,atomic::{AtomicBool,Ordering}},time::{Duration,Instant}};

const ID:&str="vKCGetIwSd0z2PcagDlUt@spot.net";
const TITLE:&str="Hitster.S01E01.DUTCH.1080p.WEB.h264-TRIPEL";
// Unchanged genuinely verified sample in sign.rs; no test-only trust switch.
const FROM:&str="Wasbeertje <r2ceRUXK82cxSlhCLnbGXUVIA6-srpbJJqYqtJfXLdubdAMMD6T0G-pcPdZe619uuF.Q4LGk2NHIuwiRJIi7V6sGnlCa0V0j4gScJAnpUHO8wU3fqtPFnYmKvzMNSIhlNPI@17a09b04d11b11c06c11z01.1651448731.20.1792828369.1.NL.Pi0aiE0W9LUv0UOMnVl8aFaUMKMM-sjxZ5m9yv2nrJeDHuWKO-pr-pvPfrvqR8tp9CY>";
const COMMENT:&str="vKCGetIwSd0z2PcagDlUt.8.one.two@spot.net";
const XML:&str="<Spotnet><Posting><Key>1</Key><Created>1792828369</Created><Category>0<SubCat>01z01</SubCat></Category><Title>Hitster.S01E01.DUTCH.1080p.WEB.h264-TRIPEL</Title><Poster>Actual Spot Poster</Poster><Description>Password is supplied: alpha&#0;beta</Description><NZB><Segment>nzb@fixture.test</Segment></NZB></Posting></Spotnet>";
#[derive(Clone,Copy)]
pub(crate) enum Mode {
    Good, Forged, Missing, Transient, Held, RejectGroup,
    PayloadBlocked, PayloadNoGroups, PayloadWrongMid, PayloadGroupMismatch,
    PayloadGroupConflict, PayloadTooBig, PayloadSecondBlocked, PayloadInvalidMid,
    PayloadTransient, PayloadMissing, PayloadRevoked,
}
pub(crate) struct Peer {
    pub(crate) server:crate::config::UsenetServer,stop:Arc<AtomicBool>,release:Arc<AtomicBool>,body_entered:Arc<AtomicBool>,
    log:Arc<Mutex<Vec<String>>>,worker:Option<std::thread::JoinHandle<()>>,
}
impl Peer {
    pub(crate) fn new(mode:Mode)->Self {
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();listener.set_nonblocking(true).unwrap();
        let mut server=crate::config::UsenetServer::new("127.0.0.1","fixture","fixture",listener.local_addr().unwrap().port());
        server.ssl=Some(false);server.compress=Some(false);server.connections=Some(4);
        let stop=Arc::new(AtomicBool::new(false));let release=Arc::new(AtomicBool::new(false));let body_entered=Arc::new(AtomicBool::new(false));let log=Arc::new(Mutex::new(Vec::new()));
        let (ended,allowed,entered,commands)=(stop.clone(),release.clone(),body_entered.clone(),log.clone());
        let worker=std::thread::spawn(move|| {
            let mut children=Vec::new();let deadline=Instant::now()+Duration::from_secs(30);
            while !ended.load(Ordering::Acquire) && Instant::now()<deadline {
                match listener.accept() {
                    Ok((stream,_))=> {
                        assert!(children.len()<32,"owned NNTP connection bound");
                        let (ended,allowed,entered,commands)=(ended.clone(),allowed.clone(),entered.clone(),commands.clone());
                        children.push(std::thread::spawn(move||serve(stream,mode,&ended,&allowed,&entered,&commands)));
                    }
                    Err(e) if e.kind()==std::io::ErrorKind::WouldBlock=>std::thread::sleep(Duration::from_millis(2)),
                    Err(e)=>panic!("owned NNTP accept: {e}"),
                }
            }
            ended.store(true,Ordering::Release);
            for child in children {child.join().unwrap();}
        });
        Self {server,stop,release,body_entered,log,worker:Some(worker)}
    }
    pub(crate) fn wait_body(&self) {
        let deadline=Instant::now()+Duration::from_secs(5);
        while !self.body_entered.load(Ordering::Acquire) {assert!(Instant::now()<deadline,"actual BODY not entered");std::thread::sleep(Duration::from_millis(2));}
    }
    pub(crate) fn count(&self,verb:&str)->usize {self.log.lock().unwrap().iter().filter(|line|line.starts_with(verb)).count()}
}
impl Drop for Peer {
    fn drop(&mut self) {self.stop.store(true,Ordering::Release);self.release.store(true,Ordering::Release);if let Some(worker)=self.worker.take(){worker.join().unwrap();}}
}
fn serve(mut stream:TcpStream,mode:Mode,stop:&AtomicBool,release:&AtomicBool,entered:&AtomicBool,log:&Mutex<Vec<String>>) {
    stream.set_read_timeout(Some(Duration::from_millis(50))).unwrap();stream.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
    if stream.write_all(b"200 owned NNTP fixture\r\n").is_err(){return}
    let mut reader=BufReader::new(stream.try_clone().unwrap());let mut group=String::new();
    for _ in 0..256 {
        if stop.load(Ordering::Acquire){return}let mut line=String::new();
        match reader.read_line(&mut line) {
            Ok(0)=>return,Ok(_)=>{},
            Err(e) if matches!(e.kind(),std::io::ErrorKind::TimedOut|std::io::ErrorKind::WouldBlock)=>continue,
            Err(_)=>return,
        }
        assert!(line.len()<=8192,"owned command length bound");let line=line.trim_end().to_owned();
        {let mut log=log.lock().unwrap();assert!(log.len()<4096,"owned command count bound");log.push(line.clone());}
        let verb=line.split_whitespace().next().unwrap_or("");
        let reply=match verb {
            "AUTHINFO" if line.contains(" USER ")=>b"381 password\r\n".to_vec(),
            "AUTHINFO"=>b"281 accepted\r\n".to_vec(),
            "MODE"=>b"200 reader\r\n".to_vec(),
            "CAPABILITIES"=>b"101 capabilities\r\nVERSION 2\r\nREADER\r\n.\r\n".to_vec(),
            "GROUP"=> {
                group=line.split_whitespace().nth(1).unwrap_or("").to_owned();
                if matches!(mode,Mode::RejectGroup) {b"411 absent group\r\n".to_vec()} else {format!("211 1 1 1 {group}\r\n").into_bytes()}
            }
            "XOVER"|"OVER"=> {
                let (subject,from,id,references)=match group.as_str() {
                    SPOT_GROUP=>(TITLE,FROM,if matches!(mode,Mode::Forged){"forged@spot.net"}else{ID},""),
                    comments::COMMENT_GROUP=>("Comment","Reader <r@example.test>",COMMENT,""),
                    _=>("REPORT <vKCGetIwSd0z2PcagDlUt@spot.net>","Reader <r@example.test>","report@spot.net","<vKCGetIwSd0z2PcagDlUt@spot.net>"),
                };
                format!("224 overview\r\n1\t{subject}\t{from}\tSat, 24 Oct 2026 12:00:00 +0000\t<{id}>\t{references}\t100\t1\r\n.\r\n").into_bytes()
            }
            "HEAD" if line.contains("actual-part@fixture.test")||line.contains("actual-second@fixture.test")=> {
                if matches!(mode,Mode::PayloadMissing) {b"430 No Such Article\r\n".to_vec()}
                else if matches!(mode,Mode::PayloadTransient) {b"431 temporarily unavailable\r\n".to_vec()} else {
                    let requested=line.strip_prefix("HEAD ").unwrap();
                    let revoked=matches!(mode,Mode::PayloadRevoked)&&log.lock().unwrap().iter()
                        .filter(|line|line.starts_with("HEAD ")&&line.contains("actual-part@fixture.test")).count()>1;
                    let blocked=matches!(mode,Mode::PayloadBlocked)||revoked
                        ||(matches!(mode,Mode::PayloadSecondBlocked)&&line.contains("actual-second@fixture.test"));
                    let mid=if matches!(mode,Mode::PayloadWrongMid) {"<wrong@fixture.test>"} else {requested};
                    let groups=if blocked {"Newsgroups: alt.binaries.preteen\r\nXref: fixture alt.binaries.preteen:1\r\n"}
                        else if matches!(mode,Mode::PayloadNoGroups) {""}
                        else if matches!(mode,Mode::PayloadGroupMismatch) {"Newsgroups: another.allowed.group\r\n"}
                        else if matches!(mode,Mode::PayloadGroupConflict) {"Newsgroups: example.spot\r\nXref: fixture another.allowed.group:1\r\n"}
                        else {"Newsgroups: example.spot\r\nXref: fixture example.spot:1\r\n"};
                    let mut head=format!("221 1 {requested}\r\nMessage-ID: {mid}\r\n{groups}").into_bytes();
                    if matches!(mode,Mode::PayloadTooBig) {head.extend(b"X-Large: ");head.extend(vec![b'x';70*1024]);head.extend(b"\r\n");}
                    head.extend(b".\r\n");head
                }
            }
            "HEAD"=>format!("221 1 <{ID}>\r\nSubject: {TITLE}\r\nFrom: {FROM}\r\nX-XML: {XML}\r\n.\r\n").into_bytes(),
            "BODY" if line.contains(COMMENT)=> {
                let mut reply=b"222 body\r\nPassword: alpha\0beta\r\nKeep every word\r\n.\r\n".to_vec();
                // This is original supplied text, not a password guess.
                reply.shrink_to_fit();reply
            }
            "BODY"=> {
                entered.store(true,Ordering::Release);
                if matches!(mode,Mode::Held) {
                    let deadline=Instant::now()+Duration::from_secs(10);
                    while !release.load(Ordering::Acquire) && !stop.load(Ordering::Acquire) && Instant::now()<deadline {std::thread::sleep(Duration::from_millis(2));}
                    if stop.load(Ordering::Acquire){return}
                }
                if matches!(mode,Mode::Missing) {b"430 No Such Article\r\n".to_vec()} else if matches!(mode,Mode::Transient) {b"431 temporarily unavailable\r\n".to_vec()} else {
                    let mut nzb=br#"<nzb><file poster="Actual NZB Poster" date="1792828369" subject="&quot;actual.mkv&quot; yEnc (1/1)"><groups><group>example.spot</group></groups><segments><segment bytes="123" number="1">actual-part@fixture.test</segment></segments></file></nzb>"#.to_vec();
                    if matches!(mode,Mode::PayloadSecondBlocked) {
                        nzb=br#"<nzb><file poster="Actual NZB Poster" date="1792828369" subject="&quot;actual.mkv&quot; yEnc (1/2)"><groups><group>example.spot</group></groups><segments><segment bytes="123" number="1">actual-part@fixture.test</segment><segment bytes="123" number="2">actual-second@fixture.test</segment></segments></file></nzb>"#.to_vec();
                    } else if matches!(mode,Mode::PayloadInvalidMid) {
                        nzb=String::from_utf8(nzb).unwrap().replace("actual-part@fixture.test","not-a-message-id").into_bytes();
                    }
                    let mut encoder=flate2::write::DeflateEncoder::new(Vec::new(),flate2::Compression::default());encoder.write_all(&nzb).unwrap();
                    let mut reply=b"222 body\r\n".to_vec();
                    for chunk in binary::escape(&encoder.finish().unwrap()).chunks(128) {
                        if chunk.first()==Some(&b'.'){reply.push(b'.');}reply.extend_from_slice(chunk);reply.extend_from_slice(b"\r\n");
                    }reply.extend_from_slice(b".\r\n");reply
                }
            }
            "QUIT"=>return,
            _=>b"500 unsupported\r\n".to_vec(),
        };
        if stream.write_all(&reply).is_err(){return}
    }
}
pub(super) fn fixture()->(crate::pg::testing::Schema,crate::pg::indexer_runtime::Runtime,tempfile::TempDir) {
    let schema=crate::pg::testing::Schema::new().expect("explicit ATLAS_PG_TEST_URL required; never skipped success");
    let dir=tempfile::tempdir().unwrap();let mut config=crate::config::Config::default();
    config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;
    config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;
    let storage=crate::pg::StorageConfig {backend:crate::pg::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:1,allow_plaintext:true};
    let headers=crate::config::HeaderStoreConfig {enabled:true,dir:dir.path().to_str().unwrap().to_owned()};
    let runtime=crate::pg::indexer_runtime::Runtime::open(&config,storage,headers,&[],&mut None).unwrap();
    (schema,runtime,dir)
}
fn service(runtime:&crate::pg::indexer_runtime::Runtime,pool:Arc<crate::nntp::Pool>,handle:tokio::runtime::Handle)->native::Service {
    native::Service::new(&runtime.owner,runtime.db.clone(),crate::config::SpotsConfig {blacklist_url:String::new(),concurrency:2,per_pass:4,comments_per_pass:4,..Default::default()},pool,handle).unwrap()
}

#[test]
#[ignore="explicit isolated PG plus owned loopback NNTP; Root admission only"]
fn native_spot_owned_nntp_publishes_real_nzb_and_preserves_comment_originals() {
    let (schema,runtime,_dir)=fixture();let peer=Peer::new(Mode::Good);
    let handle=tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let service=service(&runtime,Arc::new(crate::nntp::Pool::new(&[peer.server.clone()])),handle.handle().clone());
    assert!(sign::verify(TITLE,FROM,ID).verified,"fixture uses actual crypto");
    let before_spot=service.comments_tick(1793000000).unwrap();
    assert_eq!((before_spot.comments,before_spot.reports,before_spot.read),(1,1,0),"comment/report headers survive arriving before the spot");
    assert_eq!(peer.count("BODY"),0,"unkept comment bodies are not fetched");
    let pass=service.spots_tick(1793000000).unwrap();assert_eq!((pass.noted,pass.saved,pass.retry),(1,1,0));
    let mut observer=schema.pool.get().unwrap();
    assert_eq!(observer.query_one("select count(*) from segments where message_id=$1",&[&b"<actual-part@fixture.test>".as_slice()]).unwrap().get::<_,i64>(0),1);
    let row=observer.query_one("select s.state,s.nzb_key,p.native_original_xml,r.poster from spots_seen s join spots p on p.release_id=s.release_id join releases r on r.id=s.release_id",&[]).unwrap();
    assert_eq!(row.get::<_,i64>(0),store::SAVED);assert!(!row.get::<_,String>(1).is_empty());
    assert_eq!(row.get::<_,Vec<u8>>(2),XML.as_bytes());assert_eq!(row.get::<_,String>(3),"Actual NZB Poster");
    let body_count=peer.count("BODY");assert_eq!(service.spots_tick(1793000001).unwrap().saved,0);assert_eq!(peer.count("BODY"),body_count);
    let comments=service.comments_tick(1793000000).unwrap();assert_eq!((comments.comments,comments.reports,comments.read),(0,0,1));
    let original=observer.query_one("select body,native_original_body from spot_comments where message_id=$1",&[&COMMENT]).unwrap();
    assert_eq!(original.get::<_,Vec<u8>>(1),b"Password: alpha\0beta\nKeep every word");
    assert_eq!(original.get::<_,String>(0),"Password: alpha\u{fffd}beta\nKeep every word");
    assert_eq!(observer.query_one("select count(*) from header_blocks",&[]).unwrap().get::<_,i64>(0),0,"NZB-derived publication makes no NXV claim");
    runtime.owner.close();
}

#[test]
#[ignore="explicit isolated PG plus owned NNTP crypto/absence controls"]
fn native_spot_forged_header_never_fetches_and_absent_body_never_publishes() {
    for mode in [Mode::Forged,Mode::Missing,Mode::Transient] {
        let (schema,runtime,_dir)=fixture();let peer=Peer::new(mode);
        let handle=tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let service=service(&runtime,Arc::new(crate::nntp::Pool::new(&[peer.server.clone()])),handle.handle().clone());
        let pass=service.spots_tick(1793000000).unwrap();assert_eq!(pass.saved,0);
        let mut observer=schema.pool.get().unwrap();assert_eq!(observer.query_one("select count(*) from releases",&[]).unwrap().get::<_,i64>(0),0);
        if matches!(mode,Mode::Forged) {assert_eq!((peer.count("HEAD"),peer.count("BODY")),(0,0));assert_eq!(pass.noted,0);}
        else if matches!(mode,Mode::Missing) {assert_eq!(pass.skipped,1);assert_eq!(observer.query_one("select state from spots_seen",&[]).unwrap().get::<_,i64>(0),store::SKIPPED);}
        else {assert_eq!(pass.retry,1);let row=observer.query_one("select state,tries from spots_seen",&[]).unwrap();assert_eq!((row.get::<_,i64>(0),row.get::<_,i64>(1)),(store::TODO,1));assert_eq!(runtime.owner.failure(),None);}
        runtime.owner.close();
    }
}

#[test]
#[ignore="explicit isolated PG plus owned NNTP rollback control"]
fn native_spot_tag_failure_rolls_back_real_writer_parts_and_seen_publication() {
    let (schema,runtime,_dir)=fixture();let peer=Peer::new(Mode::Good);
    let mut observer=schema.pool.get().unwrap();
    observer.batch_execute("create function reject_spot_tag() returns trigger language plpgsql as $$begin raise exception 'controlled spot tag failure';end$$;create trigger reject_spot_tag before insert on spots for each row execute function reject_spot_tag()").unwrap();
    let handle=tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let service=service(&runtime,Arc::new(crate::nntp::Pool::new(&[peer.server.clone()])),handle.handle().clone());
    assert!(service.spots_tick(1793000000).is_err());assert_eq!(runtime.owner.failure(),Some(crate::pg::outage::Failure::Fatal));
    assert!(peer.count("BODY")>0,"failure follows a real NNTP/parse/save attempt");
    for table in ["releases","files","segments","spots"] {assert_eq!(observer.query_one(&format!("select count(*) from {table}"),&[]).unwrap().get::<_,i64>(0),0,"{table} rolled back");}
    assert_eq!(observer.query_one("select state from spots_seen",&[]).unwrap().get::<_,i64>(0),store::TODO);
    assert_eq!(observer.query_one("select count(*) from spots_seen where nzb_key is not null",&[]).unwrap().get::<_,i64>(0),0);
    runtime.owner.close();
}

#[test]
#[ignore="explicit isolated PG plus held owned NNTP stop/join control"]
fn native_spot_stop_joins_held_body_without_publication_or_another_request() {
    let (schema,runtime,_dir)=fixture();let peer=Peer::new(Mode::Held);
    let handle=tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let service=service(&runtime,Arc::new(crate::nntp::Pool::new(&[peer.server.clone()])),handle.handle().clone());
    std::thread::scope(|scope| {
        let child=scope.spawn(||service.spots_tick(1793000000));peer.wait_body();
        runtime.owner.stop.store(true,Ordering::Release);let before=peer.count("BODY");
        assert!(child.join().unwrap().is_err());assert_eq!(peer.count("BODY"),before);
    });
    let mut observer=schema.pool.get().unwrap();assert_eq!(observer.query_one("select count(*) from releases",&[]).unwrap().get::<_,i64>(0),0);
    assert_eq!(runtime.owner.failure(),None);runtime.owner.close();
}

#[test]
#[ignore="explicit isolated PG plus actual original-session outage/owned NNTP"]
fn native_spot_pg_loss_after_body_stops_original_owner_and_further_provider_work() {
    let (schema,runtime,_dir)=fixture();let peer=Peer::new(Mode::Held);
    let pid=runtime.owner.with(|client|Ok(client.query_one("select pg_backend_pid()",&[])?.get::<_,i32>(0))).unwrap();
    let handle=tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let service=service(&runtime,Arc::new(crate::nntp::Pool::new(&[peer.server.clone()])),handle.handle().clone());
    std::thread::scope(|scope| {
        let child=scope.spawn(||service.spots_tick(1793000000));peer.wait_body();
        let mut observer=schema.pool.get().unwrap();assert!(observer.query_one("select pg_terminate_backend($1)",&[&pid]).unwrap().get::<_,bool>(0));
        peer.release.store(true,Ordering::Release);assert!(child.join().unwrap().is_err());
    });
    assert_eq!(runtime.owner.failure(),Some(crate::pg::outage::Failure::Unavailable));
    assert_eq!(peer.count("HEAD"),1,"lost ownership cannot launch a payload HEAD after the held NZB BODY");
    let before=peer.log.lock().unwrap().len();assert!(service.comments_tick(1793000000).is_err());assert_eq!(peer.log.lock().unwrap().len(),before);
    let mut observer=schema.pool.get().unwrap();assert_eq!(observer.query_one("select count(*) from releases",&[]).unwrap().get::<_,i64>(0),0);runtime.owner.close();
}

#[test]
#[ignore="explicit isolated PG plus actual GROUP fallback account isolation"]
fn native_spot_cursor_uses_actual_selected_account_binding() {
    let (schema,runtime,_dir)=fixture();let primary=Peer::new(Mode::RejectGroup);let actual=Peer::new(Mode::Good);
    let handle=tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let pool=Arc::new(crate::nntp::Pool::new(&[primary.server.clone(),actual.server.clone()]));
    let hex=|bytes:&[u8;32]|bytes.iter().map(|byte|format!("{byte:02x}")).collect::<String>();
    let primary_key=hex(&pool.provider_binding_on(0).unwrap());let actual_key=hex(&pool.provider_binding_on(1).unwrap());assert_ne!(primary_key,actual_key);
    let service=service(&runtime,pool,handle.handle().clone());service.spots_tick(1793000000).unwrap();
    let storage=native_store::Store::new(&runtime.owner).unwrap();
    assert_eq!(storage.with(|tx|Ok(store::cursor(&crate::pg::sql::Pg::new(&mut *tx),&primary_key)?)).unwrap(),None);
    assert_eq!(storage.with(|tx|Ok(store::cursor(&crate::pg::sql::Pg::new(&mut *tx),&actual_key)?)).unwrap(),Some(1));
    assert!(primary.count("GROUP")>0 && actual.count("XOVER")>0);assert_eq!(schema.pool.get().unwrap().query_one("select count(*) from spots",&[]).unwrap().get::<_,i64>(0),1);
    runtime.owner.close();
}
