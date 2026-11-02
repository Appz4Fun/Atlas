# From a recovered drand48 state, walk back to Perl's srand origin (X0 = seed<<16 | 0x330E): gives seed + #rand calls since start.
import sys; sys.path.insert(0, '.')
from newsup_sim import *
r = Drand48(0xC0FFEE42); uploader(r); clk=lambda:(1730000000,123456)
ids = gen_ids(r, 500, False, clk); c,_,_,_ = candidates_from_id(ids[-1])
for x in c:
    y = x; hits = []
    for k in range(1, 200000):
        y = back(y)
        if (y & 0xFFFF) == 0x330E: hits.append((k, hex(y >> 16)))
    print("cand", hex(x), "origin hits (steps_back, seed):", hits[:3])
