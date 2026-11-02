# Wave 1 merged (06:44-06:48Z)
Strongest lead: the TRiToN pattern (random group per file + random From) matches ngPost --obfuscate with group_policy=each_file (postie has the same option). These tools leave run-constant fingerprints that survive rotation:
- Message-ID shape per tool: ngPost <32hex@sig> with From xxx13@sig.com (same domain on every article); Nyuu rand24-<ms epoch>@nyuu (timestamp orders a whole run across groups); postie 32@8.3 / From 14 <14@5.3>; nxg-upper 40@21.3 hex + shared X-Nxg header; GoPostStuff UnixNano; newsmangler epoch.part@host.
- Date behavior: ngPost sends none (server stamps arrival); Nyuu --date now = one Date for the run; postie full = random 0-6h back.
- XOVER Bytes/Lines: article size (716800 ngPost/Nyuu, 768000 NewsUP) and line width (Bytes/Lines ~ line+2).
- Arrival-time join: per-group article number -> wallclock curve from high-water-mark scans, match bursts and bytes/sec across groups. No indexer groups by timing today (nZEDb MGR needs an exact From whitelist).
- Injection-Info / X-Trace / Path tail / NNTP-Posting-Host: session-constant, HEAD/XHDR only, often stripped.
- Obscure: DMCA co-removal vectors (STAT over time) link a whole release after takedown (OS3 2015 study).
- Method: tool classifier -> block on (tool, domain/sig, time window, size) -> Fellegi-Sunter scoring or co-activity graph (Louvain).
Wave 2 plan: lean in. (A) tools not yet covered + re-upload/fill/repair posting behavior, (B) real header dumps: what each backbone exposes in OVERVIEW.FMT/HDR, (C) arrival-curve and burst-join algorithms and Xref numbering behavior.
