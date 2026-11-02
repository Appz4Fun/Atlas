# Red team: wave-09-A pipeline spec

S = spec, LF = linking-findings.md; :N = line.

## Issues, by severity

| # | Issue | Spec | Evidence | Fix |
|---|---|---|---|---|
| 1 | **Cost tiers contradict.** With obfuscated subjects, stage 2 is AB (every body), yet stages 3-5 are labelled T0. Test 1 caps bodies at 50, but with per-article random subjects it needs about 71.6k. | St. 2-5, Test 1 | S:19; w9 summary:15; w4-B:5 (sim1 assumed header-known runs) | Tier stages by input. Add T0 identity (Nyuu ms, Highwinds S+N, stable subjects). Split test 1 budgets. |
| 2 | **The join window fails on real data.** 15% of Nyuu ms gaps exceed +0.5 s (my recount: 899 gaps, p90 0.71 s). ngPost (dates, n=24) has median 6 s. postie pipelines 2 files, so its boundaries *overlap* far past the lower bound. | St. 4, Thresholds | w9B:15-21; log.md W10C (poster.go:618) | Set the window per tool and clock (Nyuu ms ≈ [-0.1, +2] s; postie overlap-tolerant). Restore sim1's \|ln rate\| ≤ 0.25 gate (w4-B:18). |
| 3 | **The primary clock is mostly absent.** Nyuu and postie set Message-ID and Date, so they get no Injection-Date. NNTP-Posting-Date is HEAD-only and sometimes absent. The spec never uses the Nyuu msgid ms (98% < 2 s). Test 2 (postie full) has no clock. | Time rules | w9 summary:18; w5-C:4,11; w7 summary:4 | Pick the clock per tool: Nyuu ms (fails under `--date`, but 0/103 public Nyuu NZBs use it: my count); ngPost server Date/Injection-Date; postie article-number rank + Xref anchors; server-ID T. |
| 4 | **hash16k is called "decisive".** A twin shares hash16k, len and MD5, so the match proves the volume, not the upload. 16k MD5 can collide across volumes. | Scoring, St. 8 | LF:91, 95; S:25 | `Proven` means file identity (hash16k+len, unique in the set). Membership needs a time/chain edge. |
| 5 | **Dedupe collapses twins and reposts.** "One row per msgid" drops a postie repost (same IDs) and its new time. Same (file, part, pcrc32, begin) under two IDs would merge a later twin. pcrc32 is BODY-only, but the stage is tagged T0. | St. 1 vs S:100 | w8B:102; LF:112 | Key on (msgid, group, num). Same segment = retry only within one session (Δt < H); otherwise twin. T0 retry tell: Nyuu keeps the ms suffix (LF:1027); else X-Received-Body-CRC (HEAD). |
| 6 | **Vetoes contradict scoring.** Stage 11 vetoes on article size and tool, while scoring says "annotate only". Mixed-tool uploads exist (repairs, NewsUP imitation), and :bytes differs per server and crosspost copy. | St. 11 vs Scoring | LF:155, 1046; w3 summary:2 | Veto only on content: decoded part size, total, size, CRC. Tool and :bytes become weights. |
| 7 | **Server keys are modelled as 6% per-connection fragments.** Real Highwinds S is constant per NZB in 7/7 uploads. Diablo PIDs are shared. Obfuscators mint own IDs: no server key. | St. 9-10 | w9B:95-97; log.md W10A; LF:1036 | Highwinds S = upload key (n=7, maybe single-connection: verify; split by N). Diablo (host, PID) = soft. Defer stitching. |
| 8 | **No container-header fallback.** 15-30% of multi-file uploads lack PAR2, and partial DMCA can remove the PAR2 articles. Article 1 as a RAR/7z header gives volume number and set at about 1 body/file. 65-80% is only the -hp + equal-size case. | Failure modes | w9C:134, 145-171 | Add stage 8b (container header). -hp salt: candidate set key (verify). |
| 9 | **Crossposts are counted per group.** One msgid in 4 groups becomes 4 overlapping runs, breaking tiling and sizeChain. Xref anchors go unused. | St. 3-4 | LF:1014; w2 summary:3 | Build runs once per msgid. Align counters with Xref pairs where present (Eweka HEAD lacks Xref, w5-C:4). |
| 10 | **"Article 1" and "last article" are ambiguous by number.** Arrival order is never part order, and the short last part arrives early. | Online 3 | LF:1041; w5 summary:2 | Address articles by yEnc part or Nyuu ms. |
| 11 | **postie spreads one upload over providers** (weighted round-robin); one backend's numbers mix in feed lag. | Time rules | LF:1038 | Flag multi-injection; widen the window or use content. |
| 12 | **Errors:** "1 chance pair in 120" (source: ~1-120 pairs); CRC +20 is de facto decisive (LF calls it a candidate); "T2" is undefined; in test 6, 1000 MiB at 768000 B gives total 1366, not 1500; RAR5-encrypted CRCs can't be alt keys. | St. 13, S:32, Test 6 | LF:114, 119, 135 | Fix; CRC = medium. |
| 13 | **"Never key on RSID" is too strict.** All vols of a set share it, and an exact twin keeps it. par2cmdline's slice depends on the whole file set. | St. 6 | w8B:98; LF:1018 | Use RSID to group PAR2 runs and flag twin candidates. No slice-size veto. |
| 14 | **Runs close after only H ≈ window + 30 s idle.** Feed lag can exceed 1 h. Late parts of closed runs are unhandled. | Online 1 | S:97; w6-C:3; LF:80 (indexer holds 4 h) | Reopen runs on late parts and hold publication for about 4 h. |

## Missing cases

- **Twins:** add a `TwinGroup` output and a repost test.
- **Alts:** an `AltGroup` on the inner CRC + unpacked size (unencrypted).
- **Partial DMCA:** runs never reach `seen == total`. Takedowns of each file's *first chunk* (w5-B:13) defeat hash16k and the article-1 header. Use another backbone, or match surviving pcrc32s to IFSC (w9C:160).
- **Repost after takedown:** postie reuses IDs, Nyuu/ngPost mint new ones; link by content as twins.
- **Mixed tools / repair fills** (part>1 with begin=1, w2 summary:4). Spray vs crosspost: #9. RAR5 -hp with no PAR2: #8, add a test.

## Cut from v1

- Stages 9-10 (fragments, skew, Split): sim-only.
- Fellegi-Sunter + EM (use a rule ladder), Hungarian, daily sweep.
- Full pcrc32 fold and LCM intervals (download path only).

## Minimal v1 (5 stages)

1. **Ingest + dedupe:** XOVER keyed (msgid, group, num), crosspost group sets, clock per tool.
2. **File runs:** stable subject / Nyuu ms at T0; yEnc headers only where ambiguous.
3. **End-to-start join:** calibrated window per clock, size bonus, chains left open.
4. **Content resolve:** PAR2 index → sizeChain → hash16k per unique-link run. Without PAR2, the article-1 container header.
5. **Union + output:** content-only vetoes, veto-aware DSU. Output `Proven`, `SizeMatched` or `TimeOnly`, plus twin/alt groups. Re-evaluate on late parts and 451s.
