//! What Atlas treats apart when indexing: posts obfuscated per article, gathered
//! into one release per upload, and groups labelled for minors, refused. The per-article rule is ported from newznab-tmux
//! (`HeaderParser::markPerArticleObfuscated`, `BUGS.md` #65), GPLv3.

use indexmap::IndexMap;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use rusqlite::{Connection, Result, functions::FunctionFlags};

use crate::parser::{Article, Release, parse_subject};

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
/// longer than this, a name isnt a real group: it counts as blocked, and costs nothing to judge
const MAX_GROUP_NAME: usize = 512;
/// no term that pieces can spell is longer than this
const MAX_JOINED: usize = 32;
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
    // a provider's LIST can send anything, and no real group name is this long
    if name.len() > MAX_GROUP_NAME {
        return true;
    }
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
                if piece.chars().count() > 7 || (taken >= 4 && !single) || word.len() + piece.len() > MAX_JOINED {
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

// ---------------------------------------------------------------- articles obfuscated per article

/// articles of one signature a slice has to hold before it can be judged, and
/// part positions its once-seen names have to span
pub const PER_ARTICLE_MIN: usize = 16;
/// letters and digits a name needs before it could be random
const TOKEN_LEN: usize = 16;

/// base name, part and part total of a subject: `<base> (12/45)`
static SUBJECT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(.+?)\s+\((\d+)\s*/\s*(\d+)\)").unwrap());
/// a file counter in a base name: `[03/62]`, `(3 of 62)`
pub(crate) static COUNTER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[\[(\s](\d{1,5})(?:/|[\s_]of[\s_]|-)(\d{1,5})[\])\[\s$:]").unwrap());

/// a run of letters and digits, with the separators random names carry: `<random>.<random>`
fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+' | b'~')
}

/// some name in `base` has enough letters and digits to be random
fn has_token(base: &str) -> bool {
    base.as_bytes()
        .split(|b| !is_name_byte(*b))
        .any(|run| run.iter().filter(|b| b.is_ascii_alphanumeric()).count() >= TOKEN_LEN)
}

/// `base` with its names collapsed: what stays the same over one upload
fn shape(base: &str) -> u64 {
    let (mut hash, mut in_name) = (0xcbf2_9ce4_8422_2325u64, false);
    for &b in base.as_bytes() {
        let name = is_name_byte(b);
        if !(name && in_name) {
            hash = (hash ^ u64::from(if name { b'x' } else { b })).wrapping_mul(0x100_0000_01b3);
        }
        in_name = name;
    }
    hash
}

/// `base` for comparing names: digit runs go unless they are in a 16+
/// character token, soo numbered volumes (`<hash>.part07.rar`) share a name
/// and random tokens stay whole
fn comparable(base: &str) -> String {
    let mut out = String::with_capacity(base.len());
    for (i, run) in base.split(|c: char| !c.is_ascii_alphanumeric()).enumerate() {
        if i > 0 {
            // what split on: the same width in every article of a signature
            out.push('.');
        }
        if run.len() >= TOKEN_LEN {
            out.push_str(run);
        } else {
            out.extend(run.chars().filter(|c| !c.is_ascii_digit()));
        }
    }
    out
}

/// subject shape, part total, file counter
type SignatureKey = (u64, i64, Option<(u32, u32)>);

#[derive(Default)]
struct Signature {
    /// name -> how many articles have it, the first one's index and part
    names: HashMap<String, (u32, usize, i64)>,
    articles: usize,
    total: i64,
}

/// Which of a slice's subjects are obfuscated per article: true = skip it.
///
/// nyuu's article obfuscation gives every article its own random subject
/// token, soo every article would become a release of one article that can
/// never complete. Multi-part subjects with a name of 16+ letters and digits
/// are grouped by subject shape, part total and file counter. A group with at
/// least `PER_ARTICLE_MIN` articles and at least 90% distinct names has its
/// once-seen names skipped, keeping first and last parts, when they span at
/// least `PER_ARTICLE_MIN` part positions.
pub fn mark_per_article_obfuscated<'a>(subjects: impl Iterator<Item = &'a str>) -> Vec<bool> {
    let mut signatures: HashMap<SignatureKey, Signature> = HashMap::new();
    let mut count = 0;
    for (i, subject) in subjects.enumerate() {
        count = i + 1;
        let Some(c) = SUBJECT_RE.captures(subject) else { continue };
        let base = c.get(1).map_or("", |m| m.as_str());
        let (Ok(part), Ok(total)) = (c[2].parse::<i64>(), c[3].parse::<i64>()) else { continue };
        if total < 2 || !has_token(base) || base.starts_with("Usenet Index Post") {
            continue;
        }
        let counter = COUNTER_RE.captures(base).and_then(|c| Some((c[1].parse().ok()?, c[2].parse().ok()?)));
        let signature = signatures.entry((shape(base), total, counter)).or_default();
        signature.total = total;
        signature.articles += 1;
        signature.names.entry(comparable(base)).and_modify(|n| n.0 += 1).or_insert((1, i, part));
    }

    let mut skip = vec![false; count];
    for signature in signatures.values() {
        // a stray duplicate must not shield the group; a real upload repeats on nearly every part
        if signature.articles < PER_ARTICLE_MIN || signature.names.len() * 10 < signature.articles * 9 {
            continue;
        }
        // repeated names are a real upload sharing the group. a first or last
        // part is kept: a real upload cut by the slice's edge can show just that one
        let once: Vec<(usize, i64)> = signature
            .names
            .values()
            .filter(|(n, _, part)| *n == 1 && *part != 1 && *part != signature.total)
            .map(|(_, i, part)| (*i, *part))
            .collect();
        // files posted round robin, each with its own random name, show one
        // article per file at only one or two positions
        if once.iter().map(|(_, part)| *part).collect::<HashSet<_>>().len() < PER_ARTICLE_MIN {
            continue;
        }
        for (i, _) in once {
            skip[i] = true;
        }
    }
    skip
}

/// A slice's articles without the ones obfuscated per article, and how many went.
pub fn drop_per_article_obfuscated(articles: Vec<Article>) -> (Vec<Article>, i64) {
    let skip = mark_per_article_obfuscated(articles.iter().map(|a| a.subject.as_str()));
    let before = articles.len();
    let kept: Vec<Article> = articles.into_iter().zip(skip).filter(|(_, s)| !s).map(|(a, _)| a).collect();
    let skipped = (before - kept.len()) as i64;
    (kept, skipped)
}

/// subject shape, part, part total, file counter
type UploadPiece = (u64, i64, i64, Option<(u32, u32)>);

/// One slice's articles, with those obfuscated per article taken out and gathered into a release per upload.
///
/// No name links them, but what stays the same over an upload does: its subject shape, poster and day. Each keeps its
/// position, and a file is its counter and part total, soo the release's NZB holds the upload as it was posted, and
/// later slices add to it. Two files of one size with no counter share a file row: see docs/TODO.md.
pub fn gather_per_article(group: &str, articles: Vec<Article>) -> (Vec<Article>, Vec<Release>, i64) {
    let skip = mark_per_article_obfuscated(articles.iter().map(|a| a.subject.as_str()));
    let (mut kept, mut gathered) = (Vec::with_capacity(articles.len()), Vec::new());
    for (article, gather) in articles.into_iter().zip(skip) {
        if gather { gathered.push(article) } else { kept.push(article) }
    }
    let count = gathered.len() as i64;

    // a poster that changes with nearly every article says nothing about the upload
    let mut posters: HashMap<u64, (HashSet<String>, usize)> = HashMap::new();
    let shapes: Vec<Option<UploadPiece>> = gathered
        .iter()
        .map(|a| {
            let c = SUBJECT_RE.captures(&a.subject)?;
            let base = c.get(1)?.as_str();
            let counter = COUNTER_RE.captures(base).and_then(|c| Some((c[1].parse().ok()?, c[2].parse().ok()?)));
            Some((shape(base), c[2].parse().ok()?, c[3].parse().ok()?, counter))
        })
        .collect();
    for (article, found) in gathered.iter().zip(&shapes) {
        if let Some((shape, ..)) = found {
            let seen = posters.entry(*shape).or_default();
            seen.0.insert(article.author.clone());
            seen.1 += 1;
        }
    }

    let mut uploads: IndexMap<String, Release> = IndexMap::new();
    for (mut article, found) in gathered.into_iter().zip(shapes) {
        let Some((shape, part, total, counter)) = found else { continue };
        let (names, seen) = &posters[&shape];
        let poster = if names.len() * 2 > *seen { "" } else { article.author.as_str() };
        let day = article.date.get(..10).unwrap_or(&article.date);
        let key = format!("{group}\0per-article\0{shape:x}\0{poster}\0{day}");
        let upload = uploads.entry(key.clone()).or_insert_with(|| Release {
            name: per_article_name(&key),
            collection: Some(key),
            ..Release::default()
        });
        let file = match counter {
            Some((n, files)) => format!("per-article.{n:05}of{files:05}.p{total}"),
            None => format!("per-article.p{total}"),
        };
        article.release_name = Some(upload.name.clone());
        article.filename = Some(file);
        article.part = Some(part);
        article.total_parts = Some(total);
        article.file_index = counter.map(|(n, _)| i64::from(n));
        article.file_total = counter.map(|(_, files)| i64::from(files));
        upload.size += article.bytes;
        upload.articles.push(article);
    }
    (kept, uploads.into_values().collect(), count)
}

/// Stable, in decimal: a run of hex would read as a random name, and leave search.
fn per_article_name(key: &str) -> String {
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3));
    format!("per-article-upload-{}-{}", hash >> 32, hash & 0xffff_ffff)
}

// ---------------------------------------------------------------- stored fragments

/// What a shard stores about a release and its file: `releases.name`,
/// `releases.parts` (0 when NULL), the number of `files` rows, and that row's
/// `subject_part`, `expected` and `subject`. None of it needs the file's
/// `segments` rows or its `blob`: `segments` is empty for a sealed file, and
/// `parts` counts what is in the blob too.
#[derive(Clone, Debug)]
pub struct StoredPost<'a> {
    pub name: &'a str,
    pub parts: i64,
    pub files: usize,
    pub part: Option<i64>,
    pub expected: Option<i64>,
    pub subject: Option<&'a str>,
}

/// a name that looks random with no neighbour to compare it to: a run of 16+
/// letters and digits, or two or more runs before the extension, each 6+ long
/// and mixing digits and letters
fn looks_random(name: &str) -> bool {
    let runs: Vec<&str> = name.split(|c: char| !c.is_ascii_alphanumeric()).filter(|r| !r.is_empty()).collect();
    if runs.iter().any(|r| r.len() >= TOKEN_LEN) {
        return true;
    }
    let mixed =
        |r: &&str| r.len() >= 6 && r.bytes().any(|b| b.is_ascii_digit()) && r.bytes().any(|b| b.is_ascii_alphabetic());
    let stems = if runs.last().is_some_and(|e| e.len() <= 4) { &runs[..runs.len() - 1] } else { &runs[..] };
    stems.len() >= 2 && stems.iter().all(mixed)
}

/// A stored release is a per-article fragment: one article in one file, whose
/// own subject claims a middle part of a bigger file (so never `(1/1)`, nor a
/// first or last part), `part` and `expected` agreeing with it, under a name
/// that looks random. The subject decides: `remap_bracket_parts` can leave
/// `expected` 3 and `part` 2 on a `(1/1)` subject. Nothing here looks at time:
/// the caller waits until nothing can still arrive for it.
pub fn stored_fragment(post: &StoredPost<'_>) -> bool {
    if post.parts != 1 || post.files != 1 {
        return false;
    }
    let Some(claim) = post.subject.and_then(parse_subject) else { return false };
    claim.part > 1
        && claim.part < claim.total_parts
        && post.part == Some(claim.part)
        && post.expected == Some(claim.total_parts)
        && looks_random(post.name)
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
    fn a_hostile_name_costs_nothing() {
        // a provider's LIST ACTIVE has no length limit: this kept 1.2 GiB before the caps
        let many = format!("alt.binaries.{}", "a.".repeat(249));
        let t = std::time::Instant::now();
        assert!(!group_blocked(&many), "under the limit, and spells nothing");
        assert!(t.elapsed() < std::time::Duration::from_secs(1));
        assert!(group_blocked(&"a.".repeat(2_000)), "past the limit");
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

    fn random(i: usize, len: usize) -> String {
        let mut x = (i as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let abc = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        (0..len)
            .map(|_| {
                x = x.wrapping_mul(0x100_0000_01b3) ^ (x >> 7);
                abc[(x % 62) as usize] as char
            })
            .collect()
    }

    fn skipped(subjects: &[String]) -> usize {
        mark_per_article_obfuscated(subjects.iter().map(String::as_str)).iter().filter(|s| **s).count()
    }

    #[test]
    fn the_real_shapes_are_caught() {
        let all = |s: Vec<String>, why: &str| assert_eq!(skipped(&s), s.len(), "{why}");
        all(
            (2..=40).map(|p| format!(r#"[41/62] "{}" yEnc ({p}/375)"#, random(p, 32))).collect(),
            "counter, quoted token",
        );
        all((2..=40).map(|p| format!(r#""{}" yEnc ({p}/5747)"#, random(p, 24))).collect(), "no counter");
        all(
            (1000..1040)
                .map(|p| format!(r#"[17/36] - "{}.{}" yEnc ({p}/59) 2088881050"#, random(p, 8), random(p + 9000, 8)))
                .collect(),
            "past the total, a dot and no 16 long run",
        );
        let dotted: Vec<String> = (2..=40)
            .map(|p| format!(r#"[17/36] - "{}.{}" yEnc ({p}/59) 2088881050"#, random(p, 8), random(p + 9000, 8)))
            .collect();
        assert!(parse_subject(&dotted[0]).is_some(), "these do become releases today");
        all(dotted, "within the total");
        let bare: Vec<String> =
            (2..=40).map(|p| format!("[{}] - {} yEnc ({p}/1250)", random(p, 2), random(p, 40))).collect();
        assert!(parse_subject(&bare[0]).is_none());
        all(bare, "no quotes");
        all(
            (2..=40)
                .map(|p| {
                    let sep = if p % 2 == 0 { "-" } else { "_" };
                    format!(r#"[3/9] "{}{sep}{}" yEnc ({p}/200)"#, random(p, 9), random(p + 500, 9 + p % 3))
                })
                .collect(),
            "a separator that varies",
        );
    }

    #[test]
    fn real_uploads_are_kept() {
        let none = |s: Vec<String>, why: &str| assert_eq!(skipped(&s), 0, "{why}");
        none(
            (1..=100).map(|p| format!(r#"[1/5] "a1B2c3D4e5F6g7H8i9J0.bin" yEnc ({p}/100)"#)).collect(),
            "one name on every part",
        );
        none(
            (1..=40).map(|p| format!(r#""a1B2c3D4e5F6g7H8i9J0.part{p:02}.rar" yEnc ({p}/50)"#)).collect(),
            "numbered volumes",
        );
        none(
            (0..40).map(|f| format!(r#""{}" yEnc ({}/50)"#, random(f, 32), 7 + f % 2)).collect(),
            "round robin, two positions",
        );
        none((2..=10).map(|p| format!(r#""{}" yEnc ({p}/5747)"#, random(p, 32))).collect(), "under the minimum");
        none((0..50).map(|f| format!(r#""{}" yEnc (1/1)"#, random(f, 32))).collect(), "single part posts");
    }

    #[test]
    fn repeats_and_first_and_last_parts_stay() {
        let mut s: Vec<String> = (2..=40).map(|p| format!(r#""{}" yEnc ({p}/50)"#, random(p, 32))).collect();
        let hits = s.len();
        s.push(format!(r#""{}" yEnc (1/50)"#, random(1000, 32)));
        s.push(format!(r#""{}" yEnc (50/50)"#, random(1001, 32)));
        s.push(r#""a1B2c3D4e5F6g7H8i9J0" yEnc (5/50)"#.into());
        s.push(r#""a1B2c3D4e5F6g7H8i9J0" yEnc (6/50)"#.into());
        assert_eq!(skipped(&s), hits);
    }

    #[test]
    fn a_share_under_90_percent_keeps_the_group() {
        let mut s: Vec<String> = (2..=20).map(|p| format!(r#""{}" yEnc ({p}/50)"#, random(p, 32))).collect();
        s.extend((21..=40).map(|p| format!(r#""a1B2c3D4e5F6g7H8i9J0" yEnc ({p}/50)"#)));
        assert_eq!(skipped(&s), 0);
    }

    fn post<'a>(name: &'a str, subject: &'a str, part: i64, expected: i64) -> StoredPost<'a> {
        StoredPost { name, parts: 1, files: 1, part: Some(part), expected: Some(expected), subject: Some(subject) }
    }

    const TOKEN: &str = "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6";

    #[test]
    fn a_fragment_is_one_middle_part_under_a_random_name() {
        let s = format!(r#"[41/62] "{TOKEN}" yEnc (316/375)"#);
        assert!(stored_fragment(&post(TOKEN, &s, 316, 375)));
        let s = format!(r#""{TOKEN}" yEnc (2441/5747)"#);
        assert!(stored_fragment(&post(TOKEN, &s, 2441, 5747)));
        let dotted = "x7Gh2kLq.p9Zt3vBn";
        let s = format!(r#"[17/36] - "{dotted}" yEnc (40/59) 2088881050"#);
        assert!(stored_fragment(&post(dotted, &s, 40, 59)));
    }

    #[test]
    fn what_is_not_a_fragment() {
        let middle = format!(r#""{TOKEN}" yEnc (7/9)"#);
        let good = || post(TOKEN, &middle, 7, 9);
        assert!(stored_fragment(&good()), "the rows below each change one thing");

        // (1/1) never, whatever the columns say
        let one = format!(r#"[2/3] "{TOKEN}" yEnc (1/1)"#);
        assert!(!stored_fragment(&post(TOKEN, &one, 1, 1)));
        assert!(!stored_fragment(&post(TOKEN, &one, 2, 3)), "remapped by its bracket counter");
        // first and last parts
        let first = format!(r#""{TOKEN}" yEnc (1/9)"#);
        assert!(!stored_fragment(&post(TOKEN, &first, 1, 9)));
        let last = format!(r#""{TOKEN}" yEnc (9/9)"#);
        assert!(!stored_fragment(&post(TOKEN, &last, 9, 9)));
        // a plain name
        let plain = r#""Some.Movie.2024.1080p.mkv" yEnc (7/9)"#;
        assert!(!stored_fragment(&post("Some.Movie.2024.1080p.mkv", plain, 7, 9)));
        assert!(!stored_fragment(&post("S01E01.x264hd.1080p.mkv", plain, 7, 9)), "1080p is too short");
        // more than one article or file, or none
        assert!(!stored_fragment(&StoredPost { parts: 2, ..good() }));
        assert!(!stored_fragment(&StoredPost { parts: 0, ..good() }));
        assert!(!stored_fragment(&StoredPost { files: 2, ..good() }));
        // the columns disagree with the subject
        assert!(!stored_fragment(&post(TOKEN, &middle, 6, 9)));
        assert!(!stored_fragment(&post(TOKEN, &middle, 7, 8)));
        // nothing to go on (a save held it back)
        assert!(!stored_fragment(&StoredPost { subject: None, ..good() }));
        assert!(!stored_fragment(&StoredPost { part: None, ..good() }));
        assert!(!stored_fragment(&StoredPost { expected: None, ..good() }));
    }

    #[test]
    fn a_sealed_fragment_reads_the_same_as_a_loose_one() {
        use crate::parser::Release;
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        crate::db::create_db_at(&main).unwrap();
        let group = "alt.binaries.test";
        let article = Article {
            subject: format!(r#""{TOKEN}" yEnc (5/9)"#),
            filename: Some(TOKEN.into()),
            part: Some(5),
            total_parts: Some(9),
            message_id: "<a@x>".into(),
            bytes: 700_000,
            ..Default::default()
        };
        let release =
            Release { name: TOKEN.into(), group: group.into(), articles: vec![article], ..Default::default() };
        crate::store::save(&main, &[release]).unwrap();
        let conn = crate::db::open_shard(&crate::store::shard_path(&main, crate::store::shard_of(group))).unwrap();

        let read = || {
            conn.query_row(
                "select r.name, r.parts, f.subject_part, f.expected, f.subject,
                     (select count(*) from files x where x.release_id = r.id)
                 from releases r join files f on f.release_id = r.id",
                [],
                |r| {
                    let (name, subject): (String, String) = (r.get(0)?, r.get(4)?);
                    Ok(stored_fragment(&StoredPost {
                        name: &name,
                        parts: r.get(1)?,
                        files: r.get::<_, i64>(5)? as usize,
                        part: r.get(2)?,
                        expected: r.get(3)?,
                        subject: Some(&subject),
                    }))
                },
            )
            .unwrap()
        };
        assert!(read(), "loose");
        let file: i64 = conn.query_row("select id from files", [], |r| r.get(0)).unwrap();
        crate::store::seal_file(&conn, file).unwrap();
        let rows: i64 = conn.query_row("select count(*) from segments", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 0, "sealed: its article is only in the blob");
        assert!(read(), "sealed");
    }

    fn obfuscated_article(part: usize, token: &str, counter: &str, author: &str) -> Article {
        Article {
            subject: format!(r#"{counter}"{token}" yEnc ({part}/500)"#),
            author: author.to_string(),
            date: "2026-11-01 10:00:00".to_string(),
            message_id: format!("<{token}@x>"),
            bytes: 100,
            ..Article::default()
        }
    }

    fn upload(from: usize, to: usize, counter: &str, author: impl Fn(usize) -> String) -> Vec<Article> {
        (from..to)
            .map(|p| obfuscated_article(p, &format!("{p:04}abcdefghijklmnopqrstuvwx"), counter, &author(p)))
            .collect()
    }

    #[test]
    fn an_upload_is_gathered_with_its_positions() {
        let (kept, uploads, count) = gather_per_article("alt.binaries.t", upload(2, 42, "", |_| "p@x".into()));
        assert_eq!((kept.len(), uploads.len(), count), (0, 1, 40));
        let release = &uploads[0];
        assert_eq!(release.size, 4_000);
        assert_eq!(release.articles[0].part, Some(2));
        assert_eq!(release.articles[0].total_parts, Some(500));
        assert_eq!(release.articles[0].filename.as_deref(), Some("per-article.p500"));
        assert!(!crate::parser::is_obfuscated(&release.name), "{}", release.name);
    }

    #[test]
    fn later_slices_of_an_upload_get_the_same_release() {
        let name =
            |from, to| gather_per_article("alt.binaries.t", upload(from, to, "", |_| "p@x".into())).1[0].name.clone();
        assert_eq!(name(2, 42), name(50, 90));
        let elsewhere = gather_per_article("alt.binaries.u", upload(2, 42, "", |_| "p@x".into())).1[0].name.clone();
        assert_ne!(name(2, 42), elsewhere, "not across groups");
        let other = gather_per_article("alt.binaries.t", upload(2, 42, "", |_| "q@x".into())).1[0].name.clone();
        assert_ne!(name(2, 42), other, "not across posters");
    }

    #[test]
    fn a_poster_that_changes_each_article_doesnt_split_it() {
        let (_, uploads, _) = gather_per_article("alt.binaries.t", upload(2, 42, "", |p| format!("{p}@x")));
        assert_eq!(uploads.len(), 1);
    }

    #[test]
    fn a_counter_keeps_an_uploads_files_apart() {
        let mut articles = upload(2, 42, "[1/2] ", |_| "p@x".into());
        articles.extend(upload(2, 42, "[2/2] ", |_| "p@x".into()).into_iter().map(|mut a| {
            a.message_id.push('2');
            a
        }));
        let (_, uploads, _) = gather_per_article("alt.binaries.t", articles);
        assert_eq!(uploads.len(), 1, "one upload");
        let files: HashSet<_> = uploads[0].articles.iter().map(|a| a.filename.clone().unwrap()).collect();
        assert_eq!(files.len(), 2, "{files:?}");
    }

    #[test]
    fn a_real_upload_is_left_alone() {
        let articles: Vec<Article> = (1..=30)
            .map(|p| Article {
                subject: format!(r#"[1/1] - "Real.Upload.2026.mkv" yEnc ({p}/30)"#),
                ..Article::default()
            })
            .collect();
        let (kept, uploads, count) = gather_per_article("alt.binaries.t", articles);
        assert_eq!((kept.len(), uploads.len(), count), (30, 0, 0));
    }
}
