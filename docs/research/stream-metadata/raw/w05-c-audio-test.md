# w05-c: audio header parsing, crc2 remux check, Chromaprint (Rust, no parser crates)

Source: 20 s, 48 kHz 5.1 (mixed sines + pink noise, amplitude-modulated). Encodes: ffmpeg AC-3 640k, E-AC-3 640k, TrueHD, DTS 1536k, FLAC, AAC. Parsers: `code/w05c_main.rs` (`headers`, `crc2`, `fp`).

## 1. Header fields: ffprobe vs Rust

| file | field | ffprobe | Rust | match |
|---|---|---|---|---|
| ac3 (MKV ES) | codec | ac3 | 0x0B77 sync, bsid 8 | yes |
| ac3 | sample rate | 48000 | fscod 0 -> 48000 | yes |
| ac3 | channels | 6, 5.1(side) | acmod 7 (3/2) + lfeon 1 = 6 | yes |
| ac3 | bitrate | 640000 | frmsizecod 36 -> 640 kbps, 2560 B/frame | yes |
| ac3 | frames | 625 packets | 625 frames walked, 1600000 B | yes |
| ac3 | bsmod / dialnorm | not in default output | bsmod 0, dialnorm 31 | not checked |
| eac3 | codec / sr / ch | eac3 / 48000 / 6 | strmtyp 0, acmod 7, lfeon 1 | yes |
| eac3 | bitrate | 640000 | frame 2560 B x 625 = 640 kbps | yes (derived) |
| eac3 | samples/frame | - | numblks 6 -> 1536, bsid 16 | derived |
| truehd | codec / sr | truehd / 48000 | sr nibble 0x0 -> 48000 | yes |
| truehd | major sync sig | - | 0xB752 at sync+8 | present |
| truehd | channels | 6, 5.1(side) | channel_meaning bytes 8C 7F 20 3C, decode NOT resolved | no |
| truehd | peak bitrate | bit_rate N/A | flags 0x0000 -> peak 0 | n/a |
| truehd | num_substreams | - | not decoded | open |
| truehd | sync count | - | 1500 occurrences of F8726FBA in ES | not validated |
| dts | codec / profile | dts / DTS | sync 7FFE8001 | yes |
| dts | sr | 48000 | SFREQ 13 -> 48000 | yes |
| dts | channels | 6 | AMODE 9 (5ch) + LFF 2 (LFE) = 6 | yes |
| dts | bitrate | 1536000 | RATE 24 -> 1536 kbps | yes |
| dts | frame | - | FSIZE 2011 -> 2012 B, NBLKS 15 -> 512 samples | derived |
| flac (raw) | sr / ch / bps | 48000 / 6 / 24 | 48000 / 6 / 24 | yes |
| flac | total samples | 960000 (20 s) | 960000 | yes |
| flac | block size | - | min=max=4608 | - |
| flac | STREAMINFO MD5 | not shown | df1ea573...8ea8d2ef9 = ffmpeg pcm_s24le md5 of decode | yes |

## 2. crc2 remux check (AC-3 syncframes, last 2 bytes per frame)

| input | frames | fnv64 of crc2 seq | first 8 crc2 |
|---|---|---|---|
| ac3.mkv raw ES | 625 | 21246215bce3e82a | ae8d 3335 6cb1 c624 70c7 4e02 9cd0 198c |
| ac3.mp4 raw ES (`-c copy -f ac3`) | 625 | 21246215bce3e82a | same |

Result: sequences identical. Raw ES files are also byte-identical (cmp). For this vector the remux-proof holds. Caveat: ffmpeg remux of AC-3 is a pure passthrough, so this does not test a repacker that rewrites frames.

## 3. Chromaprint (rusty-chromaprint 0.3, preset_test2, symphonia 0.5.5 decode to stereo i16, 20 s = 140 items)

| pair | segments | matched items | score (1 - mean BER) |
|---|---|---|---|
| flac_st.mkv vs aac_st.mkv (AAC-LC 128k, same source, stereo) | 1 | 140/140 | 0.992 |
| flac_st.mkv vs flac.mkv (5.1 FLAC, our downmix) | 1 | 140/140 | 1.000 |
| flac_st.mkv vs unrelated.flac (white noise + 220 Hz) | 0 | 0/140 | 0.000 (no match) |

Notes:
- symphonia 0.5.5 AAC fails on the 5.1 AAC file ("aac: aac too complex"), so the 5.1 AAC was not fingerprinted; the stereo pair was used.
- AC-3 decode not done: symphonia has no AC-3 decoder and oxideav-ac3 was dropped for time.
- Score = 1 - mean bit error over aligned items; for no segments the score is reported as 0.

## Open
- TrueHD channel_assignment, num_substreams and peak rate need a verified bit layout (ffmpeg source check).
- DTS LFF bit offset was taken from ETSI TS 102 114 layout and matched ffprobe (6 ch), not separately verified.
- AC-3 fingerprint via a pure-Rust AC-3 decoder not run.
