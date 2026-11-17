//! Checked Step reads through the current canonical owner. No SQLite or
//! independent cache connection can satisfy a native Step context.

use anyhow::{Result,ensure};
use postgres::Transaction;
use std::sync::Arc;

use super::indexer_runtime::Owner;
#[path="stage_links.rs"]
mod links;
#[path="stage_keys.rs"]
mod keys;
pub(crate) use keys::{KeysRead,KeysSnapshot};
#[path="stage_inventory.rs"]
mod inventory;
#[path="stage_xref.rs"]
pub(crate) mod xref;
#[path="../spotnet/native_step_reads.rs"]
mod spotnet;
#[path="stage_extra_reads.rs"]
mod extra;

/// Attempt-bound read access. Construction requires the canonical writer;
/// this value cannot reconnect or grant a Step mutation authority.
pub struct StepReads {
    owner:Arc<Owner>,
    epoch:[u8;32],
    targets:std::sync::Mutex<super::stage_queue::participants::ReadTargets>,
    /// Where this schema keeps its articles when not in PostgreSQL, once a read has found it.
    local:std::sync::OnceLock<Arc<crate::article_local::Store>>,
}

pub(crate) struct StepSnapshot {
    pub(crate) info:crate::postprocess::step::ReleaseInfo,
    pub(crate) summary:crate::postprocess::rules::FileSummary,
    pub(crate) has_state:bool,pub(crate) done:i64,pub(crate) failed:i64,
    pub(crate) db_attempts:i64,pub(crate) net_attempts:i64,pub(crate) parts_seen:i64,
    pub(crate) rerun_gen:i64,pub(crate) net_transient:i64,
    pub(crate) net_left:Option<i64>,
    pub(crate) input:[u8;32],
}

impl StepReads {
    pub(crate) fn for_owner(owner:&Arc<Owner>)->Result<Self> {
        ensure!(tokio::runtime::Handle::try_current().is_err(),"native Step reads require a standard worker thread");
        let epoch=*owner.epoch().ok_or_else(||anyhow::anyhow!("native Step reads require the canonical owner"))?.id();
        Ok(Self {owner:owner.clone(),epoch,targets:std::sync::Mutex::new(super::stage_queue::participants::ReadTargets::for_owner(owner)?),local:std::sync::OnceLock::new()})
    }
    /// The local article store of this owner's schema, if the process opened
    /// one. Found inside a read that already holds the owner, so construction
    /// never queues on it; no query at all while no store is open.
    fn local_in(&self,tx:&mut Transaction<'_>)->Result<Option<Arc<crate::article_local::Store>>> {
        if let Some(store)=self.local.get() {return Ok(Some(store.clone()))}
        Ok(crate::article_local::of(tx)?.map(|store|self.local.get_or_init(||store).clone()))
    }
    fn observe_in(&self,tx:&mut Transaction<'_>,ids:&[i64])->Result<()> {
        self.targets.lock().map_err(|_|anyhow::anyhow!("native participant observations are poisoned"))?.observe_in(tx,ids)
    }
    pub(crate) fn observed(&self)->Result<super::stage_queue::participants::Observed> {
        Ok(self.targets.lock().map_err(|_|anyhow::anyhow!("native participant observations are poisoned"))?.snapshot())
    }

    fn read<T>(&self,work:impl FnOnce(&mut Transaction<'_>)->Result<T>)->Result<T> {
        ensure!(tokio::runtime::Handle::try_current().is_err(),"native Step reads require a standard worker thread");
        ensure!(self.owner.epoch().is_some_and(|epoch|epoch.id()==&self.epoch),"native Step read epoch changed");
        // On a reader session beside the owner when one is configured (`Owner::read_only`), which proves the epoch.
        self.owner.read_only(|tx| {
            // a read past its statement timeout fails its step, not the session (the transaction rolls back on drop)
            work(tx).map_err(|error| if super::outage::is_statement_timeout(&error) {
                error.context(super::outage::StepTimedOut)
            } else {error})
        })
    }

    fn text(&self,sql:&str,id:i64)->Result<Option<String>> {
        self.read(|tx| {
            let row=tx.query_opt(sql,&[&id])?;
            row.map(|row| {
                let size:Option<i32>=row.try_get(1)?;
                ensure!(size.is_none_or(|n|(0..=64*1024*1024).contains(&n)),"native Step text exceeds read admission");
                row.try_get(0).map_err(Into::into)
            }).transpose().map(Option::flatten)
        })
    }

    pub(crate) fn probe_moviehash(&self,id:i64)->Result<Option<String>> {
        self.text("select case when octet_length(moviehash)<=67108864 then moviehash end,octet_length(moviehash) from release_probe where release_id=$1",id)
    }
    pub(crate) fn probe_info(&self,id:i64)->Result<Option<String>> {
        self.text("select case when octet_length(info)<=67108864 then info end,octet_length(info) from release_probe where release_id=$1",id)
    }
    pub(crate) fn opensubtitles_asked(&self,id:i64,hash:&str)->Result<bool> {
        self.read(|tx|Ok(tx.query_one("select exists(select 1 from release_opensubtitles where release_id=$1 and moviehash=$2)",&[&id,&hash])?.try_get(0)?))
    }
    pub(crate) fn opensubtitles_movie_imdb(&self,id:i64)->Result<Option<i64>> {
        self.read(|tx|Ok(tx.query_opt("select imdb_id from release_opensubtitles where release_id=$1 and imdb_id is not null and lower(kind)='movie'",&[&id])?.map(|row|row.try_get(0)).transpose()?))
    }
    pub(crate) fn category_exists(&self,id:i64)->Result<bool> {
        self.read(|tx|Ok(tx.query_one("select exists(select 1 from release_category where release_id=$1)",&[&id])?.try_get(0)?))
    }
    pub(crate) fn category_spot(&self,id:i64)->Result<Option<(crate::spotnet::Kind,crate::spotnet::Subcats)>> {
        Ok(self.spot_detail(id)?.and_then(|detail|detail.spot.kind().map(|kind|(kind,detail.spot.subcats))))
    }

    pub(crate) fn explicit_rerun(&self,id:i64,step:crate::postprocess::step::StepId)->Result<bool> {
        self.read(|tx|Ok(tx.query_one("select coalesce((select rerun_mask from pp_state where release_id=$1),0)&$2<>0",&[&id,&crate::postprocess::step::mask(step)])?.try_get(0)?))
    }
    pub(crate) fn content_id(&self,id:i64)->Result<Option<Vec<u8>>> {
        self.read(|tx|{let local=self.local_in(tx)?;super::content_id::read_with(tx,id,&self.owner.stop,local.as_deref())})
    }
    pub(crate) fn snapshot(&self,id:i64)->Result<Option<StepSnapshot>> {
        self.read(|tx| {
            let Some(metadata)=tx.query_opt("select coalesce(octet_length(original_name),0)::bigint+coalesce(octet_length(display_name),0)::bigint+octet_length(group_name)::bigint+coalesce(octet_length(poster),0)::bigint+coalesce(octet_length(posted_date),0)::bigint from releases where id=$1",&[&id])? else {return Ok(None)};
            ensure!(metadata.try_get::<_,i64>(0)?<=64*1024*1024,"native release exceeds snapshot admission");
            let row=tx.query_one("select r.original_name,r.display_name,r.group_name,r.poster,r.posted_date,coalesce(r.size,0),r.complete,coalesce(r.parts,0),r.file_total,r.is_obfuscated,r.name_source,
                s.release_id is not null,coalesce(s.done,0),coalesce(s.failed,0),coalesce(s.db_attempts,0),coalesce(s.net_attempts,0),coalesce(s.parts_seen,0),coalesce(s.rerun_gen,0),coalesce(s.net_transient,0),
                s.net_left from releases r left join pp_state s on s.release_id=r.id where r.id=$1",&[&id])?;
            let original_kind=self.original_spot_in(tx,id)?.and_then(|detail|detail.spot.kind());
            let group:String=row.try_get(2)?;
            let info=crate::postprocess::step::ReleaseInfo {id,
                // Existing step data includes a group fairness bucket. It is
                // never a SQLite attachment or an encoding of this PG row ID.
                shard:crate::catalog_facts::shard_of(&group),name:String::from_utf8(row.try_get(0)?)?,display_name:row.try_get(1)?,group,
                poster:row.try_get(3)?,posted_date:row.try_get::<_,Option<String>>(4)?.map(|value|crate::dates::to_iso_date(&value)),size:row.try_get(5)?,complete:row.try_get(6)?,parts:row.try_get(7)?,file_total:row.try_get(8)?,is_obfuscated:row.try_get(9)?,name_source:row.try_get(10)?,
                spot_kind:original_kind};
            let mut summary=crate::postprocess::rules::FileSummary {all_par2:true,all_images:true,..Default::default()};
            let mut after=None;
            loop {
                if self.owner.stop.load(std::sync::atomic::Ordering::Acquire) {return Err(super::outage::AttemptEnded.into());}
                // Stream every file. A horizon would misjudge large albums.
                // Only the first file's subject contributes to these rules.
                let files=tx.query("select id,case when octet_length(original_filename)<=65536 then original_filename end,octet_length(original_filename),subject_part,expected,distinct_parts,touched_at,
                    case when id=(select min(id) from files where release_id=$1) and octet_length(subject)<=67108864 then subject end,
                    case when id=(select min(id) from files where release_id=$1) then octet_length(subject) end
                    from files where release_id=$1 and ($2::bigint is null or id>$2) order by id limit 64",&[&id,&after])?;
                if files.is_empty() {break;}
                for file in files {
                    ensure!(file.try_get::<_,Option<i32>>(2)?.is_some_and(|n|n<=65536),"native file lacks admitted original name");
                    ensure!(file.try_get::<_,Option<i32>>(8)?.is_none_or(|n|n<=64*1024*1024),"native single subject exceeds snapshot admission");
                    let filename=String::from_utf8(file.try_get(1)?)?;let expected:Option<i64>=file.try_get(4)?;let seen:i64=file.try_get(5)?;
                    ensure!(seen>=0,"native file part count is negative");
                    summary.files=summary.files.checked_add(1).ok_or_else(||anyhow::anyhow!("native file count overflow"))?;
                    summary.last_touch=summary.last_touch.max(file.try_get::<_,Option<i64>>(6)?.unwrap_or(0));
                    let (present,wanted)=match expected {Some(n) if n>0=>(seen.min(n),n),_=>(1,1)};
                    summary.present=summary.present.checked_add(present).ok_or_else(||anyhow::anyhow!("native present part count overflow"))?;
                    summary.wanted=summary.wanted.checked_add(wanted).ok_or_else(||anyhow::anyhow!("native expected part count overflow"))?;
                    summary.all_par2&=crate::postprocess::rules::PAR2.is_match(&filename);summary.all_images&=crate::postprocess::rules::IMAGE.is_match(&filename);
                    summary.executable|=crate::junk::is_executable(&filename);summary.has_video|=crate::junk::is_video(&filename);
                    if summary.files==1 {summary.single_part=file.try_get(3)?;summary.single_expected=expected;summary.single_subject=file.try_get(7)?;}
                    else {summary.single_subject=None;}
                    after=Some(file.try_get::<_,i64>(0)?);
                }
            }
            let mut snapshot=StepSnapshot {info,summary,has_state:row.try_get(11)?,done:row.try_get(12)?,failed:row.try_get(13)?,db_attempts:row.try_get(14)?,net_attempts:row.try_get(15)?,parts_seen:row.try_get(16)?,rerun_gen:row.try_get(17)?,net_transient:row.try_get(18)?,net_left:row.try_get(19)?,input:[0;32]};
            use sha2::{Digest,Sha256};
            // Fixed tuple encoding retains NULL versus empty, exact original
            // strings and every value used by the actual rule/Step visit.
            let encoded=serde_json::to_vec(&(
                (&snapshot.info.name,&snapshot.info.display_name,&snapshot.info.group,&snapshot.info.poster,&snapshot.info.posted_date,snapshot.info.size,snapshot.info.complete,snapshot.info.parts,snapshot.info.file_total,snapshot.info.is_obfuscated,snapshot.info.name_source,snapshot.info.spot_kind.map(|kind|match kind {crate::spotnet::Kind::Movie=>1,crate::spotnet::Kind::Series=>2})),
                (snapshot.summary.files,snapshot.summary.last_touch,snapshot.summary.present,snapshot.summary.wanted,snapshot.summary.all_par2,snapshot.summary.all_images,snapshot.summary.executable,snapshot.summary.has_video,snapshot.summary.single_part,snapshot.summary.single_expected,&snapshot.summary.single_subject),
                (id,snapshot.has_state,snapshot.done,snapshot.failed,snapshot.db_attempts,snapshot.net_attempts,snapshot.parts_seen,snapshot.rerun_gen,snapshot.net_transient,snapshot.net_left)))?;
            let mut hash=Sha256::new();hash.update(b"atlas-native-step-input-v1");hash.update(encoded);snapshot.input=hash.finalize().into();
            Ok(Some(snapshot))
        })
    }
    fn optional<T:postgres::types::FromSqlOwned>(&self,sql:&str,id:i64)->Result<Option<T>> {
        self.read(|tx|Ok(tx.query_opt(sql,&[&id])?.map(|row|row.try_get(0)).transpose()?))
    }
    fn scalar<T:postgres::types::FromSqlOwned>(&self,sql:&str,id:i64)->Result<T> {
        self.read(|tx|Ok(tx.query_one(sql,&[&id])?.try_get(0)?))
    }
    pub(crate) fn sidecar_text_bytes(&self)->Result<i64> {
        self.scalar("select coalesce((select value from meta where key='sidecar_text_bytes'),0) where $1::bigint=0",0)
    }
    pub(crate) fn archive_total_name_bytes(&self)->Result<i64> {
        self.scalar("select coalesce((select value from meta where key='archive_name_bytes'),0) where $1::bigint=0",0)
    }
    pub(crate) fn archive_source_name_bytes(&self,id:i64,source:i64)->Result<i64> {
        self.read(|tx|Ok(tx.query_one("select coalesce(sum(octet_length(name)+32),0)::bigint from archive_inner_files where release_id=$1 and source=$2",&[&id,&source])?.try_get(0)?))
    }
    pub(crate) fn archive_source_exists(&self,id:i64,source:i64)->Result<bool> {
        self.read(|tx|Ok(tx.query_one("select exists(select 1 from archive_inner_files where release_id=$1 and source=$2)",&[&id,&source])?.try_get(0)?))
    }
    pub(crate) fn archive_inner_files(&self,id:i64)->Result<Vec<crate::archive::store::InnerFileRow>> {
        self.read(|tx| {
            // Match the legacy sequence order and horizon. Inspect lengths in
            // the same snapshot before transferring any potentially large name.
            let bytes:i64=tx.query_one("select coalesce(sum(bytes),0)::bigint from (select octet_length(name)::bigint as bytes from archive_inner_files where release_id=$1 order by seq limit 256) selected",&[&id])?.try_get(0)?;
            ensure!(bytes<=64*1024*1024,"native archive inner files exceed read admission");
            tx.query("select seq,name,size,source from archive_inner_files where release_id=$1 order by seq limit 256",&[&id])?.into_iter().map(|row|Ok(crate::archive::store::InnerFileRow {
                seq:row.try_get(0)?,name:row.try_get(1)?,size:row.try_get(2)?,source:row.try_get(3)?,
            })).collect()
        })
    }
    pub(crate) fn archive_password(&self,id:i64)->Result<Option<crate::archive::Password>> {
        Ok(self.optional::<i64>("select password from archive_release where release_id=$1",id)?.and_then(crate::archive::Password::from_code))
    }
    pub(crate) fn nfo_password(&self,id:i64)->Result<Option<String>> {
        self.text("select case when octet_length(password)<=67108864 then password end,octet_length(password) from release_nfo_password where release_id=$1",id)
    }
    pub(crate) fn local_password_check(&self,id:i64)->Result<Option<crate::archive::PasswordTest>> {
        self.read(|tx| {
            let Some(row)=tx.query_opt("select case when octet_length(password_check)<=65536 then password_check end,octet_length(password_check) from archive_release where release_id=$1",&[&id])? else {return Ok(None)};
            ensure!(row.try_get::<_,Option<i32>>(1)?.is_none_or(|n|n<=65536),"native password check exceeds read admission");
            Ok(row.try_get::<_,Option<Vec<u8>>>(0)?.as_deref().and_then(crate::archive::PasswordTest::from_bytes))
        })
    }
    pub(crate) fn stored_nfo(&self,id:i64)->Result<Option<crate::probe::nfo_store::StoredNfo>> {
        if let Some(info)=self.probe_info(id)? {
            let info=super::probe_decode::decode(info.as_bytes(),&mut (32u64<<20))?;
            if let (Some(text),Some(file))=(info.nfo,info.nfo_file) {return Ok(Some(crate::probe::nfo_store::StoredNfo {file,text}));}
        }
        self.read(|tx| {
            let Some(row)=tx.query_opt("select case when octet_length(file)+octet_length(text)<=67108864 then file end,case when octet_length(file)+octet_length(text)<=67108864 then text end,octet_length(file)::bigint+octet_length(text)::bigint from release_nfo where release_id=$1",&[&id])? else {return Ok(None)};
            ensure!(row.try_get::<_,i64>(2)?<=64*1024*1024,"native NFO exceeds read admission");
            Ok(Some(crate::probe::nfo_store::StoredNfo {file:row.try_get(0)?,text:row.try_get(1)?}))
        })
    }
    pub(crate) fn rename_named(&self,group:&str)->Result<bool> {
        self.read(|tx|Ok(tx.query_one("select exists(select 1 from meta where key in ('naming_rename_named',$1))",&[&crate::postprocess::reprocess::rename_named_key(group)])?.try_get(0)?))
    }
    pub(crate) fn media_imdb_marker(&self,id:i64)->Result<Option<Option<i64>>> {
        self.optional("select imdb_id from media_release where release_id=$1",id)
    }
    pub(crate) fn media_exists(&self,id:i64)->Result<bool> {
        self.scalar("select exists(select 1 from media_release where release_id=$1)",id)
    }
    pub(crate) fn saved_probe_media(&self,id:i64)->Result<Option<crate::probe::media::MediaInfo>> {
        self.probe_info(id)?.map(|text|super::probe_decode::decode(text.as_bytes(),&mut (32u64<<20))).transpose().map(|info|info.and_then(|info|info.media))
    }
    pub(crate) fn stale_media_ids(&self,key:i64)->Result<bool> {
        self.scalar("select exists(select 1 from (select tvdb_id,tvmaze_id,trakt_id,tmdb_id,imdb_id,tvrage_id,imdb_from_nfo,tmdb_from_tag,tvdb_from_tag from media_release where title_key=$1 order by release_id desc limit 100) recent where (tvdb_id is not null and tvdb_from_tag=0) or tvmaze_id is not null or trakt_id is not null or (tmdb_id is not null and tmdb_from_tag=0) or tvrage_id is not null or (imdb_id is not null and imdb_from_nfo=0))",key)
    }
    pub(crate) fn predb_find(&self,evidence:&crate::predb::matcher::Evidence<'_>)->Result<super::predb_cache::LookupStatus> {
        self.read(|tx|super::predb_cache::find_in(tx,evidence))
    }
    pub(crate) fn prior_predb_match(&self,id:i64)->Result<Option<i64>> {
        self.optional("select title_id from predb_match where release_id=$1",id)
    }
    pub(crate) fn has_audio_print(&self,id:i64)->Result<bool> {
        self.scalar("select exists(select 1 from release_audio_print where release_id=$1)",id)
    }
    pub(crate) fn upload_lead(&self,id:i64)->Result<Option<i64>> {
        self.read(|tx| {
            let value=tx.query_opt("select lead_id from upload_link where release_id=$1",&[&id])?.map(|row|row.try_get(0)).transpose()?;
            if let Some(lead)=value {self.observe_in(tx,&[id,lead])?;}Ok(value)
        })
    }
    pub(crate) fn upload_tried_count(&self,id:i64)->Result<i64> {
        self.scalar("select count(*) from upload_link_tried where release_id=$1",id)
    }
    pub(crate) fn release_size(&self,id:i64)->Result<Option<i64>> {
        self.read(|tx| {
            let value=tx.query_opt("select coalesce(size,0) from releases where id=$1",&[&id])?.map(|row|row.try_get(0)).transpose()?;
            if value.is_some() {self.observe_in(tx,&[id])?;}Ok(value)
        })
    }
    pub(crate) fn upload_is_lead(&self,id:i64)->Result<bool> {
        self.read(|tx| {
            let value=tx.query_one("select exists(select 1 from upload_link where release_id=$1 and lead_id=$1)",&[&id])?.try_get(0)?;
            if value {self.observe_in(tx,&[id])?;}Ok(value)
        })
    }
    pub(crate) fn release_name_marker(&self,id:i64)->Result<Option<Option<String>>> {
        self.read(|tx| {
            let Some(row)=tx.query_opt("select case when octet_length(display_name)<=67108864 then display_name end,octet_length(display_name) from releases where id=$1",&[&id])? else {return Ok(None)};
            ensure!(row.try_get::<_,Option<i32>>(1)?.is_none_or(|n|n<=64*1024*1024),"native name exceeds read admission");
            Ok(Some(row.try_get(0)?))
        })
    }
    pub(crate) fn file_clock(&self,id:i64)->Result<Option<crate::archive::link::Clock>> {
        self.read(|tx|Ok(tx.query_opt("select first_date,last_date,clock_first,clock_last from file_timing where file_id=$1",&[&id])?.map(|row|Ok::<_,anyhow::Error>(crate::archive::link::Clock::of(row.try_get(0)?,row.try_get(1)?,row.try_get(2)?,row.try_get(3)?))).transpose()?.flatten()))
    }
    pub(crate) fn finished_step_bits(&self,id:i64,mask:crate::postprocess::step::StepMask)->Result<crate::postprocess::step::StepMask> {
        self.read(|tx|Ok(tx.query_one("select coalesce((select done|failed from pp_state where release_id=$1),0)&$2",&[&id,&mask])?.try_get(0)?))
    }
    pub(crate) fn beside_owner_local(&self,id:i64)->Result<Option<i64>> {
        self.read(|tx| {
            let value=tx.query_opt("select release_id from beside_files where row_id=$1",&[&id])?.map(|row|row.try_get(0)).transpose()?;
            if let Some(owner)=value {self.observe_in(tx,&[id,owner])?;}Ok(value)
        })
    }
    pub(crate) fn legacy_step_key_salt(&self)->Result<Option<i64>> {
        self.optional("select value from meta where key='key_salt' and $1::bigint=0",0)
    }
    pub(crate) fn files(&self,id:i64,limit:usize)->Result<Vec<crate::catalog_facts::FileInfo>> {
        ensure!(limit<=5000,"native Step file limit exceeds read admission");
        self.read(|tx| {
            let metadata=tx.query("select id,coalesce(octet_length(original_filename),0)::bigint+coalesce(octet_length(subject),0)::bigint from files where release_id=$1 order by id limit $2",&[&id,&(limit as i64)])?;
            let mut ids=Vec::with_capacity(metadata.len());let mut bytes=0i64;
            for row in metadata {ids.push(row.try_get::<_,i64>(0)?);bytes=bytes.checked_add(row.try_get::<_,i64>(1)?).ok_or_else(||anyhow::anyhow!("native file byte count overflow"))?;}
            ensure!(bytes<=64*1024*1024,"native Step file inventory exceeds read admission");
            tx.query("select id,original_filename,subject,subject_part,expected,file_total,distinct_parts,touched_at from files where release_id=$1 and id=any($2) order by id",&[&id,&ids])?.into_iter().map(|row|Ok(crate::catalog_facts::FileInfo {
                id:row.try_get(0)?,filename:String::from_utf8(row.try_get(1)?)?,subject:row.try_get(2)?,subject_part:row.try_get(3)?,expected:row.try_get(4)?,file_total:row.try_get(5)?,parts_seen:row.try_get(6)?,touched_at:row.try_get(7)?,sealed:false,
            })).collect()
        })
    }
    pub(crate) fn segments(&self,id:i64,pick:&crate::catalog_facts::SegmentPick)->Result<Vec<crate::catalog_facts::SegmentRef>> {
        use crate::catalog_facts::SegmentPick;
        self.read(|tx| {
            if let Some(store)=self.local_in(tx)? {
                // No key index locally: the picked rows come back in one step,
                // under the same selection limits and message-ID byte admission.
                const ID_BYTES:i64=64*1024*1024;
                let picked=match pick {
                    SegmentPick::First(n)|SegmentPick::Last(n)=>{
                        ensure!(*n<=65536,"native Step segment selection exceeds read admission");
                        store.file_ends(id,matches!(pick,SegmentPick::Last(_)),*n,ID_BYTES)?
                    },
                    SegmentPick::Parts(parts)=>{
                        ensure!(parts.len()<=4096,"native Step part selection exceeds read admission");
                        store.file_part_copies(id,parts,16,ID_BYTES)?
                    },
                };
                let Some(picked)=picked else {anyhow::bail!("native Step segment inventory exceeds read admission")};
                return picked.into_iter().map(|row|Ok(crate::catalog_facts::SegmentRef {
                    message_id:String::from_utf8(row.message_id)?,part:row.part,bytes:row.bytes,
                })).collect();
            }
            let (sql,limit,parts)=match pick {
                SegmentPick::First(n)=>("select message_key,octet_length(message_id) from segments where file_id=$1 order by part nulls first,message_id limit $2",*n,None),
                SegmentPick::Last(n)=>("select message_key,octet_length(message_id) from segments where file_id=$1 order by part desc nulls last,message_id desc limit $2",*n,None),
                SegmentPick::Parts(parts)=>{
                    ensure!(parts.len()<=4096,"native Step part selection exceeds read admission");
                    ("select message_key,octet_length(message_id) from (select message_key,message_id,part,row_number() over(partition by part order by message_id) as n from segments where file_id=$1 and part=any($2)) picked where n<=16 order by part,message_id",parts.len()*16,Some(parts))
                },
            };
            ensure!(limit<=65536,"native Step segment selection exceeds read admission");
            let metadata=if let Some(parts)=parts {tx.query(sql,&[&id,parts])?} else {tx.query(sql,&[&id,&(limit as i64)])?};
            let mut keys:Vec<Vec<u8>>=Vec::with_capacity(metadata.len());let mut bytes=0i64;
            for row in metadata {keys.push(row.try_get(0)?);bytes=bytes.checked_add(i64::from(row.try_get::<_,i32>(1)?)).ok_or_else(||anyhow::anyhow!("native segment byte count overflow"))?;}
            ensure!(bytes<=64*1024*1024,"native Step segment inventory exceeds read admission");
            tx.query("select message_id,part,bytes from segments where file_id=$1 and message_key=any($2) order by part nulls first,message_id",&[&id,&keys])?.into_iter().map(|row|Ok(crate::catalog_facts::SegmentRef {
                message_id:String::from_utf8(row.try_get(0)?)?,part:row.try_get(1)?,bytes:row.try_get(2)?,
            })).collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_inner_files_keep_sequence_nulls_and_the_legacy_256_row_horizon() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let runtime=runtime(&schema);
        crate::pg::stage_queue_tests::push(&runtime);
        let id=runtime.owner.with(|client| {
            let id:i64=client.query_one("select id from releases",&[])?.get(0);
            client.execute("insert into archive_inner_files(release_id,seq,name,size,source) select $1,n,'inner-é-'||n,case when n%2=0 then null else n end,1+n%5 from generate_series(0,256) n",&[&id])?;
            Ok(id)
        }).unwrap();
        let reads=StepReads::for_owner(&runtime.owner).unwrap();
        let data=crate::postprocess::data::StepData::Native(&reads);
        let rows=data.archive_inner_files(id).unwrap();
        assert_eq!(rows.len(),256);
        for (n,row) in rows.iter().enumerate() {
            let n=n as i64;
            assert_eq!((row.seq,row.name.as_str(),row.size,row.source),(n,format!("inner-é-{n}").as_str(),if n%2==0 {None}else{Some(n)},1+n%5));
        }
        assert!(data.archive_inner_files(id+1).unwrap().is_empty());
        runtime.owner.close();
        assert!(matches!(data.archive_inner_files(id),Err(crate::postprocess::step::StepError::Native(_))),"an ended owner must not become an empty archive");
    }

    #[test]
    fn native_inner_files_admit_the_selected_payload_before_loading_names() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let runtime=runtime(&schema);
        crate::pg::stage_queue_tests::push(&runtime);
        let id=runtime.owner.with(|client| {
            let id:i64=client.query_one("select id from releases",&[])?.get(0);
            client.execute("insert into archive_inner_files(release_id,seq,name,source) select $1,n,repeat('x',262145),1 from generate_series(0,255) n",&[&id])?;
            Ok(id)
        }).unwrap();
        let reads=StepReads::for_owner(&runtime.owner).unwrap();
        assert!(crate::postprocess::data::StepData::Native(&reads).archive_inner_files(id).is_err());
        runtime.owner.close();
    }

    fn runtime(schema:&crate::pg::testing::Schema)->crate::pg::indexer_runtime::Runtime {
        let mut config=crate::config::Config::default();
        config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;
        config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;
        let storage=crate::pg::StorageConfig {backend:crate::pg::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),
            max_connections:1,allow_plaintext:true};
        let headers=crate::config::HeaderStoreConfig {enabled:true,dir:std::env::temp_dir().join("native-step-read-fixture").to_str().unwrap().into()};
        crate::pg::indexer_runtime::Runtime::open(&config,storage,headers,&[],&mut None).unwrap()
    }

    #[test]
    fn native_step_reads_run_on_a_reader_session_not_the_owner() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let runtime=runtime(&schema);
        let reads=StepReads::for_owner(&runtime.owner).unwrap();
        let owner_pid:i32=runtime.owner.with(|client|Ok(client.query_one("select pg_backend_pid()",&[])?.get(0))).unwrap();
        let before=runtime.owner.reader_reads();
        let (pid,read_only):(i32,String)=reads.read(|tx|{let row=tx.query_one("select pg_backend_pid(),current_setting('transaction_read_only')",&[])?;Ok((row.get(0),row.get(1)))}).unwrap();
        assert_ne!(pid,owner_pid,"a Step read must not run on the owner session");
        assert_eq!(read_only,"on");
        assert_eq!(reads.probe_moviehash(1).unwrap(),None);
        assert_eq!(runtime.owner.reader_reads(),before+2,"each Step read is one reader transaction");
        // the reader's epoch proof: the current epoch passes, another (a successor's view of a stale one) is refused
        let epoch=runtime.owner.epoch().unwrap().clone();
        let mut client=schema.pool.get().unwrap();
        {let mut tx=client.build_transaction().isolation_level(postgres::IsolationLevel::RepeatableRead).read_only(true).start().unwrap();
            epoch.verify_reader(&mut tx).unwrap();
            assert!(epoch.other_for_test().verify_reader(&mut tx).is_err(),"a stale epoch must be refused");}
        assert_eq!(runtime.owner.failure(),None);
        runtime.owner.close();
        // once the owner session has ended (its exclusive lock gone), the reader proof fails for its epoch
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(10);
        loop {
            let mut tx=client.build_transaction().isolation_level(postgres::IsolationLevel::RepeatableRead).read_only(true).start().unwrap();
            if epoch.verify_reader(&mut tx).is_err() {break}
            assert!(std::time::Instant::now()<deadline,"an ended owner's epoch still verified for a reader");
            drop(tx);std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(reads.probe_moviehash(1).is_err(),"a closed owner's reads end");
    }

    #[test]
    fn native_step_reads_take_an_owner_turn_when_every_reader_is_busy() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let runtime=runtime(&schema);
        let reads=StepReads::for_owner(&runtime.owner).unwrap();
        let owner_pid:i32=runtime.owner.with(|client|Ok(client.query_one("select pg_backend_pid()",&[])?.get(0))).unwrap();
        let held=runtime.owner.hold_readers_for_test();
        assert_eq!(held.len(),crate::pg::indexer_runtime::READER_SESSIONS);
        let before=runtime.owner.reader_reads();let started=std::time::Instant::now();
        let pid:i32=reads.read(|tx|Ok(tx.query_one("select pg_backend_pid()",&[])?.get(0))).unwrap();
        assert_eq!(pid,owner_pid,"a full reader pool falls back to the owner turn");
        assert!(started.elapsed()<std::time::Duration::from_secs(10),"a full reader pool must not hang a read");
        assert_eq!(runtime.owner.reader_reads(),before);
        assert_eq!(runtime.owner.failure(),None,"a full reader pool never ends the owner");
        drop(held);
        let pid:i32=reads.read(|tx|Ok(tx.query_one("select pg_backend_pid()",&[])?.get(0))).unwrap();
        assert_ne!(pid,owner_pid);
        runtime.owner.close();
    }

    #[test]
    fn native_reader_statement_timeout_fails_one_step_not_the_owner() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let runtime=runtime(&schema);
        let reads=StepReads::for_owner(&runtime.owner).unwrap();
        let error=reads.read(|tx|{tx.batch_execute("set local statement_timeout='100ms'")?;tx.batch_execute("select pg_sleep(2)")?;Ok(())}).unwrap_err();
        assert!(super::super::outage::is_step_timed_out(&error),"{error:#}");
        assert_eq!(runtime.owner.failure(),None);
        assert_eq!(reads.probe_moviehash(1).unwrap(),None,"the next read still runs");
        runtime.owner.close();
    }

    /// A read that handles a statement's error itself and returns a value (the repair reads return a statement
    /// their budget cut short as `Deferred`) leaves the reader's transaction aborted; that is rolled back, not taken
    /// for an owner failure. On prod the closing epoch proof sent into it failed with 25P02 and ended the attempt.
    #[test]
    fn native_reader_read_that_handled_its_own_error_doesnt_fail_the_owner() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let runtime=runtime(&schema);
        let reads=StepReads::for_owner(&runtime.owner).unwrap();
        let value=reads.read(|tx|{
            tx.batch_execute("set local statement_timeout='100ms'")?;
            assert!(tx.batch_execute("select pg_sleep(2)").is_err());
            Ok(7)
        }).unwrap();
        assert_eq!(value,7);
        assert!(runtime.owner.reader_reads()>0,"it ran on a reader session");
        assert_eq!(runtime.owner.failure(),None);
        assert_eq!(reads.probe_moviehash(1).unwrap(),None,"the next read still runs");
        runtime.owner.close();
    }

    #[test]
    fn native_step_reads_fall_back_to_the_owner_without_reader_sessions() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let mut config=crate::config::Config::default();
        config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;
        config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;
        config.native_reader_sessions=Some(0);
        let storage=crate::pg::StorageConfig {backend:crate::pg::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),
            max_connections:1,allow_plaintext:true};
        let headers=crate::config::HeaderStoreConfig {enabled:true,dir:std::env::temp_dir().join("native-step-read-fixture").to_str().unwrap().into()};
        let runtime=crate::pg::indexer_runtime::Runtime::open(&config,storage,headers,&[],&mut None).unwrap();
        let reads=StepReads::for_owner(&runtime.owner).unwrap();
        let owner_pid:i32=runtime.owner.with(|client|Ok(client.query_one("select pg_backend_pid()",&[])?.get(0))).unwrap();
        let pid:i32=reads.read(|tx|Ok(tx.query_one("select pg_backend_pid()",&[])?.get(0))).unwrap();
        assert_eq!(pid,owner_pid);
        assert_eq!(runtime.owner.reader_reads(),0);
        runtime.owner.close();
    }

    #[test]
    fn native_step_reader_rejects_an_entered_tokio_lane() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let runtime=runtime(&schema);
        let reads=StepReads::for_owner(&runtime.owner).unwrap();
        let tokio=tokio::runtime::Builder::new_current_thread().build().unwrap();
        let _entered=tokio.enter();
        assert!(reads.probe_moviehash(1).is_err());
        assert!(StepReads::for_owner(&runtime.owner).is_err());
        drop(_entered);
        assert_eq!(runtime.owner.failure(),None);
        runtime.owner.close();
    }

    #[test]
    #[ignore = "requires explicit isolated ATLAS_PG_TEST_URL and the canonical native owner"]
    fn ended_native_read_is_a_typed_attempt_failure_without_a_legacy_path() {
        let schema=crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
        let runtime=runtime(&schema);let reads=StepReads::for_owner(&runtime.owner).unwrap();
        let data=crate::postprocess::data::StepData::Native(&reads);
        assert_eq!(data.probe_moviehash(1).unwrap(),None,"an available native reader can report a genuine miss");
        runtime.owner.close();
        match data.probe_moviehash(1) {
            Err(crate::postprocess::step::StepError::Native(error))=>assert!(crate::pg::outage::is_attempt_ended(&error)),
            _=>panic!("an ended native reader must not become a per-release retry or cache miss"),
        }
        assert_eq!(runtime.owner.failure(),None,"intentional owner closure is not a new storage failure");
    }

    #[test]
    fn native_step_reads_actual_positive_negative_and_missing_facts() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let runtime=runtime(&schema);
        let id=runtime.owner.with(|client|{
            crate::pg::release_writer::save(client,&[crate::parser::Release {
                name:"Example.Release".into(),group:"example.group".into(),articles:vec![crate::parser::Article {
                    filename:Some("sample.bin".into()),message_id:"<part@example.test>".into(),part:Some(1),total_parts:Some(1),bytes:1,
                    ..Default::default()}],..Default::default()}],1)?;
            let id:i64=client.query_one("select id from releases",&[])?.get(0);
            client.execute("insert into release_probe(release_id,info,moviehash) values($1,'{}','AbCd')",&[&id])?;
            client.execute("insert into release_opensubtitles(release_id,moviehash,kind,checked_at) values($1,'AbCd','movie',1)",&[&id])?;
            client.execute("insert into release_category(release_id,category) values($1,2000)",&[&id])?;
            Ok(id)
        }).unwrap();
        runtime.owner.with(|client|{
            client.execute("insert into media_release(release_id,kind,title_key,quality) values($1,1,37,0)",&[&id])?;
            client.execute("insert into release_nfo_password(release_id,password) values($1,'')",&[&id])?;
            client.execute("insert into pp_state(release_id,done,failed) values($1,4,8)",&[&id])?;
            Ok(())
        }).unwrap();
        let reads=StepReads::for_owner(&runtime.owner).unwrap();
        let data=crate::postprocess::data::StepData::Native(&reads);
        assert_eq!(data.probe_moviehash(id).unwrap().as_deref(),Some("AbCd"));
        assert!(data.opensubtitles_asked(id,"AbCd").unwrap());assert!(!data.opensubtitles_asked(id,"abcd").unwrap());
        assert_eq!(data.opensubtitles_movie_imdb(id).unwrap(),None);
        assert!(data.category_exists(id).unwrap());assert_eq!(data.probe_moviehash(id+1).unwrap(),None);
        assert_eq!(data.nfo_password(id).unwrap().as_deref(),Some(""));
        assert_eq!(data.media_imdb_marker(id).unwrap(),Some(None));
        assert_eq!(data.media_imdb_marker(id+1).unwrap(),None);
        assert!(!data.stale_media_ids(37).unwrap());
        assert_eq!(data.finished_step_bits(id,15).unwrap(),12);
        assert_eq!(data.sidecar_text_bytes().unwrap(),0);
        assert_eq!(data.archive_total_name_bytes().unwrap(),0);
        assert!(!data.has_audio_print(id).unwrap());
        let files=data.release_files(id).unwrap();assert_eq!(files.len(),1);
        let segments=data.file_segments(files[0].id,&crate::catalog_facts::SegmentPick::First(1)).unwrap();
        assert_eq!(segments[0].message_id,"<part@example.test>");
        runtime.owner.close();
        assert!(data.probe_moviehash(id).is_err());
    }

    fn article(file:&str,id:&str,part:Option<i64>,total:Option<i64>,bytes:i64)->crate::parser::Article {
        crate::parser::Article {filename:Some(file.into()),message_id:id.into(),part,total_parts:total,bytes,..Default::default()}
    }
    fn saved(runtime:&crate::pg::indexer_runtime::Runtime,articles:Vec<crate::parser::Article>)->i64 {
        runtime.owner.with(|client| {
            crate::pg::release_writer::save(client,&[crate::parser::Release {name:"Example.Release".into(),group:"example.group".into(),articles,..Default::default()}],1)?;
            Ok(client.query_one("select id from releases",&[])?.get(0))
        }).unwrap()
    }

    // Passes with the articles in PostgreSQL and in the local store alike.
    #[test]
    fn native_segment_picks_keep_order_copy_cap_and_selection_admission() {
        use crate::catalog_facts::SegmentPick;
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let runtime=runtime(&schema);
        let mut articles=vec![article("sample.bin","<c@example.test>",Some(3),Some(3),30),article("sample.bin","<b@example.test>",Some(2),Some(3),20)];
        // Seventeen copies of part 1, saved out of order: only the sixteen lowest IDs are picked.
        for n in (0..17).rev() {articles.push(article("sample.bin",&format!("<a{n:02}@example.test>"),Some(1),Some(3),10+n));}
        let id=saved(&runtime,articles);
        let reads=StepReads::for_owner(&runtime.owner).unwrap();
        let file=reads.files(id,10).unwrap()[0].id;
        let ids=|pick:SegmentPick|reads.segments(file,&pick).unwrap().into_iter().map(|s|(s.message_id,s.part,s.bytes)).collect::<Vec<_>>();
        let copy=|n:i64|(format!("<a{n:02}@example.test>"),Some(1),10+n);
        assert_eq!(ids(SegmentPick::First(2)),vec![copy(0),copy(1)]);
        assert_eq!(ids(SegmentPick::Last(3)),vec![copy(16),("<b@example.test>".into(),Some(2),20),("<c@example.test>".into(),Some(3),30)]);
        assert_eq!(ids(SegmentPick::First(100)).len(),19);
        assert_eq!(ids(SegmentPick::Last(100)),ids(SegmentPick::First(100)));
        let mut wanted:Vec<_>=(0..16).map(copy).collect();wanted.push(("<c@example.test>".into(),Some(3),30));
        assert_eq!(ids(SegmentPick::Parts(vec![3,1,3,7])),wanted);
        assert!(ids(SegmentPick::Parts(Vec::new())).is_empty());
        assert!(ids(SegmentPick::First(0)).is_empty());
        assert!(reads.segments(file+1,&SegmentPick::First(5)).unwrap().is_empty());
        assert!(reads.segments(file,&SegmentPick::First(65537)).is_err());
        assert!(reads.segments(file,&SegmentPick::Parts(vec![1;4097])).is_err());
        runtime.owner.close();
        assert!(reads.segments(file,&SegmentPick::First(1)).is_err(),"an ended owner must not become an empty file");
    }

    #[test]
    fn native_folded_file_crc_takes_each_parts_first_recorded_copy() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let runtime=runtime(&schema);
        let id=saved(&runtime,vec![
            article("whole.bin","<a1@example.test>",Some(1),Some(2),10),article("whole.bin","<a0@example.test>",Some(1),Some(2),10),
            article("whole.bin","<b0@example.test>",Some(2),Some(2),10),
            article("short.bin","<s1@example.test>",Some(1),Some(2),10),article("short.bin","<s2@example.test>",Some(2),Some(2),10),
        ]);
        let crc=|mid:&str,decoded:i64,crc:i64|runtime.owner.with(|client|{client.execute("insert into segment_crc(release_id,message_id,decoded,crc32) values($1,$2,$3,$4)",&[&id,&mid.as_bytes(),&decoded,&crc])?;Ok(())}).unwrap();
        // Part 1's lowest ID has no CRC, so its second copy counts. short.bin lacks part 2's CRC and folds to nothing.
        crc("<a1@example.test>",700_000,0x1111_1111);crc("<b0@example.test>",600_000,0x2222_2222);crc("<s1@example.test>",900_000,0x3333_3333);
        crc("<elsewhere@example.test>",5,5);
        let reads=StepReads::for_owner(&runtime.owner).unwrap();
        let whole=crate::keys::FileCrc::new(1_300_000,crate::gf2::crc32_combine(0x1111_1111,0x2222_2222,600_000)).unwrap();
        assert_eq!(reads.release_lookup(id,None).unwrap().files,vec![whole]);
        match reads.keys_snapshot(id,None,None,std::time::Instant::now()+std::time::Duration::from_secs(10)).unwrap() {
            crate::pg::stage_reads::KeysRead::Complete(snapshot)=>assert_eq!(snapshot.computed.file_crc,Some(whole.key())),
            other=>panic!("unexpected Keys read: {other:?}"),
        }
        // The lowest copy wins once it has a CRC of its own.
        crc("<a0@example.test>",800_000,0x4444_4444);
        let whole=crate::keys::FileCrc::new(1_400_000,crate::gf2::crc32_combine(0x4444_4444,0x2222_2222,600_000)).unwrap();
        assert_eq!(reads.release_lookup(id,None).unwrap().files,vec![whole]);
        runtime.owner.close();
    }

    #[test]
    fn native_par2_names_match_leftover_files_by_their_article_bytes() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let runtime=runtime(&schema);
        let id=saved(&runtime,vec![article("aaa.bin","<one@example.test>",Some(1),Some(1),100_000),article("bbb.bin","<two@example.test>",Some(1),Some(1),500_000)]);
        runtime.owner.with(|client| {
            for (n,name,size) in [(1u8,"real-one.mkv",99_000i64),(2,"real-two.mkv",495_000)] {
                client.execute("insert into par2_files(release_id,file_id,set_id,md5,md5_16k,size,name) values($1,$2,$2,$2,$2,$3,$4)",&[&id,&vec![n;16],&size,&name])?;
            }
            Ok(())
        }).unwrap();
        let reads=StepReads::for_owner(&runtime.owner).unwrap();
        let files=reads.files(id,10).unwrap();
        let names=reads.par2_names(id,&files).unwrap();
        assert_eq!(names,std::collections::HashMap::from([("aaa.bin".to_owned(),"real-one.mkv".to_owned()),("bbb.bin".to_owned(),"real-two.mkv".to_owned())]));
        runtime.owner.close();
    }

    #[test]
    fn native_step_storage_failure_is_not_a_miss() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let runtime=runtime(&schema);let reads=StepReads::for_owner(&runtime.owner).unwrap();
        runtime.owner.with(|client|{client.batch_execute("drop table release_probe")?;Ok(())}).unwrap();
        let data=crate::postprocess::data::StepData::Native(&reads);
        assert!(data.probe_moviehash(41).is_err());
        assert_eq!(runtime.owner.failure(),Some(crate::pg::outage::Failure::Fatal));runtime.owner.close();
    }
}

#[cfg(test)]
#[path="stage_probe_admission_tests.rs"]
mod probe_admission_tests;
