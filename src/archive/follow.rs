//! Following NZBs down. An NZB whose post is small is read whole: an NZB posted in it, or one inside an archive it
//! holds (RAR, 7z, ZIP, tar, a compressed stream, anything `probe::nest` unwraps), is the next hop, and so on, up to
//! `MAX_HOPS`. What each hop lists is kept, and the names inside the archives read on the way, where a poster often
//! leaves the `{{password}}` of what the last one points to.

use std::collections::BTreeMap;

use crate::postprocess::step::{BodyRequest, Got, StepCtx, StepError};
use crate::probe::nest;

use super::names::strip_volume_suffix;
use super::nzb::{self, NzbFile};
use super::select::{self, Kind};
use super::{ARTICLE_CAP, InnerFile};

/// NZBs followed at most from the first
pub const MAX_HOPS: usize = 5;
/// A post this small is read whole to see what it holds: about what reading its first articles would cost anyway.
pub const MAX_POST_BYTES: u64 = 64 << 20;
/// and in no more articles than this
pub const MAX_POST_ARTICLES: usize = 128;
/// sets of files of one post read at most, looking for the next NZB
const MAX_SETS: usize = 3;

/// A file to read whole: its name, and its articles' message-ids with brackets, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Whole {
    pub name: String,
    pub ids: Vec<String>,
}

impl Whole {
    fn of(file: &NzbFile) -> Whole {
        let mut segments: Vec<_> = file.segments.iter().collect();
        segments.sort_by_key(|s| s.number);
        segments.dedup_by_key(|s| s.number);
        Whole { name: file.name.clone(), ids: segments.iter().map(|s| format!("<{}>", s.message_id)).collect() }
    }
}

/// Whether a post of these files is small enough to read whole.
pub fn small(files: &[NzbFile]) -> bool {
    let bytes = files.iter().map(NzbFile::bytes).fold(0, u64::saturating_add);
    let articles: usize = files.iter().map(|f| f.segments.len()).sum();
    bytes <= MAX_POST_BYTES && articles <= MAX_POST_ARTICLES
}

/// Every hop's files from the first NZB's bytes, and the names inside the archives read past it. Stops at a post too
/// big to read whole, one with no NZB in it, an article gone, a budget spent, or `MAX_HOPS`.
pub fn follow(ctx: &mut StepCtx<'_>, first: Vec<u8>) -> Result<Vec<InnerFile>, StepError> {
    let mut rows = Vec::new();
    let mut body = first;
    for _ in 0..MAX_HOPS {
        let parsed = nzb::parse(&body);
        rows.extend(parsed.files.iter().map(|f| InnerFile { name: f.name.clone(), size: Some(f.bytes()) }));
        // a cut NZB doesn't say what all of its post is
        if !parsed.complete || !small(&parsed.files) {
            break;
        }
        let wholes: Vec<Whole> = parsed.files.iter().map(Whole::of).collect();
        match nzb_inside(ctx, None, &wholes, Some(&mut rows), None)? {
            Some(next) => body = next,
            None => break,
        }
    }
    Ok(rows)
}

/// The next NZB in these files: one posted as a file, or one an archive set of them holds, read whole. `first` is a
/// set to try before the rest, as a release's own archive known by its bytes and not its name; `names` takes the names
/// inside each archive read; `known` is an article already read, by its message-id, not fetched again. All the sets
/// read share `MAX_POST_BYTES`, fetched and unpacked, whatever the NZB claimed.
pub fn nzb_inside(
    ctx: &mut StepCtx<'_>,
    first: Option<Set>,
    files: &[Whole],
    mut names: Option<&mut Vec<InnerFile>>,
    known: Option<(&str, &[u8])>,
) -> Result<Option<Vec<u8>>, StepError> {
    let mut left = MAX_POST_BYTES;
    for set in first.into_iter().chain(sets(files)) {
        let Some(raw) = download_set(ctx, &set, known, &mut left)? else { continue };
        if raw.len() == 1 && nzb::is_nzb(&raw[0]) {
            return Ok(raw.into_iter().next());
        }
        // RAR volumes carry their own headers, each unwrapped; other split archives are one stream cut in pieces
        let pieces: Vec<Vec<u8>> = if set.headed_volumes { raw } else { vec![raw.concat()] };
        let mut joined = Vec::new();
        let mut found = None;
        for piece in &pieces {
            let unwrapped = nest::unwrap_for(piece, &|name| select::classify(name) == Kind::Nzb);
            if let Some(names) = names.as_deref_mut() {
                for layer in &unwrapped.layers {
                    if let Some(name) = layer.name.clone().filter(|n| !names.iter().any(|f| &f.name == n)) {
                        names.push(InnerFile { name, size: layer.unpacked });
                    }
                }
            }
            if unwrapped.layers.iter().any(|l| l.encrypted) {
                break;
            }
            if unwrapped.inner.len() as u64 > left {
                break;
            }
            left -= unwrapped.inner.len() as u64;
            joined.extend_from_slice(&unwrapped.inner);
            // stored beside a bigger file, or in a solid stream with others: its start is in the bytes all the same
            found = found.or_else(|| nzb_start(piece).map(|at| piece[at..].to_vec()));
        }
        if nzb::is_nzb(&joined) {
            return Ok(Some(joined));
        }
        if let Some(at) = nzb_start(&joined) {
            return Ok(Some(joined[at..].to_vec()));
        }
        if found.is_some() {
            return Ok(found);
        }
    }
    Ok(None)
}

/// Files to read whole together, in order. `headed_volumes`: each is a RAR volume with its own headers, unwrapped
/// on its own; otherwise they're one stream cut in pieces, joined before unwrapping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Set {
    pub files: Vec<Whole>,
    pub headed_volumes: bool,
}

/// Where an NZB starts in these bytes, if one does.
fn nzb_start(bytes: &[u8]) -> Option<usize> {
    bytes.iter().enumerate().filter(|(_, b)| **b == b'<').map(|(at, _)| at).find(|at| nzb::is_nzb(&bytes[*at..]))
}

/// What to read for an NZB, first choice first: each NZB on its own, then archive sets with their volumes in order,
/// then single files a stream compressor made. At most `MAX_SETS`.
fn sets(files: &[Whole]) -> Vec<Set> {
    let single = |f: &Whole| Set { files: vec![f.clone()], headed_volumes: false };
    let mut out: Vec<Set> = files.iter().filter(|f| select::classify(&f.name) == Kind::Nzb).map(single).collect();
    // (family, set name) -> (volume, file)
    let mut archives: BTreeMap<(u8, String), Vec<(u32, &Whole)>> = BTreeMap::new();
    for f in files {
        let (family, volume) = match select::classify(&f.name) {
            Kind::Rar(n) => (0, n),
            Kind::SevenZip(n) => (1, n),
            Kind::Zip(n) => (2, n),
            Kind::Numbered(n) => (3, n),
            _ => continue,
        };
        archives.entry((family, strip_volume_suffix(&f.name).to_lowercase())).or_default().push((volume, f));
    }
    for ((family, _), mut volumes) in archives {
        volumes.sort_by(|a, b| (a.0, &a.1.name).cmp(&(b.0, &b.1.name)));
        out.push(Set { files: volumes.into_iter().map(|(_, f)| f.clone()).collect(), headed_volumes: family == 0 });
    }
    out.extend(files.iter().filter(|f| compressed(&f.name)).map(single));
    out.truncate(MAX_SETS);
    out
}

/// a file a stream compressor or tar made, by its name
fn compressed(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".tar", ".gz", ".tgz", ".xz", ".txz", ".lzma", ".bz2", ".tbz2", ".zst", ".lz4"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Every file of a set, each its articles decoded and joined, within what's `left` of the bytes. None: an article is
/// gone or refused, the bytes ran out, or a budget did, which ends the following without failing the step.
fn download_set(
    ctx: &mut StepCtx<'_>,
    set: &Set,
    known: Option<(&str, &[u8])>,
    left: &mut u64,
) -> Result<Option<Vec<Vec<u8>>>, StepError> {
    let mut files = Vec::with_capacity(set.files.len());
    for file in &set.files {
        let mut whole = Vec::new();
        for id in &file.ids {
            let body = match known.filter(|(known, _)| known == id) {
                Some((_, body)) => body.to_vec(),
                None => {
                    let wanted =
                        [BodyRequest { message_id: id.clone(), file: file.name.clone(), max_bytes: Some(ARTICLE_CAP) }];
                    let got = match ctx.fetch_bodies(&wanted) {
                        Ok(mut got) => got.pop().map(|b| b.got),
                        Err(StepError::Stopped) => return Err(StepError::Stopped),
                        Err(_) => return Ok(None),
                    };
                    let Some(Got::Body(body)) = got else { return Ok(None) };
                    body
                }
            };
            if body.len() as u64 > *left {
                return Ok(None);
            }
            *left -= body.len() as u64;
            whole.extend(body);
        }
        files.push(whole);
    }
    Ok(Some(files))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whole(name: &str) -> Whole {
        Whole { name: name.into(), ids: vec![format!("<{name}@x>")] }
    }

    #[test]
    fn nzbs_first_then_archive_sets_in_volume_order_then_streams() {
        let files = [
            whole("a.part02.rar"),
            whole("a.part01.rar"),
            whole("a.par2"),
            whole("a.nfo"),
            whole("b.tar.gz"),
            whole("inner-{{pw}}.nzb"),
        ];
        let found: Vec<(Vec<String>, bool)> = sets(&files)
            .into_iter()
            .map(|s| (s.files.into_iter().map(|f| f.name).collect(), s.headed_volumes))
            .collect();
        let set = |names: &[&str], headed| (names.iter().map(|n| n.to_string()).collect::<Vec<_>>(), headed);
        assert_eq!(
            found,
            [
                set(&["inner-{{pw}}.nzb"], false),
                set(&["a.part01.rar", "a.part02.rar"], true),
                set(&["b.tar.gz"], false)
            ]
        );
        let split = [whole("c.7z.002"), whole("c.7z.001")];
        assert_eq!(sets(&split)[0], Set { files: vec![whole("c.7z.001"), whole("c.7z.002")], headed_volumes: false });
    }

    #[test]
    fn a_post_is_small_by_its_bytes_and_articles() {
        let file = |segments: usize, bytes: u64| NzbFile {
            name: "x.rar".into(),
            segments: (1..=segments)
                .map(|n| nzb::Segment { number: n as i64, bytes, message_id: format!("{n}@x") })
                .collect(),
        };
        assert!(small(&[file(26, 640_000), file(14, 640_000)]));
        assert!(!small(&[file(100, 700_000)]), "70 MB");
        assert!(!small(&[file(MAX_POST_ARTICLES + 1, 10)]), "too many articles");
    }

    #[test]
    fn a_file_is_read_by_its_segments_in_number_order_once_each() {
        let file = NzbFile {
            name: "x.rar".into(),
            segments: [(2, "b"), (1, "a"), (2, "b2")]
                .into_iter()
                .map(|(n, id)| nzb::Segment { number: n, bytes: 1, message_id: id.into() })
                .collect(),
        };
        assert_eq!(Whole::of(&file).ids, ["<a>", "<b>"]);
    }
}
