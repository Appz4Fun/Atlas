//! Closed native Spotnet reads. Registered under pg so the existing RR view's
//! byte/row/deadline admission stays authoritative; no SQLite or provider read.
use anyhow::{Result,Context,ensure};
use postgres::Transaction;
use std::collections::HashMap;
use crate::spotnet::{self,store::{Detail,Tagged,Xml,Removed,RemovedBy},full::FullSpot,comments::{self,Comment}};
use super::fresh_reader::ReadView;

// Exactly the fields returned by the closed spot query, measured BEFORE PG
// decodes any text/bytea. This is read sizing, never a mutation capability.
pub(in crate::pg) struct Extent { lengths:[Option<i32>;10],pub(in crate::pg) owned:u64 }
fn length(value:Option<i32>)->Result<u64> {
    match value {None=>Ok(0),Some(n)=>{ensure!(n>=0,"negative native Spotnet extent");Ok(n as u64)}}
}
pub(in crate::pg) fn extent_in(tx:&mut Transaction<'_>,id:i64)->Result<Option<Extent>> {
    let Some(row)=tx.query_opt("select octet_length(message_id),octet_length(spotter_id),octet_length(imdb_id),octet_length(kind),octet_length(title),octet_length(coalesce(poster,'')),octet_length(coalesce(tag,'')),octet_length(coalesce(subcats,'')),octet_length(xml),octet_length(native_original_xml) from spots where release_id=$1",&[&id])? else {return Ok(None)};
    let mut lengths=[None;10];let mut owned=8192u64;
    for (at,n) in lengths.iter_mut().enumerate() {
        *n=row.try_get(at)?;let bytes=length(*n)?;
        ensure!(bytes<=if at==9 {spotnet::MAX_XML_BYTES as u64}else{3*spotnet::MAX_XML_BYTES as u64},"native Spotnet field exceeds original XML admission");
        // Raw input, PG response, UTF8 strings and existing XML parser scratch
        // are all admitted before transfer. No hidden sanitized copy fallback.
        owned=owned.checked_add(bytes.checked_mul(12).context("native Spotnet extent overflow")?).context("native Spotnet extent overflow")?;
    }
    ensure!(lengths[9].is_some(),"native Spotnet XML original is absent");
    Ok(Some(Extent {lengths,owned}))
}
pub(in crate::pg) fn detail_in(tx:&mut Transaction<'_>,id:i64,extent:&Extent)->Result<Detail> {
    let row=tx.query_one("select message_id,spotter_id,imdb_id,kind,title,coalesce(poster,''),coalesce(tag,''),category,coalesce(subcats,''),size,created,xml,native_original_xml from spots where release_id=$1",&[&id])?;
    let message_id:String=row.try_get(0)?;let spotter_id:Option<String>=row.try_get(1)?;let imdb_id:Option<String>=row.try_get(2)?;
    let kind:String=row.try_get(3)?;let title:String=row.try_get(4)?;let poster:String=row.try_get(5)?;let tag:String=row.try_get(6)?;
    let cats:String=row.try_get(8)?;let displayed:Option<String>=row.try_get(11)?;let raw:Option<Vec<u8>>=row.try_get(12)?;
    let actual=[Some(message_id.len()),spotter_id.as_ref().map(String::len),imdb_id.as_ref().map(String::len),Some(kind.len()),Some(title.len()),Some(poster.len()),Some(tag.len()),Some(cats.len()),displayed.as_ref().map(String::len),raw.as_ref().map(Vec::len)];
    ensure!(actual.into_iter().zip(extent.lengths).all(|(a,b)|a.map(|n|n as i32)==b),"native Spotnet original extent changed");
    ensure!(spotnet::valid_message_id(&message_id),"native Spotnet message identity is malformed");
    let original=original(raw,displayed.as_deref(),spotnet::MAX_XML_BYTES)?.context("native Spotnet XML original is absent")?;
    let columns=FullSpot {title,poster,tag,category:i32::try_from(row.try_get::<_,Option<i64>>(7)?.context("native Spotnet category absent")?)?,
        subcats:spotnet::header::subcats(&cats),size:u64::try_from(row.try_get::<_,Option<i64>>(9)?.context("native Spotnet size absent")?)?,
        created:row.try_get::<_,Option<i64>>(10)?.context("native Spotnet creation time absent")?,..Default::default()};
    let (spot,xml)=if original.is_empty() {(columns,Xml::Gone)} else {
        let parsed=spotnet::full::parse(&original).context("native Spotnet original XML is malformed")?;
        ensure!(parsed.title.replace('\0',"\u{fffd}")==columns.title&&parsed.poster.replace('\0',"\u{fffd}")==columns.poster&&parsed.tag.replace('\0',"\u{fffd}")==columns.tag
            &&parsed.category==columns.category&&parsed.subcats==columns.subcats&&parsed.size==columns.size&&parsed.created==columns.created,
            "native Spotnet original and stored metadata disagree");
        (parsed,Xml::Kept)
    };
    ensure!(spot.kind().is_some_and(|value|value.as_str()==kind),"native Spotnet kind does not match its original");
    ensure!(xml!=Xml::Kept||spot.imdb_id()==imdb_id,"native Spotnet IMDb identity differs from original");
    Ok(Detail {message_id,spotter_id,imdb_id,spot,xml,removed:None})
}
pub(in crate::pg) fn original(raw:Option<Vec<u8>>,displayed:Option<&str>,limit:usize)->Result<Option<String>> {
    let Some(raw)=raw else {ensure!(displayed.is_none(),"native Spotnet original is missing behind display text");return Ok(None)};
    ensure!(raw.len()<=limit,"native Spotnet original exceeds read admission");
    let original=String::from_utf8(raw).context("native Spotnet original is not UTF8")?;
    // Display is only a derivative. Its presence can never repair lost or
    // malformed original bytes; mismatched pairs are rejected as corruption.
    ensure!(displayed==Some(original.replace('\0',"\u{fffd}").as_str()),"native Spotnet original/display pair differs");
    Ok(Some(original))
}
pub(in crate::pg) fn removal_in(tx:&mut Transaction<'_>,detail:&Detail)->Result<Option<Removed>> {
    let row=tx.query_opt("select at,spotter='' from spot_disposals where message_id=$1 and (spotter='' or (spotter=$2 and at-coalesce(nullif($3::bigint,0),at)<=$4)) order by spotter='' desc,at limit 1",&[&detail.message_id,&detail.spotter_id,&detail.spot.created,&spotnet::store::OWN_DISPOSAL_SECS])?;
    if let Some(row)=row {return Ok(Some(Removed {at:row.try_get(0)?,by:if row.try_get(1)? {RemovedBy::Moderator}else{RemovedBy::Spotter}}))}
    let blocked:bool=tx.query_one("select exists(select 1 from spot_blacklist where spotter=$1)",&[&detail.spotter_id])?.try_get(0)?;
    Ok(blocked.then_some(Removed {at:0,by:RemovedBy::Blacklist}))
}
impl ReadView<'_> {
    pub(crate) fn spot_detail(&self,id:i64)->Result<Option<Detail>> {
        if !self.visible(id)? {return Ok(None)}self.before_query()?;
        let extent=extent_in(&mut self.tx.borrow_mut(),id)?;
        let Some(extent)=extent else {return Ok(None)};
        self.admit_owned(extent.owned)?;self.before_query()?;
        let mut detail=detail_in(&mut self.tx.borrow_mut(),id,&extent)?;
        self.before_query()?;detail.removed=removal_in(&mut self.tx.borrow_mut(),&detail)?;
        self.check_deadline()?;Ok(Some(detail))
    }
    pub(crate) fn spot_tags(&self,ids:&[i64])->Result<HashMap<i64,Tagged>> {
        ensure!(ids.len()<=self.budget.rows,"native Spotnet tag rows exceed read admission");
        self.admit_owned(4096+(ids.len() as u64)*512)?;
        let mut out=HashMap::with_capacity(ids.len());
        for &id in ids {
            let Some(detail)=self.spot_detail(id)? else {continue};if detail.removed.is_some(){continue}
            let spot=detail.spot;out.insert(id,Tagged {message_id:detail.message_id,kind:spot.kind().context("native Spotnet kind absent")?.as_str().into(),
                title:spot.title,poster:spot.poster,tag:spot.tag,spotter_id:detail.spotter_id,imdb_id:detail.imdb_id,subcats:spot.subcats.spotweb()});
        }Ok(out)
    }
    pub(crate) fn spot_view(&self,id:i64)->Result<Option<(Detail,Vec<Comment>,i64)>> {
        let Some(detail)=self.spot_detail(id)? else {return Ok(None)};
        let predicate=spotnet::store::disposed_sql("d","c","spotter_id",None).replace("main.","");
        let spots="select $2::text union select message_id from spots_seen where release_id=$1 and state=$3";
        self.admit_owned((self.budget.rows as u64).checked_add(1).and_then(|n|n.checked_mul(256)).and_then(|n|n.checked_add(4096)).context("native comment descriptor extent overflow")?)?;
        self.before_query()?;
        // Only fixed descriptors cross before admission. A row excess refuses
        // the whole response; it is never a silent truncation of comments.
        let extents=self.tx.borrow_mut().query(&format!("select octet_length(coalesce(poster,'')),octet_length(spotter_id),octet_length(body),octet_length(native_original_body),state from spot_comments c where spot in ({spots}) and not {predicate} order by at,message_id limit $4"),&[&id,&detail.message_id,&spotnet::store::SAVED,&i64::try_from(self.budget.rows.checked_add(1).context("native comment row extent overflow")?)?])?;
        ensure!(extents.len()<=self.budget.rows,"native Spotnet comments exceed read row admission");
        let mut expected=Vec::<[Option<i32>;4]>::with_capacity(extents.len());let mut owned=8192u64;
        for row in extents {
            let values:[Option<i32>;4]=[row.try_get(0)?,row.try_get(1)?,row.try_get(2)?,row.try_get(3)?];
            if row.try_get::<_,i64>(4)?==comments::READ {ensure!(values[3].is_some(),"read comment has no original body");}
            for (at,value) in values.into_iter().enumerate() {
                let bytes=length(value)?;if at>=2 {ensure!(bytes<=(if at==2 {3}else{2})*comments::MAX_BODY as u64,"native comment exceeds original decoding envelope");}
                owned=owned.checked_add(bytes.checked_mul(4).context("native comment extent overflow")?).context("native comment extent overflow")?;
            }owned=owned.checked_add(512).context("native comment extent overflow")?;expected.push(values);
        }
        self.admit_owned(owned)?;self.before_query()?;
        let rows=self.tx.borrow_mut().query(&format!("select at,rating,coalesce(poster,''),spotter_id,state,body,native_original_body from spot_comments c where spot in ({spots}) and not {predicate} order by at,message_id limit $4"),&[&id,&detail.message_id,&spotnet::store::SAVED,&i64::try_from(self.budget.rows.checked_add(1).context("native comment row extent overflow")?)?])?;
        ensure!(rows.len()==expected.len(),"native comment row set changed within snapshot");
        let mut comments=Vec::with_capacity(rows.len());
        for (row,extent) in rows.into_iter().zip(expected) {
            self.check_deadline()?;
            let poster:String=row.try_get(2)?;let spotter_id:Option<String>=row.try_get(3)?;let state:i64=row.try_get(4)?;
            ensure!(matches!(state,comments::WAITING|comments::READ|comments::GONE),"native comment state malformed");
            let displayed:Option<String>=row.try_get(5)?;let raw:Option<Vec<u8>>=row.try_get(6)?;
            ensure!([Some(poster.len()),spotter_id.as_ref().map(String::len),displayed.as_ref().map(String::len),raw.as_ref().map(Vec::len)].into_iter().zip(extent).all(|(a,b)|a.map(|n|n as i32)==b),"native comment extent changed");
            let body=original(raw,displayed.as_deref(),2*comments::MAX_BODY)?;
            ensure!(state!=comments::READ||body.is_some(),"native read comment original is missing");
            comments.push(Comment {at:row.try_get(0)?,rating:row.try_get::<_,i64>(1)?.clamp(0,10) as u8,poster,spotter_id,state,body:body.unwrap_or_default()});
        }
        self.before_query()?;let reports:i64=self.tx.borrow_mut().query_one(&format!("select count(*) from spot_reports where spot in ({spots})"),&[&id,&detail.message_id,&spotnet::store::SAVED])?.try_get(0)?;
        ensure!(reports>=0,"native Spotnet report count malformed");self.check_deadline()?;Ok(Some((detail,comments,reports)))
    }
}

#[cfg(test)]
#[path="native_read_tests.rs"]
mod tests;
