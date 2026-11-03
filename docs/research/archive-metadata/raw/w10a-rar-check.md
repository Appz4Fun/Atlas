# W10a: rar5 / rar4 / rev field check against unrar and rar_meta.rs

Scope: 108 entries (rar5 56, rar4 44, rev 8). Sources: unrar copies at /home/claude/scratchpad/w3c/unrar/ and /home/claude/scratchpad/w8c/ (arcread.cpp, headers.hpp, headers5.hpp, recvol5.cpp, recvol3.cpp, crypt5.cpp, crypt3.cpp, recvol.hpp, rawread.hpp). Prototype: proto/rar_meta.rs. Atlas: raw/w9a-atlas-gaps.md. Fragment: raw/w10a-rar.yaml (validated with yaml.safe_load).

Catalog discrepancies corrected in the fragment: 29 entries (each has a `check` key in the fragment).

## 1. Catalog corrections (YAML said X, source says Y)

| Entry | Catalog line | Correction (source) |
|---|---|---|
| rar5.crypt_pswcheck_algo | catalog.yaml:190 | Now verified in source: 2^N+32 iterations, XOR-fold of the 32-byte output into 8 bytes. Key uses 2^N and HashKey 2^N+16 (crypt5.cpp:84-85). Source crypt5.cpp:84-85,161,179. |
| rar5.file_unpacked_size | catalog.yaml:245 | Field is always present as a vint after file_flags, even when flag 0x8 is set. Source arcread.cpp:820-824. |
| rar5.comp_info | catalog.yaml:272 | Bit numbers are 1-based in the YAML while unrar uses 0-based FCI_* masks (headers5.hpp). Give explicit masks. "21 v1 flag" is FCI_RAR5_COMPAT (0x100000, RAR7 compression flags), not a version flag. Sources: headers5.hpp FCI_*; arcread.cpp:876. |
| rar5.comp_dict_exponent | catalog.yaml:290 | unrar masks the exponent with 0x0f when comp version is 0 (RAR 5.0) and 0x1f otherwise; prototype rar_meta.rs:883 always uses 0x1f. Source arcread.cpp:876. |
| rar5.extra_encryption | catalog.yaml:336 | Flag 0x02 is named FHEXTRA_CRYPT_HASHMAC in unrar ("MAC for unpacked data checksums"), not TWEAKED. The HashKey is derived with 2^N+16 iterations. Sources: headers5.hpp FHEXTRA_CRYPT_*; arcread.cpp:1065-1107; crypt5.cpp:84-85,161. |
| rar5.extra_time_htime | catalog.yaml:354 | YAML gives no layout. Value width depends on the unix flag; with unix+ns the ns words follow all seconds. Prototype rar_meta.rs:660 reads 8 bytes per time when unix+ns: wrong. |
| rar5.extra_redirection | catalog.yaml:372 | Missing the name-length vint between flags and the name. Source arcread.cpp:1177-1196 (prototype rar_meta.rs:678 has it right). |
| rar5.extra_unix_owner | catalog.yaml:381 | Length prefixes missing and uid/gid shown as unconditional. Actual: names are length-prefixed vints, numeric ids present only by flag. Source arcread.cpp:1197-1230 (prototype rar_meta.rs:685 matches source). |
| rar5.endarc_flags | catalog.yaml:527 | No change; atlas only. |
| rar4.head_crc | catalog.yaml:559 | YAML says CRC16. unrar computes the header check with GetCRC15 (low 16 bits of CRC32 over the header, rawread.hpp); the GetCRC15 body is not in the copies, so status is UNVERIFIED. Prototype rar_meta.rs:424 uses crc32 & 0xffff. |
| rar4.main_reserved_av | catalog.yaml:605 | Confirmed offsets: HighPosAV @7 (uint16), PosAV @9 (uint32) (SIZEOF_MAINHEAD3=13). |
| rar4.main_flags | catalog.yaml:614 | Bit 0x200 "ENCRYPTVER" is not defined in unrar headers.hpp (lines 24-37). Marked UNVERIFIED. Source headers.hpp:24-37. |
| rar4.unp_size_low | catalog.yaml:642 | Unknown-size rule is conditional on LHD_LARGE. Without LARGE, unknown = low == 0xFFFFFFFF. With LARGE, unknown only when both halves are 0xFFFFFFFF. Prototype rar_meta.rs:549 does not filter. Source arcread.cpp:320-339. |
| rar4.file_host_os | catalog.yaml:651 | Values confirmed. unrar also treats HOST_BEOS (5) as Unix (arcread.cpp:302). Prototype rar_meta.rs only treats 3 as Unix. |
| rar4.exttime | catalog.yaml:750 | Layout incomplete. ctime and atime each carry a uint32 DOS time before their remainder bytes. mtime uses the FILE header DOS time. The arctime nibble consumes no bytes. Prototype rar_meta.rs:373 reads a DOS time plus remainder for arctime too: wrong. Source arcread.cpp:392-423. |
| rar4.exttime_remainder | catalog.yaml:759 | Remainder bytes fill the high end of the 24-bit value, not the low end: shift = (J+3-count)*8. Unit is 100 ns. Source arcread.cpp:415-423. |
| rar4.file_flags | catalog.yaml:768 | Bit 0x2000 "EXTFLAGS" is not defined in unrar (headers.hpp:39-68). 0x4000 is SKIP_IF_UNKNOWN (headers.hpp:67). Marked UNVERIFIED for 0x2000. |
| rar4.file_is_directory | catalog.yaml:786 | Missing the RAR 1.5 rule: UnpVer < 20 and attr & 0x10 is a directory. Source arcread.cpp:286-288. Prototype rar_meta.rs:552 also misses it. |
| rar4.protect_mark | catalog.yaml:878 | unrar reads 8 bytes into Mark and does not compare them (arcread.cpp:466-473). The literal "Protect!" is not in the copies. |
| rar4.protect_percent | catalog.yaml:887 | Formula not in unrar copies; only the prototype estimate (rar_meta.rs:~465-470). Stays UNVERIFIED. |
| rar4.old_cmt_block | catalog.yaml:897 | Confirmed: UnpSize u16 @7, UnpVer @9, Method @10, CRC16 @11 (arcread.cpp:459-463). |
| rar4.endarc_crc_volnum | catalog.yaml:916 | Layout wrong in YAML and in prototype. DATACRC (uint32) comes first, then VOLNUMBER (uint16), both LE, at fixed offsets. Prototype rar_meta.rs ENDARC arm (line 483) reads u32 at @+7 gated on 0x8 only: wrong when 0x2 is set. Source arcread.cpp:447-456. |
| rar4.hp_kdf | catalog.yaml:935 | Confirmed: HashRounds = 0x40000 (262144), SHA-1 over password and salt (crypt3.cpp:32-46). |
| rev.rev5_signature | catalog.yaml:2353 | Confirmed: REV5_SIGN "Rar!\x1aRev", 8 bytes (w8c recvol.hpp:4-5). |
| rev.rev5_header | catalog.yaml:2362 | Offsets were missing. Confirmed from recvol5.cpp:444-472. Signature (8 bytes) at 0; CRC at 8; header size at 12; body at 16. CRC covers the size field and body. |
| rev.rev5_counts_recnum | catalog.yaml:2371 | Field offsets added. Version byte sits before the counts (recvol5.cpp:462-478). |
| rev.rev5_rev_crc | catalog.yaml:2380 | Offset added. Compared to CalcFileSum of the whole .rev file (recvol5.cpp:264-266, 527-529). |
| rev.rev5_data_volume_table | catalog.yaml:2389 | Offset added. Table exists only in the first rev file, after RevCRC (file @27). Source recvol5.cpp:483-493. |
| rev.rev3_new_trailer | catalog.yaml:2398 | Only the 7-byte EOF read is confirmed (recvol3.cpp:178-184, 454-455). The 3-ordinal and CRC layout was not checked here. |

Notes on rows above: `rar4.file_flags` and `rar4.main_flags` now flag 0x2000 and 0x200 as not defined by unrar (headers.hpp:24-68). `crypt_pswcheck_algo`, `file_unpacked_size`, `endarc_crc_volnum`, `hp_kdf` and the rev entries moved to VERIFIED after reading source.

## 2. Prototype (proto/rar_meta.rs) bugs found while checking

| Location | Bug | Source |
|---|---|---|
| rar_meta.rs:549 (rar4_file `unp`) | Unknown-size 0xFFFFFFFF not filtered: prints 4 GiB. Same bug as Atlas (w9a s2.1). | arcread.cpp:320-339 |
| rar_meta.rs:373 (rar4_exttime) | arctime nibble (i==0) reads a DOS time and remainder. unrar consumes no bytes for arctime. Also ctime/atime/mtime order is otherwise correct. | arcread.cpp:392-423 |
| rar_meta.rs:483-496 (RAR4 0x7b ENDARC) | Reads uint32 at @+7 as volume number, gated on 0x8 only. Must skip DATACRC (uint32 at @+7 when 0x2), then VOLNUMBER uint16. | arcread.cpp:447-456 |
| rar_meta.rs:552 (rar4_file is_dir) | Misses the UnpVer<20 && attr&0x10 directory rule. | arcread.cpp:286-288 |
| rar_meta.rs:660 (RAR5 extra 0x03 htime) | width is 8 when unix+ns. unrar reads uint32 seconds, then uint32 ns words after all times. Wrong values and offsets for unix+ns times. | arcread.cpp:1133-1163 |
| rar_meta.rs:862 (RAR5 file unp_size) | Unknown size (flag 0x8) is printed as 0, not null. | arcread.cpp:822-824 |
| rar_meta.rs:883 (RAR5 dict_bits) | Always masks 0x1f. unrar uses 0x0f when comp version is 0 (RAR 5.0). | arcread.cpp:876 |
| rar_meta.rs:424 (RAR4 crc_ok) | Uses crc32 & 0xffff. Consistent with unrar GetCRC15 per name, but the GetCRC15 body is not in the copies. Left UNVERIFIED. | rawread.hpp:27 |

Prototype checks that matched source: RAR5 block CRC over size..extra, hdr_size/type/flags/extra/data vints, main archive flags, locator (QO/RR relative to main header), crypt extra 0x01 layout, BLAKE2 extra 0x02, version extra 0x04, redirection extra 0x05 (with name-length vint), unix owner extra 0x06 (with length prefixes), RAR4 header fields @0..@36 incl. LARGE high words, salt, exttime nibble parsing (except arctime), PROTECT offsets (@+7, @+11, @+12, @+14). Header CRC for RAR4 uses crc32 & 0xffff.

## 3. Atlas status summary

- stored: rar5 file_data_size, file_unpacked_size, file_crc32, comp_method, file_name, crypt_lg2_count, crypt_salt, crypt_pswcheck_csum, header_contents_hidden, extra_encryption; rar4 main_flags (0x80 only), pack_size_low, unp_size_low, file_crc32, method, high_sizes, file_name, unicode_name_codes.
- parsed: the rest of the block-prefix, file-flags, name_size, host/attribute and encryption-header entries (see fragment `atlas_where`).
- no: all rev entries (w9a does not cover .rev), every extra and service record after the first, all RAR4 sub-blocks, PROTECT, comment, and hp-KDF entries, plus RAR5 main-header (kind 1) entries.

Full per-entry values with `atlas_where` are in raw/w10a-rar.yaml.

## 4. Entries still UNVERIFIED after this check

| Entry | Reason |
|---|---|
| rar5.crypt_salt_same_across_volumes | not in copies or not checked (see status) |
| rar5.qo_entry_flags | not in copies or not checked (see status) |
| rar5.rr_bytes_from_percent | not in copies or not checked (see status) |
| rar5.acl_owner_sid | not in copies or not checked (see status) |
| rar5.stm_host_url | not in copies or not checked (see status) |
| rar5.service_uow_av_ea2 | not in copies or not checked (see status) |
| rar4.head_crc | not in copies or not checked (see status) |
| rar4.sub_acl_qo_av_layouts | not in copies or not checked (see status) |
| rar4.protect_mark | not in copies or not checked (see status) |
| rar4.protect_percent | not in copies or not checked (see status) |
| rar4.hp_kdf_rar29_variant | not in copies or not checked (see status) |
| rev.rev3_new_trailer | not in copies or not checked (see status) |
| rev.rev_percent_derived | not in copies or not checked (see status) |

Also not verified by this check (left as in the catalog): qo_scope_per_volume, service_qo, service_rr, rr_percent, service_acl, service_stm_zone, stm_zone_id, endarc_trailer, RAR5 extra 0x07 subdata layout, RAR4 sub_data layout for NEWSUB types other than CMT and UOW.
