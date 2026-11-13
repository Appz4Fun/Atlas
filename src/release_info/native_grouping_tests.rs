//! Original release-grouping oracles on a canonical native Runtime and writer.
//! Fixture labels select returned native IDs; they never become catalog IDs.
use super::*;
use crate::pg::stage_queue::{Queue, Settlement, Visit, participants::ReadTargets};
use crate::postprocess::{
    apply::Applied,
    step::{StepMutation, StepWrite},
};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    time::{Duration, Instant},
};

struct Fixture {
    runtime: crate::pg::indexer_runtime::Runtime,
    schema: crate::pg::testing::Schema,
    _root: tempfile::TempDir,
    pending: RefCell<BTreeMap<(i64, String), crate::parser::Release>>,
    ids: RefCell<BTreeMap<(i64, String), i64>>,
}
impl Fixture {
    fn new() -> Self {
        std::env::var("ATLAS_PG_TEST_URL").expect("explicit isolated ATLAS_PG_TEST_URL required");
        let schema =
            crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required; never skipped success");
        let root = tempfile::tempdir().unwrap();
        let mut cfg = crate::config::Config::default();
        cfg.postprocess.enabled = false;
        cfg.predb.enabled = false;
        cfg.spots.enabled = false;
        cfg.inferred_parts = false;
        cfg.media.tvmaze.enabled = false;
        cfg.media.allocine.enabled = false;
        cfg.media.details_per_minute = 0;
        let runtime = crate::pg::indexer_runtime::Runtime::open(
            &cfg,
            crate::pg::StorageConfig {
                backend: crate::pg::Backend::Postgres,
                url: schema.url.clone(),
                schema: schema.name.clone(),
                max_connections: 1,
                allow_plaintext: true,
            },
            crate::config::HeaderStoreConfig { enabled: true, dir: root.path().to_str().unwrap().into() },
            &[],
            &mut None,
        )
        .unwrap();
        Self { runtime, schema, _root: root, pending: Default::default(), ids: Default::default() }
    }
    fn flush(&self) {
        let mut pending = self.pending.borrow_mut();
        let mut rows = pending
            .iter()
            .filter(|(key, _)| !self.ids.borrow().contains_key(*key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        // Preserve the original oracle's ordering without assigning a native ID.
        rows.sort_by_key(|((label, _), _)| *label);
        for (key, release) in rows {
            let id = self
                .runtime
                .owner
                .with(|client| {
                    crate::pg::release_writer::save(client, std::slice::from_ref(&release), 1000)?;
                    Ok(client
                        .query_one(
                            "select id from releases where group_name=$1 and original_name=$2",
                            &[&release.group, &release.name.as_bytes()],
                        )?
                        .get(0))
                })
                .unwrap();
            assert!(self.ids.borrow_mut().insert(key, id).is_none());
        }
        pending.clear();
    }
    fn put_row(&self, label: i64, name: &str, group: &str, poster: &str, files: &[(&str, &[i64], i64)]) {
        self.put_row_at(label, name, group, poster, files, "2026-11-03 07:36:18");
    }
    fn put_row_at(
        &self,
        label: i64,
        name: &str,
        group: &str,
        poster: &str,
        files: &[(&str, &[i64], i64)],
        posted: &str,
    ) {
        self.put_row_counted(label, name, group, poster, files, posted, 2);
    }
    #[allow(clippy::too_many_arguments)]
    fn put_row_counted(
        &self,
        label: i64,
        name: &str,
        group: &str,
        poster: &str,
        files: &[(&str, &[i64], i64)],
        posted: &str,
        total: i64,
    ) {
        self.put_row_subjects(label, name, group, poster, files, posted, total, false);
    }
    #[allow(clippy::too_many_arguments)]
    fn put_row_subjects(
        &self,
        label: i64,
        name: &str,
        group: &str,
        poster: &str,
        files: &[(&str, &[i64], i64)],
        posted: &str,
        total: i64,
        sized: bool,
    ) {
        self.put_row_upload(label, name, group, poster, files, posted, total, sized.then_some(""));
    }
    #[allow(clippy::too_many_arguments)]
    fn put_row_upload(
        &self,
        label: i64,
        name: &str,
        group: &str,
        poster: &str,
        files: &[(&str, &[i64], i64)],
        posted: &str,
        total: i64,
        upload: Option<&str>,
    ) {
        let date = chrono::NaiveDateTime::parse_from_str(posted, "%Y-%m-%d %H:%M:%S").unwrap().and_utc().to_rfc2822();
        let mut headers = Vec::new();
        for (file, parts, of) in files {
            for part in *parts {
                headers.push(crate::nntp::Overview {
                    number: headers.len() as u64 + 1,
                    subject: format!(
                        "[{}/{total}] - \"{file}\" yEnc ({part}/{of}){}",
                        if file.ends_with(".mkv") { 1 } else { 2 },
                        upload.map_or(String::new(), |_| format!(" {}", of * 716_800))
                    ),
                    from: poster.into(),
                    date: date.clone(),
                    message_id: format!("<{file}.{part}{}@grouping.fixture.test>", upload.unwrap_or_default()),
                    references: String::new(),
                    bytes: 100,
                    lines: 1,
                    xref_groups: None,
                });
            }
        }
        let articles = crate::nntp::headers_to_articles(headers);
        assert!(!articles.is_empty());
        assert!(articles.iter().all(|a| a.filename.is_some() && a.part.is_some()));
        let release = crate::parser::Release {
            name: name.into(),
            group: group.into(),
            poster: poster.into(),
            date: posted.into(),
            size: articles.iter().map(|a| a.bytes).sum(),
            articles,
            complete: false,
            ..Default::default()
        };
        assert!(!self.ids.borrow().contains_key(&(label, group.into())), "fixture label already saved");
        assert!(self.pending.borrow_mut().insert((label, group.into()), release).is_none());
    }
    fn row_of(&self, label: i64, group: &str) -> crate::search::ReleaseRow {
        self.flush();
        let id = self.ids.borrow()[&(label, group.into())];
        self.runtime
            .owner
            .with(|client| {
                let view = crate::pg::fresh_reader::ReadView::open(
                    client,
                    &self.schema.name,
                    Default::default(),
                    Instant::now() + Duration::from_secs(10),
                )?;
                view.release(id)?.ok_or_else(|| anyhow::anyhow!("real native fixture row missing"))
            })
            .unwrap()
    }
    fn whole_post(&self, row: &crate::search::ReleaseRow) -> Result<Option<SplitPost>> {
        self.flush();
        crate::repair::native::test_whole_post(&self.runtime.owner, row.id)
    }
    fn linked_post(&self, id: i64) -> Result<Option<SplitPost>> {
        self.flush();
        crate::repair::native::test_linked_post(&self.runtime.owner, id)
    }
    fn whole(&self, id: i64) -> Result<bool> {
        self.flush();
        crate::repair::native::test_whole(&self.runtime.owner, &[id])
    }
    fn permille(&self, ids: &[i64]) -> Result<i64> {
        self.flush();
        crate::repair::native::test_permille(&self.runtime.owner, ids)
    }
    fn mutate(&self, source: i64, targets: &[i64], mutations: Vec<StepMutation>) {
        self.flush();
        let queue = Queue::for_owner(&self.runtime.owner).unwrap();
        let now = 2_000_000_000;
        let mut mutations = Some(mutations);
        let group = self
            .runtime
            .owner
            .with(|c| Ok(c.query_one("select group_name from releases where id=$1", &[&source])?.get::<_, String>(0)))
            .unwrap();
        queue.request_walk(4, Some(&group), now).unwrap();
        for _ in 0..512 {
            queue.walk_tick(now).unwrap();
            let mut claims = queue.claim(now, 256, 60).unwrap();
            // Settle unrelated fixture work first; observe participants afterward.
            claims.sort_by_key(|claim| claim.release_id() == source);
            for claim in claims {
                let selected = claim.release_id() == source;
                let id = claim.release_id();
                let mut observed = ReadTargets::for_owner(&self.runtime.owner).unwrap();
                let (rerun,input)=self.runtime.owner.with(|client| {
                    let mut tx=client.build_transaction().isolation_level(postgres::IsolationLevel::RepeatableRead).read_only(true).start()?;
                    let mut hash=Sha256::new();
                    for row in tx.query("select id,native_work_revision,original_name from releases where id=$1 or id=any($2) order by id",&[&id,&targets])? {
                        hash.update(row.get::<_,i64>(0).to_le_bytes());hash.update(row.get::<_,i64>(1).to_le_bytes());hash.update(row.get::<_,Vec<u8>>(2));
                    }
                    let rerun=tx.query_one("select coalesce((select rerun_gen from pp_state where release_id=$1),0)",&[&id])?.get::<_,i64>(0);
                    observed.observe_in(&mut tx,if selected{targets}else{&[]})?;tx.commit()?;Ok((rerun,<[u8;32]>::from(hash.finalize())))
                }).unwrap();
                let writes = if selected {
                    mutations.take().unwrap().into_iter().map(|m| (4, StepWrite::for_mutation(source, m))).collect()
                } else {
                    Vec::new()
                };
                let applied = Applied {
                    release_id: id,
                    claimed_gen: rerun,
                    clear: claim.redo_mask(),
                    set_done: 4,
                    writes,
                    ..Default::default()
                };
                assert_eq!(
                    queue
                        .settle(
                            &claim,
                            Visit::prepare_observed(&claim, applied, input, observed.finish()).unwrap(),
                            now + 1
                        )
                        .unwrap(),
                    Settlement::Applied
                );
                if selected {
                    return;
                }
            }
        }
        panic!("bounded native fixture walk did not claim its source");
    }
    fn beside(&self, owner: i64, row: i64) {
        self.mutate(
            owner,
            &[row],
            vec![StepMutation::PlaceBesideFile { row_id: row, owner_id: owner, par2_file: Some(vec![7; 16]) }],
        );
    }
    fn remove_parts(&self, id: i64, name: &str) {
        self.runtime.owner.with(|c|{c.execute("delete from file_parts where file_id in(select id from files where release_id=$1 and original_filename=$2)",&[&id,&name.as_bytes()])?;Ok(())}).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.runtime.owner.close();
    }
}

fn isolated(name: &str, body: impl FnOnce()) {
    const CHILD: &str = "ATLAS_TEST_NATIVE_GROUPING_CHILD";
    if std::env::var(CHILD).ok().as_deref() == Some(name) {
        body();
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--ignored", "--nocapture", "--test-threads=1"])
        .env_clear()
        .env(CHILD, name)
        .env("ATLAS_HOME", home.path())
        .env("HOME", home.path())
        .env("ATLAS_PG_TEST_URL", std::env::var("ATLAS_PG_TEST_URL").expect("explicit isolated PostgreSQL required"))
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "actual native grouping child failed: {status}");
            break;
        }
        if Instant::now() >= until {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("native grouping fixture deadline exceeded");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn groups_in_one_shard(n: usize) -> Vec<String> {
    let shard = crate::catalog_facts::shard_of("alt.binaries.zz.a");
    (0..4000)
        .map(|i| format!("alt.binaries.g{i:04}"))
        .filter(|g| crate::catalog_facts::shard_of(g) == shard)
        .take(n)
        .collect()
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_post_with_a_poster_for_each_group_is_one_post() {
    isolated("release_info::native_grouping_tests::a_post_with_a_poster_for_each_group_is_one_post", || {
        let fixture = Fixture::new();
        let name = "Some.Film.2007.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.Atmos-GRP";
        let (mkv, vol) = ("Some.Film.mkv", "Some.Film.vol00+01.par2");
        let (a, b, c, d) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c", "alt.binaries.d");
        // the film's 8 parts across three groups, each with its own poster, and the group b row crossposted to d
        fixture.put_row(1, name, a, "x1@nyuu", &[(mkv, &[1, 4, 7], 8), (vol, &[1], 2)]);
        fixture.put_row(2, name, b, "x2@nyuu", &[(mkv, &[2, 5, 8], 8), (vol, &[2], 2)]);
        fixture.put_row(3, name, c, "x3@nyuu", &[(mkv, &[3, 6], 8)]);
        fixture.put_row(4, name, d, "x2@nyuu", &[(mkv, &[2, 5, 8], 8), (vol, &[2], 2)]);
        let ids: Vec<i64> = [(1, a), (2, b), (3, c)].iter().map(|(i, g)| fixture.row_of(*i, g).id).collect();
        let mut want = ids.clone();
        want.sort_unstable();
        for (i, g) in [(1, a), (2, b), (3, c), (4, d)] {
            let post = fixture.whole_post(&fixture.row_of(i, g)).unwrap().unwrap_or_else(|| panic!("row {i}"));
            assert_eq!(post.ids, want, "row {i} gets the same answer");
            assert!(post.complete);
            assert_eq!(post.groups, [a, b, c, d]);
        }
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_release_counted_with_its_thumbnail_is_whole_once_the_thumbnail_is_found() {
    isolated(
        "release_info::native_grouping_tests::a_release_counted_with_its_thumbnail_is_whole_once_the_thumbnail_is_found",
        || {
            let fixture = Fixture::new();
            let g = "alt.binaries.a";
            // `[n/4]`: a film not named as the post and two volumes here, the thumbnail posted apart as one of the four
            fixture.put_row_counted(
                1,
                "Some.Album.2026",
                g,
                "a@x",
                &[
                    ("Some.Film.2160p.mkv", &[1, 2], 2),
                    ("Some.Film.2160p.par2", &[1], 1),
                    ("Some.Film.2160p.vol0+1.par2", &[1], 1),
                ],
                "2026-11-03 07:36:18",
                4,
            );
            let lead = fixture.row_of(1, g).id;
            assert!(!fixture.whole(lead).unwrap(), "three of the four files");
            fixture.put_row_counted(
                2,
                "00-thumbnail.jpg",
                g,
                "b@x",
                &[("00-thumbnail.jpg", &[1], 1)],
                "2026-11-03 07:36:19",
                4,
            );
            let thumb = fixture.row_of(2, g).id;
            fixture.beside(lead, thumb);
            let post = fixture.whole_post(&fixture.row_of(1, g)).unwrap().unwrap();
            assert!(post.complete, "the four files together");
        },
    );
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_post_cut_across_groups_keeps_its_rows_beside_a_thumbnail_one_of_them_found() {
    isolated(
        "release_info::native_grouping_tests::a_post_cut_across_groups_keeps_its_rows_beside_a_thumbnail_one_of_them_found",
        || {
            let fixture = Fixture::new();
            let name = "Some.Film.2007.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.Atmos-GRP";
            let (mkv, vol) = ("Some.Film.mkv", "Some.Film.vol00+01.par2");
            let (a, b, c) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
            fixture.put_row(1, name, a, "x1@nyuu", &[(mkv, &[1, 4, 7], 8), (vol, &[1], 2)]);
            fixture.put_row(2, name, b, "x2@nyuu", &[(mkv, &[2, 5, 8], 8), (vol, &[2], 2)]);
            fixture.put_row(3, name, c, "x3@nyuu", &[(mkv, &[3, 6], 8)]);
            fixture.put_row(9, "00-thumbnail.jpg", a, "x9@nyuu", &[("00-thumbnail.jpg", &[1], 1)]);
            let (lead, thumb) = (fixture.row_of(1, a).id, fixture.row_of(9, a).id);
            fixture.beside(lead, thumb);
            let mut want: Vec<i64> =
                [(1, a), (2, b), (3, c), (9, a)].iter().map(|(i, g)| fixture.row_of(*i, g).id).collect();
            want.sort_unstable();
            // every row of the post, the thumbnail's too, gets the one answer
            for (i, g) in [(1, a), (2, b), (3, c), (9, a)] {
                let post = fixture.whole_post(&fixture.row_of(i, g)).unwrap().unwrap();
                assert_eq!(post.ids, want, "row {i}");
                assert!(post.complete, "row {i}");
                assert_eq!(post.lead, None, "row {i}: no lead, soo the cut file stays one file");
                assert_ne!(post.listed_id(), thumb, "row {i}");
                assert_eq!(post.groups, [a, b, c], "row {i}");
            }
        },
    );
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_poster_with_two_of_the_groups_still_gets_the_whole_post() {
    isolated("release_info::native_grouping_tests::a_poster_with_two_of_the_groups_still_gets_the_whole_post", || {
        let fixture = Fixture::new();
        let name = "Some.Show.S01E01.2160p.WEB-DL.DV.HDR.H.265-GRP";
        let mkv = "Some.Show.S01E01.mkv";
        let (a, b, c) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
        fixture.put_row(1, name, a, "x1@nyuu", &[(mkv, &[1, 4], 6)]);
        fixture.put_row(2, name, b, "x1@nyuu", &[(mkv, &[2, 5], 6)]);
        fixture.put_row(3, name, c, "x3@nyuu", &[(mkv, &[3, 6], 6)]);
        for (i, g) in [(1, a), (2, b), (3, c)] {
            let post = fixture.whole_post(&fixture.row_of(i, g)).unwrap().unwrap();
            assert_eq!(post.ids.len(), 3, "row {i}");
        }
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_row_that_only_a_later_row_links_still_joins() {
    isolated("release_info::native_grouping_tests::a_row_that_only_a_later_row_links_still_joins", || {
        let fixture = Fixture::new();
        let name = "Some.Film.2007.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.Atmos-GRP";
        let (mkv, vol) = ("Some.Film.mkv", "Some.Film.vol00+01.par2");
        let (a, b, c) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
        fixture.put_row(1, name, a, "x1@nyuu", &[(mkv, &[1, 2], 4)]);
        fixture.put_row(2, name, b, "x2@nyuu", &[(vol, &[1], 2)]);
        fixture.put_row(3, name, c, "x3@nyuu", &[(mkv, &[3, 4], 4), (vol, &[2], 2)]);
        for (i, g) in [(1, a), (2, b), (3, c)] {
            let post = fixture.whole_post(&fixture.row_of(i, g)).unwrap().unwrap_or_else(|| panic!("row {i}"));
            assert_eq!(post.ids.len(), 3, "row {i}");
            assert!(post.complete, "row {i}");
        }
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_crosspost_under_another_poster_doesnt_split_the_answer() {
    isolated("release_info::native_grouping_tests::a_crosspost_under_another_poster_doesnt_split_the_answer", || {
        let fixture = Fixture::new();
        let name = "Some.Film.2007.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.Atmos-GRP";
        let mkv = "Some.Film.mkv";
        let (a, b, c, d) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c", "alt.binaries.d");
        // C (lowest id) is A crossposted under another poster; A and B share a poster; D has the last part
        fixture.put_row(1, name, c, "copy@nyuu", &[(mkv, &[1], 3)]);
        fixture.put_row(2, name, a, "x1@nyuu", &[(mkv, &[1], 3)]);
        fixture.put_row(3, name, b, "x1@nyuu", &[(mkv, &[2], 3)]);
        fixture.put_row(4, name, d, "x4@nyuu", &[(mkv, &[3], 3)]);
        let answers: Vec<Vec<i64>> = [(1, c), (2, a), (3, b), (4, d)]
            .iter()
            .map(|(i, g)| fixture.whole_post(&fixture.row_of(*i, g)).unwrap().unwrap().ids)
            .collect();
        assert!(answers.windows(2).all(|w| w[0] == w[1]), "{answers:?}");
        assert_eq!(answers[0].len(), 3);
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn an_unrelated_upload_with_a_lower_id_doesnt_hide_the_post() {
    isolated("release_info::native_grouping_tests::an_unrelated_upload_with_a_lower_id_doesnt_hide_the_post", || {
        let fixture = Fixture::new();
        let name = "Some.Film.2007.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.Atmos-GRP";
        let (a, b, c) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
        // a whole upload of its own under other files, then the split one
        fixture.put_row(1, name, a, "whole@else", &[("Some.Film.mkv", &[1, 2, 3, 4], 4)]);
        fixture.put_row(2, name, b, "x2@nyuu", &[("Some.Film.REMUX.mkv", &[1, 2], 4)]);
        fixture.put_row(3, name, c, "x3@nyuu", &[("Some.Film.REMUX.mkv", &[3, 4], 4)]);
        assert_eq!(fixture.whole_post(&fixture.row_of(1, a)).unwrap(), None);
        for (i, g) in [(2, b), (3, c)] {
            let post = fixture.whole_post(&fixture.row_of(i, g)).unwrap().unwrap_or_else(|| panic!("row {i}"));
            assert_eq!(post.ids.len(), 2, "row {i}");
        }
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn rows_spread_past_a_day_get_one_answer_each() {
    isolated("release_info::native_grouping_tests::rows_spread_past_a_day_get_one_answer_each", || {
        let fixture = Fixture::new();
        let name = "Some.Film.2007.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.Atmos-GRP";
        let mkv = "Some.Film.mkv";
        let (a, b, c) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
        fixture.put_row_at(1, name, a, "x1@nyuu", &[(mkv, &[1], 3)], "2026-11-01 00:00:00");
        fixture.put_row_at(2, name, b, "x2@nyuu", &[(mkv, &[2], 3)], "2026-11-01 18:00:00");
        fixture.put_row_at(3, name, c, "x3@nyuu", &[(mkv, &[3], 3)], "2026-11-02 12:00:00");
        let answers: Vec<Option<Vec<i64>>> = [(1, a), (2, b), (3, c)]
            .iter()
            .map(|(i, g)| fixture.whole_post(&fixture.row_of(*i, g)).unwrap().map(|p| p.ids))
            .collect();
        let (one, two) = (fixture.row_of(1, a).id, fixture.row_of(2, b).id);
        let mut first = vec![one, two];
        first.sort_unstable();
        assert_eq!(answers[0].as_ref(), Some(&first));
        assert_eq!(answers[1].as_ref(), Some(&first), "the row in the middle goes with the first post");
        assert_eq!(answers[2], None, "too far from it, and alone");
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn an_older_upload_of_the_same_parts_isnt_a_crosspost() {
    isolated("release_info::native_grouping_tests::an_older_upload_of_the_same_parts_isnt_a_crosspost", || {
        let fixture = Fixture::new();
        let name = "Some.Film.2007.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.Atmos-GRP";
        let mkv = "Some.Film.mkv";
        let (a, b, c) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
        fixture.put_row_at(1, name, a, "old@else", &[(mkv, &[1], 2)], "2026-10-20 00:00:00");
        fixture.put_row_at(2, name, b, "x2@nyuu", &[(mkv, &[1], 2)], "2026-10-30 00:00:00");
        fixture.put_row_at(3, name, c, "x3@nyuu", &[(mkv, &[2], 2)], "2026-10-30 00:10:00");
        for (i, g) in [(2, b), (3, c)] {
            let post = fixture.whole_post(&fixture.row_of(i, g)).unwrap().unwrap_or_else(|| panic!("row {i}"));
            assert_eq!(post.ids.len(), 2, "row {i}");
        }
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_name_in_many_groups_still_finds_the_posters_own_rows() {
    isolated("release_info::native_grouping_tests::a_name_in_many_groups_still_finds_the_posters_own_rows", || {
        let fixture = Fixture::new();
        let name = "Some.Film.2007.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.Atmos-GRP";
        let mkv = "Some.Film.mkv";
        let shard = crate::catalog_facts::shard_of("alt.binaries.zz.a");
        let groups: Vec<String> = (0..2000)
            .map(|n| format!("alt.binaries.g{n:04}"))
            .filter(|g| crate::catalog_facts::shard_of(g) == shard)
            .take(70)
            .collect();
        // other posters' rows of the name come first in the name index
        for (n, g) in groups.iter().take(68).enumerate() {
            fixture.put_row(100 + n as i64, name, g, &format!("other{n}@else"), &[(mkv, &[1, 2], 2)]);
        }
        let (a, b) = (groups[68].as_str(), groups[69].as_str());
        fixture.put_row(1, name, a, "x1@nyuu", &[(mkv, &[1], 2)]);
        fixture.put_row(2, name, b, "x1@nyuu", &[(mkv, &[2], 2)]);
        let post = fixture.whole_post(&fixture.row_of(1, a)).unwrap().unwrap();
        assert_eq!(post.ids.len(), 2);
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn rows_of_another_poster_join_only_where_they_fill_gaps_under_the_same_subject() {
    isolated(
        "release_info::native_grouping_tests::rows_of_another_poster_join_only_where_they_fill_gaps_under_the_same_subject",
        || {
            let fixture = Fixture::new();
            let name = "Some.Film.2007.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.Atmos-GRP";
            let mkv = "Some.Film.mkv";
            let (a, b, c) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
            fixture.put_row(1, name, a, "x1@nyuu", &[(mkv, &[1, 3], 4)]);
            // a whole upload of its own, by someone else: it has parts the first has too
            fixture.put_row(2, name, b, "other@else", &[(mkv, &[1, 2, 3, 4], 4)]);
            assert_eq!(fixture.whole_post(&fixture.row_of(1, a)).unwrap(), None);
            assert_eq!(fixture.whole_post(&fixture.row_of(2, b)).unwrap(), None);
            // the missing parts, but under another subject: not shown to be the same file
            fixture.put_row(3, name, c, "x3@nyuu", &[("Some.Film.mp4", &[2, 4], 4)]);
            assert_eq!(fixture.whole_post(&fixture.row_of(1, a)).unwrap(), None);
        },
    );
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_split_post_is_whole_without_the_thumbnail_its_counter_counts() {
    isolated(
        "release_info::native_grouping_tests::a_split_post_is_whole_without_the_thumbnail_its_counter_counts",
        || {
            let fixture = Fixture::new();
            let name = "Some.Film.1997.2160p.MA.WEB-DL.DDP5.1.H.265-GRP";
            let (mkv, vol) = (
                "Some.Film.1997.2160p.MA.WEB-DL.DDP5.1.H.265-GRP.mkv",
                "Some.Film.1997.2160p.MA.WEB-DL.DDP5.1.H.265-GRP.vol00-01.par2",
            );
            let (a, b, c, d) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c", "alt.binaries.d");
            // `[3/3] - "thumbnail.jpg"` went to a row of its own: the film and its volume are all there is, across 4 groups
            let at = "2026-11-07 10:49:51";
            fixture.put_row_counted(1, name, a, "x1@nyuu", &[(mkv, &[1, 5], 8), (vol, &[1], 2)], at, 3);
            fixture.put_row_counted(2, name, b, "x2@nyuu", &[(mkv, &[2, 6], 8), (vol, &[2], 2)], at, 3);
            fixture.put_row_counted(3, name, c, "x3@nyuu", &[(mkv, &[3, 7], 8)], at, 3);
            fixture.put_row_counted(4, name, d, "x1@nyuu", &[(mkv, &[4, 8], 8)], at, 3);
            for (i, g) in [(1, a), (2, b), (3, c), (4, d)] {
                let row = fixture.row_of(i, g);
                let post = fixture.whole_post(&row).unwrap().unwrap_or_else(|| panic!("row {i}"));
                assert_eq!(post.ids.len(), 4, "row {i}");
                assert!(post.complete, "row {i}");
                assert!(!fixture.whole(row.id).unwrap(), "row {i} alone has half the film");
            }
            // a part of the film gone: not whole
            fixture.remove_parts(fixture.row_of(4, d).id, mkv);
            let post = fixture.whole_post(&fixture.row_of(1, a)).unwrap();
            let post = post.expect("the four rows are still one post");
            assert_eq!(post.ids.len(), 4);
            assert!(!post.complete, "a part of the film is gone");
        },
    );
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_group_row_named_as_the_film_joins_the_split_post() {
    isolated("release_info::native_grouping_tests::a_group_row_named_as_the_film_joins_the_split_post", || {
        let fixture = Fixture::new();
        let name = "Some.Film.1965.2160p.UHD.BluRay.x265-GRP";
        let (mkv, vol) =
            ("Some.Film.1965.2160p.UHD.BluRay.x265-GRP.mkv", "Some.Film.1965.2160p.UHD.BluRay.x265-GRP.vol00-01.par2");
        let (a, b, c, d) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c", "alt.binaries.d");
        let at = "2026-11-07 10:49:51";
        // the film's first article came first in group a, so its row took the film's name; elsewhere a PAR2 file did
        fixture.put_row_counted(1, mkv, a, "x1@nyuu", &[(mkv, &[1, 5], 8)], at, 3);
        fixture.put_row_counted(2, name, b, "x2@nyuu", &[(mkv, &[2, 6], 8), (vol, &[1], 2)], at, 3);
        fixture.put_row_counted(3, name, c, "x3@nyuu", &[(mkv, &[3, 7], 8), (vol, &[2], 2)], at, 3);
        fixture.put_row_counted(4, name, d, "x1@nyuu", &[(mkv, &[4, 8], 8)], at, 3);
        for (i, g) in [(1, a), (2, b), (3, c), (4, d)] {
            let row = fixture.row_of(i, g);
            let post = fixture.whole_post(&row).unwrap().unwrap_or_else(|| panic!("row {i}"));
            assert_eq!(post.ids.len(), 4, "row {i}");
            assert!(post.complete, "row {i}");
        }
        // another film's row named only like it: not part of the post
        fixture.put_row_counted(
            5,
            "Some.Film.1965.2160p.UHD.BluRay.x265-GRP.mkv.1",
            b,
            "x9@nyuu",
            &[(mkv, &[1], 8)],
            at,
            3,
        );
        let post = fixture.whole_post(&fixture.row_of(2, b)).unwrap().unwrap();
        assert_eq!(post.ids.len(), 4);
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_post_whose_articles_went_to_several_of_its_groups_is_one_post() {
    isolated(
        "release_info::native_grouping_tests::a_post_whose_articles_went_to_several_of_its_groups_is_one_post",
        || {
            let fixture = Fixture::new();
            let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
            let (mkv, vol) = ("Some.Film.mkv", "Some.Film.vol00+01.par2");
            let (a, b, c, d) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c", "alt.binaries.d");
            let at = "2026-11-07 05:16:17";
            // most articles went to two of the four groups: the rows share parts, none is whole, and each has one of its own
            // row, group, poster, the film's parts, the volume's parts
            type Row<'a> = (i64, &'a str, &'a str, &'a [i64], &'a [i64]);
            let rows: [Row; 4] = [
                (1, a, "x1@nyuu", &[1, 2, 5], &[1]),
                (2, b, "x2@nyuu", &[2, 3, 6], &[2]),
                (3, c, "x3@nyuu", &[3, 4, 7], &[1]),
                (4, d, "x1@nyuu", &[4, 1, 8], &[2]),
            ];
            for (i, g, poster, film, volume) in rows {
                fixture.put_row_subjects(i, name, g, poster, &[(mkv, film, 8), (vol, volume, 2)], at, 2, true);
            }
            for (i, g, ..) in rows {
                let row = fixture.row_of(i, g);
                let post = fixture.whole_post(&row).unwrap().unwrap_or_else(|| panic!("row {i}"));
                assert_eq!(post.ids.len(), 4, "row {i}");
                assert!(post.complete, "row {i}");
                assert!(!fixture.whole(row.id).unwrap(), "row {i} alone isn't whole");
            }
        },
    );
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn rows_sharing_parts_without_the_files_size_stay_apart() {
    isolated("release_info::native_grouping_tests::rows_sharing_parts_without_the_files_size_stay_apart", || {
        let fixture = Fixture::new();
        let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
        let mkv = "Some.Film.mkv";
        let (a, b) = ("alt.binaries.a", "alt.binaries.b");
        let at = "2026-11-07 05:16:17";
        // the same rows as a post whose articles went to two groups, but the subjects don't say how big the file is
        fixture.put_row_subjects(1, name, a, "x1@nyuu", &[(mkv, &[1, 2, 3], 4)], at, 1, false);
        fixture.put_row_subjects(2, name, b, "x2@nyuu", &[(mkv, &[3, 4], 4)], at, 1, false);
        assert_eq!(fixture.whole_post(&fixture.row_of(1, a)).unwrap(), None);
        assert_eq!(fixture.whole_post(&fixture.row_of(2, b)).unwrap(), None);
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_whole_upload_sharing_parts_with_a_row_stays_its_own_post() {
    isolated("release_info::native_grouping_tests::a_whole_upload_sharing_parts_with_a_row_stays_its_own_post", || {
        let fixture = Fixture::new();
        let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
        let mkv = "Some.Film.mkv";
        let (a, b, c) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
        let at = "2026-11-07 05:16:17";
        // a whole upload of the same file, and a row that has parts of it, either one first
        fixture.put_row_subjects(1, name, a, "x1@nyuu", &[(mkv, &[1, 3], 4)], at, 1, true);
        fixture.put_row_subjects(2, name, b, "whole@else", &[(mkv, &[1, 2, 3, 4], 4)], at, 1, true);
        fixture.put_row_subjects(3, name, c, "x3@nyuu", &[(mkv, &[2, 3], 4)], at, 1, true);
        assert_eq!(fixture.whole_post(&fixture.row_of(2, b)).unwrap(), None, "the whole upload is alone");
        // and the two pieces, still a part short together, make no post either
        assert_eq!(fixture.whole_post(&fixture.row_of(1, a)).unwrap(), None);
        assert_eq!(fixture.whole_post(&fixture.row_of(3, c)).unwrap(), None);
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn rows_sharing_parts_get_one_answer_whichever_asks() {
    isolated("release_info::native_grouping_tests::rows_sharing_parts_get_one_answer_whichever_asks", || {
        let fixture = Fixture::new();
        let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
        let mkv = "Some.Film.mkv";
        let (a, b, c) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
        let at = "2026-11-07 05:16:17";
        fixture.put_row_subjects(1, name, a, "x1@nyuu", &[(mkv, &[1, 2], 4)], at, 1, true);
        fixture.put_row_subjects(2, name, b, "x2@nyuu", &[(mkv, &[3, 4], 4)], at, 1, true);
        // parts the other two have between them: it joins only if it's looked at before the post is whole
        fixture.put_row_subjects(3, name, c, "x3@nyuu", &[(mkv, &[2, 3], 4)], at, 1, true);
        let rows = [fixture.row_of(1, a), fixture.row_of(2, b), fixture.row_of(3, c)];
        let answers: Vec<Option<SplitPost>> = rows.iter().map(|r| fixture.whole_post(r).unwrap()).collect();
        let post = answers.iter().flatten().next().expect("a post").clone();
        assert!(post.complete);
        assert!(post.ids.contains(&rows[0].id) && post.ids.contains(&rows[1].id));
        for (row, answer) in rows.iter().zip(&answers) {
            match answer {
                Some(p) => assert_eq!(p.ids, post.ids, "row {}", row.id),
                None => assert!(!post.ids.contains(&row.id), "row {} is in the post but got no answer", row.id),
            }
        }
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_post_sharing_parts_counts_each_parts_bytes_once() {
    isolated("release_info::native_grouping_tests::a_post_sharing_parts_counts_each_parts_bytes_once", || {
        let fixture = Fixture::new();
        let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
        let mkv = "Some.Film.mkv";
        let (a, b) = ("alt.binaries.a", "alt.binaries.b");
        let at = "2026-11-07 05:16:17";
        // each row holds 300 bytes of it (`put_row_subjects`: 100 a part); the subject says the file is 4 * 716,800
        fixture.put_row_subjects(1, name, a, "x1@nyuu", &[(mkv, &[1, 2, 3], 4)], at, 1, true);
        fixture.put_row_subjects(2, name, b, "x2@nyuu", &[(mkv, &[2, 3, 4], 4)], at, 1, true);
        let post = fixture.whole_post(&fixture.row_of(1, a)).unwrap().unwrap();
        assert_eq!(post.ids.len(), 2);
        assert!(post.complete);
        assert_eq!(post.size, 4 * 716_800, "the file once, not 600 bytes of rows");
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_row_whole_on_its_own_stays_out_whichever_row_comes_first() {
    isolated("release_info::native_grouping_tests::a_row_whole_on_its_own_stays_out_whichever_row_comes_first", || {
        let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
        let (mkv, vol) =
            ("Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP.mkv", "Some.Film.vol00+01.par2");
        let (a, b, c) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
        let at = "2026-11-07 05:16:17";
        // the film alone and whole in one group (whole by `one_video_named`), its pieces and the volume elsewhere
        for whole_first in [false, true] {
            let fixture = Fixture::new();
            let (w, x, z) = if whole_first { (1, 2, 3) } else { (3, 1, 2) };
            fixture.put_row_subjects(x, name, a, "x@nyuu", &[(mkv, &[1, 2], 4), (vol, &[1], 2)], at, 2, true);
            fixture.put_row_subjects(w, name, b, "w@else", &[(mkv, &[1, 2, 3, 4], 4)], at, 2, true);
            fixture.put_row_subjects(z, name, c, "z@nyuu", &[(mkv, &[3], 4), (vol, &[2], 2)], at, 2, true);
            assert_eq!(fixture.whole_post(&fixture.row_of(w, b)).unwrap(), None, "whole first: {whole_first}");
            let post = fixture.whole_post(&fixture.row_of(x, a)).unwrap().unwrap();
            assert_eq!(post.ids.len(), 2, "whole first: {whole_first}");
            assert!(!post.complete, "part 4 is only in the whole one, whole first: {whole_first}");
        }
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_split_post_keeps_its_answer_beside_a_partial_repost() {
    isolated("release_info::native_grouping_tests::a_split_post_keeps_its_answer_beside_a_partial_repost", || {
        let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
        let mkv = "Some.Film.mkv";
        let at = "2026-11-07 05:16:17";
        let g = groups_in_one_shard(3);
        // P splits the film across a and b; R posted it again to c, a part short. Every order of ids
        for (pa, pb, rc) in [(1, 2, 3), (2, 3, 1), (1, 3, 2)] {
            let fixture = Fixture::new();
            fixture.put_row_upload(pa, name, &g[0], "p@nyuu", &[(mkv, &[1, 2], 4)], at, 1, Some("p"));
            fixture.put_row_upload(pb, name, &g[1], "p@nyuu", &[(mkv, &[3, 4], 4)], at, 1, Some("p"));
            fixture.put_row_upload(rc, name, &g[2], "r@else", &[(mkv, &[1, 2, 3], 4)], at, 1, Some("r"));
            let (a, b, c) = (fixture.row_of(pa, &g[0]), fixture.row_of(pb, &g[1]), fixture.row_of(rc, &g[2]));
            let mut both = vec![a.id, b.id];
            both.sort_unstable();
            for row in [&a, &b] {
                let post = fixture.whole_post(row).unwrap().unwrap_or_else(|| panic!("order {pa}{pb}{rc}"));
                assert_eq!(post.ids, both, "order {pa}{pb}{rc}");
                assert!(post.complete, "order {pa}{pb}{rc}");
            }
            assert_eq!(fixture.whole_post(&c).unwrap(), None, "order {pa}{pb}{rc}");
        }
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_split_post_stays_whole_beside_another_posters_part_with_a_lower_id() {
    isolated(
        "release_info::native_grouping_tests::a_split_post_stays_whole_beside_another_posters_part_with_a_lower_id",
        || {
            let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
            let mkv = "Some.Film.mkv";
            let at = "2026-11-07 05:16:17";
            let g = groups_in_one_shard(3);
            // subjects without a size: P's two rows are the whole post; Q's row has one of its parts, whatever the ids
            for (x, a, c) in [(1, 2, 3), (2, 1, 3), (3, 1, 2)] {
                let fixture = Fixture::new();
                fixture.put_row_subjects(x, name, &g[0], "q@nyuu", &[(mkv, &[3], 4)], at, 1, false);
                fixture.put_row_subjects(a, name, &g[1], "p@nyuu", &[(mkv, &[1, 2], 4)], at, 1, false);
                fixture.put_row_subjects(c, name, &g[2], "p@nyuu", &[(mkv, &[3, 4], 4)], at, 1, false);
                let (ra, rc) = (fixture.row_of(a, &g[1]), fixture.row_of(c, &g[2]));
                let mut both = vec![ra.id, rc.id];
                both.sort_unstable();
                for row in [&ra, &rc] {
                    let post = fixture.whole_post(row).unwrap().unwrap_or_else(|| panic!("order {x}{a}{c}"));
                    assert_eq!(post.ids, both, "order {x}{a}{c}");
                    assert!(post.complete, "order {x}{a}{c}");
                }
            }
        },
    );
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_posters_whole_split_stays_whole_beside_another_posters_row_whole_with_one_of_them() {
    isolated(
        "release_info::native_grouping_tests::a_posters_whole_split_stays_whole_beside_another_posters_row_whole_with_one_of_them",
        || {
            let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
            let mkv = "Some.Film.mkv";
            let at = "2026-11-07 05:16:17";
            let g = groups_in_one_shard(4);
            let fixture = Fixture::new();
            // q's row has parts 3 and 4: whole with p's first row, but p's three rows are whole on their own
            fixture.put_row_subjects(1, name, &g[0], "q@nyuu", &[(mkv, &[3, 4], 4)], at, 1, false);
            fixture.put_row_subjects(2, name, &g[1], "p@nyuu", &[(mkv, &[1, 2], 4)], at, 1, false);
            fixture.put_row_subjects(3, name, &g[2], "p@nyuu", &[(mkv, &[3], 4)], at, 1, false);
            fixture.put_row_subjects(4, name, &g[3], "p@nyuu", &[(mkv, &[4], 4)], at, 1, false);
            let p_rows = [fixture.row_of(2, &g[1]), fixture.row_of(3, &g[2]), fixture.row_of(4, &g[3])];
            let mut ids: Vec<i64> = p_rows.iter().map(|r| r.id).collect();
            ids.sort_unstable();
            for row in &p_rows {
                let post = fixture.whole_post(row).unwrap().unwrap();
                assert_eq!(post.ids, ids);
                assert!(post.complete);
            }
        },
    );
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_split_post_past_the_rows_looked_at_keeps_its_answer_beside_rows_sharing_parts() {
    isolated(
        "release_info::native_grouping_tests::a_split_post_past_the_rows_looked_at_keeps_its_answer_beside_rows_sharing_parts",
        || {
            let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
            let mkv = "Some.Film.mkv";
            let at = "2026-11-07 05:16:17";
            let g = groups_in_one_shard(36);
            let fixture = Fixture::new();
            // two rows sharing parts, then whole uploads by someone else, then p's split past the rows looked at
            fixture.put_row_subjects(1, name, &g[0], "q@nyuu", &[(mkv, &[1, 2, 3], 4)], at, 1, true);
            fixture.put_row_subjects(2, name, &g[1], "r@nyuu", &[(mkv, &[2, 3, 4], 4)], at, 1, true);
            for n in 3..=34 {
                fixture.put_row_subjects(
                    n,
                    name,
                    &g[n as usize - 1],
                    "f@else",
                    &[(mkv, &[1, 2, 3, 4], 4)],
                    at,
                    1,
                    false,
                );
            }
            fixture.put_row_subjects(35, name, &g[34], "p@nyuu", &[(mkv, &[1, 2], 4)], at, 1, false);
            fixture.put_row_subjects(36, name, &g[35], "p@nyuu", &[(mkv, &[3, 4], 4)], at, 1, false);
            let (a, b) = (fixture.row_of(35, &g[34]), fixture.row_of(36, &g[35]));
            let post = fixture.whole_post(&a).unwrap().expect("p's split");
            assert_eq!(post.ids, vec![a.id, b.id]);
            assert!(post.complete);
        },
    );
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_posters_split_keeps_its_post_beside_another_posters_partial_split() {
    isolated(
        "release_info::native_grouping_tests::a_posters_split_keeps_its_post_beside_another_posters_partial_split",
        || {
            let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
            let mkv = "Some.Film.mkv";
            let at = "2026-11-07 05:16:17";
            let g = groups_in_one_shard(4);
            let fixture = Fixture::new();
            // p's whole split over two groups, q's partial one over two others: as before rows could share parts
            fixture.put_row_subjects(1, name, &g[0], "p@nyuu", &[(mkv, &[1, 2], 4)], at, 1, false);
            fixture.put_row_subjects(2, name, &g[1], "p@nyuu", &[(mkv, &[3, 4], 4)], at, 1, false);
            fixture.put_row_subjects(3, name, &g[2], "q@nyuu", &[(mkv, &[1], 4)], at, 1, false);
            fixture.put_row_subjects(4, name, &g[3], "q@nyuu", &[(mkv, &[2], 4)], at, 1, false);
            let rows: Vec<_> = (1..=4).map(|i| fixture.row_of(i, &g[i as usize - 1])).collect();
            for row in &rows[..2] {
                let post = fixture.whole_post(row).unwrap().unwrap();
                assert_eq!(post.ids, vec![rows[0].id, rows[1].id]);
                assert!(post.complete);
            }
            for row in &rows[2..] {
                let post = fixture.whole_post(row).unwrap().unwrap();
                assert_eq!(post.ids, vec![rows[2].id, rows[3].id]);
                assert!(!post.complete);
            }
        },
    );
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_crosspost_of_a_row_in_a_post_sharing_parts_isnt_in_its_posters_own_post() {
    isolated(
        "release_info::native_grouping_tests::a_crosspost_of_a_row_in_a_post_sharing_parts_isnt_in_its_posters_own_post",
        || {
            let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
            let (mkv, vol) = ("Some.Film.mkv", "Some.Film.vol00+01.par2");
            let at = "2026-11-07 05:16:17";
            let g = groups_in_one_shard(5);
            let fixture = Fixture::new();
            fixture.put_row_subjects(1, name, &g[0], "p0@nyuu", &[(mkv, &[3, 4], 4)], at, 2, true);
            fixture.put_row_subjects(2, name, &g[1], "p3@nyuu", &[(mkv, &[1, 3], 4), (vol, &[1, 2], 2)], at, 2, true);
            fixture.put_row_subjects(3, name, &g[2], "p1@nyuu", &[(mkv, &[1], 4), (vol, &[1], 2)], at, 2, true);
            // a crosspost of row 1 by row 3's poster
            fixture.put_row_subjects(4, name, &g[3], "p1@nyuu", &[(mkv, &[3, 4], 4)], at, 2, true);
            fixture.put_row_subjects(5, name, &g[4], "p0@nyuu", &[(mkv, &[1, 2, 4], 4), (vol, &[2], 2)], at, 2, true);
            let rows: Vec<_> = (1..=5).map(|i| fixture.row_of(i, &g[i as usize - 1])).collect();
            let answers: Vec<Option<SplitPost>> = rows.iter().map(|r| fixture.whole_post(r).unwrap()).collect();
            // row 4 is answered with the post of the row it copies, and is in no other
            let of_copy = answers[3].clone().expect("the copy's post");
            for (row, answer) in rows.iter().zip(&answers) {
                if let Some(p) = answer
                    && p.ids != of_copy.ids
                {
                    assert!(!p.ids.contains(&rows[3].id), "row {} has the copy in another post", row.id);
                }
            }
        },
    );
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn rows_sharing_parts_make_no_post_unless_its_whole() {
    isolated("release_info::native_grouping_tests::rows_sharing_parts_make_no_post_unless_its_whole", || {
        let fixture = Fixture::new();
        let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
        let (mkv, vol) = ("Some.Film.mkv", "Some.Film.vol00+01.par2");
        let (a, b) = ("alt.binaries.a", "alt.binaries.b");
        let at = "2026-11-07 05:16:17";
        // together still a part short: each row keeps its answer of its own (a repair verdict, for one)
        fixture.put_row_subjects(
            1,
            name,
            a,
            "x1@nyuu",
            &[(mkv, &[1, 2, 3, 4, 5, 6], 8), (vol, &[1, 2], 2)],
            at,
            2,
            true,
        );
        fixture.put_row_subjects(2, name, b, "x2@nyuu", &[(mkv, &[6, 7], 8)], at, 2, true);
        assert_eq!(fixture.whole_post(&fixture.row_of(1, a)).unwrap(), None);
        assert_eq!(fixture.whole_post(&fixture.row_of(2, b)).unwrap(), None);
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle on native Runtime"]
fn a_film_whose_counter_says_more_files_is_whole_only_when_named_as_its_post() {
    isolated(
        "release_info::native_grouping_tests::a_film_whose_counter_says_more_files_is_whole_only_when_named_as_its_post",
        || {
            let fixture = Fixture::new();
            let (a, b) = ("alt.binaries.a", "alt.binaries.b");
            let at = "2026-11-07 10:49:51";
            // the whole film alone in one row, its counter counting a thumbnail: named as the row, or not
            fixture.put_row_counted(
                1,
                "Film.2020.2160p-G",
                a,
                "x@nyuu",
                &[("Film.2020.2160p-G.mkv", &[1, 2], 2)],
                at,
                3,
            );
            fixture.put_row_counted(2, "Film.2020.2160p-G", b, "y@nyuu", &[("abc.mkv", &[1, 2], 2)], at, 3);
            assert!(fixture.whole(fixture.row_of(1, a).id).unwrap());
            assert!(!fixture.whole(fixture.row_of(2, b).id).unwrap());
        },
    );
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle"]
fn two_uploads_of_a_file_alike_in_every_header_are_whole_together() {
    isolated(
        "release_info::native_grouping_tests::two_uploads_of_a_file_alike_in_every_header_are_whole_together",
        || {
            let fixture = Fixture::new();
            let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
            let mkv = "Some.Film.mkv";
            let (a, b) = ("alt.binaries.a", "alt.binaries.b");
            let at = "2026-11-07 05:16:17";
            // the same name, part count and size, each a part short: a part of one stands in for the same part of the other
            fixture.put_row_upload(1, name, a, "p@nyuu", &[(mkv, &[1, 2, 3], 4)], at, 1, Some("p"));
            fixture.put_row_upload(2, name, b, "q@nyuu", &[(mkv, &[1, 2, 4], 4)], at, 1, Some("q"));
            for (i, g) in [(1, a), (2, b)] {
                let post = fixture.whole_post(&fixture.row_of(i, g)).unwrap().unwrap_or_else(|| panic!("row {i}"));
                assert_eq!(post.ids.len(), 2, "row {i}");
                assert!(post.complete, "row {i}");
            }
        },
    );
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original grouping oracle"]
fn one_posters_rows_sharing_parts_are_one_post() {
    isolated("release_info::native_grouping_tests::one_posters_rows_sharing_parts_are_one_post", || {
        let fixture = Fixture::new();
        let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
        let mkv = "Some.Film.mkv";
        let (a, b) = ("alt.binaries.a", "alt.binaries.b");
        let at = "2026-11-07 05:16:17";
        fixture.put_row_subjects(1, name, a, "p@nyuu", &[(mkv, &[1, 2, 3], 4)], at, 1, true);
        fixture.put_row_subjects(2, name, b, "p@nyuu", &[(mkv, &[2, 3, 4], 4)], at, 1, true);
        for (i, g) in [(1, a), (2, b)] {
            let post = fixture.whole_post(&fixture.row_of(i, g)).unwrap().unwrap_or_else(|| panic!("row {i}"));
            assert_eq!(post.ids.len(), 2, "row {i}");
            assert!(post.complete, "row {i}");
        }
    });
}

impl Fixture {
    fn data_link(&self, id: i64, lead: i64, file: u8) {
        self.mutate(
            id,
            &[lead],
            vec![StepMutation::LinkUploadData { member_id: id, lead_id: lead, par2_file: [file; 16] }],
        );
    }
    fn recovery_link(&self, id: i64, lead: i64, wanted: i64) {
        self.mutate(id, &[lead], vec![StepMutation::LinkUploadRecovery { lead_id: lead, wanted, set_id: None }]);
    }
    fn linked() -> (Self, [i64; 3]) {
        let f = Self::new();
        let group = "alt.binaries.linked";
        for (label, parts, bytes) in [(8, 3, 1_032_000), (16, 1, 900), (24, 1, 200_900)] {
            let name = format!("random{label}");
            let held = (1..=parts).collect::<Vec<_>>();
            f.put_row_counted(label, &name, group, "p", &[(&name, &held, parts)], "2026-08-22 00:00:00", 1);
            for article in &mut f.pending.borrow_mut().get_mut(&(label, group.into())).unwrap().articles {
                article.bytes = bytes;
            }
        }
        let ids = [f.row_of(8, group).id, f.row_of(16, group).id, f.row_of(24, group).id];
        (f, ids)
    }
}

#[test]
#[ignore = "explicit isolated PostgreSQL; typed canonical links"]
fn a_linked_upload_is_all_its_rows() {
    isolated("release_info::native_grouping_tests::a_linked_upload_is_all_its_rows", || {
        let (f, [lead, index, volume]) = Fixture::linked();
        assert_eq!(f.linked_post(index).unwrap(), None, "nothing linked yet");
        f.data_link(lead, lead, 1);
        f.recovery_link(index, lead, 1);
        f.recovery_link(volume, lead, 1);
        for id in [lead, index, volume] {
            let post = f.linked_post(id).unwrap().unwrap();
            assert_eq!(post.ids, [lead, index, volume]);
            assert_eq!(post.size, 3_096_000 + 900 + 200_900);
            assert_eq!((post.listed_id(), post.lead), (lead, Some(lead)));
            assert!(post.complete);
            assert_eq!(post.recovery, [index, volume]);
            let row = crate::search::ReleaseRow { id, complete: true, ..Default::default() };
            assert_eq!(f.whole_post(&row).unwrap().unwrap().lead, Some(lead));
        }
        f.recovery_link(volume, lead, 2);
        assert!(!f.linked_post(index).unwrap().unwrap().complete, "one of two wanted files found");
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; typed canonical links"]
fn a_file_posted_twice_counts_once() {
    isolated("release_info::native_grouping_tests::a_file_posted_twice_counts_once", || {
        let (f, [lead, other, recovery]) = Fixture::linked();
        f.data_link(lead, lead, 1);
        f.data_link(other, lead, 1);
        f.recovery_link(recovery, lead, 2);
        assert!(!f.linked_post(lead).unwrap().unwrap().complete, "two copies of one file of two");
        f.data_link(other, lead, 2);
        assert!(f.linked_post(lead).unwrap().unwrap().complete);
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; typed canonical links"]
fn same_named_files_of_a_linked_upload_do_not_fill_each_others_gaps() {
    isolated(
        "release_info::native_grouping_tests::same_named_files_of_a_linked_upload_do_not_fill_each_others_gaps",
        || {
            let f = Fixture::new();
            let g = "alt.binaries.linked";
            f.put_row_counted(1, "Lead.Post", g, "p", &[("same.bin", &[1, 2], 3)], "2026-08-22 00:00:00", 1);
            f.put_row_counted(2, "Recovery.Post", g, "p", &[("same.bin", &[2, 3], 3)], "2026-08-22 00:00:00", 1);
            let (lead, recovery) = (f.row_of(1, g).id, f.row_of(2, g).id);
            f.data_link(lead, lead, 1);
            f.recovery_link(recovery, lead, 1);
            assert_eq!(f.permille(&[lead, recovery]).unwrap(), 1000, "together the parts cover the file");
            assert!(!f.linked_post(lead).unwrap().unwrap().complete, "each linked row is a file of its own");
        },
    );
}

#[test]
#[ignore = "explicit isolated PostgreSQL; genuine derivative removal"]
fn a_link_whose_file_is_gone_is_no_link() {
    isolated("release_info::native_grouping_tests::a_link_whose_file_is_gone_is_no_link", || {
        let (f, [lead, index, volume]) = Fixture::linked();
        f.data_link(lead, lead, 1);
        f.recovery_link(index, lead, 1);
        f.recovery_link(volume, lead, 1);
        f.runtime
            .owner
            .with(|c| {
                c.execute("delete from releases where id=$1", &[&lead])?;
                Ok(())
            })
            .unwrap();
        assert_eq!(f.linked_post(index).unwrap(), None);
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; canonical beside mutation"]
fn a_thumbnail_found_beside_a_release_is_a_file_of_its_post() {
    isolated("release_info::native_grouping_tests::a_thumbnail_found_beside_a_release_is_a_file_of_its_post", || {
        let f = Fixture::new();
        let g = "alt.binaries.a";
        f.put_row(
            1,
            "Some.Film.2160p",
            g,
            "a@x",
            &[("Some.Film.2160p.mp4", &[1, 2, 3], 3), ("Some.Film.2160p.par2", &[1], 1)],
        );
        f.put_row(2, "00-thumbnail.jpg", g, "b@x", &[("00-thumbnail.jpg", &[1], 1)]);
        let (lead, thumb) = (f.row_of(1, g).id, f.row_of(2, g).id);
        f.beside(lead, thumb);
        for id in [lead, thumb] {
            let post = f.whole_post(&crate::search::ReleaseRow { id, ..Default::default() }).unwrap().unwrap();
            assert_eq!(post.ids, [lead, thumb]);
            assert_eq!((post.lead, post.listed_id()), (None, lead));
            assert!(post.recovery.is_empty());
            assert!(post.complete);
        }
        f.runtime
            .owner
            .with(|c| {
                let view = crate::pg::fresh_reader::ReadView::open(
                    c,
                    &f.schema.name,
                    Default::default(),
                    Instant::now() + Duration::from_secs(10),
                )?;
                assert!(view.own_contents(thumb)?.list.iter().any(|file| file.name == "00-thumbnail.jpg"));
                Ok(())
            })
            .unwrap();
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; original stem provenance oracle"]
fn per_file_cross_group_split_post_merges_by_stem() {
    isolated("release_info::native_grouping_tests::per_file_cross_group_split_post_merges_by_stem", || {
        let f = Fixture::new();
        let stem = "One.From.The.Heart.1981.2160p.UHD.BluRay.DV.REMUX.HDR10.HEVC.DTS-HD-MA.5.1-UnKn0wn";
        let (a, b) = ("alt.binaries.multimedia.aviation", "alt.binaries.multimedia.alias");
        let descriptions = [
            (8, stem.to_string(), a, "p1@nyuu", format!("{stem}.par2"), 1, 1, 1000),
            (16, format!("{stem}.vol001-003"), b, "p2@nyuu", format!("{stem}.vol001-003.par2"), 1, 1, 2000),
            (24, format!("{stem}.vol003-007"), b, "p3@nyuu", format!("{stem}.vol003-007.par2"), 1, 2, 3000),
            (32, format!("{stem}.vol003-007"), a, "p3@nyuu", format!("{stem}.vol003-007.par2"), 2, 2, 3000),
            (40, format!("{stem}.mkv"), b, "p4@nyuu", format!("{stem}.mkv"), 1, 1, 50_000),
        ];
        for (label, name, g, poster, file, part, expected, bytes) in &descriptions {
            f.put_row_counted(*label, name, g, poster, &[(file, &[*part], *expected)], "2025-11-28 02:30:00", 4);
            f.pending.borrow_mut().get_mut(&(*label, (*g).into())).unwrap().articles[0].bytes = *bytes;
        }
        let lead = f.row_of(40, b).id;
        assert_eq!(f.whole_post(&f.row_of(16, b)).unwrap(), None, "no unproven cross-poster stem merge");
        f.data_link(lead, lead, 1);
        for (label, _, g, ..) in &descriptions[..4] {
            f.recovery_link(f.row_of(*label, g).id, lead, 1);
        }
        // A genuinely accepted third party shares the stem but has no canonical link.
        f.put_row_counted(
            48,
            &format!("{stem}.jpg"),
            b,
            "other@else",
            &[(&format!("{stem}.jpg"), &[1], 1)],
            "2025-11-28 02:30:00",
            4,
        );
        let outsider = f.row_of(48, b);
        let mut want = descriptions.iter().map(|(label, _, g, ..)| f.row_of(*label, g).id).collect::<Vec<_>>();
        want.sort_unstable();
        for (label, _, g, ..) in &descriptions {
            let post = f.whole_post(&f.row_of(*label, g)).unwrap().unwrap();
            assert_eq!(post.ids, want);
            assert_eq!(post.groups, [b, a]);
            assert_eq!(post.size, 59_000);
            assert!(post.complete);
        }
        assert_eq!(f.whole_post(&outsider).unwrap(), None);
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; native per-mille and whole-post counts"]
fn per_mille_counts_parts_but_whole_requires_every_file() {
    isolated("release_info::native_grouping_tests::per_mille_counts_parts_but_whole_requires_every_file", || {
        let f = Fixture::new();
        let g = "alt.binaries.permille";
        f.put_row_counted(1, "Unrelated.Post", g, "p", &[("a.bin", &[1, 2, 3], 3)], "2026-11-03 07:36:18", 2);
        let id = f.row_of(1, g).id;
        assert_eq!(f.permille(&[id]).unwrap(), 1000);
        assert!(!f.whole(id).unwrap(), "one of two files never arrived");
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; native bounded stem candidates"]
fn stem_overflow_returns_none_without_returning_a_partial_post() {
    isolated(
        "release_info::native_grouping_tests::stem_overflow_returns_none_without_returning_a_partial_post",
        || {
            let f = Fixture::new();
            let stem = "Overflow.Stem.2026.1080p-GRP";
            for i in 0..=(LINKED_ROWS + 1) {
                let group = format!("alt.binaries.g{i}");
                let name = format!("{stem}.part{i:04}");
                let file = format!("{name}.rar");
                f.put_row_counted(
                    i as i64 + 1,
                    &name,
                    &group,
                    "same@poster",
                    &[(&file, &[1], 2)],
                    "2025-11-28 02:30:00",
                    2000,
                );
            }
            let anchor = f.row_of(1, "alt.binaries.g0");
            f.data_link(anchor.id, anchor.id, 1);
            f.runtime
                .owner
                .with(|c| {
                    assert_eq!(
                        c.query_one("select count(*) from releases", &[])?.get::<_, i64>(0),
                        LINKED_ROWS as i64 + 2
                    );
                    Ok(())
                })
                .unwrap();
            assert_eq!(f.whole_post(&anchor).unwrap(), None, "an over-limit stem cannot supply a partial answer");
        },
    );
}

impl Fixture {
    fn own_hint(&self, id: i64, override_names: Option<&Names>) -> Result<Option<PasswordHint>> {
        self.runtime.owner.with(|c| {
            let view = crate::pg::fresh_reader::ReadView::open(
                c,
                &self.schema.name,
                Default::default(),
                Instant::now() + Duration::from_secs(10),
            )?;
            let Some(names) = view.own_names(id)? else { return Ok(None) };
            let inner = view.own_inner(id)?;
            let nfo = view.own_nfo_text(id)?;
            view.own_password_hint(id, override_names.unwrap_or(&names), &inner, nfo.as_deref())
        })
    }
}

#[test]
#[ignore = "explicit isolated PostgreSQL; own inner-file password facts"]
fn a_password_in_a_name_inside_is_a_hint() {
    isolated("release_info::native_grouping_tests::a_password_in_a_name_inside_is_a_hint", || {
        let f = Fixture::new();
        let g = "alt.binaries.password";
        f.put_row(1, "Some.Post-GRP", g, "p", &[("Some.Post.rar", &[1], 26)]);
        let id = f.row_of(1, g).id;
        f.mutate(
            id,
            &[],
            vec![StepMutation::KeepArchiveFile {
                sequence: 131072,
                source: 2,
                file: crate::archive::InnerFile {
                    name: "Some.Post-{{s3cret}}.nzb".into(),
                    size: Some(1),
                    crc32: None,
                    mtime: None,
                    host_os: None,
                },
            }],
        );
        let hint = f.own_hint(id, None).unwrap().unwrap();
        assert_eq!((hint.password.as_str(), hint.source), ("s3cret", "a file inside"));
        assert!(f.own_hint(i64::MAX, None).unwrap().is_none());
    });
}

#[test]
#[ignore = "explicit isolated PostgreSQL; stored own password source labels"]
fn a_kept_password_says_where_it_is() {
    isolated("release_info::native_grouping_tests::a_kept_password_says_where_it_is", || {
        let f = Fixture::new();
        let g = "alt.binaries.password";
        for (label, name, file) in [
            (8, "YMFFZeoeVI6fG4iz", "YMFFZeoeVI6fG4iz.part001.rar"),
            (16, "Some.Post-GRP", "k3m9x.part01.rar"),
            (24, "Other.Post-GRP", "Other.Post.part01.rar"),
            (32, "Third.Post-GRP", "Third.Post.part01.rar"),
            (40, "Fourth.Post-GRP", "Fourth.Post.part01.rar"),
            (48, "q8w7e6r5", "a1b2c3.rar"),
        ] {
            f.put_row(label, name, g, "p", &[(file, &[1], 50)]);
        }
        f.pending.borrow_mut().get_mut(&(8, g.into())).unwrap().display_name =
            Some("Gone.with.the.Wind.1939-GRP".into());
        f.pending.borrow_mut().get_mut(&(24, g.into())).unwrap().articles[0].subject =
            "Other.Post [pw x7Q2p] yEnc (1/50)".into();
        let ids = [8, 16, 24, 32, 40, 48].map(|label| f.row_of(label, g).id);
        for (id, password) in
            ids.into_iter().zip(["YMFFZeoeVI6fG4iz", "k3m9x", "x7Q2p", "n0tH3re", "fr0mNfo", "Zq9vK2"])
        {
            f.mutate(id, &[], vec![StepMutation::SetNfoPassword { password: password.into() }]);
        }
        f.mutate(
            ids[4],
            &[],
            vec![StepMutation::KeepNfoForLegacyTarget {
                target_id: ids[4],
                file: "Fourth.nfo".into(),
                text: "Password: fr0mNfo".into(),
            }],
        );
        f.mutate(
            ids[5],
            &[],
            vec![StepMutation::KeepPar2File {
                file: crate::par2::Par2File {
                    file_id: [1; 16],
                    set_id: [2; 16],
                    md5: [3; 16],
                    md5_16k: [4; 16],
                    size: 10,
                    name: "Renamed.Post-Zq9vK2.rar".into(),
                    bound: false,
                    crc32: None,
                },
                sealed: false,
                complete: false,
            }],
        );
        let source = |id| f.own_hint(id, None).unwrap().unwrap().source;
        assert_eq!(source(ids[0]), "the original article name", "not the NFO: there is none");
        assert_eq!(source(ids[1]), "a file name");
        assert_eq!(source(ids[2]), "the article subject");
        assert_eq!(source(ids[3]), "another copy of this post, or a posted NZB");
        assert_eq!(source(ids[4]), "the NFO");
        assert_eq!(source(ids[5]), "a file name in the PAR2 set");
        let names = Names { posted: "YMFFZeoeVI6fG4iz".into(), display: None, obfuscated: true, source: None };
        assert_eq!(f.own_hint(ids[0], Some(&names)).unwrap().unwrap().source, "the release name");
    });
}
