# Wave 9 (10:56Z to 11:00Z): spec validation, retry signature, remaining unknowns

## Results
1. Rust spec validated against all 72,334 segments: same labels as the Python classifier for all 21 labels. 42,138 IDs hit 2+ rules (7 rule pairs) and the intended rule always wins. Four fixes are now appended to raw/w08-c-rust-spec.md: tighten JBinUp, add JBinDown, restrict Usenet.Farm loose to UFK/news.usenet.farm hosts, and add an nzb_date_bad flag.
2. Time fields hold: Nyuu ms within ±1 day of the NZB date for 99.56% of segments (every miss is one NZB dated 2033). Highwinds T1 869/869 within ±1 hour.
3. Mixed-tool NZBs: 17 of 246, but only 3 are genuinely two tools (f2dee72b8c, de7cbcbe83, 16e4f358e2). The rest are case splits, strays or fixtures. A real two-tool NZB is itself a signal: a merged or repaired post.
4. Retry signature does not reach NZBs. In 332 public NZBs, only 4 have duplicate segment numbers, and none is a Nyuu or ngPost retry pair. Nyuu writes only the ID that finally succeeded, so a retried part leaves one NZB entry. Any failed attempt that the server did accept becomes an orphan article that no NZB references. INFERRED from Nyuu source plus this sample.
5. Remaining unknown NZBs are custom or exported, not public tools:
   - 7971368e52: From "hep <posted@nc>", random [0-9a-z]{6,10}@[0-9a-z]{6,10}.[0-9a-z]{2,5}. It is a sample file in nzbget-ng's test data.
   - cf32b14460: From changes per batch of 68 articles, each batch inside one second. Random From per connection batch is an obfuscation tell.
   - c09c711e35: an NZB export (NGMS nzb_export sample) with letter-only local parts.
   No generator found for any of them.

## What it means for Atlas
- The classifier is ready to port as specified, with the wave 9 fixes.
- Do not expect retries to show as duplicate segments. Treat a duplicate segment number with different bytes as a repost or a merge, not a retry.
- Random From rotating per batch of articles inside one second points to one session. Group by (time-second bucket, batch size), not by From.

## Next wave (10, last)
(a) Which posting tools rotate From per connection or batch (ngPost random poster, Nyuu --from templates), and the batch size each uses. (b) SABnzbd and NZBGet handling of duplicate segment numbers, from source. (c) Orphan articles: can Atlas find retried, orphaned parts in XOVER (same subject and part, no NZB)? Then the final report.

## Addendum (11:02Z, with the Opus yEnc thread)
cf32b14460 is an open fingerprint row: "base64url variable-length IDs, From rotates per 68 articles (per connection?)". The Message-ID style stays the same across From boundaries, so it is not a wrapper restarting the poster. The From values are bare base64url addresses from the same generator. The Opus thread saw the same From shape fixed across a 2021 ngPost post, which suggests one unidentified wrapper.
- 11:28Z correction: From changes per FILE, not per 68 articles. Segments per From are 44×68 (equal volumes), 1×46 (last volume), and 1, 2, 2, 3, 5, 9, 17, 33, 65, 129 (a PAR2 index plus doubling recovery volumes). The IDs look like per-character draws from a 64-symbol set with random length (nanoid-like), per the Opus thread, and not unpadded base64.
