# W3C NZB search engines / dedupe (Haiku), pass 1
- Newsbin dup check: checksum of first chunk (unverified); search merges x-posts by recovered real filename.
- NZBLNK nzblnk:?t=title&h=header&p=password (h = header search string? unverified).
- usenet-dl: SHA-256 of NZB, name, job_name.
- SABnzbd duplicate detection: identical NZB name/content; smart = GuessIt title.
- SABnzbd forum p=58021: hash of sorted Message-ID set as dup key.
- nzbdav/altmount/Easynews: unknown.
## pass 2
- NZBLNK h= "identifies binary content", partial header string to search a header pool; g= groups, p= password. Not a hash.
- usenet-dl nzb.rs: SHA-256 of raw NZB bytes (order-sensitive). Sorted MID-set hash only proposed (sabnzbd p=58021). Note: MID sets only dedupe the SAME post (same NZB re-shared), never reposts, since reposts get new MIDs.
- nzbdav-rar: header-only, m0 only, detect_version works on a prefix.
- AltMount/NzbDav/Newsbin DupeCheck internals: unreached.
