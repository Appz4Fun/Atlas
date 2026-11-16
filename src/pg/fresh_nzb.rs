//! Checked own-payload NZB streaming from the fresh native catalog.
//! Physical files and all same-part message copies stay distinct. No raw
//! fingerprint/public header can authorize donor lending in this reader.
use super::{fresh_reader::ReadView,fresh_files::{Text,TextField}};
use crate::{nzb_spool::{Pool,Spool},nzb_stream::{GapCheck,XmlWriter}};
use std::{io::{self,Read,Write},sync::Arc};
use anyhow::{Context,Result,ensure};
use postgres::fallible_iterator::FallibleIterator;

pub(crate) struct Download {pub body:Spool,pub disposition:Option<crate::nzb_http::Disposition>,pub gaps:u64,pub details:Option<Vec<crate::nzb::FileGap>>}
#[derive(Clone,Copy)]
pub(crate) enum Pick<'a>{Whole,Ordinal(u64),Name(&'a str)}

const INLINE_MESSAGE_BYTES:usize=8192;

pub(super) struct Message<'a,'c> {view:&'a ReadView<'c>,release:i64,key:[u8;32],at:i64,end:i64,buffer:Vec<u8>,used:usize}
impl<'a,'c> Message<'a,'c> {
    fn chunk(view:&ReadView<'_>,release:i64,key:&[u8;32],at:i64,n:i32)->Result<Vec<u8>> {
        // The local store has no key to look an article up by: there a message carries its id (`held`).
        ensure!(view.local.is_none(),"a native article is read by key only from PostgreSQL");
        view.before_query()?;
        let row=view.tx.borrow_mut().query_opt("select substring(message_id from $3::integer for $4::integer)
            from segments where release_id=$1 and message_key=$2",&[&release,&&key[..],&i32::try_from(at+1)?,&n])?.context("native article disappeared")?;
        let bytes:Vec<u8>=row.try_get(0)?;ensure!(bytes.len()<=n as usize,"native article range exceeded admission");Ok(bytes)
    }
    pub(super) fn raw(view:&'a ReadView<'c>,release:i64,key:[u8;32],length:i64)->Result<Self> {
        ensure!(length>=0,"negative native article extent");
        Ok(Self {view,release,key,at:0,end:length,buffer:Vec::new(),used:0})
    }
    /// A message that carries its whole id, as one read from the local article store does.
    pub(super) fn held(view:&'a ReadView<'c>,release:i64,id:Vec<u8>)->Self {
        Self {view,release,key:[0;32],at:0,end:0,buffer:id,used:0}
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
    ensure!(tag==0||tag==1,"invalid native segment optional tag");let bytes=number(input)?;let length=number(input)?;
    ensure!(length>=0,"negative native article extent");Ok((key,(tag==1).then_some(part),bytes,length))
}
fn inline_message<'a>(input:&mut impl Read,length:i64,buffer:&'a mut [u8;INLINE_MESSAGE_BYTES])->Result<Option<&'a [u8]>> {
    ensure!(length>=0,"negative native article extent");
    if length>INLINE_MESSAGE_BYTES as i64 {return Ok(None);}
    let bytes=&mut buffer[..length as usize];input.read_exact(bytes)?;Ok(Some(bytes))
}
fn trimmed_message(bytes:&[u8])->&[u8] {
    let start=bytes.iter().position(|b|*b!=b'<'&&*b!=b'>').unwrap_or(bytes.len());
    let end=bytes.iter().rposition(|b|*b!=b'<'&&*b!=b'>').map_or(start,|at|at+1);
    &bytes[start..end]
}
/// One article of a local plan: the PostgreSQL record, with the id always behind it whatever its length.
fn put_local(out:&mut impl Write,segment:&crate::article_local::Segment)->Result<()> {
    out.write_all(&crate::article_local::message_key(&segment.message_id))?;
    for value in [i64::from(segment.part.is_some()),segment.part.unwrap_or(0),segment.bytes,i64::try_from(segment.message_id.len())?]{put(out,value)?;}
    out.write_all(&segment.message_id)?;Ok(())
}
fn held_message(input:&mut impl Read,length:i64)->Result<Vec<u8>> {
    let mut bytes=vec![0;usize::try_from(length)?];input.read_exact(&mut bytes)?;Ok(bytes)
}
fn trim_held(mut bytes:Vec<u8>)->Vec<u8> {
    let kept=trimmed_message(&bytes);let (start,length)=(kept.as_ptr() as usize-bytes.as_ptr() as usize,kept.len());
    bytes.truncate(start+length);bytes.drain(..start);bytes
}
const LOCAL_PAGE_ROWS:usize=4096;
const LOCAL_PAGE_BYTES:usize=1<<20;
#[derive(Clone,Copy)]
struct SparseHeader {key:[u8;32],subject_bytes:i64,poster_bytes:i64,timestamp:i64}
struct FileDescriptor {file:i64,expected:Option<i64>,name_bytes:i64,count:i64,sparse:Option<SparseHeader>}
impl FileDescriptor {
    fn read(input:&mut impl Read)->Result<Self> {
        let file=number(input)?;let tag=number(input)?;let expected=number(input)?;let name_bytes=number(input)?;let count=number(input)?;
        let sparse_tag=number(input)?;let mut key=[0;32];input.read_exact(&mut key)?;
        let subject_bytes=number(input)?;let poster_bytes=number(input)?;let timestamp=number(input)?;
        ensure!(file>0&&count>0&&name_bytes>=0&&(tag==0||tag==1)&&(sparse_tag==0||sparse_tag==1),"invalid native file descriptor");
        let sparse=if sparse_tag==1 {
            ensure!(name_bytes==0&&count==1&&subject_bytes>=0&&poster_bytes>=0,"invalid native sparse header extent");
            Some(SparseHeader {key,subject_bytes,poster_bytes,timestamp})
        }else{ensure!(key==[0;32]&&subject_bytes==0&&poster_bytes==0&&timestamp==0,"unexpected native sparse header");None};
        Ok(Self {file,expected:(tag==1).then_some(expected),name_bytes,count,sparse})
    }
    fn write(&self,out:&mut impl Write)->Result<()> {
        for value in [self.file,i64::from(self.expected.is_some()),self.expected.unwrap_or(0),self.name_bytes,self.count,i64::from(self.sparse.is_some())]{put(out,value)?;}
        out.write_all(&self.sparse.map_or([0;32],|header|header.key))?;
        let (subject,poster,timestamp)=self.sparse.map_or((0,0,0),|header|(header.subject_bytes,header.poster_bytes,header.timestamp));
        for value in [subject,poster,timestamp]{put(out,value)?;}Ok(())
    }
}
#[derive(Clone,Copy)]
enum SparseField {Subject,Poster}
struct SparseText<'view,'client> {view:&'view ReadView<'client>,release:i64,key:[u8;32],field:SparseField,at:i64,end:i64,buffer:Vec<u8>,used:usize,
    /// The article's message id when its envelope is in the local store, which has no key to find it by.
    local:Option<&'view [u8]>}
impl<'view,'client> SparseText<'view,'client> {
    fn new(view:&'view ReadView<'client>,release:i64,header:SparseHeader,field:SparseField,local:Option<&'view [u8]>)->Self {
        let end=match field {SparseField::Subject=>header.subject_bytes,SparseField::Poster=>header.poster_bytes};
        Self {view,release,key:header.key,field,at:0,end,buffer:Vec::new(),used:0,local}
    }
}
impl Read for SparseText<'_,'_> {
    fn read(&mut self,out:&mut[u8])->io::Result<usize> {
        if out.is_empty(){return Ok(0);}
        if self.used==self.buffer.len()&&self.at<self.end {
            let fill=||->Result<Vec<u8>> {
                if let Some(message)=self.local {
                    self.view.check_deadline()?;let count=(self.end-self.at).min(8192);
                    let store=self.view.local.as_ref().context("native sparse header has no local article store")?;
                    let bytes=store.nzb_envelope_range(self.release,message,matches!(self.field,SparseField::Subject),self.at,count)?.context("native sparse header disappeared")?;
                    ensure!(bytes.len() as i64==count,"native sparse header range changed");return Ok(bytes);
                }
                self.view.before_query()?;
                let query=match self.field {
                    SparseField::Subject=>"select substring(subject from $3::integer for $4::integer) from segment_nzb_metadata where release_id=$1 and message_key=$2",
                    SparseField::Poster=>"select substring(poster from $3::integer for $4::integer) from segment_nzb_metadata where release_id=$1 and message_key=$2",
                };
                let count=i32::try_from((self.end-self.at).min(8192))?;let start=i32::try_from(self.at+1)?;
                let row=self.view.tx.borrow_mut().query_opt(query,&[&self.release,&&self.key[..],&start,&count])?.context("native sparse header disappeared")?;
                let bytes:Vec<u8>=row.try_get(0)?;ensure!(bytes.len()==count as usize,"native sparse header range changed");Ok(bytes)
            };
            self.buffer=fill().map_err(io::Error::other)?;self.at+=self.buffer.len() as i64;self.used=0;
        }
        let count=out.len().min(self.buffer.len()-self.used);out[..count].copy_from_slice(&self.buffer[self.used..self.used+count]);self.used+=count;Ok(count)
    }
}
impl ReadView<'_> {
    pub(crate) fn nzb_own(&self,id:i64,pick:Pick<'_>,pool:&Arc<Pool>)->Result<Option<Download>> {
        self.nzb_own_mode(id,pick,pool,false)
    }
    /// Preserve checked file gaps when an existing caller needs owned XML.
    /// Streaming downloads retain their original bounded transport path.
    pub(crate) fn nzb_owned_build(&self,id:i64,pick:Pick<'_>,pool:&Arc<Pool>)->Result<Option<crate::nzb::NzbBuild>> {
        let Some(mut download)=self.nzb_own_mode(id,pick,pool,true)? else{return Ok(None)};
        self.admit_owned(download.body.len().checked_mul(2).context("native owned NZB extent overflow")?)?;
        let mut xml=String::with_capacity(usize::try_from(download.body.len())?);download.body.read_to_string(&mut xml)?;
        let gaps=download.details.take().context("native owned gap report missing")?;
        Ok(Some(crate::nzb::NzbBuild {xml,gaps}))
    }
    fn nzb_own_mode(&self,id:i64,pick:Pick<'_>,pool:&Arc<Pool>,collect_gaps:bool)->Result<Option<Download>> {
        let Some((group,posted,complete))=self.download_head(id)? else{return Ok(None)};
        let mut files=pool.open()?;self.before_query()?;
        let local=self.local.clone();
        let (selected,mut plan)=if let Some(store)=&local {
            let selected=self.nzb_local_plan(store,id,pick,&mut files)?;
            if selected==0 && (collect_gaps||!matches!(pick,Pick::Whole)){return Ok(None);}
            (selected,files.finish()?)
        }else{
        let mut tx=self.tx.borrow_mut();
        let mut rows=tx.query_raw("with inventory as(
            select f.id,f.expected,f.filename,count(s.message_key) articles,null::bytea singleton_key,null::bigint singleton_part,null::bytea singleton_mid,
                null::bigint subject_bytes,null::bigint poster_bytes,null::bigint original_timestamp,false sparse_present
            from files f join segments s on s.file_id=f.id and s.release_id=f.release_id
            where f.release_id=$1 and f.filename<>'' group by f.id
            union all
            select f.id,m.expected,f.filename,1::bigint,s.message_key,s.part,s.message_id,
                octet_length(m.subject)::bigint,octet_length(m.poster)::bigint,m.nzb_timestamp,m.message_key is not null
            from files f join segments s on s.file_id=f.id and s.release_id=f.release_id
            left join segment_nzb_metadata m on m.release_id=s.release_id and m.message_key=s.message_key
            where f.release_id=$1 and f.filename='')
            select id,expected,octet_length(filename)::bigint,articles,substring(singleton_key from 1 for 33),octet_length(singleton_key)::bigint,
                subject_bytes,poster_bytes,original_timestamp,sparse_present from inventory
            where($2::text is null or(filename<>'' and filename=$2))
            order by filename collate \"C\",id,singleton_part nulls first,singleton_mid",[&id as &(dyn postgres::types::ToSql+Sync),&match pick{Pick::Name(name)=>Some(name),_=>None}])?;
        let mut ordinal=0u64;let mut selected=0u64;
        while let Some(row)=rows.next()? {
            self.check_deadline()?;ordinal=ordinal.checked_add(1).context("native file ordinal overflow")?;
            if matches!(pick,Pick::Ordinal(wanted) if ordinal!=wanted){continue;}
            let file:i64=row.try_get(0)?;let expected:Option<i64>=row.try_get(1)?;let length:i64=row.try_get(2)?;let count:i64=row.try_get(3)?;
            ensure!(file>0&&length>=0&&count>0,"invalid native file descriptor");
            let key:Option<Vec<u8>>=row.try_get(4)?;let key_length:Option<i64>=row.try_get(5)?;
            let sparse=if let Some(key)=key {
                ensure!(key.len()==32&&key_length==Some(32)&&row.try_get::<_,bool>(9)?,"native original nameless metadata unavailable");
                let subject_bytes=row.try_get::<_,Option<i64>>(6)?.context("native original subject extent missing")?;
                let poster_bytes=row.try_get::<_,Option<i64>>(7)?.context("native original poster extent missing")?;
                let timestamp=row.try_get::<_,Option<i64>>(8)?.context("native original timestamp missing")?;
                ensure!(subject_bytes>=0&&poster_bytes>=0,"negative original nameless header extent");
                Some(SparseHeader {key:key.try_into().map_err(|_|anyhow::anyhow!("native sparse key extent changed"))?,subject_bytes,poster_bytes,timestamp})
            }else{ensure!(key_length.is_none()&&!row.try_get::<_,bool>(9)?,"native named identity changed");None};
            FileDescriptor {file,expected,name_bytes:length,count,sparse}.write(&mut files)?;
            selected=selected.checked_add(1).context("native selected file count overflow")?;
        }
        drop(rows);drop(tx);
        if selected==0 && (collect_gaps||!matches!(pick,Pick::Whole)){return Ok(None);}
        let mut files=files.finish()?;let mut plan=pool.open()?;
        for _ in 0..selected {
            let descriptor=FileDescriptor::read(&mut files)?;let file=descriptor.file;let count=descriptor.count;
            let singleton=descriptor.sparse.as_ref().map(|header|&header.key[..]);descriptor.write(&mut plan)?;
            self.before_query()?;let mut tx=self.tx.borrow_mut();
            let params:[&(dyn postgres::types::ToSql+Sync);3]=[&id,&file,&singleton];
            let mut rows=tx.query_raw("select substring(message_key from 1 for 33),octet_length(message_key)::bigint,
                part,bytes,octet_length(message_id)::bigint,
                case when octet_length(message_id)<=8192 then message_id else null::bytea end
                from segments where release_id=$1 and file_id=$2
                and($3::bytea is null or message_key=$3) order by part nulls first,message_id",params)?;
            let mut seen=0i64;
            while let Some(row)=rows.next()? {
                self.check_deadline()?;let key:Vec<u8>=row.try_get(0)?;
                ensure!(row.try_get::<_,i64>(1)?==32&&key.len()==32,"invalid native message key extent");
                plan.write_all(&key)?;let part:Option<i64>=row.try_get(2)?;let length:i64=row.try_get(4)?;
                ensure!(length>=0,"negative native article extent");
                let inline:Option<&[u8]>=row.try_get(5)?;
                ensure!(inline.is_some()==(length<=INLINE_MESSAGE_BYTES as i64)
                    && inline.is_none_or(|bytes|bytes.len() as i64==length),"native inline article extent changed");
                for value in [i64::from(part.is_some()),part.unwrap_or(0),row.try_get(3)?,length]{put(&mut plan,value)?;}
                // Only a bounded row payload is copied to disk. Large IDs keep
                // their range-reader identity; no file's IDs accumulate in RAM.
                if let Some(inline)=inline {plan.write_all(inline)?;}
                seen=seen.checked_add(1).context("native article count overflow")?;
            }
            ensure!(seen==count,"native article inventory changed within snapshot");
        }
        drop(files);let plan=plan.finish()?;
        (selected,plan)
        };
        let mut xml=XmlWriter::new(pool.open()?,group.as_bytes())?;let mut gaps=0u64;let mut details=collect_gaps.then(Vec::new);
        let mut message_buffer=[0;INLINE_MESSAGE_BYTES];
        for _ in 0..selected {
            let descriptor=FileDescriptor::read(&mut plan)?;let file=descriptor.file;let count=descriptor.count;
            let mut check=GapCheck::new(descriptor.expected);for _ in 0..count{
                self.check_deadline()?;let (_,part,_,length)=segment(&mut plan)?;
                if inline_message(&mut plan,length,&mut message_buffer)?.is_none()&&local.is_some() {
                    // A local plan holds every id, the long ones too: step over it.
                    ensure!(io::copy(&mut (&mut plan).take(length as u64),&mut io::sink())?==length as u64,"native local plan ended early");
                }
                check.push(part)?;
            }
            if let Some(gap)=check.finish(){
                gaps=gaps.checked_add(1).context("native gap count overflow")?;
                if let Some(details)=&mut details {
                    self.admit_owned((descriptor.name_bytes as u64).checked_mul(3).and_then(|n|n.checked_add(1024)).context("native gap description extent overflow")?)?;
                    let filename=if descriptor.name_bytes==0 {None}else{
                        let mut name=String::new();Text::new(self,TextField::FileName(file)).read_to_string(&mut name)?;
                        ensure!(name.len() as i64==descriptor.name_bytes,"native gap filename extent changed");Some(name)
                    };
                    details.push(crate::nzb::FileGap {files:false,filename,total:gap.total,have:gap.have,missing:gap.missing.clone(),stray:gap.stray});
                }
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
            let descriptor=FileDescriptor::read(&mut plan)?;let count=descriptor.count;
            // A nameless article's envelope is found by message id in the local store,
            // so its one article is read before the file's opening and written after it.
            let mut envelope=None;
            if let Some(header)=descriptor.sparse {
                if local.is_some() {
                    let(_,part,bytes,length)=segment(&mut plan)?;let message=held_message(&mut plan,length)?;
                    xml.file(SparseText::new(self,id,header,SparseField::Poster,Some(&message)),header.timestamp,SparseText::new(self,id,header,SparseField::Subject,Some(&message)))?;
                    envelope=Some((part,bytes,message));
                }else{
                xml.file(SparseText::new(self,id,header,SparseField::Poster,None),header.timestamp,SparseText::new(self,id,header,SparseField::Subject,None))?;
                }
            }else{
                xml.file(Text::new(self,TextField::ReleasePoster(id)),crate::dates::article_timestamp(&posted),Text::new(self,TextField::FileSubject(descriptor.file)))?;
            }
            xml.group(group.as_bytes())?;xml.segments()?;
            if let Some((part,bytes,message))=envelope {
                self.check_deadline()?;xml.segment(bytes,part,Message::held(self,id,trim_held(message)))?;
            }else{
            for _ in 0..count{
                self.check_deadline()?;let(key,part,bytes,length)=segment(&mut plan)?;
                match inline_message(&mut plan,length,&mut message_buffer)? {
                    Some(message)=>xml.segment(bytes,part,trimmed_message(message))?,
                    None if local.is_some()=>xml.segment(bytes,part,Message::held(self,id,trim_held(held_message(&mut plan,length)?)))?,
                    None=>xml.segment(bytes,part,Message::new(self,id,key,length)?)?,
                }
            }
            }
            xml.end_file()?;
        }
        ensure!(plan.read(&mut[0u8;1])?==0,"native NZB inventory has trailing rows");drop(plan);
        let body=xml.finish()?.finish()?;
        let disposition=crate::nzb_http::Disposition::prepare(pool,Text::new(self,TextField::ReleaseName(id)),id)?;
        Ok(Some(Download {body,disposition,gaps,details}))
    }
    /// A file's articles from the local store in NZB order (unnumbered parts first, then part, then
    /// message id), a bounded page at a time and no more than `limit` of them. Returns how many it gave.
    pub(super) fn local_segments(&self,store:&crate::article_local::Store,release:i64,file:i64,limit:i64,
        mut each:impl FnMut(crate::article_local::Segment)->Result<()>)->Result<i64> {
        let mut after:Option<(Option<i64>,Vec<u8>)>=None;let mut seen=0i64;
        loop {
            self.check_deadline()?;
            let page=store.nzb_page(release,file,after.as_ref().map(|(part,id)|(*part,id.as_slice())),LOCAL_PAGE_ROWS,LOCAL_PAGE_BYTES)?;
            let Some(last)=page.last() else{return Ok(seen)};after=Some((last.part,last.message_id.clone()));
            for segment in page {
                if seen==limit{return Ok(seen);}
                self.check_deadline()?;each(segment)?;seen=seen.checked_add(1).context("native article count overflow")?;
            }
        }
    }
    /// The inventory and the per-file article reads of the PostgreSQL path in one pass: the files come
    /// from this snapshot, their articles from the local store. Writes the plan; returns the files chosen.
    fn nzb_local_plan(&self,store:&crate::article_local::Store,id:i64,pick:Pick<'_>,plan:&mut impl Write)->Result<u64> {
        let mut tx=self.tx.borrow_mut();
        let mut rows=tx.query_raw("select id,expected,octet_length(filename)::bigint from files
            where release_id=$1 and($2::text is null or(filename<>'' and filename=$2))
            order by filename collate \"C\",id",[&id as &(dyn postgres::types::ToSql+Sync),&match pick{Pick::Name(name)=>Some(name),_=>None}])?;
        let mut ordinal=0u64;let mut selected=0u64;
        while let Some(row)=rows.next()? {
            self.check_deadline()?;
            let file:i64=row.try_get(0)?;let expected:Option<i64>=row.try_get(1)?;let length:i64=row.try_get(2)?;
            if length!=0 {
                // A named file is one inventory row, and none at all without an article.
                let count=store.nzb_file_articles(id,file)?;if count==0{continue;}
                ordinal=ordinal.checked_add(1).context("native file ordinal overflow")?;
                if matches!(pick,Pick::Ordinal(wanted) if ordinal!=wanted){continue;}
                ensure!(file>0&&length>=0&&count>0,"invalid native file descriptor");
                FileDescriptor {file,expected,name_bytes:length,count,sparse:None}.write(plan)?;
                // Articles the indexer adds after the count are left for the next request.
                let seen=self.local_segments(store,id,file,count,|segment|put_local(plan,&segment))?;
                ensure!(seen==count,"native article inventory changed within snapshot");
            }else{
                // Every article of a nameless file is its own inventory row, and its envelope the file's header.
                let mut chosen=0u64;
                self.local_segments(store,id,file,i64::MAX,|segment| {
                    ordinal=ordinal.checked_add(1).context("native file ordinal overflow")?;
                    if matches!(pick,Pick::Ordinal(wanted) if ordinal!=wanted){return Ok(());}
                    ensure!(file>0,"invalid native file descriptor");
                    let extent=store.nzb_envelope_extent(id,&segment.message_id)?.context("native original nameless metadata unavailable")?;
                    ensure!(extent.subject_bytes>=0&&extent.poster_bytes>=0,"negative original nameless header extent");
                    let header=SparseHeader {key:crate::article_local::message_key(&segment.message_id),subject_bytes:extent.subject_bytes,poster_bytes:extent.poster_bytes,timestamp:extent.nzb_timestamp};
                    FileDescriptor {file,expected:extent.expected,name_bytes:0,count:1,sparse:Some(header)}.write(plan)?;
                    put_local(plan,&segment)?;chosen=chosen.checked_add(1).context("native selected file count overflow")?;Ok(())
                })?;
                selected=selected.checked_add(chosen).context("native selected file count overflow")?;continue;
            }
            selected=selected.checked_add(1).context("native selected file count overflow")?;
        }
        Ok(selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parser::{Article,Release},pg::fresh_reader::Budget};
    use std::time::{Instant,Duration};
    #[test]
    fn inline_message_plan_preserves_bounds_and_angle_trimming() {
        let mut buffer=[0;INLINE_MESSAGE_BYTES];
        assert_eq!(trimmed_message(b"<<a<b>&>>"),b"a<b>&");
        assert!(trimmed_message(b"<><>").is_empty());
        assert!(trimmed_message(b"").is_empty());
        let mut input=b"raw".as_slice();
        assert_eq!(inline_message(&mut input,3,&mut buffer).unwrap().unwrap(),b"raw");
        assert!(input.is_empty());
        assert!(inline_message(&mut b"raw".as_slice(),4,&mut buffer).is_err());
        assert!(inline_message(&mut b"".as_slice(),-1,&mut buffer).is_err());
        let mut input=b"untouched".as_slice();
        assert!(inline_message(&mut input,8193,&mut buffer).unwrap().is_none());
        assert_eq!(input,b"untouched");
    }

    #[test]
    #[ignore="requires explicit isolated PostgreSQL and bounded NZB spool admission"]
    fn inline_and_large_message_ids_preserve_raw_xml_and_invalid_utf8_refusal() {
        use crate::search::{ArticleRow,ReleaseRow};
        let schema=crate::pg::testing::Schema::native_writer().expect("explicit isolated PG fixture");
        let mut client=schema.pool.get().unwrap();
        let ids=["<字幕&\"@example.invalid>".to_string(),"<".repeat(8192),">".repeat(8193),
            format!("<{}é&>","a".repeat(8187)),format!("<{}é&>","a".repeat(8188)),format!("<{}>","漢".repeat(4096))];
        assert_eq!(ids[3].len(),8192);assert_eq!(ids[4].len(),8193);
        let articles=ids.iter().enumerate().map(|(n,id)|Article {filename:Some(format!("case{n}.bin")),subject:format!("case{n}.bin yEnc (1/1)"),
            message_id:id.clone(),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}).collect::<Vec<_>>();
        let release=Release {name:"Message.Id.Boundaries".into(),group:"alt.binaries.test".into(),poster:"poster&test".into(),
            date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),articles,..Default::default()};
        crate::pg::release_writer::save(&mut client,std::slice::from_ref(&release),1).unwrap();
        let id:i64=client.query_one("select id from releases",&[]).unwrap().get(0);
        let row=ReleaseRow {id,name:release.name.clone(),group_name:release.group.clone(),complete:true,..Default::default()};
        let oracle=release.articles.iter().map(|a|ArticleRow {message_id:a.message_id.clone(),filename:a.filename.clone(),part:a.part,
            total_parts:a.total_parts,bytes:Some(a.bytes),subject:Some(a.subject.clone()),poster:Some(release.poster.clone()),posted_date:Some(release.date.clone())}).collect::<Vec<_>>();
        let root=tempfile::tempdir().unwrap();
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(root.path(),std::fs::Permissions::from_mode(0o700)).unwrap();}
        let pool=Pool::new(root.path().into(),crate::nzb_spool::Limits {requests:6,per_request_bytes:1<<20,total_bytes:6<<20,free_bytes:0}).unwrap();
        for pick in [Pick::Whole,Pick::Ordinal(4),Pick::Name("case4.bin")] {
            let selected=match pick {Pick::Whole=>oracle.clone(),Pick::Ordinal(_)=>vec![oracle[3].clone()],Pick::Name(_)=>vec![oracle[4].clone()]};
            let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
            let mut download=view.nzb_own(id,pick,&pool).unwrap().unwrap();let mut actual=String::new();download.body.read_to_string(&mut actual).unwrap();
            let expected=crate::nzb::render_checked(&row,&selected,std::slice::from_ref(&row.group_name),matches!(pick,Pick::Whole));
            assert_eq!(actual,expected.xml);assert_eq!(download.gaps,0);
        }
        // Corrupt raw bytea is rejected by the same strict XML text reader on
        // both sides of the inline boundary; it is never decoded lossily.
        for (original,length) in [(&ids[3],8192),(&ids[4],8193)] {
            let mut invalid=vec![b'a';length];invalid[length-1]=0xff;
            let rewrite=|client:&mut postgres::Client,from:&[u8],to:&[u8]|match crate::article_local::of_schema(&schema.name) {
                Some(store)=>store.test_rewrite_message_id(id,from,to).unwrap() as u64,
                None=>client.execute("update segments set message_id=$2 where release_id=$1 and message_id=$3",&[&id,&to,&from]).unwrap(),
            };
            assert_eq!(rewrite(&mut client,original.as_bytes(),&invalid),1);
            {
                let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
                let file=if length==8192 {"case3.bin"}else{"case4.bin"};
                let error=view.nzb_own(id,Pick::Name(file),&pool).err().expect("invalid UTF-8 cannot become an NZB");
                assert!(error.chain().any(|cause|cause.to_string().contains("invalid UTF-8 in NZB text")));
            }
            assert_eq!(rewrite(&mut client,&invalid,original.as_bytes()),1);
        }
    }

    #[test]
    #[ignore="requires explicit isolated PostgreSQL and bounded NZB spool admission"]
    fn short_message_ids_do_not_require_per_segment_database_reads() {
        use crate::search::{ArticleRow,ReleaseRow};
        let schema=crate::pg::testing::Schema::native_writer().expect("explicit isolated PG fixture");
        let mut client=schema.pool.get().unwrap();
        let mut articles=(0..256).map(|n| {
            let filename=if n<128 {"a.bin"} else {"z.bin"};
            Article {filename:Some(filename.into()),subject:format!("{filename} yEnc ({}/128)",n%128+1),
                message_id:format!("<<part{n}.字幕&@example.invalid>>"),part:Some(n%128+1),total_parts:Some(128),bytes:17,..Default::default()}
        }).collect::<Vec<_>>();
        let mut duplicate=articles[63].clone();duplicate.message_id="<duplicate-part64@example.invalid>".into();articles.push(duplicate);
        for n in 0..2 {
            articles.push(Article {filename:None,subject:format!("original nameless envelope {n} & subtitle"),
                message_id:format!("<envelope{n}@example.invalid>"),part:Some(1),total_parts:Some(1),bytes:23,..Default::default()});
        }
        let release=Release {name:"Bounded.Message.Id.Release".into(),group:"alt.binaries.test".into(),poster:"poster&test".into(),
            date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),articles,..Default::default()};
        crate::pg::release_writer::save(&mut client,std::slice::from_ref(&release),1).unwrap();
        let id:i64=client.query_one("select id from releases",&[]).unwrap().get(0);
        let row=ReleaseRow {id,name:release.name.clone(),group_name:release.group.clone(),complete:true,..Default::default()};
        let mut oracle=release.articles.iter().map(|a|ArticleRow {message_id:a.message_id.clone(),filename:a.filename.clone(),part:a.part,
            total_parts:a.total_parts,bytes:Some(a.bytes),subject:Some(a.subject.clone()),poster:Some(release.poster.clone()),posted_date:Some(release.date.clone())}).collect::<Vec<_>>();
        // The renderer consumes already ordered articles. Nameless envelopes
        // precede named files; parts and raw message bytes order each file.
        oracle.sort_by(|a,b|(&a.filename,a.part,a.message_id.as_bytes()).cmp(&(&b.filename,b.part,b.message_id.as_bytes())));
        assert_eq!(oracle[0].message_id,"<envelope0@example.invalid>");
        assert_eq!(oracle[1].message_id,"<envelope1@example.invalid>");
        let root=tempfile::tempdir().unwrap();
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(root.path(),std::fs::Permissions::from_mode(0o700)).unwrap();}
        let pool=Pool::new(root.path().into(),crate::nzb_spool::Limits {requests:6,per_request_bytes:1<<20,total_bytes:6<<20,free_bytes:0}).unwrap();
        for (pick,selected) in [(Pick::Whole,oracle.clone()),(Pick::Ordinal(1),vec![oracle[0].clone()]),
            (Pick::Ordinal(3),oracle.iter().filter(|a|a.filename.as_deref()==Some("a.bin")).cloned().collect()),
            (Pick::Name("z.bin"),oracle.iter().filter(|a|a.filename.as_deref()==Some("z.bin")).cloned().collect())] {
            let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
            let before=view.guard_query_count();
            let mut download=view.nzb_own(id,pick,&pool).unwrap().unwrap();let mut actual=String::new();download.body.read_to_string(&mut actual).unwrap();
            let expected=crate::nzb::render_checked(&row,&selected,std::slice::from_ref(&row.group_name),matches!(pick,Pick::Whole));
            assert_eq!(actual,expected.xml);assert_eq!(download.gaps,0);assert_eq!(actual.matches("<segment ").count(),selected.len());
            let guards=view.guard_query_count()-before;
            assert!(guards<128,"short message IDs must share streamed inventory reads; executed {guards} timeout-guard SQL queries");
        }
    }

    #[test]
    #[ignore="requires explicit isolated fresh PostgreSQL and spool admission"]
    fn real_native_writer_nzb_matches_own_payload_oracle_and_file_selection() {
        let schema=crate::pg::testing::Schema::native_writer().expect("explicit isolated PG fixture");
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

#[cfg(test)]
mod sparse_nameless_tests {
    use super::*;
    use crate::{parser::{Article,Release},search::{ArticleRow,ReleaseRow},pg::fresh_reader::Budget};
    use std::time::{Duration,Instant};
    #[test]
    #[ignore="requires explicit isolated PostgreSQL and bounded native spool admission"]
    fn sparse_nameless_envelopes_are_distinct_files_with_matching_list_and_get_ordinals() {
        let schema=crate::pg::testing::Schema::native_writer().expect("explicit isolated PostgreSQL");let mut client=schema.pool.get().unwrap();
        let first_subject=format!("{}tail", "α & \"original\" ".repeat(800));
        let first=Release {name:"Sparse.Envelope.Release".into(),group:"alt.binaries.test".into(),poster:"first & poster".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
            articles:vec![Article {filename:None,subject:first_subject.clone(),message_id:"<alpha@example.invalid>".into(),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}],..Default::default()};
        let mut second=first.clone();second.poster="second <poster>".into();second.date="Thu, 02 Jan 2020 00:00:00 +0000".into();
        second.articles[0].subject="second independent subject".into();second.articles[0].message_id="<beta@example.invalid>".into();second.articles[0].bytes=23;
        let mut named=first.clone();named.poster="current named poster".into();named.date="Fri, 03 Jan 2020 00:00:00 +0000".into();
        named.articles=vec![Article {filename:Some("z.mkv".into()),subject:"z.mkv yEnc (1/1)".into(),message_id:"<named-a@example.invalid>".into(),part:Some(1),total_parts:Some(1),bytes:31,..Default::default()},
            Article {filename:Some("z.mkv".into()),subject:"z.mkv yEnc (1/1)".into(),message_id:"<named-b@example.invalid>".into(),part:Some(1),total_parts:Some(1),bytes:37,..Default::default()}];
        let stats=crate::pg::release_writer::save(&mut client,&[first.clone(),second.clone(),named.clone()],1).unwrap();
        assert_eq!(stats,crate::pg::release_writer::SaveStats {releases:1,files:2,parts:4});
        let id:i64=client.query_one("select id from releases where name='Sparse.Envelope.Release'",&[]).unwrap().get(0);
        let row=ReleaseRow {id,name:first.name.clone(),group_name:first.group.clone(),complete:true,..Default::default()};
        let articles=vec![
            ArticleRow {message_id:first.articles[0].message_id.clone(),filename:None,part:Some(1),total_parts:Some(1),bytes:Some(17),subject:Some(first_subject),poster:Some(first.poster.clone()),posted_date:Some(first.date.clone())},
            ArticleRow {message_id:second.articles[0].message_id.clone(),filename:None,part:Some(1),total_parts:Some(1),bytes:Some(23),subject:Some(second.articles[0].subject.clone()),poster:Some(second.poster.clone()),posted_date:Some(second.date.clone())},
            ArticleRow {message_id:named.articles[0].message_id.clone(),filename:Some("z.mkv".into()),part:Some(1),total_parts:Some(1),bytes:Some(31),subject:Some(named.articles[0].subject.clone()),poster:Some(named.poster.clone()),posted_date:Some(named.date.clone())},
            ArticleRow {message_id:named.articles[1].message_id.clone(),filename:Some("z.mkv".into()),part:Some(1),total_parts:Some(1),bytes:Some(37),subject:Some(named.articles[1].subject.clone()),poster:Some(named.poster.clone()),posted_date:Some(named.date.clone())}];
        let root=tempfile::tempdir().unwrap();
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(root.path(),std::fs::Permissions::from_mode(0o700)).unwrap();}
        let pool=Pool::new(root.path().into(),crate::nzb_spool::Limits {requests:6,per_request_bytes:1<<20,total_bytes:6<<20,free_bytes:0}).unwrap();
        let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
        for (pick,expected) in [(Pick::Whole,articles.as_slice()),(Pick::Ordinal(1),&articles[0..1]),(Pick::Ordinal(2),&articles[1..2]),(Pick::Ordinal(3),&articles[2..4]),(Pick::Name("z.mkv"),&articles[2..4])] {
            let mut actual=view.nzb_own(id,pick,&pool).unwrap().unwrap();let mut xml=String::new();actual.body.read_to_string(&mut xml).unwrap();
            let expected=crate::nzb::render_checked(&row,expected,std::slice::from_ref(&row.group_name),matches!(pick,Pick::Whole));
            assert_eq!(xml,expected.xml);assert_eq!(actual.gaps,0);
        }
        let mut files=String::new();view.file_list(id,&pool).unwrap().unwrap().read_to_string(&mut files).unwrap();
        assert_eq!(files,format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<files id=\"{id}\" count=\"3\">\n  <file n=\"1\" size=\"17\" articles=\"1\"/>\n  <file n=\"2\" size=\"23\" articles=\"1\"/>\n  <file n=\"3\" name=\"z.mkv\" size=\"68\" articles=\"2\"/>\n</files>\n"));
        assert!(view.nzb_own(id,Pick::Ordinal(4),&pool).unwrap().is_none());
        assert!(view.nzb_own(id,Pick::Name(""),&pool).unwrap().is_none());
    }
    #[test]
    #[ignore="requires explicit isolated PostgreSQL and bounded native spool admission"]
    fn uncaptured_empty_filename_cannot_be_relabelled_as_a_literal_none_envelope() {
        let schema=crate::pg::testing::Schema::native_writer().expect("explicit isolated PostgreSQL");let mut client=schema.pool.get().unwrap();
        let release=Release {name:"Empty.Filename.Control".into(),group:"alt.binaries.test".into(),poster:"poster".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
            articles:vec![Article {filename:Some(String::new()),subject:"empty filename input".into(),message_id:"empty@example.invalid".into(),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()},
                Article {filename:Some(String::new()),subject:"another empty filename input".into(),message_id:"another-empty@example.invalid".into(),part:Some(1),total_parts:Some(1),bytes:23,..Default::default()}],..Default::default()};
        assert_eq!(crate::pg::release_writer::save(&mut client,&[release],1).unwrap().parts,2);
        assert_eq!(client.query_one("select count(*) from segment_nzb_metadata",&[]).unwrap().get::<_,i64>(0),0);
        if let Some(store)=crate::article_local::of_schema(&schema.name){assert_eq!(store.test_envelope_count().unwrap(),0);}
        let id:i64=client.query_one("select id from releases",&[]).unwrap().get(0);
        let root=tempfile::tempdir().unwrap();
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(root.path(),std::fs::Permissions::from_mode(0o700)).unwrap();}
        let pool=Pool::new(root.path().into(),crate::nzb_spool::Limits {requests:6,per_request_bytes:1<<20,total_bytes:6<<20,free_bytes:0}).unwrap();
        let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
        assert!(view.nzb_own(id,Pick::Whole,&pool).is_err(),"missing original metadata is unavailable, not a representative fallback");
        assert!(view.file_list(id,&pool).is_err(),"uncaptured empty filenames cannot invent unnamed list ordinals");
    }
}

#[cfg(test)]
mod owned_adapter_tests {
    use super::*;
    use crate::{parser::{Article,Release},pg::fresh_reader::Budget};
    use std::time::{Duration,Instant};

    #[test]
    #[ignore="requires explicit isolated PostgreSQL and bounded native spool admission"]
    fn owned_native_adapter_retains_gap_details_copies_and_whole_download_refusal() {
        let schema=crate::pg::testing::Schema::native_writer().expect("explicit isolated PostgreSQL");let mut client=schema.pool.get().unwrap();
        let release=|name:&str,total|Release {name:name.into(),group:"alt.binaries.test".into(),poster:"accepted poster".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
            articles:[17,23].into_iter().enumerate().map(|(n,bytes)|Article {filename:Some("video.mkv".into()),subject:format!("video.mkv yEnc (1/{total})"),
                message_id:format!("{name}-{n}@example.invalid"),part:Some(1),total_parts:Some(total),bytes,..Default::default()}).collect(),..Default::default()};
        assert_eq!(crate::pg::release_writer::save(&mut client,&[release("Partial.Adapter.Example",3),release("Complete.Adapter.Example",1)],1).unwrap().parts,4);
        let partial:i64=client.query_one("select id from releases where name='Partial.Adapter.Example'",&[]).unwrap().get(0);
        let complete:i64=client.query_one("select id from releases where name='Complete.Adapter.Example'",&[]).unwrap().get(0);
        let(row,articles)=crate::pg::release_reader::inventory(&mut client,partial).unwrap().unwrap().into_nzb_rows();
        let expected=crate::nzb::render_checked(&row,&articles,std::slice::from_ref(&row.group_name),false);
        let(complete_row,complete_articles)=crate::pg::release_reader::inventory(&mut client,complete).unwrap().unwrap().into_nzb_rows();
        let complete_expected=crate::nzb::render_checked(&complete_row,&complete_articles,std::slice::from_ref(&complete_row.group_name),true);
        let root=tempfile::tempdir().unwrap();
        #[cfg(unix)]{use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(root.path(),std::fs::Permissions::from_mode(0o700)).unwrap();}
        let pool=Pool::new(root.path().into(),crate::nzb_spool::Limits {requests:8,per_request_bytes:1<<20,total_bytes:8<<20,free_bytes:0}).unwrap();
        let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
        let built=view.nzb_owned_build(partial,Pick::Name("video.mkv"),&pool).unwrap().unwrap();
        assert_eq!(built.xml,expected.xml);assert_eq!(built.xml.matches("<segment ").count(),2);
        assert_eq!(built.gaps,vec![crate::nzb::FileGap {files:false,filename:Some("video.mkv".into()),total:Some(3),have:1,missing:vec![2,3],stray:0}]);
        assert!(!built.complete());
        assert!(view.nzb_owned_build(partial,Pick::Whole,&pool).is_err(),"owned API must retain the existing incomplete-whole refusal");
        assert!(view.nzb_own(partial,Pick::Whole,&pool).is_err(),"stream API must retain the existing incomplete-whole refusal");
        let built=view.nzb_owned_build(complete,Pick::Whole,&pool).unwrap().unwrap();
        assert!(built.complete());assert_eq!(built.xml,complete_expected.xml);assert_eq!(built.xml.matches("<segment ").count(),2);
        assert!(view.nzb_owned_build(complete,Pick::Ordinal(0),&pool).unwrap().is_none());
    }
}

#[cfg(test)]
mod local_parity_tests {
    use super::*;
    use crate::{article_local,parser::{Article,Release},pg::fresh_reader::Budget};
    use std::time::{Duration,Instant};

    /// Everything the NZB builder and the file lists say about one catalogue, kept in PostgreSQL or in a local store.
    fn observed(local:bool)->Vec<String> {
        let schema=crate::pg::testing::Schema::native_writer().expect("explicit isolated PostgreSQL");let mut client=schema.pool.get().unwrap();
        let mut own=None;
        if !local {article_local::unregister(&schema.name);}
        else if article_local::of_schema(&schema.name).is_none() {
            let dir=tempfile::tempdir().unwrap();
            article_local::register(&schema.name,article_local::Store::open_writer(dir.path(),&schema.name).unwrap()).unwrap();own=Some(dir);
        }
        let article=|file:Option<&str>,mid:&str,part:Option<i64>,total:i64,bytes:i64|Article {filename:file.map(str::to_owned),
            subject:format!("subject & \"{}\" of {} ({}/{total})",file.unwrap_or("no name"),&mid[..mid.len().min(24)],part.unwrap_or(0)),
            message_id:mid.into(),part,total_parts:Some(total),bytes,..Default::default()};
        let long_id=format!("<<{}é&@example.invalid>>","L".repeat(9000));
        let mut long_subject=article(None,"<nameless-b@example.invalid>",Some(1),1,29);long_subject.subject=format!("{}tail","α & \"long\" ".repeat(900));
        let whole=Release {name:"Parity.Whole.Release".into(),group:"alt.binaries.test".into(),poster:"poster & <one>".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
            articles:vec![article(Some("a.bin"),"<a-1@example.invalid>",Some(1),3,11),article(Some("a.bin"),"<a-2-z@example.invalid>",Some(2),3,13),
                article(Some("a.bin"),"<a-2-a@example.invalid>",Some(2),3,17),article(Some("a.bin"),"<<a-3@example.invalid>>",Some(3),3,19),
                article(Some("z.字幕.bin"),&long_id,Some(1),1,23),article(None,"<nameless-a@example.invalid>",Some(1),1,27),long_subject],..Default::default()};
        let gaps=Release {name:"Parity.Gaps.Release".into(),group:"alt.binaries.test".into(),poster:"poster two".into(),date:"Thu, 02 Jan 2020 00:00:00 +0000".into(),
            articles:vec![article(Some("b.bin"),"<b-1@example.invalid>",Some(1),4,31),article(Some("b.bin"),"<b-3-b@example.invalid>",Some(3),4,37),
                article(Some("b.bin"),"<b-3-a@example.invalid>",Some(3),4,41),article(Some("b.bin"),"<b-unnumbered-z@example.invalid>",None,4,43),
                article(Some("b.bin"),"<b-unnumbered-a@example.invalid>",None,4,47),article(None,"<nameless-c@example.invalid>",Some(2),2,53),
                article(None,"<nameless-d@example.invalid>",None,2,59)],..Default::default()};
        assert_eq!(crate::pg::release_writer::save(&mut client,&[whole,gaps],1).unwrap().parts,14);
        // The articles really are where this run says they are.
        let in_postgres:i64=client.query_one("select count(*) from segments",&[]).unwrap().get(0);
        assert_eq!(in_postgres,if local{0}else{14});
        assert_eq!(article_local::of_schema(&schema.name).is_some(),local);
        let root=tempfile::tempdir().unwrap();
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(root.path(),std::fs::Permissions::from_mode(0o700)).unwrap();}
        let pool=Pool::new(root.path().into(),crate::nzb_spool::Limits {requests:8,per_request_bytes:1<<20,total_bytes:8<<20,free_bytes:0}).unwrap();
        let mut seen=Vec::new();
        for name in ["Parity.Whole.Release","Parity.Gaps.Release"] {
            let id:i64=client.query_one("select id from releases where name=$1",&[&name]).unwrap().get(0);
            let mut say=|what:String,told:Result<Option<String>>|seen.push(format!("{name} #{id} {what}: {}",match told {
                Ok(Some(text))=>format!("ok {text}"),Ok(None)=>"none".into(),Err(error)=>format!("refused {error}")}));
            let picks=[Pick::Whole,Pick::Ordinal(0),Pick::Ordinal(1),Pick::Ordinal(2),Pick::Ordinal(3),Pick::Ordinal(4),Pick::Ordinal(5),
                Pick::Name("a.bin"),Pick::Name("z.字幕.bin"),Pick::Name("b.bin"),Pick::Name(""),Pick::Name("missing.bin")];
            for (n,pick) in picks.into_iter().enumerate() {
                let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(20)).unwrap();
                say(format!("stream {n}"),view.nzb_own(id,pick,&pool).and_then(|found|found.map(|mut download| {
                    let mut xml=String::new();download.body.read_to_string(&mut xml)?;Ok(format!("{} gaps\n{xml}",download.gaps))}).transpose()));
                drop(view);
                let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(20)).unwrap();
                say(format!("owned {n}"),view.nzb_owned_build(id,pick,&pool).map(|found|found.map(|built|format!("{:?}\n{}",built.gaps,built.xml))));
            }
            let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(20)).unwrap();
            say("files".into(),view.file_list(id,&pool).and_then(|found|found.map(|mut list|{let mut xml=String::new();list.read_to_string(&mut xml)?;Ok(xml)}).transpose()));
            say("listed".into(),view.nzb_listed_files(id).map(|found|found.map(|files|files.iter().map(|file|format!("{} {:?} {} {}",file.n,file.name,file.bytes,file.articles)).collect::<Vec<_>>().join("|"))));
            say("download".into(),view.prepare_download(id,&pool).and_then(|found|found.map(|(name,first,mut download)| {
                let mut xml=String::new();download.body.read_to_string(&mut xml)?;Ok(format!("{name} {first:?}\n{xml}"))}).transpose()));
            drop(view);
            let (row,articles)=crate::pg::release_reader::inventory(&mut client,id).unwrap().unwrap().into_nzb_rows();
            seen.push(format!("{name} inventory {row:?} {}",articles.iter().map(|a|format!("{:?} {:?} {:?} {}",a.filename,a.part,a.bytes,a.message_id)).collect::<Vec<_>>().join("|")));
        }
        if own.is_some(){article_local::unregister(&schema.name);}
        seen
    }

    #[test]
    #[ignore="requires explicit isolated PostgreSQL and bounded native spool admission"]
    fn an_nzb_and_its_file_lists_are_the_same_bytes_from_postgres_and_from_the_local_store() {
        let postgres=observed(false);let local=observed(true);
        assert_eq!(postgres.len(),local.len());
        for (postgres,local) in postgres.iter().zip(&local){assert_eq!(postgres,local);}
        // The comparison is of real documents, not of two refusals.
        // by name: which release gets the lower id follows the order saves of a group's shards land in
        let whole=postgres.iter().find(|line|line.starts_with("Parity.Whole.Release #")&&line.contains(" stream 0: ok 0 gaps")).expect("the whole release builds");
        assert_eq!(whole.matches("<file ").count(),4);assert_eq!(whole.matches("<segment ").count(),7);
        assert!(whole.contains(&format!(">{}é&amp;@example.invalid</segment>","L".repeat(9000))));
        assert!(whole.find("nameless-a@").unwrap()<whole.find("nameless-b@").unwrap()&&whole.find("a-2-a@").unwrap()<whole.find("a-2-z@").unwrap());
        let gaps=postgres.iter().find(|line|line.starts_with("Parity.Gaps.Release #")&&line.contains(" stream 9: ok 1 gaps")).expect("the file with gaps builds");
        assert!(gaps.find("b-unnumbered-a@").unwrap()<gaps.find("b-unnumbered-z@").unwrap()&&gaps.find("b-unnumbered-z@").unwrap()<gaps.find("b-1@").unwrap());
        assert!(gaps.contains("number=\"None\"")&&gaps.find("b-3-a@").unwrap()<gaps.find("b-3-b@").unwrap());
        assert!(postgres.iter().any(|line|line.starts_with("Parity.Gaps.Release #")&&line.contains(" files: ok")&&line.contains("count=\"3\"")));
        assert!(postgres.iter().any(|line|line.starts_with("Parity.Whole.Release #")&&line.contains(" listed: ok 1 None 27 1|2 None 29 1|3 Some(\"a.bin\") 60 4|4 Some(\"z.字幕.bin\") 23 1")));
    }
}
