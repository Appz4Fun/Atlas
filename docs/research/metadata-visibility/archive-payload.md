# Atlas map: archive and payload area

Repo: /home/claude/appz4fun/atlas, local `main` at 55d2e51 ("Follow archives inside archives, up to 10 layers (#52)").
Note: `origin/main` is one commit ahead, 67504d6 "Decode bzip2, deflate64, compress and lz4 layers (#56)", which adds
`src/probe/codec/{bzip2,inflate,lz4,lzw,mod}.rs` and changes `src/probe/nest.rs` (53 lines) and `src/probe/nest/tests.rs`.
Everything below cites 55d2e51 unless marked.

Files read in full: src/archive/{mod,rar,sevenz,zip,steps,store,select,names}.rs, src/archive/tests.rs (password, select
and plan tables), src/archive/testkit.rs (function list), src/par2.rs, src/nzb.rs, src/duplicates.rs, src/release_tags.rs,
src/nfo.rs, src/probe/{mod,step,archive,nest}.rs, src/release_info.rs, src/postprocess/{schema,step,steps,reprocess}.rs,
src/postprocess/engine.rs (seed/walk parts), src/postprocess/mod.rs (rerun), src/web.rs (release page, probe panels,
copies, fact helpers), src/api.rs (release_attrs, rss), src/store.rs (shard schema, FileInfo, migrate_shard),
src/naming/steps.rs and src/predb/step.rs (readers of archive tables), src/compact.rs (table copying),
docs/todo-duplicate-clusters.md, config.example.toml, README.md (archive section), the PR branches pr/48, 51, 54, 57, 58.

---------------------------------------------------------------------------------------------------------------------

## 1. Pipeline: how archive and payload data is produced

### 1.1 Format detection by bytes and by name

- `archive::identify(head)` (src/archive/mod.rs:51-88) decides by magic only: `Rar!\x1a\x07\x01\x00` -> Rar5,
  `Rar!\x1a\x07\x00` -> Rar4, `7z\xbc\xaf\x27\x1c` -> SevenZip, `PK\x03\x04` / `PK\x05\x06` / (`PK\x07\x08`|`PK00`)+local
  -> Zip, `PAR2\0PKT` -> Par2, image magics -> Image, media magics (EBML, ftyp, AVI, MPEG-PS, fLaC, OggS, ID3) -> Media,
  else Unknown. There is NO PAR1 (`PAR\0\0\0\0\0`), PAR3 (`PAR3\0PKT`), ACE, ARJ, LZH, tar, gzip, bzip2, xz, zstd, ISO,
  .rev or .crc detection here.
- `select::classify(filename)` (src/archive/select.rs:45-78) uses the `KIND` regex (select.rs:15-24):
  `.volN+M.par2` -> Par2Vol(n), `.par2` -> Par2Base, `.partN.rar` -> Rar(n), `.rar|.cbr` -> Rar(0), `.rNN` -> Rar(n+1),
  `.sNN` -> Rar(n+1001), `.7z.NNN` -> SevenZip(n), `.7z` -> SevenZip(0), `.zNN` -> Zip(n), `.zip|.zipx|.cbz` -> Zip(MAX),
  `.NN`/`.NNN` -> Numbered(n), `.nfo|.txt` -> Nfo, images -> Image, and a long list
  (`.mkv .mp4 ... .iso .img .bin .exe ... .sfv .srr .srs .nzb .url .diz .md5 .sha1 ... .tar .gz .tgz .bz2 .xz .ace .cab`)
  -> NotArchive; anything else -> Unidentified. Confirmed by the table test src/archive/tests.rs:604-645 (`a.001` ->
  Numbered(1), `a.iso` -> NotArchive, `a.cbz` -> Zip, `a.cbr` -> Rar(0)). So raw `.001` splits ARE probed as a
  Numbered volume family (select.rs:104-110); the catalog line "`.001` is NotArchive" is wrong for this commit, but a
  `.crc` sidecar is never read.

### 1.2 RAR parsing in the archive step (src/archive/rar.rs)

RAR4 `list4` (rar.rs:29-87):
- walks 7-byte block headers (type at +2, flags le16 at +3, size le16 at +5), MAX_BLOCKS 65536 (rar.rs:6).
- CRC16 (low half of CRC32) checked for block types 0x73, 0x74, 0x7a (rar.rs:44-52); an old main header with the
  comment flag 0x0002 covers only bytes 2..13 (rar.rs:46). A newsub 0x7a block is CRC-checked and then skipped: no arm
  reads CMT / AV / RR / ACL / STREAM / UO contents.
- main header 0x73 with flag 0x0080 (encrypted headers): `encrypted`, `encryption = Rar4Aes128`, `headers_encrypted`,
  return (rar.rs:53-59). Other main flags (volume 0x0001, solid 0x0008, new naming 0x0010, authenticity 0x0020,
  recovery record 0x0040, first volume 0x0100) are not kept.
- end block 0x7b -> `complete = true` (rar.rs:60-63); the END flag's volume number is not read.
- ADD_SIZE (flag 0x8000) read at +7 for skipping (rar.rs:65-71); the high half of PACK_SIZE comes from `file4`.
- file header 0x74 -> `file4` (rar.rs:90-113): reads UNP_SIZE (le32 at 11), NAME_SIZE (le16 at 26), HIGH_PACK/HIGH_UNP
  (le32 at 32/36 when flag 0x0100, name then at 40 else 32). Flag 0x0004 -> `encrypted`, Rar4Aes128 (rar.rs:104-107).
  Directory (flags & 0x00e0 == 0x00e0) skipped (rar.rs:108). Pushes name + unpacked size only (rar.rs:110).
  DROPPED: PACK_SIZE value, HOST_OS (+15), FILE_CRC (+16), FTIME (+20), UNP_VER (+24), METHOD (+25), ATTR (+28),
  SALT, EXT_TIME, solid flag 0x0010, split-before/after flags 0x0001/0x0002, dictionary bits 0x00e0.
- Unicode names: flag 0x0200, RAR4 packed unicode decoder `unicode4` (rar.rs:115-173), bounded by MAX_NAME_BYTES 512
  (mod.rs:26).

RAR5 `list5` (rar.rs:192-235) and `block5` (rar.rs:239-292):
- header CRC32 (le32 at +0) checked over size-vint + header (rar.rs:216-219); header size vint capped at 3 bytes.
- type 4 (archive encryption header): `encrypted`, Rar5Aes256, `headers_encrypted`, and `password_check` from
  `password_check(body, has_iv=false)` (rar.rs:250-256).
- type 5 (end): `complete` (rar.rs:257).
- type 2 (file) and 3 (service): reads file_flags, unpacked size, `_attributes` DROPPED (rar.rs:261); skips mtime 4
  bytes when file_flags&2 (rar.rs:262-264) and data CRC32 4 bytes when file_flags&4 (rar.rs:265-267) WITHOUT keeping
  them; `_compression` vint DROPPED (rar.rs:268: version bits 0-5, solid bit 6, method bits 7-9, dictionary bits 10-13);
  `_host` DROPPED (rar.rs:269); name UTF-8 lossy (rar.rs:275, 285). Only type 2 pushes a file (rar.rs:276-287); the
  service header's name (CMT, QO, ACL, STM, RR) is parsed and dropped.
- extra area of a type 2 header: only record type 1 (file encryption) is looked for (`encrypted_record` rar.rs:392-406,
  `record_check` rar.rs:377-389): sets `encrypted`, Rar5Aes256, and a `password_check` from the record with has_iv=true.
  Records 0x02 hash (BLAKE2sp), 0x03 htime, 0x04 version, 0x05 redirection, 0x06 unix owner, 0x07 service are NOT read.
- type 1 (main archive header: volume flags, volume number, solid, recovery record, lock, locator) falls through
  `_ => {}` (rar.rs:289): nothing read.
- `PasswordCheck { kdf: u8, salt: [u8;16], check: [u8;8] }` (rar.rs:296-300); `to_bytes` = kdf || salt || check, 25
  bytes (rar.rs:307-312); `from_bytes` (rar.rs:314-320). `password_check()` (rar.rs:359-374) reads version (dropped),
  flags (needs bit 1 = check present), kdf byte, 16-byte salt, SKIPS the 16-byte IV when has_iv (rar.rs:370), takes the
  8-byte check and validates it against the 4-byte SHA-256 prefix (rar.rs:372-373). The IV is never kept.
- `accepts(password)` (rar.rs:323-335): PBKDF2-HMAC-SHA256 first block with 2^kdf + 32 rounds (rar.rs:327), folded to
  8 bytes; refuses to run when kdf > MAX_KDF 20 (rar.rs:303) -> `None`, "not tried" on the page.

### 1.3 7z parsing (src/archive/sevenz.rs)

- `start(head)` (sevenz.rs:36-44): magic, major version byte 6 == 0, start-header CRC over bytes 12..32, returns
  `Start { offset, size, crc }` of the next (end) header. Parsed only; nothing stored.
- `run` (sevenz.rs:463-492): end header must lie in `head` or in `tail`; otherwise `need_more_tail` (sevenz.rs:482-485).
  CRC of the next header checked (sevenz.rs:487). `nested` (sevenz.rs:495-540) unpacks a kEncodedHeader (0x17) up to
  MAX_ROUNDS 4 (sevenz.rs:15) through LZMA only (sevenz.rs:513-516, lzma_rs with MAX_DECODED 16 MiB, sevenz.rs:433-449);
  an AES coder in the encoded header -> `encrypted`, SevenZipAes256, `headers_encrypted`, stop (sevenz.rs:506-511). The
  AES coder properties (NumCyclesPower, IV, salt) are read into `Folder.coders` (sevenz.rs:172-184) but never kept.
- `streams` (sevenz.rs:206-315): pack_pos, pack sizes (kept in the struct for header decoding), pack digests read and
  dropped (sevenz.rs:223-225); folders with coders (id + props) and bind pairs; folder unpack sizes; folder CRCs kept
  in `Folder.crc` (sevenz.rs:253-257) but only used to verify a decoded header (sevenz.rs:534); substream sizes derived
  (sevenz.rs:279-294); per-substream CRC32 digests READ AND DISCARDED (sevenz.rs:295-304).
- `files` (sevenz.rs:335-396): property 0x0e empty-stream bits, 0x0f empty-file bits, 0x11 names (UTF-16LE); every
  other property (0x12 CTime, 0x13 ATime, 0x14 MTime, 0x15 attributes, 0x16 anti, 0x18 start pos, 0x19 dummy) is
  `_ => {}` (sevenz.rs:363). Pushes name + size (empty files size 0). `complete` only when every name fit
  (sevenz.rs:390-395).
- `header` (sevenz.rs:398-430): any AES coder in the main streams -> `encrypted`, SevenZipAes256 (sevenz.rs:418-421).
  There is no 7z `PasswordTest` variant (mod.rs:167-170), so a 7z password is never verified.

### 1.4 ZIP parsing (src/archive/zip.rs)

- `list_head` (zip.rs:209-254): local headers; flags le16@6, method le16@8, uncompressed le32@22, compressed le32@18,
  name len @26, extra len @28 (zip.rs:221-222). The local CRC32 (@14) is NOT read here (the probe reads it, see 1.7).
  `is_encrypted` = flags & (0x0001|0x0040|0x2000) or method 99 (zip.rs:36-38); `zip_encryption` 99 -> ZipAes else
  ZipCrypto (zip.rs:40-42). First encrypted entry's `password_check` (zip.rs:194-207): AES -> strength from extra 0x9901
  byte 8 (zip.rs:182-191; vendor version and actual method dropped), salt (strength*4+4 bytes) + 2-byte verifier;
  ZipCrypto -> 12 header bytes + check byte (time high byte when bit 3 set, else CRC high byte). zip64 extra 0x0001
  (zip.rs:49-72). Names: UTF-8 when flag 0x0800, else UTF-8 if valid else Latin-1 (zip.rs:26-34). Directories skipped
  (zip.rs:44-46). Stops at a data-descriptor entry with packed == 0 (zip.rs:244-246). DROPPED: version needed, DOS
  time/date, other extra fields (0x5455 UT, 0x7875 uid/gid, 0x000a NTFS, 0x7075 unicode path).
- `list_tail` (zip.rs:257-337): finds the EOCD in the last 65557 bytes (zip.rs:11, 259-266), entries @10, size @12,
  zip64 EOCD (zip.rs:276-288); central entries: flags @8, method @10, packed @20, unp @24, n @28, e @30, k @32
  (zip.rs:304-312); the file comment (k bytes) is bounds-checked and skipped (zip.rs:317). DROPPED: version made by
  (@4, host OS), internal/external attributes (@36/@38), disk number, archive comment. `complete = whole && seen ==
  entries` (zip.rs:335).
- `list(head, tail)` (zip.rs:340-349): the tail listing wins when it has files; `encrypted` OR-ed and the head's
  `password_check` carried over.
- `PasswordCheck::accepts` (zip.rs:113-124): ZipCrypto key schedule (zip.rs:128-152); AES PBKDF2-HMAC-SHA1 1000 rounds
  (zip.rs:155-179). `PasswordTest::certain()` is false for ZipCrypto (mod.rs:204-206) -> "PROBABLY WORKS".
- Serialisation tag: `PasswordTest::to_bytes` prefixes zip checks with 0xff; RAR5 checks start with kdf (< 0xff)
  (mod.rs:172-193). A new variant needs a new tag.

### 1.5 Listing and Password (src/archive/mod.rs)

- `InnerFile { name, size: Option<u64> }` (mod.rs:160-163).
- `Listing { files, encrypted, headers_encrypted, encryption, password_check, complete, damaged, need_more_tail }`
  (mod.rs:211-228). `push` cleans names and caps at MAX_ENTRIES 4096 (mod.rs:25, 247-255).
- `Listing::password()` (mod.rs:234-244): Yes if encrypted; empty list: No when complete && !damaged, else Maybe;
  all names archive-like (`ARCHIVE_FILE` regex mod.rs:230-231) -> Maybe; else No. Codes 0/1/2 (mod.rs:97-113).
- `Encryption` codes 1..5 and labels "RAR4, AES-128", "RAR5, AES-256", "7z, AES-256", "ZIP, ZipCrypto", "ZIP, AES"
  (mod.rs:118-157).
- Constants: MAX_STORED 64 rows per source (mod.rs:27), ARTICLE_CAP 4 MiB (mod.rs:28), PROBE_PREFIX 64 KiB (mod.rs:29),
  PROBE_MIN_PARTS 3 (mod.rs:30), NAME_SOURCE_NFO 10 / INNER 15 / PAR2 20 (mod.rs:31-33), RERUN_ON_INNER_FILES =
  naming step 8 | predb step 16 (mod.rs:36).

### 1.6 PAR2 parsing (src/par2.rs)

- `inventory(data)` (par2.rs:61-135) resyncs on the `PAR2\0PKT` magic (par2.rs:68-77), accepts packet lengths >= 64
  and multiple of 4 (par2.rs:79), marks `truncated` when a shaped packet runs past the data (par2.rs:83).
  Packet kinds handled: `PAR 2.0\0Main\0\0\0\0` -> `block_size` (u64 at body 0) and file ids from body offset 12 on
  (par2.rs:93-101: NOTE the recoverable-file count at body offset 8 is ignored, so non-recoverable ids are read into
  `mains` too); `PAR 2.0\0FileDesc` -> `Par2File { set_id, file_id, md5, md5_16k, size, name }` (par2.rs:102-114);
  `PAR 2.0\0UniFileN` -> UTF-16LE name replaces the ASCII one for the same (set, file id) (par2.rs:115-131).
  NOT handled: IFSC, RecvSlic, Creator, Comment/UniComnt, Pkd* packets. Packet MD5 (bytes 16..32) is never verified.
- `complete` = a Main was seen and every Main id has a FileDesc (par2.rs:132-133). MAX_FILES 4096 (par2.rs:24).
- `file_descriptions`, `display_name` helpers (par2.rs:138-152); `is_base_par2(subject)` (par2.rs:19-21).
- So md5, md5_16k, file_id and set_id ARE parsed into memory (par2.rs:30-37) but the only consumer is
  `duplicates::twin_fingerprint` (steps.rs:247) which folds name, size, md5 and set_id into one SHA-1 (duplicates.rs:64-89).
  md5_16k and file_id are unused after parsing.

### 1.7 Probe-side archive head and nesting (src/probe/archive.rs, src/probe/nest.rs)

The probe step (rank 120) decodes the main file's first article whole and runs `inspect` (src/probe/step.rs:397-402):
`info.archive = archive::parse(body)`, `nest::unwrap(body)` for layers and `media::parse(nest.inner)`.

- `ArchiveHead` (probe/archive.rs:11-35) serialised fields: `format` ("RAR4" | "RAR5" | "7z" | "ZIP"), `encrypted`,
  `headers_encrypted`, `method` (RAR m0..m5, ZIP method name), `name`, `unpacked`, `packed`, `crc32`. `#[serde(skip)]`:
  `stored_at`, `continues`, `data_at`, `dir` (computed per probe, never stored).
- RAR4 (probe/archive.rs:85-147): PACK_SIZE @7 (+ high word @32), UNP_SIZE @11 (+ @36), FILE_CRC @16, METHOD @25 as
  `m{n}`, name, `encrypted` flag 0x0004, `continues` flag 0x0002, `dir`, `stored_at` when method 0x30 and not
  encrypted (probe/archive.rs:111-128). Encrypted headers 0x0080 -> one entry saying so (probe/archive.rs:97-107).
- RAR5 (probe/archive.rs:162-241): `packed` from the block header's data size, `continues` from block flag 0x10, CRC32
  when file_flags&4 (probe/archive.rs:216-219), `_attributes` and `_host` DROPPED (probe/archive.rs:212, 221),
  compression info -> method bits 7-9 only (probe/archive.rs:232-233); mtime skipped (probe/archive.rs:213-215);
  `encrypted` via `has_crypt_record` (probe/archive.rs:243-254). PR #58 adds `rar_version` (info & 0x3f) and `solid`
  (info & 0x40) as serde(skip) fields (pr/58 diff of probe/archive.rs).
- ZIP (probe/archive.rs:256-304): method name via `zip_method` (probe/archive.rs:307-320), CRC32 @14 unless the
  data-descriptor flag 0x0008 (probe/archive.rs:286), sizes incl. zip64 (probe/archive.rs:289-295), `stored_at` for
  method 0 unencrypted. Walks up to MAX_ENTRIES 256 local headers (probe/archive.rs:65) for `entries()`.
- 7z: `format: "7z"` only (probe/archive.rs:42-43); the nest module decides copy/lzma/lzma2 from the first body byte
  (nest.rs:309-339) and reads the start header's NextHeaderOffset to bound the packed streams (nest.rs:312-317).
- `nest::Layer` (nest.rs:25-51): `format` (RAR4, RAR5, 7z, ZIP, TAR, GZIP, XZ, LZMA, LZIP, ZSTD, BZIP2, COMPRESS, LZ4,
  LZOP), `method`, `name`, `unpacked`, `offset`, `stored`, `encrypted`, `stop` (reason text), `capped`; `continues` is
  serde(skip). `unwrap` (nest.rs:91-156): MAX_DEPTH 10, MAX_DECODED 16 MiB per look, MAX_RATIO 100 (nest.rs:16-20).
  `open` (nest.rs:178-226) recognises RAR/zip (biggest reachable entry, nest.rs:229-285), 7z, gzip (with original name,
  nest.rs:370-410), xz (filters named, LZMA2 only decoded, nest.rs:434-480), lzip, zstd, `.lzma` alone (nest.rs:484-499),
  tar (ustar/GNU/pax names, biggest file, nest.rs:503-564); bzip2, compress, lz4, lzop are named with stop "no decoder
  for this yet" at 55d2e51 (nest.rs:209-225; #56 on origin/main adds decoders). Compressed RAR entries stop with "no
  decoder for this yet" (nest.rs:278-279); PR #58 adds a RAR5 decoder and "solid: needs the files before it".
  Codecs at 55d2e51: Deflate (zip), Gzip, Lzma, Lzma2, Zstd (nest.rs:72-82, 167-175).
- ProbeStep (probe/step.rs:19-213): id 4, rank 120, enabled by `cfg.archive.enabled` (probe/step.rs:86-89),
  `config_stamp() = 1 | 2` (probe/step.rs:99-101; bit 2 = nesting), reruns on growth. It also reads up to 3 small text
  files (NFO/SFV/TXT): SFV -> `info.sfv`, NFO -> `info.nfo` (<= 256 KiB, probe/step.rs:22), `info.imdb_ids`, and an NFO
  password -> `SET_NFO_PASSWORD` (probe/step.rs:110-130). Writes `SET_PROBE` with the JSON (probe/step.rs:200-207).

### 1.8 The two archive steps (src/archive/steps.rs)

Registered in `postprocess::steps::all()` (src/postprocess/steps.rs:170-186): Par2Step, ListStep, ProbeStep,
OpenSubtitles, Naming, NfoName, PredbMatch, Category, media steps. Ranks must be unique and in the part's range
(steps.rs:194-212 test).

Par2Step (`archive_par2`, StepId 1, rank 100, Needs::Bodies, steps.rs:185-270):
- skips releases named as an image (steps.rs:31-34, 206-208); reruns on growth, not on rename (steps.rs:209-211).
- picks `select::par2_source` (select.rs:141-154): a base `.par2` with the fewest parts, else the recovery volume with
  the lowest first block; fetches its first article whole (`fetch_one`, ARTICLE_CAP; steps.rs:53-61, 216-217); when the
  inventory is incomplete fetches one more article (base: First(2); volume: Last(1)) and re-parses (steps.rs:219-234).
- `covers_all` = complete && one set id && FileDesc count == non-.par2 file count (steps.rs:240-243).
- `posted` twin fingerprint = `duplicates::twin_fingerprint(files, block_size, posted(name, expected|parts_seen))`
  when covers_all (steps.rs:244-248). `sizes` = (block_size, sum ceil(size/block)) when covers_all (steps.rs:249-253).
- `finish(SOURCE_PAR2, listing_of(inventory))` (steps.rs:254; `listing_of` steps.rs:72-76 keeps name + size only);
  when !covers_all the SET_CONTENT write is removed and CLEAR_POSTED added (steps.rs:256-260); SET_POSTED
  (steps.rs:261-264) and SET_PAR2_SIZES (steps.rs:265-268).

ListStep (`archive_list`, StepId 2, rank 110, steps.rs:367-430):
- budget `left = archive.max_bodies_per_release` (default 4, config.example.toml:68) bodies; probe spacing
  60 s / `probes_per_minute` (default 60) via `next_probe_slot` (steps.rs:275-279, 290-299).
- `select::plan(files, probe_files)` (select.rs:96-137): groups named volumes into (family, stem) sets, keeps the
  MAX_SETS 2 biggest by parts (select.rs:27, 117-125), reads the first 2 volumes for RAR (each has headers) and 1 for
  7z/zip/numbered, `tail` = the set's last volume; with no named set, probes up to `probe_files` Unidentified files of
  >= PROBE_MIN_PARTS parts, largest first, unless images are half the files (select.rs:128-136). A probe fetches only
  the first 64 KiB (`fetch_start`, steps.rs:63-70, 398-401).
- `identify(body)`: RAR -> `rar::list(body)` -> `finish(SOURCE_RAR)`; Zip -> `zip_list` (steps.rs:316-327: the
  central directory from the same body if the zip is one article, else the set's last article, one budget unit);
  7z -> `seven_list` (steps.rs:329-365: last article, then the article before it if adjacent; a probe skips the tail
  when the start header says the file is longer than the set, steps.rs:341-352); Par2 under another name ->
  `listing_of(par2::inventory)` (steps.rs:415-422); Image -> stop; Media/Unknown -> next head (steps.rs:405-426).

`finish` (steps.rs:84-173), shared by both:
- rows = `collapse_volumes(listing.files)` (names.rs:104-128): one row per volume set (sizes summed), other files once,
  at most MAX_STORED 64 rows, in first-appearance order.
- quota: `max_names_mb` (config.rs:551; README.md:399 default 16384) split by SHARDS, counted in `meta
  archive_name_bytes` as name bytes + 32 per row (steps.rs:94-98, 111); over quota -> rows unstored and
  `archive_names_over_quota` counted (steps.rs:117-120), shown on the stats dashboard (engine.rs:326, 334).
- writes `DELETE_SOURCE` then `INSERT_FILE` per row with `seq = source * 65536 + i` (steps.rs:99-112), `ADD_NAME_BYTES`
  delta (steps.rs:114-116).
- content fingerprint when `listing.complete && !damaged && speaks_for_release` (steps.rs:124-131):
  PAR2 speaks when its FileDesc count equals the non-par2 file count; an archive speaks when `one_archive_set` says the
  release is one archive's volumes plus par2/nfo/image/sample/sfv/nzb (duplicates.rs:122-140). A PAR2 list of archive
  volumes leaves the content fingerprint alone (steps.rs:132-139).
- archive sources only: `UPSERT_RELEASE (release_id, password code)` and, when `listing.encryption` is Some,
  `SET_ENCRYPTION (encryption code, names_hidden, password_check bytes)` (steps.rs:140-153). NOTE: a later run that
  finds no encryption updates `password` but leaves `encryption/names_hidden/password_check` as they were.
- naming: `best_name` (names.rs:73-100) when the release has no better name and (PAR2 source, or obfuscated, or a
  random-looking stem), filtered by `naming::improves` (steps.rs:155-164).
- `rerun = RERUN_ON_INNER_FILES` when rows were stored (steps.rs:170), so naming (8) and predb (16) run again.

### 1.9 NFO, SFV, NZB, release tags

- src/nfo.rs: `name_candidates` (nfo.rs:91-120) reads at most NFO_MAX_TEXT 256 KiB (nfo.rs:42), skips `=newz[NZB]=`
  indexer NFOs (nfo.rs:94-96), blanks `Source:` fields and continuations (`identity_text`, nfo.rs:67-84), collects
  literal fields (Filename/Complete Name/Release Name), scene tokens, `<title>` tags, `Release:`/`Title:` lines, TV and
  movie lines, Title (Year). Output: a name only. No other template field (Group, Date, Source, Video, Audio, Genre,
  Rating, Subs, Greets), no banner hash, no line width. The NFO step writes a name with NameSource::Nfo and an NFO
  password (naming/steps.rs:110-125). Encoding: `probe::nfo_text` decodes CP437 when not UTF-8 (probe/mod.rs:151-161)
  and the choice is dropped. IMDb ids: `probe::imdb_ids`, at most 16 (probe/mod.rs:163-185). Other ids/URLs: none.
- SFV: `probe::parse_sfv` keeps `name -> crc32` and skips `;` comment lines (probe/mod.rs:187-203).
- src/nzb.rs `render_nzb_in` (nzb.rs:66-113): `<meta type="category">group</meta>` only (nzb.rs:73); `<file
  poster date subject>` (nzb.rs:83-89), `<groups>` (nzb.rs:90-94), `<segment bytes number>message-id</segment>`
  (nzb.rs:97-106). No password meta although `archive_release.password` and `release_nfo_password` exist.
  `duplicates::filled_nzb` renders the same with gaps filled from twins (duplicates.rs:527-540).
- src/release_tags.rs: PTT parse at render time, nothing stored (release_tags.rs:1-2); `Tags` has `proper`, `repack`,
  `remastered`, `hardcoded`, `dubbed`, `subbed` (release_tags.rs:25-30), but `web::name_tags` shows only resolution,
  hdr and quality (web.rs:720-732), and the facts use codec/bit depth/container/audio/channels/languages/edition/
  group/year/network/site (web.rs:1095-1121). The repack/proper flags are parsed and never shown.

---------------------------------------------------------------------------------------------------------------------

## 2. Data model (all per-shard tables; 8 shards, src/store.rs:35; release id = seq*8+shard, store.rs:44-49)

Shard core tables (src/store.rs:133-188): `releases(id, name, group_name, poster, posted_date, size, complete, parts,
file_total, display_name, is_obfuscated)` + added `name_source` (schema.rs:96); `files(id, release_id, filename, subject,
subject_part, subject_mid, expected, file_total, seen BLOB bitmap, touched_at, blob)`; `segments(file_id, local, domain,
part, bytes)` (no Date, no article number). Sealed files move segments into `files.blob` after SEAL_AGE 3 days
(store.rs:788).

Tables of this area (all registered in `SHARD_TABLES`, src/postprocess/schema.rs:78-92, which gives create-on-open,
`added` columns on open, a gc trigger on release delete, and copying by compaction, compact.rs:745-752):

| table | columns | written by | read by |
|---|---|---|---|
| `archive_inner_files` (archive/store.rs:18-39) | release_id, seq (source*65536+i), name TEXT, size INTEGER nullable, source INTEGER (1 PAR2, 2 RAR, 3 7z, 4 ZIP); PK (release_id, seq) without rowid; trigger `archive_names_give_back` refunds `meta.archive_name_bytes` | `finish` DELETE_SOURCE + INSERT_FILE (steps.rs:99-112) | `inner_files`/`rows` limit 256 (archive/store.rs:91-99, 128-130) -> `InnerView.files` for the release page (web.rs:1001, 1144-1161) and copy cards (web.rs:1471); naming step (naming/steps.rs:45, 59); predb step evidence + inner keys (predb/step.rs:185-199, 213-215) |
| `archive_release` (archive/store.rs:42-51) | release_id PK, password INTEGER NOT NULL (0 No, 1 Yes, 2 Maybe); added: encryption INTEGER (1..5), names_hidden INTEGER, password_check BLOB (25 bytes RAR5: kdf, salt16, check8; zip: 0xff tag + strength/salt/verifier or 0, check, 12 header bytes) | UPSERT_RELEASE, SET_ENCRYPTION (steps.rs:141-152) | `view_with` (archive/store.rs:171-181), `passwords_with` for result rows and the API (archive/store.rs:137-148; web.rs:569, api.rs:235, 274), naming step `password` (naming/steps.rs:58) |
| `release_nfo_password` (archive/store.rs:54-63) | release_id PK, password TEXT | probe step (probe/step.rs:122-127) and NFO naming step (naming/steps.rs:113-118), unverified | `nfo_password` (archive/store.rs:161-169) via `release_info::password_hint_with` (release_info.rs:541-542) |
| `release_fingerprint` (duplicates.rs:21-40) | release_id PK, posted BLOB (21 bytes: version 2 + SHA-1), content BLOB (SHA-1 of sorted sizes), indexes on posted/content where not null; added: par2_block INTEGER, par2_blocks INTEGER (data block count), posted_names BLOB (declared, NO writer or reader anywhere: dead column) | SET_POSTED / SET_PAR2_SIZES / CLEAR_POSTED (Par2Step, steps.rs:256-268), SET_CONTENT / CLEAR_CONTENT (`finish`, steps.rs:135-138); `forget_old_twins` clears old shapes at startup (duplicates.rs:97-117) | `copies_with` (duplicates.rs:224-251), `clusters_with`/`cluster_count_with` (duplicates.rs:289-335), `counts_with` (duplicates.rs:418-438), `later_copies` (duplicates.rs:448-466), `par2_sizes` (duplicates.rs:469-475) -> `Contents.par2_block/par2_blocks` (release_info.rs:196-199) |
| `release_probe` (probe/mod.rs:23-47) | release_id PK, crc32 INTEGER (indexed), agent TEXT (indexed), info TEXT (JSON of `ProbeInfo`), added moviehash TEXT (indexed) | SET_PROBE (probe/step.rs:200-207) | `probe::read` (probe/mod.rs:142-148) -> `probe_html` (web.rs:1163-1170, 1243-1453); never in the API |

`ProbeInfo` JSON shape (probe/mod.rs:53-78): file, yenc{...}, file_crc32, segment_size, overhead_percent,
continuity_ok, part_size_ok, part_crc_ok, health{first,middle,last}, headers{...}, crossposted_to[], archive
{format, encrypted, headers_encrypted, method, name, unpacked, packed, crc32}, layers[{format, method?, name?,
unpacked?, offset, stored, encrypted?, stop?, capped?}], media{...}, nfo, imdb_ids[], sfv{name: crc}, moviehash.
`#[serde(default)]` on layers (probe/mod.rs:70-71) and on Layer (nest.rs:24) make old rows readable.

Other state: `meta` keys `archive_name_bytes`, `archive_names_over_quota` (archive/store.rs:78-81), `pp_stamp_<id>`,
`pp_steps_seen`, `pp_walk_seeded` (engine.rs:1089-1111). Processing state: `pp_queue`, `pp_state(done, failed,
rerun_mask ...)`, `pp_walks(kind, top_id, before_id, redo_mask, group_name)` (schema.rs:21-76).

Not stored anywhere (computed when a page opens, src/release_info.rs:1-2): `Contents` (files, par2_files,
recovery_volumes, data_bytes, recovery_bytes, articles seen/expected, data/recovery article counts, recovery_blocks from
`.volN+MM` names, release_info.rs:39-60, 118-130, 160-201), `PasswordHint` (release_info.rs:529-564), `release_tags`.

---------------------------------------------------------------------------------------------------------------------

## 3. UI surface (src/web.rs)

Release page `release_page` (web.rs:989-1017) -> `release_html` (web.rs:1028-1201):
- header tags: media label, `password_tag` (PASSWORD / PASSWORD?, web.rs:698-704, 1041), COMPLETE/INCOMPLETE,
  OBFUSCATED, `name_tags` (resolution, HDR, quality; web.rs:720-732).
- facts `<dl class="facts">` through the `fact` closure (web.rs:1047-1052): Category, Title, Size, Posted,
  Group/Groups, Poster, Email, Posted as, Name from (release_info::name_source_label: "the PAR2 file", "a file inside
  the archive", "the NFO", web.rs:1091-1093, release_info.rs:24-35), Year, Ripped from, Site, Video, Audio, Languages,
  Edition, Release group, Parts, **Encryption** = `encryption_fact(inner)` (web.rs:1125, 1763-1771: label +
  ", file names hidden"; "Encrypted, type not recorded" for old rows; "Maybe encrypted"), **Password** =
  `password_fact(inner, hint)` when `release_info::password_hint` finds one (web.rs:1126-1128, 1741-1749:
  `<code>pw</code> found in <source>` + WORKS / PROBABLY WORKS / WRONG / ", not tried"; the verdict is computed at
  render by `PasswordTest::accepts`, never stored), **Files** = contents.files (web.rs:1129-1131), **PAR2 files** =
  "N (M recovery)" (web.rs:1132-1134), **Articles** = `health` "seen of expected, x% missing" + ALL THERE / REPAIRABLE /
  NOT ENOUGH RECOVERY (web.rs:1135-1137, 1779-1787), **Repair** = `repair_fact` exact `recovery_blocks/par2_blocks` or
  "about x% (size)" from bytes (web.rs:1138, 1752-1760), Links.
- actions: Download NZB, "NZB, gaps filled from a twin" when a twin exists and articles are missing (web.rs:1171-1180).
- `/ui/details` panel (media metadata, not this area).
- **Other copies** `copies_html` (web.rs:1181, 1555-1572): TWIN / ALT badges, up to FULL_COPIES 12 `copy_card`s
  (web.rs:1456, 1463-1548) each repeating Size, Group, Poster, Email, PAR2 files, Repair, Articles, Encryption,
  Password, Video, Audio, Languages, Ripped from, Release group, Links, poster thumb; the rest as links; the footer
  sentence defining twin/alt.
- probe panels `probe_html` (web.rs:1243-1453): Tracks, Chapters, yEnc (File name, File size, Parts, Line length,
  Article size, CRC32 link, Overhead, Checks, On the server), **Packing** (Format, Method, Encryption lock tag, First
  file, Unpacked, File CRC32; web.rs:1304-1325) + **Layers** = `nested_chain` when more than one layer or a non-archive
  layer (web.rs:1326-1328, 1840-1854: "RAR5 m0 › 7z copy › movie.mkv", stop reasons as red tags; offset/stored/
  capped/unpacked/encrypted of a layer are not shown), How it was posted, then NFO (`<pre>` + IMDb links,
  web.rs:1425-1432) and SFV table (web.rs:1433-1444).
- **Files inside** table: name and size of `inner.files` (web.rs:1144-1161). The `source` column (PAR2/RAR/7z/ZIP) and
  `seq` are not shown; rows are the collapsed, <= 64-per-source rows.
- Result rows (`row`, web.rs:744-790): "N TWIN(S)" tag from `duplicates::counts` (web.rs:769-772), password tag
  (web.rs:780). Duplicates page `duplicates_page` (web.rs:1575-1660) lists clusters.

API (src/api.rs): `release_attrs` emits poster, group, resolution, video, audio, hdr, source, language, team, twins,
alts (api.rs:285-310); `password` attr with the code 1/2 (api.rs:330-332). Nothing else from this area reaches the API;
`release_probe` JSON is HTML only.

---------------------------------------------------------------------------------------------------------------------

## 4. Twins and alts (src/duplicates.rs)

- Twin key `posted` (duplicates.rs:64-89): SHA-1 over set_id, block_size, number of posted files, every posted
  (name, parts) sorted, every FileDesc (name, size, md5) sorted and deduped; prefixed by TWIN_VERSION 2 (duplicates.rs:92).
  Written only when the PAR2 set covers every posted file (steps.rs:243-248). Not rename-proof (names are hashed), not
  per file.
- Alt key `content` (duplicates.rs:173-197): SHA-1 over the sorted sizes >= MIN_CONTENT_FILE 1 MiB (duplicates.rs:56) of
  the listed inner files; archives in the list -> None; samples/proof skipped; a missing size -> None. No CRCs, no names.
- `copies_with`: twins by equal `posted`, alts by equal `content` minus twins, MAX_COPIES 50 per shard
  (duplicates.rs:57, 205-251). `counts_with` for result pages (duplicates.rs:418-438, api twins/alts attrs).
- `fill_gaps`: articles borrowed from a twin per file when name, total parts and part sizes match (duplicates.rs:479-519).
- `forget_old_twins` (duplicates.rs:97-117) is the existing precedent for a startup backfill: SQL per shard queues
  affected releases into `pp_queue`, clears the PAR2 step's done/failed bits and nulls the old columns.

---------------------------------------------------------------------------------------------------------------------

## 5. Backfill and rerun machinery

1. Step `config_stamp()` bits (src/postprocess/step.rs:58-62): on start and config reload, `seed_shard`
   (engine.rs:1073-1114) compares each enabled step's stamp with `meta.pp_stamp_<id>` and, when a bit is new on a
   seeded shard, inserts a `pp_walks` row kind 'steps' with `redo_mask = that step's bit` over all releases newest
   first (engine.rs:1103-1112). `walk_tick` (engine.rs:412-486) visits `backlog_per_second / SHARDS` rows per shard per
   second (engine.rs:421; default 2000, config.example.toml:53) and runs the redo steps. The probe uses it
   (probe/step.rs:99-101); Par2Step and ListStep do NOT override `config_stamp` (default 0), so adding one to them is
   the hook for "re-read every release's archive/PAR2 headers".
2. `atlas --reprocess --step archive_par2 --step archive_list [--group g] --apply` (reprocess.rs:74-99, step names at
   steps.rs:197 and 372) queues a 'reprocess' walk per shard (reprocess.rs:25-40, 137-162); without `--apply` it only
   counts affected releases (reprocess.rs:110-135).
3. `Stage::rerun(release_id, mask)` / `rerun_on` (postprocess/mod.rs:63-78, 117-123) per release.
4. Step outcomes: `Outcome.rerun` bits for later steps (step.rs:213-214), used by `finish` (steps.rs:170) and the probe
   (probe/step.rs:209).
5. `Reruns { on_growth: true }` for both archive steps and the probe (steps.rs:209-211, 384-386; probe/step.rs:93-95):
   more articles arriving re-runs them.
6. Startup SQL requeue like `forget_old_twins` (duplicates.rs:97-117).

Costs: every archive/PAR2 backfill is body traffic. Par2Step reads 1-2 articles, ListStep up to `max_bodies_per_release`
(4) with 64 KiB prefixes for probes; the probe step shares `body_fetches_per_minute` (600, config.example.toml:54) and
`body_connections` (2). At ~3-6 articles per release and 600 fetches/min that is roughly 100-200 releases per minute
at best, i.e. days for a million releases; the walk is newest first (engine.rs:436-437).

Schema evolution: nullable columns on existing shard tables are appended through `ShardTable.added`
(schema.rs:16-17, 102-110) at open (`store::migrate_shard` -> `schema::migrate`, store.rs:246; schema.rs:114-131);
a new table needs a `ShardTable` const in `SHARD_TABLES` (schema.rs:78-92) or compaction refuses the shard
(compact.rs:900-915); indexes are dropped and rebuilt during compaction (compact.rs:747-749). JSON growth in
`release_probe.info` needs no migration but only indexed columns are searchable (probe/mod.rs:31-44).

---------------------------------------------------------------------------------------------------------------------

## 6. Catalog coverage, item by item (visibility, from the code)

Legend: shown = on the release page or a panel it renders; stored_not_shown = in SQLite/JSON, no UI;
parsed_not_stored = read into memory and dropped; not_collected = never read.

### payload: par2
- Main.block_size: stored_not_shown. `Inventory.block_size` (par2.rs:95) -> `release_fingerprint.par2_block`
  (steps.rs:249-253, 265-268); the page shows only the derived Repair % (release_info.rs:93-96; web.rs:1752-1760).
- Main.file ids (recoverable + non-recoverable): parsed_not_stored (par2.rs:96-101; the count at body offset 8 is
  ignored so both lists merge; only the `complete` check uses them, par2.rs:132-133).
- Main.number of data blocks (derived): stored_not_shown. `par2_blocks` = sum ceil(size/block) (steps.rs:251), used
  in Repair % and `repairable` (release_info.rs:77-83) but the number itself is not printed.
- FileDesc.name, size: shown. `Par2File` -> `listing_of` -> `archive_inner_files` source 1 (steps.rs:72-76, 99-112) ->
  "Files inside" (web.rs:1144-1161). Also "Name from: the PAR2 file" (web.rs:1091-1093).
- FileDesc.md5, md5_16k, file_id: parsed_not_stored (par2.rs:103-113); md5 folded into the twin SHA-1 only
  (duplicates.rs:66, 80-85); md5_16k and file_id unused.
- UniFileN: shown (the unicode name replaces the ASCII name, par2.rs:126-131, and flows to the table); the
  ASCII/UTF-16 disagreement itself is dropped.
- IFSC: not_collected (no arm in par2.rs:93-124).
- RecvSlic: not_collected (packet ignored); the recovery volume count and `+MM` block counts come from file NAMES at
  page time (release_info.rs:118-130, 176-180) and are shown as "PAR2 files N (M recovery)" and in Repair %.
- Creator: not_collected.
- Comment / UniComnt: not_collected.
- PkdMain / PkdRecv / PkdIFSC: not_collected.
- set_id: parsed_not_stored (par2.rs:89, 113; one-set check steps.rs:241-242; first input of the twin SHA-1,
  duplicates.rs:72).
- par2_layout: shown for counts and redundancy (PAR2 files, recovery volumes, Repair % exact or estimated,
  web.rs:1132-1138); slice size stored_not_shown (par2_block); "covers nfo/sfv/sample" and the slice-size habit are
  not_collected (nothing compares FileDesc names with the sidecars beyond the covers_all count, steps.rs:240-243).

### payload: sfv, nfo, rescene
- sfv.entries: shown (`release_probe.info.sfv`, probe/step.rs:118-119; SFV table web.rs:1433-1444).
- sfv.comments: not_collected (probe/mod.rs:192-194 skips `;` lines).
- nfo.release_name: shown ("Name from: the NFO" web.rs:1091-1093; the NFO text itself web.rs:1430-1432;
  nfo.rs:91-124; naming/steps.rs:119-123).
- nfo.ids_and_urls: shown for IMDb only (probe/mod.rs:166-185, web.rs:1425-1432); TMDb/TVDB/MusicBrainz/... URLs
  not_collected.
- nfo.template_fields: not_collected as fields; the raw text (<= 256 KiB) is shown whole in `<pre class="nfo">`
  (web.rs:1430-1432), so a reader sees them but nothing parses, stores or indexes them.
- nfo.password: shown (`release_nfo_password` written by probe/step.rs:122-127 and naming/steps.rs:113-118; read by
  release_info.rs:541-542; Password fact web.rs:1126-1128, 1741-1749 with the live verdict). The verdict is
  parsed_not_stored (computed per render, web.rs:1742).
- nfo.encoding_and_shape: parsed_not_stored (CP437 vs UTF-8 decided in probe/mod.rs:151-156; line width and banner
  hash not_collected).
- rescene.srr / srs / srrdb_lookup: not_collected (`.srr`/`.srs` are NotArchive in select.rs:21; no reader).

### payload: rar4
- main_header_0x73 encrypted-headers flag 0x0080: shown (rar.rs:53-59 -> archive_release.encryption=1,
  names_hidden=1 -> "RAR4, AES-128, file names hidden", web.rs:1763-1771; probe Packing lock tag "NAMES AND FILES
  ENCRYPTED" web.rs:1308-1315). Other main flags (volume, comment, lock, solid, authenticity, recovery record, first
  volume, POS_AV) and the comment: not_collected (comment flag used only for CRC coverage, rar.rs:46).
- file_header_0x74 name + UNP_SIZE (+HIGH): shown ("Files inside", rar.rs:91-110; probe "First file"/"Unpacked"
  web.rs:1316-1323).
- file_header_0x74 FILE_CRC, METHOD (first file of the main archive only): shown in the Packing panel ("Method"
  m0..m5, "File CRC32", web.rs:1307, 1324; probe/archive.rs:113-115). The CRC is per volume slice for split files.
- file_header_0x74 PACK_SIZE (first file): stored_not_shown (`ArchiveHead.packed` in release_probe JSON,
  probe/archive.rs:111; no `dd` for it in web.rs:1304-1325).
- file_header_0x74 encrypted flag 0x0004: shown (rar.rs:104-107 -> Encryption fact; probe lock tag).
- file_header_0x74 split flags / directory flag: parsed_not_stored (`continues`, `dir` serde(skip), probe/archive.rs:
  26-34, 123-124; rar.rs:108 skips dirs).
- file_header_0x74 HOST_OS, FTIME, UNP_VER, ATTR, dictionary bits, SALT, EXT_TIME, solid flag: not_collected
  (rar.rs:91-98 reads only +11 and +26; probe/archive.rs:108-129 reads +7, +11, +16, +25, +26).
- newsub_0x7a (CMT, AV, RR, ACL, STREAM, UO): not_collected (CRC-checked then skipped, rar.rs:44-52, no match arm).
- end_0x7b: parsed_not_stored (`complete`, rar.rs:60-63); volume number and data CRC not read.
- volume_scheme: parsed_not_stored. `stored_at`/`continues` (probe/archive.rs:24-28) feed the moviehash byte map
  (probe/step.rs:324-361); the naming scheme is visible only through file names; the volume count is folded into
  "Files"; the volume size is not computed (release_info.rs:182-195 sums bytes per class only).

### payload: rar5
- main_header_type_1 (archive flags, volume number, solid, recovery record, lock, locator): not_collected
  (rar.rs:289 `_ => {}`; probe/archive.rs:194 `_ => {}`).
- encryption_header_type_4: encryption kind shown ("RAR5, AES-256, file names hidden", rar.rs:250-256 ->
  SET_ENCRYPTION); kdf, 16-byte salt and 8-byte check: stored_not_shown (archive_release.password_check,
  rar.rs:307-312; the page shows only WORKS/WRONG, web.rs:1741-1749); version and flags: parsed_not_stored
  (rar.rs:362-363); not indexed, not a key.
- file_header_type_2 name, unpacked size: shown (rar.rs:259-285 -> Files inside; probe First file / Unpacked).
- file_header_type_2 data CRC32 and method (first file of the main archive): shown (probe/archive.rs:216-219,
  232-233; Packing panel web.rs:1307, 1324). The archive step itself skips the CRC (rar.rs:265-267).
- file_header_type_2 packed size (first file): stored_not_shown (probe/archive.rs:207, JSON `packed`).
- file_header_type_2 mtime, attributes, host OS, compression version / solid / dictionary bits: not_collected
  (rar.rs:261-269 and probe/archive.rs:212-221 skip or discard them; PR #58 parses version and solid as serde(skip)).
- file_extra_records 0x01 encryption: encryption shown, kdf/salt/check stored_not_shown (rar.rs:277-282, 377-389);
  the IV is skipped (rar.rs:370) -> not_collected.
- file_extra_records 0x02 hash, 0x03 htime, 0x04 version, 0x05 redirection, 0x06 unix owner, 0x07 service:
  not_collected (only record type 1 is searched, rar.rs:383, 400).
- service_headers_type_3 (CMT, QO, ACL, STM, RR): not_collected (parsed as a header, nothing kept, rar.rs:258-287).
- end_type_5: parsed_not_stored (rar.rs:257).
- packer_fingerprint: method shown (Packing "Method" m0..m5); dictionary, solid, host OS, htime, quick open, recovery
  record size: not_collected.

### payload: sevenz
- start_header (version, CRC, next header offset/size/CRC): parsed_not_stored (sevenz.rs:36-44, 464-487;
  nest.rs:312-317).
- streams_info coders (LZMA/LZMA2/PPMd/BZip2/Deflate/BCJ/AES ids + properties): parsed_not_stored (sevenz.rs:159-184;
  AES detection only, sevenz.rs:418-421; nest.rs:327-337 reads the first stream byte to name lzma/lzma2 and that method
  IS shown in the Layers row, web.rs:1326-1328). Pack sizes: parsed_not_stored. Per-file CRC32 digests (substreams):
  parsed_not_stored (sevenz.rs:295-304 read and discarded; folder CRC only verifies a decoded header, sevenz.rs:534).
- files_info names (UTF-16LE) and sizes, empty stream/file flags: shown (sevenz.rs:349-389 -> Files inside).
  mtime/ctime/atime, attributes, anti items, dummy padding: not_collected (sevenz.rs:363).
- encrypted_header (kEncodedHeader with AES): the state is shown ("7z, AES-256, file names hidden", sevenz.rs:506-511
  -> SET_ENCRYPTION); the AES coder properties (cycles power, IV, salt) and the start-header trio: parsed_not_stored.
  No 7z password test exists (mod.rs:167-170), so Password shows ", not tried".

### payload: zip
- local_header flags, method, sizes, name: name/size shown (Files inside); method name and CRC32 of the first entry
  shown in Packing (probe/archive.rs:280-303, web.rs:1307, 1324; CRC absent when the data-descriptor flag is set,
  probe/archive.rs:286, so the catalog's "bogus 00000000" is already avoided for bit 3; AE-2 zeros are still shown as
  00000000). Version needed, DOS time/date: not_collected.
- extra_fields: zip64 0x0001 -> sizes shown (zip.rs:49-72); 0x9901 AES strength -> stored_not_shown inside
  password_check (zip.rs:182-191, 194-201) and used for the verdict; UT 0x5455, unix uid/gid 0x7875, NTFS 0x000a,
  unicode path 0x7075, AV: not_collected.
- central_directory_and_end: names/sizes shown (zip.rs:304-327); version made by (host OS, tool version),
  internal/external attributes, file comment, archive comment, disk numbers: not_collected (zip.rs:317 skips the
  comment bytes; nothing reads @4, @36, @38).

### payload: archive_common
- alt_key: shown as ALT copies (release_fingerprint.content from sorted sizes, duplicates.rs:173-197; "Other copies"
  web.rs:1555-1572; results "ALT" count only in the API, api.rs:305-308). Inner CRC32 is not part of it
  (not_collected per file; the first entry's CRC sits in JSON only).
- per_creation_random: RAR5 salt (+kdf, check) stored_not_shown (archive_release.password_check); 7z next-header
  offset/size/CRC parsed_not_stored (sevenz.rs:43); none is a lookup key (no index on password_check).
- inner_listing: shown ("Files inside", web.rs:1144-1161) with limits: volumes collapsed into one row with summed
  size (names.rs:104-128), MAX_STORED 64 rows per source (mod.rs:27), 256 rows read back (archive/store.rs:93),
  directory structure only as `/` in names (names.rs:29-42), `source` column not shown; per-file CRC not_collected.
- password_state: shown. none/data/headers: `password` code (tag PASSWORD / PASSWORD?, web.rs:698-704) +
  `encryption`/`names_hidden` (Encryption fact); password source: shown as "found in the NFO | the release name | a
  file name | the subject" (release_info.rs:539-564, web.rs:1748); verification result: shown (WORKS / PROBABLY WORKS /
  WRONG / not tried, web.rs:1741-1749) but parsed_not_stored (computed per render; no column).

### payload_additions
- par2_recvslic_twin: not_collected (RecvSlic never read; recovery volumes fetched only when there is no base,
  steps.rs:214, select.rs:141-154, and even then only FileDesc/Main/UniFileN are parsed).
- par1_and_par3: not_collected (mod.rs:64 knows only `PAR2\0PKT`; par2.rs:15 MAGIC).
- rar_data_offset_and_volume_map: parsed_not_stored (`stored_at`, `data_at`, `continues` serde(skip),
  probe/archive.rs:23-31; used for the moviehash tail lookup, probe/step.rs:324-361 and `chain_moviehash`
  probe/step.rs:366-394; volume 1 only).
- rar5_rev_and_quick_open: not_collected (`.rev` is Unidentified in select.rs:15-24 and `identify` would return
  Unknown; RAR5 type 3 service headers are dropped, rar.rs:258-287).
- recovery_record_and_comment: not_collected (RAR5 RR service header dropped; RAR4 0x78 PROTECT block skipped by the
  generic walk rar.rs:44, 72; CMT not read).
- uploader_leaks_in_archives (STM Zone.Identifier, REDIR, UOWNER/UO, 7z unix mode): not_collected (good: nothing to
  redact today).
- sevenz_coder_details: parsed_not_stored (coder ids/props sevenz.rs:172-184; LZMA props used only to decode the end
  header sevenz.rs:513-533; the nest shows "lzma"/"lzma2"/"copy" as the 7z layer method, nest.rs:323-335).
- compressed_prefix_decode: shown as the Layers row (web.rs:1326-1328, 1840-1854) for zip Deflate/zstd/LZMA (nest.rs:
  263-277), 7z LZMA/LZMA2 (nest.rs:327-336), gzip, xz LZMA2, lzip, .lzma, zstd (nest.rs:167-175); RAR compressed
  entries stop with "NO DECODER FOR THIS YET" (nest.rs:278-279) at 55d2e51 (PR #58 adds RAR5). The catalog's "atlas:
  no" predates #52. The decoded prefix itself is not a field; it feeds media parsing (probe/step.rs:400).
- zip_fingerprint_details (version made by, external attrs, extra-field order, data descriptor use, AE-1 vs AE-2):
  not_collected (zip.rs:304-327; probe/archive.rs:278-304 keeps only the data-descriptor effect on the CRC).
- repack_provenance (inner mtime + time flags): not_collected (RAR4 FTIME/EXT_TIME, RAR5 mtime/htime, 7z MTime, zip
  DOS time all dropped; see the per-format entries).
- password_checks_as_keys: stored_not_shown (archive_release.password_check holds kdf/salt/check for RAR5 and the
  zip verifier, archive/store.rs:50, 72-73; used for the WORKS/WRONG verdict only; no password table, no index, no
  cross-release link).
- ngpost_pack_defaults: not_collected as a fingerprint; the one piece visible is Method m0 in the Packing panel
  (web.rs:1307) and the 13-char `{{password}}` the hint regex finds (release_info.rs:510-526).
- srr_srs_layout: not_collected.
- indexer_archive_prior_art: unclear (a reference line, not a field; src/archive is the in-house analogue with a 64
  KiB prefix, mod.rs:29).
- listing_scope_per_volume: parsed_not_stored (`Listing.complete`, `need_more_tail` mod.rs:222-227; `complete` gates
  the content fingerprint, steps.rs:129, and the Password verdict No/Maybe, mod.rs:238; the first two RAR volumes and
  the set's last article are what is read, select.rs:121-124, steps.rs:316-365).
- legacy_and_unix_archives: the archive step does not list them (identify mod.rs:51-88; `.tar .gz .tgz .bz2 .xz .ace
  .cab` NotArchive select.rs:21), but the probe's nest opens tar (names, sizes), gzip (original name), xz (filters),
  .lzma, lzip, zstd and names bzip2/compress/lz4/lzop (nest.rs:178-226) -> shown in the Layers row for the main file
  only; the tar/gzip entries never reach archive_inner_files. ACE/ARJ/LZH: not_collected.
- disc_images (ISO PVD, UDF, El Torito, mds/nrg/ccd/cue, DVD IFO, BDMV, AACS keys): not_collected (select.rs:21 marks
  .iso/.img/.bin NotArchive; no magic in identify; media::parse does not know ISO9660 either from what probe/step.rs
  shows).
- documents_and_comics: not_collected for metadata (ComicInfo.xml, OPF, EXTH, PDF Info/ID). `.cbz`/`.cbr` are listed
  as zip/RAR by name (select.rs:17-18), so page names appear in "Files inside"; `.epub .mobi .azw3 .pdf` are NotArchive
  (select.rs:21).
- games_and_software (ROM headers, PE timestamp/PDB GUID, DAT oracles): not_collected.
- par2_exponent0_recovery_slice: not_collected (RecvSlic bodies never parsed, par2.rs:93-124).
- pack_containment (per-file length+CRC matching across releases): not_collected (fingerprints are per release,
  duplicates.rs:64-89, 173-197; no per-file table).
- rar_determinism (shared middle articles via per-article CRCs): not_collected in this area (segments keep no CRC,
  store.rs:181-188; the probe keeps one `part_crc_ok` bool and the file crc32, probe/mod.rs:57-63).
- volume_order_from_headers: not_collected (ordering is by name only: names.rs:104-128, select.rs:110-124,
  probe/step.rs:48-53; the split-before/after flags that would allow it are dropped, rar.rs:108, probe/archive.rs:123).
- sampled_sha256_key: not_collected (only the OpenSubtitles moviehash, probe/step.rs:294-394; media area).
- raw_split_and_crc_sidecars: `.001` sets ARE probed as Numbered volumes (select.rs:19, 104-110; tests.rs:626-628,
  731-737) and their first bytes identified; a `.crc` sidecar is not_collected (not in KIND; never read).
- repack_and_reuploader_tells: parsed_not_stored. `release_tags::Tags.repack/proper` are parsed per render
  (release_tags.rs:25-26, 109-110) but `name_tags` shows only resolution/HDR/quality (web.rs:720-732); no "file outside
  the scene set" flag exists (`one_archive_set` tolerates only par2/nfo/image/sample/sfv/nzb, duplicates.rs:122-140,
  and its answer is not stored).
- non_video_set_fingerprints (SFV+NFO+M3U CRC lists, EAC logs, FILE_ID.DIZ, fitgirl-bins.md5): not_collected (SFV
  CRCs are shown but never keyed; `.diz .md5 .sha1` are NotArchive, select.rs:21).
- marker_file_hashes: not_collected (tiny files appear by name and size in archive_inner_files; no CRC).

### derived_per_post (this area's items)
- contents_profile: file count, PAR2 count, recovery volumes, recovery blocks, data vs recovery bytes, articles
  seen/expected, repair %: shown, computed per page view and never stored (release_info.rs:1-2, 39-60, 160-201;
  web.rs:1129-1138). Stored parts: `par2_block`, `par2_blocks` (release_fingerprint). Volume naming scheme: visible
  only through names (Files inside; PR #48 adds a "Files in the NZB" table with per-file posted bytes).
  nfo/sfv/sample/proof present: not computed (the NFO/SFV panels show when the probe read one).
- contents_profile: volume size (the -v choice): not_collected (release_info.rs:182-195 sums bytes into
  data_bytes/recovery_bytes only; PR #48's `NzbFile.bytes` would expose posted bytes per file but not the unpacked
  volume size, which `collapse_volumes` sums away, names.rs:110-116).
- contents_profile: file-type mix: not_collected (only par2 / recovery / data classes, release_info.rs:169-180).
- PAR2 ratio (redundancy): shown ("Repair" exact from `recovery_blocks * 1000 / par2_blocks` when the set was read,
  else "about x% (bytes)", release_info.rs:93-111, web.rs:1752-1760).
- posting order (par2 first or last, sequential vs interleaved): not_collected (segments store no Date or article
  number, store.rs:181-188; `files.id` order is insertion order and nothing reads it as a sequence).

---------------------------------------------------------------------------------------------------------------------

## 7. Extension points (where new fields, tables, panels and backfills go)

1. Parsers: `rar::file4` (rar.rs:90-113) and `rar::block5` (rar.rs:239-292) for HOST_OS/FTIME/METHOD/CRC/dictionary/
   solid/mtime/host; `sevenz::files` property arms (sevenz.rs:349-364) for 0x12-0x15 times and attributes and
   `streams` (sevenz.rs:295-304) to keep substream CRCs; `zip::list_head`/`list_tail` (zip.rs:221-236, 304-327) for
   CRC, DOS time, version made by, extras. `InnerFile` (mod.rs:160-163) would grow fields; `Listing::push`
   (mod.rs:247-255) is the single entry point.
2. Storage: `archive/store.rs` `INNER_FILES.added` / `RELEASE.added` (archive/store.rs:36-39, 50) for nullable columns,
   `INSERT_FILE`/`SET_ENCRYPTION` SQL (archive/store.rs:68-73) and the `finish` params (steps.rs:103-110,
   142-152), `InnerFileRow`/`rows` (archive/store.rs:84-99) and `InnerView` (archive/store.rs:151-158) to read them.
   Per-file keys should not go on the collapsed rows (one row per volume set, names.rs:104-128): a new un-collapsed
   table (`par2_files`, `archive_entries`) registered in `SHARD_TABLES` (schema.rs:78-92) is the clean place; its
   `release_key` gives the gc trigger (schema.rs:122-128) and compaction copying (compact.rs:745-752).
3. PAR2: `par2::inventory` (par2.rs:61-135) for IFSC/Creator/RecvSlic kinds; `Par2File` already carries md5, md5_16k,
   file_id, set_id (par2.rs:30-37); `Par2Step::run` (steps.rs:212-270) is where a `par2_files` write would join
   SET_POSTED/SET_PAR2_SIZES.
4. Probe JSON: new `Option`/`#[serde(default)]` fields on `ArchiveHead` (probe/archive.rs:11-35), `Layer`
   (nest.rs:25-51) or `ProbeInfo` (probe/mod.rs:53-78) need no migration; bump `ProbeStep::config_stamp`
   (probe/step.rs:99-101) with a new bit to re-probe existing releases; add indexed columns to `PROBE.added`
   (probe/mod.rs:46) and `SET_PROBE` (probe/mod.rs:49-50) for anything searchable.
5. UI: `release_html` fact closure (web.rs:1047-1052, facts between 1055 and 1142), "Files inside" table
   (web.rs:1144-1161) for extra columns (source, CRC, mtime), `probe_html` Packing panel `dd` calls (web.rs:1304-1328),
   `nested_chain` (web.rs:1840-1854), `copy_card` facts (web.rs:1487-1536), `panel()` helper (web.rs:1235-1240) for a
   new "PAR2" or "Archive" panel; `encryption_fact`/`password_fact`/`repair_fact` (web.rs:1741-1771).
6. Backfill: implement `config_stamp` on `Par2Step`/`ListStep` (steps.rs, trait default step.rs:58-62) so a deploy
   walks every shard (engine.rs:1103-1112); or document `atlas --reprocess --step archive_par2 --step archive_list
   --apply` (reprocess.rs); or a targeted SQL requeue like `forget_old_twins` (duplicates.rs:97-117) for rows missing
   the new columns (`where new_col is null`). Walk throughput: `backlog_per_second` (engine.rs:421); body throughput:
   `body_fetches_per_minute`, `max_bodies_per_release`.
7. API: `release_attrs` (api.rs:285-310) for new newznab attrs; `caps` for new search params.
8. Twin logic: `twin_fingerprint` + `TWIN_VERSION` (duplicates.rs:64-92) with `forget_old_twins` for re-derivation.

---------------------------------------------------------------------------------------------------------------------

## 8. Risks

- Body cost of a full backfill: PAR2 1-2 articles, archives up to 4 bodies (ARTICLE_CAP 4 MiB) or 64 KiB probes per
  release, all inside `body_fetches_per_minute` 600 shared with the probe, NFO and OpenSubtitles steps; the stage walks
  newest first at `backlog_per_second` (engine.rs:421, 436). A million releases is days of traffic; adding fields to
  what the SAME articles already give (CRC, mtime, host OS, Creator, IFSC) costs nothing extra, but anything needing a
  tail article (7z end header, last RAR volume CRC) does.
- `archive_inner_files` rows are collapsed per volume set and capped at 64 per source (names.rs:104-128, mod.rs:27),
  stored under a name-byte quota (`max_names_mb`, steps.rs:94-98) that assumes 32 bytes of overhead per row
  (steps.rs:111): per-file columns on this table are lossy and change the quota's meaning.
- Rerun semantics: `finish` deletes and re-inserts a source's rows (steps.rs:99-110); `UPSERT_RELEASE` only updates
  `password`, so stale `encryption/names_hidden/password_check` survive a run that finds no encryption
  (steps.rs:140-152).
- `password_check` is a tagged blob (0xff = zip, else RAR5 kdf first, mod.rs:172-193); a new check kind needs a new
  tag and `from_bytes` must stay total for old rows. `MAX_KDF` 20 bounds the render-time PBKDF2 (rar.rs:303).
- `release_fingerprint.posted_names` is a declared column with no writer or reader (duplicates.rs:39): dead schema.
- Changing the twin inputs means a `TWIN_VERSION` bump and a `forget_old_twins`-style requeue (duplicates.rs:91-117).
- Release page cost: `copies_html` renders up to 12 `copy_card`s, each running `contents_with` (a scan of every
  article of that copy, release_info.rs:183-188) plus `view_with` and media lookups (web.rs:1463-1472); `copies_with`
  queries 8 shards x 2 columns (duplicates.rs:205-221). More per-copy facts multiply this.
- `Contents` is recomputed per view by scanning all articles (release_info.rs:160-201); for huge releases this is the
  slow part of the page and is why the catalog's rank 12 suggests storing the profile.
- Compaction refuses any shard table not in `SHARD_TABLES` (compact.rs:900-915) and recreates indexes
  (compact.rs:747-749): every new table must be registered before it ships, and large indexes lengthen compaction.
- `release_probe.info` is read whole per page (probe/mod.rs:142-148) and already carries up to 256 KiB of NFO text;
  JSON growth is fine but nothing in it is searchable without a column (probe/mod.rs:31-47).
- Privacy: UOWNER/UO/Zone.Identifier/7z unix mode are not read today; a "store every field" feature must keep them
  out or hash them (catalog open question; the code has no redaction layer).
- Open PR conflicts:
  - #48 (pr/48, based on a29ee52, 5 behind main) rewrites parts of `release_html` (adds `nzb_files_html`, renames
    "Files inside" to "Files inside the archives", changes `probe_html`'s signature to take a cover id, adds panels
    Attachments and Cover art), adds `Contents.list: Vec<NzbFile>` + `MAX_LISTED_FILES` to release_info.rs, adds
    `probe::COVER` to `SHARD_TABLES` and probe/mod.rs, and extends probe/media.rs. Any new fact row, table column on
    `Contents`, or SHARD_TABLES entry will conflict textually; #48 also already conflicts with #52's web.rs (Layers
    row) and probe/mod.rs changes.
  - "stream-details" branch: not present in this clone's refs (`git branch -a` lists pr/21, 48-51, 54, 55, 57, 58);
    #48's commit "Show cover art, attachments, every stream field and the NZB's files" is the closest; treat
    probe/media.rs and the Tracks panel (web.rs:1330-1396) as that branch's territory.
  - #51 / #54 (mid-file keyframes, H.264 decoder; based on ea5a7fc) add `src/probe/stills.rs`, a table in
    `postprocess/schema.rs` SHARD_TABLES, a step in `postprocess/steps.rs`, and edit probe/step.rs, probe/mod.rs,
    probe/moviehash.rs: list-style conflicts with any new ShardTable/step registration and with probe/step.rs edits.
  - #58 (RAR5 decoder, based on origin/main 67504d6 = #56) edits probe/archive.rs (adds `rar_version`, `solid` as
    serde(skip) on ArchiveHead) and nest.rs (Codec::Rar5, solid stop): a feature storing RAR5 version/solid/dictionary
    should build on it, and the local 55d2e51 lacks #56's `src/probe/codec/*`.
  - #57 edits web.rs `Search` (lines ~184-730) heavily: same file, different functions as the release page.
  - Finish web UI thread (twin grouping): overlaps `copies_html`/`copy_card`/`duplicates_page` (web.rs:1455-1660) and
    `duplicates::clusters_with`; the catalog's proposed `par2_files` and `segment_crc` tables must be agreed there.

## 9. Corrections to the catalog, from the code

- `.001` splits are Numbered volumes and ARE probed (select.rs:19, 104-110; tests.rs:626-628), not NotArchive.
- compressed_prefix_decode "atlas: no" is stale: #52 (55d2e51) decodes Deflate, LZMA, LZMA2, zstd, gzip prefixes and
  shows the chain as "Layers" (nest.rs; web.rs:1326-1328); RAR compressed and bzip2/compress/lz4 are the gaps at this
  commit (#56 and #58 close some).
- legacy_and_unix_archives: tar, gzip, xz, lzip, zstd and .lzma are opened by the probe's nest (nest.rs:178-226) for the
  main file, though still not by the archive listing step.
- Main packet: the recoverable-file count is ignored (par2.rs:96), so "non-recoverable list dropped" is really "merged
  into the id list".
- zip local CRC: the probe already blanks the CRC when bit 3 (data descriptor) is set (probe/archive.rs:285-286); only
  AE-2's zeroed CRC can still show as 00000000.
- `release_fingerprint.posted_names` exists as a column but is unused.
- RAR5 `_attributes` and `_host` are parsed past in both parsers (rar.rs:261, 269; probe/archive.rs:212, 221) - the
  catalog says "dropped", which is right; adding them is a two-line change each.

## 10. Open questions

- Per-file archive/PAR2 fields: new columns on the collapsed `archive_inner_files` (lossy, quota-bound) or a new
  un-collapsed table (`par2_files` / `archive_entries`) as the catalog's storage plan suggests?
- Backfill path: automatic `config_stamp` bits on Par2Step/ListStep (every deploy re-reads bodies for every release)
  or the operator's `--reprocess --apply`? Both share the 600/min body budget with the probe.
- Should the UI print raw stored values (par2 block size, salts, check bytes, fingerprints) or only derived facts?
  Where do the "stored_not_shown" items (packed size, par2_block/par2_blocks, kdf/salt/check, posted/content hashes)
  belong on the page?
- Is the stale-encryption behaviour of `UPSERT_RELEASE` + conditional `SET_ENCRYPTION` intended?
- Does #48's "Files in the NZB" table (per-file posted bytes) count as the contents_profile volume size, or should the
  `-v` size be derived from unpacked volume sizes before `collapse_volumes` sums them?
- If PAR2 per-file hashes get a table, does twin matching become per file (catalog rank 3) and does `TWIN_VERSION`
  move to 3?
- Which of #48 / #52-#58 lands first: the release page and probe JSON are the shared files.
