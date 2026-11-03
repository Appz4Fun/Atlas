# Wave 5 B: co-removal probe design (08:57-09:06Z)

Response codes:
| code | meaning today | label? | source |
| 223 STAT / 221 HEAD | metadata only; body may be gone | not proof of presence | rexum July 2026 |
| 430 | no such message-id (expiry / never arrived / takedown) | ambiguous gone | Giganews KB 9952660678285; RFC 3977 |
| 451 | removed due to DMCA (Giganews documents; rexum saw it) | STRONG takedown label | Giganews KB |
| 423 | number not in group (number-based only) | never; probe by message-id | RFC 3977 |
| 502/503/400/timeout | transient | retry | SABnzbd newswrapper.py |
| 220/222 placeholder text | 2012 body swapped ("Content removed due to DMCA.", "Article blocked", Cancelbot) | takedown | forums.sabnzbd.org p=66802 |
Tool handling: SABnzbd lumps 411/423/430/451 as missing (newswrapper.py:269); NZBGet retries 451 on the same server; nntppool, nzbcyclops, Rust nzb-nntp have no 451 case (nzb-nntp marks the connection broken). Atlas needs explicit 451/430 handling. OS3: XOVER still returned subjects for removed articles (header index lingers).

Partial vs whole: OS3 2015 zero deviation from all-or-nothing; but Newsbin 2012 "first chunk of each file missing" (Supernews/Giganews), SABnzbd 2012 5/13 PAR2 replaced, Reddit 2025 and Lemmy: "remove just enough to defeat PAR2", different articles per backbone. => one canary per fileset is no longer safe.

Design:
- Canaries per file: first segment, last segment, 2 random middle, PAR2 index segments; on a 5% subsample STAT every segment to measure partial-removal rate.
- Pipelined STAT (NewsDemon accepts deep pipelines); on 223->430/451 confirm with BODY; daily BODY of one canary per release per backbone (catches STAT-present-but-body-gone and placeholder bodies via yEnc CRC).
- Cadence: every 10-15 min for 6 h, hourly to 72 h, 6-hourly to 14 d, daily to 30 d, then weekly. Latency: OS3 medians Eweka 21.5 h, Giganews 85.5 h, Astraweb 121 h; anecdotes ~12 h (2024), hours (2025); backbones within hours of each other.
- Providers: one account per independent backbone (rexum map): Omicron cluster (Newshosting/Eweka/TweakNews/Pure = one), Giganews, UsenetExpress, Abavia, Uzo Reto, NetNews, Its Hosted; ~5 check connections each (Nyuu users rate-limited when sharing with posting).
- Cost per 1M probes per provider: STAT ~0.15 GB, HEAD 1-2 GB, BODY ~470 GB (0.47 MB each). With 4-6 canaries per file, 1M posted articles = 50-100k STATs per sweep.

Scoring (design): removal time as interval [a,b] per article per backbone cluster j; background hazard h_j(age, category) from control files; chance of an independent co-flip p_j ~ h_j*|w| (+-1 poll); co-removal rate for same release q (~0.95 OS3; measure). LR: both removed q/p_j; one removed (1-q)/(1-p_j); both survive ~1. Sum log-LR over independent clusters (resellers count once), x2 weight for 451. Permutation test on removal-time vectors against same-day removals. Confounder: one notice batch removes several releases of one title -> score only pairs that passed metadata blocking; label "same takedown batch" separately from "same post".
Next: Lumen check (wave 4 found no Usenet IDs there); 7-day pilot on ~2,000 fresh releases: 451 vs 430 per provider, STAT/BODY disagreement, partial rate, whether XOVER still lists removed articles.
Sources: rp.os3.nl/2014-2015/p82/report.pdf; usenet.rexum.space/blog/usenet-retention-tests-july-2026; support.giganews.com/hc/en-us/articles/9952660678285; forums.newsbin.com p=189837; github.com/animetosho/Nyuu/issues/113; docs.rs/crate/nzb-nntp connection.rs; newsdemon.com/usenet-takedowns.
