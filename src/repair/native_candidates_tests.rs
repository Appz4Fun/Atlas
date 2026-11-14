//! Genuine canonical writer/observed-link controls for assembled post eligibility.
//! No file_timing/release-size SQL injection, provider or skipped-success route.
use super::*;
use crate::pg::stage_queue::{Queue,Settlement,Visit,participants::ReadTargets};
use crate::postprocess::{apply::Applied,step::{StepMutation,StepWrite}};
use sha2::{Digest,Sha256};
const GROUP:&str="alt.binaries.owned.post.candidates";
const MIB:i64=1<<20;
struct Fixture {
    runtime:crate::pg::indexer_runtime::Runtime,schema:crate::pg::testing::Schema,_root:tempfile::TempDir,
    cfg:crate::config::Config,storage:crate::pg::StorageConfig,headers:crate::config::HeaderStoreConfig,
    net:Option<tokio::runtime::Runtime>,store:Arc<Store>,now:i64,
}
impl Fixture {
    fn new()->Self {
        let schema=crate::pg::testing::Schema::new().expect("explicit isolated ATLAS_PG_TEST_URL; never skipped success");
        let root=tempfile::tempdir().unwrap();let mut cfg=crate::config::Config::default();
        cfg.postprocess.enabled=false;cfg.predb.enabled=false;cfg.spots.enabled=false;cfg.inferred_parts=false;
        cfg.media.tvmaze.enabled=false;cfg.media.allocine.enabled=false;cfg.media.details_per_minute=0;
        let storage=crate::pg::StorageConfig {backend:crate::pg::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:1,allow_plaintext:true};
        let headers=crate::config::HeaderStoreConfig {enabled:true,dir:root.path().to_str().unwrap().into()};
        let runtime=crate::pg::indexer_runtime::Runtime::open(&cfg,storage.clone(),headers.clone(),&[],&mut None).unwrap();
        crate::nxv_store::live::spawn_bounded(&runtime.header_config().unwrap()).unwrap();let store=crate::nxv_store::live::handle().unwrap();
        let net=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        Self {runtime,schema,_root:root,cfg,storage,headers,net:Some(net),store,now:2_000_000_000}
    }
    fn service(&self)->Service {
        let ctx=PassContext {pool:Arc::new(crate::nntp::Pool::new(&[])),states:Default::default(),stop:self.runtime.owner.stop.clone(),verbose:false};
        let mut service=Service::new(&self.runtime.owner,&self.runtime.db,ctx,self.net.as_ref().unwrap().handle().clone(),self.store.clone(),&self.headers,Default::default()).unwrap();
        service.next=Instant::now();service
    }
    fn row(&self,tag:&str,name:&str,group:&str,posted:i64,parts:std::ops::RangeInclusive<i64>,bytes:i64)->i64 {
        let date=chrono::DateTime::from_timestamp(posted,0).unwrap().to_rfc2822();
        let release=crate::parser::Release {name:name.into(),group:group.into(),poster:"owned poster".into(),date:date.clone(),
            articles:parts.map(|part|crate::parser::Article {filename:Some(format!("{name}.rar")),subject:format!("\"{name}.rar\" yEnc ({part}/20)"),
                author:"owned poster".into(),date:date.clone(),message_id:format!("<{tag}.{part}@candidate.fixture.test>"),part:Some(part),total_parts:Some(20),
                file_total:Some(1),number:part as u64,bytes,..Default::default()}).collect(),..Default::default()};
        self.runtime.owner.with(|client| {
            crate::pg::release_writer::save(client,&[release],posted)?;
            Ok(client.query_one("select id from releases where group_name=$1 and original_name=$2",&[&group,&name.as_bytes()])?.get(0))
        }).unwrap()
    }
    fn links(&self,ids:&[i64]) {
        let lead=ids[0];let mut pending=ids.iter().enumerate().map(|(n,&id)|(id,n as u8+1)).collect::<BTreeMap<_,_>>();
        let queue=Queue::for_owner(&self.runtime.owner).unwrap();queue.request_walk(4,Some(GROUP),self.now).unwrap();
        for _ in 0..512 {
            queue.walk_tick(self.now).unwrap();let mut claims=queue.claim(self.now,256,60).unwrap();claims.sort_by_key(|c|c.release_id()!=lead);
            for claim in claims {
                let source=claim.release_id();let file=pending.get(&source).copied();let targets=if file.is_some(){vec![lead]}else{Vec::new()};
                let mut observed=ReadTargets::for_owner(&self.runtime.owner).unwrap();
                let (rerun,input)=self.runtime.owner.with(|client| {
                    let mut tx=client.build_transaction().isolation_level(IsolationLevel::RepeatableRead).read_only(true).start()?;let mut hash=Sha256::new();
                    for row in tx.query("select id,native_work_revision,original_name from releases where id=$1 or id=any($2) order by id",&[&source,&targets])? {
                        hash.update(row.get::<_,i64>(0).to_le_bytes());hash.update(row.get::<_,i64>(1).to_le_bytes());hash.update(row.get::<_,Vec<u8>>(2));
                    }
                    let rerun=tx.query_one("select coalesce((select rerun_gen from pp_state where release_id=$1),0)",&[&source])?.get::<_,i64>(0);
                    observed.observe_in(&mut tx,&targets)?;tx.commit()?;Ok((rerun,<[u8;32]>::from(hash.finalize())))
                }).unwrap();
                let writes=if let Some(file)=file{vec![(4,StepWrite::for_mutation(source,StepMutation::LinkUploadData {member_id:source,lead_id:lead,par2_file:[file;16]}))]}else{Vec::new()};
                let applied=Applied {release_id:source,claimed_gen:rerun,clear:claim.redo_mask(),set_done:4,writes,..Default::default()};
                assert_eq!(queue.settle(&claim,Visit::prepare_observed(&claim,applied,input,observed.finish()).unwrap(),self.now+1).unwrap(),Settlement::Applied);
                if file.is_some(){pending.remove(&source);}
            }
            if pending.is_empty(){return}
        }panic!("actual observed link was never claimed");
    }
    fn linked(&self,tag:&str,posted:i64)->Vec<i64> {
        let ids=(0..20).map(|n|self.row(&format!("{tag}.{n}"),&format!("{tag}.2160p.Remux.file{n:02}"),GROUP,posted,1..=19,3*MIB)).collect::<Vec<_>>();
        self.links(&ids);
        self.runtime.owner.with(|client| {
            let rows=client.query("select size,complete from releases where id=any($1)",&[&ids])?;assert_eq!(rows.len(),20);
            assert!(rows.iter().all(|r|r.get::<_,i64>(0)==57*MIB&&!r.get::<_,bool>(1)));Ok(())
        }).unwrap();
        let post=test_whole_post(&self.runtime.owner,ids[0]).unwrap().unwrap();assert_eq!(post.ids,ids);assert_eq!(post.size,1140*MIB);
        assert_eq!(test_permille(&self.runtime.owner,&ids).unwrap(),950);ids
    }
}
impl Drop for Fixture {fn drop(&mut self){self.runtime.owner.stop.store(true,Ordering::Release);drop(self.net.take());self.runtime.owner.close();crate::nxv_store::live::finish();}}
fn isolated(name:&str,work:fn()) {
    const CHILD:&str="ATLAS_TEST_NATIVE_POST_CANDIDATES";if std::env::var(CHILD).ok().as_deref()==Some(name){work();return}
    let home=tempfile::tempdir().unwrap();let mut child=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact",name,"--ignored","--nocapture","--test-threads=1"])
        .env_clear().env(CHILD,name).env("ATLAS_HOME",home.path()).env("HOME",home.path()).env("ATLAS_PG_TEST_URL",std::env::var("ATLAS_PG_TEST_URL").expect("explicit isolated PostgreSQL required")).spawn().unwrap();
    let until=Instant::now()+Duration::from_secs(60);loop {if let Some(status)=child.try_wait().unwrap(){assert!(status.success(),"actual candidate child failed: {status}");return}
        if Instant::now()>=until{child.kill().unwrap();child.wait().unwrap();panic!("actual candidate fixture deadline exceeded")}std::thread::sleep(Duration::from_millis(10));}
}
#[test]
#[ignore="explicit isolated PG; real sub64MiB native rows/observed links/Timing"]
fn recent_and_older_candidates_use_genuine_assembled_post_size() {isolated("repair::native::candidate_tests::recent_and_older_candidates_use_genuine_assembled_post_size",|| {
    let f=Fixture::new();let recent=f.linked("Recent.Assembly",f.now-3*3600);let older=f.linked("Older.Assembly",f.now-7*86400);
    let selected=storage::candidate_plan(&f.runtime.owner,f.now,&mut Service::budget()).unwrap();
    let picked_recent=selected.recent.keys.iter().map(|(id,_)|*id).collect::<Vec<_>>();let picked_older=&selected.found.older[0].keys;
    assert_eq!(picked_recent.len(),1,"recent qualification must assemble all real sub64MiB linked rows first");
    assert_eq!(picked_older.len(),1,"older qualification must use the same actual post-size predicate");
    assert!(recent.contains(&picked_recent[0]));assert!(older.contains(&picked_older[0]));
    let plan=f.service().plan(f.now).unwrap();assert_eq!(plan.found.posts.len(),2);
    assert!(plan.found.posts.iter().all(|p|p.size==1140*MIB&&p.permille==950&&!p.windows.is_empty()));
});}
#[test]
#[ignore="explicit isolated PG; actual small/nonmedia/overlapping native rows"]
fn candidate_qualification_refuses_small_and_overlapping_pseudo_posts() {isolated("repair::native::candidate_tests::candidate_qualification_refuses_small_and_overlapping_pseudo_posts",|| {
    let f=Fixture::new();f.row("small","Tiny.Archive",GROUP,f.now-3*3600,1..=1,MIB);
    // Twenty genuine same-part copies have a gross row sum over1GiB but no
    // complementary or verified-link identity establishing one large post.
    for n in 0..20 {f.row(&format!("copy{n}"),"Repeated.Copy",&format!("alt.binaries.owned.copy{n}"),f.now-3*3600,1..=19,3*MIB);}
    let nonmedia=f.row("nonmedia","Plain.Archive.Index",GROUP,f.now-3*3600,1..=19,60*MIB);
    let plan=f.service().plan(f.now).unwrap();assert_eq!(plan.found.posts.len(),1);
    assert_eq!(plan.found.posts[0].ids,[nonmedia]);assert!(!plan.found.posts[0].media);assert!(!plan.found.posts[0].windows.is_empty());
    f.runtime.owner.with(|c|{assert_eq!(c.query_one("select count(*) from repair_tried",&[])?.get::<_,i64>(0),0);Ok(())}).unwrap();
});}
#[test]
#[ignore="explicit isolated PG; bounded small-row prefix and durable owned continuation"]
fn recent_continuation_reaches_a_large_post_behind_two_hundred_small_rows() {isolated("repair::native::candidate_tests::recent_continuation_reaches_a_large_post_behind_two_hundred_small_rows",|| {
    let f=Fixture::new();let target=f.linked("Behind.Small.Prefix",f.now-3*3600);
    for n in 0..200 {f.row(&format!("noise{n}"),&format!("Small.Noise.{n:03}"),GROUP,f.now-3*3600,1..=1,MIB);}
    let mut service=f.service();let first=service.tick(f.now).unwrap();assert_eq!(first.posts,0);
    let before=f.runtime.owner.with(|c| {
        assert_eq!(c.query_one("select count(*) from repair_tried",&[])?.get::<_,i64>(0),0,"small observations are revisitable, never permanently tried");
        Ok(c.query_one("select value from meta where key='native_repair_recent_before'",&[])?.get::<_,Option<i64>>(0))
    }).unwrap().expect("the classified prefix has a durable operational continuation");
    assert!(before>target[target.len()-1]);
    let mut reached=false;
    for _ in 0..8 {
        let next=service.plan(f.now).unwrap();
        if !next.found.posts.is_empty() {
            assert_eq!(next.found.posts.len(),1);assert_eq!(next.found.posts[0].ids,target);
            assert_eq!(next.recent.next(&HashSet::new()),Some(target[target.len()-1]+1),"an unhandled qualified post cannot be skipped by cursor commit");
            reached=true;break;
        }
        let older=next.found.older.iter().map(|o|(o.shard,o.end)).collect::<Vec<_>>();
        service.mark_progress(&[],f.now,&[],&[],&older,Some(next.recent.next(&HashSet::new()))).unwrap();
    }
    assert!(reached,"bounded classified-prefix continuation must reach the actual large post");
});}
#[test]
#[ignore="explicit isolated PG; writer growth and wrap reconsider a previously small post"]
fn recent_wrap_reconsiders_small_posts_that_grow_without_fabricating_tried_rows() {isolated("repair::native::candidate_tests::recent_wrap_reconsiders_small_posts_that_grow_without_fabricating_tried_rows",|| {
    let f=Fixture::new();let id=f.row("growth","Growing.Archive",GROUP,f.now-3*3600,1..=1,60*MIB);let mut service=f.service();
    service.tick(f.now).unwrap();service.next=Instant::now();service.tick(f.now).unwrap();
    f.runtime.owner.with(|c|{assert_eq!(c.query_one("select value is null from meta where key='native_repair_recent_before'",&[])?.get::<_,bool>(0),true);Ok(())}).unwrap();
    assert_eq!(f.row("growth","Growing.Archive",GROUP,f.now-3*3600,2..=19,60*MIB),id);
    let plan=service.plan(f.now).unwrap();assert_eq!(plan.found.posts.len(),1);assert_eq!(plan.found.posts[0].ids,[id]);assert_eq!(plan.found.posts[0].size,1140*MIB);
    f.runtime.owner.with(|c|{assert_eq!(c.query_one("select count(*) from repair_tried",&[])?.get::<_,i64>(0),0);Ok(())}).unwrap();
});}
#[test]
#[ignore="explicit isolated PG; genuine bounded overlap/refusal leaves cursor reachable"]
fn deferred_candidate_keeps_the_cursor_before_unadmitted_source_without_poisoning_owner() {isolated("repair::native::candidate_tests::deferred_candidate_keeps_the_cursor_before_unadmitted_source_without_poisoning_owner",|| {
    let f=Fixture::new();let id=f.row("wide","Unadmitted.Source",GROUP,f.now-3*3600,1..=1,MIB);
    // Malformed operator/catalog payload is a genuine pre-transfer extent
    // refusal; the implementation may not claim its classification complete.
    f.runtime.owner.with(|c|{c.execute("update releases set original_name=$1 where id=$2",&[&vec![b'x';65537],&id])?;Ok(())}).unwrap();
    let mut service=f.service();let progress=service.tick(f.now).unwrap();assert!(progress.deferred>0);assert_eq!(f.runtime.owner.failure(),None);
    f.runtime.owner.with(|c|{assert_eq!(c.query_one("select value from meta where key='native_repair_recent_before'",&[])?.get::<_,i64>(0),id+1);
        assert_eq!(c.query_one("select count(*) from repair_tried",&[])?.get::<_,i64>(0),0);Ok(())}).unwrap();
});}
#[test]
#[ignore="explicit isolated PG; existing catalog restart accepts its owned cursor without pristine adoption"]
fn recent_cursor_survives_canonical_restart_without_weakening_fresh_catalog_admission() {isolated("repair::native::candidate_tests::recent_cursor_survives_canonical_restart_without_weakening_fresh_catalog_admission",|| {
    let mut f=Fixture::new();f.row("restart","Small.Restart",GROUP,f.now-3*3600,1..=1,MIB);let mut service=f.service();service.tick(f.now).unwrap();drop(service);
    let nonce=*f.runtime.owner.epoch().unwrap().catalog().nonce();let mut expected=Some(f.runtime.identity.clone());
    let before=f.runtime.owner.with(|c|Ok(c.query_one("select value from meta where key='native_repair_recent_before'",&[])?.get::<_,Option<i64>>(0))).unwrap();
    f.runtime.owner.close();f.runtime=crate::pg::indexer_runtime::Runtime::open(&f.cfg,f.storage.clone(),f.headers.clone(),&[],&mut expected).unwrap();
    assert_eq!(f.runtime.owner.epoch().unwrap().catalog().nonce(),&nonce);
    f.runtime.owner.with(|c|{assert_eq!(c.query_one("select value from meta where key='native_repair_recent_before'",&[])?.get::<_,Option<i64>>(0),before);Ok(())}).unwrap();
    let fresh=crate::pg::testing::Schema::new().expect("explicit fresh-catalog negative control");
    fresh.pool.get().unwrap().execute("insert into meta(key,value) values('native_repair_recent_before',42)",&[]).unwrap();
    let foreign=crate::pg::StorageConfig {schema:fresh.name.clone(),url:fresh.url.clone(),..f.storage.clone()};
    let result=crate::pg::indexer_runtime::Runtime::open(&f.cfg,foreign,f.headers.clone(),&[],&mut None);
    assert!(result.err().expect("a preidentity cursor must not be adopted").to_string().contains("refuses preexisting"));
});}
#[test]
#[ignore="explicit isolated PG; stale qualified input atomically refuses cursor and tried publication"]
fn changed_qualified_source_cannot_commit_a_recent_cursor_past_its_observation() {isolated("repair::native::candidate_tests::changed_qualified_source_cannot_commit_a_recent_cursor_past_its_observation",|| {
    let f=Fixture::new();let id=f.row("stale","Actual.Qualified.Remux",GROUP,f.now-3*3600,1..=19,60*MIB);let service=f.service();let plan=service.plan(f.now).unwrap();
    let sources=plan.sources.values().cloned().collect::<Vec<_>>();let empty=HashSet::new();let pending=plan.recent.next(&empty);assert_eq!(pending,Some(id+1));
    service.mark_progress(&sources,f.now,&[],&[],&[],Some(pending)).unwrap();
    // A genuinely accepted twentieth part changes the observed work revision;
    // no field is edited to manufacture a stale/final outcome.
    assert_eq!(f.row("stale","Actual.Qualified.Remux",GROUP,f.now-3*3600,20..=20,60*MIB),id);
    let completed=HashSet::from([0usize]);
    let error=service.mark_progress(&sources,f.now,&[],&[(id,950)],&[],Some(plan.recent.next(&completed))).unwrap_err();
    assert!(error.is::<SourceChanged>());assert_eq!(f.runtime.owner.failure(),None);
    f.runtime.owner.with(|c|{assert_eq!(c.query_one("select value from meta where key='native_repair_recent_before'",&[])?.get::<_,Option<i64>>(0),pending);
        assert_eq!(c.query_one("select count(*) from repair_tried",&[])?.get::<_,i64>(0),0);Ok(())}).unwrap();
    f.runtime.owner.close();
    let ended=service.mark_progress(&sources,f.now,&[],&[(id,950)],&[],Some(None)).unwrap_err();assert!(crate::pg::outage::is_attempt_ended(&ended));
    assert_eq!(f.schema.pool.get().unwrap().query_one("select value from meta where key='native_repair_recent_before'",&[]).unwrap().get::<_,Option<i64>>(0),pending);
});}
