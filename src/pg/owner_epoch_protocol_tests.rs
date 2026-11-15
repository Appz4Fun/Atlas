//! Count real protocol completions, without retaining query or authentication payloads.
use std::{
    io::{self, Read, Write},
    net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Default)]
struct Counts { ready:AtomicU64, sync:AtomicU64, simple:AtomicU64, parse:AtomicU64, commands:AtomicU64 }
impl Counts {
    fn snapshot(&self)->[u64;5] {
        [&self.ready,&self.sync,&self.simple,&self.parse,&self.commands].map(|value|value.load(Ordering::SeqCst))
    }
}

// Only a five-byte header and remaining length survive the current chunk.
// Payloads pass unchanged through fixed buffers, never into a log or Vec.
struct Frames { startup:bool, header:[u8;5], used:usize, remaining:usize }
impl Frames {
    fn new(startup:bool)->Self {Self {startup,header:[0;5],used:0,remaining:0}}
    fn consume(&mut self,mut input:&[u8],backend:bool,counts:&Counts)->io::Result<()> {
        while !input.is_empty() {
            if self.remaining!=0 {
                let n=self.remaining.min(input.len());self.remaining-=n;input=&input[n..];continue
            }
            let width=if self.startup {4}else{5};
            let n=(width-self.used).min(input.len());
            self.header[self.used..self.used+n].copy_from_slice(&input[..n]);
            self.used+=n;input=&input[n..];
            if self.used!=width {continue}
            let offset=usize::from(!self.startup);
            let length=u32::from_be_bytes(self.header[offset..offset+4].try_into().unwrap());
            if !(4..=16*1024*1024).contains(&length) {
                return Err(io::Error::new(io::ErrorKind::InvalidData,"fixture protocol extent"));
            }
            if !self.startup {
                let counter=match (backend,self.header[0]) {
                    (true,b'Z')=>Some(&counts.ready),
                    (true,b'C')=>Some(&counts.commands),
                    (false,b'S')=>Some(&counts.sync),
                    (false,b'Q')=>Some(&counts.simple),
                    (false,b'P')=>Some(&counts.parse),
                    _=>None,
                };
                if let Some(counter)=counter {counter.fetch_add(1,Ordering::SeqCst);}
            }
            self.startup=false;self.used=0;self.remaining=length as usize-4;
        }Ok(())
    }
}

enum Wire {
    Tcp(TcpStream),
}
impl Wire {
    fn connect(config:&postgres::Config)->io::Result<Self> {
        let port=config.get_ports().first().copied().unwrap_or(5432);
        match config.get_hosts().first() {
            Some(postgres::config::Host::Tcp(host))=>{
                // Setup and Drop must not wait on DNS or an unbounded local
                // socket connect. This fixture admits only literal TCP IPs.
                let ip:IpAddr=host.parse().map_err(|_|io::Error::new(io::ErrorKind::InvalidInput,"fixture requires a literal TCP address"))?;
                let address=SocketAddr::new(ip,port);
                let stream=TcpStream::connect_timeout(&address,Duration::from_secs(2))?;
                stream.set_nodelay(true)?;Ok(Self::Tcp(stream))
            },
            _=>Err(io::Error::new(io::ErrorKind::InvalidInput,"fixture upstream transport")),
        }
    }
    fn copy(&self)->io::Result<Self> {match self {
        Self::Tcp(s)=>Ok(Self::Tcp(s.try_clone()?)),
    }}
    fn timeouts(&self)->io::Result<()> {match self {
        Self::Tcp(s)=>{s.set_read_timeout(Some(Duration::from_millis(100)))?;s.set_write_timeout(Some(Duration::from_secs(2)))},
    }}
    fn shutdown(&self) {let _=match self {
        Self::Tcp(s)=>s.shutdown(Shutdown::Both),
    };}
}
impl Read for Wire {fn read(&mut self,out:&mut [u8])->io::Result<usize> {match self {
    Self::Tcp(s)=>s.read(out),
}}}
impl Write for Wire {
    fn write(&mut self,input:&[u8])->io::Result<usize> {match self {
        Self::Tcp(s)=>s.write(input),
    }}
    fn flush(&mut self)->io::Result<()> {Ok(())}
}

fn relay(mut source:Wire,mut target:Wire,backend:bool,counts:Arc<Counts>,stop:Arc<AtomicBool>,ended:Arc<AtomicBool>,failed:Arc<AtomicBool>) {
    let mut frames=Frames::new(!backend);let mut buffer=[0;8192];
    while !stop.load(Ordering::Acquire)&&!ended.load(Ordering::Acquire) {
        let result=match source.read(&mut buffer) {
            Ok(0)=>break,
            Ok(n)=>frames.consume(&buffer[..n],backend,&counts).and_then(|()|target.write_all(&buffer[..n])),
            Err(error) if matches!(error.kind(),io::ErrorKind::WouldBlock|io::ErrorKind::TimedOut)=>continue,
            Err(error)=>Err(error),
        };
        if result.is_err() {
            if !stop.load(Ordering::Acquire)&&!ended.load(Ordering::Acquire) {failed.store(true,Ordering::Release);}
            break
        }
    }
    ended.store(true,Ordering::Release);source.shutdown();target.shutdown();
}

struct Proxy {port:u16,counts:Arc<Counts>,stop:Arc<AtomicBool>,failed:Arc<AtomicBool>,thread:Option<JoinHandle<()>>}
impl Proxy {
    fn new(url:&str)->Self {
        let config:postgres::Config=url.parse().expect("explicit fixture endpoint");
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();listener.set_nonblocking(true).unwrap();
        let port=listener.local_addr().unwrap().port();
        let counts=Arc::new(Counts::default());let stop=Arc::new(AtomicBool::new(false));let failed=Arc::new(AtomicBool::new(false));
        let (out,stopping,bad)=(counts.clone(),stop.clone(),failed.clone());
        let thread=thread::spawn(move|| {
            let mut workers=Vec::<[JoinHandle<()>;2]>::new();
            let until=Instant::now()+Duration::from_secs(120);
            while !stopping.load(Ordering::Acquire)&&Instant::now()<until {
                let mut i=0;
                while i<workers.len() {
                    if workers[i].iter().all(JoinHandle::is_finished) {
                        for worker in workers.swap_remove(i) {if worker.join().is_err() {bad.store(true,Ordering::Release);}}
                    }else{i+=1}
                }
                match listener.accept() {
                    Ok((socket,_))=>{
                        if workers.len()>=4 {bad.store(true,Ordering::Release);let _=socket.shutdown(Shutdown::Both);continue}
                        let prepared=(||->io::Result<_> {
                            socket.set_nodelay(true)?;let downstream=Wire::Tcp(socket);let upstream=Wire::connect(&config)?;
                            downstream.timeouts()?;upstream.timeouts()?;
                            Ok((downstream.copy()?,upstream.copy()?,downstream,upstream))
                        })();
                        let Ok((front,back,downstream,upstream))=prepared else {bad.store(true,Ordering::Release);continue};
                        let ended=Arc::new(AtomicBool::new(false));
                        let args=(out.clone(),stopping.clone(),ended.clone(),bad.clone());
                        let forward=thread::spawn(move||relay(front,upstream,false,args.0,args.1,args.2,args.3));
                        let args=(out.clone(),stopping.clone(),ended,bad.clone());
                        let reverse=thread::spawn(move||relay(back,downstream,true,args.0,args.1,args.2,args.3));
                        workers.push([forward,reverse]);
                    },
                    Err(error) if error.kind()==io::ErrorKind::WouldBlock=>thread::sleep(Duration::from_millis(2)),
                    Err(_)=>{bad.store(true,Ordering::Release);break},
                }
            }
            if !stopping.swap(true,Ordering::AcqRel) {bad.store(true,Ordering::Release);}
            for pair in workers {for worker in pair {if worker.join().is_err() {bad.store(true,Ordering::Release);}}}
        });
        Self {port,counts,stop,failed,thread:Some(thread)}
    }
    fn url(&self,original:&str)->String {
        let config:postgres::Config=original.parse().expect("explicit fixture endpoint");
        let quote=|value:&str|format!("'{}'",value.replace('\\',"\\\\").replace('\'',"\\'"));
        // A fresh endpoint prevents Config.host()/port() append semantics from
        // silently retaining a direct Unix/TCP route around the counter.
        let mut url=format!("host=127.0.0.1 port={} sslmode=disable connect_timeout=2",self.port);
        for (key,value) in [("user",config.get_user()),("dbname",config.get_dbname()),("options",config.get_options())] {
            if let Some(value)=value {url.push_str(&format!(" {key}={}",quote(value)));}
        }
        if let Some(value)=config.get_password() {url.push_str(&format!(" password={}",quote(std::str::from_utf8(value).expect("text fixture credential"))));}
        url
    }
}
impl Drop for Proxy {fn drop(&mut self) {self.stop.store(true,Ordering::Release);self.thread.take().unwrap().join().unwrap();}}

// Test-only barrier: no background tasks use this Runtime. Holding the
// original client directly flushes queued Statement Drop Close/Sync messages
// without adding another Owner guard to the measured hundred callbacks.
fn drain(runtime:&super::Runtime) {
    let mut held=runtime.owner.client.lock().unwrap();
    assert_eq!(held.as_mut().unwrap().query_typed_one("select 1",&[]).unwrap().get::<_,i32>(0),1);
}

#[test]
#[ignore="explicit isolated PostgreSQL; one hundred real canonical Owner callbacks and framed protocol counters"]
fn guarded_callbacks_reuse_one_epoch_proof_a_second_and_prove_again_after() {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
    let proxy=Proxy::new(&schema.url);
    let mut config=crate::config::Config::default();
    config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;
    config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;
    let header_root=tempfile::tempdir().unwrap();
    let storage=crate::pg::StorageConfig {backend:crate::pg::Backend::Postgres,url:proxy.url(&schema.url),schema:schema.name.clone(),max_connections:1,allow_plaintext:true};
    let headers=crate::config::HeaderStoreConfig {enabled:true,dir:header_root.path().to_str().unwrap().into()};
    let runtime=super::Runtime::open(&config,storage,headers,&[],&mut None).unwrap();
    assert!(runtime.owner.epoch().is_some(),"a raw Owner without canonical epoch checks cannot qualify this regression");
    runtime.owner.with(|_|Ok(())).unwrap();drain(&runtime);
    let before=proxy.counts.snapshot();let started=Instant::now();let mut callbacks=0;
    for _ in 0..100 {runtime.owner.with(|_|{callbacks+=1;Ok(())}).unwrap();}
    drain(&runtime);let elapsed=started.elapsed();let after=proxy.counts.snapshot();
    let delta=std::array::from_fn::<_,5,_>(|i|after[i]-before[i]);
    // The final unnamed typed barrier contributes exactly one Sync/Ready and
    // one Parse. All deferred Close/Sync completions belong to the workload.
    let ready=delta[0]-1;let boundaries=delta[1]+delta[2]-1;let parses=delta[3]-1;let commands=delta[4]-1;
    assert_eq!(callbacks,100);assert_eq!(runtime.owner.failure(),None);assert!(!proxy.failed.load(Ordering::Acquire));
    println!("owner guard protocol: callbacks={callbacks}, commands={commands}, ready={ready}, boundaries={boundaries}, parses={parses}, elapsed_us={}",elapsed.as_micros());
    assert_eq!(ready,boundaries,"every frontend completion boundary must have a real PostgreSQL ReadyForQuery");
    // A proof is three commands, and one serves every callback within EPOCH_PROOF_FOR of it.
    let proofs=(elapsed.as_secs_f64()/super::EPOCH_PROOF_FOR.as_secs_f64()).ceil() as u64+1;
    assert_eq!(commands%3,0,"a proof keeps all three real guard commands");
    assert!(commands<=3*proofs,"callbacks within a proof's second must not prove again: {commands} commands in {elapsed:?}");
    assert_eq!(ready,commands,"no implicit prepare/close completion cycles");
    // Once the proof is older than EPOCH_PROOF_FOR the next callback proves again, all three checks.
    std::thread::sleep(super::EPOCH_PROOF_FOR+std::time::Duration::from_millis(50));
    let runtime2=&runtime;let before=proxy.counts.snapshot();
    runtime2.owner.with(|_|Ok(())).unwrap();drain(runtime2);
    let after=proxy.counts.snapshot();assert_eq!(after[4]-before[4]-1,3,"a stale proof is proven again");
    runtime.owner.close();
}

#[test]
fn framed_counter_ignores_payload_tags_and_handles_fragmented_headers() {
    let counts=Counts::default();let mut front=Frames::new(true);let mut back=Frames::new(false);
    let input=[0,0,0,12,0,3,0,0,b'Z',b'S',b'P',0,b'Q',0,0,0,8,b'P',b'S',b'Z',0,b'S',0,0,0,4,b'P',0,0,0,8,b'S',b'Z',b'P',b'Q'];
    for chunk in input.chunks(2) {front.consume(chunk,false,&counts).unwrap();}
    let output=[b'R',0,0,0,8,b'Z',b'Z',b'Z',b'Z',b'Z',0,0,0,5,b'I',b'D',0,0,0,8,b'Z',b'Z',b'Z',b'Z',b'C',0,0,0,13,b'S',b'E',b'L',b'E',b'C',b'T',b' ',b'1',0];
    for chunk in output.chunks(3) {back.consume(chunk,true,&counts).unwrap();}
    assert_eq!(counts.snapshot(),[1,1,1,1,1]);
    assert!(Frames::new(false).consume(&[b'Z',0,0,0,3],true,&counts).is_err());
}
