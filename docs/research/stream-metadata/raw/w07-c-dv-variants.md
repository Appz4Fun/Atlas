# W07-C: DV variants (P5, 8.1 CMv2.9/v4.0, 8.4, P7 MEL), L1 min_pq, stability (Atlas)
Tools: dovi_tool 2.3.4 (generate, info, editor), dolby_vision 3.4.0. Probe: docs/research/stream-metadata/code/w07c_main.rs (reads dovi_tool RPU.bin, one RPU per 00 00 00 01 chunk, parse_unspec62_nalu). Work dir: scratchpad/w7c.94ng. Samples: 48 frames, 1 shot unless noted. Test vectors only, no real P7 sample.

## 1. Variants (dovi_tool generate, same L1/L2/L5/L6/L9/L11/L254 JSON)
| variant | info "Profile" | crate dovi_profile | el_type | CMv4 set (cmv40) | CMv2.9 set (cmv29) |
|---|---|---|---|---|---|
| P5 (cm V40) | 5 | 5 | None | L3,9,11,254 | L1,2,5,6 |
| 8.4 HLG (cm V40) | 8 | 8 | None | L3,9,11,254 | L1,2,5,6 |
| 8.1 (cm V40) | 8 | 8 | None | L3,9,11,254 | L1,2,5,6 |
| 8.1 (cm V29) | 8 | 8 | None | empty | L1,2,5,6 |
| P7 MEL (editor mode 1 on 8.1 V40) | 7 (MEL) | 7 | MEL | L3,9,11,254 | L1,2,5,6 |

- dovi_tool info labels: "DM version: 2 (CM v4.0)" or "1 (CM v2.9)". 8.4 is labelled "Profile: 8" (no 8.4 label). 8.1 and 8.4 RPUs are not distinguishable by the crate or by info. 8.4 RPU is 19919 B vs 8639 B for 8.1 V40 (48 fr), same probed top-level fields; mapping size differs, not inspected further. 8.1 vs 8.4 comes only from the dvcC/transfer (w04 finding stands).
- CM detection: CMv4.0 RPUs ALWAYS carry a cmv29 set too (L1,2,5,6), so "cmv29 is Some" means nothing. Rule: cmv40 block set non-empty -> CMv4.0; cmv40 empty and cmv29 non-empty -> CMv2.9. Levels: CMv2.9 set is L1,2,5,6 (L8/9/11/254 dropped by generator); CMv4 adds L3,9,11,254 (+L8 when present).
- L2 target_max_pq 2081 (100 nit) in all. L6 1000/50/1000/400, L9 0, L11 ct1 wp0 ref true, L254 dm0 in all.

## 2. P7
- `dovi_tool generate` supports only 5, 8.1, 8.4. `dovi_tool convert` takes HEVC, not RPU. `dovi_tool editor -j {"mode":1}` (MEL conversion) on an 8.1 RPU yields profile 7 MEL: header vdr_bit_depth_minus8=4 and el_spatial_resampling_filter_flag=1, disable_residual_flag=0, nlq_method LinearDeadzone. Crate reports el_type MEL.
- FEL: NOT produced. Crate el_type = MEL iff rpu_data_nlq.rs is_mel() (zero dead-zone slope/threshold, one VDR in max, etc.), else FEL. Making FEL needs non-zero NLQ params; no dovi_tool option found. Needs a real P7 FEL sample or a crate-level edit (UNVERIFIED path).

## 3. L1 min_pq 62 vs 12: RESOLVED (generator clamp, not a parse bug)
- generate JSON L1 min_pq 62 -> RPU bitstream 12 (confirmed for all variants). Cause: dolby_vision generate.rs fixup_l1() calls clamp_values_cm_version(), which clamps min_pq to L1_MIN_PQ_MAX_VALUE = 12 (extension_metadata/blocks/level1.rs). Applied to default, shot and frame_edit blocks.
- Source-level min is separate: VdrDmData.source_min_pq = 62 (12-bit PQ). dovi_tool info's "0.0050 nits" comes from that.
- Atlas: do not expect L1 min to equal source_min_pq. Treat L1 min as 0..12 on generated samples. Real encoders may write other values (unverified).

## 4. Stability (200 frames, 3 shots: 0-69, 70-129, 130-199; L1 max/avg 3079/2048, 2081/1500, 2875/1600; L5 top/bottom 0, 60, 0; shot 3 adds L8 index 2)
| field | value changes (1-based RPU) | notes |
|---|---|---|
| dovi_profile, el_type, CM version, L2 set, L6, L9, L11, L254, source_min/max | never | constant from RPU 1 |
| L1 min/max/avg | 71, 131 | 3 distinct values |
| L5 offsets | 71, 131 | 2 distinct |
| L8 target set | 131 | {1} -> {1,2} |
| cmv40 level set | 131 | L8 block count rises |
- Answer: 1 RPU for stream-level fields. Per-shot fields (L1, L5, L8) must come from shot-boundary scans. The L2/L8 target union needs the whole file (or all shot starts).

## Gotchas
- dovi_tool RPU.bin is escaped; use parse_unspec62_nalu on each chunk. parse_rpu on raw chunks fails with "dm_alignment_zero_bit != 0".
- Unclamped L1 min never reaches a generated RPU (see 3).
- Generator shots: {"start","duration","metadata_blocks","frame_edits"}; default_metadata_blocks as fallback.
