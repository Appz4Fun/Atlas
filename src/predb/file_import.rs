//! `atlas --predb-import FILE [--apply]`: a list of titles the user already has. A dry run unless `--apply`.
//!
//! Reading is bounded before anything is decoded: a small `.gz` can expand to anything, and a line
//! with no newline would otherwise grow one buffer without limit.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use rusqlite::Connection;

use crate::{compact, paths, procs};

use super::db;
use super::import::{Counts, Entry, import};
use super::keys::valid_title;

const SOURCE_FILE: i64 = 100;
const MAX_IMPORT_LINE: u64 = 1024;
const MAX_IMPORT_LINES: u64 = 50_000_000;
const MAX_IMPORT_BYTES: u64 = 16 << 30;
const BATCH: usize = 10_000;

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Stop {
    End,
    TooManyBytes,
    TooManyLines,
}

#[derive(Default, Debug)]
pub struct ImportStats {
    pub valid: u64,
    pub invalid: u64,
    pub too_long: u64,
    pub counts: Counts,
    pub batches: u64,
}

pub struct ImportLimits {
    pub max_bytes: u64,
    pub max_lines: u64,
    pub batch: usize,
}

fn env_or(key: &str, default: u64) -> u64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn limits() -> ImportLimits {
    ImportLimits {
        max_bytes: env_or("ATLAS_PREDB_IMPORT_MAX_BYTES", MAX_IMPORT_BYTES),
        max_lines: env_or("ATLAS_PREDB_IMPORT_MAX_LINES", MAX_IMPORT_LINES),
        batch: env_or("ATLAS_PREDB_IMPORT_BATCH", BATCH as u64).max(1) as usize,
    }
}

/// a line as an entry, or None when it's no title
fn entry_of(line: &[u8]) -> Option<Entry> {
    let text = std::str::from_utf8(line).ok()?;
    let mut fields = text.trim_end_matches(['\r', '\n']).split('\t');
    let title = fields.next()?.trim();
    if !valid_title(title) {
        return None;
    }
    Some(Entry {
        title: title.to_string(),
        category: fields.next().map(str::trim).filter(|c| !c.is_empty()).map(String::from),
        pre_at: fields.next().and_then(|p| p.trim().parse().ok()),
        ..Default::default()
    })
}

fn flush(
    conn: &mut Connection,
    batch: &mut Vec<Entry>,
    now: i64,
    stats: &mut ImportStats,
    after: &mut dyn FnMut(&Connection),
) -> rusqlite::Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let c = import(conn, SOURCE_FILE, batch, now, None)?;
    stats.counts.inserted += c.inserted;
    stats.counts.updated += c.updated;
    stats.counts.skipped += c.skipped;
    stats.batches += 1;
    batch.clear();
    after(conn);
    Ok(())
}

/// Each line, into `conn` by batches when there is one. `after` runs after each committed batch.
#[doc(hidden)]
pub fn import_lines(
    reader: impl BufRead,
    mut conn: Option<&mut Connection>,
    limits: &ImportLimits,
    now: i64,
    after: &mut dyn FnMut(&Connection),
) -> anyhow::Result<(Stop, ImportStats)> {
    let mut input = reader.take(limits.max_bytes);
    let mut buf = Vec::with_capacity(MAX_IMPORT_LINE as usize + 1);
    let mut stats = ImportStats::default();
    let mut lines = 0u64;
    let mut batch: Vec<Entry> = Vec::new();
    let stop = loop {
        buf.clear();
        if (&mut input).take(MAX_IMPORT_LINE + 1).read_until(b'\n', &mut buf)? == 0 {
            break if input.limit() == 0 { Stop::TooManyBytes } else { Stop::End };
        }
        if lines == limits.max_lines {
            break Stop::TooManyLines;
        }
        lines += 1;
        if buf.last() != Some(&b'\n') && buf.len() as u64 > MAX_IMPORT_LINE {
            stats.too_long += 1;
            // the rest of this line, a piece at a time
            loop {
                buf.clear();
                let n = (&mut input).take(MAX_IMPORT_LINE + 1).read_until(b'\n', &mut buf)?;
                if n == 0 || buf.last() == Some(&b'\n') {
                    break;
                }
            }
            continue;
        }
        let Some(entry) = entry_of(&buf) else {
            stats.invalid += 1;
            continue;
        };
        stats.valid += 1;
        if let Some(conn) = &mut conn {
            batch.push(entry);
            if batch.len() >= limits.batch {
                flush(conn, &mut batch, now, &mut stats, after)?;
            }
        }
    };
    if let Some(conn) = &mut conn {
        flush(conn, &mut batch, now, &mut stats, after)?;
    }
    Ok((stop, stats))
}

fn open(path: &Path) -> std::io::Result<Box<dyn BufRead>> {
    let file = File::open(path)?;
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("gz")) {
        Ok(Box::new(BufReader::new(flate2::read::GzDecoder::new(file))))
    } else {
        Ok(Box::new(BufReader::new(file)))
    }
}

fn summary(stats: &ImportStats) -> String {
    format!("{} valid, {} invalid, {} too long", stats.valid, stats.invalid, stats.too_long)
}

fn stop_message(stop: Stop, limits: &ImportLimits) -> Option<String> {
    match stop {
        Stop::End => None,
        Stop::TooManyBytes => Some(format!("stopped at the cap of {} decompressed bytes", limits.max_bytes)),
        Stop::TooManyLines => Some(format!("stopped at the cap of {} lines", limits.max_lines)),
    }
}

fn dry_run(file: &Path, limits: &ImportLimits) -> anyhow::Result<i32> {
    let (stop, stats) = import_lines(open(file)?, None, limits, 0, &mut |_| {})?;
    println!("{}", summary(&stats));
    println!("a dry run: nothing was changed. --apply imports them");
    Ok(stop_message(stop, limits).map_or(0, |m| {
        println!("{m}");
        1
    }))
}

fn apply(main: &Path, file: &Path, limits: &ImportLimits) -> anyhow::Result<i32> {
    let reader = open(file)?;
    let _writing = compact::hold_off_compaction(main)?;
    // no tune_for_writing: it turns off auto-checkpointing, and no checkpointer serves this file
    let mut conn = db::create(&db::path_for(main))?;
    let top = |c: &Connection| -> rusqlite::Result<i64> {
        c.query_row("select coalesce(max(id), 0) from titles", [], |r| r.get(0))
    };
    let late = |c: &Connection| -> rusqlite::Result<i64> {
        c.query_row("select value from meta where key = 'late_after_id'", [], |r| r.get(0))
    };
    let caught_up = top(&conn)? == late(&conn)?;
    let now = chrono::Utc::now().timestamp();
    let (stop, stats) = import_lines(reader, Some(&mut conn), limits, now, &mut |_| {})?;
    println!("{}", summary(&stats));
    println!("{} inserted, {} updated, {} batches", stats.counts.inserted, stats.counts.updated, stats.batches);
    if caught_up && stats.counts.inserted > 0 {
        // at 1 a second the late queue would take months over what a walk does in a day
        conn.execute("update meta set value = ?1 where key = 'late_after_id'", [top(&conn)?])?;
        println!("to match releases to these, run: atlas --reprocess --step predb_match --apply");
    }
    Ok(stop_message(stop, limits).map_or(0, |m| {
        println!("{m}");
        1
    }))
}

/// `atlas --predb-import FILE [--apply]`. A local text file, gzip by its `.gz` suffix: one `title`, or
/// `title<TAB>category<TAB>pre_at`, per line. Exit 0 on success, 1 when refused or failed.
pub fn run(args: &[String]) -> i32 {
    let (mut file, mut do_apply) = (None, false);
    for arg in args {
        match arg.as_str() {
            "--predb-import" => {}
            "--apply" => do_apply = true,
            other if file.is_none() && !other.starts_with("--") => file = Some(other.to_string()),
            other => {
                println!("unknown option {other}");
                return 1;
            }
        }
    }
    let Some(file) = file else {
        println!("--predb-import needs a file");
        return 1;
    };
    let file = Path::new(&file);
    if !file.is_file() {
        println!("no file at {}", file.display());
        return 1;
    }
    let limits = limits();
    let result = if do_apply {
        if procs::indexer_alive() {
            println!("stop indexing first");
            return 1;
        }
        apply(&paths::database(), file, &limits)
    } else {
        dry_run(file, &limits)
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            println!("{e:#}");
            1
        }
    }
}
