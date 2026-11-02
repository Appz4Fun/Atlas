# Wave 3 C: public real header/NZB data for validation (Haiku, ~17 calls) -- mostly DEAD END
- No public real binary header dumps or real NZBs found. Only synthetic fixtures:
  - yencoding crate header.rs tests (https://docs.rs/crate/yencoding/0.2.0/source/src/header.rs)
  - nzb-rs doctest NZB (https://docs.rs/crate/nzb-rs/0.4.4/source/src/lib.rs)
  - Harley Hahn NZB example, MID "part1of1.<rand>@<host>.local" convention (https://harley.com/usenet/file-sharing/15-understanding-nzb-files.html)
- 103B-token Usenet text corpus 1980-2013 excludes binaries (secondhand article).
- Animetosho dumps, SABnzbd/NZBGet test data: not located.
Conclusion: Date/Injection-Date skew, file-boundary overlap, fills, Injection-Info fingerprints are UNVALIDATED. Best real data is Atlas's own: its sealed article log (~77 GB) and DB of overview rows on prod. Validation should be an offline query over that.
