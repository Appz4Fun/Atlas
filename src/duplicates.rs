//! Pure relationship facts, fingerprints, names and article-vector transforms.
//! Native database readers own their source bindings and verified payload
//! admission. Fingerprints here are candidate descriptions; calling these
//! functions does not grant native copying, inheritance or lending authority.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use sha1::{Digest, Sha1};

use crate::par2::Par2File;
use crate::search::{ArticleRow, ReleaseRow};

/// Files smaller than this don't count: NFOs, samples' subtitles, and the like differ between postings.
const MIN_CONTENT_FILE: u64 = 1 << 20;

/// Everything a posting is, from a complete PAR2 set and the files posted with it: the set itself and its block size,
/// each file's name, size and MD5, and every posted file's name with its article count, recovery volumes included.
/// The versioned result describes these supplied facts. Native lending also
/// requires the caller's actual target-bound verified payload admission.
pub fn twin_fingerprint(files: &[Par2File], block_size: Option<u64>, posted: &[(&str, i64)]) -> Option<Vec<u8>> {
    let first = files.first()?;
    let mut named: Vec<(&str, u64, [u8; 16])> = files.iter().map(|f| (f.name.as_str(), f.size, f.md5)).collect();
    named.sort_unstable();
    named.dedup();
    let mut posted = posted.to_vec();
    posted.sort_unstable();
    let mut sha = Sha1::new();
    sha.update(first.set_id);
    sha.update(block_size.unwrap_or(0).to_le_bytes());
    sha.update((posted.len() as u64).to_le_bytes());
    for (name, parts) in posted {
        sha.update((name.len() as u64).to_le_bytes());
        sha.update(name.as_bytes());
        sha.update(parts.to_le_bytes());
    }
    for (name, size, md5) in named {
        sha.update((name.len() as u64).to_le_bytes());
        sha.update(name.as_bytes());
        sha.update(size.to_le_bytes());
        sha.update(md5);
    }
    let mut out = vec![TWIN_VERSION];
    out.extend(sha.finalize());
    Some(out)
}

/// Marks a fingerprint as this kind. Ones from before a twin meant all of this are as long as the hash alone.
const TWIN_VERSION: u8 = 2;

/// Whether one archive's listing says what a whole release holds: everything real in it is that archive's volumes.
/// Names that differ count as different archives, soo an obfuscated set makes no fingerprint, which is the safe way
/// to be wrong: a release with another archive beside the one read must not match one without it.
pub fn one_archive_set<'a>(mut names: impl Iterator<Item = &'a str>) -> bool {
    use crate::archive::select::{Kind, classify};
    let mut set = None;
    names.all(|name| {
        let family = match classify(name) {
            Kind::Par2Base | Kind::Par2Vol(_) | Kind::Nfo | Kind::Nzb | Kind::Image => return true,
            Kind::Rar(_) => "rar",
            Kind::SevenZip(_) => "7z",
            Kind::Zip(_) => "zip",
            Kind::Numbered(_) => "numbered",
            Kind::NotArchive | Kind::Unidentified => {
                let lower = name.to_ascii_lowercase();
                return is_sample(&lower) || lower.ends_with(".sfv");
            }
        };
        let this = (family, volume_set(name));
        *set.get_or_insert(this) == this
    }) && set.is_some()
}

/// A volume's name without what makes it that volume: `a.part01.rar`, `a.r00`, `a.z01`, `a.7z.001` and `a.001` are `a`.
pub(crate) fn volume_set(name: &str) -> &str {
    static VOLUME_END: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"(?i)(\.part\d+\.rar|\.7z\.\d+|\.[^./\\]+)$").unwrap()
    });
    let name = crate::archive::names::base_name(name);
    VOLUME_END.find(name).filter(|m| m.start() > 0).map_or(name, |m| &name[..m.start()])
}

/// Whether any of these is an archive, soo a list of them isn't a list of what's inside.
pub fn lists_archives<'a>(mut names: impl Iterator<Item = &'a str>) -> bool {
    names.any(is_archive)
}

fn is_archive(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or_default();
    matches!(ext, "rar" | "zip" | "7z" | "par2" | "ace" | "arj" | "cab" | "lzh" | "tar" | "gz" | "bz2" | "xz")
        || (ext.len() == 3 && ext.starts_with('r') && ext[1..].bytes().all(|b| b.is_ascii_digit()))
        || (ext.len() == 3 && ext.bytes().all(|b| b.is_ascii_digit()))
}

fn is_sample(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("sample") || lower.contains("proof")
}

/// What's in a release, from what's inside its archives, or its PAR2 set when it isn't packed: the exact
/// sizes of its real files. Names don't count, because obfuscation changes them. None: a file is packed,
/// soo these aren't the real files, a real file's size isn't known, or there's nothing big enough to go by.
pub fn content_fingerprint<'a>(files: impl Iterator<Item = (&'a str, Option<u64>)>) -> Option<Vec<u8>> {
    let mut sizes = Vec::new();
    for (name, size) in files {
        if is_archive(name) {
            return None;
        }
        if is_sample(name) {
            continue;
        }
        // leaving it out would make this the same as a release without that file
        let size = size?;
        if size >= MIN_CONTENT_FILE {
            sizes.push(size);
        }
    }
    if sizes.is_empty() {
        return None;
    }
    sizes.sort_unstable();
    let mut sha = Sha1::new();
    for size in sizes {
        sha.update(size.to_le_bytes());
    }
    Some(sha.finalize().to_vec())
}

/// What another posting is to a release, in the order a page lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Copy {
    /// the same upload in another group: the same articles under the same message-ids
    Xpost,
    /// another upload of the same files, cut up the same way: other message-ids, the same bytes
    Twin,
    Alt,
}

/// A release's (posted, content) fingerprints, either one when a read has kept it.

/// Which copies a list of clusters is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClusterKind {
    Twins,
    Alts,
}

impl ClusterKind {
    pub fn column(self) -> &'static str {
        match self {
            ClusterKind::Twins => "posted",
            ClusterKind::Alts => "content",
        }
    }
}

/// How many other copies a release has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CopyCount {
    /// the same upload in other groups
    pub xposts: i64,
    /// other uploads of the same files
    pub twins: i64,
    pub alts: i64,
}

/// A supplied file-name mapping for an already admitted relationship. Native
/// callers must bind verified payload bytes separately before transforming
/// article vectors with this mapping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileTwin {
    pub id: i64,
    /// (what the other posting calls the file, what this one does), by the names their sets gave
    pub names: Vec<(String, String)>,
    /// both sets describe one data file: a posting of it under a random name is that file
    pub one_file: bool,
}

/// A name a set gives a file beside the data: its own volumes and the sidecars posted with it.
fn is_sidecar(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".par2", ".nfo", ".sfv", ".srr", ".srs", ".txt"].iter().any(|ext| lower.ends_with(ext))
}

/// A password found on one posting, shown on another that holds the same bytes: an xpost or twin of it. The same
/// archive bytes take the same password, soo it carries as it was found; whether it works was only ever tried on
/// the posting it came from.
pub struct Inherited {
    pub hint: crate::release_info::PasswordHint,
    /// the posting it was found on
    pub from: i64,
    pub from_name: String,
}

/// The existing own-set name hint, with the reads supplied by a native RR
/// loader. A hint is never a password verdict or cross-post payload proof.
pub(crate) fn par2_password_stem<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let mut data = names.into_iter().filter(|name| !is_sidecar(name));
    let first = data.next()?;
    if !is_archive(first) {
        return None;
    }
    let stem = volume_set(first);
    (looks_generated(stem) && data.all(|name| is_archive(name) && volume_set(name) == stem)).then_some(stem)
}

/// A name an uploader made up, not a title: letters and digits only, both cases.
fn looks_generated(stem: &str) -> bool {
    (10..=64).contains(&stem.len())
        && stem.bytes().all(|b| b.is_ascii_alphanumeric())
        && stem.bytes().any(|b| b.is_ascii_uppercase())
        && stem.bytes().any(|b| b.is_ascii_lowercase())
}

/// The posted name a set's name stands for among a release's articles: the same name, or the same but for case, or,
/// when `one_archive` (the set's names are one archive's volumes), the volume with the same ending under the one
/// archive name this posting used, or, when `one_file` (the set describes one data file), the one data file of a row
/// that posted it under a random name.
fn posted_name<'a>(articles: &'a [ArticleRow], name: &str, one_file: bool, one_archive: bool) -> Option<&'a str> {
    let names: Vec<&str> = {
        let mut seen: Vec<&str> = articles.iter().filter_map(|a| a.filename.as_deref()).collect();
        seen.sort_unstable();
        seen.dedup();
        seen
    };
    if let Some(found) = names.iter().copied().find(|n| *n == name) {
        return Some(found);
    }
    if let Some(found) = names.iter().copied().find(|n| n.eq_ignore_ascii_case(name)) {
        return Some(found);
    }
    if let Some(found) = renamed_volume(&names, name).filter(|_| one_archive) {
        return Some(found);
    }
    if !one_file || is_sidecar(name) {
        return None;
    }
    let data: Vec<&str> = names.iter().copied().filter(|n| !is_sidecar(n)).collect();
    match data[..] {
        [one] => Some(one),
        _ => None,
    }
}

/// The posted name of a volume a set names under another archive name: a repost that renamed every volume of one
/// archive (`YMFF.part001.rar` posted as `oCBv.part001.rar`) and kept the set made for the first upload. Only when
/// the posting's data files are all volumes of one archive, soo the set's name and the posted one stand for the same
/// thing, and only the volume with the very same ending.
fn renamed_volume<'a>(names: &[&'a str], name: &str) -> Option<&'a str> {
    if is_sidecar(name) {
        return None;
    }
    let base = crate::archive::names::base_name(name);
    let suffix = &base[volume_set(name).len()..];
    if !is_volume_ending(suffix) {
        return None;
    }
    let mut stems = names.iter().filter(|n| !is_sidecar(n)).map(|n| volume_set(n));
    let posted = stems.next()?;
    if !stems.all(|stem| stem == posted) {
        return None;
    }
    names
        .iter()
        .copied()
        .find(|n| !is_sidecar(n) && crate::archive::names::base_name(n)[posted.len()..].eq_ignore_ascii_case(suffix))
}

/// Whether what follows an archive's name is a volume's numbered ending: `.part001.rar`, `.r00`, `.z01`, `.7z.001` or
/// `.001`. A plain `.mkv` says nothing about which file it is.
fn is_volume_ending(suffix: &str) -> bool {
    static VOLUME: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"(?i)^\.(part\d+\.rar|r\d{2,}|z\d{2,}|7z\.\d+|\d{3,})$").unwrap()
    });
    VOLUME.is_match(suffix)
}

/// Whether a set's names, sidecars aside, are all volumes of one archive.
fn one_archive<'a>(names: impl Iterator<Item = &'a str>) -> bool {
    let mut stems = names.filter(|n| !is_sidecar(n)).map(volume_set);
    let Some(first) = stems.next() else { return false };
    stems.all(|stem| stem == first)
}

/// What articles were found to hold, by message-id: `(decoded length, CRC32)` from `segment_crc`, for the two
/// postings a fill is between. A part both have and both know must agree, or the two files are not the same bytes
/// however alike their sets and names.
pub type KnownCrcs = HashMap<String, (i64, i64)>;

/// Adds what `own` is missing from a file twin's articles: each of the twin's files the sets say is one of this
/// release's is read as that file, and fills its gaps by `fill_gaps`'s rules. Nothing else of the twin's is touched:
/// two postings that share an NFO can hold different archives under the same names. Returns how many.
pub fn fill_gaps_from(
    own: &mut Vec<ArticleRow>,
    twin: &[ArticleRow],
    names: &[(String, String)],
    one_file: bool,
    known: &KnownCrcs,
) -> usize {
    let mut renamed: Vec<ArticleRow> = Vec::new();
    // a renamed repost's volumes map by their endings only when each side's names are one archive's volumes
    let theirs_one = one_archive(names.iter().map(|(theirs, _)| theirs.as_str()));
    let mine_one = one_archive(names.iter().map(|(_, mine)| mine.as_str()));
    for (theirs, mine) in names {
        let (Some(from), Some(to)) =
            (posted_name(twin, theirs, one_file, theirs_one), posted_name(own, mine, one_file, mine_one))
        else {
            continue;
        };
        let to = to.to_string();
        renamed.extend(
            twin.iter()
                .filter(|a| a.filename.as_deref() == Some(from))
                .map(|a| ArticleRow { filename: Some(to.clone()), ..a.clone() }),
        );
    }
    if renamed.is_empty() {
        return 0;
    }
    fill_gaps_checked(own, &renamed, known)
}

/// Whether the file posted as `name` is missing parts among `articles`: fewer distinct parts than its subject counts.
fn has_gaps(articles: &[ArticleRow], name: &str) -> bool {
    let mut parts: HashSet<i64> = HashSet::new();
    let mut total = None;
    for a in articles.iter().filter(|a| a.filename.as_deref() == Some(name)) {
        parts.extend(a.part);
        total = total.or(a.total_parts);
    }
    total.is_some_and(|t| (parts.len() as i64) < t)
}

/// `fill_gaps_checked` with nothing known about the articles' bytes.
pub fn fill_gaps(own: &mut Vec<ArticleRow>, twin: &[ArticleRow]) -> usize {
    fill_gaps_checked(own, twin, &KnownCrcs::new())
}

/// Adds what `own` is missing from a twin's articles, file by file. A twin's file counts only under the
/// same name, with the same number of parts, the same size for every part both have, and the same decoded length and
/// CRC32 for every part both have that `known` has read: one that differs says the two files are not the same bytes,
/// and the twin lends nothing of that file. Returns how many.
pub fn fill_gaps_checked(own: &mut Vec<ArticleRow>, twin: &[ArticleRow], known: &KnownCrcs) -> usize {
    /// a file's parts this posting has: each part's wire size and message-id
    type Parts = HashMap<i64, (Option<i64>, String)>;
    let mut have: HashMap<Option<String>, Parts> = HashMap::new();
    let mut templates: HashMap<Option<String>, ArticleRow> = HashMap::new();
    for article in own.iter() {
        if let Some(part) = article.part {
            have.entry(article.filename.clone()).or_default().insert(part, (article.bytes, article.message_id.clone()));
        }
        templates.entry(article.filename.clone()).or_insert_with(|| article.clone());
    }
    let mut by_file: HashMap<&Option<String>, Vec<&ArticleRow>> = HashMap::new();
    for article in twin {
        by_file.entry(&article.filename).or_default().push(article);
    }
    let mut added = 0;
    for (filename, articles) in by_file {
        let (Some(parts), Some(template)) = (have.get(filename), templates.get(filename)) else { continue };
        let same_total = articles.iter().all(|a| a.total_parts.is_some() && a.total_parts == template.total_parts);
        let same_sizes = articles
            .iter()
            .all(|a| a.part.and_then(|part| parts.get(&part)).is_none_or(|(bytes, _)| *bytes == a.bytes));
        // a part both have, both read: the same bytes or not the same file
        let same_bytes = articles.iter().all(|a| {
            let (Some(theirs), Some((_, mine))) = (known.get(&a.message_id), a.part.and_then(|part| parts.get(&part)))
            else {
                return true;
            };
            known.get(mine).is_none_or(|mine| mine == theirs)
        });
        // with no part in common, nothing says the two were cut up the same way
        let shared = articles.iter().any(|a| a.part.is_some_and(|part| parts.contains_key(&part)));
        if !same_total || !same_sizes || !same_bytes || !shared {
            continue;
        }
        for article in articles {
            if article.part.is_some_and(|part| !parts.contains_key(&part)) {
                own.push(ArticleRow {
                    message_id: article.message_id.clone(),
                    part: article.part,
                    bytes: article.bytes,
                    ..template.clone()
                });
                added += 1;
            }
        }
    }
    if added > 0 {
        own.sort_by(|a, b| (&a.filename, a.part, &a.message_id).cmp(&(&b.filename, b.part, &b.message_id)));
    }
    added
}

/// Whether a twin of this release can lend it articles.
pub fn can_fill(copies: &[(Copy, ReleaseRow)]) -> bool {
    // an xpost's row can have parts this group's listing missed: the same articles, read in another group
    copies.iter().any(|(kind, _)| matches!(kind, Copy::Twin | Copy::Xpost))
}

/// How many members before it a cluster member is checked against: a cluster is rarely more than a few uploads.
const MAX_XPOST_WITNESSES: usize = 64;

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1 << 30;

    fn content(files: &[(&str, u64)]) -> Option<Vec<u8>> {
        content_fingerprint(files.iter().map(|(name, size)| (*name, Some(*size))))
    }

    fn par2(size: u64, md5: u8) -> Par2File {
        Par2File {
            set_id: [0; 16],
            file_id: [0; 16],
            md5: [md5; 16],
            md5_16k: [0; 16],
            size,
            name: "x".into(),
            bound: true,
            crc32: None,
        }
    }

    #[test]
    fn only_a_made_up_name_is_a_password_hint() {
        assert!(looks_generated("YMFFZeoeVI6fG4iz"));
        assert!(looks_generated("oCBvoIlhFvS0L3k1"));
        assert!(!looks_generated("Gone.with.the.Wind.1939"), "a title");
        assert!(!looks_generated("abcdefghijklmnop"), "one case");
        assert!(!looks_generated("Short1"), "too short");
    }

    #[test]
    fn a_list_of_volumes_is_a_list_of_archives() {
        assert!(lists_archives(["a.part01.rar", "a.nfo"].into_iter()));
        assert!(!lists_archives(["a.mkv", "a.nfo"].into_iter()));
    }

    #[test]
    fn only_one_archive_speaks_for_a_release() {
        let one = |names: &[&str]| one_archive_set(names.iter().copied());
        assert!(one(&["a.part01.rar", "a.part02.rar", "a.par2", "a.vol0+1.par2", "a.nfo", "a-sample.mkv"]));
        assert!(one(&["a.rar", "a.r00", "a.r01"]));
        assert!(one(&["a.7z.001", "a.7z.002"]));
        assert!(one(&["a.z01", "a.z02", "a.zip"]), "a split ZIP");
        assert!(one(&["a.rar", "a.s00"]));
        assert!(one(&["a.cbz"]));
        assert!(!one(&["a.rar", "a.zip"]), "same name, different archives");
        assert!(!one(&["a.rar", "b.rar"]), "two archives");
        assert!(!one(&["a.rar", "a.mkv"]), "something outside it");
        assert!(!one(&["a.nfo"]), "no archive at all");
    }

    #[test]
    fn a_file_of_unknown_size_makes_no_content_fingerprint() {
        assert!(content_fingerprint([("a.mkv", Some(GB)), ("b.mkv", None)].into_iter()).is_none());
        assert!(content_fingerprint([("a.mkv", Some(GB)), ("sample.mkv", None)].into_iter()).is_some());
    }

    #[test]
    fn a_twin_is_the_same_in_everything() {
        let file = |name: &str, md5: u8| Par2File { name: name.into(), ..par2(GB, md5) };
        let files = [file("a.rar", 1), file("b.rar", 2)];
        let posted = [("a.rar", 1400), ("b.rar", 1400), ("a.par2", 1), ("a.vol000+16.par2", 9)];
        let print = |files: &[Par2File], block, posted: &[(&str, i64)]| twin_fingerprint(files, Some(block), posted);
        let base = print(&files, 1024, &posted);
        assert!(base.is_some());
        let shuffled = [posted[3], posted[0], posted[2], posted[1]];
        assert_eq!(base, print(&[file("b.rar", 2), file("a.rar", 1)], 1024, &shuffled), "in any order");
        assert_ne!(base, print(&[file("a.rar", 2), file("b.rar", 1)], 1024, &posted), "swapped names");
        assert_ne!(base, print(&[file("a.rar", 1), file("b.rar", 3)], 1024, &posted), "other bytes");
        assert_ne!(base, print(&files, 2048, &posted), "another block size");
        let other_volume = [posted[0], posted[1], posted[2], ("a.vol016+16.par2", 9)];
        assert_ne!(base, print(&files, 1024, &other_volume), "other recovery slices");
        let more = [posted[0], posted[1], posted[2], posted[3], ("a.vol016+16.par2", 9)];
        assert_ne!(base, print(&files, 1024, &more), "more recovery");
        let swapped_counts = [("a.rar", 1300), ("b.rar", 1500), posted[2], posted[3]];
        let other_counts = [("a.rar", 1500), ("b.rar", 1300), posted[2], posted[3]];
        assert_ne!(print(&files, 1024, &swapped_counts), print(&files, 1024, &other_counts), "cut up differently");
        let other_set = Par2File { set_id: [7; 16], ..file("a.rar", 1) };
        assert_ne!(base, print(&[other_set, file("b.rar", 2)], 1024, &posted), "another PAR2 set");
        assert_eq!(twin_fingerprint(&[], Some(1024), &posted), None);
    }

    #[test]
    fn content_matches_on_sizes_whatever_the_names() {
        let a = content(&[("Movie.2024.mkv", 5 * GB), ("Movie.nfo", 2_000)]);
        assert!(a.is_some());
        assert_eq!(a, content(&[("c2f94a1b.mkv", 5 * GB)]), "obfuscated, and no NFO");
        assert_eq!(a, content(&[("Movie.2024.mkv", 5 * GB), ("Sample/movie-sample.mkv", 50 << 20)]), "plus a sample");
        assert_ne!(a, content(&[("Movie.2024.mkv", 5 * GB + 1)]), "another encode");
    }

    fn article(file: &str, part: i64, total: i64, bytes: i64, id: &str) -> ArticleRow {
        ArticleRow {
            message_id: id.into(),
            filename: Some(file.into()),
            part: Some(part),
            total_parts: Some(total),
            bytes: Some(bytes),
            subject: Some(format!("own {file}")),
            poster: None,
            posted_date: None,
        }
    }

    #[test]
    fn gaps_fill_only_from_a_file_that_matches() {
        let mut own = vec![article("a.rar", 1, 3, 100, "own1"), article("a.rar", 3, 3, 50, "own3")];
        let twin = vec![
            article("a.rar", 1, 3, 100, "t1"),
            article("a.rar", 2, 3, 100, "t2"),
            article("a.rar", 3, 3, 50, "t3"),
            article("other.rar", 1, 1, 10, "x"),
        ];
        assert_eq!(fill_gaps(&mut own, &twin), 1);
        assert_eq!(own.iter().map(|a| a.message_id.as_str()).collect::<Vec<_>>(), ["own1", "t2", "own3"]);
        assert_eq!(own[1].subject.as_deref(), Some("own a.rar"), "keeps this release's subject");

        // split differently: its articles wouldn't fit
        let mut own = vec![article("a.rar", 1, 3, 100, "own1")];
        assert_eq!(fill_gaps(&mut own, &[article("a.rar", 1, 3, 90, "t1"), article("a.rar", 2, 3, 90, "t2")]), 0);
        assert_eq!(fill_gaps(&mut own, &[article("a.rar", 2, 4, 100, "t2")]), 0, "another part count");
        assert_eq!(fill_gaps(&mut own, &[article("a.rar", 2, 3, 100, "t2")]), 0, "no part in common");
        // a part both have and both read must hold the same bytes, or the twin's file is another file
        let mut own = vec![article("a.rar", 1, 2, 100, "own1")];
        let twin = vec![article("a.rar", 1, 2, 100, "t1"), article("a.rar", 2, 2, 100, "t2")];
        let known: KnownCrcs = [("own1".to_string(), (90, 1)), ("t1".to_string(), (90, 2))].into_iter().collect();
        assert_eq!(fill_gaps_checked(&mut own, &twin, &known), 0, "other bytes under the same name");
        let known: KnownCrcs = [("own1".to_string(), (90, 1)), ("t1".to_string(), (90, 1))].into_iter().collect();
        assert_eq!(fill_gaps_checked(&mut own, &twin, &known), 1, "the same bytes");
        let known: KnownCrcs = [("t1".to_string(), (90, 2))].into_iter().collect();
        let mut own = vec![article("a.rar", 1, 2, 100, "own1")];
        assert_eq!(fill_gaps_checked(&mut own, &twin, &known), 1, "one side unread says nothing");
    }

    #[test]
    fn a_file_twin_fills_a_file_under_another_name() {
        let mut own = vec![article("abc123.bin", 1, 3, 100, "own1"), article("abc123.bin", 3, 3, 50, "own3")];
        let twin = vec![
            article("Movie.2024.mkv", 1, 3, 100, "t1"),
            article("Movie.2024.mkv", 2, 3, 100, "t2"),
            article("Movie.2024.mkv", 3, 3, 50, "t3"),
            article("Movie.2024.nfo", 1, 1, 10, "n1"),
        ];
        // the sets call it Movie.2024.mkv on both sides; this posting's one data file carries a random name
        let names = vec![("Movie.2024.mkv".to_string(), "Movie.2024.mkv".to_string())];
        assert_eq!(fill_gaps_from(&mut own, &twin, &names, true, &KnownCrcs::new()), 1);
        assert_eq!(own.iter().map(|a| a.message_id.as_str()).collect::<Vec<_>>(), ["own1", "t2", "own3"]);
        assert_eq!(own[1].filename.as_deref(), Some("abc123.bin"), "under this posting's name");
        // a set of several data files says nothing about which one a random name stands for
        let mut own = vec![article("abc123.bin", 1, 3, 100, "own1"), article("abc123.bin", 3, 3, 50, "own3")];
        assert_eq!(fill_gaps_from(&mut own, &twin, &names, false, &KnownCrcs::new()), 0);
        // a name the twin's articles don't carry fills nothing
        let mut own = vec![article("a.rar", 1, 2, 100, "own1")];
        assert_eq!(fill_gaps_from(&mut own, &twin, &[("x.rar".into(), "a.rar".into())], true, &KnownCrcs::new()), 0);
        // two data files here: which is which can't be told by name, soo nothing fills
        let mut own = vec![article("one.bin", 1, 3, 100, "o1"), article("two.bin", 1, 3, 100, "o2")];
        assert_eq!(fill_gaps_from(&mut own, &twin, &names, true, &KnownCrcs::new()), 0);
        // only the file the sets share is read as this release's; the twin's other files aren't, whatever they're called
        let mut own = vec![article("abc123.bin", 1, 3, 100, "own1")];
        let twin = vec![article("abc123.bin", 2, 3, 100, "t2"), article("Movie.2024.nfo", 1, 1, 10, "n1")];
        let nfo = vec![("Movie.2024.nfo".to_string(), "Movie.2024.nfo".to_string())];
        assert_eq!(fill_gaps_from(&mut own, &twin, &nfo, true, &KnownCrcs::new()), 0);
        assert_eq!(own.len(), 1);
    }

    #[test]
    fn a_renamed_repost_fills_from_the_volumes_its_set_names() {
        // the set names the first upload's volumes; this posting renamed every one of them
        let mut own = vec![
            article("oCBvoIlhFvS0L3k1.part001.rar", 1, 2, 100, "o1"),
            article("oCBvoIlhFvS0L3k1.part002.rar", 1, 2, 100, "o3"),
            article("oCBvoIlhFvS0L3k1.part002.rar", 2, 2, 100, "o4"),
            article("oCBvoIlhFvS0L3k1.par2", 1, 1, 10, "op"),
        ];
        let twin = vec![
            article("YMFFZeoeVI6fG4iz.part001.rar", 1, 2, 100, "y1"),
            article("YMFFZeoeVI6fG4iz.part001.rar", 2, 2, 100, "y2"),
            article("YMFFZeoeVI6fG4iz.part002.rar", 1, 2, 100, "y3"),
        ];
        let names: Vec<(String, String)> = ["YMFFZeoeVI6fG4iz.part001.rar", "YMFFZeoeVI6fG4iz.part002.rar"]
            .iter()
            .map(|n| (n.to_string(), n.to_string()))
            .collect();
        assert_eq!(fill_gaps_from(&mut own, &twin, &names, false, &KnownCrcs::new()), 1);
        let borrowed = own.iter().find(|a| a.message_id == "y2").unwrap();
        assert_eq!(borrowed.filename.as_deref(), Some("oCBvoIlhFvS0L3k1.part001.rar"), "under this posting's name");
        // a set of two archives' volumes can't say which posted archive is which
        let two: Vec<(String, String)> =
            ["A.part001.rar", "B.part001.rar"].iter().map(|n| (n.to_string(), n.to_string())).collect();
        let mut own = vec![article("P.part001.rar", 1, 2, 100, "p1")];
        let twin = vec![article("A.part001.rar", 2, 2, 100, "a2"), article("B.part001.rar", 2, 2, 100, "b2")];
        assert_eq!(fill_gaps_from(&mut own, &twin, &two, false, &KnownCrcs::new()), 0);
        // a plain file's ending says nothing about which file it is
        assert_eq!(renamed_volume(&["Other.mkv"], "Movie.mkv"), None);
        // a posting of two archives can't say which one the set's name stands for
        let posted = ["a.part001.rar", "b.part001.rar"];
        assert_eq!(renamed_volume(&posted, "YMFFZeoeVI6fG4iz.part001.rar"), None);
        // a volume with no posted counterpart, and a file that is no volume, map to nothing
        let posted = ["oCBv.part001.rar", "oCBv.par2"];
        assert_eq!(renamed_volume(&posted, "YMFF.part002.rar"), None);
        assert_eq!(renamed_volume(&posted, "YMFF.part001.rar"), Some("oCBv.part001.rar"));
        assert_eq!(renamed_volume(&posted, "YMFF.nfo"), None);
    }

    #[test]
    fn packed_files_and_small_files_say_nothing_about_content() {
        assert_eq!(content(&[("a.part01.rar", GB), ("a.mkv", GB)]), None);
        assert_eq!(content(&[("a.r00", GB)]), None);
        assert_eq!(content(&[("a.001", GB)]), None);
        assert_eq!(content(&[("a.nfo", 2_000)]), None);
        assert_eq!(content(&[]), None);
    }
}
