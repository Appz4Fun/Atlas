# w06-c mid-file walker (Atlas, 700 KB articles = byte ranges)

Legend: MEASURED = run in this session. ASSUMED = not measured.
Code: docs/research/stream-metadata/code/w06c_main.rs (std + rust_h264 0.6.0, release build).
Scratch: /tmp/claude-0/-home-claude/2f11c7bb-b3fb-5488-8bbf-52c60549e24e/scratchpad/w6c.gIPn (t.mkv, t_tail.mkv, refs).

## Test vectors (MEASURED)
- t.mkv: 60 s, 1280x720 H.264 (libx264 crf 18 -g 120 -keyint_min 120, yuv420p), AC-3 640k 5.1, `-cues_to_front 1`. 34,475,160 bytes = 50 articles.
  Keyframes every 5 s. Cues at 753 (front). SeekHead at 52. 23 Cluster elements.
- t_tail.mkv: same, default ffmpeg 6.1 muxing. Cues at 34,474,905 (tail, article 49). SeekHead points there.
- Note: 0x1C53BB6B also appears inside the SeekHead SeekID payload (offset 112), so raw byte search for Cues is misleading.

## Results (MEASURED, in-memory articles; wall time excludes network)
| path | keyframe time | articles total (incl 6 front) | articles at keyframe | 10 s AC-3 | wall |
|---|---|---|---|---|---|
| scan from 50% (article 25), t.mkv | 35.000 s | 22 (16 beyond front) | 14 | 313 frames = 10.016 s | 36 ms |
| cues, t_tail.mkv, target 30 s | 30.000 s | 20 (14 beyond front) | 12 | 313 frames = 10.016 s | 13 ms |
| cues, t.mkv (front Cues), target 30 s | 30.000 s | 19 (13 beyond front) | 11 | 313 frames = 10.016 s | 13 ms |

- Scan path: scan started at article 25 (~17.5 MB). First valid cluster at 20,084,792 (article 28). The keyframe is in that same cluster (cluster ts 34848 ms, keyframe block at 35000 ms, 40,920 B).
- Articles touched include the whole cluster that holds the keyframe, because each block header sits in a different article and the walker has to fetch them in order. Cluster granularity costs about 4 articles per 2.5 MB cluster here.
- Cues path cost: 1 article (the tail Cues article) plus the cluster articles. Front-Cues file needs no extra article.
- Articles needed for the keyframe alone: 8 beyond front (scan) or 5-6 beyond front (cues). Audio 10 s adds about 2-3 more articles at this bitrate (~575 KB/s total).

## Validation (MEASURED)
- Raw 0x1F43B675 hits in t.mkv: 23 total. In the scanned region: 1 hit, rejected 0. False positives: 0 observed (no decoy data in this vector, so this does not prove the rate).
- BUG FOUND AND FIXED: the first validator rejected 11 genuine clusters with reason "no-E7-first". ffmpeg writes a CRC-32 element (ID 0xBF, 4-byte payload) as the first child of each Cluster, before Timestamp 0xE7. Rule (b) in w04-a must allow optional CRC-32 (0xBF) and Void (0xEC) children before E7. The fixed validator skips up to 3 of them.

## Decode (MEASURED)
- rust_h264 0.6.0 (pure Rust, decodes AVCC): SPS/PPS from avcC via parse_avcc_config, then the IDR sample via parse_avcc(length_size). No Annex B conversion needed.
- Keyframe decode time 12.5 ms (1280x720, release).
- Luma compared with ffmpeg `-ss T -i x.mkv -frames:v 1 -pix_fmt yuv420p` at the same timestamp: 921,600 / 921,600 luma bytes exact, mean abs diff 0.0000, pHash Hamming 0 (both t=35 s and t=30 s). Chroma not compared.
- pHash: custom DCT pHash (box downscale to 32x32, 8x8 low-frequency DCT block, median threshold), not image_hasher.

## Not tested
- Lacing (Xiph, fixed, EBML) is coded but not exercised: ffmpeg AC-3 in MKV is unlaced.
- Unknown-size clusters (live muxers): the code walks to the next Level-1 ID or stops. Not tested.
- Real false-positive rate on decoy data: not measured.
- Network time: not measured (articles are slices of an in-memory file).
- MP4 and TS: out of scope for this run.

## Conclusions
1. Mid-file keyframe plus 10 s audio costs about 16 articles beyond front on a 1080p-class-bitrate file (scan path), about 14 via tail Cues. Front Cues cost 13.
2. Cues path is cheaper and deterministic. With front Cues (ffmpeg -cues_to_front 1) the mid-file lookup needs zero Cues articles.
3. Keyframe output matches ffmpeg byte-exact in luma, so rust_h264 is usable on Atlas's mid-file path as-is.
4. Update w04-a: cluster validator must allow CRC-32/Void before Timestamp.
