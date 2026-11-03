# W09-a: dolby_vision 3.4.0 on dovi_tool test RPUs (2026-11-02)

Corpus: 27 RPU `.bin` files from dovi_tool `src/tests/rpu.rs` (assets/tests/ on main, 116-492 bytes each, 1 RPU each except p8_001 = 3). No `.hevc` assets are referenced by dovi_tool tests. cmv40/l11/hdr10plus .bin paths are 404 and were skipped. Caveat: these are small test fixtures, not full real encodes, and several are dovi_tool conversions (fel_to_*, mel_to_*, profile8_from_profile5). Shot-to-shot behaviour is untested.
Probe: code/w09a_main.rs (split on 00 00 00 01, parse_unspec62_nalu per RPU, parse_rpu_file cross-check). Output recorded per file.

## Per-file (first RPU)
| file | P | el | NLQ | CM | levels | L1 min/max/avg | L2 | L5 LRTB | L6 max/min/cll/fall | L8 | L9 | L11 | L254 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| fel_rpu / fel_orig | 7 | FEL | off512 vin 2^20 dz2048 | v2.9 | 1,2,4,5,6 | 0/2873/1060 | 2081 | 0/0/276/277 | 1000/1/1712/175 | - | - | - | - |
| mel_rpu | 7 | MEL | zero | v2.9 | 1,2,4,5,6 | 0/2081/1340 | 2081 | 0/0/0/0 | 1000/1/745/47 | - | - | - | - |
| mel_orig | 7 | MEL | zero | v2.9 | 1,4,6 | 0/2546/1566 | - | - | 1000/1/0/0 | - | - | - | - |
| fel_to_mel | 7 | MEL | zero | v2.9 | 1,2,4,5,6 | 0/2873/1060 | 2081 | 0/0/276/277 | 1000/1/1712/175 | - | - | - | - |
| mel_variable_l8_length13 | 7 | MEL | zero | v4.0 | 1,2,3,4,5,6,8,9,254 | 0/3100/2048 | 2081,2851,3079 | 0/0/0/0 | 1000/1/0/0 | 1 | 0 | - | dm0 |
| data_before_crc32 | 7 | FEL | off512 | v4.0 | 1-6,9,254 | 0/2081/1229 | 2081,2851,3079 | 0/0/0/0 | 1000/1/650/621 | - | 2 | - | dm0 |
| eof_rpu | 7 | FEL | off512 | v2.9 | 1,2,4,5,6 | 0/2873/1060 | 2081 | 0/0/276/277 | 1000/1/1712/175 | - | - | - | - |
| fix_se_write | 7 | FEL | off512 | v2.9 | 1,2,4,6 | 0/2941/1088 | 2081 | - | 4000/50/0/0 | - | - | - | - |
| poly_coef_int_logic | 7 | FEL | off225/517/476 | v2.9 | 1,2,4,5,6 | 332/2540/1561 | 2081 | 0/0/280/280 | 4000/50/5036/1068 | - | - | - | - |
| trailing_bytes_rpu | 7 | FEL | off512 | v2.9 | 1,2,4,5,6 | 12/2452/887 | 2081 | 0/0/42/42 | 4000/50/1069/733 | - | - | - | - |
| profile4 | 4 | FEL | off222/456/474 | v2.9 | 1,2,4 | 0/4095/1024 | 2081 | - | - | - | - | - | - |
| fel_to_81 | 8 | None | none | v2.9 | 1,2,4,5,6 | 0/2873/1060 | 2081 | 0/0/276/277 | 1000/1/1712/175 | - | - | - | - |
| mel_to_81 | 8 | None | none | v2.9 | 1,4,6 | 0/2546/1566 | - | - | 1000/1/0/0 | - | - | - | - |
| profile8 / profile84 | 8 | None | none | v2.9 | 1,2,4,5,6 | 2/3383/819 | 2081 | 0/0/0/0 | 4000/50/0/0 | - | - | - | - |
| profile8_from_profile5-02 | 8 | None | none | v2.9 | 1,2,4,5,6 | 0/2081/819 | 2081 | 0/0/119/119 | 1000/1/943/0 | - | - | - | - |
| unordered_l8_blocks | 8 | None | none | v4.0 | 1,3,4,8,9,10,11,254 | 0/3079/2209 | - | - | - | 20,255 | 0 | ct4 | dm0 |
| p8_001_end_crc32 (3 RPUs) | 8 | None | none | v2.9 | 1,2,4,5,6 | 0/2481/1288 | 2081 | 0/0/275/276 | 1000/1/189/89 | - | - | - | - |
| profile5 | 5 | None | none | v2.9 | 1,2,4,5,6 | 2/3383/819 | 2081 | 0/0/0/0 | 4000/50/0/0 | - | - | - | - |
| profile5-02 | 5 | None | none | v2.9 | 1,2,4,5,6 | 0/2081/819 | 2081 | 0/0/119/119 | 1000/1/943/0 | - | - | - | - |
| source_p5_to_p8_001 | 5 | None | none | v2.9 | 1,2,4,5,6 | 0/3015/1153 | 2081,2851 | 0/0/69/70 | 4000/50/0/0 | - | - | - | - |
| profile20_apple | 5 | None | none | v4.0 | 1,3,8,9,10,11,254 | 12/2945/1911 | - | - | - | 255 | 2 | ct1 wp0 | dm0 |
| empty_dmv1_blocks | 5 | None | no mapping | none | none | - | - | - | - | - | - | - | - |
| level253-ext-metadata | 5 | - | - | - | - | ERROR: "CM v4.0: Unknown metadata block" | | | | | | | |
| st2094_10_level3 | - | - | - | - | - | ERROR: "CM v2.9: Disallowed block level" (expected: test asserts is_err) | | | | | | | |

Frame count: one RPU per file except p8_001 (3). Per-RPU variation was not measured beyond these.

## Assertions vs results (rpu.rs)
- fel(): fel_rpu P7 + FEL: PASS. mel(): mel_rpu P7 + MEL: PASS. fel_to_mel: MEL: PASS.
- profile4/5/8, profile5-02, profile20_apple (`assert_eq!(dovi_rpu.dovi_profile, 5)`): PASS (profile20_apple reports 5).
- p8_001_end_crc32: `rpus.len() == 3` and `[216,0,0,1] == crc32 bytes`: PASS (crc d8000001).
- st2094_10_level3_invalid: `assert!(res.is_err())`: PASS.
- level253_ext_metadata: asserts profile 5, L253 length 2 bytes, bytes [0x55;2]: FAIL with crates.io 3.4.0.

## Findings
1. FEL/MEL detection is correct on all 11 P7 fixtures (FEL: fel_rpu, fel_orig, data_before_crc32, poly_coef_int_logic, eof_rpu, fix_se_write, trailing_bytes; MEL: mel_rpu, mel_orig, fel_to_mel, mel_variable). The crate has no signalled flag. It labels MEL when nlq_offset=0, vdr_in_max_int=1, vdr_in_max=0 and deadzone=0 for all components (rpu_data_nlq.rs is_mel). A real FEL whose NLQ parameters are all zero would be labelled MEL. Not testable with these fixtures.
2. el_type is None for P5/P8 (no NLQ) and for RPUs with use_prev_vdr_rpu_flag (no mapping, see empty_dmv1). Per-frame FEL/MEL needs carry-forward from the last RPU that had a mapping.
3. Published crate 3.4.0 lacks Level 253. dovi_tool main's dolby_vision/src/rpu/extension_metadata/blocks/mod.rs has `Level253`, and the crates.io 3.4.0 source does not. Same version string, so the repo has unreleased code. A real L253 RPU makes the whole RPU fail, with no L1/L2 values. Atlas should use a git pin or treat Err as a partial result.
4. CM v4.0 rule (L3/8/9/10/11/254 present) gave correct labels on all 27 files. L254 presence matched v4.0 on every file. CMv4 RPUs in this set all carry both cmv29 and cmv40 containers; CMv2.9 files have only cmv29.
5. P8 converted from P7 (fel_to_81) drops the NLQ (el None) but keeps the CMv2.9 blocks. P5-derived P8 keeps the source L5 and L6.
6. Atlas-relevant: L6 max/min/cll/fall values are not one-per-file. Several P7/P8 files carry L6 4000/50 mastering with CLL/FALL 0, which is a real-world pattern to handle as "unknown" rather than 0.
7. L8 target 255 and L9 source_primary 0/2 both appear. Treat as valid enum values, not errors.
8. .hevc/Annex B path is implemented in code/w09a_main.rs but untested (no .hevc fixture found).
