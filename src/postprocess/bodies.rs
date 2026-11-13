//! Article bodies for steps: every budget and refusal in one place, soo a part
//! cant get the hard rules wrong on its own.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;

use crate::config::PostprocessConfig;
use crate::filters;
use crate::nntp::{BackgroundBody, BackgroundRead, NntpError, Pool, unless_stopped};

use super::rules::{IMAGE, PAR2};
use super::stats::Stats;
use super::step::{BodyRequest, Got, ReleaseInfo, StepError};

/// The refusal of a body past the release's share of this run, which a later run has again.
pub const PAST_BUDGET: &str = "past bodies_per_release";

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

    pub(super) fn service_io(&self)->(Arc<Pool>,tokio::runtime::Handle) {
        (self.pool.clone(),self.handle.clone())
    }

    /// `bodies_per_release`: what one release's run may fetch
    pub(crate) fn per_release(&self) -> i64 {
        self.config.read().unwrap().bodies_per_release
    }

    pub(crate) fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Why this body mustnt be fetched, all before any bytes move. Part 4's
    /// `fetch_prefix` asks too.
    /// A picture is refused unless `pictures`: a step that reads pictures on purpose asks for them
    /// (`StepCtx::allow_pictures`). Such a step only runs on a release outside the size bounds for a picture asked
    /// for on its page (`Step::asked_for`), soo its pictures aren't held to them.
    pub(crate) fn refusal(&self, release: &ReleaseInfo, file: &str, used: i64, pictures: bool) -> Option<&'static str> {
        let c = self.config.read().unwrap();
        if filters::group_blocked(&release.group) {
            Some("blocked group")
        } else if !(pictures && IMAGE.is_match(file))
            && ((release.size < c.body_min_size_bytes && !PAR2.is_match(file))
                || (c.body_max_size_bytes > 0 && release.size > c.body_max_size_bytes))
        {
            Some("outside the size bounds")
        } else if !pictures && IMAGE.is_match(file) {
            Some("an image")
        } else if used >= c.bodies_per_release {
            Some(PAST_BUDGET)
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

    #[cfg(test)]
    pub(crate) fn fetch(
        &self,
        release: &ReleaseInfo,
        request: &BodyRequest,
        used: &mut i64,
        deadline: Instant,
        read: BackgroundRead,
    ) -> Result<Got, StepError> {
        self.fetch_with(release, request, used, deadline, read, false)
    }

    /// `fetch`, with pictures let through when `pictures`.
    pub(crate) fn fetch_with(
        &self,
        release: &ReleaseInfo,
        request: &BodyRequest,
        used: &mut i64,
        deadline: Instant,
        read: BackgroundRead,
        pictures: bool,
    ) -> Result<Got, StepError> {
        if self.stopping() {
            return Err(StepError::Stopped);
        }
        if let Some(why) = self.refusal(release, &request.file, *used, pictures) {
            return Ok(Got::Refused(why));
        }
        let token_at = Instant::now();
        self.wait_for_token(deadline)?;
        let token_wait = token_at.elapsed();
        let Some(left) = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) else {
            return Err(StepError::OutOfBudget);
        };
        *used += 1;
        let max_bytes = {
            let cap = self.config.read().unwrap().body_max_bytes as usize;
            request.max_bytes.map_or(cap, |m| m.min(cap))
        };
        // a fetch slot is waited for until the run's deadline; with one, the body takes what its servers take, each
        // server its own time limit and retries (`Pool::background_read`), and the run's deadline doesnt cut it short
        let span = crate::metrics::span("stage.fetch_body");
        let asked = Instant::now();
        let fetched = self.handle.block_on(unless_stopped(&self.stop, async {
            let _permit = tokio::time::timeout(left, self.in_flight.acquire()).await?.expect("semaphore closed");
            Ok(self.pool.background_read(&request.message_id, max_bytes, read).await)
        }));
        if matches!(fetched, Some(Ok(Err(_))) | Some(Err(_))) {
            span.failed();
        }
        drop(span);
        let fetched = match fetched {
            Some(Err(tokio::time::error::Elapsed { .. })) => {
                return Err(StepError::Transient(no_slot(*used, asked, left, token_wait)));
            }
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

/// A body that got no fetch slot before the run's deadline: which of the run's bodies it was, how long before the
/// deadline it was asked, after how long waiting for a token of the budget, and how long it waited for a slot, soo a
/// run starved of slots is told from one whose time went before its body was asked (the budget, or the step's work).
fn no_slot(body: i64, asked: Instant, left: Duration, token_wait: Duration) -> String {
    let waited = Instant::now().saturating_duration_since(asked).as_secs_f64();
    format!(
        "no fetch slot before the run's deadline: body {body} of the run, asked {:.1} s before it after {:.1} s waiting \
         for a token, {waited:.1} s waiting for a slot",
        left.as_secs_f64(),
        token_wait.as_secs_f64()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_without_a_slot_says_where_its_time_went() {
        let asked = Instant::now() - Duration::from_secs(50);
        let said = no_slot(7, asked, Duration::from_millis(50_040), Duration::from_millis(1_200));
        let start = "no fetch slot before the run's deadline: body 7 of the run, asked 50.0 s before it after 1.2 s";
        assert!(said.starts_with(start), "{said}");
        assert!(said.contains(" waiting for a token, 50.0"), "{said}");
        let late = no_slot(1, Instant::now(), Duration::from_millis(300), Duration::ZERO);
        assert!(
            late.ends_with("asked 0.3 s before it after 0.0 s waiting for a token, 0.0 s waiting for a slot"),
            "{late}"
        );
    }

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
    fn a_prefix_read_refuses_a_blocked_group() {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        // no port answers, soo an attempt to fetch would come back as an error
        let f = fetcher(1, &runtime);
        // no real group name is this long, soo it counts as blocked
        let group = "a".repeat(10_000);
        let release = ReleaseInfo { group, size: 10, ..Default::default() };
        let ask = BodyRequest { message_id: "<a@x>".into(), file: "a.bin".into(), max_bytes: Some(64) };
        let got = f.fetch(&release, &ask, &mut 0, Instant::now() + Duration::from_secs(5), BackgroundRead::Prefix);
        assert_eq!(got.unwrap(), Got::Refused("blocked group"));
    }

    #[test]
    fn a_picture_is_refused_unless_the_step_asks_for_pictures() {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let f = fetcher(1, &runtime);
        let release = ReleaseInfo { group: "alt.binaries.t".into(), size: 4 << 30, ..Default::default() };
        assert_eq!(f.refusal(&release, "00-thumbnail.jpg", 0, false), Some("an image"));
        assert_eq!(f.refusal(&release, "00-thumbnail.jpg", 0, true), None);
        assert_eq!(f.refusal(&release, "Some.Film.mkv", 0, false), None);
    }

    #[test]
    fn a_body_takes_its_servers_time_not_the_runs_deadline() {
        // a server that takes the connection and says nothing
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let mut f = fetcher(port, &runtime);
        let pool = Pool::new(&[{
            let mut s = crate::config::UsenetServer::new("127.0.0.1", "u", "p", port);
            s.ssl = Some(false);
            s
        }])
        .with_timeout(Duration::from_millis(800));
        f.pool = Arc::new(pool);
        let release = ReleaseInfo { group: "alt.binaries.t".into(), size: 10, ..Default::default() };
        let ask = BodyRequest { message_id: "<a@x>".into(), file: "a.bin".into(), max_bytes: None };
        let mut used = 0;
        let start = Instant::now();
        let got = f.fetch(&release, &ask, &mut used, start + Duration::from_millis(300), BackgroundRead::Capped);
        assert!(matches!(got, Err(StepError::Transient(_))), "{got:?}");
        // the server's own time limit ended it, past the run's deadline
        assert!(start.elapsed() >= Duration::from_millis(700), "{:?}", start.elapsed());
        assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
        // nothing is started once the run's deadline has passed
        let got = f.fetch(&release, &ask, &mut used, Instant::now(), BackgroundRead::Capped);
        assert!(matches!(got, Err(StepError::OutOfBudget)), "{got:?}");
        drop(listener);
    }
}
