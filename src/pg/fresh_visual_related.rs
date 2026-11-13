//! Display-only picture and film associations from actual fresh PostgreSQL
//! facts. No asset file, provider, payload lending or duplicate-hiding path is
//! reachable from these readers.
use super::fresh_reader::ReadView;
use crate::{images::step::Tie,search::ReleaseRow};
use anyhow::{Result,Context,ensure};
use postgres::fallible_iterator::FallibleIterator;

const SHOWN:usize=50;
const STILL_WINDOW_MS:i128=15_000;

impl ReadView<'_> {
    pub(crate) fn same_pictures(&self,id:i64)->Result<Vec<(ReleaseRow,Tie)>> {
        if !self.visible(id)? {return Ok(Vec::new());}
        // Admit the complete own-key inventory before asking PostgreSQL to
        // compare it. Text keys never cross the wire; only fixed descriptors
        // for the final legacy-sized window are hydrated below.
        self.before_query()?;
        let extent=self.tx.borrow_mut().query_one("select count(*),coalesce(sum(octet_length(sha256)::bigint+
            coalesce(octet_length(exif_id),0)::bigint+coalesce(octet_length(document_id),0)::bigint),0)::bigint
            from release_images where release_id=$1",&[&id])?;
        let count=u64::try_from(extent.try_get::<_,i64>(0)?).context("negative native picture-key count")?;
        let bytes=u64::try_from(extent.try_get::<_,i64>(1)?).context("negative native picture-key extent")?;
        self.admit_owned(count.checked_mul(256).and_then(|n|bytes.checked_mul(3).and_then(|bytes|n.checked_add(bytes)))
            .and_then(|n|n.checked_add(65_536)).context("native picture-key admission overflow")?)?;
        if count==0 {return Ok(Vec::new());}
        self.before_query()?;
        // Legacy discovery visits own positions, then SHA/EXIF/XMP, with at
        // most fifty newest releases per key. A later stronger tie upgrades
        // its label but preserves the original discovery order among ties.
        let rows=self.tx.borrow_mut().query("with candidates as (
            select own.position,0 as tie,other.release_id from release_images own cross join lateral(
                select distinct release_id from release_images where sha256=own.sha256 and release_id<>$1
                order by release_id desc limit 50) other where own.release_id=$1
            union all
            select own.position,1 as tie,other.release_id from release_images own cross join lateral(
                select distinct release_id from release_images where exif_id=own.exif_id and release_id<>$1
                order by release_id desc limit 50) other where own.release_id=$1 and own.exif_id is not null
            union all
            select own.position,2 as tie,other.release_id from release_images own cross join lateral(
                select distinct release_id from release_images where document_id=own.document_id and release_id<>$1
                order by release_id desc limit 50) other where own.release_id=$1 and own.document_id is not null
        ), ranked as (
            select release_id,position,tie,min(tie) over(partition by release_id) as strongest,
                row_number() over(partition by release_id order by position,tie) as discovery
            from candidates
        ) select release_id,strongest from ranked where discovery=1
            order by strongest,position,tie,release_id desc limit 50",&[&id])?;
        let selected=rows.into_iter().map(|row|{
            let tie=match row.try_get::<_,i32>(1)? {0=>Tie::Same,1=>Tie::ExifId,2=>Tie::DocumentId,_=>anyhow::bail!("invalid native picture tie")};
            Ok((row.try_get::<_,i64>(0)?,tie))
        }).collect::<Result<Vec<_>>>()?;
        let ids:Vec<_>=selected.iter().map(|(id,_)|*id).collect();
        let mut hydrated=std::collections::HashMap::new();
        for page in ids.chunks(self.budget.rows) {for row in self.rows(page)? {hydrated.insert(row.id,row);}}
        Ok(selected.into_iter().filter_map(|(id,tie)|hydrated.remove(&id).map(|row|(row,tie))).collect())
    }

    pub(crate) fn same_film(&self,id:i64)->Result<Vec<ReleaseRow>> {
        if !self.visible(id)? {return Ok(Vec::new());}
        self.admit_owned(65_536+SHOWN as u64*32)?;self.before_query()?;
        let own=self.tx.borrow_mut().query("select position,at_ms,phash from release_stills
            where release_id=$1 and phash is not null order by position limit 4",&[&id])?;
        // The actual still producer records the three public positions. An
        // unexpected fourth own hash is unavailable, never silently omitted.
        ensure!(own.len()<=crate::probe::stills::POSITIONS.len(),"native own still inventory exceeds producer horizon");
        let own=own.into_iter().map(|row|Ok((u8::try_from(row.try_get::<_,i64>(0)?).context("native still position out of range")?,
            row.try_get::<_,Option<i64>>(1)?,row.try_get::<_,i64>(2)? as u64))).collect::<Result<Vec<_>>>()?;
        if own.len()<2 {return Ok(Vec::new());}
        let mut ids=Vec::with_capacity(SHOWN);self.before_query()?;
        {
            let mut tx=self.tx.borrow_mut();
            let mut rows=tx.query_raw("select release_id,position,at_ms,phash from release_stills
                where release_id<>$1 and phash is not null order by release_id desc,position",[&id])?;
            let(mut current,mut hits)=(None,0u8);
            while let Some(row)=rows.next()? {
                self.check_deadline()?;let other:i64=row.try_get(0)?;
                if current!=Some(other) {
                    if let Some(previous)=current {if hits.count_ones()>=2 {ids.push(previous);if ids.len()==SHOWN {break;}}}
                    current=Some(other);hits=0;
                }
                let position=u8::try_from(row.try_get::<_,i64>(1)?).context("native still position out of range")?;
                let at=row.try_get::<_,Option<i64>>(2)?;let hash=row.try_get::<_,i64>(3)? as u64;
                for (index,&(own_position,own_at,own_hash)) in own.iter().enumerate() {
                    let near=match (own_at,at) { (Some(a),Some(b))=>(i128::from(a)-i128::from(b)).abs()<=STILL_WINDOW_MS,_=>own_position==position };
                    if near&&crate::video::phash::distance(own_hash,hash)<=crate::video::phash::MATCH_BITS {hits|=1<<index;}
                }
            }
            if ids.len()<SHOWN&&hits.count_ones()>=2 {if let Some(last)=current {ids.push(last);}}
        }
        let mut found=std::collections::HashMap::new();
        for page in ids.chunks(self.budget.rows) {for row in self.rows(page)? {found.insert(row.id,row);}}
        Ok(ids.into_iter().filter_map(|id|found.remove(&id)).collect())
    }
}
