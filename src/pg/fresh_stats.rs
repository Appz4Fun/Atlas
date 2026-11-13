//! Statistics from actual fresh PG rows and completed native observations.
//! Native catalog retention never routes a release into a SQLite frozen shard.
use super::fresh_reader::ReadView;
use anyhow::{Result,Context,ensure};
use crate::web_stats::{Stored,DayRow,PredbRow};
use std::{collections::BTreeMap,time::Instant};

impl ReadView<'_> {
    pub(crate) fn statistics_clock(&self)->Result<i64> {
        self.before_query()?;
        Ok(self.tx.borrow_mut().query_one("select floor(extract(epoch from clock_timestamp()))::bigint",&[])?.try_get(0)?)
    }
    pub(crate) fn stored_statistics(&self,now:i64)->Result<Stored> {
        let began=Instant::now();
        self.before_query()?;
        let missing:bool=self.tx.borrow_mut().query_one("select exists(select 1 from releases where posted_clock is null limit 1)",&[])?.try_get(0)?;
        ensure!(!missing,"native posting wall-clock conversion is unavailable");
        // The legacy day query compares to_iso_date wall-clock text with a
        // UTC-formatted naive cutoff. NZB posted_at keeps the zone and is a
        // different domain; the writer stores this normalized clock separately.
        let since=now.checked_sub(86400).context("native statistics clock overflow")?;
        self.before_query()?;
        let extent=self.tx.borrow_mut().query_one("select count(*) from(select coalesce(c.category,$2) from releases r
            left join release_category c on c.release_id=r.id where r.posted_clock>=$1 group by 1) categories",&[&since,&crate::category::UNCATEGORIZED])?;
        let count:i64=extent.try_get(0)?;ensure!(count>=0,"negative native statistics category extent");
        self.admit_owned((count as u64).checked_mul(512).and_then(|n|n.checked_add(4096)).context("native statistics category extent overflow")?)?;
        self.before_query()?;
        let rows=self.tx.borrow_mut().query("select coalesce(c.category,$2),count(*),count(*) filter(where r.complete),coalesce(sum(r.size),0)::bigint
            from releases r left join release_category c on c.release_id=r.id where r.posted_clock>=$1 group by 1",&[&since,&crate::category::UNCATEGORIZED])?;
        ensure!(rows.len() as i64==count,"native statistics categories changed within snapshot");
        let mut day=BTreeMap::new();let mut total=0i64;
        for row in rows {
            let category:i64=row.try_get(0)?;let item=DayRow {count:row.try_get(1)?,complete:row.try_get(2)?,bytes:row.try_get(3)?};
            ensure!(item.count>=0&&(0..=item.count).contains(&item.complete),"invalid native day tally");
            total=total.checked_add(item.count).context("native statistics total overflow")?;
            ensure!(day.insert(category,item).is_none(),"duplicate native statistics category");
        }
        self.before_query()?;
        let extent=self.tx.borrow_mut().query_one("with recent as(select * from predb_match order by matched_at desc,release_id desc limit 25)
            select coalesce(sum(octet_length(coalesce(r.display_name,r.name))::bigint+coalesce(octet_length(m.category),0)::bigint),0)::bigint,count(*)
            from recent m join releases r on r.id=m.release_id",&[])?;
        let bytes:i64=extent.try_get(0)?;let count:i64=extent.try_get(1)?;ensure!(bytes>=0&&(0..=25).contains(&count),"invalid native PreDB display extent");
        self.admit_owned((bytes as u64).checked_mul(4).and_then(|n|n.checked_add(count as u64*1024+4096)).context("native PreDB display extent overflow")?)?;
        self.before_query()?;
        let rows=self.tx.borrow_mut().query("with recent as(select * from predb_match order by matched_at desc,release_id desc limit 25)
            select m.release_id,coalesce(r.display_name,r.name),m.kind,m.category,m.matched_at,m.pre_at,r.size,r.complete,c.category
            from recent m join releases r on r.id=m.release_id left join release_category c on c.release_id=r.id order by m.matched_at desc,m.release_id desc",&[])?;
        ensure!(rows.len() as i64==count,"native PreDB display changed within snapshot");
        let predb=rows.into_iter().map(|row|Ok(PredbRow {id:row.try_get(0)?,name:row.try_get(1)?,kind:row.try_get(2)?,category:row.try_get(3)?,
            matched_at:row.try_get(4)?,pre_at:row.try_get(5)?,size:row.try_get(6)?,complete:row.try_get(7)?,atlas_category:row.try_get::<_,Option<i64>>(8)?.unwrap_or(crate::category::UNCATEGORIZED)})).collect::<Result<Vec<_>>>()?;
        let seconds=began.elapsed().as_secs_f64();
        Ok(Stored {at:now as f64,secs:seconds,day,day_total:total,day_every:1,day_at:now as f64,day_secs:seconds,day_unindexed:0,predb,errors:Vec::new()})
    }
}

#[cfg(test)]
mod posting_clock_tests {
    use super::*;
    use crate::{parser::{Article,Release},pg::{self,fresh_reader::Budget}};
    use std::time::Duration;

    #[test]
    #[ignore="requires explicit isolated PostgreSQL and the actual posted_clock/posted_at writer"]
    fn day_statistics_use_poster_wall_clock_across_opposite_offsets() {
        let schema=pg::testing::Schema::new().expect("explicit isolated PostgreSQL");let mut client=schema.pool.get().unwrap();
        {
            let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
            let empty=view.stored_statistics(1577923200).unwrap();assert_eq!(empty.day_total,0);assert!(empty.day.is_empty()&&empty.predb.is_empty());
        }
        let releases=[("East.Clock.Example","Wed, 01 Jan 2020 00:00:00 +1200",17),
            ("West.Clock.Example","Wed, 01 Jan 2020 00:00:00 -1200",23),
            ("Old.Clock.Example","Tue, 31 Dec 2019 23:59:59 +0000",99)].into_iter().enumerate().map(|(n,(name,date,bytes))|Release {
                name:name.into(),date:date.into(),poster:"poster".into(),group:"alt.binaries.test".into(),
                articles:vec![Article {filename:Some("video.mkv".into()),subject:"video.mkv yEnc (1/1)".into(),message_id:format!("clock-stats-{n}@example.invalid"),
                    part:Some(1),total_parts:Some(1),bytes,..Default::default()}],..Default::default()
            }).collect::<Vec<_>>();
        assert_eq!(pg::release_writer::save(&mut client,&releases,1).unwrap().releases,3);
        client.execute("insert into release_category(release_id,category) select id,2040 from releases",&[]).unwrap();
        let clocks=client.query("select name,posted_clock,posted_at from releases where name<>'Old.Clock.Example' order by name",&[]).unwrap();
        assert_eq!((clocks[0].get::<_,i64>(1),clocks[0].get::<_,i64>(2)),(1577836800,1577793600));
        assert_eq!((clocks[1].get::<_,i64>(1),clocks[1].get::<_,i64>(2)),(1577836800,1577880000));
        {
            let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
            // Legacy normalizes posting text without its zone and compares
            // it against "2020-01-01 00:00:00" at this fixed UTC cutoff.
            let stored=view.stored_statistics(1577923200).unwrap();assert_eq!(stored.day_total,2);
            let movies=stored.day.get(&2040).unwrap();assert_eq!((movies.count,movies.complete,movies.bytes),(2,2,40));
        }
        client.execute("update releases set posted_clock=null where name='East.Clock.Example'",&[]).unwrap();
        let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
        assert!(view.stored_statistics(1577923200).is_err(),"unknown wall clock must not become an invented day count");
    }
}
