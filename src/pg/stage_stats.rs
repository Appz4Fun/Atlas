//! Bounded readonly native Stage observations on the original Owner session.
use anyhow::{Result,ensure};
use std::{sync::atomic::{AtomicBool,Ordering},time::{Duration,Instant}};
use super::{fresh_reader::{ReadView,Budget},indexer_runtime::{CacheDestination,destination_identity_in},stage::OwnerEpoch};

// Shared verbatim by the actual claimers and this observer. Parameters are the
// same actual wall clock; aliases are closed source identifiers, never input.
pub(crate) const DB_READY:&str="q.due_at<=$1 and (q.lease_until is null or q.lease_until<=$1)
    and not exists(select 1 from pp_state s where s.release_id=q.release_id and s.native_net_until>$1)";
pub(crate) const NET_READY:&str="s.net_wanted=true and s.net_next_at<=$1 and (s.native_net_until is null or s.native_net_until<=$1)
    and not exists(select 1 from pp_queue q where q.release_id=s.release_id and q.lease_until>$1)";
const MAX_COUNT:i64=100_000;
const MAX_WALKS:usize=128;
#[derive(Clone,Copy)]
pub(crate) struct Masks {pub enabled:i64,pub network:i64,pub net_ready_at:i64}
#[derive(Debug)]
pub(crate) struct Counts {pub due:i64,pub net:i64,pub failed:i64,pub walk_done:i64,pub walk_total:i64,pub names_over_quota:i64,pub net_claims:i64}
#[derive(Debug)]
pub(crate) enum Observation {Complete(Counts),Deferred(anyhow::Error)}
#[derive(Debug)]
pub(crate) struct LocalAdmission;
impl std::fmt::Display for LocalAdmission {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("native Stage observation exceeds local admission")}}
impl std::error::Error for LocalAdmission {}
#[derive(Debug)]
pub(crate) struct LocalReadDeadline;
impl std::fmt::Display for LocalReadDeadline {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("native Stage observation local read deadline")}}
impl std::error::Error for LocalReadDeadline {}
fn local(error:&anyhow::Error)->bool {error.is::<LocalAdmission>()||error.is::<LocalReadDeadline>()}

/// Retain both causes if rollback itself fails. The stronger real failure is
/// the source exposed to the existing outage classifier; no local admission
/// error can mask a failed rollback or a lost original connection.
#[derive(Debug)]
struct ReadRollback {read:anyhow::Error,rollback:postgres::Error}
impl std::fmt::Display for ReadRollback {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("native Stage observation read and rollback failed")}}
impl std::error::Error for ReadRollback {
    fn source(&self)->Option<&(dyn std::error::Error+'static)> {
        if !local(&self.read)&&super::outage::classify(&self.read)==super::outage::Failure::Fatal {Some(self.read.as_ref())}
        else {Some(&self.rollback)}
    }
}
fn stop(stopping:&AtomicBool)->Result<()> {if stopping.load(Ordering::Acquire) {return Err(super::outage::AttemptEnded.into())}Ok(())}
fn bound(view:&ReadView<'_>,until:Instant,stopping:&AtomicBool)->Result<()> {
    stop(stopping)?;
    let left=until.checked_duration_since(Instant::now()).filter(|left|!left.is_zero()).ok_or(LocalReadDeadline)?;
    let millis=left.as_millis().clamp(1,i32::MAX as u128).to_string();
    view.tx.borrow_mut().query_one("select set_config('statement_timeout',$1,true)",&[&millis])?;Ok(())
}
fn own_query_error(error:postgres::Error)->anyhow::Error {
    // Root policy: ONLY this locally bounded readonly query may defer the
    // exact server statement-timeout diagnostic. User cancellation and every
    // other 57014 remain genuine errors. Retain the original driver cause.
    if error.code()==Some(&postgres::error::SqlState::QUERY_CANCELED)
        &&error.as_db_error().is_some_and(|db|db.message()=="canceling statement due to statement timeout") {
        anyhow::Error::new(error).context(LocalReadDeadline)
    }else{error.into()}
}
fn own_anyhow_error(error:anyhow::Error)->anyhow::Error {
    match error.downcast::<postgres::Error>() {Ok(error)=>own_query_error(error),Err(error)=>error}
}
fn one(view:&ReadView<'_>,until:Instant,stopping:&AtomicBool,sql:&str,args:&[&(dyn postgres::types::ToSql+Sync)])->Result<postgres::Row> {
    bound(view,until,stopping)?;view.tx.borrow_mut().query_one(sql,args).map_err(own_query_error)
}
fn count(view:&ReadView<'_>,until:Instant,stopping:&AtomicBool,sql:&str,args:&[&(dyn postgres::types::ToSql+Sync)])->Result<i64> {
    let n=one(view,until,stopping,sql,args)?.try_get::<_,i64>(0)?;
    ensure!(n>=0,"native Stage observation count is negative");if n>MAX_COUNT {return Err(LocalAdmission.into())}Ok(n)
}

/// Return Deferred only AFTER an explicit successful rollback. The caller must
/// consume this enum inside Owner::with, so expected local budgets never mark a
/// healthy canonical writer fatal. Mutex/epoch preflight remains Owner policy.
pub(crate) fn observe(client:&mut postgres::Client,destination:&CacheDestination,storage:&super::StorageConfig,epoch:&OwnerEpoch,
    masks:Masks,stopping:&AtomicBool,until:Instant)->Result<Observation> {
    stop(stopping)?;ensure!(*storage==destination.storage,"native Stage observation storage changed");
    ensure!(masks.enabled>=0&&masks.network>=0,"native Stage observation masks are invalid");
    let Some(left)=until.checked_duration_since(Instant::now()).filter(|left|!left.is_zero()) else {
        return Ok(Observation::Deferred(LocalReadDeadline.into()))
    };
    if left>Duration::from_secs(2) {return Ok(Observation::Deferred(LocalAdmission.into()))}
    // Initial reader admission keeps its existing setup bound. Every query in
    // the actual observation below has the tighter caller-owned local deadline.
    let view=ReadView::open(client,&storage.schema,Budget {rows:MAX_WALKS,bytes:131_072},Instant::now()+Duration::from_secs(10))?;
    let result=(|| {
        let row=one(&view,until,stopping,"select current_schema()=$1",&[&storage.schema])?;
        ensure!(row.try_get::<_,bool>(0)?,"native Stage observation schema changed");
        bound(&view,until,stopping)?;
        ensure!(destination_identity_in(&mut *view.tx.borrow_mut(),&storage.schema).map_err(own_anyhow_error)?==destination.identity,"native Stage observation destination changed");
        ensure!(one(&view,until,stopping,"select coalesce(max(version),0) from schema_migrations",&[])?.try_get::<_,i32>(0)?==destination.version,"native Stage observation version changed");
        bound(&view,until,stopping)?;
        ensure!(super::stage::CatalogIdentity::read_in(&mut *view.tx.borrow_mut()).map_err(own_anyhow_error)?==*epoch.catalog(),"native Stage observation catalog changed");
        // Claimers use the worker host clock, including the runner-wide NET pause.
        let now=chrono::Utc::now().timestamp();
        let due=count(&view,until,stopping,&format!("select count(*) from(select 1 from pp_queue q where {DB_READY} limit 100001) admitted"),&[&now])?;
        let net=if masks.enabled&masks.network==0||now<masks.net_ready_at {0}else{
            count(&view,until,stopping,&format!("select count(*) from(select 1 from pp_state s where {NET_READY} limit 100001) admitted"),&[&now])?
        };
        let failed=count(&view,until,stopping,"select count(*) from(select 1 from pp_state where failed & $1<>0 limit 100001) admitted",&[&masks.enabled])?;
        bound(&view,until,stopping)?;
        let walks=view.tx.borrow_mut().query("select top_id,before_id from pp_walks order by id limit 129",&[]).map_err(own_query_error)?;
        if walks.len()>MAX_WALKS {return Err(LocalAdmission.into())}
        let (mut walk_done,mut walk_total)=(0i64,0i64);
        for row in walks {
            stop(stopping)?;let top:i64=row.try_get(0)?;let before:i64=row.try_get(1)?;
            ensure!(top>=0&&before>=0&&before<=top,"native Stage walk cursor is invalid");
            walk_done=walk_done.checked_add(top-before).ok_or(LocalAdmission)?;walk_total=walk_total.checked_add(top).ok_or(LocalAdmission)?;
        }
        let names_over_quota=one(&view,until,stopping,"select coalesce((select value from meta where key='archive_names_over_quota'),0)",&[])?.try_get::<_,i64>(0)?;
        ensure!(names_over_quota>=0,"native archive quota count is negative");
        let net_claims=count(&view,until,stopping,"select count(*) from(select 1 from pp_state where native_net_until>$1 and native_net_epoch=$2 limit 100001) admitted",&[&now,&&epoch.id()[..]])?;
        stop(stopping)?;if Instant::now()>=until {return Err(LocalReadDeadline.into())}
        Ok(Counts {due,net,failed,walk_done,walk_total,names_over_quota,net_claims})
    })();
    let rollback=view.tx.into_inner().rollback();
    match (result,rollback) {
        (Ok(counts),Ok(()))=>Ok(Observation::Complete(counts)),
        (Err(error),Ok(())) if local(&error)=>Ok(Observation::Deferred(error)),
        (Err(error),Ok(()))=>Err(error),
        (Ok(_),Err(error))=>Err(error.into()),
        (Err(read),Err(rollback))=>Err(ReadRollback {read,rollback}.into()),
    }
}

#[cfg(test)]
#[path="stage_stats_tests.rs"]
mod tests;
