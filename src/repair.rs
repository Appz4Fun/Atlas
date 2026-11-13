//! Parts a server's listing left out, read again from another server. A provider's overview can miss articles it
//! stores: a Nyuu post in `alt.binaries.sleazemovies` came back without 50 of its parts from the one server that
//! indexes the group, while another server listed every one. So a big post that is nearly whole has the time window
//! it was posted in read again, in each of its groups, from servers that don't index that group, and what turns up
//! is saved like any other header: the missing parts join their rows by name. Nothing here is about one release.

use std::collections::HashSet;
use std::time::Duration;

// The original-owner overview service is supplied by the selected native core.
pub(crate) mod native;

/// how often a round looks for posts to repair
const EVERY: Duration = Duration::from_secs(600);
/// what is worth the reads: media, not small posts. A post's rows can each be a part of it, soo a row needs less
const MIN_BYTES: i64 = 1 << 30;
const MIN_ROW_BYTES: i64 = 64 << 20;
/// nearly whole: a post with much more missing lacks whole groups, which another server's copy of these won't add
const MIN_PERMILLE: i64 = 900;
/// posts younger than this may still be arriving
const SETTLED_SECS: i64 = 2 * 3600;
/// and only this recent: older ones were looked at, or are past what the reads are worth
const DAYS: i64 = 3;
/// rows looked at per shard per round, newest first
const CANDIDATES: i64 = 200;
/// windows read per round
const PER_ROUND: usize = 20;
/// around a post's first and last article
const MARGIN: i64 = 300;
/// a post's span past which its days aren't asked for (see `days_of`)
const MAX_SPAN_DAYS: i64 = 2;
/// articles below a group's last number two servers are compared over, to tell whether they are one backend
const SAME_IDS_LOOK: u64 = 50;
/// a window bigger than this is a busy group's hours, not a post's minutes
const MAX_ARTICLES: u64 = 3_000_000;

/// rows of each shard an older sweep reads through per round, newest id first: past `DAYS` the reads go on, in what
/// the recent posts leave of a round, through posts nobody looked at, the backfill's mostly
const OLDER_SCAN: i64 = 50_000;
/// of them, the posts looked at per shard per round
const OLDER_CANDIDATES: i64 = 100;

/// One group's window to read again, for the rows of one post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub group: String,
    pub from: i64,
    pub to: i64,
}

/// A post looked at: its rows, its share then, the windows it wants read (none: nothing to do for it), and the days of
/// its groups' backfill to take first. `media`: its name says what the indexer is after (`is_media`), `size`: of all
/// its rows.
#[derive(Debug, Default)]
pub struct Looked {
    pub ids: Vec<i64>,
    pub permille: i64,
    pub windows: Vec<Window>,
    pub days: Vec<(String, i64)>,
    pub media: bool,
    pub size: i64,
}

/// What a round found, post by post: the recent ones first, by worth, then each shard's older ones, newest first.
#[derive(Debug, Default)]
pub struct Found {
    pub posts: Vec<Looked>,
    pub older: Vec<Older>,
}

/// A shard's older sweep this round: its posts (`posts[first..first + keys.len()]`), each found by the row in `keys`,
/// and where the sweep goes on once all of them are done (`None`: from the newest row again, the shard swept).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Older {
    pub shard: usize,
    pub first: usize,
    pub keys: Vec<i64>,
    pub end: Option<i64>,
}

impl Older {
    /// Where the sweep goes on, given the posts a round read or looked at: past every post up to the first it left for
    /// later, that one found again next round.
    fn next(&self, done: &HashSet<usize>) -> Option<i64> {
        match (0..self.keys.len()).find(|j| !done.contains(&(self.first + j))) {
            Some(j) => Some(self.keys[j] + 1),
            None => self.end,
        }
    }
}

impl Found {
    pub fn windows(&self) -> impl Iterator<Item = &Window> {
        self.posts.iter().flat_map(|p| p.windows.iter())
    }
}

/// `mark`'s writes, in one transaction.
fn mark_in(
    db: &dyn crate::pg::sql::Sql,
    now: i64,
    days: &[(String, i64)],
    tried: &[(i64, i64)],
    older: &[(usize, Option<i64>)],
) -> crate::pg::sql::Result<()> {
    use crate::pg::sql::vals;
    db.tx(|tx| {
        for (group, day) in days {
            crate::chunks::want(tx, group, *day, now)?;
        }
        for (id, permille) in tried {
            tx.execute(
                "insert into repair_tried (release_id, at, permille) values ($1, $2, $3)
                 on conflict (release_id) do update set at = excluded.at, permille = excluded.permille",
                vals![*id, now, *permille],
            )?;
        }
        for (shard, before) in older {
            tx.execute(
                "insert into repair_older (shard, before) values ($1, $2)
                 on conflict (shard) do update set before = excluded.before",
                vals![*shard as i64, *before],
            )?;
        }
        Ok(())
    })
}

/// The machine behind a server's name: host and port, without an explicit `#key`, in lower case. Two accounts at one
/// provider have the same.
fn machine(host: &str) -> String {
    host.split('#').next().unwrap_or(host).to_lowercase()
}

/// Whether two servers' GROUP numbers, (first, last), may be one backend's: last ones a moment of posting apart. Two
/// backends number a group on their own, millions apart. The first can differ: resellers of one backend each keep
/// it as long as their retention, so it takes `same_ids` to say.
fn same_spool(a: (u64, u64), b: (u64, u64)) -> bool {
    a.1.abs_diff(b.1) <= (a.1.max(b.1) / 100_000).max(1_000)
}

/// At least one number with the same Message-ID on both, and none with two different ones.
fn ids_agree(a: &[(u64, String)], b: &[(u64, String)]) -> bool {
    let a: std::collections::HashMap<u64, &str> = a.iter().map(|(n, id)| (*n, id.as_str())).collect();
    let mut same = 0;
    for (n, id) in b {
        match a.get(n) {
            Some(other) if *other == id => same += 1,
            Some(_) => return false,
            None => {}
        }
    }
    same > 0
}

/// The servers that index `group` already, by its cursors (`group@server`): reading it there again finds the same.
fn indexed_from(db: &dyn crate::pg::sql::Sql, group: &str) -> crate::pg::sql::Result<HashSet<String>> {
    let prefix = format!("{group}@");
    let names: Vec<String> = db.column(
        "select name from groups where name >= $1 and name < $2",
        crate::pg::sql::vals![&prefix, format!("{group}A")],
    )?;
    Ok(names.into_iter().filter_map(|n| n.strip_prefix(&prefix).map(str::to_string)).collect())
}

/// What a round does with `posts` (in their order): the ones it reads, within `per_round` windows (the first that
/// fit; once one doesn't, the rest wait for the next round, though one alone bigger than that still goes), and the ones
/// with nothing to read, marked looked at whatever the share: they cost nothing, and left unmarked they would be found
/// first again every round.
fn plan(posts: &[Looked], per_round: usize) -> (Vec<usize>, Vec<usize>) {
    let (mut read, mut full) = (0usize, false);
    let (mut to_read, mut looked) = (Vec::new(), Vec::new());
    for (i, post) in posts.iter().enumerate() {
        if post.windows.is_empty() {
            looked.push(i);
            continue;
        }
        if full || (read > 0 && read + post.windows.len() > per_round) {
            full = true;
            continue;
        }
        read += post.windows.len();
        to_read.push(i);
    }
    (to_read, looked)
}

/// Whether a release's name says what the indexer is after most: 2160p, a remux or Dolby Vision (like `media_counts`).
fn is_media(name: &str) -> bool {
    let name = name.to_lowercase();
    ["2160p", "remux", ".dv.", "dovi"].iter().any(|w| name.contains(w))
}

/// What the indexer is after first, then the biggest, from every shard alike: in shard order, a round's share went to
/// shard 0's posts, and the other shards' waited.
fn by_worth(posts: &mut [Looked]) {
    posts.sort_by_key(|p| (!p.media, std::cmp::Reverse(p.size)));
}

/// The days the whole post spans, from its first article in any group to its last, in each of its groups. None for
/// one spread over more than `MAX_SPAN_DAYS`: a post takes hours, and a date that far off is wrong.
fn days_of(windows: &[Window]) -> Vec<(String, i64)> {
    let (Some(from), Some(to)) = (windows.iter().map(|w| w.from).min(), windows.iter().map(|w| w.to).max()) else {
        return Vec::new();
    };
    if to - from > MAX_SPAN_DAYS * 86_400 {
        return Vec::new();
    }
    let days = crate::chunks::unix_day(from)..=crate::chunks::unix_day(to);
    windows.iter().flat_map(|w| days.clone().map(move |d| (w.group.clone(), d))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real migrated PostgreSQL round preserves its marks, cursors and atomic failure semantics.
    #[test]
    #[ignore = "requires explicit isolated PostgreSQL for native repair marks and cursors"]
    fn native_marks_group_cursors_and_failed_batch_keep_exact_postgres_facts() {
        use crate::db;
        use crate::pg::sql::vals;
        let schema = crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
        let mut client = schema.pool.get().unwrap();
        let pg = crate::pg::sql::Pg::new(&mut client);
        let c: &dyn crate::pg::sql::Sql = &pg;
        // cursors: made, moved one at a time, and read back
        assert_eq!(db::get_group_state(c, "g@a").unwrap(), None);
        db::init_group_state(c, "g@a", 100).unwrap();
        db::update_live_cursor(c, "g@a", 150).unwrap();
        db::update_backfill_cursor(c, "g@a", 50).unwrap();
        db::update_live_cursor(c, "g@b", 7).unwrap();
        db::save_group_state(c, "g2@a", db::GroupState { live_cursor: 3, backfill_cursor: 1 }).unwrap();
        assert_eq!(
            db::get_group_state(c, "g@a").unwrap(),
            Some(db::GroupState { live_cursor: 150, backfill_cursor: 50 })
        );
        db::save_group_bounds(c, "g@a", 10, 200).unwrap();
        // each end widens alone
        db::save_group_dates(c, "g@a", (60, 6000), (140, 14_000)).unwrap();
        db::save_group_dates(c, "g@a", (70, 7000), (145, 14_500)).unwrap();
        db::save_group_dates(c, "g@b", (1, 100), (7, 700)).unwrap();
        db::set_start_posted(c, "g@b", 500).unwrap();
        let rows = db::group_progress(c).unwrap();
        let a = rows.iter().find(|r| r.key == "g@a").unwrap();
        assert_eq!((a.first, a.last, a.low, a.high), (Some(10), Some(200), Some((60, 6000)), Some((145, 14_500))));
        // read through: g@a's newest saved slice is behind its live cursor, g@b's too; g2 isn't g's
        assert_eq!(crate::indexer::read_through(c, "g", "g@a").unwrap(), (Some(700), Some(14_500)));
        assert_eq!(crate::indexer::read_through_since(c, "g", "g@a", 400).unwrap(), None, "b began after");
        assert_eq!(crate::indexer::read_through_since(c, "g", "g@a", 500).unwrap(), Some(700));
        let mut from: Vec<String> = indexed_from(c, "g").unwrap().into_iter().collect();
        from.sort();
        assert_eq!(from, ["a", "b"]);

        // a round's marks: tried releases, the older sweeps, and the days asked for
        crate::chunks::add(c, "g", 20_000, 19_990).unwrap();
        mark_in(c, 1000, &[("g".into(), 19_995)], &[(5, 900), (6, 100)], &[(0, Some(42)), (1, None)]).unwrap();
        mark_in(c, 2000, &[], &[(5, 950)], &[(0, Some(41))]).unwrap();
        let tried: Vec<(i64, i64, i64)> = c
            .query("select release_id, at, permille from repair_tried order by release_id", vals![])
            .unwrap()
            .iter()
            .map(|r| (r.get(0).unwrap(), r.get(1).unwrap(), r.get(2).unwrap()))
            .collect();
        assert_eq!(tried, [(5, 2000, 950), (6, 1000, 100)]);
        let older: Vec<Option<i64>> = c.column("select before from repair_older order by shard", vals![]).unwrap();
        assert_eq!(older, [Some(41), None]);
        let k = crate::chunks::claim(c, &[("g".into(), i64::MIN)], "s", 3000).unwrap().unwrap();
        assert_eq!(k.day, 19_995, "the day asked for first");
        // The actual mark transaction must roll back its earlier wanted-day
        // write when a later tried row is refused, retaining the driver cause.
        c.batch("alter table repair_tried add constraint fixture_nonnegative_share check (permille >= 0) not valid")
            .unwrap();
        let error = mark_in(c, 4000, &[("g".into(), 19_996)], &[(7, -1)], &[(0, Some(12))]).unwrap_err();
        assert_eq!(error.postgres().unwrap().code(), Some(&postgres::error::SqlState::CHECK_VIOLATION));
        assert_eq!(c.one::<i64>("select count(*) from repair_tried where release_id=7", vals![]).unwrap(), 0);
        assert_eq!(
            c.one::<i64>("select count(*) from backfill_wanted where grp='g' and day=19996", vals![]).unwrap(),
            0
        );
        assert_eq!(
            c.opt::<Option<i64>>("select before from repair_older where shard=0", vals![]).unwrap(),
            Some(Some(41))
        );
    }

    fn looked(windows: usize, media: bool, size: i64) -> Looked {
        let window = Window { group: "g".into(), from: 0, to: 1 };
        Looked { ids: vec![size], windows: vec![window; windows], media, size, ..Default::default() }
    }

    #[test]
    fn a_round_reads_what_fits_and_marks_what_has_nothing_to_read() {
        let posts =
            [looked(3, true, 9), looked(0, false, 8), looked(2, true, 7), looked(1, false, 6), looked(0, true, 5)];
        // 3 + 2 fit in 5; the next doesn't, and nothing after it is read, but the empty ones are all marked
        assert_eq!(plan(&posts, 5), (vec![0, 2], vec![1, 4]));
        // one bigger than the share on its own still goes
        assert_eq!(plan(&[looked(8, true, 1), looked(1, true, 1)], 5), (vec![0], vec![]));
    }

    #[test]
    fn an_older_sweep_goes_on_past_what_a_round_did_and_finds_the_first_it_left_again() {
        let o = Older { shard: 3, first: 4, keys: vec![90, 70, 50], end: Some(40) };
        let done = |v: &[usize]| v.iter().copied().collect::<HashSet<usize>>();
        assert_eq!(o.next(&done(&[0, 1, 4, 5, 6])), Some(40), "all three done");
        assert_eq!(o.next(&done(&[4, 6])), Some(71), "the second left for later: found again");
        assert_eq!(o.next(&done(&[])), Some(91));
        let swept = Older { shard: 3, first: 0, keys: vec![], end: None };
        assert_eq!(swept.next(&done(&[])), None, "nothing left: from the newest again");
    }

    #[test]
    fn media_names_are_what_a_round_reads_first() {
        assert!(is_media("Resident.Evil.2026.DV.2160p.WEB.H265-SLOT"));
        assert!(is_media("Film.2007.1080p.BluRay.REMUX.AVC-X"));
        assert!(is_media("Show.S01E01.DoVi.1080p-Y"));
        assert!(!is_media("Show.S01E01.1080p.WEB-DL-Z"));
        assert!(!is_media("DVDRip.Collection"));
        let mut posts = vec![looked(1, false, 90), looked(1, true, 10), looked(1, true, 50), looked(1, false, 99)];
        by_worth(&mut posts);
        assert_eq!(posts.iter().map(|p| p.size).collect::<Vec<_>>(), [50, 10, 99, 90]);
    }

    #[test]
    fn accounts_on_one_provider_are_one_machine() {
        assert_eq!(machine("news.newshosting.com"), "news.newshosting.com");
        assert_eq!(machine("News.Newshosting.com#second"), "news.newshosting.com");
        assert_ne!(machine("127.0.0.1:1190"), machine("127.0.0.1:1191"), "a port is another server");
    }

    #[test]
    #[ignore = "requires explicit isolated PostgreSQL for the actual native daily media reducer"]
    fn big_video_releases_are_counted_by_the_actual_native_media_service() {
        let schema = crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
        let root = tempfile::tempdir().unwrap();
        let mut config = crate::config::Config::default();
        config.postprocess.enabled = false;
        config.predb.enabled = false;
        config.spots.enabled = false;
        config.inferred_parts = false;
        config.media.tvmaze.enabled = false;
        config.media.allocine.enabled = false;
        config.media.details_per_minute = 0;
        let headers = crate::config::HeaderStoreConfig { enabled: true, dir: root.path().to_str().unwrap().into() };
        let storage = crate::pg::StorageConfig {
            backend: crate::pg::Backend::Postgres,
            url: schema.url.clone(),
            schema: schema.name.clone(),
            max_connections: 1,
            allow_plaintext: true,
        };
        let runtime =
            crate::pg::indexer_runtime::Runtime::open(&config, storage, headers.clone(), &[], &mut None).unwrap();
        let big = (4i64 << 30) + 1;
        let release = |name: &str, group: &str, bytes: i64| crate::parser::Release {
            name: name.into(),
            group: group.into(),
            articles: vec![crate::parser::Article {
                message_id: format!("<{name}@x>"),
                filename: Some(format!("{name}.mkv")),
                part: Some(1),
                total_parts: Some(1),
                bytes,
                ..Default::default()
            }],
            ..Default::default()
        };
        let input = [
            release("Film.2020.2160p.WEB-DL-X", "alt.binaries.a", big),
            release("Film.2021.1080p.BluRay.REMUX-Y", "alt.binaries.a", big),
            release("Film.2022.2160p.WEB-DL-Z", "alt.binaries.a", (4i64 << 30) - 1),
            release("Show.S01E01.DV.HDR.2160p-W", "alt.binaries.b", big),
            release("Album.2020.FLAC-V", "alt.binaries.b", big),
        ];
        runtime
            .owner
            .with(|client| {
                crate::pg::release_writer::save(client, &input, 1)?;
                Ok(())
            })
            .unwrap();
        let mut service = crate::pg::media_counts::Service::new(&runtime.owner, runtime.db.clone(), &headers).unwrap();
        assert!(matches!(service.tick().unwrap(), crate::pg::media_counts::Tick::Stored));
        runtime
            .owner
            .with(|client| {
                let got = client
                    .query("select grp,releases from backfill_media order by grp", &[])?
                    .into_iter()
                    .map(|row| (row.get::<_, String>(0), row.get::<_, i64>(1)))
                    .collect::<Vec<_>>();
                assert_eq!(got, [("alt.binaries.a".to_string(), 2), ("alt.binaries.b".to_string(), 1)]);
                Ok(())
            })
            .unwrap();
        runtime.owner.close();
    }

    #[test]
    fn a_post_wants_the_days_it_spans_in_each_of_its_groups() {
        let w = |g: &str, from: i64, to: i64| Window { group: g.into(), from, to };
        // posted across midnight UTC in one group, before it in the other
        let day = 20_759 * 86_400;
        let days = days_of(&[w("a", day - 600, day - 100), w("b", day - 300, day + 200)]);
        let want = |g: &str, d: i64| (g.to_string(), d);
        assert_eq!(days, [want("a", 20_758), want("a", 20_759), want("b", 20_758), want("b", 20_759)]);
        assert!(days_of(&[]).is_empty());
        assert!(days_of(&[w("a", day, day + 600), w("b", day, 99_999 * 86_400)]).is_empty(), "a date far off");
    }

    #[test]
    fn resellers_of_one_backend_are_one_spool() {
        // alt.binaries.newznzb.sierra on eweka and newshosting a few seconds apart, and on xsnews
        let (eweka, newshosting, xsnews) = ((2, 1_010_645_689), (2, 1_010_645_691), (619_650_000, 1_019_663_750));
        assert!(same_spool(eweka, newshosting));
        assert!(!same_spool(eweka, xsnews));
        assert!(!same_spool((1, 5_000_000), (1, 9_000_000)), "two backends that both start at 1");
        // alt.binaries.german.movies on eweka and newshosting: their retention differs, the numbers don't
        assert!(same_spool((1_153_539_032, 1_153_563_359), (1_153_540_197, 1_153_563_359)));
        assert!(same_spool((1, 900), (1, 1_500)), "a small group");
    }

    #[test]
    fn one_backend_lists_the_same_ids_under_the_same_numbers() {
        let ids = |v: &[(u64, &str)]| v.iter().map(|(n, s)| (*n, s.to_string())).collect::<Vec<_>>();
        let a = ids(&[(10, "<a@x>"), (11, "<b@x>"), (12, "<c@x>")]);
        assert!(ids_agree(&a, &ids(&[(11, "<b@x>"), (12, "<c@x>"), (13, "<d@x>")])));
        assert!(!ids_agree(&a, &ids(&[(11, "<b@x>"), (12, "<z@x>")])), "a number with another article");
        assert!(!ids_agree(&a, &ids(&[(20, "<b@x>")])), "nothing to compare");
        assert!(!ids_agree(&a, &[]));
    }
}
