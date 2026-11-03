# W07-B: video decoder robustness (4K cost, HDR10 container, open-GOP CRA, interlaced H.264, MPEG-2)

Env: release build, cargo 1.97. Crates: rust_h265 0.4.0, rusty_h265 0.6.2, rust_h264 0.6.0, oxideav-mpeg12video 0.0.13, matroska-demuxer 0.8.1. References: ffmpeg 6.1.1, Y plane only (yuv420p10le / yuv420p). mkvmerge NOT available: `apt-get install mkvtoolnix` failed ("Unable to locate package"; apt-get update not tried). The HDR10 MKV was written by ffmpeg libx265 (Lavf 60.16.100) instead.

## 1. 2160p cost (3840x2160 10-bit, 3 frames, x265 default GOP, 1 thread)
| decoder | Y0 bit-exact | ms/frame (min of 5 runs, whole 3-frame run / 3) | VmHWM (whole process) |
|---|---|---|---|
| rust_h265 0.4.0 | yes (PSNR inf) | 89.8 | 288 MB |
| rusty_h265 0.6.2 | yes (PSNR inf) | 87.3 | 227 MB |
VmHWM includes the harness (ref file and u16 copies), so decoder-only RSS is lower. Each decoder ran in its own process.

## 2. HDR10 container (1280x720 10-bit, x265 hdr10=1, master-display, max-cll=1000,400)
- Remuxed to MKV by ffmpeg (no mkvmerge). Matroska codec_id V_MPEGH/ISO/HEVC, codec_private 2533 bytes, lengthSize 4.
- hvcC arrays: VPS(32) n1, SPS(33) n1, PPS(34) n1, SEI prefix(39) n3. The SEI array IS kept in hvcC.
- Frame 0 in-band NALs: 32, 33, 34, 39, 39, 39, 20. The SEIs are payload 144 (CLL, size 4), 137 (MDCV, size 24), 5 (x265 info, size 255).
- Matroska Colour as read by matroska-demuxer: matrix Bt2020Ncl, range Broadcast, transfer Bt2100, primaries Bt2020, bits_per_channel None, mastering_metadata None, max_cll Some(9), max_fall Some(9).
- ANOMALY (unresolved): MaxCLL=9 does not match the SEI 144 value (1000/400). ffprobe reports no side data. Either ffmpeg wrote a wrong element or the demuxer reads it wrong. Do not trust Matroska MaxCLL/MaxFALL until the EBML bytes are checked in hex.
- ffmpeg wrote no MasteringMetadata element. Mastering display data exists only in-band SEI 137 and in the hvcC SEI array.
- Bit depth 10 came from the SPS (decoders), not from Matroska BitsPerChannel, which is absent.

## 3. Open-GOP CRA start (960x540 10-bit, x265 keyint=48, default open GOP, 120 frames)
- Stream: IDR at 0, CRA (NAL 21) at 48 and 96. Cut = VPS/SPS/PPS + data from the CRA at frame 48.
- rust_h265 0.4.0: 72 outputs = display frames 48..119. All 72 are bit-exact to full-stream frames 48+i.
- rusty_h265 0.6.2: identical, 72/72 bit-exact.
- Both decoders drop the RASL pictures (display 45-47) with no leading pictures in the output. Full-stream decode gives 120 outputs for each decoder.
- Answer: yes, both output a clean picture at a CRA with RASL skipped.

## 4. Interlaced H.264 (720x480 4:2:0 8-bit High, `-flags +ildct+ilme -x264-params tff=1`)
- SPS: profile_idc 100, frame_mbs_only_flag=0, mb_adaptive_frame_field_flag=1, log2_max_frame_num=4, 720x480.
- Slice census: 4 slices with field_pic_flag=0 (MBAFF frame pictures), 0 field pictures. x264 emits MBAFF only, so PAFF is NOT covered.
- rust_h264 0.6.0: 4 frames out. Frame 0 bit-exact vs ffmpeg Y (PSNR inf).

## 5. MPEG-2 (720x480 yuv420p, mpeg2video q:v 3, one I-frame)
- oxideav-mpeg12video 0.0.13 `decode_video_sequence()`: one Intra frame, 720x480. Y PSNR vs ffmpeg 76.34 dB, NOT bit-exact (max diff not measured). Chroma not compared.
- Usable as a lossy thumbnail path; not a byte-exact oracle.

## Summary
- HEVC 10-bit 4K: both decoders bit-exact, about 90 ms per frame single-thread. Atlas can decode only the first keyframe for thumbnails.
- CRA start: both HEVC decoders skip RASL cleanly, so a stream can start at any CRA.
- H.264 MBAFF: rust_h264 bit-exact. PAFF is untested.
- MPEG-2: oxideav-mpeg12video is about 76 dB, not exact.
- Open: MKV MaxCLL readback anomaly (section 2); mkvmerge not tested.

Code: docs/research/stream-metadata/code/w07b_main.rs (subcommands t1..t5; test vectors in the scratchpad s/ directory)
