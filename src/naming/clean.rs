//! The release name in a raw candidate.
//!
//! Ported from newznab-tmux `app/Services/ReleaseCleaningService.php`,
//! `app/Services/NameFixing/{FileNameCleaner,NzbSplitUnwrapper}.php`, GPLv3.

use std::sync::LazyLock;

use regex::Regex;

macro_rules! re {
    ($name:ident, $p:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($p).unwrap());
    };
}

re!(
    WRAPPER,
    r"(?i)\.(?:7z\.\d{2,4}|part\d+\.rar|part\d*|r\d{2,4}|rar|z\d{2}|zip|7z|vol\d+[+-]\d+\.par2?|vol\d+[+-]\d+|par2?|tar|gz|bz2|xz|lz|lzma|cab|arj|ace|arc|zst|tgz|lz4|enc|rev|\d{3}|nfo|sfv|nzb|srr|srs)$"
);
re!(
    CODEC_AFTER_WRAPPER,
    r"(?i)\.(?:mp3|flac|m4a|aac|ogg|wav|wma|ape|opus|mka|ac3|dts|eac3|truehd|mpc|shn|tak|tta|wv|ts|divx|wmv|mpg|mpeg|rm|rmvb|m1v|m2v|mpv|mpgv|mp2|m4b|aiff?|amr|ra|w64)$"
);
re!(
    PAYLOAD,
    r"(?i)\.(?:mkv|avi|mp4|m4v|wmv|mpg|mpeg|mov|ts|m2ts|vob|divx|flv|webm|ogv|3gp|asf|rm|rmvb|f4v|mp3|flac|m4a|aac|ogg|wav|wma|ape|opus|mka|ac3|dts|eac3|truehd|mpc|shn|tak|tta|wv|jpg|jpeg|png|gif|bmp|tiff?|webp|pdf|txt|diz|md5|sha1|cue|log|epub|mobi|azw3?|djvu|cbr|cbz|fb2|lit|prc|opf|iso|bin|mdf|mds|nrg|img|ccd|sub|exe|msi|dmg|pkg|apk|xap|appx|deb|rpm|srt|idx|ass|ssa|vtt|sup|docx?|rtf|xlsx?|pptx?|odt|m3u8?|ifo|mks|ogm)$"
);
re!(SAMPLE_TAIL, r"(?i)[.\-_](?:sample|proof|subs?|thumbs?|cover|screens?)[.\-_]?$");
re!(SEGMENT_TAIL, r"(?i)[.\-_]?(?:part|vol|cd|dvd|dis[ck])\d+$");
re!(PREFIX, r"(?i)^(?:Release Name\s*:?\s*|sample[-_])");
re!(COLLISION_SUFFIX, r" \[\d{8}\]$"); // Part 2's ` [12345678]` on a taken name
re!(NZBSPLIT, r"(?i)__NZBSPLIT__[a-f0-9]{8,}__NZBSPLIT__(.+)$");
re!(SEPARATORS, r"[._\s]+");
re!(ROT13_YENC, r"(?i)\blRap\b");
re!(SPACES, r"\s+");
re!(SKIPPED_DIR, r"(?i)^(?:(?:cd|disc|disk|dvd)[^/\\]{0,4}|subs?|sample|proof|video_ts|audio_ts)$");

pub fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}
/// wrappers repeatedly, then one payload extension; a codec word after a wrapper is part of the title
pub fn strip_file_suffixes(name: &str) -> String {
    let mut s = name.to_string();
    loop {
        let n = WRAPPER.replace(&s, "").into_owned();
        if n == s {
            break;
        }
        s = n;
    }
    if s != name && CODEC_AFTER_WRAPPER.is_match(&s) {
        return s;
    }
    PAYLOAD.replace(&s, "").into_owned()
}
/// The release name in a raw candidate: no path, no control characters, no file suffix, no sample or part tail.
pub fn clean_candidate(raw: &str) -> Option<String> {
    let plain: String = raw.chars().filter(|c| !c.is_control()).collect();
    let plain = plain.trim().trim_matches(['"', '\'']);
    let plain = without_collision_suffix(plain);
    let mut name = basename(plain).to_string();
    if let Some(c) = NZBSPLIT.captures(&name) {
        name = c[1].replace('_', ".").replace('+', " ");
    }
    let name = strip_file_suffixes(&name);
    let name = SAMPLE_TAIL.replace(&name, "");
    let name = SEGMENT_TAIL.replace(&name, "");
    let name = PREFIX.replace(&name, "");
    let name = SPACES.replace_all(&name, " ");
    let name = name.trim_matches([' ', '.', '-', '_', '"', '\'', '\t']);
    (!name.is_empty()).then(|| name.to_string())
}
/// `Title-GRP/CD1/file.rar` gives `Title-GRP`. Directories named cd*, disc*, disk*, dvd* (up to 8 characters),
/// subs, sub, sample, proof, video_ts, audio_ts are skipped.
pub fn parent_dir(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split(['/', '\\']).collect();
    let (_, dirs) = parts.split_last()?;
    dirs.iter().rev().map(|d| d.trim()).find(|d| !d.is_empty() && !SKIPPED_DIR.is_match(d)).map(String::from)
}
/// The same release, whatever the case and whichever of `.`, `_` and space separates its words.
pub fn identity_key(s: &str) -> String {
    SEPARATORS.replace_all(s.trim().to_lowercase().as_str(), ".").into_owned()
}
pub fn has_collision_suffix(name: &str) -> bool {
    COLLISION_SUFFIX.is_match(name)
}
/// `name` without Part 2's collision suffix, which isnt part of what the name says
pub fn without_collision_suffix(name: &str) -> &str {
    COLLISION_SUFFIX.find(name).map_or(name, |m| &name[..m.start()])
}
pub fn rot13_subject(subject: &str) -> bool {
    ROT13_YENC.is_match(subject)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_table() {
        let table = [
            ("Some.Movie.2024.1080p.x264-GRP.mkv", "Some.Movie.2024.1080p.x264-GRP"),
            ("Some.Movie.2024.1080p.x264-GRP.part01.rar", "Some.Movie.2024.1080p.x264-GRP"),
            ("Some.Movie-GRP.vol03+04.par2", "Some.Movie-GRP"),
            ("Some.Movie-GRP.7z.003", "Some.Movie-GRP"),
            ("Movie.2020.1080p.DTS.rar", "Movie.2020.1080p.DTS"),
            ("Movie.2020.1080p.DTS.mkv", "Movie.2020.1080p.DTS"),
            ("dir\\sub/Some.Show.S01E01-GRP-sample.mkv", "Some.Show.S01E01-GRP"),
            ("Release Name: Foo.Bar-GRP", "Foo.Bar-GRP"),
            ("__NZBSPLIT__0123abcd__NZBSPLIT__Foo_Bar_2024+x264-GRP.rar", "Foo.Bar.2024 x264-GRP"),
            ("Foo.Bar.CD1.rar", "Foo.Bar"),
            ("  spaced   out   name.mp4  ", "spaced out name"),
            ("\"quoted.name.nfo\"", "quoted.name"),
            ("Title-GRP [12345678]", "Title-GRP"),
            ("T.mkv [12345678]", "T"),
            ("Movie.Name [2019]", "Movie.Name [2019]"),
        ];
        for (raw, want) in table {
            assert_eq!(clean_candidate(raw).as_deref(), Some(want), "{raw}");
        }
    }

    #[test]
    fn parent_table() {
        assert_eq!(parent_dir("A.Movie-GRP/grp-a.mkv").as_deref(), Some("A.Movie-GRP"));
        assert_eq!(parent_dir("A.Movie-GRP\\CD1\\a.rar").as_deref(), Some("A.Movie-GRP"));
        assert_eq!(parent_dir("a.mkv"), None);
        assert_eq!(parent_dir("Sample/a.mkv"), None);
    }
}
