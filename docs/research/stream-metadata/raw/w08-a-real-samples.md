# W08-A real samples: status BLOCKED (partial)

## Outcome
The requested corpus (TrueHD Atmos, E-AC-3 JOC, DTS-HD MA / DTS:X, DV P7 FEL, DV 8.4, E-AC-3 7.1) was NOT obtained. No parser was run against real TrueHD, E-AC-3, DTS or DV samples. No Rust code changed, so no code/w08a_main.rs was written.

## Why
- https://samples.ffmpeg.org/ and https://fate-suite.ffmpeg.org/ both fail at the agent egress proxy: CONNECT 403, connect_rejected ("host is not allowed for this session"). This is a deliberate allowlist block. I did not look for mirrors or other hosts to get around it.
- One proxy-status probe was denied by the auto-mode classifier. I stopped probing after that.
- dovi_tool test assets: not reachable from this session. The dolby_vision 3.4.0 crate tarball has no test assets.

## What was reachable (crates.io via cargo fetch, no build scripts run)
- oxideav-ac3 0.0.11, tests/fixtures/sine440_stereo.ac3 (12288 B) and transient_bursts_stereo.ac3 (48384 B).
  ffprobe: codec ac3, 48000 Hz, stereo, 192 kb/s, 0.512 s and 2.016 s. Encoder unknown (crate tests compare against ffmpeg). Plain AC-3: no E-AC-3, no JOC.
- matroska-demuxer 0.8.1, tests/data/hdr.mkv (99764 B). ffprobe: video "mpeg4" Simple Profile, 3840x2160, yuv420p, bt2020, smpte2084, MDCV and CLL side data on the first key frame; audio vorbis stereo 48 kHz; duration 0.128 s. Not HEVC and not DV, so it is only a container and HDR-SEI check.
- Other matroska-demuxer fixtures (subtitles.mkv, subtitles_pgs.mkv, subtitles_vobsub.mkv, simple.mkv, multi_seekhead.mkv, no_cluster_position.mkv). Container-structure only. Not probed.
- Crates with no media fixtures (sources, XML or docs only): truehd 0.7.2, truehdd 0.6.2, filmcraft-ac3 0.9.0, revelo-parsers-audio 0.5.6, vuio-media-info 0.1.4, mkvparser 0.2.0, oxideav-mkv 0.0.11, symphonia-format-mkv 0.6.1, mp4forge 0.8.0, oxideav-mp4 0.0.10, mkvdump 0.6.0, mkv-element 0.4.0.

## Parser runs
- Not run in the time cap. w07a needs symphonia, rusty_chromaprint and oxideav-core. The two AC-3 fixtures are the only real encoder bitstreams here and are the obvious first run for w07a BSI (dialnorm, bsmod, acmod, lfeon, substream count).
- Ground truth for those two files is the ffprobe output above.

## Open items (still unresolved)
- TrueHD Atmos rule: ffmpeg num_substreams==4 && MSB vs MediaInfo MSB-only.
- TrueHD major-sync CRC byte order (w06 read it big-endian, 375/375; ffmpeg reads LE).
- E-AC-3 JOC: addbsi vs EMDF detection.
- DTS:X sync 0x02000850.
- DV P7 FEL el_type (only a dovi_tool-made MEL exists so far).
- E-AC-3 7.1 dependent-substream chanmap.

## Needed to close
- Either allow samples.ffmpeg.org and fate-suite.ffmpeg.org in the session's egress allowlist, or place sample files (each under 30 MB) in the research folder. Then rerun the w06b, w07a, w05c, w06a and w07c parsers against them.
