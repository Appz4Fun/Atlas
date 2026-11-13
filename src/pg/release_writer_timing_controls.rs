//! Explicit real-PG controls for the catalog-key and accepted-only boundary.
//! Source only until Root admits the isolated PostgreSQL harness.
use super::*;

fn release() -> Release {
    Release { name:"Timing.Unknown.Headers".into(), group:"alt.binaries.timing.control".into(),
        date:"Thu, 01 Jan 2040 00:00:00 +0000".into(),
        articles:vec![Article { filename:Some("data.bin".into()), message_id:"<opaque@fixture.test>".into(),
            subject:"=?UTF-8?Q?data?= yEnc (1/2)".into(),author:"Actual author".into(),date:"not a Date".into(),
            number:0,lines:-7,references:" \t ".into(),part:Some(1),total_parts:Some(2),bytes:17,..Default::default() }],
        ..Default::default() }
}

#[test]
#[ignore="explicit isolated PostgreSQL: direct native writer must refuse missing/null catalog identity"]
fn missing_or_null_catalog_key_refuses_without_adopting_or_accepting_payload() {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated ATLAS_PG_TEST_URL required");
    let mut client=schema.pool.get().unwrap();
    assert_eq!(save(&mut client,&[],1).unwrap(),SaveStats::default());
    let release=release();
    let error=save(&mut client,std::slice::from_ref(&release),1).unwrap_err();
    assert!(format!("{error:#}").contains("existing catalog key_salt"));
    assert_eq!(client.query_one("select (select count(*) from segments)+(select count(*) from files)+(select count(*) from releases)+(select count(*) from file_timing)",&[]).unwrap().get::<_,i64>(0),0);
    assert_eq!(client.query_one("select count(*) from meta where key='key_salt'",&[]).unwrap().get::<_,i64>(0),0,"writer never manufactures a replacement identity");
    client.execute("insert into meta(key,value) values('key_salt',null)",&[]).unwrap();
    let error=save(&mut client,std::slice::from_ref(&release),1).unwrap_err();
    assert!(format!("{error:#}").contains("key_salt is null"));
    assert_eq!(client.query_one("select count(*) from segments",&[]).unwrap().get::<_,i64>(0),0);
    client.execute("update meta set value=7 where key='key_salt'",&[]).unwrap();
    assert_eq!(save(&mut client,std::slice::from_ref(&release),1).unwrap().parts,1);
    let before:i64=client.query_one("select from_hash from file_timing",&[]).unwrap().get(0);
    client.execute("delete from meta where key='key_salt'",&[]).unwrap();
    assert!(save(&mut client,&[release],2).is_err(),"even a replay cannot adopt an alternate key");
    let row=client.query_one("select from_hash,articles from file_timing",&[]).unwrap();
    assert_eq!((row.get::<_,i64>(0),row.get::<_,i64>(1)),(before,1));
}

#[test]
#[ignore="explicit isolated PostgreSQL: timing must share acceptance/rollback and preserve unknown facts"]
fn duplicate_losers_unknown_dates_and_caller_rollback_do_not_invent_timing() {
    let schema=crate::pg::testing::Schema::native_writer().expect("explicit isolated ATLAS_PG_TEST_URL required");
    let mut client=schema.pool.get().unwrap();
    let mut release=release();
    let mut loser=release.articles[0].clone();loser.author="Duplicate loser".into();
    loser.date="Thu, 01 Jan 2040 00:00:00 +0000".into();loser.number=999;loser.lines=999;loser.xref_groups=Some(99);
    let mut second=release.articles[0].clone();second.message_id="<second-opaque@fixture.test>".into();
    second.part=Some(2);second.lines=3;second.references="<real-thread@fixture.test>".into();
    release.articles.extend([loser,second]);
    let check=|tx:&mut Transaction<'_>| {
        let row=tx.query_one("select first_date,last_date,number_first,number_last,lines,references_seen,xref_groups_max,
            from_variants,encoded_subject,late_articles,articles,tool,clock_first,clock_last,hw_host,hw_key,hw_n_first,hw_n_last
            from file_timing",&[]).unwrap();
        for column in [0usize,1,2,3,6,11,12,13,16,17] {assert_eq!(row.get::<_,Option<i64>>(column),None,"unknown header fact at {column}");}
        for column in [14usize,15] {assert_eq!(row.get::<_,Option<String>>(column),None);}
        assert_eq!((row.get::<_,i64>(4),row.get::<_,i64>(5),row.get::<_,i64>(7),row.get::<_,i64>(8),row.get::<_,i64>(9),row.get::<_,i64>(10)),(3,1,1,1,0,2));
    };
    let mut tx=client.transaction().unwrap();
    assert_eq!(save_in(&mut tx,std::slice::from_ref(&release),1).unwrap().parts,2);
    check(&mut tx);tx.rollback().unwrap();
    assert_eq!(client.query_one("select (select count(*) from file_timing)+(select count(*) from segments)+(select count(*) from releases)",&[]).unwrap().get::<_,i64>(0),0);
    assert_eq!(save(&mut client,&[release],1).unwrap().parts,2);
    let mut tx=client.transaction().unwrap();check(&mut tx);tx.rollback().unwrap();
}
