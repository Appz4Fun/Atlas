//! Small durable summaries and nullable group marks, never a release/NXV scan.
use super::{Inventory,Metric,MAX_ROWS,MAX_BYTES};
use anyhow::{Result,ensure,Context};
use std::{collections::BTreeMap,sync::atomic::{AtomicBool,Ordering}};
use crate::metrics::{names::*,ObservedValue};

const PRESENCE:&str="atlas_native_group_present";
pub(super) fn owns(family:&str)->bool {
    [CATALOG_RELEASES.name,CATALOG_BYTES.name,GROUP_ARTICLE.name,GROUP_BACKFILL.name,GROUP_POSTED.name,GROUP_CHUNKS.name,GROUP_CHUNK_DAYS.name,PRESENCE].contains(&family)
}
fn gauge(out:&mut Inventory,family:&'static str,keys:&[(&str,&str)],value:i128)->Result<()> {
    ensure!(value.abs()<=9_007_199_254_740_992,"native gauge cannot exactly represent its durable integer");
    out.push(family,&crate::metrics::label_key(keys),ObservedValue::GaugeBits((value as f64).to_bits()))
}
fn workspace(bytes:&mut usize,name:&str,overhead:usize)->Result<()> {
    *bytes=bytes.checked_add(name.len()*2+overhead).context("native group workspace overflow")?;
    ensure!(*bytes<=MAX_BYTES,"native group workspace exceeds observation admission");Ok(())
}
fn stop(stop:&AtomicBool)->Result<()> {
    if stop.load(Ordering::Acquire) {return Err(crate::pg::outage::AttemptEnded.into())}Ok(())
}
#[derive(Default)]
struct Group {marks:[Option<i64>;4],posted:[Option<i64>;2]}
impl Group {
    fn progress(&self)->Option<(i128,i128)> {
        let [Some(first),Some(last),Some(back),Some(live)]=self.marks else {return None};
        let (first,last,back,live)=(i128::from(first),i128::from(last),i128::from(back),i128::from(live));
        let total=(last-first+1).max(0);Some((total,(live.min(last)-back.max(first-1)).clamp(0,total)))
    }
    fn score(&self)->(Option<(i128,i128)>,usize) {
        (self.progress().map(|(total,covered)|(covered,total)),self.marks.iter().chain(self.posted.iter()).filter(|v|v.is_some()).count())
    }
}
struct Chunks {total:i64,done:i64,days:[Option<i64>;3]}

pub(super) fn append(tx:&mut postgres::Transaction<'_>,out:&mut Inventory,stopping:&AtomicBool)->Result<()> {
    let mut after=None::<String>;let mut seen=0;let mut totals=[0i128;7];
    loop {
        stop(stopping)?;
        let rows=tx.query("select case when octet_length(group_name)<=4096 then group_name end,total,complete,nzb_ready,named,still_obfuscated,predb_matched,bytes from native_metric_catalog_groups where ($1::text is null or group_name>$1) order by group_name limit 128",&[&after])?;
        let count=rows.len();
        for row in rows {
            seen+=1;ensure!(seen<=MAX_ROWS,"native catalog group inventory exceeds observation admission");
            let group:String=row.try_get(0)?;after=Some(group.clone());
            if crate::filters::group_blocked(&group) {continue}
            for (i,total) in totals.iter_mut().enumerate() {*total=total.checked_add(i128::from(row.try_get::<_,i64>(i+1)?)).context("native catalog summary overflow")?;}
        }
        if count<128 {break}
    }
    for (i,state) in ["total","complete","nzb_ready","named","still_obfuscated","predb_matched"].into_iter().enumerate() {gauge(out,CATALOG_RELEASES.name,&[("state",state)],totals[i])?;}
    gauge(out,CATALOG_BYTES.name,&[],totals[6])?;
    let mut groups=BTreeMap::<String,Group>::new();let mut after=None::<String>;let mut seen=0;let mut owned=0;
    loop {
        stop(stopping)?;
        let rows=tx.query("select case when octet_length(name)<=4096 then name end,first_article,last_article,backfill_cursor,live_cursor,low_posted,high_posted from groups where ($1::text is null or name>$1) order by name limit 128",&[&after])?;
        let count=rows.len();
        for row in rows {
            seen+=1;ensure!(seen<=MAX_ROWS,"native cursor inventory exceeds observation admission");
            let key:String=row.try_get(0)?;after=Some(key.clone());let name=key.split('@').next().unwrap_or(&key).to_owned();
            let candidate=Group {marks:[row.try_get(1)?,row.try_get(2)?,row.try_get(3)?,row.try_get(4)?],posted:[row.try_get(5)?,row.try_get(6)?]};
            // Select one actual provider row. Never combine another account's
            // range with these cursors or turn its NULLs into caught-up zeros.
            if groups.get(&name).is_none_or(|old|candidate.score()>old.score()) {
                if !groups.contains_key(&name) {workspace(&mut owned,&name,512)?;}
                groups.insert(name,candidate);
            }
        }
        if count<128 {break}
    }
    let mut chunks=BTreeMap::<String,Chunks>::new();let mut after=None::<String>;let mut seen=0;
    loop {
        stop(stopping)?;
        let rows=tx.query("select case when octet_length(grp)<=4096 then grp end,total,done,oldest_day,newest_day,oldest_done_day from native_metric_chunk_groups where ($1::text is null or grp>$1) order by grp limit 128",&[&after])?;
        let count=rows.len();
        for row in rows {
            seen+=1;ensure!(seen<=MAX_ROWS,"native chunk group inventory exceeds observation admission");
            let name:String=row.try_get(0)?;after=Some(name.clone());
            if !groups.contains_key(&name) {workspace(&mut owned,&name,512)?;}
            workspace(&mut owned,&name,256)?;groups.entry(name.clone()).or_default();
            chunks.insert(name,Chunks {total:row.try_get(1)?,done:row.try_get(2)?,days:[Some(row.try_get(3)?),Some(row.try_get(4)?),row.try_get(5)?]});
        }
        if count<128 {break}
    }
    for (name,row) in groups {
        stop(stopping)?;gauge(out,PRESENCE,&[("group",&name)],1)?;
        for (mark,n) in ["first","last","backfill_cursor","live_cursor"].into_iter().zip(row.marks) {
            if let Some(n)=n {gauge(out,GROUP_ARTICLE.name,&[("group",&name),("mark",mark)],n.into())?;}
        }
        if let Some((total,mut covered))=row.progress() {
            if let Some(chunk)=chunks.get(&name).filter(|c|c.total>0&&c.done<c.total) {covered=total*i128::from(chunk.done)/i128::from(chunk.total);}
            gauge(out,GROUP_BACKFILL.name,&[("group",&name),("state","total")],total)?;
            gauge(out,GROUP_BACKFILL.name,&[("group",&name),("state","covered")],covered)?;
        }
        for (end,n) in ["oldest","newest"].into_iter().zip(row.posted) {if let Some(n)=n {gauge(out,GROUP_POSTED.name,&[("group",&name),("end",end)],n.into())?;}}
        if let Some(chunk)=chunks.get(&name) {
            for (state,n) in [("total",chunk.total),("done",chunk.done)] {gauge(out,GROUP_CHUNKS.name,&[("group",&name),("state",state)],n.into())?;}
            for (day,n) in ["oldest","newest","oldest_done"].into_iter().zip(chunk.days) {if let Some(n)=n {gauge(out,GROUP_CHUNK_DAYS.name,&[("group",&name),("day",day)],i128::from(n)*86400)?;}}
        }
    }Ok(())
}

pub(super) fn publish(rows:&[Metric]) {
    for family in [&CATALOG_RELEASES,&CATALOG_BYTES,&GROUP_ARTICLE,&GROUP_BACKFILL,&GROUP_POSTED,&GROUP_CHUNKS,&GROUP_CHUNK_DAYS,&NATIVE_GROUP_PRESENT] {
        family.replace(rows.iter().filter(|row|row.family==family.name).filter_map(|row|row.gauge.map(|bits|(row.labels.clone(),f64::from_bits(bits as u64)))));
    }
}
