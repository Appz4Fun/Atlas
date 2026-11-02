use super::*;
use crate::archive::crc32;
use crate::probe::archive::{ArchiveHead, entries};

/// Every file of an archive whose data decodes on its own, decoded, against the CRC in its header, or when the
/// header has a BLAKE2 hash instead, against the same file from the other archives.
fn check(name: &str, data: &[u8], seen: &mut std::collections::HashMap<String, Vec<u8>>) -> usize {
    let mut checked = 0;
    for head in entries(data).into_iter().filter(|h| !h.dir && h.stored_at.is_none()) {
        let ArchiveHead { data_at: Some(at), packed: Some(packed), unpacked: Some(size), crc32: want, .. } = head
        else {
            continue;
        };
        if head.solid {
            continue;
        }
        let packed = &data[at..at + packed as usize];
        let (out, end) = rar5(packed, head.rar_version == 1, Some(size), usize::MAX);
        assert_eq!(end, End::Done, "{name}: {:?}", head.name);
        assert_eq!(out.len() as u64, size, "{name}: {:?}", head.name);
        match want {
            Some(crc) => assert_eq!(crc32(&out), crc, "{name}: {:?}", head.name),
            None => {
                let key = head.name.clone().unwrap_or_default();
                if let Some(other) = seen.get(&key) {
                    assert!(*other == out, "{name}: {key} differs from another archive's");
                }
                seen.insert(key, out);
            }
        }
        checked += 1;
    }
    checked
}

#[test]
fn decodes_every_method_and_filter() {
    let mut seen = std::collections::HashMap::new();
    for (name, data) in [
        ("m1_fastest", &include_bytes!("../../../../tests/fixtures/rar5/m1_fastest.rar")[..]),
        ("m3_default", include_bytes!("../../../../tests/fixtures/rar5/m3_default.rar")),
        ("m5_max", include_bytes!("../../../../tests/fixtures/rar5/m5_max.rar")),
        ("filter_arm", include_bytes!("../../../../tests/fixtures/rar5/filter_arm.rar")),
        ("filter_delta", include_bytes!("../../../../tests/fixtures/rar5/filter_delta.rar")),
        ("filter_e8", include_bytes!("../../../../tests/fixtures/rar5/filter_e8.rar")),
        ("filter_e8e9", include_bytes!("../../../../tests/fixtures/rar5/filter_e8e9.rar")),
        ("solid", include_bytes!("../../../../tests/fixtures/rar5/solid.rar")),
    ] {
        assert!(check(name, data, &mut seen) > 0, "{name}: nothing checked");
    }
}

#[test]
fn a_cut_gives_a_true_front() {
    let data = include_bytes!("../../../../tests/fixtures/rar5/m3_default.rar");
    let head = entries(data).into_iter().find(|h| !h.dir).unwrap();
    let at = head.data_at.unwrap();
    let packed = &data[at..at + head.packed.unwrap() as usize];
    let (whole, _) = rar5(packed, false, head.unpacked, usize::MAX);
    for cut in [packed.len() / 4, packed.len() / 2, packed.len() * 3 / 4] {
        let (front, end) = rar5(&packed[..cut], false, head.unpacked, usize::MAX);
        assert_eq!(end, End::Cut);
        assert!(whole.starts_with(&front), "{cut}");
    }
    let (front, end) = rar5(packed, false, head.unpacked, 1000);
    assert_eq!((front.len(), end), (1000, End::Limit));
}

#[test]
fn junk_never_panics() {
    let data = include_bytes!("../../../../tests/fixtures/rar5/m3_default.rar");
    let head = entries(data).into_iter().find(|h| !h.dir).unwrap();
    let at = head.data_at.unwrap();
    let packed = &data[at..at + head.packed.unwrap() as usize];
    let mut seed = 17u32;
    for _ in 0..500 {
        let mut d = packed.to_vec();
        for _ in 0..3 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let i = (seed as usize >> 8) % d.len();
            d[i] ^= (seed >> 3) as u8 | 1;
        }
        let _ = rar5(&d, seed & 1 == 0, None, 1 << 20);
    }
}
