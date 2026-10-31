//! Which articles belong to one post, whatever its files are called: the
//! collection a subject belongs to, from per group regexes with a generic
//! fallback. Ported from newznab-tmux (`CollectionsCleaningService`,
//! `CollectionHandler`, `RegexService`, the `collection_regexes` seed), GPLv3.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use indexmap::IndexMap;
use regex::{Regex, RegexBuilder};

use crate::filters::COUNTER_RE;
use crate::parser::{Article, ParsedSubject, Release};

/// a post claiming more files than this isnt merged: its files stay releases of
/// their own, as a forged counter cant make one release of a million files
pub const MAX_COLLECTION_FILES: i64 = 10_000;

/// letters and digits a cleaned name needs to tell a post by itself; under
/// this the poster and the day have to help (`IMG_`, `DSC`)
const STRONG_NAME: usize = 16;

const IMAGE_EXT: [&str; 8] = ["jpg", "jpeg", "png", "gif", "bmp", "webp", "tif", "tiff"];
const AUDIO_EXT: [&str; 9] = ["mp3", "flac", "m4a", "ogg", "wav", "wma", "ape", "aac", "opus"];

/// id, ordinal, group_regex, flags, pattern, in the order they are tried. Made by
/// scripts/collection_regexes_to_tsv.py.
const TABLE: &str = include_str!("../data/collection_regexes.tsv");

struct Row {
    /// its line in the table: upstream's order, over every group_regex that matches
    order: usize,
    id: i32,
    case_insensitive: bool,
    regex: &'static str,
}

/// each distinct group_regex with the rows under it
static BY_GROUP_REGEX: LazyLock<Vec<(Regex, Vec<Row>)>> = LazyLock::new(|| {
    let mut by: IndexMap<&str, Vec<Row>> = IndexMap::new();
    for (order, line) in TABLE.lines().filter(|l| !l.starts_with('#')).enumerate() {
        let mut cols = line.split('\t');
        let (Some(id), Some(_ordinal), Some(group), Some(flags), Some(regex)) =
            (cols.next(), cols.next(), cols.next(), cols.next(), cols.next())
        else {
            continue;
        };
        let row = Row { order, id: id.parse().unwrap_or(0), case_insensitive: flags.contains('i'), regex };
        by.entry(group).or_default().push(row);
    }
    by.into_iter().filter_map(|(group, rows)| Some((Regex::new(group).ok()?, rows))).collect()
});

/// One group's regexes in upstream's order, compiled the first time it comes up.
type Compiled = Arc<Vec<Regex>>;
static COMPILED: LazyLock<Mutex<HashMap<String, Compiled>>> = LazyLock::new(Default::default);

fn compiled(group: &str) -> Compiled {
    if let Some(found) = COMPILED.lock().unwrap().get(group) {
        return found.clone();
    }
    let mut rows: Vec<&Row> =
        BY_GROUP_REGEX.iter().filter(|(g, _)| g.is_match(group)).flat_map(|(_, rows)| rows).collect();
    rows.sort_by_key(|r| r.order);
    // outside the lock: two threads compiling one group at once is only waste
    let regexes = rows.iter().filter_map(|row| {
        let built = RegexBuilder::new(row.regex).case_insensitive(row.case_insensitive).build();
        built.inspect_err(|e| println!("collection regex {} doesnt compile: {e}", row.id)).ok()
    });
    let list: Compiled = Arc::new(regexes.collect());
    COMPILED.lock().unwrap().insert(group.to_string(), list.clone());
    list
}

/// `<base> (12/45)`
static BASE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(.+?)\s+\(\d+\s*/\s*\d+\)").unwrap());

/// What upstream's regexes run on: the subject up to its part counter, with a
/// ` yEnc` when the subject has none. A subject with no part counter is its own base.
pub fn subject_base(subject: &str) -> Cow<'_, str> {
    let base = BASE_RE.captures(subject).and_then(|c| c.get(1)).map_or(subject.trim(), |m| m.as_str());
    if subject.as_bytes().windows(4).any(|w| w.eq_ignore_ascii_case(b"yenc")) {
        Cow::Borrowed(base)
    } else {
        Cow::Owned(format!("{base} yEnc"))
    }
}

/// (file number, files) of a base's counter
fn file_counter(base: &str) -> Option<(i64, i64)> {
    let c = COUNTER_RE.captures(base)?;
    Some((c[1].parse().ok()?, c[2].parse().ok()?))
}

const FILE_SIZES: &str = r#"\d{1,3}([,.\/])\d{1,3}\s([kmg])b|(])?\s\d+KB\s(yENC)?|"?\s\d+\sbytes?|[- ]?\d+([.,])?\d+\s([gkm])?B\s-?(\s?yenc)?|\s\(d{1,3},\d{1,3}\s\{K,M,G\}B\)\s|yEnc \d+k$|\{\d+ yEnc bytes\}|yEnc \d+ |\(\d+ ?([kmg])?b(ytes)?\) yEnc$"#;
const MUSIC_FILE_SIZES: &str = r#"\d{1,3}([,.\/])\d{1,3}\s([kmg])b|(])?\s\d+KB\s(yENC)?|"?\s\d+\sbytes?|[- ]?\d+[.,]?\d+\s([gkm])?B\s-?(\s?yenc)?|\s\(d{1,3},\d{1,3}\s\{K,M,G\}B\)\s|yEnc \d+k$|\{\d+ yEnc bytes\}|yEnc \d+ |\(\d+ ?([kmg])?b(ytes)?\) yEnc$"#;
const MARKERS: &str =
    r#"AutoRarPar\d{1,5}|\(\d+\)\s?yEnc|\d+(Amateur|Classic)| \d{4,}[a-z]{4,} |.vol\d+\+\d+|.part\d+"#;
const NUMBERING: &str = r#"((( \(\d\d\) -|(\d\d)? - \d\d\.|\d{4} \d\d -) | - \d\d-| \d\d\. [a-z]).+| \d\d of \d\d| \dof\d)\.mp3"?|([)(\[\s])\d{1,5}(\/|([\s_])of([\s_])|-)\d{1,5}([)\]\s$:])|\(\d{1,3}\|\d{1,3}\)|[^\d]{4}-\d{1,3}-\d{1,3}\.|\s\d{1,3}\sof\s\d{1,3}\.|\s\d{1,3}\/\d{1,3}|\d{1,3}of\d{1,3}\.|^\d{1,3}\/\d{1,3}\s|\d{1,3} - of \d{1,3}"#;
const MUSIC_NUMBERING: &str = r#"((( \(\d\d\) -|(\d\d)? - \d\d\.|\d{4} \d\d -) | - \d\d-| \d\d\. [a-z]).+| \d\d of \d\d| \dof\d)\.mp3"?|([(\[\s])\d{1,4}(\/|([\s_])of([\s_])|-)\d{1,4}([)\]\s$:])|\(\d{1,3}\|\d{1,3}\)|-\d{1,3}-\d{1,3}\.|\s\d{1,3}\sof\s\d{1,3}\.|\s\d{1,3}\/\d{1,3}|\d{1,3}of\d{1,3}\.|^\d{1,3}\/\d{1,3}\s|\d{1,3} - of \d{1,3}"#;
// upstream has `\d{3}(?=([\s"]))` where this has `\d{3}[\s"]`: the character
// it now takes is a quote or a space, and what is taken becomes a space
const EXTENSIONS: &str = r#"(-? [a-z0-9]+-?|\(?\d{4}\)?([_-])[a-z0-9]+)\.jpg"?| [a-z0-9]+\.mu3"?|((\d{1,3})?\.part(\d{1,5})?|\d{1,5} ?|sample|- Partie \d+)?\.(7z|\d{3}[\s"]|avi|diz|docx?|epub|idx|iso|jpg|m3u|m4a|mds|mkv|mobi|mp4|nfo|nzb|par(\s?2|")|pdf|rar|rev|rtf|r\d\d|sfv|srs|srr|sub|txt|vol.+(par2)|xls|zip|z{2,3})"?|(\s|(\d{2,3})?-)\d{2,3}\.mp3|\d{2,3}\.pdf|\.part\d{1,4}\."#;
// EXTENSIONS without what takes the digits before an extension: `\d{1,5} ?`,
// `(\d{1,3})?` before `.part`, and the `.mp3` and `.pdf` ones. Upstream's row
// is gone within hours, Atlas's stays: `Name.2019` and `Name.2021` must stay two posts
const EXTENSIONS_KEEPING_DIGITS: &str = r#"(-? [a-z0-9]+-?|\(?\d{4}\)?([_-])[a-z0-9]+)\.jpg"?| [a-z0-9]+\.mu3"?|(\.part(\d{1,5})?|sample|- Partie \d+)?\.(7z|\d{3}[\s"]|avi|diz|docx?|epub|idx|iso|jpg|m3u|m4a|mds|mkv|mobi|mp4|nfo|nzb|par(\s?2|")|pdf|rar|rev|rtf|r\d\d|sfv|srs|srr|sub|txt|vol.+(par2)|xls|zip|z{2,3})"?|\.part\d{1,4}\."#;
const LEFTOVER_EXTENSION: &str = r#"([\-_](proof|sample|thumbs?))*(\.part\d*(\.rar)?|\.rar|\.7z|\.par2)?(\d{1,3}\.rev"|\.vol\d+\+\d+.+?"|\.[A-Za-z0-9]{2,4}"|")"#;

fn insensitive(p: &str) -> Regex {
    RegexBuilder::new(p).case_insensitive(true).build().unwrap()
}

/// upstream's generic cleaner, for files numbered within a post (`numbered`)
static GENERIC_NUMBERED: LazyLock<[Regex; 5]> =
    LazyLock::new(|| [FILE_SIZES, MARKERS, NUMBERING, EXTENSIONS, LEFTOVER_EXTENSION].map(insensitive));
/// the same with a post's own digits left in
static GENERIC: LazyLock<[Regex; 5]> =
    LazyLock::new(|| [FILE_SIZES, MARKERS, NUMBERING, EXTENSIONS_KEEPING_DIGITS, LEFTOVER_EXTENSION].map(insensitive));
static MUSIC: LazyLock<[Regex; 5]> =
    LazyLock::new(|| [MUSIC_NUMBERING, r#"".+""#, EXTENSIONS, MUSIC_FILE_SIZES, MARKERS].map(insensitive));
static MUSIC_GROUP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.(flac|lossless|mp3|music|sounds)").unwrap());
static MUSIC_SUBJECT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^(\w{10,}-[a-zA-Z0-9]+ ")\d\d-.+?" yEnc$"#).unwrap());
static SHORT_MUSIC: LazyLock<Regex> = LazyLock::new(|| insensitive(r"^[\-a-z0-9$ ]{1,7}yEnc$"));
static FIRST_QUOTED_WORD: LazyLock<Regex> = LazyLock::new(|| insensitive(r#".*("[A-Z0-9]+).*?""#));
static QUOTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"".+?""#).unwrap());
static LETTERS_OR_EXTENSION: LazyLock<Regex> = LazyLock::new(|| insensitive(&format!("[a-z0-9]|{LEFTOVER_EXTENSION}")));
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s\s+").unwrap());

fn normalize(s: &str) -> String {
    SPACES.replace_all(s, " ").trim().to_string()
}

fn replace_each(patterns: &[Regex], base: &str) -> String {
    let mut out = Cow::Borrowed(base);
    for p in patterns {
        if let Cow::Owned(s) = p.replace_all(&out, " ") {
            out = Cow::Owned(s);
        }
    }
    normalize(&out)
}

/// the extension of a parsed name, lowercase. `RELEASE_SUFFIX_RE` already took
/// `.rar`, `.par2`, `.nfo` and the like off, and a year or `S01E01` isnt one.
fn extension(release_name: &str) -> Option<(&str, String)> {
    let (stem, ext) = release_name.rsplit_once('.')?;
    let fits = (2..=4).contains(&ext.len())
        && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        && ext.bytes().any(|b| b.is_ascii_alphabetic());
    fits.then(|| (stem, ext.to_ascii_lowercase()))
}

fn is_audio(release_name: &str) -> bool {
    extension(release_name).is_some_and(|(_, ext)| AUDIO_EXT.contains(&ext.as_str()))
}

/// The digits before a file's extension number the files of a post, rather than
/// tell the post: `IMG_0007.jpg`. An image always. Another file only as the
/// `n`th of 3 or more with `n` as its number: `[7/10] Show.S01E07.mkv`, since
/// `[1/2] Show.S01E02.mkv` and `[1/9] Show.S01E02.mkv` are one episode's
/// post. A subject alone cant say more: `[1/9] Show.S01E01.mkv` is the same
/// text in a pack and in a post with 8 par2 files.
fn numbered(release_name: &str, file: i64, files: i64) -> bool {
    let Some((stem, ext)) = extension(release_name) else { return false };
    let digits = stem.len() - stem.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 || digits > 5 || AUDIO_EXT.contains(&ext.as_str()) {
        return false;
    }
    if IMAGE_EXT.contains(&ext.as_str()) {
        return files >= 2;
    }
    files >= 3 && stem[stem.len() - digits..].parse() == Ok(file)
}

/// `CollectionsCleaningService::generic()`, no regex of the group matched: the
/// music cleaner in a music group or for an audio file, which leaves what is
/// in quotes out, otherwise the generic one
fn clean_generic(base: &str, group: &str, release_name: &str) -> String {
    if !MUSIC_GROUP.is_match(group) && !is_audio(release_name) {
        return replace_each(&*GENERIC, base);
    }
    if let Some(c) = MUSIC_SUBJECT.captures(base) {
        return c[1].to_string();
    }
    let clean = replace_each(&*MUSIC, base);
    if clean.len() > 10 && !SHORT_MUSIC.is_match(&clean) {
        return clean;
    }
    // too little left: what is outside the quotes, and the first word inside
    let mut extra = FIRST_QUOTED_WORD.captures(base).map_or(String::new(), |c| c[1].to_string());
    extra.extend(base.chars().filter(|c| !c.is_ascii_alphanumeric()).take(3));
    let rest = QUOTED.replace_all(base, "");
    let rest = LETTERS_OR_EXTENSION.replace_all(&rest, "");
    format!("{clean}{rest}{extra}")
}

/// `RegexService::_matchRegex`: the named groups in name order, without reqid
/// and parts ones. None when nothing, or `0`, comes of it (upstream's falsy).
fn from_regex(re: &Regex, base: &str) -> Option<String> {
    let c = re.captures(base)?;
    let mut names: Vec<&str> =
        re.capture_names().flatten().filter(|n| !n.to_ascii_lowercase().contains("reqid")).collect();
    names.retain(|n| !n.to_ascii_lowercase().contains("parts"));
    names.sort_unstable();
    let out: String = names.iter().filter_map(|n| c.name(n)).map(|m| m.as_str()).collect();
    (!out.is_empty() && out != "0").then_some(out)
}

/// `CollectionsCleaningService::collectionsCleaner`: what stays the same over a
/// post's articles, and whether a regex of the group said it
fn clean(base: &str, group: &str, release_name: &str, regexes: &[Regex]) -> (String, bool) {
    match regexes.iter().find_map(|r| from_regex(r, base)) {
        Some(name) => (name, true),
        None => (clean_generic(base, group, release_name), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_regex_compiles() {
        let mut count = 0;
        for (_, rows) in BY_GROUP_REGEX.iter() {
            for row in rows {
                RegexBuilder::new(row.regex).case_insensitive(row.case_insensitive).build()
                    .unwrap_or_else(|e| panic!("regex {}: {e}", row.id));
                assert!(row.regex.contains("(?P<match"), "regex {} has no named group: nothing would come of it", row.id);
                count += 1;
            }
        }
        assert_eq!(count, 602);
        assert_eq!(BY_GROUP_REGEX.len(), 101);
    }

    #[test]
    fn group_regexes_run_in_upstreams_order_lazily() {
        assert_eq!(compiled("alt.binaries.teevee").len(), 22);
        assert_eq!(compiled("alt.binaries.boneless").len(), 26);
        assert!(compiled("alt.binaries.no.such").is_empty());
        assert!(Arc::ptr_eq(&compiled("alt.binaries.teevee"), &compiled("alt.binaries.teevee")));
    }

    #[test]
    fn upstreams_order_holds_over_group_regexes() {
        // `alt.binaries.ath` has rows under two group_regexes
        let table: Vec<&str> = TABLE.lines().filter(|l| !l.starts_with('#'))
            .map(|l| l.split('\t').collect::<Vec<_>>())
            .filter(|c| c[2] == r"^alt\.binaries\.ath$" || c[2] == r"^alt\.binaries\.(cores|ath)$")
            .map(|c| c[4]).collect();
        let got = compiled("alt.binaries.ath");
        assert!(table.len() >= 2);
        assert_eq!(got.iter().map(|r| r.as_str()).collect::<Vec<_>>(), table);
    }

    #[test]
    fn no_group_of_the_seed_is_blocked() {
        let names = BY_GROUP_REGEX.iter().map(|(g, _)| g.as_str().replace('\\', ""));
        let hits: Vec<String> = names.filter(|g| crate::filters::group_blocked(g)).collect();
        assert!(hits.is_empty(), "{hits:?}");
    }

    const G: &str = "alt.binaries.test";

    #[test]
    fn subject_base_is_what_upstream_passes_to_its_regexes() {
        assert_eq!(subject_base(r#"[41/62] "abc" yEnc (316/375)"#), r#"[41/62] "abc" yEnc"#);
        assert_eq!(subject_base(r#"[41/62] "abc" (316/375)"#), r#"[41/62] "abc" yEnc"#);
        assert_eq!(subject_base("[3/7] - file.name.mkv yEnc"), "[3/7] - file.name.mkv yEnc");
        assert_eq!(subject_base(r#"[1/2] "abc def""#), r#"[1/2] "abc def" yEnc"#);
    }

    #[test]
    fn generic_cleaning() {
        let clean = |s: &str, name: &str| clean_generic(&subject_base(s), G, name);
        let first = clean(r#"[1/4] - "Some.Show.S01.1080p-GRP.part1.rar" yEnc (1/2)"#, "Some.Show.S01.1080p-GRP");
        assert_eq!(first, "- Some.Show.S01.1080p-GRP yEnc");
        assert_eq!(clean(r#"[2/4] - "Some.Show.S01.1080p-GRP.part2.rar" yEnc (1/1)"#, ""), first);
        assert_eq!(clean(r#"[3/4] - "Some.Show.S01.1080p-GRP.nfo" yEnc (1/1)"#, ""), first);
        assert_eq!(clean(r#"[4/4] - "Some.Show.S01.1080p-GRP.vol00+1.par2" yEnc (1/1)"#, ""), first);
        // a post's own digits stay
        assert_eq!(clean(r#"[1/2] "Name.2019.mkv" yEnc (1/9)"#, ""), "Name.2019 yEnc");
        assert_eq!(clean(r#"[2/2] "Name.2019.nfo" yEnc (1/1)"#, ""), "Name.2019 yEnc");
        assert_eq!(clean(r#"[1/20] "Name.2019.part01.rar" yEnc (1/9)"#, ""), "Name.2019 yEnc");
        assert_eq!(clean(r#"[1/2] "Name 2019.pdf" yEnc (1/9)"#, ""), "Name 2019 yEnc");
    }

    #[test]
    fn what_numbers_a_post() {
        // images, whatever the numbering starts at
        assert!(numbered("IMG_0007.jpg", 7, 50));
        assert!(numbered("DSC4831.JPG", 1, 2));
        // others only as their own position, in 3 or more
        assert!(numbered("Show.S01E07.mkv", 7, 10));
        assert!(!numbered("Show.S01E07.mkv", 1, 10));
        assert!(!numbered("Show.S01E01.mkv", 1, 2));
        assert!(!numbered("Name.2019.mkv", 1, 2));
        // nothing to go on
        assert!(!numbered("Name.mkv", 1, 10));
        assert!(!numbered("Name.2019", 1, 10), "a year isnt an extension");
        assert!(!numbered("Show.S01E01", 1, 10), "what a rar or nfo leaves");
        assert!(!numbered("Album.Track01.mp3", 1, 12), "audio goes by the music cleaner");
    }

    #[test]
    fn music_groups_and_audio_files_clean_their_own_way() {
        let music = "alt.binaries.sounds.mp3";
        let clean = |s: &str, g: &str| clean_generic(&subject_base(s), g, "x.mp3");
        let a = clean(r#"[01/12] "Artist Name - Song One.mp3" yEnc (1/8)"#, music);
        assert_eq!(a, clean(r#"[02/12] "Artist Name - Song Two.mp3" yEnc (1/8)"#, music), "what is in quotes varies");
        assert!(a.contains(r#""Artist"#), "a short result takes the first quoted word");
        assert_eq!(a, clean(r#"[01/12] "Artist Name - Song One.mp3" yEnc (1/8)"#, G), "by extension too");
        let hit = clean(r#"Some_Artist_Name-404 "00-some_artist-unknown-2009.sfv" yEnc"#, music);
        assert_eq!(hit, r#"Some_Artist_Name-404 ""#);
    }

    #[test]
    fn group_regexes_are_used() {
        // rule 6: `^(?P<match1>[a-zA-Z0-9]+)\[\d+\/\d+\] - ".+?" yEnc$` in teevee only. A made up token.
        let subject = r#"t2EI3CdWdF0hi5b8L9tkx[08/52] - "t2EI3CdWdF0hi5b8L9tkx.part07.rar" yEnc (1/3)"#;
        let group = "alt.binaries.teevee";
        let (name, by_regex) = clean(&subject_base(subject), group, "", &compiled(group));
        assert!(by_regex);
        assert_eq!(name, "t2EI3CdWdF0hi5b8L9tkx");
        let (_, by_regex) = clean(&subject_base(subject), G, "", &compiled(G));
        assert!(!by_regex);
    }
}
