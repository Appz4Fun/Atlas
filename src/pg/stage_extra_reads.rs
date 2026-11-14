//! Remaining checked worker reads on the original owner RR snapshot. No Pool,
//! SQLite connection, caller SQL or best-effort error-to-miss policy lives here.
use anyhow::{Result,ensure};
use postgres::{Transaction,types::ToSql};
use std::collections::{BTreeMap,HashMap,HashSet};
use super::StepReads;
use crate::catalog_facts::FileInfo;
use crate::archive::{beside,link,select};
const BYTES:i64=64*1024*1024;
#[derive(Default)]
struct Budget {bytes:i64,rows:i64}
impl Budget {
    fn charge(&mut self,rows:i64,bytes:i64)->Result<()> {
        ensure!(rows>=0 && bytes>=0,"negative native read extent");
        self.rows=self.rows.checked_add(rows).ok_or_else(||anyhow::anyhow!("native read row overflow"))?;
        self.bytes=self.bytes.checked_add(bytes).and_then(|n|n.checked_add(rows.checked_mul(256)?)).ok_or_else(||anyhow::anyhow!("native read byte overflow"))?;
        ensure!(self.rows<=50000 && self.bytes<=BYTES,"native worker read exceeds admission");Ok(())
    }
}
fn rows(tx:&mut Transaction<'_>,sql:&str,args:&[&(dyn ToSql+Sync)],fields:&[&str],budget:&mut Budget)->Result<Vec<postgres::Row>> {
    let size=if fields.is_empty(){"0::bigint".into()}else{fields.iter().map(|field|format!("coalesce(octet_length(q.\"{field}\")::bigint,0)")).collect::<Vec<_>>().join("+")};
    let metadata=tx.query_one(&format!("select count(*),coalesce(sum({size}),0)::bigint from ({sql}) q"),args)?;
    budget.charge(metadata.try_get(0)?,metadata.try_get(1)?)?;
    Ok(tx.query(sql,args)?)
}
fn files(tx:&mut Transaction<'_>,id:i64,limit:i64,budget:&mut Budget)->Result<Vec<FileInfo>> {
    ensure!((0..=5000).contains(&limit),"native file horizon");
    rows(tx,"select id,original_filename,subject,subject_part,expected,file_total,distinct_parts,touched_at from files where release_id=$1 order by id limit $2",&[&id,&limit],&["original_filename","subject"],budget)?.into_iter().map(|r|Ok(FileInfo {
        id:r.try_get(0)?,filename:String::from_utf8(r.try_get(1)?)?,subject:r.try_get(2)?,subject_part:r.try_get(3)?,expected:r.try_get(4)?,file_total:r.try_get(5)?,parts_seen:r.try_get(6)?,touched_at:r.try_get(7)?,sealed:false,
    })).collect()
}
fn taken(tx:&mut Transaction<'_>,id:i64)->Result<bool> {Ok(tx.query_one("select exists(select 1 from beside_files where row_id=$1) or exists(select 1 from upload_link where release_id=$1)",&[&id])?.try_get(0)?)}
fn lone(tx:&mut Transaction<'_>,id:i64,fits:impl Fn(&FileInfo)->bool,budget:&mut Budget)->Result<Option<FileInfo>> {
    let list=files(tx,id,2,budget)?;let [file]=&list[..] else{return Ok(None)};
    if file.expected.unwrap_or(1)>2 || !fits(file) || taken(tx,id)? {return Ok(None)}Ok(Some(file.clone()))
}
fn window_seconds(window:(&str,&str,&str))->Option<(i64,i64,i64)> {
    Some((crate::dates::naive_timestamp(window.0)?,crate::dates::naive_timestamp(window.1)?,crate::dates::naive_timestamp(window.2)?))
}
fn named(tx:&mut Transaction<'_>,me:i64,name:&str,window:(&str,&str,&str),budget:&mut Budget)->Result<Vec<(i64,String,Option<String>)>> {
    budget.charge(1,i64::try_from(name.len())?.checked_mul(5).ok_or_else(||anyhow::anyhow!("native name extent overflow"))?)?;
    let Some((from,to,center))=window_seconds(window) else{return Ok(Vec::new())};
    let normalized=name.replace('\0',"�");let high=format!("{normalized}\u{7f}");let mut raw_high=name.as_bytes().to_vec();raw_high.push(127);
    rows(tx,"select id,original_name,poster from releases where name >=$1 and name<$2 and original_name >=$3 and original_name<$4 and id<>$5 and posted_clock between $6 and $7 order by abs(posted_clock-$8),id limit $9",
      &[&normalized,&high,&name.as_bytes(),&raw_high,&me,&from,&to,&center,&beside::NAME_ROWS],&["original_name","poster"],budget)?.into_iter().map(|r|Ok((r.try_get(0)?,String::from_utf8(r.try_get(1)?)?,r.try_get(2)?))).collect()
}
fn start(tx:&mut Transaction<'_>)->Result<()> {tx.query_one("select set_config('statement_timeout','10000',true)",&[])?;Ok(())}

// Stream admitted rows without retaining duplicate String payloads. Tree
// lookup bounds comparisons even for adversarial names; first-seen positions
// restore the original local-then-linked order without cloning retained keys.
struct Par2Names<T> {unique:BTreeMap<T,usize>}
impl<T:Ord> Par2Names<T> {
    fn new()->Self {Self {unique:BTreeMap::new()}}
    fn insert(&mut self,name:T) {
        let position=self.unique.len();self.unique.entry(name).or_insert(position);
    }
    fn finish(self)->Vec<T> {
        let mut names=self.unique.into_iter().map(|(name,position)|(position,name)).collect::<Vec<_>>();
        names.sort_unstable_by_key(|(position,_)|*position);
        names.into_iter().map(|(_,name)|name).collect()
    }
}

impl StepReads {
    pub(crate) fn archive_requires_password_list(&self,id:i64)->Result<bool> {self.read(|tx|password_list(tx,id))}
    pub(crate) fn nfo_password_rerun_flags(&self,id:i64)->Result<(bool,bool)> {self.read(|tx|Ok((password_list(tx,id)?,tx.query_one("select exists(select 1 from release_probe where release_id=$1 and info like '%\"encrypted\":true%')",&[&id])?.try_get(0)?)))}
    pub(crate) fn par2_file_names(&self,id:i64)->Result<Vec<String>> {self.read(|tx| {
        start(tx)?;let mut budget=Budget::default();let mut names=Par2Names::new();
        for sql in ["select release_id,name from par2_files where release_id=$1 order by file_id","select pf.release_id,pf.name from upload_link ul join upload_link rec on rec.lead_id=ul.lead_id join par2_files pf on pf.release_id=rec.release_id where ul.release_id=$1 order by rec.release_id,pf.file_id"] {
            let selected=rows(tx,sql,&[&id],&["name"],&mut budget)?;
            self.observe_in(tx,&selected.iter().map(|r|r.try_get(0)).collect::<std::result::Result<Vec<i64>,_>>()?)?;
            for r in selected {names.insert(r.try_get::<_,String>(1)?);}
        }Ok(names.finish())
    })}
    pub(crate) fn par2_names(&self,id:i64,input:&[FileInfo])->Result<HashMap<String,String>> {self.read(|tx| {
        start(tx)?;let mut budget=Budget::default();budget.charge(input.len() as i64,input.iter().map(|f|f.filename.len() as i64).sum())?;
        let mut out=HashMap::new();
        let selected=rows(tx,"select f.original_filename,pf.name,pf.release_id from upload_link ul join files f on f.release_id=ul.release_id join par2_files pf on pf.file_id=ul.par2_file where ul.release_id=$1 and ul.par2_file is not null order by f.id,pf.release_id",&[&id],&["original_filename","name"],&mut budget)?;
        self.observe_in(tx,&selected.iter().map(|r|r.try_get(2)).collect::<std::result::Result<Vec<i64>,_>>()?)?;
        for r in selected {out.insert(String::from_utf8(r.try_get(0)?)?,r.try_get(1)?);}
        let pf=rows(tx,"select name,size from par2_files where release_id=$1 order by file_id",&[&id],&["name"],&mut budget)?.into_iter().map(|r|Ok((r.try_get(0)?,u64::try_from(r.try_get::<_,i64>(1)?)?))).collect::<Result<Vec<_>>>()?;
        let (unmatched_files,unmatched_par2)=select::prepare_par2_names(&mut out,input,pf);
        if unmatched_files.is_empty() || unmatched_par2.is_empty() {return Ok(out)}
        let mut wire=HashMap::new();for r in rows(tx,"select f.id,coalesce((select sum(bytes)::bigint from segments where file_id=f.id),0)::bigint from files f where f.release_id=$1 order by id",&[&id],&[],&mut budget)? {wire.insert(r.try_get(0)?,u64::try_from(r.try_get::<_,i64>(1)?)?);}
        Ok(select::finish_par2_names(out,unmatched_files,unmatched_par2,&wire))
    })}
    pub(crate) fn linked_image_files(&self,id:i64)->Result<Vec<FileInfo>> {self.read(|tx| {
        let mut budget=Budget::default();let mut out=Vec::new();
        for sql in ["select release_id from upload_link where lead_id=$1 and release_id<>$1 order by release_id limit 16","select row_id from beside_files where release_id=$1 order by row_id limit 16"] {
            let selected=tx.query(sql,&[&id])?;self.observe_in(tx,&selected.iter().map(|row|row.try_get(0)).collect::<std::result::Result<Vec<i64>,_>>()?)?;
            for row in selected {let other=row.try_get(0)?;out.extend(files(tx,other,50,&mut budget)?);}
        }Ok(out)
    })}
    pub(crate) fn requested_image_file(&self,id:i64,name:&str)->Result<Option<FileInfo>> {self.read(|tx| {
        let row=tx.query_opt("select f.release_id from files f where f.release_id in(select release_id from upload_link where lead_id=$1 union select row_id from beside_files where release_id=$1) and f.original_filename=$2 order by f.release_id,f.id limit 1",&[&id,&name.as_bytes()])?;
        let Some(row)=row else{return Ok(None)};let other=row.try_get(0)?;self.observe_in(tx,&[other])?;Ok(files(tx,other,4096,&mut Budget::default())?.into_iter().find(|f|f.filename==name))
    })}
    pub(crate) fn beside_rows_with_files(&self,id:i64)->Result<Vec<(i64,FileInfo)>> {self.read(|tx| {
        let mut budget=Budget::default();let mut out=Vec::new();let selected=tx.query("select row_id from beside_files where release_id=$1 order by row_id limit 16",&[&id])?;
        self.observe_in(tx,&selected.iter().map(|r|r.try_get(0)).collect::<std::result::Result<Vec<i64>,_>>()?)?;
        for r in selected {let other=r.try_get(0)?;out.extend(files(tx,other,2,&mut budget)?.into_iter().map(|f|(other,f)));}Ok(out)
    })}
    pub(crate) fn find_beside_nfo(&self,id:i64,stem:&str,posted:Option<&str>)->Result<Option<(i64,FileInfo)>> {self.read(|tx| {
        start(tx)?;let Some(posted)=posted else{return Ok(None)};let Some((from,to))=link::beside_window(posted,beside::NFO_SECS) else{return Ok(None)};
        let mut budget=Budget::default();let want=format!("{stem}.nfo");for (other,name,_) in named(tx,id,stem,(&from,&to,posted),&mut budget)? {
            self.observe_in(tx,&[other])?;
            if name!=stem && !name.strip_prefix(stem).is_some_and(|rest|rest.starts_with(" [")){continue}
            if let Some(file)=lone(tx,other,|f|f.filename.eq_ignore_ascii_case(&want),&mut budget)? {return Ok(Some((other,file)))}
        }Ok(None)
    })}
    pub(crate) fn find_nfo_owner(&self,id:i64,stem:&str,posted:Option<&str>)->Result<Option<i64>> {self.read(|tx| {
        start(tx)?;let Some(posted)=posted else{return Ok(None)};let Some((from,to))=link::beside_window(posted,beside::NFO_SECS) else{return Ok(None)};
        let mut budget=Budget::default();for (other,name,_) in named(tx,id,stem,(&from,&to,posted),&mut budget)? {
            self.observe_in(tx,&[other])?;
            let fits=name==stem || name.strip_prefix(stem).is_some_and(|rest|rest.starts_with(" [")) || crate::catalog_facts::VIDEO_EXTENSIONS.iter().any(|ext|name.strip_suffix(ext).is_some_and(|base|base==stem));
            if fits && files(tx,other,64,&mut budget)?.iter().any(|f|!crate::archive::beside::is_side_file(&f.filename)) {return Ok(Some(other))}
        }Ok(None)
    })}
    pub(crate) fn post_posters(&self,name:&str,stem:&str,own:Option<&str>,posted:&str)->Result<(Vec<String>,Vec<i64>)> {self.read(|tx| {
        start(tx)?;let mut posters:Vec<String>=own.map(str::to_owned).into_iter().collect();let mut ids=Vec::new();let Some((from,to))=link::beside_window(posted,86400) else{return Ok((posters,ids))};let mut budget=Budget::default();
        for look in std::iter::once(name).chain((name!=stem).then_some(stem)) {for (id,other,poster) in named(tx,-1,look,(&from,&to,posted),&mut budget)? {
            if other!=name && other!=stem {continue}self.observe_in(tx,&[id])?;if !ids.contains(&id){ids.push(id)}if let Some(poster)=poster.filter(|p|!posters.contains(p)){posters.push(poster)}
        }}Ok((posters,ids))
    })}
    pub(crate) fn link_beside_candidates(&self,id:i64,name:&str,window:(&str,&str,&str))->Result<Vec<link::Candidate>> {self.read(|tx| {
        start(tx)?;let mut budget=Budget::default();let Some((from,to,center))=window_seconds(window) else{return Ok(Vec::new())};
    let normalized=name.replace('\0',"�");let high=format!("{normalized}\u{7f}");let mut raw_high=name.as_bytes().to_vec();raw_high.push(127);
        let selected=rows(tx,"select r.id,r.size,r.parts,f.id as file_id,f.original_filename,f.expected from releases r join files f on f.release_id=r.id where r.name>=$1 and r.name<$2 and r.original_name>=$3 and r.original_name<$4 and r.id<>$5 and r.posted_clock between $6 and $7 and translate(encode(f.original_filename,'escape'),'ABCDEFGHIJKLMNOPQRSTUVWXYZ','abcdefghijklmnopqrstuvwxyz')=translate(encode($3::bytea,'escape'),'ABCDEFGHIJKLMNOPQRSTUVWXYZ','abcdefghijklmnopqrstuvwxyz') and coalesce(f.expected,1)<=2 and not exists(select 1 from files g where g.release_id=r.id and g.id<>f.id) and not exists(select 1 from upload_link l where l.release_id=r.id) and not exists(select 1 from beside_files b join releases o on o.id=b.release_id where b.row_id=r.id and b.release_id<>$5) and not exists(select 1 from upload_link_tried t where t.release_id=$5 and t.candidate_id=r.id) order by abs(r.posted_clock-$8),r.id limit $9",
          &[&normalized,&high,&name.as_bytes(),&raw_high,&id,&from,&to,&center,&(link::BESIDE_CHECKS as i64)],&["original_filename"],&mut budget)?;
        self.observe_in(tx,&selected.iter().map(|r|r.try_get(0)).collect::<std::result::Result<Vec<i64>,_>>()?)?;
        selected.into_iter().map(|r|Ok(link::Candidate::from_read(r.try_get(0)?,r.try_get(1)?,r.try_get(2)?,candidate_file(&r,3)?,0.0,None))).collect()
    })}
    pub(crate) fn link_candidates(&self,release:&crate::postprocess::step::ReleaseInfo,sizes:(u64,u64),set:Option<[u8;16]>)->Result<Vec<link::Candidate>> {self.read(|tx| {
        start(tx)?;let Some(posted)=release.posted_date.as_deref().and_then(crate::dates::naive_timestamp) else{return Ok(Vec::new())};let mut budget=Budget::default();
        let low=release.id.saturating_sub(link::ID_WINDOW);let high=release.id.saturating_add(link::ID_WINDOW);let set=set.map(|v|v.to_vec());
        let selected=rows(tx,"select r.id,r.size,r.parts,f.id as file_id,f.original_filename,f.expected,(abs(r.posted_clock-$4)::double precision/86400.0) as apart,t.first_date,t.last_date,t.clock_first,t.clock_last from releases r join files f on f.release_id=r.id left join file_timing t on t.file_id=f.id where r.id between $1 and $2 and r.id<>$3 and (abs(r.posted_clock-$4)::double precision/86400.0)<=$5 and not exists(select 1 from files g where g.release_id=r.id and g.id<>f.id) and not exists(select 1 from upload_link l where l.release_id=r.id and l.lead_id<>r.id) and not exists(select 1 from upload_link_tried t where t.release_id=$3 and t.candidate_id=r.id) and r.size>0 and r.parts>0 and(r.display_name is null or exists(select 1 from upload_link m where m.lead_id=r.id and m.recovery=1 and m.set_id=$9)) and f.expected between $7::double precision*1.005*r.parts/nullif(r.size,0)-2 and $8::double precision*1.08*r.parts/nullif(r.size,0)+2 order by abs(r.posted_clock-$4),r.id limit $6",
          &[&low,&high,&release.id,&posted,&link::DAY_WINDOW,&(link::MAX_CANDIDATES as i64),&(sizes.0 as f64),&(sizes.1 as f64),&set],&["original_filename"],&mut budget)?;
        self.observe_in(tx,&selected.iter().map(|r|r.try_get(0)).collect::<std::result::Result<Vec<i64>,_>>()?)?;
        selected.into_iter().map(|r|Ok(link::Candidate::from_read(r.try_get(0)?,r.try_get(1)?,r.try_get(2)?,candidate_file(&r,3)?,r.try_get::<_,f64>(6)?,link::Clock::of(r.try_get(7)?,r.try_get(8)?,r.try_get(9)?,r.try_get(10)?)))).collect()
    })}
    pub(crate) fn find_thumbnail(&self,id:i64,total:i64,slots:&HashSet<i64>,posters:&[String],posted:&str)->Result<Option<i64>> {self.read(|tx| {
        start(tx)?;let Some((from,to))=link::beside_window(posted,link::BESIDE_SECS) else{return Ok(None)};let mut budget=Budget::default();budget.charge(posters.len() as i64,posters.iter().map(|p|p.len() as i64).sum())?;
        let low=id.saturating_sub(link::ID_WINDOW);let high=id.saturating_add(link::ID_WINDOW);let mut ids=Vec::new();let mut have=HashSet::new();
        // The prefix..prefix+DEL range is bytewise, as in the original SQLite
        // lookup. A default-locale poster index cannot prove that access path.
        // Without a complete compatible index, keep the bounded ID fallback.
        let indexed:bool=tx.query_one(r#"select exists(
            select 1 from pg_index i join pg_class c on c.oid=i.indexrelid
            join pg_am m on m.oid=c.relam
            join pg_attribute a on a.attrelid=i.indrelid and a.attnum=i.indkey[0]
            join pg_opclass o on o.oid=i.indclass[0]
            where i.indexrelid=to_regclass(format('%I.idx_release_poster',current_schema()))
              and i.indrelid=to_regclass(format('%I.releases',current_schema()))
              and a.attname='poster' and m.amname='btree'
              and o.opcname='text_ops' and o.opcnamespace='pg_catalog'::regnamespace
              and i.indcollation[0]='pg_catalog."C"'::regcollation
              and i.indisvalid and i.indisready and i.indislive and i.indpred is null
        )"#,&[])?.try_get(0)?;
        if indexed {
            for poster in posters {
                for row in rows(tx,"select id from releases where poster collate \"C\"=$1 and id between $2 and $3 order by id limit $4",&[&poster,&low,&high,&(beside::SCANNED as i64)],&[],&mut budget)? {let other:i64=row.try_get(0)?;if other!=id&&have.insert(other){ids.push(other)}}
                if let Some((local,_))=poster.split_once('@') && local.is_ascii() && local.len()>=beside::POSTER_PREFIX+2 && !local.contains(['<',' ','"']) {
                    let prefix=&local[..beside::POSTER_PREFIX];let end=format!("{prefix}\u{7f}");
                    for row in rows(tx,"select id from releases where poster collate \"C\">=$1 and poster collate \"C\"<$2 order by poster collate \"C\",id limit $3",&[&prefix,&end,&(beside::SCANNED as i64)],&[],&mut budget)? {let other:i64=row.try_get(0)?;if (low..=high).contains(&other)&&other!=id&&have.insert(other){ids.push(other)}}
                }
            }
        }
        if !indexed || posters.iter().any(|p|p.contains('<')) {
            for sql in ["select id,original_name from releases where id<$1 and id>=$2 order by id desc limit $3","select id,original_name from releases where id>$1 and id<=$2 order by id limit $3"] {
                let bound=if sql.contains("desc"){low}else{high};
                for r in rows(tx,sql,&[&id,&bound,&beside::NEIGHBOURS],&["original_name"],&mut budget)? {let other=r.try_get(0)?;let name=String::from_utf8(r.try_get(1)?)?;if beside::picture_named(&name)&&have.insert(other){ids.push(other)}}
            }
        }
        self.observe_in(tx,&ids)?;let Some((from,to,center))=window_seconds((&from,&to,posted)) else{return Ok(None)};let mut best:Option<(f64,i64)>=None;
        for other in ids {
            let found=rows(tx,"select original_name,poster,(abs(posted_clock-$2)::double precision/86400.0) as apart from releases where id=$1 and posted_clock between $3 and $4",&[&other,&center,&from,&to],&["original_name","poster"],&mut budget)?;
            let Some(r)=found.first() else{continue};let name=String::from_utf8(r.try_get(0)?)?;let poster:String=r.try_get(1)?;
            if !beside::picture_named(&name)||!posters.iter().any(|mine|beside::posters_akin(mine,&poster)){continue}
            if lone(tx,other,|f|crate::images::is_image_name(&f.filename)&&f.subject.as_deref().and_then(beside::counter).is_some_and(|(slot,n)|n==total&&(1..=total).contains(&slot)&&!slots.contains(&slot)),&mut budget)?.is_none(){continue}
            let apart:f64=r.try_get(2)?;if best.is_none_or(|(distance,_)|apart<distance){best=Some((apart,other))}
        }Ok(best.map(|(_,id)|id))
    })}
    pub(crate) fn same_mux_media(&self,id:i64,info:&crate::probe::ProbeInfo,stamp:u64)->Result<Option<crate::probe::step::SameMux>> {
        self.read(|tx| {
            start(tx)?;
            let Some(mux)=info.media.as_ref().and_then(crate::keys::mux_text) else{return Ok(None)};
            let key=crate::keys::as_key(crate::keys::digest8(mux.as_bytes()));let mut budget=Budget::default();
            let selected=rows(tx,"select p.release_id,p.info,r.group_name,p.moviehash,c.mime,c.sha256 from release_keys k join release_probe p on p.release_id=k.release_id join releases r on r.id=k.release_id left join release_cover c on c.release_id=p.release_id where k.mux=$1 and p.release_id<>$2 order by p.release_id desc limit 16",
                &[&key,&id],&["info","group_name","moviehash","mime","sha256"],&mut budget)?;
            // Candidate facts, joined cover presence and their monotonic source
            // revisions share this original-owner RR. Settlement locks/rechecks
            // these participants even when the result only writes its own row.
            self.observe_in(tx,&selected.iter().map(|row|row.try_get(0)).collect::<std::result::Result<Vec<i64>,_>>()?)?;
            let mut remaining=32u64<<20;
            for row in selected {
                if self.owner.stop.load(std::sync::atomic::Ordering::Acquire) {return Err(crate::pg::outage::AttemptEnded.into())}
                let other:i64=row.try_get(0)?;let json:String=row.try_get(1)?;let group:String=row.try_get(2)?;
                let moviehash:Option<String>=row.try_get(3)?;let mime:Option<String>=row.try_get(4)?;let sha:Option<String>=row.try_get(5)?;
                ensure!(mime.is_some()==sha.is_some(),"native retained cover descriptor is incomplete");
                if let Some(reused)=crate::probe::step::native_same_mux_candidate(id,other,&group,info,&json,moviehash.as_deref(),mime.as_deref().zip(sha.as_deref()),stamp,&mut remaining)? {
                    return Ok(Some(reused));
                }
            }
            // Known ineligible evidence stays preserved. None only declines
            // metadata reuse; database/decode/admission errors always propagate.
            Ok(None)
        })
    }
    pub(crate) fn posted_age_days(&self,posted:&str)->Result<Option<f64>> {self.read(|tx| {
        let now:f64=tx.query_one("select extract(epoch from current_timestamp)::double precision",&[])?.try_get(0)?;
        Ok(posted_epoch(posted,now).map(|at|(now-at)/86400.0))
    })}
}
fn password_list(tx:&mut Transaction<'_>,id:i64)->Result<bool> {Ok(tx.query_one("select exists(select 1 from archive_release ar where ar.release_id=$1 and ((ar.names_hidden=1 and not exists(select 1 from archive_inner_files f where f.release_id=ar.release_id and f.source in(1,2,3))) or (ar.password=1 and exists(select 1 from archive_inner_files f where f.release_id=ar.release_id and f.source in(1,2,3) and lower(f.name) like '%.nzb') and not exists(select 1 from archive_inner_files f where f.release_id=ar.release_id and f.source=5))))",&[&id])?.try_get(0)?)}
fn posted_epoch(value:&str,now:f64)->Option<f64> {
    if value=="now" {return Some(now)}
    if let Ok(julian)=value.parse::<f64>() {return (julian.is_finite()&&(0.0..5373484.5).contains(&julian)).then_some((julian-2440587.5)*86400.0)}
    if let Ok(time)=chrono::DateTime::parse_from_rfc3339(value) {return Some(time.timestamp_micros() as f64/1e6)}
    for format in ["%Y-%m-%d %H:%M:%S%.f","%Y-%m-%dT%H:%M:%S%.f","%Y-%m-%d %H:%M","%Y-%m-%dT%H:%M"] {if let Ok(time)=chrono::NaiveDateTime::parse_from_str(value,format) {return Some(time.and_utc().timestamp_micros() as f64/1e6)}}
    for format in ["%H:%M:%S%.f","%H:%M"] {if let Ok(time)=chrono::NaiveTime::parse_from_str(value,format) {return Some(chrono::NaiveDate::from_ymd_opt(2000,1,1)?.and_time(time).and_utc().timestamp_micros() as f64/1e6)}}
    chrono::NaiveDate::parse_from_str(value,"%Y-%m-%d").ok()?.and_hms_opt(0,0,0).map(|time|time.and_utc().timestamp() as f64)
}

fn candidate_file(r:&postgres::Row,at:usize)->Result<FileInfo> {Ok(FileInfo {id:r.try_get(at)?,filename:String::from_utf8(r.try_get(at+1)?)?,subject:None,subject_part:None,expected:r.try_get(at+2)?,file_total:None,parts_seen:0,touched_at:None,sealed:false})}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::postprocess::{data::StepData,step::StepError};

    #[test]
    fn linked_par2_names_preserve_first_occurrence_and_exact_equality() {
        let mut names=Par2Names::new();
        for batch in [vec!["z.mkv","A.mkv","","a\0b.mkv"],vec!["A.mkv","a.mkv","a\0b.mkv","é.mkv","e\u{301}.mkv","","z.mkv"]] {
            for name in batch {names.insert(name.to_owned());}
        }
        assert_eq!(names.finish(),["z.mkv","A.mkv","","a\0b.mkv","a.mkv","é.mkv","e\u{301}.mkv"]);
        assert!(Par2Names::<String>::new().finish().is_empty());
    }

    #[test]
    fn linked_par2_name_deduplication_has_a_comparison_bound() {
        use std::{cell::Cell,cmp::Ordering,rc::Rc};
        struct CountedName {name:String,comparisons:Rc<Cell<usize>>,live:Rc<Cell<usize>>}
        impl Drop for CountedName {
            fn drop(&mut self) {self.live.set(self.live.get()-1);}
        }
        impl Ord for CountedName {
            fn cmp(&self,other:&Self)->Ordering {
                self.comparisons.set(self.comparisons.get()+1);
                self.name.cmp(&other.name)
            }
        }
        impl PartialOrd for CountedName {
            fn partial_cmp(&self,other:&Self)->Option<Ordering> {Some(self.cmp(other))}
        }
        impl PartialEq for CountedName {
            fn eq(&self,other:&Self)->bool {self.cmp(other)==Ordering::Equal}
        }
        impl Eq for CountedName {}

        // The reported linked-inventory shape, plus a fully distinct admitted
        // inventory. Count actual key comparisons rather than elapsed time.
        for (members,files) in [(64usize,512usize),(1,50_000)] {
            let comparisons=Rc::new(Cell::new(0));let live=Rc::new(Cell::new(0));let mut names=Par2Names::new();
            for _ in 0..members {
                for file in (0..files).rev() {
                    live.set(live.get()+1);
                    names.insert(CountedName {name:format!("inventory-file-{file:05}.mkv"),comparisons:comparisons.clone(),live:live.clone()});
                    assert!(live.get()<=files,"duplicates must not retain extra owned payloads");
                }
            }
            let actual=names.finish().into_iter().map(|mut name|std::mem::take(&mut name.name)).collect::<Vec<_>>();
            assert_eq!(actual,(0..files).rev().map(|file|format!("inventory-file-{file:05}.mkv")).collect::<Vec<_>>());
            assert_eq!(live.get(),0);
            assert!(comparisons.get()>=members*files-files,"the counted insertion path must actually run");
            assert!(comparisons.get()<=members*files*64,"linked inventories must not require a linear scan for every name");
        }
    }

    #[test]
    fn native_posted_dates_match_the_sqlite_age_oracle() {
        let now=1_800_000_000.0;
        // Fixed UTC seconds preserve the previous age-oracle cases independently
        // of the native date parser and without retaining a SQLite dependency.
        for (value,expected) in [
            ("2026-11-13",Some(1794528000.0)),
            ("2026-11-13 12:34:56",Some(1794573296.0)),
            ("2026-11-13T12:34:56.125Z",Some(1794573296.125)),
            ("2026-11-13T14:34:56+02:00",Some(1794573296.0)),
            ("12:34:56",Some(946730096.0)),
            ("2451544.5",Some(946684800.0)),
            ("invalid",None),
        ] {
            let actual=posted_epoch(value,now);
            match (expected,actual) {(Some(a),Some(b))=>assert!((a-b).abs()<0.001,"{value}: {a} != {b}"),(None,None)=>{},other=>panic!("{value}: {other:?}")}
        }
        assert_eq!(posted_epoch("now",now),Some(now));
        let mut budget=Budget::default();assert!(budget.charge(i64::MAX,0).is_err());
        let mut budget=Budget::default();assert!(budget.charge(1,BYTES).is_err(),"row envelope is part of admission");
    }

    fn add(runtime:&crate::pg::indexer_runtime::Runtime,name:&str,filename:&str)->i64 {
        runtime.owner.with(|client| {
            crate::pg::release_writer::save(client,&[crate::parser::Release {name:name.into(),group:"example.group".into(),poster:"poster@example.test".into(),date:"2026-11-13 12:00:00".into(),size:50,articles:vec![crate::parser::Article {filename:Some(filename.into()),subject:format!("\"{filename}\" yEnc (1/1)"),message_id:format!("<{name}@example.test>"),part:Some(1),total_parts:Some(1),bytes:50,..Default::default()}],..Default::default()}],1)?;
            Ok(client.query_one("select id from releases where original_name=$1",&[&name.as_bytes()])?.get(0))
        }).unwrap()
    }

    #[test]
    fn native_extra_reads_use_real_owner_raw_filenames_and_selected_related_rows() {
        let Some(schema)=crate::pg::testing::Schema::new() else{return};
        let runtime=crate::pg::stage_queue_tests::runtime(&schema);
        let own=add(&runtime,"Film","Film.mkv");let nfo=add(&runtime,"Film [nfo]","Film.nfo");
        let image=add(&runtime,"Film [picture]","pic\0name.jpg");
        runtime.owner.with(|client| {
            client.execute("insert into par2_files(release_id,file_id,set_id,md5,md5_16k,size,name) values($1,decode(repeat('01',16),'hex'),decode(repeat('02',16),'hex'),decode(repeat('03',16),'hex'),decode(repeat('04',16),'hex'),50,'Film.MKV')",&[&own])?;
            client.execute("insert into upload_link(release_id,lead_id) values($1,$2)",&[&image,&own])?;
            client.execute("insert into archive_release(release_id,password,names_hidden) values($1,0,1)",&[&own])?;
            client.execute("insert into release_probe(release_id,info) values($1,'{\"encrypted\":true}')",&[&own])?;Ok(())
        }).unwrap();
        let reads=StepReads::for_owner(&runtime.owner).unwrap();let data=StepData::Native(&reads);
        let input=reads.files(own,5000).unwrap();
        assert_eq!(data.par2_names(own,&input).unwrap().get("Film.mkv").map(String::as_str),Some("Film.MKV"));
        assert_eq!(data.par2_file_names(own).unwrap(),["Film.MKV"]);
        assert_eq!(data.nfo_password_rerun_flags(own).unwrap(),(true,true));
        let linked=data.linked_image_files(own).unwrap();assert_eq!(linked.len(),1);assert_eq!(linked[0].filename,"pic\0name.jpg");
        assert_eq!(data.requested_image_file(own,"pic\0name.jpg").unwrap().unwrap().filename,"pic\0name.jpg");
        assert!(data.requested_image_file(own,"pic�name.jpg").unwrap().is_none(),"normalized aliases cannot replace original bytes");
        let (other,file)=data.find_beside_nfo(own,"Film",Some("2026-11-13 12:00:00")).unwrap().unwrap();
        assert_eq!(other,nfo);assert_eq!(file.filename,"Film.nfo");
        assert_eq!(data.find_nfo_owner(nfo,"Film",Some("2026-11-13 12:00:00")).unwrap(),Some(own));
        let (posters,ids)=data.post_posters("Film","Film",None,"2026-11-13 12:00:00").unwrap();
        assert_eq!(posters,["poster@example.test"]);assert_eq!(ids,[own]);
        runtime.owner.close();
        assert!(matches!(data.par2_file_names(own),Err(StepError::Native(_))),"ended owner cannot become empty facts");
        assert!(matches!(data.linked_image_files(own),Err(StepError::Native(_))));
    }

    #[test]
    fn native_extra_reads_propagate_malformed_raw_names_instead_of_display_aliases() {
        let Some(schema)=crate::pg::testing::Schema::new() else{return};
        let runtime=crate::pg::stage_queue_tests::runtime(&schema);let own=add(&runtime,"Film","Film.mkv");
        let image=add(&runtime,"Film [picture]","pic.jpg");
        runtime.owner.with(|client| {
            client.execute("insert into upload_link(release_id,lead_id) values($1,$2)",&[&image,&own])?;
            client.execute("update files set original_filename=decode('ff','hex') where release_id=$1",&[&image])?;Ok(())
        }).unwrap();
        let reads=StepReads::for_owner(&runtime.owner).unwrap();let data=StepData::Native(&reads);
        assert!(matches!(data.linked_image_files(own),Err(StepError::Native(_))));runtime.owner.close();
    }

    #[test]
    fn native_worker_date_windows_keep_poster_wall_clock_and_preserve_raw_offsets() {
        let Some(schema)=crate::pg::testing::Schema::new() else{return};
        let runtime=crate::pg::stage_queue_tests::runtime(&schema);
        let ids=runtime.owner.with(|client| {
            for (name,file,date,mid) in [
                ("Film","Film.mkv","Fri, 13 Nov 2026 12:00:00 +1200","<wall-own@example.test>"),
                ("Film [nfo]","Film.nfo","Fri, 13 Nov 2026 12:00:00 -1200","<wall-nfo@example.test>"),
            ] {
                crate::pg::release_writer::save(client,&[crate::parser::Release {name:name.into(),group:"example.group".into(),date:date.into(),articles:vec![crate::parser::Article {filename:Some(file.into()),message_id:mid.into(),part:Some(1),total_parts:Some(1),bytes:1,..Default::default()}],..Default::default()}],1)?;
            }
            Ok(client.query("select id from releases order by id",&[])?.into_iter().map(|row|row.get::<_,i64>(0)).collect::<Vec<_>>())
        }).unwrap();
        let reads=StepReads::for_owner(&runtime.owner).unwrap();let own=reads.snapshot(ids[0]).unwrap().unwrap();
        assert_eq!(own.info.posted_date.as_deref(),Some("2026-11-13 12:00:00"));
        let found=reads.find_beside_nfo(ids[0],"Film",own.info.posted_date.as_deref()).unwrap().unwrap();assert_eq!(found.0,ids[1]);
        runtime.owner.with(|client| {
            let rows=client.query("select posted_date,posted_at,posted_clock from releases order by id",&[])?;
            assert_eq!(rows[0].get::<_,String>(0),"Fri, 13 Nov 2026 12:00:00 +1200");
            assert_eq!(rows[1].get::<_,String>(0),"Fri, 13 Nov 2026 12:00:00 -1200");
            assert_eq!(rows[0].get::<_,i64>(2),rows[1].get::<_,i64>(2));
            assert_eq!(rows[1].get::<_,i64>(1)-rows[0].get::<_,i64>(1),86400);Ok(())
        }).unwrap();runtime.owner.close();
    }
}

#[cfg(test)]
#[path="stage_same_mux_tests.rs"]
mod same_mux_tests;
