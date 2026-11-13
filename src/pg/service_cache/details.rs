//! Natural-key details on the caller's original owned transaction. No provider,
//! pool, SQLite callback or implicit missing/error conversion exists here.
use anyhow::{Result,ensure,Context};
use postgres::GenericClient;
use crate::media::{details::Details,resolve::Ids};
const TEXT:i64=4*1024*1024;
const FIELDS:&str="title,overview,genres,status,network,cast_names,released,bluray,poster,backdrop";

pub(crate) struct Source {pub release:i64,pub kind:i64,pub ids:Ids}
pub(crate) struct Page {pub rows:Vec<Source>,pub after:Option<i64>,pub exhausted:bool}
#[derive(Debug,PartialEq,Eq)]
pub(crate) enum TraktIdentity {Absent,Unique(i64),Ambiguous}

/// Both endpoints share one statement snapshot and an exact equality/NULL
/// prefix of migration12's partial index. Duplicates may span any number of
/// source pages: no release-order scan or unbounded ID collection is needed.
pub(crate) fn trakt_identity_in(client:&mut impl GenericClient,kind:i64,ids:&Ids)->Result<TraktIdentity> {
    ensure!((1..=2).contains(&kind),"native details kind unsupported");
    let mut conditions=vec!["kind=$1".to_string(),"trakt_id is not null".to_string()];
    let mut params:Vec<&(dyn postgres::types::ToSql+Sync)>=vec![&kind];
    for (column,id) in [("tmdb_id",&ids.tmdb),("imdb_id",&ids.imdb),("tvdb_id",&ids.tvdb),("tvmaze_id",&ids.tvmaze),("tvrage_id",&ids.tvrage)] {
        if let Some(id)=id {
            ensure!(*id>0,"native details provider identity is not positive");
            params.push(id);conditions.push(format!("{column}=${}",params.len()));
        } else {conditions.push(format!("{column} is null"));}
    }
    let prefix=conditions.join(" and ");
    let row=client.query_one(&format!("select
        (select trakt_id from media_release where {prefix} order by trakt_id asc limit 1),
        (select trakt_id from media_release where {prefix} order by trakt_id desc limit 1)"),&params)?;
    let (first,last):(Option<i64>,Option<i64>)=(row.try_get(0)?,row.try_get(1)?);
    match (first,last) {
        (None,None)=>Ok(TraktIdentity::Absent),
        (Some(first),Some(last))=>{
            ensure!(first>0&&last>=first,"native details Trakt identity is invalid");
            Ok(if first==last {TraktIdentity::Unique(first)}else{TraktIdentity::Ambiguous})
        },
        _=>anyhow::bail!("native details Trakt endpoints disagree"),
    }
}
pub(crate) fn page_in(client:&mut impl GenericClient,after:Option<i64>,limit:u16)->Result<Page> {
    ensure!((1..=64).contains(&limit),"native details page outside admission");
    // Limit the PK scan BEFORE filtering resolved/current titles. An empty
    // eligible result is not EOF: the physical cursor still advances.
    let tail="order by release_id desc limit $2) m left join releases r on r.id=m.release_id order by m.release_id desc";
    let columns="select m.release_id,m.kind,m.tvdb_id,m.tvmaze_id,m.trakt_id,m.tmdb_id,m.imdb_id,m.tvrage_id,r.id is not null from
      (select release_id,kind,tvdb_id,tvmaze_id,trakt_id,tmdb_id,imdb_id,tvrage_id from media_release";
    let page=if let Some(after)=after {
        // Keep the cursor an index condition even under a generic PG plan.
        client.query(&format!("{columns} where release_id<$1 {tail}"),&[&after,&i64::from(limit)])?
    } else {
        client.query(&format!("{columns} order by release_id desc limit $1) m left join releases r on r.id=m.release_id order by m.release_id desc"),&[&i64::from(limit)])?
    };
    let exhausted=page.is_empty();let after=page.last().map(|row|row.try_get::<_,i64>(0)).transpose()?;
    let mut rows=Vec::new();
    for row in page {
        let kind:i64=row.try_get(1)?;ensure!((1..=2).contains(&kind),"native details source kind unsupported");
        let ids=Ids {tvdb:row.try_get(2)?,tvmaze:row.try_get(3)?,trakt:row.try_get(4)?,tmdb:row.try_get(5)?,imdb:row.try_get(6)?,tvrage:row.try_get(7)?};
        if row.try_get::<_,bool>(8)?&&(ids.tmdb.is_some()||ids.imdb.is_some()||ids.tvdb.is_some()) {rows.push(Source {release:row.try_get(0)?,kind,ids});}
    }Ok(Page {rows,after,exhausted})
}
pub(crate) fn read_in(client:&mut impl GenericClient,kind:i64,id:i64)->Result<Option<Details>> {
    ensure!((1..=2).contains(&kind),"native details kind unsupported");
    let size=FIELDS.split(',').map(|name|format!("coalesce(octet_length({name}),0)::bigint")).collect::<Vec<_>>().join("+");
    // READ COMMITTED callers must admit and select the same row version.
    // Every variable-width text result is CASE-guarded in this one statement;
    // an oversized replacement returns only fixed scalars and the refusal bit.
    let sql=format!("select kind,tmdb_id,
      case when admitted then title end,case when admitted then overview end,
      case when admitted then genres end,runtime,case when admitted then status end,
      case when admitted then network end,case when admitted then cast_names end,
      case when admitted then released end,case when admitted then bluray end,
      case when admitted then poster end,case when admitted then backdrop end,
      tmdb_score,imdb_score,rotten,metacritic,trakt_score,fetched_at,admitted
      from (select *,({size}) between 0 and $3 as admitted from ext_media_details
        where kind=$1 and tmdb_id=$2) d");
    let Some(row)=client.query_opt(&sql,&[&kind,&id,&TEXT])? else{return Ok(None)};
    ensure!(row.try_get::<_,bool>(19)?,"native details text exceeds admission");
    Ok(Some(Details {kind:row.try_get(0)?,tmdb_id:row.try_get(1)?,title:row.try_get(2)?,overview:row.try_get(3)?,genres:row.try_get(4)?,runtime:row.try_get(5)?,status:row.try_get(6)?,network:row.try_get(7)?,cast:row.try_get(8)?,released:row.try_get(9)?,bluray:row.try_get(10)?,poster:row.try_get(11)?,backdrop:row.try_get(12)?,tmdb_score:row.try_get(13)?,imdb_score:row.try_get(14)?,rotten:row.try_get(15)?,metacritic:row.try_get(16)?,trakt_score:row.try_get(17)?,fetched_at:row.try_get(18)?}))
}
pub(crate) fn put_in(client:&mut impl GenericClient,d:&Details)->Result<()> {
    ensure!((1..=2).contains(&d.kind),"native details kind unsupported");
    let mut bytes=0usize;for text in [&d.title,&d.overview,&d.genres,&d.status,&d.network,&d.cast,&d.released,&d.bluray,&d.poster,&d.backdrop] {
        bytes=bytes.checked_add(text.as_ref().map_or(0,String::len)).context("native details extent overflow")?;
    }ensure!(bytes<=TEXT as usize,"native details text exceeds admission");
    client.execute("insert into ext_media_details(kind,tmdb_id,title,overview,genres,runtime,status,network,cast_names,released,bluray,poster,backdrop,tmdb_score,imdb_score,rotten,metacritic,trakt_score,fetched_at)
      values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)
      on conflict(kind,tmdb_id) do update set title=excluded.title,overview=excluded.overview,genres=excluded.genres,runtime=excluded.runtime,status=excluded.status,network=excluded.network,cast_names=excluded.cast_names,released=excluded.released,bluray=excluded.bluray,poster=excluded.poster,backdrop=excluded.backdrop,tmdb_score=excluded.tmdb_score,imdb_score=excluded.imdb_score,rotten=excluded.rotten,metacritic=excluded.metacritic,trakt_score=excluded.trakt_score,fetched_at=excluded.fetched_at",
      &[&d.kind,&d.tmdb_id,&d.title,&d.overview,&d.genres,&d.runtime,&d.status,&d.network,&d.cast,&d.released,&d.bluray,&d.poster,&d.backdrop,&d.tmdb_score,&d.imdb_score,&d.rotten,&d.metacritic,&d.trakt_score,&d.fetched_at])?;Ok(())
}
pub(crate) fn lookup_in(client:&mut impl GenericClient,key:&str)->Result<Option<(Option<i64>,i64)>> {
    ensure!(key.len()<=256,"native details lookup key exceeds admission");
    client.query_opt("select tmdb_id,checked_at from ext_media_details_lookups where lookup=$1",&[&key])?.map(|row|Ok((row.try_get(0)?,row.try_get(1)?))).transpose()
}
pub(crate) fn put_lookup_in(client:&mut impl GenericClient,key:&str,id:Option<i64>,now:i64)->Result<()> {
    ensure!(key.len()<=256,"native details lookup key exceeds admission");
    client.execute("insert into ext_media_details_lookups(lookup,tmdb_id,checked_at) values($1,$2,$3) on conflict(lookup) do update set tmdb_id=excluded.tmdb_id,checked_at=excluded.checked_at",&[&key,&id,&now])?;Ok(())
}
impl super::Cache {
    pub(crate) fn details_trakt(&self,kind:i64,ids:&Ids)->Result<TraktIdentity> {self.with(|tx|trakt_identity_in(tx,kind,ids))}
    pub(crate) fn details_page(&self,after:Option<i64>)->Result<Page> {self.with(|tx|page_in(tx,after,64))}
    pub(crate) fn details(&self,kind:i64,id:i64)->Result<Option<Details>> {self.with(|tx|read_in(tx,kind,id))}
    pub(crate) fn put_details(&self,details:&Details)->Result<()> {self.with(|tx|put_in(tx,details))}
    pub(crate) fn details_lookup(&self,key:&str)->Result<Option<(Option<i64>,i64)>> {self.with(|tx|lookup_in(tx,key))}
    pub(crate) fn put_details_lookup(&self,key:&str,id:Option<i64>,now:i64)->Result<()> {self.with(|tx|put_lookup_in(tx,key,id,now))}
}

#[cfg(test)]
#[path="details_trakt_tests.rs"]
mod trakt_tests;
