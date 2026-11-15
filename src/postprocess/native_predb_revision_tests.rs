//! Real importer interleavings between native database preparation and settlement.
use super::*;
use crate::pg::{
    indexer_runtime::Runtime,
    predb_cache::{BuildStatus, ImportStatus, LookupStatus},
    service_cache::{Cache, predb::Service},
    stage_queue::{Claim, Queue, Settlement},
};
use crate::predb::{
    import::{Counts, Entry},
    matcher::Evidence,
};
use std::sync::atomic::Ordering::Relaxed;

const MOVIE: &str = "Some.Movie.2019.1080p.WEB-DL.DD5.1.H264-GRP";
const MOVIE_KEY: i64 = 7_250_957_560_060_540_805;
const HTTP_URL: &str = "https://example.invalid/predb-revision-sentinel";
const HTTP_BODY: &str = "unrelated cached response\nexact bytes: é";
const SOURCE: i64 = 1;
const DONE: i64 = (1 << 16) | (1 << 24) | (1 << 32);

fn observed<T>(runtime: &Runtime, body: impl FnOnce(&mut postgres::Transaction<'_>) -> Result<T>) -> T {
    runtime
        .owner
        .with(|client| {
            let mut tx = client
                .build_transaction()
                .isolation_level(postgres::IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()?;
            tx.batch_execute("set local statement_timeout='3s'; set local lock_timeout='250ms'")?;
            let value = body(&mut tx)?;
            tx.commit()?;
            Ok(value)
        })
        .unwrap()
}

// Exact bounded fixture facts, including compressed external cache bytes. No
// relation here contains production data: the existing harness owns the schema.
fn catalog(runtime: &Runtime) -> Vec<String> {
    observed(runtime, |tx| {
        [
            "select coalesce(json_agg(t order by id)::text,'[]') from ext_predb_titles t",
            "select coalesce(json_agg(t order by key,title_id)::text,'[]') from ext_predb_title_keys t",
            "select coalesce(json_agg(t order by singleton)::text,'[]') from ext_predb_key_meta t",
            "select coalesce(json_agg(t order by singleton)::text,'[]') from ext_predb_key_build t",
            "select coalesce(json_agg(t order by key)::text,'[]') from ext_predb_meta t",
            "select coalesce(json_agg(t order by source)::text,'[]') from ext_predb_feeds t",
            "select coalesce(json_agg(t order by cache_key)::text,'[]') from ext_http_cache t",
            "select coalesce(json_agg(t order by title_key)::text,'[]') from ext_media_titles t",
            "select coalesce(json_agg(t order by id)::text,'[]') from ext_predb_pages t",
            "select coalesce(json_agg(t order by seq)::text,'[]') from ext_predb_page_events t",
        ]
        .into_iter()
        .map(|sql| Ok(tx.query_one(sql, &[])?.try_get::<_, String>(0)?))
        .collect()
    })
}
fn lookup_status(runtime: &Runtime, name: &str) -> LookupStatus {
    observed(runtime, |tx| {
        let evidence = Evidence { name, display_name: None, size: 2 << 20, files: vec!["movie.mkv"], inner: vec![] };
        crate::pg::predb_cache::find_in(tx, &evidence)
    })
}
fn lookup(runtime: &Runtime, name: &str) -> (i64, Option<String>) {
    let LookupStatus::Ready(found) = lookup_status(runtime, name) else {
        panic!("completed matcher must remain Ready")
    };
    (found.revision, found.found.map(|hit| hit.title.title))
}

struct Fixture<'a> {
    runtime: &'a Runtime,
    service: Service,
    cache: Cache,
    known: Entry,
    known_id: i64,
}
impl<'a> Fixture<'a> {
    fn new(runtime: &'a Runtime) -> Self {
        let known = crate::predb::feeds::parse_predb_club(
            &serde_json::from_str(include_str!("../../tests/fixtures/predb/predb_club.json")).unwrap(),
            1,
        )
        .unwrap()
        .remove(0);
        assert_ne!(known.title, MOVIE);
        let service = Service::new(&runtime.owner).unwrap();
        let cache = Cache::new(&runtime.owner).unwrap();
        cache.put_http("revision_sentinel", HTTP_URL, 200, HTTP_BODY).unwrap();
        cache
            .put_title(&crate::media::store::TitleRow {
                key: 91,
                kind: 2,
                title: "Unrelated Sentinel".into(),
                year: Some(2001),
                state: crate::media::store::STATE_NOT_FOUND,
                checked_at: 11,
                next_check: Some(22),
                misses: 3,
                fanout_done: true,
                ..Default::default()
            })
            .unwrap();
        let known_id = runtime.owner.with(|client| {
            let mut tx = client.transaction()?;
            crate::pg::guard_native_transaction(&mut tx)?;
            tx.execute("insert into ext_predb_feeds(source,key,resume_page,gap_floor,last_ok_at,next_poll_at) values($1,'predb_club',41,7,10,999)", &[&SOURCE])?;
            let id = tx.query_one("insert into ext_predb_titles(title,source,category,size_bytes,files,pre_at,nuked,nuke_reason,added_at) values($1,$2,$3,$4,$5,$6,$7,$8,9) returning id",
                &[&known.title,&SOURCE,&known.category,&known.size_bytes,&known.files,&known.pre_at,&known.nuked,&known.nuke_reason])?.try_get(0)?;
            tx.commit()?; Ok(id)
        }).unwrap();
        // An unchanged page cannot certify a genuinely unbuilt, nonempty index.
        let pending = catalog(runtime);
        let counters = service.counters.snapshot();
        assert_eq!(
            service.import_page_for_test(SOURCE, &[known.clone()], 10, Some((42, 7))).unwrap_err().to_string(),
            "PreDB derived index pending"
        );
        assert_eq!(catalog(runtime), pending);
        assert_eq!(service.counters.snapshot(), counters);
        observed(runtime, |tx| {
            let e = Evidence { name: &known.title, display_name: None, size: 1, files: vec![], inner: vec![] };
            assert!(matches!(crate::pg::predb_cache::find_in(tx, &e)?, LookupStatus::Pending));
            Ok(())
        });
        let BuildStatus::Ready(built) = cache.predb_advance(512).unwrap() else {
            panic!("one-title build must reach EOF")
        };
        assert_eq!(built.titles, 1);
        let counts = service.import_page_for_test(SOURCE, &[known.clone()], 11, None).unwrap();
        assert_eq!(counts, Counts { skipped: 1, min_known_id: Some(known_id), ..Default::default() });
        assert_eq!(lookup(runtime, &known.title).1, Some(known.title.clone()));
        assert_eq!(lookup(runtime, MOVIE).1, None);
        Self { runtime, service, cache, known, known_id }
    }
    fn import(&self, entries: &[Entry], now: i64) -> Counts {
        self.service.import_page_for_test(SOURCE, entries, now, Some((42, 7))).unwrap()
    }
    fn sentinels(&self) {
        assert_eq!(self.cache.http("revision_sentinel", HTTP_URL).unwrap(), Some((200, HTTP_BODY.into())));
        let title = self.cache.title(91).unwrap().unwrap();
        assert_eq!(title.title, "Unrelated Sentinel");
        assert_eq!((title.checked_at, title.next_check, title.misses, title.fanout_done), (11, Some(22), 3, true));
        self.cache.check().unwrap();
        assert_eq!(self.runtime.owner.failure(), None);
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Durable {
    revision: i64,
    state: Option<(i64, i64)>,
    category: Option<i64>,
    media: Option<(i64, i64)>,
    receipts: i64,
    queue: Option<(i64, i64, Option<i64>, Option<i64>)>,
}
fn durable(runtime: &Runtime, claim: &Claim) -> Durable {
    observed(runtime, |tx| {
        let id = claim.release_id();
        let revision = tx.query_one("select native_work_revision from releases where id=$1", &[&id])?.try_get(0)?;
        let state = tx
            .query_opt("select done,failed from pp_state where release_id=$1", &[&id])?
            .map(|r| Ok::<_, anyhow::Error>((r.try_get(0)?, r.try_get(1)?)))
            .transpose()?;
        let category = tx
            .query_opt("select category from release_category where release_id=$1", &[&id])?
            .map(|r| r.try_get(0))
            .transpose()?;
        let media = tx
            .query_opt("select kind,title_key from media_release where release_id=$1", &[&id])?
            .map(|r| Ok::<_, anyhow::Error>((r.try_get(0)?, r.try_get(1)?)))
            .transpose()?;
        let receipts = tx
            .query_one("select count(*) from native_operations where operation_id=$1", &[&&claim.operation_id()[..]])?
            .try_get(0)?;
        let queue = tx
            .query_opt(
                "select generation,work_serial,lease_token,lease_until from pp_queue where release_id=$1",
                &[&id],
            )?
            .map(|r| Ok::<_, anyhow::Error>((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?, r.try_get(3)?)))
            .transpose()?;
        Ok(Durable { revision, state, category, media, receipts, queue })
    })
}
fn unpublished(facts: &Durable) {
    assert_eq!((&facts.state, &facts.category, &facts.media, facts.receipts), (&None, &None, &None, 0));
}
fn classified(settlement: Settlement, facts: &Durable, runtime: &Runtime) {
    assert_eq!(
        (
            settlement,
            facts.state,
            facts.category,
            facts.media,
            facts.receipts,
            facts.queue.is_none(),
            runtime.owner.failure(),
            runtime.owner.stop.load(Relaxed)
        ),
        (Settlement::Applied, Some((DONE, 0)), Some(2040), Some((2, MOVIE_KEY)), 1, true, None, false),
        "durable movie classification after actual settlement: {facts:?}"
    );
}
fn superseded(facts: &Durable, before: &Durable) {
    unpublished(facts);
    assert_eq!(facts.revision, before.revision);
    let (generation, serial, _, _) = before.queue.unwrap();
    assert_eq!(facts.queue, Some((generation, serial + 1, None, None)));
}

fn save_movie(runtime: &Runtime, ordinal: usize) -> i64 {
    let release = crate::parser::Release {
        name: MOVIE.into(),
        group: "alt.binaries.movies".into(),
        poster: "fixture".into(),
        date: "Wed, 01 Jan 2020 00:00:00 +0000".into(),
        articles: (1..=2)
            .map(|part| crate::parser::Article {
                filename: Some("movie.mkv".into()),
                subject: format!("movie.mkv yEnc ({part}/2)"),
                message_id: format!("revision-{ordinal}-{part}@example.invalid"),
                part: Some(part),
                total_parts: Some(2),
                file_index: Some(1),
                file_total: Some(1),
                bytes: 1 << 20,
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    runtime
        .owner
        .with(|client| {
            let saved = crate::pg::release_writer::save(client, &[release], 20)?;
            assert_eq!(saved, crate::pg::release_writer::SaveStats { releases: 1, files: 1, parts: 2 });
            Ok(client.query_one("select id from releases where name=$1", &[&MOVIE])?.try_get(0)?)
        })
        .unwrap()
}
fn runner(runtime: &Runtime) -> (tokio::runtime::Runtime, Runner) {
    let mut config = crate::config::Config::default();
    config.predb.enabled = true;
    config.category.enabled = true;
    config.media.parse = true;
    config.postprocess.min_size_bytes = 0;
    config.postprocess.min_files = 1;
    config.postprocess.settle_secs = 1;
    config.postprocess.complete_settle_secs = 1;
    let handle = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut runner =
        Runner::new(&runtime.owner, &config, Arc::new(crate::nntp::Pool::new(&[])), handle.handle().clone()).unwrap();
    let [parse, _, _] = crate::media::native_steps(&runtime.owner, &config).unwrap();
    let ordered: Vec<Arc<dyn Step>> =
        vec![Arc::new(crate::predb::step::PredbMatch), Arc::new(crate::category::CategoryStep::new()), parse];
    assert_eq!(ordered.iter().map(|s| s.rank()).collect::<Vec<_>>(), vec![300, 400, 500]);
    let mut enabled = 0;
    for step in &ordered {
        assert!(step.enabled(&config));
        assert_eq!(step.needs(), Needs::Database);
        enabled |= mask(step.id());
    }
    assert_eq!(enabled, DONE);
    runner.steps = Steps { ordered, enabled, database: enabled, network: 0, on_growth: enabled, stamps: Vec::new() };
    (handle, runner)
}
fn prepare(runtime: &Runtime, runner: &Runner, queue: &Queue, id: i64, now: i64) -> (Claim, Prepared, Durable) {
    let mut claims = queue.claim(now, 1, 60).unwrap();
    assert_eq!(claims.len(), 1);
    let claim = claims.remove(0);
    assert_eq!(claim.release_id(), id);
    let before = durable(runtime, &claim);
    unpublished(&before);
    let attempted = runner.stats.steps_done.load(Relaxed);
    let prepared = runner.database(&claim, claim.redo_mask(), now).unwrap();
    assert_eq!(runner.stats.steps_done.load(Relaxed) - attempted, 3, "all three real Steps must finish before import");
    assert_eq!(durable(runtime, &claim), before, "Prepared must buffer all classification and done writes");
    (claim, prepared, before)
}
fn now(runtime: &Runtime) -> i64 {
    observed(runtime, |tx| {
        Ok(tx.query_one("select greatest(coalesce(max(due_at),0),(select coalesce(max(touched_at),0) from files)) + 10 from pp_queue", &[])?.try_get(0)?)
    })
}
fn positive_control(runtime: &Runtime, ordinal: usize) {
    let id = save_movie(runtime, ordinal);
    let at = now(runtime);
    let (_handle, runner) = runner(runtime);
    let queue = Queue::for_owner(&runtime.owner).unwrap();
    let (claim, prepared, _) = prepare(runtime, &runner, &queue, id, at);
    let settlement = prepared.settle(&queue, &claim, at + 1).unwrap();
    let facts = durable(runtime, &claim);
    eprintln!(
        "PreDB revision phase=no-import settlement={settlement:?} durable={facts:?} owner={:?}",
        runtime.owner.failure()
    );
    classified(settlement, &facts, runtime);
    runtime.owner.with(|client| Ok(client.execute("delete from releases where id=$1", &[&id])?)).unwrap();
}

#[test]
fn no_import_control_persists_buffered_movie_classification() {
    crate::media::with_native_test_runtime(
        "postprocess::native::predb_revision_tests::no_import_control_persists_buffered_movie_classification",
        |runtime| {
            let fixture = Fixture::new(runtime);
            positive_control(runtime, 0);
            fixture.sentinels();
        },
    );
}
#[test]
fn noop_feed_page_keeps_buffered_movie_classification() {
    crate::media::with_native_test_runtime(
        "postprocess::native::predb_revision_tests::noop_feed_page_keeps_buffered_movie_classification",
        |runtime| {
            let fixture = Fixture::new(runtime);
            positive_control(runtime, 0);
            let id = save_movie(runtime, 1);
            let at = now(runtime);
            let (_handle, runner) = runner(runtime);
            let queue = Queue::for_owner(&runtime.owner).unwrap();
            let (claim, prepared, before) = prepare(runtime, &runner, &queue, id, at);
            let old = lookup(runtime, MOVIE);
            let saved = catalog(runtime);
            let counts = fixture.import(&[fixture.known.clone()], at);
            let settlement = prepared.settle(&queue, &claim, at + 1).unwrap();
            let facts = durable(runtime, &claim);
            let fresh = lookup_status(runtime, MOVIE);
            let after = catalog(runtime);
            eprintln!(
                "PreDB revision phase=no-op counts={counts:?} settlement={settlement:?} before={before:?} durable={facts:?} matcher={old:?}->{fresh:?} owner={:?}",
                runtime.owner.failure()
            );
            // The intended RED is the durable tuple, after the identical positive
            // control and actual settlement, never a revision-counter-only failure.
            classified(settlement, &facts, runtime);
            assert_eq!(counts, Counts { skipped: 1, min_known_id: Some(fixture.known_id), ..Default::default() });
            let LookupStatus::Ready(fresh) = fresh else { panic!("unchanged import left completed keys Pending") };
            assert_eq!((fresh.revision, fresh.found.map(|hit| hit.title.title)), old);
            assert_eq!(after, saved);
            fixture.sentinels();
        },
    );
}
#[test]
fn changed_matcher_metadata_still_supersedes_buffered_classification() {
    crate::media::with_native_test_runtime(
        "postprocess::native::predb_revision_tests::changed_matcher_metadata_still_supersedes_buffered_classification",
        |runtime| {
            let fixture = Fixture::new(runtime);
            let id = save_movie(runtime, 0);
            let at = now(runtime);
            let (_handle, runner) = runner(runtime);
            let queue = Queue::for_owner(&runtime.owner).unwrap();
            let (claim, prepared, before) = prepare(runtime, &runner, &queue, id, at);
            let old = lookup(runtime, MOVIE).0;
            let mut changed = fixture.known.clone();
            changed.nuked = 1;
            changed.nuke_reason = Some("changed metadata".into());
            assert_eq!(
                fixture.import(&[changed], at),
                Counts { updated: 1, min_known_id: Some(fixture.known_id), ..Default::default() }
            );
            assert!(lookup(runtime, MOVIE).0 > old);
            assert_eq!(prepared.settle(&queue, &claim, at + 1).unwrap(), Settlement::Superseded);
            superseded(&durable(runtime, &claim), &before);
            let (retry, prepared, _) = prepare(runtime, &runner, &queue, id, at + 2);
            let settled = prepared.settle(&queue, &retry, at + 3).unwrap();
            classified(settled, &durable(runtime, &retry), runtime);
            fixture.sentinels();
        },
    );
}
#[test]
fn matching_title_insert_supersedes_old_no_match_and_retry_applies() {
    crate::media::with_native_test_runtime(
        "postprocess::native::predb_revision_tests::matching_title_insert_supersedes_old_no_match_and_retry_applies",
        |runtime| {
            let fixture = Fixture::new(runtime);
            let id = save_movie(runtime, 0);
            let at = now(runtime);
            let (_handle, runner) = runner(runtime);
            let queue = Queue::for_owner(&runtime.owner).unwrap();
            let (claim, prepared, before) = prepare(runtime, &runner, &queue, id, at);
            let old = lookup(runtime, MOVIE);
            assert!(old.1.is_none());
            let movie = Entry { title: MOVIE.into(), category: Some("X264".into()), ..Default::default() };
            assert_eq!(fixture.import(&[movie], at), Counts { inserted: 1, ..Default::default() });
            let fresh = lookup(runtime, MOVIE);
            assert!(fresh.0 > old.0);
            assert_eq!(fresh.1, Some(MOVIE.into()));
            // The external import has changed neither header input nor the claim.
            assert_eq!(durable(runtime, &claim), before);
            assert_eq!(prepared.settle(&queue, &claim, at + 1).unwrap(), Settlement::Superseded);
            superseded(&durable(runtime, &claim), &before);
            let (retry, prepared, _) = prepare(runtime, &runner, &queue, id, at + 2);
            let settled = prepared.settle(&queue, &retry, at + 3).unwrap();
            classified(settled, &durable(runtime, &retry), runtime);
            observed(runtime, |tx| {
                let row=tx.query_one("select t.title from predb_match p join ext_predb_titles t on t.id=p.title_id where p.release_id=$1", &[&id])?;
                assert_eq!(row.try_get::<_, String>(0)?, MOVIE);
                Ok(())
            });
            fixture.sentinels();
        },
    );
}
#[test]
fn noop_multichunk_and_rolled_back_import_preserve_facts() {
    crate::media::with_native_test_runtime(
        "postprocess::native::predb_revision_tests::noop_multichunk_and_rolled_back_import_preserve_facts",
        |runtime| {
            let fixture = Fixture::new(runtime);
            let saved = catalog(runtime);
            let old = lookup(runtime, MOVIE);
            let counters = fixture.service.counters.snapshot();
            let oversized_page = vec![fixture.known.clone(); 5001];
            assert_eq!(
                fixture.service.import_page_for_test(SOURCE, &oversized_page, 30, None).unwrap_err().to_string(),
                "PreDB parsed page exceeds row admission"
            );
            let oversized_text = Entry { title: "x".repeat(8 * 1024 * 1024 + 1), ..Default::default() };
            assert_eq!(
                fixture.service.import_page_for_test(SOURCE, &[oversized_text], 30, None).unwrap_err().to_string(),
                "PreDB parsed page exceeds byte admission"
            );
            assert_eq!(catalog(runtime), saved);
            assert_eq!(fixture.service.counters.snapshot(), counters);
            let page = vec![fixture.known.clone(); 513];
            assert_eq!(
                fixture.import(&page, 30),
                Counts { skipped: 513, min_known_id: Some(fixture.known_id), ..Default::default() }
            );
            assert_eq!(
                catalog(runtime),
                saved,
                "both actual 512-entry chunks must preserve certification and feed facts"
            );
            assert_eq!(lookup(runtime, MOVIE), old);
            let events = fixture.service.counters.snapshot();
            assert_eq!(
                events["sources"][0]["events"]["skipped"].as_u64().unwrap(),
                counters["sources"][0]["events"]["skipped"].as_u64().unwrap() + 513
            );
            let id = save_movie(runtime, 0);
            let at = now(runtime);
            let (_handle, runner) = runner(runtime);
            let queue = Queue::for_owner(&runtime.owner).unwrap();
            let (claim, prepared, before) = prepare(runtime, &runner, &queue, id, at);
            let saved = catalog(runtime);
            let counters = fixture.service.counters.snapshot();
            runtime
                .owner
                .with(|client| {
                    let mut tx = client.transaction()?;
                    crate::pg::guard_native_transaction(&mut tx)?;
                    tx.batch_execute("set local statement_timeout='3s'; set local lock_timeout='250ms'")?;
                    let movie = Entry { title: MOVIE.into(), ..Default::default() };
                    let ImportStatus::Applied { counts, revision } =
                        crate::pg::predb_cache::import_page_in(&mut tx, SOURCE, &[movie], at, Some((42, 7)))?
                    else {
                        panic!("complete index became Pending")
                    };
                    assert_eq!(counts.inserted, 1);
                    assert!(revision > old.0);
                    let feed =
                        tx.query_one("select resume_page,inserted from ext_predb_feeds where source=$1", &[&SOURCE])?;
                    assert_eq!((feed.try_get::<_, i64>(0)?, feed.try_get::<_, i64>(1)?), (42, 1));
                    assert_eq!(
                        tx.query_one("select count(*) from ext_predb_titles where title=$1", &[&MOVIE])?
                            .try_get::<_, i64>(0)?,
                        1
                    );
                    tx.rollback()?;
                    Ok(())
                })
                .unwrap();
            assert_eq!(catalog(runtime), saved);
            assert_eq!(fixture.service.counters.snapshot(), counters);
            assert_eq!(lookup(runtime, MOVIE), old);
            assert_eq!(durable(runtime, &claim), before);
            let settled = prepared.settle(&queue, &claim, at + 1).unwrap();
            classified(settled, &durable(runtime, &claim), runtime);
            fixture.sentinels();
        },
    );
}
