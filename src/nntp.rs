//! Async NNTP client (tokio): just what atlas needs (GROUP, XOVER, BODY, LIST),
//! with a connection pool per usenet server soo many requests are in flight at once.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::task::Poll;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::runtime::Runtime;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;

use regex::Regex;

use crate::config::UsenetServer;
use crate::dates::to_iso_date;
use crate::parser::Article;

#[derive(Debug)]
pub enum NntpError {
    Io(io::Error),
    /// server answered with an unexpected status code
    Reply {
        code: u16,
        message: String,
    },
    Protocol(String),
    /// a `[COMPRESS=GZIP]` response that wouldnt inflate
    Decompress(String),
}

impl NntpError {
    pub fn code(&self) -> Option<u16> {
        match self {
            NntpError::Reply { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// an XOVER range with no articles in it: 423, or 420 ("No Articles
    /// Selected") from some providers. Not 430 ("Failed forwarding") or 403
    /// ("Operation failed"): those say the server couldn't answer, and a range
    /// taken for empty is never read again
    pub fn is_empty_range(&self) -> bool {
        matches!(self.code(), Some(420 | 423))
    }

    /// A read or the whole request took too long.
    pub fn is_timeout(&self) -> bool {
        matches!(self, NntpError::Io(e) if e.kind() == io::ErrorKind::TimedOut)
    }

    /// The server closed the connection: nothing came back on it.
    pub fn is_closed(&self) -> bool {
        matches!(self, NntpError::Io(e) if e.kind() == io::ErrorKind::UnexpectedEof)
    }

    /// "430 Failed forwarding": a reseller's front end couldnt ask its backend for the article just now. It says
    /// nothing about whether the article is there, unlike a plain 430 ("No Such Article").
    pub fn failed_forwarding(&self) -> bool {
        matches!(self, NntpError::Reply { code: 430, message } if message.to_lowercase().contains("forward"))
    }

    /// 4xx
    pub fn is_temporary(&self) -> bool {
        self.code().is_some_and(|c| (400..500).contains(&c))
    }

    /// 5xx
    pub fn is_permanent(&self) -> bool {
        self.code().is_some_and(|c| (500..600).contains(&c))
    }

    /// io/protocol errors leave the stream in an unknown state, the
    /// connection cant be reused after one
    pub fn breaks_connection(&self) -> bool {
        !matches!(self, NntpError::Reply { .. })
    }
}

impl fmt::Display for NntpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NntpError::Io(e) => write!(f, "{e}"),
            NntpError::Reply { code, message } => write!(f, "{code} {message}"),
            NntpError::Protocol(m) => write!(f, "protocol error: {m}"),
            NntpError::Decompress(m) => write!(f, "compressed headers: {m}"),
        }
    }
}

impl std::error::Error for NntpError {}

impl From<io::Error> for NntpError {
    fn from(e: io::Error) -> Self {
        NntpError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, NntpError>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overview {
    pub number: u64,
    pub subject: String,
    pub from: String,
    pub date: String,
    pub message_id: String,
    pub references: String,
    pub bytes: i64,
    pub lines: i64,
    /// the groups field 8 names when it is an `Xref:` line, which only a server with `Xref:full` sends
    pub xref_groups: Option<u16>,
}

impl Overview {
    pub fn into_article(self) -> Article {
        Article {
            number: self.number,
            subject: self.subject,
            author: self.from,
            date: to_iso_date(&self.date),
            message_id: self.message_id,
            references: self.references,
            bytes: if self.bytes > 0 { self.bytes } else { bytes_from_lines(self.lines) },
            lines: self.lines,
            xref_groups: self.xref_groups,
            ..Default::default()
        }
    }
}

/// An article's size when the server's overview gives none (some list `0` for every article of a group) but its line
/// count: a yEnc line is 128 encoded bytes and its CRLF. Sizes, size filters and NZB segments go by this.
pub fn bytes_from_lines(lines: i64) -> i64 {
    lines.max(0).saturating_mul(130)
}

pub fn headers_to_articles(headers: Vec<Overview>) -> Vec<Article> {
    headers.into_iter().map(Overview::into_article).collect()
}

/// Target wire ingestion rate: 750 Mbit/s = 93,750,000 bytes/sec
pub const RATE_750_MBIT: u64 = 93_750_000;
/// Default burst capacity for the rate limiter (10,000,000 bytes)
pub const DEFAULT_BURST_BYTES: u64 = 10_000_000;

/// An asynchronous Token Bucket rate limiter capping wire throughput.
///
/// Features:
/// 1. Deadlock elimination via chunked slicing: any request > burst_bytes is sliced into chunks of <= burst_bytes.
/// 2. Fractional nanosecond accumulation (nanos_remainder): eliminates token truncation over high-frequency reads.
/// 3. FIFO waiter sequencing (tokio::sync::Semaphore): eliminates thundering herd stampedes and starvation.
#[derive(Debug)]
pub struct TokenBucket {
    rate_bytes_per_sec: u64,
    burst_bytes: u64,
    state: Mutex<BucketState>,
    waiters: Semaphore,
}

pub type RateLimiter = TokenBucket;

#[derive(Debug)]
struct BucketState {
    tokens: u64,
    nanos_remainder: u64,
    last_update: Instant,
}

impl TokenBucket {
    /// Creates a new TokenBucket with the given refill rate in bytes/sec and burst capacity in bytes.
    /// If `rate_bytes_per_sec` is 0, the limiter is unconstrained (no throttling).
    /// Non-zero rate limiters normalize a 0-byte burst capacity to 1 byte so `acquire` never stalls.
    pub fn new(rate_bytes_per_sec: u64, burst_bytes: u64) -> Self {
        let burst_bytes = if rate_bytes_per_sec > 0 {
            burst_bytes.max(1)
        } else {
            burst_bytes
        };
        Self {
            rate_bytes_per_sec,
            burst_bytes,
            state: Mutex::new(BucketState {
                tokens: burst_bytes,
                nanos_remainder: 0,
                last_update: Instant::now(),
            }),
            waiters: Semaphore::new(1),
        }
    }

    /// Default 750 Mbit/s rate limiter with 10 MB burst buffer.
    pub fn default_750_mbit() -> Self {
        Self::new(RATE_750_MBIT, DEFAULT_BURST_BYTES)
    }

    pub fn rate_bytes_per_sec(&self) -> u64 {
        self.rate_bytes_per_sec
    }

    pub fn burst_bytes(&self) -> u64 {
        self.burst_bytes
    }

    /// Asynchronously acquires `bytes` tokens.
    /// If requested bytes exceed burst capacity, slices the request into `burst_bytes` chunks
    /// to eliminate deadlocks.
    pub async fn acquire(&self, mut bytes: usize) {
        if self.rate_bytes_per_sec == 0 || bytes == 0 {
            return;
        }
        let chunk_limit = (self.burst_bytes as usize).max(1);
        while bytes > 0 {
            let chunk = bytes.min(chunk_limit);
            self.acquire_chunk(chunk as u64).await;
            bytes -= chunk;
        }
    }

    async fn acquire_chunk(&self, chunk_bytes: u64) {
        // Enforce FIFO sequencing to eliminate thundering herd wake-up stampedes
        let _permit = self.waiters.acquire().await.expect("rate limiter closed");

        loop {
            let wait_duration = {
                let mut state = self.state.lock().unwrap();
                let now = Instant::now();
                let elapsed_nanos = now.duration_since(state.last_update).as_nanos() as u64;
                state.last_update = now;

                // Accumulate integer nanoseconds using u128 arithmetic to prevent overflow and truncation
                let total_nanos = (elapsed_nanos as u128)
                    .saturating_mul(self.rate_bytes_per_sec as u128)
                    .saturating_add(state.nanos_remainder as u128);
                let replenished = total_nanos / 1_000_000_000;
                state.nanos_remainder = (total_nanos % 1_000_000_000) as u64;
                let new_tokens = (state.tokens as u128).saturating_add(replenished);
                state.tokens = new_tokens.min(self.burst_bytes as u128) as u64;

                if state.tokens >= chunk_bytes {
                    state.tokens -= chunk_bytes;
                    return;
                }

                let deficit = chunk_bytes - state.tokens;
                // Calculate exact wait duration in nanoseconds rounded up
                let wait_nanos = ((deficit as u128) * 1_000_000_000).div_ceil(self.rate_bytes_per_sec as u128);
                Duration::from_nanos(wait_nanos as u64)
            };

            tokio::time::sleep(wait_duration).await;
        }
    }

    /// Checks available tokens after updating for elapsed time, without waiting.
    pub fn available_tokens(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        let now = Instant::now();
        let elapsed_nanos = now.duration_since(state.last_update).as_nanos() as u64;
        state.last_update = now;
        let total_nanos = (elapsed_nanos as u128)
            .saturating_mul(self.rate_bytes_per_sec as u128)
            .saturating_add(state.nanos_remainder as u128);
        let replenished = total_nanos / 1_000_000_000;
        state.nanos_remainder = (total_nanos % 1_000_000_000) as u64;
        let new_tokens = (state.tokens as u128).saturating_add(replenished);
        state.tokens = new_tokens.min(self.burst_bytes as u128) as u64;
        state.tokens
    }

    /// Try to acquire `bytes` tokens immediately if available.
    /// Returns true if tokens were subtracted, false if insufficient tokens.
    pub fn try_acquire(&self, bytes: usize) -> bool {
        if self.rate_bytes_per_sec == 0 || bytes == 0 {
            return true;
        }
        if bytes > self.burst_bytes as usize {
            return false;
        }
        let mut state = self.state.lock().unwrap();
        let now = Instant::now();
        let elapsed_nanos = now.duration_since(state.last_update).as_nanos() as u64;
        state.last_update = now;
        let total_nanos = (elapsed_nanos as u128)
            .saturating_mul(self.rate_bytes_per_sec as u128)
            .saturating_add(state.nanos_remainder as u128);
        let replenished = total_nanos / 1_000_000_000;
        state.nanos_remainder = (total_nanos % 1_000_000_000) as u64;
        let new_tokens = (state.tokens as u128).saturating_add(replenished);
        state.tokens = new_tokens.min(self.burst_bytes as u128) as u64;

        if state.tokens >= bytes as u64 {
            state.tokens -= bytes as u64;
            true
        } else {
            false
        }
    }

    /// Explicitly sets the token count (useful for unit testing replenishment).
    pub fn set_tokens(&self, tokens: u64) {
        let mut state = self.state.lock().unwrap();
        state.tokens = tokens.min(self.burst_bytes);
        state.nanos_remainder = 0;
        state.last_update = Instant::now();
    }
}

trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest one XOVER and one BODY may take in all. `DEFAULT_TIMEOUT` is per read, soo a server trickling a few
/// bytes now and then held a slice (or a body) for as long as it liked: on prod 28 of 78,000 XOVERs took over 5
/// minutes and one over 30. Past these the connection goes and the work is the next try's (a slice is read again
/// later, a body tries the next server).
const XOVER_DEADLINE: Duration = Duration::from_secs(300);
const BODY_DEADLINE: Duration = Duration::from_secs(60);
/// tries of a background body on one server past its first, after a timeout or a failure
pub const BODY_RETRIES: usize = 2;

/// The error for a command that ran past its deadline.
fn past_deadline(command: &str, deadline: Duration) -> NntpError {
    NntpError::Io(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("{command} took longer than {}s in all", deadline.as_secs_f64()),
    ))
}

/// `command`, or a timeout error once it ran past `deadline`.
async fn within<T>(deadline: Duration, what: &str, command: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(deadline, command).await.unwrap_or_else(|_| Err(past_deadline(what, deadline)))
}
/// smallest XOVER slice handed to one connection
const MIN_CHUNK: u64 = 250;
/// a server whose login was rejected `AUTH_REFUSALS` times in a row is left alone this long
const AUTH_REST: Duration = Duration::from_secs(30 * 60);
/// Rejected logins in a row before `AUTH_REST`; one alone waits `DOWN_FOR` like any failed connect. Some providers
/// answer a connection limit or a backend hiccup with the same 481/502 text as a bad password, and one such reply
/// benched the server for 30 minutes.
const AUTH_REFUSALS: usize = 2;
/// A plain connection that takes a compressed one's place (`Pool::uncompressed`) asks again this many times, this far
/// apart, while the provider still counts the closed one against its limit.
const PLAIN_TRIES: usize = 5;
const PLAIN_WAIT: Duration = Duration::from_millis(500);
/// A server whose compressed headers couldnt be read gets compressed ones asked for again after this long, on
/// probation: the first unreadable listing turns it off again. Compression stayed off until a restart.
const COMPRESS_RETRY: Duration = Duration::from_secs(60 * 60);

/// The longest an `XZVER` probe may take: where it hangs (as on Highwinds backends) a probe costs this, not a whole
/// `XOVER_DEADLINE`.
const XZVER_PROBE_DEADLINE: Duration = Duration::from_secs(30);
/// After a probe that said nothing either way (a dropped connection, a `400`, a wide slice that timed out), how long
/// before the next one, soo the slice's halves and the next listings dont each pay for one.
const XZVER_PROBE_COOLDOWN: Duration = Duration::from_secs(10 * 60);

/// What `XZVER` did on a server that refuses `XFEATURE COMPRESS GZIP`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Xzver {
    /// not asked yet: the next listing asks
    Untried,
    /// one listing is asking (see `Probe`)
    Probing,
    /// the last probe said nothing either way: the next waits until then (see `XZVER_PROBE_COOLDOWN`)
    Cooling(Instant),
    /// answered a listing: listings go on asking for it
    Works,
    /// refused, unreadable, or too slow at this time: plain XOVER until `COMPRESS_RETRY` has passed
    Refused(Instant),
}

/// What one listing does about `XZVER`, see `Pool::wants_xzver`.
enum Ask {
    No,
    Use,
    /// held for its `Drop`
    Probe(#[allow(dead_code)] Probe),
}

/// The one listing asking a server for `XZVER` while it is untried. A probe dropped without an answer either way
/// (a 4xx, a listing given up on) leaves it untried for the next listing.
struct Probe(Arc<Server>);

impl Drop for Probe {
    fn drop(&mut self) {
        let mut state = self.0.xzver.lock().unwrap();
        if *state == Xzver::Probing {
            *state = Xzver::Untried;
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    /// provider wont take another connection
    TooMany,
    /// wrong username / password
    Auth,
    Other,
}

/// A failure on `server`, by kind, and when it hit.
fn note_failure(server: &str, kind: &'static str) {
    use crate::metrics::names::{NNTP_FAILURES, NNTP_LAST_FAILURE};
    let labels = [("server", server), ("kind", kind)];
    NNTP_FAILURES.inc(&labels, 1);
    NNTP_LAST_FAILURE.set(&labels, chrono::Utc::now().timestamp() as f64);
}

/// Every kind `atlas_nntp_failures_total` has, soo each series is there before its first failure.
pub const FAILURE_KINDS: [&str; 13] = [
    "timeout",
    "connection_closed",
    "io",
    "protocol",
    "decompress",
    "no_such_article",
    "no_such_group",
    "too_many_connections",
    "auth_rejected",
    "reply_4xx",
    "reply_5xx",
    "reply_other",
    "skipped_while_resting",
];

/// What kind of failure `e` is, for `atlas_nntp_failures_total`.
fn failure_kind(e: &NntpError) -> &'static str {
    match e {
        NntpError::Io(e) if e.kind() == io::ErrorKind::TimedOut => "timeout",
        NntpError::Io(e) if e.kind() == io::ErrorKind::UnexpectedEof => "connection_closed",
        NntpError::Io(_) => "io",
        NntpError::Protocol(_) => "protocol",
        NntpError::Decompress(_) => "decompress",
        NntpError::Reply { code: 420 | 423 | 430, .. } => "no_such_article",
        NntpError::Reply { code: 411, .. } => "no_such_group",
        NntpError::Reply { .. } if refusal(e) == Refusal::TooMany => "too_many_connections",
        NntpError::Reply { .. } if refusal(e) == Refusal::Auth => "auth_rejected",
        NntpError::Reply { code, .. } if (400..500).contains(code) => "reply_4xx",
        NntpError::Reply { code, .. } if (500..600).contains(code) => "reply_5xx",
        NntpError::Reply { .. } => "reply_other",
    }
}

/// Why a connect/login failed, from the reply text (codes overlap: some
/// providers use 481/502 for both a bad login and the connection limit).
fn refusal(e: &NntpError) -> Refusal {
    let NntpError::Reply { message, .. } = e else { return Refusal::Other };
    let m = message.to_lowercase();

    if ["too many", "connection limit", "number of connections", "max connections", "maximum connections"]
        .iter()
        .any(|k| m.contains(k))
    {
        Refusal::TooMany
    } else if ["auth", "denied", "login", "password", "credential", "not authorized", "unauthorized"]
        .iter()
        .any(|k| m.contains(k))
    {
        Refusal::Auth
    } else {
        Refusal::Other
    }
}

/// Which bounded read `Pool::fetch_capped` makes.
#[derive(Clone, Copy)]
enum Capped {
    Head,
    Body,
}

/// Whether a reply says the server hasn't the article: 423, or a 430 that isn't a failed forward.
pub fn says_absent(e: &NntpError) -> bool {
    matches!(e.code(), Some(423 | 430)) && !e.failed_forwarding()
}

/// The error worth reporting after one more server failed: one that may pass (a timeout, a failed forward, a
/// dropped connection) is kept over a server saying it hasn't the article, which another server may still have.
fn keep_worse(kept: &mut Option<NntpError>, e: NntpError) {
    if !(says_absent(&e) && kept.as_ref().is_some_and(|k| !says_absent(k))) {
        *kept = Some(e);
    }
}

/// A GROUP reply that says the server lacks the group: 411, or 500/501 from a
/// server without GROUP. Not 403 or 430, which say it couldnt answer right now.
pub fn carries_not(e: &NntpError) -> bool {
    matches!(e.code(), Some(411 | 500 | 501))
}

/// A group's home other than its first choice (see `Pool::select_group_known`): the server it was found on, since
/// when, and the home it left for a while, one that had indexed it before (asked again after `HOME_RETRY`).
#[derive(Clone, Copy, Debug)]
struct Home {
    server: usize,
    since: Instant,
    back: Option<usize>,
}

/// How long a group stays on the server it fell over to before its first choice, which indexed it before, is asked
/// again: a provider can answer 411 for a while (a backend in trouble), and a group that never moved back kept its
/// live indexing on another provider until the next restart.
const HOME_RETRY: Duration = Duration::from_secs(30 * 60);
/// GROUPs in a row a server that indexed a group before has to say it doesnt carry it before the group moves
const MOVE_AFTER: u32 = 3;

/// how long to wait for the end of a compressed stream once its last line arrived
const STREAM_END_WAIT: Duration = Duration::from_secs(2);
/// after a failed connect a server is skipped for this long (connect() still tries it)
const DOWN_FOR: Duration = Duration::from_secs(60);
/// A server's connection limit lowered by "too many connections" climbs back one connection at a time towards what
/// config.toml gives it, the first after this long; each refusal on the way doubles the wait (up to
/// `LIMIT_HEAL_MAX`), and reaching the configured count starts it over. A provider refuses for a while (another app on
/// the account, a reconnect storm), and the limit only ever shrank, until a restart.
const LIMIT_HEAL: Duration = Duration::from_secs(30);
const LIMIT_HEAL_MAX: Duration = Duration::from_secs(15 * 60);

/// When a lowered connection limit next goes up one, and how long the step after that waits (see `LIMIT_HEAL`).
#[derive(Debug)]
struct Heal {
    next: Option<Instant>,
    every: Duration,
    /// a step up was taken since the last refusal: one now says that step was too far
    climbed: bool,
}
/// An idle connection older than this is closed rather than used again: providers drop idle connections (frugal says
/// "400 Idle timeout"), and on prod passes failed with "connection closed" on them, hundreds of times an hour.
const STALE_IDLE: Duration = Duration::from_secs(30);

fn is_stale(idle_since: Option<Instant>) -> bool {
    idle_since.is_some_and(|t| t.elapsed() >= STALE_IDLE)
}
/// GROUPs refused with 500/501 in a row before a server counts as article only
const GROUP_REFUSALS: usize = 3;
/// A server taken for article only is tried for indexing again after this long, and twice as long each time it
/// turns out article only again (up to `ARTICLE_ONLY_MAX`): a provider can refuse GROUP for a while, and the
/// server stayed out of indexing until a restart.
const ARTICLE_ONLY_REST: Duration = Duration::from_secs(30 * 60);
const ARTICLE_ONLY_MAX: Duration = Duration::from_secs(12 * 3600);
/// how often a request in flight looks at the stop flag
const STOP_POLL: Duration = Duration::from_millis(50);
/// fetched slices a pass lets wait for it to save them, besides the ones its
/// connections hold (the unsaved budget bounds them all, this keeps one pass
/// from taking a big share of it)
const SLICES_BUFFERED: usize = 4;
/// slices one pass holds room for at once, fetching, waiting for it or being
/// parsed: without it a pass on a server of many connections takes the whole
/// unsaved budget in one burst and every other pass waits while it saves
/// them one by one
pub const STREAM_SLICES: usize = 2 * SLICES_BUFFERED;

/// `fut`'s output, or None as soon as `stop` is set: stopping doesnt wait on
/// a provider that went quiet mid reply. Dropping a request part way is safe,
/// its lease throws the connection away instead of reusing it.
pub async fn unless_stopped<F: Future>(stop: &AtomicBool, fut: F) -> Option<F::Output> {
    let mut fut = std::pin::pin!(fut);
    let mut tick = tokio::time::interval(STOP_POLL);

    std::future::poll_fn(|cx| {
        // a reply that made it is kept even when stop came in at the same time
        if let Poll::Ready(out) = fut.as_mut().poll(cx) {
            return Poll::Ready(Some(out));
        }
        if stop.load(Ordering::Relaxed) {
            return Poll::Ready(None);
        }
        while tick.poll_tick(cx).is_ready() {}
        Poll::Pending
    })
    .await
}

fn timed_out() -> NntpError {
    NntpError::Io(io::Error::new(io::ErrorKind::TimedOut, "timed out"))
}

async fn with_timeout<T, F>(limit: Duration, fut: F) -> Result<T>
where
    F: Future<Output = io::Result<T>>,
{
    match tokio::time::timeout(limit, fut).await {
        Ok(r) => r.map_err(NntpError::Io),
        Err(_) => Err(timed_out()),
    }
}

/// What a date search keeps of an XOVER listing: its first and its last
/// article with a post date, as (number, post time).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ends {
    pub first: Option<Dated>,
    pub last: Option<Dated>,
    /// rows read, and the highest number among them
    pub rows: usize,
    pub read_to: Option<u64>,
    /// every row was read (not cut short at the row limit)
    pub whole: bool,
}

impl Ends {
    /// nothing in the range
    fn empty() -> Ends {
        Ends { whole: true, ..Ends::default() }
    }

    /// fold in one overview row
    fn add(&mut self, row: &[u8]) {
        self.rows += 1;
        let mut fields = row.split(|b| *b == b'\t');
        let Some(number) = fields.next().and_then(|f| std::str::from_utf8(f).ok()?.trim().parse::<u64>().ok()) else {
            return;
        };
        self.read_to = self.read_to.max(Some(number));
        let Some(t) = fields.nth(2).and_then(|d| plausible_post_time(&String::from_utf8_lossy(d))) else {
            return;
        };
        if self.first.is_none_or(|f| number < f.0) {
            self.first = Some((number, t));
        }
        if self.last.is_none_or(|l| number > l.0) {
            self.last = Some((number, t));
        }
    }
}

/// One logged in connection to one server.
pub struct Conn {
    io: BufReader<Box<dyn AsyncStream>>,
    timeout: Duration,
    /// group selected on this connection, XOVER needs one
    pub group: Option<String>,
    /// server agreed to XFEATURE COMPRESS GZIP on this connection
    pub compressed: bool,
    /// compression was asked for and the server said no: `XZVER` may stand in (see `Pool::wants_xzver`)
    pub gzip_refused: bool,
    /// set when a compressed response timed out waiting for its stream end trailer, or the connection is a plain one
    /// standing in for a compressed one (`Pool::uncompressed`): never kept idle
    poisoned: bool,
    /// bytes received off the socket, and the same after decompression,
    /// since the pool last collected them
    wire: u64,
    text: u64,
    /// bytes written, for the command being measured
    sent: u64,
    /// the server's key, for metrics
    server: Arc<str>,
    rate_limiter: Arc<TokenBucket>,
    /// when it went back to the pool unused, see `STALE_IDLE`
    idle_since: Option<Instant>,
}

/// One command being measured, see `Conn::measure`.
struct Measured {
    command: &'static str,
    function: &'static str,
    started: Instant,
    wire: u64,
    sent: u64,
    _span: crate::metrics::Span,
}

impl Conn {
    fn measure(&self, command: &'static str, function: &'static str) -> Measured {
        Measured {
            command,
            function,
            started: Instant::now(),
            wire: self.wire,
            sent: self.sent,
            _span: crate::metrics::span(function),
        }
    }

    fn measured<T>(&self, m: Measured, r: &Result<T>) {
        use crate::metrics::names::{NNTP_BYTES, NNTP_COMMAND_SECONDS, NNTP_COMMANDS};
        let server = &*self.server;
        let result = match r {
            Ok(_) => "ok",
            Err(e) if e.is_empty_range() || e.code() == Some(430) || e.code() == Some(411) => "not_found",
            Err(_) => {
                m._span.failed();
                "error"
            }
        };
        let (received, sent) = (self.wire - m.wire, self.sent - m.sent);
        NNTP_COMMANDS.inc(&[("server", server), ("command", m.command), ("result", result)], 1);
        NNTP_COMMAND_SECONDS.observe(&[("server", server), ("command", m.command)], m.started.elapsed().as_secs_f64());
        NNTP_BYTES.inc(&[("server", server), ("command", m.command), ("direction", "received")], received);
        NNTP_BYTES.inc(&[("server", server), ("command", m.command), ("direction", "sent")], sent);
        crate::metrics::function_bytes(m.function, "received", received);
        crate::metrics::function_bytes(m.function, "sent", sent);
    }

    /// Connect and log in. With `compress` it also asks for gzip compressed
    /// header listings (XFEATURE COMPRESS GZIP), which servers may refuse.
    pub async fn open(server: &UsenetServer, timeout: Duration, compress: bool) -> Result<Conn> {
        let host = server.host.trim().trim_matches(['[', ']']).to_string();
        let tcp = with_timeout(timeout, TcpStream::connect((host.as_str(), server.port))).await?;
        let _ = tcp.set_nodelay(true);

        let stream: Box<dyn AsyncStream> = if server.use_ssl() {
            let name = rustls::pki_types::ServerName::try_from(host.clone())
                .map_err(|e| NntpError::Protocol(format!("bad host name: {e}")))?;
            let connector = tokio_rustls::TlsConnector::from(tls_config());
            Box::new(with_timeout(timeout, connector.connect(name, tcp)).await?)
        } else {
            Box::new(tcp)
        };

        let mut conn = Conn::over(stream, timeout);
        conn.server = server_key(server).into();
        let measure = conn.measure("LOGIN", "nntp.login");
        let r = conn.log_in(server, compress).await;
        conn.measured(measure, &r);
        r?;
        Ok(conn)
    }

    pub async fn open_with_limiter(
        server: &UsenetServer,
        timeout: Duration,
        compress: bool,
        limiter: Arc<TokenBucket>,
    ) -> Result<Conn> {
        let mut conn = Conn::open(server, timeout, compress).await?;
        conn.rate_limiter = limiter;
        Ok(conn)
    }

    pub fn rate_limiter(&self) -> &Arc<TokenBucket> {
        &self.rate_limiter
    }

    pub fn set_rate_limiter(&mut self, limiter: Arc<TokenBucket>) {
        self.rate_limiter = limiter;
    }

    pub fn with_rate_limiter(mut self, limiter: Arc<TokenBucket>) -> Self {
        self.rate_limiter = limiter;
        self
    }

    /// The greeting, the login, and asking for compression.
    async fn log_in(&mut self, server: &UsenetServer, compress: bool) -> Result<()> {
        let conn = self;
        let (code, message) = conn.read_status().await?;
        if code != 200 && code != 201 {
            return Err(NntpError::Reply { code, message });
        }

        if !server.username.is_empty() {
            let (code, message) = conn.command("AUTHINFO USER", Some(&server.username)).await?;
            let (code, message) = match code {
                381 => conn.command("AUTHINFO PASS", Some(&server.password)).await?,
                _ => (code, message),
            };

            if code != 281 {
                return Err(NntpError::Reply { code, message });
            }
        }

        if compress {
            let (code, _) = conn.command("XFEATURE COMPRESS GZIP", None).await?;
            conn.compressed = code == 290;
            conn.gzip_refused = code != 290;
        }

        Ok(())
    }

    fn over(stream: Box<dyn AsyncStream>, timeout: Duration) -> Conn {
        Conn {
            io: BufReader::with_capacity(256 * 1024, stream),
            timeout,
            group: None,
            compressed: false,
            gzip_refused: false,
            poisoned: false,
            wire: 0,
            text: 0,
            sent: 0,
            server: Arc::from(""),
            rate_limiter: Arc::new(TokenBucket::new(0, 0)),
            idle_since: None,
        }
    }

    /// Next chunk of raw bytes from the socket.
    async fn read_chunk(&mut self, limit: Duration) -> Result<Vec<u8>> {
        let io = &mut self.io;
        let chunk = with_timeout(limit, async move { Ok(io.fill_buf().await?.to_vec()) }).await?;

        if chunk.is_empty() {
            return Err(NntpError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed")));
        }

        self.rate_limiter.acquire(chunk.len()).await;
        self.io.consume(chunk.len());
        self.wire += chunk.len() as u64;
        Ok(chunk)
    }

    /// Body of a `[COMPRESS=GZIP]` multi-line response: a zlib (or gzip)
    /// stream holding the usual dot terminated, dot stuffed lines, handed to
    /// `line` one at a time (see `each_line`).
    async fn each_compressed_line(&mut self, mut line: impl FnMut(Vec<u8>) -> bool) -> Result<bool> {
        let mut inflater = Inflater::default();
        // inflated text not handed out yet: lines go as soon as they are
        // whole, soo it holds about one read's worth
        let mut text = Vec::new();
        let mut terminated = false;

        loop {
            if !terminated {
                let mut scanned = 0;
                while let Some(nl) = text[scanned..].iter().position(|b| *b == b'\n') {
                    let mut l = text[scanned..scanned + nl].to_vec();
                    scanned += nl + 1;

                    if l.last() == Some(&b'\r') {
                        l.pop();
                    }

                    if l == b"." {
                        terminated = true;
                        break;
                    }

                    if l.starts_with(b"..") {
                        l.remove(0);
                    }

                    self.text += l.len() as u64 + 2;
                    if !line(l) {
                        return Ok(false);
                    }
                }
                text.drain(..scanned);
            }

            // done once the terminator showed up and the compressed stream is fully read,
            // soo no stray trailer bytes are left for the next command to trip over
            if terminated && inflater.finished() {
                return Ok(true);
            }

            let chunk = if terminated {
                // all lines are in, only the end of the compressed stream is missing.
                // a server that never closes the stream properly sends nothing more
                match self.read_chunk(STREAM_END_WAIT).await {
                    Err(NntpError::Io(e)) if e.kind() == io::ErrorKind::TimedOut => {
                        self.poisoned = true;
                        return Ok(true);
                    }
                    r => r?,
                }
            } else {
                self.read_chunk(self.timeout).await?
            };
            let plain = inflater.feed(&chunk, &mut text)?;

            // TERMINATOR style servers send ".\r\n" uncompressed after the stream
            if !terminated {
                text.extend_from_slice(&plain);
            }
        }
    }

    async fn send_line(&mut self, line: &str) -> Result<()> {
        if line.contains(['\r', '\n']) {
            return Err(NntpError::Protocol("newline in command".into()));
        }

        self.sent += line.len() as u64 + 2;
        let mut buf = Vec::with_capacity(line.len() + 2);
        buf.extend_from_slice(line.as_bytes());
        buf.extend_from_slice(b"\r\n");

        let s = self.io.get_mut();
        with_timeout(self.timeout, async {
            s.write_all(&buf).await?;
            s.flush().await
        })
        .await
    }

    async fn read_raw_line(&mut self) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        let n = with_timeout(self.timeout, self.io.read_until(b'\n', &mut buf)).await?;
        self.rate_limiter.acquire(n).await;
        self.wire += n as u64;
        self.text += n as u64;

        if n == 0 {
            return Err(NntpError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed")));
        }

        while buf.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
            buf.pop();
        }

        Ok(buf)
    }

    async fn read_status(&mut self) -> Result<(u16, String)> {
        let line = self.read_raw_line().await?;
        let line = String::from_utf8_lossy(&line);
        let (code, message) = line.split_once(' ').unwrap_or((&line, ""));

        let code = code.parse::<u16>().map_err(|_| NntpError::Protocol(format!("bad status line: {line}")))?;

        Ok((code, message.to_string()))
    }

    /// One line of at most about `max` bytes. None: past that, with at most one
    /// read's worth more off the socket and the rest left unread, soo the
    /// connection cant take another request.
    async fn read_line_capped(&mut self, max: usize) -> Result<Option<Vec<u8>>> {
        let mut buf = Vec::new();
        loop {
            let (used, ended) = {
                let chunk = with_timeout(self.timeout, self.io.fill_buf()).await?;
                if chunk.is_empty() {
                    return Err(NntpError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed")));
                }
                match chunk.iter().position(|b| *b == b'\n') {
                    Some(at) => {
                        buf.extend_from_slice(&chunk[..=at]);
                        (at + 1, true)
                    }
                    None => {
                        buf.extend_from_slice(chunk);
                        (chunk.len(), false)
                    }
                }
            };
            self.rate_limiter.acquire(used).await;
            self.io.consume(used);
            self.wire += used as u64;
            self.text += used as u64;
            if ended {
                while buf.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
                    buf.pop();
                }
                return Ok(Some(buf));
            }
            if buf.len() > max {
                return Ok(None);
            }
        }
    }

    /// `body` with a bound. None: over `max_bytes`, or more than one line per
    /// 16 of them. At most `max_bytes` + 64 KiB off the wire, one long line included.
    pub async fn body_capped(&mut self, message_id: &str, max_bytes: usize) -> Result<Option<Vec<u8>>> {
        let measure = self.measure("BODY", "nntp.body_capped");
        let r = self.body_capped_unmeasured(message_id, max_bytes).await;
        self.measured(measure, &r);
        r
    }

    async fn body_capped_unmeasured(&mut self, message_id: &str, max_bytes: usize) -> Result<Option<Vec<u8>>> {
        let (code, message) = self.command("BODY", Some(message_id)).await?;
        if code != 222 {
            return Err(NntpError::Reply { code, message });
        }
        // only listings get compressed, and a compressed one cant be bounded by its lines
        if message.contains("[COMPRESS=GZIP]") {
            return Ok(None);
        }
        let (mut lines, mut read) = (Vec::new(), 0usize);
        loop {
            let Some(mut l) = self.read_line_capped(max_bytes).await? else { return Ok(None) };
            if l == b"." {
                break;
            }
            if l.starts_with(b"..") {
                l.remove(0);
            }
            read += l.len();
            lines.push(l);
            if read > max_bytes || lines.len() > max_bytes / 16 + 1 {
                return Ok(None);
            }
        }
        Ok(Some(yenc_decode(&lines).unwrap_or_else(|| lines.join(&b'\n'))))
    }

    /// ARTICLE, undecoded: headers, a blank line, then the body, lines ending in `\n`. None: past `max_bytes`,
    /// with the rest left unread.
    pub async fn article_raw(&mut self, message_id: &str, max_bytes: usize) -> Result<Option<Vec<u8>>> {
        let (code, message) = self.command("ARTICLE", Some(message_id)).await?;
        if code != 220 {
            return Err(NntpError::Reply { code, message });
        }
        let mut out = Vec::new();
        loop {
            let Some(mut l) = self.read_line_capped(max_bytes).await? else { return Ok(None) };
            if l == b"." {
                return Ok(Some(out));
            }
            if l.starts_with(b"..") {
                l.remove(0);
            }
            out.extend_from_slice(&l);
            out.push(b'\n');
            if out.len() > max_bytes {
                return Ok(None);
            }
        }
    }

    /// `article_raw`'s first `max_bytes` or so, in whole lines. False: the rest is left unread, soo the connection
    /// cant take another request.
    pub async fn article_raw_prefix(&mut self, message_id: &str, max_bytes: usize) -> Result<(Vec<u8>, bool)> {
        let (code, message) = self.command("ARTICLE", Some(message_id)).await?;
        if code != 220 {
            return Err(NntpError::Reply { code, message });
        }
        let mut out = Vec::new();
        while out.len() < max_bytes {
            let Some(mut l) = self.read_line_capped(max_bytes).await? else { break };
            if l == b"." {
                return Ok((out, true));
            }
            if l.starts_with(b"..") {
                l.remove(0);
            }
            out.extend_from_slice(&l);
            out.push(b'\n');
        }
        Ok((out, false))
    }

    /// HEAD: the headers, lines ending in `\n`, and nothing of the body. None: past `max_bytes`, with the rest left
    /// unread.
    pub async fn head(&mut self, message_id: &str, max_bytes: usize) -> Result<Option<Vec<u8>>> {
        let (code, message) = self.command("HEAD", Some(message_id)).await?;
        if code != 221 {
            return Err(NntpError::Reply { code, message });
        }
        let mut out = Vec::new();
        loop {
            let Some(mut l) = self.read_line_capped(max_bytes).await? else { return Ok(None) };
            if l == b"." {
                return Ok(Some(out));
            }
            if l.starts_with(b"..") {
                l.remove(0);
            }
            out.extend_from_slice(&l);
            out.push(b'\n');
            if out.len() > max_bytes {
                return Ok(None);
            }
        }
    }

    /// Whether a server has the article, with nothing transferred.
    pub async fn stat(&mut self, message_id: &str) -> Result<()> {
        let (code, message) = self.command("STAT", Some(message_id)).await?;
        if code == 223 { Ok(()) } else { Err(NntpError::Reply { code, message }) }
    }

    /// BODY's first `max_bytes` or so, decoded. False: the rest is left unread, soo the
    /// connection cant take another request. Line ends count: an article of empty
    /// lines stops at the cap too, with at most `max_bytes / 2` lines kept.
    pub async fn body_prefix(&mut self, message_id: &str, max_bytes: usize) -> Result<(Vec<u8>, bool)> {
        let measure = self.measure("BODY", "nntp.body_prefix");
        let r = self.body_prefix_unmeasured(message_id, max_bytes).await;
        self.measured(measure, &r);
        r
    }

    async fn body_prefix_unmeasured(&mut self, message_id: &str, max_bytes: usize) -> Result<(Vec<u8>, bool)> {
        let (code, message) = self.command("BODY", Some(message_id)).await?;
        if code != 222 {
            return Err(NntpError::Reply { code, message });
        }
        if message.contains("[COMPRESS=GZIP]") {
            return Ok((Vec::new(), false));
        }
        let (mut lines, mut read, mut whole) = (Vec::new(), 0usize, false);
        while read < max_bytes {
            let Some(mut l) = self.read_line_capped(max_bytes).await? else { break };
            if l == b"." {
                whole = true;
                break;
            }
            if l.starts_with(b"..") {
                l.remove(0);
            }
            read += l.len() + 2;
            lines.push(l);
        }
        Ok((yenc_decode(&lines).unwrap_or_else(|| lines.join(&b'\n')), whole))
    }

    /// A plain multi-line response, handed to `line` one at a time with
    /// dot-stuffing removed (see `each_line`).
    async fn each_plain_line(&mut self, mut line: impl FnMut(Vec<u8>) -> bool) -> Result<bool> {
        loop {
            let mut l = self.read_raw_line().await?;

            if l == b"." {
                return Ok(true);
            }

            if l.starts_with(b"..") {
                l.remove(0);
            }

            if !line(l) {
                return Ok(false);
            }
        }
    }

    pub async fn command(&mut self, verb: &str, args: Option<&str>) -> Result<(u16, String)> {
        match args {
            Some(a) if !a.is_empty() => self.send_line(&format!("{verb} {a}")).await?,
            _ => self.send_line(verb).await?,
        }

        self.read_status().await
    }

    pub async fn quit(&mut self) {
        if self.send_line("QUIT").await.is_ok() {
            let _ = self.read_status().await;
        }
    }

    /// GROUP -> (count, first, last, name)
    pub async fn select_group(&mut self, group: &str) -> Result<(u64, u64, u64, String)> {
        let measure = self.measure("GROUP", "nntp.group");
        let r = self.select_group_unmeasured(group).await;
        self.measured(measure, &r);
        r
    }

    async fn select_group_unmeasured(&mut self, group: &str) -> Result<(u64, u64, u64, String)> {
        let (code, message) = self.command("GROUP", Some(group)).await?;

        if code != 211 {
            self.group = None;
            return Err(NntpError::Reply { code, message });
        }

        self.group = Some(group.to_string());

        let parts: Vec<&str> = message.split_whitespace().collect();
        // numbers aint always there soo dont crash on em
        let num = |i: usize| parts.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        let name = parts.get(3).map(|s| s.to_string()).unwrap_or_else(|| group.to_string());

        Ok((num(0), num(1), num(2), name))
    }

    /// XOVER pulls a whole header range in one shot
    pub async fn xover(&mut self, start: u64, end: u64) -> Result<Vec<Overview>> {
        let measure = self.measure("XOVER", "nntp.xover");
        let r = self.xover_unmeasured(start, end).await;
        self.measured(measure, &r);
        r
    }

    async fn xover_unmeasured(&mut self, start: u64, end: u64) -> Result<Vec<Overview>> {
        let (code, message) = self.command("XOVER", Some(&format!("{start}-{end}"))).await?;

        if code != 224 {
            return Err(NntpError::Reply { code, message });
        }

        let lines = self.read_response_body(&message).await?;

        Ok(lines.iter().filter_map(|l| parse_overview(l)).collect())
    }

    /// Queries the NNTP server's UTC clock via `DATE` (`111 YYYYMMDDhhmmss`).
    pub async fn server_date_utc(&mut self) -> Result<chrono::DateTime<chrono::Utc>> {
        let (code, message) = self.command("DATE", None).await?;
        if code != 111 {
            return Err(NntpError::Reply { code, message });
        }
        let line = format!("{code} {message}");
        crate::discovery::ServerClock::parse_nntp_date(&line)
            .ok_or_else(|| NntpError::Protocol(format!("invalid DATE response: {line}")))
    }

    /// Measures the server's clock skew (`server_utc - local_utc`) in seconds via `DATE`.
    /// Positive skew means the provider clock runs ahead of UTC (e.g. Giganews `+5.74s`).
    pub async fn clock_skew_seconds(&mut self) -> Result<f64> {
        let server_dt = self.server_date_utc().await?;
        let now = chrono::Utc::now();
        Ok((server_dt.timestamp_millis() - now.timestamp_millis()) as f64 / 1_000.0)
    }

    /// `XZVER` compressed overview fetch (yEnc-encoded deflate/zlib stream) for providers
    /// such as XSNews (`reader.xsnews.nl`) and Usenet.Farm (`farm.newsgroupdirect.com`)
    /// where `XFEATURE COMPRESS GZIP` returns `500` but `XZVER` returns `224`.
    /// Refuses to send `XZVER` on Highwinds backends where `XZVER` hangs on the wire.
    pub async fn xzver(&mut self, start: u64, end: u64) -> Result<Vec<Overview>> {
        let measure = self.measure("XZVER", "nntp.xzver");
        let r = self.xzver_unmeasured(start, end).await;
        self.measured(measure, &r);
        r
    }

    async fn xzver_unmeasured(&mut self, start: u64, end: u64) -> Result<Vec<Overview>> {
        if is_highwinds_xzver_hang_host(&self.server) {
            return Err(NntpError::Protocol(format!(
                "XZVER disabled on Highwinds host {} (hangs on wire; use XFEATURE COMPRESS GZIP)",
                self.server
            )));
        }
        let (code, message) = self.command("XZVER", Some(&format!("{start}-{end}"))).await?;
        if code != 224 {
            return Err(NntpError::Reply { code, message });
        }
        let mut raw_lines = Vec::new();
        self.each_plain_line(|l| {
            raw_lines.push(l);
            true
        })
        .await?;
        decode_xzver_lines(&raw_lines)
    }

    /// BODY, yEnc decoded when the article is yEnc.
    pub async fn body(&mut self, message_id: &str) -> Result<Vec<u8>> {
        let measure = self.measure("BODY", "nntp.body");
        let r = self.body_unmeasured(message_id).await;
        self.measured(measure, &r);
        r
    }

    async fn body_unmeasured(&mut self, message_id: &str) -> Result<Vec<u8>> {
        let (code, message) = self.command("BODY", Some(message_id)).await?;

        if code != 222 {
            return Err(NntpError::Reply { code, message });
        }

        let lines = self.read_response_body(&message).await?;
        Ok(yenc_decode(&lines).unwrap_or_else(|| lines.join(&b'\n')))
    }

    /// Lines of a multi-line response. With XFEATURE COMPRESS GZIP on, servers
    /// mark any listing they compressed (XOVER, LIST, ...) with `[COMPRESS=GZIP]`
    /// on the status line.
    async fn read_response_body(&mut self, status_message: &str) -> Result<Vec<Vec<u8>>> {
        let mut lines = Vec::new();
        self.each_line(status_message, |l| {
            lines.push(l);
            true
        })
        .await?;
        Ok(lines)
    }

    /// The lines of a multi-line response handed to `line` one at a time
    /// until it returns false. True when the response was read to its end;
    /// false when `line` stopped it: the rest is left unread, soo the
    /// connection cant take another request.
    async fn each_line(&mut self, status_message: &str, line: impl FnMut(Vec<u8>) -> bool) -> Result<bool> {
        if status_message.to_ascii_uppercase().contains("COMPRESS=GZIP") {
            self.each_compressed_line(line).await
        } else {
            self.each_plain_line(line).await
        }
    }

    /// XOVER `start..=end` read for its first and last article with a post
    /// date, `rows` rows at most: a listing longer than that is left unread
    /// past them (`Ends::whole` is false) and the connection cant take
    /// another request. Listings come sorted by number (RFC 3977), soo
    /// `first` is the range's first even then.
    pub async fn xover_ends(&mut self, start: u64, end: u64, rows: usize) -> Result<Ends> {
        let measure = self.measure("XOVER", "nntp.xover_ends");
        let r = self.xover_ends_unmeasured(start, end, rows).await;
        self.measured(measure, &r);
        r
    }

    async fn xover_ends_unmeasured(&mut self, start: u64, end: u64, rows: usize) -> Result<Ends> {
        let (code, message) = self.command("XOVER", Some(&format!("{start}-{end}"))).await?;

        if code != 224 {
            return Err(NntpError::Reply { code, message });
        }

        let mut ends = Ends::default();
        let whole = self
            .each_line(&message, |line| {
                if ends.rows == rows {
                    return false;
                }
                ends.add(&line);
                true
            })
            .await?;
        ends.whole = whole;
        Ok(ends)
    }

    async fn list_active(&mut self, pattern: Option<&str>) -> Result<Vec<Vec<u8>>> {
        let args = match pattern {
            Some(p) => format!("ACTIVE {p}"),
            None => "ACTIVE".to_string(),
        };

        let (code, message) = self.command("LIST", Some(&args)).await?;

        if code != 215 {
            return Err(NntpError::Reply { code, message });
        }

        self.read_response_body(&message).await
    }

    /// LIST ACTIVE -> (name, approx article count), biggest first. Empty groups dropped.
    pub async fn list_groups(&mut self, pattern: Option<&str>) -> Result<Vec<(String, u64)>> {
        let measure = self.measure("LIST", "nntp.list_groups");
        let r = self.list_groups_unmeasured(pattern).await;
        self.measured(measure, &r);
        r
    }

    async fn list_groups_unmeasured(&mut self, pattern: Option<&str>) -> Result<Vec<(String, u64)>> {
        let lines = match self.list_active(pattern).await {
            Ok(lines) => lines,
            Err(NntpError::Reply { .. }) if pattern.is_some() => self.list_active(None).await?,
            Err(e) => return Err(e),
        };

        let mut groups = Vec::new();

        for line in lines {
            let line = String::from_utf8_lossy(&line);
            let parts: Vec<&str> = line.split_whitespace().collect();

            if parts.len() < 3 {
                continue;
            }

            let (Ok(high), Ok(low)) = (parts[1].parse::<i64>(), parts[2].parse::<i64>()) else {
                continue;
            };

            if high - low <= 0 {
                continue;
            }

            if let Some(p) = pattern
                && !wildmatch(parts[0], p)
            {
                continue;
            }

            groups.push((parts[0].to_string(), (high - low) as u64));
        }

        groups.sort_by_key(|g| std::cmp::Reverse(g.1));
        Ok(groups)
    }
}

/// Streaming inflate for compressed header listings. Works out zlib vs gzip
/// vs raw deflate from the first bytes.
#[derive(Default)]
struct Inflater {
    state: InflateState,
    /// bytes held back until the stream header can be read
    head: Vec<u8>,
    /// gzip ends with an 8 byte crc/size trailer after the deflate data
    trailer_left: usize,
}

#[derive(Default)]
enum InflateState {
    #[default]
    Start,
    Running(Box<flate2::Decompress>, bool),
    Done,
}

impl Inflater {
    fn finished(&self) -> bool {
        matches!(self.state, InflateState::Done) && self.trailer_left == 0
    }

    /// Inflate `input` into `out`. Returns bytes that came after the end of
    /// the compressed stream (plain text).
    fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<Vec<u8>> {
        let bad = |m: &str| NntpError::Decompress(m.to_string());

        if let InflateState::Start = self.state {
            self.head.extend_from_slice(input);
            let head = std::mem::take(&mut self.head);

            if head.len() < 2
                || (head[0] == b'.' && head.len() < 3 && !head.ends_with(b"\n"))
                || (head.iter().all(|b| b.is_ascii_digit()) && head.len() < 20)
            {
                self.head = head;
                return Ok(Vec::new());
            }

            // Some providers send a plain ".\r\n" terminator on an empty range or
            // plain "<number>\t..." overview lines even when the 224 banner advertises
            // [COMPRESS=GZIP].
            let digits = head.iter().take_while(|b| b.is_ascii_digit()).count();
            if head.starts_with(b".\r\n")
                || head.starts_with(b".\n")
                || (digits > 0 && head.get(digits) == Some(&b'\t'))
            {
                self.state = InflateState::Done;
                return Ok(head);
            }

            let (decompress, gzip, skip) = if head[0] == 0x1f && head[1] == 0x8b {
                match gzip_header_len(&head) {
                    None => {
                        self.head = head;
                        return Ok(Vec::new());
                    }
                    Some(Err(e)) => return Err(e),
                    Some(Ok(n)) => (flate2::Decompress::new(false), true, n),
                }
            } else if head[0] & 0x0f == 8
                && (head[0] >> 4) <= 7
                && (head[1] & 0x20) == 0
                && (u16::from(head[0]) << 8 | u16::from(head[1])) % 31 == 0
            {
                (flate2::Decompress::new(true), false, 0)
            } else {
                (flate2::Decompress::new(false), false, 0)
            };

            self.state = InflateState::Running(Box::new(decompress), gzip);
            return self.feed(&head[skip..], out);
        }

        let mut rest: &[u8] = input;

        if let InflateState::Running(d, gzip) = &mut self.state {
            let gzip = *gzip;
            // keep going while there is input left OR the last round filled the
            // output space: headers compress way better than 8:1 and whatever
            // didnt fit stays buffered inside the decompressor
            loop {
                out.reserve(rest.len().saturating_mul(8).max(256 * 1024));
                let (in_before, out_before) = (d.total_in(), out.len());
                let status = d
                    .decompress_vec(rest, out, flate2::FlushDecompress::None)
                    .map_err(|e| NntpError::Decompress(e.to_string()))?;
                rest = &rest[(d.total_in() - in_before) as usize..];
                let produced = out.len() - out_before;

                if status == flate2::Status::StreamEnd {
                    self.state = InflateState::Done;
                    self.trailer_left = if gzip { 8 } else { 0 };
                    break;
                }

                if produced == 0 {
                    if rest.is_empty() {
                        // needs more input
                        return Ok(Vec::new());
                    }
                    if d.total_in() == in_before {
                        return Err(bad("stuck"));
                    }
                }
            }
        }

        // past the end of the compressed stream
        let skip = self.trailer_left.min(rest.len());
        self.trailer_left -= skip;
        Ok(rest[skip..].to_vec())
    }
}

/// Size of a gzip member header. None = need more bytes.
fn gzip_header_len(b: &[u8]) -> Option<Result<usize>> {
    if b.len() < 10 {
        return None;
    }

    if b[2] != 8 {
        return Some(Err(NntpError::Decompress("unknown gzip method".into())));
    }

    let flags = b[3];
    let mut pos = 10;

    if flags & 0x04 != 0 {
        let xlen = u16::from_le_bytes([*b.get(pos)?, *b.get(pos + 1)?]) as usize;
        pos += 2 + xlen;
    }

    for bit in [0x08, 0x10] {
        if flags & bit != 0 {
            pos += b.get(pos..)?.iter().position(|c| *c == 0)? + 1;
        }
    }

    if flags & 0x02 != 0 {
        pos += 2;
    }

    (b.len() >= pos).then_some(Ok(pos))
}

/// One server's connections, capped at its `connections` setting.
struct Server {
    cfg: UsenetServer,
    /// what names this server in stored state, see `server_keys`
    key: String,
    /// what earlier builds named it, see `legacy_server_keys`
    legacy_keys: Vec<String>,
    idle: Mutex<Vec<Conn>>,
    permits: Arc<Semaphore>,
    down_until: Mutex<Option<Instant>>,
    /// set when the server's compressed headers couldnt be read
    no_compress: AtomicBool,
    /// consecutive compressed header failures
    compress_refusals: AtomicUsize,
    /// when `no_compress` was set (see `COMPRESS_RETRY`)
    compress_off: Mutex<Option<Instant>>,
    /// whether `XZVER` reads this server's headers, for a server that refuses compression (see `Pool::wants_xzver`)
    xzver: Mutex<Xzver>,
    /// listings `XZVER` failed in a row where it works (see `Pool::xzver_failed`)
    xzver_failures: AtomicUsize,
    /// server answered GROUP with 500/501: an article only server (fill / bonus),
    /// used for article lookups but not for indexing
    no_index: AtomicBool,
    /// GROUPs answered 500/501 in a row; a real server can reject one odd group name
    group_refusals: AtomicUsize,
    /// until when `no_index` holds, and how long the next time does (see `ARTICLE_ONLY_REST`)
    article_only: Mutex<(Option<Instant>, Duration)>,
    /// logins rejected in a row (see `AUTH_REFUSALS`)
    auth_refusals: AtomicUsize,
    /// login rejected or limit shrunk messages already printed
    warned_auth: AtomicBool,
    warned_limit: AtomicBool,
    /// connections atlas currently allows itself (starts at `connections`,
    /// shrinks when the provider refuses more, and climbs back, see `LIMIT_HEAL`)
    limit: AtomicUsize,
    heal: Mutex<Heal>,
    /// requests waiting for a free connection right now
    waiting: AtomicUsize,
    /// for the stats dashboard
    headers: AtomicU64,
    wire: AtomicU64,
    text: AtomicU64,
    /// how well it has served background bodies lately, see `Health`
    health: Mutex<Health>,
}

/// A server's rolling health for background bodies, 0 (all failing) to 1 (all answering). Each answer moves it
/// `HEALTH_STEP` of the way to 1 (a body, or a plain "not here": the server worked) or to 0 (a timeout, a failed
/// connect, a broken connection, "failed forwarding"). Between answers it drifts back toward `HEALTH_NEUTRAL`, soo a
/// server that was bad an hour ago isnt held to it, nor one good an hour ago trusted on that alone.
#[derive(Clone, Copy, Debug)]
struct Health {
    score: f64,
    at: Instant,
}

/// how far one answer moves a server's health: about the last ten answers count most
const HEALTH_STEP: f64 = 0.1;
/// where a server's health starts, and drifts back to
const HEALTH_NEUTRAL: f64 = 0.5;
/// how long it takes a server's health to drift most (63%) of the way back to neutral
const HEALTH_DRIFT: Duration = Duration::from_secs(30 * 60);
/// how much a server's poor health counts against it when a background body picks a server: a score of 0 counts as
/// being this many times fully busy
const HEALTH_WEIGHT: f64 = 1.0;

impl Health {
    fn new(now: Instant) -> Health {
        Health { score: HEALTH_NEUTRAL, at: now }
    }

    fn now(&self, now: Instant) -> f64 {
        let gone = now.saturating_duration_since(self.at).as_secs_f64() / HEALTH_DRIFT.as_secs_f64();
        HEALTH_NEUTRAL + (self.score - HEALTH_NEUTRAL) * (-gone).exp()
    }

    fn record(&mut self, worked: bool, now: Instant) {
        let score = self.now(now);
        let toward = if worked { 1.0 } else { 0.0 };
        self.score = score + (toward - score) * HEALTH_STEP;
        self.at = now;
    }
}

/// A body read for background work, see `Pool::background_body`.
#[derive(Debug, PartialEq, Eq)]
pub enum BackgroundBody {
    Body(Vec<u8>),
    Missing,
    TooBig,
}

#[derive(Clone, Copy)]
pub enum BackgroundRead {
    Capped,
    Prefix,
    /// the whole article undecoded, headers too
    Raw,
    /// `Raw`'s first `max_bytes` or so: a cut one is `Body`, never `TooBig`
    RawPrefix,
    /// nothing read: an empty `Body` says a server has it
    Stat,
    /// the headers alone, undecoded
    Head,
}

/// how long a background body waits for room before it queues like anything else
const BODY_PATIENCE: Duration = Duration::from_secs(20);

/// Counts a request waiting for a connection while it lives (a request
/// dropped while it waits is counted out too).
struct Waiting<'a>(&'a AtomicUsize);

impl<'a> Waiting<'a> {
    fn on(count: &'a AtomicUsize) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        Waiting(count)
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Server {
    /// Whether a new connection asks for compressed headers: as configured, unless they couldnt be read lately (see
    /// `COMPRESS_RETRY`).
    fn compress(&self, retry: Duration) -> bool {
        if !self.cfg.compress.unwrap_or(true) {
            return false;
        }
        if !self.no_compress.load(Ordering::Relaxed) {
            return true;
        }
        let mut off = self.compress_off.lock().unwrap();
        if off.is_some_and(|t| t.elapsed() >= retry) {
            *off = None;
            // on probation: one more unreadable listing turns it off again
            self.compress_refusals.store(GROUP_REFUSALS - 1, Ordering::Relaxed);
            self.no_compress.store(false, Ordering::Relaxed);
            println!("{}: asking for compressed headers again", self.cfg.host);
            return true;
        }
        false
    }
}

/// A connection borrowed from a server. Goes back to the idle list on drop
/// unless an error left it unusable, or a request on it never finished.
struct Lease {
    conn: Option<Conn>,
    server: Arc<Server>,
    _permit: OwnedSemaphorePermit,
    /// a request was sent and its reply not fully read yet
    busy: bool,
}

impl Lease {
    fn new(conn: Conn, server: Arc<Server>, permit: OwnedSemaphorePermit) -> Lease {
        Lease { conn: Some(conn), server, _permit: permit, busy: false }
    }

    /// The connection, for one request: `check` its result to end it. A
    /// request dropped part way (stopping) never reaches `check`, soo the
    /// lease knows the reply is still on the wire and throws the connection away.
    fn begin(&mut self) -> &mut Conn {
        self.busy = true;
        self.conn.as_mut().expect("lease without a connection")
    }

    fn group(&self) -> Option<&str> {
        self.conn.as_ref().and_then(|c| c.group.as_deref())
    }

    /// A listing came back whole on this connection: compressed, that clears the server's compression failures
    /// (a plain one says nothing about them).
    fn listing_read(&self) {
        if self.conn.as_ref().is_some_and(|c| c.compressed) {
            self.server.compress_refusals.store(0, Ordering::Relaxed);
        }
    }

    /// end the request: drop the connection when `r` broke it
    fn check<T>(&mut self, r: Result<T>) -> Result<T> {
        self.busy = false;
        if let Err(e) = &r {
            note_failure(&self.server.key, failure_kind(e));
        }
        if r.as_ref().err().is_some_and(NntpError::breaks_connection) {
            self.collect_traffic();
            self.conn = None;
        }
        r
    }

    /// move the connection's byte counts onto its server
    fn collect_traffic(&mut self) {
        if let Some(c) = self.conn.as_mut() {
            self.server.wire.fetch_add(std::mem::take(&mut c.wire), Ordering::Relaxed);
            self.server.text.fetch_add(std::mem::take(&mut c.text), Ordering::Relaxed);
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.collect_traffic();
        // half a reply left unread would be the next request's answer
        if let Some(mut conn) = self.conn.take().filter(|c| !self.busy && !c.poisoned) {
            conn.idle_since = Some(Instant::now());
            self.server.idle.lock().unwrap().push(conn);
        }
    }
}

/// One server's numbers for the stats dashboard.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ServerStat {
    /// which pool these totals are of: a new one starts them over
    #[serde(skip)]
    pub pool: u64,
    pub host: String,
    /// what names it in stored state and in metrics, see `server_keys`
    pub key: String,
    /// requests waiting for a free connection right now
    pub waiting: usize,
    pub priority: i64,
    pub connections: u32,
    /// what atlas allows itself now (lower than `connections` when the provider refused more)
    pub limit: usize,
    pub in_use: usize,
    pub open: usize,
    pub indexing: bool,
    pub state: &'static str,
    pub headers: u64,
    pub wire_bytes: u64,
    pub text_bytes: u64,
    /// how well it has served background bodies lately, 0 to 1 (see `Health`)
    pub health: f64,
}

/// an article number and when it was posted (unix seconds)
type Dated = (u64, i64);

/// A Date header as unix seconds when it could be true: from 2000 on and not
/// later than a little past now. Posters set their own Dates, soo a date search
/// passes over the others as undated rather than be thrown off by them.
pub(crate) fn plausible_post_time(date: &str) -> Option<i64> {
    /// 2000-01-01, and the slack for clocks running ahead
    const EARLIEST: i64 = 946_684_800;
    const AHEAD: i64 = 2 * 86_400;
    let t = crate::dates::posted_timestamp(date)?;
    (EARLIEST..=chrono::Utc::now().timestamp() + AHEAD).contains(&t).then_some(t)
}

/// windows of `DATE_LOOK` numbers from a server's first article that judge
/// how far back it keeps a group (`Pool::retention`)
const RETENTION_WINDOWS: u64 = 3;
/// Dates of a window passed over as forged before its earliest counts
const RETENTION_OUTLIERS: usize = 3;

/// How far back a server keeps a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    /// it has no articles of it
    Empty,
    /// from about this time (unix seconds)
    Since(i64),
    /// its first articles' dates disagree too much to tell (a forged run, or
    /// dates far out of order): from about this time, if they can be trusted
    Unsure(i64),
}

/// When a window of articles (`dates` sorted) starts: its earliest date not
/// more than a day before the one `RETENTION_OUTLIERS` places in, soo that
/// many forged far older Dates dont move it, nor does one forged later.
fn window_start(dates: &[i64]) -> i64 {
    let floor = dates[RETENTION_OUTLIERS.min(dates.len() - 1)] - 86_400;
    dates.iter().copied().find(|&d| d >= floor).unwrap_or(floor + 86_400)
}

/// A server's retention from the date each of its first windows counts by,
/// in number order, and the date of its first article (`first`,
/// when no window had a date to go by).
fn retention_of(windows: &[i64], first: i64) -> Retention {
    let since = windows.iter().copied().min().unwrap_or(first);
    let mut newest = i64::MIN;
    for &q in windows {
        if q < newest.saturating_sub(86_400) {
            return Retention::Unsure(since);
        }
        newest = newest.max(q);
    }
    Retention::Since(since)
}

/// numbers per date search request at first, and at most
const DATE_LOOK: u64 = 100;
const DATE_SCAN_MAX: u64 = DATE_LOOK << 6;
/// rows a date search reads of one listing at most: whatever span it asks
/// for (stepped over numbers can hide a crowd), it holds no more. A window
/// of `DATE_SCAN_MAX` numbers is always read whole
const DATE_ROWS: usize = DATE_SCAN_MAX as usize;

/// Several providers tried in priority order, each with up to `connections`
/// requests in flight.
///
/// GROUP/XOVER/LIST run on one "active" server (article numbers are per
/// server). `connect` picks the first server that works, and a group missing
/// on the active server is looked up on the others. BODY is fetched by
/// message-id, which is the same everywhere, soo a missing article is asked
/// of every server in priority order.
pub struct Pool {
    /// tells this pool from the one before it, see `ServerStat::pool`
    id: u64,
    servers: Vec<Arc<Server>>,
    active: AtomicUsize,
    ready: AtomicBool,
    failed_over_at: Mutex<Option<Instant>>,
    timeout: Duration,
    /// groups their first choice server doesnt carry: the server that does
    homes: Mutex<HashMap<String, Home>>,
    /// (group, server): GROUPs in a row the server said it doesnt carry the group although it indexed it before
    misses: Mutex<HashMap<(String, usize), u32>>,
    /// `HOME_RETRY`, shorter in tests
    home_retry: Duration,
    /// `LIMIT_HEAL`, shorter in tests
    limit_heal: Duration,
    /// `COMPRESS_RETRY`, shorter in tests
    compress_retry: Duration,
    unsaved: Arc<UnsavedBudget>,
    /// how long a background body waits for room before queuing
    body_patience: Duration,
    /// `XOVER_DEADLINE` and `BODY_DEADLINE`, shorter in tests
    xover_deadline: Duration,
    /// see `XZVER_PROBE_COOLDOWN`
    xzver_cooldown: Duration,
    body_deadline: Duration,
    /// tries of a background body on one server past its first, after a timeout or a failure (not after "not here")
    body_retries: usize,
    rate_limiter: Arc<TokenBucket>,
}

/// A partition of the unsaved budget dedicated to one storage shard.
pub struct ShardBudget {
    pub permits: Arc<Semaphore>,
    pub cap: usize,
    pub now: AtomicUsize,
    pub peak: AtomicUsize,
}

/// Headers fetched (or on their way) and not saved yet, over every pass on a
/// pool: what bounds the indexer's memory. A slice takes its article numbers
/// from it before its XOVER goes out, gives back what didnt come (gaps, an
/// empty or failed slice) once the reply is in, and the rest once it's saved.
struct UnsavedBudget {
    permits: Arc<Semaphore>,
    cap: usize,
    /// held right now, and the most ever held at once
    now: AtomicUsize,
    peak: AtomicUsize,
    /// per-shard partitioned budgets (crate::store::SHARDS = 8)
    shards: Vec<Arc<ShardBudget>>,
}

/// The unsaved headers cap a pool takes for `headers`: at least one, at
/// most what its semaphore holds. Config reads through this too, soo what it
/// asks for compares equal to what the pool has.
pub fn unsaved_cap(headers: usize) -> usize {
    headers.clamp(1, Semaphore::MAX_PERMITS.min(u32::MAX as usize))
}

impl UnsavedBudget {
    fn new(cap: usize) -> Arc<Self> {
        let cap = unsaved_cap(cap);
        let per_shard = (cap / crate::store::SHARDS).max(1);
        let shards = (0..crate::store::SHARDS)
            .map(|_| {
                Arc::new(ShardBudget {
                    permits: Arc::new(Semaphore::new(per_shard)),
                    cap: per_shard,
                    now: AtomicUsize::new(0),
                    peak: AtomicUsize::new(0),
                })
            })
            .collect();
        Arc::new(UnsavedBudget {
            permits: Arc::new(Semaphore::new(cap)),
            cap,
            now: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            shards,
        })
    }
}

/// A slice's room in the unsaved budget, given back when dropped.
pub struct Unsaved {
    permit: OwnedSemaphorePermit,
    budget: Arc<UnsavedBudget>,
    shard_info: Option<(Arc<ShardBudget>, OwnedSemaphorePermit)>,
    /// headers held past the room (a slice bigger than the whole budget),
    /// counted soo `now` and `peak` show what is really held
    over: usize,
    /// the slice's place among its pass's `STREAM_SLICES`
    _slot: Option<OwnedSemaphorePermit>,
}

impl Unsaved {
    /// give back all but `headers`
    fn keep(&mut self, headers: usize) {
        let extra = self.permit.num_permits().saturating_sub(headers);
        if extra > 0
            && let Some(back) = self.permit.split(extra)
        {
            self.budget.now.fetch_sub(extra, Ordering::Relaxed);
            drop(back);
        }
        if let Some((ref sb, ref mut sp)) = self.shard_info {
            let s_extra = sp.num_permits().saturating_sub(headers);
            if s_extra > 0
                && let Some(back) = sp.split(s_extra)
            {
                sb.now.fetch_sub(s_extra, Ordering::Relaxed);
                drop(back);
            }
        }
        self.over = headers.saturating_sub(self.permit.num_permits());
        if self.over > 0 {
            let now = self.budget.now.fetch_add(self.over, Ordering::Relaxed) + self.over;
            self.budget.peak.fetch_max(now, Ordering::Relaxed);
            if let Some((ref sb, _)) = self.shard_info {
                let s_now = sb.now.fetch_add(self.over, Ordering::Relaxed) + self.over;
                sb.peak.fetch_max(s_now, Ordering::Relaxed);
            }
        }
    }
}

impl Drop for Unsaved {
    fn drop(&mut self) {
        // counted down before the permit goes back (right after this), soo
        // `now` never shows more than is held
        self.budget.now.fetch_sub(self.permit.num_permits() + self.over, Ordering::Relaxed);
        if let Some((ref sb, ref sp)) = self.shard_info {
            sb.now.fetch_sub(sp.num_permits() + self.over, Ordering::Relaxed);
        }
    }
}

/// What names each server in stored state (cursors, chunk claims, sweeps),
/// from its own settings alone: adding or removing another server never
/// changes it (that would orphan its cursors). The plain host on 563 with ssl
/// (what it always was for the usual server), else `host:port` (so a plain
/// 119 and an ssl 563 on one host are two servers), and `#key` after either when the server sets `key`. Two
/// accounts on the same host and port share it (the same provider numbers
/// articles the same) unless one sets a `key`. Never the user or password.
pub fn server_keys(servers: &[UsenetServer]) -> Vec<String> {
    servers.iter().map(server_key).collect()
}

fn server_key(s: &UsenetServer) -> String {
    let base = if s.port == 563 && s.use_ssl() { s.host.clone() } else { format!("{}:{}", s.host, s.port) };
    match &s.key {
        Some(k) => format!("{base}#{k}"),
        None => base,
    }
}

/// Per server, lowercased, what earlier builds named it in stored state
/// (`host`, `host:port`, `user@host:port`, depending on the other servers
/// then) and that no server is named now: cursors saved under one are its
/// own, adopted when none are saved under its key (see `indexer`)
pub fn legacy_server_keys(servers: &[UsenetServer]) -> Vec<Vec<String>> {
    let now: Vec<String> = servers.iter().map(|s| server_key(s).to_lowercase()).collect();
    servers
        .iter()
        .map(|s| {
            let host = s.host.to_lowercase();
            let hp = format!("{host}:{}", s.port);
            let user = format!("{}@{hp}", s.username.to_lowercase());
            let mut old = Vec::new();
            for k in [host, hp, user] {
                if !now.contains(&k) && !old.contains(&k) {
                    old.push(k);
                }
            }
            old
        })
        .collect()
}

impl Pool {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn new(servers: &[UsenetServer]) -> Pool {
        let keys = server_keys(servers);
        let legacy = legacy_server_keys(servers);
        static POOLS: AtomicU64 = AtomicU64::new(1);
        Pool {
            id: POOLS.fetch_add(1, Ordering::Relaxed),
            servers: servers
                .iter()
                .zip(keys)
                .zip(legacy)
                .map(|((cfg, key), legacy_keys)| {
                    Arc::new(Server {
                        cfg: cfg.clone(),
                        key,
                        legacy_keys,
                        idle: Mutex::new(Vec::new()),
                        permits: Arc::new(Semaphore::new(cfg.connections().max(1) as usize)),
                        down_until: Mutex::new(None),
                        no_compress: AtomicBool::new(false),
                        compress_refusals: AtomicUsize::new(0),
                        compress_off: Mutex::new(None),
                        xzver: Mutex::new(Xzver::Untried),
                        xzver_failures: AtomicUsize::new(0),
                        no_index: AtomicBool::new(false),
                        group_refusals: AtomicUsize::new(0),
                        article_only: Mutex::new((None, ARTICLE_ONLY_REST)),
                        auth_refusals: AtomicUsize::new(0),
                        warned_auth: AtomicBool::new(false),
                        warned_limit: AtomicBool::new(false),
                        limit: AtomicUsize::new(cfg.connections().max(1) as usize),
                        heal: Mutex::new(Heal { next: None, every: LIMIT_HEAL, climbed: false }),
                        waiting: AtomicUsize::new(0),
                        headers: AtomicU64::new(0),
                        wire: AtomicU64::new(0),
                        text: AtomicU64::new(0),
                        health: Mutex::new(Health::new(Instant::now())),
                    })
                })
                .collect(),
            active: AtomicUsize::new(0),
            ready: AtomicBool::new(false),
            failed_over_at: Mutex::new(None),
            timeout: DEFAULT_TIMEOUT,
            homes: Mutex::new(HashMap::new()),
            misses: Mutex::new(HashMap::new()),
            home_retry: HOME_RETRY,
            limit_heal: LIMIT_HEAL,
            compress_retry: COMPRESS_RETRY,
            body_patience: BODY_PATIENCE,
            xover_deadline: XOVER_DEADLINE,
            xzver_cooldown: XZVER_PROBE_COOLDOWN,
            body_deadline: BODY_DEADLINE,
            body_retries: BODY_RETRIES,
            unsaved: UnsavedBudget::new(crate::config::DEFAULT_MAX_UNSAVED_HEADERS as usize),
            rate_limiter: Arc::new(TokenBucket::new(0, 0)),
        }
    }

    pub fn rate_limiter(&self) -> &Arc<TokenBucket> {
        &self.rate_limiter
    }

    pub fn with_rate_limiter(mut self, limiter: Arc<TokenBucket>) -> Self {
        self.rate_limiter = limiter;
        self
    }

    /// how long a group stays off a home it left before going back (`HOME_RETRY`)
    pub fn with_home_retry(mut self, after: Duration) -> Self {
        self.home_retry = after;
        self
    }

    /// how long compressed headers stay off after they couldnt be read (`COMPRESS_RETRY`)
    pub fn with_compress_retry(mut self, after: Duration) -> Self {
        self.compress_retry = after;
        self
    }

    /// how long a lowered connection limit waits before it climbs back a step (`LIMIT_HEAL`)
    pub fn with_limit_heal(mut self, after: Duration) -> Self {
        self.limit_heal = after;
        for s in &self.servers {
            s.heal.lock().unwrap().every = after;
        }
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// At most `headers` fetched and not saved yet, over every pass at once.
    pub fn with_body_patience(mut self, patience: Duration) -> Self {
        self.body_patience = patience;
        self
    }

    /// how long one XOVER and one BODY may take in all (`XOVER_DEADLINE`, `BODY_DEADLINE`)
    /// A background body's time limit on each server it asks, and how many times more it asks a server that timed
    /// out or failed (`postprocess.body_timeout_secs`, `postprocess.body_retries`).
    pub fn with_body_tries(mut self, timeout: Duration, retries: usize) -> Self {
        self.body_deadline = timeout;
        self.body_retries = retries;
        self
    }

    pub fn with_deadlines(mut self, xover: Duration, body: Duration) -> Self {
        self.xover_deadline = xover;
        self.body_deadline = body;
        self
    }

    pub fn with_max_unsaved(mut self, headers: usize) -> Self {
        self.unsaved = UnsavedBudget::new(headers);
        self
    }

    /// the cap on headers fetched and not saved yet
    pub fn max_unsaved(&self) -> usize {
        self.unsaved.cap
    }

    /// headers fetched (or being fetched) and not saved yet, right now
    pub fn unsaved_headers(&self) -> usize {
        self.unsaved.now.load(Ordering::Relaxed)
    }

    /// the most headers that were ever unsaved at once
    pub fn unsaved_peak(&self) -> usize {
        self.unsaved.peak.load(Ordering::Relaxed)
    }

    /// the cap on headers for a specific shard
    pub fn shard_max_unsaved(&self, shard: usize) -> usize {
        self.unsaved.shards.get(shard).map(|s| s.cap).unwrap_or(0)
    }

    /// headers currently held in a specific shard's budget
    pub fn shard_unsaved_headers(&self, shard: usize) -> usize {
        self.unsaved.shards.get(shard).map(|s| s.now.load(Ordering::Relaxed)).unwrap_or(0)
    }

    /// the most headers ever held in a specific shard's budget at once
    pub fn shard_unsaved_peak(&self, shard: usize) -> usize {
        self.unsaved.shards.get(shard).map(|s| s.peak.load(Ordering::Relaxed)).unwrap_or(0)
    }

    /// Room for a slice of `numbers` article numbers partitioned for `shard`.
    pub async fn reserve_unsaved_shard(&self, shard: usize, numbers: u64) -> Unsaved {
        let budget = self.unsaved.clone();
        let shard_idx = shard % crate::store::SHARDS;
        if let Some(sb) = budget.shards.get(shard_idx).cloned() {
            let n = usize::try_from(numbers).unwrap_or(usize::MAX).clamp(1, sb.cap);
            let shard_permit = sb.permits.clone().acquire_many_owned(n as u32).await.expect("semaphore closed");
            let s_now = sb.now.fetch_add(n, Ordering::Relaxed) + n;
            sb.peak.fetch_max(s_now, Ordering::Relaxed);

            let global_n = usize::try_from(numbers).unwrap_or(usize::MAX).clamp(1, budget.cap);
            let permit = budget.permits.clone().acquire_many_owned(global_n as u32).await.expect("semaphore closed");
            let now = budget.now.fetch_add(global_n, Ordering::Relaxed) + global_n;
            budget.peak.fetch_max(now, Ordering::Relaxed);
            Unsaved {
                permit,
                budget,
                shard_info: Some((sb, shard_permit)),
                over: 0,
                _slot: None,
            }
        } else {
            self.reserve_unsaved(numbers).await
        }
    }

    /// Room for a slice of `numbers` article numbers in the unsaved budget,
    /// waiting until there is. A slice bigger than the whole budget waits for
    /// all of it.
    async fn reserve_unsaved(&self, numbers: u64) -> Unsaved {
        let budget = self.unsaved.clone();
        let n = usize::try_from(numbers).unwrap_or(usize::MAX).clamp(1, budget.cap);
        let permit = budget.permits.clone().acquire_many_owned(n as u32).await.expect("semaphore closed");
        let now = budget.now.fetch_add(n, Ordering::Relaxed) + n;
        budget.peak.fetch_max(now, Ordering::Relaxed);
        Unsaved { permit, budget, shard_info: None, over: 0, _slot: None }
    }

    pub fn len(&self) -> usize {
        self.servers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    pub fn active_index(&self) -> usize {
        self.active.load(Ordering::Relaxed)
    }

    pub fn active_host(&self) -> String {
        self.servers.get(self.active_index()).map(|s| s.cfg.host.clone()).unwrap_or_default()
    }

    /// what server `i` was named in stored state before (see `legacy_server_keys`)
    pub fn legacy_keys(&self, i: usize) -> Vec<String> {
        self.servers.get(i).map(|s| s.legacy_keys.clone()).unwrap_or_default()
    }

    /// server `i`'s name in stored state (see `server_keys`)
    pub fn host(&self, i: usize) -> String {
        self.servers.get(i).map(|s| s.key.clone()).unwrap_or_default()
    }

    /// requests server `i` takes at once
    /// requests server `i` takes at once
    pub fn connections(&self, i: usize) -> usize {
        self.servers.get(i).map(|s| s.limit.load(Ordering::Relaxed)).unwrap_or(1)
    }

    fn is_down(&self, i: usize) -> bool {
        self.servers[i].down_until.lock().unwrap().is_some_and(|t| Instant::now() < t)
    }

    /// Servers that take part in indexing (`index` not turned off), in config
    /// order. Falls back to every server when all of them opted out.
    pub fn indexing_servers(&self) -> Vec<usize> {
        for s in &self.servers {
            if s.no_index.load(Ordering::Relaxed) {
                let mut rest = s.article_only.lock().unwrap();
                if rest.0.is_some_and(|t| Instant::now() >= t) {
                    rest.0 = None;
                    s.group_refusals.store(0, Ordering::Relaxed);
                    s.no_index.store(false, Ordering::Relaxed);
                    println!("{}: trying it for indexing again", s.cfg.host);
                }
            }
        }
        let enabled: Vec<usize> = (0..self.servers.len())
            .filter(|&i| self.servers[i].cfg.indexes() && !self.servers[i].no_index.load(Ordering::Relaxed))
            .collect();
        if enabled.is_empty() { (0..self.servers.len()).collect() } else { enabled }
    }

    /// Indexing servers usable right now: the ones that havent failed recently
    /// (all of them when every one is down).
    pub fn indexing_tier(&self) -> Vec<usize> {
        let all = self.indexing_servers();
        let up: Vec<usize> = all.iter().copied().filter(|&i| !self.is_down(i)).collect();
        if up.is_empty() { all } else { up }
    }

    /// Requests in flight across every indexing server.
    pub fn indexing_connections(&self) -> usize {
        self.indexing_servers().iter().map(|&i| self.connections(i)).sum::<usize>().max(1)
    }

    /// Server a group gets indexed on, see `pick_server_in`.
    pub fn pick_server(&self, group: &str) -> usize {
        self.pick_server_in(&self.indexing_tier(), group)
    }

    /// Every indexing server takes part at once: groups are spread over `tier`
    /// in proportion to each server's connections, and a group keeps its
    /// server (its cursors are per server) as long as that server is healthy.
    pub fn pick_server_in(&self, tier: &[usize], group: &str) -> usize {
        // a group its first choice doesnt carry lives where it was found, and goes back to a home it left for a while
        // (one that indexed it before) once that is due to be asked again
        {
            let mut homes = self.homes.lock().unwrap();
            if let Some(mut home) = homes.get(group).copied() {
                if let Some(back) = home.back
                    && home.since.elapsed() >= self.home_retry
                {
                    home = Home { server: back, since: Instant::now(), back: None };
                    homes.insert(group.to_string(), home);
                }
                if tier.contains(&home.server) {
                    return home.server;
                }
            }
        }

        // a server that is down only moves its own groups: the others keep the
        // server they get with every indexing server up
        let first = self.spread_over(&self.indexing_servers(), group);
        if tier.contains(&first) {
            return first;
        }
        self.spread_over(tier, group)
    }

    /// `group`'s server among `tier`, by its hash weighted by connections.
    fn spread_over(&self, tier: &[usize], group: &str) -> usize {
        let total: u64 = tier.iter().map(|&i| self.connections(i) as u64).sum();
        if total == 0 {
            return tier.first().copied().unwrap_or(0);
        }

        // fnv-1a: stable across runs and rust versions
        let hash =
            group.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
        let mut point = hash % total;

        for &i in tier {
            let c = self.connections(i) as u64;
            if point < c {
                return i;
            }
            point -= c;
        }
        tier[0]
    }

    /// The servers whose cursors on `group` are its own: where `pick_server_in` puts it with every indexing server up,
    /// and the home it left for a while and goes back to (`Home::back`).
    pub fn homes_of(&self, group: &str) -> Vec<usize> {
        let home = self.pick_server_in(&self.indexing_servers(), group);
        let back = self.homes.lock().unwrap().get(group).and_then(|h| h.back);
        std::iter::once(home).chain(back.filter(|&b| b != home)).collect()
    }

    /// The server `group`'s passes go to while the ones in `not` are failing it: the first `ranked` gives among the
    /// rest of `tier`, none when no other is left.
    pub fn fallback_in(&self, tier: &[usize], group: &str, not: &[usize]) -> Option<usize> {
        let rest: Vec<usize> = tier.iter().copied().filter(|i| !not.contains(i)).collect();
        self.ranked(&rest, group).first().copied()
    }

    /// `candidates` in the order a group should try them: rendezvous hashing
    /// weighted by connections, soo groups that need another server spread
    /// over all of them in proportion (not all onto the first in the list),
    /// and the same group always gets the same order.
    fn ranked(&self, candidates: &[usize], group: &str) -> Vec<usize> {
        let fnv = |bytes: &[u8]| {
            bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
        };
        let mut scored: Vec<(f64, usize)> = candidates
            .iter()
            .map(|&i| {
                // by host (and an explicit `key`), not port: a group's home
                // stays where it was before ports were part of server keys
                let cfg = &self.servers[i].cfg;
                let key =
                    format!("{group}\0{}{}", cfg.host, cfg.key.as_ref().map(|k| format!("#{k}")).unwrap_or_default());
                // splitmix64's finalizer: fnv alone barely changes between hosts
                let mut z = fnv(key.as_bytes());
                z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                z ^= z >> 31;
                // a well mixed number in (0, 1]
                let h = ((z >> 11) + 1) as f64 / (1u64 << 53) as f64;
                let weight = self.connections(i).max(1) as f64;
                (-(h.max(f64::MIN_POSITIVE)).ln() / weight, i)
            })
            .collect();
        scored.sort_by(|a, b| a.0.total_cmp(&b.0));
        scored.into_iter().map(|(_, i)| i).collect()
    }

    /// requests the active server takes at once
    pub fn concurrency(&self) -> usize {
        self.servers.get(self.active_index()).map(|s| s.cfg.connections() as usize).unwrap_or(1)
    }

    pub fn is_connected(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    fn no_servers() -> NntpError {
        NntpError::Protocol("no usenet servers configured".into())
    }

    /// Borrow a connection to server `i`, opening one if none is idle.
    /// `force` ignores the "recently failed" mark.
    async fn lease(&self, i: usize, force: bool) -> Result<Lease> {
        self.lease_for(i, None, force).await
    }

    /// Like `lease`, but an idle connection that already has `group` selected
    /// is taken first (saves a GROUP round trip when many groups share a server).
    async fn lease_for(&self, i: usize, group: Option<&str>, force: bool) -> Result<Lease> {
        let server = self.servers.get(i).cloned().ok_or_else(Self::no_servers)?;

        loop {
            self.heal_limit(&server);
            let waited = Instant::now();
            let permit = {
                let _waiting = Waiting::on(&server.waiting);
                server.permits.clone().acquire_owned().await.expect("semaphore closed")
            };
            crate::profile::Load::add_since(&crate::profile::LOAD.lease_wait_ns, waited);

            if let Some(lease) = self.connect_with_permit(&server, permit, group, force).await? {
                return Ok(lease);
            }
        }
    }

    /// A connection for a permit already held: an idle one, or a new one. None:
    /// the provider allows fewer connections than thought, wait for another permit.
    async fn connect_with_permit(
        &self,
        server: &Arc<Server>,
        permit: OwnedSemaphorePermit,
        group: Option<&str>,
        force: bool,
    ) -> Result<Option<Lease>> {
        let idle = {
            let mut idle = server.idle.lock().unwrap();
            // a server closes a connection that sat idle a while, and the next request on it would fail the pass
            idle.retain(|c| !is_stale(c.idle_since));
            let on_group = group.and_then(|g| idle.iter().rposition(|c| c.group.as_deref() == Some(g)));
            match on_group {
                Some(pos) => Some(idle.swap_remove(pos)),
                None => idle.pop(),
            }
        };
        if let Some(mut conn) = idle {
            conn.set_rate_limiter(self.rate_limiter.clone());
            return Ok(Some(Lease::new(conn, server.clone(), permit)));
        }

        if !force && server.down_until.lock().unwrap().is_some_and(|t| Instant::now() < t) {
            note_failure(&server.key, "skipped_while_resting");
            return Err(NntpError::Io(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!("{} failed recently, skipping it", server.cfg.host),
            )));
        }

        let connecting = crate::metrics::span("nntp.connect");
        let opened = Conn::open(&server.cfg, self.timeout, server.compress(self.compress_retry)).await;
        let connects = |result: &'static str| {
            crate::metrics::names::NNTP_CONNECTS.inc(&[("server", server.key.as_str()), ("result", result)], 1);
        };
        let e = match opened {
            Ok(mut conn) => {
                conn.set_rate_limiter(self.rate_limiter.clone());
                connects("ok");
                *server.down_until.lock().unwrap() = None;
                server.auth_refusals.store(0, Ordering::Relaxed);
                {
                    // a step back up held; at the configured count, the next lowering waits the shortest again
                    let mut heal = server.heal.lock().unwrap();
                    heal.climbed = false;
                    if heal.next.is_none() {
                        heal.every = self.limit_heal;
                    }
                }
                return Ok(Some(Lease::new(conn, server.clone(), permit)));
            }
            Err(e) => e,
        };
        connects("failed");
        connecting.failed();
        note_failure(&server.key, failure_kind(&e));

        let limit = server.limit.load(Ordering::Relaxed);
        let others_open = server.permits.available_permits() + 1 < limit;

        match refusal(&e) {
            // the provider allows fewer connections than configured (or another
            // app on the same account uses some): use one less from now on and
            // wait for a connection that is already open
            Refusal::TooMany if others_open && limit > 1 => {
                permit.forget();
                let now = server.limit.fetch_sub(1, Ordering::Relaxed) - 1;
                {
                    // refused right after a step back up: the next step waits twice as long
                    let mut heal = server.heal.lock().unwrap();
                    if std::mem::take(&mut heal.climbed) {
                        heal.every = (heal.every * 2).min(LIMIT_HEAL_MAX.max(self.limit_heal));
                    }
                    heal.next = Some(Instant::now() + heal.every);
                }
                if !server.warned_limit.swap(true, Ordering::Relaxed) {
                    println!("{}: provider refused more connections ({e}), lowering to {now} for now", server.cfg.host);
                }
                Ok(None)
            }
            Refusal::Auth if server.auth_refusals.fetch_add(1, Ordering::Relaxed) + 1 < AUTH_REFUSALS => {
                // once could be a limit or a hiccup worded like a bad login: a short rest, like any failed connect
                *server.down_until.lock().unwrap() = Some(Instant::now() + DOWN_FOR);
                Err(e)
            }
            Refusal::Auth => {
                *server.down_until.lock().unwrap() = Some(Instant::now() + AUTH_REST);
                if !server.warned_auth.swap(true, Ordering::Relaxed) {
                    println!(
                        "{}: login rejected ({e}). not using it for {}m, check the username/password",
                        server.cfg.host,
                        AUTH_REST.as_secs() / 60
                    );
                }
                Err(e)
            }
            _ => {
                if !others_open {
                    *server.down_until.lock().unwrap() = Some(Instant::now() + DOWN_FOR);
                }
                Err(e)
            }
        }
    }

    /// One more connection for a server whose limit a refusal lowered, when it is due (see `LIMIT_HEAL`).
    fn heal_limit(&self, server: &Server) {
        let configured = server.cfg.connections().max(1) as usize;
        let mut heal = server.heal.lock().unwrap();
        let Some(next) = heal.next else { return };
        if Instant::now() < next {
            return;
        }
        if server.limit.load(Ordering::Relaxed) < configured {
            server.permits.add_permits(1);
            server.limit.fetch_add(1, Ordering::Relaxed);
            heal.climbed = true;
        }
        if server.limit.load(Ordering::Relaxed) >= configured {
            // back at the configured count; the wait starts over once a connection there opens (see `connect_with_permit`)
            heal.next = None;
            server.warned_limit.store(false, Ordering::Relaxed);
        } else {
            heal.next = Some(Instant::now() + heal.every);
        }
    }

    /// A connection to server `i` for background work: only when nobody waits
    /// for one and more than a quarter of its connections are free. None: no room.
    async fn background_lease(&self, i: usize) -> Result<Option<Lease>> {
        let server = self.servers[i].clone();
        self.heal_limit(&server);
        let limit = server.limit.load(Ordering::Relaxed);
        if self.is_down(i)
            || server.waiting.load(Ordering::Relaxed) > 0
            || server.permits.available_permits() <= limit / 4
        {
            return Ok(None);
        }
        let Ok(permit) = server.permits.clone().try_acquire_owned() else { return Ok(None) };
        self.connect_with_permit(&server, permit, None, false).await
    }

    /// A body for background work. Tries each server, least busy first, taking a
    /// connection only when nobody waits for one and more than limit/4 are free.
    /// Past its patience with none it queues normally, soo it cant starve either.
    /// Missing: every server said so. Err: a server couldnt be asked.
    pub async fn background_body(&self, message_id: &str, max_bytes: usize) -> Result<BackgroundBody> {
        self.background_read(message_id, max_bytes, BackgroundRead::Capped).await
    }

    /// `background_body` that stops at `max_bytes` rather than failing there, and hangs
    /// up when it cut the article short. A cut body is `Body`, never `TooBig`.
    pub async fn background_prefix(&self, message_id: &str, max_bytes: usize) -> Result<BackgroundBody> {
        self.background_read(message_id, max_bytes, BackgroundRead::Prefix).await
    }

    pub async fn background_read(
        &self,
        message_id: &str,
        max_bytes: usize,
        read: BackgroundRead,
    ) -> Result<BackgroundBody> {
        let mut left = self.indexing_tier();
        left.sort_by(|&a, &b| self.background_rank(a).total_cmp(&self.background_rank(b)));
        let others: Vec<usize> = (0..self.servers.len()).filter(|i| !left.contains(i)).collect();
        left.extend(others);
        // a fallback server is asked once every other one said it hasnt the article
        let fallback = |i: usize| self.servers[i].cfg.is_fallback();
        left.sort_by_key(|&i| fallback(i));
        if left.is_empty() {
            return Err(Self::no_servers());
        }
        let start = Instant::now();
        // each server's last failure, if its last try failed: cleared when a later try of it says "not here"
        let mut failed: Vec<Option<NntpError>> = (0..self.servers.len()).map(|_| None).collect();
        // tries each server has had, each with its own `body_deadline`: one, and `body_retries` more after a failure
        let mut tries = vec![0usize; self.servers.len()];
        let mut answered = false;
        while !left.is_empty() {
            let mut no_room = Vec::new();
            // a failed try is tried again after the other servers, while its server isnt resting
            let mut again = |i: usize, e: NntpError, failed: &mut Vec<Option<NntpError>>, no_room: &mut Vec<usize>| {
                tries[i] += 1;
                if tries[i] <= self.body_retries && !self.is_down(i) {
                    no_room.push(i);
                }
                failed[i] = Some(e);
            };
            for i in left.drain(..) {
                // a fallback server waits while any other server waits for room: it's asked only once they've answered
                if fallback(i) && no_room.iter().any(|&j| !fallback(j)) {
                    no_room.push(i);
                    continue;
                }
                // resting after a failure, a fallback server isnt waited for: it wouldnt be asked before the rest ends
                if fallback(i) && self.is_down(i) {
                    failed[i] = Some(NntpError::Io(io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        format!("{} failed recently, skipping it", self.servers[i].cfg.host),
                    )));
                    continue;
                }
                // a server resting after a failure was counted then, not each time it's passed over
                let resting = self.is_down(i);
                let lease = if start.elapsed() >= self.body_patience {
                    self.lease(i, false).await.map(Some)
                } else {
                    self.background_lease(i).await
                };
                let mut lease = match lease {
                    Ok(Some(lease)) => lease,
                    Ok(None) => {
                        no_room.push(i);
                        continue;
                    }
                    Err(e) => {
                        if !resting {
                            self.served(i, false);
                        }
                        again(i, e, &mut failed, &mut no_room);
                        continue;
                    }
                };
                // the body, and whether it was read to its end
                let r = within(self.body_deadline, "BODY", async {
                    match read {
                        BackgroundRead::Capped => {
                            lease.begin().body_capped(message_id, max_bytes).await.map(|b| (b.is_some(), b))
                        }
                        BackgroundRead::Prefix => {
                            lease.begin().body_prefix(message_id, max_bytes).await.map(|(b, whole)| (whole, Some(b)))
                        }
                        BackgroundRead::Raw => {
                            lease.begin().article_raw(message_id, max_bytes).await.map(|b| (b.is_some(), b))
                        }
                        BackgroundRead::RawPrefix => lease
                            .begin()
                            .article_raw_prefix(message_id, max_bytes)
                            .await
                            .map(|(b, whole)| (whole, Some(b))),
                        BackgroundRead::Stat => lease.begin().stat(message_id).await.map(|()| (true, Some(Vec::new()))),
                        BackgroundRead::Head => {
                            lease.begin().head(message_id, max_bytes).await.map(|b| (b.is_some(), b))
                        }
                    }
                })
                .await;
                match r {
                    Ok((whole, Some(body))) => {
                        self.served(i, true);
                        if whole {
                            lease.check::<()>(Ok(()))?;
                        }
                        crate::profile::LOAD.bg_bodies.fetch_add(1, Ordering::Relaxed);
                        crate::profile::LOAD.bg_body_bytes.fetch_add(body.len() as u64, Ordering::Relaxed);
                        return Ok(BackgroundBody::Body(body));
                    }
                    // the rest is unread: the lease stays busy, soo the connection is thrown away
                    Ok((_, None)) => {
                        self.served(i, true);
                        return Ok(BackgroundBody::TooBig);
                    }
                    Err(e) => match lease.check::<()>(Err(e)) {
                        // couldnt ask, or anything else that says nothing about the article: if no server has it, it
                        // isnt Missing yet, and it's read again later; unless only a fallback server couldnt be asked
                        Err(e) if e.failed_forwarding() || !matches!(e, NntpError::Reply { code: 423 | 430, .. }) => {
                            self.served(i, false);
                            again(i, e, &mut failed, &mut no_room);
                        }
                        // this server doesnt have it
                        Err(_) | Ok(()) => {
                            self.served(i, true);
                            failed[i] = None;
                            answered = true;
                        }
                    },
                }
            }
            left = no_room;
            if !left.is_empty() {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
        let (fallbacks, trusted): (Vec<_>, Vec<_>) =
            failed.into_iter().enumerate().filter_map(|(i, e)| e.map(|e| (i, e))).partition(|(i, _)| fallback(*i));
        match (trusted.into_iter().next(), fallbacks.into_iter().next()) {
            (Some((_, e)), _) => Err(e),
            // only a fallback server couldnt be asked, and none other could either
            (None, Some((_, e))) if !answered => Err(e),
            _ => Ok(BackgroundBody::Missing),
        }
    }

    fn switch_to(&self, i: usize) {
        let old = self.active.swap(i, Ordering::Relaxed);
        if old != i
            && let Some(s) = self.servers.get(old)
        {
            s.idle.lock().unwrap().clear();
        }
        *self.failed_over_at.lock().unwrap() = (i > 0).then(Instant::now);
    }

    /// Use the highest priority server that answers.
    pub async fn connect(&self) -> Result<()> {
        let mut last_err = Self::no_servers();

        for i in 0..self.servers.len() {
            match self.lease(i, true).await {
                Ok(_) => {
                    if i > 0 {
                        println!("using fallback server {}", self.servers[i].cfg.host);
                    }
                    self.switch_to(i);
                    self.ready.store(true, Ordering::Relaxed);
                    return Ok(());
                }
                Err(e) => {
                    if self.servers.len() > 1 {
                        println!("couldnt connect to {}: {e}", self.servers[i].cfg.host);
                    }
                    last_err = e;
                }
            }
        }

        Err(last_err)
    }

    /// Drop every idle connection (without QUIT).
    pub fn disconnect(&self) {
        self.ready.store(false, Ordering::Relaxed);
        for s in &self.servers {
            s.idle.lock().unwrap().clear();
        }
    }

    /// QUIT and drop every idle connection.
    pub async fn close(&self) {
        self.ready.store(false, Ordering::Relaxed);
        for s in &self.servers {
            let conns: Vec<Conn> = std::mem::take(&mut *s.idle.lock().unwrap());
            for mut c in conns {
                c.quit().await;
            }
        }
    }

    /// Back to the top priority server once it has had `after` to recover.
    pub async fn restore_primary(&self, after: Duration) -> bool {
        let since = *self.failed_over_at.lock().unwrap();
        if self.active_index() == 0 || since.is_none_or(|t| t.elapsed() < after) {
            return false;
        }

        match self.lease(0, true).await {
            Ok(_) => {
                println!("back on primary server {}", self.servers[0].cfg.host);
                self.switch_to(0);
                true
            }
            Err(_) => {
                *self.failed_over_at.lock().unwrap() = Some(Instant::now());
                false
            }
        }
    }

    async fn select_on(&self, i: usize, group: &str) -> Result<(u64, u64, u64, String)> {
        let mut lease = self.lease_for(i, Some(group), false).await?;
        let r = lease.begin().select_group(group).await;

        // GROUP not supported at all: an article only (fill / bonus) server. one
        // refusal can be a group name the server doesnt like, soo it takes
        // GROUP_REFUSALS in a row with no GROUP working in between
        let server = &self.servers[i];
        match &r {
            Err(e) if matches!(e.code(), Some(500 | 501)) => {
                let refusals = server.group_refusals.fetch_add(1, Ordering::Relaxed) + 1;
                if refusals >= GROUP_REFUSALS && !server.no_index.swap(true, Ordering::Relaxed) {
                    let mut rest = server.article_only.lock().unwrap();
                    let wait = rest.1;
                    *rest = (Some(Instant::now() + wait), (wait * 2).min(ARTICLE_ONLY_MAX));
                    println!(
                        "{}: doesnt support GROUP ({e}), using it for article lookups only for {}m",
                        server.cfg.host,
                        wait.as_secs() / 60
                    );
                }
            }
            Ok(_) => server.group_refusals.store(0, Ordering::Relaxed),
            Err(_) => {}
        }

        lease.check(r)
    }

    /// GROUP on server `i` alone, for probing a server: no falling over to
    /// another server, soo where the group lives stays as it is.
    pub async fn group_on(&self, i: usize, group: &str) -> Result<(u64, u64, u64, String)> {
        self.select_on(i, group).await
    }

    /// GROUP on server `i` alone: no falling over, and the group's home stays where it is.
    pub async fn select_group_only(&self, i: usize, group: &str) -> Result<(u64, u64, u64, String)> {
        self.select_on(i, group).await
    }

    /// GROUP on the active server, falling over to the next server that carries it
    /// (which then becomes the active one).
    pub async fn select_group(&self, group: &str) -> Result<(u64, u64, u64, String)> {
        let (i, info) = self.select_group_on(self.active_index(), group).await?;
        if i != self.active_index() {
            self.switch_to(i);
        }
        Ok(info)
    }

    /// GROUP on server `prefer`, falling over to an indexing server that
    /// carries it. Returns the server used. The server found becomes the
    /// group's home for as long as the pool lives, soo only a reply that says
    /// `prefer` lacks the group moves it: 411 (no such group) or 500/501 (no
    /// GROUP at all). A 403 ("operation failed") or 430 ("failed forwarding")
    /// says the server couldnt answer just now: it fails this pass, which is
    /// tried again on the same server, the one whose cursors the group has.
    pub async fn select_group_on(&self, prefer: usize, group: &str) -> Result<(usize, (u64, u64, u64, String))> {
        self.select_group_known(prefer, group, false).await
    }

    /// `select_group_on`, where `known` says `prefer` indexed the group before (it has cursors for it). Then one
    /// reply that it lacks the group doesnt move it: that fails the pass, and only `MOVE_AFTER` in a row hand it
    /// over, to come back after `HOME_RETRY`. A group `prefer` never indexed moves on the first, for good.
    pub async fn select_group_known(
        &self,
        prefer: usize,
        group: &str,
        known: bool,
    ) -> Result<(usize, (u64, u64, u64, String))> {
        let key = (group.to_string(), prefer);
        let first_err = match self.select_on(prefer, group).await {
            Ok(r) => {
                self.misses.lock().unwrap().remove(&key);
                return Ok((prefer, r));
            }
            Err(e) if carries_not(&e) => e,
            Err(e) => return Err(e),
        };
        if known {
            let mut misses = self.misses.lock().unwrap();
            let n = misses.entry(key.clone()).or_insert(0);
            *n += 1;
            if *n < MOVE_AFTER {
                return Err(first_err);
            }
            misses.remove(&key);
        }

        // indexing servers only: one with `index: false` (a metered block
        // account) is for article lookups, never a group's home
        let indexing: Vec<usize> = self.indexing_servers().into_iter().filter(|&i| i != prefer).collect();
        for i in self.ranked(&indexing, group) {
            if let Ok(r) = self.select_on(i, group).await {
                println!("{group} not on {}, using {}", self.servers[prefer].cfg.host, self.servers[i].cfg.host);
                let mut homes = self.homes.lock().unwrap();
                // leaving a home it was only on for a while, it still goes back to the one it left first
                let back = match homes.get(group) {
                    Some(h) if h.server == prefer && h.back.is_some() => h.back,
                    _ => known.then_some(prefer),
                };
                homes.insert(group.to_string(), Home { server: i, since: Instant::now(), back });
                return Ok((i, r));
            }
        }

        Err(first_err)
    }

    /// The first and the last dated article of `start..=end` on server `i`,
    /// from an XOVER read `DATE_ROWS` rows at most (see `Conn::xover_ends`):
    /// a date search never holds a listing, however many numbers it asks
    /// for. A listing cut short costs its connection, which still has the
    /// rest of it on the wire.
    async fn ends(&self, i: usize, group: &str, start: u64, end: u64) -> Result<Ends> {
        let r = match self.ends_once(i, group, start, end, false).await {
            Err(NntpError::Decompress(e)) => {
                self.no_compression(i, &e);
                self.ends_once(i, group, start, end, true).await
            }
            r => r,
        };
        match r {
            Err(e) if e.is_empty_range() => Ok(Ends::empty()),
            r => r,
        }
    }

    async fn ends_once(&self, i: usize, group: &str, start: u64, end: u64, plain: bool) -> Result<Ends> {
        let mut lease = self.lease_for(i, Some(group), false).await?;
        if plain {
            self.uncompressed(&mut lease).await?;
        }

        if lease.group() != Some(group) {
            let r = lease.begin().select_group(group).await;
            lease.check(r)?;
        }

        let sent = Instant::now();
        let r = lease.begin().xover_ends(start, end, DATE_ROWS).await;
        crate::profile::Load::add_since(&crate::profile::LOAD.xover_ns, sent);
        crate::profile::LOAD.xovers.fetch_add(1, Ordering::Relaxed);
        match r {
            Ok(ends) => {
                lease.listing_read();
                lease.server.headers.fetch_add(ends.rows as u64, Ordering::Relaxed);
                // the rest of a listing cut short is still coming: the lease
                // stays busy, soo the connection is thrown away
                if ends.whole { lease.check(Ok(ends)) } else { Ok(ends) }
            }
            r => lease.check(r),
        }
    }

    /// The first article at or after `number` within the next `look` numbers on
    /// server `i`, with its post time. None when there is none. One request:
    /// listings are sorted, the first rows say.
    async fn posted_at(&self, i: usize, group: &str, number: u64, look: u64) -> Result<Option<Dated>> {
        let end = number.saturating_add(look - 1);
        let mut from = number;
        loop {
            let ends = self.ends(i, group, from, end).await?;
            match (ends.first, ends.read_to) {
                (Some(first), _) => return Ok(Some(first)),
                // a cut short listing whose rows had no date: on past them
                (None, Some(n)) if !ends.whole && n < end => from = n + 1,
                _ => return Ok(None),
            }
        }
    }

    /// The last article in `start..start + look` on server `i`, with its post
    /// time. None when there is none. A listing too long to read whole is
    /// halved, the upper half first, until the halves are: what was read of
    /// it answers when nothing above it is found.
    async fn last_at(&self, i: usize, group: &str, start: u64, look: u64) -> Result<Option<Dated>> {
        enum Todo {
            /// numbers still to read
            Span(u64, u64),
            /// the last article below the spans above it in the stack
            Found(Dated),
        }
        let mut todo = vec![Todo::Span(start, start.saturating_add(look))];
        while let Some(next) = todo.pop() {
            let (a, b) = match next {
                Todo::Found(d) => return Ok(Some(d)),
                Todo::Span(a, b) => (a, b),
            };
            let ends = self.ends(i, group, a, b - 1).await?;
            if ends.whole {
                if ends.last.is_some() {
                    return Ok(ends.last);
                }
                continue;
            }
            // everything up to `read_to` was read: the rest of the span, in halves
            if let Some(last) = ends.last {
                todo.push(Todo::Found(last));
            }
            let from = ends.read_to.map_or(a, |n| n + 1);
            if from < b {
                let mid = from + (b - from) / 2;
                if from < mid {
                    todo.push(Todo::Span(from, mid));
                }
                todo.push(Todo::Span(mid, b));
            }
        }
        Ok(None)
    }

    /// When the first and the last article in the `DATE_LOOK` numbers from
    /// `number` on server `i` were posted (unix seconds), in one request.
    /// Empty when there is none.
    pub async fn posted_dates(&self, i: usize, group: &str, number: u64) -> Result<Vec<i64>> {
        let ends = self.ends(i, group, number, number.saturating_add(DATE_LOOK - 1)).await?;
        Ok(ends.first.into_iter().chain(ends.last).map(|(_, t)| t).collect())
    }

    /// The first article in `from..end` on server `i`, with its post time.
    /// Missing numbers are skipped in windows that double from `DATE_LOOK` up
    /// to `DATE_SCAN_MAX` numbers; past that the windows spread out (each
    /// starts twice as far from `from` as the last ended, the last ends at
    /// `end`), soo a hole of millions takes a few dozen small requests. The
    /// numbers stepped over are read before a hit is taken as the first (see
    /// `first_before`), and before the range is taken for empty.
    async fn first_in(&self, i: usize, group: &str, from: u64, end: u64) -> Result<Option<Dated>> {
        // the numbers between windows that weren't read, lowest first
        let mut skipped = Vec::new();
        // every number from `from` up to here was read or stepped over
        let (mut at, mut size, mut read_to) = (from, DATE_LOOK, from);
        while at < end {
            if at > read_to {
                skipped.push((read_to, at));
            }
            let look = size.min(end - at);
            if let Some(hit) = self.posted_at(i, group, at, look).await? {
                return self.first_before(i, group, &skipped, hit).await.map(Some);
            }
            read_to = at + look;
            if size < DATE_SCAN_MAX {
                at = read_to;
                size *= 2;
            } else {
                at = (read_to + (read_to - from)).min(end.saturating_sub(DATE_SCAN_MAX)).max(read_to);
            }
        }
        // no window found one: the first article, if any, was stepped over
        for &(from, to) in &skipped {
            if let Some(found) = self.posted_at(i, group, from, to - from).await? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    /// The first article on server `i`, given the first one a window found
    /// (`hit`) and the numbers stepped over before it (`skipped`, lowest
    /// first): an article stepped over comes first. Those between two empty
    /// windows are read in one request each (quick when there's nothing, as
    /// in a hole). The run up to the hit is narrowed down by bisecting,
    /// taking it as one run of missing numbers followed by articles, and what
    /// the bisect took for missing is read once.
    async fn first_before(&self, i: usize, group: &str, skipped: &[(u64, u64)], hit: Dated) -> Result<Dated> {
        let Some((&(start, end), between)) = skipped.split_last() else { return Ok(hit) };
        for &(from, to) in between {
            if let Some(found) = self.posted_at(i, group, from, to - from).await? {
                return Ok(found);
            }
        }
        let (mut lo, mut hi, mut best) = (start, end, hit);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let look = DATE_LOOK.min(hi - mid);
            match self.posted_at(i, group, mid, look).await? {
                Some(found) => (best, hi) = (found, found.0),
                None => lo = mid + look,
            }
        }
        if best.0 > start
            && let Some(f) = self.posted_at(i, group, start, best.0 - start).await?
        {
            best = f;
        }
        Ok(best)
    }

    /// The last article in `from..end` on server `i`, with its post time: like
    /// `first_in`, going backwards from `end`.
    async fn last_in(&self, i: usize, group: &str, from: u64, end: u64) -> Result<Option<Dated>> {
        // the numbers between windows that weren't read, highest first
        let mut skipped = Vec::new();
        // every number from here up to `end` was read or stepped over
        let (mut to, mut size, mut read_from) = (end, DATE_LOOK, end);
        while to > from {
            if to < read_from {
                skipped.push((to, read_from));
            }
            let look = size.min(to - from);
            if let Some(hit) = self.last_at(i, group, to - look, look).await? {
                return self.last_after(i, group, &skipped, hit).await.map(Some);
            }
            read_from = to - look;
            if size < DATE_SCAN_MAX {
                to = read_from;
                size *= 2;
            } else {
                to = end.saturating_sub(2 * (end - read_from)).max(from.saturating_add(DATE_SCAN_MAX)).min(read_from);
            }
        }
        // no window found one: the last article, if any, was stepped over
        for &(from, to) in &skipped {
            if let Some(found) = self.last_at(i, group, from, to - from).await? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    /// `first_before` going backwards: the last article, given the last one
    /// a window found and the numbers stepped over after it (highest first).
    /// The run from the hit up is taken as articles followed by one run of
    /// missing numbers.
    async fn last_after(&self, i: usize, group: &str, skipped: &[(u64, u64)], hit: Dated) -> Result<Dated> {
        let Some((&(start, end), between)) = skipped.split_last() else { return Ok(hit) };
        for &(from, to) in between {
            if let Some(found) = self.last_at(i, group, from, to - from).await? {
                return Ok(found);
            }
        }
        let (mut lo, mut hi, mut best) = (start, end, hit);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let look = DATE_LOOK.min(hi - mid);
            match self.last_at(i, group, mid, look).await? {
                Some(found) => (best, lo) = (found, found.0 + 1),
                None => hi = mid,
            }
        }
        if end > best.0 + 1
            && let Some(l) = self.last_at(i, group, best.0 + 1, end - best.0 - 1).await?
        {
            best = l;
        }
        Ok(best)
    }

    /// When the first article in `low..=high` on server `i` was posted (unix
    /// seconds): how far back the server keeps the group. None when it has none.
    pub async fn first_post(&self, i: usize, group: &str, low: u64, high: u64) -> Result<Option<i64>> {
        Ok(self.first_in(i, group, low, high + 1).await?.map(|(_, t)| t))
    }

    /// How far back server `i` keeps the group (unix seconds), judged from
    /// the dates of its first `RETENTION_WINDOWS` windows of `DATE_LOOK`
    /// numbers rather than from its first Date alone: posters set their own,
    /// and one forged on the first article (later or earlier than the truth)
    /// would make the server look shallower or deeper than it is. Each
    /// window counts from its earliest date that isnt an outlier (see
    /// `window_start`), the earliest of those answers. Unsure when the windows disagree (one
    /// more than a day older than an earlier one): a forged run, or dates too
    /// far out of order to judge by. `low` and `high` are the server's marks.
    pub async fn retention(&self, i: usize, group: &str, low: u64, high: u64) -> Result<Retention> {
        let Some((first, t)) = self.first_in(i, group, low, high + 1).await? else { return Ok(Retention::Empty) };
        let mut earliest = Vec::new();
        for w in 0..RETENTION_WINDOWS {
            let from = first + w * DATE_LOOK;
            if from > high {
                break;
            }
            let rows = self.listing(i, group, from, (from + DATE_LOOK - 1).min(high)).await?;
            let mut dates: Vec<i64> = rows.iter().filter_map(|o| plausible_post_time(&o.date)).collect();
            if dates.is_empty() {
                continue;
            }
            dates.sort_unstable();
            earliest.push(window_start(&dates));
        }
        Ok(retention_of(&earliest, t))
    }

    /// The first article number in `low..=high` on server `i` posted at or after
    /// `when` (unix seconds), `high + 1` when there is none. A binary search over
    /// small article requests (about 35 for a billion numbers); a probe that
    /// lands in a hole in the numbering finds the articles on both sides of it,
    /// soo the hole is crossed once. Post dates are only roughly in order, soo
    /// the answer is approximate near the edges: callers overlap their ranges.
    pub async fn article_at(&self, i: usize, group: &str, low: u64, high: u64, when: i64) -> Result<u64> {
        // every article before lo is older than `when`, none from hi on is;
        // first is the first article from hi on (high + 1 when there is none)
        let (mut lo, mut hi, mut first) = (low, high + 1, high + 1);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let found = self.first_in(i, group, mid, hi).await?;
            // the first article found older: the window from it says whether
            // all of it is (a single forged Date doesnt move lo past the
            // articles below mid), else its first one that isnt answers
            let mut newer = found.map(|(n, _)| n);
            if let Some((n, t)) = found
                && t < when
            {
                match self.first_dated_from(i, group, n, hi, when).await? {
                    None => {
                        lo = n + 1;
                        continue;
                    }
                    Some(m) => newer = Some(m),
                }
            }
            // nothing from mid up to the article found (or up to hi)
            let gap_end = found.map_or(hi, |(n, _)| n);
            if let Some(n) = newer {
                first = n;
            }
            hi = mid;
            // mid is in a hole: the article before it decides which side
            // the answer is on, instead of halving through the hole
            if gap_end - mid > DATE_LOOK {
                match self.last_in(i, group, lo, mid).await? {
                    Some((m, t)) if t >= when => (hi, first) = (m, m),
                    _ => lo = mid,
                }
            }
        }
        Ok(first)
    }

    /// The articles of `start..=end` on server `i`, empty when there are none.
    async fn listing(&self, i: usize, group: &str, start: u64, end: u64) -> Result<Vec<Overview>> {
        match self.xover_on(i, group, start, end).await {
            Err(e) if e.is_empty_range() => Ok(Vec::new()),
            r => r,
        }
    }

    /// The first article in the `DATE_LOOK` numbers from `n` (below `hi`) on
    /// server `i` posted at or after `when`, by its plausible Date. None when
    /// every one of them was posted before.
    async fn first_dated_from(&self, i: usize, group: &str, n: u64, hi: u64, when: i64) -> Result<Option<u64>> {
        let end = n.saturating_add(DATE_LOOK - 1).min(hi.saturating_sub(1)).max(n);
        let rows = self.listing(i, group, n, end).await?;
        Ok(rows.iter().find(|o| plausible_post_time(&o.date).is_some_and(|t| t >= when)).map(|o| o.number))
    }

    /// Swap `lease`'s connection for a plain one when it asked for compressed headers. A listing that wouldnt
    /// inflate is asked for again uncompressed: a server whose compression works for every other group keeps it on
    /// (`no_compression`), and an idle connection would be compressed too. The compressed one is closed, and the plain
    /// one goes once the listing is read.
    async fn uncompressed(&self, lease: &mut Lease) -> Result<()> {
        if !lease.conn.as_ref().is_some_and(|c| c.compressed) {
            return Ok(());
        }
        lease.collect_traffic();
        // closed first: at the provider's connection limit the plain one takes its place, once the provider has seen
        // the old one go, which can take it a moment
        lease.conn = None;
        let mut tries = 0;
        let mut conn = loop {
            match Conn::open(&lease.server.cfg, self.timeout, false).await {
                Err(e) if matches!(refusal(&e), Refusal::TooMany) && tries < PLAIN_TRIES => {
                    tries += 1;
                    tokio::time::sleep(PLAIN_WAIT).await;
                }
                r => break r?,
            }
        };
        conn.set_rate_limiter(self.rate_limiter.clone());
        // used for this listing only, never kept idle: the server's other listings go on coming compressed
        conn.poisoned = true;
        lease.conn = Some(conn);
        Ok(())
    }

    /// Compressed listings from server `i` couldnt be read: it gets plain ones from now on
    /// when it has never delivered headers or failed `GROUP_REFUSALS` times in a row.
    fn no_compression(&self, i: usize, e: &str) {
        let server = &self.servers[i];
        let refusals = server.compress_refusals.fetch_add(1, Ordering::Relaxed) + 1;
        if server.headers.load(Ordering::Relaxed) == 0 || refusals >= GROUP_REFUSALS {
            if !server.no_compress.swap(true, Ordering::Relaxed) {
                *server.compress_off.lock().unwrap() = Some(Instant::now());
                println!("{}: couldnt read compressed headers ({e}), turning compression off", server.cfg.host);
            }
            server.idle.lock().unwrap().retain(|c| !c.compressed);
        }
    }

    /// The Message-IDs numbered `start..=end` in `group` on server `i`, by number.
    pub async fn ids_on(&self, i: usize, group: &str, start: u64, end: u64) -> Result<Vec<(u64, String)>> {
        Ok(self.xover_on(i, group, start, end).await?.into_iter().map(|o| (o.number, o.message_id)).collect())
    }

    pub(crate) async fn xover_on(&self, i: usize, group: &str, start: u64, end: u64) -> Result<Vec<Overview>> {
        let mut r = self.xover_once(i, group, start, end, false).await;
        if r.as_ref().err().is_some_and(NntpError::is_closed) {
            // the server closed the connection before answering: once more, on another one
            r = self.xover_once(i, group, start, end, false).await;
        }
        match r {
            // also after a closed connection's retry, which used to hand an unreadable listing to the indexer as an
            // error ("compressed headers: deflate decompression error" on prod)
            Err(NntpError::Decompress(e)) => {
                // count it against the server's compression, and redo the slice uncompressed
                self.no_compression(i, &e);
                self.xover_once(i, group, start, end, true).await
            }
            // too wide to answer in time: in halves (see `xover_halves`)
            Err(e) if e.is_timeout() && end.saturating_sub(start) + 1 >= 2 * MIN_CHUNK => {
                self.xover_halves(i, group, start, end).await
            }
            r => r,
        }
    }

    /// `start..=end` asked for as two halves, the left one first, each halved again while it times out, down to
    /// `MIN_CHUNK`: a server that scans the whole range before it answers can take longer than the read timeout for a
    /// wide one. On prod news.newshosting.com took 11 to 105 s to answer each 100,000 numbers of alt.binaries.mom, and
    /// one day chunk timed out on every try. The first half that fails fails the range, soo a server that answers
    /// nothing costs one timeout a halving.
    fn xover_halves<'a>(
        &'a self,
        i: usize,
        group: &'a str,
        start: u64,
        end: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<Overview>>> + Send + 'a>> {
        Box::pin(async move {
            let mid = start + (end - start) / 2;
            let mut rows = Vec::new();
            for (from, to) in [(start, mid), (mid + 1, end)] {
                match self.xover_on(i, group, from, to).await {
                    Ok(half) => rows.extend(half),
                    Err(e) if e.is_empty_range() => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(rows)
        })
    }

    /// One XOVER of `start..=end` on server `i`; `plain`: on a connection that didnt ask for compressed headers. On a
    /// server that refused compression it is an `XZVER` when that works there (see `wants_xzver`).
    async fn xover_once(&self, i: usize, group: &str, start: u64, end: u64, plain: bool) -> Result<Vec<Overview>> {
        let mut xzver_allowed = !plain;
        loop {
            let mut lease = self.lease_for(i, Some(group), false).await?;
            if plain {
                self.uncompressed(&mut lease).await?;
            }

            if lease.group() != Some(group) {
                let r = lease.begin().select_group(group).await;
                lease.check(r)?;
            }

            let sent = Instant::now();
            let ask = match lease.conn.as_ref() {
                Some(c) if xzver_allowed => self.wants_xzver(&lease.server, c),
                _ => Ask::No,
            };
            let probing = matches!(ask, Ask::Probe(_));
            let wide = end.saturating_sub(start) + 1 >= 2 * MIN_CHUNK;
            let mut requests = 1;
            let (method, r) = if matches!(ask, Ask::No) {
                let compressed = lease.conn.as_ref().is_some_and(|c| c.compressed);
                let method = if compressed { "xover_gzip" } else { "xover" };
                (method, within(self.xover_deadline, "XOVER", lease.begin().xover(start, end)).await)
            } else {
                let deadline =
                    if probing { self.xover_deadline.min(XZVER_PROBE_DEADLINE) } else { self.xover_deadline };
                match within(deadline, "XZVER", lease.begin().xzver(start, end)).await {
                    // nothing to tell it works by, and a server that ignores the range would read empty for good:
                    // the connection takes a plain XOVER of the slice, and the next listing probes
                    Ok(rows) if probing && rows.is_empty() => {
                        lease.busy = false;
                        requests = 2;
                        let r = within(self.xover_deadline, "XOVER", lease.begin().xover(start, end)).await;
                        if r.as_ref().is_ok_and(|rows| !rows.is_empty()) {
                            // XOVER has rows XZVER said werent there
                            let e = NntpError::Protocol("XZVER answered empty where XOVER has headers".into());
                            self.xzver_refused(&lease.server, &e);
                        }
                        ("xover", r)
                    }
                    Ok(rows) => {
                        self.xzver_works(&lease.server);
                        ("xzver", Ok(rows))
                    }
                    Err(e) if e.is_empty_range() => {
                        self.xzver_works(&lease.server);
                        ("xzver", Err(e))
                    }
                    // "400": the server is closing the connection, soo the slice goes again plain on another one
                    Err(e) if e.code() == Some(400) => {
                        self.xzver_failed(&lease.server, &e, probing, false);
                        crate::profile::Load::add_since(&crate::profile::LOAD.xover_ns, sent);
                        crate::profile::LOAD.xovers.fetch_add(1, Ordering::Relaxed);
                        let _ = lease.check(Err::<(), _>(e));
                        lease.collect_traffic();
                        lease.conn = None;
                        xzver_allowed = false;
                        continue;
                    }
                    // a status line alone, or a listing read to its end that wouldnt decode: the connection takes a
                    // plain XOVER of the slice
                    Err(e @ (NntpError::Reply { .. } | NntpError::Decompress(_))) => {
                        self.xzver_failed(&lease.server, &e, probing, false);
                        lease.busy = false;
                        requests = 2;
                        ("xover", within(self.xover_deadline, "XOVER", lease.begin().xover(start, end)).await)
                    }
                    // where it works, a wide slice that times out is halved like an XOVER's (see `xover_on`)
                    Err(e) if !probing && e.is_timeout() && wide => ("xzver", Err(e)),
                    // hung, hung up on, or garbled: the slice again with plain XOVER on another connection (this one
                    // may still have the reply coming). A wide probe that timed out may only have been slow: it
                    // counts toward `GROUP_REFUSALS` (see `xzver_failed`), and XOVER is halved in turn if it times out too
                    Err(e) => {
                        self.xzver_failed(&lease.server, &e, probing, e.is_timeout() && wide);
                        crate::profile::Load::add_since(&crate::profile::LOAD.xover_ns, sent);
                        crate::profile::LOAD.xovers.fetch_add(1, Ordering::Relaxed);
                        let _ = lease.check(Err::<(), _>(e));
                        xzver_allowed = false;
                        continue;
                    }
                }
            };
            drop(ask);
            crate::profile::Load::add_since(&crate::profile::LOAD.xover_ns, sent);
            crate::profile::LOAD.xovers.fetch_add(requests, Ordering::Relaxed);
            if let Ok(rows) = &r {
                lease.listing_read();
                lease.server.headers.fetch_add(rows.len() as u64, Ordering::Relaxed);
                crate::metrics::names::NNTP_OVERVIEW_ROWS
                    .inc(&[("server", lease.server.key.as_str()), ("method", method)], rows.len() as u64);
            }
            return lease.check(r);
        }
    }

    /// Whether a listing on `conn` asks for `XZVER`: the server refused `XFEATURE COMPRESS GZIP` on it, compression
    /// is on for it, it isnt a Highwinds one (where `XZVER` hangs), and `XZVER` hasnt failed on it lately. XSNews and
    /// Usenet.Farm refuse gzip but answer `XZVER`, which cuts header traffic to about a third. While it is untried
    /// one listing at a time asks (`Ask::Probe`), the rest plain XOVER, soo a server where it hangs costs one
    /// connection, not all of them.
    fn wants_xzver(&self, server: &Arc<Server>, conn: &Conn) -> Ask {
        if !conn.gzip_refused || !server.cfg.compress.unwrap_or(true) || is_highwinds_xzver_hang_host(&server.key) {
            return Ask::No;
        }
        let mut state = server.xzver.lock().unwrap();
        match *state {
            Xzver::Works => Ask::Use,
            Xzver::Probing => Ask::No,
            Xzver::Cooling(until) if Instant::now() < until => Ask::No,
            Xzver::Refused(at) if at.elapsed() < self.compress_retry => Ask::No,
            Xzver::Refused(_) | Xzver::Untried | Xzver::Cooling(_) => {
                if matches!(*state, Xzver::Refused(_)) {
                    println!("{}: asking for XZVER headers again", server.cfg.host);
                }
                *state = Xzver::Probing;
                Ask::Probe(Probe(server.clone()))
            }
        }
    }

    fn xzver_works(&self, server: &Server) {
        server.xzver_failures.store(0, Ordering::Relaxed);
        let mut state = server.xzver.lock().unwrap();
        if *state != Xzver::Works {
            *state = Xzver::Works;
            println!("{}: compression refused, reading headers with XZVER", server.cfg.host);
        }
    }

    /// `XZVER` failed on server `i`. Plain XOVER until `COMPRESS_RETRY` when the probe failed in a way that settles
    /// it, or after `GROUP_REFUSALS` failures in a row: one odd group, a dropped connection or a backend 502 where it
    /// works doesnt cost an hour of plain headers. A probe that said nothing either way (a dropped connection, a
    /// `400`, a wide slice that timed out, which XOVER might too) waits `XZVER_PROBE_COOLDOWN` for the next.
    fn xzver_failed(&self, server: &Server, e: &NntpError, probing: bool, wide_timeout: bool) {
        let says_nothing = e.is_closed() || e.code() == Some(400) || wide_timeout;
        if (probing && !says_nothing) || server.xzver_failures.fetch_add(1, Ordering::Relaxed) + 1 >= GROUP_REFUSALS {
            self.xzver_refused(server, e);
        } else if probing {
            let mut state = server.xzver.lock().unwrap();
            if *state == Xzver::Probing {
                *state = Xzver::Cooling(Instant::now() + self.xzver_cooldown);
            }
        }
    }

    fn xzver_refused(&self, server: &Server, e: &NntpError) {
        server.xzver_failures.store(0, Ordering::Relaxed);
        let mut state = server.xzver.lock().unwrap();
        if !matches!(*state, Xzver::Refused(_)) {
            println!("{}: XZVER failed ({e}), plain headers for now", server.cfg.host);
        }
        *state = Xzver::Refused(Instant::now());
    }

    /// Per server numbers for the stats dashboard.
    pub fn server_stats(&self) -> Vec<ServerStat> {
        let indexing = self.indexing_servers();
        self.servers
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let limit = s.limit.load(Ordering::Relaxed);
                let in_use = limit.saturating_sub(s.permits.available_permits());
                let down = s.down_until.lock().unwrap().is_some_and(|t| Instant::now() < t);
                let state = if s.no_index.load(Ordering::Relaxed) {
                    "article only"
                } else if down && s.auth_refusals.load(Ordering::Relaxed) >= AUTH_REFUSALS {
                    "login rejected"
                } else if down {
                    "resting"
                } else {
                    "ok"
                };
                ServerStat {
                    pool: self.id,
                    host: s.cfg.host.clone(),
                    key: s.key.clone(),
                    waiting: s.waiting.load(Ordering::Relaxed),
                    priority: s.cfg.priority,
                    connections: s.cfg.connections(),
                    limit,
                    in_use,
                    open: in_use + s.idle.lock().unwrap().len(),
                    indexing: indexing.contains(&i),
                    state,
                    headers: s.headers.load(Ordering::Relaxed),
                    wire_bytes: s.wire.load(Ordering::Relaxed),
                    text_bytes: s.text.load(Ordering::Relaxed),
                    health: self.health(i),
                }
            })
            .collect()
    }

    /// Fetch every `(start, end)` slice of `group` on `server` with
    /// up to `connections` requests in flight, sending each slice to the
    /// receiver the moment it arrives. A connection picks up the next slice as
    /// soon as it is free. No new slices are started once `stop` is set or a
    /// slice fails with anything but 423/420 (empty) / 5xx (not available), and
    /// once `stop` is set the slices still in flight are dropped unsent.
    ///
    /// Every slice holds its room in the pool's unsaved budget (`Unsaved`)
    /// until the receiver drops it, soo a receiver keeps it until the slice is
    /// parsed. At most `STREAM_SLICES` of a stream hold room at once, soo one
    /// stream cant take the whole budget while its receiver saves slice after
    /// slice. A slice takes its slot, then its room, then a connection, and the
    /// slices of one stream take room in order, one waiting at a time. Nothing
    /// that holds room waits for more: a slice holding some is fetching (and
    /// needs only a connection, which no one waiting for room holds), waiting
    /// in the channel for its receiver, or being parsed, soo room always comes free.
    pub fn stream_headers(
        self: &Arc<Self>,
        group: &str,
        server: usize,
        slices: Vec<(u64, u64)>,
        stop: Arc<AtomicBool>,
    ) -> tokio::sync::mpsc::Receiver<HeaderSlice> {
        let i = server;
        // several groups can stream from one server at once, the server's
        // semaphore keeps the total at its `connections`
        let workers = self.connections(i).max(1).min(slices.len().max(1)).min(STREAM_SLICES);
        let (tx, rx) = tokio::sync::mpsc::channel(SLICES_BUFFERED);
        let slots = Arc::new(Semaphore::new(STREAM_SLICES));
        let queue = Arc::new(tokio::sync::Mutex::new(std::collections::VecDeque::from(slices)));
        let halt = Arc::new(AtomicBool::new(false));

        for _ in 0..workers {
            let (pool, queue, halt, stop, tx, group, slots) =
                (self.clone(), queue.clone(), halt.clone(), stop.clone(), tx.clone(), group.to_string(), slots.clone());

            tokio::spawn(async move {
                loop {
                    if stop.load(Ordering::Relaxed) || halt.load(Ordering::Relaxed) {
                        return;
                    }

                    // a slot first: room is only taken for slices the pass has a slot for
                    let Some(slot) = unless_stopped(&stop, slots.clone().acquire_owned()).await else { return };
                    let slot = slot.expect("semaphore closed");

                    // the next slice and its room, the queue held meanwhile
                    let ((start, end), mut unsaved) = {
                        let mut queue = queue.lock().await;
                        let Some((start, end)) = queue.pop_front() else { return };
                        let room = pool.reserve_unsaved(end.saturating_sub(start).saturating_add(1));
                        let Some(mut unsaved) = unless_stopped(&stop, room).await else { return };
                        unsaved._slot = Some(slot);
                        ((start, end), unsaved)
                    };
                    if halt.load(Ordering::Relaxed) {
                        return;
                    }

                    let Some(result) = unless_stopped(&stop, pool.xover_on(i, &group, start, end)).await else {
                        return;
                    };
                    unsaved.keep(result.as_ref().map_or(0, Vec::len));

                    if let Err(e) = &result
                        && !e.is_empty_range()
                        && !e.is_permanent()
                    {
                        halt.store(true, Ordering::Relaxed);
                    }

                    if tx.send(HeaderSlice { start, end, result, unsaved }).await.is_err() {
                        return;
                    }
                }
            });
        }

        rx
    }

    /// XOVER `start..=end` of `group` on the active server, split into slices
    /// fetched in parallel over its connections. Slices with no articles
    /// (423/420) come back empty; any other failure fails the whole range.
    pub async fn fetch_headers(self: &Arc<Self>, group: &str, start: u64, end: u64) -> Result<Vec<Overview>> {
        if end < start {
            return Ok(Vec::new());
        }

        let i = self.active_index();
        let total = end - start + 1;
        let chunk = total.div_ceil(self.concurrency().max(1) as u64).max(MIN_CHUNK);

        let mut tasks = JoinSet::new();
        let mut slices = 0;
        let mut a = start;

        while a <= end {
            let b = end.min(a + chunk - 1);
            let pool = self.clone();
            let group = group.to_string();
            let idx = slices;
            tasks.spawn(async move { (idx, pool.xover_on(i, &group, a, b).await) });
            slices += 1;
            a = b + 1;
        }

        let mut parts: Vec<Vec<Overview>> = vec![Vec::new(); slices];
        let mut failure = None;

        // let every slice finish, an aborted one would return a half read connection to the pool
        while let Some(joined) = tasks.join_next().await {
            let (idx, result) = joined.map_err(|e| NntpError::Protocol(format!("xover task failed: {e}")))?;
            match result {
                Ok(rows) => parts[idx] = rows,
                Err(e) if e.is_empty_range() => {}
                Err(e) => {
                    failure.get_or_insert(e);
                }
            }
        }

        match failure {
            Some(e) => Err(e),
            None => Ok(parts.into_iter().flatten().collect()),
        }
    }

    /// How busy server `i` is: requests on it and waiting for it, per connection.
    /// a server's health now, see `Health`
    pub fn health(&self, i: usize) -> f64 {
        self.servers[i].health.lock().unwrap().now(Instant::now())
    }

    fn served(&self, i: usize, worked: bool) {
        self.servers[i].health.lock().unwrap().record(worked, Instant::now());
    }

    /// what a background body picks its server by, lowest first: how busy it is, and how poorly it has served
    fn background_rank(&self, i: usize) -> f64 {
        self.busy(i) + (1.0 - self.health(i)) * HEALTH_WEIGHT
    }

    fn busy(&self, i: usize) -> f64 {
        let s = &self.servers[i];
        let limit = s.limit.load(Ordering::Relaxed).max(1);
        let in_use = limit.saturating_sub(s.permits.available_permits());
        (in_use + s.waiting.load(Ordering::Relaxed)) as f64 / limit as f64
    }

    /// BODY from the least busy indexing server first, soo name lookups dont
    /// all queue on one server (equally busy ones in priority order), then
    /// from the others in priority order until one has it.
    pub async fn fetch_body(&self, message_id: &str) -> Result<Vec<u8>> {
        let mut order = self.indexing_tier();
        order.sort_by(|&a, &b| self.busy(a).total_cmp(&self.busy(b)));
        let rest: Vec<usize> = (0..self.servers.len()).filter(|i| !order.contains(i)).collect();
        order.extend(rest);
        let mut last_err = Self::no_servers();

        for i in order {
            let mut lease = match self.lease(i, false).await {
                Ok(l) => l,
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };

            let r = within(self.body_deadline, "BODY", lease.begin().body(message_id)).await;
            match lease.check(r) {
                Ok(body) => return Ok(body),
                Err(e) => last_err = e,
            }
        }

        Err(last_err)
    }

    /// HEAD, the way `fetch_body` reads a BODY: least busy indexing server first, then the others until one has
    /// it. None: past `max_bytes` on the server that had it. When every server fails, a failure that may pass beats
    /// a server saying it hasn't the article (`keep_worse`), so a caller can tell "gone" from "try again".
    pub async fn fetch_head(&self, message_id: &str, max_bytes: usize) -> Result<Option<Vec<u8>>> {
        self.fetch_capped(message_id, max_bytes, Capped::Head).await
    }

    /// BODY with a bound, read as `body_capped` reads it (streamed, given up past `max_bytes`), from the servers in
    /// `fetch_head`'s order and with its errors.
    pub async fn fetch_body_capped(&self, message_id: &str, max_bytes: usize) -> Result<Option<Vec<u8>>> {
        self.fetch_capped(message_id, max_bytes, Capped::Body).await
    }

    async fn fetch_capped(&self, message_id: &str, max_bytes: usize, read: Capped) -> Result<Option<Vec<u8>>> {
        let mut order = self.indexing_tier();
        order.sort_by(|&a, &b| self.busy(a).total_cmp(&self.busy(b)));
        let rest: Vec<usize> = (0..self.servers.len()).filter(|i| !order.contains(i)).collect();
        order.extend(rest);
        let mut kept: Option<NntpError> = None;
        for i in order {
            let mut lease = match self.lease(i, false).await {
                Ok(l) => l,
                Err(e) => {
                    keep_worse(&mut kept, e);
                    continue;
                }
            };
            let r = match read {
                Capped::Head => within(self.body_deadline, "HEAD", lease.begin().head(message_id, max_bytes)).await,
                Capped::Body => {
                    within(self.body_deadline, "BODY", lease.begin().body_capped(message_id, max_bytes)).await
                }
            };
            match lease.check(r) {
                // a cut read leaves the rest unread: the connection can't take another request
                Ok(None) => {
                    lease.collect_traffic();
                    lease.conn = None;
                    return Ok(None);
                }
                Ok(found) => return Ok(found),
                Err(e) => keep_worse(&mut kept, e),
            }
        }
        Err(kept.unwrap_or_else(Self::no_servers))
    }

    pub async fn list_groups(&self, pattern: Option<&str>) -> Result<Vec<(String, u64)>> {
        let mut lease = self.lease(self.active_index(), false).await?;
        let r = lease.begin().list_groups(pattern).await;
        lease.check(r)
    }
}

/// Turns a body into a name (par2 / nfo parsers).
pub type Extract = fn(&[u8]) -> Option<String>;

/// One XOVER slice from `Pool::stream_headers`.
pub struct HeaderSlice {
    pub start: u64,
    pub end: u64,
    pub result: Result<Vec<Overview>>,
    /// the slice's room in the unsaved budget: keep it until the slice is saved
    pub unsaved: Unsaved,
}

/// For each job, fetch its message-ids in order until `extract` gives a
/// name. Jobs run concurrently, limited by each server's connections.
pub async fn first_names(pool: Arc<Pool>, jobs: Vec<Vec<(String, Extract)>>) -> Vec<Option<String>> {
    let mut names = vec![None; jobs.len()];
    let mut tasks = JoinSet::new();

    for (idx, job) in jobs.into_iter().enumerate().filter(|(_, j)| !j.is_empty()) {
        let pool = pool.clone();
        tasks.spawn(async move {
            for (message_id, extract) in job {
                if let Ok(body) = pool.fetch_body(&message_id).await
                    && let Some(name) = extract(&body)
                {
                    return (idx, Some(name));
                }
            }
            (idx, None)
        });
    }

    while let Some(joined) = tasks.join_next().await {
        if let Ok((idx, name)) = joined {
            names[idx] = name;
        }
    }

    names
}

/// The pool driven from sync code on its own tokio runtime.
pub struct BlockingPool {
    rt: Runtime,
    pub pool: Arc<Pool>,
}

impl BlockingPool {
    pub fn new(servers: &[UsenetServer]) -> Self {
        Self::from_pool(Pool::new(servers))
    }

    pub fn from_config(cfg: &crate::config::Config) -> Self {
        Self::from_pool(Pool::new(&cfg.servers).with_max_unsaved(cfg.max_unsaved_headers()))
    }

    pub fn from_pool(pool: Pool) -> Self {
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8).max(4);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(threads)
            .thread_name("atlas-nntp")
            .enable_all()
            .build()
            .expect("couldnt start tokio runtime");

        BlockingPool { rt, pool: Arc::new(pool) }
    }

    pub fn len(&self) -> usize {
        self.pool.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pool.is_empty()
    }

    pub fn rate_limiter(&self) -> &Arc<TokenBucket> {
        self.pool.rate_limiter()
    }

    pub fn active_index(&self) -> usize {
        self.pool.active_index()
    }

    pub fn active_host(&self) -> String {
        self.pool.active_host()
    }

    pub fn is_connected(&self) -> bool {
        self.pool.is_connected()
    }

    pub fn connect(&self) -> Result<()> {
        self.rt.block_on(self.pool.connect())
    }

    pub fn disconnect(&self) {
        self.rt.block_on(self.pool.close());
    }

    pub fn restore_primary(&self, after: Duration) -> bool {
        self.rt.block_on(self.pool.restore_primary(after))
    }

    pub fn select_group(&self, group: &str) -> Result<(u64, u64, u64, String)> {
        self.rt.block_on(self.pool.select_group(group))
    }

    pub fn fetch_headers(&self, group: &str, start: u64, end: u64) -> Result<Vec<Overview>> {
        self.rt.block_on(self.pool.fetch_headers(group, start, end))
    }

    pub fn fetch_body(&self, message_id: &str) -> Result<Vec<u8>> {
        self.rt.block_on(self.pool.fetch_body(message_id))
    }

    pub fn list_groups(&self, pattern: Option<&str>) -> Result<Vec<(String, u64)>> {
        self.rt.block_on(self.pool.list_groups(pattern))
    }

    /// Real names for each job, see [`first_names`].
    pub fn first_names(&self, jobs: Vec<Vec<(String, Extract)>>) -> Vec<Option<String>> {
        self.rt.block_on(first_names(self.pool.clone(), jobs))
    }

    /// Run a future on this pool's runtime.
    pub fn block_on<F: Future>(&self, fut: F) -> F::Output {
        self.rt.block_on(fut)
    }

    /// Log into one server directly (selftest).
    pub fn check_server(server: &UsenetServer) -> Result<()> {
        let pool = BlockingPool::new(std::slice::from_ref(server));
        pool.rt.block_on(async {
            let mut conn = Conn::open(server, DEFAULT_TIMEOUT, false).await?;
            conn.quit().await;
            Ok(())
        })
    }
}

impl Drop for BlockingPool {
    fn drop(&mut self) {
        self.pool.disconnect();
    }
}

/// guess if the server wants ssl before we connect
pub fn detect_use_ssl(host: &str, port: u16) -> bool {
    // 563 is the ssl port, 119 is the plain one
    match port {
        563 => return true,
        119 => return false,
        _ => {}
    }

    let host = host.trim().trim_matches(['[', ']']).trim_end_matches('.').to_ascii_lowercase();

    // local servers are plain usually
    !(matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1") || host.ends_with(".local"))
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: LazyLock<Arc<rustls::ClientConfig>> = LazyLock::new(|| {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("tls versions")
            .with_root_certificates(roots)
            .with_no_client_auth();

        Arc::new(config)
    });

    CONFIG.clone()
}

fn parse_overview(line: &[u8]) -> Option<Overview> {
    let mut fields = line.split(|b| *b == b'\t');
    let number = std::str::from_utf8(fields.next()?).ok()?.trim().parse().ok()?;
    let str_field = |raw: Option<&[u8]>| -> String {
        raw.map(|f| String::from_utf8_lossy(f).into_owned()).unwrap_or_default()
    };
    let int_field = |raw: Option<&[u8]>| -> i64 {
        raw.and_then(|f| std::str::from_utf8(f).ok())
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(0)
    };
    // some servers keep their overview as UTF-8 read as cp1252 and written out again: the same article's subject
    // from them and from the rest would name two releases
    let subject = undo_mojibake(str_field(fields.next()));
    let from = undo_mojibake(str_field(fields.next()));
    let date = str_field(fields.next());
    let message_id = str_field(fields.next());
    let references = str_field(fields.next());
    let bytes = int_field(fields.next());
    let lines = int_field(fields.next());
    let xref_groups = fields
        .next()
        .map(|f| String::from_utf8_lossy(f))
        .and_then(|f| xref_groups(&f));

    Some(Overview {
        number,
        subject,
        from,
        date,
        message_id,
        references,
        bytes,
        lines,
        xref_groups,
    })
}

/// cp1252's 0x80..=0x9F as a decoder makes them chars. The five it leaves out come through as U+0081 and the like,
/// which is how Latin-1 has every byte, soo `cp1252_byte` takes those as their own byte.
const CP1252_HIGH: [char; 32] = [
    '\u{20ac}', '\u{81}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}', '\u{2c6}', '\u{2030}',
    '\u{160}', '\u{2039}', '\u{152}', '\u{8d}', '\u{17d}', '\u{8f}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201c}',
    '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}', '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}', '\u{153}', '\u{9d}',
    '\u{17e}', '\u{178}',
];

/// The byte a cp1252 (or Latin-1) decoder made `c` of. None: no such byte, soo the text wasnt made that way.
fn cp1252_byte(c: char) -> Option<u8> {
    match u32::from(c) {
        u @ 0..=0xFF => Some(u as u8),
        _ => CP1252_HIGH.iter().position(|h| *h == c).map(|i| 0x80 + i as u8),
    }
}

/// Text that is UTF-8 read as cp1252 (or Latin-1) and encoded again, as it was: `CultFilmsâ„¢` is `CultFilms™`.
/// Only when every char maps back to a byte and the bytes are UTF-8, which a real Latin-1 accent next to ASCII never
/// is (`Café`'s é is a lone 0xE9). Undone up to three times: a subject posted that way once already comes from such a
/// server twice over, and both have to end up the same.
pub(crate) fn undo_mojibake(text: String) -> String {
    let mut text = text;
    for _ in 0..3 {
        if text.is_ascii() {
            break;
        }
        let Some(bytes) = text.chars().map(cp1252_byte).collect::<Option<Vec<u8>>>() else { break };
        match String::from_utf8(bytes) {
            Ok(fixed) if fixed != text => text = fixed,
            _ => break,
        }
    }
    text
}

/// The groups an overview's `Xref:` field names: `Xref: host group:number group:number`, the full-header form
/// a server with `Xref:full` in its OVERVIEW.FMT sends. None when the field is something else, or missing.
fn xref_groups(field: &str) -> Option<u16> {
    let field = field.trim_start();
    if field.len() < 5 || !field.as_bytes()[..5].eq_ignore_ascii_case(b"xref:") {
        return None;
    }
    let groups = field[5..].split_whitespace().skip(1).filter(|t| t.contains(':')).count();
    Some(groups.min(usize::from(u16::MAX)) as u16)
}

/// fnmatch-ish match supporting `*` and `?`
pub fn wildmatch(name: &str, pattern: &str) -> bool {
    static CACHE: LazyLock<std::sync::Mutex<std::collections::HashMap<String, Regex>>> =
        LazyLock::new(Default::default);

    let mut cache = CACHE.lock().unwrap();
    let re = cache.entry(pattern.to_string()).or_insert_with(|| {
        let escaped = regex::escape(pattern).replace(r"\*", ".*").replace(r"\?", ".");
        Regex::new(&format!("^(?is:{escaped})$")).unwrap()
    });

    re.is_match(name)
}

/// Decode a yEnc article body. None when there is no `=ybegin` line.
pub fn yenc_decode(lines: &[Vec<u8>]) -> Option<Vec<u8>> {
    let start = lines.iter().position(|l| l.starts_with(b"=ybegin "))?;
    let body = &lines[start + 1..];
    let mut payload = Vec::with_capacity(body.iter().map(|l| l.len() + 1).sum());

    for line in body {
        if line.starts_with(b"=ypart ") {
            continue;
        }

        if line.starts_with(b"=yend") {
            break;
        }

        push_yenc_line(&mut payload, line);
    }

    Some(nntmux_yenc::decoder::decode(&payload))
}

/// One line's bytes for the shared decoder, which sees the lines as one buffer and drops CR and LF
/// even after an escape. Here an escape never reaches past its line, and an escaped CR or LF is a byte.
fn push_yenc_line(payload: &mut Vec<u8>, line: &[u8]) {
    let trailing = line.iter().rev().take_while(|&&b| b == b'=').count();
    let line = &line[..line.len() - trailing % 2];

    if !line.iter().any(|&b| b == b'\r' || b == b'\n') {
        payload.extend_from_slice(line);
        return;
    }

    let mut escaped = false;
    for &b in line {
        if escaped {
            if b == b'\r' || b == b'\n' {
                payload.push(b.wrapping_sub(64));
            } else {
                payload.extend_from_slice(&[b'=', b]);
            }
            escaped = false;
        } else if b == b'=' {
            escaped = true;
        } else {
            payload.push(b);
        }
    }
}

/// Returns `true` for Highwinds / Omicron backends (e.g. `news.eweka.nl`,
/// `news.newshosting.com`) where `XZVER` is advertised in `HELP` but hangs on the
/// wire and `XFEATURE COMPRESS GZIP` (`290`) must be used instead.
pub fn is_highwinds_xzver_hang_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    lower.contains("eweka.nl")
        || lower.contains("newshosting.com")
        || lower.contains("highwinds")
        || lower.contains("hwng")
        || lower.contains("tweaknews")
        || lower.contains("easynews")
        || lower.contains("usenetserver.com")
}

/// Decodes an `XZVER` multi-line response body (yEnc-wrapped or headerless yEnc lines
/// containing a raw-deflate, zlib, or gzip overview stream) into `Overview` records.
pub fn decode_xzver_lines(lines: &[Vec<u8>]) -> Result<Vec<Overview>> {
    if lines.is_empty() {
        return Ok(Vec::new());
    }
    let compressed = if let Some(decoded) = yenc_decode(lines) {
        decoded
    } else {
        let mut payload = Vec::with_capacity(lines.iter().map(|l| l.len()).sum());
        for line in lines {
            if line.starts_with(b"=ypart ") || line.starts_with(b"=yend") {
                continue;
            }
            push_yenc_line(&mut payload, line);
        }
        nntmux_yenc::decoder::decode(&payload)
    };
    let mut inflater = Inflater::default();
    let mut inflated = Vec::new();
    inflater.feed(&compressed, &mut inflated)?;
    if !inflater.finished() {
        return Err(NntpError::Decompress("truncated XZVER deflate stream".into()));
    }
    Ok(inflated
        .split(|&b| b == b'\n')
        .map(|mut l| {
            if l.last() == Some(&b'\r') {
                l = &l[..l.len() - 1];
            }
            l
        })
        .filter(|l| !l.is_empty() && *l != b".")
        .filter_map(parse_overview)
        .collect())
}

#[cfg(test)]
mod yenc_decode_tests {
    use super::yenc_decode;

    /// The decoder this one replaced, line by line, which `yenc_decode` has to keep matching.
    fn scalar(lines: &[Vec<u8>]) -> Option<Vec<u8>> {
        let start = lines.iter().position(|l| l.starts_with(b"=ybegin "))?;
        let mut out = Vec::new();

        for line in &lines[start + 1..] {
            if line.starts_with(b"=ypart ") {
                continue;
            }

            if line.starts_with(b"=yend") {
                break;
            }

            let mut escaped = false;
            for &b in line {
                if escaped {
                    out.push(b.wrapping_sub(64).wrapping_sub(42));
                    escaped = false;
                } else if b == b'=' {
                    escaped = true;
                } else if b != b'\r' && b != b'\n' {
                    out.push(b.wrapping_sub(42));
                }
            }
        }

        Some(out)
    }

    fn lines(text: &[&[u8]]) -> Vec<Vec<u8>> {
        text.iter().map(|l| l.to_vec()).collect()
    }

    fn same(lines: &[Vec<u8>]) {
        assert_eq!(yenc_decode(lines), scalar(lines), "{lines:?}");
    }

    #[test]
    fn matches_the_old_decoder_on_each_broken_form() {
        let begin: &[u8] = b"=ybegin part=1 line=128 size=9 name=x";
        let cases: &[&[&[u8]]] = &[
            // no =ybegin, or one without the space
            &[b"plain text"],
            &[b"=ybegin"],
            &[b"=ybeginning", b"abc"],
            &[],
            // lines before =ybegin are ignored
            &[b"junk", begin, b"abc", b"=yend size=3"],
            // a wrong or missing CRC doesn't matter, and no =yend keeps what decoded
            &[begin, b"abc", b"=yend size=3 pcrc32=deadbeef"],
            &[begin, b"abc", b"def"],
            &[begin],
            // only the first =yend counts, and =ypart is skipped anywhere
            &[begin, b"abc", b"=yend", b"def", b"=yend"],
            &[begin, b"=ypart begin=1 end=3", b"abc", b"=ypart begin=4 end=6", b"def"],
            &[begin, b"=ybegin part=2", b"abc"],
            // an escape at the end of a line goes with that line
            &[begin, b"abc=", b"def"],
            &[begin, b"abc==", b"def"],
            &[begin, b"abc===", b"def"],
            &[begin, b"=", b"="],
            &[begin, b"abc=", b"=yend"],
            // stray CR and LF inside a line, plain and escaped
            &[begin, b"a\rb\nc", b"d\r"],
            &[begin, b"a=\rb", b"c=\nd"],
            &[begin, b"a=\r", b"b"],
            &[begin, b"a=\r=", b"b"],
            // every value an escape can hold, and dots the caller has already unstuffed
            &[begin, b"=\x00=\xff=@=="],
            &[begin, b".abc", b"..def", b"."],
            &[begin, b"", b"", b"abc"],
        ];
        for case in cases {
            same(&lines(case));
        }
    }

    #[test]
    fn matches_the_old_decoder_at_every_length_and_position() {
        // the kernels work in blocks, soo walk an escape and a CR across them
        for len in 0..130 {
            for at in 0..len {
                for special in [&b"="[..], b"=\r", b"\r", b"\n", b"=="] {
                    let mut line: Vec<u8> = (0..len as u8).map(|b| b.wrapping_mul(7).wrapping_add(65)).collect();
                    line.splice(at..at, special.iter().copied());
                    same(&[b"=ybegin x".to_vec(), line, b"abc".to_vec(), b"=yend".to_vec()]);
                }
            }
        }
    }

    #[test]
    fn matches_the_old_decoder_on_random_bytes() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..20_000 {
            let mut article = Vec::new();
            if next() % 8 != 0 {
                article.push(b"=ybegin part=1 name=x".to_vec());
            }
            for _ in 0..next() % 6 {
                let len = (next() % 300) as usize;
                let line: Vec<u8> = (0..len)
                    .map(|_| match next() % 8 {
                        0 => b'=',
                        1 => [b'\r', b'\n', b'.', 0][(next() % 4) as usize],
                        _ => next() as u8,
                    })
                    .collect();
                article.push(line);
                match next() % 16 {
                    0 => article.push(b"=yend size=1".to_vec()),
                    1 => article.push(b"=ypart begin=1".to_vec()),
                    _ => {}
                }
            }
            same(&article);
        }
    }

    /// Run with `cargo test --release yenc_decode_speed -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn yenc_decode_speed() {
        let mut state = 1u64;
        let mut article = vec![b"=ybegin part=1 line=128 size=716800 name=x".to_vec()];
        for _ in 0..5600 {
            let mut line = Vec::with_capacity(132);
            for _ in 0..128 {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let raw = (state >> 56) as u8;
                if matches!(raw, 0 | 10 | 13 | b'=') {
                    line.push(b'=');
                    line.push(raw.wrapping_add(64));
                } else {
                    line.push(raw);
                }
            }
            article.push(line);
        }
        let bytes: usize = article.iter().map(Vec::len).sum();
        for (name, decode) in [("old", scalar as fn(&[Vec<u8>]) -> Option<Vec<u8>>), ("new", yenc_decode)] {
            let started = std::time::Instant::now();
            for _ in 0..200 {
                std::hint::black_box(decode(std::hint::black_box(&article)));
            }
            let mb = (bytes * 200) as f64 / 1e6;
            println!("{name}: {:.0} MB/s", mb / started.elapsed().as_secs_f64());
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn health_rises_with_answers_falls_with_failures_and_drifts_back() {
        let t0 = Instant::now();
        let mut h = Health::new(t0);
        assert_eq!(h.now(t0), HEALTH_NEUTRAL);
        for _ in 0..20 {
            h.record(true, t0);
        }
        assert!(h.now(t0) > 0.9, "{}", h.now(t0));
        for _ in 0..20 {
            h.record(false, t0);
        }
        assert!(h.now(t0) < 0.15, "{}", h.now(t0));
        // left alone for hours, it's about neutral again
        let later = t0 + HEALTH_DRIFT * 6;
        assert!((h.now(later) - HEALTH_NEUTRAL).abs() < 0.01, "{}", h.now(later));
        // one answer moves it a tenth of the way
        let mut h = Health::new(t0);
        h.record(false, t0);
        assert!((h.now(t0) - 0.45).abs() < 1e-9);
    }

    #[test]
    fn a_failure_that_may_pass_beats_an_absent_article() {
        let reply = |code, message: &str| NntpError::Reply { code, message: message.into() };
        let mut kept = None;
        keep_worse(&mut kept, reply(400, "busy"));
        keep_worse(&mut kept, reply(430, "No Such Article"));
        assert_eq!(kept.as_ref().and_then(NntpError::code), Some(400));
        let mut kept = None;
        keep_worse(&mut kept, reply(430, "No Such Article"));
        keep_worse(&mut kept, reply(430, "Failed forwarding"));
        assert!(kept.as_ref().is_some_and(|e| !says_absent(e)));
        let mut kept = None;
        keep_worse(&mut kept, reply(430, "No Such Article"));
        keep_worse(&mut kept, reply(423, "no such number"));
        assert!(kept.as_ref().is_some_and(says_absent));
    }

    #[test]
    fn an_overview_with_no_size_gets_one_from_its_lines() {
        let o = |bytes, lines| Overview { bytes, lines, ..Default::default() }.into_article().bytes;
        assert_eq!(o(740_604, 5_690), 740_604, "a size given stays");
        assert_eq!(o(0, 33_281), 4_326_530, "about a 4.3 MB yEnc part");
        assert_eq!(o(0, 0), 0);
    }

    /// the dates the first windows count by judge a server's retention: the earliest
    /// answers, and one more than a day older than an earlier one is unsure
    #[test]
    fn a_servers_key_is_its_own_and_doesnt_change_with_the_others() {
        let s = |h: &str, user: &str, port: u16| UsenetServer::new(h, user, "secret", port);
        let first = s("A.example", "x", 563);
        // 563 with ssl: the plain host, like always
        assert_eq!(server_keys(std::slice::from_ref(&first)), ["A.example"]);
        // a second account on the same host and port, another on another
        // port, and one with an explicit key: the first one's key stays
        let mut keyed = s("a.example", "y", 563);
        keyed.key = Some("block".into());
        let all = [first.clone(), s("a.example", "y", 563), s("a.example", "z", 119), s("a.example", "w", 443), keyed];
        assert_eq!(server_keys(&all), ["A.example", "a.example", "a.example:119", "a.example:443", "a.example#block"]);
        // a plain 119 and an ssl 563 on one host never share a key
        let mut plain119 = s("a.example", "u", 119);
        plain119.ssl = Some(false);
        let keys = server_keys(&[first.clone(), plain119]);
        assert_eq!(keys, ["A.example", "a.example:119"]);
        let mut plain = s("a.example", "v", 563);
        plain.ssl = Some(false);
        assert_eq!(server_keys(&[plain]), ["a.example:563"], "563 isnt the default without ssl");
        assert_eq!(Pool::new(&all).host(4), "a.example#block");
        // what the keys used to be, for cursors saved under them: never
        // another server's key now
        let old = legacy_server_keys(&all);
        assert_eq!(old[0], ["a.example:563", "x@a.example:563"]);
        assert_eq!(old[3], ["w@a.example:443"]);
    }

    #[test]
    fn retention_from_the_first_windows() {
        use super::{Retention, retention_of};
        assert_eq!(retention_of(&[], 50), Retention::Since(50), "no dates: the first article's");
        assert_eq!(retention_of(&[1_000, 2_000, 90_000], 7), Retention::Since(1_000));
        assert_eq!(retention_of(&[1_000, 500, 2_000], 7), Retention::Since(500), "within a day out of order");
        assert_eq!(retention_of(&[200_000, 300_000, 100_000], 7), Retention::Unsure(100_000));
        assert_eq!(retention_of(&[200_000, 100_000], 7), Retention::Unsure(100_000));
    }

    /// a window starts at its earliest date, unless a few are far before the rest
    #[test]
    fn a_window_starts_at_its_earliest_date_but_a_forged_one() {
        use super::window_start;
        assert_eq!(window_start(&[10, 3_600, 7_200, 10_800, 14_400]), 10);
        assert_eq!(window_start(&[10, 900_000, 900_100, 900_200, 900_300]), 900_000, "one forged far back");
        assert_eq!(window_start(&[5]), 5);
    }

    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn a_connection_idle_a_while_is_stale() {
        assert!(!is_stale(None), "in use, or new");
        assert!(!is_stale(Some(Instant::now())));
        assert!(is_stale(Instant::now().checked_sub(STALE_IDLE)));
    }

    /// A server that drops each connection's first XOVER without a word, as one does an idle connection it timed
    /// out, and answers on the next connection.
    #[test]
    fn an_xover_the_server_hangs_up_on_is_asked_again_on_another_connection() {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let connections = Arc::new(AtomicUsize::new(0));
            let seen = connections.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((sock, _)) = listener.accept().await else { return };
                    let first = seen.fetch_add(1, Ordering::SeqCst) == 0;
                    tokio::spawn(async move {
                        let (read, mut write) = sock.into_split();
                        let mut lines = tokio::io::BufReader::new(read);
                        let _ = write.write_all(b"200 hi\r\n").await;
                        loop {
                            let mut line = String::new();
                            if tokio::io::AsyncBufReadExt::read_line(&mut lines, &mut line).await.unwrap_or(0) == 0 {
                                return;
                            }
                            let reply: &[u8] = match line.split_whitespace().next().unwrap_or("") {
                                "AUTHINFO" if line.contains("USER") => b"381 more\r\n",
                                "AUTHINFO" => b"281 ok\r\n",
                                "GROUP" => b"211 2 1 2 g\r\n",
                                "XOVER" if first => return,
                                "XOVER" => {
                                    b"224 ok\r\n1\ta\tp\tdate\t<1@x>\t\t10\t1\r\n2\tb\tp\tdate\t<2@x>\t\t10\t1\r\n.\r\n"
                                }
                                _ => b"500 what\r\n",
                            };
                            let _ = write.write_all(reply).await;
                        }
                    });
                }
            });
            let mut server = UsenetServer::new("127.0.0.1", "u", "p", port);
            server.ssl = Some(false);
            server.compress = Some(false);
            let pool = Pool::new(&[server]);
            let rows = pool.xover_on(0, "g", 1, 2).await.unwrap();
            assert_eq!(rows.iter().map(|o| o.number).collect::<Vec<_>>(), [1, 2]);
            assert_eq!(connections.load(Ordering::SeqCst), 2);
        });
    }

    fn yenc_encode(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for &b in data {
            let e = b.wrapping_add(42);
            if matches!(e, 0 | b'\n' | b'\r' | b'=') {
                out.push(b'=');
                out.push(e.wrapping_add(64));
            } else {
                out.push(e);
            }
        }
        out
    }

    #[test]
    fn yenc_roundtrip() {
        let data: Vec<u8> = (0..=255u8).collect();
        let lines = vec![
            b"=ybegin part=1 line=128 size=256 name=x.bin".to_vec(),
            b"=ypart begin=1 end=256".to_vec(),
            yenc_encode(&data[..100]),
            yenc_encode(&data[100..]),
            b"=yend size=256 part=1".to_vec(),
        ];
        assert_eq!(yenc_decode(&lines).unwrap(), data);
        assert!(yenc_decode(&[b"plain text".to_vec()]).is_none());
    }

    #[test]
    fn ssl_detection() {
        assert!(detect_use_ssl("news.example.com", 563));
        assert!(!detect_use_ssl("news.example.com", 119));
        assert!(!detect_use_ssl("localhost", 1190));
        assert!(!detect_use_ssl("box.local", 4000));
        assert!(detect_use_ssl("news.example.com", 443));
    }

    #[test]
    fn wildcard() {
        assert!(wildmatch("alt.binaries.movies", "*movies*"));
        assert!(wildmatch("alt.binaries.tv", "alt.binaries*"));
        assert!(!wildmatch("comp.lang.rust", "alt.binaries*"));
        assert!(wildmatch("a.b", "a?b"));
    }

    #[test]
    fn overview_parse() {
        let line = b"42\tsubj\tme@x\tFri, 02 Oct 2026 10:11:12 +0000\t<id@x>\t\t1234\t10";
        let o = parse_overview(line).unwrap();
        assert_eq!(o.number, 42);
        assert_eq!(o.message_id, "<id@x>");
        assert_eq!(o.bytes, 1234);
        let a = o.into_article();
        assert_eq!(a.date, "2026-10-02 10:11:12");
        assert_eq!(a.xref_groups, None, "seven fields: no Xref");
        let full = b"42\tsubj\tme@x\tFri, 02 Oct 2026 10:11:12 +0000\t<id@x>\t\t1234\t10\tXref: news.example alt.binaries.a:42 alt.binaries.b:7";
        let o_full = parse_overview(full).unwrap();
        assert_eq!(o_full.xref_groups, Some(2));
        let a_full = o_full.into_article();
        assert_eq!(a_full.xref_groups, Some(2));
        let other = b"42\tsubj\tme@x\tFri, 02 Oct 2026 10:11:12 +0000\t<id@x>\t\t1234\t10\tBytes: 1234";
        assert_eq!(parse_overview(other).unwrap().xref_groups, None, "another header in field 8");
        assert_eq!(xref_groups("xref: host"), Some(0));
        assert_eq!(xref_groups(""), None);
    }

    fn run<F: Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
    }

    fn listing(n: usize) -> Vec<u8> {
        let mut text = Vec::new();
        for i in 1..=n {
            text.extend_from_slice(
                format!("{i}\t\"file{i}.rar\" yEnc (1/1)\tme\tdate\t<{i}@x>\t\t100\t1\r\n").as_bytes(),
            );
        }
        text.extend_from_slice(b"..dot stuffed\r\n.\r\n");
        text
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut e = flate2::GzBuilder::new().filename("x.txt").write(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    /// Feed `wire` to a connection in `piece` sized writes and read it back.
    /// Also checks nothing past the response got eaten.
    fn read_compressed(wire: Vec<u8>, piece: usize) -> Vec<Vec<u8>> {
        run(async move {
            let (client, mut server) = tokio::io::duplex(1 << 20);
            let mut conn = Conn::over(Box::new(client), Duration::from_secs(5));
            tokio::spawn(async move {
                for chunk in wire.chunks(piece) {
                    server.write_all(chunk).await.unwrap();
                    tokio::task::yield_now().await;
                }
                server.write_all(b"205 next\r\n").await.unwrap();
                tokio::time::sleep(Duration::from_secs(10)).await;
            });
            let lines = conn.read_response_body("[COMPRESS=GZIP]").await.unwrap();
            assert_eq!(conn.read_status().await.unwrap().0, 205, "read past the end of the response");
            lines
        })
    }

    #[test]
    fn compressed_listings() {
        let plain = listing(500);

        for (name, wire) in [
            ("zlib", zlib(&plain)),
            ("gzip", gzip(&plain)),
            ("plain overview", plain.clone()),
            ("zlib + plain terminator", {
                let mut w = zlib(&plain[..plain.len() - 3]);
                w.extend_from_slice(b".\r\n");
                w
            }),
        ] {
            for piece in [1, 7, 4096, 1 << 20] {
                let lines = read_compressed(wire.clone(), piece);
                assert_eq!(lines.len(), 501, "{name} piece {piece}");
                assert_eq!(lines.last().unwrap(), b".dot stuffed", "{name}");
                assert!(parse_overview(&lines[0]).is_some());
            }
        }
        for piece in [1, 2, 3, 4096] {
            let empty = read_compressed(b".\r\n".to_vec(), piece);
            assert!(empty.is_empty(), "empty plain terminator piece {piece}");
        }
    }

    #[test]
    fn highly_compressible_listing() {
        // repetitive headers inflate far past what one output reservation holds
        let mut plain = Vec::new();
        for i in 0..20_000 {
            plain.extend_from_slice(
                format!("{i}\tsame subject over and over again yEnc (1/1)\tme\tdate\t<{i}@x>\t\t1\t1\r\n").as_bytes(),
            );
        }
        plain.extend_from_slice(b".\r\n");
        let wire = zlib(&plain);
        assert!(plain.len() / wire.len() > 8, "test data should compress better than 8:1");

        for piece in [1 << 20, 4096] {
            assert_eq!(read_compressed(wire.clone(), piece).len(), 20_000, "piece {piece}");
        }
    }

    /// A date search reads a listing's first rows and no more: plain or
    /// compressed, a listing longer than its rows is left unread.
    #[test]
    fn a_long_listing_is_read_only_as_far_as_the_row_limit() {
        let dated = |n: usize| {
            let mut text = Vec::new();
            for i in 1..=n {
                let row = format!("{i}\ts{i}\tme\tFri, 02 Oct 2026 10:{:02}:00 +0000\t<{i}@x>\t\t100\t1\r\n", i % 60);
                text.extend_from_slice(row.as_bytes());
            }
            text.extend_from_slice(b".\r\n");
            text
        };
        let read = |status: &'static str, wire: Vec<u8>, rows: usize| {
            run(async move {
                let (client, mut server) = tokio::io::duplex(1 << 16);
                let mut conn = Conn::over(Box::new(client), Duration::from_secs(5));
                tokio::spawn(async move {
                    let mut cmd = [0u8; 64];
                    let _ = tokio::io::AsyncReadExt::read(&mut server, &mut cmd).await;
                    let _ = server.write_all(status.as_bytes()).await;
                    let _ = server.write_all(&wire).await;
                });
                conn.xover_ends(1, 50_000, rows).await.unwrap()
            })
        };
        let posted = |n: u64| {
            (
                n,
                chrono::DateTime::parse_from_rfc2822(&format!("Fri, 02 Oct 2026 10:{:02}:00 +0000", n % 60))
                    .unwrap()
                    .timestamp(),
            )
        };

        for (name, status, wire) in [
            ("plain", "224 overview follows\r\n", dated(50_000)),
            ("compressed", "224 overview follows [COMPRESS=GZIP]\r\n", zlib(&dated(50_000))),
        ] {
            let cut = read(status, wire.clone(), 100);
            assert_eq!((cut.whole, cut.rows, cut.read_to), (false, 100, Some(100)), "{name}");
            assert_eq!((cut.first, cut.last), (Some(posted(1)), Some(posted(100))), "{name}");

            let whole = read(status, wire, 50_000);
            assert_eq!((whole.whole, whole.rows), (true, 50_000), "{name}");
            assert_eq!((whole.first, whole.last), (Some(posted(1)), Some(posted(50_000))), "{name}");
        }
    }

    #[test]
    fn garbage_is_a_decompress_error() {
        run(async {
            let (client, mut server) = tokio::io::duplex(1 << 16);
            let mut conn = Conn::over(Box::new(client), Duration::from_secs(2));
            server.write_all(&[0x78, 0x9c, 0xff, 0xff, 0xff, 0xff, 0x00, 0x01]).await.unwrap();
            let err = conn.read_response_body("[COMPRESS=GZIP]").await.unwrap_err();
            assert!(matches!(err, NntpError::Decompress(_)), "{err}");
        });
    }

    #[test]
    fn gzip_header_needs_whole_header() {
        let g = gzip(b"hi");
        assert!(gzip_header_len(&g[..5]).is_none());
        // 10 fixed bytes + "x.txt\0"
        assert_eq!(gzip_header_len(&g).unwrap().unwrap(), 16);
    }

    #[test]
    fn groups_spread_over_every_indexing_server_by_connections() {
        let server = |host: &str, conns: u32, priority: i64, index: Option<bool>| {
            let mut s = UsenetServer::new(host, "u", "p", 563);
            s.connections = Some(conns);
            s.priority = priority;
            s.index = index;
            s
        };
        let pool = Pool::new(&[
            server("a", 10, 1, None),
            server("b", 10, 2, None),
            server("c", 30, 4, None),
            server("block", 50, 9, Some(false)),
        ]);

        assert_eq!(pool.indexing_servers(), vec![0, 1, 2], "index:false stays out");
        assert_eq!(pool.indexing_connections(), 50);

        let mut counts = [0usize; 4];
        for i in 0..5000 {
            counts[pool.pick_server(&format!("alt.binaries.group{i}"))] += 1;
        }
        // 10:10:30 connections -> roughly 20% / 20% / 60%, every priority used
        assert_eq!(counts[3], 0);
        for (i, want) in [(0, 0.2), (1, 0.2), (2, 0.6)] {
            let got = counts[i] as f64 / 5000.0;
            assert!((got - want).abs() < 0.05, "server {i}: {got} vs {want} ({counts:?})");
        }

        // stable: same group, same server
        assert_eq!(pool.pick_server("alt.binaries.x"), pool.pick_server("alt.binaries.x"));

        // groups that need another server spread over the others by connections,
        // not all onto the first in the list
        let mut first = [0usize; 4];
        for i in 0..5000 {
            first[pool.ranked(&[1, 2], &format!("alt.binaries.group{i}"))[0]] += 1;
        }
        let share_c = first[2] as f64 / 5000.0;
        assert!((share_c - 0.75).abs() < 0.05, "10:30 connections -> about 25% / 75%: {first:?}");
        assert_eq!(pool.ranked(&[1, 2], "alt.binaries.x"), pool.ranked(&[1, 2], "alt.binaries.x"), "stable order");

        // once found elsewhere, a group is picked there
        let home = Home { server: 1, since: Instant::now(), back: None };
        pool.homes.lock().unwrap().insert("alt.binaries.moved".into(), home);
        assert_eq!(pool.pick_server("alt.binaries.moved"), 1);
        // one that left a home which indexed it goes back there once that is due to be asked again: to that home,
        // not to its first choice (a home it was found on for good)
        let first = pool.pick_server("alt.binaries.back");
        let (left, other) = ((first + 1) % 3, (first + 2) % 3);
        let since = Instant::now() - HOME_RETRY;
        let away = Home { server: other, since, back: Some(left) };
        pool.homes.lock().unwrap().insert("alt.binaries.back".into(), away);
        assert_eq!(pool.pick_server("alt.binaries.back"), left, "back on the home it left");
        assert_eq!(pool.pick_server("alt.binaries.back"), left, "and it stays there");
        // not before
        let recent = Home { since: Instant::now(), ..away };
        pool.homes.lock().unwrap().insert("alt.binaries.back".into(), recent);
        assert_eq!(pool.pick_server("alt.binaries.back"), other);
        // one found elsewhere for good stays, however long ago
        pool.homes.lock().unwrap().insert("alt.binaries.moved".into(), Home { since, ..home });
        assert_eq!(pool.pick_server("alt.binaries.moved"), 1);
    }

    #[test]
    fn an_article_only_server_is_tried_for_indexing_again() {
        let pool = Pool::new(&[UsenetServer::new("a", "u", "p", 563), UsenetServer::new("fill", "u", "p", 563)]);
        let fill = &pool.servers[1];
        fill.no_index.store(true, Ordering::Relaxed);
        *fill.article_only.lock().unwrap() = (Some(Instant::now() + ARTICLE_ONLY_REST), ARTICLE_ONLY_REST * 2);
        assert_eq!(pool.indexing_servers(), vec![0], "resting from indexing");
        fill.group_refusals.store(GROUP_REFUSALS, Ordering::Relaxed);
        fill.article_only.lock().unwrap().0 = Some(Instant::now() - Duration::from_secs(1));
        assert_eq!(pool.indexing_servers(), vec![0, 1], "its rest is over");
        assert_eq!(fill.group_refusals.load(Ordering::Relaxed), 0, "refusals count from nothing again");
        assert_eq!(fill.article_only.lock().unwrap().1, ARTICLE_ONLY_REST * 2, "the next rest is longer");
    }

    #[test]
    fn a_server_going_down_only_moves_its_own_groups() {
        let server = |host: &str| {
            let mut s = UsenetServer::new(host, "u", "p", 563);
            s.connections = Some(4);
            s
        };
        let pool = Pool::new(&[server("a"), server("b"), server("c")]);
        let groups: Vec<String> = (0..300).map(|i| format!("alt.binaries.group{i}")).collect();
        let before: Vec<usize> = groups.iter().map(|g| pool.pick_server(g)).collect();

        *pool.servers[2].down_until.lock().unwrap() = Some(Instant::now() + DOWN_FOR);
        assert_eq!(pool.indexing_tier(), vec![0, 1]);
        for (g, &was) in groups.iter().zip(&before) {
            let now = pool.pick_server(g);
            if was == 2 {
                assert_ne!(now, 2, "{g}: its server is down, it goes to another");
            } else {
                assert_eq!(now, was, "{g}: its server is up, it stays there");
            }
        }

        // back up: every group is where it was
        *pool.servers[2].down_until.lock().unwrap() = None;
        assert_eq!(groups.iter().map(|g| pool.pick_server(g)).collect::<Vec<_>>(), before);
    }

    #[test]
    fn compressed_group_list() {
        let mut listing = Vec::new();
        for (name, hi, lo) in
            [("alt.binaries.movies", 900, 100), ("alt.binaries.movies.4k", 50, 1), ("alt.binaries.empty", 5, 5)]
        {
            listing.extend_from_slice(format!("{name} {hi} {lo} y\r\n").as_bytes());
        }
        listing.extend_from_slice(b".\r\n");
        let mut wire = b"215 newsgroups follow [COMPRESS=GZIP]\r\n".to_vec();
        wire.extend(zlib(&listing));

        let groups = run(async move {
            let (client, mut server) = tokio::io::duplex(1 << 16);
            let mut conn = Conn::over(Box::new(client), Duration::from_secs(5));
            tokio::spawn(async move {
                let mut cmd = vec![0u8; 64];
                let _ = tokio::io::AsyncReadExt::read(&mut server, &mut cmd).await;
                server.write_all(&wire).await.unwrap();
                tokio::time::sleep(Duration::from_secs(10)).await;
            });
            conn.list_groups(Some("*movies*")).await.unwrap()
        });

        assert_eq!(groups, vec![("alt.binaries.movies".to_string(), 800), ("alt.binaries.movies.4k".to_string(), 49)]);
    }

    #[test]
    fn refusals_from_real_providers() {
        let r = |code: u16, m: &str| refusal(&NntpError::Reply { code, message: m.into() });
        // seen from the providers in use
        let many = [
            (
                481,
                "(remote) (aucl:newsgroupdirect.com;someone@newsgroupdirect.com) exceeded maximum number of connections per user",
            ),
            (502, "Too many connections."),
            (482, "too many connections for your user"),
            (502, "connection limit (100) reached"),
            (482, "Connection limit(50) reached."),
            (502, "bonus.frugalusenet.com: too many connections - support@frugalusenet.com"),
        ];
        for (code, m) in many {
            assert_eq!(r(code, m), Refusal::TooMany, "{m}");
        }
        assert_eq!(r(502, "Authentication Failed"), Refusal::Auth);
        assert_eq!(r(502, "Access Denied. Please check your login/pw."), Refusal::Auth);
        assert_eq!(r(400, "service temporarily unavailable"), Refusal::Other);
        assert_eq!(refusal(&timed_out()), Refusal::Other);
    }

    #[test]
    fn token_bucket_replenishment_and_subtraction() {
        run(async {
            let bucket = TokenBucket::new(RATE_750_MBIT, 10_000_000);
            assert_eq!(bucket.rate_bytes_per_sec(), RATE_750_MBIT);
            assert_eq!(bucket.burst_bytes(), 10_000_000);
            assert_eq!(bucket.available_tokens(), 10_000_000);

            // Subtraction
            assert!(bucket.try_acquire(4_000_000));
            let tokens_after = bucket.available_tokens();
            assert!((6_000_000..=6_500_000).contains(&tokens_after));

            // Exhaust tokens
            assert!(bucket.try_acquire(tokens_after as usize));
            assert!(!bucket.try_acquire(1_000_000));

            // Explicit set and replenishment
            bucket.set_tokens(0);
            assert!(bucket.available_tokens() <= 100_000);

            // Sleep 50ms and check replenishment: 0.05 * 93_750_000 = 4_687_500 bytes (+- 20%)
            tokio::time::sleep(Duration::from_millis(50)).await;
            let tokens = bucket.available_tokens();
            assert!((3_500_000..=6_000_000).contains(&tokens), "replenished tokens: {tokens}");
        });
    }

    #[test]
    fn token_bucket_zero_burst_normalized_no_deadlock() {
        run(async {
            let bucket = TokenBucket::new(100_000, 0);
            assert_eq!(bucket.burst_bytes(), 1);
            tokio::time::timeout(Duration::from_millis(500), bucket.acquire(10))
                .await
                .expect("zero burst capacity must be normalized so acquire does not stall");
        });
    }

    #[test]
    fn token_bucket_large_read_no_deadlock() {
        run(async {
            // High rate so test completes quickly: 1 GB/s, burst 10 MB.
            let bucket = TokenBucket::new(1_000_000_000, 10_000_000);
            // Request 25 MB (> 2.5x burst size)
            let started = Instant::now();
            bucket.acquire(25_000_000).await;
            let elapsed = started.elapsed();
            // Since initial bucket had 10MB, deficit was 15MB at 1GB/s => ~15ms
            assert!(elapsed < Duration::from_secs(2), "deadlock or excessive latency: {elapsed:?}");
        });
    }

    #[test]
    fn token_bucket_fractional_nanos_accuracy() {
        run(async {
            // Test that rapid micro-acquisitions don't drop fractional tokens
            let bucket = TokenBucket::new(RATE_750_MBIT, 10_000_000);
            bucket.set_tokens(0);

            // Perform small sleeps and verify nanos_remainder accumulates tokens
            let start = Instant::now();
            for _ in 0..10 {
                tokio::time::sleep(Duration::from_micros(500)).await;
                let _ = bucket.available_tokens();
            }
            let total_elapsed = start.elapsed();
            let expected = (total_elapsed.as_nanos() * RATE_750_MBIT as u128 / 1_000_000_000) as u64;
            let actual = bucket.available_tokens();
            let diff = (expected as i64 - actual as i64).abs();
            assert!(diff <= 50_000, "expected ~{expected}, got {actual}, diff: {diff}");
        });
    }

    #[test]
    fn token_bucket_concurrent_simulated_tasks() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let bucket = Arc::new(TokenBucket::new(20_000_000, 2_000_000));
            let mut handles = Vec::new();
            for _ in 0..10 {
                let b = bucket.clone();
                handles.push(tokio::spawn(async move {
                    for _ in 0..5 {
                        b.acquire(20_000).await;
                    }
                }));
            }
            for h in handles {
                h.await.unwrap();
            }
        });
    }

    #[test]
    fn token_bucket_conn_read_throttling() {
        run(async {
            let (client, mut server) = tokio::io::duplex(1 << 16);
            let limiter = Arc::new(TokenBucket::new(100_000, 10_000));
            let mut conn = Conn::over(Box::new(client), Duration::from_secs(5))
                .with_rate_limiter(limiter.clone());

            tokio::spawn(async move {
                server.write_all(&vec![b'A'; 5_000]).await.unwrap();
                tokio::time::sleep(Duration::from_millis(30)).await;
                server.write_all(&vec![b'B'; 5_000]).await.unwrap();
                tokio::time::sleep(Duration::from_secs(5)).await;
            });

            // First chunk
            let chunk1 = conn.read_chunk(Duration::from_secs(5)).await.unwrap();
            assert_eq!(chunk1.len(), 5_000);

            // Second chunk
            let chunk2 = conn.read_chunk(Duration::from_secs(5)).await.unwrap();
            assert_eq!(chunk2.len(), 5_000);
            assert_eq!(conn.wire, 10_000);
        });
    }

    /// XSNews keeps some overviews as UTF-8 read as cp1252 and written again (seen 2026-11-07 on a ngPost post): the
    /// same article from it and from Eweka must give one subject, or one post is two releases.
    #[test]
    fn a_subject_a_server_encoded_twice_reads_as_the_others_send_it() {
        let eweka: &[u8] = "1\t[002/114] \"Film 1982-CultFilms\u{2122}.part001.rar\" (1/512)\tx@ngPost.com\t26 Oct 26 04:58:14 GMT\t<a@ngPost>\t\t730143\t5657\t".as_bytes();
        // ™ is E2 84 A2: read as cp1252 that is â „ ¢
        let twice = "Film 1982-CultFilms\u{e2}\u{201e}\u{a2}";
        let xsnews = format!(
            "1\t[002/114] \"{twice}.part001.rar\" (1/512)\tx@ngPost.com\t26 Oct 26 04:58:14 GMT\t<a@ngPost>\t\t730143\t5657\t"
        );
        let (a, b) = (parse_overview(eweka).unwrap(), parse_overview(xsnews.as_bytes()).unwrap());
        assert_eq!(a.subject, b.subject);
        assert!(a.subject.contains("CultFilms\u{2122}.part001.rar"), "{}", a.subject);
    }

    #[test]
    fn mojibake_is_undone_only_where_it_is_mojibake() {
        use super::undo_mojibake as undo;
        let s = |t: &str| t.to_string();
        // once, twice (a subject posted that way, then encoded again by the server), and Latin-1 controls
        assert_eq!(undo(s("espa\u{c3}\u{b1}ol")), "espa\u{f1}ol");
        assert_eq!(undo(s("He\u{c3}\u{a2}\u{e2}\u{201a}\u{ac}\u{e2}\u{201e}\u{a2}s")), "He\u{2019}s");
        assert_eq!(undo(s("CultFilms\u{e2}\u{84}\u{a2}")), "CultFilms\u{2122}");
        // real text stays: an accent next to ASCII isnt UTF-8, nor is a dash before one, and other scripts dont map
        for real in [
            "Caf\u{e9}",
            "Pok\u{e9}mon \u{2013} S\u{e9}rie",
            "\u{41c}\u{43e}\u{441}\u{43a}\u{432}\u{430}",
            "na\u{ef}ve",
            "\u{2122}",
            "plain",
            "\u{fffd}x",
        ] {
            assert_eq!(undo(s(real)), real);
        }
        // mixed with a char no cp1252 byte makes: left as it is
        assert_eq!(undo(s("espa\u{c3}\u{b1}ol \u{4e2d}")), "espa\u{c3}\u{b1}ol \u{4e2d}");
    }

    #[test]
    fn overview_empty_and_zero_lines_preserve_bytes_and_two_digit_date_years() {
        // Empirical probe (2026-11-03): Eweka/Newshosting omit Lines (empty) on 55-64% of rows;
        // XSNews sends "0" on 64% of rows; all providers send 2-digit Date years on 13-68% of rows.
        let eweka_row = b"2501325020\t[1/8] - \"sample.mkv\" yEnc (1/20)\tposter@usenet.test\t03 Nov 26 03:20:32 GMT\t<1793676032419$1@nyuu>\t\t788233\t\tXref: news.eweka.nl alt.binaries.test:2501325020";
        let xsnews_row = b"1548224143\t[2/8] - \"sample.mkv\" yEnc (1/20)\tposter@usenet.test\tTue, 03 Nov 26 03:20:33 +0000\t<1793676033000$2@nyuu>\t\t791848\t0\tXref: reader.xsnews.nl alt.binaries.test:1548224143";

        let ov_eweka = parse_overview(eweka_row).expect("must parse row with empty Lines");
        assert_eq!(ov_eweka.bytes, 788_233);
        assert_eq!(ov_eweka.lines, 0);
        let art_eweka = ov_eweka.into_article();
        assert_eq!(art_eweka.bytes, 788_233);
        assert_eq!(art_eweka.date, "2026-11-03 03:20:32");

        let ov_xsnews = parse_overview(xsnews_row).expect("must parse row with 0 Lines");
        assert_eq!(ov_xsnews.bytes, 791_848);
        assert_eq!(ov_xsnews.lines, 0);
        let art_xsnews = ov_xsnews.into_article();
        assert_eq!(art_xsnews.bytes, 791_848);
        assert_eq!(art_xsnews.date, "2026-11-03 03:20:33");
    }

    #[test]
    fn highwinds_xzver_hang_guard_and_xzver_deflate_overview_decode() {
        use flate2::write::DeflateEncoder;
        use flate2::Compression;
        use std::io::Write;

        assert!(is_highwinds_xzver_hang_host("news.eweka.nl:563"));
        assert!(is_highwinds_xzver_hang_host("news.newshosting.com:563"));
        assert!(!is_highwinds_xzver_hang_host("reader.xsnews.nl:563"));
        assert!(!is_highwinds_xzver_hang_host("farm.newsgroupdirect.com:563"));

        // Build a raw-deflate + yEnc-encoded XZVER payload (as returned by XSNews / Usenet.Farm)
        let plain_overview = b"100\tsubj 1\tfrom@test\t03 Nov 26 03:20:38 GMT\t<id1@test>\t\t791848\t0\tXref: reader.xsnews.nl alt.binaries.test:100\r\n101\tsubj 2\tfrom@test\t03 Nov 26 03:20:39 GMT\t<id2@test>\t\t384000\t\tXref: reader.xsnews.nl alt.binaries.test:101\r\n";
        let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
        enc.write_all(plain_overview).unwrap();
        let deflated = enc.finish().unwrap();

        let mut yenc_body = Vec::new();
        for &b in &deflated {
            let shifted = b.wrapping_add(42);
            if matches!(shifted, 0x00 | 0x0A | 0x0D | b'=' | b'.') {
                yenc_body.push(b'=');
                yenc_body.push(shifted.wrapping_add(64));
            } else {
                yenc_body.push(shifted);
            }
        }
        let lines = vec![
            format!("=ybegin line=128 size={} name=xzver", deflated.len()).into_bytes(),
            yenc_body,
            format!("=yend size={}", deflated.len()).into_bytes(),
        ];

        let overviews = decode_xzver_lines(&lines).expect("must decode XZVER yEnc+deflate overview");
        assert_eq!(overviews.len(), 2);
        assert_eq!(overviews[0].number, 100);
        assert_eq!(overviews[0].bytes, 791_848);
        assert_eq!(overviews[1].number, 101);
        assert_eq!(overviews[1].bytes, 384_000);

        // A non-final stored DEFLATE block (BFINAL=0, BTYPE=00) truncated before a final block must error
        let row = b"100\tsubj 1\tfrom@test\t03 Nov 26 03:20:38 GMT\t<id1@test>\t\t791848\t0\r\n";
        let len = row.len() as u16;
        let nlen = !len;
        let mut non_final_deflate = vec![0x00]; // BFINAL=0, BTYPE=00 (stored block)
        non_final_deflate.extend_from_slice(&len.to_le_bytes());
        non_final_deflate.extend_from_slice(&nlen.to_le_bytes());
        non_final_deflate.extend_from_slice(row);
        let mut truncated_yenc = Vec::new();
        for &b in &non_final_deflate {
            let shifted = b.wrapping_add(42);
            if matches!(shifted, 0x00 | 0x0A | 0x0D | b'=' | b'.') {
                truncated_yenc.push(b'=');
                truncated_yenc.push(shifted.wrapping_add(64));
            } else {
                truncated_yenc.push(shifted);
            }
        }
        let truncated_lines = vec![
            format!("=ybegin line=128 size={} name=xzver", non_final_deflate.len()).into_bytes(),
            truncated_yenc,
            format!("=yend size={}", non_final_deflate.len()).into_bytes(),
        ];
        assert!(
            matches!(decode_xzver_lines(&truncated_lines), Err(NntpError::Decompress(_))),
            "truncated XZVER stream without BFINAL=1 must return NntpError::Decompress"
        );
    }

    #[test]
    fn conn_server_date_utc_parses_111_response() {
        run(async {
            let (client, mut server) = tokio::io::duplex(4096);
            let mut conn = Conn::over(Box::new(client), Duration::from_secs(5));

            tokio::spawn(async move {
                let mut reader = BufReader::new(&mut server);
                let mut cmd = String::new();
                reader.read_line(&mut cmd).await.unwrap();
                assert_eq!(cmd, "DATE\r\n");
                reader
                    .get_mut()
                    .write_all(b"111 20261103032044\r\n")
                    .await
                    .unwrap();
            });

            let dt = conn.server_date_utc().await.unwrap();
            assert_eq!(dt.to_rfc3339(), "2026-11-03T03:20:44+00:00");
        });
    }
}

#[cfg(test)]
mod xzver_fallback_tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    const XOVER_ROWS: &[u8] = b"224 ok\r\n1\ta\tp\tdate\t<1@x>\t\t10\t1\r\n2\tb\tp\tdate\t<2@x>\t\t10\t1\r\n.\r\n";

    /// An XZVER reply as XSNews sends it: the listing deflated, then yEnc encoded.
    fn xzver_reply() -> Vec<u8> {
        xzver_of(b"1\ta\tp\tdate\t<1@x>\t\t10\t1\r\n2\tb\tp\tdate\t<2@x>\t\t10\t1\r\n")
    }

    fn xzver_of(listing: &[u8]) -> Vec<u8> {
        use flate2::Compression;
        use flate2::write::DeflateEncoder;
        use std::io::Write;
        let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
        enc.write_all(listing).unwrap();
        let deflated = enc.finish().unwrap();
        let mut out = format!("224 compressed\r\n=ybegin line=128 size={} name=xzver\r\n", deflated.len()).into_bytes();
        for &b in &deflated {
            let e = b.wrapping_add(42);
            if matches!(e, 0 | b'\n' | b'\r' | b'=' | b'.') {
                out.extend_from_slice(&[b'=', e.wrapping_add(64)]);
            } else {
                out.push(e);
            }
        }
        out.extend_from_slice(format!("\r\n=yend size={}\r\n.\r\n", deflated.len()).as_bytes());
        out
    }

    /// How the fake server answers `XZVER`.
    #[derive(Clone)]
    enum Answer {
        With(Vec<u8>),
        /// once three XOVERs came in
        AfterThreeXovers(Vec<u8>),
        /// the first time, then hangs up
        OnceThenClose(Vec<u8>),
        /// never
        Hang,
        /// hangs up
        Close,
    }

    async fn serve(xzver: Vec<u8>) -> (u16, Arc<Mutex<Vec<String>>>) {
        serve_with(Answer::With(xzver)).await
    }

    /// A server that refuses `XFEATURE COMPRESS GZIP` and answers `XZVER` as `xzver` says, logging every command.
    async fn serve_with(xzver: Answer) -> (u16, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let log = Arc::new(Mutex::new(Vec::new()));
        let seen = log.clone();
        let xzvers = Arc::new(AtomicUsize::new(0));
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else { return };
                let (seen, xzver, xzvers) = (seen.clone(), xzver.clone(), xzvers.clone());
                tokio::spawn(async move {
                    let (read, mut write) = sock.into_split();
                    let mut lines = tokio::io::BufReader::new(read);
                    let _ = write.write_all(b"200 hi\r\n").await;
                    loop {
                        let mut line = String::new();
                        if tokio::io::AsyncBufReadExt::read_line(&mut lines, &mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        let verb = line.split_whitespace().next().unwrap_or("").to_string();
                        seen.lock().unwrap().push(verb.clone());
                        if verb == "XZVER" {
                            match &xzver {
                                Answer::With(r) => {
                                    let _ = write.write_all(r).await;
                                }
                                Answer::AfterThreeXovers(r) => {
                                    for _ in 0..500 {
                                        if seen.lock().unwrap().iter().filter(|v| *v == "XOVER").count() >= 3 {
                                            break;
                                        }
                                        tokio::time::sleep(Duration::from_millis(10)).await;
                                    }
                                    let _ = write.write_all(r).await;
                                }
                                Answer::OnceThenClose(r) => {
                                    if xzvers.fetch_add(1, Ordering::SeqCst) > 0 {
                                        return;
                                    }
                                    let _ = write.write_all(r).await;
                                }
                                Answer::Hang => tokio::time::sleep(Duration::from_secs(60)).await,
                                Answer::Close => return,
                            }
                            continue;
                        }
                        let reply: &[u8] = match verb.as_str() {
                            "AUTHINFO" if line.contains("USER") => b"381 more\r\n",
                            "AUTHINFO" => b"281 ok\r\n",
                            "GROUP" => b"211 2 1 2 g\r\n",
                            "XFEATURE" => b"500 unknown command\r\n",
                            "XOVER" => XOVER_ROWS,
                            _ => b"500 what\r\n",
                        };
                        let _ = write.write_all(reply).await;
                    }
                });
            }
        });
        (port, log)
    }

    fn run<F: Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap().block_on(f)
    }

    fn pool(port: u16, compress: bool) -> Pool {
        let mut server = UsenetServer::new("127.0.0.1", "u", "p", port);
        server.ssl = Some(false);
        server.compress = Some(compress);
        Pool::new(&[server])
    }

    fn count(log: &Mutex<Vec<String>>, verb: &str) -> usize {
        log.lock().unwrap().iter().filter(|v| *v == verb).count()
    }

    fn numbers(rows: &[Overview]) -> Vec<u64> {
        rows.iter().map(|o| o.number).collect()
    }

    #[test]
    fn a_server_that_refuses_gzip_is_read_with_xzver() {
        run(async {
            let (port, log) = serve(xzver_reply()).await;
            let pool = pool(port, true);
            assert_eq!(numbers(&pool.xover_on(0, "g", 1, 2).await.unwrap()), [1, 2]);
            assert_eq!(numbers(&pool.xover_on(0, "g", 1, 2).await.unwrap()), [1, 2]);
            assert_eq!(count(&log, "XZVER"), 2);
            assert_eq!(count(&log, "XOVER"), 0);
            assert_eq!(*pool.servers[0].xzver.lock().unwrap(), Xzver::Works);
        });
    }

    #[test]
    fn xzver_refused_falls_back_to_xover_and_isnt_asked_again() {
        run(async {
            let (port, log) = serve(b"500 unknown command\r\n".to_vec()).await;
            let pool = pool(port, true);
            assert_eq!(numbers(&pool.xover_on(0, "g", 1, 2).await.unwrap()), [1, 2]);
            assert_eq!(numbers(&pool.xover_on(0, "g", 1, 2).await.unwrap()), [1, 2]);
            assert_eq!(count(&log, "XZVER"), 1, "probed once");
            assert_eq!(count(&log, "XOVER"), 2);
            assert!(matches!(*pool.servers[0].xzver.lock().unwrap(), Xzver::Refused(_)));
            // the same connection took the XOVER after the refusal
            assert_eq!(count(&log, "AUTHINFO"), 2, "one login");
        });
    }

    #[test]
    fn an_unreadable_xzver_listing_falls_back_to_xover() {
        run(async {
            let (port, log) =
                serve(b"224 compressed\r\n=ybegin line=128 size=3 name=xzver\r\nabc\r\n=yend\r\n.\r\n".to_vec()).await;
            let pool = pool(port, true);
            assert_eq!(numbers(&pool.xover_on(0, "g", 1, 2).await.unwrap()), [1, 2]);
            assert_eq!(numbers(&pool.xover_on(0, "g", 1, 2).await.unwrap()), [1, 2]);
            assert_eq!(count(&log, "XZVER"), 1);
            assert_eq!(count(&log, "XOVER"), 2);
        });
    }

    #[test]
    fn xzver_is_asked_again_after_the_retry_wait() {
        run(async {
            let (port, log) = serve(b"500 unknown command\r\n".to_vec()).await;
            let pool = pool(port, true).with_compress_retry(Duration::ZERO);
            pool.xover_on(0, "g", 1, 2).await.unwrap();
            pool.xover_on(0, "g", 1, 2).await.unwrap();
            assert_eq!(count(&log, "XZVER"), 2);
        });
    }

    #[test]
    fn no_xzver_when_compression_is_off() {
        run(async {
            let (port, log) = serve(xzver_reply()).await;
            let pool = pool(port, false);
            assert_eq!(numbers(&pool.xover_on(0, "g", 1, 2).await.unwrap()), [1, 2]);
            assert_eq!(count(&log, "XFEATURE"), 0);
            assert_eq!(count(&log, "XZVER"), 0);
        });
    }

    #[test]
    fn an_empty_range_counts_as_xzver_working() {
        run(async {
            let (port, log) = serve(b"423 no articles in that range\r\n".to_vec()).await;
            let pool = pool(port, true);
            assert!(pool.xover_on(0, "g", 1, 2).await.unwrap_err().is_empty_range());
            assert_eq!(count(&log, "XOVER"), 0);
            assert_eq!(*pool.servers[0].xzver.lock().unwrap(), Xzver::Works);
        });
    }

    #[test]
    fn a_hung_probe_is_redone_with_xover_on_another_connection() {
        run(async {
            let (port, log) = serve_with(Answer::Hang).await;
            let mut pool = pool(port, true);
            pool.xover_deadline = Duration::from_millis(300);
            assert_eq!(numbers(&pool.xover_on(0, "g", 1, 2).await.unwrap()), [1, 2]);
            assert_eq!(numbers(&pool.xover_on(0, "g", 1, 2).await.unwrap()), [1, 2]);
            assert_eq!(count(&log, "XZVER"), 1);
            assert_eq!(count(&log, "XOVER"), 2);
            assert!(matches!(*pool.servers[0].xzver.lock().unwrap(), Xzver::Refused(_)));
        });
    }

    /// a wide probe that times out may only have been slow: the slice is read with XOVER, and XZVER is refused
    /// after `GROUP_REFUSALS` of them, not the first
    #[test]
    fn a_wide_probe_that_times_out_is_read_with_xover() {
        run(async {
            let (port, log) = serve_with(Answer::Hang).await;
            let mut pool = pool(port, true);
            pool.xover_deadline = Duration::from_millis(300);
            assert!(pool.xover_on(0, "g", 1, 2 * MIN_CHUNK).await.is_ok());
            assert_eq!(count(&log, "XZVER"), 1);
            assert_eq!(count(&log, "XOVER"), 1, "the whole slice, not halves");
            assert!(matches!(*pool.servers[0].xzver.lock().unwrap(), Xzver::Cooling(_)));
            // cooling: the next listing doesnt probe
            assert!(pool.xover_on(0, "g", 1, 2 * MIN_CHUNK).await.is_ok());
            assert_eq!(count(&log, "XZVER"), 1);
            // past the cooldown, and none from now on: a probe a listing, refused after `GROUP_REFUSALS` in all
            pool.xzver_cooldown = Duration::ZERO;
            *pool.servers[0].xzver.lock().unwrap() = Xzver::Cooling(Instant::now());
            for _ in 1..GROUP_REFUSALS {
                assert!(pool.xover_on(0, "g", 1, 2 * MIN_CHUNK).await.is_ok());
            }
            assert!(matches!(*pool.servers[0].xzver.lock().unwrap(), Xzver::Refused(_)));
        });
    }

    #[test]
    fn a_server_that_hangs_up_on_xzver_gets_xover() {
        run(async {
            let (port, log) = serve_with(Answer::Close).await;
            let mut pool = pool(port, true);
            pool.xzver_cooldown = Duration::ZERO;
            for _ in 0..(GROUP_REFUSALS + 1) {
                assert_eq!(numbers(&pool.xover_on(0, "g", 1, 2).await.unwrap()), [1, 2]);
            }
            assert_eq!(count(&log, "XZVER"), GROUP_REFUSALS, "a hang-up alone could be anything: refused after a few");
            assert!(matches!(*pool.servers[0].xzver.lock().unwrap(), Xzver::Refused(_)));
        });
    }

    #[test]
    fn a_5xx_to_the_probe_refuses_xzver() {
        run(async {
            let (port, log) = serve(b"502 not for you\r\n".to_vec()).await;
            let pool = pool(port, true);
            pool.xover_on(0, "g", 1, 2).await.unwrap();
            pool.xover_on(0, "g", 1, 2).await.unwrap();
            assert_eq!(count(&log, "XZVER"), 1);
        });
    }

    #[test]
    fn a_4xx_to_the_probe_refuses_xzver() {
        run(async {
            let (port, log) = serve(b"480 later\r\n".to_vec()).await;
            let pool = pool(port, true);
            assert_eq!(numbers(&pool.xover_on(0, "g", 1, 2).await.unwrap()), [1, 2]);
            assert_eq!(numbers(&pool.xover_on(0, "g", 1, 2).await.unwrap()), [1, 2]);
            assert!(matches!(*pool.servers[0].xzver.lock().unwrap(), Xzver::Refused(_)));
            assert_eq!(count(&log, "XZVER"), 1);
        });
    }

    #[test]
    fn one_listing_at_a_time_probes() {
        run(async {
            let (port, log) = serve_with(Answer::AfterThreeXovers(xzver_reply())).await;
            let mut server = UsenetServer::new("127.0.0.1", "u", "p", port);
            server.ssl = Some(false);
            server.compress = Some(true);
            server.connections = Some(4);
            let pool = Arc::new(Pool::new(&[server]));
            let listings: Vec<_> = (0..4)
                .map(|_| {
                    let pool = pool.clone();
                    tokio::spawn(async move { pool.xover_on(0, "g", 1, 2).await.map(|r| numbers(&r)) })
                })
                .collect();
            for l in listings {
                assert_eq!(l.await.unwrap().unwrap(), [1, 2]);
            }
            assert_eq!(count(&log, "XZVER"), 1, "the others went plain while it was probing");
            assert_eq!(count(&log, "XOVER"), 3);
            assert_eq!(*pool.servers[0].xzver.lock().unwrap(), Xzver::Works);
        });
    }

    #[test]
    fn an_empty_probe_is_read_again_with_xover_and_refused_when_xover_has_rows() {
        run(async {
            let (port, log) = serve(xzver_of(b"")).await;
            let pool = pool(port, true);
            assert_eq!(numbers(&pool.xover_on(0, "g", 1, 2).await.unwrap()), [1, 2]);
            assert_eq!(count(&log, "XOVER"), 1);
            assert!(matches!(*pool.servers[0].xzver.lock().unwrap(), Xzver::Refused(_)), "XOVER had rows XZVER didnt");
        });
    }

    /// XZVER works, then the server hangs up on it: each slice is read with XOVER, and after `GROUP_REFUSALS`
    /// in a row XZVER is off.
    #[test]
    fn xzver_that_stops_working_is_turned_off() {
        run(async {
            let (port, log) = serve_with(Answer::OnceThenClose(xzver_reply())).await;
            let pool = pool(port, true);
            for _ in 0..(1 + GROUP_REFUSALS + 1) {
                assert_eq!(numbers(&pool.xover_on(0, "g", 1, 2).await.unwrap()), [1, 2]);
            }
            assert_eq!(count(&log, "XZVER"), 1 + GROUP_REFUSALS);
            assert_eq!(count(&log, "XOVER"), GROUP_REFUSALS + 1);
            assert!(matches!(*pool.servers[0].xzver.lock().unwrap(), Xzver::Refused(_)));
        });
    }

    /// A gzip server that hangs up on the first XOVER and sends an unreadable compressed listing on the retry: the
    /// slice is read plain on a third connection, not handed to the indexer as a deflate error.
    #[test]
    fn an_unreadable_listing_after_a_hang_up_is_read_plain() {
        run(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let connections = Arc::new(AtomicUsize::new(0));
            let seen = connections.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((sock, _)) = listener.accept().await else { return };
                    let n = seen.fetch_add(1, Ordering::SeqCst);
                    tokio::spawn(async move {
                        let (read, mut write) = sock.into_split();
                        let mut lines = tokio::io::BufReader::new(read);
                        let _ = write.write_all(b"200 hi\r\n").await;
                        loop {
                            let mut line = String::new();
                            if tokio::io::AsyncBufReadExt::read_line(&mut lines, &mut line).await.unwrap_or(0) == 0 {
                                return;
                            }
                            let reply: &[u8] = match line.split_whitespace().next().unwrap_or("") {
                                "AUTHINFO" if line.contains("USER") => b"381 more\r\n",
                                "AUTHINFO" => b"281 ok\r\n",
                                "GROUP" => b"211 2 1 2 g\r\n",
                                "XFEATURE" => b"290 compressing\r\n",
                                "XOVER" if n == 0 => return,
                                "XOVER" if n == 1 => b"224 ok [COMPRESS=GZIP]\r\nnot a deflate stream at all\r\n.\r\n",
                                "XOVER" => XOVER_ROWS,
                                _ => b"500 what\r\n",
                            };
                            let _ = write.write_all(reply).await;
                        }
                    });
                }
            });
            let pool = pool(port, true);
            assert_eq!(numbers(&pool.xover_on(0, "g", 1, 2).await.unwrap()), [1, 2]);
            assert_eq!(connections.load(Ordering::SeqCst), 3);
        });
    }

    /// a "400" to XZVER means the server is closing: the slice is read on another connection
    #[test]
    fn a_400_to_xzver_reads_the_slice_on_another_connection() {
        run(async {
            let (port, log) = serve(b"400 closing\r\n".to_vec()).await;
            let pool = pool(port, true);
            assert_eq!(numbers(&pool.xover_on(0, "g", 1, 2).await.unwrap()), [1, 2]);
            assert_eq!(count(&log, "AUTHINFO"), 4, "two logins");
            assert!(matches!(*pool.servers[0].xzver.lock().unwrap(), Xzver::Cooling(_)));
        });
    }
}
