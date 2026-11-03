# Wave 5 C: real provider header samples (08:57-09:06Z)

| provider (sample date) | Path | server-added headers | Message-ID / Xref / overview | evidence |
| Eweka (Highwinds) 2022 Nyuu post, raw 221 HEAD | not-for-mail | X-Complaints-To abuse@eweka.nl; NNTP-Posting-Date; Organization Eweka Internet Services; X-Received-Bytes 740059; NO Xref/NNTP-Posting-Host/Injection-Info/X-Trace | - | raw.githubusercontent.com/sabnzbd/sabctools/master/tests/yencfiles/test_head.yenc |
| Eweka 2014 | ...post02.fr7!fx09.am4.POSTED!not-for-mail | NNTP-Posting-Host "Eweka Internet Services" (placeholder); X-Received-Body-CRC, X-Received-Bytes, X-Original-Bytes | - | github.com/caronc/newsreap tests/var/25071405.5038@dcfre.net.msg |
| Eweka capabilities 2026-06 | - | - | XZVER, XZHDR, XFEATURE-COMPRESS GZIP TERMINATOR; no RFC 8054 COMPRESS; XZVER payload is zlib (78 01) despite "[COMPRESS=GZIP]" | github.com/Spottarr/Usenet issues/148, docs/adr/0006 |
| UsenetServer (Highwinds) 2020 | - | - | XOVER Date "Fri, 17 Apr 20 05:57:26 UTC" (2-digit year), Lines EMPTY, bytes 740530, full Xref "Xref: news.usenetserver.com alt.binaries.bungalow:97417009 ..."; HELP: xfeature useragent, xfeature compress gzip, xzver, xzhdr, xhdr, list counts | github.com/spoved/nntp-lib.cr |
| Usenet.Farm post (same reader) | not-for-mail | X-Ufhash (url-encoded base64), Organization Usenet.Farm, X-Received-Bytes, X-Received-Body-CRC | Message-Id <32 random letters@news.usenet.farm>; HEAD by number answers "221 0 <id>" | same |
| Newshosting 2014-15 text | not-for-mail | X-Complaints-To abuse(at)newshosting.com; NNTP-Posting-Date; Organization "Newshosting.com - Highest quality..."; X-Received-* | - | github.com/GerHobbelt/FontOrg-and-FontRenamer |
| Giganews posting 2013 via Highwinds | not-for-mail (Giganews path ends news.giganews.com.POSTED!not-for-mail) | X-Usenet-Provider http://www.giganews.com; X-Trace sv3-<b64>!<b64>; X-Postfilter 1.3.40; X-Original-Bytes; X-Received-* | Xref: highwinds ...; ID <68edne9RVoGblJLNnZ2dnUVZ_hwAAAAA@giganews.com> | FontOrg repo; r/UsenetTalk 63lapa |
| Easynews via Highwinds 2015 | ...post01.iad.highwinds-media.com!fx23.iad.POSTED!not-for-mail | Organization Easynews; X-Complaints-Info; X-Received-*; no NNTP-Posting-Date | - | newsreap tests |
| Tweaknews (Omicron) 2015/2026 | news.tweak-news.eu!spooler.pl1!82.197.223.108.MISMATCH!feeder.cambriumusenet.nl!feed.tweaknews.nl!path-stripped | X-Received-* | - | FontOrg repo; debridmediamanager/notes |
| Abavia + XS News/TorBox/Cheapnews/UseNeXT readers (nntpswitch2) 2026-09 | rebuilt not-for-mail (older asc013.abavia.com!fsa032.abavia.com!ach002.abavia.com!not-for-mail) | normalized Message-Id, X-Received-Body-Crc | OVER/HDR/XZVER/XZHDR; overview 7 fields + Xref:full; article numbers synthesized over a 14-day window | github.com/debridmediamanager/notes zurg/providers/*.md |
| Astraweb | ...isp-post01.iad!not-for-mail | - | own ID format not found | r/UsenetTalk 2017 |

Highwinds server Message-ID <T.N$S.R@fxNN.site> (e.g. NzZUA.117908$rY1.16813@fx45.am4):
- T = timestamp, little-endian base-64 digits 0-9a-zA-Z..., unix = value + 876,645,500 (epoch 1997-10-12 08:38:20 UTC). Matched NZB date= exactly on 4 of 5 NZBs (fx30.am4 2016, fx31.am4 2017, fx45 2017, fx02.iad 2025); 5th off by 865 s.
- N = per-posting-host counter, +1 per article. S = session/process token, constant for an upload. R = random-looking (<=6 digits). fxNN.site = posting host + DC (iad, am4).
=> On Highwinds-injected posts, T and N give second-exact time and a global order every provider sees identically, from XOVER, no HEAD.

XOVER reliable: Subject, From, Date, Message-ID, Bytes, full Xref (crosspost numbers on Highwinds/Abavia). Highwinds: Lines empty, 2-digit-year Date.
HEAD only: X-Received-Bytes/X-Received-Body-CRC (Highwinds, Omicron, Usenet.Farm), X-Ufhash, NNTP-Posting-Date, Organization/X-Complaints-To (= injecting provider).
Unreliable: Path (Highwinds/Abavia rebuild as not-for-mail; Omicron ends path-stripped). Don't expect Injection-Info or real NNTP-Posting-Host on binary backbones.
Next: mine many fx IDs + dates from public NZBs (alphabet beyond Z, meaning of S); same for Omicron/UE/Giganews IDs.
- T decode CONFIRMED on BigBuck.nzb by yEnc Haiku thread: 133 articles span 04:00:26-04:01:58 2016-12-06, ~1 s/article; the 865 s offset was the indexer NZB date, not the ID.
