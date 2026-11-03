# Wave 2 C: uploader pipeline clock fingerprint (Haiku)
Headers (RFC 5536 https://www.rfc-editor.org/rfc/rfc5536): Date = UA, second precision. Injection-Date = injecting server, MUST insert, may precede Date. NNTP-Posting-Date deprecated/undocumented. Injection-Info: posting-host, posting-account, logging-data, mail-complaints-to (no time). (high)
Archive times:
- RAR5 extra 0x03: flag 0x0001 Unix time_t else FILETIME; 0x0002/4/8 m/c/atime; 0x0010 ns. https://www.rarlab.com/technote.htm (high)
- RAR4 DOS local time, no zone/DST: https://www.ghisler.ch/board/viewtopic.php?p=325482 (med, forum)
- 7z MTime 0x14 etc., FILETIME UTC 100ns: https://py7zr.readthedocs.io/en/latest/archive_format.html (high)
- ZIP DOS local 2s + 0x5455 UT extra UTC (layout unverified)
- MKV DateUTC 0x4461 (unit unverified; spec says ns since 2001-01-01)
Signals: S1 packer UTC offset = round(Date - DOS local, 15min); S2 pack-to-post latency = Date - newest mtime; S3 mux-to-post = Date - DateUTC; S4 poster skew = Injection-Date - Date per injecting host; S5 sub-second precision presence = tool/OS fingerprint. S1,S4 most stable.
Pitfalls: ~2.3% of 14.5M Date headers non-GMT zones (INN list 2002-September/010390); DOS 2s + DST ambiguity, use medians; posters can forge Date. Time-stripping flags (7z -mtm-, rar -tsm0, mkvmerge no-date) unverified.
