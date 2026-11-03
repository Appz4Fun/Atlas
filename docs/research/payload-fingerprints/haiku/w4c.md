# W4C PAR2 recovery data as a linking key (Haiku research + TESTED locally 08:35Z)
Research (par2cmdline source): GF(2^16) poly 0x1100B; input i gets base 2^n with n coprime to 65535; recovery slice for exponent e = sum_i base_i^e * slice_i; inputs sorted by FileId = MD5(hash16k||length||name); Set ID = MD5(Main body: slice size, file count, FileIds). RecvSlic header (first 68 bytes: magic, length, packet MD5, set ID, type, exponent) names set + exponent from one article; packet MD5 needs the whole packet. par2 -f appends blocks to an existing set (fill keeps set ID). ParPar's test suite compares output with par2cmdline. Sources: raw.githubusercontent.com/Parchive/par2cmdline/HEAD/src/{galois.h,reedsolomon.cpp,reedsolomon.h,par2creator.cpp,descriptionpacket.cpp,mainpacket.cpp,par2fileformat.h}; animetosho/ParPar README.

TEST (par2cmdline 0.8.1 vs ParPar 0.4.6, 2 random files 3,000,000 + 1,234,567 B, slice 64 KiB; tools/par2_recovery_compare.py):
- par2cmdline twice with different CLI file order -> identical set ID and identical recovery packets.
- ParPar vs par2cmdline -> identical set ID and byte-identical recovery slices for exponents 0-2 (only the Creator packet differs: "Created by par2cmdline version 0.8.1." vs "ParPar v0.4.6 x64 [https://github.com/animetosho/parpar]").
- Rename one file (6 names tried): set ID always changes; recovery for exponent >=1 changes only when the rename flips FileId sort order (2 of 6); EXPONENT 0 RECOVERY IS IDENTICAL IN EVERY CASE.
- Different slice size (128 KiB) -> everything differs.
Why: base_i^0 = 1, so the exponent-0 recovery slice is the plain XOR of all input slices: independent of file names, order, set ID and tool.
| Signal | Proves | Cost |
|---|---|---|
| RecvSlic exponent-0 data (or a prefix of it) | twin of the file SET (same bytes, same slice size) even after renaming/obfuscating every file and re-PARring with another tool | 1 article of the first recovery volume (prefix compare) |
| Set ID | same files + same names + same slice size | 1 article |
| Recovery exponent >=1 data | same files, slice size and FileId order | 1 article |
| Creator packet | tool + version (+CPU arch for ParPar) | 1 article |
Caveat: the prefix of the exp-0 slice in article 1 covers only the first ~700 KB of the slice; equal prefixes on two posts with the same slice size are already strong (XOR of the same offsets of every input slice). Untested: MultiPar/par2j and QuickPar, posts whose first recovery volume starts at exponent >0.
