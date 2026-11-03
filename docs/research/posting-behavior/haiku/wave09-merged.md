# Wave 9 merged (10:57-11:01Z): injection headers, PAR2 real-name leak, server ordering

## B. PAR2 FileDesc real-name leak (confirmed, with a correction)
- Postie: confirmed in source, but the place is CreateSet (internal/par2/par2.go:223-259, 340) via setInputNames (internal/par2/existing.go:233-243), not par2.go:107-113 (that is an existing-PAR2 reuse check). FileDesc name = real path relative to the posted folder under every obfuscation policy, while the yEnc name is random (full) or md5(name+part) (default) (poster.go:1436-1452).
- ngPost: --compress --gen_name renames before RAR, so PAR2 covers random volume names (FileDesc obfuscated, PostingJob.cpp:991-1002, 1113-1134). --gen_par2 without compress: FileDesc REAL (331-337).
- ParPar/par2cmdline just record what they are given (ParPar default filepath-format "common"). Nyuu creates no PAR2.
- SABnzbd (deobfuscate_filenames.py:48-105) and NZBGet (ParRenamer.cpp:180-193) already rename from FileDesc names matched by 16K MD5. Atlas should do the same at index time.
- Signature: FileDesc name differs from yEnc name and yEnc name is hash-like = Postie.
- Not checked: NewsUP, nxg-upper.

## A. Server-side injection headers
- INN nnrpd (verified in source): Injection-Info posting-host = client IP/rDNS in cleartext (default on), logging-data = nnrpd PID (one per connection, so a per-session key), posting-account off by default. Path gets ".POSTED.<client host>!". These link every article from one connection regardless of From/Subject/MID/Date.
- Real dump only from Eternal September: posting-host is a 32-hex hash and posting-account a salted blob, still stable per source.
- No HEAD dump of a binary article from Highwinds, Omicron, UsenetExpress or Newsdemon was found, and none of the server Message-ID families could be verified from public sources. Atlas has to look at its own HEAD output.

## C. Server-side ordering as the Date replacement
- INN assigns article numbers per group on arrival (innd/art.c:1450-1481); all groups of a crosspost get numbers in the same call, so Xref pairs are exact simultaneous anchors between group counters on one server.
- Injection-Date: INN adds it only when the article lacks Message-ID or Date (nnrpd/post.c ~400-405, I checked this myself). So ngPost posts (no Date) get it on INN-like servers, Nyuu and Postie posts don't.
- Nyuu post-check defaults: 5 s delay, 30 s recheck, so propagation is seconds with a minutes tail (no per-backbone numbers found).
- Algorithm: per (server, group) sort by article number; align groups using crosspost Xref anchors; cluster uploads by number proximity plus size/yEnc structure; ignore Date.
