//! What a release name says about the release: resolution, source, HDR, codecs, group and so on.
//!
//! The rules follow PTT (parsett), which the Kodi add-on uses, soo both read a name the same way.
//! `regex` has no lookaround, soo the rules that need it are written as code. Names are parsed when
//! they're shown, and nothing is stored.

use std::sync::LazyLock;

use regex::{Regex, RegexBuilder};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tags {
    pub resolution: Option<String>,
    pub quality: Option<String>,
    pub hdr: Vec<&'static str>,
    pub codec: Option<&'static str>,
    pub bit_depth: Option<String>,
    pub audio: Vec<&'static str>,
    pub channels: Vec<&'static str>,
    pub network: Option<&'static str>,
    pub edition: Option<&'static str>,
    pub year: Option<i32>,
    pub group: Option<String>,
    pub languages: Vec<&'static str>,
    pub proper: bool,
    pub repack: bool,
}

struct Rule {
    re: Regex,
    value: &'static str,
}

fn insensitive(pattern: &str) -> Regex {
    #[allow(clippy::unwrap_used)]
    RegexBuilder::new(pattern).case_insensitive(true).build().unwrap()
}

fn rules(table: &[(&str, &'static str)]) -> Vec<Rule> {
    table.iter().map(|(pattern, value)| Rule { re: insensitive(pattern), value }).collect()
}

fn first(rules: &[Rule], name: &str) -> Option<&'static str> {
    rules.iter().find(|r| r.re.is_match(name)).map(|r| r.value)
}

fn all(rules: &[Rule], name: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    for rule in rules {
        if rule.re.is_match(name) && !out.contains(&rule.value) {
            out.push(rule.value);
        }
    }
    out
}

macro_rules! rule_table {
    ($name:ident, $($pattern:literal => $value:literal),+ $(,)?) => {
        static $name: LazyLock<Vec<Rule>> = LazyLock::new(|| rules(&[$(($pattern, $value)),+]));
    };
}

rule_table!(NETWORKS,
    r"\bATVP?\b" => "Apple TV",
    r"\bAMZN\b" => "Amazon",
    r"\b(NF|Netflix)\b" => "Netflix",
    r"\bNICK(elodeon)?\b" => "Nickelodeon",
    r"\bDSNY?P?\b" => "Disney",
    r"\bH(MAX|BO)\b" => "HBO",
    r"\bHULU\b" => "Hulu",
    r"\bCBS\b" => "CBS",
    r"\bNBC\b" => "NBC",
    r"\bAMC\b" => "AMC",
    r"\bPBS\b" => "PBS",
    r"\b(Crunchyroll|CR)\b" => "Crunchyroll",
    r"\bSony\b" => "Sony",
    r"\bHallmark\b" => "Hallmark",
    r"\bAdult.?Swim\b" => "Adult Swim",
    r"\b(Animal.?Planet|ANPL)\b" => "Animal Planet",
    r"\bCartoon.?Network\b" => "Cartoon Network",
    r"\bPCOK\b" => "Peacock",
    r"\bPMTP\b" => "Paramount+",
);

rule_table!(HDR,
    r"\bDV\b|dolby.?vision|\bDoVi\b" => "DV",
    r"HDR10(\+|[-.\s]?plus)" => "HDR10+",
    r"\bHDR(10)?\b" => "HDR",
    r"\bSDR\b" => "SDR",
);

rule_table!(CODECS,
    r"\b[hx][. -]?264\b" => "avc",
    r"\b[hx][. -]?265\b" => "hevc",
    r"\bHEVC10(bit)?\b|\bhevc(\s?10)?\b" => "hevc",
    r"\b(divx|xvid)\b" => "xvid",
    r"\bavc\b" => "avc",
    r"\bav1\b" => "av1",
    r"\bmpe?g\d*\b" => "mpeg",
);

rule_table!(AUDIO,
    r"\b(Dolby.?)?Atmos\b" => "Atmos",
    r"\bTrue[ .-]?HD\b" => "TrueHD",
    r"\bFLAC(\d\.\d)?(x\d+)?\b" => "FLAC",
    r"\bQ?Q?AAC(x?2)?\b" => "AAC",
    r"\bL?PCM\b" => "PCM",
    r"\bMP3\b" => "MP3",
);

rule_table!(CHANNELS,
    r"5[.\s]1(ch)?\b" => "5.1",
    r"\b7[.\- ]1(.?ch(annel)?)?\b" => "7.1",
    r"2[.\s]0\b" => "2.0",
    r"\bstereo\b" => "stereo",
    r"\bmono\b" => "mono",
);

rule_table!(EDITIONS,
    r"\bUltimate[.\s\-+_/(),]Edition\b" => "Ultimate Edition",
    r"\bExtended[.\s\-+_/(),]Director'?s\b" => "Directors Cut",
    r"\bExtended\b" => "Extended Edition",
    r"\bDirector'?s.?Cut\b" => "Directors Cut",
    r"\bCollector'?s\b" => "Collectors Edition",
    r"\bTheatrical\b" => "Theatrical",
    r"\buncut\b" => "Uncut",
    r"\bIMAX\b" => "IMAX",
    r"\bRemaster(ed)?\b" => "Remastered",
);

rule_table!(LANGUAGES,
    r"\bMULTI\b" => "Multi",
    r"\bDUAL\b" => "Dual",
    r"\bENGLISH\b" => "English",
    r"\bGERMAN\b" => "German",
    r"\bFRENCH\b|\bTRUEFRENCH\b|\bVOSTFR\b" => "French",
    r"\bSPANISH\b|\bCASTELLANO\b" => "Spanish",
    r"\bITALIAN\b|\bITA\b" => "Italian",
    r"\bRUSSIAN\b|\bRUS\b" => "Russian",
    r"\bJAPANESE\b" => "Japanese",
    r"\bKOREAN\b" => "Korean",
    r"\bHINDI\b" => "Hindi",
    r"\bPORTUGUESE\b" => "Portuguese",
    r"\bDUTCH\b" => "Dutch",
);

/// In PTT's order, which matters: the first that matches wins.
fn quality(name: &str) -> Option<String> {
    type QualityRules = (Regex, Regex, Regex, Vec<Rule>, Vec<Rule>);
    static RULES: LazyLock<QualityRules> = LazyLock::new(|| {
        (
            insensitive(r"\bBlu[ .-]*Ray"),
            insensitive(r"\bremux\b"),
            insensitive(r"(BD|BR|UHD)[- ]?remux"),
            rules(&[
                (r"\b(HD[ .-]*)?T(ELE)?S(YNC)?(Rip)?\b", "TeleSync"),
                (r"\b(DVD?|BD|BR|HD)?[ .-]*Scr(eener)?\b", "SCR"),
                (r"\bP(RE)?-?(HD|DVD)(Rip)?\b", "SCR"),
            ]),
            rules(&[
                (r"\bUHD[ .-]*Rip\b", "UHDRip"),
                (r"\bHD[ .-]*Rip\b", "HDRip"),
                (r"\bMicro[ .-]*HD\b", "HDRip"),
                (r"\b(BR|Blu[ .-]*Ray)[ .-]*Rip\b", "BRRip"),
                (r"\bBD[ .-]*Rip\b|\bBDR\b|\bBD-RM\b", "BDRip"),
                (r"\b(HD[ .-]*)?DVD[ .-]*Rip\b", "DVDRip"),
                (r"\bVHS[ .-]*Rip?\b", "VHSRip"),
                (r"\bDVD(R\d?|.*Mux)?\b", "DVD"),
                (r"\bVHS\b", "VHS"),
                (r"\bPPVRip\b", "PPVRip"),
                (r"\bHD.?TV.?Rip\b", "HDTVRip"),
                (r"\bDVB[ .-]*(Rip)?\b", "HDTV"),
                (r"\bSAT[ .-]*Rips?\b", "SATRip"),
                (r"\bTVRips?\b", "TVRip"),
                (r"\bR5\b", "R5"),
                (r"\b(DL|WEB|BD|BR)MUX\b", "WEBMux"),
                (r"\bWEB[ .-]*Rip\b", "WEBRip"),
                (r"\bWEB[ .-]?DL[ .-]?Rip\b", "WEB-DLRip"),
                (r"\bWEB[ .-]*(DL|.BDrip|.DLRIP)\b", "WEB-DL"),
                (r"\bWEB\b", "WEB"),
                (r"\b(H[DQ][ .-]*)?CAM(H[DQ])?([ .-]*Rip|Rp)?\b", "CAM"),
                (r"\b(H[DQ][ .-]*)?S[ .\-]print", "CAM"),
                (r"\bPDTV\b", "PDTV"),
                (r"\bHD(.?TV)?\b", "HDTV"),
            ]),
        )
    });
    #[allow(clippy::unwrap_used)]
    static TELECINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(HD[ .-]*)?T(ELE)?C(INE)?(Rip)?\b").unwrap());
    let (bluray, remux, packed_remux, before, after) = &*RULES;
    if let Some(hit) = first(&before[..1], name) {
        return Some(hit.into());
    }
    if TELECINE.is_match(name) {
        return Some("TeleCine".into());
    }
    if let Some(hit) = first(&before[1..], name) {
        return Some(hit.into());
    }
    if packed_remux.is_match(name) || (bluray.is_match(name) && remux.is_match(name)) {
        return Some("BluRay REMUX".into());
    }
    if remux.is_match(name) {
        return Some("REMUX".into());
    }
    // a BluRay Rip is a rip, which the rules below name
    static RIP: LazyLock<Regex> = LazyLock::new(|| insensitive(r"\bBlu[ .-]*Ray[ .-]*Rip"));
    if bluray.is_match(name) && !RIP.is_match(name) {
        return Some("BluRay".into());
    }
    first(after, name).map(Into::into)
}

fn resolution(name: &str) -> Option<String> {
    static EXACT: LazyLock<Vec<Rule>> = LazyLock::new(|| {
        rules(&[
            (r"3840x\d{4}", "2160p"),
            (r"1920x\d{3,4}", "1080p"),
            (r"1280x\d{3}", "720p"),
            (r"QHD|QuadHD|WQHD|2560x1440", "1440p"),
            (r"Full HD|FHD", "1080p"),
            (r"(BD|HD|M)(2160p?|4k)", "2160p"),
            (r"(BD|HD|M)1080p?", "1080p"),
            (r"(BD|HD|M)720p?", "720p"),
            (r"(BD|HD|M)480p?", "480p"),
            (r"\b4k\b|\b21600?[pi]\b", "2160p"),
        ])
    });
    static NUMBER: LazyLock<Regex> = LazyLock::new(|| insensitive(r"(240|360|480|576|720|1080|1440|2160)([pi])\b"));
    if let Some(hit) = NUMBER.captures(name) {
        return Some(format!("{}{}", &hit[1], hit[2].to_ascii_lowercase()));
    }
    first(&EXACT, name).map(Into::into)
}

fn dts(name: &str, audio: &mut Vec<&'static str>) {
    static LOSSLESS: LazyLock<Regex> = LazyLock::new(|| insensitive(r"\bDTS.?(HD.?Ma(ster)?|X)\b"));
    static ANY: LazyLock<Regex> = LazyLock::new(|| insensitive(r"\bDTS\b"));
    if LOSSLESS.is_match(name) {
        audio.push("DTS Lossless");
    } else if ANY.is_match(name) {
        audio.push("DTS Lossy");
    }
}

fn dolby(name: &str, audio: &mut Vec<&'static str>) {
    #[allow(clippy::unwrap_used)]
    static PLUS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"DD2?[+p]|DD Plus|Dolby Digital Plus|DDP(5[ ._]1)?|E-?AC-?3").unwrap());
    #[allow(clippy::unwrap_used)]
    static OPUS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bOPUS\b").unwrap());
    static PLUS_ANY_CASE: LazyLock<Regex> = LazyLock::new(|| insensitive(r"\bddp"));
    static PLAIN: LazyLock<Regex> =
        LazyLock::new(|| insensitive(r"\b(DD(\d([. ]\d)?)?|Dolby.?Digital|DolbyD|AC-?3(x2)?)\b"));
    if OPUS.is_match(name) {
        audio.push("OPUS");
    }
    if PLUS.is_match(name) || PLUS_ANY_CASE.is_match(name) {
        audio.push("Dolby Digital Plus");
    } else if PLAIN.is_match(name) {
        audio.push("Dolby Digital");
    }
}

fn bit_depth(name: &str) -> Option<String> {
    static BITS: LazyLock<Regex> = LazyLock::new(|| insensitive(r"\b(8|10|12)[-.]?bit\b"));
    static TEN: LazyLock<Regex> = LazyLock::new(|| insensitive(r"\bhevc\s?10\b|\bhdr10\b|\bhi10\b"));
    if let Some(hit) = BITS.captures(name) {
        return Some(format!("{}bit", &hit[1]));
    }
    TEN.is_match(name).then(|| "10bit".to_string())
}

/// The last year in the name that isn't its very start, since a title can be a year,
/// or part of an air date.
fn year(name: &str) -> Option<i32> {
    static YEAR: LazyLock<Regex> = LazyLock::new(|| insensitive(r"\b(19\d{2}|20\d{2})\b"));
    static DATE: LazyLock<Regex> = LazyLock::new(|| insensitive(r"\b(19|20)\d{2}[. -]\d{2}[. -]\d{2}\b"));
    let mut found = None;
    for hit in YEAR.captures_iter(name) {
        let Some(whole) = hit.get(0) else { continue };
        if whole.start() > 0 && !DATE.find_iter(name).any(|date| date.start() == whole.start()) {
            found = hit[1].parse().ok();
        }
    }
    found
}

/// `-GROUP` or `(GROUP)` at the end, or `[GROUP]` at the start.
fn group(name: &str) -> Option<String> {
    static IN_PARENS: LazyLock<Regex> = LazyLock::new(|| insensitive(r"\(([\w-]+)\)$"));
    static END: LazyLock<Regex> = LazyLock::new(|| insensitive(r"-\s?([^\-. \[\]()]+)(\[[\w.-]+\])?$"));
    static START: LazyLock<Regex> = LazyLock::new(|| insensitive(r"^\[([^\[\]]+)\]"));
    static NOT_A_GROUP: LazyLock<Regex> = LazyLock::new(|| {
        insensitive(r"^(\d+|S\d+.*|\d+x.*|ep?\d+|DL|Rip|HD|TV|WEB|x26[45]|h26[45]|xvid|divx|mpeg\d*|.*_.*)$")
    });
    if let Some(hit) = END.captures(name)
        && !NOT_A_GROUP.is_match(&hit[1])
    {
        return Some(hit[1].to_string());
    }
    IN_PARENS
        .captures(name)
        .or_else(|| START.captures(name))
        .map(|hit| hit[1].to_string())
        .filter(|found| !NOT_A_GROUP.is_match(found))
}

/// The name without a file extension, soo the group at its end can be found.
fn without_extension(name: &str) -> &str {
    static EXTENSION: LazyLock<Regex> = LazyLock::new(|| {
        insensitive(r"\.(mkv|mp4|avi|m4v|ts|wmv|mov|mpg|iso|zip|rar|7z|exe|nzb|par2|nfo|flac|mp3|epub|pdf)$")
    });
    EXTENSION.find(name.trim()).map_or(name.trim(), |hit| &name.trim()[..hit.start()])
}

pub fn parse(name: &str) -> Tags {
    let whole = without_extension(name);
    // `\b` doesn't see an underscore as a gap, and names use them as one
    let dotted = whole.replace('_', ".");
    let name = dotted.as_str();
    static PROPER: LazyLock<Regex> = LazyLock::new(|| insensitive(r"\b(REAL.)?PROPER\b"));
    static REPACK: LazyLock<Regex> = LazyLock::new(|| insensitive(r"\b(REPACK|RERIP)\b"));
    let mut audio = Vec::new();
    dts(name, &mut audio);
    audio.extend(all(&AUDIO, name));
    dolby(name, &mut audio);
    Tags {
        resolution: resolution(name),
        quality: quality(name),
        hdr: all(&HDR, name).into_iter().fold(Vec::new(), |mut hdr, tag| {
            // HDR10+ is an HDR format, and its name contains the plain one
            if !(tag == "HDR" && hdr.contains(&"HDR10+")) {
                hdr.push(tag);
            }
            hdr
        }),
        codec: first(&CODECS, name),
        bit_depth: bit_depth(name),
        audio,
        channels: all(&CHANNELS, name),
        network: first(&NETWORKS, name),
        edition: first(&EDITIONS, name),
        year: year(name),
        group: group(whole),
        languages: all(&LANGUAGES, name),
        proper: PROPER.is_match(name),
        repack: REPACK.is_match(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_web_release_reads_like_ptt_reads_it() {
        let t = parse("Dune.Part.Two.2024.2160p.AMZN.WEB-DL.DDP5.1.Atmos.DV.HDR10Plus.H.265-FLUX");
        assert_eq!(t.resolution.as_deref(), Some("2160p"));
        assert_eq!(t.quality.as_deref(), Some("WEB-DL"));
        assert_eq!(t.hdr, ["DV", "HDR10+"]);
        assert_eq!(t.codec, Some("hevc"));
        assert_eq!(t.channels, ["5.1"]);
        assert_eq!(t.network, Some("Amazon"));
        assert_eq!(t.year, Some(2024));
        assert_eq!(t.group.as_deref(), Some("FLUX"));
        assert!(t.audio.contains(&"Atmos") && t.audio.contains(&"Dolby Digital Plus"), "{:?}", t.audio);
    }

    #[test]
    fn a_remux_is_named_and_a_bluray_rip_is_not_a_bluray() {
        let t = parse("The.Matrix.1999.1080p.BluRay.REMUX.AVC.TrueHD.7.1-GRP");
        assert_eq!(t.quality.as_deref(), Some("BluRay REMUX"));
        assert_eq!(t.codec, Some("avc"));
        assert_eq!(t.audio, ["TrueHD"]);
        assert_eq!(t.channels, ["7.1"]);
        assert_eq!(parse("A.2019.BluRay.Rip.x264").quality.as_deref(), Some("BRRip"));
        assert_eq!(parse("A.2019.BluRay.x264").quality.as_deref(), Some("BluRay"));
    }

    #[test]
    fn plain_names_give_nothing_made_up() {
        let t = parse("c2f94a1b77d64e0f");
        assert_eq!(t, Tags::default());
        assert_eq!(parse("Holiday.Photos").group, None);
        assert_eq!(parse("A-Show-S01").group, None);
    }

    #[test]
    fn a_title_that_is_a_year_is_not_the_year() {
        assert_eq!(parse("2012.2009.1080p.BluRay.x264-GRP").year, Some(2009));
        assert_eq!(parse("1917.1080p.WEB.h264").year, None);
    }

    #[test]
    fn dts_kinds_differ() {
        assert_eq!(parse("A.DTS-HD.MA.5.1").audio, ["DTS Lossless"]);
        assert_eq!(parse("A.DTS.5.1").audio, ["DTS Lossy"]);
    }

    #[test]
    fn names_with_underscores_and_extensions_read_the_same() {
        let t = parse("Some_Show_S04E16_HDTV_XviD-LOL.mkv");
        assert_eq!(t.quality.as_deref(), Some("HDTV"));
        assert_eq!(t.codec, Some("xvid"));
        assert_eq!(t.group.as_deref(), Some("LOL"));
        assert_eq!(parse("A-Show_s01_2160p.mp4").group, None);
        assert_eq!(parse("A.DVDRip-XviD").group, None);
        assert_eq!(parse("Show.S01E01.1080i.HDTV").resolution.as_deref(), Some("1080i"));
        assert_eq!(parse("Bluray1080p.mkv").resolution.as_deref(), Some("1080p"));
        assert_eq!(parse("Some.Blu-ray.Ripper.v3").quality, None);
        assert_eq!(parse("Show.2016.12.20.Guest.2160p").year, None);
        assert_eq!(parse("A.READ.NFO.R5").network, None);
    }

    #[test]
    fn other_tags() {
        let t = parse("Show.S01E02.REPACK.720p.NF.WEB-DL.DD5.1.10bit.H264.GERMAN-NTb");
        assert!(t.repack);
        assert_eq!(t.network, Some("Netflix"));
        assert_eq!(t.bit_depth.as_deref(), Some("10bit"));
        assert_eq!(t.audio, ["Dolby Digital"]);
        assert_eq!(t.languages, ["German"]);
        assert_eq!(parse("A.2001.Directors.Cut.1080p").edition, Some("Directors Cut"));
    }
}
