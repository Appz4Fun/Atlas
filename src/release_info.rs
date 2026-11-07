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
    contents_of(conn, &[id], &[], Bytes::All)
}

/// Which bytes `contents_of` adds up: reading them means reading every article, for a big post its whole sealed log.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bytes {
    All,
    None,
    /// only where a repair verdict rests on them (`verdict_needs_bytes`)
    ForVerdict,
}

/// Whether a repair verdict needs the bytes: data to repair and recovery volumes, but no PAR2 set read to count
/// blocks with (`Contents::repairable` and `repair_per_mille` then go by sizes). Without data, or without recovery,
/// or with the set read, the counts alone give the same verdict.
fn verdict_needs_bytes(c: &Contents) -> bool {
    c.data_articles_expected > 0 && c.recovery_articles_expected > 0 && c.exact_repair_per_mille().is_none()
}

/// `contents_with` without the bytes: the files, PAR2 volumes and article counts come from each file's row. The bytes
/// (`data_bytes`, `recovery_bytes`, each listed file's) stay 0.
pub fn contents_counts_with(conn: &Connection, id: i64) -> Result<Contents> {
    contents_of(conn, &[id], &[], Bytes::None)
}

/// What a summary of a release (a copy's card) needs: its counts, and its bytes only where a repair verdict rests on
/// them (`verdict_needs_bytes`), soo every verdict is the one the full contents give.
pub fn contents_summary_with(conn: &Connection, id: i64) -> Result<Contents> {
    contents_of(conn, &[id], &[], Bytes::ForVerdict)
}

fn contents_of(conn: &Connection, ids: &[i64], recovery: &[i64], with_bytes: Bytes) -> Result<Contents> {
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
    if let Some((block, blocks)) = ids.iter().find_map(|id| crate::duplicates::par2_sizes(conn, *id)) {
        totals.par2_block = Some(block);
        totals.par2_blocks = Some(blocks);
    }
    let read_bytes = match with_bytes {
        Bytes::All => true,
        Bytes::None => false,
        Bytes::ForVerdict => verdict_needs_bytes(&totals),
    };
    let mut bytes: HashMap<String, i64> = HashMap::new();
    if read_bytes {
        // rows sharing parts (`UnionFiles::overlap`) hold some parts more than once: each part's bytes count once
        let mut counted: HashSet<(Option<String>, i64)> = HashSet::new();
        for id in ids {
            store::each_article(conn, *id, &mut |article| {
                if union.overlap
                    && let Some(part) = article.part
                    && !counted.insert((article.filename.clone(), part))
                {
                    return Ok(());
                }
                *bytes.entry(article.filename.unwrap_or_default()).or_default() += article.bytes.unwrap_or(0);
                Ok(())
            })?;
        }
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
    if let Some(mut out) = crate::shared_catalog::reader::groups_of(name) {
        crate::metrics::names::SHARED_CATALOG_READS.inc(&[("lookup", "groups_of"), ("source", "catalog")], 1);
        sort_groups(&mut out);
        return Ok(out);
    }
    crate::metrics::names::SHARED_CATALOG_READS.inc(&[("lookup", "groups_of"), ("source", "shards")], 1);
    groups_of_with(&db::open()?, name)
}

/// Most articles first, then by name.
fn sort_groups(out: &mut [GroupPosting]) {
    out.sort_by(|a, b| b.articles.cmp(&a.articles).then_with(|| a.group.cmp(&b.group)));
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
    sort_groups(&mut out);
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
pub(crate) const LINKED_ROWS: usize = 1_024;

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
    // same poster put it up. The lead and its link rows share a shard, soo its poster is read there
    let Some(poster) = poster_of(conn, id)? else { return Ok(None) };
    for set in sets {
        for other in 0..store::SHARDS {
            let Ok(mut stmt) = conn.prepare_cached(&format!(
                "select l.lead_id from s{other}.upload_link l join s{other}.releases r on r.id = l.lead_id
                 where l.set_id = ?1 and l.recovery = 1 and r.poster = ?2 limit 1"
            )) else {
                continue;
            };
            if let Some(lead) = stmt.query_row(rusqlite::params![&set, &poster], |r| r.get::<_, i64>(0)).optional()? {
                return Ok(Some(lead));
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
    whole_post_from(conn, release, false)
}

/// `whole_post_with` for one release (its page), the split post's candidates from the shared catalog when reads are on.
/// Lists, the API and the NZB keep the shards' local lookups: a page of rows would cost a catalog round trip each.
pub fn whole_post_from_catalog(conn: &Connection, release: &crate::search::ReleaseRow) -> Result<Option<SplitPost>> {
    whole_post_from(conn, release, true)
}

fn whole_post_from(conn: &Connection, release: &crate::search::ReleaseRow, catalog: bool) -> Result<Option<SplitPost>> {
    if let Some(post) = linked_post_with(conn, release.id)? {
        if let Ok(Some(mut split)) = split_post_from(conn, release, catalog)
            && post.ids.iter().all(|id| split.ids.binary_search(id).is_ok())
            && (split.ids.len() > post.ids.len() || (split.complete && !post.complete))
        {
            split.lead = post.lead;
            split.recovery = post.recovery;
            return Ok(Some(split));
        }
        return Ok(Some(post));
    }
    split_post_from(conn, release, catalog)
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
    split_post_from(conn, release, false)
}

/// `split_post_with`, its candidate rows from the shared catalog when `catalog` and catalog reads are on.
fn split_post_from(conn: &Connection, release: &crate::search::ReleaseRow, catalog: bool) -> Result<Option<SplitPost>> {
    let posted = names_with(conn, release.id)?.map_or(release.name.clone(), |n| n.posted);
    let video = video_name_twin(conn, release.id, &posted)?;
    let exact = exact_split_post(conn, &posted, video.as_deref(), release, catalog)?;
    if let Some(stem_post) = split_stem_post(conn, &posted, release, catalog)?
        && exact.as_ref().is_none_or(|e| stem_post.ids.len() > e.ids.len())
    {
        return Ok(Some(stem_post));
    }
    Ok(exact)
}

/// The other name a group's row of this post can have: the post's video's file name, when the video's first article
/// was the first that group saw of the post (`Name.mkv` beside the `Name` of the groups where a PAR2 file came first),
/// or, for that row, the name without it. Each article went to one group, so every row has the video's parts and asks
/// for the other name.
fn video_name_twin(conn: &Connection, id: i64, posted: &str) -> Result<Option<String>> {
    let lower = posted.to_ascii_lowercase();
    if let Some(ext) = store::VIDEO_EXTENSIONS.iter().find(|ext| lower.ends_with(*ext)) {
        let stem = &posted[..posted.len() - ext.len()];
        return Ok((!stem.trim().is_empty()).then(|| stem.to_string()));
    }
    let s = store::shard_of_id(id);
    let files: Vec<String> = conn
        .prepare_cached(&format!("select filename from s{s}.files where release_id = ?"))?
        .query_map([id], |r| r.get(0))?
        .collect::<Result<_>>()?;
    Ok(files.into_iter().find(|file| {
        file.len() > posted.len()
            && file.is_char_boundary(posted.len())
            && file[..posted.len()] == *posted
            && store::VIDEO_EXTENSIONS.iter().any(|ext| file[posted.len()..].eq_ignore_ascii_case(ext))
    }))
}

/// A row named as a post, in some shard.
struct Named {
    id: i64,
    group: String,
    bytes: i64,
    date: Option<f64>,
    poster: Option<String>,
}

/// The rows the catalog names as candidates, read again from their own shards by id: the shard's row is what's
/// true (a row deleted there, or changed since, the catalog may not have caught up with), its blocked groups are left
/// out as the shards' lookup does, and only rows still `named` stay. Lowest shard first, then by group name, the
/// order of each shard's (name, group) index.
fn reread_candidates(
    conn: &Connection,
    candidates: &[crate::shared_catalog::reader::NamedRow],
    named: impl Fn(&str) -> bool,
) -> Result<Vec<(usize, String, Named)>> {
    let mut out = Vec::new();
    for c in candidates {
        let s = store::shard_of_id(c.id);
        let row = conn
            .prepare_cached(&format!(
                "select coalesce(name, ''), group_name, coalesce(size, 0), julianday(posted_date), poster
                 from s{s}.releases where id = ?1 and not atlas_blocked(group_name)"
            ))?
            .query_row([c.id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    Named {
                        id: c.id,
                        group: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        bytes: r.get(2)?,
                        date: r.get(3)?,
                        poster: r.get(4)?,
                    },
                ))
            })
            .optional()?;
        if let Some((name, n)) = row
            && named(&name)
        {
            out.push((s, name, n));
        }
    }
    out.sort_by(|a, b| (a.0, a.2.group.as_bytes()).cmp(&(b.0, b.2.group.as_bytes())));
    Ok(out)
}

/// Each shard's first `per_shard` rows, as a shard's `limit` gives them.
fn first_per_shard(rows: Vec<(usize, String, Named)>, per_shard: usize) -> Vec<Named> {
    let mut taken: HashMap<usize, usize> = HashMap::new();
    rows.into_iter()
        .filter(|(s, _, _)| {
            let n = taken.entry(*s).or_insert(0);
            *n += 1;
            *n <= per_shard
        })
        .map(|(_, _, n)| n)
        .collect()
}

/// A post's rows of its name from the catalog: the poster's own and everyone's, as the shards' two lookups give them.
/// None (the shards answer) when catalog reads are off or can't answer, or when the catalog doesn't have the release
/// asked about yet: one a few seconds behind could otherwise hand over an older post of the same name in its place.
fn catalog_named(
    conn: &Connection,
    posted: &str,
    video: Option<&str>,
    release: &crate::search::ReleaseRow,
) -> Result<Option<(Vec<Named>, Vec<Named>)>> {
    let Some(rows) = crate::shared_catalog::reader::named(posted, None) else { return Ok(None) };
    if !rows.iter().any(|r| r.id == release.id) {
        return Ok(None);
    }
    let mut all = reread_candidates(conn, &rows, |name| name == posted)?;
    if let Some(video) = video {
        let Some(rows) = crate::shared_catalog::reader::named(video, None) else { return Ok(None) };
        all.extend(reread_candidates(conn, &rows, |name| name == video)?);
        all.sort_by(|a, b| (a.0, a.2.group.as_bytes()).cmp(&(b.0, b.2.group.as_bytes())));
    }
    let same: Vec<(usize, String, Named)> = all
        .iter()
        .filter(|(_, _, n)| n.poster == release.poster)
        .map(|(s, name, n)| {
            (
                *s,
                name.clone(),
                Named { id: n.id, group: n.group.clone(), bytes: n.bytes, date: n.date, poster: n.poster.clone() },
            )
        })
        .collect();
    Ok(Some((first_per_shard(same, 64), first_per_shard(all, 64))))
}

fn exact_split_post(
    conn: &Connection,
    posted: &str,
    video: Option<&str>,
    release: &crate::search::ReleaseRow,
    catalog: bool,
) -> Result<Option<SplitPost>> {
    let named_rows = |sql: fn(usize) -> String, poster: Option<&Option<String>>| -> Result<Vec<Named>> {
        let mut out = Vec::new();
        for (s, posted) in (0..store::SHARDS).flat_map(|s| std::iter::once(posted).chain(video).map(move |n| (s, n))) {
            let mut stmt = conn.prepare_cached(&sql(s))?;
            let read = |r: &rusqlite::Row<'_>| {
                Ok(Named {
                    id: r.get(0)?,
                    group: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    bytes: r.get(2)?,
                    date: r.get(3)?,
                    poster: r.get(4)?,
                })
            };
            let rows: Vec<Named> = match poster {
                Some(p) => stmt.query_map(rusqlite::params![posted, p], read)?.collect::<Result<_>>()?,
                None => stmt.query_map(rusqlite::params![posted], read)?.collect::<Result<_>>()?,
            };
            out.extend(rows);
        }
        Ok(out)
    };
    let from_catalog = if catalog { catalog_named(conn, posted, video, release)? } else { None };
    if catalog {
        let source = if from_catalog.is_some() { "catalog" } else { "shards" };
        crate::metrics::names::SHARED_CATALOG_READS.inc(&[("lookup", "split_post"), ("source", source)], 1);
    }
    let (same, named) = match from_catalog {
        Some(both) => both,
        // the poster's own rows by a lookup of their own: other posters' rows mustn't use up its limit
        None => (named_rows(split_post_sql, Some(&release.poster))?, named_rows(named_rows_sql, None)?),
    };
    let same: Vec<&Named> = same.iter().collect();
    let (across, shared, claimed) = across_posters(conn, release.id, &named)?;
    // rows of the name that share parts are grouped by `across_posters` alone, whichever row asks: a poster's own
    // rows could otherwise make a post of their own beside the one they are part of
    if shared {
        return Ok(across.map(|(post, _)| post));
    }
    let by_poster = same_poster_post(conn, &same)?;
    // one poster can have several of the groups: the post across posters is taken only if it holds all of those
    match (by_poster, across) {
        // a row of the poster's post may be in the other as a crosspost of it
        (Some(one), Some((across, covers)))
            if one.ids.iter().all(|id| covers.contains(id)) && covers.len() > one.ids.len() =>
        {
            Ok(Some(across))
        }
        // a poster's own rows can't be a post where one of them is in another, whole post: every row gets its answer
        (Some(one), across) if one.ids.iter().any(|id| claimed.contains(id)) => Ok(across.map(|(post, _)| post)),
        (Some(one), _) => Ok(Some(one)),
        (None, across) => Ok(across.map(|(post, _)| post)),
    }
}

fn same_poster_post(conn: &Connection, rows: &[&Named]) -> Result<Option<SplitPost>> {
    let dates: Vec<Option<f64>> = rows.iter().map(|n| n.date).collect();
    if !(2..=SPLIT_POST_GROUPS).contains(&rows.len()) || !close_together(&dates) {
        return Ok(None);
    }
    let mut ids: Vec<i64> = rows.iter().map(|n| n.id).collect();
    let mut groups: Vec<String> = rows.iter().map(|n| n.group.clone()).collect();
    let size = rows.iter().map(|n| n.bytes).sum();
    ids.sort_unstable();
    groups.sort_unstable();
    groups.dedup();
    let files = union_files(conn, &ids)?;
    // rows sharing parts join only as `across_posters` grows a post, row by row, each adding what the rest lack
    if !files.one_upload() || files.overlap {
        return Ok(None);
    }
    Ok(Some(SplitPost { ids, groups, size, complete: files.complete(), lead: None, recovery: Vec::new() }))
}

/// The most rows of one name looked at for a post whose groups each got a poster of their own.
const ACROSS_POSTERS_ROWS: usize = 32;

/// `across_posters`'s answer: the post the row asking is in, with the rows it stands for; whether any rows of the
/// name share parts; and every row in a whole post of two rows or more, whichever.
type AcrossPosters = (Option<(SplitPost, Vec<i64>)>, bool, HashSet<i64>);

/// One post whose groups each got another made-up poster (Nyuu and ngPost can give every connection its own From): the
/// rows share the post's files, and each holds parts of them no other row has. A name and a subject are only headers,
/// soo rows join only where they fill each other's gaps, file by file, under the same subject, and a row that is a
/// crosspost of another (the same parts of the same files) is left out of the post, its group kept. Rows are taken
/// lowest id first whichever row asks, soo every row of the post gets the same answer.
fn across_posters(conn: &Connection, anchor: i64, named: &[Named]) -> Result<AcrossPosters> {
    // every row of the name, whichever asks: a dated one, lowest ids first
    let mut rows: Vec<&Named> = named.iter().filter(|n| n.date.is_some()).collect();
    if rows.len() < 2 || !rows.iter().any(|n| n.id == anchor) {
        return Ok((None, false, HashSet::new()));
    }
    let one_poster = rows.iter().all(|n| n.poster == rows[0].poster);
    // one poster's rows are `same_poster_post`'s unless some share parts, which one read of them all tells
    if one_poster {
        let ids: Vec<i64> = rows.iter().map(|n| n.id).collect();
        if !union_files(conn, &ids)?.overlap {
            return Ok((None, false, HashSet::new()));
        }
    }
    rows.sort_by_key(|n| n.id);
    rows.truncate(ACROSS_POSTERS_ROWS);
    // each row alone, to tell a crosspost from a part
    let mut kept: Vec<(&Named, UnionFiles)> = Vec::new();
    let mut copies: Vec<(&Named, i64)> = Vec::new();
    for n in rows {
        let files = union_files(conn, &[n.id])?;
        if files.files.is_empty() {
            continue;
        }
        // a crosspost is posted with what it copies: an older upload of the same parts is a post of its own
        let near = |of: &Named| matches!((of.date, n.date), (Some(a), Some(b)) if (a - b).abs() <= SPLIT_POST_DAYS);
        match kept.iter().find(|(of, k)| near(of) && k.same_parts(&files)) {
            Some((of, _)) => copies.push((n, of.id)),
            None => kept.push((n, files)),
        }
    }
    // the rows fall into posts, each grown from its lowest row left: the same posts whoever asks. First of rows that
    // share no part; then, of the rows in none of those that's whole, posts whose rows may share parts (`fills_gaps`):
    // a file uploaded again within the hour, each upload spread over the groups, whole only as both
    let mut whole: Vec<(Vec<i64>, UnionFiles)> = Vec::new();
    let mut taken: HashSet<i64> = HashSet::new();
    for seed in 0..kept.len() {
        if taken.contains(&kept[seed].0.id) {
            continue;
        }
        // only a whole post takes its rows: one that isn't leaves them to the seeds after it
        let (chosen, union) = grow_post(conn, &kept, seed, &taken, false)?;
        if chosen.len() > 1 && union.one_upload() && union.complete() {
            taken.extend(chosen.iter().copied());
            whole.push((chosen, union));
        }
    }
    let mut taken: HashSet<i64> = whole.iter().flat_map(|(chosen, _)| chosen.iter().copied()).collect();
    let mut posts = whole;
    let mut shared = false;
    for seed in 0..kept.len() {
        if taken.contains(&kept[seed].0.id) {
            continue;
        }
        let (chosen, union) = grow_post(conn, &kept, seed, &taken, true)?;
        taken.extend(chosen.iter().copied());
        shared |= union.overlap;
        posts.push((chosen, union));
    }
    // one poster's rows are `same_poster_post`'s, unless they share parts
    if one_poster && !shared {
        return Ok((None, false, HashSet::new()));
    }
    // the rows of the whole posts: a poster's own rows that one of them is in make no post of their own
    let claimed: HashSet<i64> = posts
        .iter()
        .filter(|(chosen, union)| chosen.len() > 1 && union.one_upload() && union.complete())
        .flat_map(|(chosen, _)| chosen.iter().copied())
        .collect();
    for (chosen, union) in posts {
        let anchor_in = chosen.contains(&anchor) || copies.iter().any(|(n, of)| n.id == anchor && chosen.contains(of));
        if !anchor_in {
            continue;
        }
        if chosen.len() < 2 || !union.one_upload() {
            return Ok((None, shared, claimed));
        }
        return Ok((
            post_of(kept.iter().map(|(n, _)| *n).filter(|n| chosen.contains(&n.id)).collect(), &copies, union),
            shared,
            claimed,
        ));
    }
    Ok((None, shared, claimed))
}

/// The rows not `taken` that join `kept[seed]`, looked at again whenever one joins (a row can share a file only with
/// one that comes later): each fills the others' gaps, and they stay within `SPLIT_POST_DAYS` of each other.
fn grow_post(
    conn: &Connection,
    kept: &[(&Named, UnionFiles)],
    seed: usize,
    taken: &HashSet<i64>,
    shared_parts: bool,
) -> Result<(Vec<i64>, UnionFiles)> {
    let mut chosen = vec![kept[seed].0.id];
    let mut union = union_files(conn, &chosen)?;
    let mut span = kept[seed].0.date.map_or((0.0, 0.0), |d| (d, d));
    // a row whole on its own is an upload of its own: no row sharing parts with the post joins one that has it
    let mut whole = kept[seed].1.complete();
    let mut grew = true;
    while grew {
        grew = false;
        for (n, files) in kept {
            let Some(date) = n.date else { continue };
            if chosen.contains(&n.id) || taken.contains(&n.id) {
                continue;
            }
            let (lo, hi) = (span.0.min(date), span.1.max(date));
            // rows sharing parts only where the caller allows them
            if !shared_parts && union.shares_parts(files) {
                continue;
            }
            let shares = files.files.keys().any(|f| union.files.contains_key(f));
            if hi - lo > SPLIT_POST_DAYS || !shares || !union.fills_gaps(files, whole) {
                continue;
            }
            chosen.push(n.id);
            let grown = union_files(conn, &chosen)?;
            // a row that would make the rest no one upload (a counter-less post of other files) stays out
            if !grown.one_upload() {
                chosen.pop();
                continue;
            }
            union = grown;
            span = (lo, hi);
            whole |= files.complete();
            grew = true;
        }
    }
    Ok((chosen, union))
}

/// A post of these rows, with what it stands for: its rows and their crossposts.
fn post_of(rows: Vec<&Named>, copies: &[(&Named, i64)], union: UnionFiles) -> Option<(SplitPost, Vec<i64>)> {
    let mut ids: Vec<i64> = rows.iter().map(|n| n.id).collect();
    let mut groups: Vec<String> = rows.iter().map(|n| n.group.clone()).collect();
    groups.extend(copies.iter().filter(|(_, of)| ids.contains(of)).map(|(n, _)| n.group.clone()));
    groups.sort_unstable();
    groups.dedup();
    if groups.len() > SPLIT_POST_GROUPS {
        return None;
    }
    let summed: i64 = rows.iter().map(|n| n.bytes).sum();
    // rows sharing parts hold some bytes more than once: the files' sizes from their subjects, each by the share of
    // its parts there, or else the rows' bytes scaled by the parts there are to the parts they hold
    let size = match (union.overlap, union.subject_bytes(), union.parts_held) {
        (false, ..) => summed,
        (true, Some(bytes), _) => bytes,
        (true, None, held) if held > 0 => {
            (i128::from(summed) * i128::from(union.distinct_parts()) / i128::from(held)) as i64
        }
        (true, None, _) => summed,
    };
    ids.sort_unstable();
    let mut covers = ids.clone();
    covers.extend(copies.iter().filter(|(_, of)| ids.contains(of)).map(|(n, _)| n.id));
    Some((SplitPost { ids, groups, size, complete: union.complete(), lead: None, recovery: Vec::new() }, covers))
}

#[derive(Clone)]
struct StemCandidate {
    id: i64,
    group: String,
    bytes: i64,
    date: Option<f64>,
}

fn split_stem_post(
    conn: &Connection,
    posted: &str,
    release: &crate::search::ReleaseRow,
    catalog: bool,
) -> Result<Option<SplitPost>> {
    let stem = post_stem(posted);
    if stem.len() < 4 {
        return Ok(None);
    }
    let dot_lo = format!("{stem}.");
    let dot_hi = format!("{stem}/");
    let mut all = Vec::new();
    // the name's own row in the stem's range (byte order, as the shards compare): otherwise no row of the catalog's
    // answer could be the one asked about
    let in_range = |name: &str| name == stem || (name >= dot_lo.as_str() && name < dot_hi.as_str());
    if catalog && in_range(&release.name) {
        // as for a name: the catalog's rows only if they have the release asked about, each read again from its shard
        let rows =
            crate::shared_catalog::reader::stem_rows(&stem).filter(|rows| rows.iter().any(|r| r.id == release.id));
        let source = if rows.is_some() { "catalog" } else { "shards" };
        crate::metrics::names::SHARED_CATALOG_READS.inc(&[("lookup", "split_stem"), ("source", source)], 1);
        if let Some(rows) = rows {
            // a stem one shard holds more than LINKED_ROWS rows of is no post, as the shards' lookup stops there
            let mut per_shard: HashMap<usize, usize> = HashMap::new();
            for (s, name, n) in reread_candidates(conn, &rows, in_range)? {
                let count = per_shard.entry(s).or_insert(0);
                *count += 1;
                if *count > LINKED_ROWS {
                    return Ok(None);
                }
                if post_stem(&name).eq_ignore_ascii_case(&stem) {
                    all.push(StemCandidate { id: n.id, group: n.group, bytes: n.bytes, date: n.date });
                    if all.len() > LINKED_ROWS {
                        return Ok(None);
                    }
                }
            }
            return stem_post_of(conn, all, release, &stem);
        }
    }
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
    stem_post_of(conn, all, release, &stem)
}

/// The post a stem's candidates make with this release, if any.
fn stem_post_of(
    conn: &Connection,
    all: Vec<StemCandidate>,
    release: &crate::search::ReleaseRow,
    stem: &str,
) -> Result<Option<SplitPost>> {
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
    try_stem_subset(conn, stem, &linked)
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
        "select id, group_name, coalesce(size, 0), julianday(posted_date), poster from s{s}.releases
         where name = ?1 and +poster is ?2 and not atlas_blocked(group_name) limit 64"
    )
}

/// A shard's rows of one name, whoever posted them, by the name index. Parameters: ?1 the posted name.
fn named_rows_sql(s: usize) -> String {
    format!(
        "select id, group_name, coalesce(size, 0), julianday(posted_date), poster from s{s}.releases
         where name = ?1 and not atlas_blocked(group_name) limit 64"
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

/// Whether a file's subject (less its part number) ends with a byte count, as Nyuu and ngPost write the file's size:
/// `"Film.mkv" yEnc (/21040) 88246893816`. Two rows agreeing on such a subject agree on the file's name, article count
/// and size. Message-ids aren't compared: two uploads of one file this alike hold parts that stand in for each other.
fn subject_has_size(subject: &str) -> bool {
    subject_size(subject).is_some()
}

/// The byte count a subject ends with, after its last `)` (`subject_has_size`).
fn subject_size(subject: &str) -> Option<i64> {
    let tail = subject[subject.rfind(')')? + 1..].trim();
    if tail.is_empty() || !tail.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    tail.parse().ok()
}

/// Each file across these releases, by name, and what says whether they're one upload.
#[derive(Default)]
struct UnionFiles {
    files: BTreeMap<String, UnionFile>,
    /// what each file says the post has
    file_totals: BTreeSet<Option<i64>>,
    /// each release's filenames
    rows: Vec<BTreeSet<String>>,
    /// two releases have a file they don't agree on, or a part of it both have under a subject without the file's size
    conflict: bool,
    /// two releases have a part of a file both agree on, its subject carrying the file's size: a post whose poster
    /// sent each article to some of its groups, soo a part can be in more than one row
    overlap: bool,
    /// the parts every row holds, added up: more than `distinct_parts` where rows share parts
    parts_held: i64,
    /// whether any file carries a 0-indexed `[0/N]` or `[00/N]` counter
    zero_indexed: bool,
    /// each release's name
    names: BTreeSet<String>,
}

impl UnionFiles {
    /// Whether the releases can be parts of one upload. They say the same of every file they share, each shares one
    /// with the rest, and no part is in two of them unless its subject names the file by its size (`overlap`: the poster
    /// sent the article to more than one of the groups).
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
        if self.conflict || self.overlap || self.files.is_empty() || self.file_totals.contains(&None) {
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

    /// Whether `other` holds the very parts this does of the very same files: a crosspost, not a part of the post.
    fn same_parts(&self, other: &UnionFiles) -> bool {
        self.files.len() == other.files.len()
            && self.files.iter().all(|(name, f)| {
                other.files.get(name).is_some_and(|o| {
                    o.expected == f.expected && o.subject == f.subject && trimmed(&o.seen) == trimmed(&f.seen)
                })
            })
    }

    /// Whether `other` only adds what this lacks: every file both have is the same file under the same subject, and
    /// they say the same of how many files the post has. No part is in both, except under a subject that ends with a
    /// byte count (the same name, part count and size: `subject_has_size`), where neither `other` nor a row already
    /// in this (`whole`) is the whole post on its own, and where `other` still adds a part this lacks: a poster can
    /// send each article to some of the post's groups, soo its rows share parts, but a whole upload is its own post.
    fn fills_gaps(&self, other: &UnionFiles, whole: bool) -> bool {
        if other.conflict || self.conflict {
            return false;
        }
        let mut totals = self.file_totals.union(&other.file_totals);
        if totals.next().is_some() && totals.next().is_some() {
            return false;
        }
        let mut shared = false;
        let mut adds = false;
        let agrees = other.files.iter().all(|(name, o)| match self.files.get(name) {
            None => {
                adds = true;
                true
            }
            Some(f) => {
                let same =
                    f.expected.is_some() && f.expected == o.expected && f.subject.is_some() && f.subject == o.subject;
                let both = f.seen.iter().zip(&o.seen).any(|(a, b)| a & b != 0);
                adds |= adds_parts(&f.seen, &o.seen, o.expected);
                if both {
                    shared = true;
                    return same && f.subject.as_deref().is_some_and(subject_has_size);
                }
                same
            }
        });
        agrees && (!shared || (adds && !whole && !other.complete()))
    }

    /// The bytes of the parts there are, by each file's size in its subject (`subject_size`) times the share of its
    /// expected parts there. None when a file's subject doesn't give its size.
    fn subject_bytes(&self) -> Option<i64> {
        self.files.values().try_fold(0i64, |sum, f| {
            let size = subject_size(f.subject.as_deref()?)?;
            let expected = f.expected.filter(|e| *e > 0)?;
            let have = store::seen_count(&f.seen).min(expected);
            Some(sum + (i128::from(size) * i128::from(have) / i128::from(expected)) as i64)
        })
    }

    /// Whether `other` has a part of a file this has too.
    fn shares_parts(&self, other: &UnionFiles) -> bool {
        self.files
            .iter()
            .any(|(name, f)| other.files.get(name).is_some_and(|o| f.seen.iter().zip(&o.seen).any(|(a, b)| a & b != 0)))
    }

    /// The parts there are, each once however many rows hold it.
    fn distinct_parts(&self) -> i64 {
        self.files.values().map(|f| store::seen_count(&f.seen)).sum()
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

    /// Whether every file the post says it has is there with exactly its parts, or every file there is whole and they
    /// are one video named as the post beside side files (`store::one_video_named`): the counter can count a
    /// `thumbnail.jpg` that came as a row of its own.
    fn complete(&self) -> bool {
        if self.file_totals.len() > 1 {
            return false;
        }
        let total = self.file_totals.iter().copied().flatten().next_back();
        !self.files.is_empty()
            && self.files.values().all(|f| f.expected.is_some_and(|want| store::is_exactly(&f.seen, want)))
            && (total.is_none_or(|total| self.files.len() as i64 == total + i64::from(self.zero_indexed))
                || store::one_video_named(self.names.iter().map(String::as_str), self.files.keys().map(String::as_str)))
    }
}

/// Whether `theirs` has a part `ours` lacks, among the file's `expected` parts (a stray bit past them adds nothing).
fn adds_parts(ours: &[u8], theirs: &[u8], expected: Option<i64>) -> bool {
    theirs.iter().enumerate().any(|(at, b)| {
        let new = b & !ours.get(at).copied().unwrap_or(0);
        (0..8).any(|bit| new >> bit & 1 == 1 && expected.is_none_or(|e| (at * 8 + bit) as i64 <= e))
    })
}

/// A `seen` bitmap without its trailing empty bytes, soo two of different lengths compare by their bits.
fn trimmed(bits: &[u8]) -> &[u8] {
    let end = bits.iter().rposition(|b| *b != 0).map_or(0, |at| at + 1);
    &bits[..end]
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

/// How much of what these rows' files say they have is there, out of 1000, across all of them.
pub fn permille_of(conn: &Connection, ids: &[i64]) -> Result<i64> {
    let union = union_files(conn, ids)?;
    let (mut have, mut want) = (0i64, 0i64);
    for f in union.files.values() {
        let Some(expected) = f.expected.filter(|e| *e > 0) else { continue };
        have += store::seen_count(&f.seen).min(expected);
        want += expected;
    }
    Ok(if want == 0 { 0 } else { have * 1000 / want })
}

/// Whether these rows hold the whole post: every file its counter says it has, each with every part (unlike
/// `permille_of`, which counts the parts of the files that are there).
pub fn whole_of(conn: &Connection, ids: &[i64]) -> Result<bool> {
    Ok(union_files(conn, ids)?.complete())
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
        let name: Option<String> = conn
            .prepare_cached(&format!("select name from s{s}.releases where id = ?"))?
            .query_row([id], |r| r.get(0))
            .optional()?
            .flatten();
        union.names.extend(name);
        let mut filenames = BTreeSet::new();
        for row in rows {
            let (filename, expected, seen, total, subject) = row?;
            union.file_totals.insert(total);
            union.parts_held += store::seen_count(&seen);
            union.zero_indexed |= subject.as_deref().is_some_and(crate::parser::has_zero_file_index);
            filenames.insert(filename.clone());
            let subject = subject.as_deref().map(|subject| subject_of_file(subject, expected));
            match union.files.entry(filename) {
                Entry::Vacant(file) => {
                    file.insert(UnionFile { expected, seen, subject });
                }
                Entry::Occupied(mut file) => {
                    let file = file.get_mut();
                    let disagree =
                        expected.is_none() || file.expected != expected || subject.is_none() || file.subject != subject;
                    union.conflict |= disagree;
                    // the same part of one file in two rows is the same article only where the subject names the
                    // file by its size as well: without it, two files of a name can't be told apart
                    let sized = !disagree && subject.as_deref().is_some_and(subject_has_size);
                    if file.seen.len() < seen.len() {
                        file.seen.resize(seen.len(), 0);
                    }
                    for (slot, bits) in file.seen.iter_mut().zip(seen) {
                        if *slot & bits != 0 {
                            if sized {
                                union.overlap = true;
                            } else {
                                union.conflict = true;
                            }
                        }
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
    contents_of(conn, &post.ids, &post.recovery, Bytes::All)
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
    Regex::new(r"(?i)\{\{([^{}\s]{1,128})\}\}|\b(?:password|passwurd|passwd|passwort|passwoord|pasword|pswd|pass|pw|pwd|mot\s+de\s+passe|mdp|wachtwoord|kennwort)\b[\s._-]*(?:is\s*)?[:=]\s*([^\s\[\]\x22']{1,128})").unwrap()
});

/// Whether a matched token is a known non-password placeholder, negative word, or MediaInfo artifact.
pub fn is_false_positive_password(found: &str) -> bool {
    let f = found.trim();
    if f.is_empty() || f.len() > 128 {
        return true;
    }
    let none = [
        "none",
        "no",
        "n/a",
        "na",
        "nil",
        "null",
        "false",
        "-",
        "free",
        "not",
        "yes",
        "keins",
        "keine",
        "kein",
        "aucun",
        "geen",
        "datum",
        "available",
        "needed",
        "required",
        "protected",
        "encrypted",
        "unknown",
        "see",
        "check",
        "inside",
        "http://",
        "https://",
    ];
    if none.iter().any(|word| f.eq_ignore_ascii_case(word)) {
        return true;
    }
    if crate::archive::PasswordHarvester::is_negative_phrase(f) {
        return true;
    }
    !f.is_empty()
        && f.bytes().all(|b| matches!(b, b'-' | b'=' | b'*' | b'_' | b'.' | b':' | b'/' | b'\\' | b'|' | b'+' | b'~'))
}

/// All candidate passwords matched by [`PASSWORD`] in `text`, in order of appearance.
pub fn find_all_passwords(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for hit in PASSWORD.captures_iter(text) {
        let found = if let Some(braced) = hit.get(1) {
            let s = braced.as_str().trim();
            if matches!(s.to_ascii_lowercase().as_str(), "password" | "pass" | "pw" | "pwd") {
                continue;
            }
            s
        } else if let Some(bare) = hit.get(2) {
            let whole = hit.get(0).map_or("", |m| m.as_str()).to_ascii_lowercase();
            if matches!(whole.as_str(), "pass=1" | "pass=2" | "pass=3") {
                continue;
            }
            bare.as_str().trim_matches(['.', ','])
        } else {
            continue;
        };
        if !is_false_positive_password(found) && !out.iter().any(|p| p == found) {
            out.push(found.to_string());
        }
    }
    out
}

/// A password a poster left in a name, subject or NFO: `{{secret}}` or `password: secret`.
/// A guess from text, soo nothing has tried it.
pub fn find_password(text: &str) -> Option<String> {
    find_all_passwords(text).into_iter().next()
}

/// Finds a password in `texts` and `extra_candidates`, verifying candidates against `check` when available.
/// When `check` is present and `check.certain()` is true, an unverified regex match that `check.accepts`
/// rejects is discarded, and broader tokens/lines/URLs from `texts` are tested against `check`.
pub fn find_verified_password<'a>(
    texts: impl IntoIterator<Item = &'a str>,
    extra_candidates: &[String],
    check: Option<&crate::archive::PasswordTest>,
) -> Option<String> {
    let texts: Vec<&str> = texts.into_iter().collect();
    let empty_meta = std::collections::HashMap::new();
    let mut priority = Vec::new();
    let push_uniq = |vec: &mut Vec<String>, s: &str| {
        let t = s.trim();
        if !t.is_empty() && t.len() <= 128 && !is_false_positive_password(t) && !vec.iter().any(|x| x == t) {
            vec.push(t.to_string());
        }
    };
    for c in extra_candidates {
        push_uniq(&mut priority, c);
    }
    for &t in &texts {
        for c in find_all_passwords(t) {
            push_uniq(&mut priority, &c);
        }
        for c in crate::archive::PasswordHarvester::extract_candidates(t, Some(t), &empty_meta) {
            push_uniq(&mut priority, &c);
        }
    }
    let Some(chk) = check else {
        return priority.into_iter().next();
    };
    let mut tested = 0usize;
    const MAX_CHECK_CANDIDATES: usize = 24;
    let mut fallback: Option<String> = None;
    for cand in &priority {
        if tested >= MAX_CHECK_CANDIDATES {
            break;
        }
        tested += 1;
        match chk.accepts(cand) {
            Some(true) => return Some(cand.clone()),
            None => {
                if fallback.is_none() {
                    fallback = Some(cand.clone());
                }
            }
            Some(false) => {}
        }
    }
    if chk.accepts("").is_none() {
        return fallback.or_else(|| priority.into_iter().next());
    }
    // Broader NFO/subject token scan when explicit regexes didn't yield the password:
    // test short single-token lines, quoted tokens, values after `:`, and URLs/domains.
    let mut broad = Vec::new();
    static RE_URL: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"(?i)\b(?:https?://|www\.)[^\s\x22'<>]{3,96}|\b[a-z0-9][-a-z0-9]{1,48}\.(?:com|net|org|to|cc|info|biz|ws|de|nl|ru|me|tv|io|xyz)\b").unwrap()
    });
    for &t in &texts {
        for m in RE_URL.find_iter(t) {
            let u = m.as_str().trim_end_matches(['.', ',', ')', ']']);
            push_uniq(&mut broad, u);
            if let Some(stripped) = u.strip_prefix("https://").or_else(|| u.strip_prefix("http://")) {
                push_uniq(&mut broad, stripped.trim_end_matches('/'));
                if let Some(no_www) = stripped.strip_prefix("www.") {
                    push_uniq(&mut broad, no_www.trim_end_matches('/'));
                }
            } else if let Some(no_www) = u.strip_prefix("www.") {
                push_uniq(&mut broad, no_www.trim_end_matches('/'));
            }
        }
        for raw_line in t.lines() {
            let line = raw_line
                .trim_matches(|c: char| {
                    c.is_whitespace()
                        || ('\u{2500}'..='\u{259f}').contains(&c)
                        || matches!(c, '*' | '#' | '=' | '-' | '|' | ':' | '~')
                })
                .trim();
            if line.is_empty() {
                continue;
            }
            if line.len() <= 80 && !line.contains(char::is_whitespace) && (3..=64).contains(&line.len()) {
                push_uniq(&mut broad, line);
            }
            for (idx, _) in line.match_indices([':', '=']) {
                let rhs = line[idx + 1..].trim();
                let val = rhs.trim_matches(['"', '\'', '.', ',']);
                if !val.contains(char::is_whitespace) && (3..=64).contains(&val.len()) {
                    push_uniq(&mut broad, val);
                } else if let Some(first) = val.split_whitespace().next() {
                    let tok = first.trim_matches(['"', '\'', '.', ',', ';', ')', ']']);
                    if (3..=64).contains(&tok.len()) {
                        push_uniq(&mut broad, tok);
                    }
                }
            }
        }
    }
    for cand in broad {
        if priority.iter().any(|p| p == &cand) {
            continue;
        }
        if tested >= MAX_CHECK_CANDIDATES {
            break;
        }
        tested += 1;
        if chk.accepts(&cand) == Some(true) {
            return Some(cand);
        }
    }
    fallback
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
    // every step keeps what it found in the one table, wherever it found it: the page says where it is to be seen
    if let Some(password) = crate::archive::store::nfo_password(conn, id) {
        let source = stored_password_source(conn, id, &password, names);
        return hint(password, source);
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
    let inner = crate::archive::store::view_with(conn, id);
    // a name inside: an archive holding `Name-{{secret}}.nzb`, the password for what that NZB points to
    for row in &inner.files {
        if let Some(password) = find_password(&row.name) {
            return hint(password, "a file inside");
        }
    }
    // the harvester's other spellings: `{{pass}}`, `[pw: x]`, `pass=x`, in the names and every subject
    let texts: Vec<String> = names
        .into_iter()
        .flat_map(|n| [Some(n.posted.clone()), n.display.clone()])
        .flatten()
        .chain(files.into_iter().filter_map(|(_, subject)| subject))
        .collect();
    for text in &texts {
        if let Some(password) = harvested_password(text) {
            return hint(password, "the subject");
        }
    }
    if let Some(check) = inner.password_check.as_ref() {
        let nfo = crate::probe::nfo_store::any_text(conn, id);
        let all_texts = texts.iter().map(String::as_str).chain(nfo.as_deref());
        if let Some(password) = find_verified_password(all_texts, &[], Some(check)) {
            let source = stored_password_source(conn, id, &password, names);
            return hint(password, source);
        }
    }
    None
}

/// Where a password kept in `release_nfo_password` is written, for the page: the steps that find one by trying the
/// words of a name, a PAR2 set or an NFO against the archive's check keep it with nothing of where it came from.
fn stored_password_source(conn: &Connection, id: i64, password: &str, names: Option<&Names>) -> &'static str {
    const ELSEWHERE: &str = "another copy of this post, or a posted NZB";
    if password.is_empty() {
        return ELSEWHERE;
    }
    let s = store::shard_of_id(id);
    let in_it = |text: &str| text.contains(password);
    let read_names = || -> Option<(String, Option<String>)> {
        conn.query_row(&format!("select name, display_name from s{s}.releases where id = ?"), [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .ok()
    };
    let (posted, display) = match names {
        Some(n) => (Some(n.posted.clone()), n.display.clone()),
        None => read_names().map_or((None, None), |(p, d)| (Some(p), d)),
    };
    if posted.as_deref().is_some_and(in_it) {
        return if display.is_some() { "the original article name" } else { "the release name" };
    }
    if display.as_deref().is_some_and(in_it) {
        return "the release name";
    }
    let column = |sql: &str| -> Vec<Option<String>> {
        conn.prepare(&sql.replace("{s}", &s.to_string()))
            .and_then(|mut stmt| stmt.query_map([id], |r| r.get(0))?.collect())
            .unwrap_or_default()
    };
    let filenames = column("select filename from s{s}.files where release_id = ? order by id limit 256");
    if filenames.iter().flatten().any(|f| in_it(f)) {
        return "a file name";
    }
    let subjects = column("select subject from s{s}.files where release_id = ? order by id limit 256");
    if subjects.iter().flatten().any(|f| in_it(f)) {
        return "the article subject";
    }
    let par2 = column("select name from s{s}.par2_files where release_id = ? limit 256");
    if par2.iter().flatten().any(|f| in_it(f)) {
        return "a file name in the PAR2 set";
    }
    if crate::archive::store::view_with(conn, id).files.iter().any(|f| in_it(&f.name)) {
        return "a file inside";
    }
    if crate::probe::nfo_store::any_text(conn, id).is_some_and(|text| in_it(&text)) {
        return "the NFO";
    }
    ELSEWHERE
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
    fn catalog_candidates_are_read_again_from_their_shards_as_the_shards_give_them() {
        use crate::shared_catalog::reader::NamedRow;
        let conn = Connection::open_in_memory().unwrap();
        crate::filters::register_sql(&conn).unwrap();
        for s in 0..store::SHARDS {
            conn.execute_batch(&format!(
                "attach ':memory:' as s{s};
                 create table s{s}.releases (id INTEGER PRIMARY KEY, name TEXT, group_name TEXT, size INTEGER,
                    posted_date TEXT, poster TEXT);"
            ))
            .unwrap();
        }
        // ids carry their shard: id = seq * 8 + shard
        let id = |seq: i64, shard: i64| seq * 8 + shard;
        for i in 0..70 {
            conn.execute(
                "insert into s1.releases values (?1, 'Post', ?2, 10, '2026-11-07 12:00:00', 'p@x')",
                rusqlite::params![id(100 + (69 - i), 1), format!("a.b.g{i:03}")],
            )
            .unwrap();
        }
        conn.execute_batch(&format!(
            "insert into s2.releases values ({}, 'Post', 'a.b.other', 99, '2026-11-07 13:00:00', 'q@x');
             insert into s2.releases values ({}, 'Post', '{}', 1, null, 'p@x');
             insert into s2.releases values ({}, 'Renamed', 'a.b.moved', 1, null, 'p@x');",
            id(500, 2),
            id(600, 2),
            "x".repeat(1_000),
            id(700, 2)
        ))
        .unwrap();
        let catalog_row = |i: i64| NamedRow {
            id: i,
            shard: store::shard_of_id(i) as i16,
            name: "Post".into(),
            group: "stale".into(),
            bytes: 0,
            posted: None,
            poster: None,
        };
        let mut candidates: Vec<NamedRow> = (100..170).map(|seq| catalog_row(id(seq, 1))).collect();
        // a row the shard deleted, a blocked group, and one renamed since
        candidates.extend([
            catalog_row(id(800, 2)),
            catalog_row(id(500, 2)),
            catalog_row(id(600, 2)),
            catalog_row(id(700, 2)),
        ]);
        let rows = reread_candidates(&conn, &candidates, |name| name == "Post").unwrap();
        let named = first_per_shard(rows, 64);
        let shard1: Vec<&str> =
            named.iter().filter(|n| store::shard_of_id(n.id) == 1).map(|n| n.group.as_str()).collect();
        assert_eq!(shard1.len(), 64, "the first 64 of the shard");
        assert_eq!((shard1[0], shard1[63]), ("a.b.g000", "a.b.g063"), "by group name, as the (name, group) index");
        let other = named.iter().find(|n| n.id == id(500, 2)).unwrap();
        assert_eq!(
            (other.group.as_str(), other.bytes, other.poster.as_deref()),
            ("a.b.other", 99, Some("q@x")),
            "the shard's row, not the catalog's"
        );
        assert!(other.date.is_some_and(|d| d > 2_461_000.0), "julianday from the shard's own sql");
        for gone in [id(800, 2), id(600, 2), id(700, 2)] {
            assert!(!named.iter().any(|n| n.id == gone), "deleted, blocked or renamed: {gone}");
        }
    }

    #[test]
    fn a_password_in_a_name_inside_is_a_hint() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "attach ':memory:' as s0;
             create table s0.files (id INTEGER PRIMARY KEY, release_id INTEGER, filename TEXT, subject TEXT);
             create table s0.archive_inner_files (release_id INTEGER, seq INTEGER, name TEXT, size INTEGER,
                 source INTEGER, primary key (release_id, seq));
             insert into s0.files values (1, 8, 'Some.Post.rar', 'Some.Post.rar (01/26)');
             insert into s0.archive_inner_files values (8, 131072, 'Some.Post-{{s3cret}}.nzb', 1, 2);",
        )
        .unwrap();
        let hint = password_hint_with(&conn, 8, None).unwrap();
        assert_eq!((hint.password.as_str(), hint.source), ("s3cret", "a file inside"));
        assert!(password_hint_with(&conn, 16, None).is_none());
    }

    #[test]
    fn a_kept_password_says_where_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.s0.db");
        store::create_shard(&path).unwrap();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("attach database ?1 as s0", [path.to_string_lossy()]).unwrap();
        conn.execute_batch(
            "insert into s0.releases (id, name, group_name, display_name) values
                 (8, 'YMFFZeoeVI6fG4iz', 'a.b.ftd', 'Gone.with.the.Wind.1939-GRP'),
                 (16, 'Some.Post-GRP', 'a.b.test', null),
                 (24, 'Other.Post-GRP', 'a.b.test', null),
                 (32, 'Third.Post-GRP', 'a.b.test', null),
                 (40, 'Fourth.Post-GRP', 'a.b.test', null),
                 (48, 'q8w7e6r5', 'a.b.test', null);
             insert into s0.files (id, release_id, filename, subject, seen) values
                 (8, 8, 'YMFFZeoeVI6fG4iz.part001.rar', '[1/441] YMFFZeoeVI6fG4iz.part001.rar yEnc (1/400)', x'00'),
                 (16, 16, 'k3m9x.part01.rar', 'k3m9x.part01.rar yEnc (1/50)', x'00'),
                 (24, 24, 'Other.Post.part01.rar', 'Other.Post [pw x7Q2p] yEnc (1/50)', x'00'),
                 (32, 32, 'Third.Post.part01.rar', 'Third.Post.part01.rar yEnc (1/50)', x'00'),
                 (40, 40, 'Fourth.Post.part01.rar', 'Fourth.Post.part01.rar yEnc (1/50)', x'00'),
                 (48, 48, 'a1b2c3.rar', 'a1b2c3.rar yEnc (1/50)', x'00');
             insert into s0.par2_files (release_id, file_id, set_id, md5, md5_16k, size, name)
                 values (48, x'01', x'02', x'03', x'04', 10, 'Renamed.Post-Zq9vK2.rar');
             insert into s0.release_nfo_password (release_id, password) values
                 (8, 'YMFFZeoeVI6fG4iz'), (16, 'k3m9x'), (24, 'x7Q2p'), (32, 'n0tH3re'), (40, 'fr0mNfo'), (48, 'Zq9vK2');
             insert into s0.release_nfo (release_id, file, text) values (40, 'Fourth.nfo', 'Password: fr0mNfo');",
        )
        .unwrap();
        let source = |id| password_hint_with(&conn, id, None).unwrap().source;
        assert_eq!(source(8), "the original article name", "not the NFO: there is none");
        assert_eq!(source(16), "a file name");
        assert_eq!(source(24), "the article subject");
        assert_eq!(source(32), "another copy of this post, or a posted NZB");
        assert_eq!(source(40), "the NFO");
        assert_eq!(source(48), "a file name in the PAR2 set");
        let names = Names { posted: "YMFFZeoeVI6fG4iz".into(), display: None, obfuscated: true, source: None };
        assert_eq!(password_hint_with(&conn, 8, Some(&names)).unwrap().source, "the release name");
    }

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
    fn a_releases_counts_without_its_bytes_are_the_same_counts() {
        let (_dir, conn) = linked_shard();
        for id in [8, 16, 24] {
            let full = contents_with(&conn, id).unwrap();
            let counts = contents_counts_with(&conn, id).unwrap();
            assert_eq!(
                (
                    counts.files,
                    counts.par2_files,
                    counts.recovery_volumes,
                    counts.articles_seen,
                    counts.articles_expected
                ),
                (full.files, full.par2_files, full.recovery_volumes, full.articles_seen, full.articles_expected),
                "release {id}"
            );
            assert_eq!(counts.missing_per_mille(), full.missing_per_mille());
            assert_eq!((counts.data_bytes, counts.recovery_bytes), (0, 0), "no articles read for bytes");
            let summary = contents_summary_with(&conn, id).unwrap();
            assert_eq!(summary.repairable(), full.repairable(), "the same verdict, release {id}");
            assert_eq!(summary.exact_repair_per_mille(), full.exact_repair_per_mille());
        }
    }

    #[test]
    fn a_summary_reads_bytes_only_where_the_repair_verdict_rests_on_them() {
        let half = Contents {
            data_articles_expected: 100,
            data_articles_seen: 90,
            recovery_articles_expected: 20,
            recovery_articles_seen: 20,
            recovery_blocks: 10,
            ..Default::default()
        };
        assert!(verdict_needs_bytes(&half), "data, recovery and no PAR2 set: sizes decide");
        assert!(!verdict_needs_bytes(&Contents { recovery_articles_expected: 0, ..half.clone() }), "no recovery");
        assert!(!verdict_needs_bytes(&Contents { data_articles_expected: 0, ..half.clone() }), "recovery only");
        let with_set = Contents { par2_block: Some(1_000), par2_blocks: Some(100), ..half.clone() };
        assert!(with_set.exact_repair_per_mille().is_some());
        assert!(!verdict_needs_bytes(&with_set), "the set read: blocks decide");
        // and where they do, the verdict changes with them, which is why they're read
        let sized = Contents { data_bytes: 90_000, recovery_bytes: 20_000, ..half.clone() };
        assert!(sized.repairable() && !half.repairable());
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
        assert_eq!(
            find_password(
                r#"Various pictures - Part 19 - RAR Passwurd: variouspics [003/371] "VariousPics-Part19.part001.rar" yEnc (0001/1093)"#
            )
            .as_deref(),
            Some("variouspics")
        );
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

    /// Every shard attached, as `db::open` does.
    fn all_shards() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        crate::filters::register_sql(&conn).unwrap();
        for s in 0..store::SHARDS {
            let path = dir.path().join(format!("atlas.s{s}.db"));
            store::create_shard(&path).unwrap();
            conn.execute(&format!("attach database ?1 as s{s}"), [path.to_string_lossy()]).unwrap();
        }
        (dir, conn)
    }

    /// A row in its group's shard with one file per (name, parts held, of how many).
    fn put_row(conn: &Connection, id: i64, name: &str, group: &str, poster: &str, files: &[(&str, &[i64], i64)]) {
        put_row_at(conn, id, name, group, poster, files, "2026-11-03 07:36:18");
    }

    fn put_row_at(
        conn: &Connection,
        id: i64,
        name: &str,
        group: &str,
        poster: &str,
        files: &[(&str, &[i64], i64)],
        posted: &str,
    ) {
        put_row_counted(conn, id, name, group, poster, files, posted, 2);
    }

    /// `put_row_at` under a counter of `total` files: the film is `[1/total]`, every other file `[2/total]`.
    #[allow(clippy::too_many_arguments)]
    fn put_row_counted(
        conn: &Connection,
        id: i64,
        name: &str,
        group: &str,
        poster: &str,
        files: &[(&str, &[i64], i64)],
        posted: &str,
        total: i64,
    ) {
        put_row_subjects(conn, id, name, group, poster, files, posted, total, false);
    }

    /// `put_row_counted`, each subject ending with its file's size in bytes when `sized`, as yEnc posters write it.
    /// A sized row holds its articles too, each part's message-id the same in every row (one article sent to several
    /// groups).
    #[allow(clippy::too_many_arguments)]
    fn put_row_subjects(
        conn: &Connection,
        id: i64,
        name: &str,
        group: &str,
        poster: &str,
        files: &[(&str, &[i64], i64)],
        posted: &str,
        total: i64,
        sized: bool,
    ) {
        put_row_upload(conn, id, name, group, poster, files, posted, total, sized.then_some(""));
    }

    /// `put_row_subjects` sized, its articles' message-ids those of `upload`: another upload's parts have others.
    #[allow(clippy::too_many_arguments)]
    fn put_row_upload(
        conn: &Connection,
        id: i64,
        name: &str,
        group: &str,
        poster: &str,
        files: &[(&str, &[i64], i64)],
        posted: &str,
        total: i64,
        upload: Option<&str>,
    ) {
        let sized = upload.is_some();
        let s = store::shard_of(group);
        let id = store::global_id(id, s);
        conn.execute(
            &format!(
                "insert into s{s}.releases (id, name, group_name, poster, posted_date, size, complete, parts, file_total)
                 values (?1, ?2, ?3, ?4, ?7, ?5, 0, ?6, ?8)"
            ),
            rusqlite::params![
                id,
                name,
                group,
                poster,
                files.iter().map(|f| f.1.len() as i64 * 100).sum::<i64>(),
                files.iter().map(|f| f.1.len() as i64).sum::<i64>(),
                posted,
                total
            ],
        )
        .unwrap();
        for (at, (file, parts, of)) in files.iter().enumerate() {
            let mut bits = vec![0u8; (*of as usize) / 8 + 1];
            for p in *parts {
                bits[*p as usize / 8] |= 1 << (*p as usize % 8);
            }
            conn.execute(
                &format!(
                    "insert into s{s}.files (id, release_id, filename, subject, expected, file_total, seen)
                     values (?1, ?2, ?3, ?4, ?5, ?7, ?6)"
                ),
                rusqlite::params![
                    id * 16 + at as i64,
                    id,
                    file,
                    // the post's counter: the film first, its volumes second, whichever row has them
                    format!(
                        "[{}/{total}] - \"{file}\" yEnc ({}/{of}){}",
                        if file.ends_with(".mkv") { 1 } else { 2 },
                        parts[0],
                        if sized { format!(" {}", of * 716_800) } else { String::new() }
                    ),
                    of,
                    bits,
                    total
                ],
            )
            .unwrap();
            for p in parts.iter().filter(|_| sized) {
                let mid = format!("<{file}.{p}{}@x>", upload.unwrap_or_default());
                conn.execute(
                    &format!(
                        "insert into s{s}.segments (file_id, local, domain, part, bytes) values (?1, ?2, 0, ?3, 100)"
                    ),
                    rusqlite::params![id * 16 + at as i64, store::pack_local(&mid), p],
                )
                .unwrap();
            }
        }
    }

    fn row_of(conn: &Connection, id: i64, group: &str) -> crate::search::ReleaseRow {
        let id = store::global_id(id, store::shard_of(group));
        let s = store::shard_of(group);
        let (name, poster): (String, String) = conn
            .query_row(&format!("select name, poster from s{s}.releases where id = ?"), [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        crate::search::ReleaseRow { id, name, group_name: group.into(), poster: Some(poster), ..Default::default() }
    }

    #[test]
    fn a_post_with_a_poster_for_each_group_is_one_post() {
        let (_dir, conn) = all_shards();
        let name = "Some.Film.2007.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.Atmos-GRP";
        let (mkv, vol) = ("Some.Film.mkv", "Some.Film.vol00+01.par2");
        let (a, b, c, d) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c", "alt.binaries.d");
        // the film's 8 parts across three groups, each with its own poster, and the group b row crossposted to d
        put_row(&conn, 1, name, a, "x1@nyuu", &[(mkv, &[1, 4, 7], 8), (vol, &[1], 2)]);
        put_row(&conn, 2, name, b, "x2@nyuu", &[(mkv, &[2, 5, 8], 8), (vol, &[2], 2)]);
        put_row(&conn, 3, name, c, "x3@nyuu", &[(mkv, &[3, 6], 8)]);
        put_row(&conn, 4, name, d, "x2@nyuu", &[(mkv, &[2, 5, 8], 8), (vol, &[2], 2)]);
        let ids: Vec<i64> = [(1, a), (2, b), (3, c)].iter().map(|(i, g)| row_of(&conn, *i, g).id).collect();
        let mut want = ids.clone();
        want.sort_unstable();
        for (i, g) in [(1, a), (2, b), (3, c), (4, d)] {
            let post = whole_post_with(&conn, &row_of(&conn, i, g)).unwrap().unwrap_or_else(|| panic!("row {i}"));
            assert_eq!(post.ids, want, "row {i} gets the same answer");
            assert!(post.complete);
            assert_eq!(post.groups, [a, b, c, d]);
        }
    }

    #[test]
    fn the_other_rows_of_a_post_arent_its_twins() {
        let (_dir, conn) = all_shards();
        let name = "Some.Film.2007.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.Atmos-GRP";
        let (mkv, vol) = ("Some.Film.mkv", "Some.Film.vol00+01.par2");
        let (a, b, c, e) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c", "alt.binaries.e");
        // each article of the post went to one of three groups, and a separate upload of it came ten days before
        put_row(&conn, 1, name, a, "x1@nyuu", &[(mkv, &[1, 4, 7], 8), (vol, &[1], 2)]);
        put_row(&conn, 2, name, b, "x2@nyuu", &[(mkv, &[2, 5, 8], 8), (vol, &[2], 2)]);
        put_row(&conn, 3, name, c, "x3@nyuu", &[(mkv, &[3, 6], 8)]);
        put_row_at(&conn, 5, name, e, "old@else", &[(mkv, &[1, 2, 3, 4, 5, 6, 7, 8], 8)], "2026-10-24 07:36:18");
        let id = |i: i64, g: &str| row_of(&conn, i, g).id;
        // two rows of the post and the separate upload read the same PAR2 set: the same file, by its hashes
        for (i, g) in [(1, a), (2, b), (5, e)] {
            conn.execute(
                &format!(
                    "insert into s{}.par2_files (release_id, file_id, set_id, md5, md5_16k, size, name)
                     values (?1, ?3, ?4, ?5, ?6, 800, ?2)",
                    store::shard_of(g)
                ),
                rusqlite::params![id(i, g), mkv, [1u8; 16], [2u8; 16], [3u8; 16], [4u8; 16]],
            )
            .unwrap();
        }
        let copies = crate::duplicates::copies_with(&conn, id(1, a)).unwrap();
        let found: Vec<(crate::duplicates::Copy, i64)> = copies.iter().map(|(kind, r)| (*kind, r.id)).collect();
        assert_eq!(found, [(crate::duplicates::Copy::Twin, id(5, e))], "row 2 is this upload, not a twin of it");
        assert!(crate::duplicates::can_fill(&copies), "the separate upload can still lend articles");
        let own = crate::duplicates::copies_with(&conn, id(2, b)).unwrap();
        assert!(own.iter().all(|(_, r)| r.id != id(1, a)), "{own:?}");
        // a password one row of the post has is the whole post's, though the rows aren't listed as copies
        conn.execute(
            &format!(
                "insert into s{}.release_nfo_password (release_id, password) values (?1, 'x7Q2p')",
                store::shard_of(b)
            ),
            [id(2, b)],
        )
        .unwrap();
        let carried = crate::duplicates::carried_password(&conn, id(1, a), &copies).expect("from row 2");
        assert_eq!((carried.hint.password.as_str(), carried.from), ("x7Q2p", id(2, b)));
    }

    #[test]
    fn a_poster_with_two_of_the_groups_still_gets_the_whole_post() {
        let (_dir, conn) = all_shards();
        let name = "Some.Show.S01E01.2160p.WEB-DL.DV.HDR.H.265-GRP";
        let mkv = "Some.Show.S01E01.mkv";
        let (a, b, c) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
        put_row(&conn, 1, name, a, "x1@nyuu", &[(mkv, &[1, 4], 6)]);
        put_row(&conn, 2, name, b, "x1@nyuu", &[(mkv, &[2, 5], 6)]);
        put_row(&conn, 3, name, c, "x3@nyuu", &[(mkv, &[3, 6], 6)]);
        for (i, g) in [(1, a), (2, b), (3, c)] {
            let post = whole_post_with(&conn, &row_of(&conn, i, g)).unwrap().unwrap();
            assert_eq!(post.ids.len(), 3, "row {i}");
        }
    }

    #[test]
    fn a_row_that_only_a_later_row_links_still_joins() {
        let (_dir, conn) = all_shards();
        let name = "Some.Film.2007.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.Atmos-GRP";
        let (mkv, vol) = ("Some.Film.mkv", "Some.Film.vol00+01.par2");
        let (a, b, c) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
        put_row(&conn, 1, name, a, "x1@nyuu", &[(mkv, &[1, 2], 4)]);
        put_row(&conn, 2, name, b, "x2@nyuu", &[(vol, &[1], 2)]);
        put_row(&conn, 3, name, c, "x3@nyuu", &[(mkv, &[3, 4], 4), (vol, &[2], 2)]);
        for (i, g) in [(1, a), (2, b), (3, c)] {
            let post = whole_post_with(&conn, &row_of(&conn, i, g)).unwrap().unwrap_or_else(|| panic!("row {i}"));
            assert_eq!(post.ids.len(), 3, "row {i}");
            assert!(post.complete, "row {i}");
        }
    }

    #[test]
    fn a_crosspost_under_another_poster_doesnt_split_the_answer() {
        let (_dir, conn) = all_shards();
        let name = "Some.Film.2007.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.Atmos-GRP";
        let mkv = "Some.Film.mkv";
        let (a, b, c, d) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c", "alt.binaries.d");
        // C (lowest id) is A crossposted under another poster; A and B share a poster; D has the last part
        put_row(&conn, 1, name, c, "copy@nyuu", &[(mkv, &[1], 3)]);
        put_row(&conn, 2, name, a, "x1@nyuu", &[(mkv, &[1], 3)]);
        put_row(&conn, 3, name, b, "x1@nyuu", &[(mkv, &[2], 3)]);
        put_row(&conn, 4, name, d, "x4@nyuu", &[(mkv, &[3], 3)]);
        let answers: Vec<Vec<i64>> = [(1, c), (2, a), (3, b), (4, d)]
            .iter()
            .map(|(i, g)| whole_post_with(&conn, &row_of(&conn, *i, g)).unwrap().unwrap().ids)
            .collect();
        assert!(answers.windows(2).all(|w| w[0] == w[1]), "{answers:?}");
        assert_eq!(answers[0].len(), 3);
    }

    #[test]
    fn an_unrelated_upload_with_a_lower_id_doesnt_hide_the_post() {
        let (_dir, conn) = all_shards();
        let name = "Some.Film.2007.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.Atmos-GRP";
        let (a, b, c) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
        // a whole upload of its own under other files, then the split one
        put_row(&conn, 1, name, a, "whole@else", &[("Some.Film.mkv", &[1, 2, 3, 4], 4)]);
        put_row(&conn, 2, name, b, "x2@nyuu", &[("Some.Film.REMUX.mkv", &[1, 2], 4)]);
        put_row(&conn, 3, name, c, "x3@nyuu", &[("Some.Film.REMUX.mkv", &[3, 4], 4)]);
        assert_eq!(whole_post_with(&conn, &row_of(&conn, 1, a)).unwrap(), None);
        for (i, g) in [(2, b), (3, c)] {
            let post = whole_post_with(&conn, &row_of(&conn, i, g)).unwrap().unwrap_or_else(|| panic!("row {i}"));
            assert_eq!(post.ids.len(), 2, "row {i}");
        }
    }

    #[test]
    fn rows_spread_past_a_day_get_one_answer_each() {
        let (_dir, conn) = all_shards();
        let name = "Some.Film.2007.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.Atmos-GRP";
        let mkv = "Some.Film.mkv";
        let (a, b, c) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
        put_row_at(&conn, 1, name, a, "x1@nyuu", &[(mkv, &[1], 3)], "2026-11-01 00:00:00");
        put_row_at(&conn, 2, name, b, "x2@nyuu", &[(mkv, &[2], 3)], "2026-11-01 18:00:00");
        put_row_at(&conn, 3, name, c, "x3@nyuu", &[(mkv, &[3], 3)], "2026-11-02 12:00:00");
        let answers: Vec<Option<Vec<i64>>> = [(1, a), (2, b), (3, c)]
            .iter()
            .map(|(i, g)| whole_post_with(&conn, &row_of(&conn, *i, g)).unwrap().map(|p| p.ids))
            .collect();
        let (one, two) = (row_of(&conn, 1, a).id, row_of(&conn, 2, b).id);
        let mut first = vec![one, two];
        first.sort_unstable();
        assert_eq!(answers[0].as_ref(), Some(&first));
        assert_eq!(answers[1].as_ref(), Some(&first), "the row in the middle goes with the first post");
        assert_eq!(answers[2], None, "too far from it, and alone");
    }

    #[test]
    fn an_older_upload_of_the_same_parts_isnt_a_crosspost() {
        let (_dir, conn) = all_shards();
        let name = "Some.Film.2007.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.Atmos-GRP";
        let mkv = "Some.Film.mkv";
        let (a, b, c) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
        put_row_at(&conn, 1, name, a, "old@else", &[(mkv, &[1], 2)], "2026-10-20 00:00:00");
        put_row_at(&conn, 2, name, b, "x2@nyuu", &[(mkv, &[1], 2)], "2026-10-30 00:00:00");
        put_row_at(&conn, 3, name, c, "x3@nyuu", &[(mkv, &[2], 2)], "2026-10-30 00:10:00");
        for (i, g) in [(2, b), (3, c)] {
            let post = whole_post_with(&conn, &row_of(&conn, i, g)).unwrap().unwrap_or_else(|| panic!("row {i}"));
            assert_eq!(post.ids.len(), 2, "row {i}");
        }
    }

    #[test]
    fn a_name_in_many_groups_still_finds_the_posters_own_rows() {
        let (_dir, conn) = all_shards();
        let name = "Some.Film.2007.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.Atmos-GRP";
        let mkv = "Some.Film.mkv";
        let shard = store::shard_of("alt.binaries.zz.a");
        let groups: Vec<String> = (0..2000)
            .map(|n| format!("alt.binaries.g{n:04}"))
            .filter(|g| store::shard_of(g) == shard)
            .take(70)
            .collect();
        // other posters' rows of the name come first in the name index
        for (n, g) in groups.iter().take(68).enumerate() {
            put_row(&conn, 100 + n as i64, name, g, &format!("other{n}@else"), &[(mkv, &[1, 2], 2)]);
        }
        let (a, b) = (groups[68].as_str(), groups[69].as_str());
        put_row(&conn, 1, name, a, "x1@nyuu", &[(mkv, &[1], 2)]);
        put_row(&conn, 2, name, b, "x1@nyuu", &[(mkv, &[2], 2)]);
        let post = whole_post_with(&conn, &row_of(&conn, 1, a)).unwrap().unwrap();
        assert_eq!(post.ids.len(), 2);
    }

    #[test]
    fn rows_of_another_poster_join_only_where_they_fill_gaps_under_the_same_subject() {
        let (_dir, conn) = all_shards();
        let name = "Some.Film.2007.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.Atmos-GRP";
        let mkv = "Some.Film.mkv";
        let (a, b, c) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
        put_row(&conn, 1, name, a, "x1@nyuu", &[(mkv, &[1, 3], 4)]);
        // a whole upload of its own, by someone else: it has parts the first has too
        put_row(&conn, 2, name, b, "other@else", &[(mkv, &[1, 2, 3, 4], 4)]);
        assert_eq!(whole_post_with(&conn, &row_of(&conn, 1, a)).unwrap(), None);
        assert_eq!(whole_post_with(&conn, &row_of(&conn, 2, b)).unwrap(), None);
        // the missing parts, but under another subject: not shown to be the same file
        put_row(&conn, 3, name, c, "x3@nyuu", &[("Some.Film.mp4", &[2, 4], 4)]);
        assert_eq!(whole_post_with(&conn, &row_of(&conn, 1, a)).unwrap(), None);
    }

    #[test]
    fn a_split_post_is_whole_without_the_thumbnail_its_counter_counts() {
        let (_dir, conn) = all_shards();
        let name = "Some.Film.1997.2160p.MA.WEB-DL.DDP5.1.H.265-GRP";
        let (mkv, vol) = (
            "Some.Film.1997.2160p.MA.WEB-DL.DDP5.1.H.265-GRP.mkv",
            "Some.Film.1997.2160p.MA.WEB-DL.DDP5.1.H.265-GRP.vol00-01.par2",
        );
        let (a, b, c, d) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c", "alt.binaries.d");
        // `[3/3] - "thumbnail.jpg"` went to a row of its own: the film and its volume are all there is, across 4 groups
        let at = "2026-11-07 10:49:51";
        put_row_counted(&conn, 1, name, a, "x1@nyuu", &[(mkv, &[1, 5], 8), (vol, &[1], 2)], at, 3);
        put_row_counted(&conn, 2, name, b, "x2@nyuu", &[(mkv, &[2, 6], 8), (vol, &[2], 2)], at, 3);
        put_row_counted(&conn, 3, name, c, "x3@nyuu", &[(mkv, &[3, 7], 8)], at, 3);
        put_row_counted(&conn, 4, name, d, "x1@nyuu", &[(mkv, &[4, 8], 8)], at, 3);
        for (i, g) in [(1, a), (2, b), (3, c), (4, d)] {
            let row = row_of(&conn, i, g);
            let post = whole_post_with(&conn, &row).unwrap().unwrap_or_else(|| panic!("row {i}"));
            assert_eq!(post.ids.len(), 4, "row {i}");
            assert!(post.complete, "row {i}");
            assert!(!single_post_complete(&conn, row.id).unwrap(), "row {i} alone has half the film");
        }
        // a part of the film gone: not whole
        conn.execute(
            &format!("update s{}.files set seen = x'00' where release_id = ?1 and filename = ?2", store::shard_of(d)),
            rusqlite::params![row_of(&conn, 4, d).id, mkv],
        )
        .unwrap();
        let post = whole_post_with(&conn, &row_of(&conn, 1, a)).unwrap();
        let post = post.expect("the four rows are still one post");
        assert_eq!(post.ids.len(), 4);
        assert!(!post.complete, "a part of the film is gone");
    }

    #[test]
    fn a_group_row_named_as_the_film_joins_the_split_post() {
        let (_dir, conn) = all_shards();
        let name = "Some.Film.1965.2160p.UHD.BluRay.x265-GRP";
        let (mkv, vol) =
            ("Some.Film.1965.2160p.UHD.BluRay.x265-GRP.mkv", "Some.Film.1965.2160p.UHD.BluRay.x265-GRP.vol00-01.par2");
        let (a, b, c, d) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c", "alt.binaries.d");
        let at = "2026-11-07 10:49:51";
        // the film's first article came first in group a, so its row took the film's name; elsewhere a PAR2 file did
        put_row_counted(&conn, 1, mkv, a, "x1@nyuu", &[(mkv, &[1, 5], 8)], at, 3);
        put_row_counted(&conn, 2, name, b, "x2@nyuu", &[(mkv, &[2, 6], 8), (vol, &[1], 2)], at, 3);
        put_row_counted(&conn, 3, name, c, "x3@nyuu", &[(mkv, &[3, 7], 8), (vol, &[2], 2)], at, 3);
        put_row_counted(&conn, 4, name, d, "x1@nyuu", &[(mkv, &[4, 8], 8)], at, 3);
        for (i, g) in [(1, a), (2, b), (3, c), (4, d)] {
            let row = row_of(&conn, i, g);
            let post = whole_post_with(&conn, &row).unwrap().unwrap_or_else(|| panic!("row {i}"));
            assert_eq!(post.ids.len(), 4, "row {i}");
            assert!(post.complete, "row {i}");
        }
        // another film's row named only like it: not part of the post
        put_row_counted(
            &conn,
            5,
            "Some.Film.1965.2160p.UHD.BluRay.x265-GRP.mkv.1",
            b,
            "x9@nyuu",
            &[(mkv, &[1], 8)],
            at,
            3,
        );
        let post = whole_post_with(&conn, &row_of(&conn, 2, b)).unwrap().unwrap();
        assert_eq!(post.ids.len(), 4);
    }

    #[test]
    fn a_post_whose_articles_went_to_several_of_its_groups_is_one_post() {
        let (_dir, conn) = all_shards();
        let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
        let (mkv, vol) = ("Some.Film.mkv", "Some.Film.vol00+01.par2");
        let (a, b, c, d) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c", "alt.binaries.d");
        let at = "2026-11-07 05:16:17";
        // most articles went to two of the four groups: the rows share parts, none is whole, and each has one of its own
        // row, group, poster, the film's parts, the volume's parts
        type Row<'a> = (i64, &'a str, &'a str, &'a [i64], &'a [i64]);
        let rows: [Row; 4] = [
            (1, a, "x1@nyuu", &[1, 2, 5], &[1]),
            (2, b, "x2@nyuu", &[2, 3, 6], &[2]),
            (3, c, "x3@nyuu", &[3, 4, 7], &[1]),
            (4, d, "x1@nyuu", &[4, 1, 8], &[2]),
        ];
        for (i, g, poster, film, volume) in rows {
            put_row_subjects(&conn, i, name, g, poster, &[(mkv, film, 8), (vol, volume, 2)], at, 2, true);
        }
        for (i, g, ..) in rows {
            let row = row_of(&conn, i, g);
            let post = whole_post_with(&conn, &row).unwrap().unwrap_or_else(|| panic!("row {i}"));
            assert_eq!(post.ids.len(), 4, "row {i}");
            assert!(post.complete, "row {i}");
            assert!(!single_post_complete(&conn, row.id).unwrap(), "row {i} alone isn't whole");
        }
    }

    #[test]
    fn rows_sharing_parts_without_the_files_size_stay_apart() {
        let (_dir, conn) = all_shards();
        let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
        let mkv = "Some.Film.mkv";
        let (a, b) = ("alt.binaries.a", "alt.binaries.b");
        let at = "2026-11-07 05:16:17";
        // the same rows as a post whose articles went to two groups, but the subjects don't say how big the file is
        put_row_subjects(&conn, 1, name, a, "x1@nyuu", &[(mkv, &[1, 2, 3], 4)], at, 1, false);
        put_row_subjects(&conn, 2, name, b, "x2@nyuu", &[(mkv, &[3, 4], 4)], at, 1, false);
        assert_eq!(whole_post_with(&conn, &row_of(&conn, 1, a)).unwrap(), None);
        assert_eq!(whole_post_with(&conn, &row_of(&conn, 2, b)).unwrap(), None);
    }

    #[test]
    fn a_whole_upload_sharing_parts_with_a_row_stays_its_own_post() {
        let (_dir, conn) = all_shards();
        let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
        let mkv = "Some.Film.mkv";
        let (a, b, c) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
        let at = "2026-11-07 05:16:17";
        // a whole upload of the same file, and a row that has parts of it, either one first
        put_row_subjects(&conn, 1, name, a, "x1@nyuu", &[(mkv, &[1, 3], 4)], at, 1, true);
        put_row_subjects(&conn, 2, name, b, "whole@else", &[(mkv, &[1, 2, 3, 4], 4)], at, 1, true);
        put_row_subjects(&conn, 3, name, c, "x3@nyuu", &[(mkv, &[2, 3], 4)], at, 1, true);
        assert_eq!(whole_post_with(&conn, &row_of(&conn, 2, b)).unwrap(), None, "the whole upload is alone");
        for (i, g) in [(1, a), (3, c)] {
            let post = whole_post_with(&conn, &row_of(&conn, i, g)).unwrap().unwrap_or_else(|| panic!("row {i}"));
            assert_eq!(post.ids.len(), 2, "row {i}: the two pieces, without the whole upload");
            assert!(!post.complete, "row {i}: part 4 is only in the whole upload");
        }
    }

    #[test]
    fn rows_sharing_parts_get_one_answer_whichever_asks() {
        let (_dir, conn) = all_shards();
        let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
        let mkv = "Some.Film.mkv";
        let (a, b, c) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
        let at = "2026-11-07 05:16:17";
        put_row_subjects(&conn, 1, name, a, "x1@nyuu", &[(mkv, &[1, 2], 4)], at, 1, true);
        put_row_subjects(&conn, 2, name, b, "x2@nyuu", &[(mkv, &[3, 4], 4)], at, 1, true);
        // parts the other two have between them: it joins only if it's looked at before the post is whole
        put_row_subjects(&conn, 3, name, c, "x3@nyuu", &[(mkv, &[2, 3], 4)], at, 1, true);
        let rows = [row_of(&conn, 1, a), row_of(&conn, 2, b), row_of(&conn, 3, c)];
        let answers: Vec<Option<SplitPost>> = rows.iter().map(|r| whole_post_with(&conn, r).unwrap()).collect();
        let post = answers.iter().flatten().next().expect("a post").clone();
        assert!(post.complete);
        assert!(post.ids.contains(&rows[0].id) && post.ids.contains(&rows[1].id));
        for (row, answer) in rows.iter().zip(&answers) {
            match answer {
                Some(p) => assert_eq!(p.ids, post.ids, "row {}", row.id),
                None => assert!(!post.ids.contains(&row.id), "row {} is in the post but got no answer", row.id),
            }
        }
    }

    #[test]
    fn a_post_sharing_parts_counts_each_parts_bytes_once() {
        let (_dir, conn) = all_shards();
        let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
        let mkv = "Some.Film.mkv";
        let (a, b) = ("alt.binaries.a", "alt.binaries.b");
        let at = "2026-11-07 05:16:17";
        // each row holds 300 bytes of it (`put_row_subjects`: 100 a part); the subject says the file is 4 * 716,800
        put_row_subjects(&conn, 1, name, a, "x1@nyuu", &[(mkv, &[1, 2, 3], 4)], at, 1, true);
        put_row_subjects(&conn, 2, name, b, "x2@nyuu", &[(mkv, &[2, 3, 4], 4)], at, 1, true);
        let post = whole_post_with(&conn, &row_of(&conn, 1, a)).unwrap().unwrap();
        assert_eq!(post.ids.len(), 2);
        assert!(post.complete);
        assert_eq!(post.size, 4 * 716_800, "the file once, not 600 bytes of rows");
        // a part gone: its share of the file's size goes too
        let (_dir2, conn2) = all_shards();
        put_row_subjects(&conn2, 1, name, a, "x1@nyuu", &[(mkv, &[1, 2], 4)], at, 1, true);
        put_row_subjects(&conn2, 2, name, b, "x2@nyuu", &[(mkv, &[2, 3], 4)], at, 1, true);
        let post = whole_post_with(&conn2, &row_of(&conn2, 1, a)).unwrap().unwrap();
        assert!(!post.complete);
        assert_eq!(post.size, 3 * 716_800);
    }

    #[test]
    fn a_posters_own_rows_dont_make_a_post_beside_the_one_sharing_parts() {
        let (_dir, conn) = all_shards();
        let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
        let mkv = "Some.Film.mkv";
        let (a, b, c, d) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c", "alt.binaries.d");
        let at = "2026-11-07 05:16:17";
        // p has groups a and c, q has b and d; rows 1 and 2 share part 2
        put_row_subjects(&conn, 1, name, a, "p@nyuu", &[(mkv, &[1, 2], 4)], at, 1, true);
        put_row_subjects(&conn, 2, name, b, "q@nyuu", &[(mkv, &[2, 3], 4)], at, 1, true);
        put_row_subjects(&conn, 3, name, c, "p@nyuu", &[(mkv, &[3], 4)], at, 1, true);
        put_row_subjects(&conn, 4, name, d, "q@nyuu", &[(mkv, &[4], 4)], at, 1, true);
        let rows = [row_of(&conn, 1, a), row_of(&conn, 2, b), row_of(&conn, 3, c), row_of(&conn, 4, d)];
        let answers: Vec<Option<SplitPost>> = rows.iter().map(|r| whole_post_with(&conn, r).unwrap()).collect();
        let post = answers.iter().flatten().next().expect("a post").clone();
        assert!(post.complete);
        for (row, answer) in rows.iter().zip(&answers) {
            match answer {
                Some(p) => assert_eq!(p.ids, post.ids, "row {}", row.id),
                None => assert!(!post.ids.contains(&row.id), "row {} is in the post but got no answer", row.id),
            }
        }
    }

    #[test]
    fn a_row_whole_on_its_own_stays_out_whichever_row_comes_first() {
        let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
        let (mkv, vol) =
            ("Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP.mkv", "Some.Film.vol00+01.par2");
        let (a, b, c) = ("alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
        let at = "2026-11-07 05:16:17";
        // the film alone and whole in one group (whole by `one_video_named`), its pieces and the volume elsewhere
        for whole_first in [false, true] {
            let (_dir, conn) = all_shards();
            let (w, x, z) = if whole_first { (1, 2, 3) } else { (3, 1, 2) };
            put_row_subjects(&conn, x, name, a, "x@nyuu", &[(mkv, &[1, 2], 4), (vol, &[1], 2)], at, 2, true);
            put_row_subjects(&conn, w, name, b, "w@else", &[(mkv, &[1, 2, 3, 4], 4)], at, 2, true);
            put_row_subjects(&conn, z, name, c, "z@nyuu", &[(mkv, &[3], 4), (vol, &[2], 2)], at, 2, true);
            assert_eq!(whole_post_with(&conn, &row_of(&conn, w, b)).unwrap(), None, "whole first: {whole_first}");
            let post = whole_post_with(&conn, &row_of(&conn, x, a)).unwrap().unwrap();
            assert_eq!(post.ids.len(), 2, "whole first: {whole_first}");
            assert!(!post.complete, "part 4 is only in the whole one, whole first: {whole_first}");
        }
    }

    /// `n` group names that all go to one shard, soo their rows' ids come in the order they're given.
    fn groups_in_one_shard(n: usize) -> Vec<String> {
        let shard = store::shard_of("alt.binaries.zz.a");
        (0..4000).map(|i| format!("alt.binaries.g{i:04}")).filter(|g| store::shard_of(g) == shard).take(n).collect()
    }

    #[test]
    fn a_split_post_keeps_its_answer_beside_a_partial_repost() {
        let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
        let mkv = "Some.Film.mkv";
        let at = "2026-11-07 05:16:17";
        let g = groups_in_one_shard(3);
        // P splits the film across a and b; R posted it again to c, a part short. Every order of ids
        for (pa, pb, rc) in [(1, 2, 3), (2, 3, 1), (1, 3, 2)] {
            let (_dir, conn) = all_shards();
            put_row_upload(&conn, pa, name, &g[0], "p@nyuu", &[(mkv, &[1, 2], 4)], at, 1, Some("p"));
            put_row_upload(&conn, pb, name, &g[1], "p@nyuu", &[(mkv, &[3, 4], 4)], at, 1, Some("p"));
            put_row_upload(&conn, rc, name, &g[2], "r@else", &[(mkv, &[1, 2, 3], 4)], at, 1, Some("r"));
            let (a, b, c) = (row_of(&conn, pa, &g[0]), row_of(&conn, pb, &g[1]), row_of(&conn, rc, &g[2]));
            let mut both = vec![a.id, b.id];
            both.sort_unstable();
            for row in [&a, &b] {
                let post = whole_post_with(&conn, row).unwrap().unwrap_or_else(|| panic!("order {pa}{pb}{rc}"));
                assert_eq!(post.ids, both, "order {pa}{pb}{rc}");
                assert!(post.complete, "order {pa}{pb}{rc}");
            }
            assert_eq!(whole_post_with(&conn, &c).unwrap(), None, "order {pa}{pb}{rc}");
        }
    }

    #[test]
    fn a_split_post_stays_whole_beside_another_posters_part_with_a_lower_id() {
        let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
        let mkv = "Some.Film.mkv";
        let at = "2026-11-07 05:16:17";
        let g = groups_in_one_shard(3);
        // subjects without a size: P's two rows are the whole post; Q's row has one of its parts, whatever the ids
        for (x, a, c) in [(1, 2, 3), (2, 1, 3), (3, 1, 2)] {
            let (_dir, conn) = all_shards();
            put_row_subjects(&conn, x, name, &g[0], "q@nyuu", &[(mkv, &[3], 4)], at, 1, false);
            put_row_subjects(&conn, a, name, &g[1], "p@nyuu", &[(mkv, &[1, 2], 4)], at, 1, false);
            put_row_subjects(&conn, c, name, &g[2], "p@nyuu", &[(mkv, &[3, 4], 4)], at, 1, false);
            let (ra, rc) = (row_of(&conn, a, &g[1]), row_of(&conn, c, &g[2]));
            let mut both = vec![ra.id, rc.id];
            both.sort_unstable();
            for row in [&ra, &rc] {
                let post = whole_post_with(&conn, row).unwrap().unwrap_or_else(|| panic!("order {x}{a}{c}"));
                assert_eq!(post.ids, both, "order {x}{a}{c}");
                assert!(post.complete, "order {x}{a}{c}");
            }
        }
    }

    #[test]
    fn two_uploads_of_a_file_alike_in_every_header_are_whole_together() {
        let (_dir, conn) = all_shards();
        let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
        let mkv = "Some.Film.mkv";
        let (a, b) = ("alt.binaries.a", "alt.binaries.b");
        let at = "2026-11-07 05:16:17";
        // the same name, part count and size, each a part short: a part of one stands in for the same part of the other
        put_row_upload(&conn, 1, name, a, "p@nyuu", &[(mkv, &[1, 2, 3], 4)], at, 1, Some("p"));
        put_row_upload(&conn, 2, name, b, "q@nyuu", &[(mkv, &[1, 2, 4], 4)], at, 1, Some("q"));
        for (i, g) in [(1, a), (2, b)] {
            let post = whole_post_with(&conn, &row_of(&conn, i, g)).unwrap().unwrap_or_else(|| panic!("row {i}"));
            assert_eq!(post.ids.len(), 2, "row {i}");
            assert!(post.complete, "row {i}");
        }
        // its contents count each part's bytes once, whichever upload has it: 4 parts of 100 bytes
        let post = whole_post_with(&conn, &row_of(&conn, 1, a)).unwrap().unwrap();
        assert_eq!(contents_of_split(&conn, &post).unwrap().data_bytes, 400);
    }

    #[test]
    fn one_posters_rows_sharing_parts_are_one_post() {
        let (_dir, conn) = all_shards();
        let name = "Some.Film.2026.2160p.UHD.Blu-ray.Remux.HDR.HEVC.DTS-HD.MA.5.1-GRP";
        let mkv = "Some.Film.mkv";
        let (a, b) = ("alt.binaries.a", "alt.binaries.b");
        let at = "2026-11-07 05:16:17";
        put_row_subjects(&conn, 1, name, a, "p@nyuu", &[(mkv, &[1, 2, 3], 4)], at, 1, true);
        put_row_subjects(&conn, 2, name, b, "p@nyuu", &[(mkv, &[2, 3, 4], 4)], at, 1, true);
        for (i, g) in [(1, a), (2, b)] {
            let post = whole_post_with(&conn, &row_of(&conn, i, g)).unwrap().unwrap_or_else(|| panic!("row {i}"));
            assert_eq!(post.ids.len(), 2, "row {i}");
            assert!(post.complete, "row {i}");
        }
        // the post's contents count an article the two rows share once: 4 articles of 100 bytes
        let post = whole_post_with(&conn, &row_of(&conn, 1, a)).unwrap().unwrap();
        let contents = contents_of_split(&conn, &post).unwrap();
        assert_eq!(contents.data_bytes, 400);
    }

    #[test]
    fn a_subject_has_the_files_size_only_as_its_last_number() {
        assert!(subject_has_size(r#"[10/11] - "Film.mkv" yEnc (/21040) 88246893816"#));
        assert!(!subject_has_size(r#"[10/11] - "Film.mkv" yEnc (/21040)"#));
        assert!(!subject_has_size(r#"[10/11] - "Film.mkv" yEnc (/21040) 88 MB"#));
        assert!(!subject_has_size("Film.mkv 88246893816"));
        assert_eq!(subject_size(r#""Film.mkv" yEnc (/21040) 88246893816"#), Some(88_246_893_816));
    }

    #[test]
    fn a_film_whose_counter_says_more_files_is_whole_only_when_named_as_its_post() {
        let (_dir, conn) = all_shards();
        let (a, b) = ("alt.binaries.a", "alt.binaries.b");
        let at = "2026-11-07 10:49:51";
        // the whole film alone in one row, its counter counting a thumbnail: named as the row, or not
        put_row_counted(&conn, 1, "Film.2020.2160p-G", a, "x@nyuu", &[("Film.2020.2160p-G.mkv", &[1, 2], 2)], at, 3);
        put_row_counted(&conn, 2, "Film.2020.2160p-G", b, "y@nyuu", &[("abc.mkv", &[1, 2], 2)], at, 3);
        assert!(single_post_complete(&conn, row_of(&conn, 1, a).id).unwrap());
        assert!(!single_post_complete(&conn, row_of(&conn, 2, b).id).unwrap());
    }
}
