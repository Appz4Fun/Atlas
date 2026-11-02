# Wave 5 A: simulation round 2 (scripts sim/sim2_xover.py, sim2_seg.py, sim2_near.py, sim2_diag.py)

Same setup as wave 4 (4 groups, 40 competing uploads, 50/s background, N in {4,10,20,50}, 3 seeds). Width now a crude proxy max(4, 0.15n). wholeUp is strict.

1. XOVER-only file runs BREAK the method. The short last article arrives EARLY (it uploads faster): it is the last arrival only 44% of the time at N=4, 8% at N=10, 0% at N=50; ~N/2 full-size stragglers follow it (p95 ~0.9N). Concurrent uploads in one group interleave with nothing in XOVER to separate them.
| obf / candidates | run exact (A) | run exact (B g=10) | run >=90% ok (B g=20) | chain P/R (B, Hungarian) | wholeUp |
| 1 / hex | - | .65 | .84 | .84/.83 | 0 |
| 5 / hex | .35 | .57 | .65 | .73/.82 | 0 |
| 20 / hex | .07 | .29 | .35 | .38/.49 | 0 |
| 5 / all-716800 | .03 | .26 | .58 | .29/.74 | 0 |
| 20 / all-716800 | .02 | .25 | .44 | .32/.53 | 0 |

2-3. With header-based runs: PAR2 size-set constraint (link only runs whose byte totals match file sizes in the same PAR2 set, 0.1%) and Hungarian:
| scenario | greedy P/R/whole | Hungarian | greedy+PAR2 | Hungarian+PAR2 |
| 5 obf hex | .948/.895/.33 | .957/.921/.33 | .980/.940/.67 | .979/.936/.67 |
| 20 obf hex | .926/.876/.35 | .928/.890/.35 | .993/.949/.78 | .994/.945/.77 |
| 5 obf all716 | .925/.889/.07 | .950/.928/.20 | .973/(.174)/.53 | .976/(.174)/.60 |
| 20 obf all716 | .889/.850/.15 | .914/.889/.17 | .970/(.403)/.67 | .974/(.402)/.65 |
(bracketed recall artificially low: competing uploads have no PAR2.)
PAR2 constraint roughly doubles whole-upload correctness; Hungarian adds 0.01-0.04.

Recommended: (1) file runs from yEnc =ybegin part/total/size (HEAD/BODY first lines per candidate), not XOVER; (2) fetch each upload's PAR2 index -> file-size sets; (3) match in window [-1.2 w/rate - 0.5 s, +0.5 s], |ln rate ratio| <= 0.3, only within one PAR2 size set; (4) Hungarian with fixed no-successor cost; (5) accept a chain only when its size multiset equals the PAR2 set.
Next: read one yEnc header per time cluster instead of all; use PAR2 part-count sequence (ceil(size/716800)) to place runs by dynamic programming.
