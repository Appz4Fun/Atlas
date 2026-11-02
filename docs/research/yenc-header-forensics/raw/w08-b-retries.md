# W08-B: Usenet tool retry / Message-ID / header-order answers

Source clones: /tmp/claude-0/-home-claude/a4d53905-78b9-58a1-82a6-938c4e3c790c/scratchpad/w5b/ (paths below are relative to that dir)

| Tool | Question | Answer | file:line | VERIFIED/INFERRED |
|---|---|---|---|---|
| postie (javi11) | Default part size when unset | 750000 bytes. Applied by the load-time default when value <= 0 | javi11_postie/internal/config/config.go:574-575 | VERIFIED |
| postie (javi11) | Other size literals | 768000 appears in a config struct literal (`ArticleSizeInBytes: 768000, // 768KB`); not confirmed as the load-time default | javi11_postie/internal/config/config.go:1110 | INFERRED (which constructor it belongs to not checked) |
| postie (javi11) | Shipped YAML/docs | config-example.yaml and docs use 750000; docs say "default: 750KB" | javi11_postie/config-example.yaml:21; javi11_postie/docs/docs/configuration.md:206 | VERIFIED |
| postie (kipsilabs) | Same as above | Identical values at same lines (574-575 = 750000, 1110 = 768000, config-example:21, configuration.md:206) | kipsilabs_postie/internal/config/config.go:574-575,1110 | VERIFIED |
| postie | Message-ID on POST retry (stale conn) | Same article object re-sent, so same Message-ID. Up to 3 attempts | javi11_postie/internal/poster/reposter.go:68-71 (postYenc); javi11_postie/internal/poster/poster.go:1591 | VERIFIED |
| postie | Message-ID on checkLoop repost (failed articles) | Same Message-ID kept; code comment says intentional. Failed articles re-queued as-is, no GenerateMessageID call | javi11_postie/internal/poster/poster.go:1022-1025, 1040-1046, 1094-1102 | VERIFIED |
| postie | Message-ID on durable STAT-missing repost | Reuses manifest record's Message-ID (articleFromRecord keeps MessageID) | javi11_postie/internal/poster/reposter.go:113, 157-187; javi11_postie/internal/verification/service.go:549-573 | VERIFIED |
| postie | Where Message-IDs are generated | Only in addPost, once per segment (random or NXG format) | javi11_postie/internal/poster/poster.go:1410-1424 | VERIFIED |
| postie | Header order | postYenc passes headers as a Go map (`Extra map[string][]string`) to nntppool v4 (external, not in clone). Actual wire order is decided inside nntppool, so not verified here. Article.Encode builds headers in a map and ranges over it (random order), but Encode has no non-test callers found | javi11_postie/internal/poster/reposter.go:29-38; javi11_postie/internal/article/article.go:86-106 | INFERRED for wire order (nntppool source not present); VERIFIED that article.go:104 ranges a map |
| Nyuu (animetosho) | Message-ID on send retry | Regenerated. nntp.js calls randomizeMessageID() on every doPost attempt unless keepMessageId is set; uploader retry path calls doPost again | animetosho_Nyuu/lib/nntp.js:827-828; lib/uploader.js:243-270 (dumpPost, doPost.bind on err); lib/article.js:240-258 | VERIFIED |
| Nyuu | Message-ID on post-check repost | Regenerated. Post is pushed back to queue (uploader.js:420-431), next c.post -> doPost -> randomizeMessageID (unless keepMessageId) | animetosho_Nyuu/lib/uploader.js:420-431; lib/nntp.js:827-828 | VERIFIED |
| Nyuu | Exception: raw posts | keepMessageId defaults to true for raw posts, so the ID is kept on retry | animetosho_Nyuu/bin/nyuu.js:1087-1089; lib/article.js:240; lib/nntp.js:828 | VERIFIED |
| Nyuu | Message-ID in NZB | Segment entry uses post.messageId after post; regenerated IDs are what get written | animetosho_Nyuu/lib/uploadmgr.js:361 | INFERRED (ordering vs. regen not traced in full) |
| ngPost (mbruel) | Message-ID on send retry | Regenerated. tryResend() assigns a new QUuid (`_id = QUuid::createUuid()`); header() builds Message-ID from `_id` on each write | mbruel_ngPost/src/nntp/NntpArticle.cpp:117-124, 136-150; src/NntpConnection.cpp:387-392 | VERIFIED |
| ngPost | Retry cap | Default 5 (sNbMaxTrySending); CLI option `-r`/`retry` | mbruel_ngPost/src/nntp/NntpArticle.cpp:29; src/NgPost.cpp:191, 1519-1521 | VERIFIED |
| ngPost | Post-check (STAT) repost | Not found. NntpCheckCon.cpp checks articles but no repost-on-missing path was traced | mbruel_ngPost/src/NntpCheckCon.cpp:133-142 | INFERRED (not fully read) |

Summary: postie keeps Message-IDs on every retry and repost path (including durable STAT repost). Nyuu and ngPost regenerate on send retry (Nyuu also on post-check repost), unless Nyuu keepMessageId is set (raw posts).
