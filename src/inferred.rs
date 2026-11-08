//! Parts no listing showed, asked for by the ids their file's own articles name. An upload whose ids run
//! `part{N}of{T}.{token}@domain` (`nzb::inferred_segments`) tells us each missing part's id, and an id needs no
//! listing: every server is asked for its headers (the same all-servers read a body takes, which calls an article
//! missing only when each one says so), and what turns up is saved like any other header, soo the part joins its
//! file and counts towards whole. An id no server has is asked again later, less and less often; a post with more
//! missing parts than a round asks is asked over several rounds.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, params};

use crate::indexer::{Db, PassContext, on_db};
use crate::nntp::{BackgroundBody, BackgroundRead, Overview, Pool};
use crate::search::{ArticleRow, ReleaseRow};
use crate::store;

/// posts younger than this may still be arriving, by upload or by listing
const SETTLED_SECS: i64 = 2 * 3600;
/// how often a round looks for parts to ask for
const EVERY: Duration = Duration::from_secs(300);
/// releases this big or bigger: smaller posts aren't worth the reads
const MIN_BYTES: i64 = 64 << 20;
/// posted this recently: past it the parts a server lacks are rarely anywhere
const DAYS: i64 = 7;
/// releases looked at per shard per round, the latest posted first among those due. posted_date has no index: the
/// query walks the shard's releases, under half a second for 1.6 million on prod, and row ids don't go by when a post
/// was posted (the backfill saves old posts with new ids, and recent ones with ids from before)
const CANDIDATES: i64 = 500;
/// a post less whole than this (in thousandths) isn't one a few parts make whole, nor one whose ids prove much
const MIN_PERMILLE: i64 = 500;
/// ids asked for per round, and at once
const PER_ROUND: usize = 3_000;
const AT_ONCE: usize = 8;
/// an article's headers, more than any real one has
const HEAD_BYTES: usize = 64 << 10;
/// a post with nothing to ask for is looked at again after this: more parts may have come and prove its ids
const NOTHING_AGAIN: i64 = 86_400;
/// an id no server had (or whose headers were no use): asked again after this, doubling with each try, up to `LONGEST`
const ABSENT_AGAIN: i64 = 6 * 3600;
const LONGEST: i64 = 2 * 86_400;
/// an id a server couldn't be asked for, or whose headers couldn't be saved
const FAILED_AGAIN: i64 = 1800;

const CREATE: &str =
    "create table if not exists inferred_asked (release_id INTEGER PRIMARY KEY, next_at INTEGER NOT NULL);
     create table if not exists inferred_ids (
        message_id TEXT PRIMARY KEY, next_at INTEGER NOT NULL, tries INTEGER NOT NULL)";

/// One part to ask for: its post (by the release it was found by), the group it's saved in unless its headers say
/// another, and the row `nzb::inferred_segments` made for it.
#[derive(Clone, Debug)]
pub struct Wanted {
    pub release_id: i64,
    pub group: String,
    /// every group of its post: where else it may be saved
    pub groups: Arc<[String]>,
    pub row: ArticleRow,
}

impl Wanted {
    /// its id as `inferred_ids` keeps it, without the `<>`
    fn key(&self) -> String {
        self.row.message_id.trim().trim_matches(['<', '>']).to_string()
    }
}

/// A round's plan: the parts to ask for, in order; the rows of posts with nothing to ask; and posts whose every part
/// was asked lately, each with when the first of them is due again.
#[derive(Debug, Default)]
pub struct Plan {
    pub wanted: Vec<Wanted>,
    pub nothing: Vec<i64>,
    pub waiting: Vec<(Vec<i64>, i64)>,
    /// the rows of posts whose every due id is in `wanted`: none is due again before `FAILED_AGAIN`
    pub asked: Vec<i64>,
}

/// What became of one id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// a server had it, and it was saved. It waits like an absent one: a part saved that didn't join its file (its
    /// subject read another way) would be asked for every round
    Found,
    /// every server said it hasn't it
    Absent,
    /// a server answered with headers that aren't a listing row (none of the article's, no subject, too big), which
    /// asking again won't change
    Unusable,
    /// a server couldn't be asked, or what it gave couldn't be saved
    Failed,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Outcome::Found => "found",
            Outcome::Absent => "absent",
            Outcome::Unusable => "unusable",
            Outcome::Failed => "failed",
        }
    }
}

pub async fn run(ctx: PassContext, db: Db, stopping: impl Fn() -> bool, stop: Arc<AtomicBool>) {
    if let Err(e) = on_db(&db, |conn| Ok(conn.execute_batch(CREATE)?)).await {
        eprintln!("inferred: {e:#}");
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
        if let Err(e) = round(&ctx.pool, &db, &stopping, &stop).await
            && !stopping()
        {
            eprintln!("inferred: {e:#}");
        }
    }
}

async fn round(pool: &Arc<Pool>, db: &Db, stopping: &impl Fn() -> bool, stop: &Arc<AtomicBool>) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();
    // the scan gives up once stopping, as `repair`'s does
    let scan_stop = stop.clone();
    let plan = tokio::task::spawn_blocking(move || {
        crate::repair::reading_until(&scan_stop, |conn| {
            let post = |row: &ReleaseRow| {
                let (ids, size) = match crate::release_info::whole_post_with(conn, row)? {
                    Some(post) => (post.ids, post.size),
                    None => (vec![row.id], row.size.unwrap_or(0)),
                };
                let permille = crate::release_info::permille_of(conn, &ids)?;
                Ok((ids, permille, size))
            };
            let articles = |row: &ReleaseRow| {
                crate::nzb::inventory_with(conn, row).map(|(articles, groups, _)| (articles, groups))
            };
            plan(conn, now, PER_ROUND, post, articles)
        })
    })
    .await??;
    let Plan { wanted, nothing, mut waiting, asked } = plan;
    waiting.push((asked, now + FAILED_AGAIN));
    on_db(db, move |conn| Ok(mark_posts(conn, &nothing, &waiting, now)?)).await?;
    if wanted.is_empty() {
        return Ok(());
    }
    // asked a few at once, each across every server; an id whose read a stop drops is asked next time
    let mut outcomes: Vec<(Wanted, Outcome)> = Vec::new();
    let mut found: BTreeMap<String, Vec<(usize, Overview)>> = BTreeMap::new();
    let mut set = tokio::task::JoinSet::new();
    // whose read each task is, for one that panics
    let mut whose: HashMap<tokio::task::Id, Wanted> = HashMap::new();
    let mut queue = wanted.into_iter();
    loop {
        while set.len() < AT_ONCE && !stopping() {
            let Some(w) = queue.next() else { break };
            let pool = pool.clone();
            let id = format!("<{}>", w.key());
            let task = set.spawn(async move { pool.background_read(&id, HEAD_BYTES, BackgroundRead::Head).await });
            whose.insert(task.id(), w);
        }
        let Some(Some(done)) = crate::nntp::unless_stopped(stop, set.join_next_with_id()).await else { break };
        let (task, got) = match done {
            Ok((task, got)) => (task, Ok(got)),
            Err(e) => (e.id(), Err(())),
        };
        let Some(w) = whose.remove(&task) else { continue };
        let outcome = match got {
            Ok(Ok(BackgroundBody::Body(head))) => match overview_of(&head, &w) {
                Some((group, o)) => {
                    found.entry(group).or_default().push((outcomes.len(), o));
                    Outcome::Found
                }
                None => Outcome::Unusable,
            },
            Ok(Ok(BackgroundBody::TooBig)) => Outcome::Unusable,
            Ok(Ok(BackgroundBody::Missing)) => Outcome::Absent,
            Ok(Err(_)) | Err(()) => Outcome::Failed,
        };
        outcomes.push((w, outcome));
    }
    // a stop drops the rest of the round, as `repair`'s does (saves run whole on the writers): what wasn't marked is
    // asked again next time
    crate::nntp::unless_stopped(stop, finish(pool, db, outcomes, found, now)).await.transpose()?;
    Ok(())
}

/// Saves what was found and marks what each id came to.
async fn finish(
    pool: &Arc<Pool>,
    db: &Db,
    mut outcomes: Vec<(Wanted, Outcome)>,
    found: BTreeMap<String, Vec<(usize, Overview)>>,
    now: i64,
) -> anyhow::Result<()> {
    // saved like a listing's headers: each part joins its file by its subject. A group that couldn't be saved has its
    // ids asked again soon; the other groups are saved all the same
    for (group, headers) in found {
        let at: Vec<usize> = headers.iter().map(|(i, _)| *i).collect();
        let headers: Vec<Overview> = headers.into_iter().map(|(_, o)| o).collect();
        let unsaved = pool.reserve_unsaved_shard(store::shard_of(&group), headers.len() as u64).await;
        if let Err(e) = crate::indexer::save_slice(pool, db, &group, headers, unsaved).await {
            eprintln!("inferred: saving {group}: {e:#}");
            for i in at {
                outcomes[i].1 = Outcome::Failed;
            }
        }
    }
    let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
    let mut posts = HashSet::new();
    for (w, o) in &outcomes {
        *counts.entry(o.label()).or_default() += 1;
        posts.insert(w.release_id);
    }
    for o in [Outcome::Found, Outcome::Absent, Outcome::Unusable, Outcome::Failed] {
        crate::metrics::names::INFERRED_PARTS
            .inc(&[("outcome", o.label())], counts.get(o.label()).copied().unwrap_or(0));
    }
    println!(
        "[inferred] asked {} parts of {} posts: {} found, {} on no server, {} unusable, {} couldnt be asked or saved",
        outcomes.len(),
        posts.len(),
        counts.get("found").unwrap_or(&0),
        counts.get("absent").unwrap_or(&0),
        counts.get("unusable").unwrap_or(&0),
        counts.get("failed").unwrap_or(&0)
    );
    let marks: Vec<(String, Outcome)> = outcomes.iter().map(|(w, o)| (w.key(), *o)).collect();
    on_db(db, move |conn| Ok(mark_ids(conn, &marks, now)?)).await
}

/// The parts worth asking for this round, most nearly whole posts first, `budget` ids at most: the ids of each post
/// that aren't waiting after an earlier try, the last post taken only in part when the budget ends in it (the rest
/// next round). And the posts with nothing to ask, and those whose every id waits. `conn` has the shards attached
/// and the tables in main; `post` gives a release's post (its rows), how whole it is in thousandths and its size,
/// `articles` the articles its NZB is made of and its groups. A stop (the reader interrupted) ends the plan with that
/// error; a release another error keeps from being read has nothing to ask, for now.
pub fn plan(
    conn: &Connection,
    now: i64,
    budget: usize,
    post: impl Fn(&ReleaseRow) -> rusqlite::Result<(Vec<i64>, i64, i64)>,
    articles: impl Fn(&ReleaseRow) -> rusqlite::Result<(Vec<ArticleRow>, Vec<String>)>,
) -> rusqlite::Result<Plan> {
    let has_table = conn.prepare("select 1 from main.sqlite_master where name = 'inferred_ids'")?.exists([])?;
    if !has_table {
        return Ok(Plan::default());
    }
    let mut out = Plan::default();
    let mut seen = HashSet::new();
    // (permille, size, release, its post's rows)
    let mut posts: Vec<(i64, i64, ReleaseRow, Vec<i64>)> = Vec::new();
    for s in 0..store::SHARDS {
        let sql = format!(
            "select r.id, coalesce(r.display_name, r.name), r.group_name, r.poster, r.posted_date, r.size, r.complete,
                    r.parts
             from s{s}.releases r
             where r.complete = 0 and r.size >= ?1
               and r.posted_date >= datetime(?2, 'unixepoch') and r.posted_date < datetime(?3, 'unixepoch')
               and not exists (select 1 from main.inferred_asked a where a.release_id = r.id and a.next_at > ?4)
             order by r.posted_date desc limit ?5"
        );
        let rows = crate::repair::release_rows(
            conn,
            &sql,
            params![MIN_BYTES, now - DAYS * 86_400, now - SETTLED_SECS, now, CANDIDATES],
        )?;
        for row in rows {
            if !seen.insert(row.id) {
                continue;
            }
            if crate::filters::group_blocked(&row.group_name) {
                out.nothing.push(row.id);
                continue;
            }
            let (mut ids, permille, size) = match post(&row) {
                Ok(p) => p,
                Err(e) if interrupted(&e) => return Err(e),
                Err(e) => {
                    eprintln!("inferred: release {}: {e}", row.id);
                    out.nothing.push(row.id);
                    continue;
                }
            };
            if !ids.contains(&row.id) {
                ids.push(row.id);
            }
            seen.extend(ids.iter().copied());
            if (MIN_PERMILLE..1000).contains(&permille) {
                posts.push((permille, size, row, ids));
            } else {
                out.nothing.extend(ids);
            }
        }
    }
    posts.sort_by_key(|(permille, size, _, _)| (std::cmp::Reverse(*permille), std::cmp::Reverse(*size)));
    let mut waits = conn.prepare("select next_at from main.inferred_ids where message_id = ?1")?;
    for (_, _, row, ids) in posts {
        let room = budget.saturating_sub(out.wanted.len());
        if room == 0 {
            break;
        }
        let (articles, groups) = match articles(&row) {
            Ok(a) => a,
            Err(e) if interrupted(&e) => return Err(e),
            Err(e) => {
                eprintln!("inferred: release {}: {e}", row.id);
                out.nothing.extend(ids);
                continue;
            }
        };
        let groups: Arc<[String]> = groups.into();
        let rows: Vec<ArticleRow> = crate::nzb::inferred_segments(&articles).into_iter().flat_map(|f| f.rows).collect();
        if rows.is_empty() {
            out.nothing.extend(ids);
            continue;
        }
        let mut due = Vec::new();
        let mut first_due: Option<i64> = None;
        for r in rows {
            let w = Wanted { release_id: row.id, group: row.group_name.clone(), groups: groups.clone(), row: r };
            match waits.query_row([w.key()], |r| r.get::<_, i64>(0)).optional()? {
                Some(at) if at > now => first_due = Some(first_due.map_or(at, |f| f.min(at))),
                _ => due.push(w),
            }
        }
        match first_due {
            Some(at) if due.is_empty() => out.waiting.push((ids, at)),
            _ => {
                if due.len() <= room {
                    out.asked.extend(ids);
                }
                out.wanted.extend(due.into_iter().take(room));
            }
        }
    }
    Ok(out)
}

/// When an id is asked again after `tries` answers that it isn't to be had.
pub fn backoff(now: i64, tries: i64) -> i64 {
    let shift = tries.clamp(1, 16) - 1;
    now + (ABSENT_AGAIN << shift).min(LONGEST)
}

/// Posts with nothing to ask, looked at again after `NOTHING_AGAIN`; posts whose ids all wait, when the first is due.
/// What was due longer ago than a post is looked at for goes: its post is past `DAYS`.
fn mark_posts(conn: &mut Connection, nothing: &[i64], waiting: &[(Vec<i64>, i64)], now: i64) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    let stale = now - DAYS * 86_400;
    tx.execute("delete from inferred_asked where next_at < ?1", [stale])?;
    tx.execute("delete from inferred_ids where next_at < ?1", [stale])?;
    {
        let mut stmt =
            tx.prepare_cached("insert or replace into inferred_asked (release_id, next_at) values (?1, ?2)")?;
        for id in nothing {
            stmt.execute(params![id, now + NOTHING_AGAIN])?;
        }
        for (ids, at) in waiting {
            for id in ids {
                stmt.execute(params![id, at])?;
            }
        }
    }
    tx.commit()
}

/// What each id asked came to: one found, or that no server has (or whose headers were no use), waits longer with each
/// try; one that couldn't be asked or saved waits `FAILED_AGAIN`, its tries as they were.
fn mark_ids(conn: &mut Connection, marks: &[(String, Outcome)], now: i64) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    for (id, outcome) in marks {
        let tries: i64 = tx
            .query_row("select tries from inferred_ids where message_id = ?1", [id], |r| r.get(0))
            .optional()?
            .unwrap_or(0);
        let (next, tries) = match outcome {
            Outcome::Found | Outcome::Absent | Outcome::Unusable => (backoff(now, tries + 1), tries + 1),
            Outcome::Failed => (now + FAILED_AGAIN, tries),
        };
        tx.prepare_cached("insert or replace into inferred_ids (message_id, next_at, tries) values (?1, ?2, ?3)")?
            .execute(params![id, next, tries])?;
    }
    tx.commit()
}

/// Whether a read failed because a stop interrupted it.
fn interrupted(e: &rusqlite::Error) -> bool {
    e.sqlite_error_code() == Some(rusqlite::ErrorCode::OperationInterrupted)
}

/// The listing row a server's headers for `w` make, and the group to save it in: `w`'s, unless the headers' `Xref:`
/// names other groups of its post and not it (a post cut across groups, its file in another of them). Subject and From
/// are made as a listing makes them: a folded line's tab a space, and mojibake undone. None when they aren't the article
/// asked for. Its size is what the headers say (`Bytes:`, or `Lines:`), else the size `w` was given.
pub fn overview_of(head: &[u8], w: &Wanted) -> Option<(String, Overview)> {
    let value = |name: &str| crate::spotnet::header::header_value(head, name);
    let message_id = value("Message-ID")?;
    if message_id.trim().trim_matches(['<', '>']) != w.row.message_id.trim().trim_matches(['<', '>']) {
        return None;
    }
    let listed = |v: String| crate::nntp::undo_mojibake(v.replace('\t', " "));
    let subject = listed(value("Subject")?);
    let number_of = |n: Option<String>| n.and_then(|n| n.trim().parse::<i64>().ok()).filter(|n| *n > 0);
    let bytes = number_of(value("Bytes")).unwrap_or(0);
    let lines = number_of(value("Lines")).unwrap_or(0);
    let xref = value("Xref").unwrap_or_default();
    let numbers: Vec<(&str, &str)> = xref.split_whitespace().skip(1).filter_map(|e| e.split_once(':')).collect();
    let ours = numbers.iter().any(|(g, _)| *g == w.group);
    let group = match numbers
        .iter()
        .find(|(g, _)| !ours && w.groups.iter().any(|p| p == g) && !crate::filters::group_blocked(g))
    {
        Some((g, _)) => g.to_string(),
        None => w.group.clone(),
    };
    let row = Overview {
        // the answering server's numbers aren't the listing's: none (0, which the store's file timing passes over)
        number: 0,
        subject,
        from: listed(value("From").unwrap_or_default()),
        date: value("Date").unwrap_or_default(),
        message_id: format!("<{}>", message_id.trim().trim_matches(['<', '>'])),
        references: value("References").unwrap_or_default(),
        bytes: if bytes == 0 && lines == 0 { w.row.bytes.unwrap_or(0) } else { bytes },
        lines,
        xref_groups: (!xref.is_empty()).then(|| crate::nntp::xref_groups(&format!("Xref: {xref}"))).flatten(),
    };
    Some((group, row))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wanted(id: &str, group: &str) -> Wanted {
        Wanted {
            release_id: 1,
            group: group.into(),
            groups: vec![group.to_string()].into(),
            row: ArticleRow {
                message_id: id.into(),
                filename: Some("a.rar".into()),
                part: Some(3),
                total_parts: Some(6),
                bytes: Some(640),
                subject: None,
                poster: None,
                posted_date: None,
            },
        }
    }

    const HEAD: &[u8] =
        b"Path: x!not-for-mail\r\nFrom: DAX <dax@domain.com>\r\nSubject: [1/2] - \"a.rar\" yEnc\r\n (3/6)\r\n\
Date: Sat, 07 Nov 2026 06:22:22 GMT\r\nMessage-ID: <part3of6.tok@bzh29>\r\nLines: 5022\r\n\
Xref: news.example alt.binaries.movies:11666417361 alt.binaries.hdtv:42\r\n";

    #[test]
    fn a_servers_headers_make_the_listing_row() {
        let (group, o) = overview_of(HEAD, &wanted("<part3of6.tok@bzh29>", "alt.binaries.movies")).unwrap();
        assert_eq!(group, "alt.binaries.movies");
        assert_eq!(o.subject, "[1/2] - \"a.rar\" yEnc (3/6)");
        assert_eq!(o.from, "DAX <dax@domain.com>");
        assert_eq!(o.message_id, "<part3of6.tok@bzh29>");
        assert_eq!(o.number, 0);
        assert_eq!(o.lines, 5022);
        // no Bytes: the line count gives the size (`Overview::into_article`)
        assert_eq!(o.bytes, 0);
        assert_eq!(o.xref_groups, Some(2));
        assert_eq!(o.clone().into_article().bytes, 5022 * 130);
        // a post cut across groups: saved in the group of the post the article is in
        let mut cut = wanted("<part3of6.tok@bzh29>", "alt.binaries.teevee");
        cut.groups = vec!["alt.binaries.teevee".to_string(), "alt.binaries.movies".to_string()].into();
        assert_eq!(overview_of(HEAD, &cut).unwrap().0, "alt.binaries.movies");
        // but never in a group the post isn't in
        let alone = wanted("<part3of6.tok@bzh29>", "alt.binaries.teevee");
        assert_eq!(overview_of(HEAD, &alone).unwrap().0, "alt.binaries.teevee");
        // a folded line's tab is a space, as in a listing
        let tabbed = b"Subject: \"a.rar\" yEnc\r\n\t(3/6)\r\nMessage-ID: <part3of6.tok@bzh29>\r\n";
        assert_eq!(overview_of(tabbed, &alone).unwrap().1.subject, "\"a.rar\" yEnc (3/6)");
    }

    #[test]
    fn headers_without_a_size_keep_the_one_the_part_was_given() {
        let head = b"Subject: \"a.rar\" yEnc (3/6)\nMessage-ID: <part3of6.tok@bzh29>\nFrom: x\n";
        let (group, o) = overview_of(head, &wanted("part3of6.tok@bzh29", "g")).unwrap();
        assert_eq!((group.as_str(), o.bytes, o.lines, o.number, o.xref_groups), ("g", 640, 0, 0, None));
        let sized = b"Subject: s\nMessage-ID: <part3of6.tok@bzh29>\nBytes: 661524\n";
        assert_eq!(overview_of(sized, &wanted("<part3of6.tok@bzh29>", "g")).unwrap().1.bytes, 661524);
    }

    #[test]
    fn headers_of_another_article_or_without_a_subject_are_no_row() {
        let other = b"Subject: s\nMessage-ID: <part4of6.tok@bzh29>\n";
        assert!(overview_of(other, &wanted("<part3of6.tok@bzh29>", "g")).is_none());
        let bare = b"Message-ID: <part3of6.tok@bzh29>\n";
        assert!(overview_of(bare, &wanted("<part3of6.tok@bzh29>", "g")).is_none());
    }

    #[test]
    fn an_id_on_no_server_is_asked_again_less_often() {
        assert_eq!(backoff(0, 1), ABSENT_AGAIN);
        assert_eq!(backoff(0, 2), 2 * ABSENT_AGAIN);
        assert_eq!(backoff(0, 3), 4 * ABSENT_AGAIN);
        assert_eq!(backoff(0, 9), LONGEST);
    }

    #[test]
    fn each_id_keeps_its_own_tries() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(CREATE).unwrap();
        let row = |conn: &Connection, id: &str| -> Option<(i64, i64)> {
            conn.query_row("select next_at, tries from inferred_ids where message_id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .unwrap()
        };
        let mark = |conn: &mut Connection, id: &str, o: Outcome, now: i64| {
            mark_ids(conn, &[(id.to_string(), o)], now).unwrap();
        };
        mark(&mut conn, "a", Outcome::Absent, 100);
        assert_eq!(row(&conn, "a"), Some((100 + ABSENT_AGAIN, 1)));
        // headers that can't be used are an answer too
        mark(&mut conn, "a", Outcome::Unusable, 200);
        assert_eq!(row(&conn, "a"), Some((200 + 2 * ABSENT_AGAIN, 2)));
        // a server that couldn't be asked isn't a try
        mark(&mut conn, "a", Outcome::Failed, 300);
        assert_eq!(row(&conn, "a"), Some((300 + FAILED_AGAIN, 2)));
        // found waits too: should it not have joined its file, it isn't asked every round
        mark(&mut conn, "a", Outcome::Found, 400);
        assert_eq!(row(&conn, "a"), Some((400 + 4 * ABSENT_AGAIN, 3)));
        mark(&mut conn, "b", Outcome::Failed, 500);
        assert_eq!(row(&conn, "b"), Some((500 + FAILED_AGAIN, 0)));

        mark_posts(&mut conn, &[1], &[(vec![2, 3], 900)], 600).unwrap();
        let next = |id: i64| -> i64 {
            conn.query_row("select next_at from inferred_asked where release_id = ?1", [id], |r| r.get(0)).unwrap()
        };
        assert_eq!((next(1), next(2), next(3)), (600 + NOTHING_AGAIN, 900, 900));
        // a week on, what was due long ago is gone
        let later = 600 + NOTHING_AGAIN + DAYS * 86_400 + 1;
        mark_posts(&mut conn, &[], &[], later).unwrap();
        assert_eq!(row(&conn, "b"), None);
        assert_eq!(conn.query_row("select count(*) from inferred_asked", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    /// Every shard attached, `inferred_asked` in main, and releases (id, size, posted, complete) in shard 0.
    fn shards(rows: &[(i64, i64, &str, bool)]) -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        crate::filters::register_sql(&conn).unwrap();
        for s in 0..store::SHARDS {
            let path = dir.path().join(format!("atlas.s{s}.db"));
            store::create_shard(&path).unwrap();
            conn.execute(&format!("attach database ?1 as s{s}"), [path.to_string_lossy()]).unwrap();
        }
        conn.execute_batch(CREATE).unwrap();
        for &(id, size, posted, complete) in rows {
            conn.execute(
                "insert into s0.releases (id, name, group_name, poster, posted_date, size, complete, parts)
                 values (?1, 'r' || ?1, 'alt.binaries.movies', 'p', ?2, ?3, ?4, 1)",
                params![id, posted, size, complete],
            )
            .unwrap();
        }
        (dir, conn)
    }

    fn pattern(total: i64, have: &[i64]) -> Vec<ArticleRow> {
        have.iter()
            .map(|&p| ArticleRow {
                message_id: format!("<part{p}of{total}.tok@bzh29>"),
                part: Some(p),
                ..wanted("", "").row
            })
            .map(|mut r| {
                r.total_parts = Some(total);
                r
            })
            .collect()
    }

    #[test]
    fn a_round_asks_the_most_nearly_whole_first_and_marks_the_rest() {
        let now = chrono::Utc::now().timestamp();
        let at =
            |ago: i64| chrono::DateTime::from_timestamp(now - ago, 0).unwrap().format("%Y-%m-%d %H:%M:%S").to_string();
        let recent = at(3 * 3600);
        let fresh = at(600);
        let old =
            chrono::DateTime::from_timestamp(now - 30 * 86_400, 0).unwrap().format("%Y-%m-%d %H:%M:%S").to_string();
        let big = 1 << 30;
        let (_dir, conn) = shards(&[
            (1, big, &recent, false), // 900: asked second
            (2, big, &recent, false), // 990: asked first
            (3, big, &recent, false), // 300: too little of it
            (4, big, &recent, false), // 950, but no pattern
            (5, big, &recent, true),  // complete
            (6, 1, &recent, false),   // too small
            (7, big, &old, false),    // too old
            (8, big, &recent, false), // asked lately
            (9, big, &fresh, false),  // may still be arriving
        ]);
        conn.execute("insert into inferred_asked values (8, ?1)", [now + 100]).unwrap();
        let permille =
            |id: i64| [(1, 900), (2, 990), (3, 300), (4, 950), (8, 999)].iter().find(|p| p.0 == id).unwrap().1;
        let articles = |row: &ReleaseRow| {
            let a = match row.id {
                1 => pattern(6, &[1, 2, 4, 6]),
                2 => pattern(4, &[1, 2, 3]),
                4 => vec![wanted("<x@y>", "").row, wanted("<z@y>", "").row],
                _ => return Err(rusqlite::Error::QueryReturnedNoRows),
            };
            Ok((a, vec!["alt.binaries.movies".to_string()]))
        };
        // 1 and 11 are one post
        let post = |r: &ReleaseRow| Ok((if r.id == 1 { vec![1, 11] } else { vec![r.id] }, permille(r.id), 1 << 30));
        let p = plan(&conn, now, 100, post, articles).unwrap();
        let asked: Vec<(i64, &str)> = p.wanted.iter().map(|w| (w.release_id, w.row.message_id.as_str())).collect();
        assert_eq!(asked, [(2, "<part4of4.tok@bzh29>"), (1, "<part3of6.tok@bzh29>"), (1, "<part5of6.tok@bzh29>")]);
        assert!(p.wanted.iter().all(|w| w.group == "alt.binaries.movies"));
        assert!(p.waiting.is_empty());
        let mut asked = p.asked.clone();
        asked.sort();
        assert_eq!(asked, [1, 2, 11]);
        let mut nothing = p.nothing.clone();
        nothing.sort();
        assert_eq!(nothing, [3, 4]);

        // the budget ends in 1: it's asked in part, the rest next round
        let p = plan(&conn, now, 2, post, articles).unwrap();
        let asked: Vec<&str> = p.wanted.iter().map(|w| w.row.message_id.as_str()).collect();
        assert_eq!(asked, ["<part4of4.tok@bzh29>", "<part3of6.tok@bzh29>"]);
        // 1 isn't asked whole: it isn't marked
        assert_eq!(p.asked, [2]);

        // ids asked lately wait: 1's part 3 is skipped, and once part 5 waits too, the post waits for the first of them
        let mut conn = conn;
        mark_ids(&mut conn, &[("part3of6.tok@bzh29".into(), Outcome::Absent)], now).unwrap();
        let p = plan(&conn, now, 100, post, articles).unwrap();
        let asked: Vec<&str> = p.wanted.iter().map(|w| w.row.message_id.as_str()).collect();
        assert_eq!(asked, ["<part4of4.tok@bzh29>", "<part5of6.tok@bzh29>"]);
        mark_ids(&mut conn, &[("part5of6.tok@bzh29".into(), Outcome::Failed)], now).unwrap();
        let p = plan(&conn, now, 100, post, articles).unwrap();
        assert_eq!(p.waiting, [(vec![1, 11], now + FAILED_AGAIN)]);

        // a stop interrupting a read ends the plan, and marks nothing
        let stopped = |_: &ReleaseRow| -> rusqlite::Result<(Vec<ArticleRow>, Vec<String>)> {
            Err(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_INTERRUPT), None))
        };
        assert!(plan(&conn, now, 100, post, stopped).is_err());
    }
}
