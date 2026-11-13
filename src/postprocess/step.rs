//! What a processing step is, and what it gets to work with.

use std::time::Instant;

use rusqlite::Connection;
use super::result_value::Value;

#[cfg(test)]
#[path = "result_envelope_tests.rs"]
mod result_envelope_tests;

use crate::config::Config;
use crate::nntp::BackgroundRead;
use crate::store;

use super::bodies::BodyFetcher;
pub use super::mutation::{DeferredLegacyStepMutation, ImageRecord, SidecarRecord, StepMutation};

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
    /// Why the step doesn't apply to a release, in words for the release page: None when it does, or when it
    /// can't say. Cheap, no I/O, as `applies`.
    fn skip_reason(&self, release: &ReleaseInfo) -> Option<&'static str> {
        let _ = release;
        None
    }
    fn reruns(&self) -> Reruns {
        Reruns::default()
    }
    /// Asked for by hand on release `release_id`, as a picture on its page: a body step runs past the body gate's
    /// size and type bounds then, which would otherwise mark it done unrun. Only called for a release outside them.
    fn asked_for(&self, conn: &rusqlite::Connection, release_id: i64) -> bool {
        let _ = (conn, release_id);
        false
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
    /// a movie or series, when the release was saved from a Spotnet spot that says so (`spotnet::store::SPOTS`)
    pub spot_kind: Option<crate::spotnet::Kind>,
}

/// What a step gets for one release.
pub struct StepCtx<'a> {
    pub(crate) release: ReleaseInfo,
    pub(crate) data: super::data::StepData<'a>,
    pub(crate) facts: Vec<(&'static str, String)>,
    pub(crate) bodies: &'a BodyFetcher,
    pub(crate) used_bodies: i64,
    pub(crate) deadline: Instant,
    pub(crate) lane_can_fetch: bool,
    /// the CRC of each article body read whole this run, written with the step's outcome (`archive::store::SEGMENT_CRC`)
    pub(crate) crcs: Vec<StepWrite>,
    /// pictures may be fetched: off unless the step asks (`allow_pictures`)
    pub(crate) pictures: bool,
}

impl StepCtx<'_> {
    /// Native reads remain unavailable until the resolver, snapshots and admission have been reviewed.
    pub fn data(&self) -> &super::data::StepData<'_> {
        &self.data
    }

    /// shows a rename made earlier in this run
    pub fn release(&self) -> &ReleaseInfo {
        &self.release
    }

    /// capped at 5,000
    pub fn files(&self) -> rusqlite::Result<Vec<store::FileInfo>> {
        self.data.release_files(self.release.id)
    }

    pub fn segments(&self, file_id: i64, pick: &store::SegmentPick) -> rusqlite::Result<Vec<store::SegmentRef>> {
        self.data.file_segments(file_id, pick)
    }

    /// The legacy shard, opened `query_only`; unavailable native data returns an error.
    /// Do not keep a statement open across `fetch_bodies`: it pins the WAL.
    pub fn conn(&self) -> Result<&Connection, StepError> {
        self.data.legacy_connection()
    }

    /// A value an earlier step in this run left, see `Outcome::facts`.
    pub fn fact(&self, key: &str) -> Option<&str> {
        self.facts.iter().rev().find(|(k, _)| *k == key).map(|(_, v)| v.as_str())
    }

    /// Lets this run fetch pictures, which are refused otherwise: for a step that reads them on purpose.
    pub fn allow_pictures(&mut self) {
        self.pictures = true;
    }

    /// Only from `Needs::Bodies` steps. One at a time, within every budget.
    pub fn fetch_bodies(&mut self, wanted: &[BodyRequest]) -> Result<Vec<BodyResult>, StepError> {
        if !self.lane_can_fetch {
            return Err(StepError::Permanent("a database step cant fetch bodies".into()));
        }
        let mut results = Vec::with_capacity(wanted.len());
        for request in wanted {
            let got = self.bodies.fetch_with(
                &self.release,
                request,
                &mut self.used_bodies,
                self.deadline,
                BackgroundRead::Capped,
                self.pictures,
            )?;
            if let Got::Body(body) = &got
                && request.max_bytes.is_none_or(|cap| body.len() < cap)
            {
                self.keep_crc(&request.message_id, body.len(), crate::gf2::crc32(body));
            }
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
        let got = self.bodies.fetch_with(
            &self.release,
            wanted,
            &mut self.used_bodies,
            self.deadline,
            BackgroundRead::Prefix,
            self.pictures,
        )?;
        Ok(BodyResult { message_id: wanted.message_id.clone(), got })
    }

    /// An article's headers alone, as a server keeps them: for what to check before its body is fetched.
    pub fn fetch_head(&mut self, wanted: &BodyRequest) -> Result<BodyResult, StepError> {
        if !self.lane_can_fetch {
            return Err(StepError::Permanent("a database step cant fetch bodies".into()));
        }
        let got = self.bodies.fetch_with(
            &self.release,
            wanted,
            &mut self.used_bodies,
            self.deadline,
            BackgroundRead::Head,
            self.pictures,
        )?;
        Ok(BodyResult { message_id: wanted.message_id.clone(), got })
    }

    /// `fetch_bodies` for one article that isn't this release's, as a hop an NZB points to: nothing is kept of it
    /// with the release, not even its CRC.
    pub fn fetch_foreign_body(&mut self, wanted: &BodyRequest) -> Result<BodyResult, StepError> {
        if !self.lane_can_fetch {
            return Err(StepError::Permanent("a database step cant fetch bodies".into()));
        }
        let got = self.bodies.fetch_with(
            &self.release,
            wanted,
            &mut self.used_bodies,
            self.deadline,
            BackgroundRead::Capped,
            self.pictures,
        )?;
        Ok(BodyResult { message_id: wanted.message_id.clone(), got })
    }

    /// One article undecoded, with its headers, for what `BODY` leaves out.
    pub fn fetch_article(&mut self, wanted: &BodyRequest) -> Result<BodyResult, StepError> {
        self.raw(wanted, BackgroundRead::Raw)
    }

    /// `fetch_article` for one article's start: `wanted.max_bytes` of it, and a hang-up when that cut it short. For
    /// a file's head in an article too big to read whole.
    pub fn fetch_article_prefix(&mut self, wanted: &BodyRequest) -> Result<BodyResult, StepError> {
        self.raw(wanted, BackgroundRead::RawPrefix)
    }

    fn raw(&mut self, wanted: &BodyRequest, read: BackgroundRead) -> Result<BodyResult, StepError> {
        if !self.lane_can_fetch {
            return Err(StepError::Permanent("a database step cant fetch bodies".into()));
        }
        let got =
            self.bodies.fetch_with(&self.release, wanted, &mut self.used_bodies, self.deadline, read, self.pictures)?;
        // a cut one has no trailer, soo nothing's kept of it
        if let Got::Body(raw) = &got {
            // what the trailer says of the part: its CRC, and how many bytes it decodes to
            let article = crate::probe::yenc::split_article(raw);
            if let Some(y) = crate::probe::yenc::parse(&article.lines)
                && let Some(crc) = y.pcrc32.or(y.crc32)
            {
                let decoded = match (y.begin, y.end) {
                    (Some(begin), Some(end)) if end >= begin => end - begin + 1,
                    _ => y.yend_size.or(y.file_size).unwrap_or(0),
                };
                if decoded > 0 {
                    self.keep_crc(&wanted.message_id, decoded as usize, crc);
                }
            }
        }
        Ok(BodyResult { message_id: wanted.message_id.clone(), got })
    }

    /// An article's CRC, kept with this run's writes.
    fn keep_crc(&mut self, message_id: &str, decoded: usize, crc: u32) {
        self.crcs.push(StepWrite::for_mutation(self.release.id, StepMutation::RecordSegmentCrc {
            message_id: message_id.to_string(),decoded: decoded as i64,crc32: crc,
        }));
    }

    /// Whether a server still has an article, with nothing transferred. None: it wasn't asked.
    pub fn article_exists(&mut self, message_id: &str, file: &str) -> Result<Option<bool>, StepError> {
        if !self.lane_can_fetch {
            return Err(StepError::Permanent("a database step cant fetch bodies".into()));
        }
        let wanted = BodyRequest { message_id: message_id.to_string(), file: file.to_string(), max_bytes: Some(1) };
        let got = self.bodies.fetch_with(
            &self.release,
            &wanted,
            &mut self.used_bodies,
            self.deadline,
            BackgroundRead::Stat,
            self.pictures,
        )?;
        Ok(match got {
            Got::Body(_) | Got::TooBig => Some(true),
            Got::Missing => Some(false),
            Got::Refused(_) => None,
        })
    }

    pub fn stopping(&self) -> bool {
        self.bodies.stopping()
    }

    /// Fetches this run may still make before `postprocess.bodies_per_release` refuses them.
    pub fn bodies_left(&self) -> i64 {
        (self.bodies.per_release() - self.used_bodies).max(0)
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
    /// Not finished, nothing wrong: look again then. Uses no attempt, sets no bit. Its `writes` are still applied,
    /// soo a step can keep how far it got.
    pub wait_until: Option<i64>,
    /// Kept by the stage until the writer has answered for this outcome's batch,
    /// then dropped, on any path. So a step can hold something for exactly as
    /// long as its result isnt committed.
    pub hold: Option<Box<dyn std::any::Any + Send>>,
    /// what it found makes the release junk: it is deleted in place of these writes, see `junk`
    pub junk: Option<&'static str>,
}

pub struct StepWrite {
    pub sql: &'static str,
    pub params: Vec<Value>,
    /// Legacy writer-time callback. Never admitted by the native envelope.
    pub late_params: Option<Box<dyn FnOnce() -> Vec<Value> + Send>>,
    /// Public fields are compatibility shadows; only this private closed value
    /// supplies native mutation data, after the shadows have been checked.
    native: Option<NativeEnvelope>,
}

struct NativeEnvelope {
    release_id: i64,
    mutation: StepMutation,
}

impl StepWrite {
    pub fn new(sql: &'static str, params: Vec<Value>) -> StepWrite {
        StepWrite { sql, params, late_params: None, native: None }
    }

    pub fn with_late_params(mut self, late: Box<dyn FnOnce() -> Vec<Value> + Send>) -> Self {
        self.late_params = Some(late);
        self
    }

    /// Keeps legacy late-cache timing without admitting that cache or callback to the native writer.
    pub fn for_deferred_legacy(release_id: i64, mutation: DeferredLegacyStepMutation) -> Self {
        let sql = mutation.sqlite_sql();
        Self { sql,params: Vec::new(),late_params: Some(Box::new(move || mutation.sqlite_parameters(release_id))),native: None }
    }

    pub fn for_mutation(release_id: i64, mutation: StepMutation) -> Self {
        let (sql, params) = mutation.v1_fields(release_id);
        Self { sql, params, late_params: None, native: Some(NativeEnvelope {release_id, mutation}) }
    }

    pub(crate) fn legacy_allowed(&self)->bool {
        !self.native.as_ref().is_some_and(|envelope|matches!(envelope.mutation,StepMutation::ParseMedia {..}|StepMutation::PredbRead {..}))
    }

    fn checked_envelope(&self)->Result<&NativeEnvelope,StepError> {
        let envelope=self.native.as_ref().ok_or_else(||StepError::Retry("Step mutation is not ported to native storage".into()))?;
        let (tag,values)=envelope.mutation.v1_fields(envelope.release_id);
        if self.sql!=tag || self.params!=values || self.late_params.is_some() {
            return Err(StepError::Retry("Step mutation representation changed".into()));
        }
        Ok(envelope)
    }

    /// The queue actor checks the retained target as well as the closed
    /// representation. A result for another release cannot borrow its lease.
    pub(crate) fn native_mutation_for(&self, release_id:i64)->Result<&StepMutation,StepError> {
        if self.native.as_ref().is_none_or(|envelope|envelope.release_id!=release_id) {
            return Err(StepError::Retry("native Step result targets another release".into()));
        }
        self.native_mutation()
    }
    pub(crate) fn native_observed_for(&self,release_id:i64,targets:&crate::pg::stage_queue::participants::Observed)->Result<&StepMutation,StepError> {
        let envelope=self.checked_envelope()?;
        if envelope.release_id!=release_id || !targets.permits(release_id,&envelope.mutation) {
            return Err(StepError::Retry("native result has no observed target".into()));
        }
        Ok(&envelope.mutation)
    }

    /// Caller SQL and numeric targets alone cannot supply native authority.
    pub fn native_mutation(&self) -> Result<&StepMutation, StepError> {
        let envelope=self.checked_envelope()?;
        if envelope.mutation.requires_target_resolution() {
            return Err(StepError::Retry("Step targets require the native resolver".into()));
        }
        Ok(&envelope.mutation)
    }

    /// Encoding is data, never a lease. Visit still validates its Claim and
    /// Observed participants before this call and again at actual settlement.
    pub(crate) fn encode_native_v1(&self,step:StepMask,bytes:&mut usize,sink:&mut impl FnMut(&[u8]))->anyhow::Result<()> {
        let envelope=self.checked_envelope().map_err(|_|anyhow::anyhow!("native result is not a closed mutation"))?;
        let (tag,values)=envelope.mutation.v1_fields(envelope.release_id);
        super::result_value::encode_write_v1(step,tag,&values,bytes,sink)
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
    /// A native storage/cache error retains its driver cause. The owner drains;
    /// this cannot be converted to a cache miss or a per-release retry ACK.
    Native(anyhow::Error),
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

impl StepError {
    pub fn from_cache(error:anyhow::Error)->Self {Self::Native(error)}
    pub fn from_inventory(error:rusqlite::Error)->Self {
        if matches!(error,rusqlite::Error::UserFunctionError(_)) {Self::Native(error.into())}
        else {Self::Retry(error.to_string())}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::AtomicBool};

    #[test]
    fn a_context_without_native_authority_needs_no_sqlite_connection() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let bodies = BodyFetcher::new(Arc::new(crate::nntp::Pool::new(&[])),runtime.handle().clone(),
            Arc::new(AtomicBool::new(false)),crate::config::PostprocessConfig::default(),
            Arc::new(crate::postprocess::stats::Stats::default()));
        let context = StepCtx { release: ReleaseInfo::default(),data: super::super::data::StepData::NativeUnavailable,
            facts: Vec::new(),bodies: &bodies,used_bodies: 0,deadline: Instant::now(),lane_can_fetch: false,
            crcs: Vec::new(),pictures: false };
        assert!(matches!(context.conn(),Err(StepError::Retry(_))));
        assert!(context.files().is_err());
        assert!(context.segments(1,&store::SegmentPick::First(1)).is_err());
    }
}
