# Wave 7 A: simulation round 4 (sim/sim4_all.py, sim4_p1.py, sim4_p23.py, sim4_combo.py)
4 worlds (obf 5/20 x 2 seeds, 40 background uploads, 4 groups).

1. Whole-upload PAR2 matching with exact per-file identity:
- subsetSum: match runs to PAR2 sizes in a window, order by start time (no pairwise linking).
- sizeChain: anchor on unique sizes, grow back/forward only with sizes still left in that upload's PAR2 list, cost = gap x rate + |log rate ratio|, cheapest step first globally.
"K unique": all but last K files are a standard volume size from a pool of 2, also used by half the background.
| obf | unique sizes | subsetSum | sizeChain | old pairwise |
| 5 | all | 1.00 | 1.00 | .70 |
| 5 | K=3 | .20 | .80 | .80 |
| 5 | K=1 | .10 | .70 | .70 |
| 20 | all | 1.00 | 1.00 | .72 |
| 20 | K=3 | .08 | .78 | .62 |
| 20 | K=1 | .00 | .65 | .48 |
Distinct sizes -> solved (1.00). Equal RAR volume sizes break subset-sum; sizeChain recovers most.

2. Counter N for shared session token (no headers), modelled as perfect arrival rank (optimistic):
| share | counterSplit + pairwise | counterSplit + sizeChain |
| .1 | .58 | .81 |
| .3 | .50 | .77 |
| .5 | .34 | .40 |

3. Targeted header reads in ambiguous seconds: ~83% of articles are ambiguous at this traffic, reads .825-.866/article, wholeUp .24-.34 (.43 with sizeChain). Dead end.

Best recipe: exact file identity + sizeChain (or subset-sum); cost one PAR2 fetch per upload + identity per article. Distinct sizes -> 1.00; volume uploads -> .65-.80. No headers but session token shared <=30% -> .77-.81.
Next: break equal-volume ties with PAR2 16k MD5 by fetching the first article of each candidate run (1 body per file, <0.5% reads) -> expected ~1.0; realistic per-process counters with clock skew.
