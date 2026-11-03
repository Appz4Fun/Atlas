//! What queueing a release adds to a save. For information, it asserts nothing:
//! `cargo test --release --locked --test pp_enqueue_cost -- --ignored --nocapture`

use std::sync::atomic::Ordering;
use std::time::Instant;

use atlas::parser::{Article, Release};
use atlas::{db, store};

fn time(skip: bool, round: usize) -> f64 {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    db::create_db_at(&main).unwrap();
    store::SKIP_ENQUEUE_FOR_BENCH.store(skip, Ordering::Relaxed);
    let t = Instant::now();
    for batch in 0..10 {
        let releases: Vec<Release> = (0..10_000)
            .map(|i| {
                let name = format!("Rel.{round}.{batch}.{i}");
                Release {
                    articles: vec![Article {
                        message_id: format!("<{round}.{batch}.{i}@x>"),
                        subject: format!("\"{name}\" yEnc (1/1)"),
                        filename: Some(name.clone()),
                        part: Some(1),
                        total_parts: Some(1),
                        bytes: 700_000,
                        ..Default::default()
                    }],
                    name,
                    group: "alt.binaries.t".into(),
                    ..Default::default()
                }
            })
            .collect();
        store::save(&main, &releases).unwrap();
    }
    t.elapsed().as_secs_f64()
}

#[test]
#[ignore]
fn enqueue_cost() {
    let mut ratios: Vec<f64> = (0..5).map(|round| time(false, round) / time(true, round)).collect();
    store::SKIP_ENQUEUE_FOR_BENCH.store(false, Ordering::Relaxed);
    println!("with the queue / without: {ratios:.3?}");
    ratios.sort_by(f64::total_cmp);
    println!("median {:.3}", ratios[2]);
}
