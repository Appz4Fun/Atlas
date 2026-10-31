//! Article bodies for steps: every budget and refusal in one place, soo a part
//! cant get the hard rules wrong on its own.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;

use crate::config::PostprocessConfig;
use crate::filters;
use crate::nntp::{BackgroundBody, NntpError, Pool, unless_stopped};

use super::rules::IMAGE;
use super::stats::Stats;
use super::step::{BodyRequest, Got, ReleaseInfo, StepError};

/// Tokens by the clock, at most `cap`.
pub(crate) struct TokenBucket {
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    pub(crate) fn new() -> TokenBucket {
        TokenBucket { tokens: 1.0, last: Instant::now() }
    }

    /// Take one at `per_second` if there is one, else how long until there is.
    /// None means never: the rate is 0.
    pub(crate) fn take(&mut self, per_second: f64, cap: f64, now: Instant) -> Option<Duration> {
        if per_second <= 0.0 {
            return None;
        }
        self.tokens = (self.tokens + now.duration_since(self.last).as_secs_f64() * per_second).min(cap);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return Some(Duration::ZERO);
        }
        Some(Duration::from_secs_f64((1.0 - self.tokens) / per_second))
    }
}

pub struct BodyFetcher {
    pool: Arc<Pool>,
    handle: tokio::runtime::Handle,
    stop: Arc<AtomicBool>,
    config: RwLock<PostprocessConfig>,
    budget: Mutex<TokenBucket>,
    in_flight: Semaphore,
    stats: Arc<Stats>,
}

impl BodyFetcher {
    pub(crate) fn new(
        pool: Arc<Pool>,
        handle: tokio::runtime::Handle,
        stop: Arc<AtomicBool>,
        config: PostprocessConfig,
        stats: Arc<Stats>,
    ) -> BodyFetcher {
        let in_flight = Semaphore::new(config.body_connections.max(1));
        BodyFetcher {
            pool,
            handle,
            stop,
            config: RwLock::new(config),
            budget: Mutex::new(TokenBucket::new()),
            in_flight,
            stats,
        }
    }

    pub(crate) fn set_config(&self, config: &PostprocessConfig) {
        *self.config.write().unwrap() = config.clone();
    }

    pub(crate) fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Why this body mustnt be fetched, all before any bytes move. Part 4's
    /// `fetch_prefix` asks too.
    pub(crate) fn refusal(&self, release: &ReleaseInfo, file: &str, used: i64) -> Option<&'static str> {
        let c = self.config.read().unwrap();
        if filters::group_blocked(&release.group) {
            Some("blocked group")
        } else if release.size < c.body_min_size_bytes
            || (c.body_max_size_bytes > 0 && release.size > c.body_max_size_bytes)
        {
            Some("outside the size bounds")
        } else if IMAGE.is_match(file) {
            Some("an image")
        } else if used >= c.bodies_per_release {
            Some("past bodies_per_release")
        } else {
            None
        }
    }

    /// One token of the budget, waiting up to the run's deadline.
    pub(crate) fn wait_for_token(&self, deadline: Instant) -> Result<(), StepError> {
        loop {
            if self.stopping() {
                return Err(StepError::Stopped);
            }
            let per_minute = self.config.read().unwrap().body_fetches_per_minute;
            let per_second = per_minute as f64 / 60.0;
            let wait = self.budget.lock().unwrap().take(per_second, per_second.max(1.0), Instant::now());
            match wait {
                None => return Err(StepError::OutOfBudget),
                Some(w) if w.is_zero() => return Ok(()),
                Some(w) => {
                    if Instant::now() + w > deadline {
                        return Err(StepError::OutOfBudget);
                    }
                    std::thread::sleep(w.min(Duration::from_millis(200)));
                }
            }
        }
    }

    pub(crate) fn fetch(
        &self,
        release: &ReleaseInfo,
        request: &BodyRequest,
        used: &mut i64,
        deadline: Instant,
    ) -> Result<Got, StepError> {
        if self.stopping() {
            return Err(StepError::Stopped);
        }
        if let Some(why) = self.refusal(release, &request.file, *used) {
            return Ok(Got::Refused(why));
        }
        self.wait_for_token(deadline)?;
        *used += 1;
        let max_bytes = {
            let cap = self.config.read().unwrap().body_max_bytes as usize;
            request.max_bytes.map_or(cap, |m| m.min(cap))
        };
        let fetched = self.handle.block_on(unless_stopped(&self.stop, async {
            let _permit = self.in_flight.acquire().await.expect("semaphore closed");
            self.pool.background_body(&request.message_id, max_bytes).await
        }));
        match fetched {
            None => Err(StepError::Stopped),
            Some(Ok(BackgroundBody::Body(body))) => {
                self.stats.bodies.fetch_add(1, Ordering::Relaxed);
                self.stats.body_bytes.fetch_add(body.len() as u64, Ordering::Relaxed);
                Ok(Got::Body(body))
            }
            Some(Ok(BackgroundBody::Missing)) => Ok(Got::Missing),
            Some(Ok(BackgroundBody::TooBig)) => Ok(Got::TooBig),
            Some(Err(NntpError::Reply { code, message })) => Err(StepError::Transient(format!("{code} {message}"))),
            Some(Err(e)) => Err(StepError::Transient(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bucket_refills_by_the_clock() {
        let start = Instant::now();
        let mut b = TokenBucket::new();
        b.last = start;
        assert_eq!(b.take(1.0, 1.0, start), Some(Duration::ZERO));
        assert!(b.take(1.0, 1.0, start).unwrap() > Duration::from_millis(900));
        assert_eq!(b.take(1.0, 1.0, start + Duration::from_secs(2)), Some(Duration::ZERO));
        assert_eq!(b.take(0.0, 1.0, start), None, "no budget");
    }
}
