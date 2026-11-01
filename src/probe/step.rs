//! The processing step: a few articles of a release, read whole.

use std::sync::Mutex;

use crate::archive::{self, crc32};
use crate::config::Config;
use crate::nntp::yenc_decode;
use crate::postprocess::step::{
    BodyRequest, Got, Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId, StepWrite,
};
use crate::release_info;
use crate::store::{FileInfo, SegmentPick};

use super::{
    ProbeInfo, SET_PROBE, agent_of, derive, imdb_ids, keep_headers, media, nfo_text, parse_sfv, xref_groups, yenc,
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
fn main_file(files: &[FileInfo]) -> Option<&FileInfo> {
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

fn fetch_raw(ctx: &mut StepCtx<'_>, message_id: &str, file: &str) -> Result<Got, StepError> {
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
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let retry = |e: rusqlite::Error| StepError::Retry(e.to_string());
        let files = ctx.files().map_err(retry)?;
        let release_id = ctx.release().id;
        let mut info = ProbeInfo::default();
        let mut writes = Vec::new();

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
                        inspect(&body, &mut info);
                    }
                }
            }
            let total = file.expected.unwrap_or(file.parts_seen);
            if total > 2 {
                let middle = ctx.segments(file.id, &SegmentPick::Parts(vec![total / 2])).map_err(retry)?;
                if let Some(middle) = middle.into_iter().next()
                    && let Some(there) = ctx.article_exists(&middle.message_id, &file.filename)?
                {
                    info.health.insert("middle".into(), there);
                }
            }
            if let Some(last) = last.filter(|l| first.as_ref().is_none_or(|f| l.message_id != f.message_id)) {
                let got = fetch_raw(ctx, &last.message_id, &file.filename)?;
                if let Some(there) = there(&got) {
                    info.health.insert("last".into(), there);
                }
                if let Got::Body(raw) = got {
                    last_lines = yenc::parse(&yenc::split_article(&raw).lines);
                }
            }
            derive(&mut info, wire_bytes, file.expected, last_lines.as_ref());
        }

        let empty = info.yenc.is_none() && info.nfo.is_none() && info.sfv.is_empty() && info.health.is_empty();
        if empty {
            return Ok(Outcome { writes, ..Outcome::default() });
        }
        let crc = info.file_crc32.map(i64::from);
        let agent = agent_of(&info.headers).map(str::to_string);
        let json = serde_json::to_string(&info).map_err(|e| StepError::Permanent(e.to_string()))?;
        writes.push(StepWrite::new(SET_PROBE, vec![release_id.into(), crc.into(), agent.into(), json.into()]));
        Ok(Outcome { matched: true, writes, ..Outcome::default() })
    }
}

/// What the first bytes are. A stored file in an archive is read as itself.
fn inspect(body: &[u8], info: &mut ProbeInfo) {
    info.media = media::parse(body);
    if let Some(head) = super::archive::parse(body) {
        if let Some(inner) = head.stored_at.and_then(|at| body.get(at..)) {
            info.media = media::parse(inner);
        }
        info.archive = Some(head);
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
