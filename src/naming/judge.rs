//! How good is this as a release name?
//!
//! Plausibility, scene shape and season packs are ported from newznab-tmux
//! `app/Services/NameFixing/{FileNameCleaner,SeasonPackDetector,ReleaseUpdateService}.php`, GPLv3.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use super::obfuscation::is_obfuscated;

macro_rules! re {
    ($name:ident, $p:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($p).unwrap());
    };
}
re!(
    GENERIC,
    r"(?i)^(audio[._-]?ts|video[._-]?ts|vts[._-]?\d{1,2}[._-]?\d{1,2}|\d{1,3}|sample|proof|covers?|thumbs?|screens?|readme|index|movie|video|file|track\d*|cd\d*|dvd\d*|dis[ck]\d*|part\d*|vol\d*|image\d*|img\d*)$"
);
re!(SEGMENT, r"(?i)(?:^|[.\-_ ])(?:part|vol|r)\d+(?:\+\d+)?$");
re!(INSTALLER, r"(?i)^(setup|install|installer|patch|update|crack|keygen)\d*(?:[\s._-]|$)");
re!(WORDS, r"[\p{L}\p{N}]{3,}");
re!(TAIL, r"[-.][A-Za-z0-9]{2,}$");
re!(TV, r"(?i)\bS\d{1,2}[Eex]\d{1,3}\b");
re!(
    QUALITY,
    r"(?i)\b(480p|720p|1080p|2160p|4k|webrip|web[ .-]?dl|bluray|bdrip|dvdrip|hdtv|hdrip|xvid|x264|x265|hevc|h\.?264|proper|repack)\b"
);
re!(XXX, r"(?i)\bXXX\b");
re!(SCENE_SHAPE, r"^[\p{L}\p{N}][\p{L}\p{N}\p{M}._-]*-[A-Za-z0-9]{2,15}$");
re!(SCENE_TAIL_JUNK, r"(?i)[._-](?:sample|proof|thumbs?|part\d+|vol\d+(?:[+-]\d+)?)$");
re!(
    TECH,
    r"(?i)(?:^|[._-])(?:480p|576p|720p|1080[pi]|2160p|4320p|[48]k|web[._-]?dl|webrip|bluray|bdrip|brrip|dvdrip|hdtv|sdtv|x264|x265|h[._]?26[45]|hevc|avc|xvid|divx|aac|ac3|eac3|ddp?[0-9](?:[._][0-9])?|dts(?:[._-]?hd)?|flac|mp3|truehd|pal|ntsc)(?:$|[._-])"
);
re!(
    EPISODE,
    r"(?i)(?:^|[^a-z0-9])(?:S\d+[._ ]?E\d+|\d{1,2}x\d{2,}(?:$|[^a-z0-9]|[exs]\d)|Episode[._ ]?\d{1,4}(?:$|\D)|EP?\d{2,3}(?:$|[^0-9a-z]))"
);
re!(EPISODE_ID, r"(?i)(?:^|[^a-z0-9])(S\d+[._ ]?E\d+|\d{1,2}x\d{2,})");
re!(SEASON_TOKEN, r"(?i)(?:^|[._ \-\[(])S\d{1,4}(?:[._ ]?D(?:is[ck])?[._ ]?\d{1,2})?(?:$|[._ \-\])])");
re!(SEASON_WORD, r"(?i)(?:^|[._ \-\[(])Seasons?[._ ]?\d{1,2}(?:$|[._ \-\])])");
re!(COMPLETE, r"(?i)(?:^|[._ \-\[(])Complete[._ ](?:Series|Seasons?)(?:$|[._ \-\])])");

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Grade {
    Junk,
    Weak,
    Good,
    Scene,
}

/// "Is this a good name?"
pub fn judge(name: &str) -> Grade {
    let t = name.trim();
    let chars = t.chars().count();
    if chars == 0 || chars > 255 || t.chars().any(char::is_control) || t.contains(['\\', '/', '|']) {
        return Grade::Junk;
    }
    if GENERIC.is_match(t) || SEGMENT.is_match(t) || INSTALLER.is_match(t) || is_obfuscated(t) {
        return Grade::Junk;
    }
    let words = WORDS.find_iter(t).count();
    if words == 0 {
        return Grade::Junk;
    }
    // at least 4 words: `grp-title-1080p` has the shape but is a per-file short name, so it is only Good
    if words >= 4 && SCENE_SHAPE.is_match(t) && !SCENE_TAIL_JUNK.is_match(t) && TECH.is_match(t) {
        return Grade::Scene;
    }
    if t.len() >= 12
        && words >= 2
        && (TAIL.is_match(t) || TV.is_match(t) || QUALITY.is_match(t) || XXX.is_match(t) || is_season_pack(t))
    {
        return Grade::Good;
    }
    if t.len() >= 8 && words >= 2 {
        return Grade::Weak;
    }
    Grade::Junk
}
pub fn is_episode_name(n: &str) -> bool {
    EPISODE.is_match(n)
}
pub fn is_season_pack(n: &str) -> bool {
    !is_episode_name(n) && (SEASON_TOKEN.is_match(n) || SEASON_WORD.is_match(n) || COMPLETE.is_match(n))
}
pub fn renames_pack_to_episode(current: &str, new: &str) -> bool {
    is_season_pack(current) && is_episode_name(new) && !is_season_pack(new)
}
/// How many different SxxEyy or NxMM markers these names carry.
pub fn distinct_episodes<'a>(names: impl Iterator<Item = &'a str>) -> usize {
    let mut seen = HashSet::new();
    for name in names {
        if let Some(c) = EPISODE_ID.captures(name) {
            seen.insert(c[1].to_lowercase().replace(['.', '_', ' '], ""));
        }
    }
    seen.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn judge_table() {
        for n in [
            "Some.Show.S01E01.720p.HDTV.x264-GRP",
            "Movie.2024.1080p.WEB-DL.DDP5.1.H.264-GRP",
            "Artist-Album-WEB-2024-FLAC-GRP",
        ] {
            assert_eq!(judge(n), Grade::Scene, "{n}");
        }
        for n in [
            "grp-sometitle-1080p",
            "VA-Best_Of_Rock-2CD-2019-GRP",
            "Game.Name-CODEX",
            "Book.Title.2020.RETAIL.EPUB-GRP",
            "Cool Movie 2024 1080p",
            "Some Show S01E01",
            "Some.Show.S02.Complete",
            "Cool.Movie.2024",
        ] {
            assert_eq!(judge(n), Grade::Good, "{n}");
        }
        for n in ["Real Name", "Some Book Title"] {
            assert_eq!(judge(n), Grade::Weak, "{n}");
        }
        for n in [
            "a1B2c3D4e5F6g7H8",
            "kP93xQ7rL.ZMtwBfa",
            "Xk3Mz9PqL",
            "Inception",
            "sample",
            "setup.exe",
            "setup",
            "track01",
            "VTS_01_1",
            "Foo.Bar.part01",
            "Foo.Bar.vol03+04",
            "abc def",
            "123",
            "---",
            "a/b/c.mkv",
            "",
        ] {
            assert_eq!(judge(n), Grade::Junk, "{n}");
        }
    }

    #[test]
    fn season_rules() {
        assert!(renames_pack_to_episode("Show.S02.1080p.WEB-GRP", "Show.S02E03.1080p.WEB-GRP"));
        assert!(!renames_pack_to_episode("Show.S02E01.1080p.WEB-GRP", "Show.S02E03.1080p.WEB-GRP"));
        assert!(!renames_pack_to_episode("Xk3Mz9PqL", "Show.S02E03.1080p.WEB-GRP"));
        assert_eq!(distinct_episodes(["a.S01E01.x", "a.S01E02.x", "a.S01E01.y"].into_iter()), 2);
    }
}
