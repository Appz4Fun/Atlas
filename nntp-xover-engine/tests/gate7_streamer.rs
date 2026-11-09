//! Gate 7: the whole corpus streams through the Tokio sink byte for byte without the wire buffer growing.

use nntp_xover_engine::corpus::generate_realistic_xover_corpus;
use nntp_xover_engine::*;

#[test]
fn stream_everything_without_growing_the_buffer() {
    let lines = generate_realistic_xover_corpus(10_000);
    let refs: Vec<&[u8]> = lines.iter().map(|l| l.as_slice()).collect();
    let block = AlignedBytes::from_slice(&NxvBlockBuilder::build_hot_block(&refs).unwrap());
    let view = NxvBlockView::from_mmap_slice(&block).unwrap();
    let mut scratch = XoverStreamScratch::new();
    assert_eq!(scratch.wire_buf.capacity(), 65_536);
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let mut sink: Vec<u8> = Vec::new();
    let sent = rt.block_on(stream_xover_range_tokio(view, 0, u64::MAX, &mut scratch, &mut sink)).unwrap();
    assert_eq!(sent, 10_000);
    assert_eq!(scratch.wire_buf.capacity(), 65_536, "the wire buffer reallocated");

    let body = sink.strip_prefix(b"224 Overview information follows\r\n").expect("status line");
    let body = body.strip_suffix(b".\r\n").expect("terminator");
    let got: Vec<&[u8]> = body.split(|&b| b == b'\n').map(|l| l.strip_suffix(b"\r").unwrap_or(l)).collect();
    // split leaves one empty piece after the last CRLF
    assert_eq!(got.len(), 10_001);
    assert!(got[10_000].is_empty());
    for (i, l) in lines.iter().enumerate() {
        assert_eq!(got[i], l.as_slice(), "row {i}");
    }
    // three posters are active at once and one row in a hundred is a discussion post, soo a row's template is
    // the previous row's about four times in five, not the 99% a single poster's run would give
    let hits = scratch.cache.tpl_hits as f64 / 10_000.0;
    assert!(hits > 0.7, "template cache hit rate {hits:.3}");
    println!("template cache hit rate {hits:.3}");
}

#[test]
fn a_range_in_the_middle() {
    let lines = generate_realistic_xover_corpus(3_000);
    let refs: Vec<&[u8]> = lines.iter().map(|l| l.as_slice()).collect();
    let block = AlignedBytes::from_slice(&NxvBlockBuilder::build_hot_block(&refs).unwrap());
    let view = NxvBlockView::from_mmap_slice(&block).unwrap();
    let art =
        |l: &[u8]| -> u64 { std::str::from_utf8(l.split(|&b| b == b'\t').next().unwrap()).unwrap().parse().unwrap() };
    let (a, b) = (art(&lines[1000]), art(&lines[1500]));
    let mut scratch = XoverStreamScratch::new();
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let mut sink: Vec<u8> = Vec::new();
    let sent = rt.block_on(stream_xover_range_tokio(view, a, b, &mut scratch, &mut sink)).unwrap();
    assert_eq!(sent, 501);
    let mut want = b"224 Overview information follows\r\n".to_vec();
    for l in &lines[1000..=1500] {
        want.extend_from_slice(l);
        want.extend_from_slice(b"\r\n");
    }
    want.extend_from_slice(b".\r\n");
    assert_eq!(sink, want);
}
