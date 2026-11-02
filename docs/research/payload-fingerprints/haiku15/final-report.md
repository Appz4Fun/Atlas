# Payload fingerprints: final report

Atlas research, 2 Nov 2026. This report covers 10 waves of 3 Haiku agents on the payload angle: what the yEnc parts and the file bytes they carry can prove about two posts. Per-wave notes are in `research/payload-fingerprints/haiku15/log.md` and `raw/`. Shared findings are in `research/linking-findings.md` under "Haiku 15-min research: payload fingerprints".

## Headline

- **No existing tool links Usenet posts across NZBs by content.** Downloaders use the same checksums only to verify or rename files. Linking by content is new work for Atlas.
- **The yEnc part CRCs already give a full twin proof, and they compose.** pcrc32 is the CRC32 of the part's decoded bytes (checked in NZBGet, sabctools, Nyuu and yencode, and re-checked with zlib). CRC32 is linear, so part CRCs fold into the whole-file CRC32 and into CRCs of any shared interval. That works even when two uploads used different article sizes. Tested in `tools/crc_interval.py`.
- **One PAR2 article gives the same key with no payload.** Folding the IFSC slice CRCs, with the last slice un-padded, gives the whole-file CRC32, and it equals the folded article CRCs. Tested in `tools/slice_vs_article.py`, which confirms the method the catalog thread found in sabnzbd. This places equal-size RAR volumes on the right file, and it links twins when only one side carries PAR2.
- **Twin and alt can be told apart from bytes.** Per-creation random fields (archive salts, MKV SegmentUUID) separate "the same upload posted again" from "the same content packed again". Inner-file CRCs survive a repack, so they are the alt key.

## Cost tiers

| Tier | What has to be fetched |
| --- | --- |
| T0 | Overview or HDR only, no bodies |
| T1 | The first article of a file |
| T2 | The last article of a file |
| T3 | One PAR2 article (the index, or any volume carrying the critical packets) |
| T4 | Every article body of a file (needed for pcrc32 on all parts) |

## Signal map

Status codes: **yes** means Atlas stores and uses it. **part** means Atlas stores or parses it but doesn't use it as a link key. **no** means not collected. **?** means not checked. Atlas status comes from the catalog thread's check against main at a29ee52.

| Signal | Proves | Cost | Atlas | Notes |
| --- | --- | --- | --- | --- |
| Whole-file CRC32 from folded pcrc32 + exact size | twin candidate | T4 | no | Atlas verifies pcrc32 on the 1 to 3 probed articles and stores none. At 1e8 files about 1 to 120 chance pairs share (size, CRC32), so it is a blocking key, never a link alone. |
| CRC interval keys on the LCM grid (4 or more matching intervals) | twin, including partial twins | T4 | no | Different article sizes still compare at LCM points, e.g. 10.75 MB for 716,800 vs 768,000. Four intervals give 128 bits. Tested. |
| PAR2 FileDesc full MD5 + length | twin, per file | T3 | part | Unaffected by renames. Use it in place of File ID and Recovery Set ID, which both hash the stored name. |
| PAR2 MD5-16k + length | twin candidate, name map | T3 + T1 | part | Matches article 1 when it holds 16 KiB or more. |
| PAR2 IFSC fold to whole-file CRC32 | twin candidate per file, needs no bodies | T3 | no | Matches the folded pcrc32 of another upload. Solves the equal-size volume case. Tested. |
| PAR2 Recovery Set ID | same PAR2 set | T3 | yes | Changes on rename or a new slice size, so it is not a twin key. |
| Exponent-0 recovery slice (XOR of all slices) | twin when slice size matches | one recovery article | no | par2cmdline and ParPar both start at exponent 0. The default par2cmdline slice is ceil(total/2000) rounded to a multiple of 4 over ALL files, so one extra file in the set breaks this key (from the other payload thread). |
| Archive per-creation salt (RAR5 -hp salt, RAR5 file salt/IV, RAR4 salt, ZIP AES salt) | twin (same build), or alt when the salt differs but content keys match | T1 | no | Random each time an archive is made, so a match means the same archive build. |
| Unencrypted RAR rebuilt from the same files | same bytes in the middle of each volume | T4 | no | RAR output is deterministic. A change in mtime alone touches only the first and last part of each volume. A middle-span match is alt evidence, not a twin. |
| Inner-file CRC32 + size (RAR last volume, 7z end header, ZIP central directory) | alt (same inner file in any container) | T2. T1 for single-volume RAR and `zip -0` | part | Non-last RAR volumes hold only their chunk CRC. 7z needs an LZMA decode of the tail. |
| RAR5 BLAKE2sp of the inner file | alt, stronger key | T2 | no | Stored when RAR5 uses BLAKE2 (`-htb`). |
| Encrypted inner checksums (RAR5 tweaked, ZIP AE-2) | nothing | n/a | n/a | RAR5 tweaked checksums depend on the key. AE-2 stores CRC 0. RAR4 and ZIP AE-1 keep plain CRCs. |
| MKV SegmentUUID | same mux (twin-grade with a size match) | T1 when stored | no | The stream thread found that a new UUID with the same encoder settings means a remux. |
| Volume size profile (count, size, last size) | blocking only | T0 | ? | About 27.5 bits (from the other payload thread). Holds for encrypted reposts, but it collapses for fixed-size disc images (44% of Redump images share an exact size). Add one exact file size for a stronger block. |
| X-Received-Body-CRC per article | twin candidate per article, no bodies | T0 via HDR, Highwinds-family providers | no | Verified by the yEnc-headers thread. It is a CRC of the body text, not of decoded bytes, so it differs from pcrc32. |
| PAR2 Creator string and packet order | posting tool | T3 | no | `ParPar v0.4.5 x64 [url]` has an arch token. par2cmdline writes "Created by par2cmdline version X." and puts Creator last. |
| RAR host OS, UNP_VER, 7z UNIX_EXTENSION flag | tool or platform | T1 | no | Annotation only. |
| yEnc trailer style (pcrc32 case and padding, crc32= on the last part only) | posting tool | any body | ? | Nyuu writes crc32= on the last part only. ngPost doesn't pad. GoPostStuff uses uppercase. |

## Linking policy

1. **Only twin-grade evidence creates a twin link:** full MD5 + length, 4 or more matching interval CRCs, or the same per-creation salt plus a size match.
2. **Candidate keys only nominate pairs:** (size, CRC32), MD5-16k, X-Received-Body-CRC and the volume size profile. A second, independent signal promotes them.
3. **Alt links come from inner-file keys with a different archive build:** inner CRC or BLAKE2sp matches, and the salt differs or the volume layout differs.
4. **Tool and uploader signals only annotate.** They never link on their own.
5. **Twins are grouped and never collapsed**, in line with the project rule. Store every edge with its evidence (key type, files, offsets). Keep the cluster ID as a cache, because union-find can't split. On a retraction, recompute that component by BFS over the stored edges.
6. **Raise an alarm on cluster size, low density (chains) and bridge edges.** These are the Splink cluster metrics.

## Implementation notes

- **Decoding:** follow rapidyenc, which NZBGet and sabctools use. A part counts as verified only when its CRC matches. Never key unverified parts.
- **Parsing:** write our own tri-state prefix parsers (NeedMore, Invalid or Parsed) with checked arithmetic and caps before allocation. The integrity checks work on a prefix: RAR4 HEAD_CRC16, RAR5 block CRC32, 7z StartHeaderCRC, PAR2 packet MD5. The existing Rust crates can't parse partial data or have no allocation cap.
- **CRC math:** crc32fast 1.5 has `combine`. Un-padding and power-of-x need a small GF(2) helper, about 30 lines, as in the test scripts. The Opus payload thread has built and tested a std-only Rust version (combine, prefix and suffix removal, zero unpad, yEnc and PAR2 folds, about 0.3 ms per 10 GB file) in its opus-tools/w9b_crc32gf2/.
- **Fetching:** pipelined range OVER over 4 to 8 connections with COMPRESS DEFLATE. Fetch body-tier keys once per file and cache them. Order by value: the PAR2 article first, then article 1, then the last article, then all bodies.
- **Storage at 1e8 files:** about 20 to 30 bytes per key entry. Interval keys can reach 1e9 entries or more, so store them only for files larger than the grid and only for article-size pairs actually seen. Use append-only sorted runs with a sparse index, plus a static binary fuse filter per run (about 9 bits per key at 0.4%). Union-find takes 4 to 8 bytes per node, which is 400 to 800 MB memory-mapped.

## Measuring false links before trusting them

- Build a synthetic corpus offline. Nyuu has no dry-run mode (only `--dump-failed-posts`), so write our own yEnc article writer and use par2cmdline or ParPar (`-s` sets the slice size). The rarfile project's test/files (106 RAR3/RAR5 archives with .exp expected outputs) make a ready parser test set. Apply perturbations: re-pack, rename, a different article size, a one-byte change, and an mtime-only change.
- Report pairwise and cluster precision and recall per key type. On real data, estimate precision from sampled clusters (arXiv 2404.05622 ratio estimator) or use a Wilson interval on sampled pairs.
- Atlas has to count the share of posts with a parseable PAR2 FileDesc itself, because no public figure exists. ngPost makes no PAR2 by default. ParPar and Nyuu write an index by default.

## Open gaps

- Whether every recovery volume carries a full FileDesc and IFSC set: par2cmdline interleaves copies, but this wasn't verified end to end.
- The Creator strings for MultiPar, QuickPar and par2cmdline-turbo, and the RAR5 host OS and version fields, are unverified. Read the unrar `headers.hpp` before keying on them.
- How NZBGet's quick par check uses article CRCs: the source couldn't be fetched.
- Real-data rates: how often PAR2 is present, how often the salt fields are present, and collision counts at Atlas scale. These need a read-only sample run.

## Suggested build order

1. Store pcrc32 and the size of every fetched article, and fold them to the file CRC32. This is cheap, and every later key needs it.
2. Parse the PAR2 FileDesc and IFSC fully, and store MD5 + length and the folded CRC32 per file.
3. Probe article 1 for salts and the SegmentUUID, and the last article for inner-file CRCs.
4. Build the edge table with evidence and the cluster cache, add the alarms, then run the synthetic precision test.
5. Add interval grid keys last, for partial twins.
