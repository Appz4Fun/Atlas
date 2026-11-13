//! Fixed native snapshot reads. Payload lengths are admitted before transfer;
//! scalar-only candidate queries and paged bitmaps never load whole headers.
use super::*;
use postgres::{Row,Transaction,types::ToSql};
use crate::release_info::{Named,UnionFiles};
const TEXT:usize=65536;
pub(super) const PLAN_BYTES:usize=16*1024*1024;
#[derive(Debug)]
pub(super) struct Deferred;
impl std::fmt::Display for Deferred {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.write_str("native overview snapshot exceeds its bounded source admission")}}
impl std::error::Error for Deferred {}
pub(super) struct Budget {pub bytes:usize,pub until:Instant}
impl Budget {
    pub fn charge(&mut self,n:usize)->Result<()> {if Instant::now()>=self.until||n>PLAN_BYTES.saturating_sub(self.bytes){return Err(Deferred.into())}self.bytes+=n;Ok(())}
}
#[derive(Clone)]
pub(super) struct Source {pub id:i64,pub revision:i64,pub group:String,pub collection:bool,pub identity:[u8;32]}
#[derive(Clone)]
pub(super) struct Head {pub named:Named,pub name:String,pub source:Source,pub complete:bool}
pub(super) struct Read<'a,'b> {pub tx:&'a mut Transaction<'b>,pub budget:&'a mut Budget,pub stop:&'a AtomicBool}
impl Read<'_,'_> {
    pub fn query(&mut self,sql:&str,args:&[&(dyn ToSql+Sync)])->Result<Vec<Row>> {
        if self.stop.load(Ordering::Acquire){return Err(crate::pg::outage::AttemptEnded.into())}
        self.budget.charge(0)?;
        let remaining=self.budget.until.saturating_duration_since(Instant::now()).as_millis().clamp(1,10000).to_string();
        self.tx.query_one("select set_config('statement_timeout',$1,true)",&[&remaining])?;
        match self.tx.query(sql,args) {
            Err(e) if e.code()==Some(&postgres::error::SqlState::QUERY_CANCELED)=>Err(Deferred.into()),
            result=>result.map_err(Into::into),
        }
    }
    pub fn head(&mut self,id:i64)->Result<Option<Head>> {
        let extent=self.query("select octet_length(original_name),octet_length(group_name),octet_length(poster),octet_length(identity_key) from releases where id=$1",&[&id])?;
        let Some(extent)=extent.first() else{return Ok(None)};
        let mut bytes=1024usize;for i in 0..3 {let n:Option<i32>=extent.try_get(i)?;let n=n.unwrap_or(0);if !(0..=TEXT as i32).contains(&n){return Err(Deferred.into())}bytes=bytes.checked_add(n as usize*6).context("native overview head extent overflow")?;}
        ensure!(extent.try_get::<_,i32>(3)?==32,"native overview identity extent is invalid");self.budget.charge(bytes)?;
        let rows=self.query("select id,original_name,group_name,poster,size,posted_clock,native_work_revision,is_collection,identity_key,complete from releases where id=$1",&[&id])?;
        let Some(r)=rows.first() else{return Ok(None)};let name=String::from_utf8(r.try_get(1)?)?;let group:String=r.try_get(2)?;
        let identity:Vec<u8>=r.try_get(8)?;let identity:[u8;32]=identity.try_into().map_err(|_|anyhow::anyhow!("native overview identity changed"))?;
        let bytes:i64=r.try_get(4)?;ensure!(bytes>=0,"native overview release size is malformed");
        Ok(Some(Head {name,named:Named {id,group:group.clone(),poster:r.try_get(3)?,bytes,date:r.try_get::<_,Option<i64>>(5)?.map(|t|t as f64/86400.0+2440587.5)},
            source:Source {id,revision:r.try_get(6)?,group,collection:r.try_get(7)?,identity},complete:r.try_get(9)?}))
    }
    /// Query only indexed exact original-name keys, never a catalog-wide text
    /// payload scan. Each of the two legacy name lookups retains its512 limit.
    pub fn named(&mut self,name:&str,poster:Option<&str>,same:bool)->Result<Vec<Named>> {
        use sha2::{Digest,Sha256};let key:[u8;32]=Sha256::digest(name.as_bytes()).into();
        let rows=self.query("select id from releases where original_name_key=$1 and original_name=$2 and (not $3::boolean or poster is not distinct from $4::text) order by group_name collate \"C\",id limit 512",&[&&key[..],&name.as_bytes(),&same,&poster])?;
        let mut out=Vec::new();for r in rows {if let Some(h)=self.head(r.try_get(0)?)? {if !crate::filters::group_blocked(&h.named.group){out.push(h.named);}}}Ok(out)
    }
    pub fn union(&mut self,ids:&[i64])->Result<UnionFiles> {
        if ids.len()>crate::release_info::LINKED_ROWS{return Err(Deferred.into())}self.budget.charge(ids.len()*128)?;
        let mut union=UnionFiles::default();for &id in ids {
            let Some(head)=self.head(id)? else{return Err(SourceChanged.into())};let mut filenames=BTreeSet::new();let mut after=0i64;
            loop {
                let rows=self.query("select id,octet_length(original_filename),octet_length(subject),expected,file_total,(select max(part) from file_parts p where p.file_id=f.id),(select min(part) from file_parts p where p.file_id=f.id) from files f where release_id=$1 and id>$2 order by id limit 16",&[&id,&after])?;
                if rows.is_empty(){break;}
                for row in rows {
                    let file:i64=row.try_get(0)?;after=file;let name:Option<i32>=row.try_get(1)?;let subject:Option<i32>=row.try_get(2)?;
                    let expected:Option<i64>=row.try_get(3)?;let total:Option<i64>=row.try_get(4)?;
                    ensure!(expected.is_none_or(|n|(0..=crate::parser::MAX_PARTS).contains(&n))&&total.is_none_or(|n|(0..i64::MAX).contains(&n)),"native overview file declaration is malformed");
                    if name.is_none_or(|n|!(0..=TEXT as i32).contains(&n))||subject.is_some_and(|n|!(0..=TEXT as i32).contains(&n)){return Err(Deferred.into())}
                    let last:Option<i64>=row.try_get(5)?;if last.is_some_and(|n|!(0..=crate::parser::MAX_PARTS).contains(&n))||row.try_get::<_,Option<i64>>(6)?.is_some_and(|n|n<0){anyhow::bail!("native overview part bitmap is malformed")}
                    let bitmap=last.map_or(0,|n|n as usize/8+1);
                    // Account the source strings, folded strings/name sets and
                    // possible overlap union growth before any of them exist.
                    self.budget.charge(1024+6*(name.unwrap_or(0) as usize+subject.unwrap_or(0) as usize)+bitmap*4)?;
                    let raw=self.query("select original_filename,subject from files where id=$1 and release_id=$2",&[&file,&id])?;
                    let raw=raw.first().context("native overview file disappeared inside snapshot")?;let filename=String::from_utf8(raw.try_get(0)?)?;let subject:Option<String>=raw.try_get(1)?;
                    let mut seen=Vec::with_capacity(bitmap);let mut part_after=-1i64;
                    loop {let parts=self.query("select part from file_parts where file_id=$1 and part>$2 order by part limit 4096",&[&file,&part_after])?;if parts.is_empty(){break;}
                        for p in parts {let part:i64=p.try_get(0)?;ensure!((0..=crate::parser::MAX_PARTS).contains(&part),"native overview part outside parser bound");crate::store::insert_part(&mut seen,part);part_after=part;}}
                    union.add_file(&mut filenames,filename,expected,seen,total,subject);
                }
            }union.end_row(Some(head.name),filenames);
        }Ok(union)
    }
    /// Same pure recovery verdict as legacy Contents, using actual accepted
    /// native part/byte facts. No absent byte cache is invented as a success.
    pub fn repairable(&mut self,id:i64,union:&UnionFiles)->Result<bool> {
        if !union.every_file_here(){return Ok(false)}
        let mut facts=crate::release_info::Contents::default();let mut after=0i64;
        loop {
            let rows=self.query("select id,case when octet_length(original_filename)<=65536 then original_filename end,octet_length(original_filename),distinct_parts,greatest(coalesce(expected,distinct_parts),distinct_parts),coalesce((select sum(s.bytes)::bigint from segments s where s.file_id=f.id and s.release_id=f.release_id),0) from files f where release_id=$1 and id>$2 order by id limit 16",&[&id,&after])?;
            if rows.is_empty(){break}
            for row in rows {
                after=row.try_get(0)?;let extent:Option<i32>=row.try_get(2)?;
                if extent.is_none_or(|n|!(0..=TEXT as i32).contains(&n)){return Err(Deferred.into())}
                self.budget.charge(4*extent.unwrap() as usize+256)?;
                let name=String::from_utf8(row.try_get(1)?)?.to_ascii_lowercase();
                let seen:i64=row.try_get(3)?;let expected:i64=row.try_get(4)?;let bytes:i64=row.try_get(5)?;
                ensure!(seen>=0&&expected>=seen&&bytes>=0,"native overview recovery counters malformed");
                let add=|to:&mut i64,n:i64|->Result<()> {*to=to.checked_add(n).context("native overview recovery counter overflow")?;Ok(())};
                if name.ends_with(".par2") {
                    add(&mut facts.recovery_blocks,crate::release_info::recovery_blocks(&name))?;
                    if name.contains(".vol") {add(&mut facts.recovery_articles_seen,seen)?;add(&mut facts.recovery_articles_expected,expected)?;add(&mut facts.recovery_bytes,bytes)?;}
                }else {add(&mut facts.data_articles_seen,seen)?;add(&mut facts.data_articles_expected,expected)?;add(&mut facts.data_bytes,bytes)?;}
            }
        }
        if facts.data_articles_seen>=facts.data_articles_expected||facts.recovery_articles_seen==0{return Ok(false)}
        if let Some(row)=self.query("select par2_block,par2_blocks from release_fingerprint where release_id=$1",&[&id])?.first() {
            facts.par2_block=row.try_get(0)?;facts.par2_blocks=row.try_get(1)?;
            ensure!(facts.par2_block.is_none_or(|n|n>0)&&facts.par2_blocks.is_none_or(|n|n>0)&&facts.recovery_blocks<=i64::MAX/1000,"native overview recovery block facts malformed");
        }
        Ok(facts.repairable())
    }
    pub fn windows(&mut self,ids:&[i64])->Result<Vec<Window>> {
        let mut groups=BTreeMap::<String,(i64,i64)>::new();for &id in ids {let Some(h)=self.head(id)? else{return Err(SourceChanged.into())};
            let rows=self.query("select min(first_date),max(last_date) from file_timing where release_id=$1",&[&id])?;let row=&rows[0];
            if let (Some(first),Some(last))=(row.try_get::<_,Option<i64>>(0)?,row.try_get::<_,Option<i64>>(1)?) {
                let e=groups.entry(h.named.group).or_insert((first,last));e.0=e.0.min(first);e.1=e.1.max(last);
            }}
        self.budget.charge(groups.len()*256)?;Ok(groups.into_iter().filter(|(g,_)|!g.is_empty()).map(|(group,(from,to))|Window {group,from:from.saturating_sub(MARGIN),to:to.saturating_add(MARGIN)}).collect())
    }
    pub fn source_matches(&mut self,s:&Source)->Result<bool> {
        Ok(self.query(SOURCE_MATCH,&[&s.id,&s.revision,&s.group,&s.collection,&&s.identity[..]])?
            .first().map(|r|r.try_get(0)).transpose()?.unwrap_or(false))
    }
    pub fn sources(&mut self,ids:&[i64])->Result<Vec<Source>> {ids.iter().map(|&id|self.head(id)?.map(|h|h.source).ok_or_else(||anyhow::Error::new(SourceChanged))).collect()}
}
const SOURCE_MATCH:&str="select native_work_revision=$2 and group_name=$3 and is_collection=$4 and identity_key=$5 from releases where id=$1";
pub(super) fn source_matches(tx:&mut impl GenericClient,s:&Source,lock:bool)->Result<bool> {
    let sql=if lock {"select native_work_revision=$2 and group_name=$3 and is_collection=$4 and identity_key=$5 from releases where id=$1 for update"}else{SOURCE_MATCH};
    Ok(tx.query_opt(sql,&[&s.id,&s.revision,&s.group,&s.collection,&&s.identity[..]])?.map(|r|r.try_get(0)).transpose()?.unwrap_or(false))
}
