# W10-C adversarial critique of payload-fingerprint keys

Rule applied: the indexer acts automatically, so any key that can collide across different payloads is downgraded to "candidate, needs confirmation".

## 1. Folded yEnc pcrc32 to whole-file CRC32, matched to folded PAR2 IFSC CRCs — risk MED (as TWIN key), HIGH if used alone
- Nyuu's full option list has no switch for the trailer CRC fields. Line size (default 128) and article size (default 700K) are configurable, so the encoded bytes vary between posts while the decoded payload stays the same. That is harmless for decoded-CRC keys but breaks any key built on encoded bytes. yEnc 1.3 makes `crc32=` optional in multipart `=yend`. Decoders treat a missing pcrc32 as a normal case (see the error and decode paths of the yencoding crate). The fold therefore has gaps: when any part has no pcrc32, give the post no key. Never substitute a guessed value.
- CRC32 alone: by the birthday bound, ~1e8 files gives about 1.16e6 colliding pairs at 2^32 (n^2/2^33) if you match on CRC only. CRC+exact size removes nearly all of them, but many files share a size: fixed-size RAR volumes, padded disc images, and 50/100 MB volumes. Within one size bucket of ~1e6 files, the expected number of colliding pairs is about 116. Mitigation: the key is (size, CRC32, PAR2 16k-MD5 or a second independent value). Require two independent hashes before merging.
- PAR2 may describe different files than the ones posted (re-RAR, re-PAR, files renamed before the PAR step). Use a PAR match only to confirm a TWIN that the yEnc fold already found, never as its sole source.

## 2. PAR2 set ID / file ID / 16k-MD5 / recovery slice — risk: set ID HIGH-miss/LOW-false, file ID MED, 16k-MD5 MED
- ParPar requires `-s`, and documents that par2cmdline's default equals `-s2000` (2000 slices over all inputs) while MultiPar's default is `-s0.5w*10`. The same file therefore gets different slice sizes from different tools, and gets a different main packet when the other files in the set change. Set ID and recovery slices are per-upload, not per-payload. Use them only for TWIN, and expect misses.
- File ID = MD5(16k-MD5 || length || name). Obfuscating uploaders such as postie, the "Nyuu-obfuscated" fork and ngPost `--gen_name` randomize names, so file IDs differ for identical bytes. Drop the name and key on (16k-MD5, length) instead.
- 16k-MD5 + length is a weak payload key on its own. Files with identical headers (container headers, padding, sparse ISOs) share the first 16 KiB. Pair it with full CRC or full MD5.

## 3. Inner CRC32 + size from RAR/7z/ZIP headers (ALT) — risk HIGH without filters
- With RAR5 `-hp`, the stored CRC is an HMAC-converted value, not the real CRC (rar-stream rar5 crypto module; WinRAR's description of RAR5 encryption). Exclude it or flag it as encrypted.
- ZIP AE-2 stores CRC=0, so exclude it.
- False ALT merges on small common files (nfo, srt, sfv, sample, proof jpg, `.nfo` templates) are certain. Use a whitelist instead: only the main payload, at least 50 MB, and never sample/proof files.
- Padded images (44% of Redump sizes are shared) leave the CRC as the only discriminator. Require the CRC plus a second hash, or at least two matching inner files.

## 4. Encrypted RAR5 (V, n, S_last) + companion size — risk HIGH false merge
- ngPost `--rar_size`, `--gen_pass` and autoposters use round volume sizes, so V is low-entropy. n and S_last come from payload size plus header overhead. Two different files of near-equal size inside identical rar settings give the same tuple when the names have equal length. Use it as a candidate only. Never auto-merge on it.

## 5. NZB escape-count k=8 — risk MED-HIGH
- NZB `bytes` attributes are optional in the DTD and are tool-generated (SABnzbd forum). ngPost writes the constant 716800, Pan's export is buggy, and indexers rebuild NZBs. Also, line-size and article-size settings change escape placement and per-segment byte counts for the same payload, so the key matches only uploads made with identical posting settings. Accept only Nyuu-style NZBs with a verified variance profile, and reject constant or near-constant bytes series.

## 6. External oracles — risk MED
- srrDB, predb and Anime Tosho data are community-maintained and can be wrong or incomplete. Rate limits and robots rules may apply (not verified via proxy). KEYDB and FindVUK are legally sensitive, may be taken down, and do not identify payloads. Use them as corroboration only, cache them with a provenance field, and never let one alone trigger a merge.

## Anti-linking practices and the keys they defeat
- Random subject, yEnc name and poster per article (postie "full", ngPost UUID subjects, Nyuu-obfuscated): defeats name-based keys and PAR2 file ID. Payload keys survive.
- Random RAR name plus random password per upload (ngPost `--gen_name`/`--gen_pass`): defeats PAR2 set ID, TWIN-by-volume keys and, with `-hp`, the inner CRC. Only key 4 (sizes) partly survives.
- Different volume size, slice size or redundancy per upload: defeats the PAR2 set ID, recovery-slice keys and key 4.
- Different article or line size per upload: defeats key 5. The decoded-CRC fold survives.
- Obfuscated inner MKV names (Newsbin forum): defeats name keys. The inner CRC survives if the archive is unencrypted.
- No source found for per-upload random padding or junk files. If someone does it, it defeats all whole-file keys and leaves only inner-file ALT.

## Sources
- https://raw.githubusercontent.com/animetosho/Nyuu/HEAD/help-full.txt
- https://github.com/xompage/Nyuu-obfuscated
- https://pkg.go.dev/github.com/javi11/postie@v0.0.3
- https://raw.githubusercontent.com/mbruel/ngPost/master/README.md
- https://cdn.jsdelivr.net/npm/@animetosho/parpar@0.4.5/help.txt
- https://docs.rs/rar-stream/5.2.2/src/rar_stream/crypto/rar5.rs.html
- https://www.win-rar.com/encryption-winrar-improved-rar-5.html
- https://docs.rs/crate/yencoding/0.2.0/source/src/error.rs
- https://forums.sabnzbd.org/viewtopic.php?p=32135
- https://gitlab.gnome.org/GNOME/pan/-/issues/76
- https://forums.newsbin.com/viewtopic.php?p=230168
