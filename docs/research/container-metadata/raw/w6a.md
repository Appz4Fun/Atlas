# W6a: Local validation of disc image, MPEG-TS/M2TS and AVI header exposure (Atlas)

Scope: what the first 700 KB / 4 MiB of locally built DVD ISO, TS, M2TS and AVI files expose.
Samples built on this box (ffmpeg 6.1.1, dvdauthor, genisoimage, mkvmerge). Parser: `docs/research/container-metadata/tools/disc_ts_avi.py` (stdlib only, run with `python3 -I`).
Work dir: /tmp/w6a (samples in /tmp/w6a/dvd, /tmp/w6a/ts, /tmp/w6a/avi; not copied to the mount).

## Commands

```
# 1. DVD
ffmpeg -f lavfi -i testsrc2=size=720x576:rate=25 -f lavfi -i sine=440 -f lavfi -i sine=880 -t 30 \
  -map 0:v -map 1:a -map 2:a -c:v mpeg2video -b:v 5000k -aspect 4:3 -r 25 -pix_fmt yuv420p \
  -c:a ac3 -b:a 192k -ar 48000 -ac 2 -metadata:s:a:0 language=eng -metadata:s:a:1 language=fra \
  -f dvd -target pal-dvd dvd/src_pal.mpg
# (ffmpeg emits a dvd_nav_packet data stream with -target pal-dvd; 30.016 s; 2x AC-3 48 kHz 2ch)
dvdauthor -o dvd_out -x dvd.xml     # needs <vmgm><menus><video format="pal" aspect="4:3"/>... (without it no VIDEO_TS.IFO, "no video format specified for VMGM")
genisoimage -quiet -dvd-video -V ATLAS_DVD -udf -o dvd/disc_gen.iso dvd/dvd_out     # 21,628,928 bytes
xorriso -as mkisofs -udf ...   # FAILS: "Unsupported option '-udf'" (xorriso 1.5.6 mkisofs emulation). Use genisoimage.
# 2. TS
ffmpeg ... -c:v libx264 -c:a ac3 -metadata:s:a:0 language=eng \
  -metadata service_name="Atlas Test" -metadata service_provider="AtlasLab" -mpegts_service_id 1 -f mpegts ts/test.ts
ffmpeg -i ts/test.ts -c copy -mpegts_m2ts_mode 1 -f mpegts ts/test.m2ts
# -mpegts_service_name / -mpegts_service_provider do NOT exist (ffmpeg: "Unrecognized option"); use -metadata service_name/service_provider.
# 3. AVI
ffmpeg -f lavfi -i testsrc2=size=640x480:rate=25 -f lavfi -i sine=440:sample_rate=44100 -t 20 \
  -c:v mpeg4 -vtag XVID -q:v 5 -c:a libmp3lame -b:a 128k -metadata title="Atlas Test Title" \
  -metadata comment="Atlas comment" -metadata:s:a:0 language=eng avi/test.avi
# parser
python3 -I .../tools/disc_ts_avi.py iso dvd/disc_gen.iso --dump-dir /tmp/w6a/ifo_dump
python3 -I .../tools/disc_ts_avi.py ts  ts/test.ts      # default 700 KB
python3 -I .../tools/disc_ts_avi.py avi avi/test.avi    # default 4 MiB
```

## 1. DVD-Video ISO (genisoimage -dvd-video -udf), image 21,628,928 bytes (10,561 sectors)

- Volume recognition sectors: 16 CD001 (ISO9660 PVD), 17 CD001 (terminator), 18 BEA01, 19 NSR02, 20 TEA01.
- ISO9660 PVD (sector 16 = 0x8000): volume id @40 = `ATLAS_DVD`; volume set id @190 empty; app id @574 = genisoimage string; creation @813 = `2026110207180200`; root dir record extent 274, size 2048.
- ISO9660 VIDEO_TS dir: LBA 276 (byte 565,248). Entries: VIDEO_TS.IFO LBA 277 (byte 567,296, 12,288 B); VIDEO_TS.BUP LBA 303 (byte 620,544); VTS_01_0.IFO LBA 309 (byte 632,832, 12,288 B); VTS_01_0.BUP LBA 10404 (byte 21,307,392); VTS_01_1.VOB LBA 315 (byte 645,120, 20,662,272 B).
- UDF: AVDP at sector 256 (0x100000) and at N-256 = 10560 (both tag_ok). Main VDS loc 32, len 32,768 B. IUVD impl `*UDF LV Info`, LVI `ATLAS_DVD`. PD partition 0 start LBA 257 (byte 526,336) length 10,154. LVD impl `*genisoimage`, block 2048, FSD long_ad lbn 0 partref 0. FSD tag 256 (tag_ok), root ICB lbn 2 (abs 259). UDF root: AUDIO_TS, VIDEO_TS. VIDEO_TS FE tag 261 at lbn 6 (abs 263, byte 538,624). File entries are tag 261 (FE), short ADs, one extent each.
- VIDEO_TS.IFO and VTS_01_0.IFO both lie inside the first 700 KB (ends 645,120 and 716,800 cap) and inside the first 4 MiB. The BUP copies and the VOB sit later.
- IFO attributes (BE fields, offsets from the reference layout in w2c/w3b; the parser reads them):
  - VMG: "DVDVIDEO-VMG"; spec version 0x11 (bytes 0x20..0x21 = 00 11); category 0; nr_volumes 1; nr_title_sets 1 (@0x3E); provider id `DVDAuthor 0.7.2` (@0x40); TT_SRPT sector 1 (@0xC4); VMGM_PGCI_UT sector 2 (@0xC8).
  - VTS_01_0: "DVDVIDEO-VTS"; last VOB sector 10100 (@0x0C); VTS_PGCI sector 2 (@0xCC); video attr @0x200: MPEG-2, PAL, aspect 4:3, resolution index 0 (720x576).
  - Audio attr (8 bytes each @0x204): stream 0 AC-3 lang `en` 2ch, 48 kHz (sample_rate_code 0); stream 1 AC-3 lang `fr` 2ch 48 kHz. Quant field reads as 3 for AC-3 (meaningless for AC-3; ignore). Lang bytes 2..3 verified.
  - Subpicture count 0 (no subtitle stream made; see gaps).
  - PGCI: 1 LU; PGC0 nr_programs (= chapter/PTT count) 3 (chapters 0,10,20); nr_cells 3; playback time BCD `00:00:30.00` at 25 fps (flag 1). Matches ffmpeg 30.0 s.
- DVDDiscID candidate (MD5 over the full IFO blocks: VIDEO_TS.IFO then VTS_01_0.IFO, per libdvdread DVDDiscID as summarized in w3b §6): `918d26bab2dcf09adf08a37da9267d21`. NOT checked against a libdvdread binary here (not installed). Treat as candidate only.
- Bitrate: no field in IFO (confirmed: IFO has no bitrate). Must come from VOB/PES scan.

## 2. MPEG-TS and M2TS

ts/test.ts (716,800 bytes read, 188-byte packets, bad_sync 0):
- PID counts: PAT 0x0000 (22 pkts, all identical copies), SDT 0x0011 (5), video 0x0100 (3477), audio 0x0101 (286), PMT 0x1000 (22). No PID 0x0012 (EIT) and no 0x0014 (TDT). Expected: ffmpeg does not write EIT/TDT.
- PAT: program 1 -> PMT PID 0x1000.
- SDT (table 0x42, PID 0x11): service_id 1, service_type 0x01 (digital TV), provider `AtlasLab` (from -metadata service_provider), name `Atlas Test` (from -metadata service_name).
- PMT: PCR PID 0x0100; ES 0x0100 stream_type 0x1B (H.264), no descriptors. ES 0x0101 stream_type 0x81 (AC-3 as ATSC-style), descriptors: registration `AC-3`, ISO639 `eng` audio type 0. No 0x6A descriptor was written.

ts/test.m2ts (remux with -mpegts_m2ts_mode 1, 192-byte packets, TP_extra_header at byte 0..3, sync at 4):
- Detected: packet 192, tp_header 4, bad_sync 0.
- PMT (PID 0x0100): PCR PID 0x1011; program descriptors: registration `HDMV` and a 0x88 descriptor (len 4); video 0x1011 H.264 (0x1B); audio 0x1100 0x81 with registration `AC-3` and ISO639 `eng`.
- SDT provider `FFmpeg`, name `Service01`: the remux did not pass -metadata, so ffmpeg defaults apply. Expected.

Not from PSI (out of scope for the parser; need PES/SPS parse): video resolution, bitrate, frames, HDR/DV signalling (SEI / RPU), audio sample rate detail beyond the descriptors.

## 3. AVI (ffmpeg mpeg4 + -vtag XVID, MP3)

avi/test.avi (6,366,958 B; 4 MiB read):
- RIFF size 6,366,950; `AVI `; LIST hdrl; movi LIST at offset 10,032 (in first 700 KB). idx1 not inside the first 4 MiB read; no `ix00` in movi.
- avih: us/frame 40,000 (25.000 fps), max_bytes/s 41,000, flags 0x910 (HASINDEX|ISINTERLEAVED|TRUSTCKTYPE), total_frames 501, streams 2, 640x480.
- strh/strf video: fccType `vids`, handler `XVID`, scale 1, rate 25, length 501; BITMAPINFOHEADER compression `XVID`, bitcount 24, 640x480.
- strh/strf audio: fccType `auds`, wFormatTag 0x0055 (MP3), 1 ch, 44,100 Hz, avg 16,000 B/s (= 128 kbps), block_align 1152, cbSize 12. Language field 0 (`language=eng` metadata is NOT written to strh for AVI here).
- INFO: INAM `Atlas Test Title` (title), ICMT `Atlas comment` (comment), ISFT `Lavf60.16.100`.
- ffmpeg writes the OpenDML `LIST odml / dmlh` inside a JUNK chunk (JUNK head shows `odmldmlh`), not as a bare LIST. The walker found no 'indx' (ffmpeg writes it only for large files).
- First video chunk `00dc` at payload offset 10,478, size 22,597. Within it, `Lavc60.31.102` user-data at file offset 10,512 (user-data start code area follows `00 00 01 B2`). No `XviD` string written (`-vtag XVID` changes only the FourCC).
- Note: `-vtag XVID` sets FourCC only; the encoder user-data string is `Lavc`.

## Verdicts

| Format | Readable in first 700 KB | Readable in first 4 MiB | Notes |
|---|---|---|---|
| DVD ISO (genisoimage -udf, 1 title set) | YES for ISO9660 PVD/dirs, UDF AVDP/VDS/FSD, VIDEO_TS.IFO, VTS_01_0.IFO, DVDDiscID inputs (IFO ends at 645,120 / 716,800 for the sample) | YES (all of the above) | Layout is the sample's, not a universal rule. Real multi-title discs put IFOs at other LBAs. Whether DVDDiscID needs more than the IFOs is per libdvdread, not verified here. |
| MPEG-TS | YES (PAT/PMT/SDT, ES descriptors, lang, service name/provider) | YES | EIT (0x12) and TDT (0x14) absent in ffmpeg output; they come only from real broadcast captures. Video resolution/bitrate/HDR need PES/SPS parse. |
| M2TS (192-byte) | YES, sync at byte 4 | YES | HDMV registration and 0x88 descriptor observed. |
| AVI | YES (avih, strh/strf, INFO, JUNK, `Lavc` user-data in first chunk) | YES | idx1 and any large odml index sit at the end; the header is well inside 700 KB for these short samples. |
| Blu-ray folder | SKIPPED: no authoring tool here (per task, noted only). |

## Top findings

1. genisoimage -dvd-video -udf exposes VIDEO_TS.IFO and VTS_01_0.IFO at byte 567,296 and 632,832 (both < 700 KB); the full UDF and ISO9660 structure sits in the first ~650 KB.
2. xorriso `-as mkisofs` rejects `-udf` on this build (1.5.6); genisoimage works.
3. dvdauthor needs `<vmgm><menus><video format="pal" .../>` to emit VIDEO_TS.IFO; without it the VMG IFO is missing.
4. VTS IFO gives MPEG-2 PAL 4:3 720x576, 2 AC-3 tracks (en/fr, 2 ch, 48 kHz), 3 chapters, 30 s playback (BCD). No bitrate field in IFO.
5. ffmpeg mpegts: -mpegts_service_name / -mpegts_service_provider are invalid; use -metadata service_name / service_provider. These land in SDT (PID 0x11) as `Atlas Test` / `AtlasLab`.
6. ffmpeg writes AC-3 in TS as stream_type 0x81 with registration `AC-3` + ISO639 descriptor, not 0x06 + 0x6A.
7. AVI: -metadata title/comment lands in INFO INAM/ICMT; ffmpeg's `Lavc60.31.102` user-data string is in the first video chunk within the first 4 KiB of movi.
8. Gaps: DVDDiscID is a candidate only (not checked against libdvdread). No subtitle stream was authored. EIT/TDT only from real broadcast captures.

Verification status: parser output validated on one sample of each format; no real-world captures; no libdvdread binary run.
