//! Attempt-bound reads of the native linker's durable archive probe window.

use anyhow::{Result,ensure};
use super::StepReads;

#[derive(Debug)]
pub(crate) struct StaleWindow;
impl std::fmt::Display for StaleWindow {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("native archive probe window changed")}
}
impl std::error::Error for StaleWindow {}
pub(crate) enum LeadRead {Changed,Found(Vec<i64>)}
pub(crate) enum WindowRead {Publishing,Ready(i64,i64)}

impl StepReads {
    pub(crate) fn xref_tried(&self,id:i64,name:&str)->Result<WindowRead> {
        ensure!(crate::xref::is_upload(name) && name.len()<=65536,"native archive requires a tracked upload name");
        self.read(|tx| {
            if tx.query_one("select exists(select 1 from linker_epochs where phase='ack')",&[])?.try_get::<_,bool>(0)? {return Ok(WindowRead::Publishing)}
            let row=tx.query_opt("select u.probed,u.generation from releases r join linker_uploads u on u.release_name=r.name and u.group_name=r.group_name where r.id=$1 and r.original_name=$2",&[&id,&name.as_bytes()])?.ok_or_else(||anyhow::anyhow!("native archive probe window is unavailable"))?;
            let probed:i64=row.try_get(0)?;let generation:i64=row.try_get(1)?;
            ensure!(probed>=0 && generation>=0,"native archive probe window is invalid");Ok(WindowRead::Ready(probed,generation))
        })
    }
    pub(crate) fn xref_leads(&self,id:i64,name:&str,file:i64,generation:i64)->Result<LeadRead> {
        ensure!(crate::xref::is_upload(name) && name.len()<=65536 && file>=0,"native archive lead request is invalid");
        self.read(|tx| {
            if tx.query_one("select exists(select 1 from linker_epochs where phase='ack')",&[])?.try_get::<_,bool>(0)? {return Ok(LeadRead::Changed)}
            let row=tx.query_opt("select u.generation from releases r join linker_uploads u on u.release_name=r.name and u.group_name=r.group_name where r.id=$1 and r.original_name=$2",&[&id,&name.as_bytes()])?;
            let Some(row)=row else {return Ok(LeadRead::Changed)};
            if row.try_get::<_,i64>(0)?!=generation {return Ok(LeadRead::Changed)}
            let places=tx.query("select p.poster_key from linker_places p where p.release_name=$1 and p.file_number=$2 limit 2",&[&name,&file])?;
            ensure!(places.len()<=1,"native archive file has ambiguous lead metadata");
            let Some(place)=places.first() else {return Ok(LeadRead::Found(Vec::new()))};let key:Vec<u8>=place.try_get(0)?;
            let rows=tx.query("select part from linker_place_leads where poster_key=$1 order by ordinal limit 4097",&[&key])?;
            ensure!(rows.len()<=4096,"native archive leads exceed read admission");
            rows.into_iter().map(|row|Ok(row.try_get(0)?)).collect::<Result<Vec<_>>>().map(LeadRead::Found)
        })
    }
}

pub(crate) fn verify_read_in(tx:&mut postgres::Transaction<'_>,id:i64,generation:i64)->Result<i64> {
    if tx.query_one("select exists(select 1 from linker_epochs where phase='ack')",&[])?.try_get::<_,bool>(0)? {return Err(StaleWindow.into())}
    let row=tx.query_opt("select u.generation,u.probed from releases r join linker_uploads u on u.release_name=r.name and u.group_name=r.group_name where r.id=$1 and r.original_name=convert_to(r.name,'UTF8') and r.name like 'xref-upload-%' for share of u",&[&id])?;
    let Some(row)=row else {return Err(StaleWindow.into())};
    if row.try_get::<_,i64>(0)?!=generation {return Err(StaleWindow.into())}
    let probed:i64=row.try_get(1)?;ensure!(probed>=0 && generation>=0,"native archive probe window is invalid");Ok(probed)
}
pub(crate) fn tried_in(tx:&mut postgres::Transaction<'_>,id:i64,generation:i64,count:i64)->Result<u64> {
    let before=verify_read_in(tx,id,generation)?;
    ensure!(count>=0 && count<=before.saturating_add(crate::xref::HEAD_PROBES as i64),"native archive checkpoint exceeds its bounded probe window");
    Ok(tx.execute("update linker_uploads set probed=greatest(probed,$3) where generation=$2 and release_name=(select name from releases where id=$1)",&[&id,&generation,&count])?)
}

#[cfg(test)]
mod tests {
    use super::super::StepReads;
    use crate::pg::stage_queue::{Queue,Visit,Settlement};
    use crate::postprocess::{apply::Applied,step::{StepMutation,StepWrite}};
    #[test]
    fn native_probe_window_growth_refuses_every_old_result_before_checkpointing() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let runtime=crate::pg::stage_queue_tests::runtime(&schema);crate::pg::stage_queue_tests::push(&runtime);
        let id=runtime.owner.with(|client| {
            let id:i64=client.query_one("select id from releases",&[])?.get(0);
            client.execute("update releases set name='xref-upload-fixture',original_name=$2 where id=$1",&[&id,&b"xref-upload-fixture".as_slice()])?;
            client.batch_execute("insert into linker_uploads values('xref-upload-fixture','example.group',0,1,1,7,2,1);
                insert into linker_places(poster,release_name,group_name,file_number,file_total,parts,oldest,added) values(decode('01','hex'),'xref-upload-fixture','example.group',1,1,3,0,1);
                insert into linker_place_leads(poster,ordinal,part) values(decode('01','hex'),1,3),(decode('01','hex'),2,1)")?;Ok(id)
        }).unwrap();
        let reads=StepReads::for_owner(&runtime.owner).unwrap();let data=crate::postprocess::data::StepData::Native(&reads);
        assert_eq!(data.xref_tried(id,"xref-upload-fixture").unwrap(),Some((2,7)));
        assert_eq!(data.xref_leads(id,"xref-upload-fixture",1,Some(7)).unwrap(),vec![3,1]);
        let publishing=runtime.owner.with(|client|Ok(client.query_one("insert into linker_epochs(logical_now,phase) values(1,'ack') returning id",&[])?.get::<_,i64>(0))).unwrap();
        assert!(matches!(data.xref_tried(id,"xref-upload-fixture"),Err(crate::postprocess::step::StepError::Transient(_))));
        assert_eq!(runtime.owner.failure(),None);
        runtime.owner.with(|client|{client.execute("update linker_epochs set phase='done' where id=$1",&[&publishing])?;Ok(())}).unwrap();
        let queue=Queue::for_owner(&runtime.owner).unwrap();let claims=queue.claim(10,1,60).unwrap();let old=&claims[0];
        let result=|generation|Applied {release_id:id,writes:vec![
            (4,StepWrite::for_mutation(id,StepMutation::SetCategory {category:2000})),
            (4,StepWrite::for_mutation(id,StepMutation::XrefRead {generation})),
            (4,StepWrite::for_mutation(id,StepMutation::XrefTried {generation,count:3})),
        ],..Default::default()};
        runtime.owner.with(|client|{client.execute("update linker_uploads set generation=8,probed=0 where release_name='xref-upload-fixture'",&[])?;Ok(())}).unwrap();
        assert!(matches!(data.xref_leads(id,"xref-upload-fixture",1,Some(7)),Err(crate::postprocess::step::StepError::Transient(_))));
        assert_eq!(runtime.owner.failure(),None,"ordinary window growth must not poison the owner");
        assert_eq!(queue.settle(old,Visit::prepare(old,result(7),[7;32]).unwrap(),11).unwrap(),Settlement::Superseded);
        runtime.owner.with(|client| {
            assert_eq!(client.query_one("select count(*) from release_category",&[])?.get::<_,i64>(0),0);
            assert_eq!(client.query_one("select count(*) from native_operations",&[])?.get::<_,i64>(0),0);
            assert_eq!(client.query_one("select probed from linker_uploads",&[])?.get::<_,i64>(0),0);Ok(())
        }).unwrap();
        let claims=queue.claim(12,1,60).unwrap();let fresh=&claims[0];
        assert_eq!(queue.settle(fresh,Visit::prepare(fresh,result(8),[8;32]).unwrap(),13).unwrap(),Settlement::Applied);
        assert_eq!(queue.settle(fresh,Visit::prepare(fresh,result(8),[8;32]).unwrap(),14).unwrap(),Settlement::Replayed);
        assert_eq!(data.xref_tried(id,"xref-upload-fixture").unwrap(),Some((3,8)));
        runtime.owner.close();
        assert!(matches!(data.xref_tried(id,"xref-upload-fixture"),Err(crate::postprocess::step::StepError::Native(_))));
    }
}
