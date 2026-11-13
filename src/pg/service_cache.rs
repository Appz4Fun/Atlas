//! Media/HTTP/PreDB commands use the original native owner session. No Pool,
//! reconnect, SQLite fallback or provider call exists in this storage layer.

pub(crate) mod predb;
mod predb_late;
pub(crate) use predb_late::LateReport;

use std::io::Read;
use std::sync::{Arc,Weak};
use anyhow::{Result,ensure};
use postgres::GenericClient;
use crate::media::{resolve::Ids,store::TitleRow};
use super::indexer_runtime::{Owner,destination_identity};

const TEXT_BYTES:i64=4*1024*1024;
const HTTP_BYTES:usize=2*1024*1024;
const HTTP_TTL:i64=12*60*60;

#[derive(Clone)]
pub(crate) struct Cache {owner:Weak<Owner>}
impl Cache {
    pub(crate) fn new(owner:&Arc<Owner>)->Result<Self> {
        ensure!(owner.cache_destination().is_some() && owner.epoch().is_some(),"media cache requires a validated native owner");
        Ok(Self {owner:Arc::downgrade(owner)})
    }

    fn with<T>(&self,work:impl FnOnce(&mut postgres::Transaction<'_>)->Result<T>)->Result<T> {
        let owner=self.owner.upgrade().ok_or(super::outage::AttemptEnded)?;
        ensure!(tokio::runtime::Handle::try_current().is_err(),"native service cache requires a standard worker thread");
        let destination=owner.cache_destination().ok_or_else(||anyhow::anyhow!("native service cache destination unavailable"))?.clone();
        owner.with(|client|{
            let storage=crate::config::checked_storage_config().map_err(|_|anyhow::anyhow!("native service cache configuration unavailable"))?;
            ensure!(storage==destination.storage,"native service cache destination changed");
            ensure!(destination_identity(client,&storage.schema)?==destination.identity,"native service cache identity changed");
            ensure!(super::migrate::version(client,&storage.schema)?==destination.version,"native service cache version changed");
            let mut tx=client.transaction()?;super::history_gate::native(&mut tx)?;
            let value=work(&mut tx)?;tx.commit()?;Ok(value)
        })
    }

    pub(crate) fn predb_advance(&self, page_titles:u16)->Result<super::predb_cache::BuildStatus> {
        self.with(|tx| super::predb_cache::advance_in(tx,page_titles))
    }
    pub(crate) fn predb_import(&self,source:i64,entries:&[crate::predb::import::Entry],now:i64,cursor:Option<(i64,i64)>)->Result<super::predb_cache::ImportStatus> {
        self.with(|tx| super::predb_cache::import_in(tx,source,entries,now,cursor))
    }
    pub(crate) fn title(&self,key:i64)->Result<Option<TitleRow>> {self.with(|tx|title_in(tx,key))}
    pub(crate) fn check(&self)->Result<()> {self.with(|_|Ok(()))}
    pub(crate) fn put_title(&self,row:&TitleRow)->Result<()> {self.with(|tx|put_title_in(tx,row))}
    pub(crate) fn fanout_owed(&self,key:i64)->Result<()> {self.with(|tx|{tx.execute("update ext_media_titles set fanout_done=0 where title_key=$1",&[&key])?;Ok(())})}
    pub(crate) fn title_count(&self)->Result<u64> {self.with(|tx|Ok(u64::try_from(tx.query_one("select count(*) from ext_media_titles",&[])?.try_get::<_,i64>(0)?)?))}
    pub(crate) fn http(&self,source:&str,url:&str)->Result<Option<(u16,String)>> {self.with(|tx|http_in(tx,source,url))}
    pub(crate) fn put_http(&self,source:&str,url:&str,status:u16,body:&str)->Result<()> {self.with(|tx|put_http_in(tx,source,url,status,body))}

    /// Idempotent bounded fan-out on the same owner. New parsed rows obtain IDs
    /// inside their own settlement transaction, so finishing an empty batch
    /// cannot hide a late new row. A lost commit leaves either the old pending
    /// flag or the entire completed batch; no callback is replayed.
    pub(crate) fn fanout_batch(&self)->Result<bool> {
        self.with(|tx|{
            let Some(row)=tx.query_opt("select title_key from ext_media_titles where fanout_done=0 order by title_key limit 1",&[])? else {return Ok(false)};
            let key:i64=row.try_get(0)?;let row=title_in(tx,key)?.ok_or_else(||anyhow::anyhow!("pending media cache row disappeared"))?;
            let ids=if row.state==crate::media::store::STATE_FOUND {row.ids}else{Ids::default()};
            let candidates=tx.query("select r.id from releases r join media_release m on m.release_id=r.id where m.title_key=$1
              and ((m.tvdb_from_tag=0 and m.tvdb_id is distinct from $2) or m.tvmaze_id is distinct from $3 or m.trakt_id is distinct from $4
              or (m.tmdb_from_tag=0 and m.tmdb_id is distinct from $5) or (m.imdb_from_nfo=0 and m.imdb_id is distinct from $6) or m.tvrage_id is distinct from $7)
              order by r.id limit 1000 for update of r",&[&key,&ids.tvdb,&ids.tvmaze,&ids.trakt,&ids.tmdb,&ids.imdb,&ids.tvrage])?;
            let release_ids=candidates.into_iter().map(|row|row.try_get::<_,i64>(0)).collect::<Result<Vec<_>,_>>()?;
            if release_ids.is_empty() {tx.execute("update ext_media_titles set fanout_done=1 where title_key=$1",&[&key])?;}
            else {tx.execute("update media_release set tvdb_id=case when tvdb_from_tag=1 then tvdb_id else $3 end,tvmaze_id=$4,trakt_id=$5,
              tmdb_id=case when tmdb_from_tag=1 then tmdb_id else $6 end,imdb_id=case when imdb_from_nfo=1 then imdb_id else $7 end,tvrage_id=$8
              where title_key=$1 and release_id=any($2)",&[&key,&release_ids,&ids.tvdb,&ids.tvmaze,&ids.trakt,&ids.tmdb,&ids.imdb,&ids.tvrage])?;
              // Existing visits may have read these IDs. Advance their actual
              // locked release inputs in the same transaction as the fanout.
              tx.execute("update releases set native_work_revision=native_work_revision+1 where id=any($1)",&[&release_ids])?;
            }
            Ok(true)
        })
    }
}

pub(crate) fn title_in(client:&mut impl GenericClient,key:i64)->Result<Option<TitleRow>> {
    let row=client.query_opt("select octet_length(title)::bigint+coalesce(octet_length(found_title)::bigint,0)<=$2 as admitted,
      title_key,kind,case when octet_length(title)::bigint+coalesce(octet_length(found_title)::bigint,0)<=$2 then title end as title,
      year,state,checked_mask,tvdb_id,tvmaze_id,trakt_id,tmdb_id,imdb_id,tvrage_id,
      case when octet_length(title)::bigint+coalesce(octet_length(found_title)::bigint,0)<=$2 then found_title end as found_title,
      checked_at,next_check,misses,fanout_done from ext_media_titles where title_key=$1",&[&key,&TEXT_BYTES])?;
    let Some(row)=row else {return Ok(None)};ensure!(row.try_get::<_,bool>("admitted")?,"media cache row exceeds admission");
    let row=TitleRow {key:row.try_get("title_key")?,kind:row.try_get("kind")?,title:row.try_get("title")?,year:row.try_get::<_,Option<i64>>("year")?.map(i32::try_from).transpose()?,
      state:row.try_get("state")?,checked_mask:row.try_get("checked_mask")?,
      ids:Ids {tvdb:row.try_get("tvdb_id")?,tvmaze:row.try_get("tvmaze_id")?,trakt:row.try_get("trakt_id")?,tmdb:row.try_get("tmdb_id")?,imdb:row.try_get("imdb_id")?,tvrage:row.try_get("tvrage_id")?},
      found_title:row.try_get("found_title")?,checked_at:row.try_get("checked_at")?,next_check:row.try_get("next_check")?,misses:row.try_get("misses")?,fanout_done:row.try_get::<_,i64>("fanout_done")?!=0};
    ensure!((1..=2).contains(&row.kind) && (0..=2).contains(&row.state),"unsupported media cache kind/state");Ok(Some(row))
}

pub(crate) fn put_title_in(client:&mut impl GenericClient,row:&TitleRow)->Result<()> {
    ensure!(row.title.len().checked_add(row.found_title.as_ref().map_or(0,String::len)).is_some_and(|bytes|bytes<=TEXT_BYTES as usize),"media cache row exceeds admission");
    ensure!((1..=2).contains(&row.kind) && (0..=2).contains(&row.state),"unsupported media cache kind/state");
    client.execute("insert into ext_media_titles(title_key,kind,title,year,state,checked_mask,tvdb_id,tvmaze_id,trakt_id,tmdb_id,imdb_id,tvrage_id,found_title,checked_at,next_check,misses,fanout_done)
      values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)
      on conflict(title_key) do update set kind=excluded.kind,title=excluded.title,year=excluded.year,state=excluded.state,checked_mask=excluded.checked_mask,
      tvdb_id=excluded.tvdb_id,tvmaze_id=excluded.tvmaze_id,trakt_id=excluded.trakt_id,tmdb_id=excluded.tmdb_id,imdb_id=excluded.imdb_id,tvrage_id=excluded.tvrage_id,
      found_title=excluded.found_title,checked_at=excluded.checked_at,next_check=excluded.next_check,misses=excluded.misses,fanout_done=excluded.fanout_done",
      &[&row.key,&row.kind,&row.title,&row.year.map(i64::from),&row.state,&row.checked_mask,&row.ids.tvdb,&row.ids.tvmaze,&row.ids.trakt,&row.ids.tmdb,&row.ids.imdb,&row.ids.tvrage,
        &row.found_title,&row.checked_at,&row.next_check,&row.misses,&i64::from(row.fanout_done)])?;Ok(())
}

fn http_key(source:&str,url:&str)->Result<(String,String)> {
    ensure!(source.len()<=128 && url.len()<=16*1024,"HTTP cache key exceeds admission");
    let clean=crate::raw_store::canonical_cache_url(url);Ok((format!("GET:{source}:{clean}"),clean))
}

pub(crate) fn http_in(client:&mut impl GenericClient,source:&str,url:&str)->Result<Option<(u16,String)>> {
    let(key,_)=http_key(source,url)?;
    let row=client.query_opt("select status,fetched_at,octet_length(body_zst)::bigint<=$2 as admitted,
      case when octet_length(body_zst)::bigint<=$2 then body_zst end as body_zst from ext_http_cache where cache_key=$1",&[&key,&TEXT_BYTES])?;
    let Some(row)=row else {return Ok(None)};
    if chrono::Utc::now().timestamp().saturating_sub(row.try_get("fetched_at")?)>HTTP_TTL {return Ok(None)}
    ensure!(row.try_get::<_,bool>("admitted")?,"HTTP cache row exceeds admission");
    let status=u16::try_from(row.try_get::<_,i64>("status")?)?;let compressed:Vec<u8>=row.try_get("body_zst")?;
    let mut bytes=Vec::new();zstd::Decoder::new(&compressed[..])?.take(HTTP_BYTES as u64+1).read_to_end(&mut bytes)?;
    ensure!(bytes.len()<=HTTP_BYTES,"HTTP cache decoded body exceeds admission");Ok(Some((status,String::from_utf8(bytes)?)))
}

pub(crate) fn put_http_in(client:&mut impl GenericClient,source:&str,url:&str,status:u16,body:&str)->Result<()> {
    ensure!(body.len()<=HTTP_BYTES,"HTTP body exceeds cache admission");let(key,clean)=http_key(source,url)?;
    let compressed=zstd::bulk::compress(body.as_bytes(),3)?;let now=chrono::Utc::now().timestamp();
    client.execute("insert into ext_http_cache(cache_key,source,url,status,body_zst,fetched_at) values($1,$2,$3,$4,$5,$6)
      on conflict(cache_key) do update set source=excluded.source,url=excluded.url,status=excluded.status,body_zst=excluded.body_zst,fetched_at=excluded.fetched_at",
      &[&key,&source,&clean,&i64::from(status),&compressed,&now])?;Ok(())
}
