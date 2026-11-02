# w05a: video elementary-stream probe from first MiB of MKV (Rust vs ffprobe)

Scope: 3 synthetic 2 s clips (640x360 testsrc2, 24000/1001). Only the first 1 MiB of each file is read (all samples are under 160 KB, so the whole file was in the window). Ground truth: `ffprobe -show_streams -show_frames -read_intervals %+#1`.

## Field table

| File | Field | ffprobe | Rust | Crate | Match |
|---|---|---|---|---|---|
| a | codec / MKV codec_id | hevc / (n/a) | V_MPEGH/ISO/HEVC | matroska-demuxer | yes |
| a | profile | Main 10 | hvcC profile_idc=2 | scuffle-h265 | yes |
| a | level | 63 | level_idc=63 | scuffle-h265 | yes |
| a | size | 640x360 | SPS 640x360, cropped 640x360 | scuffle-h265 | yes |
| a | chroma / bit depth | yuv420p10le | chroma_idc=1, bd_y=10, bd_c=10 | scuffle-h265 | yes |
| a | primaries / transfer / matrix | bt2020 / smpte2084 / bt2020nc | VUI 9 / 16 / 9 | scuffle-h265 | yes |
| a | range | not captured | VUI full_range=false | scuffle-h265 | n/a |
| a | fps | 24000/1001 | VUI 24000/1001 (num_units 1001, time_scale 24000) | scuffle-h265 | yes |
| a | MDCV (SEI 137) | G(13250,34500) B(7500,3000) R(34000,16000) WP(15635,16450) L(1e7,1) | identical raw values; max 1000 cd/m2, min 0.0001 | manual SEI walk | yes |
| a | CLL (SEI 144) | 1000 / 400 | 1000 / 400 | manual SEI walk | yes |
| a | x265 UUID SEI 5 | 2 user-data SEI side-data entries | 1 SEI5 in frame0 NAL 39 (uuid 2ca2de09-b517-47db-bb55-a4fe7fc2fc4e, x265 build string) | manual SEI walk | partial, unresolved count |
| a | DV RPU (NAL 62) | not present | not present (frame0 NAL types 20,32,33,34,39) | manual | yes (none) |
| a | first keyframe offset | pkt_pos 3101 | payload at 3105 | - | +4 (block header) |
| b | codec / profile / level | h264 / High / 30 | V_MPEG4/ISO/AVC / profile_idc 100 / 30 | h264-reader | yes |
| b | size / chroma / bit depth | 640x360 / yuv420p / 8 | 640x360 / YUV420 / bd 8 | h264-reader | yes |
| b | fps | 24000/1001 | VUI timing 1001/48000 (field-doubled; fps = scale/(2*tick)) | h264-reader | yes (with /2 rule) |
| b | colour | none | VUI video_signal None | h264-reader | yes |
| b | x264 SEI 5 | 1 user-data | uuid dc45e9bd-e6d9-48b7-962c-d820d923eeef, "x264 - core 164 r3108 31e19f9" | manual | yes |
| b | first keyframe offset | pkt_pos 602 | payload at 606 | - | +4 |
| c | codec / profile / level | av1 / Main / 1 | V_AV1 / seq_profile 0 / seq_level_idx_0 1 | scuffle-av1 | yes |
| c | size | 640x360 | max_frame 640x360 | scuffle-av1 | yes |
| c | chroma / bit depth | yuv420p / 8 | subsampling (1,1), bit_depth 8, mono false | scuffle-av1 | yes |
| c | colour | none | primaries/transfer/matrix = 2 (unspecified) | scuffle-av1 | consistent |
| c | fps | 24000/1001 | seq timing_info None; fps only from MKV DefaultDuration 41708333 ns | matroska-demuxer | yes (container) |
| c | first keyframe offset | pkt_pos 564 | payload at 568 | - | +4 |

Keyframe offsets: the Rust value is the first 32 bytes of the demuxed frame payload found in the file. ffprobe pkt_pos is the SimpleBlock start. The constant +4 is the block header (track vint 1 + timecode 2 + flags 1).

## Where the SEI sits

- x265 (a): repeat-headers is on. Prefix SEIs (CLL, MDCV, UUID) appear in frame 0 (NAL 39 x3). The same three SEIs are also copied into the hvcC array `PrefixSeiNut` (n=3), so hvcC holds them too. VPS, SPS and PPS are in hvcC and frame 0.
- x264 (b): the SEI 5 sits in frame 0 (NAL 6). avcC holds only SPS/PPS.
- SVT-AV1 (c): frame 0 OBUs are [1 sequence header, 6 frame]. No temporal delimiter (stripped in MKV). av1C configOBUs is 13 bytes.

## Crates that worked

- matroska-demuxer 0.8.1 (`MatroskaFile::open(Cursor<Vec<u8>>)`, `tracks()`, `next_frame(&mut Frame)`, `codec_private()`, `video().colour()`)
- scuffle-h265 0.2.2 (`HEVCDecoderConfigurationRecord::demux(&[u8])`, `SpsNALUnit::parse(&[u8]).rbsp`, `cropped_width()`, `vui_parameters.video_signal_type`, `vui_timing_info`)
- h264-reader 0.9.0 (`nal::sps::SeqParameterSet::from_bits(rbsp::BitReader::new(&[u8]))`, `profile_idc`, `level_idc`, `pixel_dimensions()`)
- scuffle-av1 0.1.4 (`AV1CodecConfigurationRecord::demux(&mut Cursor<Bytes>)`, `ObuHeader::parse`, `seq::SequenceHeaderObu::parse(hdr, &mut payload)`)
- bytes version = "1.12.1" (needed for the av1C Cursor<Bytes>)

Versions: see docs/research/stream-metadata/code/w05a_main.rs for the full program. Run: `probe <file.mkv>`.

## Failures and notes

- scuffle-av1: the sequence header type is under `scuffle_av1::seq::`, not the crate root (the root exports only `ObuHeader`, `ObuType`, `seq`).
- h264-reader: `SpsError` does not implement std Error. Map it to String before `?`.
- Matroska Colour element (container): in a.mkv only `range` is present. Primaries, transfer, matrix, MaxCLL and MDCV are all None in the container, though the HEVC bitstream carries them. Do not rely on the Matroska Colour element for HDR. Read SPS VUI and SEI 137/144.
- DV RPU was not tested. None of the three samples has DV. ffmpeg cannot make DV, so the RPU path (NAL 62 / UNSPEC62) is unproven.
- Not extracted: has_b_frames (VUI bitstream_restrictions), audio, chapters, tag contents (only counts printed: 2 tags, 0 chapters).
- Open item: ffprobe reports 2 user-data SEI side-data entries on frame 0 of a.mkv, and the Rust walk finds 1 SEI5 in frame 0. Not resolved in this run.
