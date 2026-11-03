# Wave 6 B: simulation round 3 (sim/sim3_all.py modes sparse <K> | dp | sid; outputs sim/*.out)
Worlds: 5 or 20 obfuscated uploads, 40 competing uploads, 50/s background, N in {4,10,20,50}, 2 seeds. Linker: Hungarian + PAR2 size-set, window [-1.2w/rate-0.5,+0.5], |ln rate| <= 0.3. R low everywhere because truth includes background pairs; read P and wholeUp.

1. Sparse header reads (1 per K in arrival order; unread -> nearest read neighbour; short last parts matched by exact size):
| K | reads/article | label acc | obf runs pure | chain P | wholeUp |
| 1 | 1.00 | 1.000 | 1.000 | .984 | .71 |
| 5 | .20 | .612 | .082 | .801 | 0 |
| 10 | .10 | .562 | .084 | .751 | 0 |
| 20 | .05 | .540 | .083 | .553 | 0 |
| 50 | .02 | .518 | .073 | .650 | 0 |
Uploads interleave within each second at N up to 50; nearest-neighbour labelling ~ coin flip.
2. PAR2 part-count DP placement (XOVER only): article acc .457, run exact .075, chain P .720, wholeUp 0. Part counts pin file ends, not membership among concurrent same-size posts.
3. Server-ID session token S:
| share of uploads with colliding S | chain P | wholeUp |
| 0 | .984 | .71 |
| .1 | .987 | .68 |
| .3 | .972 | .40 |
| .5 | .957 | .14 |
A unique S = reading every header, for one XOVER line per article. (Real data: the yEnc Haiku thread saw K/S differ between posts a minute apart, i.e. unique per post so far.)
No cheap recipe reaches >= .9; best is full per-article identity + chain + PAR2 at ~.71. The residual errors are chain order/boundaries.
Caveat: worst case, all competing uploads use the same 716800 article size and timing model.
Next: PAR2 subset-sum over the whole upload instead of pairwise links; per-chain 2-pass rate/N; N-tiling to split a shared S; read headers only in ambiguous overlap windows.
