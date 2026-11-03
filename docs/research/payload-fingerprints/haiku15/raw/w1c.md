# W1C hashing tricks (Haiku)
- Whole-file CRC via crc32_combine of per-part pcrc32 + =ypart lengths; crc32fast::Hasher::combine. Twin key. Needs all parts. (pcrc32 is over decoded bytes in practice.)
- OSHash: size + first/last 64KB; 2 articles. Alt prefilter only.
- FastCDC (crate fastcdc 3.1): chunk hashes for repacks/alts; needs full decode.
- ed2k 9,728,000 blocks: only if aligned; unverified.
- Not covered: AICH, BTv2, TTH, TLSH, ssdeep.
