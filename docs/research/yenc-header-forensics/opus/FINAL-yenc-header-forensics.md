# Linking Signals Map: yEnc and NNTP header forensics (Opus 3x3, final)

Thread: research post linking via yEnc headers (Opus 3x3), waves 1-10, 2026-11-02.
Detail lives in `w1a`-`w9c` in this folder, the tool table `w8c-fingerprint-table.md`, and the Atlas spec `w9b-atlas-rust-spec.md`.
Sibling reports: `../FINAL-REPORT.md` (Haiku header thread) and `../../payload-fingerprints/final-report.md` (payload thread).

Tags used below: **MEAS** = measured on real NZBs or articles; **SRC** = read in tool source; **SYN** = synthetic test only; **INF** = inference.

## 1. Summary

Headers alone link posts in three ways:
- they name the posting tool;
- they expose a per-process or per-session key (an RNG state, a timestamp, or a server counter);
- overview sizes act as a content hash for byte twins.

The strongest result is the **stock Nyuu V8 stream linker** (MEAS). Two Message-IDs recover the Math.random state of the Nyuu process. That state links 100% of IDs in all 19 solved real NZBs (up to 16,488 IDs each), with a false-match rate of about 2^-113, from XOVER alone.

**Nyuu ms-timestamp continuity** is also proven on real data:
- 0 inversions in 57,093 segments;
- 2,565 of 2,565 orphan files placed by leave-one-out;
- it works even where the RNG does not solve.

The **escape-count / segment-size-vector twin key** is measured on two independent real repost events:
- one pair of uploads 19.4 h apart;
- one family of 4 obfuscated uploads with renamed files;
- 0 false hits after an entropy gate.

**Validated on synthetic data only:**
- Nyuu-Obfuscation solver (60/60);
- NewsUP drand48 inverter;
- ngPost seed search.

**Source-read only:** the deterministic-ID tools.

**Not linkable** through the RNG:
- separate Nyuu processes (each post is its own process, so no RNG links between NZBs);
- old Nyuu, 2017-2022 (old-V8 xorshift unsolved; Node 4 MWC1616 solved for 8 NZBs from 2017-2019, the rest still unsolved);
- CSPRNG IDs (ngPost QUuid, postie, pesto);
- server IDs `@UFK`.

None of this is implemented in Atlas yet. `w9b` stages it as phases P1-P6.

## 2. Signals table

Read cost: **XOVER** = overview only; **HEAD** = one HEAD per sampled article; **BODY** = one body (first and last =y lines); **offline** = CPU job.

| Signal | What it proves | Read cost | Evidence level | Atlas status |
|---|---|---|---|---|
| Tool classifier (Message-ID regex set, then From/Date/Subject checks, then HEAD/BODY tie-breaks; 33 tool variants + 6 server families) | tool (and injector) | XOVER; HEAD/BODY only if ambiguous | MEAS by sibling: ~86% of 72,334 real segments labelled; Rust rules match Python on all 21 labels | not implemented; spec P1 (steps 1-6), P5 (steps 7-8) |
| Stock Nyuu V8 stream linker (Node >= 12; 24 letters/ID, 48 draws/article) | same upload process: every file of the post, plus missing/unindexed articles predictable | XOVER (2 IDs bootstrap) | MEAS: 19 real NZBs, 100% of IDs on one stream each; 0 draw-order vs ms inversions; FP 26^-24 | not implemented; spec P2 (port `stream_link.py` + alignment fix) |
| Nyuu ms-timestamp continuity (`-<13-digit ms>@nyuu`) | same session; part order; orphan file placement | XOVER | MEAS: 0/57,093 inversions; 0/2,651 adjacent-file overlaps; 2,565/2,565 orphans placed | not implemented; spec P1 (ms bookkeeping), P3 (placement) |
| Old Nyuu v0.3.x binaries, Node 4 MWC1616 Math.random | same upload process (if solved) | XOVER + offline | MEAS (W10A): letters depend only on low16(r0); per-ID search ~45M candidates, 0.04 s on 4 threads. 8 real NZBs (2017-07 to 2019-02) linked 100% (13 to 1654 IDs each); 2023+ xorshift NZBs give 0 candidates (no false positives). Unsolved: Mar/Apr 2018 and 2019-06 to 2023-04 (~60 NZBs), likely npm Nyuu on Node 6-11 (xorshift s0+s1, still open) | not implemented; add to spec P2 as a second model |
| Nyuu-Obfuscation solver (Subject 32 + From 36 = 68 base-62 draws) | same upload process; predicts Subject/From/wire ID/Date offset | XOVER | SYN: 60/60 articles predicted fwd/back from one article, ~25 runs; Subject alone rank-deficient | not implemented; spec P6, behind a flag until a real post confirms |
| NewsUP drand48 inverter (own-style `sec usec . b36(rand) @ b36 . newsup`) | same process; all sibling IDs of a file | XOVER | SYN (own Perl-drand48 re-impl): unique state 37/40 from one ID, 40/40 siblings, 0 false links, 0.1 s | not implemented; spec P3 |
| ngPost srand seed search (From `[A-Za-z0-9]{13}@ngPost.com`, group picks) | same process (Froms, group choice); NOT Message-IDs | XOVER + offline (<= v4.14 ms seed: ~6 s on 32 cores; >= v4.15 2^32: ~1.75 core-h at index 0, ~59 core-h within 1000 Froms) | SYN (glibc re-impl verified vs real glibc, 7 seeds x 100k); seed-1 Windows tables INF | not implemented; spec P6 (seed-1 lookup can go online) |
| Highwinds `T1.N$K.R@fxNN.site` server-ID session key | same upload session, across groups and obfuscation | XOVER | MEAS (sibling): T1 decode 869/869 within ±1 h; N monotone 861/862 steps; K constant per post (7/7) | not implemented; spec P1 |
| Deterministic-ID tools: nxg sha256 (40@21.3 hex), geri (HMAC + pcrc32 domain), .NET Usenet library GUID per file, Upstream `time.time()@riverup`, counter IDs (PowerPost, JBinUp, yEncBin, newzNZB) | same file / same post (nxg, GUID); content per part (geri pcrc32); session by time (Upstream) | XOVER (STAT for derived nxg gaps) | SRC only (no real posts in corpus; counter IDs seen in sibling segs.tsv) | not implemented; spec P1 |
| Escape-count / segment-size-vector twin key (first differences of per-part size; entropy gate; >= n-1 parts equal; PAR2 excluded; blocking key = segcount + first 2 full sizes) | same content twin (repost, renamed or not) | XOVER `:bytes`/`:lines` or NZB `bytes` | MEAS: 2 real repost events (pair 19.4 h apart, 15/15; 4-upload obfuscated family, 7/7 volumes, 14/15 on a damaged copy); 0 false full-vector hits gated; per-segment chance 0.22%. W10C: exact matches only below 6 segments; n-1 safe from about 6 segments (10^5 files), 7 (10^6), 8 (10^7); n-2 adds 878 false pairs, never use. Add a second blocking key on segments 3-4 so a damaged early segment still blocks. Keep sd floor 40 for confirmation (70 drops 2 true groups). Cross-article-size prediction works on synthetic high-entropy data via total escape count, not on low-entropy. Treat as a candidate generator; confirm with size + CRC32 + 16k/full MD5 (payload thread). | not implemented; spec P4 |
| X-Received-Body-CRC twin key (`crc32(body CRLF ++ ".\r\n")`, unpadded decimal) | exact byte twin: same encoder, split, name= and payload | HEAD (XHDR if allowed) | MEAS by sibling on 12 real articles (formula solved); stamped at injection by Highwinds/Usenet.Farm | not implemented; spec P5 |
| Date form / header order / NZB-writer fingerprints | tool family and version; who wrote the NZB vs who posted | XOVER (Date), HEAD (order), NZB | SRC + MEAS (NZB writer table on ~350 NZBs; Nyuu `utf-8` vs `UTF-8` dates the version) | not implemented; spec P1 (Date), P5 (order) |
| yEnc edge quirks (pcrc32 padding/case, `part=`/`crc32=` presence, trailing-space escaping, blank line before `=yend`) | encoder and fork/version, even under faked IDs | BODY | SRC (encoder source per tool); not measured at scale | not implemented; spec P5 (classifier step 8) |
| Server-assigned ID families (`@UFK`, Diablo `$`, INN radix32, Giganews) | provider / injector; Diablo and INN give a soft (host, pid) session | XOVER | SRC/MEAS (sibling); `@UFK` server-assigned per NewsUP source | not implemented; spec P1 (labels only) |
| Orphan retry articles (same Subject/From/size as an NZB part, ID absent from NZB) | same session and uploader; second stored copy of a part | XOVER | SRC (Nyuu/ngPost retry paths); prevalence unknown; 0 retry pairs in 332 NZBs (NZBs keep only the winner) | not implemented; falls out of P2 orphan scan for Nyuu |
| Per-file random From custom tool (variable-length `[A-Za-z0-9_-]` IDs, one From per ~68-segment volume) | same release despite many Froms (join by timing and size shape) | XOVER | MEAS on 1 NZB (44 volumes); generator unidentified; lengths of 4k+1 rule out base64 of random bytes | not implemented; FP label only |

## 3. Tool fingerprint table (condensed from w8c)

Link method: **RNG** = PRNG state solve; **DET** = derivable IDs; **TIME** = timestamp in ID; **SRV** = server ID carries session data; **FP** = label only.

| Tool | Message-ID shape | From | Date | Segment size | Link method |
|---|---|---|---|---|---|
| Nyuu stock, Node >= 12 | `([A-Z][a-z]){12}-\d{13}@nyuu` | `$USER <$USER@host>` (leaks login + host) | `GMT`, second == ID ms | 716800, line 128 | RNG + TIME |
| Nyuu stock, old binaries (v0.3.x, Node 4) / npm on Node 6-11 | same | same | same | 716800 | TIME + FP (RNG: see W10A) |
| Nyuu wrappers / custom domain (`@PRiVATE`, transmitter etc.) | Nyuu shape, other host; `${rand(N)}` tokens | fixed or `rand(20)@rand(20).rand(3)` | as Nyuu | 716800 | RNG (extra draws) + TIME |
| Nyuu-Obfuscation | `{32}@{8}.{3}` alnum, drawn at send | `{14} <{14}@{5}.{3}>` per article | `GMT`, backdated 0-6 h | 716800 | RNG (SYN) |
| postie (random IDs) | `{32}@{8}.{3}` alnum, crypto/rand | `{14} <{14}@{5}.{3}>` | `UTC` / `+0000` | 750000 | FP |
| postie nxg / nxg | `[0-9a-f]{40}@[0-9a-f]{21}\.[0-9a-f]{3}` | from sha256(Subject) | as postie | 750000 | DET |
| nxg-upper | same shape, Comment-keyed | hex-derived | local zone | 768000 | DET / FP |
| ngPost <= v4.14 | `[0-9a-f]{32}@ngPost` (UUID v4) | `[A-Za-z0-9]{13}@ngPost.com` | none (server) | 716800 (NZB bytes flat) | RNG on From (ms seed) |
| ngPost >= v4.15 | same | same / `@<sig>.com` | none | 716800 | RNG on From (2^32 or seed-1) |
| ngPost fork v5.6+ | 32 hex, mt19937 | reseeded per call | none | 716800 | RNG on yEnc name (BODY) |
| NewsUP 2017-02..06 | none sent; server `[A-Za-z]{32}@UFK` | `NewsUP <NewsUP@somewhere.cbr>` | none | 768000 | SRV (provider only) |
| NewsUP own style | `\d{11,16}\.[0-9a-z]+@[0-9a-z]+\.newsup` | rand8 or fixed | none | 768000 | RNG (drand48) + TIME |
| NewsUP --obfuscate | imitates Nyuu/PowerPost/JBinUp/GoPostStuff with near-misses | empty | none | 768000 | FP (impostor tells) |
| GoPostStuff | `\d+\$gps@gopoststuff` (UnixNano) | n/r | none | config | TIME |
| newsmangler | `epoch.5dp.part@<provider host>` | n/r | n/r | n/r | TIME + counter |
| PowerPost 2000 A&A | `partNofM.{20}@powerpost2000AA.local` | n/r | n/r | n/r | FP (counter) |
| JBinUp / JBinDown | `{19,20}_NoM@JBinUp.local` / `.N-M@JBinDown.local` | n/r | n/r | n/r | FP (counter) |
| yEnc32 / TyEncoder, newspost, SuperPoster, sanguinews | none sent (server ID) | user | none / server | n/r | SRV / FP |
| nntpPoster | server ID or `hex32@<nntp host>` | n/r | `+0000 (UTC)` | n/r | SRV / FP |
| pesto | `hex32@<domain>` (no v4 nibble) | per-run random | n/r | n/r | FP |
| usenet-posting | dashed UUID v4 @ domain list | fake name | n/r | n/r | FP (spoofed UA list) |
| geri | `b64url(HMAC)@<pcrc32 hex>.geri` | n/r | n/r | n/r | DET |
| .NET Usenet library (NzbBuilder) | `partNofM.<guid32>@unknown.com` | n/r | n/r | n/r | DET (GUID per file) |
| Upstream / riverup | `\d{10}\.\d{6}@riverup` | n/r | n/r | n/r | TIME (µs) |
| yEncBin Poster | `PartNofM.HEX32@ts10.local` | `yEncBin@Poster.com` | n/r | n/r | TIME + counter |
| Unknown base64url custom poster | `[A-Za-z0-9_-]{20..29}@[..]{5..9}.[..]{3}` | random per file/volume | n/r | ~700-750 KB INF | FP |

Server families: Highwinds `T1.N$K.R@fxNN.site` (SRV, session key), Diablo `%08lx$seq$pid$hash@` (SRV/TIME), INN `radix32$pid$count@` (SRV), Giganews `{32}@giganews.com` (TIME), `@UFK` (FP of provider).

## 4. Dead ends and corrections

**Corrections**
- **X-Received-Body-CRC origin.** w2b said the reading server adds it. w3a corrected this: Highwinds/Usenet.Farm stamp it at injection, and it travels with the article. The sibling thread then solved the formula: `crc32(body with CRLF ++ ".\r\n")`.
- **Nyuu retries.** The 24 letters are redrawn on every send attempt, including post-check reposts. Only the ms suffix survives a retry. The published ID is the send-time draw; the build-time draw is discarded. So the stream steps 48 draws per article (72 with a retry).
- **w6a partial link counts were a bug, not data.**
  - Cause: cache-block misalignment plus a regeneration window sized for 24 draws per article.
  - Effect: counts like 164/172 and 245/815.
  - Fix (w8b, ported in w9b): try all 127 boundary shifts and size the window at about 150 draws per article. After the fix, 100% of IDs link.
- **Old-Nyuu diagnosis.**
  - w6a guessed a 2022-2023 V8 change. The V8 output conversion actually changed between V8 7.0 and 7.1 (late 2018).
  - w9a then found that the v0.3.x Nyuu binaries bundle Node 4. Node 4 uses MWC1616, not xorshift128+.
  - So the w7a/w8a old-xorshift model was probably the wrong PRNG for these posts.
- **`@UFK` is server-assigned.** The 2017 NewsUP build sent no Message-ID and recorded the server's 240-reply ID.
- **The `32hex@ngPost` post was not linkable through its From.** Its From contained `_-` and a random domain, which ngPost's From generator cannot produce. It was a fixed `FROM=` setting.
- **w8c table fixes.**
  - The sibling table had the JBinUp and JBinDown regexes swapped.
  - postie's default segment size is 750000, not 768000.
  - "Nyuu 700K vs 716800" describes one size, not two (700 KiB = 716800).
- **postie header order is fixed on the wire** (sibling, SRC: nntppool `PostHeaders.WriteTo`). This settles conflict X15. The Go-map random order found in w2c is dead code.

**Dead ends**
- **No RNG links between NZBs for tools that run one process per post.**
  - Test: 14 distinct Nyuu states, each stepped 1e9 draws: 0 hits, even for posts made 3 minutes apart.
  - Cross-post RNG linking only works when one process posts several jobs.
- **z3 and CryptoMiniSat both fail on the old-V8 `(s0+s1)` xorshift.**
  - Each draw leaks ~4.7 bits through a carry chain.
  - Neither solver finishes, even at the known cache offset. CryptoMiniSat finishes only when about 48 of the 128 state bits are given.
- **`@UFK` IDs are unpredictable.** Their 52 symbols are uniform and fail the glibc lag test.
- **`32hex@ngPost` IDs are unpredictable.** They are QUuid v4 from the OS CSPRNG.
- **No weak RNG in other posters.** The w7c survey covered ~20 repos (Rust, Go, .NET, Java, Python). It found only CSPRNGs, deterministic IDs, server IDs, or wrappers around Nyuu/ngPost.
  - NZBPostarr's Python MT group pick leaks ~4.6 bits, which is useless.
  - nntpPoster's `new Random()` picks only lengths.
- **The size key fails on some inputs.**
  - Low-entropy content has ~0 escapes, so the key only names a content class.
  - ngPost NZBs store a flat 716800 `bytes`, which carries no information.
  - PAR2 files never match across uploads because they are regenerated per post.
  - Different article sizes cannot match by segment index.
- **ngPost seed recovery does not reach Message-IDs** (CSPRNG), and archive passwords are out of scope.

## 5. Recommended Atlas rollout (from w9b §11)

| Phase | Ships | Reads | Expected yield |
|---|---|---|---|
| P1 | Classifier steps 1-6, Highwinds (host,K,T1,N) key, deterministic parsers (nxg self-check, .NET GUID, counter IDs), Nyuu ms bookkeeping | XOVER | tool label ~86% of segments; Highwinds sessions; exact intra-file groups |
| P2 | Nyuu V8 stream linker with alignment refinement and orphan scan | XOVER | 100% of IDs of every new-V8 Nyuu post (largest family); FP ~2^-113 |
| P3 | ms-continuity placement (old/wrapper Nyuu); NewsUP drand48 | XOVER | orphan placement 100% (MEAS); NewsUP streams (SYN) |
| P4 | Escape / size-vector twin key with NZB-writer + entropy gates | XOVER `:bytes`/`:lines`, NZB bytes | cross-upload content twins, incl. renamed reposts |
| P5 | X-Received-Body-CRC twin key + HEAD sampler; HEAD/BODY classifier steps 7-8 | HEAD (pipelined), XHDR probe | exact byte twins; labels for faked/server IDs |
| P6 | Offline: ngPost seed search, Nyuu cross-stream sweep, Nyuu-Obf variant (flagged), ngPost-fork mt19937 | core-hours | ngPost From/group streams; rare cross-NZB Nyuu links |

**Not planned:**
- Old-V8 Nyuu solve (Node 6-11, xorshift128+ with s0+s1 output) for the ~60 still-unsolved 2018-2023 NZBs. The Node 4 MWC1616 model solved 8 NZBs from 2017-2019, so the remaining ones need this model.
- CSPRNG and `@UFK` IDs.
- Archive passwords.

**Main risk:** a wrong tool label, not a false key match, because all key matches are verified exactly.

## 6. Open questions

- **Atlas real-data validation.** Running the classifier, the Nyuu linker and the twin keys on Atlas's own XOVER/HEAD data is pending the owner's approval. So far everything has been measured on public NZB corpora, mostly one author's test NZBs.
- **Old Nyuu (2017-2022).** Does the Node 4 MWC1616 brute force solve these IDs (W10A)? If not, were those posts made with an npm install on Node 6-11, or with a custom `${rand}` template?
- **Highwinds K.**
  - What is the collision rate across uploaders and over time?
  - Is K ever reused? K reuse was not measured.
  - Until this is measured, require (host, K) plus T1 within 1 h plus a monotone N.
- **X-Received-Body-CRC access.** Does any provider allow `XHDR X-Received-Body-CRC` or put it in `LIST OVERVIEW.FMT`? Is the CRC taken over the dot-stuffed body? Is it on every Highwinds/Farm article?
- **Size-vector twin key.**
  - Is it stable across NZB writers (body-only vs header-inclusive `bytes`)? No pair of the same content from different writers has been found yet.
  - Byte-range matching across different article sizes is untested.
  - The entropy reference should be theoretical, not fitted on this corpus. See W10C.
- **The base64url custom poster.** What generates the variable-length `[A-Za-z0-9_-]` IDs, and is the From really one per volume? Checking that the From changes line up with `<file>` boundaries would test this.
- **Nyuu-Obfuscation.** It has not been confirmed on a real post with >= 20 segments.
- **NewsUP inverter.** It has not been run against real Perl (>= 5.20 drand48 assumed).
- **ngPost Windows seed-1 Froms.** These are inferred, not observed. No hits were found for the predicted strings.
- **Orphan retry articles.** How common are they in production XOVER?
- **Header meanings.** What do X-Ufk and X-Ufhash mean? Does Highwinds XOVER carry `Lines`?
- **Multi-post single Nyuu invocations or Nyuu-as-library daemons.** These are the only case where an RNG link between NZBs could exist. Do any exist?

## 7. Index of report files and tools

All paths relative to `research/yenc-header-forensics/opus/`.

**Reports**
| File | Content |
|---|---|
| `w1a-encoder-quirks.md` | yEnc encoder quirks per tool (templates, pcrc32 padding, edge escaping) |
| `w1b-nntp-headers.md` | Message-ID grammars, From defaults, header order, injection headers |
| `w1c-indexers.md` | indexer linking (NNTmux collection hash, donor joins, PAR2 IDs, nxg) |
| `w2a-prng.md`, `w3b-prng.md`, `w4a-rng-details.md` | PRNG feasibility: V8, ngPost std::rand, NewsUP drand48; draw budgets |
| `w2b-backbone-headers.md`, `w3a-body-crc.md` | backbone injected headers; X-Received-Body-CRC origin correction |
| `w2c-poster-catalog.md`, `w3c-legacy-posters.md`, `w4b-legacy-posters2.md` | poster catalog incl. NewsUP, postie, nxg, yEnc32, PowerPost, newspost, JBinUp |
| `w4c-overview-math.md` | overview (:bytes/:lines) model; escape count as content hash |
| `w5a-nyuu-obf-sim.md` | Nyuu-Obfuscation solver, synthetic 60/60 |
| `w5b-drand48-glibc.md` | NewsUP inverter; ngPost glibc/MSVC seed search costs |
| `w5c-nzb-escape-test.md`, `w6b-real-repost.md`, `w9c-real-repost-corpus.md` | escape / size-vector twin key on real NZBs; repost events; gates; blocking key |
| `w6a-nyuu-id-real.md`, `w8b-cross-nzb-stream.md` | Nyuu V8 solver on 19 real NZBs; alignment fix; no cross-NZB links |
| `w6c-time-domain.md` | Nyuu ms continuity, orphan placement, NZB-writer table |
| `w7a-nyuu-old-v8.md`, `w8a-old-v8-solve.md`, `w9a-node-and-base64url.md` | old-Nyuu diagnosis (V8 history, z3/CMS failure, Node 4 MWC1616); base64url poster |
| `w7b-old-newsup-ngpost.md` | `@UFK` server IDs; `32hex@ngPost` post with fixed custom From |
| `w7c-weak-rng-survey.md` | survey of ~20 other posters: no new weak RNG |
| `w8c-fingerprint-table.md` | consolidated tool table (33 variants, 6 server families, 21 conflicts, classifier order) |
| `w9b-atlas-rust-spec.md` | Atlas Rust spec: modules, algorithms, costs, fixtures, phases P1-P6 |
| W10A / W10C reports | w10a-node4-mwc1616.md (+ nyuu-id-solver/mwc1616/), w10c-twin-key-robustness.md (+ repost-test3/) |

**Tools (own code; no third-party code run)**
| Folder | Content |
|---|---|
| `nyuu-id-solver/` | `nyuuid_solve.py` (new-V8, alignment fix ported), `stream_link.py` + C kernel (link and sweep between NZBs), `w8b_synth.py`, `w8b-stream-link-results.json` (fixtures), `nyuuid_solve_old.py` / `nyuuid_solve_old2.py` (old-V8 z3 / CryptoMiniSat, negative) |
| `v8-solver/` | first V8 floor(x*62) GF(2) solver (`gen.js`, `solve.py`) |
| `nyuu-obf-sim/` | Nyuu-Obfuscation re-implementation and solver (`sim.js`, `solve2.py`, `solve_subjonly.py`) |
| `rng-tools/` | `newsup_sim.py`, `seed_backtrack.py`, `ngpost_rand.c`, `msec_window.c` |
| `time-domain/` | ms continuity and leave-one-out placement (`p1*.py`, outputs) |
| `overview-model/` | escape-rate Monte Carlo and preset table (`m.py`, `t.py`) |
| `nzb-escape-test/` | escape-count key on 104 real NZBs / 69,181 segments |
| `repost-test/`, `repost-test2/` | real repost pairs; corpus index, entropy gate, blocking-key collisions |

Sibling cross-references:
- `../fingerprints.md`, `../raw/w08-c-rust-spec.md`, `../tools/msgid-classifier/`, `../tools/xrbc/` (XRBC formula);
- `../../payload-fingerprints/final-report.md` (PAR2/RAR payload keys, CRC32 GF(2) toolkit).
