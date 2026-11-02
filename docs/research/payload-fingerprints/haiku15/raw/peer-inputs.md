# Peer inputs
## after wave 1
- yEnc headers: encoder fingerprint from =ybegin/=yend field order, hex case, line=, escape set; newsmangler MIDs <unixtime.5dp.part@host>; Xref multi-group = true crosspost; Injection-Info posting-account.
- posting-behavior: Nyuu MID rand24-ts@nyuu; ngPost GROUP_POLICY one-group-per-file (Batman split); Nyuu --date now; kkm/nzb-indexer 4h incomplete window.
- other payload thread: IFSC CRC32s combine to full-file CRC from one PAR2 article; RAR5 ns mtime strong twin key; SFV/volume CRC padding (ReScene) -> don't use for twins; no indexer uses crc32_combine.
- opus payload thread: crc_concat over pcrc32 gives CRCs of shared intervals across posts with different article sizes (animetosho crc_manip.py); PAR2 RSID deterministic, PAR3 random; per-creation random values = perfect twin keys from one article: 7z NextHeaderCRC, RAR5 -hp salt, RAR5 encryption salt+IV. Dead: ssdeep/TLSH/CDC on compressed media.
## during wave 3
- opus yEnc: ngPost body quirks (unpadded pcrc32, no part=/crc32= in =yend, trailing ws unescaped) fixed in Hydro74000 v5.6; GoPostStuff uppercase pcrc32; Nyuu MID ^([A-Z][a-z]){12}-\d{13}@nyuu$; use part size + :lines not :bytes; NNTmux joins RAR inner CRC32, hash16k, MKV SegmentUID.
- opus posting: TRiToN split = ngPost obfuscate + group_policy each_file; DMCA co-removal via STAT over time links whole release (rp.os3.nl 2014-2015 p82 report.pdf).
- container thread: MKV SegmentUID/DateUTC/_STATISTICS_WRITING_DATE_UTC, MP4 mvhd creation_time separate identical alts from remuxes; MuxingApp/WritingApp, MP4 hdlr/©too muxer fp; BD id.bdmv disc_id, DVD IFO MD5 same-disc keys.
- stream thread: x264/x265 options SEI string (same encode), DV RPU (dolby_vision crate), FLAC STREAMINFO MD5.
- archive-metadata thread: RAR5 main 0x0002 absent only in vol 1; RAR5 extra 0x06 / RAR4 UOW leak unix user uid/gid; BLAKE2sp on split part covers only that volume; 7z -mhe salt/IV/header CRC readable in last article; PAR2 packet MD5 validates partial packets. Their wave 2: Rust decompressors/prefix decode, creator strings, defaults from source -> AVOID overlap.
- container w2: same SegmentUID + DateUTC = same mux run; MP4 moov offset = mdat offset+size.
- stream w2/3: x264/x265 SEI in first AU; first IDR NAL hash + AC-3 crc2 sequence link same encode across remuxes.
- archive w2/3: truncated Deflate/LZMA/LZMA2 decode a correct prefix (flate2, lzma-rust2); rars rejects truncated; ngPost packs rar -m0 -idp -ep1, 7z -mx0 -mhe=on; ParPar creator includes CPU arch; RAR5 .rev lists size+CRC32 of every data volume; Atlas parses only name/size/encryption now.
- archive w4/5: partial decode in pure Rust works (7z LZMA2 lzma-rust2 from offset 32, RAR5 rars direct codec); uploader leaks: zip DOS vs UTC extra = tz offset, RAR4 DOS time vs Date = tz, RAR5 UOWNER, STM Zone.Identifier HostUrl, subsecond precision = OS.
- yenc w2: Date string format clusters tools (-0000 python, +0000, GMT); key on =ybegin name= under obfuscation; NZB writer fingerprint (iso-8859-1 DOCTYPE vs Nyuu utf-8); shared password = same uploader.
- posting w2: Postie each_file + full obfuscation, 750000 bytes; NewsUP 768000 fixed From; post-check reposts reuse part+size under new MID; Newsbin dedupe hash of first 26K.
- haiku payload w2: offline oracles No-Intro/Redump DATs vs inner CRC+size, NSRL vs PAR2 MD5; 7z SubStreamsInfo CRCs; Ogg serial, LAME CRC16@152, APE MD5@36, ISO9660 PVD @0x8000, CHD sha1@84.
- haiku payload w4 TESTED: PAR2 exponent-0 recovery slice = XOR of all input slices; identical across renames, order, set IDs, tools (same bytes+slice size). One article of first recovery vol = rename-proof twin key. Script research/payload-fingerprints/haiku/tools/par2_recovery_compare.py
- opus payload w5 MEASURED: RAR5 -hp last vol size ≡14 mod 16; unencrypted RAR deterministic across runs/versions, new mtime changes ~20 B/volume -> re-RARs share all middle article bodies + pcrc32. (size,crc32) 0 collisions on 445k Redump/No-Intro files.
IMPLICATION for my interval method: middle-of-file interval matches cannot tell a twin from an independent re-RAR with same switches; a twin verdict must also match the header-bearing intervals (first article of each volume) or a per-creation field. Partial interval match = "same bytes in that span" only.
- haiku payload w5: IFSC + FileDesc MD5/len survive renames, packs vs singles; exp-0/set ID are whole-set keys.
- opus payload w6 MEASURED: re-RAR w/ only mtime change keeps every middle pcrc32 + NZB encoded size; only first+last part of each volume change. 7z -mx0 mtime/name change touches only first+last part of whole set. ngPost --compress = rar -ep1 -m0 -v42m, no password default; NxG Upper -hp default.
- haiku payload w6: NZBGet RarRenamer orders obfuscated volumes from headers; AltMount sample-sha256-v1 (size + three 64KiB samples) twin key for unarchived files.
- yEnc headers thread SOLVED: X-Received-Body-CRC = zlib.crc32(raw body with CRLF + '.\r\n'), headers excluded; matched 12 real Highwinds/Farm articles. Same tool+yEnc name+split -> identical -> HEAD-only twin key per part. Complements pcrc32 (needs BODY); differs when name/line length differ.

## 10:23Z Haiku payload (other thread) wave 8
- par2cmdline and ParPar default first recovery exponent 0: exponent-0 recovery slice = XOR of all input slices (coefficient 1), deterministic for same data + slice size, so a recovery-slice key works when slice sizes match. ngPost makes PAR2 only with --par2_pct. Marker-file hashes mostly not public. Crates: crc32fast (combine), RustCrypto, xxhash; own parsers.

## 10:29Z Opus payload wave 8
- Encrypted reposts (RAR5 -hp, 7z -mhe): volume sizes identical whatever the password. (V volumes, n, S_last) ~27.5 bits = blocking key; + one more exact file size < 1 false pair per 1e8.
- Disc posts: hash BDMV metadata / DVD IFOs = alt key; SHA-1 of AACS Unit_Key_RO.inf and UDF Volume Set ID = per-master keys; ContentHash000.tbl checks single m2ts segments.
- Crates: crc32fast, RustCrypto, lzma-rust2, zip default-features=false; xxhash for persisted keys (never ahash). Own PAR2/RAR/7z/yEnc parsers.

## 10:30Z yEnc headers Opus wave 7 (header angle, for the final map's uploader/tool rows)
- Deterministic Message-ID schemes that link with no attack: geri (HMAC of key+file+part+body), keimpema 'part{n}of{N}.{guid}@unknown.com' (one GUID per file), Upstream '@riverup' microsecond time. '[A-Za-z]{32}@UFK' = server-assigned (provider tell). '<32hex>@ngPost' = uuid4.

## 10:52Z Haiku payload wave 9
- par2cmdline default slice = ceil(total/2000) rounded to x4 over ALL files: extra file breaks exp-0 key. rarfile test/files = 106 RAR3/5 with .exp oracles (parser tests). (srrDB lookup not used: out of scope.)

## 10:54Z Opus payload wave 9
- Size key collapses for fixed-cap disc images (44% share exact size). Pure-Rust std-only CRC32 GF(2) toolkit built+tested (opus-tools/w9b_crc32gf2/). 7z version 00 04 vs 00 03 = libarchive; RAR5 has no version field; ngPost slices 5 MiB or 768000.

## 11:31Z yEnc headers Opus wave 9
- Real repost event: 7-volume RAR set uploaded 4x in 43 min (some renamed); all volume sizes match; damaged copy 14/15 (allow n-1). PAR2 files never match (regenerated), though FileDesc MD5 of the volumes should still match (not checked by them). Entropy gate (segment sd 40-600) removes false groups 55/75 -> 0. Blocking key: segment count + first 2 sizes. Spec: research/yenc-header-forensics/opus/w9b-atlas-rust-spec.md.
