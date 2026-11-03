# W4B late fills / reposted articles (Haiku)
- Nyuu: --check-post-tries default 1; MID re-randomized on resubmit unless --keep-message-id; --yenc-name default {filename}. (Map says post-check repost keeps MID — conflict; help says randomized by default.)
- ngPost retry: new UUID for subject + MID.
- pan-users 2018: some posters supersede a single part with a corrupt copy -> conflicts must be handled.
- NNTmux CollectionHandler: collectionHash = sha1(name + totalFiles); no size/pcrc check. groupMissingPartsIntoRanges gap > messageBuffer/4.
- yencoding-multi Assembler: identifies file by total size, places parts by byte range, OverlappingPart / InconsistentCrc / CrcMismatch errors. Good model.
- Proposed exact-match rule: article joins file F iff yEnc name==F.name, size==F.size, total==F.total, (begin,end)==stored range for part, pcrc32 equal if both present, and decoded data passes own pcrc32. Mismatch -> flag, keep first, never purge. Key (name,size,total); never subject.
MY NOTE: under obfuscation, cross-post twins can have different yEnc names; for those fall back to (size, begin,end, pcrc32) sequence equality across >=2 parts.
