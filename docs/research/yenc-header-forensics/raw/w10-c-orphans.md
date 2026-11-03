# W10-C: Orphan articles from retries (Nyuu, ngPost)
Sources: /tmp/claude-0/-home-claude/a4d53905-78b9-58a1-82a6-938c4e3c790c/scratchpad/w5b/{animetosho_Nyuu,mbruel_ngPost}

## 1. Nyuu: retries after the server may have accepted the article
- A. Timeout waiting for 240 (POST response): nntp.js:995 `_triggerError('timeout')` -> nntp.js:397-403 re-queues request while `req.tries <= requestRetries`; POST re-sent with new ID (nntp.js:828 randomizeMessageID). Timer: nntp.js:920 `_requestSetTimer(type, opts.timeout)`. Default timeout: not confirmed in read code.
- B. Timeout during post-upload (after body sent): nntp.js:975 `_requestSetTimer('post-upload', opts.postTimeout)`; nntp.js:372-391 `on-post-timeout` action `retry` or `strip-hdr=` re-posts with a new ID; `ignore` treats it as 240 (nntp.js:416-418). Strongest orphan source. Default for on-post-timeout: not confirmed.
- C. Connection reset/close after body sent: nntp.js:397-401 requeues `post` for reconnect; nntp.js:828 gives a new ID. Retry count `requestRetries` (nntp.js:397); default not confirmed.
- D. Server-side 441 after upload: nntp.js:857-860 `postRetries` with `postRetryDelay` (nntp.js:858-860), new ID each time via doPost. Only fires if the server rejected, so likely no orphan, except servers that store then return 441.
- E. Post-check STAT miss -> repost: uploader.js:383-389 (STAT by messageId), uploader.js:417-421 `check.postRetries` re-queues via self.queue.add(post) and post.c.post -> new ID (nntp.js:828). Cause here is propagation lag. Check settings: check-tries (bin/nyuu.js:373), check-delay (:365), check-retry-delay (:369), check-post-tries (:382). Defaults not confirmed.
- F. STAT itself retried on connection loss: nntp.js:790-794 uses _doRequest, so requestRetries applies. Not an orphan source.
- Note: on a 240 with a different ID, nntp.js:~868 adopts the server ID (`post.messageId = m[1]`), so the NZB gets the server ID.

## 2. Nyuu: 441/435 and ID adoption
- 441 for POST is in FAILED_POST_CODES (nntp.js:35) and treated as failure, not success (nntp.js:857).
- 435 is not present. IHAVE failures are 436/437 (nntp.js:36). No "duplicate" (435/436 as success) path found.
- ID adoption: yes, only on 240 with a `<id>` in the response (nntp.js ~861-872, RE_POST). Logs a one-time warning, sets post.messageId = server ID.

## 3. ngPost
- Retry on connection loss: NntpConnection.cpp:217-222 `_nbDisconnected++ < nbMaxTrySending()`, calls `_currentArticle->genNewId()` (new QUuid, so new ID). Re-sends whole article.
- Retry on error line: NntpConnection.cpp:385-389 (WAITING_ANSWER, non-240) -> `tryResend()`; NntpArticle.cpp:117-124 assigns a new QUuid per resend.
- Timeout: NntpConnection.cpp:298-301 `onTimeout` closes socket (only with __USE_CONNECTION_TIMEOUT__, set in ngPost_cmd.pro:25). Default sock_timeout 30000 ms (NgPost.h:260). Closing mid-body -> onDisconnected retry (217).
- nbMaxTrySending = 5 (NntpArticle.cpp:29), shared by both paths.
- 240 ID adoption: NntpConnection.cpp:350-367 parses `<id>` and calls overwriteMsgId (NntpArticle.cpp:136).
- 441/435: no special handling. Any non-240 in WAITING_ANSWER is a retry (385-389), so 441 is retried with a new ID.
- NntpCheckCon (NntpCheckCon.cpp:247) sends STAT; I found no re-post path in the check connection.

## 4. XOVER orphan signature
Expect: same Subject (incl. "yEnc (n/m)"), same From, same Bytes and Lines as the NZB segment, different Message-ID, posted seconds to minutes apart, close N on Highwinds-style counters.
Detection rule: group XOVER rows by (Subject, From, Bytes, Lines); a group with >1 Message-ID where at most one is listed in the NZB is an orphan candidate. Confirm with a close timestamp (within the retry window) and a Message-ID shape match (Nyuu/ngPost IDs differ from the NZB's, so compare generator patterns).
What it proves: same session and uploader, and that the server accepted the article once (the body was stored). It does not prove retrieval; check BODY/ARTICLE.
Backup value: the body is the same as the listed segment, so it is a real copy of that part, not a guess. But it is not in the NZB, so it only helps if the NZB is rewritten to point at it, or if a downloader is given the ID. Not free: it adds an unlisted copy and may be a dupe for the same part.

## 5. Rate and public discussion
- gh search (Nyuu): no issue found about duplicate articles from retries or 240/timeout mismatch. Related: #40 (POST 500 "Max connect time exceeded", 2017), #4 (intermittent connection, 2016), #99 (hangs at 99%, 2022). ngPost #42 "Should use another connection if one fails" (2020).
- Rate: unknown. Retries that orphan need a timeout or drop after the server stored the body. Likely uncommon per article but nonzero on long uploads with flaky links and post-timeout retry. Not measurable from public sources; would need XOVER scans of a known upload.
