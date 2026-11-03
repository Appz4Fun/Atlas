# W6B raw split files (Haiku, ~09:22Z)
- Krusader/TC .crc sidecar: "filename=foo size=... crc32=..." for the ORIGINAL file | alt key vs an unsplit or RAR'd post of the same file | 1 article. https://docs.kde.org/trunk5/en/krusader/krusader/splitter.html ; ghisler.ch board p=3531
- HJSplit/TC/Krusader/GNU split parts have no header: part N = original bytes [N*vol,(N+1)*vol). First .001 article starts with the original's magic bytes (MKV EBML etc.), so container/stream keys from other threads apply directly. forums2.newsbin.com p=195126
- yEnc =ypart begin/end are offsets within the posted file (.00N), so original offset = (N-1)*vol + begin; pcrc32 then compares with interval CRCs of an unsplit raw post (same CRC-combine method as the haiku15 LCM-grid test).
- SABnzbd joins .001 sets (filejoin) after par2; NZBGet behaviour unverified. No prevalence stats for raw/split posts; obfuscated RAR+PAR2 still dominant per Newsbin threads.
- Unverified: HJSplit/FFSJ/7z raw-split headers or presets.
