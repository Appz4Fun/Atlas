//! Which article to fetch, from file names and volume order. The name families
//! and the probing rule follow newznab-tmux, GPLv3 (`AdditionalWorkPlanner`).

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::nfo;
use crate::store::FileInfo;

use super::PROBE_MIN_PARTS;
use super::names::strip_volume_suffix;

static KIND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix) \.vol(?P<pv>\d{1,9})[+-]\d{1,9}\.par2$ | (?P<pb>\.par2)$
  | \.part(?P<rp>\d{1,6})\.rar$ | (?P<rar>\.rar|\.cbr)$ | \.r(?P<rr>\d{2,3})$ | \.s(?P<rs>\d{2})$
  | \.7z\.(?P<sv>\d{3})$ | (?P<sz>\.7z)$ | \.z(?P<zv>\d{2,3})$ | (?P<zip>\.zipx?|\.cbz)$ | \.(?P<nv>\d{2,3})$
  | (?P<nfo>\.nfo|\.txt)$ | (?P<img>\.jpe?g|\.png|\.gif|\.webp|\.bmp|\.tiff?)$
  | (?P<not>\.mkv|\.mp4|\.avi|\.m4v|\.ts|\.wmv|\.mov|\.mpe?g|\.webm|\.mp3|\.flac|\.m4a|\.ogg|\.aac|\.wav|\.iso|\.img|\.bin|\.exe|\.msi|\.dmg|\.pdf|\.epub|\.mobi|\.azw3|\.sfv|\.srr|\.srs|\.nzb|\.url|\.diz|\.md5|\.sha1|\.idx|\.sub|\.srt|\.ass|\.tar|\.gz|\.tgz|\.bz2|\.xz|\.ace|\.cab)$",
    )
    .unwrap()
});

/// the sets a release is read for, at most
const MAX_SETS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Par2Base,
    Par2Vol(u32),
    Rar(u32),
    SevenZip(u32),
    Zip(u32),
    Numbered(u32),
    Nfo,
    Image,
    NotArchive,
    Unidentified,
}

static NUMBERED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.\d{2,3}$").unwrap());

pub fn classify(filename: &str) -> Kind {
    let Some(c) = KIND.captures(filename) else { return Kind::Unidentified };
    let num = |name: &str| c.name(name).and_then(|m| m.as_str().parse::<u32>().ok());
    if let Some(n) = num("pv") {
        Kind::Par2Vol(n)
    } else if c.name("pb").is_some() {
        Kind::Par2Base
    } else if let Some(n) = num("rp") {
        Kind::Rar(n)
    } else if c.name("rar").is_some() {
        Kind::Rar(0)
    } else if let Some(n) = num("rr") {
        Kind::Rar(n + 1)
    } else if let Some(n) = num("rs") {
        Kind::Rar(n + 1001)
    } else if let Some(n) = num("sv") {
        Kind::SevenZip(n)
    } else if c.name("sz").is_some() {
        Kind::SevenZip(0)
    } else if let Some(n) = num("zv") {
        Kind::Zip(n)
    } else if c.name("zip").is_some() {
        // a split ZIP's .zip is its last volume
        Kind::Zip(u32::MAX)
    } else if let Some(n) = num("nv") {
        Kind::Numbered(n)
    } else if c.name("nfo").is_some() {
        Kind::Nfo
    } else if c.name("img").is_some() {
        Kind::Image
    } else {
        Kind::NotArchive
    }
}

/// a family and a name without its volume suffix, and that set's files by volume
type VolumeSet<'a> = ((u8, String), Vec<(u32, &'a FileInfo)>);

/// An article to read first, and where its set ends.
#[derive(Clone, Copy, Debug)]
pub struct Head<'a> {
    pub file: &'a FileInfo,
    pub tail: &'a FileInfo,
    pub probe: bool,
}

fn parts(f: &FileInfo) -> i64 {
    f.expected.unwrap_or(0).max(f.parts_seen)
}

/// What to read, first choice first: named archive sets, or with none, files with no name to go by.
pub fn plan(files: &[FileInfo], probe_files: usize) -> Vec<Head<'_>> {
    // (family, stem) -> files with their volume numbers
    let mut sets: BTreeMap<(u8, String), Vec<(u32, &FileInfo)>> = BTreeMap::new();
    for f in files {
        let (family, volume) = match classify(&f.filename) {
            Kind::Rar(n) => (0, n),
            Kind::SevenZip(n) => (1, n),
            Kind::Zip(n) => (2, n),
            Kind::Numbered(n) => (3, n),
            _ => continue,
        };
        // `.01`, as `classify` reads it. Only here: elsewhere two digits at the end of a name are part of the name
        let set = if family == 3 { NUMBERED.replace(&f.filename, "") } else { strip_volume_suffix(&f.filename).into() };
        sets.entry((family, set.to_lowercase())).or_default().push((volume, f));
    }
    let mut sets: Vec<VolumeSet<'_>> = sets.into_iter().collect();
    sets.sort_by_key(|(key, members)| {
        (std::cmp::Reverse(members.iter().map(|(_, f)| parts(f)).sum::<i64>()), key.1.clone())
    });

    let mut heads = Vec::new();
    for ((family, _), mut members) in sets.into_iter().take(MAX_SETS) {
        members.sort_by(|a, b| (a.0, &a.1.filename).cmp(&(b.0, &b.1.filename)));
        let Some(tail) = members.last().map(|(_, f)| *f) else { continue };
        // every RAR volume has its own headers, soo a first without its first article falls to the next
        let take = if family == 0 { 2 } else { 1 };
        heads.extend(members.iter().take(take).map(|(_, f)| Head { file: f, tail, probe: false }));
    }
    if !heads.is_empty() {
        return heads;
    }

    // no name to go by: largest first, and not where most files are images
    let images = files.iter().filter(|f| classify(&f.filename) == Kind::Image).count();
    if images * 2 >= files.len() {
        return Vec::new();
    }
    let mut unknown: Vec<&FileInfo> =
        files.iter().filter(|f| classify(&f.filename) == Kind::Unidentified && parts(f) >= PROBE_MIN_PARTS).collect();
    unknown.sort_by_key(|f| (std::cmp::Reverse(parts(f)), f.filename.clone()));
    unknown.into_iter().take(probe_files).map(|f| Head { file: f, tail: f, probe: true }).collect()
}

/// Which PAR2 file to read: a base file, fewest parts first, or failing that the
/// volume with the lowest first block.
pub fn par2_source(files: &[FileInfo]) -> Option<&FileInfo> {
    let base =
        files.iter().filter(|f| classify(&f.filename) == Kind::Par2Base).min_by_key(|f| (parts(f), f.filename.clone()));
    base.or_else(|| {
        files
            .iter()
            .filter_map(|f| match classify(&f.filename) {
                Kind::Par2Vol(n) => Some((n, f)),
                _ => None,
            })
            .min_by_key(|(n, f)| (*n, parts(f), f.filename.clone()))
            .map(|(_, f)| f)
    })
}

/// NFOs to read: at most 2, fewest parts first.
pub fn nfo_sources(files: &[FileInfo]) -> Vec<&FileInfo> {
    let mut found: Vec<&FileInfo> = files
        .iter()
        .filter(|f| classify(&f.filename) == Kind::Nfo || f.subject.as_deref().is_some_and(nfo::is_nfo))
        .collect();
    found.sort_by_key(|f| (parts(f), f.filename.clone()));
    found.truncate(2);
    found
}
