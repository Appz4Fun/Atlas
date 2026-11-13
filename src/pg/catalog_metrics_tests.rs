//! Source-first controls; execution is reserved for Root's isolated PG gate.
use super::*;
use crate::pg::{self,fresh_reader::Budget};
use std::time::{Duration,Instant};

fn view<'a>(client:&'a mut postgres::Client,schema:&str)->ReadView<'a> {
    ReadView::open(client,schema,Budget {rows:2,bytes:1024*1024},Instant::now()+Duration::from_secs(3)).unwrap()
}

#[test]
#[ignore="Root-only isolated PostgreSQL and native runtime admission"]
fn native_catalog_walk_reads_real_rows_feeds_and_finite_pages() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PostgreSQL");
    let runtime=pg::stage_queue_tests::runtime(&schema);pg::stage_queue_tests::push(&runtime);
    runtime.owner.with(|client| {
        client.batch_execute("insert into ext_predb_feeds(source,key) values(7,'feed-seven');
            insert into ext_predb_titles(id,title,source,added_at) values(1,'Example.Release',7,1);
            insert into predb_match(release_id,title_id,kind,category,matched_at) select id,1,1,'Movies',1 from releases;
            insert into release_category(release_id,category) select id,2000 from releases")?;Ok(())
    }).unwrap();
    let destination=runtime.owner.cache_destination().unwrap();let catalog=runtime.owner.epoch().unwrap().catalog();
    let stop=AtomicBool::new(false);let mut client=schema.pool.get().unwrap();
    let mut walk={let view=view(&mut client,&schema.name);Scan::begin(&view,destination,&destination.storage,catalog,&stop).unwrap()};
    // A later insert cannot make the current walk chase an advancing tail.
    runtime.owner.with(|client| {
        client.execute("insert into releases(group_name,is_collection,identity_key,name,poster,posted_date,is_obfuscated)
            select 'example.group',false,decode(lpad(to_hex(n),64,'0'),'hex'),'Later.Release.'||n,'p','d',false
            from generate_series(1,3) n",&[])?;Ok(())
    }).unwrap();
    assert!(!walk.is_complete());
    {let view=view(&mut client,&schema.name);walk.page(&view,&destination.storage,&stop).unwrap();}
    assert!(walk.is_complete());
    let completed=walk.finish(&stop).unwrap();assert_eq!(completed.tally.rows,1);assert!(completed.relation_bytes>0);
    completed.publish(Duration::from_millis(1));
    assert!(crate::metrics::names::DB_BYTES.rows().contains(&("file=\"native_schema\",kind=\"relations\"".into(),completed.relation_bytes as f64)));
    assert!(crate::metrics::names::CATALOG_PREDB_FEED.rows().contains(&("feed=\"feed-seven\"".into(),1.0)));
    assert!(crate::metrics::names::CATALOG_PREDB_KIND.rows().contains(&("kind=\"exact\"".into(),1.0)));
    assert!(crate::metrics::names::CATALOG_GROUP_BYTES.rows().contains(&("group=\"example.group\"".into(),1.0)));
    let mut walk={let view=view(&mut client,&schema.name);Scan::begin(&view,destination,&destination.storage,catalog,&stop).unwrap()};
    while !walk.is_complete() {let view=view(&mut client,&schema.name);walk.page(&view,&destination.storage,&stop).unwrap();}
    assert_eq!(walk.finish(&stop).unwrap().tally.rows,4);
    drop(client);runtime.owner.close();
}

#[test]
#[ignore="Root-only isolated PostgreSQL and native runtime admission"]
fn native_catalog_walk_refuses_stop_destination_drift_and_oversized_text() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PostgreSQL");
    let runtime=pg::stage_queue_tests::runtime(&schema);pg::stage_queue_tests::push(&runtime);
    let destination=runtime.owner.cache_destination().unwrap();let catalog=runtime.owner.epoch().unwrap().catalog();
    let stop=AtomicBool::new(false);let mut client=schema.pool.get().unwrap();
    let mut walk={let view=view(&mut client,&schema.name);Scan::begin(&view,destination,&destination.storage,catalog,&stop).unwrap()};
    stop.store(true,Ordering::Release);
    let error=walk.page(&view(&mut client,&schema.name),&destination.storage,&stop).unwrap_err();
    assert!(pg::outage::is_attempt_ended(&error));assert!(walk.finish(&stop).is_err());
    stop.store(false,Ordering::Release);
    let mut walk={let view=view(&mut client,&schema.name);Scan::begin(&view,destination,&destination.storage,catalog,&stop).unwrap()};
    let mut changed=destination.storage.clone();changed.schema="another_schema".into();
    assert!(walk.page(&view(&mut client,&schema.name),&changed,&stop).is_err());
    runtime.owner.with(|client| {client.execute("update releases set name=repeat('x',2000000)",&[])?;Ok(())}).unwrap();
    let mut walk={let view=view(&mut client,&schema.name);Scan::begin(&view,destination,&destination.storage,catalog,&stop).unwrap()};
    assert!(walk.page(&view(&mut client,&schema.name),&destination.storage,&stop).is_err());
    assert!(walk.finish(&stop).is_err());drop(client);runtime.owner.close();
}

#[test]
#[ignore="Root-only isolated PostgreSQL and native runtime admission"]
fn native_catalog_walk_does_not_turn_query_or_identity_failure_into_empty_success() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PostgreSQL");
    let runtime=pg::stage_queue_tests::runtime(&schema);
    let destination=runtime.owner.cache_destination().unwrap();let catalog=runtime.owner.epoch().unwrap().catalog();
    let stop=AtomicBool::new(false);let mut client=schema.pool.get().unwrap();
    let mut walk={let view=view(&mut client,&schema.name);Scan::begin(&view,destination,&destination.storage,catalog,&stop).unwrap()};
    assert!(!walk.is_complete());
    {let view=view(&mut client,&schema.name);walk.page(&view,&destination.storage,&stop).unwrap();}
    assert_eq!(walk.finish(&stop).unwrap().tally.rows,0);
    let mut canceled={let view=view(&mut client,&schema.name);Scan::begin(&view,destination,&destination.storage,catalog,&stop).unwrap()};
    {let view=view(&mut client,&schema.name);canceled.page(&view,&destination.storage,&stop).unwrap();}
    stop.store(true,Ordering::Release);assert!(canceled.finish(&stop).is_err());stop.store(false,Ordering::Release);
    let mut walk={let view=view(&mut client,&schema.name);Scan::begin(&view,destination,&destination.storage,catalog,&stop).unwrap()};
    runtime.owner.with(|client|{client.batch_execute("alter table releases rename to unavailable_releases")?;Ok(())}).unwrap();
    let error=walk.page(&view(&mut client,&schema.name),&destination.storage,&stop).unwrap_err();
    assert!(error.chain().any(|source|source.is::<postgres::Error>()));assert!(walk.finish(&stop).is_err());
    runtime.owner.with(|client|{client.batch_execute("alter table unavailable_releases rename to releases;
        update native_catalog_identity set header_root=header_root||'-drift'")?;Ok(())}).unwrap();
    assert!(Scan::begin(&view(&mut client,&schema.name),destination,&destination.storage,catalog,&stop).is_err());
    drop(client);runtime.owner.close();
}

#[test]
#[ignore="Root-only isolated PostgreSQL and native runtime admission"]
fn native_catalog_walk_query_deadline_keeps_the_driver_cause_and_no_completed_result() {
    let schema=pg::testing::Schema::new().expect("explicit isolated PostgreSQL");
    let runtime=pg::stage_queue_tests::runtime(&schema);pg::stage_queue_tests::push(&runtime);
    let destination=runtime.owner.cache_destination().unwrap();let catalog=runtime.owner.epoch().unwrap().catalog();
    let stop=AtomicBool::new(false);let mut client=schema.pool.get().unwrap();
    let mut walk={let view=view(&mut client,&schema.name);Scan::begin(&view,destination,&destination.storage,catalog,&stop).unwrap()};
    let mut locker=schema.pool.get().unwrap();let mut held=locker.transaction().unwrap();
    held.batch_execute("lock table releases in access exclusive mode").unwrap();
    let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_millis(200)).unwrap();
    let error=walk.page(&view,&destination.storage,&stop).unwrap_err();
    assert!(error.chain().filter_map(|source|source.downcast_ref::<postgres::Error>())
        .any(|error|error.code()==Some(&postgres::error::SqlState::QUERY_CANCELED)));
    assert!(walk.finish(&stop).is_err());drop(view);held.rollback().unwrap();drop(locker);drop(client);runtime.owner.close();
}

#[test]
#[ignore="Root-only isolated PostgreSQL and native runtime admission"]
fn native_catalog_observer_reports_completed_size_and_joins_before_owner_close() {
    let runtime=configured_observer_runtime();pg::stage_queue_tests::push(&runtime);
    let (sent,received)=std::sync::mpsc::channel();
    let observer=Observer::start(&runtime.owner,Duration::from_secs(60),move |bytes|{sent.send(bytes).unwrap();}).unwrap();
    assert!(received.recv_timeout(Duration::from_secs(5)).unwrap()>0);
    observer.finish().unwrap();assert!(!runtime.owner.stop.load(Ordering::Acquire));
    assert_eq!(runtime.owner.failure(),None);
    runtime.owner.with(|client|{assert_eq!(client.query_one("select 1",&[])?.get::<_,i32>(0),1);Ok(())}).unwrap();
    runtime.owner.close();
}

#[test]
#[ignore="Root-only isolated PostgreSQL and native runtime admission"]
fn native_catalog_observer_failure_never_calls_completed_and_preserves_owner_failure() {
    let runtime=configured_observer_runtime();pg::stage_queue_tests::push(&runtime);
    runtime.owner.with(|client|{client.execute("update releases set size=-1",&[])?;Ok(())}).unwrap();
    let calls=std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));let completed=calls.clone();
    let observer=Observer::start(&runtime.owner,Duration::from_secs(60),move |_|{completed.fetch_add(1,Ordering::Relaxed);}).unwrap();
    let until=Instant::now()+Duration::from_secs(5);
    while !runtime.owner.stop.load(Ordering::Acquire)&&Instant::now()<until {std::thread::sleep(Duration::from_millis(10));}
    assert_eq!(runtime.owner.failure(),Some(pg::outage::Failure::Fatal));
    assert!(observer.finish().is_err());assert_eq!(calls.load(Ordering::Acquire),0);runtime.owner.close();
}

#[test]
#[ignore="Root-only owned configured PostgreSQL home in a separate child"]
fn native_catalog_observer_private_cancel_inside_view_keeps_owner_usable() {
    let runtime=configured_observer_runtime();let local=AtomicBool::new(false);
    let result:Result<()>=observer_read(&runtime.owner,&local,|_,_| {
        local.store(true,Ordering::Release);stopping(&local)
    });
    assert!(pg::outage::is_attempt_ended(&result.unwrap_err()));
    assert!(!runtime.owner.stop.load(Ordering::Acquire));assert_eq!(runtime.owner.failure(),None);
    runtime.owner.with(|client|{assert_eq!(client.query_one("select 1",&[])?.get::<_,i32>(0),1);Ok(())}).unwrap();
    runtime.owner.close();
}

// Observer checks the actual configured destination on every page. Root must
// run each observer control in a separate child with its own fresh PG schema
// and ATLAS_HOME config, rather than mutate process-global configuration here.
fn configured_observer_runtime()->pg::indexer_runtime::Runtime {
    assert_eq!(std::env::var("ATLAS_METRICS_TEST_OWNED").as_deref(),Ok("1"));
    assert!(std::env::var_os("ATLAS_HOME").is_some(),"explicit Root-owned home required");
    let storage=crate::config::checked_storage_config().expect("Root-owned configured PG destination");
    let mut config=crate::config::Config::default();
    config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;
    config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;
    let headers=crate::config::HeaderStoreConfig {enabled:true,..Default::default()};
    pg::indexer_runtime::Runtime::open(&config,storage,headers,&[],&mut None).unwrap()
}
