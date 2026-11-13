//! Daily native release-row counts for the existing backfill priority policy.
//!
//! Like repair::media_counts, this counts rows, not whole-post identities, and
//! has no visibility, completeness, date, category, or backfill-state filter.
//! A scan fixes its upper id and observes bounded READ COMMITTED pages. Changes
//! to an already visited row and newly allocated ids wait until the next scan;
//! the old implementation likewise read each SQLite shard separately rather
//! than taking one catalog-wide snapshot. The previous completed tally remains
//! visible throughout a scan. Only its final owned transaction replaces it.
use anyhow::{Context,Result,ensure};
use std::{sync::{Arc,atomic::Ordering},time::{Duration,Instant}};
use crate::{config::HeaderStoreConfig,indexer::Db};
use super::indexer_runtime::Owner;

const COUNT_EVERY:Duration=Duration::from_secs(24*3600);
const MEDIA_BYTES:i64=4<<30;
const PAGE_ROWS:usize=512;
const PAGE_DEADLINE:Duration=Duration::from_secs(5);

#[derive(Debug,PartialEq,Eq)]
pub(crate) enum Tick {Idle,Progress,Stored}
#[derive(Clone,Copy)]
struct Scan {after:Option<i64>,through:Option<i64>}

/// One service on the original native standard-thread lane. Its random-named
/// temporary table belongs to that owner's actual PostgreSQL session; no pool,
/// persistent schema, replacement owner, or SQLite path is opened here. Counts
/// stay server-side, with at most 512 input rows and one scalar result per tick.
/// PostgreSQL bigint addition refuses overflow rather than wrapping a tally.
pub(crate) struct Service {
    owner:Arc<Owner>,db:Db,headers:HeaderStoreConfig,table:String,
    initialized:bool,scan:Option<Scan>,next:Instant,
}
impl Service {
    pub(crate) fn new(owner:&Arc<Owner>,db:Db,headers:&HeaderStoreConfig)->Result<Self> {
        ensure!(tokio::runtime::Handle::try_current().is_err(),"native media count requires a standard lane");
        db.check_native_owner(owner)?;
        owner.epoch().context("native media count requires its original epoch")?.catalog().header_config(headers)?;
        let mut nonce=[0u8;16];getrandom::fill(&mut nonce).map_err(|_|anyhow::anyhow!("native media tally identity unavailable"))?;
        let suffix=nonce.iter().map(|byte|format!("{byte:02x}")).collect::<String>();
        Ok(Self {owner:owner.clone(),db,headers:headers.clone(),table:format!("atlas_media_count_{suffix}"),initialized:false,scan:None,next:Instant::now()})
    }
    pub(crate) fn tick(&mut self)->Result<Tick> {self.tick_at(Instant::now())}
    fn tick_at(&mut self,clock:Instant)->Result<Tick> {
        ensure!(tokio::runtime::Handle::try_current().is_err(),"native media count requires a standard lane");
        self.db.check_native_owner(&self.owner)?;
        if self.owner.stop.load(Ordering::Acquire) {return Err(super::outage::AttemptEnded.into())}
        if self.scan.is_none()&&clock<self.next {return Ok(Tick::Idle)}
        let began=Instant::now();let owner=&self.owner;
        let epoch=owner.epoch().context("native media count original epoch ended")?;
        let table=&self.table;let headers=&self.headers;let scan=self.scan;
        let (next,stored)=owner.with(|client| {
            // Waiting for the shared owner and its preflight is not page work.
            // A healthy sibling worker must leave this page its full budget.
            let until=Instant::now()+PAGE_DEADLINE;
            let mut tx=client.transaction()?;
            bound(&mut tx,owner,until)?;super::history_gate::native(&mut tx)?;
            bound(&mut tx,owner,until)?;epoch.verify(&mut tx)?;
            epoch.catalog().header_config(headers)?;
            if !self.initialized {
                bound(&mut tx,owner,until)?;
                tx.batch_execute(&format!("create temporary table \"{table}\" (grp text collate \"C\" primary key,releases bigint not null check(releases>0)) on commit preserve rows"))?;
            }
            let scan=match scan {
                Some(scan)=>scan,
                None=>{
                    bound(&mut tx,owner,until)?;tx.batch_execute(&format!("truncate pg_temp.\"{table}\""))?;
                    bound(&mut tx,owner,until)?;
                    Scan {after:None,through:tx.query_one("select max(id) from releases",&[])?.try_get(0)?}
                }
            };
            bound(&mut tx,owner,until)?;
            let row=tx.query_one(&format!("with page as materialized (
                select id,group_name,size,coalesce(display_name,name) as title from releases
                where ($1::bigint is null or id>$1) and id<=$2 order by id limit $3
              ), counted as (
                insert into pg_temp.\"{table}\" as tally(grp,releases)
                select group_name,count(*) from page where size >= $4 and group_name is not null and (
                  lower(title) like '%2160p%' or lower(title) like '%remux%'
                  or lower(title) like '%.dv.%' or lower(title) like '%dovi%') group by group_name
                on conflict(grp) do update set releases=tally.releases+excluded.releases returning 1
              ) select max(id),count(*),(select count(*) from counted) from page"),
                &[&scan.after,&scan.through,&(PAGE_ROWS as i64),&MEDIA_BYTES])?;
            let after:Option<i64>=row.try_get(0)?;let rows:i64=row.try_get(1)?;
            ensure!((0..=PAGE_ROWS as i64).contains(&rows),"native media count page exceeded admission");
            let stored=rows<PAGE_ROWS as i64||after==scan.through;
            if stored {
                bound(&mut tx,owner,until)?;tx.execute("delete from backfill_media",&[])?;
                bound(&mut tx,owner,until)?;tx.execute(&format!("insert into backfill_media(grp,releases) select grp,releases from pg_temp.\"{table}\""),&[])?;
                bound(&mut tx,owner,until)?;tx.batch_execute(&format!("truncate pg_temp.\"{table}\""))?;
            }
            bound(&mut tx,owner,until)?;epoch.verify(&mut tx)?;
            bound(&mut tx,owner,until)?;tx.commit()?;
            Ok((Scan {after,through:scan.through},stored))
        })?;
        // Neither scan position nor daily cadence can advance on failed COMMIT.
        self.initialized=true;
        if stored {self.scan=None;self.next=clock+began.elapsed()+COUNT_EVERY;Ok(Tick::Stored)}
        else {self.scan=Some(next);Ok(Tick::Progress)}
    }
}
fn bound(tx:&mut postgres::Transaction<'_>,owner:&Owner,until:Instant)->Result<()> {
    if owner.stop.load(Ordering::Acquire) {return Err(super::outage::AttemptEnded.into())}
    let remaining=until.checked_duration_since(Instant::now()).filter(|duration|!duration.is_zero()).context("native media count page deadline expired")?;
    let millis=remaining.as_millis().clamp(1,i32::MAX as u128).to_string();
    tx.query_one("select set_config('statement_timeout',$1,true),set_config('lock_timeout',$1,true)",&[&millis])?;Ok(())
}

#[cfg(test)]
#[path="media_counts_tests.rs"]
mod tests;
