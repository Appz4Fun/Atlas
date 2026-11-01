//! A show or movie's title, season, episode, air date and quality, from its release name.
//!
//! The shapes are ported from newznab-tmux `app/Services/TvProcessing/Providers/AbstractTvProvider.php`
//! and `app/Services/MovieService.php`, GPLv3.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::{Captures, Regex};

const ALIASES: &str = include_str!("../../data/tv_title_aliases.tsv");

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub title: String,
    pub year: Option<i32>,
    pub season: Option<i32>,
    pub episode: Option<i32>,
    pub episode_last: Option<i32>,
    pub air_date: Option<i32>,
    pub pack: bool,
    pub quality: i32,
}

macro_rules! re {
    ($name:ident, $p:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($p).unwrap());
    };
}

re!(T1, r"(?i)^(.*?)[^a-z0-9]s([0-9]{1,2})[^a-z0-9]?e([0-9]{1,3})(?:-?e|-)([0-9]{1,3})(?:[^a-z0-9]|$)");
re!(T2, r"(?i)^(.*?)[^a-z0-9]s([0-9]{2})[^a-z0-9]?e([0-9]{2})e?([0-9]{2})(?:[^a-z0-9]|$)");
re!(T3, r"(?i)^(.*?)[^a-z0-9]s([0-9]{1,4})[^a-z0-9]?e([0-9]{1,3})[abr]?(?:v[0-9])?(?:[^a-z0-9]|$)");
re!(T4, r"(?i)^(.*?)[^a-z0-9]s([0-9]{1,2})[^a-z0-9]?d[0-9](?:[^a-z0-9]|$)");
re!(T5, r"(?i)^(.*?)[^a-z0-9]s([0-9]{1,2})(?:[^a-z0-9]|$)");
re!(T6, r"(?i)^(.*?)[^a-z0-9]([0-9]{1,2})x([0-9]{1,3})(?:[^a-z0-9]|$)");
re!(T7, r"^(.*?)[^a-zA-Z0-9]((?:19|20)[0-9]{2})[./-]([0-9]{2})[./-]([0-9]{2})(?:[^a-zA-Z0-9]|$)");
re!(T8, r"^(.*?)[^a-zA-Z0-9]([0-9]{2})([./-])([0-9]{2})([./-])((?:19|20)[0-9]{2})(?:[^a-zA-Z0-9]|$)");
re!(T9, r"(?i)^(.*?)[^a-z0-9](20[0-9]{2})[^a-z0-9]E([0-9]{1,3})(?:[^0-9]|$)");
re!(T10, r"(?i)^(.*?)[^a-z0-9]((?:19|20)[0-9]{2})[^a-z0-9]Part([0-9]{1,2})(?:[^a-z0-9]|$)");
re!(T11, r"(?i)^(.*?)[^a-z0-9](?:Part|Pt)[^a-z0-9]([0-9]{1,2})(?:[^a-z0-9]|$)");
re!(T12, r"(?i)^(.*?)[^a-z0-9]EP?[^a-z0-9]?([0-9]{1,3})(?:[^a-z0-9]|$)");
re!(T13, r"^(.*?)[_ ][-_][_ ]([0-9]{1,3})[_ ]*[\[(]");
re!(T14, r"(?i)^(.*?)[^a-z0-9]Seasons?[^a-z0-9]?([0-9]{1,2})(?:[^a-z0-9]|$)");

re!(GROUP_PREFIX, r"^\[[^\]]+\][_\s]*");
re!(JUNK_PREFIX, r"(?i)^(?:[^a-z0-9]{2,}|(?:sample|proof|repost)-)");
re!(
    CUT,
    r"(?i)[^a-z0-9](?:(?:480|576|720|1080|2160)[ip]|AAC2?|BD-?Rip|Blu-?Ray|DD5|DiVX|DLMux|DTS|DVD(?:-?Rip)?|[HX][-_. ]?26[45]|HEVC|[HPS]DTV|PROPER|REPACK|WEB[-_. ]?(?:DL|Rip)|XViD)(?:[^a-z0-9]|$)"
);
re!(YEAR, r"^(.+?)[^a-zA-Z0-9]\(?((?:19|20)[0-9]{2})\)?$");
re!(ACRONYM, r"\b((?:[A-Za-z]\.){2,}[A-Za-z]?\.?)");
re!(PARENS_OR_SEPARATOR, r"\([^)]*\)|[._]");
re!(
    LANGUAGE,
    r"(?i)\b(German|French|Spanish|Italian|Dutch|Russian|Japanese|Korean|Chinese|Portuguese|Swedish|Norwegian|Danish|Finnish|Polish|Czech|Hungarian|Romanian|Greek|Turkish|Arabic|Hebrew|Hindi|Thai|Vietnamese|Indonesian|Malay|Tagalog|DL|Dubbed|Subbed|MULTI)\b"
);
re!(TRAILING_HYPHEN, r"\s+-\s*$");
re!(SPACES, r"\s+");

const FOLLOW: &str = r"[^\w](?:(?:1080|480|720|2160)p|AC3D|Directors(?:[^\w]CUT)?|DD5\.1|(?:DVD|BD|BR|UHD)(?:Rip)?|BluRay|divx|HDTV|iNTERNAL|LiMiTED|(?:Real\.)?PROPER|RE(?:pack|Rip)|Sub\.?(?:fix|pack)|Unrated|WEB-?DL|WEBRip|(?:x|H|HEVC)[ ._-]?26[45]|xvid|AAC|REMUX)[^\w]";
re!(M1, r"(?i)([\w. '&!,:-]+)[^\w]((?:19|20)[0-9]{2})(?:[^\w]|$)");
static M2: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?i)^(?:[^\w]{{2,}})?([\w .'&!,:-]+?){FOLLOW}")).unwrap());
static FOLLOW_ANY: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!("(?i){FOLLOW}")).unwrap());
re!(MOVIE_FILE, r"(?i)\.(rar|zip|avi|mkv|mp4)$");
re!(GROUP_TAIL, r"-[A-Za-z0-9]+$");
re!(MOVIE_BRACKETS, r"\([^)]*\)|\[[^\]]*\]");

re!(RES_2160, r"(?i)(?:^|[^a-z0-9])(2160p|4k|uhd)(?:[^a-z0-9]|$)");
re!(RES_1080, r"(?i)(?:^|[^a-z0-9])(1080[pi])(?:[^a-z0-9]|$)");
re!(RES_720, r"(?i)(?:^|[^a-z0-9])(720p)(?:[^a-z0-9]|$)");
re!(RES_SD, r"(?i)(?:^|[^a-z0-9])(480p|576p)(?:[^a-z0-9]|$)");
re!(SRC_BLURAY, r"(?i)(?:^|[^a-z0-9])(blu-?ray|bdrip|brrip|remux)(?:[^a-z0-9]|$)");
re!(SRC_WEB, r"(?i)(?:^|[^a-z0-9])(web(?:[-._ ]?(?:dl|rip))?)(?:[^a-z0-9]|$)");
re!(SRC_TV, r"(?i)(?:^|[^a-z0-9])([hp]dtv)(?:[^a-z0-9]|$)");
re!(SRC_DVD, r"(?i)(?:^|[^a-z0-9])(dvd(?:rip|r|5|9)?)(?:[^a-z0-9]|$)");

static ALIAS: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| ALIASES.lines().filter_map(|l| l.split_once('\t')).collect());

/// how many aliases loaded, for a test
#[doc(hidden)]
pub fn alias_count() -> usize {
    ALIAS.len()
}

fn quality(name: &str) -> i32 {
    let resolution = if RES_2160.is_match(name) {
        4
    } else if RES_1080.is_match(name) {
        3
    } else if RES_720.is_match(name) {
        2
    } else if RES_SD.is_match(name) {
        1
    } else {
        0
    };
    let source = if SRC_BLURAY.is_match(name) {
        1
    } else if SRC_WEB.is_match(name) {
        2
    } else if SRC_TV.is_match(name) {
        3
    } else if SRC_DVD.is_match(name) {
        4
    } else {
        0
    };
    resolution | source << 4
}

/// The title and year in what comes before a marker.
fn clean_title(prefix: &str) -> Option<(String, Option<i32>)> {
    let s = GROUP_PREFIX.replace(prefix, "");
    let s = JUNK_PREFIX.replace(&s, "");
    let s = match CUT.find(&s) {
        Some(m) => &s[..m.start()],
        None => s.as_ref(),
    };
    let (s, year) = match YEAR.captures(s) {
        Some(c) => (c.get(1).map_or("", |m| m.as_str()), c[2].parse().ok()),
        None => (s, None),
    };
    let s = ACRONYM.replace_all(s, |c: &Captures<'_>| c[1].replace('.', ""));
    let s = PARENS_OR_SEPARATOR.replace_all(&s, " ");
    let s = LANGUAGE.replace_all(&s, "");
    let s = TRAILING_HYPHEN.replace(&s, "");
    let s = SPACES.replace_all(&s, " ");
    let s = s.trim().trim_matches(['"', '-']).trim();
    if s.chars().filter(|c| c.is_alphanumeric()).count() < 2 {
        return None;
    }
    let title =
        ALIAS.get(s.to_lowercase().as_str()).map_or_else(|| s.to_string(), |canonical| (*canonical).to_string());
    Some((title, year))
}

fn int(c: &Captures<'_>, i: usize) -> Option<i32> {
    c.get(i).and_then(|m| m.as_str().parse().ok())
}

fn real_date(y: i32, m: i32, d: i32) -> bool {
    u32::try_from(m)
        .ok()
        .zip(u32::try_from(d).ok())
        .is_some_and(|(m, d)| chrono::NaiveDate::from_ymd_opt(y, m, d).is_some())
}

struct Marker<'a> {
    prefix: &'a str,
    season: Option<i32>,
    episode: Option<i32>,
    last: Option<i32>,
    air_date: Option<i32>,
    pack: bool,
}

fn marker<'a>(c: &Captures<'a>, season: Option<i32>, episode: Option<i32>) -> Marker<'a> {
    Marker {
        prefix: c.get(1).map_or("", |m| m.as_str()),
        season,
        episode,
        last: None,
        air_date: None,
        pack: episode.is_none(),
    }
}

fn dated<'a>(c: &Captures<'a>, y: i32, m: i32, d: i32) -> Marker<'a> {
    Marker { air_date: Some(y * 10_000 + m * 100 + d), ..marker(c, Some(y), Some(m * 100 + d)) }
}

fn find_marker(name: &str) -> Option<Marker<'_>> {
    for rule in [&*T1, &*T2] {
        if let Some(c) = rule.captures(name) {
            let (episode, last) = (int(&c, 3), int(&c, 4));
            let last = last.filter(|l| episode.is_some_and(|e| *l > e));
            return Some(Marker { last, ..marker(&c, int(&c, 2), episode) });
        }
    }
    if let Some(c) = T3.captures(name) {
        return Some(marker(&c, int(&c, 2), int(&c, 3)));
    }
    if let Some(c) = T4.captures(name).or_else(|| T5.captures(name)) {
        return Some(marker(&c, int(&c, 2), None));
    }
    if let Some(c) = T6.captures(name) {
        return Some(marker(&c, int(&c, 2), int(&c, 3)));
    }
    if let Some(c) = T7.captures(name)
        && let (Some(y), Some(m), Some(d)) = (int(&c, 2), int(&c, 3), int(&c, 4))
        && real_date(y, m, d)
    {
        return Some(dated(&c, y, m, d));
    }
    if let Some(c) = T8.captures(name)
        && c[3] == c[5]
    {
        let (a, b, y) = (int(&c, 2)?, int(&c, 4)?, int(&c, 6)?);
        let (m, d) = if a > 12 {
            (b, a)
        } else if b > 12 {
            (a, b)
        } else if &c[3] == "." {
            (b, a)
        } else {
            (a, b)
        };
        return real_date(y, m, d).then(|| dated(&c, y, m, d));
    }
    if let Some(c) = T9.captures(name).or_else(|| T10.captures(name)) {
        return Some(marker(&c, int(&c, 2), int(&c, 3)));
    }
    if let Some(c) = T11.captures(name).or_else(|| T12.captures(name)).or_else(|| T13.captures(name)) {
        return Some(marker(&c, Some(1), int(&c, 2)));
    }
    if let Some(c) = T14.captures(name) {
        return Some(marker(&c, int(&c, 2), None));
    }
    None
}

pub fn parse_tv(name: &str) -> Option<Parsed> {
    let m = find_marker(name)?;
    let (title, year) = clean_title(m.prefix)?;
    Some(Parsed {
        title,
        year,
        season: m.season,
        episode: m.episode,
        episode_last: m.last,
        air_date: m.air_date,
        pack: m.pack,
        quality: quality(name),
    })
}

pub fn parse_movie(name: &str) -> Option<Parsed> {
    let spaced = SPACES.replace_all(&name.replace(['.', '_'], " "), " ").into_owned();
    let (raw, year): (String, Option<i32>) = if let Some(c) = M1.captures(&spaced) {
        (c[1].to_string(), c[2].parse().ok())
    } else if let Some(c) = M2.captures(&format!("{spaced} ")) {
        (c[1].to_string(), None)
    } else if name.len() < 101 && !MOVIE_FILE.is_match(name) {
        (GROUP_TAIL.replace(&spaced, "").into_owned(), None)
    } else {
        return None;
    };
    let cleaned = FOLLOW_ANY.replace_all(&format!(" {raw} "), " ").into_owned();
    let cleaned = MOVIE_BRACKETS.replace_all(&cleaned, " ");
    let title = SPACES.replace_all(&cleaned, " ");
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    if year.is_none() && (title.len() <= 2 || title.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    Some(Parsed { title: title.to_string(), year, quality: quality(name), ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    type Tv<'a> = (&'a str, &'a str, Option<i32>, i32, Option<i32>, Option<i32>, Option<i32>, bool, i32);

    #[test]
    fn parse_tv_table() {
        let table: &[Tv<'_>] = &[
            ("Some.Show.S01E02.720p.HDTV.x264-GRP", "Some Show", None, 1, Some(2), None, None, false, 50),
            ("Some.Show.S01E02E03.720p.WEB.x264-GRP", "Some Show", None, 1, Some(2), Some(3), None, false, 34),
            ("Some.Show.S01E02-E04.1080p.WEB.x264-GRP", "Some Show", None, 1, Some(2), Some(4), None, false, 35),
            ("Some.Show.S01E02-03.720p.WEB-GRP", "Some Show", None, 1, Some(2), Some(3), None, false, 34),
            ("Some.Show.S01E0203.720p.WEB-GRP", "Some Show", None, 1, Some(2), Some(3), None, false, 34),
            ("Some.Show.S01E02-720p.WEB-GRP", "Some Show", None, 1, Some(2), None, None, false, 34),
            ("The.100.S01E02.720p.HDTV.x264-GRP", "The 100", None, 1, Some(2), None, None, false, 50),
            ("Station.19.S01E02.720p.HDTV.x264-GRP", "Station 19", None, 1, Some(2), None, None, false, 50),
            ("Polizeiruf.110.S01E02.720p.HDTV.x264-GRP", "Polizeiruf 110", None, 1, Some(2), None, None, false, 50),
            ("Some.Show.S01.1080p.BluRay.x264-GRP", "Some Show", None, 1, None, None, None, true, 19),
            ("Some.Show.S01.COMPLETE.720p.WEB-GRP", "Some Show", None, 1, None, None, None, true, 34),
            ("Some.Show.Season.3.1080p.WEB-GRP", "Some Show", None, 3, None, None, None, true, 35),
            ("Some.Show.S03D2.DVDR-GRP", "Some Show", None, 3, None, None, None, true, 64),
            ("Some.Show.1x05.HDTV.XviD-GRP", "Some Show", None, 1, Some(5), None, None, false, 48),
            (
                "Some.Show.2024.03.15.Guest.Name.720p.WEB-GRP",
                "Some Show",
                None,
                2024,
                Some(315),
                None,
                Some(20240315),
                false,
                34,
            ),
            ("Some.Show.2024-03-15.720p.WEB-GRP", "Some Show", None, 2024, Some(315), None, Some(20240315), false, 34),
            ("Some.Show.15.03.2024.720p.WEB-GRP", "Some Show", None, 2024, Some(315), None, Some(20240315), false, 34),
            ("Some.Show.03-15-2024.720p.WEB-GRP", "Some Show", None, 2024, Some(315), None, Some(20240315), false, 34),
            ("Some.Show.2019.S02E03.1080p.WEB-GRP", "Some Show", Some(2019), 2, Some(3), None, None, false, 35),
            ("Some.Show.US.S02E03.1080p.WEB-GRP", "Some Show US", None, 2, Some(3), None, None, false, 35),
            ("Some.Show.S2019E03.720p.WEB-GRP", "Some Show", None, 2019, Some(3), None, None, false, 34),
            ("Some.Show.2023.E05.720p.WEB-GRP", "Some Show", None, 2023, Some(5), None, None, false, 34),
            ("Some.Show.Part.3.720p.HDTV-GRP", "Some Show", None, 1, Some(3), None, None, false, 50),
            ("Some.Show.2015.Part2.720p.HDTV-GRP", "Some Show", None, 2015, Some(2), None, None, false, 50),
            ("Some.Show.EP06.Title.DVDRip.XviD-GRP", "Some Show", None, 1, Some(6), None, None, false, 64),
            ("[SubsPlease] Some Show - 08 (1080p) [ABCD1234].mkv", "Some Show", None, 1, Some(8), None, None, false, 3),
            ("[Grp]_Some_Show_-_12_[720p].mkv", "Some Show", None, 1, Some(12), None, None, false, 2),
            ("Some Show - S02E01 - Episode Title (1080p)", "Some Show", None, 2, Some(1), None, None, false, 3),
            ("S.W.A.T.S01E02.720p.HDTV-GRP", "SWAT", None, 1, Some(2), None, None, false, 50),
            ("Détective.Privé.S01E02.GERMAN.720p.WEB-GRP", "Détective Privé", None, 1, Some(2), None, None, false, 34),
            ("Some.Show.GERMAN.DL.S01E02.720p.WEB-GRP", "Some Show", None, 1, Some(2), None, None, false, 34),
            ("Some.Show.PROPER.S01E02.720p.WEB-GRP", "Some Show", None, 1, Some(2), None, None, false, 34),
            ("Some.Show.S01E02v2.720p.WEB-GRP", "Some Show", None, 1, Some(2), None, None, false, 34),
            ("sample-Some.Show.S01E02.720p.WEB-GRP", "Some Show", None, 1, Some(2), None, None, false, 34),
        ];
        let mut wrong = Vec::new();
        for (name, title, year, season, episode, last, air, pack, q) in table {
            let want = Parsed {
                title: title.to_string(),
                year: *year,
                season: Some(*season),
                episode: *episode,
                episode_last: *last,
                air_date: *air,
                pack: *pack,
                quality: *q,
            };
            let got = parse_tv(name);
            if got.as_ref() != Some(&want) {
                wrong.push(format!("{name}: {got:?}"));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
        for name in ["Some.Show.2024.13.45.720p.WEB-GRP", "Some Movie Without Markers", "Fast.Sixth.1080p.WEB-GRP"] {
            assert_eq!(parse_tv(name), None, "{name}");
        }
    }

    #[test]
    fn parse_movie_table() {
        let table: &[(&str, &str, Option<i32>, i32)] = &[
            ("Some.Movie.2019.1080p.BluRay.x264-GRP", "Some Movie", Some(2019), 19),
            ("Some.Sequel.2049.2017.1080p.BluRay.x264-GRP", "Some Sequel 2049", Some(2017), 19),
            ("2001.A.Space.Story.1968.1080p.BluRay.x264-GRP", "2001 A Space Story", Some(1968), 19),
            ("Anti-Hero.2002.1080p.BluRay.x264-GRP", "Anti-Hero", Some(2002), 19),
            ("Some.Movie.(2019).720p.WEB-DL", "Some Movie", Some(2019), 34),
            ("Some Movie 2019 1920x1080 x264", "Some Movie", Some(2019), 0),
            ("Some.Movie.1080p.BluRay.x264-GRP", "Some Movie", None, 19),
            ("Some.Movie.DVDRip.XviD-GRP", "Some Movie", None, 64),
            ("Some.Movie-GRP", "Some Movie", None, 0),
            ("Some Film [2015] [BluRay] [1080p]", "Some Film", Some(2015), 19),
            ("1984.2019.2160p.UHD.BluRay.x265-GRP", "1984", Some(2019), 20),
            ("Some_Movie_2021_WEB-DL", "Some Movie", Some(2021), 32),
            ("12.2007.1080p.BluRay-GRP", "12", Some(2007), 19),
            ("Some.Movie.2019.German.DL.1080p.BluRay.x264-GRP", "Some Movie", Some(2019), 19),
            ("Director's.Cut.Story.1999.PROPER.DVDRip.XviD-GRP", "Director's Cut Story", Some(1999), 64),
        ];
        let mut wrong = Vec::new();
        for (name, title, year, q) in table {
            let got = parse_movie(name);
            let want = Parsed { title: title.to_string(), year: *year, quality: *q, ..Default::default() };
            if got.as_ref() != Some(&want) {
                wrong.push(format!("{name}: {got:?}"));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn an_alias_gives_the_canonical_title() {
        assert_eq!(parse_tv("Daily.Show.S01E02.720p.WEB-GRP").unwrap().title, "The Daily Show");
    }

    #[test]
    fn the_aliases_loaded() {
        assert!(alias_count() > 30, "{}", alias_count());
    }
}
