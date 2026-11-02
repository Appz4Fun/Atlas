# Posting-behaviour linking on real NZB data

## Data caveats
- `segs.tsv` has 5 columns: nzb, file date, Message-ID, ID domain, poster. It has **no subject, segment number, bytes or groups**.
  - "File" means a (nzb, date, poster) group. Row-run grouping gives the same results.
  - "Article order" means row order.
  - **Equal article size could not be tested.** A segment-count proxy was used instead.
- There are 246 NZBs, not 261, and only 202 distinct Message-ID sets. The other 44 are copies.
- Families: nyuu-style 110 (includes @PRiVATE), Highwinds 7, ngPost 2, other 120 (many are fixtures).
- Decoding checks out. For nyuu, floor(first ms) equals the NZB date in 1,137 of 1,183 files.

## 1. End-to-start join
| family / time source | gaps | median s | p90 s | frac < 2 s | frac overlap |
|---|---|---|---|---|---|
| nyuu, decoded ms | 1070 | 0.118 | 0.670 | 0.980 | 0.000 |
| nyuu, deduplicated, row runs | 1033 | 0.123 | 0.707 | 0.978 | 0.005 |
| Highwinds, decoded s | 12 | 1 | 2 | 0.75 | 0 |
| ngPost, date | 24 | 6 | 10 | 0.17 | 0 |
| other, date | 380 | 1 | 53 | 0.63 | 0 (5.5 % exactly 0) |

- **nyuu, 95 % CI bootstrapped over 92 NZBs:** median 0.07–0.21 s, p90 0.50–1.11 s, frac < 2 s 0.95–1.00.
- **The gap between files is about 7× the gap between segments inside a file** (median 0.018 s).
- **Files tile in series:** decoded ms give no negative gaps with the group definition, and 0.5 % with row runs.
- **NZB listing order matches time order in only 50 of 98 NZBs.**
- **"Other" is a mix of about 20 ID domains, mostly 1 NZB each.**
  - 20 of its 21 zero-second gaps come from a single NZB (`@2PCOnsLH.Ldy`, 0.37 of its gaps), so the parallel-file evidence there is weak.
- **NZB dates alone are too coarse:** nyuu date-only gaps have a median of 2 s, with 38 % under 2 s.

## 2. Re-linking files across NZBs

### Real overlap
**There is no genuine overlap.**
- nyuu has 11 overlapping NZB pairs. All are copies: after deduplication, 3 of the 4 pairs share every Message-ID except one, and the 4th has identical ms lists with only the ID domain rewritten.
- Highwinds and ngPost have 0.
- "Other" has 480 pairs (175 after deduplication). They are fixture dates such as 1690000000 or 1760313600, single-file NZBs, or copies that share 351–993 IDs (`t2h.py`).

### Simulated overlap
- **Method:**
  - K deduplicated real NZBs were time-shifted to start within a window of W s.
  - Labels were hidden.
  - Each file end was greedily matched one-to-one to the nearest start within maxgap (2 s unless noted).
  - Precision and recall are pairwise, over 100 trials per cell. Seed-to-seed SD is about 0.03.

| family | K | W s | precision | recall | chance precision |
|---|---|---|---|---|---|
| nyuu | 2 | 0 / 60 | 0.93 / 0.93 | 0.87 / 0.89 | 0.56 |
| nyuu | 5 | 0 / 60 | 0.73 / 0.75 | 0.67 / 0.74 | 0.29 |
| nyuu | 10 | 0 / 60 | 0.60 / 0.58 | 0.56 / 0.59 | 0.19 |
| nyuu | 20 | 0 / 60 | 0.47 / 0.45 | 0.41 / 0.45 | 0.11 |
| nyuu | 5 | 600 / 3600 | 0.93 / 0.99 | 0.89 / 0.95 | – |
| nyuu | 10 | 600 / 3600 | 0.84 / 0.98 | 0.78 / 0.91 | – |
| other (s dates) | 5 | 0 / 60 | 0.39 / 0.54 | 0.67 / 0.60 | 0.34 |
| other (s dates) | 20 | 0 / 60 | 0.11 / 0.12 | 0.72 / 0.73 | 0.10 |

- For nyuu at K = 10 and W = 0, the true successor is the nearest start in 82 % of cases (99 % at W = 3600).
- Pairwise scores are lower than that, because one wrong link merges two chains.

**Size proxy.** Consecutive files share a segment count in 590 of 939 cases. Adding a 1 s penalty when counts differ gives:

| K (W = 0) | P / R, gap only | P / R, gap + segment-count |
|---|---|---|
| 5 | 0.74 / 0.70 | 0.88 / 0.82 |
| 10 | 0.61 / 0.56 | 0.80 / 0.73 |
| 20 | 0.47 / 0.43 | 0.72 / 0.66 |

Sweeping the penalty at K = 10 (P / R): 0 gives 0.62 / 0.57, 0.25 s gives 0.76 / 0.71, and equal counts only gives 0.82 / 0.71.

**Link-level next-hop precision / recall** (nyuu, `t2f.py`) is much higher than the pairwise scores:

| K | W s | gap only | + segment-count |
|---|---|---|---|
| 5 | 0 | 0.90 / 0.88 | 0.92 / 0.89 |
| 10 | 0 | 0.82 / 0.79 | 0.89 / 0.86 |
| 20 | 0 | 0.70 / 0.68 | 0.82 / 0.80 |
| 10 | 600 | 0.96 / 0.95 | 0.96 / 0.93 |

**Reading:**
- The ms join works when there are 5 or fewer uploads per 10 minutes.
- It degrades at 10 or more simultaneous uploads.
- A size-like feature adds 0.15–0.25 precision.
- With second-resolution dates, precision is about chance.

## 3. Highwinds (7 NZBs, 869 articles)

| NZB | articles | fx | (fx, S) values | N steps | N rises |
|---|---|---|---|---|---|
| c857a4ff7e | 655 | fx33.iad | 1 | +1: 447, +2: 207 | 654/654 |
| 3fedd762a4 | 185 | fx30.am4 | 1 | +1: 184 | 184/184 |
| f5fb8db749 | 11 | fx31.am4 | 1 | +1: 8, +4, −3 | 9/10 |
| 939b1d73d5 | 11 | fx45.am4 | 1 | +1: 10 | 10/10 |
| c7c1206442 | 4 | fx09.am4 | 1 | +2: 3 | 3/3 |
| a81258bd56 | 2 | fx21.am4 | 1 | +1 | 1/1 |
| 49e9826bc0 | 1 | fx02.iad | 1 | none | none |

- **S is constant per NZB**, including across 13 file dates in one NZB.
- **N rises with article order** in 861 of 862 steps, and T never decreases with N.
- **N counts per front end, not per session:** the +2 and +4 steps are other traffic interleaved.
  - In c857a4ff7e this poster had 76 % of N (655 of 862).
  - In 3fedd762a4 the poster had 100 % of N.
- **R is unordered.**
- **The NZB date can differ from T by up to 865 s.**
- **The sample is tiny:** only 2 NZBs have more than 11 articles.

## Bottom line
- nyuu files post strictly in series, with a median gap of 0.12 s.
- The join plus a size feature links files well at moderate concurrency.
- The data contains no real concurrent uploads.
- Highwinds S is a reliable session key.

Scripts and outputs are in this directory: `common.py`, `t1*.py`, `t2*.py`, `t3.py`, `out_*.txt`.
