# W3A per-creation random fields (Haiku), pass 1
- RAR5 -hp archive encryption header (type 4, right after 8-byte sig): KDF count, salt 16 RANDOM, check 12 (derived). First article.
- RAR5 file encryption extra 0x01: salt 16 + IV 16 RANDOM, check 12. First article for first file.
- RAR5 BLAKE2sp hash extra 0x02: 32-byte content hash (deterministic) = strong alt key across RAR5 posts, rename-proof.
- RAR4 LHD_SALT 0x0400: 8-byte random salt per file (offset unverified).
- 7z: sig header 32B: StartHeaderCRC@8, NextHeaderOffset@12, NextHeaderSize@20, NextHeaderCRC@28 (deterministic but archive-unique). AES props: salt/IV random; with -mhe at tail.
- ZIP AES: salt 8/12/16 random right after local header. First article.
- PAR3 InputSetID: effectively random per set.
- MKV SegmentUUID 0x73A4 16B random (Info, first KB). Preservation on remux unverified.
- MP4 mvhd creation_time (moov may be at tail).
Insight: random fields separate "same upload reposted" (twin) from "independent re-creation of same content" (alt) — matching content keys but different salts = alt/re-pack.
## pass 2
- mkvmerge: random SegmentUID per output unless --segment-uid given. SegmentFamily 0x4444 shared across linked segments; Prev/NextUUID link splits.
- RAR4 (nwaples/rardecode archive15.go): salt 8B after filename when 0x0400; then EXT_TIME when 0x1000.
- RAR5 (archive50.go): KDF exp 1B (<=24), salt 16, check 8+4 (SHA-256 prefix).
- -hp prevalence: anecdotal ~6 encrypted of 500 movies/10k eps (reddit); obfuscation >> encryption.
- 7z StartHeaderCRC covers 0x0C-0x1F; NextHeaderCRC covers header incl mtimes -> creation-unique without encryption, weaker than a salt.
- Good Go reference parser: github.com/nwaples/rardecode.
