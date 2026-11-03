# W2B srrDB / ReScene (Haiku)
- srrDB /search/ params (from SrrDBAutomationToolkit): RarHash, ArchiveCrc, ArchiveSize, InternetSubtitlesDbHash (=OSHash), SampleCrc, ReleaseName, ImdbId; /skip:n paging by 45. Exact ArchiveCrc meaning unverified.
- SRR blocks: 0x69 header, 0x6A stored file, 0x6B OSO hash, 0x71 RAR volume metadata (carries RAR headers incl inner CRC).
- Local (inner size, CRC32) index = alt linker with no external dep; srrDB maps it to a scene name.
