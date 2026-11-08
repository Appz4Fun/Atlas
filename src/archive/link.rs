//! The files of one upload, linked back together. A fully obfuscated upload posts every file under its own random
//! subject and poster, soo each file is a release row of its own: the recovery volumes get the release's name from their
//! PAR2 set, and the files they repair sit in rows with no name at all. The set lists each file it repairs with its exact
//! size and the MD5 of its first 16 KiB, which a file's first article shows too. A row whose file matches is that file,
//! and it leads the upload: the volume rows link to it, and it takes their name.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use md5::{Digest, Md5};
use rusqlite::{OptionalExtension, params};

use crate::config::{ArchiveConfig, Config};
use crate::par2::{self, Par2File};
use crate::postprocess::bodies::PAST_BUDGET;
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

/// Each row of a linked upload, the lead's own included, and the row it's linked to. A row of the files repaired keeps
/// which of the set's files it is (its file id in the set), and a recovery row how many of them its set has to look for,
/// and its set's id when the set's list of files was sealed by it.
pub const UPLOAD_LINK: ShardTable = ShardTable {
    name: "upload_link",
    create: "create table if not exists upload_link (
        release_id INTEGER PRIMARY KEY,
        lead_id    INTEGER NOT NULL,
        recovery   INTEGER NOT NULL DEFAULT 0,
        par2_file  BLOB,
        wanted     INTEGER,
        set_id     BLOB
    )",
    indexes: &[
        ("upload_link_lead", "create index if not exists upload_link_lead on upload_link(lead_id)"),
        // a volume in another shard finds its upload by its sealed set (`release_info::linked_post_with`)
        (
            "upload_link_set",
            "create index if not exists upload_link_set on upload_link(set_id) where set_id is not null",
        ),
    ],
    release_key: Some("release_id"),
    added: &[],
};

/// The rows a recovery row has fetched an article of and found none of its set's files in, soo a later run goes on
/// from the rest.
pub const UPLOAD_LINK_TRIED: ShardTable = ShardTable {
    name: "upload_link_tried",
    create: "create table if not exists upload_link_tried (
        release_id   INTEGER NOT NULL,
        candidate_id INTEGER NOT NULL,
        PRIMARY KEY (release_id, candidate_id)
    ) without rowid",
    indexes: &[],
    release_key: Some("release_id"),
    added: &[],
};

/// A row of one small file found beside a release by the release's PAR2 set (`LinkStep::small_beside`): a
/// thumbnail posted under a subject of its own. The release's whole post takes it in (`release_info::whole_post_with`)
/// whichever of its rows is asked for, and it isn't an upload's lead, soo nothing else about the post changes.
pub const BESIDE_FILES: ShardTable = ShardTable {
    name: "beside_files",
    create: "create table if not exists beside_files (
        row_id     INTEGER PRIMARY KEY,
        release_id INTEGER NOT NULL,
        par2_file  BLOB
    )",
    indexes: &[("beside_files_release", "create index if not exists beside_files_release on beside_files(release_id)")],
    release_key: Some("row_id"),
    added: &[],
};

/// ?1 the small file's row, ?2 the release it was found beside, ?3 its file id in the set
const SET_BESIDE: &str = "insert into beside_files (row_id, release_id, par2_file) values (?1, ?2, ?3)
    on conflict(row_id) do update set release_id = excluded.release_id, par2_file = excluded.par2_file";

/// The release a small file's row was found beside, if any.
pub fn beside_owner(conn: &rusqlite::Connection, row: i64) -> rusqlite::Result<Option<i64>> {
    let s = crate::store::shard_of_id(row);
    // a shard that hasn't got the table yet has found nothing
    let Ok(mut stmt) = conn.prepare_cached(&format!("select release_id from s{s}.beside_files where row_id = ?1"))
    else {
        return Ok(None);
    };
    stmt.query_row([row], |r| r.get(0)).optional()
}

/// The rows of small files found beside release `id`.
pub fn beside_rows(conn: &rusqlite::Connection, id: i64) -> rusqlite::Result<Vec<i64>> {
    let s = crate::store::shard_of_id(id);
    let Ok(mut stmt) = conn.prepare_cached(&format!("select row_id from s{s}.beside_files where release_id = ?1"))
    else {
        return Ok(Vec::new());
    };
    stmt.query_map([id], |r| r.get(0))?.collect()
}

/// Where a lead's rows are now: another run may have moved them on to a bigger file since this one read them.
macro_rules! lead_now {
    ($lead:literal) => {
        concat!("coalesce((select n.lead_id from upload_link n where n.release_id = ", $lead, "), ", $lead, ")")
    };
}
/// ?1 the old lead, ?2 the new: every row of the old one's upload goes with the new.
pub const MOVE_UPLOAD: &str =
    concat!("update upload_link set lead_id = ", lead_now!("?2"), " where lead_id = ?1 and ?1 != ?2");
pub const LINK_LEAD: &str = "insert into upload_link (release_id, lead_id) values (?1, ?1)
    on conflict(release_id) do nothing";
/// ?1 a row of one of the files repaired, ?2 its lead, ?3 the file's id in the set
pub const LINK_DATA: &str = concat!(
    "insert into upload_link (release_id, lead_id, recovery, par2_file) values (?1, ",
    lead_now!("?2"),
    ", 0, ?3)
    on conflict(release_id) do update set lead_id = excluded.lead_id, recovery = 0, par2_file = excluded.par2_file"
);
/// ?1 a row of recovery data, ?2 its lead, ?3 how many files its set looks for, ?4 its sealed set's id
pub const LINK_RECOVERY: &str = concat!(
    "insert into upload_link (release_id, lead_id, recovery, wanted, set_id) values (?1, ",
    lead_now!("?2"),
    ", 1, ?3, ?4)
    on conflict(release_id) do update set lead_id = excluded.lead_id, recovery = 1, wanted = excluded.wanted,
        set_id = excluded.set_id"
);
const TRIED: &str = "insert into upload_link_tried (release_id, candidate_id) values (?1, ?2) on conflict do nothing";
/// A recovery volume isn't a copy of the files it repairs, whatever its set's list made it look like.
const CLEAR_CONTENT: &str = crate::duplicates::CLEAR_CONTENT;
/// The lead's name, from the set, unless something better named it.
const NAME_LEAD: &str = "update releases set display_name = ?2, name_source = ?3
    where id = ?1 and display_name is null and coalesce(name_source, 0) <= ?3";
/// The lead's steps that go by its name, to run again now it has one.
const RERUN_LEAD: &str = "insert into pp_state (release_id, net_wanted, rerun_gen, rerun_mask, net_left)
        values (?1, case when ?3 != 0 then 1 end, 1, ?2, nullif(?3, 0))
    on conflict(release_id) do update set done = done & ~?2, failed = failed & ~?2,
        net_wanted = case when ?3 != 0 then 1 else net_wanted end,
        net_left = case when ?3 != 0 then coalesce(net_left, 0) | ?3 else net_left end,
        net_next_at = case when ?3 != 0 then 0 else net_next_at end,
        rerun_gen = rerun_gen + 1, rerun_mask = rerun_mask | ?2";
const QUEUE_LEAD: &str = "insert into pp_queue (release_id, due_at) values (?1, ?2)
    on conflict(release_id) do update set due_at = min(due_at, excluded.due_at)";

/// Files smaller than this aren't looked for: an NFO or a sample says too little, and costs a fetch all the same.
const MIN_LINKED: u64 = 1 << 20;
/// Without the first 16 KiB, a file is known by its exact size alone, which only a big one makes safe.
const MIN_SIZE_ONLY: u64 = 64 << 20;
/// How far apart, in ids of this shard, an upload's rows can be: they're read from one run of articles, but other
/// groups' saves come in between, and an upload cut across groups lands in each group's own run.
const ID_WINDOW: i64 = 2_000_000;

/// The ids `saves` saves either way of `id`: ids below `cutover` were handed out eight to a save (eight shards), ids
/// from it on one to a save (`merge_shards::one_shard_from`).
fn id_window(id: i64, cutover: i64, saves: i64) -> (i64, i64) {
    let per = crate::merge_shards::LEGACY_SHARDS as i64;
    let low = if id >= cutover {
        let dense = (id - cutover).min(saves);
        id - dense - (saves - dense) * per
    } else {
        id - saves * per
    };
    let high = if id < cutover {
        let eight = ((cutover - id) / per).min(saves);
        if eight < saves { cutover + (saves - eight) } else { id + saves * per }
    } else {
        id + saves
    };
    (low, high)
}
/// How far apart, in seconds, a small file posted beside a release can say it was posted: an upload's files go up
/// within a minute or two of each other, and this allows a slow one.
const BESIDE_SECS: i64 = 30 * 60;
/// Rows checked, at most, for the small files one release's set names.
const BESIDE_CHECKS: usize = 8;
/// How far apart in days an upload's files can say they were posted. Real ones are an hour or two; a slow or resumed
/// upload a day or two.
const DAY_WINDOW: f64 = 3.0;
/// Rows looked at, at most, and articles fetched to check them, per run.
const MAX_CANDIDATES: usize = 20_000;
const MAX_CHECKS: usize = 64;
/// A volume whose files haven't turned up looks again this often, while it's this new.
const LOOK_AGAIN_SECS: i64 = 3 * 3600;
const LOOK_AGAIN_FOR_DAYS: f64 = 14.0;
/// A volume that ran out of articles to fetch before it ran out of rows to check goes on this soon, until it has
/// checked this many.
const RESUME_SECS: i64 = 300;
const MAX_TRIED: i64 = 2_000;
/// How long after one file's last article the next file of the same upload may start, for the end-to-start join that
/// puts a set's rows ahead of the rest. Tools post files in series, and on real uploads nearly every gap is under two
/// seconds; this allows a connection that stalled.
const JOIN_GAP_SECS: f64 = 60.0;

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

/// The files of a set worth looking for: big enough, each told apart from the rest by its size and first 16 KiB, and
/// each bound to its file id. Two the same would be a guess; one not bound could claim any size under any id.
pub fn wanted(files: &[Par2File]) -> Vec<&Par2File> {
    let mut seen: HashMap<(u64, [u8; 16]), usize> = HashMap::new();
    for f in files {
        *seen.entry((f.size, f.md5_16k)).or_default() += 1;
    }
    files.iter().filter(|f| f.bound && f.size >= MIN_LINKED && seen[&(f.size, f.md5_16k)] == 1).collect()
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

/// When a file's articles were posted, by the best clock its headers had: the posting tool's own milliseconds or the
/// server's, as `file_timing` keeps them, or else the poster's Date header. Seconds since the epoch.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Clock {
    pub start: f64,
    pub end: f64,
}

impl Clock {
    /// `clock_first`/`clock_last` are milliseconds, `first_date`/`last_date` seconds.
    pub fn of(first_date: Option<i64>, last_date: Option<i64>, first: Option<i64>, last: Option<i64>) -> Option<Clock> {
        match (first, last) {
            (Some(first), Some(last)) if first > 0 && last >= first => {
                Some(Clock { start: first as f64 / 1000.0, end: last as f64 / 1000.0 })
            }
            _ => match (first_date, last_date) {
                (Some(first), Some(last)) if first > 0 && last >= first => {
                    Some(Clock { start: first as f64, end: last as f64 })
                }
                _ => None,
            },
        }
    }

    /// How many seconds apart two files were posted, end to start either way: 0 when they overlap.
    pub fn gap(&self, other: &Clock) -> f64 {
        if self.end <= other.start {
            other.start - self.end
        } else if other.end <= self.start {
            self.start - other.end
        } else {
            0.0
        }
    }

    /// Whether `other` is a file of the same run of posting as this one: the one before or after it, within
    /// `JOIN_GAP_SECS`, by `merging`'s end-to-start window.
    pub fn joins(&self, other: &Clock) -> bool {
        use crate::merging::{CandidateFileRun, JoinOptions, ReleaseAssembler};
        let run = |name: &str, clock: &Clock| CandidateFileRun {
            filename: name.to_string(),
            group: String::new(),
            poster: String::new(),
            size: 0,
            yenc_total: None,
            yenc_part_size: None,
            file_index: None,
            rar_volume_idx: None,
            archive_format: None,
            member_id: None,
            byte_offset: 0,
            t_start: clock.start,
            t_end: clock.end,
            file_crc: None,
            md5_16k: None,
            full_md5: None,
            unpacked_crc32: None,
            creation_salt: None,
            is_obfuscated: false,
        };
        let options = JoinOptions { max_gap_seconds: JOIN_GAP_SECS, ..JoinOptions::default() };
        let joined = ReleaseAssembler::join_runs(&[run("a", self), run("b", other)], Some(&options));
        joined.len() == 1
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
    clock: Option<Clock>,
}

/// A file's clock, from its timing row.
fn clock_of(ctx: &StepCtx<'_>, file_id: i64) -> Option<Clock> {
    ctx.conn()
        .prepare_cached("select first_date, last_date, clock_first, clock_last from file_timing where file_id = ?")
        .ok()?
        .query_row([file_id], |r| Ok(Clock::of(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .ok()
        .flatten()
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

    /// The small files a release's own PAR2 set names that aren't among its files: a thumbnail posted beside it under
    /// a subject without the release's name, which no name or poster joins, soo it's a row of its own. The
    /// set gives each file's exact size and the MD5 of its first 16 KiB, and a small file is one article: a row
    /// posted near this one, holding one file of that name, is fetched once and checked against both. One that
    /// matches is kept as found beside the release (`BESIDE_FILES`), and its post takes it in.
    fn small_beside(&self, ctx: &mut StepCtx<'_>, files: &[FileInfo]) -> Result<Outcome, StepError> {
        let me = ctx.release().clone();
        if files.iter().all(|f| is_par2_name(&f.filename)) {
            return Ok(Outcome::default());
        }
        // the PAR2 step reads no set of a release named as a picture: nothing would ever be kept to look by
        if select::par2_source(files).is_none() || select::classify(&me.name) == select::Kind::Image {
            return Ok(Outcome::default());
        }
        // a row of an upload linked by its set is that upload's to look after
        let lead: Option<i64> = query_one(ctx, "select lead_id from upload_link where release_id = ?", me.id)?;
        if lead.is_some() {
            return Ok(Outcome::default());
        }
        // only a set the PAR2 step read and kept: nothing is fetched to find out whether there's anything to look for.
        // In the visit that reads it, its writes aren't in yet: this looks again once they are, while the PAR2 step
        // hasn't finished with the release
        let Some(inventory) = super::store::par2_inventory(ctx.conn(), "main", me.id) else {
            let par2_done: i64 = ctx
                .conn()
                .prepare_cached("select coalesce((select done | failed from pp_state where release_id = ?1), 0) & ?2")
                .and_then(|mut stmt| stmt.query_row(params![me.id, mask(super::steps::PAR2_STEP)], |r| r.get(0)))
                .map_err(retry)?;
            // the PAR2 step finishes or gives up on every release it applies to; one it passes over isn't waited on
            let wait_until = (par2_done == 0).then(|| chrono::Utc::now().timestamp() + RESUME_SECS);
            return Ok(Outcome { wait_until, ..Default::default() });
        };
        let own: HashSet<String> = files.iter().map(|f| f.filename.to_ascii_lowercase()).collect();
        // posted names that aren't the set's (an obfuscated post) say nothing of which small files are missing
        let named = inventory
            .files
            .iter()
            .filter(|f| f.size >= MIN_LINKED && !is_par2_name(&f.name))
            .all(|f| own.contains(&f.name.to_ascii_lowercase()));
        let outside = if named { beside_wanted(&inventory.files, &own) } else { Vec::new() };
        if outside.is_empty() {
            return Ok(Outcome::default());
        }
        let mut have: HashSet<Vec<u8>> = HashSet::new();
        {
            let mut stmt =
                ctx.conn().prepare_cached("select par2_file from beside_files where release_id = ?1").map_err(retry)?;
            let rows = stmt.query_map([me.id], |r| r.get::<_, Option<Vec<u8>>>(0)).map_err(retry)?;
            for row in rows {
                have.extend(row.map_err(retry)?);
            }
        }
        let mut missing: Vec<&Par2File> = outside.iter().copied().filter(|f| !have.contains(&f.file_id[..])).collect();
        let mut found: Vec<(i64, [u8; 16])> = Vec::new();
        let mut tried: Vec<i64> = Vec::new();
        let mut checks = 0;
        let mut out_of_bodies = false;
        // a small file the set names is a picture, often: the fetcher lets this step read one
        ctx.allow_pictures();
        let posted = me.posted_date.clone().unwrap_or_default();
        if let Some((from, to)) = beside_window(&posted, BESIDE_SECS) {
            // as many names looked up as rows may be checked: a set of hundreds of small files costs no more
            'files: for want in missing.clone().into_iter().take(BESIDE_CHECKS) {
                for candidate in Self::beside_candidates(ctx.conn(), me.id, &want.name, (&from, &to, &posted))? {
                    if checks >= BESIDE_CHECKS {
                        break 'files;
                    }
                    checks += 1;
                    let Some(seen) = self.check(ctx, &candidate)? else {
                        out_of_bodies = true;
                        break 'files;
                    };
                    if seen.md5_16k.is_some() && seen.is(want) {
                        found.push((candidate.id, want.file_id));
                        missing.retain(|f| f.file_id != want.file_id);
                        break;
                    }
                    // another upload's file of the same name: not fetched again
                    tried.push(candidate.id);
                }
            }
        }
        let now = chrono::Utc::now().timestamp();
        // only a run cut short by the body budget goes on soon. Nothing waits for a file not saved yet: a wait holds
        // back the release's later steps, and a row of a post cut across groups misses its siblings' small files for
        // good. A release that grows is looked at again
        let wait_until = (out_of_bodies && found.is_empty() && !missing.is_empty()).then_some(now + RESUME_SECS);
        let mut writes: Vec<StepWrite> =
            tried.iter().map(|other| StepWrite::new(TRIED, vec![me.id.into(), (*other).into()])).collect();
        if found.is_empty() {
            let matched = !have.is_empty();
            return Ok(Outcome { matched, writes, wait_until, ..Default::default() });
        }
        for (row, file_id) in &found {
            writes.push(StepWrite::new(SET_BESIDE, vec![(*row).into(), me.id.into(), file_id.to_vec().into()]));
        }
        // the pictures step reads what's linked in: run again, now there's more
        let rerun = mask(crate::images::step::IMAGES_STEP);
        Ok(Outcome { matched: true, writes, rerun, wait_until, ..Default::default() })
    }

    /// Rows posted within `window` (from, to, and the release's own time) holding one file named `name` and nothing
    /// else, in at most two articles, that no other upload has taken. Nearest in time first: an uploader that names
    /// every thumbnail the same posts dozens an hour. Such a row is named after its file (with ` [N]` after it when the
    /// name is taken), soo the name's index finds them (the date is kept off its own index, which would read the whole
    /// hour), not a scan of every file. The name is matched as the set spells it.
    fn beside_candidates(
        conn: &rusqlite::Connection,
        me: i64,
        name: &str,
        window: (&str, &str, &str),
    ) -> Result<Vec<Candidate>, StepError> {
        let mut stmt = conn
            .prepare_cached(
                "select r.id, coalesce(r.size, 0), coalesce(r.parts, 0), f.id, f.filename, f.expected
                 from releases r join files f on f.release_id = r.id
                 where r.name >= ?4 and r.name < ?4 || char(127)
                   and +r.posted_date between ?1 and ?2 and r.id != ?3 and f.filename = ?4 collate nocase
                   and coalesce(f.expected, 1) <= 2
                   and not exists (select 1 from files g where g.release_id = r.id and g.id != f.id)
                   and not exists (select 1 from upload_link l where l.release_id = r.id)
                   and not exists (select 1 from beside_files b where b.row_id = r.id and b.release_id != ?3)
                   and not exists (select 1 from upload_link_tried t where t.release_id = ?3 and t.candidate_id = r.id)
                 order by abs(julianday(r.posted_date) - julianday(?6)), r.id limit ?5",
            )
            .map_err(retry)?;
        let rows = stmt
            .query_map(params![window.0, window.1, me, name, BESIDE_CHECKS as i64, window.2], |r| {
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
                    apart: 0.0,
                    clock: None,
                })
            })
            .map_err(retry)?;
        rows.collect::<rusqlite::Result<_>>().map_err(retry)
    }

    /// The set's list of files: as the PAR2 step kept it, when it read the whole set, or else from the volume's first
    /// article and, when that doesn't hold all of it or can't be read, its last: a set repeats its descriptions there.
    /// With it, the writes that keep what was read, soo the next run reads nothing.
    fn inventory(
        &self,
        ctx: &mut StepCtx<'_>,
        file: &FileInfo,
    ) -> Result<(par2::Inventory, Vec<StepWrite>), StepError> {
        let id = ctx.release().id;
        if let Some(kept) = super::store::par2_inventory(ctx.conn(), "main", id) {
            return Ok((kept, Vec::new()));
        }
        let mut body = Vec::new();
        let first = first_article(ctx, file)?;
        if let Some(seg) = &first
            && let Some(got) = fetch_one(ctx, seg, &file.filename)?
        {
            body = got;
        }
        let mut inventory = par2::inventory(&body);
        if !inventory.complete {
            let last = ctx.segments(file.id, &SegmentPick::Last(1)).map_err(retry)?.into_iter().last();
            if let Some(last) = last.filter(|s| first.as_ref().is_none_or(|f| s.message_id != f.message_id))
                && let Some(more) = fetch_one(ctx, &last, &file.filename)?
            {
                body.extend(more);
                inventory = par2::inventory(&body);
            }
        }
        let kept = if inventory.files.is_empty() { Vec::new() } else { super::store::par2_file_writes(id, &inventory) };
        Ok((inventory, kept))
    }

    /// Single-file rows near this one, in any group of this shard, that could hold a file of between `sizes` bytes,
    /// nearest in time first. Only rows that nothing has named, or that lead an upload already: the row's file is
    /// checked against the set's size and first 16 KiB before anything links, which is what makes a file that file,
    /// whoever posted it and under whatever subject. What the article count must be for those sizes is weighed here
    /// too, roughly, soo a busy shard's other rows don't crowd the real one out of the limit; `parts_fit` then weighs
    /// each closely.
    fn candidates(
        &self,
        ctx: &StepCtx<'_>,
        sizes: (u64, u64),
        set: Option<[u8; 16]>,
    ) -> Result<Vec<Candidate>, StepError> {
        let me = ctx.release();
        let Some(posted) = me.posted_date.clone() else { return Ok(Vec::new()) };
        // `ID_WINDOW` saves either way of this one, whether their ids are from the eight shards there were (eight to
        // a save) or dense since
        let cutover =
            crate::merge_shards::one_shard_from(ctx.conn()).map_err(|e| StepError::Retry(format!("{e:#}")))?;
        let (low, high) = id_window(me.id, cutover, ID_WINDOW);
        let mut stmt = ctx
            .conn()
            .prepare_cached(
                "select r.id, coalesce(r.size, 0), coalesce(r.parts, 0), f.id, f.filename, f.expected,
                        abs(julianday(r.posted_date) - julianday(?4)),
                        t.first_date, t.last_date, t.clock_first, t.clock_last
                 from releases r join files f on f.release_id = r.id
                 left join file_timing t on t.file_id = f.id
                 where r.id between ?1 and ?2 and r.id != ?3
                   and abs(julianday(r.posted_date) - julianday(?4)) <= ?5
                   and not exists (select 1 from files g where g.release_id = r.id and g.id != f.id)
                   and not exists (select 1 from upload_link l where l.release_id = r.id and l.lead_id != r.id)
                   and not exists (select 1 from upload_link_tried t where t.release_id = ?3 and t.candidate_id = r.id)
                   and r.size > 0 and r.parts > 0
                   and (r.display_name is null
                        or exists (select 1 from upload_link m where m.lead_id = r.id and m.recovery = 1
                                   and m.set_id = ?9))
                   and f.expected between ?7 * 1.005 * r.parts / r.size - 2 and ?8 * 1.08 * r.parts / r.size + 2
                 order by abs(julianday(r.posted_date) - julianday(?4))
                 limit ?6",
            )
            .map_err(retry)?;
        let rows = stmt
            .query_map(
                params![
                    low,
                    high,
                    me.id,
                    posted,
                    DAY_WINDOW,
                    MAX_CANDIDATES as i64,
                    sizes.0 as f64,
                    sizes.1 as f64,
                    set.map(|id| id.to_vec())
                ],
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
                        clock: Clock::of(r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?),
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

    /// What a candidate's article says, or None when this run may fetch no more.
    fn check(&self, ctx: &mut StepCtx<'_>, candidate: &Candidate) -> Result<Option<Seen>, StepError> {
        let Some(seg) = self.article_of(ctx, &candidate.file)? else { return Ok(Some(Seen::default())) };
        let wanted = BodyRequest {
            message_id: seg.message_id.clone(),
            file: candidate.file.filename.clone(),
            max_bytes: Some(ARTICLE_CAP),
        };
        let got = match ctx.fetch_article(&wanted)?.got {
            // the first is gone from the server: its last says the size, which is enough for a big file
            Got::Missing => match self.last_article(ctx, &candidate.file, &seg.message_id)? {
                Some(last) => ctx.fetch_article(&BodyRequest { message_id: last.message_id, ..wanted })?.got,
                None => Got::Missing,
            },
            got => got,
        };
        Ok(match got {
            Got::Body(raw) => Some(Seen::of_article(&raw)),
            Got::Refused(why) if why == PAST_BUDGET => None,
            Got::Missing | Got::TooBig | Got::Refused(_) => Some(Seen::default()),
        })
    }

    /// A file's last article, unless that's the one already tried.
    fn last_article(&self, ctx: &StepCtx<'_>, file: &FileInfo, tried: &str) -> Result<Option<SegmentRef>, StepError> {
        let last = ctx.segments(file.id, &SegmentPick::Last(1)).map_err(retry)?.into_iter().last();
        Ok(last.filter(|s| s.message_id != tried))
    }

    /// How many rows this volume has checked and found none of its files in, over its earlier runs.
    fn tried(&self, ctx: &StepCtx<'_>, id: i64) -> Result<i64, StepError> {
        Ok(query_one(ctx, "select count(*) from upload_link_tried where release_id = ?", id)?.unwrap_or(0))
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
    /// Any row: `run` goes on only for a row of PAR2 files, whether or not its set gave it a name. A set whose names
    /// looked random named nothing, and its volumes link all the same.
    fn applies(&self, release: &ReleaseInfo) -> bool {
        let _ = release;
        true
    }
    /// A volume gets its name in the same run, from an earlier step, soo the name is what brings this back.
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: true, on_growth: true }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let id = ctx.release().id;
        let files = ctx.files().map_err(retry)?;
        let Some(par2_file) = self.par2_file(ctx, &files)? else { return self.small_beside(ctx, &files) };
        // the upload this row is in already, and which of its set's files that has
        let mine: Option<i64> = query_one(ctx, "select lead_id from upload_link where release_id = ?", id)?;
        let mut members = HashSet::new();
        let mut have: HashSet<Vec<u8>> = HashSet::new();
        if let Some(lead) = mine {
            let mut stmt = ctx
                .conn()
                .prepare_cached("select release_id, par2_file from upload_link where lead_id = ?")
                .map_err(retry)?;
            let rows = stmt
                .query_map([lead], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<Vec<u8>>>(1)?)))
                .map_err(retry)?;
            for row in rows {
                let (member, file) = row.map_err(retry)?;
                members.insert(member);
                have.extend(file);
            }
        }
        let (inventory, kept) = self.inventory(ctx, &par2_file)?;
        let young = age_days(ctx, ctx.release()).is_some_and(|days| days < LOOK_AGAIN_FOR_DAYS);
        let now = chrono::Utc::now().timestamp();
        let wanted = wanted(&inventory.files);
        if wanted.is_empty() {
            // a set that couldn't be read at all may be readable later
            let wait_until = (inventory.files.is_empty() && young).then_some(now + LOOK_AGAIN_SECS);
            return Ok(Outcome { matched: mine.is_some(), wait_until, writes: kept, ..Default::default() });
        }
        // a set read only in part may have more files than it shows: the link can't call itself whole, and it looks
        // again for the rest of the list
        let unknown = i64::from(!inventory.complete);
        let set = sealed_set(&inventory, &wanted);
        let mut missing: Vec<&Par2File> = wanted.iter().copied().filter(|f| !have.contains(&f.file_id[..])).collect();
        let (mut found, mut tried, mut more) = (Vec::new(), Vec::new(), false);
        if !missing.is_empty() {
            let sizes = missing.iter().fold((u64::MAX, 0), |(least, most), f| (least.min(f.size), most.max(f.size)));
            let mut candidates = self.candidates(ctx, sizes, set)?;
            candidates.retain(|c| {
                !members.contains(&c.id)
                    && c.file.expected.is_some_and(|expected| {
                        missing.iter().any(|want| parts_fit(want.size, expected, c.bytes, c.parts))
                    })
            });
            // rows whose first article is indexed go first: those can tell apart files of the same size, where a later
            // article only gives the size. Among those, the files posted right before or after this volume, by the
            // best clock there is: an upload's files go up one after the other
            let my_clock = clock_of(ctx, par2_file.id);
            let mut ordered = Vec::with_capacity(candidates.len());
            for candidate in candidates {
                let later = first_article(ctx, &candidate.file)?.is_none();
                let (joined, gap) = match (my_clock, candidate.clock) {
                    (Some(mine), Some(theirs)) => (mine.joins(&theirs), mine.gap(&theirs)),
                    _ => (false, candidate.apart * 86_400.0),
                };
                ordered.push((later, !joined, gap, candidate));
            }
            ordered.sort_by(|(a_later, a_apart, a_gap, _), (b_later, b_apart, b_gap, _)| {
                a_later.cmp(b_later).then(a_apart.cmp(b_apart)).then(a_gap.total_cmp(b_gap))
            });
            let mut checks = 0;
            for (_, _, _, candidate) in ordered {
                if missing.is_empty() {
                    break;
                }
                let fits = candidate.file.expected.is_some_and(|expected| {
                    missing.iter().any(|want| parts_fit(want.size, expected, candidate.bytes, candidate.parts))
                });
                if !fits {
                    continue;
                }
                if checks >= MAX_CHECKS {
                    more = true;
                    break;
                }
                checks += 1;
                let seen = match self.check(ctx, &candidate) {
                    Ok(Some(seen)) => seen,
                    // out of bodies for now: what was learned so far is kept, and the rest waits for a later run
                    Ok(None) => {
                        more = true;
                        break;
                    }
                    Err(StepError::OutOfBudget) if !found.is_empty() || !tried.is_empty() => {
                        more = true;
                        break;
                    }
                    Err(e) => return Err(e),
                };
                let fits: Vec<usize> = (0..missing.len()).filter(|&at| seen.is(missing[at])).collect();
                match fits[..] {
                    [at] => found.push((candidate, missing.remove(at))),
                    [] => tried.push(candidate.id),
                    // by its size alone it could be any of them: it's left for a run that can read its first bytes
                    _ => {}
                }
            }
        }
        let wait_until = if more && !missing.is_empty() && self.tried(ctx, id)? + (tried.len() as i64) < MAX_TRIED {
            Some(now + RESUME_SECS)
        } else if (!missing.is_empty() || unknown != 0) && young {
            Some(now + LOOK_AGAIN_SECS)
        } else {
            None
        };
        // a row that isn't one of the files read so far may be one of those not read yet
        let tried = if unknown == 0 { tried } else { Vec::new() };
        let mut writes: Vec<StepWrite> = kept;
        writes.extend(tried.iter().map(|other| StepWrite::new(TRIED, vec![id.into(), (*other).into()])));
        // every file of the set, the ones too small or too alike to look for too: the upload isn't whole without them
        let looks_for = inventory.files.len() as i64 + unknown;
        if found.is_empty() {
            // what it looks for may be known better now
            if let Some(lead) = mine {
                writes.push(StepWrite::new(
                    LINK_RECOVERY,
                    vec![id.into(), lead.into(), looks_for.into(), set.map(|id| id.to_vec()).into()],
                ));
            }
            return Ok(Outcome { matched: mine.is_some(), writes, wait_until, ..Default::default() });
        }
        // the uploads this joins: its own, and any a file it found leads. The biggest file leads them all
        let mut uploads: Vec<(i64, i64)> = Vec::new();
        if let Some(lead) = mine {
            uploads.push((
                lead,
                query_one(ctx, "select coalesce(size, 0) from releases where id = ?", lead)?.unwrap_or(0),
            ));
        }
        for (candidate, _) in &found {
            let leads: Option<i64> =
                query_one(ctx, "select 1 from upload_link where release_id = ?1 and lead_id = ?1", candidate.id)?;
            if leads.is_some() {
                uploads.push((candidate.id, candidate.bytes));
            }
        }
        let lead = found
            .iter()
            .map(|(c, _)| (c.id, c.bytes))
            .chain(uploads.iter().copied())
            .max_by_key(|&(_, bytes)| bytes)
            .map_or(id, |(lead, _)| lead);
        for (old, _) in uploads.iter().filter(|(old, _)| *old != lead) {
            writes.push(StepWrite::new(MOVE_UPLOAD, vec![(*old).into(), lead.into()]));
        }
        for (candidate, file) in &found {
            writes
                .push(StepWrite::new(LINK_DATA, vec![candidate.id.into(), lead.into(), file.file_id.to_vec().into()]));
        }
        writes.push(StepWrite::new(
            LINK_RECOVERY,
            vec![id.into(), lead.into(), looks_for.into(), set.map(|id| id.to_vec()).into()],
        ));
        writes.push(StepWrite::new(CLEAR_CONTENT, vec![id.into()]));
        writes.extend(self.lead_writes(ctx, lead, &inventory.files));
        Ok(Outcome { matched: true, writes, wait_until, ..Default::default() })
    }
}

/// The files of a release's set to look for beside it: small ones (an upload's big files are its own rows' to find),
/// bound to their ids, told apart from the rest, not PAR2, and not among the release's own files (lowercased names).
fn beside_wanted<'a>(files: &'a [Par2File], own: &HashSet<String>) -> Vec<&'a Par2File> {
    let mut seen: HashMap<(u64, [u8; 16]), usize> = HashMap::new();
    for f in files {
        *seen.entry((f.size, f.md5_16k)).or_default() += 1;
    }
    files
        .iter()
        .filter(|f| f.bound && f.size > 0 && f.size < MIN_LINKED && seen[&(f.size, f.md5_16k)] == 1)
        .filter(|f| !is_par2_name(&f.name) && !own.contains(&f.name.to_ascii_lowercase()))
        .collect()
}

/// `posted` (as releases keep it, `YYYY-MM-DD HH:MM:SS`) less and plus `secs`, in the same form.
fn beside_window(posted: &str, secs: i64) -> Option<(String, String)> {
    let at = chrono::NaiveDateTime::parse_from_str(posted.get(..19)?, "%Y-%m-%d %H:%M:%S").ok()?;
    let shift = chrono::Duration::seconds(secs);
    let form = |t: chrono::NaiveDateTime| t.format("%Y-%m-%d %H:%M:%S").to_string();
    Some((form(at - shift), form(at + shift)))
}

/// The set the files looked for all belong to, when its own id seals its list of them. Only a volume of that set can
/// join an upload already linked: anyone can post a set that lists a file of someone else's upload, but not one with
/// that upload's set id and a file of their own.
fn sealed_set(inventory: &par2::Inventory, wanted: &[&Par2File]) -> Option<[u8; 16]> {
    let set = wanted.first()?.set_id;
    wanted.iter().all(|f| f.set_id == set && inventory.sealed.contains(&(set, f.file_id))).then_some(set)
}

/// The one value a query of one id gives, if it gives a row.
fn query_one<T: rusqlite::types::FromSql>(ctx: &StepCtx<'_>, sql: &str, id: i64) -> Result<Option<T>, StepError> {
    ctx.conn().prepare_cached(sql).and_then(|mut stmt| stmt.query_row([id], |r| r.get(0)).optional()).map_err(retry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_the_same_saves_whether_ids_were_eight_to_a_save_or_one() {
        let w = 100;
        // all from before one shard: eight ids a save
        assert_eq!(id_window(10_000, 1_000_000, w), (10_000 - 800, 10_000 + 800));
        // all since: one id a save
        assert_eq!(id_window(2_000_000, 1_000_000, w), (2_000_000 - 100, 2_000_100));
        // across: 40 dense saves down to the cutover, the other 60 at eight
        assert_eq!(id_window(1_000_040, 1_000_000, w).0, 1_000_000 - 60 * 8);
        // and up: 30 saves at eight to the cutover, the other 70 dense
        assert_eq!(id_window(1_000_000 - 240, 1_000_000, w).1, 1_000_000 + 70);
        // never merged: every id dense
        assert_eq!(id_window(500, 0, w), (400, 600));
    }

    fn file(size: u64, md5: u8) -> Par2File {
        Par2File {
            set_id: [0; 16],
            file_id: [md5; 16],
            md5: [md5; 16],
            md5_16k: [md5; 16],
            size,
            name: "x".into(),
            bound: true,
            crc32: None,
        }
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

    fn links(conn: &rusqlite::Connection) -> Vec<(i64, i64)> {
        let mut stmt = conn.prepare("select release_id, lead_id from upload_link order by release_id").unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    }

    fn write(conn: &rusqlite::Connection, sql: &str, params: Vec<rusqlite::types::Value>) {
        conn.execute(sql, rusqlite::params_from_iter(params)).unwrap();
    }

    #[test]
    fn a_bigger_file_found_later_takes_the_whole_upload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.s0.db");
        crate::store::create_shard(&path).unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        let (sample, mkv, vol_a, vol_b) = (8i64, 16i64, 24i64, 32i64);
        // one volume finds only the sample, and it leads
        write(&conn, LINK_DATA, vec![sample.into(), sample.into(), vec![1u8; 16].into()]);
        write(&conn, LINK_RECOVERY, vec![vol_a.into(), sample.into(), 2i64.into(), rusqlite::types::Value::Null]);
        // another finds both: the bigger file leads, and the sample's upload goes with it
        write(&conn, MOVE_UPLOAD, vec![sample.into(), mkv.into()]);
        write(&conn, LINK_DATA, vec![mkv.into(), mkv.into(), vec![2u8; 16].into()]);
        write(&conn, LINK_RECOVERY, vec![vol_b.into(), mkv.into(), 2i64.into(), rusqlite::types::Value::Null]);
        assert_eq!(links(&conn), [(sample, mkv), (mkv, mkv), (vol_a, mkv), (vol_b, mkv)]);
        // a run that still thinks the sample leads links to where its upload went
        write(&conn, LINK_DATA, vec![sample.into(), sample.into(), vec![1u8; 16].into()]);
        write(&conn, LINK_RECOVERY, vec![vol_a.into(), sample.into(), 2i64.into(), rusqlite::types::Value::Null]);
        assert_eq!(links(&conn), [(sample, mkv), (mkv, mkv), (vol_a, mkv), (vol_b, mkv)]);
    }

    #[test]
    fn small_files_a_releases_set_names_are_looked_for_beside_it() {
        let mut thumb = file(148_562, 7);
        thumb.name = "00-thumbnail.jpg".into();
        let mut video = file(3_741_842_775, 8);
        video.name = "Some.Film.2160p.mp4".into();
        let mut nfo = file(2_000, 9);
        nfo.name = "Some.Film.2160p.nfo".into();
        let mut index = file(3_000, 10);
        index.name = "Some.Film.2160p.par2".into();
        let set = [thumb.clone(), video.clone(), nfo.clone(), index];
        let own: HashSet<String> =
            ["some.film.2160p.mp4", "some.film.2160p.nfo"].iter().map(|n| n.to_string()).collect();
        let names: Vec<&str> = beside_wanted(&set, &own).iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["00-thumbnail.jpg"], "the big file is its own row's to find; the NFO is here");
        let mut twin = thumb.clone();
        twin.name = "01-thumbnail.jpg".into();
        assert!(beside_wanted(&[thumb, twin], &HashSet::new()).is_empty(), "two alike are a guess");
        assert_eq!(
            beside_window("2026-10-10 00:01:39", 1_800),
            Some(("2026-10-09 23:31:39".into(), "2026-10-10 00:31:39".into()))
        );
        assert_eq!(beside_window("not a date", 1_800), None);
    }

    #[test]
    fn a_small_file_beside_a_release_is_a_row_of_one_file_near_it_that_no_other_upload_took() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.s0.db");
        crate::store::create_shard(&path).unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        let row = |id: i64, name: &str, posted: &str, files: &[&str]| {
            conn.execute(
                "insert into releases (id, name, group_name, posted_date, size, parts) values (?1, ?2, 'g', ?3, 1, 1)",
                params![id, name, posted],
            )
            .unwrap();
            for (n, f) in files.iter().enumerate() {
                conn.execute(
                    "insert into files (id, release_id, filename, expected, seen) values (?1, ?2, ?3, 1, x'01')",
                    params![id * 10 + n as i64, id, f],
                )
                .unwrap();
            }
        };
        row(1, "Some.Film.2160p", "2026-10-10 00:01:39", &["Some.Film.2160p.mp4", "Some.Film.2160p.par2"]);
        row(2, "00-thumbnail.jpg", "2026-10-10 00:01:40", &["00-thumbnail.jpg"]);
        row(3, "00-thumbnail.jpg [1]", "2026-10-10 02:00:00", &["00-thumbnail.jpg"]);
        row(4, "00-thumbnail.jpg [2]", "2026-10-10 00:02:05", &["00-thumbnail.jpg"]);
        row(5, "pics", "2026-10-10 00:01:41", &["00-thumbnail.jpg", "01.jpg"]);
        // another upload's already
        row(6, "00-thumbnail.jpg [3]", "2026-10-10 00:03:00", &["00-THUMBNAIL.JPG"]);
        write(&conn, LINK_DATA, vec![6i64.into(), 9i64.into(), vec![1u8; 16].into()]);
        let window = beside_window("2026-10-10 00:01:39", BESIDE_SECS).unwrap();
        let found =
            LinkStep::beside_candidates(&conn, 1, "00-thumbnail.jpg", (&window.0, &window.1, "2026-10-10 00:01:39"))
                .unwrap();
        let ids: Vec<i64> = found.iter().map(|c| c.id).collect();
        assert_eq!(ids, [2, 4], "nearest in time first, one file, not taken; the check against the set decides");
        // found beside this release, it's no other's to take, and stays this one's on the next run
        write(&conn, SET_BESIDE, vec![2i64.into(), 1i64.into(), vec![7u8; 16].into()]);
        let owner: i64 =
            conn.query_row("select release_id from beside_files where row_id = 2", [], |r| r.get(0)).unwrap();
        assert_eq!(owner, 1);
        let posted = "2026-10-10 00:01:39";
        let again = LinkStep::beside_candidates(&conn, 1, "00-thumbnail.jpg", (&window.0, &window.1, posted)).unwrap();
        assert_eq!(again.iter().map(|c| c.id).collect::<Vec<_>>(), [2, 4]);
        let other = LinkStep::beside_candidates(&conn, 4, "00-thumbnail.jpg", (&window.0, &window.1, posted)).unwrap();
        // one checked and found to be another upload's isn't fetched again
        write(&conn, TRIED, vec![1i64.into(), 4i64.into()]);
        let left = LinkStep::beside_candidates(&conn, 1, "00-thumbnail.jpg", (&window.0, &window.1, posted)).unwrap();
        assert_eq!(left.iter().map(|c| c.id).collect::<Vec<_>>(), [2]);
        assert!(!other.iter().any(|c| c.id == 2), "another release can't take it");
    }

    #[test]
    fn files_posted_one_after_another_join_by_the_clock() {
        let a = Clock { start: 1_000.0, end: 1_600.0 };
        let next = Clock { start: 1_601.5, end: 2_200.0 };
        let later = Clock { start: 1_600.0 + JOIN_GAP_SECS + 5.0, end: 3_000.0 };
        assert!(a.joins(&next));
        assert!(next.joins(&a), "either way round");
        assert!(!a.joins(&later));
        assert_eq!(a.gap(&next), 1.5);
        assert_eq!(a.gap(&Clock { start: 1_200.0, end: 1_300.0 }), 0.0, "overlapping");
        assert_eq!(Clock::of(Some(10), Some(20), Some(5_000), Some(9_000)), Some(Clock { start: 5.0, end: 9.0 }));
        assert_eq!(Clock::of(Some(10), Some(20), None, None), Some(Clock { start: 10.0, end: 20.0 }));
        assert_eq!(Clock::of(None, Some(20), None, None), None);
    }

    #[test]
    fn a_set_read_once_is_kept_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.s0.db");
        crate::store::create_shard(&path).unwrap();
        let conn = crate::db::open_shard(&path).unwrap();
        let mut a = file(2 << 20, 1);
        a.name = "a.mkv".into();
        a.crc32 = Some(7);
        let mut b = file(3 << 20, 2);
        b.name = "b.mkv".into();
        // a descriptor whose id doesn't hash its size and name reads back as it was parsed: not bound
        let mut c = file(4 << 20, 3);
        c.name = "c.mkv".into();
        c.bound = false;
        let inventory = par2::Inventory {
            files: vec![a.clone(), b.clone(), c.clone()],
            block_size: Some(1024),
            complete: true,
            truncated: false,
            sealed: vec![(a.set_id, a.file_id)],
            creators: vec![(a.set_id, "ParPar v0.4.1 x64 [https://github.com/animetosho/parpar]".into())],
        };
        for w in super::super::store::par2_file_writes(8, &inventory) {
            write(&conn, w.sql, w.params);
        }
        let back = super::super::store::par2_inventory(&conn, "main", 8).expect("read back");
        assert_eq!(back.files, vec![a.clone(), b, c]);
        assert_eq!(back.sealed, vec![(a.set_id, a.file_id)]);
        assert_eq!(back.creators, inventory.creators, "the set's Creator string reads back with it");
        assert!(back.complete);
        assert!(!back.files[2].bound, "what failed validation when parsed still fails it read back");
        assert_eq!(wanted(&back.files).len(), 2, "c names nothing");
        assert_eq!(sealed_set(&back, &wanted(&back.files)), None, "b was not sealed");
        // a set read only in part isn't read back: the next run reads the rest
        let partial = par2::Inventory { complete: false, ..inventory };
        for w in super::super::store::par2_file_writes(16, &partial) {
            write(&conn, w.sql, w.params);
        }
        assert!(super::super::store::par2_inventory(&conn, "main", 16).is_none());
    }

    #[test]
    fn a_rename_reruns_some_steps() {
        let (db, net) = *ON_RENAME;
        assert_ne!(db | net, 0);
        assert_eq!(db & net, 0);
    }
}
