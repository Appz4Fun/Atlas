import sys,numpy as np; sys.path.insert(0,'.')
import sim2_xover as X
from chainsim import build_world, ART
def seg_absorb(cands,gmul):
    segs=[];cur=[];closed=False
    for c in cands:
        if cur:
            ia=(cur[-1][1]-cur[0][1])/max(len(cur)-1,1)
            gap=c[1]-cur[-1][1]
            if (closed and gap>gmul*max(ia,0.02)) or gap>2.0:
                segs.append(cur);cur=[];closed=False
        cur.append(c)
        if c[2][6]<ART: closed=True
    if cur: segs.append(cur)
    return segs
orig=X.runs_from
def runs_from2(bygroup,meta,mode,use_hdr,gmul=4):
    if use_hdr: return orig(bygroup,meta,mode,1)
    import types
    runs=[]
    for g,st in bygroup.items():
        secs=np.floor([a[0] for a in st]).astype(int)
        _,first,cnt=np.unique(secs,return_index=True,return_counts=True)
        fi=np.repeat(first,cnt);cc=np.repeat(cnt,cnt);rankt=secs+(np.arange(len(st))-fi+0.5)/cc
        cs=[(n,rankt[n],a) for n,a in enumerate(st) if a[2]>=0 and (mode=='all716' or meta[a[2]][0]=='obf')]
        for s in seg_absorb(cs,gmul):
            ts=np.array([c[1] for c in s]);labs={}
            for c in s: labs[(c[2][2],c[2][3])]=labs.get((c[2][2],c[2][3]),0)+1
            lab=max(labs,key=labs.get)
            runs.append(dict(g=g,s=ts.min(),e=ts.max(),rate=(len(s)-1)/max(ts.max()-ts.min(),1e-3),n=len(s),bytes=sum(c[2][6] for c in s),lab=lab,pure=(len(labs)==1 and len(s)==s[0][2][5]),uid=lab[0],f=lab[1]))
    return [r for r in runs if r['n']>1]
for nob in [1,5,20]:
  for cmode in ['obf','all716']:
    for gm in [3,6,10]:
        acc=[];acc1=[];res=[]
        for s in range(3):
            b,m,t=build_world(nob,40,50,[4,10,20,50],300+s)
            R=runs_from2(b,m,cmode,0,gm)
            acc.append(np.mean([r['pure'] for r in R]))
            nf={}
            for g,st in b.items():
                for a in st:
                    if a[2]>=0: nf[a[2]]=max(nf.get(a[2],0),a[3]+1)
            mu={u:v[0] for u,v in m.items()};mu['nf']=nf
            C=X.cands(R,1.2,0.5,0.3,X.par2_index(R,m,b,0.001))
            res.append(X.score(R,X.hungarian(C,len(R)),mu))
            C=X.cands(R,1.2,0.5,0.3); res[-1]=res[-1]+X.score(R,X.hungarian(C,len(R)),mu)
        r=np.mean(res,0)
        print(f'obf={nob:2d} {cmode:6s} absorb gmul={gm:2d} runAcc={np.mean(acc):.3f} | hung P={r[3]:.3f} R={r[4]:.3f} whole={r[5]:.2f} | hung+par2 P={r[0]:.3f} R={r[1]:.3f} whole={r[2]:.2f}',flush=True)
