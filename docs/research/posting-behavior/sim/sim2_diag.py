import sys,numpy as np; sys.path.insert(0,'.')
from chainsim import build_world, ART
for N in [4,10,20,50]:
    b,m,t=build_world(5,0,50,[N],300)
    last=[];strag=[]
    for g,st in b.items():
        d={}
        for n,a in enumerate(st):
            if a[2]>=0: d.setdefault((a[2],a[3]),[]).append(a)
        for k,L in d.items():
            pos=[i for i,a in enumerate(L) if a[6]<ART][0]; last.append(pos==len(L)-1); strag.append(len(L)-1-pos)
    print(f'N={N} short-is-last-arrival={np.mean(last):.2f} stragglers after short: median={np.median(strag):.0f} p95={np.percentile(strag,95):.0f}')
