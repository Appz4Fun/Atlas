use nntp_xover_engine::container::msgid_field_pub as msgid_field;
use nntp_xover_engine::corpus::{Rng, generate};
use nntp_xover_engine::*;

fn build(lines: &[Vec<u8>]) -> AlignedBytes {
    let refs: Vec<&[u8]> = lines.iter().map(|l| l.as_slice()).collect();
    AlignedBytes::from_slice(&NxvBlockBuilder::build_hot_block(&refs).unwrap())
}

#[test]
fn every_row_comes_back_byte_for_byte() {
    let lines = generate(10_000, 7);
    let block = build(&lines);
    let view = NxvBlockView::from_mmap_slice(&block).unwrap();
    assert_eq!(view.row_count(), 10_000);
    // random access
    let mut cache = RowCache::default();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        out.clear();
        view.reconstruct_row(i, &mut cache, &mut out).unwrap();
        assert_eq!(&out, l, "row {i}");
    }
    // random access in random order
    let mut r = Rng::new(3);
    for _ in 0..5_000 {
        let i = r.below(10_000) as usize;
        out.clear();
        view.reconstruct_row(i, &mut cache, &mut out).unwrap();
        assert_eq!(&out, &lines[i], "row {i}");
    }
    // the streaming cursor
    let mut scratch = XoverStreamScratch::new();
    let mut cur = NxvRangeCursor::new_for_art_range(view, 0, u64::MAX).unwrap();
    let mut i = 0;
    while cur.format_next_row(&mut scratch) {
        let got = scratch.wire_buf.strip_suffix(b"\r\n").unwrap();
        assert_eq!(got, lines[i].as_slice(), "row {i}");
        scratch.wire_buf.clear();
        i += 1;
    }
    assert_eq!(i, 10_000);
    // nearly every row of a post reuses the cached template
    assert!(
        scratch.cache.tpl_hits > 3 * scratch.cache.tpl_misses,
        "{} {}",
        scratch.cache.tpl_hits,
        scratch.cache.tpl_misses
    );
}

#[test]
fn ranges_start_and_stop_on_article_numbers() {
    let lines = generate(3_000, 11);
    let block = build(&lines);
    let view = NxvBlockView::from_mmap_slice(&block).unwrap();
    let arts: Vec<u64> = lines
        .iter()
        .map(|l| std::str::from_utf8(l.split(|&b| b == b'\t').next().unwrap()).unwrap().parse().unwrap())
        .collect();
    let mut r = Rng::new(5);
    let mut scratch = XoverStreamScratch::new();
    for _ in 0..200 {
        let a = arts[0] - 5 + r.below(arts[2999] - arts[0] + 10);
        let b = a + r.below(600);
        let want: Vec<&Vec<u8>> =
            lines.iter().zip(&arts).filter(|(_, x)| **x >= a && **x <= b).map(|(l, _)| l).collect();
        let mut cur = NxvRangeCursor::new_for_art_range(view, a, b).unwrap();
        scratch.wire_buf.clear();
        let mut got = 0;
        while cur.format_next_row(&mut scratch) {
            got += 1;
        }
        let expect: Vec<u8> = want.iter().flat_map(|l| l.iter().copied().chain(*b"\r\n")).collect();
        assert_eq!(got, want.len());
        assert_eq!(scratch.wire_buf, expect, "range {a}-{b}");
    }
}

#[test]
fn message_ids_find_their_rows_and_fakes_find_nothing() {
    let lines = generate(10_000, 7);
    let block = build(&lines);
    let view = NxvBlockView::from_mmap_slice(&block).unwrap();
    let mut cache = RowCache::default();
    for (i, l) in lines.iter().enumerate() {
        let m = msgid_field(l).unwrap();
        assert_eq!(view.find_row_by_msgid_with(m, &mut cache), Some(i), "{}", String::from_utf8_lossy(m));
        assert_eq!(view.find_row_by_msgid(m), Some(i));
    }
    let mut r = Rng::new(99);
    let mut fp = 0;
    for k in 0..100_000 {
        let fake = format!("<fake-msgid-{k}-{}@news.example.com>", r.next_u64());
        if view.may_contain_msgid(fake.as_bytes()) {
            fp += 1;
        }
        assert_eq!(view.find_row_by_msgid_with(fake.as_bytes(), &mut cache), None);
    }
    let rate = fp as f64 / 100_000.0;
    assert!(rate <= 0.0045, "false positive rate {rate}");
}

#[test]
fn streaming_never_grows_the_wire_buffer() {
    let lines = generate(10_000, 7);
    let block = build(&lines);
    let view = NxvBlockView::from_mmap_slice(&block).unwrap();
    let mut scratch = XoverStreamScratch::new();
    let cap = scratch.wire_buf.capacity();
    assert_eq!(cap, 65_536);
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let mut sink = Vec::new();
    let n = rt.block_on(stream_xover_range_tokio(view, 0, u64::MAX, &mut scratch, &mut sink)).unwrap();
    assert_eq!(n, 10_000);
    assert_eq!(scratch.wire_buf.capacity(), cap);
    let mut want = b"224 Overview information follows\r\n".to_vec();
    for l in &lines {
        want.extend_from_slice(l);
        want.extend_from_slice(b"\r\n");
    }
    want.extend_from_slice(b".\r\n");
    assert_eq!(sink, want);
}

#[test]
fn cold_blocks_route_without_decompressing_and_come_back_whole() {
    let lines = generate(5_000, 21);
    let hot = build(&lines);
    let cold = AlignedBytes::from_slice(&compress_block_to_cold(&hot, 9).unwrap());
    assert!(cold.len() < hot.len());
    let cv = NxvBlockView::from_mmap_slice(&cold).unwrap();
    assert!(cv.is_cold());
    let m = msgid_field(&lines[1234]).unwrap();
    assert!(cv.may_contain_msgid(m));
    assert_eq!(cv.find_row_by_msgid(m), None);
    assert!(cv.reconstruct_row_vec(0).is_err());
    let back = decompress_cold_block(&cold).unwrap();
    assert_eq!(&back[..], &hot[..]);
    let hv = NxvBlockView::from_mmap_slice(&back).unwrap();
    assert_eq!(hv.find_row_by_msgid(m), Some(1234));
}

#[test]
fn odd_lines_are_kept_whole() {
    let mut lines: Vec<Vec<u8>> = vec![
        b"1\tsubj\tauthor\tnot a date\t<a@b>\t\t10\t2".to_vec(),
        b"2\tsubj\tauthor\tSat, 07 Nov 2026 12:00:00 +0000\tno-brackets\t\t10\t2".to_vec(),
        b"3\ts\x02ubj\ta\tSat, 07 Nov 2026 12:00:00 +0000\t<a@b>\t\t10\t2".to_vec(),
        b"4\tshort line".to_vec(),
        b"5\t[01/02] x (1/3)\ta\tSat, 07 Nov 2026 12:00:00 +0000\t<a.b@c>\t\t010\t2".to_vec(),
        b"6\tx\ta\tSat, 07 Nov 2026 12:00:00 +0000\t<a.b@c>\t\t\t".to_vec(),
        b"7\tx\ta\tSat, 07 Nov 2026 12:00:00 +0000\t<a.b@c>\t<r@s>\t10\t2\thost g:7\textra".to_vec(),
        b"0008\tx\ta\tSat, 07 Nov 2026 12:00:00 +0000\t<z@c>\t\t10\t2".to_vec(),
        b"9\t\xff\xfebinary\t\xe9\tSat, 07 Nov 2026 12:00:00 +0000\t<\x01x@c>\t\t-3\t-0".to_vec(),
        b"10\tx\ta\tSat, 07 Nov 2026 12:00:00 +0000\t<dup@c>\t\t10\t2\t".to_vec(),
        b"10\ty\ta\tSat, 07 Nov 2026 12:00:00 +0000\t<dup@c>\t\t10\t2".to_vec(),
    ];
    lines.extend(generate(300, 1));
    let block = build(&lines);
    let view = NxvBlockView::from_mmap_slice(&block).unwrap();
    // article order: the generated rows sort after these
    for (i, l) in lines.iter().enumerate() {
        assert_eq!(&view.reconstruct_row_vec(i).unwrap(), l, "row {i}");
    }
    assert_eq!(view.find_row_by_msgid(b"<dup@c>"), Some(9));
    assert_eq!(view.find_row_by_msgid(b"<z@c>"), Some(7));
}

#[test]
fn rejects_bad_input_and_misaligned_slices() {
    assert!(NxvBlockBuilder::build_hot_block(&[]).is_err());
    assert!(NxvBlockBuilder::build_hot_block(&[b"abc\tdef"]).is_err());
    assert!(NxvBlockBuilder::build_hot_block(&[b"1\ta\r\nb"]).is_err());
    let lines = generate(100, 2);
    let block = build(&lines);
    let mut shifted = vec![0u8; block.len() + 8];
    shifted[8..].copy_from_slice(&block);
    let al = AlignedBytes::from_slice(&shifted);
    assert!(NxvBlockView::from_mmap_slice(&al[8..]).is_err());
    let mut broken = block.to_vec();
    broken[100] ^= 1;
    assert!(NxvBlockView::from_mmap_slice(&AlignedBytes::from_slice(&broken)).is_err());
}

#[test]
fn half_hour_zones_come_back_across_consecutive_seconds() {
    let mut lines = Vec::new();
    for (i, s) in [
        "Sat, 07 Nov 2026 12:00:00 +0530",
        "Sat, 07 Nov 2026 12:00:01 +0530",
        "Sat, 07 Nov 2026 12:59:59 +0530",
        "Sat, 07 Nov 2026 13:00:00 +0530",
        "Sat, 07 Nov 2026 13:29:59 -0930",
        "Sat, 07 Nov 2026 13:30:00 -0930",
        "Sat, 07 Nov 2026 13:30:01 -0930",
        "Sat, 07 Nov 2026 13:30:01 -0700 (UTC)",
        "Sat, 07 Nov 2026 13:30:02 -0700 (UTC)",
        "Sat, 07 Nov 2026 13:59:59 -0700 (UTC)",
        "Sat, 07 Nov 2026 14:00:00 -0700 (UTC)",
    ]
    .iter()
    .enumerate()
    {
        lines.push(format!("{}\tx\ta\t{s}\t<m{i}@c>\t\t10\t2", i + 1).into_bytes());
    }
    let block = build(&lines);
    let view = NxvBlockView::from_mmap_slice(&block).unwrap();
    let mut scratch = XoverStreamScratch::new();
    let mut cur = NxvRangeCursor::new_for_art_range(view, 0, u64::MAX).unwrap();
    let mut i = 0;
    while cur.format_next_row(&mut scratch) {
        assert_eq!(scratch.wire_buf.strip_suffix(b"\r\n").unwrap(), lines[i].as_slice(), "row {i}");
        scratch.wire_buf.clear();
        i += 1;
    }
    assert_eq!(i, lines.len());
}

#[test]
fn equal_article_numbers_over_a_checkpoint_all_come_back() {
    let lines: Vec<Vec<u8>> = (0..300u32)
        .map(|i| {
            let n = if (5..140).contains(&i) {
                10
            } else if i < 5 {
                i as u64
            } else {
                10 + i as u64
            };
            format!("{n}\tsubj {i}\ta\tSat, 07 Nov 2026 12:00:00 +0000\t<d{i}@c>\t\t10\t2").into_bytes()
        })
        .collect();
    let block = build(&lines);
    let view = NxvBlockView::from_mmap_slice(&block).unwrap();
    let mut scratch = XoverStreamScratch::new();
    let mut cur = NxvRangeCursor::new_for_art_range(view, 10, 10).unwrap();
    let mut got = 0;
    while cur.format_next_row(&mut scratch) {
        got += 1;
    }
    assert_eq!(got, 135);
    assert_eq!(view.lower_bound_row(10).unwrap(), 5);
}

#[test]
fn blocks_with_the_same_layout_but_different_bytes_dont_share_a_cache() {
    // same lengths everywhere, one author byte apart: a cache keyed on layout alone would reuse author 0
    let mk = |author: &str| -> Vec<Vec<u8>> {
        (1..=300u64)
            .map(|n| format!("{n}\tsubj\t{author}\tSat, 07 Nov 2026 12:00:00 +0000\t<{n:08x}@c>\t\t10\t2").into_bytes())
            .collect()
    };
    let (a, b) = (mk("poster A <a@x>"), mk("poster B <a@x>"));
    let ba = build(&a);
    let bb = build(&b);
    let va = NxvBlockView::from_mmap_slice(&ba).unwrap();
    let vb = NxvBlockView::from_mmap_slice(&bb).unwrap();
    assert_ne!(va.header.header_crc32, vb.header.header_crc32, "the body hash is in the CRC'd prefix");
    assert_ne!(va.body_hash(), vb.body_hash());
    assert!(va.verify_body() && vb.verify_body());
    let mut cache = RowCache::default();
    let mut out = Vec::new();
    for (view, lines) in [(va, &a), (vb, &b), (va, &a)] {
        for i in [0usize, 150, 299] {
            out.clear();
            view.reconstruct_row(i, &mut cache, &mut out).unwrap();
            assert_eq!(&out, &lines[i]);
        }
    }
    // a cold block decompressed into a buffer another block was freed from is told apart the same way
    let ca = AlignedBytes::from_slice(&compress_block_to_cold(&ba, 3).unwrap());
    let cb = AlignedBytes::from_slice(&compress_block_to_cold(&bb, 3).unwrap());
    for (cold, lines) in [(&ca, &a), (&cb, &b), (&ca, &a)] {
        let hot = decompress_cold_block(cold).unwrap();
        let v = NxvBlockView::from_mmap_slice(&hot).unwrap();
        out.clear();
        v.reconstruct_row(7, &mut cache, &mut out).unwrap();
        assert_eq!(&out, &lines[7]);
    }
    // a body changed under a valid header fails the check
    let mut tampered = ba.to_vec();
    let at = va.header.cold_prefix_bytes as usize + 100;
    tampered[at] ^= 1;
    let t = AlignedBytes::from_slice(&tampered);
    assert!(!NxvBlockView::from_mmap_slice(&t).unwrap().verify_body());
}
