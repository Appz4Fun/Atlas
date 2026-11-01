//! Polling a feed: within a budget, resumable after anything, and a server's delay is never shortened.
//!
//! Ported from newznab-tmux `app/Console/Commands/PredbImportFeed.php` and `config/predb_feeds.php`, GPLv3.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rusqlite::{Connection, params};

use crate::config::PredbConfig;

use super::feeds::{FeedSpec, ParseError, page_url};
use super::import::{Counts, Entry, import};

const MAX_BODY: u64 = 8 << 20;
const MAX_BACKOFF: i64 = 21_600;
const RATE_LIMIT_DEFAULT: i64 = 60;

pub struct HttpReply {
    pub status: u16,
    pub retry_after: Option<String>,
    pub rate_reset: Option<String>,
    pub body: String,
}

pub trait Fetch: Send + Sync {
    fn get(&self, url: &str) -> Result<HttpReply, String>;
}

/// Counts `UreqFetch::get` calls. Every test but the one loopback test asserts this stays 0.
#[doc(hidden)]
pub static REAL_REQUESTS: AtomicUsize = AtomicUsize::new(0);

pub struct UreqFetch {
    agent: ureq::Agent,
}

impl UreqFetch {
    pub fn new(timeout: Duration) -> UreqFetch {
        // no redirects: a feed could aim one at a local service, and one that moves needs a new FeedSpec
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .user_agent(concat!("Atlas/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        UreqFetch { agent }
    }
}

impl Fetch for UreqFetch {
    fn get(&self, url: &str) -> Result<HttpReply, String> {
        REAL_REQUESTS.fetch_add(1, Ordering::Relaxed);
        let mut resp = self.agent.get(url).call().map_err(|e| e.to_string())?;
        let header = |name: &str| resp.headers().get(name).and_then(|v| v.to_str().ok()).map(String::from);
        let (status, retry_after, rate_reset) =
            (resp.status().as_u16(), header("retry-after"), header("x-ratelimit-reset"));
        let body = resp.body_mut().with_config().limit(MAX_BODY).read_to_string().map_err(|e| e.to_string())?;
        Ok(HttpReply { status, retry_after, rate_reset, body })
    }
}

#[derive(Debug, PartialEq)]
pub enum PollEnd {
    CaughtUp,
    Gap,
    Failed(String),
    RateLimited(i64),
    Stopped,
}

/// `Retry-After` as digits; else as an RFC 2822 date; else `X-RateLimit-Reset` as epoch seconds; else the default.
/// At least 1 and at most a week. A server's delay is never shortened.
pub fn wait_seconds(reply: &HttpReply, now: i64, default: i64) -> i64 {
    let after = reply.retry_after.as_deref().map(str::trim);
    let wait = after
        .and_then(|a| a.parse::<i64>().ok())
        .or_else(|| {
            after
                .and_then(|a| chrono::DateTime::parse_from_rfc2822(a).ok())
                .map(|at| at.timestamp().saturating_sub(now))
        })
        .or_else(|| {
            reply.rate_reset.as_deref().and_then(|r| r.trim().parse::<i64>().ok()).map(|at| at.saturating_sub(now))
        })
        .unwrap_or(default);
    wait.clamp(1, 604_800)
}

/// why a page came to nothing
enum Trouble {
    Failed(String),
    RateLimited(i64),
    Stopped,
}

struct Poller<'a> {
    fetch: &'a dyn Fetch,
    spec: &'a FeedSpec,
    page_size: u32,
    delay: Duration,
    pause: &'a dyn Fn(Duration) -> bool,
    now: i64,
    first: bool,
}

impl Poller<'_> {
    fn page(&mut self, page: i64) -> Result<Vec<Entry>, Trouble> {
        if !self.first && !(self.pause)(self.delay) {
            return Err(Trouble::Stopped);
        }
        self.first = false;
        let page = u32::try_from(page).unwrap_or(u32::MAX);
        let reply = self.fetch.get(&page_url(self.spec, page, self.page_size)).map_err(Trouble::Failed)?;
        if reply.status == 429 {
            return Err(Trouble::RateLimited(wait_seconds(&reply, self.now, RATE_LIMIT_DEFAULT)));
        }
        if reply.status != 200 {
            return Err(Trouble::Failed(format!("HTTP {}", reply.status)));
        }
        let value: serde_json::Value =
            serde_json::from_str(&reply.body).map_err(|e| Trouble::Failed(format!("bad JSON: {e}")))?;
        (self.spec.parse)(&value, page).map_err(|e| match e {
            ParseError::RateLimited(_) => Trouble::RateLimited(wait_seconds(&reply, self.now, RATE_LIMIT_DEFAULT)),
            ParseError::Shape(why) => Trouble::Failed(why),
        })
    }
}

fn past_the_end(spec: &FeedSpec, page: i64) -> bool {
    spec.max_page.is_some_and(|m| page > i64::from(m))
}

fn set_cursor(conn: &Connection, source: i64, page: i64, floor: i64) -> rusqlite::Result<()> {
    conn.execute("update feeds set resume_page = ?1, gap_floor = ?2 where source = ?3", params![page, floor, source])?;
    Ok(())
}

fn add_counts(conn: &Connection, source: i64, c: &Counts) -> rusqlite::Result<()> {
    conn.execute(
        "update feeds set inserted = inserted + ?1, updated = updated + ?2 where source = ?3",
        params![c.inserted as i64, c.updated as i64, source],
    )?;
    Ok(())
}

struct Row {
    failures: i64,
    resume_page: i64,
    gap_floor: i64,
    history_page: i64,
    history_read: i64,
    last_ok_at: Option<i64>,
}

/// the steps of a poll that use the network: what ended them, or how they came out
fn walk(conn: &mut Connection, p: &mut Poller<'_>, row: &Row, cfg: &PredbConfig) -> Result<bool, TroubleOrDb> {
    let spec = p.spec;
    let (source, now) = (spec.id, p.now);
    let mut budget = i64::from(cfg.max_pages_per_poll.max(1));
    let fresh = row.last_ok_at.is_none();
    let open = row.resume_page;
    // the open gap's floor stays: walking down from a new cursor passes through what the old one left
    let floor = if open > 0 {
        row.gap_floor
    } else {
        conn.query_row("select coalesce(max(id), 0) from titles", [], |r| r.get(0))?
    };

    // forward, newest first
    let (mut page, mut caught, mut ended) = (1i64, false, false);
    while budget > 0 {
        if past_the_end(spec, page) {
            ended = true;
            break;
        }
        let entries = p.page(page)?;
        budget -= 1;
        if entries.is_empty() {
            ended = true;
            break;
        }
        let cursor = (!fresh).then_some((page + 1, floor));
        let counts = import(conn, source, &entries, now, cursor)?;
        add_counts(conn, source, &counts)?;
        if counts.inserted == 0 {
            caught = true;
            break;
        }
        page += 1;
    }
    if ended {
        // nothing left below: no gap, whichever was open
        set_cursor(conn, source, 0, 0)?;
    } else if !caught {
        // out of budget. The cursor says where to go on, unless older pages are history
        if fresh {
            set_cursor(conn, source, 0, 0)?;
        } else {
            return Ok(true);
        }
        return Ok(false);
    } else {
        set_cursor(conn, source, open, if open > 0 { row.gap_floor } else { 0 })?;
    }

    // the gap, down to a title from before it
    let mut in_gap = caught && open > 0;
    let mut page = open;
    while in_gap {
        if budget == 0 {
            set_cursor(conn, source, page, row.gap_floor)?;
            return Ok(true);
        }
        if past_the_end(spec, page) {
            in_gap = false;
            continue;
        }
        let entries = p.page(page)?;
        budget -= 1;
        if entries.is_empty() {
            in_gap = false;
            continue;
        }
        let counts = import(conn, source, &entries, now, Some((page + 1, row.gap_floor)))?;
        add_counts(conn, source, &counts)?;
        if counts.inserted == 0 && counts.min_known_id.is_some_and(|m| m <= row.gap_floor) {
            in_gap = false;
            continue;
        }
        page += 1;
        set_cursor(conn, source, page, row.gap_floor)?;
    }
    if caught && open > 0 {
        set_cursor(conn, source, 0, 0)?;
    }

    // history, on purpose only
    let (mut read, mut page) = (row.history_read, row.history_page.max(2));
    while budget > 0 && read < i64::from(cfg.history_pages) {
        if past_the_end(spec, page) {
            read = i64::from(cfg.history_pages);
            conn.execute("update feeds set history_read = ?1 where source = ?2", params![read, source])?;
            break;
        }
        let entries = p.page(page)?;
        budget -= 1;
        if entries.is_empty() {
            read = i64::from(cfg.history_pages);
            conn.execute("update feeds set history_read = ?1 where source = ?2", params![read, source])?;
            break;
        }
        let counts = import(conn, source, &entries, now, None)?;
        add_counts(conn, source, &counts)?;
        page += 1;
        read += 1;
        conn.execute(
            "update feeds set history_page = ?1, history_read = ?2 where source = ?3",
            params![page, read, source],
        )?;
    }
    Ok(false)
}

enum TroubleOrDb {
    Trouble(Trouble),
    Db(rusqlite::Error),
}

impl From<Trouble> for TroubleOrDb {
    fn from(t: Trouble) -> Self {
        TroubleOrDb::Trouble(t)
    }
}

impl From<rusqlite::Error> for TroubleOrDb {
    fn from(e: rusqlite::Error) -> Self {
        TroubleOrDb::Db(e)
    }
}

fn file_bytes(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("select page_count * page_size from pragma_page_count(), pragma_page_size()", [], |r| r.get(0))
}

fn failed(
    conn: &Connection,
    source: i64,
    why: String,
    failures: i64,
    poll_secs: i64,
    now: i64,
) -> rusqlite::Result<PollEnd> {
    let failures = failures + 1;
    let wait = poll_secs.saturating_mul(1i64 << failures.min(16)).min(MAX_BACKOFF);
    conn.execute(
        "update feeds set failures = ?1, last_error = ?2, next_poll_at = ?3 where source = ?4",
        params![failures, why, now + wait, source],
    )?;
    Ok(PollEnd::Failed(why))
}

/// `pause` naps between requests and returns false when stopping. Writes `feeds` before returning.
pub fn poll_source(
    conn: &mut Connection,
    fetch: &dyn Fetch,
    spec: &FeedSpec,
    cfg: &PredbConfig,
    now: i64,
    pause: &dyn Fn(Duration) -> bool,
) -> rusqlite::Result<PollEnd> {
    conn.execute("insert or ignore into feeds (source, key) values (?1, ?2)", params![spec.id, spec.key])?;
    let row = conn.query_row(
        "select failures, resume_page, gap_floor, history_page, history_read, last_ok_at from feeds where source = ?1",
        [spec.id],
        |r| {
            Ok(Row {
                failures: r.get(0)?,
                resume_page: r.get(1)?,
                gap_floor: r.get(2)?,
                history_page: r.get(3)?,
                history_read: r.get(4)?,
                last_ok_at: r.get(5)?,
            })
        },
    )?;
    let poll_secs = i64::from(cfg.poll_minutes.max(1)) * 60;
    if file_bytes(conn)? > cfg.max_db_mb.saturating_mul(1 << 20) {
        return failed(conn, spec.id, "the PreDB file is over max_db_mb".into(), row.failures, poll_secs, now);
    }
    let mut poller = Poller {
        fetch,
        spec,
        page_size: cfg.page_size,
        delay: Duration::from_millis(cfg.request_delay_ms.max(spec.min_delay_ms)),
        pause,
        now,
        first: true,
    };
    match walk(conn, &mut poller, &row, cfg) {
        Ok(gap) => {
            conn.execute(
                "update feeds set failures = 0, last_ok_at = ?1, last_error = null, next_poll_at = ?2 where source = ?3",
                params![now, now + poll_secs + spec.id * 7, spec.id],
            )?;
            Ok(if gap { PollEnd::Gap } else { PollEnd::CaughtUp })
        }
        Err(TroubleOrDb::Db(e)) => Err(e),
        Err(TroubleOrDb::Trouble(Trouble::Stopped)) => Ok(PollEnd::Stopped),
        Err(TroubleOrDb::Trouble(Trouble::Failed(why))) => failed(conn, spec.id, why, row.failures, poll_secs, now),
        Err(TroubleOrDb::Trouble(Trouble::RateLimited(wait))) => {
            conn.execute(
                "update feeds set next_poll_at = ?1, last_error = 'rate limited' where source = ?2",
                params![now + wait, spec.id],
            )?;
            Ok(PollEnd::RateLimited(wait))
        }
    }
}
