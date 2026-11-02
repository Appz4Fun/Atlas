import sys;sys.path.insert(0,'.')
from sim4_all import *
from sim3_all import sparse
def seg_label(C,lab,gap=2.0):
    """C: list of (n,rt,a) candidates time-ordered; lab: dict idx->true label for read ones. Unread articles split at
    gaps/terminators into segments; each segment takes label of nearest read neighbour (<=3s) else fresh."""
    out=[None]*len(C); segs=[];cur=[]
    for k,c in enumerate(C):
        if k in lab: out[k]=lab[k]; 
        if k in lab:
            if cur: segs.append(cur);cur=[]
            continue
        if cur and c[1]-C[cur[-1]][1]>gap: segs.append(cur);cur=[]
        cur.append(k)
        if c[2][6]<ART: segs.append(cur);cur=[]
    if cur: segs.append(cur)
    for sg in segs:
        nx=sg[-1]+1; pv=sg[0]-1; L=None
        if C[sg[-1]][2][6]==ART and nx<len(C) and nx in lab and C[nx][1]-C[sg[-1]][1]<3: L=lab[nx]
        elif pv>=0 and pv in lab and C[sg[0]][1]-C[pv][1]<3 and C[pv][2][6]==ART: L=lab[pv]
        if L is None: L=('seg',C[sg[0]][0])
        for k in sg: out[k]=L
    return out
def targeted(b,pad=1):
    groups=[];reads=0;tot=0
    for g,st in b.items():
        rt=rankt_of(st); C=[(n,rt[n],a) for n,a in enumerate(st) if a[2]>=0]
        sec={}
        for c in C: sec.setdefault(int(c[1]),set()).add(c[2][2])
        amb=set(s for s,u in sec.items() if len(u)>=2); amb|={s+d for s in amb for d in range(-pad,pad+1)}
        lab={k:(c[2][2],c[2][3]) for k,c in enumerate(C) if int(c[1]) in amb}
        reads+=len(lab); tot+=len(C)
        L=seg_label(C,lab); d={}
        for k,c in enumerate(C): d.setdefault(L[k],[]).append(c)
        groups.append((g,d))
    return mkruns(groups),reads/tot
def counter_split(b,m,share,rng):
    """Shared-token cluster 'fe': global counter N over fe articles (all groups). Unique-token uploads exact.
    fe articles per group split at terminators and at jumps in per-group N-stride (rate change => different upload)."""
    S={u:('fe' if rng.random()<share else u) for u in m}
    fe=sorted(a for st in b.values() for a in st if a[2]>=0 and S[a[2]]=='fe'); N={a:i for i,a in enumerate(fe)}
    groups=[]
    for g,st in b.items():
        rt=rankt_of(st); d={}; C=[]
        for n,a in enumerate(st):
            if a[2]<0: continue
            if S[a[2]]!='fe': d.setdefault((a[2],a[3]),[]).append((n,rt[n],a))
            else: C.append((n,rt[n],a))
        cur=[];seg=0;strides=[]
        for k,c in enumerate(C):
            if cur:
                s=N[c[2]]-N[C[k-1][2]]
                med=np.median(strides[-20:]) if len(strides)>=5 else None
                if c[1]-C[k-1][1]>2 or (med and (s>6*med+20)): seg+=1;strides=[]
                else: strides.append(s)
            cur.append(c); d.setdefault(('fe',g,seg),[]).append(c)
            if c[2][6]<ART: seg+=1;strides=[]
        groups.append((g,d))
    return mkruns(groups)
if __name__=='__main__':
    rng=random.Random(2)
    W=[build_world(nob,40,50,[4,10,20,50],300+s) for nob in [5,20] for s in range(2)]
    for share in [0.1,0.3,0.5]:
        o=[]
        for b,m,t in W: o.append(run_chain(counter_split(b,m,share,rng),m,b))
        o=np.mean(o,0); print(f'P2 share={share} counterSplit P={o[0]:.3f} R={o[1]:.3f} wholeUp={o[2]:.2f}',flush=True)
    for pad in [0,1,3]:
        o=[];rr=[];pu=[]
        for b,m,t in W:
            R,r=targeted(b,pad); o.append(run_chain(R,m,b)); rr.append(r)
            pu.append(np.mean([x['pure'] for x in R if m[x['uid']][0]=='obf']))
        o=np.mean(o,0); print(f'P3 pad={pad} reads/art={np.mean(rr):.3f} obfRunPure={np.mean(pu):.3f} P={o[0]:.3f} R={o[1]:.3f} wholeUp={o[2]:.2f}',flush=True)
    o=[]
    for b,m,t in W: o.append(run_chain(exact_runs(b),m,b))
    print('P3 allread wholeUp=%.2f'%np.mean(o,0)[2])
