//! Registered as a test child of postprocess::native so fixture Steps use its
//! real private runner and queue, without exposing mutation authority.
use super::*;
use crate::postprocess::step::{Outcome, StepCtx, StepError, StepId, StepMutation, StepWrite};

#[derive(Clone, Copy)]
enum Action {
    Tags,
    Found,
}
struct MediaFixture(Action);
impl Step for MediaFixture {
    fn id(&self) -> StepId {
        StepId(32)
    }
    fn name(&self) -> &'static str {
        "media_tags_fixture"
    }
    fn rank(&self) -> u16 {
        500
    }
    fn needs(&self) -> Needs {
        Needs::Database
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let id = ctx.release().id;
        let writes = match self.0 {
            Action::Tags => {
                let info = crate::probe::media::MediaInfo {
                    tags: [("IMDB", "tt0133093"), ("TMDB", "movie/603"), ("TVDB", "81189")]
                        .into_iter()
                        .map(|(key, value)| (key.to_owned(), value.to_owned()))
                        .collect(),
                    ..Default::default()
                };
                crate::probe::step::title_writes(id, Some(&info))
            }
            Action::Found => vec![StepWrite::for_mutation(
                id,
                StepMutation::FoundMediaIds {
                    title_key: 77,
                    ids: crate::media::resolve::Ids {
                        tvdb: Some(5),
                        tvmaze: Some(6),
                        trakt: Some(7),
                        tmdb: Some(999),
                        imdb: Some(888),
                        tvrage: Some(4),
                    },
                },
            )],
        };
        Ok(Outcome { matched: true, writes, ..Default::default() })
    }
}
fn turn(runtime: &crate::pg::indexer_runtime::Runtime, action: Action, now: i64) {
    let handle = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut runner = Runner::new(
        &runtime.owner,
        &crate::config::Config::default(),
        Arc::new(crate::nntp::Pool::new(&[])),
        handle.handle().clone(),
    )
    .unwrap();
    let step: Arc<dyn Step> = Arc::new(MediaFixture(action));
    let bit = mask(step.id());
    runner.steps =
        Steps { ordered: vec![step], enabled: bit, database: bit, network: 0, on_growth: 0, stamps: Vec::new() };
    let queue = crate::pg::stage_queue::Queue::for_owner(&runtime.owner).unwrap();
    let claims = queue.claim(now, 1, 60).unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(
        runner.database(&claims[0], 0, now).unwrap().settle(&queue, &claims[0], now + 1).unwrap(),
        crate::pg::stage_queue::Settlement::Applied
    );
}
fn rerun(runtime: &crate::pg::indexer_runtime::Runtime, id: i64, now: i64) {
    runtime
        .owner
        .with(|client| {
            let mut tx = client.transaction()?;
            assert!(crate::pg::stage_queue::rerun_in(&mut tx, id, mask(StepId(32)), now)?);
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}
fn ids(runtime: &crate::pg::indexer_runtime::Runtime, id: i64) -> (Option<i64>, Option<i64>, Option<i64>) {
    runtime
        .owner
        .with(|client| {
            let row =
                client.query_one("select imdb_id,tmdb_id,tvdb_id from media_release where release_id=$1", &[&id])?;
            Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?))
        })
        .unwrap()
}
#[test]
fn a_files_own_tags_pin_its_ids_against_a_lookup_by_name() {
    let Some(schema) = crate::pg::testing::Schema::new() else {
        eprintln!("skipping PostgreSQL test: ATLAS_PG_TEST_URL is required");
        return;
    };
    let runtime = crate::pg::stage_queue_tests::runtime(&schema);
    crate::pg::stage_queue_tests::push(&runtime);
    let id = runtime
        .owner
        .with(|client| {
            let id = client.query_one("select id from releases", &[])?.try_get::<_, i64>(0)?;
            client.execute("insert into media_release(release_id,kind,title_key,quality) values($1,2,77,0)", &[&id])?;
            Ok(id)
        })
        .unwrap();
    turn(&runtime, Action::Tags, 10);
    assert_eq!(ids(&runtime, id), (Some(133_093), Some(603), None));
    rerun(&runtime, id, 20);
    turn(&runtime, Action::Found, 20);
    assert_eq!(ids(&runtime, id), (Some(133_093), Some(603), Some(5)));
    // Fixture baseline for a TV row: result writes still go through the queue.
    runtime.owner.with(|client|Ok(client.execute("update media_release set kind=1,imdb_id=null,tmdb_id=null,tvdb_id=null,imdb_from_nfo=0,tmdb_from_tag=0,tvdb_from_tag=0 where release_id=$1",&[&id])?)).unwrap();
    rerun(&runtime, id, 30);
    turn(&runtime, Action::Tags, 30);
    assert_eq!(ids(&runtime, id), (None, None, Some(81_189)));
    assert_eq!(runtime.owner.failure(), None);
}
