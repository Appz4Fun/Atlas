//! Native statistics consume completed PostgreSQL observations. Each original
//! counter is compared before any floating-point or family-wide reduction.

use anyhow::{Result,Context,ensure};
use std::{collections::{HashMap,HashSet},sync::Arc};
use sha2::{Digest,Sha256};
use crate::web_stats::{Window,Scrape,Labels,ComputedWindow};

const MIN_NS:i64=30_000_000_000;
const MAX_NS:i64=300_000_000_000;
#[derive(Clone,Copy)]
enum Value {Counter(u64),Gauge(f64),Histogram {count:u64,sum:f64}}
impl Value {fn kind(self)->u8 {match self {Self::Counter(_)=>1,Self::Gauge(_)=>2,Self::Histogram {..}=>3}}}
#[derive(Clone)]
struct Raw {key:[u8;32],family:String,labels:String,value:Value}
fn series_key(family:&str,labels:&str,kind:u8)->[u8;32] {
    let mut hash=Sha256::new();hash.update(b"atlas-native-original-series-v1");hash.update((family.len() as u64).to_le_bytes());hash.update(family.as_bytes());
    hash.update((labels.len() as u64).to_le_bytes());hash.update(labels.as_bytes());hash.update([kind]);hash.finalize().into()
}
fn charge(left:&mut u64,bytes:u64)->Result<()> {*left=left.checked_sub(bytes).context("native statistics ownership exceeds admission")?;Ok(())}
fn validate(row:&Raw)->Result<()> {
    ensure!(!row.family.is_empty()&&row.family.len()<=256&&row.labels.len()<=16384,"native metric identity exceeds admission");
    ensure!(series_key(&row.family,&row.labels,row.value.kind())==row.key,"native original-series identity changed");
    match row.value {Value::Gauge(value)=>ensure!(value.is_finite(),"nonfinite native gauge"),Value::Histogram {sum,..}=>ensure!(sum.is_finite()&&sum>=0.0,"invalid native histogram sum"),_=>{}}Ok(())
}
struct Series {family:String,labels:Labels,current:Value,previous:Option<Value>,active:bool,elapsed:i64,count:u128,sum:f64,gauge:f64}
struct Reducer {series:Vec<Series>,indices:HashMap<[u8;32],usize>,now:i64,previous:i64,at:f64,frames:usize}
impl Reducer {
    fn new(rows:Vec<Raw>,mono:i64,at:f64,left:&mut u64)->Result<Self> {
        ensure!(rows.len()<=10000&&mono>=0&&at.is_finite(),"invalid native observation extent");
        let mut series=Vec::with_capacity(rows.len());let mut indices=HashMap::with_capacity(rows.len());
        for row in rows {
            validate(&row)?;
            // Admit pair/vector growth and both current/rate presentation
            // copies before the existing label parser allocates decoded text.
            let pairs=row.labels.bytes().filter(|byte|*byte==b'=').count() as u64;
            charge(left,(row.labels.len() as u64*8).checked_add(pairs*384).and_then(|n|n.checked_add(row.family.len() as u64*8+1024)).context("native label extent overflow")?)?;
            let labels=crate::web_stats::parse_labels(&row.labels).context("invalid captured metric labels")?;
            let mut keys=HashSet::new();ensure!(labels.iter().all(|(key,_)|!key.is_empty()&&keys.insert(key)),"duplicate or empty native metric label");drop(keys);
            ensure!(indices.insert(row.key,series.len()).is_none(),"duplicate native original series");
            series.push(Series {family:row.family,labels,current:row.value,previous:Some(row.value),active:true,elapsed:0,count:0,sum:0.0,gauge:0.0});
        }
        Ok(Self {series,indices,now:mono,previous:mono,at,frames:1})
    }
    fn older(&mut self,rows:Vec<Raw>,mono:i64)->Result<()> {
        ensure!(self.frames<32&&mono>=0&&mono<self.previous&&self.now-mono<=MAX_NS,"invalid native observation order");
        let elapsed=self.previous-mono;self.frames+=1;self.previous=mono;
        let mut old=vec![None;self.series.len()];let mut seen=HashSet::with_capacity(rows.len());
        for row in rows {
            validate(&row)?;ensure!(seen.insert(row.key),"duplicate native prior series");
            if let Some(index)=self.indices.get(&row.key){old[*index]=Some(row.value);}
        }
        for (series,older)in self.series.iter_mut().zip(old) {
            if !series.active{continue;}
            let(Some(newer),Some(older))=(series.previous,older)else{series.active=false;continue;};
            match(newer,older) {
                (Value::Counter(newer),Value::Counter(older))=>series.count=series.count.checked_add(u128::from(if newer>=older{newer-older}else{newer})).context("native counter delta overflow")?,
                (Value::Gauge(newer),Value::Gauge(older))=>series.gauge+=newer-older,
                (Value::Histogram {count:newer,sum:new_sum},Value::Histogram {count:older,sum:old_sum})=>{
                    let reset=newer<older||new_sum<old_sum;
                    series.count=series.count.checked_add(u128::from(if reset{newer}else{newer-older})).context("native histogram delta overflow")?;
                    series.sum+=if reset{new_sum}else{new_sum-old_sum};
                },_=>anyhow::bail!("native metric kind changed within series"),
            }
            ensure!(series.gauge.is_finite()&&series.sum.is_finite(),"native accumulated metric is nonfinite");
            series.previous=Some(older);series.elapsed=series.elapsed.checked_add(elapsed).context("native series clock overflow")?;
        }Ok(())
    }
    fn finish(self)->Result<Window> {
        let mut current=Scrape {at:self.at,..Default::default()};let mut rates=Scrape {at:self.at,..Default::default()};
        let mut changes=HashMap::new();let mut unknown_gauges=HashSet::new();let mut slots=HashSet::new();
        let mut insert=|family:String,labels:Labels,value:f64,rate:Option<f64>|->Result<()> {
            ensure!(value.is_finite()&&rate.is_none_or(f64::is_finite),"native metric reduction is nonfinite");
            ensure!(slots.insert((family.clone(),labels.clone())),"ambiguous native metric output identity");
            if let Some(rate)=rate{rates.series.entry(family.clone()).or_default().push((labels.clone(),rate));}
            current.series.entry(family).or_default().push((labels,value));Ok(())
        };
        for series in self.series {
            let seconds=(series.elapsed>=MIN_NS).then_some(series.elapsed as f64/1_000_000_000.0);
            match series.current {
                Value::Counter(value)=>insert(series.family,series.labels,value as f64,seconds.map(|seconds|series.count as f64/seconds))?,
                Value::Gauge(value)=>{
                    if let Some(seconds)=seconds{*changes.entry(series.family.clone()).or_insert(0.0)+=series.gauge/seconds;}else{unknown_gauges.insert(series.family.clone());}
                    insert(series.family,series.labels,value,None)?;
                },
                Value::Histogram {count,sum}=>{
                    insert(format!("{}_count",series.family),series.labels.clone(),count as f64,seconds.map(|seconds|series.count as f64/seconds))?;
                    insert(format!("{}_sum",series.family),series.labels,sum,seconds.map(|seconds|series.sum/seconds))?;
                },
            }
        }
        for family in unknown_gauges{changes.remove(&family);}
        let window=ComputedWindow {from:(self.frames>1).then_some(self.previous as f64/1_000_000_000.0),to:self.now as f64/1_000_000_000.0,
            rates:Arc::new(rates),changes,lag:None};
        Ok(Window {now:Some(Arc::new(current)),computed:Some(window),..Default::default()})
    }
}

pub(crate) struct NativeStatistics {
    pub window:Window,pub pace:crate::web_pending::NativePace,pub catalog:crate::web_stats::NativeCatalog,
    pub predb:crate::web_stats::NativePredb,pub now:f64,
}
struct Observation {id:i64,process:[u8;32],at:i64,mono:i64}
impl super::fresh_reader::ReadView<'_> {
    pub(crate) fn live_statistics(&self)->Result<NativeStatistics> {
        use crate::web_stats::{NativeCatalog,NativeFeed,NativePredb};
        self.before_query()?;
        let owner=super::fresh_live::RuntimeObservation::read_in(&mut *self.tx.borrow_mut())?.context("native statistics owner is unavailable")?;
        self.before_query()?;
        let catalog=super::stage::CatalogIdentity::read_in(&mut *self.tx.borrow_mut())?;
        let now=self.statistics_clock()?;let now_ms=now.checked_mul(1000).context("native statistics clock overflow")?;
        self.before_query()?;
        let head:i64=self.tx.borrow_mut().query_opt("select o.observation_id from native_runtime_observation_current h
            join native_runtime_observations o on o.observation_id=h.observation_id
            where h.singleton and o.epoch=$1 and o.catalog_nonce=$2",&[&&owner.epoch()[..],&&catalog.nonce()[..]])?
            .context("native completed observation is unavailable")?.try_get(0)?;
        self.before_query()?;
        let metadata=self.tx.borrow_mut().query("select observation_id,process_nonce,observed_at_ms,monotonic_ns from native_runtime_observations
            where catalog_nonce=$1 and observation_id<=$2 order by observation_id desc limit 32",&[&&catalog.nonce()[..],&head])?;
        let mut observations:Vec<Observation>=Vec::new();
        for row in metadata {
            let process:Vec<u8>=row.try_get(1)?;let process:[u8;32]=process.try_into().map_err(|_|anyhow::anyhow!("native process identity extent"))?;
            let item=Observation {id:row.try_get(0)?,process,at:row.try_get(2)?,mono:row.try_get(3)?};
            ensure!(item.id>0&&item.mono>=0&&item.process!=[0;32],"invalid native observation identity");
            if let Some(current)=observations.first() {
                if item.process!=current.process||current.mono-item.mono>MAX_NS{break;}
                let previous=observations.last().unwrap();ensure!(item.id<previous.id&&item.mono<previous.mono,"native observation order changed");
            }else{ensure!(item.id==head&&item.at>0&&item.at<=now_ms+5000&&now_ms.saturating_sub(item.at)<120000,"native statistics heartbeat is stale");}
            observations.push(item);
        }
        let current=observations.first().context("native observation head disappeared")?;
        let ids=observations.iter().map(|observation|observation.id).collect::<Vec<_>>();
        self.before_query()?;
        let extents=self.tx.borrow_mut().query("select observation_id,count(*),coalesce(sum(octet_length(family)::bigint+octet_length(labels)::bigint),0)::bigint
            from native_metric_samples where observation_id=any($1) group by observation_id",&[&ids])?;
        let mut counts=HashMap::new();let mut max_rows=0u64;let mut max_bytes=0u64;
        for row in extents {
            let id:i64=row.try_get(0)?;let count:i64=row.try_get(1)?;let bytes:i64=row.try_get(2)?;
            ensure!(ids.contains(&id)&&(0..=10000).contains(&count)&&(0..=8*1024*1024).contains(&bytes),"native metric frame exceeds producer admission");
            max_rows=max_rows.max(count as u64);max_bytes=max_bytes.max(bytes as u64);
            ensure!(counts.insert(id,(count,bytes)).is_none(),"duplicate native metric frame extent");
        }
        // Reserve maximum transport/old-frame workspace once. Intermediate
        // frames are consumed and dropped, never retained as32 full snapshots.
        self.admit_owned(131072u64.checked_add(max_bytes*4).and_then(|n|n.checked_add(max_rows*1024)).context("native statistics workspace overflow")?)?;
        let available=self.remaining_owned();let mut left=available;
        let rows=self.metric_frame(current.id,*counts.get(&current.id).unwrap_or(&(0,0)))?;
        let mut reducer=Reducer::new(rows,current.mono,current.at as f64/1000.0,&mut left)?;
        for observation in observations.iter().skip(1) {
            reducer.older(self.metric_frame(observation.id,*counts.get(&observation.id).unwrap_or(&(0,0)))?,observation.mono)?;
        }
        let window=reducer.finish()?;self.admit_owned(available.checked_sub(left).context("native label ownership accounting overflow")?)?;
        let pace=self.processing_pace(Some(&owner),now)?;
        self.before_query()?;
        let row=self.tx.borrow_mut().query_one("select count(*),count(*) filter(where r.complete),count(*) filter(where r.display_name is not null),
            count(*) filter(where p.release_id is not null),count(*) filter(where r.is_obfuscated and r.display_name is null),coalesce(sum(r.size),0)::bigint
            from releases r left join predb_match p on p.release_id=r.id",&[])?;
        let catalog=NativeCatalog {total:row.try_get(0)?,complete:row.try_get(1)?,named:row.try_get(2)?,matched:row.try_get(3)?,obfuscated:row.try_get(4)?,bytes:row.try_get(5)?};
        ensure!(catalog.total>=0&&[catalog.complete,catalog.named,catalog.matched,catalog.obfuscated].into_iter().all(|count|(0..=catalog.total).contains(&count))&&catalog.bytes>=0,"invalid native catalog statistics");
        self.before_query()?;
        let row=self.tx.borrow_mut().query_one("select count(*),case when (select value from ext_predb_meta where key='late_after_id') is not null
            then count(*) filter(where id>(select value from ext_predb_meta where key='late_after_id')) else null::bigint end from ext_predb_titles",&[])?;
        let titles:i64=row.try_get(0)?;let behind:Option<i64>=row.try_get(1)?;ensure!(titles>=0&&behind.is_none_or(|behind|(0..=titles).contains(&behind)),"invalid retained PreDB statistics");
        self.before_query()?;
        let extent=self.tx.borrow_mut().query_one("select count(*),coalesce(sum(octet_length(key)::bigint+coalesce(octet_length(last_error),0)::bigint),0)::bigint from ext_predb_feeds where failures>0",&[])?;
        let count:i64=extent.try_get(0)?;let bytes:i64=extent.try_get(1)?;ensure!(count>=0&&bytes>=0,"negative native feed extent");
        let overhead=(count as u64).checked_mul(1024).and_then(|n|n.checked_add(4096)).context("native feed row extent overflow")?;
        self.admit_owned((bytes as u64).checked_mul(32).and_then(|n|n.checked_add(overhead)).context("native feed display extent overflow")?)?;
        self.before_query()?;
        let rows=self.tx.borrow_mut().query("select key,failures,last_ok_at,last_error from ext_predb_feeds where failures>0 order by source",&[])?;
        ensure!(rows.len() as i64==count,"native feed inventory changed");
        let feeds=rows.into_iter().map(|row|Ok(NativeFeed {key:row.try_get(0)?,failures:row.try_get(1)?,last_ok:row.try_get(2)?,error:row.try_get(3)?})).collect::<Result<Vec<_>>>()?;
        self.before_query()?;owner.verify_after(&mut *self.tx.borrow_mut())?;
        Ok(NativeStatistics {window,pace,catalog,predb:NativePredb {titles,behind,feeds},now:now as f64})
    }
    fn metric_frame(&self,id:i64,extent:(i64,i64))->Result<Vec<Raw>> {
        use postgres::fallible_iterator::FallibleIterator;
        self.before_query()?;let mut tx=self.tx.borrow_mut();
        let mut rows=tx.query_raw("select series_key,family,labels,kind,counter_bits,gauge_bits,histogram_count_bits,histogram_sum_bits
            from native_metric_samples where observation_id=$1 order by series_key",[&id])?;
        let mut values=Vec::with_capacity(usize::try_from(extent.0)?);let mut bytes=0i64;
        while let Some(row)=rows.next()? {
            self.check_deadline()?;ensure!((values.len() as i64)<extent.0,"native metric row extent changed");
            let key:Vec<u8>=row.try_get(0)?;let key:[u8;32]=key.try_into().map_err(|_|anyhow::anyhow!("native metric key extent"))?;
            let family:String=row.try_get(1)?;let labels:String=row.try_get(2)?;let kind:i16=row.try_get(3)?;
            bytes=bytes.checked_add(i64::try_from(family.len()+labels.len())?).context("native metric extent overflow")?;
            let(c,g,n,s):(Option<i64>,Option<i64>,Option<i64>,Option<i64>)=(row.try_get(4)?,row.try_get(5)?,row.try_get(6)?,row.try_get(7)?);
            let value=match(kind,c,g,n,s) {
                (1,Some(counter),None,None,None)=>Value::Counter(counter as u64),
                (2,None,Some(bits),None,None)=>Value::Gauge(f64::from_bits(bits as u64)),
                (3,None,None,Some(count),Some(sum))=>Value::Histogram {count:count as u64,sum:f64::from_bits(sum as u64)},
                _=>anyhow::bail!("native metric payload shape is invalid"),
            };
            values.push(Raw {key,family,labels,value});
        }
        ensure!(values.len() as i64==extent.0&&bytes==extent.1,"native metric frame changed within snapshot");Ok(values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(group:&str,value:Value)->Raw {
        let family=if matches!(value,Value::Histogram {..}){"duration_seconds"}else{"articles_total"};
        let labels=format!("group=\"{group}\"");
        Raw {key:series_key(family,&labels,value.kind()),family:family.into(),labels,value}
    }
    fn rate(window:&Window,family:&str,group:&str)->Option<f64> {
        window.computed.as_ref()?.rates.series.get(family)?.iter()
            .find(|(labels,_)|labels.iter().any(|(key,value)|key=="group"&&value==group)).map(|(_,value)|*value)
    }

    #[test]
    fn original_series_integer_deltas_resets_and_new_labels_precede_float_reduction() {
        let large=(1u64<<54)+1;
        let mut reducer=Reducer::new(vec![sample("large",Value::Counter(large+42)),sample("reset",Value::Counter(5)),sample("new",Value::Counter(9999))],
            60_000_000_000,2000.0,&mut (16*1024*1024)).unwrap();
        reducer.older(vec![sample("large",Value::Counter(large+20)),sample("reset",Value::Counter(3))],30_000_000_000).unwrap();
        reducer.older(vec![sample("large",Value::Counter(large)),sample("reset",Value::Counter(100))],0).unwrap();
        let window=reducer.finish().unwrap();
        assert_eq!(rate(&window,"articles_total","large"),Some(0.7));
        assert_eq!(rate(&window,"articles_total","reset"),Some(5.0/60.0));
        assert_eq!(rate(&window,"articles_total","new"),None,"one observation is not a zero or a known rate");
        assert!(window.then.is_none()&&window.chain.is_empty(),"native reductions do not fabricate counter samples");
    }

    #[test]
    fn histogram_resets_and_incomplete_series_do_not_cross_unobserved_intervals() {
        let mut reducer=Reducer::new(vec![sample("h",Value::Histogram {count:3,sum:1.5}),sample("gap",Value::Counter(50))],60_000_000_000,2000.0,&mut (16*1024*1024)).unwrap();
        reducer.older(vec![sample("h",Value::Histogram {count:1,sum:0.5})],30_000_000_000).unwrap();
        reducer.older(vec![sample("h",Value::Histogram {count:100,sum:50.0}),sample("gap",Value::Counter(10))],0).unwrap();
        let window=reducer.finish().unwrap();
        assert_eq!(rate(&window,"duration_seconds_count","h"),Some(3.0/60.0));
        assert_eq!(rate(&window,"duration_seconds_sum","h"),Some(1.5/60.0));
        assert_eq!(rate(&window,"articles_total","gap"),None);
    }

    #[test]
    fn malformed_labels_duplicate_series_and_short_windows_cannot_produce_measured_rates() {
        let mut row=sample("a",Value::Counter(1));row.labels="group=\"unterminated".into();row.key=series_key(&row.family,&row.labels,row.value.kind());
        assert!(Reducer::new(vec![row],0,1.0,&mut (16*1024*1024)).is_err());
        let row=sample("a",Value::Counter(1));
        assert!(Reducer::new(vec![row.clone(),row],0,1.0,&mut (16*1024*1024)).is_err());
        let mut reducer=Reducer::new(vec![sample("a",Value::Counter(5))],29_000_000_000,30.0,&mut (16*1024*1024)).unwrap();
        reducer.older(vec![sample("a",Value::Counter(1))],0).unwrap();
        assert_eq!(rate(&reducer.finish().unwrap(),"articles_total","a"),None);
        assert!(Reducer::new(vec![sample("a",Value::Counter(5))],0,1.0,&mut 32).is_err());
    }
}

#[cfg(test)]
mod pg_observation_tests {
    use super::*;
    use crate::{config::{Config,HeaderStoreConfig},pg::{self,fresh_reader::{ReadView,Budget}}};
    use std::time::{Duration,Instant};

    #[test]
    #[ignore="requires isolated PG, original canonical owner and a separately admitted 31-second actual observation window"]
    fn live_reader_uses_real_registry_capture_and_preserves_large_counter_delta() {
        let schema=pg::testing::Schema::new().expect("explicit isolated PostgreSQL");let root=tempfile::tempdir().unwrap();
        let mut config=Config::default();config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;
        config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;
        let storage=pg::StorageConfig {backend:pg::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:1,allow_plaintext:true};
        let headers=HeaderStoreConfig {enabled:true,dir:root.path().join("headers").to_str().unwrap().into()};
        let runtime=pg::indexer_runtime::Runtime::open(&config,storage,headers,&[],&mut None).unwrap();
        let mut client=schema.pool.get().unwrap();
        {
            let view=ReadView::open(&mut client,&schema.name,Budget {rows:100,bytes:64*1024*1024},Instant::now()+Duration::from_secs(10)).unwrap();
            assert!(view.live_statistics().is_err(),"missing observation head is unavailable, never an empty fabricated snapshot");
        }
        let group="alt.binaries.native-statistics-fixture";
        crate::metrics::names::ARTICLES.inc(&[("group",group)],(1u64<<63)+1);
        pg::stage_telemetry::capture(&runtime.owner,None).unwrap();
        {
            let view=ReadView::open(&mut client,&schema.name,Budget {rows:100,bytes:64*1024*1024},Instant::now()+Duration::from_secs(10)).unwrap();
            let facts=view.live_statistics().unwrap();assert!(facts.window.computed.as_ref().unwrap().rates.series.get("atlas_articles_total").is_none());
            assert_eq!(facts.catalog.total,0,"fresh empty catalog is an actual SQL result");
        }
        runtime.owner.with(|owner|{
            owner.execute("insert into ext_predb_titles(id,title,source,added_at) values(1,'Cursor.First.Example',1,1),(2,'Cursor.Second.Example',1,1)",&[])?;
            owner.execute("delete from ext_predb_meta where key='late_after_id'",&[])?;Ok(())
        }).unwrap();
        {
            let view=ReadView::open(&mut client,&schema.name,Budget {rows:100,bytes:64*1024*1024},Instant::now()+Duration::from_secs(10)).unwrap();
            assert_eq!(view.live_statistics().unwrap().predb.behind,None,"absent cursor is not a zero backlog");
        }
        for (cursor,want) in [(None,None),(Some(0i64),Some(2i64)),(Some(1),Some(1)),(Some(2),Some(0))] {
            runtime.owner.with(|owner|{owner.execute("insert into ext_predb_meta(key,value) values('late_after_id',$1) on conflict(key) do update set value=excluded.value",&[&cursor])?;Ok(())}).unwrap();
            let view=ReadView::open(&mut client,&schema.name,Budget {rows:100,bytes:64*1024*1024},Instant::now()+Duration::from_secs(10)).unwrap();
            let facts=view.live_statistics().unwrap();assert_eq!(facts.predb.titles,2);assert_eq!(facts.predb.behind,want,"actual nullable cursor must retain its meaning");
        }
        std::thread::sleep(Duration::from_secs(31));
        crate::metrics::names::ARTICLES.inc(&[("group",group)],42);
        pg::stage_telemetry::capture(&runtime.owner,None).unwrap();
        let elapsed:i64=client.query_one("select max(monotonic_ns)-min(monotonic_ns) from native_runtime_observations",&[]).unwrap().get(0);
        {
            let view=ReadView::open(&mut client,&schema.name,Budget {rows:100,bytes:64*1024*1024},Instant::now()+Duration::from_secs(10)).unwrap();
            let facts=view.live_statistics().unwrap();
            let rate=facts.window.computed.as_ref().unwrap().rates.series["atlas_articles_total"].iter()
                .find(|(labels,_)|labels.iter().any(|(key,value)|key=="group"&&value==group)).unwrap().1;
            assert!((rate*(elapsed as f64/1_000_000_000.0)-42.0).abs()<0.000001,"counter deltas must be exact before float conversion");
            let html=crate::web_stats::native_page_from_facts(&facts);
            assert!(html.contains("<footer class=\"site-footer\" role=\"contentinfo\">"));
            assert!(html.contains("https://www.patreon.com/cw/xbmc4lyfe")&&html.contains("PostgreSQL"));
            assert!(!html.contains("stats.json")&&!html.contains("Reading the shards"));
        }
        let pid=runtime.owner.with(|owner|Ok(owner.query_one("select pg_backend_pid()",&[])?.get::<_,i32>(0))).unwrap();
        runtime.owner.close();
        let stopped=Instant::now()+Duration::from_secs(2);
        loop {
            client.query_one("select pg_stat_clear_snapshot()",&[]).unwrap();
            if !client.query_one("select exists(select 1 from pg_stat_activity where pid=$1)",&[&pid]).unwrap().get::<_,bool>(0){break;}
            assert!(Instant::now()<stopped,"original owner backend must finish closing");std::thread::sleep(Duration::from_millis(5));
        }
        let view=ReadView::open(&mut client,&schema.name,Budget {rows:100,bytes:64*1024*1024},Instant::now()+Duration::from_secs(10)).unwrap();
        assert!(view.live_statistics().is_err(),"persisted epoch/head cannot replace actual current owner liveness");
    }
}
