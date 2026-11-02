//! The files of one upload, linked back together. A fully obfuscated upload posts every file under its own random
//! subject and poster, soo each file is a release row of its own: the recovery volumes get the release's name from their
//! PAR2 set, and the files they repair sit in rows with no name at all. The set lists each file it repairs with its exact
//! size and the MD5 of its first 16 KiB, which a file's first article shows too. A row whose file matches is that file,
//! and it leads the upload: the volume rows link to it, and it takes their name.

use std::collections::HashMap;
use std::sync::LazyLock;

use md5::{Digest, Md5};
use rusqlite::{OptionalExtension, params};

use crate::config::{ArchiveConfig, Config};
use crate::par2::{self, Par2File};
use crate::postprocess::schema::ShardTable;
use crate::postprocess::step::{
    BodyRequest, Got, Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId, StepMask, StepWrite, mask,
};
use crate::store::{FileInfo, SegmentPick, SegmentRef};

use super::names::best_name;
use super::steps::{fetch_one, first_article};
use super::store::SOURCE_PAR2;
use super::{ARTICLE_CAP, NAME_SOURCE_PAR2, select};

pub const LINK_STEP: StepId = StepId(7);

/// Each row of a linked upload, the lead's own included, and the row it's linked to.
pub const UPLOAD_LINK: ShardTable = ShardTable {
    name: "upload_link",
    create: "create table if not exists upload_link (
        release_id INTEGER PRIMARY KEY,
        lead_id    INTEGER NOT NULL,
        recovery   INTEGER NOT NULL DEFAULT 0
    )",
    indexes: &[("upload_link_lead", "create index if not exists upload_link_lead on upload_link(lead_id)")],
    release_key: Some("release_id"),
    added: &[],
};

pub const LINK_LEAD: &str = "insert into upload_link (release_id, lead_id) values (?1, ?1)
    on conflict(release_id) do nothing";
/// ?3: 1 for a row of recovery data, 0 for one of the files it repairs
pub const LINK_MEMBER: &str = "insert into upload_link (release_id, lead_id, recovery) values (?1, ?2, ?3)
    on conflict(release_id) do update set lead_id = excluded.lead_id, recovery = excluded.recovery";
/// A recovery volume isn't a copy of the files it repairs, whatever its set's list made it look like.
const CLEAR_CONTENT: &str = crate::duplicates::CLEAR_CONTENT;
/// The lead's name, from the set, unless something better named it.
const NAME_LEAD: &str = "update releases set display_name = ?2, name_source = ?3
    where id = ?1 and display_name is null and coalesce(name_source, 0) <= ?3";
/// The lead's steps that go by its name, to run again now it has one.
const RERUN_LEAD: &str = "insert into pp_state (release_id, net_wanted, rerun_gen, rerun_mask)
        values (?1, case when ?3 != 0 then 1 end, 1, ?2)
    on conflict(release_id) do update set done = done & ~?2, failed = failed & ~?2,
        net_wanted = case when ?3 != 0 then 1 else net_wanted end,
        net_next_at = case when ?3 != 0 then 0 else net_next_at end,
        rerun_gen = rerun_gen + 1, rerun_mask = rerun_mask | ?2";
const QUEUE_LEAD: &str = "insert into pp_queue (release_id, due_at) values (?1, ?2)
    on conflict(release_id) do update set due_at = min(due_at, excluded.due_at)";

/// Files smaller than this aren't looked for: an NFO or a sample says too little, and costs a fetch all the same.
const MIN_LINKED: u64 = 1 << 20;
/// Without the first 16 KiB, a file is known by its exact size alone, which only a big one makes safe.
const MIN_SIZE_ONLY: u64 = 64 << 20;
/// How far apart, in ids of this shard, an upload's rows can be: they're read from one run of articles, but other
/// groups' saves come in between.
const ID_WINDOW: i64 = 400_000;
/// How far apart in days an upload's files can say they were posted. Real ones are an hour or two.
const DAY_WINDOW: f64 = 1.0;
/// Rows looked at, at most, and articles fetched to check them, per run.
const MAX_CANDIDATES: usize = 5_000;
const MAX_CHECKS: usize = 16;
/// A volume whose files haven't turned up looks again this often, while it's this new.
const LOOK_AGAIN_SECS: i64 = 6 * 3600;
const LOOK_AGAIN_FOR_DAYS: f64 = 3.0;

/// The steps a rename runs again, database ones and network ones.
static ON_RENAME: LazyLock<(StepMask, StepMask)> = LazyLock::new(|| {
    crate::postprocess::steps::all().iter().filter(|s| s.reruns().on_rename).fold((0, 0), |(db, net), s| {
        if s.needs() == Needs::Database { (db | mask(s.id()), net) } else { (db, net | mask(s.id())) }
    })
});

fn retry(e: impl std::fmt::Display) -> StepError {
    StepError::Retry(e.to_string())
}

fn is_par2_name(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".par2")
}

/// The files of a set worth looking for: big enough, and each told apart from the rest by its size and first 16 KiB.
/// Two the same would be a guess.
pub fn wanted(files: &[Par2File]) -> Vec<&Par2File> {
    let mut seen: HashMap<(u64, [u8; 16]), usize> = HashMap::new();
    for f in files {
        *seen.entry((f.size, f.md5_16k)).or_default() += 1;
    }
    files.iter().filter(|f| f.size >= MIN_LINKED && seen[&(f.size, f.md5_16k)] == 1).collect()
}

/// Whether a file of `expected` articles can be one of `size` bytes, from what its articles weigh on the wire: `bytes`
/// over `parts`, which counts a header and yEnc's few percent on top of what each holds.
pub fn parts_fit(size: u64, expected: i64, bytes: i64, parts: i64) -> bool {
    if expected <= 0 || bytes <= 0 || parts <= 0 {
        return false;
    }
    let on_wire = bytes as f64 / parts as f64;
    // what an article holds, at most and at least, for that much on the wire
    let (most, least) = (on_wire / 1.005, on_wire / 1.08);
    let size = size as f64;
    let (fewest, most_parts) = ((size / most).ceil() as i64, (size / least).ceil() as i64);
    (fewest - 1..=most_parts + 1).contains(&expected)
}

/// What a file's article says of the whole file: its size, and the MD5 of its first 16 KiB when it's the first article.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub size: Option<u64>,
    pub md5_16k: Option<[u8; 16]>,
}

impl Seen {
    pub fn of_article(raw: &[u8]) -> Seen {
        let article = crate::probe::yenc::split_article(raw);
        let Some(y) = crate::probe::yenc::parse(&article.lines) else { return Seen::default() };
        let first = y.begin.is_none_or(|b| b <= 1) && y.part.is_none_or(|p| p <= 1);
        let md5_16k = first.then(|| crate::nntp::yenc_decode(&article.lines)).flatten().and_then(|body| {
            let want = y.file_size.map_or(16_384, |size| size.min(16_384)) as usize;
            body.get(..want).map(|head| Md5::digest(head).into())
        });
        Seen { size: y.file_size, md5_16k }
    }

    /// Whether this is the set's file: the same size to the byte, and the same first 16 KiB when that's known.
    pub fn is(&self, file: &Par2File) -> bool {
        self.size == Some(file.size)
            && match self.md5_16k {
                Some(md5) => md5 == file.md5_16k,
                None => file.size >= MIN_SIZE_ONLY,
            }
    }
}

/// A row that may hold one of the set's files.
#[derive(Clone, Debug)]
struct Candidate {
    id: i64,
    bytes: i64,
    parts: i64,
    file: FileInfo,
    /// days from the volume's own date
    apart: f64,
}

#[derive(Default)]
pub struct LinkStep {
    limits: std::sync::Mutex<ArchiveConfig>,
}

impl LinkStep {
    /// The set's file, when the row is nothing but PAR2: files all named .par2, or one file whose set was read from it.
    fn par2_file(&self, ctx: &StepCtx<'_>, files: &[FileInfo]) -> Result<Option<FileInfo>, StepError> {
        if files.iter().all(|f| is_par2_name(&f.filename)) {
            return Ok(select::par2_source(files).cloned());
        }
        if files.len() != 1 {
            return Ok(None);
        }
        let listed: bool = ctx
            .conn()
            .prepare_cached("select exists (select 1 from archive_inner_files where release_id = ? and source = ?)")
            .and_then(|mut stmt| stmt.query_row(params![ctx.release().id, SOURCE_PAR2], |r| r.get(0)))
            .unwrap_or(false);
        Ok(listed.then(|| files[0].clone()))
    }

    fn inventory(&self, ctx: &mut StepCtx<'_>, file: &FileInfo) -> Result<par2::Inventory, StepError> {
        let Some(seg) = first_article(ctx, file)? else { return Ok(par2::Inventory::default()) };
        let Some(mut body) = fetch_one(ctx, &seg, &file.filename)? else { return Ok(par2::Inventory::default()) };
        let mut inventory = par2::inventory(&body);
        if !inventory.complete {
            let last = ctx.segments(file.id, &SegmentPick::Last(1)).map_err(retry)?.into_iter().last();
            if let Some(last) = last.filter(|s| s.message_id != seg.message_id)
                && let Some(more) = fetch_one(ctx, &last, &file.filename)?
            {
                body.extend(more);
                inventory = par2::inventory(&body);
            }
        }
        Ok(inventory)
    }

    fn candidates(&self, ctx: &StepCtx<'_>) -> Result<Vec<Candidate>, StepError> {
        let me = ctx.release();
        let Some(posted) = me.posted_date.clone() else { return Ok(Vec::new()) };
        let span = ID_WINDOW * crate::store::SHARDS as i64;
        let mut stmt = ctx
            .conn()
            .prepare_cached(
                "select r.id, coalesce(r.size, 0), coalesce(r.parts, 0), f.id, f.filename, f.expected,
                        abs(julianday(r.posted_date) - julianday(?5))
                 from releases r join files f on f.release_id = r.id
                 where r.id between ?1 and ?2 and r.id != ?3 and r.group_name = ?4
                   and abs(julianday(r.posted_date) - julianday(?5)) <= ?6
                   and not exists (select 1 from files g where g.release_id = r.id and g.id != f.id)
                   and not exists (select 1 from upload_link l where l.release_id = r.id and l.lead_id != r.id)
                 limit ?7",
            )
            .map_err(retry)?;
        let rows = stmt
            .query_map(
                params![me.id - span, me.id + span, me.id, me.group, posted, DAY_WINDOW, MAX_CANDIDATES as i64],
                |r| {
                    Ok(Candidate {
                        id: r.get(0)?,
                        bytes: r.get(1)?,
                        parts: r.get(2)?,
                        file: FileInfo {
                            id: r.get(3)?,
                            filename: r.get(4)?,
                            subject: None,
                            subject_part: None,
                            expected: r.get(5)?,
                            file_total: None,
                            parts_seen: 0,
                            touched_at: None,
                            sealed: false,
                        },
                        apart: r.get::<_, Option<f64>>(6)?.unwrap_or(DAY_WINDOW),
                    })
                },
            )
            .map_err(retry)?;
        rows.collect::<rusqlite::Result<_>>().map_err(retry)
    }

    /// The article a file is checked by: its first, or any it has when that's missing.
    fn article_of(&self, ctx: &StepCtx<'_>, file: &FileInfo) -> Result<Option<SegmentRef>, StepError> {
        if let Some(first) = first_article(ctx, file)? {
            return Ok(Some(first));
        }
        Ok(ctx.segments(file.id, &SegmentPick::First(1)).map_err(retry)?.into_iter().next())
    }

    fn check(&self, ctx: &mut StepCtx<'_>, candidate: &Candidate) -> Result<Seen, StepError> {
        let Some(seg) = self.article_of(ctx, &candidate.file)? else { return Ok(Seen::default()) };
        let wanted = BodyRequest {
            message_id: seg.message_id.clone(),
            file: candidate.file.filename.clone(),
            max_bytes: Some(ARTICLE_CAP),
        };
        Ok(match ctx.fetch_article(&wanted)?.got {
            Got::Body(raw) => Seen::of_article(&raw),
            Got::Missing | Got::TooBig | Got::Refused(_) => Seen::default(),
        })
    }

    /// The lead's name, if it has none: the volume's own, or the set's best.
    fn lead_writes(&self, ctx: &StepCtx<'_>, lead: i64, files: &[Par2File]) -> Vec<StepWrite> {
        let named: Option<Option<String>> = ctx
            .conn()
            .prepare_cached("select display_name from releases where id = ?")
            .and_then(|mut stmt| stmt.query_row([lead], |r| r.get(0)).optional())
            .unwrap_or(None);
        let Some(None) = named else { return Vec::new() };
        let me = ctx.release();
        let name = me.display_name.clone().or_else(|| best_name(files.iter().map(|f| (f.name.as_str(), f.size))));
        let Some(name) = name else { return Vec::new() };
        let (db, net) = *ON_RENAME;
        let now = chrono::Utc::now().timestamp();
        vec![
            StepWrite::new(NAME_LEAD, vec![lead.into(), name.into(), i64::from(NAME_SOURCE_PAR2).into()]),
            StepWrite::new(RERUN_LEAD, vec![lead.into(), (db | net).into(), net.into()]),
            StepWrite::new(QUEUE_LEAD, vec![lead.into(), now.into()]),
        ]
    }
}

/// Days since a `posted_date`, by SQLite's reading of it.
fn age_days(ctx: &StepCtx<'_>, release: &ReleaseInfo) -> Option<f64> {
    let posted = release.posted_date.as_deref()?;
    ctx.conn().query_row("select julianday('now') - julianday(?)", [posted], |r| r.get(0)).ok().flatten()
}

impl Step for LinkStep {
    fn id(&self) -> StepId {
        LINK_STEP
    }
    fn name(&self) -> &'static str {
        "upload_link"
    }
    fn rank(&self) -> u16 {
        150
    }
    fn needs(&self) -> Needs {
        Needs::Bodies
    }
    fn enabled(&self, cfg: &Config) -> bool {
        *self.limits.lock().unwrap() = cfg.archive.clone();
        cfg.archive.enabled
    }
    /// Only a row its PAR2 set named: the volumes of an upload like this are the ones that get a name.
    fn applies(&self, release: &ReleaseInfo) -> bool {
        release.name_source == Some(i64::from(NAME_SOURCE_PAR2))
    }
    /// A volume gets its name in the same run, from an earlier step, soo the name is what brings this back.
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: true, on_growth: true }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let id = ctx.release().id;
        let lead: Option<i64> = ctx
            .conn()
            .prepare_cached("select lead_id from upload_link where release_id = ?")
            .and_then(|mut stmt| stmt.query_row([id], |r| r.get(0)).optional())
            .map_err(retry)?;
        if lead.is_some() {
            return Ok(Outcome { matched: true, ..Default::default() });
        }
        let files = ctx.files().map_err(retry)?;
        let Some(par2_file) = self.par2_file(ctx, &files)? else { return Ok(Outcome::default()) };
        let inventory = self.inventory(ctx, &par2_file)?;
        let wanted = wanted(&inventory.files);
        if wanted.is_empty() {
            return Ok(Outcome::default());
        }
        let mut candidates = self.candidates(ctx)?;
        candidates.sort_by(|a, b| a.apart.total_cmp(&b.apart));
        let mut checks = 0;
        let mut found: Vec<(i64, &Par2File)> = Vec::new();
        for want in &wanted {
            let fits = candidates.iter().filter(|c| {
                c.file.expected.is_some_and(|expected| parts_fit(want.size, expected, c.bytes, c.parts))
                    && !found.iter().any(|(id, _)| *id == c.id)
            });
            for candidate in fits.cloned().collect::<Vec<_>>() {
                if checks >= MAX_CHECKS {
                    break;
                }
                checks += 1;
                if self.check(ctx, &candidate)?.is(want) {
                    found.push((candidate.id, want));
                    break;
                }
            }
        }
        let Some(&(lead, _)) = found.iter().max_by_key(|(_, f)| f.size) else {
            let young = age_days(ctx, ctx.release()).is_some_and(|days| days < LOOK_AGAIN_FOR_DAYS);
            let wait_until = young.then(|| chrono::Utc::now().timestamp() + LOOK_AGAIN_SECS);
            return Ok(Outcome { wait_until, ..Default::default() });
        };
        let mut writes = vec![
            StepWrite::new(LINK_LEAD, vec![lead.into()]),
            StepWrite::new(LINK_MEMBER, vec![id.into(), lead.into(), 1i64.into()]),
            StepWrite::new(CLEAR_CONTENT, vec![id.into()]),
        ];
        // the rest of the set's files, as rows of their own, go with it
        for (other, _) in found.iter().filter(|(other, _)| *other != lead) {
            writes.push(StepWrite::new(LINK_MEMBER, vec![(*other).into(), lead.into(), 0i64.into()]));
        }
        writes.extend(self.lead_writes(ctx, lead, &inventory.files));
        Ok(Outcome { matched: true, writes, ..Default::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(size: u64, md5: u8) -> Par2File {
        Par2File { set_id: [0; 16], file_id: [md5; 16], md5: [md5; 16], md5_16k: [md5; 16], size, name: "x".into() }
    }

    #[test]
    fn only_big_files_told_apart_are_looked_for() {
        let files = [file(10, 1), file(2 << 20, 2), file(3 << 20, 3), file(3 << 20, 3)];
        let wanted = wanted(&files);
        assert_eq!(wanted.len(), 1);
        assert_eq!(wanted[0].size, 2 << 20);
    }

    #[test]
    fn parts_fit_what_the_articles_weigh() {
        // the 52.6 GB file of a real upload: 68,456 articles of 768,000 bytes, 792,884 bytes each on the wire
        assert!(parts_fit(52_573_940_439, 68_456, 54_465_670_492, 68_693));
        // the same, from only the end of it
        assert!(parts_fit(16_543_239_368, 21_541, 2_240_463_185, 2_826));
        assert!(!parts_fit(52_573_940_439, 2_190, 54_465_670_492, 68_693));
        assert!(!parts_fit(52_573_940_439, 60_000, 54_465_670_492, 68_693));
        assert!(!parts_fit(1, 0, 10, 1));
    }

    fn article(lines: &[&[u8]]) -> Vec<u8> {
        let mut raw = b"Subject: x\nMessage-ID: <a@b>\n\n".to_vec();
        for line in lines {
            raw.extend_from_slice(line);
            raw.push(b'\n');
        }
        raw
    }

    /// yEnc for bytes that need no escaping
    fn encode(data: &[u8]) -> Vec<u8> {
        data.iter().map(|b| b.wrapping_add(42)).collect()
    }

    #[test]
    fn a_first_article_shows_size_and_first_bytes() {
        let data = vec![b'a'; 100];
        let raw = article(&[
            b"=ybegin part=1 total=1 line=128 size=100 name=x",
            b"=ypart begin=1 end=100",
            &encode(&data),
            b"=yend size=100 part=1",
        ]);
        let seen = Seen::of_article(&raw);
        let md5: [u8; 16] = Md5::digest(&data).into();
        assert_eq!(seen, Seen { size: Some(100), md5_16k: Some(md5) });
        let mut want = file(100, 0);
        want.md5_16k = md5;
        assert!(seen.is(&want));
        want.md5_16k = [9; 16];
        assert!(!seen.is(&want), "first bytes that differ");
        want.md5_16k = md5;
        want.size = 101;
        assert!(!seen.is(&want), "a size that differs");
    }

    #[test]
    fn a_later_article_shows_only_the_size_which_is_enough_for_a_big_file() {
        let raw = article(&[
            b"=ybegin part=9 total=99 line=128 size=104857600 name=x",
            b"=ypart begin=8388609 end=9437184",
            b"abc",
            b"=yend size=1048576 part=9",
        ]);
        let seen = Seen::of_article(&raw);
        assert_eq!(seen, Seen { size: Some(104_857_600), md5_16k: None });
        assert!(seen.is(&file(104_857_600, 3)));
        let small = Seen { size: Some(2 << 20), md5_16k: None };
        assert!(!small.is(&file(2 << 20, 3)), "too small to go by size alone");
    }

    #[test]
    fn a_rename_reruns_some_steps() {
        let (db, net) = *ON_RENAME;
        assert_ne!(db | net, 0);
        assert_eq!(db & net, 0);
    }
}
