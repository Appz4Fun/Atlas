//! Native merge policy controls; no provider, cache, or clock is mocked.
use super::*;
use serde_json::json;

fn old()->Details {Details {kind:1,tmdb_id:42,title:Some("Old title".into()),tmdb_score:Some(71),imdb_score:Some(81),rotten:Some(91),metacritic:Some(61),trakt_score:Some(51),fetched_at:1,..Default::default()}}
fn source(release:i64,ids:Ids)->Source {Source {release,kind:1,ids}}
fn identity()->Ids {Ids {tmdb:Some(42),imdb:Some(1234567),tvdb:Some(7),..Default::default()}}

#[test]
fn a_failed_omdb_refresh_keeps_only_its_valid_old_scores_and_retries_in_an_hour() {
    let kept=old();let now=10_000_000;
    let mut details=from_tmdb(1,42,&json!({"title":"Fresh title","vote_average":8.2,"vote_count":12}),now);
    let omdb=rating_reply(&mut details,Err(Trouble::Paused(now+60)),now,add_omdb).unwrap();
    assert!(omdb);details.trakt_score=Some(86);
    let merged=preserve_failed_scores(Refreshed {details,failed:RatingFailures {omdb,trakt:false}},Some(&kept));
    assert_eq!((merged.title.as_deref(),merged.tmdb_score,merged.trakt_score),(Some("Fresh title"),Some(82),Some(86)));
    assert_eq!((merged.imdb_score,merged.rotten,merged.metacritic),(Some(81),Some(91),Some(61)));
    assert_eq!(now-merged.fetched_at,KEEP_FOR-RETRY_AFTER);
    assert!(now+RETRY_AFTER-1-merged.fetched_at<KEEP_FOR);assert_eq!(now+RETRY_AFTER-merged.fetched_at,KEEP_FOR);
}

#[test]
fn a_failed_trakt_refresh_keeps_trakt_without_overwriting_new_omdb_or_metadata() {
    let kept=old();let now=10_000_000;let mut details=from_tmdb(1,42,&json!({"title":"New"}),now);
    let omdb=rating_reply(&mut details,Ok(Reply::Json(json!({"Ratings":[
        {"Source":"Internet Movie Database","Value":"8.8/10"}, {"Source":"Rotten Tomatoes","Value":"95%"}]}))),now,add_omdb).unwrap();
    let trakt=rating_reply(&mut details,Err(Trouble::Bad("provider response unavailable".into())),now,|d,v|d.trakt_score=trakt_score(v)).unwrap();
    let merged=preserve_failed_scores(Refreshed {details,failed:RatingFailures {omdb,trakt}},Some(&kept));
    assert_eq!((merged.imdb_score,merged.rotten,merged.metacritic,merged.trakt_score),(Some(88),Some(95),None,Some(51)));
    assert_eq!(merged.title.as_deref(),Some("New"));
}

#[test]
fn absent_or_successful_unrated_providers_are_not_failures_and_do_not_reuse_old_scores() {
    let kept=old();let now=10_000_000;
    for reply in [Reply::NotFound,Reply::Json(json!({"Ratings":[]}))] {
        let mut details=from_tmdb(1,42,&json!({"title":"Fresh"}),now);
        let omdb=rating_reply(&mut details,Ok(reply),now,add_omdb).unwrap();assert!(!omdb);
        let merged=preserve_failed_scores(Refreshed {details,failed:RatingFailures {omdb,trakt:false}},Some(&kept));
        assert_eq!((merged.imdb_score,merged.rotten,merged.metacritic,merged.trakt_score),(None,None,None,None));assert_eq!(merged.fetched_at,now);
    }
    let details=Details {kind:1,tmdb_id:42,fetched_at:now,..Default::default()};
    assert_eq!(preserve_failed_scores(Refreshed {details:details.clone(),failed:RatingFailures::default()},Some(&kept)),details,"unconfigured or missing-ID providers carry no failure");
}

#[test]
fn rating_preservation_never_crosses_natural_identity_or_carries_invalid_scores() {
    let failed=RatingFailures {omdb:true,trakt:true};
    for (kind,id) in [(2,42),(1,43),(2,-42)] {
        let details=Details {kind,tmdb_id:id,..Default::default()};
        assert_eq!(preserve_failed_scores(Refreshed {details:details.clone(),failed},Some(&old())),details);
    }
    let mut kept=old();kept.imdb_score=Some(-1);kept.rotten=Some(101);kept.metacritic=Some(0);kept.trakt_score=Some(100);
    let details=Details {kind:1,tmdb_id:42,imdb_score:Some(99),..Default::default()};
    let merged=preserve_failed_scores(Refreshed {details,failed},Some(&kept));
    assert_eq!((merged.imdb_score,merged.rotten,merged.metacritic,merged.trakt_score),(Some(99),None,Some(0),Some(100)));
    let mut negative=old();negative.kind=2;negative.tmdb_id=-7;
    let merged=preserve_failed_scores(Refreshed {details:Details {kind:2,tmdb_id:-7,..Default::default()},failed},Some(&negative));
    assert_eq!(merged.trakt_score,Some(51),"same negative-TVDB key is a real matching natural identity");
}

#[test]
fn a_native_cache_error_keeps_its_original_typed_cause_instead_of_becoming_partial_success() {
    let mut details=old();let before=details.clone();
    let failure=CacheFailure::new(anyhow::Error::new(crate::pg::outage::SessionUnavailable));
    let error=rating_reply(&mut details,Err(Trouble::Cache(failure)),10_000_000,add_omdb).unwrap_err();
    let Trouble::Cache(error)=error else {panic!("cache failure must remain typed")};
    assert!(error.into_error().chain().any(|cause|cause.is::<crate::pg::outage::SessionUnavailable>()));assert_eq!(details,before);
}

#[test]
fn duplicate_titles_merge_available_trakt_before_any_freshness_selection() {
    let plain=identity();let mut richer=plain;richer.trakt=Some(900);
    let merged=merge_sources(vec![source(10,richer),source(20,plain)]);
    assert_eq!(merged.len(),1);assert_eq!(merged[0].source.release,20);assert_eq!(merged[0].source.ids,richer);
    assert!(merged[0].trakt_missing,"a page addition cannot hide a conflicting ID beyond this page");
}

#[test]
fn conflicting_provider_identities_are_not_merged_or_overwritten() {
    let base=identity();let mut one=base;one.trakt=Some(900);let mut another=base;another.trakt=Some(901);
    let merged=merge_sources(vec![source(20,one),source(10,another)]);assert_eq!(merged.len(),2);assert_eq!(merged[0].source.ids.trakt,Some(900));assert_eq!(merged[1].source.ids.trakt,Some(901));
    for other in [Ids {tmdb:Some(43),..base},Ids {imdb:Some(7654321),..base},Ids {tvdb:Some(8),..base},Ids {tvmaze:Some(8),..base},Ids {tvrage:Some(8),..base}] {
        let merged=merge_sources(vec![source(20,base),source(10,Ids {trakt:Some(900),..other})]);assert_eq!(merged.len(),2);assert_eq!(merged[0].source.ids.trakt,None);
    }
    let mut tv=source(10,Ids {trakt:Some(900),..base});tv.kind=2;
    assert_eq!(merge_sources(vec![source(20,base),tv]).len(),2);
}

#[test]
fn cross_page_trakt_admission_is_fill_only_and_ambiguity_defers_before_freshness() {
    use crate::pg::service_cache::details::TraktIdentity;
    let ids=identity();assert_eq!(bind_trakt(ids,TraktIdentity::Unique(900)).unwrap().trakt,Some(900));
    assert_eq!(bind_trakt(ids,TraktIdentity::Absent).unwrap(),ids);
    assert!(matches!(bind_trakt(ids,TraktIdentity::Ambiguous),Err(Trouble::Bad(_))));
    let known=Ids {trakt:Some(901),..ids};assert_eq!(bind_trakt(known,TraktIdentity::Unique(900)).unwrap(),known);
}

#[test]
#[ignore="requires isolated PG/private home and scripted loopback providers; invokes only its owned child"]
fn native_detail_merges_preserve_scores_and_defer_cross_page_ambiguity_on_the_actual_owned_path() {
    use std::process::{Command,Stdio};
    let schema=crate::pg::testing::Schema::new().expect("explicit isolated PG required");let home=tempfile::tempdir().unwrap();
    let config=format!("[storage]\nbackend='postgres'\nurl={}\nschema={}\nmax_connections=1\nallow_plaintext=true\n",serde_json::to_string(&schema.url).unwrap(),serde_json::to_string(&schema.name).unwrap());
    std::fs::write(home.path().join("config.toml"),config).unwrap();
    let mut child=Command::new(std::env::current_exe().unwrap()).args(["--ignored","--exact","media::details::native::merge_tests::native_detail_merge_owned_child","--nocapture"])
        .env("ATLAS_HOME",home.path()).env("ATLAS_NO_KEYRING","1").env("ATLAS_DETAIL_MERGE_CHILD","1").env_remove("ATLAS_PG_TEST_URL")
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let until=Instant::now()+Duration::from_secs(45);
    loop {if child.try_wait().unwrap().is_some(){break}if Instant::now()>=until {child.kill().unwrap();let _=child.wait();panic!("owned detail merge child timed out")}std::thread::sleep(Duration::from_millis(10));}
    let output=child.wait_with_output().unwrap();assert!(output.status.success(),"{}\n{}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr));
}

#[test]
#[ignore="only the isolated native-detail merge parent may invoke this child"]
fn native_detail_merge_owned_child() {
    use std::{net::TcpListener,io::{Read,Write},sync::{Mutex,atomic::{AtomicBool,AtomicU8,Ordering}}};
    use crate::pg::service_cache::details as pg_details;
    assert_eq!(std::env::var("ATLAS_DETAIL_MERGE_CHILD").unwrap(),"1");
    let runtime=crate::raw_store::native_pg_opensubtitles_cache_tests::open_test_native_owner().expect("actual canonical owner");
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();listener.set_nonblocking(true).unwrap();let base=format!("http://127.0.0.1:{}",listener.local_addr().unwrap().port());
    let stop=Arc::new(AtomicBool::new(false));let phase=Arc::new(AtomicU8::new(0));let requests=Arc::new(Mutex::new(Vec::<String>::new()));
    let (end,script,seen)=(stop.clone(),phase.clone(),requests.clone());
    let thread=std::thread::spawn(move|| {
        while !end.load(Ordering::Acquire) {
            let (mut socket,_)=match listener.accept() {Ok(pair)=>pair,Err(error) if error.kind()==std::io::ErrorKind::WouldBlock=>{std::thread::sleep(Duration::from_millis(5));continue},Err(error)=>panic!("{error}")};
            socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();let mut request=Vec::new();let mut byte=[0];
            while request.len()<8192&&!request.ends_with(b"\r\n\r\n") {if socket.read(&mut byte).unwrap_or(0)==0 {break}request.push(byte[0]);}
            let line=String::from_utf8_lossy(&request).lines().next().unwrap_or_default().to_string();seen.lock().unwrap().push(line.clone());
            let phase=script.load(Ordering::Acquire);
            let (status,body)=if line.starts_with("GET /tmdb/") {
                if phase==2 {(500,"{}")}else{(200,r#"{"title":"Fresh title","vote_average":8.2,"vote_count":12}"#)}
            } else if line.starts_with("GET /omdb/") {
                if phase==0 {(500,"{}")}else{(200,r#"{"Ratings":[{"Source":"Internet Movie Database","Value":"8.8/10"}]}"#)}
            } else if line.starts_with("GET /trakt/") {(200,r#"{"rating":8.6,"votes":12}"#)}else{panic!("unexpected fixture request")};
            write!(socket,"HTTP/1.1 {status} fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    struct Server {stop:Arc<AtomicBool>,thread:Option<std::thread::JoinHandle<()>>}
    impl Drop for Server {fn drop(&mut self){self.stop.store(true,Ordering::Release);self.thread.take().unwrap().join().unwrap();}}
    let _server=Server {stop,thread:Some(thread)};
    let mut config=MediaConfig::default();config.details_per_minute=60;config.tmdb.api_key="fixture".into();config.tmdb.requests_per_minute=-1;config.tmdb.base_url=Some(format!("{base}/tmdb"));
    config.tvdb.api_key.clear();config.omdb.api_key="fixture".into();config.omdb.requests_per_minute=-1;config.omdb.base_url=Some(format!("{base}/omdb"));
    config.trakt.client_id="fixture".into();config.trakt.requests_per_minute=-1;config.trakt.base_url=Some(format!("{base}/trakt"));
    let now=now_secs();let mut kept=old();kept.fetched_at=now-KEEP_FOR;
    runtime.owner.with(|client|pg_details::put_in(client,&kept)).unwrap();let ids=Ids {trakt:Some(900),..identity()};
    let service=Service::new(&runtime.owner,&config,None).unwrap();let got=service.for_title(1,&ids,now).unwrap().unwrap();
    assert_eq!((got.imdb_score,got.rotten,got.metacritic,got.trakt_score),(Some(81),Some(91),Some(61),Some(86)));assert_eq!(got.title.as_deref(),Some("Fresh title"));
    assert_eq!(now-got.fetched_at,KEEP_FOR-RETRY_AFTER);assert_eq!(requests.lock().unwrap().len(),3);
    assert_eq!(service.for_title(1,&ids,now+1).unwrap(),Some(got.clone()));assert_eq!(requests.lock().unwrap().len(),3);
    phase.store(1,Ordering::Release);let service=Service::new(&runtime.owner,&config,None).unwrap();let got=service.for_title(1,&ids,now+RETRY_AFTER).unwrap().unwrap();
    assert_eq!((got.imdb_score,got.rotten,got.metacritic,got.trakt_score),(Some(88),None,None,Some(86)));assert_eq!(got.fetched_at,now+RETRY_AFTER);
    runtime.owner.with(|client| {assert_eq!(pg_details::read_in(client,1,42)?,Some(got));
        let rows=(0..67).map(|n|crate::parser::Release {name:format!("Details.Identity.{n}"),group:"alt.binaries.details".into(),articles:vec![crate::parser::Article {
            filename:Some("video.mkv".into()),message_id:format!("<details-identity-{n}@fixture.test>"),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}],..Default::default()}).collect::<Vec<_>>();
        assert_eq!(crate::pg::release_writer::save(client,&rows,1)?.releases,67);
        client.execute("insert into media_release(release_id,kind,title_key,quality,tmdb_id) select id,1,id,0,1000000+id from releases",&[])?;
        // Explicit endpoint IDs avoid interpreting provider IDs as release IDs.
        let ids=client.query("select id from releases order by id desc",&[])?;let newest=ids[0].get::<_,i64>(0);let second=ids[1].get::<_,i64>(0);let oldest=ids.last().unwrap().get::<_,i64>(0);
        client.execute("update media_release set tmdb_id=42,imdb_id=1234567,tvdb_id=7,trakt_id=case when release_id=$1 then 900 when release_id=$2 then 901 else null end where release_id=any($3)",&[&second,&oldest,&vec![newest,second,oldest]])?;Ok(())
    }).unwrap();
    let baseline=requests.lock().unwrap().len();
    for fetched_at in [now,now-KEEP_FOR] {
        runtime.owner.with(|client| {let mut row=old();row.fetched_at=fetched_at;pg_details::put_in(client,&row)}).unwrap();
        let mut service=Service::new(&runtime.owner,&config,None).unwrap();service.start_without_delay_for_test();
        assert!(matches!(service.tick(now).unwrap(),Progress::Deferred));assert_eq!(requests.lock().unwrap().len(),baseline,"ambiguous identity cannot fetch even for expired details");
        runtime.owner.with(|client| {assert_eq!(pg_details::read_in(client,1,42)?.unwrap().fetched_at,fetched_at);Ok(())}).unwrap();
    }
    runtime.owner.with(|client| {client.execute("update media_release set trakt_id=900 where tmdb_id=42 and trakt_id is not null",&[])?;Ok(())}).unwrap();
    phase.store(2,Ordering::Release);let mut service=Service::new(&runtime.owner,&config,None).unwrap();service.start_without_delay_for_test();
    assert!(matches!(service.tick(now).unwrap(),Progress::Waiting(_)));assert!(service.pending.front().unwrap().trakt_missing,"provider pause retains original missing-ID provenance");
    let pending=service.pending.pop_front().unwrap();assert_eq!(pending.source.ids.trakt,Some(900));
    runtime.owner.with(|client| {client.execute("update media_release set trakt_id=null where tmdb_id=42",&[])?;Ok(())}).unwrap();
    phase.store(1,Ordering::Release);let service=Service::new(&runtime.owner,&config,None).unwrap();let before=requests.lock().unwrap().len();
    let got=service.for_title_with_trakt(1,&pending.source.ids,pending.trakt_missing,now).unwrap().unwrap();assert_eq!(got.trakt_score,None);
    let requests=requests.lock().unwrap();assert_eq!(requests.len()-before,2);assert!(requests[before..].iter().all(|path|!path.starts_with("GET /trakt/")));drop(requests);
    assert_eq!(runtime.owner.failure(),None);runtime.owner.close();
}
