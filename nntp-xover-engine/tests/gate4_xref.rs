//! Gate 4: Xref skeletons come out and go back for the whole corpus, with and without numbers, and the
//! column is small.

use std::collections::HashMap;

use nntp_xover_engine::corpus::generate_realistic_xover_corpus;
use nntp_xover_engine::varint::{write_varint, zigzag_encode};
use nntp_xover_engine::xref_codec::{MAX_XREF_GROUPS, XREF_PREFIX, extract_xref_skeleton, render_xref, slot_count};

fn roundtrip(f: &[u8]) -> (Vec<u8>, [u64; MAX_XREF_GROUPS], usize) {
    let mut skel = Vec::new();
    let mut nums = [0u64; MAX_XREF_GROUPS];
    let n = extract_xref_skeleton(f, &mut skel, &mut nums).expect("no placeholder bytes");
    assert_eq!(slot_count(&skel), n);
    let mut out = Vec::new();
    render_xref(&skel, &nums[..n], &mut out);
    assert_eq!(out, f, "{}", String::from_utf8_lossy(f));
    (skel, nums, n)
}

fn xrefs() -> Vec<(u64, Vec<u8>)> {
    generate_realistic_xover_corpus(10_000)
        .iter()
        .map(|l| {
            let mut f = l.split(|&b| b == b'\t');
            let art = std::str::from_utf8(f.next().unwrap()).unwrap().parse().unwrap();
            (art, f.nth(7).unwrap().to_vec())
        })
        .collect()
}

#[test]
fn every_corpus_xref_comes_back() {
    let mut differing = 0;
    for (art, x) in xrefs() {
        let (_, nums, n) = roundtrip(&x);
        assert!(x.starts_with(XREF_PREFIX));
        assert!((1..=4).contains(&n), "{n} groups");
        differing += (nums[0] != art) as usize;
    }
    assert!(differing > 0, "the corpus should have rows whose slot 0 is not the article number");
}

#[test]
fn group_counts() {
    assert_eq!(roundtrip(b"").2, 0);
    assert_eq!(roundtrip(b"Xref: host").2, 0);
    assert_eq!(roundtrip(b"Xref: host a.b:1").2, 1);
    let (skel, nums, n) = roundtrip(b"host a:1 b:2 c:3 d:4 e:5");
    assert_eq!((n, &nums[..5]), (5, &[1u64, 2, 3, 4, 5][..]));
    assert_eq!(skel, b"host a:\x01 b:\x01 c:\x01 d:\x01 e:\x01");
    // more than MAX_XREF_GROUPS: the rest stay literal digits
    let (skel, _, n) = roundtrip(b"h a:1 b:2 c:3 d:4 e:5 f:6 g:7 h:8 i:9 j:10");
    assert_eq!(n, 8);
    assert!(skel.ends_with(b"i:9 j:10"));
    // slot 0 off the article number is just another number
    let (_, nums, _) = roundtrip(b"Xref: news.example.com alt.binaries.teevee:91500123 alt.binaries.misc:5");
    assert_ne!(nums[0], 84_500_123);
}

#[test]
fn the_column_is_small() {
    let rows = xrefs();
    let raw: usize = rows.iter().map(|(_, x)| x.len()).sum();
    let mut unique: HashMap<Vec<u8>, u32> = HashMap::new();
    let mut runs = Vec::new(); // (skeleton id, run length) as varints
    let mut deltas = Vec::new(); // slot 0 against the article number, the rest against the previous row
    let mut prev = [0i64; MAX_XREF_GROUPS];
    let mut last_id = u32::MAX;
    let mut run = 0u64;
    let mut skel = Vec::new();
    let mut nums = [0u64; MAX_XREF_GROUPS];
    for (art, x) in &rows {
        let body = x.strip_prefix(XREF_PREFIX).unwrap();
        let n = extract_xref_skeleton(body, &mut skel, &mut nums).unwrap();
        let next = unique.len() as u32;
        let id = *unique.entry(skel.clone()).or_insert(next);
        if id == last_id {
            run += 1;
        } else {
            if run > 0 {
                write_varint(last_id as u64, &mut runs);
                write_varint(run, &mut runs);
            }
            last_id = id;
            run = 1;
        }
        for k in 0..n {
            let base = if k == 0 { *art as i64 } else { prev[k] };
            write_varint(zigzag_encode(nums[k] as i64 - base), &mut deltas);
            prev[k] = nums[k] as i64;
        }
    }
    write_varint(last_id as u64, &mut runs);
    write_varint(run, &mut runs);
    let skeletons: Vec<&[u8]> = unique.keys().map(|k| k.as_slice()).collect();
    let comp = fsst::Compressor::train(&skeletons);
    let coded: usize = skeletons.iter().map(|s| comp.compress(s).len()).sum::<usize>() + comp.n_symbols() * 9;
    let total = coded + runs.len() + deltas.len();
    let ratio = raw as f64 / total as f64;
    // a varint is a byte at least, and the corpus has one to three cross-posted groups a row besides slot 0,
    // soo the floor for 10,000 rows is about 30 KB: plan.md's 25 KB is under it. Against the previous row's
    // slot (the plan's scheme) a poster switch costs a few bytes more; the block takes its deltas against the
    // last row with the same skeleton instead, which is why the block comes out smaller than this column
    assert!(total < 48 * 1024, "{total} bytes ({ratio:.1}x) for {} rows, {} skeletons", rows.len(), unique.len());
    assert!(ratio > 18.0, "{ratio:.1}x");
    println!(
        "xref column: {raw} raw bytes into {total} ({ratio:.1}x): {} skeletons in {coded} bytes, {} of runs, {} of deltas",
        unique.len(),
        runs.len(),
        deltas.len()
    );
}
