# Part 1: Ingest filters

Depends on: nothing. Owns `src/filters.rs`. Base commit for every Atlas line cited here: `de598b6`. Reference paths are in newznab-tmux at the commit pinned in [00-overview.md](00-overview.md).

Every code block in this plan was compiled and run on an isolated copy of `de598b6` with `cargo test --locked` and `cargo clippy --all-targets --locked -- -D warnings`. `cargo fmt` may rewrap it; run `just format` before each commit.

## 1. Goal

Atlas stops storing uploads obfuscated per article, which today become one release per article that can never complete, and it never indexes, fetches from or serves a group whose name labels it for minors. Rows already stored from such a group stop being served the moment the new build runs, before any purge. Both filters are always on, and the indexer's stats and dashboards count what they did.

## 2. Non-goals

- Deleting anything. Stored fragments and blocked rows stay on disk until Part 3's `atlas --purge`.
- A generic binary blacklist. `database/seeders/BinaryblacklistTableSeeder.php` has 18 rules: 13 are disabled (`status` 0), and the 5 enabled ones cover only `alt.binaries.multimedia.korean` and `korea.binaries.movies`. Porting `BlacklistService::isBlackListed` (`:65`) means a per-article regex pass, a whitelist and a way to edit rules, for nothing the 118M-release problem needs.
- Remembering a signature across slices. See decision 4.
- Judging subjects or message bodies for minor labels. The block goes by group name only.
- Part repair. Atlas has none, so upstream's "disable for sparse repair ranges" has nothing to apply to.

## 3. Source map

| Upstream | Take | Leave behind |
|---|---|---|
| `BUGS.md:477-509` (#65) | Why the rule exists, its guards, and that header-only indexing can't reconstruct these uploads | FFI routing and the PHP/Rust benchmarks (`:493-509`) |
| `app/Services/Binaries/HeaderParser.php:20` | Base, part and total of a subject | The lookahead; `base.starts_with` does it |
| `HeaderParser.php:25` | File counter pattern, in `COUNTER_RE` below | |
| `HeaderParser.php:92-168` | The rule: signature `:133`, comparable name `:126-131`, 90% share `:143`, once-seen and first/last `:150-155`, position span `:159` | `CBP_PER_ARTICLE_OBFUSCATION_MIN_ARTICLES` and its 0-disables switch |
| `HeaderParser.php:341-345` | Skipped articles count as received | The `received` list; Atlas has no gap tracking |
| `app/Services/BlacklistService.php`, `database/seeders/BinaryblacklistTableSeeder.php` | Nothing, see Non-goals | All of it |

## 4. Atlas changes

### 4.1 The per-article rule, and where upstream's rule falls short

Checked against the shapes seen in the wild:

| Shape | `parse_subject` (`src/parser.rs:83-125`) | Upstream's rule | This plan |
|---|---|---|---|
| `[41/62] "<32 random>" yEnc (316/375)` | Parses | Caught | Caught |
| `"<random>" yEnc (2441/5747)` | Parses | Caught | Caught |
| `[17/36] - "<random>.<random>" yEnc (1983/59) 2088881050` | `None` at `:107`, part past total | Only if one side has a 16+ run | Caught |
| The same with `(40/59)`, within the total | Parses, so it becomes a release today | Only if one side has a 16+ run | Caught |
| `"<9 chars>-<10 chars>"`, separators varying per article | Parses | Escapes twice: no 16+ run, and the shape varies per article | Caught |
| `[<2 chars>] - <40 random> yEnc (n/1250)` | `None`, no pattern matches | Caught | Caught |

So the rule extends upstream's in two ways, and only two:

1. **The token test counts letters and digits across a whole name.** A run of `[A-Za-z0-9._+~-]` needs 16+ alphanumerics, where upstream needs 16 in a row. `x7Gh2kLq.p9Zt3vBn` qualifies.
2. **The shape collapses those separators with the letters and digits.** Upstream collapses `[A-Za-z0-9]+`, so a random `-` or `_` gives every article its own signature, and none reaches the minimum.

It must run on raw subjects, before `parse_subject`. Articles past the total never become releases, but they must count toward their signature: a slice holding 1,000 past-total articles and 10 within-total ones has to drop those 10 as well.

### 4.2 Where it runs

`save_slice` (`src/indexer.rs:1602`), inside the `spawn_blocking` at `:1609-1612`, on the articles before `group_articles`. `Progress.articles` stays `headers.len()` (`:1603`), so skipped articles count as received. Cursors move on `done == total` (`:1581`) and never look at content, `slice_date` and `in_window` read the headers before `save_slice` (`:1537-1542`), and Atlas keeps no gap list, so a skipped article cannot appear as a gap. An all-skipped slice saves nothing: `Db::save` returns on an empty list (`:348`).

`src/ai.rs:172` filters the same way.

### 4.3 The block, layer by layer

| Layer | Where (at `de598b6`) | Behavior |
|---|---|---|
| Group selection | `Config::tracked_groups`, `src/config.rs:254-262` | Blocked names are left out, the `group` fallback too. This feeds `Scheduler.groups` (`src/bg_indexer.rs:266`, set at `:647,703`), and so `take_group` (`:307`), `take_chunk` (`:468`), `take_sweep` (`:500`) and `chunk_groups` |
| Telling the user | New `Config::blocked_groups`; `run_servers` (`src/bg_indexer.rs:703`), `watch_config` (`:647`) | A log line at start and whenever the set changes; `groups_blocked` in `stats.json` |
| Groups menu | `src/groups_menu.rs:191-197`, `:163` | Filtered before `results_page`, so `page_nonempty` (`:20`) never sends GROUP for one. A warning per configured blocked group, and a count of hidden matches |
| AI search | `src/ai.rs:126,147` | Not offered to the model, never fetched |
| Indexing pass | `run_pass` `src/indexer.rs:488`, `run_sweep` `:890`, `process_range_dated` `:1482`, `index_group_with` `:1735` | Returns before any NNTP command. `index_chunk` is reached only through a claim filtered by `tracked_groups`, and stops at `process_range_dated` if config changed mid-chunk |
| Name lookups | `save_slice` `:1602`, before `first_names` `:1619` | No body is fetched |
| Saving | `save_in_transaction`, `src/store.rs:1376` | Releases of a blocked group are dropped. Covers `store::save` (`:1530`) and `db::save_releases_bulk` |
| Search | `VISIBLE_RELEASE`, `src/search.rs:36-39` | Used by `search` (`:113`), `count` (`:156`), `all_releases` (`:226`), both the FTS and LIKE paths |
| Obfuscated lists | `src/search.rs:230,234` | Same filter |
| By id | `get_release_with` `src/search.rs:247`, so `get_release` `:242`; `get_articles` `:264`; `recent_in_groups` `:252` | `None`, empty, or the group left out |
| API | `src/api.rs:189-193`, `get` `:242-251` | No code change: search filters, and `get_release` returning `None` gives the existing 404 |
| NZB and download | `src/nzb.rs:107,119`, `src/download.rs:12` | No code change: both start at `get_release` |
| Stats dashboard | `compute_content`, `src/stats_dashboard.rs:172,200` | Blocked groups are left out of the per-group totals and "biggest release". The FTS file-type counts at `:214-222` show no names and stay as they are |

`store::articles` and `store::each_article` (`src/store.rs:1547,1559`) are **not** filtered: compaction's check reads through them (`src/compact.rs:998,1024`) and must see every row. `get_articles` is the guard for serving.

Nothing rewrites `config.json`. A blocked group stays in `groups` and shows in "Remove group" (`src/app.rs:587`), so the user can remove it.

### 4.4 The block rule

Words are lowercase, with accents, combining marks and look-alike letters folded to plain ones, split on anything that isn't a letter or digit. Each word is also read with digits as letters (`t33n`, `y0ung`), and short neighbouring words are also read together, exactly: any run of single letters (`p.r.e.t.e.e.n`), and up to 4 pieces of 7 characters or fewer (`te.en`, `l0.lita`). Plurals in `s`, `z` and `es` count as the word.

| Blocked when the name has | Examples, all synthetic |
|---|---|
| An age under 18 in years, outright | `14yo`, `14-yo` |
| A term that's unambiguous alone | `preteen`, `lolita`, `lolis`, `jail.bait`, `kinderporn`, `childlover` |
| A word for an age (`teen`, `tween`, `youth`, `young`, `yung`, one edit from `young`) next to an adult or picture label | `erotica.teens`, `pictures.youth`, `yong.eroitca` |
| A word for a child (`child*`, `kid`, `kids*`, `kidz*`, `kiddie*`, `boy`, `*boy`, `kinder*`, `enfant*`, `nino`, `ninas`, `adolesc*`, `juvenil*`, `minor`) next to an adult or picture label | `nude.kids`, `erotica.boyz`, `pictures.kinder` |
| A number from 5 to 17, a limit up to 21, or a count of years, next to an adult or picture label | `erotica.12-15`, `pictures.14`, `erotica.under18`, `erotica.14yr` |
| A word built on `girl`, not `girl` or `girls` itself, next to an adult label | `erotica.girlie` |
| `teen` with a video label and no title after it | `teen.movies`, `movies.teen`, `crazy.teen.movies`, `teenmovies` |
| `young` or `youth` with a video label and no title anywhere | `young.vids`, `youth.films` |
| A limit or a count of years with a video label | `under18.movies` |
| `teen`, `young` or `youth` as all the name says | `alt.binaries.teens` |

Left alone, each pinned in `a_label_or_an_ambiguous_term_alone_is_fine`:

| Name | Why |
|---|---|
| `tv.teen-drama`, `movies.teen-wolf`, `movies.young-sheldon`, `neil-young.videos` | No adult or picture label, and a title word beside the video label |
| `movies.kids`, `kids.movies`, `dvd.children`, `video.kinder`, `sounds.kinderlieder` | Child words pair with adult and picture labels, never video: children's video groups are ordinary |
| `erotica.girls`, `pictures.girls`, `erotica.21yo`, `erotica.2024` | An adult label with nothing about a minor |
| `pictures.playboy`, `pictures.cowboys`, `pictures.nicole-kidman`, `pictures.juventus`, `pictures.nina` | A word that only contains a child word |
| `cd.image.kids`, `cd.image.12` | `image` next to `cd`, `dvd`, `iso`, `disc` or `disk` means a disc image, not a picture |
| `movies.16`, `tv.16`, `multimedia.12`, `sounds.under18` | A number or limit with no adult or picture label |
| `under.agent`, `lol.italy` | Reading pieces together only counts on an exact match, so these spell nothing |

### 4.5 The code

Create `src/filters.rs`. Add `pub mod filters;` to `src/lib.rs` after `download`. In `Cargo.toml` line 25, change the rusqlite features to `["bundled", "functions", "hooks"]`; this adds no dependency, so `Cargo.lock` is unchanged and `--locked` still passes.

```rust
//! What Atlas refuses to index: posts obfuscated per article, and groups
//! labelled for minors. The per-article rule is ported from newznab-tmux
//! (`HeaderParser::markPerArticleObfuscated`, `BUGS.md` #65), GPLv3.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use rusqlite::{Connection, Result, functions::FunctionFlags};

use crate::parser::{Article, parse_subject};

// ---------------------------------------------------------------- blocked groups

/// on their own, these label a group for minors (a token containing one)
const MINOR_ROOTS: [&str; 13] = [
    "preteen", "lolita", "lolicon", "shotacon", "jailbait", "underage", "nymphet", "childporn", "kidporn",
    "kinderporn", "childlove", "boylove", "girllove",
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
    "movie", "video", "vid", "clip", "mpeg", "mpg", "avi", "mkv", "mp4", "wmv", "film", "flix", "cam", "webcam",
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
```

### 4.6 The stored-row predicate, precisely

`stored_fragment` is true exactly when all of these hold:

1. `releases.parts = 1`. A NULL `parts` reads as 0 and fails.
2. The release has exactly one `files` row.
3. `files.subject` is not NULL and `parse_subject` accepts it. A save that held the article back leaves it NULL (`src/store.rs:1430-1438`), and the answer is false.
4. The subject's own numbers satisfy `1 < part < total`. This is what makes `(1/1)` impossible, and it needs `total >= 3`.
5. `files.subject_part` equals that part and `files.expected` equals that total.
6. `looks_random(releases.name)`. After Part 2 a name can end in ` [12345678]` when its first choice was taken. Those digits aren't a mixed run, so a `<random>.<random>` name with that ending reads as not random, which errs toward keeping the row. A 16+ run still matches.

**Why the subject and not just the columns.** `remap_bracket_parts` (`src/parser.rs:129-172`) rewrites `[2/3] "x.bin" yEnc (1/1)` to part 2 of 3 when a slice holds two articles of that name, and if both carry one message-id only one is stored (`src/store.rs:1429`). That row has `parts = 1`, `subject_part = 2`, `expected = 3`. Going by the columns alone, it is a middle part of 3, while its subject says `(1/1)`. Test `what_is_not_a_fragment` pins this.

**Sealed files.** A sealed file has no `segments` rows, so counting them would say 0 articles. The rule reads only `releases` and `files` columns, which sealing keeps (`seal_rows`, `src/store.rs:815-902`). `a_sealed_fragment_reads_the_same_as_a_loose_one` checks it end to end.

**False-positive risk.**
- A real upload of which Atlas holds exactly one middle article under a random-looking name. It's unusable as it stands, one part of at least three, so nothing the user can use is lost. The loss is real only if the rest is indexed later, after the row was deleted: those parts then start a new row with no part `p`, and PAR2 usually repairs that. Part 3's `judge` rejects on this predicate with no clock, and rejecting deletes nothing: a late article judges the row again. Only `atlas --purge` deletes, and it waits `dead_after_secs` (48 h from the last touch) first, which is what bounds this loss.
- A real name with a 16+ run, such as one long unbroken title. Same reasoning, same bound.
- It can't touch a release with two or more articles, two or more files, a `(1/1)` subject, or a first or last part.

**False negatives.** Purge relies on this predicate, so it's kept narrow, and it misses these. They stay as rows, as they are today:

- First and last parts: about `2/total` of a post's fragments.
- **About 43% of `<8>.<8>` names.** An 8-character base-62 stem mixes letters and digits with probability `1 − (52/62)^8 − (10/62)^8` ≈ 0.755, so both stems do ≈ 0.57 of the time. The other 43% read as not random. Names with a 16+ run are never affected, and neither is the slice rule, which has no randomness test: it catches these at ingest, so only rows already stored are left.
- Names under 16 alphanumerics of any other shape.

## 5. Schema and migration

None. No table, column, index or `meta` key. `atlas_blocked` exists per connection, in `db::set_up` (`src/db.rs:58-67`), and no stored view, index or trigger refers to it, so a shard opened by an older build, by `--compact`, or by `convert.rs` needs nothing. Compaction, conversion and wipe are unaffected, and no blocked row is dropped by them.

## 6. Interfaces

**Provided**, `src/filters.rs`. Parts 3–8 call the first and Part 3 the last two.

```rust
pub fn group_blocked(name: &str) -> bool;
pub fn register_sql(conn: &rusqlite::Connection) -> rusqlite::Result<()>;   // sql: atlas_blocked(group_name)
pub const PER_ARTICLE_MIN: usize = 16;
pub fn mark_per_article_obfuscated<'a>(subjects: impl Iterator<Item = &'a str>) -> Vec<bool>;
pub fn drop_per_article_obfuscated(articles: Vec<Article>) -> (Vec<Article>, i64);
pub struct StoredPost<'a> { pub name: &'a str, pub parts: i64, pub files: usize, pub part: Option<i64>,
                            pub expected: Option<i64>, pub subject: Option<&'a str> }
pub fn stored_fragment(post: &StoredPost<'_>) -> bool;
```

Elsewhere: `Config::blocked_groups(&self) -> Vec<String>`, and `indexer::Progress` gains `pub skipped: i64`. `pub(crate) static COUNTER_RE: LazyLock<Regex>` is for Part 2, which uses this one rather than a copy.

**Part 3.** `judge`'s file query (its section 6, step 4) reads `subject` and passes it. See decision 20.

**Consumed:** `parser::parse_subject`, `parser::Article`.

## 7. Config

None. Neither filter has a key, and overview rule 8 makes them the exception that is on by default. Nothing is written to `config.json`.

## 8. Tasks

Work in `.worktrees/port-01` on `port/01-ingest-filters`, per the overview. Run tests through `just` or `cargo test --locked`.

### Task 1: The block list

**Files:** create `src/filters.rs` (the blocked-groups half of 4.5, with `register_sql`), modify `src/lib.rs`, `Cargo.toml`.

- [ ] **Test first**, in `src/filters.rs`. Group names here are synthetic, written for this test:

```rust
#[test]
fn unambiguous_terms_block_on_their_own() {
    for g in [
        "alt.binaries.preteen", "alt.binaries.pre-teen", "alt.binaries.lolita", "alt.binaries.l0lita",
        "alt.binaries.loli", "alt.binaries.lolis", "alt.binaries.tv.jailbait", "alt.binaries.jail.bait",
        "alt.binaries.underaged", "alt.binaries.under-age", "alt.binaries.pedo", "alt.binaries.pedofilia",
        "alt.binaries.child.porn", "alt.binaries.kinderporn", "alt.binaries.p.r.e.t.e.e.n",
        "alt.binaries.pictures.p.r.e.t.e.e.n", "alt.binaries.l0.lita", "alt.binaries.lo.li.ta",
        // an age in years, said outright
        "alt.binaries.14yo", "alt.binaries.pictures.9yo", "alt.binaries.erotica.14yo", "alt.binaries.14-yo",
    ] {
        assert!(group_blocked(g), "{g}");
    }
}

#[test]
fn ambiguous_terms_need_a_label() {
    for g in [
        "alt.binaries.pictures.erotica.teen", "alt.binaries.erotica.teens", "alt.binaries.teen.erotica",
        "alt.binaries.young.erotica", "alt.binaries.yong.eroitca", "alt.binaries.yung.erotic",
        "alt.binaries.youngerotica", "alt.binaries.eroticateens", "ALT.BINARIES.PICTURES.EROTICA.TEEN",
        "alt.binaries.t33n.er0tica", "alt.binaries.y0ung.xxx", "alt.binaries.pictures.youth",
        "alt.binaries.pictures.teen", "alt.binaries.pics.teens", "alt.binaries.pictures.young",
        "alt.binaries.pictrues.yong", "alt.binaries.little.girls.pictures", "alt.binaries.schoolgirls.erotica",
        "alt.binaries.nude.kids", "alt.binaries.teens", "alt.binaries.young", "alt.binaries.youth",
        "alt.binaries.pictures.seventeen",
        // numbers and limits as an age, next to a label
        "alt.binaries.erotica.under18", "alt.binaries.erotica.u18", "alt.binaries.erotica.under.18",
        "alt.binaries.erotica.12-15", "alt.binaries.pictures.14", "alt.binaries.erotica.14yr",
        // words built on a child word
        "alt.binaries.erotica.childlover", "alt.binaries.erotica.kidz", "alt.binaries.erotica.boyz",
        "alt.binaries.erotica.childs", "alt.binaries.pictures.kinder", "alt.binaries.erotica.enfants",
        "alt.binaries.erotica.ninos", "alt.binaries.erotica.schoolboy", "alt.binaries.erotica.littlegirl",
        "alt.binaries.erotica.girlie", "alt.binaries.erotica.adolescent", "alt.binaries.erotica.juvenile",
        "alt.binaries.erotica.yungsters",
        // split letters
        "alt.binaries.erotica.te.en", "alt.binaries.erotica.yo.ung", "alt.binaries.te.en.x.x.x",
        // other letters
        "alt.binaries.erotica.t\u{eb}en", "alt.binaries.erotica.te\u{308}en",
        "alt.binaries.erotica.t\u{435}\u{435}n", "alt.binaries.\u{e9}rotica.teen",
        // a video label and no title
        "alt.binaries.teen.movies", "alt.binaries.movies.teen", "alt.binaries.teen.videos",
        "alt.binaries.teen.clips", "alt.binaries.teen.mpegs", "alt.binaries.teen.videos.hd",
        "alt.binaries.hot.teen.movies", "alt.binaries.crazy.teen.movies", "alt.binaries.teenmovies",
        "alt.binaries.young.vids", "alt.binaries.youth.films", "alt.binaries.multimedia.teen",
        "alt.binaries.under18.movies",
    ] {
        assert!(group_blocked(g), "{g}");
    }
}

#[test]
fn a_label_or_an_ambiguous_term_alone_is_fine() {
    for g in [
        // shows, bands and titles
        "alt.binaries.tv.teen-drama", "alt.binaries.multimedia.teen-wolf", "alt.binaries.tv.teen.shows",
        "alt.binaries.movies.teen-wolf", "alt.binaries.video.teen-titans", "alt.binaries.movies.young-sheldon",
        "alt.binaries.movies.youth-in-revolt", "alt.binaries.sounds.mp3.sonic-youth",
        "alt.binaries.sounds.mp3.neil-young", "alt.binaries.neil-young.videos",
        // children's things with no adult or picture label
        "alt.binaries.kids.tv", "alt.binaries.tv.kids", "alt.binaries.movies.kids", "alt.binaries.kids.movies",
        "alt.binaries.dvd.children", "alt.binaries.video.kinder", "alt.binaries.ebook.kinder",
        "alt.binaries.sounds.kinderlieder",
        // adult or picture labels with nothing about an age
        "alt.binaries.pictures.erotica", "alt.binaries.erotica", "alt.binaries.pictures",
        "alt.binaries.pictures.erotica.girls", "alt.binaries.erotica.girls", "alt.binaries.pictures.girls",
        "alt.binaries.erotica.18yo", "alt.binaries.erotica.21yo", "alt.binaries.erotica.2024",
        "alt.binaries.pictures.playboy", "alt.binaries.erotica.playboy", "alt.binaries.pictures.cowboys",
        "alt.binaries.pictures.nicole-kidman", "alt.binaries.pictures.kidney", "alt.binaries.pictures.juventus",
        "alt.binaries.pictures.nintendo", "alt.binaries.pictures.nina", "alt.binaries.pictures.south",
        "alt.binaries.pictures.mouth", "alt.binaries.pictures.4k", "alt.binaries.pictures.1080",
        "alt.binaries.pictures.u2",
        // disc images arent pictures
        "alt.binaries.cd.image", "alt.binaries.cd.image.kids", "alt.binaries.cd.image.12",
        "alt.binaries.dvd.image.gameboy",
        // numbers with no label
        "alt.binaries.multimedia.12", "alt.binaries.tv.16", "alt.binaries.movies.16",
        "alt.binaries.sounds.mp3.1950s", "alt.binaries.movies.2024", "alt.binaries.sounds.u2",
        "alt.binaries.sounds.under18",
        // split words that spell nothing
        "alt.binaries.under.agent", "alt.binaries.lol.italy", "alt.binaries.torpedo", "alt.binaries.movies",
        "alt.binaries.multimedia", "alt.binaries.teevee", "alt.binaries.boneless", "alt.binaries.mom", "",
    ] {
        assert!(!group_blocked(g), "{g}");
    }
}
```

- [ ] **Run:** `cargo test --locked --lib filters`. **Expected:** compile error, `group_blocked` missing.
- [ ] **Implement** per 4.5.
- [ ] **Run:** the same. **Expected:** 3 tests pass. `just lint` passes.
- [ ] **Commit:** `Block groups whose names label them for minors`

### Task 2: Stop serving blocked rows

**Files:** `src/db.rs:58-67`, `src/search.rs`, `src/stats_dashboard.rs`; create `tests/blocked_groups.rs`.

- [ ] **Test first.** A single `#[test] fn a_blocked_group_is_never_indexed_or_served` in `tests/blocked_groups.rs`, with `mod common;`, setting `ATLAS_HOME` and `ATLAS_NO_KEYRING` in an `unsafe` block as `tests/e2e.rs:16-19` does, then `db::create_db().unwrap()`. It writes rows the way an older build left them, since this build's store refuses them:

```rust
const BLOCKED: &str = "alt.binaries.pictures.erotica.teen";
const OPEN: &str = "alt.binaries.open";

fn old_row(main: &std::path::Path, group: &str, seq: i64, name: &str, obfuscated: bool) -> i64 {
    let shard = store::shard_of(group);
    let id = store::global_id(seq, shard);
    let conn = db::open_shard(&store::shard_path(main, shard)).unwrap();
    conn.execute(
        "insert into releases (id, name, group_name, poster, posted_date, size, complete, parts, is_obfuscated)
         values (?1, ?2, ?3, 'p', '2026-10-02 10:11:12', 10, 1, 1, ?4)",
        rusqlite::params![id, name, group, obfuscated],
    ).unwrap();
    conn.execute(
        "insert into files (release_id, filename, subject, subject_part, subject_mid, expected, seen)
         values (?1, ?2, 'x', 1, '<m@x>', 1, x'02')",
        rusqlite::params![id, format!("{name}.bin")],
    ).unwrap();
    let file = conn.last_insert_rowid();
    let local = [&[0u8][..], format!("<{name}@x>").as_bytes()].concat();
    conn.execute(
        "insert into segments (file_id, local, domain, part, bytes) values (?, ?, 0, 1, 10)",
        rusqlite::params![file, local],
    ).unwrap();
    id
}
```

  Then:

```rust
let main = atlas::paths::database();
let hidden = old_row(&main, BLOCKED, 1, "Findable.Thing.Hidden", false);
let hidden_obf = old_row(&main, BLOCKED, 2, "a1B2c3D4e5F6g7H8i9J0", true);
let shown = old_row(&main, OPEN, 3, "Findable.Thing.Shown", false);
let shown_obf = old_row(&main, OPEN, 4, "z9Y8x7W6v5U4t3S2r1Q0", true);

let hits = search::search_all_releases("findable", 0, 10).unwrap();
assert_eq!(hits.iter().map(|r| r.id).collect::<Vec<_>>(), vec![shown]);
assert_eq!(search::count_all_releases("findable").unwrap(), 1);
assert_eq!(search::search_releases("findable", BLOCKED, 0, 10).unwrap(), vec![]);
assert_eq!(search::count_releases("findable", BLOCKED).unwrap(), 0);
// the LIKE fallback too: an fts syntax error takes it
assert!(search::search_all_releases("Hidden\"(", 0, 10).unwrap().is_empty());
assert_eq!(search::all_releases(0, 10).unwrap().iter().map(|r| r.id).collect::<Vec<_>>(), vec![shown]);
let obfuscated = search::search_obfuscated(0, 10).unwrap();
assert_eq!(obfuscated.iter().map(|r| r.id).collect::<Vec<_>>(), vec![shown_obf]);
assert_eq!(search::count_obfuscated().unwrap(), 1);
assert_eq!(search::recent_in_groups(&[BLOCKED.to_string()], 0, 10).unwrap(), vec![]);

assert_eq!(search::get_release(hidden).unwrap(), None);
assert_eq!(search::get_release(hidden_obf).unwrap(), None);
assert_eq!(search::get_articles(hidden).unwrap(), vec![]);
assert_eq!(nzb::build_nzb(hidden).unwrap(), None);
assert!(nzb::generate_nzb(hidden, Some(home.path())).is_err());
assert!(nzb::build_nzb(shown).unwrap().is_some());

let r = api::handle("/api", "t=search&q=findable&apikey=k", "http://x", "k");
assert!(r.body.contains("Findable.Thing.Shown") && !r.body.contains("Findable.Thing.Hidden"), "{}", r.body);
let r = api::handle("/api", "t=search&apikey=k", "http://x", "k");
assert!(!r.body.contains("Hidden"), "{}", r.body);
assert_eq!(api::handle("/api", &format!("t=get&id={hidden}&apikey=k"), "http://x", "k").status, 404);
assert_eq!(api::handle("/api", &format!("t=get&id={shown}&apikey=k"), "http://x", "k").status, 200);

// nothing is deleted before a purge
let conn = db::open().unwrap();
assert_eq!(conn.query_row("select count(*) from releases", [], |r| r.get::<_, i64>(0)).unwrap(), 4);
```

- [ ] **Run:** `cargo test --locked --test blocked_groups`. **Expected:** fails on the first `assert_eq!`, the hidden row is found.
- [ ] **Implement.**
  - `src/db.rs`, `set_up`, before `Ok(conn)`: `crate::filters::register_sql(&conn)?;`
  - `src/search.rs:36-39`: append to the constant, so it reads `(...) and not atlas_blocked(r.group_name)`.
  - `:231`: `"r.is_obfuscated = 1 and not atlas_blocked(r.group_name)"`.
  - `:237`: `"select count(*) as c from s{i}.releases where is_obfuscated = 1 and not atlas_blocked(group_name)"`.
  - `:249`: `Ok(conn.query_row(&sql, [id], release_row).optional()?.filter(|r| !crate::filters::group_blocked(&r.group_name)))`
  - `recent_in_groups`, `:253-259`:

```rust
let names: Vec<&str> = groups.iter().map(String::as_str).filter(|g| !crate::filters::group_blocked(g)).collect();
if names.is_empty() {
    return Ok(Vec::new());
}
let qs = (0..names.len()).map(|n| format!("?{}", n + 3)).collect::<Vec<_>>().join(",");
let extra = names.iter().map(|g| text(*g)).collect();
```

  - `get_articles`, `:265`:

```rust
let conn = db::open()?;
if get_release_with(&conn, release_id)?.is_none() {
    return Ok(Vec::new());
}
store::articles(&conn, release_id)
```

  - `src/stats_dashboard.rs:172`: `from s{i}.releases group by group_name having not atlas_blocked(group_name)`.
  - `:200`: `select * from (select max(size), coalesce(display_name, name), group_name from s{i}.releases where not atlas_blocked(group_name))`.
- [ ] **Run:** the same, then `just test`. **Expected:** pass, `tests/convert.rs` included: its `get_release_with` calls use open groups.
- [ ] **Commit:** `Stop serving rows from blocked groups, with no purge needed`

### Task 3: Never select, index or fetch from a blocked group

**Files:** `src/config.rs:254-262`, `src/groups_menu.rs`, `src/ai.rs`, `src/indexer.rs`, `src/store.rs:1376`, `src/bg_indexer.rs`, `tests/blocked_groups.rs`.

- [ ] **Test first.** Extend the test:

```rust
let config = Config {
    group: BLOCKED.into(),
    groups: vec![BLOCKED.into(), GROUP.into(), "alt.binaries.yong.eroitca".into()],
    ..Config::default()
};
assert_eq!(config.tracked_groups(), vec![GROUP.to_string()]);
assert_eq!(config.blocked_groups(), vec![BLOCKED.to_string(), "alt.binaries.yong.eroitca".to_string()]);
let only = Config { group: BLOCKED.into(), groups: vec![BLOCKED.into()], ..Config::default() };
assert_eq!(only.tracked_groups(), Vec::<String>::new());

// a pass over a blocked group: no header, no body, no cursor, no row
let par2 = par2_file_desc("Real.Name.mkv", 100);
let mut server = Server::new(vec![
    post(1, r#""Unsafe.Thing.par2" yEnc (1/1)"#, 10, yenc_body("Unsafe.Thing.par2", &par2)),
    post(2, r#""Other.Thing.bin" yEnc (1/1)"#, 10, vec![]),
]);
std::sync::Arc::get_mut(&mut server).unwrap().any_group = true;
let port = spawn_server(server.clone());
let client = BlockingPool::new(&[mock(port, "secret", 2, 1)]);
client.connect().unwrap();
let mut indexer = Indexer::new(client, "backfill", db::open().unwrap());
indexer.index_group_with(BLOCKED, &mut |_| panic!("no slice of a blocked group")).unwrap();
assert_eq!(server.xovers.load(std::sync::atomic::Ordering::SeqCst), 0, "no headers");
assert_eq!(server.bodies_sent.load(std::sync::atomic::Ordering::SeqCst), 0, "no bodies");
let cursors: i64 =
    conn.query_row("select count(*) from groups where name like ?", [format!("{BLOCKED}%")], |r| r.get(0)).unwrap();
assert_eq!(cursors, 0);

// the store takes nothing of it, whatever path it came by
let release = atlas::parser::Release { name: "Late".into(), group: BLOCKED.into(), ..Default::default() };
store::save(&main, &[release]).unwrap();
assert_eq!(conn.query_row("select count(*) from releases", [], |r| r.get::<_, i64>(0)).unwrap(), 4);
```

- [ ] **Run:** `cargo test --locked --test blocked_groups`. **Expected:** compile error, `blocked_groups` missing.
- [ ] **Implement.**
  - `Config::tracked_groups` filters both paths; add `blocked_groups`:

```rust
pub fn tracked_groups(&self) -> Vec<String> {
    let groups: Vec<String> =
        self.groups.iter().filter(|g| !g.is_empty() && !crate::filters::group_blocked(g)).cloned().collect();

    if groups.is_empty() && !self.group.is_empty() && !crate::filters::group_blocked(&self.group) {
        return vec![self.group.clone()];
    }

    groups
}

/// configured groups that are never indexed, see `filters::group_blocked`
pub fn blocked_groups(&self) -> Vec<String> {
    let mut blocked: Vec<String> =
        self.groups.iter().chain([&self.group]).filter(|g| crate::filters::group_blocked(g)).cloned().collect();
    blocked.sort_unstable();
    blocked.dedup();
    blocked
}
```

  - `src/groups_menu.rs`, after `:163`:

```rust
for g in config.blocked_groups() {
    ui::warn(&format!("{g} is blocked and wont be indexed: its name labels it for minors"));
}
```

    and after the `collect` at `:197`:

```rust
let found = groups.len();
let groups: Vec<String> = groups.into_iter().filter(|g| !crate::filters::group_blocked(g)).collect();
if groups.len() < found {
    ui::warn(&format!("{} blocked groups not shown\n", found - groups.len()));
}
```

  - `src/ai.rs:126`: `groups.into_iter().map(|(name, _)| name).filter(|g| !group_blocked(g)).take(limit).collect()`; `:147`: `for grp in groups.iter().filter(|g| !group_blocked(g)) {`. Import `crate::filters::group_blocked`.
  - `src/indexer.rs`, first statement of `run_pass` (`:499`) and `run_sweep` (`:901`): `if crate::filters::group_blocked(group) { return Ok(Progress::default()); }`.
  - `process_range_dated`, after `:1494`: `if crate::filters::group_blocked(group) { return Ok((Progress::default(), true, 0)); }`.
  - `save_slice`, after `:1603`: `if crate::filters::group_blocked(group) { return Ok(Progress { articles, ..Progress::default() }); }`.
  - `index_group_with`, after `:1738`: `if crate::filters::group_blocked(group) { return Ok(()); }`.
  - `src/store.rs:1376`: `for release in batches.into_iter().flatten().filter(|r| !crate::filters::group_blocked(&r.group)) {`.
  - `src/bg_indexer.rs`: add `blocked: Mutex<Vec<String>>` to `Scheduler` after `:266`, initialized at `:716` from `config.blocked_groups()` and with `Mutex::new(Vec::new())` in the test constructor at `:1062`. Add:

```rust
/// Say which configured groups wont be indexed (`filters::group_blocked`).
fn say_blocked(blocked: &[String]) {
    if !blocked.is_empty() {
        println!("blocked and not indexed, their names label them for minors: {}", blocked.join(", "));
    }
}
```

    Call it in `run_servers` after `:703`, and in `watch_config` before `:647`, only when the set changed:

```rust
let blocked = config.blocked_groups();
let mut known = sched.blocked.lock().unwrap();
if *known != blocked {
    say_blocked(&blocked);
    *known = blocked;
}
drop(known);
```

    After `:446`: `stats.extra.insert("groups_blocked".into(), json!(self.blocked.lock().unwrap().len()));`
  - `src/stats_dashboard.rs`, after `:586`: `kv("groups blocked", count(s["groups_blocked"].as_i64().unwrap_or(0))),`.
- [ ] **Run:** the same, then `just test`. **Expected:** pass.
- [ ] **Commit:** `Never select, index or fetch from a blocked group, and say so`

### Task 4: The per-article rule

**Files:** `src/filters.rs` (the per-article half of 4.5).

- [ ] **Test first**, in `src/filters.rs`. Tokens are made up per article:

```rust
fn random(i: usize, len: usize) -> String {
    let mut x = (i as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let abc = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    (0..len).map(|_| { x = x.wrapping_mul(0x100_0000_01b3) ^ (x >> 7); abc[(x % 62) as usize] as char }).collect()
}

fn skipped(subjects: &[String]) -> usize {
    mark_per_article_obfuscated(subjects.iter().map(String::as_str)).iter().filter(|s| **s).count()
}

#[test]
fn the_real_shapes_are_caught() {
    let all = |s: Vec<String>, why: &str| assert_eq!(skipped(&s), s.len(), "{why}");
    all((2..=40).map(|p| format!(r#"[41/62] "{}" yEnc ({p}/375)"#, random(p, 32))).collect(), "counter, quoted token");
    all((2..=40).map(|p| format!(r#""{}" yEnc ({p}/5747)"#, random(p, 24))).collect(), "no counter");
    all((1000..1040).map(|p| format!(r#"[17/36] - "{}.{}" yEnc ({p}/59) 2088881050"#, random(p, 8), random(p + 9000, 8))).collect(),
        "past the total, a dot and no 16 long run");
    let dotted: Vec<String> =
        (2..=40).map(|p| format!(r#"[17/36] - "{}.{}" yEnc ({p}/59) 2088881050"#, random(p, 8), random(p + 9000, 8))).collect();
    assert!(parse_subject(&dotted[0]).is_some(), "these do become releases today");
    all(dotted, "within the total");
    let bare: Vec<String> = (2..=40).map(|p| format!("[{}] - {} yEnc ({p}/1250)", random(p, 2), random(p, 40))).collect();
    assert!(parse_subject(&bare[0]).is_none());
    all(bare, "no quotes");
    all((2..=40).map(|p| {
        let sep = if p % 2 == 0 { "-" } else { "_" };
        format!(r#"[3/9] "{}{sep}{}" yEnc ({p}/200)"#, random(p, 9), random(p + 500, 9 + p % 3))
    }).collect(), "a separator that varies");
}

#[test]
fn real_uploads_are_kept() {
    let none = |s: Vec<String>, why: &str| assert_eq!(skipped(&s), 0, "{why}");
    none((1..=100).map(|p| format!(r#"[1/5] "a1B2c3D4e5F6g7H8i9J0.bin" yEnc ({p}/100)"#)).collect(), "one name on every part");
    none((1..=40).map(|p| format!(r#""a1B2c3D4e5F6g7H8i9J0.part{p:02}.rar" yEnc ({p}/50)"#)).collect(), "numbered volumes");
    none((0..40).map(|f| format!(r#""{}" yEnc ({}/50)"#, random(f, 32), 7 + f % 2)).collect(), "round robin, two positions");
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
```

- [ ] **Run:** `cargo test --locked --lib filters`. **Expected:** compile error.
- [ ] **Implement** per 4.5.
- [ ] **Run:** the same. **Expected:** pass.
- [ ] **Commit:** `Recognize articles obfuscated per article within a slice`

### Task 5: Skip them, and count them

**Files:** `src/indexer.rs`, `src/ai.rs`, `src/bg_indexer.rs`, `src/dashboard.rs`, `src/stats_dashboard.rs`; create `tests/per_article.rs`, `examples/bench_filters.rs`.

- [ ] **Test first.** `tests/per_article.rs`, a single test with the env set as in Task 2 and the `token(i, len)` helper of `random` above. It posts 40 articles of each of `[41/62] "<32>" yEnc ({p}/375)`, `"<24>" yEnc ({p}/5747)` and `[17/36] - "<8>.<8>" yEnc ({p}/59) 2088881050` for `p` in `2..42`, then 30 of `[1/1] - "Real.Upload.2026.mkv" yEnc ({p}/30)` for `p` in `1..=30`, all with `post(number, subject, 700_000, vec![])` and consecutive numbers. It runs `Indexer` in `backfill` mode with `index_group_with` until `is_idle(GROUP)`, summing a `Progress` from the callback, and asserts:

```rust
assert_eq!(seen.articles, last as i64, "every article counts as received");
assert_eq!(seen.skipped, 120, "all three shapes");
let names: Vec<String> = conn.prepare("select name from releases").unwrap()
    .query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
assert_eq!(names, vec!["Real.Upload.2026.mkv".to_string()], "only the real upload");
assert!(search::search_all_releases("real upload", 0, 10).unwrap()[0].complete);
// the cursors went past them
let (live, backfill): (i64, i64) = conn.query_row(
    "select live_cursor, backfill_cursor from groups where name like ?", [format!("{GROUP}%")],
    |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
assert_eq!(live, last as i64);
assert!(backfill < 1, "backfill reached the first article, cursor {backfill}");
```

  In `src/bg_indexer.rs` tests:

```rust
#[test]
fn skipped_articles_are_counted_per_group_and_in_all() {
    let mut stats = Stats::new();
    stats.tick(&Progress { articles: 10, bytes: 5, releases: 2, skipped: 4 }, "alt.binaries.a");
    stats.tick(&Progress { articles: 10, bytes: 5, releases: 2, skipped: 1 }, "alt.binaries.a");
    assert_eq!(stats.total_skipped, 5);
    assert_eq!(stats.group_stats["alt.binaries.a"].skipped, 5);
    assert_eq!(stats.total_articles, 20, "they still count as received");
}
```

  In `src/dashboard.rs` (`renders_without_stats`), after `assert!(text.contains("running"));` at `:237`: `assert!(text.contains("skipped"), "{text}");`.
- [ ] **Run:** `cargo test --locked --test per_article`. **Expected:** compile error, no `skipped` on `Progress`.
- [ ] **Implement.**
  - `Progress` (`src/indexer.rs:18-30`): add `pub skipped: i64` and `self.skipped += other.skipped;`.
  - `save_slice`, `:1609-1612`:

```rust
let parsing = move || {
    let (articles, skipped) = crate::filters::drop_per_article_obfuscated(headers_to_articles(headers));
    let releases: Vec<Release> = group_articles(articles).into_values().collect();
    (releases, skipped)
};
let (mut releases, skipped) =
    tokio::task::spawn_blocking(parsing).await.map_err(|e| anyhow!("parse task failed: {e}"))?;
```

    and `:1636`: `Ok(Progress { articles, bytes, releases: count, skipped })`.
  - `process_range_dated`, after `:1570`:

```rust
if saved.skipped > 0 {
    println!("[{kind}] {group} {start}-{end} skipped {} articles obfuscated per article", saved.skipped);
}
```

  - `src/ai.rs:172`: `let (articles, _skipped) = drop_per_article_obfuscated(headers_to_articles(headers));` and group `articles`.
  - `src/bg_indexer.rs`: `GroupStats` (`:74`) and `Stats` (`:80`) gain `skipped` and `total_skipped`, `Stats::new` sets 0. `tick` becomes `fn tick(&mut self, p: &Progress, group: &str)`, opening with `let Progress { articles, bytes, releases, skipped } = *p;`, adding to both totals. `:574,583,599`: `stats.lock().unwrap().tick(p, &group)`. `:178`: add `"skipped": g.skipped`. `:190`: add `"total_skipped": self.total_skipped`.
  - `src/dashboard.rs:85`, after it: `kv("skipped   ", s["total_skipped"].as_i64().unwrap_or(0).to_string(), Color::Cyan),`.
  - `src/stats_dashboard.rs:600`, after it: `kv("skipped, per article", count(s["total_skipped"].as_i64().unwrap_or(0))),`.
  - `examples/bench_filters.rs`: builds 20 slices of 10,000 `Article`s by the `random` helper, half `[41/62] "<32>" yEnc ({k % 370 + 2}/375)` at each of 0%, 30% and 100%, the rest `[{k%20+1:02}/20] - "Some.Show.S01E{set%90:02}.1080p-GRP{set}.part{k%20+1:02}.rar" yEnc ({k%20+1}/20)` with `set = k / 400`. It prints articles/s and µs/article for `drop_per_article_obfuscated` at each share, and ns per call of `group_blocked` over 1,000,000 calls on two names.
- [ ] **Run:** `cargo test --locked --test per_article && just test`. **Expected:** pass.
- [ ] **Run, for information:** `cargo run --release --locked --example bench_filters`. Put the numbers in the PR description. Per the overview, a benchmark never stops a part.
- [ ] **Commit:** `Skip articles obfuscated per article, and count them`

### Task 6: The stored-row predicate

**Files:** `src/filters.rs` (stored fragments half of 4.5).

- [ ] **Test first:**

```rust
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
    let release = Release { name: TOKEN.into(), group: group.into(), articles: vec![article], ..Default::default() };
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
        ).unwrap()
    };
    assert!(read(), "loose");
    let file: i64 = conn.query_row("select id from files", [], |r| r.get(0)).unwrap();
    crate::store::seal_file(&conn, file).unwrap();
    let rows: i64 = conn.query_row("select count(*) from segments", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 0, "sealed: its article is only in the blob");
    assert!(read(), "sealed");
}
```

- [ ] **Run:** `cargo test --locked --lib filters`. **Expected:** compile error.
- [ ] **Implement** per 4.5.
- [ ] **Run:** the same. **Expected:** pass.
- [ ] **Commit:** `Tell a stored per-article fragment from what is stored about it`

### Task 7: Docs and the full check

**Files:** `README.md`.

- [ ] **Edit** the sections in section 12. **Test:** `grep -c "group_blocked\|obfuscated per article" README.md` is at least 2.
- [ ] **Run:** `just format-check && just lint && just test`, and every acceptance check in section 11. **Expected:** pass.
- [ ] **Commit:** `Document the ingest filters`

## 9. Performance and load

Measured on the user's Mac, at `nice 15`, release profile without LTO, with and without `atlas --compact` running. They're informational: no number here is an acceptance check.

| Cost | Measured | At 30,000 articles/s |
|---|---|---|
| `drop_per_article_obfuscated`, 10,000-article slice | 0.9–3.6 µs/article, 9–36 ms a slice | 3–11% of one core, on the blocking pool (`spawn_blocking`) |
| Memory per slice | One map entry per distinct name, about 1.5 MB at 10,000 distinct names, freed with the slice | Bounded by `request_size` |
| `group_blocked`, first call for a name | About 8 µs | Once per name per thread |
| `group_blocked`, after that | 15–54 ns | |
| `atlas_blocked` per row in sql | The same, plus SQLite's call. A LIKE scan of a 15M-row shard pays under 1 s more, on a scan that already takes far longer | |

The filter's cost is linear in the slice: maps are written in place, unlike a copy per article. It reads no disk and no network.

**Stored data at 118M releases:** no scan, no write, no index on upgrade. **NNTP:** none added. Skipped articles never reach `name_sources`, so body lookups can only fall, and a blocked group costs no GROUP, XOVER or BODY at all. In the overview's sample, 29 of 100 releases were fragments, so inflow to the writers falls by about that share for uploads this rule catches.

**What the slice rule can't catch.** A signature needs 16 articles in one slice, at the default `request_size` of 10,000 (`src/config.rs:27`). A file with `total < 18` can't span 16 middle positions, so it's never caught, and a slice edge can leave up to 15 stragglers of a signature behind. Both are false negatives: they become rows as today, and Part 3 can reject them later.

## 10. Failure handling

- **Bad input.** A subject the regex doesn't match, or with numbers past `i64`, is ignored by the rule and handled by `parse_subject` as today. An empty slice returns an empty mark list. A NULL or non-text `group_name` is not blocked, and can't error out of a query.
- **Server error.** Neither filter talks to a server. A failed slice never reaches them.
- **Crash partway.** Both are stateless. Cursors move only after a range is saved, so the range is redone.
- **Re-run.** The same slice gives the same marks. A different server's slices can cut a post differently, so an edge article may be kept on one and skipped on another. A kept one stays stored; nothing is lost or duplicated, since a save dedupes by message-id.
- **Rule misfire.** Counters and the per-range log line show it. `PER_ARTICLE_MIN` is the only number to change.
- **Config with only blocked groups.** `tracked_groups` is empty, so the scheduler idles as with no groups: `take_group` returns `None` (`src/bg_indexer.rs:309`), it logs which were blocked, and nothing crashes.

## 11. Acceptance checks

All in the worktree, with no live data and no network:

```bash
cargo test --locked --lib filters             # block list, shapes, guards, predicate, sealed
cargo test --locked --test blocked_groups     # every serving and indexing layer, no bytes moved
cargo test --locked --test per_article        # skipped, counted, cursors at the end, real upload kept
just format-check && just lint && just test   # all green, Cargo.lock unchanged by the feature
```

`git diff de598b6 --stat -- Cargo.lock` must be empty.

## 12. Docs

- **`README.md`:**
  - "Select groups": blocked groups are not listed, with a warning for configured ones.
  - "Index": articles obfuscated per article are skipped and counted, and rows of blocked groups stop being served without a purge.
  - "Live dashboard" and "Stats dashboard > Overview": the `skipped` and `groups blocked` lines.
  - "Limitations": fragments under 18 parts or under the minimum in a slice aren't recognized.
  - "Source layout": `src/filters.rs`, `examples/bench_filters.rs`.
  - "Credits": the rule is ported from newznab-tmux, GPLv3.
- **Dashboard pages:** Live dashboard totals, Stats Overview.

## 13. Decisions

1. **`PER_ARTICLE_MIN` is 16, a constant.** Upstream's default, proven on the 81M-collection case in `BUGS.md:484`. No floor or disable value: there's no setting to protect.
2. **The rule reads raw subjects before `parse_subject`,** so past-total articles count toward their signature while within-total ones of the same upload are dropped with them.
3. **Letters and digits are counted across a whole name, and separators collapse into the shape.** That's the only way to catch `<random>.<random>` and varying separators. It admits long real names to the map, which costs a map entry; the 90% share and the 16-position span, unchanged, keep them from being skipped. A real file repeats its name across parts, and numbered volumes reduce to one name.
4. **No memory across slices.** A slice is the unit that can run on any server in any order (`src/nntp.rs:1934`). Shared state would make the answer depend on arrival order, for at most 15 stragglers per slice edge, which become rows Part 3 can reject.
5. **First and last parts stay, as upstream.** Skipping them could cost a real upload cut by a slice edge a part it can't get back. Atlas's 10,000-number slice edges are more frequent than upstream's range edges, which makes this guard more valuable, not less.
6. **No config key for the per-article filter.** Overview rule 8 has it on by default. A switch would be a way to re-create 80M useless rows, and misfires are visible in the counters.
7. **Block by words, with edit distance on `young` (1), `erotica` (2) and `pictures` (1).** A fixed list can't cover misspellings nobody has seen, and real group lists are off limits by hard rule 2. `teen` and `youth` get no fuzzy match, because `seen`, `keen`, `south` and `mouth` are all one edit away and only pairing would stand between them and a false block.
8. **Substrings for the age roots, and what a false block costs.** `teen` matches inside `seventeen` or `canteen`, and that blocks `alt.binaries.pictures.seventeen`, which is an age word in a picture hierarchy anyway. A false block costs one group not indexed and not served. Its rows stay on disk until the user passes `--blocked` to Part 3's purge, an explicit flag whose dry run lists each group with its counts, so a wrong block is visible before anything is deleted. A miss costs what the block exists to prevent. There's no override, as asked.
9. **`boy` pairs, bare `girl` and `girls` don't.** `boys` in a picture hierarchy is a label with this history on Usenet. `girls` is an everyday label for adult content across unrelated groups, so pairing it would block adult groups this isn't about. Words built on it (`littlegirl`, `girlie`) pair, with an adult label only. That over-blocks a word like `hotgirls` in an adult group, which costs what decision 8 says. `*boy` excludes a short list of words that aren't about one (`playboy`, `cowboy`, `gameboy`), and `kid` is whole-word or a `kids`, `kidz`, `kiddie` start, because `kidney`, `kidman` and `skid` are not.
10. **A bare `teen`, `young` or `youth` label blocks.** `alt.binaries.teens` says nothing else about itself, unlike `alt.binaries.tv.teen-drama`. A benign hierarchy word is what keeps a group out of this rule.
11. **Numbers count as ages only next to a label.** A bare 5–17 is often not an age, so `movies.16` and `multimedia.12` stay. Next to an adult or picture label it is, the cost being an odd `pictures.series.12`. A limit (`u18`, `under18`) counts up to 21, since what's under it includes minors. `14yo` blocks alone. `18yo` and `21yo` don't, because they say adult. Age detection reads the word as written, before digits become letters.
12. **Video labels pair only for age words and only with no title beside them.** The review's fix adds them to the paired set outright, which would block `movies.teen-wolf`, `video.teen-titans` and `neil-young.videos`, and the original brief says a teen show mustn't be caught. So `teen` needs no title after it, and `young` and `youth`, which are also surnames and bands, need none anywhere. `crazy.teen.movies` blocks, `teen.crazy.movies` doesn't, which is the accepted miss. Child words never pair with video, because `movies.kids` is an ordinary group. `tv` isn't a video label, for the same reason.
13. **Pieces read together must match exactly, not as a substring.** The review proposed a last pass of `MINOR_ROOTS` over the name with separators removed. That blocks `under.agent` (`underage`) and `lol.italy` (`lolita`). Exact matching on runs of short pieces catches every split the review lists and neither of those.
14. **No root for `nin`.** The review suggests `niñ`/`nin` as a root. As a substring it hits `nintendo`, `running` and `morning`, so it would block any of those next to a label. The words `nino`, `ninos` and `ninas` are whole-word instead. `nina` isn't, because it's a common name.
15. **A SQL function, not a list of names in each query.** A list needs a scan or a loose index walk per query plus a cache to invalidate; a function is correct for every row with no state beyond the memo, and needs one new rusqlite feature with no new dependency. A Rust filter after the query would break paging.
16. **Rows stay, compaction and conversion carry them, and no path deletes them.** Hard rule 6: deletion is the user's choice through Part 3's purge.
17. **`store::articles` isn't filtered, `get_articles` is.** Compaction's check must see every row, or it would report a difference and refuse to swap.
18. **`config.json` is never rewritten for this.** The user configured the group, the log and menu say it's blocked, and "Remove group" stays able to remove it.
19. **`Progress.articles` includes skipped articles,** matching upstream's `received` (`HeaderParser.php:331`), and matching what `articles` already means: headers received. `bytes` and `releases` exclude them.
20. **`StoredPost` has a `subject` field that Part 3's copy of the struct lacks,** and `looks_random` is wider than its "run of 16+". Without the subject, the rule can't keep its promise that `(1/1)` never matches: see 4.6. Part 3's five fields are kept, same names and types, and `judge` adds `subject` to its query. The wider name test only adds `<random>.<random>`, which is one of the shapes this part must catch; it can't make a first, last or single part match.
21. **`stored_fragment` has no clock.** Part 3's `judge` rejects on it without a clock (its section 6, step 6), because rejecting deletes nothing, and only Part 3's purge applies `dead_after_secs` before it deletes (its decision 35), so the wait lives in one place.
