# W9C link precision evaluation (first pass, 10:18Z)
- Barnes arXiv 1509.04238: pairwise F1 minimum plus a cluster metric (VI, GMD); single-sided purity misleading; gold sets rarely > several thousand.
- arXiv 2404.05622 entity-centric evaluation: sample records, annotators recover true clusters, estimate pairwise/cluster/B-cubed P/R with PPS weights; naive benchmark precision biased up. Binette et al. 2022 bias-adjusted estimator (CI details unverified).
- Offline corpus: Nyuu does not do PAR2 and no confirmed dry-run/article-to-file; ParPar is PAR2 only. Safest: own generator (yencode-style) writing articles to disk + par2cmdline. Perturbations: re-pack, rename, other article size, one-byte change.
- Chain guards (Splink cluster metrics): size cap, low density (chains), is_bridge edges audited first; require direct key match per merge.
- No published false-merge rates found for SWH or Borg. Birthday: n^2/2^(b+1); CRC32 at 1e6 files ~116 chance pairs, so CRC32 = blocking key only.

## Second pass (10:22Z)
- Nyuu help-full.txt: no dry-run, no write-all-articles mode; only --dump-failed-posts. -o writes the NZB. -a/--article-size default 700K; --article-line-size default 128. Offline corpus needs our own yEnc writer (or a fake NNTP sink).
- ParPar 0.4.5: -s/--input-slices sets slice size or count; -o base name (path prefix likely works, untested).
- arXiv 2404.05622 s3.4.2: ratio estimator theta = E[f(c)]/E[g(c)] over clusters sampled with replacement, Binette 2022 bias adjustment + variance; intervals shown as +/-2 SD. For a random sample of pairs, Wilson interval.
