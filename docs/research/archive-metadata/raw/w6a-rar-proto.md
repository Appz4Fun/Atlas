# W6a: RAR4 + RAR5 partial-buffer header extractor (prototype)

Source: `docs/research/archive-metadata/proto/rar_meta.rs` (copy of `scratchpad/w6a/src/main.rs`). Build: `cargo build --release` in a cargo project with `crc32fast = "1.5.4"` as the only dependency. Usage: `rar_meta <file> [max_bytes]`, one JSON object per line. Size: 985 lines total, about 944 non-blank non-comment lines.

## Result summary
- No panics. Ran 167 RAR files (4 scratchpad fixtures, 1 generated RAR5 of 7.9 MB, 162 under rars-0.12.3/tests/fixtures) at full size, 1 KB, and 50%. Exit codes all below 100, no timeouts.
- Partial buffers: the first incomplete header is reported as `truncated_at` with a reason (`truncated_header`, `truncated_base_header`, `next_block_beyond_buffer`, `buffer_end_at_block_boundary`). Data areas past the buffer are reported as `data_in_buffer` < `pack_size`, not as errors.
- Layouts were coded against the w2c/w3c references and the unrar copies in scratchpad/w3c/unrar. Several fields were checked against the unrar source.
- The rars cross-check was NOT done (time cap).

## Fields that work (exercised on fixtures)
- archive: format, sfx_offset (signature scan of first 4 MB). RAR4 main flags: volume, first_volume, solid, locked, recovery_record, comment_present, new_numbering, authenticity_av, encrypted_headers. RAR5 main: volume, volume_number, solid, recovery_record, locked, locator_flags, locator QO/RR relative and absolute (absolute = main header offset + rel; the w3c reference says relative to main header start).
- RAR5 encryption header (type 4): enc_version, enc_flags, kdf_count_log2, salt_hex, check_value_hex. Seen on 6 fixtures. Parsing stops there, since later headers are encrypted.
- RAR4 MHD_PASSWORD (encrypted headers): detected, stops with `headers_encrypted_cannot_parse_further` (7 fixtures).
- file: name (RAR4 FHD_UNICODE decoded; verified on a Cyrillic name), host_os, attr_hex, unix_mode/unix_type/unix_mode_str when host is Unix, directory, unp_ver/version, method/method_name, pack_size, unp_size (RAR4 LHD_LARGE high parts included), data_offset, data_in_buffer, crc32, split_before/after, encrypted, solid, large, salt_hex (RAR4 LHD_SALT), dos_mtime_iso.
- RAR4 EXTTIME (LHD_EXTTIME): exttime_<name>_iso and exttime_<name>_rem_raw for mtime, ctime, atime, arctime (22 mtime hits). The remainder is printed raw. Its ns scaling is NOT verified.
- RAR5 file: fflags-based mtime_iso, crc32, method/solid/dict_bytes from comp_info, host, unix mode when host is Unix, split flags, blake2sp hash (extra 0x02), htime (extra 0x03) with mtime/ctime/atime and ns, encryption extra (0x01), file version (0x04), redirection (0x05), owner (0x06), subdata (0x07).
- service: name (CMT/QO/RR/RARS/UOW), pack/unp size, RAR5 RR percent (subdata 0x07 vint, 6 hits), RAR4 RR_PROTECT (version, RecSectors, TotalBlocks, rr_percent_estimate = Rec*100/Total, labeled as estimate since the formula is unverified), cmt_text when the data is stored (method 0 in RAR5, 0x30 in RAR4; 5 hits).
- end of archive: next_volume flag. RAR4 volume_number when EARC_VOLNUMBER is set.
- header CRC check per block: RAR4 low 16 bits of CRC32; RAR5 full CRC32.

## Fields not verified (code present, no fixture exercised them)
- RAR5 htime ctime/atime, file version (0x04), owner (0x06), redirection (0x05) output: 0 hits in the corpus.
- RAR4 UOW subblock: only a 64-byte hex preview of the body is emitted. Layout not decoded.
- RAR4 EXTTIME remainder scaling (ns).
- RR percent formula for RAR4 (estimate only).
- SRS blocks and RAR 1.x signatures: not handled.

## What failed or looks off
- 5 fixtures report `error: no RAR signature` (not RAR data, or the signature is past 4 MB).
- One RAR 2.0 fixture (rars-rar4-rev-gen style) reports `bad_header head_size<7` at offset 20. Not investigated. Probably a non-standard or split layout.
- A few RAR4 blocks report `header_crc_ok:false` (7 cases). Some fixtures are deliberately corrupted, and this was not checked case by case.
- RAR4 name paths keep backslashes. Not normalized.
- Not cross-checked against rars' parser or unrar output (no unrar/7z binary on this box). Field correctness is by reading the references.

## Sample output: m3_default.rar (RAR5, full file)
```
{"kind":"input","path":"w3a/fx/m3_default.rar","file_size":10007,"buffer_len":10007}
{"kind":"archive","format":"rar5","sfx_offset":0}
{"kind":"main_header","fmt":"rar5","offset":8,"header_crc_ok":true,"volume":false,"volume_number":null,"solid":false,"recovery_record":false,"locked":false,"locator_flags":1,"locator_qo_rel":9882,"locator_qo_abs":9890,"locator_rr_rel":null,"locator_rr_abs":null}
{"kind":"file","fmt":"rar5","offset":25,"header_crc_ok":true,"name":"bigtext_64k.bin","host_os":"windows","directory":false,"unp_size":65536,"unp_size_unknown":false,"pack_size":9784,"data_offset":106,"data_in_buffer":9784,"attr_hex":"0x20","dos_directory_attr":false,"dos_readonly_attr":false,"mtime
{"kind":"service","fmt":"rar5","offset":9890,"header_crc_ok":true,"name":"QO","host_os":"windows","directory":false,"unp_size":90,"unp_size_unknown":false,"pack_size":90,"data_offset":9909,"data_in_buffer":90,"attr_hex":"0x0","dos_directory_attr":false,"dos_readonly_attr":false,"mtime_iso":null,"crc
{"kind":"end_of_archive","offset":9999,"next_volume":false}
```

## Sample output: farmanager170.rar (RAR4, truncated to 1024 bytes)
```
{"kind":"input","path":"w3a/fx/farmanager170.rar","file_size":996166,"buffer_len":1024}
{"kind":"archive","format":"rar4","sfx_offset":0}
{"kind":"main_header","fmt":"rar4","offset":7,"header_crc_ok":true,"volume":false,"first_volume":false,"solid":true,"locked":true,"recovery_record":false,"comment_present":false,"new_numbering":false,"authenticity_av":false,"encrypted_headers":false}
{"kind":"file","fmt":"rar4","offset":20,"header_size":43,"header_crc_ok":true,"name":"File_id.diz","unicode_name":false,"host_os":"win32","attr_hex":"0x00000020","directory":false,"unp_ver":29,"version":"2.9","method":53,"method_name":"m5","pack_size":334,"unp_size":657,"data_offset":63,"data_in_buf
{"kind":"file","fmt":"rar4","offset":397,"header_size":56,"header_crc_ok":true,"name":"Addons\\XLat\\Descript.ion","unicode_name":false,"host_os":"win32","attr_hex":"0x00000022","directory":false,"unp_ver":29,"version":"2.9","method":53,"method_name":"m5","pack_size":37,"unp_size":61,"data_offset":4
```
