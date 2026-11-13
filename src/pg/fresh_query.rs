//! Fresh API query pages use the same Tantivy matcher and actual native media
//! IDs. SQL handles newest/category/ID lookup, never a replacement tsquery.
use super::{fresh_reader::ReadView,fresh_words};
use crate::{search::ReleaseRow,media::query::{IdQuery,IdColumn},category::CatFilter,word_index::{self,native::{self,SearchSource}}};
use anyhow::{Result,ensure,Context};

pub(crate) struct QueryPage {pub rows:Vec<ReleaseRow>,pub total:Option<u64>}
impl ReadView<'_> {
    pub(crate) fn api_query(&self,term:&str,ids:Option<&IdQuery>,cats:Option<&CatFilter>,offset:i64,limit:usize)->Result<QueryPage> {
        ensure!(offset>=0 && limit>0 && limit<=self.budget.rows,"native API page exceeds admission");
        if ids.is_some() && offset>crate::search::MAX_FILTER_OFFSET {return Ok(QueryPage {rows:Vec::new(),total:None});}
        if self.count()?==0{return Ok(QueryPage {rows:Vec::new(),total:Some(0)});}
        if let Some(ids)=ids{return self.api_ids(ids,cats,offset,limit);}
        if !term.trim().is_empty() {
            let mut source=fresh_words::Search::open(self)?;let stamp=source.stamp()?;
            let index=word_index::WordIndex::read_native(&self.word_index_root(&crate::config::header_store_config())?,stamp)?;
            let found=index.search_native(&mut source,term,cats,&[],word_index::By::Relevance,true,usize::try_from(offset)?,limit,
                native::Budget {rows:self.budget.rows,bytes:self.budget.bytes,commit_bytes:self.budget.bytes})?;
            return Ok(QueryPage {rows:found.rows,total:Some(u64::try_from(found.total)?)});
        }
        self.api_newest(cats,offset,limit)
    }
    fn api_newest(&self,cats:Option<&CatFilter>,offset:i64,limit:usize)->Result<QueryPage> {
        if cats.is_none(){return Ok(QueryPage {rows:self.recent(offset,limit)?,total:None});}
        let leaves=cats.map_or(Vec::new(),|filter|filter.leaves.clone());let uncategorized=cats.is_some_and(|filter|filter.uncategorized);
        let selection=(limit as u64).checked_mul(16).and_then(|n|n.checked_add(4096)).context("native API selection extent overflow")?;
        let room=self.remaining_owned().checked_sub(selection).context("native API descriptor workspace exceeds admission")?;
        let page_size=self.budget.rows.min(128).min(usize::try_from(room/2048).unwrap_or(usize::MAX));
        ensure!(page_size>0,"native API descriptor workspace exceeds admission");
        let mut before:Option<i64>=None;let mut passed=0i64;let mut rejects=0;let mut ids=Vec::with_capacity(limit);
        'pages: loop {
            self.before_query()?;
            let descriptors=self.tx.borrow_mut().query("select r.id,case when octet_length(r.group_name)<=$6 then r.group_name else null::text end
                from releases r left join release_category c on c.release_id=r.id
                where ($1::bigint is null or r.id<$1) and ($2 or c.category=any($3) or($4 and c.release_id is null))
                and (r.display_name is not null or(not r.is_obfuscated and(length(r.name)<16 or r.name collate \"C\" ~ '[^A-Za-z0-9]')))
                order by r.id desc limit $5",&[&before,&cats.is_none(),&leaves,&uncategorized,&i64::try_from(page_size)?,&(crate::filters::MAX_GROUP_NAME as i32)])?;
            let count=descriptors.len();if count==0{break;}
            for descriptor in descriptors {
                self.check_deadline()?;let id:i64=descriptor.try_get(0)?;before=Some(id);let group:Option<String>=descriptor.try_get(1)?;
                if group.as_deref().is_some_and(|group|!crate::filters::group_blocked(group)) {
                    if passed<offset{passed=passed.checked_add(1).context("native API offset overflow")?;}else{ids.push(id);}
                }else{rejects+=1;}
                if ids.len()==limit||rejects>crate::search::Bounds::DEFAULT.rejects{break 'pages;}
            }
            if count<page_size{break;}
        }
        let rows=self.rows(&ids)?;ensure!(rows.len()==ids.len(),"native API visibility changed during hydration");
        Ok(QueryPage {rows,total:None})
    }
    fn api_ids(&self,query:&IdQuery,cats:Option<&CatFilter>,offset:i64,limit:usize)->Result<QueryPage> {
        let mut values:[Option<i64>;6]=[None;6];
        for (column,value) in &query.ids {
            let at=match column {IdColumn::Tvdb=>0,IdColumn::Tvmaze=>1,IdColumn::Trakt=>2,IdColumn::Tmdb=>3,IdColumn::Imdb=>4,IdColumn::Tvrage=>5};values[at]=Some(*value);
        }
        let queries=["tvdb_id","tvmaze_id","trakt_id","tmdb_id","imdb_id","tvrage_id"].iter().enumerate().map(|(at,column)|format!(
            "(select release_id from(select release_id,kind,season,episode,episode_last from media_release where {column}=${} order by release_id desc limit 200000) scanned
                where kind=$7 and($8::bigint is null or season=$8) and($9::bigint is null or(episode<=$9 and coalesce(episode_last,episode)>=$9)) limit 20000)",at+1)).collect::<Vec<_>>().join(" union ");
        let sql=format!("with matches as({queries}) select r.id,case when octet_length(r.group_name)<=$15 and
            (r.display_name is not null or(not r.is_obfuscated and(length(r.name)<16 or r.name collate \"C\" ~ '[^A-Za-z0-9]')))
            then r.group_name else null::text end from matches m join releases r on r.id=m.release_id
            left join release_category c on c.release_id=r.id where ($10::bigint is null or r.id<$10)
            and($11 or c.category=any($12) or($13 and c.release_id is null)) order by r.id desc limit $14");
        let kind=query.kind as i64;let leaves=cats.map_or(Vec::new(),|filter|filter.leaves.clone());let uncategorized=cats.is_some_and(|filter|filter.uncategorized);
        let selection=(limit as u64).checked_mul(16).and_then(|n|n.checked_add(4096)).context("native ID selection extent overflow")?;
        let room=self.remaining_owned().checked_sub(selection).context("native ID descriptor workspace exceeds admission")?;
        let page_size=self.budget.rows.min(128).min(usize::try_from(room/2048).unwrap_or(usize::MAX));
        ensure!(page_size>0,"native ID descriptor workspace exceeds admission");
        let mut before:Option<i64>=None;let mut passed=0i64;let mut rejects=0;let mut ids=Vec::with_capacity(limit);
        'pages: loop {
            self.before_query()?;
            let descriptors=self.tx.borrow_mut().query(&sql,&[&values[0],&values[1],&values[2],&values[3],&values[4],&values[5],&kind,&query.season,&query.episode,
                &before,&cats.is_none(),&leaves,&uncategorized,&i64::try_from(page_size)?,&(crate::filters::MAX_GROUP_NAME as i32)])?;
            let count=descriptors.len();if count==0{break;}
            for descriptor in descriptors {
                self.check_deadline()?;let id:i64=descriptor.try_get(0)?;before=Some(id);let group:Option<String>=descriptor.try_get(1)?;
                if group.as_deref().is_some_and(|group|!crate::filters::group_blocked(group)) {
                    if passed<offset{passed=passed.checked_add(1).context("native ID offset overflow")?;}else{ids.push(id);}
                }else{rejects+=1;}
                if ids.len()==limit||rejects>crate::search::Bounds::DEFAULT.rejects{break 'pages;}
            }
            if count<page_size{break;}
        }
        let rows=self.rows(&ids)?;ensure!(rows.len()==ids.len(),"native ID visibility changed during hydration");
        Ok(QueryPage {rows,total:None})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{pg::{self,fresh_reader::Budget},parser::{Article,Release},media::{query::IdColumn,title::Kind}};
    use std::time::{Duration,Instant};
    #[test]
    #[ignore="requires explicit isolated migrated fresh PostgreSQL schema9"]
    fn fresh_recent_category_and_id_pages_use_actual_writer_ids_and_readonly_facts() {
        let schema=pg::testing::Schema::new().expect("explicit isolated PG schema9");let mut client=schema.pool.get().unwrap();
        let releases=["Alpha.Movie.2020.1080p","Beta.Movie.2021.2160p"].into_iter().enumerate().map(|(n,name)|Release {
            name:name.into(),group:"alt.binaries.test".into(),poster:"poster".into(),date:"2020-01-01".into(),
            articles:vec![Article {filename:Some("sample.bin".into()),subject:"sample.bin yEnc (1/1)".into(),message_id:format!("<part{n}@example.test>"),
                part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}],..Default::default()
        }).collect::<Vec<_>>();
        pg::release_writer::save(&mut client,&releases,1).unwrap();
        let actual=client.query("select id from releases order by id desc",&[]).unwrap().into_iter().map(|row|row.get::<_,i64>(0)).collect::<Vec<_>>();
        for id in &actual {client.execute("insert into release_category(release_id,category) values($1,2040)",&[id]).unwrap();}
        client.execute("insert into media_release(release_id,kind,title_key,quality,imdb_id,tmdb_id) values($1,2,99,0,4242422,123)",&[&actual[0]]).unwrap();
        let view=ReadView::open(&mut client,&schema.name,Budget {rows:100,..Budget::default()},Instant::now()+Duration::from_secs(10)).unwrap();
        let page=view.api_query("",None,None,0,100).unwrap();assert_eq!(page.rows.iter().map(|row|row.id).collect::<Vec<_>>(),actual);
        assert!(page.rows.iter().all(|row|row.complete));
        let category=crate::category::parse_filter("2040").unwrap();
        let page=view.api_query("",None,Some(&category),1,1).unwrap();assert_eq!(page.rows[0].id,actual[1]);
        let query=IdQuery {kind:Kind::Movie,ids:vec![(IdColumn::Imdb,4242422),(IdColumn::Tmdb,123)],season:None,episode:None};
        let page=view.api_query("",Some(&query),Some(&category),0,100).unwrap();
        assert_eq!(page.rows.iter().map(|row|row.id).collect::<Vec<_>>(),vec![actual[0]],"two namespace matches are one item");
        let other=crate::category::parse_filter("5000").unwrap();assert!(view.api_query("",Some(&query),Some(&other),0,100).unwrap().rows.is_empty());
        let missing=IdQuery {ids:vec![(IdColumn::Imdb,7777777)],..query};assert!(view.api_query("",Some(&missing),None,0,100).unwrap().rows.is_empty());
    }
}

#[cfg(test)]
mod descriptor_page_tests {
    use super::*;
    use crate::{parser::{Article,Release},pg::{self,fresh_reader::Budget},media::title::Kind};
    use std::time::{Duration,Instant};

    #[test]
    #[ignore="requires isolated PostgreSQL and actual bounded native API-page admission"]
    fn newest_category_and_media_id_offsets_do_not_charge_discarded_release_values() {
        let schema=pg::testing::Schema::new().expect("explicit isolated PostgreSQL");let mut client=schema.pool.get().unwrap();
        let releases=(0..2305).map(|n|Release {name:format!("Api.Visible.{n:04}"),group:"alt.binaries.test".into(),poster:"p".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
            articles:vec![Article {filename:Some("f.bin".into()),subject:"f.bin yEnc (1/1)".into(),message_id:format!("api-page-{n}@example.invalid"),part:Some(1),total_parts:Some(1),bytes:1,..Default::default()}],..Default::default()}).collect::<Vec<_>>();
        assert_eq!(pg::release_writer::save(&mut client,&releases,1).unwrap().releases,2305);
        client.execute("insert into release_category(release_id,category) select id,2040 from releases",&[]).unwrap();
        client.execute("insert into media_release(release_id,kind,title_key,quality,tmdb_id,imdb_id) select id,2,99,0,123,424242 from releases",&[]).unwrap();
        let all=client.query("select id from releases order by id desc",&[]).unwrap().into_iter().map(|row|row.get::<_,i64>(0)).collect::<Vec<_>>();
        let blocked=vec![all[10],all[1500],all[1650],all[2100]];
        client.execute("update releases set group_name='alt.binaries.pictures.erotica.teen' where id=any($1)",&[&blocked]).unwrap();
        let expected=all.into_iter().filter(|id|!blocked.contains(id)).collect::<Vec<_>>();
        let category=crate::category::parse_filter("2040").unwrap();
        let query=IdQuery {kind:Kind::Movie,ids:vec![(IdColumn::Tmdb,123),(IdColumn::Imdb,424242)],season:None,episode:None};
        for (ids,cats) in [(None,None),(None,Some(&category)),(Some(&query),Some(&category))] {
            let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(15)).unwrap();
            let first=view.api_query("",ids,cats,1600,50).unwrap();let second=view.api_query("",ids,cats,2000,50).unwrap();
            assert_eq!(first.rows.iter().map(|row|row.id).collect::<Vec<_>>(),expected[1600..1650]);
            assert_eq!(second.rows.iter().map(|row|row.id).collect::<Vec<_>>(),expected[2000..2050]);
            assert!(first.rows.iter().chain(&second.rows).all(|row|row.name.starts_with("Api.Visible.")&&row.group_name=="alt.binaries.test"));
        }
        for (ids,cats) in [(None,None),(None,Some(&category)),(Some(&query),Some(&category))] {
            let view=ReadView::open(&mut client,&schema.name,Budget {bytes:220000,..Budget::default()},Instant::now()+Duration::from_secs(15)).unwrap();
            let retained=view.api_query("",ids,cats,0,50).unwrap();assert_eq!(retained.rows.len(),50);
            assert!(view.api_query("",ids,cats,50,1).is_err(),"returned API values must retain all ownership charges");
            assert_eq!(retained.rows[0].id,expected[0]);
        }
    }
}
