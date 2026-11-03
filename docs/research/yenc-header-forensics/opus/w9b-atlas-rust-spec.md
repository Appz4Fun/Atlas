# Wave 9B: Atlas Rust spec for the posting-tool linkers

Sources: opus/w4a, w5a, w5b, w5c, w6a, w6b, w6c, w7a, w8b, w8c-fingerprint-table.md, code in rng-tools/, nyuu-id-solver/, time-domain/, nzb-escape-test/, repost-test/, the sibling's X-Received-Body-CRC result (../log.md W6A, ../tools/xrbc/), and the sibling's ../raw/w08-c-rust-spec.md for classifier types. All facts come from those files. Tags: MEAS = measured on real NZBs/articles, SRC = read in tool source, SYN = synthetic test only, INF = inference.

Crate rule: std plus widely used crates only: `crc32fast`, `sha2`, `hmac` (geri check only, optional), `regex`, `base64`, `hashbrown` (optional; std HashMap is fine), `rayon` (optional, offline jobs; `std::thread::scope` is enough). Everything else below is our own code: GF(2) solver, xorshift128+, drand48, glibc/MSVC rand, base-36/base-64 codecs.

## 0. Step 1 of this wave (solver port)

`nyuu-id-solver/nyuuid_solve.py` now includes the w8b fix. After the first algebraic solution, it tries all 127 cache-block boundary shifts (−63..+63 states, using `cback`/`cstep`). It keeps the shift that links the most of the 40 IDs nearest in time to the solved pair. The regeneration window is now `len(ids)*150//64+64` blocks. The old `*24//64+60` was too small: it assumed 24 draws per article, but the real rate is about 48. The CLI is unchanged: `python3 nyuuid_solve.py file.nzb`. Check (MEAS):

| NZB | before (w6a) | after |
|---|---|---|
| Unicode_Mix_2023 | 164/172 | **172/172** (window fix only, the anchor was already aligned) |
| DDDD… (815 IDs) | 245/815 | **815/815** (anchor refined 2a2dc759… → 5ac85e9d…, plus the window fix) |

Both bugs mattered. Rust must use the w8b form: refine the alignment first, then size the window from the article count. The old script's "predicted next 3 IDs" line still steps 24 draws per article, so it only predicts back-to-back sends. In Rust, predict with the 48/24/72 gap set (§2.6).

## 1. Module layout

```
atlas-link/
  src/
    lib.rs
    model.rs          // OverviewRow, ArticleRef, PostBatch, LinkEdge, LinkKind, Evidence
    classify/
      mod.rs          // §3 classifier pipeline (steps 1-9), returns ToolLabel + parsed MID fields
      msgid.rs        // regex table (w8c §1-2), compiled once (regex::RegexSet + per-rule Regex)
      header.rs       // HEAD normalisation (w8c §4) + header-order fingerprint
      yenc.rs         // optional BODY first/last lines (=ybegin/=ypart/=yend tells)
    rng/
      xorshift128p.rs // V8 step/back, Stream with 64-entry LIFO cache, draw mapping
      gf2.rs          // u128-row Gaussian elimination, symbolic step table
      drand48.rs      // step/back, Perl rand(N), inverter
      glibc_rand.rs   // TYPE_3 srand/rand, linear fast-path (optional)
      msvc_rand.rs    // LCG 214013/2531011
    linkers/
      nyuu_v8.rs      // §2 (2 IDs -> state -> enumerate, rolling key scan, refine_alignment)
      nyuu_time.rs    // §4 ms-continuity orphan placement
      newsup.rs       // §5 drand48 linker
      ngpost.rs       // §6 seed search (offline job)
      highwinds.rs    // §7 (host,K,T1,N) session key
      twin_escape.rs  // §8 size-vector twin key (+ entropy gate, NZB-writer gate)
      twin_xrbc.rs    // §9 X-Received-Body-CRC twin key
      deterministic.rs// §10 nxg, geri, keimpema, counter-in-MID tools
    jobs/
      offline.rs      // queue for the ngPost seed search, cross-stream sweeps, HEAD sampling
  tests/fixtures/     // §12
```

Storage (one row per edge): `links(a_article_or_file, b, kind: LinkKind, score: f32, evidence: JSON)`, with `a < b` (the NZBHydra-style pairwise layout from ../log.md l.249). Streams and sessions get their own id tables: `nyuu_stream(anchor_s0 u64, anchor_s1 u64, first_ms, last_ms)`, `hw_session(host, k, t1_min, t1_max, n_min, n_max)`, `drand_stream(x_anchor u64)`, `ngpost_seed(seed u32, variant)`.

Where each part runs:

| Component | Stage | Trigger |
|---|---|---|
| Classifier steps 1-6 | per article, at XOVER ingest | always |
| Highwinds key | per article | label = Highwinds |
| Nyuu ms bookkeeping | per article (store ms), per batch (placement) | label = Nyuu* |
| Nyuu V8 solve | per post batch (≥2 Nyuu IDs within one From+group+time cluster) | new batch, or a batch grows past N new IDs |
| Nyuu V8 scan of orphans | per batch, against the unlinked-Nyuu-ID hash set | after a solve |
| NewsUP drand48 | per post batch | label = NewsUP own-style |
| Escape twin key | per file once the file is complete (or after a 4-hour hold) | full parts ≥ 4 and the entropy gate passes |
| XRBC twin key | when HEAD is sampled (opportunistic) | HEAD available |
| Deterministic IDs | per article (parse) + per batch (derive / STAT) | label in {nxg, geri, keimpema, counter tools} |
| ngPost seed search | offline job | label = ngPost, From sample stored |
| Cross-stream sweep (Nyuu) | offline, daily | new stream anchors |

## 2. Nyuu V8 stream linker

### 2.1 Purpose
Stock Nyuu on Node ≥ 12 (V8 ≥ 7.1). Two Message-IDs of one Nyuu process give the full 128-bit Math.random state. From that state Atlas can list every article ID of that process (all files of the post, and any other posts made in the same invocation, before and after) and predict IDs that were never seen. MEAS: 19 NZBs, 100% of IDs linked; 0 cross-NZB links among 14 distinct processes. Each Nyuu invocation is one fresh seed, so in practice stream = process = post.

### 2.2 Input
XOVER `message-id` (and `from`, `newsgroups`/xref, `date` for batching). Shape test: `^(?:[A-Z][a-z]){12}-([0-9]{13})@nyuu$`. The custom-host variant (`@PRiVATE` etc.) uses the same 24-letter prefix: `^((?:[A-Z][a-z]){12})-([0-9]{12,13})@`. Strict case alternation is required. NewsUP's fake `@nyuu` IDs fail it (w8c row 13).

Decode: `d[j] = byte[j] - (j even ? b'A' : b'a')`, giving 24 values in 0..25. The ms suffix is the build genTime, kept as `u64`.

### 2.3 PRNG model (exact; SRC V8 7.1+, verified in w6a/w8b)
xorshift128+ with state `(s0, s1)`, both `u64`:

```rust
fn step(s0: u64, s1: u64) -> (u64, u64) {          // returns new (s0, s1)
    let mut a = s0; let b = s1;
    a ^= a << 23; a ^= a >> 17; a ^= b; a ^= b >> 26;
    (b, a)
}
fn back(n0: u64, n1: u64) -> (u64, u64) {          // inverse of step
    let b = n0;
    let t = n1 ^ b ^ (b >> 26);
    let t = unshr(t, 17);                          // r = t; repeat 64/k+1 times: r = t ^ (r >> k)
    let a = unshl(t, 23);                          // same with <<, wrapping at 64 bits
    (a, b)
}
```

Output double = `(s0_new >> 12) / 2^52`, where `s0_new` is the first element after `step`. Mapping `floor(x*N)` = `((s0_new >> 12) * N) >> 52` in `u64`/`u128` integer math (N = 26 here, 62 for Nyuu-Obf). Integer and f64 disagree only when the f64 product rounds onto an integer boundary, about 2^-49 per draw. Ignore that.

64-entry LIFO cache: V8 refills by generating 64 states into cache[0..63] in order, then serves index 63, 62, …, 0. Define an **anchor** as the state just before a refill. Block k (k may be negative) is states 64k+1..64k+64 after the anchor. Served draw index d (relative to the anchor) comes from generation `g(d) = 64*(d div 64) + 64 - (d mod 64)` (1-based; use floor division for negative d). `Stream::draws(k0, k1) -> Vec<u8>` generates each block forward and pushes it reversed.

Nyuu draw use per article (SRC w7a): 24 at build (discarded), then 24 at each POST attempt (the published ID). Gaps between consecutive published IDs: 48 normally, 24 (back-to-back sends), 72 (retry), and rarely 96/120/144/192/576. One stream per process; all connections share it.

### 2.4 Solve (2 IDs → state)
Unknown: 128 bits `(s0, s1)` at the anchor. Precompute a symbolic table: for generations g = 1..320 and bit b = 0..63, a `u128` mask with `mask[g][b]` = the set of anchor bits whose XOR is bit b of `s0` after g steps. Build it by running `step` on vectors of 64 `u128` (shift = move words, xor = xor words). Table size: 320 × 64 × 16 B = 328 KB. Build it once, lazily.

Known bits per draw value v (N = 26): `lo = ceil(v*2^52/N)`, `hi = floor(((v+1)*2^52 - 1)/N)`. For j from 51 down: while bit j of lo == bit j of hi, emit the equation `bit (j+12) of s0 = that bit`. That is about 3-5 equations per draw.

For an ID pair (A, B) with assumed gap G ∈ {48, 24, 72, 96, 144} (try in this order) and cache offset o ∈ 0..63: draw positions are `o..o+23` and `o+G..o+G+23`. Add each equation `(mask[g(pos)][j+12], bit)` into a pivot table keyed by the highest set bit. Rows are `(u128, bool)`. Reduce on insert; a 0 = 1 row is a contradiction, so reject this (G, o). When rank reaches 128, back-substitute to get the state. **Verify** by regenerating blocks 0..3 and comparing both 24-draw strings exactly.

Cost: ≤ 5 × 64 eliminations of about 200 rows × ≤128 u128 xors each, which is microseconds to about 1 ms in Rust. Python needs 0.1-1 s (MEAS).

Pair choice: sort the batch's IDs by ms. Try time-adjacent pairs first (up to 40), then all ordered pairs of the first 6. One ID alone gives about 113 bits, which is not enough.

### 2.5 Alignment refinement (mandatory; w8b and §0)
The algebra can return an anchor that is off by a few states. Such a wrong anchor still reproduces any ID lying inside one cache block, so it links only part of the post. Fix: step the solved anchor back 63 states. Then, for each of 127 shifts, link the 40 IDs nearest in ms to the solved pair, using a margin of `40*150/64 + 8` blocks, and keep the shift with the most links. Cost: 127 × about 100 blocks × 64 steps ≈ 0.8 M steps ≈ 3 ms.

Canonical anchor: after linking, move the anchor to the block boundary at or below the earliest linked draw. Store it as `nyuu_stream(anchor_s0, anchor_s1)`. Copies of the same post re-exported to different NZBs give the identical anchor (MEAS: 3 pairs), so it is also a dedupe key.

### 2.6 Enumerate / link / predict
- Window: from block `-margin` to `+margin`, with `margin = n_ids*150/64 + 64` (about 48 draws per article plus retries).
- Rolling 24-draw key: `key = Σ d[i+j]·26^(23-j)` as `u128` (26^24 < 2^113). Roll it with `key = (key - d_out·26^23)·26 + d_in`.
- Look each window key up in `HashMap<u128, ArticleIdx>` built from the batch's IDs. Cheaper prefilter, as in stream_link.py: an 8-draw prefix in a `HashSet<u64>`.
- Store `draw_index` per linked article. Draw order equals ms order with 0 inversions (MEAS), which double-checks the result.
- Repeat the solve on IDs that are still unlinked, to find more streams in the same batch (stop at 8).
- **Predict** (missing segments, never-indexed articles): for each gap in `draw_index`, the published ID of the next article sits at last+48 (or +24, +72 for retries). That gives about 2 candidate 24-letter keys per article plus 1 per retry. The ms suffix is unknown; it lies between the neighbours' ms (median gap 51 ms, p90 268 ms). A STAT of an ID needs the full ID, so this costs (Δms+1) STATs per key. Run it only on demand, e.g. to fill a known hole.
- **Index-wide orphan scan** (offline): keep a global `HashSet<u128>` of unlinked Nyuu keys. For each stream, generate G keys and probe. Cost O(G + N). Memory: 16 B per key, or 8 B with a 64-bit fingerprint (top 64 bits of the key; FP 2^-64 per probe, confirm on the full key).

### 2.7 Cost / false links
- Stepping: about 4 ns/state in C (MEAS, xs_kernel), similar in Rust. A 16,486-article post is 791k draws ≈ 3 ms.
- A false 24-draw match has probability 26^-24 ≈ 2^-112.8 per comparison. Even G·N = 10^30 gives effectively 0 false links.
- Main risk is a wrong label, not a false key match, because verification is exact.
- Cross-process sweep (stepping each anchor W states into a hash of all anchors) found 0 links across 14 processes at W = 10^9 (80 s). Run it monthly at most. It only pays off for multi-post single invocations or long-running Nyuu-as-library daemons.

### 2.8 Variants
- **Nyuu-Obfuscation** (SYN only): the same engine with N = 62 and alphabet `A-Za-z0-9`. Observations are Subject (32) + From (14+14+5+3 = 36), 68 consecutive draws of one article. Gaps: 155 normal, 156 at a new file, 112 at queue edges, 198 at a retry; wire ID is 149 (Q=1) or 304 (Q=2) after its Subject. Subject alone is rank-deficient, so do not ship the variant on Subject alone. Feature-flag it until it is confirmed on a real post.
- **Old V8 (Node ≤ 11, posts 2017-2022)**: output = `((s0 + s1) mod 2^52)`. Not linear (carry chain), and z3 timed out (w7a). Fall back to §4 time continuity and the fingerprint label. Do not schedule solves for these: if two time-adjacent IDs fail all 5 gaps × 64 offsets, mark the batch `v8_old_or_unknown`.
- `${rand(N)}` wrapper tokens (transmitter etc.) add extra draws per article. Widen the gap set or link only via §4.

### 2.9 Test fixtures
`nyuu-id-solver/w8b-stream-link-results.json`: 19 NZBs with expected anchors, link counts, and gap histograms. Expected anchor for Unicode_Mix_2023: `06692efe25a557bc 92e81233dd4f0e9f`, 172/172. `w8b_synth.py` (A→B link at 506,048 steps, C unlinked). NZBs live in the public reference-NZB repo clone (scratchpad w6a/dl/sjo). Copy 3 small ones into `tests/fixtures/nyuu/` (Unicode_Mix_2023, DDDD, Subtitles_1234).

## 3. Classifier (tool label from XOVER)

Use w8c §5 order, with sibling types from ../raw/w08-c-rust-spec.md (`ToolLabel`, `HighwindsId`, `is_fixture_host`). Implementation:

1. `regex::RegexSet` over the MID (without `<>`) for all w8c §1/§2 patterns. On a multi-hit, take the lowest rule index. Order: fixture filter → server families (Highwinds, Diablo, INN, Giganews, `@UFK`) → literal domains (`@nyuu` strict, `@ngPost`, `$gps@gopoststuff`, `@gpoststuff` = NewsUP fake, `@JBinUp.local` with/without `_NoM`, `@JBinDown.local`, PowerPost 20 vs 22 chars and N ≤ M, `.newsup`, `@riverup`, `.geri`, `@spot.net`, `[0-9]{10}\.local`, `@PRiVATE`) → structured shapes (newsmangler, keimpema `part N of M.hex32`, nxg 40@21.3, dashed UUIDv4) → ambiguous random (32@8.3 alnum: postie vs Nyuu-Obf; 32-hex: v4 nibble test chars 13 = '4' and 17 ∈ [89ab] → ngPost-custom; host = known NNTP server → nntpPoster; else pesto/unknown).
2. Server families label the injector and keep going for a tool label. Store both.
3. Free XOVER cross-checks (w8c step 6). Each is a `bool` evidence bit on the label:
   - From shape;
   - Nyuu: |Date − ms/1000| < 1 s, unless the Date was rewritten by Usenet.Farm (2-digit year + "UTC");
   - Subject without "yEnc" → NewsUP/PowerPost;
   - ngPost `-x`: Subject == MID local part;
   - `:lines` against preset part sizes (w4c table).
4. HEAD (step 7) and BODY (step 8) only when the label is still ambiguous: one article per file, pipelined.
5. Output: `ToolLabel` + parsed fields (`nyuu_ms`, `hw: HighwindsId`, `newsup: (N_str, r_b36, host_b36)`, `part/total` from counter MIDs) + `link_dispatch` (w8c step 9).

Cost: one RegexSet match per row (about 1 µs) + field parse. Memory: compiled regexes. False-label rate (MEAS, ../wave07): known grammars cover 86% of 72k segments. Misclass risk is low: (a) 32-hex IDs, (b) 32@8.3 without HEAD, (c) NewsUP imitations; the strict checks above separate these.

Fixtures: ../raw/w08-c-rust-spec.md table rows (example MIDs + expected labels); `posting-behavior/server-ids/segs.tsv` (72,334 segments; the w07 classifier result is the regression baseline); w8c conflicts X1-X21 as unit tests for the version-dependent rules.

## 4. Nyuu ms-timestamp continuity (orphan placement)

Purpose: place orphan Nyuu articles or files into their post even when V8 cannot solve (old V8, wrapper extra draws, a single orphan ID). The ms suffix is the article build time.

MEAS (101 NZBs, 93 uploads):
- 0/57,093 time inversions within a file;
- time rank == segment rank, 59,837/59,837;
- 0/2,651 adjacent-file overlaps; 0/92 upload-span overlaps;
- leave-one-out: a removed middle file's [min, max] fits exactly into the gap, 2,565/2,565.

Algorithm:
- Per (From, group-set) bucket, keep a `BTreeMap<u64 /*start_ms*/, FileSpan{end_ms, file_id, post_id}>`.
- An orphan file with span [a, b] is placed into post P if there are consecutive files F_i, F_{i+1} of P with `F_i.end < a` and `b < F_{i+1}.start`. If no post's file covers [a, b], its segment ranks must also equal its time ranks.
- A single orphan article with ms t goes to file F if `F.start ≤ t ≤ F.end`. Its segment number must be consistent with rank interpolation: find the neighbours by ms and require `seg_prev < seg < seg_next`.
- Tie rule: if two posts in the bucket claim the same gap, do not place. With mixed posters, or From randomised per article, use group + Date only and require gap width < 10 s.
- Score: `1 - (gap_slack_ms / gap_width_ms)`.

Data: about 24 B per file span. Cost: O(log n) per placement. Stage: per batch, re-run when the 4-hour completion hold expires.

False-link rate: within one uploader bucket, 0 in the leave-one-out test. Across uploaders sharing a group and posting at the same time it is higher. Concurrent Nyuu runs from different machines are not measured. Combine with V8 (exact) whenever the solve works.

Extras:
- NZB `<file date>` == first segment's ID second for 87/93 uploads. Use as an NZB truth check.
- Negative boundary gaps flag merged NZBs.
- Throughput (median 4.4 MB/s) and gap distribution (median 51 ms) form a coarse session fingerprint (INF for cross-machine use).

Fixtures: `time-domain/p1.py`, `p1b.py`, `p1c.py` (leave-one-out) and `p2_*.txt` outputs.

## 5. NewsUP drand48 inverter

Purpose: link all own-style NewsUP IDs of one Perl process (and its random From) from ONE ID.

Input: MID `^([0-9]{11,16})\.([0-9a-z]+)@([0-9a-z]+)\.newsup$`. Decode case-insensitively: the regex in w8c uses lowercase while newsup_sim.py uses uppercase B36, so accept both (conflict X6).
- `st` = decimal `"$sec$usec"`, usec not zero-padded; N = st as f64.
- r = base-36 **least-significant digit first**.
- host = b36(st), also LSD-first. It must re-encode to the same st: a free self-check.
- 2017-06 era: host = `$time` decimal, not base 36.

PRNG (Perl ≥ 5.20 internal drand48; INF that real Perl matches, not run on real perl):
- `X' = (0x5DEECE66D·X + 0xB) mod 2^48`; inverse multiplier `A^-1 mod 2^48`.
- u = X/2^48 (exact in f64).
- `rand(N)` = u·N in f64, and `int()` truncates.
- `srand(s)`: X0 = `(s as u32) << 16 | 0x330E`.

Draws per own-style ID: 2 (`int(rand(@gens))` before each ID, even when not obfuscating). IDs come back in Perl hash order, so draw order ≠ NZB order.

Inversion:
- Candidates are X in `[floor(r·2^48/N) − 2, ceil((r+1)·2^48/N) + 2]` filtered by `int(u(X)·N) == r`, using f64 exactly as Perl does (`(X as f64 / 2^48) * N`, then `trunc`). Do not use integer math here.
- 16-digit N: mean 1.07 candidates (37/40 unique).
- 11-digit N: about 16k candidates. Cap at 5 M; above that, wait for a sibling.

Link (Rust form; better than the Python O(W·T) loop):
- For every other ID in the batch, compute its candidate set and insert into `HashMap<u64 /*X*/, IdIdx>`. Long-N IDs have about 1 candidate each.
- From each candidate X of the seed ID, step forward and back up to W = 20,000 (siblings sit within ±200) and probe the map per step. Cost O(W) per candidate.
- For short-N targets, test `int(u(X)·N_t) == r_t` directly over a ±400 window.

Result: stream id = canonical X (the state at the earliest linked ID). Also:
- Regenerate the random From `rand8 <...>` and compare (40/40 SYN).
- Seed backtrack (offline, optional): step back until `(X & 0xFFFF) == 0x330E` → seed = X >> 16 and the call count. About 1 false hit per 65k steps, so confirm it (e.g. the seed should be time/pid-like).
- 24 `keys %alpha_chars` orders affect only which punctuation char appears in random strings. Try all 24 when regenerating a non-alnum From.

Cost: microseconds to milliseconds per batch; memory is tiny. False links: SYN 0/decoys at a ±200 window. For short N, one-ID matches have chance ≈ W·N/2^48 per target; for 11-digit N and W = 400 that is about 2^-28. Require 2 linked IDs before merging posts.

Fixtures: `rng-tools/newsup_sim.py` (simulator + inverter; generate golden vectors by script into `tests/fixtures/newsup.json`) and `seed_backtrack.py`. No real own-style NZB in the corpus: the 2017 ubuntu-mate NZB is `@UFK`, which is server-assigned, so do not run drand48 on it.

## 6. ngPost srand seed search (From/group prediction only; offline)

Scope: recover the `std::rand` seed only to predict/link the **From** stream (`-x` per article, or one per job) and random **group** picks (`PostingJob.cpp:739`, `NgPost.h:531`). Message-IDs are QUuid (OS CSPRNG) and cannot be linked. Archive passwords are explicitly out of scope: do not generate or store password candidates.

From generation: 13 × `AL[rand() % 62]` with `AL = "A-Za-z0-9"` (that exact order), + `@ngPost.com` (≤ v4.16) or `@<signature>.com` (HEAD).

Seeds:
- ≤ v4.14: `srand((uint)currentMSecsSinceEpoch)` → a ms-epoch truncated to u32 near the post.
- ≥ v4.15: `srand(QUuid::createUuid().data1)` → 32 bits.
- Windows releases (MinGW, Qt 5.12.5): poster threads use the per-thread msvcrt state, which is never seeded → seed 1 (INF).

glibc TYPE_3 `rand()` (verified vs glibc for 7 seeds × 100k outputs):
```
r[0] = seed (0 -> 1)
r[i] = (16807 * r[i-1]) mod 2147483647, i = 1..30, in int32 Schrage form:
       hi = r/127773; lo = r%127773; w = 16807*lo - 2836*hi; if w < 0 { w += 2147483647 }
r[i] = r[i-31], i = 31..33
discard 310: r[n] = r[n-31] + r[n-3] (wrapping u32), n = 34..343
output k:    v = r[n-31] + r[n-3] (wrapping), store, return v >> 1
```
MSVC: `x = x*214013 + 2531011 (u32)`; output `(x >> 16) & 0x7FFF`; initial x = seed (default 1).

Seed-1 tables (fixtures):
- MSVC: `p1KaLmIg2kB7f, Zp5Tm1qbi6dss, 92C31aPGJK9Lv, k7DMPDONXrwD2, yVLZrHBio74so`.
- glibc: `pkDHTxmMR18N2, l9k88EmLgN7cC, CTt9rWksb1fEB, w397vi5Ug1YHC, 3UAVUAoB9VYjC`.

Search (offline job `jobs/offline.rs`):
1. Seed-1 lookup (online, free): `HashMap<[u8;13], usize>` of the first 100k seed-1 Froms for MSVC and glibc. A hit labels the process as "unseeded Windows thread" and gives its position.
2. ≤ v4.14 ms window: seeds `[Date − run_length, Date]` in ms. A ±1 day window = 2^27.4 seeds took 48.8 s on 4 threads (about 6 s on 32 cores). Realistic windows are minutes, which is sub-second.
3. ≥ v4.15 full 2^32: index 0 only ≈ 1.75 core-h (about 200 s on 32 cores). Within the first 1000 Froms of the stream ≈ 59 core-h (about 1.9 h on 32 cores). Prefilter on the first char.

Optional speed-up (INF, untested): r[i] = seed·16807^i mod p for seed in [1, p−1], and the 310 discards plus outputs are linear over Z/2^32 in r[0..33]. So output k = Σ c_{k,i}·r_i mod 2^32 with precomputed c. That would take index 0 to about 40 mul-adds per seed. It needs special handling for seed ≥ 2^31 because of the int32 cast, so verify it against the reference above before use.

Once a seed is found:
- Regenerate the From/group stream forward.
- Link every article whose From is in it (`-x` mode: per article; normal mode: one per job, so this links jobs of one process).
- Retry = +13 draws.

Storage: `ngpost_seed(seed, variant {glibc, msvc, ms}, position)`. False link: 13 chars of 62 ≈ 2^77.4 per comparison. Over 2^32 seeds × 1000 positions ≈ 2^42 trials, the expected false hits are about 2^-35. Safe.

Fixtures: `rng-tools/ngpost_rand.c` and `msec_window.c` (reference + benchmark; port their self-test vectors). ngPost NZBs in the w6b corpus (54 files) supply real Froms, but no seed is known.

ngPost fork v5.6+ (mt19937 variant): per-call mt19937 reseed, so the From cannot be linked. The yEnc name comes from a thread_local mt19937 seeded with a 32-bit random_device (deterministic on old MinGW), giving a 2^32 brute force over mt19937 → a later phase, needs BODY.

## 7. Highwinds fxNN server-ID session key

Regex: `^([0-9A-Za-z_%]{4,6})\.([0-9]+)\$([0-9A-Za-z_%]+)\.([0-9]+)@(fx[0-9]+\.[a-z]+[0-9]*)$` → T1, N, K, R, host.

T1 decode: little-endian base 64, alphabet `0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_%` (lowercase before uppercase). `unix = Σ val(c_i)·64^i + 876645500`. Check: `EaPmB` → 1503379236 (computed again here: 40 + 10·64 + 51·64² + 22·64³ + 37·64⁴ + 876645500).

Session: key `(host, K)`.
- K is constant per post; 7 distinct K over 7 NZBs (MEAS).
- N = front-end article counter: +1 per article with 0-43% gaps (other users' articles interleave).
- R is random and ignored.

`same_session(a, b)`: same (host, K), |T1a − T1b| ≤ 3600 s, and N within [n_min − slack, n_max + slack] with slack = 2·(articles so far) or 10^4.

Data structure: `HashMap<(host, K), Vec<Session{t_min, t_max, n_min, n_max, post_ids}>>`. Split a (host, K) entry into a new Session when the T1 gap exceeds 6 h (K reuse over time is unmeasured; INF).

Cost: O(1) per article at ingest. The key survives Subject/filename obfuscation, so it links obfuscated files of one upload and cross-group copies of one upload session.

False-link rate: unknown. K lengths of 3 or more chars from a 64-symbol alphabet give ≥ 2^18 values per host, and the T1 window cuts collisions further. Measure on a large XOVER pull before trusting it alone. Until then, require (host, K) plus T1 within 1 h plus monotone N.

Fixtures: ../raw/w08-c-rust-spec.md rows 6-7 (`EaPmB.546176$JP7.228032@fx09.am4`, `FaPmB.546178$JP7.180477@fx09.am4`, same session), plus 869 Highwinds segments over 7 NZBs in segs.tsv.

## 8. Escape-count / segment-size-vector twin key

Purpose: link two uploads of the **same file bytes** with different Message-IDs (reposts, other uploader, other indexer). yEnc escape count is a content hash visible in overview size data. MEAS: a real repost 19.4 h apart matched 15/15 segments exactly; a control with regenerated content did not match.

Model (w4c/w5c), P = part payload, L = line length:
- `lines ≈ ceil(P·(1+e)/L) + 3`, with e ≈ 1.58% for random data at L = 128;
- body bytes ≈ P + esc + 2·lines + ylines_len (+3 for Nyuu NZB bytes);
- for random content, the per-part esc sd ≈ sqrt(P·p(1−p)) ≈ 105 B at P = 716800.

Per-part observable `b_i`, by source:
1. **Nyuu NZB `<segment bytes>`** (body + =y lines, no NNTP headers; residual 3 exact): `esc_i = bytes_i − P − 2·ceil(P/L) − len(=y lines) − 3`. Exact.
2. **XOVER `:bytes`** (whole article, server-specific): `b_i = :bytes − Σ len(overview header fields + names + ": " + CRLF)` = body + C. C is the injected-header constant, the same per server and session (INF). Compare only within one server, and use **first differences** `δ_i = b_i − b_{i−1}` (full parts only), which cancel C and also the =ybegin name-length constant. So the key survives renames, unlike §9.
3. **`:lines`**: about 1.76 bits per part only; tie-breaker.

Gates:
- **NZB-writer gate**: decide the `bytes=` family per NZB before using it.
  - Nyuu = body-only family (exact).
  - ngPost = flat 716800 (decoded size): **no information, exclude**.
  - SABnzbd/NZBGet/indexer re-writers = XOVER family (header-inclusive), use δ.
  - Raw/decoded family: constant per part, so use only the last-part remainder (= file size mod P).
  - Detect via w6c NZB writer signatures (XML decl case, DOCTYPE, comment, attribute order, indent) and the flat-716800 test.
- **Entropy gate**: within a file, require mean estimated escapes ≥ 5000 per part (at P = 716800; scale with P) **and** a per-file sd of b_i in [50, 250]. Zero-filled or low-entropy RARs have about 0 escapes and are useless (w5c, w6b pair B/D). Store the gate result as a flag.
- **Preset gate**: same P and L. Estimate them via `P_est = (bytes − C)/1.0158` and `L_est = P_est·1.0158/(lines − 3)` (w4c), or from the classifier label.

Key and index:
- Per file: `Vec<i32>` δ sequence (full parts in part order; order comes from subject `(i/n)` or the NZB segment number).
- Index: rolling `u64` hash (e.g. FxHash-style written by us, or `std::hash::DefaultHasher`) of each window of k = 4 consecutive δ → `HashMap<u64, Vec<(file_id, offset)>>`.
- Candidate pairs: verify by the longest equal run. A link needs a run ≥ 4 and ≥ 50% of the shorter file's full parts.

False-link rate (MEAS, 62,759 unrelated random-content file pairs): per-segment equality 0.22% (theory 0.28%); k consecutive ≈ 0.0028^k, below 10^-10 for k ≥ 4. All 39 full-sequence equal pairs were zero-escape files or shared Message-IDs, which the gates remove.

Cost: 4-8 B per part; index ≈ 16 B per window. Stage: per file at completion. Not yet supported: matching across different P/L (would need byte-range alignment rather than segment index).

Fixtures: `nzb-escape-test/an*.py` + `out1.txt` (104 NZBs / 69,181 segments); `repost-test/cmp.txt` pair A (positive), pair C (negative), pairs B/D (low-entropy, gate must reject).

## 9. X-Received-Body-CRC twin key

Definition (sibling, MEAS on 12 real articles):
`X-Received-Body-CRC = crc32_ieee(body_with_CRLF_line_endings ++ b".\r\n")`.
- Body = everything after the header/body blank line; headers are excluded.
- The value is printed in unpadded decimal.
- Dot-stuffing is untested; assume the stuffed wire form.
- Rust: `crc32fast::Hasher`.

The header is stamped at injection by Highwinds-family / Usenet.Farm servers and travels with the article (same value on two backbones; lean F in conflict X12). X-Received-Bytes is only approximate.

For Nyuu NZBs, `<segment bytes>` equals exactly the length of the CRC input, so `(len, crc)` is a 64-bit-ish per-part key, joinable from NZB + HEAD.

Twin semantics: identical CRC means identical body bytes, which requires the same encoder, the same yEnc `name=`, the same part boundaries, and the same payload. So it links exact re-injections, crossposts with distinct Message-IDs, and same-tool reposts of the same file **without** renaming. A repost with a renamed or randomised `name=` breaks it; §8 survives that case.

Access: HEAD (pipelined; about 1 KB per article; 1M articles ≈ 1000 s at depth 50, 50 ms RTT). Or `XHDR X-Received-Body-CRC` if the provider allows it (unverified; probe once per provider on a small range; RFC 3977 lets servers return 503).

Sampling plan: per file, take the first full part and the last part (2 × 32 bits), plus the middle part for files with ≥ 20 parts.

Index: `HashMap<u32, Vec<ArticleRef>>`. Candidate twins need ≥ 2 parts with equal CRC at the same part numbers and equal part sizes.

False-link rate: a single 32-bit CRC collides by chance in a 10^9-article index (birthday). With 2 matching parts at the same positions plus matching `:lines`, FP ≈ 2^-64 per file pair. Never link on one CRC alone, except when also requiring identical `:bytes`/`:lines` and same file size, and then only as `LinkKind::TwinWeak`.

Extra use: after BODY, recompute the CRC to verify article integrity. Normalise header names case-insensitively (TorBox serves `X-Received-Body-Crc`).

Fixtures: `../tools/xrbc/run6.py` + samples (e.g. upd.txt: 961135873 = crc32(body_crlf + ".\r\n"); 2262295484 without the dot line). Turn these into `tests/fixtures/xrbc/*.eml` with expected values.

## 10. Deterministic-ID tools

| Tool | Rule | Atlas action | Fixture |
|---|---|---|---|
| **nxg (reference nxg / postie nxg mode)** | `MID_n = hex(sha256(NXGHEADER ":" type ":" n))` with type ∈ {data, par2}; split h[0..40] @ h[40..61] . h[61..64]. Obfuscated Subject = hex(sha256(H)). From from P = hex(sha256(Subject)): `P[10..15] <P[10..25]@P[30..45].P[50..53]>`. NXGHEADER = base64("<21-char rand>:<data count>:<par2 count>") | Self-checks from XOVER alone (`sha2`): (1) From == f(Subject); (2) when the MID is known, Subject == sha256(H). These link all articles of one NXG post and confirm the label (FP ≈ 0). With the NXG header (from an nxglnk or `X-Nxg`), derive every data/PAR2 MID and STAT the gaps. | build vectors with our own script from the w2c rule; ../log.md l.75 |
| **nxg-upper** | same 40@21.3 shape, but the key is the Comment (incompatible derivation); `X-Nxg` = hex(nonce12 ‖ AES-GCM(fileNo:total:name:type:size)) every article; no Newsgroups header; LF-only headers | Label only (FP); try the reference nxg derivation first, then fall back to label + HEAD tells. Decrypting X-Nxg is out of scope. | w8c row 7 |
| **geri** | `b64url(HMAC-SHA256(key, "GERI2"‖enc‖file‖part‖total‖SHA256(body))) @ <pcrc32 8 hex>.geri` | The domain is the **decoded part CRC32 in clear** in overview: a tool-independent content key per part (same as yEnc pcrc32). Twin key = (pcrc32 sequence, part size). The key is unknown, so the local part is used for dedup only (server 435/441). | w8c row 28 (SRC only) |
| **keimpema / Spottarr .NET** | `part{N}of{M}.{guid32}@{base}`, one GUID per file | Group by GUID → whole file (intra-file DET, FP ≈ 0); N/M gives part index and total without the Subject. | w8c row 29 |
| Counter MIDs (free, same parser) | PowerPost `partNofM.{20}@powerpost2000AA.local` (N ≤ M), JBinUp `_NoM@JBinUp.local`, JBinDown `.N-M@`, yEncBin `PartNofM.HEX32@ts10.local` (TIME + counter; HEX32 per file INF), newsmangler `epoch.10us.part@host`, newzNZB `unix.SESSION.part@PRiVATE` (SESSION = upload session) | Parse part/total (self-check against NZB segment number) and the session token; group by it. | wave07 segs.tsv (yEncBin 2,816 segs, PowerPost 1,572, newsmangler 895) |

Cost: one SHA-256 per article for the nxg check (about 1 µs). Stage: per article (parse/self-check), per batch (derive/STAT).

## 11. Phased rollout (cheapest and highest-yield first)

| Phase | Ships | Needs | Yield (corpus) | Effort |
|---|---|---|---|---|
| **P1** | Classifier steps 1-6 (§3), Highwinds key (§7), deterministic parsers (§10: keimpema GUID, counter MIDs, nxg self-check), Nyuu ms bookkeeping | XOVER only | label for about 86% of segments; Highwinds sessions; exact intra-file groups | small: regex + parsers |
| **P2** | Nyuu V8 linker (§2), with alignment refinement and orphan scan | XOVER only; rng/xorshift + gf2 | 100% of IDs in every new-V8 Nyuu post (Nyuu = largest family: 23.8k/72k segs); exact, FP ≈ 2^-113 | medium; port stream_link.py |
| **P3** | ms-continuity placement (§4) for old-V8 / wrapper Nyuu; NewsUP drand48 (§5) | XOVER | orphan files 2,565/2,565 in leave-one-out; NewsUP own-style full stream from 1 ID (SYN) | small |
| **P4** | Escape twin key (§8), with NZB-writer + entropy gates | XOVER `:bytes`/`:lines` (+ NZB bytes) | cross-upload twins (repost confirmed) | medium; needs per-server C handling |
| **P5** | XRBC twin key (§9) + HEAD sampler; HEAD/BODY classifier steps 7-8 | HEAD (pipelined), XHDR probe | exact byte twins; tool label for faked/server-assigned MIDs | medium; I/O cost |
| **P6** | Offline jobs: ngPost seed search (§6), Nyuu cross-stream sweep, Nyuu-Obf variant (behind a flag until real-post validation), ngPost-fork mt19937 | CPU budget (core-hours) | ngPost Froms/groups per process; rare cross-NZB Nyuu links | medium; batch infra |

Not planned: old-V8 Nyuu solve (needs a SAT/guess-and-determine attack, w7a), `@UFK`/CSPRNG IDs (postie random, ngPost QUuid, pesto, usenet-posting), archive passwords.

## 12. Fixture checklist (`tests/fixtures/`)

- `nyuu/`: 3 NZBs + expected anchors and counts from w8b-stream-link-results.json; synthetic pair from w8b_synth.py (A→B at 506,048 steps); xorshift step/back 1000-step round trip.
- `nyuu_time/`: span lists from time-domain p1c leave-one-out (expected 100% placement).
- `newsup/`: golden IDs + states from newsup_sim.py (16-digit and 11-digit N cases, decoy IDs).
- `rand/`: glibc TYPE_3 and MSVC seed-1 From lists (§6); `ngpost_rand.c` self-test vectors.
- `highwinds/`: the two fx09.am4 IDs (same session) + one with a different K.
- `escape/`: repost pair A (positive), pair C (negative), pairs B/D (gate rejects), ngPost flat-bytes NZB (writer gate rejects).
- `xrbc/`: the 12 matched articles from tools/xrbc with expected CRCs.
- `classifier/`: w08-c-rust-spec example table + segs.tsv baseline counts.
