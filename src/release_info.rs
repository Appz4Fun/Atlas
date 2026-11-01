//! What the release page says about a posting itself: who posted it, its names, its PAR2 files
//! and how much of it is there. Read when the page opens, soo nothing here is stored.

use std::collections::HashMap;

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
}

impl Contents {
    /// Share of the articles that are missing, in tenths of a percent. None: no count to go by.
    pub fn missing_per_mille(&self) -> Option<i64> {
        (self.articles_expected > 0)
            .then(|| (self.articles_expected - self.articles_seen).max(0) * 1000 / self.articles_expected)
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
    Ok(conn
        .query_row(&sql, [id], |r| {
            Ok(Names {
                posted: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                display: r.get(1)?,
                obfuscated: r.get::<_, Option<i64>>(2)?.unwrap_or(0) != 0,
                source: r.get(3)?,
            })
        })
        .optional()?)
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
        totals.articles_expected += expected.unwrap_or(seen).max(seen);
        if is_par2(&filename) {
            totals.par2_files += 1;
            totals.recovery_volumes += i64::from(is_recovery_volume(&filename));
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn recovery_volumes_are_told_from_the_index() {
        assert!(is_par2("A.PAR2") && !is_recovery_volume("a.par2"));
        assert!(is_recovery_volume("a.vol000+01.par2"));
        assert!(!is_par2("a.rar"));
    }

    #[test]
    fn name_sources_read_as_words() {
        assert_eq!(name_source_label(10), Some("the NFO"));
        assert_eq!(name_source_label(100), Some("PreDB"));
        assert_eq!(name_source_label(0), None);
    }
}
