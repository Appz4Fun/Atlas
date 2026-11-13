//! Resumable late PreDB nomination over fresh native releases. Every page and
//! final rerun/cursor switch is one original-owner RC transaction. Candidate
//! keys nominate only; the same literal/hash rules recheck actual row text.
use anyhow::{Result,ensure};
use postgres::Transaction;
use crate::predb::{keys::{name_key,hash_key,title_keys,md5_hex,sha1_hex},matcher::{is_full_title,literally},late::{LATE_DELAY_SECS,LATE_MAX_RERUNS}};
use crate::naming::{clean_candidate,identity_key};
use super::predb::Service;
const TEXT_BYTES:i64=64*1024*1024;
const CANDIDATES:i64=64;

#[derive(Debug,Default)]
pub(crate) struct LateReport {pub(crate) indexed:usize,pub(crate) titles:usize,pub(crate) reruns:usize,pub(crate) overflow:usize,pub(crate) pending:bool,pub(crate) budget_exhausted:bool}
impl Service {
    /// Actual native maintenance entry point. Config-off/zero stays idle;
    /// monotonic time admits at most one configured title budget per second.
    /// The elapsed page budget never marks an incomplete title completed.
    pub(crate) fn maintenance(&self,cfg:&crate::config::PredbConfig,now:i64)->Result<LateReport> {
        let result=self.maintenance_inner(cfg,now);self.late_result(&result);result
    }
    fn late_result(&self,result:&Result<LateReport>) {
        if result.as_ref().is_err_and(|error|!crate::pg::outage::is_attempt_ended(error)) {
            self.counters.late_errors.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
        }
    }
    fn maintenance_inner(&self,cfg:&crate::config::PredbConfig,now:i64)->Result<LateReport> {
        if !cfg.enabled || cfg.late_per_second<=0 {return Ok(LateReport::default())}
        let started=std::time::Instant::now();
        {
            let mut at=self.late_at.lock().map_err(|_|anyhow::anyhow!("native PreDB late lane ended"))?;
            if at.is_some_and(|last|started.saturating_duration_since(last)<std::time::Duration::from_secs(1)) {return Ok(LateReport::default())}
            *at=Some(started);
        }
        let mut report=self.late_tick_inner(now,32)?;
        for _ in report.titles..usize::try_from(cfg.late_per_second.min(1000))? {
            if report.pending || started.elapsed()>=std::time::Duration::from_millis(200) {break}
            let page=self.cache.with(|tx|late_page(tx,now))?;
            self.counters.page(&page);
            report.titles+=page.titles;report.reruns+=page.reruns;report.overflow+=page.overflow;report.pending=page.pending;report.budget_exhausted|=page.budget_exhausted;
            if page.titles==0 {break}
        }
        Ok(report)
    }
    /// The standard worker calls this independently of network polling. Work is
    /// bounded to name_rows fresh names plus one <=64-candidate title page.
    pub(crate) fn late_tick(&self,now:i64,name_rows:u16)->Result<LateReport> {
        let result=self.late_tick_inner(now,name_rows);self.late_result(&result);result
    }
    fn late_tick_inner(&self,now:i64,name_rows:u16)->Result<LateReport> {
        ensure!((1..=32).contains(&name_rows),"PreDB late name page admission");
        let mut report=LateReport::default();
        for _ in 0..name_rows {
            if !self.cache.with(index_one)? {break} report.indexed+=1;
        }
        let page=self.cache.with(|tx|late_page(tx,now))?;
        self.counters.page(&page);
        report.titles=page.titles;report.reruns=page.reruns;report.overflow=page.overflow;report.pending=page.pending;report.budget_exhausted=page.budget_exhausted;
        Ok(report)
    }
}
fn checked_names(tx:&mut Transaction<'_>,id:i64,lock:bool)->Result<Option<(String,Option<String>,Option<i64>,i64)>> {
    let select="select original_name is not null and octet_length(original_name)::bigint+coalesce(octet_length(display_name)::bigint,0)<=$2 as admitted,
      case when octet_length(original_name)::bigint+coalesce(octet_length(display_name)::bigint,0)<=$2 then original_name end,
      case when octet_length(original_name)::bigint+coalesce(octet_length(display_name)::bigint,0)<=$2 then display_name end,
      name_source,size from releases where id=$1";
    let sql=if lock {format!("{select} for update")}else{select.to_string()};
    let Some(row)=tx.query_opt(&sql,&[&id,&TEXT_BYTES])? else {return Ok(None)};
    ensure!(row.try_get::<_,bool>(0)?,"native PreDB names exceed read admission");
    Ok(Some((String::from_utf8(row.try_get::<_,Vec<u8>>(1)?)?,row.try_get(2)?,row.try_get(3)?,row.try_get(4)?)))
}
fn release_keys(name:&str,display:Option<&str>)->std::collections::BTreeSet<i64> {
    let mut keys=std::collections::BTreeSet::new();
    for raw in std::iter::once(name).chain(display) {
        let clean=clean_candidate(raw).unwrap_or_else(||raw.to_string());
        keys.insert(name_key(&clean));
        if let Some(key)=hash_key(&clean) {keys.insert(key);}
    }
    keys
}
fn index_one(tx:&mut Transaction<'_>)->Result<bool> {
    let Some(row)=tx.query_opt("select release_id from predb_name_pending order by release_id limit 1",&[])? else {return Ok(false)};
    let id:i64=row.try_get(0)?;
    // Release first, then its derived pending row. Rename/delete cannot race
    // the real text and its key replacement inside this owner transaction.
    let Some((name,display,source,_))=checked_names(tx,id,true)? else {return Ok(true)};
    let Some(_)=tx.query_opt("select release_id from predb_name_pending where release_id=$1 for update",&[&id])? else {return Ok(true)};
    tx.execute("delete from predb_name_keys where release_id=$1",&[&id])?;
    if source.is_none_or(|source|source<100) {
        for key in release_keys(&name,display.as_deref()) {tx.execute("insert into predb_name_keys(key,release_id) values($1,$2)",&[&key,&id])?;}
    }
    tx.execute("delete from predb_name_pending where release_id=$1",&[&id])?;Ok(true)
}
fn qualifies(tx:&mut Transaction<'_>,id:i64,title_id:i64,title:&str,revision:i64,lock:bool)->Result<bool> {
    let Some((name,display,source,size))=checked_names(tx,id,lock)? else {return Ok(false)};
    if source.is_some_and(|source|source>=100) {return Ok(false)}
    let accepts=|raw:&str|literally(raw,title)||clean_candidate(raw).is_some_and(|clean|identity_key(&clean)==identity_key(title));
    let hash=clean_candidate(&name).is_some_and(|clean| {let clean=clean.to_ascii_lowercase();clean==md5_hex(title)||clean==sha1_hex(title)});
    if name==title || hash || (is_full_title(title)&&(accepts(&name)||display.as_deref().is_some_and(accepts))) {return Ok(true)}
    if !tx.query_one("select exists(select 1 from predb_inner_key where key=$1 and release_id=$2)",&[&name_key(title),&id])?.try_get::<_,bool>(0)? {return Ok(false)}
    let mut bytes=i64::try_from(name.len()+display.as_ref().map_or(0,String::len))?;
    let file_meta=tx.query("select id,octet_length(original_filename)::bigint from files where release_id=$1 order by id limit 5000",&[&id])?;
    let mut file_ids=Vec::with_capacity(file_meta.len());
    for row in file_meta {
        let length=row.try_get::<_,Option<i64>>(1)?.ok_or_else(||anyhow::anyhow!("native PreDB filename provenance unavailable"))?;
        bytes=bytes.checked_add(length).ok_or_else(||anyhow::anyhow!("native PreDB evidence size overflow"))?;file_ids.push(row.try_get::<_,i64>(0)?);
    }
    let inner_meta=tx.query("select seq,octet_length(name)::bigint from archive_inner_files where release_id=$1 order by seq limit 256",&[&id])?;
    let mut inner_ids=Vec::with_capacity(inner_meta.len());
    for row in inner_meta {bytes=bytes.checked_add(row.try_get::<_,i64>(1)?).ok_or_else(||anyhow::anyhow!("native PreDB evidence size overflow"))?;inner_ids.push(row.try_get::<_,i64>(0)?);}
    ensure!(bytes<=TEXT_BYTES,"native PreDB evidence exceeds read admission");
    let files=tx.query("select original_filename from files where release_id=$1 and id=any($2) order by id",&[&id,&file_ids])?.into_iter().map(|row|Ok(String::from_utf8(row.try_get::<_,Vec<u8>>(0)?)?)).collect::<Result<Vec<_>>>()?;
    let inner=tx.query("select name from archive_inner_files where release_id=$1 and seq=any($2) order by seq",&[&id,&inner_ids])?.into_iter().map(|row|row.try_get::<_,String>(0)).collect::<Result<Vec<_>,_>>()?;
    let evidence=crate::predb::matcher::Evidence{name:&name,display_name:display.as_deref(),size,files:files.iter().map(String::as_str).collect(),inner:inner.iter().map(String::as_str).collect()};
    Ok(crate::pg::predb_cache::match_checked_in(tx,revision,&evidence)?.is_some_and(|found|found.title.id==title_id))
}
fn late_page(tx:&mut Transaction<'_>,now:i64)->Result<LateReport> {
    let mut report=LateReport::default();
    let Some(cert)=tx.query_opt("select revision,version from ext_predb_key_meta where singleton=1",&[])? else {report.pending=true;return Ok(report)};
    ensure!(cert.try_get::<_,i64>(1)?==1,"unsupported PreDB key version");
    let revision:i64=cert.try_get(0)?;
    crate::pg::predb_cache::verify_read_in(tx,revision)?;
    if tx.query_one("select exists(select 1 from predb_name_pending)",&[])?.try_get::<_,bool>(0)? {report.pending=true;return Ok(report)}
    let names_revision:i64=tx.query_one("select last_value from predb_name_revision",&[])?.try_get(0)?;
    tx.execute("insert into ext_predb_meta(key,value) values('late_after_id',0) on conflict do nothing",&[])?;
    let cursor:i64=tx.query_one("select value from ext_predb_meta where key='late_after_id' for update",&[])?.try_get(0)?;
    if !tx.query_one("select exists(select 1 from releases)",&[])?.try_get::<_,bool>(0)? {
        if let Some(row)=tx.query_opt("select id from ext_predb_titles where id>$1 and added_at<=$2 order by id desc limit 1",&[&cursor,&now.saturating_sub(LATE_DELAY_SECS)])? {
            finish(tx,row.try_get(0)?)?;
        }
        return Ok(report)
    }
    let existing=tx.query_opt("select title_id,title_revision,names_revision,after_release from predb_late_scan where singleton for update",&[])?;
    let existing=existing.filter(|row|row.get::<_,i64>(1)==revision&&row.get::<_,i64>(2)==names_revision);
    let (title_id,after)=if let Some(row)=existing {(row.try_get::<_,i64>(0)?,row.try_get::<_,Option<i64>>(3)?)}else{
        tx.batch_execute("delete from predb_late_scan; delete from predb_late_matches")?;
        let Some(row)=tx.query_opt("select id from ext_predb_titles where id>$1 and added_at<=$2 order by id limit 1",&[&cursor,&now.saturating_sub(LATE_DELAY_SECS)])? else {return Ok(report)};
        let title_id:i64=row.try_get(0)?;
        tx.execute("insert into predb_late_scan(singleton,title_id,title_revision,names_revision) values(true,$1,$2,$3)",&[&title_id,&revision,&names_revision])?;
        (title_id,None)
    };
    let row=tx.query_one("select octet_length(title)<=4096,case when octet_length(title)<=4096 then title end from ext_predb_titles where id=$1",&[&title_id])?;
    ensure!(row.try_get::<_,bool>(0)?,"late PreDB title exceeds read admission");let title:String=row.try_get(1)?;
    let keys=title_keys(&title);let limit=CANDIDATES+1;
    // Each branch is independently bounded before the fixed-size UNION sort.
    let rows=tx.query("select distinct release_id from (
      (select release_id from predb_name_keys where key=$1 and ($5::bigint is null or release_id>$5) order by release_id limit $6)
      union all (select release_id from predb_name_keys where key=$2 and ($5::bigint is null or release_id>$5) order by release_id limit $6)
      union all (select release_id from predb_name_keys where key=$3 and ($5::bigint is null or release_id>$5) order by release_id limit $6)
      union all (select release_id from predb_inner_key where key=$4 and ($5::bigint is null or release_id>$5) order by release_id limit $6)
      ) candidates order by release_id limit $6",&[&keys[0],&keys[1],&keys[2],&name_key(&title),&after,&limit])?;
    let more=rows.len()>CANDIDATES as usize;let mut next=after;
    let ids=rows.into_iter().take(CANDIDATES as usize).map(|row|row.try_get::<_,i64>(0)).collect::<Result<Vec<_>,_>>()?;
    let prior=tx.query("select release_id from predb_late_matches order by release_id limit 17",&[])?;
    ensure!(prior.len()<=LATE_MAX_RERUNS,"late PreDB nomination invariant");
    let mut locked=ids.clone();for row in prior {locked.push(row.try_get::<_,i64>(0)?);}
    locked.sort_unstable();locked.dedup();
    // All supported native file/inner/name mutations lock their release first.
    // Lock this page plus prior matches before reading evidence or queue rows.
    tx.query("select id from releases where id=any($1) order by id for update",&[&locked])?;
    let locked_revision:i64=tx.query_one("select last_value from predb_name_revision",&[])?.try_get(0)?;
    if locked_revision!=names_revision {
        tx.batch_execute("delete from predb_late_scan; delete from predb_late_matches")?;
        report.pending=true;return Ok(report)
    }
    for id in ids {
        next=Some(id);
        if qualifies(tx,id,title_id,&title,revision,true)? {tx.execute("insert into predb_late_matches values($1) on conflict do nothing",&[&id])?;}
        let count:i64=tx.query_one("select count(*) from predb_late_matches",&[])?.try_get(0)?;
        if count>LATE_MAX_RERUNS as i64 {
            report.overflow=1;finish(tx,title_id)?;report.titles=1;return Ok(report)
        }
    }
    if more {tx.execute("update predb_late_scan set after_release=$1 where singleton",&[&next])?;report.pending=true;report.budget_exhausted=true;return Ok(report)}
    let rows=tx.query("select release_id from predb_late_matches order by release_id limit 17",&[])?;
    ensure!(rows.len()<=LATE_MAX_RERUNS,"late PreDB nomination invariant");
    let ids=rows.into_iter().map(|row|row.try_get::<_,i64>(0)).collect::<Result<Vec<_>,_>>()?;
    tx.query("select id from releases where id=any($1) order by id for update",&[&ids])?;
    let current_names:i64=tx.query_one("select last_value from predb_name_revision",&[])?.try_get(0)?;
    if current_names!=names_revision {
        tx.batch_execute("delete from predb_late_scan; delete from predb_late_matches")?;
        report.pending=true;return Ok(report)
    }
    for id in ids {if qualifies(tx,id,title_id,&title,revision,true)? && crate::pg::stage_queue::rerun_in(tx,id,crate::postprocess::step::mask(crate::predb::step::PREDB_MATCH),now)? {report.reruns+=1;}}
    finish(tx,title_id)?;report.titles=1;Ok(report)
}
fn finish(tx:&mut Transaction<'_>,id:i64)->Result<()> {
    tx.execute("update ext_predb_meta set value=$1 where key='late_after_id'",&[&id])?;
    tx.batch_execute("delete from predb_late_scan; delete from predb_late_matches")?;Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const TITLE:&str="Alpha.Beta.Gamma.2024.1080p.WEB-DL.H.264-TESTGRP";
    fn fixture()->Option<crate::pg::testing::Schema> {
        let schema=crate::pg::testing::Schema::new()?;let mut client=schema.pool.get().unwrap();
        if !client.query_one("select to_regclass('ext_predb_title_keys') is not null",&[]).unwrap().get::<_,bool>(0) {client.batch_execute(include_str!("../migrations/0010_predb_keys.fragment.sql")).unwrap();}
        if !client.query_one("select to_regclass('predb_name_keys') is not null",&[]).unwrap().get::<_,bool>(0) {client.batch_execute(include_str!("../migrations/0009_predb_late.fragment.sql")).unwrap();}
        client.execute("insert into ext_predb_titles(id,title,source,added_at) values(1,$1,1,0)",&[&TITLE]).unwrap();
        let mut tx=client.transaction().unwrap();assert!(matches!(crate::pg::predb_cache::advance_in(&mut tx,512).unwrap(),crate::pg::predb_cache::BuildStatus::Ready(_)));tx.commit().unwrap();
        drop(client);
        Some(schema)
    }
    fn release(tx:&mut Transaction<'_>,id:i64,name:&str,display:Option<&str>) {
        let normalized=name.replace('\0',"�");
        tx.execute("insert into releases(id,group_name,is_collection,identity_key,name,original_name,poster,posted_date,is_obfuscated,display_name) values($1,'test.group',false,$2,$3,$4,'poster','2024-01-01',false,$5)", &[&id,&&id.to_be_bytes()[..],&normalized,&name.as_bytes(),&display]).unwrap();
    }
    fn drain(tx:&mut Transaction<'_>) {while index_one(tx).unwrap() {}}
    #[test]
    fn native_late_name_keys_use_original_bytes_and_display_override() {
        let Some(schema)=fixture() else{return};let mut client=schema.pool.get().unwrap();let mut tx=client.transaction().unwrap();
        release(&mut tx,1,"Original\0Name",Some(TITLE));drain(&mut tx);
        assert_eq!(checked_names(&mut tx,1,false).unwrap().unwrap().0,"Original\0Name");
        let actual=tx.query("select key from predb_name_keys where release_id=1 order by key",&[]).unwrap().into_iter().map(|r|r.get::<_,i64>(0)).collect::<Vec<_>>();
        assert_eq!(actual,release_keys("Original\0Name",Some(TITLE)).into_iter().collect::<Vec<_>>());tx.commit().unwrap();
    }
    #[test]
    fn native_late_matches_literal_hash_normalized_display_and_inner_then_advances_atomically() {
        let Some(schema)=fixture() else{return};let mut client=schema.pool.get().unwrap();let mut tx=client.transaction().unwrap();
        release(&mut tx,1,TITLE,None);release(&mut tx,2,&md5_hex(TITLE),None);
        release(&mut tx,3,&TITLE.replace('.',"_"),None);release(&mut tx,4,"Obfuscated.Name-GRP",None);
        release(&mut tx,5,"Other.Obfuscated.Name-GRP",Some(TITLE));
        tx.execute("insert into predb_inner_key(key,release_id) values($1,4)",&[&name_key(TITLE)]).unwrap();
        tx.execute("insert into archive_inner_files(release_id,seq,name,size,source) values(4,0,$1,123,1)",&[&format!("{TITLE}.mkv")]).unwrap();drain(&mut tx);tx.commit().unwrap();
        let mut tx=client.transaction().unwrap();let page=late_page(&mut tx,1000).unwrap();assert_eq!((page.titles,page.reruns,page.overflow),(1,5,0));tx.rollback().unwrap();
        assert_eq!(client.query_one("select count(*) from pp_queue",&[]).unwrap().get::<_,i64>(0),0);
        let mut tx=client.transaction().unwrap();assert_eq!(late_page(&mut tx,1000).unwrap().reruns,5);tx.commit().unwrap();
        assert_eq!(client.query_one("select value from ext_predb_meta where key='late_after_id'",&[]).unwrap().get::<_,i64>(0),1);
        let mut tx=client.transaction().unwrap();assert_eq!(late_page(&mut tx,1000).unwrap().reruns,0);tx.commit().unwrap();
        assert_eq!(client.query_one("select count(*) from pp_queue",&[]).unwrap().get::<_,i64>(0),5);
    }
    #[test]
    fn native_late_collision_candidates_resume_across_commits_before_real_match() {
        let Some(schema)=fixture() else{return};let mut client=schema.pool.get().unwrap();let mut tx=client.transaction().unwrap();
        for id in 1..=70 {release(&mut tx,id,&format!("Unrelated.Title.Number{id}-OTHER"),None);}
        release(&mut tx,100,TITLE,None);drain(&mut tx);
        // Deterministic candidate-key collision injection: no hash computation
        // or candidate row alone is allowed to authorize a literal match.
        tx.execute("insert into predb_name_keys(key,release_id) select $1,n from generate_series(1,70) n on conflict do nothing",&[&name_key(TITLE)]).unwrap();tx.commit().unwrap();
        let mut tx=client.transaction().unwrap();let page=late_page(&mut tx,1000).unwrap();assert!(page.pending);assert_eq!(page.reruns,0);tx.commit().unwrap();
        let mut tx=client.transaction().unwrap();let page=late_page(&mut tx,1000).unwrap();assert_eq!((page.titles,page.reruns),(1,1));tx.commit().unwrap();
        assert_eq!(client.query_one("select release_id from pp_queue",&[]).unwrap().get::<_,i64>(0),100);
    }
    #[test]
    fn native_late_overflow_is_explicit_and_never_reruns_arbitrary_subset() {
        let Some(schema)=fixture() else{return};let mut client=schema.pool.get().unwrap();let mut tx=client.transaction().unwrap();
        for id in 1..=17 {release(&mut tx,id,TITLE,None);}drain(&mut tx);
        let page=late_page(&mut tx,1000).unwrap();assert_eq!((page.titles,page.reruns,page.overflow),(1,0,1));
        assert_eq!(tx.query_one("select count(*) from pp_queue",&[]).unwrap().get::<_,i64>(0),0);tx.commit().unwrap();
    }
    #[test]
    fn native_late_empty_catalog_fast_forward_is_derived_from_actual_empty_release_table() {
        let Some(schema)=fixture() else{return};let mut client=schema.pool.get().unwrap();let mut tx=client.transaction().unwrap();
        assert_eq!(late_page(&mut tx,1000).unwrap().reruns,0);tx.commit().unwrap();
        assert_eq!(client.query_one("select value from ext_predb_meta where key='late_after_id'",&[]).unwrap().get::<_,i64>(0),1);
        // A newly arrived release uses the normal current catalog matcher; no
        // historical release/header numeric cursor is imported or consulted.
        let mut tx=client.build_transaction().isolation_level(postgres::IsolationLevel::RepeatableRead).read_only(true).start().unwrap();
        let evidence=crate::predb::matcher::Evidence{name:TITLE,display_name:None,size:0,files:vec![],inner:vec![]};
        let crate::pg::predb_cache::LookupStatus::Ready(found)=crate::pg::predb_cache::find_in(&mut tx,&evidence).unwrap() else {panic!("completed matcher pending")};
        assert_eq!(found.found.unwrap().title.id,1);tx.commit().unwrap();
    }
    #[test]
    fn native_late_source_change_invalidates_partial_scan_before_any_rerun() {
        let Some(schema)=fixture() else{return};let mut client=schema.pool.get().unwrap();let mut tx=client.transaction().unwrap();
        for id in 1..=70 {release(&mut tx,id,&format!("Unrelated.Title.Number{id}-OTHER"),None);}release(&mut tx,100,TITLE,None);drain(&mut tx);
        tx.execute("insert into predb_name_keys(key,release_id) select $1,n from generate_series(1,70) n on conflict do nothing",&[&name_key(TITLE)]).unwrap();
        assert!(late_page(&mut tx,1000).unwrap().pending);tx.commit().unwrap();
        client.execute("update releases set name='Changed.Full.Title-OTHER',original_name=convert_to('Changed.Full.Title-OTHER','UTF8') where id=100",&[]).unwrap();
        let mut tx=client.transaction().unwrap();assert!(late_page(&mut tx,1000).unwrap().pending);drain(&mut tx);
        assert!(late_page(&mut tx,1000).unwrap().pending);tx.commit().unwrap();
        let mut tx=client.transaction().unwrap();assert_eq!(late_page(&mut tx,1000).unwrap().reruns,0);tx.commit().unwrap();
        assert_eq!(client.query_one("select count(*) from pp_queue",&[]).unwrap().get::<_,i64>(0),0);
    }

    #[test]
    fn native_late_inner_key_collisions_do_not_overflow_before_later_genuine_inner_evidence() {
        let Some(schema)=fixture() else{return};let mut client=schema.pool.get().unwrap();let mut tx=client.transaction().unwrap();
        for id in 1..=70 {
            release(&mut tx,id,&format!("Unrelated.Title.Number{id}-OTHER"),None);
            tx.execute("insert into predb_inner_key(key,release_id) values($1,$2)",&[&name_key(TITLE),&id]).unwrap();
            let unrelated=format!("Different.Original.Inner.{id}-OTHER.mkv");
            tx.execute("insert into archive_inner_files(release_id,seq,name,size,source) values($1,0,$2,123,1)",&[&id,&unrelated]).unwrap();
        }
        release(&mut tx,100,"Opaque.Release.Subject-GRP",None);
        tx.execute("insert into predb_inner_key(key,release_id) values($1,100)",&[&name_key(TITLE)]).unwrap();
        tx.execute("insert into archive_inner_files(release_id,seq,name,size,source) values(100,0,$1,123,1)",&[&format!("{TITLE}.mkv")]).unwrap();drain(&mut tx);tx.commit().unwrap();
        let mut tx=client.transaction().unwrap();let first=late_page(&mut tx,1000).unwrap();assert!(first.pending);assert_eq!((first.overflow,first.reruns),(0,0));tx.commit().unwrap();
        let mut tx=client.transaction().unwrap();let second=late_page(&mut tx,1000).unwrap();assert_eq!((second.titles,second.overflow,second.reruns),(1,0,1));tx.commit().unwrap();
        assert_eq!(client.query_one("select release_id from pp_queue",&[]).unwrap().get::<_,i64>(0),100);
    }

    #[test]
    fn native_late_changed_inner_evidence_does_not_reuse_staged_overflow_count() {
        let Some(schema)=fixture() else{return};let mut client=schema.pool.get().unwrap();let mut tx=client.transaction().unwrap();
        for id in 1..=65 {
            release(&mut tx,id,&format!("Opaque.Release.Number{id}-OTHER"),None);
            tx.execute("insert into predb_inner_key(key,release_id) values($1,$2)",&[&name_key(TITLE),&id]).unwrap();
            let inner=if id<=16 || id==65 {format!("{TITLE}.mkv")}else{format!("Unrelated.Inner.Number{id}-OTHER.mkv")};
            tx.execute("insert into archive_inner_files(release_id,seq,name,size,source) values($1,0,$2,123,1)",&[&id,&inner]).unwrap();
        }
        drain(&mut tx);assert!(late_page(&mut tx,1000).unwrap().pending);tx.commit().unwrap();
        let mut tx=client.transaction().unwrap();tx.query_one("select id from releases where id=1 for update",&[]).unwrap();
        tx.execute("update archive_inner_files set name='Changed.Inner.Name-OTHER.mkv' where release_id=1",&[]).unwrap();tx.commit().unwrap();
        let mut tx=client.transaction().unwrap();let restarted=late_page(&mut tx,1000).unwrap();assert!(restarted.pending);assert_eq!(restarted.overflow,0);tx.commit().unwrap();
        let mut tx=client.transaction().unwrap();let final_page=late_page(&mut tx,1000).unwrap();assert_eq!((final_page.titles,final_page.overflow,final_page.reruns),(1,0,16));tx.commit().unwrap();
        assert_eq!(client.query_one("select count(*) from pp_queue where release_id=1",&[]).unwrap().get::<_,i64>(0),0);
    }

    #[test]
    fn native_late_waiting_for_prior_release_lock_rechecks_revision_before_overflow() {
        let Some(schema)=fixture() else{return};let mut client=schema.pool.get().unwrap();let mut tx=client.transaction().unwrap();
        for id in 1..=65 {
            release(&mut tx,id,&format!("Opaque.Release.Number{id}-OTHER"),None);
            tx.execute("insert into predb_inner_key(key,release_id) values($1,$2)",&[&name_key(TITLE),&id]).unwrap();
            let inner=if id<=16 || id==65 {format!("{TITLE}.mkv")}else{format!("Unrelated.Inner.Number{id}-OTHER.mkv")};
            tx.execute("insert into archive_inner_files(release_id,seq,name,size,source) values($1,0,$2,123,1)",&[&id,&inner]).unwrap();
        }
        drain(&mut tx);assert!(late_page(&mut tx,1000).unwrap().pending);tx.commit().unwrap();
        let mut blocker=postgres::Client::connect(&schema.url,postgres::NoTls).unwrap();
        blocker.batch_execute(&format!("set search_path to \"{}\"; set statement_timeout='10s'",schema.name)).unwrap();
        let blocker_pid:i32=blocker.query_one("select pg_backend_pid()",&[]).unwrap().get(0);
        let mut held=blocker.transaction().unwrap();held.query_one("select id from releases where id=1 for update",&[]).unwrap();
        let (sender,receiver)=std::sync::mpsc::channel();let url=schema.url.clone();let namespace=schema.name.clone();
        let waiter=std::thread::spawn(move ||->Result<LateReport> {
            let mut connection=postgres::Client::connect(&url,postgres::NoTls)?;
            connection.batch_execute(&format!("set search_path to \"{namespace}\"; set statement_timeout='10s'; set lock_timeout='10s'"))?;
            let pid:i32=connection.query_one("select pg_backend_pid()",&[])?.try_get(0)?;sender.send(pid)?;
            let mut tx=connection.transaction()?;let page=late_page(&mut tx,1000)?;tx.commit()?;Ok(page)
        });
        // Observe the actual lock dependency. A delay alone cannot establish
        // that A captured its revision before B changed the inner evidence.
        let blocked=(||->Result<bool> {
            let pid=receiver.recv_timeout(std::time::Duration::from_secs(5))?;
            let deadline=std::time::Instant::now()+std::time::Duration::from_secs(5);
            while std::time::Instant::now()<deadline {
                if client.query_one("select $1::integer=any(pg_blocking_pids($2::integer))",&[&blocker_pid,&pid])?.try_get::<_,bool>(0)? {return Ok(true)}
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Ok(false)
        })();
        if matches!(blocked,Ok(true)) {
            held.execute("update archive_inner_files set name='Changed.Inner.Name-OTHER.mkv' where release_id=1",&[]).unwrap();held.commit().unwrap();
        } else {held.rollback().unwrap();}
        let waited=waiter.join().expect("late matcher thread");assert!(blocked.unwrap(),"did not observe the prior-release lock wait");
        let page=waited.unwrap();assert!(page.pending);assert_eq!((page.titles,page.overflow,page.reruns),(0,0,0));
        assert_eq!(client.query_one("select value from ext_predb_meta where key='late_after_id'",&[]).unwrap().get::<_,i64>(0),0);
        assert_eq!(client.query_one("select count(*) from pp_queue",&[]).unwrap().get::<_,i64>(0),0);
        let mut tx=client.transaction().unwrap();assert!(late_page(&mut tx,1000).unwrap().pending);tx.commit().unwrap();
        let mut tx=client.transaction().unwrap();let final_page=late_page(&mut tx,1000).unwrap();assert_eq!((final_page.titles,final_page.overflow,final_page.reruns),(1,0,16));tx.commit().unwrap();
    }

}
