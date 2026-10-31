//! What to call a release from the names inside it, and which of those are no
//! use. The name classes follow newznab-tmux, GPLv3 (`Par2Processor`,
//! `AdditionalWorkPlanner`).

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use super::{InnerFile, MAX_NAME_BYTES, MAX_STORED};

static VOLUME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:\.part\d+\.rar|\.vol\d+[+-]\d+\.par2|\.par2|\.r\d{2,3}|\.s\d{2}|\.z\d{2,3}|\.rar|\.cbr|\.cbz|\.zipx?|\.7z(?:\.\d{3})?|\.\d{3})$").unwrap()
});
static RANDOM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:[a-z0-9]{16,}|[0-9a-f]{8,}|\d+|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$")
        .unwrap()
});
static SUPPORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\.(?:nfo|txt|diz|sfv|srr|srs|nzb|md5|sha1|url|jpe?g|png|gif|webp|bmp|tiff?|idx|sub|srt|ass)$")
        .unwrap()
});
static EXTRA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:^|[._ -])(?:sample|proof|cover|subs?)(?:[._ -]|$)").unwrap());
static LAST_EXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.[A-Za-z0-9]{1,8}$").unwrap());

/// A name as read from an archive: no control characters, `/` for a separator,
/// bounded. Nothing is left, or only `.` or `..`: None.
pub fn clean_name(raw: &str) -> Option<String> {
    let mut name: String = raw.chars().filter(|c| !c.is_control()).map(|c| if c == '\\' { '/' } else { c }).collect();
    if name.len() > MAX_NAME_BYTES {
        let mut end = MAX_NAME_BYTES;
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        name.truncate(end);
    }
    let name = name.trim();
    let name = name.strip_prefix("./").unwrap_or(name);
    let name = name.trim_end_matches('/');
    (!name.is_empty() && name != "." && name != "..").then(|| name.to_string())
}

/// after the last separator
pub fn base_name(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// without a volume ending that isnt all of it
pub fn strip_volume_suffix(name: &str) -> &str {
    match VOLUME.find(name) {
        Some(m) if m.start() > 0 => &name[..m.start()],
        _ => name,
    }
}

/// a name's own words: no path, no volume ending, no extension
pub fn stem(name: &str) -> &str {
    let name = strip_volume_suffix(base_name(name));
    match LAST_EXT.find(name) {
        Some(m) if m.start() > 0 => &name[..m.start()],
        _ => name,
    }
}

pub fn looks_random(stem: &str) -> bool {
    RANDOM.is_match(stem)
}

/// What to call a release from its files: the biggest isnt always the useful
/// one. Random names are skipped, a real name beats a sample or a support file,
/// and among equals the most bytes wins.
pub fn best_name<'a>(files: impl Iterator<Item = (&'a str, u64)>) -> Option<String> {
    // case-insensitive shown name -> (tier, summed size, shown)
    let mut shown: HashMap<String, (u8, u64, String)> = HashMap::new();
    for (name, size) in files {
        let base = base_name(name);
        let own = stem(base);
        if looks_random(own) {
            continue;
        }
        let (tier, display) = if SUPPORT.is_match(base) {
            if !own.contains(['.', '_', ' ', '-']) {
                continue;
            }
            (0, own.to_string())
        } else if EXTRA.is_match(own) {
            (1, strip_volume_suffix(base).to_string())
        } else {
            (2, strip_volume_suffix(base).to_string())
        };
        let entry = shown.entry(display.to_lowercase()).or_insert((tier, 0, display));
        entry.0 = entry.0.max(tier);
        entry.1 = entry.1.saturating_add(size);
    }
    shown
        .into_values()
        .max_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)).then_with(|| b.2.cmp(&a.2)))
        .map(|(_, _, display)| display)
}

/// One row per set of volumes, the first of them with their sizes summed. Other
/// files once per name. In order of first appearance, at most `MAX_STORED`.
pub fn collapse_volumes(files: &[InnerFile]) -> Vec<InnerFile> {
    let mut out: Vec<InnerFile> = Vec::new();
    let mut sets: HashMap<&str, usize> = HashMap::new();
    for file in files {
        if VOLUME.is_match(&file.name) {
            let set = strip_volume_suffix(&file.name);
            if let Some(at) = sets.get(set) {
                let row = &mut out[*at];
                row.size = match (row.size, file.size) {
                    (Some(a), Some(b)) => Some(a.saturating_add(b)),
                    (a, b) => a.or(b),
                };
                continue;
            }
            if out.len() >= MAX_STORED {
                continue;
            }
            sets.insert(set, out.len());
            out.push(file.clone());
        } else if out.len() < MAX_STORED && !out.iter().any(|f| f.name == file.name) {
            out.push(file.clone());
        }
    }
    out
}
