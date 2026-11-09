//! Version-1 blocks still read. `tests/data/v1-block.nxv` was written by the version-1 writer before version 2
//! existed; every row of it comes back by row, by article number, by message-id and through the cursor, cold too.

use nntp_xover_engine::container::{
    AlignedBytes, BlockMeta, NxvBlockBuilder, NxvBlockView, VERSION_1, msgid_field_pub,
};
use nntp_xover_engine::corpus::{Rng, generate};
use nntp_xover_engine::*;

const FIXTURE: &[u8] = include_bytes!("data/v1-block.nxv");
const FIXTURE_META: BlockMeta = BlockMeta { server_id: 3, slab_gen: 77 };

fn token(r: &mut Rng, alphabet: &[u8], n: usize) -> String {
    (0..n).map(|_| alphabet[r.below(alphabet.len() as u64) as usize] as char).collect()
}

/// The lines the fixture holds: the corpus, obfuscated rows (random-token subjects, posters and domains), and odd
/// lines that are kept literal.
pub fn fixture_lines() -> Vec<Vec<u8>> {
    const B64: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    const HEX: &[u8] = b"0123456789abcdef";
    let mut lines = generate(1200, 0x5eed);
    let mut r = Rng::new(17);
    let mut art = 90_000_000u64;
    for i in 0..400 {
        art += 1 + r.below(40);
        let n = 32 + r.below(30) as usize;
        let subject = if i % 3 == 0 { token(&mut r, HEX, 32) } else { token(&mut r, B64, n) };
        let (a, c, d) = (14 + r.below(5) as usize, 5 + r.below(6) as usize, 2 + r.below(2) as usize);
        let poster = format!(
            "{} <{}@{}.{}>",
            token(&mut r, B64, a),
            token(&mut r, B64, 14),
            token(&mut r, B64, c),
            token(&mut r, B64, d)
        );
        let n = 32 + r.below(10) as usize;
        let local = token(&mut r, B64, n);
        let msgid = format!("<{local}@{}.{}>", token(&mut r, B64, 8), token(&mut r, B64, 2));
        let subject = if i % 7 == 0 { local } else { subject };
        let bytes = 739_000 + r.below(2000);
        lines.push(
            format!(
                "{art}\t{subject}\t{poster}\tSun, 08 Nov 2026 06:{:02}:{:02} GMT\t{msgid}\t\t{bytes}\t{}\tXref: news.example.com alt.binaries.bloaf:{art}",
                r.below(60),
                r.below(60),
                bytes / 130
            )
            .into_bytes(),
        );
    }
    lines.extend([
        b"1\tsubj\tauthor\tnot a date\t<a@b>\t\t10\t2".to_vec(),
        b"2\tsubj\tauthor\tSat, 07 Nov 2026 12:00:00 +0000\tno-brackets\t\t10\t2".to_vec(),
        b"4\tshort line".to_vec(),
        b"9\t\xff\xfebinary\t\xe9\tSat, 07 Nov 2026 12:00:00 +0000\t<\x01x@c>\t\t-3\t-0".to_vec(),
    ]);
    lines
}

fn art_of(l: &[u8]) -> u64 {
    std::str::from_utf8(l.split(|&b| b == b'\t').next().unwrap()).unwrap().parse().unwrap_or(0)
}

/// The fixture's lines in block order (a stable sort by article number).
fn sorted_lines() -> Vec<Vec<u8>> {
    let mut l = fixture_lines();
    l.sort_by_key(|x| art_of(x));
    l
}

/// Writes the fixture again. Run by hand (`cargo test --test format_v1 -- --ignored`) only when the fixture's lines
/// change; `the_v1_writer_still_writes_the_fixture` checks the writer reproduces it byte for byte.
#[test]
#[ignore]
fn write_v1_fixture() {
    let lines = fixture_lines();
    let refs: Vec<&[u8]> = lines.iter().map(|l| l.as_slice()).collect();
    let a = NxvBlockBuilder::build_hot_block_version(&refs, FIXTURE_META, VERSION_1).unwrap();
    let b = NxvBlockBuilder::build_hot_block_version(&refs, FIXTURE_META, VERSION_1).unwrap();
    assert_eq!(a, b, "the writer is deterministic");
    std::fs::write(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/v1-block.nxv"), &a).unwrap();
}

#[test]
fn the_v1_writer_still_writes_the_fixture() {
    let lines = fixture_lines();
    let refs: Vec<&[u8]> = lines.iter().map(|l| l.as_slice()).collect();
    let again = NxvBlockBuilder::build_hot_block_version(&refs, FIXTURE_META, VERSION_1).unwrap();
    assert!(again == FIXTURE, "the version-1 writer no longer writes what it did");
}

#[test]
fn a_v1_block_reads_every_way() {
    let lines = sorted_lines();
    let block = AlignedBytes::from_slice(FIXTURE);
    let view = NxvBlockView::from_mmap_slice(&block).unwrap();
    assert_eq!(view.header.version, 1);
    assert_eq!(view.row_count(), lines.len());
    assert_eq!(view.store_meta(), FIXTURE_META);
    assert!(view.verify_body());
    assert!(view.poster_count() > 400);
    let mut cache = RowCache::default();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        out.clear();
        view.reconstruct_row(i, &mut cache, &mut out).unwrap();
        assert_eq!(&out, l, "row {i}");
    }
    let mut r = Rng::new(9);
    for _ in 0..2000 {
        let i = r.below(lines.len() as u64) as usize;
        out.clear();
        view.reconstruct_row(i, &mut cache, &mut out).unwrap();
        assert_eq!(&out, &lines[i], "row {i}");
    }
    let mut scratch = XoverStreamScratch::new();
    let mut cur = NxvRangeCursor::new_for_art_range(view, 0, u64::MAX).unwrap();
    let mut i = 0;
    while cur.format_next_row(&mut scratch) {
        assert_eq!(scratch.wire_buf.strip_suffix(b"\r\n").unwrap(), lines[i].as_slice(), "row {i}");
        scratch.wire_buf.clear();
        i += 1;
    }
    assert_eq!(i, lines.len());
    for (i, l) in lines.iter().enumerate() {
        let a = art_of(l);
        let first = lines.iter().position(|x| art_of(x) == a).unwrap();
        assert_eq!(view.find_row_by_art_num(a), Some(first), "row {i}");
        if let Some(m) = msgid_field_pub(l) {
            let want = lines.iter().position(|x| msgid_field_pub(x) == Some(m)).unwrap();
            assert_eq!(view.find_row_by_msgid_with(m, &mut cache), Some(want), "row {i}");
        }
    }
    let cold = AlignedBytes::from_slice(&compress_block_to_cold(&block, 3).unwrap());
    let cv = NxvBlockView::from_mmap_slice(&cold).unwrap();
    assert!(cv.is_cold());
    assert_eq!(cv.header.version, 1);
    assert_eq!(cv.store_meta(), FIXTURE_META);
    let back = decompress_cold_block(&cold).unwrap();
    assert_eq!(&back[..], FIXTURE);
}
