# Wave 5 local test (08:50Z): PAR2 per-file keys vs set-level keys
Same 3,000,000 B file under three PAR2 sets (slice 64 KiB, par2cmdline 0.8.1): (a) movie.mkv + extra.bin, (e) renamed abc123.mkv alone, (f) movie.mkv + extra.bin + 50 KB junk.bin.
- FileDesc full MD5 + length: identical in all three (expected).
- IFSC packet body (ordered per-slice MD5+CRC32 list) for that file: identical in all three (9adbba58f7a2...), so it is a per-file key that survives renames, different set composition and junk files; the only requirement is the same slice size.
- Exponent-0 recovery slice: changes when a junk file is added (a vs f differ), so it is a set-level key only.
Takeaway: set-level keys (set ID, exp-0 data) link whole-post twins; per-file keys (full MD5+len, IFSC list) link files across packs/singles/junk-padded reposts. Script: tools/par2_ifsc_compare.py.
