//! Native PostgreSQL backups of valuable `ext_*` caches.
//!
//! Each dump uses one fixed snapshot, binary COPY, compressed per-table files,
//! verified checksums and a complete manifest. Restore is atomic and durable;
//! retention policy and pinned non-PAR2 preservation remain explicit. Offline
//! SQLite conversion belongs to the separately preserved recovery toolchain.

use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use postgres::Client;
use sha2::{Digest, Sha256};
use super::natural_cache::par2_quota;
pub use super::natural_cache::par2_quota::{Phase as Par2QuotaPhase,Retention as Par2Retention,Usage as Par2Usage};

fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The `ext_*` tables in the connection's schema, by name.
pub fn tables(c: &mut Client) -> Result<Vec<String>> {
    Ok(c.query(
        "select table_name::text from information_schema.tables
          where table_schema = current_schema() and table_type = 'BASE TABLE' and table_name like 'ext\\_%'
          order by table_name",
        &[],
    )?
    .iter()
    .map(|r| r.get(0))
    .collect())
}

/// A table's columns in the connection's schema, in order.
fn pg_columns(c: &mut impl postgres::GenericClient, table: &str) -> Result<Vec<String>> {
    Ok(c.query(
        "select column_name::text from information_schema.columns
          where table_schema = current_schema() and table_name = $1 order by ordinal_position",
        &[&table],
    )?
    .iter()
    .map(|r| r.get(0))
    .collect())
}

/// A completed restore must survive an immediate crash, irrespective of the
/// caller session or role's synchronous_commit default.
fn durable(tx: &mut postgres::Transaction<'_>) -> Result<()> {
    tx.batch_execute("set local synchronous_commit = on")?;
    Ok(())
}

fn tables_of(tx: &mut postgres::Transaction<'_>) -> Result<Vec<String>> {
    Ok(tx
        .query(
            "select table_name::text from information_schema.tables
              where table_schema = current_schema() and table_type = 'BASE TABLE' and table_name like 'ext\\_%'",
            &[],
        )?
        .iter()
        .map(|r| r.get(0))
        .collect())
}


/// Every identity column of the ext tables set past the highest value in it, after rows came in with their own
/// (a move, a restore), so the next row Postgres numbers doesn't take one of theirs.
fn advance_counters(tx: &mut postgres::Transaction<'_>) -> Result<()> {
    let cols: Vec<(String, String)> = tx
        .query(
            "select table_name::text, column_name::text from information_schema.columns
              where table_schema = current_schema() and table_name like 'ext\\_%' and is_identity = 'YES'",
            &[],
        )?
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    for (t, c) in cols {
        let (tq, cq) = (quoted(&t), quoted(&c));
        tx.execute(
            &format!(
                "select setval(pg_get_serial_sequence($1, $2), coalesce((select max({cq}) from {tq}), 0) + 1, false)"
            ),
            &[&tq, &c],
        )?;
    }
    let matcher_revision_installed: bool = tx.query_one(
        "select exists(select 1 from pg_sequences where schemaname=current_schema() and sequencename='ext_predb_key_revision')", &[]
    )?.get(0);
    if matcher_revision_installed {
        // A restored completed matcher revision is a stale-result fence, not
        // an identity column. Its next certificate must exceed that revision
        // AND any sequence history already observed in this destination.
        // Do not consume a fresh sequence when no completion was restored.
        tx.query_opt("select setval('ext_predb_key_revision', greatest((select last_value from ext_predb_key_revision),revision),true)
            from ext_predb_key_meta where singleton=1", &[])?;
    }
    Ok(())
}

// ---------------------------------------------------------------- dumps

/// The manifest of one dump: what's in it, and what each table held when it was taken.
#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub taken_at: String,
    pub schema_version: i32,
    pub tables: Vec<Dumped>,
    /// taken with `--allow-shrink`: what the data is meant to be from now on, so later dumps are measured against it
    /// and newer ones only, not against the dumps before it
    #[serde(default)]
    pub baseline: bool,
    /// Same-snapshot accounting of live optional PAR2 retention, not readiness.
    #[serde(default)]
    pub par2_usage:Option<Par2Usage>,
    /// Restore returns the verified source manifest and explicit final policy
    /// result; source COPY row counts/checksums are never relabelled retained.
    #[serde(skip)]
    pub restore_retention:Option<Par2Retention>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Dumped {
    pub table: String,
    pub columns: Vec<String>,
    pub rows: i64,
    pub file: String,
    pub bytes: u64,
    /// SHA-256 of the table's COPY stream as Postgres sent it (before compression), checked when the dump is
    /// written and again before a restore commits: a damaged file, or another dump's, is refused
    #[serde(default)]
    pub sha256: Option<String>,
    /// for a table whose rows come and go (`CHURNS`), how many of them never expire (`PINNED`): guarded by `shrank`
    /// as other tables' rows are, with no loss allowed
    #[serde(default)]
    pub pinned: Option<i64>,
}

/// The rows of a `CHURNS` table that are kept forever: the HTTP cache's static historical PreDB archive snapshots
/// (`raw_store::is_immutable_predb_page`).
const PINNED: &[(&str, &str)] = &[
    ("ext_http_cache", "source in ('nzedb_archive', 'dats_pciso_1998')"),
    ("ext_preserved_cache_rows", "family not in ('ext_raw_par2','ext_raw_par2_files')"),
];

/// A writer that hashes what goes through it.
struct Hashed<W> {
    inner: W,
    sha: Sha256,
}

impl<W: Write> Write for Hashed<W> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(b)?;
        self.sha.update(&b[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// `file` decompressed, its SHA-256 compared with `want`.
fn check_file(file: &Path, want: &str) -> Result<()> {
    let mut dec = zstd::Decoder::new(BufReader::new(std::fs::File::open(file)?))?;
    let mut h = Hashed { inner: std::io::sink(), sha: Sha256::new() };
    std::io::copy(&mut dec, &mut h).with_context(|| format!("reading {}", file.display()))?;
    let got = format!("{:x}", h.sha.finalize());
    if got != want {
        bail!("{}: SHA-256 {got}, the manifest says {want}", file.display());
    }
    Ok(())
}

const MANIFEST: &str = "manifest.json";

/// Every `ext_*` table written under `dir/<time>/` from one snapshot (repeatable read): a zstd-compressed binary
/// COPY per table and `manifest.json` last. It's made under a `.partial` name and renamed when whole, so a dump
/// directory without that suffix is complete. Then all but the newest `keep` dumps are removed. Returns the dump's
/// directory. A dump that looks like data was lost (`shrank`) is refused with `Shrank`, unless `allow_shrink`.
pub fn dump(c: &mut Client, dir: &Path, keep: usize, allow_shrink: bool) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let partial = dir.join(format!("{stamp}.partial"));
    let done = dir.join(&stamp);
    if done.exists() || partial.exists() {
        bail!("{} is there already", done.display());
    }
    std::fs::create_dir(&partial)?;
    let made = (|| -> Result<()> {
        let mut tx =
            c.build_transaction().isolation_level(postgres::IsolationLevel::RepeatableRead).read_only(true).start()?;
        let schema: String = tx.query_one("select current_schema()::text", &[])?.get(0);
        let version: i32 = tx.query_one("select coalesce(max(version), 0) from schema_migrations", &[])?.get(0);
        let par2_usage=if par2_quota::installed(&mut tx)? {Some(par2_quota::usage(&mut tx)?)} else {None};
        ensure!(par2_usage.is_none_or(|u|u.phase==par2_quota::Phase::Ready),"PAR2 retention is incomplete: finish maintenance before a qualified backup");
        let names: Vec<String> = tables_of(&mut tx)?;
        if names.is_empty() {
            bail!("schema {schema} has no ext_* tables: nothing to dump (wrong schema, or not migrated)");
        }
        let mut tables = Vec::new();
        for t in names {
            let columns = pg_columns(&mut tx, &t)?;
            let list = columns.iter().map(|c| quoted(c)).collect::<Vec<_>>().join(", ");
            let rows: i64 = tx.query_one(&format!("select count(*) from {}", quoted(&t)), &[])?.get(0);
            let pinned = match PINNED.iter().find(|(p, _)| *p == t) {
                Some((_, kept)) => Some(
                    tx.query_one(&format!("select count(*) from {} where {kept}", quoted(&t)), &[])?
                        .get::<_, i64>(0),
                ),
                None => None,
            };
            let file = format!("{t}.copy.zst");
            let path = partial.join(&file);
            let out = std::fs::File::create(&path)?;
            let enc = zstd::Encoder::new(BufWriter::new(out), 6)?;
            let mut hashed = Hashed { inner: enc, sha: Sha256::new() };
            let mut reader =
                tx.copy_out(&format!("copy {} ({list}) to stdout (format binary)", quoted(&t)))?;
            std::io::copy(&mut reader, &mut hashed)?;
            drop(reader);
            let sha = format!("{:x}", hashed.sha.finalize());
            let w = hashed.inner.finish()?;
            let f = w.into_inner().map_err(|e| e.into_error())?;
            f.sync_all()?;
            drop(f);
            // read back: what's on disk is what Postgres sent
            check_file(&path, &sha)?;
            let bytes = std::fs::metadata(&path)?.len();
            tables.push(Dumped { table: t, columns, rows, file, bytes, sha256: Some(sha), pinned });
        }
        tx.commit()?;
        let m = Manifest { taken_at: stamp.clone(), schema_version: version, tables, baseline: allow_shrink,
            par2_usage,restore_retention:None };
        if let Some(why) = shrank(&kept_manifests(dir), &m).filter(|_| !allow_shrink) {
            return Err(Shrank(why).into());
        }
        let mf = partial.join(MANIFEST);
        let mut f = std::fs::File::create(&mf)?;
        f.write_all(serde_json::to_string_pretty(&m)?.as_bytes())?;
        f.sync_all()?;
        let total: u64 = m.tables.iter().map(|t| t.bytes).sum();
        let rows: i64 = m.tables.iter().map(|t| t.rows).sum();
        crate::ui::dim(&format!(
            "ext dump of schema {schema}: {} tables, {rows} rows, {:.1} MB",
            m.tables.len(),
            total as f64 / 1e6
        ));
        Ok(())
    })();
    if let Err(e) = made {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(e);
    }
    std::fs::File::open(&partial)?.sync_all()?;
    std::fs::rename(&partial, &done)?;
    // the rename is only durable once the directory holding it is synced: until then a crash could lose the new
    // dump, soo the old ones stay
    match std::fs::File::open(dir).and_then(|d| d.sync_all()) {
        Ok(()) => {
            if let Err(e) = prune(dir, keep) {
                crate::ui::warn(&format!("ext dump: couldnt remove old dumps ({e:#})"));
            }
        }
        Err(e) => crate::ui::warn(&format!(
            "ext dump: couldnt sync {} after the rename ({e}), so no old dump was removed",
            dir.display()
        )),
    }
    Ok(done)
}

/// Why a dump was refused: the tables hold less than the dumps kept say they did (`shrank`).
#[derive(Debug)]
pub struct Shrank(pub String);

impl std::fmt::Display for Shrank {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "refusing to keep this dump in the rotation: {} (the dumps before it are kept)", self.0)
    }
}

impl std::error::Error for Shrank {}

/// Every finished dump's manifest in `dir` that can be read, from the newest baseline on (oldest first).
fn kept_manifests(dir: &Path) -> Vec<Manifest> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    since_baseline(
        entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| is_dump_name(n))
            .filter_map(|n| serde_json::from_slice::<Manifest>(&std::fs::read(dir.join(n).join(MANIFEST)).ok()?).ok())
            .collect::<Vec<_>>(),
    )
}

/// The newest baseline and the dumps after it, when there is one (`Manifest::baseline`), oldest first.
fn since_baseline(mut kept: Vec<Manifest>) -> Vec<Manifest> {
    kept.sort_by(|a, b| a.taken_at.cmp(&b.taken_at));
    if let Some(i) = kept.iter().rposition(|m| m.baseline) {
        kept.drain(..i);
    }
    kept
}

/// A table whose rows come and go by design (the HTTP cache's entries expire after 12 hours): left out of `shrank`,
/// all but its `PINNED` rows.
const CHURNS: &[&str] = &["ext_http_cache"];

/// Why `now` looks like a database that lost data since the dumps kept: a table they had is gone, or one is below
/// the most rows any of them had for it by more than a tenth (to none at all, for a small table). External data is
/// only ever added to, so either is a sign of a wipe, a bad restore or a wrong schema, and such a dump mustn't push
/// the good ones out of the rotation. Measured against the most any kept dump had, so a loss night after night is
/// caught too.
fn par2_table(table:&str)->bool {matches!(table,"ext_raw_par2"|"ext_raw_par2_files"|"ext_preserved_cache_rows")}
fn par2_rows(manifest:&Manifest)->Option<u64> {
    let mut rows=0u64;
    for table in &manifest.tables {
        if !par2_table(&table.table) {continue;}
        let n=if table.table=="ext_preserved_cache_rows" {table.rows.checked_sub(table.pinned?)?} else {table.rows};
        rows=rows.checked_add(u64::try_from(n).ok()?)?;
    }
    Some(rows)
}
fn quota_shrink(kept:&[Manifest],now:&Manifest)->bool {
    let Some(usage)=now.par2_usage else{return false};let Some(retained)=par2_rows(now) else{return false};
    if retained!=usage.retained_rows {return false;}
    kept.iter().all(|prior| {
        let Some(old)=par2_rows(prior) else{return false};if retained>=old {return true;}
        let previous=prior.par2_usage.map_or(0,|u|u.evicted_rows);
        usage.evicted_rows.checked_sub(previous).is_some_and(|evicted|evicted>=old-retained)
    })
}

fn shrank(kept: &[Manifest], now: &Manifest) -> Option<String> {
    let mut most: std::collections::BTreeMap<&str, i64> = std::collections::BTreeMap::new();
    let mut most_pinned: std::collections::BTreeMap<&str, i64> = std::collections::BTreeMap::new();
    for m in kept {
        for t in &m.tables {
            let e = most.entry(t.table.as_str()).or_insert(0);
            *e = (*e).max(t.rows);
            if let Some(p) = t.pinned {
                let e = most_pinned.entry(t.table.as_str()).or_insert(0);
                *e = (*e).max(p);
            }
        }
    }
    // the rows of a churning table that never expire may not go down at all: they're static snapshots
    for (table, &pinned) in &most_pinned {
        let now_pinned = now.tables.iter().find(|t| t.table == *table).map_or(0, |t| t.pinned.unwrap_or(0));
        if now_pinned < pinned {
            return Some(format!("{table} has {now_pinned} rows kept forever, an earlier dump had {pinned}"));
        }
    }
    let quota_shrink=quota_shrink(kept,now);
    for (table, &rows) in &most {
        match now.tables.iter().find(|t| t.table == *table) {
            // gone is refused for a churning table too: only its row count may go down
            None => return Some(format!("{table} was in an earlier dump and is gone")),
            Some(_) if CHURNS.contains(table) => {}
            Some(t) if par2_table(table) && t.rows<rows => {
                if !quota_shrink {return Some(format!("{table} shrank without matching whole-inventory PAR2 retention"));}
            }
            Some(t) if rows > 0 && (t.rows == 0 || t.rows * 10 < rows * 9) => {
                return Some(format!("{table} has {} rows, an earlier dump had {rows}", t.rows));
            }
            _ => {}
        }
    }
    None
}

/// Whether `name` is a finished dump's directory name (`YYYYMMDDTHHMMSSZ`).
fn is_dump_name(name: &str) -> bool {
    name.len() == 16
        && name.as_bytes()[8] == b'T'
        && name.ends_with('Z')
        && name[..8].bytes().all(|b| b.is_ascii_digit())
        && name[9..15].bytes().all(|b| b.is_ascii_digit())
}

/// All but the newest `keep` finished dumps removed, and any `.partial` left by a dump that died.
fn prune(dir: &Path, keep: usize) -> Result<()> {
    let mut done = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if !e.file_type()?.is_dir() {
            continue;
        }
        if is_dump_name(&name) {
            done.push(name);
        } else if name.strip_suffix(".partial").is_some_and(is_dump_name) {
            // one being made now has a stamp of this second; older ones are dead
            let age = e.metadata()?.modified()?.elapsed().unwrap_or_default();
            if age > Duration::from_secs(6 * 3600) {
                std::fs::remove_dir_all(e.path())?;
            }
        }
    }
    done.sort();
    let n = done.len().saturating_sub(keep.max(1));
    for name in &done[..n] {
        std::fs::remove_dir_all(dir.join(name))?;
    }
    Ok(())
}

/// A dump put back: each of its tables COPYed into the connection's schema in one transaction, its SHA-256 and row
/// count checked before anything commits. Tables that have rows are refused unless `replace`, which empties them first.
pub fn restore(c: &mut Client, from: &Path, replace: bool) -> Result<Manifest> {
    let mut m: Manifest = serde_json::from_slice(&std::fs::read(from.join(MANIFEST)).context("reading the manifest")?)?;
    let mut tx = c.transaction()?;
    durable(&mut tx)?;
    let version: i32 = tx.query_one("select coalesce(max(version), 0) from schema_migrations", &[])?.get(0);
    if version < m.schema_version {
        bail!("the dump is of schema version {}, this database is at {version}: migrate it first", m.schema_version);
    }
    // Schema fence first, then accounting, then TRUNCATE table locks. Taking
    // quota while waiting for a live exclusive owner would reverse its order.
    tx.query_one("select pg_advisory_xact_lock_shared(hashtext('atlas.indexer'),hashtext(current_schema()))",&[])?;
    if par2_quota::installed(&mut tx)? {par2_quota::lock(&mut tx)?;}
    // all or nothing: a dump missing a table would leave that table as it is beside restored ones
    let missing: Vec<String> =
        tables_of(&mut tx)?.into_iter().filter(|t| !m.tables.iter().any(|d| &d.table == t)).collect();
    // a dump from before a migration that added a table can't have it, and is still a dump of all there was then;
    // a dump of the same version missing one is broken
    if !missing.is_empty() && m.schema_version >= version {
        bail!("the dump has no {}: refusing a partial restore", missing.join(", "));
    }
    if !missing.is_empty() {
        crate::ui::warn(&format!(
            "the dump (schema version {}) predates {}: left as they are",
            m.schema_version,
            missing.join(", ")
        ));
    }
    // Validate the entire replacement before changing anything. PostgreSQL
    // requires referencing tables in the SAME TRUNCATE, even when empty.
    let mut truncate = Vec::new();
    for t in &m.tables {
        let plain = |n: &str| n.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !t.table.starts_with("ext_") || !plain(&t.table) || t.file != format!("{}.copy.zst", t.table) {
            bail!("the manifest names {:?} / {:?}: not a dump of the ext tables", t.table, t.file);
        }
        let q = quoted(&t.table);
        let any: bool = tx.query_one(&format!("select exists (select 1 from {q})"), &[])?.get(0);
        if any {
            if !replace {
                bail!("{} has rows: refused (--replace to empty it first)", t.table);
            }
        }
        truncate.push(t.table.clone());
    }
    if truncate.iter().any(|table| table == "ext_predb_titles") {
        // Only these derived matcher/scan relations depend on replaced title
        // identities. Unrelated foreign keys must still refuse the restore.
        for table in ["ext_predb_title_keys", "ext_predb_key_meta", "ext_predb_key_build", "predb_late_scan", "predb_late_matches"] {
            let exists: bool = tx.query_one("select exists(select 1 from information_schema.tables where table_schema=current_schema() and table_name=$1 and table_type='BASE TABLE')", &[&table])?.get(0);
            if exists && !truncate.iter().any(|name| name == table) { truncate.push(table.into()); }
        }
    }
    if !truncate.is_empty() {
        tx.batch_execute(&format!("truncate {}", truncate.iter().map(|name| quoted(name)).collect::<Vec<_>>().join(", ")))?;
    }
    // Source/key writes invalidate completion metadata. Restore the snapshot
    // in dependency order so later triggers cannot erase verified earlier rows.
    let mut ordered = m.tables.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|table| match table.table.as_str() {
        "ext_predb_titles" => 0,
        "ext_predb_title_keys" => 1,
        "ext_predb_key_build" => 3,
        "ext_predb_key_meta" => 4,
        _ => 2,
    });
    for t in ordered {
        let q = quoted(&t.table);
        let Some(want) = &t.sha256 else {
            bail!("{}: the dump has no checksum (made before they were recorded), so it can't be verified", t.table);
        };
        let list = t.columns.iter().map(|c| quoted(c)).collect::<Vec<_>>().join(", ");
        let file = std::fs::File::open(from.join(&t.file))?;
        let mut dec = zstd::Decoder::new(BufReader::new(file))?;
        let mut w = tx.copy_in(&format!("copy {q} ({list}) from stdin (format binary)"))?;
        let mut buf = vec![0u8; 1 << 20];
        let mut sha = Sha256::new();
        loop {
            let n = dec.read(&mut buf)?;
            if n == 0 {
                break;
            }
            sha.update(&buf[..n]);
            w.write_all(&buf[..n])?;
        }
        w.finish()?;
        // before anything commits: a file that decodes and COPYs fine can still be the wrong one
        let got = format!("{:x}", sha.finalize());
        if &got != want {
            bail!("{}: the file's SHA-256 is {got}, the manifest says {want}: not restored", t.table);
        }
        let rows: i64 = tx.query_one(&format!("select count(*) from {q}"), &[])?.get(0);
        if rows != t.rows {
            bail!("{}: {rows} rows back, the dump says {}", t.table, t.rows);
        }
    }
    advance_counters(&mut tx)?;
    m.restore_retention=par2_quota::finish_import_if_installed(&mut tx)?;
    if let Some(ref retention)=m.restore_retention {
        println!("  PAR2 restore retention: {} accounted rows before policy, {} retained rows, {} whole-inventory rows evicted; phase {:?}",
            retention.before.retained_rows,retention.retained.retained_rows,retention.evicted_rows()?,retention.retained.phase);
    }
    tx.commit()?;
    Ok(m)
}

/// Where the dumps go: `ext-dumps/` beside `atlas.db`.
pub fn dump_dir() -> PathBuf {
    crate::paths::database().with_file_name("ext-dumps")
}

/// When the nightly dump runs, local hour.
pub const DUMP_HOUR: u32 = 4;

/// How many dumps are kept.
pub const KEEP_DUMPS: usize = 7;

/// A thread that dumps the ext tables once a day, at `hour` (local time), until `stop`. A failed dump (Postgres
/// down) is tried again an hour later.
pub fn spawn_nightly(pool: Arc<super::Pool>, dir: PathBuf, hour: u32, stop: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("ext-dump".into())
        .spawn(move || {
            let mut next = next_at(chrono::Local::now(), hour);
            while !stop.load(Ordering::Relaxed) {
                if chrono::Local::now() < next {
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
                let r =
                    pool.get().map_err(anyhow::Error::from).and_then(|mut c| dump(&mut c, &dir, KEEP_DUMPS, false));
                match r {
                    Ok(_) => next = next_at(chrono::Local::now(), hour),
                    // trying again in an hour would find the same: said, and left for the next night
                    Err(e) if e.is::<Shrank>() => {
                        crate::ui::error(&format!(
                            "ext dump: {e}; check the ext_* tables, or `atlas --dump-ext --allow-shrink` if it was meant"
                        ));
                        next = next_at(chrono::Local::now(), hour);
                    }
                    Err(e) => {
                        crate::ui::warn(&format!("ext dump failed ({e:#}); trying again in an hour"));
                        next = chrono::Local::now() + chrono::Duration::hours(1);
                    }
                }
            }
        })
        .expect("spawning the ext dump thread");
}

/// The next `hour`:00 local after `now`.
fn next_at(now: chrono::DateTime<chrono::Local>, hour: u32) -> chrono::DateTime<chrono::Local> {
    use chrono::{Duration as D, TimeZone};
    let day = now.date_naive();
    for d in 0..3 {
        if let Some(t) = (day + D::days(d)).and_hms_opt(hour.min(23), 0, 0)
            && let Some(t) = chrono::Local.from_local_datetime(&t).earliest()
            && t > now
        {
            return t;
        }
    }
    now + D::days(1)
}

/// `atlas --dump-ext` (a dump now, into `ext-dumps/`) and `atlas --restore-ext DIR [--replace]`.
pub fn run_cli(args: &[String]) -> i32 {
    let go = || -> Result<()> {
        let storage = crate::config::storage_config();
        storage.check()?;
        if storage.url.is_empty() {
            bail!("no [storage] url in config.toml");
        }
        let pool = super::Pool::new(&storage)?;
        let mut c = pool.get()?;
        if args.iter().any(|a| a == "--restore-ext") {
            let from = args
                .iter()
                .skip_while(|a| *a != "--restore-ext")
                .nth(1)
                .filter(|a| !a.starts_with("--"))
                .context("--restore-ext DIR")?;
            super::migrate::run(&mut c, pool.schema())?;
            let m = restore(&mut c, Path::new(from), args.iter().any(|a| a == "--replace"))?;
            println!("restored {} tables from the dump of {}", m.tables.len(), m.taken_at);
        } else {
            let d = dump(&mut c, &dump_dir(), KEEP_DUMPS, args.iter().any(|a| a == "--allow-shrink"))?;
            println!("dumped to {}", d.display());
        }
        Ok(())
    };
    match go() {
        Ok(()) => 0,
        Err(e) => {
            crate::ui::error(&format!("ext: {e:#}"));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Literal independent native cache rows: no source DB, converter, import
    /// receipt or historical catalog is involved in native backup qualification.
    fn pg_cache_fixture(c: &mut Client) {
        let mut tx = c.transaction().unwrap();
        durable(&mut tx).unwrap();
        tx.execute("insert into ext_predb_titles(title,source,added_at) values('Some.Movie.2020.1080p.BluRay-GRP',3,10)", &[]).unwrap();
        tx.execute("insert into ext_http_cache(cache_key,source,url,status,body_zst,fetched_at) values('k','nzedb_archive','https://example.invalid/a',200,$1,5)", &[&vec![0_u8,255]]).unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn dump_names_and_next_time() {
        assert!(is_dump_name("20261109T030000Z"));
        assert!(!is_dump_name("20261109T030000Z.partial"));
        assert!(!is_dump_name("keep"));
        let now = chrono::Local::now();
        let n = next_at(now, 4);
        assert!(n > now && n - now <= chrono::Duration::hours(25));
    }

    #[test]
    fn a_dump_that_lost_rows_or_a_table_is_refused() {
        let t = |name: &str, rows| Dumped {
            table: name.into(),
            columns: vec![],
            rows,
            file: format!("{name}.copy.zst"),
            bytes: 0,
            sha256: None,
            pinned: None,
        };
        let m = |tables| Manifest { taken_at: String::new(), schema_version: 2, tables, baseline: false,par2_usage:None,restore_retention:None };
        let kept = [
            m(vec![t("ext_a", 5000), t("ext_b", 3), t("ext_http_cache", 900)]),
            m(vec![t("ext_a", 4600), t("ext_b", 3), t("ext_http_cache", 10)]),
        ];
        let ok = |a, b, h| shrank(&kept, &m(vec![t("ext_a", a), t("ext_b", b), t("ext_http_cache", h)]));
        assert_eq!(ok(5001, 3, 0), None, "the HTTP cache comes and goes");
        assert_eq!(ok(4600, 3, 5), None, "within a tenth of the most a kept dump had");
        assert!(ok(4400, 3, 5).is_some(), "measured against the most, not the newest");
        assert!(ok(5000, 0, 5).is_some(), "a small table emptied");
        assert!(shrank(&kept, &m(vec![t("ext_a", 5000), t("ext_http_cache", 1)])).is_some(), "a table gone");
        assert_eq!(shrank(&[], &m(vec![])), None);
        // a dump kept with --allow-shrink is where later ones are measured from
        let mut base = m(vec![t("ext_a", 100), t("ext_b", 3)]);
        base.baseline = true;
        base.taken_at = "20261103T040000Z".into();
        let mut old = kept.into_iter().collect::<Vec<_>>();
        old[0].taken_at = "20261101T040000Z".into();
        old[1].taken_at = "20261102T040000Z".into();
        old.push(base);
        let since = since_baseline(old);
        assert_eq!(since.len(), 1);
        assert_eq!(shrank(&since, &m(vec![t("ext_a", 101), t("ext_b", 3)])), None);
        // the HTTP cache's pinned snapshots may not go down at all, however its other rows come and go
        let cache = |rows, pinned| Dumped { pinned: Some(pinned), ..t("ext_http_cache", rows) };
        let kept = [m(vec![cache(900, 40)]), m(vec![cache(50, 41)]), m(vec![t("ext_http_cache", 7)])];
        assert_eq!(shrank(&kept, &m(vec![cache(41, 41)])), None);
        assert!(shrank(&kept, &m(vec![cache(5000, 40)])).is_some(), "one pinned row lost");
        assert!(shrank(&kept, &m(vec![t("ext_http_cache", 5000)])).is_some(), "pinned rows not counted");
        // a churning table with no pinned rows may empty, but not disappear
        let kept = [m(vec![cache(30, 0), t("ext_a", 5)])];
        assert_eq!(shrank(&kept, &m(vec![cache(0, 0), t("ext_a", 5)])), None);
        assert!(shrank(&kept, &m(vec![t("ext_a", 5)])).is_some(), "the HTTP cache table gone");
    }

    #[test]
    fn backup_shrink_accepts_only_accounted_par2_eviction_and_pins_other_preserved_families() {
        let table=|name:&str,rows,pinned|Dumped {table:name.into(),columns:vec![],rows,file:format!("{name}.copy.zst"),bytes:0,sha256:None,pinned};
        let manifest=|files,preserved,pinned,evicted|Manifest {taken_at:String::new(),schema_version:11,
          tables:vec![table("ext_raw_par2",0,None),table("ext_raw_par2_files",files,None),table("ext_preserved_cache_rows",preserved,Some(pinned)),table("ext_predb_titles",3,None)],
          baseline:false,par2_usage:Some(par2_quota::Usage {phase:par2_quota::Phase::Ready,retained_rows:(files+preserved-pinned) as u64,
            evicted_rows:evicted,..Default::default()}),restore_retention:None};
        let old=manifest(10,10,5,0);let kept=[old];
        assert!(shrank(&kept,&manifest(2,7,5,11)).is_none(),"11 actual optional rows evicted, all essential variants remain");
        assert!(shrank(&kept,&manifest(2,7,5,10)).is_some(),"counter does not explain the drop");
        assert!(shrank(&kept,&manifest(2,7,4,100)).is_some(),"even a large PAR2 counter cannot hide one non-PAR2 preserved loss");
        let mut no_proof=manifest(2,7,5,11);no_proof.par2_usage=None;assert!(shrank(&kept,&no_proof).is_some());
        let mut bad_count=manifest(2,7,5,11);bad_count.par2_usage.as_mut().unwrap().retained_rows=1;assert!(shrank(&kept,&bad_count).is_some());
        let mut essential_loss=manifest(2,7,5,11);essential_loss.tables.iter_mut().find(|t|t.table=="ext_predb_titles").unwrap().rows=0;
        assert!(shrank(&kept,&essential_loss).is_some(),"PreDB is never a churning family");
    }

    #[test]
    fn pinned_rows_are_the_raw_stores_forever_ones() {
        for key in ["nzedb_archive", "dats_pciso_1998"] {
            assert!(crate::raw_store::is_immutable_predb_page(key, 0));
            assert!(PINNED[0].1.contains(&format!("'{key}'")));
        }
    }

    #[test]
    fn prune_keeps_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        for s in ["20261101T040000Z", "20261102T040000Z", "20261103T040000Z", "other"] {
            std::fs::create_dir(dir.path().join(s)).unwrap();
        }
        prune(dir.path(), 2).unwrap();
        let mut left: Vec<String> =
            std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        assert_eq!(left, ["20261102T040000Z", "20261103T040000Z", "other"]);
    }

    #[test]
    #[ignore = "requires explicit isolated PostgreSQL native backup qualification"]
    fn native_dump_and_restore_commit_durably_when_the_default_is_off() {
        let s = super::super::testing::Schema::new().expect("explicit isolated PG required");
        let dir = tempfile::tempdir().unwrap();
        let mut c = s.pool.get().unwrap();
        c.batch_execute(
            "create function durable_only() returns trigger language plpgsql as $$ begin
               if current_setting('synchronous_commit') <> 'on' then raise exception 'not durable'; end if;
               return new; end $$;
             create trigger durable_only before insert on ext_predb_titles for each row execute function durable_only();
             set synchronous_commit = off",
        )
        .unwrap();
        pg_cache_fixture(&mut c);
        let d = dump(&mut c, &dir.path().join("ext-dumps"), 3, false).unwrap();
        restore(&mut c, &d, true).unwrap();
        let after: String = c.query_one("show synchronous_commit", &[]).unwrap().get(0);
        assert_eq!(after, "off", "only the transactions' own setting changed");
        // and without it the trigger does refuse
        let refused =
            c.execute("insert into ext_predb_titles (title, source, added_at) values ('Async.Row-GRP', 3, 1)", &[]);
        assert!(refused.is_err());
    }

    #[test]
    #[ignore = "requires explicit isolated PostgreSQL native backup qualification"]
    fn restore_replaces_predb_dependencies_without_cascading_unrelated_references() {
        let target = super::super::testing::Schema::new().expect("explicit isolated PG required");
        let dir = tempfile::tempdir().unwrap();
        let mut c = target.pool.get().unwrap();
        pg_cache_fixture(&mut c);
        let mut tx = c.transaction().unwrap();
        assert!(matches!(super::super::predb_cache::advance_in(&mut tx, 16).unwrap(), super::super::predb_cache::BuildStatus::Ready(_)));
        tx.commit().unwrap();
        let original: (i64,i64) = { let row = c.query_one("select revision,key_rows from ext_predb_key_meta", &[]).unwrap(); (row.get(0),row.get(1)) };
        assert!(original.1 > 0);
        let dump = dump(&mut c, &dir.path().join("dumps"), 3, false).unwrap();
        c.batch_execute("insert into predb_late_scan(singleton,title_id,title_revision,names_revision) values(true,1,1,1);
            create table unrelated_reference(title_id bigint references ext_predb_titles(id)); insert into unrelated_reference values(1)").unwrap();
        assert!(restore(&mut c, &dump, true).is_err(), "unrelated FK remains a blocker, never CASCADE");
        assert_eq!(c.query_one("select count(*) from predb_late_scan", &[]).unwrap().get::<_,i64>(0), 1, "failed restore rolls back derived-state reset");
        assert_eq!(c.query_one("select count(*) from unrelated_reference", &[]).unwrap().get::<_,i64>(0), 1);
        c.batch_execute("drop table unrelated_reference").unwrap();
        assert!(restore(&mut c, &dump, false).is_err(), "replacement still requires explicit admission");
        c.execute("update ext_predb_titles set title='Changed-GRP' where id=1", &[]).unwrap();
        restore(&mut c, &dump, true).unwrap();
        assert_eq!(c.query_one("select title from ext_predb_titles where id=1", &[]).unwrap().get::<_,String>(0), "Some.Movie.2020.1080p.BluRay-GRP");
        assert_eq!(c.query_one("select count(*) from predb_late_scan", &[]).unwrap().get::<_,i64>(0), 0);
        let row = c.query_one("select revision,key_rows from ext_predb_key_meta", &[]).unwrap();
        assert_eq!((row.get::<_,i64>(0),row.get::<_,i64>(1)), original, "restore invalidation triggers cannot erase the restored completion record");
        assert_eq!(c.query_one("select count(*) from ext_predb_title_keys", &[]).unwrap().get::<_,i64>(0), original.1);
    }

    #[test]
    #[ignore = "requires explicit isolated PostgreSQL native backup qualification"]
    fn restored_predb_revision_cannot_repeat_or_rewind_existing_history() {
        let source = super::super::testing::Schema::new().expect("explicit isolated PG required");
        let dir = tempfile::tempdir().unwrap();
        let mut c = source.pool.get().unwrap();
        pg_cache_fixture(&mut c);
        let mut tx = c.transaction().unwrap();
        let restored_revision = match super::super::predb_cache::advance_in(&mut tx, 16).unwrap() {
            super::super::predb_cache::BuildStatus::Ready(built) => built.revision,
            _ => panic!("tiny source index must reach actual EOF"),
        };
        tx.commit().unwrap();
        let dump = dump(&mut c, &dir.path().join("dumps"), 3, false).unwrap();
        for prior_calls in [0_i64, 32] {
            let target = super::super::testing::Schema::new().expect("explicit isolated PG required");
            let mut c = target.pool.get().unwrap();
            // Exercise both a fresh uncalled sequence and genuine higher
            // sequence history, never a hand-inserted completion record.
            let prior = c.query("select nextval('ext_predb_key_revision') from generate_series(1::bigint,$1::bigint)", &[&prior_calls]).unwrap();
            let prior_revision = prior.last().map(|row| row.get::<_,i64>(0)).unwrap_or(0);
            restore(&mut c, &dump, false).unwrap();
            let mut tx = c.transaction().unwrap();
            super::super::predb_cache::verify_read_in(&mut tx, restored_revision).unwrap();
            tx.commit().unwrap();
            c.execute("update ext_predb_titles set title='Different.Title.2021-GRP' where id=1", &[]).unwrap();
            let mut tx = c.transaction().unwrap();
            let changed_revision = match super::super::predb_cache::advance_in(&mut tx, 16).unwrap() {
                super::super::predb_cache::BuildStatus::Ready(built) => built.revision,
                _ => panic!("tiny changed index must reach actual EOF"),
            };
            tx.commit().unwrap();
            assert!(changed_revision > restored_revision && changed_revision > prior_revision, "completed change cannot reuse restored or prior revision");
            let mut tx = c.transaction().unwrap();
            let stale = super::super::predb_cache::verify_read_in(&mut tx, restored_revision).unwrap_err();
            assert!(stale.downcast_ref::<super::super::predb_cache::StaleRead>().is_some(), "stale lookup is rejected with the typed settlement refusal");
            tx.rollback().unwrap();
        }
    }

    #[test]
    #[ignore = "requires explicit isolated PostgreSQL native backup qualification"]
    fn native_dump_restore_round_trip() {
        let s = super::super::testing::Schema::new().expect("explicit isolated PG required");
        let dir = tempfile::tempdir().unwrap();
        let mut c = s.pool.get().unwrap();
        pg_cache_fixture(&mut c);
        // a new title is numbered past the moved ones, as SQLite would
        let id: i64 = c
            .query_one(
                "insert into ext_predb_titles (title, source, added_at) values ('Next', 3, 11) returning id",
                &[],
            )
            .unwrap()
            .get(0);
        assert_eq!(id, 2);

        let dumps = dir.path().join("ext-dumps");
        let d = dump(&mut c, &dumps, 3, false).unwrap();
        let all = tables(&mut c).unwrap().join(", ");
        c.batch_execute(&format!("truncate {all}, predb_late_scan, predb_late_matches")).unwrap();
        let m = restore(&mut c, &d, false).unwrap();
        assert_eq!(m.tables.iter().find(|t| t.table == "ext_predb_titles").unwrap().rows, 2);
        let title: String = c.query_one("select title from ext_predb_titles where id = 1", &[]).unwrap().get(0);
        assert_eq!(title, "Some.Movie.2020.1080p.BluRay-GRP");
        let body: Vec<u8> = c.query_one("select body_zst from ext_http_cache", &[]).unwrap().get(0);
        assert_eq!(body, [0x00, 0xff]);
        assert!(restore(&mut c, &d, false).is_err(), "tables with rows are refused without --replace");
        let pinned = m.tables.iter().find(|t| t.table == "ext_http_cache").unwrap().pinned;
        assert_eq!(pinned, Some(1), "the nzedb archive page is counted as kept forever");
        // a file that isn't the one dumped (here: another table's, decodable and the same size of rows) is
        // refused before anything commits, even with --replace
        let swapped = dir.path().join("swapped");
        std::fs::create_dir(&swapped).unwrap();
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            std::fs::copy(e.path(), swapped.join(e.file_name())).unwrap();
        }
        let mut sm: Manifest = serde_json::from_slice(&std::fs::read(swapped.join(MANIFEST)).unwrap()).unwrap();
        let t = sm.tables.iter_mut().find(|t| t.table == "ext_predb_titles").unwrap();
        t.sha256 = Some("0".repeat(64));
        std::fs::write(swapped.join(MANIFEST), serde_json::to_vec(&sm).unwrap()).unwrap();
        let err = restore(&mut c, &swapped, true).unwrap_err().to_string();
        assert!(err.contains("SHA-256"), "{err}");
        let n: i64 = c.query_one("select count(*) from ext_predb_titles", &[]).unwrap().get(0);
        assert_eq!(n, 2, "the refused restore left the tables as they were");
        restore(&mut c, &d, true).unwrap();
        let id: i64 = c
            .query_one(
                "insert into ext_predb_titles (title, source, added_at) values ('After', 3, 12) returning id",
                &[],
            )
            .unwrap()
            .get(0);
        assert_eq!(id, 3, "numbered past what the restore brought back");
    }
}
