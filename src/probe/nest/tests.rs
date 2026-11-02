use std::io::Write;

use super::*;
use crate::archive::testkit::{r4_file, r4_main, r5_block, r5_end, r5_file, r5_main, rar4, rar5, sz_wrap, vint};

/// The start of a Matroska file, then filler: what sits at the bottom of the chain.
fn video() -> Vec<u8> {
    let mut v =
        b"\x1a\x45\xdf\xa3\x9f\x42\x86\x81\x01\x42\xf7\x81\x01\x42\xf2\x81\x04\x42\xf3\x81\x08\x42\x82\x88matroska"
            .to_vec();
    // noise, as video is: a stream that packs smaller would hit the ratio limit
    let mut seed = 1u32;
    v.extend((0..80_000).map(|_| {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        (seed >> 16) as u8
    }));
    v
}

fn zip_entry(method: u16, name: &str, data: &[u8], unpacked: usize) -> Vec<u8> {
    let mut z = b"PK\x03\x04\x14\0\0\0".to_vec();
    z.extend(method.to_le_bytes());
    z.extend([0; 4]);
    z.extend(crate::archive::crc32(data).to_le_bytes());
    z.extend((data.len() as u32).to_le_bytes());
    z.extend((unpacked as u32).to_le_bytes());
    z.extend((name.len() as u16).to_le_bytes());
    z.extend(0u16.to_le_bytes());
    z.extend(name.as_bytes());
    z.extend(data);
    z
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn tar_of(name: &str, data: &[u8]) -> Vec<u8> {
    let mut h = vec![0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    h[100..107].copy_from_slice(b"0000644");
    h[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
    h[156] = b'0';
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
    h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    let mut t = h;
    t.extend(data);
    t.resize(t.len().div_ceil(512) * 512 + 1024, 0);
    t
}

fn gzip_of(name: &str, data: &[u8]) -> Vec<u8> {
    let mut e = flate2::GzBuilder::new().filename(name).write(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn xz_of(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    lzma_rs::xz_compress(&mut &data[..], &mut out).unwrap();
    out
}

fn lzma_alone_of(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    lzma_rs::lzma_compress(&mut &data[..], &mut out).unwrap();
    out
}

/// The 10 layers: 7z copy, RAR4 stored, zip deflate, RAR5 stored, tar, gzip, xz, 7z LZMA, zip stored, zstd.
fn ten_deep() -> Vec<u8> {
    let v = video();
    let l10 = zstd::encode_all(&v[..], 3).unwrap();
    let l9 = zip_entry(0, "movie.mkv.zst", &l10, l10.len());
    let l8 = sz_wrap(&lzma_alone_of(&l9)[13..], &[0; 8]);
    let l7 = xz_of(&l8);
    let l6 = gzip_of("inner.xz", &l7);
    let l5 = tar_of("inner.xz.gz", &l6);
    let l4 = rar5(&[r5_main(), r5_file(2, 0, b"inner.tar", &[], &l5), r5_end()]);
    let l3 = zip_entry(8, "inner.rar", &deflate(&l4), l4.len());
    let l2 = rar4(&[r4_main(0), r4_file(0, b"inner.zip", &l3)]);
    sz_wrap(&l2, &[0; 8])
}

#[track_caller]
fn same(got: &[u8], want: &[u8]) {
    let diff = got.iter().zip(want).position(|(a, b)| a != b);
    assert!(
        diff.is_none() && got.len() == want.len(),
        "got {} bytes, want {}, first difference at {diff:?}",
        got.len(),
        want.len()
    );
}

fn formats(n: &Nest<'_>) -> Vec<String> {
    n.layers.iter().map(|l| format!("{} {}", l.format, l.method.as_deref().unwrap_or("-"))).collect()
}

#[test]
fn ten_layers_down_to_the_video() {
    let data = ten_deep();
    let n = unwrap(&data);
    assert_eq!(
        formats(&n),
        [
            "7z copy",
            "RAR4 m0",
            "ZIP deflate",
            "RAR5 m0",
            "TAR store",
            "GZIP deflate",
            "XZ lzma2",
            "7z lzma",
            "ZIP store",
            "ZSTD zstd",
        ]
    );
    assert!(n.layers.iter().all(|l| l.stop.is_none()), "{:?}", n.layers);
    assert_eq!(n.layers[1].name.as_deref(), Some("inner.zip"));
    assert_eq!(n.layers[5].name.as_deref(), Some("inner.xz"));
    same(&n.inner, &video());
    assert!(crate::probe::media::parse(&n.inner).is_some());
    // a compressed layer was in the way
    assert_eq!(n.outer_at, None);
}

#[test]
fn an_eleventh_layer_is_named_and_left_shut() {
    let deeper = sz_wrap(&ten_deep(), &[0; 8]);
    let n = unwrap(&deeper);
    assert_eq!(n.layers.len(), MAX_DEPTH + 1);
    assert_eq!(n.layers[MAX_DEPTH].format, "ZSTD");
    assert!(n.layers[MAX_DEPTH].stop.as_deref().unwrap().contains("deeper"));
    assert!(n.inner.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]));
}

#[test]
fn a_stored_chain_stays_one_offset_into_the_outer_bytes() {
    let v = video();
    let inner = rar4(&[r4_main(0), r4_file(0, b"movie.mkv", &v)]);
    let mid = zip_entry(0, "movie.rar", &inner, inner.len());
    let outer = rar5(&[r5_main(), r5_file(2, 0, b"movie.zip", &[], &mid), r5_end()]);
    let n = unwrap(&outer);
    assert_eq!(formats(&n), ["RAR5 m0", "ZIP store", "RAR4 m0"]);
    let at = n.outer_at.unwrap();
    assert_eq!(&outer[at..at + v.len()], v.as_slice());
    same(&n.inner, &v); // cut at the stored file's end
    assert_eq!(n.layers[2].unpacked, Some(v.len() as u64));
}

#[test]
fn the_front_alone_still_goes_deep() {
    let data = ten_deep();
    // as if only the first article had been read
    for cut in [data.len() * 3 / 4, data.len() / 2] {
        let n = unwrap(&data[..cut]);
        assert!(n.layers.len() >= 3, "{cut}: {:?}", formats(&n));
        assert!(video().starts_with(&n.inner) || n.layers.last().unwrap().stop.is_some() || !n.inner.is_empty());
    }
}

#[test]
fn every_cut_of_the_chain_is_safe() {
    let data = ten_deep();
    for cut in (0..data.len()).step_by(97) {
        let _ = unwrap(&data[..cut]);
    }
    // and flipped bytes
    let mut seed = 7u32;
    for _ in 0..300 {
        let mut d = data.clone();
        for _ in 0..4 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let at = (seed as usize >> 8) % d.len();
            d[at] ^= (seed >> 3) as u8 | 1;
        }
        let _ = unwrap(&d);
    }
}

#[test]
fn encryption_stops_the_chain() {
    let v = video();
    let mut z = zip_entry(0, "movie.mkv", &v, v.len());
    z[6] = 1;
    let n = unwrap(&z);
    assert_eq!(n.layers.len(), 1);
    assert!(n.layers[0].encrypted);
    assert_eq!(n.layers[0].stop.as_deref(), Some("encrypted"));
    let hidden = rar5(&[crate::archive::testkit::r5_crypt_header(), r5_main()]);
    let n = unwrap(&hidden);
    assert_eq!((n.layers[0].format.as_str(), n.layers[0].encrypted), ("RAR5", true));
}

#[test]
fn a_bomb_decodes_only_so_far() {
    let zeros = vec![0u8; 64 << 20];
    let packed = deflate(&zeros);
    let z = zip_entry(8, "zeros", &packed, zeros.len());
    let n = unwrap(&z);
    assert!(n.layers[0].capped);
    assert!(n.inner.len() <= (z.len() * MAX_RATIO).min(MAX_DECODED));
    assert!(n.inner.len() > packed.len());
}

#[test]
fn the_budget_covers_every_layer_together() {
    // incompressible, soo each layer is bigger than the ratio needs: only the budget limits it
    let mut seed = 99u32;
    let noise: Vec<u8> = (0..(MAX_DECODED / 2 - 50_000))
        .map(|_| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (seed >> 16) as u8
        })
        .collect();
    let gz2 = gzip_of("b", &gzip_of("a", &noise));
    let n = unwrap(&gz2);
    assert_eq!(formats(&n), ["GZIP deflate", "GZIP deflate"]);
    same(&n.inner, &noise);
    let three = gzip_of("c", &gzip_of("b", &gzip_of("a", &noise)));
    let n = unwrap(&three);
    let decoded: usize = gz2.len() + gzip_of("a", &noise).len() + n.inner.len();
    assert!(decoded <= MAX_DECODED + 1000, "{decoded}");
    assert!(n.inner.len() < noise.len());
}

#[test]
fn methods_not_decoded_yet_say_so() {
    let v = video();
    let fields =
        [&vint(0)[..], &vint(v.len() as u64), &vint(0x20), &vint(3 << 7), &[0], &vint(9), b"movie.mkv"].concat();
    let r = rar5(&[r5_main(), r5_block(2, &fields, &[], Some(&v)), r5_end()]);
    let n = unwrap(&r);
    assert_eq!(formats(&n), ["RAR5 m3"]);
    assert_eq!(n.layers[0].stop.as_deref(), Some("no decoder for this yet"));
    let mut bz = b"BZh91AY&SY".to_vec();
    bz.extend([0; 20]);
    let n = unwrap(&bz);
    assert_eq!(n.layers[0].format, "BZIP2");
    assert_eq!(n.layers[0].stop.as_deref(), Some("no decoder for this yet"));
    let z = zip_entry(12, "movie.mkv", &[1, 2, 3], 3);
    let n = unwrap(&z);
    assert_eq!(formats(&n), ["ZIP bzip2"]);
    assert_eq!(n.layers[0].stop.as_deref(), Some("no decoder for this yet"));
}

#[test]
fn single_streams() {
    let v = video();
    let lz = lzma_alone_of(&v);
    same(&unwrap(&lz).inner, &v);
    let mut lzip = b"LZIP\x01\x17".to_vec();
    lzip.extend(&lz[13..]);
    let n = unwrap(&lzip);
    assert_eq!(formats(&n), ["LZIP lzma"]);
    same(&n.inner, &v);
    let z = zip_entry(93, "movie.mkv", &zstd::encode_all(&v[..], 3).unwrap(), v.len());
    same(&unwrap(&z).inner, &v);
    // python's xz, with real matches
    let xz = include_bytes!("../../../tests/fixtures/nest/sample.xz");
    let n = unwrap(xz);
    assert_eq!(formats(&n), ["XZ lzma2"]);
    assert_eq!(n.inner.len(), 34_400);
    // nothing to unwrap
    let n = unwrap(&v);
    assert!(n.layers.is_empty());
    assert_eq!(n.outer_at, Some(0));
}

#[test]
fn the_biggest_file_is_the_one_followed() {
    let v = video();
    let r = rar4(&[r4_main(0), r4_file(0, b"a.nfo", b"hello"), r4_file(0, b"movie.mkv", &v)]);
    let n = unwrap(&r);
    assert_eq!(n.layers[0].name.as_deref(), Some("movie.mkv"));
    let t = [tar_of("a.nfo", b"hi").split_at(1024).0.to_vec(), tar_of("movie.mkv", &v)].concat();
    let n = unwrap(&t);
    assert_eq!(n.layers[0].name.as_deref(), Some("movie.mkv"));
    same(&n.inner, &v);
}

#[test]
fn layers_read_back_from_their_json() {
    let data = ten_deep();
    let n = unwrap(&data);
    let json = serde_json::to_string(&n.layers).unwrap();
    let back: Vec<Layer> = serde_json::from_str(&json).unwrap();
    // `continues` isn't kept
    let kept: Vec<Layer> = n.layers.iter().map(|l| Layer { continues: false, ..l.clone() }).collect();
    assert_eq!(back, kept);
}

#[test]
fn thousands_of_7z_wrappers_stay_within_the_depth_limit() {
    // checking whether a 7z holds another must not open that one too: the stack would be the limit
    let mut data = video();
    for _ in 0..5000 {
        let mut z = b"7z\xbc\xaf\x27\x1c\x00\x04".to_vec();
        z.extend([0; 4]);
        z.extend((data.len() as u64).to_le_bytes());
        z.extend([0; 12]);
        z.extend(data);
        data = z;
    }
    let n = unwrap(&data);
    assert_eq!(n.layers.len(), MAX_DEPTH + 1);
}

#[test]
fn headers_cut_before_their_data_stop_cleanly() {
    let n = unwrap(b"LZIP\x01");
    assert_eq!(n.layers[0].stop.as_deref(), Some("no file starts in what was read"));
    // a zip LZMA entry holding only the start of its own header
    let z = zip_entry(14, "a", &[9, 0, 5, 0, 0x5d], 100);
    let n = unwrap(&z);
    assert_eq!(formats(&n), ["ZIP lzma"]);
    assert!(n.layers[0].stop.is_some());
    for cut in 0..z.len() {
        let _ = unwrap(&z[..cut]);
    }
}
