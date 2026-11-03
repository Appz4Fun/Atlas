# W10b verification check: sevenz, zip, par2, sfv, srr, srs, split_raw

Scope: 151 catalog entries in the seven sections (sevenz 52, zip 40, par2 27, sfv 5, srr 6, srs 12, split_raw 9). The fragment `raw/w10b-other.yaml` holds 159 entries: 8 were added (7 ZIP central-directory fields, 1 PAR2 Main count). The source catalog was not edited.

Date of pass: 2026-11-02.

## Sources actually read in this pass

| Source | Status | Used for |
|---|---|---|
| ip7z/7zip `CPP/7zip/Archive/7z/7zHeader.h` | fetched | NID enum (0x00-0x19), method ids, IsFilterMethod list |
| ip7z/7zip `CPP/7zip/Archive/7z/7zIn.cpp` | fetched | start header version (`_header[7]`, major == 0 check), kAnti read at line 1434, kDummy at 1439 |
| ip7z/7zip `CPP/7zip/Crypto/7zAes.cpp` | fetched | encoder SaltSize = 4 (ResetSalt, line 194-195), IV size 16 (line 204), props layout (lines 214-226), NumCyclesPower 19 (line 236) |
| LuaDist/zip `proginfo/extrafld.txt` (Info-ZIP) | fetched | layouts of 0x000a, 0x000d, 0x5455, 0x5855, 0x7875; ID table |
| libarchive `archive_read_support_format_zip.c` | fetched | 0x9901 vendor/strength/method offsets (1208-1225); ZipCrypto check byte (1306-1308, 3420) |
| PAR2 spec (parchive.github.io) | NOT reachable (proxy 403) | PAR2 checked only against the prototype and w1c notes |
| APPNOTE.TXT | not fetched | ZIP CD layout (central-directory fields) taken from w9a 5.2 and kept UNVERIFIED |
| prototypes `proto/sevenz_zip_meta.rs`, `proto/par2_sfv_srr_meta.rs` | read | offset cross-checks |

## Discrepancies

| # | Field | YAML said | Source says | Ref | Action |
|---|---|---|---|---|---|
| 1 | 7z `aes_iv` (and meta.corrections line "7z AES: IV per coder, no salt by default") | no salt by default | Encoder sets SaltSize = 4 in ResetSalt and writes it in props (salt present by default), IV = 16 bytes | 7zAes.cpp:194-226 | Fixed in fragment. The meta.corrections line in the main YAML must be changed by the main session. |
| 2 | 7z `aes_props` | "0x40 IV, 0x80 salt; byte1 nibbles" (incomplete) | bit 0x80 = salt present, bit 0x40 = IV present; byte1 hi nibble = salt size-1, lo nibble = IV size-1; salt then IV follow. Default props = 0xD3 0x3F + 4-byte salt + 16-byte IV | 7zAes.cpp:214-226; proto sevenz_zip_meta.rs:429-439 (matches) | Fixed. |
| 3 | 7z `branch_filters` | list omits SWAP2 and SWAP4 | k_SWAP2 = 0x020302, k_SWAP4 = 0x020304 are filter methods | 7zHeader.h (k_SWAP2, k_SWAP4, IsFilterMethod) | Added. |
| 4 | 7z `fork_codec_ids` | VERIFIED | Only a comment for 0x4F71101 (7z-zstd) exists; LZ4, Brotli, LZS, Lizard are not in the header | 7zHeader.h comments | Status set to UNVERIFIED. |
| 5 | 7z `other_crypto_ids` | VERIFIED | 06F10101 (ZipCrypto) and 06F10303 (RAR29) are not defined in 7zHeader.h | 7zHeader.h | Status set to UNVERIFIED. |
| 6 | ZIP `zipcrypto_check_byte` | check byte = crc>>8 | Check byte is the high byte of CRC-32 (crc>>24, decdat = p[17]) normally; when bit 3 is set it is the high byte of the DOS time (p[11]) | libarchive zip.c:1306-1308, 3420 | Fixed. |
| 7 | ZIP `extra_other_ids` | lists 0x5855 and 0x6375 (own entries exist); 0x0014-0x0023 range | Duplicates removed. 0x0014-0x0017 is the range in extrafld.txt; 0x9902, 0xA11E, 0xCAFE, 0x4690 are not in extrafld.txt | extrafld.txt:37-79 | Deduplicated; status UNVERIFIED. |
| 8 | ZIP `cd_version_made_by` host table (prototype, not YAML) | YAML table correct | Prototype maps 11 => "VFAT" (should be MVS), has no 14 (VFAT) or 19 (OS X) | proto sevenz_zip_meta.rs:1202-1210 | YAML correct; prototype bug recorded, not fixed here. |
| 9 | ZIP `central_directory` coverage | CD fields @6, @8, @10, @12-15, @16, @20, @28-31 missing | Spec layout; Atlas reads these (w9a 5.2) | w9a 5.2; APPNOTE (not fetched) | Added 7 entries: cd_version_needed, cd_gp_flags, cd_method, cd_dos_time_date, cd_crc32, cd_compressed_size, cd_name_extra_len. All UNVERIFIED. |
| 10 | PAR2 `packet_type` | bare names (Main, FileDesc, ...) in a 16-byte field | Type field = "PAR 2.0\0" + name, NUL-padded to 16 bytes (e.g. "PAR 2.0\0Main\0\0\0\0") | w1c:297-304; proto par2_sfv_srr_meta.rs:148-159 | Fixed. |
| 11 | PAR2 Main body | recovery-set file IDs start after slice size (count missing) | Body @8 uint32 = recovery-set file count N; IDs start at body @12 | proto par2_sfv_srr_meta.rs:254-256; par2.rs:96 (w9a 6) | Added `main_file_count`. Spec not re-read (403). |
| 12 | PAR2 `uni_file_name` | "Unicode name" (no ID prefix stated) | Name read from body @16, i.e. after a 16-byte File ID | proto par2_sfv_srr_meta.rs:284 | Clarified in `where`; still UNVERIFIED against the spec. |

## Confirmed without change (this pass)

- 7z: all NIDs 0x00-0x19 match 7zHeader.h (kHeader 0x01, kEncodedHeader 0x17, kStartPos 0x18, kDummy 0x19 included). Start-header major == 0 check and minor from byte 7 match 7zIn.cpp:1643-1645. Method ids verified against 7zHeader.h: Copy 0, Delta 3, ARM64 0x0A, RISCV 0x0B, LZMA 030101, LZMA2 0x21, PPMD 030401, Deflate 040108, Deflate64 040109, BZip2 040202, BCJ 03030103, BCJ2 0303011B, PPC, IA64, ARM, ARMT, SPARC, AES 06F10701. NumCyclesPower default 19.
- 7z `kAnti` is read by 7-Zip (7zIn.cpp:1434). The Atlas copy skips it (w9a 4), so Atlas lists deletion markers as files. Recorded as an Atlas bug, not a YAML error.
- ZIP: 0x9901 vendor (LE16 at +0), "AE" at +2, strength at +4, actual method at +5 (libarchive reads 1 byte: bug). 0x000a, 0x000d, 0x5455, 0x5855, 0x7875 layouts match extrafld.txt.
- ZIP EOCD, zip64 EOCD, locator, and data-descriptor offsets were NOT re-checked against APPNOTE in this pass (not fetched); they keep the status from w1c.
- PAR2: FileDesc name @56, Main ids @12, IFSC layout (file ID then per-slice MD5+CRC32) and type strings match the prototype. The PAR2 packet header (magic, length, MD5 over bytes 32+, set ID, type) was not re-checked against the spec (blocked).
- SRR block type bytes 0x69, 0x6A, 0x71, 0x6B, 0x6C match the prototype (par2_sfv_srr_meta.rs:539-571).
- Split-level magics (ZIP, gzip, bzip2, xz, zstd, EBML) match the notes in w1c s2.2 and w8b. Not checked against an external source.

## Atlas column

Every fragment entry has `atlas` (stored | parsed | no) and `atlas_where`. The mapping comes from `raw/w9a-atlas-gaps.md` section by section (sections 4, 5, 6, 7, and the 1.1 format-detection note). Counts in the fragment: stored 16, parsed 46, no 97 (159 entries, including the 8 added).

Caveats:
- split_raw is not covered by w9a sections 4-7. Those entries are set to `no` with a "not covered by w9a" note, except `raw_classify_by_magic`, which is `parsed` for RAR, 7z, ZIP and PAR2 only (w9a 1.1).
- The 7 added ZIP CD entries are marked `no` (w9a 5.2 says they are not read).

## Still UNVERIFIED (28 entries in the fragment)

sevenz: start_header_zeroed_recovery, sfx_signature_search, start_pos, fork_codec_ids, other_crypto_ids, multivolume_logical_offsets

zip: data_descriptor_zip64, extra_strong_encryption_0017, extra_other_ids, cd_version_needed, cd_gp_flags, cd_method, cd_dos_time_date, cd_crc32, cd_compressed_size, cd_name_extra_len, split_raw_zip001, split_z01_naming, tz_offset_dos_vs_ut

par2: creator_other_clients, par3_draft

sfv: sfv_generator_header, sfv_md5_sha1_variants, nfo_sidecar

srs: srs_fingerprint_srsp

split_raw: raw_last_part_shorter, raw_mp4_ftyp, sevenz_logical_split

Also not re-verified in this pass (status left as in the source catalog, not confirmed here): all PAR2 layout detail beyond the prototype (spec blocked), all SFV/SRR/SRS entries except the SRR block-type bytes, `srs_mkv_elements` EBML IDs, `aes_strength` (strength-to-salt mapping), and the ZIP `lfh_*` fields other than the 0x9901 path.

## Other items for the main session (not fixed in the fragment)

- meta.corrections in the main YAML: change "7z AES: IV per coder, no salt by default" to the salt-by-default fact (item 1).
- Prototype `sevenz_zip_meta.rs:1202-1210`: zip host table bug (item 8).
- Atlas `archive/sevenz.rs:363`: kAnti is skipped, so deletion markers are listed as files (7zIn.cpp:1434 reads it).
- Fragment is `raw/w10b-other.yaml`; validation passed with `python3 -I -c "import yaml,sys; yaml.safe_load(open(sys.argv[1]))"`.
