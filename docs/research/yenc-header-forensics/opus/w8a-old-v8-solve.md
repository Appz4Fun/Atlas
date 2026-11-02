# Wave 8A: solving old-V8 (s0+s1) Nyuu Message-IDs without z3

**Result: not solved.** CryptoMiniSat does better than z3 and the encoding is shown to be correct, but the full 128-bit search does not finish within 120-200 s, even when the cache offset is known. No real 2017-2022 NZB was attempted, because the synthetic test at the known offset already failed.

## Code
- `nyuu-id-solver/nyuuid_solve_old2.py`: the new solver (pycryptosat 5.17.0 from PyPI; no other third-party code). It has the same CLI as the old solver, plus env vars `NIDS` (IDs per attempt, spaced by the gap), `TMO`, `MTOP`, `EXTRA`. `--synth N gap seed` writes a synthetic old-V8 Nyuu NZB and prints the true offset to stderr. If a solve succeeds, it also links all IDs in the NZB (the same linking step as the old solver).
- `nyuu-id-solver/w8a_partialfix_test.py`: a test that checks the encoding (by fixing known state bits) and measures how fast it solves.

## Encoding
- The 128 initial state bits are the CMS variables. Every state bit at generation g is a GF(2) mask over them. Each needed bit is defined with a native CMS XOR clause, so CMS's Gauss-Jordan handles the linear part.
- The leak for one draw is k = floor(((A_{g-1}+A_g) mod 2^52)*26/2^52). Only bits 51..L of the adder are bit-blasted (XOR clause for each sum bit and 6 clauses for each majority carry). The carry into bit L is a free variable, so the model loses no information.
- The leaked value k is expressed by blocking every top-MTOP-bit pattern that cannot be under k's interval (default MTOP=8, window 14 bits). Draws map to generations LIFO within each 64-draw cache block: gen = 64*floor(i/64) + 63 - (i mod 64).

## Timings (4-core container, 1 CMS thread per job, true offset given)
| Setup | Result |
|---|---|
| 2 IDs (48 draws), MTOP 8 | no answer after 120 s |
| 4 IDs (96 draws), MTOP 8 | no answer after ~195 s |
| 8 IDs, MTOP 8 | no answer after ~195 s |
| 4 IDs, MTOP 5 / EXTRA 3 | no answer after ~195 s |
| 12 IDs, MTOP 6 / EXTRA 4 | no answer after ~195 s |
| 4 IDs, all 128 true bits fixed | sat, correct, 0.1 s (encoding is correct) |
| 4 IDs, 96 / 80 / 64 / 48 random true bits fixed | sat, correct state, 21 / 34 / 36 / 36 s |
| 4 IDs, 40 / 32 / 24 random true bits fixed | no answer after 100 s |

So CMS solves the problem once about 48 of the 128 state bits are known, and the difficulty jumps sharply below that. Guessing 48 bits blindly is infeasible. Adding more draws (8-12 IDs) did not help within 200 s.

## Interpretation
Each leak passes through a carry chain whose input is unknown. The SAT solver has to settle about 80 bits before propagation through the XOR system takes over. That is the same barrier z3 hit. The carry modelling is not the cause, because the encoding is exact.

## Next steps (ranked)
1. **Guess structured bits instead of random ones.** Guess the top bits of A_0 and A_1 (A_{g} for g>=2 then follow linearly over GF(2)), for example 2x12 bits, and give CMS the guessed bits as assumptions (`solve(assumptions)`). Measure whether about 24 structured bits do the work of 48 random ones. If yes, run 2^24 x 64 offsets, which is cluster-scale but feasible. Do not add these as unit clauses.
2. **Statistical / linear approach.** For a top-bit window the carry into bit 51 is biased: Pr[c51 = a50 AND b50 OR ...] is about 0.75 with depth 1 and about 0.9 with depth 3. Add terms until the leak bits become noisy linear equations in the 128 bits, then decode with a BKW / ISD-style LPN decoder over hundreds of draws. Long NZBs (1000+ IDs, i.e. 24k+ draws) give plenty of samples. Because the gap between IDs is uncertain (24/48/72), use contiguous draws from inside one ID (24 draws), or within a block where the offset is fixed.
3. **Try other solvers.** Run kissat or cadical on a DIMACS export, with the XORs converted to CNF, and use CMS with several threads and longer limits (hours, on the correct offset) to check whether it is feasible at all.
4. **Check the premise first.** Confirm the Node version (V8 <= 7.0) of the Nyuu binaries that sanderjo used in 2017-2022. If those posts came from a different generator, this work does not apply.

## Housekeeping
An earlier `pip download pycryptosat` dropped a wheel into nyuu-id-solver/. It was deleted right away. The package was then installed with pip into the container.
