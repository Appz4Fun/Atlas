import sys,numpy as np; sys.path.insert(0,'.')
import sim2_xover as X, sim2_seg as S
from chainsim import build_world, ART
# near-correct run: majority label covers >=90% of run and run holds >=90% of that file's parts
def near(b,m,cmode,gm):
    tot={}
    for g,st in b.items():
        for a in st:
            if a[2]>=0: tot[(a[2],a[3])]=a[5]
    out=[]
    for g,st in b.items():
        secs=np.floor([a[0] for a in st]).astype(int)
        _,first,cnt=np.unique(secs,return_index=True,return_counts=True)
        fi=np.repeat(first,cnt);cc=np.repeat(cnt,cnt);rankt=secs+(np.arange(len(st))-fi+0.5)/cc
        cs=[(n,rankt[n],a) for n,a in enumerate(st) if a[2]>=0 and (cmode=='all716' or m[a[2]][0]=='obf')]
        for s in S.seg_absorb(cs,gm):
            labs={}
            for c in s: labs[(c[2][2],c[2][3])]=labs.get((c[2][2],c[2][3]),0)+1
            lab=max(labs,key=labs.get); k=labs[lab]
            if m[lab[0]][0]=='obf': out.append(k/len(s)>=.9 and k/tot[lab]>=.9)
    return np.mean(out)
for nob in [1,5,20]:
  for cmode in ['obf','all716']:
    print(nob,cmode,' '.join(f'gm{gm}:near={np.mean([near(*build_world(nob,40,50,[4,10,20,50],300+s)[:2],cmode,gm) for s in range(3)]):.2f}' for gm in [6,10,20]),flush=True)
