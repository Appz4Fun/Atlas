//! The rules for linking uploads obfuscated per article back together across groups, shared by the header store's
//! linker (`nxv_store::linker`), which reads the rows and keeps the state.
//!
//! Some posters give every article its own random subject, with no part counter and no file name, soo no subject rule
//! reads them (`parser::Unparsable`). What stays the same is the poster: every article of one file carries the same
//! random From, in every group the file was spread over. And the files of one upload go up back to back to the same
//! groups, a one article PAR2 index first. The linker groups those articles into files by poster and the files into
//! uploads by time and groups (`chain`), and saves each upload as one release (`add_file`).
//!
//! Nothing here reads a body. Which article starts a file isn't in its headers, soo a file's parts are numbered in the
//! order they were posted, and a downloader puts each where its yEnc header says.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use chrono::DateTime;

use crate::parser::{Article, Release, Shape, Unparsable, is_complete, parse_subject};

/// the window an upload's files are linked in: how long a file's place and an upload stay open to articles that come
/// later, and how long the linker waits on an hour whose day isn't read in full
pub const KEEP_SECS: i64 = 24 * 3600;
/// how long a live hour waits, past the two hours an upload takes, before the linker takes it as read in full
pub const SETTLE_SECS: i64 = 15 * 60;
/// the most seconds between one file of an upload and the next
pub const GAP_SECS: i64 = 5;
/// an upload of fewer articles is noise: a random poster for each article makes uploads of one
pub const MIN_UPLOAD_ARTICLES: i64 = 10;

/// what the name of every upload saved here starts with (`is_upload`)
pub const NAME_PREFIX: &str = "xref-upload-";
/// a volume's articles the archive step tries as its first piece: the first a group got of it, in each group. Piece
/// order isn't in the headers, but the first piece goes up first, soo it is most likely the lowest numbered in its group
pub const LEADS: usize = 6;
/// the most first-piece guesses the archive step makes for one upload, over its volumes in turn
pub const HEAD_PROBES: usize = 40;

/// `atlas.xref.db` beside the main database: where an older build kept these headers in SQLite. Nothing opens it now;
/// a shard reset removes it.
pub fn path_for(main: &Path) -> PathBuf {
    main.with_file_name("atlas.xref.db")
}

/// What a poster's articles waiting in the store add up to: one file of an upload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSum {
    pub poster: String,
    /// its articles
    pub n: i64,
    /// when its first and last article were posted
    pub first: i64,
    pub last: i64,
    /// when its newest article arrived
    pub added: i64,
    pub groups: BTreeSet<i64>,
}

/// An upload saved before, that files read since in other groups can join.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seed {
    /// the groups its articles were read in
    pub groups: BTreeSet<i64>,
    pub first: i64,
    pub last: i64,
}

struct Upload {
    /// the groups its files go to; None while it has only its PAR2 index
    groups: Option<BTreeSet<i64>>,
    /// the one article file it started with, whose groups are some of the upload's
    index_groups: BTreeSet<i64>,
    /// the saved upload it goes on, and the groups that had been read for it then
    seed: Option<(usize, BTreeSet<i64>)>,
    files: Vec<usize>,
    first: i64,
    last: i64,
}

/// The files of `files` that make one upload each, in the order they went up, and the upload of `seeds` each goes on.
///
/// A one article file is a PAR2 index and starts an upload; it sorts before the files posted in the same second. Any
/// other file goes to an upload it started within, from `GAP_SECS` before its first file to as long after its last:
/// first one posted to the same groups (or one only an index has started, whose groups it covers), the most recently
/// active; else, as a small file can't reach every group its upload went to, one whose groups take in its own, the
/// fewest of them first; else a saved upload none of its groups had been read for, the closest: groups are read apart,
/// and a file read in one only after its upload was saved from another shares none with it.
pub fn chain(files: &[FileSum], seeds: &[Seed]) -> Vec<(Option<usize>, Vec<usize>)> {
    chain_until(files, seeds, &|| false).expect("never stopped")
}

/// `chain`, None once `stop` says so (asked every few thousand files).
fn chain_until(files: &[FileSum], seeds: &[Seed], stop: &dyn Fn() -> bool) -> Option<Vec<(Option<usize>, Vec<usize>)>> {
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by(|&a, &b| {
        let (x, y) = (&files[a], &files[b]);
        (x.first, x.n != 1, x.last, &x.poster).cmp(&(y.first, y.n != 1, y.last, &y.poster))
    });

    let mut uploads: Vec<Upload> = seeds
        .iter()
        .enumerate()
        .map(|(j, s)| Upload {
            groups: Some(s.groups.clone()),
            index_groups: BTreeSet::new(),
            seed: Some((j, s.groups.clone())),
            files: Vec::new(),
            first: s.first,
            last: s.last,
        })
        .collect();
    // the uploads a file can still go to, soo a pass over a day of headers isn't quadratic: seeds come in as their
    // time comes, and an upload leaves once files start past its last
    let mut by_start: Vec<usize> = (0..uploads.len()).collect();
    by_start.sort_by_key(|&j| std::cmp::Reverse(uploads[j].first));
    let mut active: Vec<usize> = Vec::new();
    for (k, i) in order.into_iter().enumerate() {
        if k % 4096 == 0 && stop() {
            return None;
        }
        let f = &files[i];
        while by_start.last().is_some_and(|&j| uploads[j].first - GAP_SECS <= f.first) {
            active.extend(by_start.pop());
        }
        active.retain(|&j| uploads[j].last + GAP_SECS >= f.first);
        let near = |u: &Upload| f.first >= u.first - GAP_SECS && f.first <= u.last + GAP_SECS;
        let pick = if f.n == 1 {
            None
        } else {
            let candidates = || active.iter().map(|&j| (j, &uploads[j])).filter(|(_, u)| near(u));
            let same = candidates()
                .filter(|(_, u)| match &u.groups {
                    Some(g) => *g == f.groups,
                    None => u.index_groups.is_subset(&f.groups),
                })
                // the most recently active, and an upload only an index has started over one it would follow on
                .max_by_key(|(_, u)| (u.last, u.groups.is_none()))
                .map(|(j, _)| j);
            let within = || {
                candidates()
                    .filter_map(|(j, u)| {
                        u.groups.as_ref().filter(|g| f.groups.is_subset(g)).map(|g| (j, g.len(), u.last))
                    })
                    .min_by_key(|&(_, len, last)| (len, std::cmp::Reverse(last)))
                    .map(|(j, _, _)| j)
            };
            let read_since = || {
                candidates()
                    .filter(|(_, u)| u.seed.as_ref().is_some_and(|(_, read)| read.is_disjoint(&f.groups)))
                    .min_by_key(|(_, u)| (f.first - u.last).abs())
                    .map(|(j, _)| j)
            };
            same.or_else(within).or_else(read_since)
        };
        match pick {
            Some(j) => {
                let u = &mut uploads[j];
                match &mut u.groups {
                    None => u.groups = Some(f.groups.clone()),
                    Some(g) if u.seed.is_some() => g.extend(f.groups.iter().copied()),
                    Some(_) => {}
                }
                u.files.push(i);
                u.first = u.first.min(f.first);
                u.last = u.last.max(f.last);
            }
            None => {
                active.push(uploads.len());
                uploads.push(Upload {
                    groups: (f.n > 1).then(|| f.groups.clone()),
                    index_groups: f.groups.clone(),
                    seed: None,
                    files: vec![i],
                    first: f.first,
                    last: f.last,
                });
            }
        }
    }
    Some(uploads.into_iter().filter(|u| !u.files.is_empty()).map(|u| (u.seed.map(|(j, _)| j), u.files)).collect())
}

/// Stable, in decimal: a run of hex would read as a random name, and leave search (as `filters::per_article_name`).
pub(crate) fn upload_name(poster: &str) -> String {
    let key = format!("xref\0{poster}");
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3));
    format!("{NAME_PREFIX}{}-{}", hash >> 32, hash & 0xffff_ffff)
}

/// How many of the upload `release`'s first-piece guesses the archive step has tried, in the order
/// `archive::steps::xref_order` gives them, and the generation of its guesses that count is of (the header store's
/// linker keeps them). None when not known (pruned, the linker not running, or in tests): the first ones, once.
pub fn tried(release: &str) -> Option<(i64, i64)> {
    if cfg!(test) {
        return None;
    }
    crate::nxv_store::linker::tried(release).flatten()
}

/// The first `count` guesses of generation `generation` of the upload `release` were tried: a later run takes the ones
/// after. Nothing when the upload has grown since, and its guesses started over.
pub fn tried_through(release: &str, generation: i64, count: i64) {
    if !cfg!(test) {
        crate::nxv_store::linker::tried_through(release, generation, count);
    }
}

/// The parts of file `file` of the upload `release` that were the first a group got of it, in the order they were
/// saved: the likely first pieces. Empty when not known (pruned, the linker not running, or in tests).
pub fn leads(release: &str, file: i64) -> Vec<i64> {
    if cfg!(test) {
        return Vec::new();
    }
    crate::nxv_store::linker::leads(release, file).unwrap_or_default()
}

/// The place of a file named for it (`add_file`): `<release>-<file>`.
pub fn file_of(filename: &str) -> Option<i64> {
    filename.rsplit_once('-')?.1.parse().ok()
}

/// Whether `name` is that of an upload this linked, whose files are named for their place and whose parts are in a
/// guessed order.
pub fn is_upload(name: &str) -> bool {
    name.starts_with(NAME_PREFIX)
}

/// `YYYY-MM-DD HH:MM:SS` of a wall clock in seconds, what `dates::to_iso_date` gives
fn iso(posted: i64) -> String {
    DateTime::from_timestamp(posted, 0).map_or_else(String::new, |d| d.format("%Y-%m-%d %H:%M:%S").to_string())
}

/// `iso`, for tools outside this module.
pub fn iso_pub(posted: i64) -> String {
    iso(posted)
}

/// One article as the store keeps it (and as the day-segment store's linker reads it, `nxv_store::linker`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) message_id: String,
    pub(crate) added: i64,
    /// the first its group got of the file, of the rows taken together
    pub(crate) lead: bool,
    pub(crate) posted: i64,
    pub(crate) grp: i64,
    pub(crate) number: i64,
    pub(crate) bytes: i64,
    pub(crate) lines: i64,
}

/// Where a file goes: its release, that release's group, its place among the upload's files, and the parts it has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Place {
    /// the parts that were a group's first, comma separated (`leads`)
    pub(crate) leads: String,
    /// when the oldest of its saved articles came
    pub(crate) oldest: i64,
    pub(crate) release: String,
    pub(crate) group: String,
    pub(crate) file: i64,
    pub(crate) files: i64,
    pub(crate) parts: i64,
}

/// `rows` of one poster as a file of `place`'s release, parts numbered on from the ones it has, of `parts` in all.
/// The parts that were a group's first of them. Shared with the day-segment store's linker (`nxv_store::linker`).
pub(crate) fn add_file(
    out: &mut HashMap<String, Release>,
    place: &Place,
    poster: &str,
    rows: Vec<Row>,
    parts: i64,
) -> Vec<i64> {
    let mut leads = Vec::new();
    let key = format!("{}\0{}", place.release, place.group);
    let release = out.entry(key).or_insert_with(|| Release {
        name: place.release.clone(),
        is_obfuscated: true,
        group: place.group.clone(),
        collection: Some(format!("xref\0{}", place.release)),
        ..Release::default()
    });
    // no count of files in it: files read later in other groups join the upload
    let filename = format!("{}-{:05}", place.release, place.file);
    for (k, row) in rows.into_iter().enumerate() {
        let part = place.parts + k as i64 + 1;
        if row.lead {
            leads.push(part);
        }
        release.size += row.bytes.max(0);
        release.articles.push(Article {
            number: row.number.max(0) as u64,
            subject: format!("[{}/{}] - \"{filename}\" yEnc ({part}/{parts})", place.file, place.files),
            author: poster.to_string(),
            date: iso(row.posted),
            message_id: row.message_id,
            bytes: row.bytes,
            lines: row.lines,
            filename: Some(filename.clone()),
            release_name: Some(place.release.clone()),
            part: Some(part),
            total_parts: Some(parts),
            file_index: Some(place.file),
            file_total: Some(place.files),
            ..Article::default()
        });
    }
    leads
}

/// The group an upload's release lives in: the one most of its articles were posted to (the lowest id on a tie).
pub(crate) fn home_group(by_group: &HashMap<i64, usize>) -> i64 {
    by_group.iter().max_by_key(|&(g, n)| (*n, std::cmp::Reverse(*g))).map_or(0, |(g, _)| *g)
}

/// A pass's releases, each marked complete or not and named for its first article's poster and date.
pub(crate) fn finish_releases(out: HashMap<String, Release>) -> Vec<Release> {
    let mut releases: Vec<Release> = out.into_values().collect();
    for r in &mut releases {
        r.complete = is_complete(&r.articles);
        if let Some(first) = r.articles.first() {
            r.poster = first.author.clone();
            r.date = first.date.clone();
        }
    }
    releases
}

/// `before`, a comma separated list of parts, with `more` after them.
pub(crate) fn with_leads(before: &str, more: Vec<i64>) -> String {
    before
        .split(',')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .chain(more.iter().map(i64::to_string))
        .collect::<Vec<_>>()
        .join(",")
}

/// Whether `article` is one this keeps: no subject rule reads it, its subject is one random token or its own
/// Message-ID, and it names a poster.
pub fn keeps(article: &Article) -> bool {
    keeps_parts(&article.subject, &article.author, &article.message_id)
}

fn keeps_parts(subject: &str, author: &str, message_id: &str) -> bool {
    !author.is_empty()
        && matches!(crate::parser::shape_of(subject, message_id), Shape::Token | Shape::IdSubject)
        && parse_subject(subject).is_none()
}

/// `keeps` of an overview row's raw subject, poster and message-id, as the indexer would read them into an article
/// (`nntp::overview_of`: subject and poster with mojibake undone), without making the article: plain ASCII, the
/// common case, is checked in place.
pub fn keeps_fields(subject: &[u8], author: &[u8], message_id: &[u8]) -> bool {
    if author.is_empty() {
        return false;
    }
    if subject.is_ascii() && message_id.is_ascii() {
        // ASCII: lossy decoding and undoing mojibake change nothing
        let (Ok(s), Ok(m)) = (std::str::from_utf8(subject), std::str::from_utf8(message_id)) else { return false };
        return keeps_parts(s, "x", m);
    }
    let s = crate::nntp::undo_mojibake(String::from_utf8_lossy(subject).into_owned());
    let m = String::from_utf8_lossy(message_id);
    keeps_parts(&s, "x", &m)
}

/// A slice's articles of `group` without the ones obfuscated per article. Those are counted as unparsable still
/// (`atlas_articles_unparsable_total`), as no subject rule reads them; the header store's linker reads them from the
/// store, where the connection layer recorded the whole listing (`nxv_store::live::record`).
pub fn hold(group: &str, articles: Vec<Article>) -> Vec<Article> {
    let (held, kept): (Vec<Article>, Vec<Article>) = articles.into_iter().partition(keeps);
    if !held.is_empty() {
        let mut unparsable = Unparsable::default();
        for a in &held {
            unparsable.note(a);
        }
        crate::collections::count_unparsable(group, &unparsable);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(poster: &str, n: i64, first: i64, last: i64, groups: &[i64]) -> FileSum {
        FileSum { poster: poster.into(), n, first, last, added: 0, groups: groups.iter().copied().collect() }
    }

    const SIX: [i64; 6] = [1, 2, 3, 4, 5, 6];
    const FOUR: [i64; 4] = [2, 3, 4, 5];

    fn posters(files: &[FileSum], chains: &[(Option<usize>, Vec<usize>)]) -> Vec<Vec<String>> {
        chains.iter().map(|(_, c)| c.iter().map(|&i| files[i].poster.clone()).collect()).collect()
    }

    #[test]
    fn an_upload_is_its_index_then_files_to_the_same_groups_back_to_back() {
        // another uploader's files to four groups end as this one starts, and its PAR2 tail comes in among them
        let files = vec![
            file("other-a", 482, -17, -15, &FOUR),
            file("other-b", 499, -1, 1, &FOUR),
            file("index", 1, 0, 0, &[1]),
            file("rar1", 293, 0, 1, &SIX),
            file("other-par2", 151, 2, 3, &FOUR),
            file("other-tail", 5, 2, 2, &[2, 4, 5]),
            file("rar2", 293, 1, 2, &SIX),
            file("rar3", 293, 2, 3, &SIX),
            file("vol-small", 8, 38, 38, &[1, 2, 3, 6]),
            file("vol-big", 469, 40, 42, &SIX),
            // well after: a third uploader's stray par2
            file("stray", 5, 73, 73, &[2, 4, 5]),
        ];
        let chains = chain(&files, &[]);
        let got = posters(&files, &chains);
        let ours = got.iter().find(|c| c.contains(&"rar1".to_string())).unwrap();
        // vol-small is 35s after rar3: past the gap, it starts an upload of its own here, soo the test spaces them
        assert_eq!(ours, &vec!["index", "rar1", "rar2", "rar3"]);
        let other = got.iter().find(|c| c.contains(&"other-b".to_string())).unwrap();
        assert!(other.contains(&"other-tail".to_string()) && other.contains(&"other-par2".to_string()), "{got:?}");
        assert!(got.iter().any(|c| c == &vec!["stray".to_string()]), "{got:?}");
    }

    #[test]
    fn the_par2_tail_follows_the_last_volume_within_the_gap() {
        let files = vec![
            file("index", 1, 0, 0, &[3]),
            file("rar1", 293, 0, 1, &SIX),
            file("rar2", 293, 1, 2, &SIX),
            file("vol8", 8, 2, 2, &[1, 2, 3, 6]),
            file("vol15", 15, 2, 3, &SIX),
            file("vol469", 469, 3, 5, &SIX),
        ];
        let got = posters(&files, &chain(&files, &[]));
        assert_eq!(got, vec![vec!["index", "rar1", "rar2", "vol8", "vol15", "vol469"]]);
    }

    #[test]
    fn the_next_index_to_the_same_groups_starts_the_next_upload() {
        let files = vec![
            file("a-index", 1, 0, 0, &[1]),
            file("a-rar1", 74, 0, 1, &SIX),
            file("a-vol", 59, 1, 2, &SIX),
            file("b-index", 1, 3, 3, &[2]),
            file("b-rar1", 74, 3, 4, &SIX),
            file("b-rar2", 74, 4, 5, &SIX),
        ];
        let got = posters(&files, &chain(&files, &[]));
        assert_eq!(got, vec![vec!["a-index", "a-rar1", "a-vol"], vec!["b-index", "b-rar1", "b-rar2"]]);
    }

    fn article(mid: &str, poster: &str, date: &str, number: u64) -> Article {
        Article {
            number,
            subject: format!("{mid}x{}", "Q".repeat(40)),
            author: poster.into(),
            date: date.into(),
            message_id: format!("<{mid}@h.x>"),
            bytes: 740_000,
            lines: 5692,
            ..Article::default()
        }
    }

    #[test]
    fn keeps_fields_agrees_with_keeps_on_overview_lines() {
        let rows: Vec<Vec<u8>> = vec![
            b"1\tabc123def456ghi789\tp@x\tSun, 08 Nov 2026 10:00:00 +0000\t<abc123def456ghi789@ngPost>\t\t1\t1"
                .to_vec(),
            b"2\tAbcdEfghIjklMnopQrst\tp@x\tSun, 08 Nov 2026 10:00:00 +0000\t<x@y>\t\t1\t1".to_vec(),
            b"3\tAbcdEfghIjklMno\tp@x\tSun, 08 Nov 2026 10:00:00 +0000\t<x@y>\t\t1\t1".to_vec(),
            b"4\tabcd.efgh.ijkl.mnop\tp@x\tSun, 08 Nov 2026 10:00:00 +0000\t<x@y>\t\t1\t1".to_vec(),
            b"5\t  AbcdEfghIjklMnopQrst  \tp@x\tSun, 08 Nov 2026 10:00:00 +0000\t<x@y>\t\t1\t1".to_vec(),
            b"6\tlocal[1]part\tp@x\tSun, 08 Nov 2026 10:00:00 +0000\t<local[1]part@h>\t\t1\t1".to_vec(),
            b"7\tAbcdEfghIjklMnopQrst\t\tSun, 08 Nov 2026 10:00:00 +0000\t<x@y>\t\t1\t1".to_vec(),
            "8\tAbcdEfgh\u{e9}IjklMnopQrst\tp@x\tSun, 08 Nov 2026 10:00:00 +0000\t<x@y>\t\t1\t1".as_bytes().to_vec(),
            "9\tl\u{c3}\u{a9}cal\tp@x\tSun, 08 Nov 2026 10:00:00 +0000\t<l\u{e9}cal@y>\t\t1\t1".as_bytes().to_vec(),
            b"10\t[1/9] - \"x.part01.rar\" yEnc (1/50)\tp@x\tSun, 08 Nov 2026 10:00:00 +0000\t<x@y>\t\t1\t1".to_vec(),
            b"11\tlocal\tp@x\tSun, 08 Nov 2026 10:00:00 +0000\t<<local@a@b>>\t\t1\t1".to_vec(),
            b"12\t\xff\xfeAbcdEfghIjklMnopQrst\tp@x\tSun, 08 Nov 2026 10:00:00 +0000\t<x@y>\t\t1\t1".to_vec(),
        ];
        let mut kept = 0;
        for l in &rows {
            let a = crate::nntp::overview_of(l).unwrap().into_article();
            let f: Vec<&[u8]> = l.split(|&b| b == b'\t').collect();
            assert_eq!(keeps_fields(f[1], f[2], f[4]), keeps(&a), "{}", String::from_utf8_lossy(l));
            kept += keeps(&a) as usize;
        }
        assert!(kept >= 4, "the sample has kept rows: {kept}");
    }

    #[test]
    fn only_subjects_no_rule_reads_are_kept() {
        let token = article("aaaaaaaaaaaaaaaaaaaaaaaa", "p@q.r", "2023-12-03 16:23:23", 1);
        assert!(keeps(&token));
        let mut counted = token.clone();
        counted.subject = format!("{} (1/50)", counted.subject);
        assert!(!keeps(&counted), "a part counter is read by `gather_per_article`");
        let mut no_poster = token.clone();
        no_poster.author.clear();
        assert!(!keeps(&no_poster));
        let mut id = token.clone();
        id.subject = "aaaaaaaaaaaaaaaaaaaaaaaa".into();
        assert!(keeps(&id), "ngPost -x");
    }

    #[test]
    fn leads_lists_join_on() {
        assert_eq!(file_of("xref-upload-1-2-00007"), Some(7));
        assert_eq!(with_leads("1,2", vec![9]), "1,2,9");
        assert_eq!(with_leads("", vec![1, 3]), "1,3");
    }
}
