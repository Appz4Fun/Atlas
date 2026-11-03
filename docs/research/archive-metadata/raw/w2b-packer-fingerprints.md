# w2b: Packer and PAR2 creator fingerprints

Scope: PAR2 creator packets, Usenet posting tools, RAR and 7-Zip header-visible defaults.
Status key: VERIFIED = read in fetched source this session. UNVERIFIED = from memory or not reached.
Blocked this session: rarlab.com (proxy 403), 7-zip.org (proxy 403), MultiPar/par2j paths (404 on all guessed paths), ngPost command-builder file (not located; only main.cpp, Poster.cpp, README read).

## 1. PAR2 creator packet

```yaml
- tool: par2cmdline (Parchive, C++)
  packet: "PAR 2.0\0Creator\0"
  client_string_format: "Created by " PACKAGE " version " VERSION "."   # VERIFIED creatorpacket.cpp:41
  PACKAGE_VERSION_value: UNVERIFIED (configure.ac 404; version.h 404)
  creator_packet_placement: one copy appended to each recovery volume file   # VERIFIED par2creator.cpp:903-907
  creator_packet_in_index_file: UNVERIFIED
  blocksize_rule: must be multiple of 4                                     # VERIFIED par2creator.cpp:440
  defaults_from_help_text:                                                 # VERIFIED commandline.cpp:95,97
    block_count: 2000
    redundancy: 5%
  unicode_filename_packets: UNVERIFIED (not checked in par2creatorsourcefile)

- tool: ParPar (animetosho, JS; lib/par2.js, bin/parpar.js)
  version_read: 0.4.6 (package.json on master, may differ from a release tag)  # VERIFIED
  creator_string: "ParPar v" + version + " " + process.arch + " [https://github.com/animetosho/parpar]"   # VERIFIED parpar.js:336
  creator_packet_write: "PAR 2.0\0Creator\0"                               # VERIFIED par2.js:561
  main_packet_magic: "PAR 2.0\0Main\0\0\0\0"                              # VERIFIED par2.js:448
  file_desc_packet: "PAR 2.0\0FileDesc"                                    # VERIFIED par2.js:811
  unicode_name_packet: "PAR 2.0\0UniFileN"                                 # VERIFIED par2.js:825
  ascii_comment_packet: "PAR 2.0\0CommASCI"; unicode: "PAR 2.0\0CommUni\0" # VERIFIED par2.js:612,628
  ifsc_packet: "PAR 2.0\0IFSC\0\0\0\0"                                     # VERIFIED par2.js:764
  recovery_slice_packet: "PAR 2.0\0RecvSlic"                              # VERIFIED par2.js:514
  ascii_charset_default: utf-8                                             # VERIFIED parpar.js ~line 118
  unicode_option_default: null, then BOTH if name has non-ASCII, else ASCII-only   # VERIFIED par2.js:566,586
  unifilen_written: only when unicode==BOTH (non-ASCII name)               # VERIFIED par2.js:600-602
  slice_size_multiple_rule: multiple of 4; default 4 unless input-slices given in bytes   # VERIFIED parpar.js:546,641
  input_slices_default: none (required)                                    # VERIFIED parpar.js:410
  auto_slice_size_max_input_slices: 32768                                  # VERIFIED parpar.js:417
  recovery_slices_default: none (required)                                 # VERIFIED parpar.js:66 (no default)
  packet_order_on_disk: UNVERIFIED (order of writes in PAR2Gen not traced)
  quickpar_warning_slice_size: >100MB may be incompatible with QuickPar   # VERIFIED parpar.js:812

- tool: MultiPar / par2j (Yutaka-Sawada)
  status: UNVERIFIED. All guessed raw paths returned 404 (source/par2j/*.c, par2j/*.c).
  creator_string: UNVERIFIED

- tool: par2cmdline-turbo (animetosho fork)
  status: UNVERIFIED (not fetched)

- tool: phpar2, QuickPar
  status: UNVERIFIED (not fetched; QuickPar closed-source)
  QuickPar creator string: UNVERIFIED
```

## 2. Usenet posting tools and packer command lines

```yaml
- tool: ngPost (mbruel)
  version_in_readme: 4.16 (example command line)                           # VERIFIED README
  packers: "external rar binary" or 7zip, selected by --compress          # VERIFIED README:108
  par2_generation: external par2 binary, PAR2_CMD / PAR2_ARGS keywords in config   # VERIFIED README:33
  defaults:
    rar_size_MB: 0 (no split)                                              # VERIFIED README:103
    par2_pct: 0 (no par2 unless set)                                       # VERIFIED README:105
    length_name: 17                                                        # VERIFIED README:114
    length_pass: 13                                                        # VERIFIED README:115
  example_cmd: "--auto ... --compress --gen_par2 --gen_name --gen_pass --rar_size 42"  # VERIFIED README:129
  rar_command_line_exact: UNVERIFIED (builder file not located; Poster.cpp contains no rar args)
  par2_command_line_exact: UNVERIFIED
  archive_name_random: default length 17, random password default length 13

- tool: Nyuu, postie, JBinUp, nzb-monkey, nxg-upper, autoposter scripts
  status: UNVERIFIED (not fetched). The brief said Nyuu does not pack.
```

## 3. RAR and 7-Zip header-visible defaults

```yaml
- tool: RAR / WinRAR (rarlab)
  status: UNVERIFIED for every value below. rarlab.com returned 403 through the proxy.
  rar5_default_dict: UNVERIFIED
  rar7_changes: UNVERIFIED
  rar4_default_dict: UNVERIFIED
  comp_version_field_rar7: UNVERIFIED
  volume_naming: .partN.rar vs .rNN: UNVERIFIED
  command_line_example: "rar a -m0 -v50m -ep1 -hp ..." is the brief's example, not verified

- tool: 7-Zip (ip7z/7zip, version read: 7-Zip 26.05 sources, DOC/readme.txt)
  version_in_source: "7-Zip 26.05 Sources"                                 # VERIFIED readme.txt:1
  history_txt: 404 on DOC/history.txt, not read
  lzma_sdk_dict_default_when_unset:                                        # VERIFIED C/LzmaEnc.c:76-82
    level_le_4: 1 << (level*2 + 16)   # 256KB, 1MB, 4MB, 16MB for levels 1-4
    level_5_to_8_on_64bit: 1 << (level + 20)   # 32MB at level 5 (SDK fallback, not 7-Zip UI table)
  7zip_ui_per_mx_dict_table: UNVERIFIED (HandlerOut.cpp does not hold it; not located)
  header_compression: SetHeaderMethod only runs when _compressHeaders is true   # VERIFIED 7zHandlerOut.cpp:60-62
  header_compression_default_value: UNVERIFIED
  lzma2_solid_block_size: derived from dicSize << 7 (cap 2^34)            # VERIFIED 7zHandlerOut.cpp:275
  p7zip_vs_7zz: UNVERIFIED (7-Zip source README only; not checked)
```

## Gaps for follow-up

- rarlab.com whatsnew and rarnew.htm: needed for RAR dict, comp version and naming.
- 7-zip.org history.txt: needed for version-by-version defaults.
- MultiPar/par2j and ngPost rar/par2 builder: find correct repo paths.
- Fetch par2cmdline configure.ac or AC_INIT for PACKAGE and VERSION.

## Sources

- https://raw.githubusercontent.com/Parchive/par2cmdline/master/src/par2creator.cpp (VERIFIED)
- https://raw.githubusercontent.com/Parchive/par2cmdline/master/src/creatorpacket.cpp (VERIFIED)
- https://raw.githubusercontent.com/Parchive/par2cmdline/master/src/creatorpacket.h (VERIFIED)
- https://raw.githubusercontent.com/Parchive/par2cmdline/master/src/commandline.cpp (VERIFIED)
- https://raw.githubusercontent.com/Parchive/par2cmdline/master/src/par2fileformat.h (VERIFIED)
- https://raw.githubusercontent.com/animetosho/ParPar/master/lib/par2.js (VERIFIED)
- https://raw.githubusercontent.com/animetosho/ParPar/master/bin/parpar.js (VERIFIED)
- https://raw.githubusercontent.com/animetosho/ParPar/master/package.json (VERIFIED)
- https://raw.githubusercontent.com/mbruel/ngPost/master/README.md (VERIFIED)
- https://raw.githubusercontent.com/mbruel/ngPost/master/src/main.cpp (VERIFIED)
- https://raw.githubusercontent.com/mbruel/ngPost/master/src/Poster.cpp (VERIFIED, no rar args)
- https://raw.githubusercontent.com/ip7z/7zip/main/DOC/readme.txt (VERIFIED)
- https://raw.githubusercontent.com/ip7z/7zip/main/C/LzmaEnc.c (VERIFIED)
- https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Archive/7z/7zHandlerOut.cpp (VERIFIED)
- https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Archive/Common/HandlerOut.cpp (VERIFIED, no dict table)
- https://www.rarlab.com/rarnew.htm (BLOCKED, 403)
- https://www.7-zip.org/history.txt (BLOCKED, 403)
