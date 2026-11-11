//! Bounded binary COPY staging and set-based identity admission.
use crate::history_identity::{ErrorCode, HistoryError};
use anyhow::{Result, ensure};
use postgres::{Transaction, binary_copy::BinaryCopyInWriter, types::Type};

pub(crate) const ROW_OVERHEAD: u64 = 256;
#[derive(Default)]
pub(crate) struct Row {
    pub tag: i16,
    pub a: i64,
    pub b: i64,
    pub c: i64,
    pub d: i64,
    pub e: i64,
    pub text: String,
    pub x: Vec<u8>,
    pub y: Vec<u8>,
    pub z: Vec<u8>,
    pub flag: bool,
    pub f: i64,
    pub g: i64,
    pub extra: String,
}
impl Row {
    pub fn bytes(&self) -> u64 {
        ROW_OVERHEAD + (self.text.len() + self.x.len() + self.y.len() + self.z.len() + self.extra.len()) as u64
    }
    pub fn mutations(&self) -> u32 {
        if self.tag == 4 { 4 } else { 3 }
    }
}
#[derive(Default)]
pub(crate) struct Cache {
    copy: Option<postgres::Statement>,
}
fn execute(tx: &mut Transaction<'_>, sql: &str, id: &[u8; 32]) -> Result<u64> {
    Ok(tx.execute_typed(sql, &[(&&id[..], Type::BYTEA)])?)
}
fn query(tx: &mut Transaction<'_>, sql: &str, id: &[u8; 32]) -> Result<postgres::Row> {
    Ok(tx.query_typed_one(sql, &[(&&id[..], Type::BYTEA)])?)
}
pub(crate) struct Applied {
    pub round_trips: u64,
    pub mutations: u64,
    pub copy_bytes: u64,
}
pub(crate) fn apply(tx: &mut Transaction<'_>, id: &[u8; 32], rows: &[Row], cache: &mut Cache) -> Result<Applied> {
    if rows.is_empty() {
        return Ok(Applied { round_trips: 0, mutations: 0, copy_bytes: 0 });
    }
    let mut mutations = 0u64;
    let mut round_trips = 16;
    if cache.copy.is_none() {
        tx.batch_execute("create temporary table atlas_history_stage(tag smallint,a bigint,b bigint,c bigint,d bigint,e bigint,t text,x bytea,y bytea,z bytea,flag boolean,f bigint,g bigint,extra text) on commit delete rows")?;
        cache.copy = Some(tx.prepare("copy pg_temp.atlas_history_stage from stdin binary")?);
        round_trips += 2;
    }
    {
        let sink = tx.copy_in(cache.copy.as_ref().expect("prepared COPY"))?;
        let mut writer = BinaryCopyInWriter::new(
            sink,
            &[
                Type::INT2,
                Type::INT8,
                Type::INT8,
                Type::INT8,
                Type::INT8,
                Type::INT8,
                Type::TEXT,
                Type::BYTEA,
                Type::BYTEA,
                Type::BYTEA,
                Type::BOOL,
                Type::INT8,
                Type::INT8,
                Type::TEXT,
            ],
        );
        for row in rows {
            writer.write(&[
                &row.tag, &row.a, &row.b, &row.c, &row.d, &row.e, &row.text, &row.x, &row.y, &row.z, &row.flag, &row.f,
                &row.g, &row.extra,
            ])?;
        }
        mutations += 2 * writer.finish()?;
    }
    mutations += execute(
        tx,
        "insert into history_sources(import_id,source_id,kind,shard_number,paired_current_source,file_manifest_digest,schema_digest) select $1,a,t,nullif(b,-1)::integer,nullif(c,-1),x,y from atlas_history_stage where tag=7 on conflict do nothing",
        id,
    )?;
    mutations += execute(
        tx,
        "insert into history_objects(import_id,object_id,source_id,scope_release_id,object_kind,source_bookmark,locator_digest,byte_length) select $1,a,b,c,t,x,y,d from atlas_history_stage where tag=1 on conflict do nothing",
        id,
    )?;
    mutations += execute(
        tx,
        "insert into history_object_chunks(import_id,object_id,ordinal,byte_offset,bytes,digest) select $1,a,b,c,x,y from atlas_history_stage where tag=2 on conflict do nothing",
        id,
    )?;
    let conflict:bool=query(tx,"select exists(
        select 1 from atlas_history_stage s left join history_sources h on h.import_id=$1 and h.source_id=s.a where s.tag=7 and (h.source_id is null or (h.kind,h.shard_number,h.paired_current_source,h.file_manifest_digest,h.schema_digest) is distinct from (s.t,nullif(s.b,-1)::integer,nullif(s.c,-1),s.x,s.y))
        union all select 1 from atlas_history_stage s left join history_objects h on h.import_id=$1 and h.object_id=s.a where s.tag=1 and (h.object_id is null or (h.source_id,h.scope_release_id,h.object_kind,h.source_bookmark,h.locator_digest,h.byte_length) is distinct from(s.b,s.c,s.t,s.x,s.y,s.d))
        union all select 1 from atlas_history_stage s left join history_object_chunks h on h.import_id=$1 and h.object_id=s.a and h.ordinal=s.b where s.tag=2 and (h.object_id is null or (h.byte_offset,h.bytes,h.digest) is distinct from(s.c,s.x,s.y))
        union all select 1 from (select distinct on(a) * from atlas_history_stage where tag=3 order by a,flag desc) s left join history_objects h on h.import_id=$1 and h.object_id=s.a where (h.object_id is null or h.byte_length<>s.d or (h.digest is not null and h.digest<>s.x) or (h.validated and (not s.flag or (h.logical_rows,h.logical_values) is distinct from(nullif(s.b,-1),nullif(s.c,-1))))) union all select 1 from atlas_history_stage s join atlas_history_stage t on s.tag=3 and t.tag=3 and s.a=t.a where (s.d,s.x) is distinct from(t.d,t.x) or (s.flag and t.flag and (s.b,s.c) is distinct from(t.b,t.c)))",id)?.get(0);
    ensure!(!conflict, HistoryError::new(ErrorCode::SourceMismatch, "resumed object/source/chunk identity differs"));
    mutations += execute(
        tx,
        "update history_objects h set digest=s.x,validated=s.flag,logical_rows=nullif(s.b,-1),logical_values=nullif(s.c,-1) from (select distinct on(a) * from atlas_history_stage where tag=3 order by a,flag desc) s where h.import_id=$1 and h.object_id=s.a",
        id,
    )?;
    mutations += execute(
        tx,
        "update history_sources h set facts_object_id=s.b from atlas_history_stage s where s.tag=9 and h.import_id=$1 and h.source_id=s.a and (h.facts_object_id is null or h.facts_object_id=s.b)",
        id,
    )?;
    mutations += execute(
        tx,
        "insert into history_release_variants(import_id,source_id,public_id,metadata_object_id,frozen_header_object_id,frozen_payload_object_id) select $1,a,b,c,nullif(d,-1),nullif(e,-1) from atlas_history_stage where tag=4 on conflict do nothing",
        id,
    )?;
    // Detect unapproved current/current and cross-lineage frozen ownership.
    let conflict:bool=query(tx,"select exists(select 1 from atlas_history_stage s join history_release_variants v on v.import_id=$1 and v.public_id=s.b and v.source_id<>s.a join history_sources old on old.import_id=v.import_id and old.source_id=v.source_id join history_sources new on new.import_id=$1 and new.source_id=s.a where s.tag=4 and not ((old.kind='current' and new.kind='frozen' and new.paired_current_source=old.source_id) or (old.kind='frozen' and new.kind='current' and old.paired_current_source=new.source_id)))",id)?.get(0);
    ensure!(!conflict, HistoryError::new(ErrorCode::SourceMismatch, "conflicting public-ID source lineages"));
    mutations += execute(
        tx,
        "insert into history_collection_aliases(import_id,source_id,original_signed_hash,target_public_id) select $1,a,b,c from atlas_history_stage where tag=5 on conflict do nothing",
        id,
    )?;
    let conflict:bool=query(tx,"select exists(select 1 from atlas_history_stage s join history_release_variants v on v.import_id=$1 and v.source_id=s.a and v.public_id=s.b where s.tag=4 and (v.metadata_object_id,v.frozen_header_object_id,v.frozen_payload_object_id) is distinct from(s.c,nullif(s.d,-1),nullif(s.e,-1)) union all select 1 from atlas_history_stage s join history_collection_aliases h on h.import_id=$1 and h.source_id=s.a and h.original_signed_hash=s.b where s.tag=5 and h.target_public_id<>s.c)",id)?.get(0);
    ensure!(!conflict, HistoryError::new(ErrorCode::SourceMismatch, "resumed variant/alias content differs"));
    mutations += execute(
        tx,
        "insert into history_catalog(import_id,public_id,selected_source_id) select distinct on(s.b) $1,s.b,s.a from atlas_history_stage s join history_sources src on src.import_id=$1 and src.source_id=s.a where s.tag=4 order by s.b,(src.kind='current') desc,s.a on conflict(import_id,public_id) do update set selected_source_id=excluded.selected_source_id where (select kind from history_sources where import_id=$1 and source_id=excluded.selected_source_id)='current' and (select kind from history_sources where import_id=$1 and source_id=history_catalog.selected_source_id)='frozen'",
        id,
    )?;
    mutations += execute(
        tx,
        "insert into history_receipts(import_id,source_id,relation_code,relation_name,state,logical_rows,digest,owner_code) select distinct on(a,b) $1,a,b::integer,x,t,case when flag then c else null end,case when flag then y else null end,extra from atlas_history_stage where tag=8 order by a,b,flag desc on conflict(import_id,source_id,relation_code) do update set state=excluded.state,logical_rows=excluded.logical_rows,digest=excluded.digest where history_receipts.relation_name=excluded.relation_name and history_receipts.owner_code=excluded.owner_code and (history_receipts.state='pending' or (history_receipts.state,history_receipts.logical_rows,history_receipts.digest) is not distinct from(excluded.state,excluded.logical_rows,excluded.digest))",
        id,
    )?;
    mutations += execute(
        tx,
        "insert into history_frozen_resolutions(import_id,source_id,public_id,header_object_id,metadata_object_id,differing_columns,oracle_code) select $1,a,b,c,d,x,t from atlas_history_stage where tag=12 on conflict do nothing",
        id,
    )?;
    let conflict:bool=query(tx,"select exists(select 1 from atlas_history_stage s left join history_frozen_resolutions h on h.import_id=$1 and h.source_id=s.a and h.public_id=s.b where s.tag=12 and (h.source_id is null or (h.header_object_id,h.metadata_object_id,h.differing_columns,h.oracle_code) is distinct from(s.c,s.d,s.x,s.t)) union all select 1 from atlas_history_stage s left join history_sources h on h.import_id=$1 and h.source_id=s.a where s.tag=9 and (h.source_id is null or h.facts_object_id is distinct from s.b) union all select 1 from atlas_history_stage s left join history_receipts h on h.import_id=$1 and h.source_id=s.a and h.relation_code=s.b where s.tag=8 and (h.source_id is null or (h.relation_name,h.owner_code) is distinct from(s.x,s.extra) or (s.flag and (h.state,h.logical_rows,h.digest) is distinct from(s.t,s.c,s.y))))",id)?.get(0);
    ensure!(!conflict, HistoryError::new(ErrorCode::SourceMismatch, "frozen resolution receipt differs"));
    // Fixed count regardless of ordinary row/cell count; COPY protocol writes
    // are separately byte bounded. No destination round trip per source row.
    Ok(Applied { round_trips, mutations, copy_bytes: 21 + rows.iter().map(|r| r.bytes() - 139).sum::<u64>() })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(id: i64) -> Row {
        Row {
            tag: 7,
            a: id,
            b: -1,
            c: -1,
            text: "current".into(),
            x: vec![1; 32],
            y: vec![2; 32],
            ..Default::default()
        }
    }
    fn object(id: i64) -> Row {
        Row {
            tag: 1,
            a: id,
            b: 1,
            text: "metadata".into(),
            x: vec![id as u8],
            y: vec![id as u8; 32],
            ..Default::default()
        }
    }
    fn variant(source: i64, object: i64) -> Row {
        Row { tag: 4, a: source, b: 7, c: object, d: -1, e: -1, ..Default::default() }
    }
    fn target(body: impl FnOnce(&mut Transaction<'_>, &[u8; 32], &mut Cache)) {
        let Some(s) = crate::pg::testing::Schema::new() else { return };
        let mut c = s.pool.get().unwrap();
        let mut tx = c.transaction().unwrap();
        let id = [1u8; 32];
        tx.execute("insert into history_imports(import_id,source_fingerprint,contract_version,state,expected_database,expected_schema,expected_role,expected_owner,reserved_next_seq,checkpoint) values($1,$1,1,'loading','test','test','test','test',900,'')",&[&&id[..]]).unwrap();
        body(&mut tx, &id, &mut Cache::default());
    }
    #[test]
    fn rejects_same_page_unpaired_current_identity_collisions() {
        target(|tx, id, cache| {
            assert!(
                apply(tx, id, &[source(1), source(2), object(1), object(2), variant(1, 1), variant(2, 2)], cache)
                    .is_err(),
                "same-page current/current collision must fail"
            );
        });
    }
    #[test]
    fn rejects_conflicting_source_facts_pointer() {
        target(|tx, id, cache| {
            apply(tx, id, &[source(1), object(1), object(2), Row { tag: 9, a: 1, b: 1, ..Default::default() }], cache)
                .unwrap();
            assert!(
                apply(tx, id, &[Row { tag: 9, a: 1, b: 2, ..Default::default() }], cache).is_err(),
                "a conflicting facts pointer may not be silently ignored"
            );
        });
    }
    #[test]
    fn rejects_conflicting_receipt_identity() {
        target(|tx, id, cache| {
            apply(
                tx,
                id,
                &[
                    source(1),
                    Row {
                        tag: 8,
                        a: 1,
                        b: 1,
                        x: b"releases".to_vec(),
                        text: "pending".into(),
                        extra: "catalog".into(),
                        ..Default::default()
                    },
                ],
                cache,
            )
            .unwrap();
            assert!(
                apply(
                    tx,
                    id,
                    &[Row {
                        tag: 8,
                        a: 1,
                        b: 1,
                        x: b"different".to_vec(),
                        text: "pending".into(),
                        extra: "catalog".into(),
                        ..Default::default()
                    }],
                    cache
                )
                .is_err(),
                "a conflicting receipt may not be silently ignored"
            );
        });
    }
}
