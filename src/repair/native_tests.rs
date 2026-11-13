//! Authored before the native overview implementation. Every case owns real
//! PG/NXV and loopback sockets; none inserts Timing or claims skipped success.
use super::*;
use std::{io::{BufRead,BufReader,Write},net::TcpListener,sync::Mutex,thread};
const GROUP:&str="alt.binaries.native.overview";
const FILE:&str="Owned.Repair.2160p.Remux.mkv";
type Log=Arc<Mutex<Vec<(usize,String)>>>;

struct Peer {config:crate::config::UsenetServer,stop:Arc<AtomicBool>,held:Arc<AtomicBool>,worker:Option<thread::JoinHandle<()>>}
impl Peer {
    fn new(index:usize,full:bool,now:i64,log:Log)->Self {
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();listener.set_nonblocking(true).unwrap();
        let mut config=crate::config::UsenetServer::new("127.0.0.1","owned-fixture","fixture-only",listener.local_addr().unwrap().port());
        config.ssl=Some(false);config.compress=Some(false);config.connections=Some(1);config.priority=index as i64;
        let stop=Arc::new(AtomicBool::new(false));let end=stop.clone();let held=Arc::new(AtomicBool::new(false));let hold=held.clone();
        let worker=thread::spawn(move|| {
            let base=match index {0=>1000,1=>2_000_000,_=>4_000_000};
            while !end.load(Ordering::Acquire) {
                let (socket,_)=match listener.accept(){Ok(pair)=>pair,Err(e)if e.kind()==std::io::ErrorKind::WouldBlock=>{thread::sleep(Duration::from_millis(5));continue},Err(e)=>panic!("owned overview accept: {e}")};
                socket.set_read_timeout(Some(Duration::from_millis(50))).unwrap();socket.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
                let mut wire=BufReader::new(socket);if wire.get_mut().write_all(b"200 owned overview fixture\r\n").is_err(){continue;}
                while !end.load(Ordering::Acquire) {
                    let mut line=String::new();match wire.read_line(&mut line){Ok(0)=>break,Ok(_)=>{},Err(e)if matches!(e.kind(),std::io::ErrorKind::WouldBlock|std::io::ErrorKind::TimedOut)=>continue,Err(_)=>break};
                    {let mut log=log.lock().unwrap();assert!(log.len()<512,"unexpected unbounded provider work");log.push((index,line.trim_end().into()));}
                    let reply=if line.starts_with("AUTHINFO USER "){"381 password\r\n".into()}
                    else if line.starts_with("AUTHINFO PASS "){"281 accepted\r\n".into()}
                    else if line==format!("GROUP {GROUP}\r\n"){format!("211 {} {} {} {GROUP}\r\n",if full{10}else{9},base+1,base+10)}
                    else if let Some(range)=line.strip_prefix("XOVER ") {
                        while hold.load(Ordering::Acquire)&&!end.load(Ordering::Acquire){thread::sleep(Duration::from_millis(5));}
                        if end.load(Ordering::Acquire){break;}
                        let (start,last)=range.trim().split_once('-').unwrap();let start:u64=start.parse().unwrap();let last:u64=last.parse().unwrap();
                        assert!(last>=start&&last-start<3_000_000,"outside bounded overview range");
                        let mut out="224 rows\r\n".to_string();
                        for part in 1..=10u64 {let number=base+part;if number<start||number>last||(!full&&part==6){continue;}
                            let date=chrono::DateTime::from_timestamp(now-3*3600+part as i64,0).unwrap().to_rfc2822();
                            out.push_str(&format!("{number}\t\"{FILE}\" yEnc ({part}/10)\tactual fixture poster\t{date}\t<part{part}@overview.fixture.test>\t\t134217728\t10\r\n"));
                        }out.push_str(".\r\n");out
                    } else {panic!("unexpected overview command: {line:?}")};
                    if wire.get_mut().write_all(reply.as_bytes()).is_err(){break;}
                }
            }
        });Self {config,stop,held,worker:Some(worker)}
    }
}
impl Drop for Peer {fn drop(&mut self){self.stop.store(true,Ordering::Release);self.worker.take().unwrap().join().unwrap();}}
struct Fixture {runtime:crate::pg::indexer_runtime::Runtime,_schema:crate::pg::testing::Schema,_root:tempfile::TempDir,net:Option<tokio::runtime::Runtime>,ctx:crate::indexer::PassContext,headers:crate::config::HeaderStoreConfig,store:Arc<Store>,peers:Vec<Peer>,log:Log,now:i64}
impl Fixture {
    fn new()->Self {
        let schema=crate::pg::testing::Schema::new().expect("explicit isolated ATLAS_PG_TEST_URL required; never skipped success");let root=tempfile::tempdir().unwrap();
        let now=chrono::Utc::now().timestamp();let log=Arc::new(Mutex::new(Vec::new()));let peers=(0..3).map(|i|Peer::new(i,i==2,now,log.clone())).collect::<Vec<_>>();
        let mut cfg=crate::config::Config::default();cfg.postprocess.enabled=false;cfg.predb.enabled=false;cfg.spots.enabled=false;cfg.inferred_parts=false;
        cfg.media.tvmaze.enabled=false;cfg.media.allocine.enabled=false;cfg.media.details_per_minute=0;cfg.servers=peers.iter().map(|p|p.config.clone()).collect();cfg.sort_servers();
        let headers=crate::config::HeaderStoreConfig {enabled:true,dir:root.path().to_str().unwrap().into()};
        let runtime=crate::pg::indexer_runtime::Runtime::open(&cfg,crate::pg::StorageConfig {backend:crate::pg::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:1,allow_plaintext:true},headers.clone(),&[],&mut None).unwrap();
        let actual=runtime.owner.epoch().unwrap().catalog().header_config(&headers).unwrap();crate::nxv_store::live::spawn_bounded(&actual).unwrap();let store=crate::nxv_store::live::handle().unwrap();
        let net=tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let ctx=crate::indexer::PassContext {pool:Arc::new(crate::nntp::Pool::new(&cfg.servers).with_max_unsaved(64)),states:Default::default(),stop:runtime.owner.stop.clone(),verbose:false};
        Self {runtime,_schema:schema,_root:root,net:Some(net),ctx,headers,store,peers,log,now}
    }
    fn seed(&self) {
        let home=self.ctx.pool.host(0);self.runtime.owner.with(|c| {c.execute("insert into groups(name,live_cursor,backfill_cursor) values($1,700,20)",&[&format!("{GROUP}@{home}")])?;Ok(())}).unwrap();
        assert_eq!(self.net.as_ref().unwrap().block_on(crate::indexer::reread_window(&self.ctx,&crate::indexer::PassSettings {request_size:16,..Default::default()},&self.runtime.db,GROUP,0,(self.now-3*3600-300,self.now-3*3600+310),3_000_000)).unwrap().articles,9);
        self.runtime.owner.with(|c| {let row=c.query_one("select first_date,last_date,articles from file_timing",&[])?;assert_eq!((row.get::<_,Option<i64>>(0),row.get::<_,Option<i64>>(1),row.get::<_,i64>(2)),(Some(self.now-3*3600+1),Some(self.now-3*3600+10),9));Ok(())}).unwrap();
    }
    fn service(&self)->Service {let mut s=Service::new(&self.runtime.owner,&self.runtime.db,self.ctx.clone(),self.net.as_ref().unwrap().handle().clone(),self.store.clone(),&self.headers,crate::indexer::PassSettings {request_size:16,..Default::default()}).unwrap();s.next=Instant::now();s}
    fn frontiers(&self)->Vec<(String,Option<i64>,Option<i64>)> {self.runtime.owner.with(|c|Ok(c.query("select name,live_cursor,backfill_cursor from groups order by name",&[])?.into_iter().map(|r|(r.get(0),r.get(1),r.get(2))).collect())).unwrap()}
}
impl Drop for Fixture {fn drop(&mut self){self.runtime.owner.stop.store(true,Ordering::Release);drop(self.net.take());self.runtime.owner.close();crate::nxv_store::live::finish();}}
fn isolated(name:&str,body:fn()) {
    const CHILD:&str="ATLAS_TEST_NATIVE_OVERVIEW_CHILD";
    if std::env::var(CHILD).ok().as_deref()==Some(name){body();return;}
    let home=tempfile::tempdir().unwrap();let mut child=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact",name,"--ignored","--nocapture","--test-threads=1"])
        .env_clear().env(CHILD,name).env("ATLAS_HOME",home.path()).env("HOME",home.path()).env("ATLAS_PG_TEST_URL",std::env::var("ATLAS_PG_TEST_URL").expect("explicit isolated PostgreSQL required")).spawn().unwrap();
    let until=Instant::now()+Duration::from_secs(40);loop {if let Some(status)=child.try_wait().unwrap(){assert!(status.success(),"actual isolated overview child failed: {status}");break;}
        if Instant::now()>=until {child.kill().unwrap();child.wait().unwrap();panic!("actual overview fixture exceeded its bounded deadline");}thread::sleep(Duration::from_millis(10));}
}
#[test]
#[ignore="explicit isolated PG/NXV and three owned loopback NNTP accounts"]
fn owned_alternate_overview_fills_actual_timing_hole_without_advancing_frontiers() {isolated("repair::native::tests::owned_alternate_overview_fills_actual_timing_hole_without_advancing_frontiers",|| {
    let f=Fixture::new();f.seed();let before=f.frontiers();let mut service=f.service();let plan=service.plan(f.now).unwrap();
    assert_eq!(plan.found.posts.len(),1);assert_eq!(plan.found.posts[0].permille,900);assert_eq!(plan.found.posts[0].windows,[Window {group:GROUP.into(),from:f.now-3*3600+1-300,to:f.now-3*3600+10+300}]);
    let at=f.log.lock().unwrap().len();let progress=service.tick(f.now).unwrap();assert_eq!(progress.posts,1);assert!(progress.headers>=19);assert_eq!(f.frontiers(),before);
    f.runtime.owner.with(|c| {let r=c.query_one("select (select count(*) from segments),(select count(*) from file_parts),(select articles from file_timing),(select count(*) from repair_tried)",&[])?;
        assert_eq!((r.get::<_,i64>(0),r.get::<_,i64>(1),r.get::<_,i64>(2),r.get::<_,i64>(3)),(10,10,10,1));Ok(())}).unwrap();
    let log=f.log.lock().unwrap();let first=log[at..].iter().position(|(i,l)|*i==1&&l.starts_with("XOVER ")).unwrap();let last=log[at..].iter().position(|(i,l)|*i==2&&l.starts_with("XOVER ")).unwrap();assert!(first<last);drop(log);
    crate::nxv_store::live::sync_point(GROUP).unwrap().expect("actual NXV capture required").wait(Duration::from_secs(2)).unwrap();drop(service);
    f.store.close().unwrap();let snap=f.store.segment_snapshot(GROUP,f.now.div_euclid(86400)).unwrap().or_else(||f.store.segment_snapshot(GROUP,(f.now-3*3600).div_euclid(86400)).unwrap()).unwrap();
    let mut actual=false;for entry in snap.entries(0,64){let bytes=snap.map_block(entry).unwrap();let view=nntp_xover_engine::container::NxvBlockView::from_mmap_slice(&bytes).unwrap();let mut rows=nntp_xover_engine::streamer::FieldCursor::new(view).unwrap();
        while let Some(row)=rows.next_row(){actual|=if let Some(raw)=row.literal {crate::nntp::overview_of(raw).is_some_and(|r|r.message_id.contains("part6@overview.fixture.test"))} else {String::from_utf8_lossy(row.msgid).contains("part6@overview.fixture.test")};}}
    assert!(actual,"the accepted missing MID must exist in the real durable NXV block");
});}
#[test]
#[ignore="explicit isolated PG; wrong owner and pretransfer source admission"]
fn wrong_owner_and_oversized_source_cannot_issue_or_mark_a_repair() {isolated("repair::native::tests::wrong_owner_and_oversized_source_cannot_issue_or_mark_a_repair",|| {
    let f=Fixture::new();f.seed();let foreign=Arc::new(Owner::new(postgres::Client::connect(&f._schema.url,postgres::NoTls).unwrap()));
    assert!(Service::new(&foreign,&f.runtime.db,f.ctx.clone(),f.net.as_ref().unwrap().handle().clone(),f.store.clone(),&f.headers,Default::default()).is_err());foreign.close();
    f.runtime.owner.with(|c| {c.execute("update releases set original_name=$1",&[&vec![b'x';65537]])?;Ok(())}).unwrap();
    let at=f.log.lock().unwrap().len();let before=f.frontiers();let mut service=f.service();let progress=service.tick(f.now).unwrap();assert_eq!(progress.posts,0);assert_eq!(progress.deferred,1);assert_eq!(f.log.lock().unwrap().len(),at);assert_eq!(f.frontiers(),before);
    f.runtime.owner.with(|c|{assert_eq!(c.query_one("select count(*) from repair_tried",&[])?.get::<_,i64>(0),0);Ok(())}).unwrap();
});}
#[test]
#[ignore="explicit isolated PG/NXV; actual held date probe and joined cancellation"]
fn stop_during_actual_overview_probe_joins_without_mark_or_frontier_advance() {isolated("repair::native::tests::stop_during_actual_overview_probe_joins_without_mark_or_frontier_advance",|| {
    let f=Fixture::new();f.seed();let before=f.frontiers();let mut observer=postgres::Client::connect(&f._schema.url,postgres::NoTls).unwrap();observer.batch_execute(&format!("set search_path to {}",f._schema.name)).unwrap();
    f.peers[1].held.store(true,Ordering::Release);let at=f.log.lock().unwrap().len();let mut service=f.service();let now=f.now;let worker=thread::spawn(move||service.tick(now));
    let until=Instant::now()+Duration::from_secs(3);loop {if f.log.lock().unwrap()[at..].iter().any(|(i,l)|*i==1&&l.starts_with("XOVER ")){break;}assert!(Instant::now()<until);thread::sleep(Duration::from_millis(5));}
    f.runtime.owner.stop.store(true,Ordering::Release);let started=Instant::now();assert!(crate::pg::outage::is_attempt_ended(&worker.join().unwrap().unwrap_err()));assert!(started.elapsed()<Duration::from_secs(2));
    assert_eq!(observer.query_one("select (select count(*) from segments),(select count(*) from repair_tried)",&[]).unwrap().get::<_,i64>(0),9);
    assert_eq!(observer.query_one("select count(*) from repair_tried",&[]).unwrap().get::<_,i64>(0),0);let after=observer.query("select name,live_cursor,backfill_cursor from groups order by name",&[]).unwrap().into_iter().map(|r|(r.get(0),r.get(1),r.get(2))).collect::<Vec<(String,Option<i64>,Option<i64>)>>();assert_eq!(after,before);
});}
#[test]
#[ignore="explicit isolated PG/NXV; original owner lifetime is mandatory"]
fn ended_original_owner_cannot_probe_or_mark_a_native_repair() {isolated("repair::native::tests::ended_original_owner_cannot_probe_or_mark_a_native_repair",|| {
    let f=Fixture::new();f.seed();let mut service=f.service();let at=f.log.lock().unwrap().len();
    let before=f.frontiers();let mut observer=postgres::Client::connect(&f._schema.url,postgres::NoTls).unwrap();
    observer.batch_execute(&format!("set search_path to {}",f._schema.name)).unwrap();
    f.runtime.owner.close();assert!(service.tick(f.now).is_err());
    assert_eq!(f.log.lock().unwrap().len(),at,"a replacement owner/session is never acquired");
    assert_eq!(observer.query_one("select count(*) from repair_tried",&[]).unwrap().get::<_,i64>(0),0);
    let after=observer.query("select name,live_cursor,backfill_cursor from groups order by name",&[]).unwrap().into_iter().map(|r|(r.get(0),r.get(1),r.get(2))).collect::<Vec<(String,Option<i64>,Option<i64>)>>();
    assert_eq!(after,before);
});}
