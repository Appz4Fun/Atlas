# Wave 2 A: arrival-curve join (07:17-07:26Z)

1. ngPost posts files strictly one after another (PostingJob.cpp 640-760; N connections pull one stream, ~2 articles buffered each). With each_file, group A gets file 1, group B file 2...: per-group rate is a square wave; the groups TILE in time (end of A ~ start of B, overlap ~N*2 articles), and their sum is a constant rate. Match END-TO-START, not concurrent bursts. Nyuu also reads files in series (async.eachSeries, lib/fileuploader.js). (4, high)
2. ngPost Message-ID is QUuid v4 (char 12 '4', char 16 [89ab]); Subject == that 32hex; From domain == signature.com. (4, high)
3. ngPost From may be predictable: built in connection threads with std::rand(), srand only on main thread. MSVC rand state is per-thread and seeds 1 if never seeded, so Windows builds may emit the same From sequence per thread: p1KaLmIg2kB7f, Zp5Tm1qbi6dss, 92C31aPGJK9Lv (computed, unverified). Linux: 32-bit seed, brute-forceable. (5, medium)
4. Nyuu ms timestamp in Message-ID; postie Date uniformly 0-6h before arrival; NxG 40@21.3. (4, high)
5. Numbering: INN ARTassignnumbers numbers all groups of a crosspost at once, in arrival order (innd/art.c ~1450). Multiple feeds/innfeed backlog leave temporary holes. Diablo multi-engine clusters do NOT sync numbers (Gnus nntp-xref-number-is-evil): sample high-water marks from one fixed backend per provider. Each provider numbers independently. Some report GROUP count = high-low+1 (no holes visible). No docs for Highwinds/Omicron/UE.
6. RFC 5537 3.5: injecting server MUST add Date if missing, so ngPost Date = arrival at uploader's provider, 1 s resolution.
7. Algorithms: Bayesian Blocks (Scargle 2013, doi 10.1088/0004-637X/764/2/167, port from astropy); BOCPD (arXiv 0710.3742, Rust crate `changepoint`); Kleinberg bursts; lead-lag via cross-pair correlation peak (arXiv 2601.01871).

Algorithm sketch:
1. Per (provider backend, group) record (HWM, time) every 30-60 s; interpolate arrival time per article number; prefer server-stamped Date where present.
2. Candidates: obfuscated subject matching a tool shape.
3. Bayesian Blocks per group -> "on" segments.
4. Graph of segments; edges across groups where one ends near the other's start (within N*2 articles of time) and bytes/sec match; bonus for shared From/Message-ID domain or Nyuu timestamp continuity.
5. Pick gap-free chains with constant total rate and ~1 group active at a time; confirm with yEnc part totals (one BODY of part 1 per file).

Next: test the MSVC per-thread rand hypothesis on real ngPost From values; measure reorder/delay at 2-3 providers; Nyuu xorshift128+ state recovery (ivanludvig.github.io/tech/google-random).
