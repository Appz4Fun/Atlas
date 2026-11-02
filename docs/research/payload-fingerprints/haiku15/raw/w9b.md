# W9B slice CRC vs article CRC (first pass, 10:27Z)
- IFSC CRC32 of the last slice covers file tail + zeros to slice size (spec mirror, par2-rs checksum.rs). CRC32 locates, MD5 confirms (par2cmdline issue 31).
- Code: sabctools crc32_combine(crc1,crc2,len) -> rapidyenc_crc_combine; crc32_zero_unpad(crc,len) -> rapidyenc_crc_unzero. ParPar crc_zeroPad (append only). par2-rs Crc32UncombineOp, crc32_padded.
- NZBGet/SABnzbd use of article CRCs against PAR2 blocks: not verified (second pass asked).
- Pitfalls: all-zero/repeated slices give identical CRCs; keep exact pad length.
- MY TEST (tools/slice_vs_article.py, 10:31Z): 5,000,003-byte random file, 716800 articles, 393216 slices. Folding article CRCs and folding IFSC slice CRCs (last one un-padded) both give the real file CRC32 0xdb18e736. So every PAR2 FileDesc entry yields a whole-file CRC32 to match against the folded article CRCs: places equal-size volumes on the right PAR2 entry with zero payload bytes. Shared interior LCM boundaries are rare (LCM here 68.8 MB), so whole-file is the main comparison.

## Second pass (10:35Z)
- NZBGet ParChecker source and rapidyenc.h could not be fetched (truncation / robots). Only verified: NZBGet quick verification calls VerifyDataFile and maps to full/partial match. Agent withdrew "LCM not enough".
