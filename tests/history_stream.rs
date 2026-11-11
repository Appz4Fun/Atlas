use std::io::{self, Read, Seek, SeekFrom, Write};
use std::{cell::Cell, rc::Rc};

use atlas::{
    blob,
    frozen::payload,
    history_stream::{self as stream, BytesKind, Config, Event, Source},
};
use rusqlite::types::Value;
use sha2::{Digest, Sha256};

// Test-only Rust allocator accounting. Zstd allocates through C and is NOT included;
// its independent window bound is exercised separately. Measurement is thread-local
// so harness/output allocations on other threads cannot masquerade as decoder state.
struct MeasuredAllocator;
thread_local! {
    static RUST_ALLOCATION: Cell<(isize, isize, u64)> = const { Cell::new((0,0,0)) };
}
fn allocation_change(delta: isize, allocated: bool) {
    let _ = RUST_ALLOCATION.try_with(|state| {
        let (live, peak, count) = state.get();
        let live = live + delta;
        state.set((live, peak.max(live), count + u64::from(allocated)));
    });
}
unsafe impl std::alloc::GlobalAlloc for MeasuredAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        let p = unsafe { std::alloc::System.alloc(layout) };
        if !p.is_null() {
            allocation_change(layout.size() as isize, true);
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        let p = unsafe { std::alloc::System.alloc_zeroed(layout) };
        if !p.is_null() {
            allocation_change(layout.size() as isize, true);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: std::alloc::Layout) {
        allocation_change(-(layout.size() as isize), false);
        unsafe { std::alloc::System.dealloc(p, layout) };
    }
    unsafe fn realloc(&self, p: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        let next = unsafe { std::alloc::System.realloc(p, layout, size) };
        if !next.is_null() {
            allocation_change(size as isize - layout.size() as isize, true);
        }
        next
    }
}
#[global_allocator]
static ALLOCATOR: MeasuredAllocator = MeasuredAllocator;
fn measure<T>(work: impl FnOnce() -> T) -> (T, isize, u64) {
    let start = RUST_ALLOCATION.with(|s| {
        let (live, _, _) = s.get();
        s.set((live, live, 0));
        live
    });
    let out = work();
    let (_, peak, count) = RUST_ALLOCATION.with(Cell::get);
    (out, peak - start, count)
}

fn tiny() -> Config {
    Config { read_buffer_bytes: 3, chunk_bytes: 2, ..Config::default() }
}
fn packed(raw: &[u8]) -> Vec<u8> {
    zstd::bulk::compress(raw, 3).unwrap()
}
fn var(out: &mut impl Write, mut n: u64) {
    while n >= 128 {
        out.write_all(&[(n as u8) | 128]).unwrap();
        n >>= 7;
    }
    out.write_all(&[n as u8]).unwrap();
}
fn bytes(out: &mut impl Write, b: &[u8]) {
    var(out, b.len() as u64);
    out.write_all(b).unwrap();
}

// Only small differential fixtures materialize output. Scale sinks below retain counters/hash only.
fn sealed_rows(packed: &[u8], base: i64) -> io::Result<Vec<blob::Seg>> {
    let mut rows = Vec::new();
    stream::sealed(packed, base, tiny(), |event| {
        match event {
            Event::Segment { part, bytes, domain, .. } => {
                rows.push(blob::Seg { part, bytes, domain, local: Vec::new() })
            }
            Event::Chunk(b) => rows.last_mut().unwrap().local.extend_from_slice(b),
            _ => {}
        }
        Ok(())
    })?;
    Ok(rows)
}
fn frozen_rows(packed: &[u8]) -> io::Result<payload::Payload> {
    let mut out = payload::Payload::default();
    let mut kind = BytesKind::Blob;
    let mut buf = Vec::new();
    stream::frozen(packed, tiny(), |event| {
        match event {
            Event::Domain(id) => out.domains.push((id, String::new())),
            Event::Table(_) => out.tables.push(payload::Table::new("", Vec::new())),
            Event::Row(_) => out.tables.last_mut().unwrap().rows.push(Vec::new()),
            Event::EmptyRows(count) => {
                // This materializing adapter is only for small oracle fixtures.
                assert!(count <= 1);
                for _ in 0..count {
                    out.tables.last_mut().unwrap().rows.push(Vec::new());
                }
            }
            Event::BytesStart { kind: k, .. } => {
                kind = k;
                buf.clear();
            }
            Event::Chunk(b) => buf.extend_from_slice(b),
            Event::BytesEnd => match kind {
                BytesKind::DomainSuffix => out.domains.last_mut().unwrap().1 = String::from_utf8(buf.clone()).unwrap(),
                BytesKind::TableName => out.tables.last_mut().unwrap().name = String::from_utf8(buf.clone()).unwrap(),
                BytesKind::ColumnName => {
                    out.tables.last_mut().unwrap().columns.push(String::from_utf8(buf.clone()).unwrap())
                }
                BytesKind::Text => out
                    .tables
                    .last_mut()
                    .unwrap()
                    .rows
                    .last_mut()
                    .unwrap()
                    .push(Value::Text(String::from_utf8(buf.clone()).unwrap())),
                BytesKind::Blob => {
                    out.tables.last_mut().unwrap().rows.last_mut().unwrap().push(Value::Blob(buf.clone()))
                }
                BytesKind::Local => panic!("local in frozen"),
            },
            Event::Null => out.tables.last_mut().unwrap().rows.last_mut().unwrap().push(Value::Null),
            Event::Integer(i) => out.tables.last_mut().unwrap().rows.last_mut().unwrap().push(Value::Integer(i)),
            Event::RealBits(i) => {
                out.tables.last_mut().unwrap().rows.last_mut().unwrap().push(Value::Real(f64::from_bits(i)))
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(out)
}
fn fixture() -> payload::Payload {
    payload::Payload {
        domains: vec![(0, "".into()), (i64::MIN, "@例.example>".into()), (i64::MAX, "@z>".into())],
        tables: vec![
            payload::Table {
                name: "files🦀".into(),
                columns: vec!["值".into()],
                rows: vec![
                    vec![Value::Null],
                    vec![Value::Integer(i64::MIN)],
                    vec![Value::Integer(i64::MAX)],
                    vec![Value::Integer(0)],
                    vec![Value::Real(-0.0)],
                    vec![Value::Real(f64::INFINITY)],
                    vec![Value::Text("aé中🦀z".into())],
                    vec![Value::Text("".into())],
                    vec![Value::Blob(vec![0, 255, 128, 1])],
                    vec![Value::Blob(vec![])],
                ],
            },
            payload::Table::new("empty", vec![]),
        ],
    }
}

#[test]
fn sealed_matches_legacy_signed_wrapping_and_local_bytes() {
    let rows = vec![
        blob::Seg { part: None, bytes: i64::MIN, domain: 0, local: vec![0, 255, 128] },
        blob::Seg { part: Some(0), bytes: i64::MAX, domain: -7, local: vec![] },
        blob::Seg { part: Some(i64::MAX), bytes: -1, domain: i64::MAX, local: b"abc".to_vec() },
        blob::Seg { part: Some(i64::MIN), bytes: 0, domain: i64::MIN, local: b"end".to_vec() },
    ];
    let encoded = blob::encode(&rows);
    assert_eq!(sealed_rows(&encoded, 0).unwrap(), blob::decode(&encoded).unwrap());
    let shifted = blob::shifted(&encoded, 8);
    assert_eq!(sealed_rows(&encoded, 8).unwrap(), blob::decode(&shifted).unwrap());
    assert!(sealed_rows(&blob::encode(&[]), 0).unwrap().is_empty());
}

#[test]
fn references_are_inspected_without_resolving_and_external_bytes_decode() {
    let packed = blob::encode(&[blob::Seg { part: None, bytes: 12, domain: 0, local: vec![255] }]);
    let at = sealed_log::Locator { segment: 4, offset: 10, len: packed.len() as u32 };
    let stored = blob::shifted(&at.to_ref(), -2);
    let r = stream::inspect_reference(&stored).unwrap();
    assert_eq!(r.domain_base, -2);
    assert_eq!(r.source, Source::Locator(at));
    assert_eq!(sealed_rows(&packed, r.domain_base).unwrap()[0].domain, 0);
    assert_eq!(stream::inspect_reference(&packed).unwrap().source, Source::Inline(&packed));
    for cut in 0..stored.len() {
        assert!(stream::inspect_reference(&stored[..cut]).is_err(), "cut {cut}");
    }
    let mut extra = at.to_ref();
    extra.push(0);
    assert!(stream::inspect_reference(&extra).is_err());
    assert!(stream::inspect_reference(b"not compressed").is_err());
}

#[test]
fn frozen_matches_legacy_all_types_and_split_utf8() {
    let p = fixture();
    let encoded = payload::encode(&p).unwrap();
    assert_eq!(frozen_rows(&encoded).unwrap(), payload::decode(&encoded).unwrap());
    assert_eq!(
        frozen_rows(&payload::encode(&payload::Payload::default()).unwrap()).unwrap(),
        payload::Payload::default()
    );
    // Preserve the exact NaN payload as bits, for which float equality is unsuitable.
    let bits = 0x7ff8_0000_0000_1234u64;
    let mut raw = vec![1, 0, 1, 1, b't', 1, 1, b'c', 1, 2];
    raw.extend(bits.to_le_bytes());
    let mut seen = None;
    stream::frozen(&packed(&raw)[..], tiny(), |e| {
        if let Event::RealBits(b) = e {
            seen = Some(b);
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, Some(bits));
}

#[test]
fn every_raw_and_compressed_truncation_is_rejected_in_both_formats() {
    let inputs = [
        blob::encode(&[blob::Seg { part: Some(2), bytes: -3, domain: -1, local: vec![0, 255, 4] }]),
        payload::encode(&fixture()).unwrap(),
    ];
    for (format, input) in inputs.iter().enumerate() {
        let decode = |p: &[u8]| {
            if format == 0 { stream::sealed(p, 0, tiny(), |_| Ok(())) } else { stream::frozen(p, tiny(), |_| Ok(())) }
        };
        assert!(decode(input).is_ok());
        for cut in 0..input.len() {
            assert!(decode(&input[..cut]).is_err(), "format {format}, compressed cut {cut}");
        }
        let raw = zstd::stream::decode_all(&input[..]).unwrap();
        let mut checksum_encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
        checksum_encoder.include_checksum(true).unwrap();
        checksum_encoder.write_all(&raw).unwrap();
        let mut checksummed = checksum_encoder.finish().unwrap();
        assert!(decode(&checksummed).is_ok());
        *checksummed.last_mut().unwrap() ^= 1;
        assert!(decode(&checksummed).is_err(), "bad frame checksum format {format}");
        let prefixed = [vec![0x50, 0x2a, 0x4d, 0x18, 0, 0, 0, 0], input.clone()].concat();
        assert!(decode(&prefixed).is_err(), "skippable prefix format {format}");
        for cut in 0..raw.len() {
            assert!(decode(&packed(&raw[..cut])).is_err(), "format {format}, raw cut {cut}");
        }
        let mut tail = raw.clone();
        tail.push(0);
        assert!(decode(&packed(&tail)).is_err());
        let mut version = raw;
        version[0] = 2;
        assert!(decode(&packed(&version)).is_err());
        for extra in [vec![0], packed(&[]), input.clone(), vec![0x50, 0x2a, 0x4d, 0x18, 0, 0, 0, 0]] {
            let mut p = input.clone();
            p.extend(extra);
            assert!(decode(&p).is_err(), "extra frame or garbage format {format}");
        }
    }
}

#[test]
fn overflowed_counts_lengths_varints_and_bad_types_are_rejected() {
    let mut huge_count = vec![1];
    var(&mut huge_count, u64::MAX);
    let overflow_varint = [vec![1], vec![255; 9], vec![2]].concat();
    let long_varint = [vec![1], vec![128; 10], vec![0]].concat();
    for raw in [huge_count, overflow_varint, long_varint] {
        assert!(stream::sealed(&packed(&raw)[..], 0, tiny(), |_| Ok(())).is_err());
        assert!(stream::frozen(&packed(&raw)[..], tiny(), |_| Ok(())).is_err());
    }
    let mut local = vec![1, 1, 0, 0, 0];
    var(&mut local, u64::MAX);
    assert!(stream::sealed(&packed(&local)[..], 0, tiny(), |_| Ok(())).is_err());
    for cell in [
        vec![5],
        vec![3, 1, 255],
        vec![3, 2, 0xc0, 0x80],
        vec![3, 3, 0xed, 0xa0, 0x80],
        vec![3, 1, 0xf0],
        vec![3, 4, 0xf4, 0x90, 0x80, 0x80],
        vec![4, 255, 255, 255, 255, 255, 255, 255, 255, 255, 1],
    ] {
        let raw = [vec![1, 0, 1, 1, b't', 1, 1, b'c', 1], cell].concat();
        assert!(stream::frozen(&packed(&raw)[..], tiny(), |_| Ok(())).is_err());
    }
    for raw in [vec![1, 1, 0, 1, 255, 0], vec![1, 0, 1, 1, 255, 0, 0], vec![1, 0, 1, 0, 1, 1, 255, 0]] {
        assert!(stream::frozen(&packed(&raw)[..], tiny(), |_| Ok(())).is_err(), "invalid UTF8 in metadata");
    }
    let mut counts = vec![1, 0, 1, 0];
    var(&mut counts, 2);
    counts.extend([0, 0]);
    var(&mut counts, u64::MAX);
    assert!(stream::frozen(&packed(&counts)[..], tiny(), |_| Ok(())).is_err());
}

struct Instrumented<R> {
    inner: R,
    calls: u64,
    bytes: usize,
    max_request: usize,
    fail_after: Option<usize>,
    shared_calls: Rc<Cell<u64>>,
}
impl<R: Read> Read for Instrumented<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.calls += 1;
        self.shared_calls.set(self.calls);
        self.max_request = self.max_request.max(out.len());
        if self.fail_after.is_some_and(|limit| self.bytes >= limit) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected read failure"));
        }
        let limit = out.len().min(1);
        let n = self.inner.read(&mut out[..limit])?;
        self.bytes += n;
        Ok(n)
    }
}
fn instrument<R>(inner: R, fail_after: Option<usize>) -> Instrumented<R> {
    Instrumented { inner, calls: 0, bytes: 0, max_request: 0, fail_after, shared_calls: Rc::new(Cell::new(0)) }
}

#[test]
fn cancellation_stops_events_and_reads_and_read_errors_keep_their_kind() {
    for (format, p) in [
        blob::encode(&[blob::Seg { part: None, bytes: 1, domain: 1, local: vec![9; 10000] }]),
        payload::encode(&fixture()).unwrap(),
    ]
    .iter()
    .enumerate()
    {
        let mut r = instrument(&p[..], None);
        let mut events = 0;
        let calls = r.shared_calls.clone();
        let mut at_cancel = 0;
        let visitor = |_: Event<'_>| {
            events += 1;
            at_cancel = calls.get();
            Err(io::Error::new(io::ErrorKind::Interrupted, "cancel"))
        };
        let result = if format == 0 {
            stream::sealed(&mut r, 0, tiny(), visitor)
        } else {
            stream::frozen(&mut r, tiny(), visitor)
        };
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert_eq!(events, 1);
        assert_eq!(r.calls, at_cancel, "no reads after visitor cancellation");
        let mut r = instrument(&p[..], Some(4));
        let result = if format == 0 {
            stream::sealed(&mut r, 0, tiny(), |_| Ok(()))
        } else {
            stream::frozen(&mut r, tiny(), |_| Ok(()))
        };
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        let mut r = instrument(&p[..], Some(p.len()));
        let result = if format == 0 {
            stream::sealed(&mut r, 0, tiny(), |_| Ok(()))
        } else {
            stream::frozen(&mut r, tiny(), |_| Ok(()))
        };
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied, "EOF verification errors propagate");
    }
}

#[test]
fn configured_expansion_window_and_buffers_are_enforced() {
    let p = payload::encode(&fixture()).unwrap();
    let size = zstd::stream::decode_all(&p[..]).unwrap().len() as u64;
    for limit in [0, size - 1] {
        assert!(stream::frozen(&p[..], Config { max_expanded_bytes: limit, ..tiny() }, |_| Ok(())).is_err());
    }
    assert_eq!(
        stream::frozen(&p[..], Config { max_expanded_bytes: size, ..tiny() }, |_| Ok(())).unwrap().expanded_bytes,
        size
    );
    for c in [
        Config { chunk_bytes: 0, ..tiny() },
        Config { read_buffer_bytes: 0, ..tiny() },
        Config { window_log_max: 0, ..tiny() },
        Config { max_expanded_bytes: (1 << 30) + 1, ..tiny() },
    ] {
        assert!(stream::frozen(&p[..], c, |_| Ok(())).is_err());
    }
    let mut r = instrument(&p[..], None);
    let s = stream::frozen(&mut r, tiny(), |_| Ok(())).unwrap();
    assert_eq!(s.compressed_bytes, p.len() as u64);
    assert_eq!(s.read_calls, r.calls);
    assert_eq!(s.max_read_request, r.max_request);
    assert!(s.max_read_request <= tiny().read_buffer_bytes);
    assert!(s.max_chunk_bytes <= tiny().chunk_bytes);
    // Configured storage only: excludes zstd's separately window-bounded internal allocation.
    assert_eq!(s.buffer_bytes, 2 * tiny().read_buffer_bytes + tiny().chunk_bytes);
    let mut raw = vec![1, 0, 1, 1, b't', 1, 1, b'c', 1, 4];
    var(&mut raw, 200_000);
    raw.resize(raw.len() + 200_000, 0);
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    encoder.window_log(20).unwrap();
    encoder.write_all(&raw).unwrap();
    let large_window = encoder.finish().unwrap();
    assert!(stream::frozen(&large_window[..], tiny(), |_| Ok(())).is_ok());
    assert!(stream::frozen(&large_window[..], Config { window_log_max: 10, ..tiny() }, |_| Ok(())).is_err());
}

#[test]
fn large_binary_text_and_local_cells_are_hashed_without_materializing_them() {
    let len = 8 * 1024 * 1024u64;
    for kind in [BytesKind::Blob, BytesKind::Text, BytesKind::Local] {
        let mut file = tempfile::tempfile().unwrap();
        let mut block = [0xabu8; 4096];
        if kind == BytesKind::Text {
            for chunk in block.as_chunks_mut::<4>().0 {
                chunk.copy_from_slice("🦀".as_bytes());
            }
        }
        let mut want = Sha256::new();
        {
            let mut e = zstd::stream::write::Encoder::new(&mut file, 3).unwrap();
            if kind == BytesKind::Local {
                e.write_all(&[1, 1, 0, 0, 0]).unwrap();
            } else {
                e.write_all(&[1, 0, 1, 1, b't', 1, 1, b'c', 1, if kind == BytesKind::Text { 3 } else { 4 }]).unwrap();
            }
            var(&mut e, len);
            for _ in 0..len / 4096 {
                e.write_all(&block).unwrap();
                want.update(block);
            }
            e.finish().unwrap();
        }
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut got = Sha256::new();
        let mut total = 0;
        let mut active = false;
        let config = Config { chunk_bytes: 127, read_buffer_bytes: 31, ..tiny() };
        let mut visitor = |event: Event<'_>| {
            match event {
                Event::BytesStart { kind: k, len: n } if k == kind => {
                    assert_eq!(n, len);
                    active = true;
                }
                Event::Chunk(b) if active => {
                    assert!(b.len() <= 127);
                    total += b.len() as u64;
                    got.update(b);
                }
                _ => {}
            }
            Ok(())
        };
        let (result, rust_peak, rust_allocations) = measure(|| {
            if kind == BytesKind::Local {
                stream::sealed(file, 0, config, &mut visitor)
            } else {
                stream::frozen(file, config, &mut visitor)
            }
        });
        let stats = result.unwrap();
        assert_eq!(total, len);
        assert_eq!(got.finalize(), want.finalize());
        assert_eq!(stats.buffer_bytes, 189);
        assert_eq!(stats.values, u64::from(kind != BytesKind::Local));
        assert!(rust_peak <= 4096, "cell allocation grew: {rust_peak}");
        eprintln!("large {kind:?} stats={stats:?} rust_peak_delta={rust_peak} rust_allocations={rust_allocations}");
    }
}

#[test]
fn legacy_nonminimal_varints_and_empty_column_rows_remain_compatible() {
    // Nonminimal zero counts are representable, unlike overflowed tenth bytes.
    let sealed = packed(&[1, 0x80, 0]);
    assert_eq!(sealed_rows(&sealed, 0).unwrap(), blob::decode(&sealed).unwrap());
    let frozen = packed(&[1, 0x80, 0, 0x80, 0]);
    assert_eq!(frozen_rows(&frozen).unwrap(), payload::decode(&frozen).unwrap());
    // The old minimum-one-byte row admission admits this empty row because
    // a second table follows; preserve that odd but decodable historical case.
    let p = payload::Payload {
        domains: vec![],
        tables: vec![
            payload::Table { name: String::new(), columns: vec![], rows: vec![vec![]] },
            payload::Table::new("", vec![]),
        ],
    };
    let frozen = payload::encode(&p).unwrap();
    assert_eq!(frozen_rows(&frozen).unwrap(), payload::decode(&frozen).unwrap());
    let invalid = packed(&[1, 0, 1, 0, 0, 1]);
    assert!(payload::decode(&invalid).is_err());
    assert!(frozen_rows(&invalid).is_err());
}

#[test]
fn quarter_million_segments_and_ten_thousand_tables_have_constant_buffers() {
    let mut baseline = None;
    for n in [1u64, 250_000] {
        let mut f = tempfile::tempfile().unwrap();
        {
            let mut e = zstd::stream::write::Encoder::new(&mut f, 3).unwrap();
            e.write_all(&[1]).unwrap();
            var(&mut e, n);
            for p in 0..n {
                var(&mut e, p + 1);
                e.write_all(&[0, 0, 1, 255]).unwrap();
            }
            e.finish().unwrap();
        }
        f.seek(SeekFrom::Start(0)).unwrap();
        let mut seen = 0;
        let (result, rust_peak, rust_allocations) = measure(|| {
            stream::sealed(f, 0, tiny(), |e| {
                if let Event::Segment { index, part, .. } = e {
                    assert_eq!(index, seen);
                    assert_eq!(part, Some(seen as i64));
                    seen += 1;
                }
                Ok(())
            })
        });
        let s = result.unwrap();
        assert!(rust_peak <= 4096, "Rust retained allocation grew: {rust_peak}");
        assert_eq!(seen, n);
        assert_eq!(s.segments, n);
        assert_eq!(*baseline.get_or_insert(s.buffer_bytes), s.buffer_bytes);
        eprintln!("sealed n={n} stats={s:?} rust_peak_delta={rust_peak} rust_allocations={rust_allocations}");
    }
    for n in [1u64, 10_000] {
        let mut f = tempfile::tempfile().unwrap();
        {
            let mut e = zstd::stream::write::Encoder::new(&mut f, 3).unwrap();
            e.write_all(&[1, 0]).unwrap();
            var(&mut e, n);
            for _ in 0..n {
                bytes(&mut e, b"files");
                e.write_all(&[1, 1, b'c', 1, 1, 2]).unwrap();
            }
            e.finish().unwrap();
        }
        f.seek(SeekFrom::Start(0)).unwrap();
        let mut seen = 0;
        let (result, rust_peak, rust_allocations) = measure(|| {
            stream::frozen(f, tiny(), |e| {
                if let Event::EndTable = e {
                    seen += 1;
                }
                Ok(())
            })
        });
        let s = result.unwrap();
        assert!(rust_peak <= 4096, "Rust retained allocation grew: {rust_peak}");
        assert_eq!(seen, n);
        assert_eq!(s.tables, n);
        assert_eq!(s.rows, n);
        assert_eq!(s.values, n);
        assert_eq!(Some(s.buffer_bytes), baseline);
        eprintln!("frozen tables={n} stats={s:?} rust_peak_delta={rust_peak} rust_allocations={rust_allocations}");
    }
}

#[test]
fn malformed_zero_column_rows_do_not_amplify_visitor_work() {
    let mut raw = vec![1, 0, 1, 0, 0];
    var(&mut raw, 100_000_000);
    let input = packed(&raw);
    assert!(payload::decode(&input).is_err());
    let mut calls = 0;
    let mut empty_rows = 0;
    let mut reader = instrument(&input[..], None);
    let result = stream::frozen(&mut reader, tiny(), |event| {
        calls += 1;
        if let Event::EmptyRows(count) = event {
            empty_rows = count;
        }
        if calls == 1000 {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "unbounded empty row events"));
        }
        Ok(())
    });
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    assert!(calls <= 12, "tiny malformed input amplified to {calls} events");
    assert_eq!(empty_rows, 100_000_000, "the provisional count is never truncated");
    assert!(reader.calls <= input.len() as u64 + 1);
    assert!(reader.max_request <= tiny().read_buffer_bytes);
    eprintln!(
        "malformed empty rows: callbacks={calls}, source_calls={}, compressed_bytes={}",
        reader.calls, reader.bytes
    );
}
