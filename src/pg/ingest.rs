//! Bounded native saves shared by indexer passes and the NXV linker.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use tokio::sync::Semaphore;

use crate::parser::{Article, Release};

#[derive(Clone, Copy, Debug)]
pub struct PgWriteLimits {
    pub max_pending_bytes: u32,
    pub max_job_bytes: u32,
    pub concurrent_saves: u16,
}

impl Default for PgWriteLimits {
    /// Saves queue on the indexer's one owner session, which takes up to three ingestion turns in a row before a
    /// waiting ordinary one. With two jobs of 8 MiB at most (32 MiB pending) the queue held too little to use them:
    /// on prod a save did ~10 ms of work after ~120 ms of waiting, fetched headers filled the unsaved budget, and
    /// ingest stayed near 40k articles a second. Eight jobs of up to 32 MiB (256 MiB pending) kept it supplied; with
    /// helper writers saving side by side (`Helpers`, one session per job) sixteen at once, 512 MiB pending: at eight
    /// prod's saves waited ~0.7 s each for a slot at ~270k articles a second.
    fn default() -> Self {
        Self { max_pending_bytes: 512 * 1024 * 1024, max_job_bytes: 32 * 1024 * 1024, concurrent_saves: 16 }
    }
}

impl PgWriteLimits {
    pub fn check(self) -> Result<()> {
        ensure!(
            self.max_job_bytes > 0 && self.max_job_bytes <= self.max_pending_bytes,
            "native save limits require 0 < max_job_bytes <= max_pending_bytes"
        );
        ensure!(self.concurrent_saves > 0, "native saves require at least one worker");
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct Native {
    source: Source,
    /// sessions that save ordinary jobs beside the owner (`with_helpers`); permits for owner work stay on the owner
    helpers: Option<Arc<Helpers>>,
    /// jobs of one group waiting for that group's running save (`save_coalesced`)
    queues: Arc<std::sync::Mutex<HashMap<String, GroupQueue>>>,
    memory: Arc<Semaphore>,
    slots: Arc<Semaphore>,
    limits: PgWriteLimits,
}

/// Jobs of one group waiting while a save of that group runs: saves of a group take its lock one after another, and
/// each waiting one held a helper session and a save slot meanwhile. They are saved together once it ends.
#[derive(Default)]
struct GroupQueue {
    pending: std::collections::VecDeque<Waiting>,
}

type Queues = Arc<std::sync::Mutex<HashMap<String, GroupQueue>>>;

struct Waiting {
    releases: Vec<Release>,
    /// the caller's logical clock, if it gave one: only jobs with the same one are saved together
    logical: Option<i64>,
    now: i64,
    bytes: usize,
    /// its share of `max_pending_bytes`, held until its outcome is told (a caller going away doesnt free it)
    _memory: tokio::sync::OwnedSemaphorePermit,
    done: tokio::sync::oneshot::Sender<std::result::Result<(), Arc<anyhow::Error>>>,
}

/// A save's failure as every job in it sees it: the original error, its chain kept for `outage::classify` and
/// `is_attempt_ended`.
#[derive(Debug)]
struct SharedError(Arc<anyhow::Error>);

impl std::fmt::Display for SharedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for SharedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(AsRef::<dyn std::error::Error + Send + Sync + 'static>::as_ref(&*self.0))
    }
}

fn told(outcome: std::result::Result<std::result::Result<(), Arc<anyhow::Error>>, tokio::sync::oneshot::error::RecvError>) -> Result<()> {
    match outcome {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(anyhow::Error::new(SharedError(error))),
        Err(_) => Err(anyhow::anyhow!("the save of the group this job waited on ended without saying how")),
    }
}

/// Jobs and bytes one merged save takes at most from a group's queue, and merged saves one task makes before it gives
/// its save slot back and queues again for one, so a busy group doesnt keep it from the others.
const MERGE_JOBS: usize = 8;
const MERGE_BYTES: usize = 64 << 20;
const DRAIN_ROUNDS: usize = 4;

/// Run `futures` side by side, their outputs in their order.
async fn join_all<F: std::future::Future>(futures: Vec<F>) -> Vec<F::Output> {
    let mut futures: Vec<_> = futures.into_iter().map(Box::pin).collect();
    let mut outputs: Vec<Option<F::Output>> = futures.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (future, output) in futures.iter_mut().zip(outputs.iter_mut()) {
            if output.is_none() {
                match future.as_mut().poll(cx) {
                    std::task::Poll::Ready(value) => *output = Some(value),
                    std::task::Poll::Pending => pending = true,
                }
            }
        }
        if pending { std::task::Poll::Pending } else { std::task::Poll::Ready(()) }
    })
    .await;
    outputs.into_iter().map(|output| output.expect("every future finished")).collect()
}

/// `releases` by the shard of its group each is saved under (`release_writer::shard`), keyed `group#shard` as the
/// writer's lock is: the parts of one job save side by side, each after the running save of its shard.
fn by_part(releases: Vec<Release>) -> Vec<(String, Vec<Release>)> {
    let mut parts = HashMap::<String, Vec<Release>>::new();
    for release in releases {
        let key = format!("{}#{}", release.group, super::release_writer::shard(&release));
        parts.entry(key).or_default().push(release);
    }
    let mut parts: Vec<_> = parts.into_iter().collect();
    parts.sort_by(|a, b| a.0.cmp(&b.0));
    parts
}

/// Takes `group`'s queue entry out unless disarmed: a drain that ends without handing on (a panic, a task that never
/// ran) lets the group go, and what still waited is told so by its dropped sender.
struct LetGo {
    queues: Queues,
    group: String,
    armed: bool,
}

impl Drop for LetGo {
    fn drop(&mut self) {
        if self.armed {
            self.queues.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&self.group);
        }
    }
}

/// The next jobs of `group` to save together, in arrival order: those with the front one's logical clock, up to
/// `MERGE_JOBS` and `MERGE_BYTES`. Empty takes the entry out (the group is let go).
fn next_batch(queues: &Queues, group: &str) -> Vec<Waiting> {
    let mut queues = queues.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(queue) = queues.get_mut(group) else { return Vec::new() };
    let Some(logical) = queue.pending.front().map(|w| w.logical) else {
        queues.remove(group);
        return Vec::new();
    };
    let mut batch = Vec::new();
    let mut size = 0usize;
    while let Some(next) = queue.pending.front() {
        if next.logical != logical || batch.len() >= MERGE_JOBS || (!batch.is_empty() && size + next.bytes > MERGE_BYTES) {
            break;
        }
        size += next.bytes;
        batch.push(queue.pending.pop_front().expect("front checked"));
    }
    batch
}

/// Save `batch` and tell each job its outcome. A merged batch that failed for anything but an ended attempt is saved
/// again job by job, so one job's bad data fails only that job.
fn save_batch(helpers: &Helpers, mut batch: Vec<Waiting>) {
    let now = batch[0].now;
    let releases: Vec<Release> = batch.iter().flat_map(|w| w.releases.iter().cloned()).collect();
    let outcome = helpers.save(&releases, now);
    drop(releases);
    match outcome {
        Err(error) if batch.len() > 1 && !super::outage::is_attempt_ended(&error) => {
            for waiting in batch.drain(..) {
                let outcome = helpers.save(&waiting.releases, waiting.now).map_err(Arc::new);
                let _ = waiting.done.send(outcome);
            }
        }
        outcome => {
            let outcome = outcome.map_err(Arc::new);
            for waiting in batch.drain(..) {
                let _ = waiting.done.send(outcome.clone());
            }
        }
    }
}

/// Drain `group`'s queue on this blocking thread, holding `slot`, for up to `DRAIN_ROUNDS` merged saves; then hand
/// on to a new task that waits for a slot again. `guard` lets the group go if this ends any other way.
fn drain(handle: tokio::runtime::Handle, slots: Arc<Semaphore>, helpers: Arc<Helpers>, mut guard: LetGo, slot: tokio::sync::OwnedSemaphorePermit) {
    for _ in 0..DRAIN_ROUNDS {
        let batch = next_batch(&guard.queues, &guard.group);
        if batch.is_empty() {
            guard.armed = false;
            return;
        }
        save_batch(&helpers, batch);
    }
    drop(slot);
    // more waits: a new task queues for a slot like any other job, and drains on
    let next = LetGo { queues: guard.queues.clone(), group: guard.group.clone(), armed: true };
    guard.armed = false;
    let on = handle.clone();
    handle.spawn(async move {
        let Ok(slot) = slots.clone().acquire_owned().await else { return };
        let _ = on.clone().spawn_blocking(move || drain(on, slots, helpers, next, slot));
    });
}

/// The most helper writer sessions a runtime opens (`config.native_helper_writers`): no more save jobs run at once
/// (`PgWriteLimits::concurrent_saves`).
pub(crate) const HELPER_WRITERS_MOST: usize = 16;

/// Times a helper's save is tried again after PostgreSQL broke a deadlock or a serialization conflict with it: saves
/// of different groups and the owner's own work now run side by side, and either one can be the one picked.
const HELPER_RETRIES: usize = 4;

/// PostgreSQL sessions that save releases beside the owner session. Each names the owner's epoch
/// (`atlas.helper_epoch`); the write guard (migration 0016) then fences every transaction of it on that epoch: the
/// owner backend alive and holding its exclusive lock, and the current-epoch row held FOR SHARE until it ends, so no
/// new owner takes over meanwhile. Saves of one group still queue on that group's lock.
pub(crate) struct Helpers {
    pool: Arc<super::Pool>,
    epoch: String,
    owner: Option<Arc<super::indexer_runtime::Owner>>,
}

impl Helpers {
    pub(crate) fn new(pool: Arc<super::Pool>, epoch: &[u8; 32]) -> Self {
        Helpers { pool, epoch: epoch.iter().map(|b| format!("{b:02x}")).collect(), owner: None }
    }

    /// Saves stop with `owner`: none starts once it stops or fails, as none would through the owner itself.
    pub(crate) fn of(mut self, owner: Arc<super::indexer_runtime::Owner>) -> Self {
        self.owner = Some(owner);
        self
    }

    pub(crate) fn save(&self, releases: &[Release], now: i64) -> Result<()> {
        let mut tries = 0;
        loop {
            if let Some(owner) = &self.owner
                && (owner.stop.load(std::sync::atomic::Ordering::Acquire) || owner.failure().is_some())
            {
                return Err(super::outage::AttemptEnded.into());
            }
            let mut client = self.pool.get()?;
            client.execute("select set_config('atlas.helper_epoch', $1, false)", &[&self.epoch])?;
            match super::release_writer::save(&mut client, releases, now) {
                Ok(_) => return Ok(()),
                Err(error) if tries < HELPER_RETRIES && retryable(&error) => tries += 1,
                Err(error) => return Err(error),
            }
        }
    }
}

/// A deadlock, serialization failure or lock wait past `HELPER_LOCK_WAIT` that rolled this transaction back: trying it
/// again is safe.
fn retryable(error: &anyhow::Error) -> bool {
    use postgres::error::SqlState;
    error.chain().filter_map(|e| e.downcast_ref::<postgres::Error>()).any(|e| {
        e.code().is_some_and(|code| {
            *code == SqlState::T_R_DEADLOCK_DETECTED
                || *code == SqlState::T_R_SERIALIZATION_FAILURE
                || *code == SqlState::LOCK_NOT_AVAILABLE
        })
    })
}

/// How long a helper writer waits for a row lock once it holds its group's lock (`release_writer::save_grouped`).
/// The owner's work locks the same releases and queue rows in its own order; a helper waiting on it gives up and tries
/// again rather than closing a deadlock the owner could be picked to lose.
pub(crate) const HELPER_LOCK_WAIT: &str = "300ms";

#[derive(Clone)]
enum Source {
    Pool(Arc<super::Pool>),
    Owner(Arc<super::indexer_runtime::Owner>),
}

impl Source {
    fn with<T>(&self, work: impl FnOnce(&mut super::Client) -> Result<T>) -> Result<T> {
        match self {
            Self::Pool(pool) => {
                let mut client = pool.get()?;
                work(&mut client)
            }
            Self::Owner(owner) => owner.with_ingest(work),
        }
    }
}

/// Admission from the original Native's shared memory/slot limits. This is
/// resource ownership only; every PG write still goes through the original Owner, whose epoch proof serves one second (`EPOCH_PROOF_FOR`).
/// Keep it through the caller's commit/rollback, reusing it only after dropping
/// each previously checked batch before allocating the next one.
pub(crate) struct NativeSavePermit {
    owner:Arc<super::indexer_runtime::Owner>,
    max_job_bytes:usize,
    _memory:tokio::sync::OwnedSemaphorePermit,
    _slot:tokio::sync::OwnedSemaphorePermit,
}
impl NativeSavePermit {
    pub(crate) fn max_job_bytes(&self)->usize {self.max_job_bytes}
    pub(crate) fn check_batch(&self,releases:&Vec<Release>)->Result<()> {
        if self.owner.stop.load(std::sync::atomic::Ordering::Acquire) {return Err(super::outage::AttemptEnded.into());}
        ensure!(payload_bytes(releases,releases.capacity())<=self.max_job_bytes,"native reserved batch exceeds max_job_bytes");
        Ok(())
    }
}

impl Native {
    pub fn new(pool: Arc<super::Pool>, limits: PgWriteLimits) -> Result<Self> {
        Self::from_source(Source::Pool(pool), limits)
    }

    pub(crate) fn with_owner(owner: Arc<super::indexer_runtime::Owner>, limits: PgWriteLimits) -> Result<Self> {
        Self::from_source(Source::Owner(owner), limits)
    }

    /// Ordinary save jobs go to `helpers` from now on; owner permits and everything else stay on the owner.
    pub(crate) fn with_helpers(mut self, helpers: Helpers) -> Self {
        self.helpers = Some(Arc::new(helpers));
        self
    }

    fn from_source(source: Source, limits: PgWriteLimits) -> Result<Self> {
        limits.check()?;
        Ok(Self {
            source,
            helpers: None,
            queues: Arc::new(std::sync::Mutex::new(HashMap::new())),
            memory: Arc::new(Semaphore::new(limits.max_pending_bytes as usize)),
            slots: Arc::new(Semaphore::new(usize::from(limits.concurrent_saves))),
            limits,
        })
    }

    pub(crate) fn check_native_owner(&self,owner:&Arc<super::indexer_runtime::Owner>)->Result<()> {
        ensure!(matches!(&self.source,Source::Owner(actual) if Arc::ptr_eq(actual,owner)),"native save requires its original owner backend");
        if owner.stop.load(std::sync::atomic::Ordering::Acquire) {return Err(super::outage::AttemptEnded.into());}
        Ok(())
    }

    /// Reserve before expanding input into Articles. Both permits come from
    /// this same Native, in ordinary save_job's memory-then-slot order.
    pub(crate) async fn reserve_native_for_owner(&self,owner:&Arc<super::indexer_runtime::Owner>)->Result<NativeSavePermit> {
        self.check_native_owner(owner)?;
        let memory_wait=self.memory.clone().acquire_many_owned(self.limits.max_job_bytes);
        tokio::pin!(memory_wait);
        let memory=loop {
            self.check_native_owner(owner)?;
            tokio::select! {
                result=&mut memory_wait=>break result?,
                _=tokio::time::sleep(std::time::Duration::from_millis(25))=>{},
            }
        };
        self.check_native_owner(owner)?;
        let slot_wait=self.slots.clone().acquire_owned();
        tokio::pin!(slot_wait);
        let slot=loop {
            self.check_native_owner(owner)?;
            tokio::select! {
                result=&mut slot_wait=>break result?,
                _=tokio::time::sleep(std::time::Duration::from_millis(25))=>{},
            }
        };
        self.check_native_owner(owner)?;
        Ok(NativeSavePermit {owner:owner.clone(),max_job_bytes:self.limits.max_job_bytes as usize,_memory:memory,_slot:slot})
    }

    pub(crate) async fn save_at(&self, releases: Vec<Release>, logical_now: i64) -> Result<()> {
        self.save_with_time(releases, Some(logical_now)).await
    }

    /// Completion covers every bounded commit. On failure or cancellation, callers must replay the input before
    /// acknowledging its cursor/hours; the native writer deduplicates the pieces that already committed.
    pub async fn save(&self, releases: Vec<Release>) -> Result<()> {
        self.save_with_time(releases, None).await
    }

    async fn save_with_time(&self, releases: Vec<Release>, logical_now: Option<i64>) -> Result<()> {
        if payload_bytes(&releases, releases.capacity()) <= self.limits.max_job_bytes as usize {
            return self.save_job(releases, logical_now).await;
        }
        // Keep the writer's whole-input executable rule even when a late fragment is in another commit.
        let mut junk = HashMap::new();
        for release in &releases {
            if let Some(name) = release
                .articles
                .iter()
                .filter_map(|a| a.filename.as_ref())
                .find(|name| crate::junk::is_executable(name))
            {
                let (collection, key) = super::release_writer::identity(release);
                junk.insert((release.group.clone(), collection, key), name.clone());
            }
        }
        let mut submitted_junk = HashSet::new();
        let mut batch = Vec::new();
        let mut batch_bytes = 0;
        for mut metadata in releases {
            let (collection, key) = super::release_writer::identity(&metadata);
            let key = (metadata.group.clone(), collection, key);
            if let Some(name) = junk.get(&key) {
                if submitted_junk.insert(key) {
                    metadata.articles = vec![Article { filename: Some(name.clone()), ..Default::default() }];
                    self.add_piece(metadata, &mut batch, &mut batch_bytes, logical_now).await?;
                }
                continue;
            }
            let articles = std::mem::take(&mut metadata.articles);
            let mut piece = metadata.clone();
            let base = payload_bytes(std::slice::from_ref(&piece), 1);
            let mut strings = 0usize;
            for article in articles {
                let heap = article_heap_bytes(&article);
                let mut capacity = next_capacity(piece.articles.len(), piece.articles.capacity());
                let projected = |capacity: usize, strings: usize| {
                    base.saturating_add(strings).saturating_add(capacity.saturating_mul(std::mem::size_of::<Article>()))
                };
                if projected(capacity, strings.saturating_add(heap)) > self.limits.max_job_bytes as usize
                    && !piece.articles.is_empty()
                {
                    self.add_piece(piece, &mut batch, &mut batch_bytes, logical_now).await?;
                    piece = metadata.clone();
                    strings = 0;
                    capacity = 1;
                }
                ensure!(
                    projected(capacity, strings.saturating_add(heap)) <= self.limits.max_job_bytes as usize,
                    "one native article or its release metadata exceeds max_job_bytes"
                );
                if capacity > piece.articles.capacity() {
                    piece.articles.reserve_exact(capacity - piece.articles.len());
                }
                strings = strings.saturating_add(heap);
                piece.articles.push(article);
            }
            if !piece.articles.is_empty() {
                self.add_piece(piece, &mut batch, &mut batch_bytes, logical_now).await?;
            }
        }
        self.save_job(batch, logical_now).await
    }

    async fn add_piece(
        &self,
        piece: Release,
        batch: &mut Vec<Release>,
        bytes: &mut usize,
        logical_now: Option<i64>,
    ) -> Result<()> {
        let owned = payload_bytes(std::slice::from_ref(&piece), 1) - std::mem::size_of::<Release>();
        let mut capacity = next_capacity(batch.len(), batch.capacity());
        let projected = |bytes: usize, old: usize, new: usize| {
            bytes.saturating_add(owned).saturating_add((new - old).saturating_mul(std::mem::size_of::<Release>()))
        };
        if projected(*bytes, batch.capacity(), capacity) > self.limits.max_job_bytes as usize && !batch.is_empty() {
            self.save_job(std::mem::take(batch), logical_now).await?;
            *bytes = 0;
            capacity = 1;
        }
        let next = projected(*bytes, batch.capacity(), capacity);
        ensure!(next <= self.limits.max_job_bytes as usize, "native release metadata exceeds max_job_bytes");
        if capacity > batch.capacity() {
            batch.reserve_exact(capacity - batch.len());
        }
        batch.push(piece);
        *bytes = next;
        Ok(())
    }

    async fn save_job(&self, releases: Vec<Release>, logical_now: Option<i64>) -> Result<()> {
        if releases.is_empty() {
            return Ok(());
        }
        let bytes = payload_bytes(&releases, releases.capacity());
        ensure!(bytes <= self.limits.max_job_bytes as usize, "native job exceeds max_job_bytes");
        let memory = self.memory.clone().acquire_many_owned(bytes as u32).await?;
        if let Some(helpers) = &self.helpers {
            return self.save_parts(helpers.clone(), releases, memory, logical_now).await;
        }
        let slot = self.slots.clone().acquire_owned().await?;
        let source = self.source.clone();
        let helpers = self.helpers.clone();
        // The blocking save owns admission, including while queued in Tokio. Cancelling its caller cannot free
        // memory/worker capacity while the payload or a transaction is still in flight.
        tokio::task::spawn_blocking(move || {
            let (_memory, _slot) = (memory, slot);
            if let Some(helpers) = helpers {
                return helpers.save(&releases, logical_now.unwrap_or_else(|| chrono::Utc::now().timestamp()));
            }
            source.with(|conn| {
                super::release_writer::save(
                    conn,
                    &releases,
                    logical_now.unwrap_or_else(|| chrono::Utc::now().timestamp()),
                )?;
                Ok(())
            })
        })
        .await
        .context("native PostgreSQL save task failed")?
        .context("saving releases to PostgreSQL")
    }
}

impl Native {
    /// Save `releases` on the helpers, its parts (`by_part`) side by side, each with its share of `memory`. The job
    /// fails if any part does.
    async fn save_parts(
        &self,
        helpers: Arc<Helpers>,
        releases: Vec<Release>,
        mut memory: tokio::sync::OwnedSemaphorePermit,
        logical_now: Option<i64>,
    ) -> Result<()> {
        let parts = by_part(releases);
        let count = parts.len();
        let mut saves = Vec::with_capacity(count);
        for (n, (key, releases)) in parts.into_iter().enumerate() {
            let bytes = payload_bytes(&releases, releases.capacity());
            // the last part keeps what is left of the permit, so the whole job's bytes stay held until all parts end
            let share = if n + 1 == count {
                std::mem::replace(&mut memory, self.memory.clone().try_acquire_many_owned(0).expect("zero permits"))
            } else {
                memory.split(bytes.min(memory.num_permits().saturating_sub(1))).unwrap_or_else(|| {
                    self.memory.clone().try_acquire_many_owned(0).expect("zero permits")
                })
            };
            saves.push(self.save_coalesced(helpers.clone(), key, releases, bytes, share, logical_now));
        }
        for outcome in join_all(saves).await {
            outcome?;
        }
        Ok(())
    }

    /// Save `releases` of `group` on the helpers, after the group's running save if there is one: a job arriving while
    /// one runs waits in the group's queue with its memory permit but no slot or session, and the running save's task
    /// saves the queue in merged batches (`next_batch`, `save_batch`, `drain`). The task owns the drain, so a caller
    /// going away never leaves a queue unserved; it returns once its own batch is saved.
    async fn save_coalesced(
        &self,
        helpers: Arc<Helpers>,
        group: String,
        releases: Vec<Release>,
        bytes: usize,
        memory: tokio::sync::OwnedSemaphorePermit,
        logical_now: Option<i64>,
    ) -> Result<()> {
        let now = logical_now.unwrap_or_else(|| chrono::Utc::now().timestamp());
        let (done, result) = tokio::sync::oneshot::channel();
        let waiting = Waiting { releases, logical: logical_now, now, bytes, _memory: memory, done };
        // queued behind a running save of the group
        let waiting = {
            let mut queues = self.queues.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match queues.get_mut(&group) {
                Some(queue) => {
                    queue.pending.push_back(waiting);
                    None
                }
                None => Some(waiting),
            }
        };
        let Some(waiting) = waiting else { return told(result.await) };
        let slot = self.slots.clone().acquire_owned().await?;
        // the group's queue starts with this job, unless another job of it started while this one waited for its slot
        let guard = {
            let mut queues = self.queues.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let started = queues.contains_key(&group);
            queues.entry(group.clone()).or_default().pending.push_back(waiting);
            (!started).then(|| LetGo { queues: self.queues.clone(), group: group.clone(), armed: true })
        };
        let Some(guard) = guard else {
            drop(slot);
            return told(result.await);
        };
        // nothing awaits between taking the group and starting its task; should the task never run, `guard` lets it go
        let handle = tokio::runtime::Handle::current();
        let slots = self.slots.clone();
        let on = handle.clone();
        drop(handle.spawn_blocking(move || drain(on, slots, helpers, guard, slot)));
        told(result.await)
    }
}

fn next_capacity(len: usize, capacity: usize) -> usize {
    if len < capacity { capacity } else { capacity.saturating_mul(2).max(1) }
}

fn article_heap_bytes(a: &Article) -> usize {
    [&a.subject, &a.author, &a.date, &a.message_id, &a.references]
        .into_iter()
        .map(String::capacity)
        .chain([&a.filename, &a.release_name].into_iter().flatten().map(String::capacity))
        .fold(0, usize::saturating_add)
}

/// Owned allocation capacity, rather than string lengths or claimed article sizes.
fn payload_bytes(releases: &[Release], capacity: usize) -> usize {
    let mut total = capacity.saturating_mul(std::mem::size_of::<Release>());
    let mut add = |n: usize| total = total.saturating_add(n);
    for r in releases {
        for s in [&r.name, &r.group, &r.poster, &r.date] {
            add(s.capacity());
        }
        for s in [&r.display_name, &r.collection].into_iter().flatten() {
            add(s.capacity());
        }
        add(r.articles.capacity().saturating_mul(std::mem::size_of::<Article>()));
        for a in &r.articles {
            add(article_heap_bytes(a));
        }
    }
    total.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    #[ignore="requires isolated PostgreSQL for original native resource ownership"]
    fn owner_save_permit_rejects_foreign_or_pooled_backend_and_checks_actual_capacity() {
        let schema=crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
        let runtime=crate::pg::stage_queue_tests::runtime(&schema);
        runtime.db.check_native_owner(&runtime.owner).unwrap();
        let foreign=Arc::new(super::super::indexer_runtime::Owner::new(postgres::Client::connect(&schema.url,postgres::NoTls).unwrap()));
        assert!(runtime.db.check_native_owner(&foreign).is_err());
        let pooled=Native::new(Arc::new(crate::pg::Pool::new(&crate::pg::StorageConfig {
            backend:crate::pg::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),allow_plaintext:true,..Default::default()
        }).unwrap()),PgWriteLimits::default()).unwrap();
        assert!(pooled.check_native_owner(&runtime.owner).is_err());
        let rt=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let permit=rt.block_on(runtime.db.reserve_native_for_owner(&runtime.owner)).unwrap();
        let valid=release("permit.example",'a');permit.check_batch(&valid).unwrap();
        let oversized=Vec::<Release>::with_capacity(permit.max_job_bytes()/std::mem::size_of::<Release>()+1);
        assert!(oversized.is_empty());assert!(permit.check_batch(&oversized).is_err(),"empty capacity still owns memory");
        permit.check_batch(&valid).unwrap();
        assert_eq!(runtime.owner.failure(),None);
        runtime.owner.with(|client| {let count:i64=client.query_one("select count(*) from releases",&[])?.get(0);assert_eq!(count,0);Ok(())}).unwrap();
        drop(permit);foreign.close();runtime.owner.close();
    }

    #[test]
    #[ignore="requires isolated PostgreSQL for shared native admission and stop"]
    fn owner_save_permit_shares_writer_slots_and_returns_partial_admission_on_cancel_or_stop() {
        use std::sync::atomic::Ordering;
        let schema=crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
        let runtime=crate::pg::stage_queue_tests::runtime(&schema);
        let limits=PgWriteLimits {max_pending_bytes:8192,max_job_bytes:4096,concurrent_saves:1};
        let native=Native::with_owner(runtime.owner.clone(),limits).unwrap();let clone=native.clone();
        let rt=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let held=native.reserve_native_for_owner(&runtime.owner).await.unwrap();
            assert_eq!(native.memory.available_permits(),4096);assert_eq!(native.slots.available_permits(),0);
            let mut ordinary=Box::pin(clone.save_job(release("shared.example",'b'),None));
            assert!(tokio::time::timeout(Duration::from_millis(50),&mut ordinary).await.is_err(),"ordinary writer must wait on the same held slot");
            assert!(native.memory.available_permits()<4096);drop(ordinary);assert_eq!(native.memory.available_permits(),4096);
            let mut canceled=Box::pin(clone.reserve_native_for_owner(&runtime.owner));
            assert!(tokio::time::timeout(Duration::from_millis(50),&mut canceled).await.is_err());
            assert_eq!(native.memory.available_permits(),0);drop(canceled);assert_eq!(native.memory.available_permits(),4096);
            let mut stopped=Box::pin(clone.reserve_native_for_owner(&runtime.owner));
            assert!(tokio::time::timeout(Duration::from_millis(50),&mut stopped).await.is_err());
            runtime.owner.stop.store(true,Ordering::Release);
            let error=match tokio::time::timeout(Duration::from_secs(1),stopped).await.unwrap() {Ok(_)=>panic!("stopped owner must not gain admission"),Err(error)=>error};
            assert!(crate::pg::outage::is_attempt_ended(&error));assert_eq!(native.memory.available_permits(),4096);
            assert!(held.check_batch(&release("stopped.example",'c')).is_err());
            assert_eq!(runtime.owner.failure(),None);drop(held);
            assert_eq!(native.memory.available_permits(),8192);assert_eq!(native.slots.available_permits(),1);
        });
        runtime.owner.close();
    }

    #[test]
    fn a_large_upload_is_saved_in_bounded_jobs_and_retries_do_not_requeue_it() {
        let Some(schema) = crate::pg::testing::Schema::native_writer() else { return };
        let pool = Arc::new(
            super::super::Pool::new(&super::super::StorageConfig {
                backend: super::super::Backend::Postgres,
                url: schema.url.clone(),
                schema: schema.name.clone(),
                allow_plaintext: true,
                ..Default::default()
            })
            .unwrap(),
        );
        let native =
            Native::new(pool, PgWriteLimits { max_pending_bytes: 4096, max_job_bytes: 4096, concurrent_saves: 2 })
                .unwrap();
        let releases = vec![Release {
            name: "Large.Release".into(),
            group: "example.test".into(),
            articles: (1..=40)
                .map(|part| Article {
                    subject: "x".repeat(128),
                    message_id: format!("<{part}@example.test>"),
                    filename: Some("file.rar".into()),
                    part: Some(part),
                    total_parts: Some(40),
                    bytes: 100,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }];
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(native.save_at(releases.clone(), 1234)).unwrap();
        let mut conn = schema.pool.get().unwrap();
        assert_eq!(
            conn.query_one("select touched_at from files", &[]).unwrap().get::<_, i64>(0),
            1234,
            "bounded pieces must keep their epoch's logical time"
        );
        let generation: i64 = conn.query_one("select generation from pp_queue", &[]).unwrap().get(0);
        rt.block_on(native.save_at(releases, 1234)).unwrap();
        assert_eq!(conn.query_one("select generation from pp_queue", &[]).unwrap().get::<_, i64>(0), generation);
        assert_eq!(conn.query_one("select count(*) from releases", &[]).unwrap().get::<_, i64>(0), 1);
        assert_eq!(conn.query_one("select count(*) from files", &[]).unwrap().get::<_, i64>(0), 1);
        assert_eq!(crate::pg::testing::articles(&mut *conn), 40);
        assert!(conn.query_one("select complete from files", &[]).unwrap().get::<_, bool>(0));
    }

    #[test]
    fn a_late_executable_prevents_earlier_pieces_from_becoming_visible() {
        let Some(schema) = crate::pg::testing::Schema::native_writer() else { return };
        let pool = Arc::new(
            super::super::Pool::new(&super::super::StorageConfig {
                backend: super::super::Backend::Postgres,
                url: schema.url.clone(),
                schema: schema.name.clone(),
                allow_plaintext: true,
                ..Default::default()
            })
            .unwrap(),
        );
        let native =
            Native::new(pool, PgWriteLimits { max_pending_bytes: 4096, max_job_bytes: 4096, concurrent_saves: 2 })
                .unwrap();
        let mut conn = schema.pool.get().unwrap();
        // A consumer must never observe the apparently clean prefix of an upload already known to contain a program.
        conn.batch_execute(
            "create function reject_exposure() returns trigger language plpgsql as $$
            begin raise exception 'unsafe upload became visible'; end $$;
            create trigger reject_exposure before insert on releases for each row execute function reject_exposure();",
        )
        .unwrap();
        let clean = Release {
            name: "Blocked.Release".into(),
            group: "example.test".into(),
            articles: (1..=40)
                .map(|part| Article {
                    subject: "x".repeat(128),
                    message_id: format!("<{part}@example.test>"),
                    filename: Some("file.rar".into()),
                    part: Some(part),
                    total_parts: Some(40),
                    bytes: 100,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let mut late = clean.clone();
        late.articles = vec![Article { filename: Some("setup.exe".into()), ..Default::default() }];
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(native.save(vec![clean.clone(), late])).unwrap();
        rt.block_on(native.save(vec![clean])).unwrap();
        assert_eq!(conn.query_one("select count(*) from releases", &[]).unwrap().get::<_, i64>(0), 0);
        assert_eq!(conn.query_one("select count(*) from release_writer_junk", &[]).unwrap().get::<_, i64>(0), 1);
    }

    fn release(group: &str, letter: char) -> Vec<Release> {
        vec![Release {
            name: letter.to_string().repeat(3000),
            group: group.into(),
            articles: vec![Article {
                message_id: format!("<{letter}@example.test>"),
                filename: Some("file.rar".into()),
                part: Some(1),
                total_parts: Some(1),
                bytes: 1,
                ..Default::default()
            }],
            ..Default::default()
        }]
    }

    #[test]
    fn cancelled_callers_do_not_release_inflight_admission() {
        let Some(schema) = crate::pg::testing::Schema::native_writer() else { return };
        let pool = Arc::new(
            super::super::Pool::new(&super::super::StorageConfig {
                backend: super::super::Backend::Postgres,
                url: schema.url.clone(),
                schema: schema.name.clone(),
                allow_plaintext: true,
                ..Default::default()
            })
            .unwrap(),
        );
        let limits = PgWriteLimits { max_pending_bytes: 4096, max_job_bytes: 4096, concurrent_saves: 2 };
        let native = Native::new(pool, limits).unwrap();
        let mut admin = schema.pool.get().unwrap();
        // Drop the lock before the runtime on a panic, so blocked blocking tasks can finish.
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let mut tx = admin.transaction().unwrap();
        tx.query_one("select pg_advisory_xact_lock(hashtextextended(current_schema() || ':first.example', 7936))", &[])
            .unwrap();
        let second = rt.block_on(async {
            let first = {
                let native = native.clone();
                tokio::spawn(async move { native.save(release("first.example", 'x')).await })
            };
            tokio::time::timeout(Duration::from_secs(5), async {
                while native.memory.available_permits() > 2048 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            first.abort();
            assert!(first.await.unwrap_err().is_cancelled());
            let mut second = {
                let native = native.clone();
                tokio::spawn(async move { native.save(release("other.example", 'y')).await })
            };
            assert!(
                tokio::time::timeout(Duration::from_millis(300), &mut second).await.is_err(),
                "another group must wait for the cancelled caller's in-flight payload"
            );
            second
        });
        tx.commit().unwrap();
        rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), second).await.unwrap().unwrap().unwrap();
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let count: i64 = admin.query_one("select count(*) from releases", &[]).unwrap().get(0);
            if count == 2 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "both admitted saves must finish");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
