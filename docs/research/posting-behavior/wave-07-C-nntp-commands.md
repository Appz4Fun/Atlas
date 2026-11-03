# Wave 7 C: NNTP commands that expose arrival (09:59-10:09Z; code read from clones of inn, nntpswitch, nntmux, nzedb)

RFC 3977 s6: "The server MUST ensure that article numbers are issued in order of arrival timestamp." NEWNEWS (s7.4) = articles posted or received since T; INN implements it against overview arrival time (nnrpd/newnews.c).

| command | reveals | support |
| NEWNEWS group date | Message-IDs arrived after T (1 s); bisect T -> per-ID arrival | INN default on (allownewnews); Diablo off by default (dreader.access); nntpswitch single group only, passes to backend ("501 NEWNEWS only supports single newsgroup names"); no binary provider CAPABILITIES dump lists it |
| GROUP / LIST ACTIVE / LIST COUNTS | HWM + count; with DATE maps number -> arrival | universal; Highwinds Cyclone/Tornado/Typhoon/Twister support LIST COUNTS (inn-workers 2009-November/017363) |
| DATE | server UTC clock | INN, Diablo, nntpswitch; some providers lack it (nntppool falls back) |
| LIST ACTIVE.TIMES | group creation only | INN, nntpswitch |
| LIST OVERVIEW.FMT | extra fields (INN extraoverviewadvertised can add Injection-Info/Path) | INN only; binary backbones seem 7 fields + Xref:full |
| HDR/XHDR/XPAT non-overview headers | per-article server metadata without HEAD | INN reads articles (slow); nntpswitch DenyNoIndexField -> "501 ... disabled" (assume Abavia blocks) |
| XZVER/XZHDR | compressed overview (zlib + yEnc framing) | Astraweb (origin), Blocknews, usenet.farm, cheapnews; nntpswitch passes through |
| XFEATURE COMPRESS GZIP | gzipped responses (290) | Giganews, Astraweb, Blocknews; usenet.farm 500; NNTmux probes it |
| LISTGROUP gaps | maybe takedowns, but Diablo hides missing numbers and OS3 saw removed articles still in XOVER; use STAT/BODY | |
| numbering across backbones | independent per backbone; XS News & Xenna share; Giganews & Supernews appear to | r/UsenetTalk 63lapa |

Cheapest arrival clock suggested: GROUP/LIST COUNTS + DATE every 30-60 s, interpolate per article number. CONFLICT: our wave 4 simulation found linear HWM interpolation gives +-10 s errors under bursty uploads; use Date seconds + within-second rank where Date is server-stamped, and server-ID time fields (Highwinds T, Diablo hex, Giganews w2) where available. Single-group NEWNEWS bisection is exact if any provider allows it: try once per provider.
Abavia nntpswitch synthesized numbers: verify monotonic with arrival before trusting.
Next: one live probe session per provider (CAPABILITIES, HELP, DATE, NEWNEWS alt.binaries.test <now-10m>, LIST OVERVIEW.FMT, LIST HEADERS, XHDR NNTP-Posting-Date <range>, LIST COUNTS) -> real support matrix; measure whether numbers in different groups advance together on one spool.
