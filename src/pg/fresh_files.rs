//! Direct fresh native file listing. No legacy shard, provider or history
//! publication participates; empty SQL is an ordinary empty fresh catalog.
use super::fresh_reader::ReadView;
use anyhow::{Context, Result, ensure};
use postgres::fallible_iterator::FallibleIterator;
use std::{io::{self, Read, Write}, sync::Arc};
use crate::{nzb_spool::{Pool,Spool}, nzb_stream::FilesWriter};

/// Closed identifiers: no catalog/user string is interpolated into SQL.
#[derive(Clone, Copy)]
pub(super) enum TextField { ReleaseName(i64), ReleasePoster(i64), ReleaseDate(i64), ReleaseGroup(i64), FileName(i64), FileSubject(i64) }
impl TextField {
    fn query(self) -> (&'static str,i64) {
        match self {
            Self::ReleaseName(id) => ("select substring(coalesce(display_name,name) from $2 for 8192) from releases where id=$1",id),
            Self::ReleasePoster(id) => ("select substring(poster from $2 for 8192) from releases where id=$1",id),
            Self::ReleaseDate(id) => ("select substring(posted_date from $2 for 8192) from releases where id=$1",id),
            Self::ReleaseGroup(id) => ("select substring(group_name from $2 for 8192) from releases where id=$1",id),
            Self::FileName(id) => ("select substring(filename from $2 for 8192) from files where id=$1",id),
            Self::FileSubject(id) => ("select substring(coalesce(subject,'') from $2 for 8192) from files where id=$1",id),
        }
    }
}

/// SQL substring positions are characters. Each returned chunk contains at
/// most 8192 Unicode scalars (32768 UTF-8 bytes), with no codepoint split and
/// no full giant value decoded by the client. Chunking never truncates text.
pub(super) struct Text<'view,'client> {
    view: &'view ReadView<'client>, field: TextField, next: i32,
    buffer: Vec<u8>, at: usize, done: bool,
}
impl<'view,'client> Text<'view,'client> {
    pub(super) fn new(view:&'view ReadView<'client>,field:TextField)->Self {
        Self {view,field,next:1,buffer:Vec::new(),at:0,done:false}
    }
}
impl Read for Text<'_,'_> {
    fn read(&mut self,out:&mut[u8])->io::Result<usize> {
        if out.is_empty() { return Ok(0); }
        if self.at==self.buffer.len() && !self.done {
            let fill=||->Result<(Vec<u8>,i32)> {
                self.view.before_query()?;
                let (query,id)=self.field.query();
                let row=self.view.tx.borrow_mut().query_opt(query,&[&id,&self.next])?.context("native text row disappeared")?;
                let text:String=row.try_get(0)?;
                let chars=i32::try_from(text.chars().count())?;
                ensure!(chars<=8192 && text.len()<=32768,"native text range exceeded admission");
                Ok((text.into_bytes(),chars))
            };
            let (bytes,chars)=fill().map_err(io::Error::other)?;
            self.done=chars<8192;
            self.next=self.next.checked_add(chars).ok_or_else(||io::Error::other("native text position overflow"))?;
            self.buffer=bytes;self.at=0;
        }
        let count=out.len().min(self.buffer.len()-self.at);
        out[..count].copy_from_slice(&self.buffer[self.at..self.at+count]);self.at+=count;
        Ok(count)
    }
}

impl ReadView<'_> {
    /// A bounded metadata cursor is spooled before filename range reads. This
    /// avoids retaining all filenames/segments or nested postgres row cursors.
    /// The disk/concurrency permit is kept through completion/error cleanup.
    pub(crate) fn file_list(&self,id:i64,pool:&Arc<Pool>)->Result<Option<Spool>> {
        if !self.visible(id)? {return Ok(None);}
        let mut descriptors=pool.open()?;
        self.before_query()?;
        let mut tx=self.tx.borrow_mut();
        let mut rows=tx.query_raw("with inventory as(
            select f.id,f.filename,count(s.message_key) articles,coalesce(sum(s.bytes),0)::bigint bytes,null::bigint singleton_part,null::bytea singleton_mid
            from files f join segments s on s.file_id=f.id and s.release_id=f.release_id
            where f.release_id=$1 and f.filename<>'' group by f.id
            union all
            select f.id,f.filename,1::bigint,s.bytes,s.part,s.message_id
            from files f join segments s on s.file_id=f.id and s.release_id=f.release_id
            where f.release_id=$1 and f.filename='')
            select id,octet_length(filename)::bigint,articles,bytes from inventory
            order by filename collate \"C\",id,singleton_part nulls first,singleton_mid",[&id])?;
        let mut count=0u64;
        while let Some(row)=rows.next()? {
            self.check_deadline()?;
            let values:[i64;4]=[row.try_get(0)?,row.try_get(1)?,row.try_get(2)?,row.try_get(3)?];
            ensure!(values[0]>0 && values[1]>=0 && values[2]>0,"invalid native file inventory");
            for value in values {descriptors.write_all(&value.to_le_bytes())?;}
            count=count.checked_add(1).context("native file count overflow")?;
        }
        drop(rows);drop(tx);
        let mut descriptors=descriptors.finish()?;
        let mut xml=FilesWriter::new(pool.open()?,id,count)?;
        for _ in 0..count {
            let mut bytes=[0u8;32];descriptors.read_exact(&mut bytes)?;
            let value=|at|i64::from_le_bytes(bytes[at..at+8].try_into().unwrap());
            let file=value(0);let length=value(8);let articles=u64::try_from(value(16))?;
            let name=(length!=0).then(||Text::new(self,TextField::FileName(file)));
            xml.file(name,value(24),articles)?;
        }
        ensure!(descriptors.read(&mut[0u8;1])?==0,"native file inventory has trailing rows");
        Ok(Some(xml.finish()?.finish()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pg::fresh_reader::Budget;
    use std::time::{Instant,Duration};
    #[test]
    #[ignore="requires explicit isolated PostgreSQL and bounded spool runtime admission"]
    fn direct_files_use_native_ids_preserve_unicode_and_all_same_part_copies() {
        let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG test database");
        let mut client=schema.pool.get().unwrap();
        let id:i64=client.query_one("insert into releases(group_name,is_collection,identity_key,name,poster,posted_date,is_obfuscated)
            values('alt.binaries.test',false,$1,'Example.Release','poster','2020-01-01',false) returning id",&[&vec![1u8;32]]).unwrap().get(0);
        let name="A.字幕.\"&.mkv";
        let file:i64=client.query_one("insert into files(release_id,filename_key,filename,expected) values($1,$2,$3,1) returning id",&[&id,&vec![2u8;32],&name]).unwrap().get(0);
        for n in 0..19u8 {
            client.execute("insert into segments(release_id,file_id,message_key,message_id,part,bytes) values($1,$2,$3,$4,1,17)",
                &[&id,&file,&vec![n;32],&format!("<part{n}@example.test>").into_bytes()]).unwrap();
        }
        let root=tempfile::tempdir().unwrap();
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(root.path(),std::fs::Permissions::from_mode(0o700)).unwrap();}
        let pool=Pool::new(root.path().into(),crate::nzb_spool::Limits {requests:3,per_request_bytes:1<<20,total_bytes:3<<20,free_bytes:0}).unwrap();
        let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(3)).unwrap();
        let mut body=String::new();view.file_list(id,&pool).unwrap().unwrap().read_to_string(&mut body).unwrap();
        assert_eq!(body,format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<files id=\"{id}\" count=\"1\">\n  <file n=\"1\" name=\"A.字幕.&quot;&amp;.mkv\" size=\"323\" articles=\"19\"/>\n</files>\n"));
        assert!(view.file_list(id+1,&pool).unwrap().is_none());
    }
}
