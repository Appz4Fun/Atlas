# W08-B: same-film keyframe hash across releases (PARTIAL, time cap hit)

Status: source clip and variants made. The Rust hash pipeline was NOT written or run, so there are no Hamming numbers and no threshold recommendation yet. No code file was written to code/w08b_main.rs.

## Source (open content)
- Netflix Open Content (opencontent.netflix.com) and Blender hosts (download.blender.org, test-videos.co.uk), archive.org, mkvtoolnix, matroska.org: blocked by proxy policy (403 on CONNECT). Do not retry.
- storage.googleapis.com gtv-videos-bucket: AccessDenied (anonymous).
- Used instead: npm package big-buck-bunny-1080p@0.0.6 (tarball 22.9 MB), file package/video.mp4. Big Buck Bunny, Blender Foundation, CC-BY 3.0 (package licence ISC is the wrapper's).
- Source: H.264 High 1920x1080 yuv420p, 24/1 fps, 30 s, 720 frames, AAC audio.
- No HDR source was available, so variant (e) PQ vs tone-mapped was not made. zscale and tonemap filters do exist in the installed ffmpeg.

## Variants made (5 s offset, 10 s long, 240 frames each, in $W/var)
| id | encode | result |
|---|---|---|
| a | x264 crf 18 1080p yuv420p | 12.8 MB |
| b | x265 crf 26 720p yuv420p10le | 1.6 MB |
| c | 2.39:1 letterbox inside 1920x1080 (scale 1920x808 + pad top 136) | 9.7 MB |
| d | eq brightness +0.04, gamma 1.08 | 12.6 MB |
| f | setpts 24/25 -> 25 fps (PAL speed-up, same frames at different timestamps) | 12.6 MB |
| e | not made (no HDR source) | - |

Note: f is a 25 fps retime of 24 fps material, so the frame at a given index differs in timestamp. The first attempt used -ss 30, which is past the 30 s end and gave empty files. Re-run with -ss 5 was valid.

## Not done (next steps)
1. Rust decode of one frame per variant with rust_h264 0.6.0 (a, c, d, f) and rust_h265 0.4.0 (b). Reuse the decode and to_32 path in docs/research/stream-metadata/code/w05b_main.rs.
2. Hashes: dHash 8x8, pHash (DCT 32->8), blockhash 16. Run with (i) no crop, (ii) auto letterbox crop (drop rows with mean luma < 20), (iii) histogram equalisation.
3. Distances: Hamming vs (a) for each variant, and vs 5 unrelated frames from the same film for the separation margin.
4. Use 3 keyframes at 25/50/75% for the speed-up case (f). Matching on a sequence of frames is untested.

## Cautions for the pipeline
- The variants are encodes of one 10 s window, so "different scene" frames need a separate cut from another part of the film. The one-clip source makes "same film" and "same scene" hard to tell apart.
- Use -pix_fmt yuv420p as the reference, not gray (see w05-b-decode-test.md).
