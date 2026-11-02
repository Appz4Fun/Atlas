//! What the release page says about a posting itself: who posted it, its names, its PAR2 files
//! and how much of it is there. Read when the page opens, soo nothing here is stored.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap};
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

/// Totals over a release's files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Contents {
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

/// The `MM` of `name.vol000+MM.par2`.
fn recovery_blocks(filename: &str) -> i64 {
    static BLOCKS: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"(?i)\.vol\d+[+-](\d+)\.par2$").unwrap()
    });
    BLOCKS.captures(filename).and_then(|hit| hit[1].parse().ok()).unwrap_or(0)
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
    contents_of(conn, &[id])
}

fn contents_of(conn: &Connection, ids: &[i64]) -> Result<Contents> {
    let mut totals = Contents::default();
    let union = union_files(conn, ids)?;
    for (filename, file) in &union.files {
        let seen = store::seen_count(&file.seen);
        totals.files += 1;
        totals.articles_seen += seen;
        let want = file.expected.unwrap_or(seen).max(seen);
        totals.articles_expected += want;
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
}

impl SplitPost {
    /// The one to list. A search that isn't about one group shows only this.
    pub fn listed_id(&self) -> i64 {
        self.ids.iter().copied().max().unwrap_or(0)
    }
}

/// How far apart a split post's groups can have got their articles, in days. Each row keeps its newest article's date,
/// and real ones are minutes apart.
const SPLIT_POST_DAYS: f64 = 1.0;

/// The rest of this release's post in other groups, if it has any. None: it's all here, or the rows that look like it
/// don't add up to one upload. Name and poster are only what the headers say, and anyone can copy those to another group,
/// soo any doubt leaves every row on its own, and every row of a post gets the same answer.
pub fn split_post_with(conn: &Connection, release: &crate::search::ReleaseRow) -> Result<Option<SplitPost>> {
    let posted = names_with(conn, release.id)?.map_or(release.name.clone(), |n| n.posted);
    let (mut ids, mut groups, mut size, mut dates) = (Vec::new(), Vec::new(), 0, Vec::new());
    for s in 0..store::SHARDS {
        let mut stmt = conn.prepare_cached(&format!(
            "select id, group_name, coalesce(size, 0), julianday(posted_date) from s{s}.releases
             where name = ?1 and poster is ?2 and not atlas_blocked(group_name) limit 64"
        ))?;
        let rows = stmt.query_map(rusqlite::params![posted, release.poster], |r| {
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
    if ids.len() < 2 || !close_together(&dates) {
        return Ok(None);
    }
    ids.sort_unstable();
    groups.sort_unstable();
    groups.dedup();
    let files = union_files(conn, &ids)?;
    if !files.one_upload() {
        return Ok(None);
    }
    Ok(Some(SplitPost { ids, groups, size, complete: files.complete() }))
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
}

impl UnionFiles {
    /// Whether the releases can be parts of one upload. Each article went to one group, soo no part is in two of them,
    /// they say the same of every file they share, and each shares one with the rest.
    fn one_upload(&self) -> bool {
        let mut totals = self.file_totals.iter();
        let total_fits = match (totals.next(), totals.next()) {
            (Some(Some(total)), None) => self.files.len() as i64 <= *total,
            // no count to go by, soo every release has the same files
            (Some(None), None) => self.rows.windows(2).all(|pair| pair[0] == pair[1]),
            _ => false,
        };
        !self.conflict && total_fits && self.linked()
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

    /// Whether every file the post says it has is there with exactly its parts.
    fn complete(&self) -> bool {
        let total = self.file_totals.iter().next().copied().flatten();
        !self.files.is_empty()
            && total.is_none_or(|total| self.files.len() as i64 == total)
            && self.files.values().all(|f| f.expected.is_some_and(|want| store::is_exactly(&f.seen, want)))
    }
}

/// A subject without the number that changes from part to part, the N in `(N/M)`.
fn subject_of_file(subject: &str) -> String {
    for (open, _) in subject.rmatch_indices('(') {
        let rest = &subject[open + 1..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let counter = rest[digits..].trim_start();
        if digits > 0 && counter.starts_with('/') {
            return format!("{}({counter}", &subject[..open]);
        }
    }
    subject.to_string()
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
            filenames.insert(filename.clone());
            let subject = subject.as_deref().map(subject_of_file);
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
    contents_of(conn, &post.ids)
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
    for (filename, subject) in files {
        if let Some(password) = find_password(&filename) {
            return hint(password, "a file name");
        }
        if let Some(password) = subject.as_deref().and_then(find_password) {
            return hint(password, "the subject");
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(recovery_blocks("a.par2"), 0);
        let c = Contents { recovery_blocks: 10, par2_block: Some(1000), par2_blocks: Some(100), ..Contents::default() };
        assert_eq!(c.exact_repair_per_mille(), Some(100), "10 of 100 blocks");
        assert_eq!(Contents::default().exact_repair_per_mille(), None);
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
