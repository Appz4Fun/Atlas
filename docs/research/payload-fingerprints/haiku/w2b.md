# W2B external hash oracles (Haiku, ~07:27Z)
- AniDB UDP FILE: lookup by size+ed2k only (returns md5/sha1/crc32); login, 1 pkt/4s, ban risk. ed2k not computable from front articles -> post-download only. https://wiki.anidb.net/UDP_API_Definition
- BEP 52 v2 per-file SHA-256 merkle root over 16 KiB blocks: needs whole file. https://bittorrent.org/beps/bep_0052.html
- NSRL RDS: MD5+SHA1 per file (no CRC32) -> match PAR2 full MD5 for software. https://www.nsrl.nist.gov/Downloads.htm
- No-Intro/Redump DATs: size, CRC32, MD5, SHA1, SHA256 per ROM/ISO -> offline match vs RAR inner CRC32+size. https://dumping.guide/submission/general
- MusicBrainz/AccurateRip/hashes.org: no usable file hashes. CUETools DB, AnimeTosho (robots), libgen README unreachable.
- xREL OpenAPI exists (github xREL-to/OpenAPI), fields unverified; predb.net lists files/size, API unverified.
- Anna's Archive md5-keyed records: likely full-file MD5 (= PAR2 FileDesc MD5) for ebooks; unverified.
Leads: Anna's Archive/libgen MD5 vs PAR2 MD5; torrent listings (exact size+name) e.g. Nyaa; local No-Intro/Redump CRC index.
