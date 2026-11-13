//! Small-wire controls through the real canonical-owner worker read adapters.
use super::StepReads;
use crate::pg::{testing::Schema,indexer_runtime::Runtime};

#[derive(Clone,Copy,Debug)]
enum Adapter {Keys,Nfo,Media}
fn read(reads:&StepReads,id:i64,adapter:Adapter)->anyhow::Result<()> {
    match adapter {
        Adapter::Keys=>{reads.stills_lookup(id)?;},
        Adapter::Nfo=>{reads.stored_nfo(id)?;},
        Adapter::Media=>{reads.saved_probe_media(id)?;},
    }Ok(())
}
fn fixture()->Option<(Schema,Runtime,i64)> {
    let schema=Schema::new()?;let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    crate::pg::stage_queue_tests::push(&runtime);
    let id=runtime.owner.with(|client|Ok(client.query_one("select id from releases",&[])?.get(0))).unwrap();
    Some((schema,runtime,id))
}
fn stored(runtime:&Runtime,id:i64,json:&str) {
    runtime.owner.with(|client| {
        client.execute("insert into release_probe(release_id,info,moviehash) values($1,$2,'0011223344556677') on conflict(release_id) do update set info=excluded.info",&[&id,&json])?;
        client.execute("insert into release_nfo(release_id,file,text) values($1,'fallback.nfo','must not mask invalid probe') on conflict do nothing",&[&id])?;Ok(())
    }).unwrap();
}
#[test]
fn native_probe_adapters_preserve_valid_facts_and_genuine_absence() {
    let Some((_schema,runtime,id))=fixture() else{return};let reads=StepReads::for_owner(&runtime.owner).unwrap();
    assert!(reads.stills_lookup(id).unwrap().moviehash.is_none());assert!(reads.stored_nfo(id).unwrap().is_none());assert!(reads.saved_probe_media(id).unwrap().is_none());
    let probe=crate::probe::ProbeInfo {nfo:Some("quoted ' <&é>".into()),nfo_file:Some("own.nfo".into()),
        media:Some(crate::probe::media::MediaInfo {container:"matroska".into(),title:Some("Kept title".into()),..Default::default()}),..Default::default()};
    stored(&runtime,id,&serde_json::to_string(&probe).unwrap());
    assert_eq!(reads.stills_lookup(id).unwrap().moviehash.as_deref(),Some("0011223344556677"));
    let nfo=reads.stored_nfo(id).unwrap().unwrap();assert_eq!((nfo.file,nfo.text),("own.nfo".into(),"quoted ' <&é>".into()));
    assert_eq!(reads.saved_probe_media(id).unwrap(),probe.media);
    stored(&runtime,id,&serde_json::to_string(&crate::probe::ProbeInfo::default()).unwrap());
    let nfo=reads.stored_nfo(id).unwrap().unwrap();assert_eq!((nfo.file,nfo.text),("fallback.nfo".into(),"must not mask invalid probe".into()));
    assert!(reads.saved_probe_media(id).unwrap().is_none());runtime.owner.close();
}
#[test]
fn native_probe_adapters_reject_small_wire_structural_amplification_and_malformed_json() {
    // Twenty thousand defaulted track objects fit below64KiB on the wire. The
    // old decode stays small enough for a safe regression; structural ownership
    // admission must still refuse it before constructing the track vector.
    let amplified=format!("{{\"media\":{{\"video\":[{}]}}}}",vec!["{}";20000].join(","));
    assert!(amplified.len()<64*1024);
    for json in ["{","{}",amplified.as_str()] {
        for adapter in [Adapter::Keys,Adapter::Nfo,Adapter::Media] {
            let Some((_schema,runtime,id))=fixture() else{return};stored(&runtime,id,json);
            let reads=StepReads::for_owner(&runtime.owner).unwrap();
            assert!(read(&reads,id,adapter).is_err(),"{adapter:?} must not turn invalid/admission-refused JSON into absence or fallback");
            runtime.owner.close();
        }
    }
}
#[test]
fn native_probe_adapter_pg_failures_keep_the_driver_cause() {
    for adapter in [Adapter::Keys,Adapter::Nfo,Adapter::Media] {
        let Some((_schema,runtime,id))=fixture() else{return};let reads=StepReads::for_owner(&runtime.owner).unwrap();
        runtime.owner.with(|client|{client.batch_execute("drop table release_probe")?;Ok(())}).unwrap();
        let error=read(&reads,id,adapter).expect_err("missing relation is a native storage failure");
        assert!(error.chain().any(|source|source.is::<postgres::Error>()),"{adapter:?} must retain PG provenance");
        runtime.owner.close();
    }
}
