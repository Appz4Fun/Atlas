//! Read-only conversion adapter for explicitly captured catalog checkpoints.
//! This module is removable with the SQLite conversion tools; runtime catalog
//! inspection depends only on PostgreSQL object handles.

use crate::history_identity::{ErrorCode, HistoryError};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SourceKind {
    Main,
    Current,
    Frozen,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogCounts {
    pub releases: u64,
    pub collections: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileManifest {
    pub source_id: i64,
    pub kind: SourceKind,
    pub shard: Option<u32>,
    pub paired_current: Option<i64>,
    pub path: PathBuf,
    pub byte_length: u64,
    pub digest: [u8; 32],
    pub schema_digest: [u8; 32],
    pub catalog_counts: Option<CatalogCounts>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub checkpoint_id: String,
    pub source_uuid: String,
    pub shard_count: u32,
    pub reserved_next_id: i64,
    pub archive_manifest: [u8; 32],
    pub main_receipt: [u8; 32],
    pub ext_receipt: [u8; 32],
    /// Captured configuration/grouping provenance, not inferred defaults.
    pub grouping_facts: Vec<u8>,
    /// Operator attestation: independent consolidated files remain immutable
    /// for this conversion. Hash verification checks content at open; stat
    /// checks detect ordinary later mutations but do not prove immutability.
    pub immutable_checkpoint: bool,
    pub files: Vec<FileManifest>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Relation {
    Releases,
    Collections,
    Facts,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RowCursor {
    pub source_id: i64,
    pub relation: Relation,
    pub after: Option<i64>,
}
pub use crate::history_identity::{CellType, Column};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CellRef {
    pub fingerprint: [u8; 32],
    pub source_id: i64,
    pub relation: Relation,
    pub rowid: i64,
    pub column: Column,
    pub kind: CellType,
    pub bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Scalar {
    Null,
    Integer(i64),
    RealBits(u64),
    Cell(CellRef),
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Descriptor {
    pub key: i64,
    pub fields: Vec<(Column, Scalar)>,
}
#[derive(Clone, Copy)]
pub struct ReadBudget {
    pub rows: u32,
    pub bytes: u64,
    pub chunk_bytes: u32,
}
#[derive(Clone, Debug)]
pub struct CellProgress {
    pub next: u64,
    pub bytes: u64,
}
#[derive(Clone, Debug)]
pub struct PageEnd {
    pub cursor: RowCursor,
    pub complete: bool,
    pub rows: u64,
    pub bytes: u64,
}

pub const DESCRIPTOR_ADMISSION: u64 = 512;
pub const MAX_CHUNK: u32 = 65536;

// The cursor is owned by Source and is dropped before Source's connections.
// Keeping SQLite's overflow-page cursor avoids repeated prefix walks on a
// giant cell. No raw handle or connection is serialized or exposed.
struct RetainedCell {
    handle: std::ptr::NonNull<rusqlite::ffi::sqlite3_blob>,
    reference: CellRef,
    indexed_until: u64,
}
impl Drop for RetainedCell {
    fn drop(&mut self) {
        // SAFETY: sqlite3_blob_open returned this owned handle; it is closed
        // exactly once and Source's connection fields are still alive.
        unsafe {
            rusqlite::ffi::sqlite3_blob_close(self.handle.as_ptr());
        }
    }
}
struct FileSource {
    manifest: FileManifest,
    conn: Connection,
    stamp: std::fs::Metadata,
    columns: Vec<Column>,
    tables: Vec<String>,
}
#[derive(Clone, Copy)]
pub struct VerificationAdmission {
    pub max_total_bytes: u64,
    pub bytes_per_second: u64,
    pub read_chunk_bytes: u32,
    pub max_open_files: u32,
    pub sqlite_cache_bytes: u64,
    pub max_schema_bytes: u64,
}
impl VerificationAdmission {
    pub fn new(max_total_bytes: u64) -> Self {
        Self {
            max_total_bytes,
            bytes_per_second: 32 * 1024 * 1024,
            read_chunk_bytes: 65536,
            max_open_files: 33,
            sqlite_cache_bytes: 16 * 1024 * 1024,
            max_schema_bytes: 16 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct CellIo {
    pub cursor_opens: u64,
    pub read_calls: u64,
    pub bytes: u64,
    pub max_request: usize,
}
pub struct Source {
    cell_io: std::cell::RefCell<CellIo>,
    active_cell: std::cell::RefCell<Option<RetainedCell>>,
    manifest: Manifest,
    fingerprint: [u8; 32],
    manifest_bytes: Vec<u8>,
    files: BTreeMap<i64, FileSource>,
    pub verified_bytes: u64,
}

const CURRENT: &[Column] = &[
    Column::Id,
    Column::Name,
    Column::Group,
    Column::Poster,
    Column::Posted,
    Column::Size,
    Column::Complete,
    Column::Parts,
    Column::FileTotal,
    Column::Display,
    Column::Obfuscated,
    Column::NameSource,
];
const FROZEN: &[Column] = &[
    Column::Id,
    Column::Name,
    Column::Group,
    Column::Display,
    Column::Poster,
    Column::Posted,
    Column::Size,
    Column::Parts,
    Column::Complete,
    Column::Category,
    Column::Reason,
    Column::FrozenAt,
    Column::Payload,
    Column::Articles,
];
const ALIASES: &[Column] = &[Column::Alias, Column::Target];
const FACTS: &[Column] = &[Column::FactValue];

pub fn fact_names(kind: SourceKind) -> &'static [&'static str] {
    match kind {
        SourceKind::Main => &["next_seq"],
        SourceKind::Current => &["id_key0", "id_key1", "grouping_from"],
        SourceKind::Frozen => &[],
    }
}
fn table(kind: SourceKind, relation: Relation) -> Result<&'static str> {
    match (kind, relation) {
        (_, Relation::Facts) => Ok("meta"),
        (SourceKind::Current, Relation::Releases) => Ok("releases"),
        (SourceKind::Current, Relation::Collections) => Ok("collections"),
        (SourceKind::Frozen, Relation::Releases) => Ok("frozen_releases"),
        (SourceKind::Frozen, Relation::Collections) => Ok("frozen_collections"),
        _ => bail!(HistoryError::new(ErrorCode::UnsupportedSchema, "relation is not available for this source")),
    }
}

/// Initial schema hashing is separately admitted; no payload cell is read.
pub fn schema_digest(conn: &Connection) -> Result<[u8; 32]> {
    Ok(schema_digest_admitted(conn, 4 * 1024 * 1024)?.0)
}
fn schema_digest_admitted(conn: &Connection, limit: u64) -> Result<([u8; 32], u64)> {
    let excessive: bool =
        conn.query_row("select exists(select 1 from sqlite_schema where length(cast(sql as blob))>65536)", [], |r| {
            r.get(0)
        })?;
    ensure!(
        !excessive,
        HistoryError::new(ErrorCode::UnsupportedSchema, "schema object exceeds verification admission")
    );
    let mut q = conn.prepare("select type,name,tbl_name,sql from sqlite_schema order by type,name")?;
    let mut rows = q.query([])?;
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut count = 0usize;
    while let Some(row) = rows.next()? {
        count += 1;
        ensure!(count <= 4096, HistoryError::new(ErrorCode::UnsupportedSchema, "too many schema objects"));
        for i in 0..4 {
            let v: Option<String> = row.get(i)?;
            bytes =
                bytes.checked_add(8 + v.as_ref().map_or(0, |v| v.len() as u64)).context("schema length overflow")?;
            ensure!(
                bytes <= limit,
                HistoryError::new(ErrorCode::InsufficientBudget, "aggregate schema verification byte admission")
            );
            match v {
                Some(v) => {
                    hash.update((v.len() as u64).to_le_bytes());
                    hash.update(v.as_bytes());
                }
                None => hash.update(u64::MAX.to_le_bytes()),
            }
        }
    }
    Ok((hash.finalize().into(), bytes))
}
fn check_columns(conn: &Connection, table: &str, allowed: &[Column], optional: &[Column]) -> Result<Vec<Column>> {
    let mut q = conn.prepare(&format!("pragma table_xinfo('{table}')"))?;
    let found = q
        .query_map([], |r| {
            Ok((r.get::<_, String>(1)?, r.get::<_, i64>(5)?, r.get::<_, i64>(6)?, r.get::<_, String>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        !found.is_empty() && found.len() <= allowed.len(),
        HistoryError::new(ErrorCode::UnsupportedSchema, "missing/extra catalog columns")
    );
    let (kind, without_rowid): (String, i64) =
        conn.query_row("select type,wr from pragma_table_list where schema='main' and name=?1", [table], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    ensure!(
        kind == "table" && found.iter().filter(|(_, pk, _, _)| *pk > 0).count() == 1,
        HistoryError::new(ErrorCode::UnsupportedSchema, "catalog requires one original primary key")
    );
    if table == "releases" || table == "frozen_releases" {
        let indexed_pk: bool =
            conn.query_row("select exists(select 1 from pragma_index_list(?1) where origin='pk')", [table], |r| {
                r.get(0)
            })?;
        ensure!(
            without_rowid == 0 && !indexed_pk,
            HistoryError::new(ErrorCode::UnsupportedSchema, "release cells require an integer rowid alias")
        );
    }

    let mut cols = Vec::new();
    for (name, pk, hidden, declared) in &found {
        ensure!(*hidden == 0, HistoryError::new(ErrorCode::UnsupportedSchema, "generated catalog column"));
        let col = allowed
            .iter()
            .find(|c| c.name() == name)
            .context(HistoryError::new(ErrorCode::UnsupportedSchema, "unknown catalog column"))?;
        if matches!(col, Column::Id | Column::Alias) {
            ensure!(
                *pk == 1 && declared.eq_ignore_ascii_case("INTEGER"),
                HistoryError::new(ErrorCode::UnsupportedSchema, "catalog key is not the integer primary key")
            );
        }
        cols.push(*col);
    }
    for col in allowed {
        ensure!(
            cols.contains(col) || optional.contains(col),
            HistoryError::new(ErrorCode::UnsupportedSchema, "required catalog column missing")
        );
    }
    Ok(cols)
}
impl Source {
    pub fn open(manifest: Manifest, hash_budget: u64) -> Result<Self> {
        Self::open_with_admission(
            manifest,
            VerificationAdmission::new(hash_budget),
            &std::sync::atomic::AtomicBool::new(false),
        )
    }
    pub fn open_with_admission(
        manifest: Manifest,
        admission: VerificationAdmission,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Self> {
        use std::sync::atomic::Ordering;
        ensure!(
            !cancel.load(Ordering::Relaxed),
            HistoryError::new(ErrorCode::Cancelled, "initial source verification")
        );
        ensure!(
            admission.max_total_bytes > 0
                && admission.bytes_per_second > 0
                && (1..=65536).contains(&admission.read_chunk_bytes)
                && admission.max_open_files > 0
                && admission.sqlite_cache_bytes >= 1024
                && admission.max_schema_bytes > 0,
            HistoryError::new(ErrorCode::InsufficientBudget, "positive initial verification limits required")
        );
        ensure!(
            manifest.archive_manifest != [0; 32],
            HistoryError::new(ErrorCode::MissingArchive, "captured archive manifest identity is required")
        );
        let hash_budget = admission.max_total_bytes;
        let verification_started = std::time::Instant::now();
        ensure!(
            manifest.version == 1 && manifest.immutable_checkpoint,
            HistoryError::new(ErrorCode::SourceMismatch, "an immutable independent checkpoint is required")
        );
        ensure!(
            !manifest.checkpoint_id.is_empty()
                && manifest.checkpoint_id.len() <= 256
                && !manifest.source_uuid.is_empty()
                && manifest.source_uuid.len() <= 256,
            HistoryError::new(ErrorCode::SourceMismatch, "invalid capture identity")
        );
        ensure!(
            manifest.shard_count > 0
                && manifest.shard_count <= 1024
                && manifest.reserved_next_id > 0
                && !manifest.grouping_facts.is_empty()
                && manifest.grouping_facts.len() <= 4 * 1024 * 1024,
            HistoryError::new(ErrorCode::SourceMismatch, "required allocator/grouping facts missing")
        );
        ensure!(
            manifest.files.len() == 1 + 2 * manifest.shard_count as usize,
            HistoryError::new(ErrorCode::SourceMismatch, "explicit main/current/frozen pairs required")
        );
        ensure!(
            manifest.files.len() <= admission.max_open_files as usize
                && admission.sqlite_cache_bytes / manifest.files.len() as u64 >= 1024,
            HistoryError::new(ErrorCode::InsufficientBudget, "source connection/cache admission")
        );
        let cache_kib = admission.sqlite_cache_bytes / manifest.files.len() as u64 / 1024;
        let total = manifest
            .files
            .iter()
            .try_fold(0u64, |n, f| n.checked_add(f.byte_length).context("source length overflow"))?;
        ensure!(
            hash_budget > 0 && total <= hash_budget,
            HistoryError::new(ErrorCode::InsufficientBudget, "initial file hashing admission")
        );
        let mut files = BTreeMap::new();
        let mut schema_left = admission.max_schema_bytes;
        let mut verified_bytes = 0u64;
        let mut mains = 0;
        for file in &manifest.files {
            ensure!(
                file.source_id > 0 && !files.contains_key(&file.source_id),
                HistoryError::new(ErrorCode::SourceMismatch, "duplicate source identity")
            );
            ensure!(
                file.path.is_absolute() && !std::fs::symlink_metadata(&file.path)?.file_type().is_symlink(),
                HistoryError::new(ErrorCode::SourceMismatch, "source must be an explicit regular file")
            );
            for suffix in ["-wal", "-shm", "-journal"] {
                ensure!(
                    !PathBuf::from(format!("{}{suffix}", file.path.display())).exists(),
                    HistoryError::new(ErrorCode::SourceMismatch, "source is not a consolidated independent checkpoint")
                );
            }
            let mut input = std::fs::File::open(&file.path)?;
            #[cfg(target_os = "macos")]
            {
                use std::os::fd::AsRawFd;
                // SAFETY: the owned file descriptor is live. F_NOCACHE changes
                // only this verification descriptor's caching policy.
                ensure!(
                    unsafe { libc::fcntl(input.as_raw_fd(), libc::F_NOCACHE, 1) } == 0,
                    HistoryError::new(ErrorCode::InsufficientBudget, "cannot admit uncached initial verification")
                );
            }
            let before = input.metadata()?;
            ensure!(
                before.is_file() && before.len() == file.byte_length,
                HistoryError::new(ErrorCode::SourceMismatch, "source file length")
            );
            let mut h = Sha256::new();
            let mut buf = vec![0u8; admission.read_chunk_bytes as usize];
            let mut file_read = 0u64;
            let mut header = [0u8; 20];
            ensure!(
                file.byte_length >= 100,
                HistoryError::new(ErrorCode::UnsupportedSchema, "truncated SQLite header")
            );
            while file_read < file.byte_length {
                ensure!(
                    !cancel.load(Ordering::Relaxed),
                    HistoryError::new(ErrorCode::Cancelled, "initial source verification")
                );
                let nread = (file.byte_length - file_read).min(buf.len() as u64) as usize;
                let n = input.read(&mut buf[..nread])?;
                ensure!(n > 0, HistoryError::new(ErrorCode::SourceMismatch, "truncated checkpoint file"));
                if file_read < 20 {
                    let take = n.min(20 - file_read as usize);
                    header[file_read as usize..file_read as usize + take].copy_from_slice(&buf[..take]);
                }
                h.update(&buf[..n]);
                file_read += n as u64;
                verified_bytes = verified_bytes.checked_add(n as u64).context("source byte overflow")?;
                let target =
                    std::time::Duration::from_secs_f64(verified_bytes as f64 / admission.bytes_per_second as f64);
                while verification_started.elapsed() < target {
                    ensure!(
                        !cancel.load(Ordering::Relaxed),
                        HistoryError::new(ErrorCode::Cancelled, "initial source verification throttle")
                    );
                    std::thread::sleep(
                        target.saturating_sub(verification_started.elapsed()).min(std::time::Duration::from_millis(20)),
                    );
                }
            }
            ensure!(
                <[u8; 32]>::from(h.finalize()) == file.digest,
                HistoryError::new(ErrorCode::SourceMismatch, "file content digest")
            );
            ensure!(
                &header[..16] == b"SQLite format 3\0" && header[18] == 1 && header[19] == 1,
                HistoryError::new(
                    ErrorCode::UnsupportedSchema,
                    "consolidate the checkpoint in rollback-journal mode before opening"
                )
            );
            let conn = Connection::open_with_flags(
                &file.path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            // SAFETY: this connection owns the live SQLite handle. Bound SQL
            // text before schema parsing; retained payload cells keep SQLite's
            // independent data-length limit and incremental reads.
            unsafe {
                rusqlite::ffi::sqlite3_limit(conn.handle(), rusqlite::ffi::SQLITE_LIMIT_SQL_LENGTH, 65536);
            }
            conn.execute_batch(&format!(
                "pragma query_only=on;pragma mmap_size=0;pragma cache_size=-{cache_kib};begin"
            ))?;
            let (schema, bytes) = schema_digest_admitted(&conn, schema_left.min(4 * 1024 * 1024))?;
            schema_left -= bytes;
            ensure!(schema == file.schema_digest, HistoryError::new(ErrorCode::SourceMismatch, "schema fingerprint"));
            ensure!(
                file.kind == SourceKind::Main || file.catalog_counts.is_some(),
                HistoryError::new(ErrorCode::SourceMismatch, "selected catalog counts missing from content manifest")
            );
            let columns = match file.kind {
                SourceKind::Main => {
                    mains += 1;
                    Vec::new()
                }
                SourceKind::Current => {
                    check_columns(&conn, "collections", ALIASES, &[])?;
                    check_columns(&conn, "releases", CURRENT, &[Column::NameSource])?
                }
                SourceKind::Frozen => {
                    check_columns(&conn, "frozen_collections", ALIASES, &[])?;
                    check_columns(&conn, "frozen_releases", FROZEN, &[Column::Articles])?
                }
            };
            if file.kind != SourceKind::Main {
                let relation = if file.kind == SourceKind::Current { "releases" } else { "frozen_releases" };
                let greatest: Option<i64> = conn
                    .query_row(&format!("select id from {relation} order by id desc limit 1"), [], |r| r.get(0))
                    .optional()?;
                ensure!(
                    greatest.is_none_or(|id| id < manifest.reserved_next_id),
                    HistoryError::new(
                        ErrorCode::SourceMismatch,
                        "allocator reservation does not exceed retained public IDs"
                    )
                );
            }
            for key in fact_names(file.kind) {
                let v: i64 = conn
                    .query_row("select value from meta where key=?1", [key], |r| r.get(0))
                    .context(HistoryError::new(ErrorCode::SourceMismatch, "required integer source fact missing"))?;
                if *key == "next_seq" {
                    ensure!(
                        v > 0
                            && v.checked_mul(i64::from(manifest.shard_count))
                                .is_some_and(|n| n <= manifest.reserved_next_id),
                        HistoryError::new(ErrorCode::SourceMismatch, "allocator reservation below original next_seq")
                    );
                }
            }
            let tables = {
                let mut q = conn.prepare(
                    "select name from sqlite_schema where type='table' and name not like 'sqlite_%' order by name",
                )?;
                q.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?
            };
            ensure!(
                tables.len() <= 4096 && tables.iter().all(|name| name.len() <= 256),
                HistoryError::new(ErrorCode::UnsupportedSchema, "table inventory admission")
            );
            files.insert(file.source_id, FileSource { manifest: file.clone(), conn, stamp: before, columns, tables });
        }
        ensure!(mains == 1, HistoryError::new(ErrorCode::SourceMismatch, "exactly one main allocator source required"));
        for shard in 0..manifest.shard_count {
            let current: Vec<_> =
                manifest.files.iter().filter(|f| f.kind == SourceKind::Current && f.shard == Some(shard)).collect();
            let frozen: Vec<_> =
                manifest.files.iter().filter(|f| f.kind == SourceKind::Frozen && f.shard == Some(shard)).collect();
            ensure!(
                current.len() == 1
                    && frozen.len() == 1
                    && current[0].paired_current.is_none()
                    && frozen[0].paired_current == Some(current[0].source_id),
                HistoryError::new(ErrorCode::SourceMismatch, "conflicting source lineage")
            );
        }
        let mut identity = manifest.clone();
        for f in &mut identity.files {
            f.path = PathBuf::new();
        }
        identity.files.sort_by_key(|f| f.source_id);
        let manifest_bytes = serde_json::to_vec(&identity)?;
        ensure!(
            manifest_bytes.len() <= 8 * 1024 * 1024,
            HistoryError::new(ErrorCode::InsufficientBudget, "source manifest admission")
        );
        let fingerprint = Sha256::digest(&manifest_bytes).into();
        let source = Self {
            manifest,
            fingerprint,
            manifest_bytes,
            files,
            verified_bytes,
            cell_io: Default::default(),
            active_cell: Default::default(),
        };
        source.check_unchanged()?;
        Ok(source)
    }
    pub fn cell_io(&self) -> CellIo {
        *self.cell_io.borrow()
    }
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }
    pub fn table_name(&self, source_id: i64, index: usize) -> Result<Option<&str>> {
        Ok(self.file(source_id)?.tables.get(index).map(String::as_str))
    }
    pub fn table_names(&self, source_id: i64) -> Result<Vec<String>> {
        let f = self.file(source_id)?;
        let mut q = f
            .conn
            .prepare("select name from sqlite_schema where type='table' and name not like 'sqlite_%' order by name")?;
        let names = q.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        ensure!(
            names.len() <= 4096 && names.iter().all(|s| s.len() <= 256),
            HistoryError::new(ErrorCode::UnsupportedSchema, "table inventory admission")
        );
        Ok(names)
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }
    pub fn kind(&self, id: i64) -> Result<SourceKind> {
        Ok(self.file(id)?.manifest.kind)
    }
    fn file(&self, id: i64) -> Result<&FileSource> {
        self.files.get(&id).context(HistoryError::new(ErrorCode::SourceMismatch, "unregistered source"))
    }
    pub fn check_unchanged(&self) -> Result<()> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        for f in self.files.values() {
            for suffix in ["-wal", "-shm", "-journal"] {
                ensure!(
                    !PathBuf::from(format!("{}{suffix}", f.manifest.path.display())).exists(),
                    HistoryError::new(ErrorCode::SourceMismatch, "captured checkpoint acquired a sidecar")
                );
            }
            let now = std::fs::metadata(&f.manifest.path)?;
            #[cfg(unix)]
            ensure!(
                now.dev() == f.stamp.dev()
                    && now.ino() == f.stamp.ino()
                    && now.len() == f.stamp.len()
                    && now.mtime() == f.stamp.mtime()
                    && now.mtime_nsec() == f.stamp.mtime_nsec()
                    && now.ctime() == f.stamp.ctime()
                    && now.ctime_nsec() == f.stamp.ctime_nsec(),
                HistoryError::new(ErrorCode::SourceMismatch, "captured source changed")
            );
            #[cfg(not(unix))]
            ensure!(
                now.len() == f.stamp.len() && now.modified()? == f.stamp.modified()?,
                HistoryError::new(ErrorCode::SourceMismatch, "captured source changed")
            );
        }
        Ok(())
    }
    pub fn scan(
        &self,
        cursor: &RowCursor,
        budget: ReadBudget,
        mut sink: impl FnMut(&Descriptor) -> Result<bool>,
    ) -> Result<PageEnd> {
        self.check_unchanged()?;
        ensure!(
            budget.rows > 0 && budget.bytes >= DESCRIPTOR_ADMISSION && (1..=MAX_CHUNK).contains(&budget.chunk_bytes),
            HistoryError::new(ErrorCode::InsufficientBudget, "source descriptor admission")
        );
        let f = self.file(cursor.source_id)?;
        let table = table(f.manifest.kind, cursor.relation)?;
        ensure!(
            cursor.relation != Relation::Facts
                || cursor.after.is_none_or(|n| n >= 0 && n < (fact_names(f.manifest.kind).len() as i64)),
            HistoryError::new(ErrorCode::SourceMismatch, "invalid source fact bookmark")
        );
        let cols = match cursor.relation {
            Relation::Releases => f.columns.as_slice(),
            Relation::Collections => ALIASES,
            Relation::Facts => FACTS,
        };
        let mut end = PageEnd { cursor: cursor.clone(), complete: false, rows: 0, bytes: 0 };
        while end.rows < u64::from(budget.rows) && budget.bytes - end.bytes >= DESCRIPTOR_ADMISSION {
            let (key, rowid, query, parameter) = if cursor.relation == Relation::Facts {
                let index = end.cursor.after.map_or(0, |i| i + 1);
                let Some(name) = fact_names(f.manifest.kind).get(index as usize) else {
                    end.complete = true;
                    break;
                };
                (
                    index,
                    0,
                    format!("select {} from meta where key=?1", descriptor_sql(cols)),
                    rusqlite::types::Value::Text((*name).into()),
                )
            } else {
                let keycol = if cursor.relation == Relation::Collections { "key" } else { "id" };
                let mut q = f.conn.prepare_cached(&format!(
                    "select {keycol} from {table} {} order by {keycol} limit 1",
                    if end.cursor.after.is_some() { "where ".to_owned() + keycol + ">?1" } else { String::new() }
                ))?;
                let key = if let Some(after) = end.cursor.after {
                    q.query_row([after], |r| r.get::<_, i64>(0))
                } else {
                    q.query_row([], |r| r.get::<_, i64>(0))
                };
                match key {
                    Ok(k) => (
                        k,
                        k,
                        format!("select {} from {table} where {keycol}=?1", descriptor_sql(cols)),
                        rusqlite::types::Value::Integer(k),
                    ),
                    Err(rusqlite::Error::QueryReturnedNoRows) => {
                        end.complete = true;
                        break;
                    }
                    Err(e) => return Err(e.into()),
                }
            };
            let mut q = f.conn.prepare_cached(&query)?;
            let mut rows = q.query([parameter])?;
            let row = rows.next()?.context(HistoryError::new(ErrorCode::SourceMismatch, "row disappeared"))?;
            let mut desc = Descriptor { key, fields: Vec::with_capacity(cols.len()) };
            let mut bytes = 8u64;
            for (i, col) in cols.iter().enumerate() {
                let ty: String = row.get(i * 2)?;
                bytes += ty.len() as u64;
                let scalar = match ty.as_str() {
                    "null" => Scalar::Null,
                    "integer" => {
                        bytes += 8;
                        Scalar::Integer(row.get(i * 2 + 1)?)
                    }
                    "real" => {
                        bytes += 8;
                        Scalar::RealBits(row.get::<_, f64>(i * 2 + 1)?.to_bits())
                    }
                    "text" | "blob" => {
                        ensure!(
                            cursor.relation != Relation::Collections,
                            HistoryError::new(ErrorCode::UnsupportedSchema, "noninteger alias")
                        );
                        let blob = f.conn.blob_open("main", table, col.name(), rowid, true)?;
                        bytes += 8;
                        Scalar::Cell(CellRef {
                            fingerprint: self.fingerprint,
                            source_id: cursor.source_id,
                            relation: cursor.relation,
                            rowid,
                            column: *col,
                            kind: if ty == "text" { CellType::Text } else { CellType::Blob },
                            bytes: blob.len() as u64,
                        })
                    }
                    _ => bail!(HistoryError::new(ErrorCode::UnsupportedSchema, "SQLite value type")),
                };
                desc.fields.push((*col, scalar));
            }
            ensure!(
                bytes <= DESCRIPTOR_ADMISSION,
                HistoryError::new(ErrorCode::UnsupportedSchema, "descriptor exceeds fixed schema admission")
            );
            end.rows += 1;
            end.bytes += bytes;
            if !sink(&desc)? {
                break;
            }
            end.cursor.after = Some(key);
        }
        Ok(end)
    }
    pub fn read_cell(
        &self,
        cell: &CellRef,
        offset: u64,
        budget: ReadBudget,
        mut sink: impl FnMut(&[u8]) -> Result<()>,
    ) -> Result<CellProgress> {
        self.check_unchanged()?;
        ensure!(
            budget.rows > 0 && budget.bytes >= 32 && budget.chunk_bytes > 0 && budget.chunk_bytes <= MAX_CHUNK,
            HistoryError::new(ErrorCode::InsufficientBudget, "cell chunk admission")
        );
        let f = self.file(cell.source_id)?;
        let table = table(f.manifest.kind, cell.relation)?;
        let allowed = if cell.relation == Relation::Facts { FACTS } else { f.columns.as_slice() };
        ensure!(
            allowed.contains(&cell.column) && cell.relation != Relation::Collections,
            HistoryError::new(ErrorCode::UnsupportedSchema, "unallowlisted cell handle")
        );
        ensure!(
            cell.fingerprint == self.fingerprint,
            HistoryError::new(ErrorCode::SourceMismatch, "cell belongs to another checkpoint")
        );
        {
            let mut active = self.active_cell.borrow_mut();
            if active.as_ref().is_none_or(|open| open.reference != *cell) {
                active.take();
                let table_name = std::ffi::CString::new(table)?;
                let column_name = std::ffi::CString::new(cell.column.name())?;
                let mut handle = std::ptr::null_mut();
                // SAFETY: Source privately owns the stable SQLite connection
                // and this handle. Fixed allowlisted C strings remain live for
                // the call; flag zero opens read-only. active_cell is declared
                // before files, so the handle closes before its connection.
                let code = unsafe {
                    rusqlite::ffi::sqlite3_blob_open(
                        f.conn.handle(),
                        c"main".as_ptr(),
                        table_name.as_ptr(),
                        column_name.as_ptr(),
                        cell.rowid,
                        0,
                        &mut handle,
                    )
                };
                ensure!(
                    code == rusqlite::ffi::SQLITE_OK,
                    HistoryError::new(
                        ErrorCode::SourceMismatch,
                        format!("incremental cell open failed (SQLite {code})")
                    )
                );
                let handle = std::ptr::NonNull::new(handle)
                    .context(HistoryError::new(ErrorCode::SourceMismatch, "null incremental cell handle"))?;
                *active = Some(RetainedCell { handle, reference: cell.clone(), indexed_until: 0 });
                self.cell_io.borrow_mut().cursor_opens += 1;
            }
            let handle = active.as_ref().expect("active read-only cell").handle;
            // SAFETY: this owned cursor is live on the owning connection and
            // no mutation/connection escape is available through Source.
            let length = unsafe { rusqlite::ffi::sqlite3_blob_bytes(handle.as_ptr()) };
            ensure!(
                length >= 0 && length as u64 == cell.bytes && offset <= cell.bytes,
                HistoryError::new(ErrorCode::SourceMismatch, "changed cell handle")
            );
        }
        // Recheck the original type using metadata only; no whole-cell Value.
        let ty: String = f
            .conn
            .prepare_cached(&format!("select typeof({}) from {table} where rowid=?1", cell.column.name()))?
            .query_row([cell.rowid], |r| r.get(0))?;
        ensure!(
            (ty == "text" && cell.kind == CellType::Text) || (ty == "blob" && cell.kind == CellType::Blob),
            HistoryError::new(ErrorCode::SourceMismatch, "cell type")
        );
        let metadata_bytes = 8 + ty.len() as u64;
        let indexed = self.active_cell.borrow().as_ref().expect("active cell").indexed_until;
        let preparing = offset < cell.bytes && indexed < offset;
        let read_offset = if preparing { indexed } else { offset };
        let remaining = if preparing { offset - indexed } else { cell.bytes - offset };
        let n = remaining.min(budget.bytes - metadata_bytes).min(u64::from(budget.chunk_bytes)) as usize;
        if n == 0 {
            return Ok(CellProgress { next: offset, bytes: metadata_bytes });
        }
        let mut bytes = vec![0; n];
        {
            let active = self.active_cell.borrow();
            let handle = active.as_ref().expect("active read-only cell").handle;
            // SAFETY: the live read-only cursor, buffer length and checked
            // SQLite-sized offset stay valid for this synchronous call.
            let code = unsafe {
                rusqlite::ffi::sqlite3_blob_read(
                    handle.as_ptr(),
                    bytes.as_mut_ptr().cast(),
                    n as i32,
                    i32::try_from(read_offset)?,
                )
            };
            ensure!(
                code == rusqlite::ffi::SQLITE_OK,
                HistoryError::new(ErrorCode::SourceMismatch, format!("incremental cell read failed (SQLite {code})"))
            );
        }
        {
            let mut io = self.cell_io.borrow_mut();
            io.read_calls += 1;
            io.bytes += n as u64;
            io.max_request = io.max_request.max(n);
        }
        {
            let mut active = self.active_cell.borrow_mut();
            let cell = active.as_mut().expect("active cell");
            cell.indexed_until = cell.indexed_until.max(read_offset + n as u64);
        }
        if preparing {
            return Ok(CellProgress { next: offset, bytes: metadata_bytes + n as u64 });
        }
        sink(&bytes)?;
        Ok(CellProgress { next: offset + n as u64, bytes: metadata_bytes + n as u64 })
    }
}
fn descriptor_sql(cols: &[Column]) -> String {
    cols.iter()
        .map(|c| format!("typeof({0}),case when typeof({0}) in ('integer','real') then {0} else null end", c.name()))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    pub(crate) fn fixture() -> (tempfile::TempDir, Manifest) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("current.db");
        let c = rusqlite::Connection::open(&p).unwrap();
        c.execute_batch("create table releases(id integer primary key,name text,group_name text,poster text,posted_date text,size integer,complete integer,parts integer,file_total integer,display_name text,is_obfuscated integer,name_source integer); create table collections(key integer primary key,release_id integer not null) without rowid; create table meta(key text primary key,value integer); insert into meta values('id_key0',-7),('id_key1',9),('grouping_from',1)").unwrap();
        drop(c);
        let bytes = std::fs::read(&p).unwrap();
        let mut m = Manifest {
            version: 1,
            checkpoint_id: "synthetic-capture".into(),
            source_uuid: "synthetic-source".into(),
            shard_count: 1,
            reserved_next_id: 900,
            archive_manifest: [3; 32],
            main_receipt: [4; 32],
            ext_receipt: [5; 32],
            grouping_facts: vec![1],
            immutable_checkpoint: true,
            files: vec![FileManifest {
                source_id: 1,
                kind: SourceKind::Current,
                shard: Some(0),
                paired_current: None,
                path: p,
                byte_length: bytes.len() as u64,
                digest: Sha256::digest(&bytes).into(),
                schema_digest: [0; 32],
                catalog_counts: Some(CatalogCounts { releases: 0, collections: 0 }),
            }],
        };
        let c = Connection::open(&m.files[0].path).unwrap();
        m.files[0].schema_digest = schema_digest(&c).unwrap();
        drop(c);
        let main = dir.path().join("main.db");
        let c = Connection::open(&main).unwrap();
        c.execute_batch(
            "create table meta(key text primary key,value integer);insert into meta values('next_seq',900)",
        )
        .unwrap();
        let schema = schema_digest(&c).unwrap();
        drop(c);
        let data = std::fs::read(&main).unwrap();
        m.files.push(FileManifest {
            source_id: 2,
            kind: SourceKind::Main,
            shard: None,
            paired_current: None,
            path: main,
            byte_length: data.len() as u64,
            digest: Sha256::digest(&data).into(),
            schema_digest: schema,
            catalog_counts: None,
        });
        let frozen = dir.path().join("frozen.db");
        let c = Connection::open(&frozen).unwrap();
        c.execute_batch("create table frozen_releases(id integer primary key,name text,group_name text,display_name text,poster text,posted_date text,size integer,parts integer,complete integer,category integer,reason text not null,frozen_at integer not null,payload blob not null);create table frozen_collections(key integer primary key,release_id integer not null) without rowid").unwrap();
        let schema = schema_digest(&c).unwrap();
        drop(c);
        let data = std::fs::read(&frozen).unwrap();
        m.files.push(FileManifest {
            source_id: 3,
            kind: SourceKind::Frozen,
            shard: Some(0),
            paired_current: Some(1),
            path: frozen,
            byte_length: data.len() as u64,
            digest: Sha256::digest(&data).into(),
            schema_digest: schema,
            catalog_counts: Some(CatalogCounts { releases: 0, collections: 0 }),
        });
        // Verification of explicitly declared synthetic sources must succeed;
        // later cases exercise paired files and all bounded cursor operations.
        (dir, m)
    }
    #[test]
    fn rejects_release_tables_without_incremental_rowids() {
        let (_dir, mut m) = fixture();
        let current = m.files.iter().find(|f| f.kind == SourceKind::Current).unwrap();
        let c = Connection::open(&current.path).unwrap();
        let sql: String = c.query_row("select sql from sqlite_schema where name='releases'", [], |r| r.get(0)).unwrap();
        c.execute_batch(&format!("drop table releases;{sql} without rowid")).unwrap();
        drop(c);
        refresh(&mut m);
        assert!(Source::open(m, 1 << 20).is_err(), "release cells require a validated incremental rowid table");
    }

    #[test]
    fn opens_an_explicit_readonly_checkpoint() {
        let (_dir, m) = fixture();
        assert!(Source::open(m, 1 << 20).is_ok());
    }
    #[test]
    fn admission_errors_have_stable_structured_codes() {
        let (_dir, m) = fixture();
        let error = Source::open(m, 0).err().unwrap();
        assert_eq!(
            error.downcast_ref::<crate::history_identity::HistoryError>().map(|e| e.code),
            Some(crate::history_identity::ErrorCode::InsufficientBudget)
        );
    }
    #[test]
    fn a_missing_archive_identity_is_explicitly_rejected() {
        let (_dir, mut m) = fixture();
        m.archive_manifest = [0; 32];
        let error = Source::open(m, 1 << 20).err();
        assert!(error.is_some(), "an empty archive identity is not preservation evidence");
        assert_eq!(
            error.unwrap().downcast_ref::<crate::history_identity::HistoryError>().map(|e| e.code),
            Some(crate::history_identity::ErrorCode::MissingArchive)
        );
    }
    #[test]
    fn schema_verification_has_an_aggregate_byte_admission() {
        let (_dir, m) = fixture();
        let mut admission = VerificationAdmission::new(1 << 20);
        admission.max_schema_bytes = 1;
        assert!(
            Source::open_with_admission(m, admission, &std::sync::atomic::AtomicBool::new(false)).is_err(),
            "schema validation must share a total capture budget"
        );
    }
    #[test]
    fn allocator_admission_rejects_a_release_at_the_reserved_floor() {
        let (_dir, mut m) = fixture();
        let current = m.files.iter().find(|f| f.kind == SourceKind::Current).unwrap();
        let c = Connection::open(&current.path).unwrap();
        c.execute("insert into releases(id) values(900)", []).unwrap();
        drop(c);
        refresh(&mut m);
        assert!(Source::open(m, 1 << 20).is_err(), "native reservation must exceed every retained public ID");
    }
    #[test]
    fn integer_source_facts_do_not_require_a_meta_rowid() {
        let (_dir, mut manifest) = fixture();
        for file in &manifest.files {
            if file.kind == SourceKind::Frozen {
                continue;
            }
            let c = Connection::open(&file.path).unwrap();
            c.execute_batch("alter table meta rename to old_meta;create table meta(key text primary key,value integer) without rowid;insert into meta select * from old_meta;drop table old_meta").unwrap();
        }
        refresh(&mut manifest);
        let source = Source::open(manifest, 1 << 20).unwrap();
        let mut values = Vec::new();
        let end = source
            .scan(
                &RowCursor { source_id: 1, relation: Relation::Facts, after: None },
                ReadBudget { rows: 8, bytes: 4096, chunk_bytes: 64 },
                |row| {
                    values.push(row.key);
                    Ok(true)
                },
            )
            .unwrap();
        assert!(end.complete);
        assert_eq!(values.len(), fact_names(SourceKind::Current).len());
    }
    #[test]
    fn request_admission_rejects_zero_rows_for_cells() {
        let (_dir, mut manifest) = fixture();
        seed_current(&mut manifest, 1);
        let source = Source::open(manifest, 1 << 20).unwrap();
        let mut cell = None;
        source
            .scan(
                &RowCursor { source_id: 1, relation: Relation::Releases, after: None },
                ReadBudget { rows: 1, bytes: 512, chunk_bytes: 64 },
                |row| {
                    if let Scalar::Cell(value) = row.fields.iter().find(|(c, _)| *c == Column::Name).unwrap().1.clone()
                    {
                        cell = Some(value);
                    }
                    Ok(true)
                },
            )
            .unwrap();
        assert!(
            source.read_cell(&cell.unwrap(), 0, ReadBudget { rows: 0, bytes: 64, chunk_bytes: 1 }, |_| Ok(())).is_err(),
            "cell reads require positive explicit row admission"
        );
    }
    #[test]
    fn request_admission_rejects_invalid_fact_bookmarks() {
        let (_dir, manifest) = fixture();
        let source = Source::open(manifest, 1 << 20).unwrap();
        for after in [-1, i64::MAX] {
            assert!(
                source
                    .scan(
                        &RowCursor { source_id: 1, relation: Relation::Facts, after: Some(after) },
                        ReadBudget { rows: 1, bytes: 512, chunk_bytes: 64 },
                        |_| Ok(true)
                    )
                    .is_err(),
                "invalid fact bookmark must fail without skipping or overflowing"
            );
        }
    }
    #[test]
    fn cancelled_initial_verification_reads_nothing() {
        let (_dir, m) = fixture();
        let cancel = std::sync::atomic::AtomicBool::new(true);
        assert!(
            Source::open_with_admission(m, VerificationAdmission::new(1 << 20), &cancel).is_err(),
            "cancelled initial verification must not open/read sources"
        );
    }
    pub(crate) fn refresh(manifest: &mut Manifest) {
        for file in &mut manifest.files {
            let c = Connection::open(&file.path).unwrap();
            file.schema_digest = schema_digest(&c).unwrap();
            if file.kind != SourceKind::Main {
                let release = if file.kind == SourceKind::Current { "releases" } else { "frozen_releases" };
                let aliases = if file.kind == SourceKind::Current { "collections" } else { "frozen_collections" };
                file.catalog_counts = Some(CatalogCounts {
                    releases: c
                        .query_row(&format!("select count(*) from {release}"), [], |r| r.get::<_, i64>(0))
                        .unwrap() as u64,
                    collections: c
                        .query_row(&format!("select count(*) from {aliases}"), [], |r| r.get::<_, i64>(0))
                        .unwrap() as u64,
                });
            }
            drop(c);
            let mut input = std::fs::File::open(&file.path).unwrap();
            file.byte_length = input.metadata().unwrap().len();
            let mut hash = Sha256::new();
            let mut bytes = [0u8; 65536];
            loop {
                let n = input.read(&mut bytes).unwrap();
                if n == 0 {
                    break;
                }
                hash.update(&bytes[..n]);
            }
            file.digest = hash.finalize().into();
        }
    }
    pub(crate) fn seed_current(manifest: &mut Manifest, rows: usize) {
        let path = manifest.files.iter().find(|f| f.kind == SourceKind::Current).unwrap().path.clone();
        let mut c = Connection::open(path).unwrap();
        let tx = c.transaction().unwrap();
        for i in 0..rows {
            let id = 3 + i as i64 * 2;
            tx.execute("insert into releases(id,name,group_name,poster,posted_date,size,complete,parts,file_total,display_name,is_obfuscated,name_source) values(?1,?2,'test.group',?3,'date',1.25,null,7,2,?4,0,40)",rusqlite::params![id,format!("posted-{i}\0é"),vec![0u8,255],if i%2==0{Some(String::new())}else{None}]).unwrap();
        }
        tx.execute_batch("insert into collections values(-9223372036854775808,3),(0,3),(7,899)").unwrap();
        tx.commit().unwrap();
        drop(c);
        refresh(manifest);
    }
    #[test]
    fn signed_keysets_and_incremental_types_preserve_source_hashes() {
        let (_dir, mut m) = fixture();
        seed_current(&mut m, 2);
        let before: Vec<_> = m.files.iter().map(|f| f.digest).collect();
        let source = Source::open(m.clone(), 1 << 20).unwrap();
        let budget = ReadBudget { rows: 1, bytes: 512, chunk_bytes: 1 };
        let mut cursor = RowCursor { source_id: 1, relation: Relation::Collections, after: None };
        let mut keys = Vec::new();
        loop {
            let end = source
                .scan(&cursor, budget, |row| {
                    keys.push(row.key);
                    Ok(true)
                })
                .unwrap();
            assert!(end.rows <= 1 && end.bytes <= 512);
            cursor = serde_json::from_slice(&serde_json::to_vec(&end.cursor).unwrap()).unwrap();
            if end.complete {
                break;
            }
        }
        assert_eq!(keys, [i64::MIN, 0, 7]);
        let mut rows = Vec::new();
        let cursor = RowCursor { source_id: 1, relation: Relation::Releases, after: None };
        source
            .scan(&cursor, ReadBudget { rows: 2, bytes: 1024, ..budget }, |row| {
                rows.push(row.clone());
                Ok(true)
            })
            .unwrap();
        assert_eq!(rows.iter().map(|r| r.key).collect::<Vec<_>>(), [3, 5]);
        assert_eq!(
            rows[0].fields.iter().find(|(c, _)| *c == Column::Size).unwrap().1,
            Scalar::RealBits(1.25f64.to_bits())
        );
        assert_eq!(rows[0].fields.iter().find(|(c, _)| *c == Column::Complete).unwrap().1, Scalar::Null);
        let cell = match &rows[0].fields.iter().find(|(c, _)| *c == Column::Name).unwrap().1 {
            Scalar::Cell(c) => c,
            _ => panic!("text handle"),
        };
        let mut offset = 0;
        let mut bytes = Vec::new();
        while offset < cell.bytes {
            let end = source
                .read_cell(cell, offset, ReadBudget { rows: 1, bytes: 32, chunk_bytes: 1 }, |b| {
                    bytes.extend_from_slice(b);
                    Ok(())
                })
                .unwrap();
            assert!(end.bytes <= 32);
            offset = end.next;
        }
        assert_eq!(bytes, b"posted-0\0\xc3\xa9");
        drop(source);
        refresh(&mut m);
        assert_eq!(before, m.files.iter().map(|f| f.digest).collect::<Vec<_>>());
    }
    pub(crate) fn seed_frozen(manifest: &mut Manifest, corrupt: bool) {
        seed_frozen_count(manifest, corrupt, 2, 0)
    }
    pub(crate) fn seed_frozen_count(manifest: &mut Manifest, corrupt: bool, count: usize, name_bytes: usize) {
        use crate::frozen::payload::{Payload, Table};
        use rusqlite::types::Value;
        let path = manifest.files.iter().find(|f| f.kind == SourceKind::Frozen).unwrap().path.clone();
        let c = Connection::open(path).unwrap();
        for index in 0..count {
            let id = match index {
                0 => 3,
                1 => 41,
                _ => 100 + index as i64,
            };
            let payload = Payload {
                domains: vec![(1, "example.test".into())],
                tables: vec![
                    Table { name: "empty".into(), columns: vec![], rows: vec![vec![]; 7] },
                    Table {
                        name: "releases".into(),
                        columns: vec![
                            "id".into(),
                            "name".into(),
                            "group_name".into(),
                            "display_name".into(),
                            "name_source".into(),
                            "size".into(),
                        ],
                        rows: vec![vec![
                            Value::Integer(id),
                            Value::Text(format!("embedded-{id}{}", "x".repeat(name_bytes))),
                            Value::Text("test.group".into()),
                            Value::Text(String::new()),
                            Value::Integer(40),
                            Value::Real(f64::from_bits(0x8000000000000000)),
                        ]],
                    },
                ],
            };
            let mut packed = crate::frozen::payload::encode(&payload).unwrap();
            if corrupt && id == 41 {
                packed.push(0);
            }
            c.execute("insert into frozen_releases(id,name,group_name,display_name,poster,posted_date,size,parts,complete,category,reason,frozen_at,payload) values(?1,'header','test.group','header-display',null,'date',0,1,1,2,'synthetic',1,?2)",rusqlite::params![id,packed]).unwrap();
        }
        c.execute_batch("insert into frozen_collections values(-9,899),(0,41),(7,3)").unwrap();
        drop(c);
        refresh(manifest);
    }
    #[test]
    fn one_cell_retains_its_read_cursor_across_forward_pages() {
        let (_dir, mut manifest) = fixture();
        seed_current(&mut manifest, 1);
        let source = Source::open(manifest, 1 << 20).unwrap();
        let cursor = RowCursor { source_id: 1, relation: Relation::Releases, after: None };
        let mut cell = None;
        source
            .scan(&cursor, ReadBudget { rows: 1, bytes: 512, chunk_bytes: 1 }, |row| {
                cell = match row.fields.iter().find(|(c, _)| *c == Column::Name).unwrap().1.clone() {
                    Scalar::Cell(cell) => Some(cell),
                    _ => None,
                };
                Ok(true)
            })
            .unwrap();
        let cell = cell.unwrap();
        let mut next = 0;
        while next < cell.bytes {
            next = source
                .read_cell(&cell, next, ReadBudget { rows: 1, bytes: 32, chunk_bytes: 1 }, |_| Ok(()))
                .unwrap()
                .next;
        }
        let io = source.cell_io();
        assert_eq!(io.bytes, cell.bytes);
        assert_eq!(io.max_request, 1);
        assert_eq!(io.cursor_opens, 1, "ordinary cell pages must retain their overflow-page cursor");
    }
    #[test]
    fn wal_header_is_rejected_before_sqlite_can_create_sidecars() {
        let (_dir, mut manifest) = fixture();
        let file = &mut manifest.files[0];
        let mut bytes = std::fs::read(&file.path).unwrap();
        bytes[18] = 2;
        bytes[19] = 2;
        std::fs::write(&file.path, &bytes).unwrap();
        file.digest = Sha256::digest(&bytes).into();
        let result = Source::open(manifest.clone(), 1 << 20);
        assert!(result.is_err(), "WAL-mode checkpoints must be consolidated before conversion");
        for suffix in ["-wal", "-shm", "-journal"] {
            assert!(
                !PathBuf::from(format!("{}{suffix}", manifest.files[0].path.display())).exists(),
                "read-only verification may not create source sidecars"
            );
        }
    }
    #[test]
    fn cell_handles_cannot_cross_checkpoint_fingerprints() {
        let (_a, mut first) = fixture();
        seed_current(&mut first, 1);
        let source = Source::open(first, 1 << 20).unwrap();
        let mut cell = None;
        source
            .scan(
                &RowCursor { source_id: 1, relation: Relation::Releases, after: None },
                ReadBudget { rows: 1, bytes: 512, chunk_bytes: 32 },
                |row| {
                    cell = match row.fields.iter().find(|(c, _)| *c == Column::Name).unwrap().1.clone() {
                        Scalar::Cell(c) => Some(c),
                        _ => None,
                    };
                    Ok(true)
                },
            )
            .unwrap();
        let (_b, mut second) = fixture();
        seed_current(&mut second, 1);
        let path = second.files[0].path.clone();
        let c = Connection::open(path).unwrap();
        c.execute("update releases set name=replace(name,'posted','POSTED')", []).unwrap();
        drop(c);
        refresh(&mut second);
        let other = Source::open(second, 1 << 20).unwrap();
        assert_ne!(source.fingerprint(), other.fingerprint());
        assert!(
            other.read_cell(&cell.unwrap(), 0, ReadBudget { rows: 1, bytes: 64, chunk_bytes: 32 }, |_| Ok(())).is_err(),
            "cell reference must be scoped to its original fingerprint"
        );
    }
    #[test]
    fn resumed_cell_seek_warms_its_prefix_within_the_read_budget() {
        let (_dir, mut manifest) = fixture();
        seed_current(&mut manifest, 1);
        let source = Source::open(manifest, 1 << 20).unwrap();
        let mut cell = None;
        source
            .scan(
                &RowCursor { source_id: 1, relation: Relation::Releases, after: None },
                ReadBudget { rows: 1, bytes: 512, chunk_bytes: 1 },
                |row| {
                    cell = match row.fields.iter().find(|(c, _)| *c == Column::Name).unwrap().1.clone() {
                        Scalar::Cell(c) => Some(c),
                        _ => None,
                    };
                    Ok(true)
                },
            )
            .unwrap();
        let cell = cell.unwrap();
        let mut chunks = 0;
        let end = source
            .read_cell(&cell, 8, ReadBudget { rows: 1, bytes: 32, chunk_bytes: 1 }, |_| {
                chunks += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(end.next, 8, "a fresh cursor must admit its seek-prefix work first");
        assert_eq!(chunks, 0);
        assert!(end.bytes <= 32);
        assert_eq!(source.cell_io().bytes, 1);
    }
}
