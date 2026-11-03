//! What the release page says about a posting itself: who posted it, its names, its PAR2 files
//! and how much of it is there. Read when the page opens, soo nothing here is stored.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use rusqlite::{Connection, OptionalExtension};

use crate::db::{self, Result};
use crate::naming::NameSource;
use crate::store;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Names {
    pub posted: String,
    pub display: Option<String>,
    pub obfuscated: bool,
    pub source: Option<i64>,
}

/// Where a display name came from, for a person.
pub fn name_source_label(source: i64) -> Option<&'static str> {
    [
        (NameSource::PostedFile, "a posted file name"),
        (NameSource::NamingRegex, "the subject"),
        (NameSource::Nfo, "the NFO"),
        (NameSource::ArchiveInner, "a file inside the archive"),
        (NameSource::Par2, "the PAR2 file"),
        (NameSource::PreDb, "PreDB"),
    ]
    .into_iter()
    .find_map(|(known, label)| (known as i64 == source).then_some(label))
}

/// One file of a release's NZB.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NzbFile {
    pub name: String,
    /// as posted: the articles' bytes, yEnc and all
    pub bytes: i64,
    pub seen: i64,
    pub expected: i64,
}

/// The most files listed of one release. Some posts are thousands of little files.
pub const MAX_LISTED_FILES: usize = 1000;

/// Totals over a release's files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Contents {
    /// the files themselves, by name, `MAX_LISTED_FILES` at most
    pub list: Vec<NzbFile>,
    pub files: i64,
    pub par2_files: i64,
    pub recovery_volumes: i64,
    /// bytes of the files that aren't PAR2
    pub data_bytes: i64,
    /// bytes of the recovery volumes
    pub recovery_bytes: i64,
    pub articles_seen: i64,
    pub articles_expected: i64,
    /// the same, for the files that aren't PAR2
    pub data_articles_seen: i64,
    pub data_articles_expected: i64,
    /// and for the recovery volumes
    pub recovery_articles_seen: i64,
    pub recovery_articles_expected: i64,
    /// from the `+MM` in recovery volume names
    pub recovery_blocks: i64,
    /// from the PAR2 set, once read
    pub par2_block: Option<i64>,
    pub par2_blocks: Option<i64>,
}

impl Contents {
    /// Share of the articles that are missing, in tenths of a percent. None: no count to go by.
    pub fn missing_per_mille(&self) -> Option<i64> {
        (self.articles_expected > 0)
            .then(|| (self.articles_expected - self.articles_seen).max(0) * 1000 / self.articles_expected)
    }

    /// Whether the recovery volumes that are there can make up for the data that isn't. Counts articles, soo
    /// it's a good guess rather than a promise: a damaged article can cost a whole block.
    pub fn repairable(&self) -> bool {
        if self.data_articles_expected == 0 || self.recovery_articles_expected == 0 {
            return false;
        }
        let missing = i128::from((self.data_articles_expected - self.data_articles_seen).max(0));
        // whole numbers: rounding each to tenths of a percent would call 2 missing of 10,000 repairable by 1 block
        if let Some(blocks) = self.par2_blocks.filter(|_| self.exact_repair_per_mille().is_some()) {
            // the blocks are counted from file names, whether or not their articles are there
            return i128::from(self.recovery_blocks)
                * i128::from(self.recovery_articles_seen)
                * i128::from(self.data_articles_expected)
                >= missing * i128::from(blocks) * i128::from(self.recovery_articles_expected);
        }
        // sizes are of the articles that are there. What's missing is taken to be as big as what isn't
        missing == 0
            || (self.data_articles_seen > 0
                && self.data_bytes > 0
                && i128::from(self.recovery_bytes) * i128::from(self.data_articles_seen)
                    >= missing * i128::from(self.data_bytes))
    }

    /// What the recovery blocks cover, in tenths of a percent, when the PAR2 set's been read.
    pub fn exact_repair_per_mille(&self) -> Option<i64> {
        let blocks = self.par2_blocks?;
        (self.par2_block? > 0 && blocks > 0 && self.recovery_blocks > 0).then(|| self.recovery_blocks * 1000 / blocks)
    }

    /// What the recovery volumes could rebuild, as a share of the data in tenths of a percent.
    /// From sizes on the wire, soo it's an estimate: real blocks are a little smaller than the volumes.
    pub fn repair_per_mille(&self) -> Option<i64> {
        if self.data_bytes <= 0 {
            return None;
        }
        // the data's whole size, from what's there of it
        let whole = if self.data_articles_seen > 0 && self.data_articles_expected > self.data_articles_seen {
            i128::from(self.data_bytes) * i128::from(self.data_articles_expected) / i128::from(self.data_articles_seen)
        } else {
            i128::from(self.data_bytes)
        };
        i64::try_from(i128::from(self.recovery_bytes) * 1000 / whole).ok()
    }
}

fn is_par2(filename: &str) -> bool {
    filename.to_ascii_lowercase().ends_with(".par2")
}

/// The `MM` of `name.vol000+MM.par2`, or the span of `name.volNNN-MMM.par2`.
fn recovery_blocks(filename: &str) -> i64 {
    static BLOCKS: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"(?i)\.vol(\d+)([+-])(\d+)\.par2$").unwrap()
    });
    let Some(hit) = BLOCKS.captures(filename) else { return 0 };
    let (Ok(start), Ok(second)) = (hit[1].parse::<i64>(), hit[3].parse::<i64>()) else { return 0 };
    if &hit[2] == "+" {
        second
    } else if second >= start {
        (second - start).max(1)
    } else {
        0
    }
}

static POST_MEDIA_EXT: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)]
    Regex::new(
        r"(?i)\.(mkv|mp4|m4v|avi|wmv|mov|ts|m2ts|mpg|mpeg|webm|iso|img|bin|7z|zip|exe|mp3|flac|m4a|ogg|wav|wma|ape|aac|opus|epub|mobi|azw3|pdf|cbr|cbz|jpg|jpeg|png|gif|bmp|webp|tiff?|sub|idx|srt|rev|diz)$",
    )
    .unwrap()
});

/// The release stem shared across a split upload's volume, parity and payload files.
pub fn post_stem(name: &str) -> String {
    let mut s = crate::parser::strip_release_suffixes(name.trim());
    loop {
        let next = POST_MEDIA_EXT.replace(&s, "").into_owned();
        let next = crate::parser::strip_release_suffixes(&next);
        if next == s || next.is_empty() {
            break;
        }
        s = next;
    }
    s
}

fn is_recovery_volume(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    lower.contains(".vol") && lower.ends_with(".par2")
}

pub fn names(id: i64) -> Result<Option<Names>> {
    names_with(&db::open()?, id)
}

pub fn names_with(conn: &Connection, id: i64) -> Result<Option<Names>> {
    let sql = format!(
        "select name, display_name, is_obfuscated, name_source from s{}.releases where id = ?",
        store::shard_of_id(id)
    );
    conn.query_row(&sql, [id], |r| {
        Ok(Names {
            posted: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
            display: r.get(1)?,
            obfuscated: r.get::<_, Option<i64>>(2)?.unwrap_or(0) != 0,
            source: r.get(3)?,
        })
    })
    .optional()
}

pub fn contents(id: i64) -> Result<Contents> {
    contents_with(&db::open()?, id)
}

pub fn contents_with(conn: &Connection, id: i64) -> Result<Contents> {
    contents_of(conn, &[id], &[])
}

/// `recovery`: rows whose files are all recovery volumes, whatever they're called.
fn contents_of(conn: &Connection, ids: &[i64], recovery: &[i64]) -> Result<Contents> {
    let mut totals = Contents::default();
    let union = union_files(conn, ids)?;
    let mut volumes: BTreeSet<String> = BTreeSet::new();
    for id in recovery {
        let s = store::shard_of_id(*id);
        let mut stmt = conn.prepare_cached(&format!("select filename from s{s}.files where release_id = ?"))?;
        for name in stmt.query_map([id], |r| r.get::<_, String>(0))? {
            volumes.insert(name?);
        }
    }
    let is_recovery_volume = |name: &str| volumes.contains(name) || is_recovery_volume(name);
    let is_par2 = |name: &str| volumes.contains(name) || is_par2(name);
    for (filename, file) in &union.files {
        let seen = store::seen_count(&file.seen);
        totals.files += 1;
        totals.articles_seen += seen;
        let want = file.expected.unwrap_or(seen).max(seen);
        totals.articles_expected += want;
        if totals.list.len() < MAX_LISTED_FILES {
            totals.list.push(NzbFile { name: filename.clone(), bytes: 0, seen, expected: want });
        }
        if is_recovery_volume(filename) {
            totals.recovery_articles_seen += seen;
            totals.recovery_articles_expected += want;
        } else if !is_par2(filename) {
            totals.data_articles_seen += seen;
            totals.data_articles_expected += want;
        }
        if is_par2(filename) {
            totals.par2_files += 1;
            totals.recovery_volumes += i64::from(is_recovery_volume(filename));
            totals.recovery_blocks += recovery_blocks(filename);
        }
    }
    let mut bytes: HashMap<String, i64> = HashMap::new();
    for id in ids {
        store::each_article(conn, *id, &mut |article| {
            *bytes.entry(article.filename.unwrap_or_default()).or_default() += article.bytes.unwrap_or(0);
            Ok(())
        })?;
    }
    for file in &mut totals.list {
        file.bytes = bytes.get(&file.name).copied().unwrap_or(0);
    }
    // the way they're read: the text files, then a RAR set from its first volume, then PAR2
    totals.list.sort_by_cached_key(|f| {
        let lower = f.name.to_ascii_lowercase();
        let rank = if is_par2(&f.name) {
            3
        } else if lower.ends_with(".rar") {
            1
        } else if lower.ends_with(".nfo") || lower.ends_with(".sfv") || lower.ends_with(".txt") {
            0
        } else {
            2
        };
        (rank, lower)
    });
    for (filename, size) in bytes {
        if is_recovery_volume(&filename) {
            totals.recovery_bytes += size;
        } else if !is_par2(&filename) {
            totals.data_bytes += size;
        }
    }
    if let Some((block, blocks)) = ids.iter().find_map(|id| crate::duplicates::par2_sizes(conn, *id)) {
        totals.par2_block = Some(block);
        totals.par2_blocks = Some(blocks);
    }
    Ok(totals)
}

/// A row that holds nothing but PAR2 data. A fully obfuscated upload posts every file under its own random subject and
/// poster, soo each file is a row of its own, and the recovery volumes are the ones that get the release's name, from the
/// set they belong to. Their size is only theirs: the files they repair are rows of their own, under other names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoveryOnly {
    /// what the set says the release's files are, with their sizes. Empty: no set has been read
    pub repairs: Vec<(String, Option<i64>)>,
}

impl RecoveryOnly {
    /// The size of the files the set repairs, when it gives every one.
    pub fn repairs_bytes(&self) -> Option<i64> {
        if self.repairs.is_empty() {
            return None;
        }
        self.repairs.iter().try_fold(0i64, |sum, (_, size)| Some(sum.saturating_add((*size)?)))
    }
}

/// The most files a row can have and still be read here: PAR2-only rows have one, or a few named .par2.
const RECOVERY_ONLY_FILES: usize = 64;

/// Whether a row's files are all PAR2, and what its set repairs. A file counts as PAR2 by its name, or, for a row of one
/// file, by its set's list having been read from it: nothing else in the row could have given it.
pub fn recovery_only_with(conn: &Connection, id: i64) -> Result<Option<RecoveryOnly>> {
    let s = store::shard_of_id(id);
    let names: Vec<String> = conn
        .prepare_cached(&format!(
            "select filename from s{s}.files where release_id = ? limit {}",
            RECOVERY_ONLY_FILES + 1
        ))?
        .query_map([id], |r| r.get(0))?
        .collect::<Result<_>>()?;
    if names.is_empty() || names.len() > RECOVERY_ONLY_FILES {
        return Ok(None);
    }
    // a shard that hasn't got the table yet has read no set
    let repairs: Vec<(String, Option<i64>)> = conn
        .prepare_cached(&format!(
            "select name, size from s{s}.archive_inner_files where release_id = ? and source = ? order by seq"
        ))
        .and_then(|mut stmt| {
            stmt.query_map(rusqlite::params![id, crate::archive::store::SOURCE_PAR2], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_>>()
        })
        .unwrap_or_default();
    let all_named_par2 = names.iter().all(|name| is_par2(name));
    let one_sniffed = names.len() == 1 && !repairs.is_empty() && !repairs.iter().any(|(name, _)| *name == names[0]);
    Ok((all_named_par2 || one_sniffed).then_some(RecoveryOnly { repairs }))
}

/// `recovery_only_with` for a page of results. Any failure reads as not known.
pub fn recovery_only_of(ids: &[i64]) -> HashMap<i64, RecoveryOnly> {
    db::with_reader(|conn| {
        Ok(ids.iter().filter_map(|id| Some((*id, recovery_only_with(conn, *id).ok().flatten()?))).collect())
    })
    .unwrap_or_default()
}

/// A posting of this name in one group. The same post in several groups is a release per group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupPosting {
    pub group: String,
    pub articles: i64,
    pub id: i64,
}

/// Every group with a release of exactly this posted name.
pub fn groups_of(name: &str) -> Result<Vec<GroupPosting>> {
    groups_of_with(&db::open()?, name)
}

pub fn groups_of_with(conn: &Connection, name: &str) -> Result<Vec<GroupPosting>> {
    let mut out = Vec::new();
    for s in 0..store::SHARDS {
        let mut stmt = conn.prepare_cached(&format!(
            "select group_name, coalesce(parts, 0), id from s{s}.releases where name = ?1 limit 64"
        ))?;
        let rows = stmt.query_map([name], |r| {
            Ok(GroupPosting {
                group: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                articles: r.get(1)?,
                id: r.get(2)?,
            })
        })?;
        for row in rows {
            out.push(row?);
        }
    }
    out.sort_by(|a, b| b.articles.cmp(&a.articles).then_with(|| a.group.cmp(&b.group)));
    Ok(out)
}

/// Every group represented in a split or linked upload's rows, with its combined article count.
pub fn groups_of_ids(conn: &Connection, ids: &[i64]) -> Result<Vec<GroupPosting>> {
    let mut by_group: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    for &id in ids {
        let s = store::shard_of_id(id);
        let mut stmt =
            conn.prepare_cached(&format!("select group_name, coalesce(parts, 0) from s{s}.releases where id = ?1"))?;
        if let Some((group, articles)) = stmt
            .query_row([id], |r| Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get::<_, i64>(1)?)))
            .optional()?
        {
            let entry = by_group.entry(group).or_insert((0, id));
            entry.0 += articles;
            entry.1 = entry.1.min(id);
        }
    }
    let mut out: Vec<GroupPosting> =
        by_group.into_iter().map(|(group, (articles, id))| GroupPosting { group, articles, id }).collect();
    out.sort_by(|a, b| b.articles.cmp(&a.articles).then_with(|| a.group.cmp(&b.group)));
    Ok(out)
}

/// One post whose articles went to several groups, each article to one of them: every group holds a release of the same
/// name from the same poster, and only together do they make the files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitPost {
    /// each group's release, lowest id first
    pub ids: Vec<i64>,
    pub groups: Vec<String>,
    pub size: i64,
    /// every file has all its parts, across the groups
    pub complete: bool,
    /// for an upload linked by its PAR2 set (`archive::link`): the row of the file it repairs, the one with its name
    pub lead: Option<i64>,
    /// of a linked upload, the rows of recovery data: their files' names don't say so
    pub recovery: Vec<i64>,
}

impl SplitPost {
    /// The one to list. A search that isn't about one group shows only this.
    pub fn listed_id(&self) -> i64 {
        self.lead.unwrap_or_else(|| self.ids.iter().copied().max().unwrap_or(0))
    }
}

/// The most rows a linked upload is read with: a set of a few hundred volumes, each posted on its own, is about the most.
/// One with more isn't shown as one upload, rather than as part of one.
const LINKED_ROWS: usize = 1_024;

/// The lead of the upload a row is linked into: by its own link row, or, for a recovery volume in another shard than
/// the upload's (the stage links within a shard only), by its sealed set, which some volume of the upload's own shard
/// linked under. None: nothing links it.
fn lead_of(conn: &Connection, id: i64) -> Result<Option<i64>> {
    let s = store::shard_of_id(id);
    // a shard that hasn't got the table yet has linked nothing
    let Ok(mut stmt) = conn.prepare_cached(&format!("select lead_id from s{s}.upload_link where release_id = ?"))
    else {
        return Ok(None);
    };
    if let Some(lead) = stmt.query_row([id], |r| r.get::<_, i64>(0)).optional()? {
        return Ok(Some(lead));
    }
    // only a row of nothing but PAR2: a data file with the set's id is its own posting of the set, maybe a twin's
    if !par2_only(conn, id)? {
        return Ok(None);
    }
    let Ok(mut sets) = conn.prepare_cached(&format!(
        "select distinct set_id from s{s}.par2_files where release_id = ? and sealed = 1 limit 8"
    )) else {
        return Ok(None);
    };
    let sets: Vec<Vec<u8>> = sets.query_map([id], |r| r.get(0))?.collect::<Result<_>>()?;
    // the set's packets are public: a volume sealed by a repost of its Main packet is only this upload's when the
    // same poster put it up
    let Some(poster) = poster_of(conn, id)? else { return Ok(None) };
    for set in sets {
        for other in 0..store::SHARDS {
            let Ok(mut stmt) = conn.prepare_cached(&format!(
                "select lead_id from s{other}.upload_link where set_id = ?1 and recovery = 1 limit 8"
            )) else {
                continue;
            };
            let leads: Vec<i64> = stmt.query_map([&set], |r| r.get::<_, i64>(0))?.collect::<Result<_>>()?;
            for lead in leads {
                if poster_of(conn, lead)?.as_deref() == Some(poster.as_str()) {
                    return Ok(Some(lead));
                }
            }
        }
    }
    Ok(None)
}

/// Who posted a release, as its articles' From says. None: no such row, or no From was kept.
fn poster_of(conn: &Connection, id: i64) -> Result<Option<String>> {
    let s = store::shard_of_id(id);
    Ok(conn
        .prepare_cached(&format!("select poster from s{s}.releases where id = ?"))?
        .query_row([id], |r| r.get::<_, Option<String>>(0))
        .optional()?
        .flatten())
}

/// Whether every file of a release is a PAR2 file: a recovery volume posted on its own.
fn par2_only(conn: &Connection, id: i64) -> Result<bool> {
    let s = store::shard_of_id(id);
    let data: i64 = conn
        .prepare_cached(&format!(
            "select count(*) from s{s}.files where release_id = ?1 and lower(filename) not like '%.par2'"
        ))?
        .query_row([id], |r| r.get(0))?;
    let any: i64 = conn
        .prepare_cached(&format!("select count(*) from s{s}.files where release_id = ?1"))?
        .query_row([id], |r| r.get(0))?;
    Ok(data == 0 && any > 0)
}

/// A release's group and size, from its shard. None: it's gone.
fn group_and_size(conn: &Connection, id: i64) -> Result<Option<(String, i64)>> {
    let s = store::shard_of_id(id);
    conn.prepare_cached(&format!("select coalesce(group_name, ''), coalesce(size, 0) from s{s}.releases where id = ?"))?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
}

/// The upload a row is linked into, all its rows together. None: it isn't linked, or its lead is gone.
pub fn linked_post_with(conn: &Connection, id: i64) -> Result<Option<SplitPost>> {
    let Some(lead) = lead_of(conn, id)? else { return Ok(None) };
    let s = store::shard_of_id(lead);
    let sql = format!(
        "select r.id, r.group_name, coalesce(r.size, 0), l.recovery, coalesce(l.wanted, 0), l.par2_file, l.set_id
         from s{s}.upload_link l join s{s}.releases r on r.id = l.release_id where l.lead_id = ?1 order by r.id
         limit {}",
        LINKED_ROWS + 1
    );
    let (mut ids, mut groups, mut size, mut has_lead, mut recovery) = (Vec::new(), Vec::new(), 0, false, Vec::new());
    // the files its sets look for, and those found: a file posted twice is still one file
    let (mut wanted, mut files) = (0, HashSet::new());
    let mut sets: Vec<Vec<u8>> = Vec::new();
    let Ok(mut stmt) = conn.prepare_cached(&sql) else { return Ok(None) };
    let rows = stmt.query_map([lead], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)? != 0,
            r.get::<_, i64>(4)?,
            r.get::<_, Option<Vec<u8>>>(5)?,
            r.get::<_, Option<Vec<u8>>>(6)?,
        ))
    })?;
    for row in rows {
        let (row_id, group, bytes, repairs, looks_for, file, set) = row?;
        has_lead |= row_id == lead;
        if repairs {
            recovery.push(row_id);
            wanted = wanted.max(looks_for);
            if let Some(set) = set.filter(|set| !sets.contains(set)) {
                sets.push(set);
            }
        } else {
            files.insert(file.unwrap_or_else(|| row_id.to_le_bytes().to_vec()));
        }
        ids.push(row_id);
        groups.push(group.unwrap_or_default());
        size += bytes;
    }
    // the set's other volumes, posted to groups of other shards by the same poster: the same sealed set is the same
    // upload. Anyone can repost a set's public Main packet, soo another poster's volume is not
    let poster = if sets.is_empty() { None } else { poster_of(conn, lead)? };
    for (set, poster) in sets.iter().zip(std::iter::repeat(poster.as_deref())) {
        let Some(poster) = poster else { break };
        for other in crate::archive::store::releases_of_set_with(conn, set, 0, poster) {
            if ids.contains(&other) || ids.len() > LINKED_ROWS || !par2_only(conn, other)? {
                continue;
            }
            let Some((group, bytes)) = group_and_size(conn, other)? else { continue };
            recovery.push(other);
            ids.push(other);
            groups.push(group);
            size += bytes;
        }
    }
    if !has_lead || ids.len() < 2 || ids.len() > LINKED_ROWS {
        return Ok(None);
    }
    ids.sort_unstable();
    recovery.sort_unstable();
    groups.sort_unstable();
    groups.dedup();
    // a file of the set that hasn't turned up is missing from the download, however whole the rest is
    // each row is a file of its own, soo each is checked alone: rows sharing a name must not fill each other's gaps
    let mut each_whole = true;
    for row in &ids {
        each_whole &= union_files(conn, &[*row])?.complete_by_files();
    }
    let complete = files.len() as i64 >= wanted && each_whole;
    Ok(Some(SplitPost { ids, groups, size, complete, lead: Some(lead), recovery }))
}

/// All of the post a row is part of: an upload linked by its PAR2 set, or the same post cut across groups or files.
pub fn whole_post_with(conn: &Connection, release: &crate::search::ReleaseRow) -> Result<Option<SplitPost>> {
    if let Some(post) = linked_post_with(conn, release.id)? {
        if let Ok(Some(mut split)) = split_post_with(conn, release)
            && post.ids.iter().all(|id| split.ids.binary_search(id).is_ok())
            && (split.ids.len() > post.ids.len() || (split.complete && !post.complete))
        {
            split.lead = post.lead;
            split.recovery = post.recovery;
            return Ok(Some(split));
        }
        return Ok(Some(post));
    }
    split_post_with(conn, release)
}

/// Whether a single-row release's files are all complete under the current 0- or 1-indexed file counter rule.
pub fn single_post_complete(conn: &Connection, id: i64) -> Result<bool> {
    Ok(union_files(conn, &[id])?.complete())
}

/// How far apart a split post's groups can have got their articles, in days. Each row keeps its newest article's date,
/// and real ones are minutes apart.
const SPLIT_POST_DAYS: f64 = 1.0;

/// The most groups a split post can span. Real ones have a handful, and every file of its NZB lists them all.
const SPLIT_POST_GROUPS: usize = 16;

/// The rest of this release's post in other groups or suffix rows, if it has any. None: it's all here, or the rows that
/// look like it don't add up to one upload. Name and poster are only what the headers say, and anyone can copy those to
/// another group, soo any doubt leaves every row on its own, and every row of a post gets the same answer.
pub fn split_post_with(conn: &Connection, release: &crate::search::ReleaseRow) -> Result<Option<SplitPost>> {
    let posted = names_with(conn, release.id)?.map_or(release.name.clone(), |n| n.posted);
    let exact = exact_split_post(conn, &posted, &release.poster)?;
    if let Some(stem_post) = split_stem_post(conn, &posted, release)?
        && exact.as_ref().is_none_or(|e| stem_post.ids.len() > e.ids.len())
    {
        return Ok(Some(stem_post));
    }
    Ok(exact)
}

fn exact_split_post(conn: &Connection, posted: &str, poster: &Option<String>) -> Result<Option<SplitPost>> {
    let (mut ids, mut groups, mut size, mut dates) = (Vec::new(), Vec::new(), 0, Vec::new());
    for s in 0..store::SHARDS {
        let mut stmt = conn.prepare_cached(&split_post_sql(s))?;
        let rows = stmt.query_map(rusqlite::params![posted, poster], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get::<_, i64>(2)?,
                r.get::<_, Option<f64>>(3)?,
            ))
        })?;
        for row in rows {
            let (id, group, bytes, date) = row?;
            ids.push(id);
            groups.push(group);
            size += bytes;
            dates.push(date);
        }
    }
    if !(2..=SPLIT_POST_GROUPS).contains(&ids.len()) || !close_together(&dates) {
        return Ok(None);
    }
    ids.sort_unstable();
    groups.sort_unstable();
    groups.dedup();
    let files = union_files(conn, &ids)?;
    if !files.one_upload() {
        return Ok(None);
    }
    Ok(Some(SplitPost { ids, groups, size, complete: files.complete(), lead: None, recovery: Vec::new() }))
}

#[derive(Clone)]
struct StemCandidate {
    id: i64,
    group: String,
    bytes: i64,
    date: Option<f64>,
}

fn split_stem_post(conn: &Connection, posted: &str, release: &crate::search::ReleaseRow) -> Result<Option<SplitPost>> {
    let stem = post_stem(posted);
    if stem.len() < 4 {
        return Ok(None);
    }
    let dot_lo = format!("{stem}.");
    let dot_hi = format!("{stem}/");
    let mut all = Vec::new();
    for s in 0..store::SHARDS {
        let mut stmt = conn.prepare_cached(&split_stem_sql(s))?;
        let rows = stmt.query_map(rusqlite::params![stem, dot_lo, dot_hi], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                r.get::<_, i64>(3)?,
                r.get::<_, Option<f64>>(4)?,
            ))
        })?;
        let mut shard_rows = 0;
        for row in rows {
            shard_rows += 1;
            if shard_rows > LINKED_ROWS {
                return Ok(None);
            }
            let (id, name, group, bytes, date) = row?;
            if post_stem(&name).eq_ignore_ascii_case(&stem) {
                all.push(StemCandidate { id, group, bytes, date });
                if all.len() > LINKED_ROWS {
                    return Ok(None);
                }
            }
        }
    }
    if all.len() < 2 {
        return Ok(None);
    }
    let Some(anchor_date) = all.iter().find(|c| c.id == release.id).and_then(|c| c.date) else {
        return Ok(None);
    };
    let close: Vec<StemCandidate> =
        all.into_iter().filter(|c| c.date.is_some_and(|d| (d - anchor_date).abs() <= SPLIT_POST_DAYS)).collect();
    let Some(linked) = linked_stem_candidates(conn, release.id, &close)? else {
        return Ok(None);
    };
    try_stem_subset(conn, &stem, &linked)
}

fn candidate_lead(conn: &Connection, id: i64) -> Result<Option<i64>> {
    let s = store::shard_of_id(id);
    let Ok(mut stmt) = conn.prepare_cached(&format!("select lead_id from s{s}.upload_link where release_id = ?1"))
    else {
        return Ok(None);
    };
    stmt.query_row([id], |r| r.get::<_, i64>(0)).optional()
}

fn linked_stem_candidates(
    conn: &Connection,
    anchor_id: i64,
    close: &[StemCandidate],
) -> Result<Option<Vec<StemCandidate>>> {
    let Some(lead) = candidate_lead(conn, anchor_id)? else {
        return Ok(None);
    };
    let mut subset = Vec::new();
    for c in close {
        if candidate_lead(conn, c.id)? == Some(lead) {
            subset.push(c.clone());
        }
    }
    if subset.len() < 2 || !subset.iter().any(|c| c.id == anchor_id) {
        return Ok(None);
    }
    Ok(Some(subset))
}

fn try_stem_subset(conn: &Connection, stem: &str, subset: &[StemCandidate]) -> Result<Option<SplitPost>> {
    if !(2..=LINKED_ROWS).contains(&subset.len()) {
        return Ok(None);
    }
    let dates: Vec<Option<f64>> = subset.iter().map(|c| c.date).collect();
    if !close_together(&dates) {
        return Ok(None);
    }
    let mut ids: Vec<i64> = subset.iter().map(|c| c.id).collect();
    let mut groups: Vec<String> = subset.iter().map(|c| c.group.clone()).collect();
    let size: i64 = subset.iter().map(|c| c.bytes).sum();
    ids.sort_unstable();
    groups.sort_unstable();
    groups.dedup();
    if groups.len() > SPLIT_POST_GROUPS {
        return Ok(None);
    }
    let files = union_files(conn, &ids)?;
    if !files.one_stem_upload(stem) {
        return Ok(None);
    }
    Ok(Some(SplitPost { ids, groups, size, complete: files.complete(), lead: None, recovery: Vec::new() }))
}

/// A shard's rows of one name and poster. `+poster` keeps the lookup on the name index: with no statistics SQLite
/// would rather walk `idx_release_poster`, every row a busy poster ever posted, and a page of results took minutes.
/// Parameters: ?1 the posted name, ?2 the poster.
fn split_post_sql(s: usize) -> String {
    format!(
        "select id, group_name, coalesce(size, 0), julianday(posted_date) from s{s}.releases
         where name = ?1 and +poster is ?2 and not atlas_blocked(group_name) limit 64"
    )
}

/// A shard's rows matching a release stem or `stem.<suffix>` via `idx_release_unique(name, group_name)`.
/// Parameters: ?1 the stem, ?2 `stem.`, ?3 `stem/`.
fn split_stem_sql(s: usize) -> String {
    format!(
        "select id, name, group_name, coalesce(size, 0), julianday(posted_date) from s{s}.releases
         where (name = ?1 or (name >= ?2 and name < ?3)) and not atlas_blocked(group_name) limit {}",
        LINKED_ROWS + 1
    )
}

fn close_together(dates: &[Option<f64>]) -> bool {
    let Some(dates) = dates.iter().copied().collect::<Option<Vec<f64>>>() else { return false };
    let (first, last) = dates.iter().fold((f64::MAX, f64::MIN), |(lo, hi), d| (lo.min(*d), hi.max(*d)));
    last - first <= SPLIT_POST_DAYS
}

/// A file across releases: what's expected of it, and which parts any of them has.
struct UnionFile {
    expected: Option<i64>,
    seen: Vec<u8>,
    /// its subject, less the part number
    subject: Option<String>,
}

/// Each file across these releases, by name, and what says whether they're one upload.
#[derive(Default)]
struct UnionFiles {
    files: BTreeMap<String, UnionFile>,
    /// what each file says the post has
    file_totals: BTreeSet<Option<i64>>,
    /// each release's filenames
    rows: Vec<BTreeSet<String>>,
    /// two releases have a file they don't agree on, or a part of it both have
    conflict: bool,
    /// whether any file carries a 0-indexed `[0/N]` or `[00/N]` counter
    zero_indexed: bool,
}

impl UnionFiles {
    /// Whether the releases can be parts of one upload. Each article went to one group, soo no part is in two of them,
    /// they say the same of every file they share, and each shares one with the rest.
    fn one_upload(&self) -> bool {
        let mut totals = self.file_totals.iter();
        let total_fits = match (totals.next(), totals.next()) {
            (Some(Some(total)), None) => self.files.len() as i64 <= *total + i64::from(self.zero_indexed),
            // no count to go by, soo every release has the same files
            (Some(None), None) => self.rows.windows(2).all(|pair| pair[0] == pair[1]),
            _ => false,
        };
        !self.conflict && total_fits && self.linked()
    }

    fn one_stem_upload(&self, stem: &str) -> bool {
        if self.conflict || self.files.is_empty() || self.file_totals.contains(&None) {
            return false;
        }
        if !self.files.keys().all(|f| post_stem(f).eq_ignore_ascii_case(stem)) {
            return false;
        }
        let non_null: Vec<i64> = self.file_totals.iter().copied().flatten().collect();
        match non_null.as_slice() {
            [total] if *total >= 2 => {
                let want = *total + i64::from(self.zero_indexed);
                self.files.len() >= 2 && (self.files.len() as i64) <= want
            }
            _ => false,
        }
    }

    fn linked(&self) -> bool {
        let Some((first, rest)) = self.rows.split_first() else { return false };
        let mut reached = first.clone();
        let mut left: Vec<&BTreeSet<String>> = rest.iter().collect();
        while let Some(at) = left.iter().position(|row| !row.is_disjoint(&reached)) {
            reached.extend(left.swap_remove(at).iter().cloned());
        }
        left.is_empty()
    }

    /// Whether every file is there with exactly its parts, whatever the subjects say the post has: a linked upload's
    /// files each came with a subject of their own.
    fn complete_by_files(&self) -> bool {
        !self.files.is_empty()
            && self.files.values().all(|f| f.expected.is_some_and(|want| store::is_exactly(&f.seen, want)))
    }

    /// Whether every file the post says it has is there with exactly its parts.
    fn complete(&self) -> bool {
        if self.file_totals.len() > 1 {
            return false;
        }
        let total = self.file_totals.iter().copied().flatten().next_back();
        !self.files.is_empty()
            && total.is_none_or(|total| self.files.len() as i64 == total + i64::from(self.zero_indexed))
            && self.files.values().all(|f| f.expected.is_some_and(|want| store::is_exactly(&f.seen, want)))
    }
}

/// A subject without the number that changes from part to part: the N in `(N/M)`, or in `[N/M]` for a post where
/// `(1/1)` says each article is a whole file, soo the bracket counter is the part number (`parser::remap_bracket_parts`).
fn subject_of_file(subject: &str, expected: Option<i64>) -> String {
    let mut subject = subject.to_string();
    let mut one_article_files = false;
    for (open, _) in subject.rmatch_indices('(') {
        let rest = &subject[open + 1..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let counter = rest[digits..].trim_start();
        if digits > 0 && counter.starts_with('/') {
            one_article_files = counter[1..].trim_start().starts_with("1)");
            subject = format!("{}({counter}", &subject[..open]);
            break;
        }
    }
    if one_article_files && let Some(end) = expected.and_then(|parts| subject.find(&format!("/{parts}]"))) {
        let number = subject[..end].trim_end_matches(|c: char| c.is_ascii_digit());
        if number.ends_with('[') && number.len() < end {
            return format!("{number}{}", &subject[end..]);
        }
    }
    subject
}

fn union_files(conn: &Connection, ids: &[i64]) -> Result<UnionFiles> {
    let mut union = UnionFiles::default();
    for id in ids {
        let s = store::shard_of_id(*id);
        let mut stmt = conn.prepare_cached(&format!(
            "select filename, expected, seen, file_total, subject from s{s}.files where release_id = ?"
        ))?;
        let rows = stmt.query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Vec<u8>>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        let mut filenames = BTreeSet::new();
        for row in rows {
            let (filename, expected, seen, total, subject) = row?;
            union.file_totals.insert(total);
            union.zero_indexed |= subject.as_deref().is_some_and(crate::parser::has_zero_file_index);
            filenames.insert(filename.clone());
            let subject = subject.as_deref().map(|subject| subject_of_file(subject, expected));
            match union.files.entry(filename) {
                Entry::Vacant(file) => {
                    file.insert(UnionFile { expected, seen, subject });
                }
                Entry::Occupied(mut file) => {
                    let file = file.get_mut();
                    union.conflict |=
                        expected.is_none() || file.expected != expected || subject.is_none() || file.subject != subject;
                    if file.seen.len() < seen.len() {
                        file.seen.resize(seen.len(), 0);
                    }
                    for (slot, bits) in file.seen.iter_mut().zip(seen) {
                        union.conflict |= *slot & bits != 0;
                        *slot |= bits;
                    }
                }
            }
        }
        union.rows.push(filenames);
    }
    Ok(union)
}

/// What's in the whole post: each file once, whichever groups have its parts.
pub fn contents_of_split(conn: &Connection, post: &SplitPost) -> Result<Contents> {
    contents_of(conn, &post.ids, &post.recovery)
}

/// A From header, split up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Poster {
    pub name: Option<String>,
    pub email: Option<String>,
}

/// `Name <a@b>`, `a@b (Name)`, or either part alone.
pub fn parse_poster(raw: &str) -> Poster {
    let raw = raw.trim();
    let tidy = |s: &str| {
        let s = s.trim().trim_matches('"').trim();
        (!s.is_empty()).then(|| s.to_string())
    };
    if let (Some(open), Some(close)) = (raw.rfind('<'), raw.rfind('>'))
        && open < close
    {
        return Poster { name: tidy(&raw[..open]), email: tidy(&raw[open + 1..close]).filter(|e| e.contains('@')) };
    }
    if let (Some(open), true) = (raw.find('('), raw.ends_with(')')) {
        let address = raw[..open].trim();
        if address.contains('@') {
            return Poster { name: tidy(&raw[open + 1..raw.len() - 1]), email: tidy(address) };
        }
    }
    if raw.contains('@') && !raw.contains(' ') {
        return Poster { name: None, email: Some(raw.to_string()) };
    }
    Poster { name: tidy(raw), email: None }
}

/// Domains posters use when they don't want a reply. Most made-up addresses aren't on this list,
/// soo a miss says nothing about an address being real.
const MADE_UP_DOMAINS: &[&str] = &[
    "example.com",
    "example.org",
    "example.net",
    "invalid",
    "invalid.com",
    "localhost",
    "nowhere.com",
    "nowhere.org",
    "none.com",
    "none.net",
    "nomail.com",
    "no.spam",
    "nospam.com",
    "nospam.org",
    "spam.com",
    "fake.com",
    "anon.com",
    "anonymous.com",
    "unknown.com",
    "test.com",
    "domain.com",
    "email.com",
];

pub fn looks_made_up(email: &str) -> bool {
    let Some((_, domain)) = email.rsplit_once('@') else { return true };
    let domain = domain.to_ascii_lowercase();
    !domain.contains('.')
        || domain.ends_with(".invalid")
        || domain.ends_with(".local")
        || MADE_UP_DOMAINS.iter().any(|known| known.eq_ignore_ascii_case(&domain))
}

static PASSWORD: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)]
    Regex::new(r"(?i)\{\{([^{}\s]{1,128})\}\}|\b(?:password|passwd|passwort|pass|pw)\b\s*(?:is\s*)?[:=]\s*([^\s\[\]\x22']{1,128})").unwrap()
});

/// A password a poster left in a name, subject or NFO: `{{secret}}` or `password: secret`.
/// A guess from text, soo nothing has tried it.
pub fn find_password(text: &str) -> Option<String> {
    let hit = PASSWORD.captures(text)?;
    // the braces say where it ends: only a bare password can have a sentence's punctuation after it
    let found = match hit.get(1) {
        Some(braced) => braced.as_str(),
        None => hit.get(2)?.as_str().trim_matches(|c: char| c == '.' || c == ','),
    };
    let none = ["none", "no", "n/a", "na", "nil", "-", "free", "not", "yes", ""];
    (!none.iter().any(|word| found.eq_ignore_ascii_case(word))).then(|| found.to_string())
}

/// What the page can say without reading any article.
pub struct PasswordHint {
    pub password: String,
    pub source: &'static str,
}

pub fn password_hint(id: i64, names: Option<&Names>) -> Option<PasswordHint> {
    let conn = db::open().ok()?;
    password_hint_with(&conn, id, names)
}

pub fn password_hint_with(conn: &Connection, id: i64, names: Option<&Names>) -> Option<PasswordHint> {
    let hint = |password: String, source| Some(PasswordHint { password, source });
    if let Some(password) = crate::archive::store::nfo_password(conn, id) {
        return hint(password, "the NFO");
    }
    for name in names.into_iter().flat_map(|n| [Some(&n.posted), n.display.as_ref()]).flatten() {
        if let Some(password) = find_password(name) {
            return hint(password, "the release name");
        }
    }
    let s = store::shard_of_id(id);
    let mut stmt = conn
        .prepare(&format!("select filename, subject from s{s}.files where release_id = ? order by id limit 64"))
        .ok()?;
    let files: Vec<(String, Option<String>)> =
        stmt.query_map([id], |r| Ok((r.get(0)?, r.get(1)?))).ok()?.filter_map(|row| row.ok()).collect();
    for (filename, subject) in &files {
        if let Some(password) = find_password(filename) {
            return hint(password, "a file name");
        }
        if let Some(password) = subject.as_deref().and_then(find_password) {
            return hint(password, "the subject");
        }
    }
    // the harvester's other spellings: `{{pass}}`, `[pw: x]`, `pass=x`, in the names and every subject
    let texts = names
        .into_iter()
        .flat_map(|n| [Some(n.posted.clone()), n.display.clone()])
        .flatten()
        .chain(files.into_iter().filter_map(|(_, subject)| subject));
    for text in texts {
        if let Some(password) = harvested_password(&text) {
            return hint(password, "the subject");
        }
    }
    None
}

/// The likeliest password `archive::PasswordHarvester` reads out of one line, if any.
pub fn harvested_password(text: &str) -> Option<String> {
    let none = std::collections::HashMap::new();
    crate::archive::PasswordHarvester::extract_candidates(text, None, &none)
        .into_iter()
        .find(|p| !p.is_empty() && p.len() <= 128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_split_post_is_looked_up_by_name_not_poster() {
        let conn = Connection::open_in_memory().unwrap();
        crate::filters::register_sql(&conn).unwrap();
        conn.execute_batch(
            "attach ':memory:' as s0;
             create table s0.releases (id INTEGER PRIMARY KEY, name TEXT, group_name TEXT, poster TEXT,
                 posted_date TEXT, size INTEGER);
             create unique index s0.idx_release_unique on releases(name, group_name);
             create index s0.idx_release_poster on releases(poster);",
        )
        .unwrap();
        let plan: Vec<String> = conn
            .prepare(&format!("explain query plan {}", split_post_sql(0)))
            .unwrap()
            .query_map(["a.name", "someone"], |r| r.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let plan = plan.join(" | ");
        assert!(plan.contains("idx_release_unique") && !plan.contains("idx_release_poster"), "{plan}");
        let stem_plan: Vec<String> = conn
            .prepare(&format!("explain query plan {}", split_stem_sql(0)))
            .unwrap()
            .query_map(["a.name", "a.name.", "a.name/"], |r| r.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let stem_plan = stem_plan.join(" | ");
        assert!(stem_plan.contains("idx_release_unique") && !stem_plan.contains("idx_release_poster"), "{stem_plan}");
    }

    /// `parts` 1..=n seen
    fn seen(n: i64) -> Vec<u8> {
        let mut bits = vec![0u8; (n as usize) / 8 + 1];
        for p in 1..=n as usize {
            bits[p / 8] |= 1 << (p % 8);
        }
        bits
    }

    /// A real shard's tables, attached as s0 the way `db::open` does, with an upload of one file and two volumes.
    fn linked_shard() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.s0.db");
        store::create_shard(&path).unwrap();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("attach database ?1 as s0", [path.to_string_lossy()]).unwrap();
        // the file, all 3 parts of it, and two volumes posted under names of their own
        for (id, size, parts) in [(8, 3_096_000, 3), (16, 900, 1), (24, 200_900, 1)] {
            conn.execute(
                "insert into s0.releases (id, name, group_name, poster, posted_date, size, complete, parts)
                 values (?1, ?2, 'a.b.test', 'p', '2026-08-22', ?3, 1, ?4)",
                rusqlite::params![id, format!("random{id}"), size, parts],
            )
            .unwrap();
            conn.execute(
                "insert into s0.files (id, release_id, filename, subject, expected, seen) values (?1, ?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, format!("random{id}"), format!("\"random{id}\" yEnc (1/{parts})"), parts, seen(parts)],
            )
            .unwrap();
        }
        (dir, conn)
    }

    #[test]
    fn a_linked_upload_is_all_its_rows() {
        let (_dir, conn) = linked_shard();
        assert_eq!(linked_post_with(&conn, 16).unwrap(), None, "nothing linked yet");
        conn.execute_batch(
            "insert into s0.upload_link (release_id, lead_id, recovery, wanted) values (8, 8, 0, null), (16, 8, 1, 1), (24, 8, 1, 1);",
        )
        .unwrap();
        for id in [8, 16, 24] {
            let post = linked_post_with(&conn, id).unwrap().unwrap();
            assert_eq!(post.ids, [8, 16, 24]);
            assert_eq!(post.size, 3_096_000 + 900 + 200_900);
            assert_eq!((post.listed_id(), post.lead), (8, Some(8)));
            assert!(post.complete);
            assert_eq!(post.recovery, [16, 24]);
        }
        let contents = contents_of_split(&conn, &linked_post_with(&conn, 8).unwrap().unwrap()).unwrap();
        assert_eq!((contents.files, contents.par2_files, contents.recovery_volumes), (3, 2, 2));
        assert_eq!(contents.data_articles_expected, 3);
        // a row that's linked is whole whatever it says of itself
        let row = crate::search::ReleaseRow { id: 16, complete: true, ..Default::default() };
        assert_eq!(whole_post_with(&conn, &row).unwrap().unwrap().lead, Some(8));
        // a set that looks for two files, with one found, isn't all there
        conn.execute_batch("update s0.upload_link set wanted = 2 where release_id = 24").unwrap();
        assert!(!linked_post_with(&conn, 16).unwrap().unwrap().complete);
    }

    #[test]
    fn a_file_posted_twice_counts_once() {
        let (_dir, conn) = linked_shard();
        conn.execute_batch(
            "insert into s0.upload_link (release_id, lead_id, recovery, wanted, par2_file)
             values (8, 8, 0, null, x'01'), (16, 8, 0, null, x'01'), (24, 8, 1, 2, null);",
        )
        .unwrap();
        assert!(!linked_post_with(&conn, 8).unwrap().unwrap().complete, "two copies of one file of two");
        conn.execute_batch("update s0.upload_link set par2_file = x'02' where release_id = 16").unwrap();
        assert!(linked_post_with(&conn, 8).unwrap().unwrap().complete);
    }

    #[test]
    fn same_named_files_of_a_linked_upload_do_not_fill_each_others_gaps() {
        let (_dir, conn) = linked_shard();
        conn.execute_batch(
            "insert into s0.upload_link (release_id, lead_id, recovery, wanted) values (8, 8, 0, null), (16, 8, 1, 1), (24, 8, 1, 1);
             update s0.files set filename = 'same.bin' where id in (8, 16);",
        )
        .unwrap();
        conn.execute("update s0.files set expected = 3, seen = ?1 where id = 8", [vec![0b011u8]]).unwrap();
        conn.execute("update s0.files set expected = 3, seen = ?1 where id = 16", [vec![0b101u8]]).unwrap();
        assert!(!linked_post_with(&conn, 8).unwrap().unwrap().complete);
    }

    #[test]
    fn a_link_whose_file_is_gone_is_no_link() {
        let (_dir, conn) = linked_shard();
        conn.execute_batch(
            "insert into s0.upload_link (release_id, lead_id, recovery) values (16, 8, 1), (24, 8, 1); delete from s0.releases where id = 8;",
        )
        .unwrap();
        assert_eq!(linked_post_with(&conn, 16).unwrap(), None);
    }

    #[test]
    fn an_estimate_goes_by_the_whole_of_the_data() {
        let half_there = Contents {
            data_articles_expected: 10,
            data_articles_seen: 5,
            data_bytes: 5_000,
            recovery_articles_expected: 3,
            recovery_articles_seen: 3,
            recovery_bytes: 3_000,
            ..Contents::default()
        };
        assert!(!half_there.repairable(), "3 can't stand in for 5");
        assert_eq!(half_there.repair_per_mille(), Some(300));
        // what's gone of the recovery volumes is already out of their size
        let thin = Contents {
            data_articles_expected: 100,
            data_articles_seen: 92,
            data_bytes: 92_000,
            recovery_articles_expected: 20,
            recovery_articles_seen: 10,
            recovery_bytes: 10_000,
            ..Contents::default()
        };
        assert!(thin.repairable());
        // overviews can leave sizes out, and no sizes say nothing
        let unsized_data = Contents { data_bytes: 0, recovery_bytes: 0, ..half_there };
        assert!(!unsized_data.repairable());
    }

    #[test]
    fn one_block_cant_repair_two_of_ten_thousand() {
        let contents = |seen| Contents {
            data_articles_expected: 10_000,
            data_articles_seen: seen,
            recovery_articles_expected: 1,
            recovery_articles_seen: 1,
            recovery_blocks: 1,
            par2_block: Some(1),
            par2_blocks: Some(10_000),
            ..Contents::default()
        };
        assert!(!contents(9_998).repairable());
        assert!(contents(9_999).repairable());
    }

    fn poster(name: Option<&str>, email: Option<&str>) -> Poster {
        Poster { name: name.map(Into::into), email: email.map(Into::into) }
    }

    #[test]
    fn a_from_header_splits_in_each_shape() {
        assert_eq!(parse_poster("Some One <a@b.com>"), poster(Some("Some One"), Some("a@b.com")));
        assert_eq!(parse_poster("\"Some One\" <a@b.com>"), poster(Some("Some One"), Some("a@b.com")));
        assert_eq!(parse_poster("<a@b.com>"), poster(None, Some("a@b.com")));
        assert_eq!(parse_poster("a@b.com (Some One)"), poster(Some("Some One"), Some("a@b.com")));
        assert_eq!(parse_poster("a@b.com"), poster(None, Some("a@b.com")));
        assert_eq!(parse_poster("Just A Name"), poster(Some("Just A Name"), None));
        assert_eq!(parse_poster("Name <not an address>"), poster(Some("Name"), None));
        assert_eq!(parse_poster("  "), poster(None, None));
        assert_eq!(parse_poster("a > b < c"), poster(Some("a > b < c"), None));
    }

    #[test]
    fn only_known_throwaway_addresses_are_called_made_up() {
        for fake in ["a@example.com", "a@nowhere.com", "a@host", "a@x.invalid", "a@NoSpam.com", "no at sign"] {
            assert!(looks_made_up(fake), "{fake}");
        }
        assert!(!looks_made_up("a@gmail.com"));
    }

    #[test]
    fn missing_and_repair_are_shares() {
        let c = Contents {
            articles_seen: 97,
            articles_expected: 100,
            data_bytes: 1000,
            recovery_bytes: 100,
            ..Contents::default()
        };
        assert_eq!(c.missing_per_mille(), Some(30));
        assert_eq!(c.repair_per_mille(), Some(100));
        assert_eq!(Contents::default().missing_per_mille(), None);
        assert_eq!(Contents::default().repair_per_mille(), None);
    }

    #[test]
    fn repair_compares_missing_data_with_the_recovery_that_is_there() {
        let holes = Contents {
            data_articles_seen: 97,
            data_articles_expected: 100,
            recovery_articles_seen: 10,
            recovery_articles_expected: 10,
            data_bytes: 1000,
            recovery_bytes: 100,
            ..Contents::default()
        };
        assert!(holes.repairable(), "3% missing, 10% repairable");
        let lost = Contents { recovery_articles_seen: 2, recovery_bytes: 20, ..holes.clone() };
        assert!(!lost.repairable(), "most of the recovery is gone too");
        let none = Contents { recovery_articles_expected: 0, recovery_articles_seen: 0, ..holes };
        assert!(!none.repairable());
    }

    #[test]
    fn exact_repair_counts_blocks() {
        assert_eq!(recovery_blocks("a.vol003+04.par2"), 4);
        assert_eq!(recovery_blocks("a.vol001-003.par2"), 2);
        assert_eq!(recovery_blocks("a.vol063-101.par2"), 38);
        assert_eq!(recovery_blocks("a.vol000-000.par2"), 1);
        assert_eq!(recovery_blocks("a.par2"), 0);
        let c = Contents { recovery_blocks: 10, par2_block: Some(1000), par2_blocks: Some(100), ..Contents::default() };
        assert_eq!(c.exact_repair_per_mille(), Some(100), "10 of 100 blocks");
        assert_eq!(Contents::default().exact_repair_per_mille(), None);
    }

    #[test]
    fn per_file_cross_group_split_post_merges_by_stem() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        crate::filters::register_sql(&conn).unwrap();
        for s in 0..store::SHARDS {
            let path = dir.path().join(format!("atlas.s{s}.db"));
            store::create_shard(&path).unwrap();
            conn.execute(&format!("attach database ?1 as s{s}"), [path.to_string_lossy()]).unwrap();
        }
        let stem = "One.From.The.Heart.1981.2160p.UHD.BluRay.DV.REMUX.HDR10.HEVC.DTS-HD-MA.5.1-UnKn0wn";
        let rows = [
            (8i64, stem.to_string(), "alt.binaries.multimedia.aviation", "p1@nyuu", format!("{stem}.par2"), 1, 1, 1000),
            (
                16,
                format!("{stem}.vol001-003"),
                "alt.binaries.multimedia.alias",
                "p2@nyuu",
                format!("{stem}.vol001-003.par2"),
                1,
                1,
                2000,
            ),
            (
                24,
                format!("{stem}.vol003-007"),
                "alt.binaries.multimedia.alias",
                "p3@nyuu",
                format!("{stem}.vol003-007.par2"),
                1,
                2,
                3000,
            ),
            (
                32,
                format!("{stem}.vol003-007"),
                "alt.binaries.multimedia.aviation",
                "p3@nyuu",
                format!("{stem}.vol003-007.par2"),
                2,
                2,
                3000,
            ),
            (
                40,
                format!("{stem}.mkv"),
                "alt.binaries.multimedia.alias",
                "p4@nyuu",
                format!("{stem}.mkv"),
                1,
                1,
                50_000,
            ),
        ];
        for (id, rname, grp, poster, fname, part, expected, bytes) in rows {
            conn.execute(
                "insert into s0.releases (id, name, group_name, poster, posted_date, size, complete, parts, file_total)
                 values (?1, ?2, ?3, ?4, '2025-11-28 02:30:00', ?5, 0, 1, 4)",
                rusqlite::params![id, rname, grp, poster, bytes],
            )
            .unwrap();
            let mut bits = vec![0u8; 1];
            bits[0] |= 1 << part;
            conn.execute(
                "insert into s0.files (id, release_id, filename, subject, expected, file_total, seen)
                 values (?1, ?1, ?2, ?3, ?4, 4, ?5)",
                rusqlite::params![id, fname, format!("[01/04] - \"{fname}\" yEnc ({part}/{expected})"), expected, bits],
            )
            .unwrap();
        }
        // Without upload_link provenance, cross-poster rows must not merge across different posters
        let unlinked_row = crate::search::ReleaseRow {
            id: 16,
            name: format!("{stem}.vol001-003"),
            group_name: "alt.binaries.multimedia.alias".into(),
            poster: Some("p2@nyuu".into()),
            complete: false,
            ..Default::default()
        };
        assert_eq!(whole_post_with(&conn, &unlinked_row).unwrap(), None);

        // Link every row of the upload via upload_link
        conn.execute_batch(
            "insert into s0.upload_link (release_id, lead_id, recovery, wanted)
             values (40, 40, 0, null), (8, 40, 1, 1), (16, 40, 1, 1), (24, 40, 1, 1), (32, 40, 1, 1);",
        )
        .unwrap();

        // An unlinked third-party row sharing the stem is rejected and cannot poison the merged post
        conn.execute(
            "insert into s0.releases (id, name, group_name, poster, posted_date, size, complete, parts, file_total)
             values (48, ?1, 'alt.binaries.multimedia.alias', 'evil@attacker', '2025-11-28 02:30:00', 999, 1, 1, 4)",
            rusqlite::params![format!("{stem}.exe")],
        )
        .unwrap();
        conn.execute(
            "insert into s0.files (id, release_id, filename, subject, expected, file_total, seen)
             values (48, 48, ?1, ?2, 1, 4, ?3)",
            rusqlite::params![format!("{stem}.exe"), format!("[04/04] - \"{stem}.exe\" yEnc (1/1)"), vec![0b10u8]],
        )
        .unwrap();

        for (id, rname, poster, complete) in [
            (8, stem.to_string(), "p1@nyuu", true),
            (16, format!("{stem}.vol001-003"), "p2@nyuu", false),
            (24, format!("{stem}.vol003-007"), "p3@nyuu", false),
            (32, format!("{stem}.vol003-007"), "p3@nyuu", false),
            (40, format!("{stem}.mkv"), "p4@nyuu", true),
        ] {
            let row = crate::search::ReleaseRow {
                id,
                name: rname,
                group_name: "alt.binaries.multimedia.alias".into(),
                poster: Some(poster.into()),
                complete,
                ..Default::default()
            };
            let post = whole_post_with(&conn, &row).unwrap().unwrap();
            assert_eq!(post.ids, [8, 16, 24, 32, 40]);
            assert_eq!(post.groups, ["alt.binaries.multimedia.alias", "alt.binaries.multimedia.aviation"]);
            assert_eq!(post.size, 1000 + 2000 + 3000 + 3000 + 50_000);
            assert!(post.complete);
        }
        let attacker_row = crate::search::ReleaseRow {
            id: 48,
            name: format!("{stem}.exe"),
            group_name: "alt.binaries.multimedia.alias".into(),
            poster: Some("evil@attacker".into()),
            complete: true,
            ..Default::default()
        };
        assert_eq!(whole_post_with(&conn, &attacker_row).unwrap(), None);
    }

    #[test]
    fn union_files_complete_honors_non_null_file_total_and_stem_overflow_returns_none() {
        let mut uf = UnionFiles::default();
        uf.file_totals.insert(None);
        uf.file_totals.insert(Some(3));
        uf.files.insert("a.par2".into(), UnionFile { expected: Some(1), seen: vec![0b10], subject: None });
        uf.files.insert("a.mkv".into(), UnionFile { expected: Some(1), seen: vec![0b10], subject: None });
        assert!(!uf.complete(), "2 files when a non-null file_total says 3 must not be complete");

        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        crate::filters::register_sql(&conn).unwrap();
        for s in 0..store::SHARDS {
            let path = dir.path().join(format!("atlas.s{s}.db"));
            store::create_shard(&path).unwrap();
            conn.execute(&format!("attach database ?1 as s{s}"), [path.to_string_lossy()]).unwrap();
        }
        let stem = "Overflow.Stem.2026.1080p-GRP";
        for i in 0..=(LINKED_ROWS + 1) {
            let id = (i as i64 + 1) * 8;
            conn.execute(
                "insert into s0.releases (id, name, group_name, poster, posted_date, size, complete, parts, file_total)
                 values (?1, ?2, ?3, 'same@poster', '2025-11-28 02:30:00', 100, 0, 1, 2000)",
                rusqlite::params![id, format!("{stem}.part{i:04}"), format!("alt.binaries.g{i}")],
            )
            .unwrap();
        }
        let row = crate::search::ReleaseRow {
            id: 8,
            name: format!("{stem}.part0000"),
            group_name: "alt.binaries.g0".into(),
            poster: Some("same@poster".into()),
            complete: false,
            ..Default::default()
        };
        assert_eq!(whole_post_with(&conn, &row).unwrap(), None);
    }

    #[test]
    fn recovery_volumes_are_told_from_the_index() {
        assert!(is_par2("A.PAR2") && !is_recovery_volume("a.par2"));
        assert!(is_recovery_volume("a.vol000+01.par2"));
        assert!(!is_par2("a.rar"));
    }

    #[test]
    fn passwords_are_found_where_posters_leave_them() {
        assert_eq!(find_password("Some.Movie.2024.{{s3cret!}}.rar").as_deref(), Some("s3cret!"));
        assert_eq!(find_password("Password: hunter2").as_deref(), Some("hunter2"));
        assert_eq!(find_password("Password: hunter2.").as_deref(), Some("hunter2"), "the end of a sentence");
        assert_eq!(find_password("Movie.{{.secret,}}.rar").as_deref(), Some(".secret,"), "the braces say where");
        assert_eq!(find_password("PW=abc123, enjoy").as_deref(), Some("abc123"));
        assert_eq!(find_password("password is: abc").as_deref(), Some("abc"));
        assert_eq!(find_password("Password: none"), None);
        assert_eq!(find_password("Passworded.Movie.2024"), None);
        assert_eq!(find_password("Compass: north").as_deref(), None);
        assert_eq!(find_password("nothing here"), None);
    }

    #[test]
    fn name_sources_read_as_words() {
        assert_eq!(name_source_label(10), Some("the NFO"));
        assert_eq!(name_source_label(100), Some("PreDB"));
        assert_eq!(name_source_label(0), None);
    }
}
