//! Index uploads obfuscated per article next to a real one: each is gathered into
//! one release and counted, the cursors pass them, the real upload stays.
//!
//! One test, and the environment is set before any thread starts.

mod common;

use atlas::indexer::{Indexer, Progress};
use atlas::nntp::BlockingPool;
use atlas::{api, db, search, store};
use common::{GROUP, Server, mock, post, spawn_server};

fn token(i: usize, len: usize) -> String {
    let mut x = (i as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let abc = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(0x100_0000_01b3) ^ (x >> 7);
            abc[(x % 62) as usize] as char
        })
        .collect()
}

#[test]
fn articles_obfuscated_per_article_are_gathered_and_counted() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
    }

    let mut subjects: Vec<String> = Vec::new();
    subjects.extend((2..42).map(|p| format!(r#"[41/62] "{}" yEnc ({p}/375)"#, token(p, 32))));
    subjects.extend((2..42).map(|p| format!(r#""{}" yEnc ({p}/5747)"#, token(p + 100, 24))));
    subjects.extend(
        (2..42).map(|p| format!(r#"[17/36] - "{}.{}" yEnc ({p}/59) 2088881050"#, token(p + 200, 8), token(p + 300, 8))),
    );
    subjects.extend((1..=30).map(|p| format!(r#"[1/1] - "Real.Upload.2026.mkv" yEnc ({p}/30)"#)));
    let last = subjects.len() as u64;
    let posts = subjects.iter().enumerate().map(|(i, s)| post(i as u64 + 1, s, 700_000, vec![])).collect();

    let server = Server::new(posts);
    let port = spawn_server(server.clone());
    db::create_db().unwrap();
    let conn = db::open().unwrap();

    let client = BlockingPool::new(&[mock(port, "secret", 4, 1)]);
    client.connect().unwrap();
    let mut indexer = Indexer::new(client, "backfill", db::open().unwrap());
    let mut seen = Progress::default();
    for _ in 0..50 {
        indexer
            .index_group_with(GROUP, &mut |p| {
                seen.articles += p.articles;
                seen.skipped += p.skipped;
            })
            .unwrap();
        if indexer.is_idle(GROUP) {
            break;
        }
    }
    assert!(indexer.is_idle(GROUP));

    assert_eq!(seen.articles, last as i64, "every article counts as received");
    assert_eq!(seen.skipped, 120, "all three shapes");
    let names: Vec<String> = conn
        .prepare("select name from releases")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(names.len(), 4, "the real upload and three gathered: {names:?}");
    assert!(names.contains(&"Real.Upload.2026.mkv".to_string()));
    let shard = db::open_shard(&store::shard_path(&home.path().join("atlas.db"), store::shard_of(GROUP))).unwrap();
    let (gathered, id): (String, i64) = shard
        .query_row("select name, id from releases where name like 'per-article-%' limit 1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    let found = search::search_all_releases(&gathered, 0, 10).unwrap();
    assert_eq!(found.first().map(|r| r.id), Some(id), "a gathered upload can be found");
    let get = |query: String| api::handle("/api", &format!("{query}&apikey=k"), "http://atlas:9090", "k");
    let whole = get(format!("t=get&id={id}"));
    assert_eq!(whole.status, 200);
    assert_eq!(whole.body.matches("<segment ").count(), 40, "{}", whole.body);
    let one = get(format!("t=get&id={id}&file=1"));
    assert_eq!((one.status, one.body.matches("<file ").count()), (200, 1));
    for missing in ["2", "0", "x"] {
        assert_eq!(get(format!("t=get&id={id}&file={missing}")).status, 404, "{missing}");
    }
    assert!(search::search_all_releases("real upload", 0, 10).unwrap()[0].complete);
    // the cursors went past them
    let (live, backfill): (i64, i64) = conn
        .query_row("select live_cursor, backfill_cursor from groups where name like ?", [format!("{GROUP}%")], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(live, last as i64);
    assert!(backfill < 1, "backfill reached the first article, cursor {backfill}");
}
