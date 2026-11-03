use crc32gf2::*;
use std::time::Instant;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 { let mut x = self.0; x ^= x << 13; x ^= x >> 7; x ^= x << 17; self.0 = x; x }
    fn below(&mut self, n: u64) -> u64 { if n == 0 { 0 } else { self.next() % n } }
    fn bytes_n(&mut self, n: usize) -> Vec<u8> { (0..n).map(|_| self.next() as u8).collect() }
    fn len(&mut self) -> usize {
        // mix of tiny, edge, and medium lengths
        match self.below(4) { 0 => self.below(3) as usize, 1 => self.below(64) as usize, 2 => self.below(5000) as usize, _ => self.below(70000) as usize }
    }
}

const ONE: u32 = 1 << 31;

#[test]
fn reference_matches_crc32fast() {
    let mut r = Rng(0x9E3779B97F4A7C15);
    assert_eq!(crc32(b"123456789"), 0xCBF43926);
    assert_eq!(crc32(b""), 0);
    for _ in 0..500 { let d = { let n = r.len(); r.bytes_n(n) }; assert_eq!(crc32(&d), crc32fast::hash(&d)); }
}

#[test]
fn algebra_facts() {
    assert_eq!(mulmod(1 << 30, XINV), ONE, "x * x^-1 == 1");
    // order of x divides 2^32-1: x^(2^32-1) == 1  -> table period 32 is valid
    let mut p = ONE; let mut base = 1u32 << 30;
    let mut e: u64 = (1u64 << 32) - 1;
    while e != 0 { if e & 1 != 0 { p = mulmod(p, base); } base = mulmod(base, base); e >>= 1; }
    assert_eq!(p, ONE);
    let mut r = Rng(7);
    for _ in 0..2000 { let n = r.next() >> r.below(64); assert_eq!(mulmod(x8n(n), x8n_inv(n)), ONE, "n={n}"); }
    for _ in 0..200000 { let (a, b) = (r.next() as u32, r.next() as u32); assert_eq!(mulmod_fast(a, b), mulmod(a, b)); }
    assert_eq!(x8n(0), ONE); assert_eq!(x8n_inv(0), ONE);
}

#[test]
fn combine_remove_props() {
    let mut r = Rng(0xDEADBEEFCAFEBABE);
    for _ in 0..3000 {
        let a = { let n = r.len(); r.bytes_n(n) }; let b = { let n = r.len(); r.bytes_n(n) };
        let mut ab = a.clone(); ab.extend_from_slice(&b);
        let (ca, cb, cab) = (crc32(&a), crc32(&b), crc32(&ab));
        let lb = b.len() as u64;
        assert_eq!(combine(ca, cb, lb), cab, "combine la={} lb={}", a.len(), lb);
        assert_eq!(remove_prefix(cab, ca, lb), cb, "remove_prefix");
        assert_eq!(remove_suffix(cab, cb, lb), ca, "remove_suffix");
    }
}

#[test]
fn zero_pad_unpad_props() {
    let mut r = Rng(12345);
    for _ in 0..3000 {
        let d = { let n = r.len(); r.bytes_n(n) }; let n = r.len();
        let mut dz = d.clone(); dz.resize(d.len() + n, 0);
        let (cd, cdz) = (crc32(&d), crc32(&dz));
        assert_eq!(zero_extend(cd, n as u64), cdz);
        assert_eq!(zero_unpad(cdz, n as u64), cd);
        assert_eq!(crc_zeros(n as u64), crc32(&vec![0u8; n]));
    }
    // huge n round trip (no direct check possible)
    for _ in 0..1000 { let c = r.next() as u32; let n = r.next() >> 20; assert_eq!(zero_unpad(zero_extend(c, n), n), c); }
}

fn split_parts(data: &[u8], r: &mut Rng, art: Option<usize>) -> Vec<(u64, u64, u32)> {
    let mut parts = Vec::new(); let mut pos = 0usize;
    while pos < data.len() {
        let len = match art { Some(a) => a, None => 1 + r.below(5000) as usize }.min(data.len() - pos);
        parts.push((pos as u64 + 1, (pos + len) as u64, crc32(&data[pos..pos + len])));
        pos += len;
    }
    parts
}

fn par2_slices(data: &[u8], s: usize) -> Vec<u32> {
    data.chunks(s).map(|c| { let mut v = c.to_vec(); v.resize(s, 0); crc32(&v) }).collect()
}

#[test]
fn yenc_par2_fold_props() {
    let mut r = Rng(0xABCDEF);
    for _ in 0..400 {
        let data = { let n = r.len(); r.bytes_n(n) };
        let whole = crc32(&data);
        let parts = split_parts(&data, &mut r, None);
        assert_eq!(fold_yenc_parts(&parts).unwrap(), (data.len() as u64, whole));
        let s = 1 + r.below(9000) as usize;
        let sl = par2_slices(&data, s);
        assert_eq!(fold_par2_slices(data.len() as u64, s as u64, &sl).unwrap(), whole, "len={} s={}", data.len(), s);
    }
    assert_eq!(fold_yenc_parts(&[]).unwrap(), (0, 0));
    assert_eq!(fold_par2_slices(0, 100, &[]).unwrap(), 0);
    assert!(matches!(fold_yenc_parts(&[(1, 10, 0), (12, 20, 0)]), Err(FoldError::Gap { .. })));
    assert!(matches!(fold_par2_slices(10, 4, &[0, 0]), Err(FoldError::SliceCount { .. })));
}

#[test]
fn slices_from_parts_props() {
    let mut r = Rng(0x5151);
    for _ in 0..400 {
        let data = { let n = r.len(); r.bytes_n(n) };
        let art = 1 + r.below(3000) as usize;
        let m = 1 + r.below(6) as usize;
        let s = art * m;
        let parts = split_parts(&data, &mut r, Some(art));
        assert_eq!(slices_from_parts(&parts, s as u64).unwrap(), par2_slices(&data, s), "len={} art={} m={}", data.len(), art, m);
    }
    // non-aligned slice size must be rejected
    assert!(matches!(slices_from_parts(&[(1, 10, 0), (11, 20, 0)], 15), Err(FoldError::Straddle { .. })));
}

#[test]
fn bench_large_n() {
    let mut r = Rng(99);
    let ns: Vec<u64> = (0..100_000).map(|_| 10_000_000_000 + r.below(1 << 30)).collect();
    let iters = ns.len() as f64;
    let mut acc = 0u32;
    let t = Instant::now(); for &n in &ns { acc = combine(acc, n as u32, n); } let a = t.elapsed();
    let t = Instant::now(); for &n in &ns { acc ^= remove_suffix(acc, n as u32, n); } let b = t.elapsed();
    let t = Instant::now(); for &n in &ns { acc ^= zero_unpad(acc, n); } let c = t.elapsed();
    let s = 1u64 << 20; let parts: Vec<u32> = (0..9537).map(|i| i as u32 * 2654435761).collect(); // ~10 GB of 1 MiB slices
    let t = Instant::now(); let w = fold_par2_slices(9537 * s - 123, s, &parts).unwrap(); let d = t.elapsed();
    let yparts: Vec<(u64,u64,u32)> = (0..14000u64).map(|i| (i*716800+1, (i+1)*716800, i as u32)).collect(); // ~10 GB, 700 KiB articles
    let t = Instant::now(); let y = fold_yenc_parts(&yparts).unwrap(); let e = t.elapsed();
    let t = Instant::now(); let sl = slices_from_parts(&yparts, 716800*4).unwrap(); let f = t.elapsed();
    eprintln!("BENCH fold 14000 yEnc parts (~10GB) {e:?}; slices_from_parts m=4 -> {} slices {f:?} [{:08x}]", sl.len(), y.1);
    eprintln!("BENCH n~1e10: combine {:.0} ns/op, remove_suffix {:.0} ns/op, zero_unpad {:.0} ns/op; fold 9537 x 1MiB PAR2 slices (~10GB) {:?} [{acc:08x} {w:08x}]",
        a.as_nanos() as f64 / iters, b.as_nanos() as f64 / iters, c.as_nanos() as f64 / iters, d);
}

#[test]
fn large_n_direct_over_4gib() {
    // Stream 5e9 bytes (> 2^32) through crc32fast and compare against O(log n) results.
    let n: u64 = 5_000_000_000;
    let head = b"usenet-binary-head";
    let chunk = vec![0u8; 1 << 22];
    let mut h = crc32fast::Hasher::new(); h.update(head);
    let mut left = n; while left > 0 { let k = left.min(chunk.len() as u64) as usize; h.update(&chunk[..k]); left -= k as u64; }
    let tail = b"tail";
    h.update(tail);
    let direct = h.finalize();
    let ch = crc32(head);
    let padded = zero_extend(ch, n);
    assert_eq!(combine(padded, crc32(tail), 4), direct);
    assert_eq!(remove_prefix(direct, ch, n + 4), combine(crc_zeros(n), crc32(tail), 4));
    assert_eq!(zero_unpad(remove_suffix(direct, crc32(tail), 4), n), ch);
    eprintln!("LARGE 5e9 zeros direct crc = {direct:08x} (matches)");
}
