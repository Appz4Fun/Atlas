//! Real fresh PG state/walk readers. Runtime/session observations and actual
//! lane measurements are separate: an alive owner does not fabricate pace.
use super::{fresh_reader::ReadView,fresh_live::RuntimeObservation};
use crate::{web_pending::{State,WalkCover},postprocess::step::ReleaseInfo};
use anyhow::{Result,Context,ensure};

pub(crate) struct Processing {pub state:State,pub owner:Option<RuntimeObservation>,pub now:i64}
impl ReadView<'_> {
    pub(crate) fn processing(&self,id:i64)->Result<Option<Processing>> {
        if !self.visible(id)?{return Ok(None);}
        self.before_query()?;
        let owner=RuntimeObservation::read_in(&mut *self.tx.borrow_mut())?;
        self.before_query()?;
        let row=self.tx.borrow_mut().query_one("select octet_length(r.name)::bigint+coalesce(octet_length(r.display_name),0)::bigint+
            octet_length(r.group_name)::bigint+octet_length(r.poster)::bigint+octet_length(r.posted_date)::bigint+
            coalesce((select octet_length(kind) from spots where release_id=r.id),0)::bigint+
            coalesce((select octet_length(last_error) from pp_state where release_id=r.id),0)::bigint
            from releases r where r.id=$1",&[&id])?;
        let bytes:i64=row.try_get(0)?;ensure!(bytes>=0,"negative native processing extent");
        self.admit_owned((bytes as u64).checked_mul(3).and_then(|n|n.checked_add(8192)).context("native processing extent overflow")?)?;
        self.before_query()?;
        let row=self.tx.borrow_mut().query_one("select name,display_name,group_name,poster,posted_date,size,complete,parts,file_total,is_obfuscated,name_source,
            (select kind from spots where release_id=r.id) from releases r where id=$1",&[&id])?;
        let spot:Option<String>=row.try_get(11)?;
        let release=ReleaseInfo {id,
            // Presentation adapter only. ProcessingScope::Native never uses
            // this legacy field to select a shard, resolve an ID or open data.
            shard:0,name:row.try_get(0)?,display_name:row.try_get(1)?,group:row.try_get(2)?,poster:row.try_get(3)?,posted_date:row.try_get(4)?,
            size:row.try_get(5)?,complete:row.try_get(6)?,parts:row.try_get(7)?,file_total:row.try_get(8)?,is_obfuscated:row.try_get(9)?,name_source:row.try_get(10)?,
            spot_kind:crate::spotnet::Kind::parse(spot.as_deref())};
        self.before_query()?;
        let row=self.tx.borrow_mut().query_opt("select done,failed,coalesce(net_wanted,false),net_left,net_next_at,net_attempts,
            coalesce(net_transient,0),db_attempts,net_rank,last_error from pp_state where release_id=$1",&[&id])?;
        // Default is used ONLY after SQL observes that this genuinely new
        // release has no pp_state row, matching the existing state semantics.
        let mut state=match row {None=>State::default(),Some(row)=>State {has_state:true,done:row.try_get(0)?,failed:row.try_get(1)?,net_wanted:row.try_get(2)?,
            net_left:row.try_get(3)?,net_next_at:row.try_get(4)?,net_attempts:row.try_get(5)?,net_transient:row.try_get(6)?,db_attempts:row.try_get(7)?,net_rank:row.try_get(8)?,last_error:row.try_get(9)?,..State::default()}};
        state.release=Some(release);
        self.before_query()?;
        state.due_at=self.tx.borrow_mut().query_opt("select due_at from pp_queue where release_id=$1",&[&id])?.map(|row|row.try_get(0)).transpose()?;
        self.before_query()?;
        let extents=self.tx.borrow_mut().query("select id,coalesce(octet_length(group_name),0)::bigint from pp_walks order by id desc limit $1",&[&i64::try_from(self.budget.rows+1)?])?;
        ensure!(extents.len()<=self.budget.rows,"native walk inventory exceeds row admission");
        let bytes=extents.iter().try_fold(4096u64,|sum,row|->Result<u64>{let n:i64=row.try_get(1)?;ensure!(n>=0,"negative native walk extent");Ok(sum.checked_add((n as u64).checked_mul(3).context("native walk extent overflow")?+256).context("native walk extent overflow")?)})?;
        self.admit_owned(bytes)?;self.before_query()?;
        let walks=self.tx.borrow_mut().query("select id,before_id,redo_mask,group_name from pp_walks order by id desc limit $1",&[&i64::try_from(self.budget.rows+1)?])?;
        ensure!(walks.len()==extents.len(),"native walks changed within snapshot");
        for (at,(row,extent)) in walks.into_iter().zip(extents).enumerate() {
            let group:Option<String>=row.try_get(3)?;
            ensure!(row.try_get::<_,i64>(0)?==extent.try_get::<_,i64>(0)?&&group.as_ref().map_or(0,String::len) as i64==extent.try_get::<_,i64>(1)?,"native walk extent changed");
            let before:i64=row.try_get(1)?;if before<=id||group.as_deref().is_some_and(|group|Some(group)!=state.release.as_ref().map(|release|release.group.as_str())){continue;}
            let redo:i64=row.try_get(2)?;if redo!=0{state.redo_walks.push((redo,at==0));}
            if state.walk.is_none(){state.walk=Some(WalkCover {before,group,active:at==0});}
        }
        self.before_query()?;
        let now:i64=self.tx.borrow_mut().query_one("select floor(extract(epoch from clock_timestamp()))::bigint",&[])?.try_get(0)?;
        self.before_query()?;
        match &owner {Some(owner)=>owner.verify_after(&mut *self.tx.borrow_mut())?,None=>ensure!(RuntimeObservation::read_in(&mut *self.tx.borrow_mut())?.is_none(),"native owner began during read")}
        Ok(Some(Processing {state,owner,now}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pg::{self,Backend,StorageConfig,fresh_reader::Budget};
    use std::time::{Duration,Instant};
    #[test]
    #[ignore="requires explicit isolated schema9 and original-owner PostgreSQL admission"]
    fn actual_fresh_state_survives_owner_fence_loss_without_fabricated_running_pace() {
        let schema=pg::testing::Schema::new().expect("explicit isolated PG schema9");
        let mut owner=schema.pool.get().unwrap();
        owner.query_one("select pg_advisory_lock(hashtext('atlas.indexer'),hashtext($1))",&[&schema.name]).unwrap();
        let root=tempfile::tempdir().unwrap();
        let headers=crate::config::HeaderStoreConfig {enabled:true,dir:root.path().to_str().unwrap().into()};
        let storage=StorageConfig {backend:Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:4,allow_plaintext:true};
        // Actual original-backend bootstrap, not hand-inserted nonce/epoch or
        // a caller readiness flag. This test never opens a public listener.
        let version=pg::migrate::version(&mut owner,&schema.name).unwrap();
        let epoch=pg::stage::open_epoch(&mut owner,&storage,&headers,version).unwrap();
        let release=crate::parser::Release {name:"Processing.Example.Release".into(),group:"alt.binaries.test".into(),poster:"poster".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
            articles:vec![crate::parser::Article {message_id:"processing-owner@example.invalid".into(),
                subject:"\"Processing.Example.Release.mkv\" yEnc (1/1)".into(),filename:Some("Processing.Example.Release.mkv".into()),
                part:Some(1),total_parts:Some(1),bytes:64,..Default::default()}],..Default::default()};
        let saved=pg::release_writer::save(&mut owner,&[release],1).unwrap();
        assert_eq!(saved,pg::release_writer::SaveStats {releases:1,files:1,parts:1});
        let id:i64=owner.query_one("select id from releases",&[]).unwrap().get(0);
        owner.execute("insert into pp_state(release_id,done,failed,net_wanted,net_left,net_attempts,last_error)
            values($1,8,16,true,32,3,'kept <error>')",&[&id]).unwrap();
        owner.execute("insert into pp_walks(kind,top_id,before_id,redo_mask,seen,ran,created_at)
            values('all',$1,$1,64,0,0,1)",&[&(id+1)]).unwrap();
        {
            let mut connection=schema.pool.get().unwrap();
            let view=ReadView::open(&mut connection,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
            let read=view.processing(id).unwrap().unwrap();
            assert_eq!(read.owner.as_ref().unwrap().epoch(),epoch.id());
            assert_eq!(read.state.done,8);assert_eq!(read.state.failed,16);
            assert!(read.state.net_wanted);assert_eq!(read.state.net_left,Some(32));assert_eq!(read.state.net_attempts,3);
            assert_eq!(read.state.last_error.as_deref(),Some("kept <error>"));
            assert_eq!(read.state.walk.as_ref().unwrap().before,id+1);assert!(read.state.walk.as_ref().unwrap().active);
            assert_eq!(read.state.redo_walks,vec![(64,true)]);
            assert!(view.processing(id+2).unwrap().is_none());
        }
        owner.query_one("select pg_advisory_unlock(hashtext('atlas.indexer'),hashtext($1))",&[&schema.name]).unwrap();
        let mut connection=schema.pool.get().unwrap();
        let view=ReadView::open(&mut connection,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
        let read=view.processing(id).unwrap().unwrap();assert!(read.owner.is_none());
        assert_eq!(read.state.done,8);assert_eq!(read.state.last_error.as_deref(),Some("kept <error>"));
    }
}
