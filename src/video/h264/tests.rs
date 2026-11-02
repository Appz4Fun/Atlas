//! Whole pictures against a reference decoder's output, by the MD5 of each plane's shown area.
//!
//! The fixtures are keyframes from libx264, each made to use different tools: 8x8 or 4x4 transforms, custom
//! scaling lists, slices, cropping, full range, extreme QPs, no deblocking, adaptive quantisation and a chroma QP
//! offset. The hashes are of ffmpeg's output for the first picture.

use md5::{Digest, Md5};

use super::*;

/// Compares a decoded picture with raw 4:2:0 frames from a reference decoder: the first difference, per plane.
fn differences(p: &Picture, yuv: &[u8]) -> Vec<String> {
    let (w, h) = (p.planes[0].width - p.crop.left - p.crop.right, p.planes[0].height - p.crop.top - p.crop.bottom);
    let mut out = Vec::new();
    let mut at = 0;
    for (c, plane) in p.planes.iter().enumerate() {
        let s = if c == 0 { 1 } else { 2 };
        let (pw, ph, x0, y0) = (w.div_ceil(s), h.div_ceil(s), p.crop.left / s, p.crop.top / s);
        'rows: for y in 0..ph {
            for x in 0..pw {
                let want = yuv[at + y * pw + x];
                let got = plane.get(x0 + x, y0 + y);
                if u16::from(want) != got {
                    let mb = ((y0 + y) * s / 16) * (plane.width * s / 16) + (x0 + x) * s / 16;
                    out.push(format!("plane {c} at ({x}, {y}), macroblock {mb}: {got} not {want}"));
                    break 'rows;
                }
            }
        }
        at += pw * ph;
    }
    out
}

fn check(stream: &[u8], yuv: &[u8]) -> Vec<String> {
    let units = split_annex_b(stream);
    match decode_keyframe(&units) {
        Ok(p) => differences(&p, yuv),
        Err(e) => vec![format!("{e}")],
    }
}

/// ATLAS_H264_DIR: a folder of .h264 streams, each with a .yuv of its first frame from another decoder.
#[test]
#[ignore]
fn compare_with_reference() {
    let dir = std::env::var("ATLAS_H264_DIR").unwrap();
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok()?.path().to_str().map(String::from))
        .filter(|p| p.ends_with(".h264"))
        .collect();
    names.sort();
    let mut bad = 0;
    for name in names {
        let stream = std::fs::read(&name).unwrap();
        let yuv = std::fs::read(name.replace(".h264", ".yuv")).unwrap();
        let d = check(&stream, &yuv);
        println!("{name}: {}", if d.is_empty() { "same".to_string() } else { d.join("; ") });
        bad += usize::from(!d.is_empty());
    }
    assert_eq!(bad, 0);
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/h264/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn md5s(p: &Picture) -> Vec<String> {
    p.planes
        .iter()
        .enumerate()
        .map(|(c, plane)| {
            let s = if c == 0 { 1 } else { 2 };
            let cr = p.crop;
            let mut h = Md5::new();
            for y in cr.top / s..plane.height - cr.bottom / s {
                for x in cr.left / s..plane.width - cr.right / s {
                    h.update([plane.get(x, y) as u8]);
                }
            }
            format!("{:x}", h.finalize())
        })
        .collect()
}

#[test]
fn pictures_match_the_reference_decoder() {
    #[rustfmt::skip]
    let cases: [(&str, (usize, usize), [&str; 3]); 11] = [
        ("i8x8", (352, 288), ["def639c64f5de810cc4dbd144dfba7f2", "094b4f671b718404e36cee53b172b852", "9bc4941e6857cba7e091ea83dd8737a1"]),
        ("i4x4", (352, 288), ["0d080227938976dc3c7ec8c6dbfbcede", "2131901de51ec9309cf9786e40401c32", "79c76d065d041e8bec722e5bf55228e8"]),
        ("cqm", (352, 288), ["9bdcf5729fe5bbfcc1736184d4261ef8", "0fdfb34e78957031953f05e0a649bc67", "d173b7a0373212310ecf0180d4bfdecc"]),
        ("slices", (352, 288), ["1ff3ab66e2f43f0dcb03fc4bd081d58d", "8c6058db09f7476120e4ac79cab55c33", "410360222387972e56fd993c46cd764f"]),
        ("cropped", (350, 198), ["636cb32d36e6047c903c98a7bc668fec", "f008824be19a901b93a2bea8280828a5", "e9167edb366591a0a5b22c3c86a4403e"]),
        ("full_range", (720, 480), ["69c10ca42434f69fe96dff379d377497", "dec69128e43ba2064e2178eb814e55a0", "5228cfcbe3aac380e6ca6d27953df96f"]),
        ("qp51", (640, 360), ["7cb2a4aa070075ef92fc0c7edca5e94a", "dafc7def658d678806dc1ff170606e35", "3ec3e0cd5b705613b17ae58aa1b37c16"]),
        ("no_deblock", (352, 288), ["c737642a6205f609ce2226bf52a88b03", "263dde6f7cfee7c079b7179896ac8c19", "e5dd6c099b1a7595bc020231027e558e"]),
        ("then_p", (352, 288), ["6a6aade191723420dfd258fe59cb02dc", "d390ae1c13acdd84a1ccb32a12692162", "5325cb4898e65b8ac58ee39fa923daae"]),
        ("aq", (1280, 720), ["edfc2d6174e05f81a318f34752604f16", "81d79c2846ebceb2667113cab3cb73b9", "bf216c9cd6c1df615a79133028541849"]),
        ("chroma_offset", (352, 288), ["3ef686f59d7134c071de5ff633e655d3", "61474c6d6f89a1b49b43d504fd047984", "a3825e38ee09d968250b2163623445f1"]),
    ];
    for (name, size, want) in cases {
        let p = decode_keyframe(&split_annex_b(&fixture(&format!("{name}.h264")))).unwrap();
        let (w, h, _) = p.luma8();
        assert_eq!((w, h), size, "{name}");
        assert_eq!(md5s(&p), want, "{name}");
        assert_eq!(p.full_range, name == "full_range", "{name}");
    }
}

#[test]
fn cavlc_is_refused_as_unsupported() {
    let e = decode_keyframe(&split_annex_b(&fixture("cavlc.h264"))).unwrap_err();
    assert_eq!(e, DecodeError::Unsupported("CAVLC"));
}

#[test]
fn a_picture_missing_slices_is_truncated() {
    let data = fixture("slices.h264");
    let units = split_annex_b(&data);
    let slices: Vec<usize> = (0..units.len()).filter(|&i| units[i][0] & 0x1F == 5).collect();
    assert_eq!(slices.len(), 4);
    let kept: Vec<&[u8]> = units.iter().enumerate().filter(|(i, _)| *i != slices[2]).map(|(_, u)| *u).collect();
    assert_eq!(decode_keyframe(&kept).unwrap_err(), DecodeError::Truncated);
}

#[test]
fn a_cut_short_slice_is_an_error() {
    let data = fixture("i8x8.h264");
    let units = split_annex_b(&data);
    let mut cut: Vec<&[u8]> = units.clone();
    let last = cut.len() - 1;
    cut[last] = &units[last][..units[last].len() / 2];
    assert!(decode_keyframe(&cut).is_err());
}

#[test]
fn a_keyframe_from_matroska_decodes() {
    let data = fixture("clip.mkv");
    let t = crate::video::mkv::video_track(&data).unwrap();
    assert_eq!(t.codec, crate::video::mkv::Codec::Avc);
    let avcc = parse_avcc(&t.private).unwrap();
    let from = data.len() / 2;
    let crate::video::mkv::Found::Frame { start, len, .. } = crate::video::mkv::find_keyframe(&data[from..], t.number)
    else {
        panic!()
    };
    let mut units: Vec<&[u8]> = avcc.units.iter().map(Vec::as_slice).collect();
    units.extend(
        crate::video::hevc::nal::split_sized(&data[from + start..from + start + len], avcc.length_size).unwrap(),
    );
    let p = decode_keyframe(&units).unwrap();
    assert_eq!(p.luma8().0, 320);
}

#[test]
fn damage_is_an_error_not_a_panic() {
    let data = fixture("aq.h264");
    for i in (0..data.len()).step_by(997) {
        let mut d = data.clone();
        d[i] ^= 0x5A;
        let _ = decode_keyframe(&split_annex_b(&d));
    }
}

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
    for name in ["i8x8", "i4x4", "cqm", "slices", "cropped", "qp51", "aq"] {
        let data = fixture(&format!("{name}.h264"));
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

#[test]
fn a_picture_bigger_than_4k_is_refused() {
    // 8K by SPS: 512 x 270 macroblocks, refused before anything is allocated
    let mut sps = vec![0x67, 100, 0, 51];
    let mut bits = String::new();
    let ue = |v: u32, bits: &mut String| {
        let n = 32 - (v + 1).leading_zeros();
        bits.push_str(&"0".repeat(n as usize - 1));
        bits.push_str(&format!("{:b}", v + 1));
    };
    // id, chroma 4:2:0, luma and chroma depth 8
    for v in [0, 1, 0, 0] {
        ue(v, &mut bits);
    }
    bits.push_str("00"); // bypass, no scaling
    ue(0, &mut bits); // log2_max_frame_num
    ue(2, &mut bits); // poc type 2
    ue(1, &mut bits); // refs
    bits.push('0');
    ue(511, &mut bits);
    ue(269, &mut bits);
    bits.push_str("1100"); // frame_mbs_only, direct8x8, no crop, no VUI
    bits.push('1');
    while !bits.len().is_multiple_of(8) {
        bits.push('0');
    }
    sps.extend(bits.as_bytes().chunks(8).map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 2).unwrap()));
    let data = fixture("i8x8.h264");
    let mut units = split_annex_b(&data);
    let at = units.iter().position(|u| u[0] & 0x1F == 7).unwrap();
    units[at] = &sps;
    assert_eq!(decode_keyframe(&units).unwrap_err(), DecodeError::Unsupported("picture bigger than 4K"));
}
