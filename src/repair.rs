//! Parts a server's listing left out, read again from another server. A provider's overview can miss articles it
//! stores: a Nyuu post in `alt.binaries.sleazemovies` came back without 50 of its parts from the one server that
//! indexes the group, while another server listed every one. So a big post that is nearly whole has the time window
//! it was posted in read again, in each of its groups, from servers that don't index that group, and what turns up
//! is saved like any other header: the missing parts join their rows by name. Nothing here is about one release.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

const CREATE: &str =
    "create table if not exists repair_tried (release_id INTEGER PRIMARY KEY, at INTEGER NOT NULL, permille INTEGER);
     create table if not exists repair_older (shard INTEGER PRIMARY KEY, before INTEGER)";

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

/// `stop` cuts a round's search of the shards short.
pub async fn run(ctx: PassContext, settings: PassSettings, db: Db, stopping: impl Fn() -> bool, stop: Arc<AtomicBool>) {
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
        // a stop drops the round where it is, mid read: its date searches and window reads dont look at the stop, and
        // a stop on prod waited 208 s for them. Safe, like a dropped pass: saves run whole on the writers, no cursor
        // moves, and a post not marked yet is read again next round
        match crate::nntp::unless_stopped(&stop, round(&ctx, &settings, &db, &stopping, &stop)).await {
            Some(Err(e)) if !stop.load(Ordering::Relaxed) => eprintln!("repair: {e:#}"),
            _ => {}
        }
    }
}

async fn round(
    ctx: &PassContext,
    settings: &PassSettings,
    db: &Db,
    stopping: &impl Fn() -> bool,
    stop: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();
    let stop = stop.clone();
    let found = tokio::task::spawn_blocking(move || reading_until(&stop, |conn| find(conn, now))).await??;
    let pool = &ctx.pool;
    let (to_read, looked) = plan(&found.posts, PER_ROUND);
    // a post with nothing to read was looked at; one with windows is marked once all of them were read, right then:
    // a round can take an hour, and one cut short by a restart had marked nothing, soo the next did it all again
    let posts = &found.posts;
    let tried: Vec<(i64, i64)> = looked
        .iter()
        .flat_map(|&i| {
            let post = &posts[i];
            post.ids.iter().map(move |id| (*id, post.permille))
        })
        .collect();
    let days: Vec<(String, i64)> = found.posts.iter().flat_map(|p| p.days.iter().cloned()).collect();
    // an older sweep moves past a post to read only once it was read: a round cut short finds it again
    let sweeps = |done: &HashSet<usize>| -> Vec<(usize, Option<i64>)> {
        found.older.iter().map(|o| (o.shard, o.next(done))).collect()
    };
    let mut done: HashSet<usize> = looked.iter().copied().collect();
    mark(db, now, days, tried, sweeps(&done)).await?;
    for post in to_read.iter().map(|&i| &posts[i]) {
        if stopping() {
            break;
        }
        // every backend that doesnt share another's overview is asked, one after another, until the post is whole:
        // only an incomplete post gets these reads, and none past the one that completes it
        'windows: for w in &post.windows {
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
            for server in pool.indexing_servers() {
                let host = pool.host(server);
                if !seen.insert(machine(&host)) {
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
                        spools.push((server, spool));
                        println!("[repair] {} {}..{} on {host}: {} headers", w.group, w.from, w.to, p.articles);
                        if p.articles > 0 && whole(&post.ids).await {
                            println!("[repair] {} is whole now", w.group);
                            break 'windows;
                        }
                    }
                    // a failure leaves its share to the next server
                    Err(e) => println!("[repair] {} {}..{} on {host}: {e:#}", w.group, w.from, w.to),
                }
            }
        }
        mark(db, now, Vec::new(), post.ids.iter().map(|id| (*id, post.permille)).collect(), Vec::new()).await?;
    }
    if !stopping() {
        done.extend(to_read.iter().copied());
        mark(db, now, Vec::new(), Vec::new(), sweeps(&done)).await?;
    }
    Ok(())
}

/// Whether the post of these rows is whole now: every file, each with every part (`release_info::whole_of`).
async fn whole(ids: &[i64]) -> bool {
    let ids = ids.to_vec();
    let whole = tokio::task::spawn_blocking(move || db::with_reader(|conn| crate::release_info::whole_of(conn, &ids)));
    matches!(whole.await, Ok(Ok(true)))
}

/// Asks for `days` of their groups' backfill first, marks the releases in `tried` as looked at (with the share their
/// post had), and moves each shard's older sweep on to where `older` says.
async fn mark(
    db: &Db,
    now: i64,
    days: Vec<(String, i64)>,
    tried: Vec<(i64, i64)>,
    older: Vec<(usize, Option<i64>)>,
) -> anyhow::Result<()> {
    on_db(db, move |conn| Ok(mark_in(conn, now, &days, &tried, &older)?)).await
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

/// `f` on a reader that gives up (SQLITE_INTERRUPT) once `stop` is set: a scan of every shard's releases runs for
/// minutes, and a stop waited for it to end. On prod the count of big releases per group, run at every start, held a
/// stop 12 minutes.
pub fn reading_until<T>(
    stop: &Arc<AtomicBool>,
    f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    db::with_reader(|conn| interruptible(conn, stop, f))
}

/// `f` on `conn`, interrupted once `stop` is set, the handler gone again after.
fn interruptible<T>(
    conn: &Connection,
    stop: &Arc<AtomicBool>,
    f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    let flag = stop.clone();
    conn.progress_handler(10_000, Some(move || flag.load(Ordering::Relaxed)))?;
    let out = f(conn);
    // a reader is kept for others: without the handler
    conn.progress_handler(10_000, None::<fn() -> bool>)?;
    out
}

/// Counts each group's big video releases now and then, for the backfill to take those groups first
/// (`chunks::MEDIA_GROUPS`). With one server or many: it doesn't read from any. `stop` cuts a count short.
pub async fn count_media_every(db: Db, stopping: impl Fn() -> bool, stop: Arc<AtomicBool>) {
    while !stopping() {
        if let Err(e) = count_media(&db, &stop).await
            && !stop.load(Ordering::Relaxed)
        {
            eprintln!("counting big releases per group: {e:#}");
        }
        let next = Instant::now() + COUNT_EVERY;
        while !stopping() && Instant::now() < next {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

/// Notes how many big video releases each group holds.
async fn count_media(db: &Db, stop: &Arc<AtomicBool>) -> anyhow::Result<()> {
    let stop = stop.clone();
    let counts = tokio::task::spawn_blocking(move || reading_until(&stop, media_counts)).await??;
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

/// Whether two servers' GROUP numbers, (first, last), may be one backend's: last ones a moment of posting apart. Two
/// backends number a group on their own, millions apart. The first can differ: resellers of one backend each keep
/// it as long as their retention, so it takes `same_ids` to say.
fn same_spool(a: (u64, u64), b: (u64, u64)) -> bool {
    a.1.abs_diff(b.1) <= (a.1.max(b.1) / 100_000).max(1_000)
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
        let rows =
            release_rows(conn, &sql, params![MIN_ROW_BYTES, now - DAYS * 86_400, now - SETTLED_SECS, CANDIDATES])?;
        for row in rows {
            if seen.contains(&row.id) {
                continue;
            }
            found.posts.push(look(conn, &row, true, &mut seen)?);
        }
    }
    by_worth(&mut found.posts);
    for s in 0..store::SHARDS {
        let first = found.posts.len();
        let (rows, end) = older_rows(conn, s, now)?;
        let mut keys = Vec::new();
        for row in rows {
            if seen.contains(&row.id) {
                continue;
            }
            keys.push(row.id);
            found.posts.push(look(conn, &row, false, &mut seen)?);
        }
        found.older.push(Older { shard: s, first, keys, end });
    }
    Ok(found)
}

/// `row`'s post, with what it wants read. `days`: and the days of its groups' backfill to take first, when it's
/// further from whole (an older post's days aren't asked for: they are what the backfill reads anyway, in its order).
fn look(conn: &Connection, row: &ReleaseRow, days: bool, seen: &mut HashSet<i64>) -> rusqlite::Result<Looked> {
    let (ids, size) = match crate::release_info::whole_post_with(conn, row) {
        Ok(Some(post)) => (post.ids, post.size),
        _ => (vec![row.id], row.size.unwrap_or(0)),
    };
    seen.extend(ids.iter().copied());
    let permille = crate::release_info::permille_of(conn, &ids).unwrap_or(0);
    let worth = size >= MIN_BYTES && (MIN_PERMILLE..1000).contains(&permille);
    let windows = if worth { windows_of(conn, &ids)? } else { Vec::new() };
    // further from whole, what's missing is more likely in days of its groups the backfill hasn't read
    let days = if days && size >= MIN_BYTES && permille < MIN_PERMILLE {
        days_of(&windows_of(conn, &ids)?)
    } else {
        Vec::new()
    };
    Ok(Looked { ids, permille, windows, days, media: is_media(&row.name), size })
}

/// The release rows `sql` selects, its columns those of `find`'s query.
pub(crate) fn release_rows(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> rusqlite::Result<Vec<ReleaseRow>> {
    conn.prepare(sql)?
        .query_map(params, |r| {
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
        .collect()
}

/// Shard `s`'s older sweep this round: the rows, newest first, of the next `OLDER_SCAN` below where it got to, that
/// were posted before `DAYS` and are worth a look like `find`'s (at most `OLDER_CANDIDATES`), and where it goes on
/// once they are done: below the last of them when that many came, else below the rows read, or from the newest
/// again (`None`) when none were left.
fn older_rows(conn: &Connection, s: usize, now: i64) -> rusqlite::Result<(Vec<ReleaseRow>, Option<i64>)> {
    let before: i64 = conn
        .query_row("select before from main.repair_older where shard = ?1", [s as i64], |r| r.get::<_, Option<i64>>(0))
        .ok()
        .flatten()
        .unwrap_or(i64::MAX);
    let window = format!("select id from s{s}.releases where id < ?1 order by id desc limit {OLDER_SCAN}");
    let lowest: Option<i64> = conn.query_row(&format!("select min(id) from ({window})"), [before], |r| r.get(0))?;
    let Some(lowest) = lowest else { return Ok((Vec::new(), None)) };
    let sql = format!(
        "select r.id, coalesce(r.display_name, r.name), r.group_name, r.poster, r.posted_date, r.size, r.complete,
                r.parts
         from s{s}.releases r
         where r.id in ({window}) and r.complete = 0 and r.size >= ?2 and r.posted_date < datetime(?3, 'unixepoch')
           and not exists (select 1 from main.repair_tried t where t.release_id = r.id)
         order by r.id desc limit ?4"
    );
    let rows = release_rows(conn, &sql, params![before, MIN_ROW_BYTES, now - DAYS * 86_400, OLDER_CANDIDATES])?;
    let end = match rows.last() {
        Some(last) if rows.len() as i64 == OLDER_CANDIDATES => last.id,
        _ => lowest,
    };
    Ok((rows, Some(end)))
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

    /// The main database's writes of a round and the group cursors it reads, alike on SQLite and Postgres.
    #[test]
    fn marks_and_group_cursors_on_both_backends() {
        use crate::db;
        use crate::pg::sql::vals;
        crate::pg::testing::each(
            |c| {
                c.execute_batch(
                    "create table groups (name TEXT PRIMARY KEY, live_cursor INTEGER, backfill_cursor INTEGER,
                        first_article INTEGER, last_article INTEGER, low_article INTEGER, low_posted INTEGER,
                        high_article INTEGER, high_posted INTEGER, start_posted INTEGER)",
                )
                .unwrap();
                c.execute_batch(CREATE).unwrap();
                crate::store::create_main(c).unwrap();
            },
            |c| {
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
                assert_eq!(
                    (a.first, a.last, a.low, a.high),
                    (Some(10), Some(200), Some((60, 6000)), Some((145, 14_500)))
                );
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
                let older: Vec<Option<i64>> =
                    c.column("select before from repair_older order by shard", vals![]).unwrap();
                assert_eq!(older, [Some(41), None]);
                let k = crate::chunks::claim(c, &[("g".into(), i64::MIN)], "s", 3000).unwrap().unwrap();
                assert_eq!(k.day, 19_995, "the day asked for first");
            },
        );
    }

    #[test]
    fn a_scan_gives_up_once_stopping_and_the_reader_reads_on_after() {
        let conn = Connection::open_in_memory().unwrap();
        let long = |c: &Connection| {
            c.query_row(
                "with recursive n(i) as (select 1 union all select i + 1 from n where i < 5000000) select count(*) from n",
                [],
                |r| r.get::<_, i64>(0),
            )
        };
        let stop = Arc::new(AtomicBool::new(true));
        let e = interruptible(&conn, &stop, long).unwrap_err();
        assert_eq!(e.sqlite_error_code(), Some(rusqlite::ErrorCode::OperationInterrupted), "{e}");
        assert_eq!(long(&conn).unwrap(), 5_000_000, "no handler left on the connection");
        assert_eq!(interruptible(&conn, &Arc::new(AtomicBool::new(false)), long).unwrap(), 5_000_000);
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
    fn an_older_sweep_reads_old_incomplete_rows_below_where_it_got_to() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let group = "alt.binaries.a";
        let s = store::shard_of(group);
        let now = 2_000_000_000;
        let old = "2033-01-01 00:00:00"; // a year before now
        let recent = "2033-05-18 00:00:00"; // a few hours before
        let release = |name: &str, posted: &str, size: i64, complete: bool| -> i64 {
            let article = crate::parser::Article {
                message_id: format!("<{name}@x>"),
                subject: format!("\"{name}\" yEnc (1/2)"),
                author: "p <p@x>".into(),
                filename: Some(format!("{name}.mkv")),
                part: Some(1),
                total_parts: Some(2),
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
            let shard = db::open_shard(&store::shard_path(&main, s)).unwrap();
            shard
                .execute(
                    "update releases set size = ?2, posted_date = ?3, complete = ?4 where name = ?1",
                    rusqlite::params![name, size, posted, complete],
                )
                .unwrap();
            shard.query_row("select id from releases where name = ?1", [name], |r| r.get(0)).unwrap()
        };
        let a = release("Old.Film.A.2020.1080p-X", old, 2 << 30, false);
        let _small = release("Old.Small.2020-X", old, 1 << 20, false);
        let _whole = release("Old.Whole.2020.1080p-X", old, 2 << 30, true);
        let b = release("Old.Film.B.2021.1080p-X", old, 2 << 30, false);
        let _new = release("New.Film.2026.1080p-X", recent, 2 << 30, false);
        let tried = release("Old.Tried.2019.1080p-X", old, 2 << 30, false);
        let conn = db::open_with_shards(&main).unwrap();
        conn.execute_batch(CREATE).unwrap();
        conn.execute("insert into repair_tried (release_id, at, permille) values (?1, 0, 0)", [tried]).unwrap();
        let ids = |rows: &[ReleaseRow]| rows.iter().map(|r| r.id).collect::<Vec<_>>();
        let (rows, end) = older_rows(&conn, s, now).unwrap();
        assert_eq!(ids(&rows), [b, a], "old, big enough, incomplete and not tried, newest first");
        let lowest: i64 = conn.query_row(&format!("select min(id) from s{s}.releases"), [], |r| r.get(0)).unwrap();
        assert_eq!(end, Some(lowest), "fewer than a round's worth: on below every row read");
        conn.execute("insert into repair_older (shard, before) values (?1, ?2)", rusqlite::params![s as i64, b])
            .unwrap();
        assert_eq!(ids(&older_rows(&conn, s, now).unwrap().0), [a], "below where it got to");
        conn.execute("update repair_older set before = ?1", [lowest]).unwrap();
        assert_eq!(older_rows(&conn, s, now).unwrap(), (vec![], None), "swept: from the newest again");
        conn.execute("update repair_older set before = null", []).unwrap();
        assert_eq!(ids(&older_rows(&conn, s, now).unwrap().0), [b, a]);
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
