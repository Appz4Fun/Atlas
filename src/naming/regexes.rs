//! The naming regexes: per group, compiled when a group first asks.
//!
//! Ported from newznab-tmux `database/seeders/ReleaseNamingRegexesTableSeeder.php` and
//! `app/Services/RegexService.php`, GPLv3. `scripts/naming_regexes_to_tsv.py` makes the data.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use regex::{Regex, RegexBuilder};

const DATA: &str = include_str!("../../data/naming_regexes.tsv");
const MAX_SUBJECT: usize = 1024;

struct Line {
    id: u32,
    group: usize,
    insensitive: bool,
    pattern: &'static str,
}

struct Table {
    /// one per distinct group regex, None when it failed to build
    groups: Vec<Option<Regex>>,
    /// in the file's order: `ordinal, group_regex, id`
    lines: Vec<Line>,
}

static TABLE: LazyLock<Table> = LazyLock::new(|| {
    let mut index: HashMap<&'static str, usize> = HashMap::new();
    let (mut groups, mut lines) = (Vec::new(), Vec::new());
    for line in DATA.lines().filter(|l| !l.starts_with('#')) {
        let mut fields = line.split('\t');
        let (Some(id), Some(_ordinal), Some(group), Some(flags), Some(pattern)) =
            (fields.next(), fields.next(), fields.next(), fields.next(), fields.next())
        else {
            eprintln!("naming regexes: a malformed line was skipped");
            continue;
        };
        let Ok(id) = id.parse() else { continue };
        let group = *index.entry(group).or_insert_with(|| {
            let built = RegexBuilder::new(group).case_insensitive(true).build();
            if built.is_err() {
                eprintln!("naming regexes: a group regex failed to build: {group}");
            }
            groups.push(built.ok());
            groups.len() - 1
        });
        lines.push(Line { id, group, insensitive: flags.contains('i'), pattern });
    }
    Table { groups, lines }
});

pub struct Rule {
    pub id: u32,
    re: Regex,
    /// capture names, sorted, without reqid or parts
    names: Vec<String>,
}

impl Rule {
    /// the rule's named groups joined in name order. Empty is None
    pub fn name_from(&self, subject: &str) -> Option<String> {
        let c = self.re.captures(subject)?;
        let joined: String = self.names.iter().filter_map(|n| c.name(n)).map(|m| m.as_str()).collect();
        (!joined.is_empty()).then_some(joined)
    }
}

fn compile(line: &Line) -> Option<Rule> {
    COMPILED.fetch_add(1, Ordering::Relaxed);
    let re = match RegexBuilder::new(line.pattern).case_insensitive(line.insensitive).build() {
        Ok(re) => re,
        Err(e) => {
            eprintln!("naming regexes: rule {} failed to build: {e}", line.id);
            return None;
        }
    };
    let mut names: Vec<String> = re
        .capture_names()
        .flatten()
        .filter(|n| {
            let lower = n.to_lowercase();
            !lower.contains("reqid") && !lower.contains("parts")
        })
        .map(String::from)
        .collect();
    names.sort();
    Some(Rule { id: line.id, re, names })
}

type Cell = Arc<OnceLock<Vec<Rule>>>;

/// A cell per group name seen. The map lock is held only to get the cell; a group compiles
/// inside its own cell, soo a slow first compile holds up only threads asking for that group.
static BY_GROUP: LazyLock<Mutex<HashMap<String, Cell>>> = LazyLock::new(Default::default);
static HAS_RULES: LazyLock<Mutex<HashMap<String, bool>>> = LazyLock::new(Default::default);

/// a group's lines match by its regex: the table is shared by every thread, soo this only reads
fn matching_groups(group: &str) -> Vec<bool> {
    TABLE.groups.iter().map(|g| g.as_ref().is_some_and(|re| re.is_match(group))).collect()
}

fn cell_for(group: &str) -> Cell {
    BY_GROUP.lock().unwrap().entry(group.to_string()).or_default().clone()
}

/// A group's rules, compiled the first time it's asked for.
pub fn rules_for(group: &str) -> Cell {
    let cell = cell_for(group);
    cell.get_or_init(|| {
        let matching = matching_groups(group);
        TABLE.lines.iter().filter(|l| matching.get(l.group).copied().unwrap_or(false)).filter_map(compile).collect()
    });
    cell
}

/// Each rule's output in order. A subject over `MAX_SUBJECT` gives none.
/// `subject` is `collections::subject_base`'s output: the form upstream's rules were written for.
pub fn names(group: &str, subject: &str) -> Vec<String> {
    if subject.len() > MAX_SUBJECT || !group_has_rules(group) {
        return Vec::new();
    }
    let cell = rules_for(group);
    cell.get().map_or_else(Vec::new, |rules| rules.iter().filter_map(|r| r.name_from(subject)).collect())
}

/// Whether any rule's group regex matches. Compiles no rule, and is remembered per group name.
pub fn group_has_rules(group: &str) -> bool {
    if let Some(known) = HAS_RULES.lock().unwrap().get(group) {
        return *known;
    }
    let has = matching_groups(group).into_iter().any(|m| m);
    HAS_RULES.lock().unwrap().insert(group.to_string(), has);
    has
}

/// rules compiled so far, for the laziness test
#[doc(hidden)]
pub static COMPILED: AtomicUsize = AtomicUsize::new(0);

/// compiles one, for the sample test
#[doc(hidden)]
pub fn rule_by_id(id: u32) -> Option<Rule> {
    TABLE.lines.iter().find(|l| l.id == id).and_then(compile)
}

/// every rule's id with its group regex, in the file's order, for tests
#[doc(hidden)]
pub fn every_id() -> Vec<u32> {
    TABLE.lines.iter().map(|l| l.id).collect()
}

/// how many distinct group regexes built, for tests
#[doc(hidden)]
pub fn group_regexes_built() -> usize {
    TABLE.groups.iter().flatten().count()
}
