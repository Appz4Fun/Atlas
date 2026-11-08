//! Gate 3: message-ids split, pack and come back for the whole corpus; the packed sizes are what the
//! alphabets allow; odd ids fall back losslessly.

use nntp_xover_engine::corpus::generate_realistic_xover_corpus;
use nntp_xover_engine::msgid_codec::{
    PackMode, classify_tail, pack_msgid_tail, split_msgid_parts, unpack_msgid_tail_into,
};
use nntp_xover_engine::*;

fn reassemble(raw: &[u8]) -> Vec<u8> {
    let (prefix, tail, domain) = split_msgid_parts(raw);
    let mut packed = Vec::new();
    pack_msgid_tail(tail, &mut packed);
    let mut out = Vec::new();
    out.push(b'<');
    out.extend_from_slice(prefix);
    unpack_msgid_tail_into(&packed, &mut out);
    out.push(b'@');
    out.extend_from_slice(domain);
    out.push(b'>');
    out
}

#[test]
fn every_corpus_message_id_comes_back() {
    for l in generate_realistic_xover_corpus(10_000) {
        let m = l.split(|&b| b == b'\t').nth(4).unwrap();
        assert_eq!(reassemble(m), m, "{}", String::from_utf8_lossy(m));
    }
}

#[test]
fn packed_sizes() {
    let size = |tail: &[u8]| {
        let mut p = Vec::new();
        pack_msgid_tail(tail, &mut p);
        let mut o = Vec::new();
        unpack_msgid_tail_into(&p, &mut o);
        assert_eq!(o, tail);
        p.len()
    };
    // 24 chars of hex with a dot: header, dot position, 23 digits in 12 bytes
    assert_eq!(classify_tail(b"853c49e6748fea9b.2545f49"), (PackMode::Hex4Bit, 16));
    assert_eq!(size(b"853c49e6748fea9b.2545f49"), 2 + 12);
    // 24 chars of base64: header, 18 bytes
    assert_eq!(classify_tail(b"K9xLm2P0qR8vW1yZ-9aB8cD4").0, PackMode::Base64Usenet);
    assert_eq!(size(b"K9xLm2P0qR8vW1yZ-9aB8cD4"), 1 + 18);
    assert_eq!(classify_tail(b"123$456=789").0, PackMode::Raw8Bit);
    assert_eq!(size(b"123$456=789"), 1 + 11);
    // a long raw tail: the length goes in a varint after the header
    let long: Vec<u8> = (0..120).map(|i| if i % 7 == 0 { b'$' } else { b'a' + (i % 26) as u8 }).collect();
    assert_eq!(size(&long), 1 + 1 + 120);
    let (p, t, d) = split_msgid_parts(b"<part01.853c49e6748fea9b.2545f491@nyuu.local>");
    assert_eq!((p, t, d), (&b"part01."[..], &b"853c49e6748fea9b.2545f491"[..], &b"nyuu.local"[..]));
}

#[test]
fn odd_message_ids_are_kept_whole_in_a_block() {
    let date = "Sat, 07 Nov 2026 12:00:00 +0000";
    let long_local: String = (0..120).map(|i| if i % 9 == 0 { '$' } else { (b'a' + (i % 26) as u8) as char }).collect();
    let lines: Vec<Vec<u8>> = [
        format!("1\tsubj\ta\t{date}\t<{long_local}@custom.org>\t\t10\t2"),
        format!("2\tsubj\ta\t{date}\tno-at-sign-here\t\t10\t2"),
        format!("3\tsubj\ta\t{date}\tmissing@brackets.org\t\t10\t2"),
        format!("4\tsubj\ta\t{date}\t<weird+id_123$456=789@custom.org>\t\t10\t2"),
        format!("5\tsubj\ta\t{date}\t<>\t\t10\t2"),
        format!("6\tsubj\ta\t{date}\t<@>\t\t10\t2"),
    ]
    .iter()
    .map(|s| s.as_bytes().to_vec())
    .collect();
    let refs: Vec<&[u8]> = lines.iter().map(|l| l.as_slice()).collect();
    let block = AlignedBytes::from_slice(&NxvBlockBuilder::build_hot_block(&refs).unwrap());
    let view = NxvBlockView::from_mmap_slice(&block).unwrap();
    for (i, l) in lines.iter().enumerate() {
        assert_eq!(&view.reconstruct_row_vec(i).unwrap(), l, "row {i}");
        let m = l.split(|&b| b == b'\t').nth(4).unwrap();
        assert_eq!(view.find_row_by_msgid(m), Some(i), "{}", String::from_utf8_lossy(m));
    }
    // rows 2, 3 and 5 can't be split (no brackets or no @), soo they are literal; the rest are structured
    assert_eq!(view.literal_rows(), 3);
}
