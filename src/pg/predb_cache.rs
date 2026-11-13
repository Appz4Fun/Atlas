//! Derived PreDB matcher keys. Callers supply the original owner's transaction;
//! no connection, fallback, provider request or transaction commit occurs here.
use anyhow::{Result, ensure};
use postgres::{GenericClient, Transaction};
use crate::predb::{keys::title_keys, matcher::{self, Evidence, Match, TitleRow}};

const PAGE: i64 = 512;
const TEXT_BYTES: i64 = 4096;
#[derive(Debug)]
pub(crate) struct Lookup { pub(crate) found: Option<Match>, pub(crate) revision: i64 }
#[derive(Debug)]
pub(crate) enum LookupStatus { Pending, Ready(Lookup) }
#[derive(Debug)]
pub(crate) struct StaleRead;
impl std::fmt::Display for StaleRead { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("PreDB matcher revision changed before settlement") } }
impl std::error::Error for StaleRead {}
#[derive(Debug)]
pub(crate) struct Built { pub(crate) revision: i64, pub(crate) titles: i64, pub(crate) keys: i64 }

#[derive(Debug)]
pub(crate) enum BuildStatus { Pending { titles: i64, keys: i64 }, Ready(Built) }

/// One resumable page under the original owner's guarded RC writer transaction.
/// Source mutations discard the build cursor; completed indexes are never served
/// until exact EOF. There is no catalog-size ceiling or restart on each page.
pub(crate) fn advance_in(tx: &mut Transaction<'_>, page_titles: u16) -> Result<BuildStatus> {
    ensure!(page_titles > 0 && i64::from(page_titles) <= PAGE, "invalid PreDB build page quota");
    ensure!(tx.query_one("select current_setting('transaction_isolation')", &[])?.try_get::<_, String>(0)? == "read committed", "PreDB rebuild requires READ COMMITTED");
    tx.batch_execute("lock table ext_predb_titles in share mode; lock table ext_predb_title_keys in access exclusive mode")?;
    if let Some(row) = tx.query_opt("select version,revision,title_rows,key_rows from ext_predb_key_meta where singleton=1", &[])? {
        ensure!(row.try_get::<_, i64>(0)? == 1, "unsupported PreDB matcher key version");
        return Ok(BuildStatus::Ready(Built { revision: row.try_get(1)?, titles: row.try_get(2)?, keys: row.try_get(3)? }));
    }
    let build = tx.query_opt("select after_id,title_rows,key_rows from ext_predb_key_build where singleton=1", &[])?;
    let (after, mut titles, mut keys) = if let Some(row) = build {
        (row.try_get::<_,Option<i64>>(0)?, row.try_get::<_,i64>(1)?, row.try_get::<_,i64>(2)?)
    } else {
        tx.batch_execute("delete from ext_predb_title_keys; insert into ext_predb_key_build(singleton,title_rows,key_rows) values(1,0,0)")?;
        (None, 0, 0)
    };
    let limit = i64::from(page_titles);
    let rows = tx.query("select id, octet_length(title)::bigint <= $3 as admitted,
        case when octet_length(title)::bigint <= $3 then title end as title
        from ext_predb_titles where ($1::bigint is null or id>$1) order by id limit $2", &[&after, &limit, &TEXT_BYTES])?;
    let eof = rows.len() < usize::from(page_titles);
    let (mut key_values, mut ids) = (Vec::with_capacity(rows.len()*3), Vec::with_capacity(rows.len()*3));
    let mut next = after;
    for row in rows {
        ensure!(row.try_get::<_, bool>("admitted")?, "PreDB title exceeds matcher admission");
        let id = row.try_get::<_, i64>("id")?;
        let title = row.try_get::<_, String>("title")?;
        for key in title_keys(&title) { key_values.push(key); ids.push(id); }
        next = Some(id); titles = titles.checked_add(1).ok_or_else(||anyhow::anyhow!("PreDB title count overflow"))?;
    }
    keys = keys.checked_add(i64::try_from(tx.execute("insert into ext_predb_title_keys(key,title_id)
        select * from unnest($1::bigint[],$2::bigint[]) on conflict do nothing", &[&key_values, &ids])?)?).ok_or_else(||anyhow::anyhow!("PreDB key count overflow"))?;
    if !eof {
        tx.execute("update ext_predb_key_build set after_id=$1,title_rows=$2,key_rows=$3 where singleton=1", &[&next,&titles,&keys])?;
        return Ok(BuildStatus::Pending { titles, keys });
    }
    let revision: i64 = tx.query_one("select nextval('ext_predb_key_revision')", &[])?.try_get(0)?;
    tx.execute("insert into ext_predb_key_meta(singleton,version,revision,top_id,title_rows,key_rows) values(1,1,$1,$2,$3,$4)", &[&revision,&next,&titles,&keys])?;
    tx.batch_execute("delete from ext_predb_key_build")?;
    Ok(BuildStatus::Ready(Built { revision, titles, keys }))
}

#[derive(Debug)]
pub(crate) enum ImportStatus { Pending, Applied { counts: crate::predb::import::Counts, revision: i64 } }

/// Cache-only feed import. The same literal key-collision checks and update
/// policy as the SQLite importer run inside the original owner's RC transaction.
/// Never starts a provider request, advances a release cursor or prunes a row.
pub(crate) fn import_in(tx: &mut Transaction<'_>, source: i64, entries: &[crate::predb::import::Entry], now: i64, cursor: Option<(i64,i64)>) -> Result<ImportStatus> {
    use crate::predb::{import::{Counts, MAX_CATEGORY, MAX_NUKE_REASON}, keys::{valid_title,name_key}};
    ensure!(entries.len() <= PAGE as usize, "PreDB import page exceeds admission");
    ensure!(tx.query_one("select current_setting('transaction_isolation')", &[])?.try_get::<_, String>(0)? == "read committed", "PreDB import requires READ COMMITTED");
    tx.batch_execute("lock table ext_predb_titles in share row exclusive mode; lock table ext_predb_title_keys in access exclusive mode")?;
    let Some(meta) = tx.query_opt("select version,top_id,title_rows,key_rows from ext_predb_key_meta where singleton=1", &[])? else { return Ok(ImportStatus::Pending) };
    ensure!(meta.try_get::<_,i64>(0)? == 1, "unsupported PreDB matcher key version");
    let mut top: Option<i64> = meta.try_get(1)?;
    let top_before = top;
    let mut titles: i64 = meta.try_get(2)?;
    let mut keys: i64 = meta.try_get(3)?;
    let mut counts = Counts::default();
    for entry in entries {
        let title = entry.title.trim();
        if !valid_title(title) { counts.skipped += 1; continue }
        let candidates = rows_for_key(tx, name_key(title))?;
        ensure!(candidates.len() <= matcher::MAX_ROWS_PER_KEY, "PreDB importer candidate key exceeds admission");
        let lower = title.to_lowercase();
        let id = candidates.into_iter().find(|row| row.title.to_lowercase() == lower).map(|row| row.id);
        let category = entry.category.as_deref().filter(|c| !c.is_empty() && c.len() <= MAX_CATEGORY);
        let reason = entry.nuke_reason.as_deref().filter(|r| !r.is_empty()).map(|r| {
            let mut end = r.len().min(MAX_NUKE_REASON); while !r.is_char_boundary(end) { end -= 1; } &r[..end]
        });
        let Some(id) = id else {
            let id: i64 = tx.query_one("insert into ext_predb_titles(title,source,category,size_bytes,files,pre_at,nuked,nuke_reason,added_at)
                values($1,$2,$3,$4,$5,$6,$7,$8,$9) returning id", &[&title,&source,&category,&entry.size_bytes,&entry.files,&entry.pre_at,&entry.nuked,&if entry.nuked != 0 { reason } else { None },&now])?.try_get(0)?;
            for key in title_keys(title) { keys += i64::try_from(tx.execute("insert into ext_predb_title_keys(key,title_id) values($1,$2) on conflict do nothing", &[&key,&id])?)?; }
            titles += 1; top = Some(top.map_or(id, |old| old.max(id))); counts.inserted += 1;
            continue;
        };
        if top_before.is_some_and(|before| id <= before) { counts.min_known_id = Some(counts.min_known_id.map_or(id, |old| old.min(id))); }
        let mut changed = tx.execute("update ext_predb_titles set category=coalesce(category,$2),size_bytes=coalesce(size_bytes,$3),files=coalesce(files,$4),pre_at=coalesce(pre_at,$5)
            where id=$1 and ((category is null and $2::text is not null) or (size_bytes is null and $3::bigint is not null) or (files is null and $4::bigint is not null) or (pre_at is null and $5::bigint is not null))", &[&id,&category,&entry.size_bytes,&entry.files,&entry.pre_at])?;
        if entry.nuked != 0 { changed += tx.execute("update ext_predb_titles set nuked=$2,nuke_reason=coalesce($3,nuke_reason)
            where id=$1 and (nuked != $2 or ($3::text is not null and nuke_reason is distinct from $3))", &[&id,&entry.nuked,&reason])?; }
        if changed > 0 { counts.updated += 1; } else { counts.skipped += 1; }
    }
    tx.execute("insert into ext_predb_meta(key,value) values('titles',$1) on conflict(key) do update set value=excluded.value", &[&titles])?;
    if counts.inserted > 0 { if let Some((page,floor)) = cursor {
        ensure!(tx.execute("update ext_predb_feeds set resume_page=$1,gap_floor=$2 where source=$3", &[&page,&floor,&source])? == 1, "PreDB feed cursor row unavailable");
    } }
    let revision: i64 = tx.query_one("select nextval('ext_predb_key_revision')", &[])?.try_get(0)?;
    tx.batch_execute("delete from ext_predb_key_meta; delete from ext_predb_key_build")?;
    tx.execute("insert into ext_predb_key_meta(singleton,version,revision,top_id,title_rows,key_rows) values(1,1,$1,$2,$3,$4)", &[&revision,&top,&titles,&keys])?;
    Ok(ImportStatus::Applied { counts, revision })
}

/// One admitted parsed feed page commits atomically. Internal key batches keep
/// their <=512-entry bound, but none can commit without the page's aggregate
/// counts and continuation. An interrupted transaction leaves no partial page.
pub(crate) fn import_page_in(tx:&mut Transaction<'_>,source:i64,entries:&[crate::predb::import::Entry],now:i64,cursor:Option<(i64,i64)>)->Result<ImportStatus> {
    ensure!(entries.len()<=5000,"PreDB parsed page exceeds row admission");
    let mut bytes=0usize;
    for entry in entries {
        for text in [Some(entry.title.as_str()),entry.category.as_deref(),entry.nuke_reason.as_deref()].into_iter().flatten() {
            bytes=bytes.checked_add(text.len()).ok_or_else(||anyhow::anyhow!("PreDB parsed page byte overflow"))?;
        }
    }
    ensure!(bytes<=8*1024*1024,"PreDB parsed page exceeds byte admission");
    let Some(mut revision)=certified_revision(tx)? else {return Ok(ImportStatus::Pending)};
    let mut total=crate::predb::import::Counts::default();
    for (index,chunk) in entries.chunks(PAGE as usize).enumerate() {
        let status=import_in(tx,source,chunk,now,None)?;
        let ImportStatus::Applied{counts,revision:next}=status else {
            ensure!(index==0,"PreDB page lost key coverage after writes");
            return Ok(ImportStatus::Pending)
        };
        revision=next;total.inserted+=counts.inserted;total.updated+=counts.updated;total.skipped+=counts.skipped;
        if let Some(id)=counts.min_known_id {total.min_known_id=Some(total.min_known_id.map_or(id,|old|old.min(id)));}
    }
    if total.inserted>0 {if let Some((page,floor))=cursor {
        ensure!(tx.execute("update ext_predb_feeds set resume_page=$1,gap_floor=$2 where source=$3",&[&page,&floor,&source])?==1,"PreDB feed cursor row unavailable");
    }}
    ensure!(tx.execute("update ext_predb_feeds set inserted=inserted+$1,updated=updated+$2 where source=$3",&[&i64::try_from(total.inserted)?,&i64::try_from(total.updated)?,&source])?==1,"PreDB feed counter row unavailable");
    Ok(ImportStatus::Applied{counts:total,revision})
}

/// Late matching has a locked completed revision in its RC owner transaction.
/// Reuse the actual matcher and all competing title candidates; a nominated
/// hash or inner key cannot authorize a match by itself.
pub(crate) fn match_checked_in(tx:&mut Transaction<'_>,revision:i64,evidence:&Evidence<'_>)->Result<Option<Match>> {
    verify_read_in(tx,revision)?;
    matcher::find_with(evidence,|key|rows_for_key(tx,key))
}

/// Called by the native worker before PreDB visits, outside any owner read lock.
pub(crate) fn advance(owner: &std::sync::Arc<super::indexer_runtime::Owner>, page_titles:u16)->Result<BuildStatus> {
    super::service_cache::Cache::new(owner)?.predb_advance(page_titles)
}

/// One immutable read snapshot; settlement must recheck the returned revision.
/// No certification means unavailable, including a truly empty but unbuilt DB.
pub(crate) fn find_in(tx: &mut Transaction<'_>, evidence: &Evidence<'_>) -> Result<LookupStatus> {
    let isolation: String = tx.query_one("select current_setting('transaction_isolation')", &[])?.try_get(0)?;
    ensure!(isolation == "repeatable read" || isolation == "serializable", "PreDB matcher requires a fixed read snapshot");
    let Some(revision) = certified_revision(tx)? else { return Ok(LookupStatus::Pending) };
    let found = matcher::find_with(evidence, |key| rows_for_key(tx,key))?;
    Ok(LookupStatus::Ready(Lookup { found, revision }))
}
fn certified_revision(client: &mut impl GenericClient) -> Result<Option<i64>> {
    let Some(row) = client.query_opt("select version,revision from ext_predb_key_meta where singleton=1", &[])? else { return Ok(None) };
    ensure!(row.try_get::<_,i64>(0)? == 1, "unsupported PreDB matcher key version");
    Ok(Some(row.try_get(1)?))
}
/// Locks certification through settlement. A concurrent title import either
/// completes before this check (stale result refuses) or invalidates after commit.
pub(crate) fn verify_read_in(tx: &mut Transaction<'_>, expected: i64) -> Result<()> {
    let Some(row) = tx.query_opt("select version,revision from ext_predb_key_meta where singleton=1 for share", &[])? else { return Err(StaleRead.into()) };
    if row.try_get::<_,i64>(0)? != 1 || row.try_get::<_,i64>(1)? != expected { return Err(StaleRead.into()) }
    Ok(())
}
fn rows_for_key(client: &mut impl GenericClient, key: i64) -> Result<Vec<TitleRow>> {
    let rows = client.query("select t.id,octet_length(t.title)::bigint+coalesce(octet_length(t.category)::bigint,0)<=$2 as admitted,
        case when octet_length(t.title)::bigint+coalesce(octet_length(t.category)::bigint,0)<=$2 then t.title end as title,
        case when octet_length(t.title)::bigint+coalesce(octet_length(t.category)::bigint,0)<=$2 then t.category end as category,
        t.size_bytes,t.pre_at,t.nuked from ext_predb_title_keys k join ext_predb_titles t on t.id=k.title_id
        where k.key=$1 order by t.id limit 17", &[&key,&TEXT_BYTES])?;
    rows.into_iter().map(|row| { ensure!(row.try_get::<_,bool>("admitted")?, "PreDB candidate exceeds matcher admission");
        Ok(TitleRow { id:row.try_get("id")?,title:row.try_get("title")?,category:row.try_get("category")?,size_bytes:row.try_get("size_bytes")?,pre_at:row.try_get("pre_at")?,nuked:row.try_get("nuked")? }) }).collect()
}
#[cfg(test)]
#[path="predb_cache_tests.rs"]
mod tests;
