# Wave 6 A: overview-only proxies (Haiku; yEnc 1.3 draft + python sim scratchpad yenc_sim.py, 300 trials)
Escapes NUL LF CR '=' -> expansion 1.015625 on random data (sim mean 1.01562 sd 0.00013). Body lines ~ ceil(P*1.015625/128) (+~2.7 in sim), :lines = body + 3 yEnc lines (RFC 3977 :lines semantics from memory).
| article size | :lines mean | sd | range | body bytes mean | sd |
| 716800 | 5691.6 | 0.82 | 5689-5694 | 739538 | 97 |
| 750000 | 5955.1 | 0.92 | 5953-5958 | 773786 | 111 |
| 768000 | 6098.0 | 0.85 | 6096-6100 | 792353 | 105 |
- :lines of any non-last article identifies article size unambiguously (bands 142-258 lines apart). Nyuu and ngPost both 716800 so not separable by size.
- Last article :lines gives file size mod S to ~±300 B (p95); total size ~ (N-1)*S + P_last.
- Equal-size RAR volumes: last-article :lines match within ±2 lines 97%; unrelated pairs ±2 at 0.094% -> ~0.4 false pairs per 30-volume set. Good filter, confirm with BODY.
- :lines is body-only, so more stable across servers than :bytes (reasoning, unsourced).
CAVEAT (Opus posting thread, spoved/nntp-lib.cr spec): Highwinds/UsenetServer XOVER has an EMPTY :lines field and 2-digit-year Date, so on Highwinds-fed providers fall back to last-article :bytes (headers included). Check Atlas overview rows per provider (Eweka).
Rule: infer S from modal :lines (sd<=1.5), estimate file size from last article, pair volumes with equal size estimate and article count, confirm with one BODY (yEnc size=) or the PAR2 bridge. Subject-stem grouping (step 1 in agent's rule) fails under obfuscation; use time windows instead.
