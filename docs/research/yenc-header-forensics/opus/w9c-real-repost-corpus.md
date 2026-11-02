# Wave 9C: real repost corpus test of the segment-size-vector key (Opus subagent)

Code and outputs: `repost-test2/` (corpus.py = index + ungated twins + prefix collisions; gate.py = entropy-gated version + prefix-collision listing; obf.py = obfuscated repost family dump; *.txt = outputs).
Corpus: github sanderjo/NZBs (108 NZB files, 2 unparseable on purpose) + nzbgetcom/nzbget tests/testdata/nzbfile (6). sabnzbd and Nyuu repos hold no .nzb fixtures; gh code search unavailable (token refused), so no wider GitHub corpus. 114 NZBs, 3,020 file entries, 2,860 distinct posts (by Message-ID set); writers: Nyuu-family 2,892, other 58, header-inclusive 43, flat (ngPost-type) 27. Eligible (non-flat, >=2 segments): 1,153.

## Main result
1. **New real repost family (strongest evidence so far).** `obfuscated-posts/reftestnzb-obfuscated-rar-*` (4 NZBs): the same 7-volume RAR set (1x3-seg + 6x15-seg, high entropy, within-file sd 146-421) uploaded **4 separate times** on 2020-02-23 (t=1582469925, 1582470507, 1582471044, 1582472513; ~43 min span), every upload with fresh Message-IDs and, in uploads 3 and 4, **new random UUID filenames** (e.g. 22f3d492... = e59e3efd... = 34929454...). All 7 volumes match exactly across all 4 uploads: 6x15 + 3 segments, 93 segment sizes per upload, 0 shared Message-IDs. Filename keys cannot link these; the size vector does. Volume c8d06c81 vs d70fafb0 (the "one-damaged-rar-file" upload) matches **14/15**: a deliberately corrupted copy. Full-vector key misses it; a "most segments equal" or prefix key catches it.
   PAR2 files in the same uploads do NOT match (vectors differ by 10-30 B per file): PAR2 regenerated / filename strings inside PAR2 changed. Use data volumes, not PAR2, for linking.
2. Pair A (w6b) re-confirmed: unique_files_with_subdir x2, 5 files x15 segs, 19.4 h apart.
3. Same-post-different-NZB (not reposts, but show the key is stable across NZB rewrites): Big Buck Bunny 497 segs (496 shared IDs, one removed), hp-passworded part21 (17/18 shared IDs), all-missing.nzb (same post with Message-IDs rewritten to `@nothereatall`, same dates, identical sizes).

## (b) False hits
- **Ungated:** 75 full-vector groups with >1 post; 55 have different filenames. Almost all are low-entropy sanderjo test data (within-file sd 1-3, e.g. mean 728149/728851): parts 02-09 of one RAR set are identical to each other and to other unrelated sets (100MB_r__400 / rwx_all_on_777 / My Very Very Important / nzb-50MB..., blabla.iso vs blabla.bin, mytestpost.part06 == part07 across NZBs). On low-entropy content the vector is a content-class fingerprint, not an identity.
- **Entropy gate** (full-seg sd 40-600 AND mean within 1.5 kB of the corpus high-entropy reference for that article size): 388 distinct vectors pass. Full-vector groups: 10 same-name + 7 diff-name, and **all 7 diff-name groups are the true obfuscated reposts above. 0 false full-vector hits.**

## (c) Near misses (same name, different vector)
- Same name, regenerated content (control): postfile.partNN.rar 100MB_r__400 vs "reftestnzb 100MB vb QUI...": 0/15 equal, constant offset ~+11,480 per segment (different fill pattern). Correctly unlinked.
- Re-RAR of low-entropy data 2 min apart: part01 13/15, part10 7/8 (w6b Pair B). Low entropy so weak.
- Same content, different article size: big_articles/toverdoos blabla.bin posted with 5 article sizes (11/21/27/53/147 segs). Segment-index key cannot link; needs a byte-range key (not tested here; low entropy anyway).
- Doctored NZBs: test-negative-/too-small-segment-bytes copies have edited byte fields (-728851, 851). A key must reject sizes outside the valid range.
- PAR2 with the same name across NZBs: never equal (regenerated per post).
- Damaged repost: 14/15 (see above).

## Collision rate / blocking key (distinct vectors, key = (segcount, first k sizes))
| k | ungated (806 vectors) | gated (388) | gated, no segcount |
|---|---|---|---|
| 1 | 32.1% | 5.2% | 34.5% |
| 2 | 26.2% | 1.2% | 1.2% |
| 3 | 22.6% | 1.2% | 1.2% |
| 5 | 20.7% | 0.65% | 0.65% |
| full | 0 by construction; 0 false hits when gated | | |
The gated k=2..5 collisions are 2 pairs: one true near-twin (damaged repost c8d06c81/d70fafb0) and one false (two files of long-dirfilename-1000plus, sd 43/68, borderline-low entropy, 20/49 equal). So **minimum blocking key = segcount + first 2 full-segment sizes, after the entropy gate**; then confirm on the full vector (exact, or >= n-1 equal to catch damaged copies). k=1 is not enough (5% even gated, 35% without segcount). Raising the sd floor to ~70 would remove the remaining false pair.

## Timestamp continuity
The 4 obfuscated uploads are separable by their per-file `date` sequences (each upload is one monotone 3-5 s/file run); the matching key links volume i of upload u to volume i of every other upload, and the timestamps keep the uploads apart. Full Nyuu ms-ID continuity was not re-run (time box).

## Caveats
- Corpus is a single author's test NZBs (Nyuu-dominated); only 2 independent real repost events (Pair A, obfuscated family), but the obfuscated one is 4 uploads x 7 files and shows renamed-file linking.
- Entropy reference means are fitted on this corpus (730378 / 739648 / 793119 per article-size bucket); a production gate should use the theoretical yEnc expectation per line length.
- No different-writer same-content pair found (still open from w6b).
