//! Parts no listing showed, asked for by the ids their file's own articles name. An upload whose ids run
//! `part{N}of{T}.{token}@domain` (`nzb::inferred_segments`) tells us each missing part's id, and an id needs no
//! listing: every server is asked for its headers (the same all-servers read a body takes, which calls an article
//! missing only when each one says so), and what turns up is saved like any other header, soo the part joins its
//! file and counts towards whole. A release whose parts every server lacks is asked again later, less and less often.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use rusqlite::{Connection, params};

use crate::indexer::{Db, PassContext, on_db};
use crate::nntp::{BackgroundBody, BackgroundRead, Overview, Pool};
use crate::search::{ArticleRow, ReleaseRow};
use crate::store;

/// how often a round looks for parts to ask for
const EVERY: Duration = Duration::from_secs(300);
/// releases this big or bigger: smaller posts aren't worth the reads
const MIN_BYTES: i64 = 64 << 20;
/// posted this recently: past it the parts a server lacks are rarely anywhere
const DAYS: i64 = 7;
/// releases looked at per shard per round, newest first among those due
const CANDIDATES: i64 = 500;
/// a post less whole than this (in thousandths) isn't one a few parts make whole, nor one whose ids prove much
const MIN_PERMILLE: i64 = 500;
/// ids asked for per round, and at once
const PER_ROUND: usize = 3_000;
const AT_ONCE: usize = 8;
/// an article's headers, more than any real one has
const HEAD_BYTES: usize = 64 << 10;
/// a release with nothing to ask for is looked at again after this: more parts may have come and prove its ids
const NOTHING_AGAIN: i64 = 86_400;
/// one whose parts weren't anywhere: after this, doubling with each try, up to `LONGEST`
const ABSENT_AGAIN: i64 = 6 * 3600;
const LONGEST: i64 = 2 * 86_400;
/// one where a server couldn't be asked, or what was found couldn't be saved
const FAILED_AGAIN: i64 = 1800;

const CREATE: &str = "create table if not exists inferred_asked (
        release_id INTEGER PRIMARY KEY, at INTEGER NOT NULL, next_at INTEGER NOT NULL, tries INTEGER NOT NULL,
        found INTEGER NOT NULL, absent INTEGER NOT NULL)";

/// One part to ask for: its post (by the release it was found by), the group it's saved in unless its headers say
/// another, and the row `nzb::inferred_segments` made for it.
#[derive(Clone, Debug)]
pub struct Wanted {
    pub release_id: i64,
    pub group: String,
    pub row: ArticleRow,
}

/// A round's plan: the parts to ask for, in order; every row of each post they're asked for (keyed by the release it
/// was found by); and the rows looked at that had nothing to ask (to look at again later).
#[derive(Debug, Default)]
pub struct Plan {
    pub wanted: Vec<Wanted>,
    pub posts: HashMap<i64, Vec<i64>>,
    pub nothing: Vec<i64>,
}

/// What asking found for one post. `unusable`: a server answered with headers that aren't a listing row (none of the
/// article's, no subject, too big), which asking again won't change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub found: i64,
    pub absent: i64,
    pub unusable: i64,
    pub failed: i64,
}

impl Tally {
    fn answered(&self) -> i64 {
        self.found + self.absent + self.unusable + self.failed
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
                let ids = match crate::release_info::whole_post_with(conn, row) {
                    Ok(Some(post)) => post.ids,
                    _ => vec![row.id],
                };
                let permille = crate::release_info::permille_of(conn, &ids).unwrap_or(0);
                (ids, permille)
            };
            let articles =
                |row: &ReleaseRow| crate::nzb::inventory_with(conn, row).ok().map(|(articles, _, _)| articles);
            plan(conn, now, PER_ROUND, post, articles)
        })
    })
    .await??;
    let Plan { wanted, posts, nothing } = plan;
    on_db(db, move |conn| Ok(mark_nothing(conn, &nothing, now)?)).await?;
    if wanted.is_empty() {
        return Ok(());
    }
    let mut planned: HashMap<i64, i64> = HashMap::new();
    for w in &wanted {
        *planned.entry(w.release_id).or_default() += 1;
    }
    // asked a few at once, each across every server
    let mut tallies: BTreeMap<i64, Tally> = BTreeMap::new();
    let mut found: BTreeMap<String, Vec<(i64, Overview)>> = BTreeMap::new();
    let mut set = tokio::task::JoinSet::new();
    let mut queue = wanted.into_iter();
    loop {
        while set.len() < AT_ONCE && !stopping() {
            let Some(w) = queue.next() else { break };
            let pool = pool.clone();
            set.spawn(async move {
                let id = format!("<{}>", w.row.message_id.trim().trim_matches(['<', '>']));
                let got = pool.background_read(&id, HEAD_BYTES, BackgroundRead::Head).await;
                (w, got)
            });
        }
        // a stop drops the reads in flight (the set aborts them), and their posts are asked again next time
        let Some(Some(done)) = crate::nntp::unless_stopped(stop, set.join_next()).await else { break };
        let Ok((w, got)) = done else { continue };
        let tally = tallies.entry(w.release_id).or_default();
        match got {
            Ok(BackgroundBody::Body(head)) => match overview_of(&head, &w) {
                Some((group, o)) => {
                    tally.found += 1;
                    found.entry(group).or_default().push((w.release_id, o));
                }
                None => tally.unusable += 1,
            },
            Ok(BackgroundBody::TooBig) => tally.unusable += 1,
            Ok(BackgroundBody::Missing) => tally.absent += 1,
            Err(_) => tally.failed += 1,
        }
    }
    // saved like a listing's headers: each part joins its file by its subject. A group that couldn't be saved has its
    // posts asked again soon; the other groups are saved all the same
    for (group, headers) in found {
        let ids: HashSet<i64> = headers.iter().map(|(id, _)| *id).collect();
        let headers: Vec<Overview> = headers.into_iter().map(|(_, o)| o).collect();
        let unsaved = pool.reserve_unsaved_shard(store::shard_of(&group), headers.len() as u64).await;
        if let Err(e) = crate::indexer::save_slice(pool, db, &group, headers, unsaved).await {
            eprintln!("inferred: saving {group}: {e:#}");
            for id in ids {
                tallies.entry(id).or_default().failed += 1;
            }
        }
    }
    let total = tallies.values().fold(Tally::default(), |t, x| Tally {
        found: t.found + x.found,
        absent: t.absent + x.absent,
        unusable: t.unusable + x.unusable,
        failed: t.failed + x.failed,
    });
    for (outcome, n) in
        [("found", total.found), ("absent", total.absent), ("unusable", total.unusable), ("failed", total.failed)]
    {
        crate::metrics::names::INFERRED_PARTS.inc(&[("outcome", outcome)], n.max(0) as u64);
    }
    println!(
        "[inferred] asked {} posts' parts: {} found, {} on no server, {} unusable, {} couldnt be asked or saved",
        tallies.len(),
        total.found,
        total.absent,
        total.unusable,
        total.failed
    );
    // a post cut short by a stop isn't marked: all its parts are asked next time
    let marks: Vec<(Vec<i64>, Tally)> = tallies
        .into_iter()
        .filter(|(id, t)| t.failed > 0 || t.answered() >= planned.get(id).copied().unwrap_or(0))
        .map(|(id, t)| (posts.get(&id).cloned().unwrap_or_else(|| vec![id]), t))
        .collect();
    on_db(db, move |conn| Ok(mark_asked(conn, &marks, now)?)).await?;
    Ok(())
}

/// The parts worth asking for this round, most nearly whole posts first, `budget` ids at most; and the releases
/// looked at that had none. `conn` has the shards attached and `inferred_asked` in main; `post` gives a release's
/// post (its rows) and how whole it is in thousandths, `articles` the articles its NZB is made of (None: they
/// couldn't be read).
pub fn plan(
    conn: &Connection,
    now: i64,
    budget: usize,
    post: impl Fn(&ReleaseRow) -> (Vec<i64>, i64),
    articles: impl Fn(&ReleaseRow) -> Option<Vec<ArticleRow>>,
) -> rusqlite::Result<Plan> {
    let has_table = conn.prepare("select 1 from main.sqlite_master where name = 'inferred_asked'")?.exists([])?;
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
             where r.complete = 0 and r.size >= ?1 and r.posted_date >= datetime(?2, 'unixepoch')
               and not exists (select 1 from main.inferred_asked a where a.release_id = r.id and a.next_at > ?3)
             order by r.id desc limit ?4"
        );
        let rows = crate::repair::release_rows(conn, &sql, params![MIN_BYTES, now - DAYS * 86_400, now, CANDIDATES])?;
        for row in rows {
            if !seen.insert(row.id) {
                continue;
            }
            let (mut ids, permille) = post(&row);
            if !ids.contains(&row.id) {
                ids.push(row.id);
            }
            seen.extend(ids.iter().copied());
            if (MIN_PERMILLE..1000).contains(&permille) {
                posts.push((permille, row.size.unwrap_or(0), row, ids));
            } else {
                out.nothing.extend(ids);
            }
        }
    }
    posts.sort_by_key(|(permille, size, _, _)| (std::cmp::Reverse(*permille), std::cmp::Reverse(*size)));
    for (_, _, row, ids) in posts {
        if out.wanted.len() >= budget {
            break;
        }
        let rows: Vec<ArticleRow> = articles(&row)
            .map(|a| crate::nzb::inferred_segments(&a).into_iter().flat_map(|f| f.rows).collect())
            .unwrap_or_default();
        if rows.is_empty() {
            out.nothing.extend(ids);
            continue;
        }
        // a post is asked whole or not at all this round, soo its tally says what became of all its parts
        if !out.wanted.is_empty() && out.wanted.len() + rows.len() > budget {
            break;
        }
        out.wanted.extend(rows.into_iter().map(|r| Wanted {
            release_id: row.id,
            group: row.group_name.clone(),
            row: r,
        }));
        out.posts.insert(row.id, ids);
    }
    Ok(out)
}

/// The listing row a server's headers for `w` make, and the group to save it in: `w`'s, unless the headers' `Xref:`
/// names others and not it (a post cut across groups, its file in another of them). None when they aren't the article
/// asked for. Its size is what the headers say (`Bytes:`, or `Lines:`), else the size `w` was given.
pub fn overview_of(head: &[u8], w: &Wanted) -> Option<(String, Overview)> {
    let value = |name: &str| crate::spotnet::header::header_value(head, name);
    let message_id = value("Message-ID")?;
    if message_id.trim().trim_matches(['<', '>']) != w.row.message_id.trim().trim_matches(['<', '>']) {
        return None;
    }
    let subject = value("Subject")?;
    let number_of = |n: Option<String>| n.and_then(|n| n.trim().parse::<i64>().ok()).filter(|n| *n > 0);
    let bytes = number_of(value("Bytes")).unwrap_or(0);
    let lines = number_of(value("Lines")).unwrap_or(0);
    let xref = value("Xref").unwrap_or_default();
    let numbers: Vec<(&str, &str)> = xref.split_whitespace().skip(1).filter_map(|e| e.split_once(':')).collect();
    let group = match numbers.iter().find(|(g, _)| *g == w.group).or(numbers.first()) {
        Some((g, _)) => g.to_string(),
        None => w.group.clone(),
    };
    let groups = numbers.len();
    let row = Overview {
        // the answering server's numbers aren't the listing's: none (0, which the store passes over)
        number: 0,
        subject,
        from: value("From").unwrap_or_default(),
        date: value("Date").unwrap_or_default(),
        message_id: format!("<{}>", message_id.trim().trim_matches(['<', '>'])),
        references: value("References").unwrap_or_default(),
        bytes: if bytes == 0 && lines == 0 { w.row.bytes.unwrap_or(0) } else { bytes },
        lines,
        xref_groups: (groups > 0).then(|| u16::try_from(groups).unwrap_or(u16::MAX)),
    };
    Some((group, row))
}

/// When a release that was asked should be asked again, by what came of it.
pub fn next_try(now: i64, tries: i64, tally: Tally) -> i64 {
    if tally.failed > 0 {
        return now + FAILED_AGAIN;
    }
    let shift = tries.clamp(1, 16) - 1;
    now + (ABSENT_AGAIN << shift).min(LONGEST)
}

fn mark_nothing(conn: &mut Connection, ids: &[i64], now: i64) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "insert into inferred_asked (release_id, at, next_at, tries, found, absent) values (?1, ?2, ?3, 0, 0, 0)
             on conflict(release_id) do update set at = ?2, next_at = ?3",
        )?;
        for id in ids {
            stmt.execute(params![id, now, now + NOTHING_AGAIN])?;
        }
    }
    tx.commit()
}

fn mark_asked(conn: &mut Connection, marks: &[(Vec<i64>, Tally)], now: i64) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    for (ids, tally) in marks {
        // the post's tries so far: the most any of its rows has
        let mut tries: i64 = 0;
        let mut found: i64 = 0;
        for id in ids {
            if let Ok((t, f)) =
                tx.query_row("select tries, found from inferred_asked where release_id = ?1", [id], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                })
            {
                tries = tries.max(t);
                found = found.max(f);
            }
        }
        // only an answer from every server counts as a try; one that couldn't be asked is asked again soon
        let tries = tries + i64::from(tally.failed == 0);
        let next = next_try(now, tries, *tally);
        let mut stmt = tx.prepare_cached(
            "insert or replace into inferred_asked (release_id, at, next_at, tries, found, absent)
             values (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for id in ids {
            stmt.execute(params![id, now, next, tries, found + tally.found, tally.absent])?;
        }
    }
    tx.commit()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wanted(id: &str, group: &str) -> Wanted {
        Wanted {
            release_id: 1,
            group: group.into(),
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
        // a post cut across groups: saved in the group the article is in
        let (group, o) = overview_of(HEAD, &wanted("<part3of6.tok@bzh29>", "alt.binaries.teevee")).unwrap();
        assert_eq!((group.as_str(), o.number), ("alt.binaries.movies", 0));
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
    fn a_release_on_no_server_is_asked_again_less_often() {
        let absent = Tally { absent: 3, ..Tally::default() };
        assert_eq!(next_try(0, 1, absent), ABSENT_AGAIN);
        assert_eq!(next_try(0, 2, absent), 2 * ABSENT_AGAIN);
        assert_eq!(next_try(0, 3, absent), 4 * ABSENT_AGAIN);
        assert_eq!(next_try(0, 9, absent), LONGEST);
        assert_eq!(next_try(0, 2, Tally { failed: 1, ..absent }), FAILED_AGAIN);
    }

    #[test]
    fn asked_posts_keep_their_tries_and_counts_on_every_row() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(CREATE).unwrap();
        mark_nothing(&mut conn, &[7], 100).unwrap();
        let row = |conn: &Connection, id: i64| -> (i64, i64, i64, i64) {
            conn.query_row(
                "select next_at, tries, found, absent from inferred_asked where release_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
        };
        assert_eq!(row(&conn, 7), (100 + NOTHING_AGAIN, 0, 0, 0));
        // a post of two rows: both are marked alike
        let ask = |tally: Tally| vec![(vec![7, 9], tally)];
        mark_asked(&mut conn, &ask(Tally { found: 2, absent: 1, ..Tally::default() }), 200).unwrap();
        assert_eq!(row(&conn, 7), (200 + ABSENT_AGAIN, 1, 2, 1));
        assert_eq!(row(&conn, 9), row(&conn, 7));
        // headers that can't be used are an answer too, and back off like absent parts
        mark_asked(&mut conn, &ask(Tally { absent: 1, unusable: 1, ..Tally::default() }), 300).unwrap();
        assert_eq!(row(&conn, 7), (300 + 2 * ABSENT_AGAIN, 2, 2, 1));
        // a server that couldn't be asked isn't a try
        mark_asked(&mut conn, &ask(Tally { failed: 1, ..Tally::default() }), 400).unwrap();
        assert_eq!(row(&conn, 9), (400 + FAILED_AGAIN, 2, 2, 0));
        // looked at again with nothing to ask keeps its tries
        mark_nothing(&mut conn, &[7], 500).unwrap();
        assert_eq!(row(&conn, 7).1, 2);
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
        let recent = chrono::DateTime::from_timestamp(now - 3600, 0).unwrap().format("%Y-%m-%d %H:%M:%S").to_string();
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
        ]);
        conn.execute("insert into inferred_asked values (8, 0, ?1, 1, 0, 0)", [now + 100]).unwrap();
        let permille =
            |id: i64| [(1, 900), (2, 990), (3, 300), (4, 950), (8, 999)].iter().find(|p| p.0 == id).unwrap().1;
        let articles = |row: &ReleaseRow| {
            Some(match row.id {
                1 => pattern(6, &[1, 2, 4, 6]),
                2 => pattern(4, &[1, 2, 3]),
                4 => vec![wanted("<x@y>", "").row, wanted("<z@y>", "").row],
                _ => return None,
            })
        };
        // 1 and 11 are one post
        let post = |r: &ReleaseRow| (if r.id == 1 { vec![1, 11] } else { vec![r.id] }, permille(r.id));
        let p = plan(&conn, now, 100, post, articles).unwrap();
        let asked: Vec<(i64, &str)> = p.wanted.iter().map(|w| (w.release_id, w.row.message_id.as_str())).collect();
        assert_eq!(asked, [(2, "<part4of4.tok@bzh29>"), (1, "<part3of6.tok@bzh29>"), (1, "<part5of6.tok@bzh29>")]);
        assert!(p.wanted.iter().all(|w| w.group == "alt.binaries.movies"));
        assert_eq!(p.posts[&1], [1, 11]);
        assert_eq!(p.posts[&2], [2]);
        let mut nothing = p.nothing.clone();
        nothing.sort();
        assert_eq!(nothing, [3, 4]);

        // a budget too small for the next release stops the round there, and leaves it unmarked: 2's one part fits,
        // 4 (looked at before 1) has nothing, and 1's two parts don't fit
        let p = plan(&conn, now, 2, post, articles).unwrap();
        assert_eq!(p.wanted.iter().map(|w| w.release_id).collect::<Vec<_>>(), [2]);
        let mut nothing = p.nothing.clone();
        nothing.sort();
        assert_eq!(nothing, [3, 4]);
    }
}
