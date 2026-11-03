# Wave 2 A: wire order and timing from uploader source (Haiku, partial)
NYUU (raw.githubusercontent.com/animetosho/Nyuu/HEAD):
- One shared FIFO article queue for all connections (lib/uploader.js ~150-156, ~226); buffer min(round(conns*0.5)+2,25). File interleave unverified (likely sequential).
- Date = post.genTime at article build (lib/article.js ~132), not send time; precedes wire by ~queue depth. (high code)
- Message-ID computed once per article (article.js ~115); retries keep it. (high)
- Post-check: STAT after 5000 ms, recheck 30000 ms, tries 2, then repost (postRetries=1) with SAME MID and SAME Date (nyuu_config.js ~68-75, uploader.js ~383-453). Signature: same MID re-sent >=35 s later. (high)
- No PAR2 in Nyuu; PAR2 timing set by external tool.
NGPOST (mbruel/ngPost src/):
- File-sequential handout (PostingJob.cpp ~640-707): file k+1 starts after k's last article handed out; N connections overlap at the boundary. (high)
- Retry: NntpArticle::tryResend() (NntpArticle.cpp ~117-131) makes a NEW QUuid each try, immediate, up to 5. Same part/subject, new MID. (high)
- No post-check/STAT code found. (med)
- Obfuscate + each_file + >1 group: group chosen per file (PostingJob.cpp ~751). (high)
NXG-UPPER (Tensai75/nxg-upper README): data and par2 articles numbered separately; Message-ID = SHA256("NxGHeader:type:number") -> DETERMINISTIC, all MIDs derivable from the NxG header; header carries total data/par2 counts. (high, README)
Rules: (1) same MID twice = Nyuu repost; same part + new MID seconds apart = ngPost retry; count once. (2) Nyuu Date non-decreasing per FIFO. (3) Link cross-group slices by yEnc name/size/total + interleaved timing.
Not covered: Postie, GoPostStuff, newsmangler source; ngPost Date.
