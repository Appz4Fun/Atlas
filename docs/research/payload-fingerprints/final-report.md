# Payload Linking Map

Payload and content fingerprints visible from yEnc parts: what each one proves, what it costs to fetch, and whether Atlas collects it. Merged from 10 waves of 3 research agents plus findings shared by the sibling threads.

## 0. How to read this

Five link classes: **TWIN** (byte-identical post, including crossposts with separate NZBs), **ALT** (same release or inner file in another container, including a re-RAR with a new mtime), **SAME FILM** (frame or audio match only), **SAME UPLOADER**, **POSTING TOOL**. Cost tiers: **0** = NZB, XOVER or headers; **0*** = the `=yend` line of bodies Atlas already fetches; **1** = one article; **2** = a few articles; **3** = whole file; **X** = external oracle.

Atlas column: from a read-only review of an Atlas `src/` copy by the archive-metadata thread (`research/archive-metadata/raw/w9a-atlas-gaps.md`), so it may lag main. **stored** = persisted, **parsed** = read then dropped, **partial** = some fields kept, **no** = not read, **?** = not checked.

## Linking policy (after the wave 10 critic)

No single key should merge posts on its own. Every key below has a known way to merge wrongly.

| Key | Use it as | Rule |
|---|---|---|
| Folded pcrc32 / PAR2 IFSC whole-file CRC | TWIN candidate | Give no key if any part lacks pcrc32 (it is optional in yEnc 1.3). Merge only on size + CRC32 + a 16k or full MD5. CRC32 alone collides about 1.16e6 times at 1e8 files, and same-size buckets (round volume sizes, padded images) still collide. PAR2 only confirms. |
| PAR2 set ID, recovery slices | TWIN confirm | High miss rate (slice size depends on the upload and the whole file set), low false-merge rate. |
| PAR2 file ID | TWIN confirm | Breaks on renamed or obfuscated names. Key on (16k MD5, length) without the name instead, plus a full hash. |
| Inner CRC + size from RAR, 7z, ZIP | ALT | Exclude RAR5 -p/-hp and ZIP AE-2. Count only main payload files of 50 MB or more, and require two matching inner files or a second hash. |
| Encrypted size tuple (V, n, S_last) | Blocking only | Candidate list; needs a sample's exact size or a decrypted key to confirm. Useless on fixed-cap disc images. |
| NZB escape-count index | TWIN candidate | Only NZBs whose segment sizes vary (not ngPost, not rebuilt NZBs). |
| srrDB, predb, Anime Tosho, KEYDB | Corroborate | Cache with provenance; respect robots and rate limits. |

Anti-linking practices seen in the wild: random names per article (postie full, ngPost UUID subjects, Nyuu-obfuscated), ngPost `--gen_name` + `--gen_pass`, and changing volume, slice or article sizes per upload. Payload hashes survive all of these except a fresh password. No source shows random padding or junk files per upload.

## Built and tested in pure Rust this session

| Prototype | Result | Path |
|---|---|---|
| CRC32 GF(2) toolkit (std only, 320 lines) | 8/8 property tests incl. a 5e9-byte direct check; ~400 ns per op at n≈1e10; folds a 10 GB file in ~0.3 ms | `opus-tools/w9b_crc32gf2/` |
| PAR2 + RAR4/RAR5 partial-buffer walker (std only, 339 lines, own MD5/CRC32) | 332/332 field checks vs the rarfile `.exp` corpus, identical on 700 KB prefixes; 6 entries missing (header-encrypted by design, RAR3 packed Unicode names). Last volume's first article gives full CRC + size | `opus-tools/w10b_walker/` |
| Python models | exact RAR5 -hp size model (33/33), size-key simulations, re-RAR and 7z experiments, NZB segment index | `opus-tools/w*_*.py` |

## 1. Bottom line

Most posts can be linked as twins or alts without downloading payload. The yEnc part CRCs (pcrc32) we already get with each body fold into a whole-file CRC32. One PAR2 index article gives every file's length, full MD5, 16k MD5 and slice CRCs. One article from the start of the last RAR volume gives the inner file's (name, size, CRC32). With the pure-Rust GF(2) toolkit (`w9b_crc32gf2`, tested past 4 GiB) these keys join across different article sizes, slice sizes and repacks. Unencrypted RAR and 7z are deterministic: a re-RAR that changes only the mtime leaves every middle article byte-identical, and a change of name or `-v` still links with 2-4 decoded articles. Encrypted reposts (RAR5 `-hp`, 7z `-mhe`) hide all content keys, but their volume sizes come from an exact, password-independent model (33/33 on real rar runs). The tuple (V, n, S_last) is a ~27-bit blocking key on encoded video. It needs a second exact size (sample, second file) to reach twin-safe odds, and it collapses to 2-18 bits on fixed-cap disc images, where the BD/DVD disc IDs take over. Size alone is only a filter. CRC32 alone is never a link. Only full MD5+length, per-creation random fields, or several matching part/interval CRCs that include header-bearing articles should create twin edges.

## 2. Matrix: what it proves × cost tier

| Proves \ Tier | 0 (NZB/XOVER/headers) | 1 (one article) | 2 (a few articles) | 3 (many / whole) | X (oracle) |
|---|---|---|---|---|---|
| **TWIN** | Folded pcrc32 → whole-file (size, CRC32) [body CRC lines, see note]; NZB escape-count segment-size index (Nyuu-style NZBs); exact segment-size + pcrc32 + offset per part; SFV CRC = yEnc crc32= of each volume; -hp/-mhe (V, n, S_last) + companion exact size (blocking → twin-candidate) | PAR2 FileDesc full MD5 + length; PAR2 16k MD5 + length (candidate); PAR2 exp-0 recovery slice (whole set, rename-proof); PAR2 Recovery Set ID (same names + slice size); RAR5 -hp crypt header salt (+PswCheck); RAR5 file-encryption salt+IV; RAR4 8-byte salt; 7z start header (NextHeaderOffset/Size/CRC); 7z AES IV; ZIP AES salt; AACS Unit_Key_RO.inf SHA-1; Content000.cer ID; UDF Volume Set ID; id.bdmv Org/Disc ID | PAR2 IFSC per-slice MD5+CRC32 (partial / per-file in packs); PAR2 IFSC fold ↔ pcrc32 fold bind; CRC-interval match on LCM grid (different article sizes); first+last article of each volume (separates twin from mtime re-RAR); ContentHash000.tbl segment check (~3 art.); sample-sha256-v1 (start/mid/end, unarchived files) | Full-file MD5/SHA/ed2k/BTv2/TTH | srrDB archive-crc / rarhash (exact scene volumes); Anime Tosho crc32k+size → full hashes; bitmagnet v1 piece hash |
| **ALT** | Total release size from NZB yEnc sizes (filter); -hp exact size model / forward check of a named candidate; middle-article pcrc32/encoded-size equality across mtime re-RARs; pack containment on per-file (len) lists | RAR last-volume file header (name, unpacked size, full CRC32); single-volume RAR / `zip -0` local header CRC+size; RAR non-last chunk CRC; RAR5 QO cached headers (pack listing, large files); 7z start header packPos (exact packed total under -mhe); raw-split .001 magic bytes + .crc sidecar; MKV SegmentUID; BDMV/IFO metadata hash; DVDDiscID (MD5 of IFOs); Windows dvdid CRC64 | 7z tail header (names, sizes, per-file CRC32; readable under -p without -mhe); ZIP central directory; RAR4 multi-file volume-start headers; CRC-algebra inner CRC of store-mode RAR/7z from pcrc32 + header bytes; SRS match_offset + 256 B signature; pack vs single containment (len,CRC32)/(len,md5_16k); RAR5 PswCheck + harvested password → decrypt head, unlock inner keys; FLAC STREAMINFO MD5; EAC log Copy CRC | Re-hash after full unpack | srrDB archive-size / archive-crc / isdbhash / store-real-crc / sample-crc; predb sizes; FindVUK KEYDB (AACS disc ID → title); No-Intro/Redump DATs; MusicBrainz/AccurateRip/CTDB via EAC TOC |
| **SAME FILM** | – | Codec config + first keyframe hash (stream thread); OSDb/moviehash (first + last article) | First-IRAP hash across MKV/MP4/TS; NFO normalized SHA-256 / simhash (same release text) | pHash frames, Chromaprint audio (stream thread) | OpenSubtitles moviehash API; srrDB isdbhash |
| **SAME UPLOADER** | Password-reuse inference from NZB meta; reuploader suffix tags | RAR5 PswCheck hit against harvested-password table; RAR5 UOWNER/uid/gid; host OS + 7z Unix mode (umask); inner mtime provenance (unrar-restored mtime) | NFO ANSI-art block hash; marker files (name + CRC32, e.g. RARBG.com.txt); extra non-scene files in a set | – | Spotnet X-User-Key (labelled) |
| **POSTING TOOL** | Article size (716800/750000/768000); slice size from PAR2 vol sizes in NZB; -hp detector: last volume size ≡ 14 mod 16; -v size of non-last volumes; NZB segment bytes == nominal (ngPost) | RAR4 vs RAR5 sig; RAR flags (RR, -k, solid, locator); RAR5 compression-info bits (rar ≥7.0); host OS; 7z version bytes 00 04 vs 00 03; ZIP made-by / extras; archive name length (17/21/22 chars) | PAR2 Creator string (par2cmdline/-turbo, ParPar+arch, par2j, QuickPar); PAR2 packet order / redundancy; RecvSlic exponent layout; 7z header property order + kDummy padding | – | – |

Note on tier 0 for pcrc32: pcrc32 sits on the `=yend` line inside each article body, so it is "free" only because Atlas already fetches bodies. Overview `:bytes`/`:lines` is the true header-only pre-filter (Haiku-15 W5 correction).

## 3. Signal inventory

| Signal | Proves | Cost tier | Strength (bits / false-match rate where measured) | Breaks when | Evidence (script or source) | Atlas |
|---|---|---|---|---|---|---|
| yEnc pcrc32 fold → whole-file CRC32 (+ size) | TWIN candidate (file) | 0* (body =yend lines) | 32 bits + size; (size,CRC32) 0 collisions in 445k real files; ~1-120 chance pairs at 1e8 files | Any byte change; missing parts (use interval match); bad decoder (yenc 0.2.2 hasher bug) | w9b_crc32gf2 `fold_yenc_parts`; haiku15/tools/slice_vs_article.py; w5b_size_uniqueness.py || no: pcrc32 not stored per article |
| CRC-interval match on LCM(article sizes) grid | TWIN (partial/incomplete posts, different article sizes) | 0* / 2 if front trimming needed | Each 10.75 MB interval ~32 bits; 4/4 intervals matched, 1-bit change dropped exactly one | Byte shift (different header length) unless trimmed with 2 decoded articles per endpoint | haiku15/tools/crc_interval.py; w9b `slices_from_parts`; w6a_exp.py || no |
| PAR2 IFSC slice CRC fold ↔ pcrc32 fold | TWIN candidate (file); binds a PAR2 set to a post with no PAR2 | 1 (PAR2 index) + pcrc32 | 32 bits + length; verified equal to zlib whole-file CRC | Slice size not a multiple of article size → only LCM grid (large files) | w9b `fold_par2_slices` (unpads last slice); haiku15/tools/slice_vs_article.py || no: IFSC not parsed |
| PAR2 FileDesc full MD5 + length | TWIN (file), rename-proof | 1 | 128 bits; twin-grade | No PAR2 in post (ngPost default off) | haiku/tools/par2_ifsc_compare.py; PAR2 spec || stored |
| PAR2 16k MD5 + length | TWIN candidate | 1 | Collides on RAR volumes from one packer (identical headers) | Same as above; ambiguous if seen twice in one post | NNTmux par_hashes prior art; SABnzbd rule || stored |
| PAR2 IFSC per-slice MD5+CRC32 runs | TWIN partial / per-file in packs | 1-2 | 128+32 bits per slice | Different slice size; identical whether file alone, renamed, or with junk files | haiku/tools/par2_ifsc_compare.py || no |
| PAR2 exp-0 recovery slice | TWIN (whole file set), rename/order/tool-proof | 1 (first RecvSlic of smallest vol) | Slice-size bytes; exact | Different slice size; any added/removed file (par2cmdline slice = ceil(total/2000)→x4 over all files); junk file joins set | haiku/tools/par2_recovery_compare.py (par2cmdline 0.8.1 vs ParPar 0.4.6) || no |
| PAR2 Recovery Set ID / File ID | TWIN of set with same names + slice size | 1 | 128 bits | Any rename (File ID = MD5(16k‖len‖name)); new slice size; PAR3 IDs random | par2cmdline descriptionpacket.cpp ComputeFileId || stored |
| PAR2 Creator / packet order / exponent layout / slice size | POSTING TOOL | 0 (slice from NZB vol sizes) / 1 | Annotate only | – | w9c_tool_tells.md P1-P7 || partial: slice size stored, Creator dropped |
| RAR last-volume file header (name, unpacked size, full CRC32) | ALT (inner file across re-RARs, 7z, raw) | 1 (first article of last volume) | 32 bits + size (~23-27 bits for video) | RAR5 -p: CRC HMACed with key; -hp: header encrypted; -htb BLAKE2sp; RAR4 CRC 0xffffffff = absent | w4a: r5.py, r4.py, r3.py on libarchive/rarfile/sharpcompress samples; w5a confirmed with rar 6.23/7.12 || partial: name + size stored, CRC dropped, no tail read |
| RAR non-last chunk CRC32 | TWIN of a volume / CRC-algebra input | 1 per volume | 32 bits | Different -v | w4a, w5a_volsum.py || partial: first article's file CRC in probe JSON |
| Inner CRC via CRC algebra (store-mode RAR/7z) | ALT verification | 2 (art. 1 tail + last art. head per volume) | Exact | -m>0; -p/-hp; -rr shifts data_end; multi-file chains | w2a identities vs zlib; w6a_exp.py; w9b `remove_prefix/remove_suffix` || no |
| Re-RAR determinism (middle-article pcrc32 / encoded-size equality) | ALT (mtime re-RAR); TWIN needs header articles too | 0* | Mtime +123 s: 46/62 pcrc equal (exactly first+last of each of 8 vols differ); rar 7.12 vs 6.23: 62/62 identical | Sub-second mtime (+7/+8 B header) shifts all bytes; different name length or -v (link via intervals, 2-4 articles); any password | w6a_exp.py, w5a_fd.py || no |
| RAR5 Quick Open record | ALT (pack listing) | 1 (last article of a volume) | Byte-identical cached headers | Caches only files with >4096 B data per volume; scene mostly RAR4 (no QO) | qo.py, qoenc.py; w7a_hp_model.py (E term) || no |
| RAR5 -hp crypt header (Lg2Count, 16 B salt, PswCheck, csum) | TWIN (same creation; groups random-named volumes of one set) | 1 | 128-bit salt; exact | Every new rar run (fresh salt) | r5.py; archive-metadata thread (HEAD_CRYPT identical across volumes, rars writer; WinRAR unverified) || stored: KDF, salt, check |
| RAR5 file-encryption salt+IV; RAR4 8-byte salt | TWIN | 1 | 128 / 64 bits | New run | rarlab technote; rardecode archive50.go || ? |
| RAR5 PswCheck vs harvested-password table | SAME UPLOADER; unlocks ALT keys | 1 + 1 KDF (2^15 PBKDF2-SHA256) per candidate | 64-bit check | ngPost GEN_PASS fresh per post; password unknown | w3a; unrar crypt5.cpp; r5.py (-hp decrypt) || partial: password check implemented |
| -hp detector: last volume size ≡ 14 mod 16 | POSTING TOOL (-hp used) | 0 | Strong, not absolute (one counterexample ≡10) | -p only, RAR4, QO edge cases | w5a, w6b_hp_model.py || no |
| -hp / -mhe exact size model (V, n, S_last) | ALT/TWIN blocking key for encrypted reposts | 0 | ~27.5 bits (p=5.3e-9/pair), ~2.6e5 false pairs at 1e7 posts; + companion exact size p=2.8e-17; real Redump sizes: 8.3 bits pooled, 22.9 with ±1 MB jitter, 27 exact-dups removed | Different -v; tsx 0/7 mismatch (~half of name lengths); fixed-cap disc sizes (GC/Wii ~2.3 bits); -rr | w7a_hp_model.py (33/33 exact), w6b_hp_model.py, w8a_real.sh + w8a_real_output.txt, w8a_sim.py, w8a_pitfalls.py, w9a_real_sizes.py || no |
| -hp forward check of a named candidate (srrDB/predb name + size) | ALT (confirms inner size to the byte) | 0 + X | Rejects all wrong sizes within ±1 MiB; ~9% FP from same-step name lengths | Unknown name length L | w6b_hp_model.py, w6b_calib.py || no |
| 7z start header (NextHeaderOffset/Size/CRC, bytes 12-31) | TWIN (per archive) | 1 | 32-bit CRC + offsets; always clear, even -mhe | mtime/version change alters it (72 B / 67 B) | w7c_parse.py, w7c_hdr.py, w7c_diff.py || parsed |
| 7z packPos under -p -mhe | ALT blocking key (exact packed total = Σ roundup16(file)) | 1 | Like size key | Different file set | w7c_parse.py || ? |
| 7z -p without -mhe: plaintext tail header | ALT (links passworded 7z to plain posts) | 1-2 (last article + LZMA decode) | Per-file CRC32 + sizes + names | -mhe=on (ngPost 7z mode) | w7c_parse.py, w7c_diff.py || partial: names + sizes, CRCs discarded |
| 7z AES IV (no salt by default) | TWIN | 1 | 128 bits | New run | archive-metadata thread (7zAes.cpp) || no |
| 7z data region (32..packPos) = raw concatenation | TWIN/ALT across 7z version, mtime, -v | 0* (middle pcrc32) | 60/62 pcrc equal on mtime/name change | Compression >0; encryption | w6a_7z.py, w7c_diff.py || no |
| ZIP local header CRC/size (`zip -0`), central directory | ALT | 1 / 2 (last 1-2 articles) | 32 bits + size | Data descriptor (Go, unseekable Python); AE-2 stores 0 | Haiku-15 W7 test (Info-ZIP 3.0) || partial: names + sizes, CRC never read |
| NZB escape-count segment-size index (k=8 middle parts, m≥2) | TWIN / mtime re-RAR ALT | 0 | ~8.6 bits/part on random/compressed; k=8 → ~69 bits; ~8e-3 false/day at 1e8 stored + 1e7/day; recall 1.0 reposts & mtime re-RARs, 0.927 at 20% missing | ngPost NZBs (bytes = nominal 716800); low-entropy content (text 0.67 bits/part; gate Rényi-2 <4 bits/part); any byte shift | w7b_segindex.py; header thread real repost check (15/15 segment byte counts on 5 files, 19 h apart) || no |
| Exact file size alone | Candidate filter | 0 | ≥100 MB free-form: 80-97% unique; fixed-capacity outputs 0-66%; <1 MB useless; collisions are same-content variants | Fixed media caps, DVD5/BD25 targets | w5b_size_uniqueness.py; w9a_real_sizes.py || stored |
| (size, CRC32) | Strong TWIN/ALT candidate | 0*-1 | 0 collisions in 445k real files | Adversarial CRC forging (4 bytes) | w5b_size_uniqueness.py || partial: sizes only |
| SFV (name, CRC32) set | TWIN of volumes (zero decode vs yEnc crc32=) | 1 | 32 bits per volume | ReScene CRC padding attacks; renames | w4c || parsed: first 3 small text files |
| SRS sample (match_offset + 256 B signature) | ALT confirmation (near-zero FP) | 2 (SRS + one candidate article) | ~2048 bits | Re-encode | pyReScene resample/main.py (w4c) || no |
| NFO raw CRC32 / normalized SHA-256 / simhash | TWIN (raw) / same release text; ANSI art = group | 1 | – | Edited NFO; no indexer dedupes on NFO | w4c || ? |
| EAC log TOC → DiscID/AccurateRip/CTDB; log Copy CRC; FLAC STREAMINFO MD5 | ALT (same rip / PCM across FLAC re-encodes) | 1-2 | PCM hash, 128-bit MD5 | Lossy transcode | w4c; cdtoc crate || ? |
| MKV SegmentUID | ALT/TWIN (same mux) | 1 | 128 bits | Remux | NNTmux fixNamesWithMedia prior art || ? |
| Pack vs single containment (per-file (len,CRC32)/(len,md5_16k), byte-weighted) | ALT (episode in season pack) | 1-2 per file | Pack eps byte-identical to scene singles | Sidecars <1 MB; Jaccard instead of containment | w4b; cross-seed compareFileTreesPartial, seasonpackarr || ? |
| BD/DVD: BDMV/IFO metadata hash, DVDDiscID, dvdid CRC64 | ALT (ISO vs folder vs decrypted) | 1-3 | MD5 / CRC64 | RipGuard stripping rewrites IFOs | w8c; libdvdread || no |
| AACS Unit_Key_RO.inf SHA-1, Content000.cer ID, UDF Volume Set ID, id.bdmv | TWIN / same pressing; KEYDB title oracle | 1 (+X) | 160 / 48 bits | Decrypted rips may drop AACS dir | w8c || no |
| ContentHash000.tbl (64-bit SHA-1 per 192 KiB of encrypted m2ts) | TWIN segment check, works on incomplete posts | 2 (~3 articles) | 64 bits per unit | Decrypted m2ts | w8c || no |
| Raw split (.001) magic + TC/Krusader .crc sidecar | ALT vs unsplit/RAR posts | 1 | CRC32 + size | – | Haiku W6 || ? |
| Sampled hashes: moviehash/OSDb (head+tail), imohash, AltMount sample-sha256-v1 | Candidate (head+tail) / TWIN (with mid sample) | 2-3 | – | Container rewrite | Haiku W7; AltMount docs || partial: OSDb moviehash (#36) |
| srrDB archive-crc / archive-size / rarhash / isdbhash / sample-crc / store-real-crc | ALT/TWIN to scene original; names obfuscated reposts | 1 + X | One live archive-size query returned exactly one release | -hp hides CRC; no bulk dump; /api/ robots-disallowed; rate | w2b live test; Haiku W9 (w9b.md) || no: no SRR parser |
| predb sizes / files | ALT candidate | X | Whole-MB sizes in nZEDb dumps (weak) | – | w3c || stored: PreDB sources |
| Anime Tosho dbexport (crc32k + size → btih, ed2k, BTv2, stored NZB) | ALT/TWIN bridge to torrents; labelled set | 1 + X | Exact | Anime only; last update 2025-01-17 | w2b, w5c || no |
| FindVUK KEYDB, No-Intro/Redump DATs, NSRL/CIRCL (negative filter) | Title oracle / ROM twin / known-software filter | X | – | Coverage | w3c, w8c || no |
| bitmagnet v1 piece hash + size | ALT bridge to torrents | 2-3 + X (self-hosted) | 160-bit piece | Piece not fully inside file; pieces only saved with SavePieces | w5c || no |
| Inner mtime + stored time fields | SAME source copy (re-pack provenance) | 1 | – | P2P sources: mtime = download time | Haiku W3/W4 || no: dropped |
| RAR5 UOWNER, host OS, 7z Unix mode | SAME UPLOADER / machine | 1 | Annotate | – | Haiku W3; w9c R3/R8 || no: dropped |
| Marker files (RARBG.com.txt etc.), extra non-scene files, reuploader suffixes | SAME UPLOADER / re-packer | 0-1 | Name + CRC32 needed (sizes overlap) | – | Haiku W6/W8 || ? |
| Archive tool tells (RAR4/5 sig, flags, compression-info bits ≥7.0, 7z 00 04 vs 00 03, property order, ZIP made-by, name length 17/21/22) | POSTING TOOL | 1-2 | Annotate | 6.x vs 7.x indistinguishable at -m0 | w9c_tool_tells.md || no: dropped |

0* = needs the body's `=yend` line, which Atlas already fetches; not overview-only.

## 4. Measured results

| Result | Number | Script |
|---|---|---|
| -hp last volume size mod 16 | ≡ 14 in all runs, rar 6.23 and 7.12, any -m; one counterexample ≡ 10 (L=110, V=225954) | w5a_volsum.py, w6b_hp_model.py |
| Non-last volume size | exactly the -v size (5m = 5,242,880) in every mode | w5a_volsum.py |
| RAR5 -hp size model | 33/33 exact (H, D, n, S_last formula incl. QO term E = 112+H when data >4096 B) | w7a_hp_model.py |
| -hp single-post candidates | ~272 (unknown name length); two posts intersect to ~16.6 mean, never unique | w6b_hp_model.py |
| Password independence | identical sizes for 1/24/80-char passwords (rar -hp, 7z -mhe, 7z -mx5) | w8a_real.sh, w8a_real_output.txt |
| (V,n,S_last) strength | ~27.5 bits, p=5.3e-9; + companion exact size p=2.8e-17 | w8a_sim.py, w8a_pitfalls.py |
| Same key on real Redump sizes (36,215 images ≥100 MB) | 8.3 bits pooled (44% share an exact size); 27 bits with exact dups removed; 22.9 bits with ±1 MB jitter; GC/Wii ~2.3, X360 4, PS1-3/PSP 13-18 bits; nfo adds ~14 bits; sample size 0 false pairs in 6.6e8 | w9a_real_sizes.py, w9a_real_output.txt |
| Size uniqueness | (size,CRC32) 0 collisions in 445k; size alone 80-97% unique ≥100 MB free-form | w5b_size_uniqueness.py |
| Re-RAR (40 MB, -v5000k, 62 parts of 716800) | mtime +123 s: 46/62 pcrc equal, 59-61/62 encoded sizes; sub-second mtime 0/62; -v4200k 4/56; -v10000k 6/58; 70-char name 0/62; rar 7.12 vs 6.23 62/62 | w6a_exp.py |
| CRC-interval linking | all 12 variant pairs linked; 0 decoded articles (mtime/version), 2 (name/sub-second/-v), 4 across a volume boundary | w6a_exp.py |
| 7z -mx0 | mtime/name change: 60/62 pcrc equal (only first+last of whole set); different -v = identical stream; 16.02 vs 24.09 differ by 67 B in headers only | w6a_7z.py, w7c_diff.py |
| Escape-count index | 8.6 bits/part random/compressed, 0.67 text; equal-size prob 0.31%; recall 1.0 (repost, mtime re-RAR), 0.927 at 20% missing | w7b_segindex.py |
| Interval match across article sizes | 716800 vs 768000: 4/4 LCM intervals (10,752,000 B) matched; 1-bit change drops one | haiku15/tools/crc_interval.py |
| pcrc32 fold = IFSC fold = whole-file CRC | equal on test files | haiku15/tools/slice_vs_article.py; w9b_crc32gf2 tests |
| PAR2 exp-0 slice | identical across renames, order, set IDs, par2cmdline 0.8.1 vs ParPar 0.4.6 | haiku/tools/par2_recovery_compare.py |
| PAR2 IFSC/FileDesc | identical alone, renamed, or with a junk file; exp-0 changes once junk joins | haiku/tools/par2_ifsc_compare.py |
| CRC32 GF(2) toolkit speed | combine 368 ns, remove_suffix 465 ns, zero_unpad 397 ns at n≈1e10; fold of 10 GB: 217 µs (PAR2 slices), 342 µs (yEnc parts); 8/8 tests incl. >4 GiB direct check | w9b_crc32gf2/test_output.txt |
| RAR split CRC layout | non-last header = chunk CRC; last = full unpacked CRC (RAR4 and RAR5) | r3.py, r4.py, r5.py, w5a_fd.py |

## 5. Rust build notes

**Crates to use (pure Rust):**
- `crc32fast` 1.5: `Hasher::combine` and `new_with_initial_len` are public, but multiply/x8n are private. Use our own `w9b_crc32gf2` crate instead (std only, 320-line lib.rs: `combine`, `remove_prefix`, `remove_suffix` via x^-8n with XINV=(POLY<<1)|1, `zero_extend`/`zero_unpad`, `crc_zeros`, `fold_yenc_parts`, `fold_par2_slices`, `slices_from_parts`). Keep crc32fast as a dev-dependency oracle. Path: `research/payload-fingerprints/opus-tools/w9b_crc32gf2/`.
- RustCrypto, one trait generation: `md-5`, `sha1`, `sha2`, `md4` 0.11; `hmac`, `pbkdf2` 0.13; `aes` 0.9; `cbc` 0.2; `blake2` (for -htb).
- `lzma-rust2` pinned at 0.23 (to match `zip`/`sevenz-rust2`) for 7z encoded headers.
- `zip` 9.2 with `default-features = false` (the default pulls in C zstd).
- `xxhash-rust` for persisted keys (never `ahash`; it is not stable across builds).
- `cdtoc` 0.13 for EAC TOC → DiscID/AccurateRip/CTDB.
- `petgraph` UnionFind or a 40-line DSU. Store every edge with its evidence and treat cluster ids as a cache, because union-find cannot split.

**Write in-house:**
- PAR2 packet scanner (partial buffers, MD5-checked packets) plus GF16 only if recovery is needed.
- RAR4/RAR5 header walker with a hand-rolled PBKDF2 loop for Key / HashKey (+16) / PswCheck (+32), and -hp header decrypt.
- 7z start header and EncodedHeader parser plus the AES KDF (2^19).
- ZIP local header / AES extra.
- Truncation-tolerant yEnc decoder that keeps escape counts, decodes like rapidyenc, and never keys a part whose CRC failed.
- -hp/-mhe size model (port w7a_hp_model.py).
- Segment-size LSM index (w7b design).
- OSDb, ed2k, BTv2 and CD-TOC IDs, and simhash.

Write every parser as a tri-state parser (NeedMore / Invalid / Parsed) with checked arithmetic and caps on declared sizes before allocating (rardecode CVE-2025-11579), and fuzz each header parser with cargo-fuzz.

**Avoid / flag:** `unrar`/`unrar_sys` (C++ FFI), `unrar-rs` (UnRAR licence), `xz2`/liblzma (C), `discid` (C). `nzbdav-rar` has no allocation cap and drops the inner CRC. `sevenz-rust2` cannot parse a 32-byte prefix. `rars` 0.12 is Apache-2.0 but niche.

**Test corpora:**
- rarfile `test/files`: 106 RAR3/RAR5 files with `.exp` oracles, including multivolume, -hp and QO.
- libarchive `.uu` fixtures (BSD-2).
- par2cmdline `tests/*.tar.gz`: GPL, so fetch them at test time and do not vendor.
- Generate our own yEnc, PAR2, 7z and RAR fixtures with rar 6.23/7.12 (archive.ubuntu.com multiverse).

## 6. Open questions / not verified

1. **RAR5 -p CRC (resolved on paper):** both earlier notes agree. Without the password the stored value is useless. With the password, HashKey comes from the same PBKDF2 run, so any candidate CRC (from SFV, yEnc or PAR2) can be checked with one HMAC, and the plain CRC can be found outright with a 2^32 search (minutes on one core). Still untested with a real archive.
2. **-hp multivolume detector edge cases:** ≡14 mod 16 failed once (≡10). It is unknown how the QO term E depends on L and on V mod 16 outside the measured grid. The detector is untested on WinRAR (Windows) output.
3. **RAR5 -hp salt across volumes:** the archive-metadata thread reports HEAD_CRYPT as byte-identical across all volumes (rars writer). This is unverified for WinRAR and rarlab rar.
4. **Size key on real video:** the clustering measurement used Redump disc images, not video. The rate of fixed-target encodes (DVD5/BD25 fits) among Usenet video is unmeasured. We also do not know how often a companion exact-size file exists in real posts.
5. **Escape-count index:** validated only on synthetic data plus one real repost pair (15/15). Precision and recall on real NZB volume are unmeasured. It does not work on ngPost NZBs, and the share of Nyuu-style NZBs is unknown.
6. **PAR2 prevalence:** there is no statistic on the share of posts with a parseable FileDesc. MultiPar/par2j, QuickPar and par2cmdline-turbo byte identity of the exp-0 slice is unverified.
7. **srrDB:** `archive-size` keyword syntax and rate limits are unverified, `rarhash` computation from live headers is untested, and the API is robots-disallowed, so we should contact the operator before bulk use. predb sizes are whole-MB only.
8. **Twin vs mtime re-RAR policy:** middle-article equality means only "same bytes there". A twin verdict needs header-bearing articles or a per-creation field, and this rule is not yet encoded or tested against a labelled set. Gold-set candidates: Anime Tosho dbexport, Spotnet spots.
9. **Backbone body integrity:** no source shows body rewriting, but nobody has run a two-provider fetch of the same Message-IDs to confirm.
10. **Disc IDs:** MAKEMKV/discatt.dat is unknown. We have not checked how often decrypted BD rips keep the AACS directory. Windows dvdid works only on ISOs.
11. **Adversarial:** CRC32 is forgeable with 4 bytes, and ReScene documents padding RAR volumes to a target CRC. Never treat CRC-only agreement as a twin edge. The W10C critic covers real counterexamples.
12. **Header-only vs body cost:** pcrc32 needs the body. It is still open whether Atlas's existing body fetches cover every part, or only a sample.
