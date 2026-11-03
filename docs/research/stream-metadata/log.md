# Elementary-stream metadata research log (video, HDR/DV, audio)

Started 2026-11-02 06:52Z. 10 waves x 3 Haiku agents (high effort, 10 min cap), Opus main merges and steers.
Scope: video ES (HEVC/AVC/AV1 parameter sets, VUI, SEI encoder strings), HDR (HDR10 MDCV/CLL, HDR10+, Dolby Vision config + RPU L1-L11/L254, FEL/MEL), audio (codec, layout, rate, depth, bitrate, Atmos/DTS:X, dialnorm), frame sampling/hashing, audio fingerprints. Rust-native only (no ffmpeg/mediainfo).
Output: YAML field catalog at research/stream-metadata/catalog.yaml. Raw agent output in raw/.

## Wave 1 (06:52Z)
- A: video ES parameter sets, VUI, encoder SEI strings, Rust parsers
- B: HDR10 / HDR10+ / Dolby Vision config + RPU via dolby_vision crate
- C: audio ES headers (AC-3/E-AC-3/TrueHD/DTS/AAC/FLAC/Opus), Atmos/DTS:X flags, Rust parsers

### Wave 1 merge (06:56Z)
Agents finished in ~2 min each (well under cap). Raw: raw/w01-a-video.md, w01-b-hdr-dv.md, w01-c-audio.md.
- Video: hvcC/av1C carry profile/tier/level, chroma, bit depth and the VPS/SPS/PPS arrays, so the MKV CodecPrivate alone gives most fields without any frame. VUI gives crop (1088->1080), SAR, full-range, primaries/transfer/matrix (PQ+BT.2020 = HDR10), exact fps from time_scale/num_units_in_tick. Crates: scuffle-h265 0.2.2 (SPS+VUI, MIT/Apache, "unstable"), scuffle-av1 (av1C), h264-reader 0.8 (SPS/PPS/SEI). x264/x265 options string SEI (first IDR AU, not in hvcC) = strongest encode fingerprint, UUID UNVERIFIED.
- HDR/DV: MKV Colour IDs verified (MaxCLL 0x55BC, MaxFALL 0x55BD, MasteringMetadata 0x55D0..0x55DA). DV config in MKV BlockAdditionMapping 0x41E4 (BlockAddIDType dvcC/dvvC, ExtraData 0x41ED). dolby_vision crate 3.4.0 MIT: DoviRpu::parse_unspec62_nalu, el_type (FEL/MEL), cmv40 (CM v4.0 vs 2.9), vdr_dm_data L1..L11. FEL/MEL rule UNVERIFIED.
- Audio: symphonia has no AC-3/E-AC-3/DTS/TrueHD, so those headers need our own small parsers. bdinfo-rs-core has ac3.rs/truehd.rs (license UNVERIFIED). DD+ Atmos = EMDF sync 0x5838 + payload 14 (JOC); TrueHD major sync 0xF8726FBA; DTS ExSS 0x64582025, XLL 0x41A29547, DTS:X probe. FLAC STREAMINFO MD5 = rename-proof audio key. MKV flags commentary/HI/VI IDs verified.
Decision: LEAN IN on source reads for wave 2 (verify the UNVERIFIED keys). Wave 3 pivots to frame sampling/hashing and audio fingerprints.

## Wave 2 (06:57Z)
- A: x264/x265/NVENC/SVT-AV1 encoder strings in source; MKV Info MuxingApp/WritingApp, mkvmerge statistics tags (BPS, NUMBER_OF_FRAMES, DURATION) placement; h264 VUI crate coverage
- B: dovi_tool source: FEL/MEL rule, extension metadata block fields, dvcC bit layout, hdr10plus parser, frames needed
- C: dec3/JOC bit layout, TrueHD Atmos (ffmpeg mlp_parser), DTS:X (dca_xll), AAC ASC, bdinfo-rs license

### Peer input after wave 1 (06:56Z)
- Container thread owns container elements (MKV Colour, BlockAdditionMapping, MP4 colr/mdcv/clli/dvcC, BD MPLS STN dynamic_range). mkvmerge statistics tags (BPS, NUMBER_OF_FRAMES) are at END of file. I own bitstream side.
- Archive thread: store-mode RAR vol1 = <200 B headers then raw inner bytes, 1:1 article->inner offset mapping; ~700 KB first article = 700 KB of MKV front. Compressed RAR / LZMA 7z unusable without decompressor; 7z -mx0 raw data after 32 B header but file table at end.
- Archive thread (06:58Z): pure-Rust compressed RAR decoder = rars 0.12.3 (Apache-2.0, young); 7z via sevenz-rust2 0.24 / lzma-rust2. Prefix-streaming decode of non-solid compressed archives may yield the MKV front, untested.

### Wave 2 merge (07:01Z)
Raw: raw/w02-a-encoder-mux.md, w02-b-dovi-source.md, w02-c-audio-atmos-dtsx.md. Source-verified now:
- x264 SEI: user_data_unregistered UUID dc45e9bd-e6d9-48b7-962c-d820d923eeef, "x264 - core %d%s - H.264/MPEG-4 AVC codec - Copy%s 2003-2024 - ... - options: %s" (no width/height in options), frame 0 only (encoder/set.c). x265: UUID 2ca2de09-b517-47db-bb55-a4fe7fc2fc4e, "x265 (build %d) - %s:%s - H.265/HEVC codec - ... options: %s", gated by --info (default on), first AU only unless --repeat-headers. NVENC/libaom/SVT-AV1 write no identity string. MP4 ©too default "Lavf<ver>".
- AV1: T.35 metadata OBUs carry HDR10+ (0xB5/0x003C) and DV RPU (provider 0x003B).
- DV: dvcC layout verified (ffmpeg dovi_isom.c): u8 major, u8 minor, u16 [profile 7b | level 6b | rpu | el | bl], byte4 [compat 4b | md_compression 2b]. RPU header -> profile (vdr_rpu_profile 0+full range=5; 1+el_spatial_resampling+!disable_residual -> 7 (10-bit EL) or 4; else 8). MEL = NLQ all-zero offsets/deadzones, else FEL; None if use_prev_vdr_rpu. CM v2.9 levels [1,2,4,5,6,255], v4.0 [3,8,9,10,11,254]; full field widths in raw file. dovi_tool "MaxCLL/MaxFALL" = max L1 over all frames (whole file); first-N RPUs give profile, EL type, CM version, L5/L6/L9/L254, mastering display; frame/scene counts and trim sets need the whole file.
- HDR10+: hdr10plus metadata.rs validate() app id 4 v1; profile A (no tone mapping, targeted lum 0) vs B (bezier). T.35 constants still UNVERIFIED.
- Audio: DD+ Atmos two signals: addbsi eac3_extension_type_a + complexity_index (ffmpeg ac3_parser.c, sets AV_PROFILE_EAC3_DDP_ATMOS) and EMDF JOC payload 14 (MediaInfo, needs >=1/8 of frames). TrueHD Atmos: ffmpeg num_substreams==4 && substream_info>>7; MediaInfo MSB only (+16ch object count). DTS:X: XLL extradata sync 0x02000850 (X) / 0xF14000D0>>1 (IMAX); MediaInfo warns sync != dynamic objects. AAC ASC exact widths + 0x2b7/0x548 SBR/PS. AC-3 dialnorm/compr/dsurmod/dsurexmod (EX, PLIIz)/cmixlev/surmixlev/bsmod formulas from MediaInfo. bdinfo-rs-core is LGPL-2.1 (reference only, not a dep).
Gaps: DV level table, compat id meanings, HDR10+ T.35 constants, MKV A_EAC3 CodecPrivate, dec3 layout. matroska.org/videolan/gitlab/docs.rs 403 via proxy; static.crates.io tarballs and raw.githubusercontent work.
Decision: PIVOT for wave 3 to frame sampling/hashing and audio fingerprints (Rust-native), plus extra video SEI (captions, film grain, HLG alt transfer, 3D/interlace). Gaps go into wave 4.

## Wave 3 (07:02Z)
- A: Rust-native keyframe decode (rav1d, openh264?, pure-Rust HEVC?) + perceptual hashes; bitstream-level IDR hash as no-decode alternative
- B: audio fingerprints (rusty-chromaprint etc.), pure-Rust decoders for AC-3/E-AC-3/DTS, compressed-domain audio keys (AC-3 frame CRCs)
- C: extra video SEI/VUI: A/53 closed captions, film grain, alternative transfer (HLG), ambient viewing, interlace/pulldown, MVC/3D, HDR10+ T.35 constants
- Container thread (06:59Z, tested mkvmerge v82/ffmpeg 6.1.1): mkvmerge stats tags at END with Cues; Lavf MKVs have Tags (ENCODER, per-track DURATION) at FRONT; Lavf writes DVCC/DVVC BlockAdditionMapping; ffmpeg MP4 default moov at end. Prefer stats tag bitrate when a tail read exists.

### Wave 3 merge (07:06Z)
Raw: raw/w03-a-frames.md, w03-b-audio-fp.md, w03-c-sei-extra.md.
- Frames: NO pure-Rust HEVC pixel decoder exists. AV1: rav1d 1.1.0 (BSD-2, pure Rust) decodes via its dav1d C-ABI (unsafe, works). H.264: openh264 0.9.9 (BSD-2, vendored C, static) 8-bit only. HEVC: libde265 crate (C, LGPL). Parse-only: hevc_parser 0.6.12 (MIT, IDR/CRA + SEI headers), matroska-demuxer 0.8.1 (is_keyframe, codec_private). Hashing: image_hasher 3.1.1 / blockhash 1.0.0 (pure Rust); crop letterbox (use L5 or VUI), 32x32 luma, rank/median hash survives PQ vs SDR tone. Same-encode no-decode key: hash of first IDR NAL/OBU payload (survives remux). At 10-20 Mb/s a few MB may hold only one keyframe.
- Audio fp: rusty-chromaprint 0.3.0 (MIT, pure Rust, 275k dl): i16 any rate, u32 per 0.124 s, match_fingerprints. audiofp 0.4.5 (MIT; Wang/Panako/Haitsma, ~3.3 s min). Decoders: symphonia 0.6.1 (MPL-2.0: AAC/FLAC/MP3/Vorbis/ALAC/PCM, no AC-3/DTS/TrueHD/Opus); oxideav-ac3 0.0.11 (MIT, pure Rust AC-3/E-AC-3 decoder, young); truehd 0.7.2 (Apache, research); no Rust DTS decoder. Compressed-domain same-track key: AC-3/E-AC-3 crc2 sequence (last 2 bytes of each syncframe; skip silence). Budget: 2160p remux ~0.8 s audio per 5 MB, 1080p encode ~13 s -> fingerprints need mid-file article fetches.
- SEI extras (ffmpeg source verified): HDR10+ T.35 0xB5/0x003C/0x0001/app 4 (AV1 variant has no country byte); A/53 CC 0xB5/0x0031 "GA94" type 0x03, cc_count>0 = "CC" tag; payload types 1 pic_timing, 4, 5, 6, 19 film grain, 45 frame packing (3 SBS, 4 TB), 137, 144, 147 alt transfer (HLG compat), 148 ambient. Interlace: AVC frame_mbs_only/mb_adaptive (PAFF/MBAFF), HEVC field_seq_flag + pic_struct. 3D: MVC NAL 15/20, MV-HEVC nuh_layer_id>0. h264-reader 0.9.0 decodes T.35/unregistered user data; hevc_parser walks SEI headers only.
Decision: wave 4 = (A) mid-file random access to get keyframes and 10-30 s of audio cheaply (MKV cluster resync, Cues), (B) remaining DV/E-AC-3 gaps, (C) HEVC/AVC decode in pure Rust: oxideav family and other Rust codec projects, plus feasibility of an intra-only decoder.

## Wave 4 (07:07Z)
- A: mid-file MKV/MP4/TS random access from article ranges (cluster resync, Cues, SimpleBlock without headers), article budgets
- B: DV level table, compat ids, L11 values, P8.4/P10, MKV A_EAC3 CodecPrivate, dec3 layout
- C: pure-Rust video decode survey (oxideav, others), intra-only HEVC feasibility
- Container thread (07:05Z): MPLS STN HEVC attr dynamic_range_type/color_space, cr_flag, hdr_plus_flag; MPLS ext (3,5) static HDR; index.bdmv ext 3.1 disc 4K/HDR10+/DV flags; TS DOVI descriptor 0xB0 = dvcC layout. archive.ubuntu.com orig tarballs work for blocked C sources.
- Container thread (07:05Z): front SeekHead gives Cues/Tags offsets; Cues at end by default (mkvmerge, Lavf unless -cues_to_front). IDs verified in container-metadata/raw/w2a.md. Cluster/TS resync is ours.

### Wave 4 merge (07:12Z)
Raw: raw/w04-a-random-access.md, w04-b-dv-eac3-gaps.md, w04-c-rust-decoders.md.
- Random access: own ~150-line MKV walker (Cluster 1F43B675 resync, validate E7 Timestamp + A3/A0 child + known track number + next cluster at offset+size). matroska-demuxer has no from-offset API; mp4 0.14 has stss/stco helpers but no mfra; mpeg2ts has no resync. Cues via front SeekHead cost 1 tail article. Budget: audio is interleaved, so a 10 s audio window costs total bitrate: 2160p remux+TrueHD ~116 articles (~81 MB), 1080p+DD+ ~16. Plan: front 6 + tail 3-5 (Cues/Tags/moov) + 3 windows at 25/50/75% (W=12 for 1080p, W=40 for 2160p).
- Gaps: dec3 layout verified (MediaInfo File_Ac3::dec3): data_rate 13, num_ind_sub 3, per substream fscod 2/bsid 5/res 1/asvc 1/bsmod 3/acmod 3/lfeon 1/res 3/num_dep_sub 4/chan_loc 9; tail 7 reserved + flag_ec3_extension_type_joc 1 + joc_complexity_index 8. MKV A_AC3/EAC3/TRUEHD/DTS CodecPrivate usually empty (ffmpeg writes only if extradata) -> must scan frames. L11 = content_type u8, whitepoint nibble, reference_mode bit 4. DV level table + compat id meanings still UNVERIFIED (Dolby PDF not reachable). 8.1 vs 8.4 only from dvcC compat id, not RPU.
- Decoders (contradicts wave 3): rust_h265 0.4.0 (roticv, MIT/Apache, pure Rust, Main/Main10/RExt, 501k downloads, updated 2026-10-30), rusty_h265 0.6.2 (Apache, forbid-unsafe, claims 147/147 conformance, 2.2k dl), rust_h264 0.6.0 (roticv, 8/10-bit, 39k dl), oxideav-mpeg12video 0.0.13 (MIT). videotoolbox 0.21.2 on macOS as HW fallback. NihAV not reachable. All README claims, untested.
Decision: wave 5 goes EMPIRICAL. This research container has cargo 1.97 and ffmpeg (used only to make test vectors, never in Atlas). Build sample MKVs and test the Rust crates end to end.

## Wave 5 (07:13Z) empirical
- A: video probe: x264/x265 10-bit HDR10/AV1 samples -> Rust SPS/VUI/SEI extraction (scuffle-h265, h264-reader, hevc_parser) vs ffprobe ground truth
- B: decode test: rust_h265 / rusty_h265 / rust_h264 on IDR from the samples, PSNR vs ffmpeg decode, speed; pHash stability 1080p vs 2160p
- C: audio: AC-3/E-AC-3/TrueHD/DTS/FLAC samples -> header parse checks, crc2 remux stability MKV vs MP4, rusty-chromaprint across codecs
- 07:14Z: hit the 40 messages/hour cross-thread limit (lifts 07:56Z). Sharing now goes through linking-findings.md; direct messages only to the release-page and sibling extraction threads, batched.

### Wave 5 merge (07:20Z) empirical, test vectors made with ffmpeg in the research container only
Raw: raw/w05-a-video-probe.md, w05-b-decode-test.md, w05-c-audio-test.md. Code: code/w05{a,b,c}_main.rs.
- Video probe (first 1 MiB of MKV, Rust only): matroska-demuxer 0.8.1 + scuffle-h265 0.2.2 + h264-reader 0.9.0 + scuffle-av1 0.1.4, manual SEI walk. Profile, level, size, bit depth, chroma, BT.2020/PQ/matrix, fps, MDCV (SEI 137) and CLL (SEI 144) all MATCH ffprobe for x265 10-bit HDR10, x264, SVT-AV1. x264 UUID + "x264 - core" string confirmed in frame 0. x265 also copies its prefix SEIs into the hvcC SEI array, so CodecPrivate alone has the x265 string + HDR10 SEI. AVC VUI fps = time_scale/(2*num_units_in_tick).
- KEY FINDING: ffmpeg-muxed x265 MKV has NO Matroska Colour primaries/transfer/MaxCLL/MDCV (only range). HDR must come from SPS VUI + SEI 137/144, not the container.
- Decode: rust_h265 0.4.0 and rusty_h265 0.6.2 decode HEVC 8-bit and 10-bit 720p keyframes BIT-EXACT vs ffmpeg (~9-11 ms/frame, ~16 MB RSS); rust_h264 0.6.0 bit-exact for H.264 High 8-bit. Limits: rust_h265 no 4:2:2/mono/>12-bit; rusty_h265 no RExt; rust_h264 no 4:4:4/lossless. image_hasher on 32x32 luma: 8-bit vs 10-bit identical hash; 720p vs 1080p 0 bits; 720p vs 360p 0-3/64. Pitfall: ffmpeg -pix_fmt gray rescales range.
- Audio: own Rust header parsers match ffprobe for AC-3, E-AC-3, DTS core, FLAC (STREAMINFO MD5 == decoded PCM MD5). TrueHD only rate + 0xB752 signature so far. AC-3 crc2 sequence identical MKV vs MP4 (passthrough remux). rusty-chromaprint: FLAC vs AAC-LC 128k score 0.992 (140/140 aligned), 5.1 vs stereo 1.000, unrelated 0. symphonia 0.5.5 rejects 5.1 AAC.

### Atlas today (main a29ee52, src/probe/media.rs, 748 lines, container-only, no bitstream parsing)
- Has: MKV/MP4 tracks, width/height/display size, frame_rate from DefaultDuration, bit_depth + transfer + primaries + mastering flag from MKV Colour only, hdr name (HDR10/PQ/HLG), DV profile/level/EL flag from BlockAdditionMapping dvcC/dvvC (bit layout matches ffmpeg; no compat id), audio codec/channels/rate, atmos from track NAME only, commentary/forced/HI flags, chapters.
- Gap shown by wave 5: x265/ffmpeg MKVs without Colour will show no HDR in Atlas today. Fix = parse hvcC SPS VUI + SEI 137/144 (already in CodecPrivate).

## Wave 6 (07:21Z)
- A: Dolby Vision + HDR10+ test vectors (dovi_tool/hdr10plus_tool to make samples only), parse RPU from MKV frames with dolby_vision crate, compat id 8.1 vs 8.4, HDR10+ SEI detect
- B: audio finish: TrueHD major sync full fields vs ffprobe, AC-3 dialnorm/bsmod, oxideav-ac3 decode -> chromaprint AC-3 vs FLAC, E-AC-3 7.1 dependent substream
- C: mid-file walker prototype: 60 s MKV sliced into 700 KB articles, cluster resync at 50%, keyframe decode + pHash, audio frames, article count
- Archive thread (07:16Z): compressed prefix decode works in pure Rust: 7z LZMA2 via lzma-rust2 Lzma2Reader from offset 32 (64 MiB dict); RAR5 via rars codec::rar50::Unpack50Decoder (~1:1, loses up to 64 KiB at cut). So first-AU fields reachable on compressed 7z/RAR5 too, not mid-file.
- Container thread (07:17Z): Atlas probe on x265 HDR10 muxed by mkvmerge AND Lavf: no MasteringMetadata, no BitsPerChannel -> Atlas says 'PQ', bit_depth None. They're adding hvcC/av1C bit depth. Answer for them (send after 07:56): wave 5A saw x265 copy prefix SEIs (incl. 137/144 and x265 info) into hvcC SEI array; ffmpeg-muxed. Need to confirm mkvmerge keeps hvcC SEI array.

### Wave 6 merge (07:27Z)
Raw: raw/w06-a-dv-hdr10plus.md, w06-b-audio-finish.md, w06-c-midfile-walker.md. Code: code/w06{a,b,c}_main.rs.
- DV P8.1 (dovi_tool 2.3.4 generate + inject-rpu, CMv4.0): dolby_vision 3.4.0 parse_unspec62_nalu on the first keyframe gives profile 8, el None (no EL), CMv4.0, L2/L5/L6 (MaxCLL 1000/FALL 400)/L8/L9/L11/L254 all MATCH the generator. L1 min_pq 12 vs 62 injected: UNRESOLVED. Gotcha: cmv29_metadata is Some even on pure CMv4 streams; decide by block levels present. 1000 nits = PQ 3079 (12-bit). ffmpeg 6.1 writes NO dvcC in MKV/MP4 -> dvcC-only detection (Atlas today) misses ffmpeg-made DV files; the RPU NAL 62 is the robust signal.
- HDR10+: hdr10plus 2.1.5 Hdr10PlusMetadata::parse (from the 0xB5 byte): profile B, T.35 181/60/1, app 4/1, 1 window, targeted 400, maxscl, avg 300, 9 percentiles, bezier: all MATCH.
- TrueHD: full major sync parse (ffmpeg mlp_parse.c port, std only) matches ffprobe on 5.1 (channels, layout, rate). Major sync CRC-16 poly 0x2D passes 375/375 read BIG-endian (ffmpeg source reads LE: unresolved). Peak bitrate field formula gives 9597 kbps vs measured 3.5 Mb/s: unverified. ffmpeg can't encode TrueHD 7.1 or Atmos; real samples needed.
- Mid-file walker (std + rust_h264): 60 s 720p x264 + AC-3 MKV in 700 KB articles. Resync from 50% -> keyframe + 10 s audio with 22 articles total (16 beyond front), 36 ms CPU; via tail Cues 20 articles; via front Cues 19. Mid-file IDR decoded bit-exact (luma 921600/921600, pHash 0). Bug fixed: ffmpeg writes CRC-32 (0xBF) before Timestamp, so the cluster validator must skip CRC-32 and Void (0xEC) first. 0 false positives seen (decoy rate unmeasured).
Not done: AC-3 option tests, E-AC-3 7.1, AC-3 -> chromaprint via oxideav-ac3.

## Wave 7 (07:28Z)
- A: audio leftovers: AC-3 dialnorm/bsmod/mixlev/dsurmod vs encoder options, E-AC-3 7.1 dependent substream, oxideav-ac3 decode -> chromaprint vs FLAC, audio window size needed
- B: video robustness: mkvmerge (if installable) keeps hvcC SEI array? writes Colour/dvcC? 2160p 10-bit decode time+RAM in rust_h265 vs rusty_h265, open-GOP CRA start, interlaced H.264 (PAFF/MBAFF), MPEG-2 via oxideav-mpeg12video
- C: DV variants: P5, P8.4 HLG, P7 (MEL/FEL if dovi_tool can make it), L1 min_pq mismatch, how many RPUs needed for a stable summary
- Container thread (07:24Z): hvcC bytes 17/18 = bitDepth minus8. ffmpeg keeps x265 SEI 137/144 in hvcC prefix-SEI array; mkvmerge strips them, keeps only SEI 5 -> HDR10 confirm for mkvmerge files needs first IDR AU. Frame payloads byte-identical across remuxes -> first-IDR hash = their alt key. They're building DV files in wave 7 (overlap with my W6A/W7C: tell them ffmpeg writes no dvcC; pipeline in raw/w06-a).

### Wave 7 merge (07:38Z)
Raw: raw/w07-a-audio.md, w07-b-video-robustness.md, w07-c-dv-variants.md.
- Audio: oxideav-ac3 0.0.11 (pure Rust) decodes ffmpeg AC-3 448k cleanly; chromaprint AC-3 vs FLAC 0.989 full, 0.987 at 5 s, 0.99 at 10-20 s; +3.2 s offset recovered; unrelated coverage 0.05. AC-3 BSI matches encoder settings for dialnorm, cmixlev, surmixlev, mixlevel, roomtyp, dsurmod. Annex D (dsurexmod, adconvtyp) unconfirmed. E-AC-3 7.1 can't be made with ffmpeg 6.1 (dep substream parser coded, untested).
- Video: 2160p 10-bit HEVC keyframe bit-exact in both rust_h265 (~90 ms) and rusty_h265 (~87 ms), single thread, ~230-290 MB peak. Open-GOP CRA start: 72/72 frames bit-exact (RASL dropped). Interlaced MBAFF H.264 bit-exact with rust_h264 (PAFF untested). MPEG-2 via oxideav-mpeg12video: 76 dB PSNR (not bit-exact, fine for thumbnails). ffmpeg MKV remux kept hvcC SEI array (144, 137, 5); container note says mkvmerge strips 137/144 (keeps 5). MKV Colour MaxCLL read 9 vs 1000 in one ffmpeg file: UNRESOLVED anomaly, prefer SEI 144.
- DV: P5 and P8 from RPU (8.1 vs 8.4 identical in RPU; only dvcC compat id / VUI transfer 18 separates them). CM rule: non-empty CMv4.0 level set => v4.0 (CMv4 RPUs also carry L1/2/5/6). P7 MEL made via dovi_tool editor mode 1 -> crate el_type MEL; FEL needs a real sample. L1 min_pq 62->12 is the generator clamp (L1_MIN_PQ_MAX_VALUE), not a parse bug; source_min_pq is separate. Per-stream fields stable from RPU 1; L1/L5/L8 set change per shot. Use parse_unspec62_nalu (emulation-escaped bytes).
Remaining gaps: real-world samples (TrueHD Atmos, DD+ JOC, DTS:X, DV P7 FEL, 8.4), PQ-vs-SDR frame hash, M2TS/MP4 bitstream access, MKV header stripping.

## Wave 8 (07:39Z)
- A: real sample corpus: ffmpeg FATE samples (truehd atmos, eac3 joc, dts-hd/x), dovi_tool/dolby_vision test RPUs (P7 FEL, 8.4), run existing probes
- B: same-film robustness on real open content (Netflix Open Content HDR10/SDR, Tears of Steel, Big Buck Bunny): frame pHash across encodes/resolutions/PQ-vs-SDR, letterbox crop
- C: bitstream access in M2TS (BD remux/ISO) and MP4 (moov tail, fragmented), MKV ContentEncoding header stripping
- 07:28Z coordinator relayed the owner's example 'Linking Signals Map' (https://claude.ai/artifact/BEGjW1bU8S6Yqm3cnPMNpE): signals by what they prove (twin/alt/same film/uploader/tool) x read cost tier, Atlas coverage colour, tested-in-Rust stat tiles, inventory table. Final report should follow that shape: stream fields by cost tier (front/rpu1/mid/tail/whole) and Atlas status.
- 07:28Z the owner's better example: 'Media metadata for releases: ideas' doc (ranked ideas table with value/effort/bandwidth/CPU, per-area field tables, fetch plan, what probes find, storage+API exposure, cost per 1000 releases, order + open questions). Final report = Claude Doc in that shape, with tested numbers (corrects: ffmpeg writes no dvcC; MKV Colour often empty; 4K decode ~90 ms not 0.2-1 s; pure-Rust decode, no ffmpeg needed).
- W8A: fate-suite/samples.ffmpeg.org blocked by egress proxy (connect_rejected). BUT raw.githubusercontent.com/quietvoid/dovi_tool/main/assets/tests/{fel_orig,mel_orig}.bin and assets/hevc_tests/*.hevc return 200 (checked 07:45Z) -> real DV RPUs available for wave 9. Real audio (Atmos/DTS:X) samples: search GitHub-hosted test media.
- Container thread (07:29Z): mkvmerge v82 writes dvvC (P8.1: 01 00 10 1d 10) / dvcC (P5: 01 00 0a 1d 00) but no Colour for raw HEVC; ffmpeg no dvcC in MP4. Asks: is SPS VUI parsing mine? ANSWER (send after 07:56): yes, SPS VUI + SEI 137/144 + RPU are mine; tested in w05a (scuffle-h265).

### Wave 8 merge (07:32Z)
Raw: raw/w08-a-real-samples.md, w08-b-samefilm-hash.md (partial), w08-c-ts-mp4-strip.md. Code: code/w08c_main.rs.
- Real samples: samples.ffmpeg.org / fate-suite, Netflix Open Content, download.blender.org, archive.org all blocked by egress proxy. raw.githubusercontent.com works (dovi_tool assets/tests/*.bin return 200) and npm tarballs work (big-buck-bunny-1080p@0.0.6, CC-BY).
- TS/M2TS: std-only walker resyncs (0x47 x5 at 188 or 192+4), PAT/PMT, PES+PTS, Annex B; first decodable IRAP (x265 makes CRA, not IDR, after the first GOP: accept NAL 16..21 with VPS/SPS/PPS before) within 1-2 articles of resync. Extracted NAL bytes identical to ffmpeg dump.
- MP4: moov-at-end = 2 articles (tail + keyframe); faststart 2; fragmented: resync on 'moof' + sane size + next 'mdat', sample at moof + trun data_offset, 2 articles. Keyframe bytes identical across all three MP4 layouts.
- MKV ContentEncoding header stripping (algo 3): prepend ContentCompSettings to every frame of that track; stripped bytes vary by muxer, so the walker must apply it before looking for 0x0B77 or NAL length. zlib/bzip2/lzo ContentCompAlgo need decoders (flate2 for zlib). Untested (no mkvmerge here).
- Same-film hash: variants made from Big Buck Bunny (x265 720p10, letterbox 2.39, +brightness/gamma, PAL 25 fps), hash run not done.

## Wave 9 (07:33Z)
- A: real Dolby Vision RPUs from dovi_tool repo assets (FEL, MEL, P5, 8.4, CMv4) parsed with dolby_vision crate
- B: finish same-film hash on the BBB variants + PQ vs SDR (zscale) + separation margin
- C: real audio samples on GitHub/npm (TrueHD Atmos, E-AC-3 JOC, DTS-HD MA / DTS:X, E-AC-3 7.1) -> run parsers

### Wave 9 merge (07:38Z)
Raw: raw/w09-a-real-dv.md, w09-b-samefilm-hash.md, w09-c-real-audio.md.
- Real DV RPUs (27 dovi_tool test fixtures from raw.githubusercontent): dolby_vision 3.4.0 FEL/MEL right on all 11 P7 fixtures; profiles 4/5/7/8 right; CM v4.0 rule (any of L3/8/9/10/11/254) right on all 27; dovi_tool's own assertions pass except level253 (crates.io 3.4.0 lacks Level253 that dovi_tool main has -> whole RPU errors; treat Err as partial or pin git). L6 often 4000/50 with CLL/FALL 0 -> store as unknown, not 0. el_type None on use_prev_vdr_rpu RPUs -> carry forward. Converted P7->P8.1 keeps CMv2.9 blocks, drops NLQ.
- Same-film hash (BBB, rust_h264/rust_h265 decode, luma == ffmpeg): pHash 64-bit on letterbox-crop + histogram-equalised luma: same frame across x265 720p10, letterbox 2.39, brightness/gamma, 25 fps retime, PQ transfer: max 4 bits; different scene of same film: min 24. Threshold <= 10/64, 3 frames with 2-of-3 vote (one within-film near-flat pair collided). Crop is mandatory (letterbox 19 bits -> 0). Real HDR grade vs SDR grade still untested.
- Real audio: truehdd/truehdd repo (Apache-2.0) has real MLP/TrueHD Atmos-form fixtures (fba_atmos_obj/cbi/dimtrim.mlp, 7.1); ffprobe reports mlp 8ch, no Atmos label. Our major-sync CRC fails on all real syncs (CRC range/byte order still unknown); Atmos rule not settled. No reachable real E-AC-3 JOC / DTS-HD / DTS:X files.

## Wave 10 (07:39Z) final
- A: Atlas integration: map catalog to src/probe/media.rs, struct/serde changes, dependency list with licences, build time + binary size of the crate set, CPU per release
- B: integrated probe prototype: one Rust binary (front parse + walker + parsers) emitting JSON with catalog keys, run on all generated vectors
- C: TrueHD: read truehd 0.7.2 / truehdd source for the major sync CRC range and Atmos (16-ch presentation) detection; test on truehdd fixtures

### Wave 10 merge (07:52Z) final
Raw: raw/w10-a-atlas-integration.md, w10-b-streamprobe.md, w10-c-truehd.md. Code: code/streamprobe/, code/w10c_main.rs.
- Atlas integration: ProbeStep feeds media::parse first article + up to 6 front parts; ProbeInfo JSON in release_probe.info, all serde(default) -> no migration; bump config_stamp 1 -> 2. All 12 crates: 48 s clean build, 85 crates, no -sys/cc, ~1.2 MB stripped; licences MIT/Apache/BSD/Zlib/Unicode + MPL-2.0 (symphonia). Struct draft in w10-a.
- streamprobe prototype: all phase-1 fields to JSON, matches ffprobe/injected on 12 MKVs; video fields identical at 256 KiB front. matroska-demuxer's open follows SeekHead to EOF and fails on partial files -> keep Atlas's own EBML walker. ffmpeg-made DV has no dvcC (so no compat id there).
- TrueHD: CRC rule solved (truehdd Crc16 poly 0x2D, range [sync, sync+C) with C=26 or 28+2*(byte26>>4) for FBA ext, BE stored), 100% on 52 files (12 truehdd fixtures incl. 3 Atmos + 39 real encoder files). Atmos = FBA && substream_info bit 7; nsub != 4 = inconsistency flag. All rules agree on all files. No object count in major sync.
Final report doc: https://claude.ai/artifact/ByUTRbbX7sHy2H78JK9iVo . Catalog: catalog.yaml (updated with wave 9-10).
- Container thread (07:45Z): MKV HDR10+ container flag = BlockAdditionMapping BlockAddIDType 4 (T.35), per-frame BlockAdditional with 0xB5/0x003C (ffmpeg matroskadec). So HDR10+ in MKV may sit in BlockAdditional, not the HEVC SEI: parser must check both. Atmos at container level only MP4 dec3.
- Archive thread final (07:49Z): https://claude.ai/artifact/NSkmWVebA67M95ps5NLqP1 ; RAR5, RAR 2.9 LZ/PPMd, 7z LZMA/LZMA2+BCJ/Delta prefix decode works forward only; mid-file needs store mode.
- Container thread final (07:51Z): https://claude.ai/artifact/Wb8pQ6mzroCGo38pZvLcVK ; measured first keyframe 576 KB (1080p x264 12M) / 970 KB (2160p HEVC 40M); 10 s video 15.7 MB / 51.7 MB (container raw/w10b.md).
