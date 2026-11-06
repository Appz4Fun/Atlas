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
    follow, identify, nzb, rar, sevenz, zip,
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

/// What an NZB an archive holds lists, from the article read: either stored in `body` or unwrapped/decrypted via
/// `nest::unwrap_with_password`. Also returns any password found in `<meta type="password">` or the inner `.nzb` name.
fn nzb_stored_in(listing: &Listing, body: &[u8], password: Option<&str>) -> Option<(Listing, Option<String>)> {
    if !listing.files.iter().any(|f| select::classify(&f.name) == Kind::Nzb) {
        return None;
    }
    let name_pw = listing
        .files
        .iter()
        .filter(|f| select::classify(&f.name) == Kind::Nzb)
        .find_map(|f| crate::release_info::find_password(&f.name));
    if !listing.encrypted
        && let Some(at) = (0..body.len().saturating_sub(4))
            .filter(|at| body.get(*at) == Some(&b'<'))
            .find(|at| body.get(*at..).is_some_and(nzb::is_nzb))
        && let Some(slice) = body.get(at..)
    {
        let inside = nzb::list(slice);
        if !inside.files.is_empty() {
            let pw = nzb::meta_password(slice).or(name_pw);
            return Some((inside, pw));
        }
    }
    let unwrapped = crate::probe::nest::unwrap_nzb_with_password(body, password);
    if !nzb::is_nzb(&unwrapped.inner) {
        return None;
    }
    let inside = nzb::list(&unwrapped.inner);
    let pw = nzb::meta_password(&unwrapped.inner).or(name_pw);
    (!inside.files.is_empty()).then_some((inside, pw))
}

/// The NZB an archive of this release names inside, read whole from the release's own articles when the release is
/// small enough to (`follow::MAX_POST_BYTES`), and followed on hop after hop. None: no NZB named, an encrypted
/// archive, a release too big, or an article not there.
/// `head_file`: the file whose first article told the archive; `known`: that article by its message-id, when it was
/// read whole and not only its start.
fn deep_nzb(
    ctx: &mut StepCtx<'_>,
    files: &[FileInfo],
    listing: &Listing,
    head_file: &FileInfo,
    known: Option<(&str, &[u8])>,
    mut left: follow::Budget,
) -> Result<Option<(Listing, Option<String>)>, StepError> {
    if listing.encrypted || !listing.files.iter().any(|f| select::classify(&f.name) == Kind::Nzb) {
        return Ok(None);
    }
    let rel = ctx.release();
    if u64::try_from(rel.size).unwrap_or(u64::MAX) > follow::MAX_POST_BYTES
        || usize::try_from(rel.parts).unwrap_or(usize::MAX) > follow::MAX_POST_ARTICLES
    {
        return Ok(None);
    }
    let mut wholes = Vec::new();
    for file in files {
        if let Some(whole) = own_whole(ctx, file)? {
            wholes.push(whole);
        }
    }
    // an archive known by its bytes, not its name, is read first: no name would pick it
    let first = match select::classify(&head_file.filename) {
        Kind::Rar(_) | Kind::SevenZip(_) | Kind::Zip(_) | Kind::Numbered(_) => None,
        _ => own_whole(ctx, head_file)?.map(|whole| follow::Set { files: vec![whole], headed_volumes: false }),
    };
    let Some(found) = follow::nzb_inside(ctx, first, &wholes, None, known, &mut left)? else {
        return Ok(None);
    };
    // too big to be read as a list here: the first article's part of it still is, by the caller
    if found.len() > follow::MAX_FOLLOWED_NZB {
        return Ok(None);
    }
    let name_pw = listing
        .files
        .iter()
        .filter(|f| select::classify(&f.name) == Kind::Nzb)
        .find_map(|f| crate::release_info::find_password(&f.name));
    let complete = nzb::parse(&found).complete;
    let (followed, hop_pw) = follow::follow_with_password(ctx, found, &mut left)?;
    Ok(Some((Listing { files: followed, complete, ..Default::default() }, hop_pw.or(name_pw))))
}

/// A file of this release to read whole: one article a part, the first copy. None: a part isnt indexed.
fn own_whole(ctx: &StepCtx<'_>, file: &FileInfo) -> Result<Option<follow::Whole>, StepError> {
    let parts = file.expected.unwrap_or(0).max(file.parts_seen);
    if parts < 1 {
        return Ok(None);
    }
    let mut ids: std::collections::BTreeMap<i64, String> = std::collections::BTreeMap::new();
    for seg in ctx.segments(file.id, &SegmentPick::Parts((1..=parts).collect())).map_err(retry)? {
        if let Some(part) = seg.part {
            ids.entry(part).or_insert(seg.message_id);
        }
    }
    // one article posted with no counter has no part number to pick it by
    if parts == 1 && ids.is_empty() {
        for seg in ctx.segments(file.id, &SegmentPick::First(1)).map_err(retry)? {
            ids.insert(1, seg.message_id);
        }
    }
    if ids.len() as i64 != parts {
        return Ok(None);
    }
    Ok(Some(follow::Whole { name: file.filename.clone(), ids: ids.into_values().collect(), trusted: true }))
}

/// An archive's outcome with the list of the NZB inside it beside its own rows, in the same writes. The archive's
/// name, password and fingerprint stand; a program the NZB lists makes the release junk as one inside would.
fn with_nzb(
    ctx: &StepCtx<'_>,
    outcome: Outcome,
    inside: Option<(Listing, Option<String>)>,
    wrapper_encrypted: bool,
    has_inner_nzb: bool,
    pw_available: bool,
    limits: &ArchiveConfig,
) -> Result<Outcome, StepError> {
    let rel = ctx.release();
    let content_id = crate::raw_store::release_content_id(ctx.conn(), rel.id).unwrap_or_default();
    let Some((inside, nzb_pw)) = inside else {
        if has_inner_nzb && (!wrapper_encrypted || pw_available) {
            crate::raw_store::put_nested_nzb_password(&rel.name, &rel.group, &content_id, "");
        }
        return Ok(outcome);
    };
    if outcome.junk.is_some() {
        return Ok(outcome);
    }
    let mut nested = finish(ctx, SOURCE_NZB, inside, limits)?;
    if let Some(pw) = nzb_pw {
        crate::raw_store::put_nested_nzb_password(&rel.name, &rel.group, &content_id, &pw);
        add_pointed_password_writes(ctx, &mut nested.writes, &pw, !wrapper_encrypted);
    } else {
        crate::raw_store::put_nested_nzb_password(&rel.name, &rel.group, &content_id, "");
    }
    Ok(merge_nzb(outcome, nested))
}

fn add_pointed_password_writes(ctx: &StepCtx<'_>, writes: &mut Vec<StepWrite>, pw: &str, set_self: bool) {
    let rel = ctx.release();
    let id = rel.id;
    if set_self {
        let content_id = crate::raw_store::release_content_id(ctx.conn(), id).unwrap_or_default();
        crate::raw_store::put_nfo_password(&rel.name, &rel.group, &content_id, pw);
        writes.push(StepWrite::new(archive_store::SET_NFO_PASSWORD, vec![id.into(), pw.to_string().into()]));
    }
    let target_names: Vec<String> = writes
        .iter()
        .filter(|w| w.sql == archive_store::MARK_POINTED)
        .filter_map(|w| match w.params.first() {
            Some(Value::Text(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    for name in target_names {
        writes.push(StepWrite::new(
            archive_store::RERUN_POINTED_LIST.as_str(),
            vec![name.clone().into(), id.into()],
        ));
        writes.push(StepWrite::new(
            archive_store::RERUN_POINTED_PROBE.as_str(),
            vec![name.clone().into(), id.into()],
        ));
        writes.push(StepWrite::new(
            archive_store::MARK_POINTED_PASSWORD,
            vec![name.clone().into(), id.into(), pw.to_string().into()],
        ));
        writes.push(StepWrite::new(
            archive_store::RANK_POINTED.as_str(),
            vec![name.into(), id.into()],
        ));
    }
}

/// Looks up a password already stored or visible in the release's names/subjects, verified against `check` when given.
fn known_or_post_password(
    ctx: &StepCtx<'_>,
    files: &[FileInfo],
    check: Option<&super::PasswordTest>,
) -> Option<String> {
    let rel = ctx.release();
    let content_id = crate::raw_store::release_content_id(ctx.conn(), rel.id).unwrap_or_default();
    let mut extra = Vec::new();
    if let Some(pw) = archive_store::nfo_password(ctx.conn(), rel.id) {
        extra.push(pw);
    }
    if let Some(pw) = crate::raw_store::get_nfo_password(&rel.name, &rel.group, &content_id)
        && !extra.contains(&pw)
    {
        extra.push(pw);
    }
    let texts = std::iter::once(rel.name.as_str())
        .chain(rel.display_name.as_deref())
        .chain(files.iter().map(|f| f.filename.as_str()))
        .chain(files.iter().filter_map(|f| f.subject.as_deref()));
    crate::release_info::find_verified_password(texts, &extra, check)
}

/// Harvests a password for an encrypted archive from the post's names/subjects, any posted `.nzb`, or any `.nfo`/`.txt`
/// (including files identified via `par2_files` or a single-part unidentified NFO), verifying candidates against `check`.
/// An NFO it fetches goes in `kept`, a write that keeps it for the probe and the other steps.
fn harvest_password_from_bodies(
    ctx: &mut StepCtx<'_>,
    files: &[FileInfo],
    left: &mut usize,
    check: Option<&super::PasswordTest>,
    kept: &mut Vec<StepWrite>,
) -> Result<Option<String>, StepError> {
    if let Some(pw) = known_or_post_password(ctx, files, check) {
        return Ok(Some(pw));
    }
    let rel_id = ctx.release().id;
    let par2_names = select::par2_name_map(ctx.conn(), rel_id, files);
    let par2_all = select::par2_file_names(ctx.conn(), rel_id);
    for par2_name in par2_all.iter().chain(par2_names.values()) {
        if let Some(pw) = crate::release_info::find_verified_password([par2_name.as_str()], &[], check) {
            return Ok(Some(pw));
        }
    }
    for nzb_file in select::nzb_sources_with_par2(files, &par2_names) {
        let Some(body) = whole_file(ctx, nzb_file, left)? else { continue };
        if identify(&body) == Format::Nzb
            && let Some(meta_pw) = nzb::meta_password(&body)
            && let Some(pw) = crate::release_info::find_verified_password(std::iter::empty(), &[meta_pw], check)
        {
            return Ok(Some(pw));
        }
    }
    let mut nfo_files = select::nfo_sources_with_par2(files, &par2_names);
    // a file only guessed to be the NFO is matched to a kept NFO by its name alone
    let guessed = nfo_files.is_empty();
    if nfo_files.is_empty()
        && let Some(unid) = files
            .iter()
            .find(|f| select::classify(&f.filename) == Kind::Unidentified && f.expected.unwrap_or(0).max(f.parts_seen) == 1)
    {
        nfo_files.push(unid);
    }
    // an NFO read before, by the probe or another step, is used as it was kept, not fetched again
    let mut kept_nfo = crate::probe::nfo_store::stored(ctx.conn(), rel_id);
    for nfo_file in nfo_files {
        let par2_name = par2_names.get(&nfo_file.filename).map(String::as_str);
        let reused = match &kept_nfo {
            Some(k) if (k.file.is_some() || !guessed) && k.is_of(&nfo_file.filename, par2_name) => kept_nfo.take(),
            _ => None,
        };
        let text = match reused {
            Some(k) => k.text,
            None => {
                let Some(seg) = first_article(ctx, nfo_file)? else { continue };
                let Some(body) = budgeted(ctx, &seg, &nfo_file.filename, left)? else { continue };
                if identify(&body) != Format::Unknown {
                    continue;
                }
                let text = crate::probe::nfo_text(&body);
                kept.push(crate::probe::nfo_store::keep(rel_id, &nfo_file.filename, &text));
                text
            }
        };
        if let Some(pw) = crate::release_info::find_verified_password([text.as_str()], &[], check) {
            return Ok(Some(pw));
        }
    }
    Ok(None)
}

/// `with_nzb`'s join: the NZB's rows go with the archive's outcome, or its junk verdict in place of it.
fn merge_nzb(mut outcome: Outcome, nested: Outcome) -> Outcome {
    if outcome.junk.is_some() || nested.junk.is_some() {
        return if outcome.junk.is_some() { outcome } else { nested };
    }
    outcome.matched |= nested.matched;
    outcome.rerun |= nested.rerun;
    outcome.writes.extend(nested.writes);
    outcome
}

/// The list a posted NZB gave before, kept in the raw store, when nothing else in the release is to be read: no
/// articles fetched for it. Not an archive, soo it has no `raw_archives` row for `get_archive` to find.
fn restore_nzb(
    ctx: &StepCtx<'_>,
    files: &[FileInfo],
    content_id: &[u8],
    require_checked: bool,
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
    let pw = known_or_post_password(ctx, files, None)
        .or_else(|| saved.iter().find_map(|f| crate::release_info::find_password(&f.name)));
    if require_checked && pw.is_none() && !crate::raw_store::has_nfo_password_record(&rel.name, &rel.group, content_id) {
        return Ok(None);
    }
    let inner_files = saved.into_iter().map(|f| InnerFile { name: f.name, size: f.size }).collect();
    let listing = Listing { files: inner_files, complete: true, ..Default::default() };
    let mut outcome = finish_with_override(ctx, SOURCE_NZB, listing, None, Some(None), false, limits)?;
    if let Some(pw) = pw {
        add_pointed_password_writes(ctx, &mut outcome.writes, &pw, true);
    }
    Ok(Some(outcome))
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

    // what an NZB points to, in this shard, is read sooner: the next hop
    if source == SOURCE_NZB {
        for name in nzb::release_names(rows.iter().map(|r| r.name.as_str())) {
            writes.push(StepWrite::new(archive_store::MARK_POINTED, vec![name.clone().into(), id.into()]));
            writes.push(StepWrite::new(archive_store::RANK_POINTED.as_str(), vec![name.into(), id.into()]));
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

/// An archive's list kept in the raw store, and the list of an NZB it holds when that was kept too.
fn restore_archive(
    ctx: &StepCtx<'_>,
    saved: crate::raw_store::SavedArchive,
    content_id: &[u8],
    require_password_inspected: bool,
    limits: &ArchiveConfig,
) -> Result<Option<Outcome>, StepError> {
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
    let wrapper_check = saved.password_check.as_deref().and_then(super::PasswordTest::from_bytes);
    let listing = Listing {
        files,
        encrypted,
        headers_encrypted: saved.names_hidden,
        encryption: saved.encryption.and_then(super::Encryption::from_code),
        password_check: wrapper_check,
        complete: saved.listing_complete,
        damaged: saved.listing_damaged,
        need_more_tail: false,
    };
    let has_inner_nzb = listing.files.iter().any(|f| select::classify(&f.name) == Kind::Nzb);
    let inner_nzb_pw = listing
        .files
        .iter()
        .filter(|f| select::classify(&f.name) == Kind::Nzb)
        .find_map(|f| crate::release_info::find_password(&f.name));
    let mut outcome =
        finish_with_override(ctx, source, listing, password_override, Some(saved.content_fp), false, limits)?;
    let posted_files = ctx.files().unwrap_or_default();
    let wrapper_pw = encrypted
        .then(|| known_or_post_password(ctx, &posted_files, wrapper_check.as_ref()))
        .flatten();
    let rel = ctx.release();
    if require_password_inspected
        && encrypted
        && wrapper_pw.is_none()
        && !crate::raw_store::has_nfo_password_record(&rel.name, &rel.group, content_id)
    {
        return Ok(None);
    }
    if let Some(ref pw) = wrapper_pw {
        crate::raw_store::put_nfo_password(&rel.name, &rel.group, content_id, pw);
        outcome.writes.push(StepWrite::new(
            archive_store::SET_NFO_PASSWORD,
            vec![rel.id.into(), pw.clone().into()],
        ));
    }
    let nested = crate::raw_store::get_archive_files(&rel.name, &rel.group, content_id, SOURCE_NZB);
    if nested.is_empty() {
        if require_password_inspected
            && has_inner_nzb
            && (!encrypted || wrapper_pw.is_some())
            && !crate::raw_store::has_nested_nzb_password_record(&rel.name, &rel.group, content_id)
        {
            return Ok(None);
        }
        return Ok(Some(outcome));
    }
    if require_password_inspected
        && inner_nzb_pw.is_none()
        && !crate::raw_store::has_nested_nzb_password_record(&rel.name, &rel.group, content_id)
    {
        return Ok(None);
    }
    let files: Vec<InnerFile> = nested.into_iter().map(|f| InnerFile { name: f.name, size: f.size }).collect();
    let hop_pw = files.iter().find_map(|f| crate::release_info::find_password(&f.name));
    let listing = Listing { files, complete: true, ..Default::default() };
    let mut nested = finish_with_override(ctx, SOURCE_NZB, listing, None, Some(None), false, limits)?;
    let nzb_pw = crate::raw_store::get_nested_nzb_password(&rel.name, &rel.group, content_id)
        .or(inner_nzb_pw)
        .or(hop_pw)
        .or_else(|| if !encrypted { known_or_post_password(ctx, &posted_files, None) } else { None });
    if let Some(pw) = nzb_pw {
        add_pointed_password_writes(ctx, &mut nested.writes, &pw, !encrypted);
    }
    Ok(Some(merge_nzb(outcome, nested)))
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
/// The incomplete movie and TV releases the PAR2 step read no set for though they have PAR2 files: before it tried
/// a volume when the first file it picked was missing the articles holding the list, or picked a sample's set.
/// Through the category index, as `store::movie_tv_whole_again`. Reads every candidate's file names: a minute on a
/// big shard, soo off its writer (`postprocess::engine::par2_from_volumes`).
pub(crate) fn par2_without_set(conn: &rusqlite::Connection) -> rusqlite::Result<Vec<i64>> {
    conn.prepare(
        "select r.id from release_category c join releases r on r.id = c.release_id
         join pp_state p on p.release_id = r.id
         where (c.category between 2000 and 2999 or c.category between 5000 and 5999) and r.complete = 0
           and not exists (select 1 from par2_files x where x.release_id = r.id) and p.done & ?1 != 0
           and exists (select 1 from files f where f.release_id = r.id and f.filename like '%.par2')",
    )?
    .query_map([crate::postprocess::step::mask(PAR2_STEP)], |r| r.get(0))?
    .collect()
}

/// Asks for the PAR2 step again on these releases.
pub(crate) fn rerun_par2(conn: &rusqlite::Connection, ids: &[i64], now: i64) -> rusqlite::Result<()> {
    for id in ids {
        crate::postprocess::rerun_on(conn, *id, crate::postprocess::step::mask(PAR2_STEP), now)?;
    }
    Ok(())
}

/// The most PAR2 files the step reads a set from before it gives up on the release.
const PAR2_TRIES: usize = 3;

/// A set's list as one PAR2 file gives it, from its first articles or its last. None: the file gave no list, its
/// articles missing or not indexed.
fn read_set(ctx: &mut StepCtx<'_>, file: &FileInfo) -> Result<Option<par2::Inventory>, StepError> {
    let is_base = select::classify(&file.filename) == Kind::Par2Base;
    // a volume can start with a recovery slice and end with the list: either end will do to start from
    let last = |ctx: &StepCtx<'_>| -> Result<Option<SegmentRef>, StepError> {
        Ok(ctx.segments(file.id, &SegmentPick::Last(1)).map_err(retry)?.into_iter().last())
    };
    let (seg, from_end) = match first_article(ctx, file)? {
        Some(seg) => (seg, false),
        None if !is_base => match last(ctx)? {
            Some(seg) => (seg, true),
            None => return Ok(None),
        },
        None => return Ok(None),
    };
    let Some(mut body) = fetch_one(ctx, &seg, &file.filename)? else { return Ok(None) };
    let mut inventory = par2::inventory(&body);
    if !inventory.complete {
        // a base's next article, or a volume's other end
        let next = if is_base {
            ctx.segments(file.id, &SegmentPick::First(2)).map_err(retry)?.into_iter().last()
        } else if from_end {
            first_article(ctx, file)?
        } else {
            last(ctx)?
        };
        if let Some(next) = next.filter(|s| s.message_id != seg.message_id)
            && let Some(more) = fetch_one(ctx, &next, &file.filename)?
        {
            body.extend(more);
            inventory = par2::inventory(&body);
        }
    }
    Ok((!inventory.files.is_empty()).then_some(inventory))
}

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
        let sources = select::par2_sources(&files);
        if sources.is_empty() {
            return Ok(Outcome::default());
        }
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
        // every file of a set carries its list: when one is missing the articles that hold it, the next may not be.
        // Once a file gave some of a list, only files of its own set may better it: another set (a sample's) would
        // put its files in place of the ones found
        let mut best: Option<(par2::Inventory, String)> = None;
        for file in sources.into_iter().take(PAR2_TRIES) {
            let stem = select::par2_stem(&file.filename);
            if best.as_ref().is_some_and(|(_, set)| *set != stem) {
                continue;
            }
            let Some(inventory) = read_set(ctx, file)? else { continue };
            let complete = inventory.complete;
            if best.as_ref().is_none_or(|(b, _)| complete || inventory.files.len() > b.files.len()) {
                best = Some((inventory, stem));
            }
            if complete {
                break;
            }
        }
        let best = best.map(|(inventory, _)| inventory);
        let Some(inventory) = best else {
            if !saved_par2.is_empty() {
                let limits = self.limits.lock().unwrap().clone();
                return restore_par2(ctx, saved_par2, par2_meta, &limits);
            }
            return Ok(Outcome::default());
        };
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
                && !(saved.names_hidden && saved.files.is_empty())
                && let Some(outcome) = restore_archive(ctx, saved, &content_id, true, &limits)?
            {
                return Ok(outcome);
            }
            if let Some(outcome) = restore_nzb(ctx, &files, &content_id, true, &limits)? {
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
        // an NFO read looking for a password, kept for the probe and the other steps
        let mut nfo_writes = Vec::new();
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
                Format::Rar4 | Format::Rar5 => {
                    let mut listing = rar::list(&body);
                    let mut pw_found = None;
                    if listing.encrypted {
                        let check = listing.password_check.as_ref();
                        pw_found = harvest_password_from_bodies(ctx, &files, &mut left, check, &mut nfo_writes)?;
                        if let Some(ref pw) = pw_found
                            && listing.headers_encrypted
                        {
                            let decrypted = rar::list_with_password(&body, Some(pw));
                            if !decrypted.files.is_empty() {
                                listing = decrypted;
                            }
                        }
                    }
                    let wrapper_encrypted = listing.encrypted;
                    let has_inner_nzb = listing.files.iter().any(|f| select::classify(&f.name) == Kind::Nzb);
                    let pw_available = pw_found.is_some();
                    let inside = deep_nzb(
                        ctx,
                        &files,
                        &listing,
                        head.file,
                        (!head.probe).then_some((seg.message_id.as_str(), &body[..])),
                        follow::Budget::of(limits.max_bodies_per_release),
                    )?
                    .or_else(|| nzb_stored_in(&listing, &body, pw_found.as_deref()));
                    let mut outcome = finish(ctx, SOURCE_RAR, listing, &limits)?;
                    outcome.writes.append(&mut nfo_writes);
                    if let Some(pw) = pw_found {
                        crate::raw_store::put_nfo_password(&rel_name, &rel_group, &content_id, &pw);
                        outcome.writes.push(StepWrite::new(
                            archive_store::SET_NFO_PASSWORD,
                            vec![rel_id.into(), pw.into()],
                        ));
                    } else if wrapper_encrypted {
                        crate::raw_store::put_nfo_password(&rel_name, &rel_group, &content_id, "");
                    }
                    return with_nzb(ctx, outcome, inside, wrapper_encrypted, has_inner_nzb, pw_available, &limits);
                }
                Format::Zip => {
                    let listing = zip_list(ctx, &head, &body, &mut left)?;
                    let wrapper_encrypted = listing.encrypted;
                    let has_inner_nzb = listing.files.iter().any(|f| select::classify(&f.name) == Kind::Nzb);
                    let pw_found = if listing.encrypted {
                        let check = listing.password_check.as_ref();
                        harvest_password_from_bodies(ctx, &files, &mut left, check, &mut nfo_writes)?
                    } else {
                        None
                    };
                    let pw_available = pw_found.is_some();
                    let inside = deep_nzb(
                        ctx,
                        &files,
                        &listing,
                        head.file,
                        (!head.probe).then_some((seg.message_id.as_str(), &body[..])),
                        follow::Budget::of(limits.max_bodies_per_release),
                    )?
                    .or_else(|| nzb_stored_in(&listing, &body, pw_found.as_deref()));
                    let mut outcome = finish(ctx, SOURCE_ZIP, listing, &limits)?;
                    outcome.writes.append(&mut nfo_writes);
                    if let Some(pw) = pw_found {
                        crate::raw_store::put_nfo_password(&rel_name, &rel_group, &content_id, &pw);
                        outcome.writes.push(StepWrite::new(
                            archive_store::SET_NFO_PASSWORD,
                            vec![rel_id.into(), pw.into()],
                        ));
                    } else if wrapper_encrypted {
                        crate::raw_store::put_nfo_password(&rel_name, &rel_group, &content_id, "");
                    }
                    return with_nzb(ctx, outcome, inside, wrapper_encrypted, has_inner_nzb, pw_available, &limits);
                }
                Format::SevenZip => {
                    let listing = seven_list(ctx, &head, &seg, &body, &mut left)?;
                    let wrapper_encrypted = listing.encrypted;
                    let has_inner_nzb = listing.files.iter().any(|f| select::classify(&f.name) == Kind::Nzb);
                    let pw_found = if listing.encrypted {
                        let check = listing.password_check.as_ref();
                        harvest_password_from_bodies(ctx, &files, &mut left, check, &mut nfo_writes)?
                    } else {
                        None
                    };
                    let pw_available = pw_found.is_some();
                    let inside = deep_nzb(
                        ctx,
                        &files,
                        &listing,
                        head.file,
                        (!head.probe).then_some((seg.message_id.as_str(), &body[..])),
                        follow::Budget::of(limits.max_bodies_per_release),
                    )?
                    .or_else(|| nzb_stored_in(&listing, &body, pw_found.as_deref()));
                    let mut outcome = finish(ctx, SOURCE_7Z, listing, &limits)?;
                    outcome.writes.append(&mut nfo_writes);
                    if let Some(pw) = pw_found {
                        crate::raw_store::put_nfo_password(&rel_name, &rel_group, &content_id, &pw);
                        outcome.writes.push(StepWrite::new(
                            archive_store::SET_NFO_PASSWORD,
                            vec![rel_id.into(), pw.into()],
                        ));
                    } else if wrapper_encrypted {
                        crate::raw_store::put_nfo_password(&rel_name, &rel_group, &content_id, "");
                    }
                    return with_nzb(ctx, outcome, inside, wrapper_encrypted, has_inner_nzb, pw_available, &limits);
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
        let par2_names = select::par2_name_map(ctx.conn(), rel_id, &files);
        for file in select::nzb_sources_with_par2(&files, &par2_names) {
            let Some(body) = whole_file(ctx, file, &mut left)? else { continue };
            if identify(&body) == Format::Nzb {
                let complete = nzb::parse(&body).complete;
                let mut budget = follow::Budget::of(limits.max_bodies_per_release);
                let (followed_files, hop_pw) = follow::follow_with_password(ctx, body, &mut budget)?;
                let mut outcome = finish(
                    ctx,
                    SOURCE_NZB,
                    Listing { files: followed_files, complete, ..Default::default() },
                    &limits,
                )?;
                if let Some(pw) = hop_pw.or_else(|| known_or_post_password(ctx, &files, None)) {
                    add_pointed_password_writes(ctx, &mut outcome.writes, &pw, true);
                } else {
                    crate::raw_store::put_nfo_password(&rel_name, &rel_group, &content_id, "");
                }
                return Ok(outcome);
            }
        }
        let rel = ctx.release();
        if let Some(saved) = crate::raw_store::get_archive(&rel.name, &rel.group, &content_id, 0, false)
            && let Some(outcome) = restore_archive(ctx, saved, &content_id, false, &limits)?
        {
            return Ok(outcome);
        }
        // its articles may be gone since
        if let Some(outcome) = restore_nzb(ctx, &files, &content_id, false, &limits)? {
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
