# w09-c: generator search for 3 unclassified NZBs

Source rows: scratchpad w7a/segs.tsv (nzb-id, date, msgid, host, from). Per-NZB slices saved beside it.
Names in From values are redacted here. Confidence: VERIFIED = checked against data or source; INFERRED = judgment.

## 7971368e52 (2453 msgs, 1 From, 6 distinct seconds 1677528951-1677528956)
- From: constant "hep <posted@nc>" (NZB escapes it as &lt;posted@nc&gt;). VERIFIED.
- Local part: 6-10 chars, alphabet [0-9a-z] only, lengths roughly uniform 6-10. VERIFIED.
- Host: exactly 2 dot labels for all rows. Label 1 is 6-10 chars, label 2 is 2-5 chars, alphabet [0-9a-z]. VERIFIED.
- Host field (col 4) equals the msgid host in 2453/2453 rows. VERIFIED.
- No constant, sequential, or timestamp part: no shared 3-char prefixes between adjacent rows, and base36 of the local part is not near date*1000. VERIFIED.
- Regex: ^[0-9a-z]{6,10}@[0-9a-z]{6,10}\.[0-9a-z]{2,5}$ . VERIFIED (shape).
- Generator: the NZB is itself a sample file in nzbget-ng/nzb-subject (samples/*.nzb, the exact same msgids and date). The search for "posted@nc" returns only that file. So this is a sample, not a poster source. Poster tool: UNKNOWN.
- Label: "rand-b36-2label-hep" INFERRED. Confidence in any tool attribution: none.

## cf32b14460 (3304 msgs, 55 From values, 1778828136-1778828171, 35 s)
- From: random mailbox per batch, not constant. 44 From values each cover exactly 68 msgs, all in one second. Other From groups are 1, 2, 3, 5, 9, 17, 33, 46, 65, 129 msgs. VERIFIED.
- Local part: 20-29 chars, alphabet [A-Za-z0-9_-] (URL-safe base64 alphabet), lengths roughly uniform 20-29. VERIFIED.
- Host: label 1 is 5-9 chars from the same alphabet, label 2 is exactly 3 chars from the same alphabet (all 3304 rows). VERIFIED.
- No timestamp or sequence (base36 test negative, no shared adjacent prefixes). VERIFIED.
- Regex: ^[A-Za-z0-9_-]{20,29}@[A-Za-z0-9_-]{5,9}\.[A-Za-z0-9_-]{3}$ . VERIFIED (shape).
- Generator: no constant literal to search. Shape fits random base64url padding with a fixed 3-char TLD-like label. Batch-of-68-per-second From pattern is a poster-side behaviour. Tool: UNKNOWN. Label: "rand-b64url-20_29@5_9.3" INFERRED.

## c09c711e35 (2138 msgs, From constant "[name] (...@bin.up)", 1766779986-1766780011, 26 s)
- From: constant "[name] (...@bin.up)" (display name plus address). VERIFIED.
- Local part: 27-44 chars, alphabet [A-Za-z] only (no digits, no - or _), roughly uniform lengths. VERIFIED.
- Host: 2-6 dot labels, alphabet [A-Za-z0-9_$.-]. Total host length 16-27. Labels 1-18 chars, with $ , - and _ appearing in labels. VERIFIED.
- Host field (col 4) equals the msgid host in 2138/2138 rows. VERIFIED.
- No timestamp or sequence. VERIFIED.
- Regex: ^[A-Za-z]{27,44}@[A-Za-z0-9_$.-]{16,27}$ . VERIFIED (shape, observed ranges).
- Generator: exact search for the From literal returns one hit, a sample export nzb_export/*.nzb in TheDancingDeveloper-org/NGMS. That file carries the same msgids and date (1766780005), so it is an export of this same posting, not a poster. Repo search of NGMS for "message-id" gives 0 code hits. The "bin.up" domain returns about 20k noisy hits, so it is not usable. VERIFIED (search results).
- Repo-name searches for yencee, newspost, nzb-monkey, nntp-rs, geri, riverup returned 0 results. These queries returned zero for every term, so the result is likely a query-syntax issue and not conclusive. Not verified against those tools.
- Label: "rand-alpha-local_hostmix-$" INFERRED. Tool: UNKNOWN.

## Scope and limits
- Not done: did not read w08-a-unknown-family.md, so no cross-check with 0421847faa. Time budget was 10 minutes.
- No candidate tool (yencee, newspost, Newsbin, UE, PowerPost, GoPostStuff, nzb-monkey, keimpema, geri, riverup, nntp-rs, Go/Rust posters) was confirmed or ruled out from source. None matched a literal.
- binsearch and nzbindex were not used (blocked).
- Confidence on attribution for all three NZBs: none. Shape regexes are VERIFIED. Labels are INFERRED.
