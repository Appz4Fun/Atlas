# W1C obfuscation + partial hashing (Haiku, ~06:58Z)
Caveat: agent conflated Nyuu and ngPost option tables (both shown from the ngPost README); msg-id "@ngPost" default belongs to ngPost, not verified for Nyuu.
- ngPost: article size default 716800, msg-id domain default "ngPost", random poster unless --from, -x obfuscate subjects to UUIDs. Nyuu fork xompage/Nyuu-obfuscated exists.
- Newsbin forum: obfuscated posts where every segment has a random filename but the subject base stays constant with only a trailing number changing -> within-post key, maybe across reposts.
- SABnzbd duplicate detection = NZB name/content + GuessIt only, no hashes.
- yEnc 1.3 draft: =yend pcrc32 required for multipart, crc32 optional (spec wording ambiguous: decoders check pcrc32 over decoded bytes).
- yencoding crate parses pcrc32 (not exposed) and crc32 (whole_file_crc32); no crc32_combine anywhere in indexer code found.
- pcrc32 only matches between posts with identical segmentation (716800 defaults make this plausible).
Leads: Nyuu --obfuscate-articles exact fields; which parts carry whole-file crc32= in real posts; subject-base persistence across reposts.
Sources: github mbruel/ngPost; xompage/Nyuu-obfuscated; forums2.newsbin.com viewtopic p=230151, p=222555; sabnzbd.org/wiki/advanced/duplicate-detection; caronc/newsreap docs/yenc-draft.1.3.txt; docs.rs yencoding
