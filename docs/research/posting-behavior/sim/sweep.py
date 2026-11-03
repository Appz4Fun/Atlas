import sys, random, numpy as np
sys.path.insert(0, '.')
from chainsim import *
rng = random.Random(1)
worlds = [build_world(20, 40, 50, [4,10,20,50], 200+s) for s in range(2)]
F = [[x for x in extract_files(b, m, t, 'all716', 'rank', 30, rng) if x['n']>1] for b,m,t in worlds]
for lo,hi in [(-1.5,0.5),(-3,1),(-4,1.5),(-8,3)]:
    for rt in [0.1,0.2,0.3,0.6]:
        r = [score(f, link(f, lo, hi, rt, 1, 0.5)) for f in F]
        print(f'win=[{lo},{hi}] rtol={rt} P={np.mean([x[0] for x in r]):.3f} R={np.mean([x[1] for x in r]):.3f}')
