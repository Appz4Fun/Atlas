# w07-a: AC-3 decode (oxideav-ac3) + Chromaprint, AC-3 BSI fields, E-AC-3 walk

Setup: oxideav-ac3 0.0.11 (`decoder::make_decoder`, one syncframe per `Packet`, `send_packet`/`receive_frame`, output `Frame::Audio` S16 LE interleaved in `data[0]`; deps oxideav-core 0.1.42). Reference: FLAC via symphonia 0.5 (w05c `decode_stereo`). Fingerprint: rusty-chromaprint 0.3 `preset_test2` + `match_fingerprints`. Source: 30 s 48 kHz stereo = 440 Hz sweep + stepped 2-tone + pink-noise bursts (0.8 s on / 3 s period). AC-3 448k made with ffmpeg 6.1 (vectors only).
score = 1 - mean bit error over matched items (random pairs ~0.5). Item = ~0.1237 s.

## 1. Fingerprint: oxideav-ac3 decode vs FLAC reference
| window | start (s) | score | coverage | first segment |
|---|---|---|---|---|
| full 30 s | 0 | 0.989 | 1.00 | a@0.00 b@0.00, 221 items |
| 5 s | 0 / 5 / 10 | 0.987 / 0.987 / 0.987 | 1.00 | 19 items each |
| 10 s | 0 / 5 / 10 | 0.989 / 0.992 / 0.990 | 1.00 | 59 items |
| 20 s | 0 / 5 / 10 | 0.991 / 0.992 / 0.988 | 1.00 | 140 items |
| shift: AC-3 encodes src from +3.2 s, vs full ref | 0 | 0.966 | 1.00 | a@3.22 b@0.00 (offset recovered) |
| shift: same-offset FLAC vs shifted AC-3 | 0 | 0.989 | 1.00 | a@0.00 b@0.00, 195 items |
| unrelated (brown noise) AC-3, full | 0 | 0.693 | 0.05 | 11 items only: spurious |
| unrelated, 5 s | 0 | 0.000 | 0.00 | no segment |

- Stable match from 5 s windows on this material. Score is only meaningful when coverage is high; use coverage to reject.
- Decoder: 938 syncframes, 0 errors. 1,440,768 samples (30.016 s), 768 samples (half a frame) longer than the 30.000 s reference.
- Whole chain is pure Rust: no ffmpeg needed for AC-3 fingerprinting.

## 2. AC-3 BSI (Rust `parse_ac3`, full A/52 5.4.2 + Annex D xbsi for bsid 6)
| field | ffmpeg 6.1 option set | Rust raw | meaning | match |
|---|---|---|---|---|
| dialnorm | -dialnorm -27 | 27 | -27 dB | yes |
| cmixlev | -center_mixlev 0.595 (5.1, acmod 7) | 1 | -4.5 dB (-3-1.5*1) | yes |
| surmixlev | -surround_mixlev 0.5 | 1 | -6 dB (-3-3*1) | yes (0.595 also maps to 1) |
| mixlevel | -mixing_level 105 | 25 | 105 dB SPL (80+25) | yes |
| roomtyp | -room_type 1 (large) | 1 | large room | yes |
| dsurmod | -dsur_mode 2, -ac 2 | 2 | Dolby Surround | yes (stream is bsid 6) |
| adconvtyp | -ad_conv_type 1 (HDCD) | 0 | standard | NO: 0 also with option 0; not reproduced |
| dsurexmod | -dsurex_mode 2 (-ac 2) | absent | would be EX | NO: bsid 6 xbsi2e=0; with dsurex 3 stream became bsid 8, no field |
| compr, langcod | not set | None | | n/a |
| copyright, origbs | defaults | 1 / 0 | | n/a |
- ffmpeg 6.1 switches to bsid 6 (alternate syntax) when mixing_level/room_type are set; that stream carries xbsi1 (dmixmod, ltrt/loro mix levels) and an addbsi block (addbsil=42, not decoded).
- Unverified: bit position of xbsi2 (dsurexmod, dheadphonmod, adconvtyp) per Annex D; no ffmpeg vector sets it. Also the MediaInfo doc (w02-c) lists dsurexmod/dmixmod bit positions as UNVERIFIED; this run does not resolve them.

## 3. E-AC-3 7.1 (`-c:a eac3 -ac 8 -channel_layout 7.1`)
- NOT PRODUCED: ffmpeg 6.1 eac3 encoder lists layouts only up to 5.1 ("Error while opening encoder" at 448k/640k/1024k). No 7.1 vector.
- Walk on 5.1 E-AC-3 (384k): 938 strmtyp-0 frames, 0 strmtyp-1 frames; acmod 7, lfeon 1, bsid 16, numblks 6, frame 1536 B. Walker stops at EOF exactly.
- Dependent-substream code (strmtyp 1: chanmape + chanmap(16), Table E2.5 names for bits 15..5) is written but UNTESTED. Bit-to-channel names are from memory; verify against A/52 Annex E.
- Next: make a 7.1 indep+dep vector with the oxideav-ac3 E-AC-3 encoder (it advertises multichannel E-AC-3) and test.

Crates: oxideav-ac3 =0.0.11, oxideav-core =0.1.42, rusty-chromaprint 0.3, symphonia 0.5 (flac). Code: docs/research/stream-metadata/code/w07a_main.rs
