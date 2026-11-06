//! Following NZBs down. An NZB whose post is small is read whole: an NZB posted in it, or one inside an archive it
//! holds (RAR, 7z, ZIP, tar, a compressed stream, anything `probe::nest` unwraps), is the next hop, and so on, up to
//! `MAX_HOPS`. What each hop lists is kept, and the names inside the archives read on the way, where a poster often
//! leaves the `{{password}}` of what the last one points to.

use std::collections::BTreeMap;

use crate::postprocess::step::{BodyRequest, Got, StepCtx, StepError};
use crate::probe::nest;

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
/// An NZB found on the way and bigger than this isn't parsed: a real one this size lists a post of hundreds of GB,
/// and reading it as text can take a few times its size. The one in the owner's first chain is 16.5 MB.
pub const MAX_FOLLOWED_NZB: usize = 24 << 20;

/// What a whole following may fetch and unpack: bytes, and articles, the latter scaled from
/// `archive.max_bodies_per_release`, soo an operator who lowers that lowers this too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub bytes: u64,
    pub articles: usize,
}

impl Budget {
    /// `MAX_POST_BYTES`, and 8 articles for each body an archive listing may read: 128 at its default 16
    pub fn of(max_bodies_per_release: i64) -> Budget {
        let articles = usize::try_from(max_bodies_per_release.max(1)).unwrap_or(1).saturating_mul(8);
        Budget { bytes: MAX_POST_BYTES, articles: articles.min(MAX_POST_ARTICLES) }
    }
}

/// A file to read whole: its name, and its articles' message-ids with brackets, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Whole {
    pub name: String,
    pub ids: Vec<String>,
    /// the release's own file, in its group: read without asking a server which groups its articles are in first
    pub trusted: bool,
}

impl Whole {
    /// None: a file that may not or can't be read: posted to no group Atlas may fetch from (none named, or every one
    /// blocked, `filters::group_blocked`), or with a segment whose message-id isn't one (`valid_id`).
    fn of(file: &NzbFile) -> Option<Whole> {
        // nothing to read, so no group of it is looked at: a crafted NZB of empty files names groups by the thousand
        if file.segments.is_empty() {
            return None;
        }
        if !file.groups.iter().any(|g| !crate::filters::group_blocked(g)) {
            return None;
        }
        let mut segments: Vec<_> = file.segments.iter().collect();
        if !segments.iter().all(|s| valid_id(&s.message_id)) {
            return None;
        }
        segments.sort_by_key(|s| s.number);
        segments.dedup_by_key(|s| s.number);
        let ids = segments.iter().map(|s| format!("<{}>", s.message_id)).collect();
        Some(Whole { name: file.name.clone(), ids, trusted: false })
    }
}

/// A message-id as RFC 5536 has them, without its brackets: `left@right`, both sides there, 3 to 248 printable
/// ASCII characters in all, no space, no `<` or `>`. Anything else would be a broken command, not a lookup.
fn valid_id(id: &str) -> bool {
    (3..=248).contains(&id.len())
        && id.bytes().all(|b| (0x21..=0x7e).contains(&b) && b != b'<' && b != b'>')
        && id
            .rsplit_once('@')
            .is_some_and(|(left, right)| !left.is_empty() && !right.is_empty() && !right.contains('@'))
}

/// Whether a post of these files is small enough to read whole.
pub fn small(files: &[NzbFile]) -> bool {
    let bytes = files.iter().map(NzbFile::bytes).fold(0, u64::saturating_add);
    let articles: usize = files.iter().map(|f| f.articles).sum();
    bytes <= MAX_POST_BYTES && articles <= MAX_POST_ARTICLES && files.iter().all(|f| f.segments.len() == f.articles)
}

/// Every hop's files from the first NZB's bytes, the last hop's first, each after the names inside the archive it was
/// found in. Stops at a post too big to read whole, one with no NZB in it, an article gone, `MAX_HOPS`, or when
/// `left`, what the whole following may fetch and unpack, runs out.
pub fn follow(ctx: &mut StepCtx<'_>, first: Vec<u8>, left: &mut Budget) -> Result<Vec<InnerFile>, StepError> {
    follow_with_password(ctx, first, left).map(|(files, _)| files)
}

/// Like [`follow`], also returning any `<meta type="password">` or `{{password}}` found in the followed NZBs or
/// the archives unwrapped along the way (preferring the last hop's password).
pub fn follow_with_password(
    ctx: &mut StepCtx<'_>,
    first: Vec<u8>,
    left: &mut Budget,
) -> Result<(Vec<InnerFile>, Option<String>), StepError> {
    // each hop's rows: the names inside the archive its NZB came out of, then its files
    let mut hops: Vec<Vec<InnerFile>> = Vec::new();
    let mut passwords: Vec<Option<String>> = Vec::new();
    let mut inside: Vec<InnerFile> = Vec::new();
    let mut body = first;
    for hop in 1..=MAX_HOPS {
        if body.len() > MAX_FOLLOWED_NZB {
            break;
        }
        let meta_pw = nzb::meta_password(&body);
        let parsed = nzb::parse(&body);
        let mut rows = std::mem::take(&mut inside);
        rows.extend(parsed.files.iter().map(|f| InnerFile::new(f.name.clone(), Some(f.bytes()))));
        let name_pw = rows.iter().find_map(|f| crate::release_info::find_password(&f.name));
        passwords.push(meta_pw.or(name_pw));
        hops.push(rows);
        // a cut NZB doesn't say what all of its post is; and past the last hop nothing more would be listed
        if !parsed.complete || !small(&parsed.files) || hop == MAX_HOPS {
            break;
        }
        let wholes: Vec<Whole> = parsed.files.iter().filter_map(Whole::of).collect();
        match nzb_inside(ctx, None, &wholes, Some(&mut inside), None, left)? {
            Some(next) => body = next,
            None => break,
        }
    }
    if let Some(last) = hops.last_mut() {
        if let Some(pw) = inside.iter().find_map(|f| crate::release_info::find_password(&f.name)) {
            passwords.push(Some(pw));
        }
        last.append(&mut inside);
    }
    let pw = passwords.into_iter().rev().flatten().next();
    // where the chain ends first: what's kept and what a name is taken from favour it
    Ok((hops.into_iter().rev().flatten().collect(), pw))
}

/// The next NZB in these files: one posted as a file, or one an archive set of them holds, read whole. `first` is a
/// set to try before the rest, as a release's own archive known by its bytes and not its name; `names` takes the names
/// inside each archive read; `known` is an article already read, by its message-id, not fetched again. What's fetched
/// and unpacked comes out of `left`, whatever the NZB claimed.
pub fn nzb_inside(
    ctx: &mut StepCtx<'_>,
    first: Option<Set>,
    files: &[Whole],
    mut names: Option<&mut Vec<InnerFile>>,
    known: Option<(&str, &[u8])>,
    left: &mut Budget,
) -> Result<Option<Vec<u8>>, StepError> {
    for set in first.into_iter().chain(sets(files)) {
        let Some(raw) = download_set(ctx, &set, known, left)? else { continue };
        if raw.len() == 1 && nzb::is_nzb(&raw[0]) {
            return Ok(raw.into_iter().next());
        }
        // RAR volumes carry their own headers, each unwrapped; other split archives are one stream cut in pieces
        let pieces: Vec<Vec<u8>> = if set.headed_volumes { raw } else { vec![split_zip_marker_off(raw.concat())] };
        let mut joined = Vec::new();
        let mut found = None;
        for piece in &pieces {
            // as far as an NZB this hop may read goes, not a probe's look at the front
            // decoding is work the fetch deadline doesn't see: none starts past it
            if std::time::Instant::now() >= ctx.deadline() {
                return Err(StepError::OutOfBudget);
            }
            let limit = usize::try_from(left.bytes).unwrap_or(usize::MAX).min(MAX_FOLLOWED_NZB);
            let unwrapped = nest::unwrap_for(piece, &|name| select::classify(name) == Kind::Nzb, limit);
            // every byte decoded on the way counts, not only what's kept: a stream can expand to much and select little
            left.bytes = left.bytes.saturating_sub(unwrapped.decoded as u64);
            if let Some(names) = names.as_deref_mut() {
                for layer in &unwrapped.layers {
                    if let Some(name) = layer.name.clone().filter(|n| !names.iter().any(|f| &f.name == n)) {
                        names.push(InnerFile::new(name, layer.unpacked));
                    }
                }
            }
            if unwrapped.layers.iter().any(|l| l.encrypted) {
                break;
            }
            if unwrapped.inner.len() as u64 > left.bytes {
                break;
            }
            left.bytes -= unwrapped.inner.len() as u64;
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

/// A split zip's first volume opens with a spanning marker before its first local header, which nothing that reads a
/// zip from its front expects: off it comes.
fn split_zip_marker_off(mut stream: Vec<u8>) -> Vec<u8> {
    if (stream.starts_with(b"PK\x07\x08") || stream.starts_with(b"PK00")) && stream.get(4..8) == Some(b"PK\x03\x04") {
        stream.drain(..4);
    }
    stream
}

/// Where an NZB starts in these bytes, if one does, in one pass: at its first `<nzb`, or at an XML prolog or doctype
/// shortly before it (`nzb::is_nzb` wants the root within 4 KiB of the start).
fn nzb_start(bytes: &[u8]) -> Option<usize> {
    let root = bytes.windows(4).position(|w| w.eq_ignore_ascii_case(b"<nzb"))?;
    let window = root.saturating_sub(4096);
    let before = &bytes[window..root];
    let prolog = [&b"<?xml"[..], b"<!doctype nzb"]
        .iter()
        .filter_map(|p| before.windows(p.len()).position(|w| w.eq_ignore_ascii_case(p)))
        .min()
        .map(|at| window + at);
    Some(prolog.filter(|at| nzb::is_nzb(&bytes[*at..])).unwrap_or(root))
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
        archives.entry((family, select::set_name(&f.name, family == 3).to_lowercase())).or_default().push((volume, f));
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
    [".tar", ".gz", ".tgz", ".xz", ".txz", ".lzma", ".lz", ".bz2", ".tbz2", ".zst", ".lz4", ".z"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Every file of a set, each its articles decoded and joined, within what's `left`. None: an article is gone or
/// refused, or `left` ran out, which ends the following without failing the step. A server down, a run's budget or
/// deadline spent, or a shutdown is the step's error: the chain is tried again, not cut short for good.
fn download_set(
    ctx: &mut StepCtx<'_>,
    set: &Set,
    known: Option<(&str, &[u8])>,
    left: &mut Budget,
) -> Result<Option<Vec<Vec<u8>>>, StepError> {
    let mut files = Vec::with_capacity(set.files.len());
    for file in &set.files {
        let mut whole = Vec::new();
        for id in &file.ids {
            let body = match known.filter(|(known, _)| known == id) {
                Some((_, body)) => body.to_vec(),
                None => {
                    // a hop's article is two requests, its headers and its body: both counted, and both must fit what
                    // this run may still fetch, soo following ends where its limits say, never on a refusal midway
                    let requests = if file.trusted { 1 } else { 2 };
                    if left.articles < requests || ctx.bodies_left() < requests as i64 {
                        return Ok(None);
                    }
                    left.articles -= requests;
                    let wanted =
                        BodyRequest { message_id: id.clone(), file: file.name.clone(), max_bytes: Some(ARTICLE_CAP) };
                    let fetched = if file.trusted {
                        ctx.fetch_bodies(std::slice::from_ref(&wanted)).map(|mut got| got.pop().map(|b| b.got))
                    } else {
                        // its headers first: the groups an NZB names are the poster's word, the article's the server's
                        match ctx.fetch_head(&wanted) {
                            Ok(head) => match head.got {
                                Got::Body(head)
                                    if posted_where_allowed(&crate::probe::yenc::split_article(&head).headers) =>
                                {
                                    ctx.fetch_foreign_body(&wanted).map(|b| Some(b.got))
                                }
                                _ => return Ok(None),
                            },
                            Err(e) => Err(e),
                        }
                    };
                    let got = match fetched {
                        Ok(got) => got,
                        Err(e @ (StepError::Stopped | StepError::Transient(_) | StepError::OutOfBudget)) => {
                            return Err(e);
                        }
                        Err(_) => return Ok(None),
                    };
                    let Some(Got::Body(body)) = got else { return Ok(None) };
                    body
                }
            };
            if body.len() as u64 > left.bytes {
                return Ok(None);
            }
            left.bytes -= body.len() as u64;
            whole.extend(body);
        }
        files.push(whole);
    }
    Ok(Some(files))
}

/// Whether an article's own headers, `Newsgroups` or the server's `Xref`, name a group Atlas may fetch from
/// (`filters::group_blocked`). Neither header: not read.
fn posted_where_allowed(headers: &[(String, String)]) -> bool {
    let groups = headers.iter().flat_map(|(name, value)| {
        let listed: Vec<&str> = if name.eq_ignore_ascii_case("Newsgroups") {
            value.split(',').map(str::trim).collect()
        } else if name.eq_ignore_ascii_case("Xref") {
            // `server group:number group:number`
            value.split_whitespace().skip(1).filter_map(|g| g.split_once(':').map(|(group, _)| group)).collect()
        } else {
            Vec::new()
        };
        listed
    });
    groups.into_iter().any(|group| !group.is_empty() && !crate::filters::group_blocked(group))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whole(name: &str) -> Whole {
        Whole { name: name.into(), ids: vec![format!("<{name}@x>")], trusted: false }
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
        let numbered = [whole("d.02"), whole("d.01")];
        assert_eq!(sets(&numbered)[0], Set { files: vec![whole("d.01"), whole("d.02")], headed_volumes: false });
        let split = [whole("c.7z.002"), whole("c.7z.001")];
        assert_eq!(sets(&split)[0], Set { files: vec![whole("c.7z.001"), whole("c.7z.002")], headed_volumes: false });
    }

    #[test]
    fn an_article_is_read_only_from_a_group_it_says_it_is_in() {
        let h = |v: &str| vec![("Newsgroups".to_string(), v.to_string())];
        assert!(posted_where_allowed(&h("alt.binaries.test")));
        assert!(!posted_where_allowed(&[]), "no header");
        assert!(!posted_where_allowed(&h(" , ")));
        let xref = vec![("Xref".to_string(), "news alt.binaries.test:1 alt.binaries.other:2".to_string())];
        assert!(posted_where_allowed(&xref));
        assert!(!posted_where_allowed(&[("Xref".to_string(), "news".to_string())]));
        let blocked = "alt.binaries.cp";
        if crate::filters::group_blocked(blocked) {
            assert!(!posted_where_allowed(&h(blocked)));
            assert!(posted_where_allowed(&h("alt.binaries.cp, alt.binaries.test")));
        }
    }

    #[test]
    fn a_split_zips_spanning_marker_comes_off() {
        assert_eq!(split_zip_marker_off(b"PK\x07\x08PK\x03\x04rest".to_vec()), b"PK\x03\x04rest");
        assert_eq!(split_zip_marker_off(b"PK00PK\x03\x04rest".to_vec()), b"PK\x03\x04rest");
        assert_eq!(split_zip_marker_off(b"PK\x07\x08junk".to_vec()), b"PK\x07\x08junk");
    }

    #[test]
    fn every_stream_nest_unwraps_is_a_candidate_by_its_name() {
        for name in [
            "n.nzb.gz",
            "n.nzb.xz",
            "n.nzb.lzma",
            "n.nzb.lz",
            "n.nzb.bz2",
            "n.nzb.zst",
            "n.nzb.lz4",
            "n.nzb.Z",
            "n.tar",
        ] {
            assert!(compressed(name), "{name}");
        }
        assert!(!compressed("n.nfo"));
    }

    #[test]
    fn an_nzbs_start_is_found_in_one_pass() {
        assert_eq!(nzb_start(b"junk\x00\x01<?xml version=\"1.0\"?>\n<nzb>"), Some(6));
        assert_eq!(nzb_start(b"no prolog <NZB xmlns=\"x\">"), Some(10));
        assert_eq!(nzb_start(b"<?xml?><rss></rss>"), None);
        // a prolog too far before the root doesn't count
        let far = [&b"<?xml?>"[..], &[b' '; 5000], b"<nzb>"].concat();
        assert_eq!(nzb_start(&far), Some(5007));
        // repeated prologs with no root cost one scan
        let many = b"<?xml ".repeat(3_000_000);
        assert_eq!(nzb_start(&many), None);
    }

    #[test]
    fn a_post_is_small_by_its_bytes_and_articles() {
        let file = |segments: usize, bytes: u64| NzbFile {
            name: "x.rar".into(),
            segments: (1..=segments)
                .map(|n| nzb::Segment { number: n as i64, bytes, message_id: format!("{n}@x") })
                .collect(),
            articles: segments,
            total_bytes: segments as u64 * bytes,
            groups: vec![],
            ..NzbFile::default()
        };
        assert!(small(&[file(26, 640_000), file(14, 640_000)]));
        assert!(!small(&[file(100, 700_000)]), "70 MB");
        assert!(!small(&[file(MAX_POST_ARTICLES + 1, 10)]), "too many articles");
    }

    #[test]
    fn a_file_is_read_by_its_segments_in_number_order_once_each() {
        let file = NzbFile {
            name: "x.rar".into(),
            segments: [(2, "b@x"), (1, "a@x"), (2, "b2@x")]
                .into_iter()
                .map(|(n, id)| nzb::Segment { number: n, bytes: 1, message_id: id.into() })
                .collect(),
            articles: 3,
            total_bytes: 3,
            groups: vec!["alt.binaries.test".into()],
            ..NzbFile::default()
        };
        assert_eq!(Whole::of(&file).unwrap().ids, ["<a@x>", "<b@x>"]);
        let blocked = NzbFile { groups: vec!["alt.binaries.cp".into()], ..file.clone() };
        assert!(crate::filters::group_blocked("alt.binaries.cp") == Whole::of(&blocked).is_none());
        assert!(Whole::of(&NzbFile { groups: vec![], ..file.clone() }).is_none(), "no group named");
        let with_id = |id: &str| NzbFile {
            segments: vec![nzb::Segment { number: 1, bytes: 1, message_id: id.into() }],
            ..file.clone()
        };
        assert!(Whole::of(&with_id("")).is_none(), "an id dropped as overlong");
        assert!(Whole::of(&NzbFile { segments: vec![], ..with_id("a@b") }).is_none(), "no segments");
        assert!(Whole::of(&with_id("a\nb")).is_none(), "a line break");
        assert!(Whole::of(&with_id("a b")).is_none(), "a space");
        assert!(Whole::of(&with_id("a>b")).is_none(), "a bracket");
        assert!(Whole::of(&with_id("no-at-sign")).is_none(), "no @");
        assert!(Whole::of(&with_id("@b")).is_none() && Whole::of(&with_id("a@")).is_none(), "an empty side");
        assert!(Whole::of(&with_id("part1of26.kd8SVOZST6uf6W9pvM$&@powerpost.local")).is_some());
    }
}
