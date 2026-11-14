//! Native replacements for the seven former `tests/archive_schema.rs` cases.
//!
//! Live contracts retained here:
//! - `reads_come_back_in_seq_order` -> the same name: exact sequence/source,
//!   nullable sizes, unknown password and numeric password codes 0/1/2.
//! - `a_source_replaces_only_its_own_rows` -> the same name: RAR replacement
//!   leaves the PAR2 listing intact.
//! - `passwords_read_across_shards` -> `passwords_read_across_native_groups`:
//!   actual issued IDs, batched passwords, own inner view and archive absence.
//! - `deleting_a_release_gives_its_names_back` ->
//!   `deleting_a_release_reclaims_names_and_keeps_the_natural_archive`:
//!   empty deletion keeps 1000, deleting `abc` releases 35, and names cascade.
//!
//! Strict SQLite mechanisms retired, not recreated:
//! - `both_tables_are_registered`: no SHARD_TABLES registry; real migrated
//!   tables, typed writes, reads and cascades exercise the storage guarantee.
//! - `an_old_database_reads_as_nothing_known`: a missing required native
//!   relation is an error, not a miss. Valid absence is retained below; ended
//!   owner/error propagation is retained in the existing StageReads controls.
//! - `compacting_keeps_the_give_back`: no SQLite shard-copy/compaction engine.
//!   The deletion case exercises the migrated native accounting trigger.
//!
//! No PostgreSQL URL means optional CI admission, as in the native shipping
//! fixtures. With a URL, every case uses the canonical Runtime and queue; no
//! fabricated release/lease/receipt identity or direct result writer is used.

use super::{ArchiveKeys, FACTS, InnerFileRow, InnerView, SOURCE_PAR2, SOURCE_RAR};
use crate::archive::{InnerFile, Password};
use crate::pg::{
    fresh_reader::{Budget as ReadBudget, ReadView},
    indexer_runtime::Runtime,
    natural_cache::{self, Budget as CacheBudget, Key},
    stage_queue::{Queue, Settlement, Visit},
    stage_reads::StepReads,
    testing::Schema,
};
use crate::postprocess::{
    apply::Applied,
    step::{StepMutation, StepWrite, mask},
};
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

const GROUP: &str = "example.archive.schema";
const POSTED: &str = "Fri, 13 Nov 2026 12:00:00 +0000";
const CACHE_BUDGET: CacheBudget = CacheBudget { bytes: 1 << 20, rows: 32 };

struct Fixture {
    runtime: Runtime,
    schema: Schema,
    _headers: tempfile::TempDir,
    clock: i64,
}

impl Fixture {
    fn new() -> Option<Self> {
        let schema = Schema::new()?;
        let mut headers = tempfile::Builder::new();
        headers.prefix("atlas-archive-schema-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            headers.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let headers = headers.tempdir().unwrap();
        let mut config = crate::config::Config::default();
        config.postprocess.enabled = false;
        config.predb.enabled = false;
        config.spots.enabled = false;
        config.inferred_parts = false;
        config.media.tvmaze.enabled = false;
        config.media.allocine.enabled = false;
        config.media.details_per_minute = 0;
        let runtime = Runtime::open(
            &config,
            crate::pg::StorageConfig {
                backend: crate::pg::Backend::Postgres,
                url: schema.url.clone(),
                schema: schema.name.clone(),
                max_connections: 1,
                allow_plaintext: true,
            },
            crate::config::HeaderStoreConfig { enabled: true, dir: headers.path().to_str().unwrap().into() },
            &[],
            &mut None,
        )
        .unwrap();
        Some(Self { runtime, schema, _headers: headers, clock: 10 })
    }

    fn add(&self, name: &str, group: &str) -> i64 {
        let settle = crate::catalog_facts::SETTLE_SECS
            .load(Ordering::Relaxed)
            .max(crate::catalog_facts::COMPLETE_SETTLE_SECS.load(Ordering::Relaxed))
            .max(1);
        self.runtime
            .owner
            .with(|client| {
                let saved = crate::pg::release_writer::save(
                    client,
                    &[crate::parser::Release {
                        name: name.into(),
                        group: group.into(),
                        poster: "archive-fixture@example.test".into(),
                        date: POSTED.into(),
                        articles: vec![crate::parser::Article {
                            filename: Some("payload.rar".into()),
                            subject: "payload.rar yEnc (1/1)".into(),
                            author: "archive-fixture@example.test".into(),
                            date: POSTED.into(),
                            message_id: format!("<{group}.{name}@archive-schema.test>"),
                            part: Some(1),
                            total_parts: Some(1),
                            bytes: 50,
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    1i64.saturating_sub(settle),
                )?;
                assert_eq!((saved.releases, saved.files, saved.parts), (1, 1, 1));
                Ok(client
                    .query_one(
                        "select id from releases where original_name=$1 and group_name=$2",
                        &[&name.as_bytes(), &group],
                    )?
                    .get(0))
            })
            .unwrap()
    }

    fn reads(&self) -> StepReads {
        StepReads::for_owner(&self.runtime.owner).unwrap()
    }

    fn settle(&mut self, id: i64, mutations: Vec<StepMutation>, delete: bool) {
        let now = self.clock;
        self.clock += 3;
        let bit = mask(crate::archive::steps::LIST_STEP);
        self.runtime
            .owner
            .with(|client| {
                let mut tx = client.transaction()?;
                assert!(crate::pg::stage_queue::rerun_in(&mut tx, id, bit, now)?);
                tx.commit()?;
                Ok(())
            })
            .unwrap();
        let queue = Queue::for_owner(&self.runtime.owner).unwrap();
        // The real newest-due turn selects the explicit intent just requested;
        // unrelated initial writer intents remain older, not fabricated leases.
        let claim = queue.claim_turn(now, 1, 60, 0).unwrap().pop().unwrap();
        assert_eq!(claim.release_id(), id);
        let reads = self.reads();
        let snapshot = reads.snapshot(id).unwrap().unwrap();
        let mut applied = Applied::native_database(id, snapshot.rerun_gen, claim.due_at(), bit, 0);
        applied.parts_seen = Some(snapshot.info.parts);
        applied.set_done = bit;
        applied.writes = mutations.into_iter().map(|mutation| (bit, StepWrite::for_mutation(id, mutation))).collect();
        if delete {
            assert!(reads.files(id, 2).unwrap().iter().all(|file| !crate::junk::is_video(&file.filename)));
            // Exercise the closed native deletion path. Queue rechecks the
            // genuine nonvideo files; this tests storage, not PC classification.
            applied.junk = Some(crate::junk::PC);
        }
        let visit = Visit::prepare_observed(&claim, applied, snapshot.input, reads.observed().unwrap()).unwrap();
        assert_eq!(queue.settle(&claim, visit, now + 1).unwrap(), Settlement::Applied);
        self.runtime
            .owner
            .with(|client| {
                assert_eq!(
                    client
                        .query_one(
                            "select count(*) from native_operations where operation_id=$1",
                            &[&claim.operation_id().as_slice()]
                        )?
                        .get::<_, i64>(0),
                    1
                );
                Ok(())
            })
            .unwrap();
        assert_eq!(self.runtime.owner.failure(), None);
    }

    fn write(&mut self, id: i64, mutations: Vec<StepMutation>) {
        self.settle(id, mutations, false);
    }

    fn delete(&mut self, id: i64) {
        self.settle(id, Vec::new(), true);
    }

    fn view<T>(&self, read: impl FnOnce(&ReadView<'_>) -> anyhow::Result<T>) -> T {
        self.runtime
            .owner
            .with(|client| {
                let view = ReadView::open(
                    client,
                    &self.schema.name,
                    ReadBudget::default(),
                    Instant::now() + Duration::from_secs(5),
                )?;
                read(&view)
            })
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.runtime.owner.close();
    }
}

fn file(source: i64, name: &str, size: Option<u64>) -> StepMutation {
    StepMutation::KeepArchiveFile { sequence: source * 65_536, source, file: InnerFile::new(name.into(), size) }
}

#[test]
fn reads_come_back_in_seq_order() {
    let Some(mut fixture) = Fixture::new() else { return };
    let id = fixture.add("Archive.Order", GROUP);
    fixture.write(
        id,
        vec![
            file(SOURCE_RAR, "b.mkv", Some(5)),
            file(SOURCE_PAR2, "a.mkv", None),
            StepMutation::AdjustArchiveNameBytes { delta: 74 },
        ],
    );
    let expected = vec![
        InnerFileRow { seq: 65_536, name: "a.mkv".into(), size: None, source: SOURCE_PAR2 },
        InnerFileRow { seq: 131_072, name: "b.mkv".into(), size: Some(5), source: SOURCE_RAR },
    ];
    assert_eq!(fixture.reads().archive_inner_files(id).unwrap(), expected);
    assert_eq!(fixture.reads().archive_password(id).unwrap(), None);
    fixture.view(|view| {
        assert_eq!(view.own_inner(id)?.files, expected);
        Ok(())
    });
    for (code, password) in [(0, Password::No), (1, Password::Yes), (2, Password::Maybe)] {
        fixture.write(id, vec![StepMutation::SetArchivePassword { password }]);
        assert_eq!(fixture.reads().archive_password(id).unwrap(), Some(password));
        fixture
            .runtime
            .owner
            .with(|client| {
                assert_eq!(
                    client
                        .query_one("select password from archive_release where release_id=$1", &[&id])?
                        .get::<_, i64>(0),
                    code
                );
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn a_source_replaces_only_its_own_rows() {
    let Some(mut fixture) = Fixture::new() else { return };
    let id = fixture.add("Archive.Replace", GROUP);
    fixture.write(
        id,
        vec![
            file(SOURCE_PAR2, "a.mkv", None),
            file(SOURCE_RAR, "old.mkv", None),
            StepMutation::AdjustArchiveNameBytes { delta: 76 },
        ],
    );
    fixture.write(id, vec![StepMutation::ClearArchiveSource { source: SOURCE_RAR }, file(SOURCE_RAR, "new.mkv", None)]);
    let rows = fixture.reads().archive_inner_files(id).unwrap();
    assert_eq!(
        rows,
        [
            InnerFileRow { seq: 65_536, name: "a.mkv".into(), size: None, source: SOURCE_PAR2 },
            InnerFileRow { seq: 131_072, name: "new.mkv".into(), size: None, source: SOURCE_RAR },
        ]
    );
    assert_eq!(fixture.reads().archive_total_name_bytes().unwrap(), 76);
    assert_eq!(fixture.reads().archive_source_name_bytes(id, SOURCE_PAR2).unwrap(), 37);
    assert_eq!(fixture.reads().archive_source_name_bytes(id, SOURCE_RAR).unwrap(), 39);
}

#[test]
fn passwords_read_across_native_groups() {
    let Some(mut fixture) = Fixture::new() else { return };
    let a = fixture.add("Archive.Password.A", "example.archive.a");
    fixture.write(a, vec![StepMutation::SetArchivePassword { password: Password::Yes }]);
    let b = fixture.add("Archive.Password.B", "example.archive.b");
    fixture.write(
        b,
        vec![
            StepMutation::SetArchivePassword { password: Password::Maybe },
            file(SOURCE_RAR, "x.mkv", Some(9)),
            StepMutation::AdjustArchiveNameBytes { delta: 37 },
        ],
    );
    let none = fixture.add("Archive.Password.Unknown", "example.archive.none");
    assert_ne!(a, b);
    assert_ne!(b, none);
    fixture.view(|view| {
        let facts = view.api_facts(&[a, b, none])?;
        let passwords = facts
            .into_iter()
            .filter_map(|(id, fact)| fact.password.map(|password| (id, password)))
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(passwords.len(), 2);
        assert_eq!(passwords[&a], Password::Yes);
        assert_eq!(passwords[&b], Password::Maybe);
        let inner = view.own_inner(b)?;
        assert_eq!(inner.password, Some(Password::Maybe));
        assert_eq!(
            inner.files,
            [InnerFileRow { seq: 131_072, name: "x.mkv".into(), size: Some(9), source: SOURCE_RAR }]
        );
        assert_eq!(view.own_inner(none)?, InnerView::default());
        Ok(())
    });
}

#[test]
fn deleting_a_release_reclaims_names_and_keeps_the_natural_archive() {
    let Some(mut fixture) = Fixture::new() else { return };
    let keeper = fixture.add("Archive.Keeper", GROUP);
    let kept_name = "z".repeat(933);
    fixture
        .write(keeper, vec![file(SOURCE_RAR, &kept_name, None), StepMutation::AdjustArchiveNameBytes { delta: 965 }]);
    let id = fixture.add("Archive.To.Delete", GROUP);
    fixture.write(
        id,
        vec![
            file(SOURCE_RAR, "abc", None),
            StepMutation::AdjustArchiveNameBytes { delta: 35 },
            StepMutation::SetArchivePassword { password: Password::Yes },
        ],
    );
    let empty = fixture.add("Archive.Empty", GROUP);
    assert_eq!(fixture.reads().archive_total_name_bytes().unwrap(), 1000);
    let reads = fixture.reads();
    let snapshot = reads.snapshot(id).unwrap().unwrap();
    let content = reads.content_id(id).unwrap().unwrap();
    let cached_files = [InnerFile::new("abc".into(), None)];
    let record = crate::raw_store::ArchiveRecord {
        name: "Archive.To.Delete",
        group_name: GROUP,
        content_id: &content,
        source: SOURCE_RAR,
        files: &cached_files,
        password: Some(1),
        encryption: None,
        names_hidden: false,
        password_check: None,
        parts: snapshot.info.parts,
        complete: snapshot.info.complete,
        listing_complete: true,
        listing_damaged: false,
        content_fp: None,
        max_names_bytes: 1 << 20,
        keys: ArchiveKeys::default(),
        key_salt: None,
        facts: FACTS,
    };
    fixture
        .runtime
        .owner
        .with(|client| {
            let mut tx = client.transaction()?;
            natural_cache::put_archive(&mut tx, &record, CACHE_BUDGET)?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    fixture.delete(empty);
    assert_eq!(fixture.reads().archive_total_name_bytes().unwrap(), 1000, "it held no names");
    fixture.delete(id);
    assert_eq!(fixture.reads().archive_total_name_bytes().unwrap(), 965);
    assert!(fixture.reads().archive_inner_files(id).unwrap().is_empty());
    assert_eq!(fixture.reads().archive_password(id).unwrap(), None);
    assert_eq!(fixture.reads().archive_source_name_bytes(keeper, SOURCE_RAR).unwrap(), 965);
    fixture.view(|view| {
        assert_eq!(view.own_inner(id)?, InnerView::default());
        Ok(())
    });
    fixture
        .runtime
        .owner
        .with(|client| {
            assert_eq!(
                client
                    .query_one("select count(*) from releases where id=any($1)", &[&vec![id, empty]])?
                    .get::<_, i64>(0),
                0
            );
            let saved = natural_cache::archive(
                client,
                Key { name: record.name, group: GROUP, content_id: &content },
                record.parts,
                record.complete,
                CACHE_BUDGET,
            )?
            .unwrap();
            assert_eq!(saved.password, Some(1));
            assert_eq!(saved.files.len(), 1);
            assert_eq!(
                (saved.files[0].seq, saved.files[0].name.as_str(), saved.files[0].size, saved.files[0].source),
                (131_072, "abc", None, SOURCE_RAR)
            );
            Ok(())
        })
        .unwrap();
}
