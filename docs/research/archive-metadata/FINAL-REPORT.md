# Archive and packer metadata: what we can read, and what it proves

Nov 2, 2026. Exported from the Claude Doc https://claude.ai/artifact/NSkmWVebA67M95ps5NLqP1

## Summary and ranking

The biggest win is free: Atlas already reads RAR, zip and 7z headers from the first article but throws away the per-file CRC32, unpacked size, timestamps and host OS. Keeping them gives an exact alt key and a packer fingerprint with no extra download. The second finding is that compressed archives are not a dead end. In pure Rust tests, truncated RAR5, RAR 2.9, 7z (LZMA, LZMA2, BCJ, Delta) and zip Deflate streams all decoded to a correct prefix at about 1 byte out per byte in, so the inner MKV header is reachable from the first few articles of compressed posts too.

The catalog behind this doc has 267 fields across RAR4, RAR5, 7z, zip, PAR2, SFV, SRR, SRS, .rev and raw splits: 226 checked against source, 41 still unverified. Atlas stores 34 of them, parses and drops 67, and never reads 166.

| Rank | Idea | Value | Effort | Extra bandwidth per release | CPU per release |
| --- | --- | --- | --- | --- | --- |
| 1 | Keep the inner file CRC32, unpacked size, mtime and host OS that the RAR, zip and 7z parsers already walk past | High (exact alt key) | Low | None | Negligible |
| 2 | Persist PAR2 FileDesc MD5, 16 KB MD5 and the Creator string per file | High (twin key that survives renames, plus tool) | Low | None (PAR2 index already fetched) | Negligible |
| 3 | Packer fingerprint from article 1: RAR host OS, method, dictionary, HTIME, owner record; zip version-made-by and extras; 7z signature version and coders | Medium (tool and uploader grouping) | Low | None | Negligible |
| 4 | Per-archive random values as twin keys: RAR5 -hp salt and check value, 7z AES IV, 7z start header NextHeaderCRC | High for encrypted and 7z posts | Low | None for RAR5 and the 7z start header; last article for the 7z IV | Negligible |
| 5 | Partial decode of compressed RAR5, RAR 2.9 and 7z to reach the inner MKV header | High (opens media probing to non-store posts) | Medium | 1 to 6 articles, as for store mode | 10 to 50 ms per release |
| 6 | Whole-file CRC32 from PAR2 IFSC slice CRCs, to join yEnc crc32 and srrDB | Medium | Low (prototype matches zlib) | None | Low |
| 7 | Uploader leaks: RAR5 Unix owner names and uid, zip DOS local time against its UTC extra, RAR4 local time against the Usenet Date | Medium (same uploader) | Low | None | Negligible |
| 8 | 7z and zip tail parse: the 7z file list and zip central directory with per-file CRCs and times | High for 7z and zip | Low (Atlas already fetches the tail) | Last 1 to 2 articles | Negligible |
| 9 | RAR5 .rev first file: size and CRC32 of every data volume | Medium | Low | One .rev article | Negligible |
| 10 | SRR and SRS parse for scene posts that include them | Low to medium | Low | One small file | Negligible |

Value scores are judgement calls from the research, not measured against live Atlas data.

## What sits where

RAR and zip put each file's header in front of its data, so one article per volume gives the file list. 7z and the zip central directory sit at the end, so they need the last article. PAR2, SFV, SRR and .rev are small separate files.

| Format | First article of a volume | Last article of a volume | Separate small file |
| --- | --- | --- | --- |
| RAR5 | Main header (volume number, solid, lock, RR, locator), encryption header if -hp, every file header in this volume with all extras | End-of-archive (next volume flag), recovery record, quick-open cache of this volume's headers | .rev volumes: size and CRC32 of every data volume |
| RAR4 | Main header flags, file headers with Unicode names, EXTTIME, salt, Unix owner subblock, comment | End-of-archive with data CRC and volume number, PROTECT recovery block | .rev (RAR 3) trailer |
| 7z | 32-byte start header: version, NextHeaderOffset, size and CRC; the first file's packed data from byte 32 | The whole file list: names, sizes, CRCs, times, attributes, coders and AES props, usually LZMA-compressed | None |
| zip | Local headers: method, flags, DOS time, CRC (unless a data descriptor), name, extras for UTC time, uid and gid, NTFS times, AES | Central directory: version made by (host OS), external attributes, comments, zip64 records | None |
| PAR2 | Not in the archive | Not in the archive | Creator, set ID, per file MD5, 16 KB MD5, size, name, slice CRCs and MD5s |
| SFV, SRR, SRS | Not in the archive | Not in the archive | Volume CRC32s; scene RAR headers, stored NFO and SFV, OSO hash; sample track CRCs |

Two details matter for linking. RAR5 marks volume 1 by leaving out the volume-number field, so part 1 is found without its filename. RAR5 quick open is per volume: it caches only that volume's headers, not a set-wide list.

## Reading inside compressed archives

Every common codec decoded a correct prefix from a cut stream in pure Rust, so compressed posts give the inner MKV front almost as cheaply as store mode. Tests ran on 8 MiB generated payloads (about 94% random bytes, like video) cut at 10, 25 and 50% of the packed data. All output matched the original prefix byte for byte.

| Codec | Crate and call | Output at a 10% cut | Granularity lost at the cut |
| --- | --- | --- | --- |
| Store (RAR -m0, 7z Copy, zip stored) | None: inner offset = data offset + X | 100% of what was fetched | None |
| zip Deflate | flate2 1.1 (miniz\_oxide) | 842 KB | A few KB |
| 7z LZMA2, no end header | lzma-rust2 0.25 Lzma2Reader from byte 32, 64 MiB dictionary | 786 KB, starts 1A 45 DF A3 | 64 to 256 KiB |
| 7z LZMA, no end header | lzma-rust2 LzmaReader with default props 0x5D | 786 KB | 256 KiB |
| 7z LZMA2 with BCJ x86 or Delta | lzma-rust2 filter::bcj and filter::delta readers | 786 KB | Same as LZMA2 |
| RAR5 | rars 0.12.3 codec::rar50::Unpack50Decoder, called directly on the packed slice | 786 KB | Up to 64 KiB plus the partial block |
| RAR 2.9 LZ and PPMd | rars codec::rar29::Unpack29 | LZ: 1 MiB; PPMd: 2 MiB at a 25% cut | 1 MiB steps |
| RAR 2.0 | rars rar20 | 0 bytes (reads all input first) | Needs a small patch |

Some rules come with it. Call the rars codecs directly: its archive parser refuses any truncated file. LZMA2 carries lc, lp and pb in the stream, so a missing 7z end header costs only the dictionary size, and a larger one is safe. Check for the EBML magic 1A 45 DF A3 at inner offset 0 to catch wrong props. 7-Zip only applies BCJ to exe-like extensions, so MKV and MP4 normally get plain LZMA2. Solid archives decode only from the start of a solid run.

Only `rars` exists as a pure-Rust decoder for compressed RAR. It is Apache-2.0 and young, so pin it and keep a fixture suite. The FFI fallback, `unrar`, wraps the C++ UnRAR, whose licence allows decoding.

## Fields worth keeping

These are the fields with a linking or display use. The full 267-field list, with byte layouts, sources and Atlas status, is in the YAML catalog.

### RAR5 and RAR4

| Field | Where | What it gives | Use |
| --- | --- | --- | --- |
| File CRC32 and unpacked size | File header, article 1 | CRC of the whole inner file (on its last part in a split) and its exact size | Alt key across RAR, 7z and zip rewraps; srrDB lookup |
| BLAKE2sp hash | RAR5 extra 0x02 | 32-byte hash of the inner file | Alt key, but only for files wholly inside one volume |
| Compression info | RAR5 comp\_info vint; RAR4 unp\_ver and method byte | Store or level, solid, dictionary (128 KiB << N, mask 0x0f for RAR 5.0), RAR 7 flag | Tool and settings fingerprint; tells whether partial decode is needed |
| Host OS and attributes | RAR5 vint (0 Windows, 1 Unix); RAR4 byte 0 to 5 | Windows or Unix packer, full st\_mode on Unix | Tool and uploader fingerprint |
| Times | RAR5 HTIME extra (Unix ns or FILETIME); RAR4 DOS local time plus EXTTIME | mtime, ctime, atime with sub-second precision | Uploader timezone (RAR4) and OS (precision) |
| Unix owner | RAR5 extra 0x06; RAR4 UOW subblock | User and group names, uid and gid | Same uploader |
| Redirection | RAR5 extra 0x05 | Symlink or hardlink target path | Path leak |
| Volume number and first-volume flag | RAR5 main header flag 0x0002; RAR4 MHD\_FIRSTVOLUME | Which part this is | Find part 1 under obfuscated names |
| Encryption header | RAR5 type 4 (-hp) | KDF count, 16-byte salt, 8-byte check value | Password test in one PBKDF2 call; set key across volumes |
| Service headers | RAR5 type 3; RAR4 NEWSUB | Comment, quick open, recovery record %, ACL owner SID, NTFS streams | Comments and SIDs identify uploaders; STM can carry Zone.Identifier HostUrl |

### 7z

| Field | Where | What it gives | Use |
| --- | --- | --- | --- |
| Start header | Bytes 0 to 31 of .7z.001 | Version (0.4 means 7-Zip 23 or later), NextHeaderOffset, size, CRC | Per-archive twin key from article 1; finds the tail |
| Coders and props | End header, StreamsInfo | LZMA or LZMA2 dictionary, lc/lp/pb, BCJ or Delta, AES cycles | Tool and settings fingerprint |
| AES IV | AES coder props | 16 random bytes per archive, readable even with -mhe; 7-Zip writes no salt by default | Twin key for encrypted 7z |
| Substream sizes and CRC32s | SubStreamsInfo | Size and CRC of every inner file | Alt key |
| Names, times, attributes | FilesInfo | UTF-16 names, FILETIME UTC, Windows attributes plus Unix st\_mode when bit 0x8000 is set | Display; uploader OS |

### zip

| Field | Where | What it gives | Use |
| --- | --- | --- | --- |
| CRC, sizes, method | Local header (article 1) or central directory (tail) when bit 3 is set | Inner file CRC32 and size | Alt key |
| Version made by | Central directory | Host OS byte and spec version | Tool fingerprint (Info-ZIP host 3, Windows tools host 0 or 10) |
| DOS time with 0x5455 or 0x000a extra | Local header | Local time and UTC time of the same file | Uploader UTC offset from article 1 |
| 0x7875 extra | Local header | Unix uid and gid | Same uploader |
| 0x9901 extra | Local header | WinZip AES strength, real method (u16), AE-1 or AE-2 | Encryption; tool |

### PAR2, SFV, SRR

| Field | Where | What it gives | Use |
| --- | --- | --- | --- |
| FileDesc MD5, 16 KB MD5, length, name | PAR2 index | Full-file hashes of every posted file | Twin key that survives renames |
| IFSC slice CRCs | PAR2 index | CRC32 per slice | Whole-file CRC32 by CRC combine (prototype matches zlib) |
| Creator | PAR2 index | "Created by par2cmdline version X." or "ParPar vX arch \[url\]" | Tool, and the uploader's CPU arch for ParPar |
| Recovery set ID | Every packet | MD5 of the Main packet, bound to file names | Same post set; a rename gives a new ID |
| SFV lines and comments | .sfv | CRC32 per volume, generator line | srrDB match; tool |
| SRR blocks | .srr | Original RAR headers, stored NFO and SFV, OSO hash | Scene release identity |

## Tool fingerprints and encrypted posts

Archive and PAR2 headers name the packer even when every name and subject is obfuscated. The rows marked inferred come from libarchive and rars test archives, not real Usenet posts.

| Tool | Archive tells | PAR2 tells |
| --- | --- | --- |
| ngPost | RAR store mode from `rar a -idp -ep1 -m0 [-v<N>m -hp<pass>]`; 7z from `-mx0 -mhe=on`; no split by default; random 17-character names and 13-character passwords | Whatever par2 binary is configured |
| ParPar | Packs nothing | "ParPar v0.4.x x64 \[https://github.com/animetosho/parpar\]" (leaks CPU arch); Unicode name packets only for non-ASCII names |
| par2cmdline | Packs nothing | "Created by par2cmdline version X."; defaults 2000 blocks, 5% |
| WinRAR 5 and later (inferred) | RAR5, host Windows, HTIME as FILETIME, Windows attributes | None |
| Linux rar 5 (inferred) | RAR5, host Unix, Unix owner record with user and group names | None |
| WinRAR 3.x (inferred) | RAR4, host Win32, DOS time only | None |
| Linux rar 3 or 4 (inferred) | RAR4, host Unix, EXTTIME present | None |
| 7-Zip 23 and later (inferred) | 7z signature version 0.4, ARM64 and RISC-V filter IDs; header compression on, header encryption only with a password | None |
| Info-ZIP | Host 3 (Unix), extras 0x5455 and 0x7875 | None |
| WinZip | Spec 6.3, NTFS 0x000a extra, AES 0x9901 AE-2 | None |

Encryption hides less than it seems. RAR5 -hp leaves only its encryption header in clear: KDF count, salt and an 8-byte check value. In a 7-volume test set made with rars, that header was byte-identical in every volume, so encrypted volumes with random names still group into one set. WinRAR has not been checked yet. The check value tests a password with one PBKDF2-HMAC-SHA256 call of 2^15 + 32 rounds, about 3 to 15 ms, which Atlas already does. RAR4 -hp puts an 8-byte salt before every block and has no check value, so a test means decrypting the next block and checking its CRC. 7z -mhe hides the file list but leaves the AES IV, cycle count and header sizes readable. zip never hides names.

## Atlas today

Atlas reads the right headers but keeps only names, sizes and encryption. This was checked against a local checkout at ad111a0, which is behind main at a29ee52, so line numbers need a recheck before a PR.

| What | Status | Where |
| --- | --- | --- |
| Inner names and sizes | Stored | `archive_inner_files` (name, size, source) |
| Encryption, hidden names, RAR5 and zip password check | Stored | `archive_release` |
| First inner file's format, method, sizes, CRC32, store offset | Stored | `release_probe.info` JSON (`probe::archive::ArchiveHead`) |
| SFV CRCs, moviehash, NFO | Stored | `release_probe` |
| Per-file CRC32 in listings | Parsed, then dropped | RAR5 skips it (rar.rs:265), RAR4 never reads it, zip CD offset 16 unread, 7z substream digests discarded (sevenz.rs:299) |
| PAR2 MD5 and 16 KB MD5 | Used for twin fingerprints, not stored with the listing | steps.rs:72-76 |
| Times, host OS, attributes, dictionary, solid, RR, owner, comments | Not read | All formats |
| PAR2 Creator, IFSC; SRR | Not read | par2.rs:93-124 has no branch; no SRR parser |

Confirmed bugs, each checked at the cited lines:

1. The probe decodes RAR4 Unicode names as the ASCII fallback only (probe/archive.rs:81-83), while the listing decodes them properly.
2. Encryption columns go stale: they are written only when Some (steps.rs:142), and the upsert sets only `password` (store.rs:70).
3. The 7z encoded header decodes LZMA only (sevenz.rs:513-516). LZMA2 or filtered headers come back as damaged.
4. The release view caps at 256 rows while 4,096 are stored (store.rs:93). PAR2 rows sort first, so a big PAR2 listing hides every archive row.
5. zip names without the UTF-8 flag decode as Latin-1, not CP437 (zip.rs:25-34). The probe's zip encrypted flag ignores bits 0x0040 and 0x2000.
6. The SFV probe takes the first three small text files (step.rs:109), so an SFV after three NFO or TXT files is never read.
7. A 7z header with no names property counts as complete with zero files, so the password state reads No (sevenz.rs:335-396).

The lowest-cost change is to add crc32, mtime and host\_os columns to `archive_inner_files`, fill them from the parsers that already walk those bytes, and switch `lzma-rs` to `lzma-rust2`, which also covers LZMA2 and BCJ.

## Fetching just enough

1. Read the first article of each volume (Atlas does this already). Parse every RAR or zip header in it, plus the 7z start header.
2. For 7z, compute the end header's position as 32 + NextHeaderOffset, map it to the last volume and article with the NZB segment sizes, and fetch only that article (rarely two). For zip, do the same for the central directory.
3. For the inner media header, map inner offset X to a volume and article. Store mode is direct: X sits at the data offset + X within the volume's payload. Compressed RAR5, RAR 2.9 and 7z decode from the data offset of volume 1 forward, so fetch articles in order until the parser has what it needs.
4. For PAR2, read the index file once per set and keep the Creator, FileDesc and IFSC packets.

## Nested archives, up to 10 levels

Atlas reads one level today: it lists the outer archive and media-parses a stored inner file, but never parses that inner file as an archive (checked at ad111a0). The target is recursion through any mix of RAR4, RAR5, 7z and zip, stored or compressed, down to 10 levels.

The design is a stack of byte sources. Each layer sees its parent through one of two views:

- **Stored view:** a linear offset map. Inner byte X is parent byte data\_offset + X, so any byte, front or tail, maps straight to an outer article.
- **Decoded view:** a forward-only prefix stream from the layer's codec (rars, lzma-rust2, flate2), with BCJ or Delta stacked on top. It reaches only the front of the inner file.

The loop: identify the bytes, parse headers, pick the largest inner file, open the right view, and repeat. A view over a view composes, so a stored chain of any depth stays a single offset map down to the outer article.

| Layer reached through | RAR or zip headers | 7z file list, zip central directory | Inner MKV front | Inner tail (moviehash) |
| --- | --- | --- | --- | --- |
| Only stored layers above | Yes | Yes, one extra article each | Yes | Yes |
| Any compressed layer above | Yes, from the decoded prefix | No: needs the whole stream | Yes | No |
| Encrypted layer above | Stop | Stop | Stop | Stop |

Limits per release: depth 10, about 16 MiB of decoded bytes in total, and a ratio cap that stops a layer when output runs past 100 times its input (zip bomb guard). Every layer is recorded with its format, method and offsets, so the release page can show the chain.

## What this cannot do

- Passworded RAR and 7z hide everything past the encryption header without the password. Only the salt, IV, KDF parameters and sizes remain.
- Solid archives decode only from the start of a solid run, so files after the first need everything before them.
- Compressed posts reach only the front of the inner file. A mid-file seek for frames or audio still needs store mode.
- 7z gives no file names or CRCs without its last article.

## Open items

- Whether WinRAR also repeats the same -hp salt in every volume (shown only for rars output).
- RAR per-version default dictionaries and the 7-Zip per-level table: rarlab.com and 7-zip.org were blocked by the proxy.
- MultiPar and QuickPar creator strings; the RAR4 recovery-percent formula; the RAR5 ACL and quick-open entry flag layouts.
- None of the prototypes has seen a real Usenet post yet. The next step is a run over a sample of first articles from the sealed log.

## Files

- YAML field catalog (267 fields): `docs/research/archive-metadata/archive-field-catalog.yaml`
- Prototype Rust extractors: `proto/rar_meta.rs`, `proto/sevenz_zip_meta.rs`, `proto/par2_sfv_srr_meta.rs` (about 3,000 lines, no panics on 235 test archives at every truncation tried)
- Research log and per-wave notes: `research/archive-metadata/log.md` and `raw/`
- Shared leads: `research/linking-findings.md`, section "Extract metadata from 7z, RAR and archives"

## Sources

- [UnRAR source mirror](https://github.com/pmachapman/unrar) (arcread.cpp, headers5.hpp, crypt5.cpp, recvol5.cpp, qopen.cpp)
- [7-Zip source](https://github.com/ip7z/7zip) (7zIn.cpp, 7zAes.cpp, 7zUpdate.cpp, 7zHandlerOut.cpp)
- [libarchive readers and test archives](https://github.com/libarchive/libarchive)
- [Info-ZIP extra field list](https://github.com/LuaDist/zip/blob/master/proginfo/extrafld.txt)
- [PAR2 specification](https://parchive.github.io/doc/Parity%20Volume%20Set%20Specification%20v2.0.html)
- [ParPar](https://github.com/animetosho/ParPar), [par2cmdline](https://github.com/Parchive/par2cmdline), [ngPost](https://github.com/mbruel/ngPost)
- [pyReScene](https://github.com/srrDB/pyrescene) (SRR and SRS)
- Crates: [rars](https://crates.io/crates/rars), [lzma-rust2](https://crates.io/crates/lzma-rust2), [sevenz-rust2](https://crates.io/crates/sevenz-rust2), [flate2](https://crates.io/crates/flate2)
