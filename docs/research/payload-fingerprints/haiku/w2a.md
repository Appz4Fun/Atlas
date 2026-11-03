# W2A store-mode offset maps (Haiku, ~07:30Z)
- RAR4: marker 7 B; file header base 32 B (+8 with LHD_LARGE 0x0100); data_start = header_start + HEAD_SIZE; data runs PackSize. SPLIT_BEFORE 0x0001 / SPLIT_AFTER 0x0002; inner offset of a fragment = running sum of PackSize along the volume chain (exact in store mode). CORRECTION: FileCRC is the whole-file CRC32 only in the last volume header; non-last volumes carry the chunk CRC (unrar volume.cpp). Alt key = first article of the last volume. END 0x7B, REVSPACE flag means RR before end. unrar headers.hpp (chromium mirror)
- RAR5: sig 8 B; block = CRC32 + header_size vint + header; data_start = start + 4 + len(vint) + header_size; flags 0x08/0x10 = continues from/into other volume. RR/QO service blocks shift offsets; skip them. https://docs.rs/crate/nzbdav-rar/latest/source/src/rar5.rs (Rust prior art, but no split-chain joining)
- 7z COPY folder: pack stream i at 32 + PackPos + prior sizes; SubStreamsInfo 0x09 sizes, 0x0A CRCs -> free inner CRCs (unless -mhe). py7zr archive_format
- ZIP stored: data = LFH + 30 + n + m; sizes/CRC in central directory (or data descriptor).
- Mapping: volume byte b -> article via =ypart begin/end; 2-3 articles per file prove interval identity via pcrc32 + crc32_combine.
- pyReScene tracks position += HeadSize + AddSize per block (good reference); nzbdav-rar crate parses headers only.
Leads: libarchive rar.c split rules; 7z last-substream rule; RAR5 RR/QO skipping, does ngPost pass -rr.
