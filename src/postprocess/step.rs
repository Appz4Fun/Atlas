//! What a processing step is, and what it gets to work with.

use std::time::Instant;

use rusqlite::Connection;
use rusqlite::types::Value;

use crate::config::Config;
use crate::nntp::BackgroundRead;
use crate::store;

use super::bodies::BodyFetcher;

/// Never reused, 1..=62.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StepId(pub u8);
/// bit `1 << id`
pub type StepMask = i64;

pub const fn mask(id: StepId) -> StepMask {
    1 << id.0
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Needs {
    Database,
    Bodies,
    External,
}

#[derive(Default, Clone, Copy, Debug)]
pub struct Reruns {
    pub on_rename: bool,
    pub on_growth: bool,
}

pub trait Step: Send + Sync + 'static {
    fn id(&self) -> StepId;
    /// the --step name, and the dashboard's
    fn name(&self) -> &'static str;
    /// run order, unique
    fn rank(&self) -> u16;
    fn needs(&self) -> Needs;
    /// Is it on? Also the one way a step sees config: called on every step
    /// before any run and on every reload, more than once for one config.
    fn enabled(&self, cfg: &Config) -> bool {
        let _ = cfg;
        true
    }
    /// Cheap, no I/O. False: not run and not marked done, in either lane.
    fn applies(&self, release: &ReleaseInfo) -> bool {
        let _ = release;
        true
    }
    fn reruns(&self) -> Reruns {
        Reruns::default()
    }
    /// Inputs that can change while `enabled` stays true, one bit each. A bit
    /// that wasnt there before redoes this step on existing releases. Cheap, no I/O.
    fn config_stamp(&self) -> u64 {
        0
    }
    /// Synchronous, on a stage thread. Safe to block on `fetch_bodies` or a `ureq` call.
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError>;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub id: i64,
    pub shard: usize,
    pub name: String,
    pub display_name: Option<String>,
    pub group: String,
    pub poster: Option<String>,
    pub posted_date: Option<String>,
    pub size: i64,
    pub parts: i64,
    pub complete: bool,
    pub file_total: Option<i64>,
    pub is_obfuscated: bool,
    pub name_source: Option<i64>,
}

/// What a step gets for one release.
pub struct StepCtx<'a> {
    pub(crate) release: ReleaseInfo,
    pub(crate) conn: &'a Connection,
    pub(crate) facts: Vec<(&'static str, String)>,
    pub(crate) bodies: &'a BodyFetcher,
    pub(crate) used_bodies: i64,
    pub(crate) deadline: Instant,
    pub(crate) lane_can_fetch: bool,
}

impl StepCtx<'_> {
    /// shows a rename made earlier in this run
    pub fn release(&self) -> &ReleaseInfo {
        &self.release
    }

    /// capped at 5,000
    pub fn files(&self) -> rusqlite::Result<Vec<store::FileInfo>> {
        store::release_files(self.conn, self.release.id, 5_000)
    }

    pub fn segments(&self, file_id: i64, pick: &store::SegmentPick) -> rusqlite::Result<Vec<store::SegmentRef>> {
        store::file_segments(self.conn, file_id, pick)
    }

    /// This shard, opened `query_only`. For reading your own tables. Do not keep
    /// a statement open across `fetch_bodies`: it pins the WAL.
    pub fn conn(&self) -> &Connection {
        self.conn
    }

    /// A value an earlier step in this run left, see `Outcome::facts`.
    pub fn fact(&self, key: &str) -> Option<&str> {
        self.facts.iter().rev().find(|(k, _)| *k == key).map(|(_, v)| v.as_str())
    }

    /// Only from `Needs::Bodies` steps. One at a time, within every budget.
    pub fn fetch_bodies(&mut self, wanted: &[BodyRequest]) -> Result<Vec<BodyResult>, StepError> {
        if !self.lane_can_fetch {
            return Err(StepError::Permanent("a database step cant fetch bodies".into()));
        }
        let mut results = Vec::with_capacity(wanted.len());
        for request in wanted {
            let got = self.bodies.fetch(
                &self.release,
                request,
                &mut self.used_bodies,
                self.deadline,
                BackgroundRead::Capped,
            )?;
            results.push(BodyResult { message_id: request.message_id.clone(), got });
        }
        Ok(results)
    }

    /// `fetch_bodies` for one article's start: `wanted.max_bytes` of it, and a hang-up
    /// when that cut it short. For a probe that doesnt know what it's about to read.
    pub fn fetch_prefix(&mut self, wanted: &BodyRequest) -> Result<BodyResult, StepError> {
        if !self.lane_can_fetch {
            return Err(StepError::Permanent("a database step cant fetch bodies".into()));
        }
        let got =
            self.bodies.fetch(&self.release, wanted, &mut self.used_bodies, self.deadline, BackgroundRead::Prefix)?;
        Ok(BodyResult { message_id: wanted.message_id.clone(), got })
    }

    /// One article undecoded, with its headers, for what `BODY` leaves out.
    pub fn fetch_article(&mut self, wanted: &BodyRequest) -> Result<BodyResult, StepError> {
        if !self.lane_can_fetch {
            return Err(StepError::Permanent("a database step cant fetch bodies".into()));
        }
        let got =
            self.bodies.fetch(&self.release, wanted, &mut self.used_bodies, self.deadline, BackgroundRead::Raw)?;
        Ok(BodyResult { message_id: wanted.message_id.clone(), got })
    }

    /// Whether a server still has an article, with nothing transferred. None: it wasn't asked.
    pub fn article_exists(&mut self, message_id: &str, file: &str) -> Result<Option<bool>, StepError> {
        if !self.lane_can_fetch {
            return Err(StepError::Permanent("a database step cant fetch bodies".into()));
        }
        let wanted = BodyRequest { message_id: message_id.to_string(), file: file.to_string(), max_bytes: Some(1) };
        let got =
            self.bodies.fetch(&self.release, &wanted, &mut self.used_bodies, self.deadline, BackgroundRead::Stat)?;
        Ok(match got {
            Got::Body(_) | Got::TooBig => Some(true),
            Got::Missing => Some(false),
            Got::Refused(_) => None,
        })
    }

    pub fn stopping(&self) -> bool {
        self.bodies.stopping()
    }

    /// run start + 120 s
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
}

pub struct BodyRequest {
    pub message_id: String,
    pub file: String,
    pub max_bytes: Option<usize>,
}

pub struct BodyResult {
    pub message_id: String,
    pub got: Got,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Got {
    Body(Vec<u8>),
    Missing,
    TooBig,
    Refused(&'static str),
}

#[derive(Default)]
pub struct Outcome {
    /// Found something, for stats. Done with `matched: false` means "checked, no match".
    pub matched: bool,
    /// Applied in one transaction with the state change. Your own tables only, or
    /// your own `releases` column. Static SQL with bound values.
    pub writes: Vec<StepWrite>,
    pub name: Option<NameChange>,
    /// Later steps to run again: bits of steps with a higher rank. Others are ignored.
    pub rerun: StepMask,
    /// For later steps in the same run (their database writes are not visible yet).
    pub facts: Vec<(&'static str, String)>,
    /// Not finished, nothing wrong: look again then. Uses no attempt, sets no bit.
    pub wait_until: Option<i64>,
    /// Kept by the stage until the writer has answered for this outcome's batch,
    /// then dropped, on any path. So a step can hold something for exactly as
    /// long as its result isnt committed.
    pub hold: Option<Box<dyn std::any::Any + Send>>,
}

pub struct StepWrite {
    pub sql: &'static str,
    pub params: Vec<Value>,
    /// Called on the shard's writer just before this write, in place of `params`, for a
    /// value that must be current then and not when the step ran. It must be quick.
    pub late_params: Option<Box<dyn FnOnce() -> Vec<Value> + Send>>,
}

impl StepWrite {
    pub fn new(sql: &'static str, params: Vec<Value>) -> StepWrite {
        StepWrite { sql, params, late_params: None }
    }
}

/// `source` 1..=127: higher wins, equal replaces. `display_name: None` leaves the
/// name, `obfuscated: None` leaves the flag. A change of either sets `name_source`,
/// soo saves leave both alone from then on. Both None: nothing happens. Only a
/// `display_name` that differs counts as a rename. Never touches releases.name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameChange {
    pub display_name: Option<String>,
    pub obfuscated: Option<bool>,
    pub source: u8,
}

#[derive(Debug)]
pub enum StepError {
    /// a real failure: uses an attempt and backs off
    Retry(String),
    /// failed bit now
    Permanent(String),
    /// nothing wrong with this release: a server down, a file locked
    Transient(String),
    /// a budget is spent for now
    OutOfBudget,
    /// shutting down
    Stopped,
}
