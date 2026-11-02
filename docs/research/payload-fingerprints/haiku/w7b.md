# W7B sampled-hash schemes for raw-posted files (Haiku, ~10:00Z)
| Scheme | Definition | Articles (716800 B parts) | External DB | Source |
|---|---|---|---|---|
| OpenSubtitles moviehash | size + LE u64 sums of first and last 64 KiB (mod 2^64) | first + last 1-2 | OpenSubtitles (Atlas already computes it, PR #36) | opensubtitles.github.io/oshash; stashapp oshash.go |
| SubDB | MD5(first 64 KiB + last 64 KiB), no size | first + last | TheSubDB | github.com/subdb/hash |
| Shooter | 4x MD5 of 4 KiB at 4096, size/3, 2size/3, size-8192 | ~4 | shooter.cn (defunct?) | gist wyyqyl; Medusa a511338 |
| Napiprojekt | MD5 of first 10 MiB | ~15 | napiprojekt | gist spajak |
| imohash | MurmurHash3-128 over 3x16 KiB (start, mid, end) with size varint in top bytes; whole file <128 KiB | 2-3 | none | kalafut/imohash algorithm.md |
| AltMount sample-sha256-v1 | SHA-256(tag, size, 3x64 KiB) | 3 | AltMount lookup | altmount duplicate-lookup.md |
All are candidate-grade (head+tail only); same-size re-encodes differ, but size-preserving middle edits collide. Recommendation: add SubDB next to the existing moviehash (same 2 reads), plus a mid-file sample (AltMount v1) for twin grade.
