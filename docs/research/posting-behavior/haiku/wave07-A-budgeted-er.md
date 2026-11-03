# Wave 07 A: Budgeted entity resolution for fetch scheduling

Web research, about 12 tool calls.

## Opened and read

1. Firmani, Saha, Srivastava. "Online Entity Resolution Using an Oracle." PVLDB 9(5), 2016. https://www.vldb.org/pvldb/vol9/p384-firmani.pdf
2. Papadakis et al. "Supervised Meta-blocking." PVLDB 7(14), 2014. https://www.vldb.org/pvldb/vol7/p1929-papadakis.pdf
3. Altowim, Kalashnikov, Mehrotra. "Progressive Approach to Relational Entity Resolution." PVLDB 7(11), 2014. https://www.vldb.org/pvldb/vol7/p999-altowim.pdf
4. Papenbrock, Heise, Naumann. "Progressive Duplicate Detection." TKDE 2015. https://hpi.de/oldsite/fileadmin/user_upload/fachgebiete/naumann/publications/PDFs/2015_papenbrock_progressive.pdf

## Seen, not opened (metadata only)

- Whang, Marmaros, Garcia-Molina. "Pay-As-You-Go Entity Resolution." TKDE 2013. https://dspace.kaist.ac.kr/handle/10203/240812 (abstract only; no PDF in the record)
- Vesdapunt, Bellare, Dalvi. "Crowdsourcing Algorithms for Entity Resolution." PVLDB 2014. Original PDF not opened. Only a commentary note was read: https://arxiv.org/pdf/1409.7472.pdf (disputes the paper's "order of magnitude" claim).

## Core scheduling ideas and reported numbers

**Firmani et al. (oracle ER).** Treats the oracle (BODY fetch) as a ranking problem; maximise progressive recall (area under recall vs questions). Transitivity: a YES path implies a match; a path with exactly one NO edge implies a non-match. Edge benefit is roughly |cluster(u)| x |cluster(v)| x p(u,v). Strategies: sedge (highest-probability edges in a window, ask largest benefit) and shybrid (nodes in expected-cluster-size order). Reported gain is mainly early recall, not total questions. Average recall loss vs sequential baseline is about 0.002-0.008. No headline percentage saving.

**Altowim et al. (progressive relational ER).** Budget split into fixed-cost windows; each window picks blocks and node pairs by benefit/cost. Benefit = match probability + "impact" (dependent matches unlocked within beta hops). Greedy by benefit per unit cost. Reported: a static block ordering reaches about 0.77% recall at the same budget (probably a typo for 77%; check the figure). Lazy resolution cut time-to-full-recall by about 24% vs Random and about 45% vs All. Planning overhead about 4.8%.

**Papenbrock et al. (progressive duplicate detection).** Orders comparisons by estimated match likelihood. PSNM widens a sorted-neighbourhood window with look-ahead; PB favours high-duplicate-density block pairs. Reported: about 2x progressiveness over SNM. On plista (about 100M records), after 2 days PSNM found 71% of duplicates, PB 93% (SNM needed about 7 days). Partition caching cut runtime 42%.

**Papadakis et al. (supervised meta-blocking).** Builds a blocking graph, trains a classifier on a small undersampled labelled set (5% of minority class sufficed), prunes edges. Reported comparison cuts: about 72% (Dmovies), 93% (Dinfoboxes), 98.5% (DBTC09), with recall loss of 1-4 points.

**Whang et al. (not read).** Pay-as-you-go ER: benefit-ordered incremental resolution under a limited budget. No numbers claimed.

## Mapping to the indexer

- Oracle = BODY fetch (yEnc begin/end/size/total + first bytes). Rank by P(match) x expected links resolved.
- Transitivity = PAR2 manifest is a hub: one index resolves all files in the set. Benefit = N_files x 1 fetch, so it ranks first.
- Blocking = time windows, :lines/:bytes size buckets, subject-stem clustering (Papenbrock sorted neighbourhood over free overview rows).
- Meta-blocking = classifier trained on early resolved pairs scores candidate edges; prune below threshold before any fetch.

## Proposed fetch-scheduling policy

1. Free stage: cluster overview rows by subject stem, :bytes, date window. Score edges with a classifier trained on resolved pairs.
2. Hub stage: when a PAR2 vol0 candidate exists, fetch its index first; rank by files it would resolve.
3. Verify stage: for each unresolved cluster, fetch the first segment of its highest-posterior member. Stop when cluster posterior clears a threshold.
4. Transitivity: after each fetch, skip pairs implied by a YES path or a single NO edge.
5. Budget windows: re-rank the queue every N fetches so benefit uses current clusters.
6. Early stop: once a set's files are all resolved via a PAR2 manifest, drop its remaining candidates.

## Estimate for a 100-volume post (my estimate, not from the papers)

- Naive (first segment of every file, then verify): about 100-200 BODY fetches plus PAR2 fetches.
- Policy: about 1-3 PAR2 index fetches resolve most volumes, then about 5-15 verification fetches for unresolved or ambiguous files. Total roughly 10-25 fetches per release, about 5-10x fewer than naive. Planning guess; measure on the real backlog.

## Limits

- The papers assume records with a fixed schema and ground truth. Usenet overview rows are noisier, so savings may not transfer.
- Figures are from PDFs opened above. Values marked "probably a typo" need checking against originals.
