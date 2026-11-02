# W4A CRC interval algebra (Haiku), pass 1
- zlib crc32.c: crc32_combine_op(crc1,crc2,op)=multmodp(op,crc1)^crc2; op=crc32_combine_gen64(len2)=x^(8*len2) mod p (zlib 1.2.12+).
- Tested in Python vs zlib (300 cases): combine and suffix removal crc(B)=crc(A||B)^shift(crc(A),len B) exact.
- crc32fast: Hasher::combine(&mut self,&Self), new_with_initial_len(init,amount). Internal combine(crc1,crc2,len2).
- Prior: SABnzbd issue #2396 proposes file CRC from article CRCs + PAR2 block CRCs; earlier #2160 "showed issues". NZBGet #525: crc32 not mandatory.
- Algorithm: per post collect (begin,end,pcrc32); for each run of contiguous present parts, crc of [s,e) = combine of those parts (no prefix needed); match intervals with endpoints on both posts' boundaries (LCM grid) by (len,crc); confirm with 2nd interval.
MY NOTE: intervals need only contiguous present parts, so a twin can be matched even when articles are missing elsewhere.
## pass 2
- SABnzbd #2396: CRC combine design; article CRCs need no ordering; PAR2 block size constant so combine coefficient precomputable; timings precomputed 1.08s vs zlib 5.74s; crcutil 0.77s. Implemented in mnightingale's sabyenc branch (function names unverified). #2160 is quick-check (RAR disagreements), not bad pcrc32.
- sabctools: CRC32 from vendored rapidyenc; combine API names unverified.
- No named encoder writing wrong pcrc32 found. yEnc 1.3 text unreachable.
