//! Small files posted beside a release that its PAR2 set doesn't name, found by name and counter (`link` finds the ones
//! a set names by their bytes).
//!
//! - The NFO, posted on its own after the upload, often to another group, under the release's own name:
//!   `[01/01] - "Some.Film.2160p-GRP.nfo" yEnc (1/1)`. Its row is named as the release (`.nfo` comes off a row's
//!   name), holds that one file, and was posted within `NFO_SECS` of it. Its row finds the release when it comes in
//!   after it, and the release finds the row when it comes in after the NFO. Either way the NFO's text is read once and
//!   kept for the release (`probe::nfo_store`), soo its page shows it.
//! - The thumbnail an upload counts as one of its files, `[8/8] - "thumbnail.jpg" yEnc (1/1)`, under a subject without
//!   the release's name and a poster of its own, soo it's a row of its own. Its counter is the release's (`/8`), it
//!   holds a slot the release's files don't, it was posted within `link::BESIDE_SECS` of the release, and its poster is
//!   one of the release's posts' posters or shares their prefix (tools that make a poster up for each file keep a
//!   prefix for the upload). Such a row is named after its file, soo it's looked up by the names posting tools give
//!   one (`PICTURE_NAMES`).
//!
//! A row found is kept as found beside the release (`link::BESIDE_FILES`): its post's NZB takes it in, it's never
//! frozen, and the pictures step reads it.

use std::collections::HashSet;
use std::sync::LazyLock;

use crate::postprocess::step::{BodyRequest, Got, Outcome, StepCtx, StepError, StepMutation, StepWrite, mask};
use crate::catalog_facts::{FileInfo, SegmentPick};

use super::link::RESUME_SECS;

/// How far apart an NFO posted on its own and its release can say they were posted: it's often posted once the upload
/// is up, which for a big one is hours after its first file.
pub const NFO_SECS: i64 = 6 * 3600;
/// Rows of one name looked at, at most, for an NFO or a thumbnail.
pub(crate) const NAME_ROWS: i64 = 16;
/// The names posting tools give the one picture they post with an upload. Each is one lookup by the name's index.
const PICTURE_NAMES: &[&str] = &[
    "thumbnail.jpg",
    "thumbnail.png",
    "thumb.jpg",
    "00-thumbnail.jpg",
    "cover.jpg",
    "cover.png",
    "poster.jpg",
    "folder.jpg",
    "front.jpg",
    "preview.jpg",
    "screens.jpg",
    "screenshot.jpg",
    "screenshots.jpg",
];
/// Files a post can be short of, at most, for a thumbnail to be looked for: the thumbnail and one more.
const MAX_SHORT: usize = 2;
/// Leading characters two posters' local parts share, at least, to be one upload's when they aren't the same.
pub(crate) const POSTER_PREFIX: usize = 8;
/// Index entries read, at most, for one range the thumbnail lookup scans.
pub(crate) const SCANNED: usize = 4_096;
/// Rows read either way of a release's id when its thumbnail can't be looked for by poster.
pub(crate) const NEIGHBOURS: i64 = 20_000;

static FILE_COUNTER: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\[(\d+)\s*/\s*(\d+)\]").unwrap());

/// What finding small files by name and counter wrote, and whether the pictures step should run again.
pub fn by_name(ctx: &mut StepCtx<'_>, files: &[FileInfo], picture_found: bool) -> Result<Outcome, StepError> {
    if let [only] = files
        && is_nfo(&only.filename)
    {
        return nfo_row(ctx, only);
    }
    if !files.iter().any(|f| !is_side_file(&f.filename)) {
        return Ok(Outcome::default());
    }
    let me = ctx.release().clone();
    let stem = crate::release_info::post_stem(&me.name);
    let beside = ctx.data().beside_rows_with_files(me.id)?;
    let mut out = Outcome::default();
    // the NFO: one found already is read when it hasn't been; one posted on its own is looked for when there's none
    let own_nfo = files.iter().any(|f| is_nfo(&f.filename));
    let found_nfo = match beside.iter().find(|(_, f)| is_nfo(&f.filename)) {
        Some((row, file)) => Some((*row, file.clone(), false)),
        None if !own_nfo && !stem.is_empty() => {
            ctx.data().find_beside_nfo(me.id, &stem, me.posted_date.as_deref())?.map(|(row, file)| (row, file, true))
        }
        None => None,
    };
    if let Some((row, file, new)) = found_nfo {
        if new {
            out.writes.push(StepWrite::for_mutation(me.id, StepMutation::PlaceBesideFile { row_id: row,owner_id: me.id,par2_file: None }));
        }
        out.matched = true;
        if ctx.data().stored_nfo(me.id)?.is_none() {
            // one that can't be read now (out of budget, a server down) is read on a later run: the step waits, which
            // holds back none of the release's later steps, and keeps the reruns this run asks for
            match read_nfo(ctx, &file)? {
                NfoRead::Text(text) => out.writes.push(crate::probe::nfo_store::keep(me.id, &file.filename, &text)),
                NfoRead::Later => out.wait_until = Some(chrono::Utc::now().timestamp() + RESUME_SECS),
                NfoRead::Never => {}
            }
        }
    }
    // the thumbnail the counter counts
    let have_picture = picture_found
        || files.iter().any(|f| crate::images::is_image_name(&f.filename))
        || beside.iter().any(|(_, f)| crate::images::is_image_name(&f.filename));
    if !have_picture
        && let Some(total) = me.file_total.filter(|n| *n >= 2)
        && let Some(posted) = me.posted_date.as_deref()
    {
        let (posters, rows) = ctx.data().post_posters(&me.name, &stem, me.poster.as_deref(), posted)?;
        // the slots the whole post holds, across its groups' rows: only a post a file or two short is looked at
        let mut slots: HashSet<i64> =
            files.iter().filter_map(|f| counter(f.subject.as_deref()?)).map(|(k, _)| k).collect();
        for row in rows {
            for f in ctx.data().counter_files(row)? {
                slots.extend(f.subject.as_deref().and_then(counter).map(|(k, _)| k));
            }
        }
        let short = (1..=total).filter(|k| !slots.contains(k)).count();
        if (1..=MAX_SHORT).contains(&short)
            && let Some(row) = ctx.data().find_thumbnail(me.id, total, &slots, &posters, posted)?
        {
            out.writes.push(StepWrite::for_mutation(me.id, StepMutation::PlaceBesideFile { row_id: row,owner_id: me.id,par2_file: None }));
            out.matched = true;
            out.rerun |= mask(crate::images::step::IMAGES_STEP);
        }
    }
    Ok(out)
}

/// A row of one NFO: the release it was posted beside, found once; its text read for that release while it has none.
fn nfo_row(ctx: &mut StepCtx<'_>, file: &FileInfo) -> Result<Outcome, StepError> {
    if file.expected.unwrap_or(1) > 2 {
        return Ok(Outcome::default());
    }
    let me = ctx.release().clone();
    let known = ctx.data().beside_owner_local(me.id)?;
    let mut out = Outcome::default();
    let owner = match known {
        Some(owner) => owner,
        None => {
            let Some(stem) = strip_nfo(&file.filename) else { return Ok(out) };
            let Some(owner) = ctx.data().find_nfo_owner(me.id, stem, me.posted_date.as_deref())? else { return Ok(out) };
            out.writes.push(StepWrite::for_mutation(me.id, StepMutation::PlaceBesideFile { row_id: me.id,owner_id: owner,par2_file: None }));
            owner
        }
    };
    out.matched = true;
    if ctx.data().stored_nfo(owner)?.is_none() {
        match read_nfo(ctx, file)? {
            NfoRead::Text(text) => out.writes.push(StepWrite::for_mutation(me.id, StepMutation::KeepNfoForLegacyTarget {
                target_id: owner,file: file.filename.clone(),text,
            })),
            // this row's own later steps are all it holds back
            NfoRead::Later => out.wait_until = Some(chrono::Utc::now().timestamp() + RESUME_SECS),
            NfoRead::Never => {}
        }
    }
    Ok(out)
}

/// What reading an NFO came to.
enum NfoRead {
    Text(String),
    /// not in this run: out of bodies, or the servers didn't answer
    Later,
    /// gone from the servers, too big for an NFO, or its first article not indexed
    Never,
}

/// An NFO's text, from its first article.
fn read_nfo(ctx: &mut StepCtx<'_>, file: &FileInfo) -> Result<NfoRead, StepError> {
    let Some(seg) = ctx.segments(file.id, &SegmentPick::First(1)).map_err(StepError::from_inventory)?.into_iter().next() else {
        return Ok(NfoRead::Never);
    };
    // a later article's text isn't the NFO's start, which the next readers go by
    if seg.part.is_some_and(|p| p > 1) {
        return Ok(NfoRead::Never);
    }
    let wanted = [BodyRequest {
        message_id: seg.message_id.clone(),
        file: file.filename.clone(),
        max_bytes: Some(super::ARTICLE_CAP),
    }];
    let used_before = ctx.used_bodies;
    let got = match ctx.fetch_bodies(&wanted) {
        Ok(got) => got.into_iter().next().map(|b| b.got),
        // out of this minute's budget: later. A server down or an article it won't give is the lane's to back off
        // from (`StepError::Transient`), not a wait of this step's that would come back every few minutes for ever
        Err(StepError::OutOfBudget) => return Ok(NfoRead::Later),
        Err(e) => return Err(e),
    };
    Ok(match got {
        Some(Got::Body(body)) if body.len() < crate::media::steps::NFO_READ => {
            NfoRead::Text(crate::probe::nfo_text(&body))
        }
        // past the release's budget after fetches of this run's own; with none, a budget of nothing would wait for ever
        Some(Got::Refused(why)) if why == crate::postprocess::bodies::PAST_BUDGET && used_before > 0 => NfoRead::Later,
        _ => NfoRead::Never,
    })
}

pub(crate) fn picture_named(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let bare = match lower.split_once(" [") {
        Some((head, rest)) if rest.ends_with(']') && rest[..rest.len() - 1].bytes().all(|b| b.is_ascii_digit()) => head,
        _ => lower.as_str(),
    };
    PICTURE_NAMES.contains(&bare)
}

/// A subject's file counter `[k/N]`.
pub(crate) fn counter(subject: &str) -> Option<(i64, i64)> {
    let c = FILE_COUNTER.captures(subject)?;
    Some((c[1].parse().ok()?, c[2].parse().ok()?))
}

/// Whether two posters are one upload's: the same, or their local parts sharing their first `POSTER_PREFIX`
/// characters, as a tool that makes a poster up for each file keeps a prefix for the upload.
pub fn posters_akin(a: &str, b: &str) -> bool {
    let address = |p: &str| -> String {
        let inner = p.rsplit_once('<').map_or(p, |(_, rest)| rest.trim_end_matches('>'));
        inner.split('@').next().unwrap_or(inner).trim().to_ascii_lowercase()
    };
    if a.trim().eq_ignore_ascii_case(b.trim()) {
        return true;
    }
    let (a, b) = (address(a), address(b));
    a.len() >= POSTER_PREFIX + 2
        && b.len() >= POSTER_PREFIX + 2
        && a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count() >= POSTER_PREFIX
}

fn is_nfo(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".nfo")
}

fn strip_nfo(name: &str) -> Option<&str> {
    let stem = name.get(..name.len().checked_sub(4)?)?;
    (is_nfo(name) && !stem.is_empty()).then_some(stem)
}

/// A file that describes a release and carries none of it: text, a checksum list, a picture, a PAR2 set.
pub(crate) fn is_side_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map_or("", |(_, ext)| ext);
    crate::frozen::rules::SIDECAR_FILES.contains(&ext)
        || crate::frozen::rules::SIDECAR_IMAGES.contains(&ext)
        || ext == "par2"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posters_of_one_upload() {
        assert!(posters_akin("ZbivKJ385zX890QucX2ivQLHn0h@5caaC6uY.W4f", "ZbivKJ385zX890Qm0z0YitxFHBA@8RaSuWBD.g08"));
        assert!(posters_akin("Poster <poster@example.com>", "poster <POSTER@example.com>"));
        assert!(!posters_akin("ZbivKJ385zX890QucX2ivQLHn0h@5caaC6uY.W4f", "dsOzhB385zX890QHcQD10x6ix8l@gS9K7mrI.LVY"));
        assert!(!posters_akin("abc@x", "abd@x"), "short ones only when the same");
    }

    #[test]
    fn counters_and_names() {
        assert_eq!(counter(r#"[8/8] - "thumbnail.jpg" yEnc (1/1) 476514"#), Some((8, 8)));
        assert_eq!(counter(r#"[01/01] - "A.nfo" yEnc (1/1)"#), Some((1, 1)));
        assert_eq!(counter("no counter"), None);
        assert_eq!(strip_nfo("Some.Film-GRP.nfo"), Some("Some.Film-GRP"));
        assert_eq!(strip_nfo(".nfo"), None);
        assert!(is_side_file("thumbnail.JPG") && is_side_file("a.vol01+02.par2") && !is_side_file("a.mkv"));
        assert!(picture_named("Thumbnail.jpg") && picture_named("thumbnail.jpg [01234567]"));
        assert!(!picture_named("thumbnail.jpg [x]") && !picture_named("screen01.jpg"));
    }

}
