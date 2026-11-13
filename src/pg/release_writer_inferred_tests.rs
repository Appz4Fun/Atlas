//! The repair producer must save its real header and durable result in one
//! caller-owned RC transaction, with no nested commit or isolation loophole.
use super::*;
#[test]
#[ignore="requires explicit isolated PG for caller-owned native header transactions"]
fn inferred_header_writer_joins_actual_transaction_and_rolls_back_without_partial_payload() {
    let schema=crate::pg::testing::Schema::native_writer().expect("explicit isolated PG required");let mut client=schema.pool.get().unwrap();
    let release=Release {name:"Native.Inferred.Source".into(),group:"example.group".into(),articles:vec![Article {filename:Some("own.bin".into()),message_id:"<part3of4.own@example.invalid>".into(),subject:"Native.Inferred.Source - \"own.bin\" yEnc (3/4)".into(),part:Some(3),total_parts:Some(4),bytes:17,..Default::default()}],..Default::default()};
    let mut tx=client.transaction().unwrap();
    let stats=save_in(&mut tx,std::slice::from_ref(&release),10).unwrap();assert_eq!(stats.parts,1);
    tx.execute("insert into inferred_ids(message_id,next_at,tries) values('part3of4.own@example.invalid',99,1)",&[]).unwrap();
    let mut next=release.clone();next.articles[0].message_id="<part4of4.own@example.invalid>".into();next.articles[0].part=Some(4);
    assert_eq!(save_in(&mut tx,&[next],10).unwrap().parts,1,"second expanded batch shares the original transaction/stage");
    assert_eq!(tx.query_one("select count(*) from segments",&[]).unwrap().get::<_,i64>(0),2);
    assert_eq!(tx.query_one("select count(*) from pg_temp.atlas_release_input",&[]).unwrap().get::<_,i64>(0),1,"prior batch input was truncated, never readopted");tx.rollback().unwrap();
    assert_eq!(client.query_one("select (select count(*) from segments)+(select count(*) from inferred_ids)+(select count(*) from releases)",&[]).unwrap().get::<_,i64>(0),0,"header/result acceptance rollback is atomic");
    let mut tx=client.build_transaction().isolation_level(IsolationLevel::RepeatableRead).start().unwrap();
    assert!(save_in(&mut tx,&[release],10).is_err(),"writer must reject a retained pre-lock snapshot");tx.rollback().unwrap();
    assert_eq!(client.query_one("select count(*) from segments",&[]).unwrap().get::<_,i64>(0),0);
}
