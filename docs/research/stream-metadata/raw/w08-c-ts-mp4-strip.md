# W08-C: reaching the elementary stream in TS/M2TS, MP4, and MKV header stripping

Code: docs/research/stream-metadata/code/w08c_main.rs (std-only; modes `ts`, `mp4`, `frag`). Test vectors built with ffmpeg 6.1 (20 s, HEVC Main10 1280x720 x265 keyint 48, AC-3 mono 384 kbps 48 kHz). x265 emits CRA (NAL 21) at each GOP, IDR only at file start.

## 1. MPEG-TS / M2TS
- Resync: packet start p is valid when 0x47 repeats 5x at stride 188 (TS) or 192 with sync at p+4 (M2TS). Arbitrary offset 1000003 resolves to 188-packet start 1000160 (TS) and 192-packet start 1000128 (M2TS).
- PAT (pid 0) then PMT: HEVC stream_type 0x24, AC-3 0x81 (also handled: 0x1B AVC, 0x83 TrueHD, 0x86 DTS-HD, 0x87 E-AC-3 as pid picks only; no decode of those).
- PES: PTS from 5-byte field; payload after PES_header_data_length. Annex B split on 00 00 01, trailing zeros trimmed.
- "Decodable" = VPS+SPS+PPS seen before an IRAP (16..21). Test found CRA (21), not IDR, because the stream has no mid-file IDR.
- Results (TS, resync 1000003): first decodable random-access point = CRA at abs 1522988 (TS packet 2781), 522,828 bytes after resync; NAL 28,364 bytes, header 2A 01. M2TS (resync 1000128): CRA at abs 1555392, 555,264 bytes after resync.
- Verification: both extracted NAL payloads are byte-identical to the ffmpeg Annex B dump (found at the same offset 1261327). This is NAL-level bit-exactness, not a decoder run; rust_h265 decode was not done (time).
- AC-3: first syncframe 0x0B77 found via scan, fscod/frmsizecod -> 1536 bytes/frame, 384 kbps, acmod + LFE -> channels. Frame size table is AC3_WORDS[frmsizecod>>1][fscod] x2.
- Article cost at 700 KB: PAT/PMT repeat every ~100 ms, so one article is enough to find tables after a random resync. Decodable CRA is ~523 KB (TS) / ~555 KB (M2TS) after resync: 1 article if the resync is at article start, 2 at worst. Note: a CRA start needs RASL (types 8/9) skipped; not implemented.

## 2. MP4 (HEVC, hvc1, 1280x720, 480 samples, 10 sync samples)
- (a) moov at end: top level ftyp@0, mdat@40 (7.2 MB), moov@7228584 (10601 B). moov lies in article 10 (tail), keyframe (sample 240, ~50%) at 3599114 len 29572 in article 5. Total articles = 2 (tail + middle).
- (b) faststart: moov@32 (article 0). Keyframe at 3609715 (article 5). Total 2 articles.
- (c) fragmented (frag_keyframe+empty_moov+delay_moov): init moov in article 0. moof found at arbitrary offset 3500000 -> moof@3602419 (type at +4, size 388, next box mdat). tfhd flags 0x39 (base_data_offset present); trun data_offset 396 -> sample 0 at 3602815, len 29572, sync. Total 2 articles (init + moof article).
- hvcC: lengthSize 4, param sets [32,33,34]. Sample entry children hvcC, pasp, btrt (no dvcC/dvvC/colr in this file).
- Verification: all three keyframe payloads, parsed as 4-byte length-prefixed NALs, give types [35,32,33,34,39,21] and every NAL is byte-identical to the ffmpeg Annex B dump.
- Walker reads only moov (stsd/stsz/stsc/stco|co64/stss) + one sample; the sample offset = chunk_offset + sum of preceding sample sizes in the chunk.

## 3. MKV ContentEncoding header stripping
- Elements: ContentEncodings 0x6D80 -> ContentEncoding 0x6240 -> ContentEncodingOrder 0x5031, ContentEncodingScope 0x5032 (bit 1 = block frames, bit 2 = CodecPrivate; default 1), ContentEncodingType 0x5033 (0 compression, 1 encryption), ContentCompression 0x5034 -> ContentCompAlgo 0x4254 (0 zlib, 1 bzlib, 2 lzo, 3 header stripping), ContentCompSettings 0x4255 (for algo 3: the bytes removed from the start of each frame).
- Rebuild: for each block of the track with algo 3 and scope bit 1: frame = ContentCompSettings || stored payload. Empty settings = no-op (ffmpeg sets needs_decoding only when settings size > 0). Scope bit 2: apply the same prefix to CodecPrivate.
- Which bytes: whatever the muxer found common at the start of frames. Not a fixed rule: a 0B 77 AC-3 sync or 4-byte NAL length may or may not be included, so the walker must not hard-code it; it must prepend what ContentCompSettings says. ffmpeg (matroskadec.c, case MATROSKA_TRACK_ENCODING_COMP_HEADERSTRIP, `needs_decoding`, scope & 2 path) does exactly this prefix-prepend.
- Walker rules: read Tracks once; if type 1 (encryption) skip track; if multiple encodings, apply by descending ContentEncodingOrder; algo 0/1/2 needs inflate/bzip2/lzo (not std), so skip and report. Lacing: ffmpeg's frame-by-frame vs whole-block order was not checked.
- Not generated: no mkvmerge in the container, ffmpeg's matroska muxer does not write header stripping. No MKV test vector; the rules above come from ebml_matroska.xml and matroskadec.c.

## Findings for the walker
- One 700 KB article after a random resync covers PAT/PMT and the next GOP start in both TS and M2TS. Accept IRAP 16..21 with in-band parameter sets, not only IDR.
- Keep byte-offset bookkeeping in absolute file offsets; the packet index is relative to resync.
