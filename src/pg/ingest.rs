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
    pool: Arc<super::Pool>,
    memory: Arc<Semaphore>,
    slots: Arc<Semaphore>,
    limits: PgWriteLimits,
}

impl Native {
    pub fn new(pool: Arc<super::Pool>, limits: PgWriteLimits) -> Result<Self> {
        limits.check()?;
        Ok(Self {
            pool,
            memory: Arc::new(Semaphore::new(limits.max_pending_bytes as usize)),
            slots: Arc::new(Semaphore::new(usize::from(limits.concurrent_saves))),
            limits,
        })
    }

    /// Completion covers every bounded commit. On failure or cancellation, callers must replay the input before
    /// acknowledging its cursor/hours; the native writer deduplicates the pieces that already committed.
    pub async fn save(&self, releases: Vec<Release>) -> Result<()> {
        if payload_bytes(&releases, releases.capacity()) <= self.limits.max_job_bytes as usize {
            return self.save_job(releases).await;
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
                    self.add_piece(metadata, &mut batch, &mut batch_bytes).await?;
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
                    self.add_piece(piece, &mut batch, &mut batch_bytes).await?;
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
                self.add_piece(piece, &mut batch, &mut batch_bytes).await?;
            }
        }
        self.save_job(batch).await
    }

    async fn add_piece(&self, piece: Release, batch: &mut Vec<Release>, bytes: &mut usize) -> Result<()> {
        let owned = payload_bytes(std::slice::from_ref(&piece), 1) - std::mem::size_of::<Release>();
        let mut capacity = next_capacity(batch.len(), batch.capacity());
        let projected = |bytes: usize, old: usize, new: usize| {
            bytes.saturating_add(owned).saturating_add((new - old).saturating_mul(std::mem::size_of::<Release>()))
        };
        if projected(*bytes, batch.capacity(), capacity) > self.limits.max_job_bytes as usize && !batch.is_empty() {
            self.save_job(std::mem::take(batch)).await?;
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

    async fn save_job(&self, releases: Vec<Release>) -> Result<()> {
        if releases.is_empty() {
            return Ok(());
        }
        let bytes = payload_bytes(&releases, releases.capacity());
        ensure!(bytes <= self.limits.max_job_bytes as usize, "native job exceeds max_job_bytes");
        let memory = self.memory.clone().acquire_many_owned(bytes as u32).await?;
        let slot = self.slots.clone().acquire_owned().await?;
        let pool = self.pool.clone();
        // The blocking save owns admission, including while queued in Tokio. Cancelling its caller cannot free
        // memory/worker capacity while the payload or a transaction is still in flight.
        tokio::task::spawn_blocking(move || {
            let (_memory, _slot) = (memory, slot);
            let mut conn = pool.get()?;
            super::release_writer::save(&mut conn, &releases, chrono::Utc::now().timestamp())?;
            Ok::<_, anyhow::Error>(())
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
        rt.block_on(native.save(releases.clone())).unwrap();
        let mut conn = schema.pool.get().unwrap();
        let generation: i64 = conn.query_one("select generation from pp_queue", &[]).unwrap().get(0);
        rt.block_on(native.save(releases)).unwrap();
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
