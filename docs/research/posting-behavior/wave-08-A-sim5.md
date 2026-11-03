# Simulation round 5: hash16k tie-break and per-connection keys

## Method
**Task 1** (`sim5_hash16k.py`, `sim5_hash16k_fail.py`)
- K = 1-8 concurrent uploads, each 50 volumes of 100 MB plus a smaller last one.
- Each upload posts to 2-4 of 8 groups, with a random group per file. The poster rotates per file.
- Speed 8-40 MB/s, gaps 0-0.6 s, dates in whole seconds, end-to-start joins within 0-2 s.
- Baseline is sizeChain, where equal sizes leave only the last volume and the start time.
- Tie-break: the MD5 of the first 16 KB of the first article is matched to PAR2 FileDesc hash16k. This gives the upload and the volume index.
- Fetch policies: hash every file, chain endpoints with bisection, or one fetch per **unique-link run**. Each counts 1 PAR2 body per upload.

**Task 2** (`sim5_sessions*.py`, `sim5_tolsweep.py`, `sim5_seeds.py`)
- 3 hosts × 4 front-ends. Each front-end has its own counter and a clock skew of ±4 s.
- P concurrent posts, each over 6-30 connections on 1-3 hosts, fed from a shared queue.
- A connection reconnects every 200-2000 articles and gets a new key. Keys are per connection: (host, fe, session).
- Stitching:
  - join sessions with ≥80% time overlap and the same article size;
  - chain reconnects on the same front-end;
  - optional veto when the yEnc part totals differ;
  - optional join on a shared per-file subject.
- Scores are pairwise precision and recall, plus "whole": the share of posts found intact at ≥99% purity.
- "Diverse" world: varied totals and sizes. "Realistic": mostly 716800-byte articles, 147 parts per file.

## Results
**Task 1, equal-size volumes** (accuracy / bodies fetched per file)

| K | sizeChain | Hash all | Endpoint bisect | **Unique-link + hash** |
|---|---|---|---|---|
| 1 | 1.00 | 1.00 / 1.00 | 1.00 / 0.04 | **1.00 / 0.04** |
| 2 | 0.72 | 1.00 / 1.00 | 0.99 / 0.61 | **1.00 / 0.66** |
| 4 | 0.33 | 1.00 / 1.00 | 0.91 / 1.29 | **1.00 / 0.88** |
| 8 | 0.24 | 1.00 / 1.00 | 0.97 / 1.60 | **1.00 / 0.96** |

Restricting links to a single group (`sim5_hash16k_pergroup.py`) does worse: accuracy 0.91-1.00 at 0.68-0.80 bodies per file, even at K=1. Unique links must be found across groups.

With failed first-article fetches (falling back to the next file in the run):

| K | 2% fail | 10% fail |
|---|---|---|
| 2 | 0.99 | 0.97-0.99 |
| 8 | 0.98-0.99 | 0.92 |

**Task 2, per-connection keys** (precision / recall / whole)

| P | Method | Diverse | Realistic |
|---|---|---|---|
| any | Per-post key (old assumption) | 1/1/1 | does not exist in reality |
| 2-32 | Raw per-connection key | 1/0.06/**0** | 1/0.06/**0** |
| 8 | Overlap + size | .74/1/.67 | .81/.99/.71 |
| 8 | + yEnc-total veto | 1/1/1 | .94/.99/.85 |
| 32 | Overlap + size | .78/1/.65 | .09/.99/.18 |
| 32 | + yEnc-total veto | 1/1/.98 | .39/.98/.48 |
| 32 | Overlap + subject union | .74/1/.66 | .11/1/.22 |
| any | Per-file subject link only | 1/1/1 | 1/1/1 |

Splitting (`sim5_split.py`, realistic): fetch 1 body per session in the larger clusters and let the yEnc name or hash16k name the post. Whole posts rise to 0.84 (P=8) and 0.85 (P=32, up from 0.48), at about 10-15 bodies per post, with precision ≥0.98.

Removing skew perfectly lifts P=32 to 0.70/0.97/0.59. A 2 s tolerance is best (0/2/5 s at P=8: 0.71/0.83/0.71 whole).

## What Atlas should do
1. **Use the hash16k tie-break, gated by ambiguity.**
   - Fetch one first-article body per unique-link run.
   - Equal-volume accuracy goes from 24-72% to 100%, at 0.04-0.96 bodies per file.
   - Fetch the PAR2 index first. On a failed fetch, try the next file in the run.
   - Drop endpoint bisection: it costs more and misses A-B-A interleaving.
   - Find unique links across groups, not per group.
2. **Retire the per-post key assumption.** Each per-connection key holds about 6% of a post, and 0% of posts are found whole. Treat keys only as fragments.
3. **Stitch on content first.** A stable per-file subject or yEnc name links fragments perfectly, because the shared queue spreads every file over many connections. Do not add time-overlap joins on top of it: they lower precision.
4. **With subjects obfuscated per article:**
   - use overlap only to propose candidates;
   - veto on article size and part total;
   - chain reconnects only on the same front-end;
   - correct each front-end's skew, then use a 2 s tolerance.

   This is reliable at P ≤ 8. On busy front-ends (P=32, about 0.5 whole), mark clusters low-confidence and split them. Fetch 1 body per session fragment; the yEnc name or hash16k names the post. This costs about 15 bodies per post and lifts whole posts from 0.48 to 0.85.

## Caveats
- Synthetic worlds; key formats were not checked against live headers.
- Fetch failures hurt most on single-file runs. Matching yEnc pcrc32 to PAR2 slice checksums is an untested fallback.
- The split assumes that any body names its post. With yEnc names obfuscated, only first articles do (via hash16k), so the cost rises.
- 3-8 seeds: whole-post figures ±0.1.
- scipy is not available, so linking uses a greedy match. Scripts: /tmp/claude-0/-home-claude/e80bf178-a617-5024-bf35-a4bf70dd12e4/scratchpad/w8a/.
