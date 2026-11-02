import sys, random, math, numpy as np
sys.path.insert(0,'.')
from chainsim import build_world, ART, G
from sim2_xover import cands, hungarian, score, par2_index
def rankt_of(st):
    secs=np.floor([a[0] for a in st]).astype(int)
    _,first,cnt=np.unique(secs,return_index=True,return_counts=True)
    fi=np.repeat(first,cnt); cc=np.repeat(cnt,cnt)
    return secs+(np.arange(len(st))-fi+0.5)/cc
def mkruns(groups):
    runs=[]
    for g,d in groups:
        for lab,s in d.items():
            ts=np.array([c[1] for c in s]); labs={}
            for c in s: labs[(c[2][2],c[2][3])]=labs.get((c[2][2],c[2][3]),0)+1
            tl=max(labs,key=labs.get); tot=s[0][2][5]
            pure=len(labs)==1 and len(s)==tot
            runs.append(dict(g=g,s=ts.min(),e=ts.max(),rate=(len(s)-1)/max(ts.max()-ts.min(),1e-3),n=len(s),
              bytes=sum(c[2][6] for c in s),lab=tl,pure=pure,uid=tl[0],f=tl[1]))
    return [r for r in runs if r['n']>1]
def sparse(bygroup,K,rng):
    """read header of 1 per K candidates; short articles matched by size to read files."""
    groups=[];reads=0;corr=0;tot=0
    for g,st in bygroup.items():
        rt=rankt_of(st)
        C=[(n,rt[n],a) for n,a in enumerate(st) if a[2]>=0]
        off=rng.randrange(K); lab=[None]*len(C); read=set()
        for i in range(off,len(C),K): lab[i]=(C[i][2][2],C[i][2][3]); read.add(i)
        reads+=len(read)
        # last-part size -> file (header gives file size => last size); exact
        known={}
        for i in read: known[(C[i][2][6] if C[i][2][6]<ART else None, lab[i])]=1
        lastsz={}
        for i in read:
            a=C[i][2]; lastsz[a[6] if a[4]==a[5] else None]=None
        fileset=set(lab[i] for i in read)
        # map last size->label using true file sizes of read files (header carries size)
        szmap={}
        for i in range(len(C)):
            a=C[i][2]
            if a[4]==a[5] and (a[2],a[3]) in fileset: szmap[a[6]]=(a[2],a[3])
        ridx=sorted(read)
        for i in range(len(C)):
            if lab[i] is not None: continue
            a=C[i][2]
            if a[6]<ART and a[6] in szmap: lab[i]=szmap[a[6]]; continue
            k=np.searchsorted(ridx,i); best=None
            for kk in (k-1,k,k-2,k+1):
                if 0<=kk<len(ridx):
                    j=ridx[kk]; d=abs(C[j][1]-C[i][1])+0.01*abs(j-i)
                    if best is None or d<best[0]: best=(d,lab[j])
            lab[i]=best[1] if best else ('x',i)
        for i in range(len(C)):
            tot+=1; corr+=lab[i]==(C[i][2][2],C[i][2][3])
        d={}
        for i in range(len(C)): d.setdefault(lab[i],[]).append(C[i])
        groups.append((g,d))
    return mkruns(groups),reads,corr/tot,tot
def dp_place(bygroup,meta,rng):
    """PAR2 sizes known for each obf upload: identify terminator by exact last size, then take the n-1 full
    articles nearest-before(+after) it as densest contiguous block of unclaimed full-size candidates."""
    res=[];groups=[]
    for g,st in bygroup.items():
        rt=rankt_of(st)
        C=[(n,rt[n],a) for n,a in enumerate(st) if a[2]>=0]
        full=[i for i,c in enumerate(C) if c[2][6]==ART]
        claimed=set(); d={}
        terms=[i for i,c in enumerate(C) if c[2][4]==c[2][5] and meta[c[2][2]][0]=='obf']
        for ti in terms:
            a=C[ti][2]; n=a[5]-1
            p=np.searchsorted(full,ti); best=None
            for after in range(0,min(60,n)+1):
                hi=p+after; lo=hi-n
                if lo<0 or hi>len(full): continue
                blk=full[lo:hi]
                span=C[blk[-1]][1]-C[blk[0]][1]
                if best is None or span<best[0]: best=(span,blk)
            blk=best[1] if best else []
            ok=sum((C[i][2][2],C[i][2][3])==(a[2],a[3]) for i in blk)
            res.append((ok/max(n,1), ok==n))
            d[(a[2],a[3])]=[C[i] for i in blk]+[C[ti]]
        groups.append((g,d))
    return mkruns(groups),np.mean([r[0] for r in res]),np.mean([r[1] for r in res])
def serverid(bygroup,meta,share,rng):
    """S token: obf target uploads get unique S, except a fraction `share` of competing uploads share
    a front-end-level S with them (S per front-end process). Cluster = articles with same S."""
    ups=sorted(meta); S={}
    for u in ups: S[u]=('fe' if rng.random()<share else u)
    groups=[]
    for g,st in bygroup.items():
        rt=rankt_of(st); d={}
        for n,a in enumerate(st):
            if a[2]<0: continue
            d.setdefault((S[a[2]],a[3] if S[a[2]]!='fe' else None),[]).append((n,rt[n],a))
        groups.append((g,d))
    return groups,S
def run_chain(R,m,b):
    nf={}
    for g,st in b.items():
        for a in st:
            if a[2]>=0: nf[a[2]]=max(nf.get(a[2],0),a[3]+1)
    mu={u:v[0] for u,v in m.items()}; mu['nf']=nf
    p2=par2_index(R,m,b,0.001)
    return score(R,hungarian(cands(R,1.2,0.5,0.3,p2),len(R)),mu)
if __name__=='__main__':
    exp=sys.argv[1]; rng=random.Random(2)
    W=[build_world(nob,40,50,[4,10,20,50],300+s) for nob in [5,20] for s in range(2)]
    if exp=='sparse':
        for K in [int(sys.argv[2])]:
            out=[];rr=[];ra=[];pur=[]
            for b,m,t in W:
                R,reads,acc,tot=sparse(b,K,rng); out.append(run_chain(R,m,b)); rr.append(reads/tot); ra.append(acc)
                pur.append(np.mean([r['pure'] for r in R if m[r['uid']][0]=='obf']))
            o=np.mean(out,0)
            print(f'K={K:2d} reads/art={np.mean(rr):.3f} artAcc={np.mean(ra):.4f} obfRunPure={np.mean(pur):.3f} P={o[0]:.3f} R={o[1]:.3f} wholeUp={o[2]:.2f}',flush=True)
    if exp=='dp':
        out=[];fa=[];fe=[]
        for b,m,t in W:
            R,a,e=dp_place(b,m,rng); out.append(run_chain(R,m,b)); fa.append(a); fe.append(e)
        o=np.mean(out,0)
        print(f'DP artAcc={np.mean(fa):.3f} fileExact={np.mean(fe):.3f} P={o[0]:.3f} R={o[1]:.3f} wholeUp={o[2]:.2f}')
    if exp=='sid':
        for share in [0,0.1,0.3,0.5]:
            out=[]
            for b,m,t in W:
                groups,S=serverid(b,m,share,rng)
                # unique-S uploads: exact; shared: fall back to sparse K=10 within 'fe' cluster only
                R=mkruns([(g,{k:v for k,v in d.items() if k[0]!='fe'}) for g,d in groups])
                fe={g:[a for a in st if a[2]>=0 and S[a[2]]=='fe'] for g,st in b.items()}
                Rf,_,_,_=sparse({g:sorted(v) for g,v in fe.items()},10,rng) if any(fe.values()) else ([],0,0,0)
                out.append(run_chain(R+Rf,m,b))
            o=np.mean(out,0)
            print(f'share={share:.1f} P={o[0]:.3f} R={o[1]:.3f} wholeUp={o[2]:.2f}',flush=True)
