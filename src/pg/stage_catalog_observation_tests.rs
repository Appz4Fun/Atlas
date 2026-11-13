//! Genuine fresh-catalog mutations must reach completed native observations.
use std::collections::BTreeMap;
use super::{stage_telemetry,indexer_runtime::Runtime};

fn frame(runtime:&Runtime)->BTreeMap<(String,String),f64> {
    stage_telemetry::capture(&runtime.owner,None).unwrap();
    runtime.owner.with(|client|Ok(client.query("select family,labels,gauge_bits from native_runtime_observation_current h join native_metric_samples s using(observation_id) where kind=2 and family in ('atlas_catalog_releases','atlas_catalog_bytes','atlas_native_group_present','atlas_group_article_number','atlas_group_backfill_articles','atlas_group_posted_timestamp_seconds','atlas_group_backfill_chunks','atlas_group_backfill_day_timestamp_seconds')",&[])?.into_iter().map(|row|((row.get(0),row.get(1)),f64::from_bits(row.get::<_,i64>(2) as u64))).collect())).unwrap()
}
fn value(frame:&BTreeMap<(String,String),f64>,family:&str,labels:&str)->Option<f64> {
    frame.get(&(family.into(),labels.into())).copied()
}

#[test]
fn native_catalog_observations_follow_real_writer_replay_metadata_match_delete_and_rollback() {
    let Some(schema)=super::testing::Schema::new() else {return};let runtime=super::stage_queue_tests::runtime(&schema);
    let empty=frame(&runtime);
    assert_eq!(value(&empty,"atlas_catalog_releases","state=\"total\""),Some(0.));
    super::stage_queue_tests::push(&runtime);super::stage_queue_tests::push(&runtime);
    let first=frame(&runtime);assert_eq!(value(&first,"atlas_catalog_releases","state=\"total\""),Some(1.));
    assert_eq!(value(&first,"atlas_catalog_bytes",""),Some(1.));
    runtime.owner.with(|client| {
        let mut tx=client.transaction()?;
        tx.execute("update releases set display_name='Known title',is_obfuscated=true,size=9",&[])?;
        tx.execute("insert into predb_match(release_id,title_id,kind,matched_at) select id,1,1,10 from releases",&[])?;
        tx.commit()?;Ok(())
    }).unwrap();
    let changed=frame(&runtime);
    for state in ["total","named","predb_matched"] {assert_eq!(value(&changed,"atlas_catalog_releases",&format!("state=\"{state}\"")),Some(1.));}
    assert_eq!(value(&changed,"atlas_catalog_releases","state=\"still_obfuscated\""),Some(0.));
    assert_eq!(value(&changed,"atlas_catalog_bytes",""),Some(9.));
    runtime.owner.with(|client| {let mut tx=client.transaction()?;tx.execute("delete from releases",&[])?;tx.rollback()?;Ok(())}).unwrap();
    assert_eq!(frame(&runtime),changed,"rolled-back cascades cannot publish decremented catalog facts");
    runtime.owner.with(|client| {client.execute("delete from releases",&[])?;Ok(())}).unwrap();
    let deleted=frame(&runtime);assert_eq!(value(&deleted,"atlas_catalog_releases","state=\"total\""),Some(0.));
    assert_eq!(value(&deleted,"atlas_catalog_releases","state=\"predb_matched\""),Some(0.));assert_eq!(value(&deleted,"atlas_catalog_bytes",""),Some(0.));
    runtime.owner.close();
}

#[test]
fn native_group_observations_preserve_null_presence_real_marks_and_chunk_rollback() {
    let Some(schema)=super::testing::Schema::new() else {return};let runtime=super::stage_queue_tests::runtime(&schema);
    runtime.owner.with(|client| {
        client.batch_execute("insert into groups(name) values('alt.binaries.unknown');
            insert into groups(name,low_posted) values('alt.binaries.time',123);
            insert into groups(name,first_article,last_article) values('alt.binaries.partial@a',10,30);
            insert into groups(name,backfill_cursor,live_cursor) values('alt.binaries.partial@b',9,30);
            insert into groups(name,first_article,last_article,backfill_cursor,live_cursor) values('alt.binaries.range',10,30,9,20);
            insert into backfill_chunks(grp,day,state) values('alt.binaries.range',1,2),('alt.binaries.range',2,0)")?;Ok(())
    }).unwrap();
    let first=frame(&runtime);
    assert_eq!(value(&first,"atlas_native_group_present","group=\"alt.binaries.unknown\""),Some(1.));
    assert!(!first.keys().any(|(family,labels)|family=="atlas_group_article_number"&&labels.contains("alt.binaries.unknown")));
    assert_eq!(value(&first,"atlas_group_posted_timestamp_seconds","group=\"alt.binaries.time\",end=\"oldest\""),Some(123.));
    assert_eq!(value(&first,"atlas_group_article_number","group=\"alt.binaries.range\",mark=\"last\""),Some(30.));
    assert_eq!(value(&first,"atlas_group_backfill_articles","group=\"alt.binaries.partial\",state=\"covered\""),None,"different accounts cannot donate missing progress marks to each other");
    assert_eq!(value(&first,"atlas_group_backfill_chunks","group=\"alt.binaries.range\",state=\"done\""),Some(1.));
    assert_eq!(value(&first,"atlas_group_backfill_chunks","group=\"alt.binaries.range\",state=\"total\""),Some(2.));
    assert_eq!(value(&first,"atlas_group_backfill_articles","group=\"alt.binaries.range\",state=\"covered\""),Some(10.));
    runtime.owner.with(|client| {let mut tx=client.transaction()?;tx.execute("delete from backfill_chunks where state=2",&[])?;tx.execute("delete from groups",&[])?;tx.rollback()?;Ok(())}).unwrap();
    assert_eq!(frame(&runtime),first);
    runtime.owner.with(|client| {client.batch_execute("delete from backfill_chunks where state=2;delete from groups where name='alt.binaries.unknown'")?;Ok(())}).unwrap();
    let deleted=frame(&runtime);assert_eq!(value(&deleted,"atlas_native_group_present","group=\"alt.binaries.unknown\""),None);
    assert_eq!(value(&deleted,"atlas_group_backfill_chunks","group=\"alt.binaries.range\",state=\"done\""),Some(0.));
    assert_eq!(value(&deleted,"atlas_group_backfill_day_timestamp_seconds","group=\"alt.binaries.range\",day=\"oldest\""),Some(172800.));
    assert_eq!(value(&deleted,"atlas_group_backfill_day_timestamp_seconds","group=\"alt.binaries.range\",day=\"oldest_done\""),None);
    runtime.owner.close();
}

fn chunk_summary(runtime:&Runtime,group:&str)->Option<(i64,i64,i64,i64,Option<i64>)> {
    runtime.owner.with(|client|Ok(client.query_opt("select total,done,oldest_day,newest_day,oldest_done_day from native_metric_chunk_groups where grp=$1",&[&group])?.map(|row|(row.get(0),row.get(1),row.get(2),row.get(3),row.get(4))))).unwrap()
}
fn no_pending_boundaries(runtime:&Runtime) {
    runtime.owner.with(|client|{assert_eq!(client.query_one("select count(*) from native_metric_chunk_touched",&[])?.get::<_,i64>(0),0);Ok(())}).unwrap();
}

#[test]
fn native_chunk_metrics_allow_actual_multirow_pruning_with_rollback_and_empty_replay() {
    let Some(schema)=super::testing::Schema::new() else {return};let runtime=super::stage_queue_tests::runtime(&schema);
    runtime.owner.with(|client|{client.batch_execute("insert into backfill_chunks(grp,day,state) values('alt.binaries.prune',1,0),('alt.binaries.prune',2,0),('alt.binaries.prune',3,0)")?;Ok(())}).unwrap();
    assert_eq!(chunk_summary(&runtime,"alt.binaries.prune"),Some((3,0,1,3,None)));no_pending_boundaries(&runtime);
    let before=frame(&runtime);
    runtime.owner.with(|client| {
        let mut tx=client.transaction()?;
        {let sql=crate::pg::sql::Pg::new(&mut tx);assert_eq!(crate::chunks::drop_before(&sql,"alt.binaries.prune",4)?,3);}
        assert_eq!(tx.query_one("select count(*) from native_metric_chunk_groups",&[])?.get::<_,i64>(0),0);
        tx.rollback()?;Ok(())
    }).unwrap();
    assert_eq!(frame(&runtime),before);no_pending_boundaries(&runtime);
    runtime.owner.with(|client| {let sql=crate::pg::sql::Pg::new(client);assert_eq!(crate::chunks::drop_before(&sql,"alt.binaries.prune",4)?,3);assert_eq!(crate::chunks::drop_before(&sql,"alt.binaries.prune",4)?,0);Ok(())}).unwrap();
    assert_eq!(chunk_summary(&runtime,"alt.binaries.prune"),None);no_pending_boundaries(&runtime);
    assert_eq!(value(&frame(&runtime),"atlas_group_backfill_chunks","group=\"alt.binaries.prune\",state=\"total\""),None,"real empty groups have no invented boundary/count row");
    runtime.owner.close();
}

#[test]
fn native_chunk_metrics_refresh_final_multirow_move_and_update_boundaries_atomically() {
    let Some(schema)=super::testing::Schema::new() else {return};let runtime=super::stage_queue_tests::runtime(&schema);
    runtime.owner.with(|client|{client.batch_execute("insert into backfill_chunks(grp,day,state) values('alt.binaries.from',1,2),('alt.binaries.from',2,0),('alt.binaries.from',3,0)")?;Ok(())}).unwrap();
    let before=frame(&runtime);
    runtime.owner.with(|client|{let mut tx=client.transaction()?;assert_eq!(tx.execute("update backfill_chunks set grp='alt.binaries.to' where grp='alt.binaries.from'",&[])?,3);tx.rollback()?;Ok(())}).unwrap();
    assert_eq!(frame(&runtime),before);assert_eq!(chunk_summary(&runtime,"alt.binaries.from"),Some((3,1,1,3,Some(1))));assert_eq!(chunk_summary(&runtime,"alt.binaries.to"),None);no_pending_boundaries(&runtime);
    runtime.owner.with(|client|{assert_eq!(client.execute("update backfill_chunks set grp='alt.binaries.to' where grp='alt.binaries.from'",&[])?,3);assert_eq!(client.execute("update backfill_chunks set grp='alt.binaries.to' where grp='alt.binaries.from'",&[])?,0);Ok(())}).unwrap();
    assert_eq!(chunk_summary(&runtime,"alt.binaries.from"),None);assert_eq!(chunk_summary(&runtime,"alt.binaries.to"),Some((3,1,1,3,Some(1))));no_pending_boundaries(&runtime);
    runtime.owner.with(|client|{assert_eq!(client.execute("update backfill_chunks set day=day+10,state=2 where grp='alt.binaries.to'",&[])?,3);Ok(())}).unwrap();
    assert_eq!(chunk_summary(&runtime,"alt.binaries.to"),Some((3,3,11,13,Some(11))));no_pending_boundaries(&runtime);
    let changed=frame(&runtime);assert_eq!(value(&changed,"atlas_group_backfill_day_timestamp_seconds","group=\"alt.binaries.to\",day=\"oldest\""),Some(11.*86400.));
    runtime.owner.close();
}
