//! PG adapters for the existing whole-post decisions. No row-only shortcut:
//! original names, PAR2 links, beside rows and complementary/XPOST facts are
//! resolved in the same bounded RR snapshot used for candidate observation.
use super::*;
use crate::release_info::{SplitPost,StemCandidate};
use super::storage::{Read,Head,Deferred};
const LINKED:usize=crate::release_info::LINKED_ROWS;
fn par2_only(r:&mut Read<'_,'_>,id:i64)->Result<bool> {
    let rows=r.query("select exists(select 1 from files where release_id=$1),exists(select 1 from files where release_id=$1 and lower(filename) not like '%.par2')",&[&id])?;
    Ok(rows[0].try_get::<_,bool>(0)?&&!rows[0].try_get::<_,bool>(1)?)
}
fn direct_lead(r:&mut Read<'_,'_>,id:i64)->Result<Option<i64>> {Ok(r.query("select lead_id from upload_link where release_id=$1",&[&id])?.first().map(|row|row.try_get(0)).transpose()?)}
fn lead(r:&mut Read<'_,'_>,head:&Head)->Result<Option<i64>> {
    if let Some(id)=direct_lead(r,head.named.id)? {return Ok(Some(id))}
    if !par2_only(r,head.named.id)? {return Ok(None)}let Some(poster)=head.named.poster.as_deref() else{return Ok(None)};
    let sets=r.query("select distinct case when octet_length(set_id)=16 then set_id end,octet_length(set_id) from par2_files where release_id=$1 and sealed=1 limit 8",&[&head.named.id])?;
    for set in sets {ensure!(set.try_get::<_,i32>(1)?==16,"native overview sealed PAR2 set extent invalid");let set:Vec<u8>=set.try_get(0)?;ensure!(set.len()==16,"native overview sealed PAR2 set extent invalid");
        if let Some(row)=r.query("select l.lead_id from upload_link l join releases h on h.id=l.lead_id where l.set_id=$1 and l.recovery=1 and h.poster=$2 order by l.release_id limit 1",&[&set,&poster])?.first(){return Ok(Some(row.try_get(0)?))}}
    Ok(None)
}
pub(super) fn linked(r:&mut Read<'_,'_>,head:&Head)->Result<Option<SplitPost>> {
    let Some(lead)=lead(r,head)? else{return Ok(None)};
    // Sizes of opaque link fields are checked in SQL before driver transfer.
    let rows=r.query("select release_id,recovery,coalesce(wanted,0),case when octet_length(par2_file)<=16 then par2_file end,octet_length(par2_file),case when octet_length(set_id)<=16 then set_id end,octet_length(set_id) from upload_link where lead_id=$1 order by release_id limit 1025",&[&lead])?;
    if rows.len()>LINKED{return Err(Deferred.into())}
    let (mut ids,mut groups,mut size,mut recovery,mut wanted,mut files,mut sets)=(Vec::new(),Vec::new(),0i64,Vec::new(),0i64,HashSet::<Vec<u8>>::new(),Vec::<Vec<u8>>::new());
    for row in rows {let id:i64=row.try_get(0)?;let Some(h)=r.head(id)? else{continue};
        ensure!(row.try_get::<_,Option<i32>>(4)?.is_none_or(|n|n==16)&&row.try_get::<_,Option<i32>>(6)?.is_none_or(|n|n==16),"native overview PAR2 link extent invalid");
        if row.try_get::<_,i64>(1)?!=0 {recovery.push(id);wanted=wanted.max(row.try_get(2)?);if let Some(set)=row.try_get::<_,Option<Vec<u8>>>(5)? {if !sets.contains(&set){sets.push(set);}}}
        else {files.insert(row.try_get::<_,Option<Vec<u8>>>(3)?.unwrap_or_else(||id.to_le_bytes().to_vec()));}
        ids.push(id);groups.push(h.named.group);size=size.checked_add(h.named.bytes).context("native overview linked size overflow")?;
    }
    if !ids.contains(&lead)||ids.len()<2{return Ok(None)}
    let poster=r.head(lead)?.and_then(|h|h.named.poster);
    if let Some(poster)=poster {for set in sets {
        let rows=r.query("select distinct p.release_id from par2_files p join releases h on h.id=p.release_id where p.set_id=$1 and p.sealed=1 and h.poster=$2 order by p.release_id limit 1025",&[&set,&poster])?;
        if rows.len()>LINKED{return Err(Deferred.into())}
        for row in rows {let id:i64=row.try_get(0)?;if ids.contains(&id)||!par2_only(r,id)?{continue;}if ids.len()==LINKED{return Err(Deferred.into())}
            if let Some(h)=r.head(id)? {recovery.push(id);ids.push(id);groups.push(h.named.group);size=size.checked_add(h.named.bytes).context("native overview recovery size overflow")?;}}
    }}
    ids.sort_unstable();recovery.sort_unstable();groups.sort();groups.dedup();let mut complete=files.len() as i64>=wanted;
    for id in &ids {complete&=r.union(&[*id])?.complete_by_files();}
    Ok(Some(SplitPost {ids,groups,size,complete,lead:Some(lead),recovery,beside:Vec::new()}))
}
fn exact(r:&mut Read<'_,'_>,head:&Head)->Result<Option<SplitPost>> {
    let mut video=None;let lower=head.name.to_ascii_lowercase();
    if let Some(ext)=crate::store::VIDEO_EXTENSIONS.iter().find(|ext|lower.ends_with(*ext)) {let stem=&head.name[..head.name.len()-ext.len()];if !stem.trim().is_empty(){video=Some(stem.to_string());}}
    else {
        let mut after=0i64;loop {let rows=r.query("select id,case when octet_length(original_filename)<=65536 then original_filename end,octet_length(original_filename) from files where release_id=$1 and id>$2 order by id limit 16",&[&head.named.id,&after])?;if rows.is_empty(){break;}
            for row in rows {after=row.try_get(0)?;if row.try_get::<_,Option<i32>>(2)?.is_none_or(|n|n>65536){return Err(Deferred.into())}let bytes:Vec<u8>=row.try_get(1)?;r.budget.charge(bytes.len()*3+128)?;let name=String::from_utf8(bytes)?;
                if name.len()>head.name.len()&&name.is_char_boundary(head.name.len())&&name[..head.name.len()]==head.name&&crate::store::VIDEO_EXTENSIONS.iter().any(|ext|name[head.name.len()..].eq_ignore_ascii_case(ext)){video=Some(name);break;}}
            if video.is_some(){break;}
        }
    }
    let (mut same,mut all)=(Vec::new(),Vec::new());
    for name in std::iter::once(head.name.as_str()).chain(video.as_deref()) {same.extend(r.named(name,head.named.poster.as_deref(),true)?);all.extend(r.named(name,None,false)?);}
    for rows in [&same,&all] {ensure!(rows.iter().map(|h|i128::from(h.bytes)).sum::<i128>()<=i128::from(i64::MAX),"native overview post size overflow");}
    crate::release_info::exact_named(head.named.id,&same,&all,&mut |ids|r.union(ids))
}
fn stem(r:&mut Read<'_,'_>,head:&Head)->Result<Option<SplitPost>> {
    let stem=crate::release_info::post_stem(&head.name);if stem.len()<4{return Ok(None)}let Some(lead)=direct_lead(r,head.named.id)? else{return Ok(None)};
    // The legacy stem path requires this exact lead. Check its global1024
    // candidate limit using scalar-only rows, then load only the linked subset.
    let low=format!("{stem}.");let high=format!("{stem}/");
    let count=r.query("select count(*) from(select 1 from releases where name=$1 or(name collate \"C\">=$2 and name collate \"C\"<$3) limit 1025) candidates",&[&stem,&low,&high])?[0].try_get::<_,i64>(0)?;
    if count>LINKED as i64{return Ok(None)}
    let rows=r.query("select release_id from upload_link where lead_id=$1 order by release_id limit 1025",&[&lead])?;if rows.len()>LINKED{return Err(Deferred.into())}
    let mut candidates=Vec::new();for row in rows {if let Some(h)=r.head(row.try_get(0)?)? {
        if crate::filters::group_blocked(&h.named.group)||!crate::release_info::post_stem(&h.name).eq_ignore_ascii_case(&stem){continue;}
        if matches!((head.named.date,h.named.date),(Some(a),Some(b))if (a-b).abs()<=1.0) {candidates.push(StemCandidate {id:h.named.id,group:h.named.group,bytes:h.named.bytes,date:h.named.date});}
    }}
    if !candidates.iter().any(|h|h.id==head.named.id){return Ok(None)}
    ensure!(candidates.iter().map(|h|i128::from(h.bytes)).sum::<i128>()<=i128::from(i64::MAX),"native overview stem size overflow");
    crate::release_info::try_stem_subset(&mut |ids|r.union(ids),&stem,&candidates)
}
fn alone(r:&mut Read<'_,'_>,head:&Head)->Result<Option<SplitPost>> {
    let linked=linked(r,head)?;let exact=exact(r,head)?;let stem=stem(r,head)?;
    let split=match (exact,stem){(Some(a),Some(b))if b.ids.len()>a.ids.len()=>Some(b),(Some(a),_)=>Some(a),(None,b)=>b};
    Ok(match(linked,split){(Some(mut p),Some(mut s))if p.ids.iter().all(|id|s.ids.binary_search(id).is_ok())&&(s.ids.len()>p.ids.len()||(s.complete&&!p.complete))=>{s.lead=p.lead;s.recovery=std::mem::take(&mut p.recovery);Some(s)},(Some(p),_)=>Some(p),(None,s)=>s})
}
pub(super) fn whole_post(r:&mut Read<'_,'_>,id:i64)->Result<Option<SplitPost>> {
    let mut anchor=id;if let Some(row)=r.query("select release_id from beside_files where row_id=$1",&[&id])?.first(){anchor=row.try_get(0)?;}
    let head=r.head(anchor)?.ok_or(SourceChanged)?;let own=alone(r,&head)?;let alone=own.is_none();
    let complete=if alone {let union=r.union(&[anchor])?;head.complete||union.complete()||r.repairable(anchor,&union)?}else{false};
    let mut post=own.unwrap_or(SplitPost {ids:vec![anchor],groups:vec![head.named.group],size:head.named.bytes,complete,lead:None,recovery:Vec::new(),beside:Vec::new()});
    let mut extra=Vec::new();for id in &post.ids {let rows=r.query("select row_id from beside_files where release_id=$1 order by row_id limit 1025",&[id])?;if rows.len()>LINKED{return Err(Deferred.into())}
        for row in rows {let found:i64=row.try_get(0)?;if !post.ids.contains(&found)&&!extra.contains(&found){if post.ids.len()+extra.len()==LINKED{return Err(Deferred.into())}extra.push(found);}}}
    if alone&&extra.is_empty(){return Ok(None)}
    if !extra.is_empty(){let mut all=post.ids.clone();all.extend(&extra);let together=post.lead.is_none()&&r.union(&all)?.complete();
        for id in extra {if let Some(h)=r.head(id)?{post.size=post.size.checked_add(h.named.bytes).context("native overview beside size overflow")?;post.groups.push(h.named.group);post.ids.push(id);post.beside.push(id);}}
        if together&&(alone||post.recovery.is_empty()){post.complete=true;}
    }
    post.ids.sort_unstable();post.groups.sort();post.groups.dedup();if post.groups.len()>16{return Err(Deferred.into())}Ok(Some(post))
}
pub(super) fn post(r:&mut Read<'_,'_>,id:i64)->Result<SplitPost> {
    if let Some(post)=whole_post(r,id)? {return Ok(post)}
    let head=r.head(id)?.ok_or(SourceChanged)?;let complete=r.union(&[id])?.complete();
    Ok(SplitPost {ids:vec![id],groups:vec![head.named.group],size:head.named.bytes,complete,lead:None,recovery:Vec::new(),beside:Vec::new()})
}
