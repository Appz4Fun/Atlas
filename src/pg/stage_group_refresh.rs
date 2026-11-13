//! Configured account LIST refresh on the existing native service lane.
//! Transport refusal preserves the previous completed capture; PG errors drain.
use anyhow::{Result,ensure};
use std::{sync::{Arc,atomic::{AtomicBool,Ordering}},time::{Duration,Instant}};
use crate::nntp::{Pool,group_capture::{Limits,REFRESH_TTL,scope_key}};
use super::indexer_runtime::Owner;

pub(crate) enum Tick {Idle,Stored(i64),ProviderUnavailable(crate::nntp::NntpError)}
struct Account {index:usize,binding:[u8;32],next:Instant}
pub(crate) struct Refresh {owner:Arc<Owner>,pool:Arc<Pool>,handle:tokio::runtime::Handle,accounts:Vec<Account>,turn:usize,limits:Limits}
impl Refresh {
    pub(crate) fn new(owner:&Arc<Owner>,pool:Arc<Pool>,handle:tokio::runtime::Handle)->Result<Self> {
        ensure!(tokio::runtime::Handle::try_current().is_err(),"native LIST refresh requires a standard lane");
        let accounts=pool.capture_index_bindings()?.into_iter().map(|(index,binding)|Account {index,binding,next:Instant::now()}).collect();
        Ok(Self {owner:owner.clone(),pool,handle,accounts,turn:0,limits:Limits {deadline:Duration::from_secs(20),..Limits::default()}})
    }
    pub(crate) fn tick(&mut self)->Result<Tick> {self.tick_at(chrono::Utc::now().timestamp(),Instant::now())}
    fn tick_at(&mut self,wall:i64,clock:Instant)->Result<Tick> {
        if self.owner.stop.load(Ordering::Acquire) {return Err(super::outage::AttemptEnded.into())}
        let total=self.accounts.len();
        let selected=(0..total).map(|n|(self.turn+n)%total).find(|&n|clock>=self.accounts[n].next);
        let Some(selected)=selected else {return Ok(Tick::Idle)};
        self.turn=(selected+1)%total;let account=&mut self.accounts[selected];
        // Both failed and successful attempts share the existing menu's ten
        // minute cadence. Reopening an attempt consults its actual PG head.
        account.next=clock+REFRESH_TTL;
        let epoch=self.owner.epoch().ok_or_else(||anyhow::anyhow!("native LIST owner ended"))?;
        let recent=self.owner.with(|client| {
            let mut tx=client.transaction()?;super::history_gate::native(&mut tx)?;epoch.verify(&mut tx)?;
            tx.batch_execute("set local statement_timeout='5s';set local lock_timeout='5s'")?;
            let row=tx.query_opt("select c.captured_at from native_group_capture_current h join native_group_captures c
                on(c.id,c.provider_binding,c.scope,c.pattern_key)=(h.capture_id,h.provider_binding,h.scope,h.pattern_key)
                where h.provider_binding=$1 and h.scope='full' and h.pattern_key=$2 and c.requested_pattern is null",
                &[&&account.binding[..],&&scope_key(None)[..]])?;
            let at=row.map(|row|row.try_get::<_,i64>(0)).transpose()?;tx.commit()?;Ok(at)
        })?;
        if let Some(age)=recent.and_then(|at|wall.checked_sub(at)).filter(|age|*age>=0&&*age<REFRESH_TTL.as_secs() as i64) {
            account.next=clock+Duration::from_secs(REFRESH_TTL.as_secs()-age as u64);return Ok(Tick::Idle)
        }
        let capture=self.handle.block_on(capture(&self.pool,account.index,self.limits,&self.owner.stop))?;
        match capture {
            Err(error)=>Ok(Tick::ProviderUnavailable(error)),
            Ok(capture)=>{
                ensure!(capture.provider_binding()==&account.binding&&capture.pattern().is_none(),"native LIST account/scope changed during its lease");
                let id=super::fresh_group_capture::store(&self.owner,capture)?;Ok(Tick::Stored(id))
            }
        }
    }
}
async fn capture(pool:&Pool,index:usize,limits:Limits,stop:&AtomicBool)->Result<crate::nntp::Result<crate::nntp::group_capture::Capture>> {
    let pending=pool.capture_groups_on(index,None,limits);tokio::pin!(pending);
    loop {
        if stop.load(Ordering::Acquire) {return Err(super::outage::AttemptEnded.into())}
        tokio::select! {
            result=&mut pending=>return Ok(result),
            _=tokio::time::sleep(Duration::from_millis(25))=>{},
        }
    }
}

#[cfg(test)]
#[path="stage_group_refresh_tests.rs"]
mod tests;
