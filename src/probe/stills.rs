//! Stills: a keyframe from a quarter, half and three quarters of the way into a release's video, decoded by
//! our own HEVC or H.264 decoder and kept as JPEGs. The articles are read from the middle of the file, a few each.

use sha2::Digest;

use crate::config::Config;
use crate::postprocess::schema::ShardTable;
use crate::postprocess::step::{Got, Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId, StepWrite};
use crate::store::{self, FileInfo, SegmentPick};
use crate::video::mkv::{self, Codec, Found, VideoTrack};
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
    added: &[],
};

const DROP_STILLS: &str = "delete from release_stills where release_id = ?1";
const SET_STILL: &str = "insert or replace into release_stills (release_id, position, at_ms, sha256, width, height)
    values (?1, ?2, ?3, ?4, ?5, ?6)";

/// Past this, the oldest stills go. One is 50 to 150 KB.
const MAX_STILL_CACHE: u64 = 2 << 30;
const TRIM_EVERY: u64 = 64;

fn still_dir() -> std::path::PathBuf {
    crate::paths::app_dir().join("stills")
}

fn still_file(sha256: &str) -> Option<std::path::PathBuf> {
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
fn read(ctx: &mut StepCtx<'_>, file: &FileInfo, part: i64) -> Result<Option<(u64, Vec<u8>)>, StepError> {
    let retry = |e: rusqlite::Error| StepError::Retry(e.to_string());
    let segs = ctx.segments(file.id, &SegmentPick::Parts(vec![part])).map_err(retry)?;
    let Some(seg) = segs.into_iter().next().filter(|s| s.part.is_none_or(|p| p == part)) else { return Ok(None) };
    let Got::Body(raw) = fetch_raw(ctx, &seg.message_id, &file.filename)? else { return Ok(None) };
    let article = yenc::split_article(&raw);
    let Some(begin) = yenc::parse(&article.lines).and_then(|l| l.begin) else { return Ok(None) };
    Ok(yenc_decode(&article.lines).map(|body| (begin.saturating_sub(1), body)))
}

/// A keyframe from part `part` on: the frame's bytes and its time.
fn keyframe_from(
    ctx: &mut StepCtx<'_>,
    file: &FileInfo,
    part: i64,
    track: u64,
) -> Result<Option<(Vec<u8>, i64)>, StepError> {
    let Some((begin, mut data)) = read(ctx, file, part)? else { return Ok(None) };
    let mut end = begin + data.len() as u64;
    for next in part + 1..=part + MORE_ARTICLES as i64 {
        match mkv::find_keyframe(&data, track) {
            Found::Frame { start, len, timestamp } => return Ok(Some((data[start..start + len].to_vec(), timestamp))),
            Found::Partial { start, len, .. } if start + len - data.len() > MORE_ARTICLES * data.len() => break,
            _ if next > parts(file) => break,
            _ => {}
        }
        // the next part has to carry straight on, or the bytes are no use
        let Some((from, more)) = read(ctx, file, next)? else { break };
        if from != end {
            break;
        }
        end += more.len() as u64;
        data.extend(more);
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

/// A still from a keyframe: JPEG bytes, width and height.
fn still_of(track: &VideoTrack, frame: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
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
    let [y, cb, cr] = picture.to_8bit(STILL_WIDTH);
    let jpeg = jpeg::encode(&y.data, &cb.data, &cr.data, y.width, y.height, QUALITY);
    Some((jpeg, y.width as u32, y.height as u32))
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
    fn applies(&self, release: &ReleaseInfo) -> bool {
        // a video worth a still is at least this big
        release.size >= 100 << 20
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let retry = |e: rusqlite::Error| StepError::Retry(e.to_string());
        let release_id = ctx.release().id;
        let info: Option<String> = ctx
            .conn()
            .query_row("select info from release_probe where release_id = ?1", [release_id], |r| r.get(0))
            .ok();
        let Some(info) = info.and_then(|i| serde_json::from_str::<ProbeInfo>(&i).ok()) else {
            return Ok(Outcome::default());
        };
        // only video we can decode: HEVC or H.264 in Matroska, for now
        let media = info.media.as_ref();
        let wanted = media.is_some_and(|m| {
            matches!(m.container.as_str(), "matroska" | "webm")
                && m.video.first().is_some_and(|v| matches!(v.codec.as_str(), "V_MPEGH/ISO/HEVC" | "V_MPEG4/ISO/AVC"))
        });
        if !wanted || info.archive.as_ref().is_some_and(|a| a.encrypted || a.method.as_deref() != Some("m0")) {
            return Ok(Outcome::default());
        }
        let files = ctx.files().map_err(retry)?;
        let Some(first) = main_file(&files) else { return Ok(Outcome::default()) };
        let Some((track, stored)) = track_of(ctx, first)? else { return Ok(Outcome::default()) };
        let layout = if stored {
            let volumes = moviehash::volumes(&first.filename, &files);
            if volumes.is_empty() {
                return Ok(Outcome::default());
            }
            Layout::Stored(volumes)
        } else {
            Layout::Plain(first)
        };
        let mut writes = vec![StepWrite::new(DROP_STILLS, vec![release_id.into()])];
        let mut made = 0;
        for percent in POSITIONS {
            let Some((file, part)) = part_at(&layout, percent) else { continue };
            let Some((frame, at)) = keyframe_from(ctx, file, part, track.number)? else { continue };
            let Some((jpeg, w, h)) = still_of(&track, &frame) else { continue };
            let Some(sha256) = keep(&jpeg) else { continue };
            made += 1;
            writes.push(StepWrite::new(
                SET_STILL,
                vec![
                    release_id.into(),
                    i64::from(percent).into(),
                    track.millis(at).into(),
                    sha256.into(),
                    i64::from(w).into(),
                    i64::from(h).into(),
                ],
            ));
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
    fn a_keyframe_makes_a_jpeg_still() {
        let data = std::fs::read(format!("{}/tests/fixtures/hevc/clip.mkv", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let track = mkv::video_track(&data).unwrap();
        let chunk = &data[data.len() / 2..];
        let Found::Frame { start, len, .. } = mkv::find_keyframe(chunk, track.number) else { panic!() };
        let (jpeg, w, h) = still_of(&track, &chunk[start..start + len]).unwrap();
        assert_eq!((w, h), (320, 180));
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
    }
}
