//! Gate 0: the corpus is the right size and shape, and the same every time.

use nntp_xover_engine::corpus::generate_realistic_xover_corpus;

#[test]
fn ten_thousand_lines_of_nine_fields() {
    let lines = generate_realistic_xover_corpus(10_000);
    assert_eq!(lines.len(), 10_000);
    for (i, l) in lines.iter().enumerate() {
        assert_eq!(l.iter().filter(|&&b| b == b'\t').count(), 8, "row {i}: {}", String::from_utf8_lossy(l));
        assert!(!l.contains(&b'\r') && !l.contains(&b'\n'), "row {i} has a line ending");
        assert!(!l.is_empty());
    }
}

#[test]
fn two_calls_give_the_same_bytes() {
    assert_eq!(generate_realistic_xover_corpus(10_000), generate_realistic_xover_corpus(10_000));
    // and a prefix of a longer run is the same run
    assert_eq!(generate_realistic_xover_corpus(500), generate_realistic_xover_corpus(10_000)[..500].to_vec());
}

#[test]
fn the_shapes_the_plan_asks_for_are_all_there() {
    let lines = generate_realistic_xover_corpus(10_000);
    let field = |l: &[u8], i: usize| l.split(|&b| b == b'\t').nth(i).unwrap().to_vec();
    let mut hex = 0;
    let mut b64 = 0;
    let mut raw = 0;
    let mut slot0_differs = 0;
    let mut plain = 0;
    let mut multi_bracket = 0;
    let mut pads = std::collections::BTreeSet::new();
    for l in &lines {
        let m = field(l, 4);
        let (_, tail, _) = nntp_xover_engine::msgid_codec::split_msgid_parts(&m);
        match nntp_xover_engine::msgid_codec::classify_tail(tail).0 {
            nntp_xover_engine::msgid_codec::PackMode::Raw8Bit => raw += 1,
            nntp_xover_engine::msgid_codec::PackMode::Base64Usenet => b64 += 1,
            _ => hex += 1,
        }
        let art = field(l, 0);
        let xref = field(l, 8);
        let first = xref.split(|&b| b == b':').nth(2).unwrap().split(|&b| b == b' ').next().unwrap().to_vec();
        if first != art {
            slot0_differs += 1;
        }
        let s = field(l, 1);
        if !s.contains(&b'(') && !s.contains(&b'[') {
            plain += 1;
        }
        if s.iter().filter(|&&b| b == b'[').count() > 2 {
            multi_bracket += 1;
        }
        if let Some(p) = s.iter().position(|&b| b == b'(') {
            let w = s[p + 1..].iter().take_while(|b| b.is_ascii_digit()).count();
            pads.insert(w);
        }
    }
    assert!((0.4..0.6).contains(&(hex as f64 / 10_000.0)), "hex {hex}");
    assert!((0.4..0.6).contains(&(b64 as f64 / 10_000.0)), "b64 {b64}");
    assert!((50..=200).contains(&raw), "raw {raw}");
    assert!((300..=700).contains(&slot0_differs), "slot 0 differs {slot0_differs}");
    assert!(plain >= 50, "plain {plain}");
    assert!(multi_bracket >= 500, "multi bracket {multi_bracket}");
    assert!(pads.len() >= 3, "paddings {pads:?}");
}
