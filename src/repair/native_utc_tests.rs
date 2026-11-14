//! Real accepted native headers and RR repair reads; no provider or fabricated
//! timing producer. The migration control deliberately removes only new UTC
//! fields from genuine rows to reproduce the pre-upgrade schema.
use super::*;
use crate::{nntp::Overview,parser::Release};
const GROUP:&str="alt.binaries.native.repair.utc";
const NOW:i64=1_767_272_400;

struct Fixture {
    runtime:crate::pg::indexer_runtime::Runtime,
    schema:crate::pg::testing::Schema,
    root:tempfile::TempDir,
}
fn runtime(schema:&crate::pg::testing::Schema,root:&std::path::Path)->crate::pg::indexer_runtime::Runtime {
    let mut config=crate::config::Config::default();
    config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;
    config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;
    crate::pg::indexer_runtime::Runtime::open(&config,crate::pg::StorageConfig {
        backend:crate::pg::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:1,allow_plaintext:true,
    },crate::config::HeaderStoreConfig {enabled:true,dir:root.to_str().unwrap().into()},&[],&mut None).unwrap()
}
impl Fixture {
    fn new()->Self {
        let schema=crate::pg::testing::Schema::new().expect("explicit isolated ATLAS_PG_TEST_URL required");
        let root=tempfile::tempdir().unwrap();let runtime=runtime(&schema,root.path());Self {runtime,schema,root}
    }
    fn release(tag:&str,dates:&[&str])->Release {
        let file=format!("Owned.UTC.{tag}.2160p.Remux.mkv");
        let headers=dates.iter().enumerate().map(|(at,date)|Overview {
            number:at as u64+1,subject:format!("\"{file}\" yEnc ({}/20)",at+1),from:"UTC fixture poster".into(),
            date:(*date).into(),message_id:format!("<{tag}.{}@utc.fixture.test>",at+1),references:String::new(),
            bytes:64*1024*1024,lines:10,xref_groups:Some(1),
        }).collect();
        let rows=crate::collections::group_articles(GROUP,crate::nntp::headers_to_articles(headers));
        assert_eq!(rows.len(),1);let mut row=rows.into_values().next().unwrap();
        row.group=GROUP.into();row.poster="UTC fixture poster".into();row.date=row.articles[0].date.clone();row
    }
    fn save(&self,row:&Release)->i64 {
        self.runtime.owner.with(|client| {
            crate::pg::release_writer::save(client,std::slice::from_ref(row),NOW)?;
            Ok(client.query_one("select id from releases where group_name=$1 and original_name=$2",&[&GROUP,&row.name.as_bytes()])?.get(0))
        }).unwrap()
    }
    fn windows(&self,ids:&[i64])->Option<Vec<Window>> {test_snapshot(&self.runtime.owner,|r|r.windows(ids)).unwrap()}
    fn timing(&self,id:i64)->(i64,i64,Option<i64>,Option<i64>,bool,i64) {
        self.runtime.owner.with(|client| {
            let row=client.query_one("select first_date,last_date,first_utc,last_utc,utc_complete,articles from file_timing where release_id=$1",&[&id])?;
            Ok((row.get(0),row.get(1),row.get(2),row.get(3),row.get(4),row.get(5)))
        }).unwrap()
    }
}
impl Drop for Fixture {fn drop(&mut self){self.runtime.owner.close();}}

#[test]
#[ignore="explicit isolated PostgreSQL; genuine accepted header timing and native RR windows"]
fn mixed_offsets_use_utc_extrema_without_changing_display_or_replay() {
    let f=Fixture::new();let row=Fixture::release("Mixed",&[
        "Thu, 01 Jan 2026 10:00:00 +0200","Thu, 01 Jan 2026 02:00:00 -0700"]);
    let id=f.save(&row);let before=f.timing(id);
    assert_eq!(before,(1_767_232_800,1_767_261_600,Some(1_767_254_400),Some(1_767_258_000),true,2));
    let windows=f.windows(&[id]).expect("explicit UTC headers must qualify");assert_eq!(windows.len(),1);
    assert_eq!((&*windows[0].group,windows[0].from,windows[0].to),(GROUP,1_767_254_100,1_767_258_300));
    // Neither a duplicate MID nor its changed Date may become new UTC proof.
    let mut replay=row.clone();for article in &mut replay.articles {article.date="Fri, 01 Jan 2044 00:00:00 +0000".into();}
    f.save(&replay);assert_eq!(f.timing(id),before);
    // UTC extrema must merge across later accepted batches, not just one COPY.
    let mut later=row.clone();later.articles.truncate(1);let article=&mut later.articles[0];
    article.message_id="<Mixed.third@utc.fixture.test>".into();article.part=Some(3);article.number=3;
    article.subject=article.subject.replace("(1/20)","(3/20)");article.date="Thu, 01 Jan 2026 00:30:00 -0700".into();
    f.save(&later);assert_eq!(f.timing(id),(1_767_227_400,1_767_261_600,Some(1_767_252_600),Some(1_767_258_000),true,3));
    let windows=f.windows(&[id]).unwrap();assert_eq!((windows[0].from,windows[0].to),(1_767_252_300,1_767_258_300));
}

#[test]
#[ignore="explicit isolated PostgreSQL; unknown UTC remains unqualified and cannot starve later candidates"]
fn unknown_utc_is_sticky_and_does_not_pin_repair_candidate_pagination() {
    let f=Fixture::new();let dates=vec!["Thu, 01 Jan 2026 10:00:00 +0200";19];
    let known_row=Fixture::release("Known",&dates);let known=f.save(&known_row);
    let unknown_row=Fixture::release("Unknown",&vec!["Thu, 01 Jan 2026 10:00:00 -0000";19]);
    let unknown=f.save(&unknown_row);assert!(unknown>known,"unknown anchor must be visited first in descending scan");
    assert!(f.windows(&[unknown]).is_none());assert!(f.windows(&[known,unknown]).is_none());
    let plan=storage::candidate_plan(&f.runtime.owner,NOW,&mut Service::budget()).unwrap();
    assert!(plan.found.posts.iter().any(|p|p.ids.contains(&known)),"unknown UTC must not pin pagination ahead of known headers");
    assert!(plan.found.posts.iter().all(|p|!p.ids.contains(&unknown)));
    f.runtime.owner.with(|client| {assert_eq!(client.query_one("select count(*) from repair_tried where release_id=$1",&[&unknown])?.get::<_,i64>(0),0);Ok(())}).unwrap();
    let mut later=unknown_row.clone();later.articles.truncate(1);let article=&mut later.articles[0];
    article.message_id="<Unknown.new@utc.fixture.test>".into();article.date="Thu, 01 Jan 2026 10:01:00 +0200".into();
    // Keep one missing part so this assertion tests UTC qualification rather
    // than complete-post exclusion; same part with a new MID is an observation.
    f.save(&later);assert!(!f.timing(unknown).4);assert!(f.windows(&[unknown]).is_none());
    let partial=f.save(&Fixture::release("PartlyKnown",&["Thu, 01 Jan 2026 10:00:00 +0200","Thu, 01 Jan 2026 10:00:00 -0000"]));
    assert_eq!(f.timing(partial).2,Some(1_767_254_400));assert!(!f.timing(partial).4);assert!(f.windows(&[partial]).is_none());
    let mut later=known_row.clone();later.articles.truncate(1);later.articles[0].message_id="<Known.unknown@utc.fixture.test>".into();
    later.articles[0].date="Thu, 01 Jan 2026 10:01:00 -0000".into();
    f.save(&later);assert!(!f.timing(known).4,"one new unknown observation invalidates previously complete UTC");
    assert!(f.windows(&[known]).is_none());
}

#[test]
#[ignore="explicit isolated PostgreSQL; migrate genuine old timing without guessing a historical zone"]
fn migration_preserves_historical_wallclock_and_keeps_unknown_utc_sticky() {
    let mut f=Fixture::new();let row=Fixture::release("Legacy",&["Thu, 01 Jan 2026 10:00:00 +0200"]);
    let id=f.save(&row);let old=f.timing(id);let pid=f.runtime.owner.with(|c|Ok(c.query_one("select pg_backend_pid()",&[])?.get::<_,i32>(0))).unwrap();
    f.runtime.owner.close();
    {
        let mut client=f.schema.pool.get().unwrap();let until=Instant::now()+Duration::from_secs(5);
        while client.query_one("select exists(select 1 from pg_stat_activity where pid=$1)",&[&pid]).unwrap().get::<_,bool>(0) {
            assert!(Instant::now()<until,"actual original owner session did not close");std::thread::sleep(Duration::from_millis(10));
        }
        // Keep the genuine old columns/rows and remove only the additive13
        // schema, reproducing a retained catalog immediately before upgrade.
        client.batch_execute("alter table file_timing drop constraint file_timing_utc_bounds;
            alter table file_timing drop column first_utc,drop column last_utc,drop column utc_complete;
            delete from schema_migrations where version=13").unwrap();
        assert_eq!(crate::pg::migrate::run(&mut client,&f.schema.name).unwrap(),vec![13]);
        let got=client.query_one("select first_date,last_date,first_utc,last_utc,utc_complete,articles from file_timing where release_id=$1",&[&id]).unwrap();
        assert_eq!((got.get::<_,i64>(0),got.get::<_,i64>(1),got.get::<_,i64>(5)),(old.0,old.1,old.5));
        assert!(got.get::<_,Option<i64>>(2).is_none()&&got.get::<_,Option<i64>>(3).is_none());assert!(!got.get::<_,bool>(4));
    }
    f.runtime=runtime(&f.schema,f.root.path());assert!(f.windows(&[id]).is_none());
    let mut later=row.clone();let article=&mut later.articles[0];article.message_id="<Legacy.new@utc.fixture.test>".into();
    article.part=Some(2);article.subject=article.subject.replace("(1/20)","(2/20)");
    f.save(&later);assert!(!f.timing(id).4,"new known headers cannot retroactively prove old UTC");assert!(f.windows(&[id]).is_none());
}
