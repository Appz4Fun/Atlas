import sys, random, math, numpy as np
sys.path.insert(0,'.')
import chainsim
from chainsim import build_world, ART, G
from sim2_xover import hungarian, score
from sim3_all import mkruns, rankt_of, run_chain
POOL=[70*ART-1234, 140*ART-777]  # common RAR volume sizes shared by many posters
def build_vol(nob,seed,K,bgshare=0.5):
    """obf uploads: all but K files are equal-size volumes from POOL; bg uploads use POOL vols w.p. bgshare."""
    orig=chainsim.sim_upload
    def su(uid,t0,nfiles,N,mb,gm,rng):
        V=rng.choice(POOL); obf=(gm=='rand'); useV=obf or rng.random()<bgshare
        r2=random.Random(rng.random())
        def fs(f):
            if not useV or (obf and K is not None and f>=nfiles-K): return r2.randint(50,300)*ART-r2.randint(1,ART-1)
            return V
        out=[];files=[]
        for f in range(nfiles):
            s=fs(f); files.append((rng.randrange(G) if obf else gm, math.ceil(s/ART), s))
        import heapq
        q=[(g,f,p,n,ART if p<n else s-ART*(n-1)) for f,(g,n,s) in enumerate(files) for p in range(1,n+1)]
        pc=mb*1e6/N; rtt=rng.uniform(.01,.08); cs=[(t0+rng.uniform(0,.2),i) for i in range(N)]; heapq.heapify(cs)
        for (g,f,p,n,sz) in q:
            t,i=heapq.heappop(cs); a=t+rtt+sz*8/pc*rng.lognormvariate(0,.25)
            out.append((a,g,uid,f,p,n,sz)); heapq.heappush(cs,(a+rtt,i))
        return out
    chainsim.sim_upload=su
    try: return build_world(nob,40,50,[4,10,20,50],seed)
    finally: chainsim.sim_upload=orig
def exact_runs(b):
    return mkruns([(g,{(a[2],a[3]):[(n,rt,a) for n,rt,a in zip(range(len(st)),rankt_of(st),st)] for a in []}) for g,st in b.items()]) if 0 else \
        mkruns([(g,_grp(st)) for g,st in b.items()])
def _grp(st):
    rt=rankt_of(st); d={}
    for n,a in enumerate(st):
        if a[2]>=0: d.setdefault((a[2],a[3]),[]).append((n,rt[n],a))
    return d
def truth_sizes(b,m):
    S={}
    for g,st in b.items():
        for a in st:
            if a[2]>=0 and m[a[2]][0]=='obf': S.setdefault(a[2],{}); S[a[2]][a[3]]=S[a[2]].get(a[3],0)+a[6]
    return {u:sorted(v.values()) for u,v in S.items()}
def subset_sum(R,b,m):
    """Each PAR2 set: claim runs with exact sizes; anchors = sizes unique among runs; window from anchors;
    joint Hungarian over (set,slot) x runs with cost = time distance outside window; order by start."""
    S=truth_sizes(b,m); bysz={}
    for i,r in enumerate(R): bysz.setdefault(r['bytes'],[]).append(i)
    slots=[];win={}
    for u,sz in S.items():
        cnt={}
        for s in sz: cnt[s]=cnt.get(s,0)+1
        anc=[bysz[s][0] for s,c in cnt.items() if len(bysz.get(s,[]))==c==1]
        if anc:
            lo=min(R[i]['s'] for i in anc); hi=max(R[i]['e'] for i in anc)
            rate=np.median([R[i]['rate'] for i in anc]); dur=sum(sz)/ART/rate
            win[u]=(min(lo,hi-dur)-2,max(hi,lo+dur)+2)
        else: win[u]=None
        for s in sz: slots.append((u,s))
    cand=sorted({i for _,s in slots for i in bysz.get(s,[])}); ci={i:k for k,i in enumerate(cand)}
    n=len(slots); A=np.full((n,len(cand)+n),1e9)
    for k,(u,s) in enumerate(slots):
        A[k,len(cand)+k]=1e6
        for i in bysz.get(s,[]):
            w=win[u]; r=R[i]
            A[k,ci[i]]=0 if w is None else max(0,w[0]-r['s'])+max(0,r['e']-w[1])
    rr,cc=lsa(A); pick={}
    for k,c in zip(rr,cc):
        if c<len(cand): pick.setdefault(slots[k][0],[]).append(cand[c])
    su={}
    for u,L in pick.items():
        L.sort(key=lambda i:R[i]['s'])
        for x,y in zip(L,L[1:]): su[x]=y
    return su
def mu_of(m,b):
    nf={}
    for g,st in b.items():
        for a in st:
            if a[2]>=0: nf[a[2]]=max(nf.get(a[2],0),a[3]+1)
    mu={u:v[0] for u,v in m.items()}; mu['nf']=nf; return mu

def lsa(A):
    n,m=A.shape; u=np.zeros(n+1);v=np.zeros(m+1);p=np.zeros(m+1,int);way=np.zeros(m+1,int)
    for i in range(1,n+1):
        p[0]=i;j0=0;minv=np.full(m+1,np.inf);used=np.zeros(m+1,bool)
        while True:
            used[j0]=True;i0=p[j0];cur=A[i0-1]-u[i0]-v[1:];free=~used[1:]
            upd=free&(cur<minv[1:]);minv[1:][upd]=cur[upd];way[1:][upd]=j0
            mv=np.where(free,minv[1:],np.inf);j1=int(np.argmin(mv))+1;delta=mv[j1-1]
            uj=np.where(used)[0];u[p[uj]]+=delta;v[uj]-=delta;minv[1:][free]-=delta;j0=j1
            if p[j0]==0: break
        while True:
            j1=way[j0];p[j0]=p[j1];j0=j1
            if j0==0: break
    rr=[];cc=[]
    for j in range(1,m+1):
        if p[j]: rr.append(p[j]-1);cc.append(j-1)
    return rr,cc
def subset_chain(R,b,m,dbg=False):
    """Anchor on unique-size runs, then grow chain backward/forward over runs whose size is still in the
    PAR2 remaining multiset; global greedy cheapest-extension-first across all PAR2 sets (claims exclusive)."""
    S=truth_sizes(b,m); bysz={}
    for i,r in enumerate(R): bysz.setdefault(r['bytes'],[]).append(i)
    claimed=set(); st={}
    for u,sz in S.items():
        rem={}
        for s in sz: rem[s]=rem.get(s,0)+1
        anc=[]
        for s,c in list(rem.items()):
            if len(bysz.get(s,[]))==c: anc+=bysz[s]
        anc=[i for i in anc if i not in claimed]
        for i in anc: rem[R[i]['bytes']]-=1; claimed.add(i)
        st[u]=dict(rem=rem,ch=sorted(anc,key=lambda i:R[i]['s']))
    def ext(u):
        d=st[u]; best=None
        if not d['ch'] or sum(d['rem'].values())==0: return None
        h=R[d['ch'][0]]; t=R[d['ch'][-1]]
        for s,c in d['rem'].items():
            if c<=0: continue
            for i in bysz.get(s,[]):
                if i in claimed: continue
                r=R[i]
                for side,a,bb in (('b',r,h),('f',t,r)):
                    gap=(bb['s']-a['e'])*a['rate']; lr=abs(math.log(r['rate']/(h if side=='b' else t)['rate']))
                    if gap<-70 or gap>3 or lr>0.4: continue
                    c_=lr/0.3+abs(gap+5)/40
                    if best is None or c_<best[0]: best=(c_,i,side)
        return best
    while True:
        opts=[(e,u) for u in st for e in [ext(u)] if e]
        if not opts: break
        (c_,i,side),u=min(opts)
        d=st[u]; claimed.add(i); d['rem'][R[i]['bytes']]-=1
        d['ch']=[i]+d['ch'] if side=='b' else d['ch']+[i]
    su={}
    for u,d in st.items():
        L=sorted(d['ch'],key=lambda i:R[i]['s'])
        for x,y in zip(L,L[1:]): su[x]=y
    return su
