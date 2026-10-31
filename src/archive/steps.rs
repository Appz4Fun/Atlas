//! Processing steps that read a release's archives, PAR2 sets and NFOs.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use rusqlite::params;
use rusqlite::types::Value;

use crate::config::{ArchiveConfig, Config};
use crate::nfo;
use crate::par2;
use crate::postprocess::step::{
    BodyRequest, Got, NameChange, Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId, StepWrite,
};
use crate::store::{self, FileInfo, SegmentPick, SegmentRef};

use super::names::{best_name, collapse_volumes, looks_random, stem};
use super::select::{self, Head, Kind};
use super::store::{self as archive_store, SOURCE_7Z, SOURCE_PAR2, SOURCE_RAR, SOURCE_ZIP};
use super::{
    ARTICLE_CAP, Format, InnerFile, Listing, NAME_SOURCE_INNER, NAME_SOURCE_NFO, NAME_SOURCE_PAR2, PROBE_PREFIX,
    RERUN_ON_INNER_FILES, identify, rar, sevenz, zip,
};

fn retry(e: impl std::fmt::Display) -> StepError {
    StepError::Retry(e.to_string())
}

/// a release named as an image keeps its file's name, soo these steps leave it be
fn named_as_an_image(release: &ReleaseInfo) -> bool {
    select::classify(&release.name) == Kind::Image
}

/// A file's first article. None: it was never indexed, and a fetch would read the wrong bytes.
fn first_article(ctx: &StepCtx<'_>, file: &FileInfo) -> Result<Option<SegmentRef>, StepError> {
    let seg = ctx.segments(file.id, &SegmentPick::First(1)).map_err(retry)?.into_iter().next();
    Ok(seg.filter(|s| s.part.is_none_or(|p| p <= 1)))
}

/// A file's last articles, last one last. Empty when its last isnt indexed.
fn tail_articles(ctx: &StepCtx<'_>, file: &FileInfo) -> Result<Vec<SegmentRef>, StepError> {
    let segs = ctx.segments(file.id, &SegmentPick::Last(2)).map_err(retry)?;
    let last_is_there = match (file.expected, segs.last().and_then(|s| s.part)) {
        (Some(expected), Some(part)) => part == expected,
        (Some(_), None) => false,
        (None, _) => true,
    };
    Ok(if last_is_there { segs } else { Vec::new() })
}

/// One whole article, within every budget. None: skip this candidate.
fn fetch_one(ctx: &mut StepCtx<'_>, seg: &SegmentRef, file: &str) -> Result<Option<Vec<u8>>, StepError> {
    let wanted =
        [BodyRequest { message_id: seg.message_id.clone(), file: file.to_string(), max_bytes: Some(ARTICLE_CAP) }];
    Ok(ctx.fetch_bodies(&wanted)?.into_iter().next().and_then(|b| match b.got {
        Got::Body(body) => Some(body),
        Got::Missing | Got::TooBig | Got::Refused(_) => None,
    }))
}

fn fetch_start(ctx: &mut StepCtx<'_>, seg: &SegmentRef, file: &str) -> Result<Option<Vec<u8>>, StepError> {
    let wanted =
        BodyRequest { message_id: seg.message_id.clone(), file: file.to_string(), max_bytes: Some(PROBE_PREFIX) };
    Ok(match ctx.fetch_prefix(&wanted)?.got {
        Got::Body(body) => Some(body),
        Got::Missing | Got::TooBig | Got::Refused(_) => None,
    })
}

fn listing_of(inventory: par2::Inventory) -> Listing {
    let complete = inventory.complete;
    let files = inventory.files.into_iter().map(|f| InnerFile { name: f.name, size: Some(f.size) }).collect();
    Listing { files, complete, ..Default::default() }
}

fn name_may_change(release: &ReleaseInfo, source: u8) -> bool {
    release.display_name.is_none() || release.name_source == Some(i64::from(source))
}

/// What a read comes to: its rows, unless the shard's names are over their total,
/// and a name when the release has none better.
fn finish(ctx: &StepCtx<'_>, source: i64, listing: Listing, limits: &ArchiveConfig) -> Result<Outcome, StepError> {
    let release = ctx.release();
    let id = release.id;
    let mut writes = Vec::new();
    let rows = collapse_volumes(&listing.files);
    let old: i64 = ctx
        .conn()
        .prepare_cached(archive_store::BYTES_OF_SOURCE)
        .and_then(|mut stmt| stmt.query_row(params![id, source], |r| r.get(0)))
        .map_err(retry)?;
    let used = store::get_meta(ctx.conn(), "archive_name_bytes").map_err(retry)?.unwrap_or(0);
    let quota = limits.max_names_mb.saturating_mul(1 << 20) / store::SHARDS as i64;
    let mut stored = false;
    // what this release already holds comes back when it's replaced
    if rows.is_empty() || used.saturating_sub(old) < quota {
        writes.push(StepWrite::new(archive_store::DELETE_SOURCE, vec![id.into(), source.into()]));
        let mut bytes = 0i64;
        for (i, row) in rows.iter().enumerate() {
            let size = row.size.and_then(|s| i64::try_from(s).ok());
            let params: Vec<Value> = vec![
                id.into(),
                (source * 65_536 + i as i64).into(),
                row.name.clone().into(),
                size.into(),
                source.into(),
            ];
            writes.push(StepWrite::new(archive_store::INSERT_FILE, params));
            bytes += row.name.len() as i64 + 32;
            stored = true;
        }
        if bytes != old {
            writes.push(StepWrite::new(archive_store::ADD_NAME_BYTES, vec![(bytes - old).into()]));
        }
    } else {
        // past the total: what's there stays, and this list goes unstored
        writes.push(StepWrite::new(archive_store::COUNT_OVER_QUOTA, vec![]));
    }
    if source != SOURCE_PAR2 {
        writes.push(StepWrite::new(archive_store::UPSERT_RELEASE, vec![id.into(), listing.password().code().into()]));
    }

    let name_source = if source == SOURCE_PAR2 { NAME_SOURCE_PAR2 } else { NAME_SOURCE_INNER };
    let name_wanted = name_may_change(release, name_source)
        && (source == SOURCE_PAR2 || release.is_obfuscated || looks_random(stem(&release.name)));
    let name = name_wanted
        .then(|| best_name(listing.files.iter().map(|f| (f.name.as_str(), f.size.unwrap_or(0)))))
        .flatten()
        .map(|n| NameChange { display_name: Some(n), obfuscated: Some(release.is_obfuscated), source: name_source });

    Ok(Outcome {
        matched: stored || listing.password() == super::Password::Yes,
        writes,
        name,
        rerun: if stored { RERUN_ON_INNER_FILES } else { 0 },
        ..Default::default()
    })
}

macro_rules! limits_from_enabled {
    () => {
        fn enabled(&self, cfg: &Config) -> bool {
            *self.limits.lock().unwrap() = cfg.archive.clone();
            cfg.archive.enabled
        }
    };
}

/// Reads a PAR2 set's file list.
#[derive(Default)]
pub struct Par2Step {
    limits: Mutex<ArchiveConfig>,
}

impl Step for Par2Step {
    fn id(&self) -> StepId {
        StepId(1)
    }
    fn name(&self) -> &'static str {
        "archive_par2"
    }
    fn rank(&self) -> u16 {
        100
    }
    fn needs(&self) -> Needs {
        Needs::Bodies
    }
    limits_from_enabled!();
    fn applies(&self, release: &ReleaseInfo) -> bool {
        !named_as_an_image(release)
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: false, on_growth: true }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let files = ctx.files().map_err(retry)?;
        let Some(file) = select::par2_source(&files) else { return Ok(Outcome::default()) };
        let is_base = select::classify(&file.filename) == Kind::Par2Base;
        let Some(seg) = first_article(ctx, file)? else { return Ok(Outcome::default()) };
        let Some(mut body) = fetch_one(ctx, &seg, &file.filename)? else { return Ok(Outcome::default()) };
        let mut inventory = par2::inventory(&body);
        if !inventory.complete {
            // a base's next article, or a volume's last: a volume can start with a recovery slice
            let pick = if is_base { SegmentPick::First(2) } else { SegmentPick::Last(1) };
            let next = ctx
                .segments(file.id, &pick)
                .map_err(retry)?
                .into_iter()
                .last()
                .filter(|s| s.message_id != seg.message_id);
            if let Some(next) = next
                && let Some(more) = fetch_one(ctx, &next, &file.filename)?
            {
                body.extend(more);
                inventory = par2::inventory(&body);
            }
        }
        if inventory.files.is_empty() {
            return Ok(Outcome::default());
        }
        let limits = self.limits.lock().unwrap().clone();
        finish(ctx, SOURCE_PAR2, listing_of(inventory), &limits)
    }
}

/// When a probe may go: at most one per `gap`. A hang-up means a new login for
/// the next request, and logins refused in a burst rest the server for everyone.
pub fn next_probe_slot(next: &mut Option<Instant>, now: Instant, gap: Duration) -> Instant {
    let at = next.filter(|n| *n > now).unwrap_or(now);
    *next = Some(at + gap);
    at
}

/// Reads an archive's list of files.
#[derive(Default)]
pub struct ListStep {
    limits: Mutex<ArchiveConfig>,
    next_probe: Mutex<Option<Instant>>,
}

impl ListStep {
    /// wait for this probe's slot, holding no lock, permit or token meanwhile
    fn wait_for_slot(&self, ctx: &StepCtx<'_>, gap: Duration) -> Result<(), StepError> {
        let at = next_probe_slot(&mut self.next_probe.lock().unwrap(), Instant::now(), gap);
        while Instant::now() < at {
            if ctx.stopping() {
                return Err(StepError::Stopped);
            }
            std::thread::sleep(at.saturating_duration_since(Instant::now()).min(Duration::from_millis(250)));
        }
        Ok(())
    }
}

/// a body of the budget left: None when it's spent or the fetch came to nothing
fn budgeted(
    ctx: &mut StepCtx<'_>,
    seg: &SegmentRef,
    file: &str,
    left: &mut usize,
) -> Result<Option<Vec<u8>>, StepError> {
    if *left == 0 {
        return Ok(None);
    }
    *left -= 1;
    fetch_one(ctx, seg, file)
}

/// ZIP's directory is at its end: the article read, then its set's last.
fn zip_list(ctx: &mut StepCtx<'_>, head: &Head<'_>, body: &[u8], left: &mut usize) -> Result<Listing, StepError> {
    let in_body = zip::list_tail(body);
    if !in_body.files.is_empty() {
        return Ok(in_body);
    }
    let segs = tail_articles(ctx, head.tail)?;
    let Some(last) = segs.last() else { return Ok(zip::list(body, &[])) };
    let tail = budgeted(ctx, last, &head.tail.filename, left)?;
    Ok(zip::list(body, tail.as_deref().unwrap_or(&[])))
}

/// 7z's header may be at its end, in up to its set's last two articles.
fn seven_list(
    ctx: &mut StepCtx<'_>,
    head: &Head<'_>,
    seg: &SegmentRef,
    body: &[u8],
    left: &mut usize,
) -> Result<Listing, StepError> {
    let listing = sevenz::list(body, &[]);
    if !listing.need_more_tail {
        return Ok(listing);
    }
    if head.probe {
        // longer than this file: its set has more volumes, and which is last isnt known
        let longer = sevenz::start(body).is_some_and(|s| {
            let len = 32u64.saturating_add(s.offset).saturating_add(s.size);
            let article = u64::try_from(seg.bytes).unwrap_or(0);
            let parts = u64::try_from(head.file.expected.unwrap_or(1)).unwrap_or(1);
            len > parts.saturating_mul(article).saturating_mul(11) / 10
        });
        if longer {
            return Ok(listing);
        }
    }
    let segs = tail_articles(ctx, head.tail)?;
    let Some(last) = segs.last() else { return Ok(listing) };
    let Some(tail) = budgeted(ctx, last, &head.tail.filename, left)? else { return Ok(listing) };
    let listing = sevenz::list(body, &tail);
    if !listing.need_more_tail {
        return Ok(listing);
    }
    let adjacent = segs.first().filter(|s| s.part.zip(last.part).is_some_and(|(a, b)| a + 1 == b));
    let Some(before) = adjacent else { return Ok(listing) };
    let Some(mut joined) = budgeted(ctx, before, &head.tail.filename, left)? else { return Ok(listing) };
    joined.extend(tail);
    Ok(sevenz::list(body, &joined))
}

impl Step for ListStep {
    fn id(&self) -> StepId {
        StepId(2)
    }
    fn name(&self) -> &'static str {
        "archive_list"
    }
    fn rank(&self) -> u16 {
        110
    }
    fn needs(&self) -> Needs {
        Needs::Bodies
    }
    limits_from_enabled!();
    fn applies(&self, release: &ReleaseInfo) -> bool {
        !named_as_an_image(release)
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: false, on_growth: true }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let limits = self.limits.lock().unwrap().clone();
        let files = ctx.files().map_err(retry)?;
        let mut left = limits.max_bodies_per_release.max(0) as usize;
        let gap = Duration::from_secs(60) / limits.probes_per_minute.max(1) as u32;
        for head in select::plan(&files, limits.probe_files.max(0) as usize) {
            let Some(seg) = first_article(ctx, head.file)? else { continue };
            if left == 0 {
                break;
            }
            left -= 1;
            let body = if head.probe {
                self.wait_for_slot(ctx, gap)?;
                fetch_start(ctx, &seg, &head.file.filename)?
            } else {
                fetch_one(ctx, &seg, &head.file.filename)?
            };
            let Some(body) = body else { continue };
            match identify(&body) {
                Format::Rar4 | Format::Rar5 => return finish(ctx, SOURCE_RAR, rar::list(&body), &limits),
                Format::Zip => {
                    let listing = zip_list(ctx, &head, &body, &mut left)?;
                    return finish(ctx, SOURCE_ZIP, listing, &limits);
                }
                Format::SevenZip => {
                    let listing = seven_list(ctx, &head, &seg, &body, &mut left)?;
                    return finish(ctx, SOURCE_7Z, listing, &limits);
                }
                Format::Par2 => {
                    // a PAR2 under another name: without a list, it cant replace the rows of one that had
                    let listing = listing_of(par2::inventory(&body));
                    if listing.files.is_empty() {
                        continue;
                    }
                    return finish(ctx, SOURCE_PAR2, listing, &limits);
                }
                // images: stop, with at most a prefix read
                Format::Image => return Ok(Outcome::default()),
                Format::Media | Format::Unknown => continue,
            }
        }
        Ok(Outcome::default())
    }
}

/// Names a release from its NFO, when nothing else named it.
#[derive(Default)]
pub struct NfoStep {
    limits: Mutex<ArchiveConfig>,
}

impl Step for NfoStep {
    fn id(&self) -> StepId {
        StepId(3)
    }
    fn name(&self) -> &'static str {
        "archive_nfo"
    }
    fn rank(&self) -> u16 {
        120
    }
    fn needs(&self) -> Needs {
        Needs::Bodies
    }
    limits_from_enabled!();
    fn applies(&self, release: &ReleaseInfo) -> bool {
        !named_as_an_image(release) && release.display_name.is_none()
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: false, on_growth: true }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let files = ctx.files().map_err(retry)?;
        for file in select::nfo_sources(&files) {
            let Some(seg) = first_article(ctx, file)? else { continue };
            let Some(body) = fetch_one(ctx, &seg, &file.filename)? else { continue };
            if let Some(name) = nfo::display_name(&body) {
                let obfuscated = Some(ctx.release().is_obfuscated);
                let change = NameChange { display_name: Some(name), obfuscated, source: NAME_SOURCE_NFO };
                return Ok(Outcome { matched: true, name: Some(change), ..Default::default() });
            }
        }
        Ok(Outcome::default())
    }
}
