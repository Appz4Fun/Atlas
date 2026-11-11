//! Fenced, resumable conversion of captured historical catalog identities.
//! Publication ends at identity_ready. No public reader/native overlay is enabled.
use super::{
    StorageConfig,
    history_digest::DigestState,
    history_stage::{self, Row},
};
use crate::history_identity::{Column, Metadata, Value};
use crate::history_identity::{ErrorCode, HistoryError};
use crate::history_source::{CellRef, Descriptor, ReadBudget, Relation, RowCursor, Scalar, Source, SourceKind};
use anyhow::{Context, Result, bail, ensure};
use postgres::types::Type;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub use crate::history_identity::ImportId;
const FIXED_COPY: u64 = 131072;
const INLINE: u64 = 512;
const MAX_METADATA: usize = 49152;
// Binary DataRow envelopes: extent values105 + framing39; payload IDs16 +
// framing19. Conservative margins include these alongside their exact bytes.
const VALIDATION_EXTENT_BYTES: u64 = 160;
const VALIDATION_PAYLOAD_OVERHEAD: u64 = 48;
const MAX_RECORD: usize = 2 * MAX_METADATA + 4096;
// SQL CASE bounds payloads before PostgreSQL sends bytea to the client. Keep
// these literal ceilings in sync with MAX_METADATA and MAX_RECORD.
const CHECKPOINT_SQL: &str = "case when octet_length(checkpoint)<=49152 then checkpoint end";
const RECORD_SQL: &str = "case when octet_length(bookmark)<=102400 then bookmark end";
fn checkpoint_payload(row: &postgres::Row, column: usize) -> Result<Vec<u8>> {
    row.get::<_, Option<Vec<u8>>>(column)
        .context(HistoryError::new(ErrorCode::CorruptObject, "checkpoint payload admission"))
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedTransport {
    PlaintextTcp,
    UnixSocket,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedDestination {
    pub database: String,
    pub schema: String,
    pub role: String,
    pub owner: String,
    pub server_version: i32,
    pub server_address: Option<String>,
    pub server_port: Option<i32>,
    pub transport: ExpectedTransport,
    pub staging_prefix: String,
}
#[derive(Clone, Copy)]
pub struct ImportBudget {
    pub source_rows: u32,
    pub source_bytes: u64,
    pub mutations: u32,
    pub copy_bytes: u64,
    pub chunk_bytes: u32,
}
impl Default for ImportBudget {
    fn default() -> Self {
        Self {
            source_rows: 512,
            source_bytes: 4 * 1024 * 1024,
            mutations: 4096,
            copy_bytes: 4 * 1024 * 1024,
            chunk_bytes: 65536,
        }
    }
}
impl ImportBudget {
    pub(crate) fn check(self) -> Result<()> {
        ensure!(
            self.source_rows > 0
                && self.source_bytes >= 512
                && self.mutations >= 16
                && self.copy_bytes >= FIXED_COPY + MAX_METADATA as u64 + 8192
                && (1..=65536).contains(&self.chunk_bytes),
            HistoryError::new(
                ErrorCode::InsufficientBudget,
                "positive page limits and fixed protocol admission required"
            )
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StepReceipt {
    pub sequence: i64,
    pub source_rows: u64,
    pub source_bytes: u64,
    pub decoded_bytes: u64,
    pub mutations: u64,
    pub copy_bytes: u64,
    pub admitted_copy_bytes: u64,
    pub callback_events: u64,
    pub sql_round_trips: u64,
    pub complete: bool,
}
#[derive(Clone, Serialize, Deserialize, Default)]
struct State {
    phase: u8,
    #[serde(default)]
    paired_main: Vec<super::history_companion::RelationReceipt>,
    manifest_offset: u64,
    manifest_ordinal: i64,
    manifest_digest: DigestState,
    register_file: usize,
    register_table: usize,
    register_started: bool,
    file: usize,
    relation: u8,
    after: Option<i64>,
    relation_rows: u64,
    relation_digest: DigestState,
    facts: Vec<i64>,
    pending: Option<Pending>,
    next_object: i64,
    validate_object: i64,
    validate_ordinal: i64,
    validate_offset: u64,
    validate_chunk_offset: u64,
    validate_digest: DigestState,
    cleanup: Option<(i64, i64)>,
    #[serde(default)]
    cleanups: Vec<(i64, i64)>,
    catalog_after: Option<i64>,
    identity_digest: DigestState,
    validate_part: u8,
    validate_active: bool,
    validate_length: u64,
    validate_expected: Vec<u8>,
    validate_chunk_digest: DigestState,
    alias_after: Option<(i64, i64)>,
    receipt_after: Option<(i64, i32)>,
    canonical_rows: u64,
    variant_rows: u64,
    #[serde(default)]
    validated_objects: u64,
    #[serde(default)]
    validated_variant_rows: u64,
    #[serde(default)]
    variant_after: Option<(i64, i64)>,
    dangling_aliases: u64,
    facts_ending: Option<Artifact>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    row: Descriptor,
    metadata: i64,
    fields: Vec<(Column, Value)>,
    cells: Vec<Cell>,
    cell: usize,
    output: Artifact,
    #[serde(default)]
    frozen: Option<Frozen>,
}
#[derive(Clone, Serialize, Deserialize, Default)]
struct Frozen {
    phase: u8,
    projection: Option<super::history_validator::Projection>,
    projection_data: Artifact,
    index: Artifact,
    rows: u64,
    values: u64,
}
struct CompletedFrame {
    source: i64,
    key: i64,
    fingerprint: [u8; 32],
    bytes: u64,
}
struct Runtime {
    _root_lock: std::fs::File,
    root: PathBuf,
    quota: u64,
    decoder: crate::history_stream::Config,
    spool: Option<super::history_spool::Spool>,
    validator: Option<super::history_validator::Validator>,
    completed: Vec<CompletedFrame>,
    scanned: bool,
    abandoned: Vec<(i64, i64)>,
}
impl Runtime {
    fn ensure_spool(&mut self, source: &Source, cell: &Cell, key: i64, validated: bool) -> Result<()> {
        if self.spool.is_none() {
            self.spool = Some(super::history_spool::Spool::open(
                &self.root,
                source.fingerprint(),
                cell.reference.source_id,
                key,
                self.quota - self.completed.iter().map(|f| f.bytes).sum::<u64>(),
                cell.offset,
                cell.reference.bytes,
                validated,
            )?);
        }
        Ok(())
    }
    fn recover(&mut self, source: &Source, cell: &Cell, key: i64, batch: &mut Batch) -> Result<bool> {
        self.ensure_spool(source, cell, key, false)?;
        let spool = self.spool.as_mut().expect("owned spool");
        if spool.input_ready {
            return Ok(true);
        }
        while spool.written < cell.offset && batch.source_left() >= 32 {
            let n = (cell.offset - spool.written).min(u64::from(batch.budget.chunk_bytes)) as u32;
            let offset = spool.written;
            let end = source.read_cell(
                &cell.reference,
                offset,
                ReadBudget { rows: 1, bytes: batch.source_left(), chunk_bytes: n },
                |bytes| spool.append(offset, bytes),
            )?;
            batch.source_bytes += end.bytes;
        }
        if spool.written < cell.offset {
            return Ok(false);
        }
        ensure!(
            spool.prefix_digest.finish()? == cell.digest.finish()?,
            HistoryError::new(ErrorCode::CorruptObject, "recovered source prefix differs")
        );
        if cell.offset == cell.reference.bytes {
            spool.finish_input(cell.offset, cell.digest.finish()?)?;
        }
        Ok(true)
    }
    fn sync_page(&mut self) -> Result<()> {
        if let Some(spool) = &mut self.spool {
            spool.sync_page()?;
        }
        Ok(())
    }
    fn scan_recovery(&mut self, source: &Source, state: &State) -> Result<()> {
        if self.scanned {
            return Ok(());
        }
        let active = state.pending.as_ref().and_then(|p| {
            p.cells.iter().find(|c| c.reference.column == Column::Payload).map(|c| (c.reference.source_id, p.row.key))
        });
        // State was authenticated against the durable checkpoint before this
        // inventory. Only its completed cleanup locators authorize a missing
        // identity in an empty deterministic frame directory.
        let mut authenticated = state.cleanups.clone();
        if let Some(old) = state.cleanup {
            authenticated.push(old);
        }
        let entries = super::history_spool::inventory(&self.root, source.fingerprint(), self.quota, &authenticated)?;
        for entry in entries {
            ensure!(
                entry.charged_bytes <= self.quota,
                HistoryError::new(ErrorCode::InsufficientBudget, "recovery frame quota")
            );
            if Some((entry.source, entry.key)) != active
                && !state.cleanups.contains(&(entry.source, entry.key))
                && state.cleanup != Some((entry.source, entry.key))
            {
                self.abandoned.push((entry.source, entry.key));
            }
        }
        self.scanned = true;
        Ok(())
    }
    fn complete(&mut self, source: &Source, key: i64, source_id: i64) -> Result<()> {
        let bytes = self.spool.take().context("missing completed frame spool")?.retire()?;
        self.completed.push(CompletedFrame { source: source_id, key, fingerprint: source.fingerprint(), bytes });
        Ok(())
    }
    fn committed(&mut self) -> Result<()> {
        for frame in self.completed.drain(..) {
            super::history_spool::Spool::cleanup(
                &self.root,
                frame.fingerprint,
                frame.source,
                frame.key,
                super::history_spool::CleanupAuthority::CommittedCheckpoint,
            )?;
        }
        Ok(())
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.validator.take();
    }
}
#[derive(Clone, Serialize, Deserialize, Default)]
struct Artifact {
    id: i64,
    offset: u64,
    ordinal: i64,
    digest: DigestState,
    started: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct Cell {
    reference: CellRef,
    object: Option<i64>,
    offset: u64,
    ordinal: i64,
    digest: DigestState,
    inline: Vec<u8>,
    started: bool,
}
struct Batch {
    rows: Vec<Row>,
    source_rows: u64,
    source_bytes: u64,
    decoded_bytes: u64,
    events: u64,
    copy_bytes: u64,
    mutations: u32,
    budget: ImportBudget,
}
impl Batch {
    fn new(budget: ImportBudget) -> Self {
        Self {
            rows: Vec::new(),
            source_rows: 0,
            source_bytes: 0,
            decoded_bytes: 0,
            events: 0,
            copy_bytes: FIXED_COPY,
            mutations: 2,
            budget,
        }
    }
    fn room(&self, mutations: u32, bytes: u64) -> bool {
        self.mutations + mutations <= self.budget.mutations && self.copy_bytes + bytes <= self.budget.copy_bytes
    }
    // Leave the caller's finalization reserve after staging the payload and
    // its row/digest overhead. Exhaustion ends this page without subtraction
    // underflow or relying on a later push failure.
    fn chunk_payload_limit(&self, reserve: u64) -> usize {
        if self.mutations.saturating_add(13) > self.budget.mutations {
            return 0;
        }
        self.budget
            .copy_bytes
            .checked_sub(self.copy_bytes)
            .and_then(|left| left.checked_sub(reserve))
            .and_then(|left| left.checked_sub(history_stage::ROW_OVERHEAD + 32))
            .unwrap_or(0)
            .min(u64::from(self.budget.chunk_bytes)) as usize
    }
    fn push(&mut self, row: Row) -> Result<()> {
        ensure!(
            self.room(row.mutations(), row.bytes()),
            HistoryError::new(ErrorCode::InsufficientBudget, "mutation/COPY admission")
        );
        self.mutations += row.mutations();
        self.copy_bytes += row.bytes();
        self.rows.push(row);
        Ok(())
    }
    fn source_left(&self) -> u64 {
        self.budget.source_bytes - self.source_bytes - self.decoded_bytes
    }
}
pub struct ImportOwner {
    client: postgres::Client,
    expected: ExpectedDestination,
    terminal: bool,
    runtime: Runtime,
    stage_cache: history_stage::Cache,
}
impl ImportOwner {
    pub fn connect(storage: StorageConfig, expected: ExpectedDestination, spool: PathBuf, quota: u64) -> Result<Self> {
        storage.check()?;
        ensure!(
            storage.is_postgres() && storage.schema == expected.schema,
            HistoryError::new(ErrorCode::DestinationMismatch, "PostgreSQL schema")
        );
        ensure!(
            super::valid_schema(&expected.schema)
                && !expected.staging_prefix.is_empty()
                && expected.schema.starts_with(&expected.staging_prefix)
                && expected.schema.len() > expected.staging_prefix.len(),
            HistoryError::new(ErrorCode::DestinationMismatch, "explicit staging prefix")
        );
        ensure!(
            quota > 0 && spool.is_absolute() && spool.is_dir(),
            HistoryError::new(ErrorCode::InsufficientBudget, "explicit private spool directory and quota")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                std::fs::symlink_metadata(&spool)?.is_dir()
                    && std::fs::metadata(&spool)?.permissions().mode() & 0o077 == 0,
                HistoryError::new(ErrorCode::DestinationMismatch, "spool directory must be private")
            );
        }
        let config = super::Pool::new(&storage)?.config.clone();
        ensure!(
            config.get_dbname() == Some(expected.database.as_str()),
            HistoryError::new(ErrorCode::DestinationMismatch, "explicit database name")
        );
        let mut client = config.connect(postgres::NoTls)?;
        verify_destination(&mut client, &expected)?;
        // The same session remains the only importer connection until dropped.
        ensure!(
            client
                .query_one("select pg_try_advisory_lock(hashtext('atlas.indexer'),hashtext($1))", &[&expected.schema])?
                .get::<_, bool>(0),
            HistoryError::new(ErrorCode::LostOwner, "another owner holds the destination")
        );
        client.batch_execute(&format!("set search_path to \"{}\"", expected.schema))?;
        verify_destination(&mut client, &expected)?;
        ensure!(
            quota >= super::history_spool::FRAME_OVERHEAD,
            HistoryError::new(ErrorCode::InsufficientBudget, "fixed spool metadata admission")
        );
        let scope = Sha256::digest(serde_json::to_vec(&(
            &expected.database,
            &expected.schema,
            &expected.role,
            &expected.owner,
            &expected.server_address,
            &expected.server_port,
            &expected.transport,
        ))?)
        .into();
        let root_lock = super::history_spool::claim_root(&spool, scope)?;
        Ok(Self {
            client,
            expected,
            terminal: false,
            runtime: Runtime {
                _root_lock: root_lock,
                root: spool,
                quota,
                decoder: Default::default(),
                spool: None,
                validator: None,
                completed: Vec::new(),
                scanned: false,
                abandoned: Vec::new(),
            },
            stage_cache: Default::default(),
        })
    }
    /// Admit the decoder window independently of per-page byte grants. Caller
    /// process admission must additionally cover source caches and COPY buffers.
    pub fn configure_decoder(&mut self, config: crate::history_stream::Config, max_window_bytes: u64) -> Result<()> {
        self.live()?;
        ensure!(
            self.runtime.validator.is_none()
                && (10..=31).contains(&config.window_log_max)
                && (1..=1048576).contains(&config.read_buffer_bytes)
                && (1..=1048576).contains(&config.chunk_bytes)
                && config.max_expanded_bytes > 0
                && max_window_bytes > 0
                && (1u64 << config.window_log_max) <= max_window_bytes,
            HistoryError::new(ErrorCode::InsufficientBudget, "explicit decoder window and buffers admission")
        );
        self.runtime.decoder = config;
        Ok(())
    }
    pub fn status(&mut self, id: ImportId) -> Result<(String, i64)> {
        self.live()?;
        let row = self
            .client
            .query_one("select state,checkpoint_sequence from history_imports where import_id=$1", &[&&id[..]])?;
        Ok((row.get(0), row.get(1)))
    }
    fn live(&self) -> Result<()> {
        ensure!(
            !self.terminal && !self.client.is_closed(),
            HistoryError::new(ErrorCode::LostOwner, "historical import ownership session ended")
        );
        Ok(())
    }
    pub fn begin(&mut self, source: &Source) -> Result<ImportId> {
        self.live()?;
        source.check_unchanged()?;
        verify_destination(&mut self.client, &self.expected)?;
        let id = source.fingerprint();
        let mut tx = self.client.transaction()?;
        tx.batch_execute("set local synchronous_commit=on")?;
        let has_history: bool = tx.query_one("select to_regclass('history_imports') is not null", &[])?.get(0);
        if has_history && let Some(row)=tx.query_opt("select import_id,source_fingerprint,expected_database,expected_schema,expected_role,expected_owner,reserved_next_seq from history_imports for update",&[])? {
                ensure!(row.get::<_,Vec<u8>>(0)==id&&row.get::<_,Vec<u8>>(1)==id&&row.get::<_,String>(2)==self.expected.database&&row.get::<_,String>(3)==self.expected.schema&&row.get::<_,String>(4)==self.expected.role&&row.get::<_,String>(5)==self.expected.owner&&row.get::<_,i64>(6)==source.manifest().reserved_next_id,HistoryError::new(ErrorCode::SourceMismatch, "destination belongs to a different import"));
                reserve(&mut tx,&self.expected.schema,source.manifest().reserved_next_id,false)?;
                tx.commit()?;return Ok(id);
        }
        // Existing owner receipts are issued in the original main/ext copy
        // transaction. Statement guards invalidate them on later native DML;
        // this session already holds the exclusive registration fence.
        let main = source
            .manifest()
            .files
            .iter()
            .find(|f| f.kind == SourceKind::Main)
            .context(HistoryError::new(ErrorCode::SourceMismatch, "main source"))?;
        let main_receipt = if has_history {
            super::history_companion::paired(&mut tx, "main_import", source.manifest().main_receipt, main.digest)?
        } else {
            None
        };
        let ext_receipt = if has_history {
            super::history_companion::paired(&mut tx, "ext", source.manifest().ext_receipt, main.digest)?
        } else {
            None
        };
        for (receipt, owns, scope) in [
            (main_receipt.as_ref(), super::main_import::owns_table as fn(&str) -> bool, "main_compat_v1"),
            (ext_receipt.as_ref(), super::ext::owns_table as fn(&str) -> bool, "ext_normalized_and_keep_v1"),
        ] {
            if let Some(receipt) = receipt {
                ensure!(
                    receipt.scope == scope
                        && receipt
                            .relations
                            .iter()
                            .enumerate()
                            .all(|(i, r)| owns(&r.table)
                                && !receipt.relations[..i].iter().any(|old| old.table == r.table)),
                    HistoryError::new(ErrorCode::SourceMismatch, "companion receipt relation inventory")
                );
            }
        }
        let tables = tx.query(
            "select tablename::text from pg_tables where schemaname=$1 order by tablename",
            &[&self.expected.schema],
        )?;
        ensure!(tables.len() <= 256, HistoryError::new(ErrorCode::DestinationMismatch, "unexpected staging schema"));
        for row in &tables {
            let table: String = row.get(0);
            if table == "schema_migrations" || table == "history_companion_receipts" {
                continue;
            }
            if main_receipt
                .iter()
                .chain(ext_receipt.iter())
                .any(|r| r.relations.iter().any(|relation| relation.table == table))
            {
                continue;
            }
            ensure!(
                super::valid_schema(&table),
                HistoryError::new(ErrorCode::DestinationMismatch, "unexpected relation name")
            );
            let predicate = if table == "meta" { " where key<>'next_seq' or value<>1" } else { "" };
            let nonempty: bool =
                tx.query_one(&format!("select exists(select 1 from \"{table}\"{predicate})"), &[])?.get(0);
            ensure!(
                !nonempty,
                HistoryError::new(
                    ErrorCode::DestinationMismatch,
                    "populated target requires an exact paired import receipt"
                )
            );
        }
        let versions =
            tx.query("select to_regclass('schema_migrations') is not null", &[])?.remove(0).get::<_, bool>(0);
        if versions {
            let done: Vec<(i32, String)> = tx
                .query("select version,name from schema_migrations order by version", &[])?
                .iter()
                .map(|r| (r.get(0), r.get(1)))
                .collect();
            let expected: Vec<_> = super::migrate::MIGRATIONS.iter().map(|(v, n, _)| (*v, n.to_string())).collect();
            ensure!(
                done == expected,
                HistoryError::new(ErrorCode::DestinationMismatch, "exact migration history required")
            );
        } else {
            super::migrate::run_in_transaction(&mut tx, &self.expected.schema)?;
        }
        reserve(&mut tx, &self.expected.schema, source.manifest().reserved_next_id, true)?;
        tx.execute(
            "update meta set value=greatest(value,$1) where key='next_seq'",
            &[&source.manifest().reserved_next_id],
        )?;
        if main_receipt.is_some() {
            // Only this deliberate allocator derivation may clear the dirty
            // flag set by the meta trigger. The receipt was clean and paired
            // above while the exclusive owner fence excluded native writers.
            ensure!(tx.execute("update history_companion_receipts set dirty=false,derived_allocator=$1 where owner_code='main_import' and receipt_digest=$2", &[&source.manifest().reserved_next_id, &&source.manifest().main_receipt[..]])? == 1, HistoryError::new(ErrorCode::SourceMismatch, "companion allocator derivation"));
        }
        let mut state = State {
            next_object: 2,
            paired_main: main_receipt.map_or_else(Vec::new, |receipt| receipt.relations),
            ..Default::default()
        };
        state.identity_digest.update(&id)?;
        let checkpoint = serde_json::to_vec(&state)?;
        tx.execute("insert into history_imports(import_id,source_fingerprint,contract_version,state,expected_database,expected_schema,expected_role,expected_owner,reserved_next_seq,checkpoint,manifest_object_id) values($1,$1,1,'loading',$2,$3,$4,$5,$6,$7,1)",&[&&id[..],&self.expected.database,&self.expected.schema,&self.expected.role,&self.expected.owner,&source.manifest().reserved_next_id,&checkpoint])?;
        let rows = vec![
            Row {
                tag: 7,
                a: 0,
                b: -1,
                c: -1,
                text: "manifest".into(),
                x: id.to_vec(),
                y: Sha256::digest(b"atlas-catalog-manifest-v1").to_vec(),
                ..Default::default()
            },
            object(1, 0, 0, "manifest", b"manifest", source.manifest_bytes().len() as u64),
        ];
        history_stage::apply(&mut tx, &id, &rows, &mut self.stage_cache)?;
        tx.commit()?;
        Ok(id)
    }
    pub fn step(
        &mut self,
        source: &Source,
        id: ImportId,
        previous_sequence: i64,
        budget: ImportBudget,
    ) -> Result<StepReceipt> {
        let result = self.step_inner(source, id, previous_sequence, budget);
        if result.is_err() {
            self.terminal = true;
            self.runtime.validator.take();
            if self.runtime.spool.as_ref().is_some_and(|spool| !spool.keep) {
                self.runtime.spool.take();
            }
        }
        result
    }
    fn step_inner(
        &mut self,
        source: &Source,
        id: ImportId,
        previous_sequence: i64,
        budget: ImportBudget,
    ) -> Result<StepReceipt> {
        self.live()?;
        budget.check()?;
        source.check_unchanged()?;
        ensure!(source.fingerprint() == id, HistoryError::new(ErrorCode::SourceMismatch, "import fingerprint"));
        let mut tx = self.client.transaction()?;
        tx.batch_execute("set local synchronous_commit=on")?;
        let row=tx.query_typed_one(&format!("select source_fingerprint,state,checkpoint_sequence,{CHECKPOINT_SQL} from history_imports where import_id=$1 for update"),&[(&&id[..],Type::BYTEA)])?;
        ensure!(
            row.get::<_, Vec<u8>>(0) == id && row.get::<_, String>(1) == "loading",
            HistoryError::new(ErrorCode::IncompleteImport, "import is not loading")
        );
        let sequence: i64 = row.get(2);
        let checkpoint = checkpoint_payload(&row, 3)?;
        if sequence == previous_sequence + 1 {
            let old = tx.query_typed_one(
                &format!("select {RECORD_SQL},digest from history_checkpoints where import_id=$1 and sequence=$2"),
                &[(&&id[..], Type::BYTEA), (&sequence, Type::INT8)],
            )?;
            let bytes = checkpoint_payload(&old, 0)?;
            let digest: Vec<u8> = old.get(1);
            ensure!(
                Sha256::digest(&bytes).as_slice() == digest,
                HistoryError::new(ErrorCode::CorruptObject, "ambiguous checkpoint digest")
            );
            let record: Checkpoint = serde_json::from_slice(&bytes)?;
            ensure!(
                record.receipt.sequence == sequence && record.state.as_bytes() == checkpoint,
                HistoryError::new(ErrorCode::SourceMismatch, "ambiguous checkpoint content")
            );
            tx.commit()?;
            return Ok(record.receipt);
        }
        ensure!(
            sequence == previous_sequence,
            HistoryError::new(ErrorCode::SourceMismatch, "checkpoint sequence conflict")
        );
        if sequence > 0 {
            let old = tx.query_typed_one(
                &format!("select {RECORD_SQL},digest from history_checkpoints where import_id=$1 and sequence=$2"),
                &[(&&id[..], Type::BYTEA), (&sequence, Type::INT8)],
            )?;
            let bytes = checkpoint_payload(&old, 0)?;
            let digest: Vec<u8> = old.get(1);
            ensure!(
                Sha256::digest(&bytes).as_slice() == digest,
                HistoryError::new(ErrorCode::CorruptObject, "durable checkpoint digest")
            );
            let old: Checkpoint = serde_json::from_slice(&bytes)?;
            ensure!(
                old.state.as_bytes() == checkpoint,
                HistoryError::new(ErrorCode::CorruptObject, "durable bookmark mismatch")
            );
        }
        let mut state: State = serde_json::from_slice(&checkpoint)?;
        let mut batch = Batch::new(budget);
        self.runtime.scan_recovery(source, &state)?;
        consume(source, &mut state, &mut batch, &mut self.runtime)?;
        // Validate only previously committed capture pages. Starting against
        // this transaction's not-yet-COPYed final capture batch would skip its
        // chunks when the validator reaches the current database EOF.
        let validation_trips =
            if state.phase == 3 && batch.rows.is_empty() && batch.source_rows == 0 && batch.source_bytes == 0 {
                validate_page(&mut tx, &id, &mut state, &mut batch)?
            } else {
                0
            };
        self.runtime.sync_page()?;
        let applied = history_stage::apply(&mut tx, &id, &batch.rows, &mut self.stage_cache)?;
        let next = sequence.checked_add(1).context("checkpoint sequence overflow")?;
        let checkpoint = serde_json::to_vec(&state)?;
        ensure!(
            checkpoint.len() <= MAX_METADATA,
            HistoryError::new(ErrorCode::InsufficientBudget, "fixed checkpoint admission")
        );
        let mut receipt = StepReceipt {
            sequence: next,
            source_rows: batch.source_rows,
            source_bytes: batch.source_bytes,
            decoded_bytes: batch.decoded_bytes,
            mutations: applied.mutations + 2,
            copy_bytes: 0,
            admitted_copy_bytes: batch.copy_bytes,
            callback_events: batch.events,
            sql_round_trips: applied.round_trips + validation_trips + if sequence > 0 { 7 } else { 6 },
            complete: state.phase == 4,
        };
        ensure!(
            receipt.mutations <= u64::from(batch.mutations),
            HistoryError::new(ErrorCode::CorruptObject, "mutation accounting exceeded reservation")
        );
        let checkpoint_text = String::from_utf8(checkpoint.clone())?;
        let mut record;
        for _ in 0..4 {
            record = serde_json::to_vec(&Checkpoint { state: checkpoint_text.clone(), receipt: receipt.clone() })?;
            receipt.copy_bytes = applied.copy_bytes + checkpoint.len() as u64 + record.len() as u64 + 160;
        }
        record = serde_json::to_vec(&Checkpoint { state: checkpoint_text, receipt: receipt.clone() })?;
        ensure!(
            record.len() <= MAX_RECORD
                && receipt.copy_bytes <= batch.copy_bytes
                && receipt.copy_bytes <= budget.copy_bytes,
            HistoryError::new(ErrorCode::InsufficientBudget, "exact COPY/checkpoint bytes")
        );
        let digest = Sha256::digest(&record).to_vec();
        tx.execute_typed("insert into history_checkpoints(import_id,sequence,bookmark,source_rows,source_bytes,decoded_bytes,callback_events,mutations,copy_bytes,digest) values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",&[(&&id[..],Type::BYTEA),(&next,Type::INT8),(&record,Type::BYTEA),(&to_i64(receipt.source_rows)?,Type::INT8),(&to_i64(receipt.source_bytes)?,Type::INT8),(&to_i64(receipt.decoded_bytes)?,Type::INT8),(&to_i64(receipt.callback_events)?,Type::INT8),(&to_i64(receipt.mutations)?,Type::INT8),(&to_i64(receipt.copy_bytes)?,Type::INT8),(&digest,Type::BYTEA)])?;
        tx.execute_typed(
            "update history_imports set checkpoint_sequence=$2,checkpoint=$3 where import_id=$1",
            &[(&&id[..], Type::BYTEA), (&next, Type::INT8), (&checkpoint, Type::BYTEA)],
        )?;
        if let Err(error) = tx.commit() {
            self.terminal = true;
            return Err(error).context(HistoryError::new(
                ErrorCode::LostOwner,
                "commit outcome requires a new fenced owner and durable checkpoint inspection",
            ));
        }
        self.runtime.committed()?;
        Ok(receipt)
    }
    pub fn finish_identity(&mut self, source: &Source, id: ImportId) -> Result<()> {
        self.live()?;
        source.check_unchanged()?;
        ensure!(source.fingerprint() == id, HistoryError::new(ErrorCode::SourceMismatch, "import fingerprint"));
        let mut tx = self.client.transaction()?;
        tx.batch_execute("set local synchronous_commit=on")?;
        let row = tx.query_one(
            &format!(
                "select state,{CHECKPOINT_SQL},checkpoint_sequence from history_imports where import_id=$1 for update"
            ),
            &[&&id[..]],
        )?;
        let sequence: i64 = row.get(2);
        let checkpoint = checkpoint_payload(&row, 1)?;
        ensure!(sequence > 0, HistoryError::new(ErrorCode::IncompleteImport, "no completed validation checkpoint"));
        let final_row = tx.query_one(
            &format!("select {RECORD_SQL},digest from history_checkpoints where import_id=$1 and sequence=$2"),
            &[&&id[..], &sequence],
        )?;
        let bytes = checkpoint_payload(&final_row, 0)?;
        let digest: Vec<u8> = final_row.get(1);
        ensure!(
            Sha256::digest(&bytes).as_slice() == digest,
            HistoryError::new(ErrorCode::CorruptObject, "final checkpoint digest")
        );
        let record: Checkpoint = serde_json::from_slice(&bytes)?;
        ensure!(
            record.state.as_bytes() == checkpoint && record.receipt.sequence == sequence && record.receipt.complete,
            HistoryError::new(ErrorCode::CorruptObject, "final checkpoint receipt")
        );
        if row.get::<_, String>(0) == "identity_ready" {
            tx.commit()?;
            return Ok(());
        }
        let state: State = serde_json::from_slice(&checkpoint)?;
        ensure!(state.phase == 4, HistoryError::new(ErrorCode::IncompleteImport, "paged validation is incomplete"));
        let provisional: bool = tx
            .query_one(
                "select exists(select 1 from history_objects where import_id=$1 and not validated)",
                &[&&id[..]],
            )?
            .get(0);
        ensure!(!provisional, HistoryError::new(ErrorCode::IncompleteImport, "provisional objects remain"));
        reserve(&mut tx, &self.expected.schema, source.manifest().reserved_next_id, false)?;
        let digest = state.identity_digest.finish()?.to_vec();
        tx.execute(
            "update history_imports set state='identity_ready',identity_digest=$2 where import_id=$1",
            &[&&id[..], &digest],
        )?;
        tx.commit()?;
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
struct Checkpoint {
    state: String,
    receipt: StepReceipt,
}
fn to_i64(n: u64) -> Result<i64> {
    i64::try_from(n).context("history count exceeds signed storage")
}
fn verify_destination(client: &mut postgres::Client, e: &ExpectedDestination) -> Result<()> {
    let row=client.query_one("select current_database(),current_user,current_setting('server_version_num')::integer,(select pg_get_userbyid(nspowner)::text from pg_namespace where nspname=$1),host(inet_server_addr()),inet_server_port(),coalesce((select ssl from pg_stat_ssl where pid=pg_backend_pid()),false)",&[&e.schema])?;
    let owner: Option<String> = row.get(3);
    let address: Option<String> = row.get(4);
    let port: Option<i32> = row.get(5);
    let transport = match e.transport {
        ExpectedTransport::PlaintextTcp => address.is_some() && port.is_some(),
        ExpectedTransport::UnixSocket => address.is_none() && port.is_none(),
    };
    ensure!(
        row.get::<_, String>(0) == e.database
            && row.get::<_, String>(1) == e.role
            && row.get::<_, i32>(2) == e.server_version
            && owner.as_deref().map_or(e.owner == e.role, |owner| owner == e.owner)
            && address == e.server_address
            && port == e.server_port
            && transport
            && !row.get::<_, bool>(6),
        HistoryError::new(ErrorCode::DestinationMismatch, "database/role/owner/server endpoint/transport identity")
    );
    Ok(())
}
fn reserve(tx: &mut postgres::Transaction<'_>, schema: &str, floor: i64, fresh: bool) -> Result<()> {
    let seq: String =
        tx.query_one("select pg_get_serial_sequence($1,'id')", &[&format!("\"{schema}\".releases")])?.get(0);
    let row=tx.query_one("select n.nspname::text,c.relname::text,s.seqincrement,s.seqcache,s.seqcycle from pg_sequence s join pg_class c on c.oid=s.seqrelid join pg_namespace n on n.oid=c.relnamespace where s.seqrelid=$1::text::regclass",&[&seq])?;
    let ns: String = row.get(0);
    let name: String = row.get(1);
    ensure!(
        ns == schema
            && super::valid_schema(&name)
            && row.get::<_, i64>(2) == 1
            && row.get::<_, i64>(3) == 1
            && !row.get::<_, bool>(4),
        HistoryError::new(ErrorCode::DestinationMismatch, "unexpected release identity sequence")
    );
    let current = tx.query_one(&format!("select last_value,is_called from \"{schema}\".\"{name}\""), &[])?;
    let value: i64 = current.get(0);
    let called: bool = current.get(1);
    let next = if called { value.checked_add(1).context("allocator overflow")? } else { value };
    if fresh {
        ensure!(
            next == 1 && !called,
            HistoryError::new(ErrorCode::DestinationMismatch, "unexpected existing native allocations")
        );
    }
    ensure!(floor > 0, HistoryError::new(ErrorCode::SourceMismatch, "invalid allocator reservation"));
    // ALTER SEQUENCE RESTART takes a transactional sequence lock and rolls
    // back with this transaction. Never use nontransactional setval here.
    tx.batch_execute(&format!("alter sequence \"{schema}\".\"{name}\" restart with {}", next.max(floor)))?;
    Ok(())
}
fn object(id: i64, source: i64, key: i64, kind: &str, bookmark: &[u8], length: u64) -> Row {
    Row {
        tag: 1,
        a: id,
        b: source,
        c: key,
        d: i64::try_from(length).expect("admitted object length"),
        text: kind.into(),
        x: bookmark.to_vec(),
        y: Sha256::digest(bookmark).to_vec(),
        ..Default::default()
    }
}
fn chunk(id: i64, ordinal: i64, offset: u64, bytes: &[u8]) -> Row {
    Row {
        tag: 2,
        a: id,
        b: ordinal,
        c: offset as i64,
        x: bytes.to_vec(),
        y: Sha256::digest(bytes).to_vec(),
        ..Default::default()
    }
}
fn seal(id: i64, length: u64, digest: [u8; 32], validated: bool) -> Row {
    Row { tag: 3, a: id, b: -1, c: -1, d: length as i64, x: digest.to_vec(), flag: validated, ..Default::default() }
}
fn allocate(state: &mut State) -> Result<i64> {
    let id = state.next_object;
    state.next_object = id.checked_add(1).context("private object ID overflow")?;
    Ok(id)
}
fn ordered(source: &Source) -> Vec<&crate::history_source::FileManifest> {
    let mut files: Vec<_> = source.manifest().files.iter().collect();
    files.sort_by_key(|f| {
        (
            match f.kind {
                SourceKind::Main => 0,
                SourceKind::Current => 1,
                SourceKind::Frozen => 2,
            },
            f.shard,
            f.source_id,
        )
    });
    files
}
fn relation(index: u8) -> Relation {
    match index {
        0 => Relation::Facts,
        1 => Relation::Releases,
        _ => Relation::Collections,
    }
}
fn relation_name(kind: SourceKind, index: u8) -> &'static str {
    match (kind, index) {
        (_, 0) => "catalog_source_facts",
        (SourceKind::Current, 1) => "releases",
        (SourceKind::Current, _) => "collections",
        (SourceKind::Frozen, 1) => "frozen_releases",
        (SourceKind::Frozen, _) => "frozen_collections",
        _ => "meta",
    }
}
fn receipt(source: i64, code: i64, name: &str, complete: bool, rows: u64, digest: [u8; 32], owner: &str) -> Row {
    Row {
        tag: 8,
        a: source,
        b: code,
        c: rows as i64,
        text: if complete { "complete" } else { "pending" }.into(),
        x: name.as_bytes().to_vec(),
        y: digest.to_vec(),
        flag: complete,
        extra: owner.into(),
        ..Default::default()
    }
}
fn emit_artifact(
    batch: &mut Batch,
    cursor: &mut Artifact,
    source: i64,
    key: i64,
    kind: &str,
    bookmark: &[u8],
    body: &[u8],
) -> Result<bool> {
    ensure!(body.len() <= MAX_METADATA, HistoryError::new(ErrorCode::InsufficientBudget, "bounded metadata index"));
    if !cursor.started {
        batch.push(object(cursor.id, source, key, kind, bookmark, body.len() as u64))?;
        cursor.started = true;
    }
    while cursor.offset < body.len() as u64 && batch.room(13, 1024) {
        let n = (body.len() as u64 - cursor.offset).min(batch.chunk_payload_limit(4096) as u64) as usize;
        if n == 0 {
            break;
        }
        let part = &body[cursor.offset as usize..cursor.offset as usize + n];
        batch.push(chunk(cursor.id, cursor.ordinal, cursor.offset, part))?;
        cursor.digest.update(part)?;
        cursor.offset += n as u64;
        cursor.ordinal += 1;
    }
    if cursor.offset != body.len() as u64 {
        return Ok(false);
    }
    batch.push(seal(cursor.id, body.len() as u64, cursor.digest.finish()?, true))?;
    Ok(true)
}
fn pending_progress(state: &State, batch: &Batch) -> (usize, u64, u64, u8, u64, u64, u64, u64, u64, usize) {
    let p = state.pending.as_ref();
    let frozen = p.and_then(|p| p.frozen.as_ref());
    (
        p.map_or(0, |p| p.cell),
        p.and_then(|p| p.cells.get(p.cell)).map_or(0, |c| c.offset),
        p.map_or(0, |p| p.output.offset),
        frozen.map_or(0, |f| f.phase),
        frozen.map_or(0, |f| f.projection_data.offset),
        frozen.map_or(0, |f| f.index.offset),
        batch.source_bytes,
        batch.decoded_bytes,
        batch.events,
        batch.rows.len(),
    )
}
fn consume(source: &Source, state: &mut State, batch: &mut Batch, runtime: &mut Runtime) -> Result<()> {
    let files = ordered(source);
    if let Some(old) = state.cleanup.take() {
        state.cleanups.push(old);
    }
    ensure!(
        state.cleanups.len() <= super::history_spool::MAX_COMPLETED,
        HistoryError::new(ErrorCode::InsufficientBudget, "checkpoint cleanup admission")
    );
    while !state.cleanups.is_empty() || !runtime.abandoned.is_empty() {
        if batch.source_left() < 128 || batch.source_rows >= u64::from(batch.budget.source_rows) {
            return Ok(());
        }
        let ((source_id, key), authority) = if let Some(key) = state.cleanups.pop() {
            (key, super::history_spool::CleanupAuthority::CommittedCheckpoint)
        } else {
            (runtime.abandoned.pop().expect("abandoned cleanup"), super::history_spool::CleanupAuthority::Identity)
        };
        super::history_spool::Spool::cleanup(&runtime.root, source.fingerprint(), source_id, key, authority)?;
        batch.source_rows += 1;
        batch.source_bytes += 128;
    }
    loop {
        if !batch.room(12, MAX_METADATA as u64 + 4096)
            || batch.source_left() < 512
            || batch.source_rows >= u64::from(batch.budget.source_rows)
        {
            break;
        }
        match state.phase {
            0 => {
                let bytes = source.manifest_bytes();
                let n = (bytes.len() as u64 - state.manifest_offset)
                    .min(batch.source_left())
                    .min(batch.chunk_payload_limit(4096) as u64) as usize;
                if n > 0 {
                    let part = &bytes[state.manifest_offset as usize..state.manifest_offset as usize + n];
                    batch.push(chunk(1, state.manifest_ordinal, state.manifest_offset, part))?;
                    state.manifest_digest.update(part)?;
                    state.manifest_offset += n as u64;
                    state.manifest_ordinal += 1;
                    batch.source_bytes += n as u64;
                }
                if state.manifest_offset == bytes.len() as u64 {
                    batch.push(seal(1, bytes.len() as u64, state.manifest_digest.finish()?, true))?;
                    batch.push(Row { tag: 9, a: 0, b: 1, ..Default::default() })?;
                    batch.push(receipt(0, 0, "manifest", true, 1, state.manifest_digest.finish()?, "catalog"))?;
                    state.phase = 1;
                }
            }
            1 => {
                let Some(file) = files.get(state.register_file) else {
                    state.phase = 2;
                    continue;
                };
                if !state.register_started {
                    batch.push(Row {
                        tag: 7,
                        a: file.source_id,
                        b: file.shard.map_or(-1, i64::from),
                        c: file.paired_current.unwrap_or(-1),
                        text: match file.kind {
                            SourceKind::Main => "main",
                            SourceKind::Current => "current",
                            SourceKind::Frozen => "frozen",
                        }
                        .into(),
                        x: file.digest.to_vec(),
                        y: file.schema_digest.to_vec(),
                        ..Default::default()
                    })?;
                    batch.push(receipt(file.source_id, 3, "catalog_source_facts", false, 0, [0; 32], "catalog"))?;
                    state.register_started = true;
                    batch.source_rows += 1;
                    batch.source_bytes += 73
                        + if file.shard.is_some() { 4 } else { 0 }
                        + if file.paired_current.is_some() { 8 } else { 0 };
                }
                if batch.source_rows >= u64::from(batch.budget.source_rows) {
                    break;
                }
                if let Some(name) = source.table_name(file.source_id, state.register_table)? {
                    let (code, owner) = if name == relation_name(file.kind, 1) && file.kind != SourceKind::Main {
                        (1, "catalog")
                    } else if name == relation_name(file.kind, 2) && file.kind != SourceKind::Main {
                        (2, "catalog")
                    } else {
                        (
                            100 + state.register_table as i64,
                            if file.kind == SourceKind::Main { "main_import" } else { "pending_history" },
                        )
                    };
                    if file.kind == SourceKind::Main
                        && let Some(verified) = state.paired_main.iter().find(|r| r.table == name)
                    {
                        batch.push(receipt(file.source_id, code, name, true, verified.rows, verified.digest, owner))?;
                    } else {
                        batch.push(receipt(file.source_id, code, name, false, 0, [0; 32], owner))?;
                    }
                    batch.source_bytes += name.len() as u64;
                    batch.source_rows += 1;
                    state.register_table += 1;
                } else {
                    state.register_file += 1;
                    state.register_table = 0;
                    state.register_started = false;
                }
            }
            2 => {
                let Some(file) = files.get(state.file) else {
                    state.phase = 3;
                    break;
                };
                if state.facts_ending.is_some() {
                    let body = serde_json::to_vec(&state.facts)?;
                    let artifact = state.facts_ending.as_mut().expect("facts continuation");
                    if emit_artifact(batch, artifact, file.source_id, 0, "facts", b"facts-index", &body)? {
                        batch.push(Row { tag: 9, a: file.source_id, b: artifact.id, ..Default::default() })?;
                        batch.push(receipt(
                            file.source_id,
                            3,
                            "catalog_source_facts",
                            true,
                            state.relation_rows,
                            state.relation_digest.finish()?,
                            "catalog",
                        ))?;
                        state.facts_ending = None;
                        state.facts.clear();
                        state.relation = 1;
                        state.after = None;
                        state.relation_rows = 0;
                        state.relation_digest = DigestState::default();
                    } else {
                        break;
                    }
                    continue;
                }
                if state.pending.is_some() {
                    let before = pending_progress(state, batch);
                    capture_pending(source, state, batch, file.source_id, file.kind, runtime)?;
                    if state.pending.is_some() && before == pending_progress(state, batch) {
                        break;
                    }
                    continue;
                }
                if file.kind == SourceKind::Main && state.relation > 0 {
                    state.file += 1;
                    state.relation = 0;
                    continue;
                }
                if file.kind == SourceKind::Frozen && runtime.completed.len() >= super::history_spool::MAX_COMPLETED {
                    break;
                }
                let cursor =
                    RowCursor { source_id: file.source_id, relation: relation(state.relation), after: state.after };
                let mut descriptor = None;
                let end = source.scan(
                    &cursor,
                    ReadBudget { rows: 1, bytes: batch.source_left(), chunk_bytes: batch.budget.chunk_bytes },
                    |row| {
                        descriptor = Some(row.clone());
                        Ok(true)
                    },
                )?;
                batch.source_rows += end.rows;
                batch.source_bytes += end.bytes;
                if let Some(row) = descriptor {
                    if state.relation == 2 {
                        let get = |column| -> Result<i64> {
                            match row.fields.iter().find(|(c, _)| *c == column).map(|(_, s)| s) {
                                Some(Scalar::Integer(i)) => Ok(*i),
                                _ => {
                                    bail!(HistoryError::new(
                                        ErrorCode::UnsupportedSchema,
                                        "alias keys and targets must be original signed integers"
                                    ))
                                }
                            }
                        };
                        let hash = get(Column::Alias)?;
                        let target = get(Column::Target)?;
                        ensure!(hash == row.key, HistoryError::new(ErrorCode::SourceMismatch, "alias bookmark"));
                        batch.push(Row { tag: 5, a: file.source_id, b: hash, c: target, ..Default::default() })?;
                        state.relation_digest.update(&hash.to_le_bytes())?;
                        state.relation_digest.update(&target.to_le_bytes())?;
                        state.relation_rows += 1;
                        state.after = Some(row.key);
                    } else {
                        if file.kind == SourceKind::Frozen
                            && let Some(Scalar::Cell(payload)) =
                                row.fields.iter().find(|(c, _)| *c == Column::Payload).map(|(_, v)| v)
                        {
                            let required = payload
                                .bytes
                                .checked_add(super::history_spool::FRAME_OVERHEAD)
                                .context("spool admission overflow")?;
                            ensure!(
                                required <= runtime.quota,
                                HistoryError::new(
                                    ErrorCode::InsufficientBudget,
                                    "frozen frame exceeds total spool quota"
                                )
                            );
                            if required > runtime.quota - runtime.completed.iter().map(|f| f.bytes).sum::<u64>() {
                                break;
                            }
                        }
                        let metadata = allocate(state)?;
                        let mut fields = Vec::new();
                        let mut cells = Vec::new();
                        for (column, scalar) in &row.fields {
                            let value = match scalar {
                                Scalar::Null => Value::Null,
                                Scalar::Integer(i) => Value::Integer(*i),
                                Scalar::RealBits(i) => Value::RealBits(*i),
                                Scalar::Cell(cell) => {
                                    let object = if cell.bytes > INLINE || *column == Column::Payload {
                                        Some(allocate(state)?)
                                    } else {
                                        None
                                    };
                                    cells.push(Cell {
                                        reference: cell.clone(),
                                        object,
                                        offset: 0,
                                        ordinal: 0,
                                        digest: DigestState::default(),
                                        inline: Vec::new(),
                                        started: false,
                                    });
                                    if let Some(object_id) = object {
                                        Value::Bytes {
                                            object_id,
                                            offset: 0,
                                            length: cell.bytes,
                                            kind: cell.kind,
                                            digest: [0; 32],
                                        }
                                    } else {
                                        Value::Inline { kind: cell.kind, bytes: Vec::new() }
                                    }
                                }
                            };
                            fields.push((*column, value));
                        }
                        state.pending = Some(Pending {
                            row,
                            metadata,
                            fields,
                            cells,
                            cell: 0,
                            output: Artifact { id: metadata, ..Default::default() },
                            frozen: None,
                        });
                    }
                } else if end.complete {
                    if state.relation > 0 {
                        let expected = file
                            .catalog_counts
                            .as_ref()
                            .context(HistoryError::new(ErrorCode::SourceMismatch, "catalog count receipt missing"))?;
                        ensure!(
                            state.relation_rows
                                == if state.relation == 1 { expected.releases } else { expected.collections },
                            HistoryError::new(
                                ErrorCode::SourceMismatch,
                                "captured catalog count differs from content manifest"
                            )
                        );
                    }
                    if state.relation == 0 {
                        state.facts_ending = Some(Artifact { id: allocate(state)?, ..Default::default() });
                        continue;
                    }
                    batch.push(receipt(
                        file.source_id,
                        if state.relation == 0 { 3 } else { i64::from(state.relation) },
                        relation_name(file.kind, state.relation),
                        true,
                        state.relation_rows,
                        state.relation_digest.finish()?,
                        "catalog",
                    ))?;
                    state.relation += 1;
                    state.after = None;
                    state.relation_rows = 0;
                    state.relation_digest = DigestState::default();
                    if state.relation == 3 {
                        state.file += 1;
                        state.relation = 0;
                    }
                }
            }
            3 => {
                break;
            }
            4 => break,
            _ => bail!(HistoryError::new(ErrorCode::CorruptObject, "unsupported checkpoint phase")),
        }
    }
    Ok(())
}
fn capture_pending(
    source: &Source,
    state: &mut State,
    batch: &mut Batch,
    source_id: i64,
    kind: SourceKind,
    runtime: &mut Runtime,
) -> Result<()> {
    let mut pending = state.pending.take().expect("pending row");
    while pending.cell < pending.cells.len() && batch.room(12, MAX_METADATA as u64 + 4096) && batch.source_left() >= 32
    {
        let cell = &mut pending.cells[pending.cell];
        if cell.reference.column == Column::Payload && !runtime.recover(source, cell, pending.row.key, batch)? {
            state.pending = Some(pending);
            return Ok(());
        }
        if batch.source_left() < 32 {
            state.pending = Some(pending);
            return Ok(());
        }
        if !cell.started {
            if let Some(id) = cell.object {
                let bookmark = serde_json::to_vec(&(relation(state.relation), pending.row.key, cell.reference.column))?;
                batch.push(object(
                    id,
                    source_id,
                    pending.row.key,
                    if cell.reference.column == Column::Payload { "frozen_payload" } else { "cell" },
                    &bookmark,
                    cell.reference.bytes,
                ))?;
            }
            cell.started = true;
        }
        let chunk_limit = batch.chunk_payload_limit(MAX_METADATA as u64 + 4096);
        if chunk_limit == 0 {
            state.pending = Some(pending);
            return Ok(());
        }
        let mut bytes = Vec::new();
        let end = source.read_cell(
            &cell.reference,
            cell.offset,
            ReadBudget { rows: 1, bytes: batch.source_left(), chunk_bytes: chunk_limit as u32 },
            |part| {
                bytes.extend_from_slice(part);
                Ok(())
            },
        )?;
        batch.source_bytes += end.bytes;
        if cell.reference.column == Column::Payload && !bytes.is_empty() {
            runtime.spool.as_mut().expect("owned spool").append(cell.offset, &bytes)?;
        }
        if let Some(id) = cell.object {
            if !bytes.is_empty() {
                batch.push(chunk(id, cell.ordinal, cell.offset, &bytes))?;
                cell.ordinal += 1;
            }
        } else {
            cell.inline.extend_from_slice(&bytes);
        }
        cell.digest.update(&bytes)?;
        cell.offset = end.next;
        if cell.offset == cell.reference.bytes {
            let digest = cell.digest.finish()?;
            if let Some(id) = cell.object {
                batch.push(seal(id, cell.reference.bytes, digest, cell.reference.column != Column::Payload))?;
            }
            let value = if let Some(object_id) = cell.object {
                Value::Bytes { object_id, offset: 0, length: cell.reference.bytes, kind: cell.reference.kind, digest }
            } else {
                Value::Inline { kind: cell.reference.kind, bytes: std::mem::take(&mut cell.inline) }
            };
            let slot = pending
                .fields
                .iter_mut()
                .find(|(c, _)| *c == cell.reference.column)
                .context("source field disappeared")?;
            slot.1 = value;
            pending.cell += 1;
        }
    }
    if pending.cell < pending.cells.len() {
        state.pending = Some(pending);
        return Ok(());
    }
    if kind == SourceKind::Frozen {
        if !finish_frozen(source, state, batch, &mut pending, runtime)? {
            state.pending = Some(pending);
        }
        return Ok(());
    }
    let metadata = Metadata { version: 1, source_id, source_key: pending.row.key, fields: pending.fields.clone() };
    let body = serde_json::to_vec(&metadata)?;
    ensure!(body.len() <= MAX_METADATA, HistoryError::new(ErrorCode::InsufficientBudget, "metadata index"));
    if !emit_artifact(
        batch,
        &mut pending.output,
        source_id,
        pending.row.key,
        "metadata",
        &serde_json::to_vec(&(relation(state.relation), pending.row.key))?,
        &body,
    )? {
        state.pending = Some(pending);
        return Ok(());
    }
    if state.relation == 1 {
        state.variant_rows = state.variant_rows.checked_add(1).context("variant count overflow")?;
        ensure!(
            pending.row.key < source.manifest().reserved_next_id,
            HistoryError::new(ErrorCode::SourceMismatch, "public ID exceeds imported allocator reservation")
        );
        batch.push(Row {
            tag: 4,
            a: source_id,
            b: pending.row.key,
            c: pending.metadata,
            d: -1,
            e: -1,
            ..Default::default()
        })?;
    } else {
        state.facts.push(pending.metadata);
    }
    state.relation_digest.update(&Sha256::digest(&body))?;
    state.relation_rows += 1;
    state.after = Some(pending.row.key);
    state.pending = None;
    Ok(())
}

fn projected_metadata(
    source_id: i64,
    key: i64,
    object: i64,
    projection: &super::history_validator::Projection,
) -> Metadata {
    use super::history_validator::ProjectedValue as P;
    Metadata {
        version: 1,
        source_id,
        source_key: key,
        fields: projection
            .fields
            .iter()
            .map(|(column, value)| {
                (
                    *column,
                    match value {
                        P::Null => Value::Null,
                        P::Integer(i) => Value::Integer(*i),
                        P::RealBits(i) => Value::RealBits(*i),
                        P::Bytes { kind, offset, length, digest } => Value::Bytes {
                            object_id: object,
                            offset: *offset,
                            length: *length,
                            kind: *kind,
                            digest: *digest,
                        },
                    },
                )
            })
            .collect(),
    }
}
fn equivalent(header: &Value, embedded: &super::history_validator::ProjectedValue) -> bool {
    use super::history_validator::ProjectedValue as P;
    match (header, embedded) {
        (Value::Null, P::Null) => true,
        (Value::Integer(a), P::Integer(b)) => a == b,
        (Value::RealBits(a), P::RealBits(b)) => a == b,
        (Value::Inline { kind, bytes }, P::Bytes { kind: other, length, digest, .. }) => {
            kind == other && bytes.len() as u64 == *length && Sha256::digest(bytes).as_slice() == digest
        }
        (
            Value::Bytes { kind, length, digest, .. },
            P::Bytes { kind: other, length: other_len, digest: other_digest, .. },
        ) => kind == other && length == other_len && digest == other_digest,
        _ => false,
    }
}
fn finish_frozen(
    source: &Source,
    state: &mut State,
    batch: &mut Batch,
    pending: &mut Pending,
    runtime: &mut Runtime,
) -> Result<bool> {
    let payload = pending
        .cells
        .iter()
        .find(|c| c.reference.column == Column::Payload)
        .context(HistoryError::new(ErrorCode::UnsupportedSchema, "frozen payload is not a byte cell"))?
        .clone();
    ensure!(
        payload.offset == payload.reference.bytes,
        HistoryError::new(ErrorCode::IncompleteImport, "frozen envelope capture is incomplete")
    );
    let payload_id =
        payload.object.context(HistoryError::new(ErrorCode::CorruptObject, "frozen payload object missing"))?;
    let source_id = payload.reference.source_id;
    let key = pending.row.key;
    if pending.frozen.is_none() {
        pending.frozen = Some(Frozen::default());
    }
    let frozen = pending.frozen.as_mut().expect("frozen continuation");
    if frozen.phase < 2 {
        if !runtime.recover(source, &payload, key, batch)? {
            return Ok(false);
        }
    } else {
        runtime.ensure_spool(source, &payload, key, true)?;
    }
    if frozen.phase == 0 {
        let header = Metadata { version: 1, source_id, source_key: key, fields: pending.fields.clone() };
        let body = serde_json::to_vec(&header)?;
        if !emit_artifact(
            batch,
            &mut pending.output,
            source_id,
            key,
            "frozen_header",
            &serde_json::to_vec(&("frozen-header", key))?,
            &body,
        )? {
            return Ok(false);
        }
        frozen.phase = 1;
        return Ok(false);
    }
    if frozen.phase == 1 {
        // The worker gets only this page's unconsumed permits. A small frame
        // can complete alongside other admitted frames; a large one stays live.
        let events = (u64::from(batch.budget.source_rows) - batch.events)
            .min(u64::from(batch.budget.mutations - batch.mutations));
        if batch.source_left() < 512 || events == 0 {
            return Ok(false);
        }
        if runtime.validator.is_none() {
            let spool = runtime.spool.as_mut().expect("owned frozen spool");
            let input = spool.input_reader()?;
            let projection = spool.projection_writer()?;
            runtime.validator = Some(super::history_validator::Validator::start(
                input,
                projection,
                payload.reference.bytes,
                payload.digest.finish()?,
                key,
                spool.quota,
                runtime.decoder,
            )?);
        }
        let progress = runtime
            .validator
            .as_mut()
            .expect("owned validator")
            .advance(batch.source_left(), u32::try_from(events)?)?;
        batch.source_bytes += progress.usage.compressed;
        batch.decoded_bytes += progress.usage.expanded;
        batch.events += progress.usage.events;
        if let Some(result) = progress.result {
            let validated =
                result.context(HistoryError::new(ErrorCode::CorruptObject, "frozen envelope codec validation"))?;
            runtime.validator.take();
            runtime.spool.as_mut().expect("owned spool").finish_projection()?;
            frozen.rows = validated.stats.rows;
            frozen.values = validated.stats.values;
            frozen.projection = Some(validated.projection);
            frozen.projection_data = Artifact { id: allocate(state)?, ..Default::default() };
            frozen.index = Artifact { id: allocate(state)?, ..Default::default() };
            frozen.phase = 2;
            let mut completed = seal(payload_id, payload.reference.bytes, payload.digest.finish()?, true);
            completed.b = to_i64(frozen.rows)?;
            completed.c = to_i64(frozen.values)?;
            batch.push(completed)?;
        }
        return Ok(false);
    }
    if frozen.phase == 2 {
        let projection = frozen
            .projection
            .as_ref()
            .context(HistoryError::new(ErrorCode::CorruptObject, "validated projection receipt missing"))?;
        let cursor = &mut frozen.projection_data;
        if !cursor.started {
            batch.push(object(
                cursor.id,
                source_id,
                key,
                "cell",
                &serde_json::to_vec(&("frozen-projection-bytes", key))?,
                projection.byte_length,
            ))?;
            cursor.started = true;
        }
        while cursor.offset < projection.byte_length
            && batch.source_left() > 0
            && batch.room(13, 1024)
            && batch.source_rows < u64::from(batch.budget.source_rows)
        {
            let count = (projection.byte_length - cursor.offset)
                .min(batch.source_left())
                .min(batch.chunk_payload_limit(4096) as u64) as usize;
            if count == 0 {
                break;
            }
            let bytes = runtime.spool.as_mut().expect("validated spool").read_projection(
                cursor.offset,
                projection.byte_length,
                count,
            )?;
            batch.push(chunk(cursor.id, cursor.ordinal, cursor.offset, &bytes))?;
            cursor.digest.update(&bytes)?;
            cursor.offset += bytes.len() as u64;
            cursor.ordinal += 1;
            batch.source_bytes += bytes.len() as u64;
            batch.source_rows += 1;
        }
        if cursor.offset != projection.byte_length {
            return Ok(false);
        }
        ensure!(
            cursor.digest.finish()? == projection.digest,
            HistoryError::new(ErrorCode::CorruptObject, "validated projection digest differs")
        );
        batch.push(seal(cursor.id, projection.byte_length, projection.digest, true))?;
        frozen.phase = 3;
        return Ok(false);
    }
    if frozen.phase == 3 {
        let projection = frozen
            .projection
            .as_ref()
            .context(HistoryError::new(ErrorCode::CorruptObject, "projection metadata missing"))?;
        let metadata = projected_metadata(source_id, key, frozen.projection_data.id, projection);
        let body = serde_json::to_vec(&metadata)?;
        if !emit_artifact(
            batch,
            &mut frozen.index,
            source_id,
            key,
            "frozen_metadata",
            &serde_json::to_vec(&("frozen-metadata-index", key))?,
            &body,
        )? {
            return Ok(false);
        }
        ensure!(
            key < source.manifest().reserved_next_id,
            HistoryError::new(ErrorCode::SourceMismatch, "frozen public ID exceeds allocator reservation")
        );
        batch.push(Row {
            tag: 4,
            a: source_id,
            b: key,
            c: frozen.index.id,
            d: pending.metadata,
            e: payload_id,
            ..Default::default()
        })?;
        let differing: Vec<Column> = projection
            .fields
            .iter()
            .filter_map(|(column, value)| {
                pending
                    .fields
                    .iter()
                    .find(|(c, _)| c == column)
                    .filter(|(_, header)| !equivalent(header, value))
                    .map(|_| *column)
            })
            .collect();
        batch.push(Row {
            tag: 12,
            a: source_id,
            b: key,
            c: pending.metadata,
            d: frozen.index.id,
            x: serde_json::to_vec(&differing)?,
            text: "embedded_release_thaw_v1".into(),
            ..Default::default()
        })?;
        state.relation_digest.update(&Sha256::digest(&serde_json::to_vec(&Metadata {
            version: 1,
            source_id,
            source_key: key,
            fields: pending.fields.clone(),
        })?))?;
        state.relation_rows = state.relation_rows.checked_add(1).context("frozen source row count overflow")?;
        state.variant_rows = state.variant_rows.checked_add(1).context("variant count overflow")?;
        state.after = Some(key);
        ensure!(
            state.cleanups.len() < super::history_spool::MAX_COMPLETED,
            HistoryError::new(ErrorCode::InsufficientBudget, "completed frame cleanup count")
        );
        state.cleanups.push((source_id, key));
        runtime.complete(source, key, source_id)?;
        return Ok(true);
    }
    bail!(HistoryError::new(ErrorCode::CorruptObject, "frozen continuation phase"))
}

fn finish_validated_object(state: &mut State) -> Result<()> {
    if state.validate_active {
        ensure!(
            state.validate_offset == state.validate_length
                && state.validate_digest.finish()?.as_slice() == state.validate_expected,
            HistoryError::new(ErrorCode::CorruptObject, "exact object length/digest")
        );
        state.identity_digest.update(&state.validate_object.to_le_bytes())?;
        state.identity_digest.update(&state.validate_expected)?;
        state.validate_active = false;
    }
    Ok(())
}
fn validate_page(
    tx: &mut postgres::Transaction<'_>,
    id: &ImportId,
    state: &mut State,
    batch: &mut Batch,
) -> Result<u64> {
    if batch.source_left() < 512 || batch.source_rows >= u64::from(batch.budget.source_rows) {
        return Ok(0);
    }
    let limit =
        (u64::from(batch.budget.source_rows) - batch.source_rows).min(batch.source_left() / 128).min(512) as i64;
    if limit == 0 {
        return Ok(0);
    }
    match state.validate_part {
        0 => {
            // An extent is one logical chunk row. The later payload query
            // reuses its identity, but all returned bytes/IDs are also charged.
            let extent_limit = (u64::from(batch.budget.source_rows) - batch.source_rows)
                .min(batch.source_left() / (VALIDATION_EXTENT_BYTES + VALIDATION_PAYLOAD_OVERHEAD + 1))
                .min(512) as i64;
            if extent_limit == 0 {
                return Ok(0);
            }
            let rows=tx.query_typed("select c.object_id,c.ordinal,c.byte_offset,octet_length(c.bytes)::bigint,c.digest,o.byte_length,o.digest,o.validated from history_object_chunks c join history_objects o using(import_id,object_id) where c.import_id=$1 and (c.object_id,c.ordinal)>=($2,$3) order by c.object_id,c.ordinal limit $4",&[(&&id[..],Type::BYTEA),(&state.validate_object,Type::INT8),(&state.validate_ordinal,Type::INT8),(&extent_limit,Type::INT8)])?;
            if rows.is_empty() {
                finish_validated_object(state)?;
                state.validate_part = 1;
                state.validate_object = 0;
                return Ok(1);
            }
            // Debit the entire fetched extent page before payload selection,
            // including lookahead rows that this step cannot consume.
            batch.source_rows += rows.len() as u64;
            batch.source_bytes += rows.len() as u64 * VALIDATION_EXTENT_BYTES;
            let (mut objects, mut ordinals, mut offsets, mut lengths) =
                (Vec::<i64>::new(), Vec::<i64>::new(), Vec::<i32>::new(), Vec::<i32>::new());
            let mut chosen = Vec::new();
            let mut left = batch.source_left();
            for row in rows {
                if left <= VALIDATION_PAYLOAD_OVERHEAD {
                    break;
                }
                let oid: i64 = row.get(0);
                let ordinal: i64 = row.get(1);
                let full: i64 = row.get(3);
                let offset = if oid == state.validate_object && ordinal == state.validate_ordinal {
                    state.validate_chunk_offset
                } else {
                    0
                };
                ensure!(
                    full > 0 && offset < full as u64,
                    HistoryError::new(ErrorCode::CorruptObject, "chunk continuation")
                );
                let n = (full as u64 - offset)
                    .min(left - VALIDATION_PAYLOAD_OVERHEAD)
                    .min(u64::from(batch.budget.chunk_bytes)) as i32;
                objects.push(oid);
                ordinals.push(ordinal);
                offsets.push(offset as i32);
                lengths.push(n);
                left -= VALIDATION_PAYLOAD_OVERHEAD + n as u64;
                chosen.push(row);
                if n as u64 + offset < full as u64 {
                    break;
                }
            }
            if chosen.is_empty() {
                return Ok(1);
            }
            let data=tx.query_typed("select r.object_id,r.ordinal,substring(c.bytes from r.start_byte+1 for r.byte_count) from unnest($2::bigint[],$3::bigint[],$4::integer[],$5::integer[]) as r(object_id,ordinal,start_byte,byte_count) join history_object_chunks c on c.import_id=$1 and c.object_id=r.object_id and c.ordinal=r.ordinal order by r.object_id,r.ordinal",&[(&&id[..],Type::BYTEA),(&objects,Type::INT8_ARRAY),(&ordinals,Type::INT8_ARRAY),(&offsets,Type::INT4_ARRAY),(&lengths,Type::INT4_ARRAY)])?;
            ensure!(data.len() == chosen.len(), HistoryError::new(ErrorCode::CorruptObject, "missing captured chunks"));
            for (index, (meta, data)) in chosen.iter().zip(&data).enumerate() {
                let oid: i64 = meta.get(0);
                let ordinal: i64 = meta.get(1);
                let byte_offset: i64 = meta.get(2);
                let full: i64 = meta.get(3);
                let digest: Vec<u8> = meta.get(4);
                ensure!(
                    meta.get::<_, bool>(7),
                    HistoryError::new(ErrorCode::IncompleteImport, "provisional object during validation")
                );
                if !state.validate_active || oid != state.validate_object {
                    finish_validated_object(state)?;
                    state.validate_object = oid;
                    state.validate_ordinal = 0;
                    state.validate_offset = 0;
                    state.validate_chunk_offset = 0;
                    state.validate_digest = DigestState::default();
                    state.validate_chunk_digest = DigestState::default();
                    state.validate_length = meta.get::<_, i64>(5) as u64;
                    state.validate_expected = meta.get(6);
                    state.validate_active = true;
                }
                ensure!(
                    ordinal == state.validate_ordinal
                        && byte_offset >= 0
                        && byte_offset as u64 + state.validate_chunk_offset == state.validate_offset,
                    HistoryError::new(ErrorCode::CorruptObject, "noncontiguous object chunks")
                );
                let bytes: Vec<u8> = data.get(2);
                ensure!(
                    bytes.len() == lengths[index] as usize,
                    HistoryError::new(ErrorCode::CorruptObject, "short object chunk")
                );
                state.validate_digest.update(&bytes)?;
                state.validate_chunk_digest.update(&bytes)?;
                state.validate_offset += bytes.len() as u64;
                state.validate_chunk_offset += bytes.len() as u64;
                batch.source_bytes += VALIDATION_PAYLOAD_OVERHEAD + bytes.len() as u64;
                if state.validate_chunk_offset == full as u64 {
                    ensure!(
                        state.validate_chunk_digest.finish()?.as_slice() == digest,
                        HistoryError::new(ErrorCode::CorruptObject, "chunk digest")
                    );
                    state.validate_chunk_offset = 0;
                    state.validate_chunk_digest = DigestState::default();
                    state.validate_ordinal += 1;
                }
            }
            Ok(2)
        }
        1 => {
            let rows=tx.query_typed("select object_id,byte_length,digest,validated,exists(select 1 from history_object_chunks c where c.import_id=o.import_id and c.object_id=o.object_id) from history_objects o where import_id=$1 and object_id>$2 order by object_id limit $3",&[(&&id[..],Type::BYTEA),(&state.validate_object,Type::INT8),(&limit,Type::INT8)])?;
            if rows.is_empty() {
                ensure!(
                    state.validated_objects + 1 == state.next_object as u64,
                    HistoryError::new(ErrorCode::CorruptObject, "missing admitted object identities")
                );
                state.validate_part = 2;
                return Ok(1);
            }
            for row in rows {
                let oid: i64 = row.get(0);
                ensure!(
                    oid > 0 && oid < state.next_object && oid as u64 == state.validated_objects + 1,
                    HistoryError::new(ErrorCode::CorruptObject, "unregistered object identity")
                );
                state.validated_objects += 1;
                let len: i64 = row.get(1);
                let digest: Vec<u8> = row.get(2);
                ensure!(row.get::<_, bool>(3), HistoryError::new(ErrorCode::IncompleteImport, "provisional object"));
                ensure!(
                    (len == 0 && !row.get::<_, bool>(4) && Sha256::digest([]).as_slice() == digest)
                        || (len > 0 && row.get::<_, bool>(4)),
                    HistoryError::new(ErrorCode::CorruptObject, "missing/extra object chunks")
                );
                if len == 0 {
                    state.identity_digest.update(&oid.to_le_bytes())?;
                    state.identity_digest.update(&digest)?;
                }
                state.validate_object = oid;
                batch.source_rows += 1;
                batch.source_bytes += 128;
            }
            Ok(1)
        }
        2 => {
            let rows = if let Some(after) = state.catalog_after {
                tx.query_typed("select public_id,selected_source_id from history_catalog where import_id=$1 and public_id>$2 order by public_id limit $3",&[(&&id[..],Type::BYTEA),(&after,Type::INT8),(&limit,Type::INT8)])?
            } else {
                tx.query_typed("select public_id,selected_source_id from history_catalog where import_id=$1 order by public_id limit $2",&[(&&id[..],Type::BYTEA),(&limit,Type::INT8)])?
            };
            if rows.is_empty() {
                state.validate_part = 3;
                return Ok(1);
            }
            for row in rows {
                let public: i64 = row.get(0);
                let source: i64 = row.get(1);
                state.identity_digest.update(&public.to_le_bytes())?;
                state.identity_digest.update(&source.to_le_bytes())?;
                state.catalog_after = Some(public);
                state.canonical_rows += 1;
                batch.source_rows += 1;
                batch.source_bytes += 16;
            }
            Ok(1)
        }
        3 => {
            let params = state.alias_after;
            let query = "select a.source_id,a.original_signed_hash,a.target_public_id,(v.public_id is null) from history_collection_aliases a left join history_release_variants v on v.import_id=a.import_id and v.source_id=a.source_id and v.public_id=a.target_public_id where a.import_id=$1";
            let rows = if let Some((source, key)) = params {
                tx.query_typed(&format!("{query} and (a.source_id,a.original_signed_hash)>($2,$3) order by a.source_id,a.original_signed_hash limit $4"),&[(&&id[..],Type::BYTEA),(&source,Type::INT8),(&key,Type::INT8),(&limit,Type::INT8)])?
            } else {
                tx.query_typed(
                    &format!("{query} order by a.source_id,a.original_signed_hash limit $2"),
                    &[(&&id[..], Type::BYTEA), (&limit, Type::INT8)],
                )?
            };
            if rows.is_empty() {
                state.validate_part = 4;
                return Ok(1);
            }
            for row in rows {
                let source: i64 = row.get(0);
                let key: i64 = row.get(1);
                let target: i64 = row.get(2);
                let dangling: bool = row.get(3);
                for value in [source, key, target] {
                    state.identity_digest.update(&value.to_le_bytes())?;
                }
                state.identity_digest.update(&[u8::from(dangling)])?;
                state.alias_after = Some((source, key));
                state.dangling_aliases += u64::from(dangling);
                batch.source_rows += 1;
                batch.source_bytes += 25;
            }
            Ok(1)
        }
        4 => {
            let query =
                "select source_id,relation_code,logical_rows,digest,state from history_receipts where import_id=$1";
            let rows = if let Some((source, code)) = state.receipt_after {
                tx.query_typed(
                    &format!("{query} and (source_id,relation_code)>($2,$3) order by source_id,relation_code limit $4"),
                    &[(&&id[..], Type::BYTEA), (&source, Type::INT8), (&code, Type::INT4), (&limit, Type::INT8)],
                )?
            } else {
                tx.query_typed(
                    &format!("{query} order by source_id,relation_code limit $2"),
                    &[(&&id[..], Type::BYTEA), (&limit, Type::INT8)],
                )?
            };
            if rows.is_empty() {
                state.validate_part = 5;
                return Ok(1);
            }
            for row in rows {
                let source: i64 = row.get(0);
                let code: i32 = row.get(1);
                if code <= 3 {
                    ensure!(
                        row.get::<_, String>(4) == "complete",
                        HistoryError::new(ErrorCode::IncompleteImport, "selected relation receipt is pending")
                    );
                }
                state.receipt_after = Some((source, code));
                batch.source_rows += 1;
                batch.source_bytes += 128;
            }
            Ok(1)
        }
        5 => {
            let query="select v.source_id,v.public_id,v.metadata_object_id,v.frozen_header_object_id,v.frozen_payload_object_id,
                coalesce(m.source_id=v.source_id and m.scope_release_id=v.public_id and m.validated and m.byte_length<=49152
                and ((s.kind='current' and m.object_kind='metadata' and v.frozen_header_object_id is null and v.frozen_payload_object_id is null)
                    or (s.kind='frozen' and m.object_kind='frozen_metadata' and h.source_id=v.source_id and h.scope_release_id=v.public_id and h.object_kind='frozen_header' and h.validated and h.byte_length<=49152
                        and p.source_id=v.source_id and p.scope_release_id=v.public_id and p.object_kind='frozen_payload' and p.validated and p.logical_rows is not null and p.logical_values is not null
                        and r.header_object_id=h.object_id and r.metadata_object_id=m.object_id))
                and c.public_id is not null and c.selected_source_id=coalesce((select w.source_id from history_release_variants w join history_sources ws on ws.import_id=w.import_id and ws.source_id=w.source_id where w.import_id=v.import_id and w.public_id=v.public_id and ws.kind='current' limit 1),v.source_id)
                and not exists(select 1 from history_release_variants ov join history_sources os on os.import_id=ov.import_id and os.source_id=ov.source_id where ov.import_id=v.import_id and ov.public_id=v.public_id and ov.source_id<>v.source_id
                    and not ((s.kind='current' and os.kind='frozen' and os.paired_current_source=s.source_id) or (s.kind='frozen' and os.kind='current' and s.paired_current_source=os.source_id))),false)
                from history_release_variants v join history_sources s on s.import_id=v.import_id and s.source_id=v.source_id
                left join history_objects m on m.import_id=v.import_id and m.object_id=v.metadata_object_id
                left join history_objects h on h.import_id=v.import_id and h.object_id=v.frozen_header_object_id
                left join history_objects p on p.import_id=v.import_id and p.object_id=v.frozen_payload_object_id
                left join history_frozen_resolutions r on r.import_id=v.import_id and r.source_id=v.source_id and r.public_id=v.public_id
                left join history_catalog c on c.import_id=v.import_id and c.public_id=v.public_id where v.import_id=$1";
            let rows = if let Some((source, key)) = state.variant_after {
                tx.query_typed(
                    &format!("{query} and (v.source_id,v.public_id)>($2,$3) order by v.source_id,v.public_id limit $4"),
                    &[(&&id[..], Type::BYTEA), (&source, Type::INT8), (&key, Type::INT8), (&limit, Type::INT8)],
                )?
            } else {
                tx.query_typed(
                    &format!("{query} order by v.source_id,v.public_id limit $2"),
                    &[(&&id[..], Type::BYTEA), (&limit, Type::INT8)],
                )?
            };
            if rows.is_empty() {
                ensure!(
                    state.validated_variant_rows == state.variant_rows,
                    HistoryError::new(ErrorCode::CorruptObject, "variant count differs from admitted source rows")
                );
                let overlap =
                    state.variant_rows.checked_sub(state.canonical_rows).context("canonical count exceeds variants")?;
                for (code, name, count) in [
                    (10, "canonical_public_ids", state.canonical_rows),
                    (11, "current_frozen_overlap", overlap),
                    (12, "source_dangling_aliases", state.dangling_aliases),
                ] {
                    batch.push(receipt(0, code, name, true, count, state.identity_digest.finish()?, "catalog"))?;
                }
                state.phase = 4;
                return Ok(1);
            }
            for row in rows {
                ensure!(
                    row.get::<_, bool>(5),
                    HistoryError::new(ErrorCode::CorruptObject, "variant source/object/winner references")
                );
                let source: i64 = row.get(0);
                let key: i64 = row.get(1);
                for value in [
                    source,
                    key,
                    row.get(2),
                    row.get::<_, Option<i64>>(3).unwrap_or(-1),
                    row.get::<_, Option<i64>>(4).unwrap_or(-1),
                ] {
                    state.identity_digest.update(&value.to_le_bytes())?;
                }
                state.variant_after = Some((source, key));
                state.validated_variant_rows += 1;
                batch.source_rows += 1;
                batch.source_bytes += 128;
            }
            Ok(1)
        }
        _ => bail!(HistoryError::new(ErrorCode::CorruptObject, "validation checkpoint phase")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_fresh_owned_target_admits_the_identity_import() {
        let Some(test) = crate::pg::testing::Schema::empty() else { return };
        let mut c = test.pool.get().unwrap();
        let row = c
            .query_one("select current_database(),current_user,current_setting('server_version_num')::integer,host(inet_server_addr()),inet_server_port()", &[])
            .unwrap();
        let expected = ExpectedDestination {
            database: row.get(0),
            schema: test.name.clone(),
            role: row.get(1),
            owner: row.get(1),
            server_version: row.get(2),
            server_address: row.get(3),
            server_port: row.get(4),
            transport: if row.get::<_, Option<String>>(3).is_some() {
                ExpectedTransport::PlaintextTcp
            } else {
                ExpectedTransport::UnixSocket
            },
            staging_prefix: "t_".into(),
        };
        let spool = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(spool.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let owner = ImportOwner::connect(
            StorageConfig {
                backend: super::super::Backend::Postgres,
                url: test.url.clone(),
                schema: test.name.clone(),
                allow_plaintext: true,
                ..Default::default()
            },
            expected,
            spool.path().to_path_buf(),
            4 * 1024 * 1024,
        );
        assert!(
            owner.is_ok(),
            "fresh explicit destination: {}",
            owner.as_ref().err().map(|e| format!("{e:#}")).unwrap_or_default()
        );
        let (_files, mut manifest) = crate::history_source::tests::fixture();
        crate::history_source::tests::seed_current(&mut manifest, 2);
        let source = Source::open(manifest, 1 << 20).unwrap();
        let mut owner = owner.unwrap();
        let id = owner.begin(&source).unwrap();
        let mut sequence = 0;
        loop {
            let receipt = owner.step(&source, id, sequence, ImportBudget::default()).unwrap();
            sequence = receipt.sequence;
            if receipt.complete {
                break;
            }
        }
        owner.finish_identity(&source, id).unwrap();
        let ids: Vec<i64> = c
            .query("select public_id from history_catalog order by public_id", &[])
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect();
        assert_eq!(ids, [3, 5]);
        let keys: Vec<i64> = c
            .query("select original_signed_hash from history_collection_aliases order by original_signed_hash", &[])
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect();
        assert_eq!(keys, [i64::MIN, 0, 7]);
        let native: i64 = c.query_one("select count(*) from releases", &[]).unwrap().get(0);
        assert_eq!(native, 0);
        let body:Vec<u8>=c.query("select ch.bytes from history_release_variants v join history_object_chunks ch on ch.import_id=v.import_id and ch.object_id=v.metadata_object_id where v.public_id=3 and v.source_id=1 order by ch.ordinal",&[]).unwrap().iter().flat_map(|r|r.get::<_,Vec<u8>>(0)).collect();
        let metadata: Metadata = serde_json::from_slice(&body).unwrap();
        assert_eq!(metadata.field(Column::Size), Some(&Value::RealBits(1.25f64.to_bits())));
        assert_eq!(metadata.field(Column::Complete), Some(&Value::Null));
        assert_eq!(
            metadata.effective_name(),
            Some(&Value::Inline { kind: crate::history_identity::CellType::Text, bytes: vec![] })
        );
        assert_eq!(metadata.field(Column::NameSource), Some(&Value::Integer(40)));
        let next: i64 = c.query_one("select nextval(pg_get_serial_sequence('releases','id'))", &[]).unwrap().get(0);
        assert_eq!(next, 900);
        let state: String = c.query_one("select state from history_imports", &[]).unwrap().get(0);
        assert_eq!(state, "identity_ready");
        assert!(
            super::super::release_queue::claim(&mut c, 1, 1, 1).is_err(),
            "identity_ready must not activate native mutation"
        );
    }
    #[test]
    fn current_frozen_identity_and_aliases_are_admitted_without_native_activation() {
        let Some(test) = crate::pg::testing::Schema::empty() else { return };
        let mut c = test.pool.get().unwrap();
        let row = c
            .query_one("select current_database(),current_user,current_setting('server_version_num')::integer,host(inet_server_addr()),inet_server_port()", &[])
            .unwrap();
        let expected = ExpectedDestination {
            database: row.get(0),
            schema: test.name.clone(),
            role: row.get(1),
            owner: row.get(1),
            server_version: row.get(2),
            server_address: row.get(3),
            server_port: row.get(4),
            transport: if row.get::<_, Option<String>>(3).is_some() {
                ExpectedTransport::PlaintextTcp
            } else {
                ExpectedTransport::UnixSocket
            },
            staging_prefix: "t_".into(),
        };
        let spool = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(spool.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let owner = ImportOwner::connect(
            StorageConfig {
                backend: super::super::Backend::Postgres,
                url: test.url.clone(),
                schema: test.name.clone(),
                allow_plaintext: true,
                ..Default::default()
            },
            expected,
            spool.path().to_path_buf(),
            4 * 1024 * 1024,
        );
        assert!(
            owner.is_ok(),
            "fresh explicit destination: {}",
            owner.as_ref().err().map(|e| format!("{e:#}")).unwrap_or_default()
        );
        let (_files, mut manifest) = crate::history_source::tests::fixture();
        crate::history_source::tests::seed_current(&mut manifest, 2);
        crate::history_source::tests::seed_frozen(&mut manifest, false);
        let source = Source::open(manifest, 1 << 20).unwrap();
        let mut owner = owner.unwrap();
        let id = owner.begin(&source).unwrap();
        let mut sequence = 0;
        loop {
            let receipt = owner.step(&source, id, sequence, ImportBudget::default()).unwrap();
            sequence = receipt.sequence;
            if receipt.complete {
                break;
            }
        }
        owner.finish_identity(&source, id).unwrap();
        let ids: Vec<i64> = c
            .query("select public_id from history_catalog order by public_id", &[])
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect();
        assert_eq!(ids, [3, 5, 41]);
        let keys: Vec<i64> = c
            .query("select original_signed_hash from history_collection_aliases order by original_signed_hash", &[])
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect();
        assert_eq!(keys, [i64::MIN, -9, 0, 0, 7, 7]);
        let native: i64 = c.query_one("select count(*) from releases", &[]).unwrap().get(0);
        assert_eq!(native, 0);
        let body:Vec<u8>=c.query("select ch.bytes from history_release_variants v join history_object_chunks ch on ch.import_id=v.import_id and ch.object_id=v.metadata_object_id where v.public_id=3 and v.source_id=1 order by ch.ordinal",&[]).unwrap().iter().flat_map(|r|r.get::<_,Vec<u8>>(0)).collect();
        let metadata: Metadata = serde_json::from_slice(&body).unwrap();
        assert_eq!(metadata.field(Column::Size), Some(&Value::RealBits(1.25f64.to_bits())));
        assert_eq!(metadata.field(Column::Complete), Some(&Value::Null));
        assert_eq!(
            metadata.effective_name(),
            Some(&Value::Inline { kind: crate::history_identity::CellType::Text, bytes: vec![] })
        );
        assert_eq!(metadata.field(Column::NameSource), Some(&Value::Integer(40)));
        let next: i64 = c.query_one("select nextval(pg_get_serial_sequence('releases','id'))", &[]).unwrap().get(0);
        assert_eq!(next, 900);
        {
            use super::super::history_catalog::{Lookup, LookupPurpose, ReadBudget, Snapshot};
            let mut catalog = Snapshot::open(&mut c, id).unwrap();
            let Lookup::Found(current) = catalog.lookup(3, LookupPurpose::Raw, ReadBudget::default()).unwrap() else {
                panic!("current winner missing")
            };
            assert_eq!(current.source_id, 1);
            let Lookup::Found(frozen) = catalog.lookup(41, LookupPurpose::Raw, ReadBudget::default()).unwrap() else {
                panic!("frozen-only identity missing")
            };
            assert_eq!(frozen.source_id, 3);
            assert!(frozen.payload_object.is_some() && frozen.header_object.is_some());
            assert_eq!(frozen.metadata.field(Column::NameSource), Some(&Value::Integer(40)));
            assert_eq!(frozen.metadata.field(Column::Size), Some(&Value::RealBits(0x8000000000000000)));
            assert!(matches!(catalog.lookup(898, LookupPurpose::Raw, ReadBudget::default()).unwrap(), Lookup::Missing));
            assert!(catalog.lookup(3, LookupPurpose::Search, ReadBudget::default()).is_err());
        }
        let state: String = c.query_one("select state from history_imports", &[]).unwrap().get(0);
        assert_eq!(state, "identity_ready");
        assert!(
            super::super::release_queue::claim(&mut c, 1, 1, 1).is_err(),
            "identity_ready must not activate native mutation"
        );
    }
    fn paired_main_fixture(case: &str, body: impl FnOnce(&mut ImportOwner, &Source, &mut postgres::Client)) {
        let Some(base) = crate::pg::testing::Schema::empty() else { return };
        let stage = format!("atlas_stage_{}", base.name);
        let config = StorageConfig {
            backend: super::super::Backend::Postgres,
            url: base.url.clone(),
            schema: stage.clone(),
            allow_plaintext: true,
            ..Default::default()
        };
        let _cleanup = crate::pg::testing::Schema {
            name: stage.clone(),
            url: base.url.clone(),
            pool: super::super::Pool::new(&config).unwrap(),
        };
        let files = tempfile::tempdir().unwrap();
        let main = super::super::main_import::tests::sqlite_snapshot(files.path());
        let mut c = base.pool.get().unwrap();
        let row = c
            .query_one("select current_database(),current_user,current_setting('server_version_num')::integer,host(inet_server_addr()),inet_server_port()", &[])
            .unwrap();
        let database: String = row.get(0);
        super::super::main_import::import_snapshot(&main, &mut c, &database, &stage).unwrap();
        c.batch_execute(&format!("set search_path to \"{stage}\"")).unwrap();
        let receipt = super::super::history_companion::digest(&mut c, &stage, "main_import").unwrap();
        assert!(receipt.is_some(), "the existing main owner must issue a source-pairing receipt");
        let (_others, mut manifest) = crate::history_source::tests::fixture();
        manifest.files.iter_mut().find(|f| f.kind == SourceKind::Main).unwrap().path = main;
        manifest.main_receipt = if case == "wrong_receipt" { [9; 32] } else { receipt.unwrap() };
        if case == "stale" {
            c.execute("update meta set value=value where key='next_seq'", &[]).unwrap();
        }
        if case == "unknown" {
            c.batch_execute("create table unexpected(value bigint);insert into unexpected values(7)").unwrap();
        }
        manifest.source_uuid = "test-source".into();
        crate::history_source::tests::refresh(&mut manifest);
        let source = Source::open(manifest, 1 << 20).unwrap();
        let spool = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(spool.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let expected = ExpectedDestination {
            database,
            schema: stage,
            role: row.get(1),
            owner: row.get(1),
            server_version: row.get(2),
            server_address: row.get(3),
            server_port: row.get(4),
            transport: if row.get::<_, Option<String>>(3).is_some() {
                ExpectedTransport::PlaintextTcp
            } else {
                ExpectedTransport::UnixSocket
            },
            staging_prefix: "atlas_stage_".into(),
        };
        let mut owner = ImportOwner::connect(config, expected, spool.path().into(), 4 * 1024 * 1024).unwrap();
        body(&mut owner, &source, &mut c);
    }
    #[test]
    fn verified_main_owner_receipt_pairs_a_populated_target() {
        paired_main_fixture("valid", |owner, source, c| {
            let id = owner.begin(source).unwrap();
            let mut seq = 0;
            loop {
                let r = owner.step(source, id, seq, ImportBudget::default()).unwrap();
                seq = r.sequence;
                if r.complete {
                    break;
                }
            }
            owner.finish_identity(source, id).unwrap();
            let complete: i64 = c
                .query_one(
                    "select count(*) from history_receipts where owner_code='main_import' and state='complete'",
                    &[],
                )
                .unwrap()
                .get(0);
            assert!(complete >= 14, "verified main table coverage retained");
            let receipt = c
                .query_one(
                    "select dirty,derived_allocator from history_companion_receipts where owner_code='main_import'",
                    &[],
                )
                .unwrap();
            assert!(!receipt.get::<_, bool>(0));
            assert_eq!(receipt.get::<_, Option<i64>>(1), Some(900));
        });
    }
    #[test]
    fn stale_wrong_or_unowned_populated_targets_are_rejected() {
        for case in ["stale", "wrong_receipt", "unknown"] {
            paired_main_fixture(case, |owner, source, c| {
                assert!(owner.begin(source).is_err(), "{case} target must fail closed");
                let imports: i64 = c.query_one("select count(*) from history_imports", &[]).unwrap().get(0);
                assert_eq!(imports, 0);
            });
        }
    }
    fn inspect_fixture(group_bytes: usize, body: impl FnOnce(&mut postgres::Client, ImportId)) {
        let Some(test) = crate::pg::testing::Schema::empty() else { return };
        let mut c = test.pool.get().unwrap();
        let row = c
            .query_one("select current_database(),current_user,current_setting('server_version_num')::integer,host(inet_server_addr()),inet_server_port()", &[])
            .unwrap();
        let expected = ExpectedDestination {
            database: row.get(0),
            schema: test.name.clone(),
            role: row.get(1),
            owner: row.get(1),
            server_version: row.get(2),
            server_address: row.get(3),
            server_port: row.get(4),
            transport: if row.get::<_, Option<String>>(3).is_some() {
                ExpectedTransport::PlaintextTcp
            } else {
                ExpectedTransport::UnixSocket
            },
            staging_prefix: "t_".into(),
        };
        let config = StorageConfig {
            backend: super::super::Backend::Postgres,
            url: test.url.clone(),
            schema: test.name.clone(),
            allow_plaintext: true,
            ..Default::default()
        };
        let (_files, mut manifest) = crate::history_source::tests::fixture();
        crate::history_source::tests::seed_current(&mut manifest, 1);
        if group_bytes > 0 {
            let path = manifest.files.iter().find(|f| f.kind == SourceKind::Current).unwrap().path.clone();
            let s = rusqlite::Connection::open(path).unwrap();
            s.execute("update releases set group_name=?1", ["g".repeat(group_bytes)]).unwrap();
            drop(s);
            crate::history_source::tests::refresh(&mut manifest);
        }
        let source = Source::open(manifest, 1 << 20).unwrap();
        let spool = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(spool.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut owner = ImportOwner::connect(config, expected, spool.path().into(), 4 * 1024 * 1024).unwrap();
        let id = owner.begin(&source).unwrap();
        let mut sequence = 0;
        loop {
            let r = owner.step(&source, id, sequence, ImportBudget::default()).unwrap();
            sequence = r.sequence;
            if r.complete {
                break;
            }
        }
        owner.finish_identity(&source, id).unwrap();
        body(&mut c, id);
    }
    #[test]
    fn corrupt_metadata_payload_is_admitted_before_any_large_fetch() {
        inspect_fixture(0, |c, id| {
            c.execute("update history_object_chunks set bytes=$1 where object_id=(select metadata_object_id from history_release_variants where public_id=3)",&[&vec![b'x';65536]]).unwrap();
            let mut snapshot = super::super::history_catalog::Snapshot::open(c, id).unwrap();
            let budget = super::super::history_catalog::ReadBudget { rows: 8, bytes: 2048, chunk_bytes: 64 };
            assert!(snapshot.lookup(3, super::super::history_catalog::LookupPurpose::Raw, budget).is_err());
            assert!(
                snapshot.stats().payload_bytes <= budget.bytes,
                "corrupt metadata fetched {} bytes under a {} byte budget",
                snapshot.stats().payload_bytes,
                budget.bytes
            );
        });
    }
    #[test]
    fn direct_policy_debits_the_same_aggregate_lookup_budget() {
        inspect_fixture(8192, |c, id| {
            let mut snapshot = super::super::history_catalog::Snapshot::open(c, id).unwrap();
            let budget = super::super::history_catalog::ReadBudget { rows: 32, bytes: 8320, chunk_bytes: 65536 };
            assert!(
                snapshot
                    .lookup(
                        3,
                        super::super::history_catalog::LookupPurpose::Direct { group_blocked: |_| false },
                        budget
                    )
                    .is_err(),
                "group policy may not receive a fresh full metadata budget"
            );
            assert!(snapshot.stats().payload_bytes <= budget.bytes);
        });
    }
    #[test]
    fn emit_artifact_copy_limited_page_preserves_reserve_and_exact_continuation() {
        let budget = ImportBudget::default();
        let mut batch = Batch::new(budget);
        batch.copy_bytes = budget.copy_bytes - 5000;
        let body = vec![b'q'; 8192];
        let mut cursor = Artifact { id: 7, ..Default::default() };
        assert!(!emit_artifact(&mut batch, &mut cursor, 1, 3, "metadata", b"key", &body).unwrap());
        assert!(batch.copy_bytes <= budget.copy_bytes - 4096, "chunk row overhead must not consume the final reserve");
        let mut stored = Vec::new();
        for row in batch.rows.iter().filter(|r| r.tag == 2) {
            stored.extend_from_slice(&row.x);
        }
        let mut next = Batch::new(budget);
        assert!(emit_artifact(&mut next, &mut cursor, 1, 3, "metadata", b"key", &body).unwrap());
        for row in next.rows.iter().filter(|r| r.tag == 2) {
            stored.extend_from_slice(&row.x);
        }
        assert_eq!(stored, body);
        assert_eq!(cursor.digest.finish().unwrap(), <[u8; 32]>::from(Sha256::digest(&body)));
    }
    struct Fixture {
        target: crate::pg::testing::Schema,
        _files: tempfile::TempDir,
        source: Source,
        spool: tempfile::TempDir,
        config: StorageConfig,
        expected: ExpectedDestination,
    }
    impl Fixture {
        fn new() -> Option<Self> {
            Self::with_frozen(false)
        }
        fn with_frozen(frozen: bool) -> Option<Self> {
            Self::with_frame_count(if frozen { 2 } else { 0 })
        }
        fn with_frame_count(frames: usize) -> Option<Self> {
            let target = crate::pg::testing::Schema::empty()?;
            let mut c = target.pool.get().unwrap();
            let row=c.query_one("select current_database(),current_user,current_setting('server_version_num')::integer,host(inet_server_addr()),inet_server_port()",&[]).unwrap();
            let expected = ExpectedDestination {
                database: row.get(0),
                schema: target.name.clone(),
                role: row.get(1),
                owner: row.get(1),
                server_version: row.get(2),
                server_address: row.get(3),
                server_port: row.get(4),
                transport: if row.get::<_, Option<String>>(3).is_some() {
                    ExpectedTransport::PlaintextTcp
                } else {
                    ExpectedTransport::UnixSocket
                },
                staging_prefix: "t_".into(),
            };
            let config = StorageConfig {
                backend: super::super::Backend::Postgres,
                url: target.url.clone(),
                schema: target.name.clone(),
                allow_plaintext: true,
                ..Default::default()
            };
            let (files, mut manifest) = crate::history_source::tests::fixture();
            crate::history_source::tests::seed_current(&mut manifest, 2);
            if frames > 0 {
                crate::history_source::tests::seed_frozen_count(&mut manifest, false, frames, 2048);
            }
            let source = Source::open(manifest, 1 << 20).unwrap();
            let spool = tempfile::tempdir().unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(spool.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            drop(c);
            Some(Self { target, _files: files, source, spool, config, expected })
        }
        fn owner(&self) -> ImportOwner {
            ImportOwner::connect(self.config.clone(), self.expected.clone(), self.spool.path().into(), 4 * 1024 * 1024)
                .unwrap()
        }
    }
    fn finish_pages(owner: &mut ImportOwner, source: &Source, id: ImportId, mut seq: i64) -> Result<()> {
        for _ in 0..10000 {
            let r = owner.step(source, id, seq, ImportBudget::default())?;
            seq = r.sequence;
            if r.complete {
                return owner.finish_identity(source, id);
            }
        }
        bail!("test exceeded bounded fixture pages")
    }
    fn capture_fixture(f: &Fixture, owner: &mut ImportOwner, id: ImportId) -> i64 {
        let mut seq = 0;
        loop {
            let r = owner.step(&f.source, id, seq, ImportBudget::default()).unwrap();
            seq = r.sequence;
            let data: Vec<u8> = owner.client.query_one("select checkpoint from history_imports", &[]).unwrap().get(0);
            let state: State = serde_json::from_slice(&data).unwrap();
            if state.phase == 3 {
                return seq;
            }
            assert!(!r.complete, "fixture must expose validation boundary");
        }
    }
    #[test]
    fn oversized_durable_checkpoint_payloads_are_rejected_before_materialization() {
        for mode in ["checkpoint", "replay", "continue", "finish"] {
            let Some(f) = Fixture::new() else { return };
            let mut owner = f.owner();
            let id = owner.begin(&f.source).unwrap();
            let mut sequence = 0;
            if mode != "checkpoint" {
                loop {
                    let receipt = owner.step(&f.source, id, sequence, ImportBudget::default()).unwrap();
                    sequence = receipt.sequence;
                    if mode != "finish" || receipt.complete {
                        break;
                    }
                }
            }
            let mut c = f.target.pool.get().unwrap();
            let sql = if mode == "checkpoint" {
                "select checkpoint from history_imports"
            } else {
                "select bookmark from history_checkpoints where sequence=$1"
            };
            let row = if mode == "checkpoint" {
                c.query_one(sql, &[]).unwrap()
            } else {
                c.query_one(sql, &[&sequence]).unwrap()
            };
            let mut bytes: Vec<u8> = row.get(0);
            bytes.resize(1024 * 1024, b' '); // Valid JSON padding must not bypass payload admission.
            if mode == "checkpoint" {
                c.execute("update history_imports set checkpoint=$1", &[&bytes]).unwrap();
            } else {
                let digest = Sha256::digest(&bytes).to_vec();
                c.execute(
                    "update history_checkpoints set bookmark=$1,digest=$2 where sequence=$3",
                    &[&bytes, &digest, &sequence],
                )
                .unwrap();
            }
            let result = if mode == "finish" {
                owner.finish_identity(&f.source, id)
            } else {
                owner
                    .step(
                        &f.source,
                        id,
                        if mode == "replay" { sequence - 1 } else { sequence },
                        ImportBudget::default(),
                    )
                    .map(|_| ())
            };
            let error = result.expect_err("oversized valid checkpoint JSON was admitted");
            let error = error.downcast_ref::<HistoryError>().expect("structured checkpoint admission error");
            assert_eq!(error.code, ErrorCode::CorruptObject);
            assert!(error.to_string().contains("checkpoint payload admission"), "{mode}: {error}");
        }
    }
    #[test]
    fn import_validation_rejects_corrupt_final_checkpoint() {
        let Some(f) = Fixture::new() else { return };
        let mut owner = f.owner();
        let id = owner.begin(&f.source).unwrap();
        let mut seq = 0;
        loop {
            let r = owner.step(&f.source, id, seq, ImportBudget::default()).unwrap();
            seq = r.sequence;
            if r.complete {
                break;
            }
        }
        let mut c = f.target.pool.get().unwrap();
        c.execute("update history_checkpoints set digest=decode(repeat('00',32),'hex') where sequence=$1", &[&seq])
            .unwrap();
        assert!(
            owner.finish_identity(&f.source, id).is_err(),
            "publication must authenticate the final durable bookmark"
        );
    }
    #[test]
    fn import_validation_rejects_corrupt_ambiguous_checkpoint_receipt() {
        let Some(f) = Fixture::new() else { return };
        let mut owner = f.owner();
        let id = owner.begin(&f.source).unwrap();
        let r = owner.step(&f.source, id, 0, ImportBudget::default()).unwrap();
        assert_eq!(r.sequence, 1);
        let mut c = f.target.pool.get().unwrap();
        c.batch_execute("update history_checkpoints set digest=decode(repeat('00',32),'hex')").unwrap();
        assert!(
            owner.step(&f.source, id, 0, ImportBudget::default()).is_err(),
            "ambiguous replay must authenticate the stored receipt"
        );
    }
    #[test]
    fn import_validation_rejects_cross_release_metadata_reference() {
        let Some(f) = Fixture::new() else { return };
        let mut owner = f.owner();
        let id = owner.begin(&f.source).unwrap();
        let seq = capture_fixture(&f, &mut owner, id);
        let mut c = f.target.pool.get().unwrap();
        c.batch_execute("update history_release_variants set metadata_object_id=(select metadata_object_id from history_release_variants where public_id=5) where public_id=3").unwrap();
        assert!(
            finish_pages(&mut owner, &f.source, id, seq).is_err(),
            "a valid object from a different release is not valid metadata for this variant"
        );
    }
    #[test]
    fn import_validation_rejects_unregistered_extra_object() {
        let Some(f) = Fixture::new() else { return };
        let mut owner = f.owner();
        let id = owner.begin(&f.source).unwrap();
        let seq = capture_fixture(&f, &mut owner, id);
        let mut c = f.target.pool.get().unwrap();
        c.execute("insert into history_objects(import_id,object_id,source_id,scope_release_id,object_kind,source_bookmark,locator_digest,byte_length,digest,validated) select $1,max(object_id)+1,0,0,'metadata','extra',$2,0,$3,true from history_objects",&[&&id[..],&Sha256::digest(b"extra").to_vec(),&Sha256::digest([]).to_vec()]).unwrap();
        assert!(
            finish_pages(&mut owner, &f.source, id, seq).is_err(),
            "extra durable objects are not part of the admitted checkpoint"
        );
    }

    #[test]
    fn import_validation_rejects_last_capture_page_chunk_corruption() {
        let Some(f) = Fixture::new() else { return };
        let mut owner = f.owner();
        let id = owner.begin(&f.source).unwrap();
        let seq = capture_fixture(&f, &mut owner, id);
        let mut c = f.target.pool.get().unwrap();
        c.batch_execute("update history_object_chunks set bytes=set_byte(bytes,0,get_byte(bytes,0)#1) where object_id=(select metadata_object_id from history_release_variants where public_id=5)").unwrap();
        assert!(
            finish_pages(&mut owner, &f.source, id, seq).is_err(),
            "newly captured chunks must be validated after their capture commit"
        );
    }

    #[test]
    fn owner_loss_is_terminal_and_recovery_replays_exact_committed_receipt() {
        let Some(f) = Fixture::new() else { return };
        let mut owner = f.owner();
        let id = owner.begin(&f.source).unwrap();
        assert!(
            ImportOwner::connect(f.config.clone(), f.expected.clone(), f.spool.path().into(), 4 * 1024 * 1024).is_err(),
            "a second importer cannot share ownership"
        );
        let receipt = owner.step(&f.source, id, 0, ImportBudget::default()).unwrap();
        assert_eq!(
            owner.step(&f.source, id, 0, ImportBudget::default()).unwrap(),
            receipt,
            "same-session ambiguous acknowledgment is exact"
        );
        let pid: i32 = owner.client.query_one("select pg_backend_pid()", &[]).unwrap().get(0);
        let mut c = f.target.pool.get().unwrap();
        assert!(c.query_one("select pg_terminate_backend($1)", &[&pid]).unwrap().get::<_, bool>(0));
        assert!(owner.step(&f.source, id, receipt.sequence, ImportBudget::default()).is_err());
        assert!(owner.terminal);
        assert!(owner.begin(&f.source).is_err(), "lost owners never reconnect");
        drop(owner);
        let mut recovered = f.owner();
        assert_eq!(recovered.begin(&f.source).unwrap(), id);
        assert_eq!(
            recovered.step(&f.source, id, 0, ImportBudget::default()).unwrap(),
            receipt,
            "new-owner ambiguous replay is exact"
        );
        finish_pages(&mut recovered, &f.source, id, receipt.sequence).unwrap();
        let next: i64 = c.query_one("select nextval(pg_get_serial_sequence('releases','id'))", &[]).unwrap().get(0);
        assert_eq!(next, 900);
        recovered.begin(&f.source).unwrap();
        let next: i64 = c.query_one("select nextval(pg_get_serial_sequence('releases','id'))", &[]).unwrap().get(0);
        assert_eq!(next, 901, "resume may not regress consumed allocator reservation");
    }
    #[test]
    fn frozen_cleanup_interruption_resumes_its_authenticated_committed_checkpoint() {
        for point in ["input.ready", "projection.ready", "identity", "before_remove_dir"] {
            let Some(f) = Fixture::with_frame_count(1) else { return };
            let mut owner = f.owner();
            let id = owner.begin(&f.source).unwrap();
            let fault = super::super::history_spool::testing::interrupt_at(point);
            let mut sequence = 0;
            loop {
                match owner.step(&f.source, id, sequence, ImportBudget::default()) {
                    Ok(receipt) => {
                        sequence = receipt.sequence;
                        assert!(!receipt.complete, "cleanup interruption did not fire");
                    }
                    Err(error) => {
                        assert!(error.to_string().contains("injected cleanup interruption"), "{point}: {error:#}");
                        break;
                    }
                }
                assert!(sequence < 1000);
            }
            drop(fault);
            drop(owner);
            let read_before = f.source.cell_io().bytes;
            let mut client = f.target.pool.get().unwrap();
            let row = client
                .query_one("select checkpoint_sequence,checkpoint from history_imports where import_id=$1", &[&&id[..]])
                .unwrap();
            let committed: i64 = row.get(0);
            assert!(committed > sequence, "cleanup must follow server commit");
            let state: State = serde_json::from_slice(&row.get::<_, Vec<u8>>(1)).unwrap();
            assert!(!state.cleanups.is_empty(), "durable checkpoint must authenticate cleanup locators");
            let mut resumed = f.owner();
            assert_eq!(resumed.begin(&f.source).unwrap(), id);
            finish_pages(&mut resumed, &f.source, id, committed).unwrap();
            assert_eq!(f.source.cell_io().bytes, read_before, "completed envelope must not be reimported");
            assert_eq!(std::fs::read_dir(f.spool.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn frozen_owner_recovery_resumes_capture_validation_projection_and_index() {
        let budget =
            ImportBudget { source_rows: 8, source_bytes: 512, mutations: 64, copy_bytes: 262144, chunk_bytes: 7 };
        for desired in [0u8, 1, 2, 3] {
            let Some(f) = Fixture::with_frozen(true) else { return };
            let mut owner = f.owner();
            let id = owner.begin(&f.source).unwrap();
            let mut seq = 0;
            let mut reached = false;
            for _ in 0..10000 {
                let r = owner.step(&f.source, id, seq, budget).unwrap();
                seq = r.sequence;
                let bytes: Vec<u8> =
                    owner.client.query_one("select checkpoint from history_imports", &[]).unwrap().get(0);
                let state: State = serde_json::from_slice(&bytes).unwrap();
                if state
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.row.key == 3 && p.frozen.as_ref().is_some_and(|v| v.phase == desired))
                {
                    reached = true;
                    break;
                }
                assert!(!r.complete, "requested recovery phase must be observable");
            }
            assert!(reached, "phase {desired}");
            let started = std::time::Instant::now();
            drop(owner);
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "paused decoder cancellation must join promptly"
            );
            let mut owner = f.owner();
            owner.begin(&f.source).unwrap();
            finish_pages(&mut owner, &f.source, id, seq).unwrap();
            let mut c = f.target.pool.get().unwrap();
            let ids: Vec<i64> = c
                .query("select public_id from history_catalog order by public_id", &[])
                .unwrap()
                .iter()
                .map(|r| r.get(0))
                .collect();
            assert_eq!(ids, [3, 5, 41]);
            assert_eq!(
                std::fs::read_dir(f.spool.path()).unwrap().count(),
                1,
                "owned frame spools cleaned after publication"
            );
        }
    }

    #[test]
    fn small_frozen_frames_share_admitted_pages_instead_of_one_phase_commit_each() {
        let count = 32;
        let Some(f) = Fixture::with_frame_count(count) else { return };
        let mut owner = f.owner();
        let id = owner.begin(&f.source).unwrap();
        let sequence = capture_fixture(&f, &mut owner, id);
        assert!(sequence < count as i64, "{count} small fully admitted frames used {sequence} capture commits");
        println!("{}", serde_json::json!({"case":"packed_small_frames","frames":count,"capture_pages":sequence}));
        finish_pages(&mut owner, &f.source, id, sequence).unwrap();
        let mut c = f.target.pool.get().unwrap();
        assert_eq!(
            c.query_one("select count(*) from history_release_variants where source_id=3", &[])
                .unwrap()
                .get::<_, i64>(0),
            count as i64
        );
    }

    #[test]
    fn request_admission_rejects_nonprogressing_value_byte_budget() {
        inspect_fixture(0, |client, id| {
            use super::super::history_catalog::{ReadBudget, Snapshot};
            let mut snapshot = Snapshot::open(client, id).unwrap();
            let mut cursor = snapshot
                .value_cursor(&Value::Inline { kind: crate::history_identity::CellType::Blob, bytes: vec![1] })
                .unwrap();
            assert!(
                snapshot
                    .read_value(&mut cursor, ReadBudget { rows: 1, bytes: 128, chunk_bytes: 1 }, |_| Ok(()))
                    .is_err(),
                "a nonempty value cannot accept a page with no payload admission"
            );
        });
    }
}
