//! Source-first worker asset controls; execute only under the isolated PG/resource harness.
use super::*;
use std::io::Read;

fn selected(bytes:&[u8])->(tempfile::TempDir,tempfile::TempDir,crate::config::NativeAssetsConfig,String) {
    let root=tempfile::tempdir().unwrap();let evidence=tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("covers")).unwrap();
    let name=crate::pg::fresh_assets::hex(&Sha256::digest(bytes));
    std::fs::write(root.path().join("covers").join(&name),bytes).unwrap();
    let manifest=evidence.path().join("assets.jsonl");
    crate::pg::cache_assets::capture_file(root.path(),&manifest,crate::pg::cache_assets::Limits {min_files:1,max_files:16,max_file_bytes:1<<20,max_total_bytes:16<<20,read_bytes_per_second:32<<20}).unwrap();
    let config=crate::config::NativeAssetsConfig {root:root.path().canonicalize().unwrap().to_str().unwrap().into(),manifest:manifest.canonicalize().unwrap().to_str().unwrap().into(),whole_sha256:crate::pg::fresh_assets::hex(&Sha256::digest(std::fs::read(&manifest).unwrap()))};
    (root,evidence,config,name)
}
fn jpeg()->Vec<u8> {[&[0xff,0xd8,0xff,0xe0][..],&[7;100],&[0xff,0xd9]].concat()}
fn source(runtime:&crate::pg::indexer_runtime::Runtime)->(i64,String,String,Vec<u8>,i64,bool) {
    crate::pg::stage_queue_tests::push(runtime);
    runtime.owner.with(|client| {
        let row=client.query_one("select id,original_name,group_name,parts,complete from releases",&[])?;
        let id:i64=row.try_get(0)?;
        Ok((id,String::from_utf8(row.try_get(1)?)?,row.try_get(2)?,crate::pg::content_id::read(client,id,&runtime.owner.stop)?.unwrap(),row.try_get(3)?,row.try_get(4)?))
    }).unwrap()
}
fn save(runtime:&crate::pg::indexer_runtime::Runtime,key:crate::pg::natural_cache::Key<'_>,parts:i64,complete:bool,sha:&str)->crate::raw_store::SavedProbe {
    let info=serde_json::to_string(&crate::probe::ProbeInfo::default()).unwrap();
    runtime.owner.with(|client| {
        let mut tx=client.transaction()?;
        crate::pg::natural_cache::put_probe(&mut tx,&crate::raw_store::ProbeRecord {name:key.name,group_name:key.group,content_id:key.content_id,crc32:None,agent:None,info:&info,moviehash:None,parts,complete,config_stamp:127,nfo_password:None,cover:Some(("image/jpeg",sha))},crate::pg::natural_cache::Budget {bytes:1<<20,rows:8})?;
        tx.commit()?;Ok(())
    }).unwrap();
    crate::raw_store::SavedProbe {crc32:None,agent:None,info,moviehash:None,nfo_password:None,cover:Some(("image/jpeg".into(),sha.into()))}
}
fn verify(grant:&Arc<Grant>,id:i64,key:crate::pg::natural_cache::Key<'_>,parts:i64,complete:bool,stamp:u64,saved:&crate::raw_store::SavedProbe)->Result<CoverStatus> {
    let crate::probe::step::SavedCoverage::Current(checked)=crate::probe::step::native_saved_probe(&saved.info,&mut (32<<20))? else {return Ok(CoverStatus::Changed)};
    grant.verify_probe(id,key,parts,complete,stamp,saved,&checked)
}
#[test]
fn worker_cover_grant_uses_real_pg_association_and_pins_verified_spool_until_settlement() {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    let (id,name,group,content,parts,complete)=source(&runtime);let key=crate::pg::natural_cache::Key {name:&name,group:&group,content_id:&content};
    let bytes=jpeg();let (root,_evidence,selection,sha)=selected(&bytes);let grant=Grant::open(&runtime.owner,Some(&selection)).unwrap().unwrap();
    let saved=save(&runtime,key,parts,complete,&sha);
    let CoverStatus::Verified(mut hold)=verify(&grant,id,key,parts,complete,127,&saved).unwrap() else {panic!("actual retained cover must qualify")};
    hold.check(&runtime.owner,id).unwrap();assert_eq!(hold.descriptor(),("image/jpeg".into(),sha.clone()));
    // Once captured, later source mutation cannot alter a pending outcome's bytes.
    std::fs::write(root.path().join("covers").join(&sha),vec![0;bytes.len()]).unwrap();
    let mut read=Vec::new();hold.spool.read_to_end(&mut read).unwrap();assert_eq!(read,bytes);
    assert!(verify(&grant,id,key,parts,complete,127,&saved).is_err(),"new corrupted read is never a hit/miss/provider fallback");
    assert!(hold.check(&runtime.owner,id+1).is_err());
}
#[test]
fn worker_cover_grant_refuses_wrong_content_forged_descriptor_and_ended_owner() {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    let (id,name,group,content,parts,complete)=source(&runtime);let key=crate::pg::natural_cache::Key {name:&name,group:&group,content_id:&content};
    let (_root,_evidence,selection,sha)=selected(&jpeg());let grant=Grant::open(&runtime.owner,Some(&selection)).unwrap().unwrap();let saved=save(&runtime,key,parts,complete,&sha);
    let mut forged=saved.clone();forged.cover.as_mut().unwrap().1="00".repeat(32);
    assert!(matches!(verify(&grant,id,key,parts,complete,127,&forged).unwrap(),CoverStatus::Changed));
    assert!(matches!(verify(&grant,id,crate::pg::natural_cache::Key {content_id:&[9],..key},parts,complete,127,&saved).unwrap(),CoverStatus::Changed));
    let CoverStatus::Verified(hold)=verify(&grant,id,key,parts,complete,127,&saved).unwrap() else {panic!()};
    runtime.owner.stop.store(true,std::sync::atomic::Ordering::Release);
    assert!(hold.check(&runtime.owner,id).is_err());assert!(verify(&grant,id,key,parts,complete,127,&saved).is_err());
}
#[test]
fn worker_native_produced_cover_reuses_durable_receipt_without_manifest_rewrite() {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    let (id,name,group,content,parts,complete)=source(&runtime);let key=crate::pg::natural_cache::Key {name:&name,group:&group,content_id:&content};
    let (_root,_evidence,selection,_)=selected(&jpeg());let grant=Grant::open(&runtime.owner,Some(&selection)).unwrap().unwrap();
    let mut bytes=jpeg();bytes[10]=8;let cover=crate::probe::media::Cover {mime:"image/jpeg",bytes:bytes.clone()};
    let produced=grant.produce_cover(id,&cover).unwrap();let (_,sha)=produced.descriptor();let saved=save(&runtime,key,parts,complete,&sha);drop(produced);
    assert!(matches!(verify(&grant,id,key,parts,complete,127,&saved).unwrap(),CoverStatus::Verified(_)));
    runtime.owner.with(|client| {client.execute("update ext_asset_files set source_digest=decode(repeat('00',32),'hex') where kind='covers' and name=$1",&[&sha])?;Ok(())}).unwrap();
    assert!(verify(&grant,id,key,parts,complete,127,&saved).is_err(),"unmatched namespace provenance cannot grant a dynamic file");
}
#[test]
fn worker_asset_selection_redacts_paths_and_requires_exact_physical_manifest() {
    let (_root,_evidence,mut config,_)=selected(&jpeg());
    assert!(!format!("{config:?}").contains(&config.root));
    config.whole_sha256="00".repeat(32);
    assert!(crate::pg::fresh_assets::AssetManifest::from_selection(&config).is_err());
}

#[test]
fn worker_cover_missing_and_overlarge_files_do_not_become_provider_cache_misses() {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    let (id,name,group,content,parts,complete)=source(&runtime);let key=crate::pg::natural_cache::Key {name:&name,group:&group,content_id:&content};
    let (root,_evidence,selection,sha)=selected(&jpeg());let grant=Grant::open(&runtime.owner,Some(&selection)).unwrap().unwrap();let saved=save(&runtime,key,parts,complete,&sha);
    std::fs::remove_file(root.path().join("covers").join(&sha)).unwrap();
    assert!(matches!(verify(&grant,id,key,parts,complete,127,&saved).unwrap(),CoverStatus::Missing));
    let mut large=jpeg();large.resize(crate::probe::media::MAX_COVER+1,0);let (root2,_evidence2,selection2,sha2)=selected(&large);
    let grant2=Grant::open(&runtime.owner,Some(&selection2)).unwrap().unwrap();let saved2=save(&runtime,key,parts,complete,&sha2);
    assert!(verify(&grant2,id,key,parts,complete,127,&saved2).is_err());
    assert_eq!(std::fs::metadata(root2.path().join("covers").join(&sha2)).unwrap().len(),large.len() as u64);
    assert!(Grant::open(&runtime.owner,None).unwrap().is_none(),"absence is unavailable, not a fabricated grant");
}

#[test]
fn worker_cover_rejects_real_pg_mime_mismatch_and_preserves_driver_errors() {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    let (id,name,group,content,parts,complete)=source(&runtime);let key=crate::pg::natural_cache::Key {name:&name,group:&group,content_id:&content};
    let (_root,_evidence,selection,sha)=selected(&jpeg());let grant=Grant::open(&runtime.owner,Some(&selection)).unwrap().unwrap();let mut saved=save(&runtime,key,parts,complete,&sha);
    runtime.owner.with(|client| {client.execute("update ext_raw_covers set mime='image/png'",&[])?;Ok(())}).unwrap();
    saved.cover.as_mut().unwrap().0="image/png".into();
    assert!(verify(&grant,id,key,parts,complete,127,&saved).is_err(),"genuine association with wrong MIME cannot mint a hold");
    runtime.owner.with(|client| {client.batch_execute("alter table ext_asset_files rename to fixture_assets_unavailable")?;Ok(())}).unwrap();
    let error=match verify(&grant,id,key,parts,complete,127,&saved) {Err(error)=>error,Ok(_)=>panic!("actual PG error must propagate")};
    assert!(error.chain().any(|cause|cause.downcast_ref::<postgres::Error>().is_some()));
}

#[test]
fn worker_cover_producer_requires_real_source_and_never_overwrites_conflicting_bytes() {
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");let runtime=crate::pg::stage_queue_tests::runtime(&schema);
    let (id,_,_,_,_,_)=source(&runtime);let (root,_evidence,selection,_)=selected(&jpeg());let grant=Grant::open(&runtime.owner,Some(&selection)).unwrap().unwrap();
    let mut bytes=jpeg();bytes[10]=9;let sha=crate::pg::fresh_assets::hex(&Sha256::digest(&bytes));let cover=crate::probe::media::Cover {mime:"image/jpeg",bytes:bytes.clone()};
    assert!(grant.produce_cover(id+999,&cover).is_err());
    assert!(!root.path().join("covers").join(&sha).exists(),"unknown source must not write namespace bytes");
    std::fs::write(root.path().join("covers").join(&sha),b"preserved conflicting original").unwrap();
    assert!(grant.produce_cover(id,&cover).is_err());
    assert_eq!(std::fs::read(root.path().join("covers").join(&sha)).unwrap(),b"preserved conflicting original");
    runtime.owner.with(|client| {assert_eq!(client.query_one("select count(*) from ext_asset_files where name=$1",&[&sha])?.try_get::<_,i64>(0)?,0);Ok(())}).unwrap();
}
