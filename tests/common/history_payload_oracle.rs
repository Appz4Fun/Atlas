//! Test-only reference encoder/materializing decoder for frozen payload v1.
//! Extracted from immutable c62d0701acd07849014cd96161a0c99f2623212e
//! src/frozen/payload.rs, Git blob a07d8a57c8aa8940719acfa2e86664351d29ca73.
//! Wire helpers, version, compression level and limits are unchanged. Only the
//! neutral Value DTO replaces the retired storage-crate type; unused lookup
//! methods and the old module's own tests are not needed by this oracle.
//! Scale tests use bounded production history_stream directly, not this
//! materializing reference. This file is included solely by its test target.
//!
//! A frozen release's rows in one value: every table it had rows in, each with its column names, and the message-id
//! domains its articles use by their text. zstd compressed. Format v1: u8 version, varint domain count, then per
//! domain (varint id, text suffix); varint table count, then per table (text name, varint column count, the column
//! names as text, varint row count, then per row one value per column). A value is a tag byte (0 null, 1 integer as
//! a zigzag varint, 2 real as 8 bytes little endian, 3 text, 4 blob), text and blobs a varint length then the bytes.

use std::io::{Error, ErrorKind, Result};

/// The five v1 wire values. This neutral reference type is used only by the
/// offline history_stream integration test, never by a runtime storage adapter.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

const VERSION: u8 = 1;
const LEVEL: i32 = 3;
/// what one payload may unpack to at most: a release with every one of its rows is far smaller
const MAX_UNPACKED: usize = 1 << 30;
/// what `encode` packs at most: a release with more rows than this stays live (its value has to fit in a row, and
/// a thaw holds it all in memory)
pub const MAX_RAW: usize = 256 << 20;

/// The rows of one table.
#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    pub name: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

impl Table {
    pub fn new(name: &str, columns: Vec<String>) -> Table {
        Table { name: name.to_string(), columns, rows: Vec::new() }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Payload {
    /// (domain id in the shard it was frozen from, its suffix)
    pub domains: Vec<(i64, String)>,
    pub tables: Vec<Table>,
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put_varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

fn bad(what: &str) -> Error {
    Error::new(ErrorKind::InvalidData, format!("frozen payload: {what}"))
}

/// The packed payload, or None when its rows come to more than `MAX_RAW`.
pub fn encode(p: &Payload) -> Option<Vec<u8>> {
    let mut raw = vec![VERSION];
    put_varint(&mut raw, p.domains.len() as u64);
    for (id, suffix) in &p.domains {
        put_varint(&mut raw, zigzag(*id));
        put_bytes(&mut raw, suffix.as_bytes());
    }
    put_varint(&mut raw, p.tables.len() as u64);
    for t in &p.tables {
        put_bytes(&mut raw, t.name.as_bytes());
        put_varint(&mut raw, t.columns.len() as u64);
        for c in &t.columns {
            put_bytes(&mut raw, c.as_bytes());
        }
        put_varint(&mut raw, t.rows.len() as u64);
        for row in &t.rows {
            for v in row {
                match v {
                    Value::Null => raw.push(0),
                    Value::Integer(i) => {
                        raw.push(1);
                        put_varint(&mut raw, zigzag(*i));
                    }
                    Value::Real(f) => {
                        raw.push(2);
                        raw.extend_from_slice(&f.to_le_bytes());
                    }
                    Value::Text(s) => {
                        raw.push(3);
                        put_bytes(&mut raw, s.as_bytes());
                    }
                    Value::Blob(b) => {
                        raw.push(4);
                        put_bytes(&mut raw, b);
                    }
                }
            }
        }
    }
    if raw.len() > MAX_RAW {
        return None;
    }
    Some(zstd::bulk::compress(&raw, LEVEL).expect("zstd compress of an in memory buffer"))
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8> {
        let b = *self.buf.get(self.pos).ok_or_else(|| bad("cut short"))?;
        self.pos += 1;
        Ok(b)
    }

    fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(bad("varint too long"))
    }

    /// a count of things each at least `min` bytes long, never more than what is left could hold
    fn count(&mut self, min: usize) -> Result<usize> {
        let n = self.varint()?;
        let left = (self.buf.len() - self.pos) as u64;
        if n > left / min.max(1) as u64 {
            return Err(bad("count past the end"));
        }
        Ok(n as usize)
    }

    fn bytes(&mut self) -> Result<&[u8]> {
        let n = self.count(1)?;
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn text(&mut self) -> Result<String> {
        String::from_utf8(self.bytes()?.to_vec()).map_err(|_| bad("text is not utf-8"))
    }
}

pub fn decode(packed: &[u8]) -> Result<Payload> {
    use std::io::Read;
    let mut raw = Vec::new();
    zstd::stream::read::Decoder::new(packed)?.take(MAX_UNPACKED as u64 + 1).read_to_end(&mut raw)?;
    if raw.len() > MAX_UNPACKED {
        return Err(bad("too big"));
    }
    let mut r = Reader { buf: &raw, pos: 0 };
    if r.byte()? != VERSION {
        return Err(bad("unknown version"));
    }
    let mut p = Payload::default();
    for _ in 0..r.count(2)? {
        let id = unzigzag(r.varint()?);
        p.domains.push((id, r.text()?));
    }
    for _ in 0..r.count(3)? {
        let name = r.text()?;
        let ncols = r.count(1)?;
        let mut columns = Vec::with_capacity(ncols);
        for _ in 0..ncols {
            columns.push(r.text()?);
        }
        let nrows = r.count(ncols.max(1))?;
        let mut rows = Vec::with_capacity(nrows);
        for _ in 0..nrows {
            let mut row = Vec::with_capacity(ncols);
            for _ in 0..ncols {
                row.push(match r.byte()? {
                    0 => Value::Null,
                    1 => Value::Integer(unzigzag(r.varint()?)),
                    2 => {
                        let end = r.pos + 8;
                        let b: [u8; 8] = r.buf.get(r.pos..end).ok_or_else(|| bad("cut short"))?.try_into().unwrap();
                        r.pos = end;
                        Value::Real(f64::from_le_bytes(b))
                    }
                    3 => Value::Text(r.text()?),
                    4 => Value::Blob(r.bytes()?.to_vec()),
                    _ => return Err(bad("unknown value tag")),
                });
            }
            rows.push(row);
        }
        p.tables.push(Table { name, columns, rows });
    }
    if r.pos != raw.len() {
        return Err(bad("bytes past the end"));
    }
    Ok(p)
}
