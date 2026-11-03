# W09-A: validation of w08-c Message-ID spec

Method: `w9a/validate.py` (python3 -I) transcribes spec section 2 verbatim, runs all rules, first match, overlaps, old-classifier diff, time checks. Corpus `segs.tsv` 72,334 rows, 246 NZBs. Per-MID output: `scratchpad/w9a/per_mid.tsv`.

## Concrete fixes

1. Section 2, rule 10 JBinUp: replace `@JBinUp\.local$` with `^[A-Za-z0-9]{19,20}_[0-9]+o[0-9]+@JBinUp\.local$`. Extract N = `_([0-9]+)o`, M = `o([0-9]+)@`. The current regex is too loose (any host ending JBinUp.local). Corpus: 0 hits, unverified.
2. Section 2, new rule 10b JBinDown (add `JBinDown` to ToolLabel, after rule 10): `^[^@]+\.[0-9]+-[0-9]+@JBinDown\.local$`. Extract N, M. The prefix shape is not given, so `[^@]+` is a placeholder. Corpus: 0 hits. Today these MIDs fall to Unknown.
3. Section 2, rule 22 UsenetFarmLoose: replace `^[A-Za-z]{32}@` with `^[A-Za-z]{32}@(?:UFK|news\.usenet\.farm)$`, so it adds nothing beyond rule 15. Reason: the current regex accepts any host. Its 116 hits all sit in NZB c09c711e35, whose other 2,022 rows are Unknown with random dotted hosts. Those 116 move to Unknown (26).
4. Section 3, add flag `nzb_date_bad` when nzb_date is after now. NZB f1e63da9b2 has nzb_date 1999903067 (about 2033) while its Nyuu ms values are about 1763803067xxx (2025-11). All 163 out-of-window Nyuu rows come from this NZB. Do not relabel.
5. Section 3, record the expected overlap counts: 1+2 = 23,831; 1+4 = 23,831; 2+4 = 23,835; 3+4 = 11,628; 5+21 = 3,689; 15+22 = 2,775; 13+14 = 211. Winners: 1, 1, 1, 3, 5, 15, 13.

## Results

- Rules vs old classify.py: 0 label changes across 72,334 MIDs and 21 labels. The spec table matches the old regexes. Only textual difference is `\d` vs `[0-9]`, which has no effect here.
- Overlaps: 42,138 MIDs hit 2+ real rules, all in the 7 pairs above. No other overlaps.
- Nyuu ms vs nzb date: 37,126 / 37,289 (99.56%) inside +-1 day; the same count is inside +-300 s. 163 outside, all in f1e63da9b2 (fix 4).
- Highwinds T1: 869 / 869 inside +-1 day and +-3600 s. Skew -865 to +47 s, median -532 s. Matches spec section 3.
- JBin: 0 MIDs of any JBin form. The JBinUp and JBinDown shapes cannot be checked against this corpus.
- Fixture rows: 654 with my definition (MID or host), spec says about 442 (host only). Use the spec's host-only definition.

## False-positive review (17 of 246 NZBs have mixed first-match labels)

- Same tool, case-pattern split: 0de43b5ede (Nyuu 1,654 / NyuuLoose 1), bedb1ff948 (1,653 / 1), 16f4108556 (554 / 1), 1092c75064 (89 / 1). Not a false positive.
- Stray row in a single-tool NZB: 4691ae9020, 5c6f640c6b, 3b7b8fa623, 8c19f46e6d, 6fbdebff00 (Nyuu + 1 Unknown); 5d1aee28f6 (Nyuu + 1 NoAtSign, MID with no @).
- Fixture: 931f5b2fbb (SyntheticA1 12 + Unknown 1), 0d617b96d8 (Unknown + NoAtSign, 2 rows).
- Rule false positive, fix 3: c09c711e35 (Unknown 2,022 + UsenetFarmLoose 116).
- Real two-tool NZBs, shapes correct: f2dee72b8c (Diablo 671 + NewsMangler 53), de7cbcbe83 (Diablo 321 + NewsMangler 31), 16e4f358e2 (PowerPost 9 + Diablo 1, eweka host).
- Unresolved, needs sampling: cf32b14460 (Unknown 2,267 + AlnumRandHost 1,037). Unknown rows have dotted random hosts, AlnumRandHost rows have single-label hosts. May be one tool with two host forms.
