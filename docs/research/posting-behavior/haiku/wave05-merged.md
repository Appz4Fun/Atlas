# Wave 5 merged (08:55-08:58Z)
- No existing indexer or downloader handles either case. nzb-monkey-go, nZEDb and pynab put the group in the key; NNTmux merges only true crossposts via Xref. All key on Subject+From and none read yEnc, so Postie full obfuscation turns every article into its own binary.
- What survives maximum obfuscation: per FILE, (yEnc size=, total=, article size) plus the begin/end chain; per RUN, header order, Date presence/zone ("UTC" Postie, none ngPost), pcrc32 padding, and for default ngPost the From/MID domain (@ngPost.com / @ngPost).
- The yEnc key lives in the body, so header-only grouping must rely on :bytes/:lines near-constancy per article; one BODY per candidate confirms.
- TRiToN correction: Batman.Knightfall...2160p...REMUX-TRiToN is ONE 47.8 GiB MKV (xrel.to NFO), so Atlas's slices are RAR/PAR2 volumes of one file spread over groups. The PAR2 bridge + RAR volume chain is the right fix, not comics heuristics. nzbking has a details page that may list its groups.

## Resolved after merge (10:54Z, from the Haiku15 posting thread)
- Postie header order: the fixed order ships. Postie pins nntppool v4.22.3 and posts via PostHeaders.WriteTo (post.go:168-236): From, Newsgroups, Subject, Message-ID, Date, then Extra headers sorted. article.Encode (map order) is dead code. Retries keep Message-ID, From and Date.
