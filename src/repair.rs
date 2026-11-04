//! Parts a server's listing left out, read again from another server. A provider's overview can miss articles it
//! stores: a Nyuu post in `alt.binaries.sleazemovies` came back without 50 of its parts from the one server that
//! indexes the group, while another server listed every one. So a big post that is nearly whole has the time window
//! it was posted in read again, in each of its groups, from servers that don't index that group, and what turns up
//! is saved like any other header: the missing parts join their rows by name. Nothing here is about one release.

use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use rusqlite::{Connection, params};

use crate::db;
use crate::indexer::{Db, PassContext, PassSettings, on_db, reread_window};
use crate::search::ReleaseRow;
use crate::store;

/// how often the big releases of each group are counted again (`count_media`)
const COUNT_EVERY: Duration = Duration::from_secs(24 * 3600);
/// a release this big is what the indexer is after, when its name says so (`media_counts`)
const MEDIA_BYTES: i64 = 4 << 30;
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
/// servers asked per window
const SERVERS: usize = 2;
/// around a post's first and last article
const MARGIN: i64 = 300;
/// a post's span past which its days aren't asked for (see `days_of`)
const MAX_SPAN_DAYS: i64 = 2;
/// articles below a group's last number two servers are compared over, to tell whether they are one backend
const SAME_IDS_LOOK: u64 = 50;
/// a window bigger than this is a busy group's hours, not a post's minutes
const MAX_ARTICLES: u64 = 3_000_000;

const CREATE: &str =
    "create table if not exists repair_tried (release_id INTEGER PRIMARY KEY, at INTEGER NOT NULL, permille INTEGER)";

/// One group's window to read again, for the rows of one post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub group: String,
    pub from: i64,
    pub to: i64,
}

/// A post looked at: its rows, its share then, the windows it wants read (none: nothing to do for it), and the days of
/// its groups' backfill to take first.
#[derive(Debug, Default)]
pub struct Looked {
    pub ids: Vec<i64>,
    pub permille: i64,
    pub windows: Vec<Window>,
    pub days: Vec<(String, i64)>,
}

/// What a round found, post by post.
#[derive(Debug, Default)]
pub struct Found {
    pub posts: Vec<Looked>,
}

impl Found {
    pub fn windows(&self) -> impl Iterator<Item = &Window> {
        self.posts.iter().flat_map(|p| p.windows.iter())
    }
}

pub async fn run(ctx: PassContext, settings: PassSettings, db: Db, stopping: impl Fn() -> bool) {
    if let Err(e) = on_db(&db, |conn| Ok(conn.execute_batch(CREATE)?)).await {
        eprintln!("repair: {e:#}");
        return;
    }
    loop {
        let deadline = Instant::now() + EVERY;
        while !stopping() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        if stopping() {
            return;
        }
        if let Err(e) = round(&ctx, &settings, &db, &stopping).await {
            eprintln!("repair: {e:#}");
        }
    }
}

async fn round(
    ctx: &PassContext,
    settings: &PassSettings,
    db: &Db,
    stopping: &impl Fn() -> bool,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();
    let found = tokio::task::spawn_blocking(move || db::with_reader(|conn| find(conn, now))).await??;
    let pool = &ctx.pool;
    // a post is marked once all its windows were read; the ones past this round's share wait for the next
    let (mut read, mut tried) = (0usize, Vec::new());
    for post in &found.posts {
        if stopping() || (read > 0 && read + post.windows.len() > PER_ROUND) {
            break;
        }
        for w in &post.windows {
            let indexing: HashSet<String> = {
                let g = w.group.clone();
                on_db(db, move |conn| Ok(indexed_from(conn, &g)?)).await?
            };
            // one provider's accounts share its overview: the group's own, and each one read, rule out their machine
            let mut seen: HashSet<String> = indexing.iter().map(|h| machine(h)).collect();
            // and so do two providers that resell one backend (eweka and newshosting), told apart by their numbers
            let mut spools = Vec::new();
            for server in pool.indexing_servers() {
                if indexing.iter().any(|h| machine(h) == machine(&pool.host(server)))
                    && let Ok((_, first, last, _)) = pool.group_on(server, &w.group).await
                {
                    spools.push((server, (first, last)));
                }
            }
            let mut read_from = 0;
            for server in pool.indexing_servers() {
                let host = pool.host(server);
                if read_from >= SERVERS || !seen.insert(machine(&host)) {
                    continue;
                }
                let spool = match pool.group_on(server, &w.group).await {
                    Ok((_, first, last, _)) => (first, last),
                    Err(e) => {
                        println!("[repair] {} on {host}: {e:#}", w.group);
                        continue;
                    }
                };
                let mut shared = false;
                for &(other, numbers) in &spools {
                    if same_spool(numbers, spool)
                        && same_ids(pool, other, server, &w.group, numbers.1.min(spool.1)).await
                    {
                        shared = true;
                        break;
                    }
                }
                if shared {
                    continue;
                }
                match reread_window(ctx, settings, db, &w.group, server, (w.from, w.to), MAX_ARTICLES).await {
                    Ok(p) => {
                        read_from += 1;
                        spools.push((server, spool));
                        println!("[repair] {} {}..{} on {host}: {} headers", w.group, w.from, w.to, p.articles);
                    }
                    // a failure leaves its share to the next server
                    Err(e) => println!("[repair] {} {}..{} on {host}: {e:#}", w.group, w.from, w.to),
                }
            }
        }
        read += post.windows.len();
        tried.extend(post.ids.iter().map(|id| (*id, post.permille)));
    }
    let days: Vec<(String, i64)> = found.posts.iter().flat_map(|p| p.days.iter().cloned()).collect();
    on_db(db, move |conn| {
        let tx = conn.transaction()?;
        for (group, day) in &days {
            crate::chunks::want(&tx, group, *day, now)?;
        }
        {
            let mut mark = tx
                .prepare_cached("insert or replace into repair_tried (release_id, at, permille) values (?1, ?2, ?3)")?;
            for (id, permille) in &tried {
                mark.execute(params![id, now, permille])?;
            }
        }
        tx.commit()?;
        Ok(())
    })
    .await
}

/// Counts each group's big video releases now and then, for the backfill to take those groups first
/// (`chunks::MEDIA_GROUPS`). With one server or many: it doesn't read from any.
pub async fn count_media_every(db: Db, stopping: impl Fn() -> bool) {
    while !stopping() {
        if let Err(e) = count_media(&db).await {
            eprintln!("counting big releases per group: {e:#}");
        }
        let next = Instant::now() + COUNT_EVERY;
        while !stopping() && Instant::now() < next {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

/// Notes how many big video releases each group holds.
async fn count_media(db: &Db) -> anyhow::Result<()> {
    let counts = tokio::task::spawn_blocking(|| db::with_reader(media_counts)).await??;
    on_db(db, move |conn| Ok(crate::chunks::set_media(conn, &counts)?)).await
}

/// Each group's releases of `MEDIA_BYTES` or more whose name says 2160p, remux or Dolby Vision.
pub fn media_counts(conn: &Connection) -> rusqlite::Result<Vec<(String, i64)>> {
    let mut counts = Vec::new();
    for s in 0..store::SHARDS {
        let sql = format!(
            "select r.group_name, count(*) from s{s}.releases r
             where r.size >= ?1 and r.group_name is not null and (
                 lower(coalesce(r.display_name, r.name)) like '%2160p%'
                 or lower(coalesce(r.display_name, r.name)) like '%remux%'
                 or lower(coalesce(r.display_name, r.name)) like '%.dv.%'
                 or lower(coalesce(r.display_name, r.name)) like '%dovi%')
             group by r.group_name"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([MEDIA_BYTES], |r| Ok((r.get(0)?, r.get(1)?)))?;
        counts.extend(rows.collect::<rusqlite::Result<Vec<(String, i64)>>>()?);
    }
    Ok(counts)
}

/// The machine behind a server's name: host and port, without an explicit `#key`, in lower case. Two accounts at one
/// provider have the same.
fn machine(host: &str) -> String {
    host.split('#').next().unwrap_or(host).to_lowercase()
}

/// Whether two servers' GROUP numbers, (first, last), are one backend's: the same first article, and last ones a
/// moment of posting apart. Two backends number a group on their own, millions apart.
fn same_spool(a: (u64, u64), b: (u64, u64)) -> bool {
    a.0 == b.0 && a.1.abs_diff(b.1) <= (a.1.max(b.1) / 100_000).max(1_000)
}

/// Whether servers `a` and `b` list the same Message-IDs under the same numbers just below `last`: GROUP numbers
/// alike only say two backends may be one. False when either can't say.
async fn same_ids(pool: &crate::nntp::Pool, a: usize, b: usize, group: &str, last: u64) -> bool {
    let start = last.saturating_sub(SAME_IDS_LOOK);
    let (Ok(ours), Ok(theirs)) = (pool.ids_on(a, group, start, last).await, pool.ids_on(b, group, start, last).await)
    else {
        return false;
    };
    ids_agree(&ours, &theirs)
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
fn indexed_from(conn: &Connection, group: &str) -> rusqlite::Result<HashSet<String>> {
    let prefix = format!("{group}@");
    let names: Vec<String> = conn
        .prepare("select name from groups where name >= ?1 and name < ?2")?
        .query_map(params![prefix, format!("{group}A")], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(names.into_iter().filter_map(|n| n.strip_prefix(&prefix).map(str::to_string)).collect())
}

/// The posts worth a second read, on a reader with every shard attached and `repair_tried` in main. A post is all
/// its rows (`release_info::whole_post_with`), and each of its groups gets one window around its rows' articles.
pub fn find(conn: &Connection, now: i64) -> rusqlite::Result<Found> {
    let has_table = conn.prepare("select 1 from main.sqlite_master where name = 'repair_tried'")?.exists([])?;
    if !has_table {
        return Ok(Found::default());
    }
    let mut found = Found::default();
    let mut seen: HashSet<i64> = HashSet::new();
    for s in 0..store::SHARDS {
        let sql = format!(
            "select r.id, coalesce(r.display_name, r.name), r.group_name, r.poster, r.posted_date, r.size, r.complete,
                    r.parts
             from s{s}.releases r
             where r.complete = 0 and r.size >= ?1
               and r.posted_date >= datetime(?2, 'unixepoch') and r.posted_date < datetime(?3, 'unixepoch')
               and not exists (select 1 from main.repair_tried t where t.release_id = r.id)
             order by r.id desc limit ?4"
        );
        let rows: Vec<ReleaseRow> = conn
            .prepare(&sql)?
            .query_map(params![MIN_ROW_BYTES, now - DAYS * 86_400, now - SETTLED_SECS, CANDIDATES], |r| {
                Ok(ReleaseRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    group_name: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    poster: r.get(3)?,
                    posted_date: r.get(4)?,
                    size: r.get(5)?,
                    complete: r.get::<_, i64>(6)? != 0,
                    parts: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        for row in rows {
            if seen.contains(&row.id) {
                continue;
            }
            let (ids, size) = match crate::release_info::whole_post_with(conn, &row) {
                Ok(Some(post)) => (post.ids, post.size),
                _ => (vec![row.id], row.size.unwrap_or(0)),
            };
            seen.extend(ids.iter().copied());
            let permille = crate::release_info::permille_of(conn, &ids).unwrap_or(0);
            let worth = size >= MIN_BYTES && (MIN_PERMILLE..1000).contains(&permille);
            let windows = if worth { windows_of(conn, &ids)? } else { Vec::new() };
            // further from whole, what's missing is more likely in days of its groups the backfill hasn't read
            let days = if size >= MIN_BYTES && permille < MIN_PERMILLE {
                days_of(&windows_of(conn, &ids)?)
            } else {
                Vec::new()
            };
            found.posts.push(Looked { ids, permille, windows, days });
        }
    }
    Ok(found)
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

/// Each group's span of these rows' articles, with a margin.
fn windows_of(conn: &Connection, ids: &[i64]) -> rusqlite::Result<Vec<Window>> {
    let mut by_group: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    for id in ids {
        let s = store::shard_of_id(*id);
        let span: Option<(String, Option<i64>, Option<i64>)> = conn
            .prepare_cached(&format!(
                "select r.group_name, min(t.first_date), max(t.last_date) from s{s}.releases r
                 left join s{s}.file_timing t on t.release_id = r.id where r.id = ?1 group by r.id"
            ))?
            .query_row([id], |r| Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get(1)?, r.get(2)?)))
            .ok();
        let Some((group, Some(first), Some(last))) = span else { continue };
        let entry = by_group.entry(group).or_insert((first, last));
        entry.0 = entry.0.min(first);
        entry.1 = entry.1.max(last);
    }
    Ok(by_group
        .into_iter()
        .filter(|(group, _)| !group.is_empty())
        .map(|(group, (first, last))| Window { group, from: first - MARGIN, to: last + MARGIN })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_on_one_provider_are_one_machine() {
        assert_eq!(machine("news.newshosting.com"), "news.newshosting.com");
        assert_eq!(machine("News.Newshosting.com#second"), "news.newshosting.com");
        assert_ne!(machine("127.0.0.1:1190"), machine("127.0.0.1:1191"), "a port is another server");
    }

    #[test]
    fn big_video_releases_are_counted_per_group() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let release = |name: &str, group: &str, size: i64| {
            let article = crate::parser::Article {
                message_id: format!("<{name}@x>"),
                subject: format!("\"{name}\" yEnc (1/1)"),
                author: "p <p@x>".into(),
                filename: Some(format!("{name}.mkv")),
                part: Some(1),
                total_parts: Some(1),
                bytes: 10,
                ..Default::default()
            };
            let r = crate::parser::Release {
                name: name.into(),
                group: group.into(),
                articles: vec![article],
                ..Default::default()
            };
            store::save(&main, &[r]).unwrap();
            let shard = db::open_shard(&store::shard_path(&main, store::shard_of(group))).unwrap();
            shard.execute("update releases set size = ?2 where name = ?1", rusqlite::params![name, size]).unwrap();
        };
        let big = MEDIA_BYTES + 1;
        release("Film.2020.2160p.WEB-DL-X", "alt.binaries.a", big);
        release("Film.2021.1080p.BluRay.REMUX-Y", "alt.binaries.a", big);
        release("Film.2022.2160p.WEB-DL-Z", "alt.binaries.a", MEDIA_BYTES - 1);
        release("Show.S01E01.DV.HDR.2160p-W", "alt.binaries.b", big);
        release("Album.2020.FLAC-V", "alt.binaries.b", big);
        let conn = db::open_with_shards(&main).unwrap();
        let mut got = media_counts(&conn).unwrap();
        got.sort();
        assert_eq!(got, [("alt.binaries.a".to_string(), 2), ("alt.binaries.b".to_string(), 1)]);
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
