# Wave 8 (10:26Z to 10:31Z): unknown ID family, retry behaviour, Rust spec

## Results
1. The "unknown" Message-IDs are not one tool. 4 NZBs hold 97% of the 10,370 unknown segments, each with a single From. The biggest (NZB 0421847faa, 3,279 segs) uses `^[A-Za-z0-9]{11,20}@[A-Za-z0-9]{28,36}$` with a 17+ char lowercase-hex run at the start of the host. It is random per segment, with no timestamp in it and no repeats. No public generator matched (Nyuu, ngPost, JBinUp, Newsleecher, nzbget, sabnzbd, yencee were checked). Treat it as a custom poster; it still gives "same tool" within an uploader's history. raw/w08-a-unknown-family.md.
2. postie part size settled: 750000 when unset (internal/config/config.go:574-575, verified by orchestrator). 768000 is only a struct literal at config.go:1110. Fingerprint conflict C9 closed.
3. Retries and Message-IDs (C4, C10 closed):
   - postie keeps the same Message-ID on POST retry, checkLoop repost and STAT-missing repost (poster.go, reposter.go). IDs are minted only in addPost.
   - Nyuu mints a new ID on every send attempt unless keepMessageId is set (lib/nntp.js:828, verified by orchestrator: "best to make it unique for retries"). Raw-post mode defaults keepMessageId=true (bin/nyuu.js:1087-1089).
   - ngPost mints a new QUuid on resend (NntpArticle.cpp:117-150), up to 5 tries.
   - Atlas impact: for Nyuu and ngPost, a part posted twice in one session has two IDs, so expect duplicate segment numbers with different IDs in the same session. That is a retry, not a twin. For postie, the same ID seen twice is the same article.
4. postie header order (C11): article.go:104 ranges a Go map, but the live path hands headers to nntppool v4, so wire order is still INFERRED.
5. Rust spec written (raw/w08-c-rust-spec.md, 150 lines): tool enum, ordered regex rules with extracted fields, precedence, std+regex function signatures, SQL columns, 10 test vectors (all 10 confirmed present in segs.tsv by orchestrator), and what each label proves.

## Next wave (9)
Lean in on validation: (a) check the Rust spec rules against the full corpus for false positives and overlaps, (b) the retry signature: search segs.tsv for duplicate part numbers with different IDs inside one NZB, (c) the second unknown family (NZB 7971368e52, From "hep <posted@nc>") and the remaining two big unknown NZBs.
