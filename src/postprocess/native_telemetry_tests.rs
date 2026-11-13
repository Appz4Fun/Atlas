//! Root runs each control in a fresh owned configured PG-home child.
use super::*;
use crate::{parser::{Article,Release},pg};
fn runtime()->pg::indexer_runtime::Runtime {
    assert_eq!(std::env::var("ATLAS_NATIVE_TELEMETRY_OWNED").as_deref(),Ok("1"));
    assert!(std::env::var_os("ATLAS_HOME").is_some(),"explicit isolated home");
    let storage=crate::config::checked_storage_config().expect("explicit owned storage");
    let mut cfg=crate::config::Config::default();cfg.postprocess.enabled=false;cfg.predb.enabled=false;cfg.spots.enabled=false;cfg.inferred_parts=false;
    cfg.media.tvmaze.enabled=false;cfg.media.allocine.enabled=false;cfg.media.details_per_minute=0;
    pg::indexer_runtime::Runtime::open(&cfg,storage,crate::config::HeaderStoreConfig {enabled:true,..Default::default()},&[],&mut None).unwrap()
}
fn populate(runtime:&pg::indexer_runtime::Runtime) {
    runtime.owner.with(|client| {
        pg::release_writer::save(client,&[Release {name:"Real.Telemetry.Row".into(),group:"alt.binaries.test".into(),
            articles:vec![Article {filename:Some("real.bin".into()),message_id:"<real-telemetry@test>".into(),part:Some(1),
                total_parts:Some(1),bytes:1,..Default::default()}],..Default::default()}],1)?;Ok(())
    }).unwrap();
}
fn runner(runtime:&pg::indexer_runtime::Runtime,handle:tokio::runtime::Handle)->Arc<Runner> {
    let mut cfg=crate::config::Config::default();cfg.postprocess.enabled=true;cfg.predb.enabled=false;cfg.spots.enabled=false;cfg.inferred_parts=false;
    cfg.media.tvmaze.enabled=false;cfg.media.allocine.enabled=false;cfg.media.details_per_minute=0;
    Arc::new(Runner::new(&runtime.owner,&cfg,Arc::new(crate::nntp::Pool::new(&[])),handle).unwrap())
}

#[test]
#[ignore="Root-only owned configured PostgreSQL home in a separate child"]
fn real_publication_updates_original_stats_and_the_genuine_bg_file_then_withdraws_on_capacity() {
    let runtime=runtime();populate(&runtime);let handle=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let runner=runner(&runtime,handle.handle().clone());let lanes=Arc::new(LaneCounters::new(runner.steps.enabled as u64).unwrap());
    let producer=Producer::new(&runner,lanes).unwrap();assert!(matches!(producer.refresh().unwrap(),Refresh::Published));
    assert_eq!(runner.stats.queue_due.load(Ordering::Relaxed),1);assert_eq!(crate::postprocess::stats::backlog(),1);
    assert_eq!(snapshot().unwrap()["queue_due"],1);assert_eq!(backlog(),Some(1));
    // Invoke the same sole serializer/writer as the actual BG report loop.
    // No fixture writes a JSON document or supplies telemetry totals.
    crate::bg_indexer::Stats::new().write("","backfill",true,false);
    let file:Value=serde_json::from_slice(&std::fs::read(crate::paths::stats_file()).unwrap()).unwrap();
    assert_eq!(file["postprocess"]["queue_due"],1);assert!(file["postprocess"]["walk"].is_object());
    assert!(file["postprocess"]["net_claims"]["running"].is_array());
    runtime.owner.with(|client|{client.batch_execute("insert into pp_walks(kind,top_id,before_id,redo_mask,created_at)
        select 'request',t,t,0,1 from generate_series(1,129), (select max(id)+1 t from releases) maximum")?;Ok(())}).unwrap();
    assert!(matches!(producer.refresh().unwrap(),Refresh::Deferred(_)));
    assert_eq!(runner.stats.queue_due.load(Ordering::Relaxed),1,"a budget must not replace real stats with zero");
    assert!(snapshot().is_none());assert!(backlog().is_none());assert_eq!(runtime.owner.failure(),None);
    crate::bg_indexer::Stats::new().write("","backfill",true,false);
    let file:Value=serde_json::from_slice(&std::fs::read(crate::paths::stats_file()).unwrap()).unwrap();
    assert!(file["postprocess"].is_null(),"unavailable observations must not gain a fresh valid payload");
    drop(producer);runtime.owner.close();
}

#[test]
#[ignore="Root-only owned configured PostgreSQL home in a separate child"]
fn a_stopped_or_failed_original_owner_cannot_publish_its_last_stats_as_current() {
    let runtime=runtime();populate(&runtime);let handle=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let runner=runner(&runtime,handle.handle().clone());let producer=Producer::new(&runner,Arc::new(LaneCounters::new(runner.steps.enabled as u64).unwrap())).unwrap();
    assert!(matches!(producer.refresh().unwrap(),Refresh::Published));assert!(snapshot().is_some());
    runtime.owner.stop.store(true,Ordering::Release);
    assert!(snapshot().is_none());assert!(backlog().is_none());
    assert!(pg::outage::is_attempt_ended(&producer.refresh().unwrap_err()));assert_eq!(runtime.owner.failure(),None);
    assert_eq!(runner.stats.queue_due.load(Ordering::Relaxed),1);drop(producer);runtime.owner.close();
}
