# Wave 2 B: reposts, fills, requests (Haiku, ~12 calls)
1. Fill = segment with same (yEnc name, size, begin, end, pcrc32) as an earlier part, later Date, new Message-ID. Fill concept: https://forums.newsbin.com/viewtopic.php?p=205550 (med, mapping unverified)
2. Newsbin dedupes by filename + first ~20-600K; name alone merges distinct releases. same URL (high claim)
3. NZB gains parts over header cycles; MID absent at T, present at T+x = late part/fill. https://forums.newsbin.com/viewtopic.php?p=212964 (med)
4. Unpost: groups segments by regex ID from subject + segment number; warns on duplicate segment numbers. https://discmaster.textfiles.com/view/6363/DATAFILE_PDCD5.iso/utilities/u/unpost/!Unpost/doc/Unpost (med)
5. usenet-dl dupe keys: SHA-256 of NZB, exact name, deobfuscated job_name. https://docs.rs/crate/usenet-dl/latest/source/src/db/duplicates.rs (high)
6. Twin key ignoring MIDs: hash of per-file (segment number, bytes) sequence + name. https://harley.com/usenet/file-sharing/15-understanding-nzb-files.html (format high; NOTE peers say :bytes varies by server, use part size + lines)
Dead: no source for REPOST/FILL subject conventions, request IDs, DMCA autoposters.
