//! Which articles belong to one post, whatever its files are called: the
//! collection a subject belongs to, from per group regexes with a generic
//! fallback. Ported from newznab-tmux (`CollectionsCleaningService`,
//! `CollectionHandler`, `RegexService`, the `collection_regexes` seed), GPLv3.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use indexmap::IndexMap;
use regex::{Regex, RegexBuilder};

use crate::filters::COUNTER_RE;
use crate::parser::{Article, ParsedSubject, Release};

/// a post claiming more files than this isnt merged: its files stay releases of
/// their own, as a forged counter cant make one release of a million files
pub const MAX_COLLECTION_FILES: i64 = 10_000;

/// letters and digits a cleaned name needs to tell a post by itself; under
/// this the poster and the day have to help (`IMG_`, `DSC`)
const STRONG_NAME: usize = 16;

const IMAGE_EXT: [&str; 8] = ["jpg", "jpeg", "png", "gif", "bmp", "webp", "tif", "tiff"];
const AUDIO_EXT: [&str; 9] = ["mp3", "flac", "m4a", "ogg", "wav", "wma", "ape", "aac", "opus"];

/// id, ordinal, group_regex, flags, pattern, in the order they are tried. Made by
/// scripts/collection_regexes_to_tsv.py.
const TABLE: &str = include_str!("../data/collection_regexes.tsv");

struct Row {
    /// its line in the table: upstream's order, over every group_regex that matches
    order: usize,
    id: i32,
    case_insensitive: bool,
    regex: &'static str,
}

/// each distinct group_regex with the rows under it
static BY_GROUP_REGEX: LazyLock<Vec<(Regex, Vec<Row>)>> = LazyLock::new(|| {
    let mut by: IndexMap<&str, Vec<Row>> = IndexMap::new();
    for (order, line) in TABLE.lines().filter(|l| !l.starts_with('#')).enumerate() {
        let mut cols = line.split('\t');
        let (Some(id), Some(_ordinal), Some(group), Some(flags), Some(regex)) =
            (cols.next(), cols.next(), cols.next(), cols.next(), cols.next())
        else {
            continue;
        };
        let row = Row { order, id: id.parse().unwrap_or(0), case_insensitive: flags.contains('i'), regex };
        by.entry(group).or_default().push(row);
    }
    by.into_iter().filter_map(|(group, rows)| Some((Regex::new(group).ok()?, rows))).collect()
});

/// One group's regexes in upstream's order, compiled the first time it comes up.
type Compiled = Arc<Vec<Regex>>;
static COMPILED: LazyLock<Mutex<HashMap<String, Compiled>>> = LazyLock::new(Default::default);

fn compiled(group: &str) -> Compiled {
    if let Some(found) = COMPILED.lock().unwrap().get(group) {
        return found.clone();
    }
    let mut rows: Vec<&Row> =
        BY_GROUP_REGEX.iter().filter(|(g, _)| g.is_match(group)).flat_map(|(_, rows)| rows).collect();
    rows.sort_by_key(|r| r.order);
    // outside the lock: two threads compiling one group at once is only waste
    let regexes = rows.iter().filter_map(|row| {
        let built = RegexBuilder::new(row.regex).case_insensitive(row.case_insensitive).build();
        built.inspect_err(|e| println!("collection regex {} doesnt compile: {e}", row.id)).ok()
    });
    let list: Compiled = Arc::new(regexes.collect());
    COMPILED.lock().unwrap().insert(group.to_string(), list.clone());
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_regex_compiles() {
        let mut count = 0;
        for (_, rows) in BY_GROUP_REGEX.iter() {
            for row in rows {
                RegexBuilder::new(row.regex).case_insensitive(row.case_insensitive).build()
                    .unwrap_or_else(|e| panic!("regex {}: {e}", row.id));
                assert!(row.regex.contains("(?P<match"), "regex {} has no named group: nothing would come of it", row.id);
                count += 1;
            }
        }
        assert_eq!(count, 602);
        assert_eq!(BY_GROUP_REGEX.len(), 101);
    }

    #[test]
    fn group_regexes_run_in_upstreams_order_lazily() {
        assert_eq!(compiled("alt.binaries.teevee").len(), 22);
        assert_eq!(compiled("alt.binaries.boneless").len(), 26);
        assert!(compiled("alt.binaries.no.such").is_empty());
        assert!(Arc::ptr_eq(&compiled("alt.binaries.teevee"), &compiled("alt.binaries.teevee")));
    }

    #[test]
    fn upstreams_order_holds_over_group_regexes() {
        // `alt.binaries.ath` has rows under two group_regexes
        let table: Vec<&str> = TABLE.lines().filter(|l| !l.starts_with('#'))
            .map(|l| l.split('\t').collect::<Vec<_>>())
            .filter(|c| c[2] == r"^alt\.binaries\.ath$" || c[2] == r"^alt\.binaries\.(cores|ath)$")
            .map(|c| c[4]).collect();
        let got = compiled("alt.binaries.ath");
        assert!(table.len() >= 2);
        assert_eq!(got.iter().map(|r| r.as_str()).collect::<Vec<_>>(), table);
    }

    #[test]
    fn no_group_of_the_seed_is_blocked() {
        let names = BY_GROUP_REGEX.iter().map(|(g, _)| g.as_str().replace('\\', ""));
        let hits: Vec<String> = names.filter(|g| crate::filters::group_blocked(g)).collect();
        assert!(hits.is_empty(), "{hits:?}");
    }
}
