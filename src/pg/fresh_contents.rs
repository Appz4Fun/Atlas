//! Own-release facts from the actual fresh catalog. These readers do not
//! combine postings, infer payload trust, fetch providers, or open sidecars.
use super::fresh_reader::ReadView;
use anyhow::{Context,Result,ensure};
use postgres::fallible_iterator::FallibleIterator;
use crate::{archive::{Encryption,Password,PasswordTest,store::{InnerView,InnerFileRow}},release_info::{Contents,Names,NzbFile,MAX_LISTED_FILES}};

fn add(total:&mut i64,value:i64)->Result<()> {
    *total=total.checked_add(value).context("native contents total overflow")?;Ok(())
}
impl ReadView<'_> {
    pub(crate) fn own_drawer(&self,id:i64,as_card:bool)->Result<Option<String>> {
        let Some(release)=self.release(id)? else{return Ok(None)};
        let names=self.own_names(id)?.context("native drawer name disappeared")?;
        let inner=self.own_inner(id)?;let contents=self.own_contents(id)?;let probe=self.probe(id)?;
        let inputs=crate::web_meta::MetaInputs {release:&release,names:Some(&names),inner:&inner,contents:&contents,probe:probe.as_ref()};
        Ok(Some(self.own_drawer_loaded(&inputs,as_card)?))
    }
    pub(crate) fn own_drawer_loaded(&self,inputs:&crate::web_meta::MetaInputs<'_>,as_card:bool)->Result<String> {
        let id=inputs.release.id;
        let files=self.metadata_files(id)?;let main=crate::probe::step::main_file(&files).cloned();
        let first_id=main.as_ref().map(|main|self.metadata_first_id(id,main)).transpose()?.flatten();
        let salt=self.own_key_salt()?;
        Ok(crate::web_meta::drawer_from_loaded(inputs,crate::web_meta::DrawerFacts {files,main,first_id,salt},as_card))
    }
    pub(crate) fn own_nfo_text(&self,id:i64)->Result<Option<String>> {
        self.before_query()?;
        let extent=self.tx.borrow_mut().query_opt("select octet_length(text)::bigint from release_nfo where release_id=$1",&[&id])?;
        let Some(extent)=extent else{return Ok(None)};let bytes:i64=extent.try_get(0)?;
        ensure!(bytes>=0,"negative native NFO extent");self.admit_owned((bytes as u64).checked_mul(4).and_then(|n|n.checked_add(256)).context("native NFO extent overflow")?)?;
        self.before_query()?;
        Ok(Some(self.tx.borrow_mut().query_one("select text from release_nfo where release_id=$1",&[&id])?.try_get(0)?))
    }
    pub(crate) fn own_subtitles(&self,id:i64)->Result<Option<crate::probe::opensubtitles::Stored>> {
        use crate::probe::opensubtitles::{Stored,Feature};
        self.before_query()?;
        let extent=self.tx.borrow_mut().query_opt("select octet_length(moviehash)::bigint+coalesce(octet_length(kind),0)::bigint+
            coalesce(octet_length(title),0)::bigint+coalesce(octet_length(release_name),0)::bigint from release_opensubtitles where release_id=$1",&[&id])?;
        let Some(extent)=extent else{return Ok(None)};let bytes:i64=extent.try_get(0)?;
        ensure!(bytes>=0,"negative native subtitle extent");self.admit_owned((bytes as u64).checked_mul(4).and_then(|n|n.checked_add(512)).context("native subtitle extent overflow")?)?;
        self.before_query()?;
        let row=self.tx.borrow_mut().query_one("select moviehash,imdb_id,tmdb_id,kind,title,year,season,episode,release_name from release_opensubtitles where release_id=$1",&[&id])?;
        let imdb_id:Option<i64>=row.try_get(1)?;
        let feature=Feature {imdb_id,tmdb_id:row.try_get(2)?,kind:row.try_get(3)?,title:row.try_get(4)?,year:row.try_get(5)?,season:row.try_get(6)?,episode:row.try_get(7)?,release_name:row.try_get(8)?};
        Ok(Some(Stored {moviehash:row.try_get(0)?,feature:imdb_id.map(|_|feature)}))
    }
    pub(crate) fn own_key_salt(&self)->Result<Option<i64>> {
        self.before_query()?;
        Ok(self.tx.borrow_mut().query_opt("select value from meta where key='key_salt'",&[])?.map(|row|row.try_get::<_,i64>(0)).transpose()?)
    }
    pub(crate) fn own_names(&self,id:i64)->Result<Option<Names>> {
        self.before_query()?;
        let extent=self.tx.borrow_mut().query_opt("select octet_length(name)::bigint+coalesce(octet_length(display_name),0)::bigint from releases where id=$1",&[&id])?;
        let Some(extent)=extent else{return Ok(None)};let bytes:i64=extent.try_get(0)?;
        ensure!(bytes>=0,"negative native name extent");self.admit_owned((bytes as u64).checked_mul(3).and_then(|n|n.checked_add(256)).context("native name extent overflow")?)?;
        self.before_query()?;
        let row=self.tx.borrow_mut().query_one("select name,display_name,is_obfuscated,name_source from releases where id=$1",&[&id])?;
        Ok(Some(Names {posted:row.try_get(0)?,display:row.try_get(1)?,obfuscated:row.try_get(2)?,source:row.try_get(3)?}))
    }
    pub(crate) fn own_inner(&self,id:i64)->Result<InnerView> {
        self.before_query()?;
        let extent=self.tx.borrow_mut().query_one("select
            coalesce((select coalesce(octet_length(packer),0)::bigint+coalesce(octet_length(password_check),0)::bigint from archive_release where release_id=$1),0),
            coalesce((select sum(octet_length(name)::bigint)::bigint from(select name from archive_inner_files where release_id=$1 order by seq limit 256) listed),0)",&[&id])?;
        let bytes=extent.try_get::<_,i64>(0)?.checked_add(extent.try_get::<_,i64>(1)?).context("native inner extent overflow")?;
        ensure!(bytes>=0,"negative native inner extent");self.admit_owned((bytes as u64).checked_mul(4).and_then(|n|n.checked_add(32768)).context("native inner extent overflow")?)?;
        self.before_query()?;
        let row=self.tx.borrow_mut().query_opt("select password,encryption,names_hidden,password_check,packer from archive_release where release_id=$1",&[&id])?;
        let mut inner=InnerView::default();
        if let Some(row)=row {
            inner.password=Some(Password::from_code(row.try_get(0)?).context("invalid native archive password code")?);
            inner.encryption=row.try_get::<_,Option<i64>>(1)?.map(|code|Encryption::from_code(code).context("invalid native encryption code")).transpose()?;
            inner.names_hidden=row.try_get::<_,Option<i64>>(2)?.unwrap_or(0)!=0;
            inner.password_check=row.try_get::<_,Option<Vec<u8>>>(3)?.map(|bytes|PasswordTest::from_bytes(&bytes).context("invalid native archive password check")).transpose()?;
            inner.packer=row.try_get(4)?;
        }
        self.before_query()?;
        let rows=self.tx.borrow_mut().query("select seq,name,size,source from archive_inner_files where release_id=$1 order by seq limit 256",&[&id])?;
        inner.files=rows.into_iter().map(|row|Ok(InnerFileRow {seq:row.try_get(0)?,name:row.try_get(1)?,size:row.try_get(2)?,source:row.try_get(3)?})).collect::<Result<_>>()?;
        Ok(inner)
    }
    pub(crate) fn own_contents(&self,id:i64)->Result<Contents> {
        self.before_query()?;
        // Admission precedes decoding filenames. Unlike a historical page cap,
        // an exceeded request budget refuses the page; totals are never partial.
        let extent=self.tx.borrow_mut().query_one("select coalesce(sum(octet_length(filename)::bigint)::bigint,0),count(*) from files where release_id=$1",&[&id])?;
        let bytes:i64=extent.try_get(0)?;let count:i64=extent.try_get(1)?;
        ensure!(bytes>=0&&count>=0,"negative native contents extent");
        self.admit_owned((bytes as u64).checked_mul(4).and_then(|n|n.checked_add((count.min(MAX_LISTED_FILES as i64) as u64)*256+4096)).context("native contents extent overflow")?)?;
        self.before_query()?;let mut tx=self.tx.borrow_mut();
        let mut rows=tx.query_raw("select f.filename,f.distinct_parts,greatest(coalesce(f.expected,f.distinct_parts),f.distinct_parts),
            coalesce((select sum(s.bytes)::bigint from segments s where s.release_id=f.release_id and s.file_id=f.id),0)
            from files f where f.release_id=$1 order by f.filename collate \"C\",f.id",[&id])?;
        let mut contents=Contents::default();
        while let Some(row)=rows.next()? {
            self.check_deadline()?;
            let name:String=row.try_get(0)?;let seen:i64=row.try_get(1)?;let expected:i64=row.try_get(2)?;let bytes:i64=row.try_get(3)?;
            ensure!(seen>=0&&expected>=seen&&bytes>=0,"invalid native contents counters");
            let lower=name.to_ascii_lowercase();let par2=lower.ends_with(".par2");let recovery=par2&&lower.contains(".vol");
            add(&mut contents.files,1)?;add(&mut contents.articles_seen,seen)?;add(&mut contents.articles_expected,expected)?;
            if contents.list.len()<MAX_LISTED_FILES {contents.list.push(NzbFile {name,bytes,seen,expected});}
            if recovery {add(&mut contents.recovery_volumes,1)?;add(&mut contents.recovery_articles_seen,seen)?;add(&mut contents.recovery_articles_expected,expected)?;add(&mut contents.recovery_bytes,bytes)?;}
            else if !par2 {add(&mut contents.data_articles_seen,seen)?;add(&mut contents.data_articles_expected,expected)?;add(&mut contents.data_bytes,bytes)?;}
            if par2 {add(&mut contents.par2_files,1)?;add(&mut contents.recovery_blocks,crate::release_info::recovery_blocks(&lower))?;}
        }
        ensure!(contents.files==count,"native contents inventory changed within snapshot");drop(rows);drop(tx);
        contents.list.sort_by_cached_key(|file|{let lower=file.name.to_ascii_lowercase();
            let rank=if lower.ends_with(".par2"){3}else if lower.ends_with(".rar"){1}else if [".nfo",".sfv",".txt"].iter().any(|end|lower.ends_with(end)){0}else{2};(rank,lower)});
        self.before_query()?;
        if let Some(row)=self.tx.borrow_mut().query_opt("select par2_block,par2_blocks from release_fingerprint where release_id=$1",&[&id])? {
            let block:Option<i64>=row.try_get(0)?;let blocks:Option<i64>=row.try_get(1)?;
            if let (Some(block),Some(blocks))=(block,blocks) {contents.par2_block=Some(block);contents.par2_blocks=Some(blocks);}
        }
        Ok(contents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parser::{Article,Release},pg::{self,fresh_reader::Budget}};
    use std::time::{Instant,Duration};
    #[test]
    #[ignore="requires explicit isolated fresh schema9 read-only web admission"]
    fn own_release_contents_keep_same_part_copy_bytes_and_actual_archive_absence() {
        let schema=pg::testing::Schema::native_writer().expect("explicit fresh schema9");let mut client=schema.pool.get().unwrap();
        let articles=[("movie.rar",1,3,17),("movie.rar",1,3,19),("movie.rar",3,3,23),("movie.vol00+02.par2",1,1,11),("note.nfo",1,1,7)]
            .into_iter().enumerate().map(|(n,(file,part,total,bytes))|Article {filename:Some(file.into()),subject:format!("{file} yEnc ({part}/{total})"),message_id:format!("<part{n}@example.test>"),part:Some(part),total_parts:Some(total),bytes,..Default::default()}).collect();
        pg::release_writer::save(&mut client,&[Release {name:"Own.Film.Example".into(),poster:"poster".into(),group:"alt.binaries.test".into(),date:"2020-01-01".into(),articles,..Default::default()}],1).unwrap();
        let id:i64=client.query_one("select id from releases",&[]).unwrap().get(0);
        let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
        let names=view.own_names(id).unwrap().unwrap();assert_eq!(names.posted,"Own.Film.Example");
        assert_eq!(view.own_inner(id).unwrap(),InnerView::default());
        let drawer=view.own_drawer(id,false).unwrap().unwrap();
        assert!(drawer.starts_with("<tr class=\"drawer-row\"><td colspan="),"{drawer}");
        assert!(drawer.contains(&format!("/ui/release?id={id}")),"{drawer}");
        assert!(!drawer.contains("posting-account")&&!drawer.contains("NNTP-Posting-Host"));
        let card=view.own_drawer(id,true).unwrap().unwrap();assert!(card.starts_with("<div class=\"drawer-card\">"),"{card}");
        let contents=view.own_contents(id).unwrap();
        assert_eq!((contents.files,contents.articles_seen,contents.articles_expected),(3,4,5));
        assert_eq!((contents.data_bytes,contents.recovery_bytes,contents.recovery_blocks),(66,11,2));
        assert_eq!(contents.list.iter().map(|file|file.name.as_str()).collect::<Vec<_>>(),["note.nfo","movie.rar","movie.vol00+02.par2"]);
        assert_eq!(contents.list[1],NzbFile {name:"movie.rar".into(),bytes:59,seen:2,expected:3});
        drop(view);
        client.execute("insert into archive_release(release_id,password,encryption,names_hidden,packer) values($1,1,4,1,'zip')",&[&id]).unwrap();
        client.execute("insert into archive_inner_files(release_id,seq,name,size,source) values($1,0,'inside.mkv',1234,4)",&[&id]).unwrap();
        let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
        let inner=view.own_inner(id).unwrap();assert_eq!(inner.password,Some(Password::Yes));assert_eq!(inner.encryption,Some(Encryption::ZipCrypto));assert!(inner.names_hidden);
        assert_eq!(inner.files,[InnerFileRow {seq:0,name:"inside.mkv".into(),size:Some(1234),source:4}]);assert_eq!(inner.packer.as_deref(),Some("zip"));
    }
}
