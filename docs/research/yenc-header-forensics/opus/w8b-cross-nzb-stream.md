# Wave 8B: Nyuu Message-ID stream linking, within and across NZBs

Code: `nyuu-id-solver/stream_link.py` (usage: `python3 stream_link.py OUTDIR W nzb...`). Synthetic test: `nyuu-id-solver/w8b_synth.py`. Results: `nyuu-id-solver/w8b-stream-link-results.json` and `w8b-stream-link-run.log`. All code is my own. No third-party code was run.

## Method
- **Stepping:** concrete xorshift128+ step `cstep` and its inverse `cback` (undo xorshift-right-17, then xorshift-left-23). Checked with 1000 steps forward and back. `Stream(anchor)` models V8's 64-entry cache. The anchor is the state at a refill boundary. Block k (negative k allowed) holds states 64k+1..64k+64, and they are served LIFO. So draws can be produced for any range of blocks before or after the anchor.
- **Solver:** a GF(2) solve from **two IDs at a given gap** (24/48/72/96/144). The w6a solver assumed the two IDs were contiguous, so it only worked on pairs with a gap of 24. Each candidate is checked by regenerating its draws.
- **Alignment fix (main new finding):** a solution whose cache-block boundary is off by a few states still reproduces every ID that falls inside one block. That is why w6a linked only part of each NZB (164/172, 8324/16486, 245/815), and why it showed false 144/192 gaps. `refine_alignment` tries all 127 boundary shifts and keeps the one that links the most nearby IDs. After the fix, **every ID in every solved NZB links**.
- **Linking:** generate ±(n·150/64+64) blocks, then match 24-draw keys through an 8-draw prefix dict. The solve is repeated on any unlinked IDs to find further streams.
- **Cross-NZB:** each stream is reduced to a canonical anchor (the block boundary at or below its earliest ID). A small C kernel steps every anchor forward W states and looks up each state in a hash of all anchors. Testing forward only from every anchor covers both orderings.

## Synthetic validation
A: 3 files / 120 IDs. Then 500k unrelated draws. B: 2 files / 60 IDs, same process. C: a new seed.
- Result: A→B linked at 506,048 steps. C linked to nothing. All 240 IDs linked to the correct streams.
- Before the alignment fix, B and C each showed a spurious second "stream" 21 or 8 states off, which confirms the misalignment effect.

## Real corpus (19 NZBs solved in w6a, sanderjo/NZBs, 2023-05 to 2026-11), W = 1e9 steps per anchor (80 s)

| NZB | IDs | files | linked | streams | span (draws) |
|---|---|---|---|---|---|
| Unicode_Mix_2023 | 172 | 19 | 172 | 1 | 8,112 |
| reftestnzb_10GB_no-rar | 16486 | 10 | 16486 | 1 | 791,160 |
| RefTestNZB_1234 (x2 copies) | 182 | 29 | 182 | 1 (identical anchor) | 8,568 |
| RefTestNZB_2023_MoreStuff | 168 | 9 | 168 | 1 | 7,896 |
| Subtitles_1234 | 33 | 11 | 33 | 1 | 1,440 |
| starrySTARnight PLAIN / inside_RARs | 30 / 30 | 1 / 4 | all | 1 each | 1,272 |
| windows_hate / windows_user_hate | 9 / 9 | 9 / 9 | all | 1 each | 264 |
| DDDD / EEEE (same post) | 815 | 14 | 815 | 1 (identical anchor) | 39,000 |
| RRRR | 169 | 13 | 169 | 1 | 7,992 |
| testfile-10GB-no-rar | 16488 | 11 | 16488 | 1 | 791,256 |
| back_from_future_100MB | 163 | 13 | 163 | 1 | 7,248 |
| hp-passworded / one-missing-part21 (same post) | 1654 / 1653 | 29 | all | 1 (identical anchor) | 78,816 |
| only-p-passworded | 1654 | 49 | 1654 | 1 | 78,816 |
| QUIRINIUS 100MB | 352 | 30 | 352 | 1 | 16,320 |

- **Within an NZB:** 100% of IDs fall on one stream, across all files: 0 files split across streams and 0 unlinked IDs.
- **Draw order matches the ms timestamp order exactly:** 0 inversions in every NZB.
- **Gaps between IDs:** mostly 48. There are some 24 and 72 gaps and rare 96/120/144/192/576 gaps; the w6a 144-gap blocks are gone. The 24/72 gaps come from build/send interleaving across connections (pipelined builds, plus a retry adds 24). The connections all share the one JS Math.random stream: one stream per process, with no separate stream per connection.
- **Span:** about 48 draws per article (16,486 IDs → 791k draws).
- **Across NZBs: 0 links among 14 distinct anchors**, even for close pairs:
  - starry PLAIN vs RARs: 3 min apart
  - only-p vs hp-passworded: 13 min apart
  - RefTestNZB_2023 vs _1234: 16 min apart
  - windows pair: 23 min apart
- 1e9 states is about 20M articles, which is far more than any one run. So each of these NZBs came from a **separate Nyuu process**: one invocation per post, each with a fresh V8 seed.
- Identical anchors appear only for re-exported copies of the same post (RefTestNZB_1234 x2, DDDD/EEEE, hp/one-missing).

## Quantification
- **Bootstrap:** two IDs of one upload (any gap from 24 to 144) recover the state in about 0.1–1 s of Python. One ID alone gives only about 113 bits, which is less than 128.
- **Enumeration:** from one solved state, **every article of that Nyuu process can be enumerated**, before and after, across refills. In the corpus that is 100% (up to 16,488 articles per post). The cost is about 48 draws per article, plus a small allowance for retries.
- **Predicting IDs:** the next IDs are predictable before they are posted, apart from the ms suffix. The candidate set is about 2 keys per article (build ID and send ID), plus one extra key per retry.
- **Scanning an index of N observed IDs:**
  - Generate G 24-draw keys. That costs about 4 ns/state in C (the kernel ran 19e9 state steps with hash checks in 80 s), or about 1 µs/state in Python.
  - Put the smaller side in a hash set and stream the other side through it. The cost is O(G+N), and memory is about 8 B per key with 64-bit fingerprints.
  - False-match rate per key pair is 26^-24 ≈ 2^-112.8, so G·N up to 1e30 still gives effectively 0 false positives.
  - Example: one 10 GB upload (G ≈ 8e5) against a 1e10-ID index costs about 1e10 lookups, minutes of I/O.
- **Linking processes:** a search across processes is pointless: they never share an orbit (fresh seeds, about 2^128 apart). The useful linking key is the **process**. All files of one post, and any other posts made in the same Nyuu invocation, are linked.

## Next steps
1. Look for multi-post single invocations, such as Nyuu run with several inputs, or a long-running nyuu "daemon"/queue tools (e.g. tools wrapping Nyuu as a library in one Node process). Only those would give cross-NZB links.
2. Use the predicted IDs to look for missing or never-indexed articles of a post. For example, the missing part21 article is predictable from the stream.
3. Port the alignment fix back into `nyuuid_solve.py`, and rerun w6a's partial counts.
4. Old V8 (2017–2022) is still unsolved (see w7a).
