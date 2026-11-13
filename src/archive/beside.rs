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

use rusqlite::{OptionalExtension, params};

use crate::postprocess::step::{BodyRequest, Got, Outcome, StepCtx, StepError, StepMutation, StepWrite, mask};
use crate::store::{FileInfo, SegmentPick};

use super::link::{BESIDE_SECS, ID_WINDOW, RESUME_SECS, beside_window, id_window, retry};

/// How far apart an NFO posted on its own and its release can say they were posted: it's often posted once the upload
/// is up, which for a big one is hours after its first file.
pub const NFO_SECS: i64 = 6 * 3600;
/// Rows of one name looked at, at most, for an NFO or a thumbnail.
const NAME_ROWS: i64 = 16;
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
const POSTER_PREFIX: usize = 8;
/// Index entries read, at most, for one range the thumbnail lookup scans.
const SCANNED: usize = 4_096;
/// Rows read either way of a release's id when its thumbnail can't be looked for by poster.
const NEIGHBOURS: i64 = 20_000;

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
    let Some(seg) = ctx.segments(file.id, &SegmentPick::First(1)).map_err(retry)?.into_iter().next() else {
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

/// The rows already found beside a release, with their one file each.
pub(crate) fn own_beside_legacy(conn: &rusqlite::Connection, id: i64) -> Result<Vec<(i64, FileInfo)>, StepError> {
    let rows: Vec<i64> = conn
        .prepare_cached("select row_id from beside_files where release_id = ?1 limit 16")
        .and_then(|mut stmt| stmt.query_map([id], |r| r.get(0))?.collect())
        .map_err(retry)?;
    let mut out = Vec::new();
    for row in rows {
        for file in crate::store::release_files(conn, row, 2).map_err(retry)? {
            out.push((row, file));
        }
    }
    Ok(out)
}

/// The release a row holding only `stem.nfo` was posted beside: a row of that name (or the name and a video's
/// extension, as a group's row of a post can be named by its video), in any group, holding files that aren't side
/// files, posted within `NFO_SECS`, nearest first.
pub(crate) fn nfo_owner(conn: &rusqlite::Connection, me: i64, stem: &str, posted: Option<&str>) -> Result<Option<i64>, StepError> {
    let Some(posted) = posted else { return Ok(None) };
    let Some((from, to)) = beside_window(posted, NFO_SECS) else { return Ok(None) };
    let rows = named_rows(conn, me, stem, (&from, &to, posted))?;
    for (id, name, _) in rows {
        let named = name == stem
            || name.strip_prefix(stem).is_some_and(|rest| rest.starts_with(" ["))
            || crate::store::VIDEO_EXTENSIONS.iter().any(|ext| name.strip_suffix(ext).is_some_and(|s| s == stem));
        if !named {
            continue;
        }
        let files = crate::store::release_files(conn, id, 64).map_err(retry)?;
        if files.iter().any(|f| !is_side_file(&f.filename)) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// A row of nothing but `stem.nfo`, in at most two articles, posted within `NFO_SECS` of the release, that nothing
/// else has taken, nearest first: (its id, its file).
pub(crate) fn nfo_beside(
    conn: &rusqlite::Connection,
    me: i64,
    stem: &str,
    posted: Option<&str>,
) -> Result<Option<(i64, FileInfo)>, StepError> {
    let Some(posted) = posted else { return Ok(None) };
    let Some((from, to)) = beside_window(posted, NFO_SECS) else { return Ok(None) };
    let want = format!("{stem}.nfo");
    for (id, name, _) in named_rows(conn, me, stem, (&from, &to, posted))? {
        // the row's own name, or the name with ` [n]` after it when the group had one of that name
        if name != stem && !name.strip_prefix(stem).is_some_and(|rest| rest.starts_with(" [")) {
            continue;
        }
        if let Some(file) = lone_file(conn, id, |f| f.filename.eq_ignore_ascii_case(&want))? {
            return Ok(Some((id, file)));
        }
    }
    Ok(None)
}

/// The one file of row `id`, in at most two articles, when it's the only one and `fits`, and the row is no other
/// release's small file and no linked upload's.
fn lone_file(
    conn: &rusqlite::Connection,
    id: i64,
    fits: impl Fn(&FileInfo) -> bool,
) -> Result<Option<FileInfo>, StepError> {
    let files = crate::store::release_files(conn, id, 2).map_err(retry)?;
    let [file] = &files[..] else { return Ok(None) };
    if file.expected.unwrap_or(1) > 2 || !fits(file) {
        return Ok(None);
    }
    let taken: bool = conn
        .prepare_cached(
            "select exists (select 1 from beside_files where row_id = ?1)
                 or exists (select 1 from upload_link where release_id = ?1)",
        )
        .and_then(|mut stmt| stmt.query_row([id], |r| r.get(0)))
        .map_err(retry)?;
    Ok((!taken).then(|| file.clone()))
}

/// Rows whose name starts with `name`, posted within `window` (from, to, and the release's own time), not `me`, nearest
/// in time first: (id, name, poster). By the name's index; the date is kept off its own.
fn named_rows(
    conn: &rusqlite::Connection,
    me: i64,
    name: &str,
    window: (&str, &str, &str),
) -> Result<Vec<(i64, String, Option<String>)>, StepError> {
    let mut stmt = conn
        .prepare_cached(
            "select r.id, r.name, r.poster from releases r
             where r.name >= ?1 and r.name < ?1 || char(127) and r.id != ?2
               and +r.posted_date between ?3 and ?4
             order by abs(julianday(r.posted_date) - julianday(?5)), r.id limit ?6",
        )
        .map_err(retry)?;
    let rows = stmt
        .query_map(params![name, me, window.0, window.1, window.2, NAME_ROWS], |r| {
            Ok((r.get(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default(), r.get(2)?))
        })
        .map_err(retry)?;
    rows.collect::<rusqlite::Result<_>>().map_err(retry)
}

/// The posters of a release's post, and its rows in other groups: its own poster, and those of the rows of its name
/// posted within a day, as a post cut across groups gives each group's row a poster of its own.
pub(crate) fn post_posters(
    conn: &rusqlite::Connection,
    name: &str,
    stem: &str,
    own: Option<&str>,
    posted: &str,
) -> Result<(Vec<String>, Vec<i64>), StepError> {
    let mut posters: Vec<String> = own.map(str::to_string).into_iter().collect();
    let mut rows = Vec::new();
    let Some((from, to)) = beside_window(posted, 24 * 3600) else { return Ok((posters, rows)) };
    let names: Vec<&str> = if name == stem { vec![name] } else { vec![name, stem] };
    for look in names {
        for (id, other, poster) in named_rows(conn, -1, look, (&from, &to, posted))? {
            if other != name && other != stem {
                continue;
            }
            if !rows.contains(&id) {
                rows.push(id);
            }
            if let Some(poster) = poster.filter(|p| !posters.contains(p)) {
                posters.push(poster);
            }
        }
    }
    Ok((posters, rows))
}

/// A row of one small picture, by a name posting tools give one, whose counter is the release's `/total` at a slot its
/// files don't hold, posted within `BESIDE_SECS` of it by one of its posts' posters (or one sharing their prefix),
/// that nothing else has taken. Nearest in time first. Looked for by poster, not by name: every upload's thumbnail
/// is named alike, but a poster's rows (or those of a prefix a tool makes up per upload) are few, and the poster
/// index holds the id, which bounds them to the ids near this release's without reading a row.
pub(crate) fn thumbnail_beside(
    conn: &rusqlite::Connection,
    me: i64,
    total: i64,
    slots: &HashSet<i64>,
    posters: &[String],
    posted: &str,
) -> Result<Option<i64>, StepError> {
    let Some((from, to)) = beside_window(posted, BESIDE_SECS) else { return Ok(None) };
    let (low, high) = id_window(me, ID_WINDOW);
    let by_poster = conn
        .query_row("select 1 from sqlite_master where type = 'index' and name = 'idx_release_poster'", [], |_| Ok(()))
        .is_ok();
    // ids from an index alone (its entries hold the id), near this release's, reading at most `SCANNED` entries: a
    // range of a busy poster, or of a name every upload's thumbnail has, can't cost more than that
    let mut ids: Vec<i64> = Vec::new();
    let mut have: HashSet<i64> = HashSet::new();
    let mut scan = |sql: &str, values: &[&dyn rusqlite::ToSql]| -> Result<(), StepError> {
        let mut stmt = conn.prepare_cached(sql).map_err(retry)?;
        let mut rows = stmt.query(values).map_err(retry)?;
        let mut read = 0;
        while let Some(row) = rows.next().map_err(retry)? {
            let id: i64 = row.get(0).map_err(retry)?;
            if (low..=high).contains(&id) && id != me && have.insert(id) {
                ids.push(id);
            }
            read += 1;
            if read >= SCANNED {
                break;
            }
        }
        Ok(())
    };
    let bare = |poster: &str| -> Option<String> {
        let (local, _) = poster.split_once('@')?;
        (local.is_ascii() && local.len() >= POSTER_PREFIX + 2 && !local.contains(['<', ' ', '"']))
            .then(|| local[..POSTER_PREFIX].to_string())
    };
    if by_poster {
        for poster in posters {
            // the poster itself, sought to the ids near this release's
            scan(
                "select id from releases indexed by idx_release_poster where poster = ?1 and id between ?2 and ?3",
                &[poster, &low, &high],
            )?;
            // a bare address: the rows sharing its local part's prefix, as a tool makes one up per upload
            if let Some(prefix) = bare(poster) {
                scan(
                    "select id from releases indexed by idx_release_poster where poster >= ?1 and poster < ?1 || char(127)",
                    &[&prefix],
                )?;
            }
        }
    }
    // a shard without the poster index (`--search-indexes`), or a poster written `Name <address>`, whose made-up
    // address no range of the poster column reaches: the rows saved nearest this one, either way, by their names.
    // Not by the name index: every upload's thumbnail is named alike, and its rows sort by a hash, not by when
    if !by_poster || posters.iter().any(|p| p.contains('<')) {
        for sql in [
            "select id, name from releases where id < ?1 and id >= ?2 order by id desc limit ?3",
            "select id, name from releases where id > ?1 and id <= ?2 order by id limit ?3",
        ] {
            let bound = if sql.contains("desc") { low } else { high };
            let mut stmt = conn.prepare_cached(sql).map_err(retry)?;
            let rows = stmt
                .query_map(params![me, bound, NEIGHBOURS], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default()))
                })
                .map_err(retry)?;
            for row in rows {
                let (id, name) = row.map_err(retry)?;
                if picture_named(&name) && have.insert(id) {
                    ids.push(id);
                }
            }
        }
    }
    let mut found: Vec<(i64, String, Option<String>, Option<f64>)> = Vec::new();
    let mut stmt = conn
        .prepare_cached(
            "select id, name, poster, abs(julianday(posted_date) - julianday(?2)) from releases
             where id = ?1 and posted_date between ?3 and ?4",
        )
        .map_err(retry)?;
    for id in ids {
        let row = stmt
            .query_row(params![id, posted, from, to], |r| {
                Ok((r.get(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default(), r.get(2)?, r.get(3)?))
            })
            .optional()
            .map_err(retry)?;
        found.extend(row);
    }
    let mut seen: HashSet<i64> = HashSet::new();
    let mut best: Option<(f64, i64)> = None;
    {
        for (id, row_name, poster, apart) in found {
            if !seen.insert(id) || !picture_named(&row_name) {
                continue;
            }
            if !poster.as_deref().is_some_and(|p| posters.iter().any(|mine| posters_akin(mine, p))) {
                continue;
            }
            let fits = |f: &FileInfo| {
                crate::images::is_image_name(&f.filename)
                    && f.subject
                        .as_deref()
                        .and_then(counter)
                        .is_some_and(|(k, n)| n == total && (1..=total).contains(&k) && !slots.contains(&k))
            };
            if lone_file(conn, id, fits)?.is_none() {
                continue;
            }
            let apart = apart.unwrap_or(f64::MAX);
            if best.is_none_or(|(b, _)| apart < b) {
                best = Some((apart, id));
            }
        }
    }
    Ok(best.map(|(_, id)| id))
}

/// Whether a row's name is one of `PICTURE_NAMES`, in any case, or one with the part of a hash a name another row
/// has already gets (`thumbnail.jpg [01234567]`).
fn picture_named(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let bare = match lower.split_once(" [") {
        Some((head, rest)) if rest.ends_with(']') && rest[..rest.len() - 1].bytes().all(|b| b.is_ascii_digit()) => head,
        _ => lower.as_str(),
    };
    PICTURE_NAMES.contains(&bare)
}

/// A subject's file counter `[k/N]`.
fn counter(subject: &str) -> Option<(i64, i64)> {
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
fn is_side_file(name: &str) -> bool {
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

    fn shard() -> (tempfile::TempDir, rusqlite::Connection) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.s0.db");
        crate::store::create_shard(&path).unwrap();
        let conn = crate::db::open_shard(&path).unwrap();
        (dir, conn)
    }

    fn row(conn: &rusqlite::Connection, id: i64, name: &str, group: &str, poster: &str, posted: &str, files: &[&str]) {
        conn.execute(
            "insert into releases (id, name, group_name, poster, posted_date, size, parts, file_total)
             values (?1, ?2, ?3, ?4, ?5, 1, 1, 8)",
            params![id, name, group, poster, posted],
        )
        .unwrap();
        for (n, f) in files.iter().enumerate() {
            let (filename, subject) = f.split_once('|').unwrap_or((f, ""));
            conn.execute(
                "insert into files (id, release_id, filename, subject, expected, seen) values (?1, ?2, ?3, ?4, 1, x'01')",
                params![id * 10 + n as i64, id, filename, subject],
            )
            .unwrap();
        }
    }

    const FILM: &str = "Men.In.Black.3.2012.2160p.MA.WEB-DL.DDP5.1.HDR.H.265-ALLEYESONME";

    #[test]
    fn an_nfo_posted_on_its_own_to_another_group_finds_its_release_and_back() {
        let (_dir, conn) = shard();
        let mkv = format!("{FILM}.mkv");
        let par2 = format!("{FILM}.par2");
        let nfo = format!("{FILM}.nfo");
        row(&conn, 1, FILM, "a.b.sleazemovies", "ZbivKJ385zX890Qa@x.y", "2026-11-08 20:58:00", &[&mkv, &par2]);
        row(&conn, 2, FILM, "a.b.boneless", "ZbivKJ385zX890Qb@x.y", "2026-11-08 18:37:29", &[&nfo]);
        // another release's NFO of the same name a week on, and a row of the name that's only side files
        row(&conn, 3, FILM, "a.b.other", "x@y", "2026-11-15 18:37:29", &[&nfo]);
        row(&conn, 4, FILM, "a.b.text", "x@y", "2026-11-08 18:40:00", &[&nfo, "x.sfv"]);
        assert_eq!(nfo_owner(&conn, 2, FILM, Some("2026-11-08 18:37:29")).unwrap(), Some(1));
        let (found, file) = nfo_beside(&conn, 1, FILM, Some("2026-11-08 20:58:00")).unwrap().unwrap();
        assert_eq!((found, file.filename.as_str()), (2, nfo.as_str()));
        // once it's found beside one release, no other takes it
        conn.execute("insert into beside_files (row_id, release_id) values (2, 1)", []).unwrap();
        assert_eq!(nfo_beside(&conn, 1, FILM, Some("2026-11-08 20:58:00")).unwrap(), None);
        assert_eq!(nfo_owner(&conn, 3, FILM, Some("2026-11-15 18:37:29")).unwrap(), None, "too far apart");
    }

    #[test]
    fn a_thumbnail_counted_by_the_release_joins_it() {
        let (_dir, conn) = shard();
        let mkv = format!("{FILM}.mkv|[7/8] - \"{FILM}.mkv\" yEnc (1/4780)");
        let par2 = format!("{FILM}.par2|[1/8] - \"{FILM}.par2\" yEnc (1/1)");
        row(
            &conn,
            1,
            FILM,
            "a.b.alias",
            "ZbivKJ385zX890QucX2ivQLHn0h@5caaC6uY.W4f",
            "2026-11-08 20:58:00",
            &[&mkv, &par2],
        );
        row(&conn, 5, FILM, "a.b.rail", "ZbivKJ385zX890QzzX2ivQLHn0h@5caaC6uY.W4f", "2026-11-08 20:57:00", &[&mkv]);
        let thumb = "thumbnail.jpg|[8/8] - \"thumbnail.jpg\" yEnc (1/1) 476514";
        row(
            &conn,
            2,
            "thumbnail.jpg",
            "a.b.sleazemovies",
            "ZbivKJ385zX890Qm0z0Yit@8RaSuWBD.g08",
            "2026-11-08 20:40:00",
            &[thumb],
        );
        // another upload's thumbnail at the same time, by another poster; one of another count
        let other = "thumbnail.jpg|[5/5] - \"thumbnail.jpg\" yEnc (1/1)";
        row(
            &conn,
            3,
            "thumbnail.jpg [00000001]",
            "a.b.alias",
            "dsOzhB385zX890QHcQ@x.y",
            "2026-11-08 20:58:00",
            &[thumb],
        );
        row(&conn, 4, "thumbnail.jpg [00000002]", "a.b.alias", "ZbivKJ385zX890Qq@x.y", "2026-11-08 20:58:30", &[other]);
        let posted = "2026-11-08 20:58:00";
        let (posters, rows) =
            post_posters(&conn, FILM, FILM, Some("ZbivKJ385zX890QucX2ivQLHn0h@5caaC6uY.W4f"), posted).unwrap();
        assert_eq!(posters.len(), 2, "its own and the rail row's: {posters:?}");
        assert_eq!(rows.len(), 2, "both groups' rows");
        let slots: HashSet<i64> = [7, 1].into();
        assert_eq!(thumbnail_beside(&conn, 1, 8, &slots, &posters, posted).unwrap(), Some(2));
        let taken: HashSet<i64> = [7, 1, 8].into();
        assert_eq!(thumbnail_beside(&conn, 1, 8, &taken, &posters, posted).unwrap(), None, "its slot is held");
        assert_eq!(thumbnail_beside(&conn, 1, 9, &slots, &posters, posted).unwrap(), None, "another count");
        // and the same by poster, on a shard with the poster index
        conn.execute_batch("create index idx_release_poster on releases(poster)").unwrap();
        assert_eq!(thumbnail_beside(&conn, 1, 8, &slots, &posters, posted).unwrap(), Some(2));
        assert_eq!(thumbnail_beside(&conn, 1, 8, &taken, &posters, posted).unwrap(), None, "its slot is held");
        assert_eq!(thumbnail_beside(&conn, 1, 9, &slots, &posters, posted).unwrap(), None, "another count");
        let plan: String = conn
            .prepare(
                "explain query plan select r.id from releases r indexed by idx_release_poster
                 where r.poster >= 'ZbivKJ38' and r.poster < 'ZbivKJ38' || char(127) and r.id between 0 and 9",
            )
            .unwrap()
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(plan.contains("idx_release_poster (poster>? AND poster<?)"), "{plan}");
    }
}
