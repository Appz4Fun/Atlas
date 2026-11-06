//! Audio fingerprints: a few seconds of a release's sound, decoded and turned into Chromaprint's items (one 32-bit
//! word every 1365 samples at 11025 Hz, about 0.124 s), soo two releases of the same film or episode can be found
//! whatever codec, bit rate or container each was encoded to. Two prints are the same sound when, lined up, their
//! items differ in fewer than 6 of 32 bits on average over 5 seconds or more.
//!
//! A print is only ever a candidate for a reader, like every key in `keys`: never a merge.

pub mod decode;
pub mod mkv;

use rusty_chromaprint::{Configuration, Fingerprinter};

/// An aligned item differing in fewer than this many of its 32 bits, on average, is the same sound. Chromaprint's own
/// matcher takes 10, at the one alignment its hash histogram picks; every offset is tried here, and the best of so
/// many is lower by chance, soo the bar is lower too. The same sound measures 0.6 bits across AAC and FLAC and 2 at
/// the worst offset of the item grid; a tune of the same rhythm and other notes, its best offset of all, 10; unrelated
/// sound about 16.
pub const MATCH_BITS: u32 = 6;
/// The fewest items two prints must overlap by to be compared: about 5 seconds.
pub const MIN_OVERLAP: usize = 40;
/// The most items a print keeps: about 30 seconds.
pub const MAX_ITEMS: usize = 256;
/// Items of a print kept in the index, by value: the ones with the smallest hash, the same items whatever the print's
/// start, soo two prints of the same sound read from a slightly different place share some.
pub const ANCHORS: usize = 24;
/// Silence and a steady tone give the same item over and over. A print with fewer different items than this says
/// too little to match on.
const MIN_DIFFERENT: usize = 24;

fn config() -> Configuration {
    Configuration::preset_test2()
}

/// One item's length in ms.
pub fn item_ms() -> f64 {
    f64::from(config().item_duration_in_seconds()) * 1000.0
}

/// The print of one channel of sound at `rate`. None when it is too short for a print, or the fingerprinter refuses
/// it (it panics on what it can't take: that is caught here).
pub fn fingerprint(samples: &[i16], rate: u32) -> Option<Vec<u32>> {
    if !(8_000..=192_000).contains(&rate) || samples.is_empty() {
        return None;
    }
    let items = std::panic::catch_unwind(|| {
        let config = config();
        let mut printer = Fingerprinter::new(&config);
        printer.start(rate, 1).ok()?;
        for chunk in samples.chunks(4096) {
            printer.consume(chunk);
        }
        printer.finish();
        Some(printer.fingerprint().to_vec())
    })
    .ok()
    .flatten()?;
    let mut items = items;
    items.truncate(MAX_ITEMS);
    (items.len() >= MIN_OVERLAP).then_some(items)
}

/// Whether a print says enough to match on: not silence, not one tone.
pub fn informative(items: &[u32]) -> bool {
    let mut seen: Vec<u32> = items.to_vec();
    seen.sort_unstable();
    seen.dedup();
    seen.len() >= MIN_DIFFERENT
}

/// A print's items, as stored: 4 bytes each, little-endian.
pub fn to_blob(items: &[u32]) -> Vec<u8> {
    items.iter().flat_map(|i| i.to_le_bytes()).collect()
}

pub fn from_blob(blob: &[u8]) -> Vec<u32> {
    blob.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).take(MAX_ITEMS).collect()
}

/// splitmix64's finaliser: spreads an item's bits, soo the anchors are any items, not the small ones.
fn mix(v: u32) -> u64 {
    let mut x = u64::from(v) ^ 0x9e37_79b9_7f4a_7c15;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// The items of a print kept in the index: different ones that don't repeat (silence and steady tones do, in every
/// release), the `ANCHORS` with the smallest hash. As the integers the index keeps.
pub fn anchors(items: &[u32]) -> Vec<i64> {
    let mut counts: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for i in items {
        *counts.entry(*i).or_default() += 1;
    }
    let mut kept: Vec<u32> = counts.into_iter().filter(|(_, n)| *n < 3).map(|(i, _)| i).collect();
    kept.sort_unstable_by_key(|i| (mix(*i), *i));
    kept.truncate(ANCHORS);
    kept.into_iter().map(i64::from).collect()
}

/// How two prints line up best: the mean bits an item differs in, the offset of `b` against `a` in items, and how
/// many items overlap there. Every offset with at least `MIN_OVERLAP` items in common is tried. None when the prints
/// are too short to overlap that much.
pub fn best_alignment(a: &[u32], b: &[u32]) -> Option<(f64, isize, usize)> {
    let mut best: Option<(f64, isize, usize)> = None;
    let (la, lb) = (a.len() as isize, b.len() as isize);
    for offset in -(lb - 1)..la {
        // a[i] against b[i - offset]
        let from = offset.max(0);
        let to = la.min(lb + offset);
        let n = (to - from).max(0) as usize;
        if n < MIN_OVERLAP {
            continue;
        }
        let bits: u32 =
            (from..to).filter_map(|i| Some((a.get(i as usize)? ^ b.get((i - offset) as usize)?).count_ones())).sum();
        let mean = f64::from(bits) / n as f64;
        if best.is_none_or(|(m, _, _)| mean < m) {
            best = Some((mean, offset, n));
        }
    }
    best
}

/// Whether two prints are the same sound: lined up, fewer than `MATCH_BITS` bits apart an item over `MIN_OVERLAP`
/// items or more. Neither may be silence or one tone.
pub fn same_sound(a: &[u32], b: &[u32]) -> bool {
    informative(a) && informative(b) && best_alignment(a, b).is_some_and(|(mean, _, _)| mean < f64::from(MATCH_BITS))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A tune: a two-note chord every quarter to half second, picked by `seed`, with a little noise.
    pub(crate) fn tune(seed: u64, secs: f64, rate: u32) -> Vec<i16> {
        let mut s = seed | 1;
        let mut rnd = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        let mut notes = Vec::new();
        let mut t = 0.0;
        while t < secs + 1.0 {
            let len = 0.25 + rnd() * 0.25;
            let f1 = 110.0 * 2f64.powf((rnd() * 36.0).floor() / 12.0);
            let f2 = 110.0 * 2f64.powf((rnd() * 36.0).floor() / 12.0);
            notes.push((t, len, f1, f2));
            t += len;
        }
        let n = (secs * f64::from(rate)) as usize;
        let mut note = 0;
        (0..n)
            .map(|i| {
                let t = i as f64 / f64::from(rate);
                while note + 1 < notes.len() && notes[note + 1].0 <= t {
                    note += 1;
                }
                let (start, len, f1, f2) = notes[note];
                let env = 1.0 - 0.8 * ((t - start) / len).min(1.0);
                let tau = 2.0 * std::f64::consts::PI * t;
                let v = (0.4 * (tau * f1).sin() + 0.25 * (tau * f2).sin()) * env + (rnd() - 0.5) * 0.02;
                (v.clamp(-1.0, 1.0) * 32767.0) as i16
            })
            .collect()
    }

    /// A rising tone across the hearing range.
    fn chirp(secs: f64, rate: u32) -> Vec<i16> {
        let n = (secs * f64::from(rate)) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / f64::from(rate);
                let phase = 2.0 * std::f64::consts::PI * (100.0 * t + 150.0 * t * t);
                (phase.sin() * 0.5 * 32767.0) as i16
            })
            .collect()
    }

    fn cut(samples: &[i16], rate: u32, from: f64, secs: f64) -> &[i16] {
        let a = (from * f64::from(rate)) as usize;
        &samples[a..a + (secs * f64::from(rate)) as usize]
    }

    #[test]
    fn a_print_is_the_same_every_time() {
        let sound = chirp(12.0, 44_100);
        let a = fingerprint(&sound, 44_100).unwrap();
        assert_eq!(a, fingerprint(&sound, 44_100).unwrap());
        assert!(a.len() >= 70, "12 s gives about 75 items: {}", a.len());
        let t = tune(7, 12.0, 48_000);
        assert_eq!(fingerprint(&t, 48_000), fingerprint(&t, 48_000));
        assert_eq!(from_blob(&to_blob(&a)), a);
        assert_eq!(anchors(&a), anchors(&a));
    }

    #[test]
    fn the_same_sound_a_little_later_matches() {
        let rate = 44_100;
        let sound = tune(7, 40.0, rate);
        let a = fingerprint(cut(&sound, rate, 10.0, 12.0), rate).unwrap();
        // half an item off is the worst case for the item grid
        for shift in [0.062, 0.37, 1.0, 2.5] {
            let b = fingerprint(cut(&sound, rate, 10.0 + shift, 12.0), rate).unwrap();
            let (mean, offset, _) = best_alignment(&a, &b).unwrap();
            assert!(same_sound(&a, &b), "shift {shift}: {mean} bits");
            let expected = (shift * 1000.0 / item_ms()).round() as isize;
            assert!((offset - expected).abs() <= 1, "shift {shift}: offset {offset}, expected {expected}");
            let shared = anchors(&a).iter().filter(|x| anchors(&b).contains(x)).count();
            assert!(shared > 0, "shift {shift}: no anchor in common");
        }
        // another rate, a little noise: still the same sound
        let other_rate = 22_050;
        let resampled: Vec<i16> = (0..(40.0 * f64::from(other_rate)) as usize)
            .map(|i| (sound[i * 2] / 2 + sound[i * 2 + 1] / 2) ^ (i as i16 & 3))
            .collect();
        let b = fingerprint(cut(&resampled, other_rate, 10.4, 12.0), other_rate).unwrap();
        assert!(same_sound(&a, &b));
    }

    #[test]
    fn different_sound_does_not_match() {
        let rate = 44_100;
        let a = fingerprint(cut(&tune(7, 30.0, rate), rate, 10.0, 12.0), rate).unwrap();
        for seed in [8, 99, 12345] {
            let b = fingerprint(cut(&tune(seed, 30.0, rate), rate, 10.0, 12.0), rate).unwrap();
            let (mean, _, _) = best_alignment(&a, &b).unwrap();
            assert!(!same_sound(&a, &b), "seed {seed}: {mean} bits");
            assert!(mean > f64::from(MATCH_BITS) + 1.0, "seed {seed}: {mean} bits");
        }
        let c = fingerprint(&chirp(12.0, rate), rate).unwrap();
        assert!(!same_sound(&a, &c));
    }

    #[test]
    fn silence_and_short_sound_give_nothing_to_match() {
        let quiet = vec![0i16; 12 * 44_100];
        let print = fingerprint(&quiet, 44_100).unwrap();
        assert!(!informative(&print));
        assert!(!same_sound(&print, &print), "silence matches nothing, itself included");
        assert!(anchors(&print).is_empty(), "silence keeps no anchor");
        assert_eq!(fingerprint(&quiet[..44_100], 44_100), None, "a second is too short");
        assert_eq!(fingerprint(&[], 44_100), None);
        assert_eq!(fingerprint(&quiet, 0), None);
        assert_eq!(fingerprint(&quiet, 5_000_000), None);
        assert_eq!(best_alignment(&[1, 2, 3], &[1, 2, 3]), None);
        assert!(from_blob(&[1, 2, 3]).is_empty());
    }
}
