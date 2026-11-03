# Wave 10C: robustness of the size-vector repost/twin key (Opus subagent)

Code and outputs: `repost-test3/`
- `yenc.py`: my own Nyuu-style yEnc size model (line 128, critical NUL/LF/CR/=, tab/space at line edges, '.' at line start, ybegin/ypart/yend lengths). On random data it gives 739.5-739.6 kB per 716800-byte article. The w9c corpus reference for that bucket was 739648.
- `synth.py` / `synth_pred.py` / `synth.txt` / `synth_out.json`: the byte-range key, tested on synthetic data.
- `t3.py` / `t3.txt`: corpus tasks 1-4.
- `n1math.py` / `n1math.txt`: analytic false-hit table for n-1 matching.

Corpus: the same sanderjo/NZBs clone as w9c (114 NZBs; 1,153 eligible distinct posts). I re-cloned it into the scratchpad because w9c had not kept it. Nothing downloaded was executed.

## 1. n-1 tolerance
**Corpus (gated, floor 40, 388 distinct vectors).** Exact matching gives 17 groups, all true and none false. Accepting one mismatched segment adds 3 pairs:
- A true pair at n=15: the damaged-RAR repost (7dc5d8c1 / d70fafb0, sd 146).
- A true pair at n=2: the same PAR2 file in two NZBs.
- A **false** pair at n=2: two unrelated 2-segment PAR2 files. At n=2, n-1 means only one segment has to match.

Allowing two mismatches (n-2) adds 878 false pairs against 23 true, almost all at n<=3. Do not use it.

**Analytic model (high-entropy data).** The full-segment encoded-size sd is 81-101 B at 716800 in the synthetic runs, and the corpus gated median is about 146. So two unrelated segments have equal sizes with probability p ≈ 0.002-0.003. The per-pair false-hit rate is about p^n for an exact match and n·p^(n-1) for an n-1 match:

| n segs | exact | n-1 |
|---|---|---|
| 3 | 3e-8 | 3e-5 |
| 4 | 1e-10 | 1e-7 |
| 5 | 3e-13 | 5e-10 |
| 6 | 1e-15 | 2e-12 |
| 8 | 9e-21 | 2e-17 |

With N files sharing a segment count, the expected number of false pairs is (N^2/2) × rate. For under 0.01 false pairs, n-1 matching is safe at:
- **n >= 6** for 10^5 files of that segment count;
- **n >= 7** for 10^6;
- **n >= 8** for 10^7.

For 3-5 segment files, use exact matching only. n-1 should also never confirm a twin on its own; send it to the CRC32/MD5 check, which agrees with the sibling thread. Blocking caveat: a key of segcount plus the first 2 sizes misses a damaged copy whose bad segment is one of the first two. Add a second blocking key on segments 3-4, or on the last two full segments.

## 2. sd floor: 40 vs 70
Raising the floor to 70 removes 36 of 418 gated files. Of those, 4 are in exact multi-post groups: 2 true twin groups (PAR2 vol126+74 and vol030+32 of one post in a rewritten NZB, sd 45/56). It removes **0 false exact groups**, because floor 40 already had none. It also removes the one false prefix collision from w9c (long-dirfilename, sd 43/68), and it does not affect the n-1 results.

**Keep 40 for the confirm step.** Use 70 only as a stricter gate on the blocking or prefix index, if prefix collisions cost too much.

## 3. Byte-range key across article sizes
**Method.** Take vector A with article size A and file length L:
1. Invert each size into data characters with `chars = size - header - 2*ceil(chars/128.3)`.
2. Compute escapes per segment as `chars - raw bytes`.
3. Assume the escape density is uniform inside each A segment, and add up the escapes over each B byte range.
4. Re-encode those totals to predicted B sizes.
5. Compare with the observed B vector using two scores: the per-segment RMS residual, and the summed residual. Escapes add up over byte ranges, so the summed residual measures total-escape conservation.

**Synthetic results** (16 segments, 4 files per kind, all files the same length, which is the worst case):

| data | direction | twin RMS | non-twin RMS | twin \|sum\| | non-twin \|sum\| |
|---|---|---|---|---|---|
| random | 716800 → 768000 / 750000 | 45-91 | 97-172 | <= 60 | 165-976 |
| random | reverse (B → A) | 31-62 | n/a | <= 54 | n/a |
| real-like (random plus zero/ASCII runs) | 716800 → 768000 / 750000 | 416-1259 | 1428-2457 | <= 123 | 6,000-15,400 |

- On random data the RMS ranges almost touch, so RMS discriminates only weakly. The summed residual separates well.
- On real-like data RMS overlaps more, because density varies inside a segment, but the summed residual separates by about 50x.

**The usable key is total-escape conservation, with the RMS as a secondary check.** The sum only holds when the per-segment header length is known, so the filename length has to be the same or known; UUID renames keep it. If the name length is unknown, a fitted offset absorbs the sum and only the weak RMS check is left. For random data the summed residual gives roughly a 5-10% false rate between two unrelated files of the same length. That is a candidate generator, not a merge.

**Corpus (toverdoos blabla.bin at 716800, 2000111, 4000111, 5111222 and 10111222 bytes per article).**
- Predicting from any coarse vector to the 147-segment vector leaves a residual of 1 B after removing the offset, and a constant offset of -14 to -19 (the header/line model is not calibrated).
- 13 unrelated 100 MB files at 147 segments score the same: 0.8-1.6 B, with offsets of +7 to +12 for one file and about +131 for the rest.
- The file's within-file sd is 2, so it is low-entropy filler and the key **cannot discriminate** on it, as w9c expected. The predictions match the twin but also match the filler class.

Verdict: the byte-range key works mechanically, including in the hard direction from fine to coarse. It discriminates only on high-entropy data, and mainly through total escapes. Use it to generate candidates when the article sizes differ, then confirm with CRC32/MD5.

## 4. Grouping files into uploads without From or subject
1,704 distinct files from 100 uploads, using the source NZB as ground truth. The NZB `date` field has 1 s resolution.
- **Real timeline.** Grouping by time gap alone gives precision/recall 0.98/1.00 at T=120 s. This test is trivial, because the uploads are months to years apart. Adding the shape rules (same article-size bucket; no file larger than the set's volume; a short volume closes the run) lowers recall to about 0.90. The cause is mixed files: PAR2 files, 1-segment files, and files in other buckets.
- **Overlapping uploads.** I squeezed all 100 uploads into a 1-hour window, keeping each upload's internal timing. Time alone collapses to precision 0.03-0.16 (recall 0.98). Time plus shape gives precision 0.27-0.42 and recall 0.52-0.56. In a 10-minute window it gives precision 0.08-0.09.

**Shape rules (equal volumes, smaller last volume, article-size bucket) cannot recover uploads when uploads interleave.** They only help to split one time cluster. Per-file random From uploads need the Nyuu Message-ID time/PRNG stream continuity from waves 6-8 as the main joining signal, with shape as a tie-breaker. A PAR2 2^k+1 rule is not usable without names: across the corpus, PAR2 segment counts are not distinctive enough to tell PAR2 from data files.

## Caveats
- This is a single-author test corpus. Only 2 real repost events are in it, plus 1 damaged copy.
- The cross-article-size case is low-entropy only, so no real high-entropy case exists to validate on.
- The synthetic samples are small: 4 files per kind and 16 segments.
- The "real-like" generator is my own assumption about structure in compressed video.
- The analytic table assumes segment sizes are independent and roughly normal.
