//! Mandatory mutually authenticated TLS for data and metrics. Authorization
//! precedes protocol-body ownership; a certificate is not a write grant.
use super::*;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject};
use rustls::{ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection, StreamOwned};
use sha2::{Digest, Sha256};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Operations granted to one exact CA-verified leaf certificate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Permissions(u8);
impl Permissions {
    pub const READ: Self = Self(1);
    pub const APPEND: Self = Self(2);
    pub const STATS: Self = Self(4);
    pub const ALL: Self = Self(7);
    fn require(self, op: u8) -> io::Result<()> {
        let wanted = match op {
            OP_READ => 1,
            OP_APPEND => 2,
            OP_STATS => 4,
            _ => return Err(invalid("unknown sealed-log operation")),
        };
        if self.0 & wanted == 0 {
            return Err(Error::new(ErrorKind::PermissionDenied, AccessDenied));
        }
        Ok(())
    }
}
impl std::str::FromStr for Permissions {
    type Err = io::Error;
    fn from_str(value: &str) -> io::Result<Self> {
        let mut bits = 0;
        for item in value.split(',') {
            let bit = match item {
                "READ" => 1,
                "APPEND" => 2,
                "STATS" => 4,
                _ => return Err(invalid("grant must list READ,APPEND,STATS operations")),
            };
            if bits & bit != 0 {
                return Err(invalid("duplicate grant operation"));
            }
            bits |= bit;
        }
        Ok(Self(bits))
    }
}

#[derive(Debug)]
pub(crate) struct AccessDenied;
impl std::fmt::Display for AccessDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("sealed-log certificate is not authorized for this operation")
    }
}
impl std::error::Error for AccessDenied {}

#[derive(Clone, Copy, Debug)]
pub struct CertificateGrant {
    pub sha256: [u8; 32],
    pub permissions: Permissions,
}
impl std::str::FromStr for CertificateGrant {
    type Err = io::Error;
    fn from_str(value: &str) -> io::Result<Self> {
        let (fingerprint, permissions) =
            value.split_once(':').ok_or_else(|| invalid("grant requires SHA256:OPERATIONS"))?;
        if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid("certificate fingerprint must contain 64 hex digits"));
        }
        let mut sha256 = [0; 32];
        for (at, byte) in sha256.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&fingerprint[at * 2..at * 2 + 2], 16)
                .map_err(|_| invalid("invalid certificate fingerprint"))?;
        }
        Ok(Self { sha256, permissions: permissions.parse()? })
    }
}

/// Admission applies before spawning or reading an unauthenticated peer.
#[derive(Clone, Copy, Debug)]
pub struct TransportLimits {
    pub connections: usize,
    pub handshake: Duration,
    pub io: Duration,
}
impl Default for TransportLimits {
    fn default() -> Self {
        Self { connections: 8, handshake: Duration::from_secs(5), io: IO_TIMEOUT }
    }
}
impl TransportLimits {
    fn check(self) -> io::Result<Self> {
        if !(1..=64).contains(&self.connections)
            || self.handshake.is_zero()
            || self.handshake > Duration::from_secs(30)
            || self.io.is_zero()
            || self.io > IO_TIMEOUT
        {
            return Err(invalid("transport admission exceeds limits"));
        }
        Ok(self)
    }
}

pub struct ServerTls {
    config: Arc<ServerConfig>,
    grants: HashMap<[u8; 32], Permissions>,
    limits: TransportLimits,
    active: Arc<AtomicUsize>,
}
pub struct ClientTls {
    config: Arc<ClientConfig>,
    name: ServerName<'static>,
}

fn pem(path: &Path) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(invalid("TLS material exceeds admission"));
    }
    Ok(bytes)
}
fn certificates(path: &Path) -> io::Result<Vec<CertificateDer<'static>>> {
    let bytes = pem(path)?;
    let certs = CertificateDer::pem_slice_iter(&bytes).collect::<Result<Vec<_>, _>>().map_err(Error::other)?;
    if certs.is_empty() {
        return Err(invalid("TLS certificate file is empty"));
    }
    Ok(certs)
}
fn key(path: &Path) -> io::Result<PrivateKeyDer<'static>> {
    PrivateKeyDer::from_pem_slice(&pem(path)?).map_err(Error::other)
}
fn roots(path: &Path) -> io::Result<RootCertStore> {
    let mut roots = RootCertStore::empty();
    for certificate in certificates(path)? {
        roots.add(certificate).map_err(Error::other)?;
    }
    Ok(roots)
}
pub fn certificate_sha256(certificate: &CertificateDer<'_>) -> [u8; 32] {
    Sha256::digest(certificate.as_ref()).into()
}

impl ServerTls {
    pub fn from_pem_files(
        cert: &Path,
        private_key: &Path,
        client_ca: &Path,
        grants: &[CertificateGrant],
        limits: TransportLimits,
    ) -> io::Result<Self> {
        let limits = limits.check()?;
        let mut permissions = HashMap::new();
        if grants.is_empty() {
            return Err(invalid("at least one explicit client grant is required"));
        }
        for grant in grants {
            if permissions.insert(grant.sha256, grant.permissions).is_some() {
                return Err(invalid("duplicate client certificate grant"));
            }
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier =
            rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots(client_ca)?), provider.clone())
                .build()
                .map_err(Error::other)?;
        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(Error::other)?
            .with_client_cert_verifier(verifier)
            .with_single_cert(certificates(cert)?, key(private_key)?)
            .map_err(Error::other)?;
        Ok(Self { config: Arc::new(config), grants: permissions, limits, active: Arc::new(AtomicUsize::new(0)) })
    }
    fn reserve(&self) -> io::Result<Slot> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                if n < self.limits.connections { n.checked_add(1) } else { None }
            })
            .map_err(|_| Error::new(ErrorKind::WouldBlock, "sealed-log connection admission is full"))?;
        Ok(Slot(self.active.clone()))
    }
}
impl ClientTls {
    pub fn from_pem_files(server_ca: &Path, cert: &Path, private_key: &Path, server_name: &str) -> io::Result<Self> {
        let config = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(Error::other)?
            .with_root_certificates(roots(server_ca)?)
            .with_client_auth_cert(certificates(cert)?, key(private_key)?)
            .map_err(Error::other)?;
        let name = ServerName::try_from(server_name.to_owned()).map_err(Error::other)?;
        Ok(Self { config: Arc::new(config), name })
    }
}
struct Slot(Arc<AtomicUsize>);
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn remaining(until: Instant) -> io::Result<Duration> {
    until
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
        .ok_or_else(|| Error::new(ErrorKind::TimedOut, "sealed-log I/O deadline expired"))
}
/// Every underlying syscall shares one absolute deadline, including trickled
/// TLS records. Per-syscall timeout renewal cannot extend a handshake/request.
pub(crate) struct DeadlineIo {
    socket: TcpStream,
    until: Instant,
}
impl DeadlineIo {
    fn new(socket: TcpStream, duration: Duration) -> Self {
        Self { socket, until: Instant::now() + duration }
    }
    pub(crate) fn check(&self) -> io::Result<()> {
        remaining(self.until).map(|_| ())
    }
    pub(crate) fn reset(&mut self, duration: Duration) {
        self.until = Instant::now() + duration;
    }
}
impl Read for DeadlineIo {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.socket.set_read_timeout(Some(remaining(self.until)?))?;
        self.socket.read(bytes)
    }
}
impl Write for DeadlineIo {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.socket.set_write_timeout(Some(remaining(self.until)?))?;
        self.socket.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.socket.set_write_timeout(Some(remaining(self.until)?))?;
        self.socket.flush()
    }
}
pub(crate) type ClientStream = StreamOwned<ClientConnection, DeadlineIo>;
pub(crate) type ServerStream = StreamOwned<ServerConnection, DeadlineIo>;

pub(crate) fn connect(addr: &str, tls: &ClientTls) -> io::Result<ClientStream> {
    let addr = addr.to_socket_addrs()?.next().ok_or_else(|| invalid("no address"))?;
    let socket = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    socket.set_nodelay(true)?;
    let mut stream = StreamOwned::new(
        ClientConnection::new(tls.config.clone(), tls.name.clone()).map_err(Error::other)?,
        DeadlineIo::new(socket, Duration::from_secs(5)),
    );
    while stream.conn.is_handshaking() {
        stream.conn.complete_io(&mut stream.sock)?;
    }
    stream.sock.reset(IO_TIMEOUT);
    Ok(stream)
}

fn accept(socket: TcpStream, tls: &ServerTls) -> io::Result<(ServerStream, Permissions)> {
    socket.set_nodelay(true)?;
    let mut stream = StreamOwned::new(
        ServerConnection::new(tls.config.clone()).map_err(Error::other)?,
        DeadlineIo::new(socket, tls.limits.handshake),
    );
    while stream.conn.is_handshaking() {
        stream.conn.complete_io(&mut stream.sock)?;
    }
    let leaf = stream
        .conn
        .peer_certificates()
        .and_then(|chain| chain.first())
        .ok_or_else(|| Error::new(ErrorKind::PermissionDenied, AccessDenied))?;
    let permissions = *tls
        .grants
        .get(&certificate_sha256(leaf))
        .ok_or_else(|| Error::new(ErrorKind::PermissionDenied, AccessDenied))?;
    Ok((stream, permissions))
}

fn data(log: &Log, stream: &mut ServerStream, permissions: Permissions, timeout: Duration) -> io::Result<()> {
    loop {
        stream.sock.reset(timeout);
        // An unauthorized operation cannot make us allocate its advertised
        // frame, parse locators, append a value or disclose even STATS.
        let mut header = [0; 5];
        stream.read_exact(&mut header)?;
        if let Err(error) = permissions.require(header[0]) {
            write_frame(stream, 1, b"operation is not authorized")?;
            return Err(error);
        }
        let length = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
        if length > MAX_FRAME {
            return Err(invalid("frame too big"));
        }
        stream.sock.check()?;
        let mut body = vec![0; length];
        stream.read_exact(&mut body)?;
        stream.sock.check()?;
        STATS.network_received.fetch_add(5 + length as u64, Relaxed);
        let sent = match handle(log, header[0], &body) {
            Ok(reply) => {
                write_frame(stream, 0, &reply)?;
                reply.len()
            }
            Err(error) => {
                let text = error.to_string();
                write_frame(stream, 1, text.as_bytes())?;
                text.len()
            }
        };
        STATS.network_sent.fetch_add(5 + sent as u64, Relaxed);
    }
}
#[cfg(test)]
pub(crate) fn serve_connection(log: &Log, socket: TcpStream, tls: &ServerTls, metrics: bool) -> io::Result<()> {
    let slot = tls.reserve()?;
    serve_admitted(log, socket, tls, metrics, slot)
}
fn serve_admitted(log: &Log, socket: TcpStream, tls: &ServerTls, metrics: bool, _slot: Slot) -> io::Result<()> {
    let _count = (!metrics).then(Open::new);
    let (mut stream, permissions) = accept(socket, tls)?;
    if metrics {
        permissions.require(OP_STATS)?;
        stream.sock.reset(tls.limits.io.min(Duration::from_secs(10)));
        super::metrics::answer(log, &mut stream)?;
        stream.conn.send_close_notify();
        stream.flush()
    } else {
        data(log, &mut stream, permissions, tls.limits.io)
    }
}
pub(crate) fn serve(log: Arc<Log>, listener: TcpListener, tls: Arc<ServerTls>, metrics: bool) -> io::Result<()> {
    for incoming in listener.incoming() {
        let socket = incoming?;
        let slot = match tls.reserve() {
            Ok(slot) => slot,
            Err(error) if error.kind() == ErrorKind::WouldBlock => continue,
            Err(error) => return Err(error),
        };
        let (log, tls) = (log.clone(), tls.clone());
        std::thread::Builder::new().name("sealed-log-tls".into()).spawn(move || {
            let _ = serve_admitted(&log, socket, &tls, metrics, slot);
        })?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "transport_tests.rs"]
pub(crate) mod tests;
