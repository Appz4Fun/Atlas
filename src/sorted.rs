//! A sort over everything a page covers, read from the database in the column's own order: the top of a sort is the
//! top of the whole category, not of its newest few thousand releases.
//!
//! Each shard gives its own top rows, the cheapest way it can: a small category is read whole and sorted, a big one
//! walks the column's index and keeps the rows in the category. Then the shards' tops are merged.

use std::cmp::Ordering;
use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension};

use crate::category::{CatFilter, TABLE, UNCATEGORIZED, label};
use crate::db::{self, Result};
use crate::field_search::{NAME_INDEX, POSTED_INDEX, SIZE_INDEX, has_index};
use crate::release_info::LINKED_ROWS;
use crate::search::{Bounds, COLUMNS, ReleaseRow, VISIBLE_RELEASE, newest_in_categories_with, probe_sql, release_row};
use crate::store;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Column {
    Size,
    Posted,
    Name,
    Group,
    Category,
}

impl Column {
    /// What the rows are ordered by, as SQL on `r`. Each has its index, as written in it.
    fn expr(self) -> &'static str {
        match self {
            Column::Size => "r.size",
            Column::Posted => "r.posted_date",
            Column::Name => "lower(coalesce(r.display_name, r.name))",
            Column::Group | Column::Category => "r.group_name",
        }
    }

    /// Rows with a value, as SQL that walks the index: an empty posted date is none, as the page counts it.
    fn valued(self) -> String {
        match self {
            Column::Posted => "r.posted_date > ''".into(),
            _ => format!("{} is not null", self.expr()),
        }
    }

    /// Rows with no value, as SQL that walks the index.
    fn unvalued(self) -> String {
        match self {
            Column::Posted => "(r.posted_date is null or r.posted_date = '')".into(),
            _ => format!("{} is null", self.expr()),
        }
    }

    fn index(self) -> &'static str {
        match self {
            Column::Size => SIZE_INDEX,
            Column::Posted => POSTED_INDEX,
            Column::Name => NAME_INDEX,
            Column::Group | Column::Category => "idx_release_group",
        }
    }
}

/// When a shard reads a category whole instead of walking an index.
#[derive(Clone, Copy)]
struct Limits {
    /// at most this many releases of a category in a shard, and they are all read and sorted
    few: i64,
    /// up to this many, reading them all is what's left when walking the column's index finds them too seldom
    some: i64,
    /// index entries a walk may pass besides those the category's share says it should
    slack: i64,
}

const LIMITS: Limits = Limits { few: 20_000, some: 200_000, slack: 50_000 };

/// How long a look for the indexes is believed: `--search-indexes` adds them while Atlas runs.
const INDEXES_CHECKED_FOR: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether every shard has the index a sort by `by` walks. Without it, ranking a big category reads and sorts all of
/// it on every click, soo the page sorts its newest releases instead until `--search-indexes --apply` has run.
pub fn ready(by: Column) -> bool {
    use std::sync::{LazyLock, Mutex};
    use std::time::Instant;
    // the category sort reads each category newest first, and the group index is made with the shard
    if matches!(by, Column::Category | Column::Group) {
        return true;
    }
    static CHECKED: LazyLock<Mutex<HashMap<Column, (Instant, bool)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
    let mut checked = CHECKED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&(at, ready)) = checked.get(&by)
        && at.elapsed() < INDEXES_CHECKED_FOR
    {
        return ready;
    }
    let ready = db::open().is_ok_and(|conn| all_indexed(&conn, by));
    checked.insert(by, (Instant::now(), ready));
    ready
}

fn all_indexed(conn: &Connection, by: Column) -> bool {
    (0..store::SHARDS).all(|s| has_index(conn, s, by.index()))
}

/// The first `want` releases in `filter` by `by`, over all of them. Releases with nothing in the column come last
/// either way, and among equals the newest comes first.
pub fn top(filter: Option<&CatFilter>, by: Column, descending: bool, want: i64) -> Result<Vec<ReleaseRow>> {
    if want <= 0 || filter.is_some_and(|f| f.leaves.is_empty() && !f.uncategorized) {
        return Ok(Vec::new());
    }
    crate::metrics::timed("search.sorted", || top_with(&db::open, filter, by, descending, want, LIMITS))
}

/// A shard's first rows, and the whole size of those that are part of a linked upload.
type ShardTop = (Vec<ReleaseRow>, HashMap<i64, i64>);

/// `top`, on connections from `open`: one for each shard, read at once.
fn top_with(
    open: &(dyn Fn() -> Result<Connection> + Sync),
    filter: Option<&CatFilter>,
    by: Column,
    descending: bool,
    want: i64,
    limits: Limits,
) -> Result<Vec<ReleaseRow>> {
    if by == Column::Category {
        return by_category(&open()?, filter, descending, want);
    }
    // on a cold disk most of the time is waiting for reads
    let tops: Vec<Result<ShardTop>> = std::thread::scope(|scope| {
        let shards: Vec<_> = (0..store::SHARDS)
            .map(|s| {
                scope.spawn(move || {
                    let conn = open()?;
                    let mut rows = shard_top(&conn, s, filter, by, descending, want, limits)?;
                    let whole = if by == Column::Size && descending {
                        linked_sizes(&conn, s, filter, &mut rows, want)?
                    } else {
                        HashMap::new()
                    };
                    Ok((rows, whole))
                })
            })
            .collect();
        shards.into_iter().map(|t| t.join().unwrap_or_else(|e| std::panic::resume_unwind(e))).collect()
    });
    let (mut rows, mut whole) = (Vec::new(), HashMap::new());
    for top in tops {
        let (top, sizes) = top?;
        rows.extend(top);
        whole.extend(sizes);
    }
    rows.sort_by(|a, b| compare(a, b, by, descending, &whole));
    rows.truncate(usize::try_from(want).unwrap_or(usize::MAX));
    // the size it was ranked by, which is the one the page shows: a row the page hasn't worked out yet sorts by it too
    for row in &mut rows {
        if let Some(&total) = whole.get(&row.id) {
            row.size = Some(total);
        }
    }
    Ok(rows)
}

/// The order `top` gives, on rows already read. `whole`: the size of the linked upload a row is part of, which the
/// page shows for it and sorts it by.
fn compare(a: &ReleaseRow, b: &ReleaseRow, by: Column, descending: bool, whole: &HashMap<i64, i64>) -> Ordering {
    let size = |r: &ReleaseRow| whole.get(&r.id).copied().or(r.size);
    fn last_if_none<T: Ord>(a: Option<T>, b: Option<T>, descending: bool) -> Ordering {
        match (a, b) {
            (Some(x), Some(y)) if descending => y.cmp(&x),
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    }
    let by_value = match by {
        Column::Size => last_if_none(size(a), size(b), descending),
        Column::Posted => last_if_none(
            a.posted_date.as_deref().filter(|d| !d.is_empty()),
            b.posted_date.as_deref().filter(|d| !d.is_empty()),
            descending,
        ),
        // as SQLite's lower(), which only knows ASCII, soo the order is the one the database picked the rows in
        Column::Name => last_if_none(Some(a.name.to_ascii_lowercase()), Some(b.name.to_ascii_lowercase()), descending),
        Column::Group | Column::Category => last_if_none(Some(&a.group_name), Some(&b.group_name), descending),
    };
    by_value.then(b.id.cmp(&a.id))
}

/// One shard's first `want`.
fn shard_top(
    conn: &Connection,
    s: usize,
    filter: Option<&CatFilter>,
    by: Column,
    descending: bool,
    want: i64,
    limits: Limits,
) -> Result<Vec<ReleaseRow>> {
    let indexed = has_index(conn, s, by.index());
    let leaves = filter.filter(|f| !f.uncategorized).map(|f| f.leaves.as_slice());
    let Some(leaves) = leaves else {
        // everything, or what has no category too: only the index can tell what's first without reading it all
        return if indexed {
            walk(conn, s, filter, by, descending, want, None).map(Option::unwrap_or_default)
        } else {
            read_all(conn, s, filter, by, descending, want)
        };
    };
    let in_category = count_in(conn, s, leaves, limits.some.saturating_add(1))?;
    if in_category <= limits.few || !indexed {
        return read_all(conn, s, filter, by, descending, want);
    }
    // as far down the index as the category's share of the shard says, and then some
    let total: i64 = conn
        .query_row(&format!("select value from s{s}.meta where key = 'releases'"), [], |r| r.get(0))
        .optional()?
        .unwrap_or(0);
    let expected = want.saturating_mul(total.max(in_category)) / in_category.max(1);
    let budget = expected.saturating_mul(4).saturating_add(limits.slack);
    match walk(conn, s, filter, by, descending, want, Some(budget))? {
        Some(rows) => Ok(rows),
        None if in_category <= limits.some => read_all(conn, s, filter, by, descending, want),
        None => walk(conn, s, filter, by, descending, want, None).map(Option::unwrap_or_default),
    }
}

/// How many releases of `leaves` the shard has, counting no further than `cap`.
fn count_in(conn: &Connection, s: usize, leaves: &[i64], cap: i64) -> Result<i64> {
    let sql = format!(
        "select count(*) from (select 1 from s{s}.release_category where category in ({}) limit ?1)",
        list(leaves)
    );
    conn.query_row(&sql, [cap], |r| r.get(0))
}

/// ids come from the category table, never from the request, soo inlining them is safe
fn list(leaves: &[i64]) -> String {
    if leaves.is_empty() { "null".into() } else { leaves.iter().map(i64::to_string).collect::<Vec<_>>().join(",") }
}

/// Every release in `filter` read and sorted by SQLite, which keeps only the top `want` as it goes.
fn read_all(
    conn: &Connection,
    s: usize,
    filter: Option<&CatFilter>,
    by: Column,
    descending: bool,
    want: i64,
) -> Result<Vec<ReleaseRow>> {
    let (from, clause) = match filter {
        None => (format!("s{s}.releases r"), String::from("1")),
        // the category table first: a category's rows, not every release's
        Some(f) if !f.uncategorized => (
            format!("s{s}.release_category c cross join s{s}.releases r on r.id = c.release_id"),
            format!("c.category in ({})", list(&f.leaves)),
        ),
        Some(f) => (
            format!("s{s}.releases r left join s{s}.release_category c on c.release_id = r.id"),
            format!("(c.category in ({}) or c.release_id is null)", list(&f.leaves)),
        ),
    };
    let (expr, order, unvalued) = (by.expr(), if descending { "desc" } else { "asc" }, by.unvalued());
    let sql = format!(
        "select {COLUMNS} from {from} where {VISIBLE_RELEASE} and {clause}
         order by {unvalued}, {expr} {order}, r.id desc limit ?1"
    );
    conn.prepare(&sql)?.query_map([want], release_row)?.collect()
}

/// Down the column's index, keeping the rows in `filter` that a client may see, until there are `want`. None: more
/// than `budget` index entries went by first.
fn walk(
    conn: &Connection,
    s: usize,
    filter: Option<&CatFilter>,
    by: Column,
    descending: bool,
    want: i64,
    budget: Option<i64>,
) -> Result<Option<Vec<ReleaseRow>>> {
    let (expr, index, order) = (by.expr(), by.index(), if descending { "desc" } else { "asc" });
    // the newest first among equals, before the walk stops: going up the index they come oldest first, and SQLite
    // sorts each run of equals by itself as the walk reaches it
    let valued = format!(
        "select r.id from s{s}.releases r indexed by {index} where {} order by {expr} {order}, r.id desc",
        by.valued()
    );
    let unvalued =
        format!("select r.id from s{s}.releases r indexed by {index} where {} order by r.id desc", by.unvalued());
    let mut category = conn.prepare(&format!("select category from s{s}.release_category where release_id = ?1"))?;
    let mut probe = conn.prepare(&probe_sql(s))?;
    let want = usize::try_from(want).unwrap_or(usize::MAX);
    let (mut rows, mut steps) = (Vec::new(), 0i64);
    for sql in [valued, unvalued] {
        let mut stmt = conn.prepare(&sql)?;
        let mut ids = stmt.query([])?;
        while let Some(id) = ids.next()? {
            let id: i64 = id.get(0)?;
            steps += 1;
            if budget.is_some_and(|b| steps > b) {
                return Ok(None);
            }
            if let Some(f) = filter {
                let kept = match category.query_row([id], |r| r.get::<_, i64>(0)).optional()? {
                    Some(c) => f.leaves.contains(&c),
                    None => f.uncategorized,
                };
                if !kept {
                    continue;
                }
            }
            if let Some(row) = probe.query_row([id], release_row).optional()? {
                rows.push(row);
                if rows.len() >= want {
                    return Ok(Some(rows));
                }
            }
        }
    }
    Ok(Some(rows))
}

/// A linked upload, posted a file at a time, is one row on the page with the size of all its files, and a size sort
/// goes by that. Going down, its pieces can each come after the shard's last row while the whole comes before it, soo
/// the biggest such uploads are read in as well; going up, every piece comes before its whole, soo it's in already.
/// Gives the whole size of each row read that is part of a linked upload. Uploads matched by name across groups are
/// still put together only on the page.
fn linked_sizes(
    conn: &Connection,
    s: usize,
    filter: Option<&CatFilter>,
    rows: &mut Vec<ReleaseRow>,
    want: i64,
) -> Result<HashMap<i64, i64>> {
    // a shard that hasn't got the table yet has linked nothing
    let Ok(mut lead_of) = conn.prepare(&format!("select lead_id from s{s}.upload_link where release_id = ?1")) else {
        return Ok(HashMap::new());
    };
    // as linked_post_with counts it: its lead still there, and neither one row nor more than a set has
    let total_sql = format!(
        "select sum(coalesce(r.size, 0)) from s{s}.upload_link l join s{s}.releases r on r.id = l.release_id
         where l.lead_id = ?1 and exists (select 1 from s{s}.releases x where x.id = ?1)
         having count(*) between 2 and {LINKED_ROWS}"
    );
    let mut total_of = conn.prepare(&total_sql)?;
    // the shard read its first `want` and there were more: what's as big as its last row as a whole or bigger can come
    // first, a newer one of the same size too
    let cutoff = match rows.last() {
        Some(last) if rows.len() as i64 >= want => last.size,
        _ => None,
    };
    if let Some(cutoff) = cutoff {
        // only uploads with a row the page can list count towards the limit: bigger ones elsewhere don't crowd them out.
        // A tie goes to the newest of those rows, as the rows' own ties do, not to a row the page leaves out
        let clause = match filter {
            None => String::from("1"),
            Some(f) if !f.uncategorized => format!("c.category in ({})", list(&f.leaves)),
            Some(f) => format!("(c.category in ({}) or c.release_id is null)", list(&f.leaves)),
        };
        let bigger = format!(
            "select l.lead_id,
                    (select max(r.id) from s{s}.upload_link m join s{s}.releases r on r.id = m.release_id
                     left join s{s}.release_category c on c.release_id = r.id
                     where m.lead_id = l.lead_id and {VISIBLE_RELEASE} and {clause}) as newest
             from s{s}.upload_link l join s{s}.releases r on r.id = l.release_id
             group by l.lead_id
             having count(*) between 2 and {LINKED_ROWS} and sum(coalesce(r.size, 0)) >= ?1
                and exists (select 1 from s{s}.releases x where x.id = l.lead_id)
                and newest is not null
             order by sum(coalesce(r.size, 0)) desc, newest desc limit ?2"
        );
        let leads: Vec<i64> = conn.prepare(&bigger)?.query_map([cutoff, want], |r| r.get(0))?.collect::<Result<_>>()?;
        let mut members = conn.prepare(&format!("select release_id from s{s}.upload_link where lead_id = ?1"))?;
        let mut category =
            conn.prepare(&format!("select category from s{s}.release_category where release_id = ?1"))?;
        let mut probe = conn.prepare(&probe_sql(s))?;
        let mut have: std::collections::HashSet<i64> = rows.iter().map(|r| r.id).collect();
        // biggest first: once `want` rows are in, the uploads after them come after them on the page too, soo reading
        // their rows (up to a set each) would only make the shard's answer longer before it's cut
        let mut added = 0i64;
        for lead in leads {
            if added >= want {
                break;
            }
            let ids: Vec<i64> = members.query_map([lead], |r| r.get(0))?.collect::<Result<_>>()?;
            for id in ids {
                if !have.insert(id) {
                    continue;
                }
                if let Some(f) = filter {
                    let kept = match category.query_row([id], |r| r.get::<_, i64>(0)).optional()? {
                        Some(c) => f.leaves.contains(&c),
                        None => f.uncategorized,
                    };
                    if !kept {
                        continue;
                    }
                }
                if let Some(row) = probe.query_row([id], release_row).optional()? {
                    rows.push(row);
                    added += 1;
                }
            }
        }
    }
    let (mut whole, mut totals) = (HashMap::new(), HashMap::new());
    for row in rows.iter() {
        let Some(lead) = lead_of.query_row([row.id], |r| r.get::<_, i64>(0)).optional()? else { continue };
        let total = match totals.get(&lead) {
            Some(&total) => total,
            None => {
                let total: Option<i64> = total_of.query_row([lead], |r| r.get(0)).optional()?.flatten();
                totals.insert(lead, total);
                total
            }
        };
        if let Some(total) = total {
            whole.insert(row.id, total);
        }
    }
    Ok(whole)
}

/// A category read as far back as it goes: a sort over all of it can't stop at its newest releases.
const WHOLE: Bounds = Bounds { rejects: usize::MAX, no_row: i64::MAX, ..Bounds::DEFAULT };

/// By category name, as the page writes it: a category at a time in that order, newest first within each.
fn by_category(conn: &Connection, filter: Option<&CatFilter>, descending: bool, want: i64) -> Result<Vec<ReleaseRow>> {
    let (leaves, uncategorized) = match filter {
        Some(f) => (f.leaves.clone(), f.uncategorized),
        None => (TABLE.iter().flat_map(|root| root.subs.iter().map(|s| s.id)).collect(), true),
    };
    let mut parts: Vec<(String, CatFilter)> = leaves
        .into_iter()
        .map(|leaf| (label(leaf).unwrap_or_default(), CatFilter { leaves: vec![leaf], uncategorized: false }))
        .collect();
    if uncategorized {
        parts.push((label(UNCATEGORIZED).unwrap_or_default(), CatFilter { leaves: Vec::new(), uncategorized: true }));
    }
    parts.sort_by(|a, b| a.0.cmp(&b.0));
    if descending {
        parts.reverse();
    }
    let mut rows: Vec<ReleaseRow> = Vec::new();
    for (_, part) in parts {
        let left = want - rows.len() as i64;
        if left <= 0 {
            break;
        }
        rows.extend(newest_in_categories_with(conn, &part, 0, left, &WHOLE)?.rows);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    struct Fixture {
        _dir: tempfile::TempDir,
        main: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        Fixture { _dir: dir, main }
    }

    /// one group in each shard
    fn groups() -> Vec<String> {
        let mut found: Vec<Option<String>> = vec![None; store::SHARDS];
        for i in 0.. {
            let group = format!("alt.binaries.t{i}");
            let slot = &mut found[store::shard_of(&group)];
            if slot.is_none() {
                *slot = Some(group);
            }
            if found.iter().all(Option::is_some) {
                break;
            }
        }
        found.into_iter().flatten().collect()
    }

    fn release(name: &str, group: &str) -> crate::parser::Release {
        let article = crate::parser::Article {
            message_id: format!("<{name}@x>"),
            subject: format!("\"{name}\" yEnc (1/1)"),
            author: "p <p@x>".into(),
            filename: Some(format!("{name}.bin")),
            part: Some(1),
            total_parts: Some(1),
            bytes: 10,
            ..Default::default()
        };
        crate::parser::Release {
            name: name.into(),
            group: group.into(),
            articles: vec![article],
            size: 10,
            complete: true,
            ..Default::default()
        }
    }

    impl Fixture {
        /// saved one at a time, soo ids rise in this order
        fn add(&self, name: &str, group: &str, size: Option<i64>, posted: &str, category: Option<i64>) -> i64 {
            store::save(&self.main, &[release(name, group)]).unwrap();
            let shard = self.shard(group);
            let id: i64 = shard.query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap();
            shard
                .execute(
                    "update releases set size = ?2, posted_date = ?3 where id = ?1",
                    rusqlite::params![id, size, posted],
                )
                .unwrap();
            if let Some(c) = category {
                shard
                    .execute(
                        "insert into release_category (release_id, category) values (?, ?)",
                        rusqlite::params![id, c],
                    )
                    .unwrap();
            }
            id
        }

        fn shard(&self, group: &str) -> Connection {
            db::open_shard(&store::shard_path(&self.main, store::shard_of(group))).unwrap()
        }

        fn index_all(&self) {
            for path in store::shard_paths(&self.main) {
                db::open_shard(&path)
                    .unwrap()
                    .execute_batch(&format!(
                        "create index if not exists {SIZE_INDEX} on releases(size);
                         create index if not exists {POSTED_INDEX} on releases(posted_date);
                         create index if not exists {NAME_INDEX} on releases(lower(coalesce(display_name, name)));"
                    ))
                    .unwrap();
            }
        }

        fn top(&self, filter: Option<&CatFilter>, by: Column, descending: bool, want: i64, limits: Limits) -> Vec<i64> {
            let main: &Path = &self.main;
            let open = || db::open_with_shards(main);
            top_with(&open, filter, by, descending, want, limits).unwrap().iter().map(|r| r.id).collect()
        }
    }

    const MOVIES: &str = "2000";
    const UHD: i64 = 2045;
    const HD: i64 = 2040;
    const TV_HD: i64 = 5040;
    /// every shard reads its category whole
    const READ_ALL: Limits = Limits { few: i64::MAX, some: i64::MAX, slack: 0 };
    /// every shard walks the index, with all the time it needs
    const WALK: Limits = Limits { few: 0, some: 0, slack: i64::MAX / 8 };
    /// a walk gives up at once, and the category is read whole
    const GIVE_UP: Limits = Limits { few: 0, some: i64::MAX, slack: i64::MIN / 2 };

    /// An old, big UHD release, many newer small ones in a parent category, and big ones outside it.
    fn library(f: &Fixture) -> (i64, Vec<i64>) {
        let groups = groups();
        let big = f.add("Batman.Begins.2005.2160p", &groups[0], Some(61_000), "2026-08-31", Some(UHD));
        let mut newer = Vec::new();
        for i in 0..40 {
            let group = &groups[i % groups.len()];
            let cat = if i % 2 == 0 { HD } else { UHD };
            newer.push(f.add(&format!("Movie.{i}.1080p"), group, Some(100 + i as i64), "2026-11-01", Some(cat)));
            // bigger than everything in Movies, but TV
            f.add(&format!("Show.S01E{i:02}"), group, Some(90_000 + i as i64), "2026-11-02", Some(TV_HD));
        }
        (big, newer)
    }

    #[test]
    fn a_parent_category_ranks_all_of_its_releases_however_old() {
        let f = fixture();
        let (big, newer) = library(&f);
        let movies = crate::category::parse_filter(MOVIES).unwrap();
        for (limits, indexed) in [(READ_ALL, false), (READ_ALL, true), (WALK, true), (GIVE_UP, true)] {
            if indexed {
                f.index_all();
            }
            let top = f.top(Some(&movies), Column::Size, true, 5, limits);
            // the newest 40 hold the rest, soo the oldest is only found over everything
            let mut want = vec![big];
            want.extend(newer.iter().rev().take(4));
            assert_eq!(top, want, "indexed {indexed}");
            let smallest = f.top(Some(&movies), Column::Size, false, 3, limits);
            assert_eq!(smallest, newer[..3], "indexed {indexed}");
        }
    }

    #[test]
    fn a_linked_upload_ranks_by_its_files_together() {
        let f = fixture();
        let group = &groups()[0];
        let shard = f.shard(group);
        shard.execute_batch(crate::archive::link::UPLOAD_LINK.create).unwrap();
        // older big ones, then an upload posted a file at a time, each smaller than all of them but bigger together
        let big: Vec<i64> =
            (0..3).map(|i| f.add(&format!("Big.{i}"), group, Some(6_000), "2026-09-01", Some(UHD))).collect();
        let lead = f.add("a1b2c3", group, Some(4_000), "2026-10-01", Some(UHD));
        let piece = f.add("d4e5f6", group, Some(4_000), "2026-10-01", Some(UHD));
        let small = f.add("Small", group, Some(10), "2026-10-02", Some(UHD));
        shard
            .execute(
                "insert into upload_link (release_id, lead_id) values (?1, ?1), (?2, ?1)",
                rusqlite::params![lead, piece],
            )
            .unwrap();
        let movies = crate::category::parse_filter(MOVIES).unwrap();
        for (limits, indexed) in [(READ_ALL, false), (WALK, true)] {
            if indexed {
                f.index_all();
            }
            for filter in [Some(&movies), None] {
                let top = f.top(filter, Column::Size, true, 2, limits);
                assert_eq!(top, [piece, lead], "indexed {indexed}: the upload's 8,000 before the 6,000s, newest first");
                // with the size it was ranked by, for a window that sorts rows it hasn't rendered
                let open = || db::open_with_shards(&f.main);
                let rows = top_with(&open, filter, Column::Size, true, 2, limits).unwrap();
                assert_eq!(rows.iter().map(|r| r.size).collect::<Vec<_>>(), [Some(8_000), Some(8_000)]);
                let all = f.top(filter, Column::Size, true, 10, limits);
                assert_eq!(all, [piece, lead, big[2], big[1], big[0], small], "indexed {indexed}");
                // going up its pieces come in by their own size, and the page puts them where the whole goes
                let up = f.top(filter, Column::Size, false, 1, limits);
                assert_eq!(up, [small], "indexed {indexed}");
            }
        }
        // as big as the last row read, and newer: ahead of it
        let same = f.add("g7h8i9", group, Some(3_000), "2026-10-04", Some(UHD));
        let same_piece = f.add("j1k2l3", group, Some(3_000), "2026-10-04", Some(UHD));
        shard
            .execute(
                "insert into upload_link (release_id, lead_id) values (?1, ?1), (?2, ?1)",
                rusqlite::params![same, same_piece],
            )
            .unwrap();
        for limits in [READ_ALL, WALK] {
            assert_eq!(f.top(Some(&movies), Column::Size, true, 4, limits), [piece, lead, same_piece, same]);
        }
        shard.execute("delete from upload_link where lead_id = ?", [same]).unwrap();
        shard.execute("delete from releases where id in (?, ?)", [same, same_piece]).unwrap();
        // bigger uploads outside the category don't use up the uploads read in
        let tv: Vec<i64> =
            (0..3).map(|i| f.add(&format!("tv{i}a"), group, Some(5_000), "2026-10-03", Some(TV_HD))).collect();
        for (i, &lead) in tv.iter().enumerate() {
            let piece = f.add(&format!("tv{i}b"), group, Some(5_000), "2026-10-03", Some(TV_HD));
            shard
                .execute(
                    "insert into upload_link (release_id, lead_id) values (?1, ?1), (?2, ?1)",
                    rusqlite::params![lead, piece],
                )
                .unwrap();
        }
        for limits in [READ_ALL, WALK] {
            assert_eq!(f.top(Some(&movies), Column::Size, true, 1, limits), [piece]);
        }
        // a lead that's gone leaves its pieces on their own, as the page does
        shard.execute("delete from releases where id = ?", [lead]).unwrap();
        assert_eq!(f.top(Some(&movies), Column::Size, true, 2, READ_ALL), [big[2], big[1]]);
    }

    #[test]
    fn linked_uploads_tied_on_size_go_newest_first_by_the_rows_the_page_lists() {
        let f = fixture();
        let group = &groups()[0];
        let shard = f.shard(group);
        shard.execute_batch(crate::archive::link::UPLOAD_LINK.create).unwrap();
        let big = f.add("Big", group, Some(6_000), "2026-09-01", Some(UHD));
        let link = |lead: i64, rest: &[i64]| {
            for id in std::iter::once(lead).chain(rest.iter().copied()) {
                shard.execute("insert into upload_link (release_id, lead_id) values (?1, ?2)", [id, lead]).unwrap();
            }
        };
        let a_lead = f.add("a1a1a1", group, Some(3_000), "2026-10-01", Some(UHD));
        let a_piece = f.add("a2a2a2", group, Some(3_000), "2026-10-01", Some(UHD));
        let b_lead = f.add("b1b1b1", group, Some(3_000), "2026-10-02", Some(UHD));
        let b_piece = f.add("b2b2b2", group, Some(3_000), "2026-10-02", Some(UHD));
        // the newest row of all is in the older upload, but in TV, which a Movies page doesn't list
        let a_tv = f.add("a3a3a3", group, Some(0), "2026-10-03", Some(TV_HD));
        link(a_lead, &[a_piece, a_tv]);
        link(b_lead, &[b_piece]);
        let movies = crate::category::parse_filter(MOVIES).unwrap();
        for limits in [READ_ALL, WALK] {
            assert_eq!(f.top(Some(&movies), Column::Size, true, 1, limits), [b_piece]);
            assert_eq!(
                f.top(Some(&movies), Column::Size, true, 5, limits),
                [b_piece, b_lead, a_piece, a_lead, big],
                "every upload as big as Big, newest first"
            );
        }
    }

    #[test]
    fn linked_uploads_stop_being_read_in_once_the_shard_has_enough() {
        let f = fixture();
        let group = &groups()[0];
        let shard = f.shard(group);
        shard.execute_batch(crate::archive::link::UPLOAD_LINK.create).unwrap();
        let bigs: Vec<i64> =
            (0..2).map(|i| f.add(&format!("Big.{i}"), group, Some(6_000), "2026-09-01", Some(UHD))).collect();
        // five uploads of three pieces, each bigger together than the Bigs
        for u in 0..5 {
            let ids: Vec<i64> =
                (0..3).map(|p| f.add(&format!("u{u}p{p}x"), group, Some(3_000), "2026-10-01", Some(UHD))).collect();
            for &id in &ids {
                shard.execute("insert into upload_link (release_id, lead_id) values (?1, ?2)", [id, ids[0]]).unwrap();
            }
        }
        let conn = db::open_with_shards(&f.main).unwrap();
        // the shard's own first two, which it had more than
        let mut rows: Vec<ReleaseRow> =
            bigs.iter().map(|&id| ReleaseRow { id, size: Some(6_000), ..Default::default() }).collect();
        linked_sizes(&conn, store::shard_of(group), None, &mut rows, 2).unwrap();
        assert_eq!(rows.len(), 2 + 3, "one upload's rows make the two wanted: the others aren't read in");
    }

    #[test]
    fn a_column_is_ready_to_rank_once_every_shard_has_its_index() {
        let f = fixture();
        let conn = db::open_with_shards(&f.main).unwrap();
        assert!(!all_indexed(&conn, Column::Size), "made without `--search-indexes`");
        f.index_all();
        assert!(all_indexed(&conn, Column::Size));
        assert!(ready(Column::Group) && ready(Column::Category), "these need no index of their own");
    }

    #[test]
    fn everything_and_no_category_rank_over_every_release() {
        let f = fixture();
        let groups = groups();
        let (big, _) = library(&f);
        let loose = f.add("Loose.Thing", &groups[3], Some(95_000), "2020-01-01", None);
        for indexed in [false, true] {
            if indexed {
                f.index_all();
            }
            let all = f.top(None, Column::Size, true, 2, WALK);
            assert_eq!(all.first(), Some(&loose), "indexed {indexed}");
            let other = crate::category::parse_filter("2000,8000").unwrap();
            assert_eq!(f.top(Some(&other), Column::Size, true, 2, WALK), [loose, big], "indexed {indexed}");
        }
    }

    #[test]
    fn missing_values_come_last_and_posted_and_name_rank_too() {
        let f = fixture();
        let groups = groups();
        let none = f.add("No.Size", &groups[1], None, "", Some(UHD));
        let a = f.add("alpha", &groups[2], Some(5), "2001-01-01", Some(UHD));
        let b = f.add("Beta", &groups[3], Some(7), "2024-01-01", Some(UHD));
        let uhd = crate::category::parse_filter(&UHD.to_string()).unwrap();
        for (limits, indexed) in [(READ_ALL, false), (WALK, true)] {
            if indexed {
                f.index_all();
            }
            assert_eq!(f.top(Some(&uhd), Column::Size, false, 10, limits), [a, b, none], "indexed {indexed}");
            assert_eq!(f.top(Some(&uhd), Column::Size, true, 10, limits), [b, a, none], "indexed {indexed}");
            assert_eq!(f.top(Some(&uhd), Column::Posted, true, 10, limits), [b, a, none], "indexed {indexed}");
            assert_eq!(f.top(Some(&uhd), Column::Posted, false, 10, limits), [a, b, none], "indexed {indexed}");
            assert_eq!(f.top(Some(&uhd), Column::Name, false, 10, limits), [a, b, none], "indexed {indexed}");
        }
    }

    #[test]
    fn an_empty_date_is_no_date_and_equals_are_newest_first_both_ways() {
        let f = fixture();
        let groups = groups();
        let group = &groups[0];
        let empty: Vec<i64> = (0..4).map(|i| f.add(&format!("Empty.{i}"), group, Some(1), "", Some(UHD))).collect();
        let dated = f.add("Dated", group, Some(1), "2020-01-01", Some(UHD));
        let uhd = crate::category::parse_filter(&UHD.to_string()).unwrap();
        for (limits, indexed) in [(READ_ALL, false), (WALK, true)] {
            if indexed {
                f.index_all();
            }
            assert_eq!(f.top(Some(&uhd), Column::Posted, false, 2, limits), [dated, empty[3]], "indexed {indexed}");
            // five in one group: the newest of them, whichever way
            for descending in [false, true] {
                let top = f.top(Some(&uhd), Column::Group, descending, 2, limits);
                assert_eq!(top, [dated, empty[3]], "indexed {indexed}, descending {descending}");
            }
        }
    }

    #[test]
    fn by_category_goes_a_category_at_a_time_in_name_order() {
        let f = fixture();
        let (_, newer) = library(&f);
        let movies = crate::category::parse_filter(MOVIES).unwrap();
        // Movies/HD before Movies/UHD, newest first within it
        let hd: Vec<i64> = newer.iter().copied().step_by(2).rev().take(3).collect();
        assert_eq!(f.top(Some(&movies), Column::Category, false, 3, LIMITS), hd);
        let uhd: Vec<i64> = newer.iter().copied().skip(1).step_by(2).rev().take(3).collect();
        assert_eq!(f.top(Some(&movies), Column::Category, true, 3, LIMITS), uhd);
    }

    #[test]
    fn a_small_category_is_read_off_the_category_table() {
        let f = fixture();
        f.index_all();
        let conn = db::open_with_shards(&f.main).unwrap();
        let (expr, filter) = (Column::Size.expr(), CatFilter { leaves: vec![UHD, HD], uncategorized: false });
        let sql = format!(
            "explain query plan select {COLUMNS} from s0.release_category c cross join s0.releases r on r.id = c.release_id
             where {VISIBLE_RELEASE} and c.category in ({}) order by ({expr} is null), {expr} desc, r.id desc limit 5",
            list(&filter.leaves)
        );
        let plan: Vec<String> =
            conn.prepare(&sql).unwrap().query_map([], |r| r.get(3)).unwrap().map(Result::unwrap).collect();
        let plan = plan.join(" / ");
        assert!(plan.starts_with("SEARCH c USING COVERING INDEX category_release"), "{plan}");
        assert!(plan.contains("SEARCH r USING INTEGER PRIMARY KEY"), "{plan}");
    }

    #[test]
    fn a_walk_reads_the_index_in_order() {
        let f = fixture();
        f.index_all();
        let conn = db::open_with_shards(&f.main).unwrap();
        for by in [Column::Size, Column::Posted, Column::Name, Column::Group] {
            for order in ["asc", "desc"] {
                let (expr, index) = (by.expr(), by.index());
                let (valued, unvalued) = (by.valued(), by.unvalued());
                let plan = |sql: String| {
                    let rows: Vec<String> = conn
                        .prepare(&format!("explain query plan {sql}"))
                        .unwrap()
                        .query_map([], |r| r.get(3))
                        .unwrap()
                        .map(Result::unwrap)
                        .collect();
                    rows.join(" / ")
                };
                let walk = plan(format!(
                    "select r.id from s0.releases r indexed by {index} where {valued} order by {expr} {order}, r.id desc"
                ));
                // runs of equals sorted as they come, never the whole index first
                assert!(walk.contains(&format!("COVERING INDEX {index}")), "{by:?} {order}: {walk}");
                assert!(!walk.contains("TEMP B-TREE FOR ORDER BY"), "{by:?} {order}: {walk}");
                let rest = plan(format!(
                    "select r.id from s0.releases r indexed by {index} where {unvalued} order by r.id desc"
                ));
                assert!(rest.contains(index), "{by:?}: {rest}");
            }
        }
    }

    fn row(id: i64, size: Option<i64>, name: &str) -> ReleaseRow {
        ReleaseRow { id, size, name: name.into(), ..ReleaseRow::default() }
    }

    #[test]
    fn rows_without_a_value_come_last_either_way() {
        let mut rows = [row(1, None, "a"), row(2, Some(5), "b"), row(3, Some(9), "c")];
        rows.sort_by(|a, b| compare(a, b, Column::Size, true, &HashMap::new()));
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [3, 2, 1]);
        rows.sort_by(|a, b| compare(a, b, Column::Size, false, &HashMap::new()));
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [2, 3, 1]);
    }

    #[test]
    fn equals_are_newest_first() {
        let mut rows = [row(1, Some(5), "x"), row(4, Some(5), "x"), row(2, Some(5), "x")];
        rows.sort_by(|a, b| compare(a, b, Column::Size, true, &HashMap::new()));
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [4, 2, 1]);
        rows.sort_by(|a, b| compare(a, b, Column::Name, false, &HashMap::new()));
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [4, 2, 1]);
    }
}
