# W7a: real-fixture validation of rar_meta / sz_zip_meta

Status: fixture run complete (235 files); no Windows-made archives. Run on 235 real/libarchive fixtures; par2meta not run (no PAR2/SFV/SRR fixtures fetched).

## Fixtures

- Source: libarchive test fixtures, `https://raw.githubusercontent.com/libarchive/libarchive/master/libarchive/test/<name>.uu` (uuencoded, decoded with python3 -I binascii.a2b_uu). Names from GitHub code search (`repo:libarchive/libarchive path:libarchive/test extension:uu`). Full name list: `/tmp/claude-0/-home-claude/1315c443-c098-58da-9f63-b135a00caf8b/scratchpad/w7a/lib_list.txt`.
- Downloaded 236; processed 235 (`test_read_format_zip_sfx` has no extension and was not dispatched). `.exe` SFX files were sent to rar_meta by mistake in the first pass; re-run with sz_zip_meta gave `not 7z/zip`.
- Not fetched: py7zr test data (GitHub code search returned nothing), rars crate fixtures (not used).
- Caveat: libarchive fixtures carry no tool provenance. Most are regression/fuzz files (crafted or truncated), so the 'origin' column below is an inference from header fingerprints, not a verified packer identity. No real WinRAR, 7-Zip, WinZip, or macOS archives were generated here (no Windows host, no network access to those tools).

## Fingerprint table (inferred origin, by header signature)

| Signature (observed) | Apparent origin | Example fixtures |
|---|---|---|
| RAR4, host unix, unp_ver 20/29, method m3/m5, EXTTIME present, no HTIME, no comment | RAR 3.x/4.x on Linux-style host (`rar`) | rar_compress_normal, rar_compress_best, rar_multivolume.part0001-4 |
| RAR4, host win32, unp_ver 29, DOS mtime only (no EXTTIME) | RAR 3.x-era Windows (WinRAR) | rar_windows, rar_filter, rar_sfx.exe (sfx_offset 98816, detected) |
| RAR4 encrypted headers (main header encrypted_headers=Y, no readable file list) | RAR 3.x/4.x with -hp | rar4_encrypted_filenames, rar4_solid_encrypted_filenames, rar_encryption_header |
| RAR5, host windows (1), HTIME present, Win32 attr, QO abs8, dict 128K | WinRAR 5.x Windows (inferred) | rar5_win32, rar5_fileattr, rar5_unicode, rar5_zip_in_rar |
| RAR5, host unix (0), HTIME present, owner_user/group recorded, QO abs8 | rar 5.x Linux (`rar`) (inferred) | rar5_owner (nobody/root), rar5_multiple_files, rar5_stored |
| RAR5 multi-volume: QO abs present on each part, solid part sets dict 1024K | rar 5.x Linux, -v split (inferred) | rar5_multiarchive.part01-08, rar5_multiarchive_solid.part01-04 |
| RAR5 header-encrypted: `encryption_header` record, parse stops (`headers_encrypted_cannot_parse_further`) | RAR 5.x with -hp | rar5_encrypted_filenames, rar5_solid_encrypted_filenames |
| ZIP host 3 (Unix), spec 2.0/3.0, extras 0x5455 + 0x7875 (UT + Unix uid/gid), Info-ZIP-style | Info-ZIP `zip` 3.0 on Unix (inferred) | zip_symlink, zip_nested, zip_comment_stored_1/2, zip_filename_koi8r, zip_filename_utf8_ru |
| ZIP host 3, spec 6.3, method 14/93/95/98 (LZMA/zstd/xz/PPMd) with 0x000a NTFS times | zipx writer, 6.3 spec (WinZip 2x or 7-Zip-style, inferred) | zip_bzip2.zipx, zip_lzma.zipx, zip_ppmd8.zipx, zip_zstd.zipx, zip_xz_multi.zipx |
| ZIP method 99, 0x9901 AES, spec 6.3, AE-2 (CRC absent) strength 256 | WinZip AES (inferred) | zip_winzip_aes128, zip_winzip_aes256(_large/_stored) |
| ZIP AE-1 (CRC present), host 0 (MS-DOS), spec 2.0, 0x9901 | WinZip AE-1 / WinZip 8-era (inferred) | zip_winzip_aes256_large_zstd |
| ZIP host 0 (MS-DOS/FAT) with extras 0x5455 + 0x7875 (zip_msdos) and spec 6.4 with 0x0017 (encryption rows), 0x000a + 0x0017 (strong encryption header), traditional or strong crypto | PKWARE-style strong-encryption writer (inferred) | zip_encryption_data, zip_encryption_partially |
| ZIP host 3, 0x5855 extra (old Info-ZIP UT), DD flag, Mac metadata | Mac OS X / Info-ZIP 2.x (inferred) | zip_mac_metadata |
| 7z sig 0.3, LZMA/LZMA2, kEncodedHeader, attr 0x81A48020 (unix mode in high 16 bits) | 7-Zip or p7zip on Unix/Windows (inferred) | 7zip_copy_2, 7zip_lzma1_2, 7zip_lzma1_lzma2, 7zip_symbolic_name |
| 7z sig 0.4, filters ARM64/RISCV/SPARC/PPC (`unknown` coder for some), attr 0x81FD8020 | newer 7-Zip (23+/24+) or 7-Zip-compatible writer (inferred) | 7zip_lzma1_arm64, 7zip_lzma2_riscv, 7zip_deflate_arm_arm64, 7zip_zstd_* |
| 7z sig 0.2 | older 7-Zip / early writer (inferred) | 7zip_packinfo_digests, 7zip_folders_oom (malformed) |
| 7z AES header (kEncodedHeader + AES-256+SHA-256 coder) | 7-Zip with -mhe=on | 7zip_encryption_header, 7zip_encryption |

Fields that do NOT identify the packer on their own: 7z has no creator-version field; RAR5 'version' is the compression algorithm version (0 for almost all, 32 in redir_varint_2byte); dict size varies with the -md setting, not the tool.

## Extractor bugs and gaps

1. **sevenz_zip_meta.rs, ZIP host table (~line 1204-1210): wrong OS names.** Only 0, 3, 7, 10, 11 are mapped, and 11 is labeled `VFAT`. APPNOTE says 11 = MVS (OS/390) and 14 = VFAT. Any ZIP with made-by host 14 prints `other`. Example: test_read_format_zip_filename_cp932.zip prints `11 (VFAT)`. Fix: map 1 Amiga, 2 OpenVMS, 4 VM/CMS, 5 Atari, 6 HPFS, 9 CP/M, 11 MVS, 13 Acorn, 14 VFAT, 16 BeOS, 18 OS/400, 19 OS/X.
2. **sevenz_zip_meta.rs: no SFX or embedded-signature scan.** test_read_format_7zip_sfx_pe.exe (7z signature at offset 213504), test_read_format_7zip_sfx_boundary_pe.exe, test_read_format_zip_sfx, and test_read_format_rar5_extra_data_size_toolong.rar.Z all print `not 7z/zip (sniff: unknown)`. The zip padded1/padded3 fixtures have `PK\x03\x04` at offset 89 (leading junk) and are also rejected, although padded2 (trailing junk) parses. rar_meta does find the RAR SFX offset (rar_sfx.exe: sfx_offset 98816).
3. **sevenz_zip_meta.rs, ZIP: test_read_format_zip_extra_padding.zip** parses the local header but reports no central directory (no eocd/cd fields). Unverified whether the file is malformed or the CD search failed.
4. **ZIP garbage input is not rejected.** zip_bz2_hang, zip_malformed1, zip_lzma_alone_leak print local-header fields from non-archive bytes (e.g. version_needed 1244.3, method 12300). The fixtures are fuzz cases, so this is a validation gap, not a crash. No panics or non-zero exits in any run (all 235 rc=0, stderr empty).
5. **rar_meta.rs: RAR5 header-encrypted archives** emit `encryption_header` and stop cleanly, which is correct. The file list is empty by design; the summarizer initially missed this, so consumers must check the `encryption_header` record.
6. **version_needed formatting**: printed as decimal major.minor (78 -> `7.8`, 93 -> `9.3`). Matches APPNOTE value encoding but the values look odd; verify against a real WinZip/7-Zip ZIP before relying on it for fingerprinting.
7. Many RAR5 fixtures print `bad_header: zero header size` / `bad_header: field overruns header bounds` (about 20 fuzz/regression files). Expected, but these files are not real packer output and should not feed the fingerprint table.

## Per-file table (auto-generated)

Columns: fmt | host | ver | meth | dict | solid | RR | QO | enc | enc_hdr | htime | exttime | owner | svc | n files | problems. For ZIP: host (made-by), spec, need, method, extras, enc, dd, zip64, aes, n entries. For 7z: sig, coders, dict, enc, enc_hdr, attr, unix, ctime/atime/mtime, n files.

| file | fmt | host | ver | meth | dict | solid | RR | QO | enc | enc_hdr | htime | exttime | owner | svc | n | problems |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| rar.rar | rar4 | unix | 20 | store | - | n | n | - | n | n | n | Y | - | - | 5 | - |
| rar3_lowdist_reset.rar | rar4 | unix | 29 | m5 | - | n | n | - | n | n | n | n | - | - | 1 | - |
| rar4_encrypted.rar | rar4 | win32 | 29 | m3/store | - | n | n | - | Y | n | n | Y | - | - | 4 | - |
| rar4_encrypted_filenames.rar | rar4 | - | - | - | - | n | n | - | n | Y | n | n | - | - | 0 | - |
| rar4_solid_encrypted_filenames.rar | rar4 | - | - | - | - | Y | n | - | n | Y | n | n | - | - | 0 | - |
| rar5_arm.rar | rar5 | unix | rar5 v0 | lzss | 128K | n | n | abs40990 | n | - | Y | n | - | QO | 1 | - |
| rar5_arm_filter_on_window_boundary.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | bad_header:zero header size |
| rar5_bad_tables.rar | rar5 | unix | rar5 v0 | lzss | 128K | n | n | - | n | - | n | n | - | - | 1 | - |
| rar5_bad_window_sz_in_mltarc_file.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | bad_header:bad main header body |
| rar5_blake2.rar | rar5 | unix | rar5 v0 | lzss | 128K | n | n | abs8 | n | - | Y | n | - | - | 1 | - |
| rar5_block_size_is_too_small.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | bad_header:zero header size |
| rar5_bytes_remaining_underflow.rar | rar5 | unix | rar5 v0 | lzss | 128K | n | n | - | n | - | n | n | - | - | 1 | truncated_at:truncated_header |
| rar5_compressed.rar | rar5 | unix | rar5 v0 | lzss | 128K | n | n | abs8 | n | - | Y | n | - | - | 1 | - |
| rar5_data_ready_pointer_leak.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | bad_header:bad main header body |
| rar5_decode_number_out_of_bounds_read.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | bad_header:field overruns header bounds |
| rar5_different_solid_window_size.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | bad_header:zero header size |
| rar5_different_window_size.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | bad_header:zero header size |
| rar5_different_winsize_on_merge.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | bad_header:zero header size |
| rar5_distance_overflow.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | bad_header:zero header size |
| rar5_encrypted.rar | rar5 | windows | rar5 v0 | lzss/store | 128K | n | n | abs8 | Y | - | Y | n | - | - | 4 | - |
| rar5_encrypted_filenames.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | - |
| rar5_extra_field_underflow.rar | rar5 | unix | rar5 v0 | store | 128K | n | n | - | n | - | n | n | - | - | 2 | - |
| rar5_extra_field_version.rar | rar5 | unix | rar5 v0 | lzss | 128K | n | n | abs8 | n | - | n | n | - | - | 2 | - |
| rar5_fileattr.rar | rar5 | windows | rar5 v0 | store | 128K | n | n | abs8 | n | - | Y | n | - | - | 8 | - |
| rar5_group_name_toolong.rar | rar5 | unix | rar5 v0 | - | 128K | n | n | - | n | - | n | n | - | - | 1 | bad_header:zero header size |
| rar5_hardlink.rar | rar5 | unix | rar5 v0 | store | 128K | n | n | abs8 | n | - | n | n | - | - | 2 | - |
| rar5_invalid_dict_reference.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | bad_header:zero header size |
| rar5_leftshift1.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | bad_header:zero header size |
| rar5_leftshift2.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | bad_header:zero header size |
| rar5_loop_bug.rar | rar5 | windows | rar5 v0 | lzss | 128K | n | n | - | n | - | n | n | - | - | 1 | - |
| rar5_main_block_extra_bytes.rar | rar5 | unix | rar5 v0 | store | 128K | n | n | - | n | - | Y | n | - | - | 1 | - |
| rar5_multiarchive.part01.rar | rar5 | unix | rar5 v0 | lzss | 512K | n | n | abs15223 | n | - | Y | n | - | QO | 1 | - |
| rar5_multiarchive.part02.rar | rar5 | unix | rar5 v0 | lzss | 512K | n | n | abs15223 | n | - | Y | n | - | QO | 1 | - |
| rar5_multiarchive.part03.rar | rar5 | unix | rar5 v0 | lzss | 512K | n | n | abs15115 | n | - | Y | n | - | QO | 2 | - |
| rar5_multiarchive.part04.rar | rar5 | unix | rar5 v0 | lzss | 512K | n | n | abs15223 | n | - | Y | n | - | QO | 1 | - |
| rar5_multiarchive.part05.rar | rar5 | unix | rar5 v0 | lzss | 512K | n | n | abs15223 | n | - | Y | n | - | QO | 1 | - |
| rar5_multiarchive.part06.rar | rar5 | unix | rar5 v0 | lzss | 512K | n | n | abs15223 | n | - | Y | n | - | QO | 1 | - |
| rar5_multiarchive.part07.rar | rar5 | unix | rar5 v0 | lzss | 512K | n | n | abs15223 | n | - | Y | n | - | QO | 1 | - |
| rar5_multiarchive.part08.rar | rar5 | unix | rar5 v0 | lzss | 512K | n | n | abs9080 | n | - | Y | n | - | QO | 1 | - |
| rar5_multiarchive_solid.part01.rar | rar5 | unix | rar5 v0 | lzss | 1024K | Y | n | abs13212 | n | - | Y | n | - | QO | 9 | - |
| rar5_multiarchive_solid.part02.rar | rar5 | unix | rar5 v0 | lzss | 1024K | Y | n | abs13212 | n | - | Y | n | - | QO | 1 | - |
| rar5_multiarchive_solid.part03.rar | rar5 | unix | rar5 v0 | lzss | 1024K | Y | n | abs13212 | n | - | Y | n | - | QO | 1 | - |
| rar5_multiarchive_solid.part04.rar | rar5 | unix | rar5 v0 | lzss | 1024K | Y | n | abs8 | n | - | Y | n | - | - | 1 | - |
| rar5_multiple_files.rar | rar5 | unix | rar5 v0 | lzss | 128K | n | n | abs8 | n | - | Y | n | - | - | 4 | - |
| rar5_multiple_files_solid.rar | rar5 | unix | rar5 v0 | lzss | 1024K | Y | n | abs8 | n | - | Y | n | - | - | 4 | - |
| rar5_nonempty_dir_stream.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | bad_header:zero header size |
| rar5_owner.rar | rar5 | unix | rar5 v0 | store | 128K | n | n | abs8 | n | - | n | n | nobody,root | - | 3 | - |
| rar5_owner_name_toolong.rar | rar5 | unix | rar5 v0 | - | 128K | n | n | - | n | - | n | n | - | - | 1 | bad_header:zero header size |
| rar5_readtables_overflow.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | bad_header:zero header size |
| rar5_redir_overflow.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | bad_header:field overruns header bounds |
| rar5_redir_varint_2byte.rar | rar5 | unix | rar5 v32 | store | 128K | n | n | - | n | - | n | n | - | - | 1 | - |
| rar5_skip_block_extra_bytes.rar | rar5 | unix | rar5 v0 | store | 128K | n | n | abs8 | n | - | Y | n | - | - | 1 | - |
| rar5_solid.rar | rar5 | unix | rar5 v0 | lzss | 1024K | Y | n | abs8 | n | - | Y | n | - | - | 7 | - |
| rar5_solid_encrypted.rar | rar5 | windows | rar5 v0 | lzss | 32768K | Y | n | abs8 | Y | - | Y | n | - | - | 4 | - |
| rar5_solid_encrypted_filenames.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | - |
| rar5_stored.rar | rar5 | unix | rar5 v0 | store | 128K | n | n | abs8 | n | - | Y | n | - | - | 1 | - |
| rar5_stored_manyfiles.rar | rar5 | unix | rar5 v0 | store | 128K | n | n | abs8 | n | - | Y | n | - | - | 3 | - |
| rar5_symlink.rar | rar5 | unix | rar5 v0 | store | 128K,32768K | n | n | abs8 | n | - | n | n | - | - | 4 | - |
| rar5_truncated_huff.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | bad_header:zero header size |
| rar5_unicode.rar | rar5 | windows | rar5 v0 | store | 128K | n | n | abs8 | n | - | Y | n | - | - | 3 | - |
| rar5_unpacked_size_exceeds_declared.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | bad_header:extra area larger than header |
| rar5_win32.rar | rar5 | windows | rar5 v0 | lzss | 128K | n | n | abs8 | n | - | Y | n | - | - | 8 | - |
| rar5_window_buf_and_size_desync.rar | rar5 | - | rar5 v | - | - | - | - | - | n | - | n | n | - | - | 0 | bad_header:bad main header body |
| rar5_zip_in_rar.rar | rar5 | windows | rar5 v0 | store | 128K | n | n | abs8 | n | - | Y | n | - | - | 3 | - |
| rar_compress_best.rar | rar4 | unix | 20/29 | m5/store | - | n | n | - | n | n | n | Y | - | - | 6 | - |
| rar_compress_normal.rar | rar4 | unix | 20/29 | m3/store | - | n | n | - | n | n | n | Y | - | - | 6 | - |
| rar_encryption_data.rar | rar4 | unix | 29 | m3 | - | n | n | - | Y | n | n | n | - | - | 2 | - |
| rar_encryption_header.rar | rar4 | - | - | - | - | n | n | - | n | Y | n | n | - | - | 0 | - |
| rar_encryption_partially.rar | rar4 | unix | 29 | m3 | - | n | n | - | Y | n | n | n | - | - | 2 | - |
| rar_endarc_huge.rar | rar4 | - | - | - | - | - | - | - | n | - | n | n | - | - | 0 | - |
| rar_filter.rar | rar4 | win32 | 29 | m3 | - | n | n | - | n | n | n | n | - | - | 1 | - |
| rar_filter_incomplete_block.rar | rar4 | unix | 29 | m5 | - | n | n | - | n | n | n | Y | - | - | 1 | - |
| rar_invalid1.rar | rar4 | unix | 29 | m3 | - | n | n | - | n | n | n | Y | - | - | 1 | truncated_at:next_block_beyond_buffer |
| rar_multi_lzss_blocks.rar | rar4 | unix | 29 | m3 | - | n | n | - | n | n | n | Y | - | - | 1 | - |
| rar_multivolume.part0001.rar | rar4 | unix | 29 | m5 | - | n | n | - | n | n | n | Y | - | - | 1 | - |
| rar_multivolume.part0002.rar | rar4 | unix | 29 | m5 | - | n | n | - | n | n | n | Y | - | - | 1 | - |
| rar_multivolume.part0003.rar | rar4 | unix | 29 | m5 | - | n | n | - | n | n | n | Y | - | - | 2 | - |
| rar_multivolume.part0004.rar | rar4 | unix | 20/29 | m5/store | - | n | n | - | n | n | n | Y | - | - | 6 | - |
| rar_newsub_huge.rar | rar4 | - | - | - | - | - | - | - | n | - | n | n | - | - | 0 | truncated_at:buffer_end_at_block_boundary |
| rar_noeof.rar | rar4 | unix | 20 | store | - | n | n | - | n | n | n | n | - | - | 1 | truncated_at:buffer_end_at_block_boundary |
| rar_overflow.rar | rar4 | win32 | 0 | m3 | - | - | - | - | n | - | n | n | - | - | 1 | truncated_at:truncated_header |
| rar_ppmd_lzss_conversion.rar | rar4 | unix | 29 | m5 | - | n | n | - | n | n | n | Y | - | - | 1 | - |
| rar_ppmd_use_after_free.rar | rar4 | macos | 13 | m5 | - | - | - | - | Y | - | n | n | - | - | 1 | truncated_at:truncated_header |
| rar_ppmd_use_after_free2.rar | rar4 | macos | 13 | m5 | - | - | - | - | Y | - | n | n | - | - | 1 | truncated_at:truncated_header |
| rar_seek_data_cursor0.rar | rar4 | unix | 20 | store | - | n | n | - | n | n | n | Y | - | - | 2 | truncated_at:next_block_beyond_buffer |
| rar_sfx.exe | rar4 | win32 | 20/29 | m3/store | - | n | n | - | n | n | n | Y | - | - | 5 | - |
| rar_subblock.rar | rar4 | unix | 20 | store | - | n | n | - | n | n | n | n | - | CMT | 1 | - |
| rar_symlink_huge.rar | rar4 | unix | 0 | m0 | - | - | - | - | n | - | n | n | - | - | 1 | truncated_at:buffer_end_at_block_boundary |
| rar_unbound_staticdata.rar | rar4 | unix | 29 | m1 | - | n | n | - | n | n | n | n | - | - | 1 | bad_header:head_size<7 |
| rar_unicode.rar | rar4 | unix/win32 | 20/29 | m3/store | - | n | n | - | n | n | n | Y | - | - | 6 | - |
| rar_windows.rar | rar4 | win32 | 20 | store | - | n | n | - | n | n | n | Y | - | - | 5 | - |

ZIP / 7z rows are in `/tmp/claude-0/-home-claude/1315c443-c098-58da-9f63-b135a00caf8b/scratchpad/w7a/rows.json`; raw extractor output is in `.../w7a/out/`.

| file | fmt | host(made_by) | spec | need | method | extras | enc | dd | zip64 | aes | n | note |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| zip.zip | zip | 3 (Unix) | 2.3 | 1.0 | 0/8 | 0x5455,0x7855 | n | Y | n | - | 3 | data descriptor with zero csz: cannot skip to next local header |
| zip_7075_utf8_paths.zip | zip | 0 (MS-DOS/FAT) | 3.1 | 2.0 | 8 | 0x000a,0x7075 | n | n | n | - | 4 | - |
| zip_7z_deflate.zip | zip | 3 (Unix) | 2.0 | 78.8 | 8 | - | n | n | n | - | 3 | - |
| zip_7z_lzma.zip | zip | 3 (Unix) | 2.0 | 77.8 | 0/14 | - | n | n | n | - | 9 | - |
| zip_bz2_hang.zip | zip | - | - | - | - | - | n | n | n | - | - | - |
| zip_bzip2.zipx | zip | 3 (Unix) | 6.3 | 81.4 | 12 | 0x000a | n | n | n | - | 1 | - |
| zip_bzip2_multi.zipx | zip | 3 (Unix) | 6.3 | 81.4 | 12 | 0x000a | n | n | n | - | 3 | - |
| zip_comment_stored_1.zip | zip | 3 (Unix) | 3.0 | 1.0 | 0 | 0x5455,0x7875 | n | n | n | - | 2 | - |
| zip_comment_stored_2.zip | zip | 3 (Unix) | 3.0 | 2.0 | 0/8 | 0x5455,0x7875 | n | n | n | - | 2 | - |
| zip_encryption_data.zip | zip | 0 (MS-DOS/FAT) | 6.4 | 5.1 | 0 | 0x000a,0x0017 | Y | n | n | - | 2 | - |
| zip_encryption_header.zip | zip | - | - | - | - | - | n | n | Y | - | 65535 | - |
| zip_encryption_partially.zip | zip | 0 (MS-DOS/FAT) | 6.4 | 1.0 | 0 | 0x000a,0x0017 | Y | n | n | - | 2 | - |
| zip_extra_padding.zip | zip | - | - | - | - | - | n | n | n | - | - | - |
| zip_filename_cp866.zip | zip | 3 (Unix) | 3.0 | 1.0 | 0 | 0x5455,0x7875 | n | n | n | - | 2 | - |
| zip_filename_cp932.zip | zip | 11 (VFAT) | 2.0 | 1.0 | 0 | - | n | n | n | - | 2 | - |
| zip_filename_koi8r.zip | zip | 3 (Unix) | 3.0 | 1.0 | 0 | 0x5455,0x7875 | n | n | n | - | 2 | - |
| zip_filename_utf8_jp.zip | zip | 3 (Unix) | 3.0 | 1.0 | 0 | 0x5455,0x7875 | n | n | n | - | 3 | - |
| zip_filename_utf8_ru.zip | zip | 3 (Unix) | 3.0 | 1.0 | 0 | 0x5455,0x7875 | n | n | n | - | 2 | - |
| zip_filename_utf8_ru2.zip | zip | 3 (Unix) | 3.0 | 1.0 | 0 | 0x5455,0x7875 | n | n | n | - | 2 | - |
| zip_high_compression.zip | zip | 3 (Unix) | 2.0 | 2.0 | 8 | 0x5455,0x7875 | n | Y | n | - | 1 | data descriptor with zero csz: cannot skip to next local header |
| zip_jar.jar | zip | 0 (MS-DOS/FAT) | 1.0 | 1.0 | 0 | 0xcafe | n | n | n | - | 1 | - |
| zip_length_at_end.zip | zip | 3 (Unix) | 3.0 | 2.0 | 8 | 0x5455,0x7875 | n | Y | n | - | 1 | data descriptor with zero csz: cannot skip to next local header |
| zip_lzma.zipx | zip | 3 (Unix) | 6.3 | 83.1 | 14 | 0x000a | n | n | n | - | 1 | - |
| zip_lzma_alone_large_size.zip | zip | 3 (Unix) | 2.0 | 77.8 | 0/14 | - | n | n | n | - | 9 | - |
| zip_lzma_alone_leak.zipx | zip | - | - | - | - | - | n | Y | n | - | - | - |
| zip_lzma_multi.zipx | zip | 3 (Unix) | 6.3 | 83.1 | 14 | 0x000a | n | n | n | - | 3 | - |
| zip_lzma_stream_end.zipx | zip | 3 (Unix) | 6.3 | 6.3 | 14 | - | n | n | n | - | 1 | - |
| zip_mac_metadata.zip | zip | 3 (Unix) | 2.1 | 2.0 | 0/8 | 0x5855 | n | Y | n | - | 3 | data descriptor with zero csz: cannot skip to next local header |
| zip_malformed1.zip | zip | - | - | - | - | - | n | n | n | - | - | - |
| zip_msdos.zip | zip | 0 (MS-DOS/FAT) | 3.0 | 1.0 | 0 | 0x5455,0x7875 | n | n | n | - | 6 | - |
| zip_nested.zip | zip | 3 (Unix) | 3.0 | 1.0 | 0 | 0x5455,0x7875 | n | n | n | - | 2 | - |
| zip_nofiletype.zip | zip | 3 (Unix) | 3.0 | 1.0 | 0 | - | n | n | n | - | 2 | - |
| zip_padded2.zip | zip | 3 (Unix) | 3.0 | 1.0 | 0 | 0x5455,0x7875 | n | n | n | - | 2 | - |
| zip_pkware_unix.zip | zip | 3 (Unix) | 2.0 | 2.0 | 0 | 0x000d | n | n | n | - | 1 | - |
| zip_ppmd8.zipx | zip | 3 (Unix) | 6.3 | 83.1 | 98 | 0x000a | n | n | n | - | 1 | - |
| zip_ppmd8_aes256_streaming.zipx | zip | 3 (Unix) | 6.3 | 6.3 | 99 | 0x000a,0x9901 | Y | Y | n | 2 256 | 1 | data descriptor with zero csz: cannot skip to next local header |
| zip_ppmd8_multi.zipx | zip | 3 (Unix) | 6.3 | 83.1 | 98 | 0x000a | n | n | n | - | 3 | - |
| zip_size_exceeds_declared_deflate.zip | zip | 0 (MS-DOS/FAT) | 2.0 | 2.0 | 0/8 | - | n | n | n | - | 2 | - |
| zip_size_exceeds_declared_stored.zip | zip | 0 (MS-DOS/FAT) | 2.0 | 2.0 | 0 | - | n | n | n | - | 2 | - |
| zip_symlink.zip | zip | 3 (Unix) | 3.0 | 1.0 | 0 | 0x5455,0x7875 | n | n | n | - | 2 | - |
| zip_symlink_unsupported_compression.zip | zip | 3 (Unix) | 2.0 | 2.0 | 0/200 | - | n | n | n | - | 2 | - |
| zip_traditional_encryption_data.zip | zip | 3 (Unix) | 3.0 | 2.0 | 8 | 0x5455,0x7875 | Y | Y | n | - | 2 | - |
| zip_uncompressed_size_off_by_4gib.zip | zip | 0 (MS-DOS/FAT) | 4.5 | 4.5 | 0 | 0x0001 | n | n | n | - | 1 | - |
| zip_ux.zip | zip | 3 (Unix) | 3.0 | 2.0 | 8 | 0x5455,0x7875 | n | n | n | - | 1 | - |
| zip_winzip_aes128.zip | zip | 3 (Unix) | 6.3 | 81.9 | 99 | 0x9901 | Y | n | n | 2 128 | 1 | - |
| zip_winzip_aes256.zip | zip | 3 (Unix) | 6.3 | 81.9 | 99 | 0x9901 | Y | n | n | 2 256 | 1 | - |
| zip_winzip_aes256_large.zip | zip | 3 (Unix) | 6.3 | 81.9 | 99 | 0x9901 | Y | n | n | 2 256 | 4 | - |
| zip_winzip_aes256_large_bzip2.zip | zip | 3 (Unix) | 6.3 | 5.1 | 99 | 0x000a,0x9901 | Y | n | n | 2 256 | 4 | - |
| zip_winzip_aes256_large_lzma.zip | zip | 3 (Unix) | 6.3 | 6.3 | 99 | 0x000a,0x9901 | Y | n | n | 2 256 | 4 | - |
| zip_winzip_aes256_large_ppmd.zip | zip | 3 (Unix) | 6.3 | 6.3 | 99 | 0x000a,0x9901 | Y | n | n | 2 256 | 4 | - |
| zip_winzip_aes256_large_xz.zip | zip | 3 (Unix) | 6.3 | 5.1 | 99 | 0x000a,0x9901 | Y | n | n | 2 256 | 4 | - |
| zip_winzip_aes256_large_zstd.zip | zip | 0 (MS-DOS/FAT) | 2.0 | 2.0 | 99 | 0x000a,0x9901 | Y | n | n | 1 256 | 4 | - |
| zip_winzip_aes256_stored.zip | zip | 3 (Unix) | 6.3 | 81.9 | 99 | 0x9901 | Y | n | n | 2 256 | 1 | - |
| zip_with_invalid_traditional_eocd.zip | zip | 3 (Unix) | 4.5 | 4.5 | 0 | 0x0001 | n | Y | Y | - | 65535 | - |
| zip_xz_multi.zipx | zip | 0 (MS-DOS/FAT) | 2.0 | 2.0 | 95 | 0x000a | n | n | n | - | 3 | - |
| zip_zip64a.zip | zip | 3 (Unix) | 3.0 | 4.5 | 8 | - | n | Y | n | - | 1 | - |
| zip_zip64b.zip | zip | 3 (Unix) | 3.0 | 4.5 | 8 | - | n | Y | n | - | 1 | - |
| zip_zipx_lzma_oom.zipx | zip | - | - | - | - | - | n | n | n | - | - | - |
| zip_zstd.zipx | zip | 3 (Unix) | 6.3 | 2.0 | 93 | - | n | n | n | - | 1 | - |
| zip_zstd_multi.zipx | zip | 3 (Unix) | 6.3 | 2.0 | 93 | - | n | n | n | - | 3 | - |

| file | fmt | sig | coders | dict | enc | enc_hdr | attr | unix | ctime/atime/mtime | n | crc_valid |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 7zip_archive_properties.7z | 7z | sig 0.3 | - | - | n | - | 0x81A48020 | Y | n/n/Y | 1 | true |
| 7zip_bcj2_bzip2.7z | 7z | sig 0.3 | BCJ2/BZip2 | - | n | - | 0x00000021 | n | n/n/Y | 1 | true |
| 7zip_bcj2_copy_1.7z | 7z | sig 0.3 | BCJ2/Copy | - | n | - | 0x00000021 | n | n/n/Y | 1 | true |
| 7zip_bcj2_copy_2.7z | 7z | sig 0.3 | BCJ2/Copy | - | n | - | 0x00000021 | n | n/n/Y | 1 | true |
| 7zip_bcj2_copy_lzma.7z | 7z | sig 0.3 | BCJ2/Copy/LZMA | 65536 | n | - | 0x00000021 | n | n/n/Y | 1 | true |
| 7zip_bcj2_deflate.7z | 7z | sig 0.3 | BCJ2/Deflate | - | n | - | 0x00000021 | n | n/n/Y | 1 | true |
| 7zip_bcj2_lzma1_1.7z | 7z | sig 0.3 | BCJ2/LZMA | 65536 | n | - | 0x00000021 | n | n/n/Y | 1 | true |
| 7zip_bcj2_lzma1_2.7z | 7z | sig 0.3 | BCJ2/LZMA | 65536 | n | - | 0x00000021 | n | n/n/Y | 1 | true |
| 7zip_bcj2_lzma2_1.7z | 7z | sig 0.3 | BCJ2/LZMA2 | 65536 | n | - | 0x00000021 | n | n/n/Y | 1 | true |
| 7zip_bcj2_lzma2_2.7z | 7z | sig 0.3 | BCJ2/LZMA/LZMA2 | 65536 | n | - | 0x00000021 | n | n/n/Y | 1 | true |
| 7zip_bcj_bzip2.7z | 7z | sig 0.3 | BCJ_x86/BZip2 | - | n | - | 0x00000021 | n | n/n/Y | 1 | true |
| 7zip_bcj_copy.7z | 7z | sig 0.3 | BCJ_x86/Copy | - | n | - | 0x00000021 | n | n/n/Y | 1 | true |
| 7zip_bcj_deflate.7z | 7z | sig 0.3 | BCJ_x86/Deflate | - | n | - | 0x00000021 | n | n/n/Y | 1 | true |
| 7zip_bcj_lzma1.7z | 7z | sig 0.3 | BCJ_x86/LZMA | 65536 | n | - | 0x816D8021 | Y | n/n/Y | 1 | true |
| 7zip_bcj_lzma2.7z | 7z | sig 0.3 | BCJ_x86/LZMA2 | 65536 | n | - | 0x816D8021 | Y | n/n/Y | 1 | true |
| 7zip_bzip2.7z | 7z | sig 0.3 | BZip2 | - | n | - | 0x81A48020 | Y | n/n/Y | 1 | true |
| 7zip_chained_lzma2.7z | 7z | sig 0.4 | LZMA2 | 4096 | n | - | 0x81B48020 | Y | n/n/n | 1 | true |
| 7zip_copy.7z | 7z | sig 0.3 | Copy | - | n | - | 0x00000020 | n | n/n/Y | 1 | true |
| 7zip_copy_2.7z | 7z | sig 0.3 | Copy/LZMA | 4096 | n | - | 0x41ED8010,0x81A48020 | Y | n/n/Y | 5 | true |
| 7zip_deflate.7z | 7z | sig 0.3 | Deflate | - | n | - | 0x81A48020 | Y | n/n/Y | 1 | true |
| 7zip_deflate_arm64.7z | 7z | sig 0.4 | ARM64/Deflate | - | n | - | 0x81FD8020 | Y | n/n/Y | 1 | true |
| 7zip_deflate_arm64_large.7z | 7z | sig 0.4 | ARM64/Deflate | - | n | - | 0x81A48020 | Y | n/n/n | 1 | true |
| 7zip_deflate_arm_arm64.7z | 7z | sig 0.4 | ARM64/Deflate/unknown | - | n | - | 0x81FD8020 | Y | n/n/Y | 2 | true |
| 7zip_deflate_armthumb.7z | 7z | sig 0.4 | Deflate/unknown | - | n | - | 0x81A48020 | Y | n/n/n | 1 | true |
| 7zip_deflate_delta.7z | 7z | sig 0.4 | Deflate/Delta | - | n | - | 0x81B48020 | Y | n/n/n | 1 | true |
| 7zip_deflate_ia64.7z | 7z | sig 0.4 | Deflate/unknown | - | n | - | 0x81A48020 | Y | n/n/n | 1 | true |
| 7zip_deflate_powerpc.7z | 7z | sig 0.4 | Deflate/unknown | - | n | - | 0x81FD8020 | Y | n/n/Y | 1 | true |
| 7zip_deflate_riscv.7z | 7z | sig 0.4 | Deflate/unknown | - | n | - | 0x81FD8020 | Y | n/n/Y | 1 | true |
| 7zip_delta4_lzma1.7z | 7z | sig 0.4 | Delta/LZMA | 32768 | n | - | 0x81A48020 | Y | n/n/Y | 1 | true |
| 7zip_delta4_lzma2.7z | 7z | sig 0.4 | Delta/LZMA2 | 32768 | n | - | 0x81A48020 | Y | n/n/Y | 1 | true |
| 7zip_delta_lzma1.7z | 7z | sig 0.3 | Delta/LZMA | 65536 | n | - | 0x81A48020 | Y | n/n/Y | 1 | true |
| 7zip_delta_lzma2.7z | 7z | sig 0.3 | Delta/LZMA2 | 65536 | n | - | 0x81A48020 | Y | n/n/Y | 1 | true |
| 7zip_empty_archive.7z | 7z | sig 0.3 | - | - | n | - | - | n | n/n/n | - | - |
| 7zip_empty_file.7z | 7z | sig 0.3 | - | - | n | - | 0x81A48020 | Y | n/n/Y | 1 | true |
| 7zip_encryption.7z | 7z | sig 0.3 | AES-256+SHA-256/LZMA | 65536 | Y | - | 0x81B48020 | Y | n/n/Y | 1 | true |
| 7zip_encryption_header.7z | 7z | sig 0.3 | AES-256+SHA-256 | - | Y | true | - | n | n/n/n | - | true |
| 7zip_encryption_partially.7z | 7z | sig 0.3 | AES-256+SHA-256/LZMA | 4096,65536 | Y | - | 0x81B48020 | Y | n/n/Y | 2 | true |
| 7zip_entries_oom.7z | 7z | sig 0.3 | - | - | n | - | - | n | n/n/n | - | false |
| 7zip_extract_second.7z | 7z | sig 0.3 | LZMA | 8388608 | n | - | 0x81B68020 | Y | Y/Y/Y | 2 | true |
| 7zip_folders_oom.7z | 7z | sig 0.2 | - | - | n | - | - | n | n/n/n | - | false |
| 7zip_lzma1.7z | 7z | sig 0.3 | LZMA | 65536 | n | - | 0x81A48020 | Y | n/n/Y | 1 | true |
| 7zip_lzma1_2.7z | 7z | sig 0.3 | LZMA | 4096,65536 | n | - | 0x41ED8010,0x81A48020 | Y | n/n/Y | 5 | true |
| 7zip_lzma1_arm.7z | 7z | sig 0.4 | LZMA/unknown | 8192 | n | - | 0x81ED8020 | Y | n/n/n | 1 | true |
| 7zip_lzma1_arm64.7z | 7z | sig 0.4 | ARM64/LZMA | 98304 | n | - | 0x81FD8020 | Y | n/n/n | 1 | true |
| 7zip_lzma1_armthumb.7z | 7z | sig 0.4 | LZMA/unknown | 98304 | n | - | 0x81A48020 | Y | n/n/n | 1 | true |
| 7zip_lzma1_ia64.7z | 7z | sig 0.4 | LZMA/unknown | 98304 | n | - | 0x81A48020 | Y | n/n/n | 1 | true |
| 7zip_lzma1_lzma2.7z | 7z | sig 0.3 | LZMA/LZMA2 | 4096,65536 | n | - | 0x41ED8010,0x81A48020 | Y | n/n/Y | 9 | true |
| 7zip_lzma1_powerpc.7z | 7z | sig 0.4 | LZMA/unknown | 98304 | n | - | 0x81FD8020 | Y | n/n/n | 1 | true |
| 7zip_lzma1_riscv.7z | 7z | sig 0.4 | LZMA/unknown | 12288 | n | - | 0x81FD8020 | Y | n/n/n | 1 | true |
| 7zip_lzma1_sparc.7z | 7z | sig 0.4 | LZMA/unknown | 1572864 | n | - | 0x81FD8020 | Y | n/n/n | 1 | true |
| 7zip_lzma2.7z | 7z | sig 0.3 | LZMA2 | 65536 | n | - | 0x81A48020 | Y | n/n/Y | 1 | true |
| 7zip_lzma2_arm.7z | 7z | sig 0.4 | LZMA2/unknown | 8192 | n | - | 0x81ED8020 | Y | n/n/Y | 1 | true |
| 7zip_lzma2_arm64.7z | 7z | sig 0.4 | ARM64/LZMA2 | 98304 | n | - | 0x81FD8020 | Y | n/n/Y | 1 | true |
| 7zip_lzma2_powerpc.7z | 7z | sig 0.4 | LZMA2/unknown | 98304 | n | - | 0x81FD8020 | Y | n/n/Y | 1 | true |
| 7zip_lzma2_riscv.7z | 7z | sig 0.4 | LZMA2/unknown | 12288 | n | - | 0x81FD8020 | Y | n/n/Y | 1 | true |
| 7zip_lzma2_sparc.7z | 7z | sig 0.4 | LZMA2/unknown | 1572864 | n | - | 0x81FD8020 | Y | n/n/Y | 1 | true |
| 7zip_malformed.7z | 7z | sig 48.48 | - | - | n | - | - | n | n/n/n | - | - |
| 7zip_malformed2.7z | 7z | sig 48.48 | - | - | n | - | - | n | n/n/n | - | - |
| 7zip_malformed4.7z | 7z | sig 0.4 | - | - | n | - | - | n | n/n/n | - | true |
| 7zip_malformed_numfiles_oom.7z | 7z | sig 0.3 | - | - | n | - | - | n | n/n/n | - | false |
| 7zip_packinfo_digests.7z | 7z | sig 0.2 | LZMA2 | 8388608 | n | - | - | n | n/n/Y | 2 | true |
| 7zip_ppmd.7z | 7z | sig 0.3 | PPMD | - | n | - | 0x00000020 | n | n/n/Y | 1 | true |
| 7zip_ppmd_arm64.7z | 7z | sig 0.4 | ARM64/PPMD | - | n | - | 0x81A48020 | Y | n/n/n | 1 | true |
| 7zip_ppmd_bcj.7z | 7z | sig 0.4 | BCJ_x86/PPMD | - | n | - | 0x81A48020 | Y | n/n/n | 1 | true |
| 7zip_ppmd_small_block.7z | 7z | sig 0.4 | LZMA/PPMD | 4096 | n | - | 0x81FF8020 | Y | n/n/Y | 4 | true |
| 7zip_solid_zstd.7z | 7z | sig 0.4 | LZMA/unknown | 4096 | n | - | 0x41ED8010,0x81A48020 | Y | n/n/Y | 5 | true |
| 7zip_symbolic_name.7z | 7z | sig 0.3 | LZMA | 4096,65536 | n | - | 0x81A48020,0xA1ED8020 | Y | n/n/Y | 2 | true |
| 7zip_win_attrib.7z | 7z | sig 0.4 | LZMA/LZMA2 | 4096 | n | - | 0x00000010,0x00000011,0x000000 | n | n/n/Y | 8 | true |
| 7zip_zstd.7z | 7z | sig 0.4 | LZMA/unknown | 4096 | n | - | 0x41ED8010,0x81A48020 | Y | n/n/Y | 5 | true |
| 7zip_zstd_arm.7z | 7z | sig 0.4 | unknown | - | n | - | 0x81ED8020 | Y | n/n/Y | 1 | true |
| 7zip_zstd_bcj.7z | 7z | sig 0.4 | BCJ_x86/unknown | - | n | - | 0x81FD8020 | Y | n/n/Y | 1 | true |
| 7zip_zstd_nobcj.7z | 7z | sig 0.4 | unknown | - | n | - | 0x81FD8020 | Y | n/n/Y | 1 | true |
| 7zip_zstd_sparc.7z | 7z | sig 0.4 | unknown | - | n | - | 0x81FD8020 | Y | n/n/Y | 1 | true |

