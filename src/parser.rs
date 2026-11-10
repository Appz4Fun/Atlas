use std::collections::{BTreeSet, HashMap};
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;

/// most parts a file can claim. a subject claiming more is forged: the part
/// bitmap grows with the part number, so it would cost storage per header
pub const MAX_PARTS: i64 = 250_000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Article {
    pub number: u64,
    pub subject: String,
    pub author: String,
    pub date: String,
    pub message_id: String,
    pub references: String,
    pub bytes: i64,
    pub lines: i64,
    /// the groups the server's Xref names, when its overview carries the line (`Xref:full`); None elsewhere
    pub xref_groups: Option<u16>,
    pub filename: Option<String>,
    pub release_name: Option<String>,
    pub part: Option<i64>,
    pub total_parts: Option<i64>,
    pub file_index: Option<i64>,
    pub file_total: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSubject {
    pub filename: String,
    pub release_name: String,
    pub part: i64,
    pub total_parts: i64,
    pub file_index: Option<i64>,
    pub file_total: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct Release {
    pub name: String,
    pub articles: Vec<Article>,
    pub size: i64,
    pub is_obfuscated: bool,
    pub display_name: Option<String>,
    pub complete: bool,
    pub group: String,
    pub poster: String,
    pub date: String,
    /// the post it belongs to when that says more than its name (`collections.rs`)
    pub collection: Option<String>,
}

// every client posts different subjects soo try all these patterns, in order
static SUBJECT_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r#""(.+?)"\s+yEnc\s+\((\d+)\s*/\s*(\d+)\)"#,
        r#"\[\d+\s*/\s*\d+\]\s+"(.+?)"\s+yEnc\s+\((\d+)\s*/\s*(\d+)\)"#,
        r#"\[\d+\s*/\s*\d+\]\s+-\s+"(.+?)"\s+-\s+[\d.]+\s+\w+\s+yEnc\s+\((\d+)\s*/\s*(\d+)\)"#,
        r#"yEnc\s+"(.+?)"\s+[\d.]+\s+\w+\s+\((\d+)\s*/\s*(\d+)\)"#,
        r#""(.+?)"\s+\((\d+)\s*/\s*(\d+)\)"#,
        r#"([^\s"]+\.[^\s"]+)\s+yEnc\s+\((\d+)\s*/\s*(\d+)\)"#,
        r#"([^\s"]+\.[^\s"]+)\s+\((\d+)\s*/\s*(\d+)\)"#,
        r#"\[\d+\s*/\s*\d+\]\s+(?:-\s+)?([^\s"]+\.[^\s"]+)\s+yEnc"#,
        r#"\[\d+\s*/\s*\d+\]\s+(?:-\s+)?([^\s"]+\.[^\s"]+)\s+\("#,
        // python had a (?=\s|$) lookahead here, consuming it is equivalent for a search
        r#"\[\d+\s*/\s*\d+\]\s+(?:-\s+)?["]([^"]+)["](?:\s|$)"#,
        // `"name" - 26,35 GB - yEnc (p/t)`: a size with a decimal comma, and a dash before yEnc
        r#""(.+?)"\s+-\s+[\d.,]+\s*\w+\s+-\s+yEnc\s+\((\d+)\s*/\s*(\d+)\)"#,
    ]
    .iter()
    .map(|p| Regex::new(&format!("(?i){p}")).unwrap())
    .collect()
});

/// `<name> [p/t]` and nothing else: one file of t parts under a random name, the bracket counting its parts
static BRACKET_PARTS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z0-9][A-Za-z0-9._-]{15,})\s+\[(\d+)\s*/\s*(\d+)\]$").unwrap());

static FILE_COUNT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[(\d+)\s*/\s*(\d+)\]").unwrap());

static RELEASE_SUFFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\.(part\d*|r\d+|z\d{2,3}|vol\d+[+-]\d+|par[23]|nfo|sfv|srr|srs|nzb|rar|7z\.\d{2,3}|(?:rar|zip)\.\d{2,3}|0\d\d|iby\d+[+-]\d+|cne[27]|cneg\d*|ene)$",
    )
    .unwrap()
});

pub fn strip_release_suffixes(name: &str) -> String {
    let mut release_name = name.to_string();
    loop {
        let stripped = RELEASE_SUFFIX_RE.replace(&release_name, "").into_owned();
        if stripped == release_name || stripped.is_empty() {
            break;
        }
        release_name = stripped;
    }
    release_name
}

pub fn is_obfuscated(name: &str) -> bool {
    crate::naming::obfuscation::is_obfuscated(name)
}

/// Whether a subject carries a 0-indexed `[0/N]` or `[00/N]` file counter.
pub fn has_zero_file_index(subject: &str) -> bool {
    FILE_COUNT_RE.captures(subject.trim()).and_then(|c| c[1].parse::<i64>().ok()).is_some_and(|idx| idx == 0)
}

pub fn parse_subject(subject: &str) -> Option<ParsedSubject> {
    // every pattern needs a `(` or a `[`: obfuscated subjects (a bare hash,
    // `{...}`), most of some groups, skip all ten regexes
    if !subject.contains(['(', '[']) {
        return None;
    }

    if let Some(c) = BRACKET_PARTS_RE.captures(subject.trim()) {
        let (part, total): (i64, i64) = (c[2].parse().ok()?, c[3].parse().ok()?);
        if part < 1 || total < 2 || part > total || total > MAX_PARTS {
            return None;
        }
        let filename = c[1].to_string();
        return Some(ParsedSubject {
            release_name: strip_release_suffixes(&filename),
            filename,
            part,
            total_parts: total,
            file_index: None,
            file_total: None,
        });
    }

    let caps = SUBJECT_PATTERNS.iter().find_map(|re| re.captures(subject))?;
    let filename = caps.get(1)?.as_str().to_string();

    // strip .part1/.r00 type shi soo every part of a release maps to one name
    let mut release_name = strip_release_suffixes(&filename);

    // some formats only give a filename, no part numbers
    let (part, total_parts) = match (caps.get(2), caps.get(3)) {
        (Some(p), Some(t)) => (p.as_str().parse().ok()?, t.as_str().parse().ok()?),
        _ => (1, 1),
    };
    // a part past the file's own total, or past any real file, is forged
    if part > MAX_PARTS || total_parts > MAX_PARTS || (total_parts != 0 && part > total_parts) {
        return None;
    }

    let (mut file_index, mut file_total) = (None, None);

    if let Some(c) = FILE_COUNT_RE.captures(subject.trim()) {
        file_index = c[1].parse().ok();
        file_total = c[2].parse().ok();
    }

    if let Some(total) = file_total.filter(|t| *t != 0)
        && release_name.chars().count() <= 2
    {
        release_name = format!("{release_name} [{total}]");
    }

    Some(ParsedSubject { filename, release_name, part, total_parts, file_index, file_total })
}

/// Posts where every file is a single article with only a `[n/m]` counter
/// are really one file split across articles. Turn the counter into part numbers.
fn remap_bracket_parts(articles: &mut [Article]) {
    let mut by_filename: IndexMap<Option<String>, Vec<usize>> = IndexMap::new();

    for (i, a) in articles.iter().enumerate() {
        by_filename.entry(a.filename.clone()).or_default().push(i);
    }

    for idxs in by_filename.values() {
        if idxs.len() < 2 {
            continue;
        }

        if idxs.iter().any(|&i| articles[i].total_parts != Some(1)) {
            continue;
        }

        if idxs.iter().any(|&i| articles[i].file_index.is_none()) {
            continue;
        }

        // a bracket counter past any real file is forged, not a part number
        if idxs.iter().any(|&i| articles[i].file_index.is_some_and(|n| n > MAX_PARTS)) {
            continue;
        }

        let total = idxs
            .iter()
            .map(|&i| articles[i].file_total.unwrap_or(0))
            .chain(std::iter::once(idxs.len() as i64))
            .max()
            .unwrap_or(0);
        if total > MAX_PARTS {
            continue;
        }

        for &i in idxs {
            let a = &mut articles[i];
            a.part = a.file_index;
            a.total_parts = Some(total);
            a.file_index = None;
            a.file_total = None;
        }
    }
}

/// Bucket articles into releases by their parsed release name, keeping
/// first-seen order. What was there before collections, and what they fall back on.
pub fn group_articles(articles: Vec<Article>) -> IndexMap<String, Release> {
    group_articles_by(articles, |_, _| None, &mut Unparsable::default())
}

/// The articles of a slice no subject rule reads, by what their subject looks like, with their bytes: most are
/// uploads obfuscated per article, which nothing links yet (see `docs/research/posting-behavior`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unparsable {
    /// ngPost `-x`: the subject is the Message-ID's local part
    pub id_subject: (u64, i64),
    /// one random token of 16+ letters and digits, and nothing else
    pub token: (u64, i64),
    pub other: (u64, i64),
}

/// What the subject of an article no subject rule reads looks like (`Unparsable`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// ngPost `-x`: the subject is the Message-ID's local part
    IdSubject,
    /// one random token of 16+ letters and digits, and nothing else
    Token,
    Other,
}

/// The shape of `article`'s subject, which `parse_subject` doesn't read.
pub fn unparsable_shape(article: &Article) -> Shape {
    shape_of(&article.subject, &article.message_id)
}

/// `unparsable_shape` of a subject and message-id as an article holds them.
pub fn shape_of(subject: &str, message_id: &str) -> Shape {
    let subject = subject.trim();
    let local = message_id.trim_matches(|c| c == '<' || c == '>').split('@').next().unwrap_or("");
    if !subject.is_empty() && subject == local {
        Shape::IdSubject
    } else if subject.len() >= 16 && subject.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        Shape::Token
    } else {
        Shape::Other
    }
}

impl Unparsable {
    pub fn note(&mut self, article: &Article) {
        let slot = match unparsable_shape(article) {
            Shape::IdSubject => &mut self.id_subject,
            Shape::Token => &mut self.token,
            Shape::Other => &mut self.other,
        };
        slot.0 += 1;
        slot.1 += article.bytes.max(0);
    }
}

/// Bucket articles into releases by what `collection_of` says, or their parsed
/// release name when it says nothing, keeping first-seen order.
pub fn group_articles_by(
    articles: Vec<Article>,
    mut collection_of: impl FnMut(&Article, &ParsedSubject) -> Option<String>,
    unparsable: &mut Unparsable,
) -> IndexMap<String, Release> {
    let total = articles.len();
    let mut releases: IndexMap<String, Release> = IndexMap::new();
    let mut dropped = 0;

    for mut article in articles {
        let Some(parsed) = parse_subject(&article.subject) else {
            dropped += 1;
            unparsable.note(&article);
            continue;
        };

        let collection = collection_of(&article, &parsed);
        let key = collection.clone().unwrap_or_else(|| parsed.release_name.clone());
        let release = releases.entry(key).or_insert_with(|| Release {
            name: parsed.release_name.clone(),
            is_obfuscated: is_obfuscated(&parsed.release_name),
            collection,
            ..Default::default()
        });

        // a collection goes by its shortest name in the slice: `T` over `T.mkv` or `T.sample.mkv`
        if release.collection.is_some() && parsed.release_name.len() < release.name.len() {
            release.is_obfuscated = is_obfuscated(&parsed.release_name);
            release.name = parsed.release_name.clone();
        }

        // stash the parsed bits on the article soo the nzb builder can use em later
        article.filename = Some(parsed.filename);
        article.release_name = Some(parsed.release_name);
        article.part = Some(parsed.part);
        article.total_parts = Some(parsed.total_parts);
        article.file_index = parsed.file_index;
        article.file_total = parsed.file_total;

        release.size += article.bytes;
        release.articles.push(article);
    }

    for release in releases.values_mut() {
        remap_bracket_parts(&mut release.articles);
    }

    if dropped > 0 {
        println!("Dropped {dropped}/{total} unparsable");
    }

    releases
}

pub fn is_complete(articles: &[Article]) -> bool {
    if articles.is_empty() {
        return false;
    }

    // split parts per file soo each file is checked on its own
    let mut by_filename: HashMap<Option<&str>, Vec<&Article>> = HashMap::new();
    for a in articles {
        by_filename.entry(a.filename.as_deref()).or_default().push(a);
    }

    for file_articles in by_filename.values() {
        let Some(expected) = file_articles.iter().filter_map(|a| a.total_parts).max() else {
            return false;
        };

        // python compared {a.part} (which can hold None) to range(1, n+1)
        let zero_based = file_articles.iter().any(|a| a.part == Some(0));
        let mut parts: BTreeSet<Option<i64>> = BTreeSet::new();
        parts.extend(file_articles.iter().map(|a| a.part));
        let wanted: BTreeSet<Option<i64>> =
            if zero_based { (0..expected).map(Some).collect() } else { (1..=expected).map(Some).collect() };

        if parts != wanted {
            return false;
        }
    }

    let file_totals: BTreeSet<i64> = articles.iter().filter_map(|a| a.file_total).collect();

    if file_totals.len() == 1 {
        let mut expected_files = *file_totals.iter().next().unwrap();
        if articles.iter().any(|a| a.file_index == Some(0) || has_zero_file_index(&a.subject)) {
            expected_files += 1;
        }
        if by_filename.len() as i64 != expected_files {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn art(subject: &str, bytes: i64) -> Article {
        Article { subject: subject.into(), bytes, message_id: format!("<{subject}>"), ..Default::default() }
    }

    #[test]
    fn parses_quoted_yenc() {
        let p = parse_subject(r#"[01/10] - "Movie.2024.1080p.part01.rar" yEnc (1/50)"#).unwrap();
        assert_eq!(p.filename, "Movie.2024.1080p.part01.rar");
        assert_eq!(p.release_name, "Movie.2024.1080p");
        assert_eq!((p.part, p.total_parts), (1, 50));
        assert_eq!((p.file_index, p.file_total), (Some(1), Some(10)));
    }

    #[test]
    fn forged_part_numbers_are_unparsable() {
        assert!(parse_subject(r#""a.rar" yEnc (1000000/1)"#).is_none());
        assert!(parse_subject(r#""a.rar" yEnc (2/1)"#).is_none());
        assert!(parse_subject(r#""a.rar" yEnc (1/1000000)"#).is_none());
        assert!(parse_subject(r#""a.rar" yEnc (1/1)"#).is_some());
    }

    #[test]
    fn forged_headers_dont_grow_seen_blobs() {
        let mut f = crate::store::FileState::default();
        for i in 0..1000 {
            let subject = format!(r#""a.rar" yEnc (1000000/1) {i}"#);
            if let Some(p) = parse_subject(&subject) {
                f.add(&Article { part: Some(p.part), total_parts: Some(p.total_parts), ..Default::default() });
            }
        }
        assert!(f.seen.is_empty());
    }

    #[test]
    fn strips_stacked_suffixes() {
        let p = parse_subject(r#""show.s01e01.vol03+04.par2" yEnc (2/3)"#).unwrap();
        assert_eq!(p.release_name, "show.s01e01");
        let hyphen = parse_subject(
            r#"[03/10] - "One.From.The.Heart.1981.2160p.UHD.BluRay.DV.REMUX.HDR10.HEVC.DTS-HD-MA.5.1-UnKn0wn.vol001-003.par2" yEnc (1/9)"#,
        )
        .unwrap();
        assert_eq!(
            hyphen.release_name,
            "One.From.The.Heart.1981.2160p.UHD.BluRay.DV.REMUX.HDR10.HEVC.DTS-HD-MA.5.1-UnKn0wn"
        );
        let bare_part = parse_subject(r#""Movie.1996.DivX.part.vol000+001.PAR2" yEnc (1/2)"#).unwrap();
        assert_eq!(bare_part.release_name, "Movie.1996.DivX");
        let nzb_part = parse_subject(r#""Studio.Title.part.nzb" yEnc (1/3)"#).unwrap();
        assert_eq!(nzb_part.release_name, "Studio.Title");
        let par3 = parse_subject(r#""[YE] Bleach - 199 (D07CB813).vol01+02.par3" yEnc (1/1)"#).unwrap();
        assert_eq!(par3.release_name, "[YE] Bleach - 199 (D07CB813)");
        let split_7z = parse_subject(r#"[145/155] "579579595695695695695795.7z.145" (1/140)"#).unwrap();
        assert_eq!(split_7z.release_name, "579579595695695695695795");
        let rot13 = parse_subject(r#"(zvfgre-z) [58/81] - "tbtbahgf.cneg7.ene" lRap (1/10)"#).unwrap();
        assert_eq!(rot13.release_name, "tbtbahgf");
        let rot13_par = parse_subject(r#"(zvfgre-z) [75/81] - "tbtbahgf.iby00+01.cne7" lRap (1/2)"#).unwrap();
        assert_eq!(rot13_par.release_name, "tbtbahgf");
    }

    #[test]
    fn bare_filename_without_parts() {
        let p = parse_subject("[3/7] - file.name.mkv yEnc").unwrap();
        assert_eq!(p.filename, "file.name.mkv");
        assert_eq!((p.part, p.total_parts), (1, 1));
        assert_eq!((p.file_index, p.file_total), (Some(3), Some(7)));
    }

    #[test]
    fn last_pattern_quoted_bracket() {
        let p = parse_subject(r#"[1/2] "abc def""#).unwrap();
        assert_eq!(p.filename, "abc def");
        assert!(parse_subject(r#"[1/2] "abc"def"#).is_none());
    }

    #[test]
    fn short_names_get_file_total() {
        let p = parse_subject(r#"[1/4] "a.rar" yEnc (1/1)"#).unwrap();
        assert_eq!(p.release_name, "a [4]");
    }

    #[test]
    fn unparsable_subjects_are_told_by_their_shape() {
        let a = |subject: &str, id: &str| Article {
            subject: subject.into(),
            message_id: id.into(),
            bytes: 740_000,
            ..Default::default()
        };
        let mut u = Unparsable::default();
        u.note(&a("22f3651d2b664749bcac60fc56a9dd1f", "<22f3651d2b664749bcac60fc56a9dd1f@ngPost>"));
        u.note(&a("auQaSojX0x9zXS0sp4fFntDBfD", "<xGeu35HP6VZvsBVKPualFQjF@ejJwrvMhTwXI.1at>"));
        u.note(&a("hello there", "<x@y>"));
        assert_eq!(u.id_subject, (1, 740_000));
        assert_eq!(u.token, (1, 740_000));
        assert_eq!(u.other, (1, 740_000));
    }

    #[test]
    fn a_random_name_with_a_bracket_counting_its_parts() {
        let p = parse_subject("nPqvAL8CUApDXfZwfDu53AHf [515/3054]").unwrap();
        assert_eq!(
            (p.filename.as_str(), p.part, p.total_parts, p.file_index),
            ("nPqvAL8CUApDXfZwfDu53AHf", 515, 3054, None)
        );
        assert!(parse_subject("short [1/2]").is_none(), "a short name can be a file counter of anything");
        assert!(parse_subject("nPqvAL8CUApDXfZwfDu53AHf [9/3]").is_none());
        assert!(parse_subject("nPqvAL8CUApDXfZwfDu53AHf [1/1]").is_none());
    }

    #[test]
    fn a_size_with_a_decimal_comma_before_yenc() {
        let p = parse_subject(r#"(49/57)  "zwPUQNgU9KW7SKN3.vol062+62.par2" - 26,35 GB - yEnc (543/1273)"#).unwrap();
        assert_eq!((p.filename.as_str(), p.part, p.total_parts), ("zwPUQNgU9KW7SKN3.vol062+62.par2", 543, 1273));
        assert_eq!(p.release_name, "zwPUQNgU9KW7SKN3");
    }

    #[test]
    fn unparsable() {
        assert!(parse_subject("hello world").is_none());
        assert!(parse_subject("").is_none());
    }

    #[test]
    fn obfuscation() {
        assert!(is_obfuscated("a1B2c3D4e5F6g7H8"));
        assert!(is_obfuscated("a1B2c3D4e5F6g7H"));
        assert!(!is_obfuscated("Some.Show.S01E01"));
        assert!(!is_obfuscated("Movie.2024.1080p.WEB"));
    }

    #[test]
    fn grouping_and_completeness() {
        let arts = vec![
            art(r#"[1/2] - "rel.part1.rar" yEnc (1/2)"#, 10),
            art(r#"[1/2] - "rel.part1.rar" yEnc (2/2)"#, 10),
            art(r#"[2/2] - "rel.part2.rar" yEnc (1/1)"#, 5),
            art("garbage", 1),
        ];
        let rels = group_articles(arts);
        assert_eq!(rels.len(), 1);
        let r = &rels["rel"];
        assert_eq!(r.size, 25);
        assert!(is_complete(&r.articles));

        let rels = group_articles(vec![art(r#"[1/2] - "why.part1.rar" yEnc (1/2)"#, 10)]);
        assert!(!is_complete(&rels["why"].articles));

        // 0-indexed file counters [00/2]..[02/2] produce 3 files with file_total = 2
        let zero_indexed = vec![
            art(r#"[00/02] - "rel0.nzb" yEnc (1/1)"#, 2),
            art(r#"[01/02] - "rel0.part1.rar" yEnc (1/1)"#, 10),
            art(r#"[02/02] - "rel0.part2.rar" yEnc (1/1)"#, 10),
        ];
        let rels = group_articles(zero_indexed);
        assert!(is_complete(&rels["rel0"].articles));
    }

    #[test]
    fn bracket_parts_remap() {
        let arts = vec![
            art(r#"[1/3] - "same.bin" yEnc (1/1)"#, 1),
            art(r#"[2/3] - "same.bin" yEnc (1/1)"#, 1),
            art(r#"[3/3] - "same.bin" yEnc (1/1)"#, 1),
        ];
        let rels = group_articles(arts);
        let r = &rels["same.bin"];
        let parts: Vec<_> = r.articles.iter().map(|a| (a.part, a.total_parts, a.file_total)).collect();
        assert_eq!(parts, vec![(Some(1), Some(3), None), (Some(2), Some(3), None), (Some(3), Some(3), None)]);
        assert!(is_complete(&r.articles));
    }

    #[test]
    fn season_identifiers_survive_suffix_stripping() {
        assert_eq!(strip_release_suffixes("Example.Show.S01.part1.rar"), "Example.Show.S01");
        assert_eq!(strip_release_suffixes("Example.Show.S02.vol000-001.par2"), "Example.Show.S02");
        assert_eq!(strip_release_suffixes("Example.Show.S01.7z.001"), "Example.Show.S01");
    }

    #[test]
    fn zero_based_parts_set_the_complete_flag() {
        let file = |parts: &[i64]| -> Vec<Article> {
            parts
                .iter()
                .map(|&p| Article {
                    filename: Some("a.bin".into()),
                    part: Some(p),
                    total_parts: Some(4),
                    ..Default::default()
                })
                .collect()
        };
        assert!(is_complete(&file(&[0, 1, 2, 3])));
        assert!(!is_complete(&file(&[0, 1, 3])));
        assert!(is_complete(&file(&[1, 2, 3, 4])));
        assert!(!is_complete(&file(&[1, 2, 4])));
    }
}
