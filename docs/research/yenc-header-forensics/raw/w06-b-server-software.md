# Wave 6 / Agent B: server software behind the IDs (Haiku). Mostly blocked.
- VERIFIED INN lib/messageid.c: `#define OFFSET 673416000L`, `<radix32(now-OFFSET)$radix32(pid)$++count@domain>`, where count is static per nnrpd process (one per connection).
- The Highwinds epoch 876645500 (1997-10-12) works the same way (time minus epoch, radix-encoded) but uses base-64 with a "T.N$K.R" layout. Diablo is the likely ancestor (INFERRED), but its source tarballs on backplane.com are proxy-blocked here.
- Link meaning: per-process counter + time + pid/session key = same connection/session, not account. Account linkage needs the POSTED Path token or Injection-Info.
- Nothing found for X-Ufhash/X-Ufk, Giganews, UsenetExpress, Abavia or Eweka token generation.
