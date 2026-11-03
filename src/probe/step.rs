//! The processing step: a few articles of a release, read whole.

use std::sync::Mutex;

use crate::archive::{self, crc32};
use crate::config::Config;
use crate::nntp::yenc_decode;
use crate::postprocess::step::{
    BodyRequest, Got, Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId, StepWrite, mask,
};
use crate::release_info;
use crate::store::{FileInfo, SegmentPick};

use super::{
    DROP_COVER, ProbeInfo, SET_COVER, SET_PROBE, agent_of, derive, imdb_ids, keep_headers, media, moviehash, nest,
    nfo_text, parse_sfv, xref_groups, yenc,
};

pub const PROBE_STEP: StepId = StepId(4);

/// The most text kept from an NFO.
const MAX_NFO: usize = 256 * 1024;

#[derive(Default)]
pub struct ProbeStep {
    on: Mutex<bool>,
}

fn is_par2(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".par2")
}

fn is_small_text(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".nfo") || lower.ends_with(".sfv") || lower.ends_with(".txt")
}

/// The file the probe's about: the one with the most parts, and of those, the first by name, soo a set of RAR
/// volumes gives its first volume.
pub(crate) fn main_file(files: &[FileInfo]) -> Option<&FileInfo> {
    let parts = |f: &FileInfo| f.expected.unwrap_or(f.parts_seen);
    let candidates: Vec<&FileInfo> =
        files.iter().filter(|f| !is_par2(&f.filename) && !is_small_text(&f.filename)).collect();
    let most = candidates.iter().map(|f| parts(f)).max()?;
    candidates.into_iter().filter(|f| parts(f) >= most.saturating_sub(1)).min_by_key(|f| first_volume_key(&f.filename))
}

/// `.rar` and `.part01.rar` and `.001` before the volumes after them.
fn first_volume_key(name: &str) -> (u8, String) {
    let lower = name.to_ascii_lowercase();
    let rank = if lower.ends_with(".rar") { 0 } else { 1 };
    (rank, lower)
}

pub(super) fn fetch_raw(ctx: &mut StepCtx<'_>, message_id: &str, file: &str) -> Result<Got, StepError> {
    let wanted = BodyRequest {
        message_id: message_id.to_string(),
        file: file.to_string(),
        max_bytes: Some(archive::ARTICLE_CAP),
    };
    Ok(ctx.fetch_article(&wanted)?.got)
}

/// Whether a server has it. None: this doesn't say, as when the article's too big to read, or the budget's gone.
fn there(got: &Got) -> Option<bool> {
    match got {
        Got::Body(_) | Got::TooBig => Some(true),
        Got::Missing => Some(false),
        Got::Refused(_) => None,
    }
}

impl Step for ProbeStep {
    fn id(&self) -> StepId {
        PROBE_STEP
    }
    fn name(&self) -> &'static str {
        "probe"
    }
    fn rank(&self) -> u16 {
        120
    }
    fn needs(&self) -> Needs {
        Needs::Bodies
    }
    fn enabled(&self, cfg: &Config) -> bool {
        *self.on.lock().unwrap_or_else(|e| e.into_inner()) = cfg.archive.enabled;
        cfg.archive.enabled
    }
    fn applies(&self, release: &ReleaseInfo) -> bool {
        !release.name.to_ascii_lowercase().ends_with(".jpg")
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: false, on_growth: true }
    }
    /// What a probe reads, a bit per version, soo a release probed before a version gets probed again. Bit 1:
    /// chapters, commentary, more than one article of a header, and the OpenSubtitles hash. Bit 2: archives inside
    /// archives, followed down. Bit 4: attachments, cover art, when it was muxed, MaxCLL and MaxFALL. Bit 8:
    /// SegmentUID, editions, default flags and the encoder's settings.
    fn config_stamp(&self) -> u64 {
        1 | 2 | 4 | 8
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let retry = |e: rusqlite::Error| StepError::Retry(e.to_string());
        let release_id = ctx.release().id;
        let rel_name = ctx.release().name.clone();
        let rel_group = ctx.release().group.clone();
        let rel_parts = ctx.release().parts;
        let rel_complete = ctx.release().complete;
        let stamp = self.config_stamp();
        let content_id = crate::raw_store::release_content_id(ctx.conn(), release_id).unwrap_or_default();
        if let Some(saved) =
            crate::raw_store::get_probe(&rel_name, &rel_group, &content_id, rel_parts, rel_complete, stamp).filter(
                |s| {
                    s.cover
                        .as_ref()
                        .is_none_or(|(_, sha)| super::cover_file(sha).is_some_and(|p| p.exists()))
                },
            )
        {
            let mut writes = Vec::new();
            if let Some(pw) = saved.nfo_password {
                writes.push(StepWrite::new(archive::store::SET_NFO_PASSWORD, vec![release_id.into(), pw.into()]));
            }
            writes.push(match saved.cover {
                Some((mime, sha256)) => {
                    StepWrite::new(SET_COVER, vec![release_id.into(), mime.into(), sha256.into()])
                }
                None => StepWrite::new(DROP_COVER, vec![release_id.into()]),
            });
            let hash = saved.moviehash.clone();
            writes.push(StepWrite::new(
                SET_PROBE,
                vec![release_id.into(), saved.crc32.into(), saved.agent.into(), saved.info.into(), hash.clone().into()],
            ));
            let rerun = if hash.is_some() { mask(super::opensubtitles::OPENSUBTITLES_STEP) } else { 0 };
            let facts = hash.map(|h| vec![(MOVIEHASH_FACT, h)]).unwrap_or_default();
            return Ok(Outcome { matched: true, writes, facts, rerun, ..Outcome::default() });
        }
        let files = ctx.files().map_err(retry)?;
        let mut info = ProbeInfo::default();
        let mut writes = Vec::new();
        let mut found_nfo_password: Option<String> = None;

        // the NFO and SFV come first, being the smallest: a budget that runs out still leaves them
        for file in files.iter().filter(|f| is_small_text(&f.filename)).take(3) {
            let Some(seg) = ctx.segments(file.id, &SegmentPick::First(1)).map_err(retry)?.into_iter().next() else {
                continue;
            };
            let Got::Body(raw) = fetch_raw(ctx, &seg.message_id, &file.filename)? else { continue };
            let article = yenc::split_article(&raw);
            let body = yenc_decode(&article.lines).unwrap_or_else(|| article.lines.join(&b'\n'));
            let text = nfo_text(&body);
            if file.filename.to_ascii_lowercase().ends_with(".sfv") {
                info.sfv = parse_sfv(&text);
            } else if info.nfo.is_none() {
                info.imdb_ids = imdb_ids(&text);
                if let Some(password) = release_info::find_password(&text) {
                    found_nfo_password = Some(password.clone());
                    writes.push(StepWrite::new(
                        archive::store::SET_NFO_PASSWORD,
                        vec![release_id.into(), password.into()],
                    ));
                }
                info.nfo = Some(text.chars().take(MAX_NFO).collect());
            }
        }

        let mut last_lines = None;
        if let Some(file) = main_file(&files) {
            info.file = Some(file.filename.clone());
            // only the real ends count: an incomplete file's earliest article isn't its first
            let first = archive::steps::first_article(ctx, file)?;
            let last = archive::steps::tail_articles(ctx, file)?.pop();
            let wire_bytes = first.as_ref().map_or(0, |f| f.bytes);
            let mut front = Vec::new();
            if let Some(first) = &first {
                let got = fetch_raw(ctx, &first.message_id, &file.filename)?;
                if let Some(there) = there(&got) {
                    info.health.insert("first".into(), there);
                }
                if let Got::Body(raw) = got {
                    let article = yenc::split_article(&raw);
                    info.headers = keep_headers(&article.headers);
                    if let Some(xref) = info.headers.get("Xref") {
                        info.crossposted_to = xref_groups(xref);
                    }
                    info.yenc = yenc::parse(&article.lines);
                    if let Some(body) = yenc_decode(&article.lines) {
                        if let Some(expected) = info.yenc.as_ref().and_then(|y| y.pcrc32) {
                            info.part_crc_ok = Some(crc32(&body) == expected);
                        }
                        front = body;
                        inspect(&front, &mut info);
                    }
                }
            }
            let total = file.expected.unwrap_or(file.parts_seen);
            if total > 2 {
                let middle = ctx.segments(file.id, &SegmentPick::Parts(vec![(total + 1) / 2])).map_err(retry)?;
                if let Some(middle) = middle.into_iter().next()
                    && let Some(there) = ctx.article_exists(&middle.message_id, &file.filename)?
                {
                    info.health.insert("middle".into(), there);
                }
            }
            let mut last_body = None;
            if let Some(last) = last.filter(|l| first.as_ref().is_none_or(|f| l.message_id != f.message_id)) {
                let got = fetch_raw(ctx, &last.message_id, &file.filename)?;
                if let Some(there) = there(&got) {
                    info.health.insert("last".into(), there);
                }
                if let Got::Body(raw) = got {
                    let article = yenc::split_article(&raw);
                    last_lines = yenc::parse(&article.lines);
                    last_body = yenc_decode(&article.lines);
                }
            }
            derive(&mut info, wire_bytes, file.expected, last_lines.as_ref());
            let known_last = last_part(&info, last_lines.as_ref(), last_body);
            info.moviehash = moviehash_of(ctx, file, &files, &front, &info, known_last.clone())?;
            // last, being the most it may read: a release's article budget goes to the checks above first
            if !front.is_empty() {
                let before = info.layers.clone();
                read_more_front(ctx, file, total, &mut front, &mut info)?;
                // more of the front can show a bigger file than the one hashed
                if info.layers != before {
                    info.moviehash = moviehash_of(ctx, file, &files, &front, &info, known_last)?;
                }
            }
        }

        let empty = info.yenc.is_none() && info.nfo.is_none() && info.sfv.is_empty() && info.health.is_empty();
        if empty {
            return Ok(Outcome { writes, ..Outcome::default() });
        }
        // a look that finds no cover drops one an earlier look kept
        let cover = info.media.as_mut().and_then(|m| m.cover.take());
        let saved_cover = cover.and_then(|c| super::keep_cover(&c.bytes).map(|sha256| (c.mime.to_string(), sha256)));
        writes.push(match &saved_cover {
            Some((mime, sha256)) => {
                StepWrite::new(SET_COVER, vec![release_id.into(), mime.clone().into(), sha256.clone().into()])
            }
            None => StepWrite::new(DROP_COVER, vec![release_id.into()]),
        });
        let crc = info.file_crc32.map(i64::from);
        let agent = agent_of(&info.headers).map(str::to_string);
        let hash = info.moviehash.clone();
        let json = serde_json::to_string(&info).map_err(|e| StepError::Permanent(e.to_string()))?;
        crate::raw_store::put_probe(&crate::raw_store::ProbeRecord {
            name: &rel_name,
            group_name: &rel_group,
            content_id: &content_id,
            crc32: crc,
            agent: agent.as_deref(),
            info: &json,
            moviehash: hash.as_deref(),
            parts: rel_parts,
            complete: rel_complete,
            config_stamp: stamp,
            nfo_password: found_nfo_password.as_deref(),
            cover: saved_cover.as_ref().map(|(m, s)| (m.as_str(), s.as_str())),
        });
        writes.push(StepWrite::new(
            SET_PROBE,
            vec![release_id.into(), crc.into(), agent.into(), json.into(), hash.clone().into()],
        ));
        // a hash found on a later look is asked about then too
        let rerun = if hash.is_some() { mask(super::opensubtitles::OPENSUBTITLES_STEP) } else { 0 };
        let facts = hash.map(|h| vec![(MOVIEHASH_FACT, h)]).unwrap_or_default();
        Ok(Outcome { matched: true, writes, facts, rerun, ..Outcome::default() })
    }
}

/// Articles read from the start of a file, at most, when its header runs past the first one: Matroska puts
/// attachments and chapters before the video, and a few MB of fonts is common.
const FRONT_ARTICLES: i64 = 6;

/// A later step's way to the hash this run found, before it's written.
pub const MOVIEHASH_FACT: &str = "moviehash";

/// More of the file's start, an article at a time, until the media header is all there.
fn read_more_front(
    ctx: &mut StepCtx<'_>,
    file: &FileInfo,
    total: i64,
    front: &mut Vec<u8>,
    info: &mut ProbeInfo,
) -> Result<(), StepError> {
    let mut part = 2;
    while info.media.as_ref().is_some_and(|m| !m.header_complete) && part <= total.min(FRONT_ARTICLES) {
        let Some(body) = read_part(ctx, file, part, front.len() as u64)? else { break };
        front.extend(body);
        inspect(front, info);
        part += 1;
    }
    // the encoder's settings are in the first frame, just past the header: one more article when they're not here yet
    let wants_encoder = info.media.as_ref().is_some_and(|m| {
        m.header_complete
            && !m.encoder_complete
            && m.video.iter().any(|v| {
                // Matroska's codec ids, and MP4's sample entry fourccs
                let c = v.codec.to_ascii_lowercase();
                ["avc", "hevc", "h264", "hvc1", "hev1"].iter().any(|k| c.contains(k))
            })
    });
    if wants_encoder
        && part <= total.min(FRONT_ARTICLES)
        && let Some(body) = read_part(ctx, file, part, front.len() as u64)?
    {
        front.extend(body);
        inspect(front, info);
    }
    Ok(())
}

/// One part of a file, decoded, when it holds the file's bytes from `offset` (0 based) on.
pub(super) fn read_part(
    ctx: &mut StepCtx<'_>,
    file: &FileInfo,
    part: i64,
    offset: u64,
) -> Result<Option<Vec<u8>>, StepError> {
    let segs = ctx.segments(file.id, &SegmentPick::Parts(vec![part])).map_err(|e| StepError::Retry(e.to_string()))?;
    let Some(seg) = segs.into_iter().next().filter(|s| s.part.is_none_or(|p| p == part)) else { return Ok(None) };
    let Got::Body(raw) = fetch_raw(ctx, &seg.message_id, &file.filename)? else { return Ok(None) };
    let article = yenc::split_article(&raw);
    if yenc::parse(&article.lines).and_then(|l| l.begin) != Some(offset + 1) {
        return Ok(None);
    }
    Ok(yenc_decode(&article.lines))
}

/// The last article already read, as (part, bytes), when it sits where its part number says.
fn last_part(info: &ProbeInfo, lines: Option<&yenc::YencLines>, body: Option<Vec<u8>>) -> Option<(i64, Vec<u8>)> {
    let segment = info.segment_size.filter(|s| *s > 0)?;
    let begin = lines?.begin?.checked_sub(1)?;
    if begin % segment != 0 {
        return None;
    }
    Some((i64::try_from(begin / segment).ok()? + 1, body?))
}

/// A file's bytes `from..to`, from the parts that hold them. `known` is a part already read.
fn file_range(
    ctx: &mut StepCtx<'_>,
    file: &FileInfo,
    segment: u64,
    from: u64,
    to: u64,
    known: Option<&(i64, Vec<u8>)>,
) -> Result<Option<Vec<u8>>, StepError> {
    let Some(parts) = moviehash::parts_for(segment, from, to) else { return Ok(None) };
    // a hash's end is 64 KiB: more than two parts means parts too small to be real
    if parts.end() - parts.start() > 1 {
        return Ok(None);
    }
    let start = (*parts.start() as u64 - 1) * segment;
    let mut out = Vec::new();
    for part in parts {
        let body = match known.filter(|(p, _)| *p == part) {
            Some((_, body)) => body.clone(),
            None => match read_part(ctx, file, part, (part as u64 - 1) * segment)? {
                Some(body) => body,
                None => return Ok(None),
            },
        };
        out.extend(body);
    }
    let (lo, hi) = (usize::try_from(from - start).ok(), usize::try_from(to - start).ok());
    Ok(lo.zip(hi).and_then(|(lo, hi)| out.get(lo..hi)).map(<[u8]>::to_vec))
}

/// The OpenSubtitles hash of the video: of the file itself, or of the one a stored RAR set holds, whose end is
/// in the set's last volume.
fn moviehash_of(
    ctx: &mut StepCtx<'_>,
    file: &FileInfo,
    files: &[FileInfo],
    front: &[u8],
    info: &ProbeInfo,
    known_last: Option<(i64, Vec<u8>)>,
) -> Result<Option<String>, StepError> {
    const CHUNK: u64 = moviehash::CHUNK as u64;
    if info.media.is_none() {
        return Ok(None);
    }
    // the walk follows the biggest file, which needn't be the first one the old way reads
    let other_file = info.layers.first().is_some_and(|l| info.archive.as_ref().is_none_or(|a| a.name != l.name));
    if info.layers.len() > 1 || other_file {
        return chain_moviehash(ctx, file, files, front, info.segment_size, known_last);
    }
    let Some(head) = &info.archive else {
        let Some(size) = info.yenc.as_ref().and_then(|y| y.file_size) else { return Ok(None) };
        if size < 2 * CHUNK {
            return Ok(None);
        }
        // posted whole in one article: both ends are already here
        if front.len() as u64 == size {
            return Ok(moviehash::moviehash(size, front, front));
        }
        let Some(segment) = info.segment_size else { return Ok(None) };
        let tail = file_range(ctx, file, segment, size - CHUNK, size, known_last.as_ref())?;
        return Ok(tail.and_then(|tail| moviehash::moviehash(size, front, &tail)));
    };
    let (Some(at), Some(size), Some(name)) = (head.stored_at, head.unpacked, head.name.as_deref()) else {
        return Ok(None);
    };
    let Some(inner_front) = front.get(at..) else { return Ok(None) };
    if size < 2 * CHUNK {
        return Ok(None);
    }
    let Some(volume) = moviehash::last_volume(&file.filename, files) else { return Ok(None) };
    let tail = if volume.id == file.id {
        // one volume: the file ends where its stored bytes do
        let (Some(packed), Some(segment)) = (head.packed, info.segment_size) else { return Ok(None) };
        let end = at as u64 + packed;
        // the volumes after it aren't indexed
        if packed < CHUNK || head.continues {
            return Ok(None);
        }
        file_range(ctx, file, segment, end - CHUNK, end, known_last.as_ref())?
    } else {
        let Some(first) = archive::steps::first_article(ctx, volume)? else { return Ok(None) };
        let Got::Body(raw) = fetch_raw(ctx, &first.message_id, &volume.filename)? else { return Ok(None) };
        let article = yenc::split_article(&raw);
        let lines = yenc::parse(&article.lines);
        let (Some(1), Some(end_byte)) = (lines.as_ref().and_then(|l| l.begin), lines.as_ref().and_then(|l| l.end))
        else {
            return Ok(None);
        };
        let Some(body) = yenc_decode(&article.lines) else { return Ok(None) };
        let Some(last_head) = super::archive::parse(&body) else { return Ok(None) };
        let (Some(last_at), Some(packed)) = (last_head.stored_at, last_head.packed) else { return Ok(None) };
        // the same file, still stored, and its last piece holds a whole 64 KiB,
        // and this is its end: the last volume indexed needn't be the set's last
        if last_head.name.as_deref() != Some(name) || packed < CHUNK || last_head.continues {
            return Ok(None);
        }
        let end = last_at as u64 + packed;
        file_range(ctx, volume, end_byte, end - CHUNK, end, Some(&(1, body)))?
    };
    Ok(tail.and_then(|tail| moviehash::moviehash(size, inner_front, &tail)))
}

/// The hash of a video under more than one layer, or under a tar or the like: only when every layer is stored,
/// soo the video's end is a known place in this one file, and the file is all there is.
fn chain_moviehash(
    ctx: &mut StepCtx<'_>,
    file: &FileInfo,
    files: &[FileInfo],
    front: &[u8],
    segment: Option<u64>,
    known_last: Option<(i64, Vec<u8>)>,
) -> Result<Option<String>, StepError> {
    const CHUNK: u64 = moviehash::CHUNK as u64;
    let nest = nest::unwrap(front);
    let (Some(at), Some(last)) = (nest.outer_at, nest.layers.last()) else { return Ok(None) };
    if nest.layers.iter().any(|l| !l.stored || l.continues || l.stop.is_some()) {
        return Ok(None);
    }
    let Some(size) = last.unpacked.filter(|s| *s >= 2 * CHUNK) else { return Ok(None) };
    // the volumes after this one would hold the end: not followed yet
    if moviehash::last_volume(&file.filename, files).is_some_and(|v| v.id != file.id) {
        return Ok(None);
    }
    let end = at as u64 + size;
    let tail = match usize::try_from(end).ok().and_then(|e| front.get(e - moviehash::CHUNK..e)) {
        Some(tail) => Some(tail.to_vec()),
        None => match segment {
            Some(segment) => file_range(ctx, file, segment, end - CHUNK, end, known_last.as_ref())?,
            None => None,
        },
    };
    Ok(tail.and_then(|tail| moviehash::moviehash(size, front.get(at..)?, &tail)))
}

/// What the first bytes are. A file in an archive, or in an archive in that, and soo on down, is read as itself.
fn inspect(body: &[u8], info: &mut ProbeInfo) {
    info.archive = super::archive::parse(body);
    let nest = nest::unwrap(body);
    info.media = media::parse(&nest.inner);
    info.layers = nest.layers;
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
    fn the_main_file_is_the_big_one_and_the_first_volume() {
        let files = [
            file("a.par2", 1),
            file("a.nfo", 1),
            file("a.r00", 100),
            file("a.rar", 100),
            file("a.r01", 100),
            file("a.r02", 40),
        ];
        assert_eq!(main_file(&files).unwrap().filename, "a.rar");
        let raw = [file("movie.mkv", 3000), file("sample.mkv", 10)];
        assert_eq!(main_file(&raw).unwrap().filename, "movie.mkv");
        assert!(main_file(&[file("a.nfo", 1)]).is_none());
    }
}
