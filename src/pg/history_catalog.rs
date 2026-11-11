//! Internal bounded inspection of an identity-ready historical catalog.
//! No HTTP, NZB, search or worker path is switched to this module. Byte sinks
//! must treat range output as provisional until its final digest succeeds.
use super::history_digest::DigestState;
use crate::history_identity::{CellType, Column, ImportId, Metadata, Value};
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy)]
pub struct ReadBudget {
    pub rows: u32,
    pub bytes: u64,
    pub chunk_bytes: u32,
}
impl Default for ReadBudget {
    fn default() -> Self {
        Self { rows: 4096, bytes: 4 * 1024 * 1024, chunk_bytes: 65536 }
    }
}
impl ReadBudget {
    fn check(self) -> Result<()> {
        ensure!(
            self.rows > 0 && self.bytes >= 128 && (1..=65536).contains(&self.chunk_bytes),
            "InsufficientBudget: positive inspection limits required"
        );
        Ok(())
    }
}
pub enum LookupPurpose {
    /// Privileged migration inspection, never a public authorization bypass.
    Raw,
    /// The caller supplies the same group policy used by its direct reader.
    Direct {
        group_blocked: fn(&str) -> bool,
    },
    Search,
}
pub struct Found {
    pub public_id: i64,
    pub source_id: i64,
    pub metadata: Metadata,
    pub header_object: Option<i64>,
    pub payload_object: Option<i64>,
}
pub enum Lookup {
    Missing,
    Blocked,
    Found(Found),
}
pub struct AliasFact {
    pub source_id: i64,
    pub original_signed_hash: i64,
    pub target_public_id: i64,
    pub dangling_in_source: bool,
    pub selected_source: Option<i64>,
}
#[derive(Clone)]
pub struct ObjectHandle {
    pub object_id: i64,
    pub source_id: i64,
    pub scope_release_id: i64,
    pub kind: String,
    pub bytes: u64,
    pub digest: [u8; 32],
}
pub struct ValueCursor {
    object: Option<i64>,
    inline: Option<Vec<u8>>,
    base: u64,
    length: u64,
    next: u64,
    digest: [u8; 32],
    hash: DigestState,
}
pub struct ReadProgress {
    pub bytes: u64,
    pub rows: u64,
    pub complete: bool,
}
pub struct Snapshot<'a> {
    tx: postgres::Transaction<'a>,
    id: ImportId,
    pub gate_version: i64,
}
impl<'a> Snapshot<'a> {
    pub fn open(client: &'a mut postgres::Client, id: ImportId) -> Result<Self> {
        let mut tx = client
            .build_transaction()
            .isolation_level(postgres::IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()?;
        let row=tx.query_opt("select state,checkpoint_sequence,expected_schema=current_schema(),identity_digest from history_imports where import_id=$1",&[&&id[..]])?.context("IncompleteImport: no registered historical import")?;
        ensure!(
            row.get::<_, String>(0) == "identity_ready"
                && row.get::<_, bool>(2)
                && row.get::<_, Option<Vec<u8>>>(3).is_some_and(|d| d.len() == 32),
            "IncompleteImport: internal inspection requires identity_ready"
        );
        Ok(Self { tx, id, gate_version: row.get(1) })
    }
    pub fn object(&mut self, id: i64) -> Result<ObjectHandle> {
        let row=self.tx.query_opt("select source_id,scope_release_id,object_kind,byte_length,digest,validated from history_objects where import_id=$1 and object_id=$2",&[&&self.id[..],&id])?.context("CorruptObject: referenced object is absent")?;
        ensure!(row.get::<_, bool>(5), "IncompleteImport: object is provisional");
        let length: i64 = row.get(3);
        ensure!(length >= 0, "CorruptObject: object length");
        let digest: Vec<u8> = row.get(4);
        let digest: [u8; 32] = digest.try_into().map_err(|_| anyhow::anyhow!("CorruptObject: object digest length"))?;
        Ok(ObjectHandle {
            object_id: id,
            source_id: row.get(0),
            scope_release_id: row.get(1),
            kind: row.get(2),
            bytes: length as u64,
            digest,
        })
    }
    fn metadata(&mut self, object: i64, source: i64, key: i64, budget: ReadBudget) -> Result<Metadata> {
        budget.check()?;
        let handle = self.object(object)?;
        ensure!(
            handle.source_id == source
                && handle.scope_release_id == key
                && matches!(handle.kind.as_str(), "metadata" | "frozen_metadata"),
            "CorruptObject: metadata source identity"
        );
        ensure!(
            handle.bytes <= 49152 && handle.bytes + 128 <= budget.bytes,
            "InsufficientBudget: metadata index admission"
        );
        let admitted = budget.rows.min(((budget.bytes - handle.bytes) / 64 - 1) as u32);
        ensure!(admitted > 0, "InsufficientBudget: metadata row admission");
        let rows=self.tx.query("select ordinal,byte_offset,bytes,digest from history_object_chunks where import_id=$1 and object_id=$2 order by ordinal limit $3",&[&&self.id[..],&object,&(i64::from(admitted)+1)])?;
        ensure!(rows.len() <= admitted as usize, "InsufficientBudget: metadata chunk count");
        let mut body = Vec::with_capacity(handle.bytes as usize);
        let mut ordinal = 0i64;
        for row in rows {
            let offset: i64 = row.get(1);
            let bytes: Vec<u8> = row.get(2);
            let digest: Vec<u8> = row.get(3);
            ensure!(
                row.get::<_, i64>(0) == ordinal
                    && offset >= 0
                    && offset as usize == body.len()
                    && body.len() + bytes.len() <= handle.bytes as usize
                    && Sha256::digest(&bytes).as_slice() == digest,
                "CorruptObject: metadata chunk continuity/digest"
            );
            body.extend_from_slice(&bytes);
            ordinal += 1;
        }
        ensure!(
            body.len() == handle.bytes as usize && Sha256::digest(&body).as_slice() == handle.digest,
            "CorruptObject: metadata object digest"
        );
        let metadata: Metadata = serde_json::from_slice(&body)?;
        ensure!(
            metadata.version == 1
                && metadata.source_id == source
                && metadata.source_key == key
                && metadata.fields.len() <= 16,
            "CorruptObject: metadata identity/version"
        );
        for (i, (column, _)) in metadata.fields.iter().enumerate() {
            ensure!(
                !metadata.fields[..i].iter().any(|(old, _)| old == column),
                "CorruptObject: duplicate metadata column"
            );
        }
        Ok(metadata)
    }
    pub fn lookup(&mut self, public_id: i64, purpose: LookupPurpose, budget: ReadBudget) -> Result<Lookup> {
        budget.check()?;
        ensure!(
            !matches!(purpose, LookupPurpose::Search),
            "UnsupportedSchema: historical search visibility is not ported"
        );
        let Some(row)=self.tx.query_opt("select v.source_id,v.metadata_object_id,v.frozen_header_object_id,v.frozen_payload_object_id from history_catalog c join history_release_variants v on v.import_id=c.import_id and v.source_id=c.selected_source_id and v.public_id=c.public_id where c.import_id=$1 and c.public_id=$2",&[&&self.id[..],&public_id])? else{return Ok(Lookup::Missing);};
        let source: i64 = row.get(0);
        let metadata = self.metadata(row.get(1), source, public_id, budget)?;
        if let LookupPurpose::Direct { group_blocked } = purpose {
            let group = metadata.field(Column::Group).context("CorruptObject: group field is missing")?;
            let kind = match group {
                Value::Inline { kind, .. } | Value::Bytes { kind, .. } => kind,
                _ => bail!("UnsupportedSchema: direct group policy requires source text"),
            };
            ensure!(*kind == CellType::Text, "UnsupportedSchema: direct group policy requires source text");
            let mut cursor = self.value_cursor(group)?;
            ensure!(cursor.length + 128 <= budget.bytes, "InsufficientBudget: group policy admission");
            let mut bytes = Vec::with_capacity(cursor.length as usize);
            let progress = self.read_value(&mut cursor, budget, |chunk| {
                bytes.extend_from_slice(chunk);
                Ok(())
            })?;
            ensure!(progress.complete, "InsufficientBudget: group policy chunk admission");
            let group = std::str::from_utf8(&bytes).context("UnsupportedSchema: group policy requires UTF-8")?;
            if group_blocked(group) {
                return Ok(Lookup::Blocked);
            }
        }
        Ok(Lookup::Found(Found {
            public_id,
            source_id: source,
            metadata,
            header_object: row.get(2),
            payload_object: row.get(3),
        }))
    }
    /// Diagnostic source-qualified fact lookup; this is not incoming routing.
    pub fn resolve_legacy(&mut self, source: i64, hash: i64) -> Result<Option<AliasFact>> {
        Ok(self.tx.query_opt("select a.target_public_id,v.public_id is null,c.selected_source_id from history_collection_aliases a left join history_release_variants v on v.import_id=a.import_id and v.source_id=a.source_id and v.public_id=a.target_public_id left join history_catalog c on c.import_id=a.import_id and c.public_id=a.target_public_id where a.import_id=$1 and a.source_id=$2 and a.original_signed_hash=$3",&[&&self.id[..],&source,&hash])?.map(|row|AliasFact{source_id:source,original_signed_hash:hash,target_public_id:row.get(0),dangling_in_source:row.get(1),selected_source:row.get(2)}))
    }
    pub fn object_cursor(&mut self, id: i64) -> Result<ValueCursor> {
        let object = self.object(id)?;
        Ok(ValueCursor {
            object: Some(id),
            inline: None,
            base: 0,
            length: object.bytes,
            next: 0,
            digest: object.digest,
            hash: Default::default(),
        })
    }
    pub fn value_cursor(&mut self, value: &Value) -> Result<ValueCursor> {
        match value {
            Value::Inline { bytes, .. } => {
                ensure!(bytes.len() <= 512, "CorruptObject: inline field exceeds format bound");
                Ok(ValueCursor {
                    object: None,
                    inline: Some(bytes.clone()),
                    base: 0,
                    length: bytes.len() as u64,
                    next: 0,
                    digest: Sha256::digest(bytes).into(),
                    hash: Default::default(),
                })
            }
            Value::Bytes { object_id, offset, length, digest, .. } => {
                let object = self.object(*object_id)?;
                ensure!(
                    offset.checked_add(*length).is_some_and(|end| end <= object.bytes),
                    "CorruptObject: field range exceeds object"
                );
                Ok(ValueCursor {
                    object: Some(*object_id),
                    inline: None,
                    base: *offset,
                    length: *length,
                    next: 0,
                    digest: *digest,
                    hash: Default::default(),
                })
            }
            _ => bail!("UnsupportedSchema: scalar has no byte cursor"),
        }
    }
    pub fn read_value(
        &mut self,
        cursor: &mut ValueCursor,
        budget: ReadBudget,
        mut sink: impl FnMut(&[u8]) -> Result<()>,
    ) -> Result<ReadProgress> {
        budget.check()?;
        let mut used = 0u64;
        let mut rows = 0u64;
        while cursor.next < cursor.length && rows < u64::from(budget.rows) && budget.bytes - used >= 128 {
            let n = (cursor.length - cursor.next).min(u64::from(budget.chunk_bytes)).min(budget.bytes - used - 128)
                as usize;
            if n == 0 {
                break;
            }
            let bytes = if let Some(inline) = &cursor.inline {
                inline[cursor.next as usize..cursor.next as usize + n].to_vec()
            } else {
                let object = cursor.object.context("CorruptObject: byte cursor object missing")?;
                let target = cursor.base.checked_add(cursor.next).context("object cursor overflow")?;
                let row=self.tx.query_opt("select byte_offset,octet_length(bytes)::bigint from history_object_chunks where import_id=$1 and object_id=$2 and byte_offset<=$3 order by byte_offset desc limit 1",&[&&self.id[..],&object,&i64::try_from(target)?])?.context("CorruptObject: byte cursor chunk missing")?;
                let start: i64 = row.get(0);
                let length: i64 = row.get(1);
                ensure!(
                    start >= 0 && length > 0 && target >= start as u64 && target < start as u64 + length as u64,
                    "CorruptObject: byte cursor chunk gap"
                );
                let inside = target - start as u64;
                let n = n.min((length as u64 - inside) as usize);
                let row=self.tx.query_one("select substring(bytes from $4+1 for $5) from history_object_chunks where import_id=$1 and object_id=$2 and byte_offset=$3",&[&&self.id[..],&object,&start,&i32::try_from(inside)?,&(n as i32)])?;
                let bytes: Vec<u8> = row.get(0);
                ensure!(bytes.len() == n, "CorruptObject: short object range");
                bytes
            };
            sink(&bytes)?;
            cursor.hash.update(&bytes)?;
            cursor.next += bytes.len() as u64;
            used += 128 + bytes.len() as u64;
            rows += 1;
        }
        let complete = cursor.next == cursor.length;
        if complete {
            ensure!(cursor.hash.finish()? == cursor.digest, "CorruptObject: inspected byte range digest");
        }
        Ok(ReadProgress { bytes: used, rows, complete })
    }
}
