# Part 4: Archive and PAR2 inspection

Every Atlas line number below is at base commit `de598b6`. Parts 1–3 merge first and shift them: find the named function, keep the intent, and record any difference in the PR. Upstream paths are at the commit pinned in `00-overview.md`. Part 3 wins any disagreement with this plan.

The layouts, parsers and test kit here were prototyped in a scratch crate before this plan was written. `bsdtar` (libarchive 3.7.4), Python's `zipfile` and `par2cmdline` read the hand-built fixtures and wrote the real samples. The parsers ran over every truncation of each fixture and about 220,000 mutated inputs in a debug build with overflow checks, with no panic. None of those tools is needed to build or test Atlas.

## 1. Goal

Atlas learns what is inside a release without downloading it. A post-processing step fetches the first article of the first volume, identifies the format from its decoded bytes, and records the inner filenames and whether the archive is passworded, for RAR4, RAR5, ZIP and 7z. PAR2 reading returns the whole file list and no longer offers a random name as the release name. Sonarr and Radarr see a `password` attribute, the terminal release view shows inner files, and Part 5 gets real names to work with. The inline par2/nfo lookup leaves the save path once the stage has a body budget. With an existing `config.json` nothing changes: no budget means the same inline lookups and the same network load as today.

## 2. Non-goals

- No payload extraction, nothing written to disk, no external `unrar` or `7z` binary.
- No filtering or hiding of passworded releases. The attribute is reported only.
- No naming regexes or name checkers (Part 5). Steps here set a name only when a release has none.
- No nested archive listing. An archive holding only archives is reported as password "maybe".
- No mapping of random-named posted files to PAR2 names by the MD5 of their first 16 KB (upstream `rust/pipeline-ffi/src/additional.rs:2248-2480`). It needs one body per file.
- No self-extracting archives: the magic must be at offset 0. No RAR seek across articles to later file headers. No partial read of a named archive: only a probe stops early.
- No dashboard work. Part 3's Post-processing page already reports steps by name.

## 3. Source map

| Upstream | Take | Leave behind |
|---|---|---|
| `composer.json:50` (`dariusiii/rarinfo`) | Nothing: the package is not in the clone | The dependency |
| `docker/app/nzb-yenc-ffi/src/formats.rs:1205-1262,1640-1720` | Cross-check of RAR4: main and first file offsets, the CRC rule, including that a main header with flag `0x0002` covers only 13 bytes, and `0x0080` meaning encrypted headers | Its JSON observations; it reads the first file only |
| `docker/app/nzb-yenc-ffi/src/formats.rs:3081-3135,3328-3420,4140-4170,4546-4564` | Cross-check of RAR5: 3-byte header size limit, file fields, encryption record, vint overflow rule | Time, hash and redirection records |
| `docker/app/nzb-yenc-ffi/src/formats.rs:4577-4601,5027-5100`, `docker/app/nzb-yenc-ffi/src/seven_zip_ranges.rs:5-15,906` | Cross-check of 7z: `NUMBER`, coder ids, major version 0, consistent empty header. Upstream marks a packed header `declared_not_decoded` | Range bookkeeping; its 1 MiB caps (Atlas's come from section 4) |
| `app/Services/AdditionalProcessing/ArchiveExtractionService.php:172-183` | An encrypted archive is a result (password yes), never a failure | Password-guessing extraction |
| `app/Services/AdditionalProcessing/ArchiveExtractionService.php:516-525` | Signature list | `ArchiveInfo`'s marker scan inside a file |
| `app/Services/AdditionalProcessing/ArchiveExtractionService.php:530-535,606-621`, `rust/pipeline-ffi/src/additional.rs:5225-5260` | 7z: the head gives size and CRC of the end header; cut it from the tail's last bytes and check the CRC | Rebuilding a fake archive to feed a reader |
| `app/Services/AdditionalProcessing/ArchiveExtractionService.php:639-735` | The packed header sits just before the end header | Listing it through the 7-Zip client and a sparse temp file |
| `app/Services/AdditionalProcessing/SevenZipListingParser.php:1-24` | Reject control characters in a path | The rest: it parses `7z l` text output, which Atlas never runs |
| `app/Services/Releases/Fingerprint/ReleaseFingerprintCollector.php:26-28` | Fragment budgets exist (16 MiB, 32 fragments); Atlas needs at most 2 tail articles | First/middle/last sampling |
| `app/Services/AdditionalProcessing/AdditionalWorkPlanner.php:96-102` | The highest-numbered volume's last segment is the 7z tail | The planner object |
| `app/Services/AdditionalProcessing/AdditionalWorkPlanner.php:206-240`, `config/nntmux.php:33` | Probe files with no extension when no archive was named, largest first, capped (upstream 3); a 7z found by a probe gets its last segment | Probing several articles per file |
| `app/Services/AdditionalProcessing/AdditionalWorkPlanner.php:761-794` | Every RAR volume has its own headers in segment 1, so a first volume missing that segment falls to the next | |
| `app/Services/AdditionalProcessing/AdditionalWorkPlanner.php:37,864-885`, `rust/pipeline-ffi/src/additional.rs:548-625,1695-1745` | Volume name families and which is first | PCRE byte-matching code |
| `app/Services/Releases/Fingerprint/NzbFingerprintAnalyzer.php:927-955` | Magic table with non-archive signatures | ISO at offset 32,769 |
| `app/Services/Releases/ReleaseBrowseService.php:41-43` | 0 none, 1 definitely | Other status values |
| `app/Services/Par2PacketAccumulator.php:51-52,171-187,198-212` | Skip recovery packets. Parse per recovery set so file-ID dedupe cannot merge sets. "Complete" means a Main whose IDs are all described | Resumable state, byte ceilings on stored state |
| `app/Services/Releases/Fingerprint/Par2MetadataParser.php:23,737-820,916-917` | Main layout, `UniFileN`, an ID cap | Verification fields |
| `app/Services/Par2Processor.php:231,652-710` | Store few files per release (upstream 21). Name classes: 32/40 hex stems, 16+ alphanumeric stems, release-like stems | Writes inside a row lock, FFI batches |

## 4. Atlas changes

- **`Cargo.toml`:** `lzma-rs = "0.3"`. `Cargo.lock` gains `lzma-rs`, `crc` and `crc-catalog` (`byteorder` is there); commit it, because `just test` is `cargo test --locked`. CRC32 is `flate2::Crc`, already a dependency.
- **`src/lib.rs`:** `pub mod archive;`.
- **`src/postprocess/schema.rs`:** `SHARD_TABLES` gains `&crate::archive::store::INNER_FILES, &crate::archive::store::RELEASE`.
- **`src/postprocess/steps.rs`:** `all()` gains `Par2Step`, `ListStep` and `NfoStep`, each `Arc::new(archive::steps::X::default())`.
- **`src/indexer.rs:1616-1620`** (`save_slice`): guarded as below. `:1624` already treats a missing entry as no name. Hard rule 9 on this path is Part 1's: `save_slice` returns before here for a blocked group. Part 2's `NAME_SOURCES_MAX` stays in `name_sources`.

```rust
let names = if crate::archive::inline_lookups() {
    first_names(pool.clone(), releases.iter().map(name_sources).collect()).await
} else {
    Vec::new()
};
```

- **`src/bg_indexer.rs`:** in `run_servers` (`:694`), after Part 3's `Stage::start` and before the workers spawn (`:753-757`), call `crate::archive::sync_inline_lookups(&config)`, whether or not the stage started. In `watch_config` (`:613`), after the rebuild check (`:622-629`), call it again next to Part 3's `stage.set_config`.
- **`src/nntp.rs`, `src/postprocess/step.rs`, `src/postprocess/bodies.rs`:** a prefix read, additive, see "Reading a prefix".
- **`src/par2.rs`:** `file_descriptions` (`:19-60`) becomes a wrapper over `inventory`, with the same signature, each name once. `display_name` (`:63-65`) becomes `names::best_name` over the inventory. `is_base_par2` (`:14`) stays for the inline path. The regex statics (`:7-8`) each get `#[allow(clippy::unwrap_used)]`, because the file-level `deny` below would reject them.
- **`src/api.rs`:** `rss` (`:204`) takes `passwords: &HashMap<i64, Password>`. The format string at `:211` gains `<newznab:attr name="password" value="N"/>` after the category when the id has an entry. `search` (`:196`) calls `archive::store::passwords(&ids)`. `:152` passes an empty map.
- **`src/app.rs`:** `release_actions` (`:170`) also loads `archive::store::view(release.id)` and `show_release` (`:77`) takes it. The summary line (`:90-100`) gains ` - passworded` in red or ` - maybe passworded` in yellow. After the files table (`:124`), `inner_table(&view) -> Option<Table>` prints up to 30 rows titled `inner files (N)`, with `Line::plain` so a hostile name is never read as markup and `ui::ellipsize` as at `:121`.
- **`src/config.rs`:** `Config.archive: ArchiveConfig` (`:134`), default at `:169`, parsed next to `:302`. `save_config` keeps unknown keys (`:447-450`) and is not changed.
- **New `src/archive/`:** `mod.rs`, `names.rs`, `rar.rs`, `zip.rs`, `sevenz.rs`, `select.rs`, `store.rs`, `steps.rs`, `testkit.rs`, `tests.rs`. `select.rs` and `names.rs` carry a GPLv3 attribution header naming the upstream files above. `rar.rs`, `zip.rs`, `sevenz.rs` and `src/par2.rs` start with the line below, so `just lint` rejects a panic path in parser code. Tests live in `tests.rs`. Attacker-controlled sums use `checked_add`, and casts use `usize::try_from`. Clippy 1.99 also rejects `chunks_exact` with a constant size: use `as_chunks::<N>().0`.

```rust
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic))]
```

### Identifying a format (`archive::identify`)

Bytes at offset 0 of the decoded article, tested in this order. `Got::Body` is already yEnc-decoded (`src/nntp.rs:505-514`, Part 3's `a_small_article_comes_back_decoded`). A subject or filename never decides a format.

| Bytes | Format |
|---|---|
| `52 61 72 21 1A 07 01 00` | `Rar5` |
| `52 61 72 21 1A 07 00` | `Rar4` |
| `37 7A BC AF 27 1C` | `SevenZip` |
| `50 4B 03 04`; `50 4B 07 08` or `50 4B 30 30` then `50 4B 03 04`; `50 4B 05 06` | `Zip` |
| `50 41 52 32 00 50 4B 54` | `Par2` |
| `FF D8 FF`; `89 50 4E 47 0D 0A 1A 0A`; `GIF87a`; `GIF89a`; `RIFF....WEBP`; `BM`; `49 49 2A 00`; `4D 4D 00 2A` | `Image` |
| `1A 45 DF A3`; `....ftyp`; `RIFF....AVI `; `00 00 01 BA`; `fLaC`; `OggS`; `ID3` | `Media` |
| anything else, including a short prefix of the above | `Unknown` |

### RAR4 (`rar::list4`)

All integers little-endian. `offset name(size)`:

```
Marker   52 61 72 21 1A 07 00
Block    0 HEAD_CRC(2)  2 HEAD_TYPE(1)  3 HEAD_FLAGS(2)  5 HEAD_SIZE(2, whole header)  7 ADD_SIZE(4, when flags & 0x8000: data bytes after the header)
Types    0x73 main, 0x74 file, 0x7A service, 0x7B end, 0x75-0x79 old blocks
Main     7 HighPosAV(2)  9 PosAV(4)
         flags: 0x0001 volume, 0x0002 old comment inside, 0x0080 headers encrypted, 0x0100 first volume
File     7 PACK_SIZE low(4, = ADD_SIZE)  11 UNP_SIZE low(4)  15 HOST_OS(1)  16 FILE_CRC(4)  20 FTIME(4)  24 UNP_VER(1)
         25 METHOD(1)  26 NAME_SIZE(2)  28 ATTR(4)
         with flag 0x0100: 32 HIGH_PACK_SIZE(4)  36 HIGH_UNP_SIZE(4)
         name at 32 or 40; then salt(8) with 0x0400 and ext time with 0x1000, both skipped by HEAD_SIZE
         flags: 0x0001 split before, 0x0002 split after, 0x0004 encrypted, 0x00E0 all set = directory, 0x0100 large, 0x0200 Unicode name
```

Walk from `pos = 7`, at most `MAX_BLOCKS = 65_536` blocks:

1. `pos == len`, under 7 bytes left, or `pos + HEAD_SIZE > len`: return as is. That is truncation, not damage.
2. `HEAD_SIZE < 7`: `damaged`. For `0x73`, `0x74`, `0x7A`, check `HEAD_CRC` against the low 16 bits of CRC32 over `2..HEAD_SIZE` (`2..13` for a main header with `0x0002`); a mismatch is `damaged`. Old blocks and the end block are not checked, because old writers do not set them reliably.
3. `0x73` with `0x0080`: `encrypted = headers_encrypted = true`, return. What follows is salt and AES data.
4. `0x74`: a header under 32 bytes, missing large fields, or a name past the header is `damaged`. `0x0004` sets `encrypted`. Not a directory: push the name with `UNP_SIZE`.
5. `0x7B`: `complete = true`, return.
6. `next = pos + HEAD_SIZE + (ADD_SIZE | HIGH_PACK_SIZE << 32)`, in `u64`. Past `len`: return. Inside one article this is the normal end.

Name, flag `0x0200` set: no NUL in it means UTF-8 (lossy). Otherwise it is `name NUL enc`. `enc[0]` is `high`. Then until `enc` runs out or `MAX_NAME_BYTES`, take 2 bits at a time from a flag byte, high bits first, reading a new flag byte when none remain:

- `00`: next byte `b`: char `b`.
- `01`: next byte `b`: char `b | high << 8`.
- `10`: next 2 bytes: char, little-endian.
- `11`: next byte `len`. If `len & 0x80`, the next byte is a correction `k`. A run of `(len & 0x7F) + 2` chars from `name[out.len()]` on, ending at `name`'s end: `((c + k) & 0xFF) | high << 8` with `k`, plain `c` without.

Flag `0x0200` clear: lossy UTF-8. libarchive decodes every command type the same way.

### RAR5 (`rar::list5`)

A `vint` is little-endian base 128, high bit continues, 10 bytes at most, and a 10th byte above 1 is rejected.

```
Marker   52 61 72 21 1A 07 01 00
Block    CRC32(u32, over the size field through the end of the header)  size(vint, at most 3 bytes, from type to end of extra)
         type(vint: 1 main, 2 file, 3 service, 4 encryption, 5 end)  flags(vint: 0x01 extra, 0x02 data, 0x08 from previous volume, 0x10 into next)
         [extra size(vint)]  [data size(vint)]  type fields  extra area (the header's last `extra size` bytes);  then data
File     file flags(vint: 0x01 directory, 0x02 mtime follows, 0x04 CRC follows, 0x08 size unknown)  unpacked(vint)  attributes(vint)
         [mtime(u32)]  [CRC32(u32)]  compression(vint)  host OS(vint)  name length(vint)  name(UTF-8, no NUL)
Extra    record: size(vint, counts from type)  type(vint: 0x01 file encryption)  data
Encrypt  version(vint)  flags(vint)  KDF(u8)  salt(16)  [check(12) with flag 0x01].  Every later header is encrypted
```

Walk from `pos = 8`, same `MAX_BLOCKS`:

1. Under 4 bytes for the CRC, or the size field or `size` bytes running past the data: return (truncation). A third size byte with its high bit set, or a size of 0: `damaged`. A CRC mismatch, or any failed read inside the verified header: `damaged`.
2. Type 4: `encrypted = headers_encrypted = true`, return. Type 5: `complete = true`, return.
3. Types 2 and 3: parse the fields. The name must end at or before `size - extra`, else `damaged`. Type 2 only: an extra record of type `0x01` sets `encrypted`; not a directory: push the name, with its size unless `0x08`. A record of size 0 or past the area ends the extra walk.
4. `next = end of header + data size`, in `u64`. Past `len`: return.

### ZIP (`zip::list_head`, `list_tail`, `list`)

```
Local    0 50 4B 03 04  4 version(2)  6 flags(2)  8 method(2, 99 = WinZip AES)  10 time,date(4)  14 CRC32(4)  18 compressed(4)  22 uncompressed(4)
         26 name length n(2)  28 extra length e(2)  30 name, extra, data
         flags: 0x0001 encrypted, 0x0008 sizes in a trailing descriptor, 0x0040 strong encryption, 0x0800 UTF-8, 0x2000 directory encrypted
Extra    id(2)  size(2)  data.  id 0x0001 (ZIP64): uncompressed(8)  compressed(8), used when the 32-bit field is FF FF FF FF
Central  0 50 4B 01 02  4 made(2)  6 needed(2)  8 flags(2)  10 method(2)  12 time,date(4)  16 CRC32(4)  20 compressed(4)  24 uncompressed(4)
         28 n(2)  30 e(2)  32 comment k(2)  34 disk(2)  36 internal(2)  38 external(4)  42 local offset(4)  46 name, extra, comment
End      0 50 4B 05 06  4 disk(2)  6 directory disk(2)  8 entries here(2)  10 entries(2)  12 directory size(4)  16 directory offset(4)  20 comment length(2)  22 comment
ZIP64    locator 50 4B 06 07 (20 bytes) just before End; record 50 4B 06 06 (56 bytes) before that: 32 entries(8)  40 directory size(8)
```

`list_head`: skip a leading `PK\x07\x08` or `PK00`, then walk. Encrypted bits or method 99 set `encrypted`. Names ending in `/` or `\` are directories. A name without `0x0800` is UTF-8 if valid, else Latin-1. `0x0008` with compressed size 0 stops after that entry. `next` is checked in `u64`. A `PK\x01\x02` or `PK\x05\x06` signature ends it; any other sets `damaged`. `complete` stays false: only the directory proves the whole list.

`list_tail(tail)`: search back over the last 65,557 bytes for an End signature whose comment length reaches the end exactly, which rejects a fake record inside a comment. None: `damaged`. Saturated size or count: read them from the ZIP64 record at `eocd - 76` when both signatures are there. The directory ends at End, or at the ZIP64 record when present. Its start is `end - size`, relative to the tail, because absolute offsets mean nothing in a tail. If that falls before the tail, start at the first `PK\x01\x02` found. Walk to the end or `MAX_ENTRIES`. `complete` needs the whole directory and a matching count.

`list(head, tail)`: if the tail read has files, use them and keep `encrypted` from either; otherwise the head read.

### 7z (`sevenz::start`, `list`)

Signature header: `0 magic(6)  6 major(1, must be 0)  7 minor(1)  8 CRC32 of 12..32 (4)  12 NextHeaderOffset(8, from byte 32)  20 NextHeaderSize(8)  28 CRC32 of the next header (4)`.

`NUMBER`: first byte `f`; `n` is its count of leading 1 bits; then `n` little-endian bytes `v`. The value is `v | (f & (0x7F >> n)) << (8 * n)`, with no high part when `n = 8`.

Grammar (property ids in hex, `00` ends a list):

```
Header       := 01 [02 {type size bytes} 00] [03 Streams] [04 Streams] [05 Files] 00
EncodedHeader:= 17 Streams
Streams      := [06 packPos numPack {09 size*numPack | 0A Digests} 00]
                [07 0B numFolders external(00) Folder*numFolders
                    0C unpackSize*(outStreams of each folder) {0A Digests} 00]
                [08 [0D numStreams*numFolders] [09 (numStreams-1 sizes per folder)] {0A Digests} 00]
                00
Digests      := allDefined(u8) [bit vector if 0] crc32(u32)*defined
Folder       := numCoders Coder*numCoders (in out)*(outTotal-1) index*(if packed>1), packed = inTotal-(outTotal-1)
Coder        := flags(u8: low 4 bits id size, 10 complex, 20 has properties, C0 must be 0) id [numIn numOut] [propSize props]
Files        := numFiles {type size bytes} 00
                0E bit vector(numFiles): empty stream
                0F bit vector(count of empty streams): empty file; empty and not this = directory
                11 external(00) then UTF-16LE names, each ended by 00 00
```

Bit vectors are most significant bit first. A substream `0A` counts every stream except a folder with one stream and a known folder CRC. Stream sizes go in order to files without an empty stream. A folder's size is its output stream not used by a bind pair. Coder ids: `03 01 01` LZMA, `06 F1 07 01` AES, `21` LZMA2.

Caps: `MAX_NEXT = MAX_DECODED = 16 MiB`, `MAX_FOLDERS = 4_096`, `MAX_CODERS = 32` per folder and per coder's streams, `MAX_ROUNDS = 4` nested encodings, a stream size list of at most `4 * MAX_ENTRIES`. `numFiles` above 8 × header bytes, or a bit vector needing more bits than the data has, is `damaged` before anything is allocated. Hitting `MAX_ENTRIES` leaves `complete` false.

`list(head, tail)`, where `head` is the archive's first bytes and `tail` its last bytes, either possibly empty:

1. `start(head)` fails on a short head, a wrong magic, a major version other than 0, or a wrong CRC: `damaged`. `NextHeaderSize == 0`: with offset and CRC both 0 an empty archive, `complete`; otherwise `damaged`. Over `MAX_NEXT`: `damaged`.
2. `len = 32 + offset + size`, checked. If `head` holds `len` bytes the window is `head[..len]` at position 0; otherwise it is `tail` at position `len - tail.len()`.
3. Window under `size` bytes: `need_more_tail`. Otherwise the next header is its last `size` bytes. A CRC mismatch is `damaged`, which also catches a set whose last volume is missing.
4. First byte `01`: parse `Header`. An AES coder in a main-stream folder sets `encrypted`; names still read.
5. First byte `17`: parse `Streams`. Any folder with an AES coder: `encrypted = headers_encrypted = true`, return. A single-coder LZMA folder with 5 property bytes is required; anything else, including LZMA2, is `damaged`, which reads as password "maybe". The packed stream is at `32 + packPos`: slice it from the window, else from `head`, else `need_more_tail`. Unpack size over `MAX_DECODED`: `damaged`. Decode, check the folder CRC when present, and loop to step 4 at most `MAX_ROUNDS` times.

Decoding, `input` being the 5 property bytes then the packed stream:

```rust
let options = lzma_rs::decompress::Options {
    unpacked_size: lzma_rs::decompress::UnpackedSize::UseProvided(Some(unpacked)),
    memlimit: Some(MAX_DECODED),
    allow_incomplete: false,
};
// third-party code fed attacker bytes: a panic in it means a bad archive
std::panic::catch_unwind(move || {
    let mut out = Vec::new();
    lzma_rs::lzma_decompress_with_options(&mut input.as_slice(), &mut out, &options).ok()?;
    Some(out)
}).ok().flatten()
```

### PAR2 (`src/par2.rs`)

```
Packet    PAR2\0PKT(8)  length(u64: whole packet, multiple of 4, at least 64)  packet MD5(16)  set id(16)  type(16)  body
Main      PAR 2.0\0Main\0\0\0\0   slice size(u64)  recoverable count(u32)  file ids(16 each)
FileDesc  PAR 2.0\0FileDesc       file id(16)  MD5(16)  MD5 of first 16 KB(16)  length(u64)  name, NUL padded to a multiple of 4
UniFileN  PAR 2.0\0UniFileN       file id(16)  UTF-16LE name
```

`inventory` keeps the behaviour of the loop at `:23-57`, rewritten with `get` for the `deny` line:

```rust
fn id_at(b: &[u8], at: usize) -> Option<[u8; 16]> { b.get(at..at.checked_add(16)?)?.try_into().ok() }
fn u64_at(b: &[u8], at: usize) -> Option<u64> { Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?)) }

let mut pos = 0usize;
while let Some(head) = data.get(pos..pos.saturating_add(HEADER_LEN)) {
    if !head.starts_with(MAGIC) {
        // resync on the next packet header
        match data.get(pos + 1..).and_then(|rest| rest.windows(8).position(|w| w == MAGIC)) {
            Some(skip) => { pos += skip + 1; continue; }
            None => break,
        }
    }
    let len = u64_at(head, 8).unwrap_or(0);
    let shaped = len >= HEADER_LEN as u64 && len.is_multiple_of(4);
    let end = usize::try_from(len).ok().and_then(|l| pos.checked_add(l));
    let Some(packet) = end.filter(|_| shaped).and_then(|e| data.get(pos..e)) else {
        inv.truncated |= shaped;   // past the data; a nonsense length is not truncation
        pos += 8;
        continue;
    };
    pos += packet.len();
    // by type: set id at 32, type at 48, body at 64
}
```

- A file is kept once per `(set id, file id, name)`. Volumes repeat these packets, so a repeat collapses, and two sets cannot merge.
- The same id under a second name breaks the spec, and both names are kept. Names are all this wants, and upstream's first-wins would hide one. It also means `file_desc_packet` (`:68`) and `tests/common/mod.rs:134`, which give every file one constant id, need no change, so `picks_largest_file` and `tests/e2e.rs:27` hold as written.
- A clean `UniFileN` name replaces the `FileDesc` name of its `(set id, file id)`, in either order.
- `complete`: at least one Main seen, and every Main id has a `FileDesc`. `MAX_FILES = 4_096` caps files, `UniFileN` names and Main ids. Packet MD5 is not checked.

`par2cmdline`'s real output, checked: a base file is `Main, FileDesc…, IFSC…, Creator`, while a `.vol` file starts with `RecvSlic`, one slice long, before the critical packets. So a volume's first article can hold no names, which is why a set with no base file also reads its volume's last article.

### Reading a prefix (`fetch_prefix`)

NNTP `BODY` has no range, so the only way to read less than an article is to stop and hang up. A probe does that, because it doesn't know what it is about to read. In a scratch copy of Atlas on the mock server, a 1 MB article gave a true decoded prefix of 64,500 bytes, the next request came on a new connection and was clean, and a 10 KB article came back whole with its connection kept.

```rust
// src/nntp.rs, beside body (:506). False: the rest is left unread, soo the connection cant take another request.
// `each_line_capped` is `each_plain_line` with Part 3's `read_line_capped(max)` in place of `read_raw_line`: a
// `None` from it (one line past the cap) ends the read with `false`, soo one enormous line costs at most one
// read's worth past `max_bytes`. A `COMPRESS=GZIP` status answers `false` with nothing read, as `body_capped` does.
pub async fn body_prefix(&mut self, message_id: &str, max_bytes: usize) -> Result<(Vec<u8>, bool)> {
    let (code, message) = self.command("BODY", Some(message_id)).await?;
    if code != 222 {
        return Err(NntpError::Reply { code, message });
    }
    let (mut lines, mut read) = (Vec::new(), 0usize);
    let whole = self
        .each_line_capped(&message, max_bytes, |l| {
            // what came over the wire: empty lines must count, or they never reach the cap. So at most `max_bytes / 2` lines
            read += l.len() + 2;
            lines.push(l);
            read < max_bytes
        })
        .await?;
    Ok((yenc_decode(&lines).unwrap_or_else(|| lines.join(&b'\n')), whole))
}
```

- `Pool::background_prefix(&self, message_id: &str, max_bytes: usize) -> Result<BackgroundBody>`: `background_body` with `body_prefix` in place of `body_capped`, sharing one private function. It never answers `TooBig`: a cut body is `Body(prefix)`. When `whole` is false, skip `lease.check`, so the lease stays busy and the connection is thrown away, as at `src/nntp.rs:1548-1550`.
- `StepCtx::fetch_prefix(&mut self, wanted: &BodyRequest) -> Result<BodyResult, StepError>`: every check `fetch_bodies` makes, in its order, one body against the budget and the per-release count. `wanted.max_bytes` is the prefix size. `Got::Body` may hold only the article's start.
- `max_bytes` is checked at line ends, counting each line's terminator so an article of empty lines stops at it too (`many_empty_lines_stop_a_prefix`: 1,000,000 empty lines and a 64 KiB cap: `whole` false, at most 32,768 lines kept, the connection dropped), and within a line by `read_line_capped`: a line past the cap comes back as `None`, which ends the read with `whole` false, as Part 3's `body_capped` does. Real posts have lines of 128–256 bytes. The mock's `yenc_body` sends one line and reads whole, so tests use a new `yenc_lined(name, data)` in `tests/common/mod.rs`: `=ybegin`, `yenc_encode` of each 100 bytes as its own line, `=yend`.

### Naming (`names.rs`)

`clean_name(raw) -> Option<String>`: drop `char::is_control`, `\` becomes `/`, at most `MAX_NAME_BYTES`, cut at a character boundary, trim, strip a leading `./` and trailing `/`. Empty, `.` and `..` are `None`. Every parser uses it.

```rust
static VOLUME: &str = r"(?i)(?:\.part\d+\.rar|\.vol\d+[+-]\d+\.par2|\.par2|\.r\d{2,3}|\.s\d{2}|\.z\d{2,3}|\.rar|\.cbr|\.cbz|\.zipx?|\.7z(?:\.\d{3})?|\.\d{3})$";
static RANDOM: &str = r"(?i)^(?:[a-z0-9]{16,}|[0-9a-f]{8,}|\d+|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$";
static SUPPORT: &str = r"(?i)\.(?:nfo|txt|diz|sfv|srr|srs|nzb|md5|sha1|url|jpe?g|png|gif|webp|bmp|tiff?|idx|sub|srt|ass)$";
static EXTRA: &str = r"(?i)(?:^|[._ -])(?:sample|proof|cover|subs?)(?:[._ -]|$)";
static LAST_EXT: &str = r"\.[A-Za-z0-9]{1,8}$";
```

`base_name`: after the last `/` or `\`. `strip_volume_suffix`: removes a `VOLUME` match not starting at 0. `stem(n)`: `base_name`, `strip_volume_suffix`, then removes a `LAST_EXT` match not starting at 0. `looks_random(s)`: `RANDOM` matches.

`best_name(files)`, the fix for "the biggest isn't the useful one":

1. Skip a file when `looks_random(stem(base))`.
2. Tier 0: `SUPPORT` matches, and only when its stem has a `[._ -]` separator; shown as its stem. Tier 1: `EXTRA` matches its stem; shown as `strip_volume_suffix(base)`. Tier 2: the rest, shown the same way.
3. Group by the shown name, case-insensitive: sizes add, the tier is the highest seen.
4. Highest `(tier, summed size)` wins, ties by the lower name. Nothing left: `None`.

`collapse_volumes(files) -> Vec<InnerFile>`: names matching `VOLUME` with the same `strip_volume_suffix` become the first of them with sizes summed; of other files, an exact repeat of a name is dropped. Order of first appearance, at most `MAX_STORED`.

### Choosing what to fetch (`select.rs`)

```rust
static KIND: &str = r"(?ix) \.vol(?P<pv>\d{1,9})[+-]\d{1,9}\.par2$ | (?P<pb>\.par2)$
  | \.part(?P<rp>\d{1,6})\.rar$ | (?P<rar>\.rar|\.cbr)$ | \.r(?P<rr>\d{2,3})$ | \.s(?P<rs>\d{2})$
  | \.7z\.(?P<sv>\d{3})$ | (?P<sz>\.7z)$ | \.z(?P<zv>\d{2,3})$ | (?P<zip>\.zipx?|\.cbz)$ | \.(?P<nv>\d{2,3})$
  | (?P<nfo>\.nfo|\.txt)$ | (?P<img>\.jpe?g|\.png|\.gif|\.webp|\.bmp|\.tiff?)$
  | (?P<not>\.mkv|\.mp4|\.avi|\.m4v|\.ts|\.wmv|\.mov|\.mpe?g|\.webm|\.mp3|\.flac|\.m4a|\.ogg|\.aac|\.wav|\.iso|\.img|\.bin|\.exe|\.msi|\.dmg|\.pdf|\.epub|\.mobi|\.azw3|\.sfv|\.srr|\.srs|\.nzb|\.url|\.diz|\.md5|\.sha1|\.idx|\.sub|\.srt|\.ass|\.tar|\.gz|\.tgz|\.bz2|\.xz|\.ace|\.cab)$";
```

`classify(filename) -> Kind`: `Par2Vol(pv)`, `Par2Base`, `Rar(n)`, `SevenZip(n)`, `Zip(n)`, `Numbered(n)`, `Nfo`, `Image`, `NotArchive`, and no match is `Unidentified`. Volume numbers order a set: `.rar` 0, `.partN.rar` N, `.rNN` NN+1, `.sNN` NN+1001, `.7z` 0, `.7z.NNN` NNN, `.zNN` NN, `.zip` `u32::MAX` (a split ZIP's `.zip` is its last volume), `.NN` and `.NNN` their number.

`plan(files, probe_files) -> Vec<Head>`, pure, where `Head { file, tail, probe }`:

1. Bucket `Rar`, `SevenZip`, `Zip` and `Numbered` files by `(family, strip_volume_suffix(filename) lowercased)`.
2. Sort sets by summed `max(expected, parts_seen)` descending, then stem. Keep `MAX_SETS = 2`.
3. In each set sort by `(volume, filename)`. RAR gives its first 2 volumes as heads, the others their first volume. Every head's `tail` is its set's last volume.
4. No heads at all: probe. Stop with nothing when `image files * 2 >= files`. Otherwise take `Unidentified` files with `max(expected, parts_seen) >= PROBE_MIN_PARTS` (3), most parts first then by name, at most `probe_files`. Each has `probe: true` and itself as `tail`.

A random-named image has no name to refuse it by, so three things bound it. Most images here are one or two articles, and the floor skips those. A larger one gets a prefix read, not the article: `PROBE_PREFIX` is 64 KiB, enough for every magic in the identify table, against about 740 KB. An `Image` result stops the release, so no second probe follows. Part 3's `fetch_bodies` refuses image-named files on top of that.

`par2_source(files)`: the `Par2Base` with the fewest parts, then by name; failing that, the `Par2Vol` with the lowest first block, then fewest parts. `nfo_sources(files)`: `Kind::Nfo` or `nfo::is_nfo(subject)`, fewest parts first, at most 2.

### Steps (`steps.rs`)

Part 3 gates body steps by `postprocess.body_min_size_bytes` and `body_max_size_bytes`, and `fetch_bodies` answers `Got::Refused` outside them, so these steps need no size test of their own.

Every body goes through `StepCtx::fetch_bodies` with `max_bytes: Some(ARTICLE_CAP)`, or `fetch_prefix` for a probe, with `file` set to the filename, so hard rule 9's checks apply. `Got::Missing`, `TooBig` and `Refused` skip that candidate. A `StepError` propagates with `?`. A `rusqlite` error becomes `StepError::Retry`.

First article: `ctx.segments(id, &SegmentPick::First(1))`. A row with `part > 1` means the first article was never indexed: skip the candidate without a fetch. Tail: `SegmentPick::Last(2)`; when `expected` is known the last row's `part` must equal it, else there is no tail.

**`Par2Step`** (`archive_par2`, id 1, rank 100). `par2_source`, fetch its first article, `inventory`. Not `complete` and under `PAR2_BODIES = 2`: fetch the next article (base: `First(2)`'s second; volume: `Last(1)`), join and parse again. Empty list: done, `matched: false`. Otherwise `finish(SOURCE_PAR2, ..)`.

**`ListStep`** (`archive_list`, id 2, rank 110):

```rust
let mut left = limits.max_bodies_per_release as usize;
for head in select::plan(&files, limits.probe_files as usize) {
    let Some(seg) = first_article(ctx, &head)? else { continue };
    // left == 0: None. A probe waits for its slot, then fetch_prefix with PROBE_PREFIX; others fetch_bodies
    let Some(body) = fetch_one(ctx, &head, &seg, &mut left)? else { continue };
    match identify(&body) {
        Format::Rar4 | Format::Rar5 => return Ok(finish(ctx, SOURCE_RAR, rar::list(&body))),
        Format::Zip => return Ok(finish(ctx, SOURCE_ZIP, zip_list(ctx, &head, &body, &mut left)?)),
        Format::SevenZip => return Ok(finish(ctx, SOURCE_7Z, seven_list(ctx, &head, &body, &mut left)?)),
        Format::Par2 => return Ok(finish(ctx, SOURCE_PAR2, par2_listing(&body))),
        Format::Image => return Ok(Outcome::default()),   // images: stop, with at most a prefix read
        Format::Media | Format::Unknown => continue,
    }
}
Ok(Outcome::default())
```

- Probe slot: a hang-up means a new login for the next request, and a refused login rests that server for 30 minutes, indexing included (`AUTH_REST`, `src/nntp.rs:128,1356`). Upstream saw bursts of logins refused (`BUGS.md` #49). So `ListStep` holds `next_probe: Mutex<Option<Instant>>`, and before each `fetch_prefix` it takes a slot from `next_probe_slot(next: &mut Option<Instant>, now: Instant, gap: Duration) -> Instant`, with `gap = 60 s / probes_per_minute`, and sleeps until then in steps of at most 250 ms, checking `ctx.stopping()` between them and returning `StepError::Stopped` when it is set. The lock is not held across the sleep.
- `zip_list`: `list_tail(&body)` first, which covers a one-article ZIP. Without files, fetch the tail's last article and call `zip::list(&body, &tail)`.
- `seven_list`: `sevenz::list(&body, &[])`. Stop unless `need_more_tail`. For a probe, `32 + offset + size > expected * seg.bytes * 11 / 10` means more volumes than this file and the last is unknown: stop without a fetch. Otherwise fetch the last article and list again. Still `need_more_tail`, a second-last row with the adjacent part exists, and `left > 0`: fetch it, prepend it, list again.

**`NfoStep`** (`archive_nfo`, id 3, rank 120). `applies` also needs `release.display_name.is_none()`. Does what `name_sources` did for NFOs: `nfo_sources`, first article of each, up to 2 bodies, `nfo::display_name`. Writes only the name.

All three: `needs: Bodies`; `applies` is false when `classify(&release.name)` is `Image`, because a one-image post keeps its file's name and a merged album takes its shortest file's (Part 2 §4.4), so those releases never enter the net lane; `reruns: Reruns { on_rename: false, on_growth: true }`; `enabled` copies `cfg.archive` into a `Mutex<ArchiveConfig>` the step holds and returns `cfg.archive.enabled`, because `StepCtx` has no config.

`finish(ctx, source, listing) -> Outcome`, `id` being the release id:

- `writes`: `DELETE_SOURCE`; unless `meta.archive_name_bytes` on `ctx.conn()` has reached `max_names_mb * 1 MiB / SHARDS`, `INSERT_FILE` for each of `collapse_volumes(files)` with `seq = source * 65_536 + i` and `size` via `i64::try_from(..).ok()`; an `update` adding `len(name) + 32` over those rows to `archive_name_bytes` in `meta`; and, unless `source == SOURCE_PAR2`, `UPSERT_RELEASE` with `listing.password().code()`. Past the quota the step still reports the password and picks a name, stores no inner files, and counts them in `archive_names_over_quota` for the dashboard. A rerun moves the counter by the net change, and a deleted release gives its bytes back through the `archive_names_give_back` trigger.
- `matched`: files were stored or the password is yes. `rerun`: `RERUN_ON_INNER_FILES` when files were stored.
- `name`, from `best_name` over the full list:

| Source | Set when | `NameChange.source` |
|---|---|---|
| PAR2 | `display_name` is none, or `name_source` is this source | `NAME_SOURCE_PAR2 = 20` |
| RAR, 7z, ZIP | as above, and `release.is_obfuscated` or `looks_random(stem(release.name))` | `NAME_SOURCE_INNER = 15` |
| NFO | as PAR2 | `NAME_SOURCE_NFO = 10` |

`NameChange.obfuscated` is `Some(release.is_obfuscated)`: inline lookups leave the flag alone and so does this. Part 5's corrected check owns it.

`Listing::password()`: `encrypted` is yes. Otherwise `complete` with no files and no damage is no. No files is maybe. Every file an archive (`(?i)\.(rar|zip|7z|ace|cab|arj|lzh|r\d{2})$`) is maybe. Otherwise no.

### Inline lookups (`mod.rs`)

```rust
static INLINE: AtomicBool = AtomicBool::new(true);
pub fn inline_lookups() -> bool { INLINE.load(Ordering::Relaxed) }
pub fn stage_owns_names(cfg: &Config) -> bool {
    cfg.postprocess.enabled && cfg.postprocess.body_fetches_per_minute > 0 && cfg.archive.enabled
}
pub fn sync_inline_lookups(cfg: &Config) { INLINE.store(!stage_owns_names(cfg), Ordering::Relaxed) }
```

| Config | Stage | `save_slice` | Bodies for names |
|---|---|---|---|
| Existing `config.json`, no keys | On, budget 0 | Inline | As today. Steps never run |
| `postprocess.enabled: false` | Off | Inline | As today |
| `body_fetches_per_minute > 0` | On, steps on | No network | Stage, within the budget |
| Budget set, `archive.enabled: false` | On, steps off | Inline | As today; the budget serves other parts |
| Menu `Index` (`Indexer::index_group`) | Not running | Inline, the static stays `true` | As today |

Once inline is off, a new release has no real name until its step runs: 15 minutes to 2 hours of settling plus the queue. Until then search hides it if it is obfuscated (`src/search.rs:36-39`), as it hides any obfuscated release with no name. A release outside `body_min_size_bytes..=body_max_size_bytes` gets no body, and a release Part 3's rules still reject, such as one of only `.par2` files, reaches no step, so neither gets a name from this part, where inline looked at everything. Part 5's database steps still reach the first kind.

## 5. Schema and migration

Names differ from the ones first proposed (`release_inner_files`, `release_archive`) because Part 3 assigns the `archive_` prefix to this part and Part 3 wins. Part 5 reads through the functions in section 6.

```sql
create table if not exists archive_inner_files (
    release_id INTEGER NOT NULL,
    seq        INTEGER NOT NULL,   -- source * 65536 + index: PAR2 rows sort first, and a source replaces only its own
    name       TEXT NOT NULL,      -- clean_name'd, at most 512 bytes
    size       INTEGER,            -- unpacked bytes, NULL when unknown
    source     INTEGER NOT NULL,   -- 1 PAR2, 2 RAR, 3 7z, 4 ZIP
    primary key (release_id, seq)
) without rowid;

-- a row only when an archive was identified. No row: nothing known, no API attribute
create table if not exists archive_release (
    release_id INTEGER PRIMARY KEY,
    password   INTEGER NOT NULL    -- 0 no, 1 yes, 2 maybe
);

-- DELETE_SOURCE
delete from archive_inner_files where release_id = ?1 and source = ?2;
-- INSERT_FILE
insert or replace into archive_inner_files (release_id, seq, name, size, source) values (?1, ?2, ?3, ?4, ?5);
-- UPSERT_RELEASE
insert or replace into archive_release (release_id, password) values (?1, ?2);
```

`store.rs` declares `INNER_FILES` and `RELEASE` as `ShardTable { name, create, indexes: &[], release_key: Some("release_id"), added: &[] }`. `MAX_STORED = 64` rows per source per release: 256 at the very most, about 4 in practice.

| Path | What happens | Test |
|---|---|---|
| Open | `schema::migrate` creates both empty tables and their `gc_*` triggers. No row is rewritten | `tests/pp_schema.rs` iterates the registry |
| `--compact` | Copied by name and counted by the check, ids preserved. `WITHOUT ROWID` needs no special case | `every_registered_table_and_column_survives_compaction` |
| `--convert` | Fresh shards get empty tables; the backlog walk reaches converted releases | `converted_shards_have_empty_stage_tables` |
| Wipe | Whole shard files go; `create_db` makes the tables again | `a_wipe_leaves_new_empty_tables` |
| Delete a release | `gc_archive_*` triggers | `deleting_a_release_clears_it_from_every_registered_table` |

## 6. Interfaces

### Provided to Part 5 and later

```rust
// src/archive/store.rs. `conn` is one shard with tables in `main`, i.e. `ctx.conn()`
pub const SOURCE_PAR2: i64 = 1; pub const SOURCE_RAR: i64 = 2; pub const SOURCE_7Z: i64 = 3; pub const SOURCE_ZIP: i64 = 4;
pub struct InnerFileRow { pub seq: i64, pub name: String, pub size: Option<i64>, pub source: i64 }
pub fn inner_files(conn: &Connection, release_id: i64) -> rusqlite::Result<Vec<InnerFileRow>>;   // seq order, at most 256
pub fn password(conn: &Connection, release_id: i64) -> rusqlite::Result<Option<Password>>;

// `conn` has the shards attached (s0..s7). Any failure, such as a missing table, reads as nothing known
pub fn passwords_with(conn: &Connection, ids: &[i64]) -> HashMap<i64, Password>;
pub fn passwords(ids: &[i64]) -> HashMap<i64, Password>;   // db::open, then passwords_with
pub struct InnerView { pub password: Option<Password>, pub files: Vec<InnerFileRow> }
pub fn view_with(conn: &Connection, release_id: i64) -> InnerView;
pub fn view(release_id: i64) -> InnerView;

// src/archive/names.rs
pub fn best_name<'a>(files: impl Iterator<Item = (&'a str, u64)>) -> Option<String>;
pub fn looks_random(stem: &str) -> bool;
pub fn stem(name: &str) -> &str;
pub fn strip_volume_suffix(name: &str) -> &str;
pub fn clean_name(raw: &str) -> Option<String>;

// src/par2.rs
pub struct Par2File { pub set_id: [u8; 16], pub file_id: [u8; 16], pub md5: [u8; 16], pub md5_16k: [u8; 16], pub size: u64, pub name: String }
#[derive(Default)] pub struct Inventory { pub files: Vec<Par2File>, pub complete: bool, pub truncated: bool }
pub fn inventory(data: &[u8]) -> Inventory;
pub fn file_descriptions(data: &[u8]) -> Vec<(String, u64)>;   // signature unchanged
pub fn display_name(data: &[u8]) -> Option<String>;            // signature unchanged, now best_name

// src/archive/select.rs. `mod.rs` declares `pub mod select;`, and Part 5 calls nfo_sources
pub enum Kind { Par2Base, Par2Vol(u32), Rar(u32), SevenZip(u32), Zip(u32), Numbered(u32), Nfo, Image, NotArchive, Unidentified }
pub fn classify(filename: &str) -> Kind;
pub struct Head<'a> { pub file: &'a FileInfo, pub tail: &'a FileInfo, pub probe: bool }
pub fn plan(files: &[FileInfo], probe_files: usize) -> Vec<Head<'_>>;
pub fn par2_source(files: &[FileInfo]) -> Option<&FileInfo>;
pub fn nfo_sources(files: &[FileInfo]) -> Vec<&FileInfo>;      // at most 2, fewest parts first
```

### Parsers (`src/archive/mod.rs`)

```rust
pub mod names; pub mod rar; pub mod select; pub mod sevenz; pub mod steps; pub mod store; pub mod zip;
pub const MAX_ENTRIES: usize = 4_096;
pub const MAX_NAME_BYTES: usize = 512;
pub const MAX_STORED: usize = 64;
pub const ARTICLE_CAP: usize = 4 * 1024 * 1024;
pub const PROBE_PREFIX: usize = 64 * 1024;
pub const PROBE_MIN_PARTS: i64 = 3;
pub const NAME_SOURCE_NFO: u8 = 10; pub const NAME_SOURCE_INNER: u8 = 15; pub const NAME_SOURCE_PAR2: u8 = 20;
/// step 8 is Part 5's naming step (Part 3's example) and 16 is Part 6's predb_match, asked directly because naming
/// may be off or skip a clean name. Unregistered bits are ignored until they exist
pub const RERUN_ON_INNER_FILES: StepMask = mask(StepId(8)) | mask(StepId(16));

#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Format { Rar4, Rar5, SevenZip, Zip, Par2, Image, Media, Unknown }
pub fn identify(head: &[u8]) -> Format;
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Password { No, Yes, Maybe }
impl Password { pub fn code(self) -> i64; pub fn from_code(n: i64) -> Option<Password>; }
#[derive(Debug, Clone, PartialEq, Eq)] pub struct InnerFile { pub name: String, pub size: Option<u64> }
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Listing {
    pub files: Vec<InnerFile>,    // at most MAX_ENTRIES, directories left out, names cleaned
    pub encrypted: bool,          // encryption was proven
    pub headers_encrypted: bool,  // and the names cannot be read
    pub complete: bool,           // the end of the list was seen
    pub damaged: bool,            // a header failed its CRC or its structure
    pub need_more_tail: bool,     // 7z: the header lies before the bytes given
}
impl Listing { pub fn password(&self) -> Password; }
pub fn crc32(b: &[u8]) -> u32;    // flate2::Crc

// total functions: bad bytes give a Listing that says so, never a panic or an Err
// rar::list(data) -> Listing, by marker to rar::list4 or rar::list5
// zip::list_head(data), zip::list_tail(data), zip::list(head, tail) -> Listing
// sevenz::start(head) -> Option<Start>, sevenz::list(head, tail) -> Listing
```

### Steps

| Id | Name | Rank | Needs | Bodies at most | `reruns` |
|---:|---|---:|---|---:|---|
| 1 | `archive_par2` | 100 | Bodies | 2 | growth |
| 2 | `archive_list` | 110 | Bodies | `max_bodies_per_release`, 4 | growth |
| 3 | `archive_nfo` | 120 | Bodies | 2 | growth |

Ids 4–7 and ranks 130–199 stay free. The net lane runs one step per claim, so these steps pass nothing through `Outcome::facts`; a later step reads the tables through `ctx.conn()`.

### Consumed

`Step`, `StepCtx`, `Outcome`, `StepWrite`, `NameChange`, `BodyRequest`, `Got`, `StepError`, `ShardTable`, `store::FileInfo`, `store::SegmentRef`, `store::SegmentPick`, `Stage::set_config`, `PostprocessConfig` (Part 3).

### Required of other parts

- **Part 1:** `save_slice` returns before the lookups for a blocked group.
- **Part 2:** a post's PAR2 and payload share a release only through a file counter or a group regex. `tests/archive_bg.rs` depends on that.
- **Part 3:** `StageParams.config` is a full `Config` and `Stage::start` evaluates `enabled`, so limits are set before the first run. `body_min_size_bytes`, `body_max_size_bytes` and `Got::Refused` as in "Steps". This part adds `StepCtx::fetch_prefix` and `Pool::background_prefix` beside what Part 3 ships and changes no Part 3 signature.
- **Part 5:** its naming step is `StepId(8)`, or it edits `RERUN_ON_INNER_FILES`. Its `NameSource` keeps 10, 15 and 20 for these sources, with posted file (3) and naming regex (5) below them and PreDB (100) above. It adds `naming::improves` to `finish`. Its NFO step removes `NfoStep` from `steps::all()` and retires id 3; `save_slice` does not come back.

## 7. Config

A top-level `archive` object, parsed into `Config.archive: ArchiveConfig`. Absent keys take these defaults, no key is written back, and bad values fall back in `from_value`'s lenient style (`src/config.rs:631`).

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Steps 1–3 may run. They fetch nothing while `postprocess.body_fetches_per_minute` is 0, so this default costs no network |
| `max_bodies_per_release` | `4` | Bodies one `archive_list` run may use, heads and tails together. Clamped to 1..=16; Part 3's `bodies_per_release` also applies |
| `probe_files` | `2` | Files with no recognized name to try by magic bytes, reading 64 KiB of each. 0 turns probing off. Clamped to 0..=8 |
| `probes_per_minute` | `60` | Probes over all threads. Each ends in a hang-up, and the next request logs in again. Clamped to 12..=600 |
| `max_names_mb` | `16384` | Inner file names stored over all shards, a stored name counting its bytes plus 32. About 5 times the estimate in section 9, so a hostile poster can't fill the disk. At least 64 |

To turn this on, set `postprocess.body_fetches_per_minute`. Inline lookups stop and the stage takes over.

## 8. Tasks

Each task: write the test first and see it fail for the stated reason, implement, run, commit. Append whatever attribution the executing session requires to commit messages. Run `just format` before committing.

### Task 1: Module, names, identification, test kit

**Files:** create `src/archive/mod.rs`, `names.rs`, `testkit.rs`, `tests.rs`; modify `src/lib.rs`, `Cargo.toml`, `Cargo.lock`.

- [ ] **Test first** in `src/archive/tests.rs`. Fails first: no module.
  - `identify_goes_by_magic_alone`: one row per line of the identify table, plus `Rar!\x1a\x07`, an empty slice and `PK\x07\x08` alone: `Unknown`.
  - `clean_name_table`: `"a\x1b[31mb"` to `a[31mb`, `"..\\..\\x"` to `../../x`, `"./a/"` to `a`, `"  "`, `"."` and `".."` to `None`, 2,000 `a` to 512 bytes, and 2,000 four-byte characters to at most 512 bytes with no cut character.
  - `looks_random_table`: true for `a1b2c3d4e5f6g7h8`, `1a2b3c4d`, `123456`, `8f14e45f-ceea-467f-a0e6-a33d1cdf2a10`; false for `Real.Name`, `movie`, `Example Movie`.
  - `best_name_table`, name and size pairs to result:

| Files | Result |
|---|---|
| `small.nfo` 10, `Real.Movie.2024.mkv` 5,000,000 | `Real.Movie.2024.mkv` |
| `Real.Name.2024.part01.rar` 100, `.part02.rar` 100, `Real.Name.2024.nfo` 5 | `Real.Name.2024` |
| `a1b2c3d4e5f6a7b8c9d0.part01.rar` 100, `.part02.rar` 100 | none |
| `0123456789abcdef0123456789abcdef.mkv` 4 GB, `Real.Movie.2024.1080p-GRP.nfo` 5,000 | `Real.Movie.2024.1080p-GRP` |
| the same hash file, `Real.Movie.2024.1080p-GRP-sample.mkv` 50 MB | `Real.Movie.2024.1080p-GRP-sample.mkv` |
| `Real.Movie.2024.sample.mkv` 50, `Other.Name.2024.mkv` 10 | `Other.Name.2024.mkv` |
| `Disc1/Real.Show.S01E01.mkv` 10, `Disc1/Real.Show.S01E02.mkv` 20 | `Real.Show.S01E02.mkv` |
| `cover.jpg` 10, `readme.txt` 5 | none |
| `Real.Name.7z.001` 10, `Real.Name.7z.002` 10 | `Real.Name` |
| `Real.Name.vol000+01.par2` 10, `Real.Name.par2` 1 | `Real.Name` |

  - `collapse_keeps_the_first_volume_and_sums`: 100 `x.partNN.rar` of 10 bytes plus an `.nfo` becomes 2 rows, the first sized 1,000. 100 distinct `.mkv` becomes `MAX_STORED`. `a.mkv` twice becomes 1 row.
  - `password_table`: every rule of `Listing::password`, including `a.rar` and `b.r00` only: maybe. `Password::from_code` round trips 0, 1 and 2, and is `None` for 3.
- [ ] **Implement.** `Cargo.toml` as in section 4. `#[doc(hidden)] pub mod testkit;` in `mod.rs` so integration tests can use it. `testkit.rs`, exactly:

```rust
//! Hand-built archives for tests; no archive tool is needed. bsdtar and zipfile read what these
//! build, apart from the deliberately broken cases in the tests.
use super::crc32 as crc;
fn le16(n: usize) -> [u8; 2] { (n as u16).to_le_bytes() }
fn le32(n: usize) -> [u8; 4] { (n as u32).to_le_bytes() }

// RAR4 block: crc16, type, flags, size, [add], fields
pub const R4_MARK: &[u8] = b"Rar!\x1a\x07\x00";
pub fn r4_block(ty: u8, flags: u16, fields: &[u8], add: Option<u32>) -> Vec<u8> {
    let flags = if add.is_some() { flags | 0x8000 } else { flags };
    let add = add.map_or(vec![], |a| a.to_le_bytes().to_vec());
    let h = [&[ty][..], &flags.to_le_bytes(), &le16(7 + add.len() + fields.len()), &add, fields].concat();
    [&(crc(&h) as u16).to_le_bytes()[..], &h].concat()
}
pub fn r4_main(flags: u16) -> Vec<u8> { r4_block(0x73, flags, &[0; 6], None) }
pub fn r4_end() -> Vec<u8> { r4_block(0x7b, 0x4000, &[], None) }
/// UNP_SIZE, HOST_OS 3, FILE_CRC 0, FTIME, UNP_VER 29, METHOD stored, NAME_SIZE, ATTR, name
pub fn r4_file_fields(name: &[u8], unp: u32) -> Vec<u8> {
    [&unp.to_le_bytes()[..], &[3, 0, 0, 0, 0], &0x5a21_5a21u32.to_le_bytes(), &[29, 0x30], &le16(name.len()), &0x20u32.to_le_bytes(), name].concat()
}
pub fn r4_file(flags: u16, name: &[u8], data: &[u8]) -> Vec<u8> {
    [r4_block(0x74, flags, &r4_file_fields(name, data.len() as u32), Some(data.len() as u32)), data.to_vec()].concat()
}
pub fn rar4(blocks: &[Vec<u8>]) -> Vec<u8> { [R4_MARK, &blocks.concat()].concat() }

// RAR5 block: crc32, size, type, flags, [extra size], [data size], fields, extra; then data
pub const R5_MARK: &[u8] = b"Rar!\x1a\x07\x01\x00";
pub fn vint(mut n: u64) -> Vec<u8> {
    let mut o = Vec::new();
    loop { let b = (n & 0x7f) as u8; n >>= 7; if n == 0 { o.push(b); return o; } o.push(b | 0x80); }
}
pub fn r5_block(ty: u64, fields: &[u8], extra: &[u8], data: Option<&[u8]>) -> Vec<u8> {
    let flags = u64::from(!extra.is_empty()) | if data.is_some() { 2 } else { 0 };
    let extra_size = if extra.is_empty() { vec![] } else { vint(extra.len() as u64) };
    let data_size = data.map_or(vec![], |d| vint(d.len() as u64));
    let inner = [&vint(ty)[..], &vint(flags), &extra_size, &data_size, fields, extra].concat();
    let body = [vint(inner.len() as u64), inner].concat();
    [&crc(&body).to_le_bytes()[..], &body, data.unwrap_or(&[])].concat()
}
pub fn r5_main() -> Vec<u8> { r5_block(1, &vint(0), &[], None) }
pub fn r5_end() -> Vec<u8> { r5_block(5, &vint(0), &[], None) }
/// file flags, unpacked size, attributes 0x20, [mtime if 2], [crc 0 if 4], compression 0, host 0, name length, name
pub fn r5_file_fields(file_flags: u64, name: &[u8], unp: u64) -> Vec<u8> {
    let mtime: &[u8] = if file_flags & 2 != 0 { &[0, 0, 0, 0x60] } else { &[] };
    let data_crc: &[u8] = if file_flags & 4 != 0 { &[0; 4] } else { &[] };
    [&vint(file_flags)[..], &vint(unp), &vint(0x20), mtime, data_crc, &[0, 0], &vint(name.len() as u64), name].concat()
}
/// `ty` 2 file, 3 service. `file_flags` 4 = crc present, 5 = a directory too. A data area is always flagged, as WinRAR does
pub fn r5_file(ty: u64, file_flags: u64, name: &[u8], extra: &[u8], data: &[u8]) -> Vec<u8> {
    r5_block(ty, &r5_file_fields(file_flags, name, data.len() as u64), extra, Some(data))
}
/// file extra record 1: size, type 1, version 0, flags 0, kdf 15, salt 16, iv 16
pub fn r5_crypt_record() -> Vec<u8> {
    let b = [&[1u8, 0, 0, 15][..], &[0x53; 16], &[0x49; 16]].concat();
    [vint(b.len() as u64), b].concat()
}
/// encryption header, type 4: version 0, flags 1, kdf 15, salt 16, check 12
pub fn r5_crypt_header() -> Vec<u8> { r5_block(4, &[&[0u8, 1, 15][..], &[0x53; 16], &[0x43; 12]].concat(), &[], None) }
pub fn rar5(blocks: &[Vec<u8>]) -> Vec<u8> { [R5_MARK, &blocks.concat()].concat() }

// 7z: magic, version, start crc, next offset, next size, next crc, packed bytes, next header
pub fn sz_num(n: u64) -> Vec<u8> {
    match n {
        0..=0x7f => vec![n as u8],
        0x80..=0x3fff => vec![0x80 | (n >> 8) as u8, n as u8],
        _ => [&[0xff][..], &n.to_le_bytes()].concat(),
    }
}
pub fn sz_wrap(packed: &[u8], next: &[u8]) -> Vec<u8> {
    let start = [&(packed.len() as u64).to_le_bytes()[..], &(next.len() as u64).to_le_bytes(), &crc(next).to_le_bytes()].concat();
    [&b"7z\xbc\xaf\x27\x1c\x00\x04"[..], &crc(&start).to_le_bytes(), &start, packed, next].concat()
}
pub const SZ_COPY: &[u8] = &[0x01, 0x01, 0x00];
pub const SZ_AES_DATA: &[u8] = &[0x01, 0x24, 0x06, 0xf1, 0x07, 0x01, 0x02, 0xd3, 0x00];
/// 04 streams for one folder with `coder`, split into `sizes` streams
pub fn sz_main_streams(pack_len: u64, sizes: &[u64], coder: &[u8]) -> Vec<u8> {
    let n = sz_num(pack_len);
    let sub = if sizes.len() == 1 { vec![] } else {
        [vec![0x0d], sz_num(sizes.len() as u64), vec![0x09], sizes[..sizes.len() - 1].iter().flat_map(|s| sz_num(*s)).collect()].concat()
    };
    [&[0x04, 0x06, 0x00, 0x01, 0x09][..], &n, &[0x00, 0x07, 0x0b, 0x01, 0x00], coder, &[0x0c], &n, &[0x00, 0x08], &sub, &[0x00, 0x00]].concat()
}
/// 05 files. `empty_bits` is one byte of the empty stream vector
pub fn sz_files(names: &[&str], empty_bits: Option<u8>) -> Vec<u8> {
    let utf16: Vec<u8> = names.iter().flat_map(|n| n.encode_utf16().chain([0])).flat_map(u16::to_le_bytes).collect();
    let empty = empty_bits.map_or(vec![], |b| vec![0x0e, 0x01, b]);
    [&[0x05][..], &sz_num(names.len() as u64), &empty, &[0x11], &sz_num(utf16.len() as u64 + 1), &[0x00], &utf16, &[0x00]].concat()
}
pub fn sz_plain_header(pack_len: u64, sizes: &[u64], names: &[&str], empty_bits: Option<u8>) -> Vec<u8> {
    [&[0x01][..], &sz_main_streams(pack_len, sizes, SZ_COPY), &sz_files(names, empty_bits), &[0x00]].concat()
}
/// raw LZMA1 and its 5 property bytes, from the crate's encoder (a .lzma file is 13 header bytes, then raw)
pub fn lzma_raw(plain: &[u8]) -> ([u8; 5], Vec<u8>) {
    let mut alone = Vec::new();
    lzma_rs::lzma_compress(&mut &plain[..], &mut alone).unwrap();
    (alone[..5].try_into().unwrap(), alone[13..].to_vec())
}
/// 17 EncodedHeader: a packed header at `pack_pos`, coder LZMA
pub fn sz_encoded_header(pack_pos: u64, pack_len: u64, unp: u64, props: &[u8; 5], crc32: u32) -> Vec<u8> {
    [&[0x17, 0x06][..], &sz_num(pack_pos), &[0x01, 0x09], &sz_num(pack_len), &[0x00, 0x07, 0x0b, 0x01, 0x00, 0x01, 0x23, 0x03, 0x01, 0x01, 0x05], props, &[0x0c], &sz_num(unp), &[0x0a, 0x01], &crc32.to_le_bytes(), &[0x00, 0x00]].concat()
}
/// `payload`, then an LZMA packed plain header listing `names`, laid out as 7-Zip does. One stream per name: 1 byte each, the last takes the rest
pub fn sz_encoded(payload: &[u8], names: &[&str]) -> Vec<u8> {
    let mut sizes = vec![1u64; names.len() - 1];
    sizes.push(payload.len() as u64 - sizes.len() as u64);
    let plain = sz_plain_header(payload.len() as u64, &sizes, names, None);
    let (props, packed) = lzma_raw(&plain);
    let next = sz_encoded_header(payload.len() as u64, packed.len() as u64, plain.len() as u64, &props, crc(&plain));
    sz_wrap(&[payload, &packed].concat(), &next)
}
/// EncodedHeader with folder LZMA then AES and bind pair (0, 1), as `7z a -p -mhe=on` writes it. bsdtar says "header is encrypted"
pub fn sz_encrypted_header(pack_pos: u64, pack_len: u64) -> Vec<u8> {
    [&[0x17, 0x06][..], &sz_num(pack_pos), &[0x01, 0x09], &sz_num(pack_len), &[0x00, 0x07, 0x0b, 0x01, 0x00, 0x02, 0x23, 0x03, 0x01, 0x01, 0x05, 0x5d, 0, 0, 0x10, 0, 0x24, 0x06, 0xf1, 0x07, 0x01, 0x02, 0xd3, 0x00, 0x00, 0x01, 0x0c], &sz_num(100), &sz_num(pack_len), &[0x00, 0x00]].concat()
}

// ZIP: stored entries, sizes equal
pub fn zip_local(flags: u16, method: u16, name: &[u8], data: &[u8]) -> Vec<u8> {
    [&b"PK\x03\x04\x14\x00"[..], &flags.to_le_bytes(), &method.to_le_bytes(), &[0; 4], &crc(data).to_le_bytes(), &le32(data.len()), &le32(data.len()), &le16(name.len()), &[0; 2], name, data].concat()
}
pub fn zip_central(flags: u16, method: u16, name: &[u8], data: &[u8], offset: u32) -> Vec<u8> {
    [&b"PK\x01\x02\x14\x00\x14\x00"[..], &flags.to_le_bytes(), &method.to_le_bytes(), &[0; 4], &crc(data).to_le_bytes(), &le32(data.len()), &le32(data.len()), &le16(name.len()), &[0; 12], &offset.to_le_bytes(), name].concat()
}
pub fn zip_eocd(entries: u16, cd_size: u32, cd_offset: u32, comment: &[u8]) -> Vec<u8> {
    [&b"PK\x05\x06\0\0\0\0"[..], &entries.to_le_bytes(), &entries.to_le_bytes(), &cd_size.to_le_bytes(), &cd_offset.to_le_bytes(), &le16(comment.len()), comment].concat()
}
pub fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let (mut body, mut cd) = (Vec::new(), Vec::new());
    for (n, d) in files {
        cd.extend(zip_central(0, 0, n.as_bytes(), d, body.len() as u32));
        body.extend(zip_local(0, 0, n.as_bytes(), d));
    }
    let eocd = zip_eocd(files.len() as u16, cd.len() as u32, body.len() as u32, b"");
    [body, cd, eocd].concat()
}
```

  Also in `tests.rs`, for Tasks 2–4:

```rust
/// every prefix: no panic, a prefix of the full list, and never complete
fn every_cut(full: &[u8], parse: impl Fn(&[u8]) -> Listing) {
    let whole = names(&parse(full));
    assert!(!whole.is_empty());
    for i in 0..full.len() {
        let part = parse(&full[..i]);
        assert!(whole.starts_with(&names(&part)) && !part.complete, "cut {i}");
    }
}
```

- [ ] **Run:** `cargo test --lib archive`. **Expected:** pass.
- [ ] **Commit:** `Add the archive module: format identification, name rules and a test kit`

### Task 2: RAR4 and RAR5

**Files:** create `src/archive/rar.rs`; tests in `tests.rs`.

- [ ] **Tests first.** Fails first: no `rar`.
  - Listing: `rar4(&[r4_main(0), r4_file(0, b"Example.Movie.2024.mkv", b"hello world"), r4_file(0, b"second.nfo", b"abc"), r4_end()])` gives both names, sizes 11 and 3, `complete`, password no. `rar5(&[r5_main(), r5_file(2, 4, b"Example.Movie.2024.mkv", &[], b"hello world"), r5_file(2, 5, b"folder", &[], b""), r5_file(3, 4, b"CMT", &[], b"note"), r5_file(2, 4, "Фильм.nfo".as_bytes(), &[], b"abc"), r5_end()])` gives the first and last names: directory and service left out.
  - Unicode, all with flag `0x0200`: `b"____.mkv\0"` plus `[0x04, 0b0101_0101, 0x24, 0x38, 0x3b, 0x4c, 0b0100_0000, 0x3c, b'.', b'm', b'k', 0, b'v']`: `Фильм.mkv`. `"Фильм2.mkv"` as UTF-8 with no NUL: itself. `b"abcdef.mkv\0"` plus `[0x04, 0b1100_0000, 0x88, 0x00]`: 10 chars, each `0x0400 | byte`. Plus `[0x04, 0b1100_0000, 8]`: `abcdef.mkv`. Plus `[0x04, 0b1100_0000, 0x7f]` on `b"ab\0"`: `ab`.
  - Encryption: `r4_main(0x0080)` then 64 bytes of `0xAA`: encrypted, headers encrypted, no files, password yes. `r4_file(0x0004, ..)`: encrypted, name kept. `r5_crypt_header()` then 64 bytes of `0xAA`: as the first. `r5_file(2, 4, b"locked.mkv", &r5_crypt_record(), b"x")`: encrypted, name kept.
  - Old main header with flag `0x0002`: CRC over its first 13 bytes, 5 comment bytes inside `HEAD_SIZE`, then a file: the file is listed, not damaged.
  - `every_cut` on the first two fixtures, the unicode one and the encrypted ones with files.
  - Malicious table, each with a valid CRC where the helper gives one:

| Input | Expected |
|---|---|
| RAR4 main, then raw `00 00 74 00 00 06 00` | `damaged` |
| RAR4 main, then raw `00 00 74 00 80 FF FF 00 00 00 00` | no files, not damaged, not complete |
| `r4_file_fields(b"abc", 3)` with bytes 15–16 set `FF FF`, in `r4_block(0x74, 0, .., Some(3))` | `damaged` |
| `r4_block(0x74, 0x0100, fields, Some(u32::MAX))`: fields as `r4_file_fields(b"abc", 0)` with both high words `FFFFFFFF` before the name | `abc`, size `0xFFFFFFFF00000000`, not complete |
| A RAR4 file with its last byte flipped | `damaged` |
| RAR4 main then 100,000 `r4_block(0x75, 0, &[], None)` | returns, not complete |
| RAR4 with 5,000 files | `MAX_ENTRIES` files, not complete |
| `r4_block(0x74, 0x00e0, &r4_file_fields(b"dir", 0), Some(0))`, then a file `f` | `f` only |
| RAR5 marker, `crc(body) + body`, `body = 80 80 80 01` + 8 zeros (4 byte size) | `damaged` |
| The same, `body = FF 7F` | no files, not damaged |
| The same, `body = 80` × 11 then `00` | `damaged` |
| RAR5 file fields with name length `vint(u64::MAX)` | `damaged` |
| RAR5 block with flags 2, data size `vint(u64::MAX)`, file `a` | `a`, not complete, not damaged |
| RAR5 block with flags 1, extra size `vint(5000)` | `damaged` |
| RAR5 extra area `[0, 0, 0]` | name kept, not encrypted, no spin |

- [ ] **Implement** section 4's walks.
- [ ] **Run:** `cargo test --lib archive::tests::rar && just lint`. **Expected:** pass, no lint from the `deny` line.
- [ ] **Commit:** `List inner files from RAR4 and RAR5 headers, and report encrypted headers as passworded`

### Task 3: ZIP

**Files:** create `src/archive/zip.rs`; tests in `tests.rs`.

- [ ] **Tests first.** Fails first: no `zip`.
  - `z = zip_of(&[("Example.Movie.2024.nfo", b"note"), ("Example.Movie.2024.mkv", b"hello world")])`: `list_head(&z)` both names, not complete; `list_tail(&z)` both, sizes 4 and 11, `complete`; `list(&z[..40], &z)` is complete.
  - `every_cut(&z, list_head)`. Every suffix for `list_tail`: `names(full).ends_with(&names(part))`, no panic.
  - Malicious table:

| Input | Expected |
|---|---|
| `zip_local(0, 0, b"abc", b"")` with bytes 26–27 `FF FF` | no files |
| The same with bytes 18–21 `FF FF FF FF` | `abc`, no overflow |
| `zip_of` of one file, comment length set to 26, then `PK\x05\x06` + 22 zeros | fake record ignored, the file is listed |
| `list_tail(&zip_of(&[("a", b"x"), ("b", b"y")])[70..])` | only `b`, not complete |
| `zip_central` with bytes 28–29 `FF FF` + `zip_eocd` | no files, no panic |
| `zip_local(f, m, ..)` for `(1,0)`, `(0x40,0)`, `(0x2000,0)`, `(0,99)` | encrypted |
| `zip_local(8, 8, b"a", b"")` then another entry | `a` only |
| `PK\x07\x08` + `zip_local(0, 0, b"a", b"")` | `a` |
| 5,000 entries through `list_tail` | `MAX_ENTRIES`, not complete |
| `zip_local(0, 0, b"a", b"")` with bytes 18–25 `FF`, byte 28 = 8, then extra `01 00 04 00 AA BB CC DD` | `a`, not complete, no panic |

- [ ] **Implement** section 4.
- [ ] **Run:** `cargo test --lib archive::tests::zip && just lint`. **Expected:** pass.
- [ ] **Commit:** `List inner files from ZIP local headers and the central directory`

### Task 4: 7z

**Files:** create `src/archive/sevenz.rs`; tests in `tests.rs`.

- [ ] **Tests first.** Fails first: no `sevenz`.
  - Plain: `sz_wrap(b"hello world", &sz_plain_header(11, &[11], &["Example.Movie.2024.mkv"], None))`: the name, size 11, `complete`, from the head alone.
  - Directory and sizes: `sz_wrap(b"hello world", &sz_plain_header(11, &[5, 6], &["a/one.mkv", "a/two.nfo", "a"], Some(0b0010_0000)))`: 2 files, sizes 5 and 6.
  - `NUMBER` round trip for 0, 0x7F, 0x80, 0x3FFF, 0x4000, 0x1FFFFF, 0x200000, `u32::MAX` and `u64::MAX`, in the shortest form of each, with the cursor ending at the last byte.
  - Encoded by the crate's encoder: `s = sz_encoded(&vec![7u8; 100_000], &["Example.Movie.2024.mkv", "Example.Movie.2024.nfo"])`: `list(&s[..4096], &[])` is `need_more_tail`; `list(&s[..4096], &s[s.len() - 2048..])` has both names; `list(&s[..4096], &s[s.len() - 40..])` is `need_more_tail`. Split the last 3,000 bytes 12 bytes from the end: the short part alone is `need_more_tail`, and both parts joined list both names.
  - Encoded by a real writer: this exact 279-byte archive from libarchive's 7-Zip writer, whose LZMA stream uses the match coding that the crate's literal-only encoder never produces:

```
377abcaf271c0003c146c887d5000000000000002200000000000000ee4de10a003c6abdffffffffe000000000008133
07ae0fce9c06c5092aaed56264db4fa49b7ad638b48e63717ceace6378de0d3a5df9c3539d62c1afb597588e0db18e5b
1c9a761f4ac31cf907f762e08e7dffa163afd8a80a6503b3e11d9bcd7d0865bf93542f8eb455a5e4e9d46ff627f29ea4
e800a40f213d81cae271375a43e8f1ffeebbe95fc9a80d63d598df3cfcad79a84feaff786bc7d45c3ecba6dc230d80da
dd69d17f26390b75f4753563e3ac5f574da7c13de66cb710ccb52a4d827dc02ef9d077db91b92163e31943989d8c5509
ffc3fd300017060c010980c900070b01000123030101055d000080000c83710a01be81fe670000
```

    It lists `Example.Show.2024.S01E0N.1080p.mkv` for N 1 to 8, in order, each size 1. `list(&b[..32], &b[44..])` does too; `list(&b[..32], &b[60..])` is `need_more_tail`. Its fields, for debugging: next offset `0xd5`, next size `0x22`; `17`, pack position 12, pack size 201, coder `03 01 01`, properties `5d 00 00 80 00`, unpack size 881, CRC `be 81 fe 67`.
  - Encrypted: `sz_wrap(&[0; 64], &sz_encrypted_header(0, 64))`: encrypted, headers encrypted, no files, password yes. `[0x01]` + `sz_main_streams(5, &[5], SZ_AES_DATA)` + `sz_files(&["a.mkv"], None)` + `[0x00]`: encrypted, not headers encrypted, name kept.
  - Cuts, on `sz_encoded(&vec![7u8; 5000], &["a.mkv", "b.nfo"])`: a head under 32 bytes gives no files; every head from 32 bytes up with the full tail gives the full list; every tail suffix gives no files or the full list, and at least one gives the full list. On the plain fixture, every cut of the head alone gives no files.
  - Malicious table:

| Next header in `sz_wrap(b"", ..)`, or input | Expected |
|---|---|
| Start CRC byte flipped | `damaged` |
| Last byte of the next header flipped | `damaged` |
| Next size `1 << 40`, valid start CRC | `damaged`, nothing allocated |
| `01 05 FF` + `u64::MAX` + `00 00` | `damaged`, no allocation for files |
| `01 05 01 11 03 00 61 00 00 00` (name with no terminator) | `damaged` |
| `01 05 01 11 02 01 00 00 00` (external names) | `damaged` |
| `01 04 07 0B 01 00 FF` + `u64::MAX` (coders) | `damaged` |
| `01 04 07 0B FF` + `u64::MAX` (folders) | `damaged` |
| `sz_encoded_header(0, 10, 1 << 40, &[0x5d, 0, 0, 0x10, 0], 0)`, 10 packed bytes | `damaged`, nothing allocated |
| `sz_encoded_header(0, 10, 100, &[0x5d, 0xff, 0xff, 0xff, 0xff], 0)`, junk data | `damaged`, nothing near 4 GiB allocated |
| `sz_encoded_header(u64::MAX - 1, 10, 100, &[0x5d, 0, 0, 0x10, 0], 0)` | `damaged`, no overflow |
| `17 06 00 01 09 0A 00 07 0B 01 00 01 21 21 01 18 0C 64 00 00`, 10 packed bytes (LZMA2) | `damaged`, password maybe |
| A plain header packed and wrapped in `sz_encoded_header` 3 times, packed bytes appended in order | listed. 6 times: `damaged` at `MAX_ROUNDS` |
| 5,000 names through `sz_files` | `MAX_ENTRIES`, not complete |

- [ ] **Implement** section 4.
- [ ] **Run:** `cargo test --lib archive::tests::sevenz && just lint`. **Expected:** pass.
- [ ] **Commit:** `List inner files from 7z headers, decoding the LZMA packed ones`

### Task 5: PAR2

**Files:** `src/par2.rs`; tests in its `mod tests`.

- [ ] **Tests first.** Fails first: `inventory` does not exist, and the random-name row returns a random name. `tests/common/mod.rs` and `file_desc_packet` do not change. Tests needing distinct ids use a local builder taking set id, file id, name and size.
  - `picks_largest_file` (`:102`) unchanged.
  - `a_constant_id_helper_keeps_every_name`: `file_desc_packet` for `small.nfo` and `Hidden.Show.S01E01.mkv` share an id: 2 files, `display_name` the second. The same packets twice: `file_descriptions` still has 2.
  - `a_random_named_set_gives_no_name`: file descs `a1b2c3d4e5f6a7b8c9d0.part01.rar` and `.part02.rar`: `display_name` none.
  - `volumes_give_the_set_name`: `Real.Name.2024.part01.rar`, `.part02.rar`, `Real.Name.2024.nfo`: `Real.Name.2024`.
  - `a_small_real_name_beats_a_big_random_one`: a hash `.mkv` of 4 GB and `Real.Movie.2024.1080p-GRP.nfo`: `Real.Movie.2024.1080p-GRP`.
  - `the_full_list_comes_back_with_hashes`: distinct id, MD5, MD5-16k and size per file, read back at their offsets.
  - `complete_needs_every_main_id`: a Main with 2 ids and 1 FileDesc: not complete; with both: complete; no Main: not complete.
  - `sets_stay_apart`: the same file id in two set ids gives 2 files. `unicode_name_replaces_the_ascii_one`.
  - Malicious table: length 0, 65 and `u64::MAX` before a good packet: it is found, not `truncated`. Length `1 << 30`: found and `truncated`. A Main with a count above its ids; a `FileDesc` of a 40-byte body and a `UniFileN` of 19: no panic, no file. `UniFileN` with an odd body: whole units kept. 5,000 `FileDesc`: `MAX_FILES`. A name holding an ANSI escape comes back without the control byte.
  - `every_prefix_of_a_set_is_safe`: every cut of `[Main, 2 FileDesc]`: no panic, files a prefix of the full list.
- [ ] **Implement** section 4, including the `deny` line and the `allow` on the regex statics. Task 5's code compiled clean under `cargo clippy --lib --tests` in a scratch crate.
- [ ] **Run:** `cargo test --lib par2 && cargo test --test e2e && cargo test --test unsaved_cap && just lint`. **Expected:** pass. `tests/e2e.rs:27` still names `Hidden.Show.S01E01.mkv`, and `tests/unsaved_cap.rs:27` still names `Real.Name.N.mkv`.
- [ ] **Commit:** `Read the whole PAR2 file list, and stop offering a random name as the release name`

### Task 6: Choosing what to fetch

**Files:** create `src/archive/select.rs`, with `pub mod select;` in `mod.rs`; tests in `tests.rs`.

- [ ] **Tests first.** Fails first: no `select`.
  - `classify_table`: `a.par2`, `A.PAR2` Par2Base; `a.vol000+01.par2` Par2Vol(0); `a.vol03-07.par2` Par2Vol(3); `a.part001.rar`, `a.part1.rar` Rar(1); `a.part12.rar` Rar(12); `a.rar`, `a.cbr` Rar(0); `a.r00` Rar(1); `a.r99` Rar(100); `a.s00` Rar(1001); `a.7z` SevenZip(0); `a.7z.001` SevenZip(1); `a.7z.012` SevenZip(12); `a.zip`, `a.zipx`, `a.cbz` Zip(MAX); `a.z01` Zip(1); `a.001`, `a.mkv.001` Numbered(1); `a.000` Numbered(0); `a.nfo`, `a.txt` Nfo; `a.jpg`, `a.JPEG`, `a.tif` Image; `a.mkv`, `a.sfv`, `a.iso` NotArchive; `a1b2c3d4e5f6g7h8`, `a.dat`, `a.xk29dj3`, the empty string Unidentified.
  - `plan_table`, by `FileInfo` built with a helper:
    - `x.part02.rar`, `x.part01.rar`, `x.part03.rar`: heads part01 then part02; tail part03. `x.r00`, `x.rar`, `x.r01`: heads `x.rar` then `x.r00`.
    - `x.7z.002`, `x.7z.001`, `x.7z.003`: head 001, tail 003. `x.7z`: head and tail itself. `x.z02`, `x.zip`, `x.z01`: head `.z01`, tail `.zip`.
    - A 5-part `small.rar` and a 200-part `big.part01.rar`: big first. Three sets: two kept.
    - `x.part01.rar` plus an extensionless 500-part file: the RAR only, no probe.
    - Extensionless files of 500, 900 and 1 parts, `probe_files` 2: 900 then 500, never the 1-part, `probe: true`. One of 2 parts: no heads. One of 3 parts: it probes.
    - Extensionless 500 parts plus 2 `.jpg`: no heads. Plus 1 `.jpg` of 3 files: probes. `probe_files` 0: no heads. Only `.mkv` and `.par2`: no heads.
  - `par2_source_table`: base over volume; fewest parts among bases; lowest first block among volumes; none. `nfo_sources_table`: by filename; by subject; at most 2, fewest parts first, returning `Vec<&FileInfo>`.
- [ ] **Implement** section 4, with the attribution header.
- [ ] **Run:** `cargo test --lib archive::tests::select`. **Expected:** pass.
- [ ] **Commit:** `Pick which article to fetch from file names and volume order`

### Task 7: Tables, config and reads

**Files:** create `src/archive/store.rs`; modify `src/postprocess/schema.rs`, `src/config.rs`, `config.example.json`; test `tests/archive_schema.rs`.

- [ ] **Tests first.** On `tempfile::tempdir()` databases, with no environment change.
  - `both_tables_are_registered`: `SHARD_TABLES` has both names with `release_key: Some("release_id")`. Fails first: not registered.
  - `reads_come_back_in_seq_order`: rows inserted out of order through section 5's statements: `inner_files` in `seq` order, PAR2 first, NULL size is `None`; `password` is `None` with no row and each code with one.
  - `past_the_quota_no_names_are_stored`: `max_names_mb` at its minimum and `archive_name_bytes` over it: a RAR of 64 maximum-length names adds no rows, keeps its password, and counts on the dashboard. A release under quota adds to the counter by what it stored.
  - `inner_files_ask_for_naming_and_predb`: `finish` storing files sets both step 8 and step 16 in `rerun`, with no name change.
  - `a_source_replaces_only_its_own_rows`: PAR2 and RAR rows; `DELETE_SOURCE` for RAR plus new rows: PAR2 untouched.
  - `passwords_read_across_shards`: `passwords_with` on `db::open_with_shards`, ids in shards 0 and 5 and one with no row: 2 entries. `view_with` gives rows and password.
  - `an_old_database_reads_as_nothing_known`: `archive_release` dropped from a shard: `passwords_with` gives an empty map, no panic.
  - In `src/config.rs`: `archive_defaults_when_absent`, `archive_keys_parse`, `archive_values_clamp` (`probes_per_minute` too), `a_bad_archive_value_falls_back`, `saving_writes_no_archive_key`.
- [ ] **Implement.** Add `"archive": {"enabled": true, "max_bodies_per_release": 4, "probe_files": 2, "probes_per_minute": 60}` to `config.example.json`.
- [ ] **Run:** `cargo test --test archive_schema && cargo test --test pp_schema && cargo test --lib compact && cargo test --test convert && cargo test --lib config`. **Expected:** pass, with Part 3's registry tests covering both tables and no change to them.
- [ ] **Commit:** `Add inner file and password tables, and their config`

### Task 8: Reading a prefix

**Files:** `src/nntp.rs`, `src/postprocess/step.rs`, `src/postprocess/bodies.rs`, `tests/common/mod.rs`; test `tests/background_prefix.rs`.

- [ ] **Tests first**, on the mock server. Fails first: no `background_prefix`.
  - `a_long_body_is_cut_and_its_connection_goes`: `yenc_lined` of 1 MB: `Body(b)` with `b.len() <= PROBE_PREFIX` and `b == data[..b.len()]`. The next request raises `accepted` and answers clean.
  - `a_short_body_comes_back_whole_and_the_connection_stays`: 10 KB: all of it, `accepted` unchanged on the next request.
  - `every_server_saying_no_is_missing` (`bodies: false`); `a_server_that_cant_be_reached_is_an_error_not_missing`.
  - `fetch_prefix_makes_every_check`, with Part 3's body harness: a blocked group, an image filename and a per-release count past its limit each give `Got::Refused` with `bodies_sent == 0`. One call uses one body of the per-release count.
- [ ] **Implement** section 4.
- [ ] **Run:** `cargo test --test background_prefix && cargo test --test background_body && cargo test --test pp_bodies`. **Expected:** pass, Part 3's tests unchanged.
- [ ] **Commit:** `Read the start of an article and hang up, for probes`

### Task 9: Steps

**Files:** create `src/archive/steps.rs`; modify `src/postprocess/steps.rs`; test `tests/archive_stage.rs`.

Harness: Part 3's `tests/pp_bodies.rs` harness on the mock server. `Stage::start` with `steps::all()` and `Config { postprocess: PostprocessConfig { settle_secs: 1, complete_settle_secs: 1, body_min_size_bytes: 0, body_fetches_per_minute: 60_000, ..Default::default() }, ..Default::default() }`. Bodies come from `post(n, subject, bytes, yenc_body(name, &data))` (`tests/common/mod.rs:126,150`), with message-id `<msg{n}@mock>`, or `yenc_lined` where a test reads a prefix. Releases go in through `store::save` with articles naming those ids, so every file named in a test shares its release whatever its subject. `Server.bodies_sent` (`:61`) counts fetches, and every count below is exact. Wait until `pp_state.done` has the step's bit. A post with `vec![]` as its body is an empty article, so fetching the wrong one fails the listing. Part 3's rules reject a release of only `.par2` files, so PAR2 releases also post an unfetched `x.mkv`.

- [ ] **Tests first.** Fails first: no steps.
  - `a_named_rar_set_reads_only_the_first_article_of_the_first_volume`: saved in the order part03, part01, part02, 3 articles each; only part01's article 1 has a body, RAR5. 1 body, one row `source = 2`, password 0.
  - `each_format_records_its_source`: RAR4 and RAR5 to 2 with 1 body; ZIP to 4 with 2 bodies; a one-article ZIP with 1 body; encoded 7z to 3 with 2 bodies.
  - `encrypted_headers_are_passworded_not_failed`: `r5_crypt_header`, RAR4 `0x0080` and `sz_encrypted_header`: `password = 1`, no file rows, done bit set, failed bit clear, `net_attempts = 0`.
  - `a_missing_first_article_moves_to_the_next_rar_volume`: part01 has only articles 2–3. 1 body, from part02.
  - `a_7z_header_split_across_two_articles_takes_a_second_tail`: 3 bodies, full list. `a_7z_whose_last_article_isnt_indexed_is_maybe`: `expected` 3, parts 1–2 only: 1 body, password 2.
  - `a_random_named_set_is_probed_by_magic`: extensionless files of 900 and 500 parts; on the 900's article 1, `yenc_lined` of a RAR5 with a 1 MB payload: 1 body, `source = 2`, the name listed from the prefix, and `accepted` shows the connection went. `probing_stops_at_its_cap`: 5 such files, no magic: 2 bodies, no rows, done.
  - `probe_slots_are_spaced`: `next_probe_slot(&mut next, now, gap)` is pure: from `None` it gives `now` and stores `now + gap`; two calls at one `now` give `now` and `now + gap`; a call after a long idle gives its own `now`.
  - `a_7z_found_by_a_probe_gets_its_own_tail`: 2 bodies. `a_probed_7z_longer_than_its_file_doesnt_fetch_a_tail`: 1 body, password 2.
  - `an_image_found_by_a_probe_stops_the_release`: `yenc_lined` of a 1 MB JPEG on the first of two probes: 1 body, a prefix only, no rows.
  - `random_named_single_articles_are_never_probed`, and files of 2 parts too; `image_named_files_are_never_fetched`: `x.jpg` and `x.mkv`, and `x.jpg` alone: 0 bodies. Rule 9.
  - `a_release_named_as_an_image_runs_no_step`: a release named `IMG_0001.jpg`, holding an extensionless 500-part file: 0 bodies, `net_wanted` NULL, no done bits 1–3.
  - `a_release_under_the_body_minimum_gets_no_body`: the default minimum, an `x.rar` set of 700 KB: 0 bodies, no rows.
  - `a_named_rar_that_is_really_7z_goes_by_its_bytes`: `x.rar` holding a 7z: `source = 3`. `a_named_rar_with_junk_records_nothing`.
  - `a_par2_set_is_listed_and_named`: `x.par2` listing `.partNN.rar` entries and an `.mkv`: 1 body, volumes collapsed, `source = 1`, `display_name` set, `name_source = 20`, no `archive_release` row.
  - `a_set_with_only_volumes_reads_first_and_last`: `x.vol000+01.par2`, packets in its last article: 2 bodies.
  - `an_existing_name_is_left_alone`: `display_name` set, `name_source` NULL: rows written, name unchanged.
  - `inner_names_only_replace_junk`: a good release name: rows, no rename. An obfuscated one: renamed, `name_source = 15`.
  - `an_nfo_names_a_release_nothing_else_named`: 1 body, `name_source = 10`. `an_nfo_is_skipped_once_named`: `.par2`, `.nfo` and `.mkv`: the PAR2 names it, and 1 body.
  - `inner_files_rerun_naming`: a recording test step `StepId(8)`, rank 200: it runs again after rows appear, and not without rows.
  - `a_late_article_lists_again`: run with no first article, save it, and the rows appear. `a_rerun_replaces_rows_without_duplicates`: `Stage::rerun(id, mask(StepId(2)))`: same rows.
  - `an_outage_costs_no_attempt`: a dead server: `net_attempts` 0, then the server returns and the step finishes.
  - `limits_follow_the_config`: `set_config` with `max_bodies_per_release: 1`: a probing release uses 1 body.
  - `no_budget_no_bodies`: default config: 0 bodies, no rows.
- [ ] **Implement** section 4, and three lines in `steps::all()`.
- [ ] **Run:** `cargo test --test archive_stage && cargo test --lib steps_have_unique && just test`. **Expected:** pass.
- [ ] **Commit:** `Inspect archives and PAR2 sets in processing steps`

### Task 10: Inline lookups leave the save path

**Files:** `src/indexer.rs`, `src/archive/mod.rs`, `src/bg_indexer.rs`; tests `tests/archive_inline.rs`, `tests/archive_bg.rs`, `tests/archive_bg_default.rs`.

- [ ] **Tests first.**
  - `stage_owns_names_table` in `tests.rs`: the table in section 4.
  - `tests/archive_inline.rs`, one test: index a mock with a `.par2` and an `.nfo` post through `Indexer`: names set, `bodies_sent > 0`. Call `set_inline_lookups(false)`, index new posts: `bodies_sent` unchanged, releases saved, `display_name` NULL. Fails first: no switch.
  - `tests/archive_bg.rs`, one test, env set first, as `tests/auto_compact.rs`: `run_until` with `postprocess: {body_fetches_per_minute: 6000, settle_secs: 1, complete_settle_secs: 1, body_min_size_bytes: 0}` and these four posts, the first with `yenc_body` of a PAR2 listing `Hidden.Show.S01E01.mkv`:

```
[1/2] "a1B2c3D4e5F6g7H8i9J0.par2" yEnc (1/1)
[2/2] "a1B2c3D4e5F6g7H8i9J0.mkv" yEnc (1/3)
[2/2] "a1B2c3D4e5F6g7H8i9J0.mkv" yEnc (2/3)
[2/2] "a1B2c3D4e5F6g7H8i9J0.mkv" yEnc (3/3)
```

    The counter is what makes Part 2 put both files in one release, named `a1B2c3D4e5F6g7H8i9J0` (checked by running Part 2's `group_articles` on them). Without it they are two releases, because `parse_subject` strips `.par2` and not `.mkv` (`src/parser.rs:75`), Part 3 rejects the first as `Par2Only`, and this test can't pass. Eventually `display_name` is `Hidden.Show.S01E01.mkv` and `name_source = 20`, which only the stage writes. Fails first: `name_source` NULL, from the inline path.
  - `tests/archive_bg_default.rs`, one test: the same posts with no `postprocess` or `archive` key. `display_name` set, `name_source` NULL, `archive_inner_files` empty. It passes before the change too, and guards hard rule 8.
- [ ] **Implement** section 4, with `#[doc(hidden)] pub fn set_inline_lookups(on: bool)`.
- [ ] **Run:** `cargo test --test archive_inline && cargo test --test archive_bg && cargo test --test archive_bg_default && cargo test --test e2e && just test`. **Expected:** pass.
- [ ] **Commit:** `Stop name lookups in the save path once the stage has a body budget`

### Task 11: API and terminal view

**Files:** `src/api.rs`, `src/app.rs`; tests in their modules.

- [ ] **Tests first.**
  - In `src/api.rs`: `rss_reports_a_known_password_only`: 3 `ReleaseRow`s, a map with one yes and one maybe: `<newznab:attr name="password" value="1"/>` and `value="2"`, none for the third, and the category attribute on all three. Fails first: `rss` takes no map.
  - In `src/app.rs`: `inner_table_lists_rows_and_marks_password`: 40 rows give 30 rows and `inner files (40)`. No rows gives `None`. A name holding `[red]` comes back literally through `Line::text` (`src/ui.rs:117`). The password text for yes, maybe, no and none.
- [ ] **Implement** section 4.
- [ ] **Run:** `cargo test --lib api && cargo test --lib app && cargo test --test e2e`. **Expected:** pass; `e2e` shows search still answering with no archive rows.
- [ ] **Commit:** `Report the password attribute, and show inner files in the release view`

### Task 12: Docs and full verification

**Files:** `README.md`, `config.example.json`.

- [ ] **Test first:** `readme_lists_every_archive_key` in `src/config.rs`'s tests: `include_str!("../README.md")` holds `max_bodies_per_release` and `probe_files`. Fails until the README changes.
- [ ] **Implement** section 12.
- [ ] **Run:** `just format-check && just lint && just test`, then section 11. **Expected:** all pass.
- [ ] **Commit:** `Document archive inspection`

## 9. Performance and load

These are estimates from the overview's counts and Part 3's numbers. None comes from the live database, which no step may touch. Default config: zero new network load, and inline lookups as today.

**Reach.** About 13% still holds, for a different reason. Part 3's rules no longer reject by size or image type, so these releases reach database steps; what stops the steps here is the body gate.

- 80,747,466 releases (68.2%) are one article. At about 740 KB they are under `body_min_size_bytes` (1 MiB) and get no body. `body_max_size_bytes` (100 GiB) removes a negligible count.
- That leaves 37,583,308 multi-article releases (31.8%), the ceiling by size.
- In the overview's sample of 100, 29 were fragments and 58 images. If the fragments are all one article and the other 13 all multi-article, the one-article share makes 39 of the images one article and 19 multi-article: about 22M releases that pass the size gate. Their files are refused as images, and `applies` keeps those named as images out of the net lane.
- That leaves the 13: about 15.4M releases, 8–25M at a 95% interval for a sample that size. Any of them under 1 MiB only lowers it. Of it, the 2,464,404 releases of 100+ articles (2.1%) are the clearest real posts.

The sample does not say how many of the 15.4M hold an archive. A release with no archive, PAR2 or NFO name and no probe candidate costs no body. One that passes the gate, with no such name and no image name, costs one run per step: a file read and two writer jobs.

A PAR2 names a release through the stage only when it shares the release with the payload. Part 3 rejects a release of only `.par2` files, and `parse_subject` keeps `x.par2` and `x.mkv` apart. RAR sets share a name without help; other posts need the file counter or group regex Part 2 merges on. Where they stay apart, inline named the PAR2's own row and the stage names neither.

**Cost per release:**

| Shape | `archive_par2` | `archive_list` | `archive_nfo` | Bodies | Bytes |
|---|---:|---:|---:|---:|---|
| Video with base PAR2 | 1 | 0 | 0 | 1 | About 100 KB |
| Named RAR set with base PAR2 | 1 | 1 | 0 | 2 | About 840 KB |
| Named RAR set, no PAR2, NFO | 0 | 1 | 1 or 0 | 1–2 | About 750 KB |
| 7z or multi-file ZIP with base PAR2 | 1 | 2 | 0 | 3 | About 1.2 MB: a last article averages half size |
| 7z with its header across 2 articles | 1 | 3 | 0 | 4 | About 1.9 MB |
| Random-named set, no names, RAR found | 0 | 1–2 | 0 | 1–2 | 64–128 KB: prefix reads |
| Random-named set, 7z found | 0 | 2–3 | 0 | 2–3 | About 430 KB–1.2 MB: a prefix, then its tail |
| Random-named set, nothing found | 0 | 2 | 0 | 2 | 128 KB |
| Worst case allowed | 2 | 4 | 2 | 8 | About 5.9 MB at 740 KB; 32 MiB at the 4 MiB article cap |
| Under the body minimum, only images, or only video and no PAR2 | 0 | 0 | 0 | 0 | 0 |

Assuming 2.2 bodies and 0.9 MB on average:

| Budget | Bodies/s | Releases/day | Newest 1M | All 15.4M | Traffic |
|---|---:|---:|---|---|---|
| 300/min | 5 | 196,000 | 5.1 days | 78 days | About 2 MB/s, at most 3.7; about 14 TB in all |
| 6,000/min | 100 | 3.9M | 6 hours | 3.9 days | About 41 MB/s, at most 74; the same 14 TB. Set `body_connections` to about 20 |

The 0.9 MB average predates prefix reads, so it overstates a backlog heavy in random names. New posts use `pp_queue` and go first, so the backlog never delays them. Inline fetches move to the stage rather than adding load, and save batches stop waiting on `first_names`.

**Probes.** `probes_per_minute` caps them whatever the budget: 86,400 a day at the default, so 43,000–86,000 random-named releases a day. That is the slowest lane if the backlog is rich in them, and raising it is the user's call, against the login risk in "Steps". Each costs one login beyond the normal pool churn. Named archives cost none.

**Disk.** `archive_inner_files` costs about 80 B a row: a 50-char name, the key, a size and a source, with no separate index because of `WITHOUT ROWID`. Collapsing volumes keeps a typical release to about 2.5 rows. At 15.4M releases: 38M rows, about 3.1 GB. `archive_release` costs about 15 B a row, at most 230 MB. Together that is about 1% of a 300 GB database, reached only as the budget gets there. Creating the tables costs 2 pages per shard.

**Writer.** A typical run writes 1 delete, 2–3 inserts and 1 upsert through Part 3's stage budget, about 40 µs. At 100 bodies/s over 8 writers that is at most 0.5 ms per second per writer, under 0.1%.

**Memory and CPU, per net thread.** A body of at most 4 MiB, 8 MiB for a joined tail. A 7z header decode of at most 16 MiB of dictionary and 16 MiB of output, freed after the release. Worst case 40 MiB; typically under 2 MB. RAR and ZIP parsing take microseconds. A typical 7z header decodes in under a millisecond; the 16 MiB worst case takes about half a second on the net thread, which holds up nothing else. API: at most 100 primary-key lookups per response plus one `db::open`, a few milliseconds beside the search itself.

## 10. Failure handling

| Case | What happens |
|---|---|
| Truncated article | Parsers return what they read, `complete` false. A listing with names is stored |
| Corrupt or malicious header | `damaged`. An identified archive gets password 2; the step is done, `matched` false. No retry, because the same bytes come back |
| A panic in `lzma-rs` | `catch_unwind`: `damaged`. Any other panic is caught per release by Part 3 and counts as `Retry` |
| Encrypted headers | Password 1, no file rows, step done. Never a failure |
| Named archive, wrong bytes | The magic decides. No archive magic: next candidate, then done with no rows |
| First article not indexed | No fetch for that candidate. RAR moves to the next volume |
| Article gone everywhere | `Got::Missing`: next candidate, then done with `matched` false. A later article re-runs it through `on_growth` |
| Over the article cap | `Got::TooBig`: next candidate; Part 3 drops the connection |
| Image, body bounds or per-release refusal | `Got::Refused`: next candidate. Nothing is retried |
| A login refused after a probe's hang-up | `StepError::Transient`: no attempt used. A refusal that reads as an auth failure rests that server for 30 minutes, for indexing too (`src/nntp.rs:1356`). Probes are spaced for this; lower `probes_per_minute` if it happens |
| An article of one enormous line | `body_prefix` reads lines through Part 3's `read_line_capped`, so the read stops within one read's worth past `max_bytes`, `whole` is false and the connection is dropped, as for any cut prefix. The same bound Part 3's `body_capped` has |
| Server down, refused login, timeout | `StepError::Transient` from `fetch_bodies`: no attempt used, the lane pauses. Inline lookups stay off; names wait |
| Budget used up | `OutOfBudget`: no attempt used, looked at again in 60 s |
| Corrupt sealed blob | `ctx.segments` error: `Retry`, then the failed bit. The release stays searchable |
| Crash mid-run | Nothing was written: Part 3's write-ahead counted one attempt and the run repeats. Rows replace by source, so a re-run is idempotent |
| A release deleted during a run | Part 3's apply skips all writes |
| Article arrives during a run | Part 3 keeps the queue row. `on_growth` lists again after settling |
| Re-run | `DELETE_SOURCE` then inserts, in one transaction. `atlas --reprocess --step archive_list --apply` re-fetches on purpose |
| Budget turned on while indexing | Takes effect at the next `watch_config` tick. A save in flight finishes inline |
| Budget turned off | Inline returns at the next tick. Steps stay queued in `net_wanted`, unrun |
| `archive_release` missing on an old database | `passwords` reads as nothing known; the API omits the attribute. Migration on open adds the table |

## 11. Acceptance checks

Run in the part's worktree. None touch live data or a live server.

```bash
just format-check && just lint && just test
# expected: all pass, no warnings, the parser modules' deny line satisfied

cargo test --lib archive && cargo test --lib par2
# expected: pass, including every truncation and malicious table, and a_random_named_set_gives_no_name

cargo test --test background_prefix
# expected: pass: a long body is cut, and its connection goes

cargo test --test archive_stage
# expected: pass, with exact bodies_sent counts for each release shape

cargo test --test archive_bg_default
# expected: pass: no new config, inline name, no stage rows

cargo test --test archive_bg
# expected: pass: with a budget, name_source is set, so the stage named it

! grep -nE 'fetch_body|background_body|background_prefix|first_names' src/archive/*.rs
# expected: no output, exit 0: every body goes through StepCtx::fetch_bodies

! grep -nE '\.unwrap\(\)|\.expect\(|panic!' src/archive/rar.rs src/archive/zip.rs src/archive/sevenz.rs
# expected: no output, exit 0

cargo test --test pp_schema && cargo test --lib compact && cargo test --test convert
# expected: pass, with both tables covered by the registry tests

grep -n 'lzma-rs' Cargo.toml Cargo.lock
# expected: both files list it, and cargo test --locked above already proved the lock is current
```

## 12. Docs

- `README.md`, "Top-level keys" (`:127`): an `archive` row pointing to a new table.
- New `### Archive inspection` under Usage: what is read (one article per archive set, about 740 KB, plus a tail for 7z and ZIP), what is stored, how to turn it on through `postprocess.body_fetches_per_minute`, what that does to inline lookups, how late names arrive and that releases outside the body bounds get none, probing, what it reads of an image it can't recognize by name and why it is spaced, per-release cost and section 9's estimates labelled as estimates.
- "Search results" (`:426`): `<newznab:attr name="password" value="N"/>`, 0 no, 1 yes, 2 maybe, absent when nothing is known.
- "Search" (`:283`): the release view shows inner files and password status.
- "Source layout" (`:491`): rows for `src/archive/`; the `src/par2.rs` row mentions the full file list.
- "Limitations" (`:510`): replace "Deobfuscation works only when a release has a par2 or nfo file with a usable name" with the new limits: a usable name in PAR2, NFO or an archive's first article, and a 7z or ZIP needs its last article.
- "Credits" (`:516`): newznab-tmux, GPLv3, for selection rules; `lzma-rs`, MIT.
- `config.example.json`: the block. Dashboard: no change; Part 3's page lists these steps by name.

## 13. Decisions

1. **Tables are `archive_inner_files` and `archive_release`.** The requested names were `release_inner_files` and `release_archive`, but Part 3 assigns the `archive_` prefix to this part and Part 3 wins (`00-overview.md`). Columns are as requested, plus `WITHOUT ROWID`. Part 5 calls `store::inner_files` and `store::password`, so no other part spells these names.
2. **7z fetches the last article, up to 2 of them, and decodes LZMA1.** 7-Zip compresses headers by default, so without decoding nearly every real 7z would list nothing, and upstream shows that by reaching for an external client. Without a tail there is no list at all. The packed header is written right before the end header and is small, so 2 articles reach anything worth listing. More would fetch megabytes for a list capped at 64 rows.
3. **`lzma-rs` and not a handwritten decoder, `sevenz-rust2`, `xz2` or `lzma-rust2`.** A decoder is about 250 more lines of attacker-facing code to own. `lzma-rs` is pure Rust and MIT, with a `memlimit`, and takes the raw property form 7z uses; the prototype decoded a real archive with it. `sevenz-rust2` reads whole seekable archives, which Atlas does not have. `xz2` adds a C build on three platforms. `lzma-rust2` works too, but its default features pull in more and its raw reader has no memory limit option. `lzma-rs` has had no release since 2023, which is acceptable for a frozen format with its input capped and its panics caught.
4. **LZMA2, BCJ and other header coders are not decoded.** 7-Zip writes LZMA1 for headers. `lzma2_decompress` takes no memory limit. Such an archive reads as password "maybe".
5. **ZIP reads the head, then the tail.** The head gives a password flag and a name for 1 body. A multi-file ZIP's real list is only at the end, and a last article is about half size, so 1 more body is worth it. A one-article ZIP finds its directory in the head and fetches nothing more.
6. **RAR uses one article per set.** A scene set has one inner file and its header is in the first bytes. Later file headers could be reached by computing their article from `PACK_SIZE`, at a body each, for names the first file already gave.
7. **Parsers are total functions returning `Listing`.** Truncation is the normal state of an article, not an error, and a caller wants whatever was read.
8. **Header CRCs are enforced except on old RAR4 blocks and the end block.** The CRC is what separates an archive from bytes that happen to start with a magic. PAR2 packet MD5 is not checked: it would add a crate for a check a hostile poster can satisfy, and names are sanitized regardless.
9. **Password semantics.** Yes needs proof from the bytes. Maybe means an archive was identified but its contents could not be seen, including archives holding only archives. No means names were read with no encryption flag, even though later files were not seen. Using maybe for an incomplete list would mark nearly every multi-volume RAR, since one article never reaches the end, and the attribute would mean nothing.
10. **No `archive_release` row for PAR2 or NFO alone, and no API attribute without a row.** PAR2 says nothing about passwords. An absent attribute is honest; `0` would be a claim.
11. **Three steps at ids 1–3, ids 4–7 left free.** Separate done bits mean `--reprocess --step archive_list` does not refetch PAR2, and a failure in one does not hold the others. PAR2 goes first because it is cheapest and most often names a release, NFO last because it runs only when nothing else named it. The order matches `name_sources` (`src/indexer.rs:1642`) for those two.
12. **The NFO step lives here for now.** Moving inline lookups out of `save_slice` must not lose NFO names between this part and Part 5, even though Part 3 lists NFO under Part 5. `archive_nfo` does only what the inline code did. Part 5 removes it.
13. **"Stage on" means it can fetch.** Part 3's stage is on by default with a budget of 0, so "stage on stops inline" taken literally would drop every inline name on upgrade, breaking hard rule 8. `stage_owns_names` requires `postprocess.enabled`, a body budget and `archive.enabled`. It is a static because saves have no config access, as with Part 3's delays.
14. **Limits are copied in `enabled`.** `StepCtx` exposes no config, and a second path to it would be the second mechanism this plan was told not to invent.
15. **Steps name a release only when it has no name.** Name quality belongs to Part 5. An inner scene name such as `grp-ttl.mkv` is often worse than the post's own, so inner names also need an obfuscated or random-looking release. Part 5's `NameSource` keeps these at 10, 15 and 20, with posted file (3) and naming regex (5) below them and PreDB (100) above, and it adds `naming::improves` to `finish`, so a weak name written here can't lock out a better one.
16. **`display_name` returns nothing rather than a random name.** Today a random-named set gets a random `display_name`, and `VISIBLE_RELEASE` (`src/search.rs:36-39`) then shows it as if real. Those releases go back to hidden, which is the point. This is the one change that reaches the default inline path; it changes no network load.
17. **Archive volume suffixes are stripped from a name, other extensions stay.** `Real.Name.part01.rar` is never the name of anything. `tests/e2e.rs` and `tests/unsaved_cap.rs` expect `.mkv` kept, and Part 5 cleans it if it wants to.
18. **PAR2 hashes are returned by `inventory` and not stored.** Nothing in this part uses them, and the requested schema has no column for them. If Part 5 wants upstream's hash matching, it adds its own table and a reprocess at the body cost it then decides on.
19. **Volumes collapse before storing, cap 64 per source.** A 64-volume set's PAR2 list holds one fact. Upstream stores 21 files; 64 covers a season pack. Without collapsing, the backlog could cost almost 80 GB instead of 3 GB. The cap is per source, so a PAR2 list cannot crowd out the archive's.
20. **`seq` is `source * 65536 + index`.** No read before write, so a step's writes stay static, a source replaces only its own rows, and order is stable.
21. **`on_growth` re-runs, with no skip when rows exist.** A step cannot tell a growth re-run from a `--reprocess`, and a skip would break the second. Growth past settling is rare, and the cost is a body or two within the budget.
22. **A probe needs 3+ parts, reads 64 KiB and stops at an image.** Hard rule 9 forbids fetching images for names, and a random-named one can't be recognized before reading it. So the floor skips the shapes most images take, the read stops at what the magic table needs, and an image ends the release. Every probe gets the short read, not only small files, because a large photo is as much an image as a small one and the prefix is enough for a RAR or a 7z start header. `max_bytes` on `fetch_bodies` can't do this: over the cap it answers `TooBig` with no bytes, hence `fetch_prefix`. It's added beside Part 3's API, so `BodyRequest` and `Got`, which later parts build and match, don't change. `probe_files` defaults to 2 rather than upstream's 3, because a RAR volume answers on any file and the third rarely adds anything.
23. **Unknown extensions are probed, known non-archives never.** `.dat` and random suffixes are how obfuscators post. `.mkv`, `.iso` or `.exe` would be whole-article fetches that upstream also skips.
24. **Sets are chosen by size, 2 at most.** The largest set is the release, and a second covers a first whose heads are missing. Listing every set of a season pack would multiply bodies for the same name.
25. **A failure to find names is done, not retried.** The same article gives the same bytes. Retries are for I/O, and Part 3 already separates `Transient`.
26. **The API reads passwords after the search, not in it.** `COLUMNS` (`src/search.rs:32`) has no shard schema and feeds four query forms, and `ReleaseRow` is built in tests and `convert.rs`. A separate lookup of at most 100 primary keys touches none of it. `rss` takes a map so its test needs no database.
27. **Magic at offset 0, and whole articles for named archives.** A scan costs CPU on every body and invites a false match in compressed data, and SFX archives are rare on Usenet. Prefix reads could also cut a named head from 740 KB to 64 KB, about 10 TB over the backlog, but each means a new login, and at 100 bodies/s that is a sustained login burst able to rest a server for 30 minutes. Probes pay it because they are rare, spaced, and don't know what they're reading.
28. **The test kit compiles into the library, `#[doc(hidden)]`.** Integration tests need the same bytes and cannot see `#[cfg(test)]`. Part 3 does the same with `set_test_steps`. It has no runtime path.
29. **Releases outside the body bounds lose inline naming once the stage owns names.** A real 800 KB post with an NFO loses a name inline would have given it. Upstream's 1 MiB floor is about what's worth a body, and Part 3's gate is the one place that decides it. Such a row stays as visible as an unnamed one is today, Part 5's database steps still reach it, and a user who wants it can lower `body_min_size_bytes`. Only a user who sets a budget sees this; the README says so.
30. **A release named as an image runs no step, without reading its files.** `applies` gets no file list, and the name is right for a one-image post and an album. It keeps most of about 22M multi-article images out of the net lane, three runs each, where every run costs two writer round trips that compete with real work. The loss is an archive posted in one release under an image's name, which Part 5's database steps still see.
31. **A PAR2 file's key includes its name.** The spec says one id means one file, so `(set id, file id)` should be enough, but keeping both names on a malformed set costs nothing, and it leaves the two constant-id test helpers that other parts' tests build on untouched.
32. **Parsers are written from the formats, not from `unrar` source.** UnRAR's licence restricts reusing its source. These files read header fields only and contain no decompression, and libarchive and upstream's `formats.rs` were the independent checks.
33. **Stored names have a byte limit and a total.** Article, body and row caps limit the rate, not the sum, and a name of 512 four-byte characters is 2 KB. A counter in `meta` is one small write in the transaction that stores the rows, where sizing the table would be a scan. Hitting the total loses inner file lists, never the password or indexing.
