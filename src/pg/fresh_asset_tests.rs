use super::*;
#[cfg(unix)]
use std::io::{Read,Write};
#[cfg(unix)]
use sha2::{Digest,Sha256};

#[test]
#[cfg(unix)]
fn admitted_asset_reads_check_manifest_content_and_refuse_unlisted_or_replaced_files() {
    let home=tempfile::tempdir().unwrap();let evidence=tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join("covers")).unwrap();
    let bytes=b"retained original asset";let name=hex(&Sha256::digest(bytes));
    std::fs::write(home.path().join("covers").join(&name),bytes).unwrap();
    let manifest=evidence.path().join("assets.jsonl");
    super::super::cache_assets::capture_file(home.path(),&manifest,super::super::cache_assets::Limits {
        min_files:1,max_files:16,max_file_bytes:1<<20,max_total_bytes:16<<20,read_bytes_per_second:32<<20}).unwrap();
    let manifest_sha:[u8;32]=Sha256::digest(std::fs::read(&manifest).unwrap()).into();
    let grant=AssetManifest::open(&home.path().canonicalize().unwrap(),&manifest.canonicalize().unwrap(),manifest_sha).unwrap();
    let scratch=tempfile::tempdir().unwrap();
    #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(scratch.path(),std::fs::Permissions::from_mode(0o700)).unwrap();}
    let pool=crate::nzb_spool::Pool::new(scratch.path().into(),crate::nzb_spool::Limits {requests:2,per_request_bytes:1<<20,total_bytes:2<<20,free_bytes:0}).unwrap();
    let request=AssetRequest {kind:AssetKind::Covers,name:name.clone(),digest:Some(Sha256::digest(bytes).into()),bytes:Some(bytes.len() as u64),content_type:"image/jpeg"};
    let mut found=grant.read(&request,&pool).unwrap().unwrap();let mut observed=Vec::new();found.read_to_end(&mut observed).unwrap();assert_eq!(observed,bytes);drop(found);
    let disagreement=AssetRequest {digest:Some([7;32]),..request.clone()};assert!(grant.read(&disagreement,&pool).is_err());
    let missing=AssetRequest {name:"unlisted".into(),..request.clone()};assert!(grant.read(&missing,&pool).unwrap().is_none());
    let traversal=AssetRequest {name:"../outside".into(),..request.clone()};assert!(grant.read(&traversal,&pool).is_err());
    std::fs::write(home.path().join("covers").join(&name),vec![b'X';bytes.len()]).unwrap();assert!(grant.read(&request,&pool).is_err());
    std::fs::remove_file(home.path().join("covers").join(&name)).unwrap();assert!(grant.read(&request,&pool).unwrap().is_none());
    #[cfg(unix)] {
        std::os::unix::fs::symlink(evidence.path().join("assets.jsonl"),home.path().join("covers").join(&name)).unwrap();
        assert!(grant.read(&request,&pool).is_err());
    }
    std::fs::OpenOptions::new().append(true).open(&manifest).unwrap().write_all(b" ").unwrap();
    assert!(AssetManifest::open(&home.path().canonicalize().unwrap(),&manifest.canonicalize().unwrap(),manifest_sha).is_err());
}

#[test]
#[ignore="requires explicit isolated fresh PostgreSQL schema9/cache10"]
fn native_asset_descriptors_require_real_pg_association_and_keep_original_poster_cache_name() {
    use crate::{parser::{Article,Release},pg::{self,fresh_reader::{ReadView,Budget}}};
    use std::{collections::HashMap,time::{Duration,Instant}};
    let schema=pg::testing::Schema::new().expect("explicit fresh PostgreSQL schema9/cache10");let mut client=schema.pool.get().unwrap();
    let release=Release {name:"Artwork.Example.Release".into(),poster:"poster".into(),group:"alt.binaries.test".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
        articles:vec![Article {filename:Some("video.mkv".into()),subject:"video.mkv yEnc (1/1)".into(),message_id:"artwork@example.invalid".into(),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}],..Default::default()};
    assert_eq!(pg::release_writer::save(&mut client,&[release],1).unwrap().releases,1);
    let id:i64=client.query_one("select id from releases",&[]).unwrap().get(0);
    let sha="c1c9d7f586315859a2f4d2be2a19505e362fe3a7b69bf186f0422512e397e965";
    client.execute("insert into release_cover(release_id,mime,sha256) values($1,'image/png',$2)",&[&id,&sha]).unwrap();
    client.execute("insert into ext_media_details(kind,tmdb_id,poster,fetched_at) values(2,123,'/retained_poster.jpg',1)",&[]).unwrap();
    let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
    let args=HashMap::from([("id".into(),id.to_string())]);
    let cover=view.asset_request("/ui/cover",&args).unwrap().unwrap();assert_eq!(cover.kind,AssetKind::Covers);assert_eq!(cover.name,sha);assert_eq!(cover.content_type,"image/png");assert!(cover.digest.is_some());
    assert!(view.asset_request("/ui/still",&args).unwrap().is_none());
    let args=HashMap::from([("kind".into(),"2".into()),("tmdb".into(),"123".into())]);
    let poster=view.asset_request("/ui/poster",&args).unwrap().unwrap();assert_eq!(poster.kind,AssetKind::Posters);assert_eq!(poster.name,"retained_poster.jpg");
    let absent=HashMap::from([("kind".into(),"2".into()),("tmdb".into(),"456".into())]);assert!(view.asset_request("/ui/poster",&absent).unwrap().is_none());
}
