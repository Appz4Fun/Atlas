//! The keys step, through the stage: a probed release gets its `release_keys` row, and releases posted from one
//! account find each other.

mod common;

use std::sync::Arc;

use atlas::db;
use atlas::keys::{self, KeysStep, Kind};
use atlas::postprocess::step::{Step, StepId, mask};
use atlas::probe::ProbeInfo;
use atlas::probe::media::MediaInfo;
use common::stage::*;

const BIT: i64 = mask(StepId(40));

fn id_of(h: &Harness, name: &str) -> i64 {
    h.shard().query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap()
}

fn probe(h: &Harness, id: i64, info: &ProbeInfo) {
    h.shard()
        .execute(
            "insert or replace into release_probe (release_id, info) values (?1, ?2)",
            rusqlite::params![id, serde_json::to_string(info).unwrap()],
        )
        .unwrap();
}

fn row(h: &Harness, id: i64) -> Option<(Option<i64>, Option<i64>, Option<i64>)> {
    h.shard()
        .query_row("select account, host, layout from release_keys where release_id = ?", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .ok()
}

fn done(h: &Harness, id: i64) -> i64 {
    h.shard().query_row("select done from pp_state where release_id = ?", [id], |r| r.get(0)).unwrap_or(0)
}

fn posted_by(account: &str) -> ProbeInfo {
    let mut info = ProbeInfo::default();
    info.headers.insert(
        "Injection-Info".into(),
        format!("news.example; posting-account=\"{account}\"; posting-host=\"198.51.100.7\""),
    );
    info.media = Some(MediaInfo { duration_secs: Some(5_400.4), ..MediaInfo::default() });
    info
}

#[test]
fn probed_releases_get_their_keys_and_find_each_other() {
    let mut h = fresh();
    let names = ["Keys.One.2024.1080p-GRP", "Keys.Two.2024.1080p-GRP", "Keys.Three.2024.1080p-GRP", "Keys.Bare-GRP"];
    h.save(&names.map(|n| release_of(n, "a.mkv", 2, 2)));
    let [one, two, three, bare] = names.map(|n| id_of(&h, n));
    probe(&h, one, &posted_by("uploader-1"));
    probe(&h, two, &posted_by("uploader-1"));
    probe(&h, three, &posted_by("uploader-2"));

    let step: Arc<dyn Step> = Arc::new(KeysStep::default());
    h.start(vec![step], quick_config());
    for id in [one, two, three, bare] {
        wait_for("the keys step to run", || done(&h, id) & BIT != 0);
    }
    h.stop();

    // the salt the pages use keys them: the step read the same one from shard 0
    let conn = db::open_with_shards(&h.main).unwrap();
    let salt = keys::salt(&conn).unwrap();
    let account = |a: &str| Some(keys::as_key(keys::salted8(salt, a)));
    let host = Some(keys::as_key(keys::salted8(salt, "198.51.100.7")));
    let layout = Some(keys::as_key(keys::digest8(b"v:|a:|s:|c:0|d:5400")));
    assert_eq!(row(&h, one), Some((account("uploader-1"), host, layout)));
    assert_eq!(row(&h, three), Some((account("uploader-2"), host, layout)));
    assert_eq!(row(&h, bare), None, "no probe and no timed lines: no row");

    let shard = h.shard();
    let lookup = keys::read_lookup(&shard, one, Some(2), Some(salt)).unwrap();
    assert_eq!(keys::count_others(&conn, one, &lookup, Kind::Account), 1);
    assert_eq!(keys::others(&conn, one, &lookup, Kind::Account, 10), vec![two]);
    assert_eq!(keys::count_others(&conn, one, &lookup, Kind::Host), 2);
    let mut layout_mates = keys::others(&conn, one, &lookup, Kind::Layout, 10);
    layout_mates.sort_unstable();
    let mut expected = vec![two, three];
    expected.sort_unstable();
    assert_eq!(layout_mates, expected);
    assert_eq!(keys::count_others(&conn, one, &lookup, Kind::Lines), 0, "no lines key, nothing to count");
    assert_eq!(keys::others(&conn, one, &lookup, Kind::Account, 0), Vec::<i64>::new());
}
