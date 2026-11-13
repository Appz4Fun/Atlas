//! Original metadata-hash horizon over actual native segment bytes. Message
//! IDs are sorted/deduplicated in SQL and streamed, never retained as a set.
use anyhow::{Context,Result,ensure};
use postgres::fallible_iterator::FallibleIterator;
use std::{io::{Read,Write},sync::Arc};
use sha2::{Digest,Sha256};
use super::{fresh_reader::ReadView,fresh_nzb::Message};
use crate::{web_meta::HashFacts,nzb_spool::Pool};

impl ReadView<'_> {
    pub(crate) fn metadata_hashes(&self,id:i64,pool:&Arc<Pool>)->Result<Option<(HashFacts,Option<crate::probe::ProbeInfo>)>> {
        if !self.visible(id)?{return Ok(None);}
        let probe=self.probe(id)?;
        self.before_query()?;
        let count:i64=self.tx.borrow_mut().query_one("select count(*) from(select 1 from segments where release_id=$1 limit 250001) bounded",&[&id])?.try_get(0)?;
        ensure!((0..=250001).contains(&count),"invalid native hash horizon");
        if count>250000{return Ok(Some((HashFacts {digest:None,articles:250000,complete:false},probe)));}
        if count==0{return Ok(Some((HashFacts {digest:None,articles:0,complete:true},probe)));}
        let mut plan=pool.open()?;self.before_query()?;let mut tx=self.tx.borrow_mut();
        let mut rows=tx.query_raw("select substring(message_key from 1 for 33),octet_length(message_key)::bigint,octet_length(message_id)::bigint
            from(select distinct on(message_id) message_key,message_id from segments where release_id=$1 order by message_id,message_key) exact_ids
            order by message_id",[&id])?;
        let mut articles=0usize;
        while let Some(row)=rows.next()? {
            self.check_deadline()?;let key:Vec<u8>=row.try_get(0)?;ensure!(key.len()==32&&row.try_get::<_,i64>(1)?==32,"native hash message key extent");
            let length:i64=row.try_get(2)?;ensure!(length>=0,"native hash article extent");
            plan.write_all(&key)?;plan.write_all(&length.to_le_bytes())?;articles=articles.checked_add(1).context("native hash count overflow")?;
            ensure!(articles<=count as usize,"native hash unique IDs exceed source rows");
        }
        drop(rows);drop(tx);let mut plan=plan.finish()?;let mut hash=Sha256::new();
        for _ in 0..articles {
            let mut key=[0;32];let mut length=[0;8];plan.read_exact(&mut key)?;plan.read_exact(&mut length)?;
            let input=Message::raw(self,id,key,i64::from_le_bytes(length))?;
            crate::nzb_stream::text_chunks(input,|text|{hash.update(text.as_bytes());Ok(())})?;hash.update(b"\n");
        }
        ensure!(plan.read(&mut[0u8;1])?==0,"native hash plan has trailing IDs");
        Ok(Some((HashFacts {digest:Some(hash.finalize().into()),articles,complete:true},probe)))
    }
}
