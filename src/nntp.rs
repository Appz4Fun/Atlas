//! Async NNTP client (tokio): just what atlas needs (GROUP, XOVER, BODY, LIST),
//! with a connection pool per usenet server soo many requests are in flight at once.

use std::fmt;
use std::future::Future;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
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
            bytes: self.bytes,
            lines: self.lines,
            ..Default::default()
        }
    }
}

pub fn headers_to_articles(headers: Vec<Overview>) -> Vec<Article> {
    headers.into_iter().map(Overview::into_article).collect()
}

trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// smallest XOVER slice handed to one connection
const MIN_CHUNK: u64 = 250;
/// a server whose login was rejected is left alone this long
const AUTH_REST: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    /// provider wont take another connection
    TooMany,
    /// wrong username / password
    Auth,
    Other,
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

/// how long to wait for the end of a compressed stream once its last line arrived
const STREAM_END_WAIT: Duration = Duration::from_secs(2);
/// after a failed connect a server is skipped for this long (connect() still tries it)
const DOWN_FOR: Duration = Duration::from_secs(60);

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

/// One logged in connection to one server.
pub struct Conn {
    io: BufReader<Box<dyn AsyncStream>>,
    timeout: Duration,
    /// group selected on this connection, XOVER needs one
    pub group: Option<String>,
    /// server agreed to XFEATURE COMPRESS GZIP on this connection
    pub compressed: bool,
}

impl Conn {
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
        }

        Ok(conn)
    }

    fn over(stream: Box<dyn AsyncStream>, timeout: Duration) -> Conn {
        Conn { io: BufReader::with_capacity(64 * 1024, stream), timeout, group: None, compressed: false }
    }

    /// Next chunk of raw bytes from the socket.
    async fn read_chunk(&mut self, limit: Duration) -> Result<Vec<u8>> {
        let io = &mut self.io;
        let chunk = with_timeout(limit, async move { Ok(io.fill_buf().await?.to_vec()) }).await?;

        if chunk.is_empty() {
            return Err(NntpError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed")));
        }

        self.io.consume(chunk.len());
        Ok(chunk)
    }

    /// Body of a `[COMPRESS=GZIP]` multi-line response: a zlib (or gzip)
    /// stream holding the usual dot terminated, dot stuffed lines.
    async fn read_compressed_multiline(&mut self) -> Result<Vec<Vec<u8>>> {
        let mut inflater = Inflater::default();
        let mut lines = Vec::new();
        let mut text = Vec::new();
        let mut scanned = 0;
        let mut terminated = false;

        loop {
            if !terminated {
                while let Some(nl) = text[scanned..].iter().position(|b| *b == b'\n') {
                    let mut line = text[scanned..scanned + nl].to_vec();
                    scanned += nl + 1;

                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }

                    if line == b"." {
                        terminated = true;
                        break;
                    }

                    if line.starts_with(b"..") {
                        line.remove(0);
                    }

                    lines.push(line);
                }
            }

            // done once the terminator showed up and the compressed stream is fully read,
            // soo no stray trailer bytes are left for the next command to trip over
            if terminated && inflater.finished() {
                return Ok(lines);
            }

            let chunk = if terminated {
                // all lines are in, only the end of the compressed stream is missing.
                // a server that never closes the stream properly sends nothing more
                match self.read_chunk(STREAM_END_WAIT).await {
                    Err(NntpError::Io(e)) if e.kind() == io::ErrorKind::TimedOut => return Ok(lines),
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

    /// Lines of a multi-line response with dot-stuffing removed.
    async fn read_multiline(&mut self) -> Result<Vec<Vec<u8>>> {
        let mut lines = Vec::new();

        loop {
            let mut line = self.read_raw_line().await?;

            if line == b"." {
                return Ok(lines);
            }

            if line.starts_with(b"..") {
                line.remove(0);
            }

            lines.push(line);
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
        let (code, message) = self.command("XOVER", Some(&format!("{start}-{end}"))).await?;

        if code != 224 {
            return Err(NntpError::Reply { code, message });
        }

        let lines = self.read_response_body(&message).await?;

        Ok(lines.iter().filter_map(|l| parse_overview(l)).collect())
    }

    /// BODY, yEnc decoded when the article is yEnc.
    pub async fn body(&mut self, message_id: &str) -> Result<Vec<u8>> {
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
        if status_message.to_ascii_uppercase().contains("COMPRESS=GZIP") {
            self.read_compressed_multiline().await
        } else {
            self.read_multiline().await
        }
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

            if head.len() < 2 {
                self.head = head;
                return Ok(Vec::new());
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
            } else if head[0] & 0x0f == 8 && (u16::from(head[0]) << 8 | u16::from(head[1])) % 31 == 0 {
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
    idle: Mutex<Vec<Conn>>,
    permits: Arc<Semaphore>,
    down_until: Mutex<Option<Instant>>,
    /// set when the server's compressed headers couldnt be read
    no_compress: AtomicBool,
    /// server answered GROUP with 500/501: an article only server (fill / bonus),
    /// used for article lookups but not for indexing
    no_index: AtomicBool,
    /// login rejected or limit shrunk messages already printed
    warned_auth: AtomicBool,
    warned_limit: AtomicBool,
    /// connections atlas currently allows itself (starts at `connections`,
    /// shrinks when the provider refuses more)
    limit: AtomicUsize,
}

impl Server {
    fn compress(&self) -> bool {
        self.cfg.compress.unwrap_or(true) && !self.no_compress.load(Ordering::Relaxed)
    }
}

/// A connection borrowed from a server. Goes back to the idle list on drop
/// unless an error left it unusable.
struct Lease {
    conn: Option<Conn>,
    server: Arc<Server>,
    _permit: OwnedSemaphorePermit,
}

impl Lease {
    fn conn(&mut self) -> &mut Conn {
        self.conn.as_mut().expect("lease without a connection")
    }

    /// drop the connection when `r` broke it
    fn check<T>(&mut self, r: Result<T>) -> Result<T> {
        if r.as_ref().err().is_some_and(NntpError::breaks_connection) {
            self.conn = None;
        }
        r
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.server.idle.lock().unwrap().push(conn);
        }
    }
}

/// Several providers tried in priority order, each with up to `connections`
/// requests in flight.
///
/// GROUP/XOVER/LIST run on one "active" server (article numbers are per
/// server). `connect` picks the first server that works, and a group missing
/// on the active server is looked up on the others. BODY is fetched by
/// message-id, which is the same everywhere, soo a missing article is asked
/// of every server in priority order.
pub struct Pool {
    servers: Vec<Arc<Server>>,
    active: AtomicUsize,
    ready: AtomicBool,
    failed_over_at: Mutex<Option<Instant>>,
    timeout: Duration,
}

impl Pool {
    pub fn new(servers: &[UsenetServer]) -> Pool {
        Pool {
            servers: servers
                .iter()
                .map(|cfg| {
                    Arc::new(Server {
                        cfg: cfg.clone(),
                        idle: Mutex::new(Vec::new()),
                        permits: Arc::new(Semaphore::new(cfg.connections().max(1) as usize)),
                        down_until: Mutex::new(None),
                        no_compress: AtomicBool::new(false),
                        no_index: AtomicBool::new(false),
                        warned_auth: AtomicBool::new(false),
                        warned_limit: AtomicBool::new(false),
                        limit: AtomicUsize::new(cfg.connections().max(1) as usize),
                    })
                })
                .collect(),
            active: AtomicUsize::new(0),
            ready: AtomicBool::new(false),
            failed_over_at: Mutex::new(None),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
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

    pub fn host(&self, i: usize) -> String {
        self.servers.get(i).map(|s| s.cfg.host.clone()).unwrap_or_default()
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
            let permit = server.permits.clone().acquire_owned().await.expect("semaphore closed");

            let idle = {
                let mut idle = server.idle.lock().unwrap();
                let on_group = group.and_then(|g| idle.iter().rposition(|c| c.group.as_deref() == Some(g)));
                match on_group {
                    Some(pos) => Some(idle.swap_remove(pos)),
                    None => idle.pop(),
                }
            };
            if let Some(conn) = idle {
                return Ok(Lease { conn: Some(conn), server, _permit: permit });
            }

            if !force && server.down_until.lock().unwrap().is_some_and(|t| Instant::now() < t) {
                return Err(NntpError::Io(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    format!("{} failed recently, skipping it", server.cfg.host),
                )));
            }

            let e = match Conn::open(&server.cfg, self.timeout, server.compress()).await {
                Ok(conn) => {
                    *server.down_until.lock().unwrap() = None;
                    return Ok(Lease { conn: Some(conn), server, _permit: permit });
                }
                Err(e) => e,
            };

            let limit = server.limit.load(Ordering::Relaxed);
            let others_open = server.permits.available_permits() + 1 < limit;

            match refusal(&e) {
                // the provider allows fewer connections than configured (or another
                // app on the same account uses some): use one less from now on and
                // wait for a connection that is already open
                Refusal::TooMany if others_open && limit > 1 => {
                    permit.forget();
                    let now = server.limit.fetch_sub(1, Ordering::Relaxed) - 1;
                    if !server.warned_limit.swap(true, Ordering::Relaxed) {
                        println!("{}: provider refused more connections ({e}), lowering to {now}", server.cfg.host);
                    }
                    continue;
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
                    return Err(e);
                }
                _ => {
                    if !others_open {
                        *server.down_until.lock().unwrap() = Some(Instant::now() + DOWN_FOR);
                    }
                    return Err(e);
                }
            }
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
        let r = lease.conn().select_group(group).await;

        // GROUP not supported at all: an article only (fill / bonus) server
        if let Err(e) = &r
            && matches!(e.code(), Some(500 | 501))
        {
            let server = &self.servers[i];
            if !server.no_index.swap(true, Ordering::Relaxed) {
                println!("{}: doesnt support GROUP ({e}), using it for article lookups only", server.cfg.host);
            }
        }

        lease.check(r)
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

    /// GROUP on server `prefer`, falling over (in priority order) to a server
    /// that carries it. Returns the server used.
    pub async fn select_group_on(&self, prefer: usize, group: &str) -> Result<(usize, (u64, u64, u64, String))> {
        let first_err = match self.select_on(prefer, group).await {
            Ok(r) => return Ok((prefer, r)),
            // 411 = this provider doesnt carry the group, 500/501 = no GROUP at all
            Err(e) if matches!(e.code(), Some(411 | 500 | 501)) => e,
            Err(e) => return Err(e),
        };

        let candidates: Vec<usize> = self
            .indexing_servers()
            .into_iter()
            .chain(0..self.servers.len())
            .filter(|&i| i != prefer && !self.servers[i].no_index.load(Ordering::Relaxed))
            .fold(Vec::new(), |mut v, i| {
                if !v.contains(&i) {
                    v.push(i);
                }
                v
            });

        for i in candidates {
            if let Ok(r) = self.select_on(i, group).await {
                println!("{group} not on {}, using {}", self.servers[prefer].cfg.host, self.servers[i].cfg.host);
                return Ok((i, r));
            }
        }

        Err(first_err)
    }

    async fn xover_on(&self, i: usize, group: &str, start: u64, end: u64) -> Result<Vec<Overview>> {
        match self.xover_once(i, group, start, end).await {
            Err(NntpError::Decompress(e)) => {
                // turn compression off for this server and redo the slice uncompressed
                let server = &self.servers[i];
                if !server.no_compress.swap(true, Ordering::Relaxed) {
                    println!("{}: couldnt read compressed headers ({e}), turning compression off", server.cfg.host);
                }
                server.idle.lock().unwrap().retain(|c| !c.compressed);
                self.xover_once(i, group, start, end).await
            }
            r => r,
        }
    }

    async fn xover_once(&self, i: usize, group: &str, start: u64, end: u64) -> Result<Vec<Overview>> {
        let mut lease = self.lease_for(i, Some(group), false).await?;

        if lease.conn().group.as_deref() != Some(group) {
            let r = lease.conn().select_group(group).await;
            lease.check(r)?;
        }

        let r = lease.conn().xover(start, end).await;
        lease.check(r)
    }

    /// Fetch every `(start, end)` slice of `group` on `server` with
    /// up to `connections` requests in flight, sending each slice to the
    /// receiver the moment it arrives. A connection picks up the next slice as
    /// soon as it is free. No new slices are started once `stop` is set or a
    /// slice fails with anything but 423 (empty) / 5xx (not available).
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
        let workers = self.connections(i).max(1).min(slices.len().max(1));
        let (tx, rx) = tokio::sync::mpsc::channel(workers * 2);
        let queue = Arc::new(Mutex::new(std::collections::VecDeque::from(slices)));
        let halt = Arc::new(AtomicBool::new(false));

        for _ in 0..workers {
            let (pool, queue, halt, stop, tx, group) =
                (self.clone(), queue.clone(), halt.clone(), stop.clone(), tx.clone(), group.to_string());

            tokio::spawn(async move {
                loop {
                    if stop.load(Ordering::Relaxed) || halt.load(Ordering::Relaxed) {
                        return;
                    }

                    let Some((start, end)) = queue.lock().unwrap().pop_front() else { return };
                    let result = pool.xover_on(i, &group, start, end).await;

                    if let Err(e) = &result
                        && e.code() != Some(423)
                        && !e.is_permanent()
                    {
                        halt.store(true, Ordering::Relaxed);
                    }

                    if tx.send(HeaderSlice { start, end, result }).await.is_err() {
                        return;
                    }
                }
            });
        }

        rx
    }

    /// XOVER `start..=end` of `group` on the active server, split into slices
    /// fetched in parallel over its connections. Slices with no articles
    /// (423) come back empty; any other failure fails the whole range.
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
                Err(e) if e.code() == Some(423) => {}
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

    /// BODY from the active server, then every other server in priority order.
    pub async fn fetch_body(&self, message_id: &str) -> Result<Vec<u8>> {
        let active = self.active_index();
        let order = std::iter::once(active).chain((0..self.servers.len()).filter(|&i| i != active));
        let mut last_err = Self::no_servers();

        for i in order {
            let mut lease = match self.lease(i, false).await {
                Ok(l) => l,
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };

            let r = lease.conn().body(message_id).await;
            match lease.check(r) {
                Ok(body) => return Ok(body),
                Err(e) => last_err = e,
            }
        }

        Err(last_err)
    }

    pub async fn list_groups(&self, pattern: Option<&str>) -> Result<Vec<(String, u64)>> {
        let mut lease = self.lease(self.active_index(), false).await?;
        let r = lease.conn().list_groups(pattern).await;
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
        Self::new(&cfg.servers)
    }

    pub fn from_pool(pool: Pool) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
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
    let fields: Vec<String> = line.split(|b| *b == b'\t').map(|f| String::from_utf8_lossy(f).into_owned()).collect();
    let field = |i: usize| fields.get(i).cloned().unwrap_or_default();
    let int = |i: usize| fields.get(i).and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0);

    Some(Overview {
        number: fields.first()?.trim().parse().ok()?,
        subject: field(1),
        from: field(2),
        date: field(3),
        message_id: field(4),
        references: field(5),
        bytes: int(6),
        lines: int(7),
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

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
            let lines = conn.read_compressed_multiline().await.unwrap();
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

    #[test]
    fn garbage_is_a_decompress_error() {
        run(async {
            let (client, mut server) = tokio::io::duplex(1 << 16);
            let mut conn = Conn::over(Box::new(client), Duration::from_secs(2));
            server.write_all(&[0x78, 0x9c, 0xff, 0xff, 0xff, 0xff, 0x00, 0x01]).await.unwrap();
            let err = conn.read_compressed_multiline().await.unwrap_err();
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
}
