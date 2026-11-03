# w3b: packer fingerprints (PAR2 creator, RAR/7z defaults, posting-tool args)

Status key: VERIFIED = read in fetched source (URL + line). VERIFIED-TEST = literal taken from a test fixture, not a live sample. UNVERIFIED = not confirmed in this pass.

Blocked during this pass: rarlab.com (direct and via web.archive.org, "SITE_BLOCKED"), Parchive/par2cmdline master and main configure.ac (client_error), Parchive/par2cmdline src/ (client_error), github.com tree pages (robots_disallowed).

## 1. PAR2 creator packet (creator string = "Created by " PACKAGE " version " VERSION ".")

```yaml
- tool: par2cmdline (mainline Parchive)
  version: UNVERIFIED (configure.ac unreachable)
  field: creator string template
  value: "Created by " PACKAGE " version " VERSION "."  -> e.g. "Created by par2cmdline version 0.8.1."
  status: VERIFIED template (libpar2 0.4 mirror, see Sources S4); version literal VERIFIED-TEST (S5)

- tool: par2cmdline (mainline) literal in test fixture
  version: 0.8.1
  field: creator packet payload
  value: "par2cmdline version 0.8.1" (NUL-padded to 4-byte multiple)
  status: VERIFIED-TEST (S5, line 40). Test fixture only, not a real-world sample.

- tool: par2cmdline-turbo (animetosho)
  version: UNVERIFIED
  field: AC_INIT package/version
  value: AC_INIT([par2cmdline], [1.3.0], [ike.devolder@gmail.com])
  status: VERIFIED configure.ac line 26 (S3). Turbo's creator string built from these macros is UNVERIFIED; mainline AC_INIT conflicts (0.8.1 vs 1.3.0), so do not assume.

- tool: ParPar
  version: prior pass
  field: creator string
  value: "ParPar v<ver> <arch> [https://github.com/animetosho/parpar]"
  status: VERIFIED by prior pass (not re-checked here)

- tool: MultiPar (Yutaka-Sawada)
  version: UNVERIFIED
  field: creator string
  value: UNVERIFIED (repo file layout not reachable; GitHub tree blocked by robots)
  status: UNVERIFIED

- tool: par2j (MultiPar-family / Sawada)
  version: UNVERIFIED
  field: creator string
  value: UNVERIFIED
  status: UNVERIFIED

- tool: QuickPar
  version: 0.9
  field: creator packet payload
  value: "QuickPar 0.9"
  status: VERIFIED-TEST (rust-par2 creator test assertion, S6). Not a live sample.

- tool: phpar2
  version: UNVERIFIED
  field: creator string
  value: UNVERIFIED
  status: UNVERIFIED (not searched in this pass)

- tool: par2-rs (Rust crate, parser)
  version: 0.10.6 (docs.rs)
  field: parser behaviour
  value: creator packet type = b"PAR 2.0\x00Creator\x00"; creator text decoded with from_utf8_lossy after first NUL (strip null padding)
  status: VERIFIED (S6, line 28-29; packets.rs TYPE_CREATOR)
```

Unicode filename packets: the PAR2 FileDesc packet (type b"PAR 2.0\x00FileDesc") carries the filename as a NUL-terminated byte string padded to a multiple of 4. par2-rs (rust-par2 0.1.2) decodes it with from_utf8_lossy and defines no separate Unicode packet type (S7). Which posting or PAR2 tools write UTF-8 filenames into FileDesc: UNVERIFIED in this pass.

## 2. RAR / WinRAR

```yaml
- tool: unrar source (pmachapman mirror)
  version: RARVER 7.30 beta 1
  field: RARVER_MAJOR / MINOR / BETA / DAY / MONTH / YEAR
  value: 7 / 30 / 1 / 9 / 9 / 2026
  status: VERIFIED (S8, version.hpp lines 1-6 as parsed by fetch). Recorded as found; not cross-checked against rarlab.

- tool: RAR format constants (unrar headers.hpp)
  field: VER_PACK / VER_PACK5 / VER_PACK7 / VER_UNPACK / VER_UNPACK5 / VER_UNPACK7
  value: 29 / 50 / 70 / 29 / 50 / 70 (hex-free decimal macros, U suffix)
  status: VERIFIED (S9, headers.hpp lines 16-21)
  note: RAR 7.0 comp version = 70 is the VER_PACK7 value. Confirms RAR 7 "comp version" encoding; RAR 5 = 50.

- tool: unrar window constants (unpack.hpp, S10)
  field: dictionary limit constants
  value: none defined in this file. MaxWinSize / MaxWinMask are runtime members (lines 398-399). ExtraDist comment allows distances up to 1 TB (line 401). LHD_WINDOW* flags in headers.hpp lines 51-58 (64 KB..4 MB encoding: LHD_WINDOW64=0x0000 ... LHD_WINDOW4096=0x00c0 in RAR 2.x/3.x header flags).
  status: VERIFIED for the flag encoding (S9). RAR 5 >4 GB dictionary value: UNVERIFIED (not in fetched files).

- tool: WinRAR / RAR 5.0 default dictionary size
  value: UNVERIFIED
- tool: WinRAR / RAR 6.x default dictionary size
  value: UNVERIFIED
- tool: WinRAR / RAR 7.0 raised dictionary size (>4 GB)
  value: UNVERIFIED (rarnew.htm via web.archive.org blocked; archive.org returned SITE_BLOCKED on 2 attempts)
- tool: RAR 5.0 default archive format (-ma4 vs -ma5)
  value: UNVERIFIED
- tool: RAR default volume naming (.partN.rar since 3.0; .rNN old style via -vn)
  value: UNVERIFIED from rarlab. Inference only: ngPost hands rar an output name "%1/%2.rar" (S11 line 986) and lets rar produce part names.
- tool: -htb, -qo, recovery record defaults
  value: UNVERIFIED
```

## 3. Posting tool packer command lines

Source: ngPost (mbruel/ngPost, GPLv3). Files: `src/PostingJob.cpp` (S11), `src/NgPost.h` (S12), `src/NgPost.cpp` (S13).

```yaml
- tool: ngPost
  file: src/NgPost.h
  field: sDefaultRarExtraOptions (non-Apple)
  line: 290
  value: "-ep1 -m0"
- tool: ngPost
  file: src/NgPost.h
  field: sDefaultRarExtraOptions (Apple/macOS)
  line: 288
  value: "-ep1 -m0 -x.DS_Store"
- tool: ngPost
  file: src/NgPost.h
  field: sDefault7zOptions
  line: 292
  value: "-mx0 -mhe=on"
- tool: ngPost
  file: src/PostingJob.cpp
  field: arg assembly
  lines: 921-986
  value:
    - 921: if (_rarPath.contains("7z")) -> 7z mode
    - 925: _rarArgs = sDefault7zOptions (7z mode)
    - 930: _rarArgs = sDefaultRarExtraOptions (rar mode)
    - 937: args.prepend("a")
    - 939: args << "-idp"   (rar only)
    - 945: args << "-mhe=on" (7z header encryption)
    - 946: args << "-p<pass>" (7z)
    - 949: args << "-hp<pass>" (rar header password)
    - 978: args << "-v<N>m"  (volume size, from _rarSize)
    - 986: <tmpdir>/<name>.7z or <name>.rar
    - 1044: args << "-r"
  status: VERIFIED (S11)
  resulting rar command (default, no password, no split): rar a -idp -ep1 -m0 <tmp>/<name>.rar <files>
  resulting rar command (split + password): rar a -idp -ep1 -m0 -v<N>m -hp<pass> <tmp>/<name>.rar <files> -r
  resulting 7z command (default): 7z a -mx0 -mhe=on <tmp>/<name>.7z <files>  (password adds -p<pass>)
- tool: ngPost
  file: src/PostingJob.cpp
  field: PAR2 input pattern
  lines: 1138, 1140
  value: 7z: <tmp>/<name>.7z* ; rar: <tmp>/<name>*rar
  status: VERIFIED (S11)
- tool: ngPost
  file: src/NgPost.cpp
  field: default volume size, naming, split
  lines: 272, 200, 201, 206, 208, 210, 212
  value: _rarSize(0) => no split by default. Archive name defaults to the NZB name (_rarName = _nzbName). --genName/random name option exists. Help text line 206 "compress inputs using RAR or 7z".
  status: VERIFIED (S13)
- tool: ngPost commented-out example
  file: src/PostingJob.cpp line 933
  value: "rar a -v50m -ed -ep1 -m0 -hp<PASS> <tmp>/<RAR_NAME>.rar <files>" (commented, not active)
  status: VERIFIED (S11)
```

Other posting tools (nzb-uploader scripts, Usenet-Uploader, postarr, upapi, camelsystem): not checked in this pass. UNVERIFIED.

## 4. 7-Zip

```yaml
- tool: 7-Zip (ip7z/7zip mirror)
  file: CPP/7zip/Archive/7z/7zHandlerOut.cpp (COutHandler::InitProps7z)
  field: header compression default
  value: _compressHeaders = true  (on by default; "hc" property)
  status: VERIFIED (S14, no line numbers in fetch)
- tool: 7-Zip
  field: header encryption default
  value: _encryptHeadersSpecified = false; _encryptHeaders = false; but CHandler::UpdateItems sets encryptHeaders = _passwordIsDefined, so a password encrypts headers unless "he" is set otherwise. Property name is "he" (ngPost uses "-mhe=on").
  status: VERIFIED (S14)
- tool: 7-Zip
  field: default method
  value: LZMA2 for level != 0; Copy for -mx0 (kDefaultMethodName=LZMA2, k_Copy_Name)
  status: VERIFIED (S14)
- tool: 7-Zip
  field: header dictionary / level
  value: header dictionary 1<<20 (k_Dictionary_ForHeaders), header level 5
  status: VERIFIED (S14)
- tool: 7-Zip
  field: solid block size
  value: kSolidBytes_Max = 1<<32, kSolidBytes_Min = 1<<24; LZMA2 solid size = cs << 6 capped at 1<<34; non-LZMA2 = dicSize << 7
  status: VERIFIED (S14)
- tool: 7-Zip
  field: per -mx dictionary table (LZMA2 -mx1..-mx9)
  value: UNVERIFIED. Get_Lzma_DicSize() is not in the fetched file. Table not retrieved (method.htm via web.archive.org blocked).
  status: UNVERIFIED
```

## Gaps (not filled)

- RAR 5.0/6.x/7.0 default dictionary sizes, -ma4/-ma5 default, RAR 7 >4 GB dictionary, -qo, -htb, recovery record defaults: blocked (rarlab and archive.org).
- par2cmdline mainline configure.ac: blocked (client_error on master and main). Version 0.8.1 rests on a test fixture.
- MultiPar, par2j, phpar2 creator strings: not found.
- Other posting tools: not checked.
- 7-Zip -mx dictionary table: not retrieved.

## Sources

- S1 https://raw.githubusercontent.com/Parchive/par2cmdline/master/configure.ac — client_error (not retrieved)
- S2 https://raw.githubusercontent.com/Parchive/par2cmdline/main/configure.ac — client_error (not retrieved)
- S3 https://raw.githubusercontent.com/animetosho/par2cmdline-turbo/master/configure.ac — line 26: AC_INIT([par2cmdline], [1.3.0], [ike.devolder@gmail.com])
- S4 https://grok.dragonflybsd.org/xref/dports/archivers/libpar2/libpar2-0.4/creatorpacket.cpp?a=true — line 38: string creator = "Created by " PACKAGE " version " VERSION "."; line 51 memcpy into packet->client
- S5 https://docs.rs/rust-par2/0.1.2/src/rust_par2/packets.rs.html — (see S7) and https://docs.rs/par2-rs/0.10.6/src/par2_rs/packet/creator.rs.html — line 40: b"par2cmdline version 0.8.1\x00\x00\x00" (test fixture); line 47: b"MyApp" (test)
- S6 https://docs.rs/rust-par2/0.1.2/src/rust_par2/packets.rs.html — TYPE_CREATOR = b"PAR 2.0\x00Creator\x00"; test assertion Some("QuickPar 0.9"); TYPE_FILE_DESC = b"PAR 2.0\x00FileDesc"
- S7 https://docs.rs/par2-rs/0.10.6/src/par2_rs/packet/creator.rs.html — lines 28-29 creator text decoded with from_utf8_lossy after first NUL
- S8 https://raw.githubusercontent.com/pmachapman/unrar/master/version.hpp — RARVER_MAJOR 7, MINOR 30, BETA 1, DAY 9, MONTH 9, YEAR 2026 (lines 1-6)
- S9 https://raw.githubusercontent.com/pmachapman/unrar/master/headers.hpp — lines 16-21 VER_PACK 29U, VER_PACK5 50U, VER_PACK7 70U, VER_UNPACK 29U, VER_UNPACK5 50U, VER_UNPACK7 70U; lines 51-58 LHD_WINDOW* flags
- S10 https://raw.githubusercontent.com/pmachapman/unrar/master/unpack.hpp — lines 385, 397-401 window members; no dictionary-limit constant
- S11 https://raw.githubusercontent.com/mbruel/ngPost/master/src/PostingJob.cpp — lines 921-1140 (arg assembly, par2 input patterns, commented example at 933)
- S12 https://raw.githubusercontent.com/mbruel/ngPost/master/src/NgPost.h — lines 288, 290 (sDefaultRarExtraOptions), 292 (sDefault7zOptions)
- S13 https://raw.githubusercontent.com/mbruel/ngPost/master/src/NgPost.cpp — lines 200-212, 272 (_rarSize(0))
- S14 https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Archive/7z/7zHandlerOut.cpp — InitProps7z (_compressHeaders = true; _encryptHeaders = false), SetMainMethod (solid sizes), UpdateItems (encryptHeaders = _passwordIsDefined), k_Dictionary_ForHeaders (1<<20), k_Level_ForHeaders (5). No line numbers in fetch output.
- Blocked: https://web.archive.org/web/2025/https://www.rarlab.com/rarnew.htm and https://web.archive.org/web/2024/https://www.rarlab.com/rarnew.htm — SITE_BLOCKED
- Blocked: https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Archive/Common/HandlerOut.cpp contents did not contain the dictionary-per-level table (see S14 for the actual dictionary logic location)
