//! Version 2's encodings, each on the rows that use it and the edges around it: per-row token subjects, segments and
//! pool entries found from checkpoints, narrow id columns, masked subject counters, sub-blocks without references,
//! and the consecutive / slot-0 sub-block flags. Every block is checked row by row, through the cursor, by article
//! number and by message-id, and cold.

use nntp_xover_engine::container::{
    AlignedBytes, BlockMeta, CP_CONSECUTIVE, CP_SLOT0_IS_ART, NxvBlockBuilder, NxvBlockView, SectionId, VERSION_1,
    VERSION_2, crc32, msgid_field_pub,
};
use nntp_xover_engine::corpus::{Rng, generate};
use nntp_xover_engine::subject_codec::extract_subject_template;
use nntp_xover_engine::*;

const B64: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ.-";
const B62: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
const HEX: &[u8] = b"0123456789abcdef";
const DATE: &str = "Sun, 08 Nov 2026 06:36:35 GMT";

fn token(r: &mut Rng, alphabet: &[u8], n: usize) -> String {
    (0..n).map(|_| alphabet[r.below(alphabet.len() as u64) as usize] as char).collect()
}

/// Whether a subject (unique in its block) is kept per row: long enough, all in the packing alphabet, no counters.
fn per_row(subject: &str) -> bool {
    let mut tpl = Vec::new();
    subject.len() >= 12
        && subject.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && extract_subject_template(subject.as_bytes(), &mut tpl).is_some_and(|_| tpl == subject.as_bytes())
}

fn art_of(l: &[u8]) -> u64 {
    std::str::from_utf8(l.split(|&b| b == b'\t').next().unwrap()).unwrap().parse().unwrap_or(0)
}

fn build(lines: &[Vec<u8>], version: u16) -> AlignedBytes {
    let refs: Vec<&[u8]> = lines.iter().map(|l| l.as_slice()).collect();
    AlignedBytes::from_slice(&NxvBlockBuilder::build_hot_block_version(&refs, BlockMeta::default(), version).unwrap())
}

/// Every way of reading `view` gives `lines` (already in article order) back.
fn check_all(view: NxvBlockView, lines: &[Vec<u8>]) {
    assert_eq!(view.row_count(), lines.len());
    let mut cache = RowCache::default();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        out.clear();
        view.reconstruct_row(i, &mut cache, &mut out).unwrap();
        assert_eq!(out, *l, "row {i}");
    }
    // random order, with one cache (pool hints in play) and with a fresh one each time (from the checkpoints)
    let mut r = Rng::new(lines.len() as u64);
    for k in 0..lines.len().min(3000) {
        let i = r.below(lines.len() as u64) as usize;
        let got = if k % 2 == 0 {
            out.clear();
            view.reconstruct_row(i, &mut cache, &mut out).unwrap();
            out.clone()
        } else {
            view.reconstruct_row_vec(i).unwrap()
        };
        assert_eq!(got, lines[i], "random row {i}");
    }
    let mut scratch = XoverStreamScratch::new();
    let mut cur = NxvRangeCursor::new_for_art_range(view, 0, u64::MAX).unwrap();
    let mut i = 0;
    while cur.format_next_row(&mut scratch) {
        assert_eq!(scratch.wire_buf.strip_suffix(b"\r\n").unwrap(), lines[i].as_slice(), "cursor row {i}");
        scratch.wire_buf.clear();
        i += 1;
    }
    assert_eq!(i, lines.len());
    for (i, l) in lines.iter().enumerate() {
        let a = art_of(l);
        assert_eq!(view.find_row_by_art_num(a), Some(lines.partition_point(|x| art_of(x) < a)), "art of row {i}");
        if let Some(m) = msgid_field_pub(l) {
            let want = lines.iter().position(|x| msgid_field_pub(x) == Some(m)).unwrap();
            assert_eq!(view.find_row_by_msgid_with(m, &mut cache), Some(want), "msgid of row {i}");
            assert_eq!(view.find_row_by_msgid(m), Some(want));
        }
    }
}

/// Builds `lines` in both versions, checks both every way and cold, and returns the version-2 block.
fn round_trip(lines: &[Vec<u8>]) -> AlignedBytes {
    let mut sorted = lines.to_vec();
    sorted.sort_by_key(|l| art_of(l));
    for version in [VERSION_1, VERSION_2] {
        let block = build(lines, version);
        let view = NxvBlockView::from_mmap_slice(&block).unwrap();
        assert_eq!(view.header.version, version);
        assert!(view.verify_body());
        check_all(view, &sorted);
        let cold = AlignedBytes::from_slice(&compress_block_to_cold(&block, 3).unwrap());
        let cv = NxvBlockView::from_mmap_slice(&cold).unwrap();
        assert!(cv.is_cold() && cv.header.version == version);
        for l in sorted.iter().take(50) {
            if let Some(m) = msgid_field_pub(l) {
                assert!(cv.may_contain_msgid(m));
            }
        }
        assert_eq!(&decompress_cold_block(&cold).unwrap()[..], &block[..]);
    }
    build(lines, VERSION_2)
}

fn line(art: u64, subject: &str, poster: &str, msgid: &str, refs: &str, xref: &str) -> Vec<u8> {
    format!("{art}\t{subject}\t{poster}\t{DATE}\t{msgid}\t{refs}\t739685\t5687\t{xref}").into_bytes()
}

fn xref(art: u64) -> String {
    format!("Xref: news.example.com alt.binaries.bloaf:{art}")
}

#[test]
fn the_corpus_round_trips_in_both_versions_and_shrinks() {
    let lines = generate(5000, 31);
    let v2 = round_trip(&lines);
    let v1 = build(&lines, VERSION_1);
    assert!(v2.len() < v1.len(), "v2 {} v1 {}", v2.len(), v1.len());
}

#[test]
fn unique_token_subjects_are_kept_per_row() {
    let mut r = Rng::new(4);
    let mut lines = Vec::new();
    let mut want_tokens = 0;
    let repeated = token(&mut r, B64, 40);
    for i in 0..700u64 {
        let art = 5000 + i * 3;
        let local = token(&mut r, B64, 30);
        let (subject, tok) = match i % 14 {
            0 | 1 => (token(&mut r, HEX, 32), true),
            2 | 3 => (token(&mut r, B64, 32 + (i as usize % 29)), true),
            4 => (token(&mut r, B64, 63), true),
            5 => (token(&mut r, B64, 64), true),
            6 => (token(&mut r, B64, 200), true),
            // stays in the dictionary: on two rows, too short, outside the alphabet, with a counter, the local part
            7 => (repeated.clone(), false),
            8 => (token(&mut r, B64, 11), false),
            9 => (format!("{}_{}", token(&mut r, B64, 10), token(&mut r, B64, 10)), false),
            10 => (format!("{}.{:03}.{}", token(&mut r, B64, 10), i, token(&mut r, B64, 10)), false),
            11 => (local.clone(), false),
            12 => ("Re: plain words with spaces".to_string(), false),
            _ => (String::new(), false),
        };
        assert!(!tok || per_row(&subject) || subject.contains('.'), "{subject}");
        want_tokens += (tok && per_row(&subject)) as usize;
        let poster =
            format!("{} <{}@{}.{}>", token(&mut r, B64, 14), token(&mut r, B64, 12), token(&mut r, B64, 6), "nl");
        lines.push(line(art, &subject, &poster, &format!("<{local}@{}.xx>", token(&mut r, B64, 8)), "", &xref(art)));
        if i % 97 == 0 {
            // a literal row now and then: its segment is empty
            lines.push(
                format!("{}\t{}\tp\tnot a date\t<lit{i}@x>\t\t1\t1", art + 1, token(&mut r, B64, 32)).into_bytes(),
            );
        }
    }
    let block = round_trip(&lines);
    let view = NxvBlockView::from_mmap_slice(&block).unwrap();
    assert_eq!(view.token_subject_rows(), want_tokens);
    assert_eq!(view.literal_rows(), 8);
    // the repeated token, the counter templates, the short and odd ones, the plain subject, the local part's empty
    // template and the empty subject: no per-row token is in the dictionary
    assert!(view.dict_sizes()[0] < 700 - want_tokens as u32, "{:?}", view.dict_sizes());
}

#[test]
fn a_block_of_only_obfuscated_rows_keeps_almost_nothing_per_row_in_its_columns() {
    let mut r = Rng::new(8);
    let lines: Vec<Vec<u8>> = (0..1000u64)
        .map(|i| {
            let art = 70_000_000 + i;
            let poster =
                format!("{} <{}@{}.fg>", token(&mut r, B62, 18), token(&mut r, B62, 14), token(&mut r, B62, 11));
            let msgid = format!("<{}@{}.Y6>", token(&mut r, B62, 42), token(&mut r, B62, 8));
            line(art, &token(&mut r, B62, 47), &poster, &msgid, "", &xref(art))
        })
        .collect();
    let block = round_trip(&lines);
    let view = NxvBlockView::from_mmap_slice(&block).unwrap();
    assert_eq!(view.token_subject_rows(), 1000);
    assert_eq!(view.dict_sizes()[0], 0, "no subject template at all");
    assert_eq!(view.poster_count(), 1000);
    let width = |id: SectionId| view.sections[id as usize].encoding_flags & 0xFF;
    // one template id (the token's), one pad mask, skeleton and prefix; authors and domains are the row numbers
    for id in [SectionId::SubjTplIds, SectionId::SubjPadMasks, SectionId::XrefSkelIds, SectionId::MsgIdPfxIds] {
        assert_eq!(width(id), 0, "{id:?}");
    }
    for id in [SectionId::AuthorIds, SectionId::DomainIds] {
        assert_eq!(width(id), 3, "{id:?}");
        assert_eq!(view.sections[id as usize].byte_len, 0);
    }
    // consecutive article numbers, slot 0 the article number: no numbers for either
    let both = CP_CONSECUTIVE | CP_SLOT0_IS_ART;
    assert!(view.checkpoints.iter().all(|c| c.varint_byte_offset & both == both));
}

#[test]
fn id_columns_take_the_narrowest_width() {
    let mk = |rows: u64, author: &dyn Fn(u64) -> String| -> Vec<Vec<u8>> {
        (0..rows)
            .map(|i| line(100 + i, "[1/2] x (1/5)", &author(i), &format!("<m{i}@x>"), "", &xref(100 + i)))
            .collect()
    };
    let cases: [(Vec<Vec<u8>>, u32); 6] = [
        (mk(300, &|_| "one poster".into()), 0),
        (mk(300, &|i| format!("poster {}", i % 10)), 1),
        (mk(600, &|i| format!("poster {}", i % 300)), 2),
        (mk(300, &|i| format!("poster {i}")), 3),
        (mk(1, &|_| "alone".into()), 0),
        // 257 posters: too many for a palette
        (mk(600, &|i| format!("poster {}", (i * 7) % 257)), 2),
    ];
    for (k, (lines, want)) in cases.iter().enumerate() {
        let block = round_trip(lines);
        let view = NxvBlockView::from_mmap_slice(&block).unwrap();
        let flags = view.sections[SectionId::AuthorIds as usize].encoding_flags;
        assert_eq!(flags & 0xFF, *want, "case {k}");
        let distinct: std::collections::HashSet<&[u8]> =
            lines.iter().map(|l| l.split(|&b| b == b'\t').nth(2).unwrap()).collect();
        assert_eq!(view.poster_count() as usize, distinct.len(), "case {k}");
        if *want < 2 {
            assert_eq!((flags >> 8) as usize, distinct.len(), "case {k}: palette length");
        }
    }
}

#[test]
fn pool_entries_are_found_from_their_checkpoints_in_every_mode() {
    let mut r = Rng::new(15);
    let words = ["Ocean", "Night", "Signal", "Winter", "Garden", "Empire", "Shadow", "River"];
    let lines: Vec<Vec<u8>> = (0..1500u64)
        .map(|i| {
            let art = 1_000 + i;
            // ~400 templates in words (FSST), posters long enough for two-byte lengths, random domains (packed)
            let t = i / 4;
            let subject = format!(
                "{} {} {} [{}/{}] - \"file{t}.rar\" yEnc ({}/9)",
                words[(t % 8) as usize],
                words[(t / 8 % 8) as usize],
                t,
                i % 4 + 1,
                4,
                i % 9 + 1
            );
            // a third of the posters long random printable strings, unique: neither FSST nor packing shrinks them (raw)
            let printable: Vec<u8> = (0x21..0x7f).collect();
            let poster = if i % 3 == 0 {
                token(&mut r, &printable, 130 + (i as usize % 40))
            } else {
                // and the rest from 50 random ones
                token(&mut Rng::new(1 + i % 50), &printable, 30)
            };
            let msgid = format!("<{}@{}.{}>", token(&mut r, HEX, 24), token(&mut r, B64, 9), token(&mut r, B64, 2));
            line(art, &subject, &poster, &msgid, "", &xref(art))
        })
        .collect();
    let block = round_trip(&lines);
    let view = NxvBlockView::from_mmap_slice(&block).unwrap();
    let modes: Vec<u16> = view.dict_stats().iter().map(|d| d.2).collect();
    let stats = view.dict_stats();
    assert!(stats[0].0 > 256 && stats[2].0 > 256 && stats[3].0 > 256, "{stats:?}");
    // subject templates FSST, posters raw, domains packed
    assert_eq!((modes[0], modes[2], modes[3]), (1, 0, 2), "{modes:?}");
}

#[test]
fn counters_that_move_alone_are_masked() {
    let mut lines = Vec::new();
    let mut art = 10u64;
    for file in 1..=6u32 {
        for part in 1..=150u32 {
            art += 1;
            let s = match file % 3 {
                // bracket, paren and file part (five counters): only the part moves
                0 => format!("[{file:02}/06] - \"x.{file:03}.par2\" yEnc ({part}/150)"),
                // two fractions, both moving now and then
                1 => format!("[{}/{}] - \"y.rar\" yEnc ({part:03}/150)", part / 50, file),
                // one fraction only (not masked)
                _ => format!("\"z.rar\" yEnc ({part}/150)"),
            };
            lines.push(line(art, &s, "p <p@q>", &format!("<{art}.x@y>"), "", &xref(art)));
            if part % 40 == 0 {
                // the same subject again: nothing moves
                art += 1;
                lines.push(line(art, &s, "p <p@q>", &format!("<{art}.x@y>"), "", &xref(art)));
            }
        }
    }
    let v2 = round_trip(&lines);
    let v1 = build(&lines, VERSION_1);
    let num = |b: &AlignedBytes| {
        NxvBlockView::from_mmap_slice(b).unwrap().sections[SectionId::NumericVarints as usize].byte_len
    };
    assert!(num(&v2) < num(&v1), "numbers v2 {} v1 {}", num(&v2), num(&v1));
}

#[test]
fn references_are_stored_only_in_sub_blocks_that_have_some() {
    let refs_at = [5u64, 300, 301, 599];
    let lines: Vec<Vec<u8>> = (0..600u64)
        .map(|i| {
            let refs = if refs_at.contains(&i) { format!("<ref{i}@x> <ref{i}b@x>") } else { String::new() };
            line(1 + i, "subject", "p <p@q>", &format!("<m{i}@x>"), &refs, &xref(1 + i))
        })
        .collect();
    let block = round_trip(&lines);
    let view = NxvBlockView::from_mmap_slice(&block).unwrap();
    // sub-blocks 0, 2 and 4 store a length a row; 1 and 3 nothing
    let cps = view.checkpoints;
    assert_eq!(cps[1].refs_byte_offset, cps[2].refs_byte_offset);
    assert_eq!(cps[3].refs_byte_offset, cps[4].refs_byte_offset);
    let bodies: usize = refs_at.iter().map(|i| format!("<ref{i}@x> <ref{i}b@x>").len()).sum();
    assert_eq!(view.sections[SectionId::ReferencesData as usize].byte_len as usize, 128 + 128 + 88 + bodies);
    // and a block with none stores nothing at all
    let none: Vec<Vec<u8>> =
        lines.iter().enumerate().filter(|(i, _)| !refs_at.contains(&(*i as u64))).map(|(_, l)| l.clone()).collect();
    let block = round_trip(&none);
    assert_eq!(NxvBlockView::from_mmap_slice(&block).unwrap().sections[SectionId::ReferencesData as usize].byte_len, 0);
}

#[test]
fn sub_block_flags_mark_only_the_sub_blocks_they_hold_for() {
    let mut lines = Vec::new();
    for i in 0..640u64 {
        // sub-block 1 has a gap, sub-block 2 two equal numbers; the rest run one by one
        let art = 1_000 + i + (i >= 200) as u64 + (i >= 300) as u64 - (i == 301) as u64;
        // sub-block 3 has a row whose slot 0 is another number, sub-block 4 rows without an Xref and cross-posts
        let x = match i {
            400 => "Xref: h a.b:5 c.d:6".to_string(),
            520..=530 => String::new(),
            531..=540 => format!("Xref: h a.b:{art} c.d:{}", 77 + i),
            _ => xref(art),
        };
        let mut l = line(art, "s", "p <p@q>", &format!("<m{i}@x>"), "", &x);
        if x.is_empty() {
            l.truncate(l.len() - 1);
        }
        lines.push(l);
    }
    let block = round_trip(&lines);
    let view = NxvBlockView::from_mmap_slice(&block).unwrap();
    let flags: Vec<u32> =
        view.checkpoints.iter().map(|c| c.varint_byte_offset & (CP_CONSECUTIVE | CP_SLOT0_IS_ART)).collect();
    let both = CP_CONSECUTIVE | CP_SLOT0_IS_ART;
    assert_eq!(flags, [both, CP_SLOT0_IS_ART, CP_SLOT0_IS_ART, CP_CONSECUTIVE, both]);

    // article numbers near the top of the range, one by one
    let top = i64::MAX as u64 - 300;
    let lines: Vec<Vec<u8>> =
        (0..300u64).map(|i| line(top + i, "s", "p", &format!("<t{i}@x>"), "", &xref(top + i))).collect();
    let block = round_trip(&lines);
    let view = NxvBlockView::from_mmap_slice(&block).unwrap();
    assert!(view.checkpoints.iter().all(|c| c.varint_byte_offset & both == both));
}

#[test]
fn store_fields_and_posters_survive_in_version_2() {
    let lines = generate(800, 3);
    let refs: Vec<&[u8]> = lines.iter().map(|l| l.as_slice()).collect();
    let meta = BlockMeta { server_id: 65_535, slab_gen: u32::MAX };
    let hot = NxvBlockBuilder::build_hot_block_meta(&refs, meta).unwrap();
    let v1 = NxvBlockBuilder::build_hot_block_version(&refs, meta, VERSION_1).unwrap();
    let (a, b) = (AlignedBytes::from_slice(&hot), AlignedBytes::from_slice(&v1));
    let (va, vb) = (NxvBlockView::from_mmap_slice(&a).unwrap(), NxvBlockView::from_mmap_slice(&b).unwrap());
    assert_eq!((va.header.version, vb.header.version), (VERSION_2, VERSION_1));
    assert_eq!(va.store_meta(), meta);
    assert_eq!(va.poster_count(), vb.poster_count());
    let cold = compress_block_to_cold(&hot, 3).unwrap();
    assert_eq!(NxvBlockView::from_mmap_slice(&AlignedBytes::from_slice(&cold)).unwrap().store_meta(), meta);
    assert!(NxvBlockBuilder::build_hot_block_version(&refs, meta, 3).is_err());
}

/// Reads every row every way, ignoring what comes back: a damaged block may give wrong rows, but never a panic.
fn read_everything(block: &[u8], ids: &[&[u8]]) {
    let al = AlignedBytes::from_slice(block);
    let Ok(view) = NxvBlockView::from_mmap_slice(&al) else { return };
    let mut cache = RowCache::default();
    let mut out = Vec::new();
    for i in 0..view.row_count() {
        out.clear();
        let _ = view.reconstruct_row(i, &mut cache, &mut out);
    }
    if let Ok(mut cur) = NxvRangeCursor::new_for_art_range(view, 0, u64::MAX) {
        let mut scratch = XoverStreamScratch::new();
        while cur.format_next_row(&mut scratch) {
            scratch.wire_buf.clear();
        }
    }
    for m in ids {
        let _ = view.find_row_by_msgid_with(m, &mut cache);
    }
    let _ = (view.literal_rows(), view.token_subject_rows(), view.dict_stats());
}

#[test]
fn damaged_version_2_blocks_read_without_panicking() {
    let mut r = Rng::new(77);
    let mut lines = generate(400, 9);
    for i in 0..300u64 {
        let art = 90_000_000 + i;
        let poster = format!("{} <{}@{}.fg>", token(&mut r, B64, 18), token(&mut r, B64, 14), token(&mut r, B64, 11));
        let msgid = format!("<{}@{}.Y6>", token(&mut r, B64, 42), token(&mut r, B64, 8));
        lines.push(line(
            art,
            &token(&mut r, HEX, 32),
            &poster,
            &msgid,
            if i % 50 == 0 { "<r@x>" } else { "" },
            &xref(art),
        ));
    }
    let block = build(&lines, VERSION_2);
    let ids: Vec<&[u8]> = lines.iter().step_by(7).filter_map(|l| msgid_field_pub(l)).collect();
    let view = NxvBlockView::from_mmap_slice(&block).unwrap();
    let body = view.header.cold_prefix_bytes as usize..block.len();
    for _ in 0..400 {
        let mut b = block.to_vec();
        for _ in 0..1 + r.below(4) {
            let at = body.start + r.below((body.end - body.start) as u64) as usize;
            b[at] ^= 1 << r.below(8);
        }
        read_everything(&b, &ids);
    }
    // damaged section table entries (flags, lengths, counts), resealed soo the header checksum passes
    for _ in 0..400 {
        let mut b = block.to_vec();
        let at = 64 + r.below(256) as usize;
        b[at] ^= 1 << r.below(8);
        let mut head = b[..320].to_vec();
        head[52..56].fill(0);
        let crc = crc32(&head);
        b[52..56].copy_from_slice(&crc.to_le_bytes());
        read_everything(&b, &ids);
    }
}

#[test]
fn obfuscated_posters_are_kept_by_shape() {
    let mut r = Rng::new(21);
    let mut lines: Vec<Vec<u8>> = (0..800u64)
        .map(|i| {
            let poster = match i % 5 {
                0 => format!("{}@{}.com", token(&mut r, B62, 10), token(&mut r, B62, 8)),
                1 => format!(
                    "\"<<< {} >>>\" <s_{}_{}@web.de>",
                    token(&mut r, B62, 14),
                    token(&mut r, B62, 7),
                    token(&mut r, B62, 5)
                ),
                // a run long enough for a two-byte packed header, and runs at both ends
                2 => format!("{} {}", token(&mut r, B62, 70), token(&mut r, B62, 3)),
                _ => format!(
                    "{} <{}@{}.{}>",
                    token(&mut r, B62, 18),
                    token(&mut r, B62, 14),
                    token(&mut r, B62, 11),
                    "fg"
                ),
            };
            let msgid = format!("<{}@{}.Y6>", token(&mut r, B62, 32), token(&mut r, B62, 8));
            line(
                70_000 + i,
                &format!("[1/5] - \"a.rar\" yEnc ({}/90)", i % 90 + 1),
                &poster,
                &msgid,
                "",
                &xref(70_000 + i),
            )
        })
        .collect();
    // and one poster that is all separators
    lines.push(line(80_000, "s", "<>@()", "<lone@x>", "", &xref(80_000)));
    let block = round_trip(&lines);
    let view = NxvBlockView::from_mmap_slice(&block).unwrap();
    assert_eq!(view.dict_stats()[2].2, 3, "the poster dictionary is shaped");
    assert_eq!(view.poster_count(), 801);
    let v1 = build(&lines, VERSION_1);
    let pool = |v: &NxvBlockView| v.dict_stats()[2].1;
    assert!(pool(&view) < pool(&NxvBlockView::from_mmap_slice(&v1).unwrap()));

    // a poster holding the slot byte (NUL) rules shapes out for its dictionary, and still comes back
    lines.push(line(80_001, "s", "odd\0poster <a@b>", "<nul@x>", "", &xref(80_001)));
    let block = round_trip(&lines);
    assert_ne!(NxvBlockView::from_mmap_slice(&block).unwrap().dict_stats()[2].2, 3);
}
