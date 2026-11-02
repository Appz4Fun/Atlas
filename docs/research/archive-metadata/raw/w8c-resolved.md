# W8c: resolution of priority UNVERIFIED items

Sources fetched this pass (raw.githubusercontent.com, 200 OK unless noted):
- unrar (pmachapman/unrar master): arcread.cpp, timefn.hpp, timefn.cpp, recvol.hpp, recvol3.cpp, recvol5.cpp, crypt3.cpp, crypt5.cpp, headers.hpp, headers5.hpp, win32acl.cpp, win32stm.cpp, sha1.cpp, unicode.cpp
- libarchive: archive_read_support_format_zip.c, _rar.c, _rar5.c
- 7-Zip (ip7z/7zip main): 7zIn.cpp, 7zDecode.cpp, 7zEncode.cpp, 7zHeader.h, 7zAes.cpp, ZipItem.h, ZipIn.cpp, ZipAddCommon.cpp
- Info-ZIP extrafld.txt (LuaDist/zip)
- NOT fetched: APPNOTE.TXT (pkware.cachefly.net returned 403 CONNECT tunnel). rarlab.com and web.archive.org not attempted (blocked per brief).

## Step 1: open items
Grep of raw/*.md found 293 UNVERIFIED lines, 283 distinct after dedupe. Full deduped list: /home/claude/scratchpad/w8c/unverified-dedupe.txt

## Priority items

### a. RAR4 EXTTIME remainder -> ns: RESOLVED
- arcread.cpp ~L392-425 (Archive::ReadHeader, LHD_EXTTIME branch). Two-byte flags word; nibble per time at shift (3-I)*4 for I = mtime, ctime, atime, arctime. Per nibble: 0x8 = present, 0x4 = add 1 s to the DOS second, low 2 bits = count of extra bytes. ctime/atime (I != 0) read their own 4-byte DOS time first.
- Remainder: bytes accumulated as Reminder |= byte << ((J+3-count)*8), i.e. the count bytes sit in the top of a 24-bit field, LE. Then Reminder *= REMINDER_PRECISION/10000000 = 100, so the accumulated value is in 100 ns ticks and is converted to ns.
- timefn.hpp:21 TICKS_PER_SECOND = 1000000000 (internal ns); timefn.hpp:40 REMINDER_PRECISION = TICKS_PER_SECOND.
- Caveat: the 24-bit field in 100 ns ticks tops out at ~1.68 s, which fits the 2-second DOS granularity, so the remainder is the sub-2-second offset. This is my reading of the code, not an explicit comment; check on a real fixture.

### b. RAR4 recovery record: layout RESOLVED, percent formula STILL UNVERIFIED
- RAR4 RR is HEAD3_PROTECT (0x78), not a NEWSUB. arcread.cpp:466-474: DataSize u32, Version u8, RecSectors u16, TotalBlocks u32, Mark[8]; NextBlockPos += DataSize. headers.hpp:331-337 ProtectHeader. SIZEOF_PROTECTHEAD = 26 (recvol3 / headers, 7-byte block header + 19 bytes).
- The 8-byte Mark is read but never compared in these sources. The literal "Protect!" string is not present in the fetched code, so the mark value is UNVERIFIED.
- RecSectors / TotalBlocks are stored but not used to compute a percentage anywhere in the fetched sources (recvol3.cpp does the decode only). Percent formula: STILL UNVERIFIED. Candidate RecSectors/TotalBlocks is not confirmed.
- NEWSUB "RR" subheader in RAR4: HEAD3_SERVICE (0x7A) with name "RR" (headers.hpp:124 SUBHEAD_TYPE_RR). arcread.cpp ~L360-385 stores optional SubData; RAR4 RR percent is not parsed from it. The RR data area is the recovery sectors, skipped via PackSize. Layout of RR-specific fields inside the RAR4 service header: STILL UNVERIFIED beyond the HEAD3_PROTECT layout above.
- Also arcread.cpp:87 Archive::SearchSubBlock(SUBHEAD_TYPE_RR) scans for the RR service block; a locator extra can give RROffset.

### c. RAR5 .rev signature: RESOLVED
- recvol.hpp:4-5: `#define REV5_SIGN "Rar!\x1aRev"` and `REV5_SIGN_SIZE 8`. So the 8 bytes are 52 61 72 21 1A 52 65 76.
- recvol5.cpp:446-470 RecVolumes5::ReadHeader: sign (8), BlockCRC u32 LE, HeaderSize u32 (bounded 5 < size <= 0x100000), CRC32 over the size field plus header, version byte must be 1, DataCount u16, RecCount u16, RecNum u16 (must be < TotalCount), RevCRC u32.
- recvol5.hpp is a 404 page in the repo, so the header file itself was not fetched. The value comes from recvol.hpp.

### d. RAR5 ACL and STM service data, and RR: RESOLVED (partial)
- ACL: win32acl.cpp:70-82. The service's data is read with ReadSubData (decompressed) into a buffer and cast to a Windows SECURITY_DESCRIPTOR (self-relative, binary). OWNER|GROUP|DACL (+SACL if privileged). No fixed header in the RAR5 ACL data. RAR4 ACL path (win32acl.cpp:40-53) uses an EA-style CRC check.
- STM (named streams, e.g. Zone.Identifier): win32stm.cpp:233-245 GetStreamNameNTFS. RAR5 (non-RARFMT15) stream name = UTF-8 bytes in SubData (UtfToWide). RAR4 (RARFMT15) = RawToWide. Stream content is decompressed via ReadSubData; checked against StreamCRC (win32stm.cpp:104). The name lives in SubData, not a separate length field.
- RR (RAR5 service): arcread.cpp:913-919. The SubData is a vint RecoveryPercent. The comment says it was a single byte up to RAR 6.02 and a vint from 6.10. The data area holds the recovery sectors.

### e. zip 0x6375 layout and WinZip AES 0x9901 method width: RESOLVED
- 0x6375: extrafld.txt, "Info-ZIP Unicode Comment Extra Field" (~L1296-1320). Layout: TSize u16, Version u8 = 1, ComCRC32 u32 (CRC32 of the central-directory comment field), UnicodeCom = remainder, UTF-8 without BOM. If the CRC does not match, the extra must be ignored and the plain comment used.
- 0x9901 (WinZip AES): 7 bytes data. VendorVersion u16 LE (1 = AE-1 with CRC, 2 = AE-2 no CRC). Vendor ID "AE" at offsets 2-3. Strength u8 at offset 4. Actual method u16 LE at offset 5-6. Evidence: ZipItem.h:43 k_WzAesExtra_Size = 7; ZipItem.h:50-66 CWzAesExtra::ParseFromSubBlock reads GetUi16(p+5), with the comment "9.31: The BUG was fixed". ZipItem.h:73-80 SetSubBlock writes the same layout. libarchive archive_read_support_format_zip.c:~1208-1225 reads only one byte (p[offset+5]), so it is incorrect for 2-byte methods.
- APPNOTE was not fetched (403), so the 0x9901 entry in APPNOTE's header-ID table is UNVERIFIED here. 7-Zip is used as the primary evidence.

### f. RAR5 HTIME flags and file flags: RESOLVED
- headers5.hpp:33-35 FHFL_DIRECTORY 0x0001, FHFL_UTIME 0x0002 (time field is Unix format), FHFL_CRC32 0x0004 (CRC32 present), FHFL_UNPUNKNOWN 0x0008 (unknown unpacked size). Also headers5.hpp:95-99 FHEXTRA_HTIME_UNIXTIME 0x01 (use Unix time_t), MTIME 0x02, CTIME 0x04, ATIME 0x08, UNIX_NS 0x10 (nanosecond flag). The FHEXTRA_HTIME_UNIX_NS bit is read in arcread.cpp:1153-1161 as an ns field, 0x3FFFFFFF masked, <1e9.

### g. RAR4 sha1_process_rar29 vs plain SHA-1: RESOLVED (partial)
- crypt3.cpp:1-56 SetKey30: RawPsw = UTF-16 code units (2 bytes/char, WideToRaw), then the 8-byte salt appended. Loop of HashRounds = 0x40000 iterations. Each iteration: sha1_process_rar29(c, RawPsw, RawLength), then the 3-byte little-endian counter I (PswNum[0..2]) via sha1_process. Every HashRounds/16 rounds, digest[4]'s low byte is saved into AESInit[] (the IV source).
- The KDF therefore uses sha1_process_rar29, not sha1_process, for the password+salt part. sha1.cpp:146 defines sha1_process_rar29 in the repo file. The difference from standard SHA-1 is inside SHA1Transform (the rar29 flag). I did not read that flag's body, so the exact difference is UNVERIFIED. The per-round counter and the 0x40000 count are VERIFIED.

### h. 7z kComment / kDummy and 7z AES password encoding: RESOLVED
- 7zHeader.h:86 NID::kComment (enum value 22 = 0x16), kDummy = 25 (0x19), kEncodedHeader = 23.
- 7zIn.cpp:1439-1445 kDummy: skips `size` bytes and sets ThereIsHeaderError if any byte is nonzero (padding). kComment: no explicit case in 7zIn.cpp; it falls into `default:` (L1492) with addPropIdToList = isKnownType = false, so it is skipped as an unknown property.
- 7z AES password: 7zEncode.cpp:226-236 and 7zDecode.cpp:436-452 both write each UTF-16 code unit low byte first (LE), 2 bytes per char, into the buffer given to CryptoSetPassword. Password is UTF-16LE, no BOM, no NUL.

### i. RAR host OS values: RESOLVED
- RAR4: headers.hpp:94-100 HOST_MSDOS=0, HOST_OS2=1, HOST_WIN32=2, HOST_UNIX=3, HOST_MACOS=4, HOST_BEOS=5. arcread.cpp:301-306 maps UNIX/BEOS to HSYS_UNIX and others to HSYS_WINDOWS.
- RAR5: la_rar5.c:1858-1860 HOST_WINDOWS=0, HOST_UNIX=1 (libarchive, decoded at L2041-2089).

## Remaining UNVERIFIED after this pass
- b: RAR4 recovery percent formula; "Protect!" mark value; RR-specific fields inside the RAR4 service header.
- g: exact body difference between sha1_process_rar29 and standard SHA-1 (sha1.cpp SHA1Transform).
- e: APPNOTE's 0x9901 entry (APPNOTE not fetched, 403).
- Not in this pass: RAR5 KDF PBKDF2 details beyond crypt5.cpp (not re-read), RAR4 -hp per-block salt layout beyond arcread.cpp SIZE_SALT30 (not re-read), 7z NID byte values beyond the enum order.
