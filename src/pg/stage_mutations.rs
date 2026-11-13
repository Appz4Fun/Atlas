//! Closed native result statements. The queue actor owns the original owner's
//! READ COMMITTED transaction and locks its release before calling this module.
//! This module grants no owner, lease, commit, retry, or connection authority.

use anyhow::{Result,bail};
use postgres::Transaction;
use crate::postprocess::mutation::StepMutation;

pub(super) fn apply(tx:&mut Transaction<'_>,id:i64,mutation:&StepMutation)->Result<u64> {
    use StepMutation::*;
    Ok(match mutation {
        OpenSubtitlesMatch {moviehash,feature,checked_at}=>{
            let f=feature.clone().unwrap_or_default();
            tx.execute("insert into release_opensubtitles(release_id,moviehash,imdb_id,tmdb_id,kind,title,year,season,episode,release_name,checked_at)
                values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) on conflict(release_id) do update set moviehash=excluded.moviehash,imdb_id=excluded.imdb_id,tmdb_id=excluded.tmdb_id,kind=excluded.kind,title=excluded.title,year=excluded.year,season=excluded.season,episode=excluded.episode,release_name=excluded.release_name,checked_at=excluded.checked_at",
                &[&id,moviehash,&f.imdb_id,&f.tmdb_id,&f.kind,&f.title,&f.year,&f.season,&f.episode,&f.release_name,checked_at])?
        },
        PinOpenSubtitlesMovieImdb {imdb_id}|PinNfoMovieImdb {imdb_id}=>tx.execute("update media_release set imdb_id=$2,imdb_from_nfo=1 where release_id=$1 and imdb_id is null",&[&id,imdb_id])?,
        SetCategory {category}=>tx.execute("insert into release_category(release_id,category) values($1,$2) on conflict(release_id) do update set category=excluded.category",&[&id,category])?,
        ClearCategory=>tx.execute("delete from release_category where release_id=$1",&[&id])?,
        SetProbeInfo {json}=>tx.execute("update release_probe set info=$2 where release_id=$1",&[&id,json])?,
        KeepProbe {crc32,agent,json,moviehash}=>tx.execute("insert into release_probe(release_id,crc32,agent,info,moviehash) values($1,$2,$3,$4,$5) on conflict(release_id) do update set crc32=excluded.crc32,agent=excluded.agent,info=excluded.info,moviehash=excluded.moviehash",&[&id,crc32,agent,json,moviehash])?,
        ClearMedia=>tx.execute("delete from media_release where release_id=$1",&[&id])?,
        ParseMedia {kind,title_key,parsed}=>{
            let ids=crate::media::native_cached_ids_in(tx,*kind,*title_key,parsed)?;
            let number=|n:Option<i32>|n.map(i64::from);
            tx.execute("insert into media_release as old(release_id,kind,title_key,season,episode,episode_last,air_date,year,quality,tvdb_id,tvmaze_id,trakt_id,tmdb_id,imdb_id,tvrage_id)
                values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)
                on conflict(release_id) do update set
                tvdb_id=case when old.title_key=excluded.title_key then case when old.tvdb_from_tag=1 then old.tvdb_id else coalesce(excluded.tvdb_id,old.tvdb_id) end else excluded.tvdb_id end,
                tvmaze_id=case when old.title_key=excluded.title_key then coalesce(excluded.tvmaze_id,old.tvmaze_id) else excluded.tvmaze_id end,
                trakt_id=case when old.title_key=excluded.title_key then coalesce(excluded.trakt_id,old.trakt_id) else excluded.trakt_id end,
                tmdb_id=case when old.title_key=excluded.title_key then case when old.tmdb_from_tag=1 then old.tmdb_id else coalesce(excluded.tmdb_id,old.tmdb_id) end else excluded.tmdb_id end,
                imdb_id=case when old.title_key=excluded.title_key then case when old.imdb_from_nfo=1 then old.imdb_id else coalesce(excluded.imdb_id,old.imdb_id) end else excluded.imdb_id end,
                tvrage_id=case when old.title_key=excluded.title_key then coalesce(excluded.tvrage_id,old.tvrage_id) else excluded.tvrage_id end,
                imdb_from_nfo=case when old.title_key=excluded.title_key then old.imdb_from_nfo else 0 end,
                tmdb_from_tag=case when old.title_key=excluded.title_key then old.tmdb_from_tag else 0 end,
                tvdb_from_tag=case when old.title_key=excluded.title_key then old.tvdb_from_tag else 0 end,
                kind=excluded.kind,title_key=excluded.title_key,season=excluded.season,episode=excluded.episode,episode_last=excluded.episode_last,air_date=excluded.air_date,year=excluded.year,quality=excluded.quality",
                &[&id,&(*kind as i64),title_key,&number(parsed.season),&number(parsed.episode),&number(parsed.episode_last),&number(parsed.air_date),&number(parsed.year),&i64::from(parsed.quality),&ids.tvdb,&ids.tvmaze,&ids.trakt,&ids.tmdb,&ids.imdb,&ids.tvrage])?
        },
        FoundMediaIds {title_key,ids}=>tx.execute("update media_release set tvdb_id=case when tvdb_from_tag=1 then tvdb_id else $3 end,tvmaze_id=$4,trakt_id=$5,tmdb_id=case when tmdb_from_tag=1 then tmdb_id else $6 end,imdb_id=case when imdb_from_nfo=1 then imdb_id else $7 end,tvrage_id=$8 where release_id=$1 and title_key=$2",
            &[&id,title_key,&ids.tvdb,&ids.tvmaze,&ids.trakt,&ids.tmdb,&ids.imdb,&ids.tvrage])?,
        PinTagImdb {imdb_id}=>tx.execute("update media_release set imdb_id=$2,imdb_from_nfo=1 where release_id=$1 and kind=2",&[&id,imdb_id])?,
        PinTagTmdb {tmdb_id,kind}=>tx.execute("update media_release set tmdb_id=$2,tmdb_from_tag=1 where release_id=$1 and kind=$3",&[&id,tmdb_id,&(*kind as i64)])?,
        PinTagTvdb {tvdb_id}=>tx.execute("update media_release set tvdb_id=$2,tvdb_from_tag=1 where release_id=$1 and kind=1",&[&id,tvdb_id])?,
        KeepNfo {file,text}=>{
            let text=text.chars().take(crate::probe::nfo_store::MAX_CHARS).collect::<String>();
            tx.execute("insert into release_nfo(release_id,file,text) values($1,$2,$3) on conflict(release_id) do nothing",&[&id,file,&text])?
        },
        SetNfoPassword {password}=>tx.execute("insert into release_nfo_password(release_id,password) values($1,$2) on conflict(release_id) do update set password=excluded.password",&[&id,password])?,
        DropNfoCopy {file}=>tx.execute("delete from release_nfo where release_id=$1 and file=$2",&[&id,file])?,
        KeepCover {mime,sha256}=>tx.execute("insert into release_cover(release_id,mime,sha256) values($1,$2,$3) on conflict(release_id) do update set mime=excluded.mime,sha256=excluded.sha256",&[&id,mime,sha256])?,
        ClearCover=>tx.execute("delete from release_cover where release_id=$1",&[&id])?,
        ReindexWords=>tx.execute("insert into word_queue(release_id) values($1) on conflict(release_id) do update set n=word_queue.n+1",&[&id])?,
        ClearPredbInnerKeys=>tx.execute("delete from predb_inner_key where release_id=$1",&[&id])?,
        AddPredbInnerKey {key}=>tx.execute("insert into predb_inner_key(key,release_id) values($1,$2) on conflict(key,release_id) do nothing",&[key,&id])?,
        PredbRead {revision}=>{super::predb_cache::verify_read_in(tx,*revision)?;0},
        XrefRead {generation}=>{super::stage_reads::xref::verify_read_in(tx,id,*generation)?;0},
        XrefTried {generation,count}=>super::stage_reads::xref::tried_in(tx,id,*generation,*count)?,
        KeepPredbMatch {title_id,kind,category,pre_at,nuked,matched_at}=>tx.execute("insert into predb_match(release_id,title_id,kind,category,pre_at,nuked,matched_at) values($1,$2,$3,$4,$5,$6,$7) on conflict(release_id) do update set title_id=excluded.title_id,kind=excluded.kind,category=excluded.category,pre_at=excluded.pre_at,nuked=excluded.nuked,matched_at=excluded.matched_at",&[&id,title_id,&(*kind as i64),category,pre_at,nuked,matched_at])?,
        KeepSidecar(record)=>tx.execute("insert into sidecars(release_id,name,group_name,file,kind,release_name,segment_uid,text,at) values($1,$2,$3,$4,$5,$6,$7,$8,$9)
            on conflict(release_id) do update set name=excluded.name,group_name=excluded.group_name,
            file=case when excluded.kind='unread' then sidecars.file else excluded.file end,
            kind=case when excluded.kind='unread' then sidecars.kind else excluded.kind end,
            release_name=case when excluded.kind='unread' then sidecars.release_name else excluded.release_name end,
            segment_uid=case when excluded.kind='unread' then sidecars.segment_uid else excluded.segment_uid end,
            text=coalesce(excluded.text,sidecars.text),at=excluded.at",&[&id,&record.name,&record.group,&record.file,&record.kind,&record.release_name,&record.segment_uid,&record.text,&record.at])?,
        EvictSidecarTexts {max_bytes}=>tx.execute("update sidecars set text=null where release_id in(select release_id from(select release_id,octet_length(text) as len,sum(octet_length(text)) over(order by at,release_id rows unbounded preceding) as run from sidecars where text is not null and release_id<>$1) old where run-len<coalesce((select value from meta where key='sidecar_text_bytes'),0)-$2)",&[&id,max_bytes])?,
        RecordSegmentCrc {message_id,decoded,crc32}=>tx.execute("insert into segment_crc(release_id,message_id,decoded,crc32) values($1,$2,$3,$4) on conflict(release_id,message_id) do update set decoded=excluded.decoded,crc32=excluded.crc32",&[&id,&message_id.as_bytes(),decoded,&i64::from(*crc32)])?,
        KeepReleaseKeys {keys}=>tx.execute("insert into release_keys(release_id,account,host,layout,lines,file_crc,body_crc,video,audio,mux,encode,thread,subs,fonts,chapters,volumes) values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)
            on conflict(release_id) do update set account=excluded.account,host=excluded.host,layout=excluded.layout,lines=excluded.lines,file_crc=excluded.file_crc,body_crc=excluded.body_crc,video=excluded.video,audio=excluded.audio,mux=excluded.mux,encode=excluded.encode,thread=excluded.thread,subs=excluded.subs,fonts=excluded.fonts,chapters=excluded.chapters,volumes=excluded.volumes",
            &[&id,&keys.account,&keys.host,&keys.layout,&keys.lines,&keys.file_crc,&keys.body_crc,&keys.video,&keys.audio,&keys.mux,&keys.encode,&keys.thread,&keys.subs,&keys.fonts,&keys.chapters,&keys.volumes])?,
        ClearReleaseKeys=>tx.execute("delete from release_keys where release_id=$1",&[&id])?,
        ClearStills=>tx.execute("delete from release_stills where release_id=$1",&[&id])?,
        KeepStill(still)=>tx.execute("insert into release_stills(release_id,position,at_ms,sha256,width,height,phash) values($1,$2,$3,$4,$5,$6,$7) on conflict(release_id,position) do update set at_ms=excluded.at_ms,sha256=excluded.sha256,width=excluded.width,height=excluded.height,phash=excluded.phash",
            &[&id,&i64::from(still.position),&still.at_ms,&still.sha256,&i64::from(still.width),&i64::from(still.height),&still.phash])?,
        ClearAudioPrints=>tx.execute("delete from release_audio_print where release_id=$1",&[&id])?,
        ClearAudioAnchors=>tx.execute("delete from release_audio_anchor where release_id=$1",&[&id])?,
        KeepAudioPrint {position,at_ms,items}=>tx.execute("insert into release_audio_print(release_id,position,at_ms,items) values($1,$2,$3,$4) on conflict(release_id,position) do update set at_ms=excluded.at_ms,items=excluded.items",&[&id,&i64::from(*position),at_ms,&crate::audio::to_blob(items)])?,
        KeepAudioAnchor {anchor}=>tx.execute("insert into release_audio_anchor(release_id,anchor) values($1,$2) on conflict(release_id,anchor) do nothing",&[&id,anchor])?,
        ClearImages=>tx.execute("delete from release_images where release_id=$1",&[&id])?,
        KeepImage {position,image}=>tx.execute("insert into release_images(release_id,position,file,sha256,format,width,height,bytes,meta,exif_id,document_id) values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) on conflict(release_id,position) do update set file=excluded.file,sha256=excluded.sha256,format=excluded.format,width=excluded.width,height=excluded.height,bytes=excluded.bytes,meta=excluded.meta,exif_id=excluded.exif_id,document_id=excluded.document_id",
            &[&id,position,&image.file,&image.sha256,&image.format,&i64::from(image.width),&i64::from(image.height),&image.bytes,&image.meta,&image.exif_id,&image.document_id])?,
        AnswerImageRequest {file,done_at,answer}=>tx.execute("update picture_requests set done_at=$3,outcome=$4 where release_id=$1 and file=$2",&[&id,file,done_at,&answer.word()])?,
        ClearArchiveSource {source}=>tx.execute("delete from archive_inner_files where release_id=$1 and source=$2",&[&id,source])?,
        KeepArchiveFile {sequence,source,file}=>tx.execute("insert into archive_inner_files(release_id,seq,name,size,source,crc32,mtime,host_os) values($1,$2,$3,$4,$5,$6,$7,$8) on conflict(release_id,seq) do update set name=excluded.name,size=excluded.size,source=excluded.source,crc32=excluded.crc32,mtime=excluded.mtime,host_os=excluded.host_os",
            &[&id,sequence,&file.name,&file.size.and_then(|n|i64::try_from(n).ok()),source,&file.crc32.map(i64::from),&file.mtime,&file.host_os.map(i64::from)])?,
        AdjustArchiveNameBytes {delta}=>tx.execute("insert into meta(key,value) values('archive_name_bytes',$1) on conflict(key) do update set value=meta.value+excluded.value",&[delta])?,
        ArchiveNamesOverQuota=>tx.execute("insert into meta(key,value) values('archive_names_over_quota',1) on conflict(key) do update set value=meta.value+1",&[])?,
        SetArchivePassword {password}=>tx.execute("insert into archive_release(release_id,password) values($1,$2) on conflict(release_id) do update set password=excluded.password",&[&id,&password.code()])?,
        SetArchiveEncryption {encryption,names_hidden,check}=>tx.execute("update archive_release set encryption=$2,names_hidden=$3,password_check=$4 where release_id=$1",&[&id,&encryption.map(crate::archive::Encryption::code),&i64::from(*names_hidden),&check.map(|check|check.to_bytes())])?,
        SetArchiveKeys {keys,facts}=>tx.execute("update archive_release set packer=$2,twin_key=$3,owner_key=$4,zone_key=$5,facts=$6 where release_id=$1",&[&id,&keys.packer,&keys.twin_key,&keys.owner_key,&keys.zone_key,facts])?,
        ClearRevVolumes=>tx.execute("delete from rev_volumes where release_id=$1",&[&id])?,
        KeepRevVolume(volume)=>tx.execute("insert into rev_volumes(release_id,volume,size,crc32) values($1,$2,$3,$4) on conflict(release_id,volume) do update set size=excluded.size,crc32=excluded.crc32",&[&id,&i64::from(volume.index),&i64::try_from(volume.size).unwrap_or(i64::MAX),&i64::from(volume.crc32)])?,
        ClearContentFingerprint=>tx.execute("update release_fingerprint set content=null where release_id=$1",&[&id])?,
        SetContentFingerprint {content}=>tx.execute("insert into release_fingerprint(release_id,content) values($1,$2) on conflict(release_id) do update set content=excluded.content",&[&id,content])?,
        SetPostedFingerprint {posted}=>tx.execute("insert into release_fingerprint(release_id,posted) values($1,$2) on conflict(release_id) do update set posted=excluded.posted",&[&id,posted])?,
        ClearPostedFingerprint=>tx.execute("update release_fingerprint set posted=null,par2_block=null,par2_blocks=null where release_id=$1",&[&id])?,
        SetPar2Sizes {block,blocks}=>tx.execute("insert into release_fingerprint(release_id,par2_block,par2_blocks) values($1,$2,$3) on conflict(release_id) do update set par2_block=excluded.par2_block,par2_blocks=excluded.par2_blocks",&[&id,block,blocks])?,
        ClearPar2Files=>tx.execute("delete from par2_files where release_id=$1",&[&id])?,
        KeepPar2File {file,sealed,complete}=>tx.execute("insert into par2_files(release_id,file_id,set_id,md5,md5_16k,size,name,crc32,bound,sealed,complete) values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) on conflict(release_id,file_id) do update set set_id=excluded.set_id,md5=excluded.md5,md5_16k=excluded.md5_16k,size=excluded.size,name=excluded.name,crc32=excluded.crc32,bound=excluded.bound,sealed=excluded.sealed,complete=excluded.complete,creator=null",
            &[&id,&&file.file_id[..],&&file.set_id[..],&&file.md5[..],&&file.md5_16k[..],&i64::try_from(file.size).unwrap_or(i64::MAX),&file.name,&file.crc32.map(i64::from),&i64::from(file.bound),&i64::from(*sealed),&i64::from(*complete)])?,
        SetPar2Creator {set_id,creator}=>tx.execute("update par2_files set creator=$3 where release_id=$1 and set_id=$2",&[&id,&&set_id[..],creator])?,
        VouchComplete=>tx.execute(VOUCH_COMPLETE,&[&id])?,
        DropPcCategory=>tx.execute("delete from release_category where release_id=$1",&[&id])?,
        // An unported result cannot be silently acknowledged. The worker
        // admission guard stays closed until every configured result is routed.
        _=>bail!("native Step mutation is not ported"),
    })
}

pub(super) fn apply_observed(tx:&mut Transaction<'_>,id:i64,mutation:&StepMutation,locked:&super::stage_queue::participants::Locked)->Result<u64> {
    anyhow::ensure!(locked.permits(id,mutation),"native cross result has no current locked participants");
    use StepMutation::*;
    Ok(match mutation {
        RememberUploadCandidate {candidate_id}=>tx.execute("insert into upload_link_tried(release_id,candidate_id) values($1,$2) on conflict do nothing",&[&id,candidate_id])?,
        PlaceBesideFile {row_id,owner_id,par2_file}=>tx.execute("insert into beside_files(row_id,release_id,par2_file) values($1,$2,$3)
            on conflict(row_id) do update set release_id=excluded.release_id,par2_file=excluded.par2_file",&[row_id,owner_id,par2_file])?,
        NameUploadLead {lead_id,name,source}=>tx.execute("update releases set display_name=$2,name_source=$3 where id=$1 and display_name is null and coalesce(name_source,0)<=$3",&[lead_id,name,&i64::from(*source)])?,
        RerunUploadLead {lead_id,clear,net}=>tx.execute("insert into pp_state as old(release_id,net_wanted,rerun_gen,rerun_mask,net_left)
            values($1,case when $3::bigint<>0 then true end,1,$2,nullif($3::bigint,0)) on conflict(release_id) do update set
            done=old.done & ~$2::bigint,failed=old.failed & ~$2::bigint,net_wanted=case when $3::bigint<>0 then true else old.net_wanted end,
            net_left=case when $3::bigint<>0 then coalesce(old.net_left,0)|$3::bigint else old.net_left end,
            net_next_at=case when $3::bigint<>0 then 0 else old.net_next_at end,rerun_gen=old.rerun_gen+1,rerun_mask=old.rerun_mask|$2::bigint",&[lead_id,clear,net])?,
        QueueUploadLead {lead_id,due_at}=>tx.execute("insert into pp_queue(release_id,due_at) values($1,$2) on conflict(release_id) do update set due_at=least(pp_queue.due_at,excluded.due_at),work_serial=pp_queue.work_serial+1",&[lead_id,due_at])?,
        MoveUpload {from_id,to_id}=>{
            let lead=locked.lead_in(tx,*to_id)?;
            tx.execute("update upload_link set lead_id=$2 where lead_id=$1 and $1<>$3",&[from_id,&lead,to_id])?
        },
        LinkUploadData {member_id,lead_id,par2_file}=>{
            let lead=locked.lead_in(tx,*lead_id)?;
            tx.execute("insert into upload_link(release_id,lead_id,recovery,par2_file) values($1,$2,0,$3)
                on conflict(release_id) do update set lead_id=excluded.lead_id,recovery=0,par2_file=excluded.par2_file",&[member_id,&lead,&&par2_file[..]])?
        },
        LinkUploadRecovery {lead_id,wanted,set_id}=>{
            let lead=locked.lead_in(tx,*lead_id)?;
            tx.execute("insert into upload_link(release_id,lead_id,recovery,wanted,set_id) values($1,$2,1,$3,$4)
                on conflict(release_id) do update set lead_id=excluded.lead_id,recovery=1,wanted=excluded.wanted,set_id=excluded.set_id",&[&id,&lead,wanted,&set_id.map(|id|id.to_vec())])?
        },
        KeepNfoForLegacyTarget {target_id,file,text}=>{
            let text=text.chars().take(crate::probe::nfo_store::MAX_CHARS).collect::<String>();
            tx.execute("insert into release_nfo(release_id,file,text) values($1,$2,$3) on conflict(release_id) do nothing",&[target_id,file,&text])?
        },
        MarkPointed {name}=>tx.execute("insert into nzb_pointed(release_id) select unnest($1::bigint[]) on conflict do nothing",&[&locked.pointed(name)?])?,
        RankPointed {name}=>{
            // Reuse the exact native rank expression with a closed correlated
            // target identifier, never a caller expression or SQL fragment.
            let sql=super::stage_queue::NET_RANK.replace("where r.id = $1","where r.id = pp_state.release_id");
            let prefix=sql.strip_suffix("where release_id=$1").ok_or_else(||anyhow::anyhow!("native rank statement shape changed"))?;
            tx.execute(&format!("{prefix}where release_id=any($1) and net_wanted=true"),&[&locked.pointed(name)?])?
        },
        RerunPointedList {name}|RerunPointedProbe {name}=>{
            let probe=crate::postprocess::step::mask(crate::probe::step::PROBE_STEP);
            let net=probe | if matches!(mutation,RerunPointedList {..}) {crate::postprocess::step::mask(crate::archive::steps::LIST_STEP)}else{0};
            let clear=net | crate::postprocess::step::mask(crate::keys::KEYS_STEP);
            let condition=if matches!(mutation,RerunPointedList {..}) {
                "exists(select 1 from archive_release ar where ar.release_id=pp_state.release_id and ((ar.names_hidden=1 and not exists(select 1 from archive_inner_files f where f.release_id=ar.release_id and f.source in(1,2,3))) or (ar.password=1 and exists(select 1 from archive_inner_files f where f.release_id=ar.release_id and f.source in(1,2,3) and lower(f.name) like '%.nzb') and not exists(select 1 from archive_inner_files f where f.release_id=ar.release_id and f.source=5))))"
            } else {"exists(select 1 from release_probe rp where rp.release_id=pp_state.release_id and rp.info like '%\"encrypted\":true%')"};
            tx.execute(&format!("update pp_state set done=done & ~$2::bigint,failed=failed & ~$2::bigint,net_wanted=true,
                net_left=coalesce(net_left,0)|$3::bigint,net_next_at=0,net_attempts=0,net_transient=0,rerun_gen=rerun_gen+1,rerun_mask=rerun_mask|$2::bigint
                where release_id=any($1) and not exists(select 1 from release_nfo_password p where p.release_id=pp_state.release_id) and {condition}"),&[&locked.pointed(name)?,&clear,&net])?
        },
        SetPointedPassword {name,password}=>tx.execute("insert into release_nfo_password(release_id,password) select unnest($1::bigint[]),$2::text on conflict(release_id) do nothing",&[&locked.pointed(name)?,password])?,
        _=>apply(tx,id,mutation)?,
    })
}

// Same actual PAR2 set-vouch conditions as store::SET_VOUCHED_COMPLETE,
// using the native writer's normalized per-file completeness instead of blobs.
const VOUCH_COMPLETE:&str="update releases set complete=true where id=$1 and not complete
 and exists(select 1 from files where release_id=$1)
 and not exists(select 1 from files where release_id=$1 and not complete)
 and exists(select 1 from par2_files where release_id=$1 and complete=1)
 and not exists(select 1 from par2_files p where p.release_id=$1 and p.complete=1
   and not exists(select 1 from files f where f.release_id=$1 and f.filename=p.name))
 and not exists(select 1 from files f where f.release_id=$1
   and lower(f.filename) not like '%.par2' and lower(f.filename) not like '%.nfo'
   and lower(f.filename) not like '%.sfv' and lower(f.filename) not like '%.srr'
   and lower(f.filename) not like '%.srs' and lower(f.filename) not like '%.txt'
   and lower(f.filename) not like '%.jpg' and lower(f.filename) not like '%.png'
   and lower(f.filename) not like '%.md5'
   and not exists(select 1 from par2_files p where p.release_id=$1 and p.complete=1 and p.name=f.filename))";
