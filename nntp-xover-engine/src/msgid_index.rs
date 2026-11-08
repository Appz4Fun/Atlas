//! `OVER <message-id>` without an article number: an 8-bit XOR filter per
//! block answers "not here" in three memory reads, and a 256-bucket CSR
//! table of (tag, row) pairs finds the row inside the block.
//!
//! Both are read straight out of the mapped block; see `container` for where
//! they sit.

use crate::container::{align_up, cast_slice};

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(C, align(8))]
pub struct Xor8Header {
    pub seed: u64,
    pub block_length: u32,
    pub fingerprint_len: u32,
}

pub const XOR8_HEADER_LEN: usize = std::mem::size_of::<Xor8Header>();
const _: () = assert!(XOR8_HEADER_LEN == 16);

#[inline(always)]
pub fn msgid_hash(raw_msgid: &[u8]) -> u64 {
    xxhash_rust::xxh3::xxh3_64(raw_msgid)
}

#[inline(always)]
fn murmur64(mut h: u64) -> u64 {
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^ (h >> 33)
}

#[inline(always)]
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[inline(always)]
fn fastrange32(x: u32, n: u32) -> u32 {
    ((x as u64 * n as u64) >> 32) as u32
}

#[inline(always)]
fn fingerprint(h: u64) -> u8 {
    (h ^ (h >> 32)) as u8
}

#[inline(always)]
fn positions(h: u64, b: u32) -> [usize; 3] {
    [
        fastrange32(h as u32, b) as usize,
        (fastrange32(h.rotate_left(21) as u32, b) + b) as usize,
        (fastrange32(h.rotate_left(42) as u32, b) + 2 * b) as usize,
    ]
}

/// Builds Xor8 filters by hypergraph peeling.
pub struct Xor8Builder;

impl Xor8Builder {
    pub fn block_length(n: usize) -> u32 {
        ((1.23 * n as f64) as u32 + 32) / 3
    }

    /// The filter section (header then fingerprints, padded to 8) for these
    /// key hashes. Duplicates are dropped first; peeling needs distinct keys.
    pub fn build(hashes: &[u64]) -> Vec<u8> {
        let mut keys = hashes.to_vec();
        keys.sort_unstable();
        keys.dedup();
        let b = Self::block_length(keys.len());
        let cap = 3 * b as usize;
        let mut rng = 0x6e78_7638_5843_4f56u64; // deterministic: the same rows give the same bytes
        let mut xormask = vec![0u64; cap];
        let mut count = vec![0u32; cap];
        let mut queue: Vec<usize> = Vec::with_capacity(cap);
        let mut stack: Vec<(u64, usize)> = Vec::with_capacity(keys.len());
        let seed = loop {
            let seed = splitmix64(&mut rng);
            xormask.fill(0);
            count.fill(0);
            queue.clear();
            stack.clear();
            for &k in &keys {
                let h = murmur64(k.wrapping_add(seed));
                for p in positions(h, b) {
                    xormask[p] ^= h;
                    count[p] += 1;
                }
            }
            queue.extend((0..cap).filter(|&i| count[i] == 1));
            while let Some(i) = queue.pop() {
                if count[i] != 1 {
                    continue;
                }
                let h = xormask[i];
                stack.push((h, i));
                for p in positions(h, b) {
                    xormask[p] ^= h;
                    count[p] -= 1;
                    if count[p] == 1 {
                        queue.push(p);
                    }
                }
            }
            if stack.len() == keys.len() {
                break seed;
            }
        };
        let mut fp = vec![0u8; cap];
        for &(h, i) in stack.iter().rev() {
            let [a, bb, c] = positions(h, b);
            fp[i] = 0;
            fp[i] = fingerprint(h) ^ fp[a] ^ fp[bb] ^ fp[c];
        }
        let header = Xor8Header { seed, block_length: b, fingerprint_len: cap as u32 };
        let mut out = Vec::with_capacity(align_up(XOR8_HEADER_LEN + cap, 8));
        out.extend_from_slice(&header.seed.to_le_bytes());
        out.extend_from_slice(&header.block_length.to_le_bytes());
        out.extend_from_slice(&header.fingerprint_len.to_le_bytes());
        out.extend_from_slice(&fp);
        out.resize(align_up(out.len(), 8), 0);
        out
    }
}

/// The filter inside a mapped section.
#[derive(Copy, Clone)]
pub struct Xor8View<'a> {
    pub header: &'a Xor8Header,
    pub fingerprints: &'a [u8],
}

impl<'a> Xor8View<'a> {
    pub fn from_section(sec: &'a [u8]) -> Option<Self> {
        let header: &Xor8Header = cast_slice::<Xor8Header>(sec.get(..XOR8_HEADER_LEN)?)?.first()?;
        let fp = sec.get(XOR8_HEADER_LEN..XOR8_HEADER_LEN + header.fingerprint_len as usize)?;
        if header.block_length == 0 || fp.len() != 3 * header.block_length as usize {
            return None;
        }
        Some(Self { header, fingerprints: fp })
    }

    #[inline(always)]
    pub fn contains_hash(&self, key: u64) -> bool {
        let h = murmur64(key.wrapping_add(self.header.seed));
        let [a, b, c] = positions(h, self.header.block_length);
        let fp = self.fingerprints;
        // positions are < 3 * block_length == fp.len() by construction
        fingerprint(h) == fp[a] ^ fp[b] ^ fp[c]
    }
}

pub const CSR_BUCKETS: usize = 256;
const CSR_OFFSETS_LEN: usize = (CSR_BUCKETS + 1) * 4;
const CSR_OFFSETS_PADDED: usize = (CSR_OFFSETS_LEN + 7) & !7;

#[inline(always)]
fn bucket(h: u64) -> usize {
    (h >> 56) as usize
}

#[inline(always)]
fn tag(h: u64) -> u8 {
    (h >> 48) as u8
}

/// The bucket index section for (hash, row) pairs: `[u32; 257]` bucket
/// offsets, `N` tags (padded to 8), then `N` u16 rows.
pub fn build_bucket_index(entries: &[(u64, u16)]) -> Vec<u8> {
    let mut sorted: Vec<(u64, u16)> = entries.to_vec();
    sorted.sort_by_key(|&(h, row)| (bucket(h), row));
    let mut offsets = [0u32; CSR_BUCKETS + 1];
    for &(h, _) in &sorted {
        offsets[bucket(h) + 1] += 1;
    }
    for i in 0..CSR_BUCKETS {
        offsets[i + 1] += offsets[i];
    }
    let n = sorted.len();
    let mut out = Vec::with_capacity(CSR_OFFSETS_PADDED + align_up(n, 8) + 2 * n + 8);
    for o in offsets {
        out.extend_from_slice(&o.to_le_bytes());
    }
    out.resize(CSR_OFFSETS_PADDED, 0);
    out.extend(sorted.iter().map(|&(h, _)| tag(h)));
    out.resize(CSR_OFFSETS_PADDED + align_up(n, 8), 0);
    for &(_, row) in &sorted {
        out.extend_from_slice(&row.to_le_bytes());
    }
    out.resize(align_up(out.len(), 8), 0);
    out
}

#[derive(Copy, Clone)]
pub struct BucketIndexView<'a> {
    offsets: &'a [u32],
    tags: &'a [u8],
    rows: &'a [u16],
}

impl<'a> BucketIndexView<'a> {
    pub fn from_section(sec: &'a [u8], n: usize) -> Option<Self> {
        let offsets = cast_slice::<u32>(sec.get(..CSR_OFFSETS_LEN)?)?;
        let tags = sec.get(CSR_OFFSETS_PADDED..CSR_OFFSETS_PADDED + n)?;
        let rs = CSR_OFFSETS_PADDED + align_up(n, 8);
        let rows = cast_slice::<u16>(sec.get(rs..rs + 2 * n)?)?;
        if offsets[CSR_BUCKETS] as usize != n {
            return None;
        }
        Some(Self { offsets, tags, rows })
    }

    /// Calls `check(row)` for each row whose tag matches, in row order, and
    /// returns the first row it accepts.
    #[inline]
    pub fn find(&self, hash: u64, mut check: impl FnMut(usize) -> bool) -> Option<usize> {
        let b = bucket(hash);
        let lo = self.offsets[b] as usize;
        let hi = (self.offsets[b + 1] as usize).min(self.tags.len());
        let t = tag(hash);
        let tags = self.tags.get(lo..hi)?;
        for (i, &x) in tags.iter().enumerate() {
            if x == t {
                let row = self.rows[lo + i] as usize;
                if check(row) {
                    return Some(row);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::AlignedBytes;

    #[test]
    fn xor8_has_no_false_negatives_and_few_false_positives() {
        let keys: Vec<u64> = (0..10_000u64).map(|i| msgid_hash(format!("<{i}@x>").as_bytes())).collect();
        let sec = AlignedBytes::from_slice(&Xor8Builder::build(&keys));
        let f = Xor8View::from_section(&sec).unwrap();
        assert!(keys.iter().all(|&k| f.contains_hash(k)));
        let fp = (0..100_000u64).filter(|i| f.contains_hash(msgid_hash(format!("<n{i}@y>").as_bytes()))).count();
        assert!(fp <= 450, "{fp}");
        let empty = AlignedBytes::from_slice(&Xor8Builder::build(&[]));
        assert!(Xor8View::from_section(&empty).is_some());
    }

    #[test]
    fn buckets_find_rows() {
        let entries: Vec<(u64, u16)> = (0..5000u16).map(|i| (msgid_hash(format!("<{i}@x>").as_bytes()), i)).collect();
        let sec = AlignedBytes::from_slice(&build_bucket_index(&entries));
        let v = BucketIndexView::from_section(&sec, entries.len()).unwrap();
        for &(h, r) in &entries {
            assert_eq!(v.find(h, |row| row == r as usize), Some(r as usize));
        }
    }
}
