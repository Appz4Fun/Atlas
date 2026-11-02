//! Whole pictures against a reference decoder's output, by the MD5 of each plane's shown area.
//!
//! The fixtures are single IRAP pictures from libx265, each made to use different tools. The hashes are of the
//! reference decoder's output: 8 bit samples as bytes, 10 bit ones as little endian pairs.

use md5::{Digest, Md5};

use super::decode_keyframe;
use super::nal::split_annex_b;
use crate::video::picture::Picture;

/// Each plane's shown samples, as the reference decoder writes them out.
fn shown(p: &Picture) -> Vec<Vec<u16>> {
    p.planes
        .iter()
        .enumerate()
        .map(|(c, plane)| {
            let s = if c == 0 { 1 } else { 2 };
            let cr = p.crop;
            let mut out = Vec::new();
            for y in cr.top / s..plane.height - cr.bottom / s {
                for x in cr.left / s..plane.width - cr.right / s {
                    out.push(plane.get(x, y));
                }
            }
            out
        })
        .collect()
}

fn md5s(p: &Picture) -> Vec<String> {
    shown(p)
        .iter()
        .map(|plane| {
            let mut h = Md5::new();
            for &v in plane {
                if p.bit_depth > 8 {
                    h.update(v.to_le_bytes());
                } else {
                    h.update([v as u8]);
                }
            }
            format!("{:x}", h.finalize())
        })
        .collect()
}

fn decode(name: &str) -> Picture {
    let path = format!("{}/tests/fixtures/hevc/{name}.hevc", env!("CARGO_MANIFEST_DIR"));
    let data = std::fs::read(path).unwrap();
    decode_keyframe(&split_annex_b(&data)).unwrap()
}

fn check(name: &str, size: (usize, usize), bit_depth: u8, want: [&str; 3]) {
    let p = decode(name);
    let (w, h, _) = p.luma8();
    assert_eq!((w, h, p.bit_depth), (size.0, size.1, bit_depth), "{name}");
    assert_eq!(md5s(&p), want, "{name}");
}

#[test]
fn main_8_bit_with_wavefronts_sao_and_deblocking() {
    // 64x64 coding tree blocks over 720 rows: the last row of them is cut short
    check(
        "i8",
        (1280, 720),
        8,
        ["129711dacd942350fc661b3a62941510", "de29d47d9223f2f616ed95772adca4ea", "ff188b06778971b27b945a941ce4e530"],
    );
}

#[test]
fn main_10_bit_with_wavefronts_sao_and_deblocking() {
    check(
        "i10",
        (1280, 720),
        10,
        ["6fbf76ff22e008e3416d58542387962c", "b90a61c5f30265f9dcbf7efcc9ee439a", "d31f1f24732b812d79b5c0072474a845"],
    );
}

#[test]
fn small_coding_trees_transform_skip_and_qp_deltas() {
    // ctu 16, quantization groups of 8, adaptive quantization
    check(
        "s1",
        (416, 240),
        8,
        ["d5152a3fa76984e931956b404443f00f", "44f4dce5790226cd217aeb4ad78647af", "5c7e5bc7213d9c8083b155292a7e9f78"],
    );
}

#[test]
fn scaling_lists_three_slices_and_sign_hiding() {
    check(
        "s2",
        (416, 240),
        8,
        ["ad15d29c56df15c2de7c565586d66dc0", "cfa9316e8308ea2184752ae978e33bba", "65c0e6acc38da50240f828f12d31f892"],
    );
}

#[test]
fn lossless_transquant_bypass() {
    check(
        "s3",
        (176, 144),
        8,
        ["b3ca4b4fc2bc0a6dacfa52677625e6e2", "d2ae7d63c9d0f0ac53d8d7c29cb81800", "74b0e3637e00cb453d83247f1ddbd7b0"],
    );
}

#[test]
fn ten_bit_low_qp_without_strong_smoothing() {
    check(
        "s4",
        (416, 240),
        10,
        ["5db890dfed99f169ac891ca2f3d9552d", "780b2258e28b667d3330ecb7821e6a15", "1536f1d6218ca07968a0c1e486747a21"],
    );
}

#[test]
fn cropped_odd_size_with_some_lossless_blocks() {
    // 422x238 is coded as 424x240 with a conformance window
    check(
        "s5",
        (422, 238),
        8,
        ["ec6c4c4c1bf84a5cf4f0e7cc89d971af", "02a09930f2e43cc58c5994ba2696abe7", "02a09930f2e43cc58c5994ba2696abe7"],
    );
}

fn units_of(name: &str) -> Vec<Vec<u8>> {
    let path = format!("{}/tests/fixtures/hevc/{name}.hevc", env!("CARGO_MANIFEST_DIR"));
    split_annex_b(&std::fs::read(path).unwrap()).into_iter().map(<[u8]>::to_vec).collect()
}

#[test]
fn a_picture_missing_slices_is_truncated() {
    // s2 has three slices: parameter sets, SEI and only the first
    let units = units_of("s2");
    let slices = units.iter().filter(|u| super::nal::is_slice((u[0] >> 1) & 0x3f)).count();
    assert_eq!(slices, 3);
    let first_slice = units.iter().position(|u| super::nal::is_slice((u[0] >> 1) & 0x3f)).unwrap();
    let refs: Vec<&[u8]> = units[..=first_slice].iter().map(Vec::as_slice).collect();
    assert_eq!(decode_keyframe(&refs).err(), Some(super::DecodeError::Truncated));
    let (partial, _) = super::decode_partly(&refs);
    assert!(partial.is_some(), "what was decoded is still there");
}

#[test]
fn the_next_picture_ends_this_one_whatever_it_is() {
    // a P slice after the keyframe: the keyframe alone decodes
    let mut units = units_of("s1");
    let slice = units.iter().position(|u| super::nal::is_slice((u[0] >> 1) & 0x3f)).unwrap();
    // TRAIL_R with first_slice_segment_in_pic_flag set and garbage after: never parsed
    units.push(vec![0x02, 0x01, 0x80 | 0x40, 0xff, 0xff]);
    let refs: Vec<&[u8]> = units.iter().map(Vec::as_slice).collect();
    assert!(slice < units.len() - 1);
    assert_eq!(md5s(&decode_keyframe(&refs).unwrap()), md5s(&decode("s1")));
}

#[test]
fn a_size_that_isnt_whole_coding_blocks_is_refused() {
    // the damage that made a height of 123 with 8 sample blocks
    let mut units = units_of("s1");
    let sps = units.iter().position(|u| (u[0] >> 1) & 0x3f == super::nal::SPS).unwrap();
    units[sps][20] ^= 0x10;
    let refs: Vec<&[u8]> = units.iter().map(Vec::as_slice).collect();
    assert!(decode_keyframe(&refs).is_err());
}

#[test]
fn damage_is_an_error_not_a_panic() {
    let path = format!("{}/tests/fixtures/hevc/s1.hevc", env!("CARGO_MANIFEST_DIR"));
    let data = std::fs::read(path).unwrap();
    let units = split_annex_b(&data);
    // cut the slice short, and flip bytes in it, at many points
    let slice = units.iter().position(|u| super::nal::is_slice((u[0] >> 1) & 0x3f)).unwrap();
    for cut in (3..units[slice].len()).step_by(97) {
        let mut damaged: Vec<Vec<u8>> = units.iter().map(|u| u.to_vec()).collect();
        damaged[slice].truncate(cut);
        let refs: Vec<&[u8]> = damaged.iter().map(Vec::as_slice).collect();
        let _ = decode_keyframe(&refs);
        let mut flipped: Vec<Vec<u8>> = units.iter().map(|u| u.to_vec()).collect();
        flipped[slice][cut] ^= 0x5a;
        let refs: Vec<&[u8]> = flipped.iter().map(Vec::as_slice).collect();
        let _ = decode_keyframe(&refs);
    }
}

/// ATLAS_HEVC_FILE=x.hevc ATLAS_HEVC_REF=x.yuv cargo test -- --ignored compare_with_reference: decodes any stream
/// and says where it first differs from the reference decoder's output.
#[test]
#[ignore]
fn compare_with_reference() {
    let data = std::fs::read(std::env::var("ATLAS_HEVC_FILE").unwrap()).unwrap();
    let (p, error) = super::decode_partly(&split_annex_b(&data));
    eprintln!("error: {error:?}");
    let p = p.unwrap();
    let reference = std::fs::read(std::env::var("ATLAS_HEVC_REF").unwrap()).unwrap();
    let mut at = 0;
    for (c, plane) in shown(&p).iter().enumerate() {
        let w = (p.planes[c].width - (p.crop.left + p.crop.right) / if c == 0 { 1 } else { 2 }).max(1);
        let mut differences = 0;
        for (i, &got) in plane.iter().enumerate() {
            let want = if p.bit_depth > 8 {
                at += 2;
                u16::from_le_bytes([reference[at - 2], reference[at - 1]])
            } else {
                at += 1;
                u16::from(reference[at - 1])
            };
            if got != want {
                if differences < 5 {
                    eprintln!("plane {c} ({}, {}): {got} not {want}", i % w, i / w);
                }
                differences += 1;
            }
        }
        eprintln!("plane {c}: {differences} differ");
    }
}

/// Flipped bits all over every fixture: run in release, it takes a while in debug.
#[test]
#[ignore]
fn random_damage_never_panics() {
    let mut seed = 0x1234_5678_9abc_def0u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for name in ["i8", "s1", "s2", "s3", "s4", "s5"] {
        let path = format!("{}/tests/fixtures/hevc/{name}.hevc", env!("CARGO_MANIFEST_DIR"));
        let data = std::fs::read(path).unwrap();
        for _ in 0..300 {
            let mut d = data.clone();
            for _ in 0..1 + next() % 8 {
                let i = (next() as usize) % d.len();
                d[i] ^= 1 << (next() % 8);
            }
            let _ = decode_keyframe(&split_annex_b(&d));
        }
    }
}
