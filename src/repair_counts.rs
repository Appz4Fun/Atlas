//! `atlas --count-repair-candidates [--sample N] [--apply]`: the releases a PAR2 repair may make whole get their files'
//! byte counts (`blob_bytes`), soo `release_info::repair_needed` can answer for them and they show as NEEDS REPAIR.
//! The indexer's own pass counts every blob in the shards, at most `blob_bytes::FETCHES` a second from the sealed log,
//! which takes months; this counts only the blobs of incomplete releases with recovery volumes whose answer waits on
//! them. It writes only counts the shards' trigger keeps right, soo it runs beside the indexer, and a run stopped
//! halfway goes on where it was the next time.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rusqlite::Connection;

use crate::{db, release_info, store};

/// Incomplete releases with a recovery volume among their files.
fn candidates(conn: &Connection, s: usize) -> rusqlite::Result<Vec<i64>> {
    conn.prepare(&format!(
        "select distinct f.release_id from s{s}.files f join s{s}.releases r on r.id = f.release_id
         where r.complete = 0 and lower(f.filename) like '%.vol%.par2' order by f.release_id"
    ))?
    .query_map([], |r| r.get(0))?
    .collect()
}

#[derive(Default)]
struct Tally {
    candidates: AtomicUsize,
    waiting: AtomicUsize,
    counted: AtomicUsize,
    failed: AtomicUsize,
    repair: AtomicUsize,
}

fn shard(
    main: &Path,
    s: usize,
    apply: bool,
    tally: &Tally,
    shown: &std::sync::Mutex<Vec<String>>,
    sample: usize,
) -> anyhow::Result<()> {
    let reader = db::open_with_shards(main)?;
    let writer = if apply { Some(db::open_at(&store::shard_path(main, s))?) } else { None };
    let stop = AtomicBool::new(false);
    let ids = candidates(&reader, s)?;
    tally.candidates.fetch_add(ids.len(), Ordering::Relaxed);
    for (i, id) in ids.iter().enumerate() {
        if release_info::repair_waits_on_counts(&reader, *id)? {
            tally.waiting.fetch_add(1, Ordering::Relaxed);
            if let Some(writer) = &writer {
                let (counted, failed) = crate::blob_bytes::count_release(writer, *id, &stop)?;
                tally.counted.fetch_add(counted, Ordering::Relaxed);
                tally.failed.fetch_add(failed, Ordering::Relaxed);
            } else {
                let mut shown = shown.lock().unwrap();
                if shown.len() < sample {
                    shown.push(format!("{id}"));
                }
                continue;
            }
        }
        if release_info::repair_needed(&reader, *id).unwrap_or(false) {
            tally.repair.fetch_add(1, Ordering::Relaxed);
        }
        if (i + 1) % 2_000 == 0 {
            println!("shard {s}: {} of {} looked at", i + 1, ids.len());
        }
    }
    Ok(())
}

pub fn run(args: &[String]) -> i32 {
    let apply = args.iter().any(|a| a == "--apply");
    let sample: usize = args
        .iter()
        .position(|a| a == "--sample")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let main = crate::paths::database();
    // writing counts: held off a compaction, and refused on shards a merge left half done
    let _held = if apply {
        match crate::compact::hold_off_compaction(&main) {
            Ok(held) => Some(held),
            Err(e) => {
                println!("{e:#}");
                return 1;
            }
        }
    } else {
        None
    };
    let tally = Tally::default();
    let shown = std::sync::Mutex::new(Vec::new());
    let failed: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..store::SHARDS)
            .map(|s| {
                let (main, tally, shown) = (&main, &tally, &shown);
                scope
                    .spawn(move || shard(main, s, apply, tally, shown, sample).map_err(|e| format!("shard {s}: {e:#}")))
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().unwrap_or_else(|_| Err("a shard panicked".into())).err()).collect()
    });
    let get = |a: &AtomicUsize| a.load(Ordering::Relaxed);
    println!(
        "{} incomplete releases with recovery volumes, {} waiting on byte counts",
        get(&tally.candidates),
        get(&tally.waiting)
    );
    if apply {
        println!(
            "counted {} blobs ({} couldn't be read); {} releases need a repair now",
            get(&tally.counted),
            get(&tally.failed),
            get(&tally.repair)
        );
    } else {
        println!("{} need a repair already; waiting, for example:", get(&tally.repair));
        for id in shown.lock().unwrap().iter() {
            println!("  {id}");
        }
        println!("a dry run: nothing was counted. --apply counts them (the indexer can keep running)");
    }
    for e in &failed {
        println!("{e}");
    }
    i32::from(!failed.is_empty())
}
