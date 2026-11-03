# Wave 08-B: how big is the obfuscation / split-posting problem?

Bottom line: no public source gives a measured prevalence for any of the three categories (a) obfuscated posts, (b) per-file group or poster rotation, or (c) posts with no public NZB. Every number found is either an NZB-age/category statistic that says nothing about obfuscation, or an unquantified forum claim. Any prevalence figure we use will be a guess until we measure it ourselves on a sample.

## Tooling defaults (measured from source)

- Postie `group_policy` default is `each_file`; its `obfuscation_policy` options are `full`, `partial`, `none`. `full` randomises subject, yEnc filename, date (within 6 h) and poster per article. `partial` keeps one poster per post. Source: https://github.com/javi11/postie (docs/docs/configuration.md, docs/docs/obfuscation.md; cloned to /tmp/w8b/postie). Confidence: measured (read from docs and code).
- ngPost `GROUP_POLICY` enum is `ALL`, `EACH_POST`, `EACH_FILE`. The default in the constructor is `ALL`, not `each_file`. Article obfuscation (`_obfuscateArticles`) defaults to false and is an opt-in flag. Its `invisible` mode fully obfuscates the article. Source: https://github.com/mbruel/ngPost (src/NgPost.cpp line ~301, src/NgPost.h line 99; cloned to /tmp/w8b/ngpost). Confidence: measured (source).
- NewsUP (https://github.com/demanuel/NewsUP) has an `obfuscate` option in its config. Its README warns that obfuscated uploads produce "non 100% correct nzbs", because full compliance does not allow obfuscation. The README gives no prevalence. Confidence: measured (README).
- Implication: a Postie default upload is already per-file poster-rotated only if obfuscation is also on. The `each_file` setting alone does not hide a poster. Confidence: measured (docs).

## Measured or dated prevalence

- r/usenet, 2024-12-28, "An examination of NZB age and category": a user with 9 indexer memberships (4 shown) sampled over 450M NZBs in categories 2000/3000/5000/6000. Age split was 0-1500 d 45%, 1501-3000 d 26%, 3001-4500 d 25%, 4501-6000 d 4%. Category split: 2000 10%, 3000 18%, 5000 52%, 6000 20%. The post gives no obfuscated or unmatched figure. Source: https://pol1.lr.ggtyler.dev/r/usenet/comments/1ho6rfz/an_examination_of_nzb_age_and_category (mirror of reddit). Confidence: measured (by a user, unaudited). Relevance: indirect. It shows NZB supply skews to recent posts, not how many posts lack an NZB.

## Forum claims (no numbers)

- Newsbin forum, 2017-10-06 to 10-10, "Regarding obfuscated posts": posters obfuscate RAR names with what looks like a one-way hash. Quade and dexter (Newsbin site admin) both guess at the method. No groups or counts. Source: https://forums2.newsbin.com/viewtopic.php?p=221348. Confidence: forum claim (guess).
- Newsbin forum, 2018-07-26, "Internet Search - Obfuscated Posts": Newsbin says obfuscated posts "still appear in results" and that a decoded copy may show under a different entry. A user says many obfuscated sets are now searchable. Source: https://forums.newsbin.com/viewtopic.php?p=225526. Confidence: stated by operator (qualitative).
- Newsbin forum, 2018-10-24, "Internet Search - Many Items Unfindable": a user calls obfuscated posts from "certain posters or indexers" "significant in number". Site admin dexter says the team "wrestle[s] with this issue constantly" and that some posts cannot be rebuilt without the original NZB. No counts. Source: https://forums.newsbin.com/viewtopic.php?p=226558. Confidence: operator statement (qualitative), user claim (unquantified).
- Newsbin forum, 2021-08-22, "New type of Usenet posts?": a downloader's site reports "a new way that a group of posters are posting" that breaks its indexer. Quade (Newsbin) says posters keep changing methods to avoid removal, and guesses the cause is obscured files. No groups or counts. Source: https://forums.newsbin.com/viewtopic.php?p=231725. Confidence: forum claim (second-hand).
- Newsbin forum, 2022-02-20 to 02-25, "new obfuscation method": NZBs with random 4-character names but correct extensions. Quade says the files are unobfuscated MKVs with PAR files, and that "the posting group, not the indexer, controls the final filename". A user sees the set on one of four indexers. Source: https://forums2.newsbin.com/viewtopic.php?p=232290. Confidence: forum claim (anecdote, four NZBs).
- r/usenet, 2024 (unnamed commenters, OP recollection): "most of the unobfuscated ones are spam" for newer posts; newer obfuscated posts "look a LOT like" older sporge; "most of the meat and potatoes are within the indexers". No percentages. Source: https://reddit.rtrace.io/r/usenet/comments/1dotqm4/im_so_old_im_new_again_nzbs_say_theyre_good_but. Confidence: forum claim (opinion).
- NGProvider marketing page (2026 copyright): "Many posts now use obfuscated filenames", no figures. Source: https://www.ngprovider.com/stealth-usenet.php. Confidence: forum-grade marketing claim.
- Cogipas.com (page dated 2019, updated to 2025): states that obfuscated files "might still be removed within a few hours" or persist "for days, weeks, or even years". No measurement. Source: https://www.cogipas.com/obfuscated-usenet/. Confidence: guess.

## Not found in this run

- binsearch.info, nzbking.com, nzbindex.com: not fetched (time budget).
- Anime Tosho stats or methodology: WebSearch returned only unrelated pages. Not found.
- NZBgeek, DrunkenSlug, NZBFinder, nzb.su, Spotweb operator stats: no stats pages found. The r/usenet thread on DrunkenSlug was blocked by robots.txt.
- NNTmux / newznab issues quantifying unmatched or obfuscated releases: search returned only code pages. Not found.
- Academic measurement of hash-named or obfuscated share on alt.binaries.*: none found. The hits were off-topic (a 2001 pornography image study, a binary-diffing paper).
- Group-level breakdown (a.b.boneless, a.b.misc, a.b.frogs): no source names these groups in connection with obfuscation. Unverified. Do not use this breakdown without measurement.

## Trend, 2017 to 2026 (qualitative only)

- 2017 to 2018: obfuscation is discussed as new; indexers (Newsbin) start to see unsearchable sets. Confidence: forum claim.
- 2021 to 2022: a new poster-level scheme breaks some indexers; the scheme differs from the older hash names. Confidence: forum claim.
- 2024: commenters describe obfuscated and unobfuscated posts side by side, with more spam among unobfuscated ones. Confidence: forum claim.
- No source gives a trend line with numbers.

## What this means for prioritising linking work

- Treat the prevalence as unknown and measure it first: sample a few days of headers from one or two groups, and count hash-like subjects, random posters and per-file poster rotation. That number sets the priority.
- Default tooling matters more than the rare cases: Postie's default `each_file` plus `full` obfuscation produces per-article random posters, so linking should key on the yEnc/NZB structure and not on poster or subject.
- Posts with no public NZB cannot be linked from public indexers at all; expect the gap to be in private-indexer data, and rank that as a data-access problem before a matching problem.
