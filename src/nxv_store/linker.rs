//! The cross-group linker over the day-segment store: what `xref` does over `atlas.xref.db`, reading the store's
//! blocks instead of a table of headers, behind `[header_store] linker`.
//!
//! The unit of work is an hour of the posters' wall clock (the scale `xref` keeps posting times on). An hour is
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
use super::sync::{fsync_only, full_flush, sync_all_durable};
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
        fsync_only(&f)?;
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
        let path = dir.join(LINKS_FILE);
        let mut l = Links { path: path.clone(), places: HashMap::new(), uploads: HashMap::new() };
        let text = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let mut lines = 0usize;
        // a torn last line (no newline) was never acknowledged: dropped
        for line in String::from_utf8_lossy(&text).split_inclusive('\n').filter_map(|x| x.strip_suffix('\n')) {
            lines += 1;
            l.apply(line);
        }
        let before = now - LINKS_LIFE;
        l.places.retain(|_, p| p.added >= before);
        l.uploads.retain(|_, u| u.added >= before);
        let live = l.places.len() + l.uploads.len();
        if lines > 2 * live + 1000 || !text.is_empty() && !text.ends_with(b"\n") {
            let mut out = String::new();
            for (poster, p) in &l.places {
                out.push_str(&Links::place_line(poster, p));
            }
            for (release, u) in &l.uploads {
                out.push_str(&Links::upload_line(release, u));
            }
            write_atomic(&path, out.as_bytes())?;
        }
        Ok(l)
    }

    /// Lines appended and made durable, then applied.
    pub fn append(&mut self, lines: &[String]) -> Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        let existed = self.path.exists();
        let f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        let mut w = &f;
        let mut buf = String::new();
        for l in lines {
            buf.push_str(l);
        }
        w.write_all(buf.as_bytes())?;
        if existed {
            full_flush(&f)?;
        } else {
            let d = File::open(self.path.parent().unwrap_or(Path::new(".")))?;
            sync_all_durable(&[&f, &d])?;
        }
        for l in lines {
            self.apply(l.trim_end_matches('\n'));
        }
        Ok(())
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
    let (first_day, last_day) = ((h * HOUR - MARGIN).div_euclid(DAY), (end - 1 + MARGIN).div_euclid(DAY));
    if !(first_day..=last_day).any(|d| not_done.contains(&d)) {
        return true;
    }
    // let go at the latest a day after its rows came, as `xref` settles a file a day after it arrives
    seen.get(&(h * HOUR).div_euclid(DAY)).is_some_and(|&t| now - t > KEEP_SECS)
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
    msgid: (u32, u16),
    /// from a block committed after the last pass (or of a segment no pass has read)
    late: bool,
}

/// What a pass found: releases to save, then `ack`.
pub struct Found {
    pub releases: Vec<Release>,
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
    pub read_secs: f64,
    pub secs: f64,
}

pub struct Linker {
    dir: PathBuf,
    pub state: State,
    pub links: Arc<Mutex<Links>>,
}

impl Linker {
    /// The linker over `store`, its state read from the store's `_state` folder.
    pub fn open(store: &Store, now: i64) -> Result<Linker> {
        let dir = store.root().join("_state");
        std::fs::create_dir_all(&dir)?;
        Ok(Linker { state: State::load(&dir)?, links: Arc::new(Mutex::new(Links::open(&dir, now)?)), dir })
    }

    /// A linker with its state in `dir` (a scratch folder, for trying it on a copy).
    pub fn open_in(dir: &Path, now: i64) -> Result<Linker> {
        std::fs::create_dir_all(dir)?;
        Ok(Linker {
            state: State::load(dir)?,
            links: Arc::new(Mutex::new(Links::open(dir, now)?)),
            dir: dir.to_path_buf(),
        })
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

    /// One pass at `now`: None when no hour is ready and nothing came late. `not_done` is the backfill days still to
    /// read; `stop` is asked as it goes (a pass given up writes nothing).
    pub fn pass(
        &self,
        store: &Store,
        now: i64,
        not_done: &HashSet<i64>,
        stop: &dyn Fn() -> bool,
    ) -> Result<Option<Found>> {
        let started = Instant::now();
        let mut state = self.state.clone();
        let segs = store.segments();
        let open_days = store.open_days();
        for (_, day, n) in &segs {
            if *n > 0 {
                state.seen.entry(*day).or_insert(now);
            }
        }
        for d in &open_days {
            state.seen.entry(*d).or_insert(now);
        }
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
            (Some(&a), Some(&b)) => (a * HOUR - MARGIN, (b + 1) * HOUR + MARGIN - 1),
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
        for late_round in [false, true] {
            for g in store.group_names() {
                let gid = group_id(&g);
                group_names.insert(gid, g.clone());
                for &d in &read_days {
                    let (map, entries) = store.day_blocks(&g, d);
                    let Some(map) = map else { continue };
                    let old = state.counts.get(&(g.clone(), d)).copied().unwrap_or(0) as usize;
                    if !late_round {
                        new_counts.push(((g.clone(), d), entries.len() as u32));
                    }
                    for e in &entries {
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
                        let view = NxvBlockView::from_mmap_slice(&map[e.offset as usize..])?;
                        // rows outside the hours read are passed over before their strings are decoded (late blocks are
                        // read whole)
                        let cur = FieldCursor::new(view)?;
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
                                (wall, bytes, lines, art) = (r.wall, r.bytes.unwrap_or(0), r.lines.unwrap_or(0), r.art);
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
                            if !seen_ids.insert(nntp_xover_engine::msgid_index::msgid_hash(msgid)) {
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
                            let at = ids.len() as u32;
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
            let mut rows: &Vec<usize> = rows;
            let mut new_rows: Vec<usize> = Vec::new();
            let mut again = false;
            if let Some(pr) = links.places.get(poster) {
                // a file already placed: its new articles (late, or of the hours linked now) are more of its parts
                let new: Vec<usize> =
                    rows.iter().copied().filter(|&i| arts[i].late || new_hour(arts[i].posted)).collect();
                if new.is_empty() {
                    continue;
                }
                if pr.place.oldest < now - KEEP_SECS {
                    // its first articles left the window long ago: one read again couldnt be told from a new one
                    continue;
                }
                // posted well after its upload ended: the poster uploading again under the same name, a new upload of
                // its own made of the new articles only
                let upload_last = links.uploads.get(&pr.place.release).map(|u| u.seed.last);
                let first_new = new.iter().map(|&i| arts[i].posted).min().unwrap_or(i64::MAX);
                if upload_last.is_none_or(|last| first_new > last + JOIN_GAP) {
                    stats.posted_again += 1;
                    new_rows = new;
                    rows = &new_rows;
                    again = true;
                }
            }
            if let (Some(pr), false) = (links.places.get(poster), again) {
                let new: Vec<usize> =
                    rows.iter().copied().filter(|&i| arts[i].late || new_hour(arts[i].posted)).collect();
                stats.late_files += 1;
                let mut file_rows = to_rows(&arts, &new, &msgid_of, now);
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
                    for &i in &new {
                        seed.groups.insert(arts[i].grp);
                        seed.first = seed.first.min(arts[i].posted);
                        seed.last = seed.last.max(arts[i].posted);
                    }
                    grown.insert(j);
                }
                continue;
            }
            let mut f = FileSum {
                poster: poster.clone(),
                n: 0,
                first: i64::MAX,
                last: i64::MIN,
                added: 0,
                groups: BTreeSet::new(),
            };
            for &i in rows {
                f.n += 1;
                f.first = f.first.min(arts[i].posted);
                f.last = f.last.max(arts[i].posted);
                f.groups.insert(arts[i].grp);
            }
            fresh.push(f);
            fresh_rows.push(rows.clone());
            again_files.push(again);
        }
        stats.files = fresh.len() as u64;
        let seeds: Vec<Seed> = saved.iter().map(|(_, u)| u.seed.clone()).collect();
        let chains = crate::xref::chain(&fresh, &seeds);
        let linked_or_span = |h: i64| state.linked.contains(&h) || span_set.contains(&h);
        let mut widened: HashSet<usize> = HashSet::new();
        for (k, (seed, files)) in chains.into_iter().enumerate() {
            if k % 4096 == 0 && stop() {
                return Ok(None);
            }
            let first = files.iter().map(|&i| fresh[i].first).min().unwrap_or(0);
            let last = files.iter().map(|&i| fresh[i].last).max().unwrap_or(0);
            // every hour it spans read, and something in it new: else it waits, or was seen before
            if !(hour_of(first)..=hour_of(last)).all(linked_or_span) {
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
        if let Some(&oldest) = days.first() {
            state.forget_before(oldest);
        }
        lines.retain(|l| l.trim_end_matches('\n').split('\t').all(field_ok));
        stats.secs = started.elapsed().as_secs_f64();
        Ok(Some(Found { releases: crate::xref::finish_releases(out), lines, state, hours: span, stats }))
    }

    /// Notes what `found`'s pass did, once its releases are saved: the links appended, then the state rewritten.
    pub fn ack(&mut self, found: Found) -> Result<()> {
        self.links.lock().unwrap_or_else(|e| e.into_inner()).append(&found.lines)?;
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
        if let Err(e) = l.append(&[line]) {
            eprintln!("header store linker: couldnt note guesses tried for {release}: {e:#}");
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
fn not_done_days() -> Result<HashSet<i64>> {
    let main = rusqlite::Connection::open_with_flags(
        crate::paths::database(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    main.busy_timeout(Duration::from_secs(30))?;
    let days = main
        .prepare("select distinct day from backfill_chunks where state != 2")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(days)
}

/// Runs the linker in the indexer until `stopping`: a pass, its releases saved, then noted; again at once while
/// passes find work, else every `PASS_EVERY`.
pub async fn run(db: crate::indexer::Db, store: Arc<Store>, stopping: impl Fn() -> bool) {
    let now = chrono::Utc::now().timestamp();
    let linker = match Linker::open(&store, now) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("header store linker: couldnt open its state: {e:#}; not linking");
            return;
        }
    };
    let _ = ACTIVE.set(Mutex::new(Some(Arc::clone(&linker.links))));
    let linker = Arc::new(Mutex::new(linker));
    while !stopping() {
        let (l, s) = (Arc::clone(&linker), Arc::clone(&store));
        let found = tokio::task::spawn_blocking(move || -> Result<Option<Found>> {
            let not_done = not_done_days()?;
            let lk = l.lock().unwrap_or_else(|e| e.into_inner());
            let stop = || crate::frozen::STOPPING.load(std::sync::atomic::Ordering::Relaxed);
            let found = lk.pass(&s, chrono::Utc::now().timestamp(), &not_done, &stop)?;
            let mut days: BTreeSet<i64> = s.segments().into_iter().map(|x| x.1).collect();
            days.extend(s.open_days());
            if let Ok(mut u) = UNLINKED.lock() {
                *u = lk.unlinked_days(&days);
            }
            Ok(found)
        })
        .await;
        let found = match found {
            Ok(Ok(Some(f))) => f,
            Ok(Ok(None)) => {
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
                if count > 0 || st.late_files > 0 {
                    println!(
                        "header store linker: saved {count} releases ({} uploads, {} files late), {articles} articles; {} blocks, {} rows in {:.0}s",
                        st.uploads, st.late_files, st.blocks, st.rows, st.secs
                    );
                }
            }
            Ok(Err(e)) => eprintln!("header store linker: couldnt note the pass: {e:#}"),
            Err(e) => eprintln!("header store linker: noting the pass failed: {e}"),
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
