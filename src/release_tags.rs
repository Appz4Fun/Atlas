//! What a release name says about the release, from the PTT port in `ptt`, in the words the pages use.
//! Names are parsed when they're shown, and nothing is stored.

use crate::ptt;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tags {
    pub title: String,
    pub resolution: Option<String>,
    pub quality: Option<String>,
    pub hdr: Vec<String>,
    pub codec: Option<String>,
    pub bit_depth: Option<String>,
    pub audio: Vec<String>,
    pub channels: Vec<String>,
    pub network: Option<String>,
    pub edition: Option<String>,
    pub year: Option<i32>,
    pub group: Option<String>,
    pub languages: Vec<String>,
    pub seasons: Vec<i64>,
    pub episodes: Vec<i64>,
    pub site: Option<String>,
    pub container: Option<String>,
    pub proper: bool,
    pub repack: bool,
    pub remastered: bool,
    pub hardcoded: bool,
    pub dubbed: bool,
    pub subbed: bool,
    pub three_d: Option<String>,
}

/// PTT's language codes, with its names for them.
fn language_name(code: &str) -> Option<&'static str> {
    Some(match code {
        "en" => "English",
        "ja" => "Japanese",
        "zh" => "Chinese",
        "ru" => "Russian",
        "ar" => "Arabic",
        "pt" => "Portuguese",
        "es" => "Spanish",
        "fr" => "French",
        "de" => "German",
        "it" => "Italian",
        "ko" => "Korean",
        "hi" => "Hindi",
        "bn" => "Bengali",
        "pa" => "Punjabi",
        "mr" => "Marathi",
        "gu" => "Gujarati",
        "ta" => "Tamil",
        "te" => "Telugu",
        "kn" => "Kannada",
        "ml" => "Malayalam",
        "th" => "Thai",
        "vi" => "Vietnamese",
        "id" => "Indonesian",
        "tr" => "Turkish",
        "he" => "Hebrew",
        "fa" => "Persian",
        "uk" => "Ukrainian",
        "el" => "Greek",
        "lt" => "Lithuanian",
        "lv" => "Latvian",
        "et" => "Estonian",
        "pl" => "Polish",
        "cs" => "Czech",
        "sk" => "Slovak",
        "hu" => "Hungarian",
        "ro" => "Romanian",
        "bg" => "Bulgarian",
        "sr" => "Serbian",
        "hr" => "Croatian",
        "sl" => "Slovenian",
        "nl" => "Dutch",
        "da" => "Danish",
        "fi" => "Finnish",
        "sv" => "Swedish",
        "no" => "Norwegian",
        "ms" => "Malay",
        "la" => "Latino",
        _ => return None,
    })
}

pub fn parse(name: &str) -> Tags {
    let p = ptt::parse(name);
    let text = |field: &str| p.text(field).map(str::to_string);
    Tags {
        title: p.title.clone(),
        resolution: text("resolution"),
        quality: text("quality"),
        hdr: p.list("hdr").to_vec(),
        codec: text("codec"),
        bit_depth: text("bit_depth"),
        audio: p.list("audio").to_vec(),
        channels: p.list("channels").to_vec(),
        network: text("network"),
        edition: text("edition"),
        year: p.int("year").and_then(|y| i32::try_from(y).ok()),
        group: text("group"),
        languages: p.list("languages").iter().filter_map(|c| language_name(c)).map(str::to_string).collect(),
        seasons: p.ints("seasons").to_vec(),
        episodes: p.ints("episodes").to_vec(),
        site: text("site"),
        container: text("container"),
        proper: p.flag("proper"),
        repack: p.flag("repack"),
        remastered: p.flag("remastered"),
        hardcoded: p.flag("hardcoded"),
        dubbed: p.flag("dubbed"),
        subbed: p.flag("subbed"),
        three_d: text("3d"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_comes_apart() {
        let t = parse("Batman.Begins.2005.1080p.UHD.BluRay.10bit.DDP5.1.x265-FaS.mkv");
        assert_eq!(t.title, "Batman Begins");
        assert_eq!(t.year, Some(2005));
        assert_eq!(t.resolution.as_deref(), Some("1080p"));
        assert_eq!(t.codec.as_deref(), Some("hevc"));
        assert_eq!(t.bit_depth.as_deref(), Some("10bit"));
        assert_eq!(t.group.as_deref(), Some("FaS"));
        assert_eq!(t.container.as_deref(), Some("mkv"));
    }

    #[test]
    fn language_codes_become_names() {
        assert_eq!(parse("Film.2020.GERMAN.1080p.WEB").languages, ["German"]);
    }
}
