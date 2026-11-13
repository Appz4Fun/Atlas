//! Stills: a keyframe from a quarter, half and three quarters of the way into a release's video, decoded by
//! our own HEVC or H.264 decoder and kept as JPEGs. The articles are read from the middle of the file, a few each.
//! In Matroska a keyframe is found by its marks in the bytes read; in MP4 the `moov`'s sample tables say where each
//! one is, soo those are read first.

use sha2::Digest;

use crate::config::Config;
use crate::keys;
use crate::postprocess::schema::ShardTable;
use crate::postprocess::step::{Got, Needs, Outcome, Reruns, Step, StepCtx, StepError, StepId, StepMutation, StepWrite, mask};
use crate::store::{self, FileInfo, SegmentPick};
use crate::video::mkv::{self, Codec, Found, VideoTrack};
use crate::video::mp4;
use crate::video::{h264, hevc, jpeg};

use super::step::{fetch_raw, main_file};
use super::{ProbeInfo, archive, moviehash, yenc};
use crate::nntp::yenc_decode;

pub const STILLS_STEP: StepId = StepId(6);

/// Where in the video the stills are from, in percent.
pub const POSITIONS: [u8; 3] = [25, 50, 75];
/// Articles read for one still at most, after the first: a 2160p keyframe can take three.
const MORE_ARTICLES: usize = 4;
/// Articles of a file's start read for its track list.
const FRONT_ARTICLES: i64 = 3;
/// How wide a still is kept, at most.
const STILL_WIDTH: usize = 960;
const QUALITY: u8 = 85;

pub const STILLS: ShardTable = ShardTable {
    name: "release_stills",
    create: "create table if not exists release_stills (
        release_id INTEGER NOT NULL,
        position   INTEGER NOT NULL,
        at_ms      INTEGER,
        sha256     TEXT NOT NULL,
        width      INTEGER NOT NULL,
        height     INTEGER NOT NULL,
        PRIMARY KEY (release_id, position)
    )",
    indexes: &[],
    release_key: Some("release_id"),
    // a 64-bit perceptual hash, stored as its bits in an i64: "the same film" across releases
    added: &[("phash", "INTEGER")],
};

/// Past this, the oldest stills go. One is 50 to 150 KB.
const MAX_STILL_CACHE: u64 = 2 << 30;
const TRIM_EVERY: u64 = 64;

fn still_dir() -> std::path::PathBuf {
    crate::paths::app_dir().join("stills")
}

pub(crate) fn still_file(sha256: &str) -> Option<std::path::PathBuf> {
    (sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit())).then(|| still_dir().join(sha256))
}

/// Keeps a still, named by its hash.
fn keep(bytes: &[u8]) -> Option<String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    let sha256: String = sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
    let file = still_file(&sha256)?;
    if !file.exists() {
        static WRITTEN: AtomicU64 = AtomicU64::new(0);
        std::fs::create_dir_all(still_dir()).ok()?;
        crate::atomic::write_atomic(&file, bytes).ok()?;
        if WRITTEN.fetch_add(1, Ordering::Relaxed).is_multiple_of(TRIM_EVERY) {
            crate::media::details::trim_posters(&still_dir(), MAX_STILL_CACHE);
        }
    }
    Some(sha256)
}

/// A release's stills, in order: (position, time in ms, width, height).
pub fn stills(conn: &rusqlite::Connection, release_id: i64) -> Vec<(u8, Option<i64>, u32, u32)> {
    let s = store::shard_of_id(release_id);
    let sql = format!(
        "select position, at_ms, width, height from s{s}.release_stills where release_id = ? order by position"
    );
    let Ok(mut q) = conn.prepare(&sql) else { return Vec::new() };
    q.query_map([release_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// The keys that name the video file itself, whichever release carries it: the Matroska SegmentUID with the mux's
/// DateUTC, and the OpenSubtitles hash (the size with the first and last 64 KiB). Both come from the probed main
/// file, never a sample or an extra, soo a release that shares one has the same pictures at the same places.
const SAME_FILE: [keys::Kind; 2] = [keys::Kind::Mux, keys::Kind::MovieHash];

/// Releases with every still hashed looked at per key and shard, newest first, for one whose files are on disk.
const SAME_FILE_CANDIDATES: i64 = 16;

/// What a release's video file can be found by: its mux key and its OpenSubtitles hash, read from its probe in
/// `schema` (`main` on a shard opened alone, `sN` with the shards attached). Empty when it has no probe.
pub fn file_lookup(conn: &rusqlite::Connection, schema: &str, release_id: i64) -> keys::Lookup {
    let row: Option<(String, Option<String>)> = conn
        .prepare_cached(&format!("select info, moviehash from {schema}.release_probe where release_id = ?1"))
        .and_then(|mut q| q.query_row([release_id], |r| Ok((r.get(0)?, r.get(1)?))))
        .ok();
    let Some((info, moviehash)) = row else { return keys::Lookup::default() };
    let mux = serde_json::from_str::<ProbeInfo>(&info)
        .ok()
        .and_then(|p| p.media.as_ref().and_then(keys::mux_text))
        .map(|t| keys::as_key(keys::digest8(t.as_bytes())));
    keys::Lookup {
        keys: keys::Keys { mux, ..keys::Keys::default() },
        moviehash: moviehash.filter(|h| !h.is_empty()),
        ..keys::Lookup::default()
    }
}

/// Whether a release has every still, each hashed and still on disk, read with the shards attached to `conn`.
fn has_all_stills(conn: &rusqlite::Connection, release_id: i64) -> bool {
    let s = store::shard_of_id(release_id);
    let kept: Vec<(String, bool)> = conn
        .prepare_cached(&format!("select sha256, phash is not null from s{s}.release_stills where release_id = ?1"))
        .and_then(|mut q| q.query_map([release_id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
        .unwrap_or_default();
    kept.len() == POSITIONS.len() && kept.iter().all(|(h, hashed)| *hashed && still_file(h).is_some_and(|f| f.exists()))
}

/// The releases other than `?1` in shard `shard` whose probed file has the `kind` key `?3`, that the pages would show
/// and that have every still hashed, newest first, at most `?2`: those without stills are left out in SQL, soo any
/// number of releases that borrow can't hide the one they borrow from.
fn with_stills_sql(kind: keys::Kind, shard: usize) -> String {
    let (from, matches) = match kind {
        keys::Kind::Mux => (format!("s{shard}.release_keys k"), "k.mux = ?3"),
        _ => (format!("s{shard}.release_probe k"), "k.moviehash = ?3"),
    };
    format!(
        "select k.release_id from {from} join s{shard}.releases r on r.id = k.release_id
         where {matches} and k.release_id != ?1 and not atlas_blocked(r.group_name)
           and (select count(*) from s{shard}.release_stills s
                where s.release_id = k.release_id and s.phash is not null) = {}
         order by k.release_id desc limit ?2",
        POSITIONS.len()
    )
}

/// Another release carrying the same video file (`SAME_FILE`) that already has its stills, and the key that ties
/// them, over every shard of `conn`. The stills are read from it, by its id: none are made or kept twice.
pub fn same_file_stills(conn: &rusqlite::Connection, own: i64, lookup: &keys::Lookup) -> Option<(i64, keys::Kind)> {
    for kind in SAME_FILE {
        let value: Option<rusqlite::types::Value> = match kind {
            keys::Kind::Mux => lookup.keys.mux.map(Into::into),
            _ => lookup.moviehash.clone().map(Into::into),
        };
        let Some(value) = value else { continue };
        let mut found: Vec<i64> = Vec::new();
        for shard in 0..store::SHARDS {
            // a shard without the tables yet has none
            let Ok(mut q) = conn.prepare_cached(&with_stills_sql(kind, shard)) else { continue };
            let Ok(rows) = q.query_map(rusqlite::params![own, SAME_FILE_CANDIDATES, value], |r| r.get::<_, i64>(0))
            else {
                continue;
            };
            found.extend(rows.filter_map(Result::ok));
        }
        found.sort_unstable_by(|a, b| b.cmp(a));
        if let Some(other) = found.into_iter().find(|other| has_all_stills(conn, *other)) {
            return Some((other, kind));
        }
    }
    None
}

/// Whose stills a release shows, with the shards attached to `conn`: its own, else those of a release with the same
/// video file, with the key that ties them. None: no stills to show.
pub fn shown_stills(conn: &rusqlite::Connection, release_id: i64) -> Option<(i64, Option<keys::Kind>)> {
    if !stills(conn, release_id).is_empty() {
        return Some((release_id, None));
    }
    let lookup = file_lookup(conn, &format!("s{}", store::shard_of_id(release_id)), release_id);
    same_file_stills(conn, release_id, &lookup).map(|(other, kind)| (other, Some(kind)))
}

/// The most releases listed as the same film.
const MAX_SAME_FILM: usize = 50;

/// How far apart in the film two stills may be and still be compared: each encode puts its keyframes in its own
/// places, soo the one nearest a quarter of the way in is seconds off from another's.
const SAME_FILM_WINDOW_MS: i64 = 15_000;

/// Other releases whose stills show the same pictures as this one's: at least two of its stills each within
/// `MATCH_BITS` of one of theirs taken near the same time, newest first. Only ever "the same film" (another encode,
/// a remux, a cut), never a twin.
pub fn same_film(conn: &rusqlite::Connection, release_id: i64) -> Vec<i64> {
    let s = store::shard_of_id(release_id);
    let own: Vec<(u8, Option<i64>, u64)> = conn
        .prepare(&format!(
            "select position, at_ms, phash from s{s}.release_stills where release_id = ? and phash is not null"
        ))
        .and_then(|mut q| {
            q.query_map([release_id], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u64)))?.collect()
        })
        .unwrap_or_default();
    if own.len() < 2 {
        return Vec::new();
    }
    // per release: which of our stills found a match, one bit each
    let mut hits: std::collections::HashMap<i64, u8> = std::collections::HashMap::new();
    for shard in 0..store::SHARDS {
        let sql = format!(
            "select release_id, position, at_ms, phash from s{shard}.release_stills
             where phash is not null and release_id != ?"
        );
        // a shard that hasn't got the table yet has none
        let Ok(mut q) = conn.prepare(&sql) else { continue };
        let Ok(rows) = q.query_map([release_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, u8>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, i64>(3)?))
        }) else {
            continue;
        };
        for (other, position, at, hash) in rows.filter_map(Result::ok) {
            let matched = own.iter().enumerate().filter(|(_, mine)| still_matches(**mine, (position, at, hash as u64)));
            for (i, _) in matched {
                *hits.entry(other).or_default() |= 1 << i;
            }
        }
    }
    let mut found: Vec<i64> = hits.into_iter().filter(|&(_, bits)| bits.count_ones() >= 2).map(|(id, _)| id).collect();
    // ids grow with time, within a shard and across them closely enough
    found.sort_unstable_by(|a, b| b.cmp(a));
    found.truncate(MAX_SAME_FILM);
    found
}

/// Two stills, (position, time, hash), show the same picture: taken near the same time (or at the same position,
/// when a time is missing) and close in hash.
fn still_matches(a: (u8, Option<i64>, u64), b: (u8, Option<i64>, u64)) -> bool {
    let near = match (a.1, b.1) {
        (Some(x), Some(y)) => (x - y).abs() <= SAME_FILM_WINDOW_MS,
        _ => a.0 == b.0,
    };
    near && crate::video::phash::distance(a.2, b.2) <= crate::video::phash::MATCH_BITS
}

/// One still's JPEG, while the folder still has it.
pub fn still(conn: &rusqlite::Connection, release_id: i64, position: u8) -> Option<Vec<u8>> {
    let s = store::shard_of_id(release_id);
    let sha256: String = conn
        .query_row(
            &format!("select sha256 from s{s}.release_stills where release_id = ?1 and position = ?2"),
            rusqlite::params![release_id, position],
            |r| r.get(0),
        )
        .ok()?;
    std::fs::read(still_file(&sha256)?).ok()
}

/// Where the video's bytes are: one file, or a stored file across RAR volumes.
enum Layout<'a> {
    Plain(&'a FileInfo),
    /// the volumes, and where the video starts in the first
    Stored(Vec<&'a FileInfo>),
}

fn parts(f: &FileInfo) -> i64 {
    f.expected.unwrap_or(f.parts_seen)
}

/// The file and part `percent` of the way through the video.
fn part_at<'a>(layout: &Layout<'a>, percent: u8) -> Option<(&'a FileInfo, i64)> {
    let volumes: &[&FileInfo] = match layout {
        Layout::Plain(f) => std::slice::from_ref(f),
        Layout::Stored(volumes) => volumes,
    };
    // by articles across the set, since the last volume is often short
    let total: i64 = volumes.iter().map(|f| parts(f).max(0)).sum();
    if total <= 0 {
        return None;
    }
    let mut at = total * i64::from(percent) / 100;
    for file in volumes {
        let n = parts(file).max(0);
        if at < n {
            return Some((file, at + 1));
        }
        at -= n;
    }
    volumes.iter().rev().find(|f| parts(f) > 0).map(|f| (*f, parts(f)))
}

/// Part `part` of a file, decoded, with what the yEnc header says it starts at.
pub(super) fn read(ctx: &mut StepCtx<'_>, file: &FileInfo, part: i64) -> Result<Option<(u64, Vec<u8>)>, StepError> {
    let segs = ctx.segments(file.id, &SegmentPick::Parts(vec![part])).map_err(StepError::from_inventory)?;
    let Some(seg) = segs.into_iter().next().filter(|s| s.part.is_none_or(|p| p == part)) else { return Ok(None) };
    let Got::Body(raw) = fetch_raw(ctx, &seg.message_id, &file.filename)? else { return Ok(None) };
    let article = yenc::split_article(&raw);
    let Some(begin) = yenc::parse(&article.lines).and_then(|l| l.begin) else { return Ok(None) };
    Ok(yenc_decode(&article.lines).map(|body| (begin.saturating_sub(1), body)))
}

/// Where a stored file's bytes sit in a RAR volume: from and to, by the volume's own header in its first part.
fn stored_span(ctx: &mut StepCtx<'_>, volume: &FileInfo) -> Result<Option<(u64, u64, Vec<u8>)>, StepError> {
    let Some((0, first)) = read(ctx, volume, 1)? else { return Ok(None) };
    let Some(head) = archive::parse(&first) else { return Ok(None) };
    let (Some(at), Some(packed)) = (head.stored_at, head.packed) else { return Ok(None) };
    Ok(Some((at as u64, at as u64 + packed, first)))
}

/// Once a stored volume's last part is read, what follows its share of the video (the end-of-archive block) is cut
/// off `data`, before it can pass for frame bytes. The new end, or None when the volume's header doesn't add up.
fn volume_end(
    ctx: &mut StepCtx<'_>,
    layout: &Layout<'_>,
    file: &FileInfo,
    next: i64,
    end: u64,
    data: &mut Vec<u8>,
) -> Result<Option<u64>, StepError> {
    if !matches!(layout, Layout::Stored(_)) || next <= parts(file) {
        return Ok(Some(end));
    }
    let Some((_, until, _)) = stored_span(ctx, file)? else { return Ok(None) };
    if end < until || end - until > data.len() as u64 {
        return Ok(None);
    }
    data.truncate(data.len() - (end - until) as usize);
    Ok(Some(until))
}

/// A keyframe from part `part` of `file` on: the frame's bytes and its time. In a stored RAR set, a frame that runs
/// off the end of one volume carries on after the next one's headers.
fn keyframe_from(
    ctx: &mut StepCtx<'_>,
    layout: &Layout<'_>,
    file: &FileInfo,
    part: i64,
    track: u64,
) -> Result<Option<(Vec<u8>, i64)>, StepError> {
    let Some((begin, mut data)) = read(ctx, file, part)? else { return Ok(None) };
    // the file being read, its next part, and where in it the data read so far ends
    let (mut file, mut next, mut end) = (file, part + 1, begin + data.len() as u64);
    let Some(e) = volume_end(ctx, layout, file, next, end, &mut data)? else { return Ok(None) };
    end = e;
    for _ in 0..MORE_ARTICLES {
        // a frame not all here yet reads on: the article budget is the limit, since a volume's last article can be
        // far shorter than the rest and says nothing about how many more the frame needs
        if let Found::Frame { start, len, timestamp } = mkv::find_keyframe(&data, track) {
            return Ok(Some((data[start..start + len].to_vec(), timestamp)));
        }
        if next > parts(file) {
            // on into the next volume, after its headers
            let Layout::Stored(volumes) = layout else { break };
            let Some(after) = volumes.iter().position(|v| v.id == file.id).and_then(|i| volumes.get(i + 1)) else {
                break;
            };
            let Some((from, _, first)) = stored_span(ctx, after)? else { break };
            let Some(body) = first.get(from as usize..) else { break };
            data.extend_from_slice(body);
            (file, next, end) = (after, 2, first.len() as u64);
        } else {
            // the next part has to carry straight on, or the bytes are no use
            let Some((from, more)) = read(ctx, file, next)? else { break };
            if from != end {
                break;
            }
            end += more.len() as u64;
            next += 1;
            data.extend(more);
        }
        let Some(e) = volume_end(ctx, layout, file, next, end, &mut data)? else { break };
        end = e;
    }
    Ok(match mkv::find_keyframe(&data, track) {
        Found::Frame { start, len, timestamp } => Some((data[start..start + len].to_vec(), timestamp)),
        _ => None,
    })
}

/// The video track, from the start of the file the video's in.
fn track_of(ctx: &mut StepCtx<'_>, first: &FileInfo) -> Result<Option<(VideoTrack, bool)>, StepError> {
    let mut front = Vec::new();
    for part in 1..=FRONT_ARTICLES.min(parts(first)) {
        let Some((begin, body)) = read(ctx, first, part)? else { break };
        if begin != front.len() as u64 {
            break;
        }
        front.extend(body);
        let (inner, stored) = match archive::parse(&front) {
            Some(head) => match head.stored_at.and_then(|at| front.get(at..)) {
                Some(inner) => (inner, true),
                None => return Ok(None),
            },
            None => (&front[..], false),
        };
        if let Some(track) = mkv::video_track(inner) {
            return Ok(Some((track, stored)));
        }
    }
    Ok(None)
}

/// One volume's share of a stored file: where in the file it starts, where in the volume, and how many bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Share {
    from: u64,
    at: u64,
    len: u64,
}

/// Reads byte ranges of the video file wherever the post keeps them: a file posted whole, or one stored across RAR
/// volumes. A volume's share is read from its own header, and the ones between are taken to hold as much as the
/// second: a set's volumes are all one size but the last. A wrong guess reads bytes that don't decode, never a
/// wrong still.
/// The most article bytes one release's stills keep while they're made: a whole `moov`, three keyframes, and the
/// parts they start and end in.
const MAX_HELD: u64 = mp4::MAX_MOOV + 3 * mp4::MAX_SAMPLE as u64 + (16 << 20);

/// Reads part `part` of a file, decoded, with what its yEnc header says it starts at (0 based).
pub(super) type Fetch<'f> = dyn FnMut(&FileInfo, i64) -> Result<Option<(u64, Vec<u8>)>, StepError> + 'f;

pub(super) struct FileBytes<'a> {
    /// by volume number from the first: a volume missing from the post leaves its slot empty, soo the ones after
    /// it keep their place in the file
    volumes: Vec<Option<&'a FileInfo>>,
    stored: bool,
    shares: Vec<Option<Share>>,
    /// the stored file's name, from the first volume: a later one must hold the same file
    name: Option<String>,
    /// each volume's article size, from its first part
    segments: Vec<Option<u64>>,
    /// the parts read so far, by (volume, part): what the yEnc header says each starts at, and its bytes
    parts: std::collections::HashMap<(usize, i64), (u64, Vec<u8>)>,
    /// the bytes of those parts
    held: u64,
    /// parts that came to nothing, not asked for again
    failed: std::collections::HashSet<(usize, i64)>,
}

impl<'a> FileBytes<'a> {
    pub(super) fn new(volumes: Vec<Option<&'a FileInfo>>, stored: bool) -> Self {
        let n = volumes.len();
        FileBytes {
            volumes,
            stored,
            shares: vec![None; n],
            name: None,
            segments: vec![None; n],
            parts: std::collections::HashMap::new(),
            held: 0,
            failed: std::collections::HashSet::new(),
        }
    }

    /// Part `part` of volume `v`, once.
    fn part(&mut self, fetch: &mut Fetch<'_>, v: usize, part: i64) -> Result<Option<(u64, Vec<u8>)>, StepError> {
        if let Some(got) = self.parts.get(&(v, part)) {
            return Ok(Some(got.clone()));
        }
        if self.failed.contains(&(v, part)) {
            return Ok(None);
        }
        let got = self.fetch_part(fetch, v, part)?;
        if got.is_none() {
            self.failed.insert((v, part));
        }
        Ok(got)
    }

    fn fetch_part(&mut self, fetch: &mut Fetch<'_>, v: usize, part: i64) -> Result<Option<(u64, Vec<u8>)>, StepError> {
        let Some(file) = self.volumes[v] else { return Ok(None) };
        let Some(got) = fetch(file, part)? else { return Ok(None) };
        let len = got.1.len() as u64;
        if part == 1 {
            if got.0 != 0 {
                return Ok(None);
            }
            self.segments[v] = Some(len);
        } else {
            // every part where its number says, and no longer than the first: soo each one read carries a whole
            // part's worth of the range, and a post can't make the reads hold more than the range they're for
            let Some(segment) = self.segments[v] else { return Ok(None) };
            if Some(got.0) != u64::try_from(part - 1).ok().and_then(|p| p.checked_mul(segment)) || len > segment {
                return Ok(None);
            }
        }
        if self.held + len > MAX_HELD {
            return Ok(None);
        }
        self.held += len;
        self.parts.insert((v, part), got.clone());
        Ok(Some(got))
    }

    /// Volume `v`'s share, from its header, starting at `from` in the file.
    fn share(&mut self, fetch: &mut Fetch<'_>, v: usize, from: u64) -> Result<Option<Share>, StepError> {
        if let Some(share) = self.shares[v] {
            return Ok(Some(share));
        }
        let Some((_, first)) = self.part(fetch, v, 1)? else { return Ok(None) };
        let share = if self.stored {
            let Some(head) = archive::parse(&first) else { return Ok(None) };
            let (Some(at), Some(len)) = (head.stored_at, head.packed) else { return Ok(None) };
            if v == 0 {
                self.name = head.name.clone();
            } else if head.name != self.name || !head.split_before {
                return Ok(None);
            }
            Share { from, at: at as u64, len }
        } else {
            Share { from: 0, at: 0, len: u64::MAX }
        };
        self.shares[v] = Some(share);
        Ok(Some(share))
    }

    /// The volume holding byte `x` of the file, and its share.
    fn locate(&mut self, fetch: &mut Fetch<'_>, x: u64) -> Result<Option<(usize, Share)>, StepError> {
        let Some(first) = self.share(fetch, 0, 0)? else { return Ok(None) };
        if x < first.from.saturating_add(first.len) {
            return Ok(Some((0, first)));
        }
        // a share already read, as when a range runs on from one volume into the next
        if let Some((v, share)) = self
            .shares
            .iter()
            .enumerate()
            .find_map(|(v, s)| s.filter(|s| (s.from..s.from + s.len).contains(&x)).map(|s| (v, s)))
        {
            return Ok(Some((v, share)));
        }
        if self.volumes.len() < 2 {
            return Ok(None);
        }
        let Some(second) = self.share(fetch, 1, first.len)? else { return Ok(None) };
        if x < second.from + second.len {
            return Ok(Some((1, second)));
        }
        if second.len == 0 {
            return Ok(None);
        }
        let v = 1 + usize::try_from((x - second.from) / second.len).unwrap_or(usize::MAX);
        if v >= self.volumes.len() {
            return Ok(None);
        }
        let Some(share) = self.share(fetch, v, second.from + (v as u64 - 1) * second.len)? else { return Ok(None) };
        Ok((x < share.from + share.len).then_some((v, share)))
    }

    /// The file's bytes `from..to`, or None when a part is missing or doesn't sit where it should.
    pub(super) fn range(&mut self, fetch: &mut Fetch<'_>, from: u64, to: u64) -> Result<Option<Vec<u8>>, StepError> {
        let mut out = Vec::with_capacity(usize::try_from(to.saturating_sub(from)).unwrap_or(0).min(64 << 20));
        let mut x = from;
        while x < to {
            let Some((v, share)) = self.locate(fetch, x)? else { return Ok(None) };
            let until = to.min(share.from.saturating_add(share.len));
            // where those bytes are in the volume
            let (mut at, end) = (share.at + (x - share.from), share.at + (until - share.from));
            while at < end {
                let part = match self.segments[v] {
                    Some(segment) if segment > 0 => i64::try_from(at / segment).unwrap_or(i64::MAX - 1) + 1,
                    _ => 1,
                };
                let Some((begin, body)) = self.part(fetch, v, part)? else { return Ok(None) };
                if begin > at || begin + body.len() as u64 <= at {
                    return Ok(None);
                }
                let lo = (at - begin) as usize;
                let hi = (end - begin).min(body.len() as u64) as usize;
                out.extend_from_slice(&body[lo..hi]);
                at = begin + hi as u64;
            }
            // the next volume's share starts where this one's ends
            if until < to && self.stored {
                let next = v + 1;
                if next >= self.volumes.len() {
                    return Ok(None);
                }
                if self.share(fetch, next, until)?.is_none() {
                    return Ok(None);
                }
            }
            x = until;
        }
        Ok(Some(out))
    }
}

/// A RAR volume's number, from 0 for the first: `.part01.rar` is 0 (or whatever number the set starts at, taken
/// off by the caller), `.rar` is 0 and `.r00` 1, on through `.s00` (101) and later letters.
fn volume_number(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    if let Some(rest) = lower.strip_suffix(".rar") {
        let digits = rest
            .rsplit_once(".part")
            .map(|(_, d)| d)
            .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()));
        return match digits {
            Some(d) => d.parse().ok(),
            None => Some(0),
        };
    }
    let ext = lower.rsplit_once('.')?.1.as_bytes();
    match ext {
        [letter @ b'r'..=b'z', d1, d2] if d1.is_ascii_digit() && d2.is_ascii_digit() => {
            Some(1 + u32::from(letter - b'r') * 100 + u32::from(d1 - b'0') * 10 + u32::from(d2 - b'0'))
        }
        _ => None,
    }
}

/// The volumes of a set in slots by their number from the first, a missing one leaving its slot empty. None when a
/// name doesn't say its number, or two say the same.
pub(super) fn volume_slots<'a>(first: &str, volumes: Vec<&'a FileInfo>) -> Option<Vec<Option<&'a FileInfo>>> {
    let start = volume_number(first)?;
    let mut slots: Vec<Option<&FileInfo>> = Vec::new();
    for v in volumes {
        let slot = usize::try_from(volume_number(&v.filename)?.checked_sub(start)?).ok()?;
        if slot >= 10_000 {
            return None;
        }
        if slots.len() <= slot {
            slots.resize(slot + 1, None);
        }
        if slots[slot].replace(v).is_some() {
            return None;
        }
    }
    slots.first().copied().flatten()?;
    Some(slots)
}

/// A video track, and the keyframes for the stills as (position, frame, time in ms).
type Frames = (VideoTrack, Vec<(u8, Vec<u8>, i64)>);

/// How many top-level boxes are walked past for the `moov`: an MP4 not made for streaming has it after the media,
/// sometimes after a `free` or `uuid` box too.
const MOOV_HOPS: usize = 8;
/// Enough for a box's header, even a 64-bit one.
const BOX_HEAD: u64 = 16;

/// The keyframes of an MP4 for the stills, as (position, frame, time in ms), and its video track. The `moov` is
/// read where the top-level boxes say it is, at the front or after the media, then each keyframe's bytes.
fn mp4_frames(
    fetch: &mut Fetch<'_>,
    volumes: Vec<Option<&FileInfo>>,
    stored: bool,
) -> Result<Option<Frames>, StepError> {
    let mut bytes = FileBytes::new(volumes, stored);
    let Some(mut peek) = bytes.range(fetch, 0, BOX_HEAD)? else { return Ok(None) };
    let mut base = 0;
    let mut moov = None;
    for _ in 0..MOOV_HOPS {
        match mp4::find_moov(&peek, base) {
            mp4::Moov::At(at, size) => {
                if size > mp4::MAX_MOOV {
                    return Ok(None);
                }
                // the part of it already here is read again from the parts kept
                moov = bytes.range(fetch, at, at + size)?;
                break;
            }
            mp4::Moov::After(next) => {
                let Some(head) = bytes.range(fetch, next, next + BOX_HEAD)? else { return Ok(None) };
                (peek, base) = (head, next);
            }
            mp4::Moov::None => return Ok(None),
        }
    }
    let Some(moov) = moov else { return Ok(None) };
    let Some((_, _, head)) = mp4::box_head(&moov) else { return Ok(None) };
    let Some(video) = moov.get(head..).and_then(mp4::video_track) else { return Ok(None) };
    let mut frames = Vec::new();
    for percent in POSITIONS {
        let Some(sample) = video.keyframe_at(percent) else { continue };
        let Some(frame) = bytes.range(fetch, sample.offset, sample.offset + u64::from(sample.size))? else { continue };
        frames.push((percent, frame, sample.millis));
    }
    Ok(Some((video.track(), frames)))
}

/// The keyframes of a Matroska video for the stills, as (position, frame, time in ms), and its video track.
fn mkv_frames(ctx: &mut StepCtx<'_>, files: &[FileInfo], first: &FileInfo) -> Result<Option<Frames>, StepError> {
    let Some((track, stored)) = track_of(ctx, first)? else { return Ok(None) };
    // by the file's own index of its keyframes, as an MP4's `moov` is read: a scan from a position finds one only
    // when a keyframe falls within the few articles read, and an encode with one every ten seconds rarely has one
    let volumes = if stored {
        volume_slots(&first.filename, moviehash::volumes(&first.filename, files))
    } else {
        Some(vec![Some(first)])
    };
    let mut frames = match volumes {
        Some(volumes) => {
            mkv_cued_frames(&mut |file: &FileInfo, part: i64| read(ctx, file, part), volumes, stored, &track)?
                .unwrap_or_default()
        }
        None => Vec::new(),
    };
    if frames.len() == POSITIONS.len() {
        return Ok(Some((track, frames)));
    }
    // the positions the Cues gave no keyframe for are scanned for one
    let layout = if stored {
        let volumes = moviehash::volumes(&first.filename, files);
        if volumes.is_empty() {
            return Ok(None);
        }
        Layout::Stored(volumes)
    } else {
        Layout::Plain(first)
    };
    for percent in POSITIONS {
        if frames.iter().any(|f| f.0 == percent) {
            continue;
        }
        let Some((file, part)) = part_at(&layout, percent) else { continue };
        let Some((frame, at)) = keyframe_from(ctx, &layout, file, part, track.number)? else { continue };
        let at_ms = track.millis(at);
        // the keyframe a cue gave another position, found again by the scan: one picture, once
        if frames.iter().any(|f| f.2 == at_ms) {
            continue;
        }
        frames.push((percent, frame, at_ms));
    }
    frames.sort_by_key(|f| f.0);
    Ok(Some((track, frames)))
}

/// Bytes of a Matroska file's start read for its SeekHead.
const MKV_FRONT: u64 = 64 << 10;
/// The most a Cues element is read for: a cue a keyframe for three hours of video is well under this.
const MAX_CUES: u64 = 4 << 20;
/// Bytes read from a cued cluster's start, before the frame begun there says how many more it needs.
const CLUSTER_READ: u64 = 1 << 20;
/// The shortest read tried near a file's end.
const MIN_CLUSTER_STEP: u64 = 4 << 10;

/// The keyframes of a Matroska file for the stills, as (position, frame, time in ms), by its Cues: the cluster each
/// position's nearest cue points at, read from its start. None when the file has no Cues its SeekHead names.
fn mkv_cued_frames(
    fetch: &mut Fetch<'_>,
    volumes: Vec<Option<&FileInfo>>,
    stored: bool,
    track: &mkv::VideoTrack,
) -> Result<Option<Vec<(u8, Vec<u8>, i64)>>, StepError> {
    let mut bytes = FileBytes::new(volumes, stored);
    let Some(front) = bytes.range(fetch, 0, MKV_FRONT)? else { return Ok(None) };
    let Some((segment, cues_at)) = mkv::cues_from_front(&front) else { return Ok(None) };
    let Some(head) = bytes.range(fetch, cues_at, cues_at.saturating_add(16))? else { return Ok(None) };
    let Some(len) = mkv::cues_len(&head).filter(|len| *len <= MAX_CUES) else { return Ok(None) };
    let Some(end) = cues_at.checked_add(len) else { return Ok(None) };
    let Some(cues) = bytes.range(fetch, cues_at, end)? else { return Ok(None) };
    let points = mkv::cue_points(&cues, track.number);
    let mut frames = Vec::new();
    let mut used = Vec::new();
    for percent in POSITIONS {
        // two positions on one cue (a short film, or cues far apart) would be one picture twice: the scan tries
        // the second
        let Some((_, cluster)) = mkv::cue_at(&points, percent).filter(|(_, c)| !used.contains(c)) else { continue };
        used.push(cluster);
        let Some(from) = segment.checked_add(cluster) else { continue };
        // one that leads to no keyframe is left to the scan: its cluster's first articles were all it cost
        if let Some((frame, timestamp)) = cued_keyframe(fetch, &mut bytes, from, track.number)? {
            frames.push((percent, frame, track.millis(timestamp)));
        }
    }
    Ok(Some(frames))
}

/// The keyframe at the start of the cluster at `from`: its bytes and its timestamp, in the segment's ticks. A
/// cluster's start is read, then as much more as the frame begun there says it needs; near the file's end a
/// shorter first read is tried.
fn cued_keyframe(
    fetch: &mut Fetch<'_>,
    bytes: &mut FileBytes<'_>,
    from: u64,
    track: u64,
) -> Result<Option<(Vec<u8>, i64)>, StepError> {
    let mut data = None;
    let mut step = CLUSTER_READ;
    while data.is_none() && step >= MIN_CLUSTER_STEP {
        let Some(to) = from.checked_add(step) else { return Ok(None) };
        data = bytes.range(fetch, from, to)?;
        step /= 4;
    }
    let Some(mut data) = data.filter(|d| !d.is_empty()) else { return Ok(None) };
    for _ in 0..2 {
        match mkv::find_keyframe(&data, track) {
            Found::Frame { start, len, timestamp } => return Ok(Some((data[start..start + len].to_vec(), timestamp))),
            Found::Partial { start, len, .. } => {
                let (Some(at), Some(to)) =
                    (from.checked_add(data.len() as u64), from.checked_add((start + len) as u64))
                else {
                    return Ok(None);
                };
                let Some(more) = bytes.range(fetch, at, to)?.filter(|m| !m.is_empty()) else { return Ok(None) };
                data.extend(more);
            }
            // a cued cluster starts with its keyframe: none in its first bytes is none at all
            _ => return Ok(None),
        }
    }
    Ok(None)
}

/// MP4 sample entries of the video the decoders read, as the probe names them.
const MP4_CODECS: [&str; 4] = ["avc1", "avc3", "hvc1", "hev1"];

/// Asks for stills again on every MP4 with HEVC or H.264 video: they had none before `mp4_frames`. Once, from the
/// stage's seed.
pub(crate) fn rerun_mp4(conn: &rusqlite::Connection, now: i64) -> rusqlite::Result<usize> {
    let codecs = MP4_CODECS.map(|c| format!("'{c}'")).join(", ");
    let ids: Vec<i64> = conn
        .prepare(&format!(
            "select release_id from release_probe
             where json_extract(info, '$.media.container') = 'mp4'
               and json_extract(info, '$.media.video[0].codec') in ({codecs})"
        ))?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for id in &ids {
        crate::postprocess::rerun_on(conn, *id, mask(STILLS_STEP), now)?;
    }
    Ok(ids.len())
}

/// A still from a keyframe: JPEG bytes, width, height, and its perceptual hash when it has one.
fn still_of(track: &VideoTrack, frame: &[u8]) -> Option<(Vec<u8>, u32, u32, Option<u64>)> {
    let picture = match track.codec {
        Codec::Hevc => {
            let hvcc = hevc::nal::parse_hvcc(&track.private).ok()?;
            let mut units = hvcc.units;
            units.extend(hevc::nal::split_sized(frame, hvcc.length_size).ok()?);
            hevc::decode_keyframe(&units).ok()?
        }
        Codec::Avc => {
            let avcc = h264::parse_avcc(&track.private).ok()?;
            let mut units: Vec<&[u8]> = avcc.units.iter().map(Vec::as_slice).collect();
            units.extend(hevc::nal::split_sized(frame, avcc.length_size).ok()?);
            h264::decode_keyframe(&units).ok()?
        }
    };
    let [y, cb, cr] = picture.to_8bit(STILL_WIDTH, track.display);
    if y.width == 0 {
        return None;
    }
    let jpeg = jpeg::encode(&y.data, &cb.data, &cr.data, y.width, y.height, QUALITY);
    Some((jpeg, y.width as u32, y.height as u32, crate::video::phash::phash(&y)))
}

#[derive(Default)]
pub struct StillsStep;

impl Step for StillsStep {
    fn id(&self) -> StepId {
        STILLS_STEP
    }
    fn name(&self) -> &'static str {
        "stills"
    }
    fn rank(&self) -> u16 {
        140
    }
    fn needs(&self) -> Needs {
        Needs::Bodies
    }
    fn reruns(&self) -> Reruns {
        // a release that grows may have the articles a still needed
        Reruns { on_rename: false, on_growth: true }
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.archive.enabled
    }
    /// Bit 1: every Matroska video gets stills, not only those of 100 MiB or more, soo smaller ones already
    /// processed are looked at again. Bit 2: each still has a perceptual hash. Bit 4: a Matroska file's keyframes
    /// are found by its Cues, soo the many done with none found by the scan are looked at again (one with its
    /// stills all kept fetches nothing).
    fn config_stamp(&self) -> u64 {
        1 | 2 | 4
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let release_id = ctx.release().id;
        // a release whose stills are all still on disk, and hashed, keeps them: the walk for a new stamp bit fetches
        // nothing. One trimmed from the folder, or from before hashes, is made again.
        let kept = ctx.data().kept_stills(release_id)?;
        if kept.len() == POSITIONS.len()
            && kept.iter().all(|(h, hashed)| *hashed && still_file(h).is_some_and(|f| f.exists()))
        {
            return Ok(Outcome { matched: true, ..Outcome::default() });
        }
        let rel_name = ctx.release().name.clone();
        let rel_group = ctx.release().group.clone();
        let content_id = ctx.data().release_content_id(release_id)?.unwrap_or_default();
        let saved_stills = crate::raw_store::get_stills_checked(&rel_name, &rel_group, &content_id).map_err(StepError::from_cache)?;
        if !ctx.data().explicit_rerun(release_id, STILLS_STEP)?
            && saved_stills.len() == POSITIONS.len()
            && saved_stills.iter().all(|s| s.phash.is_some() && still_file(&s.sha256).is_some_and(|f| f.exists()))
        {
            let mut writes = vec![StepWrite::for_mutation(release_id, StepMutation::ClearStills)];
            for s in saved_stills {
                writes.push(StepWrite::for_mutation(release_id, StepMutation::KeepStill(s)));
            }
            return Ok(Outcome { matched: true, writes, ..Outcome::default() });
        }
        let info = ctx.data().probe_info_best_effort(release_id)?;
        let Some(info) = info.and_then(|i| serde_json::from_str::<ProbeInfo>(&i).ok()) else {
            return Ok(Outcome::default());
        };
        // only video we can decode: HEVC or H.264, in Matroska or MP4
        let media = info.media.as_ref();
        let codec = media.and_then(|m| m.video.first()).map(|v| v.codec.as_str());
        let container = media.map(|m| m.container.as_str());
        let matroska = matches!(container, Some("matroska" | "webm"))
            && matches!(codec, Some("V_MPEGH/ISO/HEVC" | "V_MPEG4/ISO/AVC"));
        let is_mp4 = container == Some("mp4") && codec.is_some_and(|c| MP4_CODECS.contains(&c));
        if !(matroska || is_mp4)
            || info.archive.as_ref().is_some_and(|a| a.encrypted || a.method.as_deref() != Some("m0"))
        {
            return Ok(Outcome::default());
        }
        // the same video file in another release that already has its stills: they are shown from there, soo nothing
        // is read from the server or kept a second time
        if !ctx.data().explicit_rerun(release_id, STILLS_STEP)? {
            let lookup = ctx.data().stills_lookup(release_id)?;
            if SAME_FILE.iter().any(|k| lookup.has(*k)) {
                if let Some(kind) = ctx.data().same_file_stills(release_id, &lookup)? {
                    crate::metrics::names::STILLS_SHARED.inc(&[("key", kind.word())], 1);
                    return Ok(Outcome {
                        matched: true,
                        writes: vec![StepWrite::for_mutation(release_id, StepMutation::ClearStills)],
                        ..Outcome::default()
                    });
                }
            }
        }
        let files = ctx.files().map_err(StepError::from_inventory)?;
        let Some(first) = main_file(&files) else { return Ok(Outcome::default()) };
        let found = if matroska {
            mkv_frames(ctx, &files, first)?
        } else {
            let stored = info.archive.is_some();
            let volumes = if stored {
                match volume_slots(&first.filename, moviehash::volumes(&first.filename, &files)) {
                    Some(slots) => slots,
                    None => return Ok(Outcome::default()),
                }
            } else {
                vec![Some(first)]
            };
            mp4_frames(&mut |file: &FileInfo, part: i64| read(ctx, file, part), volumes, stored)?
        };
        let Some((track, frames)) = found else { return Ok(Outcome::default()) };
        let mut writes = vec![StepWrite::for_mutation(release_id, StepMutation::ClearStills)];
        let mut made = 0;
        let mut raw_saved = Vec::new();
        for (percent, frame, at_ms) in frames {
            let Some((jpeg, w, h, phash)) = still_of(&track, &frame) else { continue };
            let Some(sha256) = keep(&jpeg) else { continue };
            let phash_i64 = phash.map(|p| p as i64);
            made += 1;
            let still = crate::raw_store::SavedStill {
                position: percent,
                at_ms: Some(at_ms),
                sha256: sha256.clone(),
                width: w,
                height: h,
                phash: phash_i64,
            };
            raw_saved.push(still.clone());
            writes.push(StepWrite::for_mutation(release_id, StepMutation::KeepStill(still)));
        }
        // fewer than it has already (a run asked for again whose reads came to less): what it has stays
        if made < kept.iter().filter(|(h, _)| still_file(h).is_some_and(|f| f.exists())).count() {
            return Ok(Outcome { matched: true, ..Outcome::default() });
        }
        if !raw_saved.is_empty() {
            crate::raw_store::put_stills_checked(&rel_name, &rel_group, &content_id, &raw_saved).map_err(StepError::from_cache)?;
        }
        Ok(Outcome { matched: made > 0, writes, ..Outcome::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, parts: i64) -> FileInfo {
        FileInfo {
            id: 1,
            filename: name.into(),
            subject: None,
            subject_part: None,
            expected: Some(parts),
            file_total: None,
            parts_seen: parts,
            touched_at: None,
            sealed: false,
        }
    }

    #[test]
    fn positions_land_in_the_right_part_and_volume() {
        let plain = file("film.mkv", 100);
        assert_eq!(part_at(&Layout::Plain(&plain), 25).map(|(_, p)| p), Some(26));
        assert_eq!(part_at(&Layout::Plain(&plain), 75).map(|(_, p)| p), Some(76));
        let volumes: Vec<FileInfo> = (0..8).map(|i| file(&format!("film.part{:02}.rar", i + 1), 40)).collect();
        let layout = Layout::Stored(volumes.iter().collect());
        let (f, p) = part_at(&layout, 50).unwrap();
        assert_eq!((f.filename.as_str(), p), ("film.part05.rar", 1));
        let (f, p) = part_at(&layout, 75).unwrap();
        assert_eq!((f.filename.as_str(), p), ("film.part07.rar", 1));
        let (f, p) = part_at(&layout, 30).unwrap();
        assert_eq!((f.filename.as_str(), p), ("film.part03.rar", 17));
        // a short last volume counts for what it holds
        let short = [file("a.part1.rar", 100), file("a.part2.rar", 4)];
        let layout = Layout::Stored(short.iter().collect());
        let picks: Vec<_> = [25, 50, 75, 100]
            .into_iter()
            .map(|p| part_at(&layout, p).map(|(f, n)| (f.filename.clone(), n)).unwrap())
            .collect();
        assert_eq!(picks[0], ("a.part1.rar".into(), 27));
        assert_eq!(picks[1], ("a.part1.rar".into(), 53));
        assert_eq!(picks[2], ("a.part1.rar".into(), 79));
        assert_eq!(picks[3], ("a.part2.rar".into(), 4));
    }

    #[test]
    fn stills_match_near_in_time_and_hash() {
        let hash = 0x86c7_6333_7078_7875;
        assert!(still_matches((25, Some(4_000), hash), (25, Some(12_000), hash ^ 0b111)));
        assert!(!still_matches((25, Some(4_000), hash), (25, Some(40_000), hash)), "too far apart");
        assert!(!still_matches((25, Some(4_000), hash), (50, Some(4_000), !hash)), "another picture");
        assert!(still_matches((25, None, hash), (25, Some(4_000), hash)), "no time: by position");
        assert!(!still_matches((25, None, hash), (50, Some(4_000), hash)));
    }

    #[test]
    fn a_matroska_files_cues_find_a_keyframe_for_each_position() {
        let data = std::fs::read(format!("{}/tests/fixtures/hevc/clip.mkv", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let track = mkv::video_track(&data).unwrap();
        // posted whole, in articles of 16 KiB, each read counted
        const ARTICLE: usize = 16 << 10;
        let film = file("clip.mkv", data.len().div_ceil(ARTICLE) as i64);
        let mut reads = Vec::new();
        let mut fetch = |_: &FileInfo, part: i64| -> Result<Option<(u64, Vec<u8>)>, StepError> {
            reads.push(part);
            let from = (part as usize - 1) * ARTICLE;
            Ok(data.get(from..data.len().min(from + ARTICLE)).map(|b| (from as u64, b.to_vec())))
        };
        let frames = mkv_cued_frames(&mut fetch, vec![Some(&film)], false, &track).unwrap().unwrap();
        assert_eq!(frames.iter().map(|f| f.0).collect::<Vec<_>>(), POSITIONS.to_vec());
        let times: Vec<i64> = frames.iter().map(|f| f.2).collect();
        assert!(times.windows(2).all(|w| w[0] < w[1]), "{times:?}");
        for (_, frame, _) in &frames {
            assert!(still_of(&track, frame).is_some(), "each one decodes");
        }
        // each article fetched once, however many reads it's in
        let mut unique = reads.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), reads.len(), "{reads:?}");
        // a cue pointing past any file's end: nothing, at once
        let mut none = |_: &FileInfo, _: i64| -> Result<Option<(u64, Vec<u8>)>, StepError> { Ok(None) };
        let mut bytes = FileBytes::new(vec![Some(&film)], false);
        assert!(cued_keyframe(&mut none, &mut bytes, u64::MAX - 10, track.number).unwrap().is_none());
        // a file whose front names no Cues: none, for the scan to try
        let mut front_only = |_: &FileInfo, part: i64| -> Result<Option<(u64, Vec<u8>)>, StepError> {
            let mut bytes = vec![0x1A, 0x45, 0xDF, 0xA3, 0x80];
            bytes.resize(ARTICLE, 0);
            Ok((part == 1).then_some((0, bytes)))
        };
        assert!(mkv_cued_frames(&mut front_only, vec![Some(&film)], false, &track).unwrap().is_none());
    }

    #[test]
    fn a_keyframe_makes_a_jpeg_still() {
        let data = std::fs::read(format!("{}/tests/fixtures/hevc/clip.mkv", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let track = mkv::video_track(&data).unwrap();
        let chunk = &data[data.len() / 2..];
        let Found::Frame { start, len, .. } = mkv::find_keyframe(chunk, track.number) else { panic!() };
        let (jpeg, w, h, phash) = still_of(&track, &chunk[start..start + len]).unwrap();
        assert_eq!((w, h), (320, 180));
        assert!(phash.is_some());
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
    }

    /// A keyframe of a fixture's Matroska video, with the track it decodes with.
    fn fixture_keyframe(name: &str) -> (VideoTrack, Vec<u8>) {
        let data = std::fs::read(format!("{}/tests/fixtures/{name}/clip.mkv", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let track = mkv::video_track(&data).unwrap();
        let chunk = &data[data.len() / 2..];
        let Found::Frame { start, len, .. } = mkv::find_keyframe(chunk, track.number) else { panic!() };
        (track, chunk[start..start + len].to_vec())
    }

    /// An MP4 of 40 samples, a keyframe every 10th, the rest filler: its `moov` before the media, or after.
    fn mp4_file(track: &VideoTrack, keyframe: &[u8], moov_last: bool) -> Vec<u8> {
        use crate::video::mp4::tests::{moov, mp4_box};
        let entry = match track.codec {
            Codec::Hevc => b"hvc1",
            Codec::Avc => b"avc1",
        };
        let samples: Vec<Vec<u8>> =
            (0..40u8).map(|i| if i % 10 == 0 { keyframe.to_vec() } else { vec![i; 700 + usize::from(i)] }).collect();
        let sizes: Vec<u32> = samples.iter().map(|s| s.len() as u32).collect();
        let syncs = [1, 11, 21, 31];
        let ftyp = mp4_box(b"ftyp", b"isom\0\0\0\0");
        let mdat = mp4_box(b"mdat", &samples.concat());
        let moov_len = moov(entry, &track.private, &sizes, 4, 0, &syncs).len();
        if moov_last {
            let first_at = (ftyp.len() + 8) as u32;
            [ftyp, mdat, moov(entry, &track.private, &sizes, 4, first_at, &syncs)].concat()
        } else {
            let first_at = (ftyp.len() + moov_len + 8) as u32;
            [ftyp, moov(entry, &track.private, &sizes, 4, first_at, &syncs), mdat].concat()
        }
    }

    /// A file's posted parts: (part number, where it starts, its bytes).
    type Parts = Vec<(i64, u64, Vec<u8>)>;

    /// A file posted in `segment` byte parts.
    fn posted(bytes: &[u8], segment: usize) -> Parts {
        bytes.chunks(segment).enumerate().map(|(i, c)| (i as i64 + 1, (i * segment) as u64, c.to_vec())).collect()
    }

    /// RAR4 volumes storing `inner`, `share` bytes of it in each.
    fn rar4_volumes(inner: &[u8], share: usize) -> Vec<Vec<u8>> {
        let name = b"Film.2016.1080p.mp4";
        let pieces: Vec<&[u8]> = inner.chunks(share).collect();
        let last = pieces.len() - 1;
        pieces
            .iter()
            .enumerate()
            .map(|(i, piece)| {
                let flags: u16 = 0x8000 | if i > 0 { 0x01 } else { 0 } | if i < last { 0x02 } else { 0 };
                let mut h = vec![0u8, 0, 0x74];
                h.extend(flags.to_le_bytes());
                h.extend(((32 + name.len()) as u16).to_le_bytes());
                h.extend((piece.len() as u32).to_le_bytes());
                h.extend((inner.len() as u32).to_le_bytes());
                h.push(3);
                h.extend(0u32.to_le_bytes());
                h.extend(0u32.to_le_bytes());
                h.push(29);
                h.push(0x30);
                h.extend((name.len() as u16).to_le_bytes());
                h.extend(0u32.to_le_bytes());
                h.extend(name);
                [b"Rar!\x1a\x07\x00".as_slice(), &[0, 0, 0x73, 0, 0, 13, 0, 0, 0, 0, 0, 0, 0], &h, piece].concat()
            })
            .collect()
    }

    /// The stills an MP4 posted as these files makes, read through `mp4_frames`, and the parts it read.
    fn mp4_stills(files: &[(FileInfo, Parts)], stored: bool) -> (Vec<(u8, u32, u32, i64)>, usize) {
        let mut reads = 0;
        let mut fetch = |f: &FileInfo, part: i64| -> Result<Option<(u64, Vec<u8>)>, StepError> {
            reads += 1;
            let (_, parts) = files.iter().find(|(file, _)| file.id == f.id).unwrap();
            Ok(parts.iter().find(|(p, ..)| *p == part).map(|(_, begin, body)| (*begin, body.clone())))
        };
        let volumes: Vec<Option<&FileInfo>> = if stored {
            volume_slots(&files[0].0.filename, files.iter().map(|(f, _)| f).collect()).unwrap()
        } else {
            files.iter().map(|(f, _)| Some(f)).collect()
        };
        let (track, frames) = mp4_frames(&mut fetch, volumes, stored).unwrap().unwrap();
        let stills = frames
            .iter()
            .map(|(percent, frame, at_ms)| {
                let (jpeg, w, h, _) = still_of(&track, frame).expect("the keyframe decodes");
                assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
                (*percent, w, h, *at_ms)
            })
            .collect();
        (stills, reads)
    }

    fn numbered(id: i64, name: &str, parts: usize) -> FileInfo {
        FileInfo { id, ..file(name, parts as i64) }
    }

    #[test]
    fn an_mp4_posted_whole_gives_stills_with_its_moov_first_or_last() {
        for (fixture, size) in [("hevc", (320, 180)), ("h264", (320, 180))] {
            let (track, keyframe) = fixture_keyframe(fixture);
            for moov_last in [false, true] {
                let parts = posted(&mp4_file(&track, &keyframe, moov_last), 3_000);
                let film = numbered(1, "film.mp4", parts.len());
                let (stills, reads) = mp4_stills(&[(film, parts.clone())], false);
                // 25, 50 and 75% are samples 10, 20 and 30: the keyframes, at 1001/24000 s a sample
                let times: Vec<i64> = [10, 20, 30].iter().map(|n| n * 1001 * 1000 / 24000).collect();
                let want: Vec<_> = [25, 50, 75].iter().zip(&times).map(|(p, t)| (*p, size.0, size.1, *t)).collect();
                let got: Vec<_> = stills.iter().map(|(p, w, h, t)| (*p, (*w, *h), *t)).collect();
                assert_eq!(got.len(), 3, "{fixture} moov_last={moov_last}");
                for ((p, (w, h), t), (wp, ww, wh, wt)) in got.iter().zip(&want) {
                    assert_eq!((*p, *t), (*wp, *wt));
                    assert!(*w > 0 && *h > 0 && *w <= *ww && *h <= *wh);
                }
                // each part read once at most
                assert!(reads <= parts.len(), "{reads} reads of {} parts", parts.len());
            }
        }
    }

    #[test]
    fn an_mp4_stored_across_rar_volumes_gives_stills() {
        let (track, keyframe) = fixture_keyframe("hevc");
        // shares smaller than a keyframe: frames run across volumes, and across parts within one
        let share = keyframe.len() / 2 + 333;
        for moov_last in [false, true] {
            let inner = mp4_file(&track, &keyframe, moov_last);
            let files: Vec<(FileInfo, Parts)> = rar4_volumes(&inner, share)
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let parts = posted(v, 1_500);
                    (numbered(i as i64 + 1, &format!("x.part{:03}.rar", i + 1), parts.len()), parts)
                })
                .collect();
            let (stills, _) = mp4_stills(&files, true);
            assert_eq!(stills.iter().map(|s| s.0).collect::<Vec<_>>(), [25, 50, 75], "moov_last={moov_last}");
        }
    }

    #[test]
    fn a_missing_part_gives_no_still_rather_than_a_wrong_one() {
        let (track, keyframe) = fixture_keyframe("hevc");
        let mut parts = posted(&mp4_file(&track, &keyframe, false), 3_000);
        let film = numbered(1, "film.mp4", parts.len());
        // the part holding the 50% keyframe is gone
        let inner = mp4_file(&track, &keyframe, false);
        let (_, size, head) = mp4::box_head(&inner[16..]).unwrap();
        let video = mp4::video_track(&inner[16 + head..16 + size.unwrap() as usize]).unwrap();
        let at = video.keyframe_at(50).unwrap().offset as usize;
        parts.retain(|(p, ..)| *p != (at / 3_000) as i64 + 1);
        let (stills, _) = mp4_stills(&[(film, parts)], false);
        assert_eq!(stills.iter().map(|s| s.0).collect::<Vec<_>>(), [25, 75]);
    }

    #[test]
    fn mp4s_with_video_we_decode_are_asked_for_stills_again() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        crate::db::create_db_at(&main).unwrap();
        let conn = crate::db::open_shard(&crate::store::shard_path(&main, 0)).unwrap();
        for (id, info) in [
            (1i64, r#"{"media":{"container":"mp4","video":[{"codec":"hev1"}]}}"#),
            (2, r#"{"media":{"container":"mp4","video":[{"codec":"avc1"}]}}"#),
            (3, r#"{"media":{"container":"mp4","video":[{"codec":"mp4v"}]}}"#),
            (4, r#"{"media":{"container":"matroska","video":[{"codec":"V_MPEGH/ISO/HEVC"}]}}"#),
        ] {
            conn.execute("insert into releases (id, name, group_name) values (?, ?, 'g')", rusqlite::params![id, id])
                .unwrap();
            conn.execute(
                "insert into pp_state (release_id, done) values (?, ?)",
                rusqlite::params![id, mask(STILLS_STEP)],
            )
            .unwrap();
            conn.execute("insert into release_probe (release_id, info) values (?, ?)", rusqlite::params![id, info])
                .unwrap();
        }
        assert_eq!(rerun_mp4(&conn, 100).unwrap(), 2);
        let done: Vec<i64> = conn
            .prepare("select done from pp_state order by release_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(done, [0, 0, mask(STILLS_STEP) as i64, mask(STILLS_STEP) as i64]);
    }

    #[test]
    fn volumes_keep_their_place_by_number() {
        assert_eq!(volume_number("Film.part001.rar"), Some(1));
        assert_eq!(volume_number("film.PART12.RAR"), Some(12));
        assert_eq!(volume_number("film.rar"), Some(0));
        assert_eq!(volume_number("film.r00"), Some(1));
        assert_eq!(volume_number("film.s02"), Some(103));
        assert_eq!(volume_number("film.mp4"), None);
        let files: Vec<FileInfo> = [1, 2, 4].iter().map(|n| numbered(*n, &format!("x.part{n:02}.rar"), 10)).collect();
        let slots = volume_slots("x.part01.rar", files.iter().collect()).unwrap();
        assert_eq!(slots.iter().map(|s| s.map(|f| f.id)).collect::<Vec<_>>(), [Some(1), Some(2), None, Some(4)]);
        // the first one missing, or two of one number: no slots to trust
        assert!(volume_slots("x.part01.rar", files[1..].iter().collect()).is_none());
        let twice = [numbered(1, "x.part01.rar", 1), numbered(2, "x.part1.rar", 1)];
        assert!(volume_slots("x.part01.rar", twice.iter().collect()).is_none());
    }

    #[test]
    fn a_volume_missing_from_the_post_doesnt_shift_the_ones_after_it() {
        let (track, keyframe) = fixture_keyframe("hevc");
        let share = keyframe.len() / 2 + 333;
        let inner = mp4_file(&track, &keyframe, false);
        let (_, size, head) = mp4::box_head(&inner[16..]).unwrap();
        let moov_end = 16 + size.unwrap() as usize;
        let video = mp4::video_track(&inner[16 + head..moov_end]).unwrap();
        let wanted: Vec<(usize, usize)> = POSITIONS
            .iter()
            .map(|p| video.keyframe_at(*p).unwrap())
            .map(|s| (s.offset as usize, (s.offset + u64::from(s.size)) as usize))
            .chain([(0, moov_end)])
            .collect();
        let volumes = rar4_volumes(&inner, share);
        // a volume none of the stills needs, after the second
        let gone = (2..volumes.len())
            .find(|i| wanted.iter().all(|(a, b)| *b <= i * share || *a >= (i + 1) * share))
            .expect("a volume of filler");
        assert!(wanted.iter().any(|(a, _)| *a >= (gone + 1) * share), "a still is after the gap");
        let files: Vec<(FileInfo, Parts)> = volumes
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != gone)
            .map(|(i, v)| {
                let parts = posted(v, 1_500);
                (numbered(i as i64 + 1, &format!("x.part{:03}.rar", i + 1), parts.len()), parts)
            })
            .collect();
        let (stills, _) = mp4_stills(&files, true);
        assert_eq!(stills.iter().map(|s| s.0).collect::<Vec<_>>(), [25, 50, 75]);
    }

    #[test]
    fn a_part_out_of_place_or_short_is_refused() {
        let (track, keyframe) = fixture_keyframe("hevc");
        let inner = mp4_file(&track, &keyframe, false);
        let (_, size, head) = mp4::box_head(&inner[16..]).unwrap();
        let video = mp4::video_track(&inner[16 + head..16 + size.unwrap() as usize]).unwrap();
        let part_of = |p: u8| (video.keyframe_at(p).unwrap().offset as usize / 3_000) as i64 + 1;
        let mut parts = posted(&inner, 3_000);
        for (n, begin, body) in parts.iter_mut() {
            // the 25% keyframe's part says it starts a byte early, the 75% one's is cut short
            if *n == part_of(25) {
                *begin -= 1;
            }
            if *n == part_of(75) && *n != part_of(25) {
                body.truncate(10);
            }
        }
        let film = numbered(1, "film.mp4", parts.len());
        let (stills, _) = mp4_stills(&[(film, parts)], false);
        assert_eq!(stills.iter().map(|s| s.0).collect::<Vec<_>>(), [50]);
    }

    /// The main database in memory with shard 0 attached: releases, keys, probes and stills.
    fn shards() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::filters::register_sql(&conn).unwrap();
        conn.execute("attach ':memory:' as s0", []).unwrap();
        conn.execute_batch("create table s0.releases (id integer primary key, group_name text not null)").unwrap();
        for table in [&keys::RELEASE_KEYS, &super::super::PROBE, &STILLS] {
            conn.execute_batch(&table.create.replacen("exists ", "exists s0.", 1)).unwrap();
            for (column, kind) in table.added {
                let _ = conn.execute_batch(&format!("alter table s0.{} add column {column} {kind}", table.name));
            }
        }
        conn
    }

    /// A release in shard 0, probed with `media` (a Matroska SegmentUID, or none) and `moviehash`, with stills at
    /// `positions`, each kept on disk.
    fn release(conn: &rusqlite::Connection, id: i64, uid: Option<&str>, moviehash: Option<&str>, positions: &[u8]) {
        conn.execute("insert into s0.releases (id, group_name) values (?1, 'a.b.c')", [id]).unwrap();
        let media = uid.map(|u| crate::probe::MediaInfo {
            container: "matroska".into(),
            segment_uid: Some(u.into()),
            muxed_at: Some(1_700_000_000),
            ..Default::default()
        });
        let info = ProbeInfo { media, ..Default::default() };
        conn.execute(
            "insert into s0.release_probe (release_id, info, moviehash) values (?1, ?2, ?3)",
            rusqlite::params![id, serde_json::to_string(&info).unwrap(), moviehash],
        )
        .unwrap();
        let mux = file_lookup(conn, "s0", id).keys.mux;
        conn.execute("insert into s0.release_keys (release_id, mux) values (?1, ?2)", rusqlite::params![id, mux])
            .unwrap();
        for &p in positions {
            let sha = keep(format!("still {id} {p}").as_bytes()).unwrap();
            conn.execute(
                "insert into s0.release_stills (release_id, position, at_ms, sha256, width, height, phash)
                 values (?1, ?2, 0, ?3, 960, 540, 1)",
                rusqlite::params![id, p, sha],
            )
            .unwrap();
        }
    }

    #[test]
    fn a_release_with_the_same_video_file_shows_the_stills_already_made() {
        let conn = shards();
        // 0 has its stills; 8 is the same mux packed in a 7z, with none; 16 the same mux with only two
        release(&conn, 0, Some("ab12"), Some("1d9da161e916b2fd"), &POSITIONS);
        release(&conn, 8, Some("ab12"), None, &[]);
        release(&conn, 16, Some("ab12"), None, &[25, 50]);
        // 24 has only the OpenSubtitles hash of 0's file; 32 another file altogether
        release(&conn, 24, None, Some("1d9da161e916b2fd"), &[]);
        release(&conn, 32, Some("ffff"), Some("0000000000000001"), &[]);

        assert_eq!(file_lookup(&conn, "s0", 8).keys.mux, file_lookup(&conn, "s0", 0).keys.mux);
        assert!(file_lookup(&conn, "s0", 99).keys.mux.is_none(), "no probe, nothing to look by");
        assert_eq!(shown_stills(&conn, 0), Some((0, None)), "its own first");
        assert_eq!(shown_stills(&conn, 8), Some((0, Some(keys::Kind::Mux))));
        assert_eq!(shown_stills(&conn, 16), Some((16, None)), "its own, even short");
        assert_eq!(shown_stills(&conn, 24), Some((0, Some(keys::Kind::MovieHash))));
        assert_eq!(shown_stills(&conn, 32), None);
        // twenty newer releases of the same file that borrow (and so have none) don't hide the one they borrow from
        for n in 1..=20 {
            release(&conn, 32 + 8 * n, Some("ab12"), None, &[]);
        }
        assert_eq!(shown_stills(&conn, 8), Some((0, Some(keys::Kind::Mux))));
        assert_eq!(shown_stills(&conn, 32 + 8 * 20), Some((0, Some(keys::Kind::Mux))));
        // nor does a lender whose JPEGs were trimmed from the folder count
        let gone: String = conn
            .query_row("select sha256 from s0.release_stills where release_id = 0 and position = 50", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "update s0.release_stills set sha256 = ?1 where release_id = 0 and position = 50",
            [gone.replace(&gone[..1], if &gone[..1] == "0" { "1" } else { "0" })],
        )
        .unwrap();
        assert_eq!(shown_stills(&conn, 8), None);
        conn.execute("update s0.release_stills set sha256 = ?1 where release_id = 0 and position = 50", [&gone])
            .unwrap();
        assert_eq!(shown_stills(&conn, 8), Some((0, Some(keys::Kind::Mux))));
        // a release in a blocked group lends nothing
        conn.execute("update s0.releases set group_name = 'alt.binaries.pictures.erotica.teen' where id = 0", [])
            .unwrap();
        assert_eq!(shown_stills(&conn, 8), None);
    }
}
