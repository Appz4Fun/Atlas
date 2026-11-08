//! Gate 6: a block written to a file and mapped back gives every row, finds rows by article number at the
//! sub-block edges, and a cold block routes from the map and comes back whole.

use std::io::Write;

use nntp_xover_engine::corpus::{Rng, generate_realistic_xover_corpus};
use nntp_xover_engine::*;

fn art_of(l: &[u8]) -> u64 {
    std::str::from_utf8(l.split(|&b| b == b'\t').next().unwrap()).unwrap().parse().unwrap()
}

fn mapped(bytes: &[u8]) -> (tempfile::NamedTempFile, memmap2::Mmap) {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(bytes).unwrap();
    f.flush().unwrap();
    // SAFETY: the file is ours and not written again while mapped.
    let map = unsafe { memmap2::Mmap::map(f.as_file()).unwrap() };
    (f, map)
}

#[test]
fn every_row_from_a_mapped_file() {
    let lines = generate_realistic_xover_corpus(10_000);
    let refs: Vec<&[u8]> = lines.iter().map(|l| l.as_slice()).collect();
    let hot = NxvBlockBuilder::build_hot_block(&refs).unwrap();
    let (_f, map) = mapped(&hot);
    let view = NxvBlockView::from_mmap_slice(&map).unwrap();
    assert_eq!(view.row_count(), 10_000);
    let mut cache = RowCache::default();
    let mut out = Vec::new();
    let mut r = Rng::new(5);
    for _ in 0..10_000 {
        let i = r.below(10_000) as usize;
        out.clear();
        view.reconstruct_row(i, &mut cache, &mut out).unwrap();
        assert_eq!(&out, &lines[i], "row {i}");
    }
    for (i, l) in lines.iter().enumerate() {
        out.clear();
        view.reconstruct_row(i, &mut cache, &mut out).unwrap();
        assert_eq!(&out, l, "row {i}");
    }
}

#[test]
fn rows_by_article_number_at_the_edges() {
    let lines = generate_realistic_xover_corpus(10_000);
    let arts: Vec<u64> = lines.iter().map(|l| art_of(l)).collect();
    let refs: Vec<&[u8]> = lines.iter().map(|l| l.as_slice()).collect();
    let hot = NxvBlockBuilder::build_hot_block(&refs).unwrap();
    let (_f, map) = mapped(&hot);
    let view = NxvBlockView::from_mmap_slice(&map).unwrap();
    for row in [0usize, 1, 126, 127, 128, 129, 255, 256, 257, 5000, 9998, 9999] {
        assert_eq!(view.find_row_by_art_num(arts[row]), Some(row), "row {row}");
    }
    assert_eq!(view.find_row_by_art_num(arts[0] - 1), None);
    assert_eq!(view.find_row_by_art_num(arts[9999] + 1), None);
    assert_eq!(view.find_row_by_art_num(0), None);
    assert_eq!(view.find_row_by_art_num(u64::MAX), None);
    // the corpus skips numbers now and then: a missing one inside the range
    let gap = arts.windows(2).find(|w| w[1] - w[0] > 1).expect("a gap in the numbering");
    assert_eq!(view.find_row_by_art_num(gap[0] + 1), None);
    assert_eq!(view.lower_bound_row(gap[0] + 1).unwrap(), arts.iter().position(|&a| a == gap[1]).unwrap());
}

#[test]
fn a_cold_block_routes_from_the_map_and_comes_back_whole() {
    let lines = generate_realistic_xover_corpus(10_000);
    let refs: Vec<&[u8]> = lines.iter().map(|l| l.as_slice()).collect();
    let hot = NxvBlockBuilder::build_hot_block(&refs).unwrap();
    let cold = compress_block_to_cold(&hot, 9).unwrap();
    assert!(cold.len() < hot.len(), "cold {} hot {}", cold.len(), hot.len());
    println!("hot {} bytes, cold {} bytes", hot.len(), cold.len());
    let (_f, map) = mapped(&cold);
    let view = NxvBlockView::from_mmap_slice(&map).unwrap();
    assert!(view.is_cold());
    assert_eq!(view.row_count(), 10_000);
    let (first, last) = (art_of(&lines[0]), art_of(&lines[9999]));
    assert!(view.overlaps_art_range(first, first));
    assert!(view.overlaps_art_range(last, u64::MAX));
    assert!(!view.overlaps_art_range(0, first - 1));
    assert!(!view.overlaps_art_range(last + 1, u64::MAX));
    let m = lines[4242].split(|&b| b == b'\t').nth(4).unwrap();
    assert!(view.may_contain_msgid(m));
    assert!(view.find_row_by_msgid(m).is_none(), "a cold block answers only the filter");
    assert!(view.find_row_by_art_num(art_of(&lines[4242])).is_none());
    let back = decompress_cold_block(&map).unwrap();
    assert_eq!(&back[..], &hot[..]);
    let hv = NxvBlockView::from_mmap_slice(&back).unwrap();
    assert_eq!(hv.find_row_by_msgid(m), Some(4242));
    assert_eq!(hv.find_row_by_art_num(art_of(&lines[4242])), Some(4242));
}
