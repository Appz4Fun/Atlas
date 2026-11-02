# Wave 08-C: anti-indexing evasion in uploaders, and which linking signals survive

Scope: Postie, ngPost, Nyuu, NewsUP, nxg-upper, plus forks found by search (x-cord/Nyuu, x-cord/transmitter, estalker82/ngPost). Source was read from shallow clones under /tmp/w8c/. Repo heads: postie 2026-10-21, Nyuu 2026-10-05, ngPost 2024-01-26 (no commits since), nxg-upper, NewsUP, transmitter (HEAD at clone time).

Tags: **verified in source** (file:line read in this session), **changelog** (none found: no CHANGELOG in any clone, so this tag is unused), **issue** (read via WebFetch summary, not the raw thread), **guess** (inferred, not checked).

Paths below are relative to /tmp/w8c/ unless absolute.

## 1. Feature inventory

| Feature | Postie | ngPost | Nyuu | NewsUP / nxg-upper / forks | Tag and source |
|---|---|---|---|---|---|
| Random article size per file or article | Not present. Fixed `ArticleSizeInBytes`, default 750000, preset 768000 | Not present. Fixed article size | Not present. Fixed `articleSize: 716800` | Not present in README | verified in source: postie/internal/config/config.go:574-575, 1110; Nyuu/config.js:83 |
| Random article size (attempted in a fork) | - | - | Maintainer rejects it. A fork randomized filename length, which changed article length, and gets "Article length mismatch". Maintainer: "Modifications which alter an article's length will not work in Nyuu" | - | issue: Nyuu issue #121 (WebFetch summary, not the raw thread) |
| Random yEnc line length | Not checked | Fixed `line=128` | Fixed `bytesPerLine: 128` | nxg-upper has a configurable `LineLength`, fixed per run | verified in source: ngPost/src/nntp/NntpArticle.cpp:66; Nyuu/config.js:84; nxg-upper/yEncEncoder.go:49-50, 76, 84 |
| Per-article group spray | Only per file: `EACH_FILE` picks one random group per file. Not per article | Not present | Not present. Servers are picked randomly per post | - | verified in source: postie/internal/config/config.go:122; postie/internal/poster/poster.go:1373 |
| Random From per article | `full` policy: random From per article | Optional. From is random when unset, else one value for the upload | Default From is built from the local user and hostname (constant per run) | - | verified in source: postie/internal/poster/poster.go:1469-1475; ngPost/src/nntp/NntpArticle.cpp:144; ngPost/src/NgPost.cpp:472, 1496; Nyuu/config.js:101 |
| Random Message-ID per article | `MessageIDFormatRandom` (32 random chars). Default format not checked | Message-ID is a per-article id plus a per-session signature | Randomized on every submit (`randomizeMessageID`), 24 letters alternating case. Off with `keepMessageId` | - | verified in source: postie/internal/config/config.go:129-131; postie/internal/article/article.go:96; ngPost/src/nntp/NntpArticle.cpp:147; Nyuu/lib/nntp.js:828; Nyuu/lib/article.js:199-227 |
| Random Date per article | `full` policy: random within the last 6 hours, per article | Not present. Date is not set | `postDate` override. Default is the post generation time | - | verified in source: postie/internal/article/article.go:218-223; postie/internal/poster/poster.go:1493-1495; postie/internal/config/config.go:142; Nyuu/config.js:88, Nyuu/lib/article.js:132-133 |
| Subject obfuscation | `full`: random 32 chars per article. `partial`: hash of name | Subject omitted or UUID with `obfuscate` option | Subject from config | NewsUP `obfuscate` option; nxg-upper `Obfuscate` | verified in source: postie/internal/config/config.go:138-154; ngPost/src/NgPost.cpp:1446; nxg-upper/defaultconfig (Obfuscate option) |
| PAR2 filename obfuscation | `Par2ObfuscationPolicy` applies the same policies to PAR2 files. But PAR2 packets are built with the real basename | `--gen_par2` on a renamed archive | `par2_rename_settings` renames files for PAR2 (`-r0`) | NewsUP README:207-208 | verified in source: postie/internal/poster/poster.go:1438-1446; postie/internal/par2/par2.go:107-113; NewsUP/README.md:207 |
| Password-encrypted RAR (-hp, gen_pass) | Not present | `gen_pass`, `gen_name`: random archive name and password | Not present | nxg-upper: `ObfuscateRar`, `Password`, `PasswordLength` | verified in source: ngPost/src/NgPost.cpp:781-784, 1779-1783; nxg-upper/postFile.go:114; nxg-upper/README.md:49-56. Whether `-hp` (header encryption) is used: **guess** |
| Junk or padding files | Not found | Not found | Not found (grep hit is a TODO on junk responses in a fork) | Not found | verified in source (absence): grep over all clones |
| Randomized posting order | Not found | Not found | Not found | Not found | verified in source (absence). Shuffle hits in postie are GF16 SIMD names (par2.go:46-52), not posting order |
| Delays or jitter between articles | Not found | Not found | Config delays are retry/reconnect only (config.js:69-70, 37-38). No jitter in nntp.js | Not found | verified in source (absence): Nyuu/lib/nntp.js |
| Multi-server spreading | Not checked | Not checked | Random server per post. Doc says "servers are currently selected randomly for posting" | transmitter: uploads to "all specified accounts" | verified in source: Nyuu/config.js:63; transmitter README (WebFetch summary) |
| Stealth or NXG deterministic IDs | `nxg` Message-ID format. Full policy skips X-Nxg header | - | - | nxg-upper: 21-byte random string, per-article IDs are SHA256 of header + type + number, so they are deterministic given the header | verified in source: postie/internal/config/config.go:129-130, 283; postie/internal/poster/poster.go:1476-1481; nxg-upper README (WebFetch summary) |

## 2. Feature x signal matrix

Key: **S** survives, **W** weakened, **K** killed. Each cell gives the reason in one line. Features not listed here (junk, jitter, posting order) are not present in any tool, so all their cells are S.

Signals: yEnc chain = part/total/size/begin/end. Size bucket = overview :bytes/:lines. Overlap = posting-time overlap of file k end and k+1 start. PAR2 bridge = FileDesc md5(first 16 KiB)+length. RAR5 chain = volume chain from the first article. MID shape = Message-ID form. Header order = order of headers. Date gap = Date vs Injection-Date offset.

| Feature | yEnc chain | Size bucket | Overlap | PAR2 bridge | RAR5 chain | MID shape | Header order | Date gap |
|---|---|---|---|---|---|---|---|---|
| Random article size (not shipped; Nyuu rejects it, issue #121) | S: no shipped tool varies it | S: sizes fixed per tool | S | S | S | S | S | S |
| Random yEnc line length (not shipped) | S: `line=128` fixed in ngPost and Nyuu | S | S | S | S | S | S | S |
| Group spray per file (postie EACH_FILE) | S: group is not in the chain | S | S: timing unchanged | S | S | S | S | S |
| Random From per article (postie full, ngPost optional) | S: From is not in the chain | S | S | S | S | S | S: order unchanged | S |
| Random Date per article (postie full) | S | S | W: the Date header no longer shows real posting time. Server arrival time still does, so overlap survives if the tool uses arrival time. Verified in source: article.go:218-223, poster.go:1493 | S | S | S | S: order unchanged | K: Date is random, so the Date vs Injection-Date offset carries no signal. Injection-Date is set by the server, not the uploader. Tag: **guess** for the server behavior |
| Random Message-ID per article (Nyuu default, postie random) | S | S | S | S | S | W: shape stays fixed (24 letters alternating case in Nyuu; 32 random chars in postie). Per-post key is gone | S | S |
| Subject obfuscation (postie full, ngPost obfuscate) | S: yEnc name and subject are separate; the chain keys on part/begin/end | S | S | S | S | S | S | S |
| PAR2 name obfuscation (postie par2 policy, NewsUP rename) | S: obfuscation changes yEnc name, not the chain | S | S | S: FileDesc md5 and length come from content. Caveat: postie builds PAR2 with the real basename (par2.go:107-113), so the name field leaks real names | S | S | S | S | S |
| Password RAR, name-obfuscated RAR (gen_pass, ObfuscateRar) | S: chain fields unchanged | S | S | S | W if content only, K if headers encrypted (-hp). Header encryption is **guess** | S | S | S |
| Stealth/NXG deterministic IDs (postie nxg, nxg-upper) | S | S | S | S | S | S: shape fixed and computable from the header | S | S |
| X-Nxg header omitted (postie full) | S | S | S | S | S | S | W: one fewer header, so the header set changes (guess: no cluster signal depends on it) | S |

Cells marked S in the chain column are tagged **verified in source** only for postie's policy doc (config.go:138-154). That doc lists the changed fields as name, subject, From and Date, so part/total/size/begin/end are not touched. The postie yEnc emitter itself was not read.

## 3. Notes by tool

- **Postie** has the richest obfuscation: `full` randomizes subject, yEnc name, From and Date per article (config.go:138-142). `partial` keeps real Date and From (config.go:146-151). Par2 gets its own policy (poster.go:1438-1440). No size, line length, jitter or order randomization.
- **Nyuu** randomizes Message-ID on every submit (nntp.js:828). It explicitly refuses article-length variation (issue #121). Multi-server selection is random per post (config.js:63).
- **ngPost** is last changed 2024-01-26 (no newer commits in the clone). Obfuscation is subject UUID, random archive name and password, and random From. Line length is fixed at 128.
- **nxg-upper** adds RAR name and password obfuscation (postFile.go:114) and deterministic NxG IDs (README).
- **NewsUP** README warns that obfuscation makes NZBs "non 100% correct" (README:30-34). PAR2 rename is a documented option (README:207).
- **transmitter** (x-cord) is a Nyuu fork with per-post From templating and a random filename option (WebFetch summary of the README, not verified in code).

## 4. Gaps

- No 2025-2026 uploader with random article size or random line length was found. Search returned only the tools above.
- The Nyuu issue #121 text was read through a WebFetch summary. The raw thread was not read.
- The x-cord/transmitter claims come from a README summary, not from code.
- Injection-Date behavior and whether postie's default Message-ID format is random were not checked.

## Most robust signals

- **yEnc part/total/size/begin/end chain.** No shipped tool varies it. Postie's policies change only name, subject, From and Date (config.go:138-154), and Nyuu rejects length variation (issue #121).
- **PAR2 FileDesc bridge (md5 of first 16 KiB + length).** Content-derived, so it survives all name, subject, From, Date and Message-ID obfuscation seen. Caveat: postie builds PAR2 from the real basename (par2.go:107-113).
- **Article size bucket and fixed yEnc line length.** Both are tool-level constants (ngPost NntpArticle.cpp:66, Nyuu config.js:83-84). They survive every per-article feature in the table, and they fingerprint the uploader even when content is hidden.
