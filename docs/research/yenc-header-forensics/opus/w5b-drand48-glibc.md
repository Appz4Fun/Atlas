# Wave 5B: NewsUP drand48 inverter + ngPost glibc brute force (own re-implementations; third-party code not run)
NewsUP:
- own-style ID `"$sec$usec." . b36(rand("$sec$usec")) . "@" . b36("$sec$usec") . ".newsup"`, b36 LSD-first (verified), usec not zero-padded (N 11-16 digits). generate_random_ids draws int(rand(@gens)) before each ID even non-obfuscated -> 2 draws per own-style ID. IDs returned in hash order.
- Inverter (newsup_sim.py): enumerate X with int(u*N)==r. Full 16-digit N: unique state 37/40 (mean 1.07 candidates); linked all 29 siblings 40/40 with ±200-draw window; 0 decoy false links; random From regenerated 40/40; 0.1 s. Short usec (11-digit N): 16,270 candidates, one sibling resolves.
- seed_backtrack.py: step back until (X&0xFFFF)==0x330E finds Perl srand seed + call count (demo exact, false hits ~1/65k steps).
- Caveat: Perl>=5.20 drand48 assumed, not run on real perl; keys %alpha_chars order random per process (24 orders to try).
ngPost:
- randomStdFrom NgPost.h L599: 13 x AL[rand()%62], AL=A-Za-z0-9, + "@ngPost.com"; srand(QUuid data1) NgPost.cpp L315; old ms seed commented L314.
- ngpost_rand.c glibc TYPE_3 reimplementation matches system rand for 7 seeds x 100k outputs.
- glibc seed-1 Froms: pkDHTxmMR18N2, l9k88EmLgN7cC, CTt9rWksb1fEB, w397vi5Ug1YHC, 3UAVUAoB9VYjC.
- MSVC seed-1 Froms confirmed: p1KaLmIg2kB7f, Zp5Tm1qbi6dss, 92C31aPGJK9Lv, k7DMPDONXrwD2, yVLZrHBio74so.
- Brute force: index 0: 2^20 seeds 1.54 s/core -> 2^32 ~1.75 h/core, ~200 s on 32 cores. Within first 1000 Froms: ~59 core-h, ~1.9 h on 32 cores (hit at #699 in demo).
- Pre-v4.15 ms seed: ±1 day = 2^27.4 candidates, true seed found uniquely in 48.8 s on 4 threads (~6 s on 32 cores); realistic window [Date - run length, Date] is far smaller.
Files: rng-tools/ (newsup_sim.py, seed_backtrack.py, ngpost_rand.c, msec_window.c).
