//! The cross-group linker over the day-segment store: uploads obfuscated per article tied back together from the
//! store's blocks, by the rules in `xref`. It runs whenever the store is on (`[header_store] enabled`).
//!
//! The unit of work is an hour of the posters' wall clock (the scale posting times are kept on). An hour is
//! ready once every backfill day chunk covering it and two hours either side is done and two hours (and the settle
//! time) have gone by since it ended, or a day after its rows first came. A pass takes the earliest ready hours, a
//! few at a time (`SPAN_HOURS`), seals and commits whatever the store still holds of their days (a barrier), reads
//! every block around them once with the engine's field cursor, keeps the obfuscated rows (`xref::keeps_fields`),
//! drops repeats by message-id, groups them into files by poster and chains the files into uploads (`xref::chain`),
//! and builds each upload's release the way `xref` does (`xref::add_file`): same names, subjects, file names, part
//! numbering and collection.
//!
//! Nothing is marked done row by row. A linked hour never comes back; what arrives later for it comes in blocks
//! committed after the pass (each segment's block count is kept), and joins its upload: a file already placed takes
//! its new rows as more parts, a new file joins an upload saved within the window (a seed).
//!
//! State, in the store's `_state` folder:
//! - `linker.rec`: linked hours, when each day's rows were first seen, each segment's block count. Rewritten whole,
//!   atomically, after every pass whose releases were saved.
//! - `links.rec`: where each file went (its place), each upload saved (its seed), and the archive step's progress
//!   through an upload's first-piece guesses. Appended to, a week's life, compacted at open.
//!
//! A pass's releases are saved before its state is written: a crash between links the same hours again, to the same
//! names and parts, which saving again leaves as they were.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nntp_xover_engine::container::NxvBlockView;
use nntp_xover_engine::streamer::FieldCursor;

use super::store::{DAY, Store};
use super::sync::{full_flush, sync_all_durable};
use crate::parser::Release;
use crate::xref::{FileSum, KEEP_SECS, MIN_UPLOAD_ARTICLES, Place, Row, SETTLE_SECS, Seed};

const HOUR: i64 = 3600;
/// ready hours one pass links at most: the rows of these and two hours either side are held at once
pub const SPAN_HOURS: i64 = 3;
/// the context read either side of the hours linked, as `xref` keeps a file's articles for an upload
const MARGIN: i64 = 2 * HOUR;
/// block ranges are UTC, posting times the poster's wall clock: within this of each other in any zone
const ZONE: i64 = 14 * HOUR;
/// how long after an hour ends it's taken as read when live
const LIVE_WAIT: i64 = 2 * HOUR + SETTLE_SECS;
/// How long after an upload's last article a later file by the same poster still joins it. Uploads finish within about
/// two hours; past this a poster posting again under the same name starts a new upload (its release named apart,
/// `second_upload_key`). Stragglers posted much later join by file name and size, not by poster.
pub const JOIN_GAP: i64 = 3 * HOUR;
/// how long a file's place and an upload's seed are kept: an upload outlasts the window by a week, the archive step
/// may still be taking its rounds (as `xref` keeps `linked` and `uploads`)
const LINKS_LIFE: i64 = 7 * KEEP_SECS + KEEP_SECS;
const STATE_FILE: &str = "linker.rec";
const LINKS_FILE: &str = "links.rec";

/// A stable id for a group, the same in every pass and run (the store has no group table).
pub fn group_id(name: &str) -> i64 {
    let h = name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3));
    (h & (i64::MAX as u64)) as i64
}

fn hour_of(t: i64) -> i64 {
    t.div_euclid(HOUR)
}

// ---------------------------------------------------------------- linker.rec

/// What the linker has done: rewritten whole after each pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// wall-clock hours linked
    pub linked: BTreeSet<i64>,
    /// UTC day -> when its rows were first seen (unix seconds)
    pub seen: BTreeMap<i64, i64>,
    /// (group, UTC day) -> blocks of its segment the linker has read
    pub counts: BTreeMap<(String, i64), u32>,
    /// the earliest posting time of an upload that waited on an hour not linked yet: the next pass reads back to it,
    /// soo an upload longer than the margin keeps its first files though their hours were linked meanwhile
    pub waiting_from: Option<i64>,
}

impl State {
    pub fn to_text(&self) -> String {
        let mut out = String::from("# atlas header store linker\n");
        let mut it = self.linked.iter().copied().peekable();
        while let Some(from) = it.next() {
            let mut to = from;
            while it.peek() == Some(&(to + 1)) {
                to = it.next().expect("peeked");
            }
            out.push_str(&format!("L\t{from}\t{to}\n"));
        }
        for (day, at) in &self.seen {
            out.push_str(&format!("S\t{day}\t{at}\n"));
        }
        for ((group, day), n) in &self.counts {
            out.push_str(&format!("C\t{day}\t{n}\t{group}\n"));
        }
        if let Some(w) = self.waiting_from {
            out.push_str(&format!("W\t{w}\n"));
        }
        out.push_str("E\n");
        out
    }

    /// The state a file holds; None when it's cut short (no end line), which a rename never leaves.
    pub fn from_text(text: &str) -> Option<State> {
        let mut s = State::default();
        let mut ended = false;
        for line in text.lines() {
            let f: Vec<&str> = line.splitn(4, '\t').collect();
            match f.as_slice() {
                ["L", a, b] => s.linked.extend(a.parse::<i64>().ok()?..=b.parse::<i64>().ok()?),
                ["S", d, t] => {
                    s.seen.insert(d.parse().ok()?, t.parse().ok()?);
                }
                ["C", d, n, g] => {
                    s.counts.insert(((*g).to_string(), d.parse().ok()?), n.parse().ok()?);
                }
                ["W", w] => s.waiting_from = Some(w.parse().ok()?),
                ["E"] => ended = true,
                _ => {}
            }
        }
        ended.then_some(s)
    }

    /// Read from `dir`; empty when there's none yet.
    pub fn load(dir: &Path) -> Result<State> {
        let path = dir.join(STATE_FILE);
        match std::fs::read_to_string(&path) {
            Ok(t) => State::from_text(&t).with_context(|| format!("{} is cut short", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Written to `dir` atomically: a temp file, synced, renamed over, the folder synced.
    pub fn save(&self, dir: &Path) -> Result<()> {
        write_atomic(&dir.join(STATE_FILE), self.to_text().as_bytes())
    }

    /// Hours, seen days and counts of days the store no longer has dropped, soo the file stays small.
    fn forget_before(&mut self, oldest_day: i64) {
        let first_hour = (oldest_day - 2) * 24;
        self.linked = self.linked.split_off(&first_hour);
        self.seen = self.seen.split_off(&(oldest_day - 2));
        self.counts.retain(|(_, d), _| *d >= oldest_day - 2);
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = File::create(&tmp).with_context(|| format!("making {}", tmp.display()))?;
        f.write_all(bytes)?;
        // durable on the drive before the rename can be: a power cut must never leave the new name over old data
        full_flush(&f)?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("renaming over {}", path.display()))?;
    let d = File::open(path.parent().unwrap_or(Path::new(".")))?;
    full_flush(&d)?;
    Ok(())
}

// ---------------------------------------------------------------- links.rec

/// A file's place, and the first article number each group gave of it (for late parts' leads).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaceRec {
    pub(crate) place: Place,
    pub firsts: BTreeMap<i64, i64>,
    pub added: i64,
}

/// An upload saved: its release's group, its seed, its files, and the archive step's progress through its guesses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadRec {
    pub group: String,
    pub seed: Seed,
    pub files: i64,
    pub generation: i64,
    pub probed: i64,
    pub added: i64,
}

/// Where files went and what uploads were saved, kept in `links.rec`.
pub struct Links {
    path: PathBuf,
    pub places: HashMap<String, PlaceRec>,
    pub uploads: HashMap<String, UploadRec>,
}

/// First line of a `links.rec` written with commit markers: each append ends with `C\t<n>\tok` (the `ok` soo a marker
/// cut short and glued to a newline never reads as a smaller count), and a pass's lines are
/// applied only once its marker is read whole. A file without it is from before markers: its whole lines all count.
const LINKS_HEADER: &str = "V\t2\n";

/// The time and message-id hashes of an `I` line.
fn id_line(line: &str) -> Option<(i64, impl Iterator<Item = u64> + '_)> {
    let (at, list) = line.strip_prefix("I\t")?.split_once('\t')?;
    Some((at.parse().ok()?, list.split(',').filter_map(|x| u64::from_str_radix(x, 16).ok())))
}

fn csv<T: std::fmt::Display>(v: impl IntoIterator<Item = T>) -> String {
    v.into_iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",")
}

fn field_ok(s: &str) -> bool {
    !s.contains('\t') && !s.contains('\n')
}

impl Links {
    pub fn place_line(poster: &str, r: &PlaceRec) -> String {
        let p = &r.place;
        format!(
            "F\t{}\t{poster}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            r.added,
            p.release,
            p.group,
            p.file,
            p.files,
            p.parts,
            p.oldest,
            p.leads,
            csv(r.firsts.iter().map(|(g, n)| format!("{g}:{n}")))
        )
    }

    pub fn upload_line(release: &str, u: &UploadRec) -> String {
        format!(
            "U\t{}\t{release}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            u.added,
            u.group,
            csv(&u.seed.groups),
            u.seed.first,
            u.seed.last,
            u.files,
            u.generation,
            u.probed
        )
    }

    /// `I` lines for `hashes` linked at `at`, `IDS_PER_LINE` a line.
    pub fn id_lines(hashes: &[u64], at: i64) -> Vec<String> {
        hashes
            .chunks(IDS_PER_LINE)
            .map(|c| format!("I\t{at}\t{}\n", c.iter().map(|h| format!("{h:x}")).collect::<Vec<_>>().join(",")))
            .collect()
    }

    /// A place, upload or probe line applied; `I` lines aren't held here (their ids go to [`LinkedIds`]).
    fn apply(&mut self, line: &str) {
        let f: Vec<&str> = line.split('\t').collect();
        let num = |s: &str| s.parse::<i64>().ok();
        match f.as_slice() {
            ["F", added, poster, release, group, file, files, parts, oldest, leads, firsts] => {
                let (Some(added), Some(file), Some(files), Some(parts), Some(oldest)) =
                    (num(added), num(file), num(files), num(parts), num(oldest))
                else {
                    return;
                };
                let firsts = firsts
                    .split(',')
                    .filter_map(|x| x.split_once(':'))
                    .filter_map(|(g, n)| Some((g.parse().ok()?, n.parse().ok()?)))
                    .collect();
                let place = Place {
                    leads: (*leads).to_string(),
                    oldest,
                    release: (*release).to_string(),
                    group: (*group).to_string(),
                    file,
                    files,
                    parts,
                };
                self.places.insert((*poster).to_string(), PlaceRec { place, firsts, added });
            }
            ["U", added, release, group, groups, first, last, files, generation, probed] => {
                let (Some(added), Some(first), Some(last), Some(files), Some(generation), Some(probed)) =
                    (num(added), num(first), num(last), num(files), num(generation), num(probed))
                else {
                    return;
                };
                let groups = groups.split(',').filter_map(|g| g.parse().ok()).collect();
                self.uploads.insert(
                    (*release).to_string(),
                    UploadRec {
                        group: (*group).to_string(),
                        seed: Seed { groups, first, last },
                        files,
                        generation,
                        probed,
                        added,
                    },
                );
            }
            ["P", release, generation, probed] => {
                if let (Some(u), Some(generation), Some(probed)) =
                    (self.uploads.get_mut(*release), num(generation), num(probed))
                    && u.generation == generation
                {
                    u.probed = u.probed.max(probed);
                }
            }
            _ => {}
        }
    }

    /// The records in `dir`, those older than their life dropped; the file rewritten when most of it is dead.
    pub fn open(dir: &Path, now: i64) -> Result<Links> {
        Ok(Links::open_with_ids(dir, now)?.0)
    }

    /// [`Links::open`], and the message-id hashes of its `I` lines (with their times), handed out rather than kept.
    pub fn open_with_ids(dir: &Path, now: i64) -> Result<(Links, HashMap<u64, i64>)> {
        let path = dir.join(LINKS_FILE);
        let mut l = Links { path: path.clone(), places: HashMap::new(), uploads: HashMap::new() };
        let mut ids: HashMap<u64, i64> = HashMap::new();
        let text = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let text = String::from_utf8_lossy(&text);
        let marked = text.starts_with(LINKS_HEADER);
        let mut apply = |l: &mut Links, line: &str| match id_line(line) {
            Some((at, hs)) => hs.for_each(|h| {
                ids.insert(h, at);
            }),
            None => l.apply(line),
        };
        let mut lines = 0usize;
        // lines of an append whose marker isn't read yet; a torn last line (no newline) was never acknowledged
        let mut pending: Vec<&str> = Vec::new();
        for line in text.split_inclusive('\n').filter_map(|x| x.strip_suffix('\n')) {
            lines += 1;
            if !marked {
                apply(&mut l, line);
            } else if let Some(n) = line.strip_prefix("C\t") {
                // the marker's own append is the last `n` lines; any before them are of an append cut short
                if let Some((n, "ok")) = n.split_once('\t')
                    && let Ok(n) = n.parse::<usize>()
                    && n <= pending.len()
                {
                    for line in &pending[pending.len() - n..] {
                        apply(&mut l, line);
                    }
                }
                pending.clear();
            } else if line != LINKS_HEADER.trim_end() {
                pending.push(line);
            }
        }
        let torn = !pending.is_empty() || !text.ends_with('\n') || !marked;
        let before = now - LINKS_LIFE;
        l.places.retain(|_, p| p.added >= before);
        l.uploads.retain(|_, u| u.added >= before);
        ids.retain(|_, at| *at >= before);
        let mut by_time: BTreeMap<i64, Vec<u64>> = BTreeMap::new();
        for (h, at) in &ids {
            by_time.entry(*at).or_default().push(*h);
        }
        // a rewrite writes each time's ids in as few lines as hold them
        let id_lines: usize = by_time.values().map(|v| v.len().div_ceil(IDS_PER_LINE)).sum();
        let live = l.places.len() + l.uploads.len() + id_lines + 2;
        if lines > 2 * live + 1000 || torn {
            let mut out = String::from(LINKS_HEADER);
            let mut n = 0usize;
            for (poster, p) in &l.places {
                out.push_str(&Links::place_line(poster, p));
                n += 1;
            }
            for (release, u) in &l.uploads {
                out.push_str(&Links::upload_line(release, u));
                n += 1;
            }
            for (at, hashes) in by_time {
                for line in Links::id_lines(&hashes, at) {
                    out.push_str(&line);
                    n += 1;
                }
            }
            out.push_str(&format!("C\t{n}\tok\n"));
            write_atomic(&path, out.as_bytes())?;
        }
        Ok((l, ids))
    }

    /// Lines appended and made durable with their commit marker after them, then applied (`I` lines aside). On a
    /// write or flush error the file is cut back to its length before, soo no fragment is left for the next append
    /// to glue onto; a fragment left anyway (a crash mid-write) is closed off with a newline first.
    pub fn append(&mut self, lines: &[String]) -> Result<()> {
        use std::os::unix::fs::FileExt;
        if lines.is_empty() {
            return Ok(());
        }
        let existed = self.path.exists();
        let f = OpenOptions::new().create(true).read(true).append(true).open(&self.path)?;
        let len = f.metadata()?.len();
        let mut buf = String::new();
        if len == 0 {
            buf.push_str(LINKS_HEADER);
        } else {
            let mut last = [0u8; 1];
            f.read_exact_at(&mut last, len - 1)?;
            if last[0] != b'\n' {
                buf.push('\n');
            }
        }
        for l in lines {
            buf.push_str(l);
        }
        buf.push_str(&format!("C\t{}\tok\n", lines.len()));
        let wrote = (|| -> std::io::Result<()> {
            let mut w = &f;
            #[cfg(test)]
            if let Some(k) = TEAR_AFTER.take() {
                w.write_all(&buf.as_bytes()[..k.min(buf.len())])?;
                return Err(std::io::Error::other("torn for a test"));
            }
            w.write_all(buf.as_bytes())?;
            if existed {
                full_flush(&f)
            } else {
                let d = File::open(self.path.parent().unwrap_or(Path::new(".")))?;
                sync_all_durable(&[&f, &d])
            }
        })();
        if let Err(e) = wrote {
            if let Err(t) = f.set_len(len) {
                eprintln!("header store linker: couldnt cut {} back after a failed append: {t}", self.path.display());
            }
            return Err(e).with_context(|| format!("appending to {}", self.path.display()));
        }
        for l in lines {
            if id_line(l).is_none() {
                self.apply(l.trim_end_matches('\n'));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    /// The next append on this thread writes this many bytes and then fails.
    static TEAR_AFTER: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

// ---------------------------------------------------------------- linked-ids.rec

const IDS_FILE: &str = "linked-ids.rec";
const IDS_RECORD: usize = 16;
/// message-id hashes per `I` line of `links.rec`
const IDS_PER_LINE: usize = 4096;

/// Every article linked within `LINKS_LIFE`, by message-id hash: noted in `links.rec`'s `I` lines (and read from the
/// older `linked-ids.rec`, `[hash u64][added i64]` records): an article is linked at most once, soo a late block holding it again (from another server, or read again
/// after a pass that wasn't fully noted) never adds it as more parts.
pub struct LinkedIds {
    set: HashSet<u64>,
}

impl LinkedIds {
    pub fn open(dir: &Path, now: i64) -> Result<LinkedIds> {
        let path = dir.join(IDS_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let before = now - LINKS_LIFE;
        let mut set = HashSet::new();
        let mut kept = Vec::with_capacity(bytes.len());
        // a torn last record was never acknowledged: dropped
        for r in bytes.as_chunks::<IDS_RECORD>().0 {
            let added = i64::from_le_bytes(r[8..16].try_into().expect("8 bytes"));
            if added >= before {
                set.insert(u64::from_le_bytes(r[..8].try_into().expect("8 bytes")));
                kept.extend_from_slice(r);
            }
        }
        if kept.len() != bytes.len() && (kept.len() * 2 < bytes.len() || bytes.len() % IDS_RECORD != 0) {
            write_atomic(&path, &kept)?;
        }
        Ok(LinkedIds { set })
    }

    pub fn contains(&self, h: u64) -> bool {
        self.set.contains(&h)
    }

    /// Hashes held (noted elsewhere: `links.rec`'s `I` lines).
    pub fn extend(&mut self, hashes: impl IntoIterator<Item = u64>) {
        self.set.extend(hashes);
    }

    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
}

// ---------------------------------------------------------------- readiness

/// Whether wall-clock hour `h` is ready at `now`: two hours (and the settle time) past its end, and no backfill day
/// covering it and two hours either side still to read; or its UTC day's rows first came over a day ago.
pub fn ready(h: i64, now: i64, not_done: &HashSet<i64>, seen: &BTreeMap<i64, i64>) -> bool {
    let end = (h + 1) * HOUR;
    if end + LIVE_WAIT > now {
        return false;
    }
    // the UTC days a row of this wall-clock hour can sit in, whatever the poster's zone, and two hours either side
    let (first_day, last_day) = ((h * HOUR - MARGIN - ZONE).div_euclid(DAY), (end - 1 + MARGIN + ZONE).div_euclid(DAY));
    if !(first_day..=last_day).any(|d| not_done.contains(&d)) {
        return true;
    }
    // let go at the latest a day after its rows came, as `xref` settles a file a day after it arrives: the UTC days a
    // row of wall-clock hour `h` can sit in (a poster's zone moves it up to 14h either way), those that have rows all
    // first seen over a day ago
    let days = (h * HOUR - ZONE).div_euclid(DAY)..=((h + 1) * HOUR + ZONE).div_euclid(DAY);
    let stamps: Vec<i64> = days.filter_map(|d| seen.get(&d).copied()).collect();
    !stamps.is_empty() && stamps.iter().all(|&t| now - t > KEEP_SECS)
}

// ---------------------------------------------------------------- a pass

/// One kept article.
struct Art {
    poster: u32,
    posted: i64,
    grp: i64,
    number: i64,
    bytes: i64,
    lines: i64,
    msgid: (u64, u16),
    /// from a block committed after the last pass (or of a segment no pass has read)
    late: bool,
}

/// What a pass found: releases to save, then `ack`.
pub struct Found {
    pub releases: Vec<Release>,
    /// message-id hashes of every article in `releases`, noted as linked by `ack`
    ids: Vec<u64>,
    lines: Vec<String>,
    state: State,
    pub hours: Vec<i64>,
    pub stats: PassStats,
}

#[derive(Clone, Debug, Default)]
pub struct PassStats {
    pub blocks: u64,
    pub rows: u64,
    pub kept: u64,
    pub repeats: u64,
    pub files: u64,
    pub uploads: u64,
    pub late_files: u64,
    /// late rows of hours not linked yet, left to those hours' passes
    pub late_unready: u64,
    /// posters posting again past `JOIN_GAP` under a name already placed: a new upload each
    pub posted_again: u64,
    /// rows of articles linked before (in an earlier pass), passed over
    pub already_linked: u64,
    /// blocks that wouldn't read, passed over
    pub bad_blocks: u64,
    pub read_secs: f64,
    pub secs: f64,
}

pub struct Linker {
    dir: PathBuf,
    pub state: State,
    pub links: Arc<Mutex<Links>>,
    pub ids: LinkedIds,
}

impl Linker {
    /// The linker over `store`, its state read from the store's `_state` folder.
    pub fn open(store: &Store, now: i64) -> Result<Linker> {
        let dir = store.root().join("_state");
        std::fs::create_dir_all(&dir)?;
        Linker::open_in(&dir, now)
    }

    /// A linker with its state in `dir` (a scratch folder, for trying it on a copy).
    pub fn open_in(dir: &Path, now: i64) -> Result<Linker> {
        std::fs::create_dir_all(dir)?;
        let (links, linked) = Links::open_with_ids(dir, now)?;
        // articles linked: those of `links.rec`'s `I` lines, and of the older `linked-ids.rec`
        let mut ids = LinkedIds::open(dir, now)?;
        ids.extend(linked.into_keys());
        Ok(Linker { state: State::load(dir)?, links: Arc::new(Mutex::new(links)), ids, dir: dir.to_path_buf() })
    }

    /// UTC days the keep set must hold for the linker: those with a wall-clock hour it hasn't linked.
    pub fn unlinked_days(&self, store_days: &BTreeSet<i64>) -> BTreeSet<i64> {
        store_days
            .iter()
            .copied()
            .filter(|&d| {
                let (from, to) = (hour_of(d * DAY - ZONE), hour_of(d * DAY + DAY + ZONE) - 1);
                (from..=to).any(|h| !self.state.linked.contains(&h))
            })
            .collect()
    }

    /// `unlinked_days` over everything `store` holds, and the days with blocks this linker hasn't read yet (late rows).
    pub fn unlinked_in(&self, store: &Store) -> BTreeSet<i64> {
        let segs = store.segments();
        let mut days: BTreeSet<i64> = segs.iter().map(|x| x.1).collect();
        days.extend(store.open_days());
        let mut out = self.unlinked_days(&days);
        for (g, d, n) in &segs {
            if *n as u32 > self.state.counts.get(&(g.clone(), *d)).copied().unwrap_or(0) {
                out.insert(*d);
            }
        }
        out
    }

    /// One pass at `now`: None when no hour is ready and nothing came late. `not_done` is the backfill days still to
    /// read; `stop` is asked as it goes (a pass given up writes nothing).
    pub fn pass(
        &mut self,
        store: &Store,
        now: i64,
        not_done: &HashSet<i64>,
        stop: &dyn Fn() -> bool,
    ) -> Result<Option<Found>> {
        let started = Instant::now();
        let segs = store.segments();
        let open_days = store.open_days();
        // when each day's rows first came, kept whether or not this pass finds anything (the day-later fallback in
        // `ready` counts from it), and saved when a day is new
        let before = self.state.seen.len();
        for (_, day, n) in &segs {
            if *n > 0 {
                self.state.seen.entry(*day).or_insert(now);
            }
        }
        for d in &open_days {
            self.state.seen.entry(*d).or_insert(now);
        }
        if self.state.seen.len() != before {
            self.state.save(&self.dir)?;
        }
        let mut state = self.state.clone();
        // a segment gone, or holding fewer blocks than were counted (expired and made again): its count starts over,
        // soo its blocks are read (what was linked before is passed over by message-id)
        let present: HashMap<(String, i64), u32> = segs.iter().map(|(g, d, n)| ((g.clone(), *d), *n as u32)).collect();
        state.counts.retain(|k, c| present.get(k).is_some_and(|&n| n >= *c));
        let late_days: BTreeSet<i64> = segs
            .iter()
            .filter(|(g, d, n)| *n as u32 > state.counts.get(&(g.clone(), *d)).copied().unwrap_or(0))
            .map(|(_, d, _)| *d)
            .collect();
        // days with rows: committed, or still in slabs (which the barrier below seals)
        let mut days: BTreeSet<i64> = segs.iter().filter(|s| s.2 > 0).map(|s| s.1).collect();
        days.extend(open_days.iter().copied());
        // the earliest ready hours not linked, a run of them
        let mut candidates: BTreeSet<i64> = BTreeSet::new();
        for &d in &days {
            candidates.extend(hour_of(d * DAY - ZONE)..hour_of(d * DAY + DAY + ZONE));
        }
        let mut span: Vec<i64> = Vec::new();
        for h in candidates {
            if state.linked.contains(&h) {
                continue;
            }
            if !ready(h, now, not_done, &state.seen) {
                if !span.is_empty() {
                    break;
                }
                continue;
            }
            if span.last().is_some_and(|&l| l + 1 != h) || span.len() as i64 >= SPAN_HOURS {
                break;
            }
            span.push(h);
        }
        // late blocks of days whose hours are linked are taken on their own
        let late_only: BTreeSet<i64> = late_days
            .iter()
            .copied()
            .filter(|&d| (hour_of(d * DAY - ZONE)..hour_of(d * DAY + DAY + ZONE)).all(|h| state.linked.contains(&h)))
            .collect();
        if span.is_empty() && late_only.is_empty() {
            return Ok(None);
        }
        let (lo, hi) = match (span.first(), span.last()) {
            (Some(&a), Some(&b)) => {
                let lo = a * HOUR - MARGIN;
                // back to the first file of an upload that waited (never more than a day before the span)
                let lo = state.waiting_from.map_or(lo, |w| lo.min(w.max(lo - KEEP_SECS)));
                (lo, (b + 1) * HOUR + MARGIN - 1)
            }
            _ => (i64::MAX, i64::MIN),
        };
        let mut read_days: BTreeSet<i64> = late_only.clone();
        if lo <= hi {
            read_days.extend((lo - ZONE).div_euclid(DAY)..=(hi + ZONE).div_euclid(DAY));
        }
        let read_days: Vec<i64> = read_days.into_iter().collect();
        // the barrier: what the store still holds of those days sealed and committed
        store.flush_days(&read_days, Duration::from_secs(300))?;
        if stop() {
            return Ok(None);
        }
        let span_set: HashSet<i64> = span.iter().copied().collect();
        let mut stats = PassStats::default();
        let mut posters: Vec<String> = Vec::new();
        let mut poster_ids: HashMap<String, u32> = HashMap::new();
        let mut ids: Vec<u8> = Vec::new();
        let mut seen_ids: HashSet<u64> = HashSet::new();
        let mut arts: Vec<Art> = Vec::new();
        let mut group_names: HashMap<i64, String> = HashMap::new();
        let mut new_counts: Vec<((String, i64), u32)> = Vec::new();
        // blocks read before the last pass first, soo a repeat in a late block is the one dropped
        // one snapshot of every (group, day) read, for both rounds and the counts: a block committed while the pass
        // reads isn't half counted
        let mut snaps = Vec::new();
        for g in store.group_names() {
            let gid = group_id(&g);
            group_names.insert(gid, g.clone());
            for &d in &read_days {
                let (map, entries) = store.day_blocks(&g, d);
                let Some(map) = map else { continue };
                let old = state.counts.get(&(g.clone(), d)).copied().unwrap_or(0) as usize;
                new_counts.push(((g.clone(), d), entries.len() as u32));
                snaps.push((gid, map, entries, old));
            }
        }
        for late_round in [false, true] {
            for (gid, map, entries, old) in &snaps {
                let (gid, old) = (*gid, *old);
                {
                    for e in entries {
                        let late = e.seq as usize >= old;
                        if late != late_round {
                            continue;
                        }
                        // a block wholly outside the hours read, unless late
                        if !late && (e.max_ts + ZONE < lo || e.min_ts - ZONE > hi) {
                            continue;
                        }
                        if stop() {
                            return Ok(None);
                        }
                        stats.blocks += 1;
                        // a block that won't read is passed over (counted), not the whole pass given up
                        let Some(cur) = map
                            .get(e.offset as usize..)
                            .and_then(|b| NxvBlockView::from_mmap_slice(b).ok())
                            .and_then(|v| FieldCursor::new(v).ok())
                        else {
                            stats.bad_blocks += 1;
                            eprintln!(
                                "header store linker: a block of {} won't read; passed over",
                                group_names.get(&gid).map_or("?", |s| s.as_str())
                            );
                            continue;
                        };
                        // rows outside the hours read are passed over before their strings are decoded (late blocks are
                        // read whole)
                        let mut cur = if late { cur } else { cur.with_wall_window(lo, hi) };
                        stats.rows += e.rows as u64;
                        while let Some(r) = cur.next_row() {
                            let (subject, author, msgid, wall, bytes, lines, art);
                            let parsed;
                            if let Some(line) = r.literal {
                                let Some(ov) = crate::nntp::overview_of(line) else { continue };
                                parsed = ov.into_article();
                                if !crate::xref::keeps(&parsed) {
                                    continue;
                                }
                                let Some(w) = crate::dates::naive_timestamp(&parsed.date) else { continue };
                                (subject, author, msgid) =
                                    (parsed.subject.as_bytes(), parsed.author.as_bytes(), parsed.message_id.as_bytes());
                                (wall, bytes, lines, art) = (w, parsed.bytes, parsed.lines, parsed.number);
                            } else {
                                if !crate::xref::keeps_fields(r.subject, r.author, r.msgid) {
                                    continue;
                                }
                                (subject, author, msgid) = (r.subject, r.author, r.msgid);
                                let (b, l) = (r.bytes.unwrap_or(0), r.lines.unwrap_or(0));
                                // as `Overview::into_article`: no size listed, the lines say it
                                let b = if b > 0 { b } else { crate::nntp::bytes_from_lines(l) };
                                (wall, bytes, lines, art) = (r.wall, b, l, r.art);
                            }
                            let _ = subject;
                            if !late && (wall < lo || wall > hi) {
                                continue;
                            }
                            // a late block's rows of an hour not linked yet are left to that hour's own pass, which
                            // reads every block overlapping it, counted or not
                            if late && !(state.linked.contains(&hour_of(wall)) || span_set.contains(&hour_of(wall))) {
                                stats.late_unready += 1;
                                continue;
                            }
                            // hashed as the release's message-id string is (`Found::ids`), soo a non-UTF-8 id matches
                            let h =
                                nntp_xover_engine::msgid_index::msgid_hash(String::from_utf8_lossy(msgid).as_bytes());
                            if self.ids.contains(h) {
                                stats.already_linked += 1;
                                continue;
                            }
                            if !seen_ids.insert(h) {
                                stats.repeats += 1;
                                continue;
                            }
                            let poster = if author.is_ascii() {
                                std::str::from_utf8(author).unwrap_or_default().to_string()
                            } else {
                                crate::nntp::undo_mojibake(String::from_utf8_lossy(author).into_owned())
                            };
                            let pid = match poster_ids.get(&poster) {
                                Some(&p) => p,
                                None => {
                                    let p = posters.len() as u32;
                                    poster_ids.insert(poster.clone(), p);
                                    posters.push(poster);
                                    p
                                }
                            };
                            let at = ids.len() as u64;
                            ids.extend_from_slice(msgid);
                            arts.push(Art {
                                poster: pid,
                                posted: wall,
                                grp: gid,
                                number: art as i64,
                                bytes,
                                lines,
                                msgid: (at, msgid.len().min(u16::MAX as usize) as u16),
                                late,
                            });
                            stats.kept += 1;
                        }
                    }
                }
            }
        }
        stats.read_secs = started.elapsed().as_secs_f64();
        drop(poster_ids);
        let msgid_of = |a: &Art| {
            String::from_utf8_lossy(&ids[a.msgid.0 as usize..a.msgid.0 as usize + a.msgid.1 as usize]).into_owned()
        };
        // each poster's articles
        let mut by_poster: Vec<Vec<usize>> = vec![Vec::new(); posters.len()];
        for (i, a) in arts.iter().enumerate() {
            by_poster[a.poster as usize].push(i);
        }
        let links = self.links.lock().unwrap_or_else(|e| e.into_inner());
        let new_hour = |t: i64| span_set.contains(&hour_of(t));
        let mut out: HashMap<String, Release> = HashMap::new();
        let mut lines: Vec<String> = Vec::new();
        // uploads saved within the window, which late articles widen before new files are matched to them
        let mut saved: Vec<(String, UploadRec)> = links
            .uploads
            .iter()
            .filter(|(_, u)| u.added >= now - KEEP_SECS)
            .map(|(r, u)| (r.clone(), u.clone()))
            .collect();
        saved.sort_by(|a, b| (a.1.seed.first, &a.0).cmp(&(b.1.seed.first, &b.0)));
        let mut grown: BTreeSet<usize> = BTreeSet::new();
        let mut fresh: Vec<FileSum> = Vec::new();
        let mut fresh_rows: Vec<Vec<usize>> = Vec::new();
        // per fresh file: a poster's later upload, named apart from its first
        let mut again_files: Vec<bool> = Vec::new();
        for (pid, rows) in by_poster.iter().enumerate() {
            if rows.is_empty() {
                continue;
            }
            let poster = &posters[pid];
            // the poster's rows that go to new files, and whether they're a later upload of a poster already placed
            let mut rest: Vec<usize> = rows.clone();
            let mut again = false;
            if let Some(pr) = links.places.get(poster) {
                // a file already placed: its new articles (late, or of the hours linked now) within its upload's time
                // are more of its parts; the rest, before or after it by more than `JOIN_GAP` (hours are linked out of
                // order, live before backfill), are the poster uploading again under the same name
                let new: Vec<usize> =
                    rows.iter().copied().filter(|&i| arts[i].late || new_hour(arts[i].posted)).collect();
                if new.is_empty() {
                    continue;
                }
                let window =
                    links.uploads.get(&pr.place.release).map(|u| (u.seed.first - JOIN_GAP, u.seed.last + JOIN_GAP));
                let (join, others): (Vec<usize>, Vec<usize>) = new
                    .iter()
                    .partition(|&&i| window.is_some_and(|(lo, hi)| arts[i].posted >= lo && arts[i].posted <= hi));
                // within its upload's time but its first articles left the window long ago: one read again couldnt be
                // told from a new one, soo those are let go
                if !join.is_empty() && pr.place.oldest >= now - KEEP_SECS {
                    stats.late_files += 1;
                    let mut file_rows = to_rows(&arts, &join, &msgid_of, now);
                    for r in &mut file_rows {
                        r.lead = r.lead && pr.firsts.get(&r.grp).is_none_or(|&n| r.number < n);
                    }
                    let parts = pr.place.parts + file_rows.len() as i64;
                    let mut firsts = pr.firsts.clone();
                    for r in &file_rows {
                        let e = firsts.entry(r.grp).or_insert(r.number);
                        *e = (*e).min(r.number);
                    }
                    let leads = crate::xref::with_leads(
                        &pr.place.leads,
                        crate::xref::add_file(&mut out, &pr.place, poster, file_rows, parts),
                    );
                    let rec = PlaceRec { place: Place { parts, leads, ..pr.place.clone() }, firsts, added: now };
                    lines.push(Links::place_line(poster, &rec));
                    if let Some(j) = saved.iter().position(|(r, _)| *r == pr.place.release) {
                        let seed = &mut saved[j].1.seed;
                        for &i in &join {
                            seed.groups.insert(arts[i].grp);
                            seed.first = seed.first.min(arts[i].posted);
                            seed.last = seed.last.max(arts[i].posted);
                        }
                        grown.insert(j);
                    }
                }
                if others.is_empty() {
                    continue;
                }
                stats.posted_again += 1;
                rest = others;
                again = true;
            }
            // runs apart by more than `JOIN_GAP` are separate files (separate uploads, when the poster is placed)
            rest.sort_by_key(|&i| (arts[i].posted, i));
            let mut runs: Vec<Vec<usize>> = Vec::new();
            for i in rest {
                match runs.last_mut() {
                    Some(r) if again && arts[i].posted - arts[*r.last().expect("non-empty")].posted > JOIN_GAP => {
                        runs.push(vec![i])
                    }
                    Some(r) => r.push(i),
                    None => runs.push(vec![i]),
                }
            }
            for run in runs {
                let mut f = FileSum {
                    poster: poster.clone(),
                    n: 0,
                    first: i64::MAX,
                    last: i64::MIN,
                    added: 0,
                    groups: BTreeSet::new(),
                };
                for &i in &run {
                    f.n += 1;
                    f.first = f.first.min(arts[i].posted);
                    f.last = f.last.max(arts[i].posted);
                    f.groups.insert(arts[i].grp);
                }
                fresh.push(f);
                fresh_rows.push(run);
                again_files.push(again);
            }
        }
        stats.files = fresh.len() as u64;
        let seeds: Vec<Seed> = saved.iter().map(|(_, u)| u.seed.clone()).collect();
        let chains = crate::xref::chain(&fresh, &seeds);
        let linked_or_span = |h: i64| state.linked.contains(&h) || span_set.contains(&h);
        let mut waiting_min: Option<i64> = None;
        let mut widened: HashSet<usize> = HashSet::new();
        for (k, (seed, files)) in chains.into_iter().enumerate() {
            if k % 4096 == 0 && stop() {
                return Ok(None);
            }
            let first = files.iter().map(|&i| fresh[i].first).min().unwrap_or(0);
            let last = files.iter().map(|&i| fresh[i].last).max().unwrap_or(0);
            // every hour it spans read, and something in it new: else it waits, or was seen before
            if !(hour_of(first)..=hour_of(last)).all(linked_or_span) {
                waiting_min = Some(waiting_min.map_or(first, |w: i64| w.min(first)));
                continue;
            }
            let new_material =
                files.iter().any(|&i| fresh_rows[i].iter().any(|&a| arts[a].late || new_hour(arts[a].posted)));
            if !new_material {
                continue;
            }
            let articles: i64 = files.iter().map(|&i| fresh[i].n).sum();
            if seed.is_none() && articles < MIN_UPLOAD_ARTICLES {
                continue;
            }
            let mut by_group: HashMap<i64, usize> = HashMap::new();
            let rows_by_file: Vec<Vec<Row>> = files
                .iter()
                .map(|&i| {
                    let rows = to_rows(&arts, &fresh_rows[i], &msgid_of, now);
                    for r in &rows {
                        *by_group.entry(r.grp).or_default() += 1;
                    }
                    rows
                })
                .collect();
            let mut span_seed = Seed { groups: by_group.keys().copied().collect(), first, last };
            let (release, group, before, generation, probed) = match seed {
                Some(j) => {
                    let (name, u) = &saved[j];
                    span_seed.groups.extend(u.seed.groups.iter().copied());
                    span_seed.first = span_seed.first.min(u.seed.first);
                    span_seed.last = span_seed.last.max(u.seed.last);
                    widened.insert(j);
                    (name.clone(), u.group.clone(), u.files, u.generation + 1, 0)
                }
                None => {
                    let home = crate::xref::home_group(&by_group);
                    let group = group_names.get(&home).cloned().unwrap_or_default();
                    let f0 = &fresh[files[0]];
                    let key =
                        if again_files[files[0]] { second_upload_key(&f0.poster, f0.first) } else { f0.poster.clone() };
                    (crate::xref::upload_name(&key), group, 0, 0, 0)
                }
            };
            let count = before + files.len() as i64;
            for (k, (&i, rows)) in files.iter().zip(rows_by_file).enumerate() {
                let parts = rows.len() as i64;
                let mut firsts: BTreeMap<i64, i64> = BTreeMap::new();
                for r in &rows {
                    let e = firsts.entry(r.grp).or_insert(r.number);
                    *e = (*e).min(r.number);
                }
                let place = Place {
                    release: release.clone(),
                    group: group.clone(),
                    file: before + k as i64 + 1,
                    files: count,
                    parts: 0,
                    leads: String::new(),
                    oldest: now,
                };
                let leads =
                    crate::xref::with_leads("", crate::xref::add_file(&mut out, &place, &fresh[i].poster, rows, parts));
                let rec = PlaceRec { place: Place { parts, leads, ..place }, firsts, added: now };
                lines.push(Links::place_line(&fresh[i].poster, &rec));
            }
            stats.uploads += 1;
            let rec = UploadRec { group, seed: span_seed, files: count, generation, probed, added: now };
            lines.push(Links::upload_line(&release, &rec));
        }
        // uploads late articles grew and no new file joined
        for j in grown {
            if !widened.contains(&j) {
                let (name, u) = &saved[j];
                let rec = UploadRec { generation: u.generation + 1, probed: 0, added: now, ..u.clone() };
                lines.push(Links::upload_line(name, &rec));
            }
        }
        drop(links);
        // what this pass read now counts as seen
        for (key, n) in new_counts {
            state.counts.insert(key, n);
        }
        state.linked.extend(span.iter().copied());
        // a pass that linked hours sets where the next one reads back to; one of late rows only leaves it
        if !span.is_empty() {
            state.waiting_from = waiting_min;
        }
        if let Some(&oldest) = days.first() {
            state.forget_before(oldest);
        }
        lines.retain(|l| l.trim_end_matches('\n').split('\t').all(field_ok));
        stats.secs = started.elapsed().as_secs_f64();
        let releases = crate::xref::finish_releases(out);
        let ids = releases
            .iter()
            .flat_map(|r| r.articles.iter())
            .map(|a| nntp_xover_engine::msgid_index::msgid_hash(a.message_id.as_bytes()))
            .collect();
        Ok(Some(Found { releases, ids, lines, state, hours: span, stats }))
    }

    /// Notes what `found`'s pass did, once its releases are saved: the links appended, then the state rewritten.
    pub fn ack(&mut self, found: Found) -> Result<()> {
        // the articles linked and where their files went in one append, ids first: noted together or not at all (a
        // torn last line is dropped on reading back), soo a retried pass neither adds articles again as parts nor
        // leaves them linked with no place
        let mut lines = Links::id_lines(&found.ids, chrono::Utc::now().timestamp());
        lines.extend(found.lines.iter().cloned());
        self.links.lock().unwrap_or_else(|e| e.into_inner()).append(&lines)?;
        self.ids.extend(found.ids.iter().copied());
        found.state.save(&self.dir)?;
        self.state = found.state;
        Ok(())
    }
}

/// The name key of a poster's later upload (posted again past `JOIN_GAP` under the same name): the poster and the hour
/// its first article went up, soo the release doesn't fold into the first one's.
fn second_upload_key(poster: &str, first: i64) -> String {
    format!("{poster}\0{}", hour_of(first))
}

/// A file's articles as the rows `xref::add_file` takes: in group, number and message-id order, each group's first
/// marked as a lead.
fn to_rows(arts: &[Art], idx: &[usize], msgid_of: &dyn Fn(&Art) -> String, now: i64) -> Vec<Row> {
    let mut rows: Vec<Row> = idx
        .iter()
        .map(|&i| {
            let a = &arts[i];
            Row {
                message_id: msgid_of(a),
                added: now,
                lead: false,
                posted: a.posted,
                grp: a.grp,
                number: a.number,
                bytes: a.bytes,
                lines: a.lines,
            }
        })
        .collect();
    rows.sort_by(|a, b| (a.grp, a.number, &a.message_id).cmp(&(b.grp, b.number, &b.message_id)));
    let mut last: Option<i64> = None;
    for r in &mut rows {
        r.lead = last != Some(r.grp);
        last = Some(r.grp);
    }
    rows
}

// ---------------------------------------------------------------- in the indexer

static ACTIVE: OnceLock<Mutex<Option<Arc<Mutex<Links>>>>> = OnceLock::new();

/// The links of the running linker, when this linker is the one running.
fn active_links() -> Option<Arc<Mutex<Links>>> {
    ACTIVE.get().and_then(|m| m.lock().ok().and_then(|g| g.clone()))
}

/// Whether the indexer runs this linker (and the SQLite one is off).
pub fn active() -> bool {
    active_links().is_some()
}

/// `xref::tried` for this linker's uploads: Some when it's running.
pub fn tried(release: &str) -> Option<Option<(i64, i64)>> {
    let links = active_links()?;
    let l = links.lock().unwrap_or_else(|e| e.into_inner());
    Some(l.uploads.get(release).map(|u| (u.probed, u.generation)))
}

/// `xref::tried_through` for this linker's uploads: false when it isn't running.
pub fn tried_through(release: &str, generation: i64, count: i64) -> bool {
    let Some(links) = active_links() else { return false };
    let mut l = links.lock().unwrap_or_else(|e| e.into_inner());
    if l.uploads.get(release).is_some_and(|u| u.generation == generation && u.probed < count) {
        let line = format!("P\t{release}\t{generation}\t{count}\n");
        // a failed append may leave the file torn: fail-stop like a pass's note, and the next start reads it back
        if let Err(e) = l.append(&[line]) {
            eprintln!(
                "header store linker: couldnt note guesses tried for {release}: {e:#}; exiting soo recovery runs"
            );
            std::process::exit(70);
        }
    }
    true
}

/// `xref::leads` for this linker's uploads: Some when it's running.
pub fn leads(release: &str, file: i64) -> Option<Vec<i64>> {
    let links = active_links()?;
    let l = links.lock().unwrap_or_else(|e| e.into_inner());
    let leads = l
        .places
        .values()
        .find(|p| p.place.release == release && p.place.file == file)
        .map(|p| p.place.leads.split(',').filter_map(|x| x.parse().ok()).collect())
        .unwrap_or_default();
    Some(leads)
}

static UNLINKED: Mutex<BTreeSet<i64>> = Mutex::new(BTreeSet::new());

/// UTC days the linker still needs kept (`live::keep_now`).
pub fn unlinked_days() -> BTreeSet<i64> {
    UNLINKED.lock().map(|g| g.clone()).unwrap_or_default()
}

/// How often a pass is tried when the last one found nothing.
const PASS_EVERY: Duration = Duration::from_secs(60);

/// The backfill days still to read, from the main database (read only).
fn not_done_days(main: &dyn crate::pg::sql::Sql) -> Result<HashSet<i64>> {
    use crate::pg::sql::vals;
    let start: i64 = main.opt("select value from meta where key = 'backfill_forward_start'", &[])?.unwrap_or(i64::MIN);
    Ok(main
        .column("select distinct day from backfill_chunks where state != 2 and day >= $1", vals![start])?
        .into_iter()
        .collect())
}

/// Runs the linker in the indexer until `stopping`: a pass, its releases saved, then noted; again at once while
/// passes find work, else every `PASS_EVERY`.
pub async fn run(db: crate::indexer::Db, store: Arc<Store>, stopping: impl Fn() -> bool) {
    if db.is_native() {
        crate::pg::linker_runtime::run(db, store, stopping).await;
        return;
    }
    let now = chrono::Utc::now().timestamp();
    let linker = match Linker::open(&store, now) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("header store linker: couldnt open its state: {e:#}; not linking");
            return;
        }
    };
    let _ = ACTIVE.set(Mutex::new(Some(Arc::clone(&linker.links))));
    // the keep set knows the unlinked days from the state read, before any pass
    if let Ok(mut u) = UNLINKED.lock() {
        *u = linker.unlinked_in(&store);
    }
    let linker = Arc::new(Mutex::new(linker));
    while !stopping() {
        let not_done = match crate::indexer::on_db(&db, not_done_days).await {
            Ok(days) => days,
            Err(_) => {
                eprintln!("header store linker: could not read pending backfill days");
                sleep_unless(PASS_EVERY, &stopping).await;
                continue;
            }
        };
        let (l, s) = (Arc::clone(&linker), Arc::clone(&store));
        let found = tokio::task::spawn_blocking(move || -> Result<Option<Found>> {
            let mut lk = l.lock().unwrap_or_else(|e| e.into_inner());
            // refreshed before the pass too, soo a pass that fails still leaves the keep set current
            if let Ok(mut u) = UNLINKED.lock() {
                *u = lk.unlinked_in(&s);
            }
            let stop = || crate::frozen::STOPPING.load(std::sync::atomic::Ordering::Relaxed);
            let found = lk.pass(&s, chrono::Utc::now().timestamp(), &not_done, &stop)?;
            if let Ok(mut u) = UNLINKED.lock() {
                *u = lk.unlinked_in(&s);
            }
            Ok(found)
        })
        .await;
        let found = match found {
            Ok(Ok(Some(f))) => f,
            Ok(Ok(None)) => {
                // the linker has run: the keep set takes its unlinked days first, then expiry may go on
                store.set_keep(super::live::keep_now());
                store.arm_expiry();
                sleep_unless(PASS_EVERY, &stopping).await;
                continue;
            }
            Ok(Err(e)) => {
                eprintln!("header store linker: {e:#}");
                sleep_unless(PASS_EVERY, &stopping).await;
                continue;
            }
            Err(e) => {
                eprintln!("header store linker: the pass failed: {e}");
                sleep_unless(PASS_EVERY, &stopping).await;
                continue;
            }
        };
        let st = found.stats.clone();
        let linked_hours = !found.hours.is_empty();
        let (count, articles) = (found.releases.len(), found.releases.iter().map(|r| r.articles.len()).sum::<usize>());
        let mut by_shard: HashMap<usize, Vec<Release>> = HashMap::new();
        for r in &found.releases {
            by_shard.entry(crate::store::shard_of(&r.group)).or_default().push(r.clone());
        }
        let mut saved = true;
        for (_, releases) in by_shard {
            if let Err(e) = db.save(releases).await {
                eprintln!("header store linker: couldnt save uploads: {e:#}");
                saved = false;
            }
        }
        if !saved {
            // the same hours again next time, to the same names and parts
            sleep_unless(PASS_EVERY, &stopping).await;
            continue;
        }
        let l = Arc::clone(&linker);
        match tokio::task::spawn_blocking(move || l.lock().unwrap_or_else(|e| e.into_inner()).ack(found)).await {
            Ok(Ok(())) => {
                store.set_keep(super::live::keep_now());
                store.arm_expiry();
                if count > 0 || st.late_files > 0 {
                    println!(
                        "header store linker: saved {count} releases ({} uploads, {} files late), {articles} articles; {} blocks, {} rows in {:.0}s",
                        st.uploads, st.late_files, st.blocks, st.rows, st.secs
                    );
                }
            }
            // a note written in part (a torn record) mustn't be followed by more appends: fail-stop, and the next
            // start reads the files back to their last whole record
            Ok(Err(e)) => {
                eprintln!("header store linker: couldnt note the pass: {e:#}; exiting soo recovery runs");
                std::process::exit(70);
            }
            Err(e) => {
                eprintln!("header store linker: noting the pass failed: {e}; exiting soo recovery runs");
                std::process::exit(70);
            }
        }
        // a pass that linked no new hour (late rows only) doesn't go round again at once
        if !linked_hours {
            sleep_unless(PASS_EVERY, &stopping).await;
        }
    }
}

async fn sleep_unless(d: Duration, stopping: &impl Fn() -> bool) {
    let mut slept = Duration::ZERO;
    while slept < d && !stopping() {
        tokio::time::sleep(Duration::from_secs(1)).await;
        slept += Duration::from_secs(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nxv_store::store::{Clamp, KeepSet, StoreConfig};

    fn keep_all() -> KeepSet {
        KeepSet { live_from: i64::MIN / 2, ..KeepSet::default() }
    }

    fn cfg(root: &Path) -> StoreConfig {
        StoreConfig {
            sync_every: Duration::from_secs(3600),
            seal_idle: Duration::from_secs(3600),
            sealers: 1,
            ..StoreConfig::new(root)
        }
    }

    /// An overview line of an article obfuscated per article: its subject is its message-id's local part.
    fn ob(art: u64, poster: &str, secs: i64, msg: &str) -> Vec<u8> {
        let date = chrono::DateTime::from_timestamp(secs, 0).unwrap().format("%a, %d %b %Y %H:%M:%S +0000");
        format!("{art}\t{msg}\t{poster}\t{date}\t<{msg}@ngPost>\t\t740000\t5692").into_bytes()
    }

    /// An upload: a one-article index, then `files` files of `parts` articles each, to `groups`, back to back.
    fn upload(tag: &str, start: i64, files: usize, parts: usize, groups: usize) -> Vec<(usize, Vec<u8>)> {
        let mut out = Vec::new();
        let mut t = start;
        let mut n = 0u64;
        for f in 0..=files {
            let poster = format!("{tag}{f:03}@x.y");
            let count = if f == 0 { 1 } else { parts };
            for p in 0..count {
                for g in 0..groups {
                    n += 1;
                    out.push((g, ob(n, &poster, t, &format!("{tag}{f:03}p{p:03}g{g}"))));
                }
                t += 1;
            }
        }
        out
    }

    fn record(store: &Store, rows: &[(usize, Vec<u8>)], base: u64) {
        let mut by_group: BTreeMap<usize, Vec<Vec<u8>>> = BTreeMap::new();
        for (g, l) in rows {
            // article numbers per group, after `base`
            let rest = &l[l.iter().position(|&b| b == b'\t').unwrap()..];
            let n = base + by_group.get(g).map_or(0, Vec::len) as u64;
            let mut line = n.to_string().into_bytes();
            line.extend_from_slice(rest);
            by_group.entry(*g).or_default().push(line);
        }
        for (g, lines) in by_group {
            let t = store
                .record("srv", &format!("alt.binaries.g{g}"), &lines, Clamp::Unknown { now: 2_000_000_000 })
                .unwrap();
            store.tick().unwrap();
            t.wait(Duration::from_secs(5)).unwrap();
        }
    }

    const T0: i64 = 1_794_132_000; // 2026-11-08 10:00:00 UTC

    #[test]
    fn readiness_waits_for_backfill_days_and_the_live_wait() {
        let h = hour_of(T0);
        let mut not_done = HashSet::new();
        let seen = BTreeMap::new();
        assert!(!ready(h, T0 + 3600, &not_done, &seen), "too soon");
        assert!(ready(h, T0 + 3600 + LIVE_WAIT, &not_done, &seen));
        not_done.insert(T0.div_euclid(DAY));
        assert!(!ready(h, T0 + 10 * DAY, &not_done, &seen), "its day still to read");
        let mut seen = BTreeMap::new();
        seen.insert(T0.div_euclid(DAY), T0);
        assert!(ready(h, T0 + 2 * DAY, &not_done, &seen), "let go a day after its rows came");
        // an hour near midnight waits on the neighbouring day too
        let late = hour_of(T0.div_euclid(DAY) * DAY + DAY - 1);
        let mut nd = HashSet::new();
        nd.insert(T0.div_euclid(DAY) + 1);
        assert!(!ready(late, T0 + 10 * DAY, &nd, &BTreeMap::new()));
    }

    #[test]
    fn state_and_links_round_trip_and_a_torn_tail_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = State::default();
        s.linked.extend([5, 6, 7, 10]);
        s.seen.insert(20_000, 123);
        s.counts.insert(("alt.binaries.x".into(), 20_000), 4);
        s.save(dir.path()).unwrap();
        assert_eq!(State::load(dir.path()).unwrap(), s);
        assert!(State::from_text("L\t1\t2\n").is_none(), "no end line: cut short");
        let mut l = Links::open(dir.path(), 1000).unwrap();
        let place = Place {
            leads: "1,4".into(),
            oldest: 900,
            release: "xref-upload-1".into(),
            group: "g".into(),
            file: 2,
            files: 3,
            parts: 9,
        };
        let pr = PlaceRec { place, firsts: [(7, 100), (8, 200)].into(), added: 1000 };
        let u = UploadRec {
            group: "g".into(),
            seed: Seed { groups: [7, 8].into(), first: 1, last: 2 },
            files: 3,
            generation: 0,
            probed: 0,
            added: 1000,
        };
        l.append(&[
            Links::place_line("p@x", &pr),
            Links::upload_line("xref-upload-1", &u),
            "P\txref-upload-1\t0\t5\n".into(),
        ])
        .unwrap();
        drop(l);
        // a torn line after
        let mut f = OpenOptions::new().append(true).open(dir.path().join(LINKS_FILE)).unwrap();
        f.write_all(b"F\t1000\tq@x\txref-upload-2").unwrap();
        let l = Links::open(dir.path(), 1000).unwrap();
        assert_eq!(l.places.get("p@x"), Some(&pr));
        assert!(!l.places.contains_key("q@x"));
        assert_eq!(l.uploads.get("xref-upload-1").map(|u| u.probed), Some(5));
        // past their life, dropped
        let l = Links::open(dir.path(), 1000 + LINKS_LIFE + 1).unwrap();
        assert!(l.places.is_empty() && l.uploads.is_empty());
    }

    fn a_place(release: &str) -> PlaceRec {
        let place = Place {
            leads: "1".into(),
            oldest: 900,
            release: release.into(),
            group: "g".into(),
            file: 1,
            files: 2,
            parts: 4,
        };
        PlaceRec { place, firsts: [(7, 100)].into(), added: 1000 }
    }

    #[test]
    fn a_pass_cut_short_applies_none_of_its_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LINKS_FILE);
        let mut l = Links::open(dir.path(), 1000).unwrap();
        let mut whole = Links::id_lines(&[0x11], 1000);
        whole.push(Links::place_line("p@x", &a_place("xref-upload-1")));
        l.append(&whole).unwrap();
        // a pass whose ids and first place reached the disk, but not its last place nor its marker
        let mut cut = Links::id_lines(&[0xaa, 0xbb], 1000).concat();
        cut.push_str(&Links::place_line("q@x", &a_place("xref-upload-2")));
        OpenOptions::new().append(true).open(&path).unwrap().write_all(cut.as_bytes()).unwrap();
        let (l2, ids) = Links::open_with_ids(dir.path(), 1000).unwrap();
        assert!(l2.places.contains_key("p@x") && ids.contains_key(&0x11), "the whole pass stands");
        assert!(!l2.places.contains_key("q@x"), "no place of the cut pass");
        assert!(!ids.contains_key(&0xaa) && !ids.contains_key(&0xbb), "no id of the cut pass");
        // cut lines followed by a whole append (no reopen between): only the whole one counts
        OpenOptions::new().append(true).open(&path).unwrap().write_all(cut.as_bytes()).unwrap();
        l.append(&[Links::place_line("r@x", &a_place("xref-upload-3"))]).unwrap();
        assert!(!l.places.contains_key("q@x"), "the appending handle never held the cut pass");
        let (l3, ids) = Links::open_with_ids(dir.path(), 1000).unwrap();
        assert!(l3.places.contains_key("p@x") && l3.places.contains_key("r@x"));
        assert!(!l3.places.contains_key("q@x") && !ids.contains_key(&0xaa));
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn a_fragment_without_a_newline_doesnt_swallow_the_next_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LINKS_FILE);
        let mut l = Links::open(dir.path(), 1000).unwrap();
        l.append(&[Links::place_line("p@x", &a_place("xref-upload-1"))]).unwrap();
        // a torn append: part of a probe line, and a marker cut to look like a smaller count
        for frag in ["P\txref-upload-1\t0", "F\t1000\tq@x\txref-up", "I\t1000\taa,b\nC\t1"] {
            OpenOptions::new().append(true).open(&path).unwrap().write_all(frag.as_bytes()).unwrap();
            let mut whole = Links::id_lines(&[0x11], 1000);
            whole.push(Links::place_line("r@x", &a_place("xref-upload-3")));
            l.append(&whole).unwrap();
            let (l2, ids) = Links::open_with_ids(dir.path(), 1000).unwrap();
            assert!(l2.places.contains_key("p@x") && l2.places.contains_key("r@x"), "after {frag:?}");
            assert!(!l2.places.contains_key("q@x"));
            assert!(ids.contains_key(&0x11) && !ids.contains_key(&0xaa) && !ids.contains_key(&0xb), "after {frag:?}");
        }
    }

    #[test]
    fn a_failed_append_leaves_the_file_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LINKS_FILE);
        let mut l = Links::open(dir.path(), 1000).unwrap();
        l.append(&[Links::place_line("p@x", &a_place("xref-upload-1"))]).unwrap();
        let before = std::fs::read(&path).unwrap();
        // a write that fails part way through
        TEAR_AFTER.set(Some(20));
        assert!(l.append(&[Links::place_line("q@x", &a_place("xref-upload-2"))]).is_err());
        assert!(!l.places.contains_key("q@x"), "not applied");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn a_file_from_before_markers_is_read_whole_and_rewritten_with_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LINKS_FILE);
        let old = format!("{}I\t1000\taa,bb\n", Links::place_line("p@x", &a_place("xref-upload-1")));
        std::fs::write(&path, old).unwrap();
        let (l, ids) = Links::open_with_ids(dir.path(), 1000).unwrap();
        assert!(l.places.contains_key("p@x"));
        assert_eq!(ids.len(), 2);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(LINKS_HEADER) && text.ends_with("C\t2\tok\n"), "{text}");
        let (l, ids) = Links::open_with_ids(dir.path(), 1000).unwrap();
        assert!(l.places.contains_key("p@x"));
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn a_live_file_is_not_rewritten_on_open() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LINKS_FILE);
        let mut l = Links::open(dir.path(), 1000).unwrap();
        // ids enough for several lines a pass, a few passes at different times
        for pass in 0..5u64 {
            let hashes: Vec<u64> = (0..(IDS_PER_LINE as u64 * 2 + 7)).map(|i| pass << 32 | i).collect();
            let mut lines = Links::id_lines(&hashes, 1000 + pass as i64);
            lines.push(Links::place_line(&format!("p{pass}@x"), &a_place("xref-upload-1")));
            l.append(&lines).unwrap();
        }
        let ino = std::fs::metadata(&path).unwrap().ino();
        for _ in 0..3 {
            let (l, ids) = Links::open_with_ids(dir.path(), 1000).unwrap();
            assert_eq!(l.places.len(), 5);
            assert_eq!(ids.len(), 5 * (IDS_PER_LINE * 2 + 7));
        }
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), ino, "not rewritten");
    }

    #[test]
    fn a_day_pass_links_an_upload_across_groups_once_and_late_parts_join_it() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = Store::open(cfg(dir.path()), keep_all()).unwrap();
        let rows = upload("aa", T0 + 600, 5, 4, 2);
        record(&store, &rows, 1000);
        let now = T0 + 6 * 3600;
        let state = dir.path().join("lstate");
        let mut lk = Linker::open_in(&state, now).unwrap();
        let mut releases = Vec::new();
        while let Some(f) = lk.pass(&store, now, &HashSet::new(), &|| false).unwrap() {
            releases.extend(f.releases.clone());
            lk.ack(f).unwrap();
        }
        assert_eq!(releases.len(), 1, "one upload, one release");
        let r = &releases[0];
        assert!(r.name.starts_with(crate::xref::NAME_PREFIX));
        assert_eq!(r.articles.len(), 2 * (1 + 5 * 4), "every article of every file, in both groups");
        assert!(r.articles.iter().any(|a| a.subject.starts_with("[1/6] - ")), "the index is the first of six files");
        assert_eq!(r.collection.as_deref(), Some(format!("xref\0{}", r.name).as_str()));
        // nothing again
        assert!(lk.pass(&store, now + 3600, &HashSet::new(), &|| false).unwrap().is_none_or(|f| f.releases.is_empty()));
        // a late block: two more parts of the second file, from another server
        let date_t = T0 + 900;
        let late: Vec<Vec<u8>> = (0..2u64)
            .map(|k| {
                let msg = format!("aa001late{k}");
                ob(5000 + k, "aa001@x.y", date_t + k as i64, &msg)
            })
            .collect();
        let t = store.record("srv2", "alt.binaries.g0", &late, Clamp::Unknown { now: 2_000_000_000 }).unwrap();
        store.tick().unwrap();
        t.wait(Duration::from_secs(5)).unwrap();
        let mut more = Vec::new();
        while let Some(f) = lk.pass(&store, now + 7200, &HashSet::new(), &|| false).unwrap() {
            more.extend(f.releases.clone());
            lk.ack(f).unwrap();
        }
        assert_eq!(more.len(), 1);
        assert_eq!(more[0].name, r.name, "the late parts join the upload");
        assert_eq!(more[0].articles.len(), 2);
        assert!(more[0].articles[0].subject.contains("yEnc (9/10)"), "{}", more[0].articles[0].subject);
        // a restart reads the same state back
        let lk2 = Linker::open_in(&state, now + 7200).unwrap();
        assert_eq!(lk2.state, lk.state);
        assert!(lk2.links.lock().unwrap().places.contains_key("aa001@x.y"));
        store.close().unwrap();
    }

    #[test]
    fn an_upload_longer_than_the_margin_keeps_its_first_files() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = Store::open(cfg(dir.path()), keep_all()).unwrap();
        // an index then six files of 3,000 parts, a second apart: over five hours of posting
        let rows = upload("ll", T0 + 600, 6, 3000, 1);
        record(&store, &rows, 1000);
        let h0 = hour_of(T0);
        let mut lk = Linker::open_in(&dir.path().join("s"), T0).unwrap();
        // the first three hours ready: the upload runs on into hours not ready, soo it waits
        let now1 = (h0 + 3) * HOUR + LIVE_WAIT;
        let mut got = Vec::new();
        while let Some(f) = lk.pass(&store, now1, &HashSet::new(), &|| false).unwrap() {
            got.extend(f.releases.clone());
            lk.ack(f).unwrap();
        }
        assert!(got.is_empty(), "it waits for its last hours");
        assert_eq!(lk.state.waiting_from, Some(T0 + 600), "and where it starts is kept");
        // the rest ready: its first hour (linked meanwhile, past the margin) is read back
        let now2 = (h0 + 7) * HOUR + LIVE_WAIT;
        while let Some(f) = lk.pass(&store, now2, &HashSet::new(), &|| false).unwrap() {
            got.extend(f.releases.clone());
            lk.ack(f).unwrap();
        }
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].articles.len(), 1 + 6 * 3000, "every file, the first ones included");
        store.close().unwrap();
    }

    #[test]
    fn articles_listed_with_no_size_take_it_from_their_lines() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = Store::open(cfg(dir.path()), keep_all()).unwrap();
        let rows: Vec<(usize, Vec<u8>)> = upload("zz", T0 + 600, 3, 4, 1)
            .into_iter()
            .map(|(g, l)| (g, String::from_utf8(l).unwrap().replace("\t740000\t", "\t\t").into_bytes()))
            .collect();
        record(&store, &rows, 1000);
        let now = T0 + 6 * 3600;
        let mut lk = Linker::open_in(&dir.path().join("s"), now).unwrap();
        let mut got = Vec::new();
        while let Some(f) = lk.pass(&store, now, &HashSet::new(), &|| false).unwrap() {
            got.extend(f.releases.clone());
            lk.ack(f).unwrap();
        }
        assert_eq!(got.len(), 1);
        assert!(got[0].articles.iter().all(|a| a.bytes == crate::nntp::bytes_from_lines(5692)));
        store.close().unwrap();
    }

    #[test]
    fn a_pass_is_noted_in_one_append_ids_first() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = Store::open(cfg(dir.path()), keep_all()).unwrap();
        let rows = upload("nn", T0 + 600, 3, 4, 1);
        record(&store, &rows, 1000);
        let now = T0 + 6 * 3600;
        let state = dir.path().join("s");
        let mut lk = Linker::open_in(&state, now).unwrap();
        while let Some(f) = lk.pass(&store, now, &HashSet::new(), &|| false).unwrap() {
            lk.ack(f).unwrap();
        }
        let text = std::fs::read_to_string(state.join(LINKS_FILE)).unwrap();
        let first_i = text.find("I\t").unwrap();
        let first_f = text.find("F\t").unwrap();
        assert!(first_i < first_f, "the ids go first in the append");
        assert!(text.starts_with(LINKS_HEADER) && text.trim_end().rsplit('\n').next().unwrap().starts_with("C\t"));
        assert!(!state.join(IDS_FILE).exists(), "nothing written apart");
        let lk2 = Linker::open_in(&state, now).unwrap();
        assert_eq!(lk2.ids.len(), 1 + 3 * 4, "read back from the links");
        assert!(lk2.links.lock().unwrap().places.contains_key("nn001@x.y"));
        store.close().unwrap();
    }

    #[test]
    fn a_poster_posting_again_hours_later_is_a_new_upload_not_more_parts() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = Store::open(cfg(dir.path()), keep_all()).unwrap();
        let rows = upload("bb", T0 + 600, 3, 4, 1);
        record(&store, &rows, 1000);
        let now = T0 + 6 * 3600;
        let mut lk = Linker::open_in(&dir.path().join("s"), now).unwrap();
        let mut first = Vec::new();
        while let Some(f) = lk.pass(&store, now, &HashSet::new(), &|| false).unwrap() {
            first.extend(f.releases.clone());
            lk.ack(f).unwrap();
        }
        assert_eq!(first.len(), 1);
        // the same name posts twelve articles five hours after that upload ended: past JOIN_GAP
        let again_t = T0 + 600 + 5 * 3600;
        let again: Vec<Vec<u8>> =
            (0..12u64).map(|k| ob(9000 + k, "bb001@x.y", again_t + k as i64, &format!("bb001again{k}"))).collect();
        let t = store.record("srv", "alt.binaries.g0", &again, Clamp::Unknown { now: 2_000_000_000 }).unwrap();
        store.tick().unwrap();
        t.wait(Duration::from_secs(5)).unwrap();
        let later = again_t + 6 * 3600;
        let mut more = Vec::new();
        let mut posted_again = 0;
        while let Some(f) = lk.pass(&store, later, &HashSet::new(), &|| false).unwrap() {
            posted_again += f.stats.posted_again;
            more.extend(f.releases.clone());
            lk.ack(f).unwrap();
        }
        assert_eq!(posted_again, 1);
        assert_eq!(more.len(), 1, "one new upload");
        assert_ne!(more[0].name, first[0].name, "named apart from the first upload");
        assert_eq!(more[0].articles.len(), 12, "only the new articles");
        store.close().unwrap();
    }

    #[test]
    fn an_article_linked_before_is_not_added_again_from_a_late_block() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = Store::open(cfg(dir.path()), keep_all()).unwrap();
        let rows = upload("cc", T0 + 600, 3, 4, 1);
        record(&store, &rows, 1000);
        let now = T0 + 6 * 3600;
        let state = dir.path().join("s");
        let mut lk = Linker::open_in(&state, now).unwrap();
        let mut first = 0;
        while let Some(f) = lk.pass(&store, now, &HashSet::new(), &|| false).unwrap() {
            first += f.releases.iter().map(|r| r.articles.len()).sum::<usize>();
            lk.ack(f).unwrap();
        }
        assert_eq!(first, 1 + 3 * 4);
        // another server lists the same articles later: a late block of nothing new
        let again: Vec<Vec<u8>> = rows.iter().map(|(_, l)| l.clone()).collect();
        let t = store.record("srv2", "alt.binaries.g0", &again, Clamp::Unknown { now: 2_000_000_000 }).unwrap();
        store.tick().unwrap();
        t.wait(Duration::from_secs(5)).unwrap();
        let mut more = 0;
        let mut skipped = 0;
        while let Some(f) = lk.pass(&store, now + 3600, &HashSet::new(), &|| false).unwrap() {
            more += f.releases.iter().map(|r| r.articles.len()).sum::<usize>();
            skipped += f.stats.already_linked;
            lk.ack(f).unwrap();
        }
        assert_eq!(more, 0, "nothing added again");
        assert!(skipped > 0);
        // and after a restart, from the file
        let lk2 = Linker::open_in(&state, now + 3600).unwrap();
        assert_eq!(lk2.ids.len(), 1 + 3 * 4);
        store.close().unwrap();
    }

    #[test]
    fn with_backfill_never_done_an_upload_links_a_day_after_its_rows_came() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = Store::open(cfg(dir.path()), keep_all()).unwrap();
        let rows = upload("dd", T0 + 600, 3, 4, 1);
        record(&store, &rows, 1000);
        let state = dir.path().join("s");
        let mut lk = Linker::open_in(&state, T0).unwrap();
        // every day has backfill chunks still to read, as on a store reading weeks back
        let d = T0.div_euclid(DAY);
        let nd: HashSet<i64> = (d - 5..=d + 5).collect();
        let now = T0 + 6 * 3600;
        assert!(lk.pass(&store, now, &nd, &|| false).unwrap().is_none(), "nothing ready yet");
        // the first-seen time is kept across passes (and on disk), soo a day later the hour is let go
        let lk2 = Linker::open_in(&state, now).unwrap();
        assert!(!lk2.state.seen.is_empty(), "first-seen times saved");
        let later = now + DAY + 3600;
        let mut n = 0;
        while let Some(f) = lk.pass(&store, later, &nd, &|| false).unwrap() {
            n += f.releases.len();
            lk.ack(f).unwrap();
        }
        assert_eq!(n, 1);
        store.close().unwrap();
    }

    #[test]
    fn noise_and_hours_still_being_read_wait() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = Store::open(cfg(dir.path()), keep_all()).unwrap();
        // too small to be an upload
        let rows = upload("nn", T0 + 60, 1, 3, 1);
        record(&store, &rows, 1);
        let now = T0 + 6 * 3600;
        let mut lk = Linker::open_in(&dir.path().join("s"), now).unwrap();
        // its day and the days either side (whose hours its rows could fall in, in some zone) still to read
        let d = T0.div_euclid(DAY);
        let nd: HashSet<i64> = [d - 1, d, d + 1].into();
        assert!(lk.pass(&store, now, &nd, &|| false).unwrap().is_none(), "its days still to read");
        let mut n = 0;
        while let Some(f) = lk.pass(&store, now, &HashSet::new(), &|| false).unwrap() {
            n += f.releases.len();
            lk.ack(f).unwrap();
        }
        assert_eq!(n, 0, "noise saves nothing");
        store.close().unwrap();
    }
}
