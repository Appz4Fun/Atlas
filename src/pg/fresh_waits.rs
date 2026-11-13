//! Native queue/rank/walk waits use the existing adaptive count oracle. SQL
//! predicates use actual fresh IDs; no shard ID arithmetic or SQLite hint.
use anyhow::{Result,ensure};
use postgres::types::ToSql;
use super::fresh_reader::ReadView;
use crate::web_pending::{NativePace,State,Wait,Waits,wait_in};
use crate::postprocess::engine::{claim_shares,queue_shares};

impl ReadView<'_> {
    fn ahead(&self,from_where:&str,args:&[&(dyn ToSql+Sync)],cap:i64)->Result<(i64,bool)> {
        ensure!((0..=500000).contains(&cap),"native wait count exceeds original horizon");self.before_query()?;
        let n:i64=self.tx.borrow_mut().query_one(&format!("select count(*) from(select 1 from {from_where} limit {}) horizon",cap+1),args)?.try_get(0)?;
        ensure!((0..=cap+1).contains(&n),"native wait count is invalid");Ok((n.min(cap),n>cap))
    }
    pub(crate) fn processing_waits(&self,state:&State,pace:&NativePace,now:i64)->Result<Waits> {
        let Some(release)=&state.release else{return Ok(Waits::default());};let id=release.id;
        let rate=pace.measured();let mut waits=Waits::default();
        if state.net_wanted {
            let (by_rank,newest,oldest)=claim_shares();
            let ranked=|cap:i64|->Result<(i64,bool)>{
                let (higher,more)=match state.net_rank {
                    Some(rank)=>self.ahead("pp_state where net_wanted and net_rank>$1",&[&rank],cap)?,
                    None=>self.ahead("pp_state where net_wanted and net_rank is not null",&[],cap)?,
                };if more{return Ok((higher,true));}
                let (same,more)=match state.net_rank {
                    Some(rank)=>self.ahead("pp_state where net_wanted and net_rank=$1 and release_id>$2",&[&rank,&id],cap-higher)?,
                    None=>self.ahead("pp_state where net_wanted and net_rank is null and release_id>$1",&[&id],cap-higher)?,
                };Ok((higher+same,more))
            };
            let newer=|cap|self.ahead("pp_state where net_wanted and release_id>$1 and net_next_at<=$2",&[&id,&now],cap);
            let older=|cap|self.ahead("pp_state where net_wanted and release_id<$1 and net_next_at<=$2",&[&id,&now],cap);
            waits.net=Some(wait_in(rate.map(|pace|pace.net_visits),&[(by_rank,&ranked),(newest,&newer),(oldest,&older)])?);
        }
        if let Some(due)=state.due_at.filter(|due|*due<=now) {
            let (latest,longest)=queue_shares();
            let newer=|cap:i64|->Result<(i64,bool)>{
                let (n,more)=self.ahead("pp_queue where due_at>$1 and due_at<=$2",&[&due,&now],cap)?;
                if more{return Ok((n,true));}let (tie,more)=self.ahead("pp_queue where due_at=$1 and release_id>$2",&[&due,&id],cap-n)?;Ok((n+tie,more))
            };
            let older=|cap:i64|->Result<(i64,bool)>{
                let (n,more)=self.ahead("pp_queue where due_at<$1",&[&due],cap)?;
                if more{return Ok((n,true));}let (tie,more)=self.ahead("pp_queue where due_at=$1 and release_id<$2",&[&due,&id],cap-n)?;Ok((n+tie,more))
            };
            waits.queue=Some(wait_in(rate.map(|pace|pace.queue_taken),&[(latest,&newer),(longest,&older)])?);
        }
        if let Some(walk)=&state.walk {
            waits.walk=Some(if !walk.active{Wait::Later}else{
                let count=|cap|match &walk.group {
                    Some(group)=>self.ahead("releases where group_name=$3 and id>$1 and id<$2",&[&id,&walk.before,group],cap),
                    None=>self.ahead("releases where id>$1 and id<$2",&[&id,&walk.before],cap),
                };
                wait_in(rate.map(|pace|pace.walk_rows),&[(1.0,&count)])?
            });
        }
        Ok(waits)
    }
}
