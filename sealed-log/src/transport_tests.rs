//! Real loopback TLS/ACL regressions using PUBLIC synthetic identities only.
use super::*;
use std::{
    ops::{Deref, DerefMut},
    sync::atomic::AtomicU64,
    thread::JoinHandle,
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}
fn identity(name: &str) -> ClientTls {
    ClientTls::from_pem_files(
        &fixture("ca.pem"),
        &fixture(&format!("{name}.pem")),
        &fixture(&format!("{name}-key.pem")),
        "localhost",
    )
    .unwrap()
}
fn security(permissions: Permissions) -> Arc<ServerTls> {
    let certificate = certificates(&fixture("client.pem")).unwrap().remove(0);
    Arc::new(
        ServerTls::from_pem_files(
            &fixture("server.pem"),
            &fixture("server-key.pem"),
            &fixture("ca.pem"),
            &[CertificateGrant { sha256: certificate_sha256(&certificate), permissions }],
            TransportLimits::default(),
        )
        .unwrap(),
    )
}
pub(crate) fn open_log(dir: &Path) -> Arc<Log> {
    let _scope = crate::test_io::Scope::enter(dir, None);
    Log::open_with(dir, 4096).unwrap()
}
struct Fixture {
    log: Option<Arc<Log>>,
    dir: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "sealed-log-public-tls-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let log = open_log(&dir);
        Self { log: Some(log), dir }
    }
    fn log(&self) -> Arc<Log> {
        self.log.as_ref().unwrap().clone()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.log.take();
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn spawn(log: Arc<Log>, tls: Arc<ServerTls>, metrics: bool) -> (String, JoinHandle<io::Result<()>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let thread = std::thread::spawn(move || {
        let (socket, _) = listener.accept()?;
        serve_connection(&log, socket, &tls, metrics)
    });
    (addr, thread)
}
pub(crate) struct Peer {
    client: Option<Client>,
    thread: Option<JoinHandle<io::Result<()>>>,
}
impl Deref for Peer {
    type Target = Client;
    fn deref(&self) -> &Client {
        self.client.as_ref().unwrap()
    }
}
impl DerefMut for Peer {
    fn deref_mut(&mut self) -> &mut Client {
        self.client.as_mut().unwrap()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.client.take();
        if let Some(thread) = self.thread.take() {
            match thread.join() {
                Ok(Ok(())) => {}
                Ok(Err(error))
                    if matches!(
                        error.kind(),
                        ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset | ErrorKind::BrokenPipe
                    ) => {}
                Ok(Err(error)) if !std::thread::panicking() => panic!("unexpected authenticated server exit: {error}"),
                Err(panic) if !std::thread::panicking() => std::panic::resume_unwind(panic),
                _ => {}
            }
        }
    }
}
pub(crate) fn peer(log: Arc<Log>, permissions: Permissions) -> Peer {
    let (addr, thread) = spawn(log, security(permissions), false);
    let client = Client::connect(&addr, &identity("client")).unwrap();
    Peer { client: Some(client), thread: Some(thread) }
}
pub(crate) fn metrics_request(log: Arc<Log>, request: &[u8]) -> String {
    let (addr, thread) = spawn(log, security(Permissions::STATS), true);
    let mut stream = connect(&addr, &identity("client")).unwrap();
    stream.write_all(request).unwrap();
    stream.flush().unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    drop(stream);
    thread.join().unwrap().unwrap();
    reply
}

#[test]
fn trusted_client_roundtrip_preserves_values_empty_records_and_locator_bytes() {
    let f = Fixture::new();
    let mut client = peer(f.log(), Permissions::ALL);
    let places = client.append(&[(7, b"private-value"), (8, b"")]).unwrap();
    assert_eq!(places.len(), 2);
    assert_eq!(places[0].to_ref().len(), 21);
    assert_eq!(client.read(&places).unwrap(), vec![b"private-value".to_vec(), Vec::new()]);
    assert!(client.stats().unwrap().contains("segment"));
    drop(client);
    drop(f);
}

#[test]
fn plaintext_peer_cannot_dispatch_any_legacy_operation() {
    for op in [OP_APPEND, OP_READ, OP_STATS] {
        let f = Fixture::new();
        let before = f.log().tail();
        let (addr, thread) = spawn(f.log(), security(Permissions::ALL), false);
        let mut raw = TcpStream::connect(addr).unwrap();
        raw.write_all(&[op, 0, 0, 0, 0]).unwrap();
        drop(raw);
        assert!(thread.join().unwrap().is_err());
        assert_eq!(f.log().tail(), before);
    }
}

#[test]
fn untrusted_client_and_wrong_server_identity_fail_with_tls_causes() {
    for other in [false, true] {
        let f = Fixture::new();
        let before = f.log().tail();
        let (addr, thread) = spawn(f.log(), security(Permissions::ALL), false);
        let mut tls = identity(if other { "untrusted-client" } else { "client" });
        if !other {
            tls.name = ServerName::try_from("wrong.example.invalid".to_string()).unwrap();
        }
        let result = connect(&addr, &tls).and_then(|mut stream| {
            write_frame(&mut stream, OP_STATS, &[])?;
            read_frame(&mut stream).map(|_| ())
        });
        let error = result.err().expect("untrusted TLS identity must not reach STATS");
        assert!(
            error.get_ref().is_some()
                || matches!(
                    error.kind(),
                    ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset | ErrorKind::BrokenPipe
                ),
            "TLS or socket provenance must survive"
        );
        assert!(thread.join().unwrap().is_err());
        assert_eq!(f.log().tail(), before);
    }
}

#[test]
fn missing_client_certificate_is_rejected_before_application_frames() {
    let f = Fixture::new();
    let (addr, thread) = spawn(f.log(), security(Permissions::ALL), false);
    let config = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots(&fixture("ca.pem")).unwrap())
        .with_no_client_auth();
    let tls = ClientTls { config: Arc::new(config), name: ServerName::try_from("localhost".to_string()).unwrap() };
    assert!(
        connect(&addr, &tls)
            .and_then(|mut s| {
                write_frame(&mut s, OP_STATS, &[])?;
                read_frame(&mut s)
            })
            .is_err()
    );
    assert!(thread.join().unwrap().is_err());
    assert_eq!(f.log().tail().1, 0);
}

#[test]
fn ca_verified_but_ungranted_certificate_cannot_read_append_or_disclose_stats() {
    for op in [OP_READ, OP_APPEND, OP_STATS] {
        let f = Fixture::new();
        let (addr, thread) = spawn(f.log(), security(Permissions::ALL), false);
        let result = connect(&addr, &identity("other-client")).and_then(|mut s| {
            write_frame(&mut s, op, &[])?;
            read_frame(&mut s)
        });
        assert!(result.is_err());
        let error = thread.join().unwrap().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(error.get_ref().unwrap().is::<AccessDenied>());
        assert_eq!(f.log().tail().1, 0);
    }
}

#[test]
fn per_operation_grants_reject_before_advertised_body_ownership() {
    for op in [OP_READ, OP_APPEND, OP_STATS] {
        let f = Fixture::new();
        let allowed = match op {
            OP_READ => Permissions::APPEND,
            _ => Permissions::READ,
        };
        let (addr, thread) = spawn(f.log(), security(allowed), false);
        let mut stream = connect(&addr, &identity("client")).unwrap();
        // The body does not exist. Unauthorized dispatch must answer/refuse
        // immediately, without allocating or waiting for these 4 GiB of bytes.
        stream.write_all(&[op, 255, 255, 255, 255]).unwrap();
        stream.flush().unwrap();
        let (status, reply) = read_frame(&mut stream).unwrap();
        assert_eq!(status, 1);
        assert_eq!(reply, b"operation is not authorized");
        drop(stream);
        let error = thread.join().unwrap().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(f.log().tail().1, 0);
    }
    let f = Fixture::new();
    let place = f.log().append(vec![(1, b"readable".to_vec())]).unwrap();
    let mut reader = peer(f.log(), Permissions::READ);
    assert_eq!(reader.read(&place).unwrap(), vec![b"readable".to_vec()]);
}

#[test]
fn metrics_requires_the_stats_grant_and_preserves_authenticated_scrapes() {
    let f = Fixture::new();
    let good = metrics_request(f.log(), b"GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(good.starts_with("HTTP/1.1 200 OK"));
    assert!(good.contains("sealed_log_bytes "));
    let (addr, thread) = spawn(f.log(), security(Permissions::READ), true);
    let denied = connect(&addr, &identity("client")).and_then(|mut s| {
        s.write_all(b"GET /metrics HTTP/1.1\r\n\r\n")?;
        let mut out = Vec::new();
        s.read_to_end(&mut out).map(|_| out)
    });
    assert!(denied.is_err());
    assert_eq!(thread.join().unwrap().unwrap_err().kind(), ErrorKind::PermissionDenied);
}

#[test]
fn connection_admission_precedes_tls_and_releases_after_denial() {
    let f = Fixture::new();
    let mut tls = security(Permissions::ALL);
    Arc::get_mut(&mut tls).unwrap().limits.connections = 1;
    let held = tls.reserve().unwrap();
    let (addr, thread) = spawn(f.log(), tls.clone(), false);
    assert!(Client::connect(&addr, &identity("client")).is_err());
    assert_eq!(thread.join().unwrap().unwrap_err().kind(), ErrorKind::WouldBlock);
    drop(held);
    let slot = tls.reserve().expect("denied peer cannot leak a connection slot");
    drop(slot);
}

#[test]
fn expired_absolute_io_deadline_is_typed_before_any_socket_read_or_write() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut opposite, _) = listener.accept().unwrap();
    let mut stream = DeadlineIo::new(socket, Duration::from_secs(1));
    stream.until = Instant::now() - Duration::from_millis(1);
    assert_eq!(stream.read(&mut [0]).unwrap_err().kind(), ErrorKind::TimedOut);
    assert_eq!(stream.write(b"not sent").unwrap_err().kind(), ErrorKind::TimedOut);
    opposite.set_nonblocking(true).unwrap();
    assert_eq!(opposite.read(&mut [0]).unwrap_err().kind(), ErrorKind::WouldBlock);
}

#[test]
fn malformed_duplicate_and_empty_grants_do_not_widen_permissions() {
    for value in ["", "READ,READ", "read", "ADMIN", "READ,", "READ,APPEND,UNKNOWN"] {
        assert!(value.parse::<Permissions>().is_err(), "{value}");
    }
    assert!("0:READ".parse::<CertificateGrant>().is_err());
    let cert = fixture("server.pem");
    let key = fixture("server-key.pem");
    let ca = fixture("ca.pem");
    assert!(ServerTls::from_pem_files(&cert, &key, &ca, &[], TransportLimits::default()).is_err());
    let grant = CertificateGrant { sha256: [0; 32], permissions: Permissions::READ };
    assert!(ServerTls::from_pem_files(&cert, &key, &ca, &[grant, grant], TransportLimits::default()).is_err());
}

#[test]
fn unauthenticated_stalled_handshake_has_a_finite_deadline_and_releases_its_slot() {
    let f = Fixture::new();
    let log = f.log();
    let mut tls = security(Permissions::ALL);
    Arc::get_mut(&mut tls).unwrap().limits.handshake = Duration::from_millis(20);
    let inspected = tls.clone();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (sent, result) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        let outcome = serve_connection(&log, socket, &tls, false);
        let _ = sent.send(outcome);
    });
    let stalled = TcpStream::connect(addr).unwrap();
    let observed = result.recv_timeout(Duration::from_secs(2));
    drop(stalled);
    thread.join().unwrap();
    let error = observed.expect("stalled handshake exceeded its finite fixture bound").unwrap_err();
    assert!(matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock), "{error}");
    assert_eq!(inspected.active.load(Ordering::Acquire), 0);
    assert_eq!(f.log().tail().1, 0);
}
