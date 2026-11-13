//! Own picture metadata and actual request outcomes from this RR catalog.
//! Operational reader links do not grant cross-post payload lending or writes.
use anyhow::{Result,Context,ensure};
use super::fresh_reader::ReadView;
use crate::images::step::{Image,Request};

pub(crate) struct Pictures {
    pub images:Vec<(i64,Image)>,pub stills:Vec<(u8,Option<i64>,u32,u32)>,
    pub requests:Vec<Request>,
}
impl ReadView<'_> {
    pub(crate) fn own_pictures(&self,id:i64)->Result<Pictures> {
        self.before_query()?;
        let reader:i64=self.tx.borrow_mut().query_one("select coalesce((select release_id from beside_files where row_id=$1),(select lead_id from upload_link where release_id=$1),$1)",&[&id])?.try_get(0)?;
        ensure!(self.visible(reader)?,"native picture reader association is unavailable");
        let mut request_ids=vec![id];if reader!=id{request_ids.push(reader);}
        self.before_query()?;
        let extents=self.tx.borrow_mut().query_one("select
            (select count(*) from release_images where release_id=$1),
            coalesce((select sum(octet_length(file)::bigint+octet_length(sha256)::bigint+octet_length(format)::bigint+coalesce(octet_length(meta),0)::bigint)::bigint from release_images where release_id=$1),0),
            (select count(*) from release_stills where release_id=$1),
            (select count(*) from picture_requests where release_id=any($2)),
            coalesce((select sum(octet_length(file)::bigint+coalesce(octet_length(outcome),0)::bigint)::bigint from picture_requests where release_id=any($2)),0)",&[&id,&request_ids])?;
        let images:i64=extents.try_get(0)?;let stills:i64=extents.try_get(2)?;let requests:i64=extents.try_get(3)?;
        ensure!([images,stills,requests].into_iter().all(|n|(0..=5000).contains(&n)),"native picture inventory exceeds admission");
        let mut owned=(images+stills+requests) as u64*2048+4096;
        for at in [1usize,4] {let bytes:i64=extents.try_get(at)?;ensure!(bytes>=0,"negative native picture extent");owned=owned.checked_add((bytes as u64).checked_mul(16).context("native picture extent overflow")?).context("native picture extent overflow")?;}
        self.admit_owned(owned)?;self.before_query()?;
        let rows=self.tx.borrow_mut().query("select position,file,sha256,format,width,height,bytes,meta from release_images where release_id=$1 order by position",&[&id])?;
        ensure!(rows.len()==images as usize,"native picture inventory changed");
        let mut shown=Vec::with_capacity(rows.len());
        for row in rows {
            let meta:Option<String>=row.try_get(7)?;
            let jpeg=meta.as_deref().map(serde_json::from_str::<crate::images::jpeg::JpegInfo>).transpose().context("invalid native JPEG metadata")?;
            let image=Image {position:row.try_get(0)?,file:row.try_get(1)?,sha256:row.try_get(2)?,format:row.try_get(3)?,width:u32::try_from(row.try_get::<_,i64>(4)?)?,height:u32::try_from(row.try_get::<_,i64>(5)?)?,bytes:row.try_get(6)?,jpeg};
            ensure!(image.position>=0&&image.bytes>=0&&image.sha256.len()==64&&image.sha256.bytes().all(|b|b.is_ascii_hexdigit()),"invalid native image descriptor");shown.push((id,image));
        }
        self.before_query()?;
        let rows=self.tx.borrow_mut().query("select position,at_ms,width,height from release_stills where release_id=$1 order by position",&[&id])?;
        ensure!(rows.len()==stills as usize,"native still inventory changed");
        let stills=rows.into_iter().map(|row|->Result<_>{Ok((u8::try_from(row.try_get::<_,i64>(0)?)?,row.try_get(1)?,u32::try_from(row.try_get::<_,i64>(2)?)?,u32::try_from(row.try_get::<_,i64>(3)?)?))}).collect::<Result<Vec<_>>>()?;
        self.before_query()?;
        let bit=crate::postprocess::step::mask(crate::images::step::IMAGES_STEP);
        let rows=self.tx.borrow_mut().query("select q.release_id,q.file,q.requested_at,q.done_at,q.outcome,(p.release_id is not null and coalesce(p.net_left,0)&$2=0)
            from picture_requests q left join pp_state p on p.release_id=q.release_id where q.release_id=any($1) order by array_position($1::bigint[],q.release_id),q.file collate \"C\"",&[&request_ids,&bit])?;
        ensure!(rows.len()==requests as usize,"native picture request inventory changed");
        let requests=rows.into_iter().map(|row|->Result<_>{Ok(Request {release_id:row.try_get(0)?,file:row.try_get(1)?,requested_at:row.try_get(2)?,done_at:row.try_get(3)?,outcome:row.try_get(4)?,passed_over:row.try_get(5)?})}).collect::<Result<Vec<_>>>()?;
        Ok(Pictures {images:shown,stills,requests})
    }
    pub(crate) fn admit_picture_cells(&self,contents:&crate::release_info::Contents)->Result<()> {
        let bytes=contents.list.iter().filter(|file|crate::images::is_image_name(&file.name)).try_fold(0u64,|sum,file|->Result<u64>{
            sum.checked_add((file.name.len() as u64).checked_mul(12).and_then(|n|n.checked_add(2048)).context("native picture cell extent overflow")?).context("native picture cell extent overflow")
        })?;self.admit_owned(bytes)
    }
    pub(crate) fn has_own_picture(&self,id:i64,name:&str)->Result<bool> {
        if !crate::images::is_image_name(name){return Ok(false);}
        self.admit_owned((name.len() as u64).checked_mul(4).and_then(|n|n.checked_add(256)).context("native picture name extent overflow")?)?;self.before_query()?;
        Ok(self.tx.borrow_mut().query_one("select exists(select 1 from files where release_id=$1 and coalesce(original_filename,convert_to(filename,'UTF8'))=convert_to($2::text,'UTF8'))",&[&id,&name])?.try_get(0)?)
    }
    pub(crate) fn picture_eta(&self,reader:i64)->Result<Option<String>> {
        let read=self.processing(reader)?.context("native picture reader disappeared")?;
        let pace=self.processing_pace(read.owner.as_ref(),read.now)?;let waits=self.processing_waits(&read.state,&pace,read.now)?;
        let eta=crate::web_pending::native_network_eta(&read.state,&pace,&waits,read.now,"images")?;
        self.verify_runtime(read.owner.as_ref())?;Ok(eta)
    }
}
