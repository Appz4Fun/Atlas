# Payload fingerprints (Haiku x3, 10 waves): final report

Atlas research, 2 Nov 2026. Ten waves of three Haiku agents each looked for anything in a post's payload that can link two Usenet binary posts together: PAR2 packets, archive headers, raw split files, marker files, sampled hashes and external hash databases. Per-wave files are `w1a.md` to `w10c.md` in this folder, and the running log is `log.md`. Two local tests used par2cmdline 0.8.1 and ParPar 0.4.6, with scripts in `tools/`.

The sister report from the other Haiku payload thread (`../haiku15/final-report.md`) covers yEnc pcrc32 folding, interval CRCs, the linking policy and storage. This report does not repeat those. It adds the PAR2 recovery and per-file keys, what survives each anti-linking trick, re-pack provenance, name leaks, external oracles (srrDB first), per-category creation IDs, and the Rust and test-file plan.

Terms: **twin** means the same bytes posted again. **Alt** means the same release in a different container. **Same film** means the same work, not the same file. **Same uploader** means the same person or machine made it. **Tool** means it identifies the posting or packing software.

Cost tiers: **free** means the overview or yEnc header. **1 article** means one body fetch. **A few** means 2 to 5 bodies. **Every** means every article of the file. **+API** means an external lookup.

Atlas status: **yes** means stored and used. **part** means stored or parsed but not used as a link key. **no** means not collected. **?** means not checked. These come from the catalog thread's check against main at a29ee52.

## Headline

1. **The exponent-0 PAR2 recovery slice is a set-level twin key that survives renames.** Recovery slice e is the sum of base_i^e · slice_i. Every base to the power 0 is 1, so slice 0 is the XOR of all input slices. It ignores file names, file order, Set ID and the tool used. It was tested: par2cmdline and ParPar produced byte-identical slices, and renaming every file left slice 0 unchanged. It breaks when the slice size differs or a junk file is added.
2. **Per-file PAR2 keys survive everything short of changing the bytes.** These are the FileDesc full MD5 + length and the IFSC per-slice MD5/CRC list. In a test, the same file in three different sets (renamed, alone, and padded with a junk file) gave identical keys.
3. **Default slice sizes decide when exponent 0 can match.** Default par2cmdline posts match each other, and so do default MultiPar posts. par2cmdline and MultiPar don't match each other by default. ParPar has no default, so it matches only when the poster's `-s` happens to agree.
4. **srrDB can name obfuscated scene reposts.** Take the inner-file CRC32 from the last RAR volume header (1 article), then query `archive-crc:<hex>/archive-size:<n>` (1 call). It doesn't work on `-hp` sets.
5. **Obfuscation usually leaves the inner names.** ngPost `--gen_name` randomizes only the archive name. RAR inner names and the archive comment (CMT) sit in the first volume, and downloaders don't read them, so Atlas has to read them itself.

## Signal map

| Signal | Proves | Cost | Atlas | Notes |
| --- | --- | --- | --- | --- |
| PAR2 exponent-0 recovery slice (or its first article's bytes) | twin of the whole file set | 1 article (first recovery volume) | no | Tested across tools and renames. Needs the same slice size and the same file set. See the tool table below. |
| PAR2 recovery slices with exponent 1 or higher | twin of the set + FileId order | 1 article | no | Changes when a rename flips the FileId sort order (2 of 6 test renames). |
| PAR2 IFSC per-file slice MD5+CRC32 list | twin per file, across packs, singles and junk-padded sets | 1 article (index) | no | Needs the same slice size. Tested. |
| PAR2 FileDesc full MD5 + length | twin per file | 1 article (index) | part | Rename-proof. Prefer it over File ID and Set ID, which hash the name. |
| PAR2 MD5-16k + length | twin candidate, maps obfuscated names | 1 article (index) + article 1 | part | Used by SABnzbd, NZBGet and StreamNZB to rename. |
| PAR2 Set ID | same files + same names + same slice size | 1 article | yes | Not a twin key on its own. |
| PAR2 Creator packet | tool (+ CPU arch for ParPar) | 1 article | no | Annotation only. |
| PAR2 FileDesc / UniFileN name | original name, if PAR2 was made before renaming | 1 article | part | Whether ngPost and Nyuu create PAR2 before or after renaming is UNVERIFIED. |
| RAR inner file names + RAR5 archive comment (CMT) | original release name → twin/alt by name | 1 article (first volume) | part | Survives `--gen_name` and RAR4 `-p`, but not `-hp` or `-mhe`. |
| Inner-file CRC32 + unpacked size (last RAR volume, 7z SubStreamsInfo, ZIP central directory) | alt | 1 article (last volume) | part | Non-last RAR volumes carry only the chunk CRC. RAR5 `-p` turns the CRC into an HMAC. |
| srrDB `archive-crc` (+ `archive-size`) lookup | alt/twin to the named scene original | 1 article + API | no | `api.srrdb.com/v1/search/archive-crc:<HEX>/archive-size:<n>`. Accept only a single hit. About 1 request per second, with backoff on 429. Cache misses too. |
| No-Intro / Redump / NSRL offline DATs | alt to a known dump (games, discs, software) | 1 article (inner CRC) or index (MD5) | no | Offline, no rate limit. Redump has 445k (size, CRC32) pairs with 0 collisions (Opus thread). |
| Inner mtime (RAR5 ns, 7z 100 ns, RAR4 2 s) + stored time-flag set | same source copy (re-pack provenance), writer setting | 1 article | ? | unrar restores mtime and SABnzbd keeps it, so re-packs of a download inherit it. |
| RAR5 UOWNER, HOST_OS, 7z Unix mode bits | same uploader machine / OS | 1 article | no | Annotation. |
| Krusader / Total Commander `.crc` sidecar | alt (gives the original file's size + CRC32) | 1 article | no | Raw `.001` splits have no header. Part N holds the original bytes at (N-1)·volume. |
| Raw split `.001` first article | lets container/stream keys run on the original | 1 article | ? | Starts with the original file's magic bytes. |
| Scene file-set shape (RAR + NFO + SFV + Sample/Proof/Subs) | origin: scene vs re-pack | free (names) | ? | Any extra .txt/.jpg/.exe/.url means a re-pack. |
| Re-packer marker files + Sonarr re-uploader suffixes (-RP, -Obfuscated, -Scrambled, [rarbg]...) | same uploader / re-packer | free (names) | ? | Only RARBG.com.txt has a public hash (34 B, MD5 a7d518b7...). Need name + yEnc crc32 together. Size alone is not distinctive. |
| EAC log checksum / XLD signature | same rip | 1 article | no | Music. |
| FitGirl md5 list, NFO + FILE_ID.DIZ layout | specific repacker + build | free / 1 article | no | Games and 0day. |
| PE RSDS GUID + age | same build (software alt/twin) | a few | no | Verified per-build. COFF TimeDateStamp is weak. |
| PDF trailer /ID[0] | same document lineage | a few (tail) | no | |
| ISO9660 PVD volume set ID + creation time (centiseconds) | same mastering run | 1 article | no | Fixed-size disc images collide on size (44% in Redump), so use this instead. |
| Ogg serial, calibre uuid, MOBI seed | nothing durable | — | — | Regenerated on remux or conversion, or just a counter. Dead ends. |
| OpenSubtitles moviehash | candidate | first + last article | yes (#36) | |
| SubDB MD5(head+tail 64 KiB) | candidate | first + last article (same reads as moviehash) | no | Free to add next to moviehash. |
| imohash / AltMount sample-sha256-v1 (start, mid, end) | twin for unarchived files | 3 articles | no | AltMount has test vectors. |
| Raw-body hashes (Cleanfeed EMP MD5, X-Received-Body-CRC) | twin only for re-injections by the same tool with the same names | free / HEAD | no | Obfuscating posters change name= per article, so key reposts on the decoded payload instead. |
| Body mutation by providers | — | — | — | No evidence any backbone changes bodies. Key only CRC-verified parts. A missing article is not a different one. |

## PAR2 defaults by tool (for the exponent-0 key)

| Tool | Default slice | First exponent | Default redundancy | Status |
| --- | --- | --- | --- | --- |
| par2cmdline / par2cmdline-turbo | (total+1999)/2000, min 4, rounded up to ×4, over ALL files | 0 | 5% | verified from source (commandline.cpp L1192-1207) |
| ParPar | none (`-s` required) | 0 | 0 (no recovery) | verified from help and par2gen.js |
| MultiPar / par2j | `/sr10 /sn3000`: about 10·√total, capped at 3,000 blocks | 0 (`/rs`; `/rf` is the file count) | none unless set | quoted from the shipped help. The formula is derived (UNVERIFIED in code) |
| QuickPar | 3,000 blocks (per MultiPar help) | ? | 10%? | UNVERIFIED |
| ngPost | uses embedded par2cmdline, only when `--par2_pct` is set | 0 | 0 by default (sample config 8) | verified. Sample configs use fixed `-s5M` / `-s768000` |
| Nyuu | no PAR2 | — | — | verified |

The GF field is 0x1100B for par2cmdline, turbo and ParPar (verified in source), and probably for par2j (constant found in the binary). Exponent 0 doesn't depend on the field anyway.

## Which keys survive which countermeasure

| Countermeasure | Survives | Breaks |
| --- | --- | --- |
| Random subject, Message-ID, From, yEnc names | every payload key | header keys, raw-body hashes |
| Random archive names (`--gen_name`) | inner names, inner size+CRC, PAR2 MD5s, IFSC, exponent 0 | PAR2 File ID, Set ID |
| Junk file added | per-file keys (inner CRC, full MD5, IFSC) | Set ID, exponent 0 (also changes the default slice size) |
| Different volume size, no encryption | inner size + inner CRC, srrDB lookup | volume hashes, Set ID, exponent 0 |
| RAR5 `-p` | inner sizes, names | inner CRC (HMAC), ciphertext hashes |
| `-hp` / `-mhe` with a new password | volume size profile only (blocking key, about 27.5 bits) | everything else |
| Size-preserving edit (e.g. MKV title) | size, untouched IFSC slices | full MD5/CRC |

## Rust and dependencies (applying the project rule)

- **Use:** `crc32fast` (821M downloads; `Hasher::combine`), RustCrypto `md-5`/`sha1`/`sha2`, `xxhash-rust` or `twox-hash` for persisted keys.
- **Write our own:**
  - PAR2 packets + GF16 (about 650-900 lines)
  - RAR4/RAR5 headers (about 600 lines)
  - 7z headers (about 600 lines)
  - yEnc (about 150 lines)
  - murmur3/imohash, MinHash/Bloom
  - the srrDB client (a small HTTP GET + JSON)
- **Reuse inside the project:**
  - the archive-metadata thread's `proto/{rar_meta,sevenz_zip_meta,par2_sfv_srr_meta}.rs`
  - the Opus payload thread's std-only CRC32 GF(2) toolkit (`opus-tools/w9b_crc32gf2/`)
- **Test files:**
  - rarfile `test/files` (106 RAR3/RAR5 archives with `.exp` oracles, ISC believed): the best single corpus.
  - par2cmdline `tests/*.tar.gz` are GPL, so download them at test time rather than vendoring them.
  - sanderjo/NZBs has no license, so don't commit it.
  - Generate yEnc, PAR2 and 7z fixtures ourselves, as in `tools/`.

## Suggested build order (payload side, after the sister report's step 1: store pcrc32 and size)

1. Parse the PAR2 index fully. Store FileDesc MD5 + length, MD5-16k, the IFSC list hash, Creator and slice size per file.
2. Read the first article of the first recovery volume and store a hash of the exponent-0 slice prefix, keyed by (slice size, prefix hash).
3. Read the first and last RAR volume headers. Store inner names, CMT, inner size + CRC, mtime and host OS.
4. Add a rate-limited srrDB lookup on (inner CRC, size) for unencrypted RAR sets, with a persistent hit/miss cache. Keep srrDB calls light: its robots.txt disallows `/api/` and no usage policy is published.
5. Add SubDB next to moviehash, and AltMount v1 for unarchived files.
6. Annotate only: marker files, scene file-set shape, re-uploader suffixes, UOWNER/OS, Creator.

## Open gaps

- QuickPar defaults, and par2j's exact rounding in source.
- Whether ngPost and Nyuu obfuscation create PAR2 before or after renaming, which decides whether FileDesc leaks the real name.
- srrDB response shapes are taken from client code, not seen live. The `archive-size` meaning is UNVERIFIED.
- Marker-file hashes: compute them from real posts in Atlas's own data, since public sources have almost none.
- Real-data rates at Atlas scale: the share of posts with PAR2, with exponent 0 present, using default slice sizes, or with `-hp`. These need a read-only sample run.

Addendum (11:40Z): the Claude Doc version is https://claude.ai/artifact/4H6dyXh6RETqWKAK8v7phC. Following the Opus payload thread's wave 10, no key merges on its own: a twin needs size + CRC32 + a 16k or full MD5, and exponent 0 or IFSC can serve as the second proof. Open gap: the yEnc-headers thread saw a 7-volume RAR set posted 4 times in 43 minutes with regenerated PAR2. Test whether its exponent-0 slices match even though its Set IDs differ.
