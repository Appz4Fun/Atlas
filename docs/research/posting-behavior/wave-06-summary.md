# Wave 6 merged (09:28-09:38Z)
- SERVER IDS DECODED from 72k segments in 261 public NZBs + mail corpora (dataset in server-ids/):
  - Diablo (Astraweb, UsenetServer, free.fr, Eweka upload readers): <hex unix time$per-second seq$pid$hosthash[$x]@cluster> (diablo dreaderd/subs.c:260). A multi-connection upload = a set of pids on 1-2 front-ends in a tight window.
  - Giganews: 32-char base64 -> XOR every byte with 0x9D -> six LE uint32; word 2 = unix time (9/10 within 14 s); words 0/1/4 look like server/process/counter ids.
  - newsmangler: <float time>.<segment number>@<provider host> (864/864).
  - Usenet.Farm: 32 random letters, no time. UE/Omicron/Abavia/Easynews not found yet.
  - Highwinds T decode was confirmed again by another thread on BigBuck.nzb (133 articles, ~1 s apart).
- SIMULATION ROUND 3: no cheap shortcut. Reading the yEnc header for only 1 in 5 articles drops fully-correct uploads from .71 to 0 (uploads interleave inside each second). PAR2 part-count placement without headers also fails. A per-upload server session token (Highwinds S) is as good as reading every header, but degrades fast if 30% of uploads share it (.40). Remaining errors at .71 are chain order/boundaries -> try whole-upload PAR2 subset-sum.
- PROPAGATION: no published measurements; anecdotes put cross-backbone lag at seconds (direct peers) to >1 h (obscure injection points). A per-article arrival vector across 5-6 real backbones would fingerprint the injection cluster; multi-server posters (Nyuu) show a mixture. Novel - nothing academic exists.
Wave 7 plan: (A) sim round 4: whole-upload PAR2 subset-sum + N-tiling for shared session tokens + headers only in ambiguous windows; (B) finish ID decoding: Giganews word meanings, UE/Omicron/Easynews/Supernews/Abavia from mail and archive corpora; (C) pivot: NNTP commands that expose arrival time or per-article server data (NEWNEWS, LIST ACTIVE.TIMES, HDR on NNTP-Posting-Date/X-Received-*, XFEATURE, DATE) and what providers allow.
