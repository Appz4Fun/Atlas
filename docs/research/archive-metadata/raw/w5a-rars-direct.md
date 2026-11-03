# W5a: direct rars codec decode of truncated RAR packed data

Follow-up to `w4a-rar-7z-partial.md` (part (b), not done there). Goal: call rars 0.12.3 codec entry points on a cut packed-data slice and see how much correct prefix comes out. The cut is on the packed bytes of one member (not on the file), so the percentages are of `packed.len()`.

Probe: `/tmp/claude-0/-home-claude/1315c443-c098-58da-9f63-b135a00caf8b/scratchpad/w4a/probe/src/bin/direct.rs` (built as `target/release/direct`). Generated large RAR5: `/tmp/claude-0/-home-claude/1315c443-c098-58da-9f63-b135a00caf8b/scratchpad/w4a/w5a/gen_big_rar5.rar`.

## Results

Method: the full file is parsed with rars' own parser (`rars::rar50::Archive::parse` / `rars::rar15_40::Archive::parse`) to get the packed range, unpacked size, dictionary size and algorithm version. The packed slice is cut at 10/25/50/100% and fed to the codec. "Correct prefix" is the common prefix with the full-decode output. Bytes the codec emitted before the error are kept.

| fixture | packed bytes (full) | unpacked | cut % | packed fed | decoder output | correct prefix | terminal |
|---|---|---|---|---|---|---|---|
| RAR5 gen_big_rar5.rar (8 MiB, rars writer, LZ, dict 4 MiB) | 7,912,191 | 8,388,608 | 10 | 791,219 | 786,432 | 786,432 (9.4% of ref) | NeedMoreInput |
| RAR5 gen_big_rar5.rar | 7,912,191 | 8,388,608 | 25 | 1,978,047 | 2,097,152 | 2,097,152 (25.0%) | NeedMoreInput |
| RAR5 gen_big_rar5.rar | 7,912,191 | 8,388,608 | 50 | 3,956,095 | 4,194,304 | 4,194,304 (50.0%) | NeedMoreInput |
| RAR5 gen_big_rar5.rar | 7,912,191 | 8,388,608 | 100 | 7,912,191 | 8,388,608 | 8,388,608 (full, == original input) | OK |
| RAR5 m3_default.rar (WinRAR-made, dict 128 KiB) | 9,784 | 65,536 | 10 | 978 | 0 | 0 | NeedMoreInput |
| RAR5 m3_default.rar | 9,784 | 65,536 | 25 | 2,446 | 0 | 0 | NeedMoreInput |
| RAR5 m3_default.rar | 9,784 | 65,536 | 50 | 4,892 | 0 | 0 | NeedMoreInput |
| RAR5 m3_default.rar | 9,784 | 65,536 | 100 | 9,784 | 65,536 | 65,536 (full) | OK |
| RAR3 unpack20_multiblock.rar (unp_ver 20, method 0x35) | 4,761 | 16,384 | 10 | 476 | 0 | 0 | RAR 2.0 bitstream is truncated |
| RAR3 unpack20_multiblock.rar | 4,761 | 16,384 | 25 | 1,190 | 0 | 0 | RAR 2.0 bitstream is truncated |
| RAR3 unpack20_multiblock.rar | 4,761 | 16,384 | 50 | 2,380 | 0 | 0 | RAR 2.0 bitstream is truncated |
| RAR3 unpack20_multiblock.rar | 4,761 | 16,384 | 100 | 4,761 | 16,384 | 16,384 (full) | OK |
| RAR3 PPMd farmanager170.rar | - | - | - | - | - | - | NOT RUN (see Notes) |

Verification: the generated RAR5 full decode matches the 8 MiB source byte-for-byte (`matches_orig=Some(true)`). The two RAR fixtures match rars' own full decode.

Comparison with w4a part (a) (zero-padding the file and running `extract_to`): for unpack20 that path gave a correct prefix of 12,775 B at 25%, because the zero tail lets the decoder run to the end and write, so garbage follows the prefix. The direct decode gives 0 for the same cut, because the input ends early and the decoder errors before it writes anything (see source notes).

## Source findings (rars 0.12.3)

1. RAR5 (`src/codec/rar50.rs`, `decode_to_sink_with_filters`, ~line 963-1080). Output is streamed. `StreamingOutput` holds bytes in `pending` and calls the sink only when `pending.len() >= STREAM_FLUSH_THRESHOLD` (64 KiB, line 33) or at `finish`. So on error, up to 64 KiB of decoded tail is lost. The results show this: every cut output is a multiple of 64 KiB (786,432 = 12 x 64K, 2,097,152 = 32 x 64K, 4,194,304 = 64 x 64K).
2. RAR5 block granularity. The loop calls `read_compressed_block_with_allowance(input, ...)` (line ~1011; the helper at line 1689 uses `read_exact` for the whole block payload) before decoding any symbol of that block. A cut inside a block gives NeedMoreInput and zero output for that block. This is why m3_default (one block) gives 0 at 50%. The generated file has several blocks, so output comes in per-block steps.
3. RAR 2.0 / unpack20 (`src/codec/rar20.rs`, `decode_member_from_reader`, lines 232-264). This is eager: `self.bits.input.read_to_end(input)?` reads the whole input first, then `decode_until(target)`, then one `out.write_all(decoded)` at the end. A truncated input therefore fails before any byte reaches `out`. There is no flush point to patch to fix this, because the input read is eager.
4. RAR 2.9 (`src/codec/rar29.rs`, `decode_bits_member_to`, line 447-500) writes in `STREAM_CHUNK` = 1 MiB steps (line 41). It is the only path that streams output in chunks. Not tested here: no RAR 2.9 LZ fixture was in the set, and farmanager170 is PPMd.

## Code

```rust
// RAR5: direct streaming decode of a (possibly truncated) packed slice. Keeps all bytes the sink saw.
use rars::codec::rar50::{DecodedChunk, Unpack50Decoder};
use std::io::Cursor;

fn rar5_decode(packed: &[u8], alg: u8, out_size: usize, dict: usize) -> (Vec<u8>, String) {
    let mut out: Vec<u8> = Vec::new();
    let mut dec = Unpack50Decoder::new();
    let mut rd = Cursor::new(packed);            // EOF after the cut
    let r = dec.decode_member_from_reader_with_dictionary_to_sink(
        &mut rd, alg, out_size, dict, /*solid*/ false,
        |c: DecodedChunk<'_>| -> Result<(), std::io::Error> {
            match c {
                DecodedChunk::Bytes(b) => out.extend_from_slice(b),
                DecodedChunk::Repeated { byte, len } => out.resize(out.len() + len, byte),
            }
            Ok(())
        },
    );
    let e = match r { Ok(()) => "OK".to_string(), Err(e) => format!("{e:?}") };
    (out, e)
}

// Parameters come from rars' own parser on the FULL file:
//   let arc = rars::rar50::Archive::parse(&full)?;
//   let f = arc.files().find(|f| !f.is_directory()).unwrap();
//   let ci = f.decoded_compression_info()?;          // algorithm_version, dictionary_size, solid
//   let packed = f.packed_data(&arc)?;               // block.data_range bytes
//   let out_size = f.unpacked_size as usize;
```

```rust
// RAR 2.0 (unpack20, unp_ver 20/26): direct decode of a packed slice.
fn rar3lz_decode(packed: &[u8], out_size: usize) -> (Vec<u8>, String) {
    let mut out: Vec<u8> = Vec::new();
    let mut d = rars::codec::rar20::Unpack20::new();
    let r = d.decode_member_from_reader(&mut Cursor::new(packed), out_size, &mut out);
    let e = match r { Ok(()) => "OK".to_string(), Err(e) => format!("{e}") };
    (out, e)
}
// Parameters: let arc = rars::rar15_40::Archive::parse(&full)?;
//   let f = arc.files().find(|f| !f.is_directory()).unwrap();
//   let packed = &full[f.packed_range.clone()];   // len == f.pack_size
//   out_size = f.unp_size; f.unp_ver == 20 here; f.method == 0x35
```

Large RAR5 test input was made with rars' own writer (not WinRAR; no `rar`/`unrar`/`7z` binaries in the container, and no RAR5 fixture over 10 KB in rars' tests):

```rust
let mut b = rars::Builder::new(rars::ArchiveVersion::Rar50);
b.add_bytes(b"mkv_like.bin".to_vec(), input_bytes, None, None)?;
let bytes = b.to_bytes()?;     // 8 MiB input -> 7,912,338 B archive
```

## Notes and open items

- Usable prefix for RAR5 (rars-written, large): about 10/25/50% of the packed bytes gives a correct prefix of 9.4/25.0/50.0% of the unpacked data, minus up to 64 KiB of pending tail and up to one compressed block. The block size depends on the writer; WinRAR blocks may differ. Not verified on a WinRAR-made large file (none in the container).
- For WinRAR-made small members (m3_default, one block), a truncated cut gives zero output. The indexer would need the whole block first.
- Patch options (not built):
  - RAR5: when the block read hits EOF, decode only the symbols whose bits are all inside the available payload (the bit reader already tracks `bit_pos`), then return the state so the loop stops. Flush `pending` on that exit path. Moderate change (the decoder state, reps, tables and history already live in the struct).
  - RAR 2.0: replace `read_to_end` with an incremental bit reader and write in chunks, as `rar29.rs` already does. Larger change.
  - "Flush on window wrap" alone does not help RAR 2.0 (input is read eagerly) and only helps RAR5 by reducing the 64 KiB tail, not the block granularity.
- PPMd (RAR 2.9 method 0x30-0x3x, farmanager170.rar): `codec::ppmd::PpmdDecoder::decode_init(first_byte, input, esc_char)` and `decode_symbol(input)` are public, with a `PpmdByteReader` trait (`src/codec/ppmd.rs`, lines 501-503, 1020, 1058). Not wired in this run; it needs the RAR 2.9 PPMd member props from the file header. Next step.
- Cut was on the packed slice, not the file; the earlier w4a percentages were on the file.
