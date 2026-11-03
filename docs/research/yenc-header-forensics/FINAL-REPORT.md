# Linking binary posts from yEnc and NNTP headers: final report (Haiku research, 10 waves)

Thread: yEnc and NNTP header forensics. Waves 1-10 ran 06:45Z to 11:40Z on 2026-11-02, with Haiku subagents and an Opus orchestrator.
Detail: `wave01.md` to `wave10.md`, `raw/`, the full tool table `fingerprints.md`, the Rust spec `raw/w08-c-rust-spec.md`, and the classifier script `tools/msgid-classifier/`.

## The short version
1. **The Message-ID usually names the posting tool, from overview data alone.** On 72,334 real segments from 246 NZBs, ordered regex rules label about 86% of segments. The Rust spec is validated: it matches the Python classifier on every label and has 10 real test vectors.
2. **Server-assigned Message-IDs are a session key.** When a poster sends no Message-ID, the server makes one. Highwinds IDs `T1.N$K.R@fxNN.site` fully decode: T1 is the injection second, N is a front-end article counter, and K is one value per upload session. (host, K, T1 window, N range) groups every file of one upload, across groups and obfuscation, from XOVER.
3. **Nyuu IDs carry the posting time in ms**, which matches the Date header (99.56% within ±1 day of the NZB date). This orders parts and dates sessions for free.
4. **Retries mint new IDs in Nyuu and ngPost, but not in postie.** The NZB keeps only the ID that succeeded. A duplicate segment number with different bytes is therefore a repost or merge, not a retry.
5. **Obfuscating posters rotate From per file**, so From alone fails to join a release. Join on the server session key (Highwinds K) or Nyuu ID stream continuity. Time + size shape alone is weak: the Opus yEnc thread measured precision 0.27-0.42 when uploads overlap, so use it only to generate candidates.
6. **X-Received-Body-CRC** (solved by a sister thread) is a HEAD-only byte-twin key for the same tool and split.

## Signals map

Cost key:
- **overview** = XOVER/OVER fields (Subject, From, Date, Message-ID, Bytes, Lines, Xref)
- **HEAD** = full headers
- **edge** = the first and last body lines (=ybegin, =ypart, =yend)
- **body** = the full article

| Signal | Proves | Cost | Notes |
|---|---|---|---|
| Message-ID grammar (Nyuu `([A-Z][a-z]){12}-\d{13}@nyuu`, ngPost `32hex@ngPost`, newsmangler `%.5f.%d@host`, postie `{32}@{8}.{3}`, yEncBin `Part N of M.HEX32@ts.local`, JBinUp `{19,20}_NoM@JBinUp.local`, PowerPost, GoPostStuff `$gps@gopoststuff`, ...) | posting tool | overview | ~86% coverage on the corpus. NewsUP --obfuscate imitates other tools, so check exact lengths and case (spec rules). |
| Nyuu ID ms timestamp | same session (time order); posting time | overview | Equals the Date header. Nyuu redraws the 24 letters on each retry; only the ms survives. |
| Highwinds server ID `T1.N$K.R@fxNN.site` | same upload session | overview | T1 is LE base-64, `0-9a-zA-Z_%`, + 876645500 → unix seconds (0 decreases in 862 pairs). N is a front-end counter that continues across files, with 0-43% other-user gaps. K is one per post. R is random. |
| Other server-ID families (INN `radix32(t-673416000)$pid$count`, Diablo `%08lx$seq$pid$hash`, Giganews base64 XOR 0x9D, Usenet.Farm `[A-Za-z]{32}@UFK`, `-13digits@PRiVATE`) | provider; injection order; tool sends no Message-ID | overview | Shows that the tool defers to the server (sanguinews, newspost, newsmangler, nntpPoster, and Nyuu when the server returns one). |
| Date header form (`-0000`, `+0000`, `GMT`, `UTC`, `+0000 (UTC)`, absent) | posting tool family | overview | Normalise to epoch first: Usenet.Farm rewrites Date to a 2-digit year + "UTC". |
| From (exact) | same uploader | overview | Weak under obfuscation: per-file random From (e.g. 44 volumes = 44 Froms). A fixed From across a post still ties the files together. |
| Subject zero-pad width, yEnc tag present/absent | same batch or tool | overview | PowerPost omits "yEnc". Obfuscated posts differ from =ybegin name=. |
| Segment size / part size (Nyuu 716800, postie 750000, ngPost 768000 or 5 MiB) | posting tool and settings | overview (Bytes) | Bytes includes headers, so compare the size class, not exact bytes. |
| Mixed tool labels inside one NZB | merged or repaired post | NZB | 3 of 246 corpus NZBs. |
| X-Received-Body-CRC | byte twin (same tool and split) | HEAD | zlib.crc32(body incl. CRLF + ".\r\n"), solved by the yEnc 15-min thread. Highwinds and Usenet.Farm only. |
| X-Ufhash, `Message-Id` casing, Path not-for-mail | provider = Usenet.Farm | HEAD | |
| Path `fxNN.<site>.POSTED!not-for-mail` | injected via Highwinds front end NN | HEAD | Per-feed hex, not per account. |
| Injection-Info posting-account | same account (same day only on eternal-september) | HEAD | Rotates every 24h there. |
| User-Agent / X-Newsposter (`Nyuu/x.y.z`, `sanguinews v..`) | tool + version | HEAD | Often stripped. |
| Header order | posting tool | HEAD | GoPostStuff fixed order. postie may randomise (Go map); wire order unverified. |
| =ybegin/=ypart/=yend field order, total= present, part= on =yend (NewsUP/ngPost omit it), crc32 only on the last part | encoder | edge | About 2 lines per article. |
| pcrc32 case and padding (mbruel ngPost unpadded vs Hydro74000 fork %08x) | tool fork/version | edge | |
| Line length (128/256), escaping of TAB/SPACE/'.' at line edges | encoder; version (Oct 2016 Nyuu left edge chars unescaped) | edge/body | |
| STAT co-removal timing | same release (takedown removes the fileset together) | STAT, ~1 day | UvA 2014-15 study. |
| Orphan retry article: same Subject (incl. yEnc n/m), From, Bytes, Lines as an NZB article, but a Message-ID that no NZB lists | same session and uploader; a stored second copy of that part | overview | Nyuu retries with a new ID after post timeout, connection reset after the body, 441, or a post-check STAT miss (lib/nntp.js:372-403, 828, 857, 975; lib/uploader.js:383-421). ngPost does the same on reconnect and resend (NntpConnection.cpp:217-222, NntpArticle.cpp:117-124). How common it is: unknown, likely rare. |
| Per-file random From with a constant posting rhythm | same release despite many posters | overview | NZB cf32b14460 has 44 volumes x 68 segments plus PAR2 sets of 2^k+1, each file under its own From. No public tool does per-file From: ngPost uses per-post `--gen_from`, postie uses per-article From. So it is a custom poster, and the variable-length `[A-Za-z0-9_-]` shape is itself a tool fingerprint. Time + size shape alone gives precision 0.27-0.42 when uploads overlap (Opus yEnc thread), so confirm with the session key or ID stream. |
| Duplicate segment number in a merged NZB | nothing for downloaders | NZB | SABnzbd keeps the first entry (nzbparser.py ~455). NZBGet keeps the last (daemon/queue/NzbFile.cpp:60, verified). Neither falls back to the alternate ID, so Atlas must not count on alternate IDs as backups when it merges (#46). Pick the ID it has verified, and put it in the position each client reads. |

## Rust implementation notes
- The classifier needs only std + regex, with ordered rules and precedence. See `raw/w08-c-rust-spec.md`, which has the wave 9 corrections appended.
- Store the raw =y lines and line stats with Atlas's own scanner. The yenc 0.2.2 crate always dot-unstuffs, so it double-unstuffs if the NNTP layer already did (a likely cause of false pcrc32 mismatches). It also hard-errors on unknown keys. crc32fast has `Hasher::combine` for per-part CRC folding.
- Add an `nzb_date_bad` flag, and skip time-window checks when it is set.

## Settled conflicts
- Highwinds T1 uses 64 symbols (C19).
- postie's default part size is 750000 (C9).
- postie keeps IDs on retries; Nyuu and ngPost mint new ones (C4, C10).
- JBinUp vs JBinDown grammars (C17).

## Still open
- How common orphan retry articles are; this needs a prod XOVER check.
- The generator behind variable-length `[A-Za-z0-9_-]` IDs with per-file random From (NZB cf32b14460).
- 3 other custom-poster NZBs.
- postie wire header order.
- Whether Highwinds XOVER Lines is empty.
- The meaning of X-Ufk.
- Backbone body dedupe.
- Old-Nyuu RNG: the Opus yEnc thread solved Node 4 (MWC1616, 8 NZBs from 2017-2019 link 100%). About 60 NZBs from 2018-2023, likely Node 6-11, remain unsolved. See opus/FINAL-yenc-header-forensics.md.

## Suggested next steps for Atlas
1. Port the classifier, and store `tool_label` plus the extracted fields (nyuu_ms, hw_t1, hw_n, hw_k, host) per segment.
2. Build the Highwinds session join (host, K) with T1/N windows as a merge key for split and obfuscated posts.
3. Before trusting the read-only check of X-Received-Body-CRC twins on prod data, verify it.
