//! Failure counter baselines belong to pool configuration, before provider work.
use std::sync::{Arc, mpsc};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::config::UsenetServer;
use crate::metrics;
use crate::nntp::Pool;

// Public failure-kind contract, independent of the initializer under test.
const KINDS: [&str; 14] = [
    "timeout", "connection_closed", "io", "protocol", "header_store", "decompress",
    "no_such_article", "no_such_group", "too_many_connections", "auth_rejected",
    "reply_4xx", "reply_5xx", "reply_other", "skipped_while_resting",
];

fn failure_value(text: &str, server: &str, kind: &str) -> Option<u64> {
    let series = format!("atlas_nntp_failures_total{{server=\"{server}\",kind=\"{kind}\"}} ");
    text.lines().find_map(|line| line.strip_prefix(&series)?.parse().ok())
}

fn failure_rows(text: &str, server: &str) -> usize {
    let prefix = format!("atlas_nntp_failures_total{{server=\"{server}\",");
    text.lines().filter(|line| line.starts_with(&prefix)).count()
}

fn assert_zero_kinds(text: &str, server: &str) {
    for kind in KINDS {
        assert_eq!(failure_value(text, server, kind), Some(0), "missing initial zero for {server}/{kind}");
    }
    assert_eq!(failure_rows(text, server), 14, "one bounded series per failure kind");
}

/// One bounded owned connection, rejected at login; never an upstream provider.
struct AuthReject {
    port: u16,
    entered: mpsc::Receiver<()>,
    release: Option<tokio::sync::oneshot::Sender<()>>,
    worker: Option<tokio::task::JoinHandle<()>>,
}

impl AuthReject {
    fn new(runtime: &tokio::runtime::Runtime, hold_greeting: bool) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (entered, ready) = mpsc::channel();
        let (release, proceed) = tokio::sync::oneshot::channel();
        let worker = runtime.spawn(async move {
            tokio::time::timeout(Duration::from_secs(5), async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let (stream, _) = listener.accept().await.unwrap();
                entered.send(()).unwrap();
                if hold_greeting {
                    proceed.await.unwrap();
                }
                let mut stream = BufReader::new(stream);
                stream.get_mut().write_all(b"200 owned fixture ready\r\n").await.unwrap();
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                assert!(line.starts_with("AUTHINFO USER "), "unexpected fixture request");
                stream.get_mut().write_all(b"381 more\r\n").await.unwrap();
                line.clear();
                stream.read_line(&mut line).await.unwrap();
                assert!(line.starts_with("AUTHINFO PASS "), "unexpected fixture request");
                stream.get_mut().write_all(b"481 Authentication failed\r\n").await.unwrap();
            })
            .await
            .expect("owned authentication fixture exceeded its deadline");
        });
        Self { port, entered: ready, release: Some(release), worker: Some(worker) }
    }

    fn config(&self, key: &str) -> UsenetServer {
        let mut server = UsenetServer::new("127.0.0.1", "fixture-user", "fixture-password", self.port);
        server.ssl = Some(false);
        server.connections = Some(1);
        server.key = Some(key.into());
        server
    }

    fn release(&mut self) {
        self.release.take().unwrap().send(()).unwrap();
    }

    fn finish(&mut self, runtime: &tokio::runtime::Runtime) {
        runtime.block_on(self.worker.take().unwrap()).unwrap();
    }
}

impl Drop for AuthReject {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }
}

#[test]
fn failure_series_cover_the_first_pending_connect_and_zero_to_one_transition() {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let mut provider = AuthReject::new(&runtime, true);
    let pool = Arc::new(Pool::new(&[provider.config("pr25-first-attempt")]));
    let key = format!("127.0.0.1:{}#pr25-first-attempt", provider.port);
    assert_zero_kinds(&metrics::render(), &key);

    let connecting = pool.clone();
    let attempt = runtime.spawn(async move { connecting.connect().await });
    provider.entered.recv_timeout(Duration::from_secs(3)).unwrap();
    // No successful pool, scheduler or report_servers call supplies this zero.
    assert_zero_kinds(&metrics::render(), &key);
    provider.release();
    assert!(runtime.block_on(attempt).unwrap().is_err());
    provider.finish(&runtime);

    let text = metrics::render();
    assert_eq!(failure_value(&text, &key, "auth_rejected"), Some(1));
    for kind in KINDS.into_iter().filter(|kind| *kind != "auth_rejected") {
        assert_eq!(failure_value(&text, &key, kind), Some(0));
    }
    assert_eq!(failure_rows(&text, &key), 14);
}

#[test]
fn rebuilding_a_failed_pool_preserves_totals_and_initializes_a_new_server() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut provider = AuthReject::new(&runtime, false);
    let first = provider.config("pr25-rebuild");
    let key = format!("127.0.0.1:{}#pr25-rebuild", provider.port);
    let pool = Pool::new(std::slice::from_ref(&first));
    assert!(runtime.block_on(pool.connect()).is_err());
    provider.finish(&runtime);
    assert_eq!(failure_value(&metrics::render(), &key, "auth_rejected"), Some(1));
    drop(pool);

    let mut rotated = first.clone();
    rotated.username = "pr25-rotated-user".into();
    rotated.password = "pr25-rotated-password".into();
    let mut new_server = UsenetServer::new("new.pr25.example.invalid", "pr25-new-user", "pr25-new-password", 119);
    new_server.ssl = Some(false);
    let _rebuilt = Pool::new(&[first, rotated, new_server]);
    let text = metrics::render();
    assert_eq!(failure_value(&text, &key, "auth_rejected"), Some(1), "rebuild reset accumulated failures");
    assert_eq!(failure_rows(&text, &key), 14, "accounts must share the existing bounded key");
    assert_zero_kinds(&text, "new.pr25.example.invalid:119");
    for credential in ["pr25-rotated-user", "pr25-rotated-password", "pr25-new-user", "pr25-new-password"] {
        assert!(!text.contains(credential), "scrape exposed fixture credentials");
    }
}

#[test]
fn configured_failure_labels_are_bounded_and_do_not_include_account_credentials() {
    let mut first = UsenetServer::new("accounts.pr25.example.invalid", "pr25-first-user", "pr25-first-password", 119);
    first.ssl = Some(false);
    let mut second = first.clone();
    second.username = "pr25-second-user".into();
    second.password = "pr25-second-password".into();
    let _pool = Pool::new(&[first, second]);
    let text = metrics::render();
    assert_zero_kinds(&text, "accounts.pr25.example.invalid:119");
    for credential in ["pr25-first-user", "pr25-first-password", "pr25-second-user", "pr25-second-password"] {
        assert!(!text.contains(credential), "scrape exposed fixture credentials");
    }
}
