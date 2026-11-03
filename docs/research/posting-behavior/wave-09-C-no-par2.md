# Wave 9 C: linking without (complete) PAR2

## 1. Prevalence
**Tool defaults.**
- ngPost: `--par2_pct` "0 by default meaning NO par2 generation" (README). The shipped `ngPost.conf` sets `PAR2_PCT = 8` (sample-config users get PAR2).
- Nyuu: has no built-in PAR2. ParPar "integration" is still on the to-do list, so PAR2 only exists if the user runs ParPar first.
- postie: `par2: enabled: true, redundancy: '10%'`, plus `wait_for_par2: true`.
- NewsUP runs `par2 -r15` by default (log.md).

**Measured.** segs.tsv has no subjects. Its column 5 is the poster, so a `.par2` grep there only hits poster names; I matched 7 NZB ids that way, which says nothing about PAR2. I re-fetched the 1,989 NZBs in `server-ids/list.txt` from GitHub: 252 unique NZBs with `<file>` entries, close to wave 6's 261. Subjects showing `.par2`:

| subset | n | PAR2 visible |
|---|---|---|
| all | 252 | 120 (48%) |
| ≥3 files | 150 | 115 (77%) |
| ≥100 segments | 90 | 65 (72%) |
| contain RAR | 127 | 83 (65%) |
| real (not test/fixture), names visible, ≥20 segs | 20 | 16 (80%) |
| fully obfuscated names | 69 | 4 (unknown: PAR2 names are hashed too) |
| PAR2 index but no vol files | 19 | – |

Real posts without PAR2: `[PRiVATE]-[WtFnZb]` single-MKV posts (one file, sometimes plus an .nfo), an old "Posted By Rom" RAR set, and sanderjo's deliberate "rar with sfv no par2" reference NZBs.

Estimate: roughly 15–30% of multi-file uploads have no PAR2; single-file posts usually have none. Partial DMCA removal often hits the PAR2 articles (wave 5), adding "PAR2 listed but unreadable" cases. I found no published indexer statistic. Bias: the GitHub corpus is mostly test fixtures.

## 2. Fallback signals
B = bodies per file. A file is assumed to be ~100–150 articles of about 700 KB each.

| signal | needs | B | proves (same file / same volume set / order) |
|---|---|---|---|
| `=ybegin part= total= size=` + `=ypart begin end` | 1 body (any article) | 1 per run | same file: yes (size + total), and `begin` places the article within the file. Set: no. Order: no |
| X-Received-Body-CRC | HEAD/HDR, only on providers that add it (wave-05-C) | 0 | per-article dedup only |
| `pcrc32` per article, folded with crc32_combine | every BODY | ~100+ | whole-file CRC32, matchable to another copy, Nyuu `=yend crc32` (ngPost writes pcrc32 only), PAR2 or SFV. Set/order: no |
| `=yend crc32` (Nyuu, last part only) | last-article BODY | 1 | whole-file CRC32 for one body: repost key |
| RAR4 MHD/FHD flags | first article BODY | 1 | MHD_VOLUME / MHD_FIRSTVOLUME; FHD SPLIT_BEFORE/AFTER; ENDARC "next volume" + volume number. Gives set membership via the inner file name plus HOST_OS/time/attrib, and the order index. With `-hp`, only MHD stays clear (first-volume flag) |
| RAR5 main header | first article BODY | 1 | volume flag + **volume number** (order), and inner file name/mtime for the set. With `-hp`, the headers are encrypted. The set is then probably linked only by the encryption-header salt/check (needs verification). Order is lost |
| RAR last volume | first article of the last volume | 1 (included above) | file header carries the full inner-file CRC32 (RAR4) or hash (RAR5) → the "same set" check closes. No visible "next volume" flag marks the end |
| 7z `.001…` split | first article of `.001` | 1 for the set | signature header gives next-header offset + size, so total archive size = Σ(volume sizes) (a subset-sum key). Later volumes are raw bytes: equal size, no index, so order comes only from time/size |
| zip central directory | last 1–2 articles of the last piece | 1–2 | inner names, CRCs, sizes, disk numbers (.z01): set + order |
| SFV | 1 small file, 1 body | 1 per post | CRC32 per volume; match folded pcrc32 or `=yend crc32` → file + set; order from line order |
| NFO | 1 body | 1 | release-name fingerprint; same post (weak) |

**Cost to order and link a RAR set without PAR2:** about 1 body per volume (the first article), plus the cheap header read already used for runs. That is under 1% of articles. Folding pcrc32 needs about 100% of bodies.

## 3. pcrc32 vs PAR2 slice CRCs
This is consistent. Folding article pcrc32 with crc32_combine gives the whole-file CRC32. Folding the IFSC slice CRCs gives the same value, once the last slice's zero padding is removed. PAR2 stores the padded CRC; the padding is a known linear map, so the unpadded value can be derived.

So yes, linking can work from article CRCs alone **if Atlas already downloads every body**. It is a costlier version of what one small PAR2 index gives: length, hash16k (1 body) and IFSC.

When article size equals slice size (common Nyuu/ParPar setups, e.g. 700K), each pcrc32 equals one slice CRC and places the article. pcrc32 confirms on the download path; it does not discover.

## Recommended fallback order
1. **PAR2 index** (when present, even with vol files missing): sizes, hash16k, IFSC.
2. **yEnc headers per run** (already required): size/total/begin.
3. **First article of each run → container header.** RAR volume number/flags or 7z `.001` header. Last volume: inner CRC. About 1 B/file. This replaces hash16k for RAR sets without PAR2.
4. **SFV/zip CD/NFO** if they are in the post (1–2 bodies).
5. **Last article `=yend crc32`** (Nyuu) as a repost/identity key.
6. **X-Received-Body-CRC** for per-article dedup at zero body cost, where available.
7. **Full pcrc32 fold**, only for posts Atlas already downloads.

If `-hp` RAR5, no PAR2 and equal volume sizes all apply, use timing/size chains (wave 7: 65–80%) and flag low confidence.

## Sources
- https://github.com/mbruel/ngPost (README `--par2_pct`; ngPost.conf `PAR2_PCT = 8`; src/nntp/NntpArticle.cpp `=yend size pcrc32`)
- https://github.com/animetosho/Nyuu (README: ParPar not integrated; lib/article.js `crc32=` on last part via crc32_combine)
- https://github.com/kipsilabs/postie/blob/main/config-example.yaml (`par2.enabled: true`)
- https://github.com/sabnzbd/sabnzbd/blob/develop/sabnzbd/deobfuscate_filenames.py (PAR2-based rename, size-based fallback)
- https://github.com/sanderjo/NZBs (reftest NZBs with/without PAR2)
- RAR 5.0 format: https://www.rarlab.com/technote.htm
