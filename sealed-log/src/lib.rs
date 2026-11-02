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
use std::io::{self, BufReader, BufWriter, Error, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::time::{Duration, Instant};

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
    done: mpsc::Sender<io::Result<Vec<Locator>>>,
}

/// The store: one writer thread, any number of readers.
pub struct Log {
    dir: PathBuf,
    segment_bytes: u64,
    /// how far each segment may be read, the last one moving as it's written
    tail: Arc<RwLock<(u32, u64)>>,
    files: RwLock<HashMap<u32, Arc<File>>>,
    writer: Mutex<mpsc::Sender<Group>>,
}

/// The end of the good records of a segment, looking from `from`.
fn scan_end(file: &File, from: u64) -> io::Result<u64> {
    let size = file.metadata()?.len();
    let mut pos = from;
    let mut header = [0u8; HEADER];
    while pos + HEADER as u64 <= size {
        file.read_exact_at(&mut header, pos)?;
        if header[..4] != MAGIC {
            break;
        }
        let crc = u32::from_be_bytes(header[4..8].try_into().unwrap());
        let key = u64::from_be_bytes(header[8..16].try_into().unwrap());
        let len = u32::from_be_bytes(header[16..20].try_into().unwrap()) as u64;
        if len as usize > MAX_FRAME || pos + HEADER as u64 + len > size {
            break;
        }
        let mut value = vec![0u8; len as usize];
        file.read_exact_at(&mut value, pos + HEADER as u64)?;
        if crc_of(key, &value) != crc {
            break;
        }
        pos += HEADER as u64 + len;
    }
    Ok(pos)
}

impl Log {
    pub fn open(dir: &Path) -> io::Result<Arc<Log>> {
        Log::open_with(dir, SEGMENT_BYTES)
    }

    pub fn open_with(dir: &Path, segment_bytes: u64) -> io::Result<Arc<Log>> {
        fs::create_dir_all(dir)?;
        let last = fs::read_dir(dir)?
            .filter_map(|e| e.ok()?.file_name().to_str()?.strip_prefix("segment-")?.strip_suffix(".log")?.parse().ok())
            .max()
            .unwrap_or(0u32);
        let file = Arc::new(open_segment(dir, last)?);
        // a checkpoint saves reading the whole of the last segment after a restart
        let checkpoint = fs::read_to_string(dir.join("tail")).ok().and_then(|t| {
            let (s, o) = t.trim().split_once(' ')?;
            Some((s.parse::<u32>().ok()?, o.parse::<u64>().ok()?))
        });
        let from = checkpoint.filter(|c| c.0 == last).map_or(0, |c| c.1);
        let pos = scan_end(&file, from)?;
        sync_dir(dir)?;
        let reserved = file.metadata()?.len();
        let tail = Arc::new(RwLock::new((last, pos)));
        let (tx, rx) = mpsc::channel::<Group>();
        let mut files = HashMap::new();
        files.insert(last, file.clone());
        let log = Arc::new(Log {
            dir: dir.to_path_buf(),
            segment_bytes,
            tail: tail.clone(),
            files: RwLock::new(files),
            writer: Mutex::new(tx),
        });
        let state = Tail { segment: last, file, pos, reserved };
        let (d, shared) = (dir.to_path_buf(), tail);
        std::thread::Builder::new().name("sealed-log-writer".into()).spawn(move || write_loop(d, segment_bytes, state, shared, rx))?;
        Ok(log)
    }

    /// Stores values, returning where each went once they are on disk.
    pub fn append(&self, values: Vec<(u64, Vec<u8>)>) -> io::Result<Vec<Locator>> {
        let (done, wait) = mpsc::channel();
        self.writer.lock().unwrap().send(Group { values, done }).map_err(|_| Error::other("writer stopped"))?;
        wait.recv().map_err(|_| Error::other("writer stopped"))?
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
        let (seg, pos) = *self.tail.read().unwrap();
        let end = at.offset + HEADER as u64 + at.len as u64;
        if at.segment > seg || (at.segment == seg && end > pos) {
            return Err(invalid(format!("{at:?} is past the end of the log")));
        }
        let mut raw = vec![0u8; HEADER + at.len as usize];
        self.file(at.segment)?.read_exact_at(&mut raw, at.offset)?;
        if raw[..4] != MAGIC {
            return Err(invalid(format!("{at:?} is not the start of a record")));
        }
        let crc = u32::from_be_bytes(raw[4..8].try_into().unwrap());
        let key = u64::from_be_bytes(raw[8..16].try_into().unwrap());
        let len = u32::from_be_bytes(raw[16..20].try_into().unwrap());
        if len != at.len {
            return Err(invalid(format!("{at:?} holds {len} bytes")));
        }
        let value = raw.split_off(HEADER);
        if crc_of(key, &value) != crc {
            return Err(invalid(format!("{at:?} failed its checksum")));
        }
        Ok(value)
    }

    pub fn stats(&self) -> String {
        let (seg, pos) = *self.tail.read().unwrap();
        format!("{{\"segment\":{seg},\"position\":{pos},\"bytes\":{}}}", seg as u64 * self.segment_bytes + pos)
    }
}

fn write_loop(dir: PathBuf, segment_bytes: u64, mut t: Tail, shared: Arc<RwLock<(u32, u64)>>, rx: mpsc::Receiver<Group>) {
    let mut last_checkpoint = Instant::now();
    while let Ok(first) = rx.recv() {
        let mut groups = vec![first];
        let mut total: usize = groups[0].values.iter().map(|v| v.1.len()).sum();
        while total < MAX_GROUP_BYTES {
            let Ok(more) = rx.try_recv() else { break };
            total += more.values.iter().map(|v| v.1.len()).sum::<usize>();
            groups.push(more);
        }
        let before = (t.segment, t.pos);
        let answers = write_group(&dir, segment_bytes, &mut t, &groups);
        match answers {
            Ok(mut per_group) => {
                *shared.write().unwrap() = (t.segment, t.pos);
                for g in groups.iter().rev() {
                    let _ = g.done.send(Ok(per_group.pop().unwrap_or_default()));
                }
            }
            Err(e) => {
                // nobody got these places, so they're written over: a gap here
                // would end the scan of a restart early and lose what follows
                if t.segment == before.0 {
                    t.pos = before.1;
                }
                eprintln!("sealed-log: write failed: {e}");
                for g in &groups {
                    let _ = g.done.send(Err(Error::new(e.kind(), e.to_string())));
                }
            }
        }
        if last_checkpoint.elapsed() > Duration::from_secs(5) {
            last_checkpoint = Instant::now();
            let tmp = dir.join("tail.tmp");
            if fs::write(&tmp, format!("{} {}", t.segment, t.pos)).is_ok() {
                let _ = fs::rename(&tmp, dir.join("tail"));
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
            let size = (HEADER + value.len()) as u64;
            if t.pos > 0 && t.pos + size > segment_bytes {
                t.file.write_all_at(&buf, buf_at)?;
                buf.clear();
                t.file.sync_data()?;
                // the unused rest of the reservation goes back
                t.file.set_len(t.pos)?;
                t.segment += 1;
                t.file = Arc::new(open_segment(dir, t.segment)?);
                sync_dir(dir)?;
                t.pos = 0;
                t.reserved = 0;
                buf_at = 0;
            }
            if t.pos + size > t.reserved {
                t.reserved = (t.pos + size).div_ceil(ALLOCATE_BYTES) * ALLOCATE_BYTES;
                reserve(&t.file, t.reserved);
            }
            places.push(Locator { segment: t.segment, offset: t.pos, len: value.len() as u32 });
            buf.extend_from_slice(&MAGIC);
            buf.extend_from_slice(&crc_of(*key, value).to_be_bytes());
            buf.extend_from_slice(&key.to_be_bytes());
            buf.extend_from_slice(&(value.len() as u32).to_be_bytes());
            buf.extend_from_slice(value);
            t.pos += size;
        }
        out.push(places);
    }
    t.file.write_all_at(&buf, buf_at)?;
    t.file.sync_data()?;
    Ok(out)
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

fn handle(log: &Log, op: u8, body: &[u8]) -> io::Result<Vec<u8>> {
    let mut b = body;
    match op {
        OP_APPEND => {
            let mut values = Vec::new();
            while !b.is_empty() {
                let key = u64::from_be_bytes(take(&mut b, 8)?.try_into().unwrap());
                let len = u32::from_be_bytes(take(&mut b, 4)?.try_into().unwrap()) as usize;
                values.push((key, take(&mut b, len)?.to_vec()));
            }
            Ok(log.append(values)?.into_iter().flat_map(Locator::to_ref).collect())
        }
        OP_READ => {
            let mut out = Vec::new();
            while !b.is_empty() {
                let at = Locator::from_ref(take(&mut b, REF_BYTES)?).ok_or_else(|| invalid("not a place"))?;
                let value = log.read(at)?;
                out.extend_from_slice(&(value.len() as u32).to_be_bytes());
                out.extend_from_slice(&value);
            }
            Ok(out)
        }
        OP_STATS => Ok(log.stats().into_bytes()),
        _ => Err(invalid("unknown request")),
    }
}

fn serve_one(log: &Log, s: TcpStream) -> io::Result<()> {
    s.set_nodelay(true)?;
    let mut r = BufReader::with_capacity(1 << 20, s.try_clone()?);
    let mut w = BufWriter::with_capacity(1 << 20, s);
    loop {
        let (op, body) = read_frame(&mut r)?;
        match handle(log, op, &body) {
            Ok(out) => write_frame(&mut w, 0, &out)?,
            Err(e) => write_frame(&mut w, 1, e.to_string().as_bytes())?,
        }
    }
}

/// Answers requests on `listener` until the process ends.
pub fn serve(log: Arc<Log>, listener: TcpListener) -> io::Result<()> {
    for s in listener.incoming() {
        let (log, s) = (log.clone(), s?);
        std::thread::Builder::new().name("sealed-log-conn".into()).spawn(move || {
            let _ = serve_one(&log, s);
        })?;
    }
    Ok(())
}

/// A connection to the store.
pub struct Client {
    r: BufReader<TcpStream>,
    w: BufWriter<TcpStream>,
}

impl Client {
    pub fn connect(addr: &str) -> io::Result<Client> {
        let at = addr.to_socket_addrs()?.next().ok_or_else(|| invalid("no address"))?;
        let s = TcpStream::connect_timeout(&at, Duration::from_secs(5))?;
        s.set_nodelay(true)?;
        s.set_read_timeout(Some(IO_TIMEOUT))?;
        s.set_write_timeout(Some(IO_TIMEOUT))?;
        Ok(Client { r: BufReader::with_capacity(1 << 20, s.try_clone()?), w: BufWriter::with_capacity(1 << 20, s) })
    }

    fn call(&mut self, op: u8, body: &[u8]) -> io::Result<Vec<u8>> {
        write_frame(&mut self.w, op, body)?;
        let (status, out) = read_frame(&mut self.r)?;
        if status != 0 {
            return Err(Error::other(String::from_utf8_lossy(&out).into_owned()));
        }
        Ok(out)
    }

    pub fn append(&mut self, values: &[(u64, &[u8])]) -> io::Result<Vec<Locator>> {
        let mut body = Vec::with_capacity(values.iter().map(|v| v.1.len() + 12).sum());
        for (key, value) in values {
            body.extend_from_slice(&key.to_be_bytes());
            body.extend_from_slice(&(value.len() as u32).to_be_bytes());
            body.extend_from_slice(value);
        }
        let out = self.call(OP_APPEND, &body)?;
        let places: Vec<Locator> = out.chunks(REF_BYTES).filter_map(Locator::from_ref).collect();
        if places.len() != values.len() {
            return Err(invalid("answer has the wrong number of places"));
        }
        Ok(places)
    }

    pub fn read(&mut self, places: &[Locator]) -> io::Result<Vec<Vec<u8>>> {
        let body: Vec<u8> = places.iter().flat_map(|p| p.to_ref()).collect();
        let out = self.call(OP_READ, &body)?;
        let mut b = &out[..];
        let mut values = Vec::with_capacity(places.len());
        while !b.is_empty() {
            let len = u32::from_be_bytes(take(&mut b, 4)?.try_into().unwrap()) as usize;
            values.push(take(&mut b, len)?.to_vec());
        }
        if values.len() != places.len() {
            return Err(invalid("answer has the wrong number of values"));
        }
        Ok(values)
    }

    pub fn stats(&mut self) -> io::Result<String> {
        Ok(String::from_utf8_lossy(&self.call(OP_STATS, &[])?).into_owned())
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
        let log = Log::open(&dir("net")).unwrap();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || serve(log, l));
        let mut c = Client::connect(&addr).unwrap();
        let places = c.append(&[(1, b"first"), (2, b""), (3, b"third")]).unwrap();
        assert_eq!(c.read(&places).unwrap(), vec![b"first".to_vec(), vec![], b"third".to_vec()]);
        assert!(c.stats().unwrap().contains("segment"));
        // the connection outlives an error
        assert!(c.read(&[Locator { segment: 9, offset: 0, len: 1 }]).is_err());
        assert_eq!(c.read(&places[..1]).unwrap(), vec![b"first".to_vec()]);
    }

    #[test]
    fn a_place_survives_as_bytes_and_nothing_else_passes_for_one() {
        let at = Locator { segment: 7, offset: 1 << 40, len: 99 };
        assert_eq!(Locator::from_ref(&at.to_ref()), Some(at));
        assert_eq!(at.to_ref().len(), REF_BYTES);
        assert_eq!(Locator::from_ref(b"\x28\xb5\x2f\xfd zstd frame of the same size"), None);
    }

    #[test]
    fn a_restart_finds_the_end_and_drops_a_torn_write() {
        let d = dir("torn");
        let log = Log::open(&d).unwrap();
        let places = log.append(vec![(1, b"keep".to_vec()), (2, b"also".to_vec())]).unwrap();
        drop(log);
        // half a record, as a crash leaves it
        let path = segment_path(&d, 0);
        let good = places[1].offset + (HEADER + 4) as u64;
        let f = OpenOptions::new().write(true).open(&path).unwrap();
        f.write_all_at(&MAGIC, good).unwrap();
        f.write_all_at(&[9u8; 30], good + 4).unwrap();
        drop(f);
        let log = Log::open(&d).unwrap();
        let more = log.append(vec![(3, b"next".to_vec())]).unwrap();
        assert_eq!(more[0].offset, good, "the torn record is written over");
        assert_eq!(log.read(places[0]).unwrap(), b"keep");
        assert_eq!(log.read(more[0]).unwrap(), b"next");
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
