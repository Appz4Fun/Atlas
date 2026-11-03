# W06-A: DV profile 8.1 + HDR10+ sample generation and Rust parse check (Atlas)
Tools: dovi_tool 2.3.4 and hdr10plus_tool 1.7.2 (GitHub release binaries, only for test vectors). Probe: dolby_vision 3.4.0, hdr10plus 2.1.5, matroska-demuxer 0.8.1. Source: docs/research/stream-metadata/code/w06a_main.rs

## Pipeline (what worked)
- base: ffmpeg libx265 640x360 2 s (48 fr) 10-bit, BT.2020/PQ, master-display + max-cll set, Annex B.
- DV: dovi_tool generate (cm_version V40, profile "8.1", length 48, L1/L2/L5/L6/L8/L9/L11/L254 blocks) -> RPU.bin; inject-rpu -> inj81.hevc.
- HDR10+: hdr10plus_tool inject -j hdr10p.json -> hdr10p.hevc (SEI prefix, 48 frames).
- MKV: raw .hevc -> MKV with `ffmpeg -c copy` FAILS ("Can't write packet with unknown timestamp"). Working path: raw -> MP4 -> MKV (`-c copy -strict unofficial`).
- dvcC: ffmpeg 6.1 wrote NO dvcC (no BlockAdditionMapping in MKV, no dvcC box in MP4) for either sample. The DV sample has no config record. Probe prints "dvcC: absent". Byte decode path is coded but untested on real data.
- Not done: profile 8.4 (HLG) and profile 5 variants, dovi_tool dual-track/FEL samples.

## Field table: DV profile 8.1 (v81.mkv, first keyframe, UNSPEC62 NAL)
| field | injected (generator JSON) | Rust parsed | crate fn / field | match |
|---|---|---|---|---|
| dovi_profile | 8.1 | 8 | DoviRpu.dovi_profile | yes |
| el_type | none (BL only) | None | DoviRpu.el_type | yes |
| get_dovi_profile | 8 | 8 | rpu.header.get_dovi_profile() | yes |
| CMv4.0 blocks | V40 | Some | vdr.cmv40_metadata | yes |
| source_min_pq / max_pq | 62 / 3079 | 62 / 3079 | VdrDmData.source_{min,max}_pq | yes |
| L1 min/max/avg pq | 62 / 3079 / 2048 | 12 / 3079 / 2048 | ExtMetadataBlockLevel1 | NO (min) |
| L2 target_max_pq | 2081 (100 nits) | 2081 | Level2.target_max_pq | yes |
| L5 active area L/R/T/B | 0/0/0/0 | 0/0/0/0 | Level5.active_area_*_offset | yes |
| L6 max/min mastering | 1000 nit / 50 (1e-4 nit) | 1000 / 50 | Level6.max/min_display_mastering_luminance | yes |
| L6 MaxCLL / MaxFALL | 1000 / 400 | 1000 / 400 | Level6.max_content_light_level / max_frame_average_light_level | yes |
| L8 target_display_index | 1 | 1 | Level8.target_display_index | yes |
| L9 source_primary_index | 0 | 0 | Level9.source_primary_index | yes |
| L11 content_type / reference_mode | 1 / true | 1 / true | Level11.content_type, reference_mode_flag | yes |
| L254 dm_mode | 0 | 0 | Level254.dm_mode | yes |
| block counts | L1..L11, L254 = 1 each, L255 = 0 | same | vdr.level_blocks_iter(n) | yes |
| CMv2.9 block set | none | cmv29_metadata = Some (empty default) | VdrDmData.cmv29_metadata | NO: test block counts, not Option |

Notes:
- L1 min_pq: dovi_tool info reports min 0.0050 nits (consistent with source_min_pq 62), but the crate returns 12 for the L1 block. Not resolved in the time box. Either dovi_tool rewrites the L1 min or the crate reads it differently. Treat L1 min as unverified.
- The first generator run wrote max PQ 3696 (about 4000 nits). Fixed to 3079 (1000 nit PQ). Probe output above is from the corrected run.
- dovi_tool info (whole stream): Frames 48, Profile 8, DM v2 (CMv4.0), MaxCLL 1000.60, MaxFALL 92.36 (L1-derived), L6 1000/400.

## Field table: HDR10+ (hdr10p.mkv, first keyframe, prefix SEI payload 4, T.35 B5/003C/0001)
| field | injected (hdr10plus JSON) | Rust parsed | crate fn / field | match |
|---|---|---|---|---|
| profile | B (tone_map=1, bezier anchors) | B | Hdr10PlusMetadata.profile (from parse) | yes |
| T.35 country/provider/oriented | 181 / 60 / 1 (0xB5/0x003C/0x0001) | 181 / 60 / 1 | itu_t_t35_* | yes |
| app id / version | 4 / 1 | 4 / 1 | application_identifier / _version | yes |
| num_windows | 1 | 1 | num_windows | yes |
| targeted max lum | 400 | 400 | targeted_system_display_maximum_luminance | yes |
| maxscl | [800,750,700] | [800,750,700] | maxscl | yes |
| average_maxrgb | 300 | 300 | average_maxrgb | yes |
| distribution percentiles | 9 (1..99) | 9, first 1/10, last 99/780 | distribution_maxrgb | yes |
| bezier knee x/y, anchors | 100/200/3 | 100/200/3 | bezier_curve | yes |
| tone_mapping_flag | true | true | tone_mapping_flag | yes |

Call: `hdr10plus::metadata::Hdr10PlusMetadata::parse(&payload)` where payload starts at the country byte (0xB5), after the SEI payload type and size, with emulation-prevention bytes removed. The app-id check is manual in the probe.

## hdr10plus_tool JSON (needed keys, observed)
Top level: JSONInfo.HDR10plusProfile, JSONInfo.Version, ToolInfo.Tool, ToolInfo.Version, SceneInfo[] (SceneId, SequenceFrameIndex, SceneFrameIndex, NumberOfWindows, TargetedSystemDisplayMaximumLuminance, LuminanceParameters{AverageRGB, LuminanceDistributions{DistributionIndex, DistributionValues}, MaxScl}, BezierCurveData{KneePointX, KneePointY, NumberOfAnchors, Anchors}, ToneMappingFlag, ...), SceneInfoSummary.

## Other
- Probe source: docs/research/stream-metadata/code/w06a_main.rs. Run output: $W/v81_probe.txt, $W/hdr10p_probe.txt (W = the w6a scratch dir).
- Atlas implication: DV 8.1 is only identifiable by the RPU alone (profile 8, el none) plus the dvcC box. The RPU does not carry 8.1 vs 8.4, so dvcC or the transfer characteristic is needed.
