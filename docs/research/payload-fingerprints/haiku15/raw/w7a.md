# W7A pcrc32 scope (Haiku) — VERIFIED decoded bytes
- NZBGet daemon/nntp/Decoder.cpp: strstr " pcrc32=" (multipart) / " crc32=" (single); m_crc32.Append(outbuf) decoded output; dsCrcError on mismatch.
- sabctools 9.7.3 src/yenc.cc: rapidyenc_crc(dst decoded) vs crc_expected; pcrc32 then crc32 fallback.
- yencode npm README: crc over raw bytes; multi_post pcrc32 per part + crc32 whole on last part.
- Nyuu lib/article.js: pcrc32 = y.crc32(data raw); crc combined; crc32= only on last part.
- ngPost, Pan unverified.
- I re-checked with zlib: crc32([0,1,2,3,4])=0x515ad3cc, crc32([0,1,2,3])=0x8bb98613, crc32([4])=0xd56f2b94 = README values -> pcrc32/crc32 are over raw decoded bytes. CONFIRMED.
