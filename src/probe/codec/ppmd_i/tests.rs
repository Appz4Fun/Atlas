use super::*;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/ppmd/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// 80 000 bytes from a 32-letter alphabet, the text in `restart.ppmd` and `cutoff.ppmd`
fn lcg_text() -> Vec<u8> {
    let alphabet = b"abcdefghijklmnopqrstuvwxyz .,\n'-";
    let mut x = 1u32;
    (0..80_000)
        .map(|_| {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            alphabet[((x >> 16) % 32) as usize]
        })
        .collect()
}

#[test]
fn decodes_what_pyppmd_made() {
    let want = fixture("mixed.bin");
    let (out, end, _) = run(&fixture("mixed.ppmd"), Some(want.len() as u64), usize::MAX);
    assert_eq!(end, End::Done);
    assert!(out == want, "first difference at {:?}", out.iter().zip(&want).position(|(a, b)| a != b));
}

#[test]
fn the_biggest_model_a_header_can_ask_for_still_decodes() {
    // the heap keeps only what's written, so this costs a few MiB, not 256 (see heap.rs's test)
    let want = fixture("mixed.bin");
    let (out, end, _) = run(&fixture("mixed-256m.ppmd"), Some(want.len() as u64), usize::MAX);
    assert_eq!(end, End::Done);
    assert!(out == want);
}

#[test]
fn starts_over_or_cuts_back_when_memory_runs_out() {
    let want = lcg_text();
    for (name, restarts) in [("restart.ppmd", true), ("cutoff.ppmd", false)] {
        let (out, end, (restarted, cut)) = run(&fixture(name), Some(want.len() as u64), usize::MAX);
        assert_eq!(end, End::Done, "{name}");
        assert!(out == want, "{name}: first difference at {:?}", out.iter().zip(&want).position(|(a, b)| a != b));
        // the model's first start counts as one
        if restarts {
            assert!(restarted > 1 && cut == 0, "{name}: {restarted} restarts, {cut} cut-offs");
        } else {
            assert!(cut > 0 && restarted == 1, "{name}: {restarted} restarts, {cut} cut-offs");
        }
    }
}

#[test]
fn stops_at_the_limit() {
    let want = fixture("mixed.bin");
    let (out, end) = zip(&fixture("mixed.ppmd"), Some(want.len() as u64), 1000);
    assert_eq!(end, End::Limit);
    assert_eq!(out, want[..1000]);
}

#[test]
fn a_cut_stream_gives_a_true_prefix() {
    let want = fixture("mixed.bin");
    let data = fixture("mixed.ppmd");
    for keep in [2, 3, 6, 100, 5000, data.len() / 2, data.len() - 3] {
        let (out, end) = zip(&data[..keep], Some(want.len() as u64), usize::MAX);
        assert_eq!(end, End::Cut, "kept {keep}");
        assert!(want.starts_with(&out), "kept {keep}: not a prefix");
    }
}

#[test]
fn headers_it_cant_use_are_cut() {
    // order 1, and a restore method that doesn't exist
    for head in [0x00f0u16, 0x2015] {
        let mut data = head.to_le_bytes().to_vec();
        data.extend_from_slice(&[0; 16]);
        assert_eq!(zip(&data, Some(10), usize::MAX), (Vec::new(), End::Cut));
    }
    assert_eq!(zip(&[5], None, usize::MAX), (Vec::new(), End::Cut));
}

#[test]
fn junk_never_panics() {
    let mut x = 0x1234_5678u32;
    for round in 0..300 {
        let len = 2 + round * 7;
        let mut data: Vec<u8> = (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        // small memory so the model fills and starts over, or is cut back
        data[0] = (data[0] & 0x0f) | 0x10;
        data[1] &= 0x10;
        let (out, _) = zip(&data, None, 1 << 16);
        assert!(out.len() <= 1 << 16);
    }
}
