# Wave 4C: overview-only (:bytes/:lines) matching (Opus subagent, Monte Carlo + NNTmux read)
- Escape rate measured e=1.580% (L=128), 1.569% (L=256); floor 4/256.
- P=716800, L=128: body lines 5687.95 sd 0.84; wire body bytes 739504 sd 102.6. Analytic sd sqrt(P p(1-p)) ~105 B.
- lines ~ ceil(P*1.0158/L)+3 (=ybegin,=ypart,=yend); body bytes ~ P*1.0158 + 2*lines + ~150.
- :bytes = whole article; subtract known overview header lengths -> body + C (C = injected headers, ~constant per server/session, inference). Compare :bytes within one server only.
- Preset table (lines incl 3 / ~body bytes), L=128: 716800 5692/739.7k; 750000 5955/773.9k; 768000 6098/792.5k; 640000 5082/660.4k; 512000 4067/528.4k; 500000 3971/516.0k; 384000 3051/396.3k; 1048576 8325/1081.9k. L=256 halves lines. 384000@128 == 768000@256 lines (3051) -> need bytes too. P_est=(bytes-C)/1.0158, L_est=P_est*1.0158/(lines-3).
- Last-part bucket ~10.9 bits at ±1 line. Equal-size RAR volumes weaken file-level key -> fingerprint the posting set (volume count, final-volume bucket, PAR2 sizes).
- SURPRISE: escape count is a content hash visible in overview. Same bytes + same tool + same L -> identical :lines and body bytes per part. ~1.76 bits/part from :lines, ~8.7 bits/part from header-normalized :bytes; 70-part file ~120 bits from ordered :lines alone. Needs part order (subject i/n, msgid or article order). Untested on real data.
- Rules: R1 twin = same server, P, L, n, last-part lines ±1 (~2^-10.9 FP); R2 strong twin = same per-part lines/bytes sequence; R3 session = same (P,L,C residual)+Date/From/msgid proximity (filter only).
- NNTmux: HeaderParser.php ~95-97 Bytes/:bytes; PartHandler.php:85 parts.size; BinaryHandler.php ~427-452 partsize=SUM; MediaSourceStage.php:28 POSTED_SIZE_TO_REAL_SIZE=0.97; Par2FileMapper.php ~90 ceil(size/articlePartSize)==segments; :lines unused.
- Scripts: overview-model/ (m.py simulation, t.py presets).
Leads: real provider :lines/:bytes semantics; per-tool escape rate as tool ID; PAR2 size ladder.
