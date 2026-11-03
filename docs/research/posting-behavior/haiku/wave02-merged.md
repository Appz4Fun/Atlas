# Wave 2 merged (07:17-07:21Z)
## New leads
- ngPost is file-sequential, so the files of one post form a time-ordered chain even when each file goes to a different group: file k's last articles overlap file k+1's first articles (N-connection overlap). This boundary overlap is a strong stitching key for group-per-file slices.
- ngPost retries mint a NEW Message-ID instantly (same part, same subject): duplicate parts seconds apart = one article. Nyuu reposts keep the same MID ~35 s+ later.
- Nyuu Date is article build time, so Date runs monotone in queue order.
- nxg-upper: Message-IDs are SHA256("NxGHeader:type:number"), deterministic. Given the header, every article ID (data and PAR2) can be computed. Same applies to any header-based scheme.
- Fills: same (yEnc name, size, begin, end, pcrc32) with later Date + new MID = fill of an earlier post.
- Uploader clock fingerprint: S1 packer UTC offset = Date - RAR4/zip DOS local time (15-min rounding); S4 = Injection-Date - Date per injecting host; S2/S3 pack/mux-to-post latency; S5 sub-second precision presence (RAR5 0x0010 ns flag, 7z 100ns) fingerprints tool/OS.
- Prior art: usenet-dl dedupes by NZB SHA-256, exact name, deobfuscated job_name. Newsbin dedupes by filename + first bytes.
## Dead ends
- No sources for REPOST/FILL subject conventions, request IDs, DMCA autoposters.
- NNTP-Posting-Date is deprecated/undocumented in RFC 5536; prefer Injection-Date.
