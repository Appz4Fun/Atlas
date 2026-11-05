//! The name a movie or an episode is shown under: TRaSH Guides' standard Radarr and Sonarr naming, built from what
//! the release name says and the title TMDB knows it by.
//!
//! `AC317772.2026.MULTi.VFF.AD.2160p.HDR.DV.WEB.EAC3.5.1.Atmos.H265-FRQC` is shown as
//! `Matchbox the Movie (2026) [WEB-DL-2160p][DV HDR10][EAC3 Atmos 5.1][MULTi][H265]-FRQC`, and
//! `The.Bear.S05E07.HDR.2160p.WEB.h265-ETHEL` as `The Bear (2022) - S05E07 [WEB-DL-2160p][HDR10][H265]-ETHEL`.
//! Only the title and the year can come from elsewhere: every tag is one the name has. A name with no title to show
//! (a hash, an AlloCiné number no lookup has answered yet) has none, and is shown as it is.

use std::sync::LazyLock;

use regex::Regex;

use crate::category::table::{is_movie, is_tv};
use crate::media::details::Details;
use crate::media::query::MediaAttrs;
use crate::media::title::Kind;
use crate::release_tags::{self, Tags};

/// `-AsRequested`, `-Obfuscated` and the like: what reposting tools add after the group, as Sonarr strips them.
static REPOST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)-(?:RP|NZBGeek|Obfuscated|Scrambled|sample|Pre|postbot|xpost|Rakuv[a-z0-9]*|WhiteRev|BUYMORE|AsRequested|AlternativeToRequested|GEROV|Z0iDS3N|Chamele0n|4P|4Planet|AlteZachen|RePACKPOST|mediainfo)$",
    )
    .unwrap()
});
/// `NBA__NZBSPLIT__<hash>__NZBSPLIT__<name>`: an upload split by NZBSplit names what it holds last
static NZBSPLIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)__NZBSPLIT__[a-f0-9]{8,}__NZBSPLIT__(.+)$").unwrap());
/// ` [12345678]`: the counter a second post of a taken name gets
static COUNTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*\[\d{8}\]$").unwrap());
static EPISODE_CODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bS\d{1,4}[ ._-]?E\d{1,4}(?:[ ._-]?(?:E|-E?)\d{1,4})*\b|\b\d{1,2}x\d{2,3}\b").unwrap()
});
static MULTI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bmulti\b|\bdual[ ._-]*audio\b").unwrap());
/// `MULTi.Subs`, `MultiSub`: many subtitles, one language heard
static MULTI_SUBS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bmulti[ ._-]*sub\w*").unwrap());
static DTS_X: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bDTS[ ._:-]?X\b").unwrap());
static TRUEHD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)TrueHD").unwrap());
static DTS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bDTS(?:[-. ]?HD(?:[-. ]?MA)?|[-. :]?X)?").unwrap());
static EAC3: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bDDP|\bDD\+|\bE-?AC-?3").unwrap());
static AC3: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bDD|\bAC-?3").unwrap());
static FLAC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bFLAC").unwrap());
static PCM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bL?PCM").unwrap());
static OPUS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bOpus").unwrap());
static AAC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bAAC").unwrap());
/// `.7.1`, `5.1`, `.Atmos.5.1`, ` 2 0`: a count straight after a codec, an Atmos in between at most
static CHANNELS_AFTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[ ._-]?(?:Atmos[ ._-])?([1-9])[ ._]([0-2])(?:[^0-9]|$)").unwrap());
static X26: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bx26([45])\b").unwrap());
static TITLE_JUNK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"[:/\\?*"<>|]"#).unwrap());
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
/// words an episode code is followed by that aren't the episode's title
static NOT_AN_EPISODE_TITLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:final|finale|internal|proper|repack|real|readnfo|dubbed|subbed|uncut|extended|complete|multi)$",
    )
    .unwrap()
});

/// What Atlas knows of a release beyond its name.
#[derive(Clone, Copy, Debug, Default)]
pub struct Known<'a> {
    pub kind: Option<i64>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    /// yyyymmdd, for a daily show
    pub air_date: Option<i64>,
    pub year: Option<i64>,
    /// TMDB's title
    pub title: Option<&'a str>,
    /// TMDB's release or first-air date, `2022-06-23`
    pub released: Option<&'a str>,
}

impl<'a> Known<'a> {
    pub fn of(media: Option<&MediaAttrs>, details: Option<&'a Details>) -> Known<'a> {
        Known {
            kind: media.map(|m| m.kind),
            season: media.and_then(|m| m.season),
            episode: media.and_then(|m| m.episode),
            air_date: media.and_then(|m| m.air_date),
            year: media.and_then(|m| m.year),
            title: details.and_then(|d| d.title.as_deref()),
            released: details.and_then(|d| d.released.as_deref()),
        }
    }
}

/// The name a release in a movie or TV category is shown under, or None to show it as it is.
pub fn name(release: &str, category: i64, known: &Known) -> Option<String> {
    if !is_movie(category) && !is_tv(category) {
        return None;
    }
    let release = clean_release(release);
    if crate::parser::is_obfuscated(&release) {
        return None;
    }
    let tags = release_tags::parse(&release);
    let tv = match known.kind {
        Some(kind) => kind == Kind::Tv as i64,
        None => is_tv(category),
    };
    let title = title(&tags, known)?;
    // a title only the name gives needs the name to look like a release: a source, not just words and a number
    if known.title.is_none() && tags.quality.is_none() {
        return None;
    }
    let mut out =
        if tv { episode_heading(&release, &tags, known, &title)? } else { movie_heading(&tags, known, &title)? };
    out.push(' ');
    out.push_str(&specs(&release, &tags));
    if let Some(group) = tags.group.as_deref().filter(|g| !g.is_empty()) {
        out.push('-');
        out.push_str(group);
    }
    Some(out.trim_end().to_string())
}

/// No file suffix, collision counter or repost tag: what the release itself is called.
fn clean_release(release: &str) -> String {
    let release = COUNTER.replace(release.trim(), "");
    let release = NZBSPLIT.captures(&release).map_or(release.to_string(), |c| c[1].to_string());
    let mut s = crate::naming::clean::strip_file_suffixes(&release);
    loop {
        let next = REPOST.replace(&s, "").into_owned();
        if next == s {
            return s;
        }
        s = next;
    }
}

fn title(tags: &Tags, known: &Known) -> Option<String> {
    let from_name = || {
        let t = tags.title.trim();
        (crate::media::allocine::allocine_id(t).is_none() && t.chars().any(char::is_alphabetic)).then_some(t)
    };
    let title = known.title.filter(|t| !t.trim().is_empty()).or_else(from_name)?;
    // what a file name can't hold goes, as Radarr's clean title drops it
    let title = TITLE_JUNK.replace_all(title, " ");
    let title = SPACES.replace_all(title.trim(), " ").into_owned();
    (!title.is_empty()).then_some(title)
}

fn year_of(date: Option<&str>) -> Option<i64> {
    date.and_then(|d| d.get(..4)).and_then(|y| y.parse().ok()).filter(|y| (1870..=2100).contains(y))
}

/// `Matchbox the Movie (2026)`: a movie needs its year, the matched movie's first, as Radarr names it
fn movie_heading(tags: &Tags, known: &Known, title: &str) -> Option<String> {
    let matched = known.title.and(year_of(known.released));
    let year = matched.or(known.year).or(tags.year.map(i64::from))?;
    let mut out = format!("{title} ({year})");
    if let Some(edition) = &tags.edition {
        out.push_str(&format!(" {{edition-{edition}}}"));
    }
    Some(out)
}

/// `The Bear (2022) - S05E07 - Episode Title`, `CSI NY (2004) - S01`, `Conan O Brien - 2009-09-16`
fn episode_heading(release: &str, tags: &Tags, known: &Known, title: &str) -> Option<String> {
    let year = year_of(known.released).or(tags.year.map(i64::from));
    let mut out = match year {
        Some(year) if !title.ends_with(&format!("({year})")) => format!("{title} ({year})"),
        _ => title.to_string(),
    };
    if let Some(date) = known.air_date.filter(|d| *d > 10_000_000) {
        let (y, m, d) = (date / 10000, date / 100 % 100, date % 100);
        out = format!("{title} - {y:04}-{m:02}-{d:02}");
        return Some(out);
    }
    let season = known.season.or_else(|| tags.seasons.first().copied())?;
    let first = known.episode.or_else(|| tags.episodes.first().copied());
    match first {
        Some(first) => {
            out.push_str(&format!(" - S{season:02}E{first:02}"));
            // a range only when the name's episodes start where the lookup's does
            if tags.episodes.first() == Some(&first)
                && let Some(last) = tags.episodes.last().filter(|last| **last > first)
            {
                out.push_str(&format!("-E{last:02}"));
            }
            if let Some(name) = episode_title(release) {
                out.push_str(" - ");
                out.push_str(&name);
            }
        }
        None => {
            out.push_str(&format!(" - S{season:02}"));
            // a pack of several seasons: `S01-S03`
            if tags.seasons.first() == Some(&season)
                && let Some(last) = tags.seasons.last().filter(|last| **last > season)
            {
                out.push_str(&format!("-S{last:02}"));
            }
        }
    }
    Some(out)
}

/// What a name says after its episode code, before its tags: `Love and Be Loved` in
/// `AC31594.2024.S02E05.Love.and.Be.Loved.MULTi.1080p.WEB.H264-BYOR`.
fn episode_title(release: &str) -> Option<String> {
    let code = EPISODE_CODE.find(release)?;
    let rest = release[code.end()..].trim_start_matches([' ', '.', '_', '-']);
    if rest.is_empty() {
        return None;
    }
    // PTT keeps a name's first word as its title whatever it is, soo `iTALiAN` would be one: a stand-in goes first
    let title = crate::ptt::parse(&format!("Zq.{rest}")).title;
    let title = title.strip_prefix("Zq").unwrap_or("").trim_start_matches(['.', '_', '-']);
    let title = TITLE_JUNK.replace_all(title.trim(), " ");
    let title = SPACES.replace_all(title.trim(), " ").into_owned();
    let worth_it = title.chars().any(char::is_alphabetic)
        && title.chars().count() <= 80
        && !title.split(' ').all(|w| NOT_AN_EPISODE_TITLE.is_match(w));
    worth_it.then_some(title)
}

/// `[WEB-DL-2160p][DV HDR10][EAC3 Atmos 5.1][MULTi][H265]`
fn specs(release: &str, tags: &Tags) -> String {
    let mut out = String::new();
    let mut bracket = |text: String| {
        if !text.is_empty() {
            out.push('[');
            out.push_str(&text);
            out.push(']');
        }
    };
    bracket(quality(tags));
    if tags.three_d.is_some() {
        bracket("3D".to_string());
    }
    bracket(dynamic_range(tags));
    bracket(audio(release, tags));
    bracket(languages(release, tags));
    bracket(codec(release, tags));
    out
}

/// Radarr's quality, `Bluray-1080p`, `Remux-2160p`, with how the name spells WEB-DL
fn quality(tags: &Tags) -> String {
    let resolution = tags.resolution.as_deref().filter(|r| r.ends_with('p'));
    let height = resolution.and_then(|r| r.trim_end_matches('p').parse::<u32>().ok());
    let source = tags.quality.as_deref().map(|q| match q {
        "WEB" | "WEB-DL" | "WEBMux" => ("WEB-DL", true),
        "WEBRip" | "WEB-DLRip" | "UHDRip" => ("WEBRip", true),
        "BluRay REMUX" | "REMUX" => ("Remux", true),
        "BluRay" | "BDRip" | "BRRip" => ("Bluray", true),
        "HDTV" | "HDTVRip" | "PDTV" | "SATRip" | "TVRip" | "PPVRip" => {
            (if height.is_some_and(|h| h >= 720) { "HDTV" } else { "SDTV" }, height.is_some_and(|h| h >= 720))
        }
        "DVD" | "DVDRip" => ("DVD", false),
        "TeleSync" => ("TELESYNC", false),
        "TeleCine" => ("TELECINE", false),
        "SCR" => ("DVDSCR", false),
        "R5" => ("REGIONAL", false),
        other => (other, false),
    });
    let mut out = match (source, resolution) {
        (Some((source, true)), Some(resolution)) => format!("{source}-{resolution}"),
        (Some((source, _)), _) => source.to_string(),
        (None, Some(resolution)) => resolution.to_string(),
        (None, None) => String::new(),
    };
    if !out.is_empty() {
        if tags.repack {
            out.push_str(" REPACK");
        } else if tags.proper {
            out.push_str(" Proper");
        }
    }
    out
}

/// `DV HDR10`: what Radarr writes for the dynamic range, Dolby Vision first
fn dynamic_range(tags: &Tags) -> String {
    let mut out = Vec::new();
    if tags.hdr.iter().any(|h| h == "DV") {
        out.push("DV");
    }
    if tags.hdr.iter().any(|h| h == "HDR10+") {
        out.push("HDR10+");
    } else if tags.hdr.iter().any(|h| h == "HDR") {
        out.push("HDR10");
    }
    out.join(" ")
}

/// `EAC3 Atmos 5.1`: the main codec, Atmos, the channels
fn audio(release: &str, tags: &Tags) -> String {
    let has = |name: &str| tags.audio.iter().any(|a| a == name);
    let codec = if has("TrueHD") {
        Some("TrueHD")
    } else if has("DTS Lossless") {
        Some(if DTS_X.is_match(release) { "DTS-X" } else { "DTS-HD MA" })
    } else if has("Dolby Digital Plus") {
        Some("EAC3")
    } else if has("Dolby Digital") {
        Some("AC3")
    } else if has("DTS Lossy") {
        Some(if DTS_X.is_match(release) { "DTS-X" } else { "DTS" })
    } else if has("FLAC") {
        Some("FLAC")
    } else if has("PCM") {
        Some("PCM")
    } else if has("OPUS") {
        Some("Opus")
    } else if has("AAC") {
        Some("AAC")
    } else {
        None
    };
    let atmos = has("Atmos").then_some("Atmos");
    if codec.is_none() && atmos.is_none() {
        return String::new();
    }
    let channels = codec.and_then(|codec| channels_of(release, codec, tags));
    [codec, atmos, channels.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" ")
}

/// The channels of the track `codec` names: the count written right after it (`TrueHD.7.1`, `DDP5.1`,
/// `DD.Atmos.5.1`), else the only count the name has. A name with several tracks and no count by the codec has none.
fn channels_of(release: &str, codec: &str, tags: &Tags) -> Option<String> {
    let token: &Regex = match codec {
        "TrueHD" => &TRUEHD,
        "DTS-HD MA" | "DTS-X" | "DTS" => &DTS,
        "EAC3" => &EAC3,
        "AC3" => &AC3,
        "FLAC" => &FLAC,
        "PCM" => &PCM,
        "Opus" => &OPUS,
        _ => &AAC,
    };
    for found in token.find_iter(release) {
        // `DDP` and `DD+` are EAC3, not AC3
        if codec == "AC3" && release[found.end()..].starts_with(['P', 'p', '+']) {
            continue;
        }
        if let Some(c) = CHANNELS_AFTER.captures(&release[found.end()..]) {
            return Some(format!("{}.{}", &c[1], &c[2]));
        }
    }
    let mut counts: Vec<&str> = tags
        .channels
        .iter()
        .map(|c| match c.as_str() {
            "stereo" => "2.0",
            "mono" => "1.0",
            other => other,
        })
        .collect();
    counts.dedup();
    match counts.as_slice() {
        [only] => Some((*only).to_string()),
        _ => None,
    }
}

/// `MULTi`, or the one language a name is dubbed in: subtitles say nothing of what's heard
fn languages(release: &str, tags: &Tags) -> String {
    if MULTI.is_match(&MULTI_SUBS.replace_all(release, "")) {
        return "MULTi".to_string();
    }
    if tags.subbed || tags.languages.iter().all(|l| l == "English") {
        return String::new();
    }
    tags.languages.join("+")
}

/// `H265`, or `x265` when the name says it was encoded with x265
fn codec(release: &str, tags: &Tags) -> String {
    match tags.codec.as_deref() {
        Some("hevc") | Some("avc") => match X26.captures(release) {
            Some(c) => format!("x26{}", &c[1]),
            None if tags.codec.as_deref() == Some("hevc") => "H265".to_string(),
            None => "H264".to_string(),
        },
        Some("av1") => "AV1".to_string(),
        Some("xvid") => "XviD".to_string(),
        Some("mpeg") => "MPEG2".to_string(),
        Some(other) => other.to_uppercase(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOVIES_UHD: i64 = 2045;
    const MOVIES_HD: i64 = 2040;
    const TV_UHD: i64 = 5045;
    const TV_HD: i64 = 5040;
    const TV_SD: i64 = 5030;

    fn movie<'a>(title: &'a str, year: i64) -> Known<'a> {
        Known { kind: Some(Kind::Movie as i64), year: Some(year), title: Some(title), ..Known::default() }
    }

    fn show<'a>(title: &'a str, released: &'a str, season: i64, episode: Option<i64>) -> Known<'a> {
        Known {
            kind: Some(Kind::Tv as i64),
            season: Some(season),
            episode,
            title: Some(title),
            released: Some(released),
            ..Known::default()
        }
    }

    fn shown(release: &str, category: i64, known: Known) -> String {
        name(release, category, &known).unwrap_or_else(|| panic!("no name for {release}"))
    }

    #[test]
    fn an_allocine_movie_is_shown_under_its_tmdb_title() {
        assert_eq!(
            shown(
                "AC317772.2026.MULTi.VFF.AD.2160p.HDR.DV.WEB.EAC3.5.1.Atmos.H265-FRQC",
                MOVIES_UHD,
                movie("Matchbox the Movie", 2026)
            ),
            "Matchbox the Movie (2026) [WEB-DL-2160p][DV HDR10][EAC3 Atmos 5.1][MULTi][H265]-FRQC"
        );
    }

    #[test]
    fn an_allocine_number_no_lookup_answered_is_shown_as_it_is() {
        let name = "AC317772.2026.MULTi.VFF.AD.2160p.HDR.DV.WEB.EAC3.5.1.Atmos.H265-FRQC";
        assert_eq!(super::name(name, MOVIES_UHD, &Known::default()), None);
    }

    #[test]
    fn movies_from_prod() {
        assert_eq!(
            shown(
                "Avengers.Endgame.2019.UHD.BluRay.Remux.2160p.HEVC.TrueHD.7.1.Atmos-MTeam",
                MOVIES_UHD,
                movie("Avengers: Endgame", 2019)
            ),
            "Avengers Endgame (2019) [Remux-2160p][TrueHD Atmos 7.1][H265]-MTeam"
        );
        assert_eq!(
            shown(
                "Renfield.2023.REPACK.UHD.BluRay.1080p.DD.Atmos.5.1.DoVi.HDR10.x265-SM737",
                MOVIES_UHD,
                movie("Renfield", 2023)
            ),
            "Renfield (2023) [Bluray-1080p REPACK][DV HDR10][AC3 Atmos 5.1][x265]-SM737"
        );
        assert_eq!(
            shown(
                "Mistress.America.2015.1080p.BluRay.REMUX.AVC.DTS-HD.MA.5.1-EPSiLON-AsRequested.mkv",
                MOVIES_HD,
                movie("Mistress America", 2015)
            ),
            "Mistress America (2015) [Remux-1080p][DTS-HD MA 5.1][H264]-EPSiLON"
        );
        assert_eq!(
            shown(
                "Operation.Endgame.2010.German.AC3D.DL.720p.BluRay.x264-iNNOVATiV",
                2010,
                movie("Operation: Endgame", 2010)
            ),
            "Operation Endgame (2010) [Bluray-720p][German][x264]-iNNOVATiV"
        );
    }

    #[test]
    fn a_movie_with_no_lookup_keeps_the_title_it_was_posted_under() {
        assert_eq!(
            shown("Oorlogswinter.2008.BluRay.1080p.DTS.x264.dxva-EuReKA", MOVIES_HD, Known::default()),
            "Oorlogswinter (2008) [Bluray-1080p][DTS][x264]-EuReKA"
        );
        // no year, no lookup: nothing to tell it by
        assert_eq!(name("1080p.BD9.x264-RAP [39599202]", MOVIES_HD, &Known::default()), None);
    }

    #[test]
    fn episodes_from_prod() {
        assert_eq!(
            shown("The.Bear.S05E07.HDR.2160p.WEB.h265-ETHEL.mkv", TV_UHD, show("The Bear", "2022-06-23", 5, Some(7))),
            "The Bear (2022) - S05E07 [WEB-DL-2160p][HDR10][H265]-ETHEL"
        );
        assert_eq!(
            shown(
                "AC31594.2024.S02E05.Love.and.Be.Loved.MULTi.1080p.WEB.H264-BYOR",
                TV_HD,
                show("Dark Matter", "2024-05-08", 2, Some(5))
            ),
            "Dark Matter (2024) - S02E05 - Love and Be Loved [WEB-DL-1080p][MULTi][H264]-BYOR"
        );
        assert_eq!(
            shown(
                "Not.Suitable.for.Work.2026.S01E04.iTALiAN.MULTi.DV.HDR.2160p.WEB.H265-NTROPiC.mkv",
                TV_UHD,
                show("Not Suitable for Work", "2026-06-02", 1, Some(4))
            ),
            "Not Suitable for Work (2026) - S01E04 [WEB-DL-2160p][DV HDR10][MULTi][H265]-NTROPiC"
        );
        assert_eq!(
            shown("CSI.New.York.S01.DVDRip.XviD-WAT", TV_SD, show("CSI: NY", "2004-09-22", 1, None)),
            "CSI NY (2004) - S01 [DVD][XviD]-WAT"
        );
        assert_eq!(
            shown(
                "The.Greatest.2026.S01E08.FINAL.MULTI.1080p.WEB.H264-HiggsBoson",
                TV_HD,
                show("The Greatest", "2026-11-04", 1, Some(8))
            ),
            "The Greatest (2026) - S01E08 [WEB-DL-1080p][MULTi][H264]-HiggsBoson"
        );
    }

    #[test]
    fn a_matched_movie_takes_its_own_year_and_a_pack_its_seasons() {
        let known = Known {
            kind: Some(Kind::Movie as i64),
            year: Some(1981),
            title: Some("Pixote"),
            released: Some("1980-09-26"),
            ..Known::default()
        };
        assert_eq!(
            shown("Pixote.1981.iNTERNAL.DVDRip.x264.AAC-OiT", MOVIES_HD, known),
            "Pixote (1980) [DVD][AAC][x264]-OiT"
        );
        assert_eq!(
            shown("Season.Pack.S01-S03.1080p.AMZN.WEB-DL-GRP", TV_HD, show("Season Pack", "2020-01-01", 1, None)),
            "Season Pack (2020) - S01-S03 [WEB-DL-1080p]-GRP"
        );
        assert_eq!(
            shown("Movie.2020.1080p.BluRay.TrueHD.7.1.AC3.5.1.x264-GRP", MOVIES_HD, movie("Movie", 2020)),
            "Movie (2020) [Bluray-1080p][TrueHD 7.1][x264]-GRP"
        );
        // the count by the codec, wherever PTT lists it
        assert_eq!(
            shown("Movie.2020.1080p.BluRay.REMUX.TrueHD.7.1.Atmos.AC3.5.1-GRP", MOVIES_HD, movie("Movie", 2020)),
            "Movie (2020) [Remux-1080p][TrueHD Atmos 7.1]-GRP"
        );
        assert_eq!(
            shown("Movie.2020.1080p.WEB-DL.DDP5.1.AAC2.0.H.264-GRP", MOVIES_HD, movie("Movie", 2020)),
            "Movie (2020) [WEB-DL-1080p][EAC3 5.1][H264]-GRP"
        );
        // two counts and none by the codec: none
        assert_eq!(
            shown("Movie.2020.1080p.BluRay.TrueHD.Atmos.7.1.AC3.2.0.x264-GRP", MOVIES_HD, movie("Movie", 2020)),
            "Movie (2020) [Bluray-1080p][TrueHD Atmos 7.1][x264]-GRP"
        );
    }

    #[test]
    fn a_daily_show_goes_by_its_air_date() {
        let known = Known { kind: Some(Kind::Tv as i64), air_date: Some(20090916), ..Known::default() };
        assert_eq!(
            shown("conan.o.brien.2009.09.16.aaron.eckhart.720p.hdtv.x264-bajskorv", 5040, known),
            "conan o brien - 2009-09-16 [HDTV-720p][x264]-bajskorv"
        );
    }

    #[test]
    fn a_title_only_the_name_gives_needs_a_source() {
        assert_eq!(name("aaf-all.saints.s10e13.balancing.act", TV_SD, &Known::default()), None);
        assert_eq!(name("TSParty_s01_AnneAntonela.AryaSantana_2160p.by.am", TV_UHD, &Known::default()), None);
        assert_eq!(
            shown(
                "Archive X Unexplained Phenomena S01E06 Aliens and Demons 1080p AMZN WEB-DL DDP2 0 H 264-GRP.mkv",
                TV_HD,
                Known::default()
            ),
            "Archive X Unexplained Phenomena - S01E06 - Aliens and Demons [WEB-DL-1080p][EAC3 2.0][H264]-GRP"
        );
        assert_eq!(
            shown(
                "Oh.Gott.1977.German.AC3D.DL.1080p.BluRay.x264-paranoid06-mediainfo.txt",
                MOVIES_HD,
                Known::default()
            ),
            "Oh Gott (1977) [Bluray-1080p][German][x264]-paranoid06"
        );
    }

    #[test]
    fn only_movies_and_tv_and_never_a_hash() {
        assert_eq!(name("Some.Album.2024.FLAC-GRP", 3040, &Known::default()), None);
        assert_eq!(name("iVkO3PIXPQWID28OrPOBGRq3aQHcSGAo", MOVIES_UHD, &movie("Avatar", 2026)), None);
    }
}
