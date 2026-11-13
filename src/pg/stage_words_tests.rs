use super::stage_words::{Delivery,open_index};
use crate::word_index::native::{Budget,Changed,Changes,SearchSource,Source,IndexCommitReceipt};
use std::{sync::{Arc,atomic::AtomicBool},time::{Instant,Duration}};

struct Fixture {runtime:super::indexer_runtime::Runtime,schema:super::testing::Schema,headers:crate::config::HeaderStoreConfig,config:crate::config::Config,storage:super::StorageConfig,_dir:tempfile::TempDir}
impl Fixture {
    fn new()->Option<Self> {
        let schema=super::testing::Schema::new()?;let dir=tempfile::tempdir().unwrap();
        let mut config=crate::config::Config::default();config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;
        config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;
        let storage=super::StorageConfig {backend:super::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:1,allow_plaintext:true};
        let headers=crate::config::HeaderStoreConfig {enabled:true,dir:dir.path().join("headers").to_str().unwrap().into()};
        let runtime=super::indexer_runtime::Runtime::open(&config,storage.clone(),headers.clone(),&[],&mut None).unwrap();
        Some(Self {runtime,schema,headers,config,storage,_dir:dir})
    }
    fn push(&self)->i64 {
        self.runtime.owner.with(|client| {
            super::release_writer::save(client,&[crate::parser::Release {name:"Example.Release".into(),group:"example.group".into(),articles:vec![crate::parser::Article {filename:Some("sample.bin".into()),message_id:"<part@example.test>".into(),part:Some(1),total_parts:Some(1),bytes:1,..Default::default()}],..Default::default()}],1)?;
            Ok(client.query_one("select id from releases",&[])?.get(0))
        }).unwrap()
    }
    fn pending(&self)->i64 {self.runtime.owner.with(|client|Ok(client.query_one("select count(*) from word_queue",&[])?.get(0))).unwrap()}
}
impl Drop for Fixture {fn drop(&mut self) {self.runtime.owner.close();}}

#[test]
fn native_word_owner_commits_real_documents_and_does_not_ack_an_empty_batch() {
    let Some(f)=Fixture::new() else {return};
    let index=open_index(&f.runtime.owner,&f.headers,&AtomicBool::new(false)).unwrap();
    let mut source=Delivery::for_owner(&f.runtime.owner).unwrap();
    assert_eq!(index.drain_native(&mut source,Budget::default()).unwrap(),0);
    assert_eq!(index.drain_native(&mut source,Budget::default()).unwrap(),0);
    f.push();assert_eq!(f.pending(),1);
    assert_eq!(index.drain_native(&mut source,Budget::default()).unwrap(),1);
    assert_eq!(f.pending(),0);assert_eq!(index.count_native(&mut source,"example",None,&[]).unwrap(),1);
}

struct AtAck {source:Delivery,owner:Arc<super::indexer_runtime::Owner>,id:i64,close:bool}
impl SearchSource for AtAck {
    fn stamp(&mut self)->anyhow::Result<[u8;32]> {self.source.stamp()}
    fn pending(&mut self)->anyhow::Result<bool> {self.source.pending()}
    fn visible_rows(&mut self,ids:&[i64],budget:Budget)->anyhow::Result<Vec<crate::search::ReleaseRow>> {self.source.visible_rows(ids,budget)}
}
impl Source for AtAck {
    fn changes(&mut self,budget:Budget)->anyhow::Result<Changes> {self.source.changes(budget)}
    fn acknowledge(&mut self,commit:&IndexCommitReceipt,claims:&[Changed])->anyhow::Result<()> {
        if self.close {self.owner.close();}
        else {self.owner.with(|client|{client.execute("update releases set display_name='Example.Changed' where id=$1",&[&self.id])?;Ok(())})?;}
        self.source.acknowledge(commit,claims)
    }
}
#[test]
fn native_word_compare_version_keeps_a_change_queued_during_real_commit() {
    let Some(f)=Fixture::new() else {return};let index=open_index(&f.runtime.owner,&f.headers,&AtomicBool::new(false)).unwrap();let id=f.push();
    let mut raced=AtAck {source:Delivery::for_owner(&f.runtime.owner).unwrap(),owner:f.runtime.owner.clone(),id,close:false};
    assert_eq!(index.drain_native(&mut raced,Budget::default()).unwrap(),1);
    assert_eq!(f.pending(),1,"the newer version must remain pending");
    let mut source=Delivery::for_owner(&f.runtime.owner).unwrap();
    assert_eq!(index.drain_native(&mut source,Budget::default()).unwrap(),1);assert_eq!(f.pending(),0);
    assert_eq!(index.count_native(&mut source,"changed",None,&[]).unwrap(),1);
    assert_eq!(index.count_native(&mut source,"release",None,&[]).unwrap(),0);
}
#[test]
fn native_word_commit_before_owner_loss_preserves_the_queue_without_an_ack() {
    let Some(f)=Fixture::new() else {return};let index=open_index(&f.runtime.owner,&f.headers,&AtomicBool::new(false)).unwrap();let id=f.push();
    let mut stopped=AtAck {source:Delivery::for_owner(&f.runtime.owner).unwrap(),owner:f.runtime.owner.clone(),id,close:true};
    assert!(index.drain_native(&mut stopped,Budget::default()).is_err());
    let mut client=f.schema.pool.get().unwrap();
    assert_eq!(client.query_one("select count(*) from word_queue",&[]).unwrap().get::<_,i64>(0),1);
    // Read-only recovery inspection observes the genuine completed index. It
    // cannot acknowledge or mint a second writer/owner session.
    let view=super::fresh_reader::ReadView::open(&mut client,&f.schema.name,super::fresh_reader::Budget::default(),Instant::now()+Duration::from_secs(3)).unwrap();
    let mut readonly=super::fresh_words::Search::open(&view).unwrap();
    let stamp=readonly.stamp().unwrap();let root=f.runtime.owner.epoch().unwrap().catalog().word_index_root(&f.headers).unwrap();
    let reader=crate::word_index::WordIndex::read_native(&root,stamp).unwrap();
    assert_eq!(reader.count_native(&mut readonly,"example",None,&[]).unwrap(),1);
}

#[test]
fn native_word_index_survives_nxv_recovery_and_expiry_in_reserved_state() {
    let Some(f)=Fixture::new() else {return};
    f.push();
    let index=open_index(&f.runtime.owner,&f.headers,&AtomicBool::new(false)).unwrap();
    let mut source=Delivery::for_owner(&f.runtime.owner).unwrap();
    assert_eq!(index.drain_native(&mut source,Budget::default()).unwrap(),1);
    let stamp=source.stamp().unwrap();
    let catalog=f.runtime.owner.epoch().unwrap().catalog();
    let root=catalog.header_config(&f.headers).unwrap().root();
    let words=catalog.word_index_root(&f.headers).unwrap();
    assert_eq!(words,root.join("_state").join("words"));
    drop(index); // Reopen from disk after real NXV recovery and expiry below.

    let mut config=crate::nxv_store::StoreConfig::new(&root);
    config.memory_budget=16*1024*1024;config.sealers=1;config.seal_rows=1;config.bounded_maps=true;
    let keep=crate::nxv_store::KeepSet {live_from:0,..Default::default()};
    let (store,_)=crate::nxv_store::Store::open(config.clone(),keep.clone()).unwrap();
    assert!(store.group_names().is_empty(),"word-index files must never be recovered as an NNTP group");
    let line=b"1\tExample.Release yEnc (1/1)\tposter@example.invalid\tSun, 08 Nov 2026 10:00:00 +0000\t<article@fixture>\t\t100\t1".to_vec();
    store.record("server.fixture","headers.fixture",&[line],crate::nxv_store::Clamp::Live {now:1_794_150_000})
        .unwrap().wait(Duration::from_secs(3)).unwrap();
    store.close().unwrap();
    assert_eq!(store.segments().len(),1,"the fixture must create an actual expirable NXV segment");
    store.set_keep(crate::nxv_store::KeepSet {live_from:i64::MAX/4,..Default::default()});
    store.arm_expiry();
    for _ in 0..3 {store.housekeeping();}
    assert!(store.segments().is_empty(),"actual unkept header data must expire in this control");
    drop(store);
    let (again,_)=crate::nxv_store::Store::open(config,keep).unwrap();
    assert_eq!(again.group_names(),vec!["headers.fixture".to_owned()]);
    again.close().unwrap();drop(again);
    let reader=crate::word_index::WordIndex::read_native(&words,stamp).unwrap();
    assert_eq!(reader.count_native(&mut source,"example",None,&[]).unwrap(),1,
        "NXV recovery/expiry must preserve the committed word index on disk");
}

struct KeeperCleanup(Option<super::stage_words::Keeper>);
impl Drop for KeeperCleanup {
    fn drop(&mut self) {if let Some(keeper)=self.0.take() {let _=keeper.finish();}}
}

#[test]
fn native_word_keeper_joins_blocked_delivery_before_owner_replacement() {
    let Some(f)=Fixture::new() else {return};
    let id=f.push();
    let index=open_index(&f.runtime.owner,&f.headers,&AtomicBool::new(false)).unwrap();
    drop(index); // Start the actual keeper with a ready index and pending delivery.
    let old_epoch=*f.runtime.owner.epoch().unwrap().id();
    let old_nonce=*f.runtime.owner.epoch().unwrap().catalog().nonce();
    let pid:i32=f.runtime.owner.with(|client|Ok(client.query_one("select pg_backend_pid()",&[])?.get(0))).unwrap();
    let mut locker=f.schema.pool.get().unwrap();
    let mut held=locker.transaction().unwrap();
    held.batch_execute("lock table word_queue in access exclusive mode").unwrap();
    let mut keeper=KeeperCleanup(Some(super::stage_words::Keeper::start(&f.runtime.owner,f.headers.clone()).unwrap()));
    let mut observer=f.schema.pool.get().unwrap();
    let deadline=Instant::now()+Duration::from_secs(3);
    loop {
        observer.query_one("select pg_stat_clear_snapshot()",&[]).unwrap();
        let blocked=observer.query_opt("select coalesce(wait_event_type='Lock' and query like '%from word_queue%',false) from pg_stat_activity where pid=$1",&[&pid])
            .unwrap().is_some_and(|row|row.get::<_,bool>(0));
        if blocked {break;}
        assert!(Instant::now()<deadline,"keeper never entered its real original-owner delivery query");
        std::thread::sleep(Duration::from_millis(10));
    }
    let running=keeper.0.take().unwrap();
    let (sent,received)=std::sync::mpsc::channel();
    let finisher=std::thread::spawn(move|| {sent.send(running.finish()).unwrap();});
    assert!(matches!(received.recv_timeout(Duration::from_millis(50)),Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
        "finish must join the actual in-flight keeper, not abandon its owner query");
    held.rollback().unwrap();
    received.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();finisher.join().unwrap();
    assert_eq!(f.runtime.owner.failure(),None,"normal keeper drain must not poison the owner");
    f.runtime.owner.close();
    let deadline=Instant::now()+Duration::from_secs(3);
    while observer.query_one("select exists(select 1 from pg_stat_activity where pid=$1)",&[&pid]).unwrap().get::<_,bool>(0) {
        assert!(Instant::now()<deadline,"closed original owner remained active");
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut identity=Some(f.runtime.identity.clone());
    let next=super::indexer_runtime::Runtime::open(&f.config,f.storage.clone(),f.headers.clone(),&[],&mut identity).unwrap();
    assert_ne!(next.owner.epoch().unwrap().id(),&old_epoch);
    assert_eq!(next.owner.epoch().unwrap().catalog().nonce(),&old_nonce);
    assert!(f.runtime.owner.with(|_| -> anyhow::Result<()> {panic!("old callback executed")}).is_err());
    next.owner.with(|client| {client.execute("update releases set display_name='Replacement.Visible' where id=$1",&[&id])?;Ok(())}).unwrap();
    let mut replacement=KeeperCleanup(Some(super::stage_words::Keeper::start(&next.owner,f.headers.clone()).unwrap()));
    let deadline=Instant::now()+Duration::from_secs(5);
    loop {
        let pending:i64=next.owner.with(|client|Ok(client.query_one("select count(*) from word_queue",&[])?.get(0))).unwrap();
        if pending==0 {break;}
        assert!(Instant::now()<deadline,"replacement keeper did not commit its own delivery");
        std::thread::sleep(Duration::from_millis(10));
    }
    replacement.0.take().unwrap().finish().unwrap();
    let mut source=Delivery::for_owner(&next.owner).unwrap();let stamp=source.stamp().unwrap();
    let root=next.owner.epoch().unwrap().catalog().word_index_root(&f.headers).unwrap();
    let reader=crate::word_index::WordIndex::read_native(&root,stamp).unwrap();
    assert_eq!(reader.count_native(&mut source,"replacement",None,&[]).unwrap(),1);
    next.owner.close();
}
