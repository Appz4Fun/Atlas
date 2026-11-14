//! Posting models and pure identity, completeness, poster and password decisions.
//! Native readers supply facts through the shared post-facts module.

use std::sync::LazyLock;

use regex::Regex;

use crate::naming::NameSource;

#[path = "release_info/post_facts.rs"]
mod post_facts;
pub(crate) use post_facts::{Named, StemCandidate, UnionFiles, exact_named, try_stem_subset};
#[cfg(test)]
use post_facts::{UnionFile, subject_has_size, subject_size};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Names {
    pub posted: String,
    pub display: Option<String>,
    pub obfuscated: bool,
    pub source: Option<i64>,
}

/// Where a display name came from, for a person.
pub fn name_source_label(source: i64) -> Option<&'static str> {
    [
        (NameSource::PostedFile, "a posted file name"),
        (NameSource::NamingRegex, "the subject"),
        (NameSource::Nfo, "the NFO"),
        (NameSource::ArchiveInner, "a file inside the archive"),
        (NameSource::Par2, "the PAR2 file"),
        (NameSource::PreDb, "PreDB"),
    ]
    .into_iter()
    .find_map(|(known, label)| (known as i64 == source).then_some(label))
}

/// One file of a release's NZB.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NzbFile {
    pub name: String,
    /// as posted: the articles' bytes, yEnc and all
    pub bytes: i64,
    pub seen: i64,
    pub expected: i64,
}

/// The most files listed of one release. Some posts are thousands of little files.
pub const MAX_LISTED_FILES: usize = 1000;

/// Totals over a release's files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Contents {
    /// the files themselves, by name, `MAX_LISTED_FILES` at most
    pub list: Vec<NzbFile>,
    pub files: i64,
    pub par2_files: i64,
    pub recovery_volumes: i64,
    /// bytes of the files that aren't PAR2
    pub data_bytes: i64,
    /// bytes of the recovery volumes
    pub recovery_bytes: i64,
    pub articles_seen: i64,
    pub articles_expected: i64,
    /// the same, for the files that aren't PAR2
    pub data_articles_seen: i64,
    pub data_articles_expected: i64,
    /// and for the recovery volumes
    pub recovery_articles_seen: i64,
    pub recovery_articles_expected: i64,
    /// from the `+MM` in recovery volume names
    pub recovery_blocks: i64,
    /// from the PAR2 set, once read
    pub par2_block: Option<i64>,
    pub par2_blocks: Option<i64>,
}

impl Contents {
    /// Share of the articles that are missing, in tenths of a percent. None: no count to go by.
    pub fn missing_per_mille(&self) -> Option<i64> {
        (self.articles_expected > 0)
            .then(|| (self.articles_expected - self.articles_seen).max(0) * 1000 / self.articles_expected)
    }

    /// Whether the recovery volumes that are there can make up for the data that isn't. Counts articles, soo
    /// it's a good guess rather than a promise: a damaged article can cost a whole block.
    pub fn repairable(&self) -> bool {
        if self.data_articles_expected == 0 || self.recovery_articles_expected == 0 {
            return false;
        }
        let missing = i128::from((self.data_articles_expected - self.data_articles_seen).max(0));
        // whole numbers: rounding each to tenths of a percent would call 2 missing of 10,000 repairable by 1 block
        if let Some(blocks) = self.par2_blocks.filter(|_| self.exact_repair_per_mille().is_some()) {
            // the blocks are counted from file names, whether or not their articles are there
            return i128::from(self.recovery_blocks)
                * i128::from(self.recovery_articles_seen)
                * i128::from(self.data_articles_expected)
                >= missing * i128::from(blocks) * i128::from(self.recovery_articles_expected);
        }
        // sizes are of the articles that are there. What's missing is taken to be as big as what isn't
        missing == 0
            || (self.data_articles_seen > 0
                && self.data_bytes > 0
                && i128::from(self.recovery_bytes) * i128::from(self.data_articles_seen)
                    >= missing * i128::from(self.data_bytes))
    }

    /// What the recovery blocks cover, in tenths of a percent, when the PAR2 set's been read.
    pub fn exact_repair_per_mille(&self) -> Option<i64> {
        let blocks = self.par2_blocks?;
        (self.par2_block? > 0 && blocks > 0 && self.recovery_blocks > 0).then(|| self.recovery_blocks * 1000 / blocks)
    }

    /// What the recovery volumes could rebuild, as a share of the data in tenths of a percent.
    /// From sizes on the wire, soo it's an estimate: real blocks are a little smaller than the volumes.
    pub fn repair_per_mille(&self) -> Option<i64> {
        if self.data_bytes <= 0 {
            return None;
        }
        // the data's whole size, from what's there of it
        let whole = if self.data_articles_seen > 0 && self.data_articles_expected > self.data_articles_seen {
            i128::from(self.data_bytes) * i128::from(self.data_articles_expected) / i128::from(self.data_articles_seen)
        } else {
            i128::from(self.data_bytes)
        };
        i64::try_from(i128::from(self.recovery_bytes) * 1000 / whole).ok()
    }
}

fn is_par2(filename: &str) -> bool {
    filename.to_ascii_lowercase().ends_with(".par2")
}

/// The `MM` of `name.vol000+MM.par2`, or the span of `name.volNNN-MMM.par2`.
pub(crate) fn recovery_blocks(filename: &str) -> i64 {
    static BLOCKS: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"(?i)\.vol(\d+)([+-])(\d+)\.par2$").unwrap()
    });
    let Some(hit) = BLOCKS.captures(filename) else { return 0 };
    let (Ok(start), Ok(second)) = (hit[1].parse::<i64>(), hit[3].parse::<i64>()) else { return 0 };
    if &hit[2] == "+" {
        second
    } else if second >= start {
        (second - start).max(1)
    } else {
        0
    }
}

static POST_MEDIA_EXT: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)]
    Regex::new(
        r"(?i)\.(mkv|mp4|m4v|avi|wmv|mov|ts|m2ts|mpg|mpeg|webm|iso|img|bin|7z|zip|exe|mp3|flac|m4a|ogg|wav|wma|ape|aac|opus|epub|mobi|azw3|pdf|cbr|cbz|jpg|jpeg|png|gif|bmp|webp|tiff?|sub|idx|srt|rev|diz)$",
    )
    .unwrap()
});

/// The release stem shared across a split upload's volume, parity and payload files.
pub fn post_stem(name: &str) -> String {
    let mut s = crate::parser::strip_release_suffixes(name.trim());
    loop {
        let next = POST_MEDIA_EXT.replace(&s, "").into_owned();
        let next = crate::parser::strip_release_suffixes(&next);
        if next == s || next.is_empty() {
            break;
        }
        s = next;
    }
    s
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoveryOnly {
    /// what the set says the release's files are, with their sizes. Empty: no set has been read
    pub repairs: Vec<(String, Option<i64>)>,
}

impl RecoveryOnly {
    /// The size of the files the set repairs, when it gives every one.
    pub fn repairs_bytes(&self) -> Option<i64> {
        if self.repairs.is_empty() {
            return None;
        }
        self.repairs.iter().try_fold(0i64, |sum, (_, size)| Some(sum.saturating_add((*size)?)))
    }
}

/// The most files a row can have and still be read here: PAR2-only rows have one, or a few named .par2.
const RECOVERY_ONLY_FILES: usize = 64;

/// Presentation of an own row's files and its own parsed set. This does not
/// assert that another row supplies the data or authorize copying/lending.
pub(crate) fn recovery_only_from_facts(names: &[String], repairs: Vec<(String, Option<i64>)>) -> Option<RecoveryOnly> {
    if names.is_empty() || names.len() > RECOVERY_ONLY_FILES {
        return None;
    }
    let all_named_par2 = names.iter().all(|name| is_par2(name));
    let one_sniffed = names.len() == 1 && !repairs.is_empty() && !repairs.iter().any(|(name, _)| *name == names[0]);
    (all_named_par2 || one_sniffed).then_some(RecoveryOnly { repairs })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupPosting {
    pub group: String,
    pub articles: i64,
    pub id: i64,
}

/// One post whose articles went to several groups, each article to one of them: every group holds a release of the same
/// name from the same poster, and only together do they make the files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitPost {
    /// each group's release, lowest id first
    pub ids: Vec<i64>,
    pub groups: Vec<String>,
    pub size: i64,
    /// every file has all its parts, across the groups
    pub complete: bool,
    /// for an upload linked by its PAR2 set (`archive::link`): the row of the file it repairs, the one with its name
    pub lead: Option<i64>,
    /// of a linked upload, the rows of recovery data: their files' names don't say so
    pub recovery: Vec<i64>,
    /// rows of small files posted beside the post under subjects of their own, found by its PAR2 set
    /// (`archive::link::BESIDE_FILES`): never the one listed
    pub beside: Vec<i64>,
}

impl SplitPost {
    /// The one to list. A search that isn't about one group shows only this.
    pub fn listed_id(&self) -> i64 {
        self.lead.unwrap_or_else(|| self.ids.iter().copied().filter(|id| !self.beside.contains(id)).max().unwrap_or(0))
    }
}

/// The most rows a linked upload is read with: a set of a few hundred volumes, each posted on its own, is about the most.
/// One with more isn't shown as one upload, rather than as part of one.
pub(crate) const LINKED_ROWS: usize = 1_024;

/// A From header, split up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Poster {
    pub name: Option<String>,
    pub email: Option<String>,
}

/// `Name <a@b>`, `a@b (Name)`, or either part alone.
pub fn parse_poster(raw: &str) -> Poster {
    let raw = raw.trim();
    let tidy = |s: &str| {
        let s = s.trim().trim_matches('"').trim();
        (!s.is_empty()).then(|| s.to_string())
    };
    if let (Some(open), Some(close)) = (raw.rfind('<'), raw.rfind('>'))
        && open < close
    {
        return Poster { name: tidy(&raw[..open]), email: tidy(&raw[open + 1..close]).filter(|e| e.contains('@')) };
    }
    if let (Some(open), true) = (raw.find('('), raw.ends_with(')')) {
        let address = raw[..open].trim();
        if address.contains('@') {
            return Poster { name: tidy(&raw[open + 1..raw.len() - 1]), email: tidy(address) };
        }
    }
    if raw.contains('@') && !raw.contains(' ') {
        return Poster { name: None, email: Some(raw.to_string()) };
    }
    Poster { name: tidy(raw), email: None }
}

/// Domains posters use when they don't want a reply. Most made-up addresses aren't on this list,
/// soo a miss says nothing about an address being real.
const MADE_UP_DOMAINS: &[&str] = &[
    "example.com",
    "example.org",
    "example.net",
    "invalid",
    "invalid.com",
    "localhost",
    "nowhere.com",
    "nowhere.org",
    "none.com",
    "none.net",
    "nomail.com",
    "no.spam",
    "nospam.com",
    "nospam.org",
    "spam.com",
    "fake.com",
    "anon.com",
    "anonymous.com",
    "unknown.com",
    "test.com",
    "domain.com",
    "email.com",
];

pub fn looks_made_up(email: &str) -> bool {
    let Some((_, domain)) = email.rsplit_once('@') else { return true };
    let domain = domain.to_ascii_lowercase();
    !domain.contains('.')
        || domain.ends_with(".invalid")
        || domain.ends_with(".local")
        || MADE_UP_DOMAINS.iter().any(|known| known.eq_ignore_ascii_case(&domain))
}

static PASSWORD: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)]
    Regex::new(r"(?i)\{\{([^{}\s]{1,128})\}\}|\b(?:password|passwurd|passwd|passwort|passwoord|pasword|pswd|pass|pw|pwd|mot\s+de\s+passe|mdp|wachtwoord|kennwort)\b[\s._-]*(?:is\s*)?[:=]\s*([^\s\[\]\x22']{1,128})").unwrap()
});

/// Whether a matched token is a known non-password placeholder, negative word, or MediaInfo artifact.
pub fn is_false_positive_password(found: &str) -> bool {
    let f = found.trim();
    if f.is_empty() || f.len() > 128 {
        return true;
    }
    let none = [
        "none",
        "no",
        "n/a",
        "na",
        "nil",
        "null",
        "false",
        "-",
        "free",
        "not",
        "yes",
        "keins",
        "keine",
        "kein",
        "aucun",
        "geen",
        "datum",
        "available",
        "needed",
        "required",
        "protected",
        "encrypted",
        "unknown",
        "see",
        "check",
        "inside",
        "http://",
        "https://",
    ];
    if none.iter().any(|word| f.eq_ignore_ascii_case(word)) {
        return true;
    }
    if crate::archive::PasswordHarvester::is_negative_phrase(f) {
        return true;
    }
    !f.is_empty()
        && f.bytes().all(|b| matches!(b, b'-' | b'=' | b'*' | b'_' | b'.' | b':' | b'/' | b'\\' | b'|' | b'+' | b'~'))
}

/// All candidate passwords matched by [`PASSWORD`] in `text`, in order of appearance.
pub fn find_all_passwords(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for hit in PASSWORD.captures_iter(text) {
        let found = if let Some(braced) = hit.get(1) {
            let s = braced.as_str().trim();
            if matches!(s.to_ascii_lowercase().as_str(), "password" | "pass" | "pw" | "pwd") {
                continue;
            }
            s
        } else if let Some(bare) = hit.get(2) {
            let whole = hit.get(0).map_or("", |m| m.as_str()).to_ascii_lowercase();
            if matches!(whole.as_str(), "pass=1" | "pass=2" | "pass=3") {
                continue;
            }
            bare.as_str().trim_matches(['.', ','])
        } else {
            continue;
        };
        if !is_false_positive_password(found) && !out.iter().any(|p| p == found) {
            out.push(found.to_string());
        }
    }
    out
}

/// A password a poster left in a name, subject or NFO: `{{secret}}` or `password: secret`.
/// A guess from text, soo nothing has tried it.
pub fn find_password(text: &str) -> Option<String> {
    find_all_passwords(text).into_iter().next()
}

/// A single archive/probe visit shares this allowance across all password sources and check changes.
/// The memo retains at most 24 checked, 128-byte candidates; it never grants a new allowance per filename.
#[derive(Default)]
pub(crate) struct PasswordCandidateBudget {
    checked: Vec<(crate::archive::PasswordTest, String, Option<bool>)>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PasswordBudgetExhausted;

impl From<PasswordBudgetExhausted> for crate::postprocess::step::StepError {
    fn from(_: PasswordBudgetExhausted) -> Self {
        Self::Retry("password candidate budget exhausted".into())
    }
}

impl PasswordCandidateBudget {
    const LIMIT: usize = 24;

    fn verify(&mut self, check: &crate::archive::PasswordTest, candidate: &str) -> Result<Option<bool>, PasswordBudgetExhausted> {
        if let Some((_, _, result)) = self.checked.iter().find(|(old, text, _)| old == check && text == candidate) {
            return Ok(*result);
        }
        self.available()?;
        let result = check.accepts(candidate);
        self.checked.push((*check, candidate.to_owned(), result));
        Ok(result)
    }

    fn available(&self) -> Result<(), PasswordBudgetExhausted> {
        if self.checked.len() >= Self::LIMIT { Err(PasswordBudgetExhausted) } else { Ok(()) }
    }

    #[cfg(test)]
    pub(crate) fn used(&self) -> usize { self.checked.len() }
}

/// Finds a password, keeping the public single-search interface for callers without a shared visit.
pub fn find_verified_password<'a>(
    texts: impl IntoIterator<Item = &'a str>,
    extra_candidates: &[String],
    check: Option<&crate::archive::PasswordTest>,
) -> Option<String> {
    find_verified_password_with_budget(texts, extra_candidates, check, &mut PasswordCandidateBudget::default()).ok().flatten()
}

/// Explicit/cached candidates precede broader tokens. Only the existing supported verifier may accept a guess.
/// Exhaustion is distinct from a completed search with no password, so workers cannot persist a false negative.
pub(crate) fn find_verified_password_with_budget<'a>(
    texts: impl IntoIterator<Item = &'a str>,
    extra_candidates: &[String],
    check: Option<&crate::archive::PasswordTest>,
    budget: &mut PasswordCandidateBudget,
) -> Result<Option<String>, PasswordBudgetExhausted> {
    let texts: Vec<&str> = texts.into_iter().collect();
    let empty_meta = std::collections::HashMap::new();
    // Unsupported KDFs retain explicit hints without spending crypto or turning the builtin into a guess.
    let check = check.filter(|check| check.can_verify());
    macro_rules! try_candidate {
        ($candidate:expr) => {{
            let candidate = $candidate.trim();
            if !candidate.is_empty() && candidate.len() <= 128 && !is_false_positive_password(candidate) {
                match check {
                    None => return Ok(Some(candidate.to_owned())),
                    Some(check) if budget.verify(check, candidate)? == Some(true) => return Ok(Some(candidate.to_owned())),
                    Some(_) => {},
                }
            }
        }};
    }
    for candidate in extra_candidates {
        try_candidate!(candidate);
    }
    for &text in &texts {
        for candidate in find_all_passwords(text) {
            try_candidate!(candidate);
        }
        for candidate in crate::archive::PasswordHarvester::extract_candidates(text, Some(text), &empty_meta) {
            try_candidate!(candidate);
        }
    }
    if check.is_none() { return Ok(None); }
    // These guesses are verified only, after all explicit candidates. The memo deduplicates them across calls.
    const COMMON_ARCHIVE_PASSWORDS: &[&str] = &["74744567bnbnvbnb"];
    for &password in COMMON_ARCHIVE_PASSWORDS {
        try_candidate!(password);
    }
    static RE_URL: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"(?i)\b(?:https?://|www\.)[^\s\x22'<>]{3,96}|\b[a-z0-9][-a-z0-9]{1,48}\.(?:com|net|org|to|cc|info|biz|ws|de|nl|ru|me|tv|io|xyz)\b").unwrap()
    });
    for &t in &texts {
        for m in RE_URL.find_iter(t) {
            let u = m.as_str().trim_end_matches(['.', ',', ')', ']']);
            try_candidate!(u);
            if let Some(stripped) = u.strip_prefix("https://").or_else(|| u.strip_prefix("http://")) {
                try_candidate!(stripped.trim_end_matches('/'));
                if let Some(no_www) = stripped.strip_prefix("www.") {
                    try_candidate!(no_www.trim_end_matches('/'));
                }
            } else if let Some(no_www) = u.strip_prefix("www.") {
                try_candidate!(no_www.trim_end_matches('/'));
            }
        }
        for raw_line in t.lines() {
            let line = raw_line
                .trim_matches(|c: char| {
                    c.is_whitespace()
                        || ('\u{2500}'..='\u{259f}').contains(&c)
                        || matches!(c, '*' | '#' | '=' | '-' | '|' | ':' | '~')
                })
                .trim();
            if line.is_empty() {
                continue;
            }
            if line.len() <= 80 && !line.contains(char::is_whitespace) && (3..=64).contains(&line.len()) {
                try_candidate!(line);
            }
            for (idx, _) in line.match_indices([':', '=']) {
                let rhs = line[idx + 1..].trim();
                let val = rhs.trim_matches(['"', '\'', '.', ',']);
                if !val.contains(char::is_whitespace) && (3..=64).contains(&val.len()) {
                    try_candidate!(val);
                } else if let Some(first) = val.split_whitespace().next() {
                    let tok = first.trim_matches(['"', '\'', '.', ',', ';', ')', ']']);
                    if (3..=64).contains(&tok.len()) {
                        try_candidate!(tok);
                    }
                }
            }
        }
    }
    // Even an exactly spent search remains unresolved: later sources must not fetch or record an empty password.
    budget.available()?;
    Ok(None)
}

/// What the page can say without reading any article.
pub struct PasswordHint {
    pub password: String,
    pub source: &'static str,
}

/// The likeliest password `archive::PasswordHarvester` reads out of one line, if any.
pub fn harvested_password(text: &str) -> Option<String> {
    let none = std::collections::HashMap::new();
    crate::archive::PasswordHarvester::extract_candidates(text, None, &none)
        .into_iter()
        .find(|p| !p.is_empty() && p.len() <= 128)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn poster(name: Option<&str>, email: Option<&str>) -> Poster {
        Poster { name: name.map(Into::into), email: email.map(Into::into) }
    }

    #[test]
    fn an_estimate_goes_by_the_whole_of_the_data() {
        let half_there = Contents {
            data_articles_expected: 10,
            data_articles_seen: 5,
            data_bytes: 5_000,
            recovery_articles_expected: 3,
            recovery_articles_seen: 3,
            recovery_bytes: 3_000,
            ..Contents::default()
        };
        assert!(!half_there.repairable(), "3 can't stand in for 5");
        assert_eq!(half_there.repair_per_mille(), Some(300));
        // what's gone of the recovery volumes is already out of their size
        let thin = Contents {
            data_articles_expected: 100,
            data_articles_seen: 92,
            data_bytes: 92_000,
            recovery_articles_expected: 20,
            recovery_articles_seen: 10,
            recovery_bytes: 10_000,
            ..Contents::default()
        };
        assert!(thin.repairable());
        // overviews can leave sizes out, and no sizes say nothing
        let unsized_data = Contents { data_bytes: 0, recovery_bytes: 0, ..half_there };
        assert!(!unsized_data.repairable());
    }

    #[test]
    fn one_block_cant_repair_two_of_ten_thousand() {
        let contents = |seen| Contents {
            data_articles_expected: 10_000,
            data_articles_seen: seen,
            recovery_articles_expected: 1,
            recovery_articles_seen: 1,
            recovery_blocks: 1,
            par2_block: Some(1),
            par2_blocks: Some(10_000),
            ..Contents::default()
        };
        assert!(!contents(9_998).repairable());
        assert!(contents(9_999).repairable());
    }

    #[test]
    fn a_from_header_splits_in_each_shape() {
        assert_eq!(parse_poster("Some One <a@b.com>"), poster(Some("Some One"), Some("a@b.com")));
        assert_eq!(parse_poster("\"Some One\" <a@b.com>"), poster(Some("Some One"), Some("a@b.com")));
        assert_eq!(parse_poster("<a@b.com>"), poster(None, Some("a@b.com")));
        assert_eq!(parse_poster("a@b.com (Some One)"), poster(Some("Some One"), Some("a@b.com")));
        assert_eq!(parse_poster("a@b.com"), poster(None, Some("a@b.com")));
        assert_eq!(parse_poster("Just A Name"), poster(Some("Just A Name"), None));
        assert_eq!(parse_poster("Name <not an address>"), poster(Some("Name"), None));
        assert_eq!(parse_poster("  "), poster(None, None));
        assert_eq!(parse_poster("a > b < c"), poster(Some("a > b < c"), None));
    }

    #[test]
    fn only_known_throwaway_addresses_are_called_made_up() {
        for fake in ["a@example.com", "a@nowhere.com", "a@host", "a@x.invalid", "a@NoSpam.com", "no at sign"] {
            assert!(looks_made_up(fake), "{fake}");
        }
        assert!(!looks_made_up("a@gmail.com"));
    }

    #[test]
    fn missing_and_repair_are_shares() {
        let c = Contents {
            articles_seen: 97,
            articles_expected: 100,
            data_bytes: 1000,
            recovery_bytes: 100,
            ..Contents::default()
        };
        assert_eq!(c.missing_per_mille(), Some(30));
        assert_eq!(c.repair_per_mille(), Some(100));
        assert_eq!(Contents::default().missing_per_mille(), None);
        assert_eq!(Contents::default().repair_per_mille(), None);
    }

    #[test]
    fn repair_compares_missing_data_with_the_recovery_that_is_there() {
        let holes = Contents {
            data_articles_seen: 97,
            data_articles_expected: 100,
            recovery_articles_seen: 10,
            recovery_articles_expected: 10,
            data_bytes: 1000,
            recovery_bytes: 100,
            ..Contents::default()
        };
        assert!(holes.repairable(), "3% missing, 10% repairable");
        let lost = Contents { recovery_articles_seen: 2, recovery_bytes: 20, ..holes.clone() };
        assert!(!lost.repairable(), "most of the recovery is gone too");
        let none = Contents { recovery_articles_expected: 0, recovery_articles_seen: 0, ..holes };
        assert!(!none.repairable());
    }

    #[test]
    fn exact_repair_counts_blocks() {
        assert_eq!(recovery_blocks("a.vol003+04.par2"), 4);
        assert_eq!(recovery_blocks("a.vol001-003.par2"), 2);
        assert_eq!(recovery_blocks("a.vol063-101.par2"), 38);
        assert_eq!(recovery_blocks("a.vol000-000.par2"), 1);
        assert_eq!(recovery_blocks("a.par2"), 0);
        let c = Contents { recovery_blocks: 10, par2_block: Some(1000), par2_blocks: Some(100), ..Contents::default() };
        assert_eq!(c.exact_repair_per_mille(), Some(100), "10 of 100 blocks");
        assert_eq!(Contents::default().exact_repair_per_mille(), None);
    }

    #[test]
    fn recovery_volumes_are_told_from_the_index() {
        use crate::parity::is_base_par2;
        assert!(is_par2("A.PAR2") && is_base_par2("a.par2"));
        assert!(!is_base_par2("a.vol000+01.par2"));
        assert!(!is_par2("a.rar"));
    }

    #[test]
    fn passwords_are_found_where_posters_leave_them() {
        assert_eq!(find_password("Some.Movie.2024.{{s3cret!}}.rar").as_deref(), Some("s3cret!"));
        assert_eq!(find_password("Password: hunter2").as_deref(), Some("hunter2"));
        assert_eq!(find_password("Password: hunter2.").as_deref(), Some("hunter2"), "the end of a sentence");
        assert_eq!(
            find_password(
                r#"Various pictures - Part 19 - RAR Passwurd: variouspics [003/371] "VariousPics-Part19.part001.rar" yEnc (0001/1093)"#
            )
            .as_deref(),
            Some("variouspics")
        );
        assert_eq!(find_password("Movie.{{.secret,}}.rar").as_deref(), Some(".secret,"), "the braces say where");
        assert_eq!(find_password("PW=abc123, enjoy").as_deref(), Some("abc123"));
        assert_eq!(find_password("password is: abc").as_deref(), Some("abc"));
        assert_eq!(find_password("Password: none"), None);
        assert_eq!(find_password("Passworded.Movie.2024"), None);
        assert_eq!(find_password("Compass: north").as_deref(), None);
        assert_eq!(find_password("nothing here"), None);
    }

    #[test]
    fn name_sources_read_as_words() {
        assert_eq!(name_source_label(10), Some("the NFO"));
        assert_eq!(name_source_label(100), Some("PreDB"));
        assert_eq!(name_source_label(0), None);
    }

    #[test]
    fn a_subject_has_the_files_size_only_as_its_last_number() {
        assert!(subject_has_size(r#"[10/11] - "Film.mkv" yEnc (/21040) 88246893816"#));
        assert!(!subject_has_size(r#"[10/11] - "Film.mkv" yEnc (/21040)"#));
        assert!(!subject_has_size(r#"[10/11] - "Film.mkv" yEnc (/21040) 88 MB"#));
        assert!(!subject_has_size("Film.mkv 88246893816"));
        assert_eq!(subject_size(r#""Film.mkv" yEnc (/21040) 88246893816"#), Some(88_246_893_816));
    }
    #[test]
    fn union_files_complete_honors_non_null_file_total() {
        let mut uf = UnionFiles::default();
        uf.file_totals.insert(None);
        uf.file_totals.insert(Some(3));
        uf.files.insert("a.par2".into(), UnionFile { expected: Some(1), seen: vec![0b10], subject: None });
        uf.files.insert("a.mkv".into(), UnionFile { expected: Some(1), seen: vec![0b10], subject: None });
        assert!(!uf.complete(), "2 files when a non-null file_total says 3 must not be complete");
    }
}

#[cfg(test)]
#[path = "release_info/native_grouping_tests.rs"]
mod native_grouping_tests;
