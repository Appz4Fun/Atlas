use super::*;

fn row<'a>(release: i64, file: i64, id: &'a [u8], part: Option<i64>) -> NewRow<'a> {
    NewRow { release_id: release, file_id: file, message_id: id, part, bytes: 10, envelope: None }
}

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_writer(dir.path(), "test").unwrap();
    (dir, store)
}

#[test]
fn a_save_reports_only_the_parts_a_file_did_not_have() {
    let (_dir, store) = store();
    let first = store
        .add("1", &[row(1, 7, b"<a>", Some(1)), row(1, 7, b"<b>", Some(1)), row(1, 7, b"<c>", Some(3)), row(1, 8, b"<d>", None)], 100)
        .unwrap();
    assert_eq!(first, vec![NewParts { file_id: 7, count: 2, first: 1, last: 3 }]);
    let second = store.add("2", &[row(1, 7, b"<e>", Some(3)), row(1, 7, b"<f>", Some(2)), row(1, 7, b"<g>", Some(101))], 100).unwrap();
    assert_eq!(second, vec![NewParts { file_id: 7, count: 1, first: 2, last: 2 }]);
    assert_eq!(store.has(&[(1, b"<a>"), (2, b"<a>"), (1, b"<zz>")]).unwrap(), vec![true, false, false]);
    assert!(store.add("3", &[row(1, 9, b"<a>", Some(9))], 100).is_err(), "an article a release has cannot be added twice");
    assert_eq!(store.has(&[(1, b"<a>")]).unwrap(), vec![true]);
}

#[test]
fn a_rolled_back_save_is_taken_out_and_a_committed_one_stays() {
    let (_dir, store) = store();
    let envelope = Envelope { subject: b"s".to_vec(), nzb_timestamp: 5, ..Default::default() };
    store.add("10", &[row(1, 7, b"<kept>", Some(1))], 100).unwrap();
    store
        .add("11", &[NewRow { envelope: Some(envelope), ..row(1, 7, b"<gone>", Some(2)) }, row(2, 9, b"<gone2>", Some(1))], 100)
        .unwrap();
    store.add("12", &[row(3, 11, b"<running>", Some(1))], 100).unwrap();
    assert!(store.has_unsettled().unwrap());
    store
        .settle(|xid| {
            Ok(match xid {
                "10" => Ended::Committed,
                "11" => Ended::RolledBack,
                _ => Ended::Running,
            })
        })
        .unwrap();
    assert_eq!(
        store.has(&[(1, b"<kept>"), (1, b"<gone>"), (2, b"<gone2>"), (3, b"<running>")]).unwrap(),
        vec![true, false, false, true]
    );
    assert!(store.envelope(1, b"<gone>").unwrap().is_none());
    assert!(store.has_unsettled().unwrap(), "a running transaction keeps waiting");
    // The part a rolled-back save claimed is new again for the save that replays it.
    assert_eq!(
        store.add("13", &[row(1, 7, b"<gone>", Some(2))], 100).unwrap(),
        vec![NewParts { file_id: 7, count: 1, first: 2, last: 2 }]
    );
    store.settle(|_| Ok(Ended::Committed)).unwrap();
    assert!(!store.has_unsettled().unwrap());
}

#[test]
fn a_store_is_bound_to_its_catalogue_and_readers_see_the_writer() {
    let (dir, store) = store();
    store.add("1", &[row(1, 7, b"<a>", Some(1)), row(1, 7, b"<b>", Some(2))], 100).unwrap();
    let reader = Store::open_reader(dir.path()).unwrap();
    assert_eq!(reader.has(&[(1, b"<a>")]).unwrap(), vec![true]);
    store.add("2", &[row(1, 7, b"<c>", Some(3))], 100).unwrap();
    assert_eq!(reader.has(&[(1, b"<c>")]).unwrap(), vec![true], "a reader sees a later commit");
    assert!(reader.add("3", &[row(1, 7, b"<d>", Some(4))], 100).is_err());
    drop(store);
    assert!(Store::open_writer(dir.path(), "another").is_err());
    assert!(Store::open_writer(dir.path(), "test").is_ok());
}

#[test]
fn a_save_still_running_here_is_not_asked_about_unless_it_held_the_group() {
    let (_dir, store) = store();
    store.add("5", &[row(1, 7, b"<a>", Some(1))], 100).unwrap();
    store.begin("5", &[]);
    store.locked("5", "alt.binaries.held");
    // running here: nothing to ask about, by `of` or under another group's lock
    assert!(!store.has_unsettled().unwrap());
    assert!(!store.has_unsettled_besides(None, Some("alt.binaries.other")).unwrap());
    let mut asked = Vec::new();
    store.settle_under(Some("alt.binaries.other"), |xid| { asked.push(xid.to_owned()); Ok(Ended::RolledBack) }).unwrap();
    assert!(asked.is_empty());
    assert_eq!(store.has(&[(1, b"<a>")]).unwrap(), vec![true]);
    // under the lock it took it has ended, perhaps on the server's side: asked, and taken out when rolled back
    assert!(store.has_unsettled_besides(None, Some("alt.binaries.held")).unwrap());
    store.settle_under(Some("alt.binaries.held"), |xid| { asked.push(xid.to_owned()); Ok(Ended::RolledBack) }).unwrap();
    assert_eq!(asked, vec!["5".to_string()]);
    assert_eq!(store.has(&[(1, b"<a>")]).unwrap(), vec![false]);
    // once ended here it is asked about like any other
    store.add("6", &[row(2, 8, b"<b>", Some(1))], 100).unwrap();
    store.begin("6", &[]);
    assert!(!store.has_unsettled().unwrap());
    store.end("6");
    assert!(store.has_unsettled().unwrap());
}

#[test]
fn a_big_has_split_over_connections_answers_in_the_order_asked() {
    let (_dir, store) = store();
    let ids: Vec<Vec<u8>> = (0..3000).map(|n| format!("<{n}@x>").into_bytes()).collect();
    // every third article stored, over a few releases
    let stored: Vec<NewRow<'_>> =
        ids.iter().enumerate().filter(|(n, _)| n % 3 == 0).map(|(n, id)| row(1 + (n % 5) as i64, 7, id, None)).collect();
    store.add("1", &stored, 100).unwrap();
    store.confirm("1").unwrap();
    // asked in an order unlike the index's, past HAS_ALONE so it splits: every release taken for one not yet in
    // `seen`, so all of its keys read `segments` (cold), and one release above it that `seen` answers
    for shard in store.shards() {
        shard.seen_above.store(4, Ordering::Release);
    }
    let mut keys: Vec<(i64, &[u8])> = ids.iter().enumerate().rev().map(|(n, id)| (1 + (n % 5) as i64, id.as_slice())).collect();
    keys.push((5, b"<missing@x>"));
    let answers = store.has(&keys).unwrap();
    assert!(store.shards().iter().map(|s| s.has_splits.load(Ordering::Relaxed)).sum::<u64>() > 0, "the cold keys were split");
    assert!(!answers.last().unwrap());
    keys.pop();
    let answers = answers[..keys.len()].to_vec();
    for (key, known) in keys.iter().zip(&answers) {
        let n: usize = std::str::from_utf8(key.1).unwrap().trim_start_matches('<').split('@').next().unwrap().parse().unwrap();
        assert_eq!(*known, n % 3 == 0, "article {n}");
    }
}

#[test]
fn a_commit_confirmed_later_is_not_asked_about_and_is_marked_by_the_next_add() {
    let (_dir, store) = store();
    store.add("7", &[row(1, 7, b"<a>", Some(1))], 100).unwrap();
    store.confirm_later("7");
    assert!(!store.has_unsettled().unwrap(), "a commit seen here is not asked about");
    store.add("8", &[row(2, 8, b"<b>", Some(1))], 100).unwrap();
    // the add marked 7; 8 still waits
    let mut asked = Vec::new();
    store.settle(|xid| { asked.push(xid.to_owned()); Ok(Ended::Committed) }).unwrap();
    assert_eq!(asked, vec!["8".to_string()]);
    assert_eq!(store.has(&[(1, b"<a>"), (2, b"<b>")]).unwrap(), vec![true, true]);
}

#[test]
fn an_add_starts_the_wal_over_when_the_checkpointer_asks() {
    let (dir, store) = store();
    // every release in the one shard of release 10: each shard has a WAL of its own
    let shard = store.index_of(10);
    let ids: Vec<Vec<u8>> = (0..20_000).map(|n| format!("<wal-{n}@x>").into_bytes()).collect();
    for (chunk, ids) in ids.chunks(2000).enumerate() {
        let rows: Vec<NewRow<'_>> = ids.iter().map(|id| row(10 + 8 * chunk as i64, 7, id, None)).collect();
        assert_eq!(store.index_of(rows[0].release_id), shard);
        store.add(&format!("{}", 100 + chunk), &rows, 100).unwrap();
    }
    let path = dir.path().join(store::shard_file(10 % 8));
    let frames = |path: &std::path::Path| -> i64 {
        Connection::open(path).unwrap().query_row("pragma wal_checkpoint(PASSIVE)", [], |row| row.get(1)).unwrap()
    };
    let before = frames(&path);
    assert!(before > 50, "the WAL grew: {before}");
    store.on(10).restart_wanted.store(true, Ordering::Release);
    store.add("200", &[row(810, 9, b"<after>", Some(1))], 100).unwrap();
    let after = frames(&path);
    assert!(after < before, "the WAL started over: {before} then {after}");
    assert_eq!(store.has(&[(10, b"<wal-0@x>"), (810, b"<after>")]).unwrap(), vec![true, true]);
}

#[test]
fn a_store_from_before_seen_is_filled_newest_release_first_and_answers_the_same() {
    let dir = tempfile::tempdir().unwrap();
    let ids: Vec<Vec<u8>> = (0..12_000).map(|n| format!("<old-{n}@x>").into_bytes()).collect();
    {
        let store = Store::open_writer(dir.path(), "test").unwrap();
        // three releases, the middle one bigger than a fill batch
        for (release, ids) in [(5, &ids[..1000]), (6, &ids[1000..11_000]), (7, &ids[11_000..])] {
            let rows: Vec<NewRow<'_>> = ids.iter().map(|id| row(release, release * 10, id, None)).collect();
            store.add(&format!("{release}"), &rows, 100).unwrap();
            store.confirm(&format!("{release}")).unwrap();
        }
        // as a store written before `seen` was, in every file
        for shard in store.shards() {
            let conn = shard.lock().unwrap();
            conn.execute_batch("delete from seen; delete from meta where key in ('seen_above', 'seen_cursor');").unwrap();
        }
    }
    let store = Store::open_writer(dir.path(), "test").unwrap();
    // releases 5, 6 and 7 are in shards of their own, each filled from its newest release
    assert_eq!(store.on(7).seen_above.load(Ordering::Acquire), 7);
    assert_eq!(store.on(6).seen_above.load(Ordering::Acquire), 6);
    let filling = || store.shards().iter().any(|shard| shard.seen_above.load(Ordering::Acquire) != i64::MIN);
    let asked: Vec<(i64, &[u8])> =
        vec![(5, &ids[0][..]), (6, &ids[5000][..]), (7, &ids[11_500][..]), (6, &ids[0][..]), (8, b"<new>")];
    let want = vec![true, true, true, false, false];
    assert_eq!(store.has(&asked).unwrap(), want);
    let mut rounds = 0;
    while filling() {
        for shard in store.shards() {
            *shard.seen_filled_at.lock().unwrap() = None;
            shard.fill_seen(&mut shard.lock().unwrap()).unwrap();
        }
        // releases above the mark are whole in seen whenever it moves
        assert_eq!(store.has(&asked).unwrap(), want);
        rounds += 1;
        assert!(rounds < 10, "the fill finishes");
    }
    let count: i64 =
        store.shards().iter().map(|shard| shard.lock().unwrap().query_row("select count(*) from seen", [], |r| r.get::<_, i64>(0)).unwrap()).sum();
    assert_eq!(count, 12_000);
    // a rolled back save leaves seen too
    store.add("20", &[row(6, 60, b"<late>", None)], 100).unwrap();
    assert_eq!(store.has(&[(6, b"<late>")]).unwrap(), vec![true]);
    store.settle(|_| Ok(Ended::RolledBack)).unwrap();
    assert_eq!(store.has(&[(6, b"<late>")]).unwrap(), vec![false]);
}

/// A store as a build before the split wrote it: `articles.db` alone, without the split in its `meta`.
fn unsplit(dir: &std::path::Path, rows: &[NewRow<'_>]) {
    let old = Shard::open_writer(&dir.join(FILE), "test", SHARD_CACHE_KIB).unwrap();
    let rows: Vec<&NewRow<'_>> = rows.iter().collect();
    old.add("1", &rows, 100, &HashSet::new()).unwrap();
    old.confirm("1").unwrap();
}

#[test]
fn an_unsplit_store_keeps_its_releases_and_sends_newer_ones_to_the_shards() {
    let dir = tempfile::tempdir().unwrap();
    let envelope = Envelope { subject: b"old subject".to_vec(), nzb_timestamp: 9, ..Default::default() };
    unsplit(
        dir.path(),
        &[
            row(3, 30, b"<old-a>", Some(1)),
            row(3, 30, b"<old-b>", Some(2)),
            NewRow { envelope: Some(envelope.clone()), ..row(3, 31, b"<old-n>", None) },
            row(5, 50, b"<old-c>", Some(1)),
        ],
    );
    let store = Store::open_writer(dir.path(), "test").unwrap();
    assert_eq!(store.meta("shard_boundary").unwrap().as_deref(), Some("5"));
    assert_eq!((store.index_of(3), store.index_of(5)), (0, 0), "releases the file held stay in it");
    assert_ne!(store.index_of(6), 0);
    for n in 0..8 {
        assert!(dir.path().join(store::shard_file(n)).exists());
    }
    // every old article is found: none is taken for new and inserted twice
    assert_eq!(store.has(&[(3, b"<old-a>"), (3, b"<old-n>"), (5, b"<old-c>"), (3, b"<new>")]).unwrap(), vec![true, true, true, false]);
    assert!(store.add("2", &[row(3, 30, b"<old-a>", Some(1))], 100).is_err());
    // a later article of an old release goes to articles.db; its part is new to the file there
    assert_eq!(store.add("3", &[row(3, 30, b"<old-d>", Some(3)), row(9, 90, b"<new-a>", Some(1))], 100).unwrap(), vec![
        NewParts { file_id: 30, count: 1, first: 3, last: 3 },
        NewParts { file_id: 90, count: 1, first: 1, last: 1 },
    ]);
    store.confirm("3").unwrap();
    assert!(store.shards()[0].lock().unwrap().query_row("select exists(select 1 from segments where release_id = 3 and message_id = cast('<old-d>' as blob))", [], |r| r.get::<_, bool>(0)).unwrap());
    assert!(store.shards()[0].lock().unwrap().query_row("select not exists(select 1 from segments where release_id = 9)", [], |r| r.get::<_, bool>(0)).unwrap());
    // the old release's NZB and envelopes still read
    let page = store.nzb_page(3, 30, None, 10, usize::MAX).unwrap();
    assert_eq!(page.iter().map(|s| s.message_id.as_slice()).collect::<Vec<_>>(), vec![&b"<old-a>"[..], b"<old-b>", b"<old-d>"]);
    assert_eq!(store.envelope(3, b"<old-n>").unwrap(), Some(envelope));
    assert_eq!(store.file_part_bounds(30, 100).unwrap(), (Some(3), Some(1)));
    assert_eq!(store.file_part_bounds(90, 100).unwrap(), (Some(1), Some(1)));
    assert_eq!(store.segment_count().unwrap(), 6);
    // a reader sees both
    let reader = Store::open_reader(dir.path()).unwrap();
    assert_eq!(reader.has(&[(3, b"<old-d>"), (9, b"<new-a>")]).unwrap(), vec![true, true]);
    assert_eq!(reader.file_parts_page(30, None, 100, 10).unwrap(), vec![1, 2, 3]);
    drop((reader, store));
    // opened again: the same split
    let store = Store::open_writer(dir.path(), "test").unwrap();
    assert_eq!(store.index_of(5), 0);
    assert_eq!(store.has(&[(9, b"<new-a>"), (5, b"<old-c>")]).unwrap(), vec![true, true]);
    drop(store);
    // a build that doesnt split wrote a newer release into articles.db: refused, not looked for in a shard
    {
        let old = Shard::open_writer(&dir.path().join(FILE), "test", SHARD_CACHE_KIB).unwrap();
        old.add("4", &[&row(12, 120, b"<stray>", Some(1))], 100, &HashSet::new()).unwrap();
    }
    assert!(Store::open_writer(dir.path(), "test").is_err());
}

#[test]
fn a_reader_refuses_a_store_not_split_yet_and_a_shard_of_another_split() {
    let dir = tempfile::tempdir().unwrap();
    unsplit(dir.path(), &[row(1, 10, b"<a>", Some(1))]);
    assert!(Store::open_reader(dir.path()).is_err(), "the indexer splits it first");
    drop(Store::open_writer(dir.path(), "test").unwrap());
    assert!(Store::open_reader(dir.path()).is_ok());
    // a shard of another store put beside it
    let other = tempfile::tempdir().unwrap();
    let another = Store::open_writer(other.path(), "test").unwrap();
    another.add("1", &[row(2, 20, b"<b>", Some(1))], 100).unwrap();
    drop(another);
    let shard = store::shard_file(2 % 8);
    for suffix in ["", "-wal", "-shm"] {
        let from = other.path().join(format!("{shard}{suffix}"));
        if from.exists() {
            std::fs::copy(&from, dir.path().join(format!("{shard}{suffix}"))).unwrap();
        }
    }
    assert!(Store::open_writer(dir.path(), "test").is_err());
}

#[test]
fn saves_of_releases_in_different_shards_write_side_by_side() {
    let (_dir, store) = store();
    let (a, b) = (1, 2);
    assert_ne!(store.index_of(a), store.index_of(b));
    // release a's shard held by another writer: a save of release b goes on regardless
    let held = store.on(a).lock().unwrap();
    std::thread::scope(|scope| {
        let (done, wait) = std::sync::mpsc::channel();
        let store = &store;
        scope.spawn(move || {
            let gained = store.add("1", &[row(b, 20, b"<b1>", Some(1))], 100).unwrap();
            done.send(gained).unwrap();
        });
        let gained = wait.recv_timeout(std::time::Duration::from_secs(20)).expect("a save of another shard waited");
        assert_eq!(gained, vec![NewParts { file_id: 20, count: 1, first: 1, last: 1 }]);
        drop(held);
    });
    // many writers, each its own releases over every shard
    std::thread::scope(|scope| {
        for writer in 0..16i64 {
            let store = &store;
            scope.spawn(move || {
                let ids: Vec<Vec<u8>> = (0..200).map(|n| format!("<w{writer}-{n}>").into_bytes()).collect();
                for round in 0..5i64 {
                    let release = 100 + writer * 10 + round;
                    let rows: Vec<NewRow<'_>> =
                        ids[round as usize * 40..][..40].iter().enumerate().map(|(n, id)| row(release, release * 100, id, Some(n as i64))).collect();
                    let gained = store.add(&format!("x{writer}-{round}"), &rows, 100).unwrap();
                    assert_eq!(gained, vec![NewParts { file_id: release * 100, count: 40, first: 0, last: 39 }]);
                }
            });
        }
    });
    assert_eq!(store.segment_count().unwrap(), 1 + 16 * 5 * 40);
    let keys: Vec<(i64, &[u8])> = vec![(100, b"<w0-0>"), (254, b"<w15-199>"), (254, b"<w15-0>")];
    assert_eq!(store.has(&keys).unwrap(), vec![true, true, false]);
}

#[test]
fn a_save_over_several_shards_is_settled_in_every_one() {
    let (_dir, store) = store();
    let releases = [1i64, 2, 3, 4];
    assert_eq!(releases.iter().map(|&r| store.index_of(r)).collect::<HashSet<_>>().len(), 4);
    store.add("10", &releases.map(|r| row(r, r * 10, b"<kept>", Some(1))), 100).unwrap();
    let envelope = Envelope { subject: b"s".to_vec(), ..Default::default() };
    let gone: Vec<NewRow<'_>> = releases.iter().map(|&r| NewRow { envelope: Some(envelope.clone()), ..row(r, r * 10, b"<gone>", Some(2)) }).collect();
    // gains come back in the order of the rows, whichever shard each went to
    assert_eq!(
        store.add("11", &gone, 100).unwrap(),
        releases.iter().map(|&r| NewParts { file_id: r * 10, count: 1, first: 2, last: 2 }).collect::<Vec<_>>()
    );
    let mut asked = Vec::new();
    store
        .settle(|xid| {
            asked.push(xid.to_owned());
            Ok(if xid == "11" { Ended::RolledBack } else { Ended::Committed })
        })
        .unwrap();
    asked.sort();
    assert_eq!(asked, vec!["10".to_string(), "11".to_string()], "each transaction asked about once");
    for r in releases {
        assert_eq!(store.has(&[(r, b"<kept>"), (r, b"<gone>")]).unwrap(), vec![true, false]);
        assert!(store.envelope(r, b"<gone>").unwrap().is_none());
    }
    assert!(!store.has_unsettled().unwrap());
    // seen to commit: not asked about, and marked by the next add of each shard it wrote
    store.add("20", &[row(1, 10, b"<c1>", Some(3)), row(2, 20, b"<c2>", Some(3))], 100).unwrap();
    store.confirm_later("20");
    assert!(!store.has_unsettled().unwrap());
    store.add("21", &[row(1, 10, b"<c3>", Some(4))], 100).unwrap();
    let mut asked = Vec::new();
    store.settle(|xid| { asked.push(xid.to_owned()); Ok(Ended::Committed) }).unwrap();
    assert_eq!(asked, vec!["21".to_string()]);
    store.add("22", &[row(2, 20, b"<c4>", Some(4))], 100).unwrap();
    store.confirm("22").unwrap();
    assert!(!store.has_unsettled().unwrap());
    for shard in store.shards() {
        assert!(shard.confirmed.lock().unwrap().is_empty(), "every shard marked it");
    }
    // a save running here that held the group is asked about in every shard it wrote
    store.add("30", &[row(3, 30, b"<r1>", Some(5)), row(4, 40, b"<r2>", Some(5))], 100).unwrap();
    store.begin("30", &["g".to_string()]);
    assert!(!store.has_unsettled().unwrap());
    store.settle_under(Some("g"), |_| Ok(Ended::RolledBack)).unwrap();
    store.end("30");
    assert_eq!(store.has(&[(3, b"<r1>"), (4, b"<r2>"), (3, b"<kept>")]).unwrap(), vec![false, false, true]);
}

#[test]
fn a_retraction_over_several_shards_drops_each_release_once_committed() {
    let dir = tempfile::tempdir().unwrap();
    unsplit(dir.path(), &[row(1, 10, b"<old>", Some(1))]);
    let store = Store::open_writer(dir.path(), "test").unwrap();
    store.add("1", &[row(2, 20, b"<a>", Some(1)), row(3, 30, b"<b>", Some(1)), row(4, 40, b"<c>", Some(1))], 100).unwrap();
    store.confirm("1").unwrap();
    // rolled back: kept
    store.retract("2", &[1, 2, 3]).unwrap();
    store.settle(|_| Ok(Ended::RolledBack)).unwrap();
    assert_eq!(store.has(&[(1, b"<old>"), (2, b"<a>"), (3, b"<b>")]).unwrap(), vec![true, true, true]);
    // committed: gone from articles.db and from the shards, release 4 untouched
    store.retract("3", &[1, 2, 3]).unwrap();
    store.settle(|_| Ok(Ended::Committed)).unwrap();
    assert_eq!(store.has(&[(1, b"<old>"), (2, b"<a>"), (3, b"<b>"), (4, b"<c>")]).unwrap(), vec![false, false, false, true]);
    assert!(!store.nzb_release_has_articles(2).unwrap());
    assert_eq!(store.file_part_bounds(10, 100).unwrap(), (None, None));
    assert_eq!(store.segment_count().unwrap(), 1);
    // confirmed later: marked by the next add of each shard, as a save's rows are
    store.add("4", &[row(5, 50, b"<d>", Some(1)), row(6, 60, b"<e>", Some(1))], 100).unwrap();
    store.confirm("4").unwrap();
    store.retract("5", &[5, 6]).unwrap();
    store.confirm_later("5");
    store.add("6", &[row(5, 51, b"<f>", Some(1)), row(6, 61, b"<g>", Some(1))], 100).unwrap();
    store.confirm("6").unwrap();
    assert_eq!(store.has(&[(5, b"<d>"), (6, b"<e>"), (5, b"<f>")]).unwrap(), vec![false, false, true]);
}

#[test]
fn reads_by_file_find_its_shard_and_answer_as_one_file_did() {
    let dir = tempfile::tempdir().unwrap();
    unsplit(dir.path(), &[row(1, 10, b"<old-1>", Some(1)), row(1, 10, b"<old-2>", Some(1)), row(1, 10, b"<old-3>", None)]);
    let store = Store::open_writer(dir.path(), "test").unwrap();
    let rows = [
        NewRow { bytes: 7, ..row(2, 20, b"<n-a>", None) },
        NewRow { bytes: 5, ..row(2, 20, b"<n-b>", Some(2)) },
        NewRow { bytes: 5, ..row(2, 20, b"<n-c>", Some(2)) },
        NewRow { bytes: 5, ..row(2, 20, b"<n-d>", Some(1)) },
        NewRow { bytes: 3, ..row(3, 30, b"<o-a>", Some(1)) },
    ];
    store.add("1", &rows, 100).unwrap();
    store.confirm("1").unwrap();
    // in articles.db, in a shard, and in neither
    assert_eq!(store.file_parts_within(10, -1, 100, 10).unwrap(), vec![1]);
    assert_eq!(store.file_parts_within(20, -1, 100, 10).unwrap(), vec![1, 2]);
    assert!(store.file_parts_within(99, -1, 100, 10).unwrap().is_empty());
    assert_eq!(store.files_bytes(&[10, 20, 30, 99]).unwrap(), vec![30, 22, 3, 0]);
    assert_eq!(store.file_counts(&[(1, 10, 100), (2, 20, 3), (3, 30, 100), (4, 99, 100)]).unwrap(), vec![3, 3, 1, 0]);
    assert_eq!(store.common_bytes(20, None).unwrap(), Some(5));
    assert_eq!(store.file_numbers(20).unwrap(), Some((Some(5), Some(7), 1)));
    assert_eq!(store.file_numbers(99).unwrap(), None);
    assert_eq!(store.file_range_counts(20, &[(1, 1), (1, 2), (3, 9)]).unwrap(), vec![1, 2, 0]);
    let ends = store.file_ends(20, false, 2, 1000).unwrap().unwrap();
    assert_eq!(ends.iter().map(|s| s.message_id.as_slice()).collect::<Vec<_>>(), vec![&b"<n-a>"[..], b"<n-d>"]);
    let ends = store.file_ends(20, true, 2, 1000).unwrap().unwrap();
    assert_eq!(ends.iter().map(|s| s.message_id.as_slice()).collect::<Vec<_>>(), vec![&b"<n-b>"[..], b"<n-c>"]);
    let copies = store.file_part_copies_capped(20, 1, 2, 1).unwrap();
    assert_eq!(copies, vec![(1, b"<n-d>".to_vec()), (2, b"<n-b>".to_vec())]);
    let proof = store.proof_page(10, 100, None, 10).unwrap();
    assert_eq!(proof.iter().map(|p| p.part).collect::<Vec<_>>(), vec![None, Some(1), Some(1)]);
    // NZB pages walk a file in a shard as in articles.db
    let walk = |release: i64, file: i64| -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut after: Option<(Option<i64>, Vec<u8>)> = None;
        loop {
            let page = store.nzb_page(release, file, after.as_ref().map(|(p, id)| (*p, id.as_slice())), 1, usize::MAX).unwrap();
            let Some(last) = page.last() else { break };
            after = Some((last.part, last.message_id.clone()));
            out.extend(page.into_iter().map(|s| s.message_id));
        }
        out
    };
    assert_eq!(walk(1, 10), vec![b"<old-3>".to_vec(), b"<old-1>".to_vec(), b"<old-2>".to_vec()]);
    assert_eq!(walk(2, 20), vec![b"<n-a>".to_vec(), b"<n-d>".to_vec(), b"<n-b>".to_vec(), b"<n-c>".to_vec()]);
    assert_eq!(store.nzb_file_totals(2, 20).unwrap(), (4, 22));
    assert_eq!(store.release_ids(2, None, 10, usize::MAX).unwrap().len(), 4);
    assert_eq!(store.release_count(1, 100).unwrap(), 3);
    assert!(!store.holds_nothing().unwrap());
    assert_eq!(store.segment_count().unwrap(), 8);
    // a file found once is remembered; one with no rows is not, so its first rows are found later
    store.add("2", &[row(4, 99, b"<late>", Some(1))], 100).unwrap();
    assert_eq!(store.file_parts_within(99, -1, 100, 10).unwrap(), vec![1]);
}

/// Saves queued behind a held write lock: `n` threads each save its own release (all in one file), and the lock is let
/// go once every one of them waits for it.
fn queued_saves<'a>(store: &'a Store, home: i64, saves: Vec<(String, Vec<NewRow<'a>>)>) -> Vec<Result<Vec<NewParts>>> {
    let shard = store.on(home);
    let held = shard.lock().unwrap();
    std::thread::scope(|scope| {
        let threads: Vec<_> = saves.into_iter().map(|(xid, rows)| scope.spawn(move || store.add(&xid, &rows, 100))).collect();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while shard.queue.lock().unwrap().len() < threads.len() {
            assert!(std::time::Instant::now() < deadline, "the saves never queued");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        drop(held);
        threads.into_iter().map(|t| t.join().unwrap()).collect()
    })
}

#[test]
fn saves_queued_together_commit_in_one_transaction_each_with_its_own_parts() {
    let (_dir, store) = store();
    // releases of one shard: 9, 17, 25, ... with eight shards past articles.db
    let releases: Vec<i64> = (0..12).map(|n| 9 + n * 8).collect();
    assert!(releases.iter().all(|&r| store.index_of(r) == store.index_of(releases[0])));
    store.add("0", &[row(releases[0], releases[0] * 10, b"<pre>", Some(2))], 100).unwrap();
    let before = store.on(releases[0]).commits();
    let ids: Vec<Vec<Vec<u8>>> = releases.iter().map(|r| (0..5).map(|n| format!("<{r}-{n}>").into_bytes()).collect()).collect();
    let saves = releases
        .iter()
        .zip(&ids)
        .map(|(&r, ids)| (format!("x{r}"), ids.iter().enumerate().map(|(n, id)| row(r, r * 10, id, Some(n as i64))).collect()))
        .collect();
    let answers = queued_saves(&store, releases[0], saves);
    for (&r, answer) in releases.iter().zip(answers) {
        let want = if r == releases[0] {
            // part 2 the file had already
            vec![NewParts { file_id: r * 10, count: 4, first: 0, last: 4 }]
        } else {
            vec![NewParts { file_id: r * 10, count: 5, first: 0, last: 4 }]
        };
        assert_eq!(answer.unwrap(), want, "release {r}");
    }
    let commits = store.on(releases[0]).commits() - before;
    assert!(commits < releases.len() as u64, "{commits} commits for {} saves", releases.len());
    assert_eq!(store.segment_count().unwrap(), 1 + 12 * 5);
    // each save waits under its own transaction
    let mut asked = Vec::new();
    store.settle(|xid| { asked.push(xid.to_owned()); Ok(Ended::Running) }).unwrap();
    asked.sort();
    let mut want: Vec<String> = std::iter::once("0".to_owned()).chain(releases.iter().map(|r| format!("x{r}"))).collect();
    want.sort();
    assert_eq!(asked, want);
}

#[test]
fn a_save_of_an_article_already_there_fails_alone_in_its_group() {
    let (_dir, store) = store();
    let (a, b, c) = (9, 17, 25);
    store.add("0", &[row(b, 170, b"<dup>", Some(1))], 100).unwrap();
    let saves = vec![
        ("xa".to_owned(), vec![row(a, 90, b"<a1>", Some(1))]),
        ("xb".to_owned(), vec![row(b, 170, b"<b2>", Some(2)), row(b, 170, b"<dup>", Some(1))]),
        ("xc".to_owned(), vec![row(c, 250, b"<c1>", Some(1))]),
    ];
    let answers = queued_saves(&store, a, saves);
    assert_eq!(answers[0].as_ref().unwrap(), &vec![NewParts { file_id: 90, count: 1, first: 1, last: 1 }]);
    let error = format!("{:#}", answers[1].as_ref().unwrap_err());
    assert!(error.contains("already has"), "{error}");
    assert_eq!(answers[2].as_ref().unwrap(), &vec![NewParts { file_id: 250, count: 1, first: 1, last: 1 }]);
    // the failed save left nothing: not its other row, nor an unsettled entry
    assert_eq!(store.has(&[(a, b"<a1>"), (b, b"<b2>"), (b, b"<dup>"), (c, b"<c1>")]).unwrap(), vec![true, false, true, true]);
    let mut asked = Vec::new();
    store.settle(|xid| { asked.push(xid.to_owned()); Ok(Ended::Running) }).unwrap();
    asked.sort();
    assert_eq!(asked, vec!["0", "xa", "xc"]);
}

#[test]
fn a_rolled_back_save_in_a_shared_group_takes_out_only_its_rows() {
    let (_dir, store) = store();
    let (a, b, c) = (9, 17, 25);
    let saves = vec![
        ("xa".to_owned(), vec![row(a, 90, b"<a1>", Some(1)), row(a, 90, b"<a2>", Some(2))]),
        ("xb".to_owned(), vec![row(b, 170, b"<b1>", Some(1))]),
        ("xc".to_owned(), vec![row(c, 250, b"<c1>", Some(1))]),
    ];
    let before = store.on(a).commits();
    for answer in queued_saves(&store, a, saves) {
        answer.unwrap();
    }
    assert!(store.on(a).commits() - before < 3);
    store.confirm("xc").unwrap();
    store
        .settle(|xid| Ok(match xid {
            "xb" => Ended::RolledBack,
            "xa" => Ended::Running,
            other => panic!("{other} was confirmed"),
        }))
        .unwrap();
    assert_eq!(store.has(&[(a, b"<a1>"), (a, b"<a2>"), (b, b"<b1>"), (c, b"<c1>")]).unwrap(), vec![true, true, false, true]);
    store.settle(|xid| { assert_eq!(xid, "xa"); Ok(Ended::Committed) }).unwrap();
    assert!(!store.has_unsettled().unwrap());
    assert_eq!(store.has(&[(a, b"<a1>"), (a, b"<a2>")]).unwrap(), vec![true, true]);
}

#[test]
fn idle_readers_are_kept_up_to_one_total_over_every_file() {
    let (_dir, store) = store();
    std::thread::scope(|scope| {
        for shard in store.shards() {
            for _ in 0..40 {
                scope.spawn(move || shard.read(|_| { std::thread::sleep(std::time::Duration::from_millis(50)); Ok(()) }).unwrap());
            }
        }
    });
    let kept: usize = store.shards().iter().map(|s| s.readers.lock().unwrap().len()).sum();
    assert!(kept <= READERS_KEPT, "{kept} idle readers kept");
    assert_eq!(kept, store.shards()[0].idle_readers.load(Ordering::Acquire));
}
