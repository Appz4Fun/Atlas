# w2b: MP4 moov placement, tail strategy, muxer fingerprints, crates

Legend: [V] verified in this session (local ffmpeg 6.1.1 test files, or fetched source/doc text). [M] memory, not verified here. Test artifacts: /tmp/w2b/{plain,faststart,frag,ct,ct2}.mp4, dumper /tmp/w2b/script/boxdump.py (run with python3 -I).

## 1. moov placement by tool

| Tool | Default | Status |
|---|---|---|
| ffmpeg/libavformat movenc | moov at END unless `-movflags +faststart` | [V] local ffmpeg 6.1.1: plain.mp4 = ftyp, free@32, mdat@40 (28883 B), moov@28923. faststart.mp4 = ftyp, moov@32, free, mdat@3137. `movflags` option default is 0 (FFmpeg movenc option table, fetched). |
| MP4Box (GPAC) | moov at FRONT, 0.5 s interleaving by default | [V] mankier.com/1/mp4box text: "MP4Box stores by default the file with 0.5 second interleaving and meta-data (moov ...) at the beginning". `-flat` = "store file with all media data first, non-interleaved" (so moov after mdat). `-inter` doc is internally inconsistent (0.5 vs 500 ms). `-moovpad` pads after moov. The `-brand` default is not given. |
| HandBrake | "Web Optimized" option; default not stated | [V] handbrake.fr web-optimised page is a draft; it says only "places the MP4 container header at the start of the file". Default OFF is [M]. |
| L-SMASH muxer (x264/x265 CLI via L-SMASH/GPAC) | unknown | [M] only. Needs a binary test. Do not rely on it. |
| Apple AVFoundation (AVAssetWriter) | shouldOptimizeForNetworkUse default NO | [M] API default. Compressor/QuickTime export: [M] unknown. |
| Shutter Encoder, Tdarr | ffmpeg-based, so ffmpeg default unless plugin adds flags | [M]. Tdarr plugins often add +faststart. |
| yt-dlp / N_m3u8DL-RE (WEB-DL) | output is fMP4 or ffmpeg-remuxed | [M]. Fragmented output (see below) has moov at front. Remux with ffmpeg may add +faststart. |
| DASH/CMAF segments | init segment moov first, then moof/mdat pairs | [M] |

Conclusion (partially verified): front moov comes from MP4Box defaults, from ffmpeg with +faststart, from HandBrake with Web Optimized (if the option is on), from fragmented output, and from Apple optimized export. Plain ffmpeg and most Compressor-style exports put moov at the END. No measured percentage for release MP4s was found. Atlas should measure on its own corpus.

### Fragmented MP4 [V]
`ffmpeg -movflags frag_keyframe+empty_moov+default_base_moof` produced: ftyp (major iso5, compat iso5 iso6 mp41), moov@28 (mvhd, trak, mvex, udta; trak stbl has no samples), moof@1339, mdat@2623, mfra@31506 at END. No sidx was written by ffmpeg in this mode. Duration is not in moov (ffprobe reported 2.08 s from fragments). So frame count and bitrate need moof/trun walking.

## 2. Tail strategy (moov at end) [V for layout, spec logic]
- Box header: u32 size, 4cc. size==1 means u64 largesize follows at +8 (header 16 B). size==0 means to EOF (legal only for the last box). Next box = box_start + size.
- Non-fragmented, moov-at-end layout is ftyp, [free|skip|wide]*, mdat, moov. The first article already contains the mdat header, so `moov_off = mdat_off + mdat_size` (mdat_size from the 8- or 16-byte header). Walk with free/wide boxes skipped, do not assume a fixed offset (MP4Box -moovpad adds padding after moov).
- mdat size 0 (or largesize 0) = to EOF. There is then no moov after it. The file is truncated, or moov was never written (a stream write that died). Treat as "moov unavailable".
- Case moov-at-front (faststart, MP4Box, fragmented): moov is in article 1 if below ~700 KB. mvhd/trak/stbl are all front. Fragmented files need the moof chain for counts.
- Mapping to RAR and yEnc: the MP4 is a contiguous byte run in the RAR store-mode data stream. Volume data starts after each volume's RAR headers (marker + archive header + file header; continuation volumes repeat headers). So: (a) parse the RAR file header for the data offset per volume; (b) compute the absolute stream offset `abs = mp4_base + moov_off`; (c) find volume i whose data range contains abs; (d) inside that volume, article k from the yEnc `=ypart begin= end=` byte range (1-based, in the volume file), with `=ybegin size=` for total volume size. If `=ypart` is missing, use the fixed article payload length from the first article and fix up the last one. (e) fetch article k and seek within its decoded payload. The article can straddle; fetch the two neighbours when abs is within a few bytes of a boundary. Read the moov box's size from its header before fetching everything (moov can be >1 article).
- moov tail can be larger than 1 article (about 100 KB to several MB for long films with many chapters or large ilst). Fetch until the box size is satisfied.

## 3. Muxer fingerprints
Verified (ffmpeg 6.1.1 output):
- hdlr name in trak: `VideoHandler` (vide), `SoundHandler` (soun). Meta hdlr type `mdir`.
- ilst (udta/meta/ilst): `©nam` = title, `©cmt` = comment, `©too` = `Lavf60.16.100` (encoder tag, written only with metadata set; ffmpeg 6.1.1 was the tool).
- ftyp (plain): major `isom`, minor 0x00000200, compat `isom iso2 avc1 mp41`. Fragmented: major `iso5`, compat `iso5 iso6 mp41`.
- mvhd creation_time: 0 when no creation_time metadata was set. With `-metadata creation_time=2024-01-02T03:04:05Z`, mvhd and mdhd both = 3787009445 (= Unix 1704164645 + 2082844800, the 1904 epoch). Use this as a twin-vs-remux signal: a 0 means no timestamp, a real value means set by the tool or the user.
[M] (not tested here): GPAC `GPAC ISO Video Handler`, L-SMASH `L-SMASH Video Handler`, Apple `Core Media Video`, HandBrake `HandBrake x.y.z` in ©too, compressorname (x264 / HandBrake strings), ftyp sets for MP4Box (`isom iso2 mp41` etc.).

## 4. Crates (crates.io API, fetched 2026-11-02)
- mp4-atom 0.16.1, updated 2026-09-30, 431,592 downloads, repo kixelated/mp4-atom. Top-level src: any, atom, buf, emeb/emib, emsg, free, ftyp, header, mdat, mfra, moof, moov (dir), meta (dir), sample (dir), sidx, styp, uuid, uuid_boxes. No top-level dvcc/colr/mdcv/clli/btrt/ilst/udta/elst/chpl file, but moov/ and sample/ are not expanded, so [unverified] whether they live there.
- re_mp4 0.5.1, updated 2026-07-08, 2,111,597 downloads, repo rerun-io/re_mp4. src: error.rs, lib.rs, mp4box/ (not expanded), pcm.rs, reader.rs, types.rs. Box coverage not verified.
- mp4 (alfg/mp4-rust) 0.14.0, last update 2023-08-01, 15.36 M downloads. src/mp4box has ilst.rs, udta.rs, elst.rs, hdlr.rs, tkhd.rs, moof.rs, mehd, trex, tfhd, tfdt, trun, emsg, vpcc, vp09, hev1, avc1, mp4a, tx3g. No dvcc, colr, mdcv, clli, btrt, chpl, sidx file. So DV/HDR boxes are NOT parsed (verified by file presence only).
- mp4parse (Mozilla), mp4ameta: see w1b.md (not re-checked).
- Partial-buffer reading (moov from a non-contiguous buffer): NOT verified for any crate. Assume Read+Seek required. Atlas needs its own box reader for the moov tail anyway.

## 5. Surprises
- ffmpeg 6.1.1 is the local oracle: `-movflags +faststart` moves moov in front with no re-encode, file size unchanged (32020 B for both).
- mp4 (alfg) is old and lacks colr/dvcC, so DV/HDR fields need the in-house reader.
- MP4Box is the one major tool with front moov as default, which points to GPAC-muxed releases as the front-moov population. [M]
- No MP4Box binary here, so its ftyp/hdlr strings are doc-based only.

## Sources
- https://www.mankier.com/1/mp4box (MP4Box man page mirror; default moov front, -flat, -inter)
- https://handbrake.fr/docs/en/latest/advanced/web-optimised.html (draft, no default stated)
- https://lists.mplayerhq.hu/pipermail/ffmpeg-cvslog/2020-April/121746.html (from w1b)
- https://github.com/FFmpeg/FFmpeg/blob/master/libavformat/movenc.c (partly fetched: movflags default 0, faststart option text)
- https://crates.io/api/v1/crates/mp4-atom, /re_mp4, /mp4 (versions and dates)
- https://api.github.com/repos/kixelated/mp4-atom/contents/src, /rerun-io/re_mp4/contents/src, /alfg/mp4-rust/contents/src/mp4box (file listings)
- Local: ffmpeg 6.1.1 (/usr/bin/ffmpeg), ffprobe; test files in /tmp/w2b
