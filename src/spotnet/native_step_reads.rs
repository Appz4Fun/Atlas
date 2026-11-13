//! The same original-byte decoder, inside the canonical owner's RR and
//! participant observation. Returned facts cannot cross an owner/revision.
use anyhow::Result;
use super::StepReads;
impl StepReads {
    pub(crate) fn spot_detail(&self,id:i64)->Result<Option<crate::spotnet::store::Detail>> {
        self.read(|tx|self.original_spot_in(tx,id))
    }
    pub(super) fn original_spot_in(&self,tx:&mut postgres::Transaction<'_>,id:i64)->Result<Option<crate::spotnet::store::Detail>> {
            let Some(extent)=crate::pg::fresh_spots::extent_in(tx,id)? else {return Ok(None)};
            // The fixed original XML envelope is far below the existing Step
            //64MiB text envelope. Check before any text/bytea transfer.
            anyhow::ensure!(extent.owned<=64*1024*1024,"native Spotnet Step read exceeds admission");
            self.observe_in(tx,&[id])?;
            let mut detail=crate::pg::fresh_spots::detail_in(tx,id,&extent)?;
            detail.removed=crate::pg::fresh_spots::removal_in(tx,&detail)?;
            Ok(Some(detail))
    }
}
