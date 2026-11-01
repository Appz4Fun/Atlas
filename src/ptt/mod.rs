//! A port of PTT (parse-torrent-title), the parser the Kodi add-on uses, soo Atlas and the add-on read a
//! release name the same way. `rules.rs` is generated from PTT's handlers by `scripts/gen_ptt.py`, in their
//! order, which matters: each rule sees the title as the rules before it left it. The handlers that are
//! code in PTT are `custom` here. PTT is MIT licensed.

mod rules;

use std::collections::HashMap;
use std::sync::LazyLock;

use fancy_regex::Regex;

pub use rules::RULES;

/// What a rule does with what it matched.
#[derive(Clone, Copy, Debug)]
pub enum T {
    None,
    Boolean,
    Integer,
    FirstInteger,
    Lowercase,
    Uppercase,
    Range,
    RangeXOfY,
    Resolution,
    ArrayInteger,
    /// `$1` is the match
    Value(&'static str),
    Date(&'static [&'static str]),
    UniqValue(&'static str),
}

pub struct Rule {
    pub name: &'static str,
    pub pattern: &'static str,
    pub transform: T,
    pub remove: bool,
    pub skip_if_found: bool,
    pub skip_from_title: bool,
    pub skip_if_first: bool,
}

pub enum Entry {
    Rule(Rule),
    Custom(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Str(String),
    Int(i64),
    Bool(bool),
    Strs(Vec<String>),
    Ints(Vec<i64>),
}

/// Everything PTT found, by its names for things, and the title.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub title: String,
    pub fields: HashMap<&'static str, Value>,
}

impl Parsed {
    pub fn text(&self, name: &str) -> Option<&str> {
        match self.fields.get(name) {
            Some(Value::Str(s)) => Some(s),
            _ => None,
        }
    }

    pub fn int(&self, name: &str) -> Option<i64> {
        match self.fields.get(name) {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        }
    }

    pub fn flag(&self, name: &str) -> bool {
        matches!(self.fields.get(name), Some(Value::Bool(true)))
    }

    pub fn list(&self, name: &str) -> &[String] {
        match self.fields.get(name) {
            Some(Value::Strs(list)) => list,
            _ => &[],
        }
    }

    pub fn ints(&self, name: &str) -> &[i64] {
        match self.fields.get(name) {
            Some(Value::Ints(list)) => list,
            _ => &[],
        }
    }
}

fn re(pattern: &str) -> Regex {
    #[allow(clippy::unwrap_used)]
    Regex::new(pattern).unwrap()
}

enum Compiled {
    Rule(&'static Rule, Regex),
    Custom(&'static str),
}

static COMPILED: LazyLock<Vec<Compiled>> = LazyLock::new(|| {
    RULES
        .iter()
        .map(|entry| match entry {
            Entry::Rule(rule) => Compiled::Rule(rule, re(rule.pattern)),
            Entry::Custom(name) => Compiled::Custom(name),
        })
        .collect()
});

static ADULT_WORDS: LazyLock<Vec<String>> = LazyLock::new(|| {
    include_str!("combined-keywords.dat").lines().map(|l| l.trim().to_lowercase()).filter(|l| !l.is_empty()).collect()
});

fn digits(s: &str) -> Vec<i64> {
    let mut out = Vec::new();
    let mut run = String::new();
    for c in s.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() {
            run.push(c);
        } else if !run.is_empty() {
            if let Ok(n) = run.parse() {
                out.push(n);
            }
            run.clear();
        }
    }
    out
}

fn range(input: &str) -> Option<Vec<i64>> {
    let numbers = digits(input);
    match numbers.len() {
        1 => Some(numbers),
        2 if numbers[0] < numbers[1] => Some((numbers[0]..=numbers[1]).collect()),
        n if n > 2 && numbers.windows(2).all(|w| w[0] + 1 == w[1]) => Some(numbers),
        _ => None,
    }
}

fn resolution(input: &str) -> String {
    let lower = input.to_lowercase();
    for (found, name) in [
        ("2160", "2160p"),
        ("4k", "2160p"),
        ("1440", "1440p"),
        ("2k", "1440p"),
        ("1080", "1080p"),
        ("720", "720p"),
        ("480", "480p"),
        ("360", "360p"),
        ("240", "240p"),
    ] {
        if lower.contains(found) {
            return name.into();
        }
    }
    lower
}

static NON_WORD: LazyLock<Regex> = LazyLock::new(|| re(r"\W+"));
static ORDINAL: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(\d+)(st|nd|rd|th)"));
static MONTHS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        ("Janu", "Jan"),
        ("Febr", "Feb"),
        ("Marc", "Mar"),
        ("Apri", "Apr"),
        ("May", "May"),
        ("June", "Jun"),
        ("July", "Jul"),
        ("Augu", "Aug"),
        ("Sept", "Sep"),
        ("Octo", "Oct"),
        ("Nove", "Nov"),
        ("Dece", "Dec"),
    ]
    .into_iter()
    .map(|(long, short)| (re(&format!(r"(?i)\b{long}\b")), short))
    .collect()
});

fn date(input: &str, formats: &[&str]) -> Option<String> {
    let mut clean = NON_WORD.replace_all(input, " ").trim().to_string();
    for (month, short) in MONTHS.iter() {
        clean = month.replace_all(&clean, *short).into_owned();
    }
    for format in formats {
        let text = if format.contains("Do") { ORDINAL.replace_all(&clean, "$1").into_owned() } else { clean.clone() };
        let spec = format
            .replace("YYYY", "%Y")
            .replace("YY", "%y")
            .replace("MMMM", "%B")
            .replace("MMM", "%b")
            .replace("MM", "%m")
            .replace("Do", "%d")
            .replace("DD", "%d");
        if let Ok(parsed) = chrono::NaiveDate::parse_from_str(&text, &spec) {
            return Some(parsed.format("%Y-%m-%d").to_string());
        }
    }
    None
}

/// What the rule makes of `input`. None: this match doesn't count.
fn transform(t: T, input: &str, existing: Option<&Value>) -> Option<Value> {
    Some(match t {
        T::None => Value::Str(input.trim().to_string()),
        T::Boolean => Value::Bool(true),
        T::Integer => Value::Int(input.chars().filter(char::is_ascii_digit).collect::<String>().parse().ok()?),
        T::FirstInteger => Value::Int(*digits(input).first()?),
        T::Lowercase => Value::Str(input.to_lowercase().trim().to_string()),
        T::Uppercase => Value::Str(input.to_uppercase().trim().to_string()),
        T::Range => Value::Ints(range(input)?),
        T::RangeXOfY => {
            let numbers = digits(input);
            if numbers.len() != 1 {
                return None;
            }
            Value::Ints((1..=numbers[0]).collect())
        }
        T::Resolution => Value::Str(resolution(input).trim().to_string()),
        T::ArrayInteger => {
            Value::Ints(vec![input.chars().filter(char::is_ascii_digit).collect::<String>().parse().ok()?])
        }
        T::Value(value) => Value::Str(value.replace("$1", input).trim().to_string()),
        T::Date(formats) => Value::Str(date(input, formats)?),
        T::UniqValue(value) => {
            let mut list = match existing {
                Some(Value::Strs(list)) => list.clone(),
                _ => Vec::new(),
            };
            let item = value.replace("$1", input);
            if !list.contains(&item) {
                list.push(item);
            }
            Value::Strs(list)
        }
    })
}

#[derive(Clone)]
struct Matched {
    raw: Option<String>,
    index: usize,
}

struct Step {
    raw: String,
    index: usize,
    remove: bool,
    skip_from_title: bool,
}

struct Context<'a> {
    title: &'a str,
    fields: &'a mut HashMap<&'static str, Value>,
    matched: &'a mut HashMap<&'static str, Matched>,
}

static BEFORE_TITLE: LazyLock<Regex> = LazyLock::new(|| re(r"^\[([^\[\]]+)]"));

fn run_rule(rule: &'static Rule, pattern: &Regex, c: &mut Context<'_>) -> Option<Step> {
    if rule.skip_if_found && c.fields.contains_key(rule.name) {
        return None;
    }
    let hit = pattern.captures(c.title).ok()??;
    let whole = hit.get(0)?;
    let raw = whole.as_str();
    let clean = hit.get(1).map(|m| m.as_str()).filter(|s| !s.is_empty()).unwrap_or(raw);
    let value = transform(rule.transform, clean, c.fields.get(rule.name));
    let before_title = BEFORE_TITLE
        .captures(c.title)
        .ok()
        .flatten()
        .and_then(|b| b.get(1).map(|m| m.as_str().contains(raw)))
        .unwrap_or(false);
    let others: Vec<usize> = c.matched.iter().filter(|(k, _)| **k != rule.name).map(|(_, m)| m.index).collect();
    let skip_first = rule.skip_if_first && !others.is_empty() && others.iter().all(|i| whole.start() < *i);
    let value = value?;
    if skip_first {
        return None;
    }
    c.matched.entry(rule.name).or_insert(Matched { raw: Some(raw.to_string()), index: whole.start() });
    c.fields.insert(rule.name, value);
    Some(Step {
        raw: raw.to_string(),
        index: whole.start(),
        remove: rule.remove,
        skip_from_title: before_title || rule.skip_from_title,
    })
}

static VOLUME: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\bvol(?:ume)?[. -]*(\d{1,2})"));
static EP_BEGINNING: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)(?<!movie)(?<!film)(?:[ .]+-[ .]+|[(\[][ .]*)(\d{1,4})(?:a|b|v\d|\.\d)?(?:\W|$)(?!movie|film|\d+)(?<!\[480\])(?<!\[720\])(?<!\[1080)",
    )
});
static EP_MIDDLE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^(?:[(\[-][ .]?)?(\d{1,4})(?:a|b|v\d)?(?:\W|$)(?!movie|film)(?!\[(480|720|1080)\])"));
static ANIME: LazyLock<Regex> = LazyLock::new(|| re(r"One.*?Piece|Bleach|Naruto"));
static BARE_NUMBER: LazyLock<Regex> = LazyLock::new(|| re(r"\b\d{1,4}\b"));
static PT_EPISODE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)capitulo|ao"));
static DUBLADO: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)dublado"));
static CODEC_GAPS: LazyLock<Regex> = LazyLock::new(|| re(r"[ .-]"));

fn run_custom(name: &str, c: &mut Context<'_>) -> Option<Step> {
    let at = |index: usize| Step { raw: String::new(), index, remove: false, skip_from_title: false };
    match name {
        "is_adult_content" => {
            if !c.fields.contains_key("adult") {
                let lower = c.title.to_lowercase();
                if ADULT_WORDS.iter().any(|word| lower.contains(word.as_str())) {
                    c.fields.insert("adult", Value::Bool(true));
                }
            }
            None
        }
        "handle_bit_depth" => {
            if let Some(Value::Str(depth)) = c.fields.get_mut("bit_depth") {
                *depth = depth.replace([' ', '-'], "");
            }
            None
        }
        "handle_space_in_codec" => {
            if let Some(Value::Str(codec)) = c.fields.get_mut("codec") {
                *codec = CODEC_GAPS.replace_all(codec, "").into_owned();
            }
            None
        }
        "handle_volumes" => {
            let start = c.matched.get("year").map_or(0, |m| m.index);
            let hit = VOLUME.captures(c.title.get(start..)?).ok()??;
            let whole = hit.get(0)?;
            c.matched.insert("volumes", Matched { raw: None, index: whole.start() });
            c.fields.insert("volumes", Value::Ints(vec![hit.get(1)?.as_str().parse().ok()?]));
            Some(Step {
                raw: whole.as_str().to_string(),
                index: whole.start() + start,
                remove: true,
                skip_from_title: false,
            })
        }
        "handle_episodes" => {
            if c.fields.contains_key("episodes") {
                return None;
            }
            let index_of = |name: &str| c.matched.get(name).map(|m| m.index).filter(|i| *i != 0);
            let start = ["year", "seasons"].iter().filter_map(|n| index_of(n)).min().unwrap_or(0);
            let end = ["resolution", "quality", "codec", "audio"]
                .iter()
                .filter_map(|n| index_of(n))
                .chain(std::iter::once(c.title.len()))
                .min()
                .unwrap_or(c.title.len());
            let beginning = c.title.get(..end)?;
            let middle = c.title.get(start..end).unwrap_or("");
            let hit = match EP_BEGINNING.captures(beginning).ok().flatten() {
                Some(hit) => hit,
                None => EP_MIDDLE.captures(middle).ok()??,
            };
            let numbers = digits(hit.get(1)?.as_str());
            let index = c.title.find(hit.get(0)?.as_str())?;
            c.fields.insert("episodes", Value::Ints(numbers));
            Some(at(index))
        }
        "handle_anime_eps" => {
            if !ANIME.is_match(c.title).ok()?
                || matches!(c.fields.get("episodes"), Some(Value::Ints(e)) if !e.is_empty())
            {
                return None;
            }
            let hit = BARE_NUMBER.find(c.title).ok()??;
            c.fields.insert("episodes", Value::Ints(vec![hit.as_str().parse().ok()?]));
            Some(at(c.title.find(hit.as_str())?))
        }
        "infer_language_based_on_naming" => {
            let has =
                matches!(c.fields.get("languages"), Some(Value::Strs(l)) if l.iter().any(|x| x == "pt" || x == "es"));
            if !has {
                let from_episode = c
                    .matched
                    .get("episodes")
                    .and_then(|m| m.raw.as_deref())
                    .is_some_and(|raw| PT_EPISODE.is_match(raw).unwrap_or(false));
                if from_episode || DUBLADO.is_match(c.title).unwrap_or(false) {
                    let mut list = match c.fields.get("languages") {
                        Some(Value::Strs(l)) => l.clone(),
                        _ => Vec::new(),
                    };
                    list.push("pt".into());
                    c.fields.insert("languages", Value::Strs(list));
                }
            }
            None
        }
        "handle_group" => {
            let group = c.matched.get("group")?.clone();
            let raw = group.raw.as_deref().unwrap_or("");
            if raw.starts_with('[') && raw.ends_with(']') && c.fields.contains_key("group") {
                let end = group.index + raw.len();
                if c.matched.iter().any(|(k, m)| *k != "group" && m.index < end) {
                    c.fields.remove("group");
                }
            }
            None
        }
        "handle_group_exclusion" => {
            if matches!(c.fields.get("group"), Some(Value::Str(g)) if g == "-" || g.is_empty()) {
                c.fields.remove("group");
            }
            None
        }
        other => unreachable!("no code for PTT's {other}"),
    }
}

const NON_ENGLISH: &str = "\u{3040}-\u{30ff}\u{3400}-\u{4dbf}\u{4e00}-\u{9fff}\u{f900}-\u{faff}\u{ff66}-\u{ff9f}\u{0400}-\u{04ff}\u{0600}-\u{06ff}\u{0750}-\u{077f}\u{0c80}-\u{0cff}\u{0d00}-\u{0d7f}\u{0e00}-\u{0e7f}";

struct Cleaners {
    movie: Regex,
    edges: Regex,
    cast_1: Regex,
    cast_2: Regex,
    star_1: Regex,
    star_2: Regex,
    alt_titles: Regex,
    inline_1: Regex,
    inline_2: Regex,
    english_prefix: Regex,
    edges_again: Regex,
    empty_brackets: Regex,
    mp3: Regex,
    no_content: Regex,
    special_spacing: Regex,
    end_symbols: Regex,
    spacing: Regex,
}

static CLEAN: LazyLock<Cleaners> = LazyLock::new(|| {
    let n = NON_ENGLISH;
    Cleaners {
        movie: re(r"(?i)[\[(]movie[)\]]"),
        edges: re(&format!(r"^[^\w{n}#\[【★]+|[ \-:/\\\[|{{(#$&^]+$")),
        cast_1: re(r"\([^)]*[\u{0400}-\u{04ff}][^)]*\)$"),
        cast_2: re(r"\(.*\)$"),
        star_1: re(r"^[\[【★].*[\]】★][ .]?(.+)"),
        star_2: re(r"(.+)[ .]?[\[【★].*[\]】★]$"),
        alt_titles: re(&format!(r"[^/|(]*[{n}][^/|]*[/|]|[/|][^/|(]*[{n}][^/|]*")),
        inline_1: re(&format!(r"[a-zA-Z][^{n}]+[{n}].*[{n}]")),
        inline_2: re(&format!(r"[{n}].*[{n}](?=[^{n}]+[a-zA-Z])")),
        english_prefix: re(&format!(r"[a-zA-Z][^{n}]+")),
        edges_again: re(&format!(r"^[^\w{n}#]+|]$")),
        empty_brackets: re(r"\(\s*\)|\[\s*\]|\{\s*\}"),
        mp3: re(r"\bmp3$"),
        no_content: re(r"\(\W*\)|\[\W*\]|\{\W*\}"),
        special_spacing: re(r"[\-\+\_\{\}\[\]]\W{2,}"),
        end_symbols: re(r"[ \-:./\\]+$"),
        spacing: re(r"\s+"),
    }
});

fn clean_title(raw: &str) -> String {
    let c = &*CLEAN;
    let sub = |re: &Regex, s: &str, with: &str| re.replace_all(s, with).into_owned();
    let mut t = raw.replace('_', " ");
    t = sub(&c.movie, &t, "");
    t = sub(&c.edges, &t, "");
    t = sub(&c.cast_1, &t, "");
    if t.contains('/') {
        t = sub(&c.cast_2, &t, "");
    }
    t = sub(&c.star_1, &t, "$1");
    t = sub(&c.star_2, &t, "$1");
    t = sub(&c.alt_titles, &t, "");
    t = c
        .inline_1
        .replace_all(&t, |hit: &fancy_regex::Captures<'_>| {
            let whole = hit.get(0).map_or("", |m| m.as_str());
            c.english_prefix
                .find(whole)
                .ok()
                .flatten()
                .filter(|m| m.start() == 0)
                .map_or(String::new(), |m| m.as_str().to_string())
        })
        .into_owned();
    t = sub(&c.inline_2, &t, "");
    t = sub(&c.edges_again, &t, "");
    t = sub(&c.empty_brackets, &t, "");
    t = sub(&c.mp3, &t, "");
    t = sub(&c.no_content, &t, "");
    t = sub(&c.special_spacing, &t, "");
    for (open, close) in [('{', '}'), ('[', ']'), ('(', ')')] {
        if t.matches(open).count() != t.matches(close).count() {
            t = t.replace([open, close], "");
        }
    }
    if !t.contains(' ') && t.contains('.') {
        t = t.replace('.', " ");
    }
    t = sub(&c.end_symbols, &t, "");
    t = sub(&c.spacing, &t, " ");
    t.trim().to_string()
}

pub fn parse(name: &str) -> Parsed {
    let mut title: String = {
        // runs of underscores read as one space
        let mut out = String::with_capacity(name.len());
        let mut in_run = false;
        for ch in name.chars() {
            if ch == '_' {
                if !in_run {
                    out.push(' ');
                }
                in_run = true;
            } else {
                out.push(ch);
                in_run = false;
            }
        }
        out
    };
    let mut fields = HashMap::new();
    let mut matched = HashMap::new();
    let mut end_of_title = title.len();
    for entry in COMPILED.iter() {
        let step = {
            let mut context = Context { title: &title, fields: &mut fields, matched: &mut matched };
            match entry {
                Compiled::Rule(rule, pattern) => run_rule(rule, pattern, &mut context),
                Compiled::Custom(name) => run_custom(name, &mut context),
            }
        };
        let Some(step) = step else { continue };
        if step.remove
            && let Some(end) = step.index.checked_add(step.raw.len())
            && title.is_char_boundary(step.index)
            && title.is_char_boundary(end.min(title.len()))
        {
            title.replace_range(step.index..end.min(title.len()), "");
        }
        if !step.skip_from_title && step.index > 1 && step.index < end_of_title {
            end_of_title = step.index;
        }
        if step.remove && step.skip_from_title && step.index < end_of_title {
            end_of_title = end_of_title.saturating_sub(step.raw.len());
        }
    }
    fields.remove("title");
    let mut cut = end_of_title.min(title.len());
    while !title.is_char_boundary(cut) {
        cut -= 1;
    }
    Parsed { title: clean_title(&title[..cut]), fields }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rule_compiles() {
        assert!(COMPILED.len() > 400);
    }

    #[test]
    fn a_name_reads_as_ptt_reads_it() {
        let p = parse("Dune.Part.Two.2024.2160p.AMZN.WEB-DL.DDP5.1.Atmos.DV.HDR10Plus.H.265-FLUX");
        assert_eq!(p.title, "Dune Part Two");
        assert_eq!(p.int("year"), Some(2024));
        assert_eq!(p.text("resolution"), Some("2160p"));
        assert_eq!(p.text("network"), Some("Amazon"));
        assert_eq!(p.text("group"), Some("FLUX"));
        assert_eq!(p.list("hdr"), ["DV", "HDR10+"]);
    }
}
