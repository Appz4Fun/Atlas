# W7c: RAR 2.9 (unp_ver 29) truncated-decode, rar20 patch idea, 7z filters

Time-capped run (10 min). Tasks 1 and 2 done. Task 3 partly (source read only, no vendored build). Task 4 NOT run.

Probe: `/tmp/claude-0/-home-claude/1315c443-c098-58da-9f63-b135a00caf8b/scratchpad/w4a/probe/src/bin/rar29.rs` (binary `target/release/rar29`, modes `scan`, `cut`, `gen`). Output buffer is a plain `Vec<u8>` that persists across errors. Cut is on the packed member bytes, as in w5a.

## Fixtures found (rars tests/fixtures, unp_ver 29)

| member | method byte | unp size | pack size | notes |
|---|---|---|---|---|
| ppmd/farmanager170.rar `Far.exe` | 0x35 | 724,992 | 342,317 | largest PPMd-era fixture (real WinRAR) |
| rarvm/filter_bsdcat_exe.rar `bsdcat.exe` | 0x33 | 204,288 | 94,232 | largest LZ-era fixture (real) |
| rar300/rarvm_audio_stereo_rar300.rar | 0x35 | 705,644 | 15,688 | |

No real-world RAR 2.9 fixture exceeds 1 MiB unpacked. The `cut` results below for the fixtures are all 0 bytes before the end, because the decoder only emits output in 1 MiB steps (see source).

## Results

### Real fixtures (below 1 MiB, so no partial flush is possible)

| fixture | cut % | packed fed | decoder out | correct prefix | terminal |
|---|---|---|---|---|---|
| farmanager170 Far.exe (PPMd era, 0x35) | 10 | 34,231 | 0 | 0 | bitstream truncated |
| same | 25 | 85,579 | 0 | 0 | bitstream truncated |
| same | 50 | 171,158 | 0 | 0 | bitstream truncated |
| same | 100 | 342,317 | 724,992 | 724,992 (full) | OK |
| bsdcat.exe (LZ, 0x33) | 10 | 9,423 | 0 | 0 | bitstream truncated |
| same | 25 | 23,558 | 0 | 0 | bitstream truncated |
| same | 50 | 47,116 | 0 | 0 | bitstream truncated |
| same | 100 | 94,232 | 204,288 | full | OK |
| rarvm_audio_stereo (0x35) | 10/25/50 | 1,568 / 3,922 / 7,844 | 0 | 0 | bitstream truncated |

farmanager170 `FarEng.hlf` (solid follower) fails on full decode with "PPMd block reuses missing model", because it needs the prior solid member's state. This is expected for a standalone decode and is not a truncation result.

### Generated 8 MiB archives (rars writer, RAR 2.9 target)

| archive | unp | packed | cut % | packed fed | decoder out | correct prefix | % of unp | terminal |
|---|---|---|---|---|---|---|---|---|
| `rar29_lz_mix8m.rar` (LZ; 4 MiB text + 4 MiB random) | 8,388,608 | 5,782,200 | 10 | 578,220 | 1,048,576 | 1,048,576 | 12.5% | bitstream truncated |
| same | | | 25 | 1,445,550 | 3,145,728 | 3,145,728 | 37.5% | truncated |
| same | | | 50 | 2,891,100 | 5,242,880 | 5,242,880 | 62.5% | truncated |
| same | | | 100 | 5,782,200 | 8,388,608 | 8,388,608 | 100% | OK, equals the original input (`cmp`) |
| `rar29_ppmd_text8m.rar` (text; writer asked for PPMd) | 8,388,608 | 2,080,358 | 10 | 208,035 | 0 | 0 | 0% | truncated |
| same | | | 25 | 520,089 | 2,097,152 | 2,097,152 | 25.0% | truncated |
| same | | | 50 | 1,040,179 | 4,194,304 | 4,194,304 | 50.0% | truncated |
| same | | | 100 | 2,080,358 | 8,388,608 | 8,388,608 | 100% | OK |

Notes on the PPMd archive: the writer's PPMd choice is in-stream (RAR 2.9 signals PPMd inside the block stream, not in the method byte), so the header still shows 0x33. This was confirmed by the `rar29` decoder's `BlockMode::Ppmd` path (`rar29.rs:529`, `:698`), not by a separate byte check. The 10% cut gives 0 bytes because the decoder never reached the first 1 MiB flush point before input ran out.

## Source findings

1. Output is streamed in 1 MiB steps. `src/codec/rar29.rs:41` (`STREAM_CHUNK = 1024 * 1024`); `decode_bits_member_to` (lines 447-500) writes each `safe_end` range to `out` as it goes. A truncated stream therefore keeps every full 1 MiB chunk decoded before the error. Below 1 MiB, nothing is emitted.
2. Input is read eagerly. `rar29.rs:428` is `self.bits.input.read_to_end(input)?` inside `decode_member_from_reader`. This is the same eager pattern as RAR 2.0 (`rar20.rs:232-264`). It does not break prefix output here (the truncated slice just ends early), but for a network stream it means the whole packed member is buffered before any output. This is a memory and latency cost, not a correctness one.
3. Truncation error mapping: `rar29.rs` maps `Error::NeedMoreInput` to `InvalidData("RAR 2.9 bitstream is truncated")` (around lines 462-470). The decoder stops at the first symbol that needs bytes past the cut, so output stops at the last completed 1 MiB boundary.
4. The per-1 MiB flush is the only granularity. The LZ output at 10% cut (1 MiB) and the PPMd output at 25% cut (2 MiB) both sit on 1 MiB boundaries.

## Task 3: rar20 (RAR 2.0) patch idea

Not built. The idea from w5a still holds: `rar20.rs:232-264` calls `read_to_end` and then writes once, so a truncated stream yields 0 bytes. The minimal patch is to read the input in refill chunks (as `rar29` does for output) and write each decoded range to `out` at a safe boundary. Patch size: moderate. The rar20 bit reader has to refill on demand instead of relying on the full buffer. Not tested in a vendored copy this run.

## Task 4: 7z filter chains

Not run. Time cap reached. `lzma-rust2` 0.25 and `sevenz-rust2` 0.24 are present in the cargo registry (`~/.cargo/registry/src/*/`). Checking their BCJ/Delta exports and testing the py7zr LZMA2+BCJ and LZMA2+Delta cases is the next step.

## Summary of numbers

- RAR 2.9 LZ and PPMd decoders are correct on full decode for all fixtures and generated archives (checked against the original input for the LZ one).
- Truncated 8 MiB archives keep 1 MiB-granular prefixes: LZ 10/25/50% cut gives 1, 3, 5 MiB correct prefix; PPMd gives 0, 2, 4 MiB.
- Fixtures under 1 MiB give 0 bytes on any cut.
