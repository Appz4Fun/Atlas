//! Native counterparts of the retired shard sorter's equal-value oracles.
use crate::{
    config::HeaderStoreConfig,
    field_search::Fields,
    parser::{Article, Release},
    pg::{self, Backend, StorageConfig, fresh_reader::ReadView},
    web::SortBy,
    word_index::{
        By, WordIndex,
        native::{Budget, SearchSource},
    },
};
use std::time::{Duration, Instant};

#[test]
#[ignore = "requires explicit isolated PostgreSQL owner and owned native Tantivy directory"]
fn native_equal_present_values_keep_newest_ids_in_both_directions() {
    let schema = pg::testing::Schema::native_writer().expect("explicit isolated PostgreSQL fixture");
    let mut client = schema.pool.get().unwrap();
    client.query_one("select pg_advisory_lock(hashtext('atlas.indexer'),hashtext($1))", &[&schema.name]).unwrap();
    let root = tempfile::tempdir().unwrap();
    let headers = HeaderStoreConfig { enabled: true, dir: root.path().to_str().unwrap().into() };
    let storage = StorageConfig {
        backend: Backend::Postgres,
        url: schema.url.clone(),
        schema: schema.name.clone(),
        max_connections: 4,
        allow_plaintext: true,
    };
    let version = pg::migrate::version(&mut client, &schema.name).unwrap();
    let epoch = pg::stage::open_epoch(&mut client, &storage, &headers, version).unwrap();
    let mut ids = Vec::new();
    for (n, name) in ["Tied.Name", "Tied.Name", "Unrelated.Name", "Tied.Name"].into_iter().enumerate() {
        let group = format!("alt.binaries.sort{n}");
        let release = Release {
            name: name.into(),
            group: group.clone(),
            size: 5,
            complete: true,
            poster: "poster".into(),
            date: "Wed, 01 Jan 2020 00:00:00 +0000".into(),
            articles: vec![Article {
                filename: Some("sample.bin".into()),
                subject: "sample.bin yEnc (1/1)".into(),
                message_id: format!("<sort{n}@example.test>"),
                part: Some(1),
                total_parts: Some(1),
                bytes: 5,
                ..Default::default()
            }],
            ..Default::default()
        };
        let saved = pg::release_writer::save(&mut client, &[release], 1).unwrap();
        assert_eq!((saved.releases, saved.files, saved.parts), (1, 1, 1));
        ids.push(client.query_one("select id from releases where group_name=$1", &[&group]).unwrap().get::<_, i64>(0));
    }
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
    let expected = [ids[3], ids[1], ids[0]];
    let directory = epoch.catalog().word_index_root(&headers).unwrap();
    let view = ReadView::open(
        &mut client,
        &schema.name,
        pg::fresh_reader::Budget { rows: 100, ..Default::default() },
        Instant::now() + Duration::from_secs(15),
    )
    .unwrap();
    let mut source = pg::fresh_words::Search::open(&view).unwrap();
    let stamp = source.stamp().unwrap();
    let writer = WordIndex::open_native(&directory, stamp).unwrap();
    assert_eq!(writer.build_native(&mut source, Budget::default(), || true).unwrap().documents, 4);
    drop(writer);
    let reader = WordIndex::read_native(&directory, stamp).unwrap();
    for descending in [true, false] {
        let page = reader
            .search_native(&mut source, "tied", None, &[], By::Size, descending, 0, 10, Budget::default())
            .unwrap();
        assert_eq!(page.total, 3);
        assert!(page.rows.iter().all(|row| row.size == Some(5)));
        assert_eq!(page.rows.iter().map(|row| row.id).collect::<Vec<_>>(), expected);
        let (rows, count) = view
            .sorted_word_search(
                &reader,
                &mut source,
                "tied",
                None,
                &[],
                None,
                0,
                10,
                SortBy::Name,
                descending,
                false,
                false,
                None,
            )
            .unwrap();
        assert_eq!(count, 3);
        assert!(rows.iter().all(|row| row.name == "Tied.Name"));
        assert_eq!(rows.iter().map(|row| row.id).collect::<Vec<_>>(), expected);
    }
    let (rows, total) =
        view.field_search(&Fields { size: "abc".into(), ..Default::default() }, "", None, 0, 10).unwrap();
    assert!(rows.is_empty());
    assert_eq!(total, 0);
}
