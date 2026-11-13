use super::*;
pub(crate) fn fixture()->(crate::pg::testing::Schema,crate::pg::indexer_runtime::Runtime,tempfile::TempDir,Arc<crate::nxv_store::store::Store>,crate::config::HeaderStoreConfig) {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");let root=tempfile::tempdir().unwrap();
    let mut config=crate::config::Config::default();config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;
    let storage=crate::pg::StorageConfig {backend:crate::pg::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:1,allow_plaintext:true};
    let headers=crate::config::HeaderStoreConfig {enabled:true,dir:root.path().to_str().unwrap().into()};
    let runtime=crate::pg::indexer_runtime::Runtime::open(&config,storage,headers.clone(),&[],&mut None).unwrap();
    let actual=runtime.owner.epoch().unwrap().catalog().header_config(&headers).unwrap();
    let (store,_)=crate::nxv_store::store::Store::open(crate::nxv_store::store::StoreConfig::new(actual.root()),crate::nxv_store::store::KeepSet::default()).unwrap();
    (schema,runtime,root,store,headers)
}
pub(crate) fn seed(runtime:&crate::pg::indexer_runtime::Runtime,now:i64,parts:&[i64]) {
    let date=chrono::DateTime::from_timestamp(now-3*3600,0).unwrap().to_rfc2822();
    let headers=parts.iter().map(|part|crate::nntp::Overview {number:*part as u64,subject:format!("Own.Release - \"own.bin\" yEnc ({part}/4)"),from:"original poster".into(),date:date.clone(),message_id:format!("<part{part}of4.own@example.invalid>"),bytes:40*1024*1024,..Default::default()}).collect();
    let mut releases:Vec<_>=crate::collections::group_articles("example.group",crate::nntp::headers_to_articles(headers)).into_values().collect();
    for release in &mut releases {release.group="example.group".into();release.poster="original poster".into();release.date=date.clone();}
    runtime.owner.with(|client|crate::pg::release_writer::save(client,&releases,now).map(|_|())).unwrap();
}
#[test]
#[ignore="requires isolated PG and actual catalog-bound NXV owner"]
fn native_inferred_plan_proves_real_file_mids_and_is_readonly() {
    let (_schema,runtime,_root,store,headers)=fixture();let now=1800000000;seed(&runtime,now,&[1,2,4]);
    let service=Service::new(&runtime.owner,&runtime.db,Arc::new(crate::nntp::Pool::new(&[])),store.clone(),&headers).unwrap();
    let plan=service.plan(now).unwrap();assert_eq!(plan.requests.len(),1);assert_eq!(plan.requests[0].wanted.key(),"part3of4.own@example.invalid");
    assert_eq!(plan.requests[0].wanted.row.filename.as_deref(),Some("own.bin"));assert_eq!(plan.requests[0].wanted.row.part,Some(3));
    runtime.owner.with(|client| {assert_eq!(client.query_one("select (select count(*) from inferred_asked)+(select count(*) from inferred_ids)",&[])?.get::<_,i64>(0),0);Ok(())}).unwrap();
    drop(service);runtime.owner.close();store.close().unwrap();
}
#[test]
#[ignore="requires isolated PG original owner and actual source revision changes"]
fn native_inferred_source_fence_refuses_actual_writer_growth_before_any_result() {
    let (_schema,runtime,_root,store,headers)=fixture();let now=1800000000;seed(&runtime,now,&[1,2,4]);
    let service=Service::new(&runtime.owner,&runtime.db,Arc::new(crate::nntp::Pool::new(&[])),store.clone(),&headers).unwrap();
    let plan=service.plan(now).unwrap();let request=&plan.requests[0];
    assert!(service.source_current(&request.source,request.source.revision).unwrap());seed(&runtime,now,&[3]);
    assert!(!service.source_current(&request.source,request.source.revision).unwrap());
    runtime.owner.with(|client| {assert_eq!(client.query_one("select count(*) from inferred_ids",&[])?.get::<_,i64>(0),0);Ok(())}).unwrap();
    drop(service);runtime.owner.close();store.close().unwrap();
}

pub(crate) struct Peer {pub(crate) config:crate::config::UsenetServer,pub(crate) hits:Arc<std::sync::atomic::AtomicUsize>,stop:Arc<std::sync::atomic::AtomicBool>,thread:Option<std::thread::JoinHandle<()>>}
impl Drop for Peer {fn drop(&mut self){self.stop.store(true,Ordering::Release);if let Some(thread)=self.thread.take(){thread.join().unwrap();}}}
pub(crate) fn peer(now:i64,wrong_mid:bool,held:Option<(std::sync::mpsc::Sender<()>,std::sync::mpsc::Receiver<()>)>)->Peer {
    use std::{io::{BufRead,Write},sync::atomic::{AtomicBool,AtomicUsize},net::TcpListener};
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();listener.set_nonblocking(true).unwrap();let port=listener.local_addr().unwrap().port();
    let mut config=crate::config::UsenetServer::new("127.0.0.1","","",port);config.ssl=Some(false);config.compress=Some(false);config.connections=Some(1);
    let hits=Arc::new(AtomicUsize::new(0));let seen=hits.clone();let stop=Arc::new(AtomicBool::new(false));let stopping=stop.clone();
    let thread=std::thread::spawn(move|| {
        let date=chrono::DateTime::from_timestamp(now-3*3600,0).unwrap().to_rfc2822();
        while !stopping.load(Ordering::Acquire) {
            let(mut socket,_)=match listener.accept(){Ok(socket)=>socket,Err(error)if error.kind()==std::io::ErrorKind::WouldBlock=>{std::thread::sleep(Duration::from_millis(5));continue},Err(error)=>panic!("owned NNTP fixture failed: {error}")};
            socket.set_read_timeout(Some(Duration::from_millis(50))).unwrap();socket.write_all(b"200 owned HEAD fixture\r\n").unwrap();let mut input=std::io::BufReader::new(socket.try_clone().unwrap());
            loop {
                if stopping.load(Ordering::Acquire){return;}
                let mut command=String::new();match input.read_line(&mut command){Ok(0)=>break,Err(error)if matches!(error.kind(),std::io::ErrorKind::WouldBlock|std::io::ErrorKind::TimedOut)=>continue,Err(_)=>break,Ok(_)=>{}}
                assert_eq!(command,"HEAD <part3of4.own@example.invalid>\r\n","repair must never query GROUP/ranges/BODY or guess another MID");seen.fetch_add(1,Ordering::AcqRel);
                if let Some((ready,release))=&held {ready.send(()).unwrap();release.recv_timeout(Duration::from_secs(10)).unwrap();}
                let mid=if wrong_mid{"part9of9.wrong@example.invalid"}else{"part3of4.own@example.invalid"};
                let head=format!("221 actual headers\r\nSubject: Own.Release - \"own.bin\" yEnc (3/4)\r\nFrom: original poster\r\nDate: {date}\r\nMessage-ID: <{mid}>\r\nBytes: 41943040\r\n.\r\n");
                if socket.write_all(head.as_bytes()).is_err(){break;}
            }
        }
    });Peer {config,hits,stop,thread:Some(thread)}
}
pub(crate) fn net()->tokio::runtime::Runtime {tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap()}
#[test]
#[ignore="requires real isolated PG/NXV and owned NNTP; explicit no-skip qualification"]
fn native_inferred_real_head_is_durable_before_atomic_missing_part_acceptance() {
    let (_schema,runtime,_root,store,headers)=fixture();let now=1800000000;seed(&runtime,now,&[1,2,4]);let server=peer(now,false,None);let net=net();
    let pool=Arc::new(crate::nntp::Pool::new(&[server.config.clone()]).with_body_tries(Duration::from_secs(1),0));
    let mut service=Service::new(&runtime.owner,&runtime.db,pool,store.clone(),&headers).unwrap();service.next=Instant::now();
    let progress=service.tick(net.handle(),now).unwrap();assert_eq!(progress.requested,1);assert_eq!(progress.found,1);assert!(service.journal.pending().unwrap().is_none());
    runtime.owner.with(|client| {assert_eq!(client.query_one("select count(*) from segments",&[])?.get::<_,i64>(0),4);assert_eq!(client.query_one("select tries from inferred_ids where message_id='part3of4.own@example.invalid'",&[])?.get::<_,i64>(0),1);Ok(())}).unwrap();
    service.next=Instant::now();assert_eq!(service.tick(net.handle(),now+301).unwrap().requested,0);assert_eq!(server.hits.load(Ordering::Acquire),1);
    drop(service);runtime.owner.close();store.close().unwrap();
}
#[test]
#[ignore="requires real isolated PG/NXV and owned NNTP; wrong response must never normalize guessed headers"]
fn native_inferred_wrong_actual_mid_cannot_fill_the_expected_part() {
    let (_schema,runtime,_root,store,headers)=fixture();let now=1800000000;seed(&runtime,now,&[1,2,4]);let server=peer(now,true,None);let net=net();
    let mut service=Service::new(&runtime.owner,&runtime.db,Arc::new(crate::nntp::Pool::new(&[server.config.clone()]).with_body_tries(Duration::from_secs(1),0)),store.clone(),&headers).unwrap();service.next=Instant::now();
    let progress=service.tick(net.handle(),now).unwrap();assert_eq!(progress.found,0);assert_eq!(progress.unusable,1);assert!(service.journal.pending().unwrap().is_none());
    runtime.owner.with(|client| {assert_eq!(client.query_one("select count(*) from segments",&[])?.get::<_,i64>(0),3);assert_eq!(client.query_one("select count(*) from segments where part=3",&[])?.get::<_,i64>(0),0);Ok(())}).unwrap();
    assert_eq!(server.hits.load(Ordering::Acquire),1);drop(service);runtime.owner.close();store.close().unwrap();
}
#[test]
#[ignore="requires real two-session writer interleaving and owned held NNTP HEAD"]
fn native_inferred_actual_writer_growth_during_head_supersedes_without_a_late_result() {
    let (_schema,runtime,_root,store,headers)=fixture();let now=1800000000;seed(&runtime,now,&[1,2,4]);
    let (seen,ready)=std::sync::mpsc::channel();let (release,held)=std::sync::mpsc::channel();let server=peer(now,false,Some((seen,held)));let net=net();
    let mut service=Service::new(&runtime.owner,&runtime.db,Arc::new(crate::nntp::Pool::new(&[server.config.clone()]).with_body_tries(Duration::from_secs(5),0)),store.clone(),&headers).unwrap();service.next=Instant::now();let handle=net.handle().clone();
    let worker=std::thread::spawn(move||{let progress=service.tick(&handle,now).unwrap();(service,progress)});
    ready.recv_timeout(Duration::from_secs(10)).unwrap();seed(&runtime,now,&[3]);release.send(()).unwrap();let (mut service,progress)=worker.join().unwrap();
    assert_eq!(progress.found,0);assert_eq!(progress.superseded,1);assert!(service.journal.pending().unwrap().is_none());
    runtime.owner.with(|client| {assert_eq!(client.query_one("select count(*) from inferred_ids",&[])?.get::<_,i64>(0),0);assert_eq!(client.query_one("select count(*) from segments",&[])?.get::<_,i64>(0),4);Ok(())}).unwrap();
    drop(service);runtime.owner.close();store.close().unwrap();
}
#[test]
#[ignore="requires real isolated PG owner replacement, durable HEAD replay and owned NNTP"]
fn native_inferred_pending_raw_head_replays_after_owner_replacement_without_refetch_or_duplicate() {
    let (schema,runtime,_root,store,headers)=fixture();let now=1800000000;seed(&runtime,now,&[1,2,4]);let server=peer(now,false,None);let net=net();let pool=Arc::new(crate::nntp::Pool::new(&[server.config.clone()]).with_body_tries(Duration::from_secs(1),0));
    let mut service=Service::new(&runtime.owner,&runtime.db,pool.clone(),store.clone(),&headers).unwrap();let request=service.plan(now).unwrap().requests.pop().unwrap();
    let crate::nntp::HeadAnswer::Found(reply)=net.block_on(pool.background_head(&request.wanted.row.message_id)).unwrap() else {panic!("actual completed HEAD required")};
    let durable=service.journal.append(Service::record(&request,request.source.revision,&reply,now)).unwrap();drop(durable);drop(service);runtime.owner.close();store.close().unwrap();drop(store);
    let mut config=crate::config::Config::default();config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;
    let storage=crate::pg::StorageConfig {backend:crate::pg::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:1,allow_plaintext:true};
    let replacement=crate::pg::indexer_runtime::Runtime::open(&config,storage,headers.clone(),&[],&mut None).unwrap();assert_ne!(runtime.owner.epoch().unwrap().id(),replacement.owner.epoch().unwrap().id());
    let actual=replacement.owner.epoch().unwrap().catalog().header_config(&headers).unwrap();let (store,_)=crate::nxv_store::store::Store::open(crate::nxv_store::store::StoreConfig::new(actual.root()),crate::nxv_store::store::KeepSet::default()).unwrap();
    let mut service=Service::new(&replacement.owner,&replacement.db,pool,store.clone(),&headers).unwrap();service.next=Instant::now();let progress=service.tick(net.handle(),now+1).unwrap();
    assert_eq!(progress.found,1);assert_eq!(progress.requested,0,"retained real HEAD needs no new provider response");assert_eq!(server.hits.load(Ordering::Acquire),1);assert!(service.journal.pending().unwrap().is_none());
    replacement.owner.with(|client| {assert_eq!(client.query_one("select count(*) from segments",&[])?.get::<_,i64>(0),4);assert_eq!(client.query_one("select tries from inferred_ids",&[])?.get::<_,i64>(0),1);Ok(())}).unwrap();
    drop(service);replacement.owner.close();store.close().unwrap();
}

#[test]
#[ignore="requires real PG owner, held NNTP and actual asynchronous read cancellation"]
fn native_inferred_stop_drains_the_actual_head_read_before_any_part_or_result() {
    let (schema,runtime,_root,store,headers)=fixture();let now=1800000000;seed(&runtime,now,&[1,2,4]);let net=net();
    let (seen,ready)=std::sync::mpsc::channel();let (release,held)=std::sync::mpsc::channel();let server=peer(now,false,Some((seen,held)));
    let mut service=Service::new(&runtime.owner,&runtime.db,Arc::new(crate::nntp::Pool::new(&[server.config.clone()]).with_body_tries(Duration::from_secs(5),0)),store.clone(),&headers).unwrap();service.next=Instant::now();let handle=net.handle().clone();
    let worker=std::thread::spawn(move||{let result=service.tick(&handle,now);(service,result)});
    ready.recv_timeout(Duration::from_secs(10)).unwrap();runtime.owner.stop.store(true,Ordering::Release);
    let (mut service,result)=worker.join().unwrap();assert!(crate::pg::outage::is_attempt_ended(&result.unwrap_err()));assert!(service.journal.pending().unwrap().is_none());assert_eq!(runtime.owner.failure(),None);
    release.send(()).unwrap();
    let mut observer=schema.pool.get().unwrap();let mut tx=observer.build_transaction().read_only(true).start().unwrap();
    assert_eq!(tx.query_one("select count(*) from inferred_ids",&[]).unwrap().get::<_,i64>(0),0);assert_eq!(tx.query_one("select count(*) from segments",&[]).unwrap().get::<_,i64>(0),3);tx.commit().unwrap();drop(observer);
    assert_eq!(server.hits.load(Ordering::Acquire),1);drop(service);runtime.owner.close();store.close().unwrap();
}

#[test]
#[ignore="requires two real canonical owners and their actual independent native writers"]
fn native_inferred_factory_rejects_cross_owner_writer_before_journal_or_head_work() {
    let (_a_schema,a,_a_root,a_store,a_headers)=fixture();let (_b_schema,b,_b_root,b_store,_b_headers)=fixture();let server=peer(1800000000,false,None);
    let pool=Arc::new(crate::nntp::Pool::new(&[server.config.clone()]));
    assert!(!a_store.root().join("_state/inferred-heads").exists());
    assert!(Service::new(&a.owner,&b.db,pool.clone(),a_store.clone(),&a_headers).is_err(),"native Db/Owner pairing is exact even when both owners are genuine");
    assert!(!a_store.root().join("_state/inferred-heads").exists(),"a mismatched writer must not create the owner's journal");
    let service=Service::new(&a.owner,&a.db,pool,a_store.clone(),&a_headers).unwrap();assert_eq!(server.hits.load(Ordering::Acquire),0);assert_eq!(a.owner.failure(),None);assert_eq!(b.owner.failure(),None);
    drop(service);a.owner.close();b.owner.close();a_store.close().unwrap();b_store.close().unwrap();
}

#[test]
#[ignore="requires actual isolated native PG rows for target-local HEAD resource admission"]
fn native_inferred_oversized_header_filename_is_deferred_without_poisoning_owner_or_losing_data() {
    let (_schema,runtime,_root,store,headers)=fixture();let now=1800000000;let filename=format!("{}.bin","n".repeat(70*1024));let date=chrono::DateTime::from_timestamp(now-3*3600,0).unwrap().to_rfc2822();
    let release=crate::parser::Release {name:"Large.Header.Name".into(),group:"example.group".into(),poster:"p".into(),date:date.clone(),articles:[1,2,4].into_iter().map(|part|crate::parser::Article {
        filename:Some(filename.clone()),message_id:format!("<part{part}of4.large@example.invalid>"),part:Some(part),total_parts:Some(4),bytes:40*1024*1024,subject:format!("Large.Header.Name - \"{filename}\" yEnc ({part}/4)"),author:"p".into(),date:date.clone(),..Default::default()
    }).collect(),..Default::default()};
    runtime.owner.with(|client|crate::pg::release_writer::save(client,&[release],now).map(|_|())).unwrap();
    let mut service=Service::new(&runtime.owner,&runtime.db,Arc::new(crate::nntp::Pool::new(&[])),store.clone(),&headers).unwrap();let plan=service.plan(now).unwrap();assert!(plan.requests.is_empty());assert_eq!(plan.deferred,1);
    service.next=Instant::now();let net=net();let progress=service.tick(net.handle(),now).unwrap();assert_eq!(progress.deferred,1);assert_eq!(progress.requested,0);assert_eq!(runtime.owner.failure(),None);
    runtime.owner.with(|client| {assert_eq!(client.query_one("select octet_length(original_filename) from files",&[])?.get::<_,i32>(0) as usize,filename.len());assert_eq!(client.query_one("select count(*) from segments",&[])?.get::<_,i64>(0),3);assert_eq!(client.query_one("select count(*) from inferred_ids",&[])?.get::<_,i64>(0),0);Ok(())}).unwrap();
    drop(service);runtime.owner.close();store.close().unwrap();
}

#[test]
#[ignore="requires real isolated PG/NXV and actual completed NNTP HEAD before commit reconciliation"]
fn native_inferred_bracketed_catalog_and_bare_retry_key_reconcile_one_real_committed_head() {
    let (_schema,runtime,_root,store,headers)=fixture();let now=1800000000;seed(&runtime,now,&[1,2,4]);let server=peer(now,false,None);let net=net();
    let pool=Arc::new(crate::nntp::Pool::new(&[server.config.clone()]).with_body_tries(Duration::from_secs(1),0));
    let mut service=Service::new(&runtime.owner,&runtime.db,pool.clone(),store.clone(),&headers).unwrap();let request=service.plan(now).unwrap().requests.pop().unwrap();
    let crate::nntp::HeadAnswer::Found(reply)=net.block_on(pool.background_head(&request.wanted.row.message_id)).unwrap() else {panic!("actual completed HEAD required")};
    let permit=service.reserve(net.handle()).unwrap();let releases=Service::parsed_admitted(&request,reply.bytes(),&permit).unwrap().unwrap();
    assert_eq!(releases[0].articles[0].message_id,"<part3of4.own@example.invalid>","real parser/writer identity is bracketed");
    let durable=service.journal.append(Service::record(&request,request.source.revision,&reply,now)).unwrap();
    assert!(matches!(service.accept(&request,request.source.revision,Outcome::Found,Some(&releases),now).unwrap(),Acceptance::Applied(_,Outcome::Found)),"a genuinely stored part must be Found");
    drop(releases);drop(permit);drop(durable);
    runtime.owner.with(|client| {
        let row=client.query_one("select s.message_id,s.part,f.original_filename from segments s join files f on(f.release_id,f.id)=(s.release_id,s.file_id) where s.part=3",&[])?;
        assert_eq!(row.try_get::<_,Vec<u8>>(0)?,b"<part3of4.own@example.invalid>");assert_eq!(row.try_get::<_,Option<i64>>(1)?,Some(3));assert_eq!(row.try_get::<_,Vec<u8>>(2)?,b"own.bin");
        assert_eq!(client.query_one("select message_id,tries from inferred_ids",&[])?.try_get::<_,String>(0)?,"part3of4.own@example.invalid");Ok(())
    }).unwrap();
    // Exact production sequence: the header/result COMMIT succeeded, while
    // the durable raw HEAD has not yet received its journal-consumed marker.
    let mut progress=Progress::default();service.recover(net.handle(),&mut progress,now+1).unwrap();
    assert_eq!(progress.found,0);assert_eq!(progress.replayed,1);assert!(service.journal.pending().unwrap().is_none());assert_eq!(server.hits.load(Ordering::Acquire),1,"covered recovery never re-fetches");
    runtime.owner.with(|client| {assert_eq!(client.query_one("select count(*) from segments",&[])?.try_get::<_,i64>(0)?,4);assert_eq!(client.query_one("select tries from inferred_ids",&[])?.try_get::<_,i64>(0)?,1);assert_eq!(client.query_one("select count(*) from inferred_ids where message_id='<part3of4.own@example.invalid>'",&[])?.try_get::<_,i64>(0)?,0);Ok(())}).unwrap();
    drop(service);runtime.owner.close();store.close().unwrap();
}

// A prior inconsistent catalogue advertisement is installed through the real
// native writer using the observed release identity. It is deliberately not
// proof that the later, genuine HEAD belongs to that advertised file/part.
fn conflicting_advertisement(service:&Service,net:&tokio::runtime::Runtime,wrong_file:bool,now:i64) {
    let request=service.plan(now).unwrap().requests.pop().unwrap();
    let crate::nntp::HeadAnswer::Found(reply)=net.block_on(service.pool.background_head(&request.wanted.row.message_id)).unwrap() else {panic!("actual completed HEAD required")};
    let permit=service.reserve(net.handle()).unwrap();let mut releases=Service::parsed_admitted(&request,reply.bytes(),&permit).unwrap().unwrap();
    let article=&mut releases[0].articles[0];
    if wrong_file {article.filename=Some("other.bin".into());article.subject="Own.Release - \"other.bin\" yEnc (3/4)".into();}
    else {article.part=Some(2);article.subject="Own.Release - \"own.bin\" yEnc (2/4)".into();}
    permit.check_batch(&releases).unwrap();service.owner.with(|client| {let mut tx=client.transaction()?;crate::pg::release_writer::save_in(&mut tx,&releases,now)?;tx.commit()?;Ok(())}).unwrap();
    drop(releases);drop(permit);
}
#[test]
#[ignore="requires actual PG writer ownership collision and an owned real NNTP HEAD"]
fn native_inferred_same_bracketed_mid_owned_by_another_file_is_not_carried_into_the_expected_file() {
    let (_schema,runtime,_root,store,headers)=fixture();let now=1800000000;seed(&runtime,now,&[1,2,4]);let server=peer(now,false,None);let net=net();
    let mut service=Service::new(&runtime.owner,&runtime.db,Arc::new(crate::nntp::Pool::new(&[server.config.clone()]).with_body_tries(Duration::from_secs(1),0)),store.clone(),&headers).unwrap();
    conflicting_advertisement(&service,&net,true,now);service.next=Instant::now();let progress=service.tick(net.handle(),now).unwrap();
    assert_eq!(progress.found,0);assert_eq!(progress.unusable,1);assert_eq!(server.hits.load(Ordering::Acquire),2);
    runtime.owner.with(|client| {let row=client.query_one("select f.original_filename,s.part from segments s join files f on(f.release_id,f.id)=(s.release_id,s.file_id) where s.message_id=$1",&[&b"<part3of4.own@example.invalid>".as_slice()])?;assert_eq!(row.try_get::<_,Vec<u8>>(0)?,b"other.bin");assert_eq!(row.try_get::<_,Option<i64>>(1)?,Some(3));assert_eq!(client.query_one("select count(*) from segments s join files f on(f.release_id,f.id)=(s.release_id,s.file_id) where f.original_filename=$1 and s.part=3",&[&b"own.bin".as_slice()])?.try_get::<_,i64>(0)?,0);Ok(())}).unwrap();
    drop(service);runtime.owner.close();store.close().unwrap();
}
#[test]
#[ignore="requires actual PG conflicting part metadata and an owned NNTP observer"]
fn native_inferred_same_bracketed_mid_with_the_wrong_stored_part_cannot_authorize_a_repair() {
    let (_schema,runtime,_root,store,headers)=fixture();let now=1800000000;seed(&runtime,now,&[1,2,4]);let server=peer(now,false,None);let net=net();
    let mut service=Service::new(&runtime.owner,&runtime.db,Arc::new(crate::nntp::Pool::new(&[server.config.clone()]).with_body_tries(Duration::from_secs(1),0)),store.clone(),&headers).unwrap();
    conflicting_advertisement(&service,&net,false,now);assert!(service.plan(now).unwrap().requests.is_empty(),"every own MID/part must prove the template");service.next=Instant::now();let progress=service.tick(net.handle(),now).unwrap();assert_eq!(progress.found,0);assert_eq!(progress.requested,0);assert_eq!(server.hits.load(Ordering::Acquire),1);
    runtime.owner.with(|client| {let row=client.query_one("select f.original_filename,s.part from segments s join files f on(f.release_id,f.id)=(s.release_id,s.file_id) where s.message_id=$1",&[&b"<part3of4.own@example.invalid>".as_slice()])?;assert_eq!(row.try_get::<_,Vec<u8>>(0)?,b"own.bin");assert_eq!(row.try_get::<_,Option<i64>>(1)?,Some(2));assert_eq!(client.query_one("select count(*) from segments where part=3",&[])?.try_get::<_,i64>(0)?,0);assert_eq!(client.query_one("select count(*) from inferred_ids",&[])?.try_get::<_,i64>(0)?,0);Ok(())}).unwrap();
    drop(service);runtime.owner.close();store.close().unwrap();
}
