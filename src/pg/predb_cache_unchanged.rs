//! Read-only proof for one entire locked importer chunk. A failed proof leaves
//! every entry to the original sequential path, including its admission errors.
use std::mem::size_of;

use anyhow::Result;
use postgres::Transaction;

use super::{normalize_entry, PAGE, TEXT_BYTES};
use crate::predb::{import::{Counts, Entry}, keys::name_key, matcher::MAX_ROWS_PER_KEY};

const SCRATCH_BYTES: usize = 1024 * 1024;
// Stream normalization one entry/candidate at a time. Reserve for name_key's
// lowercase/regex copies and two simultaneous Rust lowercase comparisons. A
// title is <=4096 bytes; Unicode lowercase emits at most three four-byte chars
// per input scalar. Even allowing geometric String growth, 256KiB covers these
// transient capacities. Incoming valid titles are only <=255 Unicode scalars.
const NORMALIZATION_BYTES: usize = 256 * 1024;
// Charge the key array and the first read's bounded scalar rows/bookkeeping
// before allocating them. Driver SQL/parameter/result byte buffers are separate
// from the admitted candidate payload, as in the existing importer.
const FIXED_BYTES: usize = NORMALIZATION_BYTES + PAGE as usize
    * (size_of::<i64>() + 8 * size_of::<i64>() + 4 * size_of::<postgres::Row>());

struct Candidate {
    key: i64,
    id: i64,
    title: Box<str>,
    category: Option<Box<str>>,
    size_bytes: Option<i64>,
    files: Option<i64>,
    pre_at: Option<i64>,
    nuked: i64,
    reason: Option<Box<str>>,
}

pub(super) fn unchanged_chunk_in(
    tx: &mut Transaction<'_>, entries: &[Entry], top_before: Option<i64>,
) -> Result<Option<Counts>> {
    // The caller already holds the original title/key table locks and captured
    // completed certification. Do not use this proof to create certification.
    if entries.len() > PAGE as usize || FIXED_BYTES > SCRATCH_BYTES { return Ok(None) }
    let mut keys = Vec::with_capacity(PAGE as usize);
    for entry in entries {
        if let Some(normalized) = normalize_entry(entry) {
            let key = name_key(normalized.title);
            if !keys.contains(&key) { keys.push(key); }
        }
    }
    let mut counts = Counts { skipped: entries.len() as u64, ..Default::default() };
    if keys.is_empty() { return Ok(Some(counts)) }
    keys.sort_unstable();

    // Both reads walk only nominated (key,title_id) PK ranges with the same
    // 17-row sentinel, then title PK lookups. No unbounded text reaches the
    // client before candidate count, per-row admission and total byte admission.
    let extents = tx.query("select incoming.key,count(t.id)::bigint as rows,
        coalesce(sum(octet_length(t.title)::bigint),0)::bigint as title_bytes,
        coalesce(sum(octet_length(t.category)::bigint),0)::bigint as category_bytes,
        coalesce(sum(octet_length(t.nuke_reason)::bigint),0)::bigint as reason_bytes,
        coalesce(max(octet_length(t.title)::bigint+coalesce(octet_length(t.category)::bigint,0)),0)::bigint as max_text
        from unnest($1::bigint[]) as incoming(key)
        left join lateral (
            select t.id,t.title,t.category,t.nuke_reason
            from (select title_id from ext_predb_title_keys
                where key=incoming.key order by title_id limit 17) k
            join ext_predb_titles t on t.id=k.title_id
        ) t on true group by incoming.key order by incoming.key", &[&keys])?;
    if extents.len() != keys.len() { return Ok(None) }
    let mut total_rows = 0usize;
    let mut charged = FIXED_BYTES;
    for (row, key) in extents.iter().zip(&keys) {
        let count: i64 = row.try_get("rows")?;
        let max_text: i64 = row.try_get("max_text")?;
        if row.try_get::<_, i64>("key")? != *key || count <= 0
            || count > MAX_ROWS_PER_KEY as i64 || !(0..=TEXT_BYTES).contains(&max_text) {
            return Ok(None)
        }
        let Ok(count) = usize::try_from(count) else { return Ok(None) };
        // Descriptor storage includes Box/Option/scalars and the lookup key.
        // Also charge up to four Row slots per result for the driver's Vec
        // capacity (including its minimum growth), separately from driver bytes.
        let Some(descriptors) = count.checked_mul(size_of::<Candidate>() + 4 * size_of::<postgres::Row>()) else { return Ok(None) };
        let Some(next) = charged.checked_add(descriptors) else { return Ok(None) };
        charged = next;
        for field in ["title_bytes", "category_bytes", "reason_bytes"] {
            let bytes: i64 = row.try_get(field)?;
            let Ok(bytes) = usize::try_from(bytes) else { return Ok(None) };
            let Some(next) = charged.checked_add(bytes) else { return Ok(None) };
            charged = next;
        }
        if charged > SCRATCH_BYTES { return Ok(None) }
        total_rows += count;
    }
    if total_rows > PAGE as usize * (MAX_ROWS_PER_KEY + 1) { return Ok(None) }
    drop(extents);

    let rows = tx.query("select incoming.key,t.id,t.title,t.category,t.size_bytes,
        t.files,t.pre_at,t.nuked,t.nuke_reason
        from unnest($1::bigint[]) as incoming(key)
        cross join lateral (
            select t.id,t.title,t.category,t.size_bytes,t.files,t.pre_at,t.nuked,t.nuke_reason
            from (select title_id from ext_predb_title_keys
                where key=incoming.key order by title_id limit 17) k
            join ext_predb_titles t on t.id=k.title_id
        ) t order by incoming.key,t.id", &[&keys])?;
    // Locks make these reads stable. Any unexpected shape still gets the whole
    // original path; actual database/decoding errors always propagate via ?.
    if rows.len() != total_rows { return Ok(None) }
    let mut candidates = Vec::with_capacity(total_rows);
    // Recheck the exact actual payload before allocating any owned text. This
    // also keeps a future SQL change from silently exceeding the extent charge.
    let mut actual = FIXED_BYTES + total_rows * (size_of::<Candidate>() + 4 * size_of::<postgres::Row>());
    let mut previous = None;
    let mut rows_for_key = 0usize;
    for row in &rows {
        let key: i64 = row.try_get("key")?;
        let id: i64 = row.try_get("id")?;
        if keys.binary_search(&key).is_err() || previous.is_some_and(|old| old >= (key, id)) { return Ok(None) }
        rows_for_key = if previous.is_some_and(|(old, _)| old == key) { rows_for_key + 1 } else { 1 };
        if rows_for_key > MAX_ROWS_PER_KEY { return Ok(None) }
        previous = Some((key, id));
        let title: &str = row.try_get("title")?;
        let category: Option<&str> = row.try_get("category")?;
        let reason: Option<&str> = row.try_get("nuke_reason")?;
        if title.len() + category.map_or(0, str::len) > TEXT_BYTES as usize { return Ok(None) }
        for text in [Some(title), category, reason].into_iter().flatten() {
            let Some(next) = actual.checked_add(text.len()) else { return Ok(None) };
            actual = next;
        }
    }
    if actual != charged || actual > SCRATCH_BYTES { return Ok(None) }
    for row in rows {
        // Box<str> has exactly the admitted byte extent, with no spare String
        // capacity. All variable owned allocations were charged above.
        candidates.push(Candidate {
            key: row.try_get("key")?, id: row.try_get("id")?,
            title: Box::from(row.try_get::<_, &str>("title")?),
            category: row.try_get::<_, Option<&str>>("category")?.map(Box::from),
            size_bytes: row.try_get("size_bytes")?, files: row.try_get("files")?,
            pre_at: row.try_get("pre_at")?, nuked: row.try_get("nuked")?,
            reason: row.try_get::<_, Option<&str>>("nuke_reason")?.map(Box::from),
        });
    }
    for entry in entries {
        let Some(normalized) = normalize_entry(entry) else { continue };
        let key = name_key(normalized.title);
        let start = candidates.partition_point(|row| row.key < key);
        let lower = normalized.title.to_lowercase();
        // Keep original title-ID order and Rust's context-sensitive Unicode
        // lowercase; SQL lower/collation and character-wise folding differ.
        let Some(row) = candidates[start..].iter().take_while(|row| row.key == key)
            .find(|row| row.title.to_lowercase() == lower) else { return Ok(None) };
        if (row.category.is_none() && normalized.category.is_some())
            || (row.size_bytes.is_none() && entry.size_bytes.is_some())
            || (row.files.is_none() && entry.files.is_some())
            || (row.pre_at.is_none() && entry.pre_at.is_some())
            || (entry.nuked != 0 && (row.nuked != entry.nuked
                || normalized.reason.is_some_and(|reason| row.reason.as_deref() != Some(reason)))) {
            return Ok(None)
        }
        if top_before.is_some_and(|before| row.id <= before) {
            counts.min_known_id = Some(counts.min_known_id.map_or(row.id, |old| old.min(row.id)));
        }
    }
    Ok(Some(counts))
}
