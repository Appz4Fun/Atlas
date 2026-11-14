//! One bounded Keys snapshot on its original owner. Local read expiry is a
//! scheduling outcome only after the read-only transaction actually rolls back.
use anyhow::{Result, ensure};
use postgres::{Row, Transaction, types::ToSql};
use std::{sync::atomic::{AtomicBool, Ordering}, time::{Duration, Instant}};
use super::{StepReads, PROBE_QUERY, STORED_QUERY, TIMING_QUERY, decode_probe, stored_key_row, folded_in_with_budget};
use crate::keys::{Keys, FileCrc};

#[derive(Debug)]
pub(crate) struct KeysSnapshot {pub salt:i64, pub computed:Keys, pub stored:Option<Keys>}
#[derive(Debug)]
pub(crate) enum KeysRead {Complete(KeysSnapshot), Deferred(anyhow::Error)}
#[derive(Debug)]
pub(super) struct LocalKeysDeadline;
impl std::fmt::Display for LocalKeysDeadline {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("native Keys local read deadline")}
}
impl std::error::Error for LocalKeysDeadline {}

#[derive(Debug)]
pub(super) struct ReadRollback {read:anyhow::Error, rollback:postgres::Error}
impl std::fmt::Display for ReadRollback {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("native Keys read and rollback failed")}
}
impl std::error::Error for ReadRollback {
    fn source(&self)->Option<&(dyn std::error::Error+'static)> {
        if !self.read.is::<LocalKeysDeadline>() && crate::pg::outage::classify(&self.read)==crate::pg::outage::Failure::Fatal {
            Some(self.read.as_ref())
        } else {Some(&self.rollback)}
    }
}

pub(super) struct Budget<'a> {until:Instant, stop:&'a AtomicBool}
impl Budget<'_> {
    fn stopped(&self)->Result<()> {
        if self.stop.load(Ordering::Acquire) {return Err(crate::pg::outage::AttemptEnded.into())}Ok(())
    }
    pub(super) fn check(&self)->Result<()> {
        self.stopped()?;
        if Instant::now()>=self.until {return Err(LocalKeysDeadline.into())}Ok(())
    }
    fn bound(&self,tx:&mut Transaction<'_>)->Result<()> {
        self.check()?;
        let left=self.until.checked_duration_since(Instant::now()).filter(|left|!left.is_zero()).ok_or(LocalKeysDeadline)?;
        let millis=left.as_millis().clamp(1,i32::MAX as u128).to_string();
        // Setup, BEGIN, epoch verification and rollback errors remain genuine errors.
        tx.query_one("select set_config('statement_timeout',$1,true)",&[&millis])?;
        self.check()
    }
    pub(super) fn one(&self,tx:&mut Transaction<'_>,sql:&str,args:&[&(dyn ToSql+Sync)])->Result<Row> {
        self.bound(tx)?;tx.query_one(sql,args).map_err(owned_select_error)
    }
    pub(super) fn rows(&self,tx:&mut Transaction<'_>,sql:&str,args:&[&(dyn ToSql+Sync)])->Result<Vec<Row>> {
        self.bound(tx)?;tx.query(sql,args).map_err(owned_select_error)
    }
    fn optional(&self,tx:&mut Transaction<'_>,sql:&str,args:&[&(dyn ToSql+Sync)])->Result<Option<Row>> {
        self.bound(tx)?;tx.query_opt(sql,args).map_err(owned_select_error)
    }
}
fn owned_select_error(error:postgres::Error)->anyhow::Error {
    // Only this endpoint's bounded SELECTs may yield this local scheduling outcome.
    // Administrative cancellation and all other 57014 diagnostics stay fatal.
    if error.code()==Some(&postgres::error::SqlState::QUERY_CANCELED)
        && error.as_db_error().is_some_and(|db|db.message()=="canceling statement due to statement timeout") {
        anyhow::Error::new(error).context(LocalKeysDeadline)
    } else {error.into()}
}

impl StepReads {
    pub(crate) fn keys_snapshot(&self,id:i64,parts:Option<i64>,cached:Option<i64>,until:Instant)->Result<KeysRead> {
        ensure!(tokio::runtime::Handle::try_current().is_err(),"native Step reads require a standard worker thread");
        let cap=Duration::from_secs(10);
        #[cfg(test)]
        let cap=super::tests::query_budget().map_or(cap,|test|test.min(cap));
        // One absolute budget includes queue time; it is never restarted per query/file.
        let budget=Budget {until:until.min(Instant::now()+cap),stop:&self.owner.stop};
        self.owner.with(|client| {
            ensure!(self.owner.epoch().is_some_and(|epoch|epoch.id()==&self.epoch),"native Step read epoch changed");
            let mut tx=client.build_transaction().isolation_level(postgres::IsolationLevel::RepeatableRead).read_only(true).start()?;
            let result:Result<KeysSnapshot>=(|| {
                budget.check()?;
                let salt:i64=budget.one(&mut tx,"select value from meta where key='key_salt'",&[])?.try_get(0)?;
                ensure!(cached.is_none_or(|old|old==salt),"native key salt changed within its Step instance");
                let raw=budget.optional(&mut tx,PROBE_QUERY,&[&id])?;
                budget.check()?;
                let (probe,_)=decode_probe(raw)?;
                budget.check()?;
                let timing=budget.rows(&mut tx,TIMING_QUERY,&[&id])?.into_iter()
                    .map(|row|Ok((row.try_get(0)?,row.try_get(1)?))).collect::<Result<Vec<_>>>()?;
                budget.check()?;
                let mut computed=crate::keys::keys_of(probe.as_ref(),&timing,parts,Some(salt));
                budget.check()?;
                if computed.file_crc.is_none() {
                    computed.file_crc=folded_in_with_budget(&mut tx,id,&self.owner.stop,Some(&budget))?.first().copied().map(FileCrc::key);
                }
                if probe.as_ref().and_then(|p|p.archive.as_ref()).is_some_and(|a|a.encrypted||a.headers_encrypted) {
                    let metadata=budget.one(&mut tx,"select count(*),coalesce(sum(octet_length(name)),0)::bigint from par2_files where release_id=$1 and bound<>0 and complete<>0",&[&id])?;
                    ensure!(metadata.try_get::<_,i64>(0)?<=10000 && metadata.try_get::<_,i64>(1)?<=64*1024*1024,"native volume keys exceed read admission");
                    let mut files=Vec::new();
                    for row in budget.rows(&mut tx,"select name,size from par2_files where release_id=$1 and bound<>0 and complete<>0",&[&id])? {
                        budget.check()?;files.push((row.try_get::<_,String>(0)?,row.try_get::<_,i64>(1)?));
                    }
                    computed.volumes=crate::keys::volumes_text(&files).map(|text|crate::keys::as_key(crate::keys::digest8(text.as_bytes())));
                }
                budget.check()?;
                let stored=budget.optional(&mut tx,STORED_QUERY,&[&id])?.map(stored_key_row).transpose()?;
                budget.check()?;
                Ok(KeysSnapshot {salt,computed,stored})
            })();
            // Read-only success also rolls back, matching the bounded observation path.
            // No successful result/deferral can escape an unconfirmed transaction drain.
            let rollback=tx.rollback();
            match (result,rollback) {
                (Ok(snapshot),Ok(()))=>match budget.check() {
                    Ok(())=>Ok(KeysRead::Complete(snapshot)),
                    Err(error) if error.is::<LocalKeysDeadline>()=>Ok(KeysRead::Deferred(error)),
                    Err(error)=>Err(error),
                },
                (Err(error),Ok(())) if error.is::<LocalKeysDeadline>()=>{
                    budget.stopped()?;Ok(KeysRead::Deferred(error))
                },
                (Err(error),Ok(()))=>Err(error),
                (Ok(_),Err(error))=>Err(error.into()),
                (Err(read),Err(rollback))=>Err(ReadRollback {read,rollback}.into()),
            }
        })
    }
}
