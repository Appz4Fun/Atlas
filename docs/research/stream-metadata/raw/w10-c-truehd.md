# W10-C: TrueHD / MLP major sync CRC and Atmos rule

Status: CRC rule solved and verified on every sync of 52 files (12 truehdd fixtures, 39 real encoder files from encspect, 1 FFmpeg-made MLP). Atmos rule settled for the first access unit. Object count is not in the major sync.

## 1. CRC (exact rule)
Sources: truehdd truehd/src/process/extract.rs:230-256, :308-335; truehd/src/structs/sync.rs:539-549; truehd/src/utils/crc.rs:28-31, :131-141. FFmpeg libavcodec/mlp_parse.c:89-101 (ff_mlp_read_major_sync), mlp.c:87-93 (ff_mlp_checksum16), mlp.h:151-157.

- Sync word: F8 72 6F BA (FBA) or F8 72 6F BB (FBB). The 4-byte access-unit header sits before it and is not covered.
- Covered bytes: [sync, sync+C). C = 26 for FBB, and for FBA when byte 25 bit0 is 0. For FBA with byte25 bit0 = 1, C = 28 + 2*(byte26 >> 4). FFmpeg's header_size = C+2.
- Stored CRC: 16-bit big-endian at [sync+C, sync+C+2).
- Update (truehdd Crc16::update): crc = T[crc>>8] ^ (crc<<8) ^ byte, init 0, T[i] = MSB-first table for poly 0x2D (c = i<<8, 8 shifts). The byte is XORed in unshifted. FFmpeg's comment "table lookup and XOR are reversed" describes this same form.
- Our old w06b_main.rs check was wrong on both counts (standard CRC over [0, hs-4), compared with BE at hs-2). Its "CRC fail" was a bug in our code, not in the stream.
- I did not reproduce FFmpeg's ff_mlp_checksum16 expression as a standard CRC; it does not match. The truehdd form matches 100% of syncs.

## 2. Results (first major sync per file)
Columns: fmt, syncs, CRC ok, num_substreams (nsub), extended_substream_info (ext), substream_info (sub), 16ch presentation present (p16), ffmpeg rule, MediaInfo rule, truehdd rule.

| file | fmt | syncs | CRC | nsub | ext | sub | p16 | ffmpeg | MI | truehdd |
|---|---|---|---|---|---|---|---|---|---|---|
| fba_atmos_obj | FBA | 2 | 2/2 | 4 | 0 | 0xC8 | yes | yes | yes | yes |
| fba_atmos_cbi | FBA | 2 | 2/2 | 4 | 0 | 0xC8 | yes | yes | yes | yes |
| fba_atmos_dimtrim | FBA | 6 | 6/6 | 4 | 3 | 0xFC | yes | yes | yes | yes |
| fba_2ch | FBA | 2 | 2/2 | 1 | 0 | 0x14 | no | no | no | no |
| fba_176k | FBA | 1 | 1/1 | 3 | 0 | 0x7C | no | no | no | no |
| fba_192k | FBA | 1 | 1/1 | 3 | 0 | 0x7C | no | no | no | no |
| fba_192k_8ch | FBA | 1 | 1/1 | 3 | 0 | 0x7C | no | no | no | no |
| fba_spliced | FBA | 6 | 6/6 | 3 | 0 | 0x7C | no | no | no | no |
| fbb_6ch | FBB | 3 | 3/3 | 2 | 0 | 0x0D | no | no | no | no |
| fbb_6ch_single | FBB | 2 | 2/2 | 1 | 0 | 0x04 | no | no | no | no |
| fbb_copy | FBB | 3 | 3/3 | 1 | 0 | 0x05 | no | no | no | no |
| fbb_spliced | FBB | 15 | 15/15 | 2 | 0 | 0x0D | no | no | no | no |
| ff_made.mlp (ffmpeg 6.1 -c:a mlp, 2ch 48k) | FBB | 75 | 75/75 | 1 | 0 | 0x05 | no | no | no | no |

Real encoder files (encspect external-*.thd / .mlp, 39 files, 2ch to 5.1, 44.1 to 192 kHz, FBA and FBB): CRC 100% of syncs, sig 0xB752 100%. All have nsub 1 or 2, sub 0x04/0x05/0x07/0x14/0x3C, p16 = no, all three rules = no. ffprobe reports the expected codec, rate and channels (e.g. external-truehd-51-48k-24.thd: truehd 48000 6 ch) and ffmpeg logs no checksum error (spot-checked on 3 files).

## 3. Atmos rule
- ffmpeg: num_substreams == 4 && (substream_info >> 7) != 0.
- MediaInfo: (substream_info >> 7) != 0.
- truehdd: presentation masks come from PresentationMap::with_substream_info (process/mod.rs:110-119): masks[3] = ((sub>>4)&8) | (7 ^ (7 >> (ext&3))). Bit 3 of masks[3] is bit 7 of substream_info, i.e. substream 3 (the object substream) belongs to the 16-channel presentation. truehdd's sync check (sync.rs ~500-520) requires num_substreams == 4 whenever bit 7 is set (otherwise SubstreamCountInfoInconsistent, logged at Debug). So the MSB rule is the presentation rule, and nsub==4 is a consistency check.
- Atlas rule: atmos = FBA && (substream_info & 0x80) != 0. Also store atmos_inconsistent = atmos && nsub != 4. p16 = FBA && masks[3] != 0.
- The three rules agree on all 52 files. They can only differ when bit 7 is set and nsub != 4, which none of the files has. On such a file ffmpeg says no Atmos, truehdd and MediaInfo say yes. Use the MSB rule and flag the inconsistency.

## 4. Object count
truehdd does not derive an object count from the major sync. The count is in the substream-3 extra data, which is not parsed here. Atlas should store atmos = bool and objects = null from the major sync; objects needs a parse of the substream-3 data.

## Code
docs/research/stream-metadata/code/w10c_main.rs (std only; `w10c_main <file>...` prints one line per file). Fixes the CRC rule and reproduces all table columns.
