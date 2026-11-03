# Wave 2B: backbone injected headers (Opus subagent)
- Usenet.Farm (2018 via NewsDemon, https://leo.leung.xyz/wiki/Usenet): Organization: Usenet.Farm, Path: not-for-mail (stripped), X-Ufk: <opaque>, X-Ufhash: <long base64 urlencoded> (rstforums 112132, forums.suse.com 34607). Possibly encrypted trace; test for stable prefix across one upload.
- Omicron/Highwinds: Path tokens nx02.iad01.newshosting.com, peer02.iad!feed-me.highwinds-media.com; Easynews X-Complaints-To/Organization; no per-account key found.
- Giganews: X-Usenet-Provider, X-Complaints-To, X-Trace sv3-<b64>!<b64> (encrypted?); reseller Path shows only reseller (newscene.com!not-for-mail) but X-Usenet-Provider reveals backbone; server-made msgid <...AAAAA@giganews.com>.
- NewsGuy NNTP-Posting-Host per-user hash (confirmed again).
- Abavia Path asc013/fsa032/ach002.abavia.com. datahoarder.io backbone map (2026-09) lists Eweka/TweakNews as own network.
- XS News Path tail keeps injector (goblin = Easynews). Peering preserves injector Path tail unless collapsed (Usenet.Farm, resellers, INN strippath). Injection-Info not client-settable.
- Google Groups Injection-Info posting-account = per-account id (reference).
- X-Received-Bytes + X-Received-Body-CRC added by READING server (NewsDemon, XS News, NewsGuy, Easynews). If CRC is over body only, twins share it across providers -> free per-part body fingerprint via HEAD/XHDR. Needs verification.
- Article numbers per backbone family (Giganews=Supernews, XS=Xenna); Xref host can be "."; contiguity of a session only plausible on injecting server, single-connection; XREPLIC lets peer-level posters set numbers.
Leads: empirical HEAD compare of X-Received-Body-CRC on Eweka + another backbone, XHDR support; Diablo/Cyclone/Typhoon docs defining it; group signature headers (Verify-Signature-Newznzb, X-Ufk in obfuscated groups).
