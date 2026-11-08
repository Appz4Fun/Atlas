//! The field cursor gives each row's fields exactly as the written-out line has them, for both versions, for subjects
//! of every kind (templates, the message-id's local part, tokens kept per row) and for rows kept literal.

use nntp_xover_engine::container::{AlignedBytes, BlockMeta, NxvBlockBuilder, NxvBlockView, VERSION_1, VERSION_2};
use nntp_xover_engine::corpus::generate_realistic_xover_corpus;
use nntp_xover_engine::date::{STYLE_FREE_BITS, parse_date, style_offset_secs};
use nntp_xover_engine::streamer::{FieldCursor, NxvRangeCursor, XoverStreamScratch};

fn lines() -> Vec<Vec<u8>> {
    let mut l = generate_realistic_xover_corpus(3000);
    let base = 9_000_000u64;
    for k in 0..200u64 {
        let local = format!("{:032x}", k * 7919 + 13);
        // the subject is the message-id's local part
        l.push(
            format!(
                "{}\t{local}\tposter{k}@x.y\tSun, 08 Nov 2026 10:{:02}:00 +0100\t<{local}@ngPost>\t\t740000\t5692",
                base + k,
                k % 60
            )
            .into_bytes(),
        );
        // a random token subject, unique in the block
        l.push(
            format!(
                "{}\tA{:031x}\tq{k}@x\tSun, 08 Nov 2026 11:00:{:02} GMT\t<p{k}.r{k}@h>\t\t700000\t5000",
                base + 1000 + k,
                k * 104_729 + 7,
                k % 60
            )
            .into_bytes(),
        );
    }
    // rows the block keeps literal: a non-canonical article number and a date it can't read
    l.push(b"0009100000\tx\ty\tSun, 08 Nov 2026 10:00:00 +0000\t<a@b>\t\t1\t1".to_vec());
    l.push(b"9100001\tsubject\tposter\tnot a date\t<c@d>\t\t1\t1".to_vec());
    l
}

fn field(line: &[u8], k: usize) -> &[u8] {
    line.split(|&b| b == b'\t').nth(k).unwrap_or(&[])
}

#[test]
fn fields_match_the_written_out_lines_in_both_versions() {
    let l = lines();
    let refs: Vec<&[u8]> = l.iter().map(|x| x.as_slice()).collect();
    for version in [VERSION_1, VERSION_2] {
        let block = NxvBlockBuilder::build_hot_block_version(&refs, BlockMeta::default(), version).unwrap();
        let a = AlignedBytes::from_slice(&block);
        let view = NxvBlockView::from_mmap_slice(&a).unwrap();
        let mut written = Vec::new();
        let mut cur = NxvRangeCursor::new_for_art_range(view, 0, u64::MAX).unwrap();
        let mut scratch = XoverStreamScratch::new();
        loop {
            scratch.wire_buf.clear();
            if !cur.format_next_row(&mut scratch) {
                break;
            }
            let n = scratch.wire_buf.len() - 2;
            written.push(scratch.wire_buf[..n].to_vec());
        }
        let mut fc = FieldCursor::new(view).unwrap();
        let mut i = 0;
        let (mut literal, mut local, mut tokens) = (0, 0, 0);
        while let Some(r) = fc.next_row() {
            let line = &written[i];
            i += 1;
            if let Some(lit) = r.literal {
                assert_eq!(lit, line.as_slice());
                literal += 1;
                continue;
            }
            assert_eq!(r.art.to_string().as_bytes(), field(line, 0), "v{version} row {i}");
            assert_eq!(r.subject, field(line, 1), "v{version} row {i}");
            assert_eq!(r.author, field(line, 2));
            assert_eq!(r.msgid, field(line, 4));
            let (ts, style) = parse_date(field(line, 3)).unwrap();
            assert_eq!(r.utc, ts);
            assert_eq!(r.wall, ts + style_offset_secs(style & !STYLE_FREE_BITS));
            let num = |k| std::str::from_utf8(field(line, k)).unwrap().parse::<i64>().ok();
            assert_eq!(r.bytes, num(6));
            assert_eq!(r.lines, num(7));
            if field(line, 4).starts_with(b"<") && field(line, 4)[1..].starts_with(r.subject) && r.subject.len() == 32 {
                local += 1;
            }
            if r.subject.first() == Some(&b'A') && r.subject.len() == 32 {
                tokens += 1;
            }
        }
        assert_eq!(i, written.len(), "every row once");
        assert_eq!(literal, 2, "v{version}");
        assert!(local >= 200, "v{version}: {local}");
        assert!(tokens >= 200, "v{version}: {tokens}");
    }
}

#[test]
fn a_wall_clock_is_the_written_time_with_its_zone_dropped() {
    let l: Vec<Vec<u8>> = vec![
        b"1\ts\tp\tFri, 02 Oct 2026 10:11:12 +0200\t<a@b>\t\t1\t1".to_vec(),
        b"2\ts\tp\tFri, 02 Oct 2026 10:11:12 GMT\t<c@d>\t\t1\t1".to_vec(),
    ];
    let refs: Vec<&[u8]> = l.iter().map(|x| x.as_slice()).collect();
    let block = NxvBlockBuilder::build_hot_block(&refs).unwrap();
    let a = AlignedBytes::from_slice(&block);
    let mut fc = FieldCursor::new(NxvBlockView::from_mmap_slice(&a).unwrap()).unwrap();
    let r1 = fc.next_row().unwrap().wall;
    let r2 = fc.next_row().unwrap().wall;
    assert_eq!(r1, 1_790_935_872, "the clock, not the instant");
    assert_eq!(r2, 1_790_935_872);
    assert!(fc.next_row().is_none());
}
