# W4C re-PAR'd / re-split (Haiku), pass 1
- Main packet File ID arrays sorted numerically -> RSID deterministic per (files, names, slice size). Slice size not in FileDesc -> File ID equal across re-PAR'd sets of same named file. Spec ambiguity on File ID input order (check par2cmdline).
- ParPar 0.4.5: -s required; "par2cmdline default equiv -s2000", "MultiPar default equiv -s0.5w*10"; -r default 0.
- IFSC CRC vs yEnc pcrc32 only align if slice boundaries == article boundaries.
- pyrescene issue 168: same file, single vs multi-volume, different header CRC. MY NOTE: explained by peer finding — non-last RAR volumes store the CRC of that volume's chunk; whole-file CRC only in the last volume header. So read inner CRC from the LAST volume.
- Agent's table corrected: same RSID = same PAR2 file set (names+sizes+slice), not proof of twin; twin needs byte-level data keys too.
## pass 2
- par2cmdline descriptionpacket.cpp ComputeFileId: MD5(hash16k||length||name), name cut at NUL. Confirmed order.
- par2cmdline default: blocksize = ceil(total/2000) rounded to x4, floor ceil(total/32768); redundancy 5%. Deterministic from total size -> same total size + same tool = same slice size.
- ngPost wiki PAR2_ARGS example "-s1M --auto-slice-size -r1n*0.6 ..."; Nyuu sample article-size 700K, no PAR2 settings. Postie unknown.
- No tool aligns slices to articles.
MY SYNTHESIS: combine W4A + W4C: IFSC slice CRC32s and yEnc pcrc32s are both CRCs of aligned intervals, so with CRC combine/removal both can produce CRC of any interval on the LCM grid of slice size and article size (e.g. slice 1 MiB, article 716800 -> LCM 36,700,160... compute per pair). So one PAR2 index article can be checked against a different post's yEnc headers with zero payload download.
- LCM grid: 1MiB vs 716800 = 183,500,800 B; 716800 vs 768000 = 10,752,000 B; 716800 vs 750000 = 1,344,000,000 B. Fine for multi-GB files, useless for small ones.
