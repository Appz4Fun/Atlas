//! Is this release name a random string rather than a real name? A flagged name leaves search
//! (src/search.rs:36), soo a wrong yes costs more than a wrong no.
//!
//! The ideas (hex, UUIDs, case transitions, consonant runs) come from newznab-tmux
//! `app/Traits/DetectsHashedNames.php`, GPLv3. The rules here are this project's own.
use regex::Regex;
use std::sync::LazyLock;

/// The old rule. Anything it flagged stays flagged: search hides these names whatever the flag says.
static LONG_ALNUM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9]{16,}$").unwrap());
/// What `parse_subject` leaves on a name (RELEASE_SUFFIX_RE only strips archive and par2 suffixes).
static PAYLOAD_EXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\.(mkv|mp4|m4v|avi|wmv|mov|ts|m2ts|mpg|mpeg|webm|iso|img|bin|7z|zip|exe|mp3|flac|m4a|epub|mobi|azw3|pdf|cbr|cbz)$").unwrap()
});
/// A token that is release vocabulary is never random.
static MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^((19|20)\d\d|s\d{1,3}(e\d{1,4})*|e\d{2,4}|\d{1,2}x\d{2,3}|\d{3,4}[pi]|[48]k|uhd|hdr|x26[45]|h26[45]|hevc|avc|xvid|divx|av1|bluray|bdrip|brrip|dvdrip|dvd|dvdr|webrip|webdl|web|hdtv|pdtv|sdtv|remux|flac|mp3|aac|ac3|dts|ddp?\d?|truehd|atmos|proper|repack|internal|retail|german|french|multi|xxx|ps[345p]|psv|xbox|nsw|wiiu?|win(32|64)?|x64|x86|macos|osx|linux|v\d+)$").unwrap()
});
/// The 160 most common letter pairs of a word list plus German, French, Spanish and Polish words and
/// titles. A random lowercase string has about 24% of its pairs in here.
const COMMON_PAIRS: &str = "ab ac ad ae ag ai al am an ap ar as at au ba be bi bl bo br ca ce ch ci ck co cr ct cu da de di do dr ea ec ed ee el em en eo ep er es et fe fi fo ga ge gi gl go gr ha he hi ho hy ia ic id ie if ig il im in io ip ir is it iv iz ke la le li ll lo lu ly ma me mi mo mp na nc nd ne ng ni no ns nt oc od og oi ol om on oo op or os ot ou ov pa pe ph pi pl po pr qu ra rc rd re ri rm ro rr rs rt ru ry sa sc se sh si sm so sp ss st su ta te th ti to tr tu ty ul um un ur us ut ve vi ze";

const MIN_TOKEN: usize = 8; // shorter single-case or CamelCase tokens are not judged
const MIN_MIXED_TOKEN: usize = 6; // shorter mixed-case tokens are not judged
const MIN_PAIRS: usize = 5; // fewer letter pairs than this are not judged
const PLAIN_MIN_COMMON_PCT: usize = 25; // lowercase, UPPERCASE or CamelCase
const MIXED_MIN_COMMON_PCT: usize = 45; // irregular case
const MAX_CONSONANTS: usize = 6; // y counts as a vowel
const MIN_HEX: usize = 12; // one hex token with a digit
const MIN_JOINED_HEX: usize = 16; // a uuid or hash cut up by separators
const MIN_MIXED_CHARS: usize = 12; // short pieces that each mix letters and digits

fn is_common(a: u8, b: u8) -> bool {
    static TABLE: LazyLock<[bool; 676]> = LazyLock::new(|| {
        let mut t = [false; 676];
        for p in COMMON_PAIRS.split(' ') {
            let p = p.as_bytes();
            t[(p[0] - b'a') as usize * 26 + (p[1] - b'a') as usize] = true;
        }
        t
    });
    TABLE[(a - b'a') as usize * 26 + (b - b'a') as usize]
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Token {
    Neutral,
    Readable,
    Random,
}

/// One run of ASCII letters and digits.
pub fn classify(t: &str) -> Token {
    let b = t.as_bytes();
    let len = b.len();
    let digits = b.iter().filter(|c| c.is_ascii_digit()).count();
    if digits == len {
        return if len >= 16 { Token::Random } else { Token::Neutral };
    }
    if MARKER.is_match(t) {
        return Token::Readable;
    }
    if digits > 0 && len >= MIN_HEX && b.iter().all(u8::is_ascii_hexdigit) {
        return Token::Random;
    }
    let upper = b.iter().filter(|c| c.is_ascii_uppercase()).count();
    let lower = b.iter().filter(|c| c.is_ascii_lowercase()).count();
    if len >= MIN_TOKEN && digits > 0 {
        let runs = b
            .iter()
            .enumerate()
            .filter(|(i, c)| c.is_ascii_digit() && (*i == 0 || !b[*i - 1].is_ascii_digit()))
            .count();
        let interior = runs - usize::from(b[0].is_ascii_digit()) - usize::from(b[len - 1].is_ascii_digit());
        let flips = b.windows(2).filter(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase()).count();
        if interior >= 1 && (runs >= 2 && upper + lower >= 2 || flips >= 2) {
            return Token::Random;
        }
    }
    let plain = lower == 0 || upper == 0 || is_camel(b);
    if len < if plain { MIN_TOKEN } else { MIN_MIXED_TOKEN } {
        return Token::Neutral;
    }
    let (mut pairs, mut common, mut run, mut longest) = (0, 0, 0, 0);
    for (i, c) in b.iter().map(u8::to_ascii_lowercase).enumerate() {
        if !c.is_ascii_alphabetic() {
            run = 0;
            continue;
        }
        if matches!(c, b'a' | b'e' | b'i' | b'o' | b'u' | b'y') {
            run = 0
        } else {
            run += 1;
            longest = longest.max(run)
        }
        if let Some(next) = b.get(i + 1).map(u8::to_ascii_lowercase).filter(u8::is_ascii_alphabetic) {
            pairs += 1;
            common += usize::from(is_common(c, next));
        }
    }
    if pairs < MIN_PAIRS {
        return Token::Neutral;
    }
    let least = if plain { PLAIN_MIN_COMMON_PCT } else { MIXED_MIN_COMMON_PCT };
    if longest > MAX_CONSONANTS || common * 100 < pairs * least { Token::Random } else { Token::Readable }
}

/// Capitalized words of at least 3 letters each, with an optional lowercase start: iPhone, StarCraft.
fn is_camel(b: &[u8]) -> bool {
    let letters: Vec<u8> = b.iter().copied().filter(u8::is_ascii_alphabetic).collect();
    let mut i = letters.iter().take_while(|c| c.is_ascii_lowercase()).count();
    while i < letters.len() {
        if !letters[i].is_ascii_uppercase() {
            return false;
        }
        let tail = letters[i + 1..].iter().take_while(|c| c.is_ascii_lowercase()).count();
        if tail < 2 {
            return false;
        }
        i += 1 + tail;
    }
    true
}

pub fn is_obfuscated(name: &str) -> bool {
    if LONG_ALNUM.is_match(name) {
        return true;
    }
    let stem = PAYLOAD_EXT.replace(name.trim(), "");
    // words with spaces, or letters outside ASCII: a person wrote it, or it can't be judged
    if !stem.is_ascii() || stem.contains(' ') {
        return false;
    }
    let tokens: Vec<&str> = stem.split(|c: char| !c.is_ascii_alphanumeric()).filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() {
        return false;
    }
    let total: usize = tokens.iter().map(|t| t.len()).sum();
    let hex = tokens.iter().all(|t| t.bytes().all(|c| c.is_ascii_hexdigit()));
    if hex && total >= MIN_JOINED_HEX && tokens.iter().any(|t| t.bytes().any(|c| c.is_ascii_digit())) {
        return true;
    }
    let (mut random, mut readable, mut mixed, mut mixed_tokens) = (0, 0, 0, 0);
    for t in &tokens {
        let kind = classify(t);
        match kind {
            Token::Random => random += t.len(),
            Token::Readable => readable += t.len(),
            Token::Neutral => {}
        }
        if kind != Token::Readable
            && t.bytes().any(|c| c.is_ascii_digit())
            && t.bytes().any(|c| c.is_ascii_alphabetic())
        {
            mixed += t.len();
            mixed_tokens += 1;
        }
    }
    if mixed_tokens >= 2 && total >= MIN_MIXED_CHARS && mixed * 4 >= total * 3 {
        return true;
    }
    // most of the name is random, and what is left isn't release vocabulary
    random > 0 && random * 2 >= total && readable * 2 <= random
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_random_shapes() {
        for n in [
            "a1B2c3D4e5F6g7H8",
            "a1B2c3D4e5F6g7H8i9J0",
            "kP93xQ7rL.ZMtwBfa",
            "hQ72vB9nK.XWdrLpz",
            "Xk3Mz9PqL",
            "nQzw7RtKjd",
            "qT4mVx8LpW",
            "pK2xQ9mZ",
            "550e8400-e29b-41d4-a716-446655440000",
            "d41d8cd98f00b204e9800998ecf8427e",
            "D41D8CD98F00B204E9800998ECF8427E",
            "da39a3ee5e6b4b0d3255bfef95601890afd80709",
            "aB3dE-fG7hJ-kL9mN",
            "x7Kp2QmZ-t9Rw4LsV",
            "Gq7YGEWLy8wAA2NhbZx5LukEa",
            "abcd1234efgh5678",
            "zXcVbNmAsDfGhJkL",
            "kjhgfdsapoiuytre",
            "QWERTYUIOPASDFGH",
            "qzxkjwvbnm",
            "xkcdqwrtplmn",
            "Qw3Er5Ty7.mkv",
            "nMhb2KxQd7.rar.bin",
            "tZ9wL2xV_pR5nK8q",
        ] {
            assert!(is_obfuscated(n), "{n}");
        }
    }

    #[test]
    fn leaves_real_names_alone() {
        for n in [
            "Movie.2024.1080p.WEB",
            "Some.Show.S01E01.720p.HDTV.x264-GRP",
            "Avatar",
            "Inception",
            "Dune",
            "Se7en",
            "Thor2",
            "U2",
            "Blink182",
            "MacOS",
            "iPhone15",
            "WinRAR",
            "XMen2",
            "LaLaLand",
            "Dune2021",
            "GTA5",
            "Fallout4",
            "McDonald",
            "YouTube",
            "PlayStation5",
            "eBay",
            "JavaScript",
            "SpongeBob",
            "StarCraft2",
            "NaNoWriMo",
            "Windows10",
            "Office2019",
            "Photoshop",
            "Transformers",
            "Schwarzenegger",
            "Strengths",
            "Rammstein",
            "Metallica",
            "Kraftwerk",
            "Motorhead",
            "Bjork",
            "Xzibit",
            "Minecraft",
            "Cyberpunk2077",
            "Battlefield1942",
            "Spider-Man",
            "Wu-Tang-Clan",
            "Foo-Bar-GRP",
            "The.Matrix",
            "the_matrix_1999",
            "abc def",
            "broken upload",
            "Some.Thing",
            "Rel",
            "a [4]",
            "no part",
            "file.name.mkv",
            "weird.bin",
            "just.a.file",
            "ab.0001",
            "Ünïcödé.Fïlé.mkv",
            "Тестовый.Релиз",
            "Die.Hard.4.0",
            "Ocean11",
            "Apollo13",
            "Area51",
            "R2D2",
            "C3PO",
            "1984",
            "20240115",
            "Weihnachtsmann",
            "Schlafzimmer",
            "Fruehstueck",
            "Bibliotheque",
            "Chevalier",
            "Manana",
            "Corazon",
            "Przygoda",
            "Warszawa",
            "Kobenhavn",
            "Yokohama",
            "Shinjuku",
            "Dragonball",
            "Evangelion",
            "Adobe.Photoshop.2024.v25.0-GRP",
            "Microsoft.Office.2019.x64",
            "VA-Best_Of_Rock-2CD-2019-GRP",
            "Artist-Album-WEB-2024-GRP",
            "Game.Name-CODEX",
            "Foo.Bar.2024.1080p.WEB-DL.DDP5.1.H.264-x7Kp2QmZ",
            "TheMatrix",
            "MyFavoriteMovie",
            "GameOfThrones",
            "01-track_name",
            "Track01",
            "CD1",
            "IMG_1234",
            "DSC01234",
            "P1010123",
        ] {
            assert!(!is_obfuscated(n), "{n}");
        }
    }

    /// decision 6
    #[test]
    fn everything_the_old_rule_flagged_stays_flagged() {
        let mut seed = 0x1234_5678_9abc_def0u64;
        let alpha = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        for i in 0..20_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let s: String = (0..16 + i % 20).map(|k| alpha[(seed >> (k % 40)) as usize % 62] as char).collect();
            assert!(is_obfuscated(&s), "{s}");
        }
        assert!(is_obfuscated("TheMatrixReloaded"), "17 letters, as today");
    }

    #[test]
    fn most_random_short_names_are_caught() {
        let mut seed = 0x1234_5678_9abc_def0u64;
        let alpha = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            alpha[seed as usize % 62] as char
        };
        let hits = (0..20_000)
            .filter(|_| {
                let s: String = (0..10).map(|_| next()).collect();
                is_obfuscated(&s)
            })
            .count();
        assert!(hits >= 18_000, "{hits} of 20000");
    }
}
