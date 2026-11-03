# W8A decoder edge cases (Haiku)
- rapidyenc is the de facto reference (NZBGet and sabctools both use rapidyenc-derived code). It skips CR/LF anywhere, keeps spaces/tabs, carries escape state across chunks; isRaw mode handles dot-stuffing and "\r\n.\r\n"; end states NONE/CONTROL/ARTICLE.
- "=" at line end: rapidyenc decodes =CR as an escaped CR, python yenc differently -> decoders disagree.
- Nyuu 2016 unescaped last-column critical char (node-yencode #3), fixed later.
- Spec says decoders cut leading/trailing spaces; rapidyenc does not -> don't rely.
- Rules: strip CR/LF; carry escape; unstuff dots once at one layer; =yend only at line start.
MY CORRECTION: a part whose decoded CRC equals pcrc32 is trustworthy regardless of oddities (the CRC is the proof). Ambiguity flags only matter on failure: retry with the alternate interpretation (e.g. dot-unstuff on/off, =CR handling) and accept if one matches; otherwise mark unverified. Match rapidyenc behaviour by default so results agree with users' downloaders.
