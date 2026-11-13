//! TEST ONLY: current production parser/grouping/writer APIs commit real rows.
//! The unmodified baseline is expected to refuse the assertion because its
//! accepted rows have no file_timing producer. No new production API is used.
use super::*;
use crate::nntp::Overview;

const GROUP:&str="alt.binaries.native.timing";
const POSTER_A:&str="Actual poster A <a@fixture.test>";
const POSTER_B:&str="Actual poster B <b@fixture.test>";
fn header(file:&str,id:&str,part:i64,total:i64,number:u64,date:&str,from:&str,lines:i64,references:&str,xref:u16)->Overview {
    Overview {number,subject:format!("\"{file}\" yEnc ({part}/{total})"),from:from.into(),date:date.into(),message_id:id.into(),
        references:references.into(),bytes:100,lines,xref_groups:Some(xref)}
}
fn parsed(headers:Vec<Overview>)->Release {
    let releases=crate::collections::group_articles(GROUP,crate::nntp::headers_to_articles(headers));
    assert_eq!(releases.len(),1,"literal headers must describe one real file/release");
    let mut release=releases.into_values().next().unwrap();
    assert!(!release.articles.is_empty());
    release.group=GROUP.into();release.poster=release.articles[0].author.clone();release.date=release.articles[0].date.clone();
    release.complete=crate::parser::is_complete(&release.articles);release
}
#[derive(Debug,PartialEq,Eq)]
struct Timing {
    first:Option<i64>,last:Option<i64>,number_first:Option<i64>,number_last:Option<i64>,lines:Option<i64>,
    references:i64,xref:Option<i64>,from_hash:Option<i64>,from_variants:i64,encoded:i64,late:i64,articles:i64,
    tool:Option<i64>,clock_first:Option<i64>,clock_last:Option<i64>,host:Option<String>,key:Option<String>,
    hw_first:Option<i64>,hw_last:Option<i64>,
}
fn timing(client:&mut impl postgres::GenericClient,file:i64)->Option<Timing> {
    client.query_opt("select first_date,last_date,number_first,number_last,lines,references_seen,xref_groups_max,from_hash,
        from_variants,encoded_subject,late_articles,articles,tool,clock_first,clock_last,hw_host,hw_key,hw_n_first,hw_n_last
        from file_timing where file_id=$1",&[&file]).unwrap().map(|row|Timing {
            first:row.get(0),last:row.get(1),number_first:row.get(2),number_last:row.get(3),lines:row.get(4),references:row.get(5),
            xref:row.get(6),from_hash:row.get(7),from_variants:row.get(8),encoded:row.get(9),late:row.get(10),articles:row.get(11),
            tool:row.get(12),clock_first:row.get(13),clock_last:row.get(14),host:row.get(15),key:row.get(16),hw_first:row.get(17),hw_last:row.get(18),
        })
}
fn epoch(minute:u32,second:u32)->i64 {
    chrono::NaiveDate::from_ymd_opt(2026,1,1).unwrap().and_hms_opt(0,minute,second).unwrap().and_utc().timestamp()
}

#[test]
#[ignore="explicit isolated PostgreSQL: accepted native headers must produce genuine timing; baseline RED expected"]
fn native_accepted_headers_produce_timing_windows_and_idempotent_mixed_source_metadata() {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated ATLAS_PG_TEST_URL required; never skipped success");
    let root=tempfile::tempdir().unwrap();let mut config=crate::config::Config::default();
    config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;
    config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;
    let runtime=crate::pg::indexer_runtime::Runtime::open(&config,crate::pg::StorageConfig {
        backend:crate::pg::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:1,allow_plaintext:true,
    },crate::config::HeaderStoreConfig {enabled:true,dir:root.path().to_str().unwrap().into()},&[],&mut None).unwrap();
    // Two Highwinds headers use the first listing's numbering. An alternate
    // listing later contributes a real third MID, under its own number and an
    // earlier Date. Header Dates and message-ID clocks are distinct facts.
    let first=parsed(vec![
        header("Native.Timing.Example.mkv","<EaPmB.546176$JP7.228032@fx09.am4>",1,3,100,"Thu, 01 Jan 2026 00:01:00 +0000",POSTER_A,10,"",2),
        header("Native.Timing.Example.mkv","<EaPmC.546180$JP7.228040@fx09.am4>",2,3,102,"Thu, 01 Jan 2026 00:02:00 +0000",POSTER_A,20,"<topic@fixture.test>",4),
    ]);
    let alternate=parsed(vec![header("Native.Timing.Example.mkv","<ZjPsQyLgOjNtQvXbHeKaXyCm1730295651235@nyuu>",3,3,500,
        "Thu, 01 Jan 2026 00:00:30 +0000",POSTER_B,30,"<other-topic@fixture.test>",3)]);
    let control_a=parsed(vec![header("Native.Timing.ControlA.mkv","<control-a@fixture.test>",1,1,1,"Thu, 01 Jan 2026 00:00:00 +0000",POSTER_A,1,"",1)]);
    let control_b=parsed(vec![header("Native.Timing.ControlB.mkv","<control-b@fixture.test>",1,1,2,"Thu, 01 Jan 2026 00:00:00 +0000",POSTER_B,1,"",1)]);
    assert_eq!(first.name,alternate.name,"independent listing headers must target the same actual parsed identity");
    runtime.owner.with(|client| {
        assert_eq!(save(client,std::slice::from_ref(&first),10)?.parts,2);
        assert_eq!(save(client,std::slice::from_ref(&alternate),11)?.parts,1);
        assert_eq!(save(client,&[control_a.clone(),control_b.clone()],12)?.parts,2);
        Ok(())
    }).unwrap();
    let file=runtime.owner.with(|client|Ok(client.query_one("select f.id from files f join releases r on r.id=f.release_id where r.group_name=$1 and r.original_name=$2",
        &[&GROUP,&first.name.as_bytes()])?.get::<_,i64>(0))).unwrap();
    let before=runtime.owner.with(|client|Ok(timing(client,file))).unwrap();
    // An already accepted MID with conflicting timing/envelope facts remains
    // a replay. It cannot widen windows or add lines/From runs/counters.
    let mut replay=first.clone();
    for article in &mut replay.articles {
        article.date="2040-01-01 00:00:00".into();article.author="Replay must not change metadata".into();article.lines=999;
        article.references="<fake-reference@fixture.test>".into();article.number=9_999_999;article.xref_groups=Some(99);
    }
    runtime.owner.with(|client| {
        assert_eq!(save(client,&[replay,alternate.clone()],13)?.parts,0);
        assert_eq!(client.query_one("select count(*) from segments s join files f on f.id=s.file_id where f.id=$1",&[&file])?.get::<_,i64>(0),3,
            "the regression requires three real accepted PG parts before timing assertions");
        assert_eq!(client.query_one("select count(*) from file_parts where file_id=$1",&[&file])?.get::<_,i64>(0),3);
        Ok(())
    }).unwrap();
    // Expected genuine baseline assertion failure: current production saved
    // those real parts but has no native file_timing producer. Setup, missing
    // symbols, SQL/type failures do not count as this RED.
    let got=runtime.owner.with(|client|Ok(timing(client,file))).unwrap()
        .expect("native writer accepted three real headers but produced no file_timing row");
    assert_eq!(Some(&got),before.as_ref(),"replay must leave every timing fact unchanged");
    assert_eq!((got.first,got.last),(Some(epoch(0,30)),Some(epoch(2,0))));
    assert_eq!((got.number_first,got.number_last),(Some(100),Some(500)));
    assert_eq!((got.lines,got.references,got.xref,got.from_variants,got.encoded,got.late,got.articles),(Some(60),2,Some(4),2,0,0,3));
    assert_eq!(got.tool,Some(crate::discovery::ToolType::Nyuu.code()));
    assert_eq!((got.clock_first,got.clock_last),(Some(1_503_379_236_000),Some(1_730_295_651_235)));
    assert_eq!((got.host.as_deref(),got.key.as_deref(),got.hw_first,got.hw_last),(Some("fx09.am4"),Some("JP7"),Some(546_176),Some(546_180)));
    runtime.owner.with(|client| {
        let hash=|client:&mut postgres::Client,name:&str|->Option<i64> {client.query_one("select t.from_hash from file_timing t join files f on f.id=t.file_id join releases r on r.id=f.release_id where r.original_name=$1",&[&name.as_bytes()]).unwrap().get(0)};
        let (a,b)=(hash(client,&control_a.name),hash(client,&control_b.name));
        assert_eq!(got.from_hash,b,"last actual From and same-catalog control use the same stable hash");
        assert!(a.is_some()&&b.is_some()&&a!=b&&a!=Some(0)&&b!=Some(0),"real From-run evidence cannot be an absent/constant hash");
        Ok(())
    }).unwrap();
    assert_eq!(runtime.owner.failure(),None);runtime.owner.close();
}
