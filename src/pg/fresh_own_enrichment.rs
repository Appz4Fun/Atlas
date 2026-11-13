//! Own password hints and recovery descriptions. Source rows are read in the
//! existing RR view; no adjacent post, fingerprint or provider supplies facts.
use super::fresh_reader::ReadView;
use anyhow::{Context,Result,ensure};
use postgres::fallible_iterator::FallibleIterator;
use crate::{archive::store::InnerView,release_info::{Names,PasswordHint,RecoveryOnly}};

enum Par2Names {SourceLabel,AllBound}
enum HintFiles {Scan,SourceLabel}
impl ReadView<'_> {
    fn password_files(&self,id:i64,scope:HintFiles)->Result<Vec<(String,Option<String>)>> {
        let limit=match scope {HintFiles::Scan=>64i64,HintFiles::SourceLabel=>256};
        self.before_query()?;
        let row=self.tx.borrow_mut().query_one("select count(*),coalesce(sum(octet_length(filename)::bigint+coalesce(octet_length(subject),0)::bigint)::bigint,0)
            from(select filename,subject from files where release_id=$1 order by id limit $2) own",&[&id,&limit])?;
        let count:i64=row.try_get(0)?;let bytes:i64=row.try_get(1)?;ensure!((0..=limit).contains(&count)&&bytes>=0,"invalid own password file inventory");
        self.admit_owned((bytes as u64).checked_mul(8).and_then(|n|n.checked_add(count as u64*1024+4096)).context("own password file extent overflow")?)?;
        self.before_query()?;let rows=self.tx.borrow_mut().query("select filename,subject from files where release_id=$1 order by id limit $2",&[&id,&limit])?;
        ensure!(rows.len()==count as usize,"own password file inventory changed");
        rows.into_iter().map(|row|->Result<(String,Option<String>)>{Ok((row.try_get(0)?,row.try_get(1)?))}).collect()
    }
    fn password_par2_names(&self,id:i64,scope:Par2Names)->Result<Vec<String>> {
        let (bound,suffix,limit)=match scope {Par2Names::SourceLabel=>("","order by file_id limit 256",256),Par2Names::AllBound=>("and bound=1","order by file_id",5000)};
        self.before_query()?;
        let row=self.tx.borrow_mut().query_one(&format!("select count(*),coalesce(sum(octet_length(name)::bigint)::bigint,0) from(select name from par2_files where release_id=$1 {bound} {suffix}) own"),&[&id])?;
        let count:i64=row.try_get(0)?;let bytes:i64=row.try_get(1)?;ensure!((0..=limit).contains(&count)&&bytes>=0,"own PAR2 name inventory exceeds admission");
        self.admit_owned((bytes as u64).checked_mul(8).and_then(|n|n.checked_add(count as u64*256+4096)).context("own PAR2 name extent overflow")?)?;
        self.before_query()?;let rows=self.tx.borrow_mut().query(&format!("select name from par2_files where release_id=$1 {bound} {suffix}"),&[&id])?;
        ensure!(rows.len()==count as usize,"own PAR2 name inventory changed");rows.into_iter().map(|row|->Result<String>{Ok(row.try_get(0)?)}).collect()
    }
    fn password_parser_workspace<'a>(&self,texts:impl IntoIterator<Item=&'a str>)->Result<()> {
        // The existing harvester keeps candidate strings before verification.
        // Reserve that workspace before any regex/harvester invocation.
        let bytes=texts.into_iter().try_fold(4096u64,|sum,text|sum.checked_add((text.len() as u64).checked_mul(64).context("own password parser extent overflow")?).context("own password parser extent overflow"))?;
        self.admit_owned(bytes)?;self.check_deadline()
    }
    fn own_password_source(&self,id:i64,password:&str,names:&Names,inner:&InnerView,nfo:Option<&str>)->Result<&'static str> {
        const ELSEWHERE:&str="another copy of this post, or a posted NZB";
        if password.is_empty(){return Ok(ELSEWHERE);}
        if names.posted.contains(password){return Ok(if names.display.is_some(){"the original article name"}else{"the release name"});}
        if names.display.as_ref().is_some_and(|name|name.contains(password)){return Ok("the release name");}
        let files=self.password_files(id,HintFiles::SourceLabel)?;
        if files.iter().any(|(name,_)|name.contains(password)){return Ok("a file name");}
        if files.iter().any(|(_,subject)|subject.as_ref().is_some_and(|subject|subject.contains(password))){return Ok("the article subject");}
        if self.password_par2_names(id,Par2Names::SourceLabel)?.iter().any(|name|name.contains(password)){return Ok("a file name in the PAR2 set");}
        if inner.files.iter().any(|file|file.name.contains(password)){return Ok("a file inside");}
        Ok(if nfo.is_some_and(|text|text.contains(password)){"the NFO"}else{ELSEWHERE})
    }
    pub(crate) fn own_password_hint(&self,id:i64,names:&Names,inner:&InnerView,nfo:Option<&str>)->Result<Option<PasswordHint>> {
        self.before_query()?;
        let extent=self.tx.borrow_mut().query_opt("select octet_length(password)::bigint from release_nfo_password where release_id=$1",&[&id])?;
        if let Some(extent)=extent {
            let bytes:i64=extent.try_get(0)?;ensure!(bytes>=0,"negative own password extent");
            self.admit_owned((bytes as u64).checked_mul(16).and_then(|n|n.checked_add(256)).context("own password extent overflow")?)?;self.before_query()?;
            let password:String=self.tx.borrow_mut().query_one("select password from release_nfo_password where release_id=$1",&[&id])?.try_get(0)?;
            let source=self.own_password_source(id,&password,names,inner,nfo)?;return Ok(Some(PasswordHint {password,source}));
        }
        let own_names=std::iter::once(names.posted.as_str()).chain(names.display.as_deref());
        self.password_parser_workspace(own_names.clone())?;
        for name in own_names {if let Some(password)=crate::release_info::find_password(name){return Ok(Some(PasswordHint {password,source:"the release name"}));}}
        let files=self.password_files(id,HintFiles::Scan)?;self.password_parser_workspace(files.iter().take(64).flat_map(|(name,subject)|std::iter::once(name.as_str()).chain(subject.as_deref())))?;
        for (name,subject) in files.iter().take(64) {
            if let Some(password)=crate::release_info::find_password(name){return Ok(Some(PasswordHint {password,source:"a file name"}));}
            if let Some(password)=subject.as_deref().and_then(crate::release_info::find_password){return Ok(Some(PasswordHint {password,source:"the subject"}));}
        }
        self.password_parser_workspace(inner.files.iter().map(|file|file.name.as_str()))?;
        for file in &inner.files {if let Some(password)=crate::release_info::find_password(&file.name){return Ok(Some(PasswordHint {password,source:"a file inside"}));}}
        let texts=std::iter::once(names.posted.as_str()).chain(names.display.as_deref()).chain(files.iter().take(64).filter_map(|(_,subject)|subject.as_deref())).collect::<Vec<_>>();
        for text in &texts {self.check_deadline()?;if let Some(password)=crate::release_info::harvested_password(text){return Ok(Some(PasswordHint {password,source:"the subject"}));}}
        if let Some(check)=inner.password_check.as_ref() {
            self.password_parser_workspace(nfo)?;
            if let Some(password)=crate::release_info::find_verified_password(texts.iter().copied().chain(nfo),&[],Some(check)) {
                let source=self.own_password_source(id,&password,names,inner,nfo)?;return Ok(Some(PasswordHint {password,source}));
            }
        }
        let par2=self.password_par2_names(id,Par2Names::AllBound)?;
        let Some(stem)=crate::duplicates::par2_password_stem(par2.iter().map(String::as_str)) else{return Ok(None)};
        // A positive renamed-set hint needs every posted filename. Scan them
        // in one reused workspace; a first-256 prefix cannot establish it.
        self.before_query()?;let extent=self.tx.borrow_mut().query_one("select coalesce(max(octet_length(filename)),0)::bigint from files where release_id=$1",&[&id])?;
        let bytes:i64=extent.try_get(0)?;ensure!(bytes>=0,"negative own posted filename extent");
        self.admit_owned((bytes as u64).checked_mul(8).and_then(|n|n.checked_add(4096)).context("own posted filename extent overflow")?)?;
        self.before_query()?;let mut tx=self.tx.borrow_mut();let mut rows=tx.query_raw("select filename from files where release_id=$1 order by id",[&id])?;let mut seen=false;
        while let Some(row)=rows.next()? {
            self.check_deadline()?;let name:String=row.try_get(0)?;ensure!(name.len() as i64<=bytes,"own posted filename extent changed");seen=true;
            if crate::duplicates::volume_set(&name).eq_ignore_ascii_case(stem){return Ok(None);}
        }
        Ok(seen.then(||PasswordHint {password:stem.to_owned(),source:"the PAR2 set's file names"}))
    }

    pub(crate) fn own_recovery(&self,id:i64)->Result<Option<RecoveryOnly>> {
        self.before_query()?;let extent=self.tx.borrow_mut().query_one("select count(*),coalesce(sum(octet_length(filename)::bigint)::bigint,0) from(select filename from files where release_id=$1 order by id limit 65) own",&[&id])?;
        let count:i64=extent.try_get(0)?;let bytes:i64=extent.try_get(1)?;ensure!((0..=65).contains(&count)&&bytes>=0,"invalid own recovery inventory");
        if count==0||count>64{return Ok(None);}
        self.admit_owned((bytes as u64).checked_mul(8).and_then(|n|n.checked_add(count as u64*256+2048)).context("own recovery name extent overflow")?)?;
        self.before_query()?;let names=self.tx.borrow_mut().query("select filename from files where release_id=$1 order by id limit 65",&[&id])?.into_iter().map(|row|row.try_get(0)).collect::<std::result::Result<Vec<String>,_>>()?;
        ensure!(names.len()==count as usize,"own recovery inventory changed");
        if count>1&&!names.iter().all(|name|name.to_ascii_lowercase().ends_with(".par2")){return Ok(None);}
        self.before_query()?;let extent=self.tx.borrow_mut().query_one("select count(*),coalesce(sum(octet_length(name)::bigint)::bigint,0) from archive_inner_files where release_id=$1 and source=$2",&[&id,&crate::archive::store::SOURCE_PAR2])?;
        let rows:i64=extent.try_get(0)?;let bytes:i64=extent.try_get(1)?;ensure!((0..=5000).contains(&rows)&&bytes>=0,"own recovery set exceeds admission");
        self.admit_owned((bytes as u64).checked_mul(8).and_then(|n|n.checked_add(rows as u64*256+2048)).context("own recovery set extent overflow")?)?;
        self.before_query()?;let repairs=self.tx.borrow_mut().query("select name,size from archive_inner_files where release_id=$1 and source=$2 order by seq",&[&id,&crate::archive::store::SOURCE_PAR2])?.into_iter().map(|row|->Result<(String,Option<i64>)>{
            let size:Option<i64>=row.try_get(1)?;ensure!(size.is_none_or(|size|size>=0),"invalid own recovery file size");Ok((row.try_get(0)?,size))
        }).collect::<Result<Vec<_>>>()?;
        ensure!(repairs.len()==rows as usize,"own recovery set inventory changed");
        // RecoveryOnly's legacy renderer saturates; native facts must never
        // present an overflowing sum as an exact byte total. NULL stays unknown.
        repairs.iter().try_fold(0i64,|sum,(_,size)|sum.checked_add(size.unwrap_or(0)).context("own recovery byte total overflows"))?;
        Ok(crate::release_info::recovery_only_from_facts(&names,repairs))
    }
}
