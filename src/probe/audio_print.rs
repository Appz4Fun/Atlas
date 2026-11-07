//! Audio prints: a few seconds of a release's sound from a quarter, half and three quarters of the way in, as
//! Chromaprint hears it (see `audio`), kept for the releases no other key ties to another, soo the release page can
//! still say "the same sound is in 2 other releases": another encode of the same film, in another codec or
//! container, that nothing posted alongside it gives away.
//!
//! The articles are read from the middle of the file, as the stills read theirs: a file posted whole, or one stored
//! across RAR volumes. Each window starts at a time in the film, not at a share of its bytes, soo two encodes at
//! different bit rates read the same seconds: a first read lands near the time by the average bit rate, its first
//! cluster says where it really landed, and one or two more hops put it a little before. Only Matroska, and only
//! AAC-LC, FLAC, MP3, MP2, Vorbis, PCM, AC-3 and E-AC-3 tracks, which have a pure-Rust decoder. DTS, TrueHD and Opus
//! don't, and get no print.
//!
//! A print is a candidate for a reader, never a merge.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::audio::{self, decode, mkv};
use crate::config::Config;
use crate::keys::{self, Kind};
use crate::postprocess::schema::ShardTable;
use crate::postprocess::step::{Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId, StepWrite};
use crate::store::{self, FileInfo};

use super::ProbeInfo;
use super::step::main_file;
use super::stills::{Fetch, FileBytes, read, volume_slots};

pub const AUDIO_PRINT_STEP: StepId = StepId(41);

/// Where in the film the windows start, in percent of its length: the same places the stills are taken from.
pub const POSITIONS: [u8; 3] = [25, 50, 75];
/// How much sound a window keeps, at most and at least: 12 s is about 95 items, 8 s about 64.
const WINDOW_MS: i64 = 12_000;
const MIN_WINDOW_MS: i64 = 8_000;
/// A read lands this much before the window, to find a cluster start before it: Matroska muxers start a cluster
/// every few seconds.
const LEAD_MS: i64 = 3_000;
/// A landing this far after the window's start, or further before it than `MAX_BEHIND_MS`, hops.
const LATE_MS: i64 = 1_500;
const MAX_BEHIND_MS: i64 = 8_000;
const MAX_HOPS: usize = 2;
/// Sound decoded before a window and thrown away: a decoder's first frame or two are not the sound yet.
const WARMUP_MS: i64 = 300;
/// What a release's windows may fetch together, of the step's bodies: the front and the volume headers take a few.
const ARTICLES: i64 = 60;
/// The file's front read for its tracks.
const FRONT_BYTES: u64 = 1 << 20;
/// A film or episode shorter than this has too little sound between its titles.
const MIN_SECS: f64 = 120.0;
/// The other releases a print looks at, at most, per shard, before it compares their prints.
const CANDIDATES: i64 = 200;
/// Bytes read again from before a read's end, for a cluster's header cut in two by it.
const OVERLAP: usize = 64;
/// The most sound between two cluster starts looked through for one: muxers start one every 5 s at most.
const CLUSTER_MS: i64 = 7_000;

pub const AUDIO_PRINTS: ShardTable = ShardTable {
    name: "release_audio_print",
    create: "create table if not exists release_audio_print (
        release_id INTEGER NOT NULL,
        position   INTEGER NOT NULL,
        at_ms      INTEGER NOT NULL,
        items      BLOB NOT NULL,
        PRIMARY KEY (release_id, position)
    )",
    indexes: &[],
    release_key: Some("release_id"),
    added: &[],
};

/// The index the prints are found by: a few items of each, by value (`audio::anchors`).
pub const AUDIO_ANCHORS: ShardTable = ShardTable {
    name: "release_audio_anchor",
    create: "create table if not exists release_audio_anchor (
        release_id INTEGER NOT NULL,
        anchor     INTEGER NOT NULL,
        PRIMARY KEY (release_id, anchor)
    ) without rowid",
    indexes: &[(
        "release_audio_anchor_anchor",
        "create index if not exists release_audio_anchor_anchor on release_audio_anchor(anchor)",
    )],
    release_key: Some("release_id"),
    added: &[],
};

const DROP_PRINTS: &str = "delete from release_audio_print where release_id = ?1";
const DROP_ANCHORS: &str = "delete from release_audio_anchor where release_id = ?1";
const SET_PRINT: &str =
    "insert or replace into release_audio_print (release_id, position, at_ms, items) values (?1, ?2, ?3, ?4)";
const SET_ANCHOR: &str = "insert or ignore into release_audio_anchor (release_id, anchor) values (?1, ?2)";

/// Kinds that say who posted or packed it, not what: one uploader's account, host, archive owner or clock zone ties
/// together every film they post, soo sharing one doesn't make a print needless.
const WHO_POSTED: [Kind; 4] = [Kind::Account, Kind::Host, Kind::Owner, Kind::Zone];

// ---------------------------------------------------------------- finding the same sound

/// A release's prints, by position: (time in ms, items). `schema` is its shard as `conn` names it.
fn prints_in(conn: &Connection, schema: &str, id: i64) -> Vec<(i64, Vec<u32>)> {
    let sql = format!("select at_ms, items from {schema}.release_audio_print where release_id = ?1 order by position");
    let Ok(mut q) = conn.prepare_cached(&sql) else { return Vec::new() };
    q.query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))
        .map(|rows| rows.filter_map(Result::ok).map(|(at, blob)| (at, audio::from_blob(&blob))).collect())
        .unwrap_or_default()
}

/// Whether a release has a print, read from its own shard (`shard`, on its own or as `main`).
pub fn has_print(shard: &Connection, id: i64) -> bool {
    shard
        .prepare_cached("select 1 from release_audio_print where release_id = ?1 limit 1")
        .and_then(|mut q| q.exists([id]))
        .unwrap_or(false)
}

/// Two releases' prints are the same sound: as many windows as can be (two, or one when either has only one) each the
/// same sound as a different window of the other's, soo a tune heard twice in one release can't stand for two
/// matches, and the answer is the same whichever release asks.
pub fn prints_match(mine: &[(i64, Vec<u32>)], theirs: &[(i64, Vec<u32>)]) -> bool {
    let need = 2.min(mine.len()).min(theirs.len());
    let same: Vec<Vec<bool>> =
        mine.iter().map(|(_, a)| theirs.iter().map(|(_, b)| audio::same_sound(a, b)).collect()).collect();
    need > 0 && pairs(&same, 0, &mut vec![false; theirs.len()]) >= need
}

/// The most pairs of different windows, one of ours and one of theirs, that are the same sound, from our window `i`
/// on, with `taken` of theirs used: a handful of windows a side, soo every way is tried.
fn pairs(same: &[Vec<bool>], i: usize, taken: &mut Vec<bool>) -> usize {
    let Some(row) = same.get(i) else { return 0 };
    let mut best = pairs(same, i + 1, taken);
    for (j, hit) in row.iter().enumerate() {
        if *hit && !taken.get(j).copied().unwrap_or(true) {
            taken[j] = true;
            best = best.max(1 + pairs(same, i + 1, taken));
            taken[j] = false;
        }
    }
    best
}

/// The releases other than `own` with the same sound, over every shard of `conn` (the main database with the shards
/// attached), newest first, at most `limit`. Found by the anchor index, then each confirmed by comparing the prints.
/// Only releases the pages would show: none deleted or in a blocked group.
pub fn others(conn: &Connection, own: i64, limit: i64) -> Vec<i64> {
    let s = store::shard_of_id(own);
    let mine = prints_in(conn, &format!("s{s}"), own);
    if mine.is_empty() || limit <= 0 {
        return Vec::new();
    }
    let anchors: Vec<i64> = conn
        .prepare_cached(&format!("select anchor from s{s}.release_audio_anchor where release_id = ?1"))
        .and_then(|mut q| q.query_map([own], |r| r.get(0))?.collect())
        .unwrap_or_default();
    if anchors.is_empty() {
        return Vec::new();
    }
    let marks: Vec<String> = (0..anchors.len()).map(|i| format!("?{}", i + 3)).collect();
    let mut found = Vec::new();
    for shard in 0..store::SHARDS {
        let sql = format!(
            "select distinct a.release_id from s{shard}.release_audio_anchor a join s{shard}.releases r on r.id = a.release_id
             where a.anchor in ({}) and a.release_id != ?1 and not atlas_blocked(r.group_name)
             order by a.release_id desc limit ?2",
            marks.join(", ")
        );
        // a shard without the table yet has none
        let Ok(mut q) = conn.prepare(&sql) else { continue };
        let args = [own, CANDIDATES].into_iter().chain(anchors.iter().copied());
        let Ok(rows) = q.query_map(rusqlite::params_from_iter(args), |r| r.get::<_, i64>(0)) else { continue };
        let candidates: Vec<i64> = rows.filter_map(Result::ok).collect();
        for id in candidates {
            if prints_match(&mine, &prints_in(conn, &format!("s{shard}"), id)) {
                found.push(id);
            }
        }
    }
    // ids grow with time, within a shard and across them closely enough
    found.sort_unstable_by(|a, b| b.cmp(a));
    found.dedup();
    found.truncate(usize::try_from(limit).unwrap_or(0));
    found
}

/// The first key that already ties a release to another one, other than who posted it and its own print: a print
/// is only made when there's none. `conn` has every shard attached.
pub fn linked_by(conn: &Connection, own: i64, lookup: &keys::Lookup) -> Option<Kind> {
    Kind::ALL
        .into_iter()
        .filter(|k| *k != Kind::AudioPrint && !WHO_POSTED.contains(k) && lookup.has(*k))
        .find(|k| !keys::others(conn, own, lookup, *k, 1).is_empty())
}

/// The main database beside a shard's file (`atlas.s3.db` gives `atlas.db`): `store::shard_path`'s names.
pub(crate) fn main_of(shard: &str) -> Option<PathBuf> {
    let path = Path::new(shard);
    let stem = path.file_name()?.to_str()?.strip_suffix(".db")?;
    let (base, n) = stem.rsplit_once(".s")?;
    n.parse::<usize>().ok()?;
    Some(path.with_file_name(format!("{base}.db")))
}

// ---------------------------------------------------------------- reading the sound

/// What a release's prints are read from, by its probe.
#[derive(Clone, Debug, PartialEq)]
struct Plan {
    duration_ms: i64,
    /// the video file's size: the file posted, or the one stored in the RAR set
    size: u64,
    stored: bool,
    /// how much sound each window keeps
    window_ms: i64,
}

/// What prints a release can have, from its probe; or why it can have none, in the word the metrics count it under.
fn plan(info: &ProbeInfo) -> Result<Plan, &'static str> {
    let media = info.media.as_ref().ok_or("no_audio")?;
    if !matches!(media.container.as_str(), "matroska" | "webm") {
        return Err("container");
    }
    let track = media.audio.first().ok_or("no_audio")?;
    if let Some(why) = decode::why_not(&track.codec) {
        return Err(why);
    }
    let stored = match &info.archive {
        Some(a) if a.encrypted || a.method.as_deref() != Some("m0") => return Err("packed"),
        Some(_) => true,
        None => false,
    };
    let secs = media.duration_secs.filter(|d| d.is_finite() && *d >= MIN_SECS).ok_or("short")?;
    let size = if stored {
        info.archive.as_ref().and_then(|a| a.unpacked)
    } else {
        info.yenc.as_ref().and_then(|y| y.file_size)
    }
    .filter(|s| *s > 0)
    .ok_or("size")?;
    let segment = info.segment_size.filter(|s| *s > 0).ok_or("size")?;
    let duration_ms = (secs * 1000.0) as i64;
    // a window's articles: the lead and the warm-up before it, a cluster's worth of slack after, and the sound
    let per_window = (ARTICLES / POSITIONS.len() as i64 - 1 - MAX_HOPS as i64).max(1);
    let per_ms = size as f64 / duration_ms as f64;
    let fits = (per_window as f64 * segment as f64 / per_ms) as i64 - LEAD_MS - WARMUP_MS - 2_000;
    let window_ms = fits.min(WINDOW_MS);
    if window_ms < MIN_WINDOW_MS {
        return Err("bit_rate");
    }
    Ok(Plan { duration_ms, size, stored, window_ms })
}

/// The bytes of the window starting `target` ms in, read from just before it: what landed, cluster by cluster.
/// None when the articles ran out or don't sit where they should before any landing.
fn window_bytes(
    bytes: &mut FileBytes<'_>,
    fetch: &mut Fetch<'_>,
    track: &mkv::AudioTrack,
    plan: &Plan,
    target: i64,
    allowance: &dyn Fn() -> bool,
) -> Result<Option<Vec<u8>>, StepError> {
    let per_ms = plan.size as f64 / plan.duration_ms as f64;
    let at = |ms: i64| ((ms.max(0) as f64 * per_ms) as u64).min(plan.size);
    // half a second of bytes at a time: each part is fetched once and kept, soo the small reads cost no more
    let step = (per_ms * 500.0).max(64.0 * 1024.0) as u64;
    let mut from = at(target - LEAD_MS);
    for hop in 0..=MAX_HOPS {
        let mut data: Vec<u8> = Vec::new();
        let mut landed = None;
        // read on until a cluster says where this is
        while landed.is_none() && (data.len() as u64) < at(CLUSTER_MS).max(step) {
            if !allowance() {
                return Ok(None);
            }
            let x = from + data.len() as u64;
            if x >= plan.size {
                return Ok(None);
            }
            let Some(more) = bytes.range(fetch, x, (x + step).min(plan.size))? else { return Ok(None) };
            data.extend(more);
            landed = mkv::first_time(&data, track);
        }
        let Some(time) = landed else { return Ok(None) };
        let off = if time > target + LATE_MS {
            Some(-(time - target + LEAD_MS))
        } else if time < target - MAX_BEHIND_MS {
            Some(target - time - LEAD_MS)
        } else {
            None
        };
        match off {
            Some(ms) if hop < MAX_HOPS => {
                let moved = (ms as f64 * per_ms) as i64;
                from = u64::try_from(from as i64 + moved).unwrap_or(0).min(plan.size);
                continue;
            }
            Some(_) => return Ok(None),
            None => {}
        }
        // read on until a cluster starts after the window's end
        let end = target.max(time) + plan.window_ms;
        let mut seen = time;
        while seen < end {
            if !allowance() {
                break;
            }
            let x = from + data.len() as u64;
            if x >= plan.size {
                break;
            }
            let Some(more) = bytes.range(fetch, x, (x + step).min(plan.size))? else { break };
            let tail = data.len().saturating_sub(OVERLAP);
            data.extend(more);
            if let Some(t) = mkv::last_time(&data[tail..], track) {
                seen = seen.max(t);
            }
        }
        return Ok(Some(data));
    }
    Ok(None)
}

/// The print of one window's bytes: where it starts in ms, and its items. None when the sound there doesn't decode,
/// is too short, or is silence.
fn window_print(data: &[u8], track: &mkv::AudioTrack, target: i64, window_ms: i64) -> Option<(i64, Vec<u32>)> {
    let frames = mkv::frames(data, track);
    // decoded from the last block that starts by the warm-up: blocks are stored in time order
    let from = frames.iter().rposition(|f| f.lace == 0 && f.millis <= target - WARMUP_MS).unwrap_or(0);
    let frames = frames.get(from..)?;
    let refs: Vec<&[u8]> = frames.iter().map(|f| f.bytes.as_slice()).collect();
    // no more sound than the window needs from where the decode starts
    let lead = target.saturating_sub(frames.first()?.millis).max(0);
    let (rate, pcm) = decode::decode(track, &refs, lead.saturating_add(window_ms).saturating_add(1_000))?;
    let per_ms = f64::from(rate) / 1000.0;
    // when each frame starts: a block's time is its first frame's, and a laced frame after it starts where the
    // frames before it in the lace end
    let mut at = 0.0;
    let mut starts = Vec::with_capacity(pcm.len());
    for (frame, samples) in frames.iter().zip(&pcm) {
        if frame.lace == 0 {
            at = frame.millis as f64;
        }
        starts.push(at);
        at += samples.len() as f64 / per_ms;
    }
    // from the window's start to the sample, or from the second frame when it landed late: a decoder's first frame
    // is not the sound yet
    let want = usize::try_from(window_ms * i64::from(rate) / 1000).ok()?;
    let mut sound: Vec<i16> = Vec::with_capacity(want);
    let mut start = None;
    for (begin, samples) in starts.iter().zip(&pcm).skip(1) {
        let end = begin + samples.len() as f64 / per_ms;
        if end <= target as f64 || samples.is_empty() {
            continue;
        }
        let skip = if *begin < target as f64 { ((target as f64 - begin) * per_ms) as usize } else { 0 };
        start.get_or_insert((begin + skip as f64 / per_ms).round() as i64);
        sound.extend_from_slice(samples.get(skip..).unwrap_or_default());
        if sound.len() >= want {
            break;
        }
    }
    sound.truncate(want);
    if (sound.len() as i64) < MIN_WINDOW_MS * i64::from(rate) / 1000 {
        return None;
    }
    let items = audio::fingerprint(&sound, rate)?;
    audio::informative(&items).then_some((start?, items))
}

/// The windows of a release's sound, as (position, time in ms, items).
type Prints = Vec<(u8, i64, Vec<u32>)>;

fn read_prints(
    fetch: &mut Fetch<'_>,
    volumes: Vec<Option<&FileInfo>>,
    plan: &Plan,
    allowance: &dyn Fn() -> bool,
) -> Result<Option<Prints>, StepError> {
    let mut bytes = FileBytes::new(volumes, plan.stored);
    let Some(front) = bytes.range(fetch, 0, FRONT_BYTES.min(plan.size))? else { return Ok(None) };
    let Some(track) = mkv::audio_track(&front) else { return Ok(None) };
    let mut out = Vec::new();
    for position in POSITIONS {
        let target = plan.duration_ms * i64::from(position) / 100;
        let Some(data) = window_bytes(&mut bytes, fetch, &track, plan, target, allowance)? else { continue };
        if let Some((at, items)) = window_print(&data, &track, target, plan.window_ms) {
            out.push((position, at, items));
        }
    }
    Ok(Some(out))
}

// ---------------------------------------------------------------- the step

fn count(result: &'static str) {
    crate::metrics::names::AUDIO_PRINTS.inc(&[("result", result)], 1);
}

#[derive(Default)]
pub struct AudioPrintStep;

impl Step for AudioPrintStep {
    fn id(&self) -> StepId {
        AUDIO_PRINT_STEP
    }
    fn name(&self) -> &'static str {
        "audio_print"
    }
    fn rank(&self) -> u16 {
        610
    }
    fn needs(&self) -> Needs {
        Needs::Bodies
    }
    fn reruns(&self) -> Reruns {
        // a release that grows may have the articles a window needed
        Reruns { on_rename: false, on_growth: true }
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.archive.enabled
    }
    /// A film or an episode: small posts have no sound worth a print
    fn applies(&self, release: &ReleaseInfo) -> bool {
        release.size >= crate::postprocess::schema::SMALL_VIDEO
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let retry = |e: rusqlite::Error| StepError::Retry(e.to_string());
        let id = ctx.release().id;
        // a release printed before keeps its prints: nothing is fetched again
        if has_print(ctx.conn(), id) {
            return Ok(Outcome { matched: true, ..Outcome::default() });
        }
        let info: Option<String> =
            ctx.conn().query_row("select info from release_probe where release_id = ?1", [id], |r| r.get(0)).ok();
        let Some(info) = info.and_then(|i| serde_json::from_str::<ProbeInfo>(&i).ok()) else {
            return Ok(Outcome::default());
        };
        let plan = match plan(&info) {
            Ok(plan) => plan,
            Err(why) => {
                count(why);
                return Ok(Outcome::default());
            }
        };
        // nor is one kept in `atlas.raw.db` from before a shard reset, for the same files
        let (name, group) = (ctx.release().name.clone(), ctx.release().group.clone());
        let content_id = crate::raw_store::release_content_id(ctx.conn(), id).unwrap_or_default();
        if !crate::raw_store::is_explicit_rerun(ctx.conn(), id, AUDIO_PRINT_STEP) {
            let saved: Prints = crate::raw_store::get_audio_prints(&name, &group, &content_id)
                .into_iter()
                .map(|p| (p.position, p.at_ms, audio::from_blob(&p.items)))
                .filter(|(_, _, items)| !items.is_empty())
                .collect();
            if !saved.is_empty() {
                return Ok(Outcome { matched: true, writes: print_writes(id, &saved), ..Outcome::default() });
            }
        }
        // only for a release no other key ties to another
        let main = ctx.conn().path().and_then(main_of).ok_or_else(|| StepError::Retry("no main database".into()))?;
        let all = crate::db::open_with_shards(&main).map_err(retry)?;
        let lookup = keys::read_lookup(ctx.conn(), id, Some(ctx.release().parts), None).map_err(retry)?;
        if linked_by(&all, id, &lookup).is_some() {
            count("linked");
            return Ok(Outcome::default());
        }
        drop(all);
        let files = ctx.files().map_err(retry)?;
        let Some(first) = main_file(&files) else { return Ok(Outcome::default()) };
        let volumes = if plan.stored {
            match volume_slots(&first.filename, super::moviehash::volumes(&first.filename, &files)) {
                Some(slots) => slots,
                None => return Ok(Outcome::default()),
            }
        } else {
            vec![Some(first)]
        };
        let budget = ARTICLES.min(ctx.bodies_left());
        let fetched = Cell::new(0i64);
        let allowance = || fetched.get() < budget;
        let found = read_prints(
            &mut |file: &FileInfo, part: i64| {
                fetched.set(fetched.get() + 1);
                read(ctx, file, part)
            },
            volumes,
            &plan,
            &allowance,
        )?;
        let prints = found.unwrap_or_default();
        if prints.is_empty() {
            count("failed");
            return Ok(Outcome::default());
        }
        count("made");
        let saved: Vec<crate::raw_store::SavedPrint> = prints
            .iter()
            .map(|(position, at, items)| crate::raw_store::SavedPrint {
                position: *position,
                at_ms: *at,
                items: audio::to_blob(items),
            })
            .collect();
        crate::raw_store::put_audio_prints(&name, &group, &content_id, &saved);
        Ok(Outcome { matched: true, writes: print_writes(id, &prints), ..Outcome::default() })
    }
}

/// The writes that keep a release's prints, in place of any it had, and their anchors, made from the items.
fn print_writes(id: i64, prints: &Prints) -> Vec<StepWrite> {
    let mut writes = vec![StepWrite::new(DROP_PRINTS, vec![id.into()]), StepWrite::new(DROP_ANCHORS, vec![id.into()])];
    let mut anchors: Vec<i64> = Vec::new();
    for (position, at, items) in prints {
        anchors.extend(audio::anchors(items));
        writes.push(StepWrite::new(
            SET_PRINT,
            vec![id.into(), i64::from(*position).into(), (*at).into(), audio::to_blob(items).into()],
        ));
    }
    anchors.sort_unstable();
    anchors.dedup();
    writes.extend(anchors.into_iter().map(|a| StepWrite::new(SET_ANCHOR, vec![id.into(), a.into()])));
    writes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::tests::tune;
    use crate::probe::media::{AudioTrack, MediaInfo};

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/tests/fixtures/audio/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    /// The print of a fixture's sound from `target` ms on, read from its middle the way the step reads a window: from
    /// a byte part way into the file.
    fn print_of(name: &str, from_byte: usize, target: i64) -> Option<(i64, Vec<u32>)> {
        let file = fixture(name);
        let track = mkv::audio_track(&file).unwrap();
        window_print(&file[from_byte..], &track, target, 8_000)
    }

    #[test]
    fn the_same_sound_in_another_codec_matches_and_other_sound_doesnt() {
        let aac = fixture("tune.aac.mkv");
        let flac = fixture("tune.flac.mkv");
        // from about a fifth of the way in, a little after its start
        let (at_a, a) = print_of("tune.aac.mkv", aac.len() / 5, 3_000).unwrap();
        let (at_b, b) = print_of("tune.flac.mkv", flac.len() / 5, 3_200).unwrap();
        assert!((3_000..3_100).contains(&at_a) && (3_200..3_300).contains(&at_b), "{at_a} {at_b}");
        assert!(audio::informative(&a) && audio::informative(&b));
        let (bits, offset, _) = audio::best_alignment(&a, &b).unwrap();
        assert!(audio::same_sound(&a, &b), "AAC and FLAC of one tune: {bits} bits apart");
        assert!((1..=3).contains(&offset), "the FLAC window starts 200 ms later: {offset}");
        assert!(audio::anchors(&a).iter().any(|x| audio::anchors(&b).contains(x)), "an anchor in common");
        let other = fixture("other.aac.mkv");
        let (_, c) = print_of("other.aac.mkv", other.len() / 5, 3_000).unwrap();
        let (bits, _, _) = audio::best_alignment(&a, &c).unwrap();
        assert!(!audio::same_sound(&a, &c), "another tune: {bits} bits apart");
        assert!(!prints_match(&[(0, a.clone())], &[(0, c.clone())]));
        assert!(prints_match(&[(0, a.clone()), (1, a.clone())], &[(0, b.clone())]), "one window each: one is enough");
        // the same tune twice on one side matches one window of the other only once: not two matches
        let elsewhere = |seed| audio::fingerprint(&tune(seed, 12.0, 22_050), 22_050).unwrap();
        let mine = [(0, a.clone()), (1, a.clone()), (2, c.clone())];
        let theirs = [(0, b.clone()), (1, elsewhere(11)), (2, elsewhere(12))];
        assert!(!prints_match(&mine, &theirs) && !prints_match(&theirs, &mine));
        assert!(prints_match(&[(0, a.clone()), (1, c.clone())], &[(0, b.clone()), (1, c.clone())]));
        assert!(!prints_match(&[], &[(0, b)]));
    }

    #[cfg(feature = "ac3")]
    #[test]
    fn ac3_and_a_5_1_eac3_match_the_aac_encode() {
        let aac = fixture("tune.aac.mkv");
        let (_, a) = print_of("tune.aac.mkv", aac.len() / 5, 3_000).unwrap();
        let other = fixture("other.aac.mkv");
        let (_, c) = print_of("other.aac.mkv", other.len() / 5, 3_000).unwrap();
        for name in ["tune.ac3.mkv", "tune.eac3.mkv"] {
            let file = fixture(name);
            let (at, d) = print_of(name, file.len() / 5, 3_000).expect(name);
            assert!((3_000..3_100).contains(&at), "{name}: {at}");
            assert!(audio::informative(&d), "{name}");
            assert!(audio::same_sound(&a, &d), "{name}: {:?}", audio::best_alignment(&a, &d));
            assert!(!audio::same_sound(&c, &d), "{name} against another tune");
        }
    }

    #[test]
    fn vorbis_in_matroska_decodes_from_its_xiph_laced_headers() {
        // CodecPrivate is the packet count, the Xiph lengths and the three headers, as Matroska stores them
        let file = fixture("tune.vorbis.mkv");
        assert_eq!(mkv::audio_track(&file).unwrap().private.first(), Some(&2));
        let (_, v) = print_of("tune.vorbis.mkv", file.len() / 5, 3_000).unwrap();
        let aac = fixture("tune.aac.mkv");
        let (_, a) = print_of("tune.aac.mkv", aac.len() / 5, 3_000).unwrap();
        assert!(audio::same_sound(&a, &v), "{:?}", audio::best_alignment(&a, &v));
    }

    #[test]
    fn a_window_starts_on_time_inside_a_lace() {
        use crate::audio::mkv::tests::{cluster, simple};
        // PCM, four one-second frames to a fixed-laced block, a block every four seconds
        let rate = 8_000;
        let sound = tune(3, 16.0, rate);
        let track = mkv::AudioTrack {
            number: 1,
            codec: "A_PCM/INT/LIT".into(),
            sample_rate: Some(f64::from(rate)),
            channels: Some(1),
            bit_depth: Some(16),
            timestamp_scale: 1_000_000,
            ..Default::default()
        };
        let mut data = Vec::new();
        for block in 0..4 {
            let pcm: Vec<u8> =
                sound[block * 4 * 8_000..(block + 1) * 4 * 8_000].iter().flat_map(|s| s.to_le_bytes()).collect();
            data.extend(cluster(block as u64 * 4_000, &[simple(1, 0, 0x04, &[&[3u8][..], &pcm].concat())]));
        }
        let (at, print) = window_print(&data, &track, 3_000, 8_000).unwrap();
        assert_eq!(at, 3_000);
        assert_eq!(print, audio::fingerprint(&sound[3 * 8_000..11 * 8_000], rate).unwrap());
    }

    #[test]
    fn a_window_from_garbage_gives_nothing() {
        let file = fixture("tune.aac.mkv");
        let track = mkv::audio_track(&file).unwrap();
        let mut junk = file.clone();
        // every frame's bytes scrambled, the clusters left
        for (i, b) in junk.iter_mut().enumerate().skip(4096) {
            if i % 7 != 0 {
                *b = b.wrapping_mul(31).wrapping_add(i as u8);
            }
        }
        assert_eq!(window_print(&junk, &track, 3_000, 8_000).map(|(_, p)| audio::informative(&p)), None);
        assert_eq!(window_print(&[], &track, 0, 8_000), None);
        let mut dts = track.clone();
        dts.codec = "A_DTS".into();
        assert_eq!(window_print(&file, &dts, 3_000, 8_000), None, "no decoder, no print");
        let mut ac3 = track.clone();
        ac3.codec = "A_AC3".into();
        assert_eq!(window_print(&file, &ac3, 3_000, 8_000), None, "AAC frames are no AC-3 syncframes");
    }

    fn probe(codec: &str, container: &str, secs: f64) -> ProbeInfo {
        ProbeInfo {
            yenc: Some(super::super::yenc::YencLines { file_size: Some(2_000_000_000), ..Default::default() }),
            segment_size: Some(750_000),
            media: Some(MediaInfo {
                container: container.into(),
                duration_secs: Some(secs),
                audio: vec![AudioTrack { codec: codec.into(), ..Default::default() }],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn only_decodable_matroska_sound_at_a_fair_bit_rate_is_planned() {
        let p = plan(&probe("A_AAC", "matroska", 5_400.0)).unwrap();
        assert_eq!((p.duration_ms, p.size, p.stored, p.window_ms), (5_400_000, 2_000_000_000, false, WINDOW_MS));
        assert_eq!(plan(&probe("A_EAC3", "matroska", 5_400.0)).is_ok(), cfg!(feature = "ac3"));
        assert_eq!(plan(&probe("A_TRUEHD", "matroska", 5_400.0)), Err("truehd"));
        assert_eq!(plan(&probe("A_DTS", "matroska", 5_400.0)), Err("dts"));
        assert_eq!(plan(&probe("A_AAC", "mp4", 5_400.0)), Err("container"));
        assert_eq!(plan(&probe("A_AAC", "matroska", 60.0)), Err("short"));
        // 2 GB in 10 minutes is too much to read 8 s of in the budget
        assert_eq!(plan(&probe("A_AAC", "matroska", 600.0)), Err("bit_rate"));
        let mut packed = probe("A_FLAC", "matroska", 5_400.0);
        packed.archive = Some(super::super::ArchiveHead { method: Some("m3".into()), ..Default::default() });
        assert_eq!(plan(&packed), Err("packed"));
        packed.archive = Some(super::super::ArchiveHead {
            method: Some("m0".into()),
            unpacked: Some(3_000_000_000),
            ..Default::default()
        });
        assert_eq!(plan(&packed).map(|p| (p.stored, p.size)), Ok((true, 3_000_000_000)));
    }

    #[test]
    fn the_main_database_sits_beside_its_shards() {
        let main = Path::new("/data/atlas.db");
        for n in 0..store::SHARDS {
            assert_eq!(main_of(store::shard_path(main, n).to_str().unwrap()).as_deref(), Some(main));
        }
        assert_eq!(main_of("/data/atlas.db"), None);
        assert_eq!(main_of(""), None);
    }

    /// The main database in memory with shard 0 attached: releases, keys and prints.
    fn shards() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::filters::register_sql(&conn).unwrap();
        conn.execute("attach ':memory:' as s0", []).unwrap();
        conn.execute_batch("create table s0.releases (id integer primary key, group_name text not null)").unwrap();
        for table in [&keys::RELEASE_KEYS, &AUDIO_PRINTS, &AUDIO_ANCHORS] {
            let create = table.create.replacen("exists ", "exists s0.", 1);
            conn.execute_batch(&create).unwrap();
            for (column, kind) in table.added {
                // some are in `create` too
                let _ = conn.execute_batch(&format!("alter table s0.{} add column {column} {kind}", table.name));
            }
        }
        conn
    }

    fn print_rows(conn: &Connection, id: i64, prints: &[(u8, Vec<u32>)]) {
        conn.execute("insert into s0.releases (id, group_name) values (?1, 'a.b.c')", [id]).unwrap();
        for (position, items) in prints {
            conn.execute(
                "insert into s0.release_audio_print (release_id, position, at_ms, items) values (?1, ?2, 0, ?3)",
                rusqlite::params![id, position, audio::to_blob(items)],
            )
            .unwrap();
            for a in audio::anchors(items) {
                conn.execute(
                    "insert or ignore into s0.release_audio_anchor (release_id, anchor) values (?1, ?2)",
                    rusqlite::params![id, a],
                )
                .unwrap();
            }
        }
    }

    #[test]
    fn the_same_sound_is_found_through_the_anchors() {
        let conn = shards();
        let rate = 44_100;
        let sound = tune(7, 60.0, rate);
        let other = tune(8, 60.0, rate);
        let at = |s: &[i16], secs: f64| {
            let a = (secs * f64::from(rate)) as usize;
            audio::fingerprint(&s[a..a + 12 * rate as usize], rate).unwrap()
        };
        // 8 and 16 have the same sound as 0, read a little later; 24 has other sound
        print_rows(&conn, 0, &[(25, at(&sound, 10.0)), (50, at(&sound, 30.0))]);
        print_rows(&conn, 8, &[(25, at(&sound, 10.3)), (50, at(&sound, 30.7))]);
        print_rows(&conn, 16, &[(25, at(&sound, 9.8)), (50, at(&sound, 31.0))]);
        print_rows(&conn, 24, &[(25, at(&other, 10.0)), (50, at(&other, 30.0))]);
        assert_eq!(others(&conn, 0, 10), vec![16, 8]);
        assert_eq!(others(&conn, 0, 1), vec![16]);
        assert_eq!(others(&conn, 24, 10), Vec::<i64>::new());
        assert_eq!(others(&conn, 99, 10), Vec::<i64>::new(), "no print, no list");
        let lookup = keys::Lookup { audio_print: true, ..Default::default() };
        assert_eq!(keys::count_others(&conn, 0, &lookup, Kind::AudioPrint), 2);
        assert_eq!(keys::others(&conn, 8, &lookup, Kind::AudioPrint, 10), vec![16, 0]);
        // a release in a blocked group isn't listed
        conn.execute("update s0.releases set group_name = 'alt.binaries.pictures.erotica.teen' where id = 16", [])
            .unwrap();
        assert_eq!(others(&conn, 0, 10), vec![8]);
    }

    #[test]
    fn a_release_another_key_links_is_not_printed() {
        let conn = shards();
        for id in [0, 8, 16] {
            conn.execute("insert into s0.releases (id, group_name) values (?1, 'a.b.c')", [id]).unwrap();
        }
        // 0 and 8 have the same video stream; 0 and 16 the same posting account
        conn.execute("insert into s0.release_keys (release_id, video, account) values (0, 5, 7), (8, 5, null)", [])
            .unwrap();
        conn.execute("insert into s0.release_keys (release_id, account) values (16, 7)", []).unwrap();
        let mut lookup = keys::Lookup::default();
        lookup.keys.video = Some(5);
        lookup.keys.account = Some(7);
        assert_eq!(linked_by(&conn, 0, &lookup), Some(Kind::Video));
        // the account alone doesn't count: it is who posted, not what
        lookup.keys.video = None;
        assert_eq!(linked_by(&conn, 0, &lookup), None);
        // nor does a video key nobody else has
        lookup.keys.video = Some(6);
        assert_eq!(linked_by(&conn, 0, &lookup), None);
    }
}
