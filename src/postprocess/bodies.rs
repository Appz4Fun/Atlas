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
        let Some(left) = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) else {
            return Err(StepError::OutOfBudget);
        };
        *used += 1;
        let max_bytes = {
            let cap = self.config.read().unwrap().body_max_bytes as usize;
            request.max_bytes.map_or(cap, |m| m.min(cap))
        };
        // the run's deadline holds for the whole read: a slow server's timeouts are per read
        let fetched = self.handle.block_on(unless_stopped(&self.stop, async {
            tokio::time::timeout(left, async {
                let _permit = self.in_flight.acquire().await.expect("semaphore closed");
                self.pool.background_body(&request.message_id, max_bytes).await
            })
            .await
        }));
        let fetched = match fetched {
            Some(Err(_elapsed)) => return Err(StepError::Transient("a body took past the run's deadline".into())),
            Some(Ok(result)) => Some(result),
            None => None,
        };
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

    fn fetcher(port: u16, runtime: &tokio::runtime::Runtime) -> BodyFetcher {
        let mut server = crate::config::UsenetServer::new("127.0.0.1", "u", "p", port);
        server.ssl = Some(false);
        let config = PostprocessConfig { body_fetches_per_minute: 6000, body_min_size_bytes: 0, ..Default::default() };
        BodyFetcher::new(
            Arc::new(Pool::new(&[server])),
            runtime.handle().clone(),
            Arc::new(AtomicBool::new(false)),
            config,
            Arc::new(Stats::default()),
        )
    }

    #[test]
    fn a_body_stops_at_the_runs_deadline() {
        // a server that takes the connection and says nothing
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let f = fetcher(port, &runtime);
        let release = ReleaseInfo { group: "alt.binaries.t".into(), size: 10, ..Default::default() };
        let ask = BodyRequest { message_id: "<a@x>".into(), file: "a.bin".into(), max_bytes: None };
        let mut used = 0;
        let start = Instant::now();
        let got = f.fetch(&release, &ask, &mut used, start + Duration::from_millis(300));
        assert!(matches!(got, Err(StepError::Transient(_))), "{got:?}");
        assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
        // nothing is started once it has passed
        let got = f.fetch(&release, &ask, &mut used, Instant::now());
        assert!(matches!(got, Err(StepError::OutOfBudget)), "{got:?}");
        drop(listener);
    }
}
