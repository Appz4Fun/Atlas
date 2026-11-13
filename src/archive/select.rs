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
  | (?P<nfo>\.nfo|\.txt)$ | (?P<nzb>\.nzb)$ | (?P<img>\.jpe?g|\.png|\.gif|\.webp|\.bmp|\.tiff?)$
  | (?P<not>\.mkv|\.mp4|\.avi|\.m4v|\.ts|\.wmv|\.mov|\.mpe?g|\.webm|\.mp3|\.flac|\.m4a|\.ogg|\.aac|\.wav|\.iso|\.img|\.bin|\.exe|\.msi|\.dmg|\.pdf|\.epub|\.mobi|\.azw3|\.sfv|\.srr|\.srs|\.url|\.diz|\.md5|\.sha1|\.idx|\.sub|\.srt|\.ass|\.tar|\.gz|\.tgz|\.bz2|\.xz|\.ace|\.cab)$",
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
    Nzb,
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
    } else if c.name("nzb").is_some() {
        Kind::Nzb
    } else if c.name("img").is_some() {
        Kind::Image
    } else {
        Kind::NotArchive
    }
}

/// A volume's set: its name without what makes it that volume. `numbered`: a `.01`/`.001` volume, as `classify`
/// reads one; only there are two digits at the end of a name a volume number.
pub fn set_name(filename: &str, numbered: bool) -> String {
    if numbered { NUMBERED.replace(filename, "").into_owned() } else { strip_volume_suffix(filename).to_string() }
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
        let set = set_name(&f.filename, family == 3);
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

/// The volumes of an upload linked from its headers (`xref`), whose names say nothing: the files of the part count most
/// of them share (the larger on a tie), of at least `PROBE_MIN_PARTS`, in name order, which is the order they went up.
pub fn volumes(files: &[FileInfo]) -> Vec<&FileInfo> {
    let mut counts: BTreeMap<i64, usize> = BTreeMap::new();
    for f in files.iter().filter(|f| parts(f) >= PROBE_MIN_PARTS) {
        *counts.entry(parts(f)).or_default() += 1;
    }
    let Some((&size, _)) = counts.iter().max_by_key(|&(&p, &n)| (n, p)) else { return Vec::new() };
    let mut volumes: Vec<&FileInfo> = files.iter().filter(|f| parts(f) == size).collect();
    volumes.sort_by(|a, b| a.filename.cmp(&b.filename));
    volumes
}

/// Which PAR2 file to read: the first of `par2_sources`.
pub fn par2_source(files: &[FileInfo]) -> Option<&FileInfo> {
    par2_sources(files).into_iter().next()
}

/// A PAR2 file's set as its name says it: the name without `.volN+M.par2` or `.par2`, lowercased.
pub fn par2_stem(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let cut = lower.rfind(".vol").filter(|_| matches!(classify(name), Kind::Par2Vol(_))).or(lower.rfind(".par2"));
    cut.map(|at| lower[..at].to_string()).unwrap_or(lower)
}

/// The PAR2 files a set can be read from, best first: a volume carries the set's file list as a base file does.
/// Whole ones before ones missing articles, the release's own set before another (a sample's, or a set of fewer
/// files), a base before a volume, and among those the fewest parts or the lowest first block.
pub fn par2_sources(files: &[FileInfo]) -> Vec<&FileInfo> {
    let stem = par2_stem;
    let par2: Vec<(&FileInfo, Option<u32>)> = files
        .iter()
        .filter_map(|f| match classify(&f.filename) {
            Kind::Par2Base => Some((f, None)),
            Kind::Par2Vol(n) => Some((f, Some(n))),
            _ => None,
        })
        .collect();
    let mut stems: BTreeMap<String, usize> = BTreeMap::new();
    for (f, _) in &par2 {
        *stems.entry(stem(&f.filename)).or_default() += 1;
    }
    let main =
        stems.iter().max_by_key(|(name, n)| (**n, std::cmp::Reverse((*name).clone()))).map(|(name, _)| name.clone());
    let mut ranked: Vec<_> = par2
        .into_iter()
        .map(|(f, vol)| {
            let missing = f.expected.is_none_or(|e| f.parts_seen < e);
            let own = main.as_deref() == Some(stem(&f.filename).as_str()) && !stem(&f.filename).contains("sample");
            ((missing, !own, vol.is_some(), vol.unwrap_or(0), parts(f), f.filename.clone()), f)
        })
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0));
    ranked.into_iter().map(|(_, f)| f).collect()
}

/// All filenames stored in `par2_files` for `release_id` (and any upload linked via `upload_link`).
pub fn par2_file_names(conn: &rusqlite::Connection, release_id: i64) -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(mut stmt) = conn.prepare_cached("select name from par2_files where release_id = ?1")
        && let Ok(rows) = stmt.query_map([release_id], |r| r.get::<_, String>(0))
    {
        for name in rows.flatten() {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    if let Ok(mut stmt) = conn.prepare_cached(
        "select pf.name
         from upload_link ul
         join upload_link rec on rec.lead_id = ul.lead_id
         join par2_files pf on pf.release_id = rec.release_id
         where ul.release_id = ?1",
    ) && let Ok(rows) = stmt.query_map([release_id], |r| r.get::<_, String>(0))
    {
        for name in rows.flatten() {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

fn file_fits_par2(parts: i64, wire_bytes: u64, pf_size: u64) -> bool {
    if parts <= 0 {
        return false;
    }
    if parts == 1 {
        if wire_bytes > 0 {
            wire_bytes >= pf_size && wire_bytes <= pf_size.saturating_mul(108) / 100 + 4096
        } else {
            pf_size <= super::ARTICLE_CAP as u64
        }
    } else if wire_bytes > 0 {
        super::link::parts_fit(pf_size, parts, wire_bytes as i64, parts)
    } else {
        pf_size > super::ARTICLE_CAP as u64 / 2
    }
}

/// Map from posted `FileInfo.filename` to `par2_files.name` for a release, derived from `upload_link` and `par2_files`.
pub fn par2_name_map(
    conn: &rusqlite::Connection,
    release_id: i64,
    files: &[FileInfo],
) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    if let Ok(mut stmt) = conn.prepare_cached(
        "select f.filename, pf.name
         from upload_link ul
         join files f on f.release_id = ul.release_id
         join par2_files pf on pf.file_id = ul.par2_file
         where ul.release_id = ?1 and ul.par2_file is not null",
    ) && let Ok(rows) = stmt.query_map([release_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
    {
        for (posted, par2_name) in rows.flatten() {
            out.insert(posted, par2_name);
        }
    }
    let Ok(mut stmt) = conn.prepare_cached("select name, size from par2_files where release_id = ?1") else {
        return out;
    };
    let Ok(rows) = stmt.query_map([release_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))) else {
        return out;
    };
    let (unmatched_files,unmatched_par2)=prepare_par2_names(&mut out,files,rows.flatten().collect());
    if unmatched_files.is_empty() || unmatched_par2.is_empty() {return out;}
    let mut wire_sizes: std::collections::HashMap<i64, u64> = std::collections::HashMap::new();
    if let Ok(mut wstmt) = conn.prepare_cached(
        "select id, coalesce((select sum(bytes) from segments where file_id = files.id), 0), blob
         from files where release_id = ?1",
    ) && let Ok(wrows) = wstmt.query_map([release_id], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<Vec<u8>>>(2)?))
    }) {
        for (fid, seg_bytes, blob) in wrows.flatten() {
            let bytes = if seg_bytes > 0 {
                seg_bytes as u64
            } else if let Some(b) = blob.filter(|b| !crate::blob::is_remote(b)) {
                crate::blob::decode(&b)
                    .map(|segs| segs.iter().map(|s| s.bytes.max(0) as u64).sum())
                    .unwrap_or(0)
            } else {
                0
            };
            wire_sizes.insert(fid, bytes);
        }
    }
    finish_par2_names(out,unmatched_files,unmatched_par2,&wire_sizes)
}

/// Posted NZBs to read: at most 2, fewest parts first, and only one whose every article is indexed and few enough
/// to take whole.
pub fn nzb_sources(files: &[FileInfo]) -> Vec<&FileInfo> {
    nzb_sources_with_par2(files, &std::collections::HashMap::new())
}

/// Like [`nzb_sources`], also matching files whose PAR2-verified name ends in `.nzb`.
pub fn nzb_sources_with_par2<'a>(
    files: &'a [FileInfo],
    par2_names: &std::collections::HashMap<String, String>,
) -> Vec<&'a FileInfo> {
    let mut found: Vec<&FileInfo> = files
        .iter()
        .filter(|f| {
            classify(&f.filename) == Kind::Nzb
                || par2_names.get(&f.filename).is_some_and(|p| classify(p) == Kind::Nzb)
        })
        .filter(|f| (1..=super::nzb::MAX_NZB_PARTS).contains(&parts(f)) && f.parts_seen >= parts(f))
        .collect();
    found.sort_by_key(|f| (parts(f), f.filename.clone()));
    found.truncate(2);
    found
}

/// NFOs to read: at most 2, fewest parts first.
pub fn nfo_sources(files: &[FileInfo]) -> Vec<&FileInfo> {
    nfo_sources_with_par2(files, &std::collections::HashMap::new())
}

/// Like [`nfo_sources`], also matching files whose PAR2-verified name is `.nfo` or `.txt`.
pub fn nfo_sources_with_par2<'a>(
    files: &'a [FileInfo],
    par2_names: &std::collections::HashMap<String, String>,
) -> Vec<&'a FileInfo> {
    let mut found: Vec<&FileInfo> = files
        .iter()
        .filter(|f| {
            classify(&f.filename) == Kind::Nfo
                || f.subject.as_deref().is_some_and(nfo::is_nfo)
                || par2_names.get(&f.filename).is_some_and(|p| classify(p) == Kind::Nfo)
        })
        .collect();
    found.sort_by_key(|f| (parts(f), f.filename.clone()));
    found.truncate(2);
    found
}

/// Storage-pure initial case/name matching, shared by native reads.
pub(crate) fn prepare_par2_names<'a>(out:&mut std::collections::HashMap<String,String>,files:&'a [FileInfo],rows:Vec<(String,u64)>)->(Vec<&'a FileInfo>,Vec<(String,u64)>) {
    let is_par2 = |name: &str| matches!(classify(name), Kind::Par2Base | Kind::Par2Vol(_));
    let mut unmatched_par2: Vec<(String, u64)> =
        rows.into_iter().filter(|(name, _)| !is_par2(name)).collect();

    let mut unmatched_files: Vec<&FileInfo> = files
        .iter()
        .filter(|f| !is_par2(&f.filename) && !out.contains_key(&f.filename))
        .collect();
    unmatched_files.retain(|f| {
        if let Some(pos) = unmatched_par2.iter().position(|(name, _)| name.eq_ignore_ascii_case(&f.filename)) {
            let (par2_name, _) = unmatched_par2.remove(pos);
            if f.filename != par2_name {
                out.insert(f.filename.clone(), par2_name);
            }
            false
        } else {
            true
        }
    });

    (unmatched_files,unmatched_par2)
}

/// Storage-pure iterative wire-size matcher, unchanged from the legacy path.
pub(crate) fn finish_par2_names(mut out:std::collections::HashMap<String,String>,mut unmatched_files:Vec<&FileInfo>,mut unmatched_par2:Vec<(String,u64)>,wire_sizes:&std::collections::HashMap<i64,u64>)->std::collections::HashMap<String,String> {
    let wire_of = |f: &FileInfo| wire_sizes.get(&f.id).copied().unwrap_or(0);
    loop {
        let mut progressed = false;
        let mut p_idx = 0;
        while p_idx < unmatched_par2.len() {
            let (_, pf_size) = unmatched_par2[p_idx];
            let mut fit_idx = None;
            let mut fits = 0usize;
            for (fi, f) in unmatched_files.iter().enumerate() {
                if file_fits_par2(parts(f), wire_of(f), pf_size) {
                    fit_idx = Some(fi);
                    fits += 1;
                }
            }
            if fits == 1
                && let Some(fi) = fit_idx
            {
                let f = unmatched_files.remove(fi);
                let (par2_name, _) = unmatched_par2.remove(p_idx);
                out.insert(f.filename.clone(), par2_name);
                progressed = true;
            } else {
                p_idx += 1;
            }
        }
        let mut f_idx = 0;
        while f_idx < unmatched_files.len() {
            let f = unmatched_files[f_idx];
            let f_parts = parts(f);
            let f_wire = wire_of(f);
            let mut fit_idx = None;
            let mut fits = 0usize;
            for (pi, (_, pf_size)) in unmatched_par2.iter().enumerate() {
                if file_fits_par2(f_parts, f_wire, *pf_size) {
                    fit_idx = Some(pi);
                    fits += 1;
                }
            }
            if fits == 1
                && let Some(pi) = fit_idx
            {
                let f = unmatched_files.remove(f_idx);
                let (par2_name, _) = unmatched_par2.remove(pi);
                out.insert(f.filename.clone(), par2_name);
                progressed = true;
            } else {
                f_idx += 1;
            }
        }
        if !progressed {
            break;
        }
    }
    if !unmatched_files.is_empty() && unmatched_files.len() == unmatched_par2.len() {
        unmatched_files.sort_by_key(|f| (parts(f), wire_of(f), f.filename.clone()));
        unmatched_par2.sort_by_key(|(name, size)| (*size, name.clone()));
        if unmatched_files
            .iter()
            .zip(unmatched_par2.iter())
            .all(|(f, (_, pf_size))| file_fits_par2(parts(f), wire_of(f), *pf_size))
        {
            for (f, (par2_name, _)) in unmatched_files.into_iter().zip(unmatched_par2) {
                out.insert(f.filename.clone(), par2_name);
            }
        }
    }
    out
}
