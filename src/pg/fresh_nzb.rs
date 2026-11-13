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

pub(super) struct Message<'a,'c> {view:&'a ReadView<'c>,release:i64,key:[u8;32],at:i64,end:i64,buffer:Vec<u8>,used:usize}
impl<'a,'c> Message<'a,'c> {
    fn chunk(view:&ReadView<'_>,release:i64,key:&[u8;32],at:i64,n:i32)->Result<Vec<u8>> {
        view.before_query()?;
        let row=view.tx.borrow_mut().query_opt("select substring(message_id from $3::integer for $4::integer)
            from segments where release_id=$1 and message_key=$2",&[&release,&&key[..],&i32::try_from(at+1)?,&n])?.context("native article disappeared")?;
        let bytes:Vec<u8>=row.try_get(0)?;ensure!(bytes.len()<=n as usize,"native article range exceeded admission");Ok(bytes)
    }
    pub(super) fn raw(view:&'a ReadView<'c>,release:i64,key:[u8;32],length:i64)->Result<Self> {
        ensure!(length>=0,"negative native article extent");
        Ok(Self {view,release,key,at:0,end:length,buffer:Vec::new(),used:0})
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
struct SparseText<'view,'client> {view:&'view ReadView<'client>,release:i64,key:[u8;32],field:SparseField,at:i64,end:i64,buffer:Vec<u8>,used:usize}
impl<'view,'client> SparseText<'view,'client> {
    fn new(view:&'view ReadView<'client>,release:i64,header:SparseHeader,field:SparseField)->Self {
        let end=match field {SparseField::Subject=>header.subject_bytes,SparseField::Poster=>header.poster_bytes};
        Self {view,release,key:header.key,field,at:0,end,buffer:Vec::new(),used:0}
    }
}
impl Read for SparseText<'_,'_> {
    fn read(&mut self,out:&mut[u8])->io::Result<usize> {
        if out.is_empty(){return Ok(0);}
        if self.used==self.buffer.len()&&self.at<self.end {
            let fill=||->Result<Vec<u8>> {
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
                part,bytes,octet_length(message_id)::bigint from segments where release_id=$1 and file_id=$2
                and($3::bytea is null or message_key=$3) order by part nulls first,message_id",params)?;
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
        let mut xml=XmlWriter::new(pool.open()?,group.as_bytes())?;let mut gaps=0u64;let mut details=collect_gaps.then(Vec::new);
        for _ in 0..selected {
            let descriptor=FileDescriptor::read(&mut plan)?;let file=descriptor.file;let count=descriptor.count;
            let mut check=GapCheck::new(descriptor.expected);for _ in 0..count{let (_,part,_,_)=segment(&mut plan)?;check.push(part)?;}
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
            if let Some(header)=descriptor.sparse {
                xml.file(SparseText::new(self,id,header,SparseField::Poster),header.timestamp,SparseText::new(self,id,header,SparseField::Subject))?;
            }else{
                xml.file(Text::new(self,TextField::ReleasePoster(id)),crate::dates::article_timestamp(&posted),Text::new(self,TextField::FileSubject(descriptor.file)))?;
            }
            xml.group(group.as_bytes())?;xml.segments()?;
            for _ in 0..count{let(key,part,bytes,length)=segment(&mut plan)?;xml.segment(bytes,part,Message::new(self,id,key,length)?)?;}
            xml.end_file()?;
        }
        ensure!(plan.read(&mut[0u8;1])?==0,"native NZB inventory has trailing rows");drop(plan);
        let body=xml.finish()?.finish()?;
        let disposition=crate::nzb_http::Disposition::prepare(pool,Text::new(self,TextField::ReleaseName(id)),id)?;
        Ok(Some(Download {body,disposition,gaps,details}))
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

#[cfg(test)]
mod sparse_nameless_tests {
    use super::*;
    use crate::{parser::{Article,Release},search::{ArticleRow,ReleaseRow},pg::fresh_reader::Budget};
    use std::time::{Duration,Instant};
    #[test]
    #[ignore="requires explicit isolated PostgreSQL and bounded native spool admission"]
    fn sparse_nameless_envelopes_are_distinct_files_with_matching_list_and_get_ordinals() {
        let schema=crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL");let mut client=schema.pool.get().unwrap();
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
        let schema=crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL");let mut client=schema.pool.get().unwrap();
        let release=Release {name:"Empty.Filename.Control".into(),group:"alt.binaries.test".into(),poster:"poster".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
            articles:vec![Article {filename:Some(String::new()),subject:"empty filename input".into(),message_id:"empty@example.invalid".into(),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()},
                Article {filename:Some(String::new()),subject:"another empty filename input".into(),message_id:"another-empty@example.invalid".into(),part:Some(1),total_parts:Some(1),bytes:23,..Default::default()}],..Default::default()};
        assert_eq!(crate::pg::release_writer::save(&mut client,&[release],1).unwrap().parts,2);
        assert_eq!(client.query_one("select count(*) from segment_nzb_metadata",&[]).unwrap().get::<_,i64>(0),0);
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
        let schema=crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL");let mut client=schema.pool.get().unwrap();
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
