# W8a: 7z filter-chain prefix decode, RAR5 -hp volumes, RAR 2.0 patch

Status: Task 1 DONE. Task 2 DONE for rars writer output (not checked against WinRAR). Task 3 NOT ATTEMPTED (time cap).
Scratch: /tmp/claude-0/-home-claude/1315c443-c098-58da-9f63-b135a00caf8b/scratchpad/w8a (gen.py, probe/src/main.rs, *.7z, orig.bin)

## Task 1: 7z [BCJ x86, LZMA2] and [Delta(4), LZMA2] truncated-prefix decode

### Setup
- Payload: 8 MiB (8,388,608 B), first 4 bytes `1A 45 DF A3`, then random / ASCII-text / E8-E9-rich (x86 call/jmp-like) / low-entropy blocks. Seeded (random.Random(8)). sha256 of payload `4d8648e5...98f4`.
- Archives built with py7zr 1.1.4: `filters=[{"id": FILTER_X86}, {"id": FILTER_LZMA2, "preset": 6}]` and `[{"id": FILTER_DELTA, "dist": 4}, {"id": FILTER_LZMA2, "preset": 6}]`.
- Folder coder layout as written by py7zr: coder0 = LZMA2 (id 21, prop 0x16), coder1 = BCJ (03030103) or Delta (03, prop 03 = dist 4); bind pair incoder=1 <- outcoder=0. Decode order is therefore LZMA2 first, then the filter. Full-archive decode through my code gives output identical to the payload for both chains (checked: `filtered==orig: true`).
- Archive sizes: bcj.7z 5,600,575 B; delta.7z 5,730,891 B. Packed stream starts at offset 32.
- Decoder: `lzma_rust2::Lzma2Reader::new(cursor, 64 MiB, None)`, then `lzma_rust2::filter::bcj::BcjReader::new_x86(cursor, 0)` or `lzma_rust2::filter::delta::DeltaReader::new(cursor, 4)`. Both exported by lzma-rust2 0.25.0 (no hand port needed). Read loop keeps bytes produced before the error.

### Results (correct prefix vs original payload)

| chain | cut (of file size) | archive bytes fed from off 32 | LZMA2 out B | LZMA2 terminal | filtered out B | filter terminal | filtered correct prefix B | % of 8 MiB |
|---|---|---|---|---|---|---|---|---|
| BCJ+LZMA2 | 1% | 55,973 | 0 | UnexpectedEof | 0 | EOF (masked) | 0 | 0.00 |
| BCJ+LZMA2 | 5% | 279,996 | 327,680 | UnexpectedEof | 327,680 | EOF (masked) | 327,680 | 3.91 |
| BCJ+LZMA2 | 10% | 560,025 | 786,432 | UnexpectedEof | 786,432 | EOF (masked) | 786,432 | 9.38 |
| BCJ+LZMA2 | 11% | 616,031 | 917,504 | UnexpectedEof | 917,504 | EOF (masked) | 917,504 | 10.94 |
| BCJ+LZMA2 | 12% | 672,037 | 917,504 | UnexpectedEof | 917,504 | EOF (masked) | 917,504 | 10.94 |
| BCJ+LZMA2 | 15% | 840,054 | 1,179,648 | UnexpectedEof | 1,179,648 | EOF (masked) | 1,179,648 | 14.06 |
| BCJ+LZMA2 | 20% | 1,120,083 | 1,638,400 | UnexpectedEof | 1,638,400 | EOF (masked) | 1,638,400 | 19.53 |
| BCJ+LZMA2 | 25% | 1,400,111 | 1,966,080 | UnexpectedEof | 1,966,080 | EOF (masked) | 1,966,080 | 23.44 |
| BCJ+LZMA2 | 50% | 2,800,255 | 4,063,232 | UnexpectedEof | 4,063,232 | EOF (masked) | 4,063,232 | 48.44 |
| Delta(4)+LZMA2 | 1% | 57,276 | 0 | UnexpectedEof | 0 | EOF (masked) | 0 | 0.00 |
| Delta(4)+LZMA2 | 5% | 286,512 | 327,680 | UnexpectedEof | 327,680 | EOF (masked) | 327,680 | 3.91 |
| Delta(4)+LZMA2 | 10% | 573,057 | 786,432 | UnexpectedEof | 786,432 | EOF (masked) | 786,432 | 9.38 |
| Delta(4)+LZMA2 | 11% | 630,366 | 851,968 | UnexpectedEof | 851,968 | EOF (masked) | 851,968 | 10.16 |
| Delta(4)+LZMA2 | 12% | 687,674 | 917,504 | UnexpectedEof | 917,504 | EOF (masked) | 917,504 | 10.94 |
| Delta(4)+LZMA2 | 15% | 859,601 | 1,114,112 | UnexpectedEof | 1,114,112 | EOF (masked) | 1,114,112 | 13.28 |
| Delta(4)+LZMA2 | 20% | 1,146,146 | 1,572,864 | UnexpectedEof | 1,572,864 | EOF (masked) | 1,572,864 | 18.75 |
| Delta(4)+LZMA2 | 25% | 1,432,690 | 2,031,616 | UnexpectedEof | 2,031,616 | EOF (masked) | 2,031,616 | 24.22 |
| Delta(4)+LZMA2 | 50% | 2,865,413 | 4,063,232 | UnexpectedEof | 4,063,232 | EOF (masked) | 4,063,232 | 48.44 |

Findings:
- Every filtered byte emitted is correct (filtered prefix == LZMA2 prefix in all rows). Neither the BCJ x86 nor the Delta reader held back or corrupted bytes at the truncation point in these runs.
- The LZMA2 stage emits output in chunk-sized steps (multiples of 65,536 B: 327,680 = 5 x 64 KiB, 786,432 = 12 x 64 KiB, 917,504 = 14 x 64 KiB). So the usable prefix is chunk-granular, not byte-granular; the bytes after the last complete chunk are lost. The step granularity was not checked against LZMA2 chunk headers.
- lzma-rust2's BcjReader and DeltaReader convert the inner UnexpectedEof into a plain EOF, so the filter stage reports a clean end. Callers must detect truncation from the LZMA2 stage (or by size/CRC), not from the filter reader.
- Reverse filter used: lzma-rust2 public readers (not hand-ported). Filter ids confirmed in the py7zr header dump (03030103 = BCJ x86; 03 = Delta).

### 7-Zip BCJ selection (CPP/7zip/Archive/7z/7zUpdate.cpp, fetched from ip7z/7zip main)
- `g_Exe_Exts[] = { "dll", "exe", "ocx", "sfx", "sys" }`; `g_ExeUnix_Exts[] = { "so", "dylib" }` (the unix list is commented out in the non-Unix build).
- In the analysis loop, a file with an extension is content-read only when its extension is in the exe list (`IsExt_Exe(ext)` -> `needReadFile = ParseExe`), the unix list, or `wav` (`ParseWav`). Files with no extension follow `ParseNoExt`. Other extensions are not read.
- For `.mkv` / `.mp4` / `.iso`: the extension is not in any list, so the file is not content-parsed and no BCJ filter is chosen by the analyzer. Unverified caveat: I did not trace every caller of `needReadFile` or the `-mf` default in other files (e.g. UpdateCallback.cpp); this is from the 7zUpdate.cpp branch only.
- `IsExeFilter()` lists ARM64, RISCV, BCJ, BCJ2, ARM, ARMT, PPC, SPARC, IA64 as exe filters, and `Get_FilterGroup_for_Folder` groups folders by that.

## Task 2: RAR5 -hp multi-volume salt (rars 0.12.3 writer, self-consistency only)

Status: VERIFIED for rars 0.12.3 WRITER output. NOT verified against WinRAR/RAR 7 output.

### Setup
- API: `rars::rar50::write_streaming_volumes_to` with `WriterOptions` (`o.features.header_encryption = true`), `ArchiveEntry::new(name, EntrySource::from_bytes(data)).with_password("pw")`, `max_payload_per_volume = 64 KiB`, a `VolumeSink` that collects per-volume buffers, `WriterResources::default()`.
- Payload: 300,000 B xorshift random, f1.bin; f2.bin = first 100,000 B of it. Both members encrypted (writer rejects mixing encrypted and plain members).
- Result: 7 volumes (65,866 / 65,866 / 65,866 / 65,866 / 66,010 / 65,866 / 7,114 B). Probe: /tmp/claude-0/-home-claude/1315c443-c098-58da-9f63-b135a00caf8b/scratchpad/w8a/probe/src/bin/rar5hp.rs; volumes hp.part1..7.rar in the same scratchpad folder.

### HEAD_CRYPT (type 4, first block after signature, unencrypted)

| volume | HEAD_CRYPT hsize | encver | encflags | kdf_count log2 | salt (16 B) | check (12 B) |
|---|---|---|---|---|---|---|
| 1..7 (all) | 33 | 0 | 1 (check present) | 15 | f8ed64994c012694123ca5ac575fd870 | c0420700290bea0c1662c596 |

All 7 volumes carry byte-identical HEAD_CRYPT salt and check values.

### File-encryption record (extra 0x01, per member, read by parsing with the password)

| member | volumes where it appears | salt (16 B) | iv (16 B) | check (12 B) |
|---|---|---|---|---|
| f1.bin | 1, 2, 3, 4, 5 | d178ce1cfd45d7632925b52296c19647 | 573a075c2fe1bd7569efbf31f238f12f | 9ab6ca79724f9a67cf2b195c |
| f2.bin | 5, 6, 7 | 71a65d4d90e1af9f79e96d4571e23adc | 7fe0bdcea664a963ebacb07c1efa089e | 1cb45418d113b38e7f821443 |

Each member's file-level salt, iv and check repeat unchanged in every volume that carries a copy of its header. The two members use different salts. Header salts are not per-volume.

Caveats: the probe parses via `rars::rar50::Archive::parse_owned_with_password` (rar50.rs `FileEncryption`, which exposes salt/iv/check). Only the rars writer was tested. Real RAR5 writers may use different salt policies (per-volume salt or per-block salt). That is untested here.

## Task 3: RAR 2.0 incremental patch

NOT ATTEMPTED (time cap). No copy was vendored and no incremental-flush edit was made. The rars source's codec directory (src/codec/) has canonical, fast, filters, huffman; the RAR 2.0 decoder file was not located in this pass.
