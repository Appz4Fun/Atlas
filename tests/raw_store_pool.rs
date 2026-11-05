//! The raw database's connections, many callers at once.

use atlas::raw_store::{ProbeRecord, get_probe, put_probe, release_content_id};

#[test]
fn many_threads_write_and_read_the_raw_store_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let shard = dir.path().join("atlas.s0.db");
    drop(atlas::db::create_at(&shard).unwrap());
    let threads: Vec<_> = (0..48)
        .map(|t| {
            let shard = shard.clone();
            std::thread::spawn(move || {
                // binds this thread's raw store to the shard's sibling
                let conn = atlas::db::create_at(&shard).unwrap();
                release_content_id(&conn, 1);
                for n in 0..20 {
                    let name = format!("Rel.{t}.{n}");
                    let info = format!("{{\"file\":\"{name}\"}}");
                    put_probe(&ProbeRecord {
                        name: &name,
                        group_name: "alt.binaries.test",
                        content_id: b"cid",
                        crc32: Some(i64::from(t)),
                        agent: None,
                        info: &info,
                        moviehash: None,
                        parts: 10,
                        complete: true,
                        config_stamp: 1,
                        nfo_password: None,
                        cover: None,
                    });
                    let saved = get_probe(&name, "alt.binaries.test", b"cid", 10, true, 1);
                    assert_eq!(saved.map(|s| s.info), Some(info), "{name}");
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert!(dir.path().join("atlas.raw.db").exists());
}
