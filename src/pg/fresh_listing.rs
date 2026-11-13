//! Bounded result enrichment from actual fresh local facts and retained PG
//! metadata. An absent optional row is observed SQL absence, not an empty
//! historical projection or an invitation to fetch a provider.
use super::fresh_reader::ReadView;
use anyhow::{Result,ensure,Context};
use std::collections::HashMap;
use crate::{archive::Password,media::{query::MediaAttrs,resolve::Ids,details::Details}};

pub(crate) struct ApiFacts {pub category:i64,pub password:Option<Password>,pub media:Option<MediaAttrs>}
pub(crate) struct Facts {pub category:i64,pub password:Option<Password>,pub media:Option<MediaAttrs>,pub details:Option<Details>}
impl ReadView<'_> {
    pub(crate) fn listing_facts(&self,ids:&[i64])->Result<HashMap<i64,Facts>> {
        self.api_facts(ids)?.into_iter().map(|(id,facts)|{
            let details=match facts.media.as_ref() {
                Some(media)=>self.kept_details_for(media.kind,&media.ids)?,None=>None,
            };
            Ok((id,Facts {category:facts.category,password:facts.password,media:facts.media,details}))
        }).collect()
    }
    /// API output uses its own IDs/raw names; it does not load UI details,
    /// inherited title associations or artwork that the feed never renders.
    pub(crate) fn api_facts(&self,ids:&[i64])->Result<HashMap<i64,ApiFacts>> {
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
            result.insert(id,ApiFacts {category:row.try_get::<_,Option<i64>>(1)?.unwrap_or(crate::category::UNCATEGORIZED),password,media});
        }Ok(result)
    }
    /// Same no-fetch selection as details::kept_for: an explicit TMDB ID
    /// precedes the retained lookup, then a television title may use its own
    /// negative TVDB cache key. Cache age and provider switches do not make a
    /// retained record unreadable, and negative lookup answers never fetch.
    fn kept_details_for(&self,kind:i64,ids:&Ids)->Result<Option<Details>> {
        let tmdb=match ids.tmdb {
            Some(id)=>Some(id),None=>{
                let key=crate::media::details::lookup_key(kind,ids);
                self.admit_owned((key.len() as u64).checked_add(128).context("native detail lookup extent overflow")?)?;
                self.before_query()?;
                self.tx.borrow_mut().query_opt("select tmdb_id from ext_media_details_lookups where lookup=$1",&[&key])?
                    .map(|row|row.try_get::<_,Option<i64>>(0)).transpose()?.flatten()
            }
        };
        if let Some(tmdb)=tmdb {if let Some(details)=self.kept_details(kind,tmdb)? {return Ok(Some(details));}}
        if kind==crate::media::title::Kind::Tv as i64 {
            if let Some(tvdb)=ids.tvdb {
                let key=tvdb.checked_neg().context("native TVDB cache key is out of range")?;
                return self.kept_details(kind,key);
            }
        }
        Ok(None)
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
