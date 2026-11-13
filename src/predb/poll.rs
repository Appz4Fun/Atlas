//! Polling a feed: within a budget, resumable after anything, and a server's delay is never shortened.
//!
//! Ported from newznab-tmux `app/Console/Commands/PredbImportFeed.php` and `config/predb_feeds.php`, GPLv3.

use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const MAX_BODY: u64 = 8 << 20;

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
        let bytes = resp.body_mut().with_config().limit(MAX_BODY).read_to_vec().map_err(|e| e.to_string())?;
        crate::metrics::function_bytes("predb.poll_feed", "received", bytes.len() as u64);
        let body = String::from_utf8(unpack(bytes)?).map_err(|e| e.to_string())?;
        Ok(HttpReply { status, retry_after, rate_reset, body })
    }
}

/// A gzipped body as its text, no longer than any other: a small one can expand to anything.
fn unpack(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    if !bytes.starts_with(&[0x1f, 0x8b]) {
        return Ok(bytes);
    }
    let mut text = Vec::new();
    flate2::read::GzDecoder::new(bytes.as_slice())
        .take(MAX_BODY + 1)
        .read_to_end(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() as u64 > MAX_BODY {
        return Err("a gzipped body over the limit".into());
    }
    Ok(text)
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_wait_vectors_keep_headers_clamps_and_default() {
        let reply = |after: Option<&str>, reset: Option<&str>| HttpReply {
            status: 429,
            retry_after: after.map(str::to_owned),
            rate_reset: reset.map(str::to_owned),
            body: String::new(),
        };
        assert_eq!(wait_seconds(&reply(Some("120"), Some("900")), 100, 60), 120);
        assert_eq!(wait_seconds(&reply(Some("Thu, 01 Jan 1970 00:01:00 +0000"), None), 0, 60), 60);
        assert_eq!(wait_seconds(&reply(None, Some("150")), 100, 60), 50);
        assert_eq!(wait_seconds(&reply(Some("-1"), None), 0, 60), 1);
        assert_eq!(wait_seconds(&reply(Some("9999999"), None), 0, 60), 604800);
        assert_eq!(wait_seconds(&reply(None, None), 0, 60), 60);
    }
    #[test]
    fn feed_body_decoding_preserves_plain_bytes_and_refuses_invalid_gzip() {
        assert_eq!(unpack(b"feed body".to_vec()).unwrap(), b"feed body");
        assert!(unpack(vec![0x1f, 0x8b, 0]).is_err());
    }
}
