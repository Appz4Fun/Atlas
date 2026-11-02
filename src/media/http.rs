//! One provider's HTTP: paced, paused when it says so, and bounded in what it reads.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::resolve::Trouble;

const MAX_BODY: u64 = 2 << 20;
const MAX_TOKEN_WAIT: Duration = Duration::from_secs(2);
const MAX_PAUSE: i64 = 3_600;

pub(crate) fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// A token every `60 / per_minute` s, holding at most one, soo no burst passes the published limit.
pub struct Bucket {
    per_minute: f64,
    tokens: f64,
    last: Instant,
}

impl Bucket {
    pub fn new(per_minute: i64, now: Instant) -> Bucket {
        Bucket { per_minute: per_minute.max(1) as f64, tokens: 1.0, last: now }
    }

    /// Zero: a token was taken. Otherwise how long until there is one, with none taken.
    pub fn wait_for(&mut self, now: Instant) -> Duration {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.per_minute / 60.0).min(1.0);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return Duration::ZERO;
        }
        Duration::from_secs_f64((1.0 - self.tokens) * 60.0 / self.per_minute)
    }
}

fn agent(base: &str) -> ureq::Agent {
    let mut b = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        // a reply's Location is untrusted: it could aim at a local service, or carry TMDB's key elsewhere
        .max_redirects(0)
        .user_agent(concat!("atlas/", env!("CARGO_PKG_VERSION")));
    // a test's loopback server must not go through a proxy from the environment
    if base.starts_with("http://127.0.0.1") {
        b = b.proxy(None);
    }
    b.build().into()
}

#[derive(Default)]
struct Pause {
    until: i64,
    /// the last outage's wait, doubled by the next
    backoff: i64,
}

pub struct Client {
    name: &'static str,
    agent: ureq::Agent,
    bucket: Mutex<Bucket>,
    pause: Mutex<Pause>,
}

pub enum Reply {
    Json(Value),
    NotFound,
}

/// What `Client::outage` is for: no answer, or a 5xx.
fn is_outage(sent: &Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> bool {
    sent.as_ref().map_or(true, |resp| resp.status().is_server_error())
}

impl Client {
    pub fn new(name: &'static str, base: &str, per_minute: i64) -> Client {
        Client {
            name,
            agent: agent(base),
            bucket: Mutex::new(Bucket::new(per_minute, Instant::now())),
            pause: Mutex::default(),
        }
    }

    /// A token, waiting at most 2 s for it.
    fn take_token(&self) -> Result<(), Trouble> {
        loop {
            let wait = self.bucket.lock().unwrap().wait_for(Instant::now());
            if wait.is_zero() {
                return Ok(());
            }
            if wait > MAX_TOKEN_WAIT {
                return Err(Trouble::Paused(now_secs() + 1));
            }
            std::thread::sleep(wait);
        }
    }

    /// As if time had passed: a token back, and any pause over. An outage's doubling stays.
    #[doc(hidden)]
    pub fn advance(&self) {
        self.bucket.lock().unwrap().tokens = 1.0;
        self.pause.lock().unwrap().until = 0;
    }

    fn pause_for(&self, secs: i64) -> i64 {
        let until = now_secs() + secs.clamp(1, MAX_PAUSE);
        let mut pause = self.pause.lock().unwrap();
        pause.until = pause.until.max(until);
        pause.until
    }

    fn outage(&self) -> Trouble {
        let backoff = {
            let mut pause = self.pause.lock().unwrap();
            pause.backoff = if pause.backoff == 0 { 60 } else { (pause.backoff * 2).min(MAX_PAUSE) };
            pause.backoff
        };
        Trouble::Paused(self.pause_for(backoff))
    }

    /// `url` holds any key, soo it's never logged and never in a `Trouble`.
    pub fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<Reply, Trouble> {
        self.ready()?;
        let span = crate::metrics::span("media.request");
        let mut request = self.agent.get(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let sent = request.call();
        self.counted(&span, is_outage(&sent), self.answer(sent))
    }

    /// As `get`, with a body that can hold a key.
    pub fn post_json(&self, url: &str, body: &Value) -> Result<Reply, Trouble> {
        self.ready()?;
        let span = crate::metrics::span("media.request");
        let sent = self.agent.post(url).send_json(body);
        self.counted(&span, is_outage(&sent), self.answer(sent))
    }

    fn counted(
        &self,
        span: &crate::metrics::Span,
        outage: bool,
        reply: Result<Reply, Trouble>,
    ) -> Result<Reply, Trouble> {
        let result = match &reply {
            Ok(Reply::Json(_)) => "ok",
            Ok(Reply::NotFound) => "not_found",
            // what it asked for, as opposed to a pause an outage brought on
            Err(Trouble::Paused(_)) if !outage => "paused",
            Err(_) => {
                span.failed();
                "failed"
            }
        };
        crate::metrics::names::MEDIA_REQUESTS.inc(&[("provider", self.name), ("result", result)], 1);
        reply
    }

    fn ready(&self) -> Result<(), Trouble> {
        {
            let pause = self.pause.lock().unwrap();
            if pause.until > now_secs() {
                return Err(Trouble::Paused(pause.until));
            }
        }
        self.take_token()
    }

    fn answer(&self, sent: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<Reply, Trouble> {
        let Ok(mut resp) = sent else { return Err(self.outage()) };
        match resp.status().as_u16() {
            200 => {
                let body = resp.body_mut().with_config().limit(MAX_BODY).read_json::<Value>();
                let Ok(json) = body else {
                    return Err(Trouble::Bad(format!("{}: a body that doesnt parse", self.name)));
                };
                *self.pause.lock().unwrap() = Pause::default();
                Ok(Reply::Json(json))
            }
            404 => {
                *self.pause.lock().unwrap() = Pause::default();
                Ok(Reply::NotFound)
            }
            429 => {
                let wait = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<i64>().ok())
                    .unwrap_or(60);
                Err(Trouble::Paused(self.pause_for(wait)))
            }
            401 | 403 => {
                // no request goes while paused, soo this is once per pause
                eprintln!("{}: its key was refused", self.name);
                Err(Trouble::BadKey(self.pause_for(MAX_PAUSE)))
            }
            500..=599 => Err(self.outage()),
            status => Err(Trouble::Bad(format!("{}: status {status}", self.name))),
        }
    }
}

/// A query parameter's value.
pub fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A lookup of two requests keeps what its first found when the second can't go yet, per query, soo
/// the next call for it goes on from there and a low rate still finishes.
pub struct Memo<T> {
    entries: Mutex<std::collections::VecDeque<(String, T)>>,
}

const MEMO_SIZE: usize = 1_024;

impl<T: Clone> Memo<T> {
    pub fn new() -> Memo<T> {
        Memo { entries: Mutex::default() }
    }

    pub fn get(&self, key: &str) -> Option<T> {
        self.entries.lock().unwrap().iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    pub fn put(&self, key: &str, value: T) {
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|(k, _)| k != key);
        if entries.len() >= MEMO_SIZE {
            entries.pop_front();
        }
        entries.push_back((key.to_string(), value));
    }

    pub fn forget(&self, key: &str) {
        self.entries.lock().unwrap().retain(|(k, _)| k != key);
    }
}

impl<T: Clone> Default for Memo<T> {
    fn default() -> Self {
        Memo::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bucket_refills_by_the_clock() {
        let start = Instant::now();
        let mut b = Bucket::new(60, start);
        assert_eq!(b.wait_for(start), Duration::ZERO);
        let wait = b.wait_for(start);
        assert!(wait > Duration::from_millis(900) && wait <= Duration::from_secs(1), "{wait:?}");
        assert_eq!(b.wait_for(start + Duration::from_secs(1)), Duration::ZERO);
        // it holds one token however long it waits
        assert_eq!(b.wait_for(start + Duration::from_secs(100)), Duration::ZERO);
        assert!(b.wait_for(start + Duration::from_secs(100)) > Duration::ZERO);
    }

    #[test]
    fn encode_table() {
        assert_eq!(encode("Some Show"), "Some%20Show");
        assert_eq!(encode("A&B=C/D"), "A%26B%3DC%2FD");
        assert_eq!(encode("é"), "%C3%A9");
    }
}
