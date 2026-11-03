# W9b: archive and packer signal table

Scope: archive and packer metadata only (RAR4, RAR5, 7z, zip, PAR2, SFV, SRR, .rev, SRS). Container and media tags are out of scope here.

Key:
- Cost tiers: free (NZB/overview), first article (article 1 of a volume, about 700 KB), last article (final article of a volume), first+last, separate small file (PAR2, SFV, SRR, .rev index area), full download.
- Strength: exact (byte-level or CRC/size match), strong (rarely collides, needs one more check), weak (candidate only).
- Caveats are in the notes column. "Verified" means read in a fetched source or reproduced on a fixture. Fixtures are self-made or from libarchive/rars test sets, not real Usenet posts.

## Signal table

| Signal | Format | Proves | Cost tier | Strength | Notes |
|---|---|---|---|---|---|
| Ordered NZB segment byte sizes | RAR/7z/zip volumes | twin | free (NZB/overview) | strong | Same list means same yEnc split (part size and line length). An equal total alone is weak. |
| Volume total size (sum of NZB segment bytes) | any split | twin | free (NZB/overview) | weak | Necessary condition only. Use it to prune candidates before fetching. |
| yEnc =yend pcrc32 of part 1 | RAR/7z/zip volume | twin | first article | exact | Proves the bytes of part 1 only, not the whole volume. |
| RAR5 HEAD_CRYPT salt and check value (-hp) | RAR5 volumes | twin (set) | first article | exact | Identical in every volume of one set (rars writer verified; WinRAR unverified). Random per archive. |
| RAR4 -hp 8-byte block salt | RAR4 with -hp | twin | first article | exact | Random per block. No check value, so a match needs a decrypt and CRC test. |
| 7z start header NextHeaderCRC and NextHeaderSize | 7z | twin | first article | strong | Equal only if the header bytes match (same tool, settings and file order). Start header is 32 bytes at offset 0. |
| yEnc whole-file crc32 and size from last part =yend | RAR/7z/zip volume | twin | last article | exact | Only the last part carries it (yEnc 1.3). Matches the SFV and PAR2 CRC of the same file. |
| 7z AES IV in encoded-header coder props | 7z with -mhe or password | twin | last article | exact | Per-archive random (16 bytes). A re-pack gets a new IV. The StreamsInfo is plaintext even with header encryption. |
| Combined whole-volume CRC from all pcrc32 and ypart lengths | RAR/7z/zip volume | twin | full download | exact | Pure arithmetic once every part tail is read. Redundant with the last-part crc32 row, useful as a cross-check. |
| PAR2 FileDesc full-file MD5 and length | RAR/7z/zip volume | twin | separate small file | exact | Survives renames. Covers only files that the PAR2 set protects. |
| PAR2 IFSC per-slice MD5 and CRC32 | any file in a PAR2 set | twin | separate small file | strong | Slice-level match finds sub-file reuse when slice size agrees. |
| PAR2 IFSC CRC32 combined to whole-file CRC32 | any file in a PAR2 set | twin | separate small file | exact | crc32 combine with zero-unpad algebra matched zlib on a 3 MB test (9 slices). Needs slice size. |
| PAR2 FileDesc 16 KiB MD5 with exact length | any | twin (candidate) | separate small file | weak | Volumes from one packer share the first 16 KiB header and collide. Require the full MD5 too. |
| PAR2 Recovery Set ID | any PAR2 set | twin | separate small file | strong | Name-bound: any rename changes it. Same names and same bytes give the same RSID. |
| SFV CRC32 per volume | RAR/7z/zip volume | twin | separate small file | exact | 32 bits only. Pair with the NZB size to limit collisions across a large set. |
| RAR3 .rev trailer ordinals and CRC32 per data volume | RAR3 volumes | twin | separate small file | exact | Whole .rar volume CRC without downloading the .rar. Needs the recovery volume to exist. |
| RAR5 .rev header FileSize and CRC per data volume | RAR5 volumes | twin | separate small file | exact | Read the header area only. Gives size and CRC per volume. |
| SRR OSO hash block 0x6B (size, hash, name) | stored files in SRR | twin | separate small file | strong | Hash algorithm not confirmed in this pass. Check the SRR spec before relying on it. |
| RAR4 file header CRC32 and unpacked size | RAR4 | alt | first article | exact | Inner-file key. Split files carry the whole-file CRC in the last piece header. |
| RAR5 file header CRC32 and unpacked size (flag 0x0004) | RAR5 | alt | first article | exact | Same split caveat as RAR4: take the last piece for split files. |
| RAR5 BLAKE2sp file hash (extra 0x02) | RAR5 | alt | first article | exact | Only for a file wholly inside one volume. On a split first part it covers that volume only, so it is not a key there. |
| ZIP local header CRC32 and size | ZIP | alt | first article | exact | Zero when GP bit 3 is set (then use the central directory) and zero for WinZip AE-2. |
| MD5 of the first 64 KiB of decoded inner bytes | any stored or compressed | alt | first article | strong | Works on stored data, zip deflate, 7z LZMA/LZMA2 (about 1:1) and RAR5 (64 KiB steps). RAR 2.9 only in 1 MiB steps. Common intros collide, so pair with size. |
| ZIP central directory CRC32 and sizes | ZIP | alt | last article | exact | Authoritative for entries that use a data descriptor. |
| 7z substream CRC32 and size per inner file | 7z | alt | last article | exact | Needs the encoded header decoded. Lost for a -mhe header without the password. |
| SRS sample CRC32 and sample size | SRS for MKV/MP4 samples | alt (sample only) | separate small file | strong | Alt key for the sample file only, never for the feature film. |
| RAR5 UOWNER user, group, uid and gid | RAR5 from Unix builds | same uploader | first article | strong | Account name plus uid is a machine key. Absent from Windows builds. |
| RAR4 UOW owner record | RAR4 Unix host | same uploader | first article | strong | Unix host only. |
| RAR5 ACL owner SID (SECURITY_DESCRIPTOR) | RAR5 Windows with ACL | same uploader | first article | strong | Descriptor is compressed and its layout is partly unverified. Machine or domain SID. |
| ZIP local DOS time vs 0x5455 UT (UTC) | ZIP | same uploader | first article | strong | Gives packer UTC offset to about 2 s. DST leaves two candidate offsets. |
| ZIP local DOS time vs 0x000a NTFS (UTC) | ZIP from a Windows writer | same uploader | first article | strong | Same offset method. The 100 ns values also fingerprint the writer OS. |
| ZIP 0x7875 uid and gid | ZIP from Info-ZIP | same uploader | first article | strong | Unix Info-ZIP writers only. |
| RAR4 FTIME (local) vs Usenet Date header offset | RAR4 | same uploader | first article (+ free Date) | medium | Date offset is the poster clock. A cloud re-pack or another clock breaks it. |
| 7z st_mode (attribute 0x8000 Unix extension) | 7z | same uploader | last article | weak | Shows Unix origin only. 7z has no uid, so it is not a user key. |
| RAR5 STM Zone.Identifier HostUrl and ReferrerUrl | RAR5 packed with -os | same uploader | first article | weak | Download origin, not the packer. Present only with stream storage. |
| RAR4 and RAR5 archive comment (CMT) | RAR | same uploader | first article | medium | Free text, writer-set. Check the tail too. |
| ZIP archive and entry comments | ZIP | same uploader | last article | medium | Free text in EOCD and central directory. |
| PAR2 CommASCI and CommUni comment packets | PAR2 | same uploader | separate small file | medium | Free text, present only if the poster set a comment. |
| RAR4 host OS byte (0 DOS, 2 Win32, 3 Unix, 4 macOS) | RAR4 | tool | first article | strong | Linux rar writes 3, WinRAR Win32 writes 2. |
| RAR5 host OS byte (0 Windows, 1 Unix) | RAR5 | tool | first article | strong | Only two values in the writer tables. |
| RAR4 method byte (0x30 store, 0x31 to 0x35 compressed) | RAR4 | tool | first article | strong | 0x30 is store, the ngPost default. Inner bytes are readable directly. |
| RAR5 comp_info method bits (0 = store) and dict bits | RAR5 | tool | first article | strong | Method 0 is store. Dict 128 KiB << N shows the writer setting. |
| RAR4 UnpVer byte (20 or 29) | RAR4 | tool | first article | medium | Minimum algorithm version. It reflects the method, not the exact build. |
| RAR5 comp_info version bits (1 = RAR 7.0+ algorithm) | RAR5 | tool | first article | medium | The version field is 0 almost always. |
| RAR4 EXTTIME sub-second nibbles | RAR4 | tool | first article | medium | Seen in Linux rar output (fixture inference). WinRAR use is unverified. |
| RAR4 DOS-only FTIME without EXTTIME | RAR4 | tool | first article | weak | Read as WinRAR 3.x from fixtures. Not checked against real WinRAR output. |
| RAR5 HTIME with Unix flag and UNIX_NS (0x10) | RAR5 | tool | first article | medium | Nanosecond precision marks the Unix writer path. The FILETIME path differs. |
| RAR5 HEAD_CRYPT present and Lg2Count | RAR5 with -hp | tool | first article | strong | Lg2Count is the KDF cost the writer chose. Reveals the encryption setting. |
| RAR4 PROTECT block (0x78) present | RAR4 | tool | first article | medium | Recovery record present. Percent formula still unverified. |
| RAR main-header solid flag | RAR4 and RAR5 | tool | first article | medium | Solid set: later files are unreachable from article 1. |
| ngPost RAR shape (-ep1 base names, method 0) | RAR4/RAR5 | tool | first article | strong | Matches the ngPost default rar a -idp -ep1 -m0. |
| 7z signature version byte (0.2, 0.3, 0.4) | 7z | tool | first article | medium | 0.4 with ARM64 filter IDs points to newer 7-Zip (fixture inference). |
| 7z coder Copy (0x00) vs LZMA2 (0x21) | 7z | tool | last article | strong | Copy is -mx0, the ngPost default. Inner bytes sit at offset 32. |
| 7z filter IDs (BCJ, BCJ2, ARM64 0x0A, RISCV 0x0B, Delta) | 7z | tool | last article | strong | Shows exe-like detection and a newer 7-Zip build. |
| 7z LZMA props byte (0x5D default) | 7z | tool | last article | medium | Non-default props point to explicit settings. |
| 7z AES NumCyclesPower (19 default) | 7z with -mhe or password | tool | last article | medium | 19 matches the 7-Zip default. Other values point to another writer. |
| ZIP version-made-by host byte | ZIP | tool | last article | strong | Use the spec table only. The libarchive host table had bugs. |
| ZIP 0x5455 together with 0x7875 in local header | ZIP | tool | first article | strong | Info-ZIP writer signature. |
| ZIP 0x9901 AE-1 vs AE-2 and real method | ZIP WinZip AES | tool | first article | strong | AE-2 writes CRC 0. Real method is u16 at offset 5 (libarchive reads one byte, a bug). |
| PAR2 creator "Created by par2cmdline version X." | PAR2 | tool | separate small file | strong | Format verified in source. Version literal comes from a test fixture only. |
| PAR2 creator "ParPar v<ver> <arch> [url]" | PAR2 | tool | separate small file | strong | Also leaks the CPU arch (weak same-uploader hint). |
| PAR2 block count and redundancy (par2cmdline 2000 blocks, 5%) | PAR2 | tool | separate small file | medium | ngPost default is 0 percent PAR2. |
| SRR app name block 0x69 | SRR | tool | separate small file | strong | App string as the SRR writer set it. |
| Volume and yEnc file names | any | none | free (NZB/overview) | none | Rewritten by obfuscation and reposts. Never a key. |
| Volume count and size pattern | any | none | free (NZB/overview) | weak | Structure only. Same-film candidate at most. |
| RAR5 volume-number flag absent in part 1 | RAR5 | none | first article | strong | Detects part 1 without the file name. Not a link key. |
| Inner container type (EBML DocType, ISO PVD, zip or RAR magic) | any | none | first article | weak | Type only. Many films share a container. |
| Duration, resolution and codec from partial decode | any compressed or stored | none (same film) | first article | weak | Same-film candidate only. Never a twin. |
| Password check values (RAR5 PBKDF2, ZipCrypto 1 byte, AES 2 byte) | encrypted sets | none | first article | weak | Tests a guessed password, not a link. ZipCrypto has 1/256 false positives. |

## Top 12 to collect first

| Rank | Idea | Value | Effort | Extra bandwidth per release | CPU per release |
|---|---|---|---|---|---|
| 1 | Ordered NZB segment sizes: free, prunes twin candidates before any fetch | High | Trivial | 0 | Negligible |
| 2 | RAR4/RAR5 inner file CRC32 and unpacked size from article 1: exact alt key, parser already reads headers | High | Low | 0 (first article already fetched) | Negligible |
| 3 | PAR2 FileDesc full MD5 and length from the index PAR2: exact twin key that survives renames | High | Low | One small PAR2 file | Negligible |
| 4 | yEnc last-part =yend crc32 and size: exact whole-volume twin key | High | Low to medium | One last article per volume | Negligible |
| 5 | RAR5 HEAD_CRYPT salt and check: set-level key for encrypted posts | High for encrypted sets | Low | 0 | Negligible |
| 6 | 7z start header NextHeaderCRC and size from article 1: strong twin and alt hint for 7z | Medium to high | Low | 0 | Negligible |
| 7 | Packer tool fingerprint from article 1 (host OS, method, dict, signature): filters candidates by writer | Medium | Low | 0 | Negligible |
| 8 | MD5 of first 64 KiB decoded inner bytes: alt key via partial decode, pair with inner size | High | Medium to high (decoders needed) | 0 | One article decode per release |
| 9 | ZIP central directory CRC and sizes: exact alt key for zip, authoritative when data descriptors are used | High for zip | Low | One tail article per volume | Negligible |
| 10 | RAR5 BLAKE2sp file hash: exact alt key for files wholly inside one volume | High for single-volume files | Low | 0 | Negligible |
| 11 | PAR2 IFSC slice CRCs combined to whole-file CRC32: twin key when FileDesc MD5 is missing | Medium | Medium (crc32 combine exists) | Same small PAR2 files | Low |
| 12 | ZIP DOS vs 0x5455 UTC offset: same-uploader clock key from article 1 | Medium | Low | 0 | Negligible |

## Packer and tool fingerprints

| Tool | Archive tells | PAR2 tells | Default settings |
|---|---|---|---|
| ngPost (RAR mode) | RAR4/RAR5 with -ep1 base names, method 0x30 (store), no split unless set, -hp when password set. macOS builds exclude .DS_Store. | PAR2 made by an external par2 binary, so the creator string depends on that binary. | rar a -idp -ep1 -m0, plus -v<N>m and -hp<pass> when set. Archive named after the NZB or a random 17-char name. PAR2 0 percent by default. |
| ngPost (7z mode) | 7z Copy method at -mx0, encoded header with plaintext StreamsInfo, header encryption only with a password. | Same as above. | 7z a -mx0 -mhe=on, plus -p<pass> when set. Password length 13 by default. |
| WinRAR 3.x (inferred from fixtures) | RAR4 host 2 (Win32), DOS-only FTIME, no EXTTIME. | None specific. | Default dictionary UNVERIFIED. UnpVer 29. |
| WinRAR 5.x and 6.x (Windows) | RAR5 host 0 (Windows), HTIME in FILETIME form (no UNIX_NS), Win32 attributes, STM only with -os. QO per volume (libarchive fixtures). | None specific. | Default method and dict UNVERIFIED (rarlab blocked). |
| WinRAR 7.x | RAR5 comp_info version bits 1 (RAR 7.0+ algorithm) when present. | None specific. | Larger dictionary limit (above 4 GB) UNVERIFIED. |
| Linux rar 3 and 4 (inferred from fixtures) | RAR4 host 3 (Unix), EXTTIME present, UOW owner record. | None specific. | Dict and method UNVERIFIED. |
| Linux rar 5 | RAR5 host 1 (Unix), UOWNER user and group names with uid and gid, HTIME with UNIX_NS. | None specific. | UNVERIFIED. |
| rars (Rust crate, test fixtures only) | RAR5 HEAD_CRYPT identical across volumes. Not seen in real posts. | None. | Not a real-world tool. |
| 7-Zip (Windows, 26.x) | 7z signature 0.4, FILETIME times (UTC), Copy at -mx0, LZMA2 otherwise. Zip spec 6.3 with 0x000a NTFS. | None. | Header compression ON, header dict 1<<20, header level 5, LZMA2 default, solid block 1<<24 min. Per-level dict table UNVERIFIED. |
| p7zip and 7-Zip on Linux | 7z attribute 0x8000 Unix extension with st_mode. | None. | Same as 7-Zip. |
| Info-ZIP (zip on Linux) | ZIP host 3 with 0x5455 and 0x7875 in the local header. 0x000d or 0x5855 possible. GP bit 3 for streamed writes. | None. | Deflate. |
| WinZip AES | Method 99 with 0x9901 extra, AE-2 (CRC 0) or AE-1, strength 256 typical. | None. | Strength and method in the extra. |
| par2cmdline (mainline) | None. | Creator "Created by par2cmdline version X." One copy per recovery volume. | 2000 blocks, 5 percent. Version literal from fixture only. |
| par2cmdline-turbo | None. | Creator string built from configure.ac (1.3.0 in source). Output string UNVERIFIED. | UNVERIFIED. |
| ParPar | None. | "ParPar v<ver> <arch> [https://github.com/animetosho/parpar]". Writes IFSC, Creator, FileDesc, UniFileN (non-ASCII names only). Slice size multiple of 4. | Auto slice cap 32768 input slices. Slices and recovery must be given. |
| QuickPar | None. | "QuickPar 0.9" appears only as a test literal, not a live sample. | UNVERIFIED. |
| MultiPar and par2j | UNVERIFIED (repo paths unreachable). | UNVERIFIED. | UNVERIFIED. |

## Gaps and caveats

- Blocked during the waves: rarlab.com, 7-zip.org, web.archive.org, par2cmdline master configure.ac. RAR per-version dict defaults, 7-Zip -mx dict table, MultiPar and par2j creator strings, and the RAR4 recovery percent formula remain UNVERIFIED.
- Every fixture is self-made or from libarchive or rars test sets. Tool mapping from fixtures (for example WinRAR 3.x or 7-Zip 0.4) is inference, not confirmed against real output.
- RAR5 HEAD_CRYPT salt sharing across volumes is verified for the rars writer only. WinRAR output is unverified.
- RAR 2.0 partial decode was not built. RAR 2.9 and PPMd partial decode stream in 1 MiB steps.
- Sources: raw/w1a-rar.md, raw/w1b-7z.md, raw/w1c-zip-par2-sfv.md, raw/w2b-packer-fingerprints.md, raw/w2c-verified-layouts.md, raw/w3b-packer-fingerprints.md, raw/w3c-rr-qo-srs.md, raw/w5c-leaks.md, raw/w6a-rar-proto.md, raw/w7a-real-fixtures.md, raw/w7b-encrypted.md, raw/w8a-filters-hp-rar20.md, raw/w8c-resolved.md.
