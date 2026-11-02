# W6b: 7z + ZIP header metadata extractor, prototype

Source: `docs/research/archive-metadata/proto/sevenz_zip_meta.rs` (single file, 1,327 lines; deps: crc32fast, lzma-rust2 0.25). Build: `cargo build --release` in the scratchpad crate `w6b/app`. Usage: `sz_zip_meta <file> [head_bytes=716800] [tail_bytes=716800]`. Reads only the head and tail windows (plus the whole file size).

## What works (verified on fixtures)

- 7z signature header: version, StartHeaderCRC validity, NextHeaderOffset/Size/CRC, absolute header offset (32 + offset), and whether the header block is in head, tail, or unavailable. NextHeaderCRC check.
- Encoded header (kEncodedHeader): StreamsInfo parsed, coder method/props reported (LZMA lc/lp/pb/dict, LZMA2 dict, AES NumCyclesPower/salt/IV), packed header decoded with lzma-rust2 and CRC-checked. Plain kHeader parsed.
- AES-encrypted header (`hdrenc.7z`, py7zr header_encrypted): reports `7z.header_encrypted = true` from the plaintext StreamsInfo and stops (no password).
- Folders: coders, bind pairs, packed stream indices, unpack sizes, folder CRC, substream counts. Substreams: per-file sizes and CRCs.
- Files: UTF-16LE names, empty-stream/empty-file/anti, dir kind, MTime/CTime/ATime as ISO UTC (from FILETIME), attributes, Unix st_mode when 0x8000 is set.
- Prefix probe: for a single-coder folder 0 (Copy, LZMA2, LZMA), decodes the first 64 KB from the head buffer and reports the first 8 bytes and a magic sniff. `mkv_lzma2.7z` and `mkv_lzma1.7z` both give `1A45DFA3 -> Matroska/WebM (EBML)`.
- ZIP local headers from head: version needed, GP flags (encrypted, data descriptor, UTF-8), method, DOS time (local, no tz), CRC, sizes, name. Extras: 0x5455, 0x7875, 0x000a (NTFS), 0x7075, 0x9901 (AES), 0x0001 (zip64), 0x5855, 0x000d; unknown extras as raw hex.
- ZIP tail: EOCD search (backwards, validates comment length), EOCD fields, trailing bytes, zip64 locator and record (overrides counts, CD size and offset), central directory entries when the CD is inside the tail. CD fields: made-by host and spec version, external attr (Unix st_mode when host=3, DOS dir bit), disk start, local header offset, comments.
- UTC offset estimate: where DOS local time and a UTC mtime extra both exist, reports raw delta (seconds) and the nearest 15-minute candidate. On `zd.zip` (zip with -X absent, 0x5455 present) the candidate is +00:00 as expected for the sandbox TZ.
- Never panics on truncation: head sizes 0, 1, 10, 31, 32, 33, 60, 100, 200, 400, 700, 1000, 2000, 3000, 5000 over five fixtures produced no panic. main() also wraps analysis in catch_unwind and prints `!! PANIC caught` if it ever happens.

## Sample output (abridged, `multi.7z` = py7zr default header compression)

```
format = 7z
7z.sig.start_header_crc_valid = true
7z.next_header.abs_offset = 500233
7z.next_header.source = head
7z.next_header.kind = kEncodedHeader
7z.encoded_header.folder[0].coder[0].method = LZMA2
7z.encoded_header.folder[0].coder[0].lzma2.dict = 16777216
7z.main.folder[0].coder[1].method = BCJ_x86
7z.main.folder[0].unpack_size = 500016
7z.file[1].name = src7/a.txt
7z.file[1].size = 12
7z.file[1].crc32 = AF083B2D
7z.file[1].mtime = 2026-11-02T07:16:23Z
```

`hdrenc.7z`:
```
7z.encoded_header.folder[0].coder[0].method = AES-256+SHA-256
7z.encoded_header.folder[0].coder[0].aes.num_cycles_power = 19
7z.header_encrypted = true
```

`zd.zip` (default `zip`, extras present):
```
zip.local[0].x5455.mtime_utc = 2026-11-02T07:16:30Z
zip.local[0].x7875.uid = 0
zip.local[0].utc_offset_candidate = +00:00
zip.cd[0].made_by.host = 3 (Unix)
zip.cd[0].unix_mode = 100644
```

## Failures and limits (prototype)

- Multi-coder folder chains are not decoded. py7zr's default LZMA2+BCJ folders fail the header decode path only if the header itself uses a chain, and the prefix probe is skipped for chains (reported as "not probed"). BCJ, Delta, and BCJ2 are reported by name but not applied.
- Prefix probe covers folder 0 only. Solid folders beyond the first file are not decoded.
- Only one pack stream per folder is handled in the encoded-header path (typical case).
- ZIP local walk stops at a data descriptor with csize 0 (cannot find the end without decompressing), and at the head boundary.
- Not tested: ZIP64 archives (no fixture was generated), ZIP AES (0x9901) fixtures, NTFS 0x000a on real Windows zips, split 7z volumes (.7z.001), PPMD/BZip2/Deflate (no decode attempted), and multi-disk ZIP.
- Some DOS date values are local without a timezone; the UTC offset estimate depends on a 5455/UTC extra.

## Fixtures (scratchpad `w6b/fx`)

- `multi.7z`: py7zr, 3 entries, default header compression (LZMA2 header, LZMA2+BCJ data)
- `hdrenc.7z`: py7zr, password=secret, header_encryption=True
- `zx.zip` (zip -X), `zd.zip` (zip default, 0x5455 and 0x7875), `zpy.zip` (python zipfile), `zstore.zip`
- Reused: `w4a/mkv_lzma1.7z`, `w4a/mkv_lzma2.7z`

## LOC

1,327 total lines in the proto file (`wc -l`), including blanks and comments.
