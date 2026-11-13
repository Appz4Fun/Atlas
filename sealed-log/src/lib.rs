//! An append-only store for values that never change. Values go into numbered
//! segment files one after another, so a spinning disk only ever writes in
//! order, and each append answers with where its value landed. The caller keeps
//! that place, so there is no index here to grow or to seek through.
//!
//! A record on disk: magic, crc32 of the rest, u64 key, u32 length, the value.
//! An append is answered only after an fdatasync, and appends that arrive
//! together share one (group commit).

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Error, ErrorKind, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::time::{Duration, Instant};

pub mod metrics;
mod transport;
pub use transport::{CertificateGrant, ClientTls, Permissions, ServerTls, TransportLimits};
#[cfg(test)]
#[path = "integrity_tests.rs"]
mod integrity_regressions;
#[cfg(test)]
use integrity_regressions::test_io;
use metrics::STATS;

const MAGIC: [u8; 4] = *b"SLG1";
const HEADER: usize = 4 + 4 + 8 + 4;
/// a segment file grows to this, then the next one starts
pub const SEGMENT_BYTES: u64 = 8 << 30;
/// space is reserved in steps this big, so a segment stays in one run on disk
/// and an fdatasync has no file size to write
const ALLOCATE_BYTES: u64 = 1 << 30;
/// the most one request may carry, and one value may be
const MAX_FRAME: usize = 512 << 20;
/// appends written together before an fdatasync
const MAX_GROUP_BYTES: usize = 64 << 20;
const OP_APPEND: u8 = 1;
const OP_READ: u8 = 2;
const OP_STATS: u8 = 3;
const IO_TIMEOUT: Duration = Duration::from_secs(120);

fn invalid(what: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidData, what.into())
}

/// Where a value is. As bytes it starts with a tag no zstd frame starts with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Locator {
    pub segment: u32,
    pub offset: u64,
    pub len: u32,
}

pub const REF_TAG: [u8; 5] = *b"\xFFSLG1";
pub const REF_BYTES: usize = 5 + 4 + 8 + 4;

impl Locator {
    pub fn to_ref(self) -> Vec<u8> {
        let mut o = Vec::with_capacity(REF_BYTES);
        o.extend_from_slice(&REF_TAG);
        o.extend_from_slice(&self.segment.to_be_bytes());
        o.extend_from_slice(&self.offset.to_be_bytes());
        o.extend_from_slice(&self.len.to_be_bytes());
        o
    }

    pub fn from_ref(b: &[u8]) -> Option<Locator> {
        if b.len() != REF_BYTES || b[..5] != REF_TAG {
            return None;
        }
        Some(Locator {
            segment: u32::from_be_bytes(b[5..9].try_into().ok()?),
            offset: u64::from_be_bytes(b[9..17].try_into().ok()?),
            len: u32::from_be_bytes(b[17..21].try_into().ok()?),
        })
    }
}

fn crc_of(key: u64, value: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(&key.to_be_bytes());
    h.update(&(value.len() as u32).to_be_bytes());
    h.update(value);
    h.finalize()
}

fn segment_path(dir: &Path, segment: u32) -> PathBuf {
    dir.join(format!("segment-{segment:08}.log"))
}

#[cfg(target_os = "linux")]
fn reserve(file: &File, upto: u64) {
    use std::os::fd::AsRawFd;
    // only an optimization: a filesystem that refuses just fragments more
    unsafe { libc::posix_fallocate(file.as_raw_fd(), 0, upto as libc::off_t) };
}

#[cfg(not(target_os = "linux"))]
fn reserve(_file: &File, _upto: u64) {}

fn open_segment(dir: &Path, segment: u32) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).open(segment_path(dir, segment))
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

struct Tail {
    segment: u32,
    file: Arc<File>,
    pos: u64,
    reserved: u64,
}

struct Group {
    values: Vec<(u64, Vec<u8>)>,
    queued: Instant,
    done: mpsc::Sender<io::Result<Vec<Locator>>>,
}

/// The store: one writer thread, any number of readers.
pub struct Log {
    dir: PathBuf,
    segment_bytes: u64,
    /// how far each segment may be read, the last one moving as it's written
    tail: Arc<RwLock<(u32, u64)>>,
    files: RwLock<HashMap<u32, Arc<File>>>,
    writer: Mutex<Option<mpsc::Sender<Group>>>,
    failure: Arc<Mutex<Option<Arc<io::Error>>>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// Only complete checksum-valid records and a verified all-zero reservation
/// suffix are safe. Legacy checkpoints are hints, not acknowledgement proofs.
fn scan_end(file: &File) -> io::Result<u64> {
    let size = file.metadata()?.len();
    let mut pos = 0u64;
    let mut header = [0u8; HEADER];
    let mut scratch = [0u8; 64 * 1024];
    while pos < size {
        let available = size - pos;
        if available < HEADER as u64 {
            if zero_suffix(file, pos, size, &mut scratch)? {
                return Ok(pos);
            }
            return Err(invalid("ambiguous nonzero partial record header"));
        }
        file.read_exact_at(&mut header, pos)?;
        if header.iter().all(|byte| *byte == 0) {
            if zero_suffix(file, pos, size, &mut scratch)? {
                return Ok(pos);
            }
            return Err(invalid("nonzero bytes after a zero record boundary"));
        }
        if header[..4] != MAGIC {
            return Err(invalid("invalid committed record magic"));
        }
        let len = u32::from_be_bytes(header[16..20].try_into().unwrap()) as usize;
        if len > MAX_FRAME {
            return Err(invalid("record length exceeds admission"));
        }
        let end = pos
            .checked_add(HEADER as u64)
            .and_then(|at| at.checked_add(len as u64))
            .ok_or_else(|| invalid("record extent overflow"))?;
        if end > size {
            return Err(invalid("ambiguous nonzero partial record payload"));
        }
        let mut crc = crc32fast::Hasher::new();
        crc.update(&header[8..]);
        let mut at = pos + HEADER as u64;
        while at < end {
            let take = usize::try_from((end - at).min(scratch.len() as u64)).unwrap();
            file.read_exact_at(&mut scratch[..take], at)?;
            crc.update(&scratch[..take]);
            at += take as u64;
        }
        if crc.finalize() != u32::from_be_bytes(header[4..8].try_into().unwrap()) {
            return Err(invalid("committed record checksum failure"));
        }
        pos = end;
    }
    Ok(pos)
}

fn zero_suffix(file: &File, mut at: u64, end: u64, scratch: &mut [u8]) -> io::Result<bool> {
    while at < end {
        let take = usize::try_from((end - at).min(scratch.len() as u64)).unwrap();
        file.read_exact_at(&mut scratch[..take], at)?;
        if scratch[..take].iter().any(|byte| *byte != 0) {
            return Ok(false);
        }
        at += take as u64;
    }
    Ok(true)
}

/// A failed writer cannot publish another location. The original I/O error
/// remains the source for every rejected current, queued or future append.
#[derive(Debug)]
pub struct WriterFenced {
    cause: Arc<io::Error>,
}
impl std::fmt::Display for WriterFenced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sealed-log writer is fenced: {}", self.cause)
    }
}
impl std::error::Error for WriterFenced {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.cause.as_ref())
    }
}
fn fenced(cause: &Arc<io::Error>) -> io::Error {
    io::Error::new(cause.kind(), WriterFenced { cause: cause.clone() })
}
fn writer_error(failure: &Mutex<Option<Arc<io::Error>>>) -> io::Error {
    failure
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(fenced)
        .unwrap_or_else(|| Error::new(ErrorKind::BrokenPipe, "sealed-log writer stopped"))
}

impl Log {
    pub fn open(dir: &Path) -> io::Result<Arc<Log>> {
        Log::open_with(dir, SEGMENT_BYTES)
    }

    pub fn open_with(dir: &Path, segment_bytes: u64) -> io::Result<Arc<Log>> {
        if segment_bytes < HEADER as u64 {
            return Err(invalid("segment size is smaller than a record header"));
        }
        fs::create_dir_all(dir)?;
        let mut last = None;
        let mut count = 0u64;
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(number) = name.to_str().and_then(|name| name.strip_prefix("segment-")?.strip_suffix(".log"))
            else {
                continue;
            };
            let segment = number.parse::<u32>().map_err(|_| invalid("malformed segment filename"))?;
            let expected = format!("segment-{segment:08}.log");
            if name.to_str() != Some(expected.as_str()) {
                return Err(invalid("noncanonical segment filename"));
            }
            count += 1;
            last = Some(last.map_or(segment, |old: u32| old.max(segment)));
        }
        if last.is_some_and(|last| u64::from(last) + 1 != count) {
            return Err(invalid("segment inventory contains a gap"));
        }
        let existing = last.is_some();
        if !existing && (dir.join("tail").try_exists()? || dir.join("tail.tmp").try_exists()?) {
            return Err(invalid("checkpoint exists without its segment inventory"));
        }
        let last = last.unwrap_or(0);
        let file = Arc::new(if existing {
            OpenOptions::new().read(true).write(true).open(segment_path(dir, last))?
        } else {
            open_segment(dir, last)?
        });
        // A checkpoint cannot certify skipped bytes. Validate the complete last
        // active segment in bounded memory; older immutable records are checked
        // again when read. No archive-wide health claim follows from this scan.
        let pos = scan_end(&file)?;
        sync_dir(dir)?;
        let reserved = file.metadata()?.len();
        let tail = Arc::new(RwLock::new((last, pos)));
        let (tx, rx) = mpsc::channel::<Group>();
        let failure = Arc::new(Mutex::new(None));
        let mut files = HashMap::new();
        files.insert(last, file.clone());
        let log = Arc::new(Log {
            dir: dir.to_path_buf(),
            segment_bytes,
            tail: tail.clone(),
            files: RwLock::new(files),
            writer: Mutex::new(Some(tx)),
            failure: failure.clone(),
            worker: Mutex::new(None),
        });
        let state = Tail { segment: last, file, pos, reserved };
        let (d, shared) = (dir.to_path_buf(), tail);
        #[cfg(test)]
        let fixture_writer = test_io::skip_reservation(dir);
        #[cfg(test)]
        let fixture_fault = test_io::registered_fault(dir);
        let worker = std::thread::Builder::new().name("sealed-log-writer".into()).spawn(move || {
            #[cfg(test)]
            let _fixture = fixture_writer.then(|| test_io::Scope::enter(&d, fixture_fault));
            write_loop(d, segment_bytes, state, shared, failure, rx)
        })?;
        *log.worker.lock().unwrap_or_else(|e| e.into_inner()) = Some(worker);
        Ok(log)
    }

    /// Stores admitted values, returning locations only after durable commit.
    pub fn append(&self, values: Vec<(u64, Vec<u8>)>) -> io::Result<Vec<Locator>> {
        append_extent(values.iter().map(|(_, value)| value.len()), MAX_FRAME)?;
        let failure = self.failure.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(cause) = failure {
            return Err(fenced(&cause));
        }
        let (done, wait) = mpsc::channel();
        let sent = self
            .writer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .ok_or_else(|| writer_error(&self.failure))?
            .send(Group { values, done, queued: Instant::now() });
        if sent.is_err() {
            return Err(writer_error(&self.failure));
        }
        wait.recv().unwrap_or_else(|_| Err(writer_error(&self.failure)))
    }

    fn file(&self, segment: u32) -> io::Result<Arc<File>> {
        if let Some(f) = self.files.read().unwrap().get(&segment) {
            return Ok(f.clone());
        }
        let f = Arc::new(File::open(segment_path(&self.dir, segment))?);
        self.files.write().unwrap().insert(segment, f.clone());
        Ok(f)
    }

    /// A value, its checksum checked. A place past what's been written is an error.
    pub fn read(&self, at: Locator) -> io::Result<Vec<u8>> {
        let started = Instant::now();
        let value = self.read_unmeasured(at);
        STATS.disk_read_seconds.since(started);
        if let Ok(v) = &value {
            STATS.values_read.fetch_add(1, Relaxed);
            STATS.bytes_read.fetch_add(v.len() as u64, Relaxed);
        }
        value
    }

    fn read_unmeasured(&self, at: Locator) -> io::Result<Vec<u8>> {
        if at.len as usize > MAX_FRAME {
            return Err(invalid("locator length exceeds record admission"));
        }
        let end = at
            .offset
            .checked_add(HEADER as u64)
            .and_then(|n| n.checked_add(u64::from(at.len)))
            .ok_or_else(|| invalid("locator extent overflow"))?;
        let (segment, committed) = *self.tail.read().unwrap_or_else(|e| e.into_inner());
        if at.segment > segment || (at.segment == segment && end > committed) {
            return Err(invalid("locator is past the committed log boundary"));
        }
        let file = self.file(at.segment)?;
        if end > file.metadata()?.len() {
            return Err(invalid("locator is past the physical segment boundary"));
        }
        let mut header = [0u8; HEADER];
        file.read_exact_at(&mut header, at.offset)?;
        if header[..4] != MAGIC {
            return Err(invalid("locator is not a record boundary"));
        }
        let len = u32::from_be_bytes(header[16..20].try_into().unwrap());
        if len != at.len || len as usize > MAX_FRAME {
            return Err(invalid("locator and record lengths differ"));
        }
        #[cfg(test)]
        test_io::before_payload_allocation(&self.dir, len as usize)?;
        let mut value = vec![0u8; len as usize];
        file.read_exact_at(&mut value, at.offset + HEADER as u64)?;
        let key = u64::from_be_bytes(header[8..16].try_into().unwrap());
        if crc_of(key, &value) != u32::from_be_bytes(header[4..8].try_into().unwrap()) {
            STATS.checksum_failures.fetch_add(1, Relaxed);
            return Err(invalid("record failed its checksum"));
        }
        Ok(value)
    }

    pub fn stats(&self) -> String {
        let (seg, pos, bytes) = self.tail();
        format!("{{\"segment\":{seg},\"position\":{pos},\"bytes\":{bytes}}}")
    }

    /// The segment being written, how far it has come, and the bytes the log holds.
    pub fn tail(&self) -> (u32, u64, u64) {
        let (seg, pos) = *self.tail.read().unwrap();
        (seg, pos, seg as u64 * self.segment_bytes + pos)
    }

    pub fn segment_bytes(&self) -> u64 {
        self.segment_bytes
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Log {
    fn drop(&mut self) {
        self.writer.get_mut().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(worker) = self.worker.get_mut().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = worker.join();
        }
    }
}

fn write_loop(
    dir: PathBuf,
    segment_bytes: u64,
    mut t: Tail,
    shared: Arc<RwLock<(u32, u64)>>,
    failure: Arc<Mutex<Option<Arc<io::Error>>>>,
    rx: mpsc::Receiver<Group>,
) {
    while let Ok(first) = rx.recv() {
        let mut groups = vec![first];
        let mut total: usize = groups[0].values.iter().map(|v| v.1.len()).sum();
        while total < MAX_GROUP_BYTES {
            let Ok(more) = rx.try_recv() else { break };
            total += more.values.iter().map(|v| v.1.len()).sum::<usize>();
            groups.push(more);
        }
        let started = Instant::now();
        for g in &groups {
            STATS.queue_seconds.observe(started.saturating_duration_since(g.queued));
        }
        let answers = write_group(&dir, segment_bytes, &mut t, &groups);
        STATS.commit_seconds.since(started);
        match answers {
            Ok(mut per_group) => {
                *shared.write().unwrap() = (t.segment, t.pos);
                STATS.commits.fetch_add(1, Relaxed);
                STATS.commit_appends.fetch_add(groups.len() as u64, Relaxed);
                let values: usize = groups.iter().map(|g| g.values.len()).sum();
                STATS.values_appended.fetch_add(values as u64, Relaxed);
                STATS.bytes_appended.fetch_add(total as u64, Relaxed);
                for g in groups.iter().rev() {
                    let _ = g.done.send(Ok(per_group.pop().unwrap_or_default()));
                }
            }
            Err(e) => {
                let cause = Arc::new(e);
                *failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(cause.clone());
                STATS.commit_failures.fetch_add(1, Relaxed);
                for group in &groups {
                    let _ = group.done.send(Err(fenced(&cause)));
                }
                // Dropping rx rejects every queued sender; append returns this
                // same retained cause. Neither failed tail nor checkpoint moves.
                return;
            }
        }
    }
}

fn write_group(dir: &Path, segment_bytes: u64, t: &mut Tail, groups: &[Group]) -> io::Result<Vec<Vec<Locator>>> {
    let mut out = Vec::with_capacity(groups.len());
    let mut buf: Vec<u8> = Vec::new();
    let mut buf_at = t.pos;
    for g in groups {
        let mut places = Vec::with_capacity(g.values.len());
        for (key, value) in &g.values {
            let size = u64::try_from(HEADER.checked_add(value.len()).ok_or_else(|| invalid("record size overflow"))?)
                .map_err(|_| invalid("record size overflow"))?;
            let end = t.pos.checked_add(size).ok_or_else(|| invalid("segment position overflow"))?;
            if t.pos > 0 && end > segment_bytes {
                t.file.write_all_at(&buf, buf_at)?;
                buf.clear();
                sync(&t.file)?;
                STATS.segments_rolled.fetch_add(1, Relaxed);
                // the unused rest of the reservation goes back
                t.file.set_len(t.pos)?;
                t.segment = t.segment.checked_add(1).ok_or_else(|| invalid("segment index exhausted"))?;
                #[cfg(test)]
                test_io::before_rollover_open(dir)?;
                t.file = Arc::new(open_segment(dir, t.segment)?);
                sync_dir(dir)?;
                t.pos = 0;
                t.reserved = 0;
                buf_at = 0;
            }
            let end = t.pos.checked_add(size).ok_or_else(|| invalid("segment position overflow"))?;
            if end > t.reserved {
                t.reserved = end
                    .checked_add(ALLOCATE_BYTES - 1)
                    .and_then(|n| (n / ALLOCATE_BYTES).checked_mul(ALLOCATE_BYTES))
                    .ok_or_else(|| invalid("reservation extent overflow"))?;
                #[cfg(test)]
                let reserve_here = !test_io::skip_reservation(dir);
                #[cfg(not(test))]
                let reserve_here = true;
                if reserve_here {
                    reserve(&t.file, t.reserved);
                }
            }
            places.push(Locator { segment: t.segment, offset: t.pos, len: value.len() as u32 });
            buf.extend_from_slice(&MAGIC);
            buf.extend_from_slice(&crc_of(*key, value).to_be_bytes());
            buf.extend_from_slice(&key.to_be_bytes());
            buf.extend_from_slice(&(value.len() as u32).to_be_bytes());
            buf.extend_from_slice(value);
            t.pos = end;
        }
        out.push(places);
    }
    #[cfg(test)]
    test_io::before_final_write(dir, &t.file, &buf, buf_at)?;
    t.file.write_all_at(&buf, buf_at)?;
    #[cfg(test)]
    test_io::before_final_sync(dir)?;
    sync(&t.file)?;
    Ok(out)
}

fn sync(file: &File) -> io::Result<()> {
    let started = Instant::now();
    let done = file.sync_data();
    STATS.fdatasync_seconds.since(started);
    done
}

fn read_frame(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let len = u32::from_be_bytes(head[1..5].try_into().unwrap()) as usize;
    if len > MAX_FRAME {
        return Err(invalid("frame too big"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok((head[0], body))
}

fn write_frame(w: &mut impl Write, tag: u8, body: &[u8]) -> io::Result<()> {
    if body.len() > MAX_FRAME {
        return Err(invalid("frame too big"));
    }
    w.write_all(&[tag])?;
    w.write_all(&(body.len() as u32).to_be_bytes())?;
    w.write_all(body)?;
    w.flush()
}

fn take<'a>(b: &mut &'a [u8], n: usize) -> io::Result<&'a [u8]> {
    if b.len() < n {
        return Err(invalid("request cut short"));
    }
    let (head, rest) = b.split_at(n);
    *b = rest;
    Ok(head)
}

#[derive(Default)]
struct AppendExtent {
    request: usize,
    response: usize,
    count: usize,
}

impl AppendExtent {
    fn include(&mut self, value: usize, limit: usize) -> io::Result<()> {
        let request = self
            .request
            .checked_add(12)
            .and_then(|n| n.checked_add(value))
            .filter(|n| *n <= limit)
            .ok_or_else(|| invalid("append request exceeds frame admission"))?;
        let count = self.count.checked_add(1).ok_or_else(|| invalid("append count overflow"))?;
        let response = count
            .checked_mul(REF_BYTES)
            .filter(|n| *n <= limit)
            .ok_or_else(|| invalid("append response exceeds frame admission"))?;
        *self = Self { request, response, count };
        Ok(())
    }
}

fn append_extent(lengths: impl IntoIterator<Item = usize>, limit: usize) -> io::Result<AppendExtent> {
    let mut extent = AppendExtent::default();
    for len in lengths {
        extent.include(len, limit)?;
    }
    Ok(extent)
}

fn handle_append(log: &Log, body: &[u8], limit: usize) -> io::Result<Vec<u8>> {
    // Admit both wire directions using borrowed bytes before owning any values.
    let mut b = body;
    let mut extent = AppendExtent::default();
    while !b.is_empty() {
        take(&mut b, 8)?;
        let len = u32::from_be_bytes(take(&mut b, 4)?.try_into().unwrap()) as usize;
        extent.include(len, limit)?;
        take(&mut b, len)?;
    }
    let mut values = Vec::new();
    values.try_reserve_exact(extent.count).map_err(Error::other)?;
    let mut out = Vec::new();
    out.try_reserve_exact(extent.response).map_err(Error::other)?;
    b = body;
    while !b.is_empty() {
        let key = u64::from_be_bytes(take(&mut b, 8)?.try_into().unwrap());
        let len = u32::from_be_bytes(take(&mut b, 4)?.try_into().unwrap()) as usize;
        values.push((key, take(&mut b, len)?.to_vec()));
    }
    for at in log.append(values)? {
        out.extend_from_slice(&at.to_ref());
    }
    Ok(out)
}

fn handle(log: &Log, op: u8, body: &[u8]) -> io::Result<Vec<u8>> {
    let started = Instant::now();
    let answer = handle_unmeasured(log, op, body);
    let counted = match op {
        OP_APPEND => &STATS.append,
        OP_READ => &STATS.read,
        OP_STATS => &STATS.stats,
        _ => return answer,
    };
    counted.requests.fetch_add(1, Relaxed);
    if answer.is_err() {
        counted.errors.fetch_add(1, Relaxed);
    }
    counted.seconds.since(started);
    answer
}

fn handle_unmeasured(log: &Log, op: u8, body: &[u8]) -> io::Result<Vec<u8>> {
    let mut b = body;
    match op {
        OP_APPEND => handle_append(log, body, MAX_FRAME),
        OP_READ => {
            if !body.len().is_multiple_of(REF_BYTES) {
                return Err(invalid("read request is not whole locators"));
            }
            let mut response = 0usize;
            for bytes in body.chunks_exact(REF_BYTES) {
                let at = Locator::from_ref(bytes).ok_or_else(|| invalid("not a place"))?;
                response = response
                    .checked_add(4)
                    .and_then(|n| n.checked_add(at.len as usize))
                    .filter(|n| *n <= MAX_FRAME)
                    .ok_or_else(|| invalid("read response exceeds frame admission"))?;
            }
            let mut out = Vec::new();
            while !b.is_empty() {
                let at = Locator::from_ref(take(&mut b, REF_BYTES)?).ok_or_else(|| invalid("not a place"))?;
                let value = log.read(at)?;
                out.extend_from_slice(&(value.len() as u32).to_be_bytes());
                out.extend_from_slice(&value);
            }
            Ok(out)
        }
        OP_STATS if body.is_empty() => Ok(log.stats().into_bytes()),
        OP_STATS => Err(invalid("stats request must be empty")),
        _ => Err(invalid("unknown request")),
    }
}

struct Open;

impl Open {
    fn new() -> Open {
        STATS.connections.fetch_add(1, Relaxed);
        STATS.connections_total.fetch_add(1, Relaxed);
        Open
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        STATS.connections.fetch_sub(1, Relaxed);
    }
}

/// Answers only mutually authenticated, explicitly authorized clients.
pub fn serve(log: Arc<Log>, listener: TcpListener, tls: Arc<ServerTls>) -> io::Result<()> {
    transport::serve(log, listener, tls, false)
}

/// A connection to the store. An incomplete exchange permanently fences it.
pub struct Client {
    stream: Option<transport::ClientStream>,
    failure: Option<Arc<io::Error>>,
}

#[derive(Debug)]
struct ClientFenced(Arc<io::Error>);
impl std::fmt::Display for ClientFenced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sealed-log client is fenced: {}", self.0)
    }
}
impl std::error::Error for ClientFenced {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

impl Client {
    pub fn connect(addr: &str, tls: &ClientTls) -> io::Result<Client> {
        Ok(Client { stream: Some(transport::connect(addr, tls)?), failure: None })
    }

    fn fence(&mut self, error: io::Error) -> io::Error {
        self.stream.take();
        let cause = self.failure.get_or_insert_with(|| Arc::new(error));
        Error::new(cause.kind(), ClientFenced(cause.clone()))
    }

    fn call(&mut self, op: u8, body: &[u8]) -> io::Result<Vec<u8>> {
        self.call_with_timeout(op, body, IO_TIMEOUT)
    }

    fn call_with_timeout(&mut self, op: u8, body: &[u8], timeout: Duration) -> io::Result<Vec<u8>> {
        if let Some(cause) = &self.failure {
            return Err(Error::new(cause.kind(), ClientFenced(cause.clone())));
        }
        let result = (|| {
            let stream = self.stream.as_mut().expect("unfenced client owns its stream");
            stream.sock.reset(timeout);
            write_frame(stream, op, body)?;
            let reply = read_frame(stream)?;
            stream.sock.check()?;
            Ok(reply)
        })();
        let (status, out) = result.map_err(|error| self.fence(error))?;
        match status {
            0 => Ok(out),
            1 => match String::from_utf8(out) {
                Ok(message) => Err(Error::other(message)),
                Err(_) => Err(self.fence(invalid("application error is not UTF-8"))),
            },
            _ => Err(self.fence(invalid("unknown response status"))),
        }
    }

    pub fn append(&mut self, values: &[(u64, &[u8])]) -> io::Result<Vec<Locator>> {
        let extent = append_extent(values.iter().map(|(_, value)| value.len()), MAX_FRAME)?;
        let mut body = Vec::new();
        body.try_reserve_exact(extent.request).map_err(Error::other)?;
        for (key, value) in values {
            body.extend_from_slice(&key.to_be_bytes());
            body.extend_from_slice(&(value.len() as u32).to_be_bytes());
            body.extend_from_slice(value);
        }
        let out = self.call(OP_APPEND, &body)?;
        if out.len() != extent.response {
            return Err(self.fence(invalid("answer has a partial or wrong number of places")));
        }
        let places: Vec<Locator> = out.chunks(REF_BYTES).filter_map(Locator::from_ref).collect();
        if places.len() != values.len() {
            return Err(self.fence(invalid("answer has the wrong number of places")));
        }
        Ok(places)
    }

    pub fn read(&mut self, places: &[Locator]) -> io::Result<Vec<Vec<u8>>> {
        let request = places
            .len()
            .checked_mul(REF_BYTES)
            .filter(|n| *n <= MAX_FRAME)
            .ok_or_else(|| invalid("read request exceeds frame admission"))?;
        places.iter().try_fold(0usize, |sum, at| {
            sum.checked_add(4)
                .and_then(|n| n.checked_add(at.len as usize))
                .filter(|n| *n <= MAX_FRAME)
                .ok_or_else(|| invalid("read response exceeds frame admission"))
        })?;
        let mut body = Vec::with_capacity(request);
        for place in places {
            body.extend_from_slice(&place.to_ref());
        }
        let out = self.call(OP_READ, &body)?;
        let parsed = (|| {
            let mut b = &out[..];
            let mut values = Vec::with_capacity(places.len());
            for place in places {
                let len = u32::from_be_bytes(take(&mut b, 4)?.try_into().unwrap()) as usize;
                if len != place.len as usize {
                    return Err(invalid("answer differs from requested record length"));
                }
                values.push(take(&mut b, len)?.to_vec());
            }
            if !b.is_empty() {
                return Err(invalid("answer has extra values"));
            }
            Ok(values)
        })();
        parsed.map_err(|error| self.fence(error))
    }

    pub fn stats(&mut self) -> io::Result<String> {
        let out = self.call(OP_STATS, &[])?;
        String::from_utf8(out).map_err(|_| self.fence(invalid("stats answer is not UTF-8")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sealed-log-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn what_goes_in_comes_back_over_the_network() {
        let log = transport::tests::open_log(&dir("net"));
        let mut client = transport::tests::peer(log, Permissions::ALL);
        let places = client.append(&[(1, b"first"), (2, b""), (3, b"third")]).unwrap();
        assert_eq!(client.read(&places).unwrap(), vec![b"first".to_vec(), vec![], b"third".to_vec()]);
        assert!(client.stats().unwrap().contains("segment"));
        assert!(client.read(&[Locator { segment: 9, offset: 0, len: 1 }]).is_err());
        assert_eq!(client.read(&places[..1]).unwrap(), vec![b"first".to_vec()]);
    }

    #[test]
    fn a_place_survives_as_bytes_and_nothing_else_passes_for_one() {
        let at = Locator { segment: 7, offset: 1 << 40, len: 99 };
        assert_eq!(Locator::from_ref(&at.to_ref()), Some(at));
        assert_eq!(at.to_ref().len(), REF_BYTES);
        assert_eq!(Locator::from_ref(b"\x28\xb5\x2f\xfd zstd frame of the same size"), None);
    }

    #[test]
    fn a_restart_refuses_ambiguous_torn_bytes_without_changing_the_prefix() {
        let d = dir("torn");
        let log = Log::open(&d).unwrap();
        let places = log.append(vec![(1, b"keep".to_vec()), (2, b"also".to_vec())]).unwrap();
        drop(log);
        let path = segment_path(&d, 0);
        let good = places[1].offset + (HEADER + 4) as u64;
        let file = OpenOptions::new().read(true).write(true).open(&path).unwrap();
        file.write_all_at(&MAGIC, good).unwrap();
        file.write_all_at(&[9u8; 30], good + 4).unwrap();
        let length = file.metadata().unwrap().len();
        let mut prefix = vec![0; good as usize + 34];
        file.read_exact_at(&mut prefix, 0).unwrap();
        for _ in 0..2 {
            assert!(matches!(Log::open(&d),Err(error) if error.kind()==ErrorKind::InvalidData));
            let mut after = vec![0; prefix.len()];
            file.read_exact_at(&mut after, 0).unwrap();
            assert_eq!(after, prefix);
            assert_eq!(file.metadata().unwrap().len(), length);
        }
    }

    #[test]
    fn a_flipped_bit_is_an_error_not_a_value() {
        let d = dir("flip");
        let log = Log::open(&d).unwrap();
        let at = log.append(vec![(1, vec![5u8; 100])]).unwrap()[0];
        let f = OpenOptions::new().write(true).open(segment_path(&d, 0)).unwrap();
        f.write_all_at(&[6], at.offset + HEADER as u64 + 50).unwrap();
        assert!(log.read(at).unwrap_err().to_string().contains("checksum"));
    }

    #[test]
    fn segments_roll_over_and_all_stay_readable() {
        let d = dir("roll");
        let log = Log::open_with(&d, 1000).unwrap();
        let mut places = Vec::new();
        for i in 0..50u8 {
            places.extend(log.append(vec![(i as u64, vec![i; 100])]).unwrap());
        }
        assert!(places.last().unwrap().segment > 2);
        for (i, at) in places.iter().enumerate() {
            assert_eq!(log.read(*at).unwrap(), vec![i as u8; 100]);
        }
        drop(log);
        let log = Log::open_with(&d, 1000).unwrap();
        let next = log.append(vec![(99, b"on".to_vec())]).unwrap()[0];
        assert_eq!(next.segment, places.last().unwrap().segment.max(next.segment));
        assert_eq!(log.read(places[3]).unwrap(), vec![3u8; 100]);
    }

    #[test]
    fn appends_at_once_all_get_their_own_place() {
        let log = Log::open(&dir("many")).unwrap();
        let handles: Vec<_> = (0..8u64)
            .map(|t| {
                let log = log.clone();
                std::thread::spawn(move || {
                    (0..200u64)
                        .map(|i| {
                            let value = format!("{t}-{i}").into_bytes();
                            (log.append(vec![(i, value.clone())]).unwrap()[0], value)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for h in handles {
            for (at, value) in h.join().unwrap() {
                assert_eq!(log.read(at).unwrap(), value);
            }
        }
    }
}
