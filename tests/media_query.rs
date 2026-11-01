//! ID searches: the caps on what a query reads, and the indexes it reads them from.

mod common;

use atlas::category::parse_filter;
use atlas::db;
use atlas::media::query::{self, IdColumn, IdQuery, find_bounded, find_with};
use atlas::media::title::Kind;
use atlas::parser::Release;
use atlas::search::Bounds;
use common::stage::*;
use rusqlite::{Connection, params};

/// `n` episodes of a show in one shard, season 1, oldest first.
fn with_rows(h: &Harness, n: i64) -> Vec<i64> {
    let names: Vec<String> = (1..=n).map(|e| format!("Some.Show.S01E{e:02}.720p-GRP")).collect();
    let releases: Vec<Release> = names.iter().map(|n| complete(n)).collect();
    h.save(&releases);
    let shard = h.shard();
    let mut ids = Vec::new();
    for (e, name) in names.iter().enumerate() {
        let id = h.id_of(name);
        shard
            .execute(
                "insert into media_release (release_id, kind, title_key, season, episode, quality, tvdb_id)
                 values (?, 1, 5, 1, ?, 0, 3001)",
                params![id, e as i64 + 1],
            )
            .unwrap();
        ids.push(id);
    }
    ids.sort_unstable();
    ids
}

fn conn(h: &Harness) -> Connection {
    db::open_with_shards(&h.main).unwrap()
}

fn by_id(season: Option<i64>, episode: Option<i64>) -> IdQuery {
    IdQuery { kind: Kind::Tv, ids: vec![(IdColumn::Tvdb, 3001)], season, episode }
}

#[test]
fn a_big_id_keeps_its_newest_ids() {
    let h = fresh();
    let ids = with_rows(&h, 50);
    let newest: Vec<i64> = ids[45..].iter().rev().copied().collect();
    let c = conn(&h);
    for q in [by_id(None, None), by_id(Some(1), None)] {
        let page = find_bounded(&c, &q, None, 0, 100, &Bounds::DEFAULT, 100, 5).unwrap();
        assert_eq!(page.rows.iter().map(|r| r.id).collect::<Vec<_>>(), newest, "{q:?}");
    }
    let page = find_bounded(&c, &by_id(None, None), None, 0, 100, &Bounds::DEFAULT, 5, 200_000).unwrap();
    assert_eq!(page.rows.iter().map(|r| r.id).collect::<Vec<_>>(), newest);
    // an episode within the newest index rows is found, and an older one past the cap isn't
    let page = find_bounded(&c, &by_id(Some(1), Some(1)), None, 0, 100, &Bounds::DEFAULT, 100, 200_000).unwrap();
    assert_eq!(page.rows.len(), 1);
    let page = find_bounded(&c, &by_id(Some(1), Some(1)), None, 0, 100, &Bounds::DEFAULT, 100, 5).unwrap();
    assert_eq!(page.rows.len(), 0);
}

#[test]
fn the_id_queries_use_their_covering_index() {
    let h = fresh();
    let c = conn(&h);
    let plan = |sql: String, n: usize| -> String {
        let args: Vec<i64> = vec![1; n];
        let mut stmt = c.prepare(&format!("explain query plan {sql}")).unwrap();
        stmt.query_map(rusqlite::params_from_iter(args), |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>()
            .join("\n")
    };
    for column in ["tvdb_id", "tvmaze_id", "trakt_id", "tmdb_id", "imdb_id", "tvrage_id"] {
        for text in [
            plan(query::episode_sql(0, column), 6),
            plan(query::season_sql(0, column), 5),
            plan(query::id_sql(0, column), 4),
        ] {
            assert!(text.contains("USING COVERING INDEX"), "{column}: {text}");
            assert!(!text.contains("TEMP B-TREE"), "{column}: {text}");
        }
    }
}

#[test]
fn find_with_is_bounded() {
    let h = fresh();
    with_rows(&h, 60);
    let filter = parse_filter("2040").unwrap();
    let c = conn(&h);
    h.shard().execute("insert into release_category select release_id, 5040 from media_release", []).unwrap();
    let bounds = Bounds { rejects: 10, ..Bounds::DEFAULT };
    let page = find_with(&c, &by_id(None, None), Some(&filter), 0, 10, &bounds, 1_000).unwrap();
    assert_eq!((page.rows.len(), page.scanned), (0, 11));
}

#[test]
fn id_rows_are_capped_per_shard() {
    let h = fresh();
    with_rows(&h, 50);
    let page = find_with(&conn(&h), &by_id(None, None), None, 0, 100, &Bounds::DEFAULT, 5).unwrap();
    assert!(page.scanned <= 5, "{}", page.scanned);
}

#[test]
fn an_offset_past_the_cap_finds_nothing() {
    let h = fresh();
    with_rows(&h, 2);
    let page = find_with(&conn(&h), &by_id(None, None), None, 100_000, 10, &Bounds::DEFAULT, 100).unwrap();
    assert!(page.rows.is_empty());
}
