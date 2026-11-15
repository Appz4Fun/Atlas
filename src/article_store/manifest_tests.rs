//! Real-file contracts; wire fixtures are written without the production encoder.
use super::test_support::{OwnedChild, wait_marker};
use super::*;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}
fn binding() -> ReleaseBinding {
    ReleaseBinding { group: "alt.x".into(), is_collection: false, identity_key: [0x73; 32] }
}
fn setup() -> (tempfile::TempDir, PathBuf, Writer) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("payload");
    let writer = Writer::create(&root, CatalogId::from_bytes([0x42; 32])).unwrap();
    (temp, root, writer)
}
fn object(root: &Path, digest: [u8; 32]) -> PathBuf {
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    root.join("chunks").join(&hex[..2]).join(hex)
}
fn publish(writer: &mut Writer, bytes: &[u8]) -> PayloadRef {
    writer.publish(bytes.len() as u32, &mut &bytes[..], deadline()).unwrap()
}
fn field(chunk: u16, offset: u32, bytes: u32) -> FieldRef {
    FieldRef::new(bytes as u64, if bytes == 0 { vec![] } else { vec![Extent { chunk_index: chunk, offset, bytes }] })
        .unwrap()
}
fn batch(writer: &mut Writer, data: Vec<PayloadRef>, rows: Vec<AcceptedRowV1>) -> BatchRef {
    let mut builder = BatchManifestBuilder::new(writer, binding(), data, deadline()).expect("admit bounded batch");
    for row in rows {
        builder.push(row).expect("record accepted row");
    }
    builder.finish().expect("publish durable root after pages and data")
}
fn bytes(snapshot: &mut BatchSnapshot<'_>, field: &FieldRef) -> Vec<u8> {
    let mut out = vec![0; field.logical_bytes as usize];
    assert_eq!(snapshot.read_field_range(field, 0, &mut out, deadline()).unwrap(), out.len());
    out
}
fn open<'a>(reader: &'a Reader, reference: &BatchRef) -> BatchSnapshot<'a> {
    BatchSnapshot::open(reader, reference, &binding(), deadline()).expect("open exact batch binding")
}

// Missing framing, reordered fields, or a sequence-number generation breaks this literal vector.
#[test]
fn empty_manifest_matches_independent_golden_wire() {
    let (_temp, root, mut writer) = setup();
    let reference = batch(&mut writer, vec![], vec![]);
    let mut golden = b"ATAMF001\0".to_vec();
    golden.extend_from_slice(&reference.identity.to_bytes());
    golden.extend_from_slice(b"\x05\x00alt.x\x00");
    golden.extend_from_slice(&[0x73; 32]);
    golden.extend_from_slice(&[0; 8]); // u32 rows, u16 data refs, u16 pages.
    assert_eq!(fs::read(object(&root, reference.root.digest)).unwrap(), golden);
    let expected: [u8; 32] = Sha256::digest(&golden).into();
    assert_eq!(reference.generation, Generation(expected));
    let reader = Reader::open(&root, reference.identity).unwrap();
    assert!(open(&reader, &reference).next_row().unwrap().is_none());
}

// Collapsing None/zero/empty, dropping copies, normalizing opaque bytes, or sorting rows loses facts.
#[test]
fn normalized_rows_preserve_bytes_nulls_copies_and_order() {
    let (_temp, root, mut writer) = setup();
    let raw = b"<one>\0\xffraw\0name<two>canonical";
    let data = publish(&mut writer, raw);
    let mut row = AcceptedRowV1 {
        ordinal: 91,
        filename_key: Some([1; 32]),
        message_key: Some([2; 32]),
        part: Some(7),
        expected: Some(20),
        file_total: Some(0),
        file_index: Some(-1),
        bytes: -9,
        posted_at: i64::MIN,
        zero_file_index: true,
        posted_clock: Some(0),
        t_date: Some(i64::MIN),
        t_utc: Some(i64::MAX),
        t_number: Some(0),
        t_lines: Some(-1),
        t_references: Some(2),
        t_xref: Some(3),
        t_from: Some(-4),
        t_encoded: Some(5),
        t_tool: Some(6),
        t_clock: Some(i64::MAX),
        t_hw_n: Some(-7),
        original_name_key: [3; 32],
        is_obfuscated: true,
        junk: true,
        nzb: Some(NzbEnvelopeV1 { timestamp: -8, file_index: Some(0) }),
        ..Default::default()
    };
    for tag in [
        FieldTag::MessageId,
        FieldTag::Subject,
        FieldTag::Poster,
        FieldTag::PostedDate,
        FieldTag::Name,
        FieldTag::OriginalName,
        FieldTag::TimingHost,
        FieldTag::TimingKey,
        FieldTag::NzbSubject,
        FieldTag::NzbPoster,
        FieldTag::NzbPostedDate,
    ] {
        row.fields[tag.index()] = Some(field(0, 0, 5));
    }
    row.fields[FieldTag::OriginalFilename.index()] = Some(field(0, 5, 10));
    row.fields[FieldTag::Filename.index()] = Some(field(0, 20, 9));
    row.fields[FieldTag::DisplayName.index()] = Some(field(0, 0, 0));
    let mut copy = row.clone();
    copy.ordinal = -3;
    copy.fields[FieldTag::MessageId.index()] = Some(field(0, 15, 5));
    copy.fields[FieldTag::OriginalFilename.index()] = Some(field(0, 6, 9));
    let mut zero = AcceptedRowV1 { ordinal: 0, part: Some(0), ..Default::default() };
    zero.fields[FieldTag::MessageId.index()] = Some(field(0, 0, 5));
    let rows = vec![row, copy, zero, AcceptedRowV1::default()];
    let reference = batch(&mut writer, vec![data], rows.clone());
    let reader = Reader::open(&root, reference.identity).unwrap();
    let mut snapshot = open(&reader, &reference);
    for expected in rows {
        let actual = snapshot.next_row().unwrap().expect("every supplied copy survives");
        assert_eq!(actual.ordinal, expected.ordinal);
        assert_eq!(actual.filename_key, expected.filename_key);
        assert_eq!(actual.message_key, expected.message_key);
        assert_eq!(
            (actual.part, actual.expected, actual.file_total, actual.file_index),
            (expected.part, expected.expected, expected.file_total, expected.file_index)
        );
        assert_eq!(
            (actual.bytes, actual.posted_at, actual.posted_clock, actual.zero_file_index),
            (expected.bytes, expected.posted_at, expected.posted_clock, expected.zero_file_index)
        );
        assert_eq!(
            (
                actual.t_date,
                actual.t_utc,
                actual.t_number,
                actual.t_lines,
                actual.t_references,
                actual.t_xref,
                actual.t_from,
                actual.t_encoded,
                actual.t_tool,
                actual.t_clock,
                actual.t_hw_n
            ),
            (
                expected.t_date,
                expected.t_utc,
                expected.t_number,
                expected.t_lines,
                expected.t_references,
                expected.t_xref,
                expected.t_from,
                expected.t_encoded,
                expected.t_tool,
                expected.t_clock,
                expected.t_hw_n
            )
        );
        assert_eq!(
            (actual.original_name_key, actual.is_obfuscated, actual.junk, &actual.nzb),
            (expected.original_name_key, expected.is_obfuscated, expected.junk, &expected.nzb)
        );
        for (index, wanted) in expected.fields.iter().enumerate() {
            match wanted {
                None => assert!(actual.fields[index].is_none()),
                Some(wanted) => {
                    let got = actual.fields[index].as_ref().expect("present field including empty");
                    let e = wanted.extents.first();
                    let expected = e.map_or(&b""[..], |e| &raw[e.offset as usize..(e.offset + e.bytes) as usize]);
                    assert_eq!(bytes(&mut snapshot, got), expected);
                }
            }
        }
    }
    assert!(snapshot.next_row().unwrap().is_none());
}

fn pattern(offset: u64) -> u8 {
    ((offset.wrapping_mul(37) ^ (offset >> 9)) & 255) as u8
}
struct Generated {
    offset: u64,
    left: usize,
}
impl Read for Generated {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = out.len().min(self.left);
        for (i, byte) in out[..n].iter_mut().enumerate() {
            *byte = pattern(self.offset + i as u64);
        }
        self.offset += n as u64;
        self.left -= n;
        Ok(n)
    }
}
// Whole-field materialization, incorrect cross-chunk offsets, or lost envelope presence fails incremental comparison.
#[test]
fn long_envelope_crosses_chunks() {
    let (_temp, root, mut writer) = setup();
    let lengths = [MAX_CHUNK_BYTES, MAX_CHUNK_BYTES, MAX_CHUNK_BYTES, 173];
    let mut data = vec![];
    let mut offset = 0;
    let mut extents = vec![];
    for (index, length) in lengths.into_iter().enumerate() {
        data.push(writer.publish(length, &mut Generated { offset, left: length as usize }, deadline()).unwrap());
        extents.push(Extent { chunk_index: index as u16, offset: 0, bytes: length });
        offset += length as u64;
    }
    let mut row = AcceptedRowV1 { nzb: Some(NzbEnvelopeV1 { timestamp: 0, file_index: None }), ..Default::default() };
    row.fields[FieldTag::NzbSubject.index()] = Some(FieldRef::new(offset, extents).unwrap());
    row.fields[FieldTag::NzbPoster.index()] = Some(field(0, 0, 0));
    let reference = batch(&mut writer, data, vec![row]);
    let reader = Reader::open(&root, reference.identity).unwrap();
    let mut snapshot = open(&reader, &reference);
    let row = snapshot.next_row().unwrap().unwrap();
    assert_eq!(row.nzb, Some(NzbEnvelopeV1 { timestamp: 0, file_index: None }));
    assert!(row.fields[FieldTag::NzbPostedDate.index()].is_none());
    let field = row.fields[FieldTag::NzbSubject.index()].as_ref().unwrap();
    let mut out = [0; MAX_RANGE_BYTES];
    let mut at = 0;
    while at < offset {
        let n = snapshot.read_field_range(field, at, &mut out, deadline()).unwrap();
        assert!(n > 0);
        for (i, byte) in out[..n].iter().enumerate() {
            assert_eq!(*byte, pattern(at + i as u64));
        }
        at += n as u64;
    }
    assert_eq!(snapshot.read_field_range(field, at, &mut out, deadline()).unwrap(), 0);
}

// Canonical sorting must remap supplied indexes, deduplicate exact refs, and reject conflicting lengths.
#[test]
fn canonical_generations_are_immutable() {
    let (_temp, root, mut writer) = setup();
    let a = publish(&mut writer, b"alpha");
    let b = publish(&mut writer, b"beta");
    let mut row = AcceptedRowV1::default();
    row.fields[0] = Some(field(0, 0, 5));
    row.fields[1] = Some(field(1, 0, 4));
    let first = batch(&mut writer, vec![a, b], vec![row.clone()]);
    row.fields[0] = Some(field(2, 0, 5));
    row.fields[1] = Some(field(0, 0, 4));
    let reordered = batch(&mut writer, vec![b, a, a], vec![row.clone()]);
    assert_eq!(first, reordered);
    row.ordinal = 1;
    let changed = batch(&mut writer, vec![b, a, a], vec![row]);
    assert_ne!(first.generation, changed.generation);
    let conflicting = PayloadRef { bytes: a.bytes + 1, ..a };
    assert!(matches!(
        BatchManifestBuilder::new(&mut writer, binding(), vec![a, conflicting], deadline()),
        Err(Error::Corrupt)
    ));
    let reader = Reader::open(&root, first.identity).unwrap();
    let mut snapshot = open(&reader, &first);
    let row = snapshot.next_row().unwrap().unwrap();
    assert_eq!(bytes(&mut snapshot, row.fields[0].as_ref().unwrap()), b"alpha");
    assert_eq!(bytes(&mut snapshot, row.fields[1].as_ref().unwrap()), b"beta");
    assert_eq!(row.ordinal, 0);
}

// Over-budget/foreign extents may never become truncated healthy batches.
#[test]
fn manifest_rejects_binding_extent_and_budget_errors() {
    let (_temp, root, mut writer) = setup();
    let data = publish(&mut writer, b"abc");
    let good = batch(&mut writer, vec![data], vec![AcceptedRowV1::default()]);
    let reader = Reader::open(&root, good.identity).unwrap();
    let mut wrong = binding();
    wrong.identity_key[0] ^= 1;
    assert!(matches!(BatchSnapshot::open(&reader, &good, &wrong, deadline()), Err(Error::IdentityMismatch)));
    wrong = binding();
    wrong.group.push('z');
    assert!(matches!(BatchSnapshot::open(&reader, &good, &wrong, deadline()), Err(Error::IdentityMismatch)));
    wrong = binding();
    wrong.is_collection = true;
    assert!(matches!(BatchSnapshot::open(&reader, &good, &wrong, deadline()), Err(Error::IdentityMismatch)));
    let mut huge = binding();
    huge.group = "x".repeat(4097);
    assert!(matches!(BatchManifestBuilder::new(&mut writer, huge, vec![], deadline()), Err(Error::Limit)));
    assert!(matches!(
        BatchManifestBuilder::new(&mut writer, binding(), vec![data; 257], deadline()),
        Err(Error::Limit)
    ));
    for extent in [
        Extent { chunk_index: 1, offset: 0, bytes: 1 },
        Extent { chunk_index: 0, offset: 3, bytes: 1 },
        Extent { chunk_index: 0, offset: u32::MAX, bytes: 2 },
    ] {
        let mut row = AcceptedRowV1::default();
        row.fields[0] = Some(FieldRef::new(extent.bytes as u64, vec![extent]).unwrap());
        let mut builder = BatchManifestBuilder::new(&mut writer, binding(), vec![data], deadline()).unwrap();
        assert!(matches!(builder.push(row), Err(Error::Corrupt | Error::Limit)));
    }
    assert!(matches!(FieldRef::new(64 * 1024 * 1024 + 1, vec![]), Err(Error::Limit)));
    assert!(matches!(FieldRef::new(u64::MAX, vec![]), Err(Error::Limit)));
    assert!(matches!(
        FieldRef::new(1025, vec![Extent { chunk_index: 0, offset: 0, bytes: 1 }; 1025]),
        Err(Error::Limit)
    ));
    let foreign =
        PayloadRef { identity: Identity { catalog: CatalogId::from_bytes([9; 32]), ..data.identity }, ..data };
    assert!(matches!(
        BatchManifestBuilder::new(&mut writer, binding(), vec![foreign], deadline()),
        Err(Error::IdentityMismatch)
    ));
}

// Exact admission limits are batch limits; cap+1 must fail before publishing an acknowledged root.
#[test]
fn row_extent_and_logical_caps_are_checked_before_growth() {
    let (_temp, _root, mut writer) = setup();
    let data = publish(&mut writer, &[0; 1024]);
    let mut builder = BatchManifestBuilder::new(&mut writer, binding(), vec![data], deadline()).unwrap();
    for i in 0..16_384 {
        builder.push(AcceptedRowV1 { ordinal: i, ..Default::default() }).unwrap();
    }
    assert!(matches!(builder.push(AcceptedRowV1::default()), Err(Error::Limit)));
    drop(builder);
    let mut row = AcceptedRowV1::default();
    for slot in &mut row.fields[..2] {
        *slot = Some(FieldRef::new(513, vec![Extent { chunk_index: 0, offset: 0, bytes: 1 }; 513]).unwrap());
    }
    let mut builder = BatchManifestBuilder::new(&mut writer, binding(), vec![data], deadline()).unwrap();
    assert!(matches!(builder.push(row), Err(Error::Limit)));
    drop(builder);
    let big = writer.publish(MAX_CHUNK_BYTES, &mut io::repeat(0).take(MAX_CHUNK_BYTES as u64), deadline()).unwrap();
    let extents = vec![Extent { chunk_index: 0, offset: 0, bytes: MAX_CHUNK_BYTES }; 64];
    let mut row = AcceptedRowV1::default();
    for slot in &mut row.fields[..4] {
        *slot = Some(FieldRef::new(64 * 1024 * 1024, extents.clone()).unwrap());
    }
    let mut builder = BatchManifestBuilder::new(&mut writer, binding(), vec![big], deadline()).unwrap();
    builder.push(row).unwrap(); // Exactly 256 MiB logical bytes, no whole-field allocations.
    let mut extra = AcceptedRowV1::default();
    extra.fields[0] = Some(field(0, 0, 1));
    assert!(matches!(builder.push(extra), Err(Error::Limit)));
}

// The admitted deadline belongs to the whole session, including CPU-only row access and later reads.
#[test]
fn admitted_deadlines_cannot_be_extended() {
    let (_temp, root, mut writer) = setup();
    let data = publish(&mut writer, b"abc");
    let mut row = AcceptedRowV1::default();
    row.fields[0] = Some(field(0, 0, 3));
    let reference = batch(&mut writer, vec![data], vec![row.clone()]);
    assert!(matches!(BatchManifestBuilder::new(&mut writer, binding(), vec![], Instant::now()), Err(Error::Deadline)));
    let admitted = Instant::now() + Duration::from_millis(20);
    let mut builder = BatchManifestBuilder::new(&mut writer, binding(), vec![], admitted).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    assert!(matches!(builder.push(AcceptedRowV1::default()), Err(Error::Deadline)));
    assert!(matches!(builder.finish(), Err(Error::Deadline)));
    let reader = Reader::open(&root, reference.identity).unwrap();
    let admitted = Instant::now() + Duration::from_millis(20);
    let mut snapshot = BatchSnapshot::open(&reader, &reference, &binding(), admitted).unwrap();
    let token = snapshot.next_row().unwrap().unwrap().fields[0].take().unwrap();
    std::thread::sleep(Duration::from_millis(30));
    assert!(matches!(snapshot.next_row(), Err(Error::Deadline)));
    assert!(matches!(snapshot.read_field_range(&token, 0, &mut [0; 3], deadline()), Err(Error::Deadline)));
}

// A supplied extent and a decoded token from another generation/release/store are not interchangeable.
#[test]
fn decoded_field_tokens_are_bound_to_exact_snapshot() {
    let (_temp, root, mut writer) = setup();
    let data = publish(&mut writer, b"abc");
    let mut row = AcceptedRowV1::default();
    row.fields[0] = Some(field(0, 0, 3));
    let a = batch(&mut writer, vec![data], vec![row.clone()]);
    row.ordinal = 2;
    let b = batch(&mut writer, vec![data], vec![row]);
    let reader = Reader::open(&root, a.identity).unwrap();
    let mut sa = open(&reader, &a);
    let mut sb = open(&reader, &b);
    let token = sa.next_row().unwrap().unwrap().fields[0].take().unwrap();
    assert!(matches!(sb.read_field_range(&token, 0, &mut [0; 3], deadline()), Err(Error::IdentityMismatch)));
    assert!(matches!(sa.read_field_range(&field(0, 0, 3), 0, &mut [0; 3], deadline()), Err(Error::IdentityMismatch)));
    assert!(matches!(sa.read_field_range(&token, u64::MAX, &mut [0; 3], deadline()), Err(Error::Limit)));
    assert!(matches!(sa.read_field_range(&token, 0, &mut [0; MAX_RANGE_BYTES + 1], deadline()), Err(Error::Limit)));
}

// Decoded labels cannot stand in for confirming actual selected inode and directory durability.
#[test]
fn decoded_data_requires_actual_durability_and_preserves_poisoning() {
    let (_temp, root, mut writer) = setup();
    let reference = publish(&mut writer, b"abc");
    let decoded = PayloadRef::from_parts(reference.identity, reference.digest, reference.bytes).unwrap();
    let before = payload::test_sync_calls();
    writer.confirm_durable(decoded, deadline()).expect("flush actual inode and its required parents");
    assert!(payload::test_sync_calls() >= before + 4);
    payload::test_fail_final_sync();
    assert!(
        matches!(writer.confirm_durable(decoded, deadline()), Err(Error::Io(e)) if e.raw_os_error()==Some(libc::EIO))
    );
    assert!(matches!(writer.confirm_durable(decoded, deadline()), Err(Error::Busy)));
    assert!(matches!(BatchManifestBuilder::new(&mut writer, binding(), vec![decoded], deadline()), Err(Error::Busy)));
    drop(writer);
    let mut writer = Writer::open(&root, decoded.identity).unwrap();
    fs::write(object(&root, decoded.digest), b"xyz").unwrap();
    assert!(matches!(writer.confirm_durable(decoded, deadline()), Err(Error::Corrupt)));
}

// Child content is checked lazily; missing/corrupt data is a typed failure, never healthy empty bytes.
#[test]
fn lazy_data_reads_refuse_missing_corrupt_and_replaced_children() {
    for corrupt in [false, true] {
        let (_temp, root, mut writer) = setup();
        let data = publish(&mut writer, b"abc");
        let mut row = AcceptedRowV1::default();
        row.fields[0] = Some(field(0, 0, 3));
        let reference = batch(&mut writer, vec![data], vec![row]);
        let reader = Reader::open(&root, reference.identity).unwrap();
        let mut snapshot = open(&reader, &reference);
        let token = snapshot.next_row().unwrap().unwrap().fields[0].take().unwrap();
        if corrupt {
            fs::write(object(&root, data.digest), b"xyz").unwrap();
        } else {
            fs::remove_file(object(&root, data.digest)).unwrap();
        }
        let result = snapshot.read_field_range(&token, 0, &mut [0; 3], deadline());
        if corrupt {
            assert!(matches!(result, Err(Error::Corrupt)));
        } else {
            assert!(matches!(result, Err(Error::Missing)));
        }
    }
}

// A page/data descriptor must be dropped before replacement to keep the eight-FD budget usable indefinitely.
#[test]
fn many_pages_and_data_chunks_do_not_accumulate_descriptors() {
    let (_temp, root, mut writer) = setup();
    let data: Vec<_> = (0..200u16).map(|n| publish(&mut writer, &n.to_le_bytes())).collect();
    let rows: Vec<_> = (0..2000)
        .map(|n| {
            let mut row = AcceptedRowV1 { ordinal: n, ..Default::default() };
            row.fields[0] = Some(field((n % 200) as u16, 0, 2));
            row
        })
        .collect();
    let reference = batch(&mut writer, data, rows);
    let reader = Reader::open(&root, reference.identity).unwrap();
    let mut snapshot = open(&reader, &reference);
    for n in 0..2000 {
        let row = snapshot.next_row().unwrap().unwrap();
        assert_eq!(row.ordinal, n);
        assert_eq!(bytes(&mut snapshot, row.fields[0].as_ref().unwrap()), ((n % 200) as u16).to_le_bytes());
    }
    assert!(snapshot.next_row().unwrap().is_none());
}

fn replace_root(writer: &mut Writer, reference: &BatchRef, wire: &[u8]) -> BatchRef {
    let root = publish(writer, wire);
    BatchRef { generation: Generation(root.digest), root, ..reference.clone() }
}
// Rehashed malformed framing must fail in the manifest parser, independently of payload hashing.
#[test]
fn root_framing_rejects_reserved_trailing_counts_and_binding() {
    let (_temp, root, mut writer) = setup();
    let reference = batch(&mut writer, vec![], vec![]);
    let wire = fs::read(object(&root, reference.root.digest)).unwrap();
    let reader = Reader::open(&root, reference.identity).unwrap();
    let mut vectors = vec![];
    let mut v = wire.clone();
    v[0] ^= 1;
    vectors.push(v);
    let mut v = wire.clone();
    v[8] = 1;
    vectors.push(v);
    let mut v = wire.clone();
    v.push(0);
    vectors.push(v);
    vectors.push(wire[..wire.len() - 1].to_vec());
    let mut v = wire.clone();
    let at = v.len() - 4;
    v[at..at + 2].copy_from_slice(&257u16.to_le_bytes());
    vectors.push(v);
    let mut v = wire.clone();
    let at = v.len() - 2;
    v[at..].copy_from_slice(&257u16.to_le_bytes());
    vectors.push(v);
    let mut v = wire.clone();
    let at = v.len() - 8;
    v[at..at + 4].copy_from_slice(&16_385u32.to_le_bytes());
    vectors.push(v);
    for wire in vectors {
        let bad = replace_root(&mut writer, &reference, &wire);
        assert!(matches!(
            BatchSnapshot::open(&reader, &bad, &binding(), deadline()),
            Err(Error::Corrupt | Error::Limit)
        ));
    }
    let mut wrong = reference.clone();
    wrong.generation.0[0] ^= 1;
    assert!(matches!(BatchSnapshot::open(&reader, &wrong, &binding(), deadline()), Err(Error::Corrupt)));
    let mut wrong = reference.clone();
    wrong.binding.group = "wrong".into();
    assert!(matches!(BatchSnapshot::open(&reader, &wrong, &wrong.binding, deadline()), Err(Error::IdentityMismatch)));
}

// Page corruption and absence are checked on first row access, without exposing a partial descriptor.
#[test]
fn page_framing_and_missing_pages_fail_closed() {
    for mode in 0..5 {
        let (_temp, root, mut writer) = setup();
        let reference = batch(&mut writer, vec![], vec![AcceptedRowV1::default()]);
        let mut wire = fs::read(object(&root, reference.root.digest)).unwrap();
        let page_at = wire.len() - 40; // v1 page ref: digest32, bytes4, rows4.
        let digest: [u8; 32] = wire[page_at..page_at + 32].try_into().unwrap();
        let path = object(&root, digest);
        let mut page = fs::read(&path).unwrap();
        let bad = if mode == 0 {
            fs::remove_file(path).unwrap();
            reference.clone()
        } else {
            match mode {
                1 => page[8] = 1,
                2 => page.push(0),
                3 => page.truncate(page.len() - 1),
                4 => {
                    page[13..17].copy_from_slice(&u32::MAX.to_le_bytes());
                }
                _ => unreachable!(),
            }
            let child = publish(&mut writer, &page);
            wire[page_at..page_at + 32].copy_from_slice(&child.digest);
            wire[page_at + 32..page_at + 36].copy_from_slice(&child.bytes.to_le_bytes());
            replace_root(&mut writer, &reference, &wire)
        };
        let reader = Reader::open(&root, bad.identity).unwrap();
        let mut snapshot = open(&reader, &bad);
        let result = snapshot.next_row();
        if mode == 0 {
            assert!(matches!(result, Err(Error::Missing)));
        } else {
            assert!(matches!(result, Err(Error::Corrupt | Error::Limit)));
        }
    }
}

// Root acknowledgment must not precede child durability; page-only crashes leave no acknowledged batch.
#[test]
fn manifest_crash_boundaries_publish_root_last() {
    for boundary in ["manifest-page-durable", "manifest-root-before-publish", "manifest-root-durable"] {
        let (temp, root, writer) = setup();
        let identity = writer.identity();
        drop(writer);
        let marker = temp.path().join("boundary");
        let mut child = OwnedChild(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "article_store::manifest_tests::subprocess_entry", "--nocapture"])
                .env("ATLAS_MANIFEST_TEST_MODE", boundary)
                .env("ATLAS_MANIFEST_TEST_ROOT", &root)
                .env("ATLAS_MANIFEST_TEST_MARKER", &marker)
                .env(
                    "ATLAS_MANIFEST_TEST_IDENTITY",
                    identity.to_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>(),
                )
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        wait_marker(&mut child, &marker, boundary.as_bytes());
        assert!(!marker.with_extension("returned").exists());
        child.kill().unwrap();
        child.wait().unwrap();
        let mut writer = Writer::open(&root, identity).unwrap();
        let reference = batch(&mut writer, vec![], vec![AcceptedRowV1::default()]);
        let reader = Reader::open(&root, identity).unwrap();
        assert!(open(&reader, &reference).next_row().unwrap().is_some());
    }
}

// Sync uncertainty must prevent any subsequent root acknowledgment using the poisoned writer.
#[test]
fn manifest_sync_failure_prevents_root_acknowledgment() {
    let (_temp, _root, mut writer) = setup();
    let data = publish(&mut writer, b"abc");
    let mut builder = BatchManifestBuilder::new(&mut writer, binding(), vec![data], deadline()).unwrap();
    builder.push(AcceptedRowV1::default()).unwrap();
    payload::test_fail_final_sync();
    assert!(matches!(builder.finish(), Err(Error::Io(e)) if e.raw_os_error()==Some(libc::EIO)));
    assert!(matches!(writer.confirm_durable(data, deadline()), Err(Error::Busy)));
}

#[test]
fn subprocess_entry() {
    let Ok(_) = std::env::var("ATLAS_MANIFEST_TEST_MODE") else {
        return;
    };
    let root = PathBuf::from(std::env::var_os("ATLAS_MANIFEST_TEST_ROOT").unwrap());
    let marker = PathBuf::from(std::env::var_os("ATLAS_MANIFEST_TEST_MARKER").unwrap());
    let encoded = std::env::var("ATLAS_MANIFEST_TEST_IDENTITY").unwrap();
    let mut identity = [0; 48];
    for (i, b) in identity.iter_mut().enumerate() {
        *b = u8::from_str_radix(&encoded[i * 2..i * 2 + 2], 16).unwrap();
    }
    let mut writer = Writer::open(&root, Identity::from_bytes(identity)).unwrap();
    let _ = batch(&mut writer, vec![], vec![AcceptedRowV1::default()]);
    fs::write(marker.with_extension("returned"), b"returned").unwrap();
}
