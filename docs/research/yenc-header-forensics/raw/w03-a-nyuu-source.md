# Wave 3 / Agent A: Nyuu defaults from source (Haiku, then re-checked by the orchestrator with curl against master)
All lines below were VERIFIED against raw.githubusercontent.com/animetosho/Nyuu/master on 2026-11-02:
- config.js:83 `articleSize: 716800` (700 KiB). :84 `bytesPerLine: 128`. :89 `keepMessageId: false`.
- config.js:102 default `Newsgroups: 'alt.binaries.test'`, :104 `Path: ''`, :108 `'User-Agent': 'Nyuu/' + version`.
- lib/article.js:198-228 createMessageId: 24 chars that alternate uppercase/lowercase starting uppercase (`([A-Z][a-z]){12}`), then `-` and the 13-digit zero-padded genTime ms, then `@nyuu`. So the regex is `^<([A-Z][a-z]){12}-\d{13}@nyuu>$`, and genTime is also the Date.
- article.js:133 Date = `new Date(post.genTime).toUTCString()`, so "GMT" format.
- article.js:43 + _writeData: `=ybegin part=N total=T line=128 size=S name=F` (part= and total= even on single-part files), then `=ypart begin=B end=E`, then `=yend size=n part=N pcrc32=<hex lower>` with ` crc32=<hex>` ONLY on the last part (crc32_combine).
- lib/nzb.js:51-52: `<?xml version="1.0" encoding="utf-8"?>` (encoding name from the option) plus the newzBin 1.1 DOCTYPE. :86 `<file poster= date=(unix seconds) subject=>`. :99 `<segment bytes= number=>`. Whether bytes= is raw or encoded was not settled.
- A RawPost from stored articles keeps its messageId and genTime (article.js ~499-512), so a repost from a dump has the same ID.
Map corrections: Nyuu size is 716,800 (not 700,000). Message-ID grammar is the alternating-case one. The "rand24-ts" description is right but loose.
