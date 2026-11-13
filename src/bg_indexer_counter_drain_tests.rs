//! Root runs this selector in its own configured home; no fixture JSON is written.
use super::*;

#[test]
#[ignore="Root-only isolated home; exercises the genuine sole BG writer"]
fn final_service_counters_survive_the_later_index_stop_write() {
    assert_eq!(std::env::var("ATLAS_NATIVE_COUNTER_DRAIN_OWNED").as_deref(),Ok("1"));
    assert!(std::env::var_os("ATLAS_HOME").is_some());
    let events=crate::postprocess::stats::Stats::default();
    events.rejected(crate::postprocess::rules::Reason::Fragment);
    events.rejected(crate::postprocess::rules::Reason::Fragment);
    let lanes=crate::pg::stage_telemetry::LaneCounters::new(0).unwrap();
    let stage=crate::postprocess::native::FinalCounters::stage_value(&events,&lanes,&Config::default().postprocess,0);
    let mut stats=Stats::new();
    let queue=crate::metrics::names::STAGE_QUEUE.rows();let walk=crate::metrics::names::STAGE_WALK.rows();
    let predb=crate::metrics::names::PREDB_TITLES.rows();
    stats.retain_final_services(crate::postprocess::native::FinalCounters {stage:Some(stage),predb:None},91);
    assert_eq!(crate::metrics::names::STAGE_QUEUE.rows(),queue);
    assert_eq!(crate::metrics::names::STAGE_WALK.rows(),walk);
    assert_eq!(crate::metrics::names::PREDB_TITLES.rows(),predb,"counter-only drain must not zero gauges");
    let before=crate::metrics::render();
    crate::metrics::report_service_counters(stats.final_services.as_ref().unwrap().stage.as_ref(),None,91);
    assert_eq!(crate::metrics::render(),before,"same pool total must be counted once");
    stats.write("","backfill",false,true);
    // index() performs this later write after supervisor/observer shutdown.
    stats.write("","",false,false);
    let actual:serde_json::Value=serde_json::from_slice(&fs::read(paths::stats_file()).unwrap()).unwrap();
    assert_eq!(actual["postprocess"]["rejected"]["fragment"],2);
    assert!(actual["postprocess"]["queue_due"].is_null());
    assert!(actual["postprocess"]["walk"]["total"].is_null());
    assert!(actual["predb"].is_null());assert_eq!(actual["running"],false);
    stats.final_services=None;
    stats.write("","backfill",true,false);
    let actual:serde_json::Value=serde_json::from_slice(&fs::read(paths::stats_file()).unwrap()).unwrap();
    assert!(actual["postprocess"].is_null(),"a new attempt cannot inherit the old final services");
}
