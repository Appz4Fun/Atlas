# nntp-xover-engine

Columnar, compressed storage for NNTP overview lines (RFC 3977 `XOVER` / `OVER`), read in place from an mmap. Atlas uses it for the overview archive (`src/overview_archive.rs`), but the crate has no Atlas dependency.

- Every line comes back byte for byte. Each row is either taken apart by the codecs and checked at ingest to write back the same, or kept whole in a literal dictionary.
- `XOVER start-end`: a checkpoint every 128 rows, so a row is at most 127 varint steps from a known state, and a range walks one step per row.
- `OVER <message-id>`: an 8-bit XOR filter per block (about 1.23 bytes a row, 0.39% false positives) rules a block out in three memory reads. A 256-bucket CSR table of (tag, row) then finds the row, confirmed by an exact compare.
- Streaming writes into a connection-scoped 64 KB buffer that never grows, and flushes it into a Tokio `AsyncWrite`. The previous row's dictionary strings and date are cached by id, so a run of articles from one post decodes no FSST at all.
- Cold blocks zstd the body but keep the header, checkpoints and filter uncompressed at the front. Routing across many cold blocks then reads nothing compressed until a block matches.

## Layout

```
0     NxvBlockHeader (64)              magic NXVB, version, flags (cold, Xref: prefix), art/time range, rows, crc
64    SectionEntry x 16 (256)          offset, length, count per section, all 8-byte aligned
320   SkipCheckpoints  (64 per 128 rows)   \  the cold prefix:
      MsgIdXor8        (16 + 3*B bytes)    /  never compressed
      FsstTables       dictionary directory, FSST symbol tables, xref slot counts, subject counter masks
      MsgIdBucketIndex [u32; 257] offsets, N tags, N u16 rows
      SubjTplIds, SubjPadMasks, XrefSkelIds, AuthorIds, DomainIds, MsgIdPfxIds   (&[u16], one per row)
      MsgIdTailOffsets (&[u32])  StringPoolOffsets (&[u32])  StringPoolData  MsgIdTailData
      NumericVarints   one interleaved zigzag-LEB128 stream, row after row
      ReferencesData   length-prefixed, structured rows only
```

Each field and how it is stored:

| Field | Stored as |
|---|---|
| article number | delta from the previous row |
| subject | template with `[n/m]`, `(n/m)` and `.NN.` lifted out. Dictionary id, plus a PadMask of leading-zero widths, plus only the counters the template has, as deltas. If the subject is the message-id's local part (obfuscating posters do that), only a flag is stored. |
| from | dictionary id |
| date | UTC second as a delta, plus a style word for weekday, day padding, 2- or 4-digit year and zone, written back with Hinnant's `civil_from_days`. The style word is stored only when it changes. |
| message-id | domain id and local-prefix id (dictionaries), plus the tail packed by alphabet: hex 4 bits a char, Usenet base64 6 bits, otherwise raw |
| references | length and bytes |
| bytes, lines | deltas. An empty field is a style flag and stores no value. |
| Xref | skeleton id with each `group:N` number lifted out. Slot 0 is stored against the row's own article number. Every slot is a delta from the last row in the sub-block with the same skeleton, so a cross-posted post moves each group's number only a little. |

Each dictionary is stored raw, FSST-coded, or alphabet-packed (for random strings FSST can't shrink), whichever is smallest.

## Measurements

`cargo run --release --bin nxv-bench` runs the 10,000-row synthetic corpus. `nxv-bench FILE...` runs files of raw overview lines. The real files below are two slices of 25,000 rows from each group, fetched from one server on 2026-11-07. Times are on an Apple M-series laptop that was also running Atlas, so they are approximate. A cold block's random read includes decompressing its whole block.

| Data | Raw | zstd -3 per row + 64 KB dict | zstd -19 whole batch (no random access) | .nxv hot | .nxv cold |
|---|---:|---:|---:|---:|---:|
| synthetic, 10k rows | 2.7 MB | 3.73x | 11.40x | 5.68x | 10.75x |
| alt.binaries.teevee, 50k rows | 15.5 MB | 3.28x | 7.81x | 5.25x | **8.09x** |
| alt.binaries.multimedia, 50k rows | 12.1 MB | 2.95x | 5.98x | 3.91x | 5.87x |
| alt.binaries.boneless, 50k rows | 12.2 MB | 2.47x | 3.87x | 2.65x | 3.40x |

| teevee, 50k rows | Ingest | Random row by article | OVER <message-id> (filter, bucket, compare, row) | Stream XOVER |
|---|---:|---:|---:|---:|
| zstd -3 per row + dict | 57 MB/s | 296 ns | n/a | 952 MB/s |
| .nxv hot | 341 MB/s | 1.5 µs | 1.6 µs | 1.57 GB/s (12.5 Gbps) |
| .nxv cold | 244 MB/s | 1.8 ms (decompress 1 block) | 2.0 ms | 1.0 GB/s |

Against the targets in the brief:

- Hot 8 to 10x and cold 20 to 28x don't hold on real data. Message-ids, and in obfuscated groups the subjects and posters too, are random strings. No codec gets below their entropy, and they are most of what is left: 28 to 37% of a hot block is packed message-id tails. Cold teevee beats whole-batch zstd -19 while keeping random access. Boneless, almost all random, ends about 12% behind it.
- Streaming reaches 1.1 to 1.6 GB/s on real data and 1.95 GB/s on the synthetic corpus, on one core of a busy machine: close to the 2 GB/s target, not past it. Random access is bounded by the 127-row walk, not O(1) like a fixed-width table.
- The u16 id columns and u32 offset arrays the format requires for zero-copy reads cost about 18 bytes a row. That is the main gap between hot and cold.
- Zero literal rows on all three real groups. The codecs learned 2-digit years, empty `Lines`, per-article domains and subject-equals-message-id from these samples.

## Format notes

- Blocks hold up to 65,536 rows, sorted by article number. The CSR bucket offsets are u32, because 65,536 doesn't fit a u16.
- Dates take more shapes than the fixed 31-byte `Dow, DD Mon YYYY HH:MM:SS +0000`, so the formatter writes into a 40-byte buffer. The cache patches only MM:SS while the hour is unchanged.
- A checkpoint's narrow fields (`i32`) hold 0 when the first row's value doesn't fit. Deltas are always taken against exactly what the checkpoint holds, so this changes nothing.
- The reader assumes a little-endian host (compile error otherwise). It checks alignment, bounds and the header CRC before casting any section.
