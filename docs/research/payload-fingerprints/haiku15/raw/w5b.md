# W5B P2P identity literature (Haiku)
- BEP 52 v2 per-file pieces root: identical files -> same root across torrents (analogue: PAR2 full MD5).
- BEP 47 per-file sha1 = hint only.
- SFU cross-swarm study: size as relatedness hint only.
- AICH: part hashes localize damage; fakes can carry legit hashes.
- Liang/Kumar/Ross pollution (cse.engineering.nyu.edu/~ross/papers/pollution.pdf): 62-73% polluted; detect by decodability/duration, not hashes. Index poisoning (poison.pdf): decoyers <7% users, >77% copies; many-variants poster = decoy signal.
- ssdeep DFRWS 2022: fuzzy hashes unreliable; never for twins.
- Birthday: n=1e8 files -> CRC32 alone 1.2e6 FP pairs; (size,CRC32) ~1-120 pairs depending on size skew; MD5 negligible.
MY NOTE: N matching per-part pcrc32s at same part size = 32N bits; k matching LCM intervals = 32k bits + length. With N,k>=4 this is >=128 bits: twin-grade without MD5. Single (size, crc32) = candidate only.
