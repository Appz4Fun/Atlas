# w6c: codec configuration records and labelling rules (HEVC/AV1/VP9/AVC, DV, HDR)

Scope: byte layouts of codec configuration boxes (hvcC, avcC, av1C, vpcC, dvcC/dvvC), what each records about bit depth and HDR, and a labelling decision table.

Legend: **[SRC]** verified in FFmpeg source fetched this session (raw.githubusercontent.com/FFmpeg/FFmpeg/master). **[TEST]** verified by a local mux or encode (ffmpeg 6.1.1, mkvmerge v82.0, libx265/libaom/libvpx/libx264 via ffmpeg). **[MEM]** from memory, not checked this session.

Test scripts: /tmp/w6c/hvcc_dump.py (run with python3 -I). Test artifacts are in /tmp/w6c.

## 1. Bit tables

### hvcC (ISO/IEC 14496-15, HEVCDecoderConfigurationRecord) [SRC: libavformat/hevc.c write_configuration_record / hvcc_write]
| Byte | Field |
|---|---|
| 0 | configurationVersion (1) |
| 1 | general_profile_space(2) / general_tier_flag(1) / general_profile_idc(5) |
| 2-5 | general_profile_compatibility_flags (32) |
| 6-11 | general_constraint_indicator_flags (48) |
| 12 | general_level_idc |
| 13-14 | reserved(4) / min_spatial_segmentation_idc(12) |
| 15 | reserved(6) / parallelismType(2) |
| 16 | reserved(6) / chromaFormat(2) |
| 17 | reserved(5) / bitDepthLumaMinus8(3) |
| 18 | reserved(5) / bitDepthChromaMinus8(3) |
| 19-20 | avgFrameRate (16) |
| 21 | constantFrameRate(2) / numTemporalLayers(3) / temporalIdNested(1) / lengthSizeMinusOne(2) |
| 22 | numOfArrays |
| 23+ | arrays: array_completeness(1) / reserved(1) / NAL_unit_type(6); numNalus(16); per NAL: nalUnitLength(16) + bytes |

FFmpeg keeps only VPS(32), SPS(33), PPS(34), SEI prefix(39) and SEI suffix(40) arrays (hvcc_parse_nal_unit). Bit depth is always present here, so it does not depend on any optional container element.

### avcC (ISO/IEC 14496-15 AVCDecoderConfigurationRecord) [TEST]
| Byte | Field |
|---|---|
| 0 | configurationVersion (1) |
| 1 | AVCProfileIndication |
| 2 | profile_compatibility |
| 3 | AVCLevelIndication |
| 4 | reserved(6)=111111 / lengthSizeMinusOne(2) |
| 5 | reserved(3)=111 / numOfSPS(5) |
| then | SPS: length(16) + data; numOfPPS(8); PPS: length(16) + data |
| if profile in {100,110,122,144} | reserved(6)/chroma_format(2); reserved(5)/bit_depth_luma_minus8(3); reserved(5)/bit_depth_chroma_minus8(3); numOfSPSExt(8); SPSExt |

Verified on a 10-bit High 10 test file (profile 0x6e): the extension is `fd fa fa 00`, which decodes to chroma_format=1, luma=10-bit, chroma=10-bit, 0 SPSExt.

### av1C (AV1-ISOBMFF) [SRC: libavformat/av1.c ff_isom_write_av1c; TEST]
| Byte | Field |
|---|---|
| 0 | marker(1)=1 / version(7)=1 |
| 1 | seq_profile(3) / seq_level_idx_0(5) |
| 2 | seq_tier_0(1) / high_bitdepth(1) / twelve_bit(1) / monochrome(1) / chroma_subsampling_x(1) / chroma_subsampling_y(1) / chroma_sample_position(2) |
| 3 | reserved(3) / initial_presentation_delay_present(1) / initial_presentation_delay_minus_one(4) or reserved(4) |
| 4+ | configOBUs (sequence header OBU, optional metadata OBUs) |

Bit depth: high_bitdepth=0 means 8-bit; high_bitdepth=1 with twelve_bit=0 means 10-bit; twelve_bit=1 means 12-bit. Test file: `81 00 4c 00 | 0a 0b 00 00 00 04 3c ff bc da f9 40 40`. Byte2 0x4c gives high_bitdepth=1, 4:2:0. The configOBU is a sequence header OBU (type 1).

### vpcC (VP Codec ISO Media File Format) [SRC: libavformat/vpcc.c ff_isom_write_vpcc, movenc.c; TEST]
| Byte | Field |
|---|---|
| 0 | version(8)=1 |
| 1-3 | flags(24)=0 |
| 4 | profile |
| 5 | level (10x the VP9 level, e.g. 20 = 2.0) |
| 6 | bitDepth(4) / chromaSubsampling(3) / videoFullRangeFlag(1) |
| 7 | colourPrimaries |
| 8 | transferCharacteristics |
| 9 | matrixCoefficients |
| 10-11 | codecInitializationDataSize(16)=0 for VP9 |

Test file (10-bit VP9): `01 000000 02 14 a2 02 02 02 0000` decodes to profile 2, level 2.0, bitDepth 10, chroma 1, full range 0.

### dvcC / dvvC (Dolby Vision, DOVIDecoderConfigurationRecord) [SRC: libavformat/dovi_isom.c ff_isom_put_dvcc_dvvc; movenc.c mov_write_dvcc_dvvc_tag]
Payload is 24 bytes; the box is 32 bytes with the header. Fields in order:
dv_version_major(8), dv_version_minor(8), dv_profile(7), dv_level(6), rpu_present_flag(1), el_present_flag(1), bl_present_flag(1), dv_bl_signal_compatibility_id(4), dv_md_compression(2), reserved(26), then 128 reserved bits.
- movenc.c box choice: profile > 10 gives `dvwC`, profile > 7 gives `dvvC`, otherwise `dvcC`.
- movenc.c:3087 warns "Not writing 'dvcC'/'dvvC' box. Requires -strict unofficial", so ffmpeg writes these boxes only under that flag. The sandbox ffmpeg (6.1.1) was not tested with DV input (no DV source available).

### Matroska
- CodecID V_MPEGH/ISO/HEVC: CodecPrivate is the hvcC above [TEST: mkvmerge and ffmpeg both write this codec ID with hvcC].
- Colour, MasteringMetadata, and BitsPerChannel: the Matroska Colour element sits under the video track; its BitsPerChannel is optional [MEM]. I did not parse the EBML directly. ffprobe reports bits_per_raw_sample=N/A for every test file, which matches the symptom Atlas hit. Bit depth should come from hvcC, not from the Colour element.

## 2. Test results

Source: x265 via ffmpeg libx265 (ffmpeg 6.1.1), 320x240, 1 s, yuv420p10le, `hdr10=1:repeat-headers=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400`.

[TEST] bitDepthLumaMinus8 = 2 and bitDepthChromaMinus8 = 2 in every hvcC (10-bit confirmed). chromaFormat = 1. Profile = Main 10 (profile_idc 2).

[TEST] SEI arrays in hvcC (completeness flag 0 for the ffmpeg mux, 1 for mkvmerge):
- ffmpeg-muxed MKV and MP4 (from x265 extradata): SEI prefix array has 3 NALs: SEI 144 (content light, 4-byte payload), SEI 137 (mastering display, 24-byte payload), plus SEI 5 (x265 user data). So both static HDR SEIs are in the first-AU extradata.
- mkvmerge-muxed MKV: SEI prefix array has only SEI 5. mkvmerge drops 137/144 from CodecPrivate.
- Without repeat-headers and without HDR SEI (pq_nosei): hvcC has VPS/SPS/PPS only, no SEI array.
- Conclusion: static metadata in hvcC is not reliable. A parser must scan the first IDR access unit for SEI 137/144 (and for HDR10+ and DV RPU, see section 3).

[TEST] ffprobe on the mkvmerge file still reports mastering display and content light as frame side data. That comes from in-band SEI in the bitstream.

[TEST] Transfer and primaries (smpte2084 / bt2020) appear in all four test files (ffmpeg mp4, ffmpeg mkv, mkvmerge mkv, and the PQ-only file). Those values come from the Colour element or the VUI; I did not separate the two.

[TEST] avcC (10-bit H.264, libx264), av1C (libaom 10-bit), vpcC (libvpx-vp9 10-bit): all three gave the expected bit depth in their config boxes (table above).

Not tested: DV RPU/dvcC (no DV source available), HDR10+ (no test source), and ffmpeg's `-strict unofficial` DV path.

## 3. Labelling rules and decision table

Precedence, highest first. Check in this order and stop at the first match:

| # | Condition (source of evidence) | Label |
|---|---|---|
| 1 | dvcC/dvvC/dvwC present in the sample entry (MP4), or BlockAdditionMapping with DV ID (MKV) [MEM for MKV] | Dolby Vision, with profile, level, and compatibility from the record |
| 2 | DV profile 5 (bl_signal_compatibility_id = 0) | "Dolby Vision Profile 5" (no HDR10/SDR fallback) [MEM] |
| 3 | DV profile 8.1 (compat id 1) | "Dolby Vision Profile 8.1, HDR10 compatible" [MEM] |
| 4 | DV profile 8.2 (compat id 2) | "Dolby Vision Profile 8.2, SDR compatible" [MEM] |
| 5 | DV profile 8.4 (compat id 4) | "Dolby Vision Profile 8.4, HLG compatible" [MEM] |
| 6 | DV profile 7 (dual-layer BL+EL+RPU) | "Dolby Vision Profile 7" (the BL is HDR10-compatible) [MEM] |
| 7 | HDR10+ dynamic metadata: itu-t t35 SEI, country 0xB5, provider 0x003C, oriented code 0x0001 (ST 2094-40) in the bitstream | Add "HDR10+" on top of the HDR10/PQ label [MEM for the codes] |
| 8 | Transfer 16 (PQ), with SEI 137 (ST 2086) and/or SEI 144 (CLL), in hvcC or the first IDR | "HDR10" (or "HDR10, SMPTE ST 2086" when 137 is present) [TEST for the SEIs] |
| 9 | Transfer 16 (PQ), no 137/144 found | "PQ" / "HDR (PQ), no static metadata" |
| 10 | Transfer 18 (HLG) | "HLG" |
| 11 | Transfer 1 (BT.709) or unspecified, primaries 1 | "SDR BT.709" |

Notes:
- Read bit depth from hvcC bitDepthLuma/Chroma (always present), av1C high_bitdepth/twelve_bit, vpcC bitDepth, or avcC bit_depth_luma (profile 100+). Do not rely on the Matroska Colour element.
- Colour primaries, transfer, and matrix can come from the VUI in the SPS; prefer the VUI and fall back to the container.
- mediainfo phrasing [MEM, not run here]: "HDR10" with "SMPTE ST 2086" for SEI 137, and for DV: "Dolby Vision, Version 1.0, Profile 8.1, dvhe.08.06, BL+RPU, HDR10 compatible". ffprobe phrasing: "Mastering display metadata" and "Content light level metadata" as side data [TEST].
- The dvh1 versus dvhe fourcc split (parameter sets out-of-band versus in-band) is [MEM].

## 4. Old MKVs with DV and no BlockAdditionMapping [MEM, not verified]
- Likely signalled only as CodecID V_MPEGH/ISO/HEVC with a plain hvcC; the DV RPU stays in-band (NAL 62 / UNSPEC62). Parsers need to scan for RPU NALs.
- BlockAdditionMapping with DV config (BlockAddIDType dvcC/dvvC) is what newer mkvmerge writes. I could not confirm the mkvmerge version that added this; I did not check the changelog (mkvmerge here is v82.0).
- MakeMKV output is likely in-band RPU with no container DV box [MEM].

## 5. Limits
- Source is FFmpeg master (fetched this session); the local ffmpeg is 6.1.1, so newer behaviour may differ.
- Tested only what the local tools produce. No DV, HDR10+, or HLG sample was encoded.
- Items marked [MEM] need a check before Atlas depends on them.
