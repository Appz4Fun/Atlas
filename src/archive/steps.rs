//! Processing steps that read a release's archives, PAR2 sets and NFOs.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use rusqlite::params;
use rusqlite::types::Value;

use crate::duplicates;

use crate::config::{ArchiveConfig, Config};
use crate::naming::{self, NameSource};
use crate::par2;
use crate::postprocess::step::{
    BodyRequest, Got, NameChange, Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId, StepWrite,
};
use crate::store::{self, FileInfo, SegmentPick, SegmentRef};

use super::names::{best_name, collapse_volumes, looks_random, stem};
use super::select::{self, Head, Kind};
use super::store::{self as archive_store, SOURCE_7Z, SOURCE_NZB, SOURCE_PAR2, SOURCE_RAR, SOURCE_ZIP};
use super::{
    ARTICLE_CAP, Format, InnerFile, Listing, NAME_SOURCE_INNER, NAME_SOURCE_PAR2, PROBE_PREFIX, RERUN_ON_INNER_FILES,
    identify, nzb, rar, sevenz, zip,
};

fn retry(e: impl std::fmt::Display) -> StepError {
    StepError::Retry(e.to_string())
}

/// a release named as an image keeps its file's name, soo these steps leave it be
fn named_as_an_image(release: &ReleaseInfo) -> bool {
    select::classify(&release.name) == Kind::Image
}

/// A file's first article. None: it was never indexed, and a fetch would read the wrong bytes.
pub(crate) fn first_article(ctx: &StepCtx<'_>, file: &FileInfo) -> Result<Option<SegmentRef>, StepError> {
    let seg = ctx.segments(file.id, &SegmentPick::First(1)).map_err(retry)?.into_iter().next();
    Ok(seg.filter(|s| s.part.is_none_or(|p| p <= 1)))
}

/// A file's last articles, last one last. Empty when its last isnt indexed.
pub(crate) fn tail_articles(ctx: &StepCtx<'_>, file: &FileInfo) -> Result<Vec<SegmentRef>, StepError> {
    let segs = ctx.segments(file.id, &SegmentPick::Last(2)).map_err(retry)?;
    let last_is_there = match (file.expected, segs.last().and_then(|s| s.part)) {
        (Some(expected), Some(part)) => part == expected,
        (Some(_), None) => false,
        (None, _) => true,
    };
    Ok(if last_is_there { segs } else { Vec::new() })
}

/// One whole article, within every budget. None: skip this candidate.
pub(crate) fn fetch_one(ctx: &mut StepCtx<'_>, seg: &SegmentRef, file: &str) -> Result<Option<Vec<u8>>, StepError> {
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

/// A small file's every part, decoded and joined in order, within the budget: a part's repost stands in when its
/// first copy comes to nothing. None: a part isnt indexed or none of its copies came, or the file is more than a
/// list should be.
fn whole_file(ctx: &mut StepCtx<'_>, file: &FileInfo, left: &mut usize) -> Result<Option<Vec<u8>>, StepError> {
    let parts = file.expected.unwrap_or(0).max(file.parts_seen);
    if parts < 1 || parts as usize > *left {
        return Ok(None);
    }
    let mut by_part: std::collections::BTreeMap<i64, Vec<SegmentRef>> = std::collections::BTreeMap::new();
    for seg in ctx.segments(file.id, &SegmentPick::Parts((1..=parts).collect())).map_err(retry)? {
        if let Some(part) = seg.part {
            by_part.entry(part).or_default().push(seg);
        }
    }
    // one article posted with no counter has no part number to pick it by
    if parts == 1 && by_part.is_empty() {
        by_part.insert(1, ctx.segments(file.id, &SegmentPick::First(1)).map_err(retry)?);
    }
    if by_part.len() as i64 != parts || by_part.values().any(Vec::is_empty) {
        return Ok(None);
    }
    let mut whole = Vec::new();
    for copies in by_part.values() {
        let mut body = None;
        for seg in copies {
            body = budgeted(ctx, seg, &file.filename, left)?;
            if body.is_some() {
                break;
            }
        }
        let Some(body) = body else { return Ok(None) };
        whole.extend(body);
        if whole.len() > nzb::MAX_NZB_BYTES {
            return Ok(None);
        }
    }
    Ok(Some(whole))
}

/// The list a posted NZB gave before, kept in the raw store, when nothing else in the release is to be read: no
/// articles fetched for it. Not an archive, soo it has no `raw_archives` row for `get_archive` to find.
fn restore_nzb(
    ctx: &StepCtx<'_>,
    files: &[FileInfo],
    content_id: &[u8],
    limits: &ArchiveConfig,
) -> Result<Option<Outcome>, StepError> {
    if select::nzb_sources(files).is_empty() || !select::plan(files, limits.probe_files.max(0) as usize).is_empty() {
        return Ok(None);
    }
    let rel = ctx.release();
    let saved = crate::raw_store::get_archive_files(&rel.name, &rel.group, content_id, SOURCE_NZB);
    if saved.is_empty() {
        return Ok(None);
    }
    let files = saved.into_iter().map(|f| InnerFile { name: f.name, size: f.size }).collect();
    let listing = Listing { files, complete: true, ..Default::default() };
    finish_with_override(ctx, SOURCE_NZB, listing, None, Some(None), false, limits).map(Some)
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
    finish_with_override(ctx, source, listing, None, None, true, limits)
}

fn finish_with_override(
    ctx: &StepCtx<'_>,
    source: i64,
    listing: Listing,
    password_override: Option<super::Password>,
    content_override: Option<Option<Vec<u8>>>,
    persist_raw: bool,
    limits: &ArchiveConfig,
) -> Result<Outcome, StepError> {
    let release = ctx.release();
    let id = release.id;
    // a program inside: the release goes, see `junk`
    if listing.files.iter().any(|f| crate::junk::is_executable(&f.name)) {
        return Ok(Outcome { junk: Some(crate::junk::EXECUTABLE_INSIDE), ..Default::default() });
    }
    let mut writes = Vec::new();
    let rows = collapse_volumes(&listing.files);
    let pw = password_override.unwrap_or_else(|| listing.password());
    let old: i64 = ctx
        .conn()
        .prepare_cached(archive_store::BYTES_OF_SOURCE)
        .and_then(|mut stmt| stmt.query_row(params![id, source], |r| r.get(0)))
        .map_err(retry)?;
    let used = store::get_meta(ctx.conn(), "archive_name_bytes").map_err(retry)?.unwrap_or(0);
    let total_quota_bytes = limits.max_names_mb.saturating_mul(1 << 20);
    let quota = total_quota_bytes / store::SHARDS as i64;
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
    // part of a list would call two releases the same when only their first files are
    let files = ctx.files().map_err(retry)?;
    let is_par2 = |name: &str| name.to_ascii_lowercase().ends_with(".par2");
    let speaks_for_release = if source == SOURCE_PAR2 {
        listing.files.len() == files.iter().filter(|f| !is_par2(&f.filename)).count()
    } else {
        duplicates::one_archive_set(files.iter().map(|f| f.filename.as_str()))
    };
    let content = match content_override {
        // what a posted NZB lists is another post's files, not what this one holds
        _ if source == SOURCE_NZB => None,
        Some(fp) => fp,
        None => (listing.complete && !listing.damaged && speaks_for_release)
            .then(|| duplicates::content_fingerprint(listing.files.iter().map(|f| (f.name.as_str(), f.size))))
            .flatten(),
    };
    if persist_raw {
        let content_id = crate::raw_store::release_content_id(ctx.conn(), id).unwrap_or_default();
        let check_bytes = listing.password_check.map(|c| c.to_bytes());
        crate::raw_store::put_archive(&crate::raw_store::ArchiveRecord {
            name: &release.name,
            group_name: &release.group,
            content_id: &content_id,
            source,
            files: &rows,
            password: (source != SOURCE_PAR2 && source != SOURCE_NZB).then(|| pw.code()),
            encryption: listing.encryption.map(super::Encryption::code),
            names_hidden: listing.headers_encrypted,
            password_check: check_bytes.as_deref(),
            parts: release.parts,
            complete: release.complete,
            listing_complete: listing.complete,
            listing_damaged: listing.damaged,
            content_fp: (source != SOURCE_PAR2).then_some(content.as_deref()).flatten(),
            max_names_bytes: total_quota_bytes,
        });
    }
    // a PAR2 set of archive volumes says nothing about what's inside them: that's the archive step's to say
    let leaves_it = source == SOURCE_NZB
        || source == SOURCE_PAR2 && duplicates::lists_archives(listing.files.iter().map(|f| f.name.as_str()));
    if !leaves_it {
        writes.push(match content {
            Some(content) => StepWrite::new(duplicates::SET_CONTENT, vec![id.into(), content.into()]),
            None => StepWrite::new(duplicates::CLEAR_CONTENT, vec![id.into()]),
        });
    }
    // no archive was read, soo no password to tell
    if source != SOURCE_PAR2 && source != SOURCE_NZB {
        writes.push(StepWrite::new(archive_store::UPSERT_RELEASE, vec![id.into(), pw.code().into()]));
        if listing.encryption.is_some() || listing.headers_encrypted || listing.password_check.is_some() {
            writes.push(StepWrite::new(
                archive_store::SET_ENCRYPTION,
                vec![
                    id.into(),
                    listing.encryption.map(super::Encryption::code).into(),
                    i64::from(listing.headers_encrypted).into(),
                    listing.password_check.map(|check| check.to_bytes()).into(),
                ],
            ));
        }
    }

    let name_source = if source == SOURCE_PAR2 { NAME_SOURCE_PAR2 } else { NAME_SOURCE_INNER };
    let name_wanted = name_may_change(release, name_source)
        && (source == SOURCE_PAR2 || release.is_obfuscated || looks_random(stem(&release.name)));
    let naming_source = if source == SOURCE_PAR2 { NameSource::Par2 } else { NameSource::ArchiveInner };
    let name = name_wanted
        .then(|| best_name(listing.files.iter().map(|f| (f.name.as_str(), f.size.unwrap_or(0)))))
        .flatten()
        // a junk name at a high source would block every lower source for good
        .filter(|n| naming::improves(release, n, naming_source))
        .map(|n| NameChange { display_name: Some(n), obfuscated: Some(release.is_obfuscated), source: name_source });

    Ok(Outcome {
        matched: stored || pw == super::Password::Yes,
        writes,
        name,
        rerun: if stored { RERUN_ON_INNER_FILES } else { 0 },
        ..Default::default()
    })
}

fn restore_archive(
    ctx: &StepCtx<'_>,
    saved: crate::raw_store::SavedArchive,
    limits: &ArchiveConfig,
) -> Result<Outcome, StepError> {
    let source = saved.files.first().map(|f| f.source).unwrap_or_else(|| {
        match saved.encryption.and_then(super::Encryption::from_code) {
            Some(super::Encryption::SevenZipAes256) => SOURCE_7Z,
            Some(super::Encryption::ZipCrypto | super::Encryption::ZipAes) => SOURCE_ZIP,
            _ => SOURCE_RAR,
        }
    });
    let files: Vec<InnerFile> = saved.files.into_iter().map(|f| InnerFile { name: f.name, size: f.size }).collect();
    let password_override = saved.password.and_then(super::Password::from_code);
    let encrypted = password_override == Some(super::Password::Yes) || saved.encryption.is_some() || saved.names_hidden;
    let listing = Listing {
        files,
        encrypted,
        headers_encrypted: saved.names_hidden,
        encryption: saved.encryption.and_then(super::Encryption::from_code),
        password_check: saved.password_check.as_deref().and_then(super::PasswordTest::from_bytes),
        complete: saved.listing_complete,
        damaged: saved.listing_damaged,
        need_more_tail: false,
    };
    finish_with_override(ctx, source, listing, password_override, Some(saved.content_fp), false, limits)
}

fn restore_par2(
    ctx: &StepCtx<'_>,
    saved_files: Vec<crate::raw_store::SavedArchiveFile>,
    meta: Option<crate::raw_store::SavedPar2>,
    limits: &ArchiveConfig,
) -> Result<Outcome, StepError> {
    let restored: Vec<InnerFile> = saved_files.into_iter().map(|f| InnerFile { name: f.name, size: f.size }).collect();
    let covers_all = meta.as_ref().is_some_and(|m| m.covers_all);
    let content_fp = meta.as_ref().and_then(|m| m.content_fp.clone());
    let mut outcome = finish_with_override(
        ctx,
        SOURCE_PAR2,
        Listing { files: restored, complete: covers_all, ..Default::default() },
        None,
        Some(content_fp),
        false,
        limits,
    )?;
    let id = ctx.release().id;
    if !covers_all {
        outcome.writes.retain(|write| write.sql != duplicates::SET_CONTENT);
        outcome.writes.push(StepWrite::new(duplicates::CLEAR_POSTED, vec![id.into()]));
    }
    if let Some(m) = meta {
        if let Some(inv) = &m.inventory {
            outcome.writes.extend(archive_store::par2_file_writes(id, inv));
        }
        if let Some(posted) = m.posted {
            outcome.writes.push(StepWrite::new(duplicates::SET_POSTED, vec![id.into(), posted.into()]));
        }
        if let Some((block, blocks)) = m.sizes {
            outcome
                .writes
                .push(StepWrite::new(duplicates::SET_PAR2_SIZES, vec![id.into(), block.into(), blocks.into()]));
        }
    }
    Ok(outcome)
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
pub const PAR2_STEP: StepId = StepId(1);

#[derive(Default)]
pub struct Par2Step {
    limits: Mutex<ArchiveConfig>,
}

impl Step for Par2Step {
    fn id(&self) -> StepId {
        PAR2_STEP
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
        let rel_id = ctx.release().id;
        let rel_name = ctx.release().name.clone();
        let rel_group = ctx.release().group.clone();
        let rel_parts = ctx.release().parts;
        let rel_complete = ctx.release().complete;
        let content_id = crate::raw_store::release_content_id(ctx.conn(), rel_id).unwrap_or_default();
        let saved_par2 = crate::raw_store::get_archive_files(&rel_name, &rel_group, &content_id, SOURCE_PAR2);
        let par2_meta = crate::raw_store::get_par2(&rel_name, &rel_group, &content_id, rel_parts, rel_complete);
        if !crate::raw_store::is_explicit_rerun(ctx.conn(), rel_id, PAR2_STEP)
            && !saved_par2.is_empty()
            && let Some(meta) = par2_meta.as_ref().filter(|m| m.inventory.is_some())
        {
            let limits = self.limits.lock().unwrap().clone();
            return restore_par2(ctx, saved_par2, Some(meta.clone()), &limits);
        }
        let is_base = select::classify(&file.filename) == Kind::Par2Base;
        let Some(seg) = first_article(ctx, file)? else {
            if !saved_par2.is_empty() {
                let limits = self.limits.lock().unwrap().clone();
                return restore_par2(ctx, saved_par2, par2_meta, &limits);
            }
            return Ok(Outcome::default());
        };
        let Some(mut body) = fetch_one(ctx, &seg, &file.filename)? else {
            if !saved_par2.is_empty() {
                let limits = self.limits.lock().unwrap().clone();
                return restore_par2(ctx, saved_par2, par2_meta, &limits);
            }
            return Ok(Outcome::default());
        };
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
        // one set that covers every posted file. A release can hold several sets, and one proves only its own files
        let data_files = files.iter().filter(|f| !f.filename.to_ascii_lowercase().ends_with(".par2")).count();
        let one_set =
            inventory.files.first().is_some_and(|first| inventory.files.iter().all(|f| f.set_id == first.set_id));
        let covers_all = inventory.complete && one_set && inventory.files.len() == data_files;
        let posted_files: Vec<(&str, i64)> =
            files.iter().map(|f| (f.filename.as_str(), f.expected.unwrap_or(f.parts_seen))).collect();
        let posted = covers_all
            .then(|| duplicates::twin_fingerprint(&inventory.files, inventory.block_size, &posted_files))
            .flatten();
        let sizes = inventory.block_size.filter(|_| covers_all).map(|block| {
            // each file's last block is padded on its own
            let blocks: u64 = inventory.files.iter().map(|f| f.size.div_ceil(block)).sum();
            (block as i64, blocks as i64)
        });
        let leaves_content = duplicates::lists_archives(inventory.files.iter().map(|f| f.name.as_str()));
        let content_fp = (covers_all && !leaves_content)
            .then(|| duplicates::content_fingerprint(inventory.files.iter().map(|f| (f.name.as_str(), Some(f.size)))))
            .flatten();
        crate::raw_store::put_par2(&crate::raw_store::Par2Record {
            name: &rel_name,
            group_name: &rel_group,
            content_id: &content_id,
            covers_all,
            posted: posted.as_deref(),
            sizes,
            parts: rel_parts,
            complete: rel_complete,
            content_fp: content_fp.as_deref(),
            inventory: Some(&inventory),
        });
        // the set's files, hashes and all: what the link step and twin fill go by without reading the set again
        let kept = archive_store::par2_file_writes(ctx.release().id, &inventory);
        let mut outcome = finish(ctx, SOURCE_PAR2, listing_of(inventory), &limits)?;
        outcome.writes.extend(kept);
        // one set's files are only some of the release's, soo they don't say what the release holds
        if !covers_all {
            outcome.writes.retain(|write| write.sql != duplicates::SET_CONTENT);
            let id = ctx.release().id;
            outcome.writes.push(StepWrite::new(duplicates::CLEAR_POSTED, vec![id.into()]));
        }
        if let Some(posted) = posted {
            let params = vec![ctx.release().id.into(), posted.into()];
            outcome.writes.push(StepWrite::new(duplicates::SET_POSTED, params));
        }
        if let Some((block, blocks)) = sizes {
            let params = vec![ctx.release().id.into(), block.into(), blocks.into()];
            outcome.writes.push(StepWrite::new(duplicates::SET_PAR2_SIZES, params));
        }
        Ok(outcome)
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
pub const LIST_STEP: StepId = StepId(2);

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
        // with the first entry's password check, which only the local header has
        return Ok(zip::list(body, body));
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
        LIST_STEP
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
        let rel = ctx.release();
        let rel_id = rel.id;
        let rel_name = rel.name.clone();
        let rel_group = rel.group.clone();
        let content_id = crate::raw_store::release_content_id(ctx.conn(), rel_id).unwrap_or_default();
        if !crate::raw_store::is_explicit_rerun(ctx.conn(), rel_id, LIST_STEP) {
            if let Some(saved) =
                crate::raw_store::get_archive(&rel_name, &rel_group, &content_id, rel.parts, rel.complete)
            {
                return restore_archive(ctx, saved, &limits);
            }
            if let Some(outcome) = restore_nzb(ctx, &files, &content_id, &limits)? {
                return Ok(outcome);
            }
            if select::par2_source(&files).is_none() {
                let saved_par2 = crate::raw_store::get_archive_files(&rel_name, &rel_group, &content_id, SOURCE_PAR2);
                if !saved_par2.is_empty()
                    && let Some(inv) =
                        crate::raw_store::get_par2_files(&rel_name, &rel_group, &content_id).filter(|i| i.complete)
                {
                    let restored = saved_par2.into_iter().map(|f| InnerFile { name: f.name, size: f.size }).collect();
                    let mut outcome = finish_with_override(
                        ctx,
                        SOURCE_PAR2,
                        Listing { files: restored, complete: false, ..Default::default() },
                        None,
                        Some(None),
                        false,
                        &limits,
                    )?;
                    outcome.writes.extend(archive_store::par2_file_writes(rel_id, &inv));
                    return Ok(outcome);
                }
            }
        }
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
                    let inventory = par2::inventory(&body);
                    if inventory.files.is_empty() {
                        continue;
                    }
                    crate::raw_store::put_par2_files(&rel_name, &rel_group, &content_id, &inventory);
                    let kept = archive_store::par2_file_writes(ctx.release().id, &inventory);
                    let listing = listing_of(inventory);
                    let listed = listing.files.len();
                    let mut outcome = finish(ctx, SOURCE_PAR2, listing, &limits)?;
                    outcome.writes.extend(kept);
                    // finish counts files by name, and this one is PAR2 under another name: the set speaks for the
                    // release only when the rest of its files are the set's. A recovery volume posted on its own, as a
                    // fully obfuscated upload does, would otherwise claim the content of the files it repairs
                    let data_files = files
                        .iter()
                        .filter(|f| f.id != head.file.id && !f.filename.to_ascii_lowercase().ends_with(".par2"))
                        .count();
                    if data_files != listed {
                        let id = ctx.release().id;
                        for write in outcome.writes.iter_mut().filter(|w| w.sql == duplicates::SET_CONTENT) {
                            *write = StepWrite::new(duplicates::CLEAR_CONTENT, vec![id.into()]);
                        }
                    }
                    return Ok(outcome);
                }
                // images: stop, with at most a prefix read
                Format::Image => return Ok(Outcome::default()),
                // as a planned head it's an archive's name on something else: what a posted NZB lists is below
                Format::Nzb | Format::Media | Format::Unknown => continue,
            }
        }
        // no archive told what this is: a posted NZB can, by the files of the post it points to
        for file in select::nzb_sources(&files) {
            let Some(body) = whole_file(ctx, file, &mut left)? else { continue };
            if identify(&body) == Format::Nzb {
                return finish(ctx, SOURCE_NZB, nzb::list(&body), &limits);
            }
        }
        let rel = ctx.release();
        if let Some(saved) = crate::raw_store::get_archive(&rel.name, &rel.group, &content_id, 0, false) {
            return restore_archive(ctx, saved, &limits);
        }
        // its articles may be gone since
        if let Some(outcome) = restore_nzb(ctx, &files, &content_id, &limits)? {
            return Ok(outcome);
        }
        if select::par2_source(&files).is_none() {
            let saved_par2 = crate::raw_store::get_archive_files(&rel_name, &rel_group, &content_id, SOURCE_PAR2);
            if !saved_par2.is_empty() {
                let restored = saved_par2.into_iter().map(|f| InnerFile { name: f.name, size: f.size }).collect();
                let mut outcome = finish_with_override(
                    ctx,
                    SOURCE_PAR2,
                    Listing { files: restored, complete: false, ..Default::default() },
                    None,
                    Some(None),
                    false,
                    &limits,
                )?;
                if let Some(inv) = crate::raw_store::get_par2_files(&rel_name, &rel_group, &content_id) {
                    outcome.writes.extend(archive_store::par2_file_writes(rel_id, &inv));
                }
                return Ok(outcome);
            }
        }
        Ok(Outcome::default())
    }
}
