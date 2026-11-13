//! Checked own-payload NZB streaming from the fresh native catalog.
//! Physical files and all same-part message copies stay distinct. No raw
//! fingerprint/public header can authorize donor lending in this reader.
use super::{fresh_reader::ReadView,fresh_files::{Text,TextField}};
use crate::{nzb_spool::{Pool,Spool},nzb_stream::{GapCheck,XmlWriter}};
use std::{io::{self,Read,Write},sync::Arc};
use anyhow::{Context,Result,ensure};
use postgres::fallible_iterator::FallibleIterator;

pub(crate) struct Download {pub body:Spool,pub disposition:Option<crate::nzb_http::Disposition>,pub gaps:u64}
#[derive(Clone,Copy)]
pub(crate) enum Pick<'a>{Whole,Ordinal(u64),Name(&'a str)}

struct Message<'a,'c> {view:&'a ReadView<'c>,release:i64,key:[u8;32],at:i64,end:i64,buffer:Vec<u8>,used:usize}
impl<'a,'c> Message<'a,'c> {
    fn chunk(view:&ReadView<'_>,release:i64,key:&[u8;32],at:i64,n:i32)->Result<Vec<u8>> {
        view.before_query()?;
        let row=view.tx.borrow_mut().query_opt("select substring(message_id from $3::integer for $4::integer)
            from segments where release_id=$1 and message_key=$2",&[&release,&&key[..],&i32::try_from(at+1)?,&n])?.context("native article disappeared")?;
        let bytes:Vec<u8>=row.try_get(0)?;ensure!(bytes.len()<=n as usize,"native article range exceeded admission");Ok(bytes)
    }
    fn new(view:&'a ReadView<'c>,release:i64,key:[u8;32],length:i64)->Result<Self> {
        ensure!(length>=0,"negative native article extent");let mut at=0;let mut end=length;
        while at<end {let bytes=Self::chunk(view,release,&key,at,8192.min(i32::try_from(end-at).unwrap_or(i32::MAX)))?;
            ensure!(!bytes.is_empty(),"native article range ended early");
            let skip=bytes.iter().take_while(|b|**b==b'<'||**b==b'>').count();at+=skip as i64;if skip<bytes.len(){break;}}
        while end>at {let start=(end-8192).max(at);let bytes=Self::chunk(view,release,&key,start,i32::try_from(end-start)?)?;
            ensure!(bytes.len() as i64==end-start,"native article tail ended early");
            let skip=bytes.iter().rev().take_while(|b|**b==b'<'||**b==b'>').count();end-=skip as i64;if skip<bytes.len(){break;}}
        Ok(Self {view,release,key,at,end,buffer:Vec::new(),used:0})
    }
}
impl Read for Message<'_,'_> {
    fn read(&mut self,out:&mut[u8])->io::Result<usize> {
        if out.is_empty(){return Ok(0);}
        if self.used==self.buffer.len() && self.at<self.end {
            let n=8192.min(i32::try_from(self.end-self.at).unwrap_or(i32::MAX));
            self.buffer=Self::chunk(self.view,self.release,&self.key,self.at,n).map_err(io::Error::other)?;
            if self.buffer.len()!=n as usize{return Err(io::Error::other("native article range ended early"));}
            self.at+=i64::from(n);self.used=0;
        }
        let n=out.len().min(self.buffer.len()-self.used);out[..n].copy_from_slice(&self.buffer[self.used..self.used+n]);self.used+=n;Ok(n)
    }
}
fn number(input:&mut impl Read)->Result<i64>{let mut bytes=[0;8];input.read_exact(&mut bytes)?;Ok(i64::from_le_bytes(bytes))}
fn put(output:&mut impl Write,n:i64)->Result<()>{output.write_all(&n.to_le_bytes())?;Ok(())}
fn segment(input:&mut impl Read)->Result<([u8;32],Option<i64>,i64,i64)>{
    let mut key=[0;32];input.read_exact(&mut key)?;let tag=number(input)?;let part=number(input)?;
    ensure!(tag==0||tag==1,"invalid native segment optional tag");Ok((key,(tag==1).then_some(part),number(input)?,number(input)?))
}
impl ReadView<'_> {
    pub(crate) fn nzb_own(&self,id:i64,pick:Pick<'_>,pool:&Arc<Pool>)->Result<Option<Download>> {
        let Some((group,posted,complete))=self.download_head(id)? else{return Ok(None)};
        let mut files=pool.open()?;self.before_query()?;
        let mut tx=self.tx.borrow_mut();
        let mut rows=tx.query_raw("select f.id,f.expected,octet_length(f.filename)::bigint,count(s.message_key)
            from files f join segments s on s.file_id=f.id and s.release_id=f.release_id
            where f.release_id=$1 and ($2::text is null or f.filename=$2)
            group by f.id order by f.filename collate \"C\",f.id",&[&id as &(dyn postgres::types::ToSql+Sync),&match pick{Pick::Name(name)=>Some(name),_=>None}])?;
        let mut ordinal=0u64;let mut selected=0u64;
        while let Some(row)=rows.next()? {
            self.check_deadline()?;ordinal=ordinal.checked_add(1).context("native file ordinal overflow")?;
            if matches!(pick,Pick::Ordinal(wanted) if ordinal!=wanted){continue;}
            let file:i64=row.try_get(0)?;let expected:Option<i64>=row.try_get(1)?;let length:i64=row.try_get(2)?;let count:i64=row.try_get(3)?;
            ensure!(file>0&&length>=0&&count>0,"invalid native file descriptor");
            for value in [file,i64::from(expected.is_some()),expected.unwrap_or(0),length,count]{put(&mut files,value)?;}
            selected=selected.checked_add(1).context("native selected file count overflow")?;
        }
        drop(rows);drop(tx);
        if selected==0 && !matches!(pick,Pick::Whole){return Ok(None);}
        let mut files=files.finish()?;let mut plan=pool.open()?;
        for _ in 0..selected {
            let file=number(&mut files)?;let tag=number(&mut files)?;let total=number(&mut files)?;let length=number(&mut files)?;let count=number(&mut files)?;
            for value in [file,tag,total,length,count]{put(&mut plan,value)?;}
            self.before_query()?;let mut tx=self.tx.borrow_mut();
            let mut rows=tx.query_raw("select substring(message_key from 1 for 33),octet_length(message_key)::bigint,
                part,bytes,octet_length(message_id)::bigint from segments where release_id=$1 and file_id=$2
                order by part nulls first,message_id",&[&id,&file])?;
            let mut seen=0i64;
            while let Some(row)=rows.next()? {
                self.check_deadline()?;let key:Vec<u8>=row.try_get(0)?;
                ensure!(row.try_get::<_,i64>(1)?==32&&key.len()==32,"invalid native message key extent");
                plan.write_all(&key)?;let part:Option<i64>=row.try_get(2)?;
                for value in [i64::from(part.is_some()),part.unwrap_or(0),row.try_get(3)?,row.try_get(4)?]{put(&mut plan,value)?;}
                seen=seen.checked_add(1).context("native article count overflow")?;
            }
            ensure!(seen==count,"native article inventory changed within snapshot");
        }
        drop(files);let mut plan=plan.finish()?;
        let mut xml=XmlWriter::new(pool.open()?,group.as_bytes())?;let mut gaps=0u64;
        for _ in 0..selected {
            let file=number(&mut plan)?;let tag=number(&mut plan)?;let total=number(&mut plan)?;let _length=number(&mut plan)?;let count=number(&mut plan)?;
            let mut check=GapCheck::new((tag==1).then_some(total));for _ in 0..count{let (_,part,_,_)=segment(&mut plan)?;check.push(part)?;}
            if let Some(gap)=check.finish(){
                gaps=gaps.checked_add(1).context("native gap count overflow")?;
                let more=if(gap.total.unwrap_or(0)-gap.have) as usize>gap.missing.len(){",..."}else{""};
                let missing=if gap.missing.is_empty(){"-".into()}else{gap.missing.iter().map(ToString::to_string).collect::<Vec<_>>().join(",")};
                let suffix=format!(": {} of {} parts, missing {missing}{more}{}",gap.have,gap.total.map_or_else(||"?".into(),|n|n.to_string()),
                    if gap.stray>0{format!(", {} unnumbered or out of range",gap.stray)}else{String::new()});
                xml.meta(false,Text::new(self,TextField::FileName(file)).chain(suffix.as_bytes()))?;
            }
        }
        // Whole-post file-counter gaps/inference require their own bounded
        // native derivation; do not call a legacy loader or label unproved
        // incomplete whole payload as a checked complete download meanwhile.
        ensure!(!matches!(pick,Pick::Whole)||complete,"native incomplete whole-post derivation is not yet connected");
        xml.end_head()?;plan.rewind()?;
        for _ in 0..selected {
            let file=number(&mut plan)?;let _tag=number(&mut plan)?;let _total=number(&mut plan)?;let _length=number(&mut plan)?;let count=number(&mut plan)?;
            xml.file(Text::new(self,TextField::ReleasePoster(id)),crate::dates::article_timestamp(&posted),Text::new(self,TextField::FileSubject(file)))?;
            xml.group(group.as_bytes())?;xml.segments()?;
            for _ in 0..count{let(key,part,bytes,length)=segment(&mut plan)?;xml.segment(bytes,part,Message::new(self,id,key,length)?)?;}
            xml.end_file()?;
        }
        ensure!(plan.read(&mut[0u8;1])?==0,"native NZB inventory has trailing rows");drop(plan);
        let body=xml.finish()?.finish()?;
        let disposition=crate::nzb_http::Disposition::prepare(pool,Text::new(self,TextField::ReleaseName(id)),id)?;
        Ok(Some(Download {body,disposition,gaps}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parser::{Article,Release},pg::fresh_reader::Budget};
    use std::time::{Instant,Duration};
    #[test]
    #[ignore="requires explicit isolated fresh PostgreSQL and spool admission"]
    fn real_native_writer_nzb_matches_own_payload_oracle_and_file_selection() {
        let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG fixture");
        let mut client=schema.pool.get().unwrap();
        let articles=(0..19).map(|n|Article {filename:Some("sample.字幕.\"&.bin".into()),subject:"sample.字幕.bin yEnc (1/1)".into(),
            message_id:format!("<<part{n}@example.test>>"),part:Some(1),total_parts:Some(1),bytes:17,..Article::default()}).collect();
        let release=Release {name:"Native.Example.Release".into(),group:"alt.binaries.test".into(),poster:"poster&test".into(),
            date:"2020-01-01 00:00:00".into(),articles,..Release::default()};
        crate::pg::release_writer::save(&mut client,&[release],1).unwrap();
        let id:i64=client.query_one("select id from releases",&[]).unwrap().get(0);
        let (legacy,articles)=crate::pg::release_reader::inventory(&mut client,id).unwrap().unwrap().into_nzb_rows();
        let expected=crate::nzb::render_checked(&legacy,&articles,std::slice::from_ref(&legacy.group_name),true);
        assert!(expected.complete());
        let root=tempfile::tempdir().unwrap();
        #[cfg(unix)]{use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(root.path(),std::fs::Permissions::from_mode(0o700)).unwrap();}
        let pool=Pool::new(root.path().into(),crate::nzb_spool::Limits {requests:5,per_request_bytes:1<<20,total_bytes:5<<20,free_bytes:0}).unwrap();
        let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
        for pick in [Pick::Whole,Pick::Ordinal(1),Pick::Name("sample.字幕.\"&.bin")] {
            let mut download=view.nzb_own(id,pick,&pool).unwrap().unwrap();let mut body=String::new();download.body.read_to_string(&mut body).unwrap();
            assert_eq!(body,expected.xml);assert_eq!(download.gaps,0);
            assert_eq!(body.matches("<segment ").count(),19);
            assert!(body.contains(">part0@example.test</segment>"));
        }
        assert!(view.nzb_own(id,Pick::Ordinal(2),&pool).unwrap().is_none());
        assert!(view.nzb_own(id,Pick::Name("missing.bin"),&pool).unwrap().is_none());
        assert!(view.nzb_own(id+1,Pick::Whole,&pool).unwrap().is_none());
    }
}
