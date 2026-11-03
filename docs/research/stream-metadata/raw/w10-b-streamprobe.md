# w10-b streamprobe: front-bytes MKV/stream metadata probe

Source: code/streamprobe/main.rs, Cargo.toml, Cargo.lock (dolby_vision 3.4.0, hdr10plus 2.1.5, matroska-demuxer 0.8.1, scuffle-h265 0.2.2, scuffle-av1 0.1.4, h264-reader 0.9.0). Build: `cargo build --release`; run: `streamprobe <file> [--front-bytes 1MiB|256K|N]` (default 4 MiB). Output: pretty JSON with `container`, `video`, `audio[]`.

Combined from w05a (MKV, hvcC/avcC/av1C, SPS/VUI, SEI 5/137/144), w06a/w07c (RPU, HDR10+), w07a (AC-3/E-AC-3), w06b (TrueHD), w05c (DTS, FLAC). Extras beyond the spec: container.duration_s, chapters, subtitles, tags_count, video.mkv_colour, video.sps, audio language/name/track.

## Pass/fail vs ffprobe and injected values (4 MiB window)

| file | field group | result |
|---|---|---|
| x265a.mkv (HDR10 10-bit) | codec, profile Main 10, 640x360, 10-bit, 4:2:0, fps 23.976, range, primaries 9, transfer 16, matrix 9 | PASS (ffprobe) |
| x265a | level 2.1 (ffprobe raw 63), mastering G(0.265,0.69) B R WP 1000/0.0001 nits, MaxCLL 1000, MaxFALL 400, x265 encoder string | PASS |
| x265b.mkv (H.264 High) | profile High, level 3.0 (ffprobe 30), 640x360, 8-bit, 4:2:0, fps | PASS |
| c.mkv (AV1) | profile Main, level 2.1 (ffprobe idx 1), 640x360, 8-bit, full_range false (tv) | PASS |
| dv81.mkv (DV P8) | dv.profile "8", cm_version 4.0, el none, L2 2081, L5 0, L6 1000/50/1000/400, L9, L11, L254 | PASS vs injected |
| dv81.mkv | dv.profile 8.1 vs 8.4 (needs dvcC compat id) | FAIL/GAP: dvcC not found in this MKV |
| dv81.mkv | L1 min_pq | FAIL: 12 vs injected 62 (max 3079, avg 2048 match); unresolved |
| hdr10p.mkv | HDR10+ profile B, windows 1, target 400 nits, maxscl 800/750/700, avg 300 | PASS vs injected |
| comb_eac3.mkv (5.1 E-AC-3) | codec, 6ch, 48000 Hz, 640000 bps | PASS (ffprobe) |
| comb_ac3.mkv (5.1 AC-3) | 6ch, 48000, 640000 bps | PASS (ffprobe), after fixing CRC1 skip |
| comb_dts.mkv (DTS core) | 6ch, 48000, 1536000 bps | PASS (ffprobe) |
| comb_flac.mkv (FLAC) | 6ch, 48000, 24-bit, STREAMINFO md5 | parsed; md5 not cross-checked |
| comb_truehd.mkv (TrueHD) | 6ch, 48000, layout L R C LFE Ls Rs, num_substreams 2, peak 9.6 Mbps | channels/rate PASS; bitrate unverified (ffprobe N/A) |
| dialnorm (AC-3 and E-AC-3) | -31 on all test files | not cross-checked (ffmpeg default likely) |
| E-AC-3 atmos, TrueHD atmos | null (E-AC-3 JOC not parsed); false (TrueHD, 2 substreams) | not tested with Atmos sample |
| DV P5 / 8.4, MKV | no MKV vector available | not run |

## Front bytes needed

- Video fields (all of the above): identical at 256 KiB, 1 MiB and 4 MiB for x265, x264, AV1, DV, HDR10+ files. The first keyframe fits in 256 KiB at 640x360.
- Audio and container: the binding constraint is `matroska_demuxer::MatroskaFile::open`, not the codec parsers. It follows the SeekHead to Cues/Tags, which ffmpeg writes at EOF. With a 1 MiB window, the 1.7 MB comb_* files fail to open. The 8.6 MB TrueHD file needs the full file (fails at 8 MiB). A PadReader (zeros past the window) did not fix it.
- Remedy: skip SeekHead and parse Tracks/first Clusters directly with a minimal EBML walker. Not done here.

## Key gaps

1. MKV open needs the file tail via SeekHead (above).
2. dvcC/dvvC (BlockAdditionMapping) is not read through the crate. Raw search found none in dv81.mkv, so the 8.1 vs 8.4 compat id is missing.
3. L1 min_pq mismatch on dv81 (12 vs 62). Not root-caused; compare with w06a output.
4. E-AC-3 Atmos (EMDF/JOC) not implemented.
5. Audio codec names are raw MKV ids for AAC (A_AAC), not normalized.

## Example output (dv81.mkv, video.dv and HDR fields; 4 MiB)

{"dv":{"profile":"8","el":"none","cm_version":"4.0","l1":{"min_pq":12,"max_pq":3079,"avg_pq":2048},"l2_target_max_pq":[2081],"l6":{"max_display_mastering_nits":1000,"min_display_mastering_nits_raw":50,"max_cll":1000,"max_fall":400},"l5":{"left":0,"right":0,"top":0,"bottom":0},"l9":0,"l254_dm_mode":0,"dvcc":null},"hdr_formats":["HDR10","Dolby Vision"],"max_cll":1000,"max_fall":400}

## Example output (comb_eac3.mkv, audio[0])

{"codec":"eac3","channels":6,"sample_rate":48000,"bitrate":640000,"dialnorm":-31,"atmos":null,"flac_md5":null,"track":2,"language":"und"}
