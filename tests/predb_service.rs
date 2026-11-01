//! The service's threads, on a `Fetch` that opens no socket.

mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use atlas::config::{Config, PredbConfig};
use atlas::predb::poll::REAL_REQUESTS;
use atlas::predb::{Service, ServiceParams};
use common::Scripted;
use common::stage::wait_for;

fn start(sources: &[&str]) -> (tempfile::TempDir, std::sync::Arc<Service>, std::sync::Arc<Scripted>) {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    atlas::db::create_db_at(&main).unwrap();
    let fetch = Scripted::new();
    *fetch.always.lock().unwrap() = Some(serde_json::json!({"data": {"rows": []}, "list": []}).to_string());
    let config =
        PredbConfig { enabled: true, sources: sources.iter().map(|s| s.to_string()).collect(), ..Default::default() };
    let fetcher: std::sync::Arc<dyn atlas::predb::poll::Fetch> = fetch.clone();
    let service = Service::start(ServiceParams { main, config, stage: None, fetch: Some(fetcher) }).unwrap();
    (dir, service, fetch)
}

#[test]
fn stop_ends_every_thread() {
    let (_dir, service, _fetch) = start(&["predb_club"]);
    let start = Instant::now();
    service.stop_and_join();
    assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
    service.stop_and_join();
    assert_eq!(REAL_REQUESTS.load(Ordering::Relaxed), 0);
}

#[test]
fn a_source_it_doesnt_know_is_ignored() {
    let (_dir, service, fetch) = start(&["nope"]);
    std::thread::sleep(Duration::from_millis(1_500));
    assert!(fetch.urls.lock().unwrap().is_empty());
    service.stop_and_join();
}

#[test]
fn a_reload_takes_effect() {
    let (_dir, service, fetch) = start(&[]);
    std::thread::sleep(Duration::from_millis(1_200));
    assert!(fetch.urls.lock().unwrap().is_empty());
    let predb = PredbConfig { enabled: true, sources: vec!["predb_club".into()], ..Default::default() };
    service.set_config(&Config { predb, ..Config::default() });
    wait_for("a poll", || !fetch.urls.lock().unwrap().is_empty());
    let snapshot = service.snapshot();
    assert_eq!(snapshot["titles"], 0);
    service.stop_and_join();
}

#[test]
fn turning_the_late_path_on_needs_no_restart() {
    use std::sync::Arc;

    use atlas::indexer::shared_db;
    use atlas::nntp::Pool;
    use atlas::postprocess::{Stage, StageParams};
    use atlas::predb::db::{create, path_for};
    use atlas::predb::import::{Entry, import};

    let h = common::stage::fresh();
    let mut titles = create(&path_for(&h.main)).unwrap();
    import(&mut titles, 1, &[Entry { title: "Alpha.Beta.Title-GRP".into(), ..Default::default() }], 5, None).unwrap();
    let stage = Stage::start(StageParams {
        main: h.main.clone(),
        db: shared_db(atlas::db::open_at(&h.main).unwrap()),
        pool: Arc::new(Pool::new(&[])),
        config: Config::default(),
        steps: Vec::new(),
        handle: h.runtime.handle().clone(),
    });
    let off = PredbConfig { enabled: true, late_per_second: 0, ..Default::default() };
    let service = Service::start(ServiceParams {
        main: h.main.clone(),
        config: off.clone(),
        stage: Some(stage.clone()),
        fetch: None,
    })
    .unwrap();
    let cursor =
        || -> i64 { titles.query_row("select value from meta where key = 'late_after_id'", [], |r| r.get(0)).unwrap() };
    std::thread::sleep(Duration::from_millis(1_500));
    assert_eq!(cursor(), 0, "off");
    service.set_config(&Config { predb: PredbConfig { late_per_second: 5, ..off }, ..Config::default() });
    wait_for("the title to be read", || cursor() == 1);
    service.stop_and_join();
    stage.stop_and_join();
}
