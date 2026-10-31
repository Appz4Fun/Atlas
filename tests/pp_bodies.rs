//! The net lane: bodies within a budget, and an outage costs no attempts.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use atlas::config::PostprocessConfig;
use atlas::nntp::Pool;
use atlas::parser::Release;
use atlas::postprocess::step::{BodyRequest, Got, NameChange, Needs, Outcome, StepCtx, StepError, StepId, mask};
use atlas::store::SegmentPick;
use common::stage::*;
use common::{Post, Server, mock, spawn_server, yenc_body};

const BIT: i64 = mask(StepId(1));

fn log() -> Log {
    Default::default()
}

fn budget(per_minute: i64) -> PostprocessConfig {
    PostprocessConfig { body_min_size_bytes: 0, body_fetches_per_minute: per_minute, ..quick_config() }
}

/// the mock's post for every article of `release`
fn posts_of(release: &Release, first: u64, body: &[u8]) -> Vec<Post> {
    release
        .articles
        .iter()
        .enumerate()
        .map(|(i, a)| Post {
            number: first + i as u64,
            subject: a.subject.clone(),
            message_id: a.message_id.clone(),
            bytes: 700_000,
            body: yenc_body("x", body),
            date: "Fri, 02 Oct 2026 10:11:12 +0000".into(),
            author: "poster <p@x>".into(),
        })
        .collect()
}

fn request_first(ctx: &mut StepCtx<'_>) -> Result<Got, StepError> {
    let file = ctx.files().map_err(|e| StepError::Retry(e.to_string()))?.remove(0);
    let seg = ctx.segments(file.id, &SegmentPick::First(1)).map_err(|e| StepError::Retry(e.to_string()))?.remove(0);
    let wanted = [BodyRequest { message_id: seg.message_id, file: file.filename, max_bytes: None }];
    Ok(ctx.fetch_bodies(&wanted)?.remove(0).got)
}

/// a stage against a mock holding `releases`, its pool shared
fn with_server(
    h: &mut Harness,
    releases: &[Release],
    steps: Vec<Arc<dyn atlas::postprocess::step::Step>>,
    config: PostprocessConfig,
) -> Arc<Server> {
    h.save(releases);
    let mut posts = Vec::new();
    for r in releases {
        let first = posts.len() as u64 + 1;
        posts.extend(posts_of(r, first, b"payload"));
    }
    let state = Server::new(posts);
    let port = spawn_server(state.clone());
    h.start_with_pool(steps, config, Arc::new(Pool::new(&[mock(port, "secret", 4, 1)])));
    state
}

#[test]
fn no_budget_no_bodies() {
    let log = log();
    let mut h = fresh();
    let step =
        TestStep::new(1, "body", 100, Needs::Bodies, &log).runs(|ctx| request_first(ctx).map(|_| Outcome::default()));
    let db_step = TestStep::new(9, "db", 200, Needs::Database, &log);
    let state = with_server(
        &mut h,
        &[complete("Rel.One")],
        vec![step.arc(), db_step.arc()],
        PostprocessConfig { body_min_size_bytes: 0, ..quick_config() },
    );
    wait_for("the database step", || runs_of(&log, "db") == 1);
    wait_for("its row", || h.bits("Rel.One").is_some());
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(state.bodies_sent.load(Ordering::SeqCst), 0);
    assert_eq!(runs_of(&log, "body"), 0);
    assert_eq!(h.bits("Rel.One").unwrap().2, None, "net_wanted");
}

#[test]
fn the_budget_caps_fetches_per_minute() {
    let log = log();
    let mut h = fresh();
    let releases: Vec<Release> = (0..10).map(|i| complete(&format!("Rel.{i}"))).collect();
    let step = TestStep::new(1, "body", 100, Needs::Bodies, &log).runs(|ctx| {
        let file = ctx.files().map_err(|e| StepError::Retry(e.to_string()))?.remove(0);
        for seg in ctx.segments(file.id, &SegmentPick::First(2)).map_err(|e| StepError::Retry(e.to_string()))? {
            ctx.fetch_bodies(&[BodyRequest {
                message_id: seg.message_id,
                file: file.filename.clone(),
                max_bytes: None,
            }])?;
        }
        Ok(Outcome::default())
    });
    let state = with_server(&mut h, &releases, vec![step.arc()], budget(60));
    wait_for("the first body", || state.bodies_sent.load(Ordering::SeqCst) >= 1);
    let start = Instant::now();
    let before = state.bodies_sent.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_secs(3));
    let sent = state.bodies_sent.load(Ordering::SeqCst) - before;
    assert!(sent as f64 <= start.elapsed().as_secs_f64() + 2.0, "{sent} bodies in {:?}", start.elapsed());
    assert!(sent >= 1);
}

#[test]
fn outside_the_bounds_a_body_step_is_done_and_unmatched() {
    for config in [
        PostprocessConfig { body_min_size_bytes: 10 << 20, body_fetches_per_minute: 6000, ..quick_config() },
        PostprocessConfig {
            body_max_size_bytes: 1024,
            body_min_size_bytes: 0,
            body_fetches_per_minute: 6000,
            ..quick_config()
        },
    ] {
        let log = log();
        let mut h = fresh();
        let step = TestStep::new(1, "body", 100, Needs::Bodies, &log);
        with_server(&mut h, &[complete("Rel.One")], vec![step.arc()], config);
        wait_for("its bit", || h.bits("Rel.One").is_some_and(|b| b.0 == BIT));
        assert_eq!(h.bits("Rel.One").unwrap().2, None);
        assert_eq!(runs_of(&log, "body"), 0);
    }
}

#[test]
fn an_image_only_release_stays_out_of_the_net_lane() {
    let log = log();
    let mut h = fresh();
    let step = TestStep::new(1, "body", 100, Needs::Bodies, &log);
    let db_step = TestStep::new(9, "db", 200, Needs::Database, &log);
    with_server(&mut h, &[release_of("Pic.One", "pic.jpg", 2, 2)], vec![step.arc(), db_step.arc()], budget(6000));
    wait_for("both bits", || h.bits("Pic.One").is_some_and(|b| b.0 == BIT | mask(StepId(9))));
    assert_eq!(h.bits("Pic.One").unwrap().2, None);
    assert_eq!(runs_of(&log, "body"), 0);
}

#[test]
fn an_incomplete_release_gets_no_body_until_a_late_article_opens_it() {
    let log = log();
    let mut h = fresh();
    let step = TestStep::new(1, "body", 100, Needs::Bodies, &log).reruns(false, true);
    let db_step = TestStep::new(9, "db", 200, Needs::Database, &log);
    with_server(&mut h, &[release_of("Rel.One", "rel.bin", 1, 3)], vec![step.arc(), db_step.arc()], budget(6000));
    wait_for("the database step", || runs_of(&log, "db") == 1);
    wait_for("its row", || h.bits("Rel.One").is_some());
    let (done, _, wanted) = h.bits("Rel.One").unwrap();
    assert_eq!((done & BIT, wanted), (0, None));
    h.save(&[release_of("Rel.One", "rel.bin", 3, 3)]);
    wait_for("the body step", || runs_of(&log, "body") == 1);
}

#[test]
fn the_gate_never_marks_a_step_whose_applies_is_false() {
    let log = log();
    let mut h = fresh();
    let step = TestStep::new(1, "body", 100, Needs::Bodies, &log).applies(|_| false);
    let db_step = TestStep::new(9, "db", 200, Needs::Database, &log);
    let config = PostprocessConfig { body_min_size_bytes: 10 << 20, body_fetches_per_minute: 6000, ..quick_config() };
    with_server(&mut h, &[complete("Rel.One")], vec![step.arc(), db_step.arc()], config);
    wait_for("its row", || h.bits("Rel.One").is_some());
    assert_eq!(h.bits("Rel.One").unwrap().0 & BIT, 0);
}

#[test]
fn fetch_bodies_refuses_before_any_bytes_move() {
    let log = log();
    let mut h = fresh();
    let results: Arc<Mutex<Vec<Got>>> = Default::default();
    let r = results.clone();
    // this step's release is in the bounds: it asks for an image, then past bodies_per_release
    let step = TestStep::new(1, "body", 100, Needs::Bodies, &log).runs(move |ctx| {
        let ask = |file: &str| BodyRequest { message_id: "<no@x>".into(), file: file.into(), max_bytes: None };
        let got = ctx.fetch_bodies(&[ask("a.jpg"), ask("b.PNG")])?;
        r.lock().unwrap().extend(got.into_iter().map(|b| b.got));
        Ok(Outcome::default())
    });
    let config = budget(6000);
    let state = with_server(&mut h, &[complete("Rel.One")], vec![step.arc()], config);
    wait_for("its answers", || results.lock().unwrap().len() == 2);
    assert!(results.lock().unwrap().iter().all(|g| matches!(g, Got::Refused(_))));
    assert_eq!(state.bodies_sent.load(Ordering::SeqCst), 0);
}

#[test]
fn past_bodies_per_release_is_refused() {
    let log = log();
    let mut h = fresh();
    let results: Arc<Mutex<Vec<Got>>> = Default::default();
    let r = results.clone();
    let step = TestStep::new(1, "body", 100, Needs::Bodies, &log).runs(move |ctx| {
        let got = request_first(ctx)?;
        let more =
            ctx.fetch_bodies(&[BodyRequest { message_id: "<no@x>".into(), file: "x.bin".into(), max_bytes: None }])?;
        r.lock().unwrap().push(got);
        r.lock().unwrap().extend(more.into_iter().map(|b| b.got));
        Ok(Outcome::default())
    });
    let config = PostprocessConfig { bodies_per_release: 1, ..budget(6000) };
    let state = with_server(&mut h, &[complete("Rel.One")], vec![step.arc()], config);
    wait_for("its answers", || results.lock().unwrap().len() == 2);
    let got = results.lock().unwrap();
    assert_eq!(got[0], Got::Body(b"payload".to_vec()));
    assert!(matches!(got[1], Got::Refused(_)));
    assert_eq!(state.bodies_sent.load(Ordering::SeqCst), 1);
}

#[test]
fn a_server_outage_uses_no_attempts() {
    let log = log();
    let mut h = fresh();
    h.save(&[complete("Rel.One")]);
    let dead = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let step =
        TestStep::new(1, "body", 100, Needs::Bodies, &log).runs(|ctx| request_first(ctx).map(|_| Outcome::default()));
    h.start_with_pool(vec![step.arc()], budget(6000), Arc::new(Pool::new(&[mock(dead, "secret", 2, 1)])));
    wait_for("it ran", || runs_of(&log, "body") >= 1);
    wait_for("it moved on", || {
        h.field("Rel.One", "net_next_at") > chrono::Utc::now().timestamp() && h.field("Rel.One", "net_attempts") == 0
    });
    let next = h.field("Rel.One", "net_next_at") - chrono::Utc::now().timestamp();
    assert!((280..=310).contains(&next), "{next}");
    assert_eq!(runs_of(&log, "body"), 1, "the lane paused");
}

#[test]
fn wait_until_gives_back_the_attempt_begin_counted() {
    let log = log();
    let mut h = fresh();
    let at = chrono::Utc::now().timestamp() + 500;
    let step = TestStep::new(1, "body", 100, Needs::Bodies, &log)
        .runs(move |_| Ok(Outcome { wait_until: Some(at), ..Default::default() }));
    with_server(&mut h, &[complete("Rel.One")], vec![step.arc()], budget(6000));
    wait_for("it waits", || h.bits("Rel.One").is_some() && h.field("Rel.One", "net_next_at") == at);
    assert_eq!(h.field("Rel.One", "net_attempts"), 0);
    assert_eq!(h.bits("Rel.One").unwrap().0, 0);
}

#[test]
fn a_missing_article_is_for_the_step_to_judge() {
    let log = log();
    let mut h = fresh();
    let matched = Arc::new(AtomicUsize::new(0));
    let m = matched.clone();
    let step = TestStep::new(1, "body", 100, Needs::Bodies, &log).runs(move |ctx| {
        let got =
            ctx.fetch_bodies(&[BodyRequest { message_id: "<gone@x>".into(), file: "x.bin".into(), max_bytes: None }])?;
        assert_eq!(got[0].got, Got::Missing);
        m.fetch_add(1, Ordering::SeqCst);
        Ok(Outcome { matched: false, ..Default::default() })
    });
    with_server(&mut h, &[complete("Rel.One")], vec![step.arc()], budget(6000));
    wait_for("done", || h.bits("Rel.One").is_some_and(|b| b.0 == BIT));
    assert_eq!(matched.load(Ordering::SeqCst), 1);
}

#[test]
fn a_step_that_keeps_killing_the_process_is_given_up_on() {
    let log = log();
    let mut h = fresh();
    h.save(&[complete("Rel.One")]);
    let conn = h.shard();
    let id = h.id_of("Rel.One");
    // what `max_attempts` unfinished runs leave
    conn.execute("insert into pp_state (release_id, net_wanted, net_attempts, net_next_at) values (?, 1, 3, 0)", [id])
        .unwrap();
    conn.execute("delete from pp_queue", []).unwrap();
    let first = TestStep::new(1, "poison", 100, Needs::External, &log).runs(|_| panic!("never run"));
    let second = TestStep::new(2, "next", 110, Needs::External, &log);
    h.start(vec![first.arc(), second.arc()], PostprocessConfig { max_attempts: 3, ..quick_config() });
    wait_for("the failed bit", || h.bits("Rel.One").is_some_and(|b| b.1 == BIT));
    assert_eq!(h.field("Rel.One", "net_attempts"), 0);
    assert_eq!(runs_of(&log, "poison"), 0);
    wait_for("the next step", || runs_of(&log, "next") == 1);
}

#[test]
fn a_crash_mid_step_counts_one_attempt_and_retries_later() {
    let log = log();
    let mut h = fresh();
    h.save(&[complete("Rel.One")]);
    let conn = h.shard();
    let id = h.id_of("Rel.One");
    conn.execute("insert into pp_state (release_id, net_wanted) values (?, 1)", [id]).unwrap();
    conn.execute("delete from pp_queue", []).unwrap();
    // Begin, as a crash would leave it
    conn.execute("update pp_state set net_attempts = net_attempts + 1, net_next_at = 0", []).unwrap();
    let step = TestStep::new(1, "body", 100, Needs::External, &log);
    h.start(vec![step.arc()], quick_config());
    wait_for("it finishes", || h.bits("Rel.One").is_some_and(|b| b.0 == BIT));
    assert_eq!(h.field("Rel.One", "net_attempts"), 0);
}

#[test]
fn begin_counts_before_the_step_runs() {
    let log = log();
    let mut h = fresh();
    h.save(&[complete("Rel.One")]);
    let conn = h.shard();
    let id = h.id_of("Rel.One");
    conn.execute("insert into pp_state (release_id, net_wanted) values (?, 1)", [id]).unwrap();
    conn.execute("delete from pp_queue", []).unwrap();
    let seen = Arc::new(Mutex::new(None));
    let s = seen.clone();
    let step = TestStep::new(1, "body", 100, Needs::External, &log).runs(move |ctx| {
        let attempts: i64 = ctx.conn().query_row("select net_attempts from pp_state", [], |r| r.get(0)).unwrap();
        *s.lock().unwrap() = Some(attempts);
        Ok(Outcome::default())
    });
    h.start(vec![step.arc()], quick_config());
    wait_for("it ran", || seen.lock().unwrap().is_some());
    assert_eq!(*seen.lock().unwrap(), Some(1), "written ahead");
}

#[test]
fn a_db_visit_during_a_net_step_keeps_its_count() {
    let log = log();
    let mut h = fresh();
    h.save(&[complete("Rel.One")]);
    let conn = h.shard();
    let id = h.id_of("Rel.One");
    conn.execute(
        "insert into pp_state (release_id, net_attempts, net_next_at, parts_seen) values (?, 2, 99999999999, 2)",
        [id],
    )
    .unwrap();
    let net = TestStep::new(1, "net", 100, Needs::External, &log);
    let db_step = TestStep::new(9, "db", 200, Needs::Database, &log);
    h.start(vec![net.arc(), db_step.arc()], quick_config());
    wait_for("the database step", || runs_of(&log, "db") == 1);
    wait_for("its bit", || h.bits("Rel.One").is_some_and(|b| b.0 == mask(StepId(9))));
    assert_eq!(h.field("Rel.One", "net_attempts"), 2);
}

#[test]
fn a_net_step_that_renames_requeues_later_database_steps() {
    let log = log();
    let mut h = fresh();
    let net = TestStep::new(1, "net", 100, Needs::External, &log).runs(|ctx| {
        // once, on the way in
        let name = ctx.release().display_name.is_none().then(|| NameChange {
            display_name: Some("Better".into()),
            obfuscated: None,
            source: 40,
        });
        Ok(Outcome { name, ..Default::default() })
    });
    let db_step = TestStep::new(9, "db", 200, Needs::Database, &log).reruns(true, false);
    with_server(&mut h, &[complete("Rel.One")], vec![net.arc(), db_step.arc()], budget(6000));
    wait_for("the database step sees the name", || {
        log.lock().unwrap().iter().any(|(s, _, d)| *s == "db" && d.as_deref() == Some("Better"))
    });
}

#[test]
fn newest_goes_first_and_every_eighth_claim_takes_the_oldest() {
    let log = log();
    let mut h = fresh();
    let releases: Vec<Release> = (0..30).map(|i| complete(&format!("Rel.{i:02}"))).collect();
    h.save(&releases);
    let conn = h.shard();
    conn.execute_batch(
        "insert into pp_state (release_id, net_wanted) select id, 1 from releases; delete from pp_queue;",
    )
    .unwrap();
    // settled, soo every claim runs
    std::thread::sleep(Duration::from_millis(1200));
    let step = TestStep::new(1, "net", 100, Needs::External, &log);
    h.start(vec![step.arc()], PostprocessConfig { body_connections: 1, backlog_per_second: 0, ..quick_config() });
    wait_for("all ran", || runs_of(&log, "net") == 30);
    let order: Vec<String> = log.lock().unwrap().iter().map(|(_, n, _)| n.clone()).collect();
    assert_eq!(order[0], "Rel.29", "newest first");
    assert!(order.iter().take(29).any(|n| n == "Rel.00"), "the oldest doesnt wait for all the rest: {order:?}");
}

#[test]
fn a_slow_net_step_doesnt_hold_up_the_db_lane() {
    let log = log();
    let mut h = fresh();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let released = Mutex::new(released);
    let net = TestStep::new(1, "net", 100, Needs::External, &log).runs(move |_| {
        let _ = released.lock().unwrap().recv();
        Ok(Outcome::default())
    });
    let db_step = TestStep::new(9, "db", 200, Needs::Database, &log);
    with_server(&mut h, &[complete("Rel.One"), complete("Rel.Two")], vec![net.arc(), db_step.arc()], budget(6000));
    wait_for("the net step started", || runs_of(&log, "net") >= 1);
    wait_for("database steps finish", || runs_of(&log, "db") == 2);
    drop(release);
}
