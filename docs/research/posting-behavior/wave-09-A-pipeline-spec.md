# Atlas cross-group linking spec

Tiers: **T0** overview · **HDR** · **HEAD** · **1B** one body per file or run · **AB** every body.

Overview: `num, msgid, bytes, subject, from, date, group, xref`.

## Time rules

- **Never join on the poster's Date.** postie randomizes it 0-6 h back, and Nyuu `--date` makes it constant.
- **Use the article number plus Injection-Date/NNTP-Posting-Date** (HDR). Date is allowed only when the tool fingerprint shows the server added it (ngPost omits Date).
- **Read numbers from one fixed backend per provider.** Diablo engines don't sync numbers, and Abavia synthesizes them.
- **Within-second rank:** `t = sec + (rank_in_sec + 0.5)/n_in_sec`, ranked by article number. Raw 1 s timing drops recall from .94 to .77.

## Stages

| # | Stage | Inputs | Tier | Outputs |
|---|---|---|---|---|
| 1 | Dedupe | msgid, xref | T0 | One row per msgid (postie reposts and retries keep the ID). The same (file, part, pcrc32, begin) under two IDs is one segment (Nyuu/ngPost mint new IDs) |
| 2 | File identity | subject `(n/m)` and name, else yEnc `=ybegin part/total/size name` | T0 if the subject is stable per file, else AB | `FileKey` + part/total/size. XOVER alone can't cut runs; read every header (1-in-5 sampling → 0 whole) |
| 3 | Per-group file runs | 2, num, t | T0 | `FileRun`. The end is the max t of full-size parts, since the short last part arrives early |
| 4 | End-to-start join | runs, all groups | T0 | `JoinEdge` if `t_start(B) - t_end(A) ∈ [-1.2·W/rate - 0.5, +0.5]` s |
| 5 | Candidate chains | edges | T0 | Paths. A run keeps several out-edges until stage 7 |
| 6 | PAR2 index | the small `.par2` run in the window | 1B per upload | `Par2Set`: FileDesc (md5, md5_16k, len), IFSC, creator. The Recovery Set ID depends on names: store it, never key on it |
| 7 | Whole-upload size match | chains, PAR2 lens | T0 | Runs → (upload, file_idx) by **sizeChain**: anchor on unique sizes, grow only with sizes left in that list, cost `gap·rate + |ln(rate_a/rate_b)|`, cheapest step first globally. Hungarian adds 1-4 points, so keep it behind a flag |
| 8 | hash16k tie-break | runs still ambiguous | 1B per unique-link run | MD5 of the first 16 KiB of decoded article 1 = `md5_16k`, which fixes the upload and the index. Find unique links across groups. If a fetch fails, take the next file. No endpoint bisection |
| 9 | Server-key fragments | msgid grammar | T0 | `Fragment` per (host, fe, session). One key holds about 6% of a post |
| 10 | Stitching | fragments | T0, 1B per fragment when splitting | A stable per-file subject or yEnc name stitches alone; add no time joins on top. Otherwise use ≥80% overlap and the same article size, chain reconnects on the same front-end only, and correct skew with a 2 s tolerance |
| 11 | Vetoes | every edge | T0 | Article size (non-last parts), yEnc `total`, tool fingerprint (msgid grammar, From shape, header set, PAR2 creator), size not in the PAR2 list, CRC mismatch |
| 12 | Score + union | edges | T0 | Fellegi-Sunter weight, vetoed union-find, `Confidence` |
| 13 | CRC confirm | IFSC or every pcrc32 | 1B / AB | Whole-file CRC32: fold the pcrc32 values with `crc32_combine` (needs BODY, since pcrc32 sits in the yEnc trailer), or fold IFSC with the last slice unpadded. Both give the same value. (size, CRC) is blocking only: about 1 chance pair in 120 at 1e8 files |

The inner-file CRC sits only in the last RAR volume (T2): an alt key, not a placement key.

## Rust sketches

```rust
pub struct ArticleRow { num: u64, group: GroupId, msgid: MsgId, bytes: u32, subject: Sym,
    from: Sym, t: f64, xref: SmallVec<[(GroupId, u64); 4]>, srv: ServerKey, yenc: Option<YencHdr> }
pub struct YencHdr { name: Sym, part: u32, total: u32, size: u64, begin: u64, pcrc32: Option<u32> }
pub enum FileKey { Subject(Sym), YencName(Sym), Hash16k([u8; 16]), Anon(RunId) }
pub struct FileRun { id: RunId, group: GroupId, key: FileKey, nums: (u64, u64), t: (f64, f64),
    yenc_size: u64, total: u32, art_size: u32, seen: BitVec, tool: ToolFp, crc: Option<u32> }
pub struct JoinEdge { a: RunId, b: RunId, dt: f64, ev: EvidenceMask, w: f32 }
```
```rust
pub enum ServerKey { Highwinds { t: u32, n: u32, s: u32, fe: u16 },
    Diablo { t: u32, pid: u32, host: u32 }, Giganews { w0: u32, w2: u32 }, None }
pub struct Fragment { key: ServerKey, runs: Vec<RunId>, t: (f64, f64), art_size: u32, totals: SmallSet<u32> }
pub struct Par2Set { rsid: [u8; 16], files: Vec<FileDesc>, creator: Sym, slice: u64 }
pub struct FileDesc { md5: [u8; 16], md5_16k: [u8; 16], len: u64, crc: Option<u32> }
pub enum Veto { ArtSize, YencTotal, ToolFp, SizeNotInPar2, CrcMismatch }
pub enum Confidence { Proven, SizeMatched, TimeOnly, Split }
pub struct Dsu { parent: Vec<u32>, rank: Vec<u8>, veto: HashMap<u32, SmallSet<u32>> }
```

## Scoring and union

**Weights:** `w = Σ log2(m/u)`.

- **Decisive:** a hash16k or MD5 match sets `Proven`.
- **Strong:** a CRC fold match +20.
- **Medium:** in the PAR2 size list +6. Join dt in window +4..+8,, u from local-rate Poisson. Same session S or tiling N +4.
- **Weak:** tool and article size annotate only.

Seed m/u from the sims, refit by EM on PAR2-labelled uploads. Vetoes beat scores.

**Union-find:** write our own (~80 lines). `ena` has no veto, so it doesn't fit. Edges are the truth, cluster ids a cache; recompute by BFS on a split.

**Crates:** `crc32fast` (combine) + a ~30-line GF(2) unpad helper, `md-5`, `smallvec`. The yEnc parser and a tri-state (NeedMore/Invalid/Parsed) capped PAR2 parser are our own code.

## Thresholds

| Parameter | Value | Source |
|---|---|---|
| Join window | `[-1.2·W/rate - 0.5, +0.5]` s | sim1 P .975 / R .942 |
| Reorder width W | 0.75 × connections | sim1 |
| Fragment overlap | ≥ 80% | sim5 |
| Stitch tolerance after skew fix | 2 s (.83 whole, against .71 at 0 s and .71 at 5 s) | sim5 |
| Front-end skew | ±4 s | sim5 |
| Mark `Split` | > 8 concurrent posts per front-end (.48 whole at 32) | sim5 |
| Split cost | about 15 bodies per post → .85 whole | sim5 |
| Shared session token usable | ≤ 30% sharing (.77-.81), .40 at 50% | sim4 |
| hash16k budget | 0.04-0.96 bodies per file, K = 1-8 | sim5 |
| 10% fetch failures | .92-.99 | sim5 |
| Article sizes | 716800 ngPost/Nyuu, 750000 postie, 768000 nxg | w8B |
| Twin-grade CRC intervals | ≥ 4 | haiku15 |

## Online operation

1. Ingest XOVER per group from the high-water mark, running stages 1-3 per article. A run stays **open** until `seen == total` or it has been idle for `H` = max window + 30 s.
2. On close, run stage 4 against closed runs within `t ± H`, using a per-provider index ordered by t. An edge is **tentative** until both ends are closed and H has passed on every group in the pool.
3. Fetch order: PAR2 index → article 1 per unique-link run → last article → all bodies.
4. Run stages 7 and 8 per component of tentative edges, never globally. Budget about 1 body per file. Without `Proven` at that budget, mark the component `SizeMatched` or `TimeOnly`.

## When to re-evaluate a component

- A new or late run lands inside its window. (feed lag can exceed 1 h)
- A PAR2 index, hash16k result or CRC result arrives.
- An edge is vetoed, or the size passes 1.2 × the PAR2 file count. Then cut the weakest bridge and run BFS.
- A takedown (451 on BODY) or a repost appears. The repost has the same bytes, so link it as a twin and never collapse it.
- Daily sweep of `TimeOnly` components older than 6 h.

## Failure modes

- **Equal-size volumes:** sizeChain alone gets .24-.72. Stage 8 fixes it.
- **No PAR2**: rely on volume grammar and CRC. .65-.80.
- **postie `full`:** yEnc names are random per article, so only article 1 names a post and splits cost more.
- **P ≥ 32 front-ends:** fragments over-merge; split.

## Test cases

1. **Batman Knightfall.** One 47.8 GiB MKV as 48 × 1000 MiB RAR volumes + a 947.2 MiB last volume, with 716800-byte articles (1463 parts per volume, about 71.6k articles) plus PAR2. Posted in series over 4 groups, poster per file, subjects obfuscated, alongside 7 competing uploads.
   - Exactly one component: 49 RAR runs + the PAR2 runs, all `Proven`.
   - Bodies ≤ 1 + 49.
   - The inner MKV CRC comes only from the last volume.
   - Renaming the files changes no link.
2. Case 1 with postie `full` (Date randomized): same result, Date never read.
3. Case 1 with Nyuu `--date` + retries: no duplicate segments.
4. Equal volumes with K = 8 concurrent: 1.00 at ≤ 0.96 bodies per file. At 10% fetch failures, ≥ .92.
5. Per-connection keys at P = 8, then P = 32 with split: ≥ .85 whole, precision ≥ .98.
6. **Veto:** equal volume size, but totals 1463 vs 1500 and article sizes 716800 vs 768000. Never merge.
7. `fold(pcrc32) == fold(IFSC, last unpadded) == crc32(file)` for random files and article sizes.
8. Arrival-order replay with a 1 h lag on one group must equal the batch result, with no `Proven` edge retracted.
