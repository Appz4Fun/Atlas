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
