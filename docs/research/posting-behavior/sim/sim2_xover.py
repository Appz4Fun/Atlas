import sys, random, math, numpy as np
sys.path.insert(0, '.')
from chainsim import build_world, ART, G
BIG=1e9
def runs_from(bygroup, meta, mode, use_hdr, gap_s=2.0):
    """Return runs: dict(s,e,rate,n,bytes,lab=(uid,f) majority,pure,arts)."""
    runs=[]
    for g,st in bygroup.items():
        secs=np.floor([a[0] for a in st]).astype(int)
        _,first,cnt=np.unique(secs,return_index=True,return_counts=True)
        fi=np.repeat(first,cnt); cc=np.repeat(cnt,cnt)
        rankt=secs+(np.arange(len(st))-fi+0.5)/cc
        cands=[(n,rankt[n],a) for n,a in enumerate(st) if a[2]>=0 and (mode=='all716' or meta[a[2]][0]=='obf')]
        if use_hdr:
            d={}
            for c in cands: d.setdefault((c[2][2],c[2][3]),[]).append(c)
            segs=list(d.values())
        else:
            segs=[];cur=[]
            for c in cands:
                if cur and c[1]-cur[-1][1]>gap_s: segs.append(cur);cur=[]
                cur.append(c)
                if c[2][6]<ART: segs.append(cur);cur=[]
            if cur: segs.append(cur)
            # merge straggler fragments (no short terminator, small, right after a terminated run) into previous
            out=[]
            for s in segs:
                if out and s[-1][2][6]==ART and len(s)<=60 and s[0][1]-out[-1][-1][1]<3.0 and out[-1][-1][2][6]<ART:
                    out[-1]=out[-1]+s
                else: out.append(s)
            segs=out
        for s in segs:
            ts=np.array([c[1] for c in s]); labs={}
            for c in s: labs[(c[2][2],c[2][3])]=labs.get((c[2][2],c[2][3]),0)+1
            lab=max(labs,key=labs.get)
            tot=s[0][2][5]
            pure=(len(labs)==1 and len(s)==tot)
            b=sum(c[2][6] for c in s)
            runs.append(dict(g=g,s=ts.min(),e=ts.max(),rate=(len(s)-1)/max(ts.max()-ts.min(),1e-3),n=len(s),bytes=b,lab=lab,pure=pure,uid=lab[0],f=lab[1]))
    return [r for r in runs if r['n']>1]
def cands(R,k,pad,rtol,par2=None,tol=None):
    by=sorted(range(len(R)),key=lambda i:R[i]['s']); st=np.array([R[i]['s'] for i in by]); C=[]
    for i,a in enumerate(R):
        w=max(4,0.15*a['n'])  # width unknown w/o headers: crude proxy
        lo=-(k*w/a['rate'])-pad; hi=pad
        for jj in range(np.searchsorted(st,a['e']+lo),np.searchsorted(st,a['e']+hi)):
            j=by[jj]; b=R[j]
            if j==i or b['s']<=a['s']: continue
            lr=abs(math.log(b['rate']/a['rate']))
            if lr>rtol: continue
            if par2 is not None:
                ua=par2.get(i); ub=par2.get(j)
                if not ua or not ub or not (ua&ub): continue
            C.append((abs(b['s']-a['e']+0.5*w/a['rate'])/(hi-lo)+lr/rtol,i,j))
    return C
def greedy(C):
    C=sorted(C); su={};pr={}
    for c,i,j in C:
        if i in su or j in pr: continue
        su[i]=j;pr[j]=i
    return su
def hungarian(C,n,c0=1.2):
    # min-cost assignment rows=n preds, cols = n succs + n 'none' cols
    m=2*n; A=np.full((n,m),BIG)
    for c,i,j in C: A[i,j]=min(A[i,j],c)
    for i in range(n): A[i,n+i]=c0
    u=np.zeros(n+1);v=np.zeros(m+1);p=np.zeros(m+1,int);way=np.zeros(m+1,int)
    for i in range(1,n+1):
        p[0]=i;j0=0;minv=np.full(m+1,np.inf);used=np.zeros(m+1,bool)
        while True:
            used[j0]=True;i0=p[j0]
            cur=A[i0-1]-u[i0]-v[1:]
            free=~used[1:]
            upd=free&(cur<minv[1:])
            minv[1:][upd]=cur[upd];way[1:][upd]=j0
            mv=np.where(free,minv[1:],np.inf);j1=int(np.argmin(mv))+1;delta=mv[j1-1]
            uj=np.where(used)[0]
            u[p[uj]]+=delta;v[uj]-=delta;minv[1:][free]-=delta
            j0=j1
            if p[j0]==0: break
        while True:
            j1=way[j0];p[j0]=p[j1];j0=j1
            if j0==0: break
    su={}
    for j in range(1,n+1):
        if p[j] and A[p[j]-1,j-1]<BIG: su[p[j]-1]=j-1
    return su
def score(R,su,mode_up):
    idx={}
    for i,r in enumerate(R): idx.setdefault((r['uid'],r['f']),i)
    truth=set()
    for i,r in enumerate(R):
        k=(r['uid'],r['f']+1)
        if k in idx and idx[(r['uid'],r['f'])]==i: truth.add((i,idx[k]))
    pred=set(su.items()); tp=len(pred&truth)
    P=tp/len(pred) if pred else 1; Rc=tp/len(truth) if truth else 1
    ups={}
    for i,r in enumerate(R): ups.setdefault(r['uid'],[]).append(i)
    ok=0;n=0
    for u,L in ups.items():
        if mode_up.get(u)!='obf': continue
        n+=1; L.sort(key=lambda i:R[i]['f'])
        fs=[R[i]['f'] for i in L]
        good=all(R[i]['pure'] for i in L) and fs==list(range(len(fs))) and len(fs)==mode_up['nf'][u] \
             and all(su.get(L[k])==L[k+1] for k in range(len(L)-1)) and su.get(L[-1]) is None and all(su.get(x)!=L[0] for x in su)
        ok+=good
    return P,Rc,ok/max(n,1)
def par2_index(R,meta,bygroup,tolfrac):
    # PAR2 gives each obf upload's true file sizes; run bytes measured with noise; map run->set of uploads matching
    sizes={}
    for g,st in bygroup.items():
        for a in st:
            if a[2]>=0 and meta[a[2]][0]=='obf': sizes[(a[2],a[3])]=sizes.get((a[2],a[3]),0)+a[6]
    allsz=[(sz,u) for (u,f),sz in sizes.items()]; arr=np.array([s for s,_ in allsz]); us=[u for _,u in allsz]
    rng=np.random.default_rng(5); m={}
    for i,r in enumerate(R):
        est=r['bytes']*(1+rng.normal(0,tolfrac/3))
        hit=np.where(np.abs(arr-est)<=tolfrac*est)[0]
        m[i]=set(us[h] for h in hit)
    return m
if __name__=='__main__':
    rows=[]
    for nob in [5,20]:
        W=[build_world(nob,40,50,[4,10,20,50],300+s) for s in range(3)]
        for cmode in ['obf','all716']:
            for hdr in [1,0]:
                for meth in ['greedy','hung','greedy+par2','hung+par2']:
                    res=[];fra=[]
                    for b,m,t in W:
                        R=runs_from(b,m,cmode,hdr)
                        fra.append(np.mean([r['pure'] for r in R]))
                        nf={}
                        for g,st in b.items():
                            for a in st:
                                if a[2]>=0: nf[a[2]]=max(nf.get(a[2],0),a[3]+1)
                        mu={u:v[0] for u,v in m.items()}; mu['nf']=nf
                        p2=par2_index(R,m,b,0.001) if 'par2' in meth else None
                        C=cands(R,1.2,0.5,0.3,p2)
                        su=greedy(C) if meth.startswith('greedy') else hungarian(C,len(R))
                        res.append(score(R,su,mu))
                    r=np.mean(res,0)
                    print(f'obf={nob:2d} {cmode:6s} {"hdr" if hdr else "xover"} {meth:12s} runAcc={np.mean(fra):.3f} P={r[0]:.3f} R={r[1]:.3f} wholeUp={r[2]:.2f}',flush=True)
