# w9a: PAR2 defaults and GF constants, source-level check

Scope: source-level check of PAR2 block-size selection, default redundancy, first recovery exponent, and GF(2^16) polynomial for par2cmdline, par2cmdline-turbo, ParPar and MultiPar/par2j. QuickPar not checked. Sources fetched with curl on 2026-11-02; WebFetch not used for code.

## Table

| Tool | Block-size rule (quoted) | Default redundancy | First exponent | GF / bases standard? | Source URL + line |
|---|---|---|---|---|---|
| par2cmdline master, default (no -b/-s) | `blocksize = (totalsize + 1999) / 2000;` then `blocksize = std::max(blocksize, (u64)4);` then `blocksize = (blocksize + 3) & ~3;`. `totalsize` = sum of sizes of ALL input files. `blockcount` stays 0 on this path, so "-b 2000" is help text only. Min clamp `(totalsize+32767)/32768`, rounded to multiple of 4. | `redundancy = 5;` when neither -r nor -c set (5%) | `firstblock(0)` (L213); `exponent = firstrecoveryblock` (par2creator.cpp L666). Option at L708. | GF(2^16), generator 0x1100B: `typedef Galois<16,0x1100B,u16> Galois16;` (galois.h L339). `pow(0)` returns 1 (galois.h L245). Bases: `while (gcd(G::Limit, logbase) != 1) logbase++;` then `database[index] = base` (reedsolomon.cpp L232-259). | https://raw.githubusercontent.com/Parchive/par2cmdline/master/src/commandline.cpp L1192-1207, L1216-1219; galois.h L339; reedsolomon.cpp L232-259 (same path in src/) |
| par2cmdline master, `-b N` | `size` found by binary search so that `sum ceil(ceil(filesize/4)/size)` is about N. If N == number of files: `*blocksize = (largestfilesize + 3) & ~3;`. Result: `*blocksize = size*4;`. Error if N < number of files. | as above | as above | as above | libpar2.cpp L1314-1414 (L1343 N==files case; L1409 `*blocksize = size*4;`); call site commandline.cpp L1240 |
| par2cmdline-turbo, default | Identical formula: `blocksize = (totalsize + 1999) / 2000;` / `max(...,4)` / `(blocksize+3) & ~3`, with the same min clamp. | 5% (same comment block) | `firstblock(0)` (not separately checked) | Same `0x1100B` typedef (galois.h L336); same logbase/gcd loop (reedsolomon.cpp L194-215). | https://raw.githubusercontent.com/animetosho/par2cmdline-turbo/master/src/commandline.cpp L1224-1241; galois.h L336; reedsolomon.cpp L194-215 |
| par2cmdline-turbo, `-b N` | ComputeBlockSizeFromCount NOT found in turbo's .cpp files. | - | - | - | UNVERIFIED (not located; not byte-compared) |
| ParPar 0.4.5 | `-s` REQUIRED. Help: "-s2000 ... selects a slice size so that there are 2000 input slices"; "par2cmdline's default equivalent: `-s2000`"; "Multipar/par2j's default equivalent: `-s0.5w*10`". Sizes are given as a count, `-sN`, or bytes with suffix. Slice size checked to be even (`sliceSize & 1`) in src/gf.cc L265. | `-r` default 0 (no recovery) | `-e/--recovery-offset` default 0 (help-full.txt L108) | 0x1100B: `if(n > 65535) n ^= 0x1100B;` (gf16/gfmat_coeff.c L27, L42, L75); `#if GF16_POLYNOMIAL != 0x1100b` (gf16/controller_ocl_init.cpp L545). Bases: exponent loop skips exps divisible by 3, 5, 17 or 257 (gfmat_coeff.c L19-30). 3*5*17*257 = 65535, so this is the coprime-to-65535 rule. | https://registry.npmjs.org/@animetosho/parpar/-/parpar-0.4.5.tgz: package/help.txt L11-31, package/help-full.txt L108, package/gf16/gfmat_coeff.c L19-30, L27, package/gf16/controller_ocl_init.cpp L545 |
| ParPar `-s2000` vs par2cmdline default | Not byte-identical by construction: par2cmdline uses ceil(total/2000) with 4-byte rounding. ParPar's count-to-size algorithm was not read. | - | - | - | UNVERIFIED (equivalence claim is from ParPar help text only) |
| MultiPar / par2j | Source NOT reached: raw.githubusercontent.com paths tried (par2j/*, source/par2j/*, MultiPar/par2j/*) returned 404; github.com returned 403 via proxy; README has no block-size info. Only the ParPar help line "-s0.5w*10" is available. | UNVERIFIED | UNVERIFIED | UNVERIFIED | https://raw.githubusercontent.com/Yutaka-Sawada/MultiPar/master/README.md (no detail) |
| QuickPar | NOT CHECKED (archive.org not fetched in this run). | UNVERIFIED | UNVERIFIED | UNVERIFIED | - |

## Proves / Cost

- **Proves (weak alone):** exp-0 recovery = XOR of all input slices (pow(0)=1, galois.h L245). Equal exp-0 slices imply equal slice size and equal slice boundaries and padding, and that the XOR of the slices matches. A single XOR is not collision-resistant, so this is a candidate filter, not proof. Confirm with per-file MD5 / 16k hash from the PAR2 file description packets, or with exp-1 or exp-2 slices.
- **Independent of base choice:** exp-0 needs no particular GF bases, so base-ordering details do not affect the exp-0 match. Only the slice size and the byte content matter.
- **Hidden dependency:** the default par2cmdline block size depends on the TOTAL bytes of every input file in the set (commandline.cpp L1192-1199). Adding or removing one file (sample, NFO, extra volume) changes the block size and breaks the exp-0 match even when the payload bytes are identical.
- **Cost:** one recovery volume article (the exp-0 slice is in the first recovery file; which file and article is UNVERIFIED) plus the PAR2 header/packets from the same set. Roughly 1 to a few articles, dominated by the slice size.

## Tool pairs that produce matching exp-0 by default

- **par2cmdline master and par2cmdline-turbo:** default block-size formula is identical in the sources read, so same input bytes yields the same block size and the same exp-0 slice. Byte-identical output of the full recovery set was NOT tested.
- **ParPar with `-s <exact bytes>` set to par2cmdline's computed block size:** exp-0 matches (same field-independent XOR). ParPar with `-s2000` is NOT guaranteed to match par2cmdline's default block size.
- **MultiPar/par2j:** UNVERIFIED. The ParPar help says its default is `-s0.5w*10`, not the par2cmdline formula, so a default-vs-default match is NOT expected without a check.
- **QuickPar:** UNVERIFIED.

## Takeaways

1. The default par2cmdline block size is a function of total set bytes, not per-file bytes, so set composition (extra files) decides whether exp-0 matches.
2. Exp-0 is an XOR over slices and does not depend on GF bases, polynomial, or first exponent (pow(0)=1), so it matches across tools whenever the slice size and slice contents match.
3. 0x1100B is confirmed in par2cmdline, par2cmdline-turbo and ParPar sources; the coprime-to-65535 base rule is in par2cmdline (reedsolomon.cpp L211-215) and ParPar (gfmat_coeff.c L19-30).
4. ParPar requires an explicit `-s` and its `-s2000` is documented as equivalent to par2cmdline default, but this is not verified in code; treat as UNVERIFIED.
5. MultiPar/par2j and QuickPar defaults remain UNVERIFIED; they need the repo source (not reachable from this session) or archived docs.
