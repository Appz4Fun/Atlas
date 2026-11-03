# w4a: container metadata position in real-world files (partial run)

Scope limits: only GitHub raw (ietf-wg-cellar/matroska-test-files, pytorch/vision test assets) and the npm registry were reachable. Kodi, ffmpeg FATE, demolandia, 4kmedia, dolby.com and CDNs other than registry/raw GitHub were refused or unreachable. **No real DV (profile 5/7/8), HDR10, HDR10+, anime ASS/font, MakeMKV, Atmos/E-AC-3 JOC or M4V sample was obtained.** Those rows are still open.

Walker: docs/research/container-metadata/tools/walk.py (run with `python3 -I`). Downloads in scratchpad `w4a/dl`, not in the mount.

## MKV (matroska-test-files, Matroska validation set, 2010-era mkvmerge)

Source base: https://raw.githubusercontent.com/ietf-wg-cellar/matroska-test-files/master/test_files/testN.mkv

| file | size | Info/Duration | Tracks | SeekHead / Tags | Cues | first Cluster | Tags end | Chapters/Attach | fits 700 KB? |
|---|---|---|---|---|---|---|---|---|---|
| test1 | 23.3 MB | 87.336 s (TS default 1 ms), MuxingApp libebml2 | msmpeg4v2 + MP3, CodecPrivate 40 B | Tags @472 | @634 | @803 | 634 | none | yes (Tags, Cues, Tracks, Info all before 803) |
| test2 | 21.1 MB | 475090 ms at TS 100000 = 47.5 s | AVC (CodecPrivate 35 B) + AAC | Tags @714 | not captured | @1724 | 916 | none | yes |
| test3 | 21.1 MB | 49.064 s | AVC + MP3 | Tags @556 | not captured | @1505 | 736 | none | yes |
| test4 | 21.3 MB | walker parsed no top-level children (bug, not debugged) | n/a | n/a | n/a | n/a | n/a | n/a | unknown |
| test5 | 31.8 MB | 46.665 s, mkvmerge v4.0.0 | 11 tracks: AVC, AAC, 9 x S_TEXT/UTF8 subs (hun/ger/fre/spa/ita/jpn/und...), 1 AAC commentary | **Tags @31,762,539 (tail)**; SeekHead points there | n/a | @6215, first cluster 1,517,335 B | EOF | none | **no: Tags at tail, and first cluster alone is 1.5 MB** |
| test6 | 23.3 MB | 87.336 s | V_MS/VFW + MP3 | Tags @469 | n/a | @663 | 663 | none | yes |
| test7 | 21.8 MB | 37.043 s | AVC + AAC | Tags @540 | n/a | @1405 | 799 | none | yes |
| test8 | 21.2 MB | 47.341 s | AVC + AAC | Tags @540 | n/a | @1488 | 715 | none | yes |

Cross-check: ffprobe on test1 reports duration 87.336000 s and 2 streams (msmpeg4v2, mp3), which matches the walker.

## MP4 (pytorch/vision test assets, ffmpeg Lavf56.25.101, no faststart)

Source base: https://raw.githubusercontent.com/pytorch/vision/main/test/assets/videos/{R6llTwEh07w,WUzgd7C1pWA}.mp4

| file | size | ftyp / mdat | moov | faststart | avc1 / mp4a | ©too | fits 700 KB? | fits 4 MB? |
|---|---|---|---|---|---|---|---|---|
| R6llTwEh07w.mp4 | 863,543 B | mdat @40, 851,607 B | @851,647, 11,896 B | no | 340x256 AVC, 44.1 kHz stereo AAC, timescales 15360 / 44100 | Lavf56.25.101 | no | yes |
| WUzgd7C1pWA.mp4 | 889,619 B | mdat @40, 876,677 B | @876,717, 12,902 B | no | 340x256 AVC, 48 kHz stereo AAC | (present) | no | yes |

No dvcC/dvvC, colr, mdcv, clli or btrt boxes in these samples (SDR, no HDR signalling).

## Findings

1. Matroska Tags are at the head in 7 of 8 test files (before the first Cluster, 500-900 B from the start). They fit in 700 KB.
2. test5 (mkvmerge v4.0.0, 11 tracks) puts Tags at the file tail, and SeekHead points there. Its first Cluster alone is 1.5 MB, so a 700 KB or 4 MB prefix gets no Tags and at most a couple of clusters. This is the main surprise.
3. Chapters and Attachments are absent in all 8 files, so no evidence yet on where mkvmerge writes big font attachments.
4. Old mkvmerge omits TimecodeScale and Info Title in several files. Duration without TimecodeScale must default to 1 ms.
5. MP4 from ffmpeg without faststart puts moov after mdat (at about 850 KB). A 700 KB prefix holds no sample table, so the reader needs a 4 MB prefix or a tail fetch.
6. All MP4 mdhd/hdlr/stco values cross-checked with the walker output above. Track timescales and first chunk offsets look right, and mdhd language decodes to und/eng after the fix.
7. Walker bug: test4.mkv yields no top-level Segment children, so its row is unknown. Needs debugging before the Rust port relies on it.
8. Not verified: DV RPU/dvcC placement, HDR10 mdcv/clli in MKV Colour, HDR10+ SEI, ASS/font attachments, MakeMKV Tags placement, E-AC-3 JOC dec3, M4V iTunes ilst tags.

## Open items for next run

- Get DV profile 5/7/8, HDR10 and HDR10+, a fansub with ASS + fonts, a MakeMKV MKV, and an Atmos MP4 (not reachable from the proxy so far).
- Fix test4 parse and test2/test3 Cues extraction.
- Check what a faststart-remuxed MP4 gives for the same window (ffmpeg -movflags +faststart) to compare.
