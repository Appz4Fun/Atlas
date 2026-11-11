//! One in-flight frozen decoder with bounded cross-page backpressure.
//! Decoder state survives ordinary pages. Dropping the owner cancels and joins
//! this worker; only a subsequent process/session recovery restarts a frame.
use crate::history_identity::{CellType, Column};
use crate::history_stream::{self, Admission, BytesKind, Event, ReadClass};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read, Write};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum ProjectedValue {
    Null,
    Integer(i64),
    RealBits(u64),
    Bytes { kind: CellType, offset: u64, length: u64, digest: [u8; 32] },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Projection {
    pub fields: Vec<(Column, ProjectedValue)>,
    pub byte_length: u64,
    pub digest: [u8; 32],
}
pub(crate) struct Validated {
    pub stats: history_stream::Stats,
    pub projection: Projection,
}
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Usage {
    pub compressed: u64,
    pub expanded: u64,
    pub events: u64,
}
pub(crate) struct Progress {
    pub usage: Usage,
    pub result: Option<Result<Validated>>,
}
struct State {
    remaining: [u64; 3],
    usage: Usage,
    paused: bool,
    cancelled: bool,
    result: Option<Result<Validated>>,
}
struct Meter {
    state: Mutex<State>,
    changed: Condvar,
}
impl Meter {
    fn allowance_index(&self, index: usize, requested: usize) -> io::Result<usize> {
        let mut s = self.state.lock().map_err(|_| io::Error::other("validator meter poisoned"))?;
        loop {
            if s.cancelled {
                return Err(io::Error::other("frozen validation cancelled"));
            }
            if s.remaining[index] > 0 {
                // Expanded reads can enclose a compressed-read pause. A fixed
                // 256-byte quantum fits even the next minimum-size grant, so
                // completion is charged to the page on which it actually returns.
                let quantum = if index == 1 { 256 } else { usize::MAX };
                return Ok(requested.min(quantum).min(s.remaining[index] as usize));
            }
            s.paused = true;
            self.changed.notify_all();
            s = self.changed.wait(s).map_err(|_| io::Error::other("validator meter poisoned"))?;
        }
    }
    fn complete(&self, index: usize, n: usize) -> io::Result<()> {
        let mut s = self.state.lock().map_err(|_| io::Error::other("validator meter poisoned"))?;
        s.remaining[index] =
            s.remaining[index].checked_sub(n as u64).ok_or_else(|| io::Error::other("validator admission exceeded"))?;
        let counter = match index {
            0 => &mut s.usage.compressed,
            1 => &mut s.usage.expanded,
            _ => &mut s.usage.events,
        };
        *counter = counter.checked_add(n as u64).ok_or_else(|| io::Error::other("validator counter overflow"))?;
        Ok(())
    }
    fn event(&self) -> io::Result<()> {
        self.allowance_index(2, 1)?;
        self.complete(2, 1)
    }
}
impl Admission for Meter {
    fn allowance(&self, class: ReadClass, requested: usize) -> io::Result<usize> {
        self.allowance_index(if class == ReadClass::Compressed { 0 } else { 1 }, requested)
    }
    fn completed(&self, class: ReadClass, bytes: usize) -> io::Result<()> {
        self.complete(if class == ReadClass::Compressed { 0 } else { 1 }, bytes)
    }
}
pub(crate) struct Validator {
    meter: Arc<Meter>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Validator {
    pub(crate) fn start(
        input: File,
        projection: File,
        length: u64,
        digest: [u8; 32],
        public_id: i64,
        quota: u64,
        config: history_stream::Config,
    ) -> Result<Self> {
        ensure!(length <= quota, "InsufficientBudget: frozen spool quota");
        let meter = Arc::new(Meter {
            state: Mutex::new(State {
                remaining: [0; 3],
                usage: Usage::default(),
                paused: true,
                cancelled: false,
                result: None,
            }),
            changed: Condvar::new(),
        });
        let child = meter.clone();
        let worker = std::thread::Builder::new().name("atlas-history-validator".into()).spawn(move || {
            let result = validate(input, projection, length, digest, public_id, quota, config, child.clone());
            let mut s = child.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            s.result = Some(result);
            child.changed.notify_all();
        })?;
        Ok(Self { meter, worker: Some(worker) })
    }
    pub(crate) fn advance(&mut self, bytes: u64, events: u32) -> Result<Progress> {
        ensure!(
            bytes >= 512 && events > 0,
            "InsufficientBudget: validator needs 512 byte admission and positive callback work"
        );
        let mut s = self.meter.state.lock().map_err(|_| anyhow::anyhow!("validator meter poisoned"))?;
        ensure!(!s.cancelled, "LostOwner: validator has been cancelled");
        s.remaining = [bytes / 2, bytes - bytes / 2, u64::from(events)];
        s.usage = Usage::default();
        s.paused = false;
        self.meter.changed.notify_all();
        while !s.paused && s.result.is_none() {
            s = self.meter.changed.wait(s).map_err(|_| anyhow::anyhow!("validator meter poisoned"))?;
        }
        Ok(Progress { usage: s.usage, result: s.result.take() })
    }
}
impl Drop for Validator {
    fn drop(&mut self) {
        {
            let mut s = self.meter.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            s.cancelled = true;
            self.meter.changed.notify_all();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
struct Hashed<R> {
    reader: R,
    hash: Sha256,
    bytes: u64,
}
impl<R: Read> Read for Hashed<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = self.reader.read(out)?;
        self.hash.update(&out[..n]);
        self.bytes = self.bytes.checked_add(n as u64).ok_or_else(|| io::Error::other("frozen read count overflow"))?;
        Ok(n)
    }
}
const COLUMNS: &[Column] = &[
    Column::Id,
    Column::Name,
    Column::Group,
    Column::Poster,
    Column::Posted,
    Column::Size,
    Column::Complete,
    Column::Parts,
    Column::FileTotal,
    Column::Display,
    Column::Obfuscated,
    Column::NameSource,
];
struct Matcher {
    mask: u16,
    offset: u64,
    length: u64,
    table: bool,
}
impl Matcher {
    fn new(length: u64, table: bool) -> Self {
        Self { mask: if table { 1 } else { (1 << COLUMNS.len()) - 1 }, offset: 0, length, table }
    }
    fn feed(&mut self, bytes: &[u8]) {
        for i in 0..if self.table { 1 } else { COLUMNS.len() } {
            let expected = if self.table { b"releases".as_slice() } else { COLUMNS[i].name().as_bytes() };
            if self.length != expected.len() as u64
                || self.offset as usize > expected.len()
                || bytes.len() > expected.len().saturating_sub(self.offset as usize)
                || expected.get(self.offset as usize..self.offset as usize + bytes.len()) != Some(bytes)
            {
                self.mask &= !(1 << i);
            }
        }
        self.offset += bytes.len() as u64;
    }
    fn column(&self) -> Option<Column> {
        (0..COLUMNS.len()).find(|i| self.mask & (1 << i) != 0).map(|i| COLUMNS[i])
    }
}
enum Active {
    None,
    Name(Matcher),
    Value(Option<Column>, CellType, u64, u64, Sha256),
}
struct Extractor {
    file: File,
    quota: u64,
    written: u64,
    hash: Sha256,
    fields: Vec<(Column, ProjectedValue)>,
    active: Active,
    in_release: bool,
    release_tables: u8,
    columns: Vec<(u64, Column)>,
    column: u64,
    column_name: u64,
    public_id: i64,
}
impl Extractor {
    fn selected(&self) -> Option<Column> {
        if self.in_release { self.columns.iter().find(|(i, _)| *i == self.column).map(|(_, c)| *c) } else { None }
    }
    fn push(&mut self, col: Column, value: ProjectedValue) -> io::Result<()> {
        if self.fields.iter().any(|(c, _)| *c == col) {
            return Err(io::Error::other("duplicate frozen release field"));
        }
        if col == Column::Id && value != ProjectedValue::Integer(self.public_id) {
            return Err(io::Error::other("frozen header and embedded public ID disagree"));
        }
        self.fields.push((col, value));
        Ok(())
    }
    fn event(&mut self, event: Event<'_>) -> io::Result<()> {
        match event {
            Event::Table(_) => {
                self.in_release = false;
                self.columns.clear();
            }
            Event::Column(i) => self.column_name = i,
            Event::Rows(n) if self.in_release => {
                if n != 1 {
                    return Err(io::Error::other("frozen release table must contain exactly one row"));
                }
            }
            Event::Row(_) => self.column = 0,
            Event::Null | Event::Integer(_) | Event::RealBits(_) => {
                if let Some(c) = self.selected() {
                    let value = match event {
                        Event::Null => ProjectedValue::Null,
                        Event::Integer(i) => ProjectedValue::Integer(i),
                        Event::RealBits(i) => ProjectedValue::RealBits(i),
                        _ => unreachable!(),
                    };
                    self.push(c, value)?;
                }
                self.column += 1;
            }
            Event::BytesStart { kind, len } => {
                self.active = match kind {
                    BytesKind::TableName => Active::Name(Matcher::new(len, true)),
                    BytesKind::ColumnName => Active::Name(Matcher::new(len, false)),
                    BytesKind::Text | BytesKind::Blob => Active::Value(
                        self.selected(),
                        if kind == BytesKind::Text { CellType::Text } else { CellType::Blob },
                        self.written,
                        len,
                        Sha256::new(),
                    ),
                    _ => Active::None,
                };
            }
            Event::Chunk(bytes) => match &mut self.active {
                Active::Name(m) => m.feed(bytes),
                Active::Value(Some(_), _, _, _, hash) => {
                    self.written = self
                        .written
                        .checked_add(bytes.len() as u64)
                        .ok_or_else(|| io::Error::other("projection length overflow"))?;
                    if self.written > self.quota {
                        return Err(io::Error::other("frozen projection spool quota exceeded"));
                    }
                    self.file.write_all(bytes)?;
                    hash.update(bytes);
                    self.hash.update(bytes);
                }
                _ => {}
            },
            Event::BytesEnd => match std::mem::replace(&mut self.active, Active::None) {
                Active::Name(m) if m.table => {
                    self.in_release = m.mask != 0;
                    if self.in_release {
                        self.release_tables += 1;
                        if self.release_tables != 1 {
                            return Err(io::Error::other("duplicate frozen release table"));
                        }
                    }
                }
                Active::Name(m) => {
                    if self.in_release {
                        if let Some(c) = m.column() {
                            if self.columns.iter().any(|(_, old)| *old == c) {
                                return Err(io::Error::other("duplicate frozen release column"));
                            }
                            self.columns.push((self.column_name, c));
                        }
                    }
                }
                Active::Value(col, kind, offset, length, hash) => {
                    if let Some(c) = col {
                        self.push(c, ProjectedValue::Bytes { kind, offset, length, digest: hash.finalize().into() })?;
                    }
                    self.column += 1;
                }
                _ => {}
            },
            _ => {}
        }
        Ok(())
    }
}
fn validate(
    input: File,
    projection: File,
    length: u64,
    digest: [u8; 32],
    public_id: i64,
    quota: u64,
    config: history_stream::Config,
    meter: Arc<Meter>,
) -> Result<Validated> {
    let mut input = Hashed { reader: input, hash: Sha256::new(), bytes: 0 };
    let mut extract = Extractor {
        file: projection,
        quota: quota - length,
        written: 0,
        hash: Sha256::new(),
        fields: Vec::new(),
        active: Active::None,
        in_release: false,
        release_tables: 0,
        columns: Vec::new(),
        column: 0,
        column_name: 0,
        public_id,
    };
    let stats = history_stream::frozen_admitted(&mut input, config, meter.clone(), |event| {
        meter.event()?;
        extract.event(event)
    })?;
    ensure!(
        input.bytes == length && <[u8; 32]>::from(input.hash.finalize()) == digest,
        "CorruptObject: exact frozen envelope digest/length"
    );
    ensure!(
        extract.release_tables == 1
            && [Column::Id, Column::Name, Column::Group]
                .iter()
                .all(|c| extract.fields.iter().any(|(found, _)| found == c)),
        "UnsupportedSchema: frozen release metadata missing"
    );
    extract.file.sync_all().context("fsync frozen metadata projection")?;
    Ok(Validated {
        stats,
        projection: Projection {
            fields: extract.fields,
            byte_length: extract.written,
            digest: extract.hash.finalize().into(),
        },
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::io::{Seek, SeekFrom, Write};
    #[test]
    fn admits_one_owned_frozen_validator() {
        let bytes = zstd::bulk::compress(&[1, 0, 0], 1).unwrap();
        let mut input = tempfile::tempfile().unwrap();
        input.write_all(&bytes).unwrap();
        input.sync_all().unwrap();
        input.seek(SeekFrom::Start(0)).unwrap();
        let start = std::time::Instant::now();
        let result = Validator::start(
            input,
            tempfile::tempfile().unwrap(),
            bytes.len() as u64,
            Sha256::digest(&bytes).into(),
            7,
            1 << 20,
            crate::history_stream::Config::default(),
        );
        assert!(result.is_ok(), "a valid bounded validator must start without decoding the whole frame");
        drop(result);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "cancellation must terminate rather than being retried as Interrupted"
        );
    }
    fn frame(corrupt: bool) -> Vec<u8> {
        use crate::frozen::payload::{Payload, Table};
        use rusqlite::types::Value;
        let mut p = Payload {
            domains: vec![],
            tables: vec![
                Table {
                    name: "releases".into(),
                    columns: vec!["id".into(), "name".into(), "group_name".into(), "name_source".into()],
                    rows: vec![vec![
                        Value::Integer(7),
                        Value::Text("é".repeat(65536)),
                        Value::Text("test.group".into()),
                        Value::Integer(40),
                    ]],
                },
                Table { name: "empty".into(), columns: vec![], rows: vec![vec![]; 19] },
            ],
        };
        p.tables.swap(0, 1);
        let mut bytes = crate::frozen::payload::encode(&p).unwrap();
        if corrupt {
            bytes.push(0);
        }
        bytes
    }
    #[test]
    fn tiny_pages_are_linear_and_corrupt_trailers_never_return_projection() {
        for corrupt in [false, true] {
            let bytes = frame(corrupt);
            let mut input = tempfile::tempfile().unwrap();
            input.write_all(&bytes).unwrap();
            input.sync_all().unwrap();
            input.rewind().unwrap();
            let projection = tempfile::tempfile().unwrap();
            let mut worker = Validator::start(
                input,
                projection,
                bytes.len() as u64,
                Sha256::digest(&bytes).into(),
                7,
                1 << 20,
                crate::history_stream::Config { read_buffer_bytes: 64, chunk_bytes: 63, ..Default::default() },
            )
            .unwrap();
            let (mut compressed, mut expanded, mut calls) = (0u64, 0u64, 0);
            loop {
                let progress = worker.advance(512, 3).unwrap();
                assert!(progress.usage.compressed + progress.usage.expanded <= 512);
                assert!(progress.usage.events <= 3);
                compressed += progress.usage.compressed;
                expanded += progress.usage.expanded;
                calls += 1;
                assert!(calls < 10000, "forward pages must progress");
                if let Some(result) = progress.result {
                    if corrupt {
                        assert!(result.is_err());
                    } else {
                        let good = result.unwrap();
                        assert_eq!(good.stats.rows, 20);
                        assert_eq!(good.stats.values, 4);
                        assert_eq!(compressed, bytes.len() as u64, "no growing-prefix decode");
                        assert_eq!(expanded, good.stats.expanded_bytes);
                        assert_eq!(good.projection.byte_length, 131082);
                        assert_eq!(good.projection.fields.len(), 4);
                    }
                    break;
                }
            }
        }
    }
}
