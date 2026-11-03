# W5A CRC primitives (Haiku)
- rapidyenc src/crc.h: crc32_combine(crc1,crc2,len2)=crc32_shift(crc1,crc32_bytepow(len2))^crc2; crc32_zeros, crc32_unzero (remove trailing zeros -> PAR2 last-slice padding), crc32_2pow, crc32_256pow; crc_power[k]=x^(2^k). Public C API rapidyenc_crc_combine/_multiply/_unzero/_2pow/_256pow.
- sabctools wraps these: crc32_combine, crc32_multiply, crc32_zero_unpad, crc32_xpown, crc32_xpow8n -> SABnzbd article-CRC combine = rapidyenc.
- crc32fast 1.5.0: Hasher::combine pub, new_with_initial_len pub; internal X2N_TABLE + bit-serial multiply; shift_table(len) 4x256 tables for fixed stride (internal, copy it).
- crc-fast 1.6: pub checksum_combine(algo, c1, c2, len2) with PCLMULQDQ path.
MY NOTE: Atlas's interval algorithm needs only FORWARD combine of contiguous parts: Hasher::new_with_initial_len(pcrc32, part_len) then .combine() in order -> crc32fast alone is enough, no port needed. PAR2 last-slice zero padding needs unzero (or skip the last slice).
