//! Postgres as the primary store, step 1 of the move off SQLite: the connection pool, the schema's migrations, and
//! what a caller does while Postgres can't be reached. Nothing is stored here yet: `[storage] backend` stays
//! `sqlite` until the tables move over, one area at a time.
//!
//! Everything lives in one schema (`atlas` unless `[storage] schema` says otherwise). Each connection sets its
//! `search_path` to it, so the migrations and every query name tables without a schema, and a test can run the
//! same code in a schema of its own (`testing`).
//!
//! While Postgres is down (a server reboot), `Pool::with_retry` waits and tries again instead of failing, so a
//! caller that writes a cursor after its save never sees the save fail and the cursor move on regardless.

pub mod migrate;
#[cfg(test)]
pub mod testing;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};

pub use postgres::Client;

/// The `[storage] backend` choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Backend {
    #[default]
    Sqlite,
    Postgres,
}

/// The `[storage]` object of config.toml.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageConfig {
    pub backend: Backend,
    /// a Postgres connection string, `host=... port=... user=... password=... dbname=...` or `postgresql://...`
    pub url: String,
    /// the schema everything lives in
    pub schema: String,
    /// connections kept open at most
    pub max_connections: usize,
    /// the connection isn't encrypted: without this, only a loopback or private network address is used
    pub allow_plaintext: bool,
}

impl Default for StorageConfig {
    fn default() -> Self {
        StorageConfig {
            backend: Backend::Sqlite,
            url: String::new(),
            schema: "atlas".into(),
            max_connections: 16,
            allow_plaintext: false,
        }
    }
}

impl StorageConfig {
    pub fn from_value(v: Option<&serde_json::Value>) -> StorageConfig {
        let mut c = StorageConfig::default();
        let Some(v) = v else { return c };
        if let Some(b) = v.get("backend").and_then(serde_json::Value::as_str) {
            c.backend = if b.eq_ignore_ascii_case("postgres") { Backend::Postgres } else { Backend::Sqlite };
        }
        if let Some(u) = v.get("url").and_then(serde_json::Value::as_str) {
            c.url = u.to_string();
        }
        if let Some(s) = v.get("schema").and_then(serde_json::Value::as_str).filter(|s| valid_schema(s)) {
            c.schema = s.to_string();
        }
        if let Some(n) = v.get("max_connections").and_then(serde_json::Value::as_i64) {
            c.max_connections = n.clamp(1, 256) as usize;
        }
        if let Some(b) = v.get("allow_plaintext").and_then(serde_json::Value::as_bool) {
            c.allow_plaintext = b;
        }
        c
    }

    pub fn is_postgres(&self) -> bool {
        self.backend == Backend::Postgres && !self.url.is_empty()
    }
}

/// A schema name that can go into SQL as it is: lowercase letters, digits and `_`, not starting with a digit.
pub fn valid_schema(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && !s.as_bytes()[0].is_ascii_digit()
}

/// Whether `h` is a loopback or private network address, where an unencrypted connection is allowed.
pub fn private_host(h: &str) -> bool {
    if h.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match h.trim_matches(|c| c == '[' || c == ']').parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00,
        Err(_) => false,
    }
}

/// Connections to one schema, reused. A connection found closed is dropped rather than handed out again.
pub struct Pool {
    config: postgres::Config,
    schema: String,
    max: usize,
    idle: Mutex<Vec<Client>>,
}

impl Pool {
    pub fn new(storage: &StorageConfig) -> Result<Pool> {
        if !valid_schema(&storage.schema) {
            bail!("storage: schema {:?} isn't a plain lowercase name", storage.schema);
        }
        let mut config: postgres::Config = storage.url.parse().context("storage: the url")?;
        // a server that's down shouldn't hold a caller for long: a url can still set its own
        if config.get_connect_timeout().is_none() {
            config.connect_timeout(Duration::from_secs(5));
        }
        if !storage.allow_plaintext {
            for host in config.get_hosts() {
                if let postgres::config::Host::Tcp(h) = host
                    && !private_host(h)
                {
                    bail!(
                        "storage: {h} isn't a loopback or private network address; the connection isn't encrypted, set allow_plaintext = true only on a trusted network"
                    );
                }
            }
        }
        Ok(Pool {
            config,
            schema: storage.schema.clone(),
            max: storage.max_connections.max(1),
            idle: Mutex::new(Vec::new()),
        })
    }

    pub fn schema(&self) -> &str {
        &self.schema
    }

    /// A connection, opened when none is idle; its `search_path` is the pool's schema.
    pub fn get(&self) -> Result<Conn<'_>, postgres::Error> {
        let reused = {
            let mut idle = self.idle.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                match idle.pop() {
                    Some(c) if c.is_closed() => continue,
                    other => break other,
                }
            }
        };
        let client = match reused {
            Some(c) => c,
            None => {
                let mut c = self.config.connect(postgres::NoTls)?;
                c.batch_execute(&format!("set search_path to {}", self.schema))?;
                c
            }
        };
        Ok(Conn { pool: self, client: Some(client) })
    }

    /// `work` on a connection, tried again while Postgres can't be reached: every `wait` (doubling from one second
    /// to `MAX_WAIT`) until it can, or until `stop` is set (then the last error). An error that isn't about the
    /// connection (bad SQL, a constraint) comes back at once. `work` may run more than once, so it must be one
    /// transaction or otherwise safe to repeat.
    pub fn with_retry<T>(
        &self,
        stop: &AtomicBool,
        mut work: impl FnMut(&mut Client) -> Result<T, postgres::Error>,
    ) -> Result<T, postgres::Error> {
        let mut wait = Duration::from_secs(1);
        let mut said = false;
        loop {
            let tried = self.get().and_then(|mut c| work(&mut c));
            match tried {
                Ok(v) => {
                    if said {
                        crate::ui::success("postgres is back; carrying on");
                    }
                    return Ok(v);
                }
                Err(e) if is_unreachable(&e) && !stop.load(Ordering::Relaxed) => {
                    if !said {
                        crate::ui::warn(&format!("postgres can't be reached ({e}); waiting for it"));
                        said = true;
                    }
                    sleep_unless_stopped(wait, stop);
                    wait = (wait * 2).min(MAX_WAIT);
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// the longest `with_retry` waits between tries
pub const MAX_WAIT: Duration = Duration::from_secs(30);

fn sleep_unless_stopped(d: Duration, stop: &AtomicBool) {
    let step = Duration::from_millis(100);
    let mut left = d;
    while !left.is_zero() && !stop.load(Ordering::Relaxed) {
        let now = left.min(step);
        std::thread::sleep(now);
        left -= now;
    }
}

/// Whether `e` says Postgres couldn't be reached or the connection went away, rather than that the work was
/// wrong: no SQL state (an I/O or connect error, a closed connection), or one of the states a server shutting down
/// or starting up answers with.
pub fn is_unreachable(e: &postgres::Error) -> bool {
    use postgres::error::SqlState;
    match e.code() {
        None => e.is_closed() || e.as_db_error().is_none(),
        Some(code) => [
            SqlState::ADMIN_SHUTDOWN,
            SqlState::CRASH_SHUTDOWN,
            SqlState::CANNOT_CONNECT_NOW,
            SqlState::CONNECTION_EXCEPTION,
            SqlState::CONNECTION_FAILURE,
            SqlState::CONNECTION_DOES_NOT_EXIST,
            SqlState::SQLCLIENT_UNABLE_TO_ESTABLISH_SQLCONNECTION,
            SqlState::TOO_MANY_CONNECTIONS,
        ]
        .contains(code),
    }
}

/// A pooled connection, back in the pool when dropped (unless it closed, or the pool is full).
pub struct Conn<'a> {
    pool: &'a Pool,
    client: Option<Client>,
}

impl std::ops::Deref for Conn<'_> {
    type Target = Client;
    fn deref(&self) -> &Client {
        self.client.as_ref().expect("a connection until dropped")
    }
}

impl std::ops::DerefMut for Conn<'_> {
    fn deref_mut(&mut self) -> &mut Client {
        self.client.as_mut().expect("a connection until dropped")
    }
}

impl Drop for Conn<'_> {
    fn drop(&mut self) {
        let Some(c) = self.client.take() else { return };
        if c.is_closed() {
            return;
        }
        let mut idle = self.pool.idle.lock().unwrap_or_else(|e| e.into_inner());
        if idle.len() < self.pool.max {
            idle.push(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_config_reads_its_object_and_defaults_to_sqlite() {
        assert_eq!(StorageConfig::from_value(None).backend, Backend::Sqlite);
        let v = serde_json::json!({"backend": "Postgres", "url": "host=10.0.0.2 dbname=atlas", "schema": "atlas2",
                                   "max_connections": 1000, "allow_plaintext": true});
        let c = StorageConfig::from_value(Some(&v));
        assert_eq!(c.backend, Backend::Postgres);
        assert_eq!((c.schema.as_str(), c.max_connections, c.allow_plaintext), ("atlas2", 256, true));
        assert!(c.is_postgres());
        let bad = serde_json::json!({"backend": "postgres", "schema": "x; drop table groups"});
        let c = StorageConfig::from_value(Some(&bad));
        assert_eq!(c.schema, "atlas", "a schema that isn't a plain name is ignored");
        assert!(!c.is_postgres(), "no url, no postgres");
    }

    #[test]
    fn a_public_host_needs_allow_plaintext() {
        let mut c = StorageConfig { url: "host=8.8.8.8 dbname=atlas".into(), ..StorageConfig::default() };
        assert!(Pool::new(&c).is_err());
        c.allow_plaintext = true;
        assert!(Pool::new(&c).is_ok());
        c.url = "host=192.168.1.93 port=5434 dbname=atlas".into();
        c.allow_plaintext = false;
        assert!(Pool::new(&c).is_ok());
    }

    #[test]
    fn schema_names_are_plain() {
        for ok in ["atlas", "t_1", "_x"] {
            assert!(valid_schema(ok), "{ok}");
        }
        for bad in ["", "1a", "Atlas", "a-b", "a.b", "a b", &"x".repeat(64)] {
            assert!(!valid_schema(bad), "{bad}");
        }
    }

    #[test]
    fn an_unreachable_server_is_waited_for_until_stopped() {
        // nothing listens on port 9 of the loopback: every try fails to connect
        let c = StorageConfig {
            url: "host=127.0.0.1 port=9 dbname=x connect_timeout=1".into(),
            ..StorageConfig::default()
        };
        let pool = Pool::new(&c).unwrap();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let tries = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (s, t) = (stop.clone(), tries.clone());
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            s.store(true, Ordering::Relaxed);
        });
        let started = std::time::Instant::now();
        let got = pool.with_retry(&stop, |c| {
            t.fetch_add(1, Ordering::Relaxed);
            c.simple_query("select 1").map(|_| ())
        });
        stopper.join().unwrap();
        assert!(got.is_err() && is_unreachable(got.as_ref().unwrap_err()));
        assert!(started.elapsed() >= Duration::from_millis(1400), "it waited instead of failing at once");
        assert_eq!(tries.load(Ordering::Relaxed), 0, "work never ran: there was no connection to run it on");
    }

    #[test]
    fn a_bad_statement_fails_at_once() {
        let Some(t) = testing::Schema::new() else { return };
        let stop = AtomicBool::new(false);
        let mut n = 0;
        let got = t.pool.with_retry(&stop, |c| {
            n += 1;
            c.batch_execute("select * from no_such_table")
        });
        assert!(got.is_err() && !is_unreachable(got.as_ref().unwrap_err()));
        assert_eq!(n, 1);
    }
}
