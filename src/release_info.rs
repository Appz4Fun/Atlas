//! What the release page says about a posting itself: who posted it, its names, its PAR2 files
//! and how much of it is there. Read when the page opens, soo nothing here is stored.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use rusqlite::{Connection, OptionalExtension};

use crate::db::{self, Result};
use crate::naming::NameSource;
use crate::store;

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

/// Totals over a release's files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Contents {
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
        let (covered, of) = self.repair_ratio();
        let missing = i128::from((self.data_articles_expected - self.data_articles_seen).max(0));
        // whole numbers: rounding each to tenths of a percent would call 2 missing of 10,000 repairable by 1 block
        i128::from(covered) * i128::from(self.recovery_articles_seen) * i128::from(self.data_articles_expected)
            >= missing * i128::from(of.max(1)) * i128::from(self.recovery_articles_expected)
    }

    /// What can be rebuilt, over what there is to rebuild: exact from the PAR2 set, or else by sizes.
    fn repair_ratio(&self) -> (i64, i64) {
        match self.par2_blocks {
            Some(blocks) if self.par2_block.is_some_and(|b| b > 0) && blocks > 0 && self.recovery_blocks > 0 => {
                (self.recovery_blocks, blocks)
            }
            _ => (self.recovery_bytes, self.data_bytes),
        }
    }

    /// What the recovery blocks cover, in tenths of a percent, when the PAR2 set's been read.
    pub fn exact_repair_per_mille(&self) -> Option<i64> {
        let blocks = self.par2_blocks?;
        (self.par2_block? > 0 && blocks > 0 && self.recovery_blocks > 0).then(|| self.recovery_blocks * 1000 / blocks)
    }

    /// What the recovery volumes could rebuild, as a share of the data in tenths of a percent.
    /// From sizes on the wire, soo it's an estimate: real blocks are a little smaller than the volumes.
    pub fn repair_per_mille(&self) -> Option<i64> {
        (self.data_bytes > 0).then(|| self.recovery_bytes * 1000 / self.data_bytes)
    }
}

fn is_par2(filename: &str) -> bool {
    filename.to_ascii_lowercase().ends_with(".par2")
}

/// The `MM` of `name.vol000+MM.par2`.
fn recovery_blocks(filename: &str) -> i64 {
    static BLOCKS: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"(?i)\.vol\d+[+-](\d+)\.par2$").unwrap()
    });
    BLOCKS.captures(filename).and_then(|hit| hit[1].parse().ok()).unwrap_or(0)
}

fn is_recovery_volume(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    lower.contains(".vol") && lower.ends_with(".par2")
}

pub fn names(id: i64) -> Result<Option<Names>> {
    names_with(&db::open()?, id)
}

pub fn names_with(conn: &Connection, id: i64) -> Result<Option<Names>> {
    let sql = format!(
        "select name, display_name, is_obfuscated, name_source from s{}.releases where id = ?",
        store::shard_of_id(id)
    );
    conn.query_row(&sql, [id], |r| {
        Ok(Names {
            posted: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
            display: r.get(1)?,
            obfuscated: r.get::<_, Option<i64>>(2)?.unwrap_or(0) != 0,
            source: r.get(3)?,
        })
    })
    .optional()
}

pub fn contents(id: i64) -> Result<Contents> {
    contents_with(&db::open()?, id)
}

pub fn contents_with(conn: &Connection, id: i64) -> Result<Contents> {
    let s = store::shard_of_id(id);
    let mut totals = Contents::default();
    let mut stmt = conn.prepare(&format!("select filename, expected, seen from s{s}.files where release_id = ?"))?;
    let files = stmt.query_map([id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?, store::seen_count(&r.get::<_, Vec<u8>>(2)?)))
    })?;
    for file in files {
        let (filename, expected, seen) = file?;
        totals.files += 1;
        totals.articles_seen += seen;
        let want = expected.unwrap_or(seen).max(seen);
        totals.articles_expected += want;
        if is_recovery_volume(&filename) {
            totals.recovery_articles_seen += seen;
            totals.recovery_articles_expected += want;
        } else if !is_par2(&filename) {
            totals.data_articles_seen += seen;
            totals.data_articles_expected += want;
        }
        if is_par2(&filename) {
            totals.par2_files += 1;
            totals.recovery_volumes += i64::from(is_recovery_volume(&filename));
            totals.recovery_blocks += recovery_blocks(&filename);
        }
    }
    let mut bytes: HashMap<String, i64> = HashMap::new();
    store::each_article(conn, id, &mut |article| {
        *bytes.entry(article.filename.unwrap_or_default()).or_default() += article.bytes.unwrap_or(0);
        Ok(())
    })?;
    for (filename, size) in bytes {
        if is_recovery_volume(&filename) {
            totals.recovery_bytes += size;
        } else if !is_par2(&filename) {
            totals.data_bytes += size;
        }
    }
    if let Some((block, blocks)) = crate::duplicates::par2_sizes(conn, id) {
        totals.par2_block = Some(block);
        totals.par2_blocks = Some(blocks);
    }
    Ok(totals)
}

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
    Regex::new(r"(?i)\{\{([^{}\s]{1,128})\}\}|\b(?:password|passwd|passwort|pass|pw)\b\s*(?:is\s*)?[:=]\s*([^\s\[\]\x22']{1,128})").unwrap()
});

/// A password a poster left in a name, subject or NFO: `{{secret}}` or `password: secret`.
/// A guess from text, soo nothing has tried it.
pub fn find_password(text: &str) -> Option<String> {
    let hit = PASSWORD.captures(text)?;
    // the braces say where it ends: only a bare password can have a sentence's punctuation after it
    let found = match hit.get(1) {
        Some(braced) => braced.as_str(),
        None => hit.get(2)?.as_str().trim_matches(|c: char| c == '.' || c == ','),
    };
    let none = ["none", "no", "n/a", "na", "nil", "-", "free", "not", "yes", ""];
    (!none.iter().any(|word| found.eq_ignore_ascii_case(word))).then(|| found.to_string())
}

/// What the page can say without reading any article.
pub struct PasswordHint {
    pub password: String,
    pub source: &'static str,
}

pub fn password_hint(id: i64, names: Option<&Names>) -> Option<PasswordHint> {
    let conn = db::open().ok()?;
    password_hint_with(&conn, id, names)
}

pub fn password_hint_with(conn: &Connection, id: i64, names: Option<&Names>) -> Option<PasswordHint> {
    let hint = |password: String, source| Some(PasswordHint { password, source });
    if let Some(password) = crate::archive::store::nfo_password(conn, id) {
        return hint(password, "the NFO");
    }
    for name in names.into_iter().flat_map(|n| [Some(&n.posted), n.display.as_ref()]).flatten() {
        if let Some(password) = find_password(name) {
            return hint(password, "the release name");
        }
    }
    let s = store::shard_of_id(id);
    let mut stmt = conn
        .prepare(&format!("select filename, subject from s{s}.files where release_id = ? order by id limit 64"))
        .ok()?;
    let files: Vec<(String, Option<String>)> =
        stmt.query_map([id], |r| Ok((r.get(0)?, r.get(1)?))).ok()?.filter_map(|row| row.ok()).collect();
    for (filename, subject) in files {
        if let Some(password) = find_password(&filename) {
            return hint(password, "a file name");
        }
        if let Some(password) = subject.as_deref().and_then(find_password) {
            return hint(password, "the subject");
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn poster(name: Option<&str>, email: Option<&str>) -> Poster {
        Poster { name: name.map(Into::into), email: email.map(Into::into) }
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
        let lost = Contents { recovery_articles_seen: 2, ..holes.clone() };
        assert!(!lost.repairable(), "most of the recovery is gone too");
        let none = Contents { recovery_articles_expected: 0, recovery_articles_seen: 0, ..holes };
        assert!(!none.repairable());
    }

    #[test]
    fn exact_repair_counts_blocks() {
        assert_eq!(recovery_blocks("a.vol003+04.par2"), 4);
        assert_eq!(recovery_blocks("a.par2"), 0);
        let c = Contents { recovery_blocks: 10, par2_block: Some(1000), par2_blocks: Some(100), ..Contents::default() };
        assert_eq!(c.exact_repair_per_mille(), Some(100), "10 of 100 blocks");
        assert_eq!(Contents::default().exact_repair_per_mille(), None);
    }

    #[test]
    fn recovery_volumes_are_told_from_the_index() {
        assert!(is_par2("A.PAR2") && !is_recovery_volume("a.par2"));
        assert!(is_recovery_volume("a.vol000+01.par2"));
        assert!(!is_par2("a.rar"));
    }

    #[test]
    fn passwords_are_found_where_posters_leave_them() {
        assert_eq!(find_password("Some.Movie.2024.{{s3cret!}}.rar").as_deref(), Some("s3cret!"));
        assert_eq!(find_password("Password: hunter2").as_deref(), Some("hunter2"));
        assert_eq!(find_password("Password: hunter2.").as_deref(), Some("hunter2"), "the end of a sentence");
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
}
