# W2a: Pure-Rust decompressors for archive prefix metadata

Scope: which crates decode each codec found in RAR (v2.9/3.x, v5, v7), 7z, and zip-like containers, and whether each can decode a PREFIX of a compressed stream (first N compressed bytes -> first M output bytes) without the rest. Crate metadata checked against crates.io on 2026-11-02. Items marked UNVERIFIED were not confirmed against source or docs.rs.

## Method notes

- crates.io API metadata and READMEs fetched via `https://crates.io/api/v1/crates/<name>[/<ver>/readme]`.
- Source inspected for `sevenz-rust2` 0.24.2 (Cargo.toml features, lib.rs exports) and `rars` 0.12.3 (streaming.rs, codec/ and filter.rs, lib.rs options).
- docs.rs and per-crate source for lzma-rust2, ruzstd, ppmd-rust internals were NOT read in depth. Streaming-behaviour statements for them are UNVERIFIED.
- The "prefix decode" question is about the decoder's behaviour when its input hits EOF early. A streaming decoder emits output as it goes, so bytes already produced before the EOF are normally valid. Whether a given decoder flushes output that early is UNVERIFIED for every crate below. It needs a test with a truncated real archive.

## Crate inventory

| Crate | Version (crates.io) | Pure Rust / FFI | License | Last update | Codecs / role | Notes |
|---|---|---|---|---|---|---|
| sevenz-rust2 | 0.24.2 | Pure Rust | Apache-2.0 | 2026-10-30 | 7z reader/writer. Codecs: COPY, LZMA, LZMA2, BZIP2, PPMD, AES-256 (feature `aes256`: aes, cbc, sha2), DEFLATE/ZSTD/LZ4/BROTLI behind optional features (`deflate` -> flate2, `zstd` -> zstd crate = FFI). Filters: BCJ x86, ARM, ARM64, ARM_THUMB, RISC_V, PPC, SPARC, IA64, BCJ2 (decode only), DELTA. | Fork of unmaintained `sevenz-rust`. Default features: aes256, bzip2, compress, ppmd, util. Public API is archive-level (`ArchiveReader`, `BlockDecoder`, `decompress_file*`); no documented "decode this folder from a raw byte prefix" entry point. Use the per-codec decoders directly (see below). |
| lzma-rust2 | 0.25.0 | Pure Rust (optional `optimization` feature uses unsafe, guarded; off = 100% safe) | Apache-2.0 | 2026-11-01 | LZMA, LZMA2, LZIP, XZ. Port of tukaani xz-for-java. | Read-based decoders (`LzmaReader`, `Lzma2Reader`). Supports no_std. Used by sevenz-rust2. Prefix behaviour UNVERIFIED. |
| lzma-rs | 0.3.0 | Pure Rust | MIT | 2023-01-04 (stale) | LZMA, LZMA2, XZ | Very widely downloaded (~44M) but unmaintained since 2023. Streaming API exists. Prefer lzma-rust2. |
| xz2 | 0.1.7 | FFI (liblzma via lzma-sys) | MIT/Apache-2.0 | 2022-06 | LZMA, LZMA2, XZ | Stale. Only if FFI is acceptable. |
| ppmd-rust | 1.5.2 | Pure Rust (port of 7-Zip Ppmd7/Ppmd8 C code) | CC0-1.0 OR MIT-0 | 2026-10-21 | PPMd var.H (`Ppmd7`, 7z variant), PPMd7a carryless coder (.pmd), PPMd8 (PPMdI rev.1, zip). | README says: caller must bound output with known size (read_exact) or end marker, otherwise garbage symbols appear at the end. Not documented as RAR-PPMd-compatible. RAR 3.x PPMd uses variant H with its own range-coder/escape handling; compatibility UNVERIFIED. |
| rars | 0.12.3 | Pure Rust (clean-room-style project, see legal section) | Apache-2.0 | 2026-10-26 | RAR 1.3, 2.0, 2.9 (`codec/rar29`), 3.x, 5.0 (`codec/rar50`), 7 (format name `rar70`). Includes `codec/ppmd.rs`, `codec/rarvm.rs` (RAR VM programs), `filter.rs` (Delta, E8, E8E9, ARM for RAR5; RAR 2.9 filters per format), `streaming.rs` (writer side), `member_read.rs`. Encryption feature (`encryption`: aes, hmac, sha1, sha2, zeroize). | Author says: "a bit slower than WinRAR, uses more memory, slightly worse compression, could use more testing at volume". Self-described "agentic development experiment". Has a separate spec repo (bitplane/rar-research). Reader is archive/member-level (`read_members_at`, `read_volume_members_at`), with `ArchiveReadOptions` (password, output limits). Streaming partial-input behaviour UNVERIFIED. Strongest candidate for RAR, but young. |
| rar | 0.4.0 | Pure Rust | MIT | 2025-11-05 | RAR5 only in practice. | README feature table: RAR5 single/multi/split/encrypted and SAVE only (FASTEST..BEST unchecked). RAR4 entirely unchecked. Only 10k downloads. Not viable for RAR4 or RAR5 with compression. |
| unrar | 0.5.8 | FFI (bundled C++ UnRAR source via build.rs/cc) | MIT OR Apache-2.0 (crate); the bundled UnRAR code is under the UnRAR license | 2025-02-19 | RAR 2.x, 3.x, 4, 5 (whatever the bundled C++ supports). | Only prefix-friendly path if the C++ handles partial volumes: UNVERIFIED. Owner's "widespread" bar is met (770k downloads). License is the restrictive part: see legal section. Not "pure Rust". |
| unrar-ng | 0.7.7 | FFI (fork of unrar, bundled UnRAR C++) | MIT OR Apache-2.0 (crate) | 2026-05-07 | Same as unrar. | "Actively maintained fork of unrar". Still FFI. 36k downloads. |
| deflate64 | 0.1.12 | Pure Rust (port of .NET managed inflate) | MIT | 2026-03-23 | Deflate64 only. | Read-based. Made for zip files with Deflate64 from Windows. Prefix behaviour UNVERIFIED. |
| flate2 | 1.1.10 | Pure Rust by default (miniz_oxide backend); zlib-rs or libz-sys selectable | MIT OR Apache-2.0 | 2026-08-28 | DEFLATE, zlib, gzip. | Read/BufRead streams; standard for Deflate. Plain Deflate (RAR uses its own Huffman-based LZ, not Deflate; 7z DEFLATE = raw deflate). |
| miniz_oxide | 0.9.1 | Pure Rust | MIT OR Zlib OR Apache-2.0 | 2026-03-13 | DEFLATE. | Lower-level backend for flate2. |
| bzip2 | 0.6.1 | Bindings to libbzip2 API; default backend is pure Rust (libbz2-rs-sys) | MIT OR Apache-2.0 | 2025-10-16 | BZip2 Reader/Writer. | Backend pure-Rust default: UNVERIFIED (check Cargo features). Prefix behaviour UNVERIFIED. |
| ruzstd | 0.9.1 | Pure Rust decoder only | MIT | 2026-10-11 | Zstd decode. | Streaming decoder available (`StreamingDecoder`/`FrameDecoder`): UNVERIFIED for truncated-input output flushing. Zstd is not used in RAR. Only relevant for 7z zstd forks (e.g. 7-Zip-zstd). |
| zstd | 0.14.3 | FFI (libzstd) | BSD-3-Clause | 2026-11-01 | Zstd enc/dec. | Standard but FFI. |
| aes | 0.9.3 | Pure Rust (RustCrypto) | MIT OR Apache-2.0 | 2026-08-28 | AES block cipher. | Use with `cbc` (RustCrypto, version UNVERIFIED) for 7z AES-256-CBC and RAR AES-256-CBC. CBC is decryptable block-by-block, so a prefix of ciphertext yields a prefix of plaintext (padding only at the very end). |
| pbkdf2 | 0.13.0 | Pure Rust | MIT OR Apache-2.0 | 2026-04-21 | PBKDF2. | RAR5 KDF = PBKDF2-HMAC-SHA256 with 2^n iterations (n from header, default 15 = 32768): UNVERIFIED exact parameters, check RAR5 spec. |
| sha2 | 0.11.1 | Pure Rust | MIT OR Apache-2.0 | 2026-10-12 | SHA-2. | 7z AES KDF = iterated SHA-256 over (salt, UTF-16LE password, 8-byte counter), 2^NumCyclesPower rounds (default 19): UNVERIFIED against the 7z spec. RAR4 KDF = iterated SHA-1 (0x40000 rounds) over password + salt + counter: UNVERIFIED. |

## Per-codec recommendation

Legend: "Prefix" = can yield first M output bytes from first N compressed bytes without the rest (UNVERIFIED unless stated).

| Codec | Best crate | Pure Rust? | Prefix decode? | Risk |
|---|---|---|---|---|
| RAR 2.9/3.x (unp_ver 29) LZ + Huffman | rars (`codec/rar29`) | Yes | UNVERIFIED. Expected yes at member level if the decoder takes an arbitrary `Read`. Needs a test on a truncated real archive. | Medium. Young crate, self-described as needing more volume testing. Fallback: unrar (FFI). |
| RAR 3.x PPMd (variant H, RAR-specific coder) | rars (`codec/ppmd.rs`) | Yes | UNVERIFIED | Medium. ppmd-rust is NOT documented as RAR-compatible (7z/PPMd7 coder differs). |
| RAR 2.9/3.x RAR VM filters (E8, E8E9, Delta, ARM, plus RAR VM programs) | rars (`codec/rarvm.rs`, `filter.rs`) | Yes | UNVERIFIED | High for exotic RAR VM programs. E8/E8E9/Delta/ARM covered per source, but not tested. |
| RAR5 (v50) LZ + Huffman, solid | rars (`codec/rar50`), rar (SAVE only) | Yes (rars) | UNVERIFIED | Medium. rar crate does not handle compressed RAR5. |
| RAR5 filters (E8/E8E9/Delta/ARM) | rars (`filter.rs`, `FilterKind`) | Yes | UNVERIFIED | Medium. Source shows Delta, E8, E8E9, ARM for RAR5. |
| RAR7 (format name rar70 in rars) | rars 0.12.3 | Yes | UNVERIFIED | High. README claims RAR 7 coverage, but RAR 7 behaviour is not independently verified here. |
| RAR solid mode | rars: `ArchiveMember::is_solid`, `read_members_at`. Solid blocks must be decoded from the start of the solid run. | Yes | Partial by nature: a solid member's first file is decodable from the start of the run only. A prefix of a non-first solid member is NOT decodable without earlier members. | Structural limit, not a crate issue. Metadata of the first file in a solid run is reachable from the front of the first volume. |
| LZMA (7z) | lzma-rust2 (`LzmaReader`) or sevenz-rust2 | Yes | Expected yes: decoder pulls input bytes as needed. UNVERIFIED. | Low-Medium. |
| LZMA2 (7z) | lzma-rust2 (`Lzma2Reader`) | Yes | Expected yes, chunked. UNVERIFIED. | Low-Medium. |
| BCJ x86 / ARM / ARM64 / etc. (7z) | sevenz-rust2 filters | Yes | Expected yes (streaming filters). UNVERIFIED. | Low. |
| BCJ2 (7z, 4-stream) | sevenz-rust2 (decode only) | Yes | UNVERIFIED. Needs all 4 input streams to be prefixes of their own streams; in a partial 7z the call/jump streams may be unavailable. | High for partial archives. |
| PPMd H var (7z, PPMd7) | ppmd-rust (or sevenz-rust2 `ppmd` feature) | Yes | UNVERIFIED. Needs the known uncompressed size for bounding (per README). Can cut output at size M. | Low-Medium. |
| Deflate (7z) | flate2 (miniz_oxide backend) | Yes | Yes, in the sense that flate2 emits output as far as input allows and returns UnexpectedEof at the cut. Output already written is valid. UNVERIFIED for the exact flush behaviour. | Low. |
| Deflate64 (zip, some 7z) | deflate64 | Yes | UNVERIFIED | Low-Medium. Small crate (68k downloads). |
| BZip2 (7z) | bzip2 (0.6, pure-Rust backend UNVERIFIED) or sevenz-rust2 `bzip2` feature | Likely yes | UNVERIFIED. BZip2 works on 900k blocks, so a truncated block may yield nothing until the block is complete. | Medium. Prefix may be empty for small M. |
| Zstd (7z zstd fork only) | ruzstd (pure) or zstd (FFI) | ruzstd yes | UNVERIFIED | Low relevance (RAR has no zstd). |
| AES-256 (7z) | aes + cbc + sha2 (sevenz-rust2 `aes256` feature) | Yes | CBC decrypts block-by-block. Prefix yes. Key derivation needs the password. | Low-Medium. Not decrypting the outer archive without a password is expected. |
| AES-256 (RAR5) | rars `encryption` (aes, hmac, sha2) | Yes | Yes for CBC body. KDF = PBKDF2-HMAC-SHA256 (pbkdf2 crate). UNVERIFIED parameters. | Medium. |
| AES-256 (RAR4) | rars `encryption` (sha1) | Yes | UNVERIFIED | Medium. Key derivation is SHA-1-based, slow. |
| WinZip AES | none in this list | | | Not checked. |

## Practical design options

1. Pure-Rust-first: rars for RAR2.9/3.x/5/7 (Apache-2.0, pure, young), sevenz-rust2 for 7z (LZMA, LZMA2, PPMd, BCJ, AES, bzip2, deflate). Verify prefix behaviour with a truncated fixture before relying on it.
2. Mixed: keep rars for RAR and use lzma-rust2 + ppmd-rust directly for 7z streams. Avoids the full 7z archive reader, which needs the header at the end of the file (a partial volume does not have it). Pass the folder's packed bytes straight to the codec.
3. FFI fallback for RAR only: unrar or unrar-ng (C++ UnRAR). Mature, widely used, but FFI and under the UnRAR license.

Important: the 7z archive header normally sits at the END of the file. A partial download (first 1-6 articles) will not have it. Metadata extraction must therefore decode the packed stream from a known offset (32 bytes into the file, plus pack position from a header or a guess). Either the coder parameters must be known from elsewhere, or the header must be recovered. This is an architecture issue, not a crate issue. UNVERIFIED for any specific archive layout.

For RAR, the file header is at the start of each volume, so the compressed data for the first file in volume 1 is reachable from the front. This is the main reason RAR is the easier target.

## Legal and practical: porting RAR decompression

- UnRAR license (verified, text fetched from https://raw.githubusercontent.com/aawc/unrar/main/license.txt): the UnRAR source may be used "in any software to handle RAR archives without limitations free of charge", but it "cannot be used to develop RAR (WinRAR) compatible archiver and to re-create RAR compression algorithm, which is proprietary". Modified source may be distributed if the full license paragraph is included. The license is not an OSI open-source license; it is a freeware license with a no-compressor clause.
- Reading that clause literally: a decoder that handles RAR archives is within the "use in any software to handle RAR archives" permission. A decoder that is a line-by-line port of the UnRAR C++ source is a derivative work. Distributing it must carry the license text. Writing a RAR compressor is out of bounds. This is an interpretation, not legal advice; the owner should get a real legal review before shipping a port of the UnRAR source.
- The safer route (what rars says it did): clean-room implementation from published format documentation and observed behaviour, not from the UnRAR source. rars cites a separate spec repository (bitplane/rar-research). The Apache-2.0 license of rars does not itself carry the UnRAR restriction, but its provenance (clean-room or not) is not verified here: UNVERIFIED.
- Practical: the owner's "widespread crate" bar favours sevenz-rust2 / lzma-rust2 / flate2 / sha2 / aes. For RAR, no pure-Rust crate meets the widespread bar. rars is the most complete pure-Rust RAR reader found (RAR 1.3 through 7). rar (0.4.0) covers only RAR5 SAVE. unrar and unrar-ng are widespread but FFI.
- Recommendation: treat rars as a dependency candidate, with a pinned version and a fixture test suite (RAR 2.9, 3.x PPMd, 5 solid, 5 filters, encrypted). If the owner wants no dependency on a 0.x crate, a rewrite from the spec is the fallback. The crate's own "could use more testing at volume" caveat applies.

## Recommendation table

| Codec | Best crate | Prefix decode | Risk |
|---|---|---|---|
| RAR 2.9/3.x LZ | rars 0.12.3 (`rar29`) | UNVERIFIED (likely yes) | Medium |
| RAR 3.x PPMd H | rars (`ppmd`) | UNVERIFIED | Medium |
| RAR5 LZ, solid | rars (`rar50`) | UNVERIFIED | Medium |
| RAR5 / RAR 2.9 filters (E8, E8E9, Delta, ARM) | rars (`filter.rs`, `rarvm.rs`) | UNVERIFIED | Medium-High |
| RAR7 | rars 0.12.3 | UNVERIFIED | High |
| RAR (C++ fallback) | unrar 0.5.8 or unrar-ng 0.7.7 (FFI) | UNVERIFIED | Medium (FFI, UnRAR license) |
| LZMA | lzma-rust2 0.25.0 | Expected yes (UNVERIFIED) | Low |
| LZMA2 | lzma-rust2 0.25.0 | Expected yes (UNVERIFIED) | Low |
| PPMd H (7z) | ppmd-rust 1.5.2 | UNVERIFIED | Low-Medium |
| BCJ x86 and other filters | sevenz-rust2 0.24.2 | Expected yes (UNVERIFIED) | Low |
| BCJ2 | sevenz-rust2 | UNVERIFIED, all 4 streams needed | High for partial |
| BZip2 | bzip2 0.6.1 or sevenz-rust2 feature | UNVERIFIED, block-granular | Medium |
| Deflate | flate2 1.1.10 | Yes (output up to cut is valid, UNVERIFIED) | Low |
| Deflate64 | deflate64 0.1.12 | UNVERIFIED | Low-Medium |
| Zstd (7z fork) | ruzstd 0.9.1 | UNVERIFIED | Low relevance |
| AES-256 CBC (7z, RAR5) | aes 0.9.3 + cbc + sha2 / pbkdf2 | Yes (CBC is block-streaming) | Low-Medium |
| RAR4 SHA-1 KDF | rars `encryption` | UNVERIFIED | Medium |

## Open items (not verified, need testing)

- Whether each decoder emits partial output at EOF or buffers a whole block first (BZip2 and RAR blocks in particular).
- rars RAR7 correctness.
- rars PPMd variant H for RAR 3.x, against real archives.
- Exact KDF parameters for RAR5 (PBKDF2 iteration count encoding), RAR4, and 7z AES, against spec documents.
- Whether bzip2 0.6.1 uses the pure-Rust backend by default.
- Whether the `cbc` crate version is current.
- WinZip AES was not checked.

## Sources

- https://crates.io/api/v1/crates/sevenz-rust2 (metadata) and https://crates.io/api/v1/crates/sevenz-rust2/0.24.2/readme (README, fetched)
- https://static.crates.io/crates/sevenz-rust2/sevenz-rust2-0.24.2.crate (Cargo.toml features, lib.rs exports, fetched)
- https://crates.io/api/v1/crates/lzma-rust2 and /0.25.0/readme (README, fetched)
- https://crates.io/api/v1/crates/lzma-rs (metadata, stale 2023)
- https://crates.io/api/v1/crates/ppmd-rust and /1.5.2/readme (README, fetched)
- https://crates.io/api/v1/crates/rars and /0.12.3/readme; https://static.crates.io/crates/rars/rars-0.12.3.crate (src/streaming.rs, src/codec/, src/filter.rs, src/lib.rs, fetched)
- https://github.com/bitplane/rars (project home, spec repo referenced: https://github.com/bitplane/rar-research)
- https://crates.io/api/v1/crates/rar and /0.4.0/readme (fetched)
- https://crates.io/api/v1/crates/unrar (metadata, 0.5.8)
- https://crates.io/api/v1/crates/unrar-ng and /0.7.7/readme (fetched)
- https://raw.githubusercontent.com/aawc/unrar/main/license.txt (UnRAR license text, fetched)
- https://crates.io/api/v1/crates/deflate64 and /0.1.12/readme (fetched)
- https://crates.io/api/v1/crates/flate2, miniz_oxide, bzip2, ruzstd, zstd, aes, pbkdf2, sha2, xz2 (metadata only)
- https://crates.io/api/v1/crates/lzma-rs (metadata)
