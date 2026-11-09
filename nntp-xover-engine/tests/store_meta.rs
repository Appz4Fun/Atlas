//! The store's fields ride in the section table: kept through cold and back, and absent (zero) on a block built
//! without them.

use nntp_xover_engine::container::{
    AlignedBytes, BlockMeta, NxvBlockBuilder, NxvBlockView, compress_block_to_cold, decompress_cold_block,
};

fn lines() -> Vec<Vec<u8>> {
    (0..300u64)
        .map(|k| {
            format!(
                "{}\t[1/9] - \"x.part{:02}.rar\" yEnc (1/50)\tposter{}@x\tSun, 08 Nov 2026 10:00:00 +0000\t<m{k}@x>\t\t700000\t5000",
                1000 + k,
                k % 40,
                k % 7
            )
            .into_bytes()
        })
        .collect()
}

#[test]
fn store_fields_survive_cold_and_back() {
    let l = lines();
    let refs: Vec<&[u8]> = l.iter().map(|x| x.as_slice()).collect();
    let meta = BlockMeta { server_id: 9, slab_gen: 123_456 };
    let hot = NxvBlockBuilder::build_hot_block_meta(&refs, meta).unwrap();
    let a = AlignedBytes::from_slice(&hot);
    let v = NxvBlockView::from_mmap_slice(&a).unwrap();
    assert_eq!(v.store_meta(), meta);
    assert_eq!(v.poster_count(), 7);
    assert!(v.verify_body());
    let cold = compress_block_to_cold(&hot, 9).unwrap();
    let c = AlignedBytes::from_slice(&cold);
    assert_eq!(NxvBlockView::from_mmap_slice(&c).unwrap().store_meta(), meta);
    let back = decompress_cold_block(&cold).unwrap();
    assert_eq!(NxvBlockView::from_mmap_slice(&back).unwrap().store_meta(), meta);

    let plain = NxvBlockBuilder::build_hot_block(&refs).unwrap();
    let p = AlignedBytes::from_slice(&plain);
    assert_eq!(NxvBlockView::from_mmap_slice(&p).unwrap().store_meta(), BlockMeta::default());
}
