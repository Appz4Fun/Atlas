# w08-a: unknown Message-ID family profile and generator search

Scope: the 10,370 "unknown" rows from w7a classify.py (72,334 segs, 246 NZBs). Source: segs.tsv. Method: python -I profiling, GitHub code search, raw source reads of Nyuu and ngPost.

## 1. Where the unknowns actually live
- Unknowns are NOT one family. 87 NZBs, but 4 NZBs hold 10,021 of 10,370 (97%):
  - 0421847faa: 3,279 rows, From = "30385ec8... <pbe...>" (one From per NZB)
  - 7971368e52: 2,453 rows, From = "hep <posted@nc>"
  - cf32b14460: 2,267 rows, From = "MH7UIV...@C6AMazKR-6..." (long From, local parts 27-29 chars)
  - c09c711e35: 2,022 rows, From = "Name (x@bin.up)" form, local parts 38-44 chars
- Each of these NZBs has exactly one From. Shape is per-NZB, not per-corpus.
- The summary's "x{11}@x{32}" family is 0421847faa: 2,419 of its 3,279 rows match the strict regex below. The remainder have other host lengths (e.g. 37).

## 2. Profile of the main family (0421847faa, strict subset n=2,419) - VERIFIED from data
- Local part: 11-20 chars, mixed-case alnum, uniform over lengths 11..20 (~240 each).
- Host: 28-36 chars, mixed-case alnum, uniform over 28..36 (~270 each). Charset [0-9A-Za-z].
- Host starts with a lowercase-hex run of 17 chars or more in 2,393 of 2,419 rows (17: 1,774; 18: 484; 19: 135). Random alnum would give this run far less often, so the hex-looking prefix is deliberate. Remainder of host is mixed case.
- Timestamp encoding: NO. NZB date 1743978609 = 0x67F30071 (ms 0x1960D39B968). 0/2,419 hosts contain the hex of the date or ms value. Prefix8 values span 0x00013C5F..0xFFDC035C, far wider than the date's neighbourhood.
- Prefix constant per NZB: NO. 2,419 distinct 8-char prefixes among 2,419 rows.
- Prefix constant per From: NO (one From per NZB, so no per-From test beyond that).
- Gen mode: per-segment random local part plus per-segment random host. Hex-looking prefix is fixed-length, not time-based. INFERRED.

## 3. Source search
- Nyuu (VERIFIED from source, lib/article.js createMessageId and help-full.txt):
  - Default --message-id is `${rand(24)}-{timestamp}@nyuu`.
  - Default generator emits 24 chars alternating [A-Z][a-z], then "-", then a 13-digit timestamp, then "@nyuu".
  - Templates support {timestamp} and ${rand(N)}, so a custom template could produce a mixed-case alnum local part.
  - Ruled out for this family: no timestamp, no "-", host is not a fixed string. Nyuu's strict grammar matches 23,831 rows elsewhere in the corpus.
- ngPost (VERIFIED from README and help): Message-ID is a UUID-style hex value followed by "@" and the -z signature (default "ngPost"). Obfuscation mode randomises Subject, Message-ID and From. Shape is hex32@ngPost, which is the existing ngpost rule. Ruled out for this family.
- gh search (code search, repos Nyuu / ngPost / JBinUp / Newsleecher / newsup / nzbget / sabnzbd / yencee): no source found that emits a Message-ID with a 28-36 char mixed-case alnum host. JBinUp hits were unrelated repos. NOT FOUND.
- binsearch/nzbindex blocked, as expected.

## 4. Regex (main family, matches 2,419 rows, all in NZB 0421847faa)
  ^[A-Za-z0-9]{11,20}@[A-Za-z0-9]{28,36}$
  Confidence: VERIFIED from data (match count). Generator attribution: NOT FOUND.
  Refinement with hex-prefix check: ^[A-Za-z0-9]{11,20}@[0-9a-f]{17,19}[A-Za-z0-9]{11,19}$  (INFERRED, not counted)

## 5. Second family (zikeqsy4p@i3i47n5m.hpd6 type)
- Sits in NZB 7971368e52 (2,453 rows, From "hep <posted@nc>", 2023-02 date). Locals 8 and 10 chars, lowercase. Hosts like "zor2h64.1pb6" (dotted, 7 + 4 chars).
- Not regex-verified in this pass. Confidence: INFERRED from samples only.
  Candidate: ^[a-z0-9]{8,10}@[a-z0-9]{6,8}\.[a-z0-9]{4,6}$

## 6. Answer
- Largest unknown family: NZB 0421847faa (3,279 rows). Producing tool: NOT FOUND. Nyuu and ngPost are ruled out by grammar.
- Honest best attribution: a custom or unlisted poster with per-NZB From. Confidence: INFERRED.
- Next step for attribution: check whether any of the 4 NZBs' Subject or yEnc headers share a poster-specific pattern, or query the GitHub code search for the "hep <posted" From string.
