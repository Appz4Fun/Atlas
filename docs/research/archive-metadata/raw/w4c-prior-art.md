# W4c prior art: how download clients read archive and PAR2 headers from first articles

Scope: NZBGet (nzbgetcom/nzbget, develop), SABnzbd (sabnzbd/sabnzbd, develop), and the PHP rarinfo library (not retrieved, see Gaps).
Method: raw source read from raw.githubusercontent.com. Line numbers are from the fetched develop-branch files and may drift.

## Summary of findings

- Neither client parses archive headers from the first article alone. Both wait for a whole file (or the whole volume) and then parse. The only partial-data logic is for PAR2 (16 KiB MD5 from the first article) and SAB's pre-check of a file's first bytes.
- NZBGet RarReader seeks block-to-block through a complete on-disk volume, reading headers only and skipping data areas by block size. It reads only the RAR headers, but it needs the whole file present.
- SABnzbd delegates RAR parsing to the `rarfile` pip package, with a patch (SABRarFile) that captures file headers from a single `part_only` volume. This is the closest prior art to "list the filenames from one volume".
- PAR2 16k hashes: NZBGet hashes the first min(16 KiB, article) bytes as it streams the first article (RenameContentAnalyzer). SAB reads the first 16384 bytes of each completed file (deobfuscate) and takes FileDesc hash16k from the PAR2 packet.
- SAB computes whole-file CRC32 for PAR2 files from IFSC slice CRCs by combining slice CRCs. This is the trick for recovering the file's CRC without the data.

## 1. NZBGet

### 1a. daemon/postprocess/RarReader.cpp / RarReader.h

Fields read (RAR3, ReadRar3Volume / ReadRar3Block / ReadRar3File):
- Signature check: DetectRarVersion compares first 8 bytes against RAR5 `52 61 72 21 1A 07 01 00` and first 7 bytes against RAR3 `52 61 72 21 1A 07 00` (RarReader.cpp ~L105-119). RAR5 parse seeks to offset 8.
- Block header (ReadRar3Block): CRC16 (buf[0..1]), type (buf[2]), flags (buf[3..4]), size (buf[5..6]); ADDSIZE (0x8000) gives a 32-bit add size.
- MAIN block (type 0x73): flags. RAR3_MAIN_PASSWORD 0x0080 sets m_encrypted and returns false if no password was supplied. RAR3_MAIN_NEWNUMBERING 0x0010 and RAR3_MAIN_VOLUME 0x0001 are stored.
- FILE block (type 0x74, ReadRar3File): reads PACK_SIZE (first 32-bit field stored in m_size), skips HOST_OS (1), FILE_CRC (4), reads FTIME (4 into m_time), skips UNP_VER+METHOD (2), reads NAME_SIZE (16-bit), ATTR (32-bit). If flag 0x0100 (LHD_LARGE): reads HIGH_PACK (added to block.trailsize) and HIGH_UNP (added to m_size). Name is read as NAME_SIZE bytes, rejected if > 8192.
  - Possible bug: the high-word handling adds HIGH_UNP_SIZE to m_size, which already holds PACK_SIZE. For files over 4 GiB, m_size is neither a clean pack nor unpack size. Verify before relying on it.
  - Split flags: 0x0001 SPLITBEFORE and 0x0002 SPLITAFTER are stored per file, used by RarRenamer.
- ENDARC block (type 0x7b): optional DATACRC (0x0002, skip 4), VOLNUMBER (0x0008, read 32-bit into m_volumeNo), NEXTVOL (0x0001). Volume number exists only here, so a first article cannot provide it for RAR3/4.
- After each block: Seek(trailsize) skips the data area. Invalid block types outside 0x72..0x7b fail the parse.
- Encrypted RAR3: each block header is preceded by an 8-byte salt and decrypted with an AES key derived from the password (DecryptRar3Prepare, 0x40000 rounds). Without a password the parse returns false.

Fields read (RAR5, ReadRar5Volume / ReadRar5Block / ReadRar5File):
- Block header: CRC32 (4, raw), HeaderSize (vint), HeaderType (vint), HeaderFlags (vint), ExtraSize (vint, if flag 0x01), DataSize (vint, if flag 0x02). trailsize = HeaderSize + DataSize.
- MAIN (type 1): archive flags (vint). ISVOL 0x01 and VOLNR 0x02. If VOLNR, volume number (vint) is stored as m_volumeNo. Absent means first volume.
- ENCRYPTION (type 4): reads kdf count and salt, then DecryptRar5Prepare; the IV follows each header.
- FILE (type 2, ReadRar5File): file flags (vint; TIME 0x02, CRC 0x04), unpacked size (vint, stored as m_size), attributes (vint), mtime (32-bit if TIME), CRC skip 4, compression info and host OS (two vints skipped), name length (vint, max 8192), name. SPLITBEFORE 0x08 and SPLITAFTER 0x10 are the block flags. Extra area scanned for EXTRATIME (0x03) to refine mtime.
- ENDARC (type 5): NEXTVOL flag 0x01. Unknown block types above 5 fail the parse.

Volume number and set membership: the RAR5 volume number comes from the MAIN header, which is in the first bytes. For RAR3/4 it comes only from ENDARC. RarRenamer does not use these numbers directly for sets (see 1b).

How it reads: `RarVolume::Read()` opens the whole file with DiskFile and loops until EOF. It seeks past data areas, so I/O is roughly the header bytes plus the seek calls. The file must exist at full size; a seek past EOF fails and the parse returns false. So RarReader is a post-download reader, not a first-article reader. Decryption state is freed after each volume (DecryptFree).

Comments/limits: the code contains debug output only (`debug(...)` in DEBUG builds). No explicit TODO about partial files was found in RarReader.cpp.

### 1b. daemon/postprocess/RarRenamer.cpp / RarRenamer.h

- Set discovery (~L202-215): a set starts at a volume with `GetVolumeNo() == 0` whose first file lacks SplitBefore. Sets are then completed by later volumes. Sets that cannot be completed are discarded.
- Uses RarReader output only (names, split flags, volume numbers). Renames files to the names inside the RAR, so obfuscated names are fixed.
- Limit: relies on complete volumes on disk. No first-article logic.

### 1c. daemon/queue/DirectRenamer.cpp (not daemon/postprocess)

The file is at daemon/queue/DirectRenamer.cpp (daemon/postprocess/DirectRenamer.cpp returns 404).

- RenameContentAnalyzer::Append (~L713-727): streams the first article's decoded bytes into an MD5 context, but only the first `16*1024 - m_dataSize` bytes (`rem16kSize = min(len, 16K - m_dataSize)`). It also stores the first 8 bytes as m_signature, and sets m_parFile if they equal Par2::packet_magic. m_parSetId is taken from the PAR2 header setid.
- RenameContentAnalyzer::Finish (~L728-740): finalises the MD5 as hash16k (hex string).
- Limit (line ~210-211): analysis is only done when `articleInfo->GetSize() >= 16 * 1024 || fileInfo->GetArticles()->size() == 1`. The comment says "we don't support analyzing of files split into articles smaller than 16KB". So a file split into articles smaller than 16 KiB gets no hash16k unless it is a single article.
- PAR2 side (~L173-190): for each source file in the par2 repairer's sourcefilemap, it takes `GetDescriptionPacket()->Hash16k()` and stores (filename, hash16k) in parHashes.
- BuildNewRegularName (~L576-600): matches a file's hash16k against parHashes; if the name differs (case-insensitively), it renames. Keeps the old suffix when it cannot match.
- Ordering: RenameFilesInProgress / RenameCompletedFiles (~L407, ~L495) rename in-progress and completed files. Renaming waits until all first articles are done (~L265-290), checking hash16k is set for each.
- Par2 unpausing (~L315-350): unpauses the smallest PAR2 file of each set first, to get hashes early.

Fetch cost: only the first article (its decoded bytes are hashed up to 16 KiB). Article size is not stated here; the hash is only computed when the rule in the Limit bullet above holds.

### 1d. daemon/postprocess/ParParser.cpp / ParParser.h

- Parses PAR2 filenames: lowercases (L75) and trims after the last ".par2" (L85) to derive a set base name. Groups volumes into sets by base name. It does not parse PAR2 packet bodies.

### 1e. daemon/postprocess/DirectUnpack.cpp

Not read in depth (time cap). It calls the unrar machinery during download. Left for a follow-up pass.

## 2. SABnzbd (sabnzbd/sabnzbd, develop)

### 2a. sabnzbd/par2file.py (PAR2 parsing; the most useful module)

- is_par2_file (filepath): reads first 8 bytes, true if they equal `PAR2\x00PKT`.
- analyse_par2 (name, filepath): PROBABLY_PAR2_RE `(.*)\.vol(\d*)[+\-](\d*)\.par2` gives setname, vol, block. If the name does not match, it counts "RecvSlic" occurrences in 128-byte chunks to estimate the block count (worst case counts 1; comment says so).
- parse_par2_file (fname, md5of16k dict):
  - Packet header: 8-byte magic `PAR2\x00PKT`, 8-byte length (must be multiple of 4, 64..file size), 16-byte MD5 over the rest of the packet, 16-byte set id (data[:16]), 16-byte type (data[16:32]). Packets with a bad MD5 are skipped.
  - FileDesc (`PAR 2.0\x00FileDesc`): file id data[32:48], hash16k data[64:80], file length data[80:88] (little-endian u64), name data[88:] (stripped of NUL). Also builds the md5of16k duplicate table.
  - Main (`PAR 2.0\x00Main`): slice size at data[32:40] (u64), number of files at data[40:44] (u32).
  - IFSC (`PAR 2.0\x00IFSC`): file id data[32:48], then 20-byte entries from offset 48: 16-byte MD5 per slice plus a 4-byte CRC32 at +16. The code keeps the CRC32s.
  - Creator packet: logged only (useful when a PAR2 writer has bugs).
  - Early exit: if the file is over 10 MiB (SCAN_LIMIT) and all FileDesc and IFSC entries are seen, stop scanning. Small files are scanned fully.
  - Whole-file CRC32 from slice CRCs: `crc32 = crc32_multiply(crc32, coeff) ^ slice_crc` per full slice, with `coeff = crc32_xpow8n(slice_size)` from the Main packet; the tail is handled with crc32_zero_unpad and crc32_combine. This yields the file's CRC32 (`filehash`) without reading the data. It uses the sabctools C extension for these operations.
  - Duplicate handling: if two files share a hash16k, both are dropped from md5of16k (duplicates list, removed at end). Table is sorted by filename (comment explains the rare identical-file case).
  - On any exception, the table is cleared and set_id None (logged at info).
- Packet hashes are checked with hashlib.md5 over `data`. Note: a corrupt packet is silently skipped.

### 2b. sabnzbd/deobfuscate_filenames.py

- EXCLUDED_FILE_EXTS (L44): `.vob .rar .par2 .mts .m2ts .cpi .clpi .mpl .mpls .bdm .bdmv`.
- decode_par2 (parfile) (L48-82): parses the PAR2 with parse_par2_file, then for each file in the job folder reads the first 16384 bytes (`fileToMatch.read(16384)`), MD5s them, and looks up md5of16k. A match renames the file to the PAR2 name. Traversal-safe: rejects names that escape the directory (L75-79).
- recover_par2_names (filelist) (L85-106): runs decode_par2 on every .par2 in the job folder. This is the post-download (complete file) path.
- is_probably_obfuscated, get_biggest_file, deobfuscate (L109-318): heuristics for obfuscated names. Not read in full.

### 2c. sabnzbd/assembler.py (per-file decision points)

- Article decode and file completion (~L370-385): after a file is fully decoded (file_done), `rarfile.is_rarfile(filepath)` (L379) triggers check_encrypted_and_unwanted and nzo.add_to_direct_unpacker. Otherwise `par2file.is_par2_file(filepath)` (L381-385) calls nzo.handle_par2. So classification is by magic bytes on the complete file, not the name.
- is_cloaked (nzo, path, names) (~L652-678): heuristic for "encrypted via RAR inside RAR". True when the file is `.rar`, its name is a prefix of the inner name with fewer than 8 extra characters, the job has fewer than 3 names, and the name does not match the subtitle regex. Also flags filenames containing "password" that are not a SAFE_EXTS media type.
- check_encrypted_and_unwanted_files (filepath) (~L680-790):
  - Windows device-name guard (has_win_device) before opening RAR (rarfile hangs on special names).
  - `rarfile.is_rarfile` (L693) then `SABRarFile(filepath, part_only=True)` (L694). Reads `needs_password()` and `namelist()`.
  - Password trial: `zf.setpassword(pw)`. RarWrongPassword means try the next; RarCRCError containing "cannot find volume" or "unexpected end of archive" is treated as the password being correct (the next volume is missing). Other exceptions abort the check.
  - Unwanted extension check (has_unwanted_extension) over zf.namelist(). Header-encrypted RARs: setpassword is retried before listing (L~745-750).
  - Exceptions: rarfile.Error (L777) logged at info; other exceptions (e.g. ValueError on bad seek) also caught (L780-782), so the assembler never crashes.
- This shows the intended use of the partial-data trick: only the first volume is needed to list filenames and check needs_password().

### 2d. sabnzbd/misc.py: SABRarFile (L1961-2030)

- Subclass of rarfile.RarFile. In __init__, if `part_only` is set it installs `info_callback` (L1968).
- info_callback (L1973-1981): appends every Rar5FileInfo or Rar3Info that has a filename to the parser's info list and map (keyed by filename without trailing slash). So filenames from a single volume are captured even if the parser stops early. The comment says this fixes multi-volume listing.
- filelist() returns non-directory names. setpassword() resets the parser.
- _verify_file_passwords (L1997-2012): for RAR5 with per-file encryption, checks the password against XENC check value (RAR5_XENC_CHECKVAL) using rar5_check_password (L2015-2025, lru_cache 128, rejects kdf shift above RAR_MAX_KDF_SHIFT).

### 2e. sabnzbd/utils/rarvolinfo.py (volume number and original extension from one file)

- get_rar_extension(myrarfile) (L27-61): `rarfile.RarFile(path, part_only=True, info_callback=headers.append)` collects all header blocks the parser finds.
  - RAR5: volume number = `main.main_volume_number + 1` (absent means 1), extension `partNNN.rar`.
  - RAR3/4: if MAIN has the VOLUME flag, volume number = `endarc.endarc_volnr + 1` (needs the ENDARC block, so needs the end of the volume). Otherwise 1. NEWNUMBERING gives `partNN.rar`; old scheme gives `rar` (vol 1) or `r00`, `r01`, ...
  - Returns (-1, "") on any exception.
- Limit: for RAR3/4 old-style volumes the number is only in ENDARC, which is at the end of the volume. A partial first article cannot give it. Rust indexer should use the NEWNUMBERING flag (available in the first article) and derive the rest from the ordering of the NZB.

### 2f. sabnzbd/nzb/object.py and nzb/file.py (first-article handling)

- first_articles (object.py L200, L320): the first article (lowest partnum) of each file is recorded (L584-585), and first_articles_count tracked.
- Completion (L819-826): when a first article is done and all first articles are done and md5of16k is non-empty, verify_all_filenames_and_resort runs, renaming files by hash16k. Also checks availability of first articles (L1271-1275).
- nzbobject md5of16k (object.py L304): dict hash16k -> name, filled by handle_par2 (L688 passes nzo.md5of16k).
- nzbobject.py L1398-1403: nzf.md5of16k matched against self.md5of16k to produce the new filename.
- Bad-article limit (L1195-1198): if first_articles_count >= 10 and bad/first >= 0.8, the job is failed.
- nzb/file.py: finalize_crc32 (L195-202) combines per-article CRC32s in decodetable order with sabctools.crc32_combine, so a whole-file CRC32 is available after download without re-reading.

### 2g. Filesystem (sabnzbd/filesystem.py)

- Not deeply read. Relevant helpers are get_basename, has_win_device, and the sabctools-backed writers. Left for a follow-up pass.

### 2h. 7z and zip detection

- Not found in the assembler or deobfuscate code paths read. SAB classifies only by `rarfile.is_rarfile` and PAR2 magic. There is no 7z or zip signature check in the modules read. Gap: a 7z/zip grep of the full repo was not run (time cap).

### 2i. sabnzbd/nzb/article.py

- Article object with lowest_partnum, get_article(server, servers) used by the first-article availability check. Not deeply read.

## 3. Other prior art

### PHP rarinfo (dariusii/rarinfo) and NNTmux/nZEDb ArchiveInfo classes

- Not retrieved. Tried raw.githubusercontent.com for dariusii/rarinfo at master and main (root and src/), and NNTmux/newznab-tmux and nZEDb/nZEDb under app/Extensions/util and app/extensions/util. All returned 404. The paths or repo names are probably wrong. The GitHub API is blocked, so the repo layout could not be listed.
- Therefore the requested fields list, SRR block table, and partial-data handling for RAR/zip/7z/PAR2/SFV/SRR were NOT verified. Do not use the SRR layout from this report as a source. Next step: find the correct repo path (search "rarinfo php github" from WebSearch) and fetch RarInfo.php, Par2Info.php, SrrInfo.php.

## Notable tricks (for our indexer)

1. PAR2 hash16k from the first article (NZBGet RenameContentAnalyzer, SAB deobfuscate). Hash the first 16384 bytes of the decoded file. Match against FileDesc hash16k (par2 packet body offset 64..80). Fast way to rename obfuscated files before download finishes.
2. Skip hash16k when article size < 16 KiB on multi-article files (NZBGet L210-211). It is a known limit.
3. PAR2 per-file CRC32 from IFSC slice CRCs with CRC combine (SAB par2file, crc32_xpow8n / crc32_multiply / crc32_combine / crc32_zero_unpad). Lets you get the file CRC32 without the data.
4. PAR2 packet MD5 check before trusting a packet (SAB). Skip bad packets silently.
5. SABRarFile info_callback with part_only=True: capture file headers from one partial volume (SAB misc.py L1973-1981). Works on the first article of a volume.
6. RAR5 volume number from MAIN header (absent = volume 0). RAR3/4 volume number only in ENDARC (SAB rarvolinfo, NZBGet RarReader). Need the end of volume for old-style RAR3.
7. RAR encryption tests: RAR3 MAIN flag 0x0080, RAR5 ENCRYPTION block (type 4). Password check by RAR5 XENC check value (SAB misc.py L2015-2025). "cannot find volume" / "unexpected end of archive" RarCRCError is a correct-password signal when the next volume is missing (SAB assembler).
8. Cloaked RAR detection: .rar whose name is a prefix of the inner file name, few names in job (SAB is_cloaked).
9. RAR5 header block CRC is raw 4 bytes; vint encoding for all other fields (NZBGet ReadV). Use the same for our parser.
10. RAR3 LHD_LARGE 0x0100 flag adds high 32-bit words (NZBGet L~315-330). Check the m_size bug noted in 1a.

## Gaps and limits

- PHP rarinfo library: not retrieved (see section 3).
- 7z and zip: no detection code found in the read modules. Needs a repo-wide grep (raw fetch of specific files only).
- SAB rarfile library (pip `rarfile`) source not fetched. SAB's header parsing for RAR depends on it (rarfile.RarFile, Rar5Parser, Rar3Parser). The library is the real parser for SAB; fetch from github.com/markokr/rarfile for exact field list.
- NZBGet DirectUnpack.cpp and filesystem code not read in depth.
- SAB filesystem.py and nzb/article.py only partly read.
- Line numbers approximate (from develop branch as of fetch time).

## Sources

- NZBGet develop branch, raw files: https://raw.githubusercontent.com/nzbgetcom/nzbget/develop/daemon/postprocess/RarReader.cpp (and .h), .../daemon/postprocess/RarRenamer.cpp (and .h), .../daemon/postprocess/ParParser.cpp (and .h), .../daemon/postprocess/DirectUnpack.cpp, .../daemon/queue/DirectRenamer.cpp (and .h)
- SABnzbd develop branch, raw files: https://raw.githubusercontent.com/sabnzbd/sabnzbd/develop/sabnzbd/par2file.py, .../sabnzbd/deobfuscate_filenames.py, .../sabnzbd/assembler.py, .../sabnzbd/misc.py, .../sabnzbd/utils/rarvolinfo.py, .../sabnzbd/nzb/object.py, .../sabnzbd/nzb/file.py, .../sabnzbd/nzb/article.py, .../sabnzbd/nzb/__init__.py, .../sabnzbd/filesystem.py, .../sabnzbd/postproc.py
- Not available (404): sabnzbd/nzbstuff.py, sabnzbd/utils/rarfile.py (library is external), nzbgetcom/nzbget daemon/postprocess/DirectRenamer.cpp
- Not available (404): dariusii/rarinfo (master, main, src/), NNTmux/newznab-tmux app/Extensions/util/RarInfo.php, nZEDb/nZEDb app/extensions/util/RarInfo.php
