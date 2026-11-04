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

/// A post looked at: its rows, its share then, and the windows it wants read (none: nothing to do for it).
#[derive(Debug, Default)]
pub struct Looked {
    pub ids: Vec<i64>,
    pub permille: i64,
    pub windows: Vec<Window>,
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
            let mut read_from = 0;
            for server in pool.indexing_servers() {
                let host = pool.host(server);
                if read_from >= SERVERS || !seen.insert(machine(&host)) {
                    continue;
                }
                match reread_window(ctx, settings, db, &w.group, server, (w.from, w.to), MAX_ARTICLES).await {
                    Ok(p) => {
                        read_from += 1;
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
    on_db(db, move |conn| {
        let tx = conn.transaction()?;
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

/// The machine behind a server's name: host and port, without an explicit `#key`, in lower case. Two accounts at one
/// provider have the same.
fn machine(host: &str) -> String {
    host.split('#').next().unwrap_or(host).to_lowercase()
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
            found.posts.push(Looked { ids, permille, windows });
        }
    }
    Ok(found)
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
}
