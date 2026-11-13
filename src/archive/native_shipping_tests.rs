//! Native ports of retired shard fixtures. No SQLite compatibility path is used.
//!
//! Retirement map:
//! - beside::an_nfo_posted_on_its_own_to_another_group_finds_its_release_and_back
//!   -> native_nfo_associations_keep_cross_group_time_and_ownership_rules.
//! - beside::a_thumbnail_counted_by_the_release_joins_it
//!   -> native_thumbnail_associations_keep_poster_counter_and_taken_slot_rules.
//! - link::a_small_file_beside_a_release_is_a_row_of_one_file_near_it_that_no_other_upload_took
//!   -> native_named_beside_candidates_keep_order_single_file_ownership_and_tried_rules.
//! - link::a_set_read_once_is_kept_and_read_back
//!   -> native_par2_results_round_trip_complete_bound_sealed_creator_and_partial_state.
//! - link::a_bigger_file_found_later_takes_the_whole_upload -> retained stronger
//!   pg::stage_queue::target_tests::native_upload_results_follow_the_current_lead_after_an_earlier_effect_in_same_commit:
//!   actual observed-participant settlement moves the old lead and resolves a subsequent stale lead in one receipt.
//! - archive::select_tests::par2_name_map_matches_actual_shard_schema -> pure
//!   par2_name_map_keeps_verified_names_and_bounded_wire_size_matching, plus the actual native
//!   stage_reads::extra::tests::native_extra_reads_use_real_owner_raw_filenames_and_selected_related_rows
//!   and this module's real PAR2 name/inventory reads.
//! - archive::par2_reruns_go_to_incomplete_movies_and_tv_with_par2_files_and_no_set is retired:
//!   it tested only the once-per-shard pp_par2_from_volumes SQLite upgrade repair. Fresh native catalogs
//!   retain Par2Step's real volume fallback and inventory paths; explicit queue redo/receipt coverage stays
//!   in pg::stage_queue::walk_tests::native_walk_future_queue_is_skipped_but_explicit_redo_keeps_its_owned_visit.
//! Pure wanted/size/article/clock/rename/name/counter assertions remain in their original modules.

use super::*;
use crate::pg::{indexer_runtime::Runtime, stage_reads::StepReads};

fn add(runtime: &Runtime, name: &str, group: &str, poster: &str, posted: &str, files: &[&str]) -> i64 {
    runtime
        .owner
        .with(|client| {
            let articles = files
                .iter()
                .enumerate()
                .map(|(n, file)| {
                    let (filename, subject) = file.split_once('|').unwrap_or((file, ""));
                    crate::parser::Article {
                        filename: Some(filename.into()),
                        subject: subject.into(),
                        message_id: format!("<{group}.{name}.{n}@archive-fixture.test>"),
                        part: Some(1),
                        total_parts: Some(1),
                        bytes: 50,
                        ..Default::default()
                    }
                })
                .collect();
            crate::pg::release_writer::save(
                client,
                &[crate::parser::Release {
                    name: name.into(),
                    group: group.into(),
                    poster: poster.into(),
                    date: posted.into(),
                    size: 50 * files.len() as i64,
                    articles,
                    ..Default::default()
                }],
                -100_000,
            )?;
            Ok(client
                .query_one(
                    "select id from releases where original_name=$1 and group_name=$2",
                    &[&name.as_bytes(), &group],
                )?
                .get(0))
        })
        .unwrap()
}

#[test]
fn native_nfo_associations_keep_cross_group_time_and_ownership_rules() {
    let Some(schema) = crate::pg::testing::Schema::new() else { return };
    let runtime = crate::pg::stage_queue_tests::runtime(&schema);
    let film = "Some.Film.2160p-GRP";
    let own = add(
        &runtime,
        film,
        "example.video",
        "uploadA@example.test",
        "2026-11-08 20:58:00",
        &["Some.Film.2160p-GRP.mkv", "Some.Film.2160p-GRP.par2"],
    );
    let nfo =
        add(&runtime, film, "example.nfo", "uploadB@example.test", "2026-11-08 18:37:29", &["Some.Film.2160p-GRP.nfo"]);
    let late =
        add(&runtime, film, "example.late", "other@example.test", "2026-11-15 18:37:29", &["Some.Film.2160p-GRP.nfo"]);
    add(
        &runtime,
        film,
        "example.sidefiles",
        "other@example.test",
        "2026-11-08 18:40:00",
        &["Some.Film.2160p-GRP.nfo", "x.sfv"],
    );
    let reads = StepReads::for_owner(&runtime.owner).unwrap();
    assert_eq!(reads.find_nfo_owner(nfo, film, Some("2026-11-08 18:37:29")).unwrap(), Some(own));
    let (row, file) = reads.find_beside_nfo(own, film, Some("2026-11-08 20:58:00")).unwrap().unwrap();
    assert_eq!((row, file.filename.as_str()), (nfo, "Some.Film.2160p-GRP.nfo"));
    runtime
        .owner
        .with(|client| {
            client.execute("insert into beside_files(row_id,release_id) values($1,$2)", &[&nfo, &own])?;
            Ok(())
        })
        .unwrap();
    assert!(reads.find_beside_nfo(own, film, Some("2026-11-08 20:58:00")).unwrap().is_none());
    assert!(reads.find_nfo_owner(late, film, Some("2026-11-15 18:37:29")).unwrap().is_none());
    runtime.owner.close();
    assert!(reads.find_beside_nfo(own, film, Some("2026-11-08 20:58:00")).is_err());
}

#[test]
fn native_thumbnail_associations_keep_poster_counter_and_taken_slot_rules() {
    let Some(schema) = crate::pg::testing::Schema::new() else { return };
    let runtime = crate::pg::stage_queue_tests::runtime(&schema);
    let posted = "2026-11-08 20:58:00";
    let film = "Some.Film.2160p-GRP";
    let poster = "ZbivKJ385zX890QucX2ivQLHn0h@first.test";
    let own =
        add(&runtime, film, "example.video", poster, posted, &["movie.mkv|[7/8] movie", "movie.par2|[1/8] recovery"]);
    let rail = add(
        &runtime,
        film,
        "example.rail",
        "ZbivKJ385zX890QzzX2ivQLHn0h@rail.test",
        "2026-11-08 20:57:00",
        &["movie.mkv|[7/8] movie"],
    );
    let thumb = add(
        &runtime,
        "thumbnail.jpg",
        "example.picture",
        "ZbivKJ385zX890Qm0z0Yit@picture.test",
        "2026-11-08 20:40:00",
        &["thumbnail.jpg|[8/8] thumbnail"],
    );
    add(
        &runtime,
        "thumbnail.jpg [00000001]",
        "example.other",
        "otherUploader@example.test",
        posted,
        &["thumbnail.jpg|[8/8] thumbnail"],
    );
    add(
        &runtime,
        "thumbnail.jpg [00000002]",
        "example.counter",
        "ZbivKJ385zX890Qq@picture.test",
        posted,
        &["thumbnail.jpg|[5/5] thumbnail"],
    );
    let reads = StepReads::for_owner(&runtime.owner).unwrap();
    let (posters, mut rows) = reads.post_posters(film, film, Some(poster), posted).unwrap();
    rows.sort_unstable();
    let mut expected = vec![own, rail];
    expected.sort_unstable();
    assert_eq!(posters.len(), 2);
    assert_eq!(rows, expected);
    let slots = [7, 1].into_iter().collect();
    runtime
        .owner
        .with(|client| {
            client.batch_execute("drop index if exists idx_release_poster")?;
            Ok(())
        })
        .unwrap();
    for indexed in [false, true] {
        if indexed {
            runtime
                .owner
                .with(|client| {
                    client.batch_execute("create index if not exists idx_release_poster on releases(poster)")?;
                    Ok(())
                })
                .unwrap();
        }
        assert_eq!(reads.find_thumbnail(own, 8, &slots, &posters, posted).unwrap(), Some(thumb));
        assert_eq!(reads.find_thumbnail(own, 8, &[7, 1, 8].into_iter().collect(), &posters, posted).unwrap(), None);
        assert_eq!(reads.find_thumbnail(own, 9, &slots, &posters, posted).unwrap(), None);
    }
    runtime.owner.close();
}

#[test]
fn native_named_beside_candidates_keep_order_single_file_ownership_and_tried_rules() {
    let Some(schema) = crate::pg::testing::Schema::new() else { return };
    let runtime = crate::pg::stage_queue_tests::runtime(&schema);
    let posted = "2026-10-10 00:01:39";
    let own = add(&runtime, "Film", "example.video", "p@test", posted, &["Film.mkv", "Film.par2"]);
    let first =
        add(&runtime, "00-thumbnail.jpg", "example.first", "p@test", "2026-10-10 00:01:40", &["00-thumbnail.jpg"]);
    add(&runtime, "00-thumbnail.jpg [1]", "example.late", "p@test", "2026-10-10 02:00:00", &["00-thumbnail.jpg"]);
    let next =
        add(&runtime, "00-thumbnail.jpg [2]", "example.next", "p@test", "2026-10-10 00:02:05", &["00-thumbnail.jpg"]);
    add(&runtime, "pictures", "example.multiple", "p@test", "2026-10-10 00:01:41", &["00-thumbnail.jpg", "01.jpg"]);
    let taken =
        add(&runtime, "00-thumbnail.jpg [3]", "example.taken", "p@test", "2026-10-10 00:03:00", &["00-THUMBNAIL.JPG"]);
    runtime
        .owner
        .with(|client| {
            client.execute("insert into upload_link(release_id,lead_id) values($1,$2)", &[&taken, &own])?;
            Ok(())
        })
        .unwrap();
    let reads = StepReads::for_owner(&runtime.owner).unwrap();
    let window = beside_window(posted, BESIDE_SECS).unwrap();
    let candidates = |id| {
        reads
            .link_beside_candidates(id, "00-thumbnail.jpg", (&window.0, &window.1, posted))
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(candidates(own), [first, next]);
    runtime
        .owner
        .with(|client| {
            client.execute(
                "insert into beside_files(row_id,release_id,par2_file) values($1,$2,$3)",
                &[&first, &own, &vec![7u8; 16]],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(candidates(own), [first, next]);
    assert!(!candidates(next).contains(&first), "another upload cannot take an associated small file");
    runtime
        .owner
        .with(|client| {
            client.execute("insert into upload_link_tried(release_id,candidate_id) values($1,$2)", &[&own, &next])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(candidates(own), [first]);
    runtime.owner.close();
}

#[test]
fn native_par2_results_round_trip_complete_bound_sealed_creator_and_partial_state() {
    use crate::pg::stage_queue::{Queue, Settlement, Visit};
    use crate::postprocess::apply::Applied;
    let Some(schema) = crate::pg::testing::Schema::new() else { return };
    let runtime = crate::pg::stage_queue_tests::runtime(&schema);
    let own = add(&runtime, "Complete.Set", "example.complete", "p@test", "2026-11-13 12:00:00", &["complete.par2"]);
    let mk = |id, size, bound| par2::Par2File {
        file_id: [id; 16],
        set_id: [9; 16],
        md5: [id; 16],
        md5_16k: [id; 16],
        size,
        name: format!("{id}.mkv"),
        bound,
        crc32: Some(u32::MAX),
    };
    let files = vec![mk(1, 2 << 20, true), mk(2, 3 << 20, true), mk(3, 4 << 20, false)];
    let inventory = par2::Inventory {
        files: files.clone(),
        block_size: Some(1024),
        complete: true,
        sealed: vec![([9; 16], [1; 16])],
        creators: vec![([9; 16], "Fixture Creator".into())],
        ..Default::default()
    };
    let queue = Queue::for_owner(&runtime.owner).unwrap();
    let claim = queue.claim(10, 1, 60).unwrap().pop().unwrap();
    assert_eq!(claim.release_id(), own);
    let writes = crate::archive::store::par2_file_writes(own, &inventory);
    let applied = Applied {
        release_id: own,
        set_done: mask(LINK_STEP),
        writes: writes.into_iter().map(|w| (mask(LINK_STEP), w)).collect(),
        ..Default::default()
    };
    assert_eq!(
        queue.settle(&claim, Visit::prepare(&claim, applied, [7; 32]).unwrap(), 11).unwrap(),
        Settlement::Applied
    );
    let reads = StepReads::for_owner(&runtime.owner).unwrap();
    let back = reads.par2_inventory(own).unwrap().unwrap();
    assert_eq!(back.files, files);
    assert_eq!(reads.par2_file_names(own).unwrap(), ["1.mkv", "2.mkv", "3.mkv"]);
    assert_eq!(back.sealed, inventory.sealed);
    assert_eq!(back.creators, inventory.creators);
    assert!(back.complete);
    assert!(!back.files[2].bound);
    assert_eq!(wanted(&back.files).len(), 2);
    assert_eq!(sealed_set(&back, &wanted(&back.files)), None, "the second bound file was not sealed");
    let partial = add(&runtime, "Partial.Set", "example.partial", "p@test", "2026-11-13 12:00:00", &["partial.par2"]);
    let claim = queue.claim(12, 1, 60).unwrap().pop().unwrap();
    assert_eq!(claim.release_id(), partial);
    let partial_inventory = par2::Inventory { complete: false, ..inventory };
    let writes = crate::archive::store::par2_file_writes(partial, &partial_inventory);
    let applied = Applied {
        release_id: partial,
        set_done: mask(LINK_STEP),
        writes: writes.into_iter().map(|w| (mask(LINK_STEP), w)).collect(),
        ..Default::default()
    };
    assert_eq!(
        queue.settle(&claim, Visit::prepare(&claim, applied, [8; 32]).unwrap(), 13).unwrap(),
        Settlement::Applied
    );
    assert!(reads.par2_inventory(partial).unwrap().is_none());
    runtime.owner.close();
    assert!(reads.par2_inventory(own).is_err());
}
