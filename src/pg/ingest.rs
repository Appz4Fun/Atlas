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
    fn default() -> Self {
        Self { max_pending_bytes: 32 * 1024 * 1024, max_job_bytes: 8 * 1024 * 1024, concurrent_saves: 2 }
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
    memory: Arc<Semaphore>,
    slots: Arc<Semaphore>,
    limits: PgWriteLimits,
}

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
            Self::Owner(owner) => owner.with(work),
        }
    }
}

/// Admission from the original Native's shared memory/slot limits. This is
/// resource ownership only; the original Owner still validates every PG write.
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

    fn from_source(source: Source, limits: PgWriteLimits) -> Result<Self> {
        limits.check()?;
        Ok(Self {
            source,
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
        let slot = self.slots.clone().acquire_owned().await?;
        let source = self.source.clone();
        // The blocking save owns admission, including while queued in Tokio. Cancelling its caller cannot free
        // memory/worker capacity while the payload or a transaction is still in flight.
        tokio::task::spawn_blocking(move || {
            let (_memory, _slot) = (memory, slot);
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
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
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
        assert_eq!(conn.query_one("select count(*) from segments", &[]).unwrap().get::<_, i64>(0), 40);
        assert!(conn.query_one("select complete from files", &[]).unwrap().get::<_, bool>(0));
    }

    #[test]
    fn a_late_executable_prevents_earlier_pieces_from_becoming_visible() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
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
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
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
