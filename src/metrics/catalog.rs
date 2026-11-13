//! Storage-independent detailed catalog tally and metric labels. PostgreSQL
//! paging, admission and identity checks live in `pg::catalog_metrics`.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use super::names::*;
use super::{OTHER, label_key};
use crate::category::{self, UNCATEGORIZED};
use crate::naming::{Grade, NameSource, judge};

/// series for groups and release groups: the rest are summed into `(other)`
const TOP_GROUPS: usize = 100;
const TOP_RELEASE_GROUPS: usize = 150;
/// PreDB categories published, and kept during a walk: feeds and imports can bring any number
const TOP_PREDB_CATEGORIES: usize = 200;
const MAX_PREDB_CATEGORIES: usize = 10_000;
/// release group names kept during a walk; past this the ones seen once go
const MAX_RELEASE_GROUPS: usize = 200_000;

/// Retain the sampler's I/O breathing room when the native observer schedules
/// another walk. The next successful walk rests at least four walk durations.
pub(crate) fn rest_after(every: Duration, took: Duration) -> Duration {
    every.max(took.saturating_mul(4).min(Duration::from_secs(24 * 60 * 60)))
}

fn name_source_label(source: Option<i64>) -> &'static str {
    match source {
        None => "posted",
        Some(s) if s == NameSource::Cleared as i64 => "cleared",
        Some(s) if s == NameSource::PostedFile as i64 => "posted_file",
        Some(s) if s == NameSource::NamingRegex as i64 => "naming_regex",
        Some(s) if s == NameSource::InlineBody as i64 => "inline_body",
        Some(s) if s == NameSource::Nfo as i64 => "nfo",
        Some(s) if s == NameSource::ArchiveInner as i64 => "archive_inner",
        Some(s) if s == NameSource::Par2 as i64 => "par2",
        Some(s) if s == NameSource::PreDb as i64 => "predb",
        Some(_) => "other",
    }
}

/// for a name source of `source`, the label of `atlas_deobfuscated_total` and of the walk
pub fn name_source_name(source: u8) -> &'static str {
    name_source_label(Some(i64::from(source)))
}

/// how `predb_match.kind` matched
fn predb_kind_label(kind: i64) -> &'static str {
    use crate::predb::matcher::Kind;
    match kind {
        k if k == Kind::Hash as i64 => "hash",
        k if k == Kind::Exact as i64 => "exact",
        k if k == Kind::Normalized as i64 => "normalized",
        k if k == Kind::InnerFile as i64 => "inner_file",
        _ => "other",
    }
}

/// `TV/HD`, or the id for one outside the table.
fn category_label(id: i64) -> String {
    if id == UNCATEGORIZED {
        return "Uncategorized".into();
    }
    category::label(id).unwrap_or_else(|| id.to_string())
}

fn is_video_root(root: i64) -> bool {
    root == 2000 || root == 5000
}

/// Words that follow a dash in a scene name without being who released it.
const NOT_A_GROUP: &[&str] = &[
    "ac3", "avc", "bluray", "dd5", "dl", "dts", "dv", "h264", "h265", "hd", "hdr", "hdtv", "hevc", "sd", "uhd", "web",
    "webdl", "webrip", "x264", "x265", "xvid", "480p", "576p", "720p", "1080i", "1080p", "2160p", "4k",
];

/// The `GROUP` of `Some.Movie.2024.1080p.WEB-DL.x264-GROUP`, once a file
/// extension or a `[tag]` after it is taken off. None for a name not built like one.
pub fn release_group(name: &str) -> Option<&str> {
    let mut name = name.trim();
    while let Some(open) = name.strip_suffix(']').and_then(|n| n.rfind('[')) {
        name = name[..open].trim_end();
    }
    if let Some((stem, ext)) = name.rsplit_once('.')
        && (2..=4).contains(&ext.len())
        && ["mkv", "mp4", "avi", "m4v", "ts", "m2ts", "iso", "nzb", "nfo", "rar", "par2", "sfv"]
            .iter()
            .any(|e| ext.eq_ignore_ascii_case(e))
    {
        name = stem;
    }
    let (head, group) = name.rsplit_once('-')?;
    let separators = head.bytes().filter(|b| matches!(b, b'.' | b' ' | b'_')).count();
    let looks_right = separators >= 2
        && (2..=20).contains(&group.len())
        && group.bytes().all(|b| b.is_ascii_alphanumeric())
        && group.bytes().any(|b| b.is_ascii_alphabetic())
        // a hash or a random tail
        && !(group.len() >= 12 && group.bytes().all(|b| b.is_ascii_hexdigit()))
        && !NOT_A_GROUP.iter().any(|w| group.eq_ignore_ascii_case(w));
    looks_right.then_some(group)
}

#[derive(Default)]
struct Sizes {
    /// own count per bound of `SIZES`, with one more past the last
    buckets: Vec<u64>,
    sum: f64,
    count: u64,
}

#[derive(Default)]
struct GroupTally {
    blocked: bool,
    releases: u64,
    bytes: u64,
}

/// One walk's running totals.
#[derive(Default)]
pub struct Tally {
    pub rows: u64,
    total: u64,
    bytes: u64,
    complete: u64,
    nzb_ready: u64,
    named: u64,
    still_obfuscated: u64,
    predb_matched: u64,
    name_sources: BTreeMap<&'static str, u64>,
    /// category id -> (releases, bytes)
    categories: BTreeMap<i64, (u64, u64)>,
    groups: HashMap<String, GroupTally>,
    release_groups: HashMap<String, u64>,
    /// releases of the names dropped to stay under the limit
    release_groups_evicted: u64,
    video_with_group: u64,
    video_without: u64,
    sizes: BTreeMap<i64, Sizes>,
    predb_kinds: BTreeMap<i64, u64>,
    predb_categories: BTreeMap<String, u64>,
    predb_titles: HashMap<i64, u64>,
    /// root -> (matched, not matched)
    predb_roots: BTreeMap<i64, (u64, u64)>,
    feeds: BTreeMap<String, u64>,
}

/// One row of the walk.
pub struct Row<'a> {
    pub group: &'a str,
    pub size: i64,
    pub complete: bool,
    pub parts: i64,
    pub obfuscated: bool,
    pub named: bool,
    pub name_source: Option<i64>,
    pub name: &'a str,
    pub category: Option<i64>,
    pub predb: Option<(i64, Option<&'a str>, i64)>,
}

impl Tally {
    /// Admit retained native dimensions before adding a row. These limits are
    /// independent of page ownership; a million short pages cannot grow an
    /// unbounded group/feed/category map. A rejected walk is never published.
    pub(crate) fn add_native(&mut self, row: &Row<'_>, feed: Option<&str>) -> anyhow::Result<()> {
        use anyhow::ensure;
        ensure!(row.size >= 0 && row.parts >= 0, "native catalog has a negative size or part count");
        ensure!(row.group.len() <= crate::filters::MAX_GROUP_NAME, "native catalog group exceeds admission");
        ensure!(self.groups.contains_key(row.group) || self.groups.len() < 10_000,
            "native catalog group cardinality exceeds admission");
        let category = row.category.unwrap_or(UNCATEGORIZED);
        ensure!(self.categories.contains_key(&category) || self.categories.len() < 10_000,
            "native catalog category cardinality exceeds admission");
        if let Some((_, Some(category), _)) = row.predb {
            ensure!(category.len() <= 512, "native PreDB category exceeds admission");
        }
        if row.predb.is_some() {
            let feed = feed.unwrap_or("unknown");
            ensure!(feed.len() <= 512 && (self.feeds.contains_key(feed) || self.feeds.len() < 1000),
                "native PreDB feed exceeds admission");
        }
        let size = row.size.max(0) as u64;
        ensure!(self.bytes.checked_add(size).is_some()
            && self.groups.get(row.group).is_none_or(|group| group.bytes.checked_add(size).is_some())
            && self.categories.get(&category).is_none_or(|(_, bytes)| bytes.checked_add(size).is_some()),
            "native catalog byte total overflow");
        self.add(row);
        // Native pages join the retained PreDB feed directly in their RR
        // snapshot, so no title IDs escape the page or await a SQLite lookup.
        self.predb_titles.clear();
        if !crate::filters::group_blocked(row.group) && row.predb.is_some() {
            *self.feeds.entry(feed.unwrap_or("unknown").to_owned()).or_default() += 1;
        }
        Ok(())
    }

    pub fn add(&mut self, row: &Row<'_>) {
        self.rows += 1;
        let group = match self.groups.get_mut(row.group) {
            Some(group) => group,
            None => self.groups.entry(row.group.to_string()).or_insert_with(|| GroupTally {
                blocked: crate::filters::group_blocked(row.group),
                ..GroupTally::default()
            }),
        };
        if group.blocked {
            return;
        }
        let size = row.size.max(0) as u64;
        group.releases += 1;
        group.bytes += size;

        self.total += 1;
        self.bytes += size;
        self.complete += u64::from(row.complete);
        self.nzb_ready += u64::from(row.parts > 0);
        self.named += u64::from(row.named);
        self.still_obfuscated += u64::from(row.obfuscated && !row.named);
        // an inline name has no source on its row, see `naming::effective_source`
        let source = match row.name_source {
            None if row.named && judge(row.name) >= Grade::Weak => Some(NameSource::InlineBody as i64),
            source => source,
        };
        *self.name_sources.entry(name_source_label(source)).or_default() += 1;

        let category = row.category.unwrap_or(UNCATEGORIZED);
        let root = category::root_of(category);
        let in_category = self.categories.entry(category).or_default();
        in_category.0 += 1;
        in_category.1 += size;

        let sizes = self.sizes.entry(root).or_default();
        if sizes.buckets.is_empty() {
            sizes.buckets = vec![0; SIZES.len() + 1];
        }
        let at = SIZES.iter().position(|b| size as f64 <= *b).unwrap_or(SIZES.len());
        sizes.buckets[at] += 1;
        sizes.sum += size as f64;
        sizes.count += 1;

        let in_root = self.predb_roots.entry(root).or_default();
        if let Some((kind, predb_category, title)) = row.predb {
            in_root.0 += 1;
            self.predb_matched += 1;
            *self.predb_kinds.entry(kind).or_default() += 1;
            let predb_category = predb_category.unwrap_or("none");
            let known = self.predb_categories.len() < MAX_PREDB_CATEGORIES
                || self.predb_categories.contains_key(predb_category);
            let predb_category = if known { predb_category } else { OTHER };
            *self.predb_categories.entry(predb_category.to_string()).or_default() += 1;
            *self.predb_titles.entry(title).or_default() += 1;
        } else {
            in_root.1 += 1;
        }

        if is_video_root(root) && !(row.obfuscated && !row.named) {
            match release_group(row.name) {
                Some(found) => {
                    self.video_with_group += 1;
                    self.count_release_group(found, MAX_RELEASE_GROUPS);
                }
                None => self.video_without += 1,
            }
        }
    }

    /// One more release of `found`. At `limit` names the smaller half go into `(other)`: each pass frees
    /// at least half the room, soo the passes stay rare, however even the counts are.
    fn count_release_group(&mut self, found: &str, limit: usize) {
        if let Some(n) = self.release_groups.get_mut(found) {
            *n += 1;
            return;
        }
        if self.release_groups.len() >= limit {
            let mut counts: Vec<u64> = self.release_groups.values().copied().collect();
            let middle = counts.len() / 2;
            let keep_above = *counts.select_nth_unstable(middle).1;
            let mut evicted = 0;
            self.release_groups.retain(|_, n| {
                let keep = *n > keep_above;
                if !keep {
                    evicted += *n;
                }
                keep
            });
            self.release_groups_evicted += evicted;
        }
        self.release_groups.insert(found.to_string(), 1);
    }

    /// Publish a completed detailed walk. Native Stage owns the quick total and byte gauges.
    pub fn publish(&self, took: Duration) {
        let one = |name: &str, value: &str| label_key(&[(name, value)]);
        CATALOG_NAME_SOURCE.replace(self.name_sources.iter().map(|(s, n)| (one("source", s), *n as f64)));

        let category_key = |id: i64| {
            let root = category_label(category::root_of(id));
            label_key(&[("category", &category_label(id)), ("root", &root)])
        };
        CATALOG_CATEGORY.replace(self.categories.iter().map(|(id, (n, _))| (category_key(*id), *n as f64)));
        CATALOG_CATEGORY_BYTES.replace(self.categories.iter().map(|(id, (_, b))| (category_key(*id), *b as f64)));

        let mut groups: Vec<(&String, &GroupTally)> =
            self.groups.iter().filter(|(_, g)| !g.blocked && g.releases > 0).collect();
        groups.sort_by_key(|(_, g)| std::cmp::Reverse(g.bytes));
        let (top, rest) = groups.split_at(groups.len().min(TOP_GROUPS));
        let rest_releases: u64 = rest.iter().map(|(_, g)| g.releases).sum();
        let rest_bytes: u64 = rest.iter().map(|(_, g)| g.bytes).sum();
        let tail = |n: u64| (!rest.is_empty()).then(|| (one("group", OTHER), n as f64));
        CATALOG_GROUP.replace(top.iter().map(|(g, t)| (one("group", g), t.releases as f64)).chain(tail(rest_releases)));
        CATALOG_GROUP_BYTES.replace(top.iter().map(|(g, t)| (one("group", g), t.bytes as f64)).chain(tail(rest_bytes)));

        let mut release_groups: Vec<(&String, &u64)> = self.release_groups.iter().collect();
        release_groups.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        let (top, rest) = release_groups.split_at(release_groups.len().min(TOP_RELEASE_GROUPS));
        let rest_total: u64 = rest.iter().map(|(_, n)| **n).sum::<u64>() + self.release_groups_evicted;
        CATALOG_RELEASE_GROUP.replace(
            top.iter()
                .map(|(g, n)| (one("release_group", g), **n as f64))
                .chain((rest_total > 0).then(|| (one("release_group", OTHER), rest_total as f64))),
        );
        CATALOG_VIDEO.replace([
            (one("release_group", "found"), self.video_with_group as f64),
            (one("release_group", "none"), self.video_without as f64),
        ]);

        let mut size_rows = Vec::new();
        for (root, sizes) in &self.sizes {
            let root = category_label(*root);
            let mut below = 0;
            for (bound, own) in SIZES.iter().zip(&sizes.buckets) {
                below += own;
                size_rows.push((label_key(&[("root", &root), ("le", &bound.to_string())]), below as f64));
            }
            size_rows.push((label_key(&[("root", &root), ("le", "+Inf")]), sizes.count as f64));
        }
        CATALOG_SIZE.replace(size_rows);
        CATALOG_SIZE_SUM.replace(self.sizes.iter().map(|(r, s)| (one("root", &category_label(*r)), s.sum)));
        CATALOG_SIZE_COUNT.replace(self.sizes.iter().map(|(r, s)| (one("root", &category_label(*r)), s.count as f64)));

        CATALOG_PREDB_KIND
            .replace(self.predb_kinds.iter().map(|(k, n)| (one("kind", predb_kind_label(*k)), *n as f64)));
        CATALOG_PREDB_CATEGORY.replace(
            top_with_other(&self.predb_categories, TOP_PREDB_CATEGORIES)
                .iter()
                .map(|(c, n)| (one("category", c), *n as f64)),
        );
        CATALOG_PREDB_FEED.replace(self.feeds.iter().map(|(f, n)| (one("feed", f), *n as f64)));
        CATALOG_PREDB_ROOT.replace(self.predb_roots.iter().flat_map(|(root, (yes, no))| {
            let root = category_label(*root);
            [
                (label_key(&[("root", &root), ("matched", "yes")]), *yes as f64),
                (label_key(&[("root", &root), ("matched", "no")]), *no as f64),
            ]
        }));

        CATALOG_SCAN_AT.replace([(String::new(), chrono::Utc::now().timestamp() as f64)]);
        CATALOG_SCAN_SECONDS.replace([(String::new(), took.as_secs_f64())]);
        CATALOG_SCAN_ROWS.replace([(label_key(&[("state", "last")]),self.rows as f64)]);
    }


}

/// The `limit` biggest, and the rest summed as `(other)`.
fn top_with_other(counts: &BTreeMap<String, u64>, limit: usize) -> Vec<(String, u64)> {
    let mut sorted: Vec<(&String, &u64)> = counts.iter().filter(|(name, _)| name.as_str() != OTHER).collect();
    sorted.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    let (top, rest) = sorted.split_at(sorted.len().min(limit));
    let mut out: Vec<(String, u64)> = top.iter().map(|(name, n)| ((*name).clone(), **n)).collect();
    let other = counts.get(OTHER).copied().unwrap_or(0) + rest.iter().map(|(_, n)| **n).sum::<u64>();
    if other > 0 {
        out.push((OTHER.to_string(), other));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_walk_rests_longer_than_its_interval() {
        assert_eq!(rest_after(Duration::from_secs(60),Duration::from_secs(1200)),Duration::from_secs(4800));
        assert_eq!(rest_after(Duration::from_secs(60),Duration::ZERO),Duration::from_secs(60));
        assert_eq!(rest_after(Duration::from_secs(60),Duration::MAX),Duration::from_secs(86400));
    }

    #[test]
    fn native_feed_labels_and_category_sizes_follow_admitted_rows() {
        let mut tally = Tally::default();
        let row = Row { group:"alt.binaries.test", size:17, complete:true, parts:1,
            obfuscated:false, named:false, name_source:None,
            name:"Some.Movie.2024.1080p-GROUP", category:Some(2000),
            predb:Some((1,Some("Movies"),9)) };
        tally.add_native(&row,Some("feed-seven")).unwrap();
        assert_eq!(tally.rows,1);assert_eq!(tally.categories[&2000],(1,17));
        assert_eq!(tally.feeds["feed-seven"],1);assert!(tally.predb_titles.is_empty());
        let oversized="x".repeat(4097);
        assert!(tally.add_native(&row,Some(&oversized)).is_err());
        assert_eq!(tally.rows,1,"failed admission must not add a partial row");
        let negative=Row {size:-1,..row};
        assert!(tally.add_native(&negative,Some("feed-seven")).is_err());assert_eq!(tally.rows,1);
    }

    #[test]
    fn native_byte_overflow_rejects_the_row_before_mutating_the_tally() {
        let mut tally = Tally::default();
        let row = Row {group:"alt.binaries.test",size:i64::MAX,complete:false,parts:0,
            obfuscated:false,named:false,name_source:None,name:"Example",category:None,predb:None};
        tally.add_native(&row,None).unwrap();tally.add_native(&row,None).unwrap();
        assert!(tally.add_native(&row,None).is_err());assert_eq!(tally.rows,2);
        assert_eq!(tally.bytes,18_446_744_073_709_551_614);
    }

    #[test]
    fn release_group_reads_scene_names() {
        for (name, want) in [
            ("Some.Movie.2024.1080p.WEB-DL.x264-GROUP", Some("GROUP")),
            ("Some.Movie.2024.1080p.WEB-DL.x264-GROUP.mkv", Some("GROUP")),
            ("Some Show S01E02 1080p WEB h264-NTb [rartv]", Some("NTb")),
            ("Some.Movie.2024.1080p.WEB-DL", None),
            ("Some.Movie.2024.2160p-x265", None),
            ("short-name", None),
            ("Some.Movie.2024.1080p-0123456789abcdef", None),
            ("Some.Movie.2024.1080p-1234", None),
            ("", None),
        ] {
            assert_eq!(release_group(name), want, "{name}");
        }
    }

    #[test]
    fn release_groups_past_the_limit_add_up_under_other() {
        let mut tally = Tally::default();
        // every name seen twice, soo no count is smaller than another
        for name in ["a", "b", "c", "d"] {
            tally.count_release_group(name, 4);
            tally.count_release_group(name, 4);
        }
        tally.count_release_group("e", 4);
        assert!(tally.release_groups.len() <= 4, "{:?}", tally.release_groups);
        assert_eq!(tally.release_groups["e"], 1);
        let kept: u64 = tally.release_groups.values().sum();
        assert_eq!(kept + tally.release_groups_evicted, 9, "nothing is lost");
        assert!(tally.release_groups_evicted > 0);
    }

    #[test]
    fn past_the_limit_categories_add_up_under_other() {
        let counts =
            BTreeMap::from([("a".to_string(), 5), ("b".to_string(), 3), ("c".to_string(), 1), (OTHER.to_string(), 2)]);
        assert_eq!(top_with_other(&counts, 1), vec![("a".to_string(), 5), (OTHER.to_string(), 6)]);
    }

    #[test]
    fn an_inline_name_is_told_from_the_posted_one() {
        let row = |name, named| Row {
            group: "alt.binaries.test",
            size: 0,
            complete: true,
            parts: 1,
            obfuscated: true,
            named,
            name_source: None,
            name,
            category: None,
            predb: None,
        };
        let mut tally = Tally::default();
        tally.add(&row("Some.Movie.2024.1080p.WEB-DL.x264-GROUP", true));
        tally.add(&row("abcdef0123456789", false));
        assert_eq!(tally.name_sources["inline_body"], 1);
        assert_eq!(tally.name_sources["posted"], 1);
    }

    #[test]
    fn every_name_source_has_a_label() {
        for (source, label) in [
            (NameSource::Nfo, "nfo"),
            (NameSource::Par2, "par2"),
            (NameSource::PreDb, "predb"),
            (NameSource::ArchiveInner, "archive_inner"),
        ] {
            assert_eq!(name_source_name(source.value()), label);
        }
        assert_eq!(name_source_label(Some(77)), "other");
    }
}
