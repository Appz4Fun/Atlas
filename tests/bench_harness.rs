//! End to end throughput harness, not part of the normal test run: real
//! captured headers (`examples/capture_xover`) served by mock servers with
//! provider like latency, indexed by many backfill passes at once the way
//! the background indexer runs them. Prints one line of numbers per run.
//!
//!     ATLAS_CORPUS=/tmp/xover.tsv cargo test --release --test bench_harness -- --ignored --nocapture
//!
//! Knobs (env): BENCH_GROUPS (groups indexed at once, 24), BENCH_SERVERS (3),
//! BENCH_CONNS (connections per server, 20), BENCH_XOVER_MS (per XOVER, 200),
//! BENCH_BODY_MS (per BODY, 100), BENCH_CAP (unsaved headers, 500000),
//! BENCH_STALLS (XOVERs per server that hang past the timeout, 0),
//! BENCH_TIMEOUT_MS (request timeout, 30000), BENCH_LIMIT (headers of the
//! corpus to serve, all), BENCH_SECS (stop after, 120).

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use atlas::indexer::{PassContext, PassSettings, RunStates, run_pass, shared_db};
use atlas::nntp::Pool;
use common::{Post, Server, mock, par2_file_desc, spawn_server, yenc_body};

fn env<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// The captured rows, renumbered 1.. in order, with bodies for name lookups:
/// a base par2's first segment and an nfo name their release, every other
/// segment of them is filler like the middle of a real par2.
fn corpus(path: &str, limit: usize) -> Vec<Post> {
    let text = std::fs::read_to_string(path).expect("ATLAS_CORPUS");
    let filler: Vec<Vec<u8>> = (0..64).map(|_| vec![b'x'; 128]).collect();
    let mut posts = Vec::new();
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 8 {
            continue;
        }
        let number = posts.len() as u64 + 1;
        let subject = f[1].to_string();
        let first = atlas::parser::parse_subject(&subject).is_some_and(|p| p.part == 1);
        let body = if atlas::par2::is_base_par2(&subject) {
            if first {
                yenc_body("x.par2", &par2_file_desc(&format!("Named.{number}.mkv"), 1 << 30))
            } else {
                filler.clone()
            }
        } else if atlas::nfo::is_nfo(&subject) {
            if first { vec![format!("Release Name: Named.{number}").into_bytes()] } else { filler.clone() }
        } else {
            vec![]
        };
        posts.push(Post {
            number,
            subject,
            message_id: format!("<{number}@bench>"),
            bytes: f[6].parse().unwrap_or(0),
            body,
            date: f[3].to_string(),
        });
        if posts.len() >= limit {
            break;
        }
    }
    posts
}

#[test]
#[ignore]
fn bench() {
    let Ok(path) = std::env::var("ATLAS_CORPUS") else {
        eprintln!("set ATLAS_CORPUS");
        return;
    };
    let groups: usize = env("BENCH_GROUPS", 24);
    let nservers: usize = env("BENCH_SERVERS", 3);
    let conns: u32 = env("BENCH_CONNS", 20);
    let xover_ms: u64 = env("BENCH_XOVER_MS", 200);
    let body_ms: u64 = env("BENCH_BODY_MS", 100);
    let cap: usize = env("BENCH_CAP", 500_000);
    let stalls: usize = env("BENCH_STALLS", 0);
    let timeout_ms: u64 = env("BENCH_TIMEOUT_MS", 30_000);
    let limit: usize = env("BENCH_LIMIT", usize::MAX);
    let secs: u64 = env("BENCH_SECS", 120);

    let posts = corpus(&path, limit);
    let per_group = posts.len() as u64;
    let servers: Vec<Arc<Server>> = (0..nservers)
        .map(|_| {
            let mut s = Server::new(posts.clone());
            let m = Arc::get_mut(&mut s).unwrap();
            m.any_group = true;
            m.buffer_listing = true;
            m.xover_delay = Duration::from_millis(xover_ms);
            m.body_delay = Duration::from_millis(body_ms);
            m.stall = Duration::from_millis(timeout_ms * 2);
            m.stalls_left.store(stalls, Ordering::SeqCst);
            s
        })
        .collect();
    drop(posts);
    let cfgs: Vec<_> =
        servers.iter().enumerate().map(|(i, s)| mock(spawn_server(s.clone()), "secret", conns, i as i64 + 1)).collect();
    let pool = Arc::new(Pool::new(&cfgs).with_max_unsaved(cap).with_timeout(Duration::from_millis(timeout_ms)));

    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    atlas::db::create_db_at(&main).unwrap();
    let db = shared_db(atlas::db::open_at(&main).unwrap());
    let ctx = Arc::new(PassContext {
        pool: pool.clone(),
        states: RunStates::default(),
        stop: Arc::new(AtomicBool::new(false)),
        verbose: false,
    });
    let settings = Arc::new(PassSettings {
        mode: "backfill".into(),
        batch_size: 500_000,
        request_size: 10_000,
        split_min_backlog: i64::MAX,
        ..Default::default()
    });

    let saved = Arc::new(AtomicU64::new(0));
    let errors = Arc::new(AtomicU64::new(0));
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let t = Instant::now();
    let done_groups = rt.block_on(async {
        pool.connect().await.unwrap();
        let mut passes = tokio::task::JoinSet::new();
        for g in 0..groups {
            let (ctx, settings, db, saved, errors) =
                (ctx.clone(), settings.clone(), db.clone(), saved.clone(), errors.clone());
            passes.spawn(async move {
                let group = format!("alt.binaries.bench{g}");
                while !ctx.stop.load(Ordering::Relaxed) {
                    let mut progress = |p: &atlas::indexer::Progress| {
                        saved.fetch_add(p.articles as u64, Ordering::Relaxed);
                    };
                    // dropped on stop, like the background indexer does
                    let pass = run_pass(&ctx, &settings, &db, &group, g % nservers, &mut progress);
                    let Some(result) = atlas::nntp::unless_stopped(&ctx.stop, pass).await else { break };
                    match result {
                        Ok(_) if ctx.states.is_idle(&group) => return true,
                        Ok(_) => {}
                        Err(_) => {
                            errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                false
            });
        }
        let stop = ctx.stop.clone();
        let deadline = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(secs)).await;
            stop.store(true, Ordering::Relaxed);
        });
        let mut done = 0;
        while let Some(r) = passes.join_next().await {
            done += r.unwrap() as usize;
        }
        deadline.abort();
        done
    });
    let elapsed = t.elapsed().as_secs_f64();
    let saved = saved.load(Ordering::Relaxed);
    let bodies: usize = servers.iter().map(|s| s.bodies_sent.load(Ordering::SeqCst)).sum();
    let xovers: usize = servers.iter().map(|s| s.xovers.load(Ordering::SeqCst)).sum();
    println!(
        "RESULT groups_done={done_groups}/{groups} headers={saved} of {} secs={elapsed:.1} headers_per_s={:.0} \
         xovers={xovers} bodies={bodies} errors={} unsaved_peak={}",
        per_group * groups as u64,
        saved as f64 / elapsed,
        errors.load(Ordering::Relaxed),
        pool.unsaved_peak()
    );
    // with ATLAS_PROFILE=1: where the passes spent their time
    if atlas::profile::enabled() {
        println!("RESULT {}", atlas::profile::report(elapsed));
        println!("RESULT {}", atlas::profile::LOAD.snapshot());
    }
}
