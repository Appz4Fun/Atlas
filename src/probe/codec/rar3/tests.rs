use super::*;
use crate::probe::archive::{ArchiveHead, entries};

/// A RAR4 decoder: packed data, unpacked size, limit, whether the data is all there.
pub(crate) type Decode = fn(&[u8], Option<u64>, usize, bool) -> (Vec<u8>, End);

/// Every compressed file of an archive that decodes on its own, decoded with `decode` and checked against the CRC
/// in its header. How many were checked.
pub(crate) fn check(name: &str, data: &[u8], version: &[u8], decode: Decode) -> usize {
    let mut checked = 0;
    for head in entries(data).into_iter().filter(|h| !h.dir && h.stored_at.is_none() && !h.solid) {
        let ArchiveHead { data_at: Some(at), packed: Some(packed), unpacked: Some(size), crc32: Some(want), .. } = head
        else {
            continue;
        };
        assert!(version.contains(&head.rar_version), "{name}: {:?} is version {}", head.name, head.rar_version);
        let packed = &data[at..at + packed as usize];
        let (out, end) = decode(packed, Some(size), usize::MAX, true);
        assert_eq!(end, End::Done, "{name}: {:?}", head.name);
        assert_eq!(out.len() as u64, size, "{name}: {:?}", head.name);
        assert_eq!(crc32(&out), want, "{name}: {:?}", head.name);
        // a cut front is a true front
        for cut in [packed.len() / 3, packed.len() * 2 / 3] {
            let (front, _) = decode(&packed[..cut], Some(size), usize::MAX, false);
            assert!(out.starts_with(&front), "{name}: {:?} cut at {cut}", head.name);
        }
        let (front, end) = decode(packed, Some(size), 100, true);
        assert_eq!((front.len().min(100), end == End::Limit || size <= 100), (out.len().min(100), true));
        checked += 1;
    }
    checked
}

#[test]
fn decodes_lz_and_every_standard_filter() {
    for (name, data) in [
        ("compressed_text", &include_bytes!("../../../../tests/fixtures/rar3/compressed_text_rar300.rar")[..]),
        ("audio", include_bytes!("../../../../tests/fixtures/rar3/rarvm_audio_stereo_rar300.rar")),
        ("delta", include_bytes!("../../../../tests/fixtures/rar3/rarvm_delta_4ch_rar300.rar")),
        ("itanium", include_bytes!("../../../../tests/fixtures/rar3/rarvm_itanium_synthetic_rar300.rar")),
        ("rgb", include_bytes!("../../../../tests/fixtures/rar3/rarvm_rgb_gradient_rar300.rar")),
        ("e8", include_bytes!("../../../../tests/fixtures/rar3/rarvm_x86_e8_rar300.rar")),
        ("e8e9", include_bytes!("../../../../tests/fixtures/rar3/rarvm_x86_e8e9_rar300.rar")),
        ("solid", include_bytes!("../../../../tests/fixtures/rar3/solid_rar300.rar")),
    ] {
        assert!(check(name, data, &[29], rar29) > 0, "{name}: nothing checked");
    }
}

#[test]
fn a_ppmd_block_is_told_apart() {
    let data = include_bytes!("../../../../tests/fixtures/rar3/ppmd_lz_match_rar300.rar");
    let head = entries(data).into_iter().find(|h| !h.dir).unwrap();
    let at = head.data_at.unwrap();
    assert!(starts_with_ppmd(&data[at..]));
    let (out, end) = rar29(&data[at..], head.unpacked, usize::MAX, true);
    assert_eq!((out.len(), end), (0, End::Cut));
}

#[test]
fn junk_never_panics() {
    let mut seed = 7u32;
    for n in 0..3000 {
        let data: Vec<u8> = (0..(n % 300))
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (seed >> 16) as u8
            })
            .collect();
        let _ = rar29(&data, None, 1 << 16, n % 2 == 0);
        let _ = crate::probe::codec::rar20::rar20(&data, None, 1 << 16, n % 2 == 0);
    }
}
