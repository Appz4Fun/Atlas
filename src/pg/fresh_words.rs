//! Read-only fresh-catalog Tantivy input/hydration. The canonical indexer
//! owns the delivery queue ACK; a web reader has no write/keeper capability.
use super::{fresh_reader::ReadView,stage::CatalogIdentity};
use crate::word_index::native::{self,Budget,Record,SearchSource,BuildSource};
use anyhow::{Context,Result,ensure};
use sha2::{Digest,Sha256};

const VISIBLE:&str="(r.display_name is not null or (not r.is_obfuscated and (length(r.name)<16 or r.name collate \"C\" ~ '[^A-Za-z0-9]')))";
const JOINS:&str="from releases r left join release_category c on c.release_id=r.id
    left join media_release m on m.release_id=r.id
    left join ext_media_titles t on t.title_key=m.title_key
        and (r.name collate \"C\" ~ '^AC[0-9]{4}' or r.display_name collate \"C\" ~ '^AC[0-9]{4}')";

/// Shared catalog/codec namespace for the readonly builder and the sole-owner
/// delivery lane. It derives from an actual typed catalog identity, not a raw
/// caller nonce or a mutable revision/ready flag.
pub(crate) fn catalog_stamp(identity:&CatalogIdentity)->[u8;32] {
    let mut digest=Sha256::new();digest.update(b"atlas.fresh.word.source.v1\0");
    digest.update(identity.nonce());digest.update(identity.codec_version().to_le_bytes());
    digest.update(native::codec_stamp().as_bytes());digest.finalize().into()
}

pub(crate) struct Search<'view,'client> { view:&'view ReadView<'client>,stamp:[u8;32] }
impl<'view,'client> Search<'view,'client> {
    pub(crate) fn open(view:&'view ReadView<'client>)->Result<Self> {
        view.before_query()?;
        let identity=CatalogIdentity::read_in(&mut *view.tx.borrow_mut())?;
        // Revision is intentionally excluded: newly indexed rows queue normal
        // catch-up instead of invalidating/rebuilding the entire catalog.
        Ok(Self {view,stamp:catalog_stamp(&identity)})
    }
    pub(crate) fn page(&self,after:i64,budget:Budget)->Result<Vec<Record>> {
        budget.check()?;
        ensure!(budget.rows<=self.view.budget.rows && budget.bytes<=self.view.budget.bytes,"native word page exceeds reader admission");
        let mut cursor=after;
        loop {
            self.view.before_query()?;
            let ids=self.view.tx.borrow_mut().query(&format!("select r.id from releases r where {VISIBLE}
                and r.id>$1 order by r.id limit $2"),&[&cursor,&i64::try_from(budget.rows)?])?
                .into_iter().map(|row|row.try_get(0)).collect::<std::result::Result<Vec<i64>,_>>()?;
            if ids.is_empty() {return Ok(Vec::new());}
            cursor=*ids.last().unwrap();
            let rows=self.records(&ids,budget)?;
            if !rows.is_empty() || ids.len()<budget.rows {return Ok(rows);}
        }
    }
    /// Every variable-length selected input is admitted before decoding. Both
    /// ext cache lookup and local release facts use this same read-only RR tx;
    /// no media provider switch controls whether a retained title is readable.
    pub(crate) fn records(&self,ids:&[i64],budget:Budget)->Result<Vec<Record>> {
        budget.check()?;ensure!(ids.len()<=budget.rows,"native index ID batch exceeds admission");
        if ids.is_empty(){return Ok(Vec::new());}
        self.view.before_query()?;
        let extents=self.view.tx.borrow_mut().query(&format!("select r.id,
            octet_length(r.name)::bigint,coalesce(octet_length(r.display_name),0)::bigint,
            octet_length(r.group_name)::bigint,octet_length(r.posted_date)::bigint,
            coalesce(octet_length(t.found_title),0)::bigint,coalesce(octet_length(t.title),0)::bigint
            {JOINS} where r.id=any($1) and {VISIBLE} order by r.id"),&[&ids])?;
        let mut extent=65536u64+(ids.len() as u64)*2048;
        for row in &extents {for at in 1..=6 {
            let size:i64=row.try_get(at)?;ensure!(size>=0,"native word input has negative extent");
            extent=extent.checked_add((size as u64).checked_mul(3).context("native word input extent overflow")?).context("native word admission overflow")?;
        }}
        ensure!(extent<=budget.bytes,"native word input exceeds page byte admission");
        self.view.before_query()?;
        let rows=self.view.tx.borrow_mut().query(&format!("select r.id,r.name,r.display_name,r.group_name,r.posted_date,
            r.size,c.category,t.found_title,t.title {JOINS} where r.id=any($1) and {VISIBLE} order by r.id"),&[&ids])?;
        ensure!(rows.len()==extents.len(),"native word source changed within snapshot");
        let mut result=Vec::with_capacity(rows.len());
        for (row,extent) in rows.into_iter().zip(extents) {
            let found:Option<String>=row.try_get(7)?;let asked:Option<String>=row.try_get(8)?;
            let record=Record {id:row.try_get(0)?,name:row.try_get(1)?,display_name:row.try_get(2)?,
                group:row.try_get(3)?,posted:row.try_get(4)?,size:row.try_get(5)?,category:row.try_get(6)?,
                known_as:asked.as_deref().and_then(|asked|native::decoded_title(found.as_deref(),asked))};
            ensure!(record.id==extent.try_get::<_,i64>(0)?,"native word identity changed");
            let sizes=[record.name.len(),record.display_name.as_ref().map_or(0,String::len),record.group.len(),
                record.posted.as_ref().map_or(0,String::len),found.as_ref().map_or(0,String::len),asked.as_ref().map_or(0,String::len)];
            for (at,size) in sizes.into_iter().enumerate(){ensure!(i64::try_from(size)?==extent.try_get::<_,i64>(at+1)?,"native word extent changed");}
            if !crate::filters::group_blocked(&record.group){result.push(record);}
        }
        Ok(result)
    }
}
impl SearchSource for Search<'_,'_> {
    fn stamp(&mut self)->Result<[u8;32]>{Ok(self.stamp)}
    fn pending(&mut self)->Result<bool>{
        self.view.before_query()?;
        Ok(self.view.tx.borrow_mut().query_one("select exists(select 1 from word_queue)",&[])?.try_get(0)?)
    }
    fn visible_rows(&mut self,ids:&[i64],budget:Budget)->Result<Vec<crate::search::ReleaseRow>> {
        budget.check()?;ensure!(ids.len()<=budget.rows,"native index hydration exceeds admission");
        self.view.rows(ids)
    }
}
/// A readonly build view has no ACK method. The indexer's sole-owner adapter
/// separately implements delivery Source using owner-minted queue batches.
impl BuildSource for Search<'_,'_> {
    fn expected_documents(&mut self)->Result<u64>{
        let budget=Budget {rows:self.view.budget.rows,bytes:self.view.budget.bytes,commit_bytes:self.view.budget.bytes};
        let mut after=0;let mut count=0u64;
        loop {let rows=self.page(after,budget)?;if rows.is_empty(){break;}
            after=rows.last().unwrap().id;count=count.checked_add(rows.len() as u64).context("native word count overflow")?;
        }Ok(count)
    }
    fn build_page(&mut self,after:i64,budget:Budget)->Result<Vec<Record>>{self.page(after,budget)}
}
