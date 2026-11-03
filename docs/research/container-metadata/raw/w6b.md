# w6b: container-independent ALT keys (remux identity test)

Scope: one synthetic source (30 s, 1920x1080 libx264 veryfast keyint 48 bframes 3, AC-3 2ch 192k, 1 SRT),
10 remux variants, 1 negative control (same film, different encode, crf 28). Tools: mkvmerge v82, ffmpeg 6.1.1.
Script: tools/altkey.py (stdlib, `python3 -I altkey.py [--limit N] [--ref REF] FILES`).
Working dir /tmp/w6b (scratch, not kept).

Variants (all derived from src.264 + src.ac3 + sub.srt unless noted):
- a.mkv    mkvmerge (reference)
- b.mkv    mkvmerge, 61 s later
- c1/c2    mkvmerge --engage no_variable_data, back to back (1 s apart)
- d1/d2    ffmpeg -c copy -> MKV (from a.mkv), 2 s apart, d2 without bitexact
- e.mp4    ffmpeg -c copy -> MP4 -movflags +faststart (from a.mkv; SRT -> mov_text)
- f.mkv    mkvmerge from e.mp4 (round trip)
- g1.mkv   mkvmerge --cluster-length 48 (blocks)
- g2.mkv   mkvmerge --cluster-length 500 (blocks)
- n_crf28  negative control: different encode, same audio and subtitle
- NOT DONE: lacing variant (my two attempted flags were rejected; no lacing case tested)

## 1. Byte diff vs a.mkv (walk.py output diffed + cmp)

| pair | differing bytes | what differs |
|---|---|---|
| a vs b | 78 | Info DateUTC (8 B) and two `_STATISTICS_WRITING_DATE_UTC` tags. Clusters, Tracks, Cues identical. |
| a vs c1 | 19.5 M (layout) | MuxingApp/WritingApp become `no_variable_data`; DateUTC becomes an invalid value; statistics tags dropped; Void/Info/Tracks sizes shift. Cluster bytes equal (first cluster 1262422 B both). |
| c1 vs c2 | 0 | byte-identical |
| a vs d1 | 19.5 M (layout) | Lavf: Tags moved before Clusters, Void removed, first cluster 1259705 B (was 1262422). |
| d1 vs d2 | 76 | DateUTC only |
| a vs e.mp4 | all | different container |
| a vs f.mkv | 19.5 M (layout) | Void 1131->1176 B, one extra Tag, DateUTC; cluster sizes identical. |
| a vs g2 | 74 | DateUTC + statistics tags; cluster layout identical at 500-block limit. |
| a vs g1 | 19.0 M | 45 clusters (first 539 KB) instead of 15 (first 1.26 MB). |

TrackUID: unchanged across a/b/c/g2 (no diff lines). SegmentUID: not present in these outputs.
Statistics-tag dates and DateUTC are the only volatile fields in the default mkvmerge mux.

## 2. Key matrix (full parse; "match" = equal to a.mkv)

| variant | cfg_avcc | cfg_parm | ac3_norm | v_f0 | v_gop1 | v_n24 | a_f100 | meta |
|---|---|---|---|---|---|---|---|---|
| b.mkv | match | match | match | match | match | match | match | match |
| c1.mkv | match | match | match | match | match | match | match | match |
| c2.mkv | match | match | match | match | match | match | match | match |
| d1.mkv | match | match | match | match | match | match | match | match |
| d2.mkv | match | match | match | match | match | match | match | match |
| e.mp4 | match | match | match | match | match | match | match | match |
| f.mkv | match | match | match | match | match | match | match | match |
| g1.mkv | match | match | match | match | match | match | match | match |
| g2.mkv | match | match | match | match | match | match | match | match |
| n_crf28 (neg.) | NO | NO | match | NO | NO | NO | match | match |

Key definitions (sha256 prefix, 16 hex):
- cfg_avcc: avcC record (MKV CodecPrivate == MP4 avcC payload)
- cfg_parm: SPS+PPS NAL bytes (framing-independent)
- ac3_norm: fscod, bsid, bsmod, acmod, lfeon from first AC-3 syncframe
- v_f0: first video sample (AVCC payload, decode order)
- v_gop1: payloads from sample 0 to next sync sample
- v_n24: first 24 video payloads
- a_f100: first 100 AC-3 payloads
- meta: normalized codec list + video/audio frame counts (full parse only)

## 3. Prefix test (--limit 700000, a Atlas-style read; keys valid only if prefix holds N frames)

| variant | visible V / A frames | first 1V+1A | first 8V+16A | first 24V+100A |
|---|---|---|---|---|
| a, b, c1, f, g2 | 26 / 40 | match | match | match |
| d1 | 26 / 32 | match | match | A short (32) |
| e.mp4 | 25 / 30 | match | match | short (30 A) |
| g1 | 26 / 37 | match | match | short (37 A) |
| n_crf28 | 48 / 63 | NO (video) | NO (video) | NO |

v_gop1 at 700 KB: MP4 is short (partial GOP), so NO. Byte-budget keys ("first N bytes of video") break
because the file offset cut-off sees a different frame count per interleave; dropped from the design.
Audio alone (a_f100, ac3_norm) matches the same-film negative control: audio cannot discriminate.

## 4. Measured sizes and x264 SEI

- IDR frame 0: 40 214 B (qp-normal source). Negative control IDR: 26 613 B.
- first GOP (48 frames incl. P/B): 1.21 MB. A 700 KB prefix does NOT contain a full GOP.
- Frame 0 NAL types: 7 (SPS), 8 (PPS), 6 (SEI), 5 (IDR). No AUD (type 9) was present in this source.
- x264 user-data SEI (UUID dc45e9bd..., 678 B) is byte-identical in frame 0 of every variant.

## 5. Findings for Atlas

1. Remuxes (mkvmerge, ffmpeg -c copy, MKV<->MP4, cluster-size changes) keep every frame payload byte-identical; only container framing and volatile tags change.
2. The volatile fields in a default mkvmerge file are DateUTC and two statistics-tag dates (78 B). --engage no_variable_data is deterministic but writes garbage DateUTC and strips the "mkvmerge" WritingApp; do not key on those.
3. avcC (cfg_avcc) and frame payload hashes survive every remux tested. avcC also separates the same film at a different encode (crf 28 -> cfg differs).
4. Frame-count keys are the right unit. A byte-budget prefix does not give the same frame set across muxers (26 vs 25 vs 30 AC-3 frames in 700 KB).
5. Smallest key that survives every tested remux inside 700 KB: v_f0 + first 8 video payloads + first 16 AC-3 payloads, together with cfg_parm. It needs about 200 KB here; at real 1080p bitrates the IDR is more like 100-500 KB, so budget 700 KB and check coverage.
6. Audio-only keys (a_f100, ac3_norm, meta) are NOT discriminative: they match the same-film control. Always combine with video.
7. The first-cluster boundary (mkvmerge 15 clusters vs 45) does not change payload hashes. It only changes how many frames the prefix holds.

## Caveats (not tested)

- No real Usenet media, no re-RAR, no Atmos/DTS-HD, no 4 MB prefix case for the crf28 negative.
- No AUD-carrying source (x264 --aud), no TS/M2TS input, no lacing variant (see NOT DONE), no fragmented MP4.
- MP4 side handled only for non-fragmented, stss/stsc/stco/co64 files; MKV only for SimpleBlock/BlockGroup with no unknown-size Clusters.
- meta key is not a discriminator (equal for same-film).

## Reproduce

Source: ffmpeg -f lavfi testsrc2 1920x1080 24 fps 30 s, libx264 veryfast, keyint=48 min-keyint=24 scenecut=40 bframes=3 aq-mode=1 -> src.264; ffmpeg lavfi sine 48 kHz 2 ch -> ac3 192k -> src.ac3; sub.srt 2 cues.
Muxes: mkvmerge -q -o X.mkv --default-duration 0:24000/1001p src.264 src.ac3 sub.srt (and variants above).
Keys: python3 -I tools/altkey.py --ref a.mkv b.mkv c1.mkv c2.mkv d1.mkv d2.mkv e.mp4 f.mkv g1.mkv g2.mkv
