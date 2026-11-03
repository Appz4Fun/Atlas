# Posting-behaviour linking: consolidated signal tables (waves 1-9)

Sources: `research/posting-behavior/wave-0{1..9}-*.md`, `log.md`. Payload keys (pcrc32/IFSC folds, salts, inner CRCs): see `payload-fingerprints/haiku15/final-report.md`. Atlas status: **not built** unless marked; **part** = stored but unused for linking, per `research/linking-findings.md` (catalog check against main a29ee52) and the payload report.

Read cost: **OV** overview row (free) · **HDR** · **HEAD** · **1B/file** · **1B/upload** · **AB** every body · **time** repeated probes.
Evidence: **sim** simulated · **real** checked on the 246 public NZBs / 72k segments · **src** read in tool or server source · **hyp** hypothesis.

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

Command support is from source and old dumps; no live matrix exists. `nntp-probe/nntp_probe.py` (read-only, tested on fake servers) settles it per account.

| Provider family | Server ID | Decodable fields | Commands known | Unknowns |
|---|---|---|---|---|
| Highwinds (UsenetServer, Newshosting, Easynews; same shape seen @giganews.com; Eweka in 2014-22 samples) | `T.N$S.R@fxNN.dc` | T time (base64 0-9a-zA-Z_%, +876,645,500), N front-end counter, S session, fxNN front-end | XZVER, XZHDR, XFEATURE COMPRESS GZIP (sends zlib), XHDR, LIST COUNTS; Lines empty, 2-digit-year Date; HEAD: X-Received-Bytes/Body-CRC, Org, NNTP-Posting-Date (not Easynews) | R field; NEWNEWS; share of uploads given server IDs |
| Giganews (+ white labels; Supernews numbering shared) | 32 b64 `@giganews.com` | XOR 0x9D → 6 LE u32: w2 time; w0 PID?, w1 counter?, w4 box? | XFEATURE COMPRESS GZIP; X-Trace, X-Usenet-Provider; 451 = DMCA | w0/w1/w4 on a multi-segment upload; NEWNEWS |
| Diablo (Astraweb, UsenetServer upload, free.fr, Eweka uploadreader) | `hex$seq$pid$hosthash[$x]` | time, per-second seq, pid, front-end hash | NEWNEWS off by default; XZVER (Astraweb origin); gzip; engines don't sync numbers | Astraweb `$x` varies per article (hash of IP/account/body?); UsenetServer +3600 s skew |
| INN | `radix32(t)$radix32(pid)$count` | time, pid, counter | NEWNEWS on; OVERVIEW.FMT extras; adds Injection-Date | coverage on binary backbones |
| Supernews | base32 0-9a-v | first 7 chars = time | — | rest of ID; 13-char form |
| Usenet.Farm | 32 random letters | none | XZVER; XFEATURE 500; X-Ufhash, X-Received-* | — |
| Abavia/nntpswitch (XS News, TorBox, Cheapnews, UseNeXT) | normalized | — | OVER/HDR/XZVER/XZHDR; non-overview HDR blocked (assumed from nntpswitch); NEWNEWS single group | ID format; whether synthesized numbers follow arrival |
| Omicron (Tweaknews), UsenetExpress | none found | — | Omicron Path ends path-stripped; UE opened IHAVE | ID formats; everything else (run `nntp-probe/`) |

## 4. Dead ends

| Idea | Why dropped |
|---|---|
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
| Overview Lines on Highwinds | Empty; use :bytes |
| yEnc `line=128` as tool tell | Default in all main tools |
| newsmangler-style `float.n@provider` as server IDs | Client-generated, not server |
| yEnc-name stitching for postie full | Name is random per article; use size + begin/end + hash16k |
