# W5c: Leak-field catalog (RAR4, RAR5, ZIP, 7z, PAR2, SFV)

Purpose: every archive/PAR2 field that leaks uploader or source information (timezone, OS, Windows SID, Unix uid, machine paths, Mark-of-the-Web source URL, comments), so the indexer can link posts by the same uploader or origin.

Conventions:
- `read_from`: `first_article` = inside the first ~700 KB of the volume; `last_article` = needs the tail (central directory, EOCD, 7z encoded header); `full` = needs decompression or the whole file; `separate_small_file` = sidecar (.par2, .sfv, .srr).
- `likelihood`: rough prior that a typical Usenet release carries the field (H/M/L). These are estimates, not measured; UNVERIFIED.
- Tags: **VERIFIED (url, fn)** = read in a fetched primary/near-primary source this session. **VERIFIED-bg** = verified in an earlier background note (w1a/w1c/w2c/w3c), not re-fetched here. **UNVERIFIED** = no source read.
- Line numbers: the WebFetch pass returned function/case names, not line numbers. Where w2c gives approximate lines they are cited as "approx".

---

## 0. Ground rules that change the leak analysis

1. Only two timestamp families exist: local-time (DOS 2-second fields, RAR4 FTIME, ZIP local DOS time) and UTC (RAR5 time, ZIP 0x5455 / 0x000a, 7z FILETIME, RAR4 EXTTIME base is DOS/local). A single archive that carries both a local and a UTC value for the same instant gives the uploader's UTC offset (see section 1.3).
2. "Archive bit 0x20 = untouched Windows file" is a weak inference, not a fact. The archive bit is set on most newly written files on NTFS/FAT and cleared only by backup tools. Treat it as "Windows FS origin likely", L-confidence. UNVERIFIED (no Microsoft source read).
3. The most valuable leaks in practice are the ones that reach a `first_article`: RAR4 FTIME/EXTTIME, RAR5 FILE time + UOWNER + REDIR + service headers, ZIP local headers (DOS + 0x5455 + 0x000a + 0x7875 + 0x000d), 7z is last_article only.

---

## 1. Timezone leaks

| # | Field | format | exact location | read_from | leaks | likelihood | tag |
|---|---|---|---|---|---|---|---|
| 1.1 | RAR4 FTIME (32-bit DOS date/time, 2 s resolution) | DOS local time, no TZ | file header, base fields (after PACK/UNP/HOST_OS/CRC) | first_article | local clock of the packer, no zone. Pair with a UTC field to get offset | H (every RAR4 file) | VERIFIED (libarchive `get_time()` in archive_read_support_format_rar.c: `mktime()`, `tm_isdst=-1`, applies host local zone implicitly) |
| 1.2 | RAR4 EXTTIME (flag 0x1000): 4 nibbles for mtime/ctime/atime/arctime with sub-second byte counts | base for mtime is the DOS time (local). Sub-second bits extend it | after salt, in file header | first_article | packer sub-second precision: fingerprints WinRAR build vs other tools. Its base is local, so it does not give an offset by itself | M (WinRAR writes it for NTFS times; UNVERIFIED which versions) | VERIFIED layout (w2c §3; libarchive `read_exttime()` approx L1886-1975). NS_UNIT constant UNVERIFIED |
| 1.3 | ZIP local-header DOS mtime (u16 time @+10, u16 date @+12) | DOS local time | local file header | first_article | local clock. No TZ | H | VERIFIED (libarchive `zip_read_local_file_header`: `mtime = __archive_dos_to_unix(archive_le32dec(p+10))`). Whether `__archive_dos_to_unix` treats it as local or UTC: UNVERIFIED (body not in fetched excerpt) |
| 1.4 | ZIP 0x5455 "UT" extended timestamp: flags byte, then mtime/atime/ctime as signed LE32 Unix seconds | **UTC** Unix time | local header extra (and central if writer adds it) | first_article (local) / last_article (central) | pairing with 1.3 gives the UTC offset of the packer's clock at write time (DOS 2 s resolution, so offset is ±2 s) | M-H for Info-ZIP-style zips (writer-dependent; 7-Zip writes NTFS 0x000a instead, UNVERIFIED) | VERIFIED (libarchive `process_extra` case 0x5455, approx L907-963 per w2c; `zip_read_local_file_header` sets nsec=0) |
| 1.5 | ZIP 0x000a NTFS: 4 reserved, tag 0x0001 size 24: mtime/atime/ctime as FILETIME (100 ns since 1601) | **UTC** | local header extra | first_article | high-precision UTC instant. Compare to DOS 1.3 for offset; the 100 ns value is an OS fingerprint (Windows-side writer) | M for Windows-made zips | VERIFIED (libarchive case 0x000a, approx L790-864 per w2c; raw FILETIME 0 = absent) and VERIFIED-bg (w1c §1.x) |
| 1.6 | RAR5 FILE mtime (uint32 Unix seconds, or FILETIME uint64 when flag says so) | **UTC** | file header time field, or extra 0x03 (HTIME) record | first_article | UTC instant. Compare to RAR4-style local fields in same set, or to Usenet Date | H (every RAR5 file has mtime) | VERIFIED (libarchive `parse_file_extra_htime`; IS_UNIX 0x01, HAS_MTIME 0x02, HAS_CTIME 0x04, HAS_ATIME 0x08, HAS_UNIX_NS 0x10) |
| 1.7 | RAR5 HTIME nanoseconds (HAS_UNIX_NS 0x10, valid only with IS_UNIX) | UTC, with 32-bit ns per field | HTIME extra | first_article | sub-second precision: Linux `rar` vs WinRAR differ (FILETIME path has ns from conversion). OS fingerprint. Note libarchive honours 0x10 only when IS_UNIX set | M | VERIFIED (libarchive `parse_htime_item`) ; which OS writes which precision UNVERIFIED |
| 1.8 | RAR5 main-archive creation time (main extra 0x02, flags 0x02 "creation time present", 0x04 Unix vs FILETIME, 0x08 ns) | UTC | main header extra | first_article | creation instant of the archive; same leak as 1.6 but for the archive itself | L-M | VERIFIED-bg (w1a §2). Not re-read in libarchive this session |
| 1.9 | 7z kMTime (0x14), kCTime (0x12), kATime (0x13) per file | FILETIME, **UTC**. No local field exists in 7z | file-property records in the (possibly encoded) header | last_article | UTC; combine with the Usenet Date header offset (post-level) to check whether archive times match post time | H for 7z (almost every item) | VERIFIED (7zIn.cpp `ReadHeader()`: `case NID::kMTime/kCTime/kATime` -> `ReadUInt64DefVector`) |
| 1.10 | Usenet Date header offset vs any archive time | post header `Date:` carries uploader's zone (RFC 5322 +hhmm) | outside the archive | first_article | Cross-check: if archive DOS/local value + TZ inferred from UT matches the Date header offset, the packer and poster are the same machine/location. Mismatch = different machine or cloud re-pack | H (any post) | Method only; no source needed |

### 1.11 Derivation rule (how to get the offset)
- Let `L` = RAR4 FTIME or ZIP DOS time (decoded as naive local), `U` = UT/NTFS/RAR5 time (Unix UTC). Offset = `L - U` (seconds, rounded to 2 s because DOS is 2 s). Only valid for the instant of creation, so DST gives two candidate offsets per year; resolve with the Usenet Date month (UNVERIFIED test).
- Needs both fields in the same archive. RAR4 has no UTC field, so RAR4 only gives the offset via EXTTIME if its sub-second pattern exposes a second base (UNVERIFIED), or via Date-header comparison (1.10).
- ZIP is the best target: DOS (local) and UT/NTFS (UTC) appear side by side in the local header of Info-ZIP / WinZip-style writers (UNVERIFIED for 7-Zip).

---

## 2. NTFS alternate streams and Mark-of-the-Web (MOTW)

| # | Field | format | exact location | read_from | leaks | likelihood | tag |
|---|---|---|---|---|---|---|---|
| 2.1 | RAR5 service "STM" (SUBHEAD_TYPE_STREAM) child of a file header (HFL_CHILD 0x20) | UTF-8 stream name in service name field; payload = stream bytes, method 0 usually | service block after the file header | first_article (if the file is in vol 1 and the STM block is inside the first article) | stream name `:Zone.Identifier` and its body: ZoneId, **ReferrerUrl, HostUrl** = the URL the release was downloaded from. Direct origin leak | L-M (only when packer used `-os`; see 2.4) | VERIFIED-bg (w3c §1.6, headers.hpp:118-125 SUBHEAD_TYPE_STREAM = "STM"; arcread.cpp:1518 comment) |
| 2.2 | RAR4 NEWSUB name "STM" (SUBHEAD_TYPE_STREAM); StreamHeader has UnpSize, StreamCRC; data = stream bytes | RAR 3.x+ subblock | subblock after the file header | first_article / last_article | same as 2.1 | L-M | VERIFIED-bg (w1a §NEWSUB; w3c §2.2 headers.hpp). libarchive skips NEWSUB entirely (VERIFIED: "NEWSUB records are metadata-only", consumed) |
| 2.3 | RAR2.0 StreamHead (MHD/StreamHead, methods 0x31-0x35) | legacy | file-level | first_article | legacy stream | L (old) | VERIFIED (unrar win32stm.cpp `ExtractStreams20`) |
| 2.4 | Default behaviour: WinRAR/`rar -os` writes NTFS streams only when `-os` is given (store streams) | switch-dependent | - | - | Presence of STM with Zone.Identifier implies the packer used `-os` (or a tool that does). Default `rar a` does not store it | - | UNVERIFIED for WinRAR GUI default; libarchive does not write RAR. unrar source only reads/propagates |
| 2.5 | Stream body: `[ZoneTransfer]` / `ZoneId=0..4` / optional `ReferrerUrl=` / `HostUrl=` | text INI, CRLF | inside the STM stream data | first_article if stored (method 0) | ZoneId 3 = Internet zone (download). ReferrerUrl/HostUrl = source site | - | ZoneId range 0-4 VERIFIED (unrar motw.cpp `ParseZoneIdStream`: accepts 0-4, rejects duplicate ZoneId=). Meaning of values (3 = Internet) UNVERIFIED (MS docs not fetched) |
| 2.6 | unrar propagation of MOTW on extract (`PROPAGATE_MOTW`) | runtime behaviour | extract path | - | When the archive carries Zone.Identifier, extraction writes it onto extracted files (`CreateZoneIdStream`) only when a `MotwList` mask matches and the command is X/E. `AllFields` decides whether HostUrl/ReferrerUrl are kept; default parser keeps only `[ZoneTransfer]` + `ZoneId=N` (so URLs are dropped by default) | - | VERIFIED (unrar motw.cpp; extract.cpp `ExtractArchiveInit` and `ExtractCurrentFile`, both under `#ifdef PROPAGATE_MOTW`). Whether PROPAGATE_MOTW is defined in shipped WinRAR: UNVERIFIED |
| 2.7 | unrar stream-name validation (`GetStreamNameNTFS`, `IsNtfsProhibitedStream`) | - | - | - | Rejects names with 2+ colons; names must start with ':' and contain no '\\' or '/'. Relevant for indexers that must not trust names like `:Zone.Identifier:$DATA` | - | VERIFIED (unrar win32stm.cpp) |
| 2.8 | 7z alternate streams (7-Zip `-sns`) | 7z kParent / IsAltStream model | 7z header file items | last_article | would carry Zone.Identifier like RAR | UNVERIFIED | Fetched ip7z sources show kParent, IsAltStream and kNtSecure **only in commented-out code** (7zIn.cpp, 7zUpdate.cpp, 7zHandlerOut.cpp). Live ADS support lives in files not fetched (e.g. UI/Common) |
| 2.9 | ZIP alternate streams | none in the format | - | - | No ADS field in ZIP; NTFS extra 0x000a carries times only. Cannot carry Zone.Identifier | - | VERIFIED-absence (libarchive zip reader has no ADS path) ; writers not checked |

---

## 3. Owner, SID and Unix ownership

| # | Field | format | exact location | read_from | leaks | likelihood | tag |
|---|---|---|---|---|---|---|---|
| 3.1 | RAR5 service "ACL" (SUBHEAD_TYPE_ACL, NTFS security descriptor, written with `-ow`) | Windows SECURITY_DESCRIPTOR (self-relative); owner SID | service block (child of file header) | first_article if in the first article | owner SID `S-1-5-21-<machine-or-domain>-<RID>` = Windows machine/domain fingerprint, and RID = account. Same SID across posts = same uploader machine | L (needs `-ow`) | VERIFIED-bg (name "ACL" at headers.hpp:120 per w3c). Layout of the descriptor payload: UNVERIFIED |
| 3.2 | RAR4 "ACL" subheader (NEWSUB, name ACL) | same SD payload | NEWSUB | first_article / last_article | same as 3.1 | L | VERIFIED-bg (w1a §NEWSUB name list); payload UNVERIFIED |
| 3.3 | RAR4 "UOW" subheader (Unix owner) | owner name (<=256), group name (<=256), numeric uid/gid | NEWSUB | first_article | Unix username + uid. Only written by Unix builds | L | Name VERIFIED-bg (w3c headers.hpp:118-125 "UOW"). Field layout VERIFIED-bg (w1a, unrar UnixOwnerName/UnixGroupName). libarchive RAR4 does not parse it (VERIFIED: no uid/gid in archive_read_support_format_rar.c) |
| 3.4 | RAR5 extra 0x06 UOWNER (file header) | flags: 0x01 user name, 0x02 group name, 0x04 uid, 0x08 gid; names as vint length + bytes; uid/gid as vint | file header extra | first_article | Linux/macOS username (`root`, `user`...) and numeric uid (1000 = first user, 0 = root builds) | M for Unix-built rar, zero for WinRAR | VERIFIED (libarchive `parse_file_extra_owner`, EX_UOWNER = 0x06; names capped 255 chars, consumed to end) |
| 3.5 | ZIP 0x7875 "ux" Info-ZIP New Unix: version=1, UIDSize, UID (LE, 2 or 4 bytes), GIDSize, GID | numeric uid/gid | local header and central extra | first_article (local) / last_article (central) | numeric uid/gid of packer; 1000 typical for first Linux user | M for Info-ZIP zips | VERIFIED (libarchive case 0x7875, approx L1172-1206 per w2c; only sizes 2/4 decoded, TODO for others) |
| 3.6 | ZIP 0x5855 "UX" Info-ZIP old Unix: atime, mtime (LE32), uid/gid LE16 if datasize>=12 | UTC Unix seconds + uid/gid | local/central extra | first_article | UTC time (1.4-like) + uid/gid | L | VERIFIED (libarchive case 0x5855, approx L964-986 per w2c) |
| 3.7 | ZIP 0x000d PKWARE Unix: atime, mtime, uid (LE16), gid (LE16), optional device/link tail | local header only | local header extra | first_article | UTC time + uid/gid. Tail (link target) not parsed by libarchive | L | VERIFIED (libarchive case 0x000d, approx L865-886 per w2c) |
| 3.8 | 7z attrib high 16 bits = POSIX st_mode; bit 0x8000 = Unix extension flag | st_mode incl. owner/group/other bits (umask fingerprint) | kWinAttrib (0x15) per file | last_article | Unix host + umask/perm pattern (e.g. 0755 vs 0644 vs 0664). No uid/gid in 7z | M for p7zip/Linux 7z | VERIFIED-decode (7zUpdate.cpp: `if (ui.Attrib & 0x8000) st_mode = ui.Attrib >> 16`). 7zIn.cpp stores raw Attrib, does not decode it (VERIFIED). Encoder side not fetched |
| 3.9 | 7z kNtSecure (0x1A?) security descriptors | SID | file-level secure blocks | last_article | Windows SID (as 3.1) | L | VERIFIED-absence-in-live-code: kNtSecure is inside `/* */` in 7zIn.cpp and in commented-out UpdateItems code (7zHandlerOut.cpp). ID number UNVERIFIED |

---

## 4. Path leaks

| # | Field | format | exact location | read_from | leaks | likelihood | tag |
|---|---|---|---|---|---|---|---|
| 4.1 | RAR5 extra 0x05 REDIR: symlink (type 1 unix, 2 windows), hardlink (type 4); target path | UTF-8 target path | file header extra | first_article | link target = original directory layout of the packer (e.g. `/home/<user>/...`, `C:\Users\<name>\...`). Also a link breaks "largest file = movie" heuristics | L-M | VERIFIED (libarchive `parse_file_extra_redir`; junction type 3 and filecopy type 5 NOT handled, so they are skipped) |
| 4.2 | RAR stored names with directory prefix (`-ep1` not used) | UTF-8 / OEM names | file header name field | first_article | directory tree of packer, e.g. `Downloads\<show>\...` or `home/<user>/...` | M when not `-ep1` | UNVERIFIED for absolute-path capture (WinRAR strips drive letter; behaviour not read) |
| 4.3 | ZIP entry names; backslash separators when host = MSDOS/NTFS | name bytes in local/central header; 0x7075 Unicode path extra (layout UNVERIFIED) | local header name, central name | first_article (local) / last_article (central) | packer path prefix. libarchive normalizes backslash to '/' when system==0 and no '/', so the raw name is what you must keep | M | VERIFIED (libarchive `zip_read_local_file_header` separator normalization). Prefix leak itself: VERIFIED-bg (w1c) |
| 4.4 | 7z names (kName 0x11, UTF-16LE, NUL terminated) | UTF-16 | header kName property | last_article (or first if header unencoded) | packer directory tree; uses '/' from `ReplaceSlashes_OsToUnix`; Windows packers store backslash converted | M | VERIFIED (7zIn.cpp `case NID::kName`) ; 7zHandlerOut `ReplaceSlashes_OsToUnix` VERIFIED |
| 4.5 | PAR2 File Description packet filename (and ParPar `-f` path if given) | UTF-8/ASCII name, packet MD5-checked | PAR2 packet body after File ID | first_article if vol 0 is in article 1; else full | filename as typed when par2 was run; a relative path with directories = packer layout | M | UNVERIFIED (PAR2 spec not re-fetched; w1c §3 background) |
| 4.6 | SFV entries: `FILENAME<ws>CRC32` | plain text | SFV lines | separate_small_file | file names only; no path unless the packer wrote one | M | VERIFIED-bg (w1c §4; Wikipedia SFV fetched there) |

---

## 5. Comments and creator strings

| # | Field | format | exact location | read_from | leaks | likelihood | tag |
|---|---|---|---|---|---|---|---|
| 5.1 | RAR5 CMT service (name "CMT", archive comment) | UTF-8 text, method 0-5 (compressed = needs unpack) | service block after main header, or later | first_article if right after main header and stored; else full | group tags, URLs, release notes, poster handle; the most common free-text leak | M (only if packer added a comment) | VERIFIED-bg (w3c §1.2, arcread.cpp:909-910 MainComment=true; ReadSubData accepts Method 0..5) |
| 5.2 | RAR4 COMM_HEAD (0x75, RAR 2.9-) and NEWSUB CMT (3.x+) | RAR4 comment, optional compression | main header area (old) / NEWSUB (new) | first_article (old) / search (new) | same as 5.1 | M for RAR3-era posts | VERIFIED-bg (w3c §2.3, arcread.cpp:459-465, arccmt.cpp:54) |
| 5.3 | RAR5 main extra 0x02 archive name (flag 0x01) | UTF-8 | main header extra | first_article | original archive name, often a group-tagged name | L | VERIFIED-bg (w1a §2) |
| 5.4 | ZIP EOCD comment (last 22 bytes + comment length @+20) | UTF-8/ASCII | end of central directory | last_article | "Created by ...", packer name/version, release notes | M | VERIFIED-bg (w1c §1.4, §1.13); semantics UNVERIFIED |
| 5.5 | ZIP per-entry comment (central dir @+32, length 2 bytes) | text | central dir | last_article | same class as 5.4 | L | VERIFIED-bg (w1c §1.3) |
| 5.6 | ZIP 0x6375 Info-ZIP Unicode comment (version, CRC32 of standard comment, UTF-8 text) | UTF-8 | central/local extra | last_article | Unicode version of 5.4/5.5 (may differ from ASCII = encoding fingerprint) | L | PARTIAL (w2c §5: tag and purpose verified; layout not field-verified) |
| 5.7 | PAR2 ASCII comment packet `PAR 2.0\0CommASCI` | ASCII text body | PAR2 packet (any file of the set) | first_article if in vol 0 | packer notes; often tool name ("created by ParPar") | M | VERIFIED-bg (w1c §3, table of packet types) |
| 5.8 | PAR2 Unicode comment packet `PAR 2.0\0CommUni` (16-byte MD5 of ASCII comment, UTF-8 body) | UTF-8 | PAR2 packet | first_article if in vol 0 | same as 5.7; MD5 linkage shows whether the unicode text differs from the ASCII text (edited by a second tool) | L | VERIFIED-bg (w1c §3 body layout) |
| 5.9 | SFV header lines starting with ';' (e.g. `; Generated by QuickSFV ... on <date>`, `; Generated by cksfv`, `; Generated by RapidCRC`, `; Generated by pyReScene`) | text | first lines of .sfv | separate_small_file | generator tool, version, date-time with local clock (possibly TZ) | M | UNVERIFIED for exact strings (only `; This is a comment` verified via Wikipedia SFV in w1c) |
| 5.10 | SRR (ReScene) stored fields: file names, sizes, dates, attributes, comments | binary blocks | .srr file | separate_small_file | a full dump of the source release metadata (dates, attributes, comments), typically 10-20 KB | M for scene-style releases | VERIFIED-bg (w1c §4 and rescene.wikidot.com fetched there). Block-level fields UNVERIFIED |
| 5.11 | NFO text file (scene release notes; packer name, group, date, sometimes URL) | plain text, CP437 | separate file or inside archive | full (inside archive) / separate | group/handle, release date, URLs, ASCII art fingerprint | H for scene releases | UNVERIFIED (no NFO spec fetched; standard practice) |

---

## 6. Attribute and host-OS leaks

| # | Field | format | exact location | read_from | leaks | likelihood | tag |
|---|---|---|---|---|---|---|---|
| 6.1 | RAR4 HOST_OS byte (0 MSDOS, 1 OS2, 2 WIN32, 3 UNIX, 4 MAC_OS, 5 BEOS) | uint8 | file header, base (at HOST_OS offset) | first_article | OS of the packer per file. Also changes how ATTR is read | H | VERIFIED (libarchive `read_header()` OS_* macros, archive_read_support_format_rar.c) |
| 6.2 | RAR4 ATTR for WIN32 host = Windows attributes (0x10 dir, 0x01 RO, 0x02 hidden, 0x04 system, **0x20 archive**); for UNIX host = full 32-bit st_mode | uint32 | file header ATTR field | first_article | Windows vs Unix build; archive bit as weak Windows-FS hint (see section 0.2) | H | VERIFIED (libarchive: UNIX/MAC/BEOS use stored attr as mode; MSDOS/OS2/WIN32 synthesize; fflags from ATTR_READONLY/HIDDEN/SYSTEM). 0x20 interpretation UNVERIFIED as "untouched" |
| 6.3 | RAR5 HostOS vint in file header (0 = Windows, 1 = Unix) | vint | file header | first_article | OS of packer | H | VERIFIED-bg (w3c §1.1, arcread.cpp:852-853, headers5.hpp:10-22) |
| 6.4 | ZIP "version made by" host byte (upper byte of central version-made-by @+4; 0 MSDOS, 3 UNIX, 10 NTFS, 11 VFAT...) | uint8 | central directory @+5 | last_article | packer OS + spec version (lower byte). Very strong packer fingerprint | H | VERIFIED-bg (w1c §1.3, "upper byte = host OS, verified, fetched"). Caution: libarchive's `zip_entry->system = p[5]` in the **local** header reads the upper byte of "version needed", not version-made-by; do not treat it as host OS (VERIFIED-by-reading of libarchive `zip_read_local_file_header`) |
| 6.5 | ZIP external attributes (central @+38, 4 bytes): low byte DOS (0x10 dir, 0x01 RO, 0x20 archive); if host=3 high 16 bits = st_mode | uint32 | central dir @+38 | last_article | same as 6.2 + Unix mode (umask/perm) | H | VERIFIED (libarchive `case 0x6c78` keyed on system: 3 -> `external_attributes >> 16`; 0 -> dir/0664 or 0775 rules; other -> mode 0). Offset @+38 VERIFIED-bg (w1c §1.3) |
| 6.6 | 7z kWinAttrib (0x15) per file: Windows attribute bits, plus 0x8000 Unix flag and st_mode in bits 16-31 | uint32 | header kWinAttrib | last_article | Windows vs Unix build, archive bit, hidden/system bits, Unix perms | H | VERIFIED (7zIn.cpp `case NID::kWinAttrib` reads defined-vector + `Read_UInt32_Vector`); 0x8000 decode VERIFIED (7zUpdate.cpp). w1b flags a spec inconsistency about 0x8000; use 0x8000 = Unix flag |
| 6.7 | 7z kDummy (0x19) padding | bytes that must be zero | header | last_article | padding should be zero; nonzero padding = non-7-Zip writer (L-confidence) | L | VERIFIED (7zIn.cpp `case NID::kDummy`: sets header error on nonzero bytes). Writer-fingerprint use UNVERIFIED |
| 6.8 | 7z kComment (0x16) | text (if present) | header | last_article | packer comment | L | VERIFIED-absence-in-handling: 7zIn.cpp has no case, so it is skipped with an unsupported-feature warning. Presence in real 7z UNVERIFIED |
| 6.9 | RAR4 mtime fallback without EXTTIME, plus DOS 2 s rounding | DOS local | file header | first_article | 2 s rounding = DOS-origin fingerprint (exact seconds only for RAR5/EXTTIME) | M | VERIFIED-bg (w1a §file-time) and libarchive get_time (2 s) |

---

## 7. Cross-check table (how fields combine to link posts)

| Combo | Gives | Needs |
|---|---|---|
| ZIP DOS (1.3) + UT or NTFS (1.4 / 1.5) same entry | UTC offset of packer (±2 s) | local header, both fields |
| RAR5 mtime (1.6) + Usenet Date offset (1.10) | Is packer clock in the same zone as poster? | first_article, Date header |
| RAR5 HTIME ns (1.7) + RAR5 HostOS (6.3) | Build/OS fingerprint | first_article |
| RAR5 UOWNER (3.4) username + uid | Same Unix account across posts | first_article |
| 7z kWinAttrib 0x8000 st_mode (6.6) + ZIP host 3 mode (6.5) | Unix packer, umask / perms | last_article |
| STM Zone.Identifier HostUrl (2.5) | Download origin site | first_article, only if the packer used -os |
| SID from ACL (3.1) | Windows machine/account across posts | first_article, only if `-ow` |
| EOCD comment (5.4) / CMT (5.1) / PAR2 CommASCI (5.7) | Shared group tag or packer string | last_article / first_article |

---

## 8. Open items (UNVERIFIED, next steps)

- ZIP `__archive_dos_to_unix()` local vs UTC (libarchive body not in excerpt). Fetch the remaining part of archive_read_support_format_zip.c or APPNOTE.TXT section 4.4.
- 7-Zip ZIP writer: does it emit 0x5455 or only 0x000a? Check ip7z/7zip CPP/7zip/Archive/Zip/ZipUpdate.cpp.
- 7-Zip live ADS encoder (`-sns`): search ip7z/7zip for `IsAltStream` in UI/Common/UpdateCallback.cpp and 7zUpdate paths beyond the excerpt.
- unrar `PROPAGATE_MOTW` define in shipped builds (pmachapman/unrar makefile/CMake).
- RAR5 ACL payload and UOW/ACL RAR4 layouts: unrar arcread.cpp ReadSubData / headers.hpp (not fetched).
- Zone.Identifier ZoneId meanings: Microsoft "Attachment Manager" docs (not fetched).

---

## Sources

VERIFIED sources fetched this session (raw.githubusercontent.com; line numbers not returned, function/case names cited):
- https://raw.githubusercontent.com/libarchive/libarchive/master/libarchive/archive_read_support_format_rar5.c (fns: `process_head_file_extra`, `parse_file_extra_owner`, `parse_file_extra_htime`, `parse_htime_item`, `parse_file_extra_redir`, `process_head_service`, `process_main_locator_extra_block`; enum EX_*)
- https://raw.githubusercontent.com/libarchive/libarchive/master/libarchive/archive_read_support_format_rar.c (fns: `read_header()` OS_* switch, `get_time()`, `read_exttime()`, `process_head_file` / NEWSUB / COMM skip paths)
- https://raw.githubusercontent.com/libarchive/libarchive/master/libarchive/archive_read_support_format_zip.c (fns: `process_extra` cases 0x5455, 0x7875, 0x5855, 0x000d, 0x000a absent, 0x6c78 system-keyed mode; `zip_read_local_file_header`)
- https://raw.githubusercontent.com/pmachapman/unrar/master/win32stm.cpp (fns: `ExtractStreams`, `ExtractStreams20`, `GetStreamNameNTFS`, `IsNtfsProhibitedStream`; #ifdef _WIN_ALL)
- https://raw.githubusercontent.com/pmachapman/unrar/master/motw.cpp (class MARK_OF_THE_WEB: `ReadZoneIdStream`, `ParseZoneIdStream` (ZoneId 0-4), `CreateZoneIdStream`, `IsNameConflicting`, `IsFileStreamMoreSecure`)
- https://raw.githubusercontent.com/pmachapman/unrar/master/extract.cpp (fns: `ExtractArchiveInit`, `ExtractCurrentFile`, `SetFileAttr` call sites; #ifdef PROPAGATE_MOTW)
- https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Archive/7z/7zItem.h (CFileItem fields; IsAltStream commented out)
- https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Archive/7z/7zIn.cpp (fns: `CInArchive::ReadHeader()`; kName, kWinAttrib, kMTime/kCTime/kATime, kDummy, kNtSecure in comment, kParent in comment; kComment via default)
- https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Archive/7z/7zUpdate.cpp (0x8000 check and `st_mode = Attrib >> 16`; secure/altstream code commented)
- https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Archive/7z/7zHandlerOut.cpp (kpidAttrib pass-through; kpidNtSecure, kpidIsAltStream commented)

VERIFIED-bg (earlier background notes in docs/research/archive-metadata/raw/):
- w1a-rar.md (RAR4/RAR5 header layouts, service names, CMT/ACL/STM/UOW, EXTTIME, main extra 0x02)
- w1b-7z.md (7z property IDs, attribute bit notes, discrepancy list in §13)
- w1c-zip-par2-sfv.md (ZIP central/local layout, extra IDs, EOCD comment, PAR2 CommASCI/CommUni, SFV, SRR)
- w2c-verified-layouts.md (approx line numbers for libarchive ZIP extras; RAR4 EXTTIME layout; PARTIAL 0x6375)
- w3c-rr-qo-srs.md (unrar arcread.cpp:909-910, arcread.cpp:459-465, arccmt.cpp:54, headers.hpp:118-125, headers5.hpp:10-22, arcread.cpp:852-853, arcread.cpp:1518)

Not fetched (UNVERIFIED): Microsoft Zone.Identifier/Attachment Manager docs; PAR2 specification; APPNOTE.TXT (ZIP); NFO spec; SFV generator header strings.
