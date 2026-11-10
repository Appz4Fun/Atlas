//! Bounded native saves shared by indexer passes and the NXV linker.

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

    pub async fn save(&self, releases: Vec<Release>) -> Result<()> {
        if releases.is_empty() {
            return Ok(());
        }
        let bytes = payload_bytes(&releases, releases.capacity());
        ensure!(
            bytes <= self.limits.max_job_bytes as usize,
            "native save payload exceeds max_job_bytes; reduce the indexing request size"
        );
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
            for s in [&a.subject, &a.author, &a.date, &a.message_id, &a.references] {
                add(s.capacity());
            }
            for s in [&a.filename, &a.release_name].into_iter().flatten() {
                add(s.capacity());
            }
        }
    }
    total.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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
