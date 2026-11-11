//! Fenced, resumable conversion of captured historical catalog identities.
//! Publication ends at identity_ready. No public reader/native overlay is enabled.
use super::{
    StorageConfig,
    history_digest::DigestState,
    history_stage::{self, Row},
};
use crate::history_identity::{Column, Metadata, Value};
use crate::history_source::{CellRef, Descriptor, ReadBudget, Relation, RowCursor, Scalar, Source, SourceKind};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub use crate::history_identity::ImportId;
const FIXED_COPY: u64 = 131072;
const INLINE: u64 = 512;
const MAX_METADATA: usize = 49152;
#[derive(Clone)]
pub struct ExpectedDestination {
    pub database: String,
    pub schema: String,
    pub role: String,
    pub owner: String,
    pub server_version: i32,
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
    fn check(self) -> Result<()> {
        ensure!(
            self.source_rows > 0
                && self.source_bytes >= 512
                && self.mutations >= 16
                && self.copy_bytes >= FIXED_COPY + MAX_METADATA as u64 + 8192
                && (1..=65536).contains(&self.chunk_bytes),
            "InsufficientBudget: positive page limits and fixed protocol admission required"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
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
struct Runtime {
    root: PathBuf,
    quota: u64,
    spool: Option<super::history_spool::Spool>,
    validator: Option<super::history_validator::Validator>,
    completed: bool,
}
impl Runtime {
    fn ensure_spool(&mut self, source: &Source, cell: &Cell, key: i64, validated: bool) -> Result<()> {
        if self.spool.is_none() {
            self.spool = Some(super::history_spool::Spool::open(
                &self.root,
                source.fingerprint(),
                cell.reference.source_id,
                key,
                self.quota,
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
            "CorruptObject: recovered source prefix differs"
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
    fn committed(&mut self) {
        if self.completed {
            if let Some(mut spool) = self.spool.take() {
                spool.keep = false;
                spool.remove();
            }
            self.completed = false;
        }
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
    fn push(&mut self, row: Row) -> Result<()> {
        ensure!(self.room(row.mutations(), row.bytes()), "InsufficientBudget: mutation/COPY admission");
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
        ensure!(storage.is_postgres() && storage.schema == expected.schema, "DestinationMismatch: PostgreSQL schema");
        ensure!(
            super::valid_schema(&expected.schema)
                && !expected.staging_prefix.is_empty()
                && expected.schema.starts_with(&expected.staging_prefix),
            "DestinationMismatch: explicit staging prefix"
        );
        ensure!(
            quota > 0 && spool.is_absolute() && spool.is_dir(),
            "InsufficientBudget: explicit private spool directory and quota"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                std::fs::symlink_metadata(&spool)?.is_dir()
                    && std::fs::metadata(&spool)?.permissions().mode() & 0o077 == 0,
                "DestinationMismatch: spool directory must be private"
            );
        }
        let config = super::Pool::new(&storage)?.config.clone();
        ensure!(config.get_dbname() == Some(expected.database.as_str()), "DestinationMismatch: explicit database name");
        let mut client = config.connect(postgres::NoTls)?;
        verify_destination(&mut client, &expected)?;
        // The same session remains the only importer connection until dropped.
        client.query_one("select pg_advisory_lock(hashtext('atlas.indexer'),hashtext($1))", &[&expected.schema])?;
        client.batch_execute(&format!("set search_path to \"{}\"", expected.schema))?;
        verify_destination(&mut client, &expected)?;
        Ok(Self {
            client,
            expected,
            terminal: false,
            runtime: Runtime { root: spool, quota, spool: None, validator: None, completed: false },
            stage_cache: Default::default(),
        })
    }
    fn live(&self) -> Result<()> {
        ensure!(!self.terminal && !self.client.is_closed(), "LostOwner: historical import ownership session ended");
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
        if has_history {
            if let Some(row)=tx.query_opt("select import_id,source_fingerprint,expected_database,expected_schema,expected_role,expected_owner,reserved_next_seq from history_imports for update",&[])? {
                ensure!(row.get::<_,Vec<u8>>(0)==id&&row.get::<_,Vec<u8>>(1)==id&&row.get::<_,String>(2)==self.expected.database&&row.get::<_,String>(3)==self.expected.schema&&row.get::<_,String>(4)==self.expected.role&&row.get::<_,String>(5)==self.expected.owner&&row.get::<_,i64>(6)==source.manifest().reserved_next_id,"SourceMismatch: destination belongs to a different import");
                reserve(&mut tx,&self.expected.schema,source.manifest().reserved_next_id,false)?;
                tx.commit()?;return Ok(id);
            }
        }
        // Existing owner receipts are issued in the original main/ext copy
        // transaction. Statement guards invalidate them on later native DML;
        // this session already holds the exclusive registration fence.
        let main = source
            .manifest()
            .files
            .iter()
            .find(|f| f.kind == SourceKind::Main)
            .context("SourceMismatch: main source")?;
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
                    "SourceMismatch: companion receipt relation inventory"
                );
            }
        }
        let tables = tx.query(
            "select tablename::text from pg_tables where schemaname=$1 order by tablename",
            &[&self.expected.schema],
        )?;
        ensure!(tables.len() <= 256, "DestinationMismatch: unexpected staging schema");
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
            ensure!(super::valid_schema(&table), "DestinationMismatch: unexpected relation name");
            let predicate = if table == "meta" { " where key<>'next_seq' or value<>1" } else { "" };
            let nonempty: bool =
                tx.query_one(&format!("select exists(select 1 from \"{table}\"{predicate})"), &[])?.get(0);
            ensure!(!nonempty, "DestinationMismatch: populated target requires an exact paired import receipt");
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
            ensure!(done == expected, "DestinationMismatch: exact migration history required");
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
            ensure!(tx.execute("update history_companion_receipts set dirty=false,derived_allocator=$1 where owner_code='main_import' and receipt_digest=$2", &[&source.manifest().reserved_next_id, &&source.manifest().main_receipt[..]])? == 1, "SourceMismatch: companion allocator derivation");
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
        ensure!(source.fingerprint() == id, "SourceMismatch: import fingerprint");
        let mut tx = self.client.transaction()?;
        tx.batch_execute("set local synchronous_commit=on")?;
        let row=tx.query_one("select source_fingerprint,state,checkpoint_sequence,checkpoint from history_imports where import_id=$1 for update",&[&&id[..]])?;
        ensure!(
            row.get::<_, Vec<u8>>(0) == id && row.get::<_, String>(1) == "loading",
            "IncompleteImport: import is not loading"
        );
        let sequence: i64 = row.get(2);
        let checkpoint: Vec<u8> = row.get(3);
        if sequence == previous_sequence + 1 {
            let old = tx.query_one(
                "select bookmark from history_checkpoints where import_id=$1 and sequence=$2",
                &[&&id[..], &sequence],
            )?;
            let record: Checkpoint = serde_json::from_slice(&old.get::<_, Vec<u8>>(0))?;
            ensure!(record.state.as_bytes() == checkpoint, "SourceMismatch: ambiguous checkpoint content");
            tx.commit()?;
            return Ok(record.receipt);
        }
        ensure!(sequence == previous_sequence, "SourceMismatch: checkpoint sequence conflict");
        if sequence > 0 {
            let old = tx.query_one(
                "select bookmark,digest from history_checkpoints where import_id=$1 and sequence=$2",
                &[&&id[..], &sequence],
            )?;
            let bytes: Vec<u8> = old.get(0);
            let digest: Vec<u8> = old.get(1);
            ensure!(Sha256::digest(&bytes).as_slice() == digest, "CorruptObject: durable checkpoint digest");
            let old: Checkpoint = serde_json::from_slice(&bytes)?;
            ensure!(old.state.as_bytes() == checkpoint, "CorruptObject: durable bookmark mismatch");
        }
        let mut state: State = serde_json::from_slice(&checkpoint)?;
        let mut batch = Batch::new(budget);
        consume(source, &mut state, &mut batch, &mut self.runtime)?;
        let validation_trips = if state.phase == 3 { validate_page(&mut tx, &id, &mut state, &mut batch)? } else { 0 };
        self.runtime.sync_page()?;
        let applied = history_stage::apply(&mut tx, &id, &batch.rows, &mut self.stage_cache)?;
        let next = sequence.checked_add(1).context("checkpoint sequence overflow")?;
        let checkpoint = serde_json::to_vec(&state)?;
        ensure!(checkpoint.len() <= MAX_METADATA, "InsufficientBudget: fixed checkpoint admission");
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
            "CorruptObject: mutation accounting exceeded reservation"
        );
        let checkpoint_text = String::from_utf8(checkpoint.clone())?;
        let mut record;
        for _ in 0..4 {
            record = serde_json::to_vec(&Checkpoint { state: checkpoint_text.clone(), receipt: receipt.clone() })?;
            receipt.copy_bytes = applied.copy_bytes + checkpoint.len() as u64 + record.len() as u64 + 160;
        }
        record = serde_json::to_vec(&Checkpoint { state: checkpoint_text, receipt: receipt.clone() })?;
        ensure!(
            receipt.copy_bytes <= batch.copy_bytes && receipt.copy_bytes <= budget.copy_bytes,
            "InsufficientBudget: exact COPY/checkpoint bytes"
        );
        let digest = Sha256::digest(&record).to_vec();
        tx.execute("insert into history_checkpoints(import_id,sequence,bookmark,source_rows,source_bytes,decoded_bytes,callback_events,mutations,copy_bytes,digest) values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",&[&&id[..],&next,&record,&to_i64(receipt.source_rows)?,&to_i64(receipt.source_bytes)?,&to_i64(receipt.decoded_bytes)?,&to_i64(receipt.callback_events)?,&to_i64(receipt.mutations)?,&to_i64(receipt.copy_bytes)?,&digest])?;
        tx.execute(
            "update history_imports set checkpoint_sequence=$2,checkpoint=$3 where import_id=$1",
            &[&&id[..], &next, &checkpoint],
        )?;
        if let Err(error) = tx.commit() {
            self.terminal = true;
            return Err(error)
                .context("LostOwner: commit outcome requires a new fenced owner and durable checkpoint inspection");
        }
        self.runtime.committed();
        Ok(receipt)
    }
    pub fn finish_identity(&mut self, source: &Source, id: ImportId) -> Result<()> {
        self.live()?;
        source.check_unchanged()?;
        ensure!(source.fingerprint() == id, "SourceMismatch: import fingerprint");
        let mut tx = self.client.transaction()?;
        tx.batch_execute("set local synchronous_commit=on")?;
        let row =
            tx.query_one("select state,checkpoint from history_imports where import_id=$1 for update", &[&&id[..]])?;
        if row.get::<_, String>(0) == "identity_ready" {
            tx.commit()?;
            return Ok(());
        }
        let state: State = serde_json::from_slice(&row.get::<_, Vec<u8>>(1))?;
        ensure!(state.phase == 4, "IncompleteImport: paged validation is incomplete");
        let provisional: bool = tx
            .query_one(
                "select exists(select 1 from history_objects where import_id=$1 and not validated)",
                &[&&id[..]],
            )?
            .get(0);
        ensure!(!provisional, "IncompleteImport: provisional objects remain");
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
    let row=client.query_one("select current_database(),current_user,current_setting('server_version_num')::integer,(select pg_get_userbyid(nspowner)::text from pg_namespace where nspname=$1)",&[&e.schema])?;
    ensure!(
        row.get::<_, String>(0) == e.database
            && row.get::<_, String>(1) == e.role
            && row.get::<_, i32>(2) == e.server_version
            && row.get::<_, Option<String>>(3).as_deref() == Some(e.owner.as_str()),
        "DestinationMismatch: database/role/owner/server identity"
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
        "DestinationMismatch: unexpected release identity sequence"
    );
    let current = tx.query_one(&format!("select last_value,is_called from \"{schema}\".\"{name}\""), &[])?;
    let value: i64 = current.get(0);
    let called: bool = current.get(1);
    let next = if called { value.checked_add(1).context("allocator overflow")? } else { value };
    if fresh {
        ensure!(next == 1 && !called, "DestinationMismatch: unexpected existing native allocations");
    }
    ensure!(floor > 0, "SourceMismatch: invalid allocator reservation");
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
    ensure!(body.len() <= MAX_METADATA, "InsufficientBudget: bounded metadata index");
    if !cursor.started {
        batch.push(object(cursor.id, source, key, kind, bookmark, body.len() as u64))?;
        cursor.started = true;
    }
    while cursor.offset < body.len() as u64 && batch.room(13, 1024) {
        let n = (body.len() as u64 - cursor.offset)
            .min(u64::from(batch.budget.chunk_bytes))
            .min(batch.budget.copy_bytes - batch.copy_bytes - 4096) as usize;
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
fn consume(source: &Source, state: &mut State, batch: &mut Batch, runtime: &mut Runtime) -> Result<()> {
    let files = ordered(source);
    if let Some((source_id, key)) = state.cleanup.take() {
        super::history_spool::Spool::cleanup(&runtime.root, source.fingerprint(), source_id, key)?;
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
                    .min(u64::from(batch.budget.chunk_bytes))
                    .min(batch.budget.copy_bytes - batch.copy_bytes - 4096) as usize;
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
                    capture_pending(source, state, batch, file.source_id, file.kind, runtime)?;
                    if runtime.completed {
                        break;
                    }
                    if state.pending.is_some() {
                        break;
                    }
                    continue;
                }
                if file.kind == SourceKind::Main && state.relation > 0 {
                    state.file += 1;
                    state.relation = 0;
                    continue;
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
                                    bail!("UnsupportedSchema: alias keys and targets must be original signed integers")
                                }
                            }
                        };
                        let hash = get(Column::Alias)?;
                        let target = get(Column::Target)?;
                        ensure!(hash == row.key, "SourceMismatch: alias bookmark");
                        batch.push(Row { tag: 5, a: file.source_id, b: hash, c: target, ..Default::default() })?;
                        state.relation_digest.update(&hash.to_le_bytes())?;
                        state.relation_digest.update(&target.to_le_bytes())?;
                        state.relation_rows += 1;
                        state.after = Some(row.key);
                    } else {
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
                        let expected =
                            file.catalog_counts.as_ref().context("SourceMismatch: catalog count receipt missing")?;
                        ensure!(
                            state.relation_rows
                                == if state.relation == 1 { expected.releases } else { expected.collections },
                            "SourceMismatch: captured catalog count differs from content manifest"
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
            _ => bail!("CorruptObject: unsupported checkpoint phase"),
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
        let mut bytes = Vec::new();
        let end = source.read_cell(
            &cell.reference,
            cell.offset,
            ReadBudget {
                rows: 1,
                bytes: batch.source_left(),
                chunk_bytes: batch
                    .budget
                    .chunk_bytes
                    .min((batch.budget.copy_bytes - batch.copy_bytes - MAX_METADATA as u64 - 4096) as u32),
            },
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
    ensure!(body.len() <= MAX_METADATA, "InsufficientBudget: metadata index");
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
            "SourceMismatch: public ID exceeds imported allocator reservation"
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
        .context("UnsupportedSchema: frozen payload is not a byte cell")?
        .clone();
    ensure!(payload.offset == payload.reference.bytes, "IncompleteImport: frozen envelope capture is incomplete");
    let payload_id = payload.object.context("CorruptObject: frozen payload object missing")?;
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
        // A fresh grant owns the whole page, so compressed + expanded reads
        // and every provisional callback have exact per-page counters.
        if batch.source_bytes != 0 || batch.decoded_bytes != 0 || batch.source_rows != 0 || !batch.rows.is_empty() {
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
                runtime.quota,
                crate::history_stream::Config::default(),
            )?);
        }
        let progress = runtime
            .validator
            .as_mut()
            .expect("owned validator")
            .advance(batch.budget.source_bytes, batch.budget.source_rows.min(batch.budget.mutations - 2))?;
        batch.source_bytes += progress.usage.compressed;
        batch.decoded_bytes += progress.usage.expanded;
        batch.events += progress.usage.events;
        if let Some(result) = progress.result {
            let validated = result?;
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
        let projection = frozen.projection.as_ref().context("CorruptObject: validated projection receipt missing")?;
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
                .min(u64::from(batch.budget.chunk_bytes))
                .min(batch.budget.copy_bytes - batch.copy_bytes - 4096) as usize;
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
        ensure!(cursor.digest.finish()? == projection.digest, "CorruptObject: validated projection digest differs");
        batch.push(seal(cursor.id, projection.byte_length, projection.digest, true))?;
        frozen.phase = 3;
        return Ok(false);
    }
    if frozen.phase == 3 {
        let projection = frozen.projection.as_ref().context("CorruptObject: projection metadata missing")?;
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
            "SourceMismatch: frozen public ID exceeds allocator reservation"
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
        state.cleanup = Some((source_id, key));
        runtime.completed = true;
        return Ok(true);
    }
    bail!("CorruptObject: frozen continuation phase")
}

fn finish_validated_object(state: &mut State) -> Result<()> {
    if state.validate_active {
        ensure!(
            state.validate_offset == state.validate_length
                && state.validate_digest.finish()?.as_slice() == state.validate_expected,
            "CorruptObject: exact object length/digest"
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
            let rows=tx.query("select c.object_id,c.ordinal,c.byte_offset,octet_length(c.bytes)::bigint,c.digest,o.byte_length,o.digest,o.validated from history_object_chunks c join history_objects o using(import_id,object_id) where c.import_id=$1 and (c.object_id,c.ordinal)>=($2,$3) order by c.object_id,c.ordinal limit $4",&[&&id[..],&state.validate_object,&state.validate_ordinal,&limit])?;
            if rows.is_empty() {
                finish_validated_object(state)?;
                state.validate_part = 1;
                state.validate_object = 0;
                return Ok(1);
            }
            let (mut objects, mut ordinals, mut offsets, mut lengths) =
                (Vec::<i64>::new(), Vec::<i64>::new(), Vec::<i32>::new(), Vec::<i32>::new());
            let mut chosen = Vec::new();
            let mut left = batch.source_left();
            for row in rows {
                if left < 129 {
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
                ensure!(full > 0 && offset < full as u64, "CorruptObject: chunk continuation");
                let n = (full as u64 - offset).min(left - 128) as i32;
                objects.push(oid);
                ordinals.push(ordinal);
                offsets.push(offset as i32);
                lengths.push(n);
                left -= 128 + n as u64;
                chosen.push(row);
                if n as u64 + offset < full as u64 {
                    break;
                }
            }
            if chosen.is_empty() {
                return Ok(1);
            }
            let data=tx.query("select r.object_id,r.ordinal,substring(c.bytes from r.start_byte+1 for r.byte_count) from unnest($2::bigint[],$3::bigint[],$4::integer[],$5::integer[]) as r(object_id,ordinal,start_byte,byte_count) join history_object_chunks c on c.import_id=$1 and c.object_id=r.object_id and c.ordinal=r.ordinal order by r.object_id,r.ordinal",&[&&id[..],&objects,&ordinals,&offsets,&lengths])?;
            ensure!(data.len() == chosen.len(), "CorruptObject: missing captured chunks");
            for (index, (meta, data)) in chosen.iter().zip(&data).enumerate() {
                let oid: i64 = meta.get(0);
                let ordinal: i64 = meta.get(1);
                let byte_offset: i64 = meta.get(2);
                let full: i64 = meta.get(3);
                let digest: Vec<u8> = meta.get(4);
                ensure!(meta.get::<_, bool>(7), "IncompleteImport: provisional object during validation");
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
                    "CorruptObject: noncontiguous object chunks"
                );
                let bytes: Vec<u8> = data.get(2);
                ensure!(bytes.len() == lengths[index] as usize, "CorruptObject: short object chunk");
                state.validate_digest.update(&bytes)?;
                state.validate_chunk_digest.update(&bytes)?;
                state.validate_offset += bytes.len() as u64;
                state.validate_chunk_offset += bytes.len() as u64;
                batch.source_bytes += 128 + bytes.len() as u64;
                batch.source_rows += 1;
                if state.validate_chunk_offset == full as u64 {
                    ensure!(state.validate_chunk_digest.finish()?.as_slice() == digest, "CorruptObject: chunk digest");
                    state.validate_chunk_offset = 0;
                    state.validate_chunk_digest = DigestState::default();
                    state.validate_ordinal += 1;
                }
            }
            Ok(2)
        }
        1 => {
            let rows=tx.query("select object_id,byte_length,digest,validated,exists(select 1 from history_object_chunks c where c.import_id=o.import_id and c.object_id=o.object_id) from history_objects o where import_id=$1 and object_id>$2 order by object_id limit $3",&[&&id[..],&state.validate_object,&limit])?;
            if rows.is_empty() {
                state.validate_part = 2;
                return Ok(1);
            }
            for row in rows {
                let oid: i64 = row.get(0);
                let len: i64 = row.get(1);
                let digest: Vec<u8> = row.get(2);
                ensure!(row.get::<_, bool>(3), "IncompleteImport: provisional object");
                ensure!(
                    (len == 0 && !row.get::<_, bool>(4) && Sha256::digest([]).as_slice() == digest)
                        || (len > 0 && row.get::<_, bool>(4)),
                    "CorruptObject: missing/extra object chunks"
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
                tx.query("select public_id,selected_source_id from history_catalog where import_id=$1 and public_id>$2 order by public_id limit $3",&[&&id[..],&after,&limit])?
            } else {
                tx.query("select public_id,selected_source_id from history_catalog where import_id=$1 order by public_id limit $2",&[&&id[..],&limit])?
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
                tx.query(&format!("{query} and (a.source_id,a.original_signed_hash)>($2,$3) order by a.source_id,a.original_signed_hash limit $4"),&[&&id[..],&source,&key,&limit])?
            } else {
                tx.query(&format!("{query} order by a.source_id,a.original_signed_hash limit $2"), &[&&id[..], &limit])?
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
                tx.query(
                    &format!("{query} and (source_id,relation_code)>($2,$3) order by source_id,relation_code limit $4"),
                    &[&&id[..], &source, &code, &limit],
                )?
            } else {
                tx.query(&format!("{query} order by source_id,relation_code limit $2"), &[&&id[..], &limit])?
            };
            if rows.is_empty() {
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
                let source: i64 = row.get(0);
                let code: i32 = row.get(1);
                if code <= 3 {
                    ensure!(
                        row.get::<_, String>(4) == "complete",
                        "IncompleteImport: selected relation receipt is pending"
                    );
                }
                state.receipt_after = Some((source, code));
                batch.source_rows += 1;
                batch.source_bytes += 128;
            }
            Ok(1)
        }
        _ => bail!("CorruptObject: validation checkpoint phase"),
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
            .query_one("select current_database(),current_user,current_setting('server_version_num')::integer", &[])
            .unwrap();
        let expected = ExpectedDestination {
            database: row.get(0),
            schema: test.name.clone(),
            role: row.get(1),
            owner: row.get(1),
            server_version: row.get(2),
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
            .query_one("select current_database(),current_user,current_setting('server_version_num')::integer", &[])
            .unwrap();
        let expected = ExpectedDestination {
            database: row.get(0),
            schema: test.name.clone(),
            role: row.get(1),
            owner: row.get(1),
            server_version: row.get(2),
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
            .query_one("select current_database(),current_user,current_setting('server_version_num')::integer", &[])
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
            .query_one("select current_database(),current_user,current_setting('server_version_num')::integer", &[])
            .unwrap();
        let expected = ExpectedDestination {
            database: row.get(0),
            schema: test.name.clone(),
            role: row.get(1),
            owner: row.get(1),
            server_version: row.get(2),
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
}
