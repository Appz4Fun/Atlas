//! Native public NZB/CLI adapters. Selection happens before any legacy opener;
//! generation publishes the completed spool after its readonly snapshot ends.
use super::{fresh_reader::ReadView,fresh_nzb::Pick};
use anyhow::{Result,Context,ensure};
use std::path::{Path,PathBuf};

pub(crate) type PreparedDownload=(String,Option<String>,super::fresh_nzb::Download);

impl ReadView<'_> {
    pub(crate) fn nzb_export_name(&self,id:i64)->Result<Option<String>> {
        if !self.visible(id)?{return Ok(None);}
        self.before_query()?;
        let length:i64=self.tx.borrow_mut().query_one("select octet_length(coalesce(display_name,name))::bigint from releases where id=$1",&[&id])?.try_get(0)?;
        ensure!(length>=0,"negative native export name extent");
        self.admit_owned((length as u64).checked_mul(3).and_then(|n|n.checked_add(4096)).context("native export name extent overflow")?)?;
        self.before_query()?;
        let name:String=self.tx.borrow_mut().query_one("select coalesce(display_name,name) from releases where id=$1",&[&id])?.try_get(0)?;
        ensure!(name.len() as i64==length,"native export name extent changed");Ok(Some(name))
    }
    pub(crate) fn nzb_listed_files(&self,id:i64)->Result<Option<Vec<crate::nzb::ListedFile>>> {
        if !self.visible(id)?{return Ok(None);}
        let inventory="with inventory as(
            select f.id,f.filename,count(*) articles,coalesce(sum(s.bytes),0)::bigint bytes,null::bigint singleton_part,null::bytea singleton_mid,true sparse_present
            from files f join segments s on s.release_id=f.release_id and s.file_id=f.id where f.release_id=$1 and f.filename<>'' group by f.id
            union all select f.id,f.filename,1::bigint,s.bytes,s.part,s.message_id,m.message_key is not null
            from files f join segments s on s.release_id=f.release_id and s.file_id=f.id
            left join segment_nzb_metadata m on m.release_id=s.release_id and m.message_key=s.message_key
            where f.release_id=$1 and f.filename='')";
        self.before_query()?;
        let extent=self.tx.borrow_mut().query_one(&format!("{inventory} select count(*),coalesce(sum(octet_length(filename)::bigint),0)::bigint,coalesce(bool_and(sparse_present),true) from inventory"),&[&id])?;
        let count:i64=extent.try_get(0)?;let bytes:i64=extent.try_get(1)?;ensure!(count>=0&&bytes>=0,"negative native listed-file extent");
        ensure!(extent.try_get::<_,bool>(2)?,"native original nameless file metadata unavailable");
        self.admit_owned((count as u64).checked_mul(256).and_then(|n|(bytes as u64).checked_mul(3).and_then(|bytes|n.checked_add(bytes))).and_then(|n|n.checked_add(4096)).context("native listed-file extent overflow")?)?;
        self.before_query()?;
        let rows=self.tx.borrow_mut().query(&format!("{inventory} select filename,articles,bytes from inventory order by filename collate \"C\",id,singleton_part nulls first,singleton_mid"),&[&id])?;
        ensure!(rows.len() as i64==count,"native listed-file inventory changed");
        Ok(Some(rows.into_iter().enumerate().map(|(index,row)|{
            let name:String=row.try_get(0)?;let articles:i64=row.try_get(1)?;ensure!(articles>0,"native listed file has no articles");
            Ok(crate::nzb::ListedFile {n:index+1,name:(!name.is_empty()).then_some(name),bytes:row.try_get(2)?,articles:usize::try_from(articles)?})
        }).collect::<Result<Vec<_>>>()?))
    }
    /// End the same readonly snapshot with a completed, checked NZB. Callers
    /// may only begin downloader effects after this returns successfully.
    pub(crate) fn prepare_download(&self,id:i64,pool:&std::sync::Arc<crate::nzb_spool::Pool>)->Result<Option<PreparedDownload>> {
        let Some(name)=self.nzb_export_name(id)? else{return Ok(None)};
        self.before_query()?;
        let length=self.tx.borrow_mut().query_opt("select octet_length(filename)::bigint from files where release_id=$1 and filename<>'' order by filename collate \"C\",id limit 1",&[&id])?;
        let first=if let Some(length)=length {
            let length:i64=length.try_get(0)?;ensure!(length>=0,"negative native download filename extent");
            self.admit_owned((length as u64).checked_mul(3).and_then(|n|n.checked_add(4096)).context("native download filename extent overflow")?)?;
            self.before_query()?;
            Some(self.tx.borrow_mut().query_one("select filename from files where release_id=$1 and filename<>'' order by filename collate \"C\",id limit 1",&[&id])?.try_get::<_,String>(0)?)
        }else{None};
        self.before_query()?;
        let exists:bool=self.tx.borrow_mut().query_one("select exists(select 1 from segments where release_id=$1 limit 1)",&[&id])?.try_get(0)?;
        if !exists {return Ok(None);}
        let download=self.nzb_own(id,Pick::Whole,pool)?.context("native preflight release disappeared")?;
        Ok(Some((name,first,download)))
    }
}

pub(crate) fn build(storage:&super::StorageConfig,id:i64,pick:Pick<'_>)->Result<Option<crate::nzb::NzbBuild>> {
    crate::api::fresh::read(storage,|view|view.nzb_owned_build(id,pick,&crate::api::fresh::spools()?))
}
pub(crate) fn files(storage:&super::StorageConfig,id:i64)->Result<Option<Vec<crate::nzb::ListedFile>>> {
    crate::api::fresh::read(storage,|view|view.nzb_listed_files(id))
}
pub(crate) fn download_metadata(storage:&super::StorageConfig,id:i64)->Result<Option<PreparedDownload>> {
    crate::api::fresh::read(storage,|view|view.prepare_download(id,&crate::api::fresh::spools()?))
}
pub(crate) fn generate(storage:&super::StorageConfig,id:i64,output_dir:Option<&Path>)->Result<PathBuf> {
    let (name,download)=crate::api::fresh::read(storage,|view|{
        let name=view.nzb_export_name(id)?.context("release not found")?;
        let download=view.nzb_own(id,Pick::Whole,&crate::api::fresh::spools()?)?.context("no articles found")?;
        Ok((name,download))
    })?;
    publish_download(&name,id,output_dir,download)
}
pub(crate) fn publish_download(name:&str,id:i64,output_dir:Option<&Path>,download:super::fresh_nzb::Download)->Result<PathBuf> {
    let filename=crate::nzb::nzb_filename(name,Some(id));
    let target=output_dir.map_or_else(||PathBuf::from(&filename),|dir|dir.join(&filename));
    if download.gaps>0 {crate::ui::warn(&format!("incomplete: {} gap report(s), retained in the NZB metadata",download.gaps));}
    let until=std::time::Instant::now()+std::time::Duration::from_secs(30);
    download.body.publish_atomic(&target,||{
        if std::time::Instant::now()>=until{return Err(std::io::Error::new(std::io::ErrorKind::TimedOut,"native NZB publication deadline expired"));}Ok(())
    })?;
    crate::ui::success(&format!("Saved {}",target.display()));Ok(target)
}

#[cfg(test)]
mod listed_file_tests {
    use super::*;
    use crate::{parser::{Article,Release},pg::{self,fresh_reader::Budget}};
    use std::time::{Duration,Instant};
    #[test]
    #[ignore="requires explicit isolated fresh PostgreSQL schema and bounded native read admission"]
    fn cli_file_inventory_keeps_original_nameless_ordinals_and_refuses_uncaptured_empty_names() {
        let schema=pg::testing::Schema::new().expect("explicit isolated fresh schema");let mut client=schema.pool.get().unwrap();
        let articles=[(None,"<none-a@example.invalid>",2,17),(None,"<none-b@example.invalid>",1,23),
            (Some("movie.bin"),"<named-a@example.invalid>",1,19),(Some("movie.bin"),"<named-b@example.invalid>",1,29)].into_iter()
            .map(|(file,mid,part,bytes)|Article {filename:file.map(str::to_owned),subject:format!("original {mid}"),message_id:mid.into(),part:Some(part),total_parts:Some(2),bytes,..Default::default()}).collect();
        let saved=pg::release_writer::save(&mut client,&[Release {name:"CLI.Genuine.Nameless".into(),group:"alt.binaries.test".into(),poster:"original poster".into(),
            date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),articles,..Default::default()}],1).unwrap();assert_eq!((saved.releases,saved.files,saved.parts),(1,2,4));
        let good:i64=client.query_one("select id from releases where name='CLI.Genuine.Nameless'",&[]).unwrap().get(0);
        let saved=pg::release_writer::save(&mut client,&[Release {name:"CLI.Uncaptured.Empty".into(),group:"alt.binaries.test".into(),poster:"poster".into(),
            date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),articles:(0..2).map(|n|Article {filename:Some(String::new()),subject:format!("empty {n}"),
                message_id:format!("<empty-{n}@example.invalid>"),part:Some(1),total_parts:Some(1),bytes:31,..Default::default()}).collect(),..Default::default()}],1).unwrap();
        assert_eq!((saved.releases,saved.files,saved.parts),(1,1,2));
        let bad:i64=client.query_one("select id from releases where name='CLI.Uncaptured.Empty'",&[]).unwrap().get(0);
        {
            let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
            let files=view.nzb_listed_files(good).unwrap().unwrap();assert_eq!(files.len(),3);
            assert_eq!(files.iter().map(|file|(file.n,file.name.as_deref(),file.bytes,file.articles)).collect::<Vec<_>>(),
                [(1,None,23,1),(2,None,17,1),(3,Some("movie.bin"),48,2)]);
            assert!(view.nzb_listed_files(bad).is_err(),"uncaptured Some(empty) rows cannot be relabeled as original filename=None");
            assert!(view.nzb_listed_files(i64::MAX).unwrap().is_none());
        }
        client.execute("delete from segment_nzb_metadata where release_id=$1 and message_key=(select message_key from segments where release_id=$1 and message_id=$2)",&[&good,&b"<none-b@example.invalid>".as_slice()]).unwrap();
        let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
        assert!(view.nzb_listed_files(good).is_err(),"a missing original header refuses the whole list, never silently filters or renumbers files");
    }
}
