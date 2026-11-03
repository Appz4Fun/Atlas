# Linking binary posts by posting behaviour: final report

10 research waves (2026-11-02, 06:30-11:40Z), 30 subagents. The angle is behavioural and temporal: timing, article numbers, spray across groups, poster rotation, server-assigned IDs, provider commands, and clustering. Payload signals (CRCs, PAR2 internals, archive headers) belong to the payload-fingerprints threads and are only referenced here.

## Bottom line

1. **Upload tools post files one after another, and none of them shuffle the order or add jitter.**
   - A file's last article and the next file's first article are almost back to back.
   - On 202 real public NZBs, 98% of gaps from one Nyuu file's end to the next file's start are under 2 s (median 0.12 s).
   - Joining files end-to-start therefore rebuilds the chain even when every file sits in a different group under a different poster. That is the Batman Knightfall pattern.
2. **The join should get its time from the right clock.**
   - The poster's Date header is unreliable: postie randomizes it by 0-6 h, and Nyuu `--date` fixes it to one value.
   - Use the Nyuu Message-ID milliseconds, server IDs that encode time (Highwinds T, Giganews w2, Diablo hex), or article-number rank within one second. Injection-Date exists only on ngPost posts.
3. **Time alone gets crowded. Content resolves the ambiguity.**
   - Match the whole upload against the PAR2 file-size list. One PAR2 index body per upload, then break ties with the first-16 KB MD5 of each candidate run.
   - This gives 100% correct uploads in simulation, including equal-size RAR volumes, at 0.04-0.96 bodies per file.
   - Without PAR2 (about half of public NZBs, 15-30% of multi-file ones), read the RAR/7z volume header from article 1 of each run, at about 1 body per file.
4. **Server session keys are fragments, not post IDs.**
   - Tools spread a post over many connections, so in simulation each key covers about 6% of a post.
   - In real data, though, Highwinds S was constant per upload in 7/7 NZBs. Diablo (host, PID) is a soft key.
5. **Uploaders defeat names, never mechanics.**
   - Guides and tool docs cover subjects, posters and passwords.
   - Nothing covers timing, posting order, article size, yEnc size/total or PAR2 descriptors, and no tool can change those.
6. **Twins stay separate rows.** A hash16k or PAR2 match proves which file a run is, not which upload it belongs to. A later twin must link as a twin, not merge into the original.

## Recommended v1 (5 stages)

| # | Stage | Read cost | Notes |
|---|---|---|---|
| 1 | Ingest + dedupe | overview | Key on (msgid, group, article num). Keep Xref crosspost sets. Pick a clock per tool: Nyuu ms, then server-ID time, then article rank in the second. |
| 2 | File runs | overview; yEnc header only where ambiguous | Use a stable subject or Nyuu ms where present. With per-article random subjects (postie full), file identity needs every body. Mark those uploads expensive. |
| 3 | End-to-start join | overview | Calibrate the window per tool and clock (Nyuu about −0.1..+2 s; postie overlap-tolerant). Add a size-likeness bonus (+.15-.25 precision). Keep chains open. |
| 4 | Content resolve | 1 body per upload + ≤1 per file | PAR2 index → sizeChain → hash16k per unique-link run. With no PAR2: the RAR/7z header in article 1 plus the last volume's inner CRC. Retry on the next file if article 1 is gone (DMCA). |
| 5 | Union + output | n/a | Veto only on content (part size, yEnc total, CRC). Use our own veto-aware union-find (~80 lines; `ena` has no veto). Outputs: Proven / SizeMatched / TimeOnly, plus twin and alt groups. Re-evaluate on late parts (hold about 4 h) and on 451s. |

Cut from v1: per-connection fragment stitching, Fellegi-Sunter with EM (use a rule ladder first), Hungarian matching, and full pcrc32 folds. Full spec: `wave-09-A-pipeline-spec.md`; required fixes to it: `wave-10-A-redteam.md`.

## Key numbers

| Result | Value | Source |
|---|---|---|
| Real Nyuu end-to-start gap | median 0.12 s, p90 0.67 s, 98% < 2 s (n=1,070) | wave 9B |
| Whole-second NZB dates only | 38% of gaps < 2 s; join precision about chance | wave 9B |
| Re-link of real NZBs by time only | P .93 (2 concurrent), .60 (10 simultaneous), .98 (10 spread over an hour) | wave 9B |
| Rank-in-second timing vs raw 1 s Date | recall .94 vs .77 | wave 2 |
| Whole-upload PAR2 size match | 100% whole with distinct sizes; 65-80% with equal volumes | wave 7 |
| + hash16k tie-break | 100% at 1-8 concurrent uploads, 0.04-0.96 bodies per file | wave 8 |
| Per-connection key alone | ~6% of a post, 0% whole; stitched .85 (8 posts/front-end), .5 (32) | wave 8 |
| Highwinds decode | T 0/862 order violations; S constant per NZB 7/7; N rises 861/862 | waves 5-9 |
| PAR2 presence in public NZBs | 48% all, 77% with 3+ files | wave 9C |
| Sampling 1 in 5 yEnc headers | whole uploads .71 → 0 | wave 3 |

## 1. Signal table

| Signal | Proves | Read cost | Works on | Evidence | Atlas |
|---|---|---|---|---|---|
| End-to-start join with within-second rank (`t = sec + (rank+0.5)/n`, window `[-1.2·width/rate-0.5, +0.5]` s; width ≈ 0.75 × connections) | same upload (file chain) | OV (+ yEnc header per article if subjects are obfuscated) | serial posters: ngPost, Nyuu, postie (2 files overlap) | sim P .975 / R .942 (raw 1 s Date: R .77); real Nyuu gaps median 0.12 s, 98% < 2 s (n=1,070); re-link P .93 (2 concurrent), .60 (10 simultaneous) | not built |
| Article-number contiguity / dense window | same session (order) | OV | all; one fixed backend (not Abavia synthesized numbers or mixed Diablo engines) | src (RFC 3977 s6); no tool jitters order | not built |
| Xref crosspost anchors | order only (aligns group counters exactly); NewsUP: same uploader (group pool) | OV | INN-numbered spools, Highwinds, Abavia (full Xref) | src (INN numbers all groups in one call) | part: XOVER Xref dropped (`parse_overview` reads 8 columns); 1 HEAD/release |
| NNTP-Posting-Date / Injection-Date | order only (server clock, 1 s) | HDR/HEAD | Injection-Date only if client omits Date/ID (ngPost); NNTP-Posting-Date on Eweka/Newshosting | src + real headers | part: Date, Injection-Date kept, never compared |
| Date header | order only (only when server-stamped) | OV | ngPost (server adds Date); never Nyuu `--date` or postie full | src | not built |
| NEWNEWS single-group bisection | order only (exact per-ID arrival second) | ~log2(window) NEWNEWS calls per window, all IDs at once | INN (on by default); nntpswitch passes single-group; Diablo off | src; unprobed (hyp) | not built |
| Highwinds `<T.N$S.R@fxNN.site>` decode | same session (S); order (N); time (T) | OV | posts where the client lets Highwinds assign IDs | real: T 0/862 order violations; S constant per NZB (7/7), differs between same-day NZBs; N rises 861/862. sim: unique S matches reading every header (.71 whole); .40 if 30% share S | not built |
| Giganews XOR-0x9D decode | time (w2); same connection (w0+w4, hyp) | OV | Giganews + comcast/speakeasy/cavtel labels | real w2 20/20 within 1 s; w0/w1/w4 hyp | not built |
| Diablo `<hex$seq$pid$hosthash[$x]@cluster>` | same session ((hosthash, pid) set in window; a pid serves many connections); time | OV | Astraweb, UsenetServer upload, free.fr, Eweka uploadreader | src + real 521/602 within 10 min | not built |
| INN radix32 server ID | same connection (pid + counter); time | OV | INN servers that assign IDs | src; no binary samples | not built |
| Supernews base32 prefix | time only | OV | Supernews | real 5/6 decode to plausible 2006-07 time | not built |
| Nyuu ms Message-ID `([A-Z][a-z]){12}-\d{13}@nyuu` | order (ms, across groups); same upload | OV | Nyuu (NewsUP imitates the shape) | real: ms = NZB date 1,137/1,183 files | not built |
| Sibling-ID prediction + STAT (NxG sha256, newsmangler float, NewsUP drand48, Nyuu V8 / ngPost 32-bit seed solvers) | same upload | STAT per guess (NxG ~8 min seed search on 32 cores; newsmangler < 10 STATs/sibling) | nxg-upper, postie nxg mode, newsmangler, NewsUP, Nyuu, ngPost | src; real stock Nyuu: 2 IDs recover RNG state, up to 906 IDs linked (yEnc thread) | not built |
| Equal last-part :bytes / volume-size profile | same upload (blocking) | OV | RAR/7z volume uploads (50 MiB → 74 parts, last 102,400 B) | src; blocking only (payload report) | not built |
| Retry/fill tells (same part+pcrc32+begin under 2 IDs; nzb-repair begin=1 for part>1; nzbrefresh same ID, Date reset) | same upload / repair event | 1 body header per article | Nyuu, ngPost, nzb-repair, nzbrefresh | src | not built |
| postie partial md5(subject), md5(name+part) | same upload + real name | OV + predb dictionary | postie partial | src | not built |
| X-Received-Body-CRC | per-article dedupe | HDR | Highwinds, Omicron, Usenet.Farm | real headers | not built |
| Per-tool article size / bytes-per-line | same tool (veto) | OV | all: 716,800 ngPost/Nyuu; 768,000 postie (750,000 fallback), NewsUP, nxg, GoPostStuff | src; real Nyuu yEnc bytes 728-740K | part: :bytes kept, :lines dropped |
| From / Message-ID / header-set fingerprint | same tool; Nyuu default From = same uploader | OV (header set: HEAD) | all tools (table 2) | src | part: msgid domain in `domains`; 12 KEPT_HEADERS, order lost |
| PAR2 whole-upload size match (sizeChain) | same upload + file index | 1B/upload | PAR2 uploads (48% of NZBs; 77% at 3+ files) | sim: distinct sizes 1.00 whole; equal volumes .65-.80 | part (FileDesc parsed) |
| hash16k tie-break (MD5 first 16 KiB of article 1 vs FileDesc) | same upload + file index | 1B/file (0.04-0.96 per file, gated) | PAR2 uploads | sim: .24-.72 → 1.00 (1-8 concurrent); .92-.99 at 10% fetch failure | part |
| Per-connection key stitching | same session → same upload | OV; split: ~15 B/post | Diablo (pid), Giganews (w0?), INN (pid); Highwinds S looked per-upload in real data | sim: key ≈ 6% of a post; stitched .85 whole (P=8), .5 (P=32, .85 split) | not built |
| RAR volume header chain (volume no., flags, last-volume inner CRC; 7z .001) | same upload + order | 1B/file | RAR4, RAR5 without `-hp`; 7z first volume | src; `-hp` + no PAR2 + equal sizes: .65-.80 (sim) | not built |
| Takedown 451 canaries / co-removal | same upload (co-removal) | time: 4-6 canaries/file, STAT pipelined, BODY confirm; 1M STATs ≈ 0.15 GB | Giganews 451 documented; 430 ambiguous elsewhere | lit: OS3 2015 all-or-nothing, now often partial | not built |
| Repost identity | repost/twin | OV (postie same IDs) / 1B (PAR2 md5_16k+len, `=yend crc32`) | postie keeps IDs; Nyuu keeps ms+Date; ngPost new UUID; nzbrefresh same ID | src | part: main-file CRC in `release_probe.crc32` |
| Clock offset (RAR4 DOS local vs MKV DateUTC; RAR5 time flag) | same uploader (UTC offset, packer OS, mux-to-post latency) | 1B/upload (part01 article 1) | remuxed MKV in RAR | src; hyp | not built |
| Nyuu clock skew (msgid ms - arrival) | same uploader (weak) | OV | Nyuu | hyp (Kohno; flattened by NTP) | not built |
| Uploader profile (tiered settings hash, NewsUP group-pool MinHash, PAR2 creator, FP-STALKER upgrades) | same uploader | OV + 1B/upload | all; NewsUP pool in ~10 articles | lit: 3% of publishers ≈ 2/3 content; Timeprints needs hundreds of posts per alias; hyp | not built |
| Cross-backbone propagation vector | injection cluster | time (pollers on 5-6 backbones) | all | hyp, nothing published | not built |
| Fellegi-Sunter scoring + vetoed union-find | combines rows | n/a | all | spec weights: hash16k/MD5 decisive, CRC +20, PAR2 size +6, join +4..+8, session key +4; tool annotates only; hyp | not built |

## 2. Posting-tool fingerprints

| Tool | Message-ID | From | Date | Article size | Retry ID | Group policy | PAR2 default | Header set |
|---|---|---|---|---|---|---|---|---|
| ngPost | `<32hex@ngPost>`; obfuscated Subject = local part | `13 alnum@ngPost.com` (domain = msgid sig + .com); per session, per article when obfuscated | none; server stamps | 716,800 | new UUID (up to 5 retries) | EACH_POST; EACH_FILE (random group/file) with obfuscation | 0% (sample conf 8%) | From, Newsgroups, Subject, Message-ID only |
| Nyuu | `rand24-<13-digit ms>@nyuu` | real `$USER <$USER@hostname>` | build time, GMT; `--date` = one value per run | 716,800 | new letters, same ms + Date (unless `--keep-message-id`); post-check off by default | user's groups | none built in (payload report disagrees) | + Date, `User-Agent: Nyuu/x.y` |
| postie | `rand32@rand8.rand3` | `rand14 <rand14@rand5.rand3>` per article | RFC1123Z +0000; full mode now - U(0, 6 h) | 768,000 (750,000 fallback) | same Message-ID, 10 s-5 min | each_file | 10%, wait_for_par2 | From, Newsgroups, Subject, Message-ID, Date (fixed, per source); optional X-Nxg; full mode randomizes yEnc name per article |
| nxg-upper | `sha256(hdr:type:n)` as `40@21.3` hex, all derivable | random per upload: `hex15@hex15.hex3` or `rand12@rand8.rand3` (notes differ) | not recorded | 768,000 | same derived ID | not recorded | not recorded (RAR `-hp`, 153.6 MB volumes) | X-Nxg (constant value, or constant length per file: notes differ), empty Path |
| NewsUP | default none (server assigns, e.g. `32 letters@UFK`); generated `<sec><usec>.<b36>@<b36 time>.newsup`; obfuscated: imitates Nyuu, JBinUp, newsmangler, PowerPost (drand48 stream) | `NewsUP <NewsUP@somewhere.cbr>`; random 8 chars; empty when obfuscated | not recorded | 768,000 | not recorded | obfuscated: random group subset per article, crossposted | par2 -r15 -s768000 (7z -v50m) | not recorded; real yEnc name kept |
| GoPostStuff | `UnixNano$gps@gopoststuff` | not recorded | not recorded | 768,000 | not recorded | not recorded | not recorded (no packing) | From, Newsgroups, Message-ID, X-Newsposter, Subject; uppercase pcrc32 |
| newsmangler | `<float time>.<segment no.>@<provider host>` (float per session in real NZBs) | not recorded | not recorded | not recorded | not recorded | not recorded | not recorded | X-Newsposter: newsmangler |

## 3. Providers

Live provider probe completed on 2026-11-03 (`nntp-probe/results-2026-11-03.md`) across 9 configured logins:

| Provider family | Server ID | Decodable fields | Commands & empirical probe results (2026-11-03) | Remaining unknowns |
|---|---|---|---|---|
| Highwinds (`news.eweka.nl`, `news.newshosting.com` — identical counters) | `T.N$S.R@fxNN.dc` | T time (base64 0-9a-zA-Z_%, +876,645,500), N front-end counter, S session, fxNN front-end | `NEWNEWS` 400; `XHDR` non-overview returns `221` with no values; `Lines` empty on 55-64% of rows; 2-digit `Date` years on 55-64%; `DATE` skew `-0.56..-0.78 s`; `XFEATURE COMPRESS GZIP` 290; `XZVER` listed in `HELP` but hangs (timeout) | R field; share of uploads given server IDs |
| Giganews (`super.newsgroupdirect.com`, `news.frugalusenet.com` close numbering) | 32 b64 `@giganews.com` | XOR 0x9D → 6 LE u32: w2 time; w0 PID?, w1 counter?, w4 box? | `NEWNEWS` 502 (frugal 500); `XHDR` non-overview 501; `Lines` empty 0%; 2-digit `Date` years 41-46%; **`DATE` skew +5.74 s on Giganews** (`-0.36 s` on frugal); `XFEATURE COMPRESS GZIP` 290; `XZVER` 440/500 | w0/w1/w4 on a multi-segment upload |
| Newsgroupdirect / Viper (`news` / `viper.newsgroupdirect.com` — identical counters) | — | — | `NEWNEWS` 400; `XHDR` non-overview 400; `Lines` empty 0%; 2-digit `Date` years 60-68%; `DATE` skew `-0.33..-0.81 s`; `XFEATURE COMPRESS GZIP` 290; `XZVER` 400 | Server-assigned ID shape on binary groups |
| Diablo (Astraweb, UsenetServer upload, free.fr, Eweka uploadreader) | `hex$seq$pid$hosthash[$x]` | time, per-second seq, pid, front-end hash | `NEWNEWS` off; `XZVER` (Astraweb origin); gzip; engines don't sync numbers | Astraweb `$x` varies per article; UsenetServer +3600 s skew |
| INN | `radix32(t)$radix32(pid)$count` | time, pid, counter | OVERVIEW.FMT extras; adds Injection-Date | coverage on binary backbones |
| Supernews | base32 0-9a-v | first 7 chars = time | — | rest of ID; 13-char form |
| Usenet.Farm (`farm.newsgroupdirect.com`, nntpd) | 32 random letters | none | `NEWNEWS` 500; `XHDR` non-overview `221` with no values (`HDR` in `CAPABILITIES`); `Lines` empty 13%; 2-digit `Date` years 13%; `DATE` skew `-0.25 s`; **`XZVER` 224**; `XFEATURE COMPRESS GZIP` 500 | — |
| Abavia/nntpswitch2 (`reader.xsnews.nl`) | normalized | — | `NEWNEWS` 500; `XHDR` non-overview 503; `Lines` `"0"` on 64% of rows; 2-digit `Date` years 64%; `DATE` skew `-0.67 s`; **`XZVER` 224**; `XFEATURE COMPRESS GZIP` 500; synthesized ~400M article window | ID format; whether synthesized numbers follow arrival |
| Fill / bonus (`bonus.frugalusenet.com`) | — | — | `GROUP` 501, `LIST` 511 (cannot select or list groups; article-by-Message-ID lookup only); `DATE` skew `-0.87 s`; `XFEATURE COMPRESS GZIP` 290 | — |

## 4. Dead ends

| Idea | Why dropped |
|---|---|
| `NEWNEWS` single-group bisection | Disabled (`400`/`500`/`502`) on all 9 probed provider logins (2026-11-03 probe) |
| `XHDR`/`HDR` for `NNTP-Posting-Date`, `Injection-Info`, `Path`, or `X-Received-Body-CRC` | Every provider returns `221` with no values or `400`/`501`/`503` (2026-11-03 probe); costs one `HEAD` per article |
| Arrival from high-water-mark interpolation | ±10 s errors under bursts (sim 1) |
| Simultaneous-burst join across groups | Tools post files in series; join end-to-start |
| Cutting file runs from XOVER alone | Last part arrives early; runs .07-.65 exact, 0 whole |
| Sampling 1 in 5 yEnc headers | Whole uploads .71 → 0 (sim 3) |
| PAR2 part-count placement without headers | Failed (sim 3) |
| Header reads only in ambiguous seconds | 83% ambiguous; saves nothing |
| hash16k within one group / endpoint bisection | Worse than cross-group |
| Per-connection key alone | ~6% of a post per key, 0% whole |
| Time joins on top of content stitching | Lower precision (sim 5) |
| Whole-second NZB dates for the join | 38% of gaps < 2 s; precision ≈ chance |
| Plain transitive closure; `ena` crate | F1 .54 → 0 in literature; `ena` lacks vetoes, so own ~80-line union-find |
| Path / Injection-Info / NNTP-Posting-Host | Mostly stripped or rebuilt (a 2016 Giganews sample kept a host IP) |
| PAR2 Recovery Set ID as link key | Changes on rename |
| STAT/HEAD or LISTGROUP gaps for takedowns | Say present after BODY is gone (451); use BODY |
| Lumen notices as ground truth | No Usenet Message-IDs |
| Overview `Lines` field on any provider | Empty on 55-64% of Highwinds and 13% of Usenet.Farm rows, and `"0"` on 64% of XSNews rows (2026-11-03 probe); always use `:bytes` |
| Assuming 4-digit years in `Date` headers | 13-68% of overview rows on every provider use 2-digit years (2026-11-03 probe) |
| Uncalibrated cross-provider time joins | Giganews (`super.newsgroupdirect.com`) runs **+5.74 s** ahead on `DATE`; calibrate per-provider skew before joining |
| Sending `XZVER` to Highwinds servers | Listed in `HELP` on `news.eweka.nl` and `news.newshosting.com` but hangs (timeout); use `XFEATURE COMPRESS GZIP` (`290`) on Highwinds and `XZVER` (`224`) on XSNews / Usenet.Farm |
| yEnc `line=128` as tool tell | Default in all main tools |
| newsmangler-style `float.n@provider` as server IDs | Client-generated, not server |
| yEnc-name stitching for postie full | Name is random per article; use size + begin/end + hash16k |

## Live calibration and rollout

Full plan: `wave-10-B-live-calibration.md`.

- **Phase 0, instrument.** Persist per article: provider/backend, group, article number, Xref, msgid shape class, decoded server time/session/counter, Nyuu ms, :bytes, yEnc fields where a body is fetched, Date, NNTP-Posting-Date and Injection-Date. Export about 15 low-cardinality metrics on the LAN only, with no names or IDs in labels. Run the probe on each provider.
- **Phase 1, shadow (2 weeks).** The linker writes to shadow tables. The UI keeps the current grouping. Linking fetches are capped at ≤1 body per file and ≤5% of the download budget.
- **Phase 2, fit thresholds** on 80% of the ground truth: clear-subject uploads with labels masked, external NZBs, postie PAR2 real names, and Nyuu ms. The other 20% is held out.
- **Phase 3, promote one confidence level at a time**, after 7 days at precision ≥.98 (Proven ≥.995), recall at least 5 points above the current grouping on obfuscated labels, 0 Proven retractions, and ≤1 body per linked file.

## What the owner did & next probe step

- **Completed (2026-11-03):** The read-only provider probe (`nntp-probe/nntp_probe.py`) was run across all 9 configured provider logins (`nntp-probe/results-2026-11-03.md`), settling `NEWNEWS` (disabled everywhere), `XHDR`/`HDR` on non-overview headers (empty/refused everywhere), `Lines` (empty or `"0"` on up to 64% of rows), 2-digit `Date` years (13–68% of rows), `DATE` clock skew (**+5.74 s** on Giganews), and `XZVER` vs `XFEATURE COMPRESS GZIP`.
- **Optional follow-up:** `alt.binaries.test` was mostly Nyuu client-generated Message-IDs, so re-running the read-only probe on a busy obfuscated binaries group will sample server-assigned Message-IDs in the wild.

## Open questions

- Is Highwinds S per upload or per connection? 7 real NZBs suggest per upload, maybe single-connection posts. Verify on Atlas's own header log.
- Server-ID formats for UsenetExpress, Omicron, Abavia and Easynews: `alt.binaries.test` in the 2026-11-03 probe contained mostly Nyuu client IDs; re-run on a busy obfuscated binary group to sample server-assigned IDs.
- Giganews w0/w1/w4 meaning on a multi-segment upload.
- Does the RAR5 `-hp` salt repeat across the volumes of one set? If yes, it is a set key for encrypted uploads without PAR2.
- Conflicting notes: whether Nyuu writes PAR2 by default (ParPar is separate), the nxg-upper From shape, and whether the X-Nxg value is constant.
- Cross-backbone propagation timing as an injection fingerprint: nothing published; it would need pollers on 5-6 backbones.

## Files

All under `research/posting-behavior/`:

- `wave-NN-summary.md` (10) and per-agent reports `wave-NN-{A,B,C}-*.md`
- `sim/`: simulators (rounds 1-5), real-data checks (`w9b/`), red-team counts (`w10a/`)
- `server-ids/`: 72k-segment corpus and decoders
- `nntp-probe/`: the read-only probe and its offline test kit
- Shared summary: `research/linking-findings.md`, sections "posting behavior (Opus)"
- Peer final reports:
  - payload fingerprints: `research/payload-fingerprints/final-report.md`, `haiku15/final-report.md`, `haiku/final-report.md`
  - yEnc headers: `research/yenc-header-forensics/`

Claude Doc version: https://claude.ai/artifact/UADbR1wn9f5MGHs5TJRRHm

Caveat (added 12:10Z, from the yEnc-headers final wave): with overlapping uploads, time plus size shape alone gives only .27-.42 precision on real NZBs. Time joins propose candidates; Nyuu ID-stream continuity, PAR2 or archive headers confirm them.
