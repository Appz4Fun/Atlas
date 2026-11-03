# W9C - payload-level toolchain tells (first/last article bytes)

Scope: bytes inside the decoded yEnc payload. Confidence: S = read in source this wave, K = prior knowledge / format spec, not re-verified this wave.

| # | Where (article) | Bytes / field | Tell | Conf | Source |
|---|---|---|---|---|---|
| R1 | first | `52 61 72 21 1A 07 00` vs `... 1A 07 01 00` | RAR4 (rar <=4.x, or rar 5-6 `-ma4`) vs RAR5 (rar >=5.0 default) | K | RAR5 technote |
| R2 | first | RAR4 MAIN_HEAD flags: 0x0001 volume, 0x0010 new numbering (.partNN.rar), 0x0040 RR, 0x0080 encrypted headers (-hp), 0x0100 first volume | No 0x0010 on a volume set = old-style .rNN naming (`-vn`) | S | unrar headers.hpp |
| R3 | first | RAR4 FILE_HEAD UnpVer (20/26/29; 29 for all rar 3.x-4.x/-ma4) + HostOS (2=Win32, 3=Unix) | Separates WinRAR/rar.exe (2) from Linux/mac rar (3); UnpVer 29 is not version-specific | S | unrar headers.hpp |
| R4 | first | RAR5 main header archive flags: 0x01 volume, 0x02 volnumber (absent only in vol 1), 0x04 solid, 0x08 RR present, 0x10 locked (`-k`) | 0x08 = `-rr` used; 0x10 = `-k` (ngPost sample RAR_EXTRA is `-ep1 -m0 -k -rr5p`) | S | unrar headers5.hpp |
| R5 | first | RAR5 main-header extra record 0x01 Locator (flag 0x01 QO offset, 0x02 RR offset) | Locator only written when QO (`-qo`, default on for big archives) or RR exists; QO list itself sits at archive end (last article) | S/K | headers5.hpp |
| R6 | first | RAR5 main-header extra record 0x02 Metadata (name 0x01, ctime 0x02, unix time 0x04, ns 0x08) | Present only with `-ams` (save archive name/time); newer rar (6.x+) only - rare in usenet posts | S/K | headers5.hpp |
| R7 | first | RAR5 file header Compression Info: bits 0-5 algo (0=RAR5.0, 1=RAR7.0), 6 solid, 7-9 method (0=store), 10-14 dict, 15-19 dict fraction, bit 20 FCI_RAR5_COMPAT | algo=1 or nonzero fraction or bit 20 -> rar >=7.0. Stored (-m0) files written by 6.x and 7.x matched byte-for-byte in earlier waves, so for -m0 posts this field does not separate 6.x/7.x | S | unrar headers5.hpp, arcread.cpp |
| R8 | first | RAR5 file header Host OS (0=Windows, 1=Unix) | Windows WinRAR/rar.exe vs Linux/mac rar (ngPost on Linux, AppImage = Unix) | K | technote |
| R9 | first | RAR5 encryption header (type 4) immediately after signature | `-hp` (NxG Upper default; ngPost when GEN_PASS/RAR_PASS) | K | technote |
| R10 | last | RAR5 end-of-archive header flags 0x01 = "not last volume"; RR/QO blocks before it | Volume count unknown from first article; last article shows RR/QO presence | K | technote |
| R11 | name | `.partNN.rar` (RAR5 default and RAR4 `MHD_NEWNUMBERING`) vs `.rar/.r00` | `.rNN` requires `-vn` (rar 5+) or rar <=2.x-3.x default | K | rar docs |
| R12 | any | RAR volumes made by 7-Zip | Impossible: 7-Zip only reads RAR; any RAR = rarlab rar/WinRAR (or a library wrapper of it) | K | 7-Zip docs |
| Z1 | first | 7z sig `37 7A BC AF 27 1C` + `00 04` | 7-Zip (all modern), p7zip, 7zz, py7zr (b"\x00",b"\x04") | S | 7zOut.cpp FillSignature; py7zr archiveinfo.py |
| Z2 | first | 7z sig + `00 03` | libarchive (bsdtar, `archive_write_set_format_7zip`) writes minor 3 | S | libarchive 7zip writer l.971-972 |
| Z3 | last (header) | FilesInfo order: 7-Zip = EmptyStream, EmptyFile, Anti, [kDummy 0x19 pad to 16-byte], Name, CTime, ATime, MTime, StartPos, [kDummy pad to 4], WinAttrib | Reference order | S | 7zOut.cpp |
| Z4 | last | libarchive order: ..., Name, MTime, CTime, ATime, Attributes; no kDummy | MTime-before-CTime + minor 3 = libarchive | S | libarchive writer l.1483-1521 |
| Z5 | last | py7zr: EmptyStream or EmptyFile, kDummy padding to 4-byte (not 16), Name, MTime only, Attributes | No CTime/ATime ever; 4-byte alignment | S | py7zr archiveinfo.py l.962-994 |
| Z6 | last | Header usually LZMA-packed (kEncodedHeader 0x17); with `-mhe=on` it is AES - Z3-Z5 unreadable | Encoded+AES header = ngPost 7z mode (`-mx0 -mhe=on` sample) | S/K | ngPost.conf |
| P1 | any .par2 | Creator packet `PAR 2.0\0Creator\0`, body "Created by par2cmdline version X." | par2cmdline AND par2cmdline-turbo both use `"Created by " PACKAGE " version " VERSION "."`; turbo PACKAGE is "par2cmdline-turbo" | S | creatorpacket.cpp (both) |
| P2 | any .par2 | "ParPar vX.Y.Z <arch> [https://github.com/<owner>/parpar]" (literal ParPar repo URL) | ParPar version and CPU arch (x64/arm64/ia32) | S | ParPar bin/parpar.js l.336 |
| P3 | any .par2 | "par2j vX.Y.Z" | MultiPar backend; version = PRODUCT_VERSION | S | MultiPar par2j create.c l.900 |
| P4 | any .par2 | "QuickPar 0.9" / "Created by phpar2 ..." | QuickPar / phpar2 (legacy Windows) | K | - |
| P5 | slice size | par2cmdline default `-b2000` blocks, `-r5%`, variable (power-of-2-ish) recovery file sizes | slice = ceil(total/2000) rounded to x4 | S | commandline.cpp l.95-97 |
| P6 | slice size | ParPar has no default; `-s` mandatory; ngPost sample `-s5M -r1n*0.6 -p1l`; ngPost par2cmdline sample `c -l -m1024 -r8 -s768000`; MultiPar sample `/rr8 /lc40 /lr /rd2 /ss768000`; par2j native default ~ sqrt(total)*10 | 5 MiB exact slice -> ParPar via ngPost; 768000 -> ngPost with par2/MultiPar; ngPost default PAR2_PCT=8 | S | ngPost.conf, ParPar help |
| P7 | volume layout | ParPar default `--slice-dist pow2`, `--packet-redundancy pow2`, names `vol12+10` (or `-n` -> `vol12-22`); critical packets interleaved | Packet ordering/copy count separates ParPar from par2cmdline | S | ParPar help-full.txt |
| ZP1 | first | ZIP version-made-by host byte: 7-Zip writes host 0 (FAT) on Windows, 3 (Unix) on p7zip/7zz builds; ExtractVersion host always 0 | Old p7zip wrote Unix in extract field | S | 7-Zip ZipUpdate.cpp l.45-56 |
| ZP2 | first | Python zipfile: create_version 20 (0x14), create_system 3 non-Windows / 0 Windows, no extra fields | Bare 0x0314 + no extras = Python script | S | cpython zipfile |
| ZP3 | first | Info-ZIP zip 3.0 Unix: made-by 0x031E, extras UT 0x5455 + ux 0x7875 | Info-ZIP | K | - |

## Posting-tool wrapper defaults (payload side)

| Tool | Packer | Names | Password | PAR2 tool / % |
|---|---|---|---|---|
| ngPost | external rar (or 7z) `a -idp` + RAR_EXTRA; `-hp` if pass; `-v42m` if size set; RAR_MAX 99 vols | GEN_NAME random, LENGTH_NAME 22 in sample conf (code default 17) | GEN_PASS, LENGTH_PASS 15 (code default 13) | bundled par2cmdline (Win/AppImage) or ParPar; PAR2_PCT 8 |
| NxG Upper | rar.exe | 21-byte random string reused for rar and par2 names (rar obfuscation optional) | yes (-hp), PasswordLength option | par2cmdline-turbo or ParPar |
| Nyuu / postie / GoPostStuff | none (raw files) | as given | none | external, user-chosen |

Notes
- RAR version cannot be read from RAR5 headers directly; only feature bits (R6, R7) give a lower bound (>=7.0 for algo 1/fract/compat bit).
- For 7z the property-order and minor-version tells are the strongest library discriminators, but require the end header (last article) and an unencrypted header.
