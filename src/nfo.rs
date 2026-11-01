//! Pull a release name out of an .nfo file.
//!
//! The fields, lines and `Source:` rule are ported from newznab-tmux
//! `app/Services/NameFixing/Extractors/NfoNameExtractor.php`, GPLv3.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::naming::{Grade, clean_candidate, identity_key, judge};

static NFO_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)\.(?:nfo|txt)(?:"|\s|$)"#).unwrap());
static TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<(?:title|name|release(?:name)?)>\s*(.*?)\s*</").unwrap());
static LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*(?:release\s*name|release|title|movie|series)\s*:\s*(.+?)\s*$").unwrap());
static WS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
static FILLER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[-=_*# .]+$").unwrap());

static SOURCE_FIELD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^[ \t]*Source[ \t]*:").unwrap());
static CONTINUATION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[ \t]+\S").unwrap());
static FIELD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[ \t]*[A-Za-z][A-Za-z0-9 _-]*[ \t]*:").unwrap());
static WIN_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[ \t]*[A-Za-z]:[\\/]").unwrap());
static LITERAL_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?im)^[ \t]*(?:Filename|File[ \t]+Name|Complete[ \t]+Name|Release(?:[ \t]+Name)?)[ \t]*[:=][ \t]*([^\r\n]+)$",
    )
    .unwrap()
});
static SCENE_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\p{L}\p{N}][\p{L}\p{N}\p{M}._-]{6,}-[A-Za-z0-9]{2,15}").unwrap());
static TV_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i):[ \t]+([^\r\n]*?S\d{1,3}[._ -]?[ED]\d{1,3}[^\r\n]*?)(?:[ \t]{2,}|\r|\n|$)").unwrap()
});
static MOVIE_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i):[ \t]+([^\r\n]*?(?:19|20)\d\d[^\r\n]*?(?:BDRip|bluray|DVD(?:R|Rip)?|XVID|WEB-?DL|1080p|720p)[^\r\n]*?)(?:[ \t]{2,}|\r|\n|$)")
        .unwrap()
});
static TITLE_YEAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^[ \t]*([\p{L}\p{N}][\p{L}\p{N} .,'&:-]{2,80}\s?\((?:19|20)\d\d\))").unwrap());
const NFO_MAX_TEXT: usize = 262_144;
const MAX_TOKENS: usize = 100;

/// What in `text` has the shape of a scene name, for the caller to judge. A token touching more name text is
/// only a piece of a name: what follows the apostrophe in `Show.S01E01.It's.Here.720p.x264-GRP` looks whole on its own.
pub fn scene_tokens(text: &str) -> impl Iterator<Item = &str> {
    const OPENS: &[u8] = b"\"[(<{|:,";
    const CLOSES: &[u8] = b"\"])>}|:,";
    SCENE_TOKEN
        .find_iter(text)
        .take(MAX_TOKENS)
        .filter(move |m| {
            let before = m.start().checked_sub(1).map(|at| text.as_bytes()[at]);
            let after = text.as_bytes().get(m.end()).copied();
            before.is_none_or(|b| b.is_ascii_whitespace() || OPENS.contains(&b))
                && after.is_none_or(|b| b.is_ascii_whitespace() || CLOSES.contains(&b))
        })
        .map(|m| m.as_str())
}

pub fn is_nfo(subject: &str) -> bool {
    NFO_RE.is_match(subject)
}

/// A Source field and its indented continuation lines become blank lines: they name another release.
pub fn identity_text(text: &str) -> String {
    let mut in_source = false;
    let lines: Vec<&str> = text
        .split('\n')
        .map(|line| {
            if SOURCE_FIELD.is_match(line) {
                in_source = true;
                return "";
            }
            if in_source && CONTINUATION.is_match(line) && !FIELD.is_match(line) && !WIN_PATH.is_match(line) {
                return "";
            }
            in_source = false;
            line
        })
        .collect();
    lines.join("\n")
}

fn tidy(c: &str) -> String {
    WS_RE.replace_all(c, " ").trim_matches([' ', '.', '\t', '\r', '\n']).to_string()
}

/// Best first, deduped, each passing `valid_name`. Empty for an indexer's own NFO. Only the head is read.
pub fn name_candidates(data: &[u8]) -> Vec<String> {
    let head = data.get(..NFO_MAX_TEXT).unwrap_or(data);
    let text = String::from_utf8_lossy(head);
    if text.trim_start().starts_with("=newz[NZB]=") {
        return Vec::new();
    }
    // the regexes' line ends are \n only
    let text = identity_text(&text.replace("\r\n", "\n").replace('\r', "\n"));

    let literal: Vec<String> = LITERAL_FIELD.captures_iter(&text).filter_map(|c| clean_candidate(&c[1])).collect();
    let graded = |want: Grade| literal.iter().filter(move |c| judge(c) == want).cloned();

    let mut all: Vec<String> = graded(Grade::Scene).collect();
    all.extend(
        SCENE_TOKEN
            .find_iter(&text)
            .take(MAX_TOKENS)
            .map(|m| m.as_str().to_string())
            .filter(|t| judge(t) == Grade::Scene),
    );
    all.extend(TAG_RE.captures_iter(&text).map(|c| tidy(&c[1])));
    all.extend(text.split(['\n', '\x0b', '\x0c']).filter_map(|l| LINE_RE.captures(l)).map(|c| tidy(&c[1])));
    all.extend(graded(Grade::Good));
    for line in [&*TV_LINE, &*MOVIE_LINE, &*TITLE_YEAR] {
        all.extend(line.captures_iter(&text).map(|c| tidy(&c[1])));
    }

    let mut seen = HashSet::new();
    all.into_iter().filter(|name| valid_name(name) && seen.insert(identity_key(name))).collect()
}

pub fn display_name(data: &[u8]) -> Option<String> {
    name_candidates(data).into_iter().next()
}

pub fn valid_name(name: &str) -> bool {
    let len = name.chars().count();

    if !(3..=240).contains(&len) {
        return false;
    }

    if FILLER_RE.is_match(name) {
        return false;
    }

    !matches!(name.to_lowercase().as_str(), "nfo" | "release" | "title" | "unknown" | "none")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_field_is_not_the_release() {
        let nfo = "Source : Other.Movie.2019.1080p.BluRay.x264-OTHER\n  Other.Movie.2019.720p.BluRay.x264-OTHER\nRelease: Real.Movie.2024.1080p.WEB-DL.x264-GRP\n";
        assert_eq!(display_name(nfo.as_bytes()).as_deref(), Some("Real.Movie.2024.1080p.WEB-DL.x264-GRP"));
        assert_eq!(identity_text(nfo), "\n\nRelease: Real.Movie.2024.1080p.WEB-DL.x264-GRP\n");
    }

    #[test]
    fn a_scene_name_anywhere_beats_a_weak_field() {
        let nfo = b"Title: Some Film\n  Real.Movie.2024.1080p.WEB-DL.x264-GRP  \n";
        assert_eq!(display_name(nfo).as_deref(), Some("Real.Movie.2024.1080p.WEB-DL.x264-GRP"));
    }

    #[test]
    fn an_indexer_nfo_gives_nothing() {
        assert!(name_candidates(b"=newz[NZB]=\nRelease: Real.Movie.2024.1080p.WEB-DL.x264-GRP\n").is_empty());
    }

    #[test]
    fn a_complete_name_field_loses_its_path() {
        let nfo = b"Complete name : /x/Real.Movie.2024.1080p.WEB-DL.x264-GRP.mkv";
        assert_eq!(display_name(nfo).as_deref(), Some("Real.Movie.2024.1080p.WEB-DL.x264-GRP"));
    }

    #[test]
    fn title_and_year() {
        assert_eq!(display_name(b"Nice Film (2019)\nplot blah\n").as_deref(), Some("Nice Film (2019)"));
    }

    #[test]
    fn the_e2e_nfo_still_reads() {
        let nfo = b"some ascii art\n.leading dot line\nRelease Name: Cool Movie 2024 1080p";
        assert_eq!(display_name(nfo).as_deref(), Some("Cool Movie 2024 1080p"));
    }

    #[test]
    fn windows_line_ends_read_the_same() {
        let nfo = b"Complete name : /x/Real.Movie.2024.1080p.WEB-DL.x264-GRP.mkv\r\nmore\r\n";
        assert_eq!(display_name(nfo).as_deref(), Some("Real.Movie.2024.1080p.WEB-DL.x264-GRP"));
    }

    #[test]
    fn only_the_head_is_read() {
        let mut nfo = vec![b' '; 262_144];
        nfo.extend_from_slice(b"\nReal.Movie.2024.1080p.WEB-DL.x264-GRP\n");
        assert!(name_candidates(&nfo).is_empty());
    }

    #[test]
    fn detects_nfo_subjects() {
        assert!(is_nfo(r#""abc.nfo" yEnc (1/1)"#));
        assert!(is_nfo("abc.TXT"));
        assert!(!is_nfo(r#""abc.nfox" yEnc"#));
    }

    #[test]
    fn finds_names() {
        assert_eq!(display_name(b"<title> Cool.Movie.2024 </title>").as_deref(), Some("Cool.Movie.2024"));
        assert_eq!(
            display_name(b"=====\n  Release Name :  Some   Show S01E01  \nfoo").as_deref(),
            Some("Some Show S01E01")
        );
        assert_eq!(display_name(b"title: ----\nmovie: Real Name..").as_deref(), Some("Real Name"));
        assert_eq!(display_name(b"title: none"), None);
        assert_eq!(display_name(b""), None);
    }
}
