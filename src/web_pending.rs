//! The release page's Processing panel: each step of the processing stage, whether it has run on the release, and
//! for the ones still to come about when, from where the release stands in its shard's line and how fast that shard's
//! lanes went over the last minutes (the indexer's `stats.json`). A lazy fragment: it reads one shard, and every
//! count of what is ahead stops at `AHEAD_CAP`.

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::{Arc, OnceLock};

use rusqlite::{Connection, OptionalExtension, ToSql};
use serde_json::Value;

use crate::api::{ApiResponse, escape};
use crate::postprocess::engine::{NET_BATCH, NET_CRASH_RETRY, STEP_DEADLINE, claim_shares, queue_shares};
use crate::postprocess::step::{Needs, ReleaseInfo, Step};
use crate::store;
use crate::web::{html, tag, thousands};

/// The most rows counted ahead of a release in one line. Past it the wait is only a lower bound.
const AHEAD_CAP: i64 = 500_000;
/// stats.json older than this: the indexer isn't running, and nothing moves.
const STALE_SECS: f64 = 120.0;
/// Rates over less than this aren't trusted yet.
const MIN_WINDOW_SECS: i64 = 60;

/// A step, as the page shows it.
struct StepMeta {
    id: u8,
    name: &'static str,
    rank: u16,
    /// runs in the net lane (bodies or an outside lookup)
    net: bool,
    step: Arc<dyn Step>,
}

impl StepMeta {
    /// Whether a database step would run on the release at its next visit. A net step isn't asked: some of theirs
    /// go by the indexer's own config and caches, and `pp_state.net_left` says which are left anyway.
    fn applies(&self, release: Option<&ReleaseInfo>) -> bool {
        self.net || release.is_none_or(|r| self.step.applies(r))
    }
}

trait ViewStep {
    fn view_id(&self) -> u8;
    fn view_name(&self) -> &'static str;
    fn view_net(&self) -> bool;
}
impl ViewStep for StepMeta {
    fn view_id(&self) -> u8 { self.id }
    fn view_name(&self) -> &'static str { self.name }
    fn view_net(&self) -> bool { self.net }
}
impl ViewStep for crate::postprocess::steps::Descriptor {
    fn view_id(&self) -> u8 { self.id.0 }
    fn view_name(&self) -> &'static str { self.name }
    fn view_net(&self) -> bool { self.needs != Needs::Database }
}

/// A checked live loader derives these predicates from the same pinned PG
/// facts/config as the stage. They never call Media::shared/wants/looked_up.
pub(crate) struct Applicability {
    pub(crate) id: u8,
    pub(crate) applies: bool,
    pub(crate) skipped: Option<&'static str>,
}

/// The only cache-dependent reasons in the current step catalog. A checked
/// loader derives title presence from the actual category/options/name target,
/// and reads kept lookup/IMDb state in PG. This DTO is not read authority.
pub(crate) enum MediaRuleFacts {
    NoRelease,
    NoTitle,
    Tv { looked_up: bool },
    Movie { looked_up: bool, imdb_known: bool },
}

pub(crate) fn applicability_from_media(facts: MediaRuleFacts) -> Vec<Applicability> {
    // Every registered DB step currently inherits applies=true. Net steps
    // short-circuit it in StepMeta. The catalog oracle guards that metadata;
    // native semantic qualification must also cover future predicate changes.
    crate::postprocess::steps::catalog().iter()
        .filter(|step| step.id != crate::probe::body_crc::BODY_CRC_STEP)
        .map(|step| {
            let skipped = match (step.id.0, &facts) {
                (33, MediaRuleFacts::NoTitle) => Some("no movie or TV title in its name to look up"),
                (33, MediaRuleFacts::Tv { looked_up: true } | MediaRuleFacts::Movie { looked_up: true, .. }) =>
                    Some("its title's IDs are already known, or it was looked up lately"),
                (34, MediaRuleFacts::NoTitle) => Some("no movie title in its name to find an IMDb id for"),
                (34, MediaRuleFacts::Tv { .. }) => Some("an episode: only a movie's NFO is read for its IMDb id"),
                (34, MediaRuleFacts::Movie { imdb_known: true, .. }) => Some("its title's IMDb id is already known"),
                _ => None,
            };
            Applicability { id: step.id.0, applies: true, skipped }
        }).collect()
}

#[derive(Clone, Copy)]
pub(crate) enum ProcessingScope { Shard(usize), Native }
impl ProcessingScope {
    fn label(self) -> String {
        match self { Self::Shard(shard) => format!("shard {shard}"), Self::Native => "the native stage".into() }
    }
}

fn step_list() -> &'static [StepMeta] {
    static STEPS: OnceLock<Vec<StepMeta>> = OnceLock::new();
    STEPS.get_or_init(|| {
        let mut steps: Vec<StepMeta> = crate::postprocess::steps::all()
            .into_iter()
            // a one-off backfill, run only where a seed asked it: not one of the steps a release waits for
            .filter(|s| s.id() != crate::probe::body_crc::BODY_CRC_STEP)
            .map(|s| StepMeta {
                id: s.id().0,
                name: s.name(),
                rank: s.rank(),
                net: s.needs() != Needs::Database,
                step: s,
            })
            .collect();
        steps.sort_by_key(|s| s.rank);
        steps
    })
}

/// What each step is for, in words.
pub(crate) fn label(name: &str) -> &str {
    match name {
        "archive_par2" => "PAR2 read",
        "archive_list" => "Archive listing",
        "upload_link" => "PAR2 upload link",
        "probe" => "Probe (tracks, codecs)",
        "opensubtitles" => "OpenSubtitles match",
        "stills" => "Stills",
        "naming" => "Naming",
        "nfo" => "Name from the NFO",
        "sidecar" => "Sidecar text",
        "sidecar_image" => "Sidecar cover",
        "images" => "Pictures",
        "predb_match" => "PreDB match",
        "category" => "Category",
        "media_parse" => "Title parse",
        "media_lookup" => "Media lookup",
        "media_nfo" => "IDs from the NFO",
        "opensubtitles_imdb" => "IMDb from OpenSubtitles",
        "keys" => "Duplicate keys",
        "probe_body_crc" => "Body CRC from the server",
        "audio_print" => "Audio fingerprint",
        other => other,
    }
}

const fn bit(id: u8) -> i64 {
    1 << id
}

/// A release's rows in the processing stage's tables.
#[derive(Clone, Debug, Default)]
pub(crate) struct State {
    /// as the steps see it; None: the release is gone
    pub(crate) release: Option<ReleaseInfo>,
    pub(crate) has_state: bool,
    pub(crate) done: i64,
    pub(crate) failed: i64,
    pub(crate) net_wanted: bool,
    /// the net steps left; None on rows written before it was kept
    pub(crate) net_left: Option<i64>,
    pub(crate) net_next_at: i64,
    pub(crate) net_attempts: i64,
    pub(crate) net_transient: i64,
    pub(crate) db_attempts: i64,
    pub(crate) net_rank: Option<i64>,
    pub(crate) last_error: Option<String>,
    /// its queue row's time
    pub(crate) due_at: Option<i64>,
    /// the first walk that will get to it, in the order walks run
    pub(crate) walk: Option<WalkCover>,
    /// the walks that will get to it and run steps again, done or not, in the order they run: (those steps,
    /// whether it's the walk in progress)
    pub(crate) redo_walks: Vec<(i64, bool)>,
}

/// A walk that will visit a release (see `engine::walk_tick`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct WalkCover {
    /// it goes down the shard's ids from here
    pub(crate) before: i64,
    /// only this group's releases
    pub(crate) group: Option<String>,
    /// it's the walk in progress: the others wait for it
    pub(crate) active: bool,
}

fn read_state(conn: &Connection, s: usize, id: i64) -> rusqlite::Result<State> {
    let mut state = conn
        .query_row(
            &format!(
                "select done, failed, coalesce(net_wanted, 0), net_left, net_next_at, net_attempts,
                        coalesce(net_transient, 0), db_attempts, net_rank, last_error
                 from s{s}.pp_state where release_id = ?"
            ),
            [id],
            |r| {
                Ok(State {
                    has_state: true,
                    done: r.get(0)?,
                    failed: r.get(1)?,
                    net_wanted: r.get::<_, i64>(2)? == 1,
                    net_left: r.get(3)?,
                    net_next_at: r.get(4)?,
                    net_attempts: r.get(5)?,
                    net_transient: r.get(6)?,
                    db_attempts: r.get(7)?,
                    net_rank: r.get(8)?,
                    last_error: r.get(9)?,
                    ..Default::default()
                })
            },
        )
        .optional()?
        .unwrap_or_default();
    state.release = conn
        .query_row(
            &format!(
                "select name, display_name, group_name, poster, posted_date, coalesce(size, 0), coalesce(complete, 0),
                        coalesce(parts, 0), file_total, coalesce(is_obfuscated, 0), name_source,
                        (select kind from s{s}.spots where release_id = ?1)
                 from s{s}.releases where id = ?1"
            ),
            [id],
            |r| {
                Ok(ReleaseInfo {
                    id,
                    shard: s,
                    name: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    display_name: r.get(1)?,
                    group: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    poster: r.get(3)?,
                    posted_date: r.get(4)?,
                    size: r.get(5)?,
                    complete: r.get::<_, i64>(6)? != 0,
                    parts: r.get(7)?,
                    file_total: r.get(8)?,
                    is_obfuscated: r.get::<_, i64>(9)? != 0,
                    name_source: r.get(10)?,
                    spot_kind: crate::spotnet::Kind::parse(r.get::<_, Option<String>>(11)?.as_deref()),
                })
            },
        )
        .optional()?;
    state.due_at = conn
        .query_row(&format!("select due_at from s{s}.pp_queue where release_id = ?"), [id], |r| r.get(0))
        .optional()?;
    // the newest walk goes first, then the next newest (see `engine::walk_tick`). There are a few at most
    let walks: Vec<(i64, i64, Option<String>)> = conn
        .prepare(&format!("select before_id, redo_mask, group_name from s{s}.pp_walks order by id desc"))?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let group = state.release.as_ref().map(|r| r.group.as_str());
    for (at, (before, redo, walk_group)) in walks.into_iter().enumerate() {
        if before <= id || walk_group.as_deref().is_some_and(|g| Some(g) != group) {
            continue;
        }
        if redo != 0 {
            state.redo_walks.push((redo, at == 0));
        }
        if state.walk.is_none() {
            state.walk = Some(WalkCover { before, group: walk_group, active: at == 0 });
        }
    }
    Ok(state)
}

/// How fast a shard's lanes went, per minute, and whether the indexer is running at all.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Pace {
    pub(crate) running: bool,
    /// the steps that are on; None when the indexer hasn't said
    pub(crate) enabled: Option<i64>,
    /// seconds the rates are over; under `MIN_WINDOW_SECS` they're not used
    pub(crate) window_secs: i64,
    /// releases the net lane ran a step of, and the ones it was then done with
    pub(crate) net_visits: f64,
    pub(crate) net_finished: f64,
    pub(crate) queue_taken: f64,
    pub(crate) walk_rows: f64,
}

impl Pace {
    fn measured(&self) -> bool {
        self.running && self.window_secs >= MIN_WINDOW_SECS
    }
}

fn pace_of(stats: &Value, shard: usize, now: f64) -> Pace {
    let p = &stats["postprocess"];
    let lanes = &p["lanes"];
    let rate = |key: &str| lanes[key].get(shard).and_then(Value::as_f64).unwrap_or(0.0);
    let at = stats["load"]["at"].as_f64().unwrap_or(0.0);
    Pace {
        running: at > 0.0 && now - at < STALE_SECS && p.is_object(),
        enabled: p["enabled"].as_i64(),
        window_secs: lanes["window_secs"].as_i64().unwrap_or(0),
        net_visits: rate("net_visits"),
        net_finished: rate("net_finished"),
        queue_taken: rate("queue_taken"),
        walk_rows: rate("walk_rows"),
    }
}

/// How long until a release's turn in a line, in minutes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Wait {
    /// about this long, with this many ahead of it in the order that gets to it first
    About(f64, i64),
    /// at least this long: more than `AHEAD_CAP` are ahead in every order
    AtLeast(f64),
    /// somewhere between: an order counted to the end gives the longer, one that stopped at `AHEAD_CAP` could still
    /// beat it; with how many are ahead in the one counted
    Between(f64, f64, i64),
    /// the lane hasn't gone fast enough lately to tell; how many are ahead in its main order, if counted
    Unknown(Option<(i64, bool)>),
    /// a walk that waits for the one in progress
    Later,
}

/// One of the orders a lane takes turns between: its share of the lane's batches, and a count of the rows ahead of
/// the release in it, up to a cap, saying whether it stopped there.
pub(crate) type Order<'a, E = rusqlite::Error> = (f64, &'a dyn Fn(i64) -> std::result::Result<(i64, bool), E>);

/// The release's wait in a lane that gets through `per_minute` releases of the shard and takes turns between
/// `orders`: its turn comes in whichever order reaches it first. The first order is the lane's main one, always
/// counted; the others only as far as could beat the best so far, soo a page counts little.
pub(crate) fn wait_in<E>(per_minute: Option<f64>, orders: &[Order<'_, E>]) -> std::result::Result<Wait, E> {
    let Some(rate) = per_minute.filter(|r| *r > 0.0) else {
        let main = match orders.first() {
            Some((_, count)) => Some(count(AHEAD_CAP)?),
            None => None,
        };
        return Ok(Wait::Unknown(main));
    };
    let mut best: Option<(f64, i64)> = None;
    let mut floor = f64::INFINITY;
    for (share, count) in orders {
        if *share <= 0.0 {
            continue;
        }
        let speed = share * rate;
        let cap = match best {
            Some((minutes, _)) => ((minutes * speed).ceil() as i64).min(AHEAD_CAP),
            None => AHEAD_CAP,
        };
        let (ahead, stopped) = count(cap)?;
        let minutes = ahead as f64 / speed;
        if !stopped {
            if best.is_none_or(|(b, _)| minutes < b) {
                best = Some((minutes, ahead));
            }
        } else if cap == AHEAD_CAP {
            floor = floor.min(minutes);
        }
    }
    Ok(match best {
        Some((minutes, ahead)) if floor < minutes => Wait::Between(floor, minutes, ahead),
        Some((minutes, ahead)) => Wait::About(minutes, ahead),
        None if floor.is_finite() => Wait::AtLeast(floor),
        None => Wait::Unknown(None),
    })
}

/// Rows of one shard a query finds, up to `cap`: (count, whether it stopped at the cap).
fn capped(conn: &Connection, from_where: &str, args: &[&dyn ToSql], cap: i64) -> rusqlite::Result<(i64, bool)> {
    let n: i64 =
        conn.query_row(&format!("select count(*) from (select 1 from {from_where} limit {})", cap + 1), args, |r| {
            r.get(0)
        })?;
    Ok((n.min(cap), n > cap))
}

/// The net lane's wait for this release: by rank (biggest video first, newest first within a rank), the newest, or
/// the oldest, as `engine::claim_order` takes turns. A release out of the line for now (a retry) is counted ahead
/// in the rank order all the same, soo that order errs long.
fn net_wait(
    conn: &Connection,
    s: usize,
    id: i64,
    rank: Option<i64>,
    rate: Option<f64>,
    now: i64,
) -> rusqlite::Result<Wait> {
    let (by_rank, newest, oldest) = claim_shares();
    let ranked = |cap: i64| -> rusqlite::Result<(i64, bool)> {
        let table = format!("s{s}.pp_state indexed by pp_state_net_rank where net_wanted = 1");
        let (higher, more) = match rank {
            Some(r) => capped(conn, &format!("{table} and net_rank > ?1"), &[&r], cap)?,
            None => capped(conn, &format!("{table} and net_rank is not null"), &[], cap)?,
        };
        if more {
            return Ok((higher, true));
        }
        let (same, more) = match rank {
            Some(r) => {
                capped(conn, &format!("{table} and net_rank = ?1 and release_id > ?2"), &[&r, &id], cap - higher)?
            }
            None => capped(conn, &format!("{table} and net_rank is null and release_id > ?1"), &[&id], cap - higher)?,
        };
        Ok((higher + same, more))
    };
    let by_id = |op: &'static str| {
        move |cap: i64| {
            capped(
                conn,
                &format!(
                    "s{s}.pp_state indexed by pp_state_net where net_wanted = 1 and release_id {op} ?1 and net_next_at <= ?2"
                ),
                &[&id, &now],
                cap,
            )
        }
    };
    let (newer, older) = (by_id(">"), by_id("<"));
    wait_in(rate, &[(by_rank, &ranked), (newest, &newer), (oldest, &older)])
}

/// The database lane's wait for a queue row due now: the latest due first, or the longest waiting, as
/// `engine::queue_order` takes turns.
fn queue_wait(conn: &Connection, s: usize, id: i64, due: i64, rate: Option<f64>, now: i64) -> rusqlite::Result<Wait> {
    let (latest, longest) = queue_shares();
    // rows due at the same time go by release id, as `pp_queue_due` holds them: down it for the latest due, up it
    // for the longest waiting
    let ahead = |strictly: &str, tied: &str, args: &[&dyn ToSql], cap: i64| -> rusqlite::Result<(i64, bool)> {
        let (n, more) = capped(conn, &format!("s{s}.pp_queue where {strictly}"), args, cap)?;
        if more {
            return Ok((n, true));
        }
        let (t, more) =
            capped(conn, &format!("s{s}.pp_queue where due_at = ?1 and release_id {tied} ?2"), &[&due, &id], cap - n)?;
        Ok((n + t, more))
    };
    let later = |cap| ahead("due_at > ?1 and due_at <= ?2", ">", &[&due, &now], cap);
    let earlier = |cap| ahead("due_at < ?1", "<", &[&due], cap);
    wait_in(rate, &[(latest, &later), (longest, &earlier)])
}

/// A walk's wait: it goes down the shard's ids from `before`, through its group's releases when it has one.
fn walk_wait(conn: &Connection, s: usize, id: i64, walk: &WalkCover, rate: Option<f64>) -> rusqlite::Result<Wait> {
    if !walk.active {
        return Ok(Wait::Later);
    }
    let rows = |cap| match &walk.group {
        Some(g) => capped(
            conn,
            &format!("s{s}.releases where group_name = ?3 and id > ?1 and id < ?2"),
            &[&id, &walk.before, g],
            cap,
        ),
        None => capped(conn, &format!("s{s}.releases where id > ?1 and id < ?2"), &[&id, &walk.before], cap),
    };
    wait_in(rate, &[(1.0, &rows)])
}

/// Where one step stands.
#[derive(Clone, Debug, PartialEq)]
enum Standing {
    Done,
    Failed,
    Off,
    /// not run and not queued: it doesn't apply, or waits for more of the release's files. The step's own reason,
    /// when it gives one
    Skipped(Option<&'static str>),
    /// out of line until then: a retry, settling
    Until(i64, String),
    /// a run of it began moments ago; if cut short, it's tried again then
    Running(i64),
    /// a net step after the one running now: the lane takes a release's steps one claim at a time
    AfterRunning,
    /// in line; `next`: the first of the release's steps there
    Queued {
        wait: Wait,
        after: f64,
        next: bool,
        lane: &'static str,
    },
}

/// What the stage's lanes have in store for a release.
#[derive(Debug, Default)]
pub(crate) struct Waits {
    pub(crate) net: Option<Wait>,
    pub(crate) queue: Option<Wait>,
    pub(crate) walk: Option<Wait>,
}

/// Each step's standing, by rank.
fn standings(state: &State, pace: &Pace, waits: &Waits, now: i64) -> Vec<(&'static StepMeta, Standing)> {
    standings_using(step_list(), state, pace, waits, now,
        |step| step.applies(state.release.as_ref()),
        |step| state.release.as_ref().and_then(|release| step.step.skip_reason(release)))
}

fn standings_using<'a, S: ViewStep>(steps: &'a [S], state: &State, pace: &Pace, waits: &Waits, now: i64,
    applies: impl Fn(&S) -> bool, skipped: impl Fn(&S) -> Option<&'static str>) -> Vec<(&'a S, Standing)> {
    let enabled = pace.enabled.unwrap_or(-1);
    // a net step after the release's first goes a claim later: one claim of its shard by rank takes NET_BATCH releases
    let per_step = if pace.measured() && pace.net_visits > 0.0 {
        NET_BATCH as f64 / (claim_shares().0 * pace.net_visits)
    } else {
        0.0
    };
    let mut net_ahead = 0.0;
    let mut running_seen = false;
    let mut out = Vec::new();
    for step in steps {
        let b = bit(step.view_id());
        let why = || Standing::Skipped(skipped(step));
        // a reprocess asked for it again, done or not: the walk in progress gets there in its time, another after it
        let redo = state.redo_walks.iter().find(|(redo, _)| redo & b != 0).filter(|_| enabled & b != 0);
        let standing = if let Some((_, active)) = redo {
            let wait = if *active { waits.walk.unwrap_or(Wait::Unknown(None)) } else { Wait::Later };
            Standing::Queued { wait, after: 0.0, next: !step.view_net(), lane: "walk" }
        } else if state.done & b != 0 {
            Standing::Done
        } else if state.failed & b != 0 {
            Standing::Failed
        } else if enabled & b == 0 {
            Standing::Off
        } else if !applies(step) {
            why()
        } else if step.view_net() {
            let left = state.net_wanted && state.net_left.is_none_or(|m| m & b != 0);
            if !left {
                // a release with no row yet gets its net steps once a visit has looked at it
                match (&waits.queue, &waits.walk) {
                    _ if !state.has_state && state.due_at.is_some_and(|due| due > now) => Standing::Until(
                        state.due_at.unwrap_or(now),
                        "the post settling, then its first look decides which of these it needs".into(),
                    ),
                    (Some(wait), _) if !state.has_state => {
                        Standing::Queued { wait: *wait, after: 0.0, next: false, lane: "database" }
                    }
                    (None, Some(wait)) if !state.has_state => {
                        Standing::Queued { wait: *wait, after: 0.0, next: false, lane: "walk" }
                    }
                    _ => why(),
                }
            } else if state.net_next_at > now && began_moments_ago(state, now) {
                // the run is the release's first net step left; the rest come after it
                if running_seen {
                    Standing::AfterRunning
                } else {
                    running_seen = true;
                    Standing::Running(state.net_next_at)
                }
            } else if state.net_next_at > now {
                let why = if state.net_transient > 0 {
                    format!("servers didn't answer ({} in a row)", state.net_transient)
                } else if state.net_attempts > 0 {
                    format!("a retry ({} attempts so far), or a run that's still going", state.net_attempts)
                } else {
                    "waiting for more of its files or a run in progress".to_string()
                };
                Standing::Until(state.net_next_at, why)
            } else {
                let after = net_ahead;
                net_ahead += per_step;
                Standing::Queued {
                    wait: waits.net.unwrap_or(Wait::Unknown(None)),
                    after,
                    next: after == 0.0,
                    lane: "net",
                }
            }
        } else {
            match (state.due_at, &waits.queue, &waits.walk) {
                (Some(due), _, _) if due > now => {
                    let why = if state.db_attempts > 0 {
                        format!("retry after {} failed attempts", state.db_attempts)
                    } else {
                        "waiting for the post to settle".to_string()
                    };
                    Standing::Until(due, why)
                }
                (Some(_), Some(wait), _) => Standing::Queued { wait: *wait, after: 0.0, next: true, lane: "database" },
                (None, _, Some(wait)) => Standing::Queued { wait: *wait, after: 0.0, next: true, lane: "walk" },
                _ => why(),
            }
        };
        out.push((step, standing));
    }
    out
}

/// Whether the net lane's `net_next_at` is the one a run sets as it begins (`engine::NET_CRASH_RETRY` on, for a run
/// the process dies in) rather than a retry's backoff. A first retry's backoff is about as long, soo this can be
/// wrong for a minute or two after a failure.
fn began_moments_ago(state: &State, now: i64) -> bool {
    let left = state.net_next_at - now;
    let deadline = STEP_DEADLINE.as_secs() as i64;
    state.net_attempts > 0 && left <= NET_CRASH_RETRY && left > NET_CRASH_RETRY - deadline - 60
}

/// Minutes in words: "4 min", "3 h", "2 days".
fn span(minutes: f64) -> String {
    if minutes < 1.0 {
        "under a minute".into()
    } else if minutes < 90.0 {
        format!("{:.0} min", minutes)
    } else if minutes < 48.0 * 60.0 {
        format!("{:.0} h", minutes / 60.0)
    } else {
        format!("{} days", thousands((minutes / 1440.0).round() as i64))
    }
}

fn clock(at: i64) -> String {
    chrono::DateTime::from_timestamp(at, 0).map_or(String::new(), |t| t.format("%Y-%m-%d %H:%M UTC").to_string())
}

fn render(id: i64, state: &State, pace: &Pace, rows: &[(&'static StepMeta, Standing)], now: i64) -> String {
    render_using(ProcessingScope::Shard(store::shard_of_id(id)), state, pace, rows, now)
}

fn when(wait: &Wait, after: f64, lane: &str, scope: ProcessingScope) -> String {
    let shard = scope.label();
    let line = match lane {
        "walk" => format!("rows the walk of {shard} still has to look at"),
        "database" => format!("ahead in {shard}'s queue"),
        _ => format!("ahead in {shard}'s net line"),
    };
    match wait {
        Wait::About(minutes, ahead) => {
            format!("in about {} · {} {line}", span(minutes + after), thousands(*ahead))
        }
        Wait::Between(soonest, latest, ahead) => format!(
            "in {} to {} · {} {line} in the order that's sure to get there",
            span(soonest + after),
            span(latest + after),
            thousands(*ahead)
        ),
        Wait::AtLeast(minutes) => {
            format!("in more than {} · over {} {line}", span(minutes + after), thousands(AHEAD_CAP))
        }
        Wait::Unknown(Some((ahead, stopped))) => {
            let over = if *stopped { "over " } else { "" };
            format!("{over}{} {line}; too little has gone through lately to tell when", thousands(*ahead))
        }
        Wait::Unknown(None) => "too little has gone through lately to tell when".into(),
        Wait::Later => format!("once the walk of {shard} in progress is done, by a later walk"),
    }
}

fn render_using<S: ViewStep>(scope: ProcessingScope, state: &State, pace: &Pace, rows: &[(&S, Standing)], now: i64) -> String {
    let pending: Vec<_> = rows
        .iter()
        .filter(|(_, s)| {
            matches!(s, Standing::Queued { .. } | Standing::Until(..) | Standing::Running(_) | Standing::AfterRunning)
        })
        .collect();
    let mut table = String::new();
    for (step, standing) in rows {
        let lane = if step.view_net() { "network" } else { "database" };
        let (state_tag, note) = match standing {
            Standing::Done => (tag("tag-ok", "DONE"), String::new()),
            Standing::Failed => (tag("tag-bad", "FAILED"), state.last_error.as_deref().map(escape).unwrap_or_default()),
            Standing::Off => (tag("", "OFF"), "turned off in the settings".into()),
            Standing::Skipped(why) => {
                (tag("", "SKIPPED"), escape(why.unwrap_or("doesn't apply, or waits for more of its files")))
            }
            Standing::Until(at, why) => {
                (tag("tag-warn", "WAITING"), format!("not before {} · {}", escape(&clock(*at)), escape(why)))
            }
            Standing::Running(retry) => (
                tag("tag-ref", "RUNNING"),
                format!("probably running now · if it's cut short, tried again not before {}", escape(&clock(*retry))),
            ),
            Standing::AfterRunning => (tag("tag-warn", "QUEUED"), "after the step running now".into()),
            Standing::Queued { wait, after, next, lane } => {
                let chip = if *next { tag("tag-ref", "NEXT") } else { tag("tag-warn", "QUEUED") };
                let at = if pace.running {
                    when(wait, *after, lane, scope)
                } else {
                    "on hold: the indexer isn't running".into()
                };
                (chip, escape(&at))
            }
        };
        let _ = write!(
            table,
            "<tr><td>{}</td><td class=\"pending-lane\">{lane}</td><td class=\"pending-state\">{state_tag}</td><td>{note}</td></tr>",
            escape(label(step.view_name()))
        );
    }
    let summary = if pending.is_empty() {
        let done = rows.iter().filter(|(_, s)| *s == Standing::Done).count();
        if !state.has_state && state.due_at.is_none() && state.walk.is_none() {
            "Nothing is lined up for this release: the stage's rules passed it by (too small, a fragment, junk), or it \
             hasn't been looked at yet."
                .to_string()
        } else {
            format!("Nothing left to run: {done} steps done.")
        }
    } else {
        let names: Vec<&str> = pending.iter().map(|(s, _)| label(s.view_name())).collect();
        let soonest = pending
            .iter()
            .filter_map(|(_, s)| match s {
                Standing::Queued { wait: Wait::About(m, _) | Wait::Between(m, ..), after, .. } => Some(m + after),
                Standing::Until(at, _) => Some((at - now) as f64 / 60.0),
                Standing::Running(_) => Some(0.0),
                _ => None,
            })
            .fold(f64::INFINITY, f64::min);
        let next = if soonest.is_finite() && pace.running {
            format!(" The next one runs in about {}.", span(soonest))
        } else {
            String::new()
        };
        format!("{} of {} steps still to run: {}.{next}", pending.len(), rows.len(), names.join(", "))
    };
    let guess = if state.net_wanted && state.net_left.is_none() {
        " The network steps listed as queued are the ones not done yet: this release was last looked at before Atlas \
         kept which of them it still needs."
    } else {
        ""
    };
    let rates = if pace.measured() {
        format!(
            "{} lately: {:.0} releases a minute through the net line ({:.0} steps), {:.0} queue rows a minute. \
             Times are at that pace, if nothing new goes ahead.",
            match scope { ProcessingScope::Shard(shard) => format!("Shard {shard}"), ProcessingScope::Native => "Native stage".into() },
            pace.net_finished, pace.net_visits, pace.queue_taken
        )
    } else if pace.running {
        "The indexer started a moment ago: times show once it has run a minute.".into()
    } else {
        "The indexer isn't running, soo nothing moves until it's back.".into()
    };
    format!(
        "<section id=\"pending\"><h2>Processing</h2><p>{}</p><div class=\"table-wrap\"><table><thead><tr><th>Step</th>\
         <th class=\"pending-lane\">Lane</th><th>State</th><th>When</th></tr></thead><tbody>{table}</tbody></table></div>\
         <p class=\"note\">{}{}</p></section>",
        escape(&summary),
        escape(&rates),
        escape(guess)
    )
}

/// A release's standing in the processing stage: its rows, its shard's pace, and the lanes' waits.
fn read_all(id: i64) -> rusqlite::Result<(State, Pace, Waits, i64)> {
    let s = store::shard_of_id(id);
    let now = chrono::Utc::now();
    let stats: Value = std::fs::read(crate::paths::stats_file())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    let pace = pace_of(&stats, s, now.timestamp_millis() as f64 / 1000.0);
    let now = now.timestamp();
    let (state, waits) = crate::db::with_reader(|conn| {
        let state = read_state(conn, s, id)?;
        let measured = pace.measured();
        let rate = |r: f64| measured.then_some(r);
        let mut waits = Waits::default();
        if state.net_wanted && state.net_next_at <= now {
            waits.net = Some(net_wait(conn, s, id, state.net_rank, rate(pace.net_finished), now)?);
        }
        if let Some(due) = state.due_at.filter(|d| *d <= now) {
            waits.queue = Some(queue_wait(conn, s, id, due, rate(pace.queue_taken), now)?);
        }
        if let Some(walk) = state.walk.as_ref().filter(|_| state.due_at.is_none() || !state.redo_walks.is_empty()) {
            waits.walk = Some(walk_wait(conn, s, id, walk, rate(pace.walk_rows))?);
        }
        Ok((state, waits))
    })?;
    Ok((state, pace, waits, now))
}

/// `/ui/pending?id=`: the Processing panel of a release.
pub(crate) fn fragment(args: &HashMap<String, String>) -> ApiResponse {
    let id = args.get("id").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0);
    match read_all(id) {
        Ok((state, pace, waits, now)) => {
            html(200, render_from_facts(id, &state, &pace, &waits, now))
        }
        Err(e) => {
            eprintln!("web pending {id} failed: {e}");
            html(200, String::new())
        }
    }
}

fn render_from_facts(id: i64, state: &State, pace: &Pace, waits: &Waits, now: i64) -> String {
    let rows = standings(state, pace, waits, now);
    render(id, state, pace, &rows, now)
}

/// Native processing shares the actual standing/order/ETA renderer, without
/// constructing legacy Step services. All rule predicates/reasons must have
/// been derived by the checked PG loader under the same source watermark;
/// missing rows cannot become Default applicability or an empty panel.
pub(crate) fn native_from_facts(scope: ProcessingScope, state: &State, pace: &Pace, waits: &Waits,
    rules: &[Applicability], now: i64) -> anyhow::Result<String> {
    use anyhow::ensure;
    let steps: Vec<_> = crate::postprocess::steps::catalog().iter()
        .filter(|step| step.id != crate::probe::body_crc::BODY_CRC_STEP).copied().collect();
    ensure!(rules.len() == steps.len() && rules.iter().zip(&steps).all(|(rule, step)| rule.id == step.id.0),
        "native processing applicability is incomplete or reordered");
    ensure!(pace.window_secs >= 0 && [pace.net_visits, pace.net_finished, pace.queue_taken, pace.walk_rows]
        .into_iter().all(|rate| rate.is_finite() && rate >= 0.0), "native processing pace is invalid");
    let rule = |step: &crate::postprocess::steps::Descriptor| &rules[steps.iter().position(|other| other.id == step.id).unwrap()];
    let rows = standings_using(&steps, state, pace, waits, now,
        |step| step.view_net() || state.release.is_none() || rule(step).applies,
        |step| rule(step).skipped);
    Ok(render_using(scope, state, pace, &rows, now))
}

/// When step `name` runs next on release `id`, in a few words for a line of the release page: "in about 4 min
/// (around 21:40 UTC)". None when it isn't lined up (done, or it doesn't apply).
pub(crate) fn step_eta(id: i64, name: &str) -> Option<String> {
    let (state, pace, waits, now) = read_all(id).map_err(|e| eprintln!("web pending {id} failed: {e}")).ok()?;
    let rows = standings(&state, &pace, &waits, now);
    let (_, standing) = rows.into_iter().find(|(step, _)| step.name == name)?;
    eta_words(&standing, &pace, now)
}

fn eta_words(standing: &Standing, pace: &Pace, now: i64) -> Option<String> {
    let around = |minutes: f64| {
        let at = now + (minutes * 60.0).round() as i64;
        chrono::DateTime::from_timestamp(at, 0).map_or(String::new(), |t| t.format(" (around %H:%M UTC)").to_string())
    };
    Some(match standing {
        Standing::Done | Standing::Failed | Standing::Skipped(_) => return None,
        Standing::Off => "the Pictures step is turned off in the settings".into(),
        _ if !pace.running => "on hold: the indexer isn't running".into(),
        Standing::Running(_) => "being read now".into(),
        Standing::AfterRunning => "next, after the step running now".into(),
        Standing::Until(at, _) => format!("not before {}", clock(*at)),
        Standing::Queued { wait, after, .. } => match wait {
            Wait::About(minutes, _) if minutes + after < 1.0 => "in under a minute".into(),
            Wait::About(minutes, _) => format!("in about {}{}", span(minutes + after), around(minutes + after)),
            Wait::Between(soonest, latest, _) => format!("in {} to {}", span(soonest + after), span(latest + after)),
            Wait::AtLeast(minutes) => format!("in more than {}", span(minutes + after)),
            Wait::Unknown(_) => "soon: too little has gone through lately to say when".into(),
            Wait::Later => "after the walk in progress".into(),
        },
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use rusqlite::params;

    use super::*;

    #[test]
    fn native_processing_keeps_standings_and_requires_every_rule_fact() {
        let mut state = State { has_state: true, done: bit(8), failed: bit(16), net_wanted: true,
            net_left: Some(bit(4)), last_error: Some("<failed>&".into()), ..Default::default() };
        let rules: Vec<_> = crate::postprocess::steps::catalog().iter()
            .filter(|step| step.id != crate::probe::body_crc::BODY_CRC_STEP)
            .map(|step| Applicability { id: step.id.0, applies: true, skipped: None }).collect();
        let pace = Pace { running: true, enabled: Some(-1), window_secs: 60, net_visits: 120.0,
            net_finished: 30.0, queue_taken: 40.0, walk_rows: 10.0 };
        let waits = Waits { net: Some(Wait::About(2.0, 60)), ..Default::default() };
        let html = native_from_facts(ProcessingScope::Native, &state, &pace, &waits, &rules, 100).unwrap();
        for text in ["Processing", "DONE", "FAILED", "&lt;failed&gt;&amp;", "ahead in the native stage's net line", "Native stage lately"] {
            assert!(html.contains(text), "native actual standing {text}");
        }
        assert!(!html.contains("Shard ") && !html.contains("shard "));
        assert!(native_from_facts(ProcessingScope::Native, &state, &pace, &waits, &rules[1..], 100).is_err());
        state.redo_walks.push((bit(8), true));
        let queued = native_from_facts(ProcessingScope::Native, &state, &pace, &Waits { walk: Some(Wait::Later), ..Default::default() }, &rules, 100).unwrap();
        assert!(queued.contains("by a later walk"));
    }

    #[test]
    fn native_media_reasons_use_kept_facts_and_preserve_absence() {
        let why = |facts, id| applicability_from_media(facts).into_iter().find(|rule| rule.id == id).unwrap().skipped;
        assert_eq!(why(MediaRuleFacts::NoRelease, 33), None);
        assert_eq!(why(MediaRuleFacts::NoTitle, 33), Some("no movie or TV title in its name to look up"));
        assert_eq!(why(MediaRuleFacts::Tv { looked_up: false }, 33), None);
        assert_eq!(why(MediaRuleFacts::Tv { looked_up: true }, 33), Some("its title's IDs are already known, or it was looked up lately"));
        assert_eq!(why(MediaRuleFacts::Tv { looked_up: false }, 34), Some("an episode: only a movie's NFO is read for its IMDb id"));
        assert_eq!(why(MediaRuleFacts::Movie { looked_up: false, imdb_known: false }, 34), None);
        assert_eq!(why(MediaRuleFacts::Movie { looked_up: false, imdb_known: true }, 34), Some("its title's IMDb id is already known"));
        assert!(applicability_from_media(MediaRuleFacts::NoTitle).iter().all(|rule| rule.applies));
    }

    fn counted(n: i64) -> impl Fn(i64) -> rusqlite::Result<(i64, bool)> {
        move |cap| Ok((n.min(cap), n > cap))
    }

    #[test]
    fn a_steps_wait_in_a_few_words() {
        let pace = Pace { running: true, ..Pace::default() };
        let queued = Standing::Queued { wait: Wait::About(3.6, 12), after: 0.4, next: true, lane: "net" };
        assert_eq!(eta_words(&queued, &pace, 0).as_deref(), Some("in about 4 min (around 00:04 UTC)"));
        assert_eq!(eta_words(&Standing::Done, &pace, 0), None);
        let now = Standing::Queued { wait: Wait::About(0.0, 0), after: 0.0, next: true, lane: "net" };
        assert_eq!(eta_words(&now, &pace, 0).as_deref(), Some("in under a minute"));
        let stopped = Pace::default();
        assert_eq!(eta_words(&queued, &stopped, 0).as_deref(), Some("on hold: the indexer isn't running"));
        assert_eq!(
            eta_words(&Standing::Off, &stopped, 0).as_deref(),
            Some("the Pictures step is turned off in the settings")
        );
    }

    #[test]
    fn the_order_that_reaches_a_release_first_sets_its_wait() {
        // 600 ahead by rank at 3/4 of 100 a minute is 8 min; 50 ahead by age at 1/8 is 4 min
        let (rank, newest, oldest) = (counted(600), counted(10_000), counted(50));
        let wait = wait_in(Some(100.0), &[(0.75, &rank), (0.125, &newest), (0.125, &oldest)]).unwrap();
        assert_eq!(wait, Wait::About(4.0, 50));
    }

    #[test]
    fn later_orders_count_only_as_far_as_could_beat_the_best() {
        let asked = Cell::new(0);
        let rank = counted(75);
        let newest = |cap: i64| {
            asked.set(cap);
            Ok((cap, true))
        };
        let wait = wait_in(Some(100.0), &[(0.75, &rank), (0.125, &newest)]).unwrap();
        assert_eq!(wait, Wait::About(1.0, 75));
        assert_eq!(asked.get(), 13, "1 min at 12.5 a minute");
    }

    #[test]
    fn past_the_cap_everywhere_the_wait_is_a_floor() {
        let far = counted(AHEAD_CAP * 4);
        let wait = wait_in(Some(1000.0), &[(0.5, &far), (0.5, &far)]).unwrap();
        assert_eq!(wait, Wait::AtLeast(AHEAD_CAP as f64 / 500.0));
    }

    #[test]
    fn an_order_stopped_at_the_cap_keeps_the_wait_a_range() {
        // at 1,000 a minute: rank and newest stop at the cap (at least 667 and 4,000 min), oldest says 3,200 min
        let (rank, newest, oldest) = (counted(600_000), counted(600_000), counted(400_000));
        let wait = wait_in(Some(1000.0), &[(0.75, &rank), (0.125, &newest), (0.125, &oldest)]).unwrap();
        assert_eq!(wait, Wait::Between(AHEAD_CAP as f64 / 750.0, 3200.0, 400_000));
    }

    #[test]
    fn with_no_pace_it_says_how_many_are_ahead() {
        let rank = counted(42);
        assert_eq!(wait_in(None, &[(0.75, &rank)]).unwrap(), Wait::Unknown(Some((42, false))));
        assert_eq!(wait_in(Some(0.0), &[(0.75, &rank)]).unwrap(), Wait::Unknown(Some((42, false))));
    }

    fn step(name: &str) -> &'static StepMeta {
        step_list().iter().find(|s| s.name == name).unwrap()
    }

    fn pace() -> Pace {
        Pace {
            running: true,
            enabled: Some(-1),
            window_secs: 600,
            net_visits: 240.0,
            net_finished: 60.0,
            ..Default::default()
        }
    }

    fn standing_of<'a>(rows: &'a [(&'static StepMeta, Standing)], name: &str) -> &'a Standing {
        &rows.iter().find(|(s, _)| s.name == name).unwrap().1
    }

    #[test]
    fn each_step_shows_whether_it_ran_and_when_the_rest_will() {
        let (probe, list, naming, lookup) = (step("probe"), step("archive_list"), step("naming"), step("media_lookup"));
        let state = State {
            has_state: true,
            done: bit(list.id) | bit(naming.id),
            net_wanted: true,
            net_left: Some(bit(probe.id) | bit(lookup.id)),
            net_rank: Some(1),
            ..Default::default()
        };
        let waits = Waits { net: Some(Wait::About(120.0, 7200)), ..Default::default() };
        let rows = standings(&state, &pace(), &waits, 1_000);
        assert_eq!(*standing_of(&rows, "archive_list"), Standing::Done);
        assert_eq!(*standing_of(&rows, "naming"), Standing::Done);
        assert!(matches!(standing_of(&rows, "probe"), Standing::Queued { next: true, after, .. } if *after == 0.0));
        assert!(
            matches!(standing_of(&rows, "media_lookup"), Standing::Queued { next: false, after, .. } if *after > 0.0)
        );
        assert_eq!(*standing_of(&rows, "archive_par2"), Standing::Skipped(None), "not in net_left");
        assert_eq!(*standing_of(&rows, "predb_match"), Standing::Skipped(None), "no queue row, no walk");

        let html = render(8, &state, &pace(), &rows, 1_000);
        assert!(html.contains("2 of 19 steps still to run: Probe (tracks, codecs), Media lookup."), "{html}");
        assert!(html.contains("in about 2 h · 7,200 ahead in shard 0's net line"), "{html}");
    }

    #[test]
    fn rows_from_before_net_left_list_every_net_step_not_done() {
        let state = State { has_state: true, net_wanted: true, net_left: None, ..Default::default() };
        let rows = standings(&state, &pace(), &Waits::default(), 0);
        assert!(matches!(standing_of(&rows, "archive_par2"), Standing::Queued { next: true, .. }));
        assert!(matches!(standing_of(&rows, "probe"), Standing::Queued { next: false, .. }));
        assert!(render(8, &state, &pace(), &rows, 0).contains("last looked at before Atlas"));
    }

    #[test]
    fn a_retry_or_settling_waits_until_its_time_and_an_off_step_says_so() {
        let probe = step("probe");
        let state = State {
            has_state: true,
            net_wanted: true,
            net_left: Some(bit(probe.id)),
            net_next_at: 5_000,
            net_attempts: 2,
            due_at: Some(4_000),
            ..Default::default()
        };
        let pace = Pace { enabled: Some(!bit(step("stills").id)), ..pace() };
        let rows = standings(&state, &pace, &Waits::default(), 1_000);
        assert_eq!(
            *standing_of(&rows, "probe"),
            Standing::Until(5_000, "a retry (2 attempts so far), or a run that's still going".into())
        );
        assert_eq!(*standing_of(&rows, "naming"), Standing::Until(4_000, "waiting for the post to settle".into()));
        assert_eq!(*standing_of(&rows, "stills"), Standing::Off);
    }

    #[test]
    fn a_release_not_looked_at_yet_waits_for_the_walk() {
        let state = State { walk: Some(WalkCover { before: 100, group: None, active: true }), ..Default::default() };
        let waits = Waits { walk: Some(Wait::About(30.0, 9000)), ..Default::default() };
        let rows = standings(&state, &pace(), &waits, 0);
        assert!(matches!(standing_of(&rows, "naming"), Standing::Queued { lane: "walk", .. }));
        assert!(matches!(standing_of(&rows, "probe"), Standing::Queued { lane: "walk", .. }));
    }

    #[test]
    fn a_new_release_still_settling_has_its_net_steps_waiting_too() {
        let state = State { due_at: Some(5_000), ..Default::default() };
        let rows = standings(&state, &pace(), &Waits::default(), 1_000);
        assert!(matches!(standing_of(&rows, "probe"), Standing::Until(5_000, _)));
        assert!(matches!(standing_of(&rows, "naming"), Standing::Until(5_000, _)));
    }

    #[test]
    fn a_reprocess_walk_shows_its_steps_as_coming_again() {
        let naming = step("naming");
        let state = State {
            has_state: true,
            done: -1,
            walk: Some(WalkCover { before: 100, group: None, active: false }),
            redo_walks: vec![(bit(step("probe").id), true), (bit(naming.id), false)],
            ..Default::default()
        };
        let rows = standings(&state, &pace(), &Waits::default(), 0);
        assert!(matches!(standing_of(&rows, "naming"), Standing::Queued { lane: "walk", wait: Wait::Later, .. }));
        // the walk in progress asked for the probe again, and gets there at its own pace
        let waits = Waits { walk: Some(Wait::About(30.0, 900)), ..Default::default() };
        let rows = standings(&state, &pace(), &waits, 0);
        assert!(matches!(standing_of(&rows, "probe"), Standing::Queued { wait: Wait::About(..), .. }));
        assert!(matches!(standing_of(&rows, "naming"), Standing::Queued { wait: Wait::Later, .. }));
        assert_eq!(*standing_of(&rows, "category"), Standing::Done);
        assert!(render(8, &state, &pace(), &rows, 0).contains("by a later walk"));
    }

    #[test]
    fn a_net_step_that_just_began_is_running_not_a_retry() {
        let probe = step("probe");
        let state = State {
            has_state: true,
            net_wanted: true,
            net_left: Some(bit(probe.id)),
            net_attempts: 1,
            net_next_at: 1_000 + NET_CRASH_RETRY - 5,
            ..Default::default()
        };
        let rows = standings(&state, &pace(), &Waits::default(), 1_000);
        assert_eq!(*standing_of(&rows, "probe"), Standing::Running(1_000 + NET_CRASH_RETRY - 5));
        let two = State { net_left: Some(bit(probe.id) | bit(step("media_lookup").id)), ..state.clone() };
        let rows = standings(&two, &pace(), &Waits::default(), 1_000);
        assert!(matches!(standing_of(&rows, "probe"), Standing::Running(_)));
        assert_eq!(*standing_of(&rows, "media_lookup"), Standing::AfterRunning, "only the first runs");
        let later = State { net_next_at: 1_000 + 3_600, ..state };
        let rows = standings(&later, &pace(), &Waits::default(), 1_000);
        assert!(matches!(standing_of(&rows, "probe"), Standing::Until(_, why) if why.starts_with("a retry")));
    }

    #[test]
    fn an_indexer_that_stopped_writing_stats_isnt_running() {
        let stats = serde_json::json!({
            "load": {"at": 1000.0},
            "postprocess": {"enabled": 6, "lanes": {"window_secs": 600, "net_visits": [1, 2, 3, 4, 5, 6, 7, 8],
                "net_finished": [0, 1, 0, 0, 0, 0, 0, 0], "queue_taken": [9, 9, 9, 9, 9, 9, 9, 9],
                "walk_rows": [0, 0, 0, 0, 0, 0, 0, 0]}}
        });
        let fresh = pace_of(&stats, 1, 1010.0);
        assert!(fresh.measured());
        assert_eq!((fresh.net_visits, fresh.net_finished, fresh.enabled), (2.0, 1.0, Some(6)));
        assert!(!pace_of(&stats, 1, 1000.0 + STALE_SECS + 1.0).running);
        assert!(!pace_of(&Value::Null, 1, 0.0).running);
    }

    #[test]
    fn counts_on_a_shard_stop_at_their_cap() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        crate::db::create_db_at(&main).unwrap();
        let conn = crate::db::open_with_shards(&main).unwrap();
        let s = store::SHARDS - 1;
        for (i, rank) in [(1, 2), (2, 1), (3, 1), (4, 0), (5, 1)] {
            let id = i * store::SHARDS as i64 + s as i64;
            conn.execute(
                &format!("insert into s{s}.pp_state (release_id, net_wanted, net_rank) values (?, 1, ?)"),
                params![id, rank],
            )
            .unwrap();
        }
        let id = 3 * store::SHARDS as i64 + s as i64;
        // ahead by rank: the rank 2, and the newer rank 1 (5); by id: 4 and 5 newer, 1 and 2 older
        let wait = net_wait(&conn, s, id, Some(1), Some(8.0), 0).unwrap();
        assert_eq!(wait, Wait::About(2.0 / 6.0, 2));
        // the queue: rows due together count by release id, as the queue's index holds them
        for (i, due) in [(1, 50), (2, 100), (3, 100), (4, 100), (5, 200)] {
            let qid = i * store::SHARDS as i64 + s as i64;
            conn.execute(&format!("insert into s{s}.pp_queue (release_id, due_at) values (?, ?)"), params![qid, due])
                .unwrap();
        }
        // latest due: 5 (later) and 4 (tied, higher id) at 3/4 of 8 a minute; longest waiting: 1 and 2 at 1/4
        assert_eq!(queue_wait(&conn, s, id, 100, Some(8.0), 1_000).unwrap(), Wait::About(2.0 / 6.0, 2));
        // a group's walk counts only that group's releases
        for (i, group) in [(6, "a"), (7, "b"), (8, "a"), (9, "a")] {
            conn.execute(
                &format!("insert into s{s}.releases (id, name, group_name) values (?, ?, ?)"),
                params![i * store::SHARDS as i64 + s as i64, format!("r{i}"), group],
            )
            .unwrap();
        }
        let walk = WalkCover { before: 10 * store::SHARDS as i64, group: Some("a".into()), active: true };
        let from = 6 * store::SHARDS as i64 + s as i64;
        assert_eq!(walk_wait(&conn, s, from, &walk, Some(60.0)).unwrap(), Wait::About(2.0 / 60.0, 2));
        assert_eq!(capped(&conn, &format!("s{s}.pp_state where net_wanted = 1"), &[], 3).unwrap(), (3, true));
    }
}
