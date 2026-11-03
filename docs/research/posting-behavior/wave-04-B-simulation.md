# Wave 4 B: simulation of the end-to-start join (scripts in research/posting-behavior/sim/: chainsim.py, sweep.py, sweep2.py; python3 -I, ~1 min each)

Result: end-to-start chaining links ~97.5% of file pairs correctly (precision) and finds ~94% (recall) in the hardest setup, IF arrival is ordered by within-second rank and the window is sized from reorder width. Plain 1-s Date costs ~15 recall points. Linear interpolation from high-water-mark samples FAILS (errors -12..+10 s; bursts make rate uneven).

Setup: files in series; N conns from one queue; per article RTT + size*8/(bw/N) with +-25% lognormal jitter; 716800 B parts; files 50-300 parts; uploads 8-25 files; random 1 of 4 groups per file; N in {4,10,20,50}; 50-500 Mbit/s. Background Poisson 5 or 50/s per group plus (hard mode) 40 competing single-group uploads. Indexer sees article number + Date (1 s) + HWM every 30 s. ASSUMES file runs already known from yEnc part headers (not tested without).

Gap end(k) -> start(k+1): N=4 median +0.06 s (-0.03..+0.14); N=20 -0.11 s (-0.54..+0.17); N=50 -0.8 s (-2.7..-0.3).
Reorder width ~0.75 N: N=4 -> 2; 10 -> 7 (5-10); 20 -> 15 (10-23); 50 -> 39 (27-48). Separates small vs large N; widths within 50% lifts precision 3-9 pts.

20 obfuscated + 40 competing uploads, 3 seeds:
| time source | fixed [-3,+1] s P/R | adaptive P/R | uploads fully right |
| true arrival | .942/.903 | .974/.944 | 0.66 |
| Date 1 s | .878/.752 | .911/.768 | 0.19 |
| within-second rank | .932/.891 | .975/.942 | 0.68 |
Adaptive window [-1.2*width/rate - 0.5 s, +0.5 s]. Within-second rank = order by article number inside each Date second and spread evenly. 5 uploads: .986/.956. Fixed [-1.5,+0.5] s with rate tol 0.3: .975/.937; [-8,+3]: .917/.887.

Failure: only 66-75% of uploads have every link right (one bad link of 8-25 breaks it). Rate is the weakest feature.
Thresholds: Date + within-second rank; window as above (or fixed [-1.5,+0.5] s without part headers); |ln rate ratio| <= 0.25; widths within 50% (floor 4); one-to-one greedy; confirm chains before publishing.
Next: confirm chains by cumulative yEnc size vs PAR2/NZB totals; file runs without yEnc headers (short last part in XOVER bytes + contiguity); global assignment (Hungarian/Viterbi) with per-upload rate/N model.
