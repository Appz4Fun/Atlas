# Wave 10A: Node 4 (V8 4.5) MWC1616 recovery of stock Nyuu Message-IDs

## Result
**MWC1616 is confirmed for part of the 2017-2019 corpus.** 8 of the old NZBs that failed under xorshift128+ now link **100%** into one Math.random stream each, about 0.1 s per NZB:

| first post | NZB | IDs | linked | brute-force s | gap histogram (draws between consecutive IDs) |
|---|---|---|---|---|---|
| 2017-07-28 | reftestnzb-666MB-...plus-nonsense | 1105 | 1105 (1104 + 1 in a second group) | 0.06 | 24:116 48:895 72:73 96:16 120:3 |
| 2018-08-30 | chinese-chars-in-rar-par-filenames | 13 | 13 | 0.08 | 24:3 48:8 72:1 |
| 2018-09-14 | reftestnzb-subdirs | 288 | 288 | 0.03 | 24:5 48:279 72:3 |
| 2018-12-26 | reftestnzb 1GB - broken third rar | 1654 | 1654 | 0.04 | 24:534 48:754 72:196 96:169 |
| 2019-02-06 | chinese-chars-only-in-resulting-file | 88 | 88 | 0.04 | 24:8 48:75 72:2 96:2 |
| 2019-02-15 | with-passsssword | 604 | 604 | 0.02 | 24:112 48:411 72:59 96:14 120:4 144:1 168:2 |
| 2019-02-16 | long-dirfilename-1000plus | 170 | 170 | 0.04 | 24:82 48:40 72:14 96:33 |
| 2019-02-23 | obfuscated-stuff-200MB | 297 | 297 | 0.04 | 24:4 48:290 72:2 |

Every gap is a multiple of 24, which matches whole discarded or retried IDs. 48 (one discarded build-time ID) is the most common gap.

**Not MWC1616 (0 candidates on all 8 IDs tried per NZB):** 2018-03-11/12 (ASCII-Japanese x2, ecoji), 2018-04-17 (my_nzb x2), and every NZB from 2019-06-15 to 2023-04-03 (~55). These posters must have run Nyuu from npm on their own newer Node. Next step: run the w8a old-V8 s0+s1 (Node 6-11) model on them.
**Negative control:** all 2023-05-26+ NZBs (xorshift128+, solved in w6a) also give 0 candidates, so the filter gives no false positives in practice.

The poster switched Node runtime twice, in 2018-03/04 and around 2019-06. So the PRNG model is itself a time fingerprint.

## 1. Source verification
- Node v4.9.1 `deps/v8/include/v8-version.h`: **V8 4.5.103.53**. Nyuu v0.3.1/0.3.6/0.3.8 bundle Node 4.7.3/4.8.3/4.9.1 (w9a), so all of them run V8 4.5.
- v8 tag 4.5.103.35 `src/math.js` (identical algorithm):
```js
var rngstate;  // Initialized to a Uint32Array during genesis.
function MathRandom() {
  var r0 = (MathImul(18030, rngstate[0] & 0xFFFF) + (rngstate[0] >>> 16)) | 0;
  rngstate[0] = r0;
  var r1 = (MathImul(36969, rngstate[1] & 0xFFFF) + (rngstate[1] >>> 16)) | 0;
  rngstate[1] = r1;
  var x = ((r0 << 16) + (r1 & 0xFFFF)) | 0;
  return (x < 0 ? (x + 0x100000000) : x) * 2.3283064365386962890625e-10;
}
```
- State: a 2-word `rngstate` Uint32Array per native context. There is **no cache/array of precomputed values**, unlike the 64-entry xorshift cache in later V8. Seeding is in `bootstrapper.cc`: `isolate()->random_number_generator()->NextBytes(state, ...)` per context, from the isolate RNG (urandom, or --random-seed).
- Nyuu v0.3.8 `lib/article.js` `Post.prototype.createMessageId`: `String.fromCharCode(65 + Math.random()*26, 97 + Math.random()*26, ...)` (12 pairs). fromCharCode does ToUint16, so letter = floor(x*26). The arguments are evaluated left to right, so the 24 draws are consecutive.

## 2. Why r0 alone decides the letters
`(r0<<16)` has 16 zero low bits and `r1 & 0xFFFF < 2^16`, so the add has **no carry**. The top 16 bits of x are exactly low16(r0). So letter = floor((low16(r0)·2^16 + t)·26 / 2^32) with t = low16(r1) unknown. That is fixed by r0, except when a letter boundary falls inside the 2^16-wide t window (about 26·2^16/2^32 ≈ 0.04 % per draw). The solver accepts any letter in [lo, hi] for that window. After any step r0 = 18030·lo + hi < 18030·2^16 ≈ 2^30.1, so the search over r0 (the state after the ID's first draw) is H < 18030 × L in the ~2521 low halves consistent with letter 1, about 45 M candidates. The first 2-3 letters reject almost all of them. r1 cannot be recovered and is not needed for linking.

## 3. Method and code (nyuu-id-solver/mwc1616/)
- `mwc_link.c` (gcc -O3 -pthread; arg = threads). It sorts IDs by timestamp and brute-forces r0 for the earliest unlinked ID (unique solution each time). It then generates 200·N+20000 forward r0 states, indexes them by a 6-letter nominal-prefix hash, and confirms each remaining ID with the tolerant 24-letter check. It repeats for any IDs left over (new group), and gives up after 8 zero-candidate IDs. It prints groups, linked counts, timings and a gap histogram.
- `synth.py N seed` is an exact Python port of MathRandom plus Nyuu's ID loop, with random 0/1/2 discarded IDs between published ones. Validation: 500/500 and 300/300 synthetic IDs linked, gaps only 24/48/72.
- `run_corpus.py <nzb-dir> <binary>`: per-NZB driver. Corpus is the sanderjo/NZBs git clone (data only; nothing from it was executed).
- Full per-NZB output: `w10a-corpus-results.txt`.

## Caveats / follow-ups
- 2017-07-28: 1 ID fell outside the anchor's forward window, probably an earlier draw index than the earliest timestamp. A backward step or a second anchor would fix it.
- Brute force handles any first-ID offset. No alignment problem exists because there is no cache.
- Cross-NZB linking: same process means the same r0 stream, so solved r0 values from different NZBs can be checked with a forward walk. This was not attempted in this wave.
