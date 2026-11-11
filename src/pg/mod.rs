//! PostgreSQL storage, schema migrations, native ingestion, and isolated migration tooling.
//! The background indexer has an explicit native ingestion profile that validates its destination and refuses
//! auxiliary services that still require their own storage port.
//!
//! Everything lives in one schema (`atlas` unless `[storage] schema` says otherwise). Each connection sets its
//! `search_path` to it, so the migrations and every query name tables without a schema, and a test can run the
//! same code in a schema of its own (`testing`).
//!
//! While Postgres is down (a server reboot), `Pool::with_retry` waits and tries again instead of failing, so a
//! caller pauses rather than moving a cursor on past a save that didn't happen. The work it repeats must be safe to
//! run again even when the first try's commit landed and only its answer was lost (see `with_retry`).

pub mod header_blocks;
pub(crate) mod indexer_runtime;
pub mod ingest;
pub(crate) mod linker;
mod linker_legacy;
mod linker_stage;
pub mod main_import;
pub mod migrate;
pub mod release_queue;
pub mod release_reader;
pub mod release_writer;
pub mod sql;
#[cfg(test)]
pub mod testing;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};

pub use postgres::Client;

/// The `[storage] backend` choice.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Backend {
    #[default]
    Sqlite,
    Postgres,
    /// a name that's neither: refused by `StorageConfig::check` rather than taken for one of them
    Unknown(String),
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
            c.backend = match b.to_ascii_lowercase().as_str() {
                "sqlite" => Backend::Sqlite,
                "postgres" | "postgresql" | "pg" => Backend::Postgres,
                _ => Backend::Unknown(b.to_string()),
            };
        }
        if let Some(u) = v.get("url").and_then(serde_json::Value::as_str) {
            c.url = u.to_string();
        }
        // kept as written: a bad one is refused (`check`), never swapped for the default
        if let Some(s) = v.get("schema").and_then(serde_json::Value::as_str) {
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
        self.backend == Backend::Postgres
    }

    /// An error for a `[storage]` that can't be what was meant: a backend name that's neither, or Postgres with no
    /// url or a schema that isn't a plain name. Checked at start, soo data never goes quietly to the wrong place.
    pub fn check(&self) -> Result<()> {
        match &self.backend {
            Backend::Sqlite => Ok(()),
            Backend::Unknown(b) => bail!("storage: backend {b:?} is neither \"sqlite\" nor \"postgres\""),
            Backend::Postgres if self.url.is_empty() => bail!("storage: backend is postgres but there's no url"),
            Backend::Postgres if !valid_schema(&self.schema) => {
                bail!("storage: schema {:?} isn't a plain lowercase name", self.schema)
            }
            Backend::Postgres => Ok(()),
        }
    }
}

/// A schema name that can go into SQL (always double-quoted): lowercase letters, digits and `_`, not starting with
/// a digit, and not `pg_`, which Postgres keeps for itself.
pub fn valid_schema(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && !s.as_bytes()[0].is_ascii_digit()
        && !s.starts_with("pg_")
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

/// Connections to one schema, reused, `max` open at most (a caller past it waits for one to come back). A
/// connection found closed is dropped rather than handed out again.
pub struct Pool {
    config: postgres::Config,
    schema: String,
    max: usize,
    state: Mutex<PoolState>,
    freed: Condvar,
}

#[derive(Default)]
struct PoolState {
    idle: Vec<Client>,
    /// connections open, idle or handed out
    open: usize,
}

impl Drop for Pool {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
        let clients = std::mem::take(&mut state.idle);
        if clients.is_empty() {
            return;
        }
        // postgres::Client owns a synchronous runtime, including during Drop. The final Arc may be released
        // by an async indexer task; use a plain thread so cleanup is safe even while Tokio is shutting down.
        if tokio::runtime::Handle::try_current().is_ok() {
            std::thread::spawn(move || drop(clients));
        } else {
            drop(clients);
        }
    }
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
        // a server gone without a reset (a crash, a partition) is noticed in about 30 seconds, not the kernel's 15
        // minutes of retransmits: keepalives on an idle connection, a user timeout on one waiting for an answer
        let url = storage.url.to_ascii_lowercase();
        if !url.contains("keepalives_idle") {
            config.keepalives_idle(Duration::from_secs(30));
        }
        if config.get_keepalives_interval().is_none() {
            config.keepalives_interval(Duration::from_secs(10));
        }
        if config.get_keepalives_retries().is_none() {
            config.keepalives_retries(3);
        }
        if !url.contains("tcp_user_timeout") {
            config.tcp_user_timeout(Duration::from_secs(30));
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
            // a hostaddr is where it actually connects, whatever host says
            for ip in config.get_hostaddrs() {
                if !private_host(&ip.to_string()) {
                    bail!(
                        "storage: {ip} isn't a loopback or private network address; the connection isn't encrypted, set allow_plaintext = true only on a trusted network"
                    );
                }
            }
        }
        Ok(Pool {
            config,
            schema: storage.schema.clone(),
            max: storage.max_connections.max(1),
            state: Mutex::new(PoolState::default()),
            freed: Condvar::new(),
        })
    }

    pub fn schema(&self) -> &str {
        &self.schema
    }

    /// A connection: an idle one, or a new one while fewer than `max` are open (else it waits for one to come
    /// back). A new one's `search_path` is the pool's schema.
    pub fn get(&self) -> Result<Conn<'_>, postgres::Error> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            while let Some(c) = st.idle.pop() {
                if c.is_closed() {
                    st.open -= 1;
                    continue;
                }
                return Ok(Conn { pool: self, client: Some(c), reused: true });
            }
            if st.open < self.max {
                break;
            }
            st = self.freed.wait(st).unwrap_or_else(|e| e.into_inner());
        }
        // counted before connecting, soo others don't open past `max` meanwhile
        st.open += 1;
        drop(st);
        let opened = self.config.connect(postgres::NoTls).and_then(|mut c| {
            c.batch_execute(&format!("set search_path to \"{}\"", self.schema))?;
            Ok(c)
        });
        match opened {
            Ok(c) => Ok(Conn { pool: self, client: Some(c), reused: false }),
            Err(e) => {
                self.forget_one();
                Err(e)
            }
        }
    }

    /// One connection fewer open (it closed, or failed to open), and a waiter woken for its place.
    fn forget_one(&self) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.open -= 1;
        drop(st);
        self.freed.notify_one();
    }

    /// Every idle connection closed: after Postgres went away, those still look open (a connection only notices
    /// when it's used) and would each fail once more.
    fn drop_idle(&self) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let n = st.idle.len();
        st.idle.clear();
        st.open -= n;
        drop(st);
        self.freed.notify_all();
    }

    /// `work` on a connection, tried again while Postgres can't be reached: every `wait` (doubling from one second
    /// to `MAX_WAIT`) until it can, or until `stop` is set (then the last error). An error that isn't about the
    /// connection (bad SQL, a constraint, a row that isn't there) comes back at once. A reused connection that
    /// turns out dead (Postgres restarted since) is tried again at once on a new one before any wait.
    ///
    /// `work` may run more than once, and a try whose commit landed but whose answer was lost (the connection
    /// dropped in between) runs again too: it must give the same result applied twice. One transaction isn't
    /// enough on its own: write with upserts (`on conflict ... do update` / `do nothing`), compare-and-set (a cursor
    /// moved only from the value read), or keys made by the caller, never a plain insert or `value = value + n`. A transaction is
    /// opened with `Client::transaction` only (it rolls back if dropped), never with a raw `begin`, and nothing
    /// may `set` a session setting: the connection goes back to the pool as `work` leaves it.
    pub fn with_retry<T>(
        &self,
        stop: &AtomicBool,
        mut work: impl FnMut(&mut Client) -> Result<T, postgres::Error>,
    ) -> Result<T, postgres::Error> {
        let mut wait = Duration::from_secs(1);
        let mut said = false;
        loop {
            let (mut reused, mut closed) = (false, false);
            let tried = self.get().and_then(|mut c| {
                reused = c.reused;
                let r = work(&mut c);
                closed = r.is_err() && c.is_closed();
                r
            });
            match tried {
                Ok(v) => {
                    if said {
                        crate::ui::success("postgres is back; carrying on");
                    }
                    return Ok(v);
                }
                // a reused connection the server ended while it sat idle (a restart, `idle_session_timeout`): the
                // others idle may be gone too, and a new one is tried at once, whatever the error said
                Err(_) if reused && closed && !stop.load(Ordering::Relaxed) => self.drop_idle(),
                Err(e) if is_unreachable(&e) && !stop.load(Ordering::Relaxed) => {
                    self.drop_idle();
                    if reused {
                        continue;
                    }
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
/// wrong: a closed connection, an I/O or connect error (refused, a name that won't resolve, a timeout), or one of
/// the states a server shutting down or starting up answers with. Not a wrong row count, a type that won't
/// convert, bad parameters or a bad config: those come from the work, and waiting won't fix them.
pub fn is_unreachable(e: &postgres::Error) -> bool {
    use postgres::error::SqlState;
    match e.code() {
        // not PermissionDenied: a url with no user the OS can't name, or a server that isn't the session kind asked
        // for (`target_session_attrs`), both config mistakes
        None => {
            e.is_closed()
                || std::error::Error::source(e)
                    .and_then(|s| s.downcast_ref::<std::io::Error>())
                    .is_some_and(|io| io.kind() != std::io::ErrorKind::PermissionDenied)
        }
        Some(code) => [
            SqlState::ADMIN_SHUTDOWN,
            SqlState::CRASH_SHUTDOWN,
            SqlState::CANNOT_CONNECT_NOW,
            SqlState::CONNECTION_EXCEPTION,
            SqlState::CONNECTION_FAILURE,
            SqlState::CONNECTION_DOES_NOT_EXIST,
            SqlState::SQLCLIENT_UNABLE_TO_ESTABLISH_SQLCONNECTION,
            SqlState::TOO_MANY_CONNECTIONS,
            SqlState::IDLE_SESSION_TIMEOUT,
        ]
        .contains(code),
    }
}

/// A pooled connection, back in the pool when dropped (unless it closed). See `Pool::with_retry` for what it must
/// be left like.
pub struct Conn<'a> {
    pool: &'a Pool,
    client: Option<Client>,
    /// it was idle in the pool before, rather than opened for this
    reused: bool,
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
            self.pool.forget_one();
            return;
        }
        let mut st = self.pool.state.lock().unwrap_or_else(|e| e.into_inner());
        st.idle.push(c);
        drop(st);
        self.pool.freed.notify_one();
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
        assert!(c.check().is_ok());
        let bad = serde_json::json!({"backend": "postgres", "url": "host=10.0.0.2", "schema": "x; drop table groups"});
        assert!(StorageConfig::from_value(Some(&bad)).check().is_err(), "a schema that isn't a plain name is refused");
        let no_url = serde_json::json!({"backend": "postgres"});
        assert!(StorageConfig::from_value(Some(&no_url)).check().is_err(), "postgres with no url is refused");
        let typo = serde_json::json!({"backend": "postgress", "url": "host=10.0.0.2"});
        let c = StorageConfig::from_value(Some(&typo));
        assert!(
            !c.is_postgres() && c.check().is_err(),
            "a backend name that's neither is refused, not taken for sqlite"
        );
        let pg = serde_json::json!({"backend": "postgresql", "url": "host=10.0.0.2"});
        assert!(StorageConfig::from_value(Some(&pg)).is_postgres());
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
        c.url = "host=localhost hostaddr=203.0.113.5 dbname=atlas".into();
        assert!(Pool::new(&c).is_err(), "where hostaddr points is what counts");
    }

    #[test]
    fn schema_names_are_plain() {
        for ok in ["atlas", "t_1", "_x"] {
            assert!(valid_schema(ok), "{ok}");
        }
        for bad in ["", "1a", "Atlas", "a-b", "a.b", "a b", "pg_atlas", &"x".repeat(64)] {
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
        // a row that isn't there is the work's mistake too, not an outage
        let got = t.pool.with_retry(&stop, |c| c.query_one("select 1 where false", &[]).map(|_| ()));
        assert!(got.is_err() && !is_unreachable(got.as_ref().unwrap_err()));
    }

    #[test]
    fn a_dead_idle_connection_is_replaced_without_a_wait() {
        let Some(t) = testing::Schema::new() else { return };
        let stop = AtomicBool::new(false);
        let pid: i32 = t.pool.with_retry(&stop, |c| Ok(c.query_one("select pg_backend_pid()", &[])?.get(0))).unwrap();
        // the server ends the idle connection, as a restart would
        let mut admin = postgres::Client::connect(&t.url, postgres::NoTls).unwrap();
        admin.execute("select pg_terminate_backend($1)", &[&pid]).unwrap();
        let started = std::time::Instant::now();
        let again: i32 = t.pool.with_retry(&stop, |c| Ok(c.query_one("select pg_backend_pid()", &[])?.get(0))).unwrap();
        assert_ne!(again, pid);
        assert!(started.elapsed() < Duration::from_millis(900), "no backoff wait for a connection that was idle");
    }

    #[test]
    fn an_idle_session_timeout_is_retried_on_a_new_connection() {
        let Some(t) = testing::Schema::new() else { return };
        let stop = AtomicBool::new(false);
        // a connection the server ends on its own after 200ms idle (57P05, not one of the outage states)
        t.pool.with_retry(&stop, |c| c.batch_execute("set idle_session_timeout = 200")).unwrap();
        std::thread::sleep(Duration::from_millis(600));
        let got: i32 = t.pool.with_retry(&stop, |c| Ok(c.query_one("select 1", &[])?.get(0))).unwrap();
        assert_eq!(got, 1);
    }

    #[test]
    fn no_more_than_max_connections_are_open() {
        let Some(t) = testing::Schema::new() else { return };
        // the test pool keeps 4 at most
        let held: Vec<_> = (0..4).map(|_| t.pool.get().unwrap()).collect();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::scope(|s| {
            s.spawn(|| {
                let c = t.pool.get().unwrap();
                tx.send(()).unwrap();
                drop(c);
            });
            assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "a fifth waits");
            drop(held);
            assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok(), "and gets one once they're back");
        });
    }
}
