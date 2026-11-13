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
    assert_eq!(merged.len(),1);assert_eq!(merged[0].release,20);assert_eq!(merged[0].ids,richer);
}

#[test]
fn conflicting_provider_identities_are_not_merged_or_overwritten() {
    let base=identity();let mut one=base;one.trakt=Some(900);let mut another=base;another.trakt=Some(901);
    let merged=merge_sources(vec![source(20,one),source(10,another)]);assert_eq!(merged.len(),2);assert_eq!(merged[0].ids.trakt,Some(900));assert_eq!(merged[1].ids.trakt,Some(901));
    for other in [Ids {tmdb:Some(43),..base},Ids {imdb:Some(7654321),..base},Ids {tvdb:Some(8),..base},Ids {tvmaze:Some(8),..base},Ids {tvrage:Some(8),..base}] {
        let merged=merge_sources(vec![source(20,base),source(10,Ids {trakt:Some(900),..other})]);assert_eq!(merged.len(),2);assert_eq!(merged[0].ids.trakt,None);
    }
    let mut tv=source(10,Ids {trakt:Some(900),..base});tv.kind=2;
    assert_eq!(merge_sources(vec![source(20,base),tv]).len(),2);
}
