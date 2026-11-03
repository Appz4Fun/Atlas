# W4a: Prefix decode of truncated RAR and truncated real .7z

Follow-up to `w3a-prefix-decode.md`. Goal: from the first P% of a packed archive (what an indexer has from the first articles), get a correct decoded prefix.

Scratch and probe: `/tmp/claude-0/-home-claude/1315c443-c098-58da-9f63-b135a00caf8b/scratchpad/w4a/` (probe source `probe/src/main.rs`, binary `w4a_probe`, `./w4a_probe rar` and `./w4a_probe 7z`). Crates: rars 0.12.3, lzma-rust2 0.25.0, py7zr 1.1.4.

## Summary

- RAR, (a) zero-pad to original length, then plain `rars` extract: fails for 3 of 4 fixtures. Zero output for RAR3 PPMd, RAR 1.54 and RAR5 at all cuts, because rars rejects the zeroed headers or CRCs before any output. RAR3 LZ (unpack20) emits correct data before the cut (12,775 B correct from a 25% cut; 13,977 B from 50%).
- RAR, (b) direct decoder on packed data: NOT completed within the time cap. The entry points are identified (below).
- 7z LZMA2 with 64 MiB dict fed from offset 32 of a truncated real .7z: correct prefix at every cut, starts with `1A 45 DF A3`. Output is 786,432 / 2,031,616 / 4,128,768 B at 10 / 25 / 50% (all matched the original byte-for-byte).
- 7z LZMA1 with props 0x5D and 64 MiB dict, unknown size: same, 786,432 / 2,097,152 / 4,194,304 B, all correct prefixes.
- The LZMA2 stream carries its own lc/lp/pb in the first chunk header (control byte 0xE0), so only the dictionary size is missing from the end header.

## 1. RAR

### (a) Zero-padded truncation through rars 0.12.3

Method: bytes `[0..cut]` of the real fixture, the rest zero-filled to the full file length, then `rars::ArchiveReader::read_reader` and `extract_to` with a shared sink. Reference is the full decode. "Longest correct prefix" is the common prefix with the reference. Output past the cut is garbage and is not counted in it.

| fixture | cut % | bytes of real data | rars output bytes | longest correct prefix | error (first) |
|---|---|---|---|---|---|
| RAR3 PPMd farmanager170.rar (ref 3,343,305 B) | 10 | 99,616 | 0 | 0 | checksum mismatch |
| RAR3 PPMd farmanager170.rar | 25 | 249,041 | 0 | 0 | invalid header: RAR 1.5 block header is too short |
| RAR3 PPMd farmanager170.rar | 50 | 498,083 | 0 | 0 | invalid header: RAR 1.5 block header is too short |
| RAR 1.54 doc_154_best.rar (ref 502,961 B) | 10 | 18,939 | 0 | 0 | invalid header |
| RAR 1.54 doc_154_best.rar | 25 | 47,347 | 0 | 0 | invalid header |
| RAR 1.54 doc_154_best.rar | 50 | 94,695 | 0 | 0 | invalid header |
| RAR5 m3_default.rar (ref 65,536 B) | 10 | 1,000 | 0 | 0 | checksum mismatch at archive offset 0x26a2 |
| RAR5 m3_default.rar | 25 | 2,501 | 0 | 0 | checksum mismatch at archive offset 0x26a2 |
| RAR5 m3_default.rar | 50 | 5,003 | 0 | 0 | checksum mismatch at archive offset 0x26a2 |
| RAR3 LZ unpack20_multiblock.rar (ref 16,384 B) | 10 | 482 | 0 | 0 | RAR 2.0 bitstream is truncated |
| RAR3 LZ unpack20_multiblock.rar | 25 | 1,206 | 16,384 | 12,775 | checksum mismatch (entry multiblock.bin) |
| RAR3 LZ unpack20_multiblock.rar | 50 | 2,413 | 16,384 | 13,977 | checksum mismatch (entry multiblock.bin) |

Reading: rars does not gate on a file-length check alone. It validates the zeroed headers and the packed-data CRC, so the zero tail stops it early. The RAR5 error offset (0x26a2) is inside the zeroed region, so the RAR5 fixture keeps its header/metadata at the end of the file. The LZ case is the only one that emits data, and it emits a correct prefix up to the cut, then an error. So (a) is a partial route only for RAR3 LZ, and fails for PPMd and RAR5.

### (b) Direct decoder on the packed slice (not completed)

Identified public entry points in rars 0.12.3 (`pub mod codec` is exported from `lib.rs`), so they can be called without `ArchiveReader`:

```rust
// RAR5 (src/codec/rar50.rs:518). Needs algorithm_version, output_size, dictionary_size from the file header.
let mut dec = rars::codec::rar50::Unpack50Decoder::new();
dec.decode_member_from_reader_with_dictionary_to_sink(
    &mut packed_reader, algorithm_version, output_size, dictionary_size, /*solid*/ false,
    |chunk| { out.write_all(chunk.bytes())?; Ok::<_, std::io::Error>(()) },
)?;   // check the DecodedChunk accessor name in src/codec/rar50.rs before use

// RAR3 LZ (Unpack29/RAR 2.9 path, src/codec/rar29.rs:162)
let mut d = rars::codec::rar29::Unpack29::new();
d.decode_member_from_reader(&mut packed_reader, output_size, &mut out)?;

// RAR3 PPMd: src/codec/ppmd.rs (PpmdDecoder::decode_init / decode_symbol, line ~1020/1058). Not wired up.
```

Not done: the packed-data offset needs header parsing (RAR5: sig 8 B + header CRC + header-size vint + block fields; RAR4: block start + HEAD_SIZE), and the RAR3 PPMd props (order, memory) come from the file header. Next step if needed: parse the first file header in each fixture, then call the decoders above on `full[data_off..cut]` and compare the LCP to the full decode.

## 2. 7z (real archives from py7zr 1.1.4)

Fixtures: `in/mkv_like.bin` is 8 MiB, starting `1A 45 DF A3`, text 4,000 B blocks alternated with 64 KiB of random data. `mkv_lzma2.7z` (8,007,350 B, LZMA2 preset 6) and `mkv_lzma1.7z` (8,012,728 B, LZMA1 preset 6, props 0x5D). Both non-solid, single file. Start header read: next-header offset 8,007,295 (LZMA2) / 8,012,673 (LZMA1), next-header size 23 B. Packed stream starts at offset 32.

Truncation: the .7z is cut at P% of its total length, then the 32-byte start header is dropped and the rest is fed raw into the decoder. The 64 MiB dictionary is assumed (the real dict is in the missing end header; a larger dict is safe because a valid stream never references beyond the real dict).

| container | coder | bytes fed from offset 32 | cut % | output bytes | correct prefix bytes | starts `1A 45 DF A3` | terminal |
|---|---|---|---|---|---|---|---|
| mkv_lzma2.7z | LZMA2 (21), 64 MiB dict | 800,703 | 10 | 786,432 | 786,432 | yes | UnexpectedEof |
| mkv_lzma2.7z | LZMA2 | 2,001,805 | 25 | 2,031,616 | 2,031,616 | yes | UnexpectedEof |
| mkv_lzma2.7z | LZMA2 | 4,003,643 | 50 | 4,128,768 | 4,128,768 | yes | UnexpectedEof |
| mkv_lzma1.7z | LZMA1 (030101), props 0x5D, 64 MiB dict, size unknown (u64::MAX) | 801,240 | 10 | 786,432 | 786,432 | yes | UnexpectedEof |
| mkv_lzma1.7z | LZMA1 | 2,003,150 | 25 | 2,097,152 | 2,097,152 | yes | UnexpectedEof |
| mkv_lzma1.7z | LZMA1 | 4,006,332 | 50 | 4,194,304 | 4,194,304 | yes | UnexpectedEof |

Notes:
- Output counts are multiples of 256 KiB (lzma-rust2 hands out dictionary-flush chunks). The usable prefix is coarser than the cut. The prefix is still byte-correct.
- The 0x5D props for LZMA1 are the py7zr/preset-6 default. A real archive with other lc/lp/pb would decode garbage; check the header if available. A mismatch shows up as no `1A 45 DF A3` at the start, so the magic check doubles as a props sanity check.
- Our LZMA2 test shows the first packed bytes `E0 FC 0A EF FE 5D 00 0D`. The first chunk (control 0xE0) carries the props byte 0x5D, so LZMA2 needs only the dict size from outside the packed stream.

## Code

```rust
// 7z: parse the 32-byte start header, then feed packed bytes from offset 32.
let full = std::fs::read(path)?;                       // only the first P% needs to be on disk
assert_eq!(&full[..6], &[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]);
let next_hdr_off  = u64::from_le_bytes(full[12..20].try_into()?); // informational
let cut = full.len() * pct / 100;
let body = full[32..cut].to_vec();                     // packed stream starts at 32 (PackPos = 0 here)

// LZMA2 (coder 21): dict only. Props are inside the first chunk.
let r = lzma_rust2::Lzma2Reader::new(Cursor::new(body), 64 << 20, None);

// LZMA1 (030101): unknown size, lc/lp/pb 0x5D. Dict 64 MiB.
let r = lzma_rust2::LzmaReader::new_with_props(Cursor::new(body), u64::MAX, 0x5D, 64 << 20, None)?;

// drain until Err: keep output written before the error.
let mut out = Vec::new(); let mut buf = vec![0u8; 65536];
loop { match r.read(&mut buf) { Ok(0) => break, Ok(n) => out.extend_from_slice(&buf[..n]), Err(_) => break } }
```

```rust
// RAR zero-padded truncation (part (a)).
let mut padded = full[..take].to_vec();
padded.resize(full.len(), 0);
let arc = rars::ArchiveReader::read_reader(std::io::Cursor::new(padded))?;
arc.extract_to(None, move |_m| Ok(Box::new(Sink(buf.clone())) as Box<dyn std::io::Write>))?;
// Sink: a Write that appends to Arc<Mutex<Vec<u8>>>, so bytes emitted before the error survive.
```

## Open items

- RAR (b) direct-decoder path: not built. Needs RAR5/RAR3 header parsing for the data offset and the compression params.
- 7z: a real end header (dict prop) was not parsed. The test used the fixed 64 MiB dict.
- Not tested: solid 7z, multi-file 7z (first file prefix only), BCJ/ARM filters, and 7z with a non-default LZMA2 dict size.
