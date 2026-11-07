//! A spot's kind as a hint for its release's category. The spotter says whether a post is a movie or a series,
//! which the name alone often doesn't: `Hard labeur (1985)` names no episode, a Moto3 qualifying session reads like
//! a movie. When the category step's pick (`category::classify`) isn't under the spot's root, the spot wins: its
//! kind gives the root, and the name's resolution or the spot's format gives the leaf.

use super::{Kind, Subcats};

/// The genre (`d`) Spotweb calls Sport.
const SPORT: u16 = 18;

/// How sharp a spot's video is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Quality {
    Uhd,
    Hd,
    Sd,
    BluRay,
    Dvd,
    ThreeD,
}

/// The category release `name` gets from its spot, or None to keep `classified`, the category step's pick (0 for
/// none), when it is already under the spot's root.
pub fn hint(kind: Kind, subcats: &Subcats, name: &str, classified: i64) -> Option<i64> {
    let root = match kind {
        Kind::Movie => 2000,
        Kind::Series => 5000,
    };
    if crate::category::root_of(classified) == root {
        return None;
    }
    if kind == Kind::Series && subcats.d.contains(&SPORT) {
        return Some(5060);
    }
    let quality = from_name(name).or_else(|| from_format(&subcats.a));
    Some(match (kind, quality) {
        (Kind::Movie, Some(Quality::Uhd)) => 2045,
        (Kind::Movie, Some(Quality::Hd)) => 2040,
        (Kind::Movie, Some(Quality::Sd)) => 2030,
        (Kind::Movie, Some(Quality::BluRay)) => 2060,
        (Kind::Movie, Some(Quality::Dvd)) => 2070,
        (Kind::Movie, Some(Quality::ThreeD)) => 2050,
        (Kind::Movie, None) => 2999,
        (Kind::Series, Some(Quality::Uhd)) => 5045,
        (Kind::Series, Some(Quality::Hd | Quality::BluRay | Quality::ThreeD)) => 5040,
        (Kind::Series, Some(Quality::Sd | Quality::Dvd)) => 5030,
        (Kind::Series, None) => 5999,
    })
}

/// The resolution a release name states, when it states one.
fn from_name(name: &str) -> Option<Quality> {
    let lower = name.to_ascii_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let has = |list: &[&str]| words.iter().any(|w| list.contains(w));
    if has(&["2160p", "4k", "uhd"]) {
        Some(Quality::Uhd)
    } else if has(&["1080p", "1080i", "720p"]) {
        Some(Quality::Hd)
    } else if has(&["480p", "576p", "xvid", "divx", "dvdrip"]) {
        Some(Quality::Sd)
    } else {
        None
    }
}

/// What the spot's format (`a`, `names::FORMAT`) says: the sharpest one it lists.
fn from_format(formats: &[u16]) -> Option<Quality> {
    let of = |a: u16| match a {
        15 => Some(Quality::Uhd),
        14 => Some(Quality::ThreeD),
        6 => Some(Quality::BluRay),
        4 | 7 | 8 | 9 => Some(Quality::Hd),
        3 | 10 => Some(Quality::Dvd),
        0..=2 => Some(Quality::Sd),
        _ => None,
    };
    let rank = |q: &Quality| match q {
        Quality::Uhd => 0,
        Quality::ThreeD => 1,
        Quality::BluRay => 2,
        Quality::Hd => 3,
        Quality::Dvd => 4,
        Quality::Sd => 5,
    };
    formats.iter().filter_map(|a| of(*a)).min_by_key(rank)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spotnet::header::subcats;

    #[test]
    fn a_pick_under_the_spots_root_stays() {
        let s = subcats("a9|b12|c6|z0|");
        assert_eq!(hint(Kind::Movie, &s, "Toy.Story.5.2026.DUTCH.1080p.WEB.h264-TRIPEL", 2010), None);
        assert_eq!(hint(Kind::Series, &subcats("a9|b4|z1|"), "Show S01E01 1080p", 5040), None);
    }

    #[test]
    fn the_spots_kind_gives_the_root() {
        // releases on prod the category step put elsewhere, or nowhere
        let movie = subcats("a9|b12|c6|c10|d21|z0|");
        assert_eq!(hint(Kind::Movie, &movie, "Landship.2026.1080p.x264.AAC5.1-GP-M-NLsubs", 0), Some(2040));
        assert_eq!(
            hint(Kind::Movie, &subcats("a15|b3|c6|d0|z0|"), "MCU 05. Captain America 2160p UHD", 3020),
            Some(2045)
        );
        assert_eq!(
            hint(Kind::Movie, &subcats("a9|b12|d7|d21|z0|"), "The Hunley (1999) - 1080p TVrip FHD", 5040),
            Some(2040)
        );
        assert_eq!(
            hint(Kind::Movie, &subcats("a4|b3|d0|z0|"), "Dirty Deeds (2002) - AV1 - Topaz Enhanced DVD", 0),
            Some(2040)
        );
        let series = subcats("a9|b4|b3|c0|d11|d7|z1|");
        assert_eq!(hint(Kind::Series, &series, "Hard labeur (1985)", 0), Some(5040));
        let flemish = subcats("a9|b4|b11|c6|d11|z1|");
        assert_eq!(
            hint(Kind::Series, &flemish, "Ze.Zeggen.Dat.Terug.Naar.School.2026.FLEMISH.1080p.WEB.h264-TRIPEL", 2010),
            Some(5040)
        );
        assert_eq!(hint(Kind::Series, &subcats("a9|b4|c1|d11|d7|z1|"), "Morfeusz 2026", 8010), Some(5040));
    }

    #[test]
    fn sport_and_the_leaves() {
        let sport = subcats("a9|b4|b11|c0|c10|d11|d18|z1|");
        assert_eq!(
            hint(Kind::Series, &sport, "Moto3.2026.Qatar.Qualifying.Two.1080p.WEB.h264-BILLIE", 2040),
            Some(5060)
        );
        assert_eq!(hint(Kind::Movie, &subcats("a6|z0|"), "Landship.2026.COMPLETE.BLURAY-CBGB", 0), Some(2060));
        assert_eq!(hint(Kind::Movie, &subcats("a3|z0|"), "Old film", 0), Some(2070));
        assert_eq!(hint(Kind::Movie, &subcats("a14|a9|z0|"), "Avatar 3D", 0), Some(2050));
        assert_eq!(hint(Kind::Series, &subcats("a10|z1|"), "Show box", 0), Some(5030));
        assert_eq!(hint(Kind::Movie, &subcats("a0|z0|"), "Old film 2160p", 0), Some(2045), "the name first");
        assert_eq!(hint(Kind::Movie, &subcats("z0|"), "No idea", 0), Some(2999));
        assert_eq!(hint(Kind::Series, &subcats(""), "No idea", 0), Some(5999));
    }
}
