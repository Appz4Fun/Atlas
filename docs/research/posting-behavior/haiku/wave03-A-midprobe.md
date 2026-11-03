# Wave 3 A: discovery by Message-ID (Haiku, ~15 calls)
1. RFC 3977 6.2.1: ARTICLE/HEAD/STAT by <message-id> MUST NOT change the selected group; article number returned as 0 unless in current group. STAT: "223 0|n <mid>" or "430". https://www.rfc-editor.org/rfc/rfc3977.txt (high)
2. Xref via HEAD <mid> or HDR Xref <mid>: unverified, test live. Note: for group-per-file slices each article is in ONE group, so Xref only tells that slice's group; siblings still need predicted MIDs or overview scans.
3. NZBGet "download health" = % articles retrieved; no STAT detail. https://nzbget.com/documentation/understanding-download-health-in-nzbget/
4. SABnzbd pre-check exists but is no longer recommended by maintainer; slow (~1 KB/s). https://forums.sabnzbd.org/viewtopic.php?p=94857
5. No evidence of provider STAT-flood throttling found.
6. newsmangler/GoPostStuff/NxG/NZBLNK formats not re-verified (peers have them).
Algorithm: seed MIDs -> HEAD for Xref -> STAT predicted siblings (only from deterministic/time-based formats: nxg-upper, newsmangler, GoPostStuff) -> second provider re-check -> coverage ratio.
