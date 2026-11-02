# Wave 5 / Agent A: real Message-ID samples (Haiku). The orchestrator re-checked them against BigBuck.nzb.
NZB sites (binsearch, nzbindex, animetosho, web.archive.org) are blocked by this container's egress proxy. Samples came from public NZBs on GitHub instead.
Counts: Highwinds server-assigned 658 (BigBuck.nzb from megadrive/premiumize-api-issue @fx33.iad; a Binsearch reftest @fx21.am4; an NZBIndex earth.nzb @fx02.iad), ngPost 2140 (`823c1a00e1774afab13c3ef695a32f0c@ngPost`, 32 hex with no dashes, nzbget-ng sample), Nyuu 332 (`KiNgMbNsLfYrYaBlIdUzJiQc-1520877517401@nyuu`, sanderjo/NZBs).
Highwinds format `T1.N$K.R@fxNN.site`, e.g. uJq1A.251076$8E2.241359@fx33.iad, vJq1A.251077$8E2.66954@fx33.iad ...
- T1 (5 chars): a base-64-like counter, least significant char first. It moves ~0.76 per article and rises with post date across files (2016 < 2020 < 2025), so it is clock-like. INFERRED.
- N: a SERVER-WIDE article counter on that front end. ORCHESTRATOR CHECK: it does NOT reset per file. It continues across the post's files (par2 251076, part01 251077-251116, part02 251117-251156, part03 251157-251209 with 40 segments in a 53 range ...). The gaps are other users' articles injected on the same fx33 in between. So N is an injection clock shared by all posters on that front end.
- K ($8E2, BA5, 6aQa): constant across the whole post and different between posts, so it is a per-connection or per-session key. INFERRED.
- R: random per article. Not usable for ordering.
- host fxNN.site: the injecting front end (iad = Ashburn, am4 = Amsterdam).
Linking: same K + same host + N ranges that tile in time = one upload session, even across groups and obfuscated subjects. N interleaving bounds which posts were concurrent. Cost: overview (Message-ID is in XOVER).
