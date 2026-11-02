# W3B cross-seed size-set matching (Haiku, ~08:02Z)
- cross-seed matchMode strict/flexible/partial; fuzzySizeThreshold default 0.02; partial ignores nfo/sample/txt/subs up to 200 MiB (ignoreNonRelevantFilesToResume); seasonFromEpisodes. https://www.cross-seed.org/docs/basics/options , /docs/tutorials/partial-matching
- qui hardlink mode: required byte coverage = 100% - tolerance (0 = exact). https://github.com/autobrr/qui/discussions/2169
- autotorrent/qbit_automatch: size+extension only, duplicates error, fuzzy-name tie-break. https://raw.githubusercontent.com/styper/qbit_automatch/HEAD/README.md
- FP case: a RAR'd set had the same total size as the unpacked content -> compare per-file lists, never totals. https://github.com/ccf-2012/seedcross/issues/20
- Sonarr picks the largest identical NZB (more PAR2). NZBHydra2 dedupe criteria undocumented. Radarr/NNTmux collision logic not found.
- MinHash/LSH banding thresholds (128 hashes: 32x4 ~0.42, 16x8 ~0.71, 8x16 ~0.88). https://sabrresearch.com/cookbooks/pure-python-minhash-lsh-deduplication
- No published stats on exact-size uniqueness of video files (gap: Atlas could measure this on its own catalog).
Proposed Usenet rule: inner-file (size) multiset coverage, exact sizes, ignore sidecars, >=1 large file exact + CRC when available = alt; size-only = candidate.
