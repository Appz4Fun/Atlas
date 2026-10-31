//! What Atlas refuses to index: posts obfuscated per article, and groups
//! labelled for minors. The per-article rule is ported from newznab-tmux
//! (`HeaderParser::markPerArticleObfuscated`, `BUGS.md` #65), GPLv3.

use std::collections::HashMap;

use rusqlite::{Connection, Result, functions::FunctionFlags};

// ---------------------------------------------------------------- blocked groups

/// on their own, these label a group for minors (a token containing one)
const MINOR_ROOTS: [&str; 13] = [
    "preteen",
    "lolita",
    "lolicon",
    "shotacon",
    "jailbait",
    "underage",
    "nymphet",
    "childporn",
    "kidporn",
    "kinderporn",
    "childlove",
    "boylove",
    "girllove",
];
/// the same, as a whole word, singular
const MINOR_WHOLE: [&str; 4] = ["loli", "shota", "pthc", "ptsc"];
/// the same, as a word's start
const MINOR_PREFIXES: [&str; 2] = ["pedo", "paedo"];

/// words for an age, in a word: they pair with an adult, picture or video label
const TEEN_ROOTS: [&str; 2] = ["teen", "tween"];
const YOUTH_ROOTS: [&str; 3] = ["youth", "young", "yung"];
/// the same, as a whole word, singular
const AGE_WHOLE: [&str; 6] = ["yuth", "youht", "jong", "jung", "jeune", "tiener"];

/// words for a child, in a word: they pair with an adult or picture label, not
/// a video one, soo `alt.binaries.kids.movies` is left alone
const CHILD_ROOTS: [&str; 5] = ["child", "kinder", "enfant", "adolesc", "juvenil"];
/// the same, as a whole word, singular
const CHILD_WHOLE: [&str; 4] = ["kid", "boy", "minor", "nino"];
/// the same, as a word's start
const CHILD_PREFIXES: [&str; 4] = ["kids", "kidz", "kiddie", "kiddy"];
/// the same, in one word or over several: `schoolgirls`, `little.boys`
const CHILD_JOINED: [&str; 4] = ["schoolgirl", "schoolboy", "littlegirl", "littleboy"];
/// words ending in `boy` that arent about one
const NOT_A_BOY: [&str; 7] = ["playboy", "cowboy", "gameboy", "tomboy", "busboy", "bellboy", "beachboy"];

/// adult labels (a word containing one)
const ADULT_ROOTS: [&str; 10] =
    ["erot", "porn", "pron", "xxx", "hentai", "nudis", "naturis", "fetish", "hardcore", "softcore"];
/// the same, as a whole word
const ADULT_WHOLE: [&str; 11] =
    ["ero", "sex", "sexy", "nude", "nudes", "naked", "nudity", "adult", "adults", "amateur", "amateurs"];
/// the same, split up: `x.x.x`
const ADULT_JOINED: [&str; 6] = ["erotica", "erotic", "porn", "porno", "xxx", "nude"];
/// picture labels, as a whole word
const PICTURE_WHOLE: [&str; 11] =
    ["pics", "pic", "photo", "photos", "foto", "fotos", "jpg", "jpeg", "gallery", "galleries", "bilder"];
/// picture labels too, unless the name is about disc images (`alt.binaries.cd.image`)
const IMAGE_WHOLE: [&str; 4] = ["image", "images", "img", "imgs"];
const DISC_WORDS: [&str; 5] = ["cd", "dvd", "iso", "disc", "disk"];
/// video labels, as a whole word, singular
const VIDEO_WHOLE: [&str; 15] = [
    "movie",
    "video",
    "vid",
    "clip",
    "mpeg",
    "mpg",
    "avi",
    "mkv",
    "mp4",
    "wmv",
    "film",
    "flix",
    "cam",
    "webcam",
    "multimedia",
];
/// video labels inside a word: `teenmovies`
const VIDEO_ROOTS: [&str; 6] = ["movie", "video", "vids", "clip", "film", "flix"];
/// words that say nothing more about a group with a video label, soo they cant be a title
const NOT_A_TITLE: [&str; 15] = [
    "hd", "sd", "dvd", "xvid", "divx", "x264", "x265", "hevc", "high", "res", "hot", "sweet", "cute", "little", "tiny",
];
/// tokens that say nothing about a group
const HIERARCHY: [&str; 4] = ["alt", "binaries", "binary", "bin"];

/// A group Atlas must never index, fetch bodies from, or serve anything of:
/// its name labels it for minors. Always on: there is no setting for it.
///
/// Blocked when the name states an age under 18 in years (`14yo`), or holds a
/// term that is unambiguous on its own, or a word for an age or a child next to
/// an adult or picture label, or an age below 18 there (`14`, `u18`, `14yr`), or
/// `teen`, `young` or `youth` with a video label and no title, or when one of
/// those three is all the name says. Case, accents, digits standing in for
/// letters, plurals and split letters dont hide a term, and `young` and
/// `erotica` are matched within a small edit distance. A teen show in a group
/// with no adult or picture label is not blocked.
pub fn group_blocked(name: &str) -> bool {
    // asked once per row by sql and per release by a save, of a few thousand
    // names at most: each thread remembers its answers
    thread_local! {
        static SEEN: std::cell::RefCell<HashMap<String, bool>> = std::cell::RefCell::new(HashMap::new());
    }
    SEEN.with_borrow_mut(|seen| {
        if let Some(answer) = seen.get(name) {
            return *answer;
        }
        if seen.len() >= 65_536 {
            seen.clear();
        }
        let answer = names_minors(name);
        seen.insert(name.to_string(), answer);
        answer
    })
}

/// a group name in words: `raw` as written (lowercase, accents gone), `read`
/// with digits taken as the letters they stand in for, `joined` what runs of
/// short words spell when put together
struct Words {
    raw: Vec<String>,
    read: Vec<String>,
    joined: Vec<String>,
}

impl Words {
    fn of(name: &str) -> Words {
        let folded = fold(name);
        let raw: Vec<String> =
            folded.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(String::from).collect();
        let read = raw.iter().map(|t| as_letters(t)).collect();
        let mut joined = Vec::new();
        for start in 0..raw.len() {
            let mut word = String::new();
            for (taken, piece) in raw[start..].iter().enumerate() {
                let single = piece.chars().count() == 1;
                // any run of single letters (`p.r.e.t.e.e.n`), up to 4 short pieces
                if piece.chars().count() > 7 || (taken >= 4 && !single) {
                    break;
                }
                word.push_str(piece);
                if taken > 0 {
                    joined.push(as_letters(&word));
                }
            }
        }
        Words { raw, read, joined }
    }

    /// any word as written or as read
    fn any(&self, test: impl Fn(&str) -> bool) -> bool {
        self.raw.iter().chain(&self.read).any(|t| test(t))
    }

    /// the words the name says something with
    fn titled(&self) -> impl Iterator<Item = &String> {
        self.read.iter().filter(|t| !HIERARCHY.contains(&t.as_str()))
    }
}

/// lowercase, with accents and look-alike letters read as plain ones
fn fold(name: &str) -> String {
    name.chars()
        .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'а' => 'a',
            'ç' | 'ć' | 'č' | 'с' => 'c',
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' | 'е' => 'e',
            'ì' | 'í' | 'î' | 'ï' | 'ī' | 'і' => 'i',
            'ñ' | 'ń' => 'n',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'о' => 'o',
            'р' => 'p',
            'š' | 'ś' => 's',
            'ù' | 'ú' | 'û' | 'ü' | 'ū' => 'u',
            'ý' | 'ÿ' | 'у' => 'y',
            'ž' | 'ź' | 'ż' => 'z',
            'х' => 'x',
            c => c,
        })
        .collect()
}

/// digits read as the letters they stand in for, when a word has both (`t33n`, `y0ung`)
fn as_letters(t: &str) -> String {
    if !(t.bytes().any(|b| b.is_ascii_digit()) && t.bytes().any(|b| b.is_ascii_alphabetic())) {
        return t.to_string();
    }
    t.chars()
        .map(|c| match c {
            '0' => 'o',
            '1' => 'i',
            '3' => 'e',
            '4' => 'a',
            '5' => 's',
            '7' => 't',
            c => c,
        })
        .collect()
}

/// a word without a plural ending: `kids`, `boyz`
fn singular(t: &str) -> &str {
    t.strip_suffix(['s', 'z']).unwrap_or(t)
}

/// `word` is `root` or a form of it: `lolis`, `lolies`, `underaged`
fn form_of(word: &str, root: &str) -> bool {
    word.strip_prefix(root).is_some_and(|rest| matches!(rest, "" | "s" | "z" | "d" | "es"))
}

#[derive(Clone, Copy, PartialEq)]
enum Age {
    None,
    /// a number that could be an age: only says so next to a label
    Number,
    /// says it's an age limit or a count of years: `u18`, `under16`, `14yr`
    Limit,
    /// says so outright: `14yo`
    Stated,
}

/// what a word as written says about an age under 18. A limit up to 21 counts,
/// since what is under it includes minors.
fn age_in(word: &str) -> Age {
    let digits = word.bytes().take_while(u8::is_ascii_digit).count();
    let (number, unit) = word.split_at(digits);
    if let (true, Ok(years)) = (digits <= 2, number.parse::<u32>()) {
        match unit {
            "" if (5..=17).contains(&years) => return Age::Number,
            "yo" if (1..=17).contains(&years) => return Age::Stated,
            "yr" | "yrs" | "y" | "jr" if (1..=17).contains(&years) => return Age::Limit,
            _ => {}
        }
    }
    let limit = ["under", "sub", "u"].iter().find_map(|p| word.strip_prefix(p));
    match limit.filter(|n| n.len() == 2).and_then(|n| n.parse::<u32>().ok()) {
        Some(years) if years <= 21 => Age::Limit,
        _ => Age::None,
    }
}

fn is_teen(t: &str) -> bool {
    TEEN_ROOTS.iter().any(|r| t.contains(r))
}

fn is_youth(t: &str) -> bool {
    YOUTH_ROOTS.iter().any(|r| t.contains(r)) || AGE_WHOLE.contains(&singular(t)) || near(t, "young", 1)
}

fn is_child(t: &str) -> bool {
    let one = singular(t);
    CHILD_ROOTS.iter().any(|r| t.contains(r))
        || CHILD_WHOLE.contains(&one)
        || t == "ninas"
        || CHILD_PREFIXES.iter().any(|p| t.starts_with(p))
        || CHILD_JOINED.contains(&one)
        || (one.ends_with("boy") && !NOT_A_BOY.contains(&one))
        || t.strip_prefix("boys").or(t.strip_prefix("boy")).is_some_and(|rest| !rest.is_empty() && is_adult(rest))
}

fn is_adult(t: &str) -> bool {
    ADULT_ROOTS.iter().any(|r| t.contains(r)) || ADULT_WHOLE.contains(&t) || near(t, "erotica", 2)
}

fn is_video(t: &str) -> bool {
    VIDEO_WHOLE.contains(&singular(t))
}

fn names_minors(name: &str) -> bool {
    let words = Words::of(name);

    let unambiguous = words.any(|t| {
        MINOR_ROOTS.iter().any(|r| t.contains(r))
            || MINOR_WHOLE.iter().any(|r| form_of(t, r))
            || MINOR_PREFIXES.iter().any(|p| t.starts_with(p))
    }) || words
        .joined
        .iter()
        .any(|j| MINOR_ROOTS.iter().any(|r| form_of(j, r)) || MINOR_WHOLE.iter().any(|r| form_of(j, r)));

    // a number and its unit can be two words: `14-yo`, `under.18`
    let pairs = words.raw.windows(2).map(|w| format!("{}{}", w[0], w[1]));
    let ages: Vec<Age> = words.raw.iter().map(|t| age_in(t)).chain(pairs.map(|t| age_in(&t))).collect();
    if unambiguous || ages.contains(&Age::Stated) {
        return true;
    }
    let number = ages.contains(&Age::Number);
    let limit = ages.contains(&Age::Limit);

    let spells = |roots: &[&str]| words.joined.iter().any(|j| roots.iter().any(|r| form_of(j, r)));
    let teen = words.any(is_teen) || spells(&TEEN_ROOTS);
    let youth = words.any(is_youth) || spells(&YOUTH_ROOTS);
    let child = words.any(is_child) || spells(&CHILD_JOINED) || spells(&CHILD_ROOTS);

    let adult = words.any(is_adult) || spells(&ADULT_JOINED);
    let disc = words.any(|t| DISC_WORDS.contains(&t));
    let picture =
        words.any(|t| PICTURE_WHOLE.contains(&t) || near(t, "pictures", 1) || (!disc && IMAGE_WHOLE.contains(&t)));
    if (adult || picture) && (teen || youth || child || number || limit) {
        return true;
    }
    // a bare `girl` is an everyday adult label: only a word built on it counts, and only with an adult one
    if adult && words.any(|t| t.contains("girl") && singular(t) != "girl") {
        return true;
    }

    // a video label pairs when nothing else in the name could be a title:
    // `teen.movies`, but not `movies.teen-wolf` or `neil-young.videos`
    let plain = |t: &str| is_video(t) || NOT_A_TITLE.contains(&t) || is_teen(t) || is_youth(t);
    let titled: Vec<&String> = words.titled().collect();
    if words.any(is_video) {
        let after_teen = titled.iter().position(|t| is_teen(t)).map(|at| &titled[at + 1..]);
        if limit
            || after_teen.is_some_and(|after| after.iter().all(|t| plain(t)))
            || (youth && titled.iter().all(|t| plain(t)))
        {
            return true;
        }
    }
    if words
        .any(|t| (is_teen(t) || YOUTH_ROOTS.iter().any(|r| t.contains(r))) && VIDEO_ROOTS.iter().any(|r| t.contains(r)))
    {
        return true;
    }

    // nothing but the label: `alt.binaries.teens`
    titled.len() == 1
        && (is_teen(titled[0]) || YOUTH_ROOTS.iter().any(|r| titled[0].contains(r)) || near(titled[0], "young", 1))
}

/// `word` within `max` edits of `target`: a letter added, dropped, changed, or
/// two next to each other swapped
fn near(word: &str, target: &str, max: usize) -> bool {
    let (a, b) = (word.as_bytes(), target.as_bytes());
    if a.len().abs_diff(b.len()) > max || a.len() < 4 {
        return false;
    }
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    d[0] = (0..=b.len()).collect();
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let change = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + change);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[a.len()][b.len()] <= max
}

/// Lets sql ask `atlas_blocked(group_name)`. Every connection has it (`db::set_up`).
/// NULL, or anything that isnt text, isnt blocked.
pub fn register_sql(conn: &Connection) -> Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC | FunctionFlags::SQLITE_INNOCUOUS;
    conn.create_scalar_function("atlas_blocked", 1, flags, |ctx| Ok(ctx.get_raw(0).as_str().is_ok_and(group_blocked)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unambiguous_terms_block_on_their_own() {
        for g in [
            "alt.binaries.preteen",
            "alt.binaries.pre-teen",
            "alt.binaries.lolita",
            "alt.binaries.l0lita",
            "alt.binaries.loli",
            "alt.binaries.lolis",
            "alt.binaries.tv.jailbait",
            "alt.binaries.jail.bait",
            "alt.binaries.underaged",
            "alt.binaries.under-age",
            "alt.binaries.pedo",
            "alt.binaries.pedofilia",
            "alt.binaries.child.porn",
            "alt.binaries.kinderporn",
            "alt.binaries.p.r.e.t.e.e.n",
            "alt.binaries.pictures.p.r.e.t.e.e.n",
            "alt.binaries.l0.lita",
            "alt.binaries.lo.li.ta",
            // an age in years, said outright
            "alt.binaries.14yo",
            "alt.binaries.pictures.9yo",
            "alt.binaries.erotica.14yo",
            "alt.binaries.14-yo",
        ] {
            assert!(group_blocked(g), "{g}");
        }
    }

    #[test]
    fn ambiguous_terms_need_a_label() {
        for g in [
            "alt.binaries.pictures.erotica.teen",
            "alt.binaries.erotica.teens",
            "alt.binaries.teen.erotica",
            "alt.binaries.young.erotica",
            "alt.binaries.yong.eroitca",
            "alt.binaries.yung.erotic",
            "alt.binaries.youngerotica",
            "alt.binaries.eroticateens",
            "ALT.BINARIES.PICTURES.EROTICA.TEEN",
            "alt.binaries.t33n.er0tica",
            "alt.binaries.y0ung.xxx",
            "alt.binaries.pictures.youth",
            "alt.binaries.pictures.teen",
            "alt.binaries.pics.teens",
            "alt.binaries.pictures.young",
            "alt.binaries.pictrues.yong",
            "alt.binaries.little.girls.pictures",
            "alt.binaries.schoolgirls.erotica",
            "alt.binaries.nude.kids",
            "alt.binaries.teens",
            "alt.binaries.young",
            "alt.binaries.youth",
            "alt.binaries.pictures.seventeen",
            // numbers and limits as an age, next to a label
            "alt.binaries.erotica.under18",
            "alt.binaries.erotica.u18",
            "alt.binaries.erotica.under.18",
            "alt.binaries.erotica.12-15",
            "alt.binaries.pictures.14",
            "alt.binaries.erotica.14yr",
            // words built on a child word
            "alt.binaries.erotica.childlover",
            "alt.binaries.erotica.kidz",
            "alt.binaries.erotica.boyz",
            "alt.binaries.erotica.childs",
            "alt.binaries.pictures.kinder",
            "alt.binaries.erotica.enfants",
            "alt.binaries.erotica.ninos",
            "alt.binaries.erotica.schoolboy",
            "alt.binaries.erotica.littlegirl",
            "alt.binaries.erotica.girlie",
            "alt.binaries.erotica.adolescent",
            "alt.binaries.erotica.juvenile",
            "alt.binaries.erotica.yungsters",
            // split letters
            "alt.binaries.erotica.te.en",
            "alt.binaries.erotica.yo.ung",
            "alt.binaries.te.en.x.x.x",
            // other letters
            "alt.binaries.erotica.t\u{eb}en",
            "alt.binaries.erotica.te\u{308}en",
            "alt.binaries.erotica.t\u{435}\u{435}n",
            "alt.binaries.\u{e9}rotica.teen",
            // a video label and no title
            "alt.binaries.teen.movies",
            "alt.binaries.movies.teen",
            "alt.binaries.teen.videos",
            "alt.binaries.teen.clips",
            "alt.binaries.teen.mpegs",
            "alt.binaries.teen.videos.hd",
            "alt.binaries.hot.teen.movies",
            "alt.binaries.crazy.teen.movies",
            "alt.binaries.teenmovies",
            "alt.binaries.young.vids",
            "alt.binaries.youth.films",
            "alt.binaries.multimedia.teen",
            "alt.binaries.under18.movies",
        ] {
            assert!(group_blocked(g), "{g}");
        }
    }

    #[test]
    fn a_label_or_an_ambiguous_term_alone_is_fine() {
        for g in [
            // shows, bands and titles
            "alt.binaries.tv.teen-drama",
            "alt.binaries.multimedia.teen-wolf",
            "alt.binaries.tv.teen.shows",
            "alt.binaries.movies.teen-wolf",
            "alt.binaries.video.teen-titans",
            "alt.binaries.movies.young-sheldon",
            "alt.binaries.movies.youth-in-revolt",
            "alt.binaries.sounds.mp3.sonic-youth",
            "alt.binaries.sounds.mp3.neil-young",
            "alt.binaries.neil-young.videos",
            // children's things with no adult or picture label
            "alt.binaries.kids.tv",
            "alt.binaries.tv.kids",
            "alt.binaries.movies.kids",
            "alt.binaries.kids.movies",
            "alt.binaries.dvd.children",
            "alt.binaries.video.kinder",
            "alt.binaries.ebook.kinder",
            "alt.binaries.sounds.kinderlieder",
            // adult or picture labels with nothing about an age
            "alt.binaries.pictures.erotica",
            "alt.binaries.erotica",
            "alt.binaries.pictures",
            "alt.binaries.pictures.erotica.girls",
            "alt.binaries.erotica.girls",
            "alt.binaries.pictures.girls",
            "alt.binaries.erotica.18yo",
            "alt.binaries.erotica.21yo",
            "alt.binaries.erotica.2024",
            "alt.binaries.pictures.playboy",
            "alt.binaries.erotica.playboy",
            "alt.binaries.pictures.cowboys",
            "alt.binaries.pictures.nicole-kidman",
            "alt.binaries.pictures.kidney",
            "alt.binaries.pictures.juventus",
            "alt.binaries.pictures.nintendo",
            "alt.binaries.pictures.nina",
            "alt.binaries.pictures.south",
            "alt.binaries.pictures.mouth",
            "alt.binaries.pictures.4k",
            "alt.binaries.pictures.1080",
            "alt.binaries.pictures.u2",
            // disc images arent pictures
            "alt.binaries.cd.image",
            "alt.binaries.cd.image.kids",
            "alt.binaries.cd.image.12",
            "alt.binaries.dvd.image.gameboy",
            // numbers with no label
            "alt.binaries.multimedia.12",
            "alt.binaries.tv.16",
            "alt.binaries.movies.16",
            "alt.binaries.sounds.mp3.1950s",
            "alt.binaries.movies.2024",
            "alt.binaries.sounds.u2",
            "alt.binaries.sounds.under18",
            // split words that spell nothing
            "alt.binaries.under.agent",
            "alt.binaries.lol.italy",
            "alt.binaries.torpedo",
            "alt.binaries.movies",
            "alt.binaries.multimedia",
            "alt.binaries.teevee",
            "alt.binaries.boneless",
            "alt.binaries.mom",
            "",
        ] {
            assert!(!group_blocked(g), "{g}");
        }
    }
}
