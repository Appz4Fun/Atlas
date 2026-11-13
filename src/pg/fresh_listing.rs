//! Bounded result enrichment from actual fresh local facts and retained PG
//! metadata. An absent optional row is observed SQL absence, not an empty
//! historical projection or an invitation to fetch a provider.
use super::fresh_reader::ReadView;
use anyhow::{Result,ensure,Context};
use std::collections::HashMap;
use crate::{archive::Password,media::{query::MediaAttrs,resolve::Ids,details::Details}};

pub(crate) struct Facts {pub category:i64,pub password:Option<Password>,pub media:Option<MediaAttrs>,pub details:Option<Details>}
impl ReadView<'_> {
    pub(crate) fn listing_facts(&self,ids:&[i64])->Result<HashMap<i64,Facts>> {
        ensure!(ids.len()<=self.budget.rows,"native listing exceeds row admission");
        if ids.is_empty(){return Ok(HashMap::new());}
        self.before_query()?;
        let rows=self.tx.borrow_mut().query("select r.id,c.category,a.password,m.kind,m.season,m.episode,m.air_date,m.year,
            m.tvdb_id,m.tvmaze_id,m.trakt_id,m.tmdb_id,m.imdb_id,m.tvrage_id
            from releases r left join release_category c on c.release_id=r.id
            left join archive_release a on a.release_id=r.id left join media_release m on m.release_id=r.id
            where r.id=any($1) order by r.id",&[&ids])?;
        let mut result=HashMap::new();
        for row in rows {
            let id:i64=row.try_get(0)?;
            let media=if let Some(kind)=row.try_get::<_,Option<i64>>(3)? {Some(MediaAttrs {kind,season:row.try_get(4)?,episode:row.try_get(5)?,air_date:row.try_get(6)?,year:row.try_get(7)?,
                ids:Ids {tvdb:row.try_get(8)?,tvmaze:row.try_get(9)?,trakt:row.try_get(10)?,tmdb:row.try_get(11)?,imdb:row.try_get(12)?,tvrage:row.try_get(13)?}})}else{None};
            let password=row.try_get::<_,Option<i64>>(2)?.map(|code|Password::from_code(code).context("native password code is invalid")).transpose()?;
            let details=match media.as_ref().and_then(|media|media.ids.tmdb.map(|tmdb|(media.kind,tmdb))) {
                Some((kind,tmdb))=>self.kept_details(kind,tmdb)?,None=>None,
            };
            result.insert(id,Facts {category:row.try_get::<_,Option<i64>>(1)?.unwrap_or(crate::category::UNCATEGORIZED),password,media,details});
        }Ok(result)
    }
    pub(crate) fn kept_details(&self,kind:i64,tmdb:i64)->Result<Option<Details>> {
        self.before_query()?;
        let size=self.tx.borrow_mut().query_opt("select coalesce(octet_length(title),0)::bigint+coalesce(octet_length(overview),0)::bigint+
            coalesce(octet_length(genres),0)::bigint+coalesce(octet_length(status),0)::bigint+coalesce(octet_length(network),0)::bigint+
            coalesce(octet_length(cast_names),0)::bigint+coalesce(octet_length(released),0)::bigint+coalesce(octet_length(bluray),0)::bigint+
            coalesce(octet_length(poster),0)::bigint+coalesce(octet_length(backdrop),0)::bigint
            from ext_media_details where kind=$1 and tmdb_id=$2",&[&kind,&tmdb])?;
        let Some(size)=size else{return Ok(None)};let size:i64=size.try_get(0)?;
        ensure!(size>=0,"negative native details extent");
        self.admit_owned((size as u64).checked_mul(3).and_then(|n|n.checked_add(4096)).context("native kept details extent overflow")?)?;
        self.before_query()?;
        let row=self.tx.borrow_mut().query_one("select title,overview,genres,runtime,status,network,cast_names,released,bluray,poster,backdrop,
            tmdb_score,imdb_score,rotten,metacritic,trakt_score,fetched_at from ext_media_details where kind=$1 and tmdb_id=$2",&[&kind,&tmdb])?;
        Ok(Some(Details {kind,tmdb_id:tmdb,title:row.try_get(0)?,overview:row.try_get(1)?,genres:row.try_get(2)?,runtime:row.try_get(3)?,status:row.try_get(4)?,
            network:row.try_get(5)?,cast:row.try_get(6)?,released:row.try_get(7)?,bluray:row.try_get(8)?,poster:row.try_get(9)?,backdrop:row.try_get(10)?,
            tmdb_score:row.try_get(11)?,imdb_score:row.try_get(12)?,rotten:row.try_get(13)?,metacritic:row.try_get(14)?,trakt_score:row.try_get(15)?,fetched_at:row.try_get(16)?}))
    }
}
