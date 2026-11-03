# w06-b: TrueHD major sync (Rust port of FFmpeg mlp_parse.c), partial

Time cap hit. Done: TrueHD only. NOT done: AC-3 option tests, E-AC-3 7.1 dependent substream, oxideav-ac3 + chromaprint cross-codec score.
Code: `code/w06b_main.rs` (std only; `headers` = `w06b_main <raw.thd>`). Vectors: ffmpeg 6.1 truehd encoder, 48 kHz, 5 s, `-strict -2`.

## 1. Field table (TrueHD 5.1 vector, raw ES from `-c copy -f truehd`)

| field | ffprobe | Rust | match |
|---|---|---|---|
| codec / profile | truehd / unknown | 0xF8726FBA, stream_type 0xBA | yes |
| sample rate | 48000 | ratebits 0 -> 48000 | yes |
| channels / layout | 6 / 5.1(side) | 6ch mask 0x0F = L R C LFE Ls Rs (13-bit mask also 0x000F) | yes |
| access unit | pkt 354 B | AU length 0x0B1 words = 354 B | yes |
| num_substreams | - | 2 | - |
| substream_info | - | 0x3C, MSB 0 (no Atmos) | - |
| is_vbr | - | 1 | - |
| peak_bitrate | N/A | field 3199 -> (3199*48000+8)>>4 = 9597 kbps | unverified |
| max AU rate | file 1178 kbps avg | max AU 366 B -> 3.5 Mbps (AU rate, not the header peak) | gap |
| sync CRC (375 syncs) | - | 375/375 pass | yes |
| 0xB752 signature | - | 375/375 present | yes |

## 2. Channel layout

| file | ffprobe | Rust 6ch mask | Rust 13-bit mask | match |
|---|---|---|---|---|
| thd51 | 6, 5.1(side) | 0x0F (6) | 0x000F (6) | yes |
| thd71 (ffmpeg "7.1") | 5, 5.0(side) | 0x0B (5) | 0x000B (5) | yes (same mask) |

The 7.1 TrueHD vector was NOT produced: the ffmpeg 6.1 truehd encoder lists only mono..5.1(side). A real 7.1 sample is needed.

## 3. Checksum finding (conflicts with FFmpeg source reading)

- FFmpeg `ff_mlp_checksum16(buf, hs-2)` XORs LE16 of bytes [hs-4..hs-2] and compares with `AV_RL16(buf+hs-2)`.
- On the vector, CRC-16 poly 0x2D (MSB-first, init 0) over bytes [0..24) = 0xE4D1. Stored bytes 26-27 are `e4 d1`, so it matches as **big-endian**, not little-endian.
- With BE compare all 375 syncs pass. With the literal LE compare none pass. Treat the byte order as UNRESOLVED until a second source is checked against a real disc.

## 4. Other notes

- Atmos rules differ: FFmpeg needs num_substreams==4 and MSB set; MediaInfo uses the MSB only. Both are false on these vectors, so no test separates them.
- Channel tables: 0xBA uses a 5-bit mask (6ch presentation) and a 13-bit mask (8ch/16ch). Bit order from mlp_parse.h thd_chancount: LR, C, LFE, LRs, LRvh, LRc, LRrs, Cs, Ts, LRsd, LRw, Cvh, LFE2.
- AU header: 16-bit word, low 12 bits = length in words, 4 bytes before the major sync.

## Open
- Real 7.1 TrueHD sample (8ch mask, expected 0x4F-style 13-bit value) and Atmos (16ch) sample.
- Peak rate: header field vs measured AU rate (3.5 vs 9.6 Mbps) unexplained.
- CRC byte order (see 3).
- AC-3 dialnorm/bsmod/acmod option tests, E-AC-3 7.1 (strmtyp=1 dependent substream), and oxideav-ac3 fingerprint scores: not run.
