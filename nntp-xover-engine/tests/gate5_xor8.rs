//! Gate 5: the Xor8 filter and the bucket index over the corpus message-ids, standalone.

use nntp_xover_engine::container::AlignedBytes;
use nntp_xover_engine::corpus::{Rng, generate_realistic_xover_corpus};
use nntp_xover_engine::msgid_index::{BucketIndexView, Xor8Builder, Xor8View, build_bucket_index, msgid_hash};

#[test]
fn filter_and_buckets_resolve_every_id_and_reject_every_fake() {
    let ids: Vec<Vec<u8>> = generate_realistic_xover_corpus(10_000)
        .iter()
        .map(|l| l.split(|&b| b == b'\t').nth(4).unwrap().to_vec())
        .collect();
    let hashes: Vec<u64> = ids.iter().map(|m| msgid_hash(m)).collect();
    let filter_bytes = AlignedBytes::from_slice(&Xor8Builder::build(&hashes));
    let filter = Xor8View::from_section(&filter_bytes).unwrap();
    assert!(filter_bytes.len() <= 16 + 3 * Xor8Builder::block_length(10_000) as usize + 8);
    let entries: Vec<(u64, u16)> = hashes.iter().enumerate().map(|(i, &h)| (h, i as u16)).collect();
    let index_bytes = AlignedBytes::from_slice(&build_bucket_index(&entries));
    let index = BucketIndexView::from_section(&index_bytes, entries.len()).unwrap();

    // no false negatives, and the exact row for every id
    for (i, h) in hashes.iter().enumerate() {
        assert!(filter.contains_hash(*h), "{}", String::from_utf8_lossy(&ids[i]));
        assert_eq!(index.find(*h, |row| ids[row] == ids[i]), Some(i));
    }

    // fakes: the filter lets a few through, the exact compare none
    let mut r = Rng::new(99);
    let mut passed = 0;
    for k in 0..100_000 {
        let fake = format!("<fake-{k}.{:016x}@nowhere.example>", r.next_u64());
        let h = msgid_hash(fake.as_bytes());
        if filter.contains_hash(h) {
            passed += 1;
            assert_eq!(index.find(h, |row| ids[row] == fake.as_bytes()), None);
        }
    }
    let fpr = passed as f64 / 100_000.0;
    assert!(fpr <= 0.0045, "false positive rate {fpr:.4}");
    println!("xor8: {} bytes for 10,000 ids, false positive rate {fpr:.4}", filter_bytes.len());
}
