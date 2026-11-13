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

use crate::audio::{self, decode, mkv};
use crate::catalog_facts::FileInfo;
use crate::config::Config;
use crate::postprocess::step::{
    Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId, StepMutation, StepWrite,
};

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
/// Bytes read again from before a read's end, for a cluster's header cut in two by it.
const OVERLAP: usize = 64;
/// The most sound between two cluster starts looked through for one: muxers start one every 5 s at most.
const CLUSTER_MS: i64 = 7_000;

// ---------------------------------------------------------------- finding the same sound

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
        release.size >= crate::postprocess::rules::SMALL_VIDEO
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let id = ctx.release().id;
        // a release printed before keeps its prints: nothing is fetched again
        if ctx.data().has_audio_print(id)? {
            return Ok(Outcome { matched: true, ..Outcome::default() });
        }
        let info = ctx.data().probe_info_best_effort(id)?;
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
        let content_id = ctx.data().release_content_id(id)?.unwrap_or_default();
        if !ctx.data().explicit_rerun(id, AUDIO_PRINT_STEP)? {
            let saved: Prints = crate::raw_store::get_audio_prints_checked(&name, &group, &content_id).map_err(StepError::from_cache)?
                .into_iter()
                .map(|p| (p.position, p.at_ms, audio::from_blob(&p.items)))
                .filter(|(_, _, items)| !items.is_empty())
                .collect();
            if !saved.is_empty() {
                return Ok(Outcome { matched: true, writes: print_writes(id, &saved), ..Outcome::default() });
            }
        }
        // only for a release no other key ties to another
        let lookup = ctx.data().release_lookup(id, Some(ctx.release().parts))?;
        if ctx.data().audio_linked_by(id, &lookup)?.is_some() {
            count("linked");
            return Ok(Outcome::default());
        }
        let files = ctx.files().map_err(StepError::from_inventory)?;
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
        crate::raw_store::put_audio_prints_checked(&name, &group, &content_id, &saved).map_err(StepError::from_cache)?;
        Ok(Outcome { matched: true, writes: print_writes(id, &prints), ..Outcome::default() })
    }
}

/// The writes that keep a release's prints, in place of any it had, and their anchors, made from the items.
fn print_writes(id: i64, prints: &Prints) -> Vec<StepWrite> {
    let mut writes = vec![
        StepWrite::for_mutation(id, StepMutation::ClearAudioPrints),
        StepWrite::for_mutation(id, StepMutation::ClearAudioAnchors),
    ];
    let mut anchors: Vec<i64> = Vec::new();
    for (position, at, items) in prints {
        anchors.extend(audio::anchors(items));
        writes.push(StepWrite::for_mutation(
            id,
            StepMutation::KeepAudioPrint { position: *position, at_ms: *at, items: items.clone() },
        ));
    }
    anchors.sort_unstable();
    anchors.dedup();
    writes.extend(
        anchors.into_iter().map(|anchor| StepWrite::for_mutation(id, StepMutation::KeepAudioAnchor { anchor })),
    );
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
    #[ignore = "requires isolated PostgreSQL test database"]
    fn native_audio_links_require_content_keys_and_preserve_database_errors() {
        use crate::keys::{Kind, Lookup};
        use crate::pg::stage_reads::StepReads;
        let schema = crate::pg::testing::Schema::new().expect("PostgreSQL fixture required");
        let runtime = crate::pg::stage_queue_tests::runtime(&schema);
        let ids = runtime
            .owner
            .with(|client| {
                for n in 0..3 {
                    crate::pg::release_writer::save(
                        client,
                        &[crate::parser::Release {
                            name: format!("Audio.Example.{n}"),
                            group: "example.group".into(),
                            articles: vec![crate::parser::Article {
                                filename: Some("sound.mkv".into()),
                                message_id: format!("<audio-{n}@example.test>"),
                                part: Some(1),
                                total_parts: Some(1),
                                bytes: 1,
                                ..Default::default()
                            }],
                            ..Default::default()
                        }],
                        -10000,
                    )?;
                }
                Ok(client
                    .query("select id from releases order by name", &[])?
                    .into_iter()
                    .map(|r| r.get::<_, i64>(0))
                    .collect::<Vec<_>>())
            })
            .unwrap();
        assert_eq!(ids.len(), 3);
        runtime
            .owner
            .with(|client| {
                client.execute(
                    "insert into release_keys(release_id,video,account) values($1,5,7),($2,5,null),($3,null,7)",
                    &[&ids[0], &ids[1], &ids[2]],
                )?;
                Ok(())
            })
            .unwrap();
        let reads = StepReads::for_owner(&runtime.owner).unwrap();
        let mut lookup = Lookup::default();
        lookup.keys.video = Some(5);
        lookup.keys.account = Some(7);
        assert_eq!(reads.audio_linked_by(ids[0], &lookup).unwrap(), Some(Kind::Video));
        lookup.keys.video = None;
        assert_eq!(reads.audio_linked_by(ids[0], &lookup).unwrap(), None, "same account is not same content");
        lookup.keys.video = Some(6);
        assert_eq!(reads.audio_linked_by(ids[0], &lookup).unwrap(), None);
        runtime
            .owner
            .with(|client| {
                client.execute(
                    "update releases set group_name='alt.binaries.pictures.erotica.teen' where id=$1",
                    &[&ids[1]],
                )?;
                Ok(())
            })
            .unwrap();
        lookup.keys.video = Some(5);
        assert_eq!(reads.audio_linked_by(ids[0], &lookup).unwrap(), None, "blocked content is excluded");
        runtime
            .owner
            .with(|client| {
                client.batch_execute("drop table release_keys")?;
                Ok(())
            })
            .unwrap();
        let error = reads.audio_linked_by(ids[0], &lookup).expect_err("missing relation is not no link");
        assert!(error.chain().any(|e| e.is::<postgres::Error>()));
        runtime.owner.close();
    }
    #[test]
    #[ignore = "requires isolated PostgreSQL test database"]
    fn native_same_sound_is_found_through_real_anchors_and_checked_prints() {
        use crate::pg::fresh_reader::{Budget, ReadView};
        use std::time::{Duration, Instant};
        let schema = crate::pg::testing::Schema::new().expect("PostgreSQL fixture required");
        let runtime = crate::pg::stage_queue_tests::runtime(&schema);
        let ids = runtime
            .owner
            .with(|client| {
                for n in 0..4 {
                    crate::pg::release_writer::save(
                        client,
                        &[crate::parser::Release {
                            name: format!("Sound.Example.{n}"),
                            group: "example.group".into(),
                            articles: vec![crate::parser::Article {
                                filename: Some("sound.mkv".into()),
                                message_id: format!("<sound-{n}@example.test>"),
                                part: Some(1),
                                total_parts: Some(1),
                                bytes: 1,
                                ..Default::default()
                            }],
                            ..Default::default()
                        }],
                        -10000,
                    )?;
                }
                Ok(client
                    .query("select id from releases order by name", &[])?
                    .into_iter()
                    .map(|r| r.get::<_, i64>(0))
                    .collect::<Vec<_>>())
            })
            .unwrap();
        assert_eq!(ids.len(), 4);
        let rate = 44_100;
        let sound = tune(7, 60.0, rate);
        let other = tune(8, 60.0, rate);
        let at = |s: &[i16], secs: f64| {
            let a = (secs * f64::from(rate)) as usize;
            audio::fingerprint(&s[a..a + 12 * rate as usize], rate).unwrap()
        };
        let prints = [
            [(25, at(&sound, 10.0)), (50, at(&sound, 30.0))],
            [(25, at(&sound, 10.3)), (50, at(&sound, 30.7))],
            [(25, at(&sound, 9.8)), (50, at(&sound, 31.0))],
            [(25, at(&other, 10.0)), (50, at(&other, 30.0))],
        ];
        runtime.owner.with(|client| {
            for (id,windows) in ids.iter().zip(&prints) {
                for (position,items) in windows {
                    client.execute("insert into release_audio_print(release_id,position,at_ms,items) values($1,$2,0,$3)",&[id,&i64::from(*position),&audio::to_blob(items)])?;
                    for anchor in audio::anchors(items) {
                        client.execute("insert into release_audio_anchor(release_id,anchor) values($1,$2) on conflict do nothing",&[id,&anchor])?;
                    }
                }
            } Ok(())
        }).unwrap();
        let related = |own: i64| {
            runtime
                .owner
                .with(|client| {
                    let view = ReadView::open(
                        client,
                        &schema.name,
                        Budget::default(),
                        Instant::now() + Duration::from_secs(10),
                    )?;
                    Ok(view
                        .same_key(own, crate::keys::Kind::AudioPrint)?
                        .expect("own release")
                        .rows
                        .into_iter()
                        .map(|r| r.id)
                        .collect::<Vec<_>>())
                })
                .unwrap()
        };
        let mut expected = vec![ids[1], ids[2]];
        expected.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(related(ids[0]), expected);
        assert!(related(ids[3]).is_empty(), "another tune is not a matching release");
        runtime
            .owner
            .with(|client| {
                client.execute(
                    "update releases set group_name='alt.binaries.pictures.erotica.teen' where id=$1",
                    &[&ids[2]],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(related(ids[0]), vec![ids[1]]);
        runtime.owner.close();
    }
}
